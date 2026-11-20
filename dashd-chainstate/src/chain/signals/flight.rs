//! The record of the `BlockConnected` dispatch the signals thread is running or holding, which
//! shutdown's fence reads (LK1a F, D-F3): the dispatch's listener snapshot is taken under the
//! record's lock, so a fence after an `unregister` either finds the record or the dispatch took
//! the list without the listener.
//!
//! Generic over the snapshot and the end signal, so the loom model (`--cfg loom`) runs it over a
//! plain list.

#[cfg(loom)]
use loom::sync::Mutex;
#[cfg(not(loom))]
use std::sync::Mutex;

/// The dispatch's snapshot and its end signal.
struct Record<L, E> {
    listeners: L,
    ended: E,
}

/// The one dispatch record: set as a dispatch takes off, cleared as it lands.
pub(super) struct FlightSlot<L, E>(Mutex<Option<Record<L, E>>>);

impl<L, E> Default for FlightSlot<L, E> {
    fn default() -> Self {
        FlightSlot(Mutex::new(None))
    }
}

impl<L: Clone, E: Clone> FlightSlot<L, E> {
    /// Takes `load`'s snapshot under the record's lock and records it with `ended`.
    pub(super) fn take_off(&self, load: impl FnOnce() -> L, ended: E) -> L {
        let mut slot = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let listeners = load();
        *slot = Some(Record {
            listeners: listeners.clone(),
            ended,
        });
        listeners
    }

    /// Clears the record; its end signal is dropped after the lock.
    pub(super) fn land(&self) {
        let ended = self.0.lock().unwrap_or_else(|p| p.into_inner()).take();
        drop(ended);
    }

    /// The running dispatch's end signal, if its snapshot `holds` the listener.
    pub(super) fn holding(&self, holds: impl FnOnce(&L) -> bool) -> Option<E> {
        let slot = self.0.lock().unwrap_or_else(|p| p.into_inner());
        slot.as_ref().filter(|r| holds(&r.listeners)).map(|r| r.ended.clone())
    }
}

/// D-F3's race: a dispatch taking off against `unregister` and the fence. Whenever the fence
/// finds no record holding the listener, the dispatch has either taken a list without it or
/// already finished running it.
#[cfg(all(loom, test))]
mod loom_tests {
    use loom::sync::Arc;
    use loom::sync::atomic::{AtomicBool, Ordering};
    use loom::thread;

    use super::*;

    #[test]
    fn loom_a_fence_after_unregister_never_misses_a_dispatch_holding_the_listener() {
        let mut model = loom::model::Builder::new();
        model.preemption_bound = Some(2);
        model.check(|| {
            let list = Arc::new(Mutex::new(Arc::new(vec![1_u32, 2])));
            let slot = Arc::new(FlightSlot::<Arc<Vec<u32>>, ()>::default());
            let ran = Arc::new(AtomicBool::new(false));
            let dispatch = {
                let (list, slot, ran) = (Arc::clone(&list), Arc::clone(&slot), Arc::clone(&ran));
                thread::spawn(move || {
                    let load = || Arc::clone(&list.lock().unwrap());
                    let taken = slot.take_off(load, ());
                    if taken.contains(&1) {
                        ran.store(true, Ordering::SeqCst);
                    }
                    slot.land();
                    taken.contains(&1)
                })
            };
            *list.lock().unwrap() = Arc::new(vec![2]);
            let waits = slot.holding(|l| l.contains(&1)).is_some();
            let ran_at_fence = ran.load(Ordering::SeqCst);
            let took = dispatch.join().expect("dispatch");
            assert!(waits || !took || ran_at_fence, "the fence missed a dispatch holding it");
        });
    }
}
