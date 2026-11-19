//! Errors and lock states of wallet encryption.

use super::params::KdfParams;

/// Errors of the encryption, unlock and secret-access paths.
///
/// Each variant maps to the Dash Core RPC error noted beside it. No variant carries
/// a passphrase, key or any other input byte.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum CryptoError {
    /// Core -15.
    #[error("wallet is not encrypted")]
    NotEncrypted,
    /// Core -15.
    #[error("wallet is already encrypted")]
    AlreadyEncrypted,
    /// Core -16.
    #[error("wallet does not contain private keys")]
    NoPrivateKeys,
    /// Core -8.
    #[error("passphrase cannot be empty")]
    EmptyPassphrase,
    /// Core -14.
    #[error("the passphrase entered was incorrect")]
    IncorrectPassphrase,
    /// Core -17.
    #[error("wallet is already fully unlocked")]
    AlreadyUnlocked,
    /// Core -13.
    #[error("wallet is locked")]
    Locked,
    /// Core -13 (`EnsureWalletIsUnlocked` refuses a mixing-only unlock).
    #[error("wallet is unlocked for mixing only")]
    MixingOnly,
    /// The parameters of a write are below the policy's floor.
    #[error("key derivation parameters below policy floor")]
    WeakKdf,
    /// Opening the file needs more KDF work than the caller's budget allows.
    #[error("key derivation needs more than the unauthenticated budget")]
    KdfOverBudget {
        /// The parameters the file asks for.
        required: KdfParams,
    },
    /// The file's KDF parameters exceed the absolute ceiling.
    #[error("key derivation parameters above the absolute ceiling")]
    KdfAboveCeiling,
    /// An unknown version, kind, flag or trailing data.
    #[error("unsupported encrypted format")]
    UnsupportedFormat,
    /// An AEAD failure on a record (not on the master-key unwrap).
    #[error("encrypted data is corrupt or was tampered with")]
    Corrupt,
    /// The KDF's memory could not be reserved.
    #[error("not enough memory for key derivation")]
    OutOfMemory,
    /// The system random number generator failed.
    #[error("random number generator failed")]
    Rng,
}

/// How far an encrypted wallet is unlocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockMode {
    /// Every operation is allowed.
    Full,
    /// Only CoinJoin and key-pool top-up may use the secrets (Core `mixingonly`).
    MixingOnly,
}

/// The lock state of a wallet, as Dash Core's Qt `WalletModel` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockState {
    /// The wallet is not encrypted; its secrets are always available.
    Unencrypted,
    /// Encrypted and locked.
    Locked,
    /// Encrypted and unlocked for CoinJoin and key-pool top-up only.
    UnlockedMixingOnly,
    /// Encrypted and fully unlocked.
    Unlocked,
}

/// What an operation needs the wallet's secrets for. A mixing-only unlock permits
/// only [`CoinJoin`](Self::CoinJoin) and [`KeyPoolTopUp`](Self::KeyPoolTopUp).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretUse {
    /// Any ordinary use (deriving private keys, signing a transaction).
    General,
    /// CoinJoin mixing.
    CoinJoin,
    /// Topping up the key pool.
    KeyPoolTopUp,
    /// Exporting a secret (seed, mnemonic, passphrase, private key).
    Export,
    /// Signing a message.
    MessageSign,
}

impl SecretUse {
    /// Whether a mixing-only unlock allows this use (Core `IsLocked(true)` callers).
    pub fn allowed_while_mixing_only(self) -> bool {
        matches!(self, SecretUse::CoinJoin | SecretUse::KeyPoolTopUp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_carry_no_input() {
        let over = CryptoError::KdfOverBudget {
            required: KdfParams::DESKTOP_FLOOR,
        };
        assert_eq!(over.to_string(), "key derivation needs more than the unauthenticated budget");
        assert_eq!(CryptoError::Locked.to_string(), "wallet is locked");
    }

    #[test]
    fn mixing_only_uses() {
        let allowed: Vec<_> = [
            SecretUse::General,
            SecretUse::CoinJoin,
            SecretUse::KeyPoolTopUp,
            SecretUse::Export,
            SecretUse::MessageSign,
        ]
        .into_iter()
        .filter(|u| u.allowed_while_mixing_only())
        .collect();
        assert_eq!(allowed, [SecretUse::CoinJoin, SecretUse::KeyPoolTopUp]);
    }
}
