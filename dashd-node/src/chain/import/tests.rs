//! The sink over a blk file on disk and the node's block files, driving V through its handle.

use std::os::unix::fs::DirBuilderExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
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
    AttachCoins, LocalShutdown, NoFlush, NoUi, NullConnector, RecordingAbort, StoredRecord,
    ValidationInterface, genesis_block,
};
use dashd_chainstate::store::{GlobalFailStop, GuardedFiles, NodeBlockFiles};
use dashd_store::flatfile::{BlockFiles as StoreFiles, BlockFilesConfig, BlockFilesState, OsFs};

use super::*;
use crate::chain::tests::{branch, id, regtest};
use crate::chain::{Chain, NodeSeams, open_parts};

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
    _dir: tempfile::TempDir,
}

impl Node {
    fn start(blk: &[u8]) -> Node {
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
        let shutdown = Arc::new(LocalShutdown::default());
        let clock: Arc<dyn Clock> =
            Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 86_400));
        let node = NodeSeams {
            clock,
            sporks: Arc::new(dash_chain::StaticSporks::for_network(&p, None)),
            abort: abort.clone(),
            ui: Arc::new(NoUi),
            shutdown: shutdown.clone(),
            stop_at_height: 0,
            read_ahead: dashd_chainstate::chain::ReadAheadSetup::off(),
            check_block_index: true,
        };
        let records = std::iter::empty::<StoredRecord>();
        let parts = open_parts(p.clone(), node, records, Box::new(files), false, true)
            .expect("an empty index");
        let heard = Arc::new(HeardTips::default());
        parts.signals.register(heard.clone());
        let mut chain = Chain::start(parts).expect("V starts");
        heard.v.set(chain.handle().clone()).map_err(|_| ()).expect("once");
        let coins = AttachCoins {
            connector: Box::new(NullConnector),
            flush: Box::new(NoFlush),
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
            _dir: dir,
        }
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
        if std::thread::panicking() {
            return;
        }
        let exit = self.chain.stop().expect("V's state");
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
