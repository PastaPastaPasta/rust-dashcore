//! Test-only fault hooks (the internal `__fault-hooks` feature). Every hook is inert
//! unless a test arms it, because `--all-features` turns the feature on in every consumer.

use std::future::Future;

use tokio::sync::broadcast::{self, error::RecvError};
use tokio_util::sync::CancellationToken;

use crate::sync::SyncEvent;

tokio::task_local! {
    static SYNC_EVENT_CAPACITY: usize;
    static MONITOR_EVENT_CAPACITY: usize;
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

/// Runs `future` so that every client started inside it feeds its `SyncEvent` monitor
/// from a bus holding `capacity` events, relayed from the sync event bus. The sync
/// managers keep the bus they had.
///
/// # Panics
///
/// If `capacity` is zero.
pub async fn with_monitor_event_capacity<F: Future>(capacity: usize, future: F) -> F::Output {
    assert!(capacity > 0, "a monitor event bus holds at least one event");
    MONITOR_EVENT_CAPACITY.scope(capacity, future).await
}

/// The `SyncEvent` monitor's receiver: `source` itself, or inside
/// [`with_monitor_event_capacity`] a small bus a relay task feeds from `source` until
/// `source` closes or `shutdown`.
pub(crate) fn monitor_event_receiver(
    mut source: broadcast::Receiver<SyncEvent>,
    shutdown: CancellationToken,
) -> broadcast::Receiver<SyncEvent> {
    let Ok(capacity) = MONITOR_EVENT_CAPACITY.try_with(|capacity| *capacity) else {
        return source;
    };
    let (relay, receiver) = broadcast::channel(capacity);
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                result = source.recv() => result,
            };
            match event {
                Ok(event) => {
                    if relay.send(event).is_err() {
                        return;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!("SyncEvent monitor relay lagged, skipped {} events", n)
                }
                Err(RecvError::Closed) => return,
            }
        }
        while let Ok(event) = source.try_recv() {
            let _ = relay.send(event);
        }
    });
    receiver
}

#[cfg(test)]
mod tests {
    use tokio::sync::broadcast::{self, error::RecvError, error::TryRecvError};
    use tokio_util::sync::CancellationToken;

    use super::{monitor_event_receiver, with_monitor_event_capacity, with_sync_event_capacity};
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

    fn send(bus: &broadcast::Sender<SyncEvent>, sends: u32) {
        for tip_height in 0..sends {
            bus.send(SyncEvent::BlockHeadersStored {
                tip_height,
            })
            .unwrap();
        }
    }

    async fn monitor_lags_after(sends: u32) -> bool {
        let bus = sync_event_bus();
        let mut receiver = monitor_event_receiver(bus.subscribe(), CancellationToken::new());
        send(&bus, sends);
        matches!(receiver.recv().await, Err(RecvError::Lagged(_)))
    }

    #[tokio::test]
    async fn monitor_event_capacity_applies_only_inside_its_scope() {
        assert!(with_monitor_event_capacity(4, monitor_lags_after(5)).await);
        assert!(!with_monitor_event_capacity(4, monitor_lags_after(4)).await);
        assert!(!with_monitor_event_capacity(4, async { lags_after(sync_event_bus(), 64) }).await);
        assert!(!monitor_lags_after(64).await);
    }

    #[tokio::test]
    async fn monitor_relay_closes_with_its_source_or_on_shutdown() {
        let shutdown = CancellationToken::new();
        let bus = sync_event_bus();
        let mut stopped = with_monitor_event_capacity(4, async {
            monitor_event_receiver(bus.subscribe(), shutdown.clone())
        })
        .await;
        send(&bus, 2);
        shutdown.cancel();
        for _ in 0..2 {
            assert!(next(&mut stopped).await.is_ok());
        }
        assert!(matches!(next(&mut stopped).await, Err(RecvError::Closed)));

        let bus = sync_event_bus();
        let mut closed = with_monitor_event_capacity(4, async {
            monitor_event_receiver(bus.subscribe(), CancellationToken::new())
        })
        .await;
        send(&bus, 1);
        drop(bus);
        assert!(next(&mut closed).await.is_ok());
        assert!(matches!(next(&mut closed).await, Err(RecvError::Closed)));
    }

    async fn next(receiver: &mut broadcast::Receiver<SyncEvent>) -> Result<SyncEvent, RecvError> {
        tokio::time::timeout(std::time::Duration::from_secs(10), receiver.recv())
            .await
            .expect("the relay never closed")
    }
}
