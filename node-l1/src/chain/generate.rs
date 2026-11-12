//! Intents to ops (O2b §3.2–§3.4): the world model picks parents, coins and targets, the
//! builder makes the blocks, the tie model (`ties`) defers, retargets or drops what could tie,
//! and each op carries what the generator expects Core to reply (checked when it runs, against
//! the world as it then is: O2b-3 §2.5).

use std::collections::{BTreeMap, BTreeSet};

use dash_oracle_sys::{ChainOp, FlushMode, RestartKind};

use super::build::{
    self, BlockPlan, Built, Input, Lock, MATURITY, REGTEST_BITS, SPACING, TESTDUMMY_BIT, TxPlan,
    VERSION_BITS_TOP,
};
use super::census::Census;
use super::core::{DIP3_HEIGHT, SPORK_19, SPORK_OFF, Schedule};
use super::intent::{
    self, DELIVER_REVERSED, Encoding, HEADERS_FIRST, HOLD, Intent, Ref, RefKind, SIGNAL, TxShape,
};
use super::mutate::{self, BANK, OPERATORS, Operator, Stage, bank_select, chain_operator};
use super::step::ReplyClass;
use super::ties;
use super::world::{Block, Coin, Hash, Held, Outpoint, Seq, World};

/// At most this many blocks built per input (O2b §6.3).
pub const MAX_BLOCKS: u32 = 64;
/// At most this many transactions per block (O2b §6.3).
pub const MAX_TXS: usize = 16;
/// The fan-out shape's outputs, on every schedule but S0p.
pub const FAN_OUT: usize = 32;
/// S0p's fan-out shape's outputs (OD-L3-117): the smallest power of two at which S0p's directed
/// seeds reach a natural CACHE_LARGE mirror, at over half S0's executions per CPU-second.
pub const S0P_FAN_OUT: usize = 512;

/// The fan-out shape's outputs on `schedule` ([`World::fan_out`]).
pub fn fan_out(schedule: Schedule) -> usize {
    match schedule {
        Schedule::S0p => S0P_FAN_OUT,
        Schedule::S0 | Schedule::S1 | Schedule::S2 => FAN_OUT,
    }
}
/// The smallest coin a block spends: every shape and operator leaves each output positive.
pub(crate) const MIN_COIN: i64 = 100_000;
/// Forks start at most this far below the active tip.
const MAX_FORK_DEPTH: u8 = 12;

/// What an op is, for its expectation (a block's mutation, if any, is the world's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum What {
    Block(usize),
    Header(usize),
    Proposal {
        id: usize,
        again: bool,
    },
    /// Invalidate, reconsider, precious: the target, if the model knows it.
    Target(Option<usize>),
    /// The reply's class must be [`ReplyClass::Ok`].
    Ok,
    /// The op's reply follows from the op and the world (ChainLock records, restarts).
    Op,
    /// Anything goes (raw blocks, activation).
    Any,
}

/// One op the generator planned.
#[derive(Clone, Debug)]
pub struct Planned {
    pub op: ChainOp,
    pub what: What,
}

impl Planned {
    pub fn new(op: ChainOp, what: What) -> Planned {
        Planned {
            op,
            what,
        }
    }
}

/// An expectation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expect {
    /// Stored and not connected: no BlockChecked for it (`inconclusive`).
    Stored,
    Ok,
    /// A reject with one of these prefixes.
    Reject(&'static [&'static str]),
    Down,
    Any,
}

impl Expect {
    /// For the census: `reject` lists its prefixes.
    pub fn name(&self) -> String {
        match self {
            Expect::Stored => "stored".into(),
            Expect::Ok => "ok".into(),
            Expect::Reject(prefixes) => format!("reject {}", prefixes.join("|")),
            Expect::Down => "down".into(),
            Expect::Any => "any".into(),
        }
    }

    pub fn met(&self, class: &ReplyClass) -> bool {
        match (self, class) {
            (Expect::Any, _) => true,
            (Expect::Down, ReplyClass::Down) => true,
            (Expect::Ok, ReplyClass::Ok) => true,
            (Expect::Stored, ReplyClass::Inconclusive) => true,
            (Expect::Reject(prefixes), ReplyClass::Reject(r)) => {
                prefixes.iter().any(|p| r.starts_with(p))
            }
            _ => false,
        }
    }
}

/// Whether block `id` conflicts with the best ChainLock: it is at or below the locked height
/// and not the locked block's ancestor (Core's `HasConflictingChainLock`, against the lock whose
/// block it has indexed).
pub fn cl_conflict(w: &World, id: usize) -> bool {
    w.best_cl.filter(|&cl| w.blocks[cl].indexed()).is_some_and(|cl| conflicts_with(w, cl, id))
}

/// Whether block `id` is at or below lock block `cl`'s height and not its ancestor.
fn conflicts_with(w: &World, cl: usize, id: usize) -> bool {
    let b = &w.blocks[id];
    b.height <= w.blocks[cl].height && w.ancestor(cl, b.height) != Some(id)
}

/// Whether block `id` conflicts with a lock a reset dropped ([`World::cl_dropped`]): Core's
/// marks against it outlive it, and the model's `invalid` can miss them (4f2 DESIGN §4.2, r2).
pub fn cl_dropped_conflict(w: &World, id: usize) -> bool {
    w.cl_dropped.iter().any(|&cl| conflicts_with(w, cl, id))
}

/// Whether Core may hold a conflict mark on block `id` by the geometry: [`cl_conflict`] or
/// [`cl_dropped_conflict`]. The ungated readers of the mark (`v:4654`) read this, and answer
/// `Any` on it.
pub fn cl_mark_maybe(w: &World, id: usize) -> bool {
    cl_conflict(w, id) || cl_dropped_conflict(w, id)
}

/// [`cl_conflict`] as Core's `HasConflictingChainLock` answers it: nothing conflicts while spork
/// 19 is off (`chainlock/chainlock.cpp:73-82`). Conflict marks Core set before are not gated
/// (`bad-prevblk-chainlock`): those sites read [`cl_conflict`].
pub fn cl_conflict_live(w: &World, id: usize) -> bool {
    w.spork19_active() && cl_conflict(w, id)
}

/// Whether Core would connect block `id` on arrival: every block between it and the active
/// chain has its data, and so does a chain on it (it, or its stored descendants) with more
/// work than the tip.
fn connects(w: &World, id: usize) -> bool {
    let linked = w
        .ancestry(id)
        .skip(1)
        .take_while(|&a| !w.is_ancestor(a, w.tip))
        .all(|a| w.blocks[a].held == Held::Data);
    linked && stored_reach(w, id) > w.blocks[w.tip].height
}

/// The height of the highest block reachable from `id` (itself included) through
/// descendants whose data Core holds and that are not marked (`FindMostWorkChain` skips those).
fn stored_reach(w: &World, id: usize) -> u32 {
    let mut top = w.blocks[id].height;
    let mut next = vec![id];
    while let Some(b) = next.pop() {
        for c in w.children(b).filter(|&c| w.blocks[c].held == Held::Data && !w.blocks[c].invalid) {
            top = top.max(w.blocks[c].height);
            next.push(c);
        }
    }
    top
}

/// What Core should reply to `p`, given the world before it runs.
pub fn expect(w: &World, p: &Planned) -> Expect {
    expect_named(w, p).0
}

/// The inventory occurrences a reply to `p` can be credited to (O2b-4 §4.2): the model names a
/// site only where the world fixes it, and the census credits it only if the reply carries the
/// site's reason. Naming never changes what [`Expect::met`] accepts.
#[derive(Clone, Copy, Default)]
pub enum Named {
    #[default]
    None,
    /// The block's operator, as its first variant says Core refuses it in this mode.
    Op(&'static Operator, mutate::Mode),
    /// A structural reply: its reason and occurrence.
    Site(&'static str, &'static str),
}

impl Named {
    /// The (reason, occurrence) a reply at `par` is credited to, if any.
    pub fn site(self, par: mutate::Par) -> Option<(&'static str, &'static str)> {
        match self {
            Named::None => None,
            Named::Op(op, mode) => {
                let v = op.variants.first()?;
                match (v.expect)(mode, par, *v.sites.first()?) {
                    mutate::Expect::Reject {
                        reason,
                        occurrence,
                    } => Some((reason, occurrence)),
                    _ => None,
                }
            }
            Named::Site(reason, occurrence) => Some((reason, occurrence)),
        }
    }
}

// The structural occurrences the model names (inventory keys, v24.0.0).
const ABH_DUPLICATE: &str = "validation.cpp:AcceptBlockHeader#1:duplicate";
const ABH_DUPLICATE_CL: &str = "validation.cpp:AcceptBlockHeader#2:duplicate";
const ABH_NO_PREV: &str = "validation.cpp:AcceptBlockHeader#3:prev-blk-not-found";
const ABH_BAD_PREV: &str = "validation.cpp:AcceptBlockHeader#4:bad-prevblk";
const ABH_PREV_CL: &str = "validation.cpp:AcceptBlockHeader#5:bad-prevblk-chainlock";
const ABH_FAILED_ANCESTOR: &str = "validation.cpp:AcceptBlockHeader#6:bad-prevblk";
const ABH_CL: &str = "validation.cpp:AcceptBlockHeader#7:bad-chainlock";
const TBV_CL: &str = "validation.cpp:TestBlockValidity#1:bad-chainlock";
const CONN_CL: &str = "validation.cpp:ConnectBlock#1:bad-chainlock";
const CCBH_NEW: &str = "validation.cpp:ContextualCheckBlockHeader#5:time-too-new";

/// [`expect`] with the site its reply is credited to.
pub fn expect_named(w: &World, p: &Planned) -> (Expect, Named) {
    use mutate::Mode;
    let n = Named::None;
    if w.down {
        return match p.op {
            ChainOp::SetTime(_) => (Expect::Ok, n),
            // Unchecked (D11): a load after a refused one.
            ChainOp::Restart(_) => (Expect::Any, n),
            _ => (Expect::Down, n),
        };
    }
    // More than `MAX_FUTURE` past the adjusted time (the mock clock, with no peers).
    let too_new = |id: usize| w.too_new(id);
    // The block's fault: its operator, unless that was `time_too_new` and the clock has
    // since caught up with it (`too_new` judges the time itself); on S2, a line defect whose
    // scripts Core checks.
    let operator = |id: usize| {
        w.blocks[id]
            .mutation
            .map(chain_operator)
            .filter(|op| op.name != "time_too_new" || too_new(id))
            .or_else(|| w.line.and_then(|l| l.fault(w, id)))
    };
    // A parent Core may hold conflict-marked ([`cl_mark_maybe`], the lock's geometry or a dropped
    // lock's) names #5, never #4, though the model does not know whether Core marked it: naming
    // only credits a reply that carries the site's reason whole (`census::carries`, OD-L3-155),
    // and `bad-prevblk-chainlock` is #5's
    // alone (v:4654-4657; pinned by the inventory test
    // `bad_prevblk_chainlock_has_one_site`), so a `bad-prevblk` reply credits nothing.
    // Why `AcceptBlockHeader` refuses a new header under `parent` before its contextual checks,
    // in its order: the parent marked failed (#4), marked conflicting (#5), or unmarked with a
    // failed ancestor (#6). A parent the model holds failed only under its own INVALIDATE of an
    // ancestor may be either #4 or #6 in Core: named neither.
    let prev_site = |parent: usize| -> Named {
        if cl_mark_maybe(w, parent) {
            Named::Site("bad-prevblk-chainlock", ABH_PREV_CL)
        } else if w.blocks[parent].invalid && w.under_own_invalidate(parent) {
            Named::None
        } else if w.blocks[parent].invalid {
            Named::Site("bad-prevblk", ABH_BAD_PREV)
        } else if w.ancestry(parent).skip(1).any(|a| w.blocks[a].invalid) {
            Named::Site("bad-prevblk", ABH_FAILED_ANCESTOR)
        } else {
            Named::None
        }
    };
    // A new header's checks after its proof of work and its parent's lookup, in
    // `AcceptBlockHeader`'s order: `None` when they pass.
    let new_header = |id: usize, parent: usize, mode: Mode| -> Option<(Expect, Named)> {
        if !w.presumed_valid(parent) || cl_mark_maybe(w, parent) {
            // `bad-prevblk`, `bad-prevblk-chainlock`, or a parent Core has not judged.
            return Some((Expect::Any, prev_site(parent)));
        }
        // `ContextualCheckBlockHeader` checks the version after the time.
        let before_time = |op: &Operator| op.rejects != ["bad-version"] || !too_new(id);
        match operator(id) {
            Some(op) if op.stage == Stage::Header && before_time(op) => {
                Some((Expect::Reject(op.rejects), Named::Op(op, mode)))
            }
            _ if too_new(id) => {
                Some((Expect::Reject(&["time-too-new"]), Named::Site("time-too-new", CCBH_NEW)))
            }
            _ if cl_conflict_live(w, id) => {
                Some((Expect::Reject(&["bad-chainlock"]), Named::Site("bad-chainlock", ABH_CL)))
            }
            _ => None,
        }
    };
    // `submitblock`: a known block is a duplicate; then `CheckBlock` (context-free), the header
    // if it is new, `ContextualCheckBlock` and, if the block connects, `ConnectBlock`.
    let block = |id: usize| -> (Expect, Named) {
        let b = &w.blocks[id];
        let Some(parent) = b.parent else {
            return (Expect::Any, n);
        };
        // `duplicate` (`v:4627-4630`): a block with data, or one Core may hold marked.
        if b.held == Held::Data || b.invalid || (b.indexed() && cl_dropped_conflict(w, id)) {
            return (Expect::Any, n);
        }
        // A line defect under a best header the model does not know.
        if w.line.is_some_and(|l| l.defects.contains(&id) && l.skips(w, id).is_none()) {
            return (Expect::Any, n);
        }
        let op = operator(id);
        if let Some(op) = op.filter(|op| matches!(op.stage, Stage::Pow | Stage::Check)) {
            return (Expect::Reject(op.rejects), Named::Op(op, Mode::Submit));
        }
        if !b.indexed() {
            if !w.blocks[parent].indexed() {
                return (
                    Expect::Reject(&["prev-blk-not-found"]),
                    Named::Site("prev-blk-not-found", ABH_NO_PREV),
                );
            }
            if let Some(e) = new_header(id, parent, Mode::Submit) {
                return e;
            }
        }
        if !w.presumed_valid(parent) || w.ancestry(parent).any(|a| cl_mark_maybe(w, a)) {
            return (Expect::Any, n);
        }
        match op {
            Some(op) if op.stage == Stage::Contextual => {
                (Expect::Reject(op.rejects), Named::Op(op, Mode::Submit))
            }
            // A header known before the lock skipped its check: ConnectBlock checks it first.
            _ if cl_conflict_live(w, id) && connects(w, id) => {
                (Expect::Reject(&["bad-chainlock"]), Named::Site("bad-chainlock", CONN_CL))
            }
            None if connects(w, id) => (Expect::Ok, n),
            None => (Expect::Stored, n),
            // Found only by ConnectBlock: a block Core only stores is accepted.
            Some(op) if op.stage == Stage::Connect && !connects(w, id) => (Expect::Stored, n),
            Some(op) => (Expect::Reject(op.rejects), Named::Op(op, Mode::Submit)),
        }
    };
    match &p.what {
        What::Block(id) => block(*id),
        What::Header(id) => {
            let b = &w.blocks[*id];
            let Some(parent) = b.parent else {
                return (Expect::Any, n);
            };
            if !w.blocks[parent].indexed() {
                return (Expect::Reject(&["Must submit previous header"]), n);
            }
            if b.indexed() {
                // A known header: `duplicate` if it is marked failed (#1) or conflicting (#2).
                let named = if b.invalid {
                    Named::Site("duplicate", ABH_DUPLICATE)
                } else if cl_conflict(w, *id) {
                    Named::Site("duplicate", ABH_DUPLICATE_CL)
                } else {
                    n
                };
                return (Expect::Any, named);
            }
            // A conflict mark above the lock's height outlives a reconsider (`bad-prevblk-chainlock`).
            if w.ancestry(parent).any(|a| cl_mark_maybe(w, a)) {
                // Named only where the parent itself may carry the mark: one Core carried above
                // the lock's height is not the model's to place.
                let named = if cl_mark_maybe(w, parent) {
                    Named::Site("bad-prevblk-chainlock", ABH_PREV_CL)
                } else {
                    n
                };
                return (Expect::Any, named);
            }
            match operator(*id) {
                Some(op) if op.stage == Stage::Pow => {
                    (Expect::Reject(op.rejects), Named::Op(op, Mode::Header))
                }
                _ => new_header(*id, parent, Mode::Header).unwrap_or((Expect::Ok, n)),
            }
        }
        What::Proposal {
            id,
            again,
        } => {
            // TestBlockValidity on the tip: the ChainLock first, then the contextual header
            // checks (no proof of work), CheckBlock, ContextualCheckBlock and ConnectBlock, so
            // every operator's reason shows.
            if w.blocks[*id].parent != Some(w.tip) || *again {
                return (Expect::Any, n);
            }
            if cl_conflict_live(w, *id) {
                return (Expect::Reject(&["bad-chainlock"]), Named::Site("bad-chainlock", TBV_CL));
            }
            match (operator(*id), too_new(*id)) {
                (None, false) => (Expect::Ok, n),
                (None, true) => {
                    (Expect::Reject(&["time-too-new"]), Named::Site("time-too-new", CCBH_NEW))
                }
                (Some(op), true) if matches!(op.stage, Stage::Pow | Stage::Header) => {
                    (Expect::Any, n)
                }
                (Some(_), true) => {
                    (Expect::Reject(&["time-too-new"]), Named::Site("time-too-new", CCBH_NEW))
                }
                (Some(op), false) => (Expect::Reject(op.rejects), Named::Op(op, Mode::Proposal)),
            }
        }
        What::Target(Some(t)) if w.blocks[*t].indexed() => (Expect::Ok, n),
        What::Target(_) => (Expect::Reject(&["Block not found"]), n),
        What::Ok => (Expect::Ok, n),
        What::Op => (
            match &p.op {
                // `ProcessNewChainLock`: a lock recorded since the load is `seen`, one at or below
                // the best recorded height `stale` (the shim's signatures are all valid), after its
                // `CheckActiveState` (`h.cpp:136-150`), whose off → on edge resets that height but
                // not the seen set. Core's seen set is pruned once it grows past `CL_SEEN_KEPT`
                // entries.
                ChainOp::ClRecord {
                    ..
                } if w.cl_seen.len() >= CL_SEEN_KEPT => Expect::Any,
                ChainOp::ClRecord {
                    height,
                    hash,
                    ..
                } if w.cl_seen.contains(&(*height, *hash)) => Expect::Reject(&["seen"]),
                ChainOp::ClRecord {
                    height,
                    ..
                } if *height <= w.cl_after_check().1 => Expect::Reject(&["stale"]),
                // The load refuses a chainstate whose tip is more than `MAX_FUTURE` ahead of the
                // clock (`VerifyLoadedChainstate`): a clean restart's tip is the live one, a
                // KILL's the last one written.
                ChainOp::Restart(kind) => {
                    let tip = match kind {
                        RestartKind::Clean => w.tip,
                        RestartKind::Kill => w.coins_tip,
                    };
                    if w.too_new(tip) {
                        Expect::Reject(&[
                            "The block database contains a block which appears to be from the future",
                        ])
                    } else {
                        Expect::Ok
                    }
                }
                _ => Expect::Ok,
            },
            n,
        ),
        What::Any => (Expect::Any, n),
    }
}

/// The census name of an enforcement the tie model refuses after a record.
const UNENFORCED: &str = "cl_unenforced";

/// The ChainLock handler's seen set: an insert at this size prunes it
/// (`SEEN_CHAINLOCKS_MAX_SIZE`).
pub(crate) const CL_SEEN_KEPT: usize = 2048;

/// Whether enforcing the lock on `cl` would clear failures under it: `EnforceBlock` resets the
/// failure flags of a locked block that is not valid to its transactions (a header-only entry),
/// and with them those of its marked descendants.
fn resets_failures(w: &World, cl: usize) -> bool {
    w.blocks[cl].held != Held::Data && w.subtree(cl).into_iter().any(|i| w.blocks[i].invalid)
}

/// Why the generator leaves an enforcement out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refused {
    /// The lock's block is no longer presumed valid (the input's own INVALIDATE below it:
    /// enforcing would reconsider the branch), or enforcing would clear failures under it
    /// ([`resets_failures`]): counted `cl_on_invalid`.
    OnInvalid,
    /// The tie model finds it structural (G-T3).
    Structural,
    /// The tie model finds it risky (G-T2).
    Risky,
}

/// CL_ENFORCE of the lock on `cl`, unless it is [`Refused`].
fn enforce(w: &World, cl: Option<usize>) -> Result<Planned, Refused> {
    // Spork 19 off: the op's `CheckActiveState` disables the handler, and `EnforceBestChainLock`
    // returns at once (`h.cpp:273-277`).
    if !w.spork19_active() {
        return Ok(Planned::new(ChainOp::ClEnforce, What::Ok));
    }
    if cl.is_some_and(|c| !w.presumed_valid(c) || resets_failures(w, c)) {
        return Err(Refused::OnInvalid);
    }
    if ties::enforce_walks_siblings(w, cl) {
        return Err(Refused::Structural);
    }
    if ties::enforce_from(w, cl).is_some_and(|h| ties::risky_from(w, h)) {
        return Err(Refused::Risky);
    }
    Ok(Planned::new(ChainOp::ClEnforce, What::Ok))
}

/// [`enforce`], a refusal counted under `cl_on_invalid`, `structural` or `risky`.
fn enforce_counted(
    w: &World,
    cl: Option<usize>,
    census: &mut Census,
    structural: &'static str,
    risky: &'static str,
) -> Option<Planned> {
    enforce(w, cl)
        .inspect_err(|r| {
            census.avoided(match r {
                Refused::OnInvalid => "cl_on_invalid",
                Refused::Structural => structural,
                Refused::Risky => risky,
            })
        })
        .ok()
}

/// The generator enforces the best recorded ChainLock as soon as its block is known, before
/// any later tree- or state-changing op (Core schedules enforcement; the window between them
/// is outside 3a's scope). The runner asks after every op: `None` when nothing is due,
/// `Some(None)` when the tie model refuses the enforcement (`avoided: cl_unenforced`), which
/// ends the input.
///
/// After spork 19 was off, the timer's enforcement is due too ([`World::cl_window_due`]),
/// counted `cl_timer_enforce`.
pub fn due_enforce(w: &World, census: &mut Census) -> Option<Option<Planned>> {
    let lock = w.cl_after_check().0;
    if w.cl_due() {
        return Some(enforce_counted(w, lock, census, UNENFORCED, UNENFORCED));
    }
    if w.cl_window_due() {
        census.row("cl_timer_enforce");
        return Some(enforce_counted(w, lock, census, UNENFORCED, UNENFORCED));
    }
    None
}

/// The generator's per-input state.
pub struct Gen {
    pub blocks_left: u32,
    /// The operator the next built block gets.
    armed: Option<usize>,
    raw_used: bool,
    /// Blocks this input built, oldest first.
    pub built: Vec<usize>,
    /// D34's experiment has admitted spork intents (O2b-4).
    pub spork_admitted: bool,
}

/// Whether D34's spork experiment has passed (O2b-4 4f, its twelve assertions): it has, so
/// `chain-v2` admits spork intents ([`Gen::for_encoding`]), which the world model follows
/// (spork 19 and the ChainLock handler's state, 4f2).
pub const SPORKS_ADMITTED: bool = true;

impl Gen {
    /// A generator for programs in `encoding`: spork intents are admitted for `chain-v2` only, once
    /// [`SPORKS_ADMITTED`] (Q11: legacy `diff_chain` resolves as before).
    pub fn for_encoding(encoding: Encoding) -> Gen {
        Gen {
            spork_admitted: SPORKS_ADMITTED && encoding == Encoding::V2,
            ..Gen::default()
        }
    }
}

impl Default for Gen {
    fn default() -> Gen {
        Gen {
            blocks_left: MAX_BLOCKS,
            armed: None,
            raw_used: false,
            built: Vec::new(),
            spork_admitted: false,
        }
    }
}

/// The [`OPERATORS`] index a `Mutate { op }` arms: `op` modulo the catalogue's length.
pub fn mutate_operator(op: u8) -> usize {
    usize::from(op) % OPERATORS.len()
}

/// The [`OPERATORS`] index a `Propose`'s `mutate` byte applies: none for 0, else `mutate - 1`
/// modulo the catalogue's length.
pub fn propose_operator(mutate: u8) -> Option<usize> {
    (mutate > 0).then(|| (usize::from(mutate) - 1) % OPERATORS.len())
}

/// What a mutating intent selects (O2b-4 §3a.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    /// A chain operator index ([`chain_operator`]).
    Operator(usize),
    /// A `Propose` with no operator: a plain block.
    Plain,
    /// A `chain-v2` bank slot the bank does not fill: the intent is dropped (`unknown_slot`).
    UnknownSlot,
}

/// G-ENC-4's append property over `inputs` and 10,000 fixed-PRNG `chain-v2` inputs: for each k,
/// the bank's first k + 1 builders change an intent's selection from the first k's only where its
/// slot was unknown before, to the new builder, and do change some (k up to the bank's length,
/// plus one appended builder past it). The first failure, if any.
pub fn g_enc4_append_only(mut inputs: Vec<Vec<u8>>) -> Result<(), String> {
    let copy = |o: &Operator| Operator {
        name: o.name,
        rejects: o.rejects,
        stage: o.stage,
        par: o.par,
        apply: o.apply,
        variants: o.variants,
    };
    let mut grown: Vec<Operator> = BANK.iter().map(copy).collect();
    grown.push(Operator {
        name: "test.appended",
        ..copy(&OPERATORS[0])
    });
    let mut x: u64 = 0x4f32_6234;
    let mut next = move || {
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    for _ in 0..10_000 {
        let len = 2 + (next() % 399) as usize;
        inputs.push((0..len).map(|_| next() as u8).collect());
    }
    let programs: Vec<intent::Program> =
        inputs.iter().map(|b| intent::Program::decode_v2(b)).collect();
    for k in 0..grown.len() {
        let (bank, next) = (&grown[..k], &grown[..k + 1]);
        let (mut changed, mut unknown) = (0, 0);
        for intent in programs.iter().flat_map(|p| &p.intents) {
            let before = selection(intent, bank);
            unknown += usize::from(before == Some(Selection::UnknownSlot));
            let after = selection(intent, next);
            if before != after {
                if before != Some(Selection::UnknownSlot)
                    || after != Some(Selection::Operator(OPERATORS.len() + k))
                {
                    return Err(format!("k {k}: {intent:?} went from {before:?} to {after:?}"));
                }
                changed += 1;
            }
        }
        if changed == 0 || unknown <= changed {
            return Err(format!("k {k}: changed {changed} of {unknown} unknown"));
        }
    }
    Ok(())
}

/// What `intent` selects with `bank` as `chain-v2`'s bank (the generator's is [`BANK`]); `None`
/// for an intent that selects no operator.
pub fn selection(intent: &Intent, bank: &[Operator]) -> Option<Selection> {
    let bank_slot =
        |slot: u8| bank_select(slot, bank).map_or(Selection::UnknownSlot, Selection::Operator);
    Some(match *intent {
        Intent::Mutate {
            op,
        } => Selection::Operator(mutate_operator(op)),
        Intent::Propose {
            mutate,
            ..
        } => propose_operator(mutate).map_or(Selection::Plain, Selection::Operator),
        Intent::MutateX {
            slot,
        }
        | Intent::ProposeX {
            slot,
            ..
        } => bank_slot(slot),
        _ => return None,
    })
}

/// Whether a block carrying `op` may be stored and then fail at connect: a Connect-stage operator
/// Core rejects. A valid-edge operator connects; Check and Pow stages are refused on arrival, and
/// Header and Contextual ones (`time_too_new` too, which `expect` re-judges by the clock) leave the
/// header failed with no data, so Core never tries the branch.
pub(crate) fn may_fail_at_connect(op: &Operator) -> bool {
    op.stage == Stage::Connect && !op.rejects.is_empty()
}

/// S2 (DEC-1368): whether `out`'s ops could make Core reorganise away from a connected line
/// defect whose scripts it would now check, onto a branch that may fail to connect. Core then
/// retries through `FindMostWorkChain` after `ActivateBestChainStep`'s invalid-found exit, and
/// the old chain back over the defect fails too; `PruneBlockIndexCandidates` has already dropped
/// the entries below, so the tip is pruned from the set of blocks Core may activate (a known Core
/// state).
///
/// A defect's skip has lapsed when Core's best header has left the line (a failure's reset, or a
/// load). An INVALIDATE of an ancestor of the best header resets it within its own op
/// (`InvalidChainFound`, before its `ActivateBestChain`), so for that op every connected defect
/// below its target counts as lapsing, as it does when the best header is unknown. A branch may
/// fail when one of its blocks carries a Connect-stage operator that Core rejects. A branch with
/// no such block, or one forking above every connected defect Core would check, is let through.
///
/// RESTART and the run loop's CL_ENFORCE ops never reach this check, and need not: a ChainLock
/// lands only on presumed-valid ancestry, so never above a mutated block, and a load retries a
/// failed branch only when a KILL lost its failure mark, which, flushes being ordered, also loses
/// any later lapse.
// Core: src/validation.cpp:3530-3592 at v24.0.0
// Core: src/validation.cpp:3785 at v24.0.0
// Core: src/validation.cpp:3657-3667 at v24.0.0
// Core: src/validation.cpp:3594-3603 at v24.0.0
// Core: src/validation.cpp:2054-2055 at v24.0.0
// Core: src/validation.cpp:4045 at v24.0.0
fn av_reconnect(w: &World, out: &[Planned]) -> bool {
    let Some(line) = w.line else {
        return false;
    };
    let connected: Vec<usize> = line
        .defects
        .iter()
        .copied()
        .filter(|&d| {
            let b = &w.blocks[d];
            b.held == Held::Data && !b.invalid && w.is_ancestor(d, w.tip)
        })
        .collect();
    // The highest connected defect below `limit` that Core would check: its skip lapsed, or
    // (`lapsing`) any. A branch forking below it disconnects it.
    let high_below = |limit: u32, lapsing: bool| {
        connected
            .iter()
            .filter(|&&d| w.blocks[d].height < limit && (lapsing || line.skips(w, d) != Some(true)))
            .map(|&d| w.blocks[d].height)
            .max()
    };
    let planned: BTreeSet<usize> = out
        .iter()
        .filter_map(|p| match p.what {
            What::Block(id) => Some(id),
            _ => None,
        })
        .collect();
    let data = |b: usize| w.blocks[b].held == Held::Data || planned.contains(&b);
    let fails =
        |b: usize| w.blocks[b].mutation.map(chain_operator).is_some_and(may_fail_at_connect);
    // A branch to `top` (its blocks' marks cleared when `reset`) that forks below height `high`,
    // that Core can activate, and in which a block may fail at connect.
    let risky = |top: usize, reset: bool, high: u32| {
        let Some(fork) = w.ancestry(top).find(|&a| w.is_ancestor(a, w.tip)) else {
            return false;
        };
        if w.blocks[fork].height >= high {
            return false;
        }
        let path: Vec<usize> = w.ancestry(top).take_while(|&a| a != fork).collect();
        path.iter().all(|&b| data(b) && (reset || !w.blocks[b].invalid))
            && path.iter().any(|&b| fails(b))
    };
    // `from` and the blocks above it Core would hold data for once `out` runs.
    let tops = |from: usize, reset: bool| -> Vec<usize> {
        w.subtree(from)
            .into_iter()
            .filter(|&b| b == from || (data(b) && (reset || !w.blocks[b].invalid)))
            .collect()
    };
    let tip = w.blocks[w.tip].height;
    out.iter().any(|p| match (&p.op, &p.what) {
        (ChainOp::Block(_), What::Block(x)) => high_below(u32::MAX, false).is_some_and(|high| {
            tops(*x, false).into_iter().any(|t| w.blocks[t].height > tip && risky(t, false, high))
        }),
        (ChainOp::Precious(_), What::Target(Some(t))) => high_below(u32::MAX, false)
            .is_some_and(|high| w.blocks[*t].height >= tip && risky(*t, false, high)),
        (
            ChainOp::Reconsider {
                ..
            },
            What::Target(Some(t)),
        ) => high_below(u32::MAX, false).is_some_and(|high| {
            tops(*t, true).into_iter().any(|b| w.blocks[b].height > tip && risky(b, true, high))
        }),
        // An active block: the tip falls to its parent, and any branch with more work than that
        // is tried. Only the defects below the target stay connected.
        (ChainOp::Invalidate(_), What::Target(Some(t))) if w.is_ancestor(*t, w.tip) => {
            let height = w.blocks[*t].height;
            let resets = w.best_header.is_none_or(|best| w.is_ancestor(*t, best));
            high_below(height, resets).is_some_and(|high| {
                (0..w.blocks.len()).any(|b| {
                    data(b)
                        && !w.blocks[b].invalid
                        && w.blocks[b].height >= height
                        && !w.is_ancestor(*t, b)
                        && risky(b, false, high)
                })
            })
        }
        _ => false,
    })
}

/// An extension's block count: 1..=8, `n` itself when it is in range (0 is 8).
fn count(n: u8) -> u8 {
    n.wrapping_sub(1) % 8 + 1
}

pub(crate) fn mtp(w: &World, parent: usize) -> u32 {
    let mut times: Vec<u32> = w.ancestry(parent).take(11).map(|i| w.blocks[i].time).collect();
    times.sort_unstable();
    times[times.len() / 2]
}

impl Gen {
    /// The ops of one intent. Blocks it builds are registered in `w` (held [`Held::Nothing`]);
    /// what Core makes of them is folded in when they run. On S2, an intent whose ops could
    /// make Core reconnect a line defect it would now check is dropped (`av_reconnect`). A
    /// dropped intent's blocks stay built, unsent (`Held::Nothing`, in `built` and on
    /// `w.branches`, so a later `deliver` or `unsent#k` is checked again), with their share of
    /// `blocks_left` spent and the armed operator taken; its ops never set the clock.
    pub fn resolve(&mut self, w: &mut World, intent: &Intent, census: &mut Census) -> Vec<Planned> {
        let out = self.intent_ops(w, intent, census);
        if av_reconnect(w, &out) {
            debug_assert!(
                !out.iter().any(|p| matches!(p.op, ChainOp::SetTime(_))),
                "a dropped intent's SETTIME would leave the model's clock ahead of both sides'"
            );
            census.avoided("av_reconnect");
            return Vec::new();
        }
        out
    }

    fn intent_ops(&mut self, w: &mut World, intent: &Intent, census: &mut Census) -> Vec<Planned> {
        let mut out = Vec::new();
        match intent {
            Intent::Extend {
                branch,
                n,
                flags,
            } => {
                let (slot, tip) = self.branch(w, *branch);
                if let Some(last) = self.chain(w, tip, count(*n), *flags, true, &mut out) {
                    match slot {
                        Some(slot) => w.branches[slot] = last,
                        // A held extension of the active chain is a branch to deliver later.
                        None if flags & HOLD != 0 => w.branches.push(last),
                        None => {}
                    }
                }
            }
            Intent::Fork {
                depth,
                n,
                flags,
            } => {
                let base = w.active_at_depth(u32::from(depth % MAX_FORK_DEPTH));
                if let Some(last) = self.chain(w, base, count(*n), *flags, false, &mut out) {
                    w.branches.push(last);
                }
            }
            Intent::Mutate {
                ..
            }
            | Intent::MutateX {
                ..
            } => match selection(intent, BANK) {
                Some(Selection::Operator(m)) => self.armed = Some(m),
                _ => census.avoided("unknown_slot"),
            },
            Intent::Propose {
                on,
                again,
                ..
            }
            | Intent::ProposeX {
                on,
                again,
                ..
            } => match selection(intent, BANK) {
                Some(Selection::Operator(m)) => self.propose(w, *on, Some(m), *again, &mut out),
                Some(Selection::Plain) => self.propose(w, *on, None, *again, &mut out),
                _ => census.avoided("unknown_slot"),
            },
            Intent::Invalidate(r) => {
                self.target_op(w, *r, "invalidate", ChainOp::Invalidate, census, &mut out);
            }
            Intent::Precious(r) => {
                self.target_op(w, *r, "precious", ChainOp::Precious, census, &mut out);
            }
            Intent::Reconsider(r, ignore) => {
                // RECONSIDER after a consensus failure is outside 3a's scope: it only undoes the
                // input's own INVALIDATE (a block of it, or one above it).
                let undoes_own = |t: usize| w.ancestry(t).any(|a| w.own_invalidates.contains(&a));
                if let Some(t) = self.target(w, *r) {
                    if !undoes_own(t) {
                        census.avoided("reconsider_not_own");
                        return out;
                    }
                    if w.consensus_failure {
                        census.avoided("reconsider_after_failure");
                        return out;
                    }
                    // Rule 2 scope (DEC-256).
                    if w.reconsider_reaches_stored_under_failed(t) {
                        census.avoided("reconsider_scope");
                        return out;
                    }
                    // Keeping a conflict mark is read from the lock's geometry, which no longer
                    // tells Core's marks apart (DESIGN §4.2).
                    if !*ignore
                        && w.cl_flags_unsure
                        && w.subtree(t)
                            .into_iter()
                            .chain(w.ancestry(t))
                            .any(|a| w.blocks[a].invalid)
                    {
                        census.avoided("cl_flag_unsure");
                        return out;
                    }
                    // Ignoring ChainLocks, `ResetBlockFailureFlags` still skips a subtree block
                    // that passes `IsValid()` (`v:4190`): one with its data keeps a conflict
                    // mark. `MarkConflictingBlock` marks a sibling of the lock's chain and the
                    // active blocks it disconnects above it (`v:4124-4132`), so a data block
                    // may hold one if it is `invalid` or its ancestry conflicts with the best
                    // lock or a dropped one; the model tells none of them apart (DESIGN §4.2).
                    if *ignore
                        && w.cl_flags_unsure
                        && w.subtree(t).into_iter().any(|a| {
                            a != t
                                && w.blocks[a].held == Held::Data
                                && (w.blocks[a].invalid
                                    || w.ancestry(a).any(|x| cl_mark_maybe(w, x)))
                        })
                    {
                        census.avoided("cl_flag_unsure_ignore");
                        return out;
                    }
                }
                let make = |hash| ChainOp::Reconsider {
                    hash,
                    ignore_chainlocks: *ignore,
                };
                self.target_op(w, *r, "reconsider", make, census, &mut out);
            }
            Intent::Cl(r) => self.cl_record(w, *r, census, &mut out),
            Intent::ClEnforce => {
                out.extend(enforce_counted(
                    w,
                    w.cl_after_check().0,
                    census,
                    "cl_enforce_site13",
                    "cl_enforce",
                ));
            }
            Intent::Spork {
                on,
            } => {
                if self.spork_admitted {
                    let value = if *on {
                        0
                    } else {
                        SPORK_OFF
                    };
                    out.push(Planned::new(
                        ChainOp::Spork {
                            id: SPORK_19,
                            value,
                        },
                        What::Ok,
                    ));
                } else {
                    census.avoided("spork_not_admitted");
                }
            }
            Intent::Flush(m) => {
                let mode = [FlushMode::Always, FlushMode::IfNeeded, FlushMode::Periodic]
                    [usize::from(m % 3)];
                out.push(Planned::new(
                    ChainOp::Flush {
                        mode,
                        label: String::new(),
                    },
                    What::Ok,
                ));
            }
            Intent::Restart(k) => self.restart(w, *k, census, &mut out),
            Intent::Time(n) => {
                out.push(Planned::new(
                    ChainOp::SetTime(w.mocktime + (i64::from(*n) + 1) * 120),
                    What::Ok,
                ));
            }
            Intent::Warp(n) => {
                let time = (w.mocktime - (i64::from(*n) + 1) * 120).max(1);
                out.push(Planned::new(ChainOp::SetTime(time), What::Ok));
            }
            Intent::Raw(bytes) => {
                if !self.raw_used {
                    self.raw_used = true;
                    out.push(Planned::new(ChainOp::Block(bytes.clone()), What::Any));
                }
            }
            Intent::Activate => out.push(Planned::new(ChainOp::Activate, What::Any)),
            Intent::Deliver {
                branch,
            } => {
                let (_, tip) = self.branch(w, branch & !DELIVER_REVERSED);
                let mut held = w.undelivered(tip);
                if branch & DELIVER_REVERSED != 0 {
                    held.reverse();
                }
                for id in held {
                    out.push(Planned::new(
                        ChainOp::Block(w.blocks[id].built_bytes()),
                        What::Block(id),
                    ));
                }
            }
        }
        out
    }

    /// A proposal of a new block on `on`'s block (with chain operator `mutation`, if any), or with
    /// `again`, of `on`'s own bytes again.
    fn propose(
        &mut self,
        w: &mut World,
        on: Ref,
        mutation: Option<usize>,
        again: bool,
        out: &mut Vec<Planned>,
    ) {
        let target = self.target(w, on);
        if again {
            if let Some(t) = target
                && let Some(built) = &w.blocks[t].built
            {
                out.push(Planned::new(
                    ChainOp::Proposal(built.bytes.clone()),
                    What::Proposal {
                        id: t,
                        again: true,
                    },
                ));
            }
        } else if self.blocks_left > 0 {
            let parent = target.unwrap_or(w.tip);
            self.clock(w, parent, out);
            let id = self.block(w, parent, 0, mutation);
            out.push(Planned::new(
                ChainOp::Proposal(w.blocks[id].built_bytes()),
                What::Proposal {
                    id,
                    again: false,
                },
            ));
        }
    }

    /// Invalidate, precious or reconsider (`kind`, `make`) on `r`'s block, unless the tie model
    /// finds it risky. A reference to no block gets a hash no block has.
    fn target_op(
        &self,
        w: &World,
        r: Ref,
        kind: &'static str,
        make: impl FnOnce(Hash) -> ChainOp,
        census: &mut Census,
        out: &mut Vec<Planned>,
    ) {
        let target = self.target(w, r);
        let from = target.map_or(0, |t| w.blocks[t].height.saturating_sub(1));
        if target.is_some() && ties::risky_from(w, from) {
            census.avoided(kind);
            return;
        }
        let hash = target.map_or_else(|| random_hash(w), |t| w.blocks[t].hash);
        out.push(Planned::new(make(hash), What::Target(target)));
    }

    /// A ChainLock record on `r`'s block.
    fn cl_record(&self, w: &World, r: Ref, census: &mut Census, out: &mut Vec<Planned>) {
        let target = self.target(w, r);
        // The quorum signs only a block it accepted, and the shim checks no signature:
        // no lock on a block built invalid or known failed.
        if target.is_some_and(|t| !w.presumed_valid(t)) {
            census.avoided("cl_on_invalid");
            return;
        }
        // A lock on an indexed block is enforced right after its record ([`due_enforce`]): the
        // record is left out when that enforcement would be.
        let best = w.cl_after_check().0;
        let better = |t: usize| {
            w.blocks[t].indexed() && best.is_none_or(|b| w.blocks[t].height > w.blocks[b].height)
        };
        if let Some(t) = target.filter(|&t| better(t))
            && enforce_counted(w, Some(t), census, UNENFORCED, UNENFORCED).is_none()
        {
            return;
        }
        let (height, hash) = target.map_or_else(
            || (w.blocks[w.tip].height as i32 + 1, random_hash(w)),
            |t| (w.blocks[t].height as i32, w.blocks[t].hash),
        );
        out.push(Planned::new(
            ChainOp::ClRecord {
                height,
                hash,
                sig: [0; 96],
            },
            What::Op,
        ));
    }

    /// A restart (clean when `k` is even), deferred behind an extension of the active chain
    /// when the tie model asks for one (G-T1), or dropped when that does not fit.
    ///
    /// A KILL loses what was not written: when what the load reads has a tie, the extension (if
    /// the live chain needs one) is flushed before it.
    fn restart(&mut self, w: &mut World, k: u8, census: &mut Census, out: &mut Vec<Planned>) {
        let kind = if k & 1 == 0 {
            RestartKind::Clean
        } else {
            RestartKind::Kill
        };
        let flush = kind == RestartKind::Kill && ties::kill_deficit(w) > 0;
        let deficit = if kind == RestartKind::Clean || flush {
            ties::restart_deficit(w)
        } else {
            0
        };
        // An extension at or below the lock's height conflicts with it.
        let locked_above = w.spork19_active()
            && w.best_cl.is_some_and(|cl| w.blocks[cl].height > w.blocks[w.tip].height);
        if deficit > 0 || flush {
            if w.down || deficit > self.blocks_left || (deficit > 0 && locked_above) {
                census.avoided("restart_tie");
                return;
            }
            census.deferred_restart();
        }
        if deficit > 0 {
            // The extension is valid whatever is armed.
            let armed = self.armed.take();
            let tip = w.tip;
            self.chain(w, tip, deficit as u8, 0, true, out);
            self.armed = armed;
        }
        if flush {
            let op = ChainOp::Flush {
                mode: FlushMode::Always,
                label: String::new(),
            };
            out.push(Planned::new(op, What::Ok));
        }
        out.push(Planned::new(ChainOp::Restart(kind), What::Op));
    }

    /// Branch `b`: 0 is the active chain, `k` the `(k - 1)`-th fork (modulo the count).
    fn branch(&self, w: &World, b: u8) -> (Option<usize>, usize) {
        if b == 0 || w.branches.is_empty() {
            return (None, w.tip);
        }
        let slot = (usize::from(b) - 1) % w.branches.len();
        (Some(slot), w.branches[slot])
    }

    fn target(&self, w: &World, r: Ref) -> Option<usize> {
        let pick = |v: &[usize], k: u8| (!v.is_empty()).then(|| v[usize::from(k) % v.len()]);
        match r.kind() {
            // Never below the image's maturity window (no invalidation of genesis).
            RefKind::Active(k) => {
                let floor = w.blocks[w.image_tip].height - MATURITY;
                let h = w.blocks[w.tip].height.saturating_sub(u32::from(k)).max(floor);
                w.ancestor(w.tip, h)
            }
            RefKind::Built(k) => pick(&self.built, k).or(Some(w.tip)),
            RefKind::Branch(k) => pick(&w.branches, k).or(Some(w.tip)),
            RefKind::Unsent(k) => {
                let unsent: Vec<usize> = self
                    .built
                    .iter()
                    .copied()
                    .filter(|&i| w.blocks[i].held == Held::Nothing)
                    .collect();
                pick(&unsent, k)
            }
            RefKind::Random => None,
        }
    }

    /// `n` blocks on `tip`, sent as `flags` says; the last block, if any was built.
    ///
    /// An extension (`line`, not a fork) of a block that S2's line continues takes the line's
    /// next block, unless an operator is armed: the line is prebuilt, so only `flags`' sending
    /// applies to it (headers first, held), not its transactions or signal (the line spends and
    /// signals; O2b §2.5, OD-L3-96 Q1). An armed operator builds a sibling of the line's block,
    /// a fork. A line block already stored, or known failed, is not taken again: the extension
    /// builds on its parent as it would on S0.
    fn chain(
        &mut self,
        w: &mut World,
        tip: usize,
        n: u8,
        flags: u8,
        line: bool,
        out: &mut Vec<Planned>,
    ) -> Option<usize> {
        let mut ids = Vec::new();
        let mut parent = tip;
        for _ in 0..n {
            if self.blocks_left == 0 {
                break;
            }
            let mutation = self.armed.take();
            let next = w.line.and_then(|l| l.child(parent)).filter(|&c| {
                let b = &w.blocks[c];
                line && mutation.is_none() && b.held == Held::Header && !b.invalid
            });
            parent = match next {
                // Known to both sides as a header already: no clock it must wait for.
                Some(c) => {
                    self.blocks_left -= 1;
                    self.built.push(c);
                    c
                }
                None => {
                    self.clock(w, parent, out);
                    self.block(w, parent, flags, mutation)
                }
            };
            ids.push(parent);
        }
        let hold = flags & HOLD != 0;
        if flags & HEADERS_FIRST != 0 {
            for &id in &ids {
                let header = w.blocks[id].built.as_ref().expect("built").header;
                out.push(Planned::new(ChainOp::Header(header), What::Header(id)));
            }
        }
        if !hold {
            for &id in &ids {
                out.push(Planned::new(ChainOp::Block(w.blocks[id].built_bytes()), What::Block(id)));
            }
        }
        ids.last().copied()
    }

    /// Advance the clock when the next block's earliest time would be more than an hour
    /// ahead of it (blocks stay valid, `time-too-new` is an operator's).
    fn clock(&self, w: &mut World, parent: usize, out: &mut Vec<Planned>) {
        let earliest = i64::from(mtp(w, parent)) + 1;
        if earliest > w.mocktime + 3600 {
            w.mocktime = earliest;
            out.push(Planned::new(ChainOp::SetTime(earliest), What::Ok));
        }
    }

    /// Build and register one block on `parent`.
    fn block(&mut self, w: &mut World, parent: usize, flags: u8, mutation: Option<usize>) -> usize {
        self.blocks_left -= 1;
        let height = w.blocks[parent].height + 1;
        assert!(height < DIP3_HEIGHT, "a generated height reaches the raised DIP3 (O2b-3 P5)");
        let mtp = mtp(w, parent);
        let parent_time = w.blocks[parent].time;
        let clock = u32::try_from(w.mocktime).unwrap_or(u32::MAX);
        let time = (mtp + 1).max((parent_time + SPACING).min(clock));
        w.tag += 1;
        let tag = w.tag;
        let utxos = w.utxos(parent);
        // Coins too small to split (a branch of many spends) are left alone: no output goes
        // negative.
        let mut coins: Vec<Coin> = utxos
            .values()
            .filter(|c| c.mature_at(height) && c.value >= MIN_COIN)
            .copied()
            .collect();
        // Vary which coins a block spends.
        let rot = (tag as usize) % coins.len().max(1);
        coins.rotate_left(rot);
        let immature = utxos.values().find(|c| !c.mature_at(height)).copied();
        let (mut txs, used) = shaped_txs(intent::shape(flags), &coins, w.fan_out);
        txs.truncate(MAX_TXS - 1);
        let fees = fee_total(&txs, &utxos);
        txs.insert(0, build::coinbase(height, build::subsidy(height) + fees, tag));
        let version = VERSION_BITS_TOP
            | if flags & SIGNAL != 0 {
                1 << TESTDUMMY_BIT
            } else {
                0
            };
        let mut plan = BlockPlan {
            prev: w.blocks[parent].hash,
            version,
            time,
            bits: REGTEST_BITS,
            txs,
            merkle_override: None,
        };
        let rest: Vec<Coin> = coins.iter().skip(used).copied().collect();
        // The parent's transactions a builder can repeat into a BIP30 collision ([`bip30_priors`]).
        let prior: Vec<TxPlan> = match mutation.map(chain_operator) {
            Some(op) if mutate::NEEDS_PRIOR.contains(&op.name) => w.blocks[parent]
                .built
                .as_ref()
                .and_then(|b| BlockPlan::from_block(&b.bytes).ok())
                .map(|b| bip30_priors(&b.txs))
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        // Low on a short branch, too few coins are mature for an operator: the block is plain.
        // So is one whose operator repeats a parent transaction the parent has none of.
        let mutation = mutation.filter(|&m| {
            rest.len() >= mutate::COINS
                && (!mutate::NEEDS_PRIOR.contains(&chain_operator(m).name) || !prior.is_empty())
        });
        if let Some(m) = mutation {
            let cx = mutate::Cx {
                height,
                mtp,
                mocktime: w.mocktime,
                coins: &rest,
                immature,
                l3: None,
                prior: &prior,
            };
            (chain_operator(m).apply)(&mut plan, &cx);
        }
        let built = plan.build();
        let (mut creates, spends) = effects(&plan, &built, height);
        if mutation.is_some() {
            // A mutated block's outputs may be its fault: its descendants spend none of them,
            // so their replies stay their own.
            creates.clear();
        }
        let id = w.add(Block {
            hash: built.hash,
            parent: Some(parent),
            height,
            time: plan.time,
            built: Some(built),
            creates,
            spends,
            held: Held::Nothing,
            invalid: false,
            mutation,
            seq: Seq::Arrived,
            durable: None,
        });
        self.built.push(id);
        id
    }
}

/// The transactions of a block's `shape`, spending `coins` from the front, and how many coins
/// they take.
pub(crate) fn shaped_txs(shape: TxShape, coins: &[Coin], fan_out: usize) -> (Vec<TxPlan>, usize) {
    let to_true = |v: i64| (v, Lock::True.script_pubkey());
    let mut txs = Vec::new();
    let used = match shape {
        TxShape::None => 0,
        TxShape::Spends => {
            for c in coins.iter().take(2) {
                let each = (c.value - 1_000) / 2;
                txs.push(TxPlan::simple(
                    vec![Input::new(c.outpoint, Vec::new())],
                    vec![to_true(each), to_true(each)],
                ));
            }
            2
        }
        TxShape::FanOut => {
            if let Some(c) = coins.first() {
                let each = (c.value - 1_000) / fan_out as i64;
                txs.push(TxPlan::simple(
                    vec![Input::new(c.outpoint, Vec::new())],
                    (0..fan_out).map(|_| to_true(each)).collect(),
                ));
            }
            1
        }
        TxShape::Chain => {
            if let Some(c) = coins.first() {
                let mut prev = (c.outpoint, c.value);
                for _ in 0..3 {
                    let tx = TxPlan::simple(
                        vec![Input::new(prev.0, Vec::new())],
                        vec![to_true(prev.1 - 1_000)],
                    );
                    prev = ((tx.txid(), 0), prev.1 - 1_000);
                    txs.push(tx);
                }
            }
            1
        }
    };
    (txs, used)
}

/// The transactions of a block's `txs` (its coinbase first) that a child repeating one collides
/// with in ConnectBlock's BIP30 check, which looks only at outputs still in the coins view
/// (v24.0.0 validation.cpp:2608-2619): each but the coinbase with a spendable output, whose
/// spendable outputs no later transaction of the block spends. An unspendable output never
/// becomes a coin, so it neither qualifies a transaction nor stops one.
pub(crate) fn bip30_priors(txs: &[TxPlan]) -> Vec<TxPlan> {
    let spent: BTreeSet<([u8; 32], u32)> =
        txs.iter().flat_map(|t| t.inputs.iter().map(|i| i.prevout)).collect();
    txs.iter()
        .skip(1)
        .filter(|t| {
            let id = t.txid();
            let live: Vec<u32> = (0..t.outputs.len() as u32)
                .filter(|&o| !dash_consensus::coins::is_unspendable(&t.outputs[o as usize].1))
                .collect();
            !live.is_empty() && live.iter().all(|&o| !spent.contains(&(id, o)))
        })
        .cloned()
        .collect()
}

/// What `txs` leave to the coinbase: their inputs less their outputs. An input spends a coin
/// of `utxos`, or an output of an earlier transaction of the block.
pub(crate) fn fee_total(txs: &[TxPlan], utxos: &BTreeMap<Outpoint, Coin>) -> i64 {
    txs.iter()
        .map(|t| {
            let out: i64 = t.outputs.iter().map(|o| o.0).sum();
            let input: i64 = t
                .inputs
                .iter()
                .map(|i| {
                    utxos.get(&i.prevout).map_or_else(|| txs_value(txs, i.prevout), |c| c.value)
                })
                .sum();
            input - out
        })
        .sum()
}

fn txs_value(txs: &[TxPlan], prevout: Outpoint) -> i64 {
    txs.iter()
        .find(|t| t.txid() == prevout.0)
        .and_then(|t| t.outputs.get(prevout.1 as usize))
        .map_or(0, |o| o.0)
}

/// The coins a built block creates (its positive `OP_TRUE` outputs) and the outpoints its
/// transactions spend (the coinbase spends none).
pub(crate) fn effects(plan: &BlockPlan, built: &Built, height: u32) -> (Vec<Coin>, Vec<Outpoint>) {
    let true_script = Lock::True.script_pubkey();
    let mut creates = Vec::new();
    let mut spends = Vec::new();
    for (t, tx) in plan.txs.iter().enumerate() {
        if t > 0 {
            spends.extend(tx.inputs.iter().map(|i| i.prevout));
        }
        for (o, (value, script)) in tx.outputs.iter().enumerate() {
            if *script == true_script && *value > 0 {
                creates.push(Coin {
                    outpoint: (built.txids[t], o as u32),
                    value: *value,
                    lock: Lock::True,
                    height,
                    coinbase: t == 0,
                });
            }
        }
    }
    (creates, spends)
}

fn random_hash(w: &World) -> Hash {
    let mut h = [0x77; 32];
    h[..8].copy_from_slice(&w.tag.to_le_bytes());
    h
}
#[cfg(test)]
mod tests {
    use dash_oracle_sys::ChainOp;

    use super::{
        ABH_BAD_PREV, ABH_CL, ABH_DUPLICATE, ABH_DUPLICATE_CL, ABH_FAILED_ANCESTOR, ABH_NO_PREV,
        ABH_PREV_CL, CONN_CL, Expect, Gen, Input, Lock, Planned, TBV_CL, TxPlan, TxShape, What,
        bip30_priors, expect, expect_named, shaped_txs,
    };
    use crate::chain::census::Census;
    use crate::chain::core::{SPORK_19, SPORK_OFF, T_RECENT, image_world};
    use crate::chain::intent::{Encoding, Intent, Ref};
    use crate::chain::mutate::Par;
    use crate::chain::world::{Block, Coin, Held, Seq, World};

    const TIME: u32 = 1_700_000_000;

    fn block(n: u8, parent: Option<usize>, height: u32, held: Held) -> Block {
        Block {
            hash: [n; 32],
            parent,
            height,
            time: TIME,
            built: None,
            creates: Vec::new(),
            spends: Vec::new(),
            held,
            invalid: false,
            mutation: None,
            seq: Seq::Loaded,
            durable: None,
        }
    }

    fn site(w: &World, what: What) -> Option<&'static str> {
        let (_, named) = expect_named(w, &Planned::new(ChainOp::Activate, what));
        named.site(Par::One).map(|(_, key)| key)
    }

    /// OD-L3-154: `bad-prevblk-chainlock` begins no reason in a10's inventory but #5's, so the
    /// model naming #5 on a parent Core only may hold conflict-marked credits no other site's
    /// reply (`Census::credit` matches a reply that carries the site's reason whole). An inventory
    /// with a second such emitter fails here, and the ruling is to be revisited.
    #[test]
    fn bad_prevblk_chainlock_has_one_site() {
        let inventory: serde_json::Value = serde_json::from_str(include_str!(
            "../../../contrib/node/l3-census/inventory-v24.0.0.json"
        ))
        .expect("the inventory");
        let reason = "bad-prevblk-chainlock";
        let sites: Vec<&str> = inventory["rows"]
            .as_array()
            .expect("its rows")
            .iter()
            .filter(|r| r["template"].as_str().is_some_and(|t| t.starts_with(reason)))
            .map(|r| r["key"].as_str().expect("a key"))
            .collect();
        assert_eq!(sites, [ABH_PREV_CL], "the sites whose reason begins {reason}");
        let scripts = inventory["script_errors"].as_array().expect("its script errors");
        assert!(scripts.iter().all(|e| !e.as_str().expect("an error").starts_with(reason)));
    }

    /// T10 (§4.2): each structural case names its own site, and only where the world fixes it.
    #[test]
    fn structural_sites_are_named_only_where_the_world_fixes_them() {
        // 0 ← 1 ← 2 ← 3 (the tip), with forks; `lock` and `stored` are siblings at height 4.
        let chain = (0..4u8)
            .map(|h| block(h, usize::from(h).checked_sub(1), u32::from(h), Held::Data))
            .collect();
        let mut w = World::from_chain(chain, i64::from(TIME));
        let add = |w: &mut World, n: u8, parent: usize, held: Held| {
            let height = w.blocks[parent].height + 1;
            w.add(block(n, Some(parent), height, held))
        };
        let lock = add(&mut w, 10, 3, Held::Header);
        let stored = add(&mut w, 11, 3, Held::Header);
        let unsent = add(&mut w, 12, 3, Held::Nothing);
        let orphan = add(&mut w, 13, unsent, Held::Nothing);
        let failed = add(&mut w, 14, 1, Held::Data);
        w.blocks[failed].invalid = true;
        let under_failed = add(&mut w, 15, failed, Held::Nothing);
        let unmarked = add(&mut w, 16, failed, Held::Data);
        let under_unmarked = add(&mut w, 17, unmarked, Held::Nothing);
        let conflicting = add(&mut w, 18, 2, Held::Data);
        let under_conflicting = add(&mut w, 19, conflicting, Held::Nothing);
        let at_lock_height = add(&mut w, 20, 2, Held::Nothing);
        // The input's INVALIDATE of `invalidated` marks its subtree in the model; Core marks only
        // `invalidated`, so a header on its header-only child is #4 or #6 there.
        let invalidated = add(&mut w, 21, 1, Held::Header);
        let below_invalidated = add(&mut w, 22, invalidated, Held::Header);
        let under_target = add(&mut w, 23, invalidated, Held::Nothing);
        let under_below = add(&mut w, 24, below_invalidated, Held::Nothing);
        for id in [invalidated, below_invalidated] {
            w.blocks[id].invalid = true;
        }
        w.own_invalidates.insert(invalidated);
        // `marked` and `failed_mark` are invalid; a reset dropped a lock on `lock` that `marked`
        // conflicts with (checked in a copy of the world), so only `marked` may carry Core's mark:
        // it names #5, never #4 (OD-L3-154).
        let marked = add(&mut w, 25, 1, Held::Data);
        let under_marked = add(&mut w, 26, marked, Held::Nothing);
        let failed_mark = add(&mut w, 27, 1, Held::Data);
        let under_failed_mark = add(&mut w, 28, failed_mark, Held::Nothing);
        for id in [marked, failed_mark] {
            w.blocks[id].invalid = true;
        }
        let mut dropped = w.clone();
        dropped.cl_dropped.push(lock);

        // No lock recorded: every fork is plain.
        assert_eq!(site(&w, What::Block(orphan)), Some(ABH_NO_PREV), "#29 unindexed parent");
        assert_eq!(site(&w, What::Header(orphan)), None, "Must submit… is not an inventory row");
        assert_eq!(site(&w, What::Block(under_failed)), Some(ABH_BAD_PREV), "#30 parent failed");
        assert_eq!(
            site(&w, What::Block(under_unmarked)),
            Some(ABH_FAILED_ANCESTOR),
            "#32 a failed ancestor, the parent unmarked"
        );
        assert_eq!(site(&w, What::Header(failed)), Some(ABH_DUPLICATE), "#27 indexed, failed");
        assert_eq!(
            site(&w, What::Block(under_target)),
            Some(ABH_BAD_PREV),
            "#30 the INVALIDATE's own target is marked"
        );
        assert_eq!(
            site(&w, What::Header(under_below)),
            None,
            "#30 or #32: a descendant of the INVALIDATE's target"
        );
        assert_eq!(site(&w, What::Block(under_below)), None, "#30 or #32, submitblock too");
        assert_eq!(
            site(&dropped, What::Header(under_marked)),
            Some(ABH_PREV_CL),
            "#31: Core may hold the parent conflict-marked"
        );
        assert_eq!(
            site(&dropped, What::Block(under_marked)),
            Some(ABH_PREV_CL),
            "#31, submitblock"
        );
        assert_eq!(
            site(&w, What::Header(under_failed_mark)),
            Some(ABH_BAD_PREV),
            "#30 the parent carries a failure"
        );
        assert_eq!(site(&w, What::Header(conflicting)), None, "no lock: an indexed fork");
        assert_eq!(site(&w, What::Header(at_lock_height)), None, "no lock: a new valid header");
        assert_eq!(
            site(
                &w,
                What::Proposal {
                    id: stored,
                    again: false
                }
            ),
            None,
            "no lock"
        );

        // A lock at height 4 on `lock`: every fork below it on another branch conflicts.
        w.best_cl = Some(lock);
        assert_eq!(
            site(&w, What::Block(under_conflicting)),
            Some(ABH_PREV_CL),
            "#31: the parent conflicts the lock"
        );
        assert_eq!(
            site(&w, What::Header(at_lock_height)),
            Some(ABH_CL),
            "#33 a header conflicting the lock at its height"
        );
        assert_eq!(
            site(&w, What::Header(conflicting)),
            Some(ABH_DUPLICATE_CL),
            "#28 indexed, conflicting"
        );
        assert_eq!(site(&w, What::Header(failed)), Some(ABH_DUPLICATE), "#27 failed comes first");
        assert_eq!(site(&w, What::Header(2)), None, "an indexed valid header names nothing");
        assert_eq!(
            site(
                &w,
                What::Proposal {
                    id: stored,
                    again: false
                }
            ),
            Some(TBV_CL),
            "#36 a proposal at a locked height"
        );
        assert_eq!(
            site(&w, What::Block(stored)),
            Some(CONN_CL),
            "#38 a stored block connected after its height's lock conflicts"
        );
        assert_eq!(site(&w, What::Block(3)), None, "a valid known block names nothing");
    }

    /// 4f2 test 17: once admitted, a spork intent resolves to a `SPORK` op for `chain-v2` only;
    /// `chain-v1` still counts it avoided (`spork_not_admitted`).
    #[test]
    fn sporks_resolve_only_for_chain_v2() {
        assert!(
            Gen::for_encoding(Encoding::V2).spork_admitted,
            "the spork experiment admitted sporks"
        );
        for (encoding, on, value) in [
            (Encoding::V1, false, None),
            (Encoding::V2, false, Some(SPORK_OFF)),
            (Encoding::V2, true, Some(0)),
        ] {
            let mut w = image_world();
            w.mocktime = T_RECENT;
            w.loaded();
            let mut census = Census::default();
            let ops = Gen::for_encoding(encoding).resolve(
                &mut w,
                &Intent::Spork {
                    on,
                },
                &mut census,
            );
            let ops: Vec<ChainOp> = ops.into_iter().map(|p| p.op).collect();
            match value {
                None => {
                    assert!(ops.is_empty(), "{encoding:?}: {ops:?}");
                    assert_eq!(census.avoided.get("spork_not_admitted"), Some(&1));
                }
                Some(value) => {
                    assert_eq!(
                        ops,
                        [ChainOp::Spork {
                            id: SPORK_19,
                            value
                        }],
                        "{encoding:?}"
                    );
                    assert!(census.avoided.is_empty(), "{:?}", census.avoided);
                }
            }
        }
    }

    /// 4f2 test 26: while the conflict marks are unsure, a reconsider that keeps them is avoided
    /// (`cl_flag_unsure`) over an invalid block. One that ignores ChainLocks still runs on the
    /// target alone, and is avoided (`cl_flag_unsure_ignore`) over a block below it with its data
    /// that Core may hold marked (`v:4190`: a block `MarkConflictingBlock` disconnected passes
    /// `IsValid()` and keeps its mark): one the model holds invalid, or one whose ancestry
    /// conflicts with a lock a reset dropped, which the model does not hold invalid (r2).
    #[test]
    fn unsure_marks_avoid_a_reconsider_that_keeps_them() {
        let mut w = image_world();
        w.mocktime = T_RECENT;
        w.loaded();
        let tip = w.tip;
        w.blocks[tip].invalid = true;
        w.own_invalidates.insert(tip);
        let block = |w: &World, n: u8, parent: usize, invalid: bool| {
            let mut hash = [n; 32];
            hash[..8].copy_from_slice(&(w.blocks.len() as u64).to_le_bytes());
            Block {
                hash,
                parent: Some(parent),
                height: w.blocks[parent].height + 1,
                time: w.blocks[parent].time + 150,
                built: None,
                creates: Vec::new(),
                spends: Vec::new(),
                held: Held::Data,
                invalid,
                mutation: None,
                seq: Seq::Arrived,
                durable: None,
            }
        };
        let mut marked = w.clone();
        marked.add(block(&w, 0xa6, tip, true));
        // A lock beside the tip, dropped by a reset: the tip conflicts with it, and the block
        // below the tip is not invalid in the model.
        let mut dropped = w.clone();
        let parent = w.blocks[tip].parent.expect("parent");
        let lock = dropped.add(block(&w, 0xa7, parent, false));
        dropped.cl_dropped.push(lock);
        let child = block(&dropped, 0xa8, tip, false);
        dropped.add(child);
        for (below, unsure, ignore, avoided) in [
            ("nothing", false, false, None),
            ("nothing", true, false, Some("cl_flag_unsure")),
            ("nothing", true, true, None),
            ("marked", false, true, None),
            ("marked", true, true, Some("cl_flag_unsure_ignore")),
            ("dropped", false, true, None),
            ("dropped", true, true, Some("cl_flag_unsure_ignore")),
        ] {
            let mut w = match below {
                "marked" => marked.clone(),
                "dropped" => dropped.clone(),
                _ => w.clone(),
            };
            w.cl_flags_unsure = unsure;
            let mut census = Census::default();
            let ops = Gen::for_encoding(Encoding::V2).resolve(
                &mut w,
                &Intent::Reconsider(Ref(0), ignore),
                &mut census,
            );
            let case = format!("below {below} unsure {unsure} ignore {ignore}");
            let arms: Vec<&str> = census
                .avoided
                .keys()
                .copied()
                .filter(|k| k.starts_with("cl_flag_unsure"))
                .collect();
            assert_eq!(arms, avoided.into_iter().collect::<Vec<_>>(), "{case}");
            assert_eq!(ops.is_empty(), avoided.is_some(), "{case}: {:?}", census.avoided);
        }
    }

    /// 4f2 test 31 (r2 finding 1): a lock a reset dropped still widens the readers of Core's
    /// conflict mark to `Any` (DESIGN §4.2): the duplicate read of an indexed block that
    /// conflicts with it and that the model does not hold invalid (`v:4627`), and a new
    /// header's ancestry read (`v:4654`). Without the dropped lock both answers are definite.
    #[test]
    fn a_dropped_lock_widens_the_mark_s_readers() {
        let mut w = image_world();
        w.mocktime = T_RECENT;
        w.loaded();
        let tip = w.tip;
        let below = w.blocks[tip].parent.expect("parent");
        let block = |w: &World, n: u8, parent: usize, held: Held| {
            let mut hash = [n; 32];
            hash[..8].copy_from_slice(&(w.blocks.len() as u64).to_le_bytes());
            Block {
                hash,
                parent: Some(parent),
                height: w.blocks[parent].height + 1,
                time: w.blocks[parent].time + 150,
                built: None,
                creates: Vec::new(),
                spends: Vec::new(),
                held,
                invalid: false,
                mutation: None,
                seq: Seq::Arrived,
                durable: None,
            }
        };
        // A lock beside the tip; an indexed header beside both; a header not yet sent on the tip.
        let lock = w.add(block(&w, 0xb1, below, Held::Data));
        let header = w.add(block(&w, 0xb2, below, Held::Header));
        let child = w.add(block(&w, 0xb3, tip, Held::Nothing));
        let mut dropped = w.clone();
        dropped.cl_dropped.push(lock);
        dropped.cl_flags_unsure = true;
        let duplicate = Planned::new(ChainOp::Block(Vec::new()), What::Block(header));
        let new_header = Planned::new(ChainOp::Header([0; 80]), What::Header(child));
        for p in [&duplicate, &new_header] {
            assert_ne!(expect(&w, p), Expect::Any, "{:?} without the dropped lock", p.what);
            assert_eq!(expect(&dropped, p), Expect::Any, "{:?}", p.what);
        }
    }

    /// OD-L3-171 (1): a BIP30 repeat takes only a parent transaction whose spendable outputs the
    /// parent leaves unspent. Under `tx=chain` that is the last of the three, as each spends the
    /// one before; under `tx=spends` both. An `OP_RETURN`-only one never qualifies, but one with
    /// a live output beside an `OP_RETURN` does.
    #[test]
    fn a_bip30_repeat_takes_a_transaction_the_parent_leaves_unspent() {
        let coins: Vec<Coin> = (1..=2u8)
            .map(|n| Coin {
                outpoint: ([n; 32], 0),
                value: 10_000_000,
                lock: Lock::True,
                height: 1,
                coinbase: true,
            })
            .collect();
        let coinbase = TxPlan::simple(Vec::new(), vec![(1, Lock::True.script_pubkey())]);
        let block = |txs: &[TxPlan]| [std::slice::from_ref(&coinbase), txs].concat();
        let (chain, _) = shaped_txs(TxShape::Chain, &coins, 32);
        assert_eq!(chain.len(), 3);
        assert_eq!(bip30_priors(&block(&chain)), [chain[2].clone()]);
        let (spends, _) = shaped_txs(TxShape::Spends, &coins, 32);
        assert_eq!(bip30_priors(&block(&spends)), spends);
        let op_return = vec![0x6a, 0x01, 0x00];
        let only_return = TxPlan::simple(
            vec![Input::new(coins[0].outpoint, Vec::new())],
            vec![(0, op_return.clone())],
        );
        let beside = TxPlan::simple(
            vec![Input::new(coins[1].outpoint, Vec::new())],
            vec![(0, op_return), (1_000, Lock::True.script_pubkey())],
        );
        assert_eq!(bip30_priors(&block(&[only_return, beside.clone()])), [beside]);
    }
}
