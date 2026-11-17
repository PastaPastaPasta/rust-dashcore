//! V's half of the block import (`LoadExternalBlockFile`, `ThreadImport`): records and children
//! accepted at their scanned position, on an index that `-reindex` opened with no entry. As the
//! importer does, each activates genesis once it is accepted (`validation.cpp:5539-5552` at
//! v24.0.0): `CheckBlockIndex` asserts that an index with no active chain holds at most genesis
//! (`:5638-5641`).

use std::sync::Arc;
use std::time::Duration;

use dash_chain::{ChainTipStatus, FlatFilePos, Status, Work};
use dash_consensus::Block;
use dash_params::BuriedDeployment;
use dash_primitives::BlockHash;

use super::failure::{restartable, three_forks, tip};
use super::support::{
    Mined, Rig, RigConfig, SPACING, SharedFiles, branch, cb_script, child, coinbase, mine,
    mine_version, regtest, sorted,
};
use crate::chain::check::check_block_index_holding;
use crate::chain::load::LoadPrune;
use crate::chain::{
    BlockFiles, BlockSource, ChainHandle, Fatal, FlushHost, FlushSite, FlushStateMode, ImportKind,
    Imported, LocalShutdown, MemBlockFiles, NoFlush, NodeBlockIndex, SiteDecision, load_chain_tip,
    open_block_index,
};

const SOON: Duration = Duration::from_secs(120);

/// V on an index `-reindex` opened: no entry, nothing written.
fn reindexing(files: Box<dyn BlockFiles>) -> Rig {
    Rig::with(RigConfig {
        reindex: true,
        files: Some(files),
        ..RigConfig::default()
    })
}

fn import(v: &ChainHandle, m: &Mined, kind: ImportKind) -> Result<Imported, String> {
    v.import_block(m.block.clone(), None, kind).recv().expect("V answers the import")
}

fn record(rig: &Rig, m: &Mined) -> Result<Imported, String> {
    let r = import(&rig.v, m, ImportKind::Record);
    rig.assert_check_block_index();
    r
}

fn activate(rig: &Rig) {
    assert_eq!(rig.v.activate_best_chain().recv(), Ok(Ok(())));
    rig.assert_check_block_index();
}

/// Each entry's index record by hash (height, work, status, `nTx`, `nChainTx`, candidate), and
/// `getchaintips`, ids aside: what an import and a normal load of the same blocks must agree on.
type Entry = (BlockHash, u32, Work, u32, u32, u32, bool);
type Tip = (BlockHash, u32, Work, u32, ChainTipStatus);

fn records(rig: &Rig) -> (Vec<Entry>, Vec<Tip>, BlockHash) {
    let (entries, tips) = rig
        .v
        .query(|q| {
            let r = q.chain.reader();
            let hash = |id| r.entry(id).expect("resident").hash;
            let mut entries: Vec<Entry> = (q.chain.ids())
                .map(|id| {
                    let e = r.entry(id).expect("resident");
                    let (chain_tx, candidate) = (q.idx.n_chain_tx(id), q.idx.is_candidate(id));
                    (e.hash, e.height, e.chain_work, e.status.bits(), e.n_tx, chain_tx, candidate)
                })
                .collect();
            entries.sort_by_key(|e| *e.0.as_bytes());
            let tips = q.chain.chain_tips_with(&|id| q.idx.n_chain_tx(id) != 0).expect("a tip");
            let tips: Vec<Tip> = (tips.iter())
                .map(|t| (hash(t.id), t.height, t.chain_work, t.branchlen, t.status))
                .collect();
            (entries, tips)
        })
        .recv()
        .expect("V serves");
    (entries, tips, rig.v.reader().snapshot().tip.best_header_hash)
}

/// The import on an empty index: readers and V's view see no entry, `CheckBlockIndex` takes
/// Core's rule with no active chain, init's ABC does nothing, `NotifyHeaderTip` has nothing to
/// notify, and the open wrote no genesis block.
#[test]
fn v_starts_on_an_index_with_no_entry() {
    let files = SharedFiles::default();
    let mut rig = reindexing(Box::new(files.clone()));
    assert_eq!(rig.init, Ok(Ok(())));
    assert!(!rig.v.view().snap.has_tip());
    assert_eq!(rig.v.reader().lookup(&rig.genesis.hash()), None);
    rig.assert_check_block_index();
    assert_eq!(rig.v.notify_header_tip().recv(), Ok(false));
    activate(&rig);
    assert_eq!(files.stored(), 0, "-reindex skips LoadGenesisBlock");
    assert!(rig.stop().fatal.is_none());
}

/// `open_block_index` on an empty index writes genesis unless `-reindex` (`node/chainstate.cpp:210`
/// at v24.0.0).
#[test]
fn only_reindex_skips_the_genesis_write() {
    for reindex in [false, true] {
        let p = regtest();
        let mut files = MemBlockFiles::default();
        let fresh = crate::chain::LoadedIndex::Empty {
            cfg: dash_chain::ChainConfig::node(&p),
            svc: super::support::services(&p),
        };
        let shutdown = LocalShutdown::default();
        let (chain, _) = open_block_index(p.clone(), fresh, &mut files, &shutdown, false, reindex)
            .expect("opened");
        assert_eq!(chain.ids().count(), usize::from(!reindex), "reindex {reindex}");
        assert_eq!(files.stored(), usize::from(!reindex), "reindex {reindex}");
    }
}

/// Genesis at its position, then its children, a record whose parent comes later (no entry, for
/// the importer's map), and that child once its parent is in: each step passes
/// `CheckBlockIndex`, a stored record is answered with its height, and the index then equals a
/// normally loaded twin's.
#[test]
fn an_import_from_genesis_equals_a_loaded_twin() {
    let files = SharedFiles::default();
    let mut rig = reindexing(Box::new(files.clone()));
    let g = rig.genesis.clone();
    let a = branch(&rig.p, &g, 3, 0);
    let b = branch(&rig.p, &g, 2, 1);

    assert_eq!(record(&rig, &g), Ok(Imported::Accepted));
    activate(&rig);
    assert_eq!(tip(&rig), rig.id(&g));
    assert_eq!(rig.v.notify_header_tip().recv(), Ok(true));
    for m in &a[..2] {
        assert_eq!(record(&rig, m), Ok(Imported::Accepted));
    }
    assert_eq!(record(&rig, &b[1]), Ok(Imported::UnknownParent));
    assert_eq!(rig.v.reader().lookup(&b[1].hash()), None);
    assert_eq!(record(&rig, &a[2]), Ok(Imported::Accepted));
    assert_eq!(record(&rig, &b[0]), Ok(Imported::Accepted));
    assert_eq!(import(&rig.v, &b[1], ImportKind::Child), Ok(Imported::Accepted));
    rig.assert_check_block_index();
    assert_eq!(
        record(&rig, &a[0]),
        Ok(Imported::Stored {
            height: 1
        })
    );
    assert_eq!(files.stored(), 6);
    activate(&rig);
    assert_eq!(tip(&rig), rig.id(&a[2]));

    let twin = Rig::new();
    for m in a.iter().chain(&b) {
        twin.accept(m);
    }
    assert_eq!(records(&rig), records(&twin));
    assert!(rig.stop().fatal.is_none());
}

/// The reader the importing thread holds sees a block V imported once the import answers, so the
/// next record, its child, is processed and not mapped as an unknown parent's.
#[test]
fn an_imported_parent_is_known_to_the_reader_when_its_import_answers() {
    let rig = reindexing(Box::new(SharedFiles::default()));
    let reader = rig.v.reader().clone();
    let a = branch(&rig.p, &rig.genesis, 2, 0);
    assert_eq!(import(&rig.v, &rig.genesis, ImportKind::Record), Ok(Imported::Accepted));
    activate(&rig);
    assert_eq!(import(&rig.v, &a[0], ImportKind::Record), Ok(Imported::Accepted));
    assert!(reader.lookup(&a[0].hash()).is_some());
    assert_eq!(import(&rig.v, &a[1], ImportKind::Record), Ok(Imported::Accepted));
    assert!(reader.lookup(&a[1].hash()).is_some());
}

/// `ThreadImport`'s `LoadGenesisBlock` after the scan: on an index without genesis it writes
/// and indexes genesis, as the normal load does; with genesis indexed it does nothing.
#[test]
fn the_genesis_retry_adds_genesis_once() {
    let files = SharedFiles::default();
    let mut rig = reindexing(Box::new(files.clone()));
    assert_eq!(rig.v.load_genesis_block().recv(), Ok(true));
    assert!(rig.status(&rig.genesis).has(Status::HAVE_DATA));
    assert_eq!(files.stored(), 1);
    rig.assert_check_block_index();
    activate(&rig);
    assert_eq!(rig.v.load_genesis_block().recv(), Ok(true));
    assert_eq!(files.stored(), 1);
    assert_eq!(records(&rig), records(&Rig::new()));
    assert!(rig.stop().fatal.is_none());
}

/// A block whose contextual check fails (`bad-cb-height`) is indexed and marked failed, as
/// `AcceptBlock` leaves it.
#[test]
fn a_contextually_invalid_record_leaves_a_failed_entry() {
    let rig = reindexing(Box::new(SharedFiles::default()));
    let g = &rig.genesis;
    assert_eq!(record(&rig, g), Ok(Imported::Accepted));
    activate(&rig);
    let bad = Mined {
        block: mine(&rig.p, g.hash(), g.time() + SPACING, vec![coinbase(&cb_script(2, 0))]),
        height: 1,
    };
    assert_eq!(record(&rig, &bad), Ok(Imported::Rejected));
    assert!(rig.status(&bad).has(Status::FAILED_VALID));
}

/// A header `AcceptBlockHeader` refuses (`bad-version(0x00000003)` past BIP65) leaves no entry,
/// so its child stays a record with an unknown parent.
#[test]
fn a_refused_header_leaves_no_entry_and_its_child_unknown() {
    let rig = reindexing(Box::new(SharedFiles::default()));
    let cltv = rig.p.consensus.deployment_height(BuriedDeployment::Cltv).to_u32();
    assert_eq!(record(&rig, &rig.genesis), Ok(Imported::Accepted));
    activate(&rig);
    let below = branch(&rig.p, &rig.genesis, cltv - 1, 0);
    for m in &below {
        assert_eq!(record(&rig, m), Ok(Imported::Accepted));
    }
    let last = below.last().unwrap_or(&rig.genesis);
    let cb = coinbase(&cb_script(cltv, 0));
    let v3 = Mined {
        block: mine_version(&rig.p, last.hash(), last.time() + SPACING, vec![cb], 3),
        height: cltv,
    };
    assert_eq!(record(&rig, &v3), Ok(Imported::Rejected));
    assert_eq!(rig.v.reader().lookup(&v3.hash()), None);
    assert_eq!(record(&rig, &child(&rig.p, &v3, 0)), Ok(Imported::UnknownParent));
}

/// Block files that fail one block's write with an I/O error.
struct FailsOn {
    files: SharedFiles,
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
        BlockFiles::read_block(&self.files, pos, hash)
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
        self.files.read_ahead()
    }
}

/// V's own `AbortNode` in an import (the block's write fails) answers the import with the error,
/// Core's `state.IsError()`, then stops V with the node's one report.
#[test]
fn an_abort_node_in_an_import_answers_its_error() {
    let p = regtest();
    let a1 = child(&p, &Mined::genesis(&p), 0);
    let mut rig = reindexing(Box::new(FailsOn {
        files: SharedFiles::default(),
        hash: a1.hash(),
    }));
    assert_eq!(record(&rig, &rig.genesis), Ok(Imported::Accepted));
    activate(&rig);
    let msg = "System error: disk on fire";
    assert_eq!(import(&rig.v, &a1, ImportKind::Record), Err(msg.into()));
    rig.stop_on(msg);
}

/// The tied-candidate fallback records its candidate set and keeps the same candidates as before.
/// `CheckBlockIndex` exempts only the recorded candidate set (DEC-1620), and only during the
/// startup hold.
#[test]
fn the_startup_hold_exempts_only_the_recorded_candidate_set() {
    let (mut rig, _files) = restartable();
    let [main, side, other] = three_forks(&rig, 2);
    let [a2, b2, c2] = [&main[1], &side[1], &other[1]].map(|m| rig.id(m));
    assert!(b2 < a2 && a2 < c2);
    assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    let exit = rig.stop();
    let mut chain = exit.chain;
    let mut idx = NodeBlockIndex::load(&chain);
    let kept = chain.private_tip().is_some_and(|(t, _)| t.id == a2);
    load_chain_tip(&mut chain, &mut idx, a2).expect("the tied-candidate fallback");
    if kept {
        let reader = chain.reader();
        LoadPrune::decide(&reader, &idx, a2)
            .expect("the tied-candidate fallback")
            .run(&mut idx, &reader, a2);
    }
    assert_eq!(sorted(idx.candidates()), [b2, c2]);
    assert_eq!(sorted(idx.tied_kept().iter().copied()), [c2]);
    let check = |idx: &NodeBlockIndex, holding| {
        check_block_index_holding(&chain, idx, false, holding).map_err(|e| (e.what, e.id))
    };
    assert_eq!(check(&idx, true), Ok(()));
    let rule = "a block worse than the tip, or with an unseen parent, is a candidate";
    assert_eq!(check(&idx, false), Err((rule, c2)));
    assert!(idx.unrecord_tied_kept(c2));
    assert_eq!(check(&idx, true), Err((rule, c2)));
}

/// An import during the startup hold is served before startup's ABC: its accept's
/// `CheckBlockIndex` passes on the recorded candidate set, and the ABC then connects it.
#[test]
fn an_import_is_served_during_the_startup_hold() {
    let (mut rig, files) = restartable();
    let [main, _, other] = three_forks(&rig, 2);
    let a2 = rig.id(&main[1]);
    assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    let exit = rig.stop();
    let c3 = child(&rig.p, &other[1], 2);
    let block = c3.block.clone();
    let (sent, answers) = std::sync::mpsc::channel();
    let cfg = RigConfig {
        before_init: Some(Box::new(move |v: &ChainHandle| {
            let imported = v.import_block(block, None, ImportKind::Record).recv_timeout(SOON);
            sent.send(imported).expect("the test waits");
        })),
        ..RigConfig::default()
    };
    let mut rig = Rig::restart_with(exit, Box::new(files), a2, cfg).expect("V starts");
    let imported = answers.recv().expect("sent before init's ABC");
    assert_eq!(imported, Some(Ok(Ok(Imported::Accepted))));
    assert_eq!(tip(&rig), rig.id(&c3));
    let recorded = rig.v.query(|q| q.idx.tied_kept().len()).recv();
    assert_eq!(recorded, Ok(0), "the recorded candidate set ends with the startup hold");
    assert!(rig.stop().fatal.is_none());
}

/// The flush side's reindex clear, counted, ending as `fail` says.
struct Clears {
    count: Arc<std::sync::atomic::AtomicU32>,
    fail: ClearEnd,
}

/// How [`Clears`] ends the clear (OD-CH5-77).
#[derive(Clone, Copy)]
enum ClearEnd {
    Written,
    /// The clear's own job fails, as W's `AbortNode` does.
    Failed,
    /// A failure met before the clear ran: an earlier job's, or another store's latched.
    Before,
}

const CLEAR_FAILED: &str = "System error: the clear failed";
const FAILED_BEFORE: &str = "System error: an earlier flush failed";

impl Clears {
    fn site(fail: ClearEnd) -> (Box<dyn FlushSite>, Arc<std::sync::atomic::AtomicU32>) {
        let count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let site = Clears {
            count: count.clone(),
            fail,
        };
        (Box::new(site), count)
    }
}

impl FlushSite for Clears {
    fn at_site(&mut self, m: FlushStateMode, h: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        NoFlush.at_site(m, h)
    }

    fn after_step(&mut self, d: SiteDecision, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        NoFlush.after_step(d, h)
    }

    fn service(&mut self, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        NoFlush.service(h)
    }

    fn reindex_clear(&mut self, _host: &mut FlushHost<'_>) -> Result<Result<(), Fatal>, Fatal> {
        self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match self.fail {
            ClearEnd::Written => Ok(Ok(())),
            ClearEnd::Failed => Ok(Err(Fatal::flush_failed(CLEAR_FAILED))),
            ClearEnd::Before => Err(Fatal::flush_failed(FAILED_BEFORE)),
        }
    }
}

/// V starts with `fReindex` set (`SetLoading`'s reindex before V serves), and `FinishReindex`
/// runs the flush side's clear once, then resets it (`node/blockstorage.cpp:923-924` at v24.0.0).
#[test]
fn finish_reindex_clears_the_flag_after_the_flush_sides_clear() {
    let (site, clears) = Clears::site(ClearEnd::Written);
    let mut rig = Rig::with(RigConfig {
        reindex: true,
        files: Some(Box::new(SharedFiles::default())),
        flush: Some(site),
        ..RigConfig::default()
    });
    assert!(rig.v.loading().reindex(), "set before V serves");
    assert_eq!(rig.v.finish_reindex().recv(), Ok(Ok(())));
    assert_eq!(clears.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!rig.v.loading().reindex());
    assert!(rig.stop().fatal.is_none());
}

/// A failed clear is the flush's `AbortNode`, which the flush side reports: `FinishReindex`
/// answers its error, V stops on it without a report of its own, and `fReindex` stays set.
#[test]
fn a_failed_reindex_clear_answers_its_error_and_stops_v() {
    let (site, _) = Clears::site(ClearEnd::Failed);
    let mut rig = Rig::with(RigConfig {
        reindex: true,
        files: Some(Box::new(SharedFiles::default())),
        flush: Some(site),
        ..RigConfig::default()
    });
    assert_eq!(rig.v.finish_reindex().recv(), Ok(Err(CLEAR_FAILED.into())));
    assert!(rig.v.loading().reindex());
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some(CLEAR_FAILED));
    assert!(rig.abort.messages().is_empty());
}

/// A failure met before the clear ran (an earlier job's, or another store's latched) is not the
/// clear's: V stops on it and drops the reply, as the import's other drops, and `fReindex` stays
/// set (OD-CH5-77).
#[test]
fn a_failure_before_the_reindex_clear_drops_its_reply() {
    let (site, _) = Clears::site(ClearEnd::Before);
    let mut rig = Rig::with(RigConfig {
        reindex: true,
        files: Some(Box::new(SharedFiles::default())),
        flush: Some(site),
        ..RigConfig::default()
    });
    assert!(rig.v.finish_reindex().recv().is_err(), "the reply is dropped");
    assert!(rig.v.loading().reindex());
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some(FAILED_BEFORE));
    assert!(rig.abort.messages().is_empty());
}

/// The tied-candidate fallback's state, as [`an_import_is_served_during_the_startup_hold`] builds
/// it, restarted as that test restarts it, with `cfg`.
fn tied_candidate_fallback(cfg: impl FnOnce(SharedFiles) -> RigConfig) -> (Rig, [Vec<Mined>; 3]) {
    let (mut rig, files) = restartable();
    let forks = three_forks(&rig, 2);
    let a2 = rig.id(&forks[0][1]);
    assert_eq!(rig.v.invalidate_block(forks[0][0].hash()).recv(), Ok(Ok(())));
    let exit = rig.stop();
    let rig = Rig::restart_with(exit, Box::new(files.clone()), a2, cfg(files)).expect("V starts");
    (rig, forks)
}

/// A stored reindex resumed on the tied-candidate fallback: V starts in the startup hold with
/// `fReindex` set and is sent dashd-node's `reindex_import` order after an empty scan: the
/// clear, the genesis retry, then startup's ABC. The first two are served in the hold, which
/// still queues a query behind them; startup's ABC ends it and clears the recorded candidate set.
#[test]
fn the_clear_and_the_genesis_retry_are_served_during_the_startup_hold() {
    let (sent, answers) = std::sync::mpsc::channel();
    let (mut rig, _) = tied_candidate_fallback(|files| RigConfig {
        reindex: true,
        files: Some(Box::new(files)),
        before_init: Some(Box::new(move |v: &ChainHandle| {
            let reindex = v.loading().reindex();
            let cleared = v.finish_reindex().recv_timeout(SOON);
            let retried = v.load_genesis_block().recv_timeout(SOON);
            let held = v.query(|q| q.idx.tied_kept().len());
            let early = held.recv_timeout(SOON / 4);
            let state = (reindex, cleared, retried, v.loading().reindex(), early);
            sent.send((state, held)).expect("the test waits");
        })),
        ..RigConfig::default()
    });
    let (state, held) = answers.recv().expect("sent before init's ABC");
    assert_eq!(state, (true, Some(Ok(Ok(()))), Some(Ok(true)), false, None));
    assert_eq!(rig.init, Ok(Ok(())));
    assert_eq!(held.recv(), Ok(0), "the held query is answered after startup's ABC");
    let recorded = rig.v.query(|q| q.idx.tied_kept().len()).recv();
    assert_eq!(recorded, Ok(0), "the recorded candidate set ends with the startup hold");
    rig.assert_check_block_index();
    assert!(rig.stop().fatal.is_none());
}

/// The ABC a file's import sends once its genesis is in (`validation.cpp:5539-5552` at v24.0.0)
/// ends the startup hold before startup's own: a query, which the hold would queue behind that
/// ABC, is answered at once after it, with the recorded candidate set cleared.
#[test]
fn a_files_genesis_abc_ends_the_startup_hold() {
    let (sent, answers) = std::sync::mpsc::channel();
    let (mut rig, _) = tied_candidate_fallback(|files| RigConfig {
        reindex: true,
        files: Some(Box::new(files)),
        before_init: Some(Box::new(move |v: &ChainHandle| {
            let abc = v.activate_best_chain().recv_timeout(SOON);
            let recorded = v.query(|q| q.idx.tied_kept().len()).recv_timeout(SOON / 4);
            sent.send((abc, recorded)).expect("the test waits");
        })),
        ..RigConfig::default()
    });
    let (abc, recorded) = answers.recv().expect("sent before init's ABC");
    assert_eq!((abc, recorded), (Some(Ok(Ok(()))), Some(Ok(0))));
    rig.assert_check_block_index();
    assert!(rig.stop().fatal.is_none());
}
