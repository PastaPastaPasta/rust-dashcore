//! `WalletManager` entry points that take wallet secrets beyond a plain BIP39 phrase.

use crate::{WalletError, WalletId, WalletManager};
use dashcore::prelude::CoreBlockHeight;
use key_wallet::mnemonic::{MnemonicCompat, WalletMnemonic};
use key_wallet::wallet::initialization::WalletAccountCreationOptions;
use key_wallet::wallet::managed_wallet_info::wallet_info_interface::WalletInfoInterface;
use key_wallet::{Passphrase, Wallet};

impl<T: WalletInfoInterface + Send + Sync + 'static> WalletManager<T> {
    /// Create a wallet from `phrase` and a BIP39 `passphrase` under `compat` and add
    /// it to the manager. Returns the wallet ID.
    ///
    /// [`MnemonicCompat::DashCore`] accepts and hashes the phrase exactly as Dash
    /// Core does; [`MnemonicCompat::Bip39`] with an empty passphrase gives the same
    /// wallet as [`WalletManager::create_wallet_from_mnemonic`].
    ///
    /// # Errors
    /// `InvalidMnemonic` when the phrase is refused (the message names a position or
    /// count, never a word), `WalletCreation` when the wallet cannot be built (for
    /// example a BIP39 phrase with a passphrase that is not UTF-8), `WalletExists`
    /// when the manager already holds it.
    pub fn create_wallet_from_mnemonic_with_passphrase(
        &mut self,
        phrase: &str,
        passphrase: Passphrase,
        compat: MnemonicCompat,
        birth_height: CoreBlockHeight,
        account_creation_options: WalletAccountCreationOptions,
    ) -> Result<WalletId, WalletError> {
        let mnemonic = WalletMnemonic::parse(phrase, compat)
            .map_err(|e| WalletError::InvalidMnemonic(e.to_string()))?;
        let wallet = Wallet::from_mnemonic_with_passphrase(
            mnemonic,
            passphrase,
            self.network,
            account_creation_options,
        )
        .map_err(|e| WalletError::WalletCreation(e.to_string()))?;

        let wallet_id = wallet.compute_wallet_id();
        if self.wallets.contains_key(&wallet_id) {
            return Err(WalletError::WalletExists(wallet_id));
        }
        let managed_info = T::from_wallet(&wallet, birth_height);
        self.wallets.insert(wallet_id, wallet);
        self.wallet_infos.insert(wallet_id, managed_info);
        self.bump_structural_revision();
        Ok(wallet_id)
    }
}

#[cfg(test)]
mod tests {
    use crate::WalletManager;
    use key_wallet::mnemonic::MnemonicCompat;
    use key_wallet::wallet::initialization::WalletAccountCreationOptions;
    use key_wallet::wallet::managed_wallet_info::ManagedWalletInfo;
    use key_wallet::wallet::WalletType;
    use key_wallet::{Network, Passphrase};

    const PHRASE: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn manager() -> WalletManager<ManagedWalletInfo> {
        WalletManager::new(Network::Testnet)
    }

    #[test]
    fn empty_bip39_passphrase_is_the_plain_mnemonic_wallet() {
        let mut plain = manager();
        let plain_id = plain
            .create_wallet_from_mnemonic(PHRASE, 0, WalletAccountCreationOptions::Default)
            .unwrap();
        let mut with = manager();
        let id = with
            .create_wallet_from_mnemonic_with_passphrase(
                PHRASE,
                Passphrase::new(Vec::new()),
                MnemonicCompat::Bip39,
                0,
                WalletAccountCreationOptions::Default,
            )
            .unwrap();
        assert_eq!(id, plain_id);
        assert!(matches!(with.get_wallet(&id).unwrap().wallet_type, WalletType::Mnemonic { .. }));
    }

    #[test]
    fn passphrase_and_compat_give_distinct_wallets() {
        let mut m = manager();
        let trezor = || Passphrase::from_slice(b"TREZOR");
        let bip = m
            .create_wallet_from_mnemonic_with_passphrase(
                PHRASE,
                trezor(),
                MnemonicCompat::Bip39,
                0,
                WalletAccountCreationOptions::Default,
            )
            .unwrap();
        // Same strict phrase and ASCII passphrase: Core's seed is the BIP39 seed.
        let err = m
            .create_wallet_from_mnemonic_with_passphrase(
                PHRASE,
                trezor(),
                MnemonicCompat::DashCore,
                0,
                WalletAccountCreationOptions::Default,
            )
            .unwrap_err();
        assert!(matches!(err, crate::WalletError::WalletExists(id) if id == bip));
        let other = m
            .create_wallet_from_mnemonic_with_passphrase(
                PHRASE,
                Passphrase::from_slice(b"TREZOR\0"),
                MnemonicCompat::DashCore,
                0,
                WalletAccountCreationOptions::Default,
            )
            .unwrap();
        assert_ne!(other, bip);
        assert!(matches!(
            m.get_wallet(&other).unwrap().wallet_type,
            WalletType::MnemonicWithPassphrase { .. }
        ));
    }

    #[test]
    fn errors_name_no_word() {
        let bad = PHRASE.replace("about", "hunter");
        let err = manager()
            .create_wallet_from_mnemonic_with_passphrase(
                &bad,
                Passphrase::new(Vec::new()),
                MnemonicCompat::DashCore,
                0,
                WalletAccountCreationOptions::Default,
            )
            .unwrap_err();
        assert!(matches!(err, crate::WalletError::InvalidMnemonic(_)));
        assert!(!err.to_string().contains("hunter"), "{err}");
    }

    #[cfg(feature = "bincode")]
    #[test]
    fn import_reads_the_envelope() {
        let mut m = manager();
        let id = m
            .create_wallet_from_mnemonic_with_passphrase(
                PHRASE,
                Passphrase::from_slice(b"pass"),
                MnemonicCompat::DashCore,
                0,
                WalletAccountCreationOptions::Default,
            )
            .unwrap();
        let bytes = m.get_wallet(&id).unwrap().to_bytes().unwrap();
        let mut other = manager();
        assert_eq!(other.import_wallet_from_bytes(&bytes, 0).unwrap(), id);
    }
}
