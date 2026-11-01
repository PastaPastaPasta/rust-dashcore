//! BL-594, CH7b-2's entry condition: route B's production tip binding on a real V loop. An
//! `OnV` command reaches `ChainQuery::bind`, `OnVBinding::members` and `NodeEvo`'s
//! `EvoReads::with_llmq`, and answers LQ1's model over the lists the committed evo layer holds,
//! under the published scheme (B1). A call queued while an attempt holds the facade's gate
//! exclusively waits on the gate before it reads anything (B2). C9: `OnVBinding::quorum` and
//! `scan_quorums` answer the quorums mined on V's chain, through the facade's caches (B3); a
//! failed row write is reported once through each of the three calls (M-1).

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::SeqCst;

use dash_chain::ChainReader;
use dash_consensus::primitives::Header;
use dash_consensus::{Block, BlockId, BlsCtx};
use dash_dml::creditpool::CreditPool;
use dash_dml::store::MinedCommitmentRecord;
use dash_dml::testkit::numbered;
use dash_dml::{EvoDelta, MnList, StoreError};
use dash_evo::cbtx::{CoinbaseFacts, nonnull_coinbase_chainlock};
use dash_evo::llmq::reference::ModelMembersFacade;
use dash_evo::llmq::{CompleteChain, LlmqCtx, LlmqSource};
use dash_evo::types::ProTxHash;
use dash_params::ChainSetup;
use dash_params::LlmqType;
use dash_primitives::{BlockHash, BlockHeader, QuorumHash, QuorumVvecHash};
use dash_types::Numeric as _;
use dashd_store::{ErrorKind, MemoryEngine};

use super::llmq::{wait_until, waiters};
use super::llmq_members::{MapRows, ROTATED, TEST, World, params, pros, world};
use super::node_evo::{Store, coinbase_at, node_evo_over_blocks};
use super::support::{Mined, Rig, RigConfig, SPACING, branch, mine};
use crate::chain::{
    AbortSink, BlockConnector, BlockStatusWriter, ConnectCx, ConnectOutcome, DisconnectResult,
    EvoReadCx, EvoReads, Fatal, LlmqCall, NullConnector, OnVMembersError, ProposalChecks,
    RecordingAbort, TipLlmq, VContext, ViewTarget,
};
use crate::connect::EvoHooks as _;
use crate::evo::RocksEvoStore;
use crate::evo::ev1::Ev1NotBuilt;
use crate::evo::node::{Ancestry, EvoBlocks, NodeEvo, SharedEvoLayer};
use crate::evo::testkit;
use crate::flush::EffectLedger;
use crate::llmq::facade::MembersFacade;
use crate::llmq::quorum::QuorumPtr;
use crate::llmq::rows::RuntimeRows;
use crate::store::{CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine};

/// DIP0003, V19, V20 and DIP0024 from here.
pub(super) const FORKS: u32 = 2;
pub(super) const TIP: u32 = 60;
/// A non-rotated base, whose work block is 40.
const BASE: u32 = 48;
/// A rotated cycle base.
pub(super) const CYCLE: u32 = 24;
/// The quorums [`Setup`]'s chain mines, as (type, base, mining block): the non-rotated ones
/// based at 24 and [`BASE`], in their mining windows (10 to 18 past the base), and the rotated
/// cycle's index 0 (12 to 20).
const MINED: [(LlmqType, u32, u32); 3] =
    [(TEST, CYCLE, 34), (TEST, BASE, 58), (ROTATED, CYCLE, 36)];

/// Every block's coinbase is the fixture's CbTx v3 at its height, with a non-null ChainLock: past
/// V20 a work block's modifier is then the signature form, serialized under the binding's
/// scheme.
pub(super) struct CbTxBlocks;

impl EvoBlocks for CbTxBlocks {
    fn read(&self, reader: &ChainReader, id: BlockId) -> Option<Arc<Block>> {
        Some(cbtx_block(crate::chain::index::entry(reader, id).height))
    }
}

/// The chain to [`TIP`]: from DIP0003 each block's coinbase is the fixture's CbTx at its height,
/// as [`CbTxBlocks`] reads it, so V connects it.
fn cbtx_branch(p: &ChainSetup) -> Vec<Mined> {
    let mut blocks = branch(p, &Mined::genesis(p), FORKS - 1, 7);
    while blocks.len() < TIP as usize {
        let parent = blocks.last().expect("a parent");
        let height = parent.height + 1;
        let cb = coinbase_at(i32::try_from(height).expect("a small height"), 0);
        let block = mine(p, parent.hash(), parent.time() + SPACING, vec![cb]);
        blocks.push(Mined {
            block,
            height,
        });
    }
    blocks
}

fn cbtx_block(height: u32) -> Arc<Block> {
    let height = i32::try_from(height).expect("a small height");
    Arc::new(Block::new(Header::new(BlockHeader::default()), vec![coinbase_at(height, 0)]))
}

/// The list after a DIP0003 block: masternodes 1 to 9.
pub(super) fn list(hash: BlockHash, height: u32) -> MnList {
    let mut list = MnList::new(hash, i32::try_from(height).expect("a small height"), 0);
    for dmn in (1..=9).map(numbered) {
        list.try_add(dmn, true).expect("distinct masternodes");
    }
    list
}

/// The committed layer: each DIP0003 block's list row, the first one a snapshot, and its credit
/// pool, an empty one, which V's view reads once V20 is active.
pub(super) fn seed(store: &Store, layer: &SharedEvoLayer, blocks: &[Mined]) {
    seed_mined(store, layer, blocks, &[]);
}

/// [`seed`], with the mined rows of `mined`'s (type, base, mining block) quorums.
pub(super) fn seed_mined(
    store: &Store,
    layer: &SharedEvoLayer,
    blocks: &[Mined],
    mined: &[(LlmqType, u32, u32)],
) {
    let mut prev = MnList::default_pre_dip3();
    for b in blocks.iter().filter(|b| b.height >= FORKS) {
        let next = list(b.hash(), b.height);
        let delta = EvoDelta {
            block: b.hash(),
            height: b.height,
            dml: Some(testkit::dml(&prev, &next, b.height == FORKS)),
            mined_commitments: mined
                .iter()
                .filter(|m| m.2 == b.height)
                .map(|&(t, base, _)| {
                    mined_row(t, &blocks[usize::try_from(base - 1).expect("small")])
                })
                .collect(),
            credit_data: None,
            credit_pool: Some(Arc::new(Ok(CreditPool::default()))),
            mnhf_signals: None,
            erased_mined: Vec::new(),
            evo_tip: Some(b.hash()),
        };
        store.commit(&mut layer.lock(), &delta).expect("the row commits");
        prev = next;
    }
}

/// The row `ProcessCommitment` writes for a non-null commitment of `llmq_type` based at `base`:
/// a rotated one at index 0.
pub(super) fn mined_row(llmq_type: LlmqType, base: &Mined) -> MinedCommitmentRecord {
    let rotation_index = (llmq_type == ROTATED).then_some(0);
    let quorum_hash = testkit::quorum_hash(&base.hash());
    let version = if rotation_index.is_some() {
        2
    } else {
        1
    };
    let mut qc = testkit::commitment(llmq_type, quorum_hash, version);
    qc.quorum_vvec_hash = QuorumVvecHash::from([1; 32]);
    MinedCommitmentRecord {
        llmq_type,
        quorum_hash,
        base_height: base.height,
        rotation_index,
        commitment: Arc::new(qc),
    }
}

/// The model's inputs: the seeded lists and the fixture's blocks, by V's index.
pub(super) struct Lists<'a> {
    pub(super) reader: &'a ChainReader,
}

impl LlmqSource<MnList> for Lists<'_> {
    type Error = StoreError;

    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<MnList>>, StoreError> {
        let e = crate::chain::index::entry(self.reader, at);
        Ok(Some(Arc::new(if e.height < FORKS {
            MnList::default_pre_dip3()
        } else {
            list(e.hash, e.height)
        })))
    }

    fn coinbase_facts(&self, at: BlockId) -> Result<Option<CoinbaseFacts>, StoreError> {
        let block = cbtx_block(crate::chain::index::entry(self.reader, at).height);
        Ok(Some(CoinbaseFacts::First(block.txs()[0].clone())))
    }
}

/// A source that counts the reads it serves.
struct Counted<'a> {
    src: &'a dyn LlmqSource<MnList, Error = StoreError>,
    reads: &'a AtomicUsize,
}

impl LlmqSource<MnList> for Counted<'_> {
    type Error = StoreError;

    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<MnList>>, StoreError> {
        self.reads.fetch_add(1, SeqCst);
        self.src.mn_list(at)
    }

    fn coinbase_facts(&self, at: BlockId) -> Result<Option<CoinbaseFacts>, StoreError> {
        self.reads.fetch_add(1, SeqCst);
        self.src.coinbase_facts(at)
    }
}

/// V's connector: [`NullConnector`]'s connects, with `NodeEvo` as V's evo reads and its scheme
/// published. Each read of the source `NodeEvo` binds is counted.
struct RouteB {
    evo: NodeEvo<MemoryEngine, Ev1NotBuilt>,
    reads: Arc<AtomicUsize>,
}

impl BlockConnector for RouteB {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.connect_block(cx, v, target, block, checked, id, st)
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        NullConnector.disconnect_block(cx, v, target, block, id)
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        block: &Block,
        prev: BlockId,
        check_pow: bool,
        check_merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.test_block_validity(cx, v, checks, block, prev, check_pow, check_merkle)
    }

    fn published_bls(&self) -> Option<BlsCtx> {
        self.evo.published_bls()
    }

    fn evo_reads(&self) -> &dyn EvoReads {
        self
    }
}

impl EvoReads for RouteB {
    fn with_llmq(
        &self,
        cx: EvoReadCx<'_>,
        bls: BlsCtx,
        call: &mut LlmqCall<'_>,
    ) -> Result<(), OnVMembersError> {
        self.evo.with_llmq(cx, bls, &mut |at| {
            let src = Counted {
                src: at.src,
                reads: &self.reads,
            };
            call(TipLlmq {
                src: &src,
                ..at
            })
        })
    }

    fn list_at(&self, cx: EvoReadCx<'_>, at: BlockId) -> Result<Arc<MnList>, StoreError> {
        self.evo.list_at(cx, at)
    }

    fn credit_pool_after(
        &self,
        cx: EvoReadCx<'_>,
        at: BlockId,
    ) -> Result<Option<Arc<dash_dml::creditpool::PoolState>>, StoreError> {
        self.evo.credit_pool_after(cx, at)
    }

    fn has_mined_commitment(
        &self,
        cx: EvoReadCx<'_>,
        llmq_type: LlmqType,
        quorum_hash: &dash_primitives::QuorumHash,
        tip: BlockId,
    ) -> Result<bool, StoreError> {
        self.evo.has_mined_commitment(cx, llmq_type, quorum_hash, tip)
    }
}

/// A V over the seeded chain, connected to [`TIP`] through [`NullConnector`]: `NodeEvo`
/// committed to every DIP0003 block's list and [`MINED`]'s rows and publishing the basic scheme,
/// and route B's facade.
pub(super) struct Setup {
    p: Arc<ChainSetup>,
    pub(super) rig: Rig,
    pub(super) w: World,
    pub(super) blocks: Vec<Mined>,
    ids: Vec<BlockId>,
    pub(super) reads: Arc<AtomicUsize>,
}

impl Setup {
    pub(super) fn new() -> Setup {
        let p = Arc::new(params(FORKS, FORKS));
        let w = world(&p);
        Setup::over(p, w, None)
    }

    /// [`Setup::new`] with `w`'s facade as route B's, and `abort`, if any, as V's `AbortNode`
    /// sink.
    fn over(p: Arc<ChainSetup>, w: World, abort: Option<Arc<dyn AbortSink>>) -> Setup {
        let blocks = cbtx_branch(&p);
        let (evo, layer, store) =
            node_evo_over_blocks(&p, BlsCtx::BASIC, Ev1NotBuilt, Box::new(CbTxBlocks), None);
        seed_mined(&store, &layer, &blocks, &MINED);
        let reads = Arc::new(AtomicUsize::new(0));
        let rig_sink = abort.is_none();
        let rig = Rig::with(RigConfig {
            params: Some(p.clone()),
            connector: Some(Box::new(RouteB {
                evo,
                reads: reads.clone(),
            })),
            facade: Some(w.facade.clone()),
            abort,
            ..RigConfig::default()
        });
        rig.headers(&blocks);
        for b in &blocks {
            rig.accept(b);
        }
        if rig_sink {
            rig.assert_no_abort();
        }
        let ids = rig.ids(&blocks);
        Setup {
            p,
            rig,
            w,
            blocks,
            ids,
            reads,
        }
    }

    pub(super) fn at(&self, height: u32) -> BlockId {
        self.ids[usize::try_from(height - 1).expect("small")]
    }

    pub(super) fn hash(&self, height: u32) -> BlockHash {
        self.blocks[usize::try_from(height - 1).expect("small")].hash()
    }

    fn quorum_hash(&self, height: u32) -> QuorumHash {
        testkit::quorum_hash(&self.hash(height))
    }

    /// LQ1's model of `calls` in order, on V over V's index and deployments, the seeded lists
    /// and the basic scheme: the rosters, and the rows it stored.
    pub(super) fn model(&self, calls: Vec<(LlmqType, BlockId)>) -> (Vec<Vec<ProTxHash>>, MapRows) {
        let p = self.p.clone();
        let out = Arc::new(std::sync::Mutex::new(None));
        let put = out.clone();
        self.rig.on_v(move |v| {
            let chain = Ancestry(&v.reader);
            let complete = CompleteChain::<StoreError>::new(&chain);
            let deployments = v.deployments.view();
            let ctx = LlmqCtx::new(&p, &complete, &deployments, BlsCtx::BASIC).expect("in domain");
            let src = Lists {
                reader: &v.reader,
            };
            let mut facade = ModelMembersFacade::new(&p);
            let mut rows = MapRows::default();
            let rosters = calls
                .into_iter()
                .map(|(t, base)| facade.members(&ctx, &src, &mut rows, t, base, false))
                .map(|r| r.map(|(m, _)| pros(&m)).expect("the model answers"))
                .collect();
            *put.lock().expect("out") = Some((rosters, rows));
            Ok(())
        });
        out.lock().expect("out").take().expect("V ran it")
    }
}

/// Route B's call: `GetAllQuorumMembers(t, base, false)` through the tip binding.
type Call = Result<Vec<ProTxHash>, String>;

fn route_b(s: &Setup, t: LlmqType, base: BlockId) -> crate::chain::ActorReply<Call> {
    s.rig.v.on_v(move |on_v, q| {
        q.bind(on_v).members(t, base, false).map(|m| pros(&m)).map_err(|e| e.to_string())
    })
}

/// Route B's `GetQuorum(t, hash)`.
fn quorum_b(s: &Setup, t: LlmqType, hash: QuorumHash) -> crate::chain::ActorReply<Quorum> {
    s.rig.v.on_v(move |on_v, q| q.bind(on_v).quorum(t, &hash).map_err(|e| e.to_string()))
}

/// Route B's `ScanQuorums(t, start, count)`.
pub(super) fn scan_b(
    s: &Setup,
    t: LlmqType,
    start: BlockId,
    count: usize,
) -> crate::chain::ActorReply<Scan> {
    s.rig
        .v
        .on_v(move |on_v, q| q.bind(on_v).scan_quorums(t, start, count).map_err(|e| e.to_string()))
}

type Quorum = Result<Option<QuorumPtr>, String>;
pub(super) type Scan = Result<Vec<QuorumPtr>, String>;

fn bases(quorums: &[QuorumPtr]) -> Vec<BlockId> {
    quorums.iter().map(|q| q.base).collect()
}

/// B1: through the binding, a non-rotated and a rotated call answer the model's rosters over the
/// committed layer's lists and the published scheme; the rotation writes its row to the runtime
/// rows inline, and a second call hits the cache without a read or a write.
#[test]
fn the_tip_binding_answers_the_model_over_the_committed_layer() {
    let s = Setup::new();
    // The work block's ChainLock serializes differently under the block-1 scheme, so a binding
    // that took it would answer another roster.
    let work = CoinbaseFacts::First(cbtx_block(BASE - 8).txs()[0].clone());
    let cl = |bls: BlsCtx| {
        nonnull_coinbase_chainlock(&work, BASE - 8, true, bls)
            .map(|(s, _)| s.to_bytes(bls.scheme()))
    };
    assert!(cl(BlsCtx::BASIC).is_some(), "a ChainLock under the published scheme");
    assert_ne!(cl(BlsCtx::BASIC), cl(BlsCtx::LEGACY), "the schemes agree");

    let (want, rows) = s.model(vec![(TEST, s.at(BASE)), (ROTATED, s.at(CYCLE))]);
    let got = route_b(&s, TEST, s.at(BASE)).recv().expect("served");
    assert_eq!(got.as_deref(), Ok(&want[0][..]), "the non-rotated roster");
    assert_eq!(want[0].len(), 3, "a full roster");

    assert_eq!(s.w.store.writes.load(SeqCst), 0);
    let got = route_b(&s, ROTATED, s.at(CYCLE)).recv().expect("served");
    assert_eq!(got.as_deref(), Ok(&want[1][..]), "the rotated roster");
    let key = (ROTATED.to_base(), s.hash(CYCLE));
    assert_eq!(s.w.store.writes.load(SeqCst), 1, "the row, written inline");
    assert_eq!(s.w.store.rows.lock().get(&key), rows.0.get(&key), "the model's row");
    assert!(rows.0.contains_key(&key));

    let reads = s.reads.load(SeqCst);
    let again = route_b(&s, ROTATED, s.at(CYCLE)).recv().expect("served");
    assert_eq!(again, got, "the cached roster");
    assert_eq!(s.reads.load(SeqCst), reads, "a cache hit reads no source");
    assert_eq!(s.w.store.writes.load(SeqCst), 1, "a cache hit writes no row");
}

/// B2: with the facade's gate held exclusively, as an attempt holds it, a route-B call queued on
/// V waits on the gate before its binding reads anything, and answers once the gate is free.
#[test]
fn a_route_b_call_waits_on_a_held_gate_before_it_reads() {
    let s = Setup::new();
    let (want, _) = s.model(vec![(TEST, s.at(BASE))]);
    let before = s.reads.load(SeqCst);
    let gate = s.w.facade.gate().write();
    let reply = route_b(&s, TEST, s.at(BASE));
    wait_until("the route-B call waits on the gate", || waiters(&s.w.facade) == 1);
    assert_eq!(s.reads.load(SeqCst), before, "a source read under the held gate");
    assert!(reply.try_recv().is_none(), "a reply under the held gate");
    drop(gate);
    let got = reply.recv().expect("served");
    assert_eq!(got.as_deref(), Ok(&want[0][..]));
    assert!(s.reads.load(SeqCst) > before, "the call read its inputs");
}

/// B3 (C9): route B's `GetQuorum(type, hash)` at V's tip 60 builds a quorum mined on V's chain
/// from its mined row and the facade's members (the model's roster), and caches it: a second call
/// answers the same object without a source read. A block no mined quorum of the type is based
/// at, and an unknown hash, give none. `ScanQuorums` from the tip answers the mined quorums newest first,
/// taking the cached object, and writes them to the scan cache under its store block, the tip,
/// inside base 48's mining window (58 to 66); from 50, before it, the store block is the previous
/// window's end, 42, where only base 24 is mined. A scan reads the cache under its store block.
#[test]
fn route_b_gets_and_scans_the_quorums_mined_on_vs_chain() {
    let s = Setup::new();
    let (want, _) = s.model(vec![(TEST, s.at(BASE)), (TEST, s.at(CYCLE))]);
    let quorum = |t, h| quorum_b(&s, t, h).recv().expect("served").expect("no fault");

    let q = quorum(TEST, s.quorum_hash(BASE)).expect("mined at 58");
    assert_eq!((q.base, q.base_hash, q.mined_block), (s.at(BASE), s.hash(BASE), s.hash(58)));
    let row = mined_row(TEST, &s.blocks[usize::try_from(BASE - 1).expect("small")]);
    assert_eq!(q.commitment, row.commitment, "the mined row's commitment");
    assert_eq!(pros(&q.members), want[0], "the facade's roster");
    let reads = s.reads.load(SeqCst);
    let again = quorum(TEST, s.quorum_hash(BASE)).expect("mined at 58");
    assert!(Arc::ptr_eq(&again.0, &q.0), "the cached object");
    assert_eq!(s.reads.load(SeqCst), reads, "a cache hit reads no source");
    for (t, h) in [(TEST, s.quorum_hash(36)), (ROTATED, s.quorum_hash(BASE))] {
        assert!(quorum(t, h).is_none(), "{t:?} based at {h}: nothing mined");
    }
    assert!(quorum(TEST, QuorumHash::from([7; 32])).is_none(), "no such block");

    let scan =
        |start, count| scan_b(&s, TEST, start, count).recv().expect("served").expect("no fault");
    let scanned = scan(s.at(TIP), 2);
    assert_eq!(bases(&scanned), [s.at(BASE), s.at(CYCLE)], "newest first");
    assert!(Arc::ptr_eq(&scanned[0].0, &q.0), "the object `GetQuorum` cached");
    assert_eq!(pros(&scanned[1].members), want[1]);
    let cached = |store: u32| {
        let mut quorums = s.w.facade.test_quorums().expect("free");
        quorums.scans.get(TEST, &s.hash(store)).map(|c| bases(c))
    };
    assert_eq!(cached(TIP), Some(vec![s.at(BASE), s.at(CYCLE)]), "the store block's entry");
    assert_eq!(bases(&scan(s.at(50), 2)), [s.at(CYCLE)], "base 48 is mined after 42");
    assert_eq!(cached(42), Some(vec![s.at(CYCLE)]));

    let older = scanned[1].clone();
    s.w.facade.test_quorums().expect("free").scans.insert(TEST, s.hash(TIP), vec![older]);
    let reads = s.reads.load(SeqCst);
    assert_eq!(bases(&scan(s.at(TIP), 1)), [s.at(CYCLE)], "the cache's entry");
    assert_eq!(s.reads.load(SeqCst), reads, "a cache hit reads no source");
}

/// OD-CP-14 M-1: route B's rotated row write fails in the node's guarded engine (an injected I/O
/// failure, as `a_failed_snapshot_write_is_reported_once`), in the build `call` makes. The rows'
/// store reports it as it fails (`write_failed`), so that is the one `AbortNode`: the facade's
/// trip finds the latch tripped, the call answers the read fault `InlineRows` maps it to, nothing
/// is cached, and V reports nothing more, nor as it stops.
fn reported_once(call: impl FnOnce(&Setup) -> Result<(), String>) {
    let p = Arc::new(params(FORKS, FORKS));
    let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
    engine.inject_failure(ErrorKind::Io);
    let sink = Arc::new(RecordingAbort::default());
    let latch = GlobalFailStop::new();
    let db = Arc::new(GuardedEngine::new(engine, latch.clone(), sink.clone()));
    let store = Arc::new(RocksEvoStore::new(db, testkit::codec(), testkit::genesis()));
    let rows = Arc::new(RuntimeRows::new(store, Arc::new(EffectLedger::default())));
    let facade = MembersFacade::new(&p, rows.clone(), latch.clone()).expect("in domain");
    let w = World {
        facade: Arc::new(facade),
        rows,
        store: Arc::default(),
        latch: latch.clone(),
    };
    let mut s = Setup::over(p, w, Some(sink.clone()));
    let got = call(&s);
    let failure = latch.tripped().expect("the guard latched it").clone();
    assert_eq!(failure.site, "chainstate write");
    let text = format!("System error while flushing: {}", failure.message);
    let err = got.expect_err("no answer");
    assert!(err.contains(&text), "{err}");
    assert_eq!(sink.reports(), [(text.clone(), None)], "the store's report");
    assert_eq!(s.w.rows.peek(ROTATED, &s.hash(CYCLE)), None, "a cached row");
    let caches = s.w.facade.test_caches();
    assert!(caches.by_base.cache(ROTATED).is_none_or(|c| c.is_empty()));
    assert!(caches.by_cycle_index.cache(ROTATED).is_none_or(|c| c.is_empty()));
    drop(caches);
    let mut quorums = s.w.facade.test_quorums().expect("free");
    assert!(quorums.objects.get(ROTATED, &s.hash(CYCLE)).is_none(), "a cached quorum");
    assert!(quorums.scans.get(ROTATED, &s.hash(TIP)).is_none(), "a cached scan");
    drop(quorums);
    s.rig.stop();
    assert_eq!(sink.reports(), [(text, None)], "reported once");
}

#[test]
fn a_failed_route_b_write_in_the_guarded_store_is_reported_once() {
    reported_once(|s| route_b(s, ROTATED, s.at(CYCLE)).recv().expect("served").map(|_| ()));
}

/// M-1 through C9's `GetQuorum`: the rotated quorum mined at 36 is built, and its members call
/// writes the row.
#[test]
fn a_failed_route_b_write_through_get_quorum_is_reported_once() {
    reported_once(|s| {
        let got = quorum_b(s, ROTATED, s.quorum_hash(CYCLE)).recv().expect("served");
        got.map(|q| assert!(q.is_none(), "{q:?}"))
    });
}

/// M-1 through C9's `ScanQuorums`: the scan from the tip builds the same quorum, so the scan
/// answers the fault and writes no scan cache.
#[test]
fn a_failed_route_b_write_through_a_scan_is_reported_once() {
    reported_once(|s| {
        let got = scan_b(s, ROTATED, s.at(TIP), 1).recv().expect("served");
        got.map(|q| assert!(q.is_empty(), "{q:?}"))
    });
}
