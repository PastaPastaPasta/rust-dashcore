//! The storage engine contract shared by the Rust Dash node and dash-spv (SB0 §2.2, §9).
//!
//! - [`KvEngine`]: the narrow engine trait every database goes through: point and batched
//!   reads, prefix iteration, atomic multi-column-family write batches (buffered or durable
//!   commit), snapshots and explicit durability barriers. It encodes no ordering with block
//!   files or other databases; the flush protocol composes the primitives.
//! - [`MemoryEngine`]: a `BTreeMap` per column family, for unit tests and the `kv_model` fuzz
//!   target. The node's RocksDB engine is in `dashd-store`; the SPV store's SQLite engine is in
//!   `dash-kv-sqlite`.
//! - [`Table`]: a typed view of one column family through explicit [`KeyCodec`]/[`ValueCodec`]
//!   implementations. No serde or bincode touches stored bytes (FR-STO-05).
//! - [`DbSchema`]: the column families of a database and its schema version, stamped into the
//!   `meta` column family on creation and checked on open (FR-STO-02).
//! - [`fail_stop`], [`check_schema`], [`check_record`] and [`StoreError::engine`]: what an engine
//!   implementation needs to follow the contract's fail-stop and schema rules.
//! - [`fs`]: the filesystem seam block files go through ([`FlatFs`]).
//! - [`GlobalFailStop`] and [`AbortSink`]: the fail-stop latch every store of a node (or of an SPV
//!   client) shares, and where its fatal failures go (CH2 §2.4.4, BL-109); [`LatchedEngine`]
//!   puts an engine of a shared-runtime store behind it.
//! - [`BlockingIo`] and [`run_blocking`]: the host's bounded service that runs blocking storage
//!   work, as a shared crate starts no threads of its own (SB0 §7, L5).
//! - `faulty` (feature `faulty`): ST3a's power-loss model; `model` (feature `model`): the
//!   `kv_model` differential driver, and `contract`, the suite every engine's tests run
//!   (`engine_contract_tests!`). All test-only.

mod blocking;
pub mod codec;
#[cfg(any(test, feature = "model"))]
pub mod contract;
mod engine;
mod error;
pub mod fail_stop;
#[cfg(feature = "faulty")]
pub mod faulty;
pub mod fs;
mod latch;
mod latched;
mod memory;
#[cfg(any(test, feature = "model"))]
pub mod model;
mod schema;
mod table;

pub use blocking::{BlockingError, BlockingIo, InlineIo, Job, Pending, run_blocking};
pub use codec::{BeU32, BeU64, CodecError, KeyCodec, ValueCodec};
pub use engine::{Cf, Commit, KvBatch, KvEngine, KvRead};
pub use error::{ErrorKind, Result, StoreError};
pub use fs::{FlatFs, FlatRead, OpenFailed, open_by_path};
pub use latch::{
    AbortMark, AbortSink, DeferScope, Failure, Fatal, GlobalFailStop, Parker, RecordingSink,
    Registration,
};
pub use latched::{LatchedEngine, LatchedSnapshot, Sites};
pub use memory::{MemoryBatch, MemoryEngine, MemorySnapshot};
pub use schema::{
    CfProfile, CfSpec, DbSchema, SCHEMA_KEY, SchemaState, check_record, check_schema, schema_record,
};
pub use table::Table;
