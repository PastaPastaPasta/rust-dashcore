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
//! O2b-3b adds `Mode::Diff` (with `sut`): each op runs on `NodeSut` too and the two steps are
//! compared (`super::diff`). The first difference ends the input as a divergence, and an op the
//! node does not serve yet ends it out of domain. [`Mode::CoreOnly`] runs as before.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, VecDeque};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use dash_oracle_sys::{ChainOp, Pin, RawReport, RestartKind};
use serde_json::{Map, Value};

use super::census::{Census, Fold, Fs, Phase, Totals, Writer, class_name};
use super::core::{CoreStep, CoreSut, Schedule, T_OLD, T_RECENT};
#[cfg(feature = "sut")]
use super::diff::{Diff, Judgement};
use super::generate::{Gen, Planned, What, cl_conflict, due_enforce, expect};
use super::intent::{Encoding, MAX_INPUT, Program, check_env};
use super::mutate::{Operator, chain_operator, script_failure};
#[cfg(feature = "sut")]
use super::node::NodeStep;
use super::step::{ReplyClass, Step};
use super::ties::{HARNESS_SITES, TieSite};
use super::world::{Held, World};
use crate::{Compared, Divergence, Outcome};

/// A chain fuzz target (O2b-4 4a, D39): the session it runs, keyed by
/// `(pin, mode, schedule, plant)`, and the encoding its input bytes are read with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainTarget {
    pub name: &'static str,
    pub mode: Mode,
    pub schedule: Schedule,
    pub encoding: Encoding,
    /// The bug its node side arms (O2b-4 4b), once the session's image exists.
    pub plant: ChainPlant,
}

impl ChainTarget {
    /// Its session's key on `pin`: a session serves every target with the same key.
    fn key(&self, pin: Pin) -> (Pin, Mode, Schedule, ChainPlant) {
        (pin, self.mode, self.schedule, self.plant)
    }
}

/// A chain target's planted bug (O2b-4 4b): what the node side of a planted search arms. A
/// target with one is built only with `planted`, whose consts are the only ones that name one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainPlant {
    None,
    /// The check queue's verdict for a block whose transaction loop stopped early.
    P3a,
    /// `InvalidateBlock`'s walked descendants left out of the dirty set.
    P3b,
}

#[cfg(feature = "planted")]
impl ChainPlant {
    /// The node's bug, if any.
    pub fn planted(self) -> Option<dashd_chainstate::Planted> {
        match self {
            ChainPlant::None => None,
            ChainPlant::P3a => Some(dashd_chainstate::Planted::P3a),
            ChainPlant::P3b => Some(dashd_chainstate::Planted::P3b),
        }
    }
}

/// `diff_chain`: Core alone on S0, `chain-v1` (O2b-3a), unchanged until the manager retires it.
pub const DIFF_CHAIN: ChainTarget = ChainTarget {
    name: "diff_chain",
    mode: Mode::CoreOnly,
    schedule: Schedule::S0,
    encoding: Encoding::V1,
    plant: ChainPlant::None,
};

/// `diff_chain_s0`: Core and the node on S0, `chain-v2` (O2b-4 4a). Its `TARGETS` row names this
/// constant, so the row and [`CHAIN_TARGETS`] cannot disagree on its schedule.
#[cfg(feature = "sut")]
pub const DIFF_CHAIN_S0: ChainTarget = ChainTarget {
    name: "diff_chain_s0",
    mode: Mode::Diff,
    schedule: Schedule::S0,
    encoding: Encoding::V2,
    plant: ChainPlant::None,
};

/// `diff_chain_par`: Core and the node on S1, `chain-v2` (O2b-4 4a), as [`DIFF_CHAIN_S0`].
#[cfg(feature = "sut")]
pub const DIFF_CHAIN_PAR: ChainTarget = ChainTarget {
    name: "diff_chain_par",
    mode: Mode::Diff,
    schedule: Schedule::S1,
    encoding: Encoding::V2,
    plant: ChainPlant::None,
};

/// `diff_chain_par_p3a`: [`DIFF_CHAIN_PAR`] with P3a planted in the node (O2b-4 4b), the check
/// queue's verdict searched for on S1.
#[cfg(feature = "planted")]
pub const DIFF_CHAIN_PAR_P3A: ChainTarget = ChainTarget {
    name: "diff_chain_par_p3a",
    mode: Mode::Diff,
    schedule: Schedule::S1,
    encoding: Encoding::V2,
    plant: ChainPlant::P3a,
};

/// `diff_chain_p3b`: [`DIFF_CHAIN_S0`] with P3b planted in the node (O2b-4 4b), the walked
/// descendants' missing dirty marks searched for on S0.
#[cfg(feature = "planted")]
pub const DIFF_CHAIN_P3B: ChainTarget = ChainTarget {
    name: "diff_chain_p3b",
    mode: Mode::Diff,
    schedule: Schedule::S0,
    encoding: Encoding::V2,
    plant: ChainPlant::P3b,
};

/// `diff_chain_av`: Core and the node on S2, `chain-v2` (O2b-4 4d), as [`DIFF_CHAIN_S0`].
#[cfg(feature = "sut")]
pub const DIFF_CHAIN_AV: ChainTarget = ChainTarget {
    name: "diff_chain_av",
    mode: Mode::Diff,
    schedule: Schedule::S2,
    encoding: Encoding::V2,
    plant: ChainPlant::None,
};

/// Every chain fuzz target: `node-l1/chain-encodings.toml` lists the same.
pub const CHAIN_TARGETS: &[ChainTarget] = &[
    DIFF_CHAIN,
    #[cfg(feature = "sut")]
    DIFF_CHAIN_S0,
    #[cfg(feature = "sut")]
    DIFF_CHAIN_PAR,
    #[cfg(feature = "planted")]
    DIFF_CHAIN_PAR_P3A,
    #[cfg(feature = "planted")]
    DIFF_CHAIN_P3B,
    #[cfg(feature = "sut")]
    DIFF_CHAIN_AV,
];

/// The chain target that runs `mode` on `schedule` with `plant`, if any.
pub fn chain_target(mode: Mode, schedule: Schedule, plant: ChainPlant) -> Option<ChainTarget> {
    CHAIN_TARGETS
        .iter()
        .copied()
        .find(|t| t.mode == mode && t.schedule == schedule && t.plant == plant)
}

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
    /// Core's tie taint fired (D30) at this op kind with these sites, or a harness site did
    /// (`h1`, OD-L3-97): the input is discarded.
    Tainted(&'static str, Vec<TieSite>),
    /// A reply the generator did not expect (`op: expected …, got …`): the input stops.
    Unexpected(String),
    /// Diff mode: the first plane or post field where the two systems differ.
    Diverged(Divergence),
    /// Diff mode: an op the node refused before running it ([`Refused::Op`]). A queue drain
    /// both sides show is not an end but a partial refusal ([`Run::partial`], D49).
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
    pub fn discard(&self) -> Option<(&'static str, &[TieSite])> {
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
    /// Diff mode's partial refusals (D49): each step the node ran and both sides showed as a
    /// queue drain, by its report's index (0: the restore) and op kind. The input went on past
    /// each: it is not an end.
    pub partial: Vec<(usize, &'static str)>,
    pub census: Census,
    /// A traced run's ops ([`Runner::trace`]), one per report after the restore, through the last
    /// op both systems ran alike: an op that diverged, was refused or tainted is not in it.
    #[cfg(feature = "l3")]
    pub trace: Vec<Traced>,
    /// A traced Diff run's MuHash3072 of the node's coins after the restore (step 0), judged the
    /// same as Core's.
    #[cfg(feature = "l3")]
    pub start_muhash: Option<[u8; 32]>,
}

/// One op of a traced run: the op, Core's step, and in Diff mode the MuHash3072 of the node's
/// coins after it (internal byte order), the node's step judged the same as Core's.
#[cfg(feature = "l3")]
#[derive(Clone, Debug)]
pub struct Traced {
    pub op: ChainOp,
    pub step: Step,
    pub muhash: Option<[u8; 32]>,
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
    /// The encoding its programs come in, which decides what its generator admits
    /// ([`Gen::for_encoding`]).
    pub encoding: Encoding,
    core: CoreSut,
    /// Diff mode's node.
    #[cfg(feature = "sut")]
    diff: Option<Diff>,
    image: World,
    pub totals: Totals,
    /// The last reset's load of the image at [`T_RECENT`]: an input at that clock starts from
    /// it rather than restoring again.
    ready: Option<CoreStep>,
    /// A test's change to the steps one report's judgement sees ([`Runner::plant`]).
    #[cfg(feature = "sut")]
    plant: Option<Plant>,
    /// Whether runs keep their ops ([`Run::trace`]).
    #[cfg(feature = "l3")]
    trace: bool,
}

/// A change to what the Diff runner sees at report `at` (0: the restore), for tests of what a
/// judgement does to an input (D49): `core` changes Core's step as judged, `node` the node's (the
/// world model still follows Core's real step); `refuse` has the node refuse the op before
/// running it ([`Refused::Op`]); `taint` reads Core's session as tainted after it.
#[cfg(feature = "sut")]
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct Plant {
    pub at: usize,
    pub core: fn(&mut Step),
    pub node: fn(&mut NodeStep),
    pub refuse: bool,
    pub taint: bool,
}

#[cfg(feature = "sut")]
impl Plant {
    /// No change at report `at`; set the fields to plant one.
    pub fn at(at: usize) -> Plant {
        Plant {
            at,
            core: |_| {},
            node: |_| {},
            refuse: false,
            taint: false,
        }
    }
}

/// [`Diff::judge`] of report `at`'s steps, with `plant` applied if it is for that report.
#[cfg(feature = "sut")]
fn judge_at(
    diff: &Diff,
    plant: Option<Plant>,
    at: usize,
    core: &Step,
    node: &NodeStep,
) -> Judgement {
    match plant.filter(|plant| plant.at == at) {
        None => diff.judge(core, node),
        Some(p) => {
            let (mut core, mut node) = (core.clone(), node.clone());
            (p.core)(&mut core);
            (p.node)(&mut node);
            diff.judge(&core, &node)
        }
    }
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
                    && w.blocks[id].mutation.is_some_and(|m| chain_operator(m).par)
                    && matches!(self.schedule, Schedule::S1 | Schedule::S2)
                    && matches!(class, ReplyClass::Reject(_))
                {
                    census.row("multi_failure_s1");
                }
                // S2 (O2b-4 §4.5): a line defect connected unchecked below the assumevalid
                // block, and a script failure above it refused; a defect sent under a best
                // header the model does not know, whose reply is not scored (OD-L3-100).
                if let What::Block(id) = p.what
                    && let Some(line) = w.line
                {
                    if line.defects.contains(&id) && ok {
                        census.row("av_skipped_below");
                    }
                    let b = &w.blocks[id];
                    if line.defects.contains(&id)
                        && b.held != Held::Data
                        && !b.invalid
                        && line.skips(w, id).is_none()
                    {
                        census.row("av_best_unknown");
                    }
                    if w.blocks[id].height > w.blocks[line.av].height
                        && let Some(op) = w.blocks[id].mutation.map(chain_operator)
                        && script_failure(op)
                        && checked_reject(op, &class)
                    {
                        census.row("av_checked_above");
                    }
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

/// Whether `class` is `op`'s own failure as Core reports it: one of its rejects, or `-par=4`'s
/// check-queue verdict, `block-validation-failed` (another reason, a failed parent's
/// `bad-prevblk` say, is not).
fn checked_reject(op: &Operator, class: &ReplyClass) -> bool {
    matches!(class, ReplyClass::Reject(r)
        if op.rejects.iter().chain(&["block-validation-failed"]).any(|p| r.starts_with(p)))
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
        Runner::with_plant(pin, schedule, mode, ChainPlant::None)
    }

    /// [`Runner::with_mode`] with `plant` armed in the node once its image exists (O2b-4 4b).
    ///
    /// # Panics
    ///
    /// On a plant in Core-only mode, which has no node to arm it in, or in a build without
    /// `planted`, whose node has no plants. Both before any image is built.
    pub fn with_plant(
        pin: Pin,
        schedule: Schedule,
        mode: Mode,
        plant: ChainPlant,
    ) -> Result<Runner, ShimFault> {
        assert!(
            plant == ChainPlant::None || (mode != Mode::CoreOnly && cfg!(feature = "planted")),
            "{plant:?} needs the node built with `planted`: Core alone arms nothing"
        );
        #[cfg(feature = "sut")]
        let diff = match mode {
            Mode::CoreOnly => None,
            Mode::Diff => Some(Diff::start(pin, schedule, plant).map_err(|e| node_fault(&e))?),
        };
        let (core, image) = CoreSut::start(pin, schedule).map_err(ShimFault)?;
        let mut runner = Runner {
            pin,
            schedule,
            mode,
            encoding: Encoding::V1,
            core,
            #[cfg(feature = "sut")]
            diff,
            image,
            totals: Totals::default(),
            ready: None,
            #[cfg(feature = "sut")]
            plant: None,
            #[cfg(feature = "l3")]
            trace: false,
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

    /// Every later run's judgements see `plant` (`None`: none), a test's injected difference.
    #[cfg(feature = "sut")]
    pub fn plant(&mut self, plant: Option<Plant>) {
        self.plant = plant;
    }

    /// Every later run keeps its ops ([`Run::trace`]) if `on`.
    #[cfg(feature = "l3")]
    pub fn trace(&mut self, on: bool) {
        self.trace = on;
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
        let node_restored = match &mut self.diff {
            Some(diff) => Some(diff.begin(w.mocktime).map_err(|e| node_fault(&e))?),
            None => None,
        };
        let mut run = Run {
            end: End::Done,
            raws: vec![restored.raw],
            kinds: vec!["restore"],
            partial: Vec::new(),
            census: Census::default(),
            #[cfg(feature = "l3")]
            trace: Vec::new(),
            #[cfg(feature = "l3")]
            start_muhash: None,
        };
        let mut generator = Gen::for_encoding(self.encoding);
        let mut rows = Rows::new(self.schedule);
        let restore = Planned::new(ChainOp::SetTime(w.mocktime), What::Ok);
        // Step 0 (BL-283, Q9): the restores judged as any op's steps; a queue drain is a partial
        // refusal as an op's is (D49), with no row credit.
        #[cfg(feature = "sut")]
        if let (Some(diff), Some(node)) = (&self.diff, &node_restored) {
            match judge_at(diff, self.plant, 0, &restored.step, node) {
                Judgement::Same(skipped) => run.census.skipped(skipped),
                Judgement::Refused(why, skipped) => {
                    run.census.skipped(skipped);
                    let unsupported = Unsupported {
                        kind: "restore",
                        why,
                    };
                    run.census.unsupported(unsupported.kind, unsupported.key());
                    if why != Refused::QueueDrain {
                        run.end = End::Unsupported(unsupported);
                        return Ok(run);
                    }
                    run.partial.push((0, unsupported.kind));
                }
                Judgement::Tied(site) => {
                    run.end = End::Tainted("restore", vec![site]);
                    return Ok(run);
                }
                Judgement::Diverged(d) => {
                    rows.see(&w, &restore, &restored.step, &mut run.census);
                    run.end = End::Diverged(d);
                    return Ok(run);
                }
            }
        }
        if run.partial.is_empty() {
            rows.see(&w, &restore, &restored.step, &mut run.census);
        }
        #[cfg(feature = "l3")]
        if self.trace
            && let Some(diff) = &self.diff
        {
            run.start_muhash = Some(diff.muhash().map_err(|e| node_fault(&e))?);
        }
        for intent in &program.intents {
            let mut ops: VecDeque<Planned> =
                generator.resolve(&mut w, intent, &mut run.census).into();
            while let Some(p) = ops.pop_front() {
                let expected = expect(&w, &p);
                let kind = op_kind(&p.op);
                let lost = matches!(p.op, ChainOp::Restart(RestartKind::Kill))
                    && w.blocks.iter().any(|b| b.dirty());
                let old_tip = w.tip;
                // The report this op's step will be.
                #[cfg(feature = "sut")]
                let at = run.raws.len();
                #[cfg(feature = "sut")]
                let plant = self.plant.filter(|plant| plant.at == at);
                #[cfg(feature = "sut")]
                let node = match self.diff.as_mut().map(|diff| match plant {
                    Some(plant) if plant.refuse => Err(Unsupported::new(&p.op, Refused::Op)),
                    _ => diff.step(&p.op),
                }) {
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
                #[cfg(feature = "sut")]
                let tainted = done.tainted || plant.is_some_and(|plant| plant.taint);
                #[cfg(not(feature = "sut"))]
                let tainted = done.tainted;
                if tainted {
                    run.census.op(kind, &class);
                    run.end = End::Tainted(
                        kind,
                        step.post.tie.iter().map(|&s| TieSite::Core(s)).collect(),
                    );
                    return Ok(run);
                }
                // Diff mode's queue drain both sides show (D49): the input goes on, but the op
                // is counted only as out of domain, and earns no row credit.
                #[allow(unused_mut)]
                let mut refused = false;
                #[cfg(feature = "sut")]
                if let (Some(diff), Some(node)) = (&self.diff, &node) {
                    // A refusal stands only if Core's step shows the same; else compared. The
                    // records a refused step compared go to the census as any step's (Sol r1
                    // m-1).
                    match judge_at(diff, plant, at, &step, node) {
                        Judgement::Same(skipped) => run.census.skipped(skipped),
                        Judgement::Refused(why, skipped) => {
                            run.census.skipped(skipped);
                            let unsupported = Unsupported::new(&p.op, why);
                            run.census.unsupported(unsupported.kind, unsupported.key());
                            if why != Refused::QueueDrain {
                                run.end = End::Unsupported(unsupported);
                                return Ok(run);
                            }
                            run.partial.push((at, kind));
                            refused = true;
                        }
                        // The harness's load tie (OD-L3-97): ends the input as Core's taint does.
                        Judgement::Tied(site) => {
                            run.census.op(kind, &class);
                            run.end = End::Tainted(kind, vec![site]);
                            return Ok(run);
                        }
                        Judgement::Diverged(d) => {
                            run.census.op(kind, &class);
                            run.end = End::Diverged(d);
                            return Ok(run);
                        }
                    }
                }
                if !refused {
                    run.census.op(kind, &class);
                }
                #[cfg(feature = "l3")]
                if self.trace {
                    // The node's coins where Diff held them equal: not at a refused step, nor
                    // where the node is down.
                    let muhash = match &self.diff {
                        Some(diff) if !refused && !step.post.down => {
                            Some(diff.muhash().map_err(|e| node_fault(&e))?)
                        }
                        _ => None,
                    };
                    run.trace.push(Traced {
                        op: p.op.clone(),
                        step: step.clone(),
                        muhash,
                    });
                }
                if !expected.met(&class) {
                    let what =
                        format!("{kind}: expected {}, got {}", expected.name(), reply_name(&class));
                    run.census.unexpected(what.clone());
                    run.end = End::Unexpected(what);
                    return Ok(run);
                }
                if !refused {
                    rows.see(&w, &p, &step, &mut run.census);
                }
                w.observe(&p.op, &step);
                if !refused {
                    rows.settled(&w, &p.op, old_tip, lost, &mut run.census);
                }
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
/// in `<target>-unexpected/` of the temporary directory (the run's private `tmp/` under
/// node-fuzz/run.sh), for the expectations' triage; the first [`KEPT_UNEXPECTED`] only. So an
/// unexpected input's coverage depends a little on how many came before it (the first creates
/// the directory, those past the cap return at once): the one exception to D36's input
/// independence, harmless while no input is unexpected.
fn kept_unexpected(data: &[u8], what: &str, target: ChainTarget) {
    static KEPT: AtomicUsize = AtomicUsize::new(0);
    if KEPT.fetch_add(1, Ordering::Relaxed) >= KEPT_UNEXPECTED {
        return;
    }
    let dir = std::env::temp_dir().join(format!("{}-unexpected", target.name));
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
    eprintln!("{}: an unexpected reply ({what}); the input is {saved}", target.name);
}

/// A chain target's census file name (D39): `<target>.stats.json`, one per target, so that no
/// two targets' censuses mix.
pub fn stats_name(target: ChainTarget) -> String {
    format!("{}.stats.json", target.name)
}

/// What a chain fuzz target's process was started with (O2b-4 §3a.3, §4.6): read from its
/// environment once per thread.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    /// `NODE_L1_CHAIN_ENCODING`: the encoding node-fuzz/run.sh gives the target, its registry
    /// row's.
    pub encoding: Option<String>,
    /// `NODE_L1_CHAIN_PHASE`: the certify replay the census is evidence for.
    pub phase: Option<String>,
    /// `NODE_L1_CHAIN_STATS`: the census file.
    pub stats: Option<PathBuf>,
    /// `NODE_FUZZ_RUN` and `NODE_FUZZ_PIN`: the run and the pin, as run.sh names them.
    pub run: Option<String>,
    pub pin: Option<String>,
}

impl Launch {
    fn from_env() -> Launch {
        let var = |name| std::env::var(name).ok();
        Launch {
            encoding: var("NODE_L1_CHAIN_ENCODING"),
            phase: var("NODE_L1_CHAIN_PHASE"),
            stats: std::env::var_os("NODE_L1_CHAIN_STATS").map(PathBuf::from),
            run: var("NODE_FUZZ_RUN"),
            pin: var("NODE_FUZZ_PIN"),
        }
    }

    /// Why `target` must not start under this launch, if it must not: an encoding other than its
    /// own ([`check_env`]), or a phase that is not one or has no census file to go to.
    pub fn check(&self, target: ChainTarget) -> Result<(), String> {
        check_env(target.name, self.encoding.as_deref())?;
        match (self.phase.as_deref(), &self.stats) {
            (Some(p), _) if Phase::parse(p).is_none() => {
                Err(format!("NODE_L1_CHAIN_PHASE {p} is not a phase"))
            }
            (Some(_), None) => Err("NODE_L1_CHAIN_PHASE is set without NODE_L1_CHAIN_STATS".into()),
            _ => Ok(()),
        }
    }

    /// `target`'s census writer: to `NODE_L1_CHAIN_STATS`, else [`stats_name`] in the temporary
    /// directory (run.sh gives the target a clean environment whose `TMPDIR` is the run's
    /// private `tmp/`).
    fn writer(&self, target: ChainTarget) -> Writer {
        let dest =
            self.stats.clone().unwrap_or_else(|| std::env::temp_dir().join(stats_name(target)));
        let mut provenance = Map::new();
        provenance.insert("schedule".into(), target.schedule.name().into());
        provenance.insert("run".into(), self.run.clone().into());
        provenance.insert("pin".into(), self.pin.clone().into());
        Writer::new(dest, self.phase.as_deref().and_then(Phase::parse), provenance, Fs)
    }
}

/// This thread's [`Launch`], read from the environment the first time.
fn launch() -> &'static Launch {
    LAUNCH.with(|slot| {
        slot.get().unwrap_or_else(|| {
            let launch: &'static Launch = Box::leak(Box::new(Launch::from_env()));
            slot.set(Some(launch));
            launch
        })
    })
}

/// This thread's census writer for `target`. Another target's fold (only tests run two chain
/// targets in one thread) starts a new writer, without the old one's failure.
fn writer(target: ChainTarget) -> &'static RefCell<Writer> {
    WRITER.with(|slot| match slot.get() {
        Some((name, writer)) if name == target.name => writer,
        _ => {
            let writer: &'static RefCell<Writer> =
                Box::leak(Box::new(RefCell::new(launch().writer(target))));
            slot.set(Some((target.name, writer)));
            writer
        }
    })
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
    target: ChainTarget,
    /// Given up after a shim fault or for another key: its last fold is a replaced one's.
    replaced: bool,
    runner: Runner,
    /// Allocated once, at the session's start, so that no input grows it.
    unfolded: Vec<(Census, End)>,
}

impl Session {
    fn new(pin: Pin, target: ChainTarget) -> Result<Session, ShimFault> {
        Ok(Session {
            target,
            replaced: false,
            runner: Runner::with_plant(pin, target.schedule, target.mode, target.plant).map(
                |runner| Runner {
                    encoding: target.encoding,
                    ..runner
                },
            )?,
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
        self.fold_as(Fold::Periodic);
    }

    /// [`Session::fold`], as `fold`: the census file through this thread's writer for the
    /// target, which a replaced session's fold fails when it has a phase.
    fn fold_as(&mut self, fold: Fold) {
        let target = self.target;
        let folded = !self.unfolded.is_empty() || self.runner.totals.inputs > 0;
        let mut writer = writer(target).borrow_mut();
        if !folded {
            if fold == Fold::Replaced {
                writer.fail();
            }
            return;
        }
        let stamp = writer.stamp(fold, EXECUTIONS.get());
        let text = fold_into(
            &mut self.runner.totals,
            &mut self.unfolded,
            target,
            stamp.as_ref().unwrap_or(&Map::new()),
        );
        if stamp.is_some() {
            writer.write(&text);
        }
    }
}

/// `unfolded` into `totals`; the census file's text, naming `target` and its encoding, with
/// `stamp`'s keys ([`Writer::stamp`]). Never inlined, so that [`warm_fold`] and a session's fold
/// run the same copy of it.
#[inline(never)]
fn fold_into(
    totals: &mut Totals,
    unfolded: &mut Vec<(Census, End)>,
    target: ChainTarget,
    stamp: &Map<String, Value>,
) -> String {
    for (census, end) in unfolded.drain(..) {
        totals.input(census, end.discard());
    }
    let mut census = totals.to_json();
    census["target"] = target.name.into();
    census["encoding"] = target.encoding.name().into();
    for (key, value) in stamp {
        census[key] = value.clone();
    }
    census.to_string()
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

/// The `i`th of every tie site in turn: Core's 1-14 (D30), then the harness's (OD-L3-97).
fn warm_site(i: usize) -> TieSite {
    let core = (1..=14).map(TieSite::Core);
    let all: Vec<TieSite> = core.chain(HARNESS_SITES).collect();
    all[i % all.len()]
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
        let core = |sites: &[u32]| sites.iter().map(|&s| TieSite::Core(s)).collect::<Vec<_>>();
        for sites in [core(&[13]), core(&[1]), core(&[1, 13]), Vec::new()] {
            unfolded.push((special.clone(), End::Tainted("cl_enforce", sites)));
        }
        for i in warm_order() {
            unfolded.push((one(i), End::Done));
            unfolded.push((one(i), End::Tainted(name(i), vec![warm_site(i)])));
        }
        std::hint::black_box(fold_into(&mut totals, &mut unfolded, DIFF_CHAIN, &warm_stamp()));
    }
    // Censuses and totals of every size a real fold meets, so every B-tree height and every
    // sort length the census file's maps take: each folded, scored and discarded, into fresh
    // totals.
    for n in [2, 3, 5, 8, 13, 21, 34, 55, 89, 144, WARM_KEYS] {
        let mut census = Census::default();
        for i in warm_order().take(n) {
            add(&mut census, i);
        }
        let mut unfolded = vec![
            (census.clone(), End::Done),
            (census, End::Tainted("restart", vec![warm_site(n)])),
        ];
        std::hint::black_box(fold_into(
            &mut Totals::default(),
            &mut unfolded,
            DIFF_CHAIN,
            &warm_stamp(),
        ));
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

/// A stamp of the keys a phase census gains ([`Writer::stamp`]), for [`warm_fold`]'s folds.
fn warm_stamp() -> Map<String, Value> {
    let mut stamp = Map::new();
    for (key, value) in [
        ("executions", Value::from(u64::MAX)),
        ("phase", "pre-replay".into()),
        ("schedule", "S0".into()),
        ("run", "20000101T000000Z-v24.0-1".into()),
        ("pin", Value::Null),
        ("complete", false.into()),
    ] {
        stamp.insert(key.into(), value);
    }
    stamp
}

impl Drop for Session {
    /// At the process's exit (its census's last fold), or when a shim fault or another pin
    /// replaces the session.
    fn drop(&mut self) {
        self.fold_as(if self.replaced {
            Fold::Replaced
        } else {
            Fold::Exit
        });
    }
}

/// [`LAUNCH`]'s type: what the process was started with, leaked once read.
type LaunchSlot = Cell<Option<&'static Launch>>;
/// [`WRITER`]'s type: the census writer by its target's name, leaked once made.
type WriterSlot = Cell<Option<(&'static str, &'static RefCell<Writer>)>>;
/// [`EXECUTIONS`]'s type.
type ExecSlot = Cell<u64>;

thread_local! {
    /// One chain session per process (O1c).
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    /// Whether [`Session::warm`] has run in this process: once, whichever session runs it.
    static WARMED: Cell<bool> = const { Cell::new(false) };
    /// What the process was started with, once read.
    static LAUNCH: LaunchSlot = const { Cell::new(None) };
    /// The census writer, by the target it writes for; its failure lasts the process.
    static WRITER: WriterSlot = const { Cell::new(None) };
    /// The chain target's comparisons in this process, inputs over [`MAX_INPUT`] included (O2b-4
    /// §4.6: a replay's census holds one per unit libFuzzer counts).
    static EXECUTIONS: ExecSlot = const { Cell::new(0) };
}

// `Session::drop` folds at the thread's exit, among the thread-locals' destructors, which run in
// the reverse of the order the thread first touched them: what the fold reads must have no
// destructor, so that it is still there whatever that order was.
const _: () = assert!(!std::mem::needs_drop::<LaunchSlot>());
const _: () = assert!(!std::mem::needs_drop::<WriterSlot>());
const _: () = assert!(!std::mem::needs_drop::<ExecSlot>());

/// Gives the session up, after a shim fault or for another key: its fold is a replaced one's.
fn give_up(slot: &mut Option<Session>) {
    if let Some(session) = slot.as_mut() {
        session.replaced = true;
    }
    *slot = None;
}

/// The session for `target` on `pin`, started unless one with its key (D39) is running.
fn session_on(
    slot: &mut Option<Session>,
    pin: Pin,
    target: ChainTarget,
) -> Result<&mut Session, ShimFault> {
    if slot.as_ref().is_some_and(|s| s.target.key(s.runner.pin) != target.key(pin)) {
        give_up(slot);
    }
    if slot.is_none() {
        *slot = Some(Session::new(pin, target)?);
    }
    Ok(slot.as_mut().expect("set above"))
}

/// The target this thread's session serves, if it has one.
#[cfg(test)]
fn session_target() -> Option<ChainTarget> {
    SESSION.with_borrow(|slot| slot.as_ref().map(|s| s.target))
}

/// A chain target's start, from libFuzzer's initialisation hook: the session, so that no
/// input's coverage includes it (O2b-3h). A failed start is the first input's shim fault.
pub fn start_target(pin: Pin, target: ChainTarget) {
    SESSION.with_borrow_mut(|slot| {
        let _ = session_on(slot, pin, target);
    });
}

/// A chain fuzz target's start, its `TARGETS` row's: it refuses to run, before any input, under
/// a launch [`Launch::check`] refuses (O2b-4 §3a.3 (1), §4.6), and is [`start_target`] otherwise.
pub fn start_fuzz_target(pin: Pin, target: ChainTarget) {
    if let Err(why) = launch().check(target) {
        eprintln!("{}: refusing to run: {why}", target.name);
        std::process::abort();
    }
    start_target(pin, target);
}

/// `diff_chain`'s start: [`start_fuzz_target`] for [`DIFF_CHAIN`].
pub fn start(pin: Pin) {
    start_fuzz_target(pin, DIFF_CHAIN);
}

/// `diff_chain`'s comparison, Core alone: [`fuzz_target`] for [`DIFF_CHAIN`].
pub fn fuzz_one(data: &[u8], pin: Pin) -> Compared {
    fuzz_target(data, pin, DIFF_CHAIN)
}

/// The unplanted Diff chain target on `schedule` (`diff_chain_s0` on S0, `diff_chain_par` on S1).
///
/// # Panics
/// If no chain target runs Diff on `schedule`: every schedule has one ([`CHAIN_TARGETS`]).
#[cfg(feature = "sut")]
pub fn diff_target(schedule: Schedule) -> ChainTarget {
    chain_target(Mode::Diff, schedule, ChainPlant::None)
        .unwrap_or_else(|| panic!("no Diff chain target on {}", schedule.name()))
}

/// The Diff target on `schedule`'s start: [`start_target`] for [`diff_target`].
#[cfg(feature = "sut")]
pub fn start_diff(pin: Pin, schedule: Schedule) {
    start_target(pin, diff_target(schedule));
}

/// The Diff target on `schedule`'s comparison: [`fuzz_target`] for [`diff_target`].
#[cfg(feature = "sut")]
pub fn fuzz_one_diff(data: &[u8], pin: Pin, schedule: Schedule) -> Compared {
    fuzz_target(data, pin, diff_target(schedule))
}

/// A chain target's comparison: the input decoded with the target's encoding and run in its
/// session. A tie discard is [`Outcome::Discarded`], an unexpected reply stops the input
/// (counted in the census, logged and kept), and a shim fault or a replay that differs is a
/// divergence. In Diff mode the first plane where the systems differ is the divergence, and an
/// op the node does not serve yet makes the input out of domain.
pub fn fuzz_target(data: &[u8], pin: Pin, target: ChainTarget) -> Compared {
    EXECUTIONS.set(EXECUTIONS.get() + 1);
    if data.len() > MAX_INPUT {
        return Ok(Outcome::OutOfDomain);
    }
    let program = Program::decode(data, target.encoding);
    SESSION.with_borrow_mut(|slot| {
        let session = session_on(slot, pin, target).map_err(|fault| {
            // D8's per-leaf report (or any start failure) once: every later input fails the same.
            static REPORTED: std::sync::Once = std::sync::Once::new();
            REPORTED.call_once(|| {
                eprintln!("{}: the session did not start: {}", target.name, fault.0);
            });
            shim()
        })?;
        let runner = &mut session.runner;
        let Ok(run) = runner.run(&program) else {
            // The session is broken (oracle.h): the next input starts a new one.
            give_up(slot);
            return Err(shim());
        };
        let mut verdict = match &run.end {
            End::Done => Ok(Outcome::BothAccept),
            End::Tainted(..) => Ok(Outcome::Discarded),
            End::Unexpected(what) => {
                kept_unexpected(data, what, target);
                Ok(Outcome::OutOfDomain)
            }
            End::Unsupported(_) => Ok(Outcome::OutOfDomain),
            End::Diverged(d) => Err(*d),
        };
        if replayed(data) {
            runner.totals.replays += 1;
            let Ok(again) = runner.run(&program) else {
                give_up(slot);
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
    use super::{
        CHAIN_TARGETS, DIFF_CHAIN, HARNESS_SITES, TieSite, WARM_KEYS, input_hash, replayed,
        warm_fold, warm_order,
    };
    use super::{Census, End, SESSION, Totals, fold_into, session_target, stats_name};
    use super::{ChainPlant, ChainTarget};
    #[cfg(feature = "sut")]
    use super::{Mode, Pin, Schedule, chain_target, diff_target};

    /// D39: each chain target writes its own census file, `<target>.stats.json`, which names the
    /// target and its encoding; `diff_chain`'s keeps its name.
    #[test]
    fn each_target_has_its_own_stats_file() {
        assert_eq!(stats_name(DIFF_CHAIN), "diff_chain.stats.json");
        let mut names: Vec<String> = CHAIN_TARGETS.iter().map(|&t| stats_name(t)).collect();
        for (t, name) in CHAIN_TARGETS.iter().zip(&names) {
            assert_eq!(*name, format!("{}.stats.json", t.name));
            let mut unfolded = vec![(Census::default(), End::Done)];
            let text = fold_into(&mut Totals::default(), &mut unfolded, *t, &Default::default());
            let census: serde_json::Value = serde_json::from_str(&text).expect("JSON");
            assert_eq!((&census["target"], &census["inputs"]), (&t.name.into(), &1.into()));
            assert_eq!(census["encoding"], t.encoding.name());
        }
        names.sort();
        names.dedup();
        assert_eq!(names.len(), CHAIN_TARGETS.len());
    }

    /// D39: one session per `(pin, mode, schedule, plant)`. Each target has its own key, Diff
    /// runs on every schedule, `diff_chain` keeps Core alone on S0 with `chain-v1`, and a planted
    /// search never shares the session of the unplanted target on its schedule (O2b-4 4b).
    #[cfg(feature = "sut")]
    #[test]
    fn chain_targets_have_their_own_sessions() {
        use crate::chain::intent::Encoding;
        let name = |m, s| chain_target(m, s, ChainPlant::None).map(|t| t.name);
        assert_eq!(name(Mode::CoreOnly, Schedule::S0), Some("diff_chain"));
        assert_eq!(name(Mode::Diff, Schedule::S0), Some("diff_chain_s0"));
        assert_eq!(name(Mode::Diff, Schedule::S1), Some("diff_chain_par"));
        assert_eq!(name(Mode::Diff, Schedule::S2), Some("diff_chain_av"));
        assert_eq!(name(Mode::CoreOnly, Schedule::S1), None);
        assert_eq!(name(Mode::CoreOnly, Schedule::S2), None);
        assert_eq!(diff_target(Schedule::S1).schedule, Schedule::S1);
        assert_eq!(diff_target(Schedule::S1).name, "diff_chain_par");
        assert_eq!(diff_target(Schedule::S0).name, "diff_chain_s0");
        #[cfg(feature = "planted")]
        {
            let planted = |s, p| chain_target(Mode::Diff, s, p).map(|t| t.name);
            assert_eq!(planted(Schedule::S1, ChainPlant::P3a), Some("diff_chain_par_p3a"));
            assert_eq!(planted(Schedule::S0, ChainPlant::P3b), Some("diff_chain_p3b"));
        }
        for (i, a) in CHAIN_TARGETS.iter().enumerate() {
            assert_eq!(a.encoding == Encoding::V1, a.mode == Mode::CoreOnly, "{}", a.name);
            for b in &CHAIN_TARGETS[i + 1..] {
                assert_ne!(a.key(Pin::V24_0), b.key(Pin::V24_0), "{} and {}", a.name, b.name);
            }
            assert_eq!(a.key(Pin::V24_0), (Pin::V24_0, a.mode, a.schedule, a.plant));
        }
    }

    /// The plant a chain target's name gives it: `_p3a` and `_p3b` name a planted search's bug.
    fn named_plant(name: &str) -> ChainPlant {
        if name.ends_with("_p3a") {
            ChainPlant::P3a
        } else if name.ends_with("_p3b") {
            ChainPlant::P3b
        } else {
            ChainPlant::None
        }
    }

    /// BL-223 (OD-L3-98): a Diff runner's node keeps no disconnect-pool call from an earlier
    /// input. Over 60 generated `chain-v2` inputs, each run twice in a row, the log after the first
    /// run equals the log after the second: it holds that input's calls and nothing the input
    /// before it left, so its length is bounded by one input's calls. Each input's second run is
    /// left in the log for the next input's first to find, which a runner that kept the log would
    /// show. An input at the reset's clock starts from the image the reset prepared, whose own
    /// calls, if any, are dropped with the rest: no judgement reads the log.
    #[cfg(feature = "sut")]
    #[test]
    fn a_diff_input_starts_with_an_empty_pool_log() {
        use crate::chain::intent::{Encoding, Program};
        let mut runner =
            super::Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::Diff).expect("the session");
        let mut x: u64 = 7;
        let mut next = move || {
            x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        let (mut with_calls, mut left_behind, mut longest) = (0, 0, 0);
        let mut left = 0;
        for _ in 0..60 {
            let len = 16 + usize::try_from(next() % 385).expect("below 385");
            let bytes: Vec<u8> = (0..len).map(|_| next().to_le_bytes()[0]).collect();
            let program = Program::decode(&bytes, Encoding::V2);
            let pools = |runner: &super::Runner| {
                runner.diff.as_ref().expect("Diff mode").pools().take_calls()
            };
            runner.run(&program).expect("the shim");
            let first = pools(&runner);
            runner.run(&program).expect("the shim");
            let second = pools(&runner);
            assert_eq!(first, second, "no earlier input's call\n{program}");
            runner.run(&program).expect("the shim");
            left_behind += usize::from(left > 0);
            left = second.len();
            with_calls += usize::from(!second.is_empty());
            longest = longest.max(second.len());
        }
        assert!(with_calls >= 5, "inputs that log calls: {with_calls}");
        assert!(left_behind >= 5, "inputs after one that left calls: {left_behind}");
        assert!(longest > 0);
    }

    /// `node-l1/chain-encodings.toml` lists exactly [`CHAIN_TARGETS`], in order, and node-l1's
    /// fuzz `TARGETS` hold the same chain targets. Without `planted` the file's planted rows
    /// ([`PLANTED_CHAIN_TARGETS`]) are not compiled; with it, each of those names is a target
    /// with its bug and no other target has one (O2b-4 4b).
    ///
    /// [`PLANTED_CHAIN_TARGETS`]: crate::chain::intent::PLANTED_CHAIN_TARGETS
    #[cfg(feature = "sut")]
    #[test]
    fn chain_encodings_match_the_targets() {
        use crate::chain::intent::PLANTED_CHAIN_TARGETS;
        let mut listed = crate::chain::intent::chain_encodings();
        if cfg!(not(feature = "planted")) {
            listed.retain(|(name, _)| !PLANTED_CHAIN_TARGETS.contains(name));
        }
        let compiled: Vec<_> = CHAIN_TARGETS.iter().map(|t| (t.name, t.encoding)).collect();
        assert_eq!(listed, compiled);
        for t in CHAIN_TARGETS {
            let planted = PLANTED_CHAIN_TARGETS.contains(&t.name);
            assert_eq!(t.plant != ChainPlant::None, planted, "{}: {:?}", t.name, t.plant);
        }
        #[cfg(feature = "planted")]
        for name in PLANTED_CHAIN_TARGETS {
            let t = CHAIN_TARGETS.iter().find(|t| t.name == name);
            assert!(t.is_some_and(|t| t.plant != ChainPlant::None), "{name}: a planted target");
        }
        let fuzz: Vec<&str> =
            crate::TARGETS.iter().map(|t| t.name).filter(|n| n.starts_with("diff_chain")).collect();
        assert_eq!(fuzz, CHAIN_TARGETS.iter().map(|t| t.name).collect::<Vec<_>>());
    }

    /// Review r1 m3: each chain row of `TARGETS` reaches the chain target of its own name, through
    /// its start and through its comparison, so a row wired to another schedule's target fails.
    /// The session's target carries the row's plant (O2b-4 4b): a planted row wired to the other
    /// plant, or to none, fails.
    #[test]
    fn each_fuzz_target_runs_its_own_chain_target() {
        let rows: Vec<_> =
            crate::TARGETS.iter().filter(|t| t.name.starts_with("diff_chain")).collect();
        assert_eq!(rows.len(), CHAIN_TARGETS.len());
        let own = |name: &str| -> ChainTarget {
            *CHAIN_TARGETS.iter().find(|t| t.name == name).expect("a chain target")
        };
        for row in rows {
            let start = row.start.expect("a chain target starts its session");
            launched(own(row.name), None, None);
            SESSION.with_borrow_mut(|slot| *slot = None);
            start(super::Pin::V24_0);
            assert_eq!(session_target(), Some(own(row.name)), "{}'s start", row.name);
            SESSION.with_borrow_mut(|slot| *slot = None);
            (row.compare)(&[], super::Pin::V24_0).expect("an empty input");
            let session = session_target().expect("a session");
            assert_eq!(session, own(row.name), "{}'s comparison", row.name);
            assert_eq!(session.plant, named_plant(row.name), "{}'s plant", row.name);
        }
        SESSION.with_borrow_mut(|slot| *slot = None);
    }

    /// This thread's launch for `target`: its own encoding, run and pin, and `phase`'s census to
    /// `stats`.
    fn launched(
        target: super::ChainTarget,
        phase: Option<&str>,
        stats: Option<std::path::PathBuf>,
    ) {
        super::LAUNCH.set(Some(Box::leak(Box::new(super::Launch {
            encoding: Some(target.encoding.name().into()),
            phase: phase.map(Into::into),
            stats,
            run: Some("20261108T000000Z-v24.0-7".into()),
            pin: Some("v24.0".into()),
        }))));
        super::WRITER.set(None);
        super::EXECUTIONS.set(0);
    }

    /// `diff_chain`'s comparison of `input`, in this thread's session.
    fn compared(input: &[u8]) {
        super::fuzz_target(input, super::Pin::V24_0, DIFF_CHAIN).expect("the shim");
    }

    fn read(path: &std::path::Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).expect("the census")).expect("JSON")
    }

    /// The exit fold: this thread's session goes as it does at the process's exit.
    fn exit() {
        SESSION.with_borrow_mut(Option::take);
    }

    /// O2b-4 §4.6 (4e2 test 1): under a phase the census goes only to its destination, written
    /// beside it and renamed over it, naming the target, schedule, phase, run, pin, encoding and
    /// executions, with nothing left beside it.
    #[test]
    fn phase_census_is_written_by_rename_with_provenance() {
        let dir = tempfile::tempdir().expect("a directory");
        let dest = dir.path().join("census.json");
        launched(DIFF_CHAIN, Some("pre-replay"), Some(dest.clone()));
        compared(&[]);
        compared(&[0]);
        let census = read(&dest);
        for (key, want) in [
            ("target", serde_json::Value::from("diff_chain")),
            ("schedule", "S0".into()),
            ("phase", "pre-replay".into()),
            ("run", "20261108T000000Z-v24.0-7".into()),
            ("pin", "v24.0".into()),
            ("encoding", "chain-v1".into()),
            ("executions", 2.into()),
            ("complete", false.into()),
        ] {
            assert_eq!(census[key], want, "{key}");
        }
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .expect("the directory")
            .map(|e| e.expect("an entry").file_name())
            .collect();
        assert_eq!(names, ["census.json"]);
        exit();
    }

    /// 4e2 test 2: only the exit fold sets `complete`, and `executions` counts every comparison,
    /// one over `MAX_INPUT` too, which `inputs` does not.
    #[test]
    fn only_the_exit_fold_sets_complete() {
        let dir = tempfile::tempdir().expect("a directory");
        let dest = dir.path().join("census.json");
        launched(DIFF_CHAIN, Some("pre-replay"), Some(dest.clone()));
        compared(&[]);
        compared(&[0]);
        assert_eq!(read(&dest)["complete"], false, "the warm's fold");
        compared(&vec![0; super::MAX_INPUT + 1]);
        exit();
        let census = read(&dest);
        assert_eq!(census["complete"], true, "the exit fold");
        assert_eq!((&census["executions"], &census["inputs"]), (&3.into(), &2.into()));
    }

    /// 4e2 test 5: a session given up after a shim fault fails its phase census: the census is
    /// removed, the marker left, and no later session of the process writes one.
    #[test]
    fn a_replaced_session_marks_the_phase_census_failed() {
        let dir = tempfile::tempdir().expect("a directory");
        let dest = dir.path().join("census.json");
        launched(DIFF_CHAIN, Some("pre-replay"), Some(dest.clone()));
        compared(&[0]);
        assert!(dest.exists());
        SESSION.with_borrow_mut(super::give_up);
        assert!(!dest.exists(), "removed");
        assert!(dir.path().join(super::super::census::FAILED_MARKER).exists(), "marked");
        compared(&[0]);
        exit();
        assert!(!dest.exists(), "nothing written after the failure");
    }

    /// Review r1 B1 (a): a thread whose first touch is its session, as a fuzz process's start is,
    /// folds at its exit: the census is the exit fold's, with both comparisons.
    #[test]
    fn a_thread_that_starts_with_its_session_folds_at_its_exit() {
        let dir = tempfile::tempdir().expect("a directory");
        let dest = dir.path().join("census.json");
        let stats = dest.clone();
        std::thread::spawn(move || {
            super::start_target(super::Pin::V24_0, DIFF_CHAIN);
            launched(DIFF_CHAIN, Some("pre-replay"), Some(stats));
            compared(&[]);
            compared(&[0]);
        })
        .join()
        .expect("the thread");
        let census = read(&dest);
        assert_eq!((&census["complete"], &census["executions"]), (&true.into(), &2.into()));
    }

    /// This test binary, to run its ignored test `name` alone with its output not captured, in a
    /// clean chain environment.
    fn child(name: &str) -> std::process::Command {
        let module = module_path!().split_once("::").expect("a crate's module").1;
        let mut child = std::process::Command::new(std::env::current_exe().expect("the binary"));
        child.args(["--exact", &format!("{module}::{name}"), "--ignored", "--nocapture"]);
        for var in [
            "NODE_L1_CHAIN_ENCODING",
            "NODE_L1_CHAIN_PHASE",
            "NODE_L1_CHAIN_STATS",
            "NODE_FUZZ_RUN",
            "NODE_FUZZ_PIN",
        ] {
            child.env_remove(var);
        }
        child
    }

    /// Names the chain target [`exit_fold_child`] runs.
    const EXIT_FOLD_CHILD: &str = "NODE_L1_TEST_EXIT_FOLD_CHILD";

    /// [`the_exit_fold_runs_when_a_process_exits`]'s child: a chain fuzz target's process as
    /// libFuzzer runs it, its start read from the environment, two inputs, then `exit`.
    #[test]
    #[ignore = "the child of the_exit_fold_runs_when_a_process_exits"]
    fn exit_fold_child() {
        let Some(name) = std::env::var_os(EXIT_FOLD_CHILD) else {
            return;
        };
        let target = *CHAIN_TARGETS.iter().find(|t| *t.name == *name).expect("a chain target");
        super::start_fuzz_target(super::Pin::V24_0, target);
        for input in [&[][..], &[0]] {
            super::fuzz_target(input, super::Pin::V24_0, target).expect("the shim");
        }
        std::process::exit(0);
    }

    /// Review r1 B1 (b), r2 n1: a process that exits after its inputs exits 0 with its census
    /// complete, for the chain-v1 target and, with `sut`, a chain-v2 one (the floor gate's).
    #[test]
    fn the_exit_fold_runs_when_a_process_exits() {
        const EXITING: &[ChainTarget] = &[
            DIFF_CHAIN,
            #[cfg(feature = "sut")]
            super::DIFF_CHAIN_S0,
        ];
        for target in EXITING {
            let dir = tempfile::tempdir().expect("a directory");
            let dest = dir.path().join("census.json");
            let out = child("exit_fold_child")
                .env(EXIT_FOLD_CHILD, target.name)
                .env("NODE_L1_CHAIN_ENCODING", target.encoding.name())
                .env("NODE_L1_CHAIN_PHASE", "pre-replay")
                .env("NODE_L1_CHAIN_STATS", &dest)
                .output()
                .expect("the child");
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "{}: {}\n{stderr}", target.name, out.status);
            let census = read(&dest);
            assert_eq!(
                (&census["complete"], &census["executions"]),
                (&true.into(), &2.into()),
                "{}",
                target.name
            );
        }
    }

    /// Names the `TARGETS` row [`start_child`] starts.
    const START_CHILD: &str = "NODE_L1_TEST_START_CHILD";

    /// [`each_chain_row_refuses_another_encoding_at_its_start`]'s child: a `TARGETS` row's start.
    #[test]
    #[ignore = "the child of each_chain_row_refuses_another_encoding_at_its_start"]
    fn start_child() {
        let Some(name) = std::env::var_os(START_CHILD) else {
            return;
        };
        let row = crate::TARGETS.iter().find(|t| *t.name == *name).expect("a row");
        (row.start.expect("a start"))(super::Pin::V24_0);
        std::process::exit(0);
    }

    /// Review r1 m1: each chain row of `TARGETS`, in a process of its own, aborts at its start,
    /// naming itself, when the encoding variable is unset or names the other encoding, and starts
    /// under its own.
    #[test]
    fn each_chain_row_refuses_another_encoding_at_its_start() {
        use std::os::unix::process::ExitStatusExt;
        /// Linux's.
        const SIGABRT: i32 = 6;
        let rows: Vec<_> =
            crate::TARGETS.iter().filter(|t| t.name.starts_with("diff_chain")).collect();
        assert_eq!(rows.len(), CHAIN_TARGETS.len());
        for row in rows {
            let target = CHAIN_TARGETS.iter().find(|t| t.name == row.name).expect("a chain target");
            let own = target.encoding.name();
            let other = if own == "chain-v1" {
                "chain-v2"
            } else {
                "chain-v1"
            };
            for encoding in [None, Some(other), Some(own)] {
                let mut child = child("start_child");
                child.env(START_CHILD, row.name);
                if let Some(encoding) = encoding {
                    child.env("NODE_L1_CHAIN_ENCODING", encoding);
                }
                let out = child.output().expect("the child");
                let stderr = String::from_utf8_lossy(&out.stderr);
                let what = format!("{} under {encoding:?}: {}\n{stderr}", row.name, out.status);
                if encoding == Some(own) {
                    assert!(out.status.success(), "{what}");
                    assert!(!stderr.contains("refusing to run"), "{what}");
                } else {
                    assert_eq!(out.status.signal(), Some(SIGABRT), "{what}");
                    assert!(stderr.contains(&format!("{}: refusing to run: ", row.name)), "{what}");
                }
            }
        }
    }

    /// 4e2 test 6: without a phase the stats file is today's (O2b §6.5, 4a's target and
    /// encoding) plus `executions`, written in place, with no phase key.
    #[test]
    fn without_a_phase_the_stats_file_is_todays_plus_encoding_and_executions() {
        let dir = tempfile::tempdir().expect("a directory");
        let dest = dir.path().join("diff_chain.stats.json");
        launched(DIFF_CHAIN, None, Some(dest.clone()));
        compared(&[0]);
        exit();
        let census = read(&dest);
        let keys: Vec<&str> =
            census.as_object().expect("an object").keys().map(String::as_str).collect();
        let mut want = vec![
            "avoided",
            "cl_enforce",
            "deferred_restarts",
            "discard_ops",
            "discard_rate",
            "discard_sites",
            "discarded",
            "encoding",
            "executions",
            "inputs",
            "ops",
            "rejects",
            "replay_mismatches",
            "replays",
            "rows",
            "site13",
            "target",
            "unexpected",
        ];
        want.sort_unstable();
        assert_eq!(keys, want);
        assert_eq!((&census["encoding"], &census["executions"]), (&"chain-v1".into(), &1.into()));
    }

    /// 4e2 tests 4 and 7 (G-ENC-5's abort case, at the library's level): a chain target's start
    /// refuses an encoding variable that is unset, empty or another encoding's, and a phase that
    /// is not one or has no census file; each target starts under its own.
    #[test]
    fn the_encoding_check_refuses_a_missing_or_wrong_variable() {
        use crate::chain::intent::check_env;
        for t in CHAIN_TARGETS {
            let own = t.encoding.name();
            assert_eq!(check_env(t.name, Some(own)), Ok(()), "{}", t.name);
            let other = if own == "chain-v1" {
                "chain-v2"
            } else {
                "chain-v1"
            };
            for bad in [None, Some(""), Some(other), Some("chain-v3"), Some(" chain-v2")] {
                let why = check_env(t.name, bad).expect_err("refused");
                assert!(why.contains(own) && why.contains(t.name), "{why}");
            }
        }
        assert!(check_env("diff_tx_decode", Some("chain-v1")).is_err(), "not a chain target");
        let launch = |encoding: Option<&str>| super::Launch {
            encoding: encoding.map(Into::into),
            ..Default::default()
        };
        assert!(launch(None).check(DIFF_CHAIN).is_err());
        assert!(launch(Some("chain-v2")).check(DIFF_CHAIN).is_err());
        assert_eq!(launch(Some("chain-v1")).check(DIFF_CHAIN), Ok(()));
    }

    /// 4e2 test 4: a phase without a census file to go to, or one that is not a phase, refuses
    /// the start; a phase with its file, or none, starts.
    #[test]
    fn phase_without_a_stats_path_refuses_to_start() {
        let launch = |phase: Option<&str>, stats: Option<&str>| super::Launch {
            encoding: Some("chain-v1".into()),
            phase: phase.map(Into::into),
            stats: stats.map(Into::into),
            ..Default::default()
        };
        let why = launch(Some("pre-replay"), None).check(DIFF_CHAIN).expect_err("refused");
        assert!(why.contains("NODE_L1_CHAIN_STATS"), "{why}");
        assert!(launch(Some("during"), Some("/x")).check(DIFF_CHAIN).is_err());
        for phase in ["pre-replay", "post-replay"] {
            assert_eq!(launch(Some(phase), Some("/x")).check(DIFF_CHAIN), Ok(()));
        }
        assert_eq!(launch(None, None).check(DIFF_CHAIN), Ok(()));
    }

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
            (1..=14).map(TieSite::Core).chain(HARNESS_SITES).collect::<Vec<_>>()
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

    /// S2's rows (O2b-4 4d r1 m2, m3): `av_checked_above` scores a script failure above the
    /// assumevalid block only on that failure's own reject (or `-par=4`'s check-queue verdict),
    /// not on another reason; a defect sent under a best header the model does not know scores
    /// `av_best_unknown`.
    #[cfg(feature = "sut")]
    #[test]
    fn s2_rows_score_only_what_they_name() {
        use super::{Planned, Rows, Step, What};
        use crate::chain::mutate::{BANK, OPERATORS, chain_operator};
        use dash_oracle_sys::{ChainOp, FlushCounts, Post, Reply};

        let reply = |reason: Option<&str>| Step {
            reply: Reply {
                checked: Vec::new(),
                outcome: Ok(reason.map_or(serde_json::Value::Null, serde_json::Value::from)),
            },
            records: Vec::new(),
            events: Vec::new(),
            post: Post {
                down: true,
                flush_delta: FlushCounts::default(),
                tie: Vec::new(),
                persisted: None,
                tip: None,
                best_header: None,
                best_invalid: None,
                candidates: None,
                utxo: None,
                index: None,
                flushes: None,
                ibd: None,
                best_cl: None,
                sporks: None,
                tips: None,
                deployments: None,
            },
        };
        let mut w = crate::chain::av::world();
        let line = w.line.expect("S2's line");
        let script_false = (0..OPERATORS.len() + BANK.len())
            .find(|&m| chain_operator(m).name == "script_false")
            .expect("script_false is a chain operator");
        let above = line.av + 1;
        w.blocks[above].mutation = Some(script_false);
        let row = |w: &crate::chain::world::World, id: usize, reason: Option<&str>, name: &str| {
            let mut census = Census::default();
            let p = Planned::new(ChainOp::Block(Vec::new()), What::Block(id));
            Rows::new(Schedule::S2).see(w, &p, &reply(reason), &mut census);
            census.rows.get(name).copied().unwrap_or(0)
        };
        let checked = "av_checked_above";
        assert_eq!(row(&w, above, Some("mandatory-script-verify-flag-failed (x)"), checked), 1);
        assert_eq!(row(&w, above, Some("block-validation-failed"), checked), 1);
        assert_eq!(row(&w, above, Some("bad-prevblk"), checked), 0, "another reason");
        assert_eq!(row(&w, above, Some("duplicate-invalid"), checked), 0, "another reason");

        let defect = line.defects[0];
        assert_eq!(row(&w, defect, None, "av_best_unknown"), 0, "the best header is known");
        w.best_header = None;
        assert_eq!(row(&w, defect, None, "av_best_unknown"), 1);
    }
}
