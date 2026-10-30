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
//! differs in `records`), and the flush counters until a10. A difference in those planes
//! ([`GAPS`]) is the gaps', not a finding, and masks nothing:
//! the divergence is named by the first other plane that differs, if one does. Nothing is
//! excepted from the comparison for them.
//!
//! A library mode with its tests only: no fuzz target runs it (DEC-286). The restore step is not
//! compared: its reply is `RESTART`'s, which the node does not serve until a10.

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

/// The planes V lacks until CH3a (records until a7; the flush counters until a10). A difference
/// there masks none after it: [`judge`] names the first plane outside them that differs, if one
/// does.
pub const GAPS: [&str; 6] =
    ["records", "attempts", "sparse records", "post.flush_delta", "post.persisted", "post.flushes"];

/// What the after-the-op refusal reads of Core's step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// The queued signals the op delivered.
    pub queued: usize,
}

/// What the after-the-op refusal reads of the node's step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeFacts {
    /// The queued signals the op delivered.
    pub queued: usize,
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
/// [`QUEUE_LIMIT`] signals, its events are taken in the order a drain at the op's end gives
/// (the synchronous ones, then the queued ones, each in its own order): a drain inside the op
/// only interleaves the two. A queue drain both sides' facts show ([`refused`]) stands only if
/// the steps are otherwise the same. Else the steps are compared, and a difference is named by
/// the first plane outside [`GAPS`] that differs, or else by the first gap.
pub fn judge(core: &Step, node: &NodeStep, exclude: &[FieldId]) -> Judgement {
    let core_facts = Facts::core(core);
    let core = if core_facts.queued > QUEUE_LIMIT {
        Cow::Owned(drained_at_end(core))
    } else {
        Cow::Borrowed(core)
    };
    let (skipped, differ) = differences(&core, &node.step, exclude);
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

/// `step` with its events as a drain at the op's end lists them: the synchronous ones, then
/// the queued ones, each subsequence in its own order. A step whose queue drained only at the
/// op's end is unchanged: its signals already come in that order.
pub fn drained_at_end(step: &Step) -> Step {
    let (queued, mut events): (Vec<_>, Vec<_>) =
        step.events.iter().cloned().partition(|e| QUEUED.contains(&e.name.as_str()));
    events.extend(queued);
    Step {
        events,
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
            self.side.restore(mocktime)
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
    use super::*;

    /// The queue refusal needs both sides above the bound with the same count.
    #[test]
    fn a_queue_drain_needs_the_same_count_on_both_sides() {
        let over = QUEUE_LIMIT + 2;
        let both = |n, c| {
            refused(
                &NodeFacts {
                    queued: n,
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
