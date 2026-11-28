//! How a session's lines are written (`batchedlogger.h:26-33`, v24.0.0).

use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

use dash_llmq::dkg::session::DkgEvent;

use super::log_events;

struct Line {
    target: String,
    text: String,
}

/// The lines `f` logs on this thread.
fn logged(f: impl FnOnce()) -> Vec<Line> {
    struct Lines(Mutex<Vec<Line>>);
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
                        self.0.push_str(&format!("{v:?}"));
                    }
                }
            }
            assert_eq!(*event.metadata().level(), tracing::Level::DEBUG);
            let mut m = Message(String::new());
            event.record(&mut m);
            self.0.lock().expect("lines").push(Line {
                target: event.metadata().target().to_owned(),
                text: m.0,
            });
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    // BL-479: never the only registered dispatcher.
    static KEEP: OnceLock<tracing::Dispatch> = OnceLock::new();
    KEEP.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    let lines = Arc::new(Lines(Mutex::new(Vec::new())));
    tracing::subscriber::with_default(Arc::clone(&lines), f);
    std::mem::take(&mut *lines.0.lock().expect("lines"))
}

fn dkg_lines(lines: &[Line]) -> Vec<&str> {
    assert!(lines.iter().all(|l| l.target == "llmq-dkg"));
    lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>()
}

fn event(record: Option<u32>, func: &'static str, text: &str) -> DkgEvent {
    DkgEvent {
        record,
        func,
        text: text.to_owned(),
    }
}

#[test]
fn a_record_is_one_line_and_a_lone_line_carries_its_function() {
    let events = [
        event(Some(0), "Contribute", "generating contributions"),
        event(Some(0), "Contribute", "generated contributions"),
        event(None, "CBLSWorker::AsyncVerifyContributionShares", "batch check failed"),
        event(Some(1), "SendContributions", "sending contributions"),
    ];
    let lines = logged(|| log_events(&events));
    assert_eq!(
        dkg_lines(&lines),
        [
            "    generating contributions\n    generated contributions",
            "CBLSWorker::AsyncVerifyContributionShares -- batch check failed",
            "    sending contributions",
        ]
    );
}

#[test]
fn get_verified_contributions_logs_under_llmq() {
    let events = [event(None, "GetVerifiedContributions", "this node does not have vvec")];
    let lines = logged(|| log_events(&events));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].target, "llmq");
    assert_eq!(lines[0].text, "GetVerifiedContributions -- this node does not have vvec");
}
