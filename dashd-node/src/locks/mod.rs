//! The node's ChainLock handler: tip-time lock verification over one coherent validation turn,
//! the handler's chain over V, and the coinbase route's worker (LK1a F); and a masternode's
//! ChainLock and InstantSend signers (LK1c).

mod chain;
pub(crate) mod signers;
mod verify;
mod worker;

#[cfg(test)]
pub use chain::ClGate;
pub use chain::{ClPoint, NodeClChain, NodeHandler, SharedHandler, SharedRelay};
pub use signers::{LockSignerParts, LockSigners};
pub use verify::NodeClVerify;
#[cfg(test)]
pub(crate) use verify::tests::v_with_facade_over;
pub use worker::{CoinbaseGates, CoinbaseJob, CoinbasePost, CoinbaseRoute, CoinbaseWorker};
