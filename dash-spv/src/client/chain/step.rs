//! One header step (PLAN-r1 §2.2.1): dash-chain's SPV recipe (SB4 §9.1), run inside one store
//! job. Stages 1–3 compute and commit; stages 4–5 publish and drain.

use dash_chain::{
    updated_block_tip_schedules, ApplyError, BlockId, BlockRef, ChainError, ChainLockRef,
    ClOutcome, Durability, HeaderBatch, HeaderStop, Parker, PeerId, Publish, VirtualTip,
};
use dash_clock::NodeSeconds;

use super::score::{Refused, MAX_BLOCKS_TO_ANNOUNCE};
use super::{Chain, StoreHandle};
use crate::storage::store as spv_store;

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
    parker: &Parker,
    now: NodeSeconds,
) -> Result<StepOut, StepError> {
    let committed = commit(c, h, input)?;
    publish(c, committed, parker, now)
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
/// and the fanout drained, parking on `parker` while a subscriber lacks space.
pub(super) fn publish<H: StoreHandle>(
    c: &mut Chain<H>,
    committed: Committed,
    parker: &Parker,
    now: NodeSeconds,
) -> Result<StepOut, StepError> {
    let Committed {
        mut out,
        publish,
    } = committed;
    let Some((to, vt)) = publish else {
        return Ok(out);
    };
    let sum = c.activate_segmented(to, &mut || parker.park())?;
    c.records_committed();
    while c.retry_pending() == Publish::Blocked {
        parker.park();
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
