//! The wallet file: the legacy bincode layout when it can hold the wallet, the
//! versioned envelope v1 otherwise.
//!
//! Envelope v1, all integers little-endian:
//!
//! ```text
//! 0xFF ‖ "KWW" ‖ version u8 = 1 ‖ flags u8
//!      ‖ len u32 ‖ core: the wallet's fields, legacy layout, envelope wallet-type tags
//!      ‖ len u32 ‖ imported public entries
//!      ‖ len u32 ‖ imported secrets
//!      ‖ count u32 ‖ count × (len u32 ‖ sealed record)
//!      ‖ end of input
//! ```
//!
//! The leading `0xFF` is not a valid start of the legacy layout (its first field is
//! the `Network` enum, whose varint tag is small), so a legacy reader fails on an
//! envelope instead of misreading it.
//!
//! This version writes no imported entries, no sealed records and no flags: those
//! sections are empty and the count is 0. A reader that meets any of them, an
//! unknown version or flag, or trailing bytes refuses the whole file; nothing is
//! skipped. The legacy branch keeps the old leniency about trailing bytes.

use bincode::config::{self, Configuration, LittleEndian, Varint};
use bincode::enc::write::SizeWriter;

use super::codec::{is_legacy, EnvelopeCore};
use super::Wallet;
use crate::crypto::CryptoError;
use crate::Error;

const MAGIC: [u8; 4] = [0xFF, b'K', b'W', b'W'];
const VERSION: u8 = 1;
/// Magic, version and flags.
const HEADER_LEN: usize = MAGIC.len() + 2;

/// The most bytes one decode may claim. Secret buffers are allocated at their
/// declared length before they are read, so a crafted length must fail here rather
/// than allocate. Far above any real wallet (a few KiB plus ~200 B per account).
const DECODE_LIMIT: usize = 64 * 1024 * 1024;

type LimitedConfig = Configuration<LittleEndian, Varint, config::Limit<DECODE_LIMIT>>;

fn limited() -> LimitedConfig {
    config::standard().with_limit::<DECODE_LIMIT>()
}

/// The number of bytes `value` encodes to (a size pass that writes nothing).
fn encoded_size(value: &impl bincode::Encode) -> Result<usize, Error> {
    let mut size = SizeWriter::default();
    bincode::encode_into_writer(value, &mut size, config::standard())
        .map_err(|e| Error::Serialization(format!("Failed to encode wallet: {e}")))?;
    Ok(size.bytes_written)
}

fn unsupported(what: &str) -> Error {
    Error::Serialization(format!("{} ({what})", CryptoError::UnsupportedFormat))
}

impl Wallet {
    /// Serialises the wallet: the legacy layout ([`Wallet::backup`]'s bytes) when
    /// the wallet uses nothing newer, envelope v1 otherwise (see `wallet/file.rs`).
    /// [`Wallet::from_bytes`] reads both.
    ///
    /// An unencrypted wallet's secrets (mnemonic, passphrase, seed, keys) are in
    /// these bytes in clear.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        if is_legacy(&self.wallet_type) {
            // `Wallet::backup`'s bytes, in a buffer sized up front.
            let size = encoded_size(self)?;
            let mut out = Vec::with_capacity(size);
            bincode::encode_into_std_write(self, &mut out, config::standard())
                .map_err(|e| Error::Serialization(format!("Failed to encode wallet: {e}")))?;
            debug_assert_eq!(out.len(), size);
            return Ok(out);
        }
        let core = EnvelopeCore(self);
        let core_size = encoded_size(&core)?;
        let core_len = u32::try_from(core_size)
            .map_err(|_| Error::Serialization("wallet too large".into()))?;

        // Sized up front, so the buffer holding the secrets never reallocates.
        let total = HEADER_LEN + 4 + core_size + 4 + 4 + 4;
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.push(0); // flags
        out.extend_from_slice(&core_len.to_le_bytes());
        bincode::encode_into_std_write(&core, &mut out, config::standard())
            .map_err(|e| Error::Serialization(format!("Failed to encode wallet: {e}")))?;
        out.extend_from_slice(&0u32.to_le_bytes()); // imported public entries
        out.extend_from_slice(&0u32.to_le_bytes()); // imported secrets
        out.extend_from_slice(&0u32.to_le_bytes()); // sealed records
        debug_assert_eq!(out.len(), total);
        Ok(out)
    }

    /// Reads [`Wallet::to_bytes`] output: envelope v1, or the legacy layout (as
    /// [`Wallet::restore`] does, trailing bytes ignored). Every decode is bounded,
    /// so a crafted length fails instead of allocating.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.first() != Some(&MAGIC[0]) {
            return bincode::decode_from_slice(bytes, limited())
                .map(|(wallet, _)| wallet)
                .map_err(|e| Error::Serialization(format!("Failed to restore wallet: {e}")));
        }
        let mut reader = Reader(bytes);
        if reader.take(MAGIC.len())? != MAGIC {
            return Err(unsupported("magic"));
        }
        if reader.byte()? != VERSION {
            return Err(unsupported("version"));
        }
        // Bit 0 (stored wallet id) is not written by this version.
        if reader.byte()? != 0 {
            return Err(unsupported("flags"));
        }
        let core = reader.section()?;
        let (EnvelopeCore(wallet), read) = bincode::decode_from_slice(core, limited())
            .map_err(|e| Error::Serialization(format!("Failed to restore wallet: {e}")))?;
        if read != core.len() {
            return Err(unsupported("trailing bytes in the wallet section"));
        }
        if !reader.section()?.is_empty() {
            return Err(unsupported("imported entries"));
        }
        if !reader.section()?.is_empty() {
            return Err(unsupported("imported secrets"));
        }
        if reader.u32()? != 0 {
            return Err(unsupported("sealed records"));
        }
        if !reader.0.is_empty() {
            return Err(unsupported("trailing bytes"));
        }
        Ok(wallet)
    }
}

/// A cursor that checks every length against the remaining input.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if n > self.0.len() {
            return Err(Error::Serialization("truncated wallet file".into()));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, Error> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A `len u32 ‖ bytes` section.
    fn section(&mut self) -> Result<&'a [u8], Error> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mnemonic::MnemonicCompat;
    use crate::secret::Passphrase;
    use crate::wallet::initialization::WalletAccountCreationOptions;
    use crate::wallet::WalletType;
    use crate::Network;

    const PHRASE: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn passphrase_wallet(compat: MnemonicCompat) -> Wallet {
        Wallet::from_phrase(
            PHRASE,
            Passphrase::from_slice(b"TREZOR"),
            compat,
            Network::Testnet,
            WalletAccountCreationOptions::Default,
        )
        .unwrap()
    }

    fn envelope_of(core: &[u8], tail: &[u8]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&[VERSION, 0]);
        out.extend_from_slice(&(core.len() as u32).to_le_bytes());
        out.extend_from_slice(core);
        out.extend_from_slice(tail);
        out
    }

    fn core_bytes(wallet: &Wallet) -> Vec<u8> {
        bincode::encode_to_vec(EnvelopeCore(wallet), config::standard()).unwrap()
    }

    const EMPTY_TAIL: [u8; 12] = [0; 12];

    #[test]
    fn legacy_wallets_keep_the_legacy_bytes() {
        let wallet = Wallet::from_phrase(
            PHRASE,
            Passphrase::new(Vec::new()),
            MnemonicCompat::Bip39,
            Network::Testnet,
            WalletAccountCreationOptions::Default,
        )
        .unwrap();
        assert!(matches!(wallet.wallet_type, WalletType::Mnemonic { .. }));
        let bytes = wallet.to_bytes().unwrap();
        assert_eq!(bytes, wallet.backup().unwrap());
        assert_eq!(Wallet::from_bytes(&bytes).unwrap().wallet_id, wallet.wallet_id);
        // The legacy branch ignores trailing bytes, as `import_wallet_from_bytes` did.
        let mut padded = bytes.clone();
        padded.extend_from_slice(&[1, 2, 3]);
        assert_eq!(Wallet::from_bytes(&padded).unwrap().wallet_id, wallet.wallet_id);
    }

    #[test]
    fn envelope_round_trips_and_legacy_refuses() {
        for compat in [MnemonicCompat::Bip39, MnemonicCompat::DashCore] {
            let wallet = passphrase_wallet(compat);
            let bytes = wallet.to_bytes().unwrap();
            assert_eq!(bytes, envelope_of(&core_bytes(&wallet), &EMPTY_TAIL));
            let decoded = Wallet::from_bytes(&bytes).unwrap();
            assert_eq!(decoded.to_bytes().unwrap(), bytes);
            assert_eq!(decoded.wallet_id, wallet.wallet_id);
            let WalletType::MnemonicWithPassphrase {
                mnemonic,
                passphrase,
                ..
            } = &decoded.wallet_type
            else {
                panic!("variant");
            };
            assert_eq!(mnemonic.compat(), compat);
            assert_eq!(passphrase.expose(), b"TREZOR");

            // The legacy entry points refuse it, and legacy readers fail on the envelope.
            assert!(matches!(
                bincode::encode_to_vec(&wallet, config::standard()),
                Err(bincode::error::EncodeError::Other(_))
            ));
            assert!(wallet.backup().is_err());
            assert!(Wallet::restore(&bytes).is_err());
        }
    }

    #[test]
    fn envelope_is_strict() {
        let wallet = passphrase_wallet(MnemonicCompat::DashCore);
        let core = core_bytes(&wallet);
        let good = envelope_of(&core, &EMPTY_TAIL);
        assert!(Wallet::from_bytes(&good).is_ok());

        let refused = |bytes: &[u8]| {
            let err = Wallet::from_bytes(bytes).unwrap_err();
            assert!(matches!(err, Error::Serialization(_)), "{err:?}");
            err.to_string()
        };
        // Every truncation fails.
        for cut in 1..good.len() {
            refused(&good[..cut]);
        }
        // Trailing bytes, flags and version.
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(refused(&trailing).contains("unsupported"));
        for (offset, value) in [(4, 2u8), (4, 0), (5, 1), (5, 0x80), (1, b'X')] {
            let mut bad = good.clone();
            bad[offset] = value;
            assert!(refused(&bad).contains("unsupported"), "offset {offset}");
        }
        // Non-empty imported sections and records are refused until a version writes them.
        for tail in [
            [&1u32.to_le_bytes()[..], &[0], &[0; 8]].concat(),
            [&[0; 4][..], &1u32.to_le_bytes(), &[0], &[0; 4]].concat(),
            [&[0; 8][..], &1u32.to_le_bytes()].concat(),
        ] {
            assert!(refused(&envelope_of(&core, &tail)).contains("unsupported"));
        }
        // A core section longer than its wallet.
        let mut long_core = core.clone();
        long_core.push(0);
        assert!(refused(&envelope_of(&long_core, &EMPTY_TAIL)).contains("unsupported"));
        // A section length past the end of the input.
        let mut huge = good.clone();
        huge[6..10].copy_from_slice(&u32::MAX.to_le_bytes());
        refused(&huge);
    }

    #[test]
    fn envelope_rejects_later_wallet_types() {
        let wallet = passphrase_wallet(MnemonicCompat::Bip39);
        let mut core = core_bytes(&wallet);
        // network (1 byte) + wallet id (32 bytes) precede the wallet-type tag.
        assert_eq!(core[33], 5);
        for tag in [6u8, 7, 8] {
            core[33] = tag;
            let err = Wallet::from_bytes(&envelope_of(&core, &EMPTY_TAIL)).unwrap_err();
            assert!(err.to_string().contains(&format!("found: {tag} ")), "{err}");
        }
    }

    #[test]
    fn bip39_passphrase_must_be_utf8_on_decode() {
        let wallet = passphrase_wallet(MnemonicCompat::Bip39);
        let core = core_bytes(&wallet);
        // Replace "TREZOR" (6 bytes after its length byte) with invalid UTF-8.
        let at = core.windows(7).position(|w| w == b"\x06TREZOR").unwrap();
        let mut bad = core.clone();
        bad[at + 1] = 0xFF;
        let err = Wallet::from_bytes(&envelope_of(&bad, &EMPTY_TAIL)).unwrap_err();
        assert!(err.to_string().contains("UTF-8"), "{err}");
        // Dash Core mode keeps raw bytes.
        let core = core_bytes(&passphrase_wallet(MnemonicCompat::DashCore));
        let at = core.windows(7).position(|w| w == b"\x06TREZOR").unwrap();
        let mut raw = core.clone();
        raw[at + 1] = 0xFF;
        assert!(Wallet::from_bytes(&envelope_of(&raw, &EMPTY_TAIL)).is_ok());
    }

    /// Replaces the passphrase of a variant-5 wallet in memory.
    fn with_passphrase(mut wallet: Wallet, bytes: &[u8]) -> Wallet {
        let WalletType::MnemonicWithPassphrase {
            passphrase,
            ..
        } = &mut wallet.wallet_type
        else {
            panic!("variant");
        };
        *passphrase = Passphrase::from_slice(bytes);
        wallet
    }

    /// The writer keeps the reader's invariants: no BIP39 variant 5 with a passphrase that
    /// is not UTF-8, and none with an empty passphrase (that wallet is variant 0). The
    /// errors name no secret.
    #[test]
    fn writer_refuses_what_the_reader_refuses() {
        for bad in [&[0xFF, b'x'][..], b""] {
            let wallet = with_passphrase(passphrase_wallet(MnemonicCompat::Bip39), bad);
            let err = wallet.to_bytes().unwrap_err();
            assert!(matches!(err, Error::Serialization(_)), "{err:?}");
            let text = format!("{err} {err:?}");
            assert!(text.contains("BIP39"), "{text}");
            assert!(!text.contains("abandon"), "{text}");
            assert!(bincode::encode_to_vec(EnvelopeCore(&wallet), config::standard()).is_err());
        }
        // Dash Core mode keeps any bytes, the empty passphrase included.
        for raw in [&[0xFF, b'x'][..], b""] {
            let wallet = with_passphrase(passphrase_wallet(MnemonicCompat::DashCore), raw);
            let bytes = wallet.to_bytes().unwrap();
            let back = Wallet::from_bytes(&bytes).unwrap();
            assert_eq!(back.to_bytes().unwrap(), bytes);
        }
    }

    /// A BIP39 variant 5 with an empty passphrase is not the canonical form (that wallet is
    /// variant 0), so the reader refuses it.
    #[test]
    fn reader_refuses_bip39_with_an_empty_passphrase() {
        let core = core_bytes(&passphrase_wallet(MnemonicCompat::Bip39));
        let at = core.windows(7).position(|w| w == b"\x06TREZOR").unwrap();
        let mut empty = core[..at].to_vec();
        empty.push(0);
        empty.extend_from_slice(&core[at + 7..]);
        let err = Wallet::from_bytes(&envelope_of(&empty, &EMPTY_TAIL)).unwrap_err();
        assert!(err.to_string().contains("empty passphrase"), "{err}");
        // The same bytes in Dash Core mode are a valid wallet (the reader does not
        // re-derive the root key, D108).
        let core = core_bytes(&passphrase_wallet(MnemonicCompat::DashCore));
        let at = core.windows(7).position(|w| w == b"\x06TREZOR").unwrap();
        let mut empty = core[..at].to_vec();
        empty.push(0);
        empty.extend_from_slice(&core[at + 7..]);
        assert!(Wallet::from_bytes(&envelope_of(&empty, &EMPTY_TAIL)).is_ok());
    }

    #[test]
    fn crafted_lengths_fail_without_allocating() {
        let wallet = passphrase_wallet(MnemonicCompat::DashCore);
        let core = core_bytes(&wallet);
        // Tag 5 at offset 33, the `WalletMnemonic` tag at 34, then the phrase length
        // varint: claim 2^62 bytes.
        assert_eq!(core[33..35], [5, 1]);
        let mut bad = core[..35].to_vec();
        bad.extend_from_slice(&[0xFD]);
        bad.extend_from_slice(&(1u64 << 62).to_le_bytes());
        bad.extend_from_slice(&core[36..]);
        assert!(Wallet::from_bytes(&envelope_of(&bad, &EMPTY_TAIL)).is_err());
        // The same through the legacy branch: a mnemonic wallet whose phrase length is huge.
        let legacy = Wallet::from_phrase(
            PHRASE,
            Passphrase::new(Vec::new()),
            MnemonicCompat::Bip39,
            Network::Testnet,
            WalletAccountCreationOptions::None,
        )
        .unwrap()
        .to_bytes()
        .unwrap();
        assert_eq!(legacy[33], 0);
        let mut bad = legacy[..34].to_vec();
        bad.extend_from_slice(&[0xFD]);
        bad.extend_from_slice(&(1u64 << 62).to_le_bytes());
        bad.extend_from_slice(&legacy[35..]);
        assert!(Wallet::from_bytes(&bad).is_err());
    }
}
