//! The per-client fail-stop latch shared with dash-chain.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use dash_chain::FailStop;

/// A client-scoped fail-stop latch for dash-chain and its storage owner.
///
/// The first reason is retained for diagnostics. Once tripped, the latch cannot
/// be cleared until a new client is constructed.
#[derive(Debug, Default)]
pub struct SpvFailStop {
    tripped: AtomicBool,
    reason: OnceLock<String>,
}

impl SpvFailStop {
    /// Record the first fatal reason and trip the latch.
    pub fn trip(&self, reason: impl Into<String>) {
        let first = self.reason.set(reason.into()).is_ok();
        self.tripped.store(true, Ordering::Release);
        if first {
            tracing::error!(reason = ?self.reason.get(), "SPV fail-stop tripped");
        }
    }

    /// The first recorded fatal reason, if the latch has tripped.
    pub fn reason(&self) -> Option<String> {
        self.reason.get().cloned()
    }
}

impl FailStop for SpvFailStop {
    fn read_failed(&self, what: &str) {
        self.trip(what);
    }

    fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::Acquire)
    }
}

impl SpvFailStop {
    /// Share this latch with a dash-chain service that accepts `FailStop`.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_first_reason() {
        let latch = SpvFailStop::default();
        assert!(!latch.is_tripped());
        latch.trip("first");
        latch.trip("second");
        assert!(latch.is_tripped());
        assert_eq!(latch.reason().as_deref(), Some("first"));
    }

    #[test]
    fn implements_dash_chain_fail_stop() {
        let latch = SpvFailStop::default();
        FailStop::read_failed(&latch, "read failed");
        assert!(FailStop::is_tripped(&latch));
        assert_eq!(latch.reason().as_deref(), Some("read failed"));
    }

    #[test]
    fn concurrent_trips_publish_the_first_reason_with_the_latch() {
        let latch = Arc::new(SpvFailStop::default());
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let latch = Arc::clone(&latch);
                std::thread::spawn(move || latch.trip(format!("reason {i}")))
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert!(latch.is_tripped());
        assert!(latch.reason().is_some());
    }

    /// Holds the winning trip inside its log event, so a losing trip returns while the winner
    /// has not finished.
    struct SlowEvents;

    impl tracing::Subscriber for SlowEvents {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, _: &tracing::Event<'_>) {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn every_returned_trip_sees_the_latch_tripped() {
        let latch = Arc::new(SpvFailStop::default());
        let observing = Arc::new(AtomicBool::new(true));
        let reader = {
            let (latch, observing) = (Arc::clone(&latch), Arc::clone(&observing));
            std::thread::spawn(move || {
                while observing.load(Ordering::Acquire) {
                    assert!(!latch.is_tripped() || latch.reason().is_some());
                }
            })
        };
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let latch = Arc::clone(&latch);
                std::thread::spawn(move || {
                    tracing::subscriber::with_default(SlowEvents, || {
                        latch.trip(format!("reason {i}"))
                    });
                    latch.is_tripped()
                })
            })
            .collect();
        let seen: Vec<bool> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        observing.store(false, Ordering::Release);
        reader.join().unwrap();
        assert_eq!(seen, vec![true; 8], "a trip returned before the latch was tripped");
    }
}
