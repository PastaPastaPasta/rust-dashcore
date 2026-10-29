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
