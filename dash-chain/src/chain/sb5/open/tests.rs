//! SB5 F7: SPV `open` (§8) over SB4's SPV chain and `MemHeaderStore`: the reopened chain is the
//! one that committed (P4, with `best_header` against an independent scan), E5's recompute, the
//! refusals, and P5's ids after a lost suffix. `fork_target` and the `r6m1_reopen_*` cases wait
//! on SB4 C6.

use std::cmp::Reverse;
use std::sync::Arc;

use dash_consensus::primitives::Header;
use dash_params::{ChainSetup, Network};
use dash_primitives::BlockHash;

use crate::Work;
use crate::chain::sb5::step_tests::{check, committed, mark, meta, real_step, stages};
use crate::chain::spv_tests::{ANCHOR, Spv, T0, latched, mine_n, regtest_pack, resident};
use crate::error::{ChainError, FailStop as _, StoreError};
use crate::mem_store::{Corrupt, MemHeaderStore, ReadKind};
use crate::shared::ColdInvalidation;
use crate::step::{Durability, HeaderStepSink as _, MetaDelta, StepRow, StepTail};
use crate::testing::{Latch, batch, params, services};
use crate::walk;
use crate::{
    BlockId, CHAIN_SCHEMA, ChainConfig, ChainLockRef, ChainMeta, ClPersisted, Corruption, Evicting,
    HeaderChain, HeaderRecord, HeaderStore, OpenError, RecordKind, Services, Status,
};

/// The trunk's first header's time; header `i` is mined at `at(i)`.
const START: u32 = T0 + 150 * 25;

/// The height below the anchored chain's floor (the pack's first header, `ANCHOR − 24`).
const BELOW_FLOOR: u32 = ANCHOR - 25;

fn at(i: usize) -> u32 {
    START + 150 * (i as u32 + 1)
}

fn reopen(p: &Arc<ChainSetup>, store: &MemHeaderStore) -> Spv {
    Spv::open(p.clone(), ChainConfig::spv(p, 10), services(p), store.clone()).expect("open")
}

/// P4's reference: the most-work `IsValid(VALID_TREE)` accepted record, then the lowest id, by
/// a scan of every stored record (not `open`'s leaf reduction).
fn scan_best(store: &MemHeaderStore) -> BlockId {
    let recs = store.from_height(0).expect("read");
    let eligible = recs
        .iter()
        .filter(|r| r.kind == RecordKind::Accepted && r.status.is_valid(Status::VALID_TREE));
    eligible.max_by_key(|r| (r.chain_work, Reverse(r.id))).expect("an eligible record").id
}

/// `b`, reopened, is `a`: tip, best header, ids, the window and each resident entry, the
/// resident leaves, the active chain, the checkpoint cache and the ChainLock state.
fn same(a: &Spv, b: &Spv) {
    assert_eq!(a.best_header, b.best_header, "best_header");
    assert_eq!(a.resident, b.resident, "the window");
    assert_eq!(a.leaves, b.leaves, "the resident leaves");
    let (sa, sb) = (a.feeds().feed_snapshot(), b.feeds().feed_snapshot());
    assert_eq!(sa.read().tip, sb.read().tip);
    holds_the_same(a, b);
}

/// [`same`] where `a` stepped through ChainLock marks or down to a lower tip:
/// - **`best_header`.** The writer's steps back when a mark lands on it, while `open`'s E5
///   recompute keeps marked leaves eligible, as Core's `m_best_header` differs before and after
///   `LoadBlockIndex`. The caller checks `b`'s against P4's scan.
/// - **The window.** Entries the writer evicted against a higher tip stay cold, while `open`
///   loads every record from the tip's `tip − w`: `b`'s window holds `a`'s and those.
/// - **The active chain's base.** After a disconnect below the window the writer's base stays
///   at the fork until the tip is `w` past it, while `open` windows at `tip − w`: `b`'s base is
///   that, at or below `a`'s, and the heights both hold are equal.
///
/// Everything else is equal, the entries `a` holds field by field.
fn holds_the_same(a: &Spv, b: &Spv) {
    let tip = a.private_tip().expect("a tip").0;
    assert_eq!(tip, b.private_tip().expect("a tip").0, "tip");
    assert_eq!((a.next_id, a.floor), (b.next_id, b.floor));
    assert!(a.resident.is_subset(&b.resident), "the window");
    let base = tip.height.saturating_sub(b.cfg.window.expect("SPV"));
    assert!(b.resident.difference(&a.resident).all(|&(h, _)| h >= base), "loaded from tip − w");
    let entry = |c: &Spv, id| {
        c.shared.arena.with(id, |e, g| {
            let s = g.group.read_own().status;
            (e.hash, e.prev, e.skip, e.height, e.chain_work, e.mtp, e.time_max, e.context, s)
        })
    };
    for &(_, id) in &a.resident {
        assert_eq!(entry(a, id), entry(b, id), "{id:?}");
    }
    assert!(a.leaves.is_subset(&b.leaves), "the resident leaves");
    assert!(*b.max_cold_leaf_work.lock() <= *a.max_cold_leaf_work.lock());
    assert_eq!((b.active.base(), b.active.len()), (base.max(b.floor), a.active.len()));
    assert!(b.active.base() <= a.active.base(), "the active chain's base");
    for h in a.active.base()..a.active.len() {
        assert_eq!(a.active.get(h), b.active.get(h), "active at {h}");
    }
    let (sa, sb) = (a.feeds().feed_snapshot(), b.feeds().feed_snapshot());
    assert_eq!(sa.range(0, u32::MAX).expect("read"), sb.range(0, u32::MAX).expect("read"));
    assert_eq!(sa.read().tip.tip_hash, sb.read().tip.tip_hash);
    assert_eq!(a.last_checkpoint, b.last_checkpoint);
    assert_eq!(a.cl_state(), b.cl_state(), "the ChainLock rows and observed flag");
}

/// Reopens the store and checks the result against `chain` and P4's scan.
fn reopen_same(p: &Arc<ChainSetup>, chain: &Spv, store: &MemHeaderStore) -> Spv {
    let b = reopen(p, store);
    same(chain, &b);
    assert_eq!(b.best_header(), scan_best(store), "P4: best_header");
    b
}

/// Commits `rows` and `tail` as a crafted step.
fn commit_tail(store: &mut MemHeaderStore, rows: &[StepRow], tail: StepTail) {
    store.begin_step();
    store.rows(rows).expect("rows");
    store.commit_step(&tail, Durability::Buffered).expect("commit");
}

/// The store's meta as a step's `MetaDelta`.
fn meta_delta(store: &MemHeaderStore) -> MetaDelta {
    let m = store.chain_meta().expect("read").expect("a meta row");
    MetaDelta {
        next_id: m.next_id,
        tip: m.tip,
        best_header: m.best_header,
    }
}

fn id_of(chain: &Spv, h: &Header) -> BlockId {
    chain.reader().try_lookup(&h.hash()).expect("read").expect("held")
}

/// P4 over real steps at w = 10: after the pack, stale branches on resident and cold blocks,
/// resident and cold marks, and a reorg deeper than the window, each reopen is the chain that
/// committed. The reopened chain then steps on, and reopens again to itself.
#[test]
fn open_reproduces_the_chain() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = committed(&p, &pack, services(&p));
    reopen_same(&p, &chain, &store);

    let trunk = mine_n(&p, pack.hash, START, 60);
    let steps = [
        trunk[..12].to_vec(),
        mine_n(&p, trunk[8].hash(), at(9) + 7, 2),
        trunk[12..30].to_vec(),
        mine_n(&p, trunk[5].hash(), at(6) + 9, 3),
        mine_n(&p, trunk[25].hash(), at(26) + 11, 1),
    ];
    for s in &steps {
        real_step(&mut chain, &mut store, s);
        reopen_same(&p, &chain, &store);
    }
    // A resident stale block, and a cold active one below the later reorg's fork.
    let stale = BlockId(chain.next_id - 1);
    mark(&mut chain, stale);
    mark(&mut chain, BlockId(27));
    real_step(&mut chain, &mut store, &trunk[30..35]);
    reopen_same(&p, &chain, &store);
    let branch = mine_n(&p, trunk[20].hash(), at(21) + 3, 20);
    real_step(&mut chain, &mut store, &branch);
    let mut b = reopen_same(&p, &chain, &store);
    drop(chain);

    real_step(&mut b, &mut store, &trunk[35..]);
    check(&b, &store);
    assert_eq!(b.feeds().feed_snapshot().read().tip.tip_hash, trunk[59].hash());
    reopen_same(&p, &b, &store);
}

/// A chain from genesis reopens with its floor at 0 and its active chain windowed at `tip − w`.
#[test]
fn open_from_genesis() {
    let p = params(Network::Regtest);
    let mut store = MemHeaderStore::new();
    let mut chain =
        Spv::new_genesis(p.clone(), ChainConfig::spv(&p, 10), services(&p), store.clone())
            .expect("genesis");
    let genesis = Header::new(dash_consensus::params::genesis_block(&p).header);
    store.reset(meta(genesis.hash(), 0)).expect("reset");
    let trunk = mine_n(&p, genesis.hash(), T0, 30);
    real_step(&mut chain, &mut store, &trunk[..5]);
    reopen_same(&p, &chain, &store);
    real_step(&mut chain, &mut store, &trunk[5..]);
    let b = reopen_same(&p, &chain, &store);
    assert_eq!((b.active.base(), b.active.len()), (20, 31), "windowed at tip − w");
}

/// An anchored chain at w = 10 with a trunk of 10, committed.
fn trunk10() -> (Arc<ChainSetup>, Spv, MemHeaderStore, Vec<Header>) {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = committed(&p, &pack, services(&p));
    let trunk = mine_n(&p, pack.hash, START, 10);
    real_step(&mut chain, &mut store, &trunk);
    (p, chain, store, trunk)
}

/// Accepts a branch of 6 off `trunk[5]`, heavier than the trunk by 2, and returns it with
/// its leaf's id. Nothing is committed.
fn heavy_branch(p: &ChainSetup, chain: &mut Spv, trunk: &[Header]) -> (Vec<Header>, BlockId) {
    let branch = mine_n(p, trunk[5].hash(), at(6) + 3, 6);
    chain.accept(batch(&branch), None).expect("accept");
    let leaf = id_of(chain, &branch[5]);
    (branch, leaf)
}

/// E5: `open` recomputes `best_header` from the leaves, not from `meta.best_header`. (A
/// walked-back in-memory best header is SB4 C6's.)
#[test]
fn e5_reopen_heavier_leaf() {
    let (p, chain, mut store, trunk) = trunk10();
    let walked_back = MetaDelta {
        best_header: BlockId(26),
        ..meta_delta(&store)
    };
    commit_tail(
        &mut store,
        &[],
        StepTail {
            active: None,
            cl: None,
            meta: walked_back,
        },
    );
    let b = reopen_same(&p, &chain, &store);
    assert_eq!(b.best_header(), id_of(&b, &trunk[9]));
}

/// E5: a CONFLICT-marked, not FAILED, heaviest leaf is eligible: it is `best_header` after
/// `open`, though the step's fork target, and so the tip, stopped at its parent.
#[test]
fn e5_conflict_leaf_eligible() {
    let (p, mut chain, mut store, trunk) = trunk10();
    let (branch, leaf) = heavy_branch(&p, &mut chain, &trunk);
    mark(&mut chain, leaf);
    stages(&mut chain, &mut store);
    let b = reopen_same(&p, &chain, &store);
    assert_eq!(b.best_header(), leaf);
    assert_eq!(b.private_tip().expect("a tip").0.id, id_of(&b, &branch[4]));
}

/// E5: a FAILED heaviest leaf stands for its highest non-FAILED ancestor, its parent here.
#[test]
fn e5_failed_leaf_parent() {
    let (p, mut chain, mut store, trunk) = trunk10();
    let (branch, leaf) = heavy_branch(&p, &mut chain, &trunk);
    chain.set_status(leaf, |s| Status(s.bits() | Status::FAILED_VALID)).expect("mark");
    chain.mark_dirty(leaf);
    stages(&mut chain, &mut store);
    let b = reopen(&p, &store);
    assert_eq!(b.best_header(), id_of(&b, &branch[4]));
    assert_eq!(b.best_header(), scan_best(&store), "P4");
}

/// E5: of two equal-work leaves, the lower id (D9).
#[test]
fn e5_tie_membership() {
    let (p, mut chain, mut store, trunk) = trunk10();
    let sibling = mine_n(&p, trunk[8].hash(), at(9) + 5, 1);
    real_step(&mut chain, &mut store, &sibling);
    let b = reopen_same(&p, &chain, &store);
    let (first, second) = (id_of(&b, &trunk[9]), id_of(&b, &sibling[0]));
    assert!(first < second);
    assert_eq!(b.best_header(), first);
}

/// E5's walk down a FAILED path into cold records: a branch FAILED from its leaf down to
/// heights below the window stands for its highest non-FAILED ancestor, which is cold and
/// lighter than the tip.
#[test]
fn e5_failed_path_into_cold_records() {
    let (p, chain, mut store, trunk, stale) = stepped();
    drop(chain);
    let failed = |bits| Status(Status::VALID_TREE | bits);
    let rows = [
        StepRow::Status(stale[1], failed(Status::FAILED_VALID)),
        StepRow::Status(stale[0], failed(Status::FAILED_CHILD)),
    ];
    let m = meta_delta(&store);
    commit_tail(
        &mut store,
        &rows,
        StepTail {
            active: None,
            cl: None,
            meta: m,
        },
    );
    let b = reopen(&p, &store);
    assert!(b.shared.arena.imm(stale[0], |_| ()).is_none(), "the path is cold");
    assert_eq!(b.best_header(), id_of(&b, &trunk[29]));
    assert_eq!(b.best_header(), scan_best(&store), "P4");
}

/// An anchored chain at w = 10 with a trunk of 30 and a stale branch of 2 off `trunk[15]`
/// (below the window), all of it durable; the branch's ids are returned.
fn stepped() -> (Arc<ChainSetup>, Spv, MemHeaderStore, Vec<Header>, [BlockId; 2]) {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = committed(&p, &pack, services(&p));
    let trunk = mine_n(&p, pack.hash, START, 30);
    real_step(&mut chain, &mut store, &trunk[..20]);
    let stale = mine_n(&p, trunk[15].hash(), at(16) + 3, 2);
    real_step(&mut chain, &mut store, &stale);
    let stale = [id_of(&chain, &stale[0]), id_of(&chain, &stale[1])];
    real_step(&mut chain, &mut store, &trunk[20..]);
    store.sync_wal().expect("sync");
    (p, chain, store, trunk, stale)
}

/// The issued-id watermark survives a reopen (DEC-187 amendment 3): `open` restores it from the
/// store's `next_id`, so the reopened chain reads its cold records, and one that comes back
/// under `next_id` is refused as corruption, which trips the latch.
#[test]
fn open_restores_the_issued_watermark() {
    let (p, chain, store, trunk, _) = stepped();
    let (x, next) = (id_of(&chain, &trunk[3]), BlockId(chain.next_id));
    drop(chain);
    let (svc, latch) = latched(&p);
    let reopened =
        Spv::open(p.clone(), ChainConfig::spv(&p, 10), svc, store.clone()).expect("open");
    assert!(!resident(&reopened, x));
    let reader = reopened.reader();
    assert_eq!(reader.try_lookup(&trunk[3].hash()), Ok(Some(x)), "below the watermark");
    store.faults(|f| f.corrupt(x, Corrupt::Id(next)));
    reopened.shared.invalidate_cold(ColdInvalidation::Clear);
    match reader.try_lookup(&trunk[3].hash()) {
        Err(ChainError::Storage(e)) => {
            assert!(e.what().contains("never issued"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    assert!(latch.is_tripped());
}

/// `open`'s error over `store`, and whether it tripped the latch.
fn open_err_in<S: HeaderStore>(p: &Arc<ChainSetup>, store: S) -> (OpenError, bool) {
    let latch = Arc::new(Latch::default());
    let svc = Services {
        fail: latch.clone(),
        ..services(p)
    };
    let r = HeaderChain::<S, Evicting>::open(p.clone(), ChainConfig::spv(p, 10), svc, store);
    (r.err().expect("refused"), latch.is_tripped())
}

fn open_err(p: &Arc<ChainSetup>, store: &MemHeaderStore) -> (OpenError, bool) {
    open_err_in(p, store.clone())
}

fn corruption_in<S: HeaderStore>(p: &Arc<ChainSetup>, store: S) -> Corruption {
    match open_err_in(p, store) {
        (OpenError::Corrupt(c), true) => c,
        other => panic!("not a corruption that trips the latch: {other:?}"),
    }
}

fn corruption(p: &Arc<ChainSetup>, store: &MemHeaderStore) -> Corruption {
    corruption_in(p, store.clone())
}

/// The refusals that need no records: an empty store, another schema, the node's mode.
#[test]
fn open_refuses_no_chain() {
    let p = params(Network::Regtest);
    let store = MemHeaderStore::new();
    assert!(matches!(open_err(&p, &store), (OpenError::Empty, false)));
    let pack = regtest_pack(&p);
    store
        .reset(ChainMeta {
            schema: CHAIN_SCHEMA - 1,
            ..meta(pack.hash, pack.height)
        })
        .expect("reset");
    let (e, tripped) = open_err(&p, &store);
    assert!(matches!(e, OpenError::Schema(s, CHAIN_SCHEMA) if s == CHAIN_SCHEMA - 1) && !tripped);
    let node = Spv::open(p.clone(), ChainConfig::node(&p), services(&p), store.clone());
    assert!(matches!(node, Err(OpenError::Config(_))));
}

/// Each corruption trips the latch: a stored id at `next_id`, a failed tip. (Each store is
/// cut back to the durable `stepped` image before the next case.)
#[test]
fn open_refuses_inconsistent_meta_and_rows() {
    let (p, chain, mut store, trunk, _) = stepped();
    let m = meta_delta(&store);
    let tip = id_of(&chain, &trunk[29]);
    drop(chain);
    let crafted = |next_id: u32| StepTail {
        active: None,
        cl: None,
        meta: MetaDelta {
            next_id: BlockId(next_id),
            ..m
        },
    };
    commit_tail(&mut store, &[], crafted(m.next_id.0 - 1));
    assert_eq!(corruption(&p, &store), Corruption::NextIdNotAbove(BlockId(m.next_id.0 - 1)));

    let mut store = store.power_loss_keeping(0);
    let failed = Status(Status::VALID_TREE | Status::FAILED_VALID);
    commit_tail(&mut store, &[StepRow::Status(tip, failed)], crafted(m.next_id.0));
    assert_eq!(corruption(&p, &store), Corruption::TipNotLeafPath(tip));
}

/// A tip with no record: the meta row of a store whose records are gone.
#[test]
fn open_refuses_a_tip_with_no_record() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let store = MemHeaderStore::new();
    store
        .reset(ChainMeta {
            next_id: BlockId(40),
            tip: BlockId(30),
            ..meta(pack.hash, pack.height)
        })
        .expect("reset");
    assert_eq!(corruption(&p, &store), Corruption::MissingRecord(BlockId(30)));
}

/// An anchor no pack has: genesis above 0, or another block at 0 or inside DGW's window.
#[test]
fn open_refuses_an_anchor_off_its_heights() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let genesis = Header::new(dash_consensus::params::genesis_block(&p).header).hash();
    for (hash, height) in [(genesis, 24), (pack.hash, 0), (pack.hash, 1), (pack.hash, 23)] {
        let store = MemHeaderStore::new();
        store.reset(meta(hash, height)).expect("reset");
        assert_eq!(corruption(&p, &store), Corruption::Anchor(height), "{height}");
    }
}

type RowsEdit = Box<dyn Fn(&mut Vec<(u32, BlockId)>) + Send + Sync>;
type RecordsEdit = Box<dyn Fn(&mut Vec<HeaderRecord>) + Send + Sync>;
type RecordEdit = Box<dyn Fn(&mut HeaderRecord) + Send + Sync>;

/// `MemHeaderStore` with some reads edited, as damaged SQLite tables or indexes would read.
struct Tampered {
    store: MemHeaderStore,
    /// Each record read by id or hash, so `walk_down` and the cold reads too.
    records: RecordEdit,
    active: RowsEdit,
    from_height: RecordsEdit,
    leaves: RecordsEdit,
}

impl Tampered {
    fn new(store: &MemHeaderStore) -> Tampered {
        Tampered {
            store: store.clone(),
            records: Box::new(|_| ()),
            active: Box::new(|_| ()),
            from_height: Box::new(|_| ()),
            leaves: Box::new(|_| ()),
        }
    }

    fn edited(&self, rec: Option<HeaderRecord>) -> Option<HeaderRecord> {
        rec.map(|mut r| {
            (self.records)(&mut r);
            r
        })
    }
}

impl HeaderStore for Tampered {
    fn by_hash(&self, h: &BlockHash) -> Result<Option<HeaderRecord>, StoreError> {
        Ok(self.edited(self.store.by_hash(h)?))
    }
    fn by_id(&self, id: BlockId) -> Result<Option<HeaderRecord>, StoreError> {
        Ok(self.edited(self.store.by_id(id)?))
    }
    fn active_at(&self, height: u32) -> Result<Option<BlockId>, StoreError> {
        self.store.active_at(height)
    }
    fn from_height(&self, h: u32) -> Result<Vec<HeaderRecord>, StoreError> {
        let mut recs = self.store.from_height(h)?;
        (self.from_height)(&mut recs);
        Ok(recs)
    }
    fn leaves_from(&self, from: BlockId, out: &mut [(BlockId, Work)]) -> Result<usize, StoreError> {
        let mut recs = self.store.leaves()?;
        (self.leaves)(&mut recs);
        let page = recs.iter().filter(|r| r.id >= from).map(|r| (r.id, r.chain_work));
        Ok(out.iter_mut().zip(page).map(|(slot, leaf)| *slot = leaf).count())
    }
    fn children_from(
        &self,
        prev: &BlockHash,
        from: BlockId,
        out: &mut [BlockId],
    ) -> Result<usize, StoreError> {
        self.store.children_from(prev, from, out)
    }
    fn chain_meta(&self) -> Result<Option<ChainMeta>, StoreError> {
        self.store.chain_meta()
    }
    fn active_from(&self, lo: u32) -> Result<Vec<(u32, BlockId)>, StoreError> {
        let mut rows = self.store.active_from(lo)?;
        (self.active)(&mut rows);
        Ok(rows)
    }
    fn max_id(&self) -> Result<Option<BlockId>, StoreError> {
        self.store.max_id()
    }
}

/// `active` rows that are not the tip's ancestry: a stale id in the window, a stale id at the
/// height below it, a missing row below it, a row past the tip, dense rows from below the floor.
#[test]
fn open_refuses_active_rows_off_the_tip() {
    let (p, chain, store, trunk, stale) = stepped();
    let other = id_of(&chain, &trunk[3]);
    drop(chain);
    // The tip is at ANCHOR + 30, so the window starts at ANCHOR + 20; rows start at the anchor.
    let row = |h: u32| (h - ANCHOR) as usize;
    let cases: [(RowsEdit, u32); 5] = [
        (Box::new(move |r| r[row(ANCHOR + 25)].1 = other), ANCHOR + 25),
        (Box::new(move |r| r[row(ANCHOR + 19)].1 = stale[1]), ANCHOR + 19),
        (Box::new(move |r| _ = r.remove(row(ANCHOR + 7))), ANCHOR + 7),
        (Box::new(|r| r.push((ANCHOR + 31, BlockId(0)))), ANCHOR + 30),
        (
            Box::new(|r| _ = r.splice(0..0, (BELOW_FLOOR..ANCHOR).map(|h| (h, BlockId(0))))),
            BELOW_FLOOR,
        ),
    ];
    for (edit, height) in cases {
        let tampered = Tampered {
            active: edit,
            ..Tampered::new(&store)
        };
        assert_eq!(
            corruption_in(&p, tampered),
            Corruption::ActiveMismatch {
                height
            }
        );
    }
}

/// A height index that misses records: the tip (`MissingRecord`) or a window ancestor (its
/// child's `MissingParent`), each found by id and hash but not by height.
#[test]
fn open_refuses_records_missing_from_the_window() {
    let (p, chain, store, trunk, _) = stepped();
    let (tip, mid, child) =
        (id_of(&chain, &trunk[29]), id_of(&chain, &trunk[24]), id_of(&chain, &trunk[25]));
    drop(chain);
    for (gone, want) in
        [(tip, Corruption::MissingRecord(tip)), (mid, Corruption::MissingParent(child))]
    {
        let tampered = Tampered {
            from_height: Box::new(move |recs| recs.retain(|r| r.id != gone)),
            ..Tampered::new(&store)
        };
        assert_eq!(corruption_in(&p, tampered), want);
    }
}

/// A leaf index without the tip's subtree leaves no path as heavy as the tip.
#[test]
fn open_refuses_leaves_lighter_than_the_tip() {
    let (p, chain, store, trunk, _) = stepped();
    let tip = id_of(&chain, &trunk[29]);
    drop(chain);
    let tampered = Tampered {
        leaves: Box::new(move |recs| recs.retain(|r| r.id != tip)),
        ..Tampered::new(&store)
    };
    assert_eq!(corruption_in(&p, tampered), Corruption::TipNotLeafPath(tip));
}

/// A window record whose parent is not stored, or whose skip names itself, a later window
/// record or an id never issued, is corruption.
#[test]
fn open_refuses_corrupt_records() {
    let (p, chain, store, trunk, _) = stepped();
    let (id, later) = (id_of(&chain, &trunk[25]), id_of(&chain, &trunk[28]));
    let next_id = chain.next_id;
    drop(chain);
    store.faults(|f| f.corrupt(id, Corrupt::MissingParent));
    assert_eq!(corruption(&p, &store), Corruption::MissingParent(id));
    let store = store.power_loss_keeping(0);
    store.faults(|f| *f = Default::default());
    store.faults(|f| f.corrupt(id, Corrupt::BadSkip));
    assert_eq!(corruption(&p, &store), Corruption::SkipHeight(id));
    store.faults(|f| *f = Default::default());
    for skip in [later, BlockId(next_id)] {
        let tampered = Tampered {
            from_height: Box::new(move |recs| {
                recs.iter_mut().filter(|r| r.id == id).for_each(|r| r.skip = Some(skip));
            }),
            ..Tampered::new(&store)
        };
        assert_eq!(corruption_in(&p, tampered), Corruption::SkipHeight(id), "{skip:?}");
    }
}

/// Window records off their links' heights, which the in-memory walks would panic on (as Core
/// asserts): a record one above its parent's next height, and a skip to a resident or a cold
/// record off `GetSkipHeight`. Each is refused as corruption, with the latch tripped.
#[test]
fn open_refuses_window_links_off_their_heights() {
    let (p, chain, store, trunk, _) = stepped();
    let id = id_of(&chain, &trunk[25]);
    let h = store.by_id(id).expect("read").expect("stored").height;
    // `open` windows at `tip − 10`, as `open_err_in` configures it: `h − 1` is resident there,
    // and `base − 1` is cold; neither is at `GetSkipHeight(h)`.
    let base = chain.private_tip().expect("a tip").0.height - 10;
    let sh = walk::skip_height(h);
    assert!(base < h - 1 && base - 1 > chain.floor && sh >= chain.floor);
    assert!(![h - 1, base - 1].contains(&sh));
    let at = |height| chain.reader().try_ancestor(id, height).expect("read").expect("an ancestor");
    let (resident, cold) = (at(h - 1), at(base - 1));
    drop(chain);
    // Each case edits the window load's copy of `id`'s record.
    let window_edit = |edit: fn(&mut HeaderRecord, BlockId), arg: BlockId| Tampered {
        from_height: Box::new(move |recs| {
            recs.iter_mut().filter(|r| r.id == id).for_each(|r| edit(r, arg));
        }),
        ..Tampered::new(&store)
    };
    let raised = window_edit(|r, _| r.height += 1, id);
    assert_eq!(corruption_in(&p, raised), Corruption::MissingParent(id));
    for skip in [resident, cold] {
        let tampered = window_edit(|r, s| r.skip = Some(s), skip);
        assert_eq!(corruption_in(&p, tampered), Corruption::SkipHeight(id), "{skip:?}");
    }
}

/// Records whose heights disagree with the rows: a tip below the floor, and the cold run under
/// the lowest row shifted up one, so `walk_down`'s top is above the lowest row's height.
#[test]
fn open_refuses_records_off_their_heights() {
    let (p, chain, store, _, _) = stepped();
    let tip = chain.private_tip().expect("a tip").0.id;
    drop(chain);
    let cases: [(RecordEdit, u32); 2] = [
        (
            Box::new(move |r| {
                if r.id == tip {
                    r.height = BELOW_FLOOR
                }
            }),
            BELOW_FLOOR,
        ),
        (
            Box::new(|r| {
                if r.height <= ANCHOR {
                    r.height += 1
                }
            }),
            ANCHOR,
        ),
    ];
    for (edit, height) in cases {
        let tampered = Tampered {
            records: edit,
            ..Tampered::new(&store)
        };
        assert_eq!(
            corruption_in(&p, tampered),
            Corruption::ActiveMismatch {
                height
            }
        );
    }
}

/// Step 7 walks a FAILED leaf's cold path by each parent's hash; a parent off the height below
/// is refused, so corrupt records that link back into a FAILED cycle cannot loop it. SB4's cold
/// read (`cold_parent`) refuses it first as a store corruption (`Corruption::Store`), with the
/// latch tripped; step 7's own check stays as a second line.
#[test]
fn open_refuses_a_failed_path_off_its_heights() {
    let (p, chain, store, _, stale) = stepped();
    drop(chain);
    let leaf_hash = store.by_id(stale[1]).expect("read").expect("stored").hash;
    let tampered = Tampered {
        records: Box::new(move |r| {
            if stale.contains(&r.id) {
                r.status = Status(r.status.bits() | Status::FAILED_VALID);
            }
            if r.id == stale[0] {
                // The branch is at heights 16 and 17 over `trunk[15]`: its first record's
                // parent is now its own child, one height above it.
                r.header.prev_hash = leaf_hash;
            }
        }),
        ..Tampered::new(&store)
    };
    match open_err_in(&p, tampered) {
        (OpenError::Corrupt(Corruption::Store(what)), true) => {
            let want = format!("record {:?} at height", stale[0]);
            assert!(what.starts_with(&want) && what.contains(": parent"), "{what}");
        }
        other => panic!("not refused by the cold read with the latch tripped: {other:?}"),
    }
}

/// The ChainLock best and floor come back from `meta`.
#[test]
fn open_restores_the_chainlock_state() {
    let (p, chain, mut store, trunk) = trunk10();
    let floor = id_of(&chain, &trunk[7]);
    let best = ChainLockRef {
        height: ANCHOR + 9,
        hash: trunk[8].hash(),
    };
    let m = meta_delta(&store);
    commit_tail(
        &mut store,
        &[],
        StepTail {
            active: None,
            cl: Some(ClPersisted {
                best: Some(best),
                floor: Some(floor),
                enabled_observed: false,
            }),
            meta: m,
        },
    );
    let b = reopen(&p, &store);
    assert_eq!(b.cl_best, Some(best));
    let f = b.cl_floor.expect("a floor");
    assert_eq!((f.id, f.hash, f.height), (floor, trunk[7].hash(), ANCHOR + 8));
}

/// Every read `open` makes can fail: `Storage`, and the latch trips.
#[test]
fn open_read_failures_trip_the_latch() {
    let (p, _chain, store, _, _) = stepped();
    for kind in [
        ReadKind::ChainMeta,
        ReadKind::MaxId,
        ReadKind::ById,
        ReadKind::FromHeight,
        ReadKind::ByHash,
        ReadKind::ActiveFrom,
        ReadKind::WalkDown,
        ReadKind::Leaves,
    ] {
        store.faults(|f| f.fail_read(kind, 1));
        let (e, tripped) = open_err(&p, &store);
        assert!(matches!(e, OpenError::Storage(StoreError::Io(_))) && tripped, "{kind:?}: {e:?}");
        assert!(store.faults(|f| f.reads(kind)) > 0);
    }
    reopen(&p, &store);
}

/// P5: after a power loss keeps the first of two Buffered steps, `open` restores the cut's
/// `next_id`; the lost ids are issued again to other headers, and only those are ever stored
/// under them.
#[test]
fn p5_reopen_after_a_lost_suffix() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = committed(&p, &pack, services(&p));
    let trunk = mine_n(&p, pack.hash, START, 20);
    real_step(&mut chain, &mut store, &trunk[..10]);
    store.sync_wal().expect("sync");
    real_step(&mut chain, &mut store, &trunk[10..15]);
    let cut = chain.next_id;
    real_step(&mut chain, &mut store, &trunk[15..]);
    let lost: Vec<BlockId> = (cut..chain.next_id).map(BlockId).collect();
    let mut store = store.power_loss_keeping(1);
    drop(chain);

    let mut b = reopen(&p, &store);
    assert_eq!(b.next_id, cut);
    assert_eq!(b.private_tip().expect("a tip").0.id, id_of(&b, &trunk[14]));
    let alt = mine_n(&p, trunk[14].hash(), at(15) + 11, 8);
    real_step(&mut b, &mut store, &alt);
    check(&b, &store);
    for (id, h) in lost.iter().zip(&alt) {
        assert_eq!(store.by_id(*id).expect("read").expect("reissued").hash, h.hash());
    }
    store.sync_wal().expect("sync");
    let store = store.power_loss();
    let c = reopen(&p, &store);
    assert_eq!(
        (c.private_tip().map(|t| t.0), c.best_header, c.next_id),
        (b.private_tip().map(|t| t.0), b.best_header, b.next_id)
    );
    check(&c, &store);
}

/// The reopened window serves heights below it through the cold path, and a step with nothing
/// new leaves the reopened chain as it was.
#[test]
fn reopened_snapshot_reads_cold_heights() {
    let (p, chain, store, trunk, _) = stepped();
    let b = reopen_same(&p, &chain, &store);
    let s = b.feeds().feed_snapshot();
    let low = s.at_height_checked(ANCHOR + 1).expect("read").expect("held");
    assert_eq!(low.hash, trunk[0].hash());
    assert!(b.shared.arena.imm(low.id, |_| ()).is_none(), "cold");
    drop(chain);
    // A step with nothing new on a reopened chain commits nothing new.
    let (mut c, mut s) = (reopen(&p, &store), store.clone());
    stages(&mut c, &mut s);
    check(&c, &s);
    reopen_same(&p, &b, &store);
}

mod cl;
mod kinds;
