//! Storage layer of the Rust Dash node (A1 §2.8).
//!
//! - [`RocksEngine`] (feature `rocksdb`): the production engine, configured as A1 §2.8 describes.
//! - [`flatfile`]: Core-identical block and undo files (`blk?????.dat`, `rev?????.dat`).
//!
//! The engine contract ([`KvEngine`]), the in-memory engine, typed tables, codecs, schemas and
//! ST3a's power-loss model moved to `dash-kv` (SB10), shared with dash-spv's SQLite store. They
//! are re-exported here under their old paths; new code names `dash_kv`.

pub use dash_kv::codec;
#[cfg(any(test, feature = "model"))]
pub use dash_kv::model;
pub use dash_kv::{
    BeU32, BeU64, Cf, CfProfile, CfSpec, CodecError, Commit, DbSchema, ErrorKind, KeyCodec,
    KvBatch, KvEngine, KvRead, MemoryBatch, MemoryEngine, MemorySnapshot, Result, SCHEMA_KEY,
    SchemaState, StoreError, Table, ValueCodec, schema_record,
};
pub use dash_kv::{CfVerdict, Invalidation, LiveVerdict, copy_point_steps};
#[cfg(all(feature = "rocksdb", any(test, feature = "test-hooks")))]
pub use rocks::ProbeCounters;
#[cfg(feature = "rocksdb")]
pub use rocks::{
    LiveCopy, RocksBatch, RocksConfig, RocksEngine, RocksMemory, RocksSnapshot, STORE_MARKER,
};

// CH5-5's counters are for process tests only: no optimized build carries them. Clippy is exempt,
// as the pre-push hook lints `--release --all-features`.
#[cfg(all(feature = "test-hooks", not(debug_assertions), not(clippy)))]
compile_error!("feature `test-hooks` is for tests only; build without it");

/// ST3a's power-loss model: `dash_kv::faulty`, and the node's systems under test over block files.
#[cfg(feature = "faulty")]
pub mod faulty {
    pub use dash_kv::faulty::*;
    pub mod toy;
}
pub mod flatfile;
#[cfg(feature = "test-hooks")]
pub mod livecopy;
#[cfg(feature = "rocksdb")]
mod rocks;
#[cfg(test)]
mod seam;
