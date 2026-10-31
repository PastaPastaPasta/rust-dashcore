//! CH4-4 (DEC-835): the connect read-ahead on the connector harness. Each scenario builds a
//! chain, wipes its coins and reconnects it from genesis: `-reindex-chainstate`'s shape, without
//! CH5's flag. It runs once with the read-ahead off, then once per read-ahead mode: settled (the
//! reader reads all it can before each take, so every run reads the same blocks) and
//! free-running, each with xcheck and without. Every run must log the same StateDigest, send
//! the same signals in the same order and stop on the same `AbortNode`, including with faults
//! planted in the block files.

use std::io::{self, Write};

use dash_chain::{BlockRef, FlatFilePos};

use super::event_order::Order;
use super::*;
use crate::chain::read_ahead::{Counts, DEPTH, Probe};
use crate::chain::{BlockFiles as ChainFiles, BlockSource, ReadAheadSetup};

/// The chain's length: more than the read-ahead's depth.
const N: u32 = 100;
/// The block a scenario plants its fault at: [`DEPTH`] blocks before the chain's end and more, so
/// the reader can fill its queue past it.
const K: u32 = 30;

/// A fault in the block files at one block.
#[derive(Clone)]
enum Fault {
    /// Every read of the block fails.
    Unreadable(BlockHash),
    /// The read-ahead's read of the block fails; V's succeeds.
    ReaderOnly(BlockHash),
    /// Every read of the block returns this block, which has its hash.
    Corrupt(BlockHash, Arc<Block>),
    /// The read-ahead's read of the block returns this block, which has its hash.
    ReaderCorrupt(BlockHash, Arc<Block>),
}

/// The rig's block files with `fault`.
#[derive(Clone)]
struct Faulty {
    files: SharedFiles,
    fault: Option<Fault>,
}

impl Faulty {
    fn read(&self, pos: FlatFilePos, hash: &BlockHash, ahead: bool) -> Result<Arc<Block>, String> {
        match &self.fault {
            Some(Fault::Unreadable(h)) if h == hash => Err(format!("{hash} is unreadable")),
            Some(Fault::ReaderOnly(h)) if ahead && h == hash => {
                Err(format!("{hash} is unreadable ahead"))
            }
            Some(Fault::Corrupt(h, b)) if h == hash => Ok(b.clone()),
            Some(Fault::ReaderCorrupt(h, b)) if ahead && h == hash => Ok(b.clone()),
            _ => ChainFiles::read_block(&self.files, pos, hash),
        }
    }
}

impl ChainFiles for Faulty {
    fn save_block(
        &mut self,
        block: &Arc<Block>,
        height: u32,
        known: Option<FlatFilePos>,
    ) -> Result<Option<FlatFilePos>, String> {
        self.files.save_block(block, height, known)
    }

    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        self.read(pos, hash, false)
    }

    fn open_block_file(&self, file: i32) -> bool {
        self.files.open_block_file(file)
    }

    fn current_usage(&self) -> u64 {
        self.files.current_usage()
    }

    fn scan_already_pruned_files(&mut self, have_pruned: bool) -> Result<(), String> {
        self.files.scan_already_pruned_files(have_pruned)
    }

    fn read_ahead(&self) -> Option<Arc<dyn BlockSource>> {
        Some(Arc::new(Ahead(self.clone())))
    }
}

/// [`Faulty`]'s reads on the read-ahead's thread.
struct Ahead(Faulty);

impl BlockSource for Ahead {
    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        self.0.read(pos, hash, true)
    }
}

/// A StateDigest log into `log` from `genesis`, the wiped coins' block.
#[cfg(feature = "statedigest")]
fn digest(log: &Buf, genesis: BlockId) -> Option<Box<dyn DigestSink>> {
    use dash_consensus::coins::MuHash3072;
    use dash_consensus::coins::stats::{CoinStatsAccumulator, CoinStatsTotals};
    use dash_statedigest::{Chain, FileKind, Header, Writer};

    let totals = CoinStatsAccumulator::from_parts(MuHash3072::new(), CoinStatsTotals::default());
    let header = Header {
        file: FileKind::Digest,
        src: Writer::Rdashd,
        ver: b"test".to_vec(),
        chain: Chain::Regtest,
        run: b"1-1".to_vec(),
    };
    let log = crate::connect::DigestLog::seeded(log.clone(), header, totals, genesis);
    // A finalize at every block would be most of a debug run's time.
    let at = crate::connect::MuHashAt::Only([K - 1, N].into());
    Some(Box::new(log.expect("the H is written").with_muhash_at(at)))
}

/// Without the StateDigest, the runs compare everything else.
#[cfg(not(feature = "statedigest"))]
fn digest(_: &Buf, _: BlockId) -> Option<Box<dyn DigestSink>> {
    None
}

/// The digest log's bytes.
#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("log").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// What a reconnect left, which must not depend on the read-ahead.
#[derive(Debug, PartialEq, Eq)]
struct Run {
    digest: Vec<u8>,
    sync: Vec<String>,
    queued: Vec<String>,
    reports: Vec<String>,
    fatal: Option<String>,
    coins: Option<BlockHash>,
}

/// A scenario: the chain, a branch off it, the seeds of the build and of the reconnect.
struct Scenario {
    p: Arc<ChainParams>,
    blocks: Vec<Mined>,
    branch: Vec<Mined>,
    built: Vec<(OutPoint, TxOut, u32, bool)>,
    seeds: Vec<(OutPoint, TxOut, u32, bool)>,
    fault: Option<Fault>,
    stop_at: i64,
    aborts: bool,
}

impl Scenario {
    /// Blocks 1–[`N`] on genesis, each spending its parent's output (block 1 the seed), with
    /// `extra` spent by block [`K`] too.
    fn new(extra: Option<OutPoint>) -> Scenario {
        let p = regtest();
        let mut parent = Mined::genesis(&p);
        let mut prev = seed_op(1);
        let mut blocks = Vec::new();
        for h in 1..=N {
            let mut inputs = vec![prev];
            inputs.extend(extra.filter(|_| h == K));
            let t = spend(&inputs, 50 * COIN - Amount::from(h) * 1_000);
            prev = op(&t, 0);
            let m = block_on(&p, &parent, vec![t]);
            parent = m.clone();
            blocks.push(m);
        }
        let seeds = vec![seed(1, 50 * COIN, &[OP_TRUE])];
        Scenario {
            p,
            blocks,
            branch: Vec::new(),
            built: seeds.clone(),
            seeds,
            fault: None,
            stop_at: 0,
            aborts: false,
        }
    }

    fn hash(&self, h: u32) -> BlockHash {
        self.blocks[h as usize - 1].hash()
    }

    /// The build: every block accepted and connected, then the branch stored beside it.
    fn build(&self) -> (VExit, Stores, BlockRef) {
        let mut w = world(Setup {
            params: Some(self.p.clone()),
            seeds: self.built.clone(),
            ..Setup::default()
        });
        let genesis = w.coins.lock().expect("coins").best_block().expect("genesis");
        for m in &self.blocks {
            w.rig.accept(m);
        }
        for m in &self.branch {
            w.rig.submit(m);
        }
        let exit = w.rig.stop();
        assert_eq!(exit.fatal, None);
        (exit, w.stores.clone(), genesis)
    }

    /// The build, its coins wiped back to genesis, reconnected with `read_ahead`.
    fn run(&self, read_ahead: ReadAheadSetup) -> Run {
        let (exit, stores, genesis) = self.build();
        let wiped = Stores {
            coins: Stores::new().coins,
            ..stores
        };
        BlockOverlay::new(&mut wiped.coins.lock().expect("coins")).commit(genesis).expect("wiped");
        let log = Buf::default();
        let order = Arc::new(Order::default());
        let (mut cfg, stores, sink, _) = node(Setup {
            params: Some(self.p.clone()),
            stores: Some(wiped),
            seeds: self.seeds.clone(),
            digest: digest(&log, genesis.id),
            node_stop: true,
            listener: Some(order.clone()),
            read_ahead,
            ..Setup::default()
        });
        cfg.stop_at_height = self.stop_at;
        cfg.aborts_before_init = self.aborts;
        let files = Faulty {
            files: stores.blocks.clone(),
            fault: self.fault.clone(),
        };
        let mut rig = Rig::restart_with(exit, Box::new(files), genesis.id, cfg).expect("V starts");
        let fatal = rig.stop().fatal.map(|f| f.0);
        // The signals thread runs the queue out as the rig drops.
        drop(rig);
        let digest = log.0.lock().expect("log").clone();
        let (sync, queued) = order.take();
        Run {
            digest,
            sync,
            queued,
            reports: sink.messages(),
            fatal,
            coins: coins_at(&stores.coins),
        }
    }

    /// The run with the read-ahead off, then each read-ahead mode's, which must be the same:
    /// the off run, and the settled run's reader counts, which xcheck does not change.
    fn parity(&self) -> (Run, Counts) {
        let off = self.run(ReadAheadSetup::off());
        let settled = |xcheck| {
            let probe = Probe::settled();
            let run = self.run(ReadAheadSetup::threaded(xcheck).probed(&probe));
            assert_same(&run, &off, &format!("settled, xcheck {xcheck}"));
            probe.counts()
        };
        let counts = settled(false);
        assert_eq!(settled(true), counts, "xcheck changes no read");
        for xcheck in [false, true] {
            let free = self.run(ReadAheadSetup::threaded(xcheck));
            assert_same(&free, &off, &format!("free-running, xcheck {xcheck}"));
        }
        (off, counts)
    }
}

/// `got` is `want`, field by field.
fn assert_same(got: &Run, want: &Run, mode: &str) {
    assert_eq!(got.fatal, want.fatal, "{mode}: the AbortNode");
    assert_eq!(got.reports, want.reports, "{mode}: the reports");
    assert_eq!(got.sync, want.sync, "{mode}: the synchronous signals");
    assert_eq!(got.queued, want.queued, "{mode}: the queued signals");
    assert_eq!(got.coins, want.coins, "{mode}: the coins' block");
    assert!(got.digest == want.digest, "{mode}: the StateDigest differs");
}

/// The blocks connected in `run`, in order.
fn connected(run: &Run) -> usize {
    run.queued.iter().filter(|e| e.starts_with("connected")).count()
}

/// A reconnect from genesis reads every block ahead: the same StateDigest and signals as V's own
/// reads, with each block read once and taken, and the queue full at the depth.
#[test]
fn a_reconnect_from_genesis_reads_each_block_ahead_once() {
    let s = Scenario::new(None);
    let (off, counts) = s.parity();
    assert_eq!(off.fatal, None);
    assert_eq!(connected(&off), N as usize);
    assert_eq!(off.coins, Some(s.hash(N)));
    assert_eq!(
        counts,
        Counts {
            reads: N,
            hits: N,
            fallbacks: 0,
            epochs: 0,
            max_ready: DEPTH as usize,
        }
    );
}

/// A block no read can read is Core's `AbortNode("Failed to read block")` at that block
/// (`v:3416-3418`): the read-ahead's failure leaves the read to V.
#[test]
fn an_unreadable_block_aborts_at_its_block() {
    let mut s = Scenario::new(None);
    s.fault = Some(Fault::Unreadable(s.hash(K)));
    s.aborts = true;
    let (off, counts) = s.parity();
    assert_eq!(off.fatal.as_deref(), Some("Failed to read block"));
    assert_eq!(off.reports, ["Failed to read block"]);
    assert_eq!(off.coins, Some(s.hash(K - 1)));
    assert_eq!((counts.hits, counts.fallbacks), (K - 1, 1));
}

/// A read only the read-ahead fails is V's own read: the reconnect goes on as without it.
#[test]
fn a_read_ahead_failure_falls_back_to_vs_read() {
    let mut s = Scenario::new(None);
    s.fault = Some(Fault::ReaderOnly(s.hash(K)));
    let (off, counts) = s.parity();
    assert_eq!(off.fatal, None);
    assert_eq!(off.coins, Some(s.hash(N)));
    assert_eq!((counts.reads, counts.hits, counts.fallbacks), (N, N - 1, 1));
}

/// The block at `K` with its last transaction twice: the same header (CVE-2012-2459), so the
/// same hash, and `CheckBlock` fails it as mutated.
fn mutated(s: &Scenario) -> Arc<Block> {
    let b = &s.blocks[K as usize - 1].block;
    let mut txs = b.txs().to_vec();
    txs.push(txs.last().expect("a transaction").clone());
    let m = Arc::new(Block::new(*b.header(), txs));
    assert_eq!(m.hash(), b.hash());
    m
}

/// A stored block `CheckBlock` fails, read back mutated, is Core's corruption `AbortNode` at that
/// block (`v:2505-2514`): the reader's check fails it, and `ConnectBlock`'s own check meets it.
#[test]
fn a_block_checkblock_fails_aborts_at_its_block() {
    let mut s = Scenario::new(None);
    s.fault = Some(Fault::Corrupt(s.hash(K), mutated(&s)));
    s.aborts = true;
    let (off, counts) = s.parity();
    assert_eq!(off.fatal.as_deref(), Some(CORRUPT_BLOCK));
    assert_eq!(off.coins, Some(s.hash(K - 1)));
    assert_eq!((counts.hits, counts.fallbacks), (K - 1, 1));
}

/// A block only the read-ahead reads mutated fails its check there, and V reads it itself.
#[test]
fn a_block_the_reader_reads_mutated_is_read_again() {
    let mut s = Scenario::new(None);
    s.fault = Some(Fault::ReaderCorrupt(s.hash(K), mutated(&s)));
    let (off, counts) = s.parity();
    assert_eq!(off.fatal, None);
    assert_eq!(off.coins, Some(s.hash(N)));
    assert_eq!((counts.hits, counts.fallbacks), (N - 1, 1));
}

/// A reorg in the middle of the read-ahead: block `K` spends a coin the reconnect's seeds lack,
/// so it is invalid there, and V reorgs to a branch off `K − 3` with the reader [`DEPTH`] blocks
/// ahead on the old path. The verdict, the reorg and the branch's connects are V's own; the
/// epoch moves on, and the branch's blocks are read ahead too.
#[test]
fn a_reorg_mid_read_ahead_has_the_same_verdicts() {
    let mut s = Scenario::new(Some(seed_op(2)));
    s.built.push(seed(2, COIN, &[OP_TRUE]));
    let fork = s.blocks[K as usize - 4].clone();
    let mut parent = fork;
    for _ in 0..3 {
        let m = block_on(&s.p, &parent, vec![]);
        parent = m.clone();
        s.branch.push(m);
    }
    let (off, counts) = s.parity();
    assert_eq!(off.fatal, None);
    assert_eq!(off.coins, Some(parent.hash()));
    let invalid = format!("checked {} Invalid", s.hash(K));
    assert!(off.sync.iter().any(|e| e.starts_with(&invalid)), "{:?}", off.sync);
    let disconnected = off.queued.iter().filter(|e| e.starts_with("disconnected")).count();
    assert_eq!(disconnected, 2);
    assert_eq!((counts.hits, counts.fallbacks), (K + 3, 0));
    assert!(counts.epochs >= 1, "{counts:?}");
    // With a one-block byte bound the old path's blocks wait unread at the reorg, until the
    // epoch drops them.
    let probe = Probe::settled();
    let tight = s.run(ReadAheadSetup::threaded(false).with_bounds(DEPTH, 1).probed(&probe));
    assert_same(&tight, &off, "settled, a one-block byte bound");
    let c = probe.counts();
    assert_eq!((c.hits, c.fallbacks, c.max_ready), (K + 3, 0, 1), "{c:?}");
}

/// A stop in the middle of the read-ahead (`-stopatheight`): V stops at Core's block with
/// blocks read ahead of it, and the reader stops with V.
#[test]
fn a_stop_mid_read_ahead_stops_at_the_same_block() {
    let mut s = Scenario::new(None);
    s.stop_at = i64::from(K);
    let (off, counts) = s.parity();
    assert_eq!(off.fatal, None);
    assert_eq!(off.coins, Some(s.hash(K)));
    assert_eq!((counts.hits, counts.fallbacks), (K, 0));
    assert!(counts.reads > K, "{counts:?}");
}

/// The reconnect of `s` with a settled reader that passes every block unchecked (a planted bug),
/// under xcheck or not.
fn unchecked(s: &Scenario, xcheck: bool) -> Run {
    s.run(ReadAheadSetup::threaded(xcheck).probed(&Probe::unchecked()))
}

/// Q13: xcheck stops V on a reader that passes a block V's own read would not. A reader that
/// skips `CheckBlock` hands V the mutated block only it reads (bytes other than V's read), or the
/// mutated block both read (which V's `CheckBlock` fails). Without xcheck V judges the mutated
/// block past Core's verdict, which is what xcheck exists to catch.
#[test]
fn xcheck_stops_v_on_a_reader_that_passes_what_v_would_not() {
    let mut s = Scenario::new(None);
    let (k, m) = (s.hash(K), mutated(&s));
    let invalid = format!("checked {k} Invalid");
    for (fault, off_aborts, want) in [
        (Fault::ReaderCorrupt(k, m.clone()), false, format!("the read-ahead's block {k} differs")),
        (Fault::Corrupt(k, m), true, format!("the read-ahead passed block {k}, which CheckBlock")),
    ] {
        s.fault = Some(fault);
        s.aborts = off_aborts;
        let off = s.run(ReadAheadSetup::off());
        s.aborts = false;
        let wrong = unchecked(&s, false);
        assert_ne!(wrong.sync, off.sync, "{want}: the planted bug changes the verdicts");
        assert!(wrong.sync.iter().any(|e| e.starts_with(&invalid)), "{want}: {:?}", wrong.sync);
        s.aborts = true;
        let caught = unchecked(&s, true);
        let fatal = caught.fatal.expect("xcheck stops V");
        assert!(fatal.starts_with(&format!("consensusxcheck: {want}")), "{fatal}");
        assert_eq!(caught.reports, std::slice::from_ref(&fatal));
        assert_eq!(caught.coins, Some(s.hash(K - 1)), "{fatal}");
        assert!(!caught.sync.iter().any(|e| e.contains(&k.to_string())), "{fatal}");
    }
}
