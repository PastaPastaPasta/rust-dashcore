//! The chainstate database (CH2 §2.6, §2.10): its schema, and the guarded engine every coins read
//! goes through.
//!
//! [`GuardedEngine`]'s and [`GuardedSnapshot`]'s reads trip the node-wide fail-stop latch on an
//! engine error or a stored coin that does not decode, and only they construct a
//! [`CoinsReadError`]; its writes and barriers are refused once the latch has tripped, as are
//! [`GuardedFiles`]' mutations. [`CoreAbortSink`] is the production [`AbortSink`]. [`Meta`] holds
//! the `meta` records, and [`wipe_chainstate`] is `-reindex-chainstate`'s wipe. [`NodeBlockFiles`]
//! is V's view of the block files.

mod abort;
mod blocks;
mod guarded;
mod meta;
#[cfg(any(test, feature = "model"))]
mod probe;
mod schema;

pub use abort::CoreAbortSink;
pub use blocks::{NodeBlockFiles, scan_already_pruned_files};
pub(crate) use guarded::InitReads;
// The latch and the sink are `dash_kv`'s (SB10, BL-109), re-exported under their CH2 paths.
pub use dash_kv::{
    AbortMark, AbortSink, Failure, Fatal, GlobalFailStop, Parker, RecordingSink, Registration,
};
pub use guarded::{CoinsReadError, GuardedEngine, GuardedFiles, GuardedRead, GuardedSnapshot};
pub(crate) use meta::{EFFECT_CFS, EVO_TIP, nonempty_wiped_cf};
#[cfg(feature = "crash")]
pub(crate) use meta::{HEAD_BLOCKS, LAST_FILE, PRUNED_FILES, TIP};
pub use meta::{Meta, wipe_chainstate};
#[cfg(any(test, feature = "model"))]
pub use probe::ProbeEngine;
pub use schema::CHAINSTATE_SCHEMA;

/// An index position as the block files name it; `None` is Core's null position (`nFile == -1`).
pub(crate) fn store_pos(p: dash_chain::FlatFilePos) -> Option<dashd_store::flatfile::FlatFilePos> {
    Some(dashd_store::flatfile::FlatFilePos::new(u32::try_from(p.file).ok()?, p.pos))
}
