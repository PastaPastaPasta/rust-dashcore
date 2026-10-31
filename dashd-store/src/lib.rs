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
#[cfg(feature = "rocksdb")]
pub use rocks::{RocksBatch, RocksConfig, RocksEngine, RocksMemory, RocksSnapshot, STORE_MARKER};

/// ST3a's power-loss model: `dash_kv::faulty`, and the node's systems under test over block files.
#[cfg(feature = "faulty")]
pub mod faulty {
    pub use dash_kv::faulty::*;
    pub mod toy;
}
pub mod flatfile;
#[cfg(feature = "rocksdb")]
mod rocks;
