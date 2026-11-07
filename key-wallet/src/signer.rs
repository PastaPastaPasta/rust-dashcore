//! External signer abstraction.
//!
//! A [`Signer`] answers signing requests for private keys the host does not
//! hold. It is the integration point for hardware wallets and remote signers
//! used with [`WalletType::ExternalSignable`](crate::wallet::WalletType::ExternalSignable):
//! the device owns every private key, and the host only sends derivation paths
//! plus either pre-computed sighashes or full transactions — depending on what
//! the device supports (see [`SignerMethod`]).
//!
//! The trait is async because hardware-wallet round-trips are inherently
//! asynchronous (USB, BLE, network). Soft-wallet implementations can wrap a
//! sync derive-and-sign in `async {}` without meaningful overhead.

use async_trait::async_trait;
use dashcore::Transaction;
use secp256k1::{ecdsa, PublicKey};

use crate::bip32::{DerivationPath, ExtendedPubKey};
use crate::Utxo;

/// A signing method a [`Signer`] can perform.
///
/// Callers check which methods a signer supports via
/// [`Signer::supported_methods`] and dispatch accordingly. A remote cloud
/// signer or soft wallet typically supports [`SignerMethod::Digest`] (blind
/// sighash signing). A hardware wallet protecting the user from a
/// compromised host cannot safely sign blind digests — it needs the full
/// transaction to re-hash and display — so it advertises
/// [`SignerMethod::Transaction`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignerMethod {
    /// Sign a host-computed 32-byte digest. The device trusts that the
    /// digest matches the intended transaction: fast, but offers no
    /// on-device review of what's actually being signed. Suitable for
    /// trusted remote signers and HSMs; **not** suitable for hardware
    /// wallets that defend against a compromised host.
    Digest,

    /// Sign a full Dash transaction of a given [`TransactionCategory`].
    /// The signer receives the unsigned transaction plus per-input
    /// metadata, re-hashes it internally, and (for hardware wallets) may
    /// present transaction details to the user for approval.
    ///
    /// A signer advertises one variant per category it can parse and
    /// render — hardware-wallet firmware typically ships support for
    /// categories rather than individual transaction types.
    Transaction(TransactionCategory),
}

/// Category of Dash transaction, grouped by on-chain purpose.
///
/// Categories correspond to the transaction shapes a signer has to
/// understand in order to safely display and sign them. Grouping by
/// category (rather than by the raw DIP-2 type byte) matches how
/// hardware-wallet firmware tends to gate feature support: a firmware
/// release either understands "masternode lifecycle transactions" or it
/// doesn't — not one specific sub-type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransactionCategory {
    /// Classical transaction — P2PKH / P2SH value transfer, no special
    /// payload. DIP-2 type 0.
    Classical,

    /// Platform credit flow. Locks Dash on L1 to credit Platform, or
    /// unlocks credits back to L1. DIP-2 types 8 (AssetLock) and 9
    /// (AssetUnlock).
    PlatformCredits,

    /// DIP-3 masternode lifecycle. Register, update, or revoke a
    /// masternode. DIP-2 types 3 (ProRegTx), 4 (ProUpServTx), 5
    /// (ProUpRegTx), 6 (ProUpRevTx).
    MasternodeLifecycle,
}

/// Sign on behalf of keys the host does not possess.
///
/// The `Send + Sync` supertrait bounds match how callers use a signer in
/// practice: `build_asset_lock_with_signer` (and future signer-driven
/// builders) call `&signer` methods across `.await` points, so any
/// implementor that doesn't satisfy both bounds would fail to compile at
/// the call site with a cryptic "future is not `Send`/`Sync`" message.
/// Putting the bounds on the trait surfaces the requirement up front.
///
/// A signer that only ever sees a sighash and a derivation path cannot tell
/// which coins it is spending. One that needs to bind its consent to the exact
/// inputs (for example to refuse an asset-lock drain whose coin selection no
/// longer matches what the user approved) implements
/// [`Signer::approve_inputs`], which the wallet's builders call once per
/// signed build, before any signature is requested.
#[async_trait]
pub trait Signer: Send + Sync {
    /// Error produced by the underlying signing device or service.
    ///
    /// The bound is intentionally loose — only `Display + Send + Sync +
    /// 'static` — so bring-your-own error types (including `String`) work
    /// out of the box. Implementors **should** prefer a type that also
    /// implements `std::error::Error` (derived via `thiserror` or hand-
    /// rolled) so callers can chain causes and inspect source errors;
    /// this crate's call sites currently collapse the signer error to a
    /// `String` via `Display`, which works for either shape.
    type Error: std::fmt::Display + Send + Sync + 'static;

    /// Signing methods this signer can perform. A caller that needs a
    /// method the signer doesn't advertise should fail fast rather than
    /// invoke a trait method it knows will be rejected.
    ///
    /// Returned as a borrowed slice so signers can back this with a
    /// `&'static` constant when capabilities are fixed, or with a field
    /// when they're resolved at runtime (e.g. after a firmware-version
    /// handshake).
    fn supported_methods(&self) -> &[SignerMethod];

    /// Convenience: whether `method` appears in [`Self::supported_methods`].
    fn supports(&self, method: SignerMethod) -> bool {
        self.supported_methods().contains(&method)
    }

    /// Produce an ECDSA signature over `sighash` for the key at `path`,
    /// along with the compressed public key needed to assemble the scriptSig.
    ///
    /// `sighash` is the pre-computed 32-byte message digest (e.g. a legacy
    /// P2PKH sighash). The signer must not re-derive or alter it.
    ///
    /// Only valid when the signer supports [`SignerMethod::Digest`].
    async fn sign_ecdsa(
        &self,
        path: &DerivationPath,
        sighash: [u8; 32],
    ) -> Result<(ecdsa::Signature, PublicKey), Self::Error>;

    /// Approve or refuse the inputs a build is about to sign.
    ///
    /// The wallet's signing builders call this exactly once per build, after
    /// coin selection and payload finalization and before the first
    /// [`Self::sign_ecdsa`] request. `tx` is the unsigned transaction exactly
    /// as it will be signed (special payload finalized, empty scriptSigs), and
    /// `inputs` holds exactly the coins to be signed, one per transaction
    /// input and in the same order. The call happens inside the build that
    /// selected those coins, so a caller holding its wallet lock across the
    /// build gets an approval that is atomic with coin selection.
    ///
    /// Returning an error aborts the build: no signature is requested, the
    /// build's input reservation is released, and the error is returned as a
    /// signing failure.
    ///
    /// The default approves every build.
    async fn approve_inputs(&self, tx: &Transaction, inputs: &[Utxo]) -> Result<(), Self::Error> {
        let _ = (tx, inputs);
        Ok(())
    }

    /// Return the compressed public key at `path` without signing.
    ///
    /// Used to capture per-output public keys (e.g. asset-lock credit-output
    /// keys) that the caller later references when signing Platform state
    /// transitions.
    async fn public_key(&self, path: &DerivationPath) -> Result<PublicKey, Self::Error>;
}

/// A [`Signer`] that can additionally export BIP-32 extended public keys.
///
/// Kept separate from [`Signer`] because exporting an extended public key
/// is a key-export capability, not a signing one: a pure signing backend
/// (an HSM or remote signing service whose policy forbids exporting chain
/// codes, since an xpub plus any descendant non-hardened private key
/// compromises the whole subtree) can implement [`Signer`] alone, while
/// callers that need offline descendant derivation bound on this trait.
#[async_trait]
pub trait ExtendedPubKeySigner: Signer {
    /// Return the BIP-32 extended public key at `path` — the public point
    /// plus the chain code and parent fingerprint, so the caller can
    /// non-hardened-derive a whole range of descendants from a single
    /// (possibly hardened) request without further signer round-trips
    /// (e.g. DashPay contact payment addresses under
    /// `m/9'/coin'/15'/account'/sender/recipient`).
    ///
    /// Distinct from [`Signer::public_key`], which returns only the leaf
    /// point (no chain code). A signer that cannot export an extended public
    /// key at a hardened path should return an error rather than panic.
    async fn extended_public_key(
        &self,
        path: &DerivationPath,
    ) -> Result<ExtendedPubKey, Self::Error>;
}
