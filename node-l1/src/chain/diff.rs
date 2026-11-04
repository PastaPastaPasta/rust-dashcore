//! Diff mode (O2b §4, O2b-3b): each planned op runs on `NodeSut` as well as on Core, and the two
//! steps go through [`compare`] with the golden keys left out of the record plane (D16). The
//! first difference ends the input as a [`Divergence`]; an op the node does not serve ends it
//! out of domain ([`Unsupported`]), counted in the census under its reason, never a divergence.
//!
//! One refusal is decided after the op, on both steps ([`judge`]): a **queue drain**, where both
//! sides queued the same number of signals, more than Core's queue holds before it may drain
//! mid-op, and the steps are otherwise the same (where Core's drain fell is not the node's to
//! say). Anything else is compared, so a queue count on one side only is reported as the
//! divergence it is. Until a7 and a10 no real node step compares the same (it has no records
//! and no flush counters), so the refusal cannot fire yet: such an op diverges, which fails
//! safe.
//!
//! V connects and disconnects through ND1's `CoreConnector` and flushes through `NodeFlush`
//! ([`super::node`]), so a reorg is compared plane by plane as any other op, and its undo bits
//! are in the `index` digest; its `utxo` is read from V's coins cache as the shim reads Core's.
//! What it lacks until CH3a: records until a7 (so a step whose Core records are not empty
//! differs in `records`), and the flush counters until a10-2. A difference in those planes
//! ([`GAPS`]) is the gaps', not a finding, and masks nothing:
//! the divergence is named by the first other plane that differs, if one does. Nothing is
//! excepted from the comparison for them.
//!
//! A library mode with its tests only: no fuzz target runs it (DEC-286). The restore step is not
//! compared yet: the runner keeps the node's restore report from a10-2 (BL-283, Q9).

use std::borrow::Cow;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use dash_oracle_sys::{ChainOp, Pin};
use dash_statedigest::compare::FieldId;
use dash_statedigest::{FIELDS, FieldSource, Kind};

use super::compare::{Skipped, differences};
use super::core::{Schedule, T_RECENT};
use super::node::{NodeSide, NodeStep};
use super::run::{Refused, Unsupported};
use super::step::Step;
use crate::Divergence;

/// `LimitValidationInterfaceQueue`'s bound: with more callbacks pending at an
/// `ActivateBestChain` iteration's start, Core drains its queue there. An extend connects one
/// block per iteration and queues two signals for it, so Core first drains at the seventh
/// block's iteration; a count above ten is therefore one block conservative for extends.
// Core: validation.cpp:3826-3831, :3870 (the drain), :3909, :3932 (the two signals)
pub const QUEUE_LIMIT: usize = 10;

/// The signals Core's validation interface queues (the rest run at their call sites).
// Core: validationinterface.cpp:215-227, :254-278, :309-316 at the pin
pub const QUEUED: [&str; 5] = [
    "UpdatedBlockTip",
    "BlockConnected",
    "BlockDisconnected",
    "ChainStateFlushed",
    "NotifyChainLock",
];

/// The planes V lacks until CH3a (records until a7; the flush counters until a10's tally). A
/// difference there masks none after it: [`judge`] names the first plane outside them that
/// differs, if one does.
pub const GAPS: [&str; 5] =
    ["records", "attempts", "sparse records", "post.flush_delta", "post.flushes"];

/// What the after-the-op refusal reads of Core's step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// The queued signals the op delivered.
    pub queued: usize,
}

/// What the after-the-op refusal reads of the node's step, and where its teardown ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeFacts {
    /// The queued signals the op delivered.
    pub queued: usize,
    /// The events an earlier V's teardown delivered (a `RESTART`'s or a restore's), heard
    /// before the new V started, which the step lists first as Core's does; 0 for any other op.
    pub teardown: usize,
    /// The events an earlier V's listener heard after the new V started, which Core never logs:
    /// the step diverges in its events ([`judge`]).
    pub late: usize,
}

impl Facts {
    /// Core's: its events.
    pub fn core(step: &Step) -> Facts {
        Facts {
            queued: step.events.iter().filter(|e| QUEUED.contains(&e.name.as_str())).count(),
        }
    }
}

/// The after-the-op refusal both sides' facts show: both queued the same count above
/// [`QUEUE_LIMIT`]. [`judge`] then checks the steps are otherwise the same; with no refusal
/// here, the steps are compared.
pub fn refused(node: &NodeFacts, core: &Facts) -> Option<Refused> {
    (node.queued > QUEUE_LIMIT && node.queued == core.queued).then_some(Refused::QueueDrain)
}

/// What the Diff runner makes of one op run on both sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Judgement {
    /// The steps are the same; what the record plane left out.
    Same(Skipped),
    /// Out of domain: both sides show the refusal, and they agree on what it leaves comparable.
    Refused(Refused),
    Diverged(Divergence),
}

/// One op's steps, Core's and the node's, judged. Where Core queued more than
/// [`QUEUE_LIMIT`] signals, the events after the teardown's are taken in the order a drain at
/// the op's end gives ([`as_judged`]): a drain inside the op only interleaves the two kinds. A
/// queue drain both sides' facts show ([`refused`]) stands only if the steps are otherwise the
/// same. Else the steps are compared, and a difference is named by the first plane outside
/// [`GAPS`] that differs, or else by the first gap. A node step with an earlier V's signal heard
/// late ([`NodeFacts::late`]) diverges in its events outright.
// Core: src/test/oracle/oracle_chain.cpp:906-910, :1000-1003 at the pin
pub fn judge(core: &Step, node: &NodeStep, exclude: &[FieldId]) -> Judgement {
    if node.facts.late > 0 {
        return Judgement::Diverged(Divergence {
            field: "events",
        });
    }
    let core_facts = Facts::core(core);
    let (core, node_step) = as_judged(core, node);
    let (skipped, differ) = differences(&core, &node_step, exclude);
    let compared = match differ.iter().find(|f| !GAPS.contains(f)).or(differ.first()) {
        None => Judgement::Same(skipped),
        Some(&field) => Judgement::Diverged(Divergence {
            field,
        }),
    };
    match (refused(&node.facts, &core_facts), compared) {
        (Some(why), Judgement::Same(_)) => Judgement::Refused(why),
        (_, compared) => compared,
    }
}

/// Core's step and the node's as [`judge`] compares them. Past [`QUEUE_LIMIT`] queued signals
/// in Core's step, each step's events after the node's teardown ([`NodeFacts::teardown`]) in the
/// order a drain at the op's end gives ([`drained_at_end`]), the teardown's first as heard: Core
/// drains the old session's queue before the reopen, so they are exact on both sides. The prefix
/// covers what the node heard before its new V started; a teardown signal heard after that is
/// [`NodeFacts::late`], which [`judge`] holds diverged. Else both as their logs have them.
// Core: src/test/oracle/oracle_chain.cpp:899-910 (Teardown) at the pin
pub fn as_judged<'a>(core: &'a Step, node: &'a NodeStep) -> (Cow<'a, Step>, Cow<'a, Step>) {
    if Facts::core(core).queued > QUEUE_LIMIT {
        let kept = node.facts.teardown;
        (Cow::Owned(drained_at_end(core, kept)), Cow::Owned(drained_at_end(&node.step, kept)))
    } else {
        (Cow::Borrowed(core), Cow::Borrowed(&node.step))
    }
}

/// `step` with its events after the first `kept` as a drain at the op's end lists them: the
/// synchronous ones, then the queued ones, each subsequence in its own order. A step whose queue
/// drained only at the op's end is unchanged: its signals already come in that order.
pub fn drained_at_end(step: &Step, kept: usize) -> Step {
    let (head, rest) = step.events.split_at(kept.min(step.events.len()));
    let (queued, sync): (Vec<_>, Vec<_>) =
        rest.iter().cloned().partition(|e| QUEUED.contains(&e.name.as_str()));
    Step {
        events: [head.to_vec(), sync, queued].concat(),
        ..step.clone()
    }
}

/// The golden keys (O2b §4.2, D16): written by the golden extractor and the node's digest tap,
/// never by Core's patch, so left out of the record plane.
fn golden_keys() -> Vec<FieldId> {
    FIELDS
        .iter()
        .filter(|f| f.source == FieldSource::Golden)
        .filter(|f| matches!(f.kind, Kind::Connect | Kind::Reject | Kind::Disconnect))
        .filter_map(|f| FieldId::find(f.kind, f.class, f.key))
        .collect()
}

/// A private directory under the temporary directory, removed with everything in it on drop.
struct SessionDir(PathBuf);

impl SessionDir {
    fn new() -> Result<SessionDir, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("diff_chain-node.{}.{n}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(SessionDir(path))
    }
}

impl Drop for SessionDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The node's half of a Diff-mode runner.
pub struct Diff {
    side: NodeSide,
    /// The node holds the image as a reset left it, at [`T_RECENT`].
    ready: bool,
    golden: Vec<FieldId>,
    /// Declared last: removed after the node is gone.
    _dir: SessionDir,
}

impl Diff {
    /// The node's side on `schedule`: start it before Core's session (D8's check needs the
    /// oracle free on this thread).
    pub fn start(pin: Pin, schedule: Schedule) -> Result<Diff, String> {
        let dir = SessionDir::new()?;
        let (side, _) = NodeSide::start(pin, schedule, &dir.0)?;
        Ok(Diff {
            side,
            ready: false,
            golden: golden_keys(),
            _dir: dir,
        })
    }

    /// The image at [`T_RECENT`], as Core's reset leaves it.
    pub fn reset(&mut self) -> Result<(), String> {
        self.ready = false;
        self.side.restore(T_RECENT)?;
        self.ready = true;
        Ok(())
    }

    /// An input's start at `mocktime`: the reset's image if it is at that clock, else the image
    /// restored there.
    pub fn begin(&mut self, mocktime: i64) -> Result<(), String> {
        let ready = std::mem::take(&mut self.ready);
        if ready && mocktime == T_RECENT {
            Ok(())
        } else {
            self.side.restore(mocktime).map(drop)
        }
    }

    pub fn step(&mut self, op: &ChainOp) -> Result<NodeStep, Unsupported> {
        self.side.step(op)
    }

    /// Core's step against the node's ([`judge`], the golden keys left out of the record
    /// plane).
    pub fn judge(&self, core: &Step, node: &NodeStep) -> Judgement {
        judge(core, node, &self.golden)
    }
}

#[cfg(test)]
mod tests {
    use dash_oracle_sys::{Event, FlushCounts, Post, Reply};
    use serde_json::Value;

    use super::*;

    fn event(name: &str, block: u32) -> Event {
        Event {
            name: name.to_owned(),
            args: vec![Value::from(block)],
        }
    }

    /// A `RESTART`'s step with these events, the rest the same on both sides.
    fn restart(events: Vec<Event>) -> Step {
        Step {
            reply: Reply {
                checked: Vec::new(),
                outcome: Ok(Value::Null),
            },
            records: Vec::new(),
            events,
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
        }
    }

    const BLOCKS: u32 = 6;

    /// Each reconnect's synchronous `BlockChecked`.
    fn checked() -> impl Iterator<Item = Event> {
        (1..=BLOCKS).map(|b| event("BlockChecked", b))
    }

    /// Each reconnect's two queued signals.
    fn connected() -> impl Iterator<Item = Event> {
        (1..=BLOCKS).flat_map(|b| [event("BlockConnected", b), event("UpdatedBlockTip", b)])
    }

    /// Core's clean `RESTART` of [`BLOCKS`] reconnects (M-1's program): the teardown's
    /// `ChainStateFlushed`, drained before the reopen, then the load's signals as a drain at each
    /// `ActivateBestChain` iteration interleaves them, 13 queued in all.
    // Core: src/test/oracle/oracle_chain.cpp:899-910, validation.cpp:3826-3831 at the pin
    fn core_restart() -> Step {
        let flushed = event("ChainStateFlushed", 0);
        let load = (1..=BLOCKS).flat_map(|b| {
            [event("BlockChecked", b), event("BlockConnected", b), event("UpdatedBlockTip", b)]
        });
        let step = restart([flushed].into_iter().chain(load).collect());
        assert!(Facts::core(&step).queued > QUEUE_LIMIT);
        step
    }

    /// The node's restart step with `events`, `teardown` of them heard before the new V and
    /// `late` after it from the old V's listener.
    fn node(events: Vec<Event>, teardown: usize, late: usize) -> NodeStep {
        let queued = events.iter().filter(|e| QUEUED.contains(&e.name.as_str())).count();
        NodeStep {
            step: restart(events),
            facts: NodeFacts {
                queued,
                teardown,
                late,
            },
            proposal_class: None,
        }
    }

    /// M-1: past the queue bound the teardown's `ChainStateFlushed` stays first on both sides,
    /// and only the load's events take a drain's order. The node's (teardown, then its load's
    /// synchronous and queued signals) is Core's: the steps are the same, so the queue drain
    /// both sides show stands.
    #[test]
    fn an_over_limit_restart_keeps_the_teardown_first() {
        let flushed = event("ChainStateFlushed", 0);
        let node = node([flushed].into_iter().chain(checked()).chain(connected()).collect(), 1, 0);
        let core = core_restart();
        let (c, n) = as_judged(&core, &node);
        assert_eq!(differences(&c, &n, &[]).1, Vec::<&str>::new());
        assert_eq!(judge(&core, &node, &[]), Judgement::Refused(Refused::QueueDrain));
    }

    /// M-1's prefix mechanism (Opus r2 n-1): the teardown's count of events stays first, raw, on
    /// both sides, so a counted teardown `ChainStateFlushed` listed after the load's synchronous
    /// signals diverges where a drain's order over the whole step would hide it. The node never
    /// lists a counted teardown event late (its step opens with them); the realistic form is
    /// [`a_teardown_signal_heard_late_diverges`].
    #[test]
    fn a_teardown_signal_after_the_load_s_diverges() {
        let flushed = event("ChainStateFlushed", 0);
        let node = node(checked().chain([flushed]).chain(connected()).collect(), 1, 0);
        let judged = judge(&core_restart(), &node, &[]);
        assert_eq!(
            judged,
            Judgement::Diverged(Divergence {
                field: "events"
            })
        );
    }

    /// Opus r2 m-1: the old V's teardown `ChainStateFlushed` heard after the new V started, so
    /// counted late and not in the teardown, first among the queued signals. A drain's order
    /// over both steps would make them the same; Core never logs an old-session signal after the
    /// reopen, so the step diverges in its events.
    // Core: src/test/oracle/oracle_chain.cpp:906-910, :1000-1003 at the pin
    #[test]
    fn a_teardown_signal_heard_late_diverges() {
        let flushed = event("ChainStateFlushed", 0);
        let node = node(checked().chain([flushed]).chain(connected()).collect(), 0, 1);
        let judged = judge(&core_restart(), &node, &[]);
        assert_eq!(
            judged,
            Judgement::Diverged(Divergence {
                field: "events"
            })
        );
    }

    /// The queue refusal needs both sides above the bound with the same count.
    #[test]
    fn a_queue_drain_needs_the_same_count_on_both_sides() {
        let over = QUEUE_LIMIT + 2;
        let both = |n, c| {
            refused(
                &NodeFacts {
                    queued: n,
                    teardown: 0,
                    late: 0,
                },
                &Facts {
                    queued: c,
                },
            )
        };
        assert_eq!(both(over, over), Some(Refused::QueueDrain));
        assert_eq!(both(QUEUE_LIMIT, QUEUE_LIMIT), None);
        assert_eq!(both(over, QUEUE_LIMIT), None, "node only");
        assert_eq!(both(QUEUE_LIMIT, over), None, "Core only");
        assert_eq!(both(over, over + 2), None, "different counts");
    }
}
