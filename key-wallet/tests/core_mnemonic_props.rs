//! Property tests of the Dash Core mnemonic port (design §6.3): Core's `Check` against a
//! line-by-line transcription of the C++ loop, its relation to strict BIP39, the seeds, the
//! entropy round trip and the wallet file round trip.

use key_wallet::mnemonic::{
    core_check, core_seed, DashCoreMnemonic, MnemonicCompat, MnemonicError, WalletMnemonic,
};
use key_wallet::wallet::initialization::WalletAccountCreationOptions;
use key_wallet::{Language, Mnemonic, Network, Passphrase, Wallet};
use proptest::prelude::*;

fn english() -> &'static [&'static str; 2048] {
    Language::English.word_list().try_into().expect("2048 words")
}

/// `CMnemonic::Check` (`src/wallet/bip39.cpp:78-138` at `0364ed626ec2`), transcribed statement by
/// statement: an independent model for the port in `mnemonic/dash_core.rs`. Kept in the C++
/// form, so clippy's idioms are off here.
#[allow(clippy::manual_range_contains, clippy::manual_is_multiple_of)]
fn cpp_check(mnemonic: &[u8]) -> bool {
    let wordlist = english();
    if mnemonic.is_empty() {
        return false;
    }
    let mut n_word_count: u32 = 0;
    for &c in mnemonic {
        if c == b' ' {
            n_word_count += 1;
        }
    }
    n_word_count += 1;
    if n_word_count % 3 != 0 || n_word_count < 12 || n_word_count > 24 {
        return false;
    }
    let mut ss_current_word: Vec<u8> = Vec::new();
    let mut bits = [0u8; 32 + 1];
    let mut n_bits_count: u32 = 0;
    let mut i = 0usize;
    while i < mnemonic.len() {
        ss_current_word.clear();
        while i + ss_current_word.len() < mnemonic.len()
            && mnemonic[i + ss_current_word.len()] != b' '
        {
            if ss_current_word.len() >= 9 {
                return false;
            }
            ss_current_word.push(mnemonic[i + ss_current_word.len()]);
        }
        i += ss_current_word.len();
        let mut n_word_index: u32 = 0;
        loop {
            // `wordlist` is NULL-terminated in C++.
            let Some(word) = wordlist.get(n_word_index as usize) else {
                return false;
            };
            if ss_current_word == word.as_bytes() {
                for ki in 0..11u32 {
                    if n_word_index & (1 << (10 - ki)) != 0 {
                        bits[(n_bits_count / 8) as usize] |= 1 << (7 - (n_bits_count % 8));
                    }
                    n_bits_count += 1;
                }
                break;
            }
            n_word_index += 1;
        }
        i += 1; // the `++i` of the outer `for`
    }
    if n_bits_count != n_word_count * 11 {
        return false;
    }
    bits[32] = bits[(n_word_count * 4 / 3) as usize];
    let hash = {
        use dashcore_hashes::{sha256, Hash};
        sha256::Hash::hash(&bits[..(n_word_count * 4 / 3) as usize]).to_byte_array()
    };
    bits[..32].copy_from_slice(&hash);
    // `const char checksum_length`, `const char mask`: `^` is XOR; the operands of `&` and `==`
    // are promoted to int (every mask is positive, so sign extension never applies).
    let checksum_length = (n_word_count / 3) as i8;
    let mask = ((2 ^ i32::from(checksum_length)) << (8 - i32::from(checksum_length))) as i8;
    (i32::from(bits[0]) & i32::from(mask)) == (i32::from(bits[32]) & i32::from(mask))
}

/// Words joined by single spaces.
fn phrase_of(indices: &[usize]) -> String {
    indices.iter().map(|&i| english()[i]).collect::<Vec<_>>().join(" ")
}

/// Random English word sequences of 12 to 24 words (valid and invalid counts).
fn word_sequence() -> impl Strategy<Value = String> {
    prop::collection::vec(0usize..2048, 12..=24).prop_map(|w| phrase_of(&w))
}

/// Words, near-words and separators: exercises the tokeniser's edges (empty words, tabs,
/// trailing spaces, 9/10-letter words, uppercase, non-ASCII). Half the cases are a phrase of
/// 12 to 24 English words with exactly one such edit, so they reach the word lookup and often
/// the checksum; the other half mix tokens freely.
fn wordy_bytes() -> impl Strategy<Value = Vec<u8>> {
    let token = || {
        prop_oneof![
            6 => (0usize..2048).prop_map(|i| english()[i].as_bytes().to_vec()),
            1 => Just(Vec::new()),
            1 => prop::collection::vec(b'a'..=b'z', 0..12),
            1 => (0usize..2048).prop_map(|i| english()[i].to_uppercase().into_bytes()),
            1 => prop::collection::vec(any::<u8>(), 1..4),
        ]
    };
    let separator =
        || prop_oneof![8 => Just(&b" "[..]), 1 => Just(&b"  "[..]), 1 => Just(&b"\t"[..])];
    let mixed = prop::collection::vec((token(), separator()), 1..=26).prop_map(|parts| {
        let mut out = Vec::new();
        let n = parts.len();
        for (i, (token, separator)) in parts.into_iter().enumerate() {
            out.extend_from_slice(&token);
            if i + 1 < n {
                out.extend_from_slice(separator);
            }
        }
        out
    });
    // One edit of a single-spaced phrase: replace a word by any token, replace a space by any
    // separator, or add a leading or trailing space.
    let edited = (prop::collection::vec(0usize..2048, 12..=24), any::<prop::sample::Index>())
        .prop_flat_map(move |(words, at)| (Just(words), Just(at), 0u8..4, token(), separator()))
        .prop_map(|(words, at, kind, token, separator)| {
            let mut tokens: Vec<Vec<u8>> =
                words.iter().map(|&i| english()[i].as_bytes().to_vec()).collect();
            let mut separators = vec![&b" "[..]; tokens.len() - 1];
            match kind {
                0 => {
                    let i = at.index(tokens.len());
                    tokens[i] = token;
                }
                1 => {
                    let i = at.index(separators.len());
                    separators[i] = separator;
                }
                2 => tokens[0].insert(0, b' '),
                _ => tokens.last_mut().unwrap().push(b' '),
            }
            let mut out = tokens[0].clone();
            for (separator, token) in separators.iter().zip(&tokens[1..]) {
                out.extend_from_slice(separator);
                out.extend_from_slice(token);
            }
            out
        });
    prop_oneof![mixed, edited]
}

/// A phrase of `words` English words that Core's check accepts and strict BIP39 refuses: the
/// first such last word, searching from `start`.
fn core_only_phrase(words: &[usize], start: usize) -> Option<String> {
    (0..2048).map(|k| (start + k) % 2048).find_map(|last| {
        let mut indices = words.to_vec();
        indices.push(last);
        let phrase = phrase_of(&indices);
        let p = WalletMnemonic::portability(phrase.as_bytes(), b"");
        (p.core_accepts && !p.bip39_accepts).then_some(phrase)
    })
}

/// Entropy of 16 to 32 bytes in steps of 4.
fn entropy() -> impl Strategy<Value = Vec<u8>> {
    (4usize..=8).prop_flat_map(|n| prop::collection::vec(any::<u8>(), n * 4))
}

/// Passphrases that probe the seed differences: ASCII around the 248/249-byte salt limit,
/// characters NFKD changes or keeps, NUL, and bytes that are not UTF-8.
fn passphrase() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        3 => "[ -~]{0,40}".prop_map(String::into_bytes),
        1 => (240usize..=260).prop_map(|n| vec![b'p'; n]),
        2 => "[a-z\u{e9}\u{301}\u{ff21}\u{fb01}\u{2126}\u{c5}\u{0}]{0,12}".prop_map(String::into_bytes),
        1 => (246usize..=250).prop_map(|n| {
            // A two-byte character straddling the 256-byte salt cut.
            let mut p = vec![b'p'; n];
            p.extend_from_slice("\u{e9}".as_bytes());
            p
        }),
        1 => prop::collection::vec(any::<u8>(), 0..20),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Arbitrary bytes (0 to 300) almost never reach the word lookup, so this mostly checks
    /// that both refuse the same inputs early, and that nothing panics.
    #[test]
    fn check_refuses_like_cpp_on_random_bytes(bytes in prop::collection::vec(any::<u8>(), 0..=300)) {
        prop_assert_eq!(core_check(&bytes), cpp_check(&bytes));
        prop_assert_eq!(DashCoreMnemonic::parse(&bytes).is_ok(), cpp_check(&bytes));
        let _ = WalletMnemonic::portability(&bytes, &bytes);
    }

    /// Equality with the transcription on near-phrases (see `wordy_bytes`).
    #[test]
    fn check_equals_cpp_on_wordy_bytes(bytes in wordy_bytes()) {
        prop_assert_eq!(core_check(&bytes), cpp_check(&bytes));
    }

    /// Equality on 12 to 24 English words, where most counts reach the checksum; and strict
    /// BIP39 implies Core (Core's check is a superset).
    #[test]
    fn check_equals_cpp_on_words(phrase in word_sequence()) {
        let core = core_check(phrase.as_bytes());
        prop_assert_eq!(core, cpp_check(phrase.as_bytes()));
        let p = WalletMnemonic::portability(phrase.as_bytes(), b"");
        prop_assert!(!p.bip39_accepts || core);
        let bip39 = bip39::Mnemonic::parse_in_normalized(bip39::Language::English, &phrase).is_ok();
        prop_assert!(!bip39 || core);
        prop_assert_eq!(p.bip39_accepts, bip39);
    }

    /// Every strict phrase passes Core's check.
    #[test]
    fn strict_phrases_pass_core(entropy in entropy()) {
        let strict = Mnemonic::from_entropy(&entropy, Language::English).unwrap();
        prop_assert!(core_check(strict.phrase_zeroizing().as_bytes()));
    }

    /// `FromData` → `Check` round trip, and the same phrase as strict BIP39's encoding.
    #[test]
    fn entropy_round_trips(entropy in entropy()) {
        let core = DashCoreMnemonic::from_entropy(&entropy).unwrap();
        let reparsed = DashCoreMnemonic::parse(core.phrase().expose()).unwrap();
        prop_assert_eq!(&reparsed, &core);
        prop_assert!(core.is_strict_bip39());
        prop_assert_eq!(core.word_count(), entropy.len() * 3 / 4);
        let strict = Mnemonic::from_entropy(&entropy, Language::English).unwrap();
        let strict_phrase = strict.phrase_zeroizing();
        prop_assert_eq!(core.phrase().expose(), strict_phrase.as_bytes());
    }

    #[test]
    fn other_entropy_lengths_are_refused(len in 0usize..64) {
        prop_assume!(!(16..=32).contains(&len) || len % 4 != 0);
        prop_assert!(DashCoreMnemonic::from_entropy(&vec![0u8; len]).is_err());
    }
}

proptest! {
    // Each case runs PBKDF2 (2048 rounds) two or three times.
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Core's seed is the BIP39 seed for a strict phrase and an ASCII passphrase of at most
    /// 248 bytes.
    #[test]
    fn core_seed_is_bip39_seed_below_the_cut(entropy in entropy(), pass in "[ -~]{0,248}") {
        let strict = Mnemonic::from_entropy(&entropy, Language::English).unwrap();
        let phrase = strict.phrase_zeroizing();
        prop_assert_eq!(*core_seed(phrase.as_bytes(), pass.as_bytes()), *strict.to_seed_zeroizing(&pass));
    }

    /// `seeds_agree` predicts exactly whether Core's and BIP39's seeds are equal (where BIP39
    /// has a seed: a strict phrase and a UTF-8 passphrase); it is false whenever either side
    /// cannot take the input.
    #[test]
    fn seeds_agree_predicts_equality(entropy in entropy(), pass in passphrase()) {
        let strict = Mnemonic::from_entropy(&entropy, Language::English).unwrap();
        let phrase = strict.phrase_zeroizing();
        let p = WalletMnemonic::portability(phrase.as_bytes(), &pass);
        prop_assert!(p.core_accepts && p.bip39_accepts);
        let core = core_seed(phrase.as_bytes(), &pass);
        match core::str::from_utf8(&pass) {
            Ok(text) => prop_assert_eq!(p.seeds_agree(), *core == *strict.to_seed_zeroizing(text)),
            Err(_) => prop_assert!(!p.seeds_agree()),
        }
        let wallet_mnemonic = WalletMnemonic::parse(&phrase, MnemonicCompat::DashCore).unwrap();
        prop_assert_eq!(*wallet_mnemonic.to_seed(&Passphrase::from_slice(&pass)).unwrap(), *core);
    }

    /// A phrase only Core accepts is routed by mode: BIP39 mode refuses it with a checksum
    /// error, DashCore mode parses it and derives its seed with Core's `ToSeed`.
    #[test]
    fn core_only_phrases_have_a_core_seed_only(
        words in (4usize..=8).prop_flat_map(|n| prop::collection::vec(0usize..2048, n * 3 - 1)),
        start in 0usize..2048,
        pass in passphrase(),
    ) {
        let phrase = core_only_phrase(&words, start);
        prop_assert!(phrase.is_some(), "no core-only last word");
        let phrase = phrase.unwrap();
        prop_assert_eq!(
            WalletMnemonic::parse(&phrase, MnemonicCompat::Bip39).unwrap_err(),
            MnemonicError::Checksum
        );
        let core = WalletMnemonic::parse(&phrase, MnemonicCompat::DashCore).unwrap();
        let seed = core.to_seed(&Passphrase::from_slice(&pass)).unwrap();
        prop_assert_eq!(*seed, *core_seed(phrase.as_bytes(), &pass));
        prop_assert!(!WalletMnemonic::portability(phrase.as_bytes(), &pass).seeds_agree());
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Layout: encode → decode is the identity for passphrase and DashCore wallets (envelope
    /// v1) and for the plain mnemonic wallet (legacy bytes), and keeps the ID and the seed.
    #[test]
    fn wallet_file_round_trips(
        entropy in entropy(),
        pass in passphrase(),
        dash_core in any::<bool>(),
        testnet in any::<bool>(),
    ) {
        let compat = if dash_core { MnemonicCompat::DashCore } else { MnemonicCompat::Bip39 };
        let network = if testnet { Network::Testnet } else { Network::Mainnet };
        let strict = Mnemonic::from_entropy(&entropy, Language::English).unwrap();
        let phrase = strict.phrase_zeroizing();
        let made = Wallet::from_phrase(
            &phrase,
            Passphrase::from_slice(&pass),
            compat,
            network,
            WalletAccountCreationOptions::None,
        );
        let wallet = match made {
            Ok(wallet) => wallet,
            Err(_) => {
                // Only BIP39 with a passphrase that is not UTF-8 is refused.
                prop_assert!(!dash_core && core::str::from_utf8(&pass).is_err());
                return Ok(());
            }
        };
        let bytes = wallet.to_bytes().unwrap();
        prop_assert_eq!(bytes.first() == Some(&0xFF), dash_core || !pass.is_empty());
        let back = Wallet::from_bytes(&bytes).unwrap();
        prop_assert_eq!(back.wallet_id, wallet.wallet_id);
        prop_assert_eq!(back.network, wallet.network);
        prop_assert_eq!(
            back.seed_zeroizing().unwrap().map(|s| *s),
            wallet.seed_zeroizing().unwrap().map(|s| *s)
        );
        prop_assert_eq!(back.mnemonic_passphrase().unwrap(), wallet.mnemonic_passphrase().unwrap());
        prop_assert_eq!(back.to_bytes().unwrap(), bytes);
    }
}
