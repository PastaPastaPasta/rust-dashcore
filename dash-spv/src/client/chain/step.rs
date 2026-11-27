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
/// once, at every later wait too; when two such checks in a row find none, every open pump is
/// closed, parked or not, since a pump woken on a refilled slot but not yet run still reads as
/// parked. The closes are logged when the wait drops, after the step's hold.
///
/// The slice that spends the time is clipped to what is left, so the first check with the time
/// spent comes at `time` plus that park's oversleep. After it, a slice is parked only at a check
/// that closes nothing right after one that closed some, and every open pump is closed at its
/// end unless one has started running; so the step parks at most one slice past `time` when no
/// pump starts running meanwhile, and one per pump open at that first check in all, each slice
/// plus its oversleep.
pub(super) struct StepWait<'a> {
    parker: &'a Parker,
    subs: Option<(&'a SubTable, ChainGen)>,
    budget: LagBudget,
    parked: Duration,
    /// The last check with the time spent closed nothing.
    fruitless: bool,
    closed: Vec<(SubKind, &'static str)>,
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
            fruitless: false,
            closed: Vec::new(),
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
            fruitless: false,
            closed: Vec::new(),
        }
    }

    /// Parks a slice, or none once the time is spent and a pump can be closed for it; at the
    /// wake, the event limit, then the time limit. The event limit counts only at a slice that
    /// ran out: a wake by a pump making room shows the pumps moving.
    fn wait(&mut self) {
        let Some((table, gen)) = self.subs else {
            self.parker.park();
            return;
        };
        if self.parked >= self.budget.time && self.close_lagging(table.live(gen), false) {
            return;
        }
        let left = self.budget.time.saturating_sub(self.parked);
        let slice = if left.is_zero() {
            PARK_SLICE
        } else {
            left.min(PARK_SLICE)
        };
        let start = Instant::now();
        let woken = self.parker.park_timeout(slice);
        let d = start.elapsed();
        self.parked += d;
        #[cfg(test)]
        table.lag.parked(d);
        self.close_lagging(table.live(gen), !woken);
    }

    /// Closes the open pumps that trail the most advanced one by `events` when `events`, or,
    /// with the time spent, every open pump not parked, or every open pump after two checks in a
    /// row that closed nothing; whether it closed any.
    fn close_lagging(&mut self, live: Vec<SubEntry>, events: bool) -> bool {
        let open: Vec<(SubEntry, u64)> =
            live.into_iter().filter(|e| !e.closed()).map(|e| (e.clone(), e.popped())).collect();
        let ahead = open.iter().map(|(_, p)| *p).max().unwrap_or(0);
        let trailing = open.iter().filter(|(_, p)| events && ahead - p >= self.budget.events);
        let (limit, lagging): (&'static str, Vec<&SubEntry>) =
            match trailing.map(|(e, _)| e).collect::<Vec<_>>() {
                v if !v.is_empty() => ("events", v),
                _ if self.parked >= self.budget.time => {
                    let all = open.iter().map(|(e, _)| e);
                    match all.clone().filter(|e| !e.parked()).collect::<Vec<_>>() {
                        busy if busy.is_empty() && self.fruitless => ("time", all.collect()),
                        busy => ("time", busy),
                    }
                }
                _ => return false,
            };
        self.fruitless = lagging.is_empty();
        for e in &lagging {
            e.close();
            #[cfg(test)]
            self.subs.expect("a table").0.lag.closed(e.kind, limit);
            self.closed.push((e.kind, limit));
        }
        !lagging.is_empty()
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
