//! Core's disconnect pool, `DisconnectedBlockTransactions` (`kernel/disconnected_transactions.h`;
//! CH3 §4.3, D25): the transactions of the blocks a reorg disconnects, kept to re-add to the
//! mempool once it has connected the new chain.
//!
//! - [`DisconnectedBlockTransactions`]: the pool, the production [`DisconnectPool`].
//! - [`memusage`]: Core's memory accounting, the pool's logical budget.
//! - [`buckets`]: the bucket count of libstdc++'s `std::unordered_map`, which the accounting reads.
//! - `PoolsOnly` (test support): a mempool of the pools alone, counting what they evict and
//!   logging V's calls on them.
//!
//! [`DisconnectPool`]: crate::chain::DisconnectPool

pub mod buckets;
mod disconnected;
pub mod memusage;
#[cfg(feature = "test-support")]
mod pools_only;
#[cfg(test)]
mod tests;

pub use disconnected::{DisconnectedBlockTransactions, MAX_DISCONNECTED_TX_POOL_SIZE};
#[cfg(feature = "test-support")]
pub use pools_only::{PoolCall, PoolsOnly};
