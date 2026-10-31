//! CH7a's evo integration (CH7 §2.1): the evo store under V, its layer and its read views.
//!
//! - [`codec`]: the evo rows' keys and values (A1 §2.9), and `EvoDelta` ⇄ rows;
//! - [`EvoLayer`]: the committed rows not yet durable, as merged indexes (D22), and CH2's evo
//!   window;
//! - [`RocksEvoStore`]: the evo column families under the layer;
//! - [`EvoReadView`]: a pinned read of all three, dash-dml's `EvoStore` (D17);
//! - [`caches`]: the per-chainstate caches Core keeps for cost (§3.7);
//! - [`effects`]: the attempt log's appliers (D12);
//! - [`ev1`]: the seam EV1's checks bind in CH7a-2b, and its stand-in (D44);
//! - [`cl`]: H4's step 10 with no best ChainLock, until CH7b (D45);
//! - [`guard`]: readiness guard G1q, until CH7b-2 (D3);
//! - [`node`]: `NodeEvo`, the production evo hooks (§2.1);
//! - [`not_built`]: the readiness stubs H4 runs on before CH7b (D2).

pub mod caches;
pub mod cl;
pub mod codec;
pub mod effects;
pub mod ev1;
pub mod guard;
mod layer;
pub mod node;
pub mod not_built;
mod store;
mod view;

pub use codec::{EvoCf, EvoCodec};
pub use layer::{EvoIndex, EvoLayer, EvoWindowDelta, Lookup};
pub use store::RocksEvoStore;
pub use view::EvoReadView;

#[cfg(test)]
pub(crate) mod testkit;
#[cfg(test)]
mod tests;
