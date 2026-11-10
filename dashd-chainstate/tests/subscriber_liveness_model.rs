//! The subscriber liveness model of CH2 §4.4.3 ("rev3"), as an explicit-state model checker.
//!
//! This is an ABSTRACTION of the threaded design of §4.4.1, not a proof of it, and not a test of
//! the implementation. It models, over a two-branch block tree (a main chain and one fork at
//! height 2):
//!
//! - **V**, the validation thread: one tip change per step (a connect, or one disconnect of a
//!   scripted reorg of depth 0-2). Each change gets its sequence number at once and its event is
//!   pending until every Synced queue has room (backpressure; one event at a time, so the queue
//!   bound Q is never exceeded). V may freeze while an event is pending, and freezes only when
//!   mandated (`FlushDue`, a full active window, the end) and only if something changed. A pending
//!   `FlushDue` always gets a flush, an empty one if nothing changed (§4.4.1 rule 5).
//! - **W**, the flush writer: the rewind gate, B1 (`T_ack := new`, captures the Synced
//!   subscribers, `FlushCommit` with `tip_seq`), the wait for acks, Bk, release.
//! - **Subscribers**: Synced ones on one shared events thread or one thread each, Syncing ones on
//!   their own thread. Rules 1-6 of §4.4.1: case A/B by sequence number, checkpoints only on the
//!   leg toward `new`, off-path drop, compression before parking, parking (never blocking), wake
//!   on `FlushCommit` / `RewindRequest` / any enqueue, the rewind commit, the v24 eager capped
//!   commit at `LCA(pprev, cap)` on a disconnect, and the capped cadence.
//! - **The Syncing to Synced handoff** (rev3, G-R3-M4, F10) in two steps: `Y.ready` (catch-up
//!   reached the tip it saw) and `Y.subscribe`, atomic with V's tip state. It re-checks that p is
//!   V's tip (else back to Syncing), commits at `LCA(p, cap)` and installs the cut `seq_synced`,
//!   the next sequence number V will assign. An event pending at the handoff is already in p and
//!   is not delivered.
//!
//! **Checked in every reachable state**: R11a (no durable L strictly ahead of G = `T_ack`), the
//! data bound `p <= h(L) + D_max`, L on p's path, every Synced data queue <= Q, and F10's
//! exclusion (a Synced subscriber never ignores a `Connected(b)` whose parent is a strict
//! ancestor of p, nor a `Disconnected(b)` with b a strict ancestor of p). On top of those,
//! **AG EF done**: every reachable state can still reach completion. A state with no moves that
//! is not done is a deadlock. This finds deadlocks and fixed points. It does not rule out an
//! infinite run that ignores an enabled step forever (§4.4.2 assumes weak fairness).
//!
//! **Not modelled**: block contents, rows and state, I/O and its failures, pruning, shutdown,
//! real thread scheduling, memory and journal bytes, independent fanout (V emits to all Synced
//! queues at once, F3).
//!
//! Ported line by line from `/work/scratch/ch2-rev3/liveness_model_v3.py` (rev3), SHA-256
//! `49d5440f3a31af26857612ff2248376cce93d94b4f3ccd24f993edd70f308015`; the expected state counts
//! below are that model's. State sets are small bitsets where Python used frozensets (processed
//! blocks, skipped sequence numbers, consumed commits); the semantics are the same. Hashing is
//! deterministic (no `RandomState`), so a run is reproducible.
//!
//! The planted regressions are the ones of §4.4.3's table (X17-X28). Each is a switch on the
//! model's rule that disables one of the design's rules, and the checker must then fail.
//!
//! - `grid_pr_subset` runs 20 of the 120 grid configurations on every `cargo test`.
//! - `grid_full` (`--ignored`, nightly) runs all 120: 19,367,358 states. `LIVENESS_SHARD` and
//!   `LIVENESS_NSHARDS` select `GRID[shard::nshards]` as the Python's `--shard` does;
//!   `LIVENESS_THREADS` sets the number of worker threads (default 4).

use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

// ---------------------------------------------------------------------------------------------
// A deterministic hasher (FxHash), so the state sets do not depend on `RandomState`.
// ---------------------------------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct Fx(u64);

impl Fx {
    #[inline]
    fn add(&mut self, w: u64) {
        self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl Hasher for Fx {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut b = [0u8; 8];
            b[..chunk.len()].copy_from_slice(chunk);
            self.add(u64::from_le_bytes(b));
        }
    }
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
    fn write_u128(&mut self, i: u128) {
        self.add(i as u64);
        self.add((i >> 64) as u64);
    }
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
    fn finish(&self) -> u64 {
        self.0.rotate_left(26)
    }
}

type Det = BuildHasherDefault<Fx>;

// ---------------------------------------------------------------------------------------------
// The block tree: heights 0.., branch 0 the main chain, branch 1 the fork above height FORK.
// ---------------------------------------------------------------------------------------------

const FORK: u8 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Block {
    h: u8,
    br: u8,
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.h, self.br)
    }
}

const GEN: Block = Block {
    h: 0,
    br: 0,
};

const fn blk(h: u8, br: u8) -> Block {
    Block {
        h,
        br,
    }
}

fn parent(b: Block) -> Option<Block> {
    if b.h == 0 {
        return None;
    }
    if b.br == 1 && b.h - 1 > FORK {
        return Some(blk(b.h - 1, 1));
    }
    Some(blk(b.h - 1, 0))
}

/// Whether `a` is an ancestor of `b`, or `b` itself.
fn is_ancestor(a: Block, mut b: Block) -> bool {
    loop {
        if b.h < a.h {
            return false;
        }
        if b == a {
            return true;
        }
        match parent(b) {
            Some(p) => b = p,
            None => return false,
        }
    }
}

fn lca(mut a: Block, mut b: Block) -> Block {
    while a.h > b.h {
        a = parent(a).expect("a above genesis");
    }
    while b.h > a.h {
        b = parent(b).expect("b above genesis");
    }
    while a != b {
        a = parent(a).expect("a above genesis");
        b = parent(b).expect("b above genesis");
    }
    a
}

fn strictly_above(new: Block, l: Block) -> bool {
    is_ancestor(new, l) && l != new
}

// ---------------------------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------------------------

/// An event of a subscriber's queue, or V's pending event: `Connected(b, seq)`,
/// `Disconnected(b, seq)`, or a queued copy of a `FlushCommit { new, tip_seq }`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Ev {
    C(Block, i32),
    D(Block, i32),
    F {
        fid: u32,
        new: Block,
        tseq: i32,
    },
}

/// A subscriber's outstanding `FlushCommit`, read from the hub: `(fid, new, tip_seq)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Out {
    fid: u32,
    new: Block,
    tseq: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Cls {
    /// Synced.
    S,
    /// Syncing.
    Y,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Sub {
    cls: Cls,
    p: Block,
    l: Block,
    queue: Vec<Ev>,
    out: Option<Out>,
    parked: bool,
    blocked: bool,
    /// The set of processed blocks, as a bitset over `h * 2 + br`.
    proc: u64,
    rreq: Option<Block>,
    last_seq: i32,
    seq_synced: i32,
    /// The sequence numbers compression removed ahead of `last_seq`, as a bitset.
    skipped: u64,
    ready: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Stage {
    Pre,
    Rgate,
    Acks,
    Done,
}

/// The flush in flight: `(fid, new, stage, captured, acked, tip_seq)`. `cap` and `acked` are
/// bitsets over the subscriber index.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Flush {
    fid: u32,
    new: Block,
    stage: Stage,
    cap: u8,
    acked: u8,
    tseq: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Reorg {
    No,
    /// The reorg's disconnects are in progress.
    Mid,
    Yes,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct St {
    vtip: Block,
    tip_seq: i32,
    nseq: i32,
    pend: Option<Ev>,
    reorged: Reorg,
    since: u32,
    lastf: Block,
    f: Option<Flush>,
    tack: Block,
    due: bool,
    nfid: u32,
    /// The set of `(subscriber, fid)` whose commit was acked, as a bitset over `i * 64 + fid`.
    consumed: u128,
    subs: Vec<Sub>,
    tblk: Option<u8>,
}

fn proc_bit(b: Block) -> u64 {
    1u64 << (u32::from(b.h) * 2 + u32::from(b.br))
}

/// The bit of a sequence number in a `skipped` set; none for the "no event yet" value -1.
fn seq_bit(seq: i32) -> u64 {
    if seq < 0 {
        0
    } else {
        1u64 << seq
    }
}

/// The bits of `skipped` strictly above `seq`.
fn above(seq: i32) -> u64 {
    !((2u64 << seq) - 1)
}

fn consumed_bit(i: usize, fid: u32) -> u128 {
    assert!(fid < 64, "flush id {fid} outside the consumed bitset");
    1u128 << (i as u32 * 64 + fid)
}

fn ev_seq(e: Ev) -> i32 {
    match e {
        Ev::C(_, s) | Ev::D(_, s) => s,
        Ev::F {
            tseq,
            ..
        } => tseq,
    }
}

fn ev_block(e: Ev) -> Block {
    match e {
        Ev::C(b, _) | Ev::D(b, _) => b,
        Ev::F {
            new,
            ..
        } => new,
    }
}

/// The data events in a queue; `FlushCommit` copies do not count against Q.
fn nqueue(s: &Sub) -> usize {
    s.queue.iter().filter(|e| !matches!(e, Ev::F { .. })).count()
}

// ---------------------------------------------------------------------------------------------
// Configuration and plants.
// ---------------------------------------------------------------------------------------------

/// The planted regressions (switches that disable a rule of §4.4.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Plant {
    /// X17 (python `block-at-limit`): the events thread blocks at the limit (Opus F9).
    BlockAtLimit,
    /// X18 (`no-caseA`): no case-A ack (GPT B1).
    NoCaseA,
    /// X19 (`no-rgate`): a backward B1 without the rewind gate.
    NoRgate,
    /// X20 (`no-compress`), with X24: park without compression (and without the empty flush).
    NoCompress,
    /// X22 (`queue-caseA`): case A by queue membership (Opus R2-M6).
    QueueCaseA,
    /// X23 (`no-wake`): no wake on enqueue (GPT r2 note).
    NoWake,
    /// X24 (`no-empty-flush`): no empty flush on `FlushDue`.
    NoEmptyFlush,
    /// X27 (`nonatomic-handoff`): the catch-up handoff without the live-tip re-check (G-R3-M4).
    NonatomicHandoff,
    /// X28 (`include-pending`): the pre-cut pending event is delivered at the handoff (rev2).
    IncludePending,
    /// `no-eager`: no capped eager commit on a disconnect. The Python defines it, no table row.
    NoEager,
    /// `no-drop`: no off-path drop of a connect. The Python defines it, no table row.
    NoDrop,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DoneMode {
    /// Completion needs a final flush (`L == tip`).
    Strong,
    /// All work drained, nobody parked or waiting, no `FlushDue` pending: a locator may lag the
    /// tip until the next flush, as Core's does.
    Quiescent,
}

#[derive(Clone)]
struct Cfg {
    d: u8,
    q: usize,
    act: u32,
    maxh: u8,
    reorg: u8,
    shared: bool,
    plant: Vec<Plant>,
    /// One char per subscriber: `S` Synced, `Y` Syncing.
    init: &'static str,
    /// The v24 profile: a capped commit on a disconnect (D13 ruling).
    eager: bool,
    /// The reorg is an `invalidateblock` with no alternative chain: V stops at the fork.
    inval: bool,
    done: DoneMode,
}

impl Default for Cfg {
    fn default() -> Cfg {
        Cfg {
            d: 2,
            q: 1,
            act: 2,
            maxh: 5,
            reorg: 0,
            shared: true,
            plant: Vec::new(),
            init: "SS",
            eager: true,
            inval: false,
            done: DoneMode::Strong,
        }
    }
}

impl Cfg {
    fn has(&self, p: Plant) -> bool {
        self.plant.contains(&p)
    }
}

// ---------------------------------------------------------------------------------------------
// Labels, for witnesses and the handoff counters.
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Init,
    VEmit,
    VDisc,
    VReorgDone,
    VConn,
    VReorgStart,
    VFreeze,
    VFreezePending,
    VRelease,
    WRgate,
    WB1,
    WBk,
    CommitA,
    AckL,
    SkipFc,
    CommitQ,
    IgnD,
    Disc,
    DiscRewind,
    DiscEager,
    DropC,
    IgnC,
    Ckpt,
    Compress,
    Park,
    BlockThread,
    Unblock,
    ConnS,
    YRgate,
    YRewind,
    /// The re-check at the handoff failed: V's tip moved between catch-up and subscribe.
    YUnready,
    YSynced,
    /// A handoff taken with an event pending emission.
    YSyncedPending,
    YReady,
    YCadence,
    YDue,
    YStep,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Lbl {
    kind: Kind,
    sub: u8,
    at: Block,
}

impl fmt::Display for Lbl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}[{}]@{}", self.kind, self.sub, self.at)
    }
}

fn lbl(kind: Kind, sub: usize, at: Block) -> Lbl {
    Lbl {
        kind,
        sub: sub as u8,
        at,
    }
}

type Move = (Lbl, St);
type Res<T> = Result<T, String>;

// ---------------------------------------------------------------------------------------------
// Commits and the invariants' checks.
// ---------------------------------------------------------------------------------------------

fn check_commit(x: Block, l: Block, tack: Block, why: &'static str) -> Res<()> {
    if strictly_above(tack, x) {
        return Err(format!("R11a: {why} locator {x} ahead of G {tack}"));
    }
    if why != "rewind" && !is_ancestor(l, x) {
        return Err(format!("{why}: locator {x} not a descendant of L {l}"));
    }
    Ok(())
}

fn sub_commit(s: &Sub, x: Block, tack: Block, why: &'static str) -> Res<Sub> {
    check_commit(x, s.l, tack, why)?;
    let mut n = s.clone();
    n.l = x;
    Ok(n)
}

/// The commit cap: the rewind gate's `new` while it is closed, else `T_ack`.
fn cap_of(st: &St) -> Block {
    match st.f {
        Some(f) if f.stage == Stage::Rgate => f.new,
        _ => st.tack,
    }
}

fn put(st: &St, i: usize, s: Sub) -> St {
    let mut n = st.clone();
    n.subs[i] = s;
    n
}

// ---------------------------------------------------------------------------------------------
// V.
// ---------------------------------------------------------------------------------------------

fn final_tip(cfg: &Cfg) -> Block {
    if cfg.inval {
        return blk(FORK, 0);
    }
    blk(cfg.maxh, u8::from(cfg.reorg > 0))
}

fn v_moves(st: &St, cfg: &Cfg) -> Vec<Move> {
    let mut out = Vec::new();
    let synced: Vec<usize> = (0..st.subs.len())
        .filter(|&i| {
            let s = &st.subs[i];
            s.cls == Cls::S && st.pend.is_none_or(|p| s.seq_synced <= ev_seq(p))
        })
        .collect();
    let room = synced.iter().all(|&i| nqueue(&st.subs[i]) < cfg.q);
    match st.pend {
        Some(pend) => {
            if room {
                // Emit the pending event; an enqueue wakes a parked subscriber (rule 3).
                let mut n = st.clone();
                for &i in &synced {
                    let s = &mut n.subs[i];
                    s.queue.push(pend);
                    s.parked = s.parked && cfg.has(Plant::NoWake);
                }
                n.pend = None;
                out.push((lbl(Kind::VEmit, 0, ev_block(pend)), n));
            }
        }
        None => {
            if st.reorged == Reorg::Mid {
                if st.vtip.h > FORK {
                    let b = st.vtip;
                    let mut n = st.clone();
                    n.vtip = parent(b).expect("above genesis");
                    n.tip_seq = st.nseq;
                    n.nseq = st.nseq + 1;
                    n.pend = Some(Ev::D(b, st.nseq));
                    n.since = st.since + 1;
                    out.push((lbl(Kind::VDisc, 0, b), n));
                } else {
                    let mut n = st.clone();
                    n.reorged = Reorg::Yes;
                    out.push((lbl(Kind::VReorgDone, 0, st.vtip), n));
                }
            } else {
                let br = u8::from(st.reorged == Reorg::Yes);
                let mut nxt = Some(blk(st.vtip.h + 1, br));
                if cfg.reorg > 0 && st.reorged == Reorg::No && st.vtip.h + 1 > FORK + cfg.reorg {
                    nxt = None;
                }
                if cfg.inval && st.reorged == Reorg::Yes {
                    nxt = None;
                }
                if let Some(nx) = nxt
                    && nx.h <= cfg.maxh
                    && st.since < cfg.act
                {
                    let mut n = st.clone();
                    n.vtip = nx;
                    n.tip_seq = st.nseq;
                    n.nseq = st.nseq + 1;
                    n.pend = Some(Ev::C(nx, st.nseq));
                    n.since = st.since + 1;
                    out.push((lbl(Kind::VConn, 0, nx), n));
                }
                if cfg.reorg > 0 && st.reorged == Reorg::No && st.vtip == blk(FORK + cfg.reorg, 0) {
                    let mut n = st.clone();
                    n.reorged = Reorg::Mid;
                    out.push((lbl(Kind::VReorgStart, 0, st.vtip), n));
                }
            }
        }
    }
    let at_end = st.vtip == final_tip(cfg)
        && (st.reorged == Reorg::Yes || cfg.reorg == 0)
        && st.pend.is_none();
    let changed = (st.vtip != st.lastf || st.since >= cfg.act) && st.since > 0;
    // Rule 5: a pending FlushDue always gets a flush, an EMPTY one if nothing changed (it still
    // issues FlushCommit).
    let empty_ok = st.due && !cfg.has(Plant::NoEmptyFlush);
    if st.f.is_none() && (changed || empty_ok) && (st.due || st.since >= cfg.act || at_end) {
        let mut n = st.clone();
        n.f = Some(Flush {
            fid: st.nfid,
            new: st.vtip,
            stage: Stage::Pre,
            cap: 0,
            acked: 0,
            tseq: st.tip_seq,
        });
        n.nfid = st.nfid + 1;
        n.since = 0;
        n.lastf = st.vtip;
        n.due = false;
        let kind = if st.pend.is_some() {
            Kind::VFreezePending
        } else {
            Kind::VFreeze
        };
        out.push((lbl(kind, 0, st.vtip), n));
    }
    if let Some(f) = st.f
        && f.stage == Stage::Done
    {
        let mut n = st.clone();
        n.f = None;
        out.push((lbl(Kind::VRelease, 0, f.new), n));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// W.
// ---------------------------------------------------------------------------------------------

/// The subscribers whose durable L strictly descends from `new`.
fn gate_targets(st: &St, new: Block) -> Vec<usize> {
    (0..st.subs.len()).filter(|&i| strictly_above(new, st.subs[i].l)).collect()
}

fn w_moves(st: &St, cfg: &Cfg) -> Vec<Move> {
    let Some(f) = st.f else {
        return Vec::new();
    };
    let Flush {
        fid,
        new,
        stage,
        cap,
        acked,
        tseq,
    } = f;
    if stage == Stage::Pre && !cfg.has(Plant::NoRgate) {
        let tg = gate_targets(st, new);
        if !tg.is_empty() {
            let mut n = st.clone();
            for &i in &tg {
                n.subs[i].rreq = Some(new);
                n.subs[i].parked = false;
            }
            n.f = Some(Flush {
                fid,
                new,
                stage: Stage::Rgate,
                cap: 0,
                acked: 0,
                tseq,
            });
            return vec![(lbl(Kind::WRgate, 0, new), n)];
        }
    }
    if stage == Stage::Pre || stage == Stage::Rgate {
        if stage == Stage::Rgate && !gate_targets(st, new).is_empty() {
            return Vec::new();
        }
        let mut n = st.clone();
        let mut captured = 0u8;
        for (i, s) in n.subs.iter().enumerate() {
            if s.cls == Cls::S {
                captured |= 1 << i;
            }
        }
        for s in &mut n.subs {
            s.rreq = None;
        }
        for (i, s) in n.subs.iter_mut().enumerate() {
            if captured & (1 << i) != 0 {
                s.queue.push(Ev::F {
                    fid,
                    new,
                    tseq,
                });
                s.out = Some(Out {
                    fid,
                    new,
                    tseq,
                });
                s.parked = false;
            }
        }
        n.tack = new;
        n.f = Some(Flush {
            fid,
            new,
            stage: if captured != 0 {
                Stage::Acks
            } else {
                Stage::Done
            },
            cap: captured,
            acked: 0,
            tseq,
        });
        return vec![(lbl(Kind::WB1, 0, new), n)];
    }
    if stage == Stage::Acks && acked & cap == cap {
        let mut n = st.clone();
        n.f = Some(Flush {
            stage: Stage::Done,
            ..f
        });
        return vec![(lbl(Kind::WBk, 0, new), n)];
    }
    Vec::new()
}

/// The subscriber `i` acks the commit `fid`.
fn ack(mut st: St, i: usize, fid: u32) -> St {
    let f = st.f.as_mut().expect("an ack with no flush in flight");
    f.acked |= 1 << i;
    st.consumed |= consumed_bit(i, fid);
    st
}

// ---------------------------------------------------------------------------------------------
// Subscribers.
// ---------------------------------------------------------------------------------------------

/// Case B test. rev2: by sequence number. X22 plants rev1's queue-membership test.
fn tip_event_ahead(s: &Sub, tseq: i32, cfg: &Cfg) -> bool {
    if cfg.has(Plant::QueueCaseA) {
        return s.queue.iter().any(|e| matches!(e, Ev::C(_, q) if *q == tseq));
    }
    tseq >= s.seq_synced && tseq > s.last_seq && s.skipped & seq_bit(tseq) == 0
}

/// The subscriber with its head event removed.
fn pop_event(s: &Sub, e: Ev) -> Sub {
    let mut n = s.clone();
    n.queue.remove(0);
    if let Ev::C(_, q) | Ev::D(_, q) = e {
        n.last_seq = q;
        n.skipped = s.skipped & above(q);
    }
    n
}

fn synced_step(st: &St, i: usize, cfg: &Cfg) -> Res<Option<Move>> {
    let s = &st.subs[i];
    let d = cfg.d;
    if s.parked {
        return Ok(None);
    }
    let out = s.out.filter(|o| st.consumed & consumed_bit(i, o.fid) == 0);
    let has_d = s.queue.iter().any(|e| matches!(e, Ev::D(..)));
    let mut target: Option<Block> = None;
    if let Some(o) = out
        && !cfg.has(Plant::NoCaseA)
    {
        if tip_event_ahead(s, o.tseq, cfg) {
            target = Some(o.new);
        } else {
            let x = lca(s.p, o.new);
            if is_ancestor(s.l, x) {
                let mut s2 = if x != s.l {
                    sub_commit(s, x, st.tack, "commit-step")?
                } else {
                    s.clone()
                };
                s2.out = None;
                return Ok(Some((lbl(Kind::CommitA, i, x), ack(put(st, i, s2), i, o.fid))));
            }
            if !has_d {
                let mut s2 = s.clone();
                s2.out = None;
                return Ok(Some((lbl(Kind::AckL, i, s.l), ack(put(st, i, s2), i, o.fid))));
            }
        }
    }
    if target.is_none()
        && let Some(r) = s.rreq
        && strictly_above(r, s.l)
    {
        target = Some(r);
    }
    let Some(&e) = s.queue.first() else {
        return Ok(None);
    };
    if let Ev::F {
        fid,
        new,
        ..
    } = e
    {
        // rev2: the queued copy is always a no-op (the commit is serviced out of band from the
        // hub).
        if st.consumed & consumed_bit(i, fid) != 0 || !cfg.has(Plant::NoCaseA) {
            return Ok(Some((lbl(Kind::SkipFc, i, new), put(st, i, pop_event(s, e)))));
        }
        let x = lca(s.p, new);
        let mut s2 = sub_commit(&pop_event(s, e), x, st.tack, "commit-step")?;
        s2.out = None;
        return Ok(Some((lbl(Kind::CommitQ, i, x), ack(put(st, i, s2), i, fid))));
    }
    let pop = pop_event(s, e);
    if let Ev::D(b, _) = e {
        if b != s.p {
            if is_ancestor(b, s.p) {
                return Err(format!(
                    "sub {i}: ignored Disconnected({b}), a strict ancestor of p={} (pre-cut or lost event)",
                    s.p
                ));
            }
            return Ok(Some((lbl(Kind::IgnD, i, b), put(st, i, pop))));
        }
        let np = parent(b).expect("a disconnected block has a parent");
        let mut s2 = pop;
        s2.p = np;
        let mut kind = Kind::Disc;
        let mut at = b;
        if np.h < s.l.h || !is_ancestor(s.l, np) {
            check_commit(np, np, st.tack, "rewind")?;
            s2.l = np;
            kind = Kind::DiscRewind;
        } else if cfg.eager && !cfg.has(Plant::NoEager) {
            // D13 ruling: capped commit at LCA(pprev, T_ack).
            let x = lca(np, cap_of(st));
            if is_ancestor(s2.l, x) && x != s2.l {
                s2 = sub_commit(&s2, x, cap_of(st), "eager")?;
                kind = Kind::DiscEager;
                at = x;
            }
        }
        return Ok(Some((lbl(kind, i, at), put(st, i, s2))));
    }
    let b = ev_block(e);
    if let Some(t) = target
        && !cfg.has(Plant::NoDrop)
        && !is_ancestor(b, t)
    {
        return Ok(Some((lbl(Kind::DropC, i, b), put(st, i, pop))));
    }
    let pb = parent(b).expect("a connected block has a parent");
    if pb != s.p {
        if is_ancestor(pb, s.p) {
            return Err(format!(
                "sub {i}: ignored Connected({b}) whose parent is a strict ancestor of p={} (pre-cut or lost event)",
                s.p
            ));
        }
        return Ok(Some((lbl(Kind::IgnC, i, b), put(st, i, pop))));
    }
    if b.h > s.l.h + d {
        if let (Some(_), Some(t)) = (out, target) {
            if !is_ancestor(s.p, t) {
                return Err("checkpoint off the leg".to_string());
            }
            let x = s.p;
            if x == s.l {
                return Ok(None);
            }
            return Ok(Some((
                lbl(Kind::Ckpt, i, x),
                put(st, i, sub_commit(s, x, st.tack, "checkpoint")?),
            )));
        }
        let later = (1..s.queue.len()).find(|&k| matches!(s.queue[k], Ev::D(bb, _) if bb == b));
        if let Some(k) = later
            && !cfg.has(Plant::NoCompress)
        {
            let mut s2 = s.clone();
            s2.queue.remove(k);
            s2.queue.remove(0);
            s2.last_seq = ev_seq(e);
            s2.skipped = s.skipped | seq_bit(ev_seq(s.queue[k]));
            return Ok(Some((lbl(Kind::Compress, i, b), put(st, i, s2))));
        }
        if !cfg.has(Plant::BlockAtLimit) {
            let mut n = put(st, i, s.clone());
            n.subs[i].parked = true;
            n.due = true;
            return Ok(Some((lbl(Kind::Park, i, b), n)));
        }
        if s.blocked {
            return Ok(None);
        }
        let mut n = put(st, i, s.clone());
        n.subs[i].blocked = true;
        n.due = true;
        n.tblk = if cfg.shared {
            Some(i as u8)
        } else {
            st.tblk
        };
        return Ok(Some((lbl(Kind::BlockThread, i, b), n)));
    }
    let mut s2 = pop;
    s2.p = b;
    s2.proc |= proc_bit(b);
    Ok(Some((lbl(Kind::ConnS, i, b), put(st, i, s2))))
}

fn syncing_step(st: &St, i: usize, cfg: &Cfg) -> Res<Option<Move>> {
    let s = &st.subs[i];
    if let Some(r) = s.rreq
        && strictly_above(r, s.l)
    {
        let f = lca(s.p, r);
        check_commit(f, f, st.tack, "rewind")?;
        let mut s2 = s.clone();
        s2.p = f;
        s2.l = f;
        return Ok(Some((lbl(Kind::YRgate, i, f), put(st, i, s2))));
    }
    if !is_ancestor(s.p, st.vtip) {
        let f = lca(s.p, st.vtip);
        let mut s2 = s.clone();
        s2.p = f;
        if f.h < s.l.h || !is_ancestor(s.l, f) {
            check_commit(f, f, st.tack, "rewind")?;
            s2.l = f;
        }
        return Ok(Some((lbl(Kind::YRewind, i, f), put(st, i, s2))));
    }
    if s.ready {
        // rev3 (G-R3-M4): subscribe, atomically with V's tip state.
        if s.p != st.vtip && !cfg.has(Plant::NonatomicHandoff) {
            let mut s2 = s.clone();
            s2.ready = false;
            return Ok(Some((lbl(Kind::YUnready, i, s.p), put(st, i, s2))));
        }
        let x = lca(s.p, cap_of(st));
        let mut s2 = s.clone();
        if is_ancestor(s.l, x) {
            s2 = sub_commit(s, x, cap_of(st), "cadence")?;
        }
        let first = match st.pend {
            // rev2: the pending event is delivered too.
            Some(p) if cfg.has(Plant::IncludePending) => ev_seq(p),
            // rev3: the cut is the next seq V assigns; the pending change is already in p.
            _ => st.nseq,
        };
        s2.cls = Cls::S;
        s2.queue = Vec::new();
        s2.out = None;
        s2.last_seq = first - 1;
        s2.seq_synced = first;
        s2.skipped = 0;
        s2.ready = false;
        let kind = if st.pend.is_some() {
            Kind::YSyncedPending
        } else {
            Kind::YSynced
        };
        return Ok(Some((lbl(kind, i, x), put(st, i, s2))));
    }
    if s.p == st.vtip {
        let mut s2 = s.clone();
        s2.ready = true;
        return Ok(Some((lbl(Kind::YReady, i, s.p), put(st, i, s2))));
    }
    let h = s.p.h + 1;
    let mut nxt = st.vtip;
    while nxt.h > h {
        nxt = parent(nxt).expect("above genesis");
    }
    if h > s.l.h + cfg.d {
        let x = lca(s.p, cap_of(st));
        if is_ancestor(s.l, x) && x != s.l {
            return Ok(Some((
                lbl(Kind::YCadence, i, x),
                put(st, i, sub_commit(s, x, cap_of(st), "cadence")?),
            )));
        }
        if st.due {
            return Ok(None);
        }
        let mut n = st.clone();
        n.due = true;
        return Ok(Some((lbl(Kind::YDue, i, s.p), n)));
    }
    let mut s2 = s.clone();
    s2.p = nxt;
    s2.proc |= proc_bit(nxt);
    Ok(Some((lbl(Kind::YStep, i, nxt), put(st, i, s2))))
}

fn moves(st: &St, cfg: &Cfg) -> Res<Vec<Move>> {
    let mut out = v_moves(st, cfg);
    out.extend(w_moves(st, cfg));
    for (i, s) in st.subs.iter().enumerate() {
        if s.cls == Cls::Y {
            out.extend(syncing_step(st, i, cfg)?);
            continue;
        }
        if s.blocked {
            if let Some(o) = s.out
                && st.consumed & consumed_bit(i, o.fid) == 0
            {
                let mut n = st.clone();
                n.subs[i].blocked = false;
                n.tblk = if st.tblk == Some(i as u8) {
                    None
                } else {
                    st.tblk
                };
                out.push((lbl(Kind::Unblock, i, s.p), n));
            }
            continue;
        }
        if cfg.shared && st.tblk.is_some_and(|t| usize::from(t) != i) {
            continue;
        }
        out.extend(synced_step(st, i, cfg)?);
    }
    Ok(out)
}

fn invariants(st: &St, cfg: &Cfg) -> Res<()> {
    for (i, s) in st.subs.iter().enumerate() {
        if s.p.h > s.l.h + cfg.d {
            return Err(format!("data bound: sub {i} p={} L={}", s.p, s.l));
        }
        if !is_ancestor(s.l, s.p) {
            return Err(format!("sub {i}: L={} not on p={}", s.l, s.p));
        }
        if strictly_above(st.tack, s.l) {
            return Err(format!("R11a: sub {i} durable L={} strictly ahead of G={}", s.l, st.tack));
        }
        if s.cls == Cls::S && nqueue(s) > cfg.q {
            return Err(format!("queue bound: sub {i} holds {} > Q={}", nqueue(s), cfg.q));
        }
    }
    Ok(())
}

fn is_done(st: &St, cfg: &Cfg) -> bool {
    if st.pend.is_some() || st.reorged == Reorg::Mid || (cfg.reorg > 0 && st.reorged != Reorg::Yes)
    {
        return false;
    }
    if st.vtip != final_tip(cfg) || st.f.is_some() {
        return false;
    }
    if cfg.done == DoneMode::Quiescent {
        return !st.due
            && st.subs.iter().all(|s| {
                s.cls == Cls::S && s.p == st.vtip && s.queue.is_empty() && !s.parked && !s.blocked
            });
    }
    if st.tack != st.vtip {
        return false;
    }
    st.subs.iter().all(|s| {
        s.cls == Cls::S
            && s.p == st.vtip
            && s.l == st.vtip
            && s.queue.is_empty()
            && !s.parked
            && !s.blocked
    })
}

fn new_sub(cls: Cls) -> Sub {
    Sub {
        cls,
        p: GEN,
        l: GEN,
        queue: Vec::new(),
        out: None,
        parked: false,
        blocked: false,
        proc: 0,
        rreq: None,
        last_seq: -1,
        seq_synced: 0,
        skipped: 0,
        ready: false,
    }
}

fn initial(cfg: &Cfg) -> St {
    let subs = cfg
        .init
        .chars()
        .map(|c| {
            new_sub(if c == 'S' {
                Cls::S
            } else {
                Cls::Y
            })
        })
        .collect();
    St {
        vtip: GEN,
        tip_seq: -1,
        nseq: 0,
        pend: None,
        reorged: Reorg::No,
        since: 0,
        lastf: GEN,
        f: None,
        tack: GEN,
        due: false,
        nfid: 0,
        consumed: 0,
        subs,
        tblk: None,
    }
}

// ---------------------------------------------------------------------------------------------
// The explorer: BFS over the reachable states, then "every reachable state reaches done".
// ---------------------------------------------------------------------------------------------

/// The Python's default state limit.
const LIMIT: usize = 6_000_000;

#[derive(Default)]
struct Stats {
    states: usize,
    done: usize,
    /// States with no moves that are not done.
    deadlocks: usize,
    /// Reachable states from which no done state is reachable.
    cannot_finish: usize,
    /// All transitions (moves from every reachable state), as the Python's `explore_labels`.
    transitions: usize,
    /// Transitions in which the handoff re-check failed (counted over all moves from all states).
    unready: usize,
    /// Transitions that are a handoff taken with an event pending emission.
    synced_pending: usize,
    /// The shortest trace to a state that cannot finish.
    witness: Vec<Lbl>,
}

enum Outcome {
    /// An invariant failed; `trace` is the path to the state.
    Safety {
        msg: String,
        trace: Vec<Lbl>,
    },
    /// More than the limit's number of states.
    Limit {
        states: usize,
    },
    Complete(Stats),
}

impl Outcome {
    /// The Python's `ok`: no violation and every state can finish.
    fn ok(&self) -> bool {
        matches!(self, Outcome::Complete(s) if s.cannot_finish == 0)
    }

    fn stats(&self) -> &Stats {
        match self {
            Outcome::Complete(s) => s,
            other => panic!("expected a completed exploration, got {other}"),
        }
    }

    /// `(states, done, deadlocks, cannot_finish)`.
    fn counts(&self) -> (usize, usize, usize, usize) {
        let s = self.stats();
        (s.states, s.done, s.deadlocks, s.cannot_finish)
    }
}

fn tail(trace: &[Lbl]) -> String {
    let from = trace.len().saturating_sub(40);
    trace[from..].iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Safety {
                msg,
                trace,
            } => {
                write!(f, "safety violation: {msg}; trace ({} steps): {}", trace.len(), tail(trace))
            }
            Outcome::Limit {
                states,
            } => write!(f, "state limit exceeded at {states} states"),
            Outcome::Complete(s) => write!(
                f,
                "states={} done={} deadlocks={} cannot_finish={} witness ({} steps): {}",
                s.states,
                s.done,
                s.deadlocks,
                s.cannot_finish,
                s.witness.len(),
                tail(&s.witness)
            ),
        }
    }
}

fn trace(from: &[(u32, Lbl)], mut id: usize) -> Vec<Lbl> {
    let mut out = Vec::new();
    while from[id].0 != u32::MAX {
        out.push(from[id].1);
        id = from[id].0 as usize;
    }
    out.reverse();
    out
}

fn explore(cfg: &Cfg, init: Option<St>, limit: usize) -> Outcome {
    let s0 = Rc::new(init.unwrap_or_else(|| initial(cfg)));
    let mut ids: HashMap<Rc<St>, u32, Det> = HashMap::default();
    let mut states: Vec<Rc<St>> = vec![Rc::clone(&s0)];
    // The BFS tree: each state's predecessor and the move that first reached it.
    let mut from: Vec<(u32, Lbl)> = vec![(u32::MAX, lbl(Kind::Init, 0, GEN))];
    ids.insert(s0, 0);
    let mut edges: Vec<(u32, u32)> = Vec::new();
    let mut terminal = 0usize;
    let (mut unready, mut synced_pending, mut transitions) = (0usize, 0usize, 0usize);
    // BFS order is insertion order, so the frontier is a cursor into `states`.
    let mut head = 0usize;
    while head < states.len() {
        let id = head;
        head += 1;
        let st = Rc::clone(&states[id]);
        let ms = match invariants(&st, cfg).and_then(|()| moves(&st, cfg)) {
            Ok(ms) => ms,
            Err(msg) => {
                return Outcome::Safety {
                    msg,
                    trace: trace(&from, id),
                };
            }
        };
        if ms.is_empty() && !is_done(&st, cfg) {
            terminal += 1;
        }
        for (l, nx) in ms {
            transitions += 1;
            match l.kind {
                Kind::YUnready => unready += 1,
                Kind::YSyncedPending => synced_pending += 1,
                _ => {}
            }
            let nid = match ids.get(&nx) {
                Some(&n) => n,
                None => {
                    let n = states.len() as u32;
                    let rc = Rc::new(nx);
                    ids.insert(Rc::clone(&rc), n);
                    states.push(rc);
                    from.push((id as u32, l));
                    if states.len() > limit {
                        return Outcome::Limit {
                            states: states.len(),
                        };
                    }
                    n
                }
            };
            edges.push((id as u32, nid));
        }
    }
    drop(ids);
    let n = states.len();
    let done_flags: Vec<bool> = states.iter().map(|s| is_done(s, cfg)).collect();
    // AG EF done: walk the reverse edges from the done states.
    let mut start = vec![0u32; n + 1];
    for &(_, to) in &edges {
        start[to as usize + 1] += 1;
    }
    for k in 0..n {
        start[k + 1] += start[k];
    }
    let mut fill = start.clone();
    let mut preds = vec![0u32; edges.len()];
    for &(fr, to) in &edges {
        preds[fill[to as usize] as usize] = fr;
        fill[to as usize] += 1;
    }
    drop(edges);
    let mut good = done_flags.clone();
    let mut stack: Vec<u32> = (0..n as u32).filter(|&k| good[k as usize]).collect();
    while let Some(x) = stack.pop() {
        for &y in &preds[start[x as usize] as usize..start[x as usize + 1] as usize] {
            if !good[y as usize] {
                good[y as usize] = true;
                stack.push(y);
            }
        }
    }
    let cannot_finish = good.iter().filter(|&&g| !g).count();
    // The shortest witness: BFS ids are in order of depth, so the first bad id is a nearest one.
    let witness = good.iter().position(|&g| !g).map(|w| trace(&from, w)).unwrap_or_default();
    Outcome::Complete(Stats {
        states: n,
        done: done_flags.iter().filter(|&&d| d).count(),
        deadlocks: terminal,
        cannot_finish,
        transitions,
        unready,
        synced_pending,
        witness,
    })
}

// ---------------------------------------------------------------------------------------------
// The directed starting states.
// ---------------------------------------------------------------------------------------------

/// GPT r1 B1, exactly: L=0, p=8, D=8, F frozen at new=8 (event seq 7 already processed),
/// Connected(9..17) queued (seqs 8..16) before B1 returns.
fn gpt_b1_state() -> St {
    let queue = (9..18u8).map(|h| Ev::C(blk(h, 0), i32::from(h) - 1)).collect();
    let mut sub = new_sub(Cls::S);
    sub.p = blk(8, 0);
    sub.queue = queue;
    sub.proc = (1..9u8).fold(0, |acc, h| acc | proc_bit(blk(h, 0)));
    sub.last_seq = 7;
    St {
        vtip: blk(17, 0),
        tip_seq: 16,
        nseq: 17,
        pend: None,
        reorged: Reorg::No,
        since: 9,
        lastf: blk(8, 0),
        f: Some(Flush {
            fid: 0,
            new: blk(8, 0),
            stage: Stage::Pre,
            cap: 0,
            acked: 0,
            tseq: 7,
        }),
        tack: GEN,
        due: false,
        nfid: 1,
        consumed: 0,
        subs: vec![sub],
        tblk: None,
    }
}

fn gpt_b1_cfg(plant: &[Plant]) -> Cfg {
    Cfg {
        d: 8,
        q: 16,
        act: 99,
        maxh: 17,
        shared: true,
        init: "S",
        plant: plant.to_vec(),
        ..Cfg::default()
    }
}

/// GPT r2 note (injected, not claimed reachable): a subscriber already PARKED at Connected(3)
/// (L=1, D=1), with V back at the last frozen tip 2 and the matching Disconnected(3) still pending
/// emission; nothing changed since the freeze, no FlushDue posted.
fn injected_parked_state() -> St {
    let mut sub = new_sub(Cls::S);
    sub.p = blk(2, 0);
    sub.l = blk(1, 0);
    sub.queue = vec![Ev::C(blk(3, 0), 2)];
    sub.parked = true;
    sub.proc = proc_bit(blk(1, 0)) | proc_bit(blk(2, 0));
    sub.last_seq = 1;
    St {
        vtip: blk(2, 0),
        tip_seq: 3,
        nseq: 4,
        pend: Some(Ev::D(blk(3, 0), 3)),
        reorged: Reorg::Mid,
        since: 2,
        lastf: blk(2, 0),
        f: None,
        tack: blk(2, 0),
        due: false,
        nfid: 1,
        consumed: 0,
        subs: vec![sub],
        tblk: None,
    }
}

fn injected_cfg(plant: &[Plant]) -> Cfg {
    Cfg {
        init: "S",
        shared: true,
        d: 1,
        q: 2,
        act: 3,
        reorg: 1,
        maxh: 4,
        inval: true,
        done: DoneMode::Quiescent,
        plant: plant.to_vec(),
        ..Cfg::default()
    }
}

/// Opus R2-M6 witness configuration (the smallest): SS, shared, D=1, Q=1, no reorg, MAXH=3.
fn opus_r2_m6_cfg(plant: &[Plant]) -> Cfg {
    Cfg {
        init: "SS",
        shared: true,
        d: 1,
        q: 1,
        act: 1,
        maxh: 3,
        plant: plant.to_vec(),
        ..Cfg::default()
    }
}

// ---------------------------------------------------------------------------------------------
// The grid: 3 subscriber inits x shared/per-thread x 5 (D_max, Q, ACT) x 4 reorg modes = 120.
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Axes {
    init: &'static str,
    shared: bool,
    d: u8,
    q: usize,
    act: u32,
    reorg: u8,
    inval: bool,
}

impl Axes {
    fn cfg(self, plant: &[Plant]) -> Cfg {
        Cfg {
            init: self.init,
            shared: self.shared,
            d: self.d,
            q: self.q,
            act: self.act,
            reorg: self.reorg,
            inval: self.inval,
            maxh: 5,
            plant: plant.to_vec(),
            ..Cfg::default()
        }
    }

    fn name(self) -> String {
        format!(
            "init={},shared={},D={},Q={},ACT={},REORG={},inval={}",
            self.init, self.shared, self.d, self.q, self.act, self.reorg, self.inval
        )
    }
}

/// The Python's `GRID`, in its order (so `GRID[shard::nshards]` is the same shard).
fn grid() -> Vec<Axes> {
    let mut g = Vec::new();
    for init in ["SS", "SY", "YS"] {
        for shared in [true, false] {
            for (d, q, act) in [(1, 1, 1), (1, 1, 2), (2, 1, 2), (2, 2, 3), (1, 2, 3)] {
                for (reorg, inval) in [(0, false), (1, false), (2, false), (2, true)] {
                    g.push(Axes {
                        init,
                        shared,
                        d,
                        q,
                        act,
                        reorg,
                        inval,
                    });
                }
            }
        }
    }
    g
}

/// The Python model's `(states, done)` for each grid configuration, in `grid()` order, from its
/// `run-grid-*.txt` (every one passes: no deadlock, nothing that cannot finish). The sum is
/// 19,367,358, the figure of CH2 §4.4.3.
const EXPECTED: [(usize, usize); 120] = [
    (1942, 1),      //   0 SS shared D=1 Q=1 ACT=1 REORG=0
    (8244, 5),      //   1 SS shared D=1 Q=1 ACT=1 REORG=1
    (13692, 6),     //   2 SS shared D=1 Q=1 ACT=1 REORG=2
    (5020, 6),      //   3 SS shared D=1 Q=1 ACT=1 REORG=2 inval
    (3749, 4),      //   4 SS shared D=1 Q=1 ACT=2 REORG=0
    (11694, 13),    //   5 SS shared D=1 Q=1 ACT=2 REORG=1
    (24942, 19),    //   6 SS shared D=1 Q=1 ACT=2 REORG=2
    (7935, 8),      //   7 SS shared D=1 Q=1 ACT=2 REORG=2 inval
    (1470, 1),      //   8 SS shared D=2 Q=1 ACT=2 REORG=0
    (4567, 4),      //   9 SS shared D=2 Q=1 ACT=2 REORG=1
    (11935, 8),     //  10 SS shared D=2 Q=1 ACT=2 REORG=2
    (4402, 11),     //  11 SS shared D=2 Q=1 ACT=2 REORG=2 inval
    (2831, 2),      //  12 SS shared D=2 Q=2 ACT=3 REORG=0
    (28680, 14),    //  13 SS shared D=2 Q=2 ACT=3 REORG=1
    (25814, 20),    //  14 SS shared D=2 Q=2 ACT=3 REORG=2
    (8158, 8),      //  15 SS shared D=2 Q=2 ACT=3 REORG=2 inval
    (10529, 6),     //  16 SS shared D=1 Q=2 ACT=3 REORG=0
    (30114, 22),    //  17 SS shared D=1 Q=2 ACT=3 REORG=1
    (151372, 33),   //  18 SS shared D=1 Q=2 ACT=3 REORG=2
    (42859, 20),    //  19 SS shared D=1 Q=2 ACT=3 REORG=2 inval
    (1942, 1),      //  20 SS split  D=1 Q=1 ACT=1 REORG=0
    (8244, 5),      //  21 SS split  D=1 Q=1 ACT=1 REORG=1
    (13692, 6),     //  22 SS split  D=1 Q=1 ACT=1 REORG=2
    (5020, 6),      //  23 SS split  D=1 Q=1 ACT=1 REORG=2 inval
    (3749, 4),      //  24 SS split  D=1 Q=1 ACT=2 REORG=0
    (11694, 13),    //  25 SS split  D=1 Q=1 ACT=2 REORG=1
    (24942, 19),    //  26 SS split  D=1 Q=1 ACT=2 REORG=2
    (7935, 8),      //  27 SS split  D=1 Q=1 ACT=2 REORG=2 inval
    (1470, 1),      //  28 SS split  D=2 Q=1 ACT=2 REORG=0
    (4567, 4),      //  29 SS split  D=2 Q=1 ACT=2 REORG=1
    (11935, 8),     //  30 SS split  D=2 Q=1 ACT=2 REORG=2
    (4402, 11),     //  31 SS split  D=2 Q=1 ACT=2 REORG=2 inval
    (2831, 2),      //  32 SS split  D=2 Q=2 ACT=3 REORG=0
    (28680, 14),    //  33 SS split  D=2 Q=2 ACT=3 REORG=1
    (25814, 20),    //  34 SS split  D=2 Q=2 ACT=3 REORG=2
    (8158, 8),      //  35 SS split  D=2 Q=2 ACT=3 REORG=2 inval
    (10529, 6),     //  36 SS split  D=1 Q=2 ACT=3 REORG=0
    (30114, 22),    //  37 SS split  D=1 Q=2 ACT=3 REORG=1
    (151372, 33),   //  38 SS split  D=1 Q=2 ACT=3 REORG=2
    (42859, 20),    //  39 SS split  D=1 Q=2 ACT=3 REORG=2 inval
    (15040, 11),    //  40 SY shared D=1 Q=1 ACT=1 REORG=0
    (108041, 85),   //  41 SY shared D=1 Q=1 ACT=1 REORG=1
    (278184, 181),  //  42 SY shared D=1 Q=1 ACT=1 REORG=2
    (62094, 97),    //  43 SY shared D=1 Q=1 ACT=1 REORG=2 inval
    (29514, 37),    //  44 SY shared D=1 Q=1 ACT=2 REORG=0
    (144075, 182),  //  45 SY shared D=1 Q=1 ACT=2 REORG=1
    (489134, 470),  //  46 SY shared D=1 Q=1 ACT=2 REORG=2
    (87377, 115),   //  47 SY shared D=1 Q=1 ACT=2 REORG=2 inval
    (11873, 10),    //  48 SY shared D=2 Q=1 ACT=2 REORG=0
    (57399, 55),    //  49 SY shared D=2 Q=1 ACT=2 REORG=1
    (220221, 172),  //  50 SY shared D=2 Q=1 ACT=2 REORG=2
    (50335, 133),   //  51 SY shared D=2 Q=1 ACT=2 REORG=2 inval
    (19278, 17),    //  52 SY shared D=2 Q=2 ACT=3 REORG=0
    (200826, 133),  //  53 SY shared D=2 Q=2 ACT=3 REORG=1
    (358871, 392),  //  54 SY shared D=2 Q=2 ACT=3 REORG=2
    (67075, 84),    //  55 SY shared D=2 Q=2 ACT=3 REORG=2 inval
    (63795, 54),    //  56 SY shared D=1 Q=2 ACT=3 REORG=0
    (278600, 256),  //  57 SY shared D=1 Q=2 ACT=3 REORG=1
    (1776271, 692), //  58 SY shared D=1 Q=2 ACT=3 REORG=2
    (323862, 225),  //  59 SY shared D=1 Q=2 ACT=3 REORG=2 inval
    (15040, 11),    //  60 SY split  D=1 Q=1 ACT=1 REORG=0
    (108041, 85),   //  61 SY split  D=1 Q=1 ACT=1 REORG=1
    (278184, 181),  //  62 SY split  D=1 Q=1 ACT=1 REORG=2
    (62094, 97),    //  63 SY split  D=1 Q=1 ACT=1 REORG=2 inval
    (29514, 37),    //  64 SY split  D=1 Q=1 ACT=2 REORG=0
    (144075, 182),  //  65 SY split  D=1 Q=1 ACT=2 REORG=1
    (489134, 470),  //  66 SY split  D=1 Q=1 ACT=2 REORG=2
    (87377, 115),   //  67 SY split  D=1 Q=1 ACT=2 REORG=2 inval
    (11873, 10),    //  68 SY split  D=2 Q=1 ACT=2 REORG=0
    (57399, 55),    //  69 SY split  D=2 Q=1 ACT=2 REORG=1
    (220221, 172),  //  70 SY split  D=2 Q=1 ACT=2 REORG=2
    (50335, 133),   //  71 SY split  D=2 Q=1 ACT=2 REORG=2 inval
    (19278, 17),    //  72 SY split  D=2 Q=2 ACT=3 REORG=0
    (200826, 133),  //  73 SY split  D=2 Q=2 ACT=3 REORG=1
    (358871, 392),  //  74 SY split  D=2 Q=2 ACT=3 REORG=2
    (67075, 84),    //  75 SY split  D=2 Q=2 ACT=3 REORG=2 inval
    (63795, 54),    //  76 SY split  D=1 Q=2 ACT=3 REORG=0
    (278600, 256),  //  77 SY split  D=1 Q=2 ACT=3 REORG=1
    (1776271, 692), //  78 SY split  D=1 Q=2 ACT=3 REORG=2
    (323862, 225),  //  79 SY split  D=1 Q=2 ACT=3 REORG=2 inval
    (15040, 11),    //  80 YS shared D=1 Q=1 ACT=1 REORG=0
    (108041, 85),   //  81 YS shared D=1 Q=1 ACT=1 REORG=1
    (278184, 181),  //  82 YS shared D=1 Q=1 ACT=1 REORG=2
    (62094, 97),    //  83 YS shared D=1 Q=1 ACT=1 REORG=2 inval
    (29514, 37),    //  84 YS shared D=1 Q=1 ACT=2 REORG=0
    (144075, 182),  //  85 YS shared D=1 Q=1 ACT=2 REORG=1
    (489134, 470),  //  86 YS shared D=1 Q=1 ACT=2 REORG=2
    (87377, 115),   //  87 YS shared D=1 Q=1 ACT=2 REORG=2 inval
    (11873, 10),    //  88 YS shared D=2 Q=1 ACT=2 REORG=0
    (57399, 55),    //  89 YS shared D=2 Q=1 ACT=2 REORG=1
    (220221, 172),  //  90 YS shared D=2 Q=1 ACT=2 REORG=2
    (50335, 133),   //  91 YS shared D=2 Q=1 ACT=2 REORG=2 inval
    (19278, 17),    //  92 YS shared D=2 Q=2 ACT=3 REORG=0
    (200826, 133),  //  93 YS shared D=2 Q=2 ACT=3 REORG=1
    (358871, 392),  //  94 YS shared D=2 Q=2 ACT=3 REORG=2
    (67075, 84),    //  95 YS shared D=2 Q=2 ACT=3 REORG=2 inval
    (63795, 54),    //  96 YS shared D=1 Q=2 ACT=3 REORG=0
    (278600, 256),  //  97 YS shared D=1 Q=2 ACT=3 REORG=1
    (1776271, 692), //  98 YS shared D=1 Q=2 ACT=3 REORG=2
    (323862, 225),  //  99 YS shared D=1 Q=2 ACT=3 REORG=2 inval
    (15040, 11),    // 100 YS split  D=1 Q=1 ACT=1 REORG=0
    (108041, 85),   // 101 YS split  D=1 Q=1 ACT=1 REORG=1
    (278184, 181),  // 102 YS split  D=1 Q=1 ACT=1 REORG=2
    (62094, 97),    // 103 YS split  D=1 Q=1 ACT=1 REORG=2 inval
    (29514, 37),    // 104 YS split  D=1 Q=1 ACT=2 REORG=0
    (144075, 182),  // 105 YS split  D=1 Q=1 ACT=2 REORG=1
    (489134, 470),  // 106 YS split  D=1 Q=1 ACT=2 REORG=2
    (87377, 115),   // 107 YS split  D=1 Q=1 ACT=2 REORG=2 inval
    (11873, 10),    // 108 YS split  D=2 Q=1 ACT=2 REORG=0
    (57399, 55),    // 109 YS split  D=2 Q=1 ACT=2 REORG=1
    (220221, 172),  // 110 YS split  D=2 Q=1 ACT=2 REORG=2
    (50335, 133),   // 111 YS split  D=2 Q=1 ACT=2 REORG=2 inval
    (19278, 17),    // 112 YS split  D=2 Q=2 ACT=3 REORG=0
    (200826, 133),  // 113 YS split  D=2 Q=2 ACT=3 REORG=1
    (358871, 392),  // 114 YS split  D=2 Q=2 ACT=3 REORG=2
    (67075, 84),    // 115 YS split  D=2 Q=2 ACT=3 REORG=2 inval
    (63795, 54),    // 116 YS split  D=1 Q=2 ACT=3 REORG=0
    (278600, 256),  // 117 YS split  D=1 Q=2 ACT=3 REORG=1
    (1776271, 692), // 118 YS split  D=1 Q=2 ACT=3 REORG=2
    (323862, 225),  // 119 YS split  D=1 Q=2 ACT=3 REORG=2 inval
];

/// The total of `EXPECTED`: CH2 §4.4.3's "120 configurations, 19,367,358 states".
const GRID_TOTAL_STATES: usize = 19_367_358;

/// The handoff counters over the Syncing-and-reorg part of the grid (60 configurations), from the
/// Python's `run-handoff.txt`: re-check failures, and handoffs taken with an event pending.
const HANDOFF_UNREADY_TOTAL: usize = 983_348;
const HANDOFF_PENDING_TOTAL: usize = 160_320;

// ---------------------------------------------------------------------------------------------
// Test drivers.
// ---------------------------------------------------------------------------------------------

/// Runs `explore` for grid configuration `i` and checks it against the Python's counts: the
/// state and done counts, and no deadlock and nothing that cannot finish.
fn check_grid_config(i: usize, ax: Axes) -> Stats {
    let out = explore(&ax.cfg(&[]), None, LIMIT);
    let name = ax.name();
    let Outcome::Complete(stats) = out else {
        panic!("grid[{i}] {name}: {out}");
    };
    assert!(
        stats.deadlocks == 0 && stats.cannot_finish == 0,
        "grid[{i}] {name}: deadlocks={} cannot_finish={} witness: {}",
        stats.deadlocks,
        stats.cannot_finish,
        tail(&stats.witness)
    );
    assert_eq!(
        (stats.states, stats.done),
        EXPECTED[i],
        "grid[{i}] {name}: (states, done) differ from liveness_model_v3.py"
    );
    stats
}

/// What a worker adds up over its configurations.
#[derive(Default)]
struct Totals {
    states: usize,
    unready: usize,
    synced_pending: usize,
}

/// Checks the grid configurations `indices` on `threads` worker threads (largest first).
fn check_grid(indices: &[usize], threads: usize) -> Totals {
    let axes = grid();
    let mut order = indices.to_vec();
    order.sort_by_key(|&i| std::cmp::Reverse(EXPECTED[i].0));
    let next = AtomicUsize::new(0);
    let totals = std::sync::Mutex::new(Totals::default());
    std::thread::scope(|scope| {
        for _ in 0..threads.clamp(1, order.len().max(1)) {
            scope.spawn(|| {
                loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&i) = order.get(k) else {
                        break;
                    };
                    let ax = axes[i];
                    let stats = check_grid_config(i, ax);
                    let mut t = totals.lock().unwrap();
                    t.states += stats.states;
                    if ax.init != "SS" && ax.reorg > 0 {
                        t.unready += stats.unready;
                        t.synced_pending += stats.synced_pending;
                    }
                }
            });
        }
    });
    totals.into_inner().unwrap()
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name).map_or(default, |v| v.parse().unwrap_or_else(|_| panic!("{name}={v}")))
}

/// The expected outcome of a planted or directed run.
fn assert_counts(what: &str, out: &Outcome, expect: (usize, usize, usize, usize)) {
    assert!(matches!(out, Outcome::Complete(_)), "{what}: {out}");
    assert_eq!(
        out.counts(),
        expect,
        "{what}: (states, done, deadlocks, cannot_finish) differ from the Python"
    );
}

fn run_planted(ax: Axes, plant: Plant) -> Outcome {
    explore(&ax.cfg(&[plant]), None, LIMIT)
}

/// Finds the first grid configuration (in the Python's order) in `pool` on which `plants` fails,
/// as the Python's plant search does.
fn first_failure(plants: &[Plant], pool: impl Iterator<Item = usize>) -> Option<(usize, Outcome)> {
    let axes = grid();
    for i in pool {
        let out = explore(&axes[i].cfg(plants), None, LIMIT);
        if !out.ok() {
            return Some((i, out));
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// The grid.
// ---------------------------------------------------------------------------------------------

/// Indices into `grid()`: `init * 40 + (split as usize) * 20 + triple * 4 + reorg_mode`, where the
/// triples are (D,Q,ACT) = (1,1,1) (1,1,2) (2,1,2) (2,2,3) (1,2,3) and the reorg modes are none,
/// depth 1, depth 2, depth-2 `invalidateblock`.
///
/// The 20 configurations cover:
/// - every subscriber-init combination (SS: 10, SY: 7, YS: 3);
/// - shared and per-thread events threads (10 each);
/// - every (D_max, Q, ACT) triple (4 configurations each);
/// - every reorg mode (none 8, depth 1 5, depth 2 2, `invalidateblock` 5);
/// - both handoff races (the Syncing configurations with a reorg: see `handoff_coverage`).
const PR_SUBSET: [usize; 20] = [
    0, 7, 9, 12, 16, // SS shared: t0/none, t1/inval, t2/r1, t3/none, t4/none
    22, 25, 30, 35, 37, // SS split: t0/r2, t1/r1, t2/r2, t3/inval, t4/r1
    48, 44, 51, // SY shared: t2/none, t1/none, t2/inval
    60, 69, 63, // SY split: t0/none, t2/r1, t0/inval
    84, 76, 109,
    83, // YS shared t1/none, SY split t4/none, YS split t2/r1, YS shared t0/inval
];

/// 20 of the 120 grid configurations (see `PR_SUBSET`); the whole grid is `grid_full`.
#[test]
fn grid_pr_subset() {
    let axes = grid();
    let inits: std::collections::BTreeSet<_> = PR_SUBSET.iter().map(|&i| axes[i].init).collect();
    assert_eq!(inits.len(), 3, "the subset covers every init");
    assert!(
        PR_SUBSET.iter().any(|&i| axes[i].shared) && PR_SUBSET.iter().any(|&i| !axes[i].shared)
    );
    let triples: std::collections::BTreeSet<_> =
        PR_SUBSET.iter().map(|&i| (axes[i].d, axes[i].q, axes[i].act)).collect();
    assert_eq!(triples.len(), 5, "the subset covers every (D_max, Q, ACT)");
    let modes: std::collections::BTreeSet<_> =
        PR_SUBSET.iter().map(|&i| (axes[i].reorg, axes[i].inval)).collect();
    assert_eq!(modes.len(), 4, "the subset covers every reorg mode");
    check_grid(&PR_SUBSET, env_usize("LIVENESS_THREADS", 4));
}

/// The whole rev3 grid: 120 configurations, 19,367,358 states. Nightly: `cargo test --release
/// -p dashd-chainstate --test subscriber_liveness_model -- --ignored grid_full`.
/// `LIVENESS_SHARD` / `LIVENESS_NSHARDS` run `GRID[shard::nshards]` as the Python's `--shard`;
/// `LIVENESS_THREADS` sets the worker threads (default 4).
#[test]
#[ignore = "nightly: 19 million states"]
fn grid_full() {
    let shard = env_usize("LIVENESS_SHARD", 0);
    let nshards = env_usize("LIVENESS_NSHARDS", 1);
    assert!(nshards >= 1 && shard < nshards, "LIVENESS_SHARD={shard} LIVENESS_NSHARDS={nshards}");
    let indices: Vec<usize> = (shard..EXPECTED.len()).step_by(nshards).collect();
    let totals = check_grid(&indices, env_usize("LIVENESS_THREADS", 4));
    let expected: usize = indices.iter().map(|&i| EXPECTED[i].0).sum();
    assert_eq!(totals.states, expected, "the shard's total number of states");
    if nshards == 1 {
        assert_eq!(
            totals.states, GRID_TOTAL_STATES,
            "the grid's total number of states (CH2 §4.4.3)"
        );
        assert_eq!(
            totals.unready, HANDOFF_UNREADY_TOTAL,
            "re-check failures over the Syncing+reorg grid"
        );
        assert_eq!(totals.synced_pending, HANDOFF_PENDING_TOTAL, "handoffs with an event pending");
    }
    println!("grid_full: {} configurations, {} states", indices.len(), totals.states);
}

#[test]
fn expected_table_matches_the_design() {
    assert_eq!(grid().len(), EXPECTED.len());
    assert_eq!(EXPECTED.iter().map(|e| e.0).sum::<usize>(), GRID_TOTAL_STATES);
    for (k, &i) in PR_SUBSET.iter().enumerate() {
        assert!(!PR_SUBSET[..k].contains(&i), "grid[{i}] listed twice");
    }
}

// ---------------------------------------------------------------------------------------------
// Directed regressions: the unplanted model passes, the planted one fails.
// ---------------------------------------------------------------------------------------------

/// GPT r1 B1, exact schedule (rule 1's case A by sequence number).
#[test]
fn directed_gpt_b1() {
    let cfg = gpt_b1_cfg(&[]);
    assert_counts("GPT-B1, no plant", &explore(&cfg, Some(gpt_b1_state()), LIMIT), (122, 3, 0, 0));
    // X18: no case-A ack.
    let cfg = gpt_b1_cfg(&[Plant::NoCaseA]);
    let out = explore(&cfg, Some(gpt_b1_state()), LIMIT);
    assert_counts("GPT-B1, X18", &out, (5, 0, 1, 5));
    assert!(!out.ok());
}

/// GPT r2's injected parked state: wake on enqueue (X23), compression and the empty flush (X20,
/// X24).
#[test]
fn directed_injected_parked_state() {
    let run = |plant: &[Plant]| explore(&injected_cfg(plant), Some(injected_parked_state()), LIMIT);
    assert_counts("injected, no plant", &run(&[]), (5, 1, 0, 0));
    // X23: no wake on enqueue.
    let out = run(&[Plant::NoWake]);
    assert_counts("injected, X23", &out, (3, 0, 1, 3));
    assert!(!out.ok());
    // X20 alone and X24 alone are subsumed by the other rule.
    let out = run(&[Plant::NoCompress]);
    assert_counts("injected, X20 alone", &out, (25, 1, 0, 0));
    assert!(out.ok());
    let out = run(&[Plant::NoEmptyFlush]);
    assert_counts("injected, X24 alone", &out, (5, 1, 0, 0));
    assert!(out.ok());
    // X20 with X24: nothing wakes the parked subscriber.
    let out = run(&[Plant::NoCompress, Plant::NoEmptyFlush]);
    assert_counts("injected, X20+X24", &out, (5, 0, 1, 5));
    assert!(!out.ok());
}

/// Opus R2-M6's witness configuration (smallest): case A by sequence number, not by queue
/// membership.
#[test]
fn directed_opus_r2_m6() {
    assert_counts(
        "Opus R2-M6, no plant",
        &explore(&opus_r2_m6_cfg(&[]), None, LIMIT),
        (834, 1, 0, 0),
    );
    // X22: case A by queue membership.
    let out = explore(&opus_r2_m6_cfg(&[Plant::QueueCaseA]), None, LIMIT);
    assert_counts("Opus R2-M6, X22", &out, (5497, 7, 9, 797));
    assert!(!out.ok());
    assert_eq!(out.stats().witness.len(), 24, "the shortest witness has the Python's length");
}

// ---------------------------------------------------------------------------------------------
// Planted regressions (§4.4.3's table): each must be caught.
// ---------------------------------------------------------------------------------------------

fn axes_of(i: usize) -> Axes {
    grid()[i]
}

/// X17: the events thread blocks at the limit (Opus F9). Caught by the grid.
#[test]
fn plants_x17_block_at_the_limit() {
    let out = run_planted(axes_of(4), Plant::BlockAtLimit);
    assert_counts("X17", &out, (3772, 4, 4, 4));
    assert!(!out.ok());
    assert_eq!(out.stats().witness.len(), 30);
    // The Python's search finds it first on grid[4].
    let (i, _) = first_failure(&[Plant::BlockAtLimit], 0..5).expect("caught");
    assert_eq!(i, 4);
}

/// X18: no case-A ack (GPT B1). Caught by the grid.
#[test]
fn plants_x18_no_case_a() {
    let out = run_planted(axes_of(0), Plant::NoCaseA);
    assert_counts("X18", &out, (648, 1, 9, 434));
    assert!(!out.ok());
    assert_eq!(out.stats().witness.len(), 3);
}

/// X19: a backward B1 without the rewind gate. Caught by the grid, as an R11a violation.
#[test]
fn plants_x19_no_rewind_gate() {
    let out = run_planted(axes_of(1), Plant::NoRgate);
    match &out {
        Outcome::Safety {
            msg,
            trace,
        } => {
            assert_eq!(msg, "R11a: sub 0 durable L=(3, 0) strictly ahead of G=(2, 0)");
            assert_eq!(trace.len(), 36, "the Python's trace length");
        }
        other => panic!("X19 must be an R11a violation, got {other}"),
    }
    let (i, _) = first_failure(&[Plant::NoRgate], 0..2).expect("caught");
    assert_eq!(i, 1);
}

/// X22: case A by queue membership (Opus R2-M6). Caught by the grid.
#[test]
fn plants_x22_queue_case_a() {
    let out = run_planted(axes_of(0), Plant::QueueCaseA);
    assert_counts("X22", &out, (27528, 11, 15, 1703));
    assert!(!out.ok());
    assert_eq!(out.stats().witness.len(), 48);
}

/// X27: the catch-up handoff without the live-tip re-check (G-R3-M4). Caught by the grid, on the
/// Syncing configurations.
#[test]
fn plants_x27_nonatomic_handoff() {
    let out = run_planted(axes_of(40), Plant::NonatomicHandoff);
    assert_counts("X27", &out, (43063, 11, 44, 29393));
    assert!(!out.ok());
    assert_eq!(out.stats().witness.len(), 2);
    // The Python searches the non-SS configurations only; the first is grid[40].
    let pool = (0..EXPECTED.len()).filter(|&i| axes_of(i).init != "SS").take(1);
    assert_eq!(first_failure(&[Plant::NonatomicHandoff], pool).map(|f| f.0), Some(40));
}

/// X28: the pre-cut pending event delivered at the handoff (rev2). Caught by the grid, by the
/// F10 exclusion invariant.
#[test]
fn plants_x28_include_pending() {
    let out = run_planted(axes_of(40), Plant::IncludePending);
    match &out {
        Outcome::Safety {
            msg,
            trace,
        } => {
            assert_eq!(
                msg,
                "sub 1: ignored Connected((1, 0)) whose parent is a strict ancestor of p=(1, 0) \
                 (pre-cut or lost event)"
            );
            assert_eq!(trace.len(), 5, "the Python's trace length");
        }
        other => panic!("X28 must violate F10's exclusion, got {other}"),
    }
}

/// X20, X23 and X24 are caught only on the injected state (§4.4.3): rules 1 and 5 subsume them
/// in the grid. X23 alone, and X20 with X24, fail on the injected parked state. X20 and X24 alone
/// do not (`directed_injected_parked_state`).
#[test]
fn plants_x20_x23_x24_on_the_injected_state() {
    let run = |plant: &[Plant]| explore(&injected_cfg(plant), Some(injected_parked_state()), LIMIT);
    assert!(!run(&[Plant::NoWake]).ok(), "X23");
    assert!(!run(&[Plant::NoCompress, Plant::NoEmptyFlush]).ok(), "X20 with X24");
    assert!(run(&[Plant::NoCompress]).ok(), "X20 alone");
    assert!(run(&[Plant::NoEmptyFlush]).ok(), "X24 alone");
    // On a small grid configuration, the same plants are not caught.
    for plant in
        [&[Plant::NoWake][..], &[Plant::NoCompress, Plant::NoEmptyFlush], &[Plant::NoEmptyFlush]]
    {
        for i in [0, 1, 7, 12] {
            let out = explore(&axes_of(i).cfg(plant), None, LIMIT);
            assert!(out.ok(), "grid[{i}] {plant:?}: {out}");
        }
    }
}

/// X20, X23 and X24 are not reachable failures anywhere in the grid (§4.4.3). Nightly.
#[test]
#[ignore = "nightly: three more passes over the grid"]
fn plants_x20_x23_x24_not_reachable_in_the_grid() {
    let shard = env_usize("LIVENESS_SHARD", 0);
    let nshards = env_usize("LIVENESS_NSHARDS", 1);
    let pool: Vec<usize> = (shard..EXPECTED.len()).step_by(nshards).collect();
    for plants in
        [&[Plant::NoCompress, Plant::NoEmptyFlush][..], &[Plant::NoWake], &[Plant::NoEmptyFlush]]
    {
        if let Some((i, out)) = first_failure(plants, pool.iter().copied()) {
            panic!("{plants:?} fails on grid[{i}]: {out}");
        }
    }
}

/// The two plants the Python defines but §4.4.3's table does not list. They change nothing in
/// the small configurations here (`no-eager`) or only the state count (`no-drop`).
#[test]
fn plants_no_eager_no_drop_match_the_python() {
    for (i, eager, no_drop) in [(1, (8244, 5), (5415, 2)), (13, (28680, 14), (30093, 20))] {
        let out = run_planted(axes_of(i), Plant::NoEager);
        assert_counts("no-eager", &out, (eager.0, eager.1, 0, 0));
        let out = run_planted(axes_of(i), Plant::NoDrop);
        assert_counts("no-drop", &out, (no_drop.0, no_drop.1, 0, 0));
    }
}

// ---------------------------------------------------------------------------------------------
// Handoff coverage (G-R3-M4).
// ---------------------------------------------------------------------------------------------

/// On Syncing-and-reorg configurations both of Sol's races are reachable in the correct model,
/// which completes through them: V's tip moved between catch-up and `subscribe_at` (the re-check
/// fails), and a handoff taken with an event pending emission. The counts are the Python's
/// `explore_labels` over the same configuration.
#[test]
fn handoff_coverage() {
    // (grid index, transitions, re-check failures, handoffs with an event pending)
    for (i, transitions, unready, pending) in
        [(49, 144_364, 4702, 777), (51, 129_749, 2577, 795), (83, 166_512, 2641, 969)]
    {
        let ax = axes_of(i);
        assert!(ax.init != "SS" && ax.reorg > 0);
        let stats = check_grid_config(i, ax);
        assert!(stats.unready > 0, "grid[{i}]: the re-check never fails");
        assert!(stats.synced_pending > 0, "grid[{i}]: no handoff with an event pending");
        assert_eq!(
            (stats.transitions, stats.unready, stats.synced_pending),
            (transitions, unready, pending),
            "grid[{i}] {}",
            ax.name()
        );
    }
}
