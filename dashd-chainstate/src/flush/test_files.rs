//! The prune tests' block files: equal files of ten blocks each, with a blk and a rev file of
//! each on disk, so a test sees step 12's unlink.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use dashd_store::flatfile::{BlockFileInfo, BlockFilesState};

/// Regtest's prune retention, Core's `MinBlocksToKeep()`, which the prune tests' heights are set
/// against (regtest's params agree: `prune::tests`).
// Core: chainparams.cpp:885 at v24.0.0
pub(super) const KEEP: u32 = 288;

/// `n` files of ten blocks each, of `size` and `undo_size` bytes; the last one is current.
pub(super) fn equal_files(n: u32, size: u32, undo_size: u32) -> BlockFilesState {
    let info = |f: u32| BlockFileInfo {
        blocks: 10,
        size,
        undo_size,
        height_first: f * 10,
        height_last: f * 10 + 9,
        time_first: 0,
        time_last: 0,
    };
    BlockFilesState {
        infos: (0..n).map(info).collect(),
        last_file: n - 1,
    }
}

/// Creates `blocks` with a one-byte blk and rev file for each of `state`'s files.
pub(super) fn create_on_disk(blocks: &Path, state: &BlockFilesState) {
    std::fs::create_dir_all(blocks).unwrap();
    for n in 0..state.infos.len() {
        for kind in ["blk", "rev"] {
            std::fs::write(blocks.join(format!("{kind}{n:05}.dat")), [0]).unwrap();
        }
    }
}

/// Whether file `n`'s blk file is in `blocks`.
pub(super) fn blk_on_disk(blocks: &Path, n: u32) -> bool {
    blocks.join(format!("blk{n:05}.dat")).exists()
}

/// The lines logged at a level `keep` accepts while `f` runs on this thread.
pub(super) fn logged<T>(
    keep: fn(tracing::Level) -> bool,
    f: impl FnOnce() -> T,
) -> (T, Vec<String>) {
    struct Capture(fn(tracing::Level) -> bool, Mutex<Vec<String>>);
    impl tracing::Subscriber for Capture {
        fn enabled(&self, m: &tracing::Metadata<'_>) -> bool {
            (self.0)(*m.level())
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
            self.1.lock().unwrap().push(m.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    // BL-479: never the only registered dispatcher, or a thread without one that registers a
    // callsite first caches `Interest::never` for it (dashd-store's `keep_callsites_registered`).
    static KEEP: OnceLock<tracing::Dispatch> = OnceLock::new();
    KEEP.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    let capture = Arc::new(Capture(keep, Mutex::new(Vec::new())));
    let out = tracing::subscriber::with_default(Arc::clone(&capture), f);
    let lines = std::mem::take(&mut *capture.1.lock().unwrap());
    (out, lines)
}
