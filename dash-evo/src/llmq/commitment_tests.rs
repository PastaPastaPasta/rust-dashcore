//! SB8's verifier-only cases (EV addendum m3): Core's `CFinalCommitment::Verify` over an injected
//! member source, every rejection in Core's order, and the SB0 §5.1 mutation gate: `membersSig`
//! binds the multiset of signing keys and nothing else.

use std::cell::Cell;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_params::{BuriedDeployment, ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_pkc::bls::core::{BlsPkValue, BlsSigValue};
use dash_pkc::bls::{
    BlsEagerPk, BlsEagerSig, BlsPublicKey, BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSigId,
    BlsSignature,
};
use dash_primitives::{DynBitset, LlmqType, OutPoint, QuorumHash, QuorumVvecHash, TxHash};
use dash_types::codec::BaseCodec;

use super::*;
use crate::effect::Effect;
use crate::llmq::{CompleteChain, LlmqCtx, MemberError};
use crate::testkit::{MemChain, pro_tx};
use crate::types::ProTxHash;

const TEST: LlmqType = LlmqType::LlmqTest;
/// The base: a cycle boundary of LLMQ_TEST (`dkgInterval` 24).
const BASE: u32 = 48;

/// A masternode with an operator key: a basic-scheme key from ikm `[seed; 32]`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Member {
    pro_tx_hash: ProTxHash,
    collateral: OutPoint,
    seed: u8,
}

fn secret(seed: u8) -> BlsSecretKey<BlsScIetf> {
    BlsSecretKey::from_ikm(&[seed; 32]).expect("a key")
}

fn pk_value(pk: &BlsPublicKey<BlsScIetf>) -> BlsPkValue {
    BlsPkValue::from_canonical(&pk.to_bytes(), BlsSchemeKind::Basic).expect("canonical")
}

fn sig_value(sig: &BlsSignature<BlsScIetf>) -> BlsSigValue {
    BlsSigValue::from_canonical(&sig.to_bytes(), BlsSchemeKind::Basic).expect("canonical")
}

impl MnEntryView for Member {
    fn pro_tx_hash(&self) -> &ProTxHash {
        &self.pro_tx_hash
    }
    fn collateral(&self) -> &OutPoint {
        &self.collateral
    }
    fn is_evo(&self) -> bool {
        false
    }
    fn is_banned(&self) -> bool {
        false
    }
    fn confirmed_hash_is_null(&self) -> bool {
        false
    }
    fn confirmed_hash_with_protx(&self) -> &[u8; 32] {
        &[1; 32]
    }
}

impl OperatorKeyView for Member {
    fn operator_key(&self) -> BlsPkValue {
        pk_value(&secret(self.seed).public_key())
    }
}

fn member(seed: u8) -> Arc<Member> {
    Arc::new(Member {
        pro_tx_hash: pro_tx(seed),
        collateral: OutPoint {
            hash: TxHash::from([seed; 32]),
            index: 0,
        },
        seed,
    })
}

/// The injected member source: a fixed roster, and how often it was read.
struct Roster {
    members: Vec<Arc<Member>>,
    reads: Cell<usize>,
}

impl Roster {
    fn of(seeds: &[u8]) -> Roster {
        Roster {
            members: seeds.iter().map(|&s| member(s)).collect(),
            reads: Cell::new(0),
        }
    }
}

impl QuorumMemberSource for Roster {
    type Entry = Arc<Member>;
    type Error = ();
    fn members(
        &self,
        _: LlmqType,
        _: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Member>>, MemberError<()>> {
        self.reads.set(self.reads.get() + 1);
        Ok(self.members.clone().into())
    }
}

/// V19 active everywhere (basic scheme, non-rotated version 3); nothing else.
struct V19Only;

impl DeploymentView for V19Only {
    fn active_after(&self, _: Option<BlockId>, d: Deployment) -> bool {
        d == Deployment::Buried(BuriedDeployment::V19)
    }
    fn active_at(&self, _: BlockId, d: Deployment) -> bool {
        d == Deployment::Buried(BuriedDeployment::V19)
    }
}

fn regtest() -> ChainParams {
    ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().expect("regtest")
}

/// A linear chain `0 ..= 60`: `BlockId(h)` at height `h`.
fn chain() -> MemChain {
    let mut chain = MemChain::new();
    let genesis = chain.genesis();
    chain.extend(genesis, 60);
    chain
}

fn bits(size: usize, set: &[usize]) -> DynBitset {
    let mut data = vec![0u8; size.div_ceil(8)];
    for &i in set {
        data[i / 8] |= 1 << (i % 8);
    }
    DynBitset {
        num_bits: size as u64,
        data,
    }
}

/// The commitment of LLMQ_TEST (size 3, minSize 2) at `BASE` with every member valid, signed by
/// the operator keys `signer_seeds` (at the positions `signers`) and the quorum key of seed 99.
fn commitment(
    chain: &MemChain,
    params: &ChainParams,
    signers: &[usize],
    signer_seeds: &[u8],
) -> LlmqCommitment {
    let size = params.consensus.llmq(TEST).expect("configured").size as usize;
    let quorum_key = secret(99);
    let mut qc = LlmqCommitment {
        version: 3,
        llmq_type: TEST,
        quorum_hash: QuorumHash::from(*chain.hash(BlockId(BASE)).as_bytes()),
        quorum_index: None,
        signers: bits(size, signers),
        valid_members: bits(size, &(0..size).collect::<Vec<_>>()),
        quorum_public_key: BlsEagerPk::new(
            pk_value(&quorum_key.public_key()),
            BlsSchemeKind::Basic,
        ),
        quorum_vvec_hash: QuorumVvecHash::from([0x11; 32]),
        quorum_sig: BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic),
        members_sig: BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic),
    };
    sign(&mut qc, signer_seeds, 99);
    qc
}

/// The hash `qc`'s signatures are over, in the basic scheme.
fn commitment_hash(qc: &LlmqCommitment) -> [u8; 32] {
    build_commitment_hash(
        qc.llmq_type,
        &qc.quorum_hash,
        &qc.valid_members,
        &qc.quorum_public_key,
        qc.quorum_vvec_hash.as_bytes(),
        BlsCtx::BASIC,
    )
}

/// Signs `qc` as it stands: `membersSig` the secure aggregate of `signer_seeds`' signatures,
/// `quorumSig` the key of `quorum_seed`.
fn sign(qc: &mut LlmqCommitment, signer_seeds: &[u8], quorum_seed: u8) {
    sign_over(qc, commitment_hash(qc), signer_seeds, quorum_seed);
}

/// [`sign`] over `hash`.
fn sign_over(qc: &mut LlmqCommitment, hash: [u8; 32], signer_seeds: &[u8], quorum_seed: u8) {
    let keys: Vec<_> = signer_seeds.iter().map(|&s| secret(s)).collect();
    let sigs: Vec<_> = keys.iter().map(|k| k.sign_with(&hash, BlsSigId::Basic)).collect();
    let pks: Vec<_> = keys.iter().map(|k| k.public_key()).collect();
    let members_sig = BlsSignature::secure_aggregate(
        &sigs.iter().collect::<Vec<_>>(),
        &pks.iter().collect::<Vec<_>>(),
    )
    .expect("an aggregate");
    qc.members_sig = BlsEagerSig::new(sig_value(&members_sig), BlsSchemeKind::Basic);
    let quorum_sig = secret(quorum_seed).sign_with(&hash, BlsSigId::Basic);
    qc.quorum_sig = BlsEagerSig::new(sig_value(&quorum_sig), BlsSchemeKind::Basic);
}

struct Fixture {
    params: ChainParams,
    chain: MemChain,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture {
            params: regtest(),
            chain: chain(),
        }
    }

    /// Runs `f` on a context over this fixture's chain and parameters, and a fresh effect sink.
    fn with_ctx<R>(&self, f: impl FnOnce(&LlmqCtx<'_, ()>, &mut Vec<Effect>) -> R) -> R {
        self.with_ctx_under(&V19Only, f)
    }

    /// [`Fixture::with_ctx`] under `deployments`.
    fn with_ctx_under<R>(
        &self,
        deployments: &dyn DeploymentView,
        f: impl FnOnce(&LlmqCtx<'_, ()>, &mut Vec<Effect>) -> R,
    ) -> R {
        self.with_ctx_in(deployments, BlsCtx::BASIC, f)
    }

    /// [`Fixture::with_ctx_under`] in the scheme of `bls`.
    fn with_ctx_in<R>(
        &self,
        deployments: &dyn DeploymentView,
        bls: BlsCtx,
        f: impl FnOnce(&LlmqCtx<'_, ()>, &mut Vec<Effect>) -> R,
    ) -> R {
        let view = CompleteChain::<()>::new(&self.chain);
        let ctx = LlmqCtx::new(&self.params, &view, deployments, bls).expect("in domain");
        f(&ctx, &mut Vec::new())
    }

    fn verify(
        &self,
        roster: &Roster,
        qc: &LlmqCommitment,
        check_sigs: bool,
    ) -> Result<(), CommitmentError<()>> {
        self.verify_on(roster, qc, check_sigs.then_some(SigPath::Sync))
    }

    fn verify_on(
        &self,
        roster: &Roster,
        qc: &LlmqCommitment,
        sigs: Option<SigPath>,
    ) -> Result<(), CommitmentError<()>> {
        self.with_ctx(|ctx, effects| {
            verify_commitment(ctx, roster, qc, BlockId(BASE), sigs, effects)
        })
    }

    fn verify_quorum(
        &self,
        roster: &Roster,
        qc: &LlmqCommitment,
    ) -> Result<VerifiedQuorum, CommitmentError<()>> {
        self.with_ctx(|ctx, effects| {
            verify_quorum(ctx, roster, qc, BlockId(BASE), SigPath::Sync, effects)
        })
    }
}

const REJECTED_MEMBERS: Result<(), CommitmentError<()>> =
    Err(CommitmentError::Rejected(CommitmentReject::MembersSignature));

#[test]
fn a_signed_commitment_verifies_with_its_roster() {
    let f = Fixture::new();
    // Roster [1, 2, 3], members 1 and 2 signing (bits [1, 1, 0]).
    let roster = Roster::of(&[1, 2, 3]);
    let qc = commitment(&f.chain, &f.params, &[0, 1], &[1, 2]);
    assert_eq!(f.verify(&roster, &qc, true), Ok(()));
    assert_eq!(roster.reads.get(), 1, "members are read once");
    let verified = f.verify_quorum(&roster, &qc).expect("verified");
    assert_eq!(verified.quorum_hash(), &qc.quorum_hash);
    assert_eq!(verified.llmq_type(), TEST);
    assert_eq!(verified.quorum_index(), 0);
    assert_eq!(verified.public_key(), qc.quorum_public_key.value());
}

/// SB0 §5.1's mutation gate (DEC-105 f1): the verdict depends on the multiset of the operator
/// keys at the signer positions and on nothing else.
#[test]
fn members_sig_binds_the_signing_key_multiset_only() {
    let f = Fixture::new();
    let qc = commitment(&f.chain, &f.params, &[0, 1], &[1, 2]);
    // The roster Core computes: [A, B, C] with bits [1, 1, 0].
    assert_eq!(f.verify(&Roster::of(&[1, 2, 3]), &qc, true), Ok(()));
    // A permutation of the signers: [B, A, C], the same inputs and verdict.
    assert_eq!(f.verify(&Roster::of(&[2, 1, 3]), &qc, true), Ok(()));
    // A non-signer changed: [A, B, D] (Sol's case), the same verdict.
    assert_eq!(f.verify(&Roster::of(&[1, 2, 4]), &qc, true), Ok(()));
    // The signing-key multiset changed: a signer replaced by an outsider, or swapped with the
    // non-signer.
    assert_eq!(f.verify(&Roster::of(&[1, 4, 3]), &qc, true), REJECTED_MEMBERS);
    assert_eq!(f.verify(&Roster::of(&[1, 3, 2]), &qc, true), REJECTED_MEMBERS);
    // A key repeated: the multiset {A, A} is not {A, B}.
    assert_eq!(f.verify(&Roster::of(&[1, 1, 3]), &qc, true), REJECTED_MEMBERS);
    // Without signature checks the roster is never read for keys.
    assert_eq!(f.verify(&Roster::of(&[1, 4, 3]), &qc, false), Ok(()));
}

/// No member list leaves verification (SB0 §5.1, DEC-105 f1): the verified quorum carries its
/// identity and key, and nothing that names a member.
#[test]
fn a_verified_quorum_exposes_no_members() {
    let f = Fixture::new();
    let qc = commitment(&f.chain, &f.params, &[0, 1], &[1, 2]);
    let verified = f.verify_quorum(&Roster::of(&[1, 2, 3]), &qc).expect("verified");
    let shown = format!("{verified:?}");
    for seed in [1u8, 2, 3] {
        let member = format!("{:?}", pro_tx(seed));
        assert!(!shown.contains(&member), "{shown} names {member}");
    }
    // Its whole public surface: type, hash, index, key.
    let _: (LlmqType, &QuorumHash, i16, &BlsPkValue) = (
        verified.llmq_type(),
        verified.quorum_hash(),
        verified.quorum_index(),
        verified.public_key(),
    );
}

#[test]
fn the_quorum_signature_is_checked_after_the_members_signature() {
    let f = Fixture::new();
    let roster = Roster::of(&[1, 2, 3]);
    let mut qc = commitment(&f.chain, &f.params, &[0, 1], &[1, 2]);
    // The quorum key's signature by another key.
    sign(&mut qc, &[1, 2], 98);
    assert_eq!(
        f.verify(&roster, &qc, true),
        Err(CommitmentError::Rejected(CommitmentReject::QuorumSignature))
    );
    // Both wrong: the members signature is reported, as Core checks it first.
    sign(&mut qc, &[1, 3], 98);
    assert_eq!(f.verify(&roster, &qc, true), REJECTED_MEMBERS);
}

/// Every structural rejection, each with a commitment that passes every earlier check.
#[test]
fn every_structural_rejection_in_cores_order() {
    let f = Fixture::new();
    let roster = Roster::of(&[1, 2, 3]);
    let good = commitment(&f.chain, &f.params, &[0, 1], &[1, 2]);
    let rejects = |qc: &LlmqCommitment| match f.verify(&roster, qc, true) {
        Err(CommitmentError::Rejected(r)) => Some(r),
        Ok(()) => None,
        other => panic!("{other:?}"),
    };
    let with = |edit: &dyn Fn(&mut LlmqCommitment)| {
        let mut qc = good.clone();
        edit(&mut qc);
        rejects(&qc)
    };
    assert_eq!(
        with(&|qc| qc.version = 0),
        Some(CommitmentReject::Version {
            found: 0,
            expected: 3
        })
    );
    assert_eq!(
        with(&|qc| qc.version = 1),
        Some(CommitmentReject::Version {
            found: 1,
            expected: 3
        })
    );
    assert_eq!(
        with(&|qc| qc.quorum_hash = QuorumHash::from([7; 32])),
        Some(CommitmentReject::QuorumHash)
    );
    assert_eq!(with(&|qc| qc.quorum_index = Some(1)), Some(CommitmentReject::QuorumIndex));
    assert_eq!(with(&|qc| qc.signers = bits(4, &[0, 1])), Some(CommitmentReject::SignersSize));
    assert_eq!(
        with(&|qc| qc.valid_members = bits(2, &[0, 1])),
        Some(CommitmentReject::ValidMembersSize)
    );
    assert_eq!(
        with(&|qc| qc.valid_members = bits(3, &[0])),
        Some(CommitmentReject::ValidMembersCount)
    );
    assert_eq!(with(&|qc| qc.signers = bits(3, &[2])), Some(CommitmentReject::SignersCount));
    assert_eq!(
        with(&|qc| qc.quorum_public_key =
            BlsEagerPk::new(BlsPkValue::invalid(), BlsSchemeKind::Basic)),
        Some(CommitmentReject::QuorumPublicKey)
    );
    assert_eq!(
        with(&|qc| qc.quorum_vvec_hash = QuorumVvecHash::from([0; 32])),
        Some(CommitmentReject::VvecHash)
    );
    assert_eq!(
        with(&|qc| qc.members_sig = BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic)),
        Some(CommitmentReject::MembersSigObject)
    );
    assert_eq!(
        with(&|qc| qc.quorum_sig = BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic)),
        Some(CommitmentReject::QuorumSigObject)
    );
    // Earlier checks win: a wrong version and a wrong hash report the version.
    assert!(matches!(
        with(&|qc| {
            qc.version = 1;
            qc.quorum_hash = QuorumHash::from([7; 32]);
        }),
        Some(CommitmentReject::Version { .. })
    ));
}

/// Bits at or past the number of members: Core rejects them before any signature check.
#[test]
fn bits_past_the_members_are_rejected() {
    let f = Fixture::new();
    let qc = commitment(&f.chain, &f.params, &[0, 1], &[1, 2]);
    // Two members: the valid bit at 2 is past them.
    assert_eq!(
        f.verify(&Roster::of(&[1, 2]), &qc, false),
        Err(CommitmentError::Rejected(CommitmentReject::ValidMemberBit(2)))
    );
    // Valid bits for the two only, a signer at 2.
    let mut qc = commitment(&f.chain, &f.params, &[0, 2], &[1, 3]);
    qc.valid_members = bits(3, &[0, 1]);
    assert_eq!(
        f.verify(&Roster::of(&[1, 2]), &qc, false),
        Err(CommitmentError::Rejected(CommitmentReject::SignerBit(2)))
    );
    // No members (the type not enabled at the base, say): the first set bit is reported.
    assert_eq!(
        f.verify(&Roster::of(&[]), &qc, false),
        Err(CommitmentError::Rejected(CommitmentReject::ValidMemberBit(0)))
    );
}

/// A type the chain does not configure is Core's first rejection ("invalid llmqType"); a base
/// with no parent its second. A chain or member read failure is no verdict.
#[test]
fn unknown_types_and_parentless_bases_reject_and_read_failures_do_not() {
    let f = Fixture::new();
    let qc = commitment(&f.chain, &f.params, &[0, 1], &[1, 2]);
    let mut unknown = qc.clone();
    unknown.llmq_type = LlmqType::Llmq400_60;
    assert_eq!(
        f.verify(&Roster::of(&[1, 2, 3]), &unknown, true),
        Err(CommitmentError::Rejected(CommitmentReject::LlmqType))
    );
    // Genesis as the base: parentless.
    let parentless = f.with_ctx(|ctx, effects| {
        verify_commitment(ctx, &Roster::of(&[1, 2, 3]), &qc, BlockId(0), None, effects)
    });
    assert_eq!(parentless, Err(CommitmentError::Rejected(CommitmentReject::ParentlessBase)));

    /// A source whose view fails.
    struct Failing;
    impl QuorumMemberSource for Failing {
        type Entry = Arc<Member>;
        type Error = ();
        fn members(
            &self,
            _: LlmqType,
            base: BlockId,
            _: &mut dyn EffectSink,
        ) -> Result<QuorumMembers<Arc<Member>>, MemberError<()>> {
            Err(MemberError::Missing(base))
        }
    }
    let failed = f.with_ctx(|ctx, effects| {
        verify_commitment(ctx, &Failing, &qc, BlockId(BASE), Some(SigPath::Sync), effects)
    });
    assert_eq!(failed, Err(CommitmentError::Members(MemberError::Missing(BlockId(BASE)))));
}

/// A single-member type: `membersSig` is the one member's own signature, and no members is a
/// rejection of its own.
#[test]
fn a_single_member_quorum() {
    let mut f = Fixture::new();
    for p in &mut f.params.consensus.llmqs {
        if p.llmq_type == TEST {
            p.size = 1;
            p.min_size = 1;
            p.threshold = 1;
        }
    }
    let mut qc = commitment(&f.chain, &f.params, &[0], &[5]);
    // A null vvec hash is allowed for a single member.
    qc.quorum_vvec_hash = QuorumVvecHash::from([0; 32]);
    let hash = commitment_hash(&qc);
    // The member's plain signature, not a secure aggregate.
    qc.members_sig = BlsEagerSig::new(
        sig_value(&secret(5).sign_with(&hash, BlsSigId::Basic)),
        BlsSchemeKind::Basic,
    );
    qc.quorum_sig = BlsEagerSig::new(
        sig_value(&secret(99).sign_with(&hash, BlsSigId::Basic)),
        BlsSchemeKind::Basic,
    );
    assert_eq!(f.verify(&Roster::of(&[5]), &qc, true), Ok(()));
    assert_eq!(
        f.verify(&Roster::of(&[6]), &qc, true),
        Err(CommitmentError::Rejected(CommitmentReject::MemberSignature))
    );
    // Mined: Core's verdict on that path (SB8 D18; possible security note: see manager).
    assert_eq!(f.verify_on(&Roster::of(&[5]), &qc, Some(SigPath::Mined)), Ok(()));
    assert_eq!(f.verify_on(&Roster::of(&[6]), &qc, Some(SigPath::Mined)), Ok(()));
    let mut both_wrong = qc.clone();
    both_wrong.quorum_sig = qc.members_sig.clone();
    assert_eq!(f.verify_on(&Roster::of(&[6]), &both_wrong, Some(SigPath::Mined)), Ok(()));
    // The right member: the quorum signature is still checked.
    assert_eq!(
        f.verify_on(&Roster::of(&[5]), &both_wrong, Some(SigPath::Mined)),
        Err(CommitmentError::Rejected(CommitmentReject::QuorumSignature))
    );
    // With no members the valid bit at 0 is rejected first; with that bit cleared (and minSize
    // 0) the missing member is.
    for p in &mut f.params.consensus.llmqs {
        if p.llmq_type == TEST {
            p.min_size = 0;
        }
    }
    qc.valid_members = bits(1, &[]);
    qc.signers = bits(1, &[]);
    assert_eq!(
        f.verify(&Roster::of(&[]), &qc, true),
        Err(CommitmentError::Rejected(CommitmentReject::NoMembers))
    );
    // Mined: Core's verdict on that path (D18).
    assert_eq!(f.verify_on(&Roster::of(&[]), &qc, Some(SigPath::Mined)), Ok(()));
}

/// A multi-member type whose `minSize` is 1 (regtest `-llmqtestparams 3:1`, or a devnet), with
/// one signer: Core's synchronous check is a secure aggregate over the one key, `ProcessBlock`'s
/// `BlsCheck` an insecure check of it (`llmq/utils.cpp` v24:561-579), so a plain signature and a
/// one-key secure aggregate each pass exactly one path. No signer at all fails both.
#[test]
fn one_signer_follows_each_paths_check() {
    let mut f = Fixture::new();
    for p in &mut f.params.consensus.llmqs {
        if p.llmq_type == TEST {
            p.min_size = 1;
        }
    }
    let roster = Roster::of(&[1, 2, 3]);
    let sync = Some(SigPath::Sync);
    let mined = Some(SigPath::Mined);
    // `sign`: the secure aggregate of the one signature, weighted by its key.
    let aggregate = commitment(&f.chain, &f.params, &[0], &[1]);
    assert_eq!(f.verify_on(&roster, &aggregate, sync), Ok(()));
    assert_eq!(f.verify_on(&roster, &aggregate, mined), REJECTED_MEMBERS);
    let mut plain = aggregate.clone();
    plain.members_sig = BlsEagerSig::new(
        sig_value(&secret(1).sign_with(&commitment_hash(&plain), BlsSigId::Basic)),
        BlsSchemeKind::Basic,
    );
    assert_eq!(f.verify_on(&roster, &plain, sync), REJECTED_MEMBERS);
    assert_eq!(f.verify_on(&roster, &plain, mined), Ok(()));
    // Several signers: a secure aggregate on both paths.
    let two = commitment(&f.chain, &f.params, &[0, 2], &[1, 3]);
    assert_eq!(f.verify_on(&roster, &two, sync), Ok(()));
    assert_eq!(f.verify_on(&roster, &two, mined), Ok(()));
    // No signer (minSize 0): `VerifySecureAggregated` of no keys is false, and so is `BlsCheck`.
    for p in &mut f.params.consensus.llmqs {
        if p.llmq_type == TEST {
            p.min_size = 0;
        }
    }
    let mut none = plain.clone();
    none.signers = bits(3, &[]);
    assert_eq!(f.verify_on(&roster, &none, sync), REJECTED_MEMBERS);
    assert_eq!(f.verify_on(&roster, &none, mined), REJECTED_MEMBERS);
}

/// DIP0024 and V19 active everywhere.
struct RotationActive;

impl DeploymentView for RotationActive {
    fn active_after(&self, _: Option<BlockId>, d: Deployment) -> bool {
        matches!(d, Deployment::Buried(BuriedDeployment::V19 | BuriedDeployment::Dip0024))
    }
    fn active_at(&self, b: BlockId, d: Deployment) -> bool {
        self.active_after(Some(b), d)
    }
}

/// A rotated commitment (version 4, LLMQ_TEST_DIP0024): the index is the base's height modulo
/// `dkgInterval`, and the version needs DIP0024 active before the cycle.
#[test]
fn a_rotated_commitment_carries_its_index_and_version_4() {
    let f = Fixture::new();
    let rotated = LlmqType::LlmqTestDip0024;
    let params = *f.params.consensus.llmq(rotated).expect("configured");
    assert!(params.use_rotation);
    let base = BASE + 1;
    let index = base % params.dkg_interval.unsigned_abs();
    assert_ne!(index, 0, "a base past the cycle's first block");
    let size = params.size as usize;
    let seeds: Vec<u8> = (1..=size as u8).collect();
    let mut qc = commitment(&f.chain, &f.params, &[0], &[1]);
    qc.version = 4;
    qc.llmq_type = rotated;
    qc.quorum_hash = QuorumHash::from(*f.chain.hash(BlockId(base)).as_bytes());
    qc.quorum_index = Some(index as i16);
    qc.signers = bits(size, &(0..size).collect::<Vec<_>>());
    qc.valid_members = qc.signers.clone();
    sign(&mut qc, &seeds, 99);
    let roster = Roster::of(&seeds);
    let verify = |deployments: &dyn DeploymentView, qc: &LlmqCommitment| {
        f.with_ctx_under(deployments, |ctx, effects| {
            verify_commitment(ctx, &roster, qc, BlockId(base), Some(SigPath::Sync), effects)
        })
    };
    assert_eq!(verify(&RotationActive, &qc), Ok(()));
    let mut wrong_index = qc.clone();
    wrong_index.quorum_index = Some(0);
    assert_eq!(
        verify(&RotationActive, &wrong_index),
        Err(CommitmentError::Rejected(CommitmentReject::QuorumIndex))
    );
    // DIP0024 inactive: the rotated type's commitment is version 3.
    assert_eq!(
        verify(&V19Only, &qc),
        Err(CommitmentError::Rejected(CommitmentReject::Version {
            found: 4,
            expected: 3
        }))
    );
}

/// V19 active after the blocks at and above a height.
struct V19From(u32);

impl DeploymentView for V19From {
    fn active_after(&self, b: Option<BlockId>, d: Deployment) -> bool {
        d == Deployment::Buried(BuriedDeployment::V19) && b.is_some_and(|b| b.0 >= self.0)
    }
    fn active_at(&self, b: BlockId, d: Deployment) -> bool {
        self.active_after(Some(b), d)
    }
}

/// The version reads V19 as active after the base, not at it: V19 activating in the block after
/// the base is version 3, in the block after that version 1. (Legacy-scheme signatures are
/// covered by the recorded pre-V19 commitments in node-l1, D15.)
#[test]
fn the_version_reads_v19_after_the_base() {
    let f = Fixture::new();
    let qc = commitment(&f.chain, &f.params, &[0, 1], &[1, 2]);
    let roster = Roster::of(&[1, 2, 3]);
    let verify = |deployments: &dyn DeploymentView, qc: &LlmqCommitment| {
        f.with_ctx_under(deployments, |ctx, effects| {
            verify_commitment(ctx, &roster, qc, BlockId(BASE), None, effects)
        })
    };
    assert_eq!(verify(&V19From(BASE), &qc), Ok(()));
    assert_eq!(
        verify(&V19From(BASE + 1), &qc),
        Err(CommitmentError::Rejected(CommitmentReject::Version {
            found: 3,
            expected: 1
        }))
    );
    let mut legacy = qc.clone();
    legacy.version = 1;
    assert_eq!(verify(&V19From(BASE + 1), &legacy), Ok(()));
}

/// `BuildCommitmentHash` serializes the key under the caller's scheme, and the bitset as Core's
/// `DYNBITSET`.
#[test]
fn the_commitment_hash_follows_cores_serialization() {
    let key = BlsEagerPk::new(pk_value(&secret(9).public_key()), BlsSchemeKind::Basic);
    let valid = bits(10, &[0, 3, 9]);
    let quorum_hash = QuorumHash::from([0x42; 32]);
    let vvec = [0x11; 32];
    let mut pre = vec![TEST.to_base()];
    pre.extend_from_slice(quorum_hash.as_bytes());
    pre.extend_from_slice(&[10, 0b0000_1001, 0b0000_0010]);
    let basic = [pre.clone(), key.to_bytes(BlsSchemeKind::Basic).to_vec(), vvec.to_vec()].concat();
    let legacy = [pre, key.to_bytes(BlsSchemeKind::Legacy).to_vec(), vvec.to_vec()].concat();
    let hash = |bls| build_commitment_hash(TEST, &quorum_hash, &valid, &key, &vvec, bls);
    assert_eq!(hash(BlsCtx::BASIC), dash_consensus::hash::sha256d(&basic));
    assert_eq!(hash(BlsCtx::LEGACY), dash_consensus::hash::sha256d(&legacy));
    assert_ne!(hash(BlsCtx::BASIC), hash(BlsCtx::LEGACY));
    // Padding bits a hand-built bitset holds are not serialized.
    let mut padded = valid.clone();
    padded.data[1] |= 0b1111_1100;
    assert_eq!(
        build_commitment_hash(TEST, &quorum_hash, &padded, &key, &vvec, BlsCtx::BASIC),
        hash(BlsCtx::BASIC)
    );
}

#[test]
fn versions_follow_core() {
    assert_eq!(
        [
            commitment_version(false, false),
            commitment_version(true, false),
            commitment_version(false, true),
            commitment_version(true, true)
        ],
        [1, 2, 3, 4]
    );
}

/// The null commitment of LLMQ_TEST at `BASE`, as Core's `CFinalCommitment(params, quorumHash)`
/// builds it: sized bitsets, nothing set, invalid BLS fields.
fn null_commitment(f: &Fixture) -> LlmqCommitment {
    let size = f.params.consensus.llmq(TEST).expect("configured").size as usize;
    LlmqCommitment {
        version: 3,
        llmq_type: TEST,
        quorum_hash: QuorumHash::from(*f.chain.hash(BlockId(BASE)).as_bytes()),
        quorum_index: None,
        signers: bits(size, &[]),
        valid_members: bits(size, &[]),
        quorum_public_key: BlsEagerPk::new(BlsPkValue::invalid(), BlsSchemeKind::Basic),
        quorum_vvec_hash: QuorumVvecHash::from([0; 32]),
        quorum_sig: BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic),
        members_sig: BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic),
    }
}

/// Quirk 36: `IsNull()` holds iff no bit of either bitset is set, the vvec hash is zero and none
/// of the three BLS fields is a valid object (`llmq/commitment.h` v24:118-131). A field that holds
/// bytes Core cannot read as a point or signature is invalid, so it still counts as null, and
/// `VerifyNull` (`llmq/commitment.cpp` v24:211-224) accepts it when the sizes match.
#[test]
fn quirk_36_null_commitment_iff_invalid() {
    let f = Fixture::new();
    let null = null_commitment(&f);
    assert!(null.is_null());
    assert!(verify_null(&f.params, &null));

    // Each field that Core counts makes the commitment non-null.
    let signed = commitment(&f.chain, &f.params, &[0, 1], &[1, 2]);
    let size = null.signers.num_bits as usize;
    let mut cases: Vec<(&str, LlmqCommitment)> = Vec::new();
    let mut c = null.clone();
    c.signers = bits(size, &[0]);
    cases.push(("a signer", c));
    let mut c = null.clone();
    c.valid_members = bits(size, &[2]);
    cases.push(("a valid member", c));
    let mut c = null.clone();
    c.quorum_public_key = signed.quorum_public_key.clone();
    cases.push(("a valid quorum key", c));
    let mut c = null.clone();
    c.quorum_vvec_hash = QuorumVvecHash::from([0x11; 32]);
    cases.push(("a vvec hash", c));
    let mut c = null.clone();
    c.quorum_sig = signed.quorum_sig.clone();
    cases.push(("a valid quorum signature", c));
    let mut c = null.clone();
    c.members_sig = signed.members_sig.clone();
    cases.push(("a valid members signature", c));
    for (how, c) in cases {
        assert!(!c.is_null(), "{how}");
        assert!(!verify_null(&f.params, &c), "{how}");
    }

    // A field canonical only in the other scheme reads as the invalid object (`bls/bls.h`
    // v24:190-199 `Reset()`s it; bytes canonical in neither throw "malleable BLS object"): still
    // null.
    let read = |pk: &[u8], sig: &[u8]| {
        let mut bytes = Vec::new();
        bytes.extend(3u16.to_le_bytes());
        bytes.push(TEST.to_base());
        bytes.extend(null.quorum_hash.as_bytes());
        bytes.extend([size as u8, 0, size as u8, 0]);
        bytes.extend(pk);
        bytes.extend([0; 32]);
        bytes.extend(sig);
        bytes.extend(sig);
        LlmqCommitment::decode(&mut &bytes[..])
    };
    let pk = signed.quorum_public_key.to_bytes(BlsSchemeKind::Legacy);
    let sig = signed.quorum_sig.to_bytes(BlsSchemeKind::Legacy);
    let other_scheme = read(&pk, &sig).expect("a commitment");
    assert!(!other_scheme.quorum_public_key.is_valid() && !other_scheme.quorum_sig.is_valid());
    assert!(other_scheme.is_null(), "a field of the other scheme is invalid");
    assert!(verify_null(&f.params, &other_scheme));
    assert!(read(&[0; 48], &[0; 96]).expect("a commitment").is_null());
    assert!(read(&[0xff; 48], &[0xff; 96]).is_err(), "malleable BLS object");

    // VerifyNull also needs the sizes, and a configured type.
    let mut short = null.clone();
    short.signers = bits(size - 1, &[]);
    assert!(short.is_null() && !verify_null(&f.params, &short));
    let mut unknown = null.clone();
    unknown.llmq_type = LlmqType::from_base(0xee);
    assert!(unknown.is_null() && !verify_null(&f.params, &unknown));
}

/// Quirk 20: the version a commitment must carry follows its quorum base block
/// (`DeploymentActiveAfter(base, V19)`, `llmq/commitment.cpp` v24:131), while the public-key
/// bytes `BuildCommitmentHash` hashes follow the global scheme (`hw << pubKey`, v24:312-325). A
/// quorum based before V19 carries legacy version 1, yet with V19 active at the tip its
/// signatures are over the key's basic bytes.
#[test]
fn quirk_20_commitment_version_and_hash_scheme() {
    let f = Fixture::new();
    let roster = Roster::of(&[1, 2, 3]);
    let verify = |deployments: &dyn DeploymentView, qc: &LlmqCommitment| {
        f.with_ctx_under(deployments, |ctx, effects| {
            verify_commitment(ctx, &roster, qc, BlockId(BASE), Some(SigPath::Sync), effects)
        })
    };
    // Signed over the basic bytes of the key, the global scheme of the context; the version is
    // not hashed.
    let mut legacy = commitment(&f.chain, &f.params, &[0, 1], &[1, 2]);
    legacy.version = 1;
    assert_eq!(verify(&V19From(BASE + 1), &legacy), Ok(()));
    // The same version at a base where V19 is active is wrong.
    assert_eq!(
        verify(&V19From(BASE), &legacy),
        Err(CommitmentError::Rejected(CommitmentReject::Version {
            found: 1,
            expected: 3
        }))
    );
    // Signed over the key's legacy bytes, the scheme of its version, it does not verify.
    let legacy_hash = build_commitment_hash(
        legacy.llmq_type,
        &legacy.quorum_hash,
        &legacy.valid_members,
        &legacy.quorum_public_key,
        legacy.quorum_vvec_hash.as_bytes(),
        BlsCtx::LEGACY,
    );
    assert_ne!(legacy_hash, commitment_hash(&legacy));
    let mut by_version = legacy.clone();
    sign_over(&mut by_version, legacy_hash, &[1, 2], 99);
    assert_eq!(verify(&V19From(BASE + 1), &by_version), REJECTED_MEMBERS);
}

/// `verify_mined_signatures` alone, the signature phase of `ProcessBlock`'s `fBLSChecks`
/// (LQ2 D11): a good commitment passes, a bad members or quorum signature is rejected, and an
/// unconfigured type is the no-queue `true`.
#[test]
fn verify_mined_signatures_checks_both_signatures() {
    let f = Fixture::new();
    let roster = Roster::of(&[1, 2, 3]);
    let mined = |qc: &LlmqCommitment| {
        f.with_ctx(|ctx, effects| verify_mined_signatures(ctx, &roster, qc, BlockId(BASE), effects))
    };
    let mut qc = commitment(&f.chain, &f.params, &[0, 1], &[1, 2]);
    assert_eq!(mined(&qc), Ok(()));
    sign(&mut qc, &[1, 3], 99);
    assert!(matches!(mined(&qc), Err(CommitmentError::Rejected(_))));
    let mut quorum = commitment(&f.chain, &f.params, &[0, 1], &[1, 2]);
    sign(&mut quorum, &[1, 2], 98);
    assert!(matches!(mined(&quorum), Err(CommitmentError::Rejected(_))));
    qc.llmq_type = LlmqType::Llmq400_60;
    assert_eq!(mined(&qc), Ok(()));
}

/// `BlsCheck`'s accessors give back what it was built from (BS11b's batch reads them), and its
/// verdict is Core's `BlsCheck::operator()` (`utils.cpp` v24:561-579), an unsure failure none.
#[test]
fn a_bls_check_reads_back() {
    let hash = [7; 32];
    let sig = sig_value(&secret(1).sign_with(&hash, BlsSigId::Basic));
    let keys = vec![pk_value(&secret(1).public_key()), pk_value(&secret(2).public_key())];
    let reject = CommitmentReject::MembersSignature;
    let check = BlsCheck::new(sig.clone(), keys.clone(), hash, BlsSchemeKind::Legacy, reject, true);
    assert_eq!(check.sig(), &sig);
    assert_eq!(check.keys(), &keys[..]);
    assert_eq!(check.msg_hash(), &hash);
    assert_eq!(check.scheme(), BlsSchemeKind::Legacy);
    assert!(check.is_unsure());
    assert_eq!(check.reject(), reject);
    let one = |key: usize, unsure: bool| {
        let keys = vec![keys[key].clone()];
        BlsCheck::new(sig.clone(), keys, hash, BlsSchemeKind::Basic, reject, unsure).verdict()
    };
    assert_eq!(one(0, false), BlsVerdict::Valid);
    assert_eq!(one(1, false), BlsVerdict::Invalid);
    assert_eq!(one(1, true), BlsVerdict::Unsupported);
    assert!(!BlsCheck::new(sig, vec![], hash, BlsSchemeKind::Basic, reject, false).is_unsure());
}
