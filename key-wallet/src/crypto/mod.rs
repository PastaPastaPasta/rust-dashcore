//! Wallet encryption support types.
//!
//! The error, lock-state and KDF parameter types are always compiled, so APIs that
//! exist without the `encryption` feature (such as [`Wallet::seed_zeroizing`] and
//! [`Wallet::require`]) can name them.
//!
//! [`Wallet::seed_zeroizing`]: crate::Wallet::seed_zeroizing
//! [`Wallet::require`]: crate::Wallet::require

pub mod error;
pub mod params;

pub use error::{CryptoError, LockState, SecretUse, UnlockMode};
pub use params::{KdfBudget, KdfParams};
