//! The header task: it owns the chain, runs its steps and the host's commands.

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dash_chain::{
    Anchor, AnchorState, BlockId, ChainLockRef, ChainReadSnapshot, ClOutcome, HeaderStop, Notify,
    Parker, PeerId,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::MissedTickBehavior;

use super::ingress::{HeaderIngress, IngressReply, Queued, INGRESS_CAP};
use super::no_wait::{self, SpvReply, SubError};
use super::score::{HeaderMisbehaviour, Refused, Unconnecting};
use super::status::{tip_ref, Asked, EstablishGuard, GuardedEstablish, HeaderSyncState, Round};
use super::step::{self, Lead, StepError, StepInput, StepOut};
use super::sub::SubTable;
use super::{
    chain_error, replace_chain, Chain, ChainGen, ChainHostSetup, ChainSource, SourceState,
    StoreHandle,
};
use crate::bridge::headers::BridgedHeaders;
use crate::error::{SpvError, StorageError, SyncError};

const CMD_CAP: usize = 16;

/// The ChainLock timer (SB4 §9.1): `CheckActiveState` and `EnforceBestChainLock`.
const TICK: Duration = Duration::from_secs(5);

enum HeaderCmd {
    Replace {
        anchor: Anchor,
        reply: oneshot::Sender<ReplaceOutcome>,
    },
    Headers {
        peer: PeerId,
        headers: BridgedHeaders,
        reply: oneshot::Sender<Result<HeadersReply, SpvError>>,
    },
    Step {
        input: StepInput,
        reply: oneshot::Sender<Result<Stepped, SpvError>>,
    },
    Establish {
        guard: Option<EstablishGuard>,
        reply: oneshot::Sender<GuardedEstablish>,
    },
}

/// The header task's command side. Each reply is sent once its command is done. A chain
/// subscriber's pump never awaits one: there it fails at once ([`SubError::WaitOnNoWaitTask`]).
#[derive(Clone)]
pub struct HeaderHandle {
    cmd: mpsc::Sender<HeaderCmd>,
    /// The commands whose send has completed.
    #[cfg(test)]
    sent: Arc<AtomicUsize>,
}

/// The replies to the batches a [`HeaderIngress`] queued, in the order the task took them.
pub type IngressReplies = mpsc::UnboundedReceiver<IngressReply>;

#[derive(Debug)]
pub enum ReplaceOutcome {
    /// The chain of this generation is live at the new anchor.
    Replaced(ChainGen),
    /// The store's chain is Established, so nothing was done: the chain, its generation and
    /// the published source are unchanged and the task runs on. Replacing an Established
    /// chain is S6's (Reanchor, `clear_storage`).
    Refused(String),
    /// The reset or the new chain failed: the fail-stop is tripped and the task has stopped.
    /// Also the answer, with nothing tripped, when the task had already stopped or the caller
    /// is a chain subscriber's pump.
    FailedDurably(SpvError),
}

/// A header batch's result, once its steps are committed and published.
#[derive(Debug)]
pub struct HeadersReply {
    /// The generation of the chain that took the batch.
    pub gen: ChainGen,
    /// Why the batch stopped early, if it did: a rejected header or one below the anchor.
    pub stop: Option<HeaderStop>,
    /// Core's `pindexLast`: the last header accepted or already known.
    pub last_accepted: Option<BlockId>,
    /// The batch was not handed to `accept`, by Core's checks before it.
    pub refused: Option<Refused>,
    /// The peer's misbehaviour for the batch, with Core's weight.
    pub misbehaviour: Option<HeaderMisbehaviour>,
    /// The snapshot published after the last step.
    pub snap: Arc<ChainReadSnapshot>,
}

/// A ChainLock's result, once its step is committed and published.
#[derive(Debug)]
pub struct ClReply {
    /// `Pending` when its header is not known yet: the lock is the best one, and the floor
    /// moves when the header arrives.
    pub outcome: ClOutcome,
    pub gen: ChainGen,
    pub snap: Arc<ChainReadSnapshot>,
}

struct Stepped {
    out: StepOut,
    gen: ChainGen,
    snap: Arc<ChainReadSnapshot>,
}

impl HeaderHandle {
    /// Drops the chain, durably resets the store to a Candidate at `anchor`, and builds the
    /// chain of the next generation there.
    pub async fn replace(&self, anchor: Anchor) -> ReplaceOutcome {
        let cmd = |reply| HeaderCmd::Replace {
            anchor,
            reply,
        };
        self.request(cmd).await.unwrap_or_else(|e| ReplaceOutcome::FailedDurably(e.into()))
    }

    /// Sends a command and awaits its reply, which a chain subscriber's pump never does.
    async fn request<T>(
        &self,
        cmd: impl FnOnce(oneshot::Sender<T>) -> HeaderCmd,
    ) -> Result<T, SubError> {
        no_wait::check()?;
        let (reply, done) = oneshot::channel();
        self.cmd.send(cmd(reply)).await.map_err(|_| SubError::Stopped)?;
        #[cfg(test)]
        self.sent.fetch_add(1, Ordering::SeqCst);
        SpvReply::new(done).await
    }

    /// Accepts `headers` from `peer` (Core's `ProcessNewBlockHeaders`) as one command: one step
    /// per `step_headers` of them, back to back with no other command between them, so one
    /// generation takes the whole batch (`net_processing.cpp:3367`), stopping at the first step
    /// that stops early. The reply is the whole batch's: the stop's index counts from its first
    /// header, and `last_accepted` is the last header any step accepted or knew. An empty batch
    /// runs no step and is answered with the published snapshot (`:3333-3336`). Only the bridge
    /// makes the batch, so every hash `accept` reads is the X11 `dash-consensus` computed
    /// (SB12a). An error is fatal: the fail-stop is tripped and the task has stopped, except
    /// while the store closes for a stop, when the task stops with nothing tripped. The headers
    /// manager queues its peers' batches through [`HeaderIngress`] instead, which never waits.
    pub async fn headers(
        &self,
        peer: PeerId,
        headers: BridgedHeaders,
    ) -> Result<HeadersReply, SpvError> {
        let cmd = |reply| HeaderCmd::Headers {
            peer,
            headers,
            reply,
        };
        self.request(cmd).await?
    }

    /// Applies a ChainLock whose signature is verified (Core's `ProcessNewChainLock` after
    /// `VerifyChainLock`). While the chain observes SPORK_19 off, the lock is recorded but
    /// neither enforced nor a bound on fork choice. Errors as [`headers`](Self::headers).
    pub async fn apply_chainlock(&self, cl: ChainLockRef) -> Result<ClReply, SpvError> {
        let s = self.step(StepInput::ChainLock(cl)).await?;
        let outcome = s.out.cl.ok_or_else(|| {
            SyncError::InvalidState("a ChainLock step without its outcome".into())
        })?;
        Ok(ClReply {
            outcome,
            gen: s.gen,
            snap: s.snap,
        })
    }

    async fn step(&self, input: StepInput) -> Result<Stepped, SpvError> {
        let cmd = |reply| HeaderCmd::Step {
            input,
            reply,
        };
        self.request(cmd).await?
    }

    /// Establishes the store's anchor at a publication boundary, between two steps, if the chain
    /// is still at `guard`'s generation and published `tip_seq`, caught up and not behind; else
    /// `Stale` with the current state, and nothing written. The status says `established` before
    /// the reply. A failed write trips the fail-stop and stops the task.
    pub async fn establish_guarded(&self, guard: EstablishGuard) -> GuardedEstablish {
        self.establish_with(Some(guard)).await
    }

    /// [`establish_guarded`](Self::establish_guarded) with no guard: the path with masternode
    /// sync off.
    pub async fn establish(&self) -> GuardedEstablish {
        self.establish_with(None).await
    }

    /// The commands whose send has completed, on any clone of this handle.
    #[cfg(test)]
    pub(super) fn sent(&self) -> usize {
        self.sent.load(Ordering::SeqCst)
    }

    async fn establish_with(&self, guard: Option<EstablishGuard>) -> GuardedEstablish {
        let cmd = |reply| HeaderCmd::Establish {
            guard,
            reply,
        };
        self.request(cmd).await.unwrap_or_else(|e| GuardedEstablish::Failed(e.into()))
    }
}

fn offset(stop: HeaderStop, by: usize) -> HeaderStop {
    match stop {
        HeaderStop::Invalid {
            index,
            reject,
        } => HeaderStop::Invalid {
            index: by + index,
            reject,
        },
        HeaderStop::BelowAnchor {
            index,
        } => HeaderStop::BelowAnchor {
            index: by + index,
        },
    }
}

pub(super) fn stopped() -> SpvError {
    SpvError::ChannelFailure("header task".into(), "stopped".into())
}

struct HeaderTask<H: StoreHandle> {
    h: H,
    setup: ChainHostSetup,
    chain: Option<Chain<H>>,
    gen: ChainGen,
    source: watch::Sender<ChainSource>,
    /// Its only lasting owner: the ingress holds it weakly to publish unknown-hash observations,
    /// so the watch closes when the task stops.
    status: Arc<watch::Sender<HeaderSyncState>>,
    cmd: mpsc::Receiver<HeaderCmd>,
    ingress: mpsc::Receiver<Queued>,
    /// Signalled after each batch the task takes off `ingress`.
    space: Arc<tokio::sync::Notify>,
    replies: mpsc::UnboundedSender<IngressReply>,
    /// The chain's `step_headers`: `accept` refuses a larger batch.
    step_headers: usize,
    /// The writer's one wake, on every chain of the task (SB5 §4.8).
    parker: Arc<Parker>,
    /// A step's `UpdatedBlockTip` scheduled a check.
    scheduled: bool,
    unconnecting: Unconnecting,
    /// The subscribers' pumps, which the writer waits on.
    subs: Arc<SubTable>,
}

/// The task's handles: commands, the published source and status, and its ingress with the
/// replies.
pub(super) struct Spawned {
    pub(super) handle: HeaderHandle,
    pub(super) source: watch::Receiver<ChainSource>,
    pub(super) status: watch::Receiver<HeaderSyncState>,
    pub(super) ingress: HeaderIngress,
    pub(super) replies: IngressReplies,
}

/// Starts the task on `chain`, published `Live` at generation 1. It stops when every handle
/// is gone, a step fails or a replacement fails, and publishes `Stopped`.
pub(super) fn spawn<H: StoreHandle>(
    h: H,
    setup: ChainHostSetup,
    chain: Chain<H>,
    subs: Arc<SubTable>,
) -> Spawned {
    let gen = ChainGen(1);
    let step_headers = chain.config().feeds.step_headers as usize;
    let parker = Parker::new();
    chain.set_producer_wake(Notify::Thread(Arc::clone(&parker)));
    let (source, sub) = watch::channel(ChainSource::live(gen, &chain, &Arc::default()));
    let (status, sync) =
        watch::channel(HeaderSyncState::new(gen, &chain.snapshot(), established(&h)));
    let status = Arc::new(status);
    let weak = Arc::downgrade(&status);
    let (cmd, commands) = mpsc::channel(CMD_CAP);
    let (queue, ingress) = mpsc::channel(INGRESS_CAP);
    let (replies, answers) = mpsc::unbounded_channel();
    let space = Arc::new(tokio::sync::Notify::new());
    let task = HeaderTask {
        h,
        setup,
        chain: Some(chain),
        gen,
        source,
        status,
        cmd: commands,
        ingress,
        space: Arc::clone(&space),
        replies,
        step_headers,
        parker,
        scheduled: false,
        unconnecting: Unconnecting::default(),
        subs,
    };
    tokio::spawn(task.run());
    Spawned {
        handle: HeaderHandle {
            cmd,
            #[cfg(test)]
            sent: Arc::default(),
        },
        source: sub,
        status: sync,
        ingress: HeaderIngress::new(queue, space, weak),
        replies: answers,
    }
}

fn established<H: StoreHandle>(h: &H) -> bool {
    h.store().view().meta.is_some_and(|m| m.anchor_state == AnchorState::Established)
}

/// The task's next input.
enum Next {
    Tick,
    Cmd(HeaderCmd),
    Queued(Queued),
}

/// However the task ends, `Stopped` is published; a panic trips the fail-stop.
impl<H: StoreHandle> Drop for HeaderTask<H> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.setup.fail.trip("the header task panicked");
        }
        drop(self.chain.take());
        self.source.send_modify(|s| *s = s.with(SourceState::Stopped));
    }
}

impl<H: StoreHandle> HeaderTask<H> {
    async fn run(mut self) {
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + TICK, TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            let next = if std::mem::take(&mut self.scheduled) {
                Next::Tick
            } else {
                tokio::select! {
                    biased;
                    _ = tick.tick() => Next::Tick,
                    cmd = self.cmd.recv() => match cmd {
                        Some(cmd) => Next::Cmd(cmd),
                        None => break,
                    },
                    Some(q) = self.ingress.recv() => Next::Queued(q),
                }
            };
            let failed = match next {
                Next::Tick => self.step(StepInput::Tick, true).await.is_err(),
                Next::Cmd(cmd) => self.command(cmd).await,
                Next::Queued(Queued {
                    peer,
                    headers,
                    round,
                }) => {
                    self.space.notify_one();
                    #[cfg(test)]
                    self.subs.task.batch().await;
                    let round = round.filter(|r| self.asks_now(r.asked));
                    let reply = self.headers(peer, headers, round).await;
                    let failed = reply.is_err();
                    let _ = self.replies.send(IngressReply {
                        peer,
                        reply,
                    });
                    failed
                }
            };
            if failed {
                break;
            }
        }
    }

    /// Runs `cmd` and answers it; whether it failed, which stops the task.
    async fn command(&mut self, cmd: HeaderCmd) -> bool {
        match cmd {
            HeaderCmd::Replace {
                anchor,
                reply,
            } => {
                let out = self.replace(anchor).await;
                let failed = matches!(out, ReplaceOutcome::FailedDurably(_));
                let _ = reply.send(out);
                failed
            }
            HeaderCmd::Headers {
                peer,
                headers,
                reply,
            } => {
                let out = self.headers(peer, headers, None).await;
                let failed = out.is_err();
                let _ = reply.send(out);
                failed
            }
            HeaderCmd::Step {
                input,
                reply,
            } => {
                let out = self.step(input, true).await;
                let failed = out.is_err();
                let _ = reply.send(out);
                failed
            }
            HeaderCmd::Establish {
                guard,
                reply,
            } => {
                let out = self.establish(guard).await;
                let failed = matches!(out, GuardedEstablish::Failed(_));
                let _ = reply.send(out);
                failed
            }
        }
    }

    /// Whether `asked` names the generation and the tip the next batch starts from.
    fn asks_now(&self, asked: Asked) -> bool {
        asked.gen == self.gen
            && self.chain.as_ref().is_some_and(|c| tip_ref(&c.snapshot()) == asked.tip)
    }

    /// PLAN-r1 §2.11 at the task's publication boundary: the last step's job has returned, so
    /// its publication and drain are done, no hold is open, and no step starts before the reply.
    async fn establish(&mut self, guard: Option<EstablishGuard>) -> GuardedEstablish {
        let now = *self.status.borrow();
        let Some(chain) = self.chain.as_ref() else {
            return GuardedEstablish::Failed(stopped());
        };
        if let Some(g) = guard {
            let published = chain.snapshot().tip_seq;
            if g.gen != self.gen || g.tip_seq != published || !now.caught_up || now.behind {
                return GuardedEstablish::Stale(now);
            }
        }
        if !now.established {
            let h = self.h.clone();
            let done = self.h.job(move || h.store().establish()).await;
            match done.and_then(|r| r.map_err(|e| SpvError::Storage(e.into()))) {
                Ok(()) => {}
                Err(e @ SpvError::Storage(StorageError::ShuttingDown)) => {
                    return GuardedEstablish::Failed(e)
                }
                Err(e) => {
                    self.setup.fail.trip(format!("establishing the header chain: {e}"));
                    return GuardedEstablish::Failed(e);
                }
            }
            self.status.send_modify(|s| s.established = true);
        }
        GuardedEstablish::Established
    }

    /// The batch's steps, then its `round`'s state, with no other input between them. A batch
    /// with a round publishes no state of its own steps: the round's update names their tip.
    async fn headers(
        &mut self,
        peer: PeerId,
        headers: BridgedHeaders,
        round: Option<Round>,
    ) -> Result<HeadersReply, SpvError> {
        let mut reply = HeadersReply {
            gen: self.gen,
            stop: None,
            last_accepted: None,
            refused: None,
            misbehaviour: None,
            snap: self.chain.as_ref().ok_or_else(stopped)?.snapshot(),
        };
        if headers.is_empty() {
            return Ok(self.settle(round, reply).await);
        }
        let mut lead = Some(Lead {
            len: headers.len(),
            continuous: headers.continuous(),
        });
        let mut first = 0;
        for batch in headers.batches(self.step_headers) {
            let n = batch.headers.len();
            let s = self
                .step(
                    StepInput::Headers {
                        peer,
                        batch,
                        lead: lead.take(),
                    },
                    round.is_none(),
                )
                .await?;
            reply.snap = s.snap;
            reply.last_accepted = s.out.last_accepted.or(reply.last_accepted);
            reply.stop = s.out.stop.map(|stop| offset(stop, first));
            reply.refused = s.out.refused;
            if reply.stop.is_some() || reply.refused.is_some() {
                break;
            }
            #[cfg(test)]
            self.subs.task.stepped().await;
            first += n;
        }
        reply.misbehaviour =
            self.unconnecting.score(peer, reply.refused.as_ref(), reply.stop.as_ref());
        if let Some(m) = &reply.misbehaviour {
            tracing::debug!("peer={}: {} (weight {})", peer.0, m.reason, m.weight);
        }
        Ok(self.settle(round, reply).await)
    }

    /// Publishes `round`'s state at `reply`'s snapshot, when the batch has a round, reading the
    /// outstanding observation in the same update. A batch without one has published its
    /// state with its steps, the last carrying how it ended.
    async fn settle(&mut self, round: Option<Round>, reply: HeadersReply) -> HeadersReply {
        #[cfg(test)]
        self.subs.task.settle().await;
        if let Some(round) = round {
            self.status.send_if_modified(|s| {
                let new = s.after(round, &reply);
                replace_if_new(s, new)
            });
        }
        reply
    }

    /// One step in one store job, the chain moved in and handed back (PLAN-r1 §2.2.2): any
    /// error trips the fail-stop and the task stops taking input. A step refused because the
    /// store is closing for a stop (BL-911) never ran: nothing is tripped, and the task stops
    /// with it. The state is updated to the step's snapshot when `status`, with how the step
    /// ended.
    async fn step(&mut self, input: StepInput, status: bool) -> Result<Stepped, SpvError> {
        let mut chain = self.chain.take().ok_or_else(stopped)?;
        let (h, parker) = (self.h.clone(), Arc::clone(&self.parker));
        let (subs, gen, lag) = (Arc::clone(&self.subs), self.gen, self.setup.lag);
        let clock = Arc::clone(&self.setup.config.clock);
        let ran = self
            .h
            .job(move || {
                let mut wait = step::StepWait::new(&parker, &subs, gen, lag);
                let out = step::step(&mut chain, &h, input, &mut wait, clock.node_now());
                (chain, out)
            })
            .await;
        let e = match ran {
            Ok((chain, Ok(out))) => {
                self.scheduled |= out.schedule;
                let snap = chain.snapshot();
                if status {
                    self.status.send_if_modified(|s| {
                        let new = s.at(&snap, &out);
                        replace_if_new(s, new)
                    });
                }
                self.chain = Some(chain);
                return Ok(Stepped {
                    out,
                    gen: self.gen,
                    snap,
                });
            }
            Ok((_, Err(StepError::Chain(e)))) => chain_error(e),
            Ok((_, Err(StepError::Store(e)))) => SpvError::Storage(e.into()),
            Err(e @ SpvError::Storage(StorageError::ShuttingDown)) => return Err(e),
            Err(e) => e,
        };
        self.setup.fail.trip(format!("a header step: {e}"));
        Err(e)
    }

    /// PLAN-r1 §2.3's replacement, in order: `Replacing` at the current generation, the chain
    /// dropped, one store job doing the Durable reset and the build, then the next generation's
    /// first state and its source `Live` with the task's wake. On a failure the fail-stop is
    /// tripped and the caller stops the task. An Established store is refused before any of it;
    /// the store's own refusal stays behind it.
    async fn replace(&mut self, anchor: Anchor) -> ReplaceOutcome {
        let state = self.h.store().view().meta.map(|m| m.anchor_state);
        if state == Some(AnchorState::Established) {
            return ReplaceOutcome::Refused("replacing an Established header chain".into());
        }
        let retired = Arc::clone(&self.source.borrow().guard);
        self.source.send_modify(|s| *s = s.with(SourceState::Replacing));
        drop(self.chain.take());
        self.scheduled = false;
        let (h, setup) = (self.h.clone(), self.setup.clone());
        let built = self.h.job(move || replace_chain(&h, &setup, &anchor, &retired)).await;
        let chain = match built.and_then(|chain| chain) {
            Ok(chain) => chain,
            Err(e) => {
                self.setup.fail.trip(format!("replacing the header chain: {e}"));
                return ReplaceOutcome::FailedDurably(e);
            }
        };
        chain.set_producer_wake(Notify::Thread(Arc::clone(&self.parker)));
        self.gen = ChainGen(self.gen.0 + 1);
        let (gen, snap) = (self.gen, chain.snapshot());
        self.status.send_modify(|s| *s = s.replaced(gen, &snap));
        self.source.send_replace(ChainSource::live(self.gen, &chain, &Arc::default()));
        self.chain = Some(chain);
        ReplaceOutcome::Replaced(self.gen)
    }
}

/// Sets `*s` to `new`; whether it changed.
fn replace_if_new(s: &mut HeaderSyncState, new: HeaderSyncState) -> bool {
    std::mem::replace(s, new) != new
}
