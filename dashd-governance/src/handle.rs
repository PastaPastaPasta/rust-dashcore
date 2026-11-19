//! The governance lock and the published trigger view. Admission, the tip work and the clean
//! run as V commands; votes take the governance lock only.

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};

use arc_swap::ArcSwap;
use dash_dml::MnList;
use dash_governance::GovParams;
use dash_governance::manager::{Effects, Manager, ObjectOutcome, TriggerView, VoteOutcome};
use dash_governance::superblocks::HeightOverflow;
use dash_primitives::{GovObject, GovVote};
use dashd_indexes::TxIndexReader;

use crate::collateral::AtUseCollateral;
use crate::exec::{EffectsSink, GovChain, GovEnv, GovExec, GovReply, reply};

/// `admit`'s answer.
#[derive(Debug)]
pub struct Admitted {
    /// `ProcessObject`'s.
    pub outcome: Result<ObjectOutcome, HeightOverflow>,
    /// To apply after the lock is released.
    pub effects: Effects,
    /// The index failed on the collateral: the object is refused as on a miss, but the fault is
    /// ours, not the peer's.
    pub index_fault: bool,
}

/// `tip`'s answer. Its effects go to the [`EffectsSink`].
#[derive(Debug)]
pub struct TipDone {
    pub result: Result<(), HeightOverflow>,
    /// The index failed on a postponed object's collateral, which the pass dropped as on a miss.
    pub index_fault: bool,
}

/// Where an answer's effects go once the lock is released.
enum Deliver {
    /// Back to the waiting caller, in the answer.
    Reply,
    /// To the [`EffectsSink`], for a caller that posts and does not wait.
    Sink,
}

pub struct GovernanceHandle<E> {
    state: Mutex<Manager>,
    published: ArcSwap<TriggerView>,
    exec: E,
    env: Arc<dyn GovEnv>,
    sink: Arc<dyn EffectsSink>,
    index: Arc<dyn TxIndexReader>,
    params: GovParams,
}

impl<E: GovExec + 'static> GovernanceHandle<E> {
    /// Takes a fully loaded `manager` (load, `check_and_remove`, `init_on_load`, `set_loaded`):
    /// the view is republished only when the generation moves, which loading does not.
    pub fn new(
        manager: Manager,
        exec: E,
        env: Arc<dyn GovEnv>,
        sink: Arc<dyn EffectsSink>,
        index: Arc<dyn TxIndexReader>,
        params: GovParams,
    ) -> Arc<Self> {
        Arc::new(Self {
            published: ArcSwap::from_pointee(manager.trigger_view()),
            state: Mutex::new(manager),
            exec,
            env,
            sink,
            index,
            params,
        })
    }

    /// The trigger view published by the last trigger-affecting mutation.
    pub fn published(&self) -> Arc<TriggerView> {
        self.published.load_full()
    }

    /// `ProcessObject` as one command.
    // Core: governance/net_governance.cpp:193; governance/governance.cpp:227-275
    #[must_use = "the reply carries the admission's effects"]
    pub fn admit(self: &Arc<Self>, obj: GovObject) -> GovReply<Admitted> {
        self.command(Deliver::Reply, move |this, m, chain| {
            let list = chain.dmn_list();
            let (clock, synced) = (this.env.clock(), this.env.synced());
            let lookup = AtUseCollateral::for_object(&obj, m, &this.params, &*this.index, chain);
            let outcome = m.process_object_at(&obj, &list, &clock, synced, &this.params, &lookup);
            let index_fault = lookup.index_fault();
            (
                move |effects| Admitted {
                    outcome,
                    effects,
                    index_fault,
                },
                Effects::default(),
            )
        })
    }

    /// The queued `UpdatedBlockTip` work at `height` as one command.
    // Core: governance/governance.cpp:1119-1143
    pub fn tip(self: &Arc<Self>, height: i32) -> GovReply<TipDone> {
        self.command(Deliver::Sink, move |this, m, chain| {
            let list = chain.dmn_list();
            let (clock, synced) = (this.env.clock(), this.env.synced());
            let lookup = AtUseCollateral::for_postponed(m, synced, &*this.index, chain);
            let (result, effects) =
                match m.updated_block_tip(height, &list, &clock, synced, &this.params, &lookup) {
                    Ok(effects) => (Ok(()), effects),
                    Err(e) => (Err(e), Effects::default()),
                };
            let index_fault = lookup.index_fault();
            (
                move |_| TipDone {
                    result,
                    index_fault,
                },
                effects,
            )
        })
    }

    /// The scheduled `CheckAndRemove` as one command.
    // Core: governance/governance.cpp:400-513
    pub fn clean(self: &Arc<Self>) -> GovReply<()> {
        self.command(Deliver::Sink, move |this, m, chain| {
            let list = chain.dmn_list();
            let (clock, synced) = (this.env.clock(), this.env.blockchain_synced());
            (|_| (), m.check_and_remove(&list, &clock, synced, &this.params))
        })
    }

    /// `ProcessVote` under the governance lock alone, off V, against the caller's DMN-tip list.
    // Core: governance/governance.cpp:818-877
    pub fn process_vote(&self, vote: &GovVote, list: &MnList) -> (VoteOutcome, Effects) {
        let mut m = self.lock();
        let outcome = m.process_vote_at(vote, list, &self.env.clock(), &self.params);
        let mut effects = Effects::default();
        self.settle(&mut m, &mut effects);
        (outcome, effects)
    }

    /// Posts `f` as one command. It runs under the governance lock, the queues are drained into
    /// its effects and the view is republished before the lock is released; then the effects
    /// go where `deliver` says and `f`'s answer is built.
    fn command<T: Send + 'static, A: FnOnce(Effects) -> T>(
        self: &Arc<Self>,
        deliver: Deliver,
        f: impl FnOnce(&Self, &mut Manager, &dyn GovChain) -> (A, Effects) + Send + 'static,
    ) -> GovReply<T> {
        let this = Arc::clone(self);
        let (tx, rx) = reply();
        self.exec.post(Box::new(move |chain| {
            let mut m = this.lock();
            let (answer, mut effects) = f(&this, &mut m, chain);
            this.settle(&mut m, &mut effects);
            drop(m);
            let answer = match deliver {
                Deliver::Reply => answer(effects),
                Deliver::Sink => {
                    this.sink.submit(effects);
                    answer(Effects::default())
                }
            };
            let _ = tx.send(answer);
        }));
        rx
    }

    /// Drains the relay and meta queues into `effects` and republishes when the generation moved.
    fn settle(&self, m: &mut Manager, effects: &mut Effects) {
        let relay = m.take_relay_queue();
        effects.relay_objects.extend(relay.objects);
        effects.relay_votes.extend(relay.votes);
        effects.meta.extend(m.take_meta());
        if m.generation() != self.published.load().generation {
            self.published.store(Arc::new(m.trigger_view()));
        }
    }

    /// A panic under the lock stops the node, as Core's `assert` aborts: the state it left is
    /// never used.
    pub(crate) fn lock(&self) -> Locked<'_> {
        let guard = self.state.lock().expect("governance state poisoned");
        #[cfg(test)]
        crate::tests::note(crate::tests::Event::Lock(crate::exec::fakes::current_command()));
        Locked(guard)
    }

    /// The manager, for tests; recorded like every other lock.
    #[cfg(test)]
    pub(crate) fn state_for_test(&self) -> Locked<'_> {
        self.lock()
    }
}

/// The governance lock held. Tests record its release.
pub(crate) struct Locked<'a>(MutexGuard<'a, Manager>);

impl Deref for Locked<'_> {
    type Target = Manager;

    fn deref(&self) -> &Manager {
        &self.0
    }
}

impl DerefMut for Locked<'_> {
    fn deref_mut(&mut self) -> &mut Manager {
        &mut self.0
    }
}

#[cfg(test)]
impl Drop for Locked<'_> {
    fn drop(&mut self) {
        crate::tests::note(crate::tests::Event::Unlock);
    }
}
