//! The header chain's sync state (PLAN-r1 §2.4.4) and its guarded establishment (§2.11, P6).

use dash_chain::{BlockRef, ChainReadSnapshot};

use super::score::Refused;
use super::step::StepOut;
use super::{ChainGen, HeadersReply};
use crate::error::SpvError;

/// The header chain's sync state, as the header task publishes it after each step, except the
/// steps of a batch with a [`Round`]: their tip is published with the round's facts in one
/// update once the batch has run, so no state names a tip whose round is still to come.
/// `caught_up` and `behind` are never both true. While an unknown-hash observation is
/// [`outstanding`](Self::outstanding) the state is behind, not caught up; otherwise both are
/// false until the first round of a generation, and again after a step that disconnects the
/// tip they were judged at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderSyncState {
    pub gen: ChainGen,
    pub tip: BlockRef,
    pub tip_seq: u64,
    /// Core's `m_best_header`.
    pub best_header: BlockRef,
    /// The chain has reached the peers' best header: the sync peer's last `headers` reply was
    /// short and taken whole, no announced hash is unknown to the chain, and the steps of that
    /// reply have published. Only steps that extend the tip keep it; the step of a batch
    /// without a round that is refused or stops early publishes it cleared, and an unknown-hash
    /// observation clears it until a round asked at its `observed` or later is caught up: a
    /// round asked after it, until `observed` saturates.
    pub caught_up: bool,
    /// The last round was not caught up (a full, refused or partly taken reply, or an announced
    /// hash the chain does not hold), or an unknown-hash observation is outstanding.
    pub behind: bool,
    /// The store's anchor is Established.
    pub established: bool,
    /// The unknown-hash observations made, across generations, saturating: each is published
    /// with `caught_up` cleared, `behind` set and `outstanding` at its count, in one update. One
    /// is made by [`HeaderIngress::unknown_hash`](super::HeaderIngress::unknown_hash), and by a
    /// batch refused as [`Refused::Unconnecting`]. At `u64::MAX` it stops counting: a round
    /// asked before a further observation then carries the same `observed` as one asked after
    /// it, so it ends that observation too when it is otherwise caught up.
    pub observed: u64,
    /// The `observed` of the last unknown-hash observation, until a [`Round`] asked at it or
    /// later is caught up; kept across generations. While it is `Some`, no update reports
    /// caught up. S6 reads it only as a reason to ask again: its rounds copy
    /// [`asked`](Self::asked), which carries `observed`.
    pub outstanding: Option<u64>,
}

/// The published state a `getheaders` request was built from: all three fields copied from
/// that [`HeaderSyncState`] by [`HeaderSyncState::asked`], whatever the locator starts with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asked {
    /// Its `gen`.
    pub gen: ChainGen,
    /// Its `tip`, the published tip.
    pub tip: BlockRef,
    /// Its `observed`.
    pub observed: u64,
}

/// The headers manager's word on its sync peer's reply to one `getheaders`, queued on the task's
/// ingress with that reply ([`HeaderIngress::reply`](super::HeaderIngress::reply)). The task
/// applies it once the batch's steps have run, before it serves any command, and only when, as
/// the task takes the batch, `asked.gen` is its generation and `asked.tip` its tip: otherwise
/// the round is dropped and the batch taken as any other, its headers still stepped. Applied,
/// the chain is caught up when `complete`, the batch was taken whole (no header refused, no
/// stop, so every header processed; an empty reply is taken whole), no announced hash is
/// unknown, and `asked.observed` is no older than the
/// [`outstanding`](HeaderSyncState::outstanding) observation, if any, as the round's update
/// reads it; it is behind otherwise. A refused or partly taken batch is never caught up,
/// whatever `complete` says, and a batch refused as [`Refused::Unconnecting`] is an
/// unknown-hash observation of its own. A caught-up round ends the outstanding observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Round {
    /// The published state the `getheaders` locator was built from.
    pub asked: Asked,
    /// The peer's reply was short: fewer than Core's `MAX_HEADERS_RESULTS`.
    pub complete: bool,
    /// A hash a peer announced in this round is not in the chain.
    pub unknown_announced: bool,
}

impl HeaderSyncState {
    /// The host's first state, at `snap`, neither caught up nor behind, with no observation.
    pub(super) fn new(gen: ChainGen, snap: &ChainReadSnapshot, established: bool) -> Self {
        Self {
            gen,
            tip: tip_ref(snap),
            tip_seq: snap.tip_seq,
            best_header: BlockRef {
                id: snap.tip.best_header,
                hash: snap.tip.best_header_hash,
                height: snap.tip.best_header_height,
            },
            caught_up: false,
            behind: false,
            established,
            observed: 0,
            outstanding: None,
        }
    }

    /// The state at `snap` after a step from this state's tip with `out`: its round kept only
    /// when the step forked at this tip, an extension of it, or published nothing; else neither
    /// caught up nor behind. A step that stops or is refused ends with `caught_up` cleared, and
    /// one refused as [`Refused::Unconnecting`] with an unknown-hash observation; `behind`
    /// stays as it was otherwise.
    pub(super) fn at(self, snap: &ChainReadSnapshot, out: &StepOut) -> Self {
        let kept = out.fork.is_none_or(|f| f == self.tip);
        let base = self.fresh(snap);
        let mut new = Self {
            caught_up: kept && self.caught_up,
            behind: base.behind || (kept && self.behind),
            ..base
        };
        if out.stop.is_some() || out.refused.is_some() {
            new.caught_up = false;
        }
        new.unconnecting(out.refused.as_ref())
    }

    /// The state at `reply`'s snapshot once `round`'s batch has run, as [`Round`] says.
    pub(super) fn after(self, round: Round, reply: &HeadersReply) -> Self {
        let whole = reply.stop.is_none() && reply.refused.is_none();
        let covered = self.outstanding.is_none_or(|o| round.asked.observed >= o);
        let caught_up = round.complete && whole && !round.unknown_announced && covered;
        Self {
            caught_up,
            behind: !caught_up,
            outstanding: self.outstanding.filter(|_| !caught_up),
            ..self.fresh(&reply.snap)
        }
        .unconnecting(reply.refused.as_ref())
    }

    /// Generation `gen`'s first state, at `snap`: the observation kept.
    pub(super) fn replaced(self, gen: ChainGen, snap: &ChainReadSnapshot) -> Self {
        Self {
            gen,
            established: false,
            ..self.fresh(snap)
        }
    }

    /// An unknown-hash observation: `observed` bumped, saturating, and outstanding at it; not
    /// caught up, and behind.
    pub(super) fn observe(&mut self) {
        self.observed = self.observed.saturating_add(1);
        self.outstanding = Some(self.observed);
        self.caught_up = false;
        self.behind = true;
    }

    /// This state, with the observation a [`Refused::Unconnecting`] refusal makes.
    fn unconnecting(mut self, refused: Option<&Refused>) -> Self {
        if matches!(refused, Some(Refused::Unconnecting { .. })) {
            self.observe();
        }
        self
    }

    /// This state at `snap`, not caught up, and behind only while an observation is
    /// outstanding.
    fn fresh(self, snap: &ChainReadSnapshot) -> Self {
        Self {
            observed: self.observed,
            outstanding: self.outstanding,
            behind: self.outstanding.is_some(),
            ..Self::new(self.gen, snap, self.established)
        }
    }

    /// What a `getheaders` request built from this state asks, for its [`Round`].
    pub fn asked(&self) -> Asked {
        Asked {
            gen: self.gen,
            tip: self.tip,
            observed: self.observed,
        }
    }

    /// What a [`SubItem::Sync`](super::SubItem::Sync) reports a change of.
    pub(super) fn facts(&self) -> (bool, bool, bool) {
        (self.caught_up, self.behind, self.established)
    }
}

pub(super) fn tip_ref(s: &ChainReadSnapshot) -> BlockRef {
    BlockRef {
        id: s.tip.tip,
        hash: s.tip.tip_hash,
        height: s.tip.tip_height,
    }
}

/// The state a caller judged covered, which
/// [`establish_guarded`](super::HeaderHandle::establish_guarded) checks at a publication
/// boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EstablishGuard {
    pub gen: ChainGen,
    pub tip_seq: u64,
}

#[derive(Debug)]
pub enum GuardedEstablish {
    /// The store's anchor is Established, and the published state says so.
    Established,
    /// A check failed and nothing was written: the current state.
    Stale(HeaderSyncState),
    /// Nothing was established. The failure ends the task when the Durable establish failed,
    /// which trips the fail-stop, and when the store refused the job as it shuts down, which
    /// trips nothing; the reply can come before the task publishes `Stopped`. A call made once
    /// the task has stopped fails too. Only a marked subscriber pump's call, refused before it
    /// is sent since a pump may not wait on the task, leaves the task running.
    Failed(SpvError),
}
