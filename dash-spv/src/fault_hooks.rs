//! Test-only fault hooks (the internal `__fault-hooks` feature).
//!
//! Every hook is inert unless a test arms it, because `--all-features` turns the feature on in
//! every consumer.

use std::future::Future;

tokio::task_local! {
    static SYNC_EVENT_CAPACITY: usize;
}

/// Runs `future` so that every client constructed inside it gets a sync event bus holding
/// `capacity` events instead of the default, so a slow subscriber lags early.
///
/// # Panics
///
/// If `capacity` is zero.
pub async fn with_sync_event_capacity<F: Future>(capacity: usize, future: F) -> F::Output {
    assert!(capacity > 0, "a sync event bus holds at least one event");
    SYNC_EVENT_CAPACITY.scope(capacity, future).await
}

pub(crate) fn sync_event_capacity() -> Option<usize> {
    SYNC_EVENT_CAPACITY.try_with(|capacity| *capacity).ok()
}

#[cfg(test)]
mod tests {
    use tokio::sync::broadcast::{self, error::TryRecvError};

    use super::with_sync_event_capacity;
    use crate::sync::{sync_event_bus, SyncEvent};

    fn lags_after(bus: broadcast::Sender<SyncEvent>, sends: u32) -> bool {
        let mut receiver = bus.subscribe();
        for tip_height in 0..sends {
            bus.send(SyncEvent::BlockHeadersStored {
                tip_height,
            })
            .unwrap();
        }
        matches!(receiver.try_recv(), Err(TryRecvError::Lagged(_)))
    }

    #[tokio::test]
    async fn sync_event_capacity_applies_only_inside_its_scope() {
        assert!(with_sync_event_capacity(4, async { lags_after(sync_event_bus(), 5) }).await);
        assert!(!with_sync_event_capacity(4, async { lags_after(sync_event_bus(), 4) }).await);
        assert!(!lags_after(sync_event_bus(), 64));
    }
}
