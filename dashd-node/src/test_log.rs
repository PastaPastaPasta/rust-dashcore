//! Test-only log capture shared by the crate's unit tests.

use std::cell::RefCell;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock, PoisonError, RwLock};

use tracing::subscriber::Interest;

/// The lines a subscriber saw, as (level, message).
#[derive(Default)]
pub(crate) struct Lines(Mutex<Vec<(tracing::Level, String)>>);

impl Lines {
    /// The messages so far.
    pub(crate) fn messages(&self) -> Vec<String> {
        self.0.lock().unwrap().iter().map(|(_, m)| m.clone()).collect()
    }
}

impl tracing::Subscriber for Lines {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn fmt::Debug) {
                if f.name() == "message" {
                    self.0 = format!("{v:?}");
                }
            }
        }
        let mut m = Message(String::new());
        event.record(&mut m);
        self.0.lock().unwrap().push((*event.metadata().level(), m.0));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// BL-479: never the only registered dispatcher, or a thread without one that registers a
/// callsite first caches `Interest::never` for it (dashd-store's `keep_callsites_registered`).
fn keep_callsites_registered() {
    static KEEP: OnceLock<tracing::Dispatch> = OnceLock::new();
    KEEP.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
}

/// The lines a call logs on this thread, as (level, message).
pub(crate) fn logged<T>(f: impl FnOnce() -> T) -> (T, Vec<(tracing::Level, String)>) {
    keep_callsites_registered();
    let lines = Arc::new(Lines::default());
    let out = tracing::subscriber::with_default(Arc::clone(&lines), f);
    let lines = std::mem::take(&mut *lines.0.lock().unwrap());
    (out, lines)
}

thread_local! {
    static CAPTURED: RefCell<Option<tracing::dispatcher::DefaultGuard>> = const { RefCell::new(None) };
}

/// Sends the rest of this thread's lines to `lines`, until the thread ends; a thread already
/// sending its lines somewhere keeps doing so.
pub(crate) fn capture_this_thread(lines: &Arc<Lines>) {
    keep_callsites_registered();
    CAPTURED.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            let dispatch = tracing::Dispatch::new(Arc::clone(lines));
            *c = Some(tracing::dispatcher::set_default(&dispatch));
        }
    });
}

type Tapped = Arc<dyn tracing::Subscriber + Send + Sync>;

/// The subscribers [`tap`] gave the global default, by tap.
static TAPS: RwLock<Vec<(u64, Tapped)>> = RwLock::new(Vec::new());

fn taps() -> Vec<Tapped> {
    let taps = TAPS.read().unwrap_or_else(PoisonError::into_inner);
    taps.iter().map(|(_, s)| Arc::clone(s)).collect()
}

/// This binary's one global subscriber: every tap hears each event it enables.
struct Fanout;

impl tracing::Subscriber for Fanout {
    fn register_callsite(&self, _: &'static tracing::Metadata<'static>) -> Interest {
        Interest::sometimes()
    }
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        taps().iter().any(|s| s.enabled(meta))
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        for s in taps() {
            if s.enabled(event.metadata()) {
                s.event(event);
            }
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// A subscriber of [`tap`]'s, hearing the global default's events until dropped.
pub(crate) struct Tap(u64);

/// Gives `subscriber` every event that reaches the global default, which is where node threads
/// log. Tests share the one global default this installs instead of each installing their own,
/// which only the first could.
pub(crate) fn tap(subscriber: impl tracing::Subscriber + Send + Sync + 'static) -> Tap {
    static INSTALL: Once = Once::new();
    static NEXT: AtomicU64 = AtomicU64::new(0);
    INSTALL.call_once(|| {
        tracing::subscriber::set_global_default(Fanout)
            .expect("the binary's one global subscriber");
    });
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    TAPS.write().unwrap_or_else(PoisonError::into_inner).push((id, Arc::new(subscriber)));
    Tap(id)
}

impl Drop for Tap {
    fn drop(&mut self) {
        TAPS.write().unwrap_or_else(PoisonError::into_inner).retain(|(id, _)| *id != self.0);
    }
}
