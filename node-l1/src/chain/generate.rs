//! Intents to ops (O2b §3.2–§3.4): the world model picks parents, coins and targets, the
//! builder makes the blocks, the tie model (`ties`) defers, retargets or drops what could tie,
//! and each op carries what the generator expects Core to reply (checked when it runs, against
//! the world as it then is: O2b-3 §2.5).

use std::collections::BTreeMap;

use dash_oracle_sys::{ChainOp, FlushMode, RestartKind};

use super::build::{
    self, BlockPlan, Built, Input, Lock, MATURITY, REGTEST_BITS, SPACING, TESTDUMMY_BIT, TxPlan,
    VERSION_BITS_TOP,
};
use super::census::Census;
use super::core::{DIP3_HEIGHT, SPORK_19, SPORK_OFF};
use super::intent::{
    self, DELIVER_REVERSED, HEADERS_FIRST, HOLD, Intent, Ref, RefKind, SIGNAL, TxShape,
};
use super::mutate::{self, OPERATORS, Operator, Stage};
use super::step::ReplyClass;
use super::ties;
use super::world::{Block, Coin, Hash, Held, Outpoint, Seq, World};

/// At most this many blocks built per input (O2b §6.3).
pub const MAX_BLOCKS: u32 = 64;
/// At most this many transactions per block (O2b §6.3).
pub const MAX_TXS: usize = 16;
/// The fan-out shape's outputs.
const FAN_OUT: usize = 32;
/// The smallest coin a block spends: every shape and operator leaves each output positive.
const MIN_COIN: i64 = 100_000;
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
    let Some(cl) = w.best_cl.filter(|&cl| w.blocks[cl].indexed()) else {
        return false;
    };
    let b = &w.blocks[id];
    b.height <= w.blocks[cl].height && w.ancestor(cl, b.height) != Some(id)
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
    if w.down {
        return match p.op {
            ChainOp::SetTime(_) => Expect::Ok,
            // Unchecked (D11): a load after a refused one.
            ChainOp::Restart(_) => Expect::Any,
            _ => Expect::Down,
        };
    }
    // More than `MAX_FUTURE` past the adjusted time (the mock clock, with no peers).
    let too_new = |id: usize| w.too_new(id);
    // The block's fault: its operator, unless that was `time_too_new` and the clock has
    // since caught up with it (`too_new` judges the time itself).
    let operator = |id: usize| {
        w.blocks[id]
            .mutation
            .map(|m| &OPERATORS[m])
            .filter(|op| op.name != "time_too_new" || too_new(id))
    };
    // A new header's checks after its proof of work and its parent's lookup, in
    // `AcceptBlockHeader`'s order: `None` when they pass.
    let new_header = |id: usize, parent: usize| -> Option<Expect> {
        if !w.presumed_valid(parent) || cl_conflict(w, parent) {
            // `bad-prevblk`, `bad-prevblk-chainlock`, or a parent Core has not judged.
            return Some(Expect::Any);
        }
        // `ContextualCheckBlockHeader` checks the version after the time.
        let before_time = |op: &Operator| op.rejects != ["bad-version"] || !too_new(id);
        match operator(id) {
            Some(op) if op.stage == Stage::Header && before_time(op) => {
                Some(Expect::Reject(op.rejects))
            }
            _ if too_new(id) => Some(Expect::Reject(&["time-too-new"])),
            _ if cl_conflict(w, id) => Some(Expect::Reject(&["bad-chainlock"])),
            _ => None,
        }
    };
    // `submitblock`: a known block is a duplicate; then `CheckBlock` (context-free), the header
    // if it is new, `ContextualCheckBlock` and, if the block connects, `ConnectBlock`.
    let block = |id: usize| -> Expect {
        let b = &w.blocks[id];
        let Some(parent) = b.parent else {
            return Expect::Any;
        };
        if b.held == Held::Data || b.invalid {
            return Expect::Any;
        }
        let op = operator(id);
        if let Some(op) = op.filter(|op| matches!(op.stage, Stage::Pow | Stage::Check)) {
            return Expect::Reject(op.rejects);
        }
        if !b.indexed() {
            if !w.blocks[parent].indexed() {
                return Expect::Reject(&["prev-blk-not-found"]);
            }
            if let Some(e) = new_header(id, parent) {
                return e;
            }
        }
        if !w.presumed_valid(parent) || w.ancestry(parent).any(|a| cl_conflict(w, a)) {
            return Expect::Any;
        }
        match op {
            Some(op) if op.stage == Stage::Contextual => Expect::Reject(op.rejects),
            // A header known before the lock skipped its check: ConnectBlock checks it first.
            _ if cl_conflict(w, id) && connects(w, id) => Expect::Reject(&["bad-chainlock"]),
            None if connects(w, id) => Expect::Ok,
            None => Expect::Stored,
            // Found only by ConnectBlock: a block Core only stores is accepted.
            Some(op) if op.stage == Stage::Connect && !connects(w, id) => Expect::Stored,
            Some(op) => Expect::Reject(op.rejects),
        }
    };
    match &p.what {
        What::Block(id) => block(*id),
        What::Header(id) => {
            let b = &w.blocks[*id];
            let Some(parent) = b.parent else {
                return Expect::Any;
            };
            if !w.blocks[parent].indexed() {
                return Expect::Reject(&["Must submit previous header"]);
            }
            if b.indexed() {
                return Expect::Any;
            }
            // A conflict mark above the lock's height outlives a reconsider (`bad-prevblk-chainlock`).
            if w.ancestry(parent).any(|a| cl_conflict(w, a)) {
                return Expect::Any;
            }
            match operator(*id) {
                Some(op) if op.stage == Stage::Pow => Expect::Reject(op.rejects),
                _ => new_header(*id, parent).unwrap_or(Expect::Ok),
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
                return Expect::Any;
            }
            if cl_conflict(w, *id) {
                return Expect::Reject(&["bad-chainlock"]);
            }
            match (operator(*id), too_new(*id)) {
                (None, false) => Expect::Ok,
                (None, true) => Expect::Reject(&["time-too-new"]),
                (Some(op), true) if matches!(op.stage, Stage::Pow | Stage::Header) => Expect::Any,
                (Some(_), true) => Expect::Reject(&["time-too-new"]),
                (Some(op), false) => Expect::Reject(op.rejects),
            }
        }
        What::Target(Some(t)) if w.blocks[*t].indexed() => Expect::Ok,
        What::Target(_) => Expect::Reject(&["Block not found"]),
        What::Ok => Expect::Ok,
        What::Op => match &p.op {
            // `ProcessNewChainLock`: a lock recorded since the load is `seen`, one at or below
            // the best recorded height `stale` (the shim's signatures are all valid). Core's
            // seen set is pruned once it grows past `CL_SEEN_KEPT` entries.
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
            } if *height <= w.cl_height => Expect::Reject(&["stale"]),
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
        What::Any => Expect::Any,
    }
}

/// The census name of an enforcement the tie model refuses after a record.
const UNENFORCED: &str = "cl_unenforced";

/// The ChainLock handler's seen set: an insert at this size prunes it
/// (`SEEN_CHAINLOCKS_MAX_SIZE`).
const CL_SEEN_KEPT: usize = 2048;

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
pub fn due_enforce(w: &World, census: &mut Census) -> Option<Option<Planned>> {
    w.cl_due().then(|| enforce_counted(w, w.best_cl, census, UNENFORCED, UNENFORCED))
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

/// An extension's block count: 1..=8, `n` itself when it is in range (0 is 8).
fn count(n: u8) -> u8 {
    n.wrapping_sub(1) % 8 + 1
}

fn mtp(w: &World, parent: usize) -> u32 {
    let mut times: Vec<u32> = w.ancestry(parent).take(11).map(|i| w.blocks[i].time).collect();
    times.sort_unstable();
    times[times.len() / 2]
}

impl Gen {
    /// The ops of one intent. Blocks it builds are registered in `w` (held [`Held::Nothing`]);
    /// what Core makes of them is folded in when they run.
    pub fn resolve(&mut self, w: &mut World, intent: &Intent, census: &mut Census) -> Vec<Planned> {
        let mut out = Vec::new();
        match intent {
            Intent::Extend {
                branch,
                n,
                flags,
            } => {
                let (slot, tip) = self.branch(w, *branch);
                if let Some(last) = self.chain(w, tip, count(*n), *flags, &mut out) {
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
                if let Some(last) = self.chain(w, base, count(*n), *flags, &mut out) {
                    w.branches.push(last);
                }
            }
            Intent::Mutate {
                op,
            } => self.armed = Some(usize::from(*op) % OPERATORS.len()),
            Intent::Propose {
                on,
                mutate,
                again,
            } => self.propose(w, *on, *mutate, *again, &mut out),
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
                    w.best_cl,
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

    /// A proposal of a new block on `on`'s block (with operator `mutate - 1` if nonzero), or with
    /// `again`, of `on`'s own bytes again.
    fn propose(&mut self, w: &mut World, on: Ref, mutate: u8, again: bool, out: &mut Vec<Planned>) {
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
            let mutation = (mutate > 0).then(|| (usize::from(mutate) - 1) % OPERATORS.len());
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
        let better = |t: usize| {
            w.blocks[t].indexed()
                && w.best_cl.is_none_or(|b| w.blocks[t].height > w.blocks[b].height)
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
        let locked_above = w.best_cl.is_some_and(|cl| w.blocks[cl].height > w.blocks[w.tip].height);
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
            self.chain(w, tip, deficit as u8, 0, out);
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
    fn chain(
        &mut self,
        w: &mut World,
        tip: usize,
        n: u8,
        flags: u8,
        out: &mut Vec<Planned>,
    ) -> Option<usize> {
        let mut ids = Vec::new();
        let mut parent = tip;
        for _ in 0..n {
            if self.blocks_left == 0 {
                break;
            }
            self.clock(w, parent, out);
            let mutation = self.armed.take();
            parent = self.block(w, parent, flags, mutation);
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
        let (mut txs, used) = shaped_txs(intent::shape(flags), &coins);
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
        // Low on a short branch, too few coins are mature for an operator: the block is plain.
        let mutation = mutation.filter(|_| rest.len() >= mutate::COINS);
        if let Some(m) = mutation {
            let cx = mutate::Cx {
                height,
                mtp,
                mocktime: w.mocktime,
                coins: &rest,
                immature,
            };
            (OPERATORS[m].apply)(&mut plan, &cx);
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
fn shaped_txs(shape: TxShape, coins: &[Coin]) -> (Vec<TxPlan>, usize) {
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
                let each = (c.value - 1_000) / FAN_OUT as i64;
                txs.push(TxPlan::simple(
                    vec![Input::new(c.outpoint, Vec::new())],
                    (0..FAN_OUT).map(|_| to_true(each)).collect(),
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

/// What `txs` leave to the coinbase: their inputs less their outputs. An input spends a coin
/// of `utxos`, or an output of an earlier transaction of the block.
fn fee_total(txs: &[TxPlan], utxos: &BTreeMap<Outpoint, Coin>) -> i64 {
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
fn effects(plan: &BlockPlan, built: &Built, height: u32) -> (Vec<Coin>, Vec<Outpoint>) {
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
