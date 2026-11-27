//! One header step (PLAN-r1 §2.2.1): dash-chain's SPV recipe (SB4 §9.1), run inside one store
//! job. Stages 1–3 compute and commit; stages 4–5 publish and drain.

use std::time::{Duration, Instant};

use dash_chain::{
    updated_block_tip_schedules, ApplyError, BlockId, BlockRef, ChainError, ChainLockRef,
    ClOutcome, Durability, FeedConfig, HeaderBatch, HeaderStop, Parker, PeerId, Publish,
    VirtualTip,
};
use dash_clock::NodeSeconds;

use super::score::{Refused, MAX_BLOCKS_TO_ANNOUNCE};
use super::sub::{SubEntry, SubKind, SubTable};
use super::{Chain, ChainGen, StoreHandle};
use crate::storage::store as spv_store;

/// How long one header step waits for its chain subscribers (PLAN-r1 §2.4.3). A subscriber
/// that holds the step up past either limit loses its feed and resyncs; lag is never an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LagBudget {
    /// The time the step's writer may spend parked for space, summed over the whole step.
    pub time: Duration,
    /// How many events a subscriber may trail the most advanced one by.
    pub events: u64,
}

impl LagBudget {
    /// 5 s, and twice the feeds' capacity.
    pub fn spv(feeds: &FeedConfig) -> LagBudget {
        LagBudget {
            time: Duration::from_secs(5),
            events: 2 * u64::from(feeds.capacity),
        }
    }
}

/// The longest one park of a step's writer: the event limit is checked after each that runs out.
pub(super) const PARK_SLICE: Duration = Duration::from_millis(50);

/// One header step's wait for subscriber space, under its [`LagBudget`]. At each wake: the event
/// limit, then the time limit. Once the time is spent, every open pump not parked is closed at
/// once, at every later wait too. The waker the writer runs as it pushes clears a pump's
/// `parked`, and a slot is in the lag table from the moment it goes live, so a pump the writer
/// waits on reads busy, except one that read its feed empty just before a push it then marks
/// parked over. For that one, the backstop (DEC-1947, DEC-1952): the parks past the spent time
/// share one slice, each asking what is left of it, woken or not; once it has run out with
/// nothing to close, every open pump is closed, parked ones too, once per step. A pump whose
/// slot is closing or stale is not counted, and a slot is closed and logged once, by whoever
/// closes it first. The closes are logged when the wait drops, after the step's hold.
///
/// The bound (DEC-1954, DEC-1966), for host-managed feeds, outside a concurrent
/// `stop_subscribers()` close: every live feed the writer waits on is a [`ChainSub`]'s, in the
/// lag table, as a [`SourceState::Live`](super::SourceState::Live)'s feeds are opaque outside
/// the crate. Each park before the backstop asks at most what is left to `time` and one slice,
/// so the step parks at most `time`, one slice and ε, ε the oversleep of the park that crosses
/// them. The backstop then closes every pump the writer waits on: a slot that goes live in the
/// drain after the release holds no events of this step, so the next retry finishes. The known
/// exception (S5d3): a stop that has claimed a slot but not yet deregistered it hides the slot
/// from the backstop until the stop finishes. Only a feed outside the table, which the crate's
/// own tests take, is waited on past it, in fresh slices.
///
/// [`ChainSub`]: super::ChainSub
pub(super) struct StepWait<'a> {
    parker: &'a Parker,
    subs: Option<(&'a SubTable, ChainGen)>,
    budget: LagBudget,
    parked: Duration,
    closed: Vec<(SubKind, &'static str)>,
    backstop: bool,
}

impl<'a> StepWait<'a> {
    pub(super) fn new(
        parker: &'a Parker,
        subs: &'a SubTable,
        gen: ChainGen,
        budget: LagBudget,
    ) -> Self {
        #[cfg(test)]
        subs.lag.step();
        Self {
            parker,
            subs: Some((subs, gen)),
            budget,
            parked: Duration::ZERO,
            closed: Vec::new(),
            backstop: false,
        }
    }

    /// A wait with no subscribers to close: it parks until woken.
    #[cfg(test)]
    pub(super) fn bare(parker: &'a Parker) -> Self {
        Self {
            parker,
            subs: None,
            budget: LagBudget {
                time: Duration::MAX,
                events: u64::MAX,
            },
            parked: Duration::ZERO,
            closed: Vec::new(),
            backstop: false,
        }
    }

    /// Parks a slice, or none once the time is spent and a pump can be closed for it, or once the
    /// slice past it has run out; at the wake, the event limit, then the time limit. The event
    /// limit counts only at a park that ran its slice out (DEC-1938): a wake by a pump making
    /// room shows the pumps moving, and a wait that parks nothing has seen no slice.
    fn wait(&mut self) {
        let Some((table, gen)) = self.subs else {
            self.parker.park();
            return;
        };
        #[cfg(test)]
        table.lag.waiting();
        if self.parked >= self.budget.time && self.close_lagging(table.live(gen), false) {
            return;
        }
        let slice = match self.budget.time.saturating_sub(self.parked) {
            left if !left.is_zero() => left.min(PARK_SLICE),
            _ if self.backstop => PARK_SLICE,
            _ => PARK_SLICE.saturating_sub(self.over()),
        };
        let woken = (!slice.is_zero()).then(|| {
            let start = Instant::now();
            #[cfg(test)]
            let charged = table.lag.parking(self.parked, slice, self.parker);
            let woken = self.parker.park_timeout(slice);
            let d = start.elapsed();
            #[cfg(test)]
            let d = charged.unwrap_or(d);
            self.parked += d;
            #[cfg(test)]
            table.lag.parked(slice, d, woken);
            woken
        });
        let live = table.live(gen);
        if self.close_lagging(live.clone(), woken == Some(false))
            || self.over() < PARK_SLICE
            || std::mem::replace(&mut self.backstop, true)
        {
            return;
        }
        #[cfg(test)]
        table.lag.backstop();
        let open: Vec<SubEntry> = live.into_iter().filter(SubEntry::open).collect();
        self.close(open.iter(), "time");
    }

    /// The time parked past `time`.
    fn over(&self) -> Duration {
        self.parked.saturating_sub(self.budget.time)
    }

    /// Closes the open pumps that trail the most advanced one by `events` when `events`, or,
    /// with the time spent, every open pump not parked; whether it closed any. A pump's `popped`
    /// is read before its slot is checked open, so each `seq` counted was popped from an open
    /// slot of this generation: the writer has pushed at least that far, and a pump trailing it
    /// by `events` trails the writer by as many, even if that slot retires just after.
    fn close_lagging(&mut self, live: Vec<SubEntry>, events: bool) -> bool {
        let open: Vec<(SubEntry, Option<u64>)> = live
            .into_iter()
            .map(|e| {
                let popped = e.popped();
                (e, popped)
            })
            .filter(|(e, _)| e.open())
            .collect();
        let ahead = open.iter().filter_map(|(_, p)| *p).max().unwrap_or(0);
        let trailing = open
            .iter()
            .filter(|(_, p)| events && p.is_some_and(|p| ahead - p >= self.budget.events));
        let (limit, lagging): (&'static str, Vec<&SubEntry>) =
            match trailing.map(|(e, _)| e).collect::<Vec<_>>() {
                v if !v.is_empty() => ("events", v),
                _ if self.parked >= self.budget.time => {
                    ("time", open.iter().map(|(e, _)| e).filter(|e| !e.parked()).collect())
                }
                _ => return false,
            };
        self.close(lagging.iter().copied(), limit)
    }

    /// Closes and records each of `entries` this call closes first; whether it closed any.
    fn close<'e>(
        &mut self,
        entries: impl Iterator<Item = &'e SubEntry>,
        limit: &'static str,
    ) -> bool {
        let mut any = false;
        for e in entries.filter(|e| e.close()) {
            #[cfg(test)]
            self.subs.expect("a table").0.lag.closed(e.kind, limit);
            self.closed.push((e.kind, limit));
            any = true;
        }
        any
    }
}

impl Drop for StepWait<'_> {
    fn drop(&mut self) {
        for (kind, limit) in self.closed.drain(..) {
            tracing::info!("chain subscriber {kind:?}: feed closed ({limit}), resync");
        }
    }
}

/// What a step runs on.
pub(super) enum StepInput {
    /// `ProcessNewBlockHeaders`, then `EnforceBestChainLock` if a late header moved the floor.
    /// A message's first step carries its `lead`, Core's checks before the call.
    Headers {
        peer: PeerId,
        batch: HeaderBatch,
        lead: Option<Lead>,
    },
    /// A verified ChainLock: `ProcessNewChainLock`'s index side.
    ChainLock(ChainLockRef),
    /// The 5 s timer, and the check an `UpdatedBlockTip` scheduled: `CheckActiveState`, then
    /// `EnforceBestChainLock`.
    Tick,
}

/// The whole message a first step's batch leads.
#[derive(Clone, Copy, Debug)]
pub(super) struct Lead {
    pub(super) len: usize,
    pub(super) continuous: bool,
}

/// What a step did.
#[derive(Debug, Default)]
pub(super) struct StepOut {
    pub(super) stop: Option<HeaderStop>,
    pub(super) last_accepted: Option<BlockId>,
    pub(super) cl: Option<ClOutcome>,
    /// The batch was not handed to `accept`.
    pub(super) refused: Option<Refused>,
    /// An `UpdatedBlockTip` of this step passed Core's guards (`chainlock/handler.cpp:187-189`):
    /// the task runs one more `Tick` before its next input.
    pub(super) schedule: bool,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum StepError {
    #[error(transparent)]
    Chain(#[from] ChainError),
    #[error(transparent)]
    Store(#[from] spv_store::Error),
}

/// Stages 1–3 done: what stage 4 publishes, `None` when the step changed nothing.
pub(super) struct Committed {
    out: StepOut,
    publish: Option<(BlockRef, VirtualTip)>,
}

/// The whole step. Every error is fatal: before stage 4 nothing is published, and after a
/// sub-step moved the tip dash-chain has tripped the latch.
pub(super) fn step<H: StoreHandle>(
    c: &mut Chain<H>,
    h: &H,
    input: StepInput,
    wait: &mut StepWait<'_>,
    now: NodeSeconds,
) -> Result<StepOut, StepError> {
    let committed = commit(c, h, input)?;
    publish(c, committed, wait, now)
}

/// [`step`] with a bare wait.
#[cfg(test)]
pub(super) fn step_parked<H: StoreHandle>(
    c: &mut Chain<H>,
    h: &H,
    input: StepInput,
    parker: &Parker,
    now: NodeSeconds,
) -> Result<StepOut, StepError> {
    step(c, h, input, &mut StepWait::bare(parker), now)
}

/// Stages 1–3: the input against one `VirtualTip`, the fork target, and one Buffered commit
/// of the step's records, marks, ChainLock rows and tip. A step that moves nothing and
/// changes no record or ChainLock row commits nothing (SB4 n8).
pub(super) fn commit<H: StoreHandle>(
    c: &mut Chain<H>,
    h: &H,
    input: StepInput,
) -> Result<Committed, StepError> {
    let cl_before = c.cl_state();
    let mut vt = c.virtual_tip();
    let mut out = StepOut::default();
    match input {
        StepInput::Headers {
            peer,
            batch,
            lead,
        } => {
            out.refused = match lead {
                Some(lead) => refusal(c, &batch, lead)?,
                None => None,
            };
            if out.refused.is_some() {
                return Ok(Committed {
                    out,
                    publish: None,
                });
            }
            let r = c.accept(batch, Some(peer))?;
            (out.stop, out.last_accepted) = (r.stop, r.last_accepted);
            if r.floor_moved.is_some() {
                c.enforce_best_chainlock(&mut vt).map_err(applied)?;
            }
        }
        StepInput::ChainLock(cl) => {
            out.cl = Some(c.apply_chainlock(cl, &mut vt).map_err(applied)?);
        }
        StepInput::Tick => {
            c.check_cl_active_state();
            c.enforce_best_chainlock(&mut vt).map_err(applied)?;
        }
    }
    let target = c.fork_target(&vt)?;
    let e = c.reader().try_entry(target.id)?;
    #[cfg(test)]
    let e = e.filter(|_| !super::tests::driver::target_hidden());
    let e = e.ok_or(ChainError::Internal("the fork target is not held"))?;
    let to = BlockRef {
        id: e.id,
        hash: e.hash,
        height: e.height,
    };
    let (tip, _) = c.private_tip().ok_or(ChainError::Internal("a header step with no tip"))?;
    let moved = tip.id != to.id;
    if !moved && c.dirty_len() == 0 && c.cl_state() == cl_before {
        return Ok(Committed {
            out,
            publish: None,
        });
    }
    let mut step = h.store().begin_step()?;
    let tail = c.write_header_step(moved.then_some(to), &mut step)?;
    step.commit(&tail, Durability::Buffered)?;
    Ok(Committed {
        out,
        publish: Some((to, vt)),
    })
}

/// Stages 4–5: one publication (also for a status-only step), the committed records released,
/// and the fanout drained, waiting on `wait` while a subscriber lacks space.
pub(super) fn publish<H: StoreHandle>(
    c: &mut Chain<H>,
    committed: Committed,
    wait: &mut StepWait<'_>,
    now: NodeSeconds,
) -> Result<StepOut, StepError> {
    let Committed {
        mut out,
        publish,
    } = committed;
    let Some((to, vt)) = publish else {
        return Ok(out);
    };
    let sum = c.activate_segmented(to, &mut || wait.wait())?;
    c.records_committed();
    while c.retry_pending() == Publish::Blocked {
        wait.wait();
    }
    let ibd = c.is_ibd_private(now, false);
    out.schedule = updated_block_tip_schedules(sum.snap.tip.tip, Some(sum.fork.id), ibd)
        || vt.notified_tips().iter().any(|&t| updated_block_tip_schedules(t, None, ibd));
    Ok(out)
}

/// Core's checks before `ProcessNewBlockHeaders` (`net_processing.cpp:3341-3360`): a message
/// whose first header's parent is unknown, or whose headers do not chain, is not accepted.
fn refusal<H: StoreHandle>(
    c: &Chain<H>,
    batch: &HeaderBatch,
    lead: Lead,
) -> Result<Option<Refused>, ChainError> {
    let reader = c.reader();
    let Some(first) = batch.headers.first() else {
        return Ok(None);
    };
    if reader.try_lookup(&first.prev_hash())?.is_some() {
        return Ok((!lead.continuous).then_some(Refused::NonContinuous));
    }
    Ok(Some(if lead.len <= MAX_BLOCKS_TO_ANNOUNCE {
        Refused::Unconnecting {
            locator: reader.try_locator(Some(c.best_header()))?,
        }
    } else {
        Refused::UnconnectingBatch
    }))
}

fn applied(e: ApplyError<ChainError>) -> ChainError {
    match e {
        ApplyError::Chain(e) | ApplyError::Host(e) => e,
    }
}
