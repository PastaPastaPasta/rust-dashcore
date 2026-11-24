//! The chain-command seam. Core runs object admission, the postponed pass, the scheduled clean
//! and the queued tip work under `cs_main`; here each runs as one closure on the chain actor V,
//! which reads the chain through [`GovChain`] and is queued through [`GovExec`]. A command reads
//! the time and sync state through [`GovEnv`] when it runs, and a command nobody waits for hands
//! its effects to an [`EffectsSink`].

use std::sync::{Arc, mpsc};

use dash_dml::MnList;
use dash_governance::manager::{Effects, GovClock};
use dash_primitives::BlockHash;

/// The chain state a governance command reads inside its closure, on V. Not `Sync`: V's own
/// query view implements it (S4).
pub trait GovChain {
    /// The height of `block` when it is on the active chain.
    // Core: governance/object.cpp:857-858
    fn active_height(&self, block: &BlockHash) -> Option<u32>;

    /// `ActiveChain().Height()`.
    fn tip_height(&self) -> u32;

    /// `GetListAtChainTip()`: the list at the DMN tip.
    // Core: governance/governance.cpp:250
    fn dmn_list(&self) -> Arc<MnList>;
}

/// One governance command.
pub type GovCommand = Box<dyn FnOnce(&dyn GovChain) + Send>;

/// Runs governance commands one at a time, each alone and in the order posted, as V serves its
/// command lane.
pub trait GovExec: Send + Sync {
    /// Queues `command`. An executor that drops it unrun disconnects its reply.
    fn post(&self, command: GovCommand);
}

impl<E: GovExec + ?Sized> GovExec for Arc<E> {
    fn post(&self, command: GovCommand) {
        (**self).post(command);
    }
}

/// The time and sync state a governance command reads when it runs.
pub trait GovEnv: Send + Sync {
    /// `GetTime()` and `GetAdjustedTime()`.
    fn clock(&self) -> GovClock;

    /// `m_mn_sync.IsSynced()`.
    fn synced(&self) -> bool;

    /// `m_mn_sync.IsBlockchainSynced()`.
    fn blockchain_synced(&self) -> bool;
}

/// N's effects worker: it applies each command's effects, in submission order, after the
/// governance lock is released. Submitting never waits. That order is the order `tip` and `clean`
/// complete in only: `admit` and `process_vote` return their effects to their callers, so relay
/// order across the two routes need not be lock order; S3b and S4 must not rely on it. A vote's
/// meta cannot land after its object's `RemoveObject` this way: a vote on a marked object is
/// refused, and a clean erases only objects marked at least 600 s earlier.
pub trait EffectsSink: Send + Sync {
    fn submit(&self, effects: Effects);
}

/// A posted command's answer. Dropping it leaves the command queued.
#[derive(Debug)]
pub struct GovReply<T>(mpsc::Receiver<T>);

impl<T> GovReply<T> {
    /// Waits for the command; `None` when the executor dropped it unrun.
    pub fn wait(self) -> Option<T> {
        self.0.recv().ok()
    }
}

pub(crate) fn reply<T>() -> (mpsc::Sender<T>, GovReply<T>) {
    let (tx, rx) = mpsc::channel();
    (tx, GovReply(rx))
}

/// A serial executor, a non-`Sync` chain, a settable environment and a recording sink, for tests.
#[cfg(any(test, feature = "testing"))]
pub mod fakes {
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::marker::PhantomData;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

    use dash_dml::MnList;
    use dash_governance::manager::{Effects, GovClock};
    use dash_primitives::BlockHash;

    use super::{EffectsSink, GovChain, GovCommand, GovEnv, GovExec};

    /// A settable time and sync state.
    pub struct FakeEnv(Mutex<(GovClock, bool, bool)>);

    impl FakeEnv {
        /// `clock`, synced and blockchain-synced.
        pub fn new(clock: GovClock) -> Self {
            Self(Mutex::new((clock, true, true)))
        }

        pub fn set(&self, clock: GovClock, synced: bool, blockchain_synced: bool) {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner) =
                (clock, synced, blockchain_synced);
        }

        fn get(&self) -> (GovClock, bool, bool) {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    impl GovEnv for FakeEnv {
        fn clock(&self) -> GovClock {
            self.get().0
        }

        fn synced(&self) -> bool {
            self.get().1
        }

        fn blockchain_synced(&self) -> bool {
            self.get().2
        }
    }

    /// Keeps every submitted `Effects`, in order.
    #[derive(Default)]
    pub struct RecordingSink(Mutex<Vec<Effects>>);

    impl RecordingSink {
        /// The effects submitted so far, taken.
        pub fn take(&self) -> Vec<Effects> {
            std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
        }
    }

    impl EffectsSink for RecordingSink {
        fn submit(&self, effects: Effects) {
            self.0.lock().unwrap_or_else(PoisonError::into_inner).push(effects);
        }
    }

    thread_local! {
        static CURRENT: Cell<Option<u64>> = const { Cell::new(None) };
    }

    /// The id of the [`SerialExec`] command running on this thread.
    pub fn current_command() -> Option<u64> {
        CURRENT.with(Cell::get)
    }

    /// A chain with a tip height, active blocks and a DMN-tip list. `!Sync`, as V's view is.
    pub struct FakeChain {
        pub tip: u32,
        pub active: BTreeMap<BlockHash, u32>,
        pub list: Arc<MnList>,
        _not_sync: PhantomData<Cell<()>>,
    }

    impl FakeChain {
        pub fn new(tip: u32, list: MnList) -> Self {
            Self {
                tip,
                active: BTreeMap::new(),
                list: Arc::new(list),
                _not_sync: PhantomData,
            }
        }
    }

    impl GovChain for FakeChain {
        fn active_height(&self, block: &BlockHash) -> Option<u32> {
            self.active.get(block).copied()
        }

        fn tip_height(&self) -> u32 {
            self.tip
        }

        fn dmn_list(&self) -> Arc<MnList> {
            Arc::clone(&self.list)
        }
    }

    /// A command's entry or exit, by command id.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ExecEvent {
        Enter(u64),
        Exit(u64),
    }

    /// Runs each command on the posting thread under one lock over the chain, so commands run
    /// one at a time and a poster waits for the command ahead of it.
    pub struct SerialExec {
        chain: Mutex<FakeChain>,
        log: Mutex<Vec<ExecEvent>>,
        next: AtomicU64,
        waiting: AtomicUsize,
    }

    impl SerialExec {
        pub fn new(chain: FakeChain) -> Self {
            Self {
                chain: Mutex::new(chain),
                log: Mutex::new(Vec::new()),
                next: AtomicU64::new(1),
                waiting: AtomicUsize::new(0),
            }
        }

        /// The chain, between commands.
        pub fn chain(&self) -> MutexGuard<'_, FakeChain> {
            self.chain.lock().unwrap_or_else(PoisonError::into_inner)
        }

        pub fn log(&self) -> Vec<ExecEvent> {
            self.log.lock().unwrap_or_else(PoisonError::into_inner).clone()
        }

        /// Posters waiting behind a running command.
        pub fn waiting(&self) -> usize {
            self.waiting.load(Ordering::SeqCst)
        }

        fn record(&self, event: ExecEvent) {
            self.log.lock().unwrap_or_else(PoisonError::into_inner).push(event);
        }
    }

    impl GovExec for SerialExec {
        fn post(&self, command: GovCommand) {
            self.waiting.fetch_add(1, Ordering::SeqCst);
            let chain = self.chain();
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            let id = self.next.fetch_add(1, Ordering::SeqCst);
            self.record(ExecEvent::Enter(id));
            CURRENT.with(|c| c.set(Some(id)));
            command(&*chain);
            CURRENT.with(|c| c.set(None));
            self.record(ExecEvent::Exit(id));
        }
    }

    trait AmbiguousIfSync<A> {
        fn item() {}
    }
    impl<T: ?Sized> AmbiguousIfSync<()> for T {}
    struct IsSync;
    impl<T: ?Sized + Sync> AmbiguousIfSync<IsSync> for T {}

    // `FakeChain` is not `Sync` (the call is ambiguous otherwise), and the seam takes it.
    const _: fn() = || {
        let _ = <FakeChain as AmbiguousIfSync<_>>::item;
        let _: fn(&FakeChain) -> &dyn GovChain = |c| c;
    };
}
