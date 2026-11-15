//! The sink over a blk file on disk and the node's block files, driving V through its handle.

use std::os::unix::fs::DirBuilderExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use dash_clock::{Clock, MockClock};
use dash_consensus::header::HeaderParams;
use dash_consensus::pow::check_proof_of_work;
use dash_consensus::primitives::{Header, core_bytes};
use dash_consensus::{Block, BlockId};
use dash_coreimport::blockfiles::external::{ScanReport, load_external_block_file};
use dash_num::CompactTarget;
use dash_params::BuriedDeployment;
use dash_primitives::BlockHeader;
use dashd_chainstate::chain::{
    AbortSink, AttachCoins, BlockFiles, Fatal, FlushBell, FlushHost, FlushSite, FlushStateMode,
    LocalShutdown, NoFlush, NoUi, NullConnector, RecordingAbort, SiteDecision, StoredRecord,
    ValidationInterface, genesis_block,
};
use dashd_chainstate::store::{GlobalFailStop, GuardedFiles, NodeBlockFiles};
use dashd_store::flatfile::{BlockFiles as StoreFiles, BlockFilesConfig, BlockFilesState, OsFs};

use super::*;
use crate::chain::tests::{branch, id, regtest};
use crate::chain::{Chain, NodeSeams, Reindex, open_parts};

const REGTEST_BITS: u32 = 0x207f_ffff;

/// Each block's synchronous `NotifyHeaderTip`, with whether V's view had a tip then.
#[derive(Default)]
struct HeardTips {
    v: OnceLock<ChainHandle>,
    heard: Mutex<Vec<(BlockId, bool)>>,
}

impl ValidationInterface for HeardTips {
    fn notify_header_tip(&self, id: BlockId, _ibd: bool) {
        let has_tip = self.v.get().expect("set at start").view().snap.has_tip();
        self.heard.lock().expect("heard").push((id, has_tip));
    }
}

/// A node started with `-reindex` on an empty index, over the node's block files in a blocks
/// directory holding `blk` as `blk00000.dat`.
struct Node {
    p: Arc<ChainSetup>,
    genesis: Arc<Block>,
    chain: Chain,
    read: Arc<dyn BlockAtPos>,
    shutdown: Arc<LocalShutdown>,
    abort: Arc<RecordingAbort>,
    heard: Arc<HeardTips>,
    blk: PathBuf,
    /// V stops on this `AbortNode`, its one report.
    stops_on: Option<String>,
    /// A test stopped V itself ([`Node::stop_v`]).
    stopped: bool,
    _dir: tempfile::TempDir,
}

/// What a test changes in [`Node::start`].
#[derive(Default)]
struct Opts {
    /// The flush V takes with the coins, [`NoFlush`] by default.
    flush: Option<Box<dyn FlushSite>>,
    /// The block whose write fails ([`FailsOn`]).
    fails_on: Option<BlockHash>,
    /// The `AbortNode` V stops on.
    stops_on: Option<String>,
}

/// The node's `AbortNode` sink: the report, then `StartShutdown`, as ND1's `NodeAbort` does.
struct ShutdownAbort {
    reports: Arc<RecordingAbort>,
    shutdown: Arc<LocalShutdown>,
}

impl AbortSink for ShutdownAbort {
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>) {
        self.reports.abort_node(log_msg, user_msg);
        self.shutdown.start_shutdown();
    }

    fn read_failed(&self, what: &str) {
        self.reports.read_failed(what);
        self.shutdown.start_shutdown();
    }
}

/// The node's block files, failing one block's write with an I/O error.
struct FailsOn {
    files: Box<dyn BlockFiles>,
    hash: BlockHash,
}

impl BlockFiles for FailsOn {
    fn save_block(
        &mut self,
        block: &Arc<Block>,
        height: u32,
        known: Option<FlatFilePos>,
    ) -> Result<Option<FlatFilePos>, String> {
        if block.hash() == self.hash {
            return Err("disk on fire".into());
        }
        self.files.save_block(block, height, known)
    }

    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        self.files.read_block(pos, hash)
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

    fn read_ahead(&self) -> Option<Arc<dyn dashd_chainstate::chain::BlockSource>> {
        self.files.read_ahead()
    }
}

/// W's failure, rung at the admission [`ring_at`](Self::ring_at) names and met at V's next
/// release point: an `AbortNode` V takes as its own (D240).
#[derive(Default)]
struct FailsAtAdmission {
    bell: Option<FlushBell>,
    /// The admissions left until the one that rings; 0 rings none.
    ring_at: Arc<AtomicU32>,
    rung: AtomicBool,
    failed: bool,
}

const W_FAILED: &str = "System error: W failed";

impl FailsAtAdmission {
    /// Rings at the `n`th admission from now.
    fn ring_at(at: &AtomicU32, n: u32) {
        at.store(n, Ordering::SeqCst);
    }
}

impl FlushSite for FailsAtAdmission {
    fn attach(&mut self, bell: FlushBell) {
        self.bell = Some(bell);
    }

    fn admit(&self) -> Result<(), Fatal> {
        let left = self.ring_at.load(Ordering::SeqCst);
        if left > 0 {
            self.ring_at.store(left - 1, Ordering::SeqCst);
            if left == 1 {
                self.rung.store(true, Ordering::SeqCst);
                self.bell.as_ref().expect("attached").ring();
            }
        }
        Ok(())
    }

    fn at_site(&mut self, m: FlushStateMode, h: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        NoFlush.at_site(m, h)
    }

    fn after_step(&mut self, d: SiteDecision, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        NoFlush.after_step(d, h)
    }

    fn service(&mut self, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        NoFlush.service(h)
    }

    fn answer_due(&mut self, _host: &mut FlushHost<'_>) -> Result<(), Fatal> {
        if !self.rung.load(Ordering::SeqCst) || std::mem::replace(&mut self.failed, true) {
            return Ok(());
        }
        Err(Fatal::abort_node(W_FAILED))
    }
}

impl Node {
    fn start(blk: &[u8]) -> Node {
        Node::with(blk, Opts::default())
    }

    fn with(blk: &[u8], opts: Opts) -> Node {
        let p = regtest();
        let genesis = genesis_block(&p);
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = dir.path().join("blocks");
        std::fs::DirBuilder::new().mode(0o700).create(&blocks).expect("dir");
        let path = blocks.join("blk00000.dat");
        std::fs::write(&path, blk).expect("blk");
        let latch = GlobalFailStop::new();
        let cfg = BlockFilesConfig::new(blocks, p.message_start);
        let store = StoreFiles::with_fs(Arc::new(OsFs::new()), cfg, BlockFilesState::default());
        let abort = Arc::new(RecordingAbort::default());
        let pow = *HeaderParams::from_chain_params(&p).expect("regtest").pow();
        let files = NodeBlockFiles::new(
            Arc::new(GuardedFiles::new(store, latch.clone())),
            pow,
            Arc::new(dashd_chainstate::chain::AbortPath::new(abort.clone(), latch)),
        );
        let read = files.block_at_pos();
        let files: Box<dyn BlockFiles> = match opts.fails_on {
            Some(hash) => Box::new(FailsOn {
                files: Box::new(files),
                hash,
            }),
            None => Box::new(files),
        };
        let shutdown = Arc::new(LocalShutdown::default());
        let sink = Arc::new(ShutdownAbort {
            reports: abort.clone(),
            shutdown: shutdown.clone(),
        });
        let clock: Arc<dyn Clock> =
            Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 86_400));
        let node = NodeSeams {
            clock,
            sporks: Arc::new(dash_chain::StaticSporks::for_network(&p, None)),
            abort: sink,
            ui: Arc::new(NoUi),
            shutdown: shutdown.clone(),
            stop_at_height: 0,
            read_ahead: dashd_chainstate::chain::ReadAheadSetup::off(),
            check_block_index: true,
        };
        let records = std::iter::empty::<StoredRecord>();
        let reindex = Reindex {
            argument: true,
            stored: false,
        };
        let parts =
            open_parts(p.clone(), node, records, files, false, reindex).expect("an empty index");
        let heard = Arc::new(HeardTips::default());
        parts.signals.register(heard.clone());
        let mut chain = Chain::start(parts).expect("V starts");
        heard.v.set(chain.handle().clone()).map_err(|_| ()).expect("once");
        let coins = AttachCoins {
            connector: Box::new(NullConnector),
            flush: opts.flush.unwrap_or_else(|| Box::new(NoFlush)),
            facade: None,
        };
        chain.attach_coins(coins).expect("V takes the coins");
        Node {
            p,
            genesis,
            chain,
            read,
            shutdown,
            abort,
            heard,
            blk: path,
            stops_on: opts.stops_on,
            stopped: false,
            _dir: dir,
        }
    }

    /// Stops V without a shutdown request, as no stop of the node's does: V's state, clean.
    fn stop_v(&mut self) {
        let exit = self.chain.stop().expect("V's state");
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        self.stopped = true;
    }

    /// `blk00000.dat`'s `-reindex` import ([`load_external_file`]), and the lines it logged.
    fn load(&self, map: &mut UnknownParents) -> (FileEnd, Vec<String>) {
        let file = std::fs::File::open(&self.blk).expect("blk");
        let reindex = ReindexFile {
            file: 0,
            unknown: map,
            read: &*self.read,
        };
        let (end, lines) = crate::test_log::logged(|| {
            load_external_file(self.v(), &self.p, file, Some(reindex), &*self.shutdown)
        });
        (end, lines.into_iter().map(|(_, line)| line).collect())
    }

    /// `ThreadImport`'s check after the file, and the lines it logged.
    fn after_file(&self, end: FileEnd) -> (bool, Vec<String>) {
        let (go_on, lines) = crate::test_log::logged(|| continue_after(end, &*self.shutdown));
        (go_on, lines.into_iter().map(|(_, line)| line).collect())
    }

    fn v(&self) -> &ChainHandle {
        self.chain.handle()
    }

    /// `blk00000.dat`'s `-reindex` scan.
    fn reindex(&self, map: &mut UnknownParents) -> (ScanReport, u32, Option<ScanEnd>) {
        let file = std::fs::File::open(&self.blk).expect("blk");
        let mut sink =
            ExternalImport::reindex(self.v(), &self.p, 0, map, &*self.read, &*self.shutdown);
        let report = load_external_block_file(file, self.p.message_start, &mut sink);
        (report, sink.loaded(), sink.end())
    }

    fn heard(&self) -> Vec<(BlockId, bool)> {
        std::mem::take(&mut *self.heard.heard.lock().expect("heard"))
    }

    fn activate(&self) {
        assert_eq!(self.v().activate_best_chain().recv(), Ok(Ok(())));
    }

    fn tip(&self) -> BlockId {
        self.v().view().snap.tip.tip
    }

    /// The entry's file and data position.
    fn stored_at(&self, block: &Block) -> (Option<i32>, u32) {
        let e = self.v().reader().entry(id(self.v(), block)).expect("resident");
        (e.file, e.data_pos)
    }

    fn indexed(&self, block: &Block) -> bool {
        self.v().reader().lookup(&block.hash()).is_some()
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if std::thread::panicking() || self.stopped {
            return;
        }
        let exit = self.chain.stop();
        if let Some(msg) = &self.stops_on {
            assert_eq!(self.abort.messages(), std::slice::from_ref(msg));
            return;
        }
        let exit = exit.expect("V's state");
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        assert!(self.abort.messages().is_empty(), "{:?}", self.abort.messages());
    }
}

/// Records of `blocks` after `prefix` bytes that are no record, and each block's data position.
fn blk(p: &ChainSetup, prefix: usize, blocks: &[&Block]) -> (Vec<u8>, Vec<u32>) {
    let mut bytes = vec![0x5a; prefix];
    let mut at = Vec::new();
    for b in blocks {
        let raw = core_bytes(*b);
        bytes.extend_from_slice(&p.message_start);
        bytes.extend_from_slice(&u32::try_from(raw.len()).expect("small").to_le_bytes());
        at.push(u32::try_from(bytes.len()).expect("small"));
        bytes.extend_from_slice(raw.as_slice());
    }
    (bytes, at)
}

/// `block` with its header's version and a nonce `keep` accepts.
fn remined(p: &ChainSetup, block: &Block, version: i32, keep: impl Fn(bool) -> bool) -> Arc<Block> {
    let pow = *HeaderParams::from_chain_params(p).expect("regtest").pow();
    let h = block.header().raw();
    let header = (0..)
        .map(|nonce| {
            let raw = BlockHeader::new(
                version,
                h.prev_hash(),
                h.merkle_root(),
                h.time(),
                REGTEST_BITS,
                nonce,
            );
            Header::new(raw)
        })
        .find(|h| {
            keep(check_proof_of_work(h.hash().as_bytes(), CompactTarget::new(REGTEST_BITS), &pow))
        })
        .expect("a nonce");
    Arc::new(Block::new(header, block.txs().to_vec()))
}

/// On the empty index the wait for genesis goes on; the scan's genesis activation ends it.
#[test]
fn the_wait_for_genesis_ends_at_the_scans_genesis() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let (bytes, _) = blk(&p, 0, &[&genesis]);
    let node = Node::start(&bytes);
    assert!(!node.v().view().snap.has_tip());
    let polls = AtomicU32::new(0);
    node.chain.wait_for_genesis(|| polls.fetch_add(1, Ordering::SeqCst) >= 1);
    assert!(polls.load(Ordering::SeqCst) >= 2, "it waited");
    let (_, loaded, end) = node.reindex(&mut UnknownParents::default());
    assert_eq!((loaded, end), (1, None));
    node.chain.wait_for_genesis(|| panic!("V has a tip"));
    assert_eq!(node.tip(), id(node.v(), &node.genesis));
}

/// A reindex of records in any order: a record whose parent is not indexed yet waits in the map,
/// and once its parent is accepted it is read back at its position and accepted there, as are
/// its own waiting children. Every block is indexed at its scanned position.
#[test]
fn records_out_of_order_are_accepted_through_the_map() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let a = branch(&p, &genesis, 0, 3, 0);
    let b = branch(&p, &genesis, 0, 3, 1);
    let order: [&Block; 7] = [&genesis, &a[0], &b[2], &b[1], &a[1], &b[0], &a[2]];
    let (bytes, at) = blk(&p, 5, &order);
    let node = Node::start(&bytes);
    let mut map = UnknownParents::default();
    let (report, loaded, end) = node.reindex(&mut map);
    assert_eq!((loaded, end, report.read_failed), (7, None, false));
    assert!(map.is_empty());
    for (i, block) in order.iter().enumerate() {
        assert_eq!(node.stored_at(block), (Some(0), at[i]), "record {i}");
    }
    node.activate();
    // The branches tie on work; b2, received first through the map, has the lower sequence id.
    assert_eq!(node.tip(), id(node.v(), &b[2]));
}

/// The sink's checks before a record is read classify a block whose parent V imported earlier
/// in the same file as known: V publishes before it answers.
#[test]
fn a_child_after_its_parent_in_the_file_is_not_mapped() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let a = branch(&p, &genesis, 0, 2, 0);
    let (bytes, _) = blk(&p, 0, &[&genesis, &a[0], &a[1]]);
    let node = Node::start(&bytes);
    let mut map = UnknownParents::default();
    let (_, loaded, _) = node.reindex(&mut map);
    assert_eq!(loaded, 3);
    assert!(map.is_empty());
    assert!(node.indexed(&a[1]));
}

/// A record the reader classed as new that V finds stored (indexed with its data in between)
/// takes Core's "already had" branch: no accept, then what follows a record. Core's line is
/// logged only for a block that is not genesis, at a height divisible by 1000.
#[test]
fn a_record_v_finds_stored_takes_the_already_had_branch() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let a = branch(&p, &genesis, 0, 1, 0);
    let (bytes, at) = blk(&p, 0, &[&genesis, &a[0]]);
    let node = Node::start(&bytes);
    let mut map = UnknownParents::default();
    assert_eq!(node.reindex(&mut map).1, 2);
    let mut sink =
        ExternalImport::reindex(node.v(), &node.p, 0, &mut map, &*node.read, &*node.shutdown);
    let hash = *a[0].hash().as_bytes();
    let raw = core_bytes(&*a[0]);
    let (flow, lines) = crate::test_log::logged(|| {
        let flow = sink.block(raw.as_slice(), &hash, u64::from(at[1]));
        (flow, sink.after(&hash))
    });
    assert_eq!(flow, (Flow::Continue, Flow::Continue));
    assert_eq!((sink.loaded(), sink.end()), (0, None));
    assert!(lines.is_empty(), "height 1: {lines:?}");
    let line = |hash: BlockHash, height| {
        let ((), lines) = crate::test_log::logged(|| sink.already_had(hash, height));
        lines.into_iter().map(|(_, l)| l).collect::<Vec<_>>()
    };
    let a0 = a[0].hash();
    assert_eq!(line(a0, 2000), [format!("Block Import: already had block {a0} at height 2000")]);
    assert!(line(a0, 1999).is_empty());
    assert!(line(genesis.hash(), 0).is_empty());
}

/// A record whose parent is not indexed is mapped by its parent at its position, whether the
/// reader or V found the parent unknown, with Core's line.
#[test]
fn an_out_of_order_record_is_mapped_at_its_position() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let a = branch(&p, &genesis, 0, 2, 0);
    let node = Node::start(&[]);
    let mut map = UnknownParents::default();
    let mut sink =
        ExternalImport::reindex(node.v(), &node.p, 7, &mut map, &*node.read, &*node.shutdown);
    let (a1, a0) = (a[1].hash(), a[0].hash());
    let ((), lines) = crate::test_log::logged(|| sink.out_of_order(a1, a0, 1 << 32 | 21));
    let line = format!("LoadExternalBlockFile: Out of order block {a1}, parent {a0} not known");
    assert_eq!(lines.into_iter().map(|(_, l)| l).collect::<Vec<_>>(), [line]);
    let at = FlatFilePos {
        file: 7,
        pos: 21,
    };
    assert_eq!(map.by_prev.get(&a0).map(|q| q.iter().copied().collect::<Vec<_>>()), Some(vec![at]));
}

/// `NotifyHeaderTip` follows genesis's activation, then each record, then each child processed
/// from the map.
#[test]
fn notify_header_tip_follows_genesis_activation_and_each_child() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let b = branch(&p, &genesis, 0, 2, 1);
    let (bytes, _) = blk(&p, 0, &[&genesis, &b[1], &b[0]]);
    let node = Node::start(&bytes);
    node.reindex(&mut UnknownParents::default());
    let v = node.v();
    let ids = [&genesis, &b[0], &b[1]].map(|blk| id(v, blk));
    assert_eq!(node.heard(), [(ids[0], true), (ids[1], true), (ids[2], true)]);
}

/// A block `AcceptBlock` fails contextually (`bad-cb-height`) is indexed failed; one whose header
/// it refuses (`bad-version(0x00000003)` past BIP65) is not indexed, so its child stays in the
/// map.
#[test]
fn a_refused_header_leaves_its_child_in_the_map() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let bad_height = branch(&p, &genesis, 1, 1, 9).remove(0);
    let cltv = p.consensus.deployment_height(BuriedDeployment::Cltv).to_u32();
    let below = branch(&p, &genesis, 0, cltv - 1, 0);
    let last: &Block = below.last().map_or(&genesis, |b| b);
    let at_cltv = branch(&p, last, cltv - 1, 2, 0);
    let v3 = remined(&p, &at_cltv[0], 3, |ok| ok);
    let child = branch(&p, &v3, cltv, 1, 0).remove(0);
    let mut order: Vec<&Block> = vec![&genesis, &bad_height];
    order.extend(below.iter().map(|b| &**b));
    order.extend([&*v3, &*child]);
    let (bytes, _) = blk(&p, 0, &order);
    let node = Node::start(&bytes);
    let mut map = UnknownParents::default();
    let (_, loaded, end) = node.reindex(&mut map);
    assert_eq!((loaded, end), (1 + cltv - 1, None));
    let failed = node.v().reader().entry(id(node.v(), &bad_height)).expect("indexed").status;
    assert!(failed.has(dash_chain::Status::FAILED_VALID));
    assert!(!node.indexed(&v3) && !node.indexed(&child));
    assert_eq!(map.len(), 1);
}

/// A mapped child whose read fails (`ReadBlockFromDisk`'s proof of work) is dropped from the map
/// with Core's line, and the scan goes on.
#[test]
fn a_child_that_fails_its_read_is_dropped() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let a = branch(&p, &genesis, 0, 3, 0);
    let unworked = remined(&p, &a[1], a[1].header().raw().version(), |ok| !ok);
    let (bytes, _) = blk(&p, 0, &[&genesis, &unworked, &a[0], &a[2]]);
    let node = Node::start(&bytes);
    let mut map = UnknownParents::default();
    let (_, loaded, end) = node.reindex(&mut map);
    assert_eq!((loaded, end), (2, None));
    assert!(!node.indexed(&unworked));
    assert_eq!(map.len(), 1, "a[2] waits for a[1]");
}

/// A shutdown request ends the scan before the next record.
#[test]
fn a_shutdown_ends_the_scan() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let (bytes, _) = blk(&p, 0, &[&genesis]);
    let node = Node::start(&bytes);
    node.shutdown.start_shutdown();
    let (report, loaded, end) = node.reindex(&mut UnknownParents::default());
    assert_eq!((report.interrupted, loaded, end), (true, 0, Some(ScanEnd::Shutdown)));
    assert!(!node.indexed(&genesis));
}

/// `-loadblock`: no positions, so V writes each block, and no map.
#[test]
fn a_load_block_file_writes_its_blocks_and_maps_nothing() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let a = branch(&p, &genesis, 0, 2, 0);
    let node = Node::start(&[]);
    let (bytes, _) = blk(&p, 3, &[&genesis, &a[1], &a[0]]);
    let mut sink = ExternalImport::load_block(node.v(), &node.p, &*node.shutdown);
    load_external_block_file(bytes.as_slice(), p.message_start, &mut sink);
    assert_eq!((sink.loaded(), sink.end()), (2, None));
    assert!(!node.indexed(&a[1]));
    assert_eq!(node.stored_at(&genesis), (Some(0), 8));
    let appended = 8 + u32::try_from(core_bytes(&*genesis).len()).expect("small") + 8;
    assert_eq!(node.stored_at(&a[0]), (Some(0), appended));
}

fn loaded_line(lines: &[String]) -> Option<&String> {
    lines.iter().find(|l| l.starts_with("Loaded ") && l.contains(" blocks from external file in "))
}

const EXIT: &str = "Shutdown requested. Exit ThreadImport";

/// A file's import logs Core's "Loaded" line with the blocks accepted, the mapped child among
/// them, and the import goes on to the next file.
// Core: validation.cpp:5624 at v24.0.0
#[test]
fn a_files_import_logs_the_blocks_it_loaded() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let a = branch(&p, &genesis, 0, 2, 0);
    let (bytes, _) = blk(&p, 0, &[&genesis, &a[1], &a[0]]);
    let node = Node::start(&bytes);
    let (end, lines) = node.load(&mut UnknownParents::default());
    assert_eq!(end, FileEnd::Loaded);
    let line = loaded_line(&lines).expect("the Loaded line");
    assert!(line.starts_with("Loaded 3 blocks from external file in "), "{line}");
    assert!(line.ends_with("ms"), "{line}");
    assert_eq!(node.after_file(end), (true, vec![]));
}

/// Bytes the scan cannot deserialize inside a record are Core's "unexpected data" line under
/// REINDEX, at the record's last byte, with the stream error's `what()`, and the scan goes on to
/// the next record.
// Core: validation.cpp:5607-5619 at v24.0.0
#[test]
fn undeserializable_record_bytes_are_unexpected_data() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let a = branch(&p, &genesis, 0, 1, 0);
    let (mut bytes, _) = blk(&p, 0, &[&genesis]);
    let mut bad = core_bytes(&*a[0]).as_slice()[..80].to_vec();
    bad.extend_from_slice(&[0xfd, 0x01, 0x00]);
    let at = bytes.len();
    bytes.extend_from_slice(&p.message_start);
    bytes.extend_from_slice(&u32::try_from(bad.len()).expect("small").to_le_bytes());
    bytes.extend_from_slice(&bad);
    let (good, _) = blk(&p, 0, &[&a[0]]);
    bytes.extend_from_slice(&good);
    let node = Node::start(&bytes);
    let (end, lines) = node.load(&mut UnknownParents::default());
    assert_eq!(end, FileEnd::Loaded);
    let what = StreamError::NonCanonicalCompactSize.what();
    // `nRewind - 1`, `nRewind` being the record's end (`nBlockPos + nSize`, v:5500).
    let offset = at + 8 + bad.len() - 1;
    let want = format!(
        "LoadExternalBlockFile: unexpected data at file offset 0x{offset:x} - {what}. continuing"
    );
    assert!(lines.contains(&want), "{want}: {lines:?}");
    assert!(loaded_line(&lines).expect("Loaded").starts_with("Loaded 2 blocks"), "{lines:?}");
    assert!(node.indexed(&a[0]));
}

/// A shutdown requested before a record is Core's return there: no "Loaded" line, then the
/// check after the file logs Core's line and the import returns.
// Core: validation.cpp:5466, node/blockstorage.cpp:917-920 at v24.0.0
#[test]
fn a_shutdown_before_a_record_returns_without_the_loaded_line() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let (bytes, _) = blk(&p, 0, &[&genesis]);
    let node = Node::start(&bytes);
    node.shutdown.start_shutdown();
    let (end, lines) = node.load(&mut UnknownParents::default());
    assert_eq!(end, FileEnd::Shutdown);
    assert_eq!(loaded_line(&lines), None, "{lines:?}");
    assert_eq!(node.after_file(end), (false, vec![EXIT.to_owned()]));
}

/// D240 on a file's genesis ABC: W's failure, rung as V admits the ABC (the second command, after
/// genesis's import), is met at the ABC's first release point; V answers it `Ok` and stops,
/// after its report has requested the shutdown. The `NotifyHeaderTip` after it disconnects, so
/// the import reads the flag: Core's next record check returns, with no "Loaded" line, then the
/// check after the file logs Core's line (OD-CH5-71).
// Core: validation.cpp:3789-3791, :5466, :5539-5552 at v24.0.0; node/blockstorage.cpp:917-920
#[test]
fn a_files_genesis_abc_stopped_by_d240_ends_as_a_shutdown() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let a = branch(&p, &genesis, 0, 1, 0);
    let (bytes, _) = blk(&p, 0, &[&genesis, &a[0]]);
    let site = FailsAtAdmission::default();
    let ring_at = Arc::clone(&site.ring_at);
    let node = Node::with(
        &bytes,
        Opts {
            flush: Some(Box::new(site)),
            stops_on: Some(W_FAILED.to_owned()),
            ..Opts::default()
        },
    );
    FailsAtAdmission::ring_at(&ring_at, 2);
    let (end, lines) = node.load(&mut UnknownParents::default());
    assert_eq!(end, FileEnd::Shutdown);
    assert_eq!(loaded_line(&lines), None, "{lines:?}");
    assert!(node.indexed(&genesis), "genesis's import ran before the ABC");
    assert!(!node.indexed(&a[0]), "no record after genesis");
    assert_eq!(node.after_file(end), (false, vec![EXIT.to_owned()]));
}

/// A record's import answers `Err` after V's own `AbortNode` (its write failed): Core's break,
/// so "Loaded" counts the blocks before it, then the check after the file sees the shutdown the
/// report requested.
// Core: validation.cpp:5528-5533, :5624 at v24.0.0; node/blockstorage.cpp:917-920
#[test]
fn a_records_failed_write_logs_loaded_then_exits() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let a = branch(&p, &genesis, 0, 2, 0);
    let (bytes, _) = blk(&p, 0, &[&genesis, &a[0], &a[1]]);
    let node = Node::with(
        &bytes,
        Opts {
            fails_on: Some(a[0].hash()),
            stops_on: Some("System error: disk on fire".to_owned()),
            ..Opts::default()
        },
    );
    let (end, lines) = node.load(&mut UnknownParents::default());
    assert_eq!(end, FileEnd::Loaded);
    let line = loaded_line(&lines).expect("Loaded");
    assert!(line.starts_with("Loaded 1 blocks from external file in "), "{line}");
    assert_eq!(node.after_file(end), (false, vec![EXIT.to_owned()]));
}

/// A mapped child's `Err` (V's own `AbortNode`: its write failed) does not end the children's
/// processing, as Core ignores a child's state: the sink goes on to `NotifyHeaderTip`, which V,
/// stopped, drops, and that ends the file as Core's next record check does, without "Loaded".
// Core: validation.cpp:5579-5606, :5466 at v24.0.0
#[test]
fn a_child_s_error_goes_on_with_the_children() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let a = branch(&p, &genesis, 0, 2, 0);
    let (bytes, _) = blk(&p, 0, &[&genesis, &a[1], &a[0]]);
    let node = Node::with(
        &bytes,
        Opts {
            fails_on: Some(a[1].hash()),
            stops_on: Some("System error: disk on fire".to_owned()),
            ..Opts::default()
        },
    );
    let (end, lines) = node.load(&mut UnknownParents::default());
    assert_eq!(end, FileEnd::Shutdown);
    assert_eq!(loaded_line(&lines), None, "{lines:?}");
    assert!(node.indexed(&a[0]));
}

/// A reply V drops while the node's flag is clear ends the file early with no line, and the
/// import returns without Core's shutdown line (OD-CH5-71).
#[test]
fn a_disconnect_without_a_shutdown_ends_the_import_silently() {
    let node = Node::start(&[]);
    assert_eq!(node.after_file(FileEnd::Disconnected), (false, vec![]));
}

/// The per-file function on a V that stopped with the node's flag clear: genesis's import is
/// dropped, the flag read once is clear, so the file ends [`FileEnd::Disconnected`], with no
/// "Loaded" line, and the import returns with no line (OD-CH5-71).
#[test]
fn a_file_v_drops_without_a_shutdown_ends_disconnected() {
    let p = regtest();
    let genesis = genesis_block(&p);
    let (bytes, _) = blk(&p, 0, &[&genesis]);
    let mut node = Node::start(&bytes);
    node.stop_v();
    assert!(!node.shutdown.shutdown_requested());
    let (end, lines) = node.load(&mut UnknownParents::default());
    assert_eq!(end, FileEnd::Disconnected);
    assert_eq!(loaded_line(&lines), None, "{lines:?}");
    assert_eq!(node.after_file(end), (false, vec![]));
}
