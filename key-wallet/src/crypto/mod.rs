//! Wallet encryption support types.
//!
//! The error, lock-state and KDF parameter types are always compiled, so APIs that
//! exist without the `encryption` feature (such as [`Wallet::try_wallet_seed_bytes`] and
//! [`Wallet::require`]) can name them.
//!
//! [`Wallet::try_wallet_seed_bytes`]: crate::Wallet::try_wallet_seed_bytes
//! [`Wallet::require`]: crate::Wallet::require

pub mod error;
pub mod params;

pub use error::{CryptoError, LockState, SecretUse, UnlockMode};
pub use params::{KdfBudget, KdfParams};
