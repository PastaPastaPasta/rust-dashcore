use super::*;

#[path = "core_vectors.rs"]
mod core_vectors;

use core_vectors::{CHECKS, DEFAULT, HARDENED, RANGE, UNPARSABLE};

/// `CMainParams`' Base58 prefixes, which Core's descriptor tests run on.
// Core: chainparams.cpp:263-273
pub(crate) fn mainnet() -> KeyIoParams {
    KeyIoParams {
        pubkey_address: 76,
        script_address: 16,
        secret_key: 204,
        ext_public_key: [0x04, 0x88, 0xB2, 0x1E],
        ext_secret_key: [0x04, 0x88, 0xAD, 0xE4],
        platform_hrp: Some("dash".into()),
    }
}

/// `CRegTestParams`' (testnet's and the devnets' too).
// Core: chainparams.cpp:933-943
pub(crate) fn regtest() -> KeyIoParams {
    KeyIoParams {
        pubkey_address: 140,
        script_address: 19,
        secret_key: 239,
        ext_public_key: [0x04, 0x35, 0x87, 0xCF],
        ext_secret_key: [0x04, 0x35, 0x83, 0x94],
        platform_hrp: Some("tdash".into()),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parsed(s: &str, p: &KeyIoParams) -> (Descriptor, Keys) {
    let mut keys = Keys::new();
    let desc = parse(s, p, &mut keys).unwrap_or_else(|e| panic!("{s}: {e}"));
    (desc, keys)
}

/// Core's `DoCheck`, the parts of it this port has: both forms parse, are ranged as the flags
/// say, and expand at each position to the vector's scripts, with the private keys for a
/// hardened derivation.
// Core: src/test/descriptor_tests.cpp:116-268
#[test]
fn core_check_vectors_parse_and_expand_to_cores_scripts() {
    let p = mainnet();
    for c in CHECKS {
        let (prv, keys_prv) = parsed(c.prv, &p);
        let (pubd, keys_pub) = parsed(c.pub_, &p);
        let range = c.flags & RANGE != 0;
        assert_eq!(prv.is_range(), range, "line {}", c.line);
        assert_eq!(pubd.is_range(), range, "line {}", c.line);
        let provider = if c.flags & HARDENED != 0 {
            &keys_prv
        } else {
            &keys_pub
        };
        let max = if range {
            c.scripts.len()
        } else {
            3
        };
        for pos in 0..max {
            let want: Vec<String> = c.scripts[if range {
                pos
            } else {
                0
            }]
            .iter()
            .map(|s| s.to_string())
            .collect();
            for desc in [&prv, &pubd] {
                let got =
                    desc.expand(pos as u32, provider).unwrap_or_else(|| panic!("line {}", c.line));
                let got: Vec<String> = got.iter().map(|s| hex(s)).collect();
                assert_eq!(got, want, "line {} pos {pos}", c.line);
            }
        }
        if c.flags & HARDENED != 0 {
            assert!(pubd.expand(0, &keys_pub).is_none(), "line {}: no private key", c.line);
        }
    }
    assert!(CHECKS.iter().any(|c| c.flags == DEFAULT));
}

/// The normalized forms Core prints carry its checksum: the port computes the same one.
#[test]
fn checksums_match_cores_normalized_descriptors() {
    let mut seen = 0;
    for c in CHECKS {
        for s in [c.prv, c.pub_, c.norm_pub] {
            if let Some((body, sum)) = s.split_once('#') {
                assert_eq!(descriptor_checksum(body).as_deref(), Some(sum), "line {}", c.line);
                seen += 1;
            }
        }
    }
    assert!(seen >= 5, "{seen}");
}

/// Core's `CheckUnparsable`: both forms fail, the public one with Core's error.
// Core: src/test/descriptor_tests.cpp:22-31
#[test]
fn core_unparsable_vectors_fail_with_cores_error() {
    let p = mainnet();
    for (line, prv, pubd, error) in UNPARSABLE {
        assert!(parse(prv, &p, &mut Keys::new()).is_err(), "line {line}");
        assert_eq!(parse(pubd, &p, &mut Keys::new()).err().as_deref(), Some(*error), "line {line}");
    }
}

#[test]
fn checksum_errors_are_cores() {
    let p = mainnet();
    let err = |s: &str| parse(s, &p, &mut Keys::new()).err().unwrap();
    assert_eq!(err("raw(00)#a#b"), "Multiple '#' symbols");
    assert_eq!(err("raw(00)#abc"), "Expected 8 character checksum, not 3 characters");
    assert_eq!(err("raw(00)#"), "Expected 8 character checksum, not 0 characters");
    let sum = descriptor_checksum("raw(00)").unwrap();
    assert!(parse(&format!("raw(00)#{sum}"), &p, &mut Keys::new()).is_ok());
    assert_eq!(
        err("raw(00)#qqqqqqqq"),
        format!("Provided checksum 'qqqqqqqq' does not match computed checksum '{sum}'")
    );
}

/// A descriptor Core parses but does not consume entirely fails with an empty error, and so
/// does an `sh()` with something after its script.
#[test]
fn leftover_input_fails_with_cores_empty_error() {
    let p = mainnet();
    assert_eq!(parse("raw(00))", &p, &mut Keys::new()).err().as_deref(), Some(""));
    assert_eq!(parse("raw(00),", &p, &mut Keys::new()).err().as_deref(), Some(""));
    let g = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
    assert!(parse(&format!("sh(pk({g}))"), &p, &mut Keys::new()).is_ok());
    assert_eq!(parse(&format!("sh(pk({g}),1)"), &p, &mut Keys::new()).err().as_deref(), Some(""));
    assert_eq!(
        parse("foo(00)", &p, &mut Keys::new()).err().as_deref(),
        Some("'foo(00)' is not a valid descriptor function")
    );
    assert_eq!(
        parse("sh(raw(00))", &p, &mut Keys::new()).err().as_deref(),
        Some("Can only have raw() at top level")
    );
}

/// `rpc_generate.py`'s descriptors, on regtest's parameters.
// Core: test/functional/rpc_generate.py:44-105, test_framework/address.py:18
#[test]
fn rpc_generate_descriptors_give_cores_answers() {
    let p = regtest();
    // combo() of a compressed key: its P2PKH script, the address the test reads back.
    let combo = script_from_descriptor(
        "combo(0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798)",
        &p,
    )
    .unwrap();
    let address = decode_destination("yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg6", &p).unwrap();
    assert_eq!(combo, address.script());

    let unspendable = decode_destination("yVg3NBUHNEhgDceqwVUjsZHreC5PBHnUo9", &p).unwrap();
    assert_eq!(
        script_from_descriptor("addr(yVg3NBUHNEhgDceqwVUjsZHreC5PBHnUo9)#e5kt0jtk", &p).unwrap(),
        unspendable.script()
    );

    let tpub = "tpubD6NzVbkrYhZ4XgiXtGrdW5XDAPFCL9h7we1vwNCpn8tGbBcgfVYjXyhWo4E1xkh56hjod1RhGjxbaTLV3X4FyWuejifB9jusQ46QzG87VKp";
    assert_eq!(
        script_from_descriptor(&format!("pkh({tpub}/0/*)"), &p),
        Err(DescriptorError::Ranged)
    );
    assert_eq!(
        script_from_descriptor(&format!("pkh({tpub}/0'/0)"), &p),
        Err(DescriptorError::CannotDerive)
    );
    assert!(script_from_descriptor(&format!("pkh({tpub}/0/0)"), &p).is_ok());
    assert_eq!(
        script_from_descriptor("1234", &p),
        Err(DescriptorError::Parse("'1234' is not a valid descriptor function".into()))
    );
    // `MiniWallet`'s descriptor: raw() with Core's checksum.
    let raw = format!("raw(51)#{}", descriptor_checksum("raw(51)").unwrap());
    assert_eq!(script_from_descriptor(&raw, &p).unwrap(), vec![0x51]);
}

/// Of a `combo()`'s scripts, an uncompressed key has its P2PK only, which is taken; a
/// compressed one has three, and the second (P2PKH) is taken.
// Core: script/descriptor.cpp:836-848, rpc/mining.cpp:205-216
#[test]
fn combo_picks_cores_script() {
    let p = mainnet();
    let c = CHECKS.iter().find(|c| c.line == 371).unwrap();
    assert_eq!(hex(&script_from_descriptor(c.pub_, &p).unwrap()), c.scripts[0][0]);
    let c = CHECKS.iter().find(|c| c.line == 365).unwrap();
    assert_eq!(hex(&script_from_descriptor(c.pub_, &p).unwrap()), c.scripts[0][1]);
}

#[test]
fn small_ints_and_multisig_counts_use_cores_pushes() {
    let mut s = Vec::new();
    for n in [0, 1, 16, 17, 20] {
        push_small_int(&mut s, n);
    }
    assert_eq!(s, [0x00, 0x51, 0x60, 0x01, 0x11, 0x01, 0x14]);
}

#[test]
fn key_paths_follow_parse_uint32() {
    assert_eq!(parse_key_path(&["k", "+1", "2h", "3'"]), Ok(vec![1, 0x8000_0002, 0x8000_0003]));
    assert_eq!(
        parse_key_path(&["k", "+-1"]),
        Err("Key path value '+-1' is not a valid uint32".into())
    );
    assert_eq!(parse_key_path(&["k", ""]), Err("Key path value '' is not a valid uint32".into()));
    assert_eq!(
        parse_key_path(&["k", "4294967296"]),
        Err("Key path value '4294967296' is not a valid uint32".into())
    );
}
