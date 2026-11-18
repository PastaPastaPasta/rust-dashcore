// SPDX-License-Identifier: MIT

//! The transaction index (Core's `TxIndex`): its store, row codec and data object.

pub mod codec;
pub mod data;
pub mod reader;

use dashd_chainstate::subscriber::RowSchema;
use dashd_store::{Cf, CfProfile, CfSpec, DbSchema};

pub use data::TxIndexData;
pub use reader::{StoredTxIndex, TxFiles};

/// The index's own store.
pub static TXINDEX_DB: DbSchema = DbSchema {
    name: "txindex",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("t", CfProfile::Ordered)],
};

/// The locator and the epoch row.
pub const META: Cf = DbSchema::META;
/// One row per indexed transaction position.
pub const TX: Cf = Cf::new(1);

// Core: index/base.cpp:27
pub const BEST_BLOCK: &[u8] = b"B";
/// The process epoch, outside the row schema: the data object writes it before the driver opens.
pub const EPOCH: &[u8] = b"E";

/// Inexact rows only: no journal, no state keys.
pub static TXINDEX_ROWS: RowSchema = RowSchema {
    height_cfs: &[],
    archive_cfs: &[],
    state_keys: &[],
    keyed_cfs: &[],
    inexact_cfs: &[TX],
    has_files: false,
    locator: (META, BEST_BLOCK),
};
