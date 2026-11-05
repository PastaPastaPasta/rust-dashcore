//! The coins cache (CH2 §2.2–§2.4): Core's `CCoinsViewCache` over the coins database, in layers.
//!
//! - [`OutPointKey`]: an outpoint as the cache holds it, and its `coins` column-family key.
//! - [`PackedCoin`]: Core's `Coin` bytes, inline up to [`INLINE_MAX`].
//! - [`layer`]: one cache layer behind [`CoinLayerOps`]: [`layer::ShardedLayer`] (CH2 §2.3; the
//!   layout gate's choice).
//! - [`CoinsCache`]: the tip, active → F → prefetch → database (§2.4.1), with [`BlockOverlay`],
//!   Core's child view (§2.4.2), and the prefetch fence (§2.4.3).
//! - [`read_batch`] and [`PrefetchProducer`]: CH4's S2 producer, inline and on worker threads.
//! - [`CoinsBudget`] and [`Usage`]: Core's `nTotalSpace` and the native ledger (§2.5).
//! - [`CoinsTable`]: the `coins` column family (§2.6).

mod budget;
mod cache;
mod hasher;
mod key;
pub mod layer;
mod ledger;
#[cfg(any(test, feature = "model"))]
pub mod model;
mod overlay;
mod packed;
mod prefetch;
mod producer;
#[cfg(any(test, feature = "model"))]
pub mod reference;
mod scan_job;
mod stable;
mod stats_job;
mod table;
#[cfg(test)]
mod tests;

#[cfg(feature = "planted-bugs")]
pub use crate::planted::Planted;
pub use budget::{
    BoundaryCheck, CoinsBoundary, CoinsBudget, CoinsCacheSizeState, Ratio, Usage, core_size_arith,
};
pub use cache::{CoinSrc, CoinsCache, FrozenLayer, Resized};
/// A block as CH2 names it: `dash-chain`'s (SB4; CH2 E-b1.1).
pub use dash_chain::BlockRef;
pub use hasher::Salt;
pub(crate) use hasher::splitmix64;
pub use key::{DB_KEY_MAX, OutPointKey};
pub use layer::{CoinLayerOps, Flags, SlotRef};
pub use ledger::{HeldOut, LedgerBox};
pub use overlay::BlockOverlay;
pub use packed::{DecodeFailure, INLINE_MAX, PackedCoin};
pub use prefetch::{PrefetchBatch, PrefetchRequest, read_batch};
pub use producer::PrefetchProducer;
pub use scan_job::{CoinsScan, CoinsScanError, CoinsScanJob, CoinsScanResult, start_coins_scan};
pub use stable::StableBytes;
pub use stats_job::{CoinsDump, CoinsStats, CoinsStatsError, CoinsStatsJob, start_coins_stats};
pub use table::CoinsTable;
