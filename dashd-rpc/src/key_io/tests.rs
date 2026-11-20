use super::*;
use crate::descriptor::tests::{mainnet, regtest};

#[test]
fn addresses_decode_on_their_own_chain_only() {
    let (main, reg) = (mainnet(), regtest());
    // Core: test/functional/rpc_generate.py:52 (regtest P2PKH of the generator point).
    let generator = hash160(
        &PublicKey::from_secret_key(
            &SecretKey::from_secret_bytes([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 1,
            ])
            .unwrap(),
        )
        .serialize(),
    );
    let address = "yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg6";
    assert_eq!(decode_destination(address, &reg), Some(Destination::PkHash(generator)));
    assert_eq!(decode_destination(address, &main), None);
    // Core: test/functional/test_framework/address.py:19 (regtest P2SH of OP_TRUE).
    let p2sh = decode_destination("8zJctvfrzGZ5s1zQ3kagwyW1DsPYSQ4V2P", &reg);
    assert_eq!(p2sh, Some(Destination::ScriptHash(hash160(&[0x51]))));
}

/// `DecodeBase58` skips whitespace around the string, not inside it; a NUL fails.
// Core: base58.cpp:40-42, 72-77, 163-165
#[test]
fn whitespace_around_an_address_is_skipped() {
    let reg = regtest();
    let a = "yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg6";
    let want = decode_destination(a, &reg);
    assert!(want.is_some());
    assert_eq!(decode_destination(&format!(" \t\n{a}\x0b\x0c\r "), &reg), want);
    assert_eq!(decode_destination(&format!("{} {}", &a[..10], &a[10..]), &reg), None);
    assert_eq!(decode_destination(&format!("{a}\0"), &reg), None);
}

/// The longest string within the largest limit (78 bytes of 0xff and the checksum: 112
/// characters) still decodes; a longer one is refused before the decoder runs, where Core's decode
/// stops at the limit (the decoder is quadratic: an 8 MiB argument would hold a worker for hours).
// Core: base58.cpp:44-46, 54-56 (the limit checks)
#[test]
fn base58_strings_past_the_largest_limit_are_refused_at_once() {
    let longest = encode_check(&[0xff; 78]);
    assert_eq!(longest.len(), MAX_BASE58_CHARS);
    assert_eq!(decode_base58_check(&format!(" {longest} ")), Some(vec![0xff; 78]));
    assert_eq!(decode_base58_check(&format!("{longest}2")), None);
    assert_eq!(decode_destination(&"2".repeat(8 << 20), &regtest()), None);
}

/// A string starting with the Platform HRP, in any case, is never an L1 destination.
// Core: key_io.cpp:49-51, 78-103
#[test]
fn platform_hrp_prefix_is_never_a_destination() {
    let mut reg = regtest();
    // A real regtest address, its prefix taken as the HRP: refused on the prefix alone.
    let a = "yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg6";
    reg.platform_hrp = Some("ywz".into());
    assert_eq!(decode_destination(a, &reg), None);
    reg.platform_hrp = None;
    assert!(decode_destination(a, &reg).is_some());
}

#[path = "bip32_vectors.rs"]
mod bip32_vectors;

/// Core's `RunTest`: each step's xprv and xpub decode, derive the next step's (privately, and
/// publicly when not hardened), and the child's header and keys equal the next step's.
// Core: src/test/bip32_tests.cpp:124-158
#[test]
fn core_bip32_chains_derive() {
    let p = mainnet();
    for (name, line, steps) in bip32_vectors::CHAINS {
        for pair in steps.windows(2) {
            let ((pubs, prvs, child), (next_pub, next_prv, _)) = (pair[0], pair[1]);
            let key = decode_ext_key(prvs, &p).unwrap_or_else(|| panic!("{name} {line}"));
            let pubkey = decode_ext_pubkey(pubs, &p).unwrap();
            assert_eq!(key.neuter(), pubkey, "{name}");
            let derived = key.derive(child).unwrap();
            assert_eq!(derived, decode_ext_key(next_prv, &p).unwrap(), "{name} {child:#x}");
            assert_eq!(derived.neuter(), decode_ext_pubkey(next_pub, &p).unwrap(), "{name}");
            if child >> 31 == 0 {
                assert_eq!(pubkey.derive(child).unwrap(), derived.neuter(), "{name}");
            }
        }
    }
}

// Core: src/test/bip32_tests.cpp:178-185
#[test]
fn core_test5_strings_are_no_ext_keys() {
    let p = mainnet();
    for s in bip32_vectors::TEST5 {
        assert!(decode_ext_key(s, &p).is_none(), "{s}");
        assert!(decode_ext_pubkey(s, &p).is_none(), "{s}");
    }
}

/// Depth 255 is the last a key derives to.
// Core: src/test/bip32_tests.cpp:187-203
#[test]
fn derivation_stops_at_depth_255() {
    let p = mainnet();
    let (pubs, prvs, _) = bip32_vectors::CHAINS[0].2[0];
    let (mut key, mut pubkey) =
        (decode_ext_key(prvs, &p).unwrap(), decode_ext_pubkey(pubs, &p).unwrap());
    for _ in 0..255 {
        key = key.derive(0).unwrap();
        pubkey = pubkey.derive(0).unwrap();
    }
    assert!(key.derive(0).is_none());
    assert!(pubkey.derive(0).is_none());
}

#[test]
fn ext_keys_hold_cores_decode_rules() {
    let p = mainnet();
    let xprv = "xprv9s21ZrQH143K3QTDL4LXw2F7HEK3wJUD2nW2nRk4stbPy6cq3jPPqjiChkVvvNKmPGJxWUtg6LnF5kejMRNNU3TGtRBeJgk33yuGBxrMPHi";
    // Re-encoded with a non-zero byte before the secret: Core's `CExtKey::Decode` drops it.
    let mut data = base58ck::decode_check(xprv).unwrap();
    data[4 + 41] = 1;
    assert!(decode_ext_key(&encode_check(&data), &p).is_none());
    // A master key (depth 0) with a child number: invalid.
    let mut data = base58ck::decode_check(xprv).unwrap();
    data[4 + 8] = 1;
    assert!(decode_ext_key(&encode_check(&data), &p).is_none());
    // Mainnet prefixes on regtest: no key.
    assert!(decode_ext_key(xprv, &crate::descriptor::tests::regtest()).is_none());
}

#[test]
fn wif_secrets_decode_with_their_compression() {
    let p = mainnet();
    // Core: src/test/descriptor_tests.cpp:363, 369 (the same key, compressed and not).
    let c = decode_secret("XJvEUEcFWCHCyruc8ZX5exPZaGe4UR7gC5FHrhwPnQGDs1uWCsT2", &p).unwrap();
    assert!(c.compressed);
    assert_eq!(
        hex(&c.pubkey()),
        "03a34b99f22c790c4e36b2b3c2c35a36db06226e41c692fc82b8b56ac1c540c5bd"
    );
    let u = decode_secret("7sH936MDoVPFVk2VoaCM5yW8P3BfPyffnZECyaHrZwfLgWpS13e", &p).unwrap();
    assert!(!u.compressed);
    assert_eq!(u.pubkey().len(), 65);
    // Bitcoin's WIF of the same key: the wrong prefix.
    assert!(decode_secret("L4rK1yDtCWekvXuE6oXD9jCYfFNV2cWRpVuPLBcCU2z8TrisoyY1", &p).is_none());
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `EncodeBase58Check`, for re-encoding a tampered payload.
// Core: base58.cpp:87-123, 137-143
fn encode_check(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut bytes = data.to_vec();
    bytes.extend_from_slice(&dash_consensus::hash::sha256d(data)[..4]);
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();
    let mut digits: Vec<u8> = Vec::new();
    for &b in &bytes[zeros..] {
        let mut carry = u32::from(b);
        for d in digits.iter_mut() {
            carry += u32::from(*d) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut out = "1".repeat(zeros);
    out.extend(digits.iter().rev().map(|&d| char::from(ALPHABET[d as usize])));
    out
}

/// The chain a vector names: testnet has regtest's prefixes and HRP.
fn chain(name: &str) -> KeyIoParams {
    match name {
        "main" => mainnet(),
        "test" | "regtest" => regtest(),
        other => panic!("chain {other}"),
    }
}

fn vectors(json: &str) -> Vec<crate::UniValue> {
    crate::UniValue::read(json.as_bytes())
        .expect("JSON")
        .get_values()
        .expect("an array")
        .into_iter()
        .cloned()
        .collect()
}

fn field<'a>(v: &'a crate::UniValue, key: &str) -> Option<&'a crate::UniValue> {
    let crate::UniValue::Obj(fields) = v else {
        panic!("an object")
    };
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Core's valid vectors parse to their script or secret, a case flip fails unless the vector
/// says it survives, and the destination encodes back to the string.
// Core: src/test/key_io_tests.cpp:24-118
#[test]
fn core_key_io_valid_vectors() {
    for test in vectors(include_str!("data/key_io_valid.json")) {
        let t = test.get_values().unwrap();
        let (s, payload, meta) = (t[0].get_str().unwrap(), t[1].get_str().unwrap(), t[2]);
        let p = chain(field(meta, "chain").unwrap().get_str().unwrap());
        if field(meta, "isPrivkey").unwrap().get_bool().unwrap() {
            let key = decode_secret(s, &p).unwrap_or_else(|| panic!("{s}"));
            let compressed = field(meta, "isCompressed").unwrap().get_bool().unwrap();
            assert_eq!(key.compressed, compressed, "{s}");
            assert_eq!(hex(&key.secret.bytes()), payload, "{s}");
            assert_eq!(decode_destination(s, &p), None, "{s}");
            continue;
        }
        let dest = decode_destination_with_error(s, &p).unwrap_or_else(|e| panic!("{s}: {e:?}"));
        assert_eq!(hex(&dest.script()), payload, "{s}");
        let flipped: String = s
            .chars()
            .map(|c| {
                if c.is_ascii_lowercase() {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect();
        let survives = field(meta, "tryCaseFlip").is_some_and(|v| v.get_bool().unwrap());
        assert_eq!(decode_destination(&flipped, &p).is_some(), survives, "{s}");
        assert!(decode_secret(s, &p).is_none(), "{s}");
        let script: Vec<u8> = (0..payload.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&payload[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(extract_destination(&script), Some(dest), "{s}");
        assert_eq!(encode_destination(&dest, &p), s);
    }
}

// Core: src/test/key_io_tests.cpp:122-145
#[test]
fn core_key_io_invalid_vectors() {
    for test in vectors(include_str!("data/key_io_invalid.json")) {
        let s = test.get_values().unwrap()[0].get_str().unwrap().to_owned();
        for p in [mainnet(), regtest()] {
            assert!(decode_destination(&s, &p).is_none(), "{s}");
            assert!(decode_secret(&s, &p).is_none(), "{s}");
        }
    }
}

/// DIP-18's test vectors round-trip on their chain.
// Core: src/test/key_io_tests.cpp:150-189
#[test]
fn dip18_platform_addresses_round_trip() {
    let samples = [
        (
            "f7da0a2b5cbd4ff6bb2c4d89b67d2f3ffeec0525",
            "dash1krma5z3ttj75la4m93xcndna9ullamq9y5e9n5rs",
            true,
            false,
        ),
        (
            "a5ff0046217fd1c7d238e3e146cc5bfd90832a7e",
            "dash1kzjl7qzxy9lar37j8r37z3kvt07epqe20ckxfezw",
            true,
            false,
        ),
        (
            "6d92674fd64472a3dfcfc3ebcfed7382bf699d7b",
            "dash1kpkeye606ez89g7lelp7hnldwwpt76va0v3j6x28",
            true,
            false,
        ),
        (
            "f7da0a2b5cbd4ff6bb2c4d89b67d2f3ffeec0525",
            "tdash1krma5z3ttj75la4m93xcndna9ullamq9y5fzq2j7",
            false,
            false,
        ),
        (
            "a5ff0046217fd1c7d238e3e146cc5bfd90832a7e",
            "tdash1kzjl7qzxy9lar37j8r37z3kvt07epqe20cxp68nq",
            false,
            false,
        ),
        (
            "6d92674fd64472a3dfcfc3ebcfed7382bf699d7b",
            "tdash1kpkeye606ez89g7lelp7hnldwwpt76va0vp4fcmf",
            false,
            false,
        ),
        (
            "43fa183cf3fb6e9e7dc62b692aeb4fc8d8045636",
            "dash1sppl5xpu70aka8nacc4kj2htflydspzkxch4cad6",
            true,
            true,
        ),
        (
            "43fa183cf3fb6e9e7dc62b692aeb4fc8d8045636",
            "tdash1sppl5xpu70aka8nacc4kj2htflydspzkxc8jtru5",
            false,
            true,
        ),
    ];
    for (hash, address, main, p2sh) in samples {
        let p = if main {
            mainnet()
        } else {
            regtest()
        };
        let dest = decode_platform_destination(address, &p).unwrap_or_else(|e| panic!("{e}"));
        let h = match dest {
            PlatformDestination::P2sh(h) if p2sh => h,
            PlatformDestination::P2pkh(h) if !p2sh => h,
            other => panic!("{other:?}"),
        };
        assert_eq!(hex(&h), hash);
        assert_eq!(encode_platform_destination(&dest, &p), address);
        // Never an L1 destination; the error names it.
        assert_eq!(
            decode_destination_with_error(address, &p),
            Err(("This is a Dash Platform address, not a Dash Core address".into(), vec![]))
        );
    }
}

/// The refusals of Core's `dip18_platform_invalid`, each with its error text.
// Core: src/test/key_io_tests.cpp:192-246
#[test]
fn dip18_platform_refusals_name_their_cause() {
    use crate::bech32::{Encoding, convert_bits, encode};
    let main = mainnet();
    let enc = |encoding, payload: &str| {
        let bytes: Vec<u8> = (0..payload.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&payload[i..i + 2], 16).unwrap())
            .collect();
        encode(encoding, "dash", &convert_bits(&bytes, 8, 5, true).unwrap())
    };
    let cases = [
        (
            "tdash1krma5z3ttj75la4m93xcndna9ullamq9y5fzq2j7".to_owned(),
            "Invalid Platform HRP for the selected network",
        ),
        ("Dash1krma5z3ttj75la4m93xcndna9ullamq9y5e9n5rs".to_owned(), "Invalid bech32m encoding"),
        (
            enc(Encoding::Bech32, "b0f7da0a2b5cbd4ff6bb2c4d89b67d2f3ffeec0525"),
            "DIP-18 Platform addresses require bech32m checksum",
        ),
        (
            enc(Encoding::Bech32m, "00f7da0a2b5cbd4ff6bb2c4d89b67d2f3ffeec0525"),
            "Unknown DIP-18 type byte",
        ),
        (
            enc(Encoding::Bech32m, "b0f7da0a2b5cbd4ff6bb2c4d89b67d2f3ffeec05"),
            "Invalid Platform address payload length",
        ),
        ("".to_owned(), "Invalid bech32m encoding"),
        ("not-an-address".to_owned(), "Invalid bech32m encoding"),
    ];
    for (s, error) in cases {
        assert_eq!(decode_platform_destination(&s, &main), Err(error), "{s}");
    }
    assert_eq!(
        decode_platform_destination("dash1krma5z3ttj75la4m93xcndna9ullamq9y5e9n5rs", &regtest()),
        Err("Invalid Platform HRP for the selected network")
    );
}

/// `DecodeDestination`'s error texts and locations, one per branch.
// Core: key_io.cpp:42-102
#[test]
fn decode_destination_names_each_refusal() {
    let reg = regtest();
    let err = |s: &str| decode_destination_with_error(s, &reg).unwrap_err();
    let msg = |s: &str| err(s).0;
    // A mainnet P2PKH: a valid Base58Check payload under another prefix.
    assert_eq!(
        msg("XqZHYpoksmbEPtWstAEki3o8pJ8ZsPfXpK"),
        "Invalid prefix for Base58-encoded address"
    );
    // regtest's P2PKH prefix with a 19-byte hash.
    let short = encode_base58_check(&[[140].as_slice(), &[7; 19]].concat());
    assert_eq!(msg(&short), "Invalid length for Base58 address");
    // A good address with its last character changed: a bad checksum.
    assert_eq!(
        msg("yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg7"),
        "Invalid checksum or length of Base58 address"
    );
    // A 22-byte payload: no address, though the checksum holds.
    let long = encode_base58_check(&[140; 22]);
    assert_eq!(msg(&long), "Invalid checksum or length of Base58 address");
    // Not Base58 at all ('0' and 'l' are outside the alphabet).
    assert_eq!(msg("0OIl"), "Not a valid Bech32m or Base58 encoding");
    assert_eq!(msg(""), "Invalid checksum or length of Base58 address");
    // Over 100 bytes without the checksum.
    assert_eq!(msg(&"z".repeat(140)), "Not a valid Bech32m or Base58 encoding");
    // The Platform HRP, then a bad character: LocateErrors' answer and offset.
    assert_eq!(
        err("tdash1krma5z3ttj75la4m93xcndna9ullamq9y5fzq2jb"),
        ("Invalid Base 32 character".into(), vec![45])
    );
    assert_eq!(
        err("tdash1krma5z3ttj75la4m93xcndna9ullamq9y5fzq2j8"),
        ("Invalid Bech32m checksum".into(), vec![45])
    );
}

/// `DecodeBase58`: whitespace around, never inside; the limit counts leading zeros.
// Core: base58.cpp:38-80
#[test]
fn decode_base58_matches_cores_limits() {
    assert_eq!(decode_base58(" 1112 ", 4), Some(vec![0, 0, 0, 1]));
    assert_eq!(decode_base58("1112", 3), None);
    assert_eq!(decode_base58("11111", 4), None);
    assert_eq!(decode_base58("2 2", 10), None);
    assert_eq!(decode_base58("2\0", 10), None);
    assert_eq!(decode_base58("5Q", 1), Some(vec![0xff]));
    assert_eq!(decode_base58("LUv", 1), None);
    assert_eq!(decode_base58("", 0), Some(vec![]));
}

/// `base58_DecodeBase58`'s `base58_encode_decode.json` rows decode to their bytes, under the
/// 256-byte limit Core's test passes.
// Core: test/base58_tests.cpp:44-61
#[test]
fn decode_base58_core_vectors() {
    let text = dash_core_vectors::read_core_text("src/test/data/base58_encode_decode.json");
    let cases = dash_core_vectors::cases::load_base58(&text).unwrap();
    assert!(!cases.is_empty());
    for c in cases {
        assert_eq!(decode_base58(&c.base58, 256), Some(c.data), "row {}", c.index);
    }
}
