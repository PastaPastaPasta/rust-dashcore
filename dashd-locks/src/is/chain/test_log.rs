//! The lib tests' one global tracing subscriber, shared by every IS test that reads log lines.
//!
//! [`lines`] and [`lines_with`] read the events of the chainstate connector and of this crate
//! from every thread, V's included, as `(target, message)`. [`capture`] reads every event of its
//! own thread while its closure runs, as "LEVEL message". Both are views of one global
//! subscriber: a test binary has only one, and a scoped default races with tracing's shared
//! callsite interest when tests run in parallel.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::sync::{Mutex, Once};

static LINES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

thread_local! {
    static CAPTURED: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

struct Capture;

struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        }
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let meta = event.metadata();
        let target = meta.target();
        let global =
            target.starts_with("dashd_chainstate::connect") || target.starts_with("dashd_locks");
        let capturing = CAPTURED.with(|captured| captured.borrow().is_some());
        if !global && !capturing {
            return;
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        if capturing {
            CAPTURED.with(|captured| {
                if let Some(lines) = captured.borrow_mut().as_mut() {
                    lines.push(format!("{} {}", meta.level(), message.0));
                }
            });
        }
        if global {
            LINES.lock().unwrap().push((target.to_owned(), message.0));
        }
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

/// Installs the subscriber, once per test binary.
pub(crate) fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        tracing::subscriber::set_global_default(Capture).expect("the only global subscriber");
    });
}

/// The captured `(target, line)` pairs `keep` selects, in order.
#[cfg(feature = "test-support")]
pub(crate) fn lines(keep: impl Fn(&str, &str) -> bool) -> Vec<String> {
    let lines = LINES.lock().unwrap();
    lines.iter().filter(|(t, l)| keep(t, l)).map(|(_, l)| l.clone()).collect()
}

#[cfg(feature = "test-support")]
pub(crate) fn lines_with(needle: &str) -> Vec<String> {
    lines(|_, line| line.contains(needle))
}

/// `f`'s result and every event this thread logged while it ran, as "LEVEL message".
pub(crate) fn capture<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    install();
    tracing::callsite::rebuild_interest_cache();
    CAPTURED.with(|captured| *captured.borrow_mut() = Some(Vec::new()));
    let out = f();
    let lines = CAPTURED.with(|captured| captured.borrow_mut().take()).unwrap_or_default();
    (out, lines)
}
