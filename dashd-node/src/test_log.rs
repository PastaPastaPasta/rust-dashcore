//! Test-only log capture shared by the crate's unit tests.

use std::fmt;
use std::sync::{Arc, Mutex};

/// The lines a call logs on this thread, as (level, message).
pub(crate) fn logged<T>(f: impl FnOnce() -> T) -> (T, Vec<(tracing::Level, String)>) {
    struct Lines(Mutex<Vec<(tracing::Level, String)>>);
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
    let lines = Arc::new(Lines(Mutex::new(Vec::new())));
    let out = tracing::subscriber::with_default(Arc::clone(&lines), f);
    let lines = std::mem::take(&mut *lines.0.lock().unwrap());
    (out, lines)
}
