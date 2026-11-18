// SPDX-License-Identifier: MIT

//! BIP158 compact block filters as Dash Core v24 defines them (node program task SB9).
//!
//! * [`gcs`]: the Golomb-coded-set codec (Core's `GCSFilter`);
//! * [`elements`]: the Dash element set of the basic filter, with the special-transaction payload
//!   fields (Core's `BasicFilterElements` and `ExtractSpecialTxFilterElements`);
//! * [`chain`]: the filter hash and the filter-header chain check (BIP157);
//! * [`BlockFilter`]: one block's filter, Core's `BlockFilter`.
//!
//! The crate is pure and shared (layer P, SB0 §2.3): no I/O, threads, clocks or sockets. The node
//! builds filters with it (IX1) and serves them (P6b); SPV checks and matches the ones it
//! downloads. It takes base-sdk types; `dashcore::bip158` converts to them and keeps the
//! signatures dashcore callers use (SB0 §5.5).

pub mod chain;
pub mod elements;
pub mod gcs;
pub mod siphash;

mod block_filter;

pub use block_filter::{BlockFilter, BuildError};
pub use chain::{
    ChainError, FilterHash, FilterHeader, check_filter, check_links, extend_headers, filter_hash,
    filter_header,
};
pub use gcs::{
    BASIC_FILTER_M, BASIC_FILTER_P, ByteSource, FilterError, FilterQuery, GcsParams, SliceSource,
    check_encoded, encode, match_all, match_all_from, match_any, match_any_from, read_count,
};
