//! Lock state and the gate every secret-reading path goes through.
//!
//! Always compiled: without an encrypted wallet type every wallet is
//! [`LockState::Unencrypted`] and [`Wallet::require`] always succeeds.

use super::{Wallet, WalletType};
use crate::crypto::{CryptoError, LockState, SecretUse};

impl Wallet {
    /// Whether the wallet's secrets are encrypted, and how far they are unlocked.
    pub fn lock_state(&self) -> LockState {
        match &self.wallet_type {
            WalletType::Mnemonic {
                ..
            }
            | WalletType::Seed {
                ..
            }
            | WalletType::ExtendedPrivKey(_)
            | WalletType::ExternalSignable
            | WalletType::WatchOnly
            | WalletType::MnemonicWithPassphrase {
                ..
            } => LockState::Unencrypted,
        }
    }

    /// Checks that the wallet's secrets may be used for `use_`: always for an
    /// unencrypted or fully unlocked wallet; for CoinJoin and key-pool top-up only
    /// while unlocked for mixing; never while locked. Says nothing about whether
    /// the wallet holds secrets at all.
    pub fn require(&self, use_: SecretUse) -> Result<(), CryptoError> {
        match self.lock_state() {
            LockState::Unencrypted | LockState::Unlocked => Ok(()),
            LockState::UnlockedMixingOnly if use_.allowed_while_mixing_only() => Ok(()),
            LockState::UnlockedMixingOnly => Err(CryptoError::MixingOnly),
            LockState::Locked => Err(CryptoError::Locked),
        }
    }
}
