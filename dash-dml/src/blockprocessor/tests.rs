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
use std::convert::Infallible;
use std::sync::Arc;

use dash_consensus::ctx::{Deployment, DeploymentView, bls_ctx_for_block};
use dash_consensus::primitives::{
    Block, Tx, core_serialize, decode_block, write_commitment, write_compact_size,
};
use dash_consensus::{
    BlockId, ChainAncestry, ConsensusCtx, HeaderCtx, TxValidationError, TxValidationResult,
};
use dash_evo::bls::BlsCtx;
use dash_evo::ctx::EvoCtx;
use dash_evo::effect::{Effect, EffectSink};
use dash_evo::llmq::{
    BlsCheck, BlsCheckRunner, BlsVerdict, CommitmentReject, CompleteChain, LlmqChain,
    MinedCommitment, MinedFamily, MinedIndexEntry, QuorumMemberSource, QuorumMembers,
    SerialBlsChecks, build_commitment_hash, commitment_version,
};
use dash_evo::sml::merkle_root_quorums_from_hashes;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
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
use crate::netinfo::NetInfoRules;
use crate::specialtx::qc::{CommitmentVerifier, NodeCommitmentVerifier, check_llmq_commitment};
use crate::specialtx::quorum::TestQuorums;
use crate::specialtx::reasons::{
    BAD_QC_QUORUM_HASH, BAD_QC_VERSION, BAD_TX_TYPE_DIP3_INACTIVE, FAILED_CHECK_SPECIAL_TX,
};
use crate::specialtx::sig::SerialVerifier;
use crate::specialtx::testctx::{ModelCoins, NoCheckpoints};
use crate::specialtx::{
    CheckResult, NULL_PREV, QuorumSeams, SpecialTxCtx, SpecialTxFault, SpecialTxRules,
    check_special_tx_inner,
};
use crate::store::{EvoDelta, MemEvoStore, MinedErase};
use crate::testkit::replay::{CORE_NET_INFO, mainnet};
use crate::testkit::{MemChain, dmn_with, operator_key, pro_tx};
use crate::{Dmn, MnList, MnType};

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
    DynBitset::new(size as u64, data).expect("a bitset of `size` bits")
}

/// Signs `qc` as it stands, in the basic scheme: `membersSig` the secure aggregate of the
/// operator keys of `seeds`, `quorumSig` the quorum key's.
fn sign(qc: &mut LlmqCommitment, seeds: &[u8]) {
    let hash = build_commitment_hash(
        qc.llmq_type(),
        &qc.quorum_hash(),
        qc.valid_members(),
        qc.quorum_public_key(),
        qc.quorum_vvec_hash().as_bytes(),
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
    *qc = dash_primitives::Commitment::new(
        qc.version(),
        qc.llmq_type(),
        qc.quorum_hash(),
        qc.quorum_index(),
        qc.signers().clone(),
        qc.valid_members().clone(),
        qc.quorum_public_key().clone(),
        qc.quorum_vvec_hash(),
        qc.quorum_sig().clone(),
        BlsEagerSig::new(sig_value(&members_sig), BlsSchemeKind::Basic),
    );
    let quorum_sig = secret(QUORUM_SEED).sign_with(&hash, BlsSigId::Basic);
    *qc = dash_primitives::Commitment::new(
        qc.version(),
        qc.llmq_type(),
        qc.quorum_hash(),
        qc.quorum_index(),
        qc.signers().clone(),
        qc.valid_members().clone(),
        qc.quorum_public_key().clone(),
        qc.quorum_vvec_hash(),
        BlsEagerSig::new(sig_value(&quorum_sig), BlsSchemeKind::Basic),
        qc.members_sig().clone(),
    );
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
    core_serialize(&Transaction::new(version, tx_type, vec![], vec![], 0, extra_payload))
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

/// A recording [`QcTrace`]: each line as `(variant, Core's text)`, in order.
#[derive(Default)]
struct Lines(RefCell<Vec<(String, String)>>);

impl QcTrace for Lines {
    fn event(&self, event: QcEvent<'_>) {
        let debug = format!("{event:?}");
        let name = debug.split([' ', '{']).next().unwrap_or_default().to_owned();
        self.0.borrow_mut().push((name, event.core_line()));
    }
}

impl Lines {
    /// The variants and the texts recorded since the last call, and clears them.
    fn take(&self) -> (Vec<String>, Vec<String>) {
        self.0.take().into_iter().unzip()
    }
}

struct Fixture {
    params: ChainSetup,
    chain: MemChain,
    index: Index,
    deps: Heights,
    store: MemEvoStore,
    roster: Roster,
    precompute: Precompute,
    trace: Lines,
    bls: BlsCtx,
}

impl Fixture {
    /// Regtest v24, the main chain `0 ..= TIP` and the fork at 48, DIP3, V19 and DIP0024 from
    /// height 1, an empty store.
    fn new() -> Self {
        Self::with_deployments(1, 1, 1)
    }

    fn with_deployments(dip3: u32, v19: u32, dip24: u32) -> Self {
        let params =
            ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest");
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
            trace: Lines::default(),
            bls: BlsCtx::BASIC,
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
        self.with_view(&self.chain, &self.trace, members, precompute, bls_checks, f)
    }

    /// [`Fixture::with_sources`], the LLMQ context reading `chain` and the lines going to
    /// `trace`.
    fn with_view<R>(
        &self,
        chain: &dyn ChainAncestry,
        trace: &dyn QcTrace,
        members: &DmlMemberSource<'_>,
        precompute: &dyn QuorumPrecompute<Error = StoreError>,
        bls_checks: &dyn BlsCheckRunner,
        f: impl FnOnce(&QcCtx<'_>) -> R,
    ) -> R {
        let chain = CompleteChain::<StoreError>::new(chain);
        let llmq = LlmqCtx::new(&self.params, &chain, &self.deps, self.bls).expect("in domain");
        let mined = self.store.view(&self.chain);
        f(&QcCtx {
            llmq,
            index: &self.index,
            mined: &mined,
            members,
            precompute,
            bls_checks,
            trace,
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
        let size = usize::from(self.llmq(llmq_type).size);
        let version = self.version(llmq_type, BlockId(base));
        let mut qc = LlmqCommitment::new(
            version,
            llmq_type,
            self.qhash(base),
            (version == 2 || version == 4).then_some((base % 24) as i16),
            bits(size, signers),
            bits(size, &(0..size).collect::<Vec<_>>()),
            BlsEagerPk::new(pk_value(&secret(QUORUM_SEED).public_key()), BlsSchemeKind::Basic),
            QuorumVvecHash::from([0x11; 32]),
            BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic),
            BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic),
        );
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
        null_commitment(
            llmq_type,
            self.llmq(llmq_type),
            self.qhash(base),
            version,
            (base % 24) as i16,
        )
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
                    let params = self.llmq(qc.llmq_type());
                    let (base, base_height) =
                        ctx.index.lookup(&as_block_hash(&qc.quorum_hash())).expect("a known base");
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
                trace: ctx.trace,
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
        llmq_type: qc.llmq_type(),
        quorum_hash: qc.quorum_hash(),
        base_height,
        rotation_index,
        commitment: Arc::new(qc.clone()),
    }
}

fn erase(qc: &LlmqCommitment) -> Effect {
    Effect::EraseMineableCommitment {
        llmq_type: qc.llmq_type(),
        quorum_hash: qc.quorum_hash(),
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
        f.with(|ctx| {
            num_commitments_required(ctx, llmq_type, f.llmq(llmq_type), BlockId(tip), height)
        })
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
    bad = dash_primitives::Commitment::new(
        bad.version(),
        bad.llmq_type(),
        f.qhash(49),
        bad.quorum_index(),
        bad.signers().clone(),
        bad.valid_members().clone(),
        bad.quorum_public_key().clone(),
        bad.quorum_vvec_hash(),
        bad.quorum_sig().clone(),
        bad.members_sig().clone(),
    );
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
    zero = dash_primitives::Commitment::new(
        zero.version(),
        zero.llmq_type(),
        QuorumHash::from([0; 32]),
        zero.quorum_index(),
        zero.signers().clone(),
        zero.valid_members().clone(),
        zero.quorum_public_key().clone(),
        zero.quorum_vvec_hash(),
        zero.quorum_sig().clone(),
        zero.members_sig().clone(),
    );
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
    other = dash_primitives::Commitment::new(
        other.version(),
        other.llmq_type(),
        other.quorum_hash(),
        None,
        other.signers().clone(),
        other.valid_members().clone(),
        other.quorum_public_key().clone(),
        other.quorum_vvec_hash(),
        other.quorum_sig().clone(),
        other.members_sig().clone(),
    );
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
    weak = dash_primitives::Commitment::new(
        weak.version(),
        weak.llmq_type(),
        weak.quorum_hash(),
        weak.quorum_index(),
        bits(3, &[0]),
        weak.valid_members().clone(),
        weak.quorum_public_key().clone(),
        weak.quorum_vvec_hash(),
        weak.quorum_sig().clone(),
        weak.members_sig().clone(),
    );
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
    unconfigured = dash_primitives::Commitment::new(
        unconfigured.version(),
        UNCONFIGURED,
        unconfigured.quorum_hash(),
        unconfigured.quorum_index(),
        unconfigured.signers().clone(),
        unconfigured.valid_members().clone(),
        unconfigured.quorum_public_key().clone(),
        unconfigured.quorum_vvec_hash(),
        unconfigured.quorum_sig().clone(),
        unconfigured.members_sig().clone(),
    );
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
    wrong = dash_primitives::Commitment::new(
        wrong.version(),
        wrong.llmq_type(),
        f.qhash(49),
        wrong.quorum_index(),
        wrong.signers().clone(),
        wrong.valid_members().clone(),
        wrong.quorum_public_key().clone(),
        wrong.quorum_vvec_hash(),
        wrong.quorum_sig().clone(),
        wrong.members_sig().clone(),
    );
    assert_eq!(f.connect(58, &[wrong, platform.clone()], WRITE).0, invalid(BAD_QC_BLOCK));
    let mut fork = f.test_qc();
    fork = dash_primitives::Commitment::new(
        fork.version(),
        fork.llmq_type(),
        f.hash(FORK_48),
        fork.quorum_index(),
        fork.signers().clone(),
        fork.valid_members().clone(),
        fork.quorum_public_key().clone(),
        fork.quorum_vvec_hash(),
        fork.quorum_sig().clone(),
        fork.members_sig().clone(),
    );
    assert_eq!(f.connect(58, &[fork, platform.clone()], WRITE).0, invalid(BAD_QC_BLOCK));
    let mut null = f.null(TEST, 48);
    null = dash_primitives::Commitment::new(
        null.version(),
        null.llmq_type(),
        f.qhash(47),
        null.quorum_index(),
        null.signers().clone(),
        null.valid_members().clone(),
        null.quorum_public_key().clone(),
        null.quorum_vvec_hash(),
        null.quorum_sig().clone(),
        null.members_sig().clone(),
    );
    assert_eq!(f.connect(58, &[null, platform.clone()], WRITE).0, invalid(BAD_QC_BLOCK));
    // A rotated commitment's index selects the block.
    let (test, null_test) = (f.test_qc(), f.null(TEST, 48));
    let mut swapped = f.rot_qc(1);
    swapped = dash_primitives::Commitment::new(
        swapped.version(),
        swapped.llmq_type(),
        swapped.quorum_hash(),
        Some(0),
        swapped.signers().clone(),
        swapped.valid_members().clone(),
        swapped.quorum_public_key().clone(),
        swapped.quorum_vvec_hash(),
        swapped.quorum_sig().clone(),
        swapped.members_sig().clone(),
    );
    let qcs = [null_test.clone(), platform.clone(), f.rot_qc(0), swapped];
    assert_eq!(f.connect(60, &qcs, WRITE).0, invalid(BAD_QC_BLOCK));
    // An index above the tip: Core's null hash.
    let mut far = f.rot_qc(1);
    far = dash_primitives::Commitment::new(
        far.version(),
        far.llmq_type(),
        far.quorum_hash(),
        Some(20),
        far.signers().clone(),
        far.valid_members().clone(),
        far.quorum_public_key().clone(),
        far.quorum_vvec_hash(),
        far.quorum_sig().clone(),
        far.members_sig().clone(),
    );
    let qcs = [null_test, platform.clone(), f.rot_qc(0), far];
    assert_eq!(f.connect(60, &qcs, WRITE).0, invalid(BAD_QC_BLOCK));
    // The checks of the second commitment run after the first's row.
    let mut wrong_platform = platform;
    wrong_platform = dash_primitives::Commitment::new(
        wrong_platform.version(),
        wrong_platform.llmq_type(),
        f.qhash(49),
        wrong_platform.quorum_index(),
        wrong_platform.signers().clone(),
        wrong_platform.valid_members().clone(),
        wrong_platform.quorum_public_key().clone(),
        wrong_platform.quorum_vvec_hash(),
        wrong_platform.quorum_sig().clone(),
        wrong_platform.members_sig().clone(),
    );
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
    long = dash_primitives::Commitment::new(
        long.version(),
        long.llmq_type(),
        long.quorum_hash(),
        long.quorum_index(),
        bits(4, &[]),
        long.valid_members().clone(),
        long.quorum_public_key().clone(),
        long.quorum_vvec_hash(),
        long.quorum_sig().clone(),
        long.members_sig().clone(),
    );
    assert!(long.is_null());
    assert_eq!(f.connect(58, &[long, platform.clone()], WRITE).0, invalid(BAD_QC_INVALID_NULL));
    let mut short = f.null(TEST, 48);
    short = dash_primitives::Commitment::new(
        short.version(),
        short.llmq_type(),
        short.quorum_hash(),
        short.quorum_index(),
        short.signers().clone(),
        bits(2, &[]),
        short.quorum_public_key().clone(),
        short.quorum_vvec_hash(),
        short.quorum_sig().clone(),
        short.members_sig().clone(),
    );
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
    weak = dash_primitives::Commitment::new(
        weak.version(),
        weak.llmq_type(),
        weak.quorum_hash(),
        weak.quorum_index(),
        bits(3, &[0]),
        weak.valid_members().clone(),
        weak.quorum_public_key().clone(),
        weak.quorum_vvec_hash(),
        weak.quorum_sig().clone(),
        weak.members_sig().clone(),
    );
    assert_eq!(f.connect(58, &[weak, platform.clone()], WRITE).0, invalid(BAD_QC_INVALID));
    let mut version = f.test_qc();
    version.set_version(1);
    assert_eq!(f.connect(58, &[version, platform.clone()], WRITE).0, invalid(BAD_QC_INVALID));
    let mut no_key = f.test_qc();
    no_key = dash_primitives::Commitment::new(
        no_key.version(),
        no_key.llmq_type(),
        no_key.quorum_hash(),
        no_key.quorum_index(),
        no_key.signers().clone(),
        no_key.valid_members().clone(),
        BlsEagerPk::new(BlsPkValue::invalid(), BlsSchemeKind::Basic),
        no_key.quorum_vvec_hash(),
        no_key.quorum_sig().clone(),
        no_key.members_sig().clone(),
    );
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
    bad_quorum = dash_primitives::Commitment::new(
        bad_quorum.version(),
        bad_quorum.llmq_type(),
        bad_quorum.quorum_hash(),
        bad_quorum.quorum_index(),
        bad_quorum.signers().clone(),
        bad_quorum.valid_members().clone(),
        bad_quorum.quorum_public_key().clone(),
        bad_quorum.quorum_vvec_hash(),
        bad_members.members_sig().clone(),
        bad_quorum.members_sig().clone(),
    );
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
    unknown = dash_primitives::Commitment::new(
        unknown.version(),
        unknown.llmq_type(),
        QuorumHash::from([0x5a; 32]),
        unknown.quorum_index(),
        unknown.signers().clone(),
        unknown.valid_members().clone(),
        unknown.quorum_public_key().clone(),
        unknown.quorum_vvec_hash(),
        unknown.quorum_sig().clone(),
        unknown.members_sig().clone(),
    );
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
    unknown = dash_primitives::Commitment::new(
        unknown.version(),
        unknown.llmq_type(),
        QuorumHash::from([0x5a; 32]),
        unknown.quorum_index(),
        unknown.signers().clone(),
        unknown.valid_members().clone(),
        unknown.quorum_public_key().clone(),
        unknown.quorum_vvec_hash(),
        unknown.quorum_sig().clone(),
        unknown.members_sig().clone(),
    );
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
    bad_quorum = dash_primitives::Commitment::new(
        bad_quorum.version(),
        bad_quorum.llmq_type(),
        bad_quorum.quorum_hash(),
        bad_quorum.quorum_index(),
        bad_quorum.signers().clone(),
        bad_quorum.valid_members().clone(),
        bad_quorum.quorum_public_key().clone(),
        bad_quorum.quorum_vvec_hash(),
        rot0.members_sig().clone(),
        bad_quorum.members_sig().clone(),
    );
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
    unknown = dash_primitives::Commitment::new(
        unknown.version(),
        unknown.llmq_type(),
        QuorumHash::from([0x5a; 32]),
        unknown.quorum_index(),
        unknown.signers().clone(),
        unknown.valid_members().clone(),
        unknown.quorum_public_key().clone(),
        unknown.quorum_vvec_hash(),
        unknown.quorum_sig().clone(),
        unknown.members_sig().clone(),
    );
    let qcs = [unknown, platform, rot0, qcs[3].clone()];
    let recording = RecordingReversed::default();
    assert_eq!(
        f.connect_with(&recording, 60, &qcs, WRITE_BLS).0,
        Err(QcBlockError::NoReason("no known block index for a quorum hash"))
    );
    assert!(recording.0.take().is_empty());
}

// ---- 8. undo ----

/// `UndoBlock` (v24:519-562): the precompute in undo mode, then per non-null commitment in
/// `GetCommitmentsFromBlock`'s order the tombstone of its mined rows (`q_mc` and the height-index
/// row at the undone block's height, in the indexed family where rotation is enabled at it) and a
/// `ReinsertMineableCommitment`; a null commitment erases nothing. A block whose commitments Core
/// cannot read is its reasonless `false`.
#[test]
fn undo_erases_and_reinserts_the_non_null_commitments() {
    let f = Fixture::new();
    let (test, rot0, rot1) = (f.test_qc(), f.rot_qc(0), f.rot_qc(1));
    let block = qc_block(
        60,
        &[f.null(PLATFORM, 48), rot1.clone(), test.clone(), f.null(ROT, 50), rot0.clone()],
    );
    let mut effects = Vec::new();
    let r = f.with(|ctx| undo_block(ctx, &block, BlockId(60), &mut effects));
    let erase = |qc: &LlmqCommitment, rotation_index| MinedErase {
        llmq_type: qc.llmq_type(),
        quorum_hash: qc.quorum_hash(),
        mined_height: 60,
        rotation_index,
    };
    assert_eq!(r, Ok(vec![erase(&test, None), erase(&rot1, Some(1)), erase(&rot0, Some(0))]));
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
    assert_eq!((r, effects), (Ok(vec![]), vec![precomputed(BlockId(57), PrecomputeMode::Undo)]));
    // The family is read at the undone block: with DIP0024 from 60 the cycle at 48 is plain and
    // the cycle at 72 indexed.
    let f = Fixture::with_deployments(1, 1, 60);
    let (plain, indexed) = (f.rot_qc(0), f.signed(ROT, 73, &[0, 1, 2, 3]));
    for (qc, height, rotation_index) in [(&plain, 58, None), (&indexed, 84, Some(1))] {
        let block = qc_block(height, std::slice::from_ref(qc));
        let mut effects = Vec::new();
        let r = f.with(|ctx| undo_block(ctx, &block, BlockId(height), &mut effects));
        let erased = MinedErase {
            llmq_type: ROT,
            quorum_hash: qc.quorum_hash(),
            mined_height: height,
            rotation_index,
        };
        assert_eq!(r, Ok(vec![erased]), "{height}");
    }
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
                .map(|params| params.iter().map(|&(t, _)| t).collect())
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
    other = dash_primitives::Commitment::new(
        other.version(),
        other.llmq_type(),
        f.qhash(49),
        other.quorum_index(),
        other.signers().clone(),
        other.valid_members().clone(),
        other.quorum_public_key().clone(),
        other.quorum_vvec_hash(),
        other.quorum_sig().clone(),
        other.members_sig().clone(),
    );
    assert_eq!(f.connect(50, std::slice::from_ref(&other), WRITE).0, invalid(BAD_QC_BLOCK));
    let mut long = null.clone();
    long = dash_primitives::Commitment::new(
        long.version(),
        long.llmq_type(),
        long.quorum_hash(),
        long.quorum_index(),
        long.signers().clone(),
        bits(4, &[]),
        long.quorum_public_key().clone(),
        long.quorum_vvec_hash(),
        long.quorum_sig().clone(),
        long.members_sig().clone(),
    );
    assert_eq!(f.connect(50, std::slice::from_ref(&long), WRITE).0, invalid(BAD_QC_INVALID_NULL));
    assert_eq!(f.node_check(qc_payload(50, &long), 49), tx_invalid(BAD_QC_INVALID_NULL));
    assert_eq!(f.node_check(qc_payload(51, &null), 49), tx_invalid(BAD_QC_HEIGHT));

    // signers >= minSize (2 for LLMQ_TEST), valid members too.
    let platform = f.null(PLATFORM, 48);
    let mut one = f.test_qc();
    one = dash_primitives::Commitment::new(
        one.version(),
        one.llmq_type(),
        one.quorum_hash(),
        one.quorum_index(),
        bits(3, &[1]),
        one.valid_members().clone(),
        one.quorum_public_key().clone(),
        one.quorum_vvec_hash(),
        one.quorum_sig().clone(),
        one.members_sig().clone(),
    );
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
    few_valid = dash_primitives::Commitment::new(
        few_valid.version(),
        few_valid.llmq_type(),
        few_valid.quorum_hash(),
        few_valid.quorum_index(),
        few_valid.signers().clone(),
        bits(3, &[0]),
        few_valid.quorum_public_key().clone(),
        few_valid.quorum_vvec_hash(),
        few_valid.quorum_sig().clone(),
        few_valid.members_sig().clone(),
    );
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
    assert_eq!(from.map(|qcs| qcs[0].signers().count_ones()), Ok(3));
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
    assert_eq!(pool.best_for(TEST, &two.quorum_hash()).map(|qc| commitment_hash(qc)), Some(h3));
    assert!(pool.has(&h2) && pool.has(&h3), "Core's leak keeps the old one");
    assert_eq!(pool.get(&h2).map(|qc| (**qc).clone()), Some(two.clone()));
    assert_eq!(pool.add(&two), None, "fewer signers");
    assert_eq!(pool.add(&three), None);
    // Another quorum or type has its own entry.
    let platform = f.signed(PLATFORM, 48, &[0, 1]);
    assert_eq!(pool.add(&platform), Some(commitment_hash(&platform)));
    assert_eq!(pool.best_for(TEST, &two.quorum_hash()).map(|qc| commitment_hash(qc)), Some(h3));
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
    assert!(pool.best_for(TEST, &two.quorum_hash()).is_none());
    assert!(!pool.has(&h2) && pool.has(&h3));
    // An erasure whose hash the pool does not hold removes the entry only.
    let mut pool = MineablePool::new();
    pool.add(&two);
    pool.apply(&Effect::EraseMineableCommitment {
        llmq_type: TEST,
        quorum_hash: two.quorum_hash(),
        commitment_hash: [0; 32],
    });
    assert!(pool.best_for(TEST, &two.quorum_hash()).is_none());
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
    assert!(pool.best_for(TEST, &two.quorum_hash()).is_none(), "the entry names a removed hash");
    // The type is part of the key.
    let mut pool = MineablePool::new();
    pool.add(&two);
    pool.apply(&Effect::EraseMineableCommitment {
        llmq_type: PLATFORM,
        quorum_hash: two.quorum_hash(),
        commitment_hash: [0; 32],
    });
    assert_eq!(pool.best_for(TEST, &two.quorum_hash()).map(|qc| commitment_hash(qc)), Some(h2));
    // Re-insertion is `AddMineableCommitment`.
    let mut pool = MineablePool::new();
    pool.apply(&Effect::ReinsertMineableCommitment {
        commitment: Arc::new(two.clone()),
    });
    assert_eq!(pool.best_for(TEST, &two.quorum_hash()).map(|qc| commitment_hash(qc)), Some(h2));
    pool.apply(&Effect::ReinsertMineableCommitment {
        commitment: Arc::new(three.clone()),
    });
    assert_eq!(pool.best_for(TEST, &two.quorum_hash()).map(|qc| commitment_hash(qc)), Some(h3));
    // Other effects leave it unchanged.
    for other in [precomputed(BlockId(1), PrecomputeMode::Connect), members_read(TEST, BlockId(48))]
    {
        pool.apply(&other);
    }
    assert!(pool.has(&h2) && pool.has(&h3));
    assert_eq!(pool.best_for(TEST, &two.quorum_hash()).map(|qc| commitment_hash(qc)), Some(h3));
}

/// `GetMineableCommitments` (v24:880-936): `None` when no commitment is required; else per
/// quorum of the cycle, unless mined, the pool's commitment or a null one with the index and the
/// version for rotation and V19 at `pindex`.
#[test]
fn mineable_commitments_pool_or_null() {
    let mut f = Fixture::new();
    let mineable = |f: &Fixture, pool: &MineablePool, llmq_type, tip: u32, height| {
        f.with(|ctx| {
            pool.mineable_commitments(ctx, llmq_type, f.llmq(llmq_type), BlockId(tip), height)
        })
        .expect("complete")
    };
    let mut pool = MineablePool::new();
    assert_eq!(mineable(&f, &pool, TEST, 56, 57), None);
    let null = mineable(&f, &pool, TEST, 57, 58).expect("required");
    assert_eq!(null, vec![f.null(TEST, 48)]);
    assert_eq!(
        (null[0].version(), null[0].quorum_index(), null[0].signers().num_bits()),
        (3, None, 3)
    );
    assert!(null[0].is_null());
    let rot = mineable(&f, &pool, ROT, 59, 60).expect("required");
    assert_eq!(rot, vec![f.null(ROT, 48), f.null(ROT, 49)]);
    assert_eq!(
        (rot[1].version(), rot[1].quorum_index(), rot[1].valid_members().num_bits()),
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
    assert_eq!((legacy[0].version(), legacy[0].quorum_index()), (1, None));
    let legacy = mineable(&f, &empty, ROT, 59, 60).expect("required");
    assert_eq!(
        legacy.iter().map(|qc| (qc.version(), qc.quorum_index())).collect::<Vec<_>>(),
        vec![(2, Some(0)), (2, Some(1))]
    );
    // Rotation off at `pindex` (DIP0024 from 60): one plain-version quorum.
    let f = Fixture::with_deployments(1, 1, 60);
    let plain = mineable(&f, &empty, ROT, 59, 60).expect("required");
    assert_eq!(
        plain.iter().map(|qc| (qc.version(), qc.quorum_index())).collect::<Vec<_>>(),
        vec![(3, None)]
    );
}

/// `GetMineableCommitments`' already-mined skip (v24:911) holds when the pool still has the mined
/// commitment, as after a level-3 VerifyDB walk re-added it (v24:556): with index 0 of a rotated
/// cycle mined and pooled, and index 1 neither, the miner is offered only index 1's null. The
/// skip matters only in such a partly mined rotated cycle; without rotation, a required cycle's
/// one quorum is unmined (`GetNumCommitmentsRequired`, v24:606-626).
#[test]
fn a_mined_quorum_is_not_offered_again_from_a_refilled_pool() {
    let mut f = Fixture::new();
    f.mine(60, &[f.rot_qc(0)]);
    let mut pool = MineablePool::new();
    pool.add(&f.rot_qc(0));
    assert!(pool.best_for(ROT, &f.qhash(48)).is_some(), "the walk's re-add");
    let offered = f.with(|ctx| pool.mineable_commitments(ctx, ROT, f.llmq(ROT), BlockId(60), 61));
    assert_eq!(offered.expect("complete"), Some(vec![f.null(ROT, 49)]));
}

/// `GetMineableCommitmentsTx` (v24:938-958): each of `GetMineableCommitments`' commitments in its
/// own special transaction, version 3, type 6, no inputs or outputs, lock time 0, the payload
/// `CFinalCommitmentTxPayload { 1, nHeight, commitment }`; `None` with it.
#[test]
fn mineable_commitments_tx_wraps_each_commitment() {
    let f = Fixture::new();
    let pool = MineablePool::new();
    let txs = |llmq_type, tip: u32, height| {
        f.with(|ctx| {
            pool.mineable_commitments_tx(ctx, llmq_type, f.llmq(llmq_type), BlockId(tip), height)
        })
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
/// BL-475: a `qfcommit` that reaches the signature checks reads the members twice, as Core's
/// `Verify(…, checkSigs = true)` does (`llmq/commitment.cpp` v24:177, then :53), and both reads'
/// effects reach the caller's sink.
#[test]
fn a_qfcommit_reads_the_members_twice() {
    let f = Fixture::new();
    let qc = f.test_qc();
    let mut effects = Vec::new();
    let outcome = f.with(|ctx| {
        qfcommit_verdict(
            &mut MineablePool::new(),
            &ctx.llmq,
            ctx.index,
            ctx.mined,
            ctx.members,
            BlockId(60),
            QFCOMMIT_STALE_REPROP_BAN_VERSION,
            &qc,
            &mut effects,
        )
    });
    assert_eq!(outcome, Ok(QfcommitOutcome::Accepted(Some(commitment_hash(&qc)))));
    assert_eq!(effects, vec![members_read(TEST, BlockId(48)); 2]);
}

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
    unconfigured = dash_primitives::Commitment::new(
        unconfigured.version(),
        UNCONFIGURED,
        unconfigured.quorum_hash(),
        unconfigured.quorum_index(),
        unconfigured.signers().clone(),
        unconfigured.valid_members().clone(),
        unconfigured.quorum_public_key().clone(),
        unconfigured.quorum_vvec_hash(),
        unconfigured.quorum_sig().clone(),
        unconfigured.members_sig().clone(),
    );
    unconfigured = dash_primitives::Commitment::new(
        unconfigured.version(),
        unconfigured.llmq_type(),
        QuorumHash::from([0x5a; 32]),
        unconfigured.quorum_index(),
        unconfigured.signers().clone(),
        unconfigured.valid_members().clone(),
        unconfigured.quorum_public_key().clone(),
        unconfigured.quorum_vvec_hash(),
        unconfigured.quorum_sig().clone(),
        unconfigured.members_sig().clone(),
    );
    assert_eq!(run(&f, &mut pool, 60, NEW, &unconfigured), QfcommitOutcome::Misbehaving(100));
    // An unknown base (:144-151) and a base off the active chain (:152-157) are ignored, before
    // the interval check.
    let mut unknown = qc.clone();
    unknown = dash_primitives::Commitment::new(
        unknown.version(),
        unknown.llmq_type(),
        QuorumHash::from([0x5a; 32]),
        unknown.quorum_index(),
        unknown.signers().clone(),
        unknown.valid_members().clone(),
        unknown.quorum_public_key().clone(),
        unknown.quorum_vvec_hash(),
        unknown.quorum_sig().clone(),
        unknown.members_sig().clone(),
    );
    assert_eq!(run(&f, &mut pool, 60, NEW, &unknown), QfcommitOutcome::Ignored);
    let mut fork = f.signed(TEST, 48, &[0, 1]);
    fork = dash_primitives::Commitment::new(
        fork.version(),
        fork.llmq_type(),
        f.hash(FORK_49),
        fork.quorum_index(),
        fork.signers().clone(),
        fork.valid_members().clone(),
        fork.quorum_public_key().clone(),
        fork.quorum_vvec_hash(),
        fork.quorum_sig().clone(),
        fork.members_sig().clone(),
    );
    assert_eq!(run(&f, &mut pool, 60, NEW, &fork), QfcommitOutcome::Ignored);
    let mut above = qc.clone();
    above = dash_primitives::Commitment::new(
        above.version(),
        above.llmq_type(),
        f.qhash(61),
        above.quorum_index(),
        above.signers().clone(),
        above.valid_members().clone(),
        above.quorum_public_key().clone(),
        above.quorum_vvec_hash(),
        above.quorum_sig().clone(),
        above.members_sig().clone(),
    );
    assert_eq!(run(&f, &mut pool, 60, NEW, &above), QfcommitOutcome::Ignored);
    // Not the interval's first block plus the index (:158-164).
    let mut off = qc.clone();
    off = dash_primitives::Commitment::new(
        off.version(),
        off.llmq_type(),
        f.qhash(49),
        off.quorum_index(),
        off.signers().clone(),
        off.valid_members().clone(),
        off.quorum_public_key().clone(),
        off.quorum_vvec_hash(),
        off.quorum_sig().clone(),
        off.members_sig().clone(),
    );
    assert_eq!(run(&f, &mut pool, 60, NEW, &off), QfcommitOutcome::Misbehaving(100));
    let mut bad_index = f.rot_qc(1);
    bad_index = dash_primitives::Commitment::new(
        bad_index.version(),
        bad_index.llmq_type(),
        bad_index.quorum_hash(),
        Some(2),
        bad_index.signers().clone(),
        bad_index.valid_members().clone(),
        bad_index.quorum_public_key().clone(),
        bad_index.quorum_vvec_hash(),
        bad_index.quorum_sig().clone(),
        bad_index.members_sig().clone(),
    );
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
    weak = dash_primitives::Commitment::new(
        weak.version(),
        weak.llmq_type(),
        weak.quorum_hash(),
        weak.quorum_index(),
        bits(3, &[0]),
        weak.valid_members().clone(),
        weak.quorum_public_key().clone(),
        weak.quorum_vvec_hash(),
        weak.quorum_sig().clone(),
        weak.members_sig().clone(),
    );
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
        effects.emit(members_read(qc.llmq_type(), base));
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
    unconfigured = dash_primitives::Commitment::new(
        unconfigured.version(),
        UNCONFIGURED,
        unconfigured.quorum_hash(),
        unconfigured.quorum_index(),
        unconfigured.signers().clone(),
        unconfigured.valid_members().clone(),
        unconfigured.quorum_public_key().clone(),
        unconfigured.quorum_vvec_hash(),
        unconfigured.quorum_sig().clone(),
        unconfigured.members_sig().clone(),
    );
    unconfigured = dash_primitives::Commitment::new(
        unconfigured.version(),
        unconfigured.llmq_type(),
        QuorumHash::from([0x5a; 32]),
        unconfigured.quorum_index(),
        unconfigured.signers().clone(),
        unconfigured.valid_members().clone(),
        unconfigured.quorum_public_key().clone(),
        unconfigured.quorum_vvec_hash(),
        unconfigured.quorum_sig().clone(),
        unconfigured.members_sig().clone(),
    );
    assert_eq!(
        check(qc_payload_versioned(0, 0, &unconfigured)),
        tx_invalid(BAD_QC_COMMITMENT_TYPE)
    );
    // The payload version is 1 (`CURRENT_VERSION`); it is checked before the height.
    assert_eq!(check(qc_payload_versioned(0, 58, &qc)), tx_invalid(BAD_QC_VERSION));
    assert_eq!(check(qc_payload_versioned(2, 58, &qc)), tx_invalid(BAD_QC_VERSION));
    assert_eq!(check(qc_payload_versioned(0, 0, &qc)), tx_invalid(BAD_QC_VERSION));
    let mut unknown = qc.clone();
    unknown = dash_primitives::Commitment::new(
        unknown.version(),
        unknown.llmq_type(),
        QuorumHash::from([0x5a; 32]),
        unknown.quorum_index(),
        unknown.signers().clone(),
        unknown.valid_members().clone(),
        unknown.quorum_public_key().clone(),
        unknown.quorum_vvec_hash(),
        unknown.quorum_sig().clone(),
        unknown.members_sig().clone(),
    );
    assert_eq!(check(qc_payload_versioned(2, 0, &unknown)), tx_invalid(BAD_QC_VERSION));
    // The height is `prev + 1`, before the quorum hash.
    assert_eq!(check(qc_payload(57, &qc)), tx_invalid(BAD_QC_HEIGHT));
    assert_eq!(check(qc_payload(59, &qc)), tx_invalid(BAD_QC_HEIGHT));
    assert_eq!(check(qc_payload(0, &unknown)), tx_invalid(BAD_QC_HEIGHT));
    // The quorum hash: unknown, off `prev`'s chain, or above `prev`.
    assert_eq!(check(qc_payload(58, &unknown)), tx_invalid(BAD_QC_QUORUM_HASH));
    let mut fork = qc.clone();
    fork = dash_primitives::Commitment::new(
        fork.version(),
        fork.llmq_type(),
        f.hash(FORK_48),
        fork.quorum_index(),
        fork.signers().clone(),
        fork.valid_members().clone(),
        fork.quorum_public_key().clone(),
        fork.quorum_vvec_hash(),
        fork.quorum_sig().clone(),
        fork.members_sig().clone(),
    );
    assert_eq!(check(qc_payload(58, &fork)), tx_invalid(BAD_QC_QUORUM_HASH));
    let mut above = qc.clone();
    above = dash_primitives::Commitment::new(
        above.version(),
        above.llmq_type(),
        f.qhash(58),
        above.quorum_index(),
        above.signers().clone(),
        above.valid_members().clone(),
        above.quorum_public_key().clone(),
        above.quorum_vvec_hash(),
        above.quorum_sig().clone(),
        above.members_sig().clone(),
    );
    assert_eq!(check(qc_payload(58, &above)), tx_invalid(BAD_QC_QUORUM_HASH));
    assert!(accept.calls().is_empty());
    // `prev` itself is on its chain, and so is a fork block for a check on the fork.
    let mut at_prev = qc.clone();
    at_prev = dash_primitives::Commitment::new(
        at_prev.version(),
        at_prev.llmq_type(),
        f.qhash(57),
        at_prev.quorum_index(),
        at_prev.signers().clone(),
        at_prev.valid_members().clone(),
        at_prev.quorum_public_key().clone(),
        at_prev.quorum_vvec_hash(),
        at_prev.quorum_sig().clone(),
        at_prev.members_sig().clone(),
    );
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
    version.set_version(4);
    version = dash_primitives::Commitment::new(
        version.version(),
        version.llmq_type(),
        version.quorum_hash(),
        Some(0),
        version.signers().clone(),
        version.valid_members().clone(),
        version.quorum_public_key().clone(),
        version.quorum_vvec_hash(),
        version.quorum_sig().clone(),
        version.members_sig().clone(),
    );
    assert_eq!(f.node_check(qc_payload(58, &version), 57), tx_invalid(BAD_QC_INVALID));
    let mut long = f.null(TEST, 48);
    long = dash_primitives::Commitment::new(
        long.version(),
        long.llmq_type(),
        long.quorum_hash(),
        long.quorum_index(),
        bits(4, &[]),
        long.valid_members().clone(),
        long.quorum_public_key().clone(),
        long.quorum_vvec_hash(),
        long.quorum_sig().clone(),
        long.members_sig().clone(),
    );
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

/// The capability a stub source that cannot answer yet names (CH7 D2).
const NOT_BUILT: &str = "quorum members (a stub source)";

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
    assert_eq!((r.map(|_| vec![]), effects), (not_built, vec![]));
}

// --- EV1-5b-1: type 6 through `check_special_tx_inner` (`specialtxman.cpp:257-258`) ---

/// `CheckSpecialTxInner` of the transaction of `payload` on `prev` (`None`: a null
/// `pindexPrev`), type 6's seams the fixture's with `verifier` when `bound`, with the effects
/// they record.
fn dispatch(
    f: &Fixture,
    payload: Vec<u8>,
    prev: Option<u32>,
    verifier: &dyn CommitmentVerifier,
    bound: bool,
) -> (CheckResult<Infallible>, Vec<Effect>) {
    let tx = qc_tx(payload);
    let mut effects = Vec::new();
    let height = prev.map_or(0, |p| f.chain.header(BlockId(p)).height + 1);
    let cons = ConsensusCtx {
        params: &f.params,
        height,
        prev: prev.map(BlockId),
        chain: &f.chain,
        deployments: &f.deps,
        checkpoints: &NoCheckpoints,
        bls: bls_ctx_for_block(&f.params, height),
        median_time_past: 0,
        header_check_time: 0,
    };
    let list = MnList::scratch();
    let coins = ModelCoins::default();
    let mut quorums = TestQuorums::default();
    let r = f.with(|ctx| {
        let mut cx = SpecialTxCtx {
            evo: EvoCtx {
                cons: &cons,
                check_sigs: false,
            },
            rules: SpecialTxRules {
                v24: true,
                evo_shares: false,
                ipv4_optional: false,
            },
            list: &list,
            coins: &coins,
            net_info: NetInfoRules::new(&f.params, mainnet(), CORE_NET_INFO),
            verifier: &SerialVerifier,
            quorum: bound.then_some(QuorumSeams {
                llmq: ctx.llmq,
                index: ctx.index,
                commitments: verifier,
                quorums: &mut quorums,
                effects: &mut effects,
            }),
            unlock_indexes: None,
        };
        check_special_tx_inner(&tx, tx.payload(), &mut cx)
    });
    (r, effects)
}

/// The dispatcher's type-6 arm is `CheckLLMQCommitment` on `pindexPrev`, verdicts and effects
/// alike, after DIP3's gate. Kills 6d (the arm unported, or on another block than `prev`).
#[test]
fn type_6_dispatches_to_check_llmq_commitment() {
    let f = Fixture::new();
    let qc = f.test_qc();
    let mut unknown = qc.clone();
    unknown = dash_primitives::Commitment::new(
        unknown.version(),
        unknown.llmq_type(),
        QuorumHash::from([0x5a; 32]),
        unknown.quorum_index(),
        unknown.signers().clone(),
        unknown.valid_members().clone(),
        unknown.quorum_public_key().clone(),
        unknown.quorum_vvec_hash(),
        unknown.quorum_sig().clone(),
        unknown.members_sig().clone(),
    );
    let mut at_prev = qc.clone();
    at_prev = dash_primitives::Commitment::new(
        at_prev.version(),
        at_prev.llmq_type(),
        f.qhash(57),
        at_prev.quorum_index(),
        at_prev.signers().clone(),
        at_prev.valid_members().clone(),
        at_prev.quorum_public_key().clone(),
        at_prev.quorum_vvec_hash(),
        at_prev.quorum_sig().clone(),
        at_prev.members_sig().clone(),
    );
    let mut fork = qc.clone();
    fork = dash_primitives::Commitment::new(
        fork.version(),
        fork.llmq_type(),
        f.hash(FORK_48),
        fork.quorum_index(),
        fork.signers().clone(),
        fork.valid_members().clone(),
        fork.quorum_public_key().clone(),
        fork.quorum_vvec_hash(),
        fork.quorum_sig().clone(),
        fork.members_sig().clone(),
    );
    let payloads = [
        vec![1],
        qc_payload_versioned(0, 58, &qc),
        qc_payload(57, &qc),
        qc_payload(58, &unknown),
        qc_payload(58, &fork),
        qc_payload(58, &qc),
        qc_payload(58, &at_prev),
        qc_payload(58, &f.null(TEST, 48)),
    ];
    for (verify_null, verify) in [(true, Ok(true)), (false, Ok(false))] {
        for payload in &payloads {
            let fake = Fake::new(verify_null, verify.clone());
            let direct = f.check(payload.clone(), 57, &fake);
            let direct_calls = fake.calls();
            let fake = Fake::new(verify_null, verify.clone());
            let (r, effects) = dispatch(&f, payload.clone(), Some(57), &fake, true);
            assert_eq!(r, direct.0.map_err(SpecialTxFault::Commitment), "{payload:?}");
            assert_eq!((fake.calls(), effects), (direct_calls, direct.1), "{payload:?}");
        }
    }
    let fake = Fake::new(true, Ok(true));
    let (r, _) = dispatch(&f, qc_payload(51, &f.test_qc()), Some(50), &fake, true);
    assert_eq!(r, Ok(Ok(())), "on prev 50");
    assert_eq!(fake.calls(), vec![Call::Verify(BlockId(48))]);

    // Before DIP0003 the gate rejects first.
    let early = Fixture::with_deployments(59, 1, 1);
    let fake = Fake::new(true, Ok(true));
    let (r, _) = dispatch(&early, qc_payload(58, &early.test_qc()), Some(57), &fake, true);
    assert_eq!(
        r,
        Ok(Err(TxValidationError::new(TxValidationResult::BadSpecial, BAD_TX_TYPE_DIP3_INACTIVE)))
    );
    assert!(fake.calls().is_empty());
}

/// A verifier without a verdict stays a fault through the dispatcher, never a reject, except
/// the unknown-type throw Core's catch turns into `failed-check-special-tx` (D25). Kills 6f.
#[test]
fn type_6_verifier_faults_stay_faults() {
    let f = Fixture::new();
    let payload = qc_payload(58, &f.test_qc());
    let store = QcBlockError::Fault(NodeMemberError::Store(StoreError::inconsistent("a read")));
    for fault in [QcBlockError::BlsUnsupported, store, QcBlockError::NoReason("no base")] {
        let fake = Fake::new(true, Err(fault.clone()));
        let (r, _) = dispatch(&f, payload.clone(), Some(57), &fake, true);
        assert_eq!(r, Err(SpecialTxFault::Commitment(fault)));
    }
    let unknown = QcBlockError::Fault(NodeMemberError::UnknownLlmqType(0xff));
    let (r, _) = dispatch(&f, payload, Some(57), &Fake::new(true, Err(unknown)), true);
    assert_eq!(
        r,
        Ok(Err(TxValidationError::new(TxValidationResult::Consensus, FAILED_CHECK_SPECIAL_TX)))
    );
}

/// Without type 6's seams the dispatcher faults before any check; with DIP0003 active after a
/// null `pindexPrev` it faults too, as Core has no verdict there.
#[test]
fn type_6_unbound_or_without_prev_faults() {
    let f = Fixture::new();
    let fake = Fake::new(true, Ok(true));
    let (r, _) = dispatch(&f, vec![1], Some(57), &fake, false);
    assert_eq!(r, Err(SpecialTxFault::Unbound(TxType::QuorumCommitment)));

    let from_genesis = Fixture::with_deployments(0, 1, 1);
    let (r, _) = dispatch(&from_genesis, vec![1], None, &fake, true);
    assert_eq!(r, Err(SpecialTxFault::Commitment(QcBlockError::NoReason(NULL_PREV))));
    assert!(fake.calls().is_empty());
}

// ---- LQ2 D26: the lines of the mined-commitment path ----

/// `ProcessCommitment`'s lines for a non-null commitment checked and written (v24:340-345,
/// :408, :423-426, :455-456).
const WRITTEN: [&str; 5] = ["Processing", "VerifyDump", "ValidQuorum", "Built", "Processed"];

/// T2: `process_block`'s lines in Core's order (v24:212-300): the count line for the rotated
/// type once its counts pass, `VerifySignatureAsync`'s per non-null commitment whose checks
/// are queued, then each commitment's `ProcessCommitment` lines; a null commitment logs only
/// the first of them.
#[test]
fn process_block_logs_cores_lines_in_order() {
    let f = Fixture::new();
    let qcs = [f.null(TEST, 48), f.rot_qc(0), f.rot_qc(1), f.null(PLATFORM, 48)];
    let mut want = vec!["CommitmentsRequired", "SignatureAsync", "SignatureAsync", "Processing"];
    want.extend(WRITTEN);
    want.extend(WRITTEN);
    want.push("Processing");
    assert_eq!(f.connect(60, &qcs, WRITE_BLS).0.map(|rows| rows.len()), Ok(2));
    let (names, text) = f.trace.take();
    assert_eq!(names, want);
    // One line for the rotated type beside the two non-rotated ones, with Core's counts.
    assert_eq!(
        text[0],
        "[ProcessBlock] h[60] numCommitmentsRequired[2] numCommitmentsInNewBlock[2]"
    );
    assert!(text[1].contains("members[") && text[1].ends_with(']'));
    // `fJustCheck`: no `Built` or `Processed`.
    f.connect(60, &qcs, CHECK_BLS).0.expect("valid");
    let checked: Vec<_> =
        want.iter().copied().filter(|n| *n != "Built" && *n != "Processed").collect();
    assert_eq!(f.trace.take().0, checked);
    // Without `fBLSChecks`: no `VerifySignatureAsync` dump.
    f.connect(60, &qcs, WRITE).0.expect("valid");
    let no_bls: Vec<_> = want.iter().copied().filter(|n| *n != "SignatureAsync").collect();
    assert_eq!(f.trace.take().0, no_bls);
}

/// T2: crash replay (`tip: None`, and no `fBLSChecks`, as Core's `RollforwardBlock` runs it)
/// skips the count line, and `ProcessCommitment`'s first line prints Core's null
/// `GetQuorumBlockHash` (Core's chain has no tip there), after that function's own line
/// (BL-851, LQ2 D27).
#[test]
fn crash_replay_logs_no_count_line_and_the_null_hash() {
    let f = Fixture::new();
    let qcs = [f.null(TEST, 48), f.rot_qc(0), f.rot_qc(1), f.null(PLATFORM, 48)];
    let block = qc_block(60, &qcs);
    assert_eq!(f.connect_on(&block, 60, None, WRITE).0.map(|rows| rows.len()), Ok(2));
    let (names, text) = f.trace.take();
    assert_eq!(names[..2], ["QuorumBlockHashEmpty", "Processing"]);
    assert!(names.iter().all(|n| n != "CommitmentsRequired" && n != "SignatureAsync"));
    let processing: Vec<&String> =
        names.iter().zip(&text).filter(|(n, _)| *n == "Processing").map(|(_, t)| t).collect();
    assert_eq!(processing.len(), 4);
    let zeros = format!("quorumHash={},", "0".repeat(64));
    assert!(processing.iter().all(|text| text.contains(&zeros)), "{processing:?}");
    // On the tip it is the cycle's block.
    f.connect(60, &qcs, WRITE_BLS).0.expect("valid");
    let on_tip = f.trace.take().1;
    assert!(
        on_tip
            .iter()
            .filter(|t| t.starts_with("ProcessCommitment -- processing"))
            .all(|t| !t.contains(&zeros))
    );
}

/// A runner that finds every check valid: the block's lines past the queue, whatever the scheme
/// its signatures were made in.
struct AllValid;

impl BlsCheckRunner for AllValid {
    fn run(&self, _checks: Vec<BlsCheck>) -> BlsVerdict {
        BlsVerdict::Valid
    }
}

/// T2: `VerifySignatureAsync`'s and `ProcessCommitment`'s key is the block's scheme's encoding,
/// as Core's `CBLSPublicKey::ToString()` reads the global scheme flag (bls.h:223-232): legacy
/// before V19, basic after.
#[test]
fn the_key_lines_use_the_blocks_scheme() {
    let hex = |bytes: [u8; 48]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    for (bls, scheme) in
        [(BlsCtx::BASIC, BlsSchemeKind::Basic), (BlsCtx::LEGACY, BlsSchemeKind::Legacy)]
    {
        let mut f = Fixture::new();
        f.bls = bls;
        let qc = f.rot_qc(0);
        let key = hex(qc.quorum_public_key().to_bytes(scheme));
        let other = hex(qc.quorum_public_key().to_bytes(match scheme {
            BlsSchemeKind::Basic => BlsSchemeKind::Legacy,
            _ => BlsSchemeKind::Basic,
        }));
        assert_ne!(key, other);
        let qcs = [f.null(TEST, 48), qc, f.rot_qc(1), f.null(PLATFORM, 48)];
        f.connect_with(&AllValid, 60, &qcs, WRITE_BLS).0.expect("valid");
        let (names, text) = f.trace.take();
        let keyed: Vec<&String> = names
            .iter()
            .zip(&text)
            .filter(|(n, _)| *n == "SignatureAsync" || *n == "Processing")
            .map(|(_, t)| t)
            .collect();
        assert_eq!(keyed.len(), 6);
        assert!(keyed[0].contains(&format!("quorumPublicKey[{key}]")), "{scheme:?}: {}", keyed[0]);
        assert!(
            keyed.iter().any(|t| t.contains(&format!("quorumPublicKey={key} "))),
            "{scheme:?}: {keyed:?}"
        );
        assert!(keyed.iter().all(|t| !t.contains(&other)), "{scheme:?}: {keyed:?}");
    }
}

/// A chain that counts the LLMQ context's reads of it.
struct CountingChain<'a> {
    chain: &'a MemChain,
    reads: std::sync::atomic::AtomicUsize,
}

impl CountingChain<'_> {
    fn count(&self) {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl ChainAncestry for CountingChain<'_> {
    fn header(&self, id: BlockId) -> HeaderCtx {
        self.count();
        self.chain.header(id)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        self.count();
        self.chain.ancestor(id, height)
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        self.count();
        self.chain.prev(id)
    }
}

/// The count line's rotation test (Core's `IsQuorumRotationEnabled`, v24:250) is the only read
/// made to decide a line. A trace that takes no lines makes no such read, so a block outside
/// every mining window reads the chain no more than before the lines existed; a trace that takes
/// lines reads more and gives the same result.
#[test]
fn a_trace_that_takes_no_lines_adds_no_chain_read() {
    let f = Fixture::new();
    let block = qc_block(57, &[]);
    let at = QcBlockAt {
        id: BlockId(57),
        height: 57,
    };
    let run = |trace: &dyn QcTrace| {
        let chain = CountingChain {
            chain: &f.chain,
            reads: Default::default(),
        };
        let mut effects = Vec::new();
        let r = f.with_view(&chain, trace, &f.roster, &f.precompute, &SerialBlsChecks, |ctx| {
            process_block(ctx, &block, at, Some(BlockId(56)), WRITE, &mut effects)
        });
        (r, effects, chain.reads.into_inner())
    };
    let (quiet, quiet_effects, quiet_reads) = run(&crate::NoQcTrace);
    let lines = Lines::default();
    let (logged, logged_effects, logged_reads) = run(&lines);
    assert_eq!(quiet, Ok(vec![]));
    assert_eq!(logged, quiet);
    assert_eq!(logged_effects, quiet_effects);
    assert_eq!(lines.take().0, ["CommitmentsRequired"]);
    assert!(quiet_reads < logged_reads, "{quiet_reads} reads without lines, {logged_reads} with");
}

/// T2: the count line is logged after that type's count checks pass, on every block rotation
/// is enabled at, inside the mining window or not (v24:234-253).
#[test]
fn the_count_line_follows_the_count_checks() {
    let f = Fixture::new();
    let (null_test, null_platform) = (f.null(TEST, 48), f.null(PLATFORM, 48));
    // Rotated type missing: TEST (earlier in `llmqs` order) passed, nothing logged.
    assert_eq!(
        f.connect(60, &[null_test.clone(), null_platform.clone(), f.null(ROT, 48)], WRITE).0,
        invalid(BAD_QC_MISSING)
    );
    assert!(f.trace.take().0.is_empty());
    // A later type missing: the rotated type's line is already logged.
    assert_eq!(
        f.connect(60, &[null_test, f.null(ROT, 48), f.null(ROT, 49)], WRITE).0,
        invalid(BAD_QC_MISSING)
    );
    assert_eq!(f.trace.take().0, ["CommitmentsRequired"]);
    // Outside the window.
    assert_eq!(f.connect(57, &[], WRITE).0, Ok(vec![]));
    assert_eq!(
        f.trace.take().1,
        ["[ProcessBlock] h[57] numCommitmentsRequired[0] numCommitmentsInNewBlock[0]"]
    );
}

/// T3: `CheckLLMQCommitment`'s lines (v24:240-310) through the node's verifier: the dump once
/// the type is known, `Verify`'s two lines and `VALID` for a non-null commitment that passes, the
/// dump alone for a null one or one rejected after it, nothing before it.
#[test]
fn check_llmq_commitment_logs_cores_lines() {
    let f = Fixture::new();
    let qc = f.test_qc();
    let run = |payload: Vec<u8>, prev: u32| {
        let r = f.node_check(payload, prev);
        (r, f.trace.take().0)
    };
    assert_eq!(f.node_check(qc_payload(58, &qc), 57), Ok(Ok(())));
    let (names, text) = f.trace.take();
    assert_eq!(names, ["CheckDump", "VerifyDump", "ValidQuorum", "CheckValid"]);
    assert_eq!(text[3], "CFinalCommitment -- h[57] CheckLLMQCommitment VALID");
    assert!(
        text[1].starts_with("CFinalCommitment::Verify mns[4] validMembers[v[0]=1v[1]=1v[2]=1]")
    );
    let dump = vec!["CheckDump".to_owned()];
    assert_eq!(run(qc_payload(58, &f.null(TEST, 48)), 57), (Ok(Ok(())), dump.clone()));
    // Rejected before the dump: the reject's own line only (BL-851, LQ2 D27).
    let rejected = vec!["CheckReject".to_owned()];
    assert_eq!(run(vec![1], 57), (tx_invalid(BAD_QC_PAYLOAD), rejected.clone()));
    let unconfigured = dash_primitives::Commitment::new(
        qc.version(),
        UNCONFIGURED,
        qc.quorum_hash(),
        qc.quorum_index(),
        qc.signers().clone(),
        qc.valid_members().clone(),
        qc.quorum_public_key().clone(),
        qc.quorum_vvec_hash(),
        qc.quorum_sig().clone(),
        qc.members_sig().clone(),
    );
    assert_eq!(
        run(qc_payload(58, &unconfigured), 57),
        (tx_invalid(BAD_QC_COMMITMENT_TYPE), rejected)
    );
    // Rejected after it.
    let dump_rejected = vec!["CheckDump".to_owned(), "CheckReject".to_owned()];
    assert_eq!(
        run(qc_payload_versioned(2, 58, &qc), 57),
        (tx_invalid(BAD_QC_VERSION), dump_rejected.clone())
    );
    assert_eq!(run(qc_payload(59, &qc), 57), (tx_invalid(BAD_QC_HEIGHT), dump_rejected));
    let unknown = dash_primitives::Commitment::new(
        qc.version(),
        qc.llmq_type(),
        QuorumHash::from([0x5a; 32]),
        qc.quorum_index(),
        qc.signers().clone(),
        qc.valid_members().clone(),
        qc.quorum_public_key().clone(),
        qc.quorum_vvec_hash(),
        qc.quorum_sig().clone(),
        qc.members_sig().clone(),
    );
    assert_eq!(run(qc_payload(58, &unknown), 57), (tx_invalid(BAD_QC_QUORUM_HASH), dump));
}

// ---- LQ2 D27: the lines of a mined commitment that fails (BL-851) ----

/// A commitment's fields, to rebuild it with some changed.
struct Parts {
    version: u16,
    llmq_type: LlmqType,
    quorum_hash: QuorumHash,
    index: Option<i16>,
    signers: DynBitset,
    valid: DynBitset,
    key: BlsEagerPk,
    vvec: QuorumVvecHash,
    quorum_sig: BlsEagerSig,
    members_sig: BlsEagerSig,
}

/// `qc` with the fields `edit` changes.
fn edited(qc: &LlmqCommitment, edit: impl FnOnce(&mut Parts)) -> LlmqCommitment {
    let mut p = Parts {
        version: qc.version(),
        llmq_type: qc.llmq_type(),
        quorum_hash: qc.quorum_hash(),
        index: qc.quorum_index(),
        signers: qc.signers().clone(),
        valid: qc.valid_members().clone(),
        key: qc.quorum_public_key().clone(),
        vvec: qc.quorum_vvec_hash(),
        quorum_sig: qc.quorum_sig().clone(),
        members_sig: qc.members_sig().clone(),
    };
    edit(&mut p);
    LlmqCommitment::new(
        p.version,
        p.llmq_type,
        p.quorum_hash,
        p.index,
        p.signers,
        p.valid,
        p.key,
        p.vvec,
        p.quorum_sig,
        p.members_sig,
    )
}

/// The main chain's block at 60, a rotated type's mining block.
const AT_60: QcBlockAt = QcBlockAt {
    id: BlockId(60),
    height: 60,
};

/// `process_block` of `block` at [`AT_60`] on `tip`, over `members` instead of the fixture's.
fn connect_over(
    f: &Fixture,
    members: &DmlMemberSource<'_>,
    block: &Block,
    tip: Option<BlockId>,
    mode: QcMode,
) -> Result<Vec<MinedCommitmentRecord>, QcBlockError> {
    f.with_sources(members, &f.precompute, &SerialBlsChecks, |ctx| {
        process_block(ctx, block, AT_60, tip, mode, &mut Vec::new())
    })
}

/// The first `n` members of the fixture's roster.
fn roster_of(f: &Fixture, n: usize) -> Roster {
    Roster(f.roster.0[..n].to_vec())
}

/// What `check_llmq_commitment` returns.
type TxCheck = Result<Result<(), TxValidationError>, QcBlockError>;

/// [`Fixture::node_check`] with `members` as the members source: the verdict, then the
/// variants and texts logged.
fn node_check_on(
    f: &Fixture,
    members: &DmlMemberSource<'_>,
    payload: Vec<u8>,
) -> (TxCheck, Vec<String>, Vec<String>) {
    let r = f.with_sources(members, &f.precompute, &SerialBlsChecks, |ctx| {
        let verifier = NodeCommitmentVerifier {
            llmq: ctx.llmq,
            members: ctx.members,
            trace: ctx.trace,
        };
        check_llmq_commitment(
            &qc_tx(payload),
            BlockId(57),
            &ctx.llmq,
            ctx.index,
            &verifier,
            &mut Vec::new(),
        )
    });
    let (names, text) = f.trace.take();
    (r, names, text)
}

/// BL-851 T3: `CheckLLMQCommitment`'s reject lines through the node's verifier
/// (`commitment.cpp` v24:117-302), one case per reject a payload reaches: nothing before C1 and
/// C2's line; the dump (line 1) before C3 to C6; `VerifySizes`' line before C5; `Verify`'s reject
/// line before C6, its member dump only before a bit check's.
#[test]
fn check_llmq_commitment_logs_cores_reject_lines() {
    let f = Fixture::new();
    let qc = f.test_qc();
    let q = qc.quorum_hash();
    let full = &f.roster;
    let check =
        |members: &Roster, qc: &LlmqCommitment| node_check_on(&f, members, qc_payload(58, qc));
    let c = |text: &str| format!("CFinalCommitment -- h[57] {text}");
    let verify_failed = c(&format!("invalid qcTx.commitment[{q}] Verify failed"));
    let verify_rejected = ["CheckDump", "VerifyReject", "CheckReject"];

    // C1, C2: the parent's height, no dump.
    let (r, names, text) = node_check_on(&f, full, vec![1]);
    assert_eq!(r, tx_invalid(BAD_QC_PAYLOAD));
    assert_eq!(names, ["CheckReject"]);
    assert_eq!(text, [c("GetTxPayload LLMQCommitment failed")]);
    let (r, names, text) = check(full, &edited(&qc, |p| p.llmq_type = UNCONFIGURED));
    assert_eq!(r, tx_invalid(BAD_QC_COMMITMENT_TYPE));
    assert_eq!(names, ["CheckReject"]);
    assert_eq!(text, [c(&format!("GetLLMQ failed for llmqType[{}]", UNCONFIGURED.to_base()))]);
    // C3, C4: after the dump.
    let (r, names, text) = node_check_on(&f, full, qc_payload_versioned(2, 58, &qc));
    assert_eq!(r, tx_invalid(BAD_QC_VERSION));
    assert_eq!(names, ["CheckDump", "CheckReject"]);
    assert_eq!(text[1], c("invalid qcTx.nVersion[2]"));
    let (r, names, text) = node_check_on(&f, full, qc_payload(60, &qc));
    assert_eq!(r, tx_invalid(BAD_QC_HEIGHT));
    assert_eq!(names, ["CheckDump", "CheckReject"]);
    assert_eq!(text[1], c("invalid qcTx.nHeight[60]"));
    // C5: `VerifySizes`' one line, the signers' size first.
    let null = f.null(TEST, 48);
    let null_failed = c(&format!("invalid qcTx.commitment[{q}] VerifyNull failed"));
    let sizes = [
        (edited(&null, |p| p.signers = bits(4, &[])), "invalid signers.size=4"),
        (edited(&null, |p| p.signers = bits(2, &[])), "invalid signers.size=2"),
        (
            edited(&null, |p| {
                p.signers = bits(4, &[]);
                p.valid = bits(4, &[]);
            }),
            "invalid signers.size=4",
        ),
        (edited(&null, |p| p.valid = bits(2, &[])), "invalid validMembers.size=2"),
    ];
    for (bad, line) in sizes {
        let (r, names, text) = check(full, &bad);
        assert_eq!(r, tx_invalid(BAD_QC_INVALID_NULL), "{line}");
        assert_eq!(names, verify_rejected, "{line}");
        assert_eq!(text[1..], [format!("CFinalCommitment -- q[{q}] {line}"), null_failed.clone()]);
    }

    // C6, with `Verify`'s line before it: V2 to V13 without the member dump.
    let invalid_pk = BlsEagerPk::new(BlsPkValue::invalid(), BlsSchemeKind::Basic);
    let invalid_sig = || BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic);
    let genesis = edited(&qc, |p| p.quorum_hash = f.qhash(0));
    let cases: Vec<(LlmqCommitment, String)> = vec![
        (genesis.clone(), format!("q[{}] parentless quorum base block", f.qhash(0))),
        (edited(&qc, |p| p.version = 1), format!("q[{q}] invalid nVersion=1 expected=3")),
        (
            edited(&qc, |p| p.quorum_hash = f.qhash(49)),
            format!("q[{}] invalid quorumIndex=0", f.qhash(49)),
        ),
        (edited(&qc, |p| p.signers = bits(4, &[0, 1])), format!("q[{q}] invalid signers.size=4")),
        (
            edited(&qc, |p| p.valid = bits(4, &[0, 1, 2])),
            format!("q[{q}] invalid validMembers.size=4"),
        ),
        (
            edited(&qc, |p| p.valid = bits(3, &[0])),
            format!("q[{q}] invalid validMembers count. validMembersCount=1"),
        ),
        (
            edited(&qc, |p| p.signers = bits(3, &[2])),
            format!("q[{q}] invalid signers count. signersCount=1"),
        ),
        (edited(&qc, |p| p.key = invalid_pk), format!("q[{q}] invalid quorumPublicKey")),
        (
            edited(&qc, |p| p.vvec = QuorumVvecHash::from([0; 32])),
            format!("q[{q}] invalid quorumVvecHash"),
        ),
        (edited(&qc, |p| p.members_sig = invalid_sig()), format!("q[{q}] invalid membersSig")),
        (edited(&qc, |p| p.quorum_sig = invalid_sig()), format!("q[{q}] invalid vvecSig")),
    ];
    for (bad, line) in cases {
        let (r, names, text) = check(full, &bad);
        assert_eq!(r, tx_invalid(BAD_QC_INVALID), "{line}");
        assert_eq!(names, verify_rejected, "{line}");
        let failed = c(&format!("invalid qcTx.commitment[{}] Verify failed", bad.quorum_hash()));
        assert_eq!(text[1..], [format!("CFinalCommitment -- {line}"), failed], "{line}");
    }
    // V14, V15: the member dump with the read's count, then the bit's line.
    let two = roster_of(&f, 2);
    let with_dump = ["CheckDump", "VerifyDump", "VerifyReject", "CheckReject"];
    let (r, names, text) = check(&two, &qc);
    assert_eq!(r, tx_invalid(BAD_QC_INVALID));
    assert_eq!(names, with_dump);
    assert_eq!(
        text[1..],
        [
            "CFinalCommitment::Verify mns[2] validMembers[v[0]=1v[1]=1v[2]=1] \
         signers[s[0]=1s[1]=1s[2]=0]"
                .to_owned(),
            format!(
                "CFinalCommitment -- q[{q}] invalid validMembers bitset. bit 2 should not be set"
            ),
            verify_failed.clone(),
        ]
    );
    let signer_bit = edited(&qc, |p| {
        p.signers = bits(3, &[0, 2]);
        p.valid = bits(3, &[0, 1]);
    });
    let (r, names, text) = check(&two, &signer_bit);
    assert_eq!(r, tx_invalid(BAD_QC_INVALID));
    assert_eq!(names, with_dump);
    assert!(text[1].starts_with("CFinalCommitment::Verify mns[2] "), "{}", text[1]);
    assert_eq!(
        text[2..],
        [
            format!("CFinalCommitment -- q[{q}] invalid signers bitset. bit 2 should not be set"),
            verify_failed,
        ]
    );
    // One member, the first offending bit 2: `mns` is the read's count, not the bit.
    let gap = edited(&qc, |p| {
        p.signers = bits(3, &[0, 2]);
        p.valid = bits(3, &[0, 2]);
    });
    let (r, _, text) = check(&roster_of(&f, 1), &gap);
    assert_eq!(r, tx_invalid(BAD_QC_INVALID));
    assert_eq!(
        text[1..3],
        [
            "CFinalCommitment::Verify mns[1] validMembers[v[0]=1v[1]=0v[2]=1] \
         signers[s[0]=1s[1]=0s[2]=1]"
                .to_owned(),
            format!(
                "CFinalCommitment -- q[{q}] invalid validMembers bitset. bit 2 should not be set"
            ),
        ]
    );
    // The quorum-hash rejects log nothing of their own.
    let (r, names, _) = check(full, &edited(&qc, |p| p.quorum_hash = QuorumHash::from([0x5a; 32])));
    assert_eq!(r, tx_invalid(BAD_QC_QUORUM_HASH));
    assert_eq!(names, ["CheckDump"]);
}

/// A chain view whose header read of `fault` fails.
struct HeaderFault<'a> {
    chain: &'a MemChain,
    fault: BlockId,
}

impl LlmqChain for HeaderFault<'_> {
    type Error = StoreError;

    fn header(&self, id: BlockId) -> Result<HeaderCtx, StoreError> {
        if id == self.fault {
            return Err(StoreError::inconsistent("a header read"));
        }
        Ok(self.chain.header(id))
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, StoreError> {
        Ok(self.chain.ancestor(id, height))
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, StoreError> {
        Ok(self.chain.prev(id))
    }
}

/// BL-851 T3: C1 to C3 read the parent's height for their line alone: where that read fails
/// they log nothing and the verdict stays; C4's read is the check's own, a fault as before.
#[test]
fn a_failed_parent_read_logs_no_reject_line() {
    let f = Fixture::new();
    let chain = HeaderFault {
        chain: &f.chain,
        fault: BlockId(57),
    };
    let llmq = LlmqCtx::new(&f.params, &chain, &f.deps, BlsCtx::BASIC).expect("in domain");
    let verifier = NodeCommitmentVerifier {
        llmq,
        members: &f.roster,
        trace: &f.trace,
    };
    let check = |payload| {
        let r = check_llmq_commitment(
            &qc_tx(payload),
            BlockId(57),
            &llmq,
            &f.index,
            &verifier,
            &mut Vec::new(),
        );
        (r, f.trace.take().0)
    };
    let qc = f.test_qc();
    assert_eq!(check(vec![1]), (tx_invalid(BAD_QC_PAYLOAD), vec![]));
    let unconfigured = edited(&qc, |p| p.llmq_type = UNCONFIGURED);
    assert_eq!(check(qc_payload(58, &unconfigured)), (tx_invalid(BAD_QC_COMMITMENT_TYPE), vec![]));
    assert_eq!(
        check(qc_payload_versioned(0, 58, &qc)),
        (tx_invalid(BAD_QC_VERSION), vec!["CheckDump".to_owned()])
    );
    let (r, names) = check(qc_payload(58, &qc));
    assert!(matches!(r, Err(QcBlockError::Fault(_))), "{r:?}");
    assert_eq!(names, ["CheckDump"]);
}

/// C1 to C3 read the parent's height for their line alone, so a trace that takes no lines makes
/// no read on those rejects, and one that takes lines makes that one read and gives the same
/// verdict (LQ2 D26's rule).
#[test]
fn the_early_reject_lines_read_only_for_a_trace_that_takes_lines() {
    let f = Fixture::new();
    let qc = f.test_qc();
    let run = |trace: &dyn QcTrace, payload: Vec<u8>| {
        let counting = CountingChain {
            chain: &f.chain,
            reads: Default::default(),
        };
        let chain = CompleteChain::<StoreError>::new(&counting);
        let llmq = LlmqCtx::new(&f.params, &chain, &f.deps, BlsCtx::BASIC).expect("in domain");
        let verifier = NodeCommitmentVerifier {
            llmq,
            members: &f.roster,
            trace,
        };
        let r = check_llmq_commitment(
            &qc_tx(payload),
            BlockId(57),
            &llmq,
            &f.index,
            &verifier,
            &mut Vec::new(),
        );
        (r, counting.reads.into_inner())
    };
    let payloads = [
        (vec![1], BAD_QC_PAYLOAD),
        (qc_payload(58, &edited(&qc, |p| p.llmq_type = UNCONFIGURED)), BAD_QC_COMMITMENT_TYPE),
        (qc_payload_versioned(0, 58, &qc), BAD_QC_VERSION),
    ];
    for (payload, reason) in payloads {
        let (quiet, quiet_reads) = run(&crate::NoQcTrace, payload.clone());
        let lines = Lines::default();
        let (logged, logged_reads) = run(&lines, payload);
        assert_eq!(quiet, tx_invalid(reason));
        assert_eq!(logged, quiet, "{reason}");
        assert_eq!((quiet_reads, logged_reads), (0, 1), "{reason}");
        assert!(lines.take().0.iter().any(|n| n == "CheckReject"), "{reason}");
    }
}

/// BL-851 (LQ2 D27): `ProcessBlock`'s pre-pass `bad-qc-dup` is not `ProcessCommitment`, so it
/// logs no F and no `ProcessCommitment` reject line, in every mode (`blockprocessor.cpp`
/// v24:260-264).
#[test]
fn the_prepass_duplicate_logs_no_reject_line() {
    let f = Fixture::new();
    let rot0 = f.rot_qc(0);
    let dup = vec![f.test_qc(), f.null(PLATFORM, 48), rot0.clone(), rot0];
    for mode in MODES {
        f.trace.take();
        assert_eq!(f.connect(60, &dup, mode).0, invalid(BAD_QC_DUP), "{mode:?}");
        let names = f.trace.take().0;
        assert!(
            !names.iter().any(|n| n == "Failed" || n == "ProcessReject"),
            "{mode:?}: {names:?}"
        );
    }
}

/// BL-851 T2 (LQ2 D27): `ProcessCommitment`'s reject lines and `ProcessBlock`'s `failed` line
/// (`blockprocessor.cpp` v24:287, :333-413) in Core's order, in every mode the reject is reached.
#[test]
fn process_block_logs_cores_reject_lines() {
    let f = Fixture::new();
    let (null_test, null_platform) = (f.null(TEST, 48), f.null(PLATFORM, 48));
    let failed = |qc: &LlmqCommitment, height: u32| {
        format!(
            "[ProcessBlock] failed h[{height}] llmqType[{}] version[{}] quorumIndex[{}] \
             quorumHash[{}]",
            qc.llmq_type().to_base(),
            qc.version(),
            quorum_index(qc),
            qc.quorum_hash()
        )
    };

    // An older cycle's commitment: `bad-qc-block` after its first line, then `failed`.
    let old = f.signed(TEST, 24, &[0, 1]);
    let qcs = [old.clone(), f.rot_qc(0), f.rot_qc(1), null_platform.clone()];
    for mode in MODES {
        assert_eq!(f.connect(60, &qcs, mode).0, invalid(BAD_QC_BLOCK), "{mode:?}");
        let (names, text) = f.trace.take();
        let tail = &names[names.len() - 3..];
        assert_eq!(tail, ["Processing", "ProcessReject", "Failed"], "{mode:?}");
        assert!(!names.iter().any(|n| n == "QuorumBlockHashEmpty"), "{mode:?}");
        let n = text.len();
        assert!(text[n - 2].contains(&format!(
            "quorumHash={}, qc.quorumHash={} signers=2",
            f.qhash(48),
            old.quorum_hash()
        )));
        assert!(text[n - 2].ends_with(" non equal quorumHash."), "{}", text[n - 2]);
        assert_eq!(text[n - 1], failed(&old, 60));
    }

    // Two rotated commitments, the second of an older cycle: the first is written, then the
    // second fails.
    let old_rot = f.signed(ROT, 25, &[0, 1, 2, 3]);
    let qcs = [null_test.clone(), f.rot_qc(0), old_rot.clone(), null_platform.clone()];
    assert_eq!(f.connect(60, &qcs, WRITE).0, invalid(BAD_QC_BLOCK));
    let (names, text) = f.trace.take();
    let mut want = vec!["CommitmentsRequired", "Processing"];
    want.extend(WRITTEN);
    want.extend(["Processing", "ProcessReject", "Failed"]);
    assert_eq!(names, want);
    assert_eq!(text.last().expect("lines"), &failed(&old_rot, 60));
    assert!(text[text.len() - 2].contains(&format!("quorumIndex=1, quorumHash={}", f.qhash(49))));

    // A commitment the active chain already mined: `bad-qc-dup` logs no line of its own.
    let mut f = Fixture::new();
    f.mine(58, &[f.rot_qc(0)]);
    let qcs = [null_test.clone(), f.rot_qc(0), null_platform.clone()];
    for mode in MODES {
        assert_eq!(f.connect(60, &qcs, mode).0, invalid(BAD_QC_DUP), "{mode:?}");
        let (names, text) = f.trace.take();
        assert_eq!(names[names.len() - 2..], ["Processing", "Failed"], "{mode:?}");
        assert_eq!(text.last().expect("lines"), &failed(&f.rot_qc(0), 60));
    }

    // A null commitment of the wrong size: `VerifyNull`'s sizes line, then Core's own.
    let f = Fixture::new();
    let short = edited(&null_test, |p| p.valid = bits(2, &[]));
    assert_eq!(
        f.connect(60, &[short.clone(), f.rot_qc(0), f.rot_qc(1), null_platform.clone()], WRITE).0,
        invalid(BAD_QC_INVALID_NULL)
    );
    let (names, text) = f.trace.take();
    assert_eq!(names[1..], ["Processing", "VerifyReject", "ProcessReject", "Failed"]);
    assert_eq!(
        text[2],
        format!("CFinalCommitment -- q[{}] invalid validMembers.size=2", f.qhash(48))
    );
    assert_eq!(
        text[3],
        format!(
            "ProcessCommitment -- height=60, type=100, quorumIndex=0, quorumHash={}, signers=0, \
             validMembers=0qc verifynull failed.",
            f.qhash(48)
        )
    );

    // `Verify` refusing the commitment it passed in `CheckLLMQCommitment` (here, fewer members):
    // the dump, the bit's line, Core's own, then `failed`.
    let qc = f.test_qc();
    let qcs = [qc.clone(), f.rot_qc(0), f.rot_qc(1), null_platform.clone()];
    let block = qc_block(60, &qcs);
    let two = roster_of(&f, 2);
    for mode in [WRITE, CHECK] {
        let r = connect_over(&f, &two, &block, Some(BlockId(59)), mode);
        assert_eq!(r, invalid(BAD_QC_INVALID), "{mode:?}");
        let (names, text) = f.trace.take();
        assert_eq!(
            names[1..],
            ["Processing", "VerifyDump", "VerifyReject", "ProcessReject", "Failed"],
            "{mode:?}"
        );
        assert!(text[2].starts_with("CFinalCommitment::Verify mns[2] "));
        assert!(text[4].ends_with(" qc verify failed."), "{}", text[4]);
        assert_eq!(text[5], failed(&qc, 60));
    }

    // A fault logs no `failed` (LQ2 D27): the member read of `Verify` fails.
    for mode in [WRITE, CHECK] {
        let r = connect_over(&f, &NotBuilt, &block, Some(BlockId(59)), mode);
        assert!(matches!(r, Err(QcBlockError::Fault(_))), "{r:?}");
        let (names, _) = f.trace.take();
        assert_eq!(names[1..], ["Processing"], "{mode:?}");
    }
}

/// BL-851 T2: the lines no block that passed `CheckLLMQCommitment` reaches (LQ2 D27), through
/// the fakes: an unknown base (Core's `return false`, then `failed`; on the signature path
/// `ProcessBlock`'s own line and no `failed`), an unconfigured type, and `GetQuorumBlockHash`'s
/// null hash with a tip and in crash replay.
#[test]
fn process_block_logs_the_unreachable_reject_lines() {
    let mut f = Fixture::new();
    let (null_test, null_platform) = (f.null(TEST, 48), f.null(PLATFORM, 48));
    let qc = f.test_qc();
    let qcs = [qc.clone(), f.rot_qc(0), f.rot_qc(1), null_platform.clone()];
    f.index.0.remove(f.qhash(48).as_bytes());
    let unknown_base = |height: Option<u32>| {
        let prefix = match height {
            Some(h) => format!("[ProcessBlock] h[{h}]"),
            None => "ProcessCommitment --".to_owned(),
        };
        format!(
            "{prefix} unexpectedly failed due to no known pindex for hash[{}]",
            qc.quorum_hash()
        )
    };
    let no_reason = Err(QcBlockError::NoReason("no known block index for a quorum hash"));
    for mode in [WRITE, CHECK] {
        assert_eq!(f.connect(60, &qcs, mode).0, no_reason, "{mode:?}");
        let (names, text) = f.trace.take();
        assert_eq!(names[1..], ["Processing", "UnknownBase", "Failed"], "{mode:?}");
        assert_eq!(text[2], unknown_base(None));
    }
    for mode in [WRITE_BLS, CHECK_BLS] {
        assert_eq!(f.connect(60, &qcs, mode).0, no_reason, "{mode:?}");
        let (names, text) = f.trace.take();
        assert_eq!(names, ["CommitmentsRequired", "UnknownBase"], "{mode:?}");
        assert_eq!(text[1], unknown_base(Some(60)));
    }

    // An unconfigured type, past `GetCommitmentsFromBlock`: `ProcessCommitment` alone.
    let f = Fixture::new();
    let unconfigured = edited(&qc, |p| p.llmq_type = UNCONFIGURED);
    let r = f.with(|ctx| {
        process_commitment(
            ctx,
            AT_60,
            Some(BlockId(59)),
            BlockId(59),
            &unconfigured,
            false,
            &mut Vec::new(),
        )
    });
    assert_eq!(r, Err(QcBlockError::NoReason("invalid commitment type")));
    assert_eq!(
        f.trace.take().1,
        [format!("ProcessCommitment -- invalid commitment type {}", UNCONFIGURED.to_base())]
    );

    // With a tip, an index past the chain: `GetQuorumBlockHash`'s line, then the null hash.
    let far = edited(&f.rot_qc(1), |p| p.index = Some(20));
    let r = f.with(|ctx| {
        process_commitment(ctx, AT_60, Some(BlockId(59)), BlockId(59), &far, false, &mut Vec::new())
    });
    assert_eq!(r, invalid(BAD_QC_BLOCK));
    let (names, text) = f.trace.take();
    assert_eq!(names, ["QuorumBlockHashEmpty", "Processing", "ProcessReject"]);
    assert_eq!(
        text[0],
        "[GetQuorumBlockHash] llmqType[103] h[60] qi[20] quorumStartHeight[68] quorumHash[EMPTY]"
    );
    assert!(text[2].ends_with(" quorumHash is null."), "{}", text[2]);

    // Crash replay: the line before every commitment's first, null ones included; a null quorum
    // hash is `bad-qc-block`, then `failed`.
    let qcs = [null_test.clone(), f.rot_qc(0), f.rot_qc(1), null_platform.clone()];
    let block = qc_block(60, &qcs);
    assert_eq!(f.connect_on(&block, 60, None, WRITE).0.map(|rows| rows.len()), Ok(2));
    let (names, text) = f.trace.take();
    let firsts: Vec<(&String, &String)> =
        names.iter().zip(&names[1..]).filter(|(_, next)| *next == "Processing").collect();
    assert_eq!(firsts.len(), 4);
    assert!(firsts.iter().all(|(g, _)| *g == "QuorumBlockHashEmpty"), "{names:?}");
    assert_eq!(
        text[0],
        "[GetQuorumBlockHash] llmqType[100] h[60] qi[0] quorumStartHeight[48] quorumHash[EMPTY]"
    );
    let nameless = edited(&null_test, |p| p.quorum_hash = QuorumHash::from([0; 32]));
    let block = qc_block(60, &[nameless.clone(), f.rot_qc(0), f.rot_qc(1), null_platform]);
    assert_eq!(f.connect_on(&block, 60, None, WRITE).0, invalid(BAD_QC_BLOCK));
    let (names, text) = f.trace.take();
    assert_eq!(names, ["QuorumBlockHashEmpty", "Processing", "ProcessReject", "Failed"]);
    assert!(text[2].contains(&format!("quorumHash={}, signers=0", "0".repeat(64))), "{}", text[2]);
    assert!(text[2].ends_with(" quorumHash is null."));
}

/// A chain view with no block at `missing`: `GetQuorumBlockHash`'s null hash with a tip.
struct Gap<'a> {
    chain: &'a MemChain,
    missing: std::ops::RangeInclusive<u32>,
}

impl LlmqChain for Gap<'_> {
    type Error = StoreError;

    fn header(&self, id: BlockId) -> Result<HeaderCtx, StoreError> {
        Ok(self.chain.header(id))
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, StoreError> {
        Ok(self.chain.ancestor(id, height).filter(|_| !self.missing.contains(&height)))
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, StoreError> {
        Ok(self.chain.prev(id))
    }
}

/// BL-851 T2: `GetNumCommitmentsRequired`'s `GetQuorumBlockHash` logs its line for each quorum
/// whose base the chain lacks, which a tip never does inside the mining window (LQ2 D27), here
/// through a chain view with a gap; such a quorum is not counted.
#[test]
fn the_count_logs_each_missing_quorum_block() {
    let f = Fixture::new();
    let chain = Gap {
        chain: &f.chain,
        missing: 48..=49,
    };
    let llmq = LlmqCtx::new(&f.params, &chain, &f.deps, BlsCtx::BASIC).expect("in domain");
    let mined = f.store.view(&f.chain);
    let ctx = QcCtx {
        llmq,
        index: &f.index,
        mined: &mined,
        members: &f.roster,
        precompute: &f.precompute,
        bls_checks: &SerialBlsChecks,
        trace: &f.trace,
    };
    assert_eq!(num_commitments_required(&ctx, ROT, f.llmq(ROT), BlockId(59), 60), Ok(0));
    let (names, text) = f.trace.take();
    assert_eq!(names, ["QuorumBlockHashEmpty"; 2]);
    assert_eq!(
        text,
        [
            "[GetQuorumBlockHash] llmqType[103] h[60] qi[0] quorumStartHeight[48] quorumHash[EMPTY]",
            "[GetQuorumBlockHash] llmqType[103] h[60] qi[1] quorumStartHeight[49] quorumHash[EMPTY]",
        ]
    );
    // Outside the window nothing is read.
    assert_eq!(num_commitments_required(&ctx, ROT, f.llmq(ROT), BlockId(49), 50), Ok(0));
    assert!(f.trace.take().0.is_empty());
}

/// BL-851 T2: in crash replay P4 and P6 print the commitment's own hash, the one `ProcessCommitment`
/// compares there, not `GetQuorumBlockHash`'s null one (`blockprocessor.cpp` v24:347-350).
#[test]
fn crash_replay_reject_lines_print_the_commitments_hash() {
    let f = Fixture::new();
    let null_platform = f.null(PLATFORM, 48);
    let short = edited(&f.null(TEST, 48), |p| p.valid = bits(2, &[]));
    let block = qc_block(60, &[short, f.rot_qc(0), f.rot_qc(1), null_platform.clone()]);
    assert_eq!(f.connect_on(&block, 60, None, WRITE).0, invalid(BAD_QC_INVALID_NULL));
    let (names, text) = f.trace.take();
    assert_eq!(
        names,
        ["QuorumBlockHashEmpty", "Processing", "VerifyReject", "ProcessReject", "Failed"]
    );
    assert!(text[3].contains(&format!("quorumHash={}, signers=0", f.qhash(48))), "{}", text[3]);
    let qc = f.test_qc();
    let block = qc_block(60, &[qc, f.rot_qc(0), f.rot_qc(1), null_platform]);
    let r = connect_over(&f, &roster_of(&f, 2), &block, None, WRITE);
    assert_eq!(r, invalid(BAD_QC_INVALID));
    let (names, text) = f.trace.take();
    assert_eq!(names[names.len() - 2..], ["ProcessReject", "Failed"]);
    let p6 = &text[text.len() - 2];
    assert!(p6.contains(&format!("quorumHash={}, signers=2", f.qhash(48))), "{p6}");
    assert!(p6.ends_with(" qc verify failed."), "{p6}");
}
