//! Choosing between strict BIP39 and Dash Core's mnemonic rules.

use unicode_normalization::UnicodeNormalization;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[cfg(feature = "bincode")]
use bincode_derive::{Decode, Encode};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use super::dash_core::{self, DashCoreMnemonic, CORE_SALT_MAX_BYTES};
use super::Mnemonic;
use crate::secret::Passphrase;

/// How a phrase is validated and turned into a seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
pub enum MnemonicCompat {
    /// BIP39 as specified: full checksum, NFKD on phrase and passphrase, all 10
    /// word lists.
    #[default]
    Bip39,
    /// Dash Core's `CMnemonic` (`bip39.cpp:78-149`): English only, Core's checksum
    /// mask, raw bytes (no NFKD), salt (`"mnemonic"` ‖ passphrase) cut at 256 bytes.
    DashCore,
}

/// Why a phrase, entropy or passphrase was refused. Never carries the phrase or
/// any word, only positions and counts.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum MnemonicError {
    /// The phrase is empty.
    #[error("mnemonic is empty")]
    Empty,
    /// The number of words is not allowed.
    #[error("mnemonic word count {0} is not 12, 15, 18, 21 or 24")]
    WordCount(usize),
    /// The word at this zero-based position is not in the word list.
    #[error("mnemonic word {index} is not in the word list")]
    UnknownWord {
        /// Zero-based word position.
        index: usize,
    },
    /// The checksum does not match.
    #[error("mnemonic checksum mismatch")]
    Checksum,
    /// The entropy length is not allowed.
    #[error("entropy length {0} is not 16..=32 bytes in steps of 4")]
    EntropyLength(usize),
    /// The requested strength is not allowed.
    #[error("mnemonic bits {0} not in 128, 160, 192, 224, 256")]
    Bits(u32),
    /// BIP39 needs a UTF-8 passphrase (it is NFKD-normalised).
    #[error("passphrase is not valid UTF-8 (required for BIP39 NFKD)")]
    PassphraseNotUtf8,
    /// The passphrase is longer than a caller-enforced limit.
    #[error("passphrase is {0} bytes; at most 256 allowed")]
    PassphraseTooLong(usize),
    /// The system random number generator failed while generating a phrase.
    #[error("random number generator failed")]
    Rng,
}

impl From<MnemonicError> for crate::Error {
    fn from(e: MnemonicError) -> Self {
        crate::Error::InvalidMnemonic(e.to_string())
    }
}

/// The mnemonic a wallet keeps, in the form that reproduces its seed.
///
/// Its bincode codec is hand-written with an explicit tag per variant (0 = `Bip39`,
/// 1 = `DashCore`, the order the derive used), so the wallet-file bytes never depend
/// on declaration order.
#[derive(Clone, Debug, Zeroize, ZeroizeOnDrop)]
pub enum WalletMnemonic {
    /// A strict BIP39 phrase in any supported language.
    Bip39(Mnemonic),
    /// A phrase Dash Core's `Check` accepts, hashed as Core hashes it.
    DashCore(DashCoreMnemonic),
}

#[cfg(feature = "bincode")]
const TAG_BIP39: u32 = 0;
#[cfg(feature = "bincode")]
const TAG_DASH_CORE: u32 = 1;

#[cfg(feature = "bincode")]
impl bincode::Encode for WalletMnemonic {
    fn encode<E: bincode::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), bincode::error::EncodeError> {
        match self {
            WalletMnemonic::Bip39(mnemonic) => {
                bincode::Encode::encode(&TAG_BIP39, encoder)?;
                bincode::Encode::encode(mnemonic, encoder)
            }
            WalletMnemonic::DashCore(mnemonic) => {
                bincode::Encode::encode(&TAG_DASH_CORE, encoder)?;
                bincode::Encode::encode(mnemonic, encoder)
            }
        }
    }
}

#[cfg(feature = "bincode")]
impl<C> bincode::Decode<C> for WalletMnemonic {
    fn decode<D: bincode::de::Decoder<Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, bincode::error::DecodeError> {
        match <u32 as bincode::Decode<C>>::decode(decoder)? {
            TAG_BIP39 => Ok(WalletMnemonic::Bip39(bincode::Decode::decode(decoder)?)),
            TAG_DASH_CORE => Ok(WalletMnemonic::DashCore(bincode::Decode::decode(decoder)?)),
            found => Err(bincode::error::DecodeError::UnexpectedVariant {
                type_name: "WalletMnemonic",
                allowed: &bincode::error::AllowedEnumVariants::Range {
                    min: TAG_BIP39,
                    max: TAG_DASH_CORE,
                },
                found,
            }),
        }
    }
}

/// Delegates to `Decode`: no field borrows from the input.
#[cfg(feature = "bincode")]
impl<'de, C> bincode::BorrowDecode<'de, C> for WalletMnemonic {
    fn borrow_decode<D: bincode::de::BorrowDecoder<'de, Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, bincode::error::DecodeError> {
        bincode::Decode::decode(decoder)
    }
}

impl WalletMnemonic {
    /// Parses `phrase` under `compat`: [`Mnemonic::from_phrase`] for BIP39 (every
    /// supported language, NFKD), [`DashCoreMnemonic::parse`] for Dash Core.
    pub fn parse(phrase: &str, compat: MnemonicCompat) -> Result<Self, MnemonicError> {
        match compat {
            MnemonicCompat::Bip39 => Mnemonic::from_phrase(phrase)
                .map(WalletMnemonic::Bip39)
                .map_err(|_| bip39_error(phrase)),
            MnemonicCompat::DashCore => {
                DashCoreMnemonic::parse(phrase.as_bytes()).map(WalletMnemonic::DashCore)
            }
        }
    }

    /// The rules this mnemonic follows.
    pub fn compat(&self) -> MnemonicCompat {
        match self {
            WalletMnemonic::Bip39(_) => MnemonicCompat::Bip39,
            WalletMnemonic::DashCore(_) => MnemonicCompat::DashCore,
        }
    }

    /// The seed for `passphrase`. BIP39 needs a UTF-8 passphrase (it is NFKD
    /// normalised); Dash Core uses the raw bytes, cut as Core cuts them.
    pub fn to_seed(&self, passphrase: &Passphrase) -> Result<Zeroizing<[u8; 64]>, MnemonicError> {
        match self {
            WalletMnemonic::Bip39(mnemonic) => {
                let passphrase = core::str::from_utf8(passphrase.expose())
                    .map_err(|_| MnemonicError::PassphraseNotUtf8)?;
                Ok(mnemonic.to_seed_zeroizing(passphrase))
            }
            WalletMnemonic::DashCore(mnemonic) => Ok(mnemonic.to_seed(passphrase.expose())),
        }
    }

    /// Where strict BIP39 (English) and Dash Core would disagree about `phrase` and
    /// `passphrase`: for UI warnings and for deriving both ways on import.
    pub fn portability(phrase: &[u8], passphrase: &[u8]) -> Portability {
        let nfkd_changes_passphrase = match core::str::from_utf8(passphrase) {
            Ok(text) => text.nfkd().ne(text.chars()),
            // BIP39 cannot hash bytes that are not UTF-8 at all.
            Err(_) => true,
        };
        Portability {
            core_accepts: dash_core::core_check(phrase),
            bip39_accepts: dash_core::strict_bip39(phrase),
            nfkd_changes_passphrase,
            salt_truncated: 8 + passphrase.len() > CORE_SALT_MAX_BYTES,
        }
    }
}

/// The strict-BIP39 reason a phrase that [`Mnemonic::from_phrase`] refused fails, in
/// the language its words belong to.
fn bip39_error(phrase: &str) -> MnemonicError {
    use bip39::{Error, Language, Mnemonic as Bip39Mnemonic};
    // The word count is the same in every language; check it first, as the parser does.
    if let Err(Error::BadWordCount(n)) = Bip39Mnemonic::parse_in(Language::English, phrase) {
        return if n == 0 {
            MnemonicError::Empty
        } else {
            MnemonicError::WordCount(n)
        };
    }
    // The language-detecting parse names the first word no word list holds, or the
    // checksum failure of the one language that holds every word.
    match Bip39Mnemonic::parse(phrase) {
        Err(Error::BadWordCount(0)) => MnemonicError::Empty,
        Err(Error::BadWordCount(n)) => MnemonicError::WordCount(n),
        Err(Error::UnknownWord(index)) => MnemonicError::UnknownWord {
            index,
        },
        // Every word is in several lists, and `from_phrase` tried each of them: the
        // checksum failed in all.
        Err(Error::InvalidChecksum | Error::AmbiguousLanguages(_)) => MnemonicError::Checksum,
        // Not produced by parsing; and the parse cannot succeed, since `from_phrase`
        // tried the language it would pick.
        Err(Error::BadEntropyBitCount(_)) | Ok(_) => MnemonicError::Checksum,
    }
}

/// How a phrase and passphrase fare under strict BIP39 and under Dash Core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Portability {
    /// Core's `Check` accepts the phrase.
    pub core_accepts: bool,
    /// Strict BIP39 accepts the phrase in English (full checksum, single spaces).
    pub bip39_accepts: bool,
    /// NFKD changes the passphrase (or it is not UTF-8), so BIP39 hashes other
    /// bytes than Core.
    pub nfkd_changes_passphrase: bool,
    /// `8 + |passphrase| > 256`: Core cuts the salt, BIP39 does not.
    pub salt_truncated: bool,
}

impl Portability {
    /// Whether the BIP39 seed and Core's seed are the same for this input (both
    /// accept the phrase and they hash the same salt).
    pub fn seeds_agree(&self) -> bool {
        self.core_accepts
            && self.bip39_accepts
            && !self.nfkd_changes_passphrase
            && !self.salt_truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::SecretBytes;

    const ABANDON_12: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn parse_by_compat() {
        let bip = WalletMnemonic::parse(ABANDON_12, MnemonicCompat::Bip39).unwrap();
        let core = WalletMnemonic::parse(ABANDON_12, MnemonicCompat::DashCore).unwrap();
        assert_eq!(
            (bip.compat(), core.compat()),
            (MnemonicCompat::Bip39, MnemonicCompat::DashCore)
        );
        let pass = SecretBytes::from_slice(b"TREZOR");
        assert_eq!(*bip.to_seed(&pass).unwrap(), *core.to_seed(&pass).unwrap());

        assert_eq!(
            WalletMnemonic::parse("", MnemonicCompat::Bip39).unwrap_err(),
            MnemonicError::Empty
        );
        assert_eq!(
            WalletMnemonic::parse("abandon about", MnemonicCompat::Bip39).unwrap_err(),
            MnemonicError::WordCount(2)
        );
        let bad = ABANDON_12.replace("about", "zzz");
        assert_eq!(
            WalletMnemonic::parse(&bad, MnemonicCompat::Bip39).unwrap_err(),
            MnemonicError::UnknownWord {
                index: 11
            }
        );
        let wrong = ABANDON_12.replace("about", "abandon");
        assert_eq!(
            WalletMnemonic::parse(&wrong, MnemonicCompat::Bip39).unwrap_err(),
            MnemonicError::Checksum
        );
    }

    /// A phrase in another language is reported in that language: a Japanese phrase with a
    /// bad checksum is a checksum error (not an unknown English word), and a Japanese phrase
    /// with one foreign word names that word's position.
    #[test]
    fn bip39_errors_use_the_phrase_language() {
        let words = crate::mnemonic::Language::Japanese.word_list();
        // Word 0 twelve times: the checksum of zero entropy needs another last word.
        let bad_checksum = [words[0]; 12].join("\u{3000}");
        assert_eq!(
            WalletMnemonic::parse(&bad_checksum, MnemonicCompat::Bip39).unwrap_err(),
            MnemonicError::Checksum
        );
        let valid = bip39::Mnemonic::from_entropy_in(bip39::Language::Japanese, &[0; 16]).unwrap();
        let mut unknown: Vec<&str> = valid.word_indices().map(|i| words[i]).collect();
        unknown[7] = "zzz";
        assert_eq!(
            WalletMnemonic::parse(&unknown.join(" "), MnemonicCompat::Bip39).unwrap_err(),
            MnemonicError::UnknownWord {
                index: 7
            }
        );
        // A count error wins over the language.
        assert_eq!(
            WalletMnemonic::parse(&[words[0]; 11].join(" "), MnemonicCompat::Bip39).unwrap_err(),
            MnemonicError::WordCount(11)
        );
    }

    #[test]
    fn bip39_needs_utf8_passphrase() {
        let bip = WalletMnemonic::parse(ABANDON_12, MnemonicCompat::Bip39).unwrap();
        let core = WalletMnemonic::parse(ABANDON_12, MnemonicCompat::DashCore).unwrap();
        let raw = SecretBytes::from_slice(&[0xff, 0x00]);
        assert_eq!(bip.to_seed(&raw).unwrap_err(), MnemonicError::PassphraseNotUtf8);
        assert!(core.to_seed(&raw).is_ok());
    }

    #[test]
    fn portability_flags() {
        let p = WalletMnemonic::portability(ABANDON_12.as_bytes(), b"TREZOR");
        assert!(p.core_accepts && p.bip39_accepts && p.seeds_agree());
        let nfc = WalletMnemonic::portability(ABANDON_12.as_bytes(), "caf\u{e9}".as_bytes());
        assert!(nfc.nfkd_changes_passphrase && !nfc.seeds_agree());
        let long = WalletMnemonic::portability(ABANDON_12.as_bytes(), &[b'a'; 249]);
        assert!(long.salt_truncated && !long.seeds_agree());
        assert!(!WalletMnemonic::portability(ABANDON_12.as_bytes(), &[b'a'; 248]).salt_truncated);
        let spaced = ABANDON_12.replace(' ', "  ");
        let p = WalletMnemonic::portability(spaced.as_bytes(), b"");
        assert!(!p.core_accepts && !p.bip39_accepts);
    }

    #[test]
    fn errors_name_no_word() {
        let bad = ABANDON_12.replace("about", "secretword");
        let err = WalletMnemonic::parse(&bad, MnemonicCompat::DashCore).unwrap_err();
        assert!(!err.to_string().contains("secretword"));
        assert!(!format!("{err:?}").contains("secretword"));
        let err = WalletMnemonic::parse(&bad, MnemonicCompat::Bip39).unwrap_err();
        assert!(!err.to_string().contains("secretword"));
    }
}
