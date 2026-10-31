//! Core's `CFinalCommitment::Verify` with signatures (SB8; DEC-109 (4)): the one commitment check
//! the node (LQ2) and SPV (SB0 §5.1, §5.2 step 5) share, and the [`VerifiedQuorum`] it produces,
//! the only quorum key a lock is ever verified against (SB0 §5.2 step 6).
//!
//! The members come from a [`QuorumMemberSource`]: the node's process-wide facade over its
//! masternode lists, or SPV's reconstruction over its simplified lists
//! ([`UncachedMembers`] over [`crate::sml::SmlList`]). Over the SML the reconstruction is exact
//! only up to what `membersSig` binds (SB0 §5.1): the multiset of operator keys at the signer
//! positions. A [`VerifiedQuorum`] therefore carries the quorum's identity and key and never its
//! members.

use std::cell::RefCell;
use std::fmt;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_consensus::ctx::Deployment;
use dash_consensus::hash::HashWriter;
use dash_consensus::primitives::{CoreWrite, write_dyn_bitset};
use dash_params::LlmqType;
use dash_params::{BuriedDeployment, ChainSetup, LlmqParams};
use dash_pkc::bls::BlsEagerPk;
use dash_pkc::bls::core::{
    BlsPkValue, BlsSigValue, BlsVerdict, verify_insecure, verify_secure_aggregated,
};
use dash_primitives::{DynBitset, QuorumHash};
use dash_types::Numeric;

use super::LlmqCommitment;
use super::ctx::{LlmqCtx, LlmqError, LlmqSource, SnapshotRows};
use super::members::{MemberError, QuorumMemberSource, QuorumMembers, members_uncached};
use super::params::{dkg_interval, is_quorum_rotation_enabled};
use super::view::{MnEntryView, MnListView};
use crate::bls::{BlsCtx, BlsSchemeKind};
use crate::effect::EffectSink;

/// A member entry whose operator key a commitment's `membersSig` is checked against.
pub trait OperatorKeyView: MnEntryView {
    /// `pdmnState->pubKeyOperator.Get()`: the key decoded under the scheme its state stores it in
    /// (legacy iff the state's version is 1), the invalid object when it does not decode
    /// (`bls/bls.h` v24:492-510).
    fn operator_key(&self) -> BlsPkValue;
}

/// Forwards to `T`, as the entry view's forwarding impl does (EV addendum M1).
impl<T: OperatorKeyView> OperatorKeyView for Arc<T> {
    fn operator_key(&self) -> BlsPkValue {
        (**self).operator_key()
    }
}

/// `CFinalCommitment::GetVersion(is_rotation_enabled, is_basic_scheme_active)`
/// (`llmq/commitment.h` v24:85-92).
// Core: llmq/commitment.h:85 CFinalCommitment::GetVersion
pub fn commitment_version(rotation_enabled: bool, basic_scheme_active: bool) -> u16 {
    match (rotation_enabled, basic_scheme_active) {
        (true, true) => 4,
        (true, false) => 2,
        (false, true) => 3,
        (false, false) => 1,
    }
}

/// Core's `BuildCommitmentHash`: the SHA256d of the type, the quorum hash, the valid-members
/// bitset (`DYNBITSET`), the quorum public key and the vvec hash. The key serializes under the
/// caller's scheme (Core's process-wide flag), zeros when it is invalid.
// Core: llmq/commitment.cpp:312 BuildCommitmentHash
pub fn build_commitment_hash(
    llmq_type: LlmqType,
    quorum_hash: &QuorumHash,
    valid_members: &DynBitset,
    public_key: &BlsEagerPk,
    vvec_hash: &[u8; 32],
    bls: BlsCtx,
) -> [u8; 32] {
    let mut w = HashWriter::new();
    w.write(&[llmq_type.to_base()]);
    w.write(quorum_hash.as_bytes());
    write_dyn_bitset(valid_members, &mut w);
    w.write(&public_key.to_bytes(bls.scheme()));
    w.write(vvec_hash);
    w.finish()
}

/// `size_t(params.size)`.
fn quorum_size(params: &LlmqParams) -> u64 {
    u64::from(params.size)
}

/// Core's `CFinalCommitment::VerifySizes(params)`: both bitsets are as long as the quorum.
// Core: llmq/commitment.cpp:226 CFinalCommitment::VerifySizes
pub fn verify_sizes(params: &LlmqParams, qc: &LlmqCommitment) -> bool {
    let size = quorum_size(params);
    qc.signers.num_bits == size && qc.valid_members.num_bits == size
}

/// Core's `CFinalCommitment::VerifyNull()`: the type is configured (enabled or not, quirk 19), the
/// commitment is null ([`LlmqCommitment::is_null`], quirk 36) and its bitsets have the quorum's
/// size. It reads no block: a null commitment's height and quorum hash are its callers' checks.
// Core: llmq/commitment.cpp:211 CFinalCommitment::VerifyNull
pub fn verify_null(params: &ChainSetup, qc: &LlmqCommitment) -> bool {
    params.consensus.llmq(qc.llmq_type).is_some_and(|llmq| qc.is_null() && verify_sizes(llmq, qc))
}

/// `bits[i]`, false past the bitset (a bitset built by hand may hold fewer bytes than its size).
fn bit_set(bits: &DynBitset, i: u64) -> bool {
    bits.get(i) == Some(true)
}

/// Why Core's `CFinalCommitment::Verify` returns false: one variant per log line, in Core's
/// order (`llmq/commitment.cpp` v24:113-209 and `VerifySignatureAsync`, :35-110).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitmentReject {
    /// "invalid llmqType": the type is not configured.
    LlmqType,
    /// "parentless quorum base block".
    ParentlessBase,
    /// "invalid nVersion": zero, or not the version the base's rotation and V19 state give.
    Version {
        /// `nVersion`.
        found: u16,
        /// `GetVersion(IsQuorumRotationEnabled, V19 active after the base)`.
        expected: u16,
    },
    /// "invalid quorumHash": the base block's hash differs.
    QuorumHash,
    /// "invalid quorumIndex": not `base.nHeight % dkgInterval`.
    QuorumIndex,
    /// "invalid signers.size".
    SignersSize,
    /// "invalid validMembers.size".
    ValidMembersSize,
    /// "invalid validMembers count": below `minSize`.
    ValidMembersCount,
    /// "invalid signers count": below `minSize`.
    SignersCount,
    /// "invalid quorumPublicKey".
    QuorumPublicKey,
    /// "invalid quorumVvecHash": null for a quorum of more than one member.
    VvecHash,
    /// "invalid membersSig": the object is invalid.
    MembersSigObject,
    /// "invalid vvecSig": `quorumSig` is invalid.
    QuorumSigObject,
    /// "invalid validMembers bitset": a bit set at or past the number of members.
    ValidMemberBit(usize),
    /// "invalid signers bitset": a bit set at or past the number of members.
    SignerBit(usize),
    /// "no quorum members", for a single-member quorum.
    NoMembers,
    /// "invalid member signature": a single-member quorum's `membersSig`.
    MemberSignature,
    /// "invalid aggregated members signature".
    MembersSignature,
    /// "invalid quorum signature".
    QuorumSignature,
}

impl fmt::Display for CommitmentReject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LlmqType => f.write_str("invalid llmqType"),
            Self::ParentlessBase => f.write_str("parentless quorum base block"),
            Self::Version {
                found,
                expected,
            } => write!(f, "invalid nVersion={found} expected={expected}"),
            Self::QuorumHash => f.write_str("invalid quorumHash"),
            Self::QuorumIndex => f.write_str("invalid quorumIndex"),
            Self::SignersSize => f.write_str("invalid signers.size"),
            Self::ValidMembersSize => f.write_str("invalid validMembers.size"),
            Self::ValidMembersCount => f.write_str("invalid validMembers count"),
            Self::SignersCount => f.write_str("invalid signers count"),
            Self::QuorumPublicKey => f.write_str("invalid quorumPublicKey"),
            Self::VvecHash => f.write_str("invalid quorumVvecHash"),
            Self::MembersSigObject => f.write_str("invalid membersSig"),
            Self::QuorumSigObject => f.write_str("invalid vvecSig"),
            Self::ValidMemberBit(i) => {
                write!(f, "invalid validMembers bitset. bit {i} should not be set")
            }
            Self::SignerBit(i) => write!(f, "invalid signers bitset. bit {i} should not be set"),
            Self::NoMembers => f.write_str("no quorum members"),
            Self::MemberSignature => f.write_str("invalid member signature"),
            Self::MembersSignature => f.write_str("invalid aggregated members signature"),
            Self::QuorumSignature => f.write_str("invalid quorum signature"),
        }
    }
}

/// Why a commitment did not verify.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitmentError<E> {
    /// Core's `Verify` returns false.
    Rejected(CommitmentReject),
    /// A signature check's answer depends on relic's arithmetic on a point off the curve or the
    /// subgroup, which BS12 does not reproduce ([`BlsVerdict::Unsupported`]): no verdict. Only
    /// the legacy scheme reaches it.
    BlsUnsupported,
    /// A chain or member read gave no answer.
    Members(MemberError<E>),
}

impl<E> From<MemberError<E>> for CommitmentError<E> {
    fn from(e: MemberError<E>) -> Self {
        CommitmentError::Members(e)
    }
}

impl<E: fmt::Display> fmt::Display for CommitmentError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommitmentError::Rejected(r) => write!(f, "commitment rejected: {r}"),
            CommitmentError::BlsUnsupported => {
                f.write_str("commitment signature check outside the supported BLS domain")
            }
            CommitmentError::Members(e) => write!(f, "commitment members: {e}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for CommitmentError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CommitmentError::Members(e) => Some(e),
            CommitmentError::Rejected(_) | CommitmentError::BlsUnsupported => None,
        }
    }
}

fn reject<T, E>(r: CommitmentReject) -> Result<T, CommitmentError<E>> {
    Err(CommitmentError::Rejected(r))
}

/// The path a commitment's signatures take in Core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigPath {
    /// `Verify(…, checkSigs = true)`: `VerifySignatureAsync` with no check queue. Core's check of
    /// a `qfcommit` received over P2P (`llmq/blockprocessor.cpp` v24:195).
    Sync,
    /// A commitment mined in a block, with `fBLSChecks` (else Core checks no signature: `None`):
    /// `ProcessBlock`'s `VerifySignatureAsync` with a check queue (`llmq/blockprocessor.cpp`
    /// v24:267-284). Each queued `BlsCheck` (`llmq/utils.cpp` v24:561-579) verifies several keys
    /// as a secure aggregate, one key insecurely, and no key as false. A single-member quorum
    /// takes Core's verdict on this path too (SB8 D18; possible security note: see manager).
    Mined,
}

/// Core's `CFinalCommitment::Verify(util_params, checkSigs)` for the commitment `qc` whose quorum
/// base is `base`, with the members `members` gives for it (read once, after the structural
/// checks, where Core reads them) and the scheme of `ctx.bls()`: `bls_ctx_for_block(h)` for a
/// commitment mined in block `h` (Core's `ProcessBlock` runs under it), the tip's for a
/// `qfcommit` received over P2P. `sigs`: `None` is `checkSigs = false`; otherwise the path of the
/// signature checks ([`SigPath`]).
///
/// `Ok(())` exactly where Core returns true (with [`SigPath::Mined`]: where `ProcessBlock`'s
/// checks pass). Core's `Verify` takes no `bad-qc-*` decision itself; LQ2's
/// `CheckLLMQCommitment` and `ProcessCommitment` map a false to theirs.
// Core: llmq/commitment.cpp:113 CFinalCommitment::Verify
pub fn verify_commitment<E, M>(
    ctx: &LlmqCtx<'_, E>,
    members: &M,
    qc: &LlmqCommitment,
    base: BlockId,
    sigs: Option<SigPath>,
    effects: &mut dyn EffectSink,
) -> Result<(), CommitmentError<E>>
where
    M: QuorumMemberSource<Error = E> + ?Sized,
    M::Entry: OperatorKeyView,
{
    let params = match ctx.llmq(qc.llmq_type) {
        Ok(params) => params,
        Err(MemberError::UnknownLlmqType(_)) => return reject(CommitmentReject::LlmqType),
        Err(e) => return Err(e.into()),
    };
    if ctx.chain().prev(base).map_err(MemberError::View)?.is_none() {
        return reject(CommitmentReject::ParentlessBase);
    }
    let v19 = ctx.deployments().active_after(Some(base), Deployment::Buried(BuriedDeployment::V19));
    let expected = commitment_version(is_quorum_rotation_enabled(ctx, params, base)?, v19);
    if qc.version == 0 || qc.version != expected {
        return reject(CommitmentReject::Version {
            found: qc.version,
            expected,
        });
    }
    if ctx.hash(base)?.as_bytes() != qc.quorum_hash.as_bytes() {
        return reject(CommitmentReject::QuorumHash);
    }
    // `int` arithmetic: the height modulo a positive interval, against the `int16_t` index.
    let index = i64::from(ctx.height(base)? % dkg_interval(params));
    if index != i64::from(qc.quorum_index.unwrap_or(0)) {
        return reject(CommitmentReject::QuorumIndex);
    }
    let size = quorum_size(params);
    if qc.signers.num_bits != size {
        return reject(CommitmentReject::SignersSize);
    }
    if qc.valid_members.num_bits != size {
        return reject(CommitmentReject::ValidMembersSize);
    }
    let min_size = i64::from(params.min_size);
    if (qc.valid_members.count_ones() as i64) < min_size {
        return reject(CommitmentReject::ValidMembersCount);
    }
    if (qc.signers.count_ones() as i64) < min_size {
        return reject(CommitmentReject::SignersCount);
    }
    if !qc.quorum_public_key.is_valid() {
        return reject(CommitmentReject::QuorumPublicKey);
    }
    if !params.is_single_member() && qc.quorum_vvec_hash.as_bytes() == &[0u8; 32] {
        return reject(CommitmentReject::VvecHash);
    }
    if !qc.members_sig.is_valid() {
        return reject(CommitmentReject::MembersSigObject);
    }
    if !qc.quorum_sig.is_valid() {
        return reject(CommitmentReject::QuorumSigObject);
    }
    let members = members.members(qc.llmq_type, base, effects)?;
    for i in members.len()..usize::try_from(size).unwrap_or(usize::MAX) {
        if bit_set(&qc.valid_members, i as u64) {
            return reject(CommitmentReject::ValidMemberBit(i));
        }
        if bit_set(&qc.signers, i as u64) {
            return reject(CommitmentReject::SignerBit(i));
        }
    }
    if let Some(path) = sigs {
        verify_signatures(path, ctx.bls(), params.is_single_member(), qc, &members)?;
    }
    Ok(())
}

/// `VerifySignatureAsync(base, &queue)` as `ProcessBlock` calls it with `fBLSChecks`, on a
/// non-null commitment `CheckLLMQCommitment` has checked: appends the checks Core queues to
/// `queue`, for the block's [`BlsCheckRunner`] (LQ2 D11, DEC-325). Core ignores the call's own
/// return value (`llmq/blockprocessor.cpp` v24:277), so where it returns before queuing (an
/// unconfigured type, a parentless base, a single-member quorum without members or with a member
/// signature that fails, SB8 D18) nothing is queued and the block passes this step. The members
/// are read as `Verify` reads them, with their effects. An error is a fault.
// Core: llmq/commitment.cpp:35 CFinalCommitment::VerifySignatureAsync
pub fn queue_mined_signatures<E, M>(
    ctx: &LlmqCtx<'_, E>,
    members: &M,
    qc: &LlmqCommitment,
    base: BlockId,
    effects: &mut dyn EffectSink,
    queue: &mut Vec<BlsCheck>,
) -> Result<(), MemberError<E>>
where
    M: QuorumMemberSource<Error = E> + ?Sized,
    M::Entry: OperatorKeyView,
{
    let params = match ctx.llmq(qc.llmq_type) {
        Ok(params) => params,
        Err(MemberError::UnknownLlmqType(_)) => return Ok(()),
        Err(e) => return Err(e),
    };
    if ctx.chain().prev(base).map_err(MemberError::View)?.is_none() {
        return Ok(());
    }
    let members = members.members(qc.llmq_type, base, effects)?;
    queue_signatures(ctx.bls(), params.is_single_member(), qc, &members, queue);
    Ok(())
}

/// One commitment's [`queue_mined_signatures`] with its checks run here, one after another: the
/// per-commitment verdict, with the reason of the first check that fails (the twin of LQ2 D11).
pub fn verify_mined_signatures<E, M>(
    ctx: &LlmqCtx<'_, E>,
    members: &M,
    qc: &LlmqCommitment,
    base: BlockId,
    effects: &mut dyn EffectSink,
) -> Result<(), CommitmentError<E>>
where
    M: QuorumMemberSource<Error = E> + ?Sized,
    M::Entry: OperatorKeyView,
{
    let mut queue = Vec::new();
    queue_mined_signatures(ctx, members, qc, base, effects, &mut queue)?;
    queued_verdict(&queue)
}

/// One check Core queues for `ProcessBlock` (`utils::BlsCheck`): a signature, the keys it is
/// checked against and the commitment hash, in the scheme of the block.
#[derive(Clone, Debug)]
pub struct BlsCheck {
    sig: BlsSigValue,
    keys: Vec<BlsPkValue>,
    hash: [u8; 32],
    scheme: BlsSchemeKind,
    /// The commitment's reason when the check fails.
    reject: CommitmentReject,
    /// A single-member quorum's quorum check whose member check gave no BLS verdict: Core queues
    /// it only when that member signature verifies, so its failure is no verdict either.
    unsure: bool,
}

impl BlsCheck {
    /// The check of `sig` against `keys` over `hash` in `scheme`, failing the commitment with
    /// `reject`; `unsure` makes a failure no verdict (a single-member quorum check whose member
    /// check gave none).
    pub fn new(
        sig: BlsSigValue,
        keys: Vec<BlsPkValue>,
        hash: [u8; 32],
        scheme: BlsSchemeKind,
        reject: CommitmentReject,
        unsure: bool,
    ) -> Self {
        Self {
            sig,
            keys,
            hash,
            scheme,
            reject,
            unsure,
        }
    }

    /// The signature.
    pub fn sig(&self) -> &BlsSigValue {
        &self.sig
    }

    /// The keys: several are one secure aggregate.
    pub fn keys(&self) -> &[BlsPkValue] {
        &self.keys
    }

    /// The commitment hash signed.
    pub fn msg_hash(&self) -> &[u8; 32] {
        &self.hash
    }

    /// The scheme of the block.
    pub fn scheme(&self) -> BlsSchemeKind {
        self.scheme
    }

    /// Whether a failure is no verdict rather than `Invalid`.
    pub fn is_unsure(&self) -> bool {
        self.unsure
    }

    /// Core's `BlsCheck::operator()`: several keys as one secure aggregate, one key insecure, and
    /// none fails ("no public keys are provided").
    // Core: llmq/utils.cpp:561 BlsCheck::operator()
    pub fn verdict(&self) -> BlsVerdict {
        let keys: Vec<&BlsPkValue> = self.keys.iter().collect();
        let verdict = match &keys[..] {
            [] => BlsVerdict::Invalid,
            [key] => verify_insecure(&self.sig, key, &self.hash, self.scheme),
            _ => verify_secure_aggregated(&self.sig, &keys, &self.hash, self.scheme),
        };
        match verdict {
            BlsVerdict::Valid => BlsVerdict::Valid,
            _ if self.unsure => BlsVerdict::Unsupported,
            v => v,
        }
    }

    /// The commitment's reason when the check fails.
    pub fn reject(&self) -> CommitmentReject {
        self.reject
    }
}

/// Core's BLS check queue as `ProcessBlock` uses it under `fBLSChecks` (`m_bls_queue`,
/// `llmq/blockprocessor.cpp` v24:267-284; LQ2 D11, DEC-325): runs every check of a block and
/// gives one verdict: `Invalid` when some check's [`BlsCheck::verdict`] is `Invalid`, else
/// `Unsupported` when some check's is, else `Valid`. A runner may skip the rest once a check
/// fails, as Core's workers do. The node runs the checks on Core's BLS worker threads; BS11b's
/// batch verification plugs in here, and a batch that fails falls back to each check's
/// `verdict`, since an unsure check's failure is `Unsupported`, not `Invalid`.
pub trait BlsCheckRunner {
    /// The verdict of `checks`.
    fn run(&self, checks: Vec<BlsCheck>) -> BlsVerdict;
}

/// The checks one after another on the caller: Core's queue without worker threads.
#[derive(Clone, Copy, Debug, Default)]
pub struct SerialBlsChecks;

impl BlsCheckRunner for SerialBlsChecks {
    fn run(&self, checks: Vec<BlsCheck>) -> BlsVerdict {
        match serial_verdict(&checks) {
            Err(_) => BlsVerdict::Invalid,
            Ok(verdict) => verdict,
        }
    }
}

/// The checks run in order: the first that fails, else `Unsupported` when one has no BLS verdict,
/// else `Valid`.
fn serial_verdict(checks: &[BlsCheck]) -> Result<BlsVerdict, &BlsCheck> {
    let mut unsupported = false;
    for check in checks {
        match check.verdict() {
            BlsVerdict::Invalid => return Err(check),
            BlsVerdict::Unsupported => unsupported = true,
            BlsVerdict::Valid => {}
        }
    }
    Ok(if unsupported {
        BlsVerdict::Unsupported
    } else {
        BlsVerdict::Valid
    })
}

/// The queued checks of one commitment run in order: the reason of the first that fails, else
/// no verdict when one has none.
fn queued_verdict<E>(queue: &[BlsCheck]) -> Result<(), CommitmentError<E>> {
    match serial_verdict(queue) {
        Err(check) => reject(check.reject),
        Ok(BlsVerdict::Unsupported) => Err(CommitmentError::BlsUnsupported),
        Ok(_) => Ok(()),
    }
}

/// `VerifySignatureAsync` with a queue, after `Verify`'s checks, with the members `Verify` read:
/// a single-member quorum's member signature checked at once (a failure queues nothing), a
/// several-member quorum's members signature queued, then the quorum signature queued.
// Core: llmq/commitment.cpp:35-110 CFinalCommitment::VerifySignatureAsync
fn queue_signatures<T: OperatorKeyView>(
    bls: BlsCtx,
    single_member: bool,
    qc: &LlmqCommitment,
    members: &QuorumMembers<T>,
    queue: &mut Vec<BlsCheck>,
) {
    if single_member && members.is_empty() {
        // "no quorum members": nothing queued (D18).
        return;
    }
    let hash = commitment_sig_hash(bls, qc);
    let scheme = bls.scheme();
    let mut unsure = false;
    if single_member {
        match verify_insecure(qc.members_sig.value(), &members[0].operator_key(), &hash, scheme) {
            BlsVerdict::Valid => {}
            // Core's mined verdict (D18; possible security note: see manager).
            BlsVerdict::Invalid => return,
            BlsVerdict::Unsupported => unsure = true,
        }
    } else {
        queue.push(BlsCheck {
            sig: qc.members_sig.value().clone(),
            keys: signer_keys(qc, members),
            hash,
            scheme,
            reject: CommitmentReject::MembersSignature,
            unsure: false,
        });
    }
    queue.push(BlsCheck {
        sig: qc.quorum_sig.value().clone(),
        keys: vec![qc.quorum_public_key.value().clone()],
        hash,
        scheme,
        reject: CommitmentReject::QuorumSignature,
        unsure,
    });
}

/// `BuildCommitmentHash` of `qc` in the scheme of `bls`.
fn commitment_sig_hash(bls: BlsCtx, qc: &LlmqCommitment) -> [u8; 32] {
    build_commitment_hash(
        qc.llmq_type,
        &qc.quorum_hash,
        &qc.valid_members,
        &qc.quorum_public_key,
        qc.quorum_vvec_hash.as_bytes(),
        bls,
    )
}

/// The operator keys of the members at the commitment's signer bits. `signers[i]` for every
/// member: Verify's size check makes the bitset as long as the quorum, which no member list
/// exceeds.
fn signer_keys<T: OperatorKeyView>(
    qc: &LlmqCommitment,
    members: &QuorumMembers<T>,
) -> Vec<BlsPkValue> {
    // Sized for the signers: a filtered `collect` grows from 4 and reallocates 5 times for a
    // quorum of 60, and the queue keeps every commitment's keys alive at once.
    let signers = usize::try_from(qc.signers.count_ones()).unwrap_or(usize::MAX);
    let mut keys = Vec::with_capacity(signers.min(members.len()));
    for (i, m) in members.iter().enumerate() {
        if bit_set(&qc.signers, i as u64) {
            keys.push(m.operator_key());
        }
    }
    keys
}

/// `VerifySignatureAsync` on `path`, after `Verify`'s checks, with the members `Verify` read.
/// (On the mined path Core runs `CheckLLMQCommitment`'s structural `Verify(…, false)` on every
/// commitment transaction first, then the signature checks, then `ProcessCommitment`'s
/// `Verify(…, false)`: the order here.)
///
/// Core rejects when either check fails (`Sync`: in order; `Mined`: any queued check), so a
/// definite `false` of one decides even when the other has no BLS verdict.
// Core: llmq/commitment.cpp:35 CFinalCommitment::VerifySignatureAsync; llmq/utils.cpp:561 BlsCheck
fn verify_signatures<E, T: OperatorKeyView>(
    path: SigPath,
    bls: BlsCtx,
    single_member: bool,
    qc: &LlmqCommitment,
    members: &QuorumMembers<T>,
) -> Result<(), CommitmentError<E>> {
    if path == SigPath::Mined {
        let mut queue = Vec::new();
        queue_signatures(bls, single_member, qc, members, &mut queue);
        return queued_verdict(&queue);
    }
    if single_member && members.is_empty() {
        return reject(CommitmentReject::NoMembers);
    }
    let hash = commitment_sig_hash(bls, qc);
    let flag = bls.scheme();
    let members_sig: &BlsSigValue = qc.members_sig.value();
    let (members_verdict, members_reject) = if single_member {
        let verdict = verify_insecure(members_sig, &members[0].operator_key(), &hash, flag);
        (verdict, CommitmentReject::MemberSignature)
    } else {
        let keys = signer_keys(qc, members);
        let keys: Vec<&BlsPkValue> = keys.iter().collect();
        let verdict = verify_secure_aggregated(members_sig, &keys, &hash, flag);
        (verdict, CommitmentReject::MembersSignature)
    };
    if members_verdict == BlsVerdict::Invalid {
        return reject(members_reject);
    }
    let quorum_verdict =
        verify_insecure(qc.quorum_sig.value(), qc.quorum_public_key.value(), &hash, flag);
    check(quorum_verdict, CommitmentReject::QuorumSignature)?;
    check(members_verdict, members_reject)
}

fn check<E>(verdict: BlsVerdict, on_invalid: CommitmentReject) -> Result<(), CommitmentError<E>> {
    match verdict {
        BlsVerdict::Valid => Ok(()),
        BlsVerdict::Invalid => reject(on_invalid),
        BlsVerdict::Unsupported => Err(CommitmentError::BlsUnsupported),
    }
}

/// The identity and public key of a quorum, the only key locks are verified against (SB0 §5.1:
/// SPV's `Verified`). The key comes from a commitment that passed [`verify_commitment`] with
/// signatures, or, behind the `reload` or `mined` feature, from a recorded verdict or a mined
/// commitment the node's validation accepted: not every value attests that its signatures were
/// checked. It holds no member list. [`verify_quorum`] builds one; so does `reloaded`, behind
/// `reload`, for a caller restoring its own recorded verdict (dash-mnlist's restart, DEC-412); and
/// so does `mined`, behind `mined`, for the node's validation-accepted mined commitments, which
/// Core never verifies again (OD-CP-2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedQuorum {
    llmq_type: LlmqType,
    quorum_hash: QuorumHash,
    quorum_index: i16,
    public_key: BlsPkValue,
}

impl VerifiedQuorum {
    /// `qc->llmqType`.
    pub fn llmq_type(&self) -> LlmqType {
        self.llmq_type
    }

    /// `qc->quorumHash`.
    pub fn quorum_hash(&self) -> &QuorumHash {
        &self.quorum_hash
    }

    /// `qc->quorumIndex`: 0 for a version that carries none.
    pub fn quorum_index(&self) -> i16 {
        self.quorum_index
    }

    /// `qc->quorumPublicKey`, as decoded under the commitment version's scheme.
    pub fn public_key(&self) -> &BlsPkValue {
        &self.public_key
    }

    /// A quorum taken as verified, for the lock checks' tests.
    #[cfg(test)]
    pub(crate) fn for_tests(
        llmq_type: LlmqType,
        quorum_hash: QuorumHash,
        quorum_index: i16,
        public_key: BlsPkValue,
    ) -> VerifiedQuorum {
        VerifiedQuorum {
            llmq_type,
            quorum_hash,
            quorum_index,
            public_key,
        }
    }

    /// The value [`verify_quorum`] returned for `qc`, rebuilt from a store that recorded that
    /// verdict (SPV's restart, `dash-mnlist` D38). It is read from the commitment alone, so it
    /// verifies nothing: only a caller reloading its own recorded verdict may use it, and only
    /// dash-mnlist enables `reload` (DEC-412). Hidden from the docs so no other caller takes it
    /// for a way to verify.
    #[cfg(feature = "reload")]
    #[doc(hidden)]
    pub fn reloaded(qc: &LlmqCommitment) -> VerifiedQuorum {
        VerifiedQuorum::of(qc)
    }

    /// The key of a commitment the node's block validation accepted and mined, as Core takes it:
    /// `BuildQuorumFromCommitment` keeps `GetMinedCommitment`'s `qc` as it is, and a lock is
    /// checked against `qc->quorumPublicKey` with no second look at the commitment. That holds
    /// for commitments connected under `-assumevalid` too, whose signatures were never checked.
    ///
    /// It verifies nothing. Only `dashd-chainstate` enables `mined` (OD-CP-2, widening DEC-412),
    /// for mined rows its own validation wrote; its dependents see it by Cargo's unification and
    /// may call it only for such rows (or, in test harnesses, for a recorded chain's).
    // Core: v24.0.0-rc.4 llmq/quorumsman.cpp:58-105 BuildQuorumFromCommitment, :795-796
    // VerifyRecoveredSig
    #[cfg(any(test, feature = "mined"))]
    pub fn mined(qc: &LlmqCommitment) -> VerifiedQuorum {
        VerifiedQuorum::of(qc)
    }

    pub(crate) fn of(qc: &LlmqCommitment) -> VerifiedQuorum {
        VerifiedQuorum {
            llmq_type: qc.llmq_type,
            quorum_hash: qc.quorum_hash,
            quorum_index: qc.quorum_index.unwrap_or(0),
            public_key: qc.quorum_public_key.value().clone(),
        }
    }
}

/// [`verify_commitment`] with signatures on `path`; on success, the quorum as a
/// [`VerifiedQuorum`]. SPV's §5.2 step 5 verifies a mined commitment ([`SigPath::Mined`]): a
/// failure leaves the quorum `Unverified`.
pub fn verify_quorum<E, M>(
    ctx: &LlmqCtx<'_, E>,
    members: &M,
    qc: &LlmqCommitment,
    base: BlockId,
    path: SigPath,
    effects: &mut dyn EffectSink,
) -> Result<VerifiedQuorum, CommitmentError<E>>
where
    M: QuorumMemberSource<Error = E> + ?Sized,
    M::Entry: OperatorKeyView,
{
    verify_commitment(ctx, members, qc, base, Some(path), effects)?;
    Ok(VerifiedQuorum::of(qc))
}

/// A [`QuorumMemberSource`] with no cache: [`members_uncached`] over a list source and snapshot
/// rows. SPV's `membersSig` input over its simplified lists (SB0 §5.1, §5.2 step 5), and the
/// node's tests and comparators. Node consensus paths use CH7b's facade instead (LQ1 §3.7.4).
pub struct UncachedMembers<'a, 'r, E, L: MnListView> {
    ctx: LlmqCtx<'a, E>,
    src: &'a dyn LlmqSource<L, Error = E>,
    rows: RefCell<&'r mut dyn SnapshotRows<Error = E>>,
}

impl<'a, 'r, E, L: MnListView> UncachedMembers<'a, 'r, E, L> {
    /// Members of quorums read through `ctx` and `src`; a rotated computation reads and stores
    /// its snapshot rows in `rows`.
    pub fn new(
        ctx: LlmqCtx<'a, E>,
        src: &'a dyn LlmqSource<L, Error = E>,
        rows: &'r mut dyn SnapshotRows<Error = E>,
    ) -> Self {
        UncachedMembers {
            ctx,
            src,
            rows: RefCell::new(rows),
        }
    }
}

impl<E, L: MnListView> QuorumMemberSource for UncachedMembers<'_, '_, E, L> {
    type Entry = L::Entry;
    type Error = E;

    fn members(
        &self,
        llmq_type: LlmqType,
        base: BlockId,
        _effects: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<L::Entry>, LlmqError<E>> {
        let mut rows = self.rows.borrow_mut();
        members_uncached(&self.ctx, self.src, &mut **rows, llmq_type, base)
    }
}

#[cfg(test)]
#[path = "commitment_tests.rs"]
mod tests;
