//! Per-thread capture of `tracing` event messages through one process-wide subscriber.
//!
//! A scoped `set_default` subscriber loses events when it is the only dispatcher: a parallel test
//! that reaches a callsite first, on a thread with no subscriber, caches its interest as `never`.
//! One global subscriber that registers every callsite as `sometimes` and routes each event to
//! the calling thread's sink avoids that (as `sim_platformban`).
//!
//! Until `set_global_default` returns, the subscriber's level hint is `OFF`, so the global max
//! level stays `OFF` and no thread can register a callsite against the not-yet-global dispatcher
//! and cache `never`.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once};

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// One capture's targets and the `message` field of each event on them, in emission order.
struct Sink {
    targets: &'static [&'static str],
    lines: Mutex<Vec<String>>,
}

thread_local! {
    static SINK: RefCell<Option<Arc<Sink>>> = const { RefCell::new(None) };
}

/// The capture of this thread's events, until it drops.
pub struct LogCapture(Arc<Sink>);

impl LogCapture {
    /// The captured messages, in emission order.
    pub fn lines(&self) -> MutexGuard<'_, Vec<String>> {
        self.0.lines.lock().unwrap()
    }
}

impl Drop for LogCapture {
    fn drop(&mut self) {
        SINK.with(|sink| *sink.borrow_mut() = None);
    }
}

/// Starts capturing the `message` field of this thread's events on `targets`.
pub fn capture(targets: &'static [&'static str]) -> LogCapture {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        tracing::subscriber::set_global_default(ThreadSinks).expect("no other global subscriber");
        INSTALLED.store(true, Ordering::Release);
    });
    tracing::callsite::rebuild_interest_cache();
    let sink = Arc::new(Sink {
        targets,
        lines: Mutex::default(),
    });
    SINK.with(|s| *s.borrow_mut() = Some(Arc::clone(&sink)));
    LogCapture(sink)
}

/// The process-wide subscriber, which routes each event to the calling thread's sink.
struct ThreadSinks;

impl tracing::Subscriber for ThreadSinks {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        if INSTALLED.load(Ordering::Acquire) {
            None
        } else {
            Some(tracing::level_filters::LevelFilter::OFF)
        }
    }
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        meta.is_event()
            && SINK.with(|sink| {
                sink.borrow().as_ref().is_some_and(|s| s.targets.contains(&meta.target()))
            })
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Message<'a>(&'a mut String);
        impl tracing::field::Visit for Message<'_> {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    *self.0 = format!("{v:?}");
                }
            }
        }
        let mut line = String::new();
        event.record(&mut Message(&mut line));
        SINK.with(|sink| {
            if let Some(sink) = &*sink.borrow() {
                sink.lines.lock().unwrap().push(line);
            }
        });
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}
