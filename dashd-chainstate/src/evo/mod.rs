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
//! - [`node`]: `NodeEvo`, the production evo hooks (§2.1).

pub mod caches;
pub mod cl;
pub mod codec;
pub mod effects;
pub mod ev1;
pub mod gates;
mod layer;
pub mod node;
pub mod state_hash;
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
