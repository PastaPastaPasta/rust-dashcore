//! The prune selectors' log lines, captured with a thread-scoped `tracing` subscriber (BL-479).
//!
//! This must stay its own test binary, and no test here may reach these callsites outside
//! `logged`. tracing-core caches each callsite's interest for the whole process. While a single
//! dispatcher is registered, it computes that interest without a lock, from the default of
//! whichever thread registers the callsite first. In the crate's shared lib binary, another test's
//! thread with no subscriber could store `Interest::never` after a capture had begun, and the
//! capture missed the line (BL-456, BL-479). No keeper can prevent that, because libtest starts
//! the other threads before any test runs. Here, only these tests' own threads reach the
//! callsites they assert.
// tracing-core 0.1.36: callsite.rs `DefaultCallsite::register`, `Rebuilder::for_each`

use std::sync::{Arc, Mutex, OnceLock};

use dashd_store::flatfile::{
    BlockFileInfo, BlockFiles, BlockFilesConfig, BlockFilesState, MIN_BLOCKS_TO_KEEP,
};

const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

/// The lines logged while `f` runs on this thread: (target, level, message).
fn logged<T>(f: impl FnOnce() -> T) -> (T, Vec<(String, tracing::Level, String)>) {
    struct Capture(Mutex<Vec<(String, tracing::Level, String)>>);
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
            let meta = event.metadata();
            self.0.lock().unwrap().push((meta.target().to_owned(), *meta.level(), m.0));
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    keep_callsites_registered();
    let capture = Arc::new(Capture(Mutex::new(Vec::new())));
    let out = tracing::subscriber::with_default(Arc::clone(&capture), f);
    let lines = std::mem::take(&mut *capture.0.lock().unwrap());
    (out, lines)
}

/// Registers a dispatcher that is never dropped, so a capture is never the only one. With a single
/// registered dispatcher, a thread without one that registers a callsite first (the regression
/// test's spawned thread) caches `Interest::never` for every thread. With two, every registration
/// asks all live dispatchers under the dispatcher lock, and a live capture keeps the interest off
/// `never`.
// tracing-core 0.1.36: callsite.rs `Dispatchers::register_dispatch`, `Rebuilder::for_each`
fn keep_callsites_registered() {
    static KEEP: OnceLock<tracing::Dispatch> = OnceLock::new();
    KEEP.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
}

/// A callsite another thread hits first, while `logged` runs, still logs to the capture.
#[test]
fn logged_sees_a_callsite_another_thread_registers_first() {
    fn line() {
        tracing::debug!(target: "bl479", "seen");
    }
    let ((), lines) = logged(|| {
        std::thread::scope(|s| s.spawn(line).join().unwrap());
        line();
    });
    assert_eq!(lines, vec![("bl479".to_owned(), tracing::Level::DEBUG, "seen".to_owned())]);
}

#[test]
fn prune_selectors_log_cores_lines_and_plans_log_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = BlockFilesConfig::new(tmp.path().join("blocks"), MAGIC);
    let state = BlockFilesState {
        infos: (0..4)
            .map(|f| BlockFileInfo {
                blocks: 1,
                size: 12 << 20,
                undo_size: 4 << 20,
                height_first: f * 10,
                height_last: f * 10 + 9,
                time_first: 0,
                time_last: 0,
            })
            .collect(),
        last_file: 3,
    };
    let open = || BlockFiles::open(cfg.clone(), state.clone());
    let manual_line = |n: usize| {
        let line = format!("Prune (Manual): prune_height=19 removed {n} blk/rev pairs");
        ("dashd_store::flatfile::blocks".to_owned(), tracing::Level::INFO, line)
    };
    let auto_line = |target: u64, actual: u64, diff: i64, n: usize| {
        let line = format!(
            "target={target}MiB actual={actual}MiB diff={diff}MiB max_prune_height=19 removed {n} blk/rev pairs"
        );
        ("prune".to_owned(), tracing::Level::DEBUG, line)
    };
    // Core's `chain_tip_height - MIN_BLOCKS_TO_KEEP` is 19 at a tip of 307.
    let tip = Some(19 + MIN_BLOCKS_TO_KEEP);
    let (plan, lines) = logged(|| open().plan_files_to_prune_manual(u32::MAX, tip));
    assert_eq!((plan.files.as_slice(), lines.len()), (&[0, 1][..], 0));
    assert_eq!(logged(|| plan.log()).1, vec![manual_line(2)]);
    let (files, lines) = logged(|| open().find_files_to_prune_manual(u32::MAX, tip));
    assert_eq!((files, lines), (vec![0, 1], vec![manual_line(2)]));
    // Automatic pruning to 40 MiB takes the two 16 MiB files at or below height 19 and stops
    // short of its target: 64 − 32 MiB plus the 17 MiB buffer.
    let target = 40 << 20;
    let (plan, lines) = logged(|| open().plan_files_to_prune(target, 0, tip, i32::MAX, false));
    assert_eq!((plan.files.as_slice(), plan.met_target, lines.len()), (&[0, 1][..], false, 0));
    let (files, lines) = logged(|| open().find_files_to_prune(target, 0, tip, i32::MAX, false));
    assert_eq!((files, lines), (vec![0, 1], vec![auto_line(40, 32, 8, 2)]));
    // Under its target with the buffer, it selects nothing and still logs.
    let (files, lines) = logged(|| open().find_files_to_prune(100 << 20, 0, tip, i32::MAX, true));
    assert_eq!((files, lines), (Vec::new(), vec![auto_line(100, 64, 36, 0)]));
    // Core logs nothing where it returns before selecting: no tip, no target, a low tip.
    for (target, after, tip) in [(target, 0, None), (0, 0, tip), (target, 1_000, tip)] {
        let (files, lines) =
            logged(|| open().find_files_to_prune(target, after, tip, i32::MAX, false));
        assert_eq!((files, lines), (Vec::new(), Vec::new()));
    }
    let (_, lines) = logged(|| open().find_files_to_prune_manual(5, None));
    assert_eq!(lines, Vec::new());
    // A manual prune logs its line when it selects nothing.
    let (_, lines) = logged(|| open().find_files_to_prune_manual(5, Some(5)));
    let line = "Prune (Manual): prune_height=5 removed 0 blk/rev pairs".to_owned();
    assert_eq!(
        lines,
        vec![("dashd_store::flatfile::blocks".to_owned(), tracing::Level::INFO, line)]
    );
}
