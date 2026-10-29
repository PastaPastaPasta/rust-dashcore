//! CH7a's evo integration (CH7 §2.1): the evo store under V, its layer and its read views.
//!
//! - [`codec`]: the evo rows' keys and values (A1 §2.9), and `EvoDelta` ⇄ rows;
//! - [`EvoLayer`]: the committed rows not yet durable, as merged indexes (D22), and CH2's evo
//!   window;
//! - [`RocksEvoStore`]: the evo column families under the layer;
//! - [`EvoReadView`]: a pinned read of all three, dash-dml's `EvoStore` (D17).

pub mod codec;
mod layer;
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
