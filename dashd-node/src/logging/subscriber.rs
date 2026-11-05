//! `tracing` in front of the [`Logger`] (DEC-200; see the module doc of [`super`]).

use std::fmt::{self, Write};
use std::sync::Arc;

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Metadata, Subscriber};

use super::{Level, LogFlags, Logger, category_of_target};

/// Renders `tracing` events as Core's log lines. Spans are accepted and ignored: Core's lines
/// carry no context.
pub struct CoreSubscriber {
    logger: Arc<Logger>,
}

impl CoreSubscriber {
    pub fn new(logger: Arc<Logger>) -> CoreSubscriber {
        CoreSubscriber {
            logger,
        }
    }

    /// The category a line is logged under and whether it is logged now.
    fn route(&self, meta: &Metadata<'_>) -> (LogFlags, Level, bool) {
        let level = Level::of_tracing(*meta.level());
        match category_of_target(meta.target()) {
            Some(category) => {
                (category, level, self.logger.will_log_category_level(category, level))
            }
            None => {
                let logged = level >= Level::Info
                    || (self.logger.all_categories_enabled() && level >= self.logger.log_level());
                (LogFlags::ALL, level, logged)
            }
        }
    }
}

impl Subscriber for CoreSubscriber {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        // `-debug`, `-loglevel` and the `logging` RPC change what is logged at run time.
        Interest::sometimes()
    }

    fn enabled(&self, meta: &Metadata<'_>) -> bool {
        if !meta.is_event() {
            return false;
        }
        // Without `-debug` no DEBUG or TRACE line is logged: skip the target lookup.
        if *meta.level() > tracing::Level::INFO && self.logger.category_mask() == LogFlags::NONE {
            return false;
        }
        self.route(meta).2
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let meta = event.metadata();
        let (category, level, logged) = self.route(meta);
        if !logged {
            return;
        }
        let mut line = Line::default();
        event.record(&mut line);
        let mut s = line.message;
        if s.ends_with('\n') {
            s.pop();
        }
        s.push_str(&line.fields);
        s.push('\n');
        self.logger.log_print_str(
            &s,
            meta.module_path().unwrap_or(""),
            meta.file().unwrap_or(""),
            meta.line().unwrap_or(0),
            category,
            level,
        );
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

/// An event's `message`, then its other fields as ` name=value`.
#[derive(Default)]
struct Line {
    message: String,
    fields: String,
}

impl Visit for Line {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}
