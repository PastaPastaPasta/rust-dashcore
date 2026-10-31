//! Lock verification (SB8; SB0 §5.2 step 6): ChainLocks, InstantSend locks and Core's
//! selection-based recovered-signature check, over a scan whose entries each carry their local
//! verification status; Core's verdict on a `qsigrec` against the quorum it names ([`recsig`],
//! LQ3a); and the messages' wire hashes and signature reads ([`wire`], LQ3a).
//!
//! **Selection first, then status (DEC-105 F4).** The quorum a lock is checked against is the one
//! Core's `SelectQuorumForSigning` picks over Core's whole scan ([`select_from_scan`], LQ1 §3.9).
//! Only that commitment's status is then read: a [`QuorumStatus::Verified`] quorum's key verifies
//! the signature; any other status makes the lock [`LockReject::QuorumUnavailable`]. No other
//! quorum is ever substituted, so a local status costs liveness and never safety (SB0 §5.1).
//!
//! The view ([`QuorumScanView`]) supplies the scan: the node from its quorum manager (every
//! entry verified at connect), SPV from the authenticated set it holds for the selection start
//! block (SB0 §5.2.1, SB11).
//!
//! Signatures are verified one at a time with BS12's serial `bls::core` (no threads):
//! individual verification; possible security note: see manager (SB8 D8, LQ3a D2).

#[cfg(test)]
mod capture_tests;
pub mod recsig;
#[cfg(test)]
mod recsig_tests;
#[cfg(test)]
mod tests;
pub mod wire;
#[cfg(test)]
mod wire_tests;

pub use recsig::{
    RecSigBan, RecSigError, RecSigIgnore, RecSigVerdict, is_quorum_active, serves_recovered_sig,
    verify_qsigrec,
};
pub use wire::{
    ClsigHashSchemes, RecoveredSig, SigRead, clsig_hash, isdlock_hash, read_eager_sig,
    read_lazy_sig,
};

use std::collections::BTreeSet;
use std::fmt;

use dash_consensus::BlockId;
use dash_consensus::hash::HashWriter;
use dash_consensus::primitives::{CoreWrite, write_compact_size};
use dash_consensus::tx::max_block_size;
use dash_p2p_core::{ClSig, IsdLock};
use dash_params::{ChainParams, LlmqParams};
use dash_pkc::bls::BlsGlobalSig;
use dash_pkc::bls::core::{BlsSigValue, BlsVerdict, verify_insecure};
use dash_primitives::{BlockHash, LlmqType, OutPoint, QuorumHash, TxHash};
use dash_types::Numeric;

use crate::bls::BlsCtx;
use crate::llmq::{
    SIGN_HEIGHT_OFFSET, ScanEntry, VerifiedQuorum, select_from_scan, select_start_height,
};

/// Core's `InstantSendLock::MAX_INPUTS`: `MaxBlockSize() / 41` (`instantsend/lock.h` v24:31).
pub const ISLOCK_MAX_INPUTS: usize = max_block_size(true) as usize / 41;

/// Core's `chainlock::GenSigRequestId(nHeight)`: `SerializeHash(pair(string_view "clsig",
/// int32 nHeight))`.
// Core: chainlock/clsig.cpp:248 GenSigRequestId
pub fn chainlock_request_id(height: i32) -> [u8; 32] {
    let mut w = HashWriter::new();
    write_prefix(b"clsig", &mut w);
    w.write(&height.to_le_bytes());
    w.finish()
}

/// Core's `InstantSendLock::GetRequestId()`: the hash of the prefix "islock" and the inputs
/// vector.
// Core: instantsend/lock.cpp:18 InstantSendLock::GetRequestId
pub fn islock_request_id(inputs: &[OutPoint]) -> [u8; 32] {
    let mut w = HashWriter::new();
    write_prefix(b"islock", &mut w);
    write_compact_size(inputs.len() as u64, &mut w);
    for input in inputs {
        w.write(input.hash.as_bytes());
        w.write(&input.index.to_le_bytes());
    }
    w.finish()
}

/// A `std::string_view` serializes as a compact size and its bytes (`serialize.h` v24).
fn write_prefix(prefix: &[u8], w: &mut impl CoreWrite) {
    write_compact_size(prefix.len() as u64, w);
    w.write(prefix);
}

/// Core's `llmq::SignHash{type, quorumHash, id, msgHash}`: the message a quorum's recovered
/// signature signs.
// Core: llmq/signhash.cpp:14 SignHash::SignHash
pub fn sign_hash(
    llmq_type: LlmqType,
    quorum_hash: &QuorumHash,
    id: &[u8; 32],
    msg_hash: &[u8; 32],
) -> [u8; 32] {
    let mut w = HashWriter::new();
    w.write(&[llmq_type.to_base()]);
    w.write(quorum_hash.as_bytes());
    w.write(id);
    w.write(msg_hash);
    w.finish()
}

/// A scanned commitment's local status (SB0 §5.1, §5.2 steps 5-6).
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "a scan's entries are mostly verified: a box would allocate per entry"
)]
pub enum QuorumStatus {
    /// The commitment verified with signatures; its key verifies locks.
    Verified(VerifiedQuorum),
    /// The commitment did not verify locally (a reconstruction it could not check, or a check
    /// outside BS12's domain). Locks that select it are unavailable.
    Unverified,
    /// The commitment's verification inputs are not held yet (a list, a `qrinfo`, a block below
    /// the anchor). Locks that select it are unavailable.
    Unavailable,
}

/// One entry of Core's scan as the lock checks read it: the quorum's base, hash and index, which
/// selection reads, and its status, which only the selected entry's check reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannedQuorum {
    base: BlockId,
    quorum_hash: QuorumHash,
    quorum_index: i16,
    status: QuorumStatus,
}

impl ScannedQuorum {
    /// A verified quorum based at `base`.
    pub fn verified(base: BlockId, quorum: VerifiedQuorum) -> ScannedQuorum {
        ScannedQuorum {
            base,
            quorum_hash: *quorum.quorum_hash(),
            quorum_index: quorum.quorum_index(),
            status: QuorumStatus::Verified(quorum),
        }
    }

    /// A quorum in the scan whose commitment did not verify locally ([`QuorumStatus::Unverified`]).
    pub fn unverified(base: BlockId, quorum_hash: QuorumHash, quorum_index: i16) -> ScannedQuorum {
        ScannedQuorum {
            base,
            quorum_hash,
            quorum_index,
            status: QuorumStatus::Unverified,
        }
    }

    /// A quorum in the scan whose verification inputs are not held ([`QuorumStatus::Unavailable`]).
    pub fn unavailable(base: BlockId, quorum_hash: QuorumHash, quorum_index: i16) -> ScannedQuorum {
        ScannedQuorum {
            base,
            quorum_hash,
            quorum_index,
            status: QuorumStatus::Unavailable,
        }
    }

    /// The entry's status.
    pub fn status(&self) -> &QuorumStatus {
        &self.status
    }
}

impl ScanEntry for ScannedQuorum {
    fn base(&self) -> BlockId {
        self.base
    }
    fn quorum_hash(&self) -> QuorumHash {
        self.quorum_hash
    }
    fn quorum_index(&self) -> i16 {
        self.quorum_index
    }
}

/// Core's `ScanQuorums(type, start, signingActiveQuorumCount)` at a selection start block, with
/// `IsQuorumRotationEnabled(params, start)`, which selects the selection rule.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuorumScan {
    /// `IsQuorumRotationEnabled(llmq_params, pindexStart)`.
    pub rotation_enabled: bool,
    /// The scan, newest first, every entry whatever its status, in Core's order and count
    /// (LQ1 §3.8): empty where Core's is.
    pub entries: Vec<ScannedQuorum>,
}

/// What a view holds for a selection start block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanLookup {
    /// The scan Core would compute there.
    Held(QuorumScan),
    /// The view does not hold the set at that block yet (SPV fetches it, SB0 §5.2.1).
    Unavailable,
    /// The block is below the view's recent-lock horizon (SB0 §5.2.1 rule 4).
    OutsideHorizon,
}

/// The chain and quorum state a lock check reads (SB0 §5.2 step 6).
pub trait QuorumScanView {
    /// The view's read error.
    type Error;

    /// The active chain's tip height (`active_chain.Height()`; for InstantSend, the manager's
    /// cached tip).
    ///
    /// One tip for both of Core's reads, a simplification: Core's InstantSend check takes the
    /// sign height from the manager's cached tip (`m_is_manager.GetTipHeight()`,
    /// `instantsend/net_instantsend.cpp` v24:143-148) and the `-1` substitution and the start
    /// block bound from `active_chain.Height()` (`llmq/quorumsman.cpp` v24:711-722). They agree
    /// whenever the cached tip is the active one and differ in Core only transiently; a view that
    /// needs Core's transient answer must split this method.
    fn tip_height(&self) -> u32;

    /// [`QuorumScan`] at the active chain's block at `start_height` (at most the tip).
    fn scan(&self, llmq_type: LlmqType, start_height: u32) -> Result<ScanLookup, Self::Error>;

    /// Core's `ScanQuorums(type, count)` at the active chain's tip: the type's newest `count`
    /// quorums, every entry whatever its status, in Core's order and count. `IsQuorumActive`
    /// reads it with `count = keepOldConnections` (LQ3a), a set at least as large as
    /// [`Self::scan`]'s `signingActiveQuorumCount`.
    ///
    /// As Core's (`llmq/quorumsman.cpp` v24:170-268): empty when the type is not enabled at the
    /// tip; counted from the tip's cycle-adjusted store block (the previous cycle's mining end
    /// before this cycle's mining window, this cycle's after it); empty when any quorum it lists
    /// cannot be built.
    fn scan_at_tip(&self, llmq_type: LlmqType, count: usize) -> Result<ScanLookup, Self::Error>;

    /// `LookupBlockIndex(hash)->nHeight`: any header the view knows, on the active chain or not;
    /// `None` when it knows none.
    fn block_height(&self, hash: &BlockHash) -> Result<Option<u32>, Self::Error>;

    /// Core's `GetQuorum(type, quorumHash)`: `None` where Core's is null (no such block, or no
    /// commitment of the type mined for it), else the quorum's status.
    fn quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<Option<QuorumStatus>, Self::Error>;
}

/// Why a lock is not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockReject {
    /// Core's `VerifyRecSigStatus::NoQuorum`: no start block, or no quorum selected.
    NoQuorum,
    /// The selected quorum is not [`QuorumStatus::Verified`], or the view does not hold the set
    /// it selects from. Not Core's answer: the lock may be valid.
    QuorumUnavailable,
    /// The selection start block is below the view's recent-lock horizon. Not Core's answer.
    OutsideHorizon,
    /// Core's `VerifyRecSigStatus::Invalid`: the selected quorum's key rejects the signature.
    InvalidSignature,
    /// Core cannot read the signature: a ChainLock's bytes are canonical in neither scheme (its
    /// `Unserialize` throws), or an InstantSend lock's do not decode under the scheme
    /// (`sig.Get()` is invalid).
    MalformedSignature,
    /// An InstantSend lock with more than [`ISLOCK_MAX_INPUTS`] inputs, or not `TriviallyValid`:
    /// a null txid, no inputs, or a repeated input.
    Malformed,
    /// An InstantSend lock whose cycle hash names no block the view knows.
    UnknownCycle,
    /// An InstantSend lock whose cycle block is not at a multiple of `dkgInterval`.
    InvalidCycle,
}

/// A lock check's failure: a verdict, or no verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockError<E> {
    /// The lock is not accepted.
    Rejected(LockReject),
    /// The signature check depends on arithmetic BS12 does not reproduce
    /// ([`BlsVerdict::Unsupported`]).
    BlsUnsupported,
    /// The lock's LLMQ type is not configured.
    UnknownLlmqType(u8),
    /// The view failed.
    View(E),
}

impl<E: fmt::Display> fmt::Display for LockError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockError::Rejected(r) => write!(f, "lock rejected: {r:?}"),
            LockError::BlsUnsupported => f.write_str("lock signature outside the BLS domain"),
            LockError::UnknownLlmqType(t) => write!(f, "unknown LLMQ type {t}"),
            LockError::View(e) => write!(f, "lock view: {e}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for LockError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LockError::View(e) => Some(e),
            _ => None,
        }
    }
}

fn rejected<T, E>(r: LockReject) -> Result<T, LockError<E>> {
    Err(LockError::Rejected(r))
}

fn llmq<E>(params: &ChainParams, llmq_type: LlmqType) -> Result<&LlmqParams, LockError<E>> {
    params.consensus.llmq(llmq_type).ok_or(LockError::UnknownLlmqType(llmq_type.to_base()))
}

/// `sig.VerifyInsecure(qc->quorumPublicKey, SignHash{...})` against the quorum, which must be of
/// the type the lock is checked under: a view answering with another type's quorum gives no
/// verdict ([`LockReject::QuorumUnavailable`]), never a check under the wrong `SignHash`.
fn verify_with<E>(
    llmq_type: LlmqType,
    quorum: &VerifiedQuorum,
    id: &[u8; 32],
    msg_hash: &[u8; 32],
    sig: &BlsSigValue,
    bls: BlsCtx,
) -> Result<(), LockError<E>> {
    if quorum.llmq_type() != llmq_type {
        return rejected(LockReject::QuorumUnavailable);
    }
    let hash = sign_hash(llmq_type, quorum.quorum_hash(), id, msg_hash);
    match verify_insecure(sig, quorum.public_key(), &hash, bls.scheme()) {
        BlsVerdict::Valid => Ok(()),
        BlsVerdict::Invalid => rejected(LockReject::InvalidSignature),
        BlsVerdict::Unsupported => Err(LockError::BlsUnsupported),
    }
}

/// The selected quorum's key, if it may verify the lock.
fn require_verified<E>(status: &QuorumStatus) -> Result<&VerifiedQuorum, LockError<E>> {
    match status {
        QuorumStatus::Verified(q) => Ok(q),
        QuorumStatus::Unverified | QuorumStatus::Unavailable => {
            rejected(LockReject::QuorumUnavailable)
        }
    }
}

/// Core's `SelectQuorumForSigning(params, active_chain, qman, selectionHash, signHeight,
/// signOffset)`, then the selected entry's status: its verified quorum.
// Core: llmq/quorumsman.cpp:777 SelectQuorumForSigning, :711 SelectQuorumForSigningStartBlock
fn select_verified<V: QuorumScanView + ?Sized>(
    params: &LlmqParams,
    view: &V,
    selection_hash: &[u8; 32],
    sign_height: i32,
    sign_offset: i32,
) -> Result<VerifiedQuorum, LockError<V::Error>> {
    let tip = i32::try_from(view.tip_height()).unwrap_or(i32::MAX);
    let Some(start) = select_start_height(tip, sign_height, sign_offset) else {
        return rejected(LockReject::NoQuorum);
    };
    let scan = match view.scan(params.llmq_type, start).map_err(LockError::View)? {
        ScanLookup::Held(scan) => scan,
        ScanLookup::Unavailable => return rejected(LockReject::QuorumUnavailable),
        ScanLookup::OutsideHorizon => return rejected(LockReject::OutsideHorizon),
    };
    let Some(selected) =
        select_from_scan(params, scan.rotation_enabled, &scan.entries, selection_hash)
    else {
        return rejected(LockReject::NoQuorum);
    };
    require_verified(&selected.status).cloned()
}

/// Core's `VerifyRecoveredSig(type, active_chain, qman, signedAtHeight, id, msgHash, sig,
/// signOffset)`: the quorum selected for `id` at `sign_height` (-1: the tip) and `sign_offset`
/// verifies `sig` over the sign hash. On success, the quorum that signed.
///
/// Core's `qsigrec` intake does not come here: it verifies a recovered signature against the
/// quorum the message names, a lookup, not a selection ([`verify_qsigrec`]).
// Core: llmq/quorumsman.cpp:849 VerifyRecoveredSig
#[allow(clippy::too_many_arguments, reason = "Core's signature")]
pub fn verify_recovered_sig<V: QuorumScanView + ?Sized>(
    params: &ChainParams,
    view: &V,
    bls: BlsCtx,
    llmq_type: LlmqType,
    sign_height: i32,
    sign_offset: i32,
    id: &[u8; 32],
    msg_hash: &[u8; 32],
    sig: &BlsSigValue,
) -> Result<VerifiedQuorum, LockError<V::Error>> {
    let llmq = llmq(params, llmq_type)?;
    let quorum = select_verified(llmq, view, id, sign_height, sign_offset)?;
    verify_with(llmq_type, &quorum, id, msg_hash, sig, bls)?;
    Ok(quorum)
}

/// A ChainLock whose signature the selected quorum verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedChainLock {
    /// The locked block's height.
    pub height: i32,
    /// The locked block's hash.
    pub block_hash: BlockHash,
    /// The quorum that signed it.
    pub quorum: VerifiedQuorum,
}

/// Core's `chainlock::VerifyChainLock(params, chain, qman, clsig)` for a ChainLock's height,
/// block hash and signature (as read under the caller's scheme): the ChainLock type's quorum
/// selected for the request id at `height - SIGN_HEIGHT_OFFSET` verifies the signature over the
/// block hash. Also what checks a CbTx's best ChainLock (`chainlock::VerifyChainLock` at
/// `evo/specialtxman.cpp` v24:202); Core skips that call when the CbTx carries the ChainLock its
/// handler already holds as best (a cache, SB8 D20), which this function does not model.
// Core: chainlock/clsig.cpp:253 VerifyChainLock
pub fn verify_chainlock_sig<V: QuorumScanView + ?Sized>(
    params: &ChainParams,
    view: &V,
    bls: BlsCtx,
    height: i32,
    block_hash: &BlockHash,
    sig: &BlsSigValue,
) -> Result<VerifiedChainLock, LockError<V::Error>> {
    let quorum = verify_recovered_sig(
        params,
        view,
        bls,
        params.consensus.llmq_type_chain_locks,
        height,
        SIGN_HEIGHT_OFFSET as i32,
        &chainlock_request_id(height),
        block_hash.as_bytes(),
        sig,
    )?;
    Ok(VerifiedChainLock {
        height,
        block_hash: *block_hash,
        quorum,
    })
}

/// The two reads of the process-wide BLS flag that a `qsigrec` or `clsig` check makes: `read`
/// when the message is deserialized, `check` when its signature is verified. Core holds no lock
/// across them, so they differ when the V19 block connects or disconnects in between (LQ3a D18,
/// D21). Named fields, so a caller cannot swap them by position (LQ3a D26).
///
/// `read` belongs to the message: the node (LK1) samples it when it decodes the message and
/// carries it with the message to the check, and samples only `check` there (LQ3a D27).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LockSchemes {
    /// The flag when the message was read.
    pub read: BlsCtx,
    /// The flag when the signature is checked.
    pub check: BlsCtx,
}

impl LockSchemes {
    /// One scheme for the read and the check: no switch between them.
    pub const fn both(bls: BlsCtx) -> Self {
        LockSchemes {
            read: bls,
            check: bls,
        }
    }
}

/// A `clsig` message: its signature read as Core reads it under `schemes.read` (eagerly; bytes
/// canonical in neither scheme throw), then [`verify_chainlock_sig`] under `schemes.check`.
///
/// `schemes.read` is the flag when the message is deserialized (`net_processing.cpp` v24:5797),
/// `schemes.check` the flag when the handler verifies it (`chainlock/handler.cpp` v24:156,
/// `bls/bls.cpp` v24:311); see [`LockSchemes`] (LQ3a D21).
// Core: chainlock/chainlock.h:41 ChainLockSig SERIALIZE_METHODS; chainlock/clsig.cpp:263 VerifyChainLock
pub fn verify_chainlock<V: QuorumScanView + ?Sized>(
    params: &ChainParams,
    view: &V,
    schemes: LockSchemes,
    clsig: &ClSig,
) -> Result<VerifiedChainLock, LockError<V::Error>> {
    let Ok(sig) = BlsGlobalSig::decode(clsig.sig.as_bytes()) else {
        return rejected(LockReject::MalformedSignature);
    };
    verify_chainlock_sig(
        params,
        view,
        schemes.check,
        clsig.height,
        &clsig.block_hash,
        sig.get(schemes.read).value(),
    )
}

/// The schemes an `isdlock`'s two verification passes check under: Core's flag when each pass
/// runs. Core's worker runs the current active set's pass and then the previous set's as two
/// calls, each reading the flag afresh, with no lock held across them
/// (`instantsend/net_instantsend.cpp` v24:312-327, :339-353; `bls/bls.cpp` v24:335). The node
/// (LK1) samples the flag at the start of each pass. That is a timing Core can show, not every
/// interleaving Core's worker allows: LK1 owns the schedule of a pass's checks. The two differ
/// when the V19 block connects or disconnects between the passes (LQ3a D22). The quorum view
/// stays one snapshot (D6): the flag flips inside `ConnectBlock`, before the tip moves, so a pass
/// can see the new flag with the old chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IslockPassSchemes {
    /// The current active set's pass (offset 0).
    pub current: BlsCtx,
    /// The previous active set's pass (offset `dkgInterval`).
    pub previous: BlsCtx,
}

impl IslockPassSchemes {
    /// Both passes under one scheme: no switch between them.
    pub const fn both(bls: BlsCtx) -> Self {
        IslockPassSchemes {
            current: bls,
            previous: bls,
        }
    }
}

/// The flag reads of an `isdlock` check: `read` when the message is read (the lazy signature
/// keeps it, `bls/bls.h` v24:410, :500), and each pass's check scheme in `passes`. As with
/// [`LockSchemes`], `read` belongs to the message: LK1 samples it at decode and carries it to the
/// passes (LQ3a D26, D27).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IslockSchemes {
    /// The flag when the message was read.
    pub read: BlsCtx,
    /// The flag when each pass checks.
    pub passes: IslockPassSchemes,
}

impl IslockSchemes {
    /// One scheme for the read and both passes: no switch between them.
    pub const fn both(bls: BlsCtx) -> Self {
        IslockSchemes {
            read: bls,
            passes: IslockPassSchemes::both(bls),
        }
    }
}

/// An InstantSend lock whose signature a quorum verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedIsLock {
    /// The locked transaction.
    pub txid: TxHash,
    /// The lock's request id.
    pub request_id: [u8; 32],
    /// The quorum that signed it.
    pub quorum: VerifiedQuorum,
}

/// Core's `InstantSendLock::TriviallyValid()`: a txid, at least one input and at most
/// [`ISLOCK_MAX_INPUTS`], each input once.
// Core: instantsend/lock.cpp:30 InstantSendLock::TriviallyValid
pub fn islock_trivially_valid(islock: &IsdLock) -> bool {
    if islock.txid.as_bytes() == &[0u8; 32]
        || islock.inputs.is_empty()
        || islock.inputs.len() > ISLOCK_MAX_INPUTS
    {
        return false;
    }
    let mut seen = BTreeSet::new();
    islock.inputs.iter().all(|input| seen.insert((*input.hash.as_bytes(), input.index)))
}

/// Core's checks of an `isdlock` (`instantsend/net_instantsend.cpp` v24): the size and
/// `TriviallyValid` (:253, :262), the cycle block known (:266) and at a cycle boundary (:276),
/// the signature readable (:122), then verification against the InstantSend type's quorum:
/// rotated, the quorum selected for the request id at the lock's sign height (the tip, or
/// `cycleHeight + dkgInterval - 1` once the tip is past the cycle); otherwise the quorum the
/// cycle hash names (:142-160). Core tries the current active set (offset 0) and then the
/// previous one (offset `dkgInterval`, :339-353); either verifying accepts the lock.
///
/// A pass whose quorum is not verified decides nothing: the lock is accepted when a pass
/// verifies, rejected when every pass is Core's rejection, and otherwise unavailable. A rejection
/// is the last pass's: Core's response to the lock follows it alone, because the first pass runs
/// without scoring the peer (:340-353). So a lock the current set rejects and the previous set
/// has no quorum for is [`LockReject::NoQuorum`], not [`LockReject::InvalidSignature`]
/// (O2a-LQ3 D23, BL-209, LQ3a D32).
///
/// The signature is a lazy field: it reads under `schemes.read`, the flag when the message was
/// read (the wrapper keeps it, `bls/bls.h` v24:410, :500). Each pass checks it under its own
/// scheme in `schemes.passes`, the flag when that pass runs (`bls/bls.cpp` v24:335; :311 is the
/// individual check). They differ when the V19 block connects or disconnects in between (LQ3a
/// D21, D22).
// Core: instantsend/net_instantsend.cpp:237 ProcessMessage, :104 BuildVerificationBatch,
// :330 ProcessPendingISLocks
pub fn verify_islock<V: QuorumScanView + ?Sized>(
    params: &ChainParams,
    view: &V,
    schemes: IslockSchemes,
    islock: &IsdLock,
) -> Result<VerifiedIsLock, LockError<V::Error>> {
    if !islock_trivially_valid(islock) {
        return rejected(LockReject::Malformed);
    }
    let cycle_height = if islock.cycle_hash.as_bytes() == &[0u8; 32] {
        None
    } else {
        view.block_height(&islock.cycle_hash).map_err(LockError::View)?
    };
    let Some(cycle_height) = cycle_height else {
        return rejected(LockReject::UnknownCycle);
    };
    let llmq = llmq(params, params.consensus.llmq_type_dip0024_instant_send)?;
    // `dkgInterval` is positive in every configured table (`LlmqCtx::new` checks the domain).
    let interval = llmq.dkg_interval;
    if interval < 1 || i64::from(cycle_height) % i64::from(interval) != 0 {
        return rejected(LockReject::InvalidCycle);
    }
    let SigRead::Valid(sig) = read_lazy_sig(islock.sig.as_bytes(), schemes.read) else {
        return rejected(LockReject::MalformedSignature);
    };
    let id = islock_request_id(&islock.inputs);
    let tip = i64::from(view.tip_height());
    let sign_height = if i64::from(cycle_height) + i64::from(interval) < tip {
        // Below the tip, so inside `int`.
        (i64::from(cycle_height) + i64::from(interval) - 1) as i32
    } else {
        -1
    };

    let mut outcomes = Vec::with_capacity(2);
    let passes = schemes.passes;
    for (offset, check) in [(0, passes.current), (interval, passes.previous)] {
        match islock_pass(llmq, view, islock, &id, &sig, sign_height, offset, check) {
            Ok(quorum) => {
                return Ok(VerifiedIsLock {
                    txid: islock.txid,
                    request_id: id,
                    quorum,
                });
            }
            Err(LockError::View(e)) => return Err(LockError::View(e)),
            Err(other) => outcomes.push(other),
        }
    }
    Err(combine(outcomes))
}

/// One pass of `BuildVerificationBatch` and the batch's verdict for the lock.
// Core: instantsend/net_instantsend.cpp:142-160
#[allow(clippy::too_many_arguments, reason = "one pass of Core's loop body")]
fn islock_pass<V: QuorumScanView + ?Sized>(
    llmq: &LlmqParams,
    view: &V,
    islock: &IsdLock,
    id: &[u8; 32],
    sig: &BlsSigValue,
    sign_height: i32,
    offset: i32,
    bls: BlsCtx,
) -> Result<VerifiedQuorum, LockError<V::Error>> {
    let quorum = if llmq.use_rotation {
        select_verified(llmq, view, id, sign_height, offset)?
    } else {
        // Non-rotated: the cycle hash names the quorum (`GetQuorum(type, cycleHash)`).
        let quorum_hash = QuorumHash::from(*islock.cycle_hash.as_bytes());
        let quorum = match view.quorum(llmq.llmq_type, &quorum_hash).map_err(LockError::View)? {
            None => return rejected(LockReject::NoQuorum),
            Some(status) => require_verified(&status)?.clone(),
        };
        // The view's answer must be the quorum the cycle hash names.
        if quorum.quorum_hash() != &quorum_hash {
            return rejected(LockReject::QuorumUnavailable);
        }
        quorum
    };
    verify_with(llmq.llmq_type, &quorum, id, islock.txid.as_bytes(), sig, bls)?;
    Ok(quorum)
}

/// The answer of the passes, in pass order, none of which verified: the last pass's rejection
/// when every pass is Core's rejection (Core's response follows the last pass), else no verdict,
/// else unavailable, else outside the horizon. An outcome a pass cannot give is returned as it
/// is.
fn combine<E>(outcomes: Vec<LockError<E>>) -> LockError<E> {
    let (mut last, mut unsupported, mut unavailable, mut outside) =
        (LockReject::NoQuorum, false, false, false);
    for outcome in outcomes {
        match outcome {
            LockError::Rejected(r @ (LockReject::InvalidSignature | LockReject::NoQuorum)) => {
                last = r;
            }
            LockError::BlsUnsupported => unsupported = true,
            LockError::Rejected(LockReject::QuorumUnavailable) => unavailable = true,
            LockError::Rejected(LockReject::OutsideHorizon) => outside = true,
            other => return other,
        }
    }
    if unsupported {
        LockError::BlsUnsupported
    } else if unavailable {
        LockError::Rejected(LockReject::QuorumUnavailable)
    } else if outside {
        LockError::Rejected(LockReject::OutsideHorizon)
    } else {
        LockError::Rejected(last)
    }
}
