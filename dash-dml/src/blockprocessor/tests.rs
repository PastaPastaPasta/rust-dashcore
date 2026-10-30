//! LQ2 block-processor tests: Core's `CQuorumBlockProcessor` rules (`process_block`,
//! `ProcessCommitment`, `GetCommitmentsFromBlock`, the helpers, `UndoBlock`, `GetQcHashes` and
//! `CalcCbTxMerkleRootQuorums`), the mineable pool and `qfcommit`, and `CheckLLMQCommitment`, each
//! on regtest v24 parameters over an in-memory chain, citing the Core v24.0.0-rc.3 line it
//! asserts. Quirks 18 and 19 (`docs/node-program/quirks.toml`) are pinned here.
//!
//! Regtest configures LLMQ_TEST (100), LLMQ_TEST_INSTANTSEND (104), LLMQ_TEST_V17 (102),
//! LLMQ_TEST_DIP0024 (103, rotated) and LLMQ_TEST_PLATFORM (106), all with `dkgInterval` 24.
//! Under the default deployments (DIP3, V19 and DIP0024 from height 1) the types enabled at a
//! parent are 100, 103 and 106: 104 is off once DIP0024 is active with DIP0024 quorums, and 102
//! needs its versionbits deployment (`validation.cpp` v24:6658-6700).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::primitives::{
    Block, Tx, core_serialize, decode_block, write_commitment, write_compact_size,
};
use dash_consensus::{BlockId, ChainAncestry, HeaderCtx, TxValidationError, TxValidationResult};
use dash_evo::bls::BlsCtx;
use dash_evo::effect::{Effect, EffectSink};
use dash_evo::llmq::{
    BlsCheck, BlsCheckRunner, BlsVerdict, CommitmentReject, CompleteChain, LlmqChain,
    MinedCommitment, MinedFamily, MinedIndexEntry, QuorumMemberSource, QuorumMembers,
    SerialBlsChecks, build_commitment_hash, commitment_version,
};
use dash_evo::sml::merkle_root_quorums_from_hashes;
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_pkc::bls::core::{BlsPkValue, BlsSigValue};
use dash_pkc::bls::{
    BlsEagerPk, BlsEagerSig, BlsPublicKey, BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSigId,
    BlsSignature,
};
use dash_primitives::{DynBitset, QuorumVvecHash, Transaction};

use super::pool::{
    MineablePool, QFCOMMIT_STALE_REPROP_BAN_VERSION, QfcommitOutcome, null_commitment,
    qfcommit_verdict,
};
use super::*;
use crate::specialtx::qc::{CommitmentVerifier, NodeCommitmentVerifier, check_llmq_commitment};
use crate::specialtx::reasons::{BAD_QC_QUORUM_HASH, BAD_QC_VERSION};
use crate::store::{EvoDelta, MemEvoStore};
use crate::testkit::{MemChain, dmn_with, operator_key, pro_tx};
use crate::{Dmn, MnType};

const TEST: LlmqType = LlmqType::LlmqTest;
const ROT: LlmqType = LlmqType::LlmqTestDip0024;
const INSTANTSEND: LlmqType = LlmqType::LlmqTestInstantsend;
const V17: LlmqType = LlmqType::LlmqTestV17;
const PLATFORM: LlmqType = LlmqType::LlmqTestPlatform;
/// Configured on mainnet, not on regtest.
const UNCONFIGURED: LlmqType = LlmqType::Llmq50_60;

/// The main chain is `0 ..= TIP`, `BlockId(h)` at height `h`.
const TIP: u32 = 100;
/// A block at height 48 off the main chain (a child of 47), and its child at 49.
const FORK_48: BlockId = BlockId(TIP + 1);
const FORK_49: BlockId = BlockId(TIP + 2);

/// The operator-key seeds of the four members, in member order.
const SEEDS: [u8; 4] = [1, 2, 3, 4];
/// The seed of the quorum key.
const QUORUM_SEED: u8 = 99;

const WRITE: QcMode = QcMode {
    just_check: false,
    bls_checks: false,
};
const CHECK: QcMode = QcMode {
    just_check: true,
    bls_checks: false,
};
const WRITE_BLS: QcMode = QcMode {
    just_check: false,
    bls_checks: true,
};
const CHECK_BLS: QcMode = QcMode {
    just_check: true,
    bls_checks: true,
};
const MODES: [QcMode; 4] = [WRITE, CHECK, WRITE_BLS, CHECK_BLS];

// ---- keys and commitments ----

fn secret(seed: u8) -> BlsSecretKey<BlsScIetf> {
    BlsSecretKey::from_ikm(&[seed; 32]).expect("a key")
}

fn pk_value(pk: &BlsPublicKey<BlsScIetf>) -> BlsPkValue {
    BlsPkValue::from_canonical(&pk.to_bytes(), BlsSchemeKind::Basic).expect("canonical")
}

fn sig_value(sig: &BlsSignature<BlsScIetf>) -> BlsSigValue {
    BlsSigValue::from_canonical(&sig.to_bytes(), BlsSchemeKind::Basic).expect("canonical")
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

/// Signs `qc` as it stands, in the basic scheme: `membersSig` the secure aggregate of the
/// operator keys of `seeds`, `quorumSig` the quorum key's.
fn sign(qc: &mut LlmqCommitment, seeds: &[u8]) {
    let hash = build_commitment_hash(
        qc.llmq_type,
        &qc.quorum_hash,
        &qc.valid_members,
        &qc.quorum_public_key,
        qc.quorum_vvec_hash.as_bytes(),
        BlsCtx::BASIC,
    );
    let keys: Vec<_> = seeds.iter().map(|&s| secret(s)).collect();
    let sigs: Vec<_> = keys.iter().map(|k| k.sign_with(&hash, BlsSigId::Basic)).collect();
    let pks: Vec<_> = keys.iter().map(|k| k.public_key()).collect();
    let members_sig = BlsSignature::secure_aggregate(
        &sigs.iter().collect::<Vec<_>>(),
        &pks.iter().collect::<Vec<_>>(),
    )
    .expect("an aggregate");
    qc.members_sig = BlsEagerSig::new(sig_value(&members_sig), BlsSchemeKind::Basic);
    let quorum_sig = secret(QUORUM_SEED).sign_with(&hash, BlsSigId::Basic);
    qc.quorum_sig = BlsEagerSig::new(sig_value(&quorum_sig), BlsSchemeKind::Basic);
}

/// `CFinalCommitmentTxPayload` version 1 at `height` around `qc`.
fn qc_payload(height: u32, qc: &LlmqCommitment) -> Vec<u8> {
    qc_payload_versioned(1, height, qc)
}

/// [`qc_payload`] with the payload version `version`.
fn qc_payload_versioned(version: u16, height: u32, qc: &LlmqCommitment) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend(version.to_le_bytes());
    p.extend(height.to_le_bytes());
    write_commitment(qc, &mut p);
    p
}

fn tx_bytes(version: i16, tx_type: TxType, extra_payload: Vec<u8>) -> Vec<u8> {
    core_serialize(&Transaction {
        version,
        tx_type,
        inputs: vec![],
        outputs: vec![],
        lock_time: 0,
        extra_payload,
    })
}

/// A plain version-1 transaction, the coinbase position's.
fn plain_tx() -> Vec<u8> {
    tx_bytes(1, TxType::Spend, vec![])
}

/// A version-3 quorum-commitment transaction with `payload`.
fn qc_tx_bytes(payload: Vec<u8>) -> Vec<u8> {
    tx_bytes(3, TxType::QuorumCommitment, payload)
}

/// A block of the serialized `txs` under a zero header.
fn block_of(txs: &[Vec<u8>]) -> Block {
    let mut raw = vec![0; 80];
    dash_consensus::primitives::write_compact_size(txs.len() as u64, &mut raw);
    for tx in txs {
        raw.extend_from_slice(tx);
    }
    decode_block(&mut &raw[..]).expect("a block").into_value()
}

/// A block of a plain transaction, then one quorum-commitment transaction per payload.
fn payload_block(payloads: &[Vec<u8>]) -> Block {
    let mut txs = vec![plain_tx()];
    txs.extend(payloads.iter().cloned().map(qc_tx_bytes));
    block_of(&txs)
}

/// The block at `height` mining `qcs`, in this order.
fn qc_block(height: u32, qcs: &[LlmqCommitment]) -> Block {
    payload_block(&qcs.iter().map(|qc| qc_payload(height, qc)).collect::<Vec<_>>())
}

/// The quorum-commitment transaction of `payload`.
fn qc_tx(payload: Vec<u8>) -> Arc<Tx> {
    Arc::clone(&payload_block(&[payload]).txs()[1])
}

// ---- the views ----

/// Deployments by height: DIP3, V19 and DIP0024 from the given heights, nothing else.
#[derive(Clone)]
struct Heights {
    chain: MemChain,
    dip3: u32,
    v19: u32,
    dip24: u32,
}

impl Heights {
    fn first(&self, d: Deployment) -> u32 {
        match d {
            Deployment::Buried(BuriedDeployment::Dip0003) => self.dip3,
            Deployment::Buried(BuriedDeployment::V19) => self.v19,
            Deployment::Buried(BuriedDeployment::Dip0024) => self.dip24,
            _ => u32::MAX,
        }
    }
}

impl DeploymentView for Heights {
    fn active_after(&self, prev: Option<BlockId>, d: Deployment) -> bool {
        prev.map_or(0, |p| self.chain.header(p).height + 1) >= self.first(d)
    }

    fn active_at(&self, block: BlockId, d: Deployment) -> bool {
        self.chain.header(block).height >= self.first(d)
    }
}

/// `LookupBlockIndex` over every block of the tree.
struct Index(BTreeMap<[u8; 32], (BlockId, u32)>);

impl BlockLookup for Index {
    fn lookup(&self, hash: &BlockHash) -> Option<(BlockId, u32)> {
        self.0.get(hash.as_bytes()).copied()
    }
}

/// The effect a members read records: a marker naming the type and the base.
fn members_read(llmq_type: LlmqType, base: BlockId) -> Effect {
    Effect::ResetPlatformBan {
        pro_tx_hash: pro_tx(llmq_type.to_base()),
        height: base.0 as i32,
    }
}

/// The effect the precompute records: a marker naming the block and the mode.
fn precomputed(block: BlockId, mode: PrecomputeMode) -> Effect {
    Effect::NotifyMnListChanged {
        at: BlockHash::from([block.0 as u8; 32]),
        undo: mode == PrecomputeMode::Undo,
    }
}

/// The member source: the four members of [`SEEDS`] for every quorum, each read recorded as
/// [`members_read`].
struct Roster(Vec<Arc<Dmn>>);

impl Roster {
    fn new() -> Self {
        Roster(
            SEEDS
                .iter()
                .map(|&n| {
                    dmn_with(n, MnType::Regular, |_, cold| {
                        cold.pub_key_operator = operator_key(n, BlsSchemeKind::Basic);
                    })
                })
                .collect(),
        )
    }
}

impl QuorumMemberSource for Roster {
    type Entry = Arc<Dmn>;
    type Error = StoreError;

    fn members(
        &self,
        llmq_type: LlmqType,
        base: BlockId,
        effects: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        effects.emit(members_read(llmq_type, base));
        Ok(self.0.clone().into())
    }
}

/// The precompute: records each call, and emits [`precomputed`].
#[derive(Default)]
struct Precompute(RefCell<Vec<(BlockId, PrecomputeMode)>>);

impl QuorumPrecompute for Precompute {
    type Error = StoreError;

    fn precompute(
        &self,
        block: BlockId,
        mode: PrecomputeMode,
        effects: &mut dyn EffectSink,
    ) -> Result<(), MemberError<StoreError>> {
        self.0.borrow_mut().push((block, mode));
        effects.emit(precomputed(block, mode));
        Ok(())
    }
}

/// A reader whose index rows have lost their commitments (Core's "commitment-not-found").
struct Lossy<'a>(&'a dyn MinedCommitmentReader<Error = StoreError>);

impl MinedCommitmentReader for Lossy<'_> {
    type Error = StoreError;

    fn chain(&self) -> &dyn ChainAncestry {
        self.0.chain()
    }

    fn mined_commitment(
        &self,
        _: LlmqType,
        _: &QuorumHash,
        _: BlockId,
    ) -> Result<Option<MinedCommitment>, StoreError> {
        Ok(None)
    }

    fn mined_index(
        &self,
        llmq_type: LlmqType,
        family: MinedFamily,
        chain_end: BlockId,
        max_mined_height: u32,
        limit: usize,
    ) -> Result<Vec<MinedIndexEntry>, StoreError> {
        self.0.mined_index(llmq_type, family, chain_end, max_mined_height, limit)
    }
}

/// A chain view in which one block reports height `u32::MAX`.
struct Tall<'a> {
    chain: &'a MemChain,
    tall: BlockId,
}

impl LlmqChain for Tall<'_> {
    type Error = StoreError;

    fn header(&self, id: BlockId) -> Result<HeaderCtx, StoreError> {
        let mut header = self.chain.header(id);
        if id == self.tall {
            header.height = u32::MAX;
        }
        Ok(header)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, StoreError> {
        Ok(self.chain.ancestor(id, height))
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, StoreError> {
        Ok(self.chain.prev(id))
    }
}

// ---- the fixture ----

/// What `process_block` returned, and the effects it emitted.
type Run = (Result<Vec<MinedCommitmentRecord>, QcBlockError>, Vec<Effect>);

struct Fixture {
    params: ChainParams,
    chain: MemChain,
    index: Index,
    deps: Heights,
    store: MemEvoStore,
    roster: Roster,
    precompute: Precompute,
}

impl Fixture {
    /// Regtest v24, the main chain `0 ..= TIP` and the fork at 48, DIP3, V19 and DIP0024 from
    /// height 1, an empty store.
    fn new() -> Self {
        Self::with_deployments(1, 1, 1)
    }

    fn with_deployments(dip3: u32, v19: u32, dip24: u32) -> Self {
        let params =
            ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().expect("regtest");
        let mut chain = MemChain::new();
        let genesis = chain.genesis();
        chain.extend(genesis, TIP);
        let fork = chain.child(BlockId(47));
        assert_eq!(fork, FORK_48);
        assert_eq!(chain.child(fork), FORK_49);
        let index = Index(
            (0..=FORK_49.0)
                .map(|id| {
                    let header = chain.header(BlockId(id));
                    (*header.hash.as_bytes(), (BlockId(id), header.height))
                })
                .collect(),
        );
        let deps = Heights {
            chain: chain.clone(),
            dip3,
            v19,
            dip24,
        };
        Fixture {
            params,
            chain,
            index,
            deps,
            store: MemEvoStore::new(),
            roster: Roster::new(),
            precompute: Precompute::default(),
        }
    }

    /// Runs `f` on the block processor's context over this fixture.
    fn with<R>(&self, f: impl FnOnce(&QcCtx<'_>) -> R) -> R {
        self.with_runner(&SerialBlsChecks, f)
    }

    /// [`Fixture::with`], the block's signature checks run by `bls_checks`.
    fn with_runner<R>(
        &self,
        bls_checks: &dyn BlsCheckRunner,
        f: impl FnOnce(&QcCtx<'_>) -> R,
    ) -> R {
        self.with_sources(&self.roster, &self.precompute, bls_checks, f)
    }

    /// [`Fixture::with_runner`] over `members` and `precompute` instead of the fixture's.
    fn with_sources<R>(
        &self,
        members: &DmlMemberSource<'_>,
        precompute: &dyn QuorumPrecompute<Error = StoreError>,
        bls_checks: &dyn BlsCheckRunner,
        f: impl FnOnce(&QcCtx<'_>) -> R,
    ) -> R {
        let chain = CompleteChain::<StoreError>::new(&self.chain);
        let llmq =
            LlmqCtx::new(&self.params, &chain, &self.deps, BlsCtx::BASIC).expect("in domain");
        let mined = self.store.view(&self.chain);
        f(&QcCtx {
            llmq,
            index: &self.index,
            mined: &mined,
            members,
            precompute,
            bls_checks,
        })
    }

    fn llmq(&self, llmq_type: LlmqType) -> &LlmqParams {
        self.params.consensus.llmq(llmq_type).expect("configured")
    }

    fn hash(&self, id: BlockId) -> QuorumHash {
        QuorumHash::from(*self.chain.hash(id).as_bytes())
    }

    /// The quorum hash of the main chain's block at `height`.
    fn qhash(&self, height: u32) -> QuorumHash {
        self.hash(BlockId(height))
    }

    /// The version `Verify` expects of a commitment of `llmq_type` based at `base`.
    fn version(&self, llmq_type: LlmqType, base: BlockId) -> u16 {
        let params = self.llmq(llmq_type);
        let rotation =
            self.with(|ctx| is_quorum_rotation_enabled(&ctx.llmq, params, base)).expect("complete");
        let v19 = self.deps.active_after(Some(base), Deployment::Buried(BuriedDeployment::V19));
        commitment_version(rotation, v19)
    }

    /// The commitment of `llmq_type` based at the main chain's block `base` (its index the
    /// base's offset in the cycle), every member valid, signed by the members at `signers`.
    fn signed(&self, llmq_type: LlmqType, base: u32, signers: &[usize]) -> LlmqCommitment {
        let size = self.llmq(llmq_type).size as usize;
        let version = self.version(llmq_type, BlockId(base));
        let mut qc = LlmqCommitment {
            version,
            llmq_type,
            quorum_hash: self.qhash(base),
            quorum_index: (version == 2 || version == 4).then_some((base % 24) as i16),
            signers: bits(size, signers),
            valid_members: bits(size, &(0..size).collect::<Vec<_>>()),
            quorum_public_key: BlsEagerPk::new(
                pk_value(&secret(QUORUM_SEED).public_key()),
                BlsSchemeKind::Basic,
            ),
            quorum_vvec_hash: QuorumVvecHash::from([0x11; 32]),
            quorum_sig: BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic),
            members_sig: BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic),
        };
        let seeds: Vec<u8> = signers.iter().map(|&i| SEEDS[i]).collect();
        sign(&mut qc, &seeds);
        qc
    }

    /// LLMQ_TEST's commitment at 48 signed by members 0 and 1 (`minSize`).
    fn test_qc(&self) -> LlmqCommitment {
        self.signed(TEST, 48, &[0, 1])
    }

    /// The rotated commitment of index `index` (base `48 + index`), every member signing.
    fn rot_qc(&self, index: u32) -> LlmqCommitment {
        self.signed(ROT, 48 + index, &[0, 1, 2, 3])
    }

    /// The null commitment the miner builds for the quorum based at `base`.
    fn null(&self, llmq_type: LlmqType, base: u32) -> LlmqCommitment {
        let version = self.version(llmq_type, BlockId(base));
        null_commitment(self.llmq(llmq_type), self.qhash(base), version, (base % 24) as i16)
    }

    /// `process_block` of `block` at the main chain's `height`, on the tip `tip`.
    fn connect_on(&self, block: &Block, height: u32, tip: Option<BlockId>, mode: QcMode) -> Run {
        self.connect_on_with(&SerialBlsChecks, block, height, tip, mode)
    }

    /// [`Fixture::connect_on`], the block's signature checks run by `bls_checks`.
    fn connect_on_with(
        &self,
        bls_checks: &dyn BlsCheckRunner,
        block: &Block,
        height: u32,
        tip: Option<BlockId>,
        mode: QcMode,
    ) -> Run {
        let mut effects = Vec::new();
        let at = QcBlockAt {
            id: BlockId(height),
            height,
        };
        let r = self
            .with_runner(bls_checks, |ctx| process_block(ctx, block, at, tip, mode, &mut effects));
        (r, effects)
    }

    /// `process_block` of `qcs` mined at the main chain's `height`, connected on its parent.
    fn connect(&self, height: u32, qcs: &[LlmqCommitment], mode: QcMode) -> Run {
        self.connect_with(&SerialBlsChecks, height, qcs, mode)
    }

    /// [`Fixture::connect`], the block's signature checks run by `bls_checks`.
    fn connect_with(
        &self,
        bls_checks: &dyn BlsCheckRunner,
        height: u32,
        qcs: &[LlmqCommitment],
        mode: QcMode,
    ) -> Run {
        let block = qc_block(height, qcs);
        self.connect_on_with(bls_checks, &block, height, Some(BlockId(height - 1)), mode)
    }

    /// Writes the rows of `qcs` mined by the main chain's block `height`, as `process_block`
    /// returns them.
    fn mine(&mut self, height: u32, qcs: &[LlmqCommitment]) {
        self.mine_on(BlockId(height), height, qcs);
    }

    /// [`Fixture::mine`] by the block `block` at `height`, on any branch.
    fn mine_on(&mut self, block: BlockId, height: u32, qcs: &[LlmqCommitment]) {
        let records = self.with(|ctx| {
            qcs.iter()
                .map(|qc| {
                    let params = self.llmq(qc.llmq_type);
                    let (base, base_height) =
                        ctx.index.lookup(&as_block_hash(&qc.quorum_hash)).expect("a known base");
                    mined_record(&ctx.llmq, params, qc, base, base_height).expect("complete")
                })
                .collect()
        });
        self.store
            .apply(&EvoDelta {
                block: self.chain.hash(block),
                height,
                dml: None,
                mined_commitments: records,
                credit_data: None,
                credit_pool: None,
                mnhf_signals: None,
                erased_mined: Vec::new(),
                evo_tip: None,
            })
            .expect("a consistent delta");
    }

    fn precomputes(&self) -> Vec<(BlockId, PrecomputeMode)> {
        self.precompute.0.borrow().clone()
    }

    /// `check_llmq_commitment` of the transaction of `payload` on `prev`.
    fn check(
        &self,
        payload: Vec<u8>,
        prev: u32,
        verifier: &dyn CommitmentVerifier,
    ) -> (Result<Result<(), TxValidationError>, QcBlockError>, Vec<Effect>) {
        let tx = qc_tx(payload);
        let mut effects = Vec::new();
        let r = self.with(|ctx| {
            check_llmq_commitment(&tx, BlockId(prev), &ctx.llmq, ctx.index, verifier, &mut effects)
        });
        (r, effects)
    }

    /// [`Fixture::check`] with the node's verifier.
    fn node_check(
        &self,
        payload: Vec<u8>,
        prev: u32,
    ) -> Result<Result<(), TxValidationError>, QcBlockError> {
        self.with(|ctx| {
            let verifier = NodeCommitmentVerifier {
                llmq: ctx.llmq,
                members: ctx.members,
            };
            check_llmq_commitment(
                &qc_tx(payload),
                BlockId(prev),
                &ctx.llmq,
                ctx.index,
                &verifier,
                &mut Vec::new(),
            )
        })
    }

    /// `qfcommit_verdict` on the main chain's `tip` for a peer of `version`.
    fn qfcommit(
        &self,
        pool: &mut MineablePool,
        tip: u32,
        version: i32,
        qc: &LlmqCommitment,
    ) -> Result<QfcommitOutcome, QcBlockError> {
        self.with(|ctx| {
            qfcommit_verdict(
                pool,
                &ctx.llmq,
                ctx.index,
                ctx.mined,
                ctx.members,
                BlockId(tip),
                version,
                qc,
                &mut Vec::new(),
            )
        })
    }
}

fn invalid<T>(reason: &'static str) -> Result<T, QcBlockError> {
    Err(QcBlockError::invalid(reason))
}

fn tx_invalid(reason: &'static str) -> Result<Result<(), TxValidationError>, QcBlockError> {
    Ok(Err(TxValidationError::new(TxValidationResult::Consensus, reason)))
}

fn record(
    qc: &LlmqCommitment,
    base_height: u32,
    rotation_index: Option<i16>,
) -> MinedCommitmentRecord {
    MinedCommitmentRecord {
        llmq_type: qc.llmq_type,
        quorum_hash: qc.quorum_hash,
        base_height,
        rotation_index,
        commitment: Arc::new(qc.clone()),
    }
}

fn erase(qc: &LlmqCommitment) -> Effect {
    Effect::EraseMineableCommitment {
        llmq_type: qc.llmq_type,
        quorum_hash: qc.quorum_hash,
        commitment_hash: commitment_hash(qc),
    }
}

// ---- 1. the helpers ----

/// `IsMiningPhase` (`blockprocessor.cpp` v24:311-324): `[cycleStart + start, cycleStart + end]`,
/// both edges included.
#[test]
fn mining_phase_includes_both_window_edges() {
    let f = Fixture::new();
    // LLMQ_TEST: window [10, 18] of each 24-block cycle.
    let test = f.llmq(TEST);
    let window: Vec<u32> = (40..100).filter(|&h| is_mining_phase(test, h)).collect();
    let expected: Vec<u32> = (40..=42).chain(58..=66).chain(82..=90).collect();
    assert_eq!(window, expected);
    // LLMQ_TEST_DIP0024: window [12, 20].
    let rot = f.llmq(ROT);
    assert!(!is_mining_phase(rot, 59));
    assert!(is_mining_phase(rot, 60));
    assert!(is_mining_phase(rot, 68));
    assert!(!is_mining_phase(rot, 69));
    assert!(is_mining_phase(rot, 12), "the first cycle has a window");
}

/// `GetQuorumBlockHash` (v24:629-642): the active chain's block at `nHeight - nHeight %
/// dkgInterval + quorumIndex`, null where the chain has none (above the tip, or below zero).
#[test]
fn quorum_block_hash_takes_the_cycle_start_plus_the_index() {
    let f = Fixture::new();
    let (test, rot) = (f.llmq(TEST), f.llmq(ROT));
    let hash = |params, tip: u32, height, index| {
        f.with(|ctx| quorum_block_hash(&ctx.llmq, params, BlockId(tip), height, index))
            .expect("complete")
    };
    let block = |h| Some(f.chain.hash(BlockId(h)));
    assert_eq!(hash(test, 57, 58, 0), block(48));
    assert_eq!(hash(test, 70, 71, 0), block(48), "the cycle start, not the height");
    assert_eq!(hash(test, 72, 73, 0), block(72), "the next cycle starts at 72");
    assert_eq!(hash(rot, 59, 60, 1), block(49), "plus the quorum index");
    assert_eq!(hash(rot, 59, 60, 3), block(51));
    // The block being connected, and anything above the tip: Core's null (v24:628).
    assert_eq!(hash(test, 71, 72, 0), None);
    assert_eq!(hash(test, 47, 58, 0), None);
    assert_eq!(hash(test, 57, 58, 30), None, "48 + 30 is above the tip");
    // Below zero.
    assert_eq!(hash(test, 57, 58, -48), Some(f.chain.hash(BlockId(0))));
    assert_eq!(hash(test, 57, 58, -49), None);
    // Through the tip's ancestry: a tip on the fork reads the fork's block.
    let on_fork =
        f.with(|ctx| quorum_block_hash(&ctx.llmq, test, FORK_49, 58, 0)).expect("complete");
    assert_eq!(on_fork, Some(f.chain.hash(FORK_48)));
}

/// `GetNumCommitmentsRequired` (v24:606-626): 0 outside the window; one per quorum of the cycle
/// whose base the chain holds and that is not mined, `signingActiveQuorumCount` of them for a
/// rotated type, with rotation evaluated at `pindex`: the tip for a new block, else the tip's
/// ancestor at the height (v24:614).
#[test]
fn commitments_required_count_unmined_quorums_of_the_window() {
    let mut f = Fixture::new();
    let required = |f: &Fixture, llmq_type, tip: u32, height| {
        f.with(|ctx| num_commitments_required(ctx, f.llmq(llmq_type), BlockId(tip), height))
            .expect("complete")
    };
    assert_eq!(required(&f, TEST, 56, 57), 0);
    assert_eq!(required(&f, TEST, 57, 58), 1);
    assert_eq!(required(&f, TEST, 65, 66), 1);
    assert_eq!(required(&f, TEST, 66, 67), 0);
    assert_eq!(required(&f, TEST, 47, 58), 0, "the base is not held");
    assert_eq!(required(&f, ROT, 58, 59), 0);
    assert_eq!(required(&f, ROT, 59, 60), 2, "signingActiveQuorumCount");
    // Mined: none for TEST after its commitment, one left for the rotated type.
    f.mine(58, &[f.test_qc()]);
    assert_eq!(required(&f, TEST, 57, 58), 1, "the row is above the tip");
    assert_eq!(required(&f, TEST, 58, 59), 0);
    assert_eq!(required(&f, TEST, 65, 66), 0);
    f.mine(60, &[f.rot_qc(0)]);
    assert_eq!(required(&f, ROT, 60, 61), 1);
    assert_eq!(required(&f, ROT, 59, 60), 2, "rows resolve through the tip");
    // A mined commitment on another branch does not count: the fork's tip still needs it.
    assert_eq!(required(&f, TEST, FORK_49.0, 58), 1);

    // `pindex` is the ancestor at the height when the tip is above it. With DIP0024 from
    // height 60, rotation is off for the cycle at 48 (decided at 47) and on for the cycle at 72.
    let f = Fixture::with_deployments(1, 1, 60);
    assert_eq!(required(&f, ROT, 59, 60), 1, "rotation off at the tip 59");
    assert_eq!(required(&f, ROT, 75, 60), 1, "rotation off at the ancestor 60, not the tip 75");
    assert_eq!(required(&f, ROT, 83, 84), 2, "rotation on at the tip 83");
}

// ---- 2. below DIP3 ----

/// `ProcessBlock` below DIP3 (v24:218-221) writes only its best-block marker: nothing is read,
/// not even a payload that does not decode, and the precompute does not run.
#[test]
fn below_dip3_nothing_is_read() {
    let f = Fixture::with_deployments(59, 1, 1);
    let bad = payload_block(&[vec![1]]);
    for mode in MODES {
        assert_eq!(f.connect_on(&bad, 58, Some(BlockId(57)), mode), (Ok(vec![]), vec![]));
    }
    assert_eq!(f.precomputes(), vec![]);
    // The first DIP3 block reads it.
    let (r, effects) = f.connect_on(&bad, 59, Some(BlockId(58)), WRITE);
    assert_eq!(r, invalid(BAD_QC_PAYLOAD));
    assert_eq!(effects, vec![precomputed(BlockId(59), PrecomputeMode::Connect)]);
    assert_eq!(f.precomputes(), vec![(BlockId(59), PrecomputeMode::Connect)]);
}

// ---- 3. the counts ----

/// The count checks (v24:234-252), per type enabled at the parent: fewer commitments than
/// required is `bad-qc-missing`, more is `bad-qc-not-allowed`, and a null commitment counts.
#[test]
fn counts_need_exactly_the_required_commitments() {
    let mut f = Fixture::new();
    let (qc, null_test, null_platform) = (f.test_qc(), f.null(TEST, 48), f.null(PLATFORM, 48));
    // In both windows' first block at 58, TEST and PLATFORM need one each.
    assert_eq!(f.connect(58, &[], WRITE).0, invalid(BAD_QC_MISSING));
    assert_eq!(
        f.connect(58, std::slice::from_ref(&null_platform), WRITE).0,
        invalid(BAD_QC_MISSING)
    );
    assert_eq!(f.connect(58, std::slice::from_ref(&qc), WRITE).0, invalid(BAD_QC_MISSING));
    assert_eq!(
        f.connect(58, &[qc.clone(), null_platform.clone()], WRITE).0,
        Ok(vec![record(&qc, 48, None)])
    );
    assert_eq!(f.connect(58, &[null_test.clone(), null_platform.clone()], WRITE).0, Ok(vec![]));
    // The rotated type's window starts at 60: none of it is allowed at 58.
    let rot = f.null(ROT, 48);
    assert_eq!(
        f.connect(58, &[qc.clone(), null_platform.clone(), rot.clone()], WRITE).0,
        invalid(BAD_QC_NOT_ALLOWED)
    );
    // At 60 it needs both quorums.
    let both = |extra: &[LlmqCommitment]| {
        let mut qcs = vec![null_test.clone(), null_platform.clone()];
        qcs.extend_from_slice(extra);
        qcs
    };
    assert_eq!(f.connect(60, &both(std::slice::from_ref(&rot)), WRITE).0, invalid(BAD_QC_MISSING));
    assert_eq!(f.connect(60, &both(&[rot.clone(), f.null(ROT, 49)]), WRITE).0, Ok(vec![]));
    assert_eq!(
        f.connect(60, &both(&[rot.clone(), f.null(ROT, 49), f.null(ROT, 50)]), WRITE).0,
        invalid(BAD_QC_NOT_ALLOWED)
    );
    // Outside the window, nothing.
    assert_eq!(f.connect(57, &[], WRITE).0, Ok(vec![]));
    assert_eq!(
        f.connect(57, std::slice::from_ref(&null_test), WRITE).0,
        invalid(BAD_QC_NOT_ALLOWED)
    );
    assert_eq!(
        f.connect(67, std::slice::from_ref(&null_test), WRITE).0,
        invalid(BAD_QC_NOT_ALLOWED)
    );
    // Once mined, no other commitment of the type, null included.
    f.mine(58, std::slice::from_ref(&qc));
    assert_eq!(
        f.connect(59, &[null_test.clone(), null_platform.clone()], WRITE).0,
        invalid(BAD_QC_NOT_ALLOWED)
    );
    assert_eq!(f.connect(59, std::slice::from_ref(&null_platform), WRITE).0, Ok(vec![]));
    // The checks run before the pre-pass and the commitments: a missing type wins over a
    // commitment that would fail.
    let mut bad = qc.clone();
    bad.quorum_hash = f.qhash(49);
    assert_eq!(f.connect(58, &[bad], WRITE).0, invalid(BAD_QC_MISSING));
}

/// Crash replay (`Tip() == nullptr`, v24:236-238, :347-349; LQ2 D5): the count checks are
/// skipped, and each commitment's own quorum hash is taken.
#[test]
fn crash_replay_skips_the_counts() {
    let f = Fixture::new();
    let platform = f.null(PLATFORM, 48);
    let block = qc_block(58, std::slice::from_ref(&platform));
    assert_eq!(f.connect_on(&block, 58, Some(BlockId(57)), WRITE).0, invalid(BAD_QC_MISSING));
    assert_eq!(f.connect_on(&block, 58, None, WRITE).0, Ok(vec![]));
    let empty = qc_block(58, &[]);
    assert_eq!(f.connect_on(&empty, 58, None, WRITE).0, Ok(vec![]));
    // A null quorum hash is still `bad-qc-block` (v24:351-358).
    let mut zero = platform.clone();
    zero.quorum_hash = QuorumHash::from([0; 32]);
    let block = qc_block(58, std::slice::from_ref(&zero));
    assert_eq!(f.connect_on(&block, 58, None, WRITE).0, invalid(BAD_QC_BLOCK));
    // Core's replay runs before `LoadChainTip`, so a non-null commitment above height 0 trips
    // `IsMiningPhase`'s assert (v24:317, :395); the verdicts below are LQ2's own (D5), not
    // Core's. Outside the window: `bad-qc-not-allowed` on the tip, `bad-qc-height` in replay.
    let qc = qc_block(57, &[f.test_qc()]);
    assert_eq!(f.connect_on(&qc, 57, Some(BlockId(56)), WRITE).0, invalid(BAD_QC_NOT_ALLOWED));
    assert_eq!(f.connect_on(&qc, 57, None, WRITE).0, invalid(BAD_QC_HEIGHT));
    // The quorum hash is the commitment's own: a hash that is not the cycle's block passes the
    // `bad-qc-block` checks, and `Verify` judges it.
    let mut other = f.signed(TEST, 49, &[0, 1]);
    other.quorum_index = None;
    let block = qc_block(58, &[other]);
    assert_eq!(f.connect_on(&block, 58, Some(BlockId(57)), WRITE).0, invalid(BAD_QC_MISSING));
    assert_eq!(f.connect_on(&block, 58, None, WRITE).0, invalid(BAD_QC_INVALID));
}

// ---- 4. row 24 ----

/// Row 24 (A1 §2.25; LQ2 D10): v24's pre-pass (v24:255-265) rejects two non-null commitments of
/// one (type, quorumHash) with `bad-qc-dup` in every mode, after the counts and before the
/// signature checks and every `ProcessCommitment`; null duplicates are exempt.
#[test]
fn row_24_duplicate_prepass() {
    let f = Fixture::new();
    let (test, platform, rot0, rot1) =
        (f.test_qc(), f.null(PLATFORM, 48), f.rot_qc(0), f.rot_qc(1));
    let block = |rots: &[LlmqCommitment]| {
        let mut qcs = vec![test.clone(), platform.clone()];
        qcs.extend_from_slice(rots);
        qcs
    };
    let dup = block(&[rot0.clone(), rot0.clone()]);
    for mode in MODES {
        let (r, effects) = f.connect(60, &dup, mode);
        assert_eq!(r, invalid(BAD_QC_DUP), "{mode:?}");
        // Nothing beyond the precompute: no members read, no erasure.
        assert_eq!(effects, vec![precomputed(BlockId(60), PrecomputeMode::Connect)], "{mode:?}");
    }
    // It wins over a bad signature with the signature checks.
    let mut bad_sig = rot0.clone();
    sign(&mut bad_sig, &[1, 2, 3, 3]);
    let bad_sigs = block(&[bad_sig.clone(), bad_sig.clone()]);
    assert_eq!(f.connect(60, &bad_sigs, WRITE_BLS).0, invalid(BAD_QC_DUP));
    // ... which is what the same block without the duplicate gets.
    assert_eq!(
        f.connect(60, &block(&[bad_sig, rot1.clone()]), WRITE_BLS).0,
        invalid(BAD_QC_INVALID)
    );
    // It wins over a failure of an earlier commitment's `ProcessCommitment`: LLMQ_TEST sorts
    // first and has one signer (`bad-qc-invalid` alone).
    let mut weak = f.signed(TEST, 48, &[0]);
    weak.signers = bits(3, &[0]);
    let weak_dup = vec![weak.clone(), platform.clone(), rot0.clone(), rot0.clone()];
    for mode in MODES {
        assert_eq!(f.connect(60, &weak_dup, mode).0, invalid(BAD_QC_DUP), "{mode:?}");
    }
    let weak_alone = vec![weak, platform.clone(), rot0.clone(), rot1.clone()];
    assert_eq!(f.connect(60, &weak_alone, WRITE).0, invalid(BAD_QC_INVALID));
    // Two null duplicates pass the pre-pass, and each is a valid null commitment.
    let null0 = f.null(ROT, 48);
    assert_eq!(
        f.connect(60, &block(&[null0.clone(), null0]), WRITE).0,
        Ok(vec![record(&test, 48, None)])
    );
    // Distinct quorums are no duplicate.
    let (r, _) = f.connect(60, &block(&[rot0.clone(), rot1.clone()]), WRITE);
    assert_eq!(
        r,
        Ok(vec![record(&test, 48, None), record(&rot0, 48, Some(0)), record(&rot1, 49, Some(1))])
    );
}

// ---- 5. commitments_from_block ----

/// `GetCommitmentsFromBlock` (v24:564-604): in Core's order, a payload that does not decode,
/// an unconfigured type, a second commitment of a type not rotated at `pindex` (null or not),
/// and any commitment before DIP3 at `pindex`; otherwise the commitments stably sorted by type.
#[test]
fn commitments_from_block_rules() {
    let f = Fixture::with_deployments(50, 1, 1);
    let from = |block: &Block, pindex: u32| {
        f.with(|ctx| commitments_from_block(&ctx.llmq, block, BlockId(pindex)))
    };
    let (test, null_test) = (f.test_qc(), f.null(TEST, 48));
    // A payload that does not decode, or with trailing bytes.
    assert_eq!(from(&payload_block(&[vec![1]]), 58), invalid(BAD_QC_PAYLOAD));
    let mut trailing = qc_payload(58, &test);
    trailing.push(0);
    assert_eq!(from(&payload_block(&[trailing]), 58), invalid(BAD_QC_PAYLOAD));
    let mut unconfigured = test.clone();
    unconfigured.llmq_type = UNCONFIGURED;
    assert_eq!(from(&qc_block(58, &[unconfigured.clone()]), 58), invalid(BAD_QC_COMMITMENT_TYPE));
    // In block order: the first failure wins.
    let bad_first = payload_block(&[vec![1], qc_payload(58, &unconfigured)]);
    assert_eq!(from(&bad_first, 58), invalid(BAD_QC_PAYLOAD));
    let type_first = payload_block(&[qc_payload(58, &unconfigured), vec![1]]);
    assert_eq!(from(&type_first, 58), invalid(BAD_QC_COMMITMENT_TYPE));
    // A non-rotated type twice, null or not.
    assert_eq!(
        from(&qc_block(58, &[null_test.clone(), null_test.clone()]), 58),
        invalid(BAD_QC_DUP)
    );
    assert_eq!(from(&qc_block(58, &[test.clone(), null_test.clone()]), 58), invalid(BAD_QC_DUP));
    assert_eq!(
        from(&qc_block(58, &[test.clone(), f.null(PLATFORM, 48), null_test.clone()]), 58),
        invalid(BAD_QC_DUP)
    );
    // A rotated type twice: allowed where rotation is enabled at `pindex` (the cycle at 48),
    // a duplicate in the first cycle, for which rotation is never enabled (LQ2 D12).
    let rot_twice = qc_block(60, &[f.null(ROT, 48), f.null(ROT, 49)]);
    assert_eq!(from(&rot_twice, 60).map(|qcs| qcs.len()), Ok(2));
    assert_eq!(from(&rot_twice, 20), invalid(BAD_QC_DUP));
    // Rotation is read at `pindex`, not its parent: DIP0024 from 60 enables it for the cycle at
    // 72 but not at 71's (48).
    let late = Fixture::with_deployments(1, 1, 60);
    let at_72 = qc_block(72, &[late.null(ROT, 72), late.null(ROT, 73)]);
    let from_late = |pindex: u32| {
        late.with(|ctx| commitments_from_block(&ctx.llmq, &at_72, BlockId(pindex)))
            .map(|qcs| qcs.len())
    };
    assert_eq!(from_late(72), Ok(2));
    assert_eq!(from_late(71), invalid(BAD_QC_DUP));
    // Before DIP3 at `pindex` (50 here): any commitment is premature, and an empty block is not;
    // the decode failures come first.
    assert_eq!(
        from(&qc_block(49, std::slice::from_ref(&null_test)), 49),
        invalid(BAD_QC_PREMATURE)
    );
    assert_eq!(
        from(&qc_block(50, std::slice::from_ref(&null_test)), 50),
        Ok(vec![null_test.clone()])
    );
    assert_eq!(from(&qc_block(49, &[]), 49), Ok(vec![]));
    assert_eq!(from(&payload_block(&[vec![1]]), 49), invalid(BAD_QC_PAYLOAD));
    assert_eq!(
        from(&qc_block(49, &[null_test.clone(), null_test.clone()]), 49),
        invalid(BAD_QC_DUP)
    );
    // Other transactions are skipped; the result is sorted by type, block order within a type.
    let (rot0, rot1, platform) = (f.rot_qc(0), f.rot_qc(1), f.null(PLATFORM, 48));
    let mut txs = vec![plain_tx()];
    for qc in [&platform, &rot1, &test, &rot0] {
        txs.push(qc_tx_bytes(qc_payload(60, qc)));
        txs.push(plain_tx());
    }
    assert_eq!(from(&block_of(&txs), 60), Ok(vec![test, rot1, rot0, platform]));
}

// ---- 6. ProcessCommitment ----

/// `ProcessCommitment`'s `bad-qc-block` (v24:338-367): the quorum hash must be the active
/// chain's block at the cycle start plus the commitment's index, null commitments included.
#[test]
fn process_commitment_needs_the_cycle_block() {
    let f = Fixture::new();
    let platform = f.null(PLATFORM, 48);
    let mut wrong = f.test_qc();
    wrong.quorum_hash = f.qhash(49);
    assert_eq!(f.connect(58, &[wrong, platform.clone()], WRITE).0, invalid(BAD_QC_BLOCK));
    let mut fork = f.test_qc();
    fork.quorum_hash = f.hash(FORK_48);
    assert_eq!(f.connect(58, &[fork, platform.clone()], WRITE).0, invalid(BAD_QC_BLOCK));
    let mut null = f.null(TEST, 48);
    null.quorum_hash = f.qhash(47);
    assert_eq!(f.connect(58, &[null, platform.clone()], WRITE).0, invalid(BAD_QC_BLOCK));
    // A rotated commitment's index selects the block.
    let (test, null_test) = (f.test_qc(), f.null(TEST, 48));
    let mut swapped = f.rot_qc(1);
    swapped.quorum_index = Some(0);
    let qcs = [null_test.clone(), platform.clone(), f.rot_qc(0), swapped];
    assert_eq!(f.connect(60, &qcs, WRITE).0, invalid(BAD_QC_BLOCK));
    // An index above the tip: Core's null hash.
    let mut far = f.rot_qc(1);
    far.quorum_index = Some(20);
    let qcs = [null_test, platform.clone(), f.rot_qc(0), far];
    assert_eq!(f.connect(60, &qcs, WRITE).0, invalid(BAD_QC_BLOCK));
    // The checks of the second commitment run after the first's row.
    let mut wrong_platform = platform;
    wrong_platform.quorum_hash = f.qhash(49);
    let (r, effects) = f.connect(58, &[test.clone(), wrong_platform], WRITE);
    assert_eq!(r, invalid(BAD_QC_BLOCK));
    assert_eq!(
        effects,
        vec![
            precomputed(BlockId(58), PrecomputeMode::Connect),
            members_read(TEST, BlockId(48)),
            erase(&test),
        ],
        "an effect stays emitted when a later commitment fails"
    );
}

/// A null commitment (v24:369-379) is checked by `VerifyNull` only and writes nothing.
#[test]
fn process_commitment_null() {
    let f = Fixture::new();
    let platform = f.null(PLATFORM, 48);
    let (r, effects) = f.connect(58, &[f.null(TEST, 48), platform.clone()], WRITE);
    assert_eq!(r, Ok(vec![]));
    assert_eq!(effects, vec![precomputed(BlockId(58), PrecomputeMode::Connect)]);
    // `VerifySizes` fails: still null, not valid.
    let mut long = f.null(TEST, 48);
    long.signers = bits(4, &[]);
    assert!(long.is_null());
    assert_eq!(f.connect(58, &[long, platform.clone()], WRITE).0, invalid(BAD_QC_INVALID_NULL));
    let mut short = f.null(TEST, 48);
    short.valid_members = bits(2, &[]);
    assert_eq!(f.connect(58, &[short, platform], WRITE).0, invalid(BAD_QC_INVALID_NULL));
}

/// `bad-qc-dup` for a commitment the active chain already mined (v24:381-393), reached only in
/// crash replay (the counts reject it on the tip), through the parent's ancestry.
#[test]
fn process_commitment_already_mined() {
    let mut f = Fixture::new();
    let qc = f.test_qc();
    f.mine(58, std::slice::from_ref(&qc));
    let block = qc_block(59, std::slice::from_ref(&qc));
    assert_eq!(f.connect_on(&block, 59, Some(BlockId(58)), WRITE).0, invalid(BAD_QC_NOT_ALLOWED));
    assert_eq!(f.connect_on(&block, 59, None, WRITE).0, invalid(BAD_QC_DUP));
    // Another commitment of the quorum is a duplicate too.
    let other = qc_block(59, &[f.signed(TEST, 48, &[0, 1, 2])]);
    assert_eq!(f.connect_on(&other, 59, None, CHECK).0, invalid(BAD_QC_DUP));
    // A null one is not: it returns before the read.
    let null = qc_block(59, &[f.null(TEST, 48)]);
    assert_eq!(f.connect_on(&null, 59, None, WRITE).0, Ok(vec![]));
    // The block that mined it is not in the parent's ancestry: no duplicate (58 itself, and a
    // later block of the fork, where the row does not resolve).
    let again = qc_block(58, std::slice::from_ref(&qc));
    assert_eq!(f.connect_on(&again, 58, None, WRITE).0, Ok(vec![record(&qc, 48, None)]));
}

/// `bad-qc-height` (v24:395-398): reachable for a type not enabled at the parent (the counts do
/// not cover it) and in crash replay.
#[test]
fn process_commitment_outside_the_mining_phase() {
    let f = Fixture::new();
    // LLMQ_TEST_INSTANTSEND is off under DIP0024: a non-null commitment of it at 50.
    let is = f.signed(INSTANTSEND, 48, &[0, 1]);
    assert_eq!(f.connect(50, std::slice::from_ref(&is), WRITE).0, invalid(BAD_QC_HEIGHT));
    // In its window it reaches `Verify` (this roster answers for every type; Core's
    // `GetAllQuorumMembers` is empty for a type not enabled at the base's parent, which
    // `Verify`'s member-bit check then rejects).
    let qcs = [f.null(TEST, 48), is.clone(), f.null(PLATFORM, 48)];
    assert_eq!(f.connect(58, &qcs, WRITE).0, Ok(vec![record(&is, 48, None)]));
    // Window edges, through crash replay.
    for (height, ok) in [(57, false), (58, true), (66, true), (67, false)] {
        let block = qc_block(height, &[f.test_qc()]);
        let r = f.connect_on(&block, height, None, CHECK).0;
        assert_eq!(
            r,
            if ok {
                Ok(vec![])
            } else {
                invalid(BAD_QC_HEIGHT)
            },
            "{height}"
        );
    }
}

/// `Verify(base, checkSigs = false)` (v24:407-415; `commitment.cpp` v24:113-209): a failure is
/// `bad-qc-invalid`; signatures are not checked.
#[test]
fn process_commitment_verifies_without_signatures() {
    let f = Fixture::new();
    let platform = f.null(PLATFORM, 48);
    let mut weak = f.test_qc();
    weak.signers = bits(3, &[0]);
    assert_eq!(f.connect(58, &[weak, platform.clone()], WRITE).0, invalid(BAD_QC_INVALID));
    let mut version = f.test_qc();
    version.version = 1;
    assert_eq!(f.connect(58, &[version, platform.clone()], WRITE).0, invalid(BAD_QC_INVALID));
    let mut no_key = f.test_qc();
    no_key.quorum_public_key = BlsEagerPk::new(BlsPkValue::invalid(), BlsSchemeKind::Basic);
    assert_eq!(f.connect(58, &[no_key, platform.clone()], WRITE).0, invalid(BAD_QC_INVALID));
    // A bad membersSig passes without the signature checks.
    let mut bad_sig = f.test_qc();
    sign(&mut bad_sig, &[1, 3]);
    assert_eq!(
        f.connect(58, &[bad_sig.clone(), platform], WRITE).0,
        Ok(vec![record(&bad_sig, 48, None)])
    );
}

/// `fJustCheck` (v24:417-419): every check, then nothing written and no erasure; otherwise one
/// row per non-null commitment and, after the members' effects, one `EraseMineableCommitment`
/// with the commitment's `SerializeHash` (v24:428-453; LQ2 D7).
#[test]
fn process_commitment_writes_and_erases() {
    let f = Fixture::new();
    let (test, platform, rot0, rot1) =
        (f.test_qc(), f.null(PLATFORM, 48), f.rot_qc(0), f.rot_qc(1));
    let qcs = [rot1.clone(), platform.clone(), rot0.clone(), test.clone()];
    let pre = precomputed(BlockId(60), PrecomputeMode::Connect);
    let (r, effects) = f.connect(60, &qcs, CHECK);
    assert_eq!(r, Ok(vec![]));
    assert_eq!(
        effects,
        vec![
            pre.clone(),
            members_read(TEST, BlockId(48)),
            members_read(ROT, BlockId(49)),
            members_read(ROT, BlockId(48))
        ]
    );
    let (r, effects) = f.connect(60, &qcs, WRITE);
    assert_eq!(
        r,
        Ok(vec![record(&test, 48, None), record(&rot1, 49, Some(1)), record(&rot0, 48, Some(0))])
    );
    assert_eq!(
        effects,
        vec![
            pre.clone(),
            members_read(TEST, BlockId(48)),
            erase(&test),
            members_read(ROT, BlockId(49)),
            erase(&rot1),
            members_read(ROT, BlockId(48)),
            erase(&rot0),
        ]
    );
    // With the signature checks, all of them first (v24:267-284), then the commitments.
    let (r, effects) = f.connect(60, &qcs, WRITE_BLS);
    assert_eq!(r.map(|rows| rows.len()), Ok(3));
    assert_eq!(
        effects,
        vec![
            pre,
            members_read(TEST, BlockId(48)),
            members_read(ROT, BlockId(49)),
            members_read(ROT, BlockId(48)),
            members_read(TEST, BlockId(48)),
            erase(&test),
            members_read(ROT, BlockId(49)),
            erase(&rot1),
            members_read(ROT, BlockId(48)),
            erase(&rot0),
        ]
    );
    assert_eq!(f.precomputes(), vec![(BlockId(60), PrecomputeMode::Connect); 3]);
}

/// The row's family is decided by `IsQuorumRotationEnabled` at the **base** (v24:421, :437-441;
/// LQ2 D12): a rotated type's commitment based in the first cycle (rotation never enabled) goes
/// in the plain family, wherever it is mined.
#[test]
fn mined_record_family_follows_the_base() {
    let f = Fixture::new();
    let params = f.llmq(ROT);
    let qc = f.rot_qc(1);
    let at = |base: u32| {
        f.with(|ctx| mined_record(&ctx.llmq, params, &qc, BlockId(base), base)).expect("complete")
    };
    assert_eq!(at(49), record(&qc, 49, Some(1)));
    assert_eq!(at(20), record(&qc, 20, None));
    // DIP0024 from 60: off for the cycle at 48, on from the cycle at 72.
    let f = Fixture::with_deployments(1, 1, 60);
    let at = |base: u32| {
        f.with(|ctx| mined_record(&ctx.llmq, params, &qc, BlockId(base), base)).expect("complete")
    };
    assert_eq!(at(49), record(&qc, 49, None));
    assert_eq!(at(73), record(&qc, 73, Some(1)));
    // The plain record of a TEST commitment.
    let test = f.test_qc();
    let r = f.with(|ctx| mined_record(&ctx.llmq, f.llmq(TEST), &test, BlockId(48), 48));
    assert_eq!(r, Ok(record(&test, 48, None)));
}

// ---- 7. fBLSChecks ----

/// `fBLSChecks` (v24:267-284): the queued signature checks of every non-null commitment, one
/// `bad-qc-invalid` for any failure; without them `Verify(checkSigs = false)` accepts a bad
/// signature.
#[test]
fn bls_checks_reject_a_bad_signature() {
    let f = Fixture::new();
    let platform = f.null(PLATFORM, 48);
    let mut bad_members = f.test_qc();
    sign(&mut bad_members, &[1, 3]);
    let mut bad_quorum = f.test_qc();
    bad_quorum.quorum_sig = bad_members.members_sig.clone();
    for bad in [bad_members, bad_quorum] {
        let qcs = [bad.clone(), platform.clone()];
        for mode in [WRITE_BLS, CHECK_BLS] {
            let (r, effects) = f.connect(58, &qcs, mode);
            assert_eq!(r, invalid(BAD_QC_INVALID), "{mode:?}");
            assert_eq!(
                effects,
                vec![
                    precomputed(BlockId(58), PrecomputeMode::Connect),
                    members_read(TEST, BlockId(48))
                ]
            );
        }
        assert_eq!(f.connect(58, &qcs, WRITE).0, Ok(vec![record(&bad, 48, None)]));
        assert_eq!(f.connect(58, &qcs, CHECK).0, Ok(vec![]));
    }
    // A good signature passes both.
    let qcs = [f.test_qc(), platform.clone()];
    assert_eq!(f.connect(58, &qcs, WRITE_BLS).0, Ok(vec![record(&f.test_qc(), 48, None)]));
    // An unknown base on the signature path is Core's reasonless `false` (v24:271-276, LQ2
    // D11); without the checks `ProcessCommitment` sees `bad-qc-block` first.
    let mut unknown = f.test_qc();
    unknown.quorum_hash = QuorumHash::from([0x5a; 32]);
    let qcs = [unknown, platform];
    assert_eq!(
        f.connect(58, &qcs, WRITE_BLS).0,
        Err(QcBlockError::NoReason("no known block index for a quorum hash"))
    );
    assert_eq!(f.connect(58, &qcs, WRITE).0, invalid(BAD_QC_BLOCK));
}

/// Core queues the checks of every commitment before `Wait()` (v24:267-283): a failure does not
/// stop the loop, so the later commitments' members are still read, and a later unknown base is
/// still the reasonless `false` (v24:271-274), returned before the queue's verdict.
#[test]
fn bls_checks_queue_every_commitment() {
    let f = Fixture::new();
    let (rot0, rot1) = (f.rot_qc(0), f.rot_qc(1));
    let mut bad = f.test_qc();
    sign(&mut bad, &[1, 3]);
    let qcs = [rot1.clone(), f.null(PLATFORM, 48), rot0, bad];
    let (r, effects) = f.connect(60, &qcs, WRITE_BLS);
    assert_eq!(r, invalid(BAD_QC_INVALID));
    assert_eq!(
        effects,
        vec![
            precomputed(BlockId(60), PrecomputeMode::Connect),
            members_read(TEST, BlockId(48)),
            members_read(ROT, BlockId(49)),
            members_read(ROT, BlockId(48)),
        ]
    );
    let mut unknown = rot1;
    unknown.quorum_hash = QuorumHash::from([0x5a; 32]);
    let qcs = [unknown, qcs[1].clone(), qcs[2].clone(), qcs[3].clone()];
    assert_eq!(
        f.connect(60, &qcs, WRITE_BLS).0,
        Err(QcBlockError::NoReason("no known block index for a quorum hash"))
    );
}

/// A runner that records each check's reason and verdict, then runs them in reverse: the order
/// Core's workers finish in is not the queue's.
#[derive(Default)]
struct RecordingReversed(std::cell::RefCell<Vec<Vec<(CommitmentReject, BlsVerdict)>>>);

impl BlsCheckRunner for RecordingReversed {
    fn run(&self, mut checks: Vec<BlsCheck>) -> BlsVerdict {
        self.0.borrow_mut().push(checks.iter().map(|c| (c.reject(), c.verdict())).collect());
        checks.reverse();
        SerialBlsChecks.run(checks)
    }
}

/// One queue for the block (DEC-325): every commitment's checks, in `GetCommitmentsFromBlock`'s
/// order (LLMQ_TEST's, then the rotated quorums of index 1 and 0), go to the runner in one call,
/// whose single verdict is the block's. Several commitments failing in different ways (a quorum
/// signature, a members signature) give the same `bad-qc-invalid` and the same effects whatever
/// order the checks run in, and an unknown base anywhere is still the reasonless `false` before
/// any check runs (v24:267-284).
#[test]
fn bls_checks_one_queue_for_the_block() {
    use BlsVerdict::{Invalid, Valid};
    use CommitmentReject::{MembersSignature as M, QuorumSignature as Q};
    let f = Fixture::new();
    let (rot0, rot1, platform) = (f.rot_qc(0), f.rot_qc(1), f.null(PLATFORM, 48));
    let mut bad_quorum = rot1.clone();
    bad_quorum.quorum_sig = rot0.members_sig.clone();
    let mut bad_members = f.test_qc();
    sign(&mut bad_members, &[1, 3]);
    let qcs = [bad_quorum, platform.clone(), rot0.clone(), bad_members];
    let serial = f.connect(60, &qcs, WRITE_BLS);
    assert_eq!(serial.0, invalid(BAD_QC_INVALID));
    let recording = RecordingReversed::default();
    assert_eq!(f.connect_with(&recording, 60, &qcs, WRITE_BLS), serial);
    assert_eq!(
        recording.0.take(),
        vec![vec![(M, Invalid), (Q, Valid), (M, Valid), (Q, Invalid), (M, Valid), (Q, Valid)]]
    );
    // A good block: every check queued, one `Valid`.
    let good = [rot1.clone(), platform.clone(), rot0.clone(), f.test_qc()];
    let recording = RecordingReversed::default();
    let (r, _) = f.connect_with(&recording, 60, &good, WRITE_BLS);
    assert_eq!(r.map(|records| records.len()), Ok(3));
    assert_eq!(recording.0.take(), vec![[(M, Valid), (Q, Valid)].repeat(3)]);
    // An unknown base after a failing commitment: the reasonless `false`, no check run.
    let mut unknown = rot1;
    unknown.quorum_hash = QuorumHash::from([0x5a; 32]);
    let qcs = [unknown, platform, rot0, qcs[3].clone()];
    let recording = RecordingReversed::default();
    assert_eq!(
        f.connect_with(&recording, 60, &qcs, WRITE_BLS).0,
        Err(QcBlockError::NoReason("no known block index for a quorum hash"))
    );
    assert!(recording.0.take().is_empty());
}

// ---- 8. undo ----

/// `UndoBlock` (v24:519-562; LQ2 D6): the precompute in undo mode, then one
/// `ReinsertMineableCommitment` per non-null commitment in `GetCommitmentsFromBlock`'s order; a
/// block whose commitments Core cannot read is its reasonless `false`.
#[test]
fn undo_reinserts_the_non_null_commitments() {
    let f = Fixture::new();
    let (test, rot0, rot1) = (f.test_qc(), f.rot_qc(0), f.rot_qc(1));
    let block = qc_block(
        60,
        &[f.null(PLATFORM, 48), rot1.clone(), test.clone(), f.null(ROT, 50), rot0.clone()],
    );
    let mut effects = Vec::new();
    let r = f.with(|ctx| undo_block(ctx, &block, BlockId(60), &mut effects));
    assert_eq!(r, Ok(()));
    let reinsert = |qc: &LlmqCommitment| Effect::ReinsertMineableCommitment {
        commitment: Arc::new(qc.clone()),
    };
    assert_eq!(
        effects,
        vec![
            precomputed(BlockId(60), PrecomputeMode::Undo),
            reinsert(&test),
            reinsert(&rot1),
            reinsert(&rot0)
        ]
    );
    assert_eq!(f.precomputes(), vec![(BlockId(60), PrecomputeMode::Undo)]);
    // Unreadable: the payload, or a non-rotated duplicate.
    for bad in [payload_block(&[vec![1]]), qc_block(58, &[test.clone(), test])] {
        let mut effects = Vec::new();
        let r = f.with(|ctx| undo_block(ctx, &bad, BlockId(58), &mut effects));
        assert_eq!(r, Err(QcBlockError::NoReason("commitments of a disconnected block")));
        assert_eq!(effects, vec![precomputed(BlockId(58), PrecomputeMode::Undo)]);
    }
    // No commitment: only the precompute.
    let mut effects = Vec::new();
    let r = f.with(|ctx| undo_block(ctx, &qc_block(57, &[]), BlockId(57), &mut effects));
    assert_eq!((r, effects), (Ok(()), vec![precomputed(BlockId(57), PrecomputeMode::Undo)]));
}

// ---- 9. qc_hashes and quorum_root ----

/// A fixture with mined commitments: LLMQ_TEST at 24, 48 and 72 (mined at 34, 58, 82), the
/// rotated type's index 0 at 48 and 72 and index 1 at 49 (mined at 60, 84 and 61), and
/// LLMQ_TEST_PLATFORM at 48 (mined at 58).
fn mined_fixture() -> (Fixture, BTreeMap<&'static str, LlmqCommitment>) {
    let mut f = Fixture::new();
    let qcs = BTreeMap::from([
        ("test24", f.signed(TEST, 24, &[0, 1])),
        ("test48", f.test_qc()),
        ("test72", f.signed(TEST, 72, &[0, 1, 2])),
        ("rot48", f.rot_qc(0)),
        ("rot49", f.rot_qc(1)),
        ("rot72", f.signed(ROT, 72, &[0, 1, 2, 3])),
        ("platform48", f.signed(PLATFORM, 48, &[1, 2])),
    ]);
    f.mine(34, &[qcs["test24"].clone()]);
    f.mine(58, &[qcs["test48"].clone(), qcs["platform48"].clone()]);
    f.mine(60, &[qcs["rot48"].clone()]);
    f.mine(61, &[qcs["rot49"].clone()]);
    f.mine(82, &[qcs["test72"].clone()]);
    f.mine(84, &[qcs["rot72"].clone()]);
    (f, qcs)
}

/// [`qc_hashes`] at `prev` through `cache`, every commitment read lost.
fn lossy_hashes_at(
    f: &Fixture,
    cache: &mut QcHashCache,
    prev: u32,
) -> Result<QcHashes, QcBlockError> {
    f.with(|ctx| qc_hashes(&ctx.llmq, &Lossy(ctx.mined), cache, BlockId(prev)))
}

fn hashes_at(f: &Fixture, prev: u32) -> Result<QcHashes, QcBlockError> {
    f.with(|ctx| qc_hashes(&ctx.llmq, ctx.mined, &mut QcHashCache::new(), BlockId(prev)))
}

/// `GetQcHashes` over `GetMinedAndActiveCommitmentsUntilBlock` (v24:470-517, :818-832): the
/// newest `signingActiveQuorumCount` plain hashes newest first, and the newest per quorum index
/// of a rotated type, read through `prev`.
#[test]
fn qc_hashes_newest_first_and_per_index() {
    let (f, qcs) = mined_fixture();
    let h = |name: &str| commitment_hash(&qcs[name]);
    let mut expected = QcHashes::default();
    expected.push_plain(TEST, h("test72"));
    expected.push_plain(TEST, h("test48"));
    expected.push_plain(PLATFORM, h("platform48"));
    expected.insert_indexed(ROT, 0, h("rot72"));
    expected.insert_indexed(ROT, 1, h("rot49"));
    assert_eq!(hashes_at(&f, 90), Ok(expected));
    // At 60: test48 and test24 (the cap of two), rot48 only (rot49 is mined at 61).
    let mut expected = QcHashes::default();
    expected.push_plain(TEST, h("test48"));
    expected.push_plain(TEST, h("test24"));
    expected.push_plain(PLATFORM, h("platform48"));
    expected.insert_indexed(ROT, 0, h("rot48"));
    assert_eq!(hashes_at(&f, 60), Ok(expected));
    // A row resolves only from its mining block on.
    let mut expected = QcHashes::default();
    expected.push_plain(TEST, h("test24"));
    assert_eq!(hashes_at(&f, 57), Ok(expected.clone()));
    assert_eq!(hashes_at(&f, 34), Ok(expected));
    assert_eq!(hashes_at(&f, 33), Ok(QcHashes::default()));
    // On the fork from 47, only what 47's ancestry mined.
    let mut expected = QcHashes::default();
    expected.push_plain(TEST, h("test24"));
    assert_eq!(hashes_at(&f, FORK_49.0), Ok(expected));
    // An index row without its commitment: Core's `nullopt` (v24:499-503), a store fault.
    let lost = lossy_hashes_at(&f, &mut QcHashCache::new(), 90);
    assert_eq!(lost, Err(StoreError::inconsistent("commitment-not-found").into()));
    let none = lossy_hashes_at(&f, &mut QcHashCache::new(), 33);
    assert_eq!(none, Ok(QcHashes::default()));
}

/// `CalcCbTxMerkleRootQuorums` (`evo/cbtx.cpp` v24:65-156): SB7's `merkle_root_quorums` over the
/// active hashes at `prev` and the block's own special-version type-6 transactions after
/// `vtx[0]`; a payload that does not decode is the CbTx reason.
#[test]
fn quorum_root_is_sb7s_root_over_the_hashes() {
    let (f, qcs) = mined_fixture();
    let h = |name: &str| commitment_hash(&qcs[name]);
    let root = |block: &Block, prev: u32| {
        f.with(|ctx| {
            quorum_root(&ctx.llmq, ctx.mined, &mut QcHashCache::new(), block, BlockId(prev))
        })
        .expect("no fault")
    };
    let all = vec![h("test72"), h("test48"), h("platform48"), h("rot72"), h("rot49")];
    let expected = merkle_root_quorums_from_hashes(all.clone()).expect("a root");
    assert_eq!(root(&qc_block(91, &[]), 90), Ok(expected));
    // The block's own commitment replaces the oldest of its type; null ones are skipped.
    let new = f.signed(TEST, 72, &[1, 2]);
    let block = qc_block(91, &[new.clone(), f.null(PLATFORM, 72)]);
    let with_new =
        vec![commitment_hash(&new), h("test72"), h("platform48"), h("rot72"), h("rot49")];
    assert_eq!(root(&block, 90), Ok(merkle_root_quorums_from_hashes(with_new).expect("a root")));
    let rotated = f.signed(ROT, 73, &[0, 1, 2, 3]);
    let block = qc_block(91, std::slice::from_ref(&rotated));
    let with_rotated =
        vec![h("test72"), h("test48"), h("platform48"), h("rot72"), commitment_hash(&rotated)];
    assert_eq!(
        root(&block, 90),
        Ok(merkle_root_quorums_from_hashes(with_rotated).expect("a root"))
    );
    // A payload that does not decode; `vtx[0]` and non-special transactions are not read.
    let bad = payload_block(&[vec![1]]);
    assert_eq!(
        root(&bad, 90).map_err(|e| e.reject_reason().to_owned()),
        Err("bad-qc-payload-calc-cbtx-quorummerkleroot".to_owned())
    );
    let first = block_of(&[qc_tx_bytes(vec![1]), tx_bytes(2, TxType::QuorumCommitment, vec![1])]);
    assert_eq!(root(&first, 90), Ok(merkle_root_quorums_from_hashes(all).expect("a root")));
    // A fault of the reader is the outer error.
    let lost = f.with(|ctx| {
        let block = qc_block(91, &[]);
        quorum_root(&ctx.llmq, &Lossy(ctx.mined), &mut QcHashCache::new(), &block, BlockId(90))
    });
    assert_eq!(lost, Err(StoreError::inconsistent("commitment-not-found").into()));
}

/// Quirk 18: merkleRootQuorums iterates every configured LLMQ type, disabled ones included, so
/// stale leaves remain (`evo/cbtx.cpp` v24:65-160; `GetMinedAndActiveCommitmentsUntilBlock`
/// iterates `consensus.llmqs`, `llmq/blockprocessor.cpp` v24:818-832; LQ2 D13). Here
/// LLMQ_TEST_INSTANTSEND (off once DIP0024 is active) and LLMQ_TEST_V17 (off without its
/// versionbits deployment) are configured, not enabled at `prev`, and their mined commitments
/// are leaves of the root.
#[test]
fn quirk_18_quorum_root_over_configured_types() {
    let (mut f, qcs) = mined_fixture();
    let (is, v17) = (f.signed(INSTANTSEND, 48, &[0, 1]), f.signed(V17, 72, &[0, 2]));
    f.mine(59, std::slice::from_ref(&is));
    f.mine(83, std::slice::from_ref(&v17));
    let enabled: Vec<LlmqType> = f
        .with(|ctx| {
            enabled_quorum_params(&ctx.llmq, BlockId(90))
                .map(|params| params.iter().map(|p| p.llmq_type).collect())
        })
        .expect("complete");
    assert_eq!(enabled, vec![TEST, ROT, PLATFORM]);
    let hashes = hashes_at(&f, 90).expect("no fault");
    assert_eq!(hashes.plain.get(&INSTANTSEND.to_base()), Some(&vec![commitment_hash(&is)]));
    assert_eq!(hashes.plain.get(&V17.to_base()), Some(&vec![commitment_hash(&v17)]));
    let h = |name: &str| commitment_hash(&qcs[name]);
    let enabled_only = vec![h("test72"), h("test48"), h("platform48"), h("rot72"), h("rot49")];
    let mut every = enabled_only.clone();
    every.extend([commitment_hash(&is), commitment_hash(&v17)]);
    let root = f
        .with(|ctx| {
            let block = qc_block(91, &[]);
            quorum_root(&ctx.llmq, ctx.mined, &mut QcHashCache::new(), &block, BlockId(90))
        })
        .expect("no fault");
    assert_eq!(root, Ok(merkle_root_quorums_from_hashes(every).expect("a root")));
    assert_ne!(root, Ok(merkle_root_quorums_from_hashes(enabled_only).expect("a root")));
    // A disabled type's new commitment in the block is a leaf too.
    let is72 = f.signed(INSTANTSEND, 72, &[1, 2]);
    let root = f
        .with(|ctx| {
            quorum_root(
                &ctx.llmq,
                ctx.mined,
                &mut QcHashCache::new(),
                &qc_block(91, std::slice::from_ref(&is72)),
                BlockId(90),
            )
        })
        .expect("no fault");
    let leaves = vec![
        h("test72"),
        h("test48"),
        h("platform48"),
        h("rot72"),
        h("rot49"),
        commitment_hash(&is72),
        commitment_hash(&is),
        commitment_hash(&v17),
    ];
    assert_eq!(root, Ok(merkle_root_quorums_from_hashes(leaves).expect("a root")));
}

/// Core's `GetQcHashes` caches (v24:470-516; LQ2 D22, DEC-325): one cache read through any
/// `prev`, the fork's included, gives what an empty one gives; the hashes of an unchanged set,
/// and then of records already read, are reused without reading their commitments (a lossy
/// reader shows it); `DropQcHashesCache` (:446, :551) follows the effects of a connect that
/// writes and of an undo, not those of a check.
#[test]
fn qc_hash_cache_is_transparent_and_dropped() {
    let (f, qcs) = mined_fixture();
    let mut cache = QcHashCache::new();
    for prev in [90, 60, 57, 33, FORK_49.0, 90, 60] {
        let cached = f.with(|ctx| qc_hashes(&ctx.llmq, ctx.mined, &mut cache, BlockId(prev)));
        assert_eq!(cached, hashes_at(&f, prev), "at {prev}");
    }
    // The set at 95 is 90's.
    let at_90 = hashes_at(&f, 90);
    f.with(|ctx| qc_hashes(&ctx.llmq, ctx.mined, &mut cache, BlockId(90))).expect("no fault");
    assert_eq!(lossy_hashes_at(&f, &mut cache, 95), at_90);
    assert!(lossy_hashes_at(&f, &mut QcHashCache::new(), 95).is_err());
    // 60's records, then 57's set (test24, one of them): each record's hash is reused.
    f.with(|ctx| qc_hashes(&ctx.llmq, ctx.mined, &mut cache, BlockId(60))).expect("no fault");
    assert_eq!(lossy_hashes_at(&f, &mut cache, 57), hashes_at(&f, 57));
    // A record not read since: its commitment is read.
    assert!(lossy_hashes_at(&f, &mut cache, 90).is_err());

    let warm = |cache: &mut QcHashCache| {
        f.with(|ctx| qc_hashes(&ctx.llmq, ctx.mined, cache, BlockId(90))).expect("no fault");
        assert!(lossy_hashes_at(&f, cache, 90).is_ok());
    };
    // LLMQ_TEST_PLATFORM's quorum at 72, unmined, in its mining phase at 83.
    let block = qc_block(83, &[f.signed(PLATFORM, 72, &[0, 1])]);
    // A check writes nothing and erases nothing: the cache stays.
    warm(&mut cache);
    let (r, effects) = f.connect_on(&block, 83, None, CHECK);
    assert!(r.is_ok(), "{r:?}");
    effects.iter().for_each(|e| cache.apply(e));
    assert!(lossy_hashes_at(&f, &mut cache, 90).is_ok());
    // A connect that writes drops it, as does an undo.
    let (r, effects) = f.connect_on(&block, 83, None, WRITE);
    assert!(r.is_ok(), "{r:?}");
    effects.iter().for_each(|e| cache.apply(e));
    assert!(lossy_hashes_at(&f, &mut cache, 90).is_err());
    warm(&mut cache);
    let mut effects = Vec::new();
    f.with(|ctx| {
        undo_block(ctx, &qc_block(84, &[qcs["rot72"].clone()]), BlockId(84), &mut effects)
    })
    .expect("undone");
    effects.iter().for_each(|e| cache.apply(e));
    assert!(lossy_hashes_at(&f, &mut cache, 90).is_err());
    // `clear` is `DropQcHashesCache`.
    warm(&mut cache);
    cache.clear();
    assert!(lossy_hashes_at(&f, &mut cache, 90).is_err());

    // LLMQ_TEST_PLATFORM's quorum at 24 mined by 48 and, with other signers, by the fork's 48:
    // the record's mining block keeps the two hashes apart through one cache.
    let (mut f, _) = mined_fixture();
    let (main, fork) = (f.signed(PLATFORM, 24, &[0, 1]), f.signed(PLATFORM, 24, &[1, 2]));
    assert_ne!(commitment_hash(&main), commitment_hash(&fork));
    f.mine(48, std::slice::from_ref(&main));
    f.mine_on(FORK_48, 48, std::slice::from_ref(&fork));
    let mut cache = QcHashCache::new();
    for prev in [60, FORK_49.0, 60, FORK_49.0] {
        let cached = f.with(|ctx| qc_hashes(&ctx.llmq, ctx.mined, &mut cache, BlockId(prev)));
        assert_eq!(cached, hashes_at(&f, prev), "at {prev}");
    }
    let platform =
        |prev: u32| hashes_at(&f, prev).expect("no fault").plain[&PLATFORM.to_base()].clone();
    assert!(platform(60).contains(&commitment_hash(&main)));
    assert_eq!(platform(FORK_49.0), vec![commitment_hash(&fork)]);
}

// ---- 10. quirk 19 ----

/// Quirk 19: Null commitments of disabled types are accepted at any height subject to height,
/// quorumHash and VerifyNull; signers >= minSize; bitset padding bits are a parse error
/// (`llmq/commitment.cpp` v24:153-160, :211-230, :240-310; `llmq/blockprocessor.cpp`
/// v24:369-378; `serialize.h` v24:452-476, :620-634). The count checks cover only types enabled
/// at the parent (v24:234), and `ProcessCommitment` returns for a null commitment after its
/// quorum hash and `VerifyNull`, before `IsMiningPhase`.
#[test]
fn quirk_19_null_commitments_and_bitset_padding() {
    let f = Fixture::new();
    // LLMQ_TEST_INSTANTSEND is configured and not enabled; 50 is outside its window.
    let null = f.null(INSTANTSEND, 48);
    let (r, effects) = f.connect(50, std::slice::from_ref(&null), WRITE);
    assert_eq!((r, effects), (Ok(vec![]), vec![precomputed(BlockId(50), PrecomputeMode::Connect)]));
    assert_eq!(f.node_check(qc_payload(50, &null), 49), Ok(Ok(())));
    // The same commitment non-null is held to the mining phase.
    let signed = f.signed(INSTANTSEND, 48, &[0, 1]);
    assert_eq!(f.connect(50, std::slice::from_ref(&signed), WRITE).0, invalid(BAD_QC_HEIGHT));
    // Its quorum hash, its sizes and its transaction height are still checked.
    let mut other = null.clone();
    other.quorum_hash = f.qhash(49);
    assert_eq!(f.connect(50, std::slice::from_ref(&other), WRITE).0, invalid(BAD_QC_BLOCK));
    let mut long = null.clone();
    long.valid_members = bits(4, &[]);
    assert_eq!(f.connect(50, std::slice::from_ref(&long), WRITE).0, invalid(BAD_QC_INVALID_NULL));
    assert_eq!(f.node_check(qc_payload(50, &long), 49), tx_invalid(BAD_QC_INVALID_NULL));
    assert_eq!(f.node_check(qc_payload(51, &null), 49), tx_invalid(BAD_QC_HEIGHT));

    // signers >= minSize (2 for LLMQ_TEST), valid members too.
    let platform = f.null(PLATFORM, 48);
    let mut one = f.test_qc();
    one.signers = bits(3, &[1]);
    sign(&mut one, &[2]);
    assert_eq!(f.connect(58, &[one.clone(), platform.clone()], WRITE).0, invalid(BAD_QC_INVALID));
    assert_eq!(f.node_check(qc_payload(58, &one), 57), tx_invalid(BAD_QC_INVALID));
    let two = f.test_qc();
    assert_eq!(
        f.connect(58, &[two.clone(), platform.clone()], WRITE_BLS).0,
        Ok(vec![record(&two, 48, None)])
    );
    assert_eq!(f.node_check(qc_payload(58, &two), 57), Ok(Ok(())));
    let mut few_valid = f.test_qc();
    few_valid.valid_members = bits(3, &[0]);
    assert_eq!(
        f.connect(58, &[few_valid.clone(), platform.clone()], WRITE).0,
        invalid(BAD_QC_INVALID)
    );
    assert_eq!(f.node_check(qc_payload(58, &few_valid), 57), tx_invalid(BAD_QC_INVALID));

    // Bitset padding: version 3 has no index, so after the payload's version and height, the
    // commitment's version, type and quorum hash, each bitset is one size byte (3) and one data
    // byte (bits 0-2 used, 3-7 padding).
    let payload = qc_payload(58, &two);
    const SIGNERS: usize = 2 + 4 + 2 + 1 + 32;
    const VALID: usize = SIGNERS + 2;
    assert_eq!(&payload[SIGNERS..VALID + 2], &[3, 0b011, 3, 0b111]);
    for (at, padded) in [(SIGNERS + 1, 0b1011), (SIGNERS + 1, 0b1000_0011), (VALID + 1, 0b1111)] {
        let mut bytes = payload.clone();
        bytes[at] = padded;
        let block = payload_block(&[bytes.clone()]);
        let from = f.with(|ctx| commitments_from_block(&ctx.llmq, &block, BlockId(58)));
        assert_eq!(from, invalid(BAD_QC_PAYLOAD), "{at} {padded:#b}");
        assert_eq!(f.connect_on(&block, 58, Some(BlockId(57)), WRITE).0, invalid(BAD_QC_PAYLOAD));
        assert_eq!(f.node_check(bytes, 57), tx_invalid(BAD_QC_PAYLOAD), "{at} {padded:#b}");
    }
    // Without padding the same edit decodes.
    let mut unpadded = payload;
    unpadded[SIGNERS + 1] = 0b111;
    let block = payload_block(std::slice::from_ref(&unpadded));
    let from = f.with(|ctx| commitments_from_block(&ctx.llmq, &block, BlockId(58)));
    assert_eq!(from.map(|qcs| qcs[0].signers.count_ones()), Ok(3));
}

// ---- 11. the pool ----

/// `AddMineableCommitment` (v24:840-867; LQ2 D8): the first commitment of a quorum is relayed,
/// one with no more signers is ignored, one with more replaces the quorum's entry and the
/// replaced one stays retrievable by hash.
#[test]
fn pool_add_keeps_the_replaced_commitment() {
    let f = Fixture::new();
    let mut pool = MineablePool::new();
    let (two, three, other) =
        (f.test_qc(), f.signed(TEST, 48, &[0, 1, 2]), f.signed(TEST, 48, &[1, 2]));
    let (h2, h3, ho) = (commitment_hash(&two), commitment_hash(&three), commitment_hash(&other));
    assert_eq!(pool.add(&two), Some(h2));
    assert_eq!(pool.add(&two), None, "equal signers");
    assert_eq!(pool.add(&other), None, "equal signers, another commitment");
    assert!(!pool.has(&ho));
    assert_eq!(pool.add(&three), Some(h3));
    assert_eq!(pool.best_for(TEST, &two.quorum_hash).map(|qc| commitment_hash(qc)), Some(h3));
    assert!(pool.has(&h2) && pool.has(&h3), "Core's leak keeps the old one");
    assert_eq!(pool.get(&h2).map(|qc| (**qc).clone()), Some(two.clone()));
    assert_eq!(pool.add(&two), None, "fewer signers");
    assert_eq!(pool.add(&three), None);
    // Another quorum or type has its own entry.
    let platform = f.signed(PLATFORM, 48, &[0, 1]);
    assert_eq!(pool.add(&platform), Some(commitment_hash(&platform)));
    assert_eq!(pool.best_for(TEST, &two.quorum_hash).map(|qc| commitment_hash(qc)), Some(h3));
    assert_eq!(pool.best_for(TEST, &f.qhash(72)), None);
}

/// The pool's effects: `ProcessCommitment`'s erasure by quorum and by hash (v24:448-453) and
/// `UndoBlock`'s re-insertion (v24:556); others are not the pool's.
#[test]
fn pool_apply_erases_by_quorum_and_by_hash() {
    let f = Fixture::new();
    let (two, three) = (f.test_qc(), f.signed(TEST, 48, &[0, 1, 2]));
    let (h2, h3) = (commitment_hash(&two), commitment_hash(&three));
    let mut pool = MineablePool::new();
    pool.add(&two);
    pool.add(&three);
    // A block mines the old one: the quorum's entry and the old commitment go, the new one stays.
    pool.apply(&erase(&two));
    assert!(pool.best_for(TEST, &two.quorum_hash).is_none());
    assert!(!pool.has(&h2) && pool.has(&h3));
    // An erasure whose hash the pool does not hold removes the entry only.
    let mut pool = MineablePool::new();
    pool.add(&two);
    pool.apply(&Effect::EraseMineableCommitment {
        llmq_type: TEST,
        quorum_hash: two.quorum_hash,
        commitment_hash: [0; 32],
    });
    assert!(pool.best_for(TEST, &two.quorum_hash).is_none());
    assert!(pool.has(&h2));
    // ... and one of another quorum with the commitment's hash removes the commitment only.
    let mut pool = MineablePool::new();
    pool.add(&two);
    pool.apply(&Effect::EraseMineableCommitment {
        llmq_type: TEST,
        quorum_hash: f.qhash(72),
        commitment_hash: h2,
    });
    assert!(!pool.has(&h2));
    assert!(pool.best_for(TEST, &two.quorum_hash).is_none(), "the entry names a removed hash");
    // The type is part of the key.
    let mut pool = MineablePool::new();
    pool.add(&two);
    pool.apply(&Effect::EraseMineableCommitment {
        llmq_type: PLATFORM,
        quorum_hash: two.quorum_hash,
        commitment_hash: [0; 32],
    });
    assert_eq!(pool.best_for(TEST, &two.quorum_hash).map(|qc| commitment_hash(qc)), Some(h2));
    // Re-insertion is `AddMineableCommitment`.
    let mut pool = MineablePool::new();
    pool.apply(&Effect::ReinsertMineableCommitment {
        commitment: Arc::new(two.clone()),
    });
    assert_eq!(pool.best_for(TEST, &two.quorum_hash).map(|qc| commitment_hash(qc)), Some(h2));
    pool.apply(&Effect::ReinsertMineableCommitment {
        commitment: Arc::new(three.clone()),
    });
    assert_eq!(pool.best_for(TEST, &two.quorum_hash).map(|qc| commitment_hash(qc)), Some(h3));
    // Other effects leave it unchanged.
    for other in [precomputed(BlockId(1), PrecomputeMode::Connect), members_read(TEST, BlockId(48))]
    {
        pool.apply(&other);
    }
    assert!(pool.has(&h2) && pool.has(&h3));
    assert_eq!(pool.best_for(TEST, &two.quorum_hash).map(|qc| commitment_hash(qc)), Some(h3));
}

/// `GetMineableCommitments` (v24:880-936): `None` when no commitment is required; else per
/// quorum of the cycle, unless mined, the pool's commitment or a null one with the index and the
/// version for rotation and V19 at `pindex`.
#[test]
fn mineable_commitments_pool_or_null() {
    let mut f = Fixture::new();
    let mineable = |f: &Fixture, pool: &MineablePool, llmq_type, tip: u32, height| {
        f.with(|ctx| pool.mineable_commitments(ctx, f.llmq(llmq_type), BlockId(tip), height))
            .expect("complete")
    };
    let mut pool = MineablePool::new();
    assert_eq!(mineable(&f, &pool, TEST, 56, 57), None);
    let null = mineable(&f, &pool, TEST, 57, 58).expect("required");
    assert_eq!(null, vec![f.null(TEST, 48)]);
    assert_eq!((null[0].version, null[0].quorum_index, null[0].signers.num_bits), (3, None, 3));
    assert!(null[0].is_null());
    let rot = mineable(&f, &pool, ROT, 59, 60).expect("required");
    assert_eq!(rot, vec![f.null(ROT, 48), f.null(ROT, 49)]);
    assert_eq!(
        (rot[1].version, rot[1].quorum_index, rot[1].valid_members.num_bits),
        (4, Some(1), 4)
    );
    // The pool's commitment where it holds one, its best.
    pool.add(&f.rot_qc(1));
    let three = f.signed(TEST, 48, &[0, 1, 2]);
    pool.add(&f.test_qc());
    pool.add(&three);
    assert_eq!(mineable(&f, &pool, ROT, 59, 60), Some(vec![f.null(ROT, 48), f.rot_qc(1)]));
    assert_eq!(mineable(&f, &pool, TEST, 57, 58), Some(vec![three.clone()]));
    // Mined quorums are skipped, and none when none is required.
    f.mine(58, std::slice::from_ref(&three));
    f.mine(60, &[f.rot_qc(0)]);
    assert_eq!(mineable(&f, &pool, TEST, 58, 59), None);
    assert_eq!(mineable(&f, &pool, ROT, 60, 61), Some(vec![f.rot_qc(1)]));
    // The version follows V19 and rotation at `pindex`: legacy without V19.
    let f = Fixture::with_deployments(1, 1000, 1);
    let empty = MineablePool::new();
    let legacy = mineable(&f, &empty, TEST, 57, 58).expect("required");
    assert_eq!((legacy[0].version, legacy[0].quorum_index), (1, None));
    let legacy = mineable(&f, &empty, ROT, 59, 60).expect("required");
    assert_eq!(
        legacy.iter().map(|qc| (qc.version, qc.quorum_index)).collect::<Vec<_>>(),
        vec![(2, Some(0)), (2, Some(1))]
    );
    // Rotation off at `pindex` (DIP0024 from 60): one plain-version quorum.
    let f = Fixture::with_deployments(1, 1, 60);
    let plain = mineable(&f, &empty, ROT, 59, 60).expect("required");
    assert_eq!(
        plain.iter().map(|qc| (qc.version, qc.quorum_index)).collect::<Vec<_>>(),
        vec![(3, None)]
    );
}

/// `GetMineableCommitmentsTx` (v24:938-958): each of `GetMineableCommitments`' commitments in its
/// own special transaction, version 3, type 6, no inputs or outputs, lock time 0, the payload
/// `CFinalCommitmentTxPayload { 1, nHeight, commitment }`; `None` with it.
#[test]
fn mineable_commitments_tx_wraps_each_commitment() {
    let f = Fixture::new();
    let pool = MineablePool::new();
    let txs = |llmq_type, tip: u32, height| {
        f.with(|ctx| pool.mineable_commitments_tx(ctx, f.llmq(llmq_type), BlockId(tip), height))
            .expect("complete")
    };
    assert!(txs(TEST, 56, 57).is_none());
    let rot = txs(ROT, 59, 60).expect("required");
    let nulls = [f.null(ROT, 48), f.null(ROT, 49)];
    assert_eq!(rot.len(), nulls.len());
    for (tx, qc) in rot.iter().zip(&nulls) {
        let mut payload = vec![1, 0, 60, 0, 0, 0];
        write_commitment(qc, &mut payload);
        // nVersion 3 | nType 6, no inputs, no outputs, nLockTime 0, then the payload.
        let mut expected = vec![3, 0, 6, 0, 0, 0, 0, 0, 0, 0];
        write_compact_size(payload.len() as u64, &mut expected);
        expected.extend(&payload);
        assert_eq!(core_serialize(&**tx), expected);
    }
}

/// `ProcessMessage`'s `qfcommit` checks past the solicitation gate (v24:121-209; LQ2 D9), in
/// Core's order.
#[test]
fn qfcommit_verdicts_in_cores_order() {
    let mut f = Fixture::new();
    const NEW: i32 = QFCOMMIT_STALE_REPROP_BAN_VERSION;
    let qc = f.test_qc();
    let mut pool = MineablePool::new();
    let run = |f: &Fixture, pool: &mut MineablePool, tip, version, qc: &LlmqCommitment| {
        f.qfcommit(pool, tip, version, qc).expect("no fault")
    };
    // Null (v24:124-128) and an unconfigured type (:130-136).
    assert_eq!(run(&f, &mut pool, 60, NEW, &f.null(TEST, 48)), QfcommitOutcome::Misbehaving(100));
    let mut unconfigured = qc.clone();
    unconfigured.llmq_type = UNCONFIGURED;
    unconfigured.quorum_hash = QuorumHash::from([0x5a; 32]);
    assert_eq!(run(&f, &mut pool, 60, NEW, &unconfigured), QfcommitOutcome::Misbehaving(100));
    // An unknown base (:144-151) and a base off the active chain (:152-157) are ignored, before
    // the interval check.
    let mut unknown = qc.clone();
    unknown.quorum_hash = QuorumHash::from([0x5a; 32]);
    assert_eq!(run(&f, &mut pool, 60, NEW, &unknown), QfcommitOutcome::Ignored);
    let mut fork = f.signed(TEST, 48, &[0, 1]);
    fork.quorum_hash = f.hash(FORK_49);
    assert_eq!(run(&f, &mut pool, 60, NEW, &fork), QfcommitOutcome::Ignored);
    let mut above = qc.clone();
    above.quorum_hash = f.qhash(61);
    assert_eq!(run(&f, &mut pool, 60, NEW, &above), QfcommitOutcome::Ignored);
    // Not the interval's first block plus the index (:158-164).
    let mut off = qc.clone();
    off.quorum_hash = f.qhash(49);
    assert_eq!(run(&f, &mut pool, 60, NEW, &off), QfcommitOutcome::Misbehaving(100));
    let mut bad_index = f.rot_qc(1);
    bad_index.quorum_index = Some(2);
    assert_eq!(run(&f, &mut pool, 60, NEW, &bad_index), QfcommitOutcome::Misbehaving(100));
    let rot1 = f.rot_qc(1);
    assert_eq!(
        run(&f, &mut MineablePool::new(), 60, NEW, &rot1),
        QfcommitOutcome::Accepted(Some(commitment_hash(&rot1)))
    );
    // Stale (:165-172): below `tip - dkgInterval`, punished from the ban version on.
    assert_eq!(run(&f, &mut pool, 73, NEW, &qc), QfcommitOutcome::Misbehaving(100));
    assert_eq!(run(&f, &mut pool, 73, NEW - 1, &qc), QfcommitOutcome::Ignored);
    assert!(!pool.has(&commitment_hash(&qc)));
    let h3 = commitment_hash(&f.signed(TEST, 48, &[0, 1, 2]));
    assert_eq!(
        run(&f, &mut pool, 72, NEW, &qc),
        QfcommitOutcome::Accepted(Some(commitment_hash(&qc)))
    );
    assert!(pool.has(&commitment_hash(&qc)));
    // The pool holds no more signers than this one (:181-193): ignored, unverified.
    let mut equal = f.signed(TEST, 48, &[1, 2]);
    sign(&mut equal, &[1, 1]);
    assert_eq!(run(&f, &mut pool, 60, NEW, &equal), QfcommitOutcome::Ignored);
    let mut bad = f.signed(TEST, 48, &[0, 1, 2]);
    sign(&mut bad, &[1, 2, 2]);
    assert_eq!(run(&f, &mut pool, 60, NEW, &bad), QfcommitOutcome::Ignored);
    // A fresh pool: `Verify(checkSigs = true)` (:195-201).
    let mut pool = MineablePool::new();
    assert_eq!(run(&f, &mut pool, 60, NEW, &bad), QfcommitOutcome::Misbehaving(100));
    let mut weak = qc.clone();
    weak.signers = bits(3, &[0]);
    assert_eq!(run(&f, &mut pool, 60, NEW, &weak), QfcommitOutcome::Misbehaving(100));
    assert!(!pool.has(&commitment_hash(&bad)));
    let three = f.signed(TEST, 48, &[0, 1, 2]);
    assert_eq!(run(&f, &mut pool, 60, NEW, &three), QfcommitOutcome::Accepted(Some(h3)));
    // Fewer signers than the pool's: Core's comparison lets it through to `Verify` and
    // `AddMineableCommitment`, which does not relay it.
    assert_eq!(run(&f, &mut pool, 60, NEW, &qc), QfcommitOutcome::Accepted(None));
    assert!(!pool.has(&commitment_hash(&qc)));
    assert_eq!(run(&f, &mut pool, 60, NEW, &weak), QfcommitOutcome::Misbehaving(100));
    // Already mined (:173-178), checked before the pool and after staleness.
    f.mine(58, std::slice::from_ref(&qc));
    let mut pool = MineablePool::new();
    assert_eq!(run(&f, &mut pool, 60, NEW, &three), QfcommitOutcome::Ignored);
    assert_eq!(run(&f, &mut pool, 60, NEW, &bad), QfcommitOutcome::Ignored);
    assert_eq!(run(&f, &mut pool, 73, NEW, &three), QfcommitOutcome::Misbehaving(100));
    // Through the tip: mined at 58, not yet at 57.
    assert_eq!(run(&f, &mut pool, 57, NEW, &three), QfcommitOutcome::Accepted(Some(h3)));
}

// ---- 12. CheckLLMQCommitment ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    Null,
    Verify(BlockId),
}

/// A verifier with fixed answers that records its calls; `verify` emits a members read.
struct Fake {
    null: bool,
    verify: Result<bool, QcBlockError>,
    calls: RefCell<Vec<Call>>,
}

impl Fake {
    fn new(null: bool, verify: Result<bool, QcBlockError>) -> Self {
        Fake {
            null,
            verify,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }
}

impl CommitmentVerifier for Fake {
    fn verify_null(&self, _: &LlmqCommitment) -> bool {
        self.calls.borrow_mut().push(Call::Null);
        self.null
    }

    fn verify(
        &self,
        qc: &LlmqCommitment,
        base: BlockId,
        effects: &mut dyn EffectSink,
    ) -> Result<bool, QcBlockError> {
        self.calls.borrow_mut().push(Call::Verify(base));
        effects.emit(members_read(qc.llmq_type, base));
        self.verify.clone()
    }
}

/// `CheckLLMQCommitment` (`llmq/commitment.cpp` v24:240-310) steps 1-5: the payload, the type,
/// the payload version, the height and the quorum hash, in this order, none calling the
/// verifier.
#[test]
fn check_llmq_commitment_steps_in_order() {
    let f = Fixture::new();
    let qc = f.test_qc();
    let accept = Fake::new(true, Ok(true));
    let check = |payload| f.check(payload, 57, &accept).0;
    assert_eq!(check(vec![1]), tx_invalid(BAD_QC_PAYLOAD));
    let mut trailing = qc_payload(58, &qc);
    trailing.push(0);
    assert_eq!(check(trailing), tx_invalid(BAD_QC_PAYLOAD));
    let mut unconfigured = qc.clone();
    unconfigured.llmq_type = UNCONFIGURED;
    unconfigured.quorum_hash = QuorumHash::from([0x5a; 32]);
    assert_eq!(
        check(qc_payload_versioned(0, 0, &unconfigured)),
        tx_invalid(BAD_QC_COMMITMENT_TYPE)
    );
    // The payload version is 1 (`CURRENT_VERSION`); it is checked before the height.
    assert_eq!(check(qc_payload_versioned(0, 58, &qc)), tx_invalid(BAD_QC_VERSION));
    assert_eq!(check(qc_payload_versioned(2, 58, &qc)), tx_invalid(BAD_QC_VERSION));
    assert_eq!(check(qc_payload_versioned(0, 0, &qc)), tx_invalid(BAD_QC_VERSION));
    let mut unknown = qc.clone();
    unknown.quorum_hash = QuorumHash::from([0x5a; 32]);
    assert_eq!(check(qc_payload_versioned(2, 0, &unknown)), tx_invalid(BAD_QC_VERSION));
    // The height is `prev + 1`, before the quorum hash.
    assert_eq!(check(qc_payload(57, &qc)), tx_invalid(BAD_QC_HEIGHT));
    assert_eq!(check(qc_payload(59, &qc)), tx_invalid(BAD_QC_HEIGHT));
    assert_eq!(check(qc_payload(0, &unknown)), tx_invalid(BAD_QC_HEIGHT));
    // The quorum hash: unknown, off `prev`'s chain, or above `prev`.
    assert_eq!(check(qc_payload(58, &unknown)), tx_invalid(BAD_QC_QUORUM_HASH));
    let mut fork = qc.clone();
    fork.quorum_hash = f.hash(FORK_48);
    assert_eq!(check(qc_payload(58, &fork)), tx_invalid(BAD_QC_QUORUM_HASH));
    let mut above = qc.clone();
    above.quorum_hash = f.qhash(58);
    assert_eq!(check(qc_payload(58, &above)), tx_invalid(BAD_QC_QUORUM_HASH));
    assert!(accept.calls().is_empty());
    // `prev` itself is on its chain, and so is a fork block for a check on the fork.
    let mut at_prev = qc.clone();
    at_prev.quorum_hash = f.qhash(57);
    assert_eq!(check(qc_payload(58, &at_prev)), Ok(Ok(())));
    assert_eq!(accept.calls(), vec![Call::Verify(BlockId(57))]);
    assert_eq!(f.check(qc_payload(50, &fork), FORK_49.0, &accept).0, Ok(Ok(())));
    assert_eq!(f.check(qc_payload(50, &qc), FORK_49.0, &accept).0, tx_invalid(BAD_QC_QUORUM_HASH));
}

/// The height check is `qcTx.nHeight != uint32_t(pindexPrev->nHeight + 1)` (v24:275): the
/// sum wraps.
#[test]
fn check_llmq_commitment_height_wraps() {
    let f = Fixture::new();
    let chain = Tall {
        chain: &f.chain,
        tall: BlockId(57),
    };
    let llmq = LlmqCtx::new(&f.params, &chain, &f.deps, BlsCtx::BASIC).expect("in domain");
    let accept = Fake::new(true, Ok(true));
    let check = |height| {
        let tx = qc_tx(qc_payload(height, &f.null(TEST, 48)));
        check_llmq_commitment(&tx, BlockId(57), &llmq, &f.index, &accept, &mut Vec::new())
    };
    assert_eq!(check(0), Ok(Ok(())));
    assert_eq!(check(u32::MAX), tx_invalid(BAD_QC_HEIGHT));
    assert_eq!(check(58), tx_invalid(BAD_QC_HEIGHT));
}

/// Steps 6 and 7 (v24:292-305): a null commitment goes to `VerifyNull` only, any other to
/// `Verify(base, false)` with the looked-up base; a verifier fault is the outer error.
#[test]
fn check_llmq_commitment_verifier_calls() {
    let f = Fixture::new();
    let (qc, null) = (qc_payload(58, &f.test_qc()), qc_payload(58, &f.null(TEST, 48)));
    for (null_ok, expected) in [(true, Ok(Ok(()))), (false, tx_invalid(BAD_QC_INVALID_NULL))] {
        let fake = Fake::new(null_ok, Ok(true));
        let (r, effects) = f.check(null.clone(), 57, &fake);
        assert_eq!(r, expected);
        assert_eq!((fake.calls(), effects), (vec![Call::Null], vec![]));
    }
    for (verify, expected) in [
        (Ok(true), Ok(Ok(()))),
        (Ok(false), tx_invalid(BAD_QC_INVALID)),
        (Err(QcBlockError::BlsUnsupported), Err(QcBlockError::BlsUnsupported)),
    ] {
        let fake = Fake::new(false, verify);
        let (r, effects) = f.check(qc.clone(), 57, &fake);
        assert_eq!(r, expected);
        assert_eq!(fake.calls(), vec![Call::Verify(BlockId(48))]);
        assert_eq!(
            effects,
            vec![members_read(TEST, BlockId(48))],
            "the verifier's effects pass through"
        );
    }
}

/// The node's verifier: `VerifyNull`, and `Verify(base, checkSigs = false)` over the members.
#[test]
fn check_llmq_commitment_with_the_node_verifier() {
    let f = Fixture::new();
    assert_eq!(f.node_check(qc_payload(58, &f.test_qc()), 57), Ok(Ok(())));
    assert_eq!(f.node_check(qc_payload(58, &f.null(TEST, 48)), 57), Ok(Ok(())));
    let mut bad_sig = f.test_qc();
    sign(&mut bad_sig, &[1, 3]);
    assert_eq!(f.node_check(qc_payload(58, &bad_sig), 57), Ok(Ok(())), "no signature is checked");
    let mut version = f.test_qc();
    version.version = 4;
    version.quorum_index = Some(0);
    assert_eq!(f.node_check(qc_payload(58, &version), 57), tx_invalid(BAD_QC_INVALID));
    let mut long = f.null(TEST, 48);
    long.signers = bits(4, &[]);
    assert_eq!(f.node_check(qc_payload(58, &long), 57), tx_invalid(BAD_QC_INVALID_NULL));
    // Any height outside the mining window: the transaction check does not read it.
    assert_eq!(f.node_check(qc_payload(51, &f.test_qc()), 50), Ok(Ok(())));
}

/// A verdict carries its reason; a reasonless failure, a BLS gap and a fault carry none.
#[test]
fn qc_block_error_reason_and_display() {
    let e = QcBlockError::invalid(BAD_QC_INVALID);
    assert_eq!(e.reject_reason(), Some(BAD_QC_INVALID));
    assert!(e.to_string().contains(BAD_QC_INVALID), "{e}");
    let no = QcBlockError::NoReason("no known block index for a quorum hash");
    assert_eq!(no.reject_reason(), None);
    assert_eq!(
        no.to_string(),
        "quorum block processing failed: no known block index for a quorum hash"
    );
    assert_eq!(QcBlockError::BlsUnsupported.reject_reason(), None);
    assert!(!QcBlockError::BlsUnsupported.to_string().is_empty());
}

// ---- 9. readiness refusals (CH7 D2) ----

/// The capability the node's stub sources name (CH7 D2).
const NOT_BUILT: &str = "quorum validation (CH7b)";

/// A members source or precompute that cannot answer yet: [`StoreError::NotBuilt`].
struct NotBuilt;

impl QuorumMemberSource for NotBuilt {
    type Entry = Arc<Dmn>;
    type Error = StoreError;

    fn members(
        &self,
        _: LlmqType,
        _: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        Err(MemberError::View(StoreError::NotBuilt(NOT_BUILT)))
    }
}

impl QuorumPrecompute for NotBuilt {
    type Error = StoreError;

    fn precompute(
        &self,
        _: BlockId,
        _: PrecomputeMode,
        _: &mut dyn EffectSink,
    ) -> Result<(), MemberError<StoreError>> {
        Err(MemberError::View(StoreError::NotBuilt(NOT_BUILT)))
    }
}

/// CH7 D2: a [`StoreError::NotBuilt`] from the members source or the precompute comes back
/// unchanged, as a fault naming its capability: never a verdict, never `NoReason`.
#[test]
fn a_not_built_source_comes_back_unchanged() {
    let f = Fixture::new();
    let not_built =
        Err(QcBlockError::Fault(NodeMemberError::Store(StoreError::NotBuilt(NOT_BUILT))));
    let (qc, null_platform) = (f.test_qc(), f.null(PLATFORM, 48));
    let block = qc_block(58, &[qc, null_platform]);
    let at = QcBlockAt {
        id: BlockId(58),
        height: 58,
    };
    // The members of a non-null commitment (`ProcessCommitment`'s `Verify`), in every mode.
    for mode in MODES {
        let mut effects = Vec::new();
        let r = f.with_sources(&NotBuilt, &f.precompute, &SerialBlsChecks, |ctx| {
            process_block(ctx, &block, at, Some(BlockId(57)), mode, &mut effects)
        });
        assert_eq!(r, not_built, "{mode:?}");
        let Err(QcBlockError::Fault(e)) = r else {
            unreachable!()
        };
        assert_eq!(e.capability(), Some(NOT_BUILT));
    }
    // The precompute, before anything else is read, on connect and on undo.
    let mut effects = Vec::new();
    let r = f.with_sources(&f.roster, &NotBuilt, &SerialBlsChecks, |ctx| {
        process_block(ctx, &block, at, Some(BlockId(57)), WRITE, &mut effects)
    });
    assert_eq!((r, effects), (not_built.clone(), vec![]));
    let mut effects = Vec::new();
    let r = f.with_sources(&f.roster, &NotBuilt, &SerialBlsChecks, |ctx| {
        undo_block(ctx, &block, BlockId(58), &mut effects)
    });
    assert_eq!((r.map(|()| vec![]), effects), (not_built, vec![]));
}
