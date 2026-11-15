use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::Duration;

use dash_clock::Clock;
use dash_consensus::BlockId;
use dash_dml::MnList;
use dash_dml::StoreError;
use dash_dml::creditpool::{CreditPool, CreditPoolInfo, PoolError, PoolState, RangesSet};
use dash_evo::cbtx::historical::{Carrier, CarrierEntry};
use dash_num::Hash256;
use dash_params::LlmqType;
use dash_pkc::bls::{BlsScIetf, BlsSecretKey};
use dash_primitives::QuorumHash;
use dash_types::Numeric as _;
use dashd_chainstate::chain::{
    BlockConnector, BlockStatusWriter, ClSigs, ConnectCx, ConnectOutcome, DisconnectResult,
    EhfRows, EvoReadCx, EvoReads, Fatal, MemBlockFiles, NoEvoReads, NoFlush, NullConnector,
    ProposalChecks, RecordingAbort, ScratchParent, ScratchView, VContext, ViewTarget,
};

use super::*;
use crate::chain::Chain;
use crate::chain::tests::{SharedFiles, branch, fresh, id, imported, regtest, rpc, startup_over};

const BLS: BlsCtx = BlsCtx::BASIC;
/// The fixture's committed scheme: not the regtest height rule's basic.
const COMMITTED: BlsCtx = BlsCtx::LEGACY;

fn block_ref(id: u32, height: u32) -> BlockRef {
    BlockRef {
        id: BlockId(id),
        hash: BlockHash::from([u8::try_from(id).unwrap(); 32]),
        height,
    }
}

fn lock_at(height: i32, hash: BlockHash) -> Arc<ChainLockSig> {
    Arc::new(ChainLockSig {
        height,
        block_hash: hash,
        sig: [0x5a; 96],
    })
}

/// A chain of ids 0..=6 at heights 0..=6, the tip at 6.
fn active(h: u32) -> Option<BlockId> {
    (h <= 6).then_some(BlockId(h))
}

/// `pBlockIndexBestCL` reads the best lock's height on the tip's chain, never its hash, and
/// only without a best lock, or with Core's null one, the tip's coinbase.
// Core: rpc/rawtransaction.cpp:851-864
#[test]
fn the_chainlock_block_is_the_best_locks_height_else_the_coinbases() {
    let tip = block_ref(6, 6);
    let unread = || -> Result<Option<u32>, ()> { panic!("the coinbase is read with a best lock") };
    let other_branch = lock_at(4, BlockHash::from([0xee; 32]));
    assert_eq!(cl_block(Some(&other_branch), unread, &tip, active), Ok(Some(BlockId(4))));
    let above = lock_at(7, BlockHash::from([0xee; 32]));
    assert_eq!(cl_block(Some(&above), unread, &tip, active), Ok(None));
    let at_tip = lock_at(6, tip.hash);
    assert_eq!(cl_block(Some(&at_tip), unread, &tip, active), Ok(Some(tip.id)));
    // A lock at -1 on a nonzero hash is not Core's null one: its ancestor is none.
    let below = lock_at(-1, BlockHash::from([0xee; 32]));
    assert_eq!(cl_block(Some(&below), unread, &tip, active), Ok(None));

    let diff = |d: Option<u32>| move || -> Result<Option<u32>, ()> { Ok(d) };
    assert_eq!(cl_block(None, diff(Some(0)), &tip, active), Ok(Some(BlockId(5))));
    assert_eq!(cl_block(None, diff(Some(2)), &tip, active), Ok(Some(BlockId(3))));
    assert_eq!(cl_block(None, diff(Some(5)), &tip, active), Ok(Some(BlockId(0))));
    assert_eq!(cl_block(None, diff(Some(6)), &tip, active), Ok(None));
    assert_eq!(cl_block(None, diff(Some(u32::MAX)), &tip, active), Ok(None));
    assert_eq!(cl_block(None, diff(None), &tip, active), Ok(None));
    let null = ChainLockSig::null();
    assert_eq!(cl_block(Some(&null), diff(Some(0)), &tip, active), Ok(Some(BlockId(5))));
    assert_eq!(cl_block(None, || Err("store"), &tip, active), Err("store"));
}

/// A source of `ancestors` whose every carrier embeds `entry`.
struct Certified {
    ancestors: Vec<Carrier>,
    entry: CarrierEntry,
}

impl CarrierSource for Certified {
    fn ancestor(&self, height: i32) -> Option<Carrier> {
        self.ancestors.get(usize::try_from(height).ok()?).copied()
    }

    fn load(&mut self, _: &Carrier) -> Result<CarrierEntry, HistoricalClError> {
        Ok(self.entry)
    }

    fn interrupted(&self) -> bool {
        false
    }
}

fn signed_entry() -> CarrierEntry {
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[7; 32]).expect("a key");
    CarrierEntry {
        height_diff: 0,
        signature: key.sign_message(b"bl-689a").to_bytes(),
    }
}

fn certified(chain: &ChainHandle, blocks: &[Arc<Block>]) -> Certified {
    let reader = chain.reader();
    let genesis = reader.ancestor(reader.lookup(&blocks[0].hash()).unwrap(), 0).unwrap();
    let ancestors = std::iter::once(genesis)
        .chain(blocks.iter().map(|b| reader.lookup(&b.hash()).unwrap()))
        .enumerate()
        .map(|(height, id)| Carrier {
            height: i32::try_from(height).unwrap(),
            hash: reader.entry(id).unwrap().hash,
            id,
        })
        .collect();
    Certified {
        ancestors,
        entry: signed_entry(),
    }
}

/// A started V over `files` on `n` blocks of regtest, with the coins attached through
/// `connector`.
fn started(
    files: Box<dyn BlockFiles>,
    connector: Box<dyn BlockConnector>,
    n: u32,
) -> (Chain, Vec<Arc<Block>>, Arc<RecordingAbort>) {
    let p = regtest();
    let genesis = dashd_chainstate::chain::genesis_block(&p);
    let time = i64::from(genesis.header().raw().time()) + 3600;
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(time));
    let abort = Arc::new(RecordingAbort::default());
    let mut chain = Chain::start(fresh(&p, &clock, &abort, files)).expect("V starts");
    startup_over(&mut chain, connector, Box::new(NoFlush));
    imported(&mut chain);
    let blocks = branch(&p, &genesis, 0, n, 0);
    (chain, blocks, abort)
}

fn connect(chain: &Chain, block: &Arc<Block>) {
    let r = chain.handle().process_block(Arc::clone(block), true, None, true).recv();
    assert!(r.expect("V answers").accepted);
}

/// Holds V in a query turn until the returned sender is dropped or sent to; returns once V is
/// held.
fn hold_v(chain: &ChainHandle) -> mpsc::Sender<()> {
    let (release, held) = mpsc::channel::<()>();
    let (entered, inside) = mpsc::channel();
    let _turn = chain.query(move |_| {
        entered.send(()).expect("the test waits");
        let _ = held.recv();
    });
    inside.recv().expect("V runs the turn");
    release
}

/// The found carrier's active-chain membership is read after the search and before
/// `Signed()`: a carrier that left the active chain is ChainChanged, with no signature decoded;
/// a V that no longer answers is the server's abort.
// Core: rpc/quorums.cpp:1442-1451
#[test]
fn a_carrier_off_the_active_chain_is_chain_changed_before_the_signature() {
    let (mut chain, blocks, abort) =
        started(Box::new(MemBlockFiles::default()), Box::new(NullConnector), 6);
    for b in &blocks {
        connect(&chain, b);
    }
    let handle = chain.handle().clone();
    let active =
        |carrier| handle.query(move |q| q.chain.active_contains_private(carrier)).recv().ok();
    let up = Cell::new(false);
    let bls_read = Cell::new(0);
    let bls = || {
        bls_read.set(bls_read.get() + 1);
        BLS
    };

    let found = chainlock_lookup(4, 6, 1, certified(&handle, &blocks), &up, active, bls);
    let want = HistoricalChainLock {
        block_hash: blocks[3].hash(),
        signature: signed_entry().signature,
        carrier_height: 5,
    };
    assert_eq!(found, Ok(want));
    assert_eq!(bls_read.get(), 1);

    let invalidated = handle.invalidate_block(blocks[4].hash()).recv().expect("V answers");
    assert_eq!(invalidated, Ok(()));
    let source = certified(&handle, &blocks);
    assert_eq!(source.ancestor(5).map(|c| c.hash), Some(blocks[4].hash()), "still readable");
    let changed = chainlock_lookup(4, 6, 1, source, &up, active, bls);
    assert_eq!(changed, Err(ClLookupError::ChainChanged));
    let gone = chainlock_lookup(4, 6, 1, certified(&handle, &blocks), &up, |_| None, bls);
    assert_eq!(gone, Err(ClLookupError::Failed(aborted())));
    assert_eq!(bls_read.get(), 1, "no signature decoded off the active chain");

    chain.stop();
    assert!(abort.messages().is_empty(), "{:?}", abort.messages());
}

/// Each carrier's position is read in a V query turn: a load waits while V is held. Its block
/// is then read from the files: a version-1 coinbase is no CbTx, so the read block is
/// `InvalidCoinbase`, and without files it is unavailable. The fixed tip's ancestry stays
/// readable after it leaves the active chain.
// Core: rpc/quorums.cpp:1437-1441; node/blockstorage.cpp:833-846; chainlock/clsig.cpp:128-150
#[test]
fn each_carrier_position_is_read_in_a_v_turn() {
    let files = SharedFiles::default();
    let (mut chain, blocks, abort) = started(Box::new(files.clone()), Box::new(NullConnector), 6);
    for b in &blocks {
        connect(&chain, b);
    }
    let handle = chain.handle().clone();
    let tip = id(&handle, &blocks[5]);
    let block_files: Mutex<Box<dyn BlockFiles>> = Mutex::new(Box::new(files));
    let up = Cell::new(false);
    let mut source = NodeCarriers {
        chain: &handle,
        tip,
        files: Some(&block_files),
        v_gone: &up,
    };
    let at = |source: &NodeCarriers<'_>, h| source.ancestor(h).expect("an ancestor");
    assert_eq!(at(&source, 3).hash, blocks[2].hash());
    assert_eq!(source.ancestor(7), None);
    assert_eq!(source.ancestor(-1), None);
    let carrier = at(&source, 3);
    assert_eq!(source.load(&carrier), Err(HistoricalClError::InvalidCoinbase));

    let release = hold_v(&handle);
    std::thread::scope(|scope| {
        let (done, loaded) = mpsc::channel();
        let (chain, files) = (&handle, &block_files);
        scope.spawn(move || {
            let up = Cell::new(false);
            let mut source = NodeCarriers {
                chain,
                tip,
                files: Some(files),
                v_gone: &up,
            };
            done.send(source.load(&carrier)).expect("the test waits");
        });
        let waited = loaded.recv_timeout(Duration::from_millis(300));
        assert!(waited.is_err(), "the position waits for V: {waited:?}");
        release.send(()).expect("V is held");
        let load = loaded.recv_timeout(Duration::from_secs(60)).expect("V released");
        assert_eq!(load, Err(HistoricalClError::InvalidCoinbase));
    });

    let invalidated = handle.invalidate_block(blocks[3].hash()).recv().expect("V answers");
    assert_eq!(invalidated, Ok(()));
    let off = at(&source, 6);
    assert_eq!(off.hash, blocks[5].hash());
    assert_eq!(source.load(&off), Err(HistoricalClError::InvalidCoinbase), "still read");
    let mut unfiled = NodeCarriers {
        files: None,
        ..source
    };
    assert_eq!(unfiled.load(&carrier), Err(HistoricalClError::Unavailable));
    assert!(!up.get());

    let reconsidered = handle.reconsider_block(blocks[3].hash(), false).recv().expect("V answers");
    assert_eq!(reconsidered, Ok(()));
    let invalid = HistoricalClError::InvalidCoinbase.to_string();
    let lookup = chainlock_lookup(2, 6, 1, source, &up, |_| Some(true), || BLS);
    assert_eq!(lookup, Err(ClLookupError::Failed(invalid)), "carrier 3 is read first");
    // Regtest's V20 is above this chain, so the node's own lookup reads no carrier.
    let none = node_chainlock_by_height(&handle, Some(&block_files), 2);
    assert_eq!(none, Err(ClLookupError::NotFound));

    chain.stop();
    assert!(abort.messages().is_empty(), "{:?}", abort.messages());
}

/// A V that answers no more during the search is the server's abort, not missing block data:
/// the carrier's position turn has no answer.
// Core: rpc/server.cpp (the server's abort text, RP1a D9)
#[test]
fn a_v_gone_during_the_search_is_the_servers_abort() {
    let files = SharedFiles::default();
    let (mut chain, blocks, _abort) = started(Box::new(files.clone()), Box::new(NullConnector), 6);
    for b in &blocks {
        connect(&chain, b);
    }
    let handle = chain.handle().clone();
    let tip = id(&handle, &blocks[5]);
    chain.stop();
    let block_files: Mutex<Box<dyn BlockFiles>> = Mutex::new(Box::new(files));
    let gone = Cell::new(false);
    let mut source = NodeCarriers {
        chain: &handle,
        tip,
        files: Some(&block_files),
        v_gone: &gone,
    };
    let carrier = source.ancestor(3).expect("the index outlives V");
    assert_eq!(source.load(&carrier), Err(HistoricalClError::Unavailable));
    assert!(gone.get());
    gone.set(false);
    let lookup = chainlock_lookup(2, 6, 1, source, &gone, |_| Some(true), || BLS);
    assert_eq!(lookup, Err(ClLookupError::Failed(aborted())));
    let lookup = node_chainlock_by_height(&handle, Some(&block_files), 2);
    assert_eq!(lookup, Err(ClLookupError::Failed(aborted())), "the tip turn too");
}

/// One evo read V's turn made: a pool after the block at a height, or the tip coinbase's lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
    Pool(u32),
    Coinbase(u32, BlsCtx),
}

/// Evo reads from tables: an index is in the pool after every block from its unlock height;
/// each height's coinbase embeds the lock of its table row. Every read is logged.
struct FixtureReads {
    unlocks: Vec<(u64, u32)>,
    coinbase: BTreeMap<u32, u32>,
    log: Arc<Mutex<Vec<Read>>>,
    probe: Arc<Mutex<Probe>>,
}

/// A test's hold on [`FixtureReads`]' pool reads: `hook` runs in the first one after it is set;
/// each read logs whether V's index holds `watch` by then.
#[derive(Default)]
struct Probe {
    hook: Option<Box<dyn FnOnce() + Send>>,
    watch: Option<BlockHash>,
    seen: Vec<bool>,
}

impl EvoReads for FixtureReads {
    fn list_at(&self, cx: EvoReadCx<'_>, at: BlockId) -> Result<Arc<MnList>, StoreError> {
        NoEvoReads.list_at(cx, at)
    }

    fn credit_pool_after(
        &self,
        cx: EvoReadCx<'_>,
        at: BlockId,
    ) -> Result<Option<Arc<PoolState>>, StoreError> {
        let height = cx.reader.entry(at).expect("resident").height;
        self.log.lock().unwrap().push(Read::Pool(height));
        let mut probe = self.probe.lock().unwrap();
        if let Some(watch) = probe.watch {
            let seen = cx.reader.lookup(&watch).is_some();
            probe.seen.push(seen);
        }
        if let Some(hook) = probe.hook.take() {
            hook();
        }
        let mut indexes = RangesSet::default();
        for (index, _) in self.unlocks.iter().filter(|(_, h)| *h <= height) {
            indexes.add(*index);
        }
        Ok(Some(Arc::new(Ok(CreditPool {
            indexes,
            ..CreditPool::default()
        }))))
    }

    fn credit_pool_info(
        &self,
        cx: EvoReadCx<'_>,
        at: BlockId,
        window_blocks: i32,
    ) -> Result<Result<CreditPoolInfo, PoolError>, StoreError> {
        NoEvoReads.credit_pool_info(cx, at, window_blocks)
    }

    fn has_mined_commitment(
        &self,
        cx: EvoReadCx<'_>,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        tip: BlockId,
    ) -> Result<bool, StoreError> {
        NoEvoReads.has_mined_commitment(cx, llmq_type, quorum_hash, tip)
    }

    fn coinbase_chainlock_at(
        &self,
        cx: EvoReadCx<'_>,
        at: BlockId,
        bls: BlsCtx,
    ) -> Result<Option<u32>, StoreError> {
        let height = cx.reader.entry(at).expect("resident").height;
        self.log.lock().unwrap().push(Read::Coinbase(height, bls));
        Ok(self.coinbase.get(&height).copied())
    }
}

/// [`NullConnector`]'s connects, publishing [`COMMITTED`] as the committed scheme, with
/// [`FixtureReads`] as V's evo reads.
struct FixtureConnector(FixtureReads);

impl BlockConnector for FixtureConnector {
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
        Some(COMMITTED)
    }

    fn evo_reads(&self) -> &dyn EvoReads {
        &self.0
    }

    fn ehf_rows(&self) -> Option<Arc<dyn EhfRows>> {
        NullConnector.ehf_rows()
    }

    fn verify_scratch(
        &mut self,
        parent: ScratchParent,
    ) -> Result<Option<Box<dyn ScratchView>>, Fatal> {
        NullConnector.verify_scratch(parent)
    }

    fn take_scratch_refusal(&mut self) -> Option<String> {
        NullConnector.take_scratch_refusal()
    }

    fn take_undo_fault(&mut self) -> Option<Fatal> {
        NullConnector.take_undo_fault()
    }
}

/// The tip's height in every fixture.
const T: u32 = 6;

/// One fixture: a real V on `T` blocks over [`FixtureReads`], the node's command table on it,
/// and the block `T + 1` not yet connected.
struct Fixture {
    chain: Chain,
    blocks: Vec<Arc<Block>>,
    slot: Arc<ChainSlot>,
    table: Arc<RpcServer>,
    log: Arc<Mutex<Vec<Read>>>,
    probe: Arc<Mutex<Probe>>,
    abort: Arc<RecordingAbort>,
}

impl Fixture {
    fn new(unlocks: &[(u64, u32)], coinbase: &[(u32, u32)]) -> Fixture {
        let log = Arc::new(Mutex::new(Vec::new()));
        let probe = Arc::new(Mutex::new(Probe::default()));
        let reads = FixtureReads {
            unlocks: unlocks.to_vec(),
            coinbase: coinbase.iter().copied().collect(),
            log: Arc::clone(&log),
            probe: Arc::clone(&probe),
        };
        let files = Box::new(MemBlockFiles::default());
        let (chain, blocks, abort) = started(files, Box::new(FixtureConnector(reads)), T + 1);
        for b in &blocks[..T as usize] {
            connect(&chain, b);
        }
        let p = regtest();
        let slot = Arc::new(ChainSlot::default());
        slot.attach(chain.handle(), chain.signals(), None, None, None);
        let clock = Arc::new(MockClock::at(1_296_688_662 + 3600));
        let table = command_table(
            &p,
            &Arc::new(Logger::new()),
            &Arc::new(ShutdownState::new()),
            &clock,
            (true, &Scheduler::new()),
            &slot,
            &Arc::default(),
            &Arc::default(),
            &Arc::default(),
            Default::default(),
            std::env::temp_dir(),
        );
        table.start();
        table.set_warmup_finished();
        log.lock().unwrap().clear();
        Fixture {
            chain,
            blocks,
            slot,
            table,
            log,
            probe,
            abort,
        }
    }

    /// The block at `height` on the fixture's chain.
    fn block(&self, height: u32) -> &Block {
        &self.blocks[height as usize - 1]
    }

    /// `UpdateBestChainlock` for a lock on `hash` at `height`, then the reads it made dropped.
    fn record(&self, height: u32, hash: BlockHash) {
        self.record_sig(lock_at(i32::try_from(height).unwrap(), hash));
    }

    /// `UpdateBestChainlock` for `sig`, then the reads it made dropped.
    fn record_sig(&self, sig: Arc<ChainLockSig>) {
        let hash = sig.block_hash;
        let mut b = *hash.as_bytes();
        b[0] ^= 0x5a;
        let sig_hash = Hash256::from_lendian(b);
        let reply = self.chain.handle().record_chainlock(sig, sig_hash, None).recv();
        assert!(reply.is_ok(), "V records the lock");
        // The publish after the lock reads the tip's pool for the view, not for the RPC.
        self.chain.handle().query(|_| ()).recv().expect("V answers");
        self.take_reads();
    }

    /// `getassetunlockstatuses` for `indexes`, at `height` if given: each index's status.
    fn statuses(&self, indexes: &[u64], height: Option<u32>) -> Vec<&'static str> {
        let list: Vec<String> = indexes.iter().map(|i| format!(r#""{i}""#)).collect();
        let params = match height {
            Some(h) => format!("[[{}], {h}]", list.join(",")),
            None => format!("[[{}]]", list.join(",")),
        };
        let reply = rpc(&self.table, "getassetunlockstatuses", &params).expect("statuses");
        let parsed = dashd_rpc::UniValue::read(reply.as_bytes()).expect("JSON");
        let mut out = Vec::new();
        for (i, row) in parsed.get_values().expect("an array").iter().enumerate() {
            let want = format!(r#"{{"index":{},"status":"#, indexes[i]);
            let written = row.write();
            assert!(written.starts_with(&want), "{written}");
            let status = ["chainlocked", "mined", "unknown"]
                .into_iter()
                .find(|s| written == format!(r#"{want}"{s}"}}"#))
                .unwrap_or_else(|| panic!("an exact status row: {written}"));
            out.push(status);
        }
        out
    }

    fn take_reads(&self) -> Vec<Read> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }

    /// V's lock state and the published view's: no best lock, no known one, the zero hash.
    fn assert_no_best_lock(&self) {
        fn unset(cl: &ClSigs) -> bool {
            cl.best.is_none() && cl.best_known.is_none() && cl.best_hash == Hash256::ZERO
        }
        let v = self.chain.handle().query(|q| unset(q.cl)).recv().expect("V answers");
        assert!(v, "V's ClSigs");
        assert!(unset(&self.chain.handle().view_handle().cl()), "the published ClSigs");
        assert!(self.chain.handle().view().best_chainlock().is_none(), "the published view");
        let none = r#"{"code":-32603,"message":"Unable to find any ChainLock"}"#.to_owned();
        assert_eq!(rpc(&self.table, "getbestchainlock", "[]"), Err((-32603, none)));
    }

    fn finish(mut self) {
        self.slot.detach();
        self.table.interrupt();
        self.chain.stop();
        assert!(self.abort.messages().is_empty(), "{:?}", self.abort.messages());
    }
}

/// Whether Core's answer with a ChainLock block at `l_d` and rdashd's at `l_r` (-1: none) differ
/// for an index unlocked at `h` (`None`: never) on a tip at [`T`].
fn interval_differs(l_d: i64, l_r: i64, h: Option<u32>) -> bool {
    h.is_some_and(|h| l_d.min(l_r) < i64::from(h) && i64::from(h) <= l_d.max(l_r))
}

/// The best lock each case records, by height, on its own fresh fixture.
#[derive(Clone, Copy, Debug)]
enum Best {
    Unset,
    At(u32),
    UnknownAbove,
}

/// AD-BL689a-2: rdashd has no best ChainLock (until LK1a), so it takes Core's own null-lock
/// path, the tip coinbase's lock; a fixture with a best lock recorded is Core's answer with
/// one. The two differ exactly for an index unlocked above the lower of the two lock heights
/// and at or below the higher; with a height every case agrees; no best lock is ever derived.
// Core: rpc/rawtransaction.cpp:827-899
#[test]
fn ad_bl689a_2_no_best_chainlock_takes_the_coinbase_path() {
    const C: u32 = T - 1;
    // Fixture B: the tip's coinbase certifies C (diff 0); a, b, d unlocked at T, C, C - 1.
    let b_unlocks = [(101, T), (102, C), (103, C - 1)];
    let b_coinbase = [(T, 0), (T + 1, 0)];
    let b_indexes = [101, 102, 103, 999];
    let b_heights = [Some(T), Some(C), Some(C - 1), None];
    let b_cases: [(Best, [&str; 4]); 5] = [
        (Best::Unset, ["mined", "chainlocked", "chainlocked", "unknown"]),
        (Best::At(T), ["chainlocked", "chainlocked", "chainlocked", "unknown"]),
        (Best::At(C), ["mined", "chainlocked", "chainlocked", "unknown"]),
        (Best::At(C - 1), ["mined", "mined", "chainlocked", "unknown"]),
        (Best::UnknownAbove, ["mined", "mined", "mined", "unknown"]),
    ];
    // Fixture A: no certificate at the tip; x, y unlocked at T - 2 and T.
    let a_unlocks = [(201, T - 2), (202, T)];
    let a_coinbase = [(T + 1, 0)];
    let a_indexes = [201, 202, 999];
    let a_heights = [Some(T - 2), Some(T), None];
    let a_cases: [(Best, [&str; 3]); 2] = [
        (Best::Unset, ["mined", "mined", "unknown"]),
        (Best::At(T), ["chainlocked", "chainlocked", "unknown"]),
    ];

    run_fixture(&b_unlocks, &b_coinbase, i64::from(C), &b_indexes, &b_heights, &b_cases);
    run_fixture(&a_unlocks, &a_coinbase, -1, &a_indexes, &a_heights, &a_cases);
}

/// Each case on its own fixture: the no-height answer, the reads behind it, the interval rule
/// against the unset case, and every height's answer, equal across cases. The unset case then
/// shows no best lock is written through repeated calls and a tip advance whose coinbase
/// carries a newer lock.
fn run_fixture<const N: usize>(
    unlocks: &[(u64, u32)],
    coinbase: &[(u32, u32)],
    l_r: i64,
    indexes: &[u64; N],
    unlocked_at: &[Option<u32>; N],
    cases: &[(Best, [&str; N])],
) {
    let unset = cases[0].1;
    let mut by_height: Option<Vec<Vec<&str>>> = None;
    for (best, want) in cases {
        let f = Fixture::new(unlocks, coinbase);
        let l_d = match best {
            Best::Unset => None,
            Best::At(h) => {
                f.record(*h, f.block(*h).hash());
                Some(i64::from(*h))
            }
            Best::UnknownAbove => {
                f.record(T + 1, BlockHash::from([0x77; 32]));
                Some(-1)
            }
        };
        assert_eq!(f.statuses(indexes, None), want, "{best:?}");
        let reads = f.take_reads();
        let cl_height = match best {
            Best::Unset => u32::try_from(l_r).ok(),
            Best::At(h) => Some(*h),
            Best::UnknownAbove => None,
        };
        let mut want_reads = Vec::new();
        if matches!(best, Best::Unset) {
            want_reads.push(Read::Coinbase(T, COMMITTED));
        }
        want_reads.extend(cl_height.map(Read::Pool));
        if cl_height != Some(T) {
            want_reads.push(Read::Pool(T));
        }
        assert_eq!(reads, want_reads, "{best:?}: the coinbase only without a best lock");

        if let Some(l_d) = l_d {
            for (i, h) in unlocked_at.iter().enumerate() {
                let differs = want[i] != unset[i];
                assert_eq!(
                    differs,
                    interval_differs(l_d, l_r, *h),
                    "{best:?} index {}",
                    indexes[i]
                );
                if differs {
                    let (rdashd, core) = if l_d > l_r {
                        ("mined", "chainlocked")
                    } else {
                        ("chainlocked", "mined")
                    };
                    assert_eq!(
                        (unset[i], want[i]),
                        (rdashd, core),
                        "{best:?} index {}",
                        indexes[i]
                    );
                }
            }
        }

        let heights: Vec<Vec<&str>> = (0..=T).map(|h| f.statuses(indexes, Some(h))).collect();
        for (h, answer) in heights.iter().enumerate() {
            for (i, at) in unlocked_at.iter().enumerate() {
                let locked = at.is_some_and(|at| at as usize <= h);
                assert_eq!(
                    answer[i],
                    if locked {
                        "chainlocked"
                    } else {
                        "unknown"
                    },
                    "{h}"
                );
            }
        }
        match &by_height {
            Some(first) => assert_eq!(&heights, first, "{best:?}: a height's answer"),
            None => by_height = Some(heights),
        }

        if matches!(best, Best::Unset) {
            f.assert_no_best_lock();
            // Core's null lock is stale against no best lock: nothing records it.
            f.record_sig(ChainLockSig::null_arc());
            f.assert_no_best_lock();
            assert_eq!(f.statuses(indexes, None), *want, "after a null lock");
            for _ in 0..3 {
                assert_eq!(f.statuses(indexes, None), *want);
            }
            f.assert_no_best_lock();
            connect(&f.chain, &f.blocks[T as usize]);
            let advanced: Vec<&str> = unlocked_at
                .iter()
                .map(|h| {
                    if h.is_some() {
                        "chainlocked"
                    } else {
                        "unknown"
                    }
                })
                .collect();
            for _ in 0..2 {
                assert_eq!(f.statuses(indexes, None), advanced, "{best:?} at T + 1");
            }
            f.assert_no_best_lock();
        }
        f.finish();
    }
}

/// `Writer::asset_unlock_pools` reads in one V turn: a block sent to V during its first pool
/// read connects only after the call, so both pools are the state before it, and the call
/// answers as on that state; then V connects it.
// Core: rpc/rawtransaction.cpp:827-876
#[test]
fn the_unlock_pools_are_one_v_turn() {
    const C: u32 = T - 1;
    let f = Fixture::new(&[(101, T), (102, C), (103, C - 1)], &[(T, 0), (T + 1, 0)]);
    let next = Arc::clone(&f.blocks[T as usize]);
    let handle = f.chain.handle().clone();
    let (sent, reply) = mpsc::channel();
    {
        let mut probe = f.probe.lock().unwrap();
        probe.watch = Some(next.hash());
        probe.hook = Some(Box::new(move || {
            let r = handle.process_block(next, true, None, true);
            sent.send(r).expect("the test waits");
        }));
    }
    let indexes = [101, 102, 103, 999];
    assert_eq!(f.statuses(&indexes, None), ["mined", "chainlocked", "chainlocked", "unknown"]);
    let block = reply.recv().expect("the hook ran").recv().expect("V answers");
    assert!(block.accepted, "V connects it after the call");
    // The call's reads, then the connect's own.
    let reads = f.take_reads();
    assert_eq!(reads[..3], [Read::Coinbase(T, COMMITTED), Read::Pool(C), Read::Pool(T)]);
    let seen = f.probe.lock().unwrap().seen.clone();
    assert_eq!(seen[..2], [false, false], "both pools before the block: {seen:?}");
    let tip = f.chain.handle().query(|q| q.chain.private_tip().map(|(t, _)| t.height));
    assert_eq!(tip.recv().expect("V answers"), Some(T + 1));
    f.finish();
}

/// A best lock naming a block V indexed off the active chain, at another height, is resolved by
/// its height on the tip's chain: the active block there, never the named one.
// Core: rpc/rawtransaction.cpp:851-855; chainlock/chainlock.cpp:115-125
#[test]
fn a_best_lock_on_another_branch_is_the_active_block_at_its_height() {
    const H: u32 = T - 1;
    const SIDE: u32 = 3;
    let f = Fixture::new(&[(301, SIDE), (302, SIDE + 1), (303, H), (304, T)], &[]);
    let genesis = dashd_chainstate::chain::genesis_block(&regtest());
    let side = branch(&regtest(), &genesis, 0, SIDE, 1);
    for b in &side {
        connect(&f.chain, b);
    }
    let named = side[SIDE as usize - 1].hash();
    let handle = f.chain.handle().clone();
    let placed = handle.query(move |q| {
        let id = q.chain.reader().lookup(&named).expect("indexed");
        let height = q.chain.reader().entry(id).expect("resident").height;
        (height, q.chain.active_contains_private(id), q.chain.private_tip().map(|(t, _)| t.height))
    });
    assert_eq!(placed.recv().expect("V answers"), (SIDE, false, Some(T)));
    assert_ne!(f.block(H).hash(), named);
    f.take_reads();

    f.record(H, named);
    let sigs = handle.query(|q| (q.cl.best.clone(), q.cl.best_known.is_none())).recv();
    let (best, unknown) = sigs.expect("V answers");
    assert_eq!(best.map(|b| (b.height, b.block_hash)), Some((i32::try_from(H).unwrap(), named)));
    assert!(unknown, "a height mismatch moves no floor");

    let indexes = [301, 302, 303, 304];
    assert_eq!(f.statuses(&indexes, None), ["chainlocked", "chainlocked", "chainlocked", "mined"]);
    assert_eq!(f.take_reads(), [Read::Pool(H), Read::Pool(T)]);
    f.finish();
}
