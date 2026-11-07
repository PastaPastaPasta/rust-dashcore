//! A-F12 (SB5 §10.4, DEC-155, DEC-157 (3)): the SPV step's observed memory is within §6.2's
//! cap, under a counting global allocator. The header writer's thread counts what it allocates
//! during a step, except while the `MemHeaderStore` works (its tables stand in for SQLite's
//! pages, outside the budget). A step's observed size is its peak of live counted bytes above
//! the larger of its start and its end: what stays allocated after the step (new resident
//! entries, cold cache records, the published snapshot's chunks) is residency (§6.3), and the
//! rest is the step's. Pre-step bytes freed before the peak, and residency allocated after it,
//! understate the peak by their size; in these steps both happen after the peak (the cache
//! clear and the overlay's drain are stage 5's). It lives here because `dash-chain` forbids
//! `unsafe`.
//!
//! DEC-187 amendment 2 splits the count into two accounts, and each counted allocation is
//! charged to exactly one:
//! - **residency** (§6.3): what the chain allocates while it grows its resident set (the
//!   arena's chunks and tables, the hash index's table, SPV's resident-height set and chunk
//!   counts, the active chain's fresh nodes), and those blocks' frees wherever they happen on
//!   this thread. The thread also keeps their total from its first allocation on, counted or
//!   not, which §6.3's formula bounds; a block freed on another thread stays in it, which only
//!   overstates it;
//! - **step** (§6.2): everything else.
//!
//! Only two things are in neither: the store's work (gated, as above), and a caller's
//! `HeaderBatch` built before the measured call ([`exclude`]), the host's decoded message
//! (amendment 2 (b)), whose free inside `accept` is not counted. The cases that exclude a batch
//! assert that exactly its two buffers went uncounted.

#![cfg(not(loom))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};

use dash_chain::test_util::{
    ARENA_CHUNK_BYTES, ARENA_ENTRY_BYTES, ARENA_TABLE_BYTES, DeepActive, OVERLAY_BUCKET_BYTES,
    RESIDENT_INTERNAL_BYTES, RESIDENT_LEAF_BYTES, RIG_ANCHOR, RigMark, SpvRig, active_bytes, drain,
    grown_table_bytes, in_residency, index_bytes, index_table_bytes, resident_bytes, subscribe,
};
use dash_chain::{
    ACTIVE_PATHS_BYTES, BlockId, ChainLockRef, ControlKind, ControlMsg, FeedConfig, HeaderBatch,
    MAX_CONTROL_KINDS, MAX_FEEDS, Needs, ReadKind, Refused, STEP_FIXED_BYTES, Status, per_feed_cap,
    registry_bytes, step_cap,
};
use dash_consensus::primitives::Header;
use dash_primitives::BlockHash;

/// Slots for the residency blocks a thread holds at once, open-addressed: a chunk is two, the
/// index, the chunk counts and the arena's second-level table one each, the resident set one
/// per B-tree node and the active chain one per node, ≈ 2,900 at w = 10,000 and B = 2,000.
const HELD_SLOTS: usize = 8_192;
/// Held blocks past this many are lost (the case fails), so a probe always meets an empty slot.
const HELD_MAX: usize = HELD_SLOTS / 4 * 3;

thread_local! {
    /// Whether this thread's allocations count.
    static ON: Cell<bool> = const { Cell::new(false) };
    /// Live counted bytes, and their peak since the last reset: both accounts.
    static LIVE: Cell<(i64, i64)> = const { Cell::new((0, 0)) };
    /// The step account, counted the same way.
    static STEP: Cell<(i64, i64)> = const { Cell::new((0, 0)) };
    /// The residency account, counted the same way.
    static RES: Cell<(i64, i64)> = const { Cell::new((0, 0)) };
    /// Every residency block this thread holds, counted or not: (address, size).
    static HELD: RefCell<[(usize, usize); HELD_SLOTS]> =
        const { RefCell::new([(0, 0); HELD_SLOTS]) };
    /// The held blocks' totals.
    static HELD_TOTALS: Cell<HeldTotals> = const { Cell::new(HeldTotals::NONE) };
    /// The caller's batch buffers, freed uncounted, and the bytes freed so.
    static EXCLUDED: Cell<([usize; 2], usize)> = const { Cell::new(([0; 2], 0)) };
}

/// A thread-local count: live bytes, and their peak since the last reset.
type Account = std::thread::LocalKey<Cell<(i64, i64)>>;

/// The held residency blocks' bytes and count, the peak of the bytes since the last reset, and
/// whether a block found no slot.
#[derive(Clone, Copy)]
struct HeldTotals {
    bytes: usize,
    peak: usize,
    count: usize,
    lost: bool,
}

impl HeldTotals {
    const NONE: HeldTotals = HeldTotals {
        bytes: 0,
        peak: 0,
        count: 0,
        lost: false,
    };
}

fn bump(account: &'static Account, bytes: i64) {
    let _ = account.try_with(|c| {
        let (live, peak) = c.get();
        c.set((live + bytes, peak.max(live + bytes)));
    });
}

/// Counts `bytes` in the total and in the residency account if `res`, else the step account.
fn add(bytes: i64, res: bool) {
    if ON.try_with(Cell::get).unwrap_or(false) {
        let account = if res {
            &RES
        } else {
            &STEP
        };
        bump(&LIVE, bytes);
        bump(account, bytes);
    }
}

/// `p`'s first slot in `HELD`.
fn home(p: usize) -> usize {
    (p >> 4).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (usize::BITS - HELD_SLOTS.trailing_zeros())
}

/// Holds a residency block.
fn hold(p: *mut u8, size: usize) {
    let _ = HELD_TOTALS.try_with(|c| {
        let mut t = c.get();
        if t.count == HELD_MAX {
            t.lost = true;
            c.set(t);
            return;
        }
        let _ = HELD.try_with(|h| {
            let mut h = h.borrow_mut();
            let mut i = home(p as usize);
            while h[i].0 != 0 {
                i = (i + 1) % HELD_SLOTS;
            }
            h[i] = (p as usize, size);
        });
        t.bytes += size;
        t.peak = t.peak.max(t.bytes);
        t.count += 1;
        c.set(t);
    });
}

/// Releases `p` if it is a held residency block; whether it was. The slots after it that
/// probed past it shift back.
fn release(p: *mut u8) -> bool {
    let size = HELD.try_with(|h| {
        let mut h = h.borrow_mut();
        let mut i = home(p as usize);
        while h[i].0 != p as usize {
            if h[i].0 == 0 {
                return None;
            }
            i = (i + 1) % HELD_SLOTS;
        }
        let size = h[i].1;
        let mut j = i;
        loop {
            j = (j + 1) % HELD_SLOTS;
            if h[j].0 == 0 {
                break;
            }
            // `h[j]` may fill the hole at `i` when its first slot is not in (i, j].
            let k = home(h[j].0);
            if (i < j && (k <= i || k > j)) || (j < i && k <= i && k > j) {
                h[i] = h[j];
                i = j;
            }
        }
        h[i] = (0, 0);
        Some(size)
    });
    let Ok(Some(size)) = size else {
        return false;
    };
    let _ = HELD_TOTALS.try_with(|c| {
        let mut t = c.get();
        t.bytes -= size;
        t.count -= 1;
        c.set(t);
    });
    true
}

/// Whether `p`, of `size` bytes, is one of the caller's batch buffers, which it then forgets.
fn excluded(p: *mut u8, size: usize) -> bool {
    EXCLUDED
        .try_with(|e| {
            let (mut ps, bytes) = e.get();
            let hit = ps.iter_mut().find(|q| **q == p as usize).map(|q| *q = 0).is_some();
            e.set((ps, bytes + usize::from(hit) * size));
            hit
        })
        .unwrap_or(false)
}

/// Leaves `batch`'s buffers out of both accounts: the caller built it before the step.
fn exclude(batch: &HeaderBatch) {
    EXCLUDED
        .with(|e| e.set(([batch.headers.as_ptr() as usize, batch.hashes.as_ptr() as usize], 0)));
}

/// The bytes freed uncounted since [`exclude`], once both of its buffers were.
fn excluded_bytes() -> Option<usize> {
    let (ps, bytes) = EXCLUDED.with(Cell::get);
    (ps == [0; 2]).then_some(bytes)
}

fn set_on(on: bool) {
    ON.with(|c| c.set(on));
}

/// Resets the counts and starts counting.
fn start() {
    for c in [&LIVE, &STEP, &RES] {
        c.with(|c| c.set((0, 0)));
    }
    HELD_TOTALS.with(|c| {
        let t = c.get();
        c.set(HeldTotals {
            peak: t.bytes,
            ..t
        });
    });
    set_on(true);
}

/// A step's `marks` that stops counting while the store works.
fn gate(m: RigMark) {
    match m {
        RigMark::StoreIn => set_on(false),
        RigMark::StoreOut => set_on(true),
        _ => {}
    }
}

/// A block's allocation: residency if the chain is growing it.
fn allocated(p: *mut u8, size: usize) {
    if p.is_null() {
        return;
    }
    let res = in_residency();
    if res {
        hold(p, size);
    }
    add(size as i64, res);
}

/// A block's free: residency if it was held, nothing if the caller's batch.
fn freed(p: *mut u8, size: usize) {
    if release(p) {
        add(-(size as i64), true);
    } else if !excluded(p, size) {
        add(-(size as i64), false);
    }
}

struct Counting;

#[global_allocator]
static COUNTING: Counting = Counting;

// SAFETY: every method forwards to `System` with the caller's arguments; the counting touches
// only const-initialized thread-locals, which never allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        allocated(p, l.size());
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        freed(p, l.size());
        unsafe { System.dealloc(p, l) }
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        allocated(p, l.size());
        p
    }

    /// A growing buffer counts as its old and new buffers together, as a copying realloc holds
    /// them, then the old one leaves. The new buffer is residency if the old one was or the
    /// chain is growing it; the old one is classified before `q`, which may be `p`, is held.
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let held = release(p);
        let batch = !held && excluded(p, l.size());
        let q = unsafe { System.realloc(p, l, n) };
        if q.is_null() {
            if held {
                hold(p, l.size());
            }
            return q;
        }
        let res = held || in_residency();
        if res {
            hold(q, n);
        }
        add(n as i64, res);
        if !batch {
            add(-(l.size() as i64), held);
        }
        q
    }
}

/// The observed size of one `step` call: the peak of live counted bytes above the larger of
/// the start and the end. The store's work is not counted, nor is `on`, which hears every
/// mark (the test's readers drain at `Wait`).
fn observe<T>(
    step: impl FnOnce(&mut dyn FnMut(RigMark)) -> T,
    on: &mut dyn FnMut(RigMark),
) -> (T, usize) {
    let (out, accounts) = observe_accounts(step, on);
    (out, accounts.both)
}

/// One step's observation, by account.
#[derive(Debug)]
struct Accounts {
    /// Both accounts together: [`observe`]'s.
    both: usize,
    /// The step account's observed size.
    step: usize,
    /// The residency account's observed size.
    residency: usize,
    /// The peak of the thread's held residency blocks during the step.
    held_peak: usize,
}

/// The observed size of one `step` call per account, each as [`observe`] takes it.
fn observe_accounts<T>(
    step: impl FnOnce(&mut dyn FnMut(RigMark)) -> T,
    on: &mut dyn FnMut(RigMark),
) -> (T, Accounts) {
    start();
    let out = step(&mut |m| {
        let was = ON.with(|c| c.replace(false));
        on(m);
        set_on(was);
        gate(m);
    });
    set_on(false);
    let observed = |account: &'static Account| {
        let (end, peak) = account.with(Cell::get);
        (peak - end.max(0)) as usize
    };
    let held = HELD_TOTALS.with(Cell::get);
    assert!(!held.lost, "a residency block found no slot");
    let accounts = Accounts {
        both: observed(&LIVE),
        step: observed(&STEP),
        residency: observed(&RES),
        held_peak: held.peak,
    };
    (out, accounts)
}

/// Counts what `f` allocates, net.
fn counted<T>(f: impl FnOnce() -> T) -> (T, i64) {
    start();
    let out = f();
    set_on(false);
    (out, LIVE.with(Cell::get).0)
}

/// `peak_deep_reorg_paused_readers` (Sol's witness): w = 2,048, Q = E = W = 256 (mobile),
/// branches A and B of `depth` headers each forking at the anchor, `readers` subscribers that
/// take nothing until the writer waits on them, and one header on B that switches: d = depth,
/// c = depth + 1. The writer waits, the observed step is ≤ the step cap, and so is the budget's
/// own sub-step peak; after the readers drain, every event reached each of them and nothing is
/// pending. Returns the observed step and the budget's peak.
fn deep_reorg(depth: u32, readers: usize) -> (usize, usize) {
    let feeds = FeedConfig::spv_mobile();
    let cold = 256;
    let cap = step_cap(&feeds, cold);
    let mut rig = SpvRig::new(2_048, feeds, cold, false);
    let a = rig.mine_on_anchor(0, depth);
    let b = rig.mine_on_anchor(7, depth + 1);
    rig.extend(&a);
    rig.extend(&b[..depth as usize]);
    let hub = rig.chain.feeds();
    let mut subs: Vec<_> = (0..readers).map(|_| subscribe(&hub).1).collect();
    let (mut got, mut waits) = (vec![0; readers], 0);
    let last = &b[depth as usize..];
    let (summary, observed) = observe(|marks| rig.step(last, &[], marks), &mut |m| {
        if m == RigMark::Wait {
            waits += 1;
            for (f, n) in subs.iter_mut().zip(&mut got) {
                *n += drain(&mut **f);
            }
        }
    });
    let summary = summary.expect("the switch");
    assert_eq!((summary.disconnected, summary.connected), (depth, depth + 1));
    assert!(waits > 0, "the paused readers held the writer");
    for (f, n) in subs.iter_mut().zip(&mut got) {
        *n += drain(&mut **f);
    }
    rig.settle(&mut subs);
    for (f, n) in subs.iter_mut().zip(&mut got) {
        *n += drain(&mut **f);
        assert_eq!(*n, 2 * depth as usize + 1, "every event reached the reader");
    }
    let stats = rig.chain.step_budget();
    assert_eq!(stats.cap, cap);
    assert!(stats.peak <= cap, "the budget's own peak {} ≤ {cap}", stats.peak);
    assert!(observed <= cap, "observed {observed} ≤ {cap}");
    (observed, stats.peak)
}

#[test]
fn peak_deep_reorg_paused_readers() {
    let five = deep_reorg(9_000, 5);
    assert_eq!(deep_reorg(9_000, 1), five, "independent of the readers");
    eprintln!("A-F12 deep reorg, 9,000: (observed, budget peak) = {five:?}");
}

/// The 50,000-deep rerun: the maximum does not grow with d.
#[test]
#[ignore = "nightly: mines 118,002 regtest headers"]
fn peak_deep_reorg_paused_readers_50k() {
    let deep = deep_reorg(50_000, 1);
    assert_eq!(deep, deep_reorg(9_000, 1), "independent of d");
    eprintln!("A-F12 deep reorg, 50,000: {deep:?}");
}

/// The status overlay's bucket count for `m` entries, at least the 128 of the 64 entries it
/// keeps between steps.
fn overlay_buckets(m: usize) -> usize {
    (m * 8 / 7).next_power_of_two().max(128)
}

/// §6.2's M(m): the status overlay's old and new tables during its last rehash.
fn marks_term(m: usize) -> usize {
    let b = overlay_buckets(m);
    OVERLAY_BUCKET_BYTES * (b + b / 2) + 32
}

/// The enforcement's id lists, DEC-187's L(m): 12 B per slot of the next power of two, two
/// `Vec<BlockId>` that grow in step with their old and new buffers. SPV now keeps one, the
/// step's `VirtualTip`'s (the `EnforceReport`'s stays empty), and the bound keeps both.
fn lists_term(m: usize) -> usize {
    12 * m.next_power_of_two()
}

/// `peak_mark_walk_write_batches` at `m` marks: A of `m` headers is active at w = 2,048, B of
/// 50 forks at the anchor, and a lock on B's end makes the step's enforcement disconnect and
/// mark all of A, nearly all of it cold, reading fewer records than it marks (the overlay keeps
/// ids and statuses, not records). The sink gets ⌈marks / W⌉ batches of at most W rows;
/// the step's observed size is ≤ cap + M(m) + the enforcement's id lists, and the marks
/// account's peak ≤ M(m); the invalidation list overflows, so the whole cache is cleared; the
/// reopened store holds the marks. Returns the observed step, the marks peak, M(m) and the
/// step's by-id reads.
fn mark_walk(m: u32) -> (usize, usize, usize, u64) {
    let feeds = FeedConfig::spv_mobile();
    let (w, cold) = (feeds.write_batch as usize, 256);
    let cap = step_cap(&feeds, cold);
    let mut rig = SpvRig::new(2_048, feeds, cold, true);
    let a = rig.mine_on_anchor(0, m);
    let b = rig.mine_on_anchor(7, 50);
    rig.extend(&a);
    rig.extend(&b);
    let lock = ChainLockRef {
        height: RIG_ANCHOR + 50,
        hash: b[49].hash(),
    };
    // The anchor, cold and not marked: a lookup by hash caches it, and only a clear of the
    // whole cache drops it.
    let anchor = rig.chain.reader().try_lookup(&rig.anchor.hash()).expect("read").expect("held");
    let reads = |rig: &SpvRig| rig.store.faults(|f| f.reads(ReadKind::ById));
    let before = reads(&rig);
    let mut batches = Vec::new();
    let (summary, observed) = observe(|marks| rig.step(&[], &[lock], marks), &mut |mark| {
        if let RigMark::Rows(n) = mark {
            batches.push(n);
        }
    });
    let step_reads = reads(&rig) - before;
    let summary = summary.expect("the lock's step");
    assert!(step_reads < u64::from(m), "{step_reads} cold reads for {m} marks");
    assert_eq!((summary.disconnected, summary.connected), (m, 50));
    let rows: usize = batches.iter().sum();
    assert_eq!(rows, m as usize, "one status row per mark, no new record");
    assert_eq!(batches.len(), rows.div_ceil(w));
    assert!(batches.iter().all(|&n| n <= w));
    let stats = rig.chain.step_budget();
    let (mm, lists) = (marks_term(m as usize), lists_term(m as usize));
    assert!(stats.marks_peak <= mm, "marks peak {} ≤ M = {mm}", stats.marks_peak);
    // The account is the new table exactly, so one sized from another entry type fails here.
    let table = OVERLAY_BUCKET_BYTES * overlay_buckets(m as usize) + 16;
    assert_eq!(stats.marks_peak, table, "the marks account is the overlay's table");
    let bound = cap + mm + lists;
    assert!(observed <= bound, "observed {observed} ≤ cap + M + lists = {bound}");

    let before = reads(&rig);
    rig.chain.reader().try_entry(anchor).expect("read").expect("held");
    assert_eq!(reads(&rig), before + 1, "the whole cache was cleared");

    let reopened = rig.reopen().expect("open");
    let r = reopened.reader();
    for h in &a {
        let id = r.try_lookup(&h.hash()).expect("read").expect("held");
        let e = r.try_entry(id).expect("read").expect("held");
        assert!(e.status.has(Status::CONFLICT_CHAINLOCK), "{id:?} is marked after the reopen");
    }
    (observed, stats.marks_peak, mm, step_reads)
}

#[test]
fn peak_mark_walk_write_batches() {
    let r = mark_walk(9_000);
    eprintln!("A-F12 mark walk, 9,000: (observed, marks peak, M, by-id reads) = {r:?}");
}

/// The slope at 50,000: M(m) allows ~26 B per mark (the account measures ~17), and L(m) 12
/// to 24.
#[test]
#[ignore = "nightly: mines 50,050 regtest headers"]
fn peak_mark_walk_write_batches_50k() {
    let r = mark_walk(50_000);
    eprintln!("A-F12 mark walk, 50,000: (observed, marks peak, M, by-id reads) = {r:?}");
}

/// `peak_wide_tree` (Sol's R1-M1 witness): w = 32, `width` one-header siblings of the anchor,
/// then a trunk of 100 on it, so every sibling leaves the window as a stored leaf. A lock on the
/// last sibling marks the trunk and every other sibling, reading the anchor's children from the
/// store; the step after it, with nothing new, still reads every cold leaf, the CONFLICT ones
/// keeping their work. Both read the store a page at a time, so each observed step is ≤
/// cap + M(m) + L(m), m its marks, with no term for the tree's width. With `reopen`, the
/// chain is reopened from its store before the lock; a quiet step reruns after a reopen.
fn wide_tree(width: u32, reopen: bool) -> (usize, usize) {
    let feeds = FeedConfig::spv_mobile();
    let cold = 256;
    let cap = step_cap(&feeds, cold);
    let mut rig = SpvRig::new(32, feeds, cold, true);
    let siblings: Vec<Header> = (0..width).map(|i| rig.mine_on_anchor(10 + i, 1)[0]).collect();
    rig.extend(&siblings);
    let trunk = rig.mine_on_anchor(0, 100);
    rig.extend(&trunk);
    if reopen {
        rig.chain = rig.reopen().expect("open");
    }
    let lock = ChainLockRef {
        height: RIG_ANCHOR + 1,
        hash: siblings[width as usize - 1].hash(),
    };
    let reads = |rig: &SpvRig, kind| rig.store.faults(|f| f.reads(kind));
    let children = reads(&rig, ReadKind::Children);
    let (summary, locked) = observe(|marks| rig.step(&[], &[lock], marks), &mut |_| {});
    let summary = summary.expect("the lock's step");
    assert_eq!((summary.disconnected, summary.connected), (100, 1));
    assert!(reads(&rig, ReadKind::Children) - children > 1, "the children are read in pages");
    let m = (width - 1 + 100) as usize;
    let bound = cap + marks_term(m) + lists_term(m);
    assert!(locked <= bound, "lock: observed {locked} ≤ cap + M + L = {bound}");

    let quiet_bound = cap + marks_term(0) + lists_term(0);
    let quiet = |rig: &mut SpvRig, what: &str| {
        let leaves = reads(rig, ReadKind::Leaves);
        let (summary, observed) = observe(|marks| rig.step(&[], &[], marks), &mut |_| {});
        let summary = summary.expect("the quiet step");
        assert_eq!((summary.disconnected, summary.connected), (0, 0), "{what}");
        assert!(observed <= quiet_bound, "{what}: observed {observed} ≤ {quiet_bound}");
        (observed, reads(rig, ReadKind::Leaves) - leaves)
    };
    let (after, pages) = quiet(&mut rig, "quiet");
    assert!(pages > 1, "the leaves are read in pages");
    // Reopened at the locked sibling, the window holds every sibling: none is cold.
    rig.chain = rig.reopen().expect("open");
    quiet(&mut rig, "quiet after a reopen");
    (locked, after)
}

#[test]
fn peak_wide_tree() {
    let r = wide_tree(9_000, false);
    eprintln!("A-F12 wide tree, 9,000: (lock, quiet) observed = {r:?}");
}

#[test]
fn peak_wide_tree_reopened() {
    let r = wide_tree(9_000, true);
    eprintln!("A-F12 wide tree, 9,000, reopened: (lock, quiet) observed = {r:?}");
}

/// `peak_registry_republish` (Sol's R1-M2 witness): Q = E = W = C = 1, B = 5, `max_feeds` =
/// [`MAX_FEEDS`], w = 32; every reader live, one registry snapshot retained, and one header
/// that fills each queue. In the next step of 5, at each wait the writer finds 8 more readers
/// deregistered and every reader drained, so its retires republish the registry inside the
/// hold. Each registry is reserved at `max_feeds` entries, and the observed step is ≤ the cap,
/// whose registry term is sized for them.
#[test]
fn peak_registry_republish() {
    let feeds = FeedConfig {
        max_feeds: MAX_FEEDS,
        capacity: 1,
        segment_events: 1,
        write_batch: 1,
        step_headers: 5,
        ..FeedConfig::spv_mobile()
    };
    let n = MAX_FEEDS as usize;
    let mut rig = SpvRig::new(32, feeds, 1, false);
    let hub = rig.chain.feeds();
    let mut subs: Vec<_> = (0..n).map(|_| subscribe(&hub)).collect();
    let retained = hub.registry();
    let headers = rig.mine_on_anchor(0, 6);
    rig.step(&headers[..1], &[], &mut |_| {}).expect("the first header");
    let mut gone = 0;
    let (summary, observed) = observe(|marks| rig.step(&headers[1..], &[], marks), &mut |mark| {
        if mark == RigMark::Wait && gone < n {
            for (id, _) in &subs[gone..gone + 8] {
                hub.deregister(*id);
            }
            gone += 8;
            for (_, f) in &mut subs {
                drain(&mut **f);
            }
        }
    });
    assert_eq!(summary.expect("the step").connected, 5);
    assert!(gone >= 16, "the writer waited, and retired, more than once: {gone}");
    let registry = hub.registry();
    assert_eq!((retained.feeds.len(), registry.feeds.len()), (n, n - gone));
    assert_eq!(registry.feeds.capacity(), n, "reserved at max_feeds");
    let cap = step_cap(&feeds, 1);
    assert!(observed <= cap, "observed {observed} ≤ cap {cap}");
    eprintln!("A-F12 registry republish, {n} feeds: observed {observed} ≤ cap {cap}");
}

/// `budget_pending_takes_blocked`: a reader at Q = E = 256 that takes nothing until the writer
/// waits holds the pending segment full. A step of 1,000 connects (4 sub-steps) fills the
/// reader with the first, waits before planning the third and the fourth, and resumes each time
/// the reader drains; the fourth is fanned out at the release and stays pending until later
/// retries. No allocation passes the cap meanwhile.
#[test]
fn budget_pending_takes_blocked() {
    let feeds = FeedConfig::spv_mobile();
    let cap = step_cap(&feeds, 256);
    let mut rig = SpvRig::new(2_048, feeds, 256, false);
    let hs = rig.mine_on_anchor(0, 1_000);
    let mut f = subscribe(&rig.chain.feeds()).1;
    let (mut waits, mut got) = (0, 0);
    let (summary, observed) = observe(|marks| rig.step(&hs, &[], marks), &mut |m| {
        if m == RigMark::Wait {
            waits += 1;
            assert_eq!(drain(&mut *f), 256, "the reader was full");
            got += 256;
        }
    });
    let summary = summary.expect("the step");
    assert_eq!((summary.connected, summary.sub_steps), (1_000, 4));
    assert_eq!(waits, 2, "a wait before the third and the fourth");
    for _ in 0..8 {
        got += drain(&mut *f);
        rig.chain.retry_pending();
    }
    assert_eq!(got, 1_000, "every event reached the reader");
    assert!(observed <= cap, "observed {observed} ≤ {cap}");
}

/// `budget_feed_cap`: 8 subscribers at Q = 256 cost at most the per-feed cap each and the feed
/// cap together; a 9th `register` is `TableFull`. Over 2,000 one-header steps with controls of
/// random kinds sent between them and every reader draining after each step, the subscriber
/// side's allocations (the controls' payloads, sent and taken) net to exactly nothing.
#[test]
fn budget_feed_cap() {
    let feeds = FeedConfig::spv_mobile();
    let per = per_feed_cap(feeds.capacity);
    let mut rig = SpvRig::new(2_048, feeds, 256, false);
    let hub = rig.chain.feeds();
    let mut subs = Vec::new();
    let mut total = 0;
    for _ in 0..feeds.max_feeds {
        let ((id, f), bytes) = counted(|| subscribe(&hub));
        assert!(bytes as usize <= per, "one feed: {bytes} ≤ {per}");
        total += bytes as usize;
        subs.push((id, f));
    }
    assert!(total <= feeds.feed_cap(), "{total} ≤ {}", feeds.feed_cap());
    assert!(matches!(hub.register(Needs::default()), Err(Refused::TableFull)));

    let hs = rig.mine_on_anchor(0, 2_000);
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut grown = 0;
    for (i, h) in hs.iter().enumerate() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let (id, _) = &subs[x as usize % subs.len()];
        let kind = ControlKind::new(((x >> 8) % MAX_CONTROL_KINDS as u64) as u8);
        let ((), sent) = counted(|| {
            let _ = hub.send_control(*id, ControlMsg::new(kind, i as u64));
        });
        grown += sent;
        rig.step(std::slice::from_ref(h), &[], &mut |_| {})
            .unwrap_or_else(|e| panic!("step {i}: {e}"));
        for (id, f) in &mut subs {
            let (_, taken) = counted(|| drain(&mut **f));
            grown += taken;
            assert_eq!(hub.control_outstanding(*id), 0, "the reader took its control");
        }
    }
    assert_eq!(grown, 0, "every payload sent was freed when taken");
}

/// An arena chunk's entries (SB4's chunk adds a 160 B header and bitmap).
const CHUNK_BYTES: usize = 1_024 * ARENA_ENTRY_BYTES;

/// The distinct chunks of `ids`, which are in id order.
fn chunks_of(ids: &[BlockId]) -> usize {
    let mut held: Vec<_> = ids.iter().map(|id| id.0 / 1_024).collect();
    held.dedup();
    held.len()
}

/// The arena's chunks: those holding a resident entry (a chunk is released when it holds none,
/// SB0 §4.4). Returns `(chunks, N_win, S)`, where S is the chunks past the ⌈N_win / 1,024⌉ + 1
/// that N_win contiguous ids can span.
fn chunks(rig: &SpvRig) -> (usize, usize, usize) {
    let ids: Vec<_> = rig.chain.ids().collect();
    let held = chunks_of(&ids);
    (held, ids.len(), held.saturating_sub(ids.len().div_ceil(1_024) + 1))
}

/// One round of [`out_of_order_stale`]: 1,023 filler headers forking at the anchor, far below
/// the window and lighter than the active chain, so evicted at the step's stage 4; then `last`.
/// Counts what the round's steps keep, store aside.
fn filler_round(rig: &mut SpvRig, round: u32, last: Header) {
    let mut batch = rig.mine_on_anchor(11 + round, 1_023);
    batch.push(last);
    set_on(true);
    rig.step(&batch, &[], &mut gate).expect("a round");
    set_on(false);
}

/// `peak_out_of_order_chunks` (R2-m2), its stale variant at w = 2,048: after 12,000 active
/// headers, each of `rounds` steps accepts 1,023 evicted filler headers and one stale sibling
/// of the tip (an equal-work tie, so it stays stale and in the window), so each sibling pins a
/// chunk of its own past the active run's. A typical chain has S = 0, and chunks ≤ N_win always.
/// The memory the rounds keep is the pinned chunks', so the count is not just the arena's own
/// bookkeeping: no chunk outlives its last resident entry.
fn out_of_order_stale(rounds: u32) {
    let mut rig = SpvRig::new(2_048, FeedConfig::spv_mobile(), 256, false);
    let a = rig.mine_on_anchor(0, 12_000);
    rig.extend(&a);
    let (n0, n_win, s) = chunks(&rig);
    assert_eq!(s, 0, "typical: {n0} chunks for {n_win} entries");
    let t = dash_chain::test_util::RIG_T0 + 150 * (25 + 11_999) + 1;
    LIVE.with(|c| c.set((0, 0)));
    for r in 0..rounds {
        let sibling = rig.mine(a[11_998].hash(), t + r, 1)[0];
        filler_round(&mut rig, r, sibling);
    }
    let kept = LIVE.with(Cell::get).0;
    assert_eq!(rig.chain.private_tip().expect("a tip").0.hash, a[11_999].hash());
    let (n, n_win, s) = chunks(&rig);
    // The siblings are the highest resident ids; the active run is below them.
    let ids: Vec<_> = rig.chain.ids().collect();
    let active = chunks_of(&ids[..n_win - rounds as usize]);
    assert_eq!(n, active + rounds as usize, "each sibling pins its chunk: {n} for {n_win}");
    assert!(s + 1 >= rounds as usize && n <= n_win);
    let pinned = ((n - n0) * CHUNK_BYTES) as i64;
    assert!((kept - pinned).abs() < 128 * 1_024, "kept {kept} B for {} pinned chunks", n - n0);
    eprintln!("A-F12 out-of-order, stale: {n} chunks, N_win {n_win}, S {s}, kept {kept} B");
}

#[test]
fn peak_out_of_order_chunks() {
    out_of_order_stale(8);
}

/// The 100-chunk fixture: 24,576,000 B of pinned chunks at the measured entry size.
#[test]
#[ignore = "nightly: mines 114,400 regtest headers"]
fn peak_out_of_order_chunks_100() {
    out_of_order_stale(100);
}

/// `peak_out_of_order_chunks`, the sparse active-path variant at w = 16: each round's active
/// header follows 1,023 filler headers, so successive active ids land in different chunks and
/// the window's entries each pin one. A8 would not help: the pins are active.
#[test]
fn peak_out_of_order_chunks_sparse_active() {
    let rounds = 24;
    let mut rig = SpvRig::new(16, FeedConfig::spv_mobile(), 256, false);
    let a = rig.mine_on_anchor(0, 1_100 + rounds);
    rig.extend(&a[..1_100]);
    for r in 0..rounds {
        filler_round(&mut rig, r, a[1_100 + r as usize]);
    }
    let (n, n_win, s) = chunks(&rig);
    assert_eq!(n_win, 17, "the window's 17 active entries");
    assert_eq!(n, n_win, "each in a chunk of its own");
    assert_eq!(s, n_win - 2);
    eprintln!("A-F12 out-of-order, sparse active: {n} chunks, N_win {n_win}, S {s}");
}

/// The arena's fixed tables, which §6.3 leaves out: the first level (4,096 slots of 32 B) and
/// one second-level table of 1,024 chunk pointers (each covers 2^20 ids).
const ARENA_ROOT: usize = 4_096 * 32 + 1_024 * 8;

/// `mem_spv_typical_*`: SB4's A-31 extended (§6.3): a chain synced past its window with five
/// subscribers draining after each step. Everything the writer's thread allocates from the
/// rig's construction on (its pack's 25 store rows included), except the store's work during
/// steps, peaks within §6.3's typical `total` corrected three ways:
/// - its arena row at the measured entry size ([`ARENA_ENTRY_BYTES`] rather than 200 B), over
///   the row's ⌈(w + 1 + B) / 1,024⌉ + 1 chunks;
/// - plus [`ARENA_ROOT`];
/// - plus the decoded `headers` message during accept, which §6.2 leaves outside the step budget
///   (dash-p2p's) but which is on the writer's thread until accept consumes it.
fn mem_typical(w: u32, feeds: FeedConfig, cold: u32, headers: u32, total: usize) -> (i64, i64) {
    let b = feeds.step_headers as usize;
    let n_chunks = (w as usize + 1 + b).div_ceil(1_024) + 1;
    let message = b * (size_of::<Header>() + size_of::<BlockHash>());
    let bound = total + n_chunks * 1_024 * (ARENA_ENTRY_BYTES - 200) + ARENA_ROOT + message;
    start();
    let mut rig = SpvRig::new(w, feeds, cold, false);
    let hub = rig.chain.feeds();
    let mut subs: Vec<_> = (0..5).map(|_| subscribe(&hub).1).collect();
    set_on(false);
    let hs = rig.mine_on_anchor(0, headers);
    for chunk in hs.chunks(b) {
        set_on(true);
        let r = rig.step(chunk, &[], &mut |m| {
            gate(m);
            if m == RigMark::Wait {
                for f in &mut subs {
                    drain(&mut **f);
                }
            }
        });
        r.expect("a step");
        rig.settle(&mut subs);
        set_on(false);
    }
    let (live, peak) = LIVE.with(Cell::get);
    assert!(peak as usize <= bound, "peak {peak} ≤ {bound} (live after: {live})");
    (live, peak)
}

#[test]
fn mem_spv_typical_w10000() {
    let r = mem_typical(10_000, FeedConfig::spv(), 1_024, 14_000, 4_420_000);
    eprintln!("A-F12 typical, w = 10,000: (live after, peak) = {r:?}");
}

#[test]
fn mem_spv_typical_w2048() {
    let r = mem_typical(2_048, FeedConfig::spv_mobile(), 256, 6_000, 1_580_000);
    eprintln!("A-F12 typical, w = 2,048: (live after, peak) = {r:?}");
}

/// The path term, exactly (DEC-187 amendment 2 (a)): a push at the deepest heights copies one
/// leaf and an inner node per level, each in its `Arc`, out of a retained view. §6.2 charges two
/// such paths: the tip's, and the base's when a disconnect below the window re-extends it.
#[test]
fn exact_path_term() {
    let mut deep = DeepActive::new();
    let ((), bytes) = counted(|| deep.push());
    assert_eq!(bytes as usize, ACTIVE_PATHS_BYTES / 2);
    eprintln!("A-F12 path term: one path {bytes} B, the term {ACTIVE_PATHS_BYTES} B");
}

/// The fixed term, exactly (DEC-187 amendment 3): a release's publication with the old snapshot
/// and status facts held allocates the two new containers and nothing else, at its peak and at
/// its end.
#[test]
fn exact_fixed_term() {
    let mut rig = SpvRig::new(32, FeedConfig::spv_mobile(), 256, false);
    let headers = rig.mine_on_anchor(0, 2);
    rig.step(&headers, &[], &mut |_| {}).expect("a step");
    let mut live = (0, 0);
    rig.republish(|publish| {
        counted(publish);
        live = LIVE.with(Cell::get);
    });
    let fixed = STEP_FIXED_BYTES as i64;
    assert_eq!(live, (fixed, fixed), "(end, peak)");
    eprintln!("A-F12 fixed term: {fixed} B");
}

/// The registry term, exactly (DEC-187 amendment 2 (a)): with every feed of `max_feeds` live and
/// the registry retained, a deregistration publishes one registry beside it: its `Arc`, the
/// `Registry` and `max_feeds` reserved entries. §6.2 charges two, the one built and the one
/// published, which the step's retires can each replace.
#[test]
fn exact_registry_term() {
    let feeds = FeedConfig {
        max_feeds: MAX_FEEDS,
        ..FeedConfig::spv_mobile()
    };
    let rig = SpvRig::new(32, feeds, 256, false);
    let hub = rig.chain.feeds();
    let subs: Vec<_> = (0..MAX_FEEDS).map(|_| subscribe(&hub)).collect();
    let retained = hub.registry();
    let ((), bytes) = counted(|| hub.deregister(subs[0].0));
    assert_eq!(retained.feeds.len(), MAX_FEEDS as usize);
    assert_eq!(bytes as usize, registry_bytes(MAX_FEEDS) / 2);
    eprintln!(
        "A-F12 registry term: one registry {bytes} B, the term {} B",
        registry_bytes(MAX_FEEDS)
    );
}

/// §6.3's index table, exactly: hashbrown's table for a capacity is [`index_table_bytes`] at its
/// bucket count (the smallest power of two, at least 4, whose 7/8, or b − 1 below 8, holds it).
#[test]
fn exact_index_table() {
    for capacity in [3, 7, 100, 3_584, 7_168, 14_336] {
        let buckets = match capacity {
            0..4 => 4,
            4..8 => 8,
            _ => (capacity * 8 / 7_usize).next_power_of_two(),
        };
        let (table, bytes) = counted(|| hashbrown::HashTable::<BlockId>::with_capacity(capacity));
        assert_eq!(bytes as usize, index_table_bytes(buckets), "capacity {capacity}");
        drop(table);
    }
    // §6.3's typical windows: N_win + B = 4,049 on mobile, 12,001 on desktop.
    assert_eq!(index_bytes(4_049), index_table_bytes(16_384) + index_table_bytes(8_192));
    assert_eq!(index_bytes(12_001), index_table_bytes(32_768) + index_table_bytes(16_384));
}

/// `peak_full_step` (DEC-187 amendment 2 (d)): a plain step of B headers on a chain primed past
/// its window (N_win = w + 1, S = 0), its batch built before the measured call and left out of
/// both accounts. The step account is ≤ the cap + M(0) + L(0). The thread's residency blocks
/// peak within §6.3's ⌈(N_win + B) / 1,024⌉ + 1 chunks, the arena's second-level table, the
/// index term with its rehash transient, the resident set's term, the chunk counts' table, and
/// the active chain twice (the chain's, and the published view's originals until the release).
/// The only bytes in neither account are the batch's two buffers.
fn full_step(w: u32, feeds: FeedConfig, cold: u32) -> Accounts {
    let b = feeds.step_headers;
    let mut rig = SpvRig::new(w, feeds, cold, false);
    let headers = rig.mine_on_anchor(0, w + 2 * b);
    let (primed, last) = headers.split_at((w + b) as usize);
    rig.extend(primed);
    let batch = SpvRig::batch(last);
    let batch_bytes = batch.headers.capacity() * size_of::<Header>()
        + batch.hashes.capacity() * size_of::<BlockHash>();
    exclude(&batch);
    let (summary, a) =
        observe_accounts(|marks| rig.step_batch(Some(batch), &[], marks), &mut |_| {});
    assert_eq!(summary.expect("the step").connected, b);
    assert_eq!(excluded_bytes(), Some(batch_bytes), "accept freed the batch, and only it");
    let n = (w + 1 + b) as usize;
    let chunks = n.div_ceil(1_024) + 1;
    let residency = chunks * ARENA_CHUNK_BYTES
        + ARENA_TABLE_BYTES
        + index_bytes(n)
        + resident_bytes(n)
        + grown_table_bytes(chunks, 2 * size_of::<u32>())
        + 2 * active_bytes(n);
    let cap = step_cap(&feeds, cold) + marks_term(0) + lists_term(0);
    eprintln!(
        "A-F12 full step, w = {w}, B = {b}: step {} ≤ {cap}; residency {} (held peak {} ≤ \
         {residency}: {chunks} chunks, table, index {}, resident set {}); both {}; batch {} \
         outside",
        a.step,
        a.residency,
        a.held_peak,
        index_bytes(n),
        resident_bytes(n),
        a.both,
        batch_bytes,
    );
    assert!(a.step <= cap, "step {} ≤ {cap}", a.step);
    assert!(a.held_peak <= residency, "residency {} ≤ {residency}", a.held_peak);
    a
}

#[test]
fn peak_full_step_mobile() {
    full_step(2_048, FeedConfig::spv_mobile(), 256);
}

#[test]
fn peak_full_step_desktop() {
    full_step(10_000, FeedConfig::spv(), 1_024);
}

#[test]
fn peak_full_step_mobile_b500() {
    let feeds = FeedConfig {
        step_headers: 500,
        ..FeedConfig::spv_mobile()
    };
    full_step(2_048, feeds, 256);
}

/// §6.3's resident-set nodes, exactly: std's B-tree over `(height, id)` allocates one leaf for
/// its first 11 keys, and the 12th splits it into two leaves under an internal root.
#[test]
fn exact_resident_nodes() {
    let mut set = std::collections::BTreeSet::new();
    let ((), first) = counted(|| {
        set.insert((0_u32, BlockId(0)));
    });
    assert_eq!(first as usize, RESIDENT_LEAF_BYTES);
    for k in 1..11 {
        set.insert((k, BlockId(k)));
    }
    let ((), split) = counted(|| {
        set.insert((11, BlockId(11)));
    });
    assert_eq!(split as usize, RESIDENT_LEAF_BYTES + RESIDENT_INTERNAL_BYTES);
}
