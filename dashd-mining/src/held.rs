//! The template turn's log lines, held until the build counts. Core logs `CreateNewBlock`'s lines
//! once, under `cs_main` across the build and `TestBlockValidity`; a build discarded because a
//! block reached V between the turns logs nothing.
// Core: v24.0.2 src/node/miner.cpp:194, :257-363

use std::fmt;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Value, Visit, display};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::{Interest, Subscriber};
use tracing::{Dispatch, Event, Metadata};

/// An event's callsite and its fields' values, in the callsite's field order, as the node's
/// subscriber renders them.
type Line = (&'static Metadata<'static>, Vec<Option<String>>);

/// Lines held for the thread that made this: a line is held where that thread's dispatcher
/// would log it, and replayed to that dispatcher.
pub(crate) struct HeldLines {
    to: Dispatch,
    lines: Mutex<Vec<Line>>,
}

impl HeldLines {
    pub(crate) fn for_this_thread() -> Arc<HeldLines> {
        Arc::new(HeldLines {
            to: tracing::dispatcher::get_default(Dispatch::clone),
            lines: Mutex::default(),
        })
    }

    /// Runs `f` on any thread with the lines it logs held here.
    pub(crate) fn hold<T>(self: &Arc<Self>, f: impl FnOnce() -> T) -> T {
        tracing::subscriber::with_default(Holder(Arc::clone(self)), f)
    }

    /// Logs the held lines, in order.
    pub(crate) fn replay(&self) {
        let lines = std::mem::take(&mut *self.lines.lock().unwrap_or_else(|p| p.into_inner()));
        for (meta, values) in lines {
            let shown: Vec<_> = values.iter().map(|v| v.as_deref().map(display)).collect();
            let values: Vec<Option<&dyn Value>> =
                shown.iter().map(|v| v.as_ref().map(|d| d as &dyn Value)).collect();
            self.to.event(&Event::new(meta, &meta.fields().value_set_all(&values)));
        }
    }
}

struct Holder(Arc<HeldLines>);

impl Subscriber for Holder {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }

    fn enabled(&self, meta: &Metadata<'_>) -> bool {
        self.0.to.enabled(meta)
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let meta = event.metadata();
        let mut values = Values(vec![None; meta.fields().len()]);
        event.record(&mut values);
        self.0.lines.lock().unwrap_or_else(|p| p.into_inner()).push((meta, values.0));
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

struct Values(Vec<Option<String>>);

impl Visit for Values {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0[field.index()] = Some(value.to_owned());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0[field.index()] = Some(format!("{value:?}"));
    }
}
