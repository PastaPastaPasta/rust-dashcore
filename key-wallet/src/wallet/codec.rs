//! Hand-written bincode codec for [`Wallet`] and [`WalletType`].
//!
//! The bytes are identical to what `derive(Encode, Decode)` produced (pinned by the
//! `wallet_layout_v0` goldens), but each `WalletType` variant has an explicit tag,
//! so declaration order or a `cfg`-gated variant can never shift one.
//!
//! Two forms share the tags:
//! - the **legacy** form (`Encode`/`Decode` on `Wallet` and `WalletType`, as
//!   `Wallet::backup` and `bincode::encode_to_vec` use them) knows tags 0–4 only. Its
//!   encoder refuses a later wallet type and its decoder rejects tags 5–7 like any
//!   unknown tag, in every build;
//! - the **envelope** form ([`EnvelopeCore`], the core section of the versioned
//!   wallet file, `wallet/file.rs`) also knows tag 5.
//!
//! `BorrowDecode` goes through `Decode`: no field borrows from the input, and every
//! field's two impls decode the same bytes the same way.

use bincode::de::{BorrowDecoder, Decoder};
use bincode::enc::Encoder;
use bincode::error::{AllowedEnumVariants, DecodeError, EncodeError};
use bincode::{BorrowDecode, Decode, Encode};
use zeroize::Zeroizing;

use super::{Wallet, WalletType};
use crate::mnemonic::{MnemonicCompat, WalletMnemonic};
use crate::secret::Passphrase;
use crate::seed::Seed;

const TAG_MNEMONIC: u32 = 0;
const TAG_SEED: u32 = 1;
const TAG_EXTENDED_PRIV_KEY: u32 = 2;
const TAG_EXTERNAL_SIGNABLE: u32 = 3;
const TAG_WATCH_ONLY: u32 = 4;
const TAG_MNEMONIC_WITH_PASSPHRASE: u32 = 5;

const LEGACY_TAGS: AllowedEnumVariants = AllowedEnumVariants::Range {
    min: TAG_MNEMONIC,
    max: TAG_WATCH_ONLY,
};

const ENVELOPE_TAGS: AllowedEnumVariants = AllowedEnumVariants::Range {
    min: TAG_MNEMONIC,
    max: TAG_MNEMONIC_WITH_PASSPHRASE,
};

/// Which tags a codec knows.
#[derive(Clone, Copy, PartialEq)]
enum Form {
    Legacy,
    Envelope,
}

/// Whether `wallet_type` can be written in the legacy form.
pub(super) fn is_legacy(wallet_type: &WalletType) -> bool {
    match wallet_type {
        WalletType::Mnemonic {
            ..
        }
        | WalletType::Seed {
            ..
        }
        | WalletType::ExtendedPrivKey(_)
        | WalletType::ExternalSignable
        | WalletType::WatchOnly => true,
        WalletType::MnemonicWithPassphrase {
            ..
        } => false,
    }
}

/// The variant-5 invariants every writer keeps and the reader requires: BIP39 hashes a
/// UTF-8 passphrase (the constructors' rule), and a BIP39 mnemonic with an empty
/// passphrase is variant 0, never 5 (the one canonical form). The message names no
/// secret.
fn check_mnemonic_with_passphrase(
    mnemonic: &WalletMnemonic,
    passphrase: &Passphrase,
) -> Result<(), &'static str> {
    if mnemonic.compat() != MnemonicCompat::Bip39 {
        return Ok(());
    }
    if passphrase.is_empty() {
        return Err("a BIP39 wallet with an empty passphrase is WalletType::Mnemonic");
    }
    if core::str::from_utf8(passphrase.expose()).is_err() {
        return Err("BIP39 wallet passphrase is not valid UTF-8");
    }
    Ok(())
}

fn encode_wallet_type<E: Encoder>(
    wallet_type: &WalletType,
    form: Form,
    encoder: &mut E,
) -> Result<(), EncodeError> {
    match wallet_type {
        WalletType::Mnemonic {
            mnemonic,
            root_extended_private_key,
        } => {
            TAG_MNEMONIC.encode(encoder)?;
            mnemonic.encode(encoder)?;
            root_extended_private_key.encode(encoder)
        }
        WalletType::Seed {
            seed,
            root_extended_private_key,
        } => {
            TAG_SEED.encode(encoder)?;
            seed.encode(encoder)?;
            root_extended_private_key.encode(encoder)
        }
        WalletType::ExtendedPrivKey(root_extended_private_key) => {
            TAG_EXTENDED_PRIV_KEY.encode(encoder)?;
            root_extended_private_key.encode(encoder)
        }
        WalletType::ExternalSignable => TAG_EXTERNAL_SIGNABLE.encode(encoder),
        WalletType::WatchOnly => TAG_WATCH_ONLY.encode(encoder),
        WalletType::MnemonicWithPassphrase {
            mnemonic,
            passphrase,
            root_extended_private_key,
        } => {
            if form == Form::Legacy {
                return Err(EncodeError::Other(
                    "this wallet type needs the versioned wallet file (Wallet::to_bytes)",
                ));
            }
            check_mnemonic_with_passphrase(mnemonic, passphrase).map_err(EncodeError::Other)?;
            TAG_MNEMONIC_WITH_PASSPHRASE.encode(encoder)?;
            mnemonic.encode(encoder)?;
            passphrase.encode(encoder)?;
            root_extended_private_key.encode(encoder)
        }
    }
}

fn decode_wallet_type<D: Decoder>(form: Form, decoder: &mut D) -> Result<WalletType, DecodeError> {
    Ok(match u32::decode(decoder)? {
        TAG_MNEMONIC => WalletType::Mnemonic {
            mnemonic: Decode::decode(decoder)?,
            root_extended_private_key: Decode::decode(decoder)?,
        },
        TAG_SEED => {
            // `Seed` is `Copy` with no `Drop`: wipe this copy if the key fails to decode.
            let seed = Zeroizing::new(Seed::decode(decoder)?);
            WalletType::Seed {
                root_extended_private_key: Decode::decode(decoder)?,
                seed: *seed,
            }
        }
        TAG_EXTENDED_PRIV_KEY => WalletType::ExtendedPrivKey(Decode::decode(decoder)?),
        TAG_EXTERNAL_SIGNABLE => WalletType::ExternalSignable,
        TAG_WATCH_ONLY => WalletType::WatchOnly,
        TAG_MNEMONIC_WITH_PASSPHRASE if form == Form::Envelope => {
            let mnemonic: WalletMnemonic = Decode::decode(decoder)?;
            let passphrase: Passphrase = Decode::decode(decoder)?;
            check_mnemonic_with_passphrase(&mnemonic, &passphrase)
                .map_err(|e| DecodeError::OtherString(e.into()))?;
            WalletType::MnemonicWithPassphrase {
                mnemonic,
                passphrase,
                root_extended_private_key: Decode::decode(decoder)?,
            }
        }
        found => {
            return Err(DecodeError::UnexpectedVariant {
                type_name: "WalletType",
                allowed: match form {
                    Form::Legacy => &LEGACY_TAGS,
                    Form::Envelope => &ENVELOPE_TAGS,
                },
                found,
            })
        }
    })
}

impl Encode for WalletType {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        encode_wallet_type(self, Form::Legacy, encoder)
    }
}

impl<C> Decode<C> for WalletType {
    fn decode<D: Decoder<Context = C>>(decoder: &mut D) -> Result<Self, DecodeError> {
        decode_wallet_type(Form::Legacy, decoder)
    }
}

impl<'de, C> BorrowDecode<'de, C> for WalletType {
    fn borrow_decode<D: BorrowDecoder<'de, Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::decode(decoder)
    }
}

fn encode_wallet<E: Encoder>(
    wallet: &Wallet,
    form: Form,
    encoder: &mut E,
) -> Result<(), EncodeError> {
    wallet.network.encode(encoder)?;
    wallet.wallet_id.encode(encoder)?;
    encode_wallet_type(&wallet.wallet_type, form, encoder)?;
    wallet.accounts.encode(encoder)
}

fn decode_wallet<D: Decoder>(form: Form, decoder: &mut D) -> Result<Wallet, DecodeError> {
    Ok(Wallet {
        network: Decode::decode(decoder)?,
        wallet_id: Decode::decode(decoder)?,
        wallet_type: decode_wallet_type(form, decoder)?,
        accounts: Decode::decode(decoder)?,
    })
}

/// Fields in declaration order, as the derive wrote them.
impl Encode for Wallet {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        encode_wallet(self, Form::Legacy, encoder)
    }
}

impl<C> Decode<C> for Wallet {
    fn decode<D: Decoder<Context = C>>(decoder: &mut D) -> Result<Self, DecodeError> {
        decode_wallet(Form::Legacy, decoder)
    }
}

impl<'de, C> BorrowDecode<'de, C> for Wallet {
    fn borrow_decode<D: BorrowDecoder<'de, Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::decode(decoder)
    }
}

/// The core section of the versioned wallet file: the legacy field layout, with the
/// envelope's wallet-type tags. Encodes a borrowed wallet; decodes into an owned one.
pub(super) struct EnvelopeCore<W>(pub W);

impl Encode for EnvelopeCore<&Wallet> {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        encode_wallet(self.0, Form::Envelope, encoder)
    }
}

impl<C> Decode<C> for EnvelopeCore<Wallet> {
    fn decode<D: Decoder<Context = C>>(decoder: &mut D) -> Result<Self, DecodeError> {
        decode_wallet(Form::Envelope, decoder).map(EnvelopeCore)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::AccountCollection;
    use crate::Network;

    const CONFIG: bincode::config::Configuration = bincode::config::standard();

    /// A keyless wallet whose type tag is `tag`.
    fn wallet_bytes_with_tag(tag: u32) -> Vec<u8> {
        let fields = (Network::Testnet, [7u8; 32], tag, AccountCollection::default());
        bincode::encode_to_vec(fields, CONFIG).unwrap()
    }

    #[test]
    fn keyless_tags_decode() {
        for (tag, watch_only) in [(TAG_EXTERNAL_SIGNABLE, false), (TAG_WATCH_ONLY, true)] {
            let bytes = wallet_bytes_with_tag(tag);
            let (wallet, read): (Wallet, usize) =
                bincode::decode_from_slice(&bytes, CONFIG).unwrap();
            assert_eq!(read, bytes.len());
            assert_eq!(matches!(wallet.wallet_type, WalletType::WatchOnly), watch_only);
            assert_eq!(bincode::encode_to_vec(&wallet, CONFIG).unwrap(), bytes);
        }
    }

    fn assert_bad_tag<T: core::fmt::Debug>(result: Result<T, DecodeError>, tag: u32) {
        assert_bad_tag_in(result, tag, TAG_WATCH_ONLY);
    }

    fn assert_bad_tag_in<T: core::fmt::Debug>(
        result: Result<T, DecodeError>,
        tag: u32,
        expected_max: u32,
    ) {
        match result {
            Err(DecodeError::UnexpectedVariant {
                type_name: "WalletType",
                allowed:
                    AllowedEnumVariants::Range {
                        min: 0,
                        max,
                    },
                found,
            }) => assert_eq!((found, *max), (tag, expected_max)),
            other => panic!("tag {tag}: {other:?}"),
        }
    }

    /// Tags 5–7 are reserved for later wallet types; the legacy decoder refuses
    /// them (and every other unknown tag) through every entry point.
    #[test]
    fn reserved_and_unknown_tags_are_rejected() {
        for tag in [5, 6, 7, 8, 255, u32::MAX] {
            let bytes = wallet_bytes_with_tag(tag);
            assert_bad_tag(bincode::decode_from_slice::<Wallet, _>(&bytes, CONFIG), tag);
            assert_bad_tag(bincode::borrow_decode_from_slice::<Wallet, _>(&bytes, CONFIG), tag);
            let tag_bytes = bincode::encode_to_vec(tag, CONFIG).unwrap();
            assert_bad_tag(bincode::decode_from_slice::<WalletType, _>(&tag_bytes, CONFIG), tag);
            match Wallet::restore(&bytes) {
                Err(crate::Error::Serialization(message)) => {
                    assert!(message.contains(&format!("found: {tag} ")), "tag {tag}: {message}")
                }
                other => panic!("tag {tag}: {:?}", other.map(|_| ())),
            }
        }
    }
}
