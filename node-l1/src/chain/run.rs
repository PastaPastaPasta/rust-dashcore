//! Running programs (O2b-3 §2.5): an input is the image restored at its start time, then its
//! intents resolved one at a time into ops, each run on Core and folded into the world model.
//! Every run ends with the image restored again at [`T_RECENT`], so that each input starts from
//! the same state whatever ran before it (O2b-3h: the coverage libFuzzer sees for an input must
//! not depend on its predecessors, D36).
//!
//! O2b-3a runs Core alone (P6): each step is checked for the shim's health (a step that fails
//! is a shim fault), against the generator's expectation, and, on a sample of inputs, for
//! same-process determinism (the input run again gives byte-identical reports over its
//! untainted prefix: O2b-1's "B alone = B after A", D35). It is never a certification leg.
//!
//! O2b-3b adds [`Mode::Diff`] (with `sut`): each op runs on `NodeSut` too and the two steps are
//! compared ([`super::diff`]). The first difference ends the input as a divergence, and an op the
//! node does not serve yet ends it out of domain. [`Mode::CoreOnly`] runs as before.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, VecDeque};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use dash_oracle_sys::{ChainOp, Pin, RawReport, RestartKind};

use super::census::{Census, Totals, class_name};
use super::core::{CoreStep, CoreSut, Schedule, T_OLD, T_RECENT};
#[cfg(feature = "sut")]
use super::diff::{Diff, Judgement};
use super::generate::{Gen, Planned, What, cl_conflict, due_enforce, expect};
use super::intent::{MAX_INPUT, Program};
use super::mutate::OPERATORS;
use super::step::{ReplyClass, Step};
use super::world::World;
use crate::{Compared, Divergence, Outcome};

/// The census name of an op.
pub fn op_kind(op: &ChainOp) -> &'static str {
    match op {
        ChainOp::Block(_) => "block",
        ChainOp::Header(_) => "header",
        ChainOp::Proposal(_) => "proposal",
        ChainOp::Invalidate(_) => "invalidate",
        ChainOp::Reconsider {
            ..
        } => "reconsider",
        ChainOp::Precious(_) => "precious",
        ChainOp::Activate => "activate",
        ChainOp::ClRecord {
            ..
        } => "cl_record",
        ChainOp::ClEnforce => "cl_enforce",
        ChainOp::Spork {
            ..
        } => "spork",
        ChainOp::Flush {
            ..
        } => "flush",
        ChainOp::Restart(_) => "restart",
        ChainOp::SetTime(_) => "settime",
    }
}

/// What a runner runs each op on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Core alone (O2b-3a).
    CoreOnly,
    /// Core and `NodeSut`, compared op by op (O2b-3b).
    #[cfg(feature = "sut")]
    Diff,
}

/// How an input ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// Every intent ran.
    Done,
    /// Core's tie taint fired (D30) at this op kind with these sites: the input is discarded.
    Tainted(&'static str, Vec<u32>),
    /// A reply the generator did not expect (`op: expected …, got …`): the input stops.
    Unexpected(String),
    /// Diff mode: the first plane or post field where the two systems differ.
    Diverged(Divergence),
    /// Diff mode: an op the node did not serve.
    Unsupported(Unsupported),
}

/// Why the node did not serve an op (Diff mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// An op kind V does not serve yet: nothing ran it.
    Op,
    /// Both sides queued the same count of signals, so many that Core may have delivered some
    /// mid-op, where the node's events cannot say; the steps are otherwise the same.
    QueueDrain,
}

/// An op the node did not serve: its kind ([`op_kind`]) and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unsupported {
    pub kind: &'static str,
    pub why: Refused,
}

impl Unsupported {
    pub fn new(op: &ChainOp, why: Refused) -> Unsupported {
        Unsupported {
            kind: op_kind(op),
            why,
        }
    }

    /// The census key: `unsupported` for an op kind refused before it ran, else the reason it
    /// ran unserved.
    pub fn key(&self) -> &'static str {
        match self.why {
            Refused::Op => "unsupported",
            Refused::QueueDrain => "unsupported-queue-drain",
        }
    }
}

impl End {
    /// For a discarded input, the op kind and tie sites of the step that fired the taint.
    pub fn discard(&self) -> Option<(&'static str, &[u32])> {
        match self {
            End::Tainted(op, sites) => Some((*op, &sites[..])),
            _ => None,
        }
    }
}

/// One input's run.
pub struct Run {
    pub end: End,
    /// The version-normalized raw reports, step 0 (the restore) first, through the step that
    /// ended the input.
    pub raws: Vec<RawReport>,
    /// Each report's op kind ([`op_kind`]; `restore` for step 0).
    pub kinds: Vec<&'static str>,
    pub census: Census,
}

impl Run {
    /// The reports a replay must reproduce: all but a tainted last step.
    pub fn untainted(&self) -> &[RawReport] {
        match self.end {
            End::Tainted(..) => &self.raws[..self.raws.len() - 1],
            _ => &self.raws,
        }
    }
}

/// A shim fault: the session is unusable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShimFault(pub String);

/// The process's runner for one schedule.
pub struct Runner {
    pub pin: Pin,
    pub schedule: Schedule,
    pub mode: Mode,
    core: CoreSut,
    /// Diff mode's node.
    #[cfg(feature = "sut")]
    diff: Option<Diff>,
    image: World,
    pub totals: Totals,
    /// The last reset's load of the image at [`T_RECENT`]: an input at that clock starts from
    /// it rather than restoring again.
    ready: Option<CoreStep>,
}

/// What the run loop tracks for the named census rows (O2b §6.5).
struct Rows {
    schedule: Schedule,
    /// Blocks an INVALIDATE of the active chain marked.
    invalidated: BTreeSet<usize>,
    /// ChainLocks recorded before the model's node had the block's header.
    cl_early: BTreeSet<usize>,
    ibd: Option<bool>,
    testdummy_active: bool,
}

impl Rows {
    fn new(schedule: Schedule) -> Rows {
        Rows {
            schedule,
            invalidated: BTreeSet::new(),
            cl_early: BTreeSet::new(),
            ibd: None,
            testdummy_active: false,
        }
    }

    /// The rows `p`'s step reached, read with the world as it was before the step.
    fn see(&mut self, w: &World, p: &Planned, step: &Step, census: &mut Census) {
        let class = step.class(&p.op);
        let ok = class == ReplyClass::Ok;
        match &p.op {
            ChainOp::Invalidate(h) if ok => {
                if let Some(id) = w.id(h).filter(|&id| w.is_ancestor(id, w.tip)) {
                    self.invalidated.insert(id);
                }
            }
            ChainOp::Reconsider {
                hash,
                ignore_chainlocks: true,
            } if ok && w.id(hash).is_some_and(|id| w.ancestry(id).any(|a| cl_conflict(w, a))) => {
                census.row("reconsider_ignore_cl_conflict");
            }
            ChainOp::ClRecord {
                hash,
                ..
            } => {
                if let Some(id) = w.id(hash).filter(|&id| !w.blocks[id].indexed()) {
                    self.cl_early.insert(id);
                }
            }
            ChainOp::Block(_) | ChainOp::Header(_) => {
                if let What::Block(id) | What::Header(id) = p.what
                    && self.cl_early.contains(&id)
                    && matches!(
                        class,
                        ReplyClass::Ok | ReplyClass::Duplicate | ReplyClass::Inconclusive
                    )
                {
                    self.cl_early.remove(&id);
                    census.row("cl_before_header");
                }
                if let What::Block(id) = p.what
                    && w.blocks[id].mutation.is_some_and(|m| OPERATORS[m].par)
                    && self.schedule == Schedule::S1
                    && matches!(class, ReplyClass::Reject(_))
                {
                    census.row("multi_failure_s1");
                }
            }
            _ => {}
        }
        if let Some(ibd) = step.post.ibd {
            if matches!(p.op, ChainOp::SetTime(_)) && self.ibd.is_some_and(|was| was != ibd) {
                census.row("ibd_flip_settime");
            }
            self.ibd = Some(ibd);
        }
        if let Some(d) = &step.post.deployments {
            let active =
                d.pointer("/deployments/testdummy/active").and_then(|a| a.as_bool()) == Some(true);
            if active && !self.testdummy_active {
                census.row("testdummy_active");
            }
            self.testdummy_active = active;
        }
    }

    /// The rows read off the world after `op`'s step was folded in. `lost`: a kill with
    /// unflushed index entries.
    fn settled(&self, w: &World, op: &ChainOp, old_tip: usize, lost: bool, census: &mut Census) {
        if reorg_depth(w, old_tip) >= 3 {
            census.row("reorg_3");
        }
        match op {
            ChainOp::Restart(_) => {
                if lost {
                    census.row("kill_with_unflushed_entries");
                }
                if w.down {
                    census.row("refused_load");
                }
                if self.invalidated.iter().any(|&id| w.is_ancestor(id, w.tip)) {
                    census.row("invalidate_undone_by_restart");
                }
            }
            ChainOp::Precious(_) if w.tip != old_tip => census.row("precious_switch"),
            _ => {}
        }
    }
}

/// How deep below the old tip the active chain switched (0: no reorg).
fn reorg_depth(w: &World, old_tip: usize) -> u32 {
    let fork = w.ancestry(old_tip).find(|&a| w.is_ancestor(a, w.tip)).unwrap_or(0);
    w.blocks[old_tip].height - w.blocks[fork].height
}

impl Runner {
    /// The session, its image built, for `schedule`, reset as after a run.
    pub fn new(pin: Pin, schedule: Schedule) -> Result<Runner, ShimFault> {
        Runner::with_mode(pin, schedule, Mode::CoreOnly)
    }

    /// [`Runner::new`] in `mode`. Diff mode starts the node first (D8's check needs the oracle
    /// free); a failure on either side is a [`ShimFault`].
    pub fn with_mode(pin: Pin, schedule: Schedule, mode: Mode) -> Result<Runner, ShimFault> {
        #[cfg(feature = "sut")]
        let diff = match mode {
            Mode::CoreOnly => None,
            Mode::Diff => Some(Diff::start(pin, schedule).map_err(|e| node_fault(&e))?),
        };
        let (core, image) = CoreSut::start(pin, schedule).map_err(ShimFault)?;
        let mut runner = Runner {
            pin,
            schedule,
            mode,
            core,
            #[cfg(feature = "sut")]
            diff,
            image,
            totals: Totals::default(),
            ready: None,
        };
        runner.reset()?;
        Ok(runner)
    }

    /// The image restored at [`T_RECENT`]: the state every run starts from.
    fn reset(&mut self) -> Result<(), ShimFault> {
        self.ready = Some(self.core.restore(T_RECENT).map_err(ShimFault)?);
        #[cfg(feature = "sut")]
        if let Some(diff) = &mut self.diff {
            diff.reset().map_err(|e| node_fault(&e))?;
        }
        Ok(())
    }

    /// Run `program` from the image, then reset.
    pub fn run(&mut self, program: &Program) -> Result<Run, ShimFault> {
        let run = self.run_from_image(program)?;
        self.reset()?;
        Ok(run)
    }

    fn run_from_image(&mut self, program: &Program) -> Result<Run, ShimFault> {
        let mut w = self.image.clone();
        w.mocktime = if program.old {
            T_OLD
        } else {
            T_RECENT
        };
        let restored = match self.ready.take() {
            Some(ready) if w.mocktime == T_RECENT => ready,
            _ => self.core.restore(w.mocktime).map_err(ShimFault)?,
        };
        w.loaded();
        if restored.step.post.down {
            return Err(ShimFault("the image's load was refused".into()));
        }
        if restored.tainted {
            // A clean image's restore clears the latch, and a one-chain image has no tie (D9):
            // a shim fault, not a discard.
            return Err(ShimFault("the image's load tainted".into()));
        }
        #[cfg(feature = "sut")]
        if let Some(diff) = &mut self.diff {
            diff.begin(w.mocktime).map_err(|e| node_fault(&e))?;
        }
        let mut run = Run {
            end: End::Done,
            raws: vec![restored.raw],
            kinds: vec!["restore"],
            census: Census::default(),
        };
        let mut generator = Gen::default();
        let mut rows = Rows::new(self.schedule);
        let restore = Planned::new(ChainOp::SetTime(w.mocktime), What::Ok);
        rows.see(&w, &restore, &restored.step, &mut run.census);
        for intent in &program.intents {
            let mut ops: VecDeque<Planned> =
                generator.resolve(&mut w, intent, &mut run.census).into();
            while let Some(p) = ops.pop_front() {
                let expected = expect(&w, &p);
                let kind = op_kind(&p.op);
                let lost = matches!(p.op, ChainOp::Restart(RestartKind::Kill))
                    && w.blocks.iter().any(|b| b.dirty());
                let old_tip = w.tip;
                #[cfg(feature = "sut")]
                let node = match self.diff.as_mut().map(|diff| diff.step(&p.op)) {
                    None => None,
                    Some(Ok(ran)) => Some(ran),
                    Some(Err(unsupported)) => {
                        run.census.unsupported(unsupported.kind, unsupported.key());
                        run.end = End::Unsupported(unsupported);
                        return Ok(run);
                    }
                };
                let done = self.core.step(&p.op).map_err(ShimFault)?;
                run.raws.push(done.raw);
                run.kinds.push(kind);
                let step = done.step;
                let class = step.class(&p.op);
                // The session's latch, not this report's sites: the input stops at the first
                // tie-decided step and nothing after it is scored (O2b-2 F1).
                if done.tainted {
                    run.census.op(kind, &class);
                    run.end = End::Tainted(kind, step.post.tie.clone());
                    return Ok(run);
                }
                #[cfg(feature = "sut")]
                if let (Some(diff), Some(node)) = (&self.diff, &node) {
                    // A refusal stands only if Core's step shows the same; else compared. A
                    // refused op is counted as out of domain only, not among the ops.
                    match diff.judge(&step, node) {
                        Judgement::Same(skipped) => run.census.verifydb(skipped.verifydb),
                        Judgement::Refused(why) => {
                            let unsupported = Unsupported::new(&p.op, why);
                            run.census.unsupported(unsupported.kind, unsupported.key());
                            run.end = End::Unsupported(unsupported);
                            return Ok(run);
                        }
                        Judgement::Diverged(d) => {
                            run.census.op(kind, &class);
                            run.end = End::Diverged(d);
                            return Ok(run);
                        }
                    }
                }
                run.census.op(kind, &class);
                if !expected.met(&class) {
                    let what =
                        format!("{kind}: expected {}, got {}", expected.name(), reply_name(&class));
                    run.census.unexpected(what.clone());
                    run.end = End::Unexpected(what);
                    return Ok(run);
                }
                rows.see(&w, &p, &step, &mut run.census);
                w.observe(&p.op, &step);
                rows.settled(&w, &p.op, old_tip, lost, &mut run.census);
                match due_enforce(&w, &mut run.census) {
                    Some(Some(enforce)) => ops.push_front(enforce),
                    // Every later op is outside 3a's scope: the input ends here.
                    Some(None) => return Ok(run),
                    None => {}
                }
            }
        }
        Ok(run)
    }
}

/// A failure of the node's side, as the runner reports a session it cannot use.
#[cfg(feature = "sut")]
fn node_fault(message: &str) -> ShimFault {
    ShimFault(format!("node: {message}"))
}

fn reply_name(class: &ReplyClass) -> String {
    match class {
        ReplyClass::Reject(r) => format!("reject {r}"),
        other => class_name(other).to_owned(),
    }
}

/// The fuzz target replays about one input in this many.
pub const REPLAY_EVERY: u64 = 16;

/// 64-bit FNV-1a: fixed, so that which inputs are replayed does not move with the toolchain
/// (`DefaultHasher` promises no stability across releases).
fn input_hash(data: &[u8]) -> u64 {
    data.iter()
        .fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// Whether the fuzz target replays `data`: chosen by the input's hash, so that an input is
/// replayed or not wherever it comes in a run (O2b-3h, D36). The hash's high half decides: its
/// low bits mix only the bytes' low bits.
pub fn replayed(data: &[u8]) -> bool {
    (input_hash(data) >> 32).is_multiple_of(REPLAY_EVERY)
}

/// How many unexpected inputs one process logs and keeps.
const KEPT_UNEXPECTED: usize = 64;

/// An input that ended on a reply the generator did not expect: logged, and kept (its bytes)
/// in `diff_chain-unexpected/` of the temporary directory (the run's private `tmp/` under
/// node-fuzz/run.sh), for the expectations' triage; the first [`KEPT_UNEXPECTED`] only. So an
/// unexpected input's coverage depends a little on how many came before it (the first creates
/// the directory, those past the cap return at once): the one exception to D36's input
/// independence, harmless while no input is unexpected.
fn kept_unexpected(data: &[u8], what: &str) {
    static KEPT: AtomicUsize = AtomicUsize::new(0);
    if KEPT.fetch_add(1, Ordering::Relaxed) >= KEPT_UNEXPECTED {
        return;
    }
    let dir = std::env::temp_dir().join("diff_chain-unexpected");
    let path = dir.join(format!("{:016x}", input_hash(data)));
    let kept = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .and_then(|()| std::fs::write(&path, data));
    let saved = match kept {
        Ok(()) => format!("kept as {}", path.display()),
        Err(e) => format!("not kept: {e}"),
    };
    eprintln!("diff_chain: an unexpected reply ({what}); the input is {saved}");
}

/// The census file: `NODE_L1_CHAIN_STATS`, else `diff_chain.stats.json` in the temporary
/// directory (node-fuzz/run.sh gives the target a clean environment whose `TMPDIR` is the run's
/// private `tmp/`).
fn stats_path() -> PathBuf {
    std::env::var_os("NODE_L1_CHAIN_STATS")
        .map_or_else(|| std::env::temp_dir().join("diff_chain.stats.json"), PathBuf::from)
}

/// Inputs whose census waits to be folded into the totals. A fold's coverage depends on what the
/// totals hold already, so it runs only this often and when the session goes, and writes the
/// census file: an input's coverage then does not depend on how many inputs came before it
/// (O2b-3h), but for the one input in this many that the fold falls in. The fold at exit runs
/// in the thread's destructors, which libFuzzer's `exit` paths (`-runs`, `-max_total_time`, a
/// graceful stop) run and its SIGINT and crash paths (`_Exit`) do not: a process stopped that
/// way loses up to `FOLD_EVERY - 1` inputs of census, so a reader takes a missing or short
/// census file after such a stop as expected. Coverage and verdicts are unaffected.
///
/// Most of the fold's coverage is new to libFuzzer only the first time it runs, so the process
/// warms it in its first non-empty input that a session runs ([`Session::warm`]; the corpus's
/// first such input, so the coverage lands in libFuzzer's corpus load, INITED), and the first
/// fold of the inputs' own censuses comes at input `FOLD_EVERY + 1`. The warm cannot reach all
/// of the fold's data-dependent paths (B-tree shapes, the rate's digits), so a later fold can
/// still add a little coverage once (DEC-260 (3), (4)).
const FOLD_EVERY: usize = 4096;

/// The fuzz process's runner, and the censuses of the inputs since the last fold.
struct Session {
    runner: Runner,
    /// Allocated once, at the session's start, so that no input grows it.
    unfolded: Vec<(Census, End)>,
}

impl Session {
    fn new(pin: Pin, mode: Mode) -> Result<Session, ShimFault> {
        Ok(Session {
            // S0; S1 is O2b-4's `diff_chain_par`.
            runner: Runner::with_mode(pin, Schedule::S0, mode)?,
            unfolded: Vec::with_capacity(FOLD_EVERY),
        })
    }

    /// Runs the fold's code once per process, inside the input that calls it: [`warm_fold`]'s
    /// synthetic censuses into scratch totals, then this session's own fold of the inputs so far
    /// (which writes the census file). libFuzzer clears its coverage counters before each input
    /// and, in fuzzing and corpus load, collects no empty input's features, so neither the
    /// initialisation hook nor its empty first input can do this. Nothing here reaches the
    /// oracle; the scratch totals are dropped.
    fn warm(&mut self) {
        if WARMED.replace(true) {
            return;
        }
        std::hint::black_box(warm_fold());
        self.fold_if(true);
    }

    /// Count an input; every [`FOLD_EVERY`] inputs, fold.
    fn input(&mut self, census: Census, end: End) {
        self.unfolded.push((census, end));
        self.fold_if(self.unfolded.len() == FOLD_EVERY);
    }

    /// Folds if `due`. The warm and the inputs fold through this one call, and the fold's code
    /// is never inlined, so that a due fold runs only code the warm ran: an inlined copy at
    /// each call site would be new coverage at the first real fold.
    #[inline(never)]
    fn fold_if(&mut self, due: bool) {
        if std::hint::black_box(due) {
            self.fold();
        }
    }

    /// The waiting censuses into the totals, and the totals into the census file.
    #[inline(never)]
    fn fold(&mut self) {
        if self.unfolded.is_empty() && self.runner.totals.inputs == 0 {
            return;
        }
        let census = fold_into(&mut self.runner.totals, &mut self.unfolded);
        let _ = std::fs::write(stats_path(), census);
    }
}

/// `unfolded` into `totals`; the census file's text. Never inlined, so that [`warm_fold`] and a
/// session's fold run the same copy of it.
#[inline(never)]
fn fold_into(totals: &mut Totals, unfolded: &mut Vec<(Census, End)>) -> String {
    for (census, end) in unfolded.drain(..) {
        totals.input(census, end.discard());
    }
    totals.to_json().to_string()
}

/// Keys per map in [`warm_fold`]'s censuses: more than a B-tree's leaf holds, so its maps grow
/// inner levels, as a long run's totals do. Not a multiple of 128: a block the warm runs once
/// per key in each of its two rounds would wrap its 8-bit coverage counter to 0, unseen.
const WARM_KEYS: usize = 199;
const _: () = assert!(!WARM_KEYS.is_multiple_of(128));

/// [`warm_fold`]'s key names: the suffixes of this text, all distinct.
static WARM_NAMES: [u8; WARM_KEYS] = [b'w'; WARM_KEYS];

/// The order [`warm_fold`] takes its keys in: every key once, scrambled, so that the totals'
/// maps take new keys at both ends and in between, as they do from real inputs.
fn warm_order() -> impl Iterator<Item = usize> {
    (0..WARM_KEYS).map(|i| i * 97 % WARM_KEYS)
}

/// Folds synthetic censuses into scratch totals twice (new keys, then the same keys again),
/// with every way an input can end, every tie site, op kinds with two classes and keys that the
/// census file must escape; folds censuses of every size; formats a spread of rates and counts;
/// and returns the totals. Nothing reaches the oracle, a session's totals or the census file.
fn warm_fold() -> Totals {
    let name = |i: usize| std::str::from_utf8(&WARM_NAMES[i..]).expect("ASCII");
    let add = |census: &mut Census, i: usize| {
        // Two classes of one kind, one a prefix of the other.
        census.ops.insert((name(i), "re"), 1);
        census.ops.insert((name(i), "reject"), 1);
        census.rejects.insert(i.to_string(), 1);
        census.rows.insert(name(i), 1);
        census.avoided.insert(name(i), 1);
        census.unexpected.insert(i.to_string(), 1);
        census.deferred_restarts = 1;
    };
    let one = |i: usize| {
        let mut census = Census::default();
        add(&mut census, i);
        census
    };
    let mut special = Census::default();
    special.ops.insert(("cl_enforce", "ok"), 1);
    special.ops.insert(("cl_enforce", "reject"), 1);
    for key in ["\"\\\n\u{1}…", "a\"b\u{8}\u{c}\t\r", "x\""] {
        special.unexpected.insert(key.to_owned(), 1);
    }
    let mut totals = Totals::default();
    for _ in 0..2 {
        let mut unfolded = vec![
            (Census::default(), End::Done),
            (special.clone(), End::Done),
            (special.clone(), End::Unexpected(String::new())),
            (special.clone(), End::Unexpected("x".to_owned())),
            (Census::default(), End::Tainted("restart", Vec::new())),
        ];
        for sites in [vec![13], vec![1], vec![1, 13], Vec::new()] {
            unfolded.push((special.clone(), End::Tainted("cl_enforce", sites)));
        }
        for i in warm_order() {
            unfolded.push((one(i), End::Done));
            // The tie sites are 1-14 (D30).
            unfolded.push((one(i), End::Tainted(name(i), vec![(i % 14 + 1) as u32])));
        }
        std::hint::black_box(fold_into(&mut totals, &mut unfolded));
    }
    // Censuses and totals of every size a real fold meets, so every B-tree height and every
    // sort length the census file's maps take: each folded, scored and discarded, into fresh
    // totals.
    for n in [2, 3, 5, 8, 13, 21, 34, 55, 89, 144, WARM_KEYS] {
        let mut census = Census::default();
        for i in warm_order().take(n) {
            add(&mut census, i);
        }
        let site = (n % 14 + 1) as u32;
        let mut unfolded =
            vec![(census.clone(), End::Done), (census, End::Tainted("restart", vec![site]))];
        std::hint::black_box(fold_into(&mut Totals::default(), &mut unfolded));
    }
    for i in 0..WARM_KEYS {
        for rate in [(i + 1) as f64 / 4096.0, (i + 1) as f64 / (i * i * 31 + 7) as f64] {
            std::hint::black_box(serde_json::Value::from(rate).to_string());
        }
    }
    // Counts of every length: a real fold's totals outgrow the warm's own (itoa's branches).
    for n in (0..20).flat_map(|k| [10u64.pow(k), 10u64.pow(k) - 1, 10u64.pow(k) / 4 * 7]) {
        std::hint::black_box(serde_json::Value::from(n).to_string());
    }
    totals
}

impl Drop for Session {
    /// At the process's exit, or when a shim fault or another pin replaces the session.
    fn drop(&mut self) {
        self.fold();
    }
}

thread_local! {
    /// One chain session per process (O1c).
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    /// Whether [`Session::warm`] has run in this process: once, whichever session runs it.
    static WARMED: Cell<bool> = const { Cell::new(false) };
}

/// The session on `pin` in `mode`, started unless it is running.
fn session_on(slot: &mut Option<Session>, pin: Pin, mode: Mode) -> Result<&mut Session, ShimFault> {
    slot.take_if(|s| s.runner.pin != pin || s.runner.mode != mode);
    if slot.is_none() {
        *slot = Some(Session::new(pin, mode)?);
    }
    Ok(slot.as_mut().expect("set above"))
}

/// `diff_chain`'s start, from libFuzzer's initialisation hook: the session, so that no input's
/// coverage includes it (O2b-3h). A failed start is the first input's shim fault.
pub fn start(pin: Pin) {
    SESSION.with_borrow_mut(|slot| {
        let _ = session_on(slot, pin, Mode::CoreOnly);
    });
}

/// `diff_chain`'s comparison, Core alone: [`fuzz_one_in`] in [`Mode::CoreOnly`].
pub fn fuzz_one(data: &[u8], pin: Pin) -> Compared {
    fuzz_one_in(data, pin, Mode::CoreOnly)
}

/// `diff_chain`'s comparison in `mode`: a tie discard is [`Outcome::Discarded`], an unexpected
/// reply stops the input (counted in the census, logged and kept), and a shim fault or a replay
/// that differs is a divergence. In Diff mode the first plane where the systems differ is the
/// divergence, and an op the node does not serve yet makes the input out of domain.
pub fn fuzz_one_in(data: &[u8], pin: Pin, mode: Mode) -> Compared {
    if data.len() > MAX_INPUT {
        return Ok(Outcome::OutOfDomain);
    }
    let program = Program::decode(data);
    SESSION.with_borrow_mut(|slot| {
        let session = session_on(slot, pin, mode).map_err(|fault| {
            // D8's per-leaf report (or any start failure) once: every later input fails the same.
            static REPORTED: std::sync::Once = std::sync::Once::new();
            REPORTED.call_once(|| eprintln!("diff_chain: the session did not start: {}", fault.0));
            shim()
        })?;
        let runner = &mut session.runner;
        let Ok(run) = runner.run(&program) else {
            // The session is broken (oracle.h): the next input starts a new one.
            *slot = None;
            return Err(shim());
        };
        let mut verdict = match &run.end {
            End::Done => Ok(Outcome::BothAccept),
            End::Tainted(..) => Ok(Outcome::Discarded),
            End::Unexpected(what) => {
                kept_unexpected(data, what);
                Ok(Outcome::OutOfDomain)
            }
            End::Unsupported(_) => Ok(Outcome::OutOfDomain),
            End::Diverged(d) => Err(*d),
        };
        if replayed(data) {
            runner.totals.replays += 1;
            let Ok(again) = runner.run(&program) else {
                *slot = None;
                return Err(shim());
            };
            if !replays(&run, &again) {
                runner.totals.replay_mismatches += 1;
                verdict = Err(Divergence {
                    field: "replay",
                });
            }
        }
        session.input(run.census, run.end);
        if !data.is_empty() {
            session.warm();
        }
        verdict
    })
}

/// Whether `again` reproduces `run`: the same end and reports, or, when either was tainted,
/// the same reports as far as both are untainted.
pub fn replays(run: &Run, again: &Run) -> bool {
    let tainted = |r: &Run| matches!(r.end, End::Tainted(..));
    if tainted(run) || tainted(again) {
        let (a, b) = (run.untainted(), again.untainted());
        let n = a.len().min(b.len());
        a[..n] == b[..n]
    } else {
        run.end == again.end && run.raws == again.raws
    }
}

fn shim() -> Divergence {
    Divergence {
        field: "shim",
    }
}

#[cfg(test)]
mod tests {
    use super::{WARM_KEYS, input_hash, replayed, warm_fold, warm_order};

    #[test]
    fn the_warm_fold_reaches_what_real_folds_do() {
        let order: Vec<usize> = warm_order().collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..WARM_KEYS).collect::<Vec<_>>(), "every key once");
        assert!(order.windows(2).any(|w| w[0] < w[1]) && order.windows(2).any(|w| w[0] > w[1]));

        let totals = warm_fold();
        let c = &totals.census;
        // Every tie site, and every cl_enforce discard but the siteless one's.
        assert_eq!(
            totals.discard_sites.keys().copied().collect::<Vec<_>>(),
            (1..=14).collect::<Vec<_>>()
        );
        assert_eq!(totals.site13, 2 * 2);
        assert_eq!(totals.discard_ops.len(), WARM_KEYS + 2);
        assert_eq!(totals.discard_ops["restart"], 2);
        // Kinds with two classes, one pair a prefix of the other.
        assert!(c.ops.contains_key(&("w", "re")) && c.ops.contains_key(&("w", "reject")));
        assert!(
            c.ops.contains_key(&("cl_enforce", "ok"))
                && c.ops.contains_key(&("cl_enforce", "reject"))
        );
        for len in [c.rejects.len(), c.rows.len(), c.avoided.len()] {
            assert_eq!(len, WARM_KEYS);
        }
        // Every key folded in again in the second round.
        assert!(c.rows.values().all(|&n| n == 2));
        let json = totals.to_json().to_string();
        for key in [r#""\"\\\n\u0001…""#, r#""a\"b\b\f\t\r""#, r#""x\"""#] {
            assert!(json.contains(key), "{key}");
        }
    }

    #[test]
    fn the_replay_choice_is_a_fixed_hash() {
        // FNV-1a's published vectors.
        assert_eq!(input_hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(input_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(input_hash(b"foobar"), 0x8594_4171_f739_67e8);
        // About one input in `REPLAY_EVERY`.
        let n = (0u32..16_000).filter(|i| replayed(&i.to_le_bytes())).count();
        assert!((800..1200).contains(&n), "{n} of 16000 replayed");
    }
}
