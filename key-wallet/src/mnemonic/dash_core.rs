//! Dash Core's `CMnemonic` (`src/wallet/bip39.cpp`), ported verbatim.
//!
//! Core differs from BIP39 in three ways, and each is reproduced here:
//! - `Check` compares the checksum under a mask computed with `^` (XOR) where a
//!   power was meant, so it checks only some of the checksum bits and accepts every
//!   valid BIP39 phrase plus phrases whose unchecked bits are wrong;
//! - nothing is Unicode-normalised: the phrase and the passphrase are raw bytes;
//! - `ToSeed` cuts the salt (`"mnemonic"` followed by the passphrase) at 256 bytes.
//!
//! The word list is English only: the `bip39` crate's list, which is byte-equal to
//! Core's `bip39_english.h` (checked by a test).

use core::fmt;

use dashcore_hashes::{sha256, sha512, Hash, HashEngine, Hmac, HmacEngine};
use subtle::{Choice, ConditionallySelectable, ConstantTimeEq};
use unicode_normalization::UnicodeNormalization;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::MnemonicError;
use crate::secret::SecretBytes;

/// Core's salt cap (`bip39.cpp` `ToSeed`): the salt is `"mnemonic" ‖ passphrase`
/// cut at this many bytes, so only the first 248 passphrase bytes count.
pub const CORE_SALT_MAX_BYTES: usize = 256;

/// Core's legacy-wallet passphrase limit (`hdchain.cpp` "Mnemonic passphrase is too
/// long, must be at most 256 characters"). Core's descriptor wallets have no limit
/// and truncate silently, so the library does not enforce it; a caller that
/// reproduces a legacy wallet does.
pub const CORE_LEGACY_MAX_PASSPHRASE_BYTES: usize = 256;

/// PBKDF2 iterations of `ToSeed`.
const PBKDF2_ROUNDS: u32 = 2048;

/// Longest word Core's tokeniser accepts before the lookup (`ssCurrentWord.size() >= 9`
/// stops a 10th character).
const MAX_WORD_BYTES: usize = 9;

/// The 2048-word English list Core uses (`bip39_english.h`).
fn word_list() -> &'static [&'static str; 2048] {
    super::bip39_word_list(bip39::Language::English)
}

/// A phrase accepted by Core's `CMnemonic::Check`, holding the exact bytes Core
/// hashes. Its `Debug` shows only the word count.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct DashCoreMnemonic {
    phrase: SecretBytes,
}

impl DashCoreMnemonic {
    /// Port of `CMnemonic::Check` (`bip39.cpp:78-138`): the same tokenisation (count
    /// of `' '` plus one, no trimming), the 10-character guard, a case-sensitive
    /// exact lookup and Core's checksum mask. Accepts exactly what Core accepts.
    pub fn parse(phrase: &[u8]) -> Result<Self, MnemonicError> {
        check(phrase)?;
        Ok(Self {
            phrase: SecretBytes::from_slice(phrase),
        })
    }

    /// Like [`parse`](Self::parse), keeping the buffer it is given.
    #[cfg(feature = "bincode")]
    fn from_secret(phrase: SecretBytes) -> Result<Self, MnemonicError> {
        check(phrase.expose())?;
        Ok(Self {
            phrase,
        })
    }

    /// Port of `CMnemonic::Generate` (`bip39.cpp:33-42`): `bits` must be one of 128,
    /// 160, 192, 224 or 256 (Core's `-mnemonicbits` values).
    #[cfg(feature = "getrandom")]
    pub fn generate(bits: u32) -> Result<Self, MnemonicError> {
        if !bits.is_multiple_of(32) || !(128..=256).contains(&bits) {
            return Err(MnemonicError::Bits(bits));
        }
        let mut entropy = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut entropy[..]).map_err(|_| MnemonicError::Rng)?;
        Self::from_entropy(&entropy[..bits as usize / 8])
    }

    /// Port of `CMnemonic::FromData` (`bip39.cpp:45-76`): standard BIP39 encoding of
    /// 16 to 32 bytes (a multiple of 4) of entropy, single spaces.
    pub fn from_entropy(entropy: &[u8]) -> Result<Self, MnemonicError> {
        let len = entropy.len();
        if !len.is_multiple_of(4) || !(16..=32).contains(&len) {
            return Err(MnemonicError::EntropyLength(len));
        }
        let mut bits = Zeroizing::new([0u8; 33]);
        bits[..len].copy_from_slice(entropy);
        bits[len] = sha256::Hash::hash(entropy).to_byte_array()[0];

        let words = len * 3 / 4;
        let list = word_list();
        let capacity = words * (MAX_WORD_BYTES + 1);
        let mut phrase = Zeroizing::new(Vec::with_capacity(capacity));
        for i in 0..words {
            let mut index = 0usize;
            for j in 0..11 {
                let bit = i * 11 + j;
                index = (index << 1) | usize::from(bits[bit / 8] & (1 << (7 - bit % 8)) != 0);
            }
            if i > 0 {
                phrase.push(b' ');
            }
            phrase.extend_from_slice(list[index].as_bytes());
        }
        debug_assert!(phrase.len() <= capacity);
        Ok(Self {
            phrase: SecretBytes::new(core::mem::take(&mut *phrase)),
        })
    }

    /// Port of `CMnemonic::ToSeed` (`bip39.cpp:140-149`): PBKDF2-HMAC-SHA512, 2048
    /// rounds, over the raw phrase bytes, with the salt `"mnemonic" ‖ passphrase` cut
    /// at [`CORE_SALT_MAX_BYTES`]. No normalisation.
    pub fn to_seed(&self, passphrase: &[u8]) -> Zeroizing<[u8; 64]> {
        core_seed(self.phrase.expose(), passphrase)
    }

    /// The phrase bytes.
    pub fn phrase(&self) -> &SecretBytes {
        &self.phrase
    }

    /// Number of words (`' '` count plus one).
    pub fn word_count(&self) -> usize {
        word_count(self.phrase.expose())
    }

    /// Whether the phrase also passes strict BIP39 (English, full checksum).
    pub fn is_strict_bip39(&self) -> bool {
        strict_bip39(self.phrase.expose())
    }
}

/// `Debug` never shows a word or byte of the phrase.
impl fmt::Debug for DashCoreMnemonic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DashCoreMnemonic({} words)", self.word_count())
    }
}

/// Same layout as [`SecretBytes`] (and `String`): a length prefix, then the bytes.
#[cfg(feature = "bincode")]
impl bincode::Encode for DashCoreMnemonic {
    fn encode<E: bincode::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), bincode::error::EncodeError> {
        self.phrase.encode(encoder)
    }
}

/// Decodes into a wiped buffer and re-runs Core's `Check`; a phrase Core rejects is
/// an error that names no word.
#[cfg(feature = "bincode")]
impl<C> bincode::Decode<C> for DashCoreMnemonic {
    fn decode<D: bincode::de::Decoder<Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, bincode::error::DecodeError> {
        let phrase: SecretBytes = bincode::Decode::decode(decoder)?;
        Self::from_secret(phrase).map_err(|_| {
            bincode::error::DecodeError::OtherString("invalid Dash Core mnemonic".into())
        })
    }
}

/// Delegates to `Decode`: the phrase never aliases the input buffer.
#[cfg(feature = "bincode")]
impl<'de, C> bincode::BorrowDecode<'de, C> for DashCoreMnemonic {
    fn borrow_decode<D: bincode::de::BorrowDecoder<'de, Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, bincode::error::DecodeError> {
        bincode::Decode::decode(decoder)
    }
}

/// Whether Core's `CMnemonic::Check` accepts `phrase`.
pub fn core_check(phrase: &[u8]) -> bool {
    check(phrase).is_ok()
}

/// Core's `CMnemonic::ToSeed`, which runs no `Check`: PBKDF2-HMAC-SHA512 over the
/// raw `phrase`, salt `"mnemonic" ‖ passphrase` cut at [`CORE_SALT_MAX_BYTES`].
pub fn core_seed(phrase: &[u8], passphrase: &[u8]) -> Zeroizing<[u8; 64]> {
    let salt_len = (8 + passphrase.len()).min(CORE_SALT_MAX_BYTES);
    let mut salt = Zeroizing::new(Vec::with_capacity(salt_len));
    salt.extend_from_slice(b"mnemonic");
    salt.extend_from_slice(&passphrase[..salt_len - 8]);
    pbkdf2_hmac_sha512_one_block(phrase, &salt, PBKDF2_ROUNDS)
}

/// The BIP39 seed: PBKDF2-HMAC-SHA512, 2048 rounds, over `phrase` (the words joined by
/// single spaces, as BIP39 hashes them), salt `"mnemonic" ‖ NFKD(passphrase)`, never cut.
/// The salt is built in a zeroizing buffer of its exact size, so it never reallocates.
pub(super) fn bip39_seed(phrase: &[u8], passphrase: &str) -> Zeroizing<[u8; 64]> {
    let nfkd_len: usize = passphrase.nfkd().map(char::len_utf8).sum();
    let mut salt = Zeroizing::new(Vec::with_capacity(8 + nfkd_len));
    salt.extend_from_slice(b"mnemonic");
    let mut utf8 = Zeroizing::new([0u8; 4]);
    for c in passphrase.nfkd() {
        salt.extend_from_slice(c.encode_utf8(&mut utf8[..]).as_bytes());
    }
    debug_assert_eq!(salt.len(), 8 + nfkd_len);
    pbkdf2_hmac_sha512_one_block(phrase, &salt, PBKDF2_ROUNDS)
}

/// Core counts words as the number of spaces plus one.
pub(super) fn word_count(phrase: &[u8]) -> usize {
    phrase.iter().filter(|&&b| b == b' ').count() + 1
}

/// Strict BIP39 in English: every word in the list, single spaces, full checksum.
pub(super) fn strict_bip39(phrase: &[u8]) -> bool {
    core::str::from_utf8(phrase)
        .is_ok_and(|p| bip39::Mnemonic::parse_in_normalized(bip39::Language::English, p).is_ok())
        && check(phrase).is_ok()
}

/// `CMnemonic::Check`, returning why a phrase fails.
fn check(phrase: &[u8]) -> Result<(), MnemonicError> {
    if phrase.is_empty() {
        return Err(MnemonicError::Empty);
    }
    let words = word_count(phrase);
    if !words.is_multiple_of(3) || !(12..=24).contains(&words) {
        return Err(MnemonicError::WordCount(words));
    }

    // Core stops a word at its 10th character, and looks up even the empty word
    // between two spaces (no word is empty). A trailing space ends Core's loop without
    // a lookup, then fails its bit count; both are an unknown word here.
    let mut tokens = Zeroizing::new([0u128; MAX_WORDS]);
    for (slot, token) in tokens.iter_mut().zip(phrase.split(|&b| b == b' ')) {
        *slot = pack(token);
    }
    let (found, word_indices) = lookup(&tokens[..words]);

    // Core collects 11 bits per word into a 33-byte buffer (264 bits at 24 words).
    let mut bits = Zeroizing::new([0u8; 33]);
    for (index, (found, &word_index)) in
        found.iter().zip(word_indices.iter()).take(words).enumerate()
    {
        if !bool::from(*found) {
            return Err(MnemonicError::UnknownWord {
                index,
            });
        }
        for k in 0..11 {
            let bit = index * 11 + k;
            let set = ((word_index >> (10 - k)) & 1) as u8;
            bits[bit / 8] |= set << (7 - bit % 8);
        }
    }

    // bits[32] = bits[nWordCount * 4 / 3]; SHA256 of the entropy bytes into bits[0..32].
    let entropy_len = words * 4 / 3;
    let checksum = bits[entropy_len];
    let hash = Zeroizing::new(sha256::Hash::hash(&bits[..entropy_len]).to_byte_array());
    // `(2 ^ checksum_length) << (8 - checksum_length)`: `^` is XOR in C++.
    let checksum_length = (words / 3) as u32;
    let mask = ((2 ^ checksum_length) << (8 - checksum_length)) as u8;
    if hash[0] & mask == checksum & mask {
        Ok(())
    } else {
        Err(MnemonicError::Checksum)
    }
}

/// Most words a phrase `check` accepts has.
const MAX_WORDS: usize = 24;

/// A token as a number: its length in the low byte, then its bytes, then zeros. Two
/// tokens of at most [`MAX_WORD_BYTES`] bytes pack equal exactly when they are equal; a
/// longer one gets length byte `0xFF`, which no word has.
fn pack(token: &[u8]) -> u128 {
    let mut bytes = Zeroizing::new([0u8; 16]);
    let over = u8::from(token.len() > MAX_WORD_BYTES);
    bytes[0] = token.len().min(MAX_WORD_BYTES) as u8 | over.wrapping_neg();
    for (slot, b) in bytes[1..=MAX_WORD_BYTES].iter_mut().zip(token) {
        *slot = *b;
    }
    u128::from_le_bytes(*bytes)
}

/// Looks every packed token up in the word list: whether each is there, and its index
/// (0 when it is not). Each token is compared with all 2048 words in constant time and
/// the index is selected without branching, so the time taken depends only on the
/// number of tokens, not on which words they are.
fn lookup(tokens: &[u128]) -> ([Choice; MAX_WORDS], Zeroizing<[u16; MAX_WORDS]>) {
    let mut found = [Choice::from(0); MAX_WORDS];
    let mut indices = Zeroizing::new([0u16; MAX_WORDS]);
    for (i, word) in (0u16..).zip(word_list().iter()) {
        let word = pack(word.as_bytes());
        for ((token, found), index) in tokens.iter().zip(&mut found).zip(indices.iter_mut()) {
            let hit = word.ct_eq(token);
            index.conditional_assign(&i, hit);
            *found |= hit;
        }
    }
    (found, indices)
}

/// PBKDF2-HMAC-SHA512 producing exactly one 64-byte block (`dkLen` = `hLen`).
///
/// The keyed HMAC states are built from pads this function owns and wipes, and are
/// overwritten when it returns. Copies the hash engines make internally are not
/// reachable from here (as with register spills).
fn pbkdf2_hmac_sha512_one_block(password: &[u8], salt: &[u8], rounds: u32) -> Zeroizing<[u8; 64]> {
    const BLOCK: usize = 128;
    let mut key = Zeroizing::new([0u8; BLOCK]);
    if password.len() > BLOCK {
        key[..64].copy_from_slice(&sha512::Hash::hash(password).to_byte_array());
    } else {
        key[..password.len()].copy_from_slice(password);
    }
    let mut pad = Zeroizing::new([0u8; BLOCK]);
    let mut keyed = |byte: u8| {
        for (p, k) in pad.iter_mut().zip(key.iter()) {
            *p = k ^ byte;
        }
        let mut engine = sha512::HashEngine::default();
        engine.input(&pad[..]);
        engine
    };
    let mut hmac = HmacEngine::<sha512::Hash>::from_inner_engines(keyed(0x36), keyed(0x5c));

    let mac = |hmac: &HmacEngine<sha512::Hash>, data: &[&[u8]]| -> Zeroizing<[u8; 64]> {
        let mut engine = hmac.clone();
        for d in data {
            engine.input(d);
        }
        Zeroizing::new(Hmac::<sha512::Hash>::from_engine(engine).to_byte_array())
    };
    let mut u = mac(&hmac, &[salt, &1u32.to_be_bytes()]);
    let mut out = Zeroizing::new(*u);
    for _ in 1..rounds {
        u = mac(&hmac, &[&u[..]]);
        for (o, x) in out.iter_mut().zip(u.iter()) {
            *o ^= x;
        }
    }
    hmac = HmacEngine::from_inner_engines(Default::default(), Default::default());
    core::hint::black_box(&mut hmac);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABANDON_12: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    /// sha256 of Core's `bip39_english.h` word list, one word per line with a
    /// trailing newline each, transcribed at the program pin `0364ed626ec2`
    /// (`git show 0364ed626ec2:src/wallet/bip39_english.h`). It equals the sha256 of
    /// the BIP39 reference `english.txt`.
    const CORE_WORDLIST_SHA256: &str =
        "2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda";

    #[test]
    fn word_list_is_byte_equal_to_core() {
        let mut joined = Vec::new();
        for word in word_list() {
            joined.extend_from_slice(word.as_bytes());
            joined.push(b'\n');
        }
        assert_eq!(sha256::Hash::hash(&joined).to_string(), CORE_WORDLIST_SHA256);
        assert!(word_list().iter().all(|w| w.len() <= MAX_WORD_BYTES));
    }

    #[test]
    fn masks_match_the_design_table() {
        let mask = |words: u32| ((2 ^ (words / 3)) << (8 - words / 3)) as u8;
        assert_eq!([12, 15, 18, 21, 24].map(mask), [0x60, 0x38, 0x10, 0x0A, 0x0A]);
    }

    #[test]
    fn rejections() {
        assert_eq!(DashCoreMnemonic::parse(b"").unwrap_err(), MnemonicError::Empty);
        assert_eq!(DashCoreMnemonic::parse(b"abandon").unwrap_err(), MnemonicError::WordCount(1));
        let upper = ABANDON_12.replace("about", "About");
        assert_eq!(
            DashCoreMnemonic::parse(upper.as_bytes()).unwrap_err(),
            MnemonicError::UnknownWord {
                index: 11
            }
        );
        let leading = format!(" {}", &ABANDON_12[8..]);
        assert_eq!(
            DashCoreMnemonic::parse(leading.as_bytes()).unwrap_err(),
            MnemonicError::UnknownWord {
                index: 0
            }
        );
        let trailing = format!("{} ", &ABANDON_12[..ABANDON_12.len() - 6]);
        assert!(!core_check(trailing.as_bytes()));
        let wrong = ABANDON_12.replace("about", "abandon");
        assert_eq!(DashCoreMnemonic::parse(wrong.as_bytes()).unwrap_err(), MnemonicError::Checksum);
    }

    /// Core compares whole tokens (`std::string` against the list entry), so a list word with
    /// a byte added before or after it, a 9- or 10-byte token that starts with a word, and a
    /// lone NUL are all unknown words.
    #[test]
    fn near_words_are_unknown() {
        let words: Vec<&str> = ABANDON_12.split(' ').collect();
        for token in
            [&b"abandon\0"[..], b"\0abandon", b"abandon\x80", b"abandonaa", b"abandonaaa", b"\0"]
        {
            let mut phrase = Vec::new();
            for (i, word) in words.iter().enumerate() {
                if i > 0 {
                    phrase.push(b' ');
                }
                phrase.extend_from_slice(if i == 3 {
                    token
                } else {
                    word.as_bytes()
                });
            }
            assert_eq!(
                DashCoreMnemonic::parse(&phrase).unwrap_err(),
                MnemonicError::UnknownWord {
                    index: 3
                },
                "token {token:?}"
            );
            assert!(!core_check(&phrase), "token {token:?}");
        }
    }

    #[test]
    fn debug_shows_only_the_word_count() {
        let m = DashCoreMnemonic::parse(ABANDON_12.as_bytes()).unwrap();
        assert_eq!(format!("{m:?}"), "DashCoreMnemonic(12 words)");
        assert_eq!(format!("{m:#?}"), "DashCoreMnemonic(12 words)");
    }

    #[test]
    fn strict_phrase_seed_matches_bip39() {
        let m = DashCoreMnemonic::parse(ABANDON_12.as_bytes()).unwrap();
        assert!(m.is_strict_bip39());
        let bip = bip39::Mnemonic::parse(ABANDON_12).unwrap();
        assert_eq!(*m.to_seed(b"TREZOR"), bip.to_seed("TREZOR"));
        assert_eq!(*m.to_seed(b""), bip.to_seed(""));
    }

    #[test]
    fn long_password_is_hashed_into_the_key() {
        // A 24-word phrase of 8-letter words is longer than one SHA-512 block.
        let long = [b'x'; 200];
        let bip = {
            // Reference: RFC 2898 over `HmacEngine::new`, which hashes a long key itself.
            let mut key = HmacEngine::<sha512::Hash>::new(&long);
            key.input(b"mnemonicpass");
            key.input(&1u32.to_be_bytes());
            let mut u = Hmac::<sha512::Hash>::from_engine(key).to_byte_array();
            let mut out = u;
            for _ in 1..PBKDF2_ROUNDS {
                let mut e = HmacEngine::<sha512::Hash>::new(&long);
                e.input(&u);
                u = Hmac::<sha512::Hash>::from_engine(e).to_byte_array();
                out.iter_mut().zip(u.iter()).for_each(|(o, x)| *o ^= x);
            }
            out
        };
        assert_eq!(*core_seed(&long, b"pass"), bip);
    }

    /// `bip39_seed` is the bip39 crate's seed, NFKD and long phrases included.
    #[test]
    fn bip39_seed_matches_the_bip39_crate() {
        let long = bip39::Mnemonic::from_entropy(&[0xA5; 32]).unwrap();
        for phrase in [ABANDON_12.to_string(), long.to_string()] {
            let bip = bip39::Mnemonic::parse(&phrase).unwrap();
            for pass in
                ["", "TREZOR", "TREZOR-\u{c5}\u{e9}", "\u{ff21}\u{fb01}\u{1f600}", &"p".repeat(300)]
            {
                assert_eq!(*bip39_seed(phrase.as_bytes(), pass), bip.to_seed(pass), "{pass:?}");
            }
        }
        assert!(long.to_string().len() > 128);
    }

    #[test]
    fn salt_is_cut_at_256_bytes() {
        let p248 = [b'a'; 248];
        let mut p300 = [b'a'; 300];
        p300[260] = b'b';
        assert_eq!(core_seed(b"x", &p248), core_seed(b"x", &p300));
        let mut p249 = [b'a'; 249];
        p249[248] = b'c';
        assert_eq!(core_seed(b"x", &p248), core_seed(b"x", &p249));
        assert_ne!(core_seed(b"x", &p248), core_seed(b"x", &p248[..247]));
    }

    #[test]
    fn from_entropy_lengths() {
        for len in [16, 20, 24, 28, 32] {
            let m = DashCoreMnemonic::from_entropy(&vec![0x5a; len]).unwrap();
            assert_eq!(m.word_count(), len * 3 / 4);
            assert!(m.is_strict_bip39());
        }
        for len in [0, 15, 17, 36] {
            assert_eq!(
                DashCoreMnemonic::from_entropy(&vec![0; len]).unwrap_err(),
                MnemonicError::EntropyLength(len)
            );
        }
    }

    #[cfg(feature = "getrandom")]
    #[test]
    fn generate_word_counts() {
        for (bits, words) in [(128, 12), (160, 15), (192, 18), (224, 21), (256, 24)] {
            assert_eq!(DashCoreMnemonic::generate(bits).unwrap().word_count(), words);
        }
        for bits in [0, 96, 127, 129, 288, u32::MAX] {
            assert_eq!(DashCoreMnemonic::generate(bits).unwrap_err(), MnemonicError::Bits(bits));
        }
    }
}
