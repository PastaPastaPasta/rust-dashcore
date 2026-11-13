//! The warn leg (plan §7.2, D100) against a real regtest dashd v24: an unknown bit's LOCKED_IN
//! in UpdateTip's log and ACTIVE's concise warning, dashd against V ([`warn`]). Its own binary:
//! V logs UpdateTip on its own thread, so only a global `tracing` capture hears it (DEC-895).
//! `contrib/node/l3-tier1.sh` runs it and keeps its report (`L3_WARN_REPORT`); it fails closed.

use std::path::Path;
use std::sync::Mutex;

use node_l1::l3::{rdc_head, version, warn};

fn repo() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
}

#[test]
fn the_unknown_bit_warns_as_dashd_does() {
    tracing::subscriber::set_global_default(Capture).expect("the only global subscriber");
    let scratch = tempfile::tempdir().expect("a temp dir");
    let dashd = version::resolve(repo(), scratch.path()).expect("the run's dashd (plan §3.1)");
    let log = warn::run(&dashd, scratch.path(), &|| UPDATE_TIP.lock().expect("the log").clone());
    let report = log.report(&dashd, &rdc_head(repo()));
    if let Ok(path) = std::env::var("L3_WARN_REPORT") {
        let text = serde_json::to_string_pretty(&report).expect("a report");
        std::fs::write(&path, text).unwrap_or_else(|e| panic!("{path}: {e}"));
    }
    if let Some(e) = log.error {
        let kept = scratch.keep();
        panic!("the warn leg failed (kept {}): {e}", kept.display());
    }
}

/// V's UpdateTip lines (`dashd_chainstate`), from every thread.
static UPDATE_TIP: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Capture;

impl tracing::Subscriber for Capture {
    fn enabled(&self, m: &tracing::Metadata<'_>) -> bool {
        m.target().starts_with("dashd_chainstate")
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    self.0 = format!("{v:?}");
                }
            }
        }
        let mut m = Message(String::new());
        event.record(&mut m);
        if m.0.starts_with("UpdateTip: ") {
            UPDATE_TIP.lock().expect("the log").push(m.0);
        }
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}
