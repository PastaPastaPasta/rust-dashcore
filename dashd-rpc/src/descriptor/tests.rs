use super::*;

#[path = "core_vectors.rs"]
mod core_vectors;

use core_vectors::{CHECKS, DEFAULT, HARDENED, RANGE, UNPARSABLE};

/// `UNSOLVABLE`: the descriptor is not expected to be solvable.
const UNSOLVABLE: u32 = 4;

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

fn parsed(s: &str, p: &KeyIoParams) -> (Descriptor, Keys) {
    let mut keys = Keys::new();
    let desc = parse(s, p, &mut keys, false).unwrap_or_else(|e| panic!("{s}: {e}"));
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

/// Core's `EqualDescriptor`: equal, with a checksum on only one side ignored.
// Core: src/test/descriptor_tests.cpp:46-56
fn equal_descriptor(mut a: &str, mut b: &str) -> bool {
    let check = |s: &str| s.len() > 9 && s.as_bytes()[s.len() - 9] == b'#';
    if check(a) != check(b) {
        if check(a) {
            a = &a[..a.len() - 9];
        }
        if check(b) {
            b = &b[..b.len() - 9];
        }
    }
    a == b
}

/// Core's `UseHInsteadOfApostrophe`: every `'` as `h`, with the checksum recomputed.
// Core: src/test/descriptor_tests.cpp:58-75
fn use_h_instead_of_apostrophe(desc: &str) -> String {
    let mut ret = desc.replace('\'', "h");
    if get_descriptor_checksum(&ret).is_empty() {
        ret.truncate(desc.len() - 9);
        let sum = get_descriptor_checksum(&ret);
        ret = ret + "#" + &sum;
    }
    ret
}

/// Core's `DoCheck` string and key checks: both forms print as the public form (keeping the
/// written `'` or `h`), only the private form carries keys, and solvability is as the flags
/// say; then again with `h` for every `'` where both forms have one (`Check`).
// Core: src/test/descriptor_tests.cpp:79-120, :270-282
#[test]
fn core_check_vectors_print_as_cores_public_form() {
    let p = mainnet();
    for c in CHECKS {
        let mut cases = vec![(c.prv.to_owned(), c.pub_.to_owned())];
        if c.prv.contains('\'') && c.pub_.contains('\'') {
            cases.push((use_h_instead_of_apostrophe(c.prv), use_h_instead_of_apostrophe(c.pub_)));
        }
        for (prv, pubd) in cases {
            let parse_priv = ParsedDescriptor::parse(&prv, &p, false).unwrap();
            let parse_pub = ParsedDescriptor::parse(&pubd, &p, false).unwrap();
            for d in [&parse_priv, &parse_pub] {
                let printed = d.to_string(&p);
                assert!(equal_descriptor(&pubd, &printed), "line {}: {printed} vs {pubd}", c.line);
                assert_eq!(d.is_solvable(), c.flags & UNSOLVABLE == 0, "line {}", c.line);
            }
            assert!(parse_priv.has_private_keys(), "line {}", c.line);
            assert!(!parse_pub.has_private_keys(), "line {}", c.line);
        }
    }
}

/// `require_checksum`: "Missing checksum" before any other check of the body; the checksum
/// `GetDescriptorChecksum` reports is the body's, or empty on a `CheckChecksum` failure.
// Core: script/descriptor.cpp:1212-1264, rpc/output_script.cpp:254
#[test]
fn a_required_checksum_is_cores() {
    let p = mainnet();
    let body = "pk(03a34b99f22c790c4e36b2b3c2c35a36db06226e41c692fc82b8b56ac1c540c5bd)";
    let err = |s: &str| ParsedDescriptor::parse(s, &p, true).err().unwrap();
    assert_eq!(err(body), "Missing checksum");
    assert_eq!(err("pk(\u{1})"), "Missing checksum");
    assert_eq!(err("a#b#c"), "Multiple '#' symbols");
    let sum = get_descriptor_checksum(body);
    assert_eq!(sum.len(), 8);
    let with = format!("{body}#{sum}");
    assert!(ParsedDescriptor::parse(&with, &p, true).is_ok());
    assert_eq!(get_descriptor_checksum(&with), sum);
    assert_eq!(get_descriptor_checksum(&format!("{body}#xxxxxxxx")), "");
    assert_eq!(get_descriptor_checksum("pk(\u{1})"), "");
}

/// The apostrophe flag is shared by a key's origin and its path, the last hardened element
/// (or `*'`/`*h`) written deciding it for the whole key.
// Core: script/descriptor.cpp:875-899, 950-1003
#[test]
fn the_last_hardened_marker_decides_a_keys_apostrophes() {
    let p = mainnet();
    let xpub = "xpub661MyMwAqRbcFtXgS5sYJABqqG9YLmC4Q1Rdap9gSE8NqtwybGhePY2gZ29ESFjqJoCu1Rupje8YtGqsefD265TMg7usUDFdp6W1EGMcet8";
    let printed = |body: &str| {
        let s = ParsedDescriptor::parse(body, &p, false).unwrap().to_string(&p);
        s[..s.len() - 9].to_owned()
    };
    // The origin's `'`, then the path's `h`: `h` everywhere.
    assert_eq!(
        printed(&format!("pkh([deadbeef/1']{xpub}/2h)")),
        format!("pkh([deadbeef/1h]{xpub}/2h)")
    );
    // The path's `h`, then `*'`: the derivation suffix is checked first, so `h` wins.
    assert_eq!(printed(&format!("pkh({xpub}/2h/*')")), format!("pkh({xpub}/2h/*h)"));
    // `*'` with an unhardened path: `'`.
    assert_eq!(
        printed(&format!("pkh([deadbeef/0h]{xpub}/2/*')")),
        format!("pkh([deadbeef/0']{xpub}/2/*')")
    );
    // No hardened element: the flag stays false, and nothing prints it.
    assert_eq!(
        printed(&format!("pkh([deadbeef/1]{xpub}/2/*)")),
        format!("pkh([deadbeef/1]{xpub}/2/*)")
    );
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
        assert!(parse(prv, &p, &mut Keys::new(), false).is_err(), "line {line}");
        assert_eq!(
            parse(pubd, &p, &mut Keys::new(), false).err().as_deref(),
            Some(*error),
            "line {line}"
        );
    }
}

#[test]
fn checksum_errors_are_cores() {
    let p = mainnet();
    let err = |s: &str| parse(s, &p, &mut Keys::new(), false).err().unwrap();
    assert_eq!(err("raw(00)#a#b"), "Multiple '#' symbols");
    assert_eq!(err("raw(00)#abc"), "Expected 8 character checksum, not 3 characters");
    assert_eq!(err("raw(00)#"), "Expected 8 character checksum, not 0 characters");
    // The checksum's presence and length come before the payload's characters.
    assert_eq!(err("pk(\x01)#abc"), "Expected 8 character checksum, not 3 characters");
    assert_eq!(
        parse("pk(\x01)", &p, &mut Keys::new(), true).err().as_deref(),
        Some("Missing checksum")
    );
    let sum = descriptor_checksum("raw(00)").unwrap();
    assert!(parse(&format!("raw(00)#{sum}"), &p, &mut Keys::new(), false).is_ok());
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
    assert_eq!(parse("raw(00))", &p, &mut Keys::new(), false).err().as_deref(), Some(""));
    assert_eq!(parse("raw(00),", &p, &mut Keys::new(), false).err().as_deref(), Some(""));
    let g = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
    assert!(parse(&format!("sh(pk({g}))"), &p, &mut Keys::new(), false).is_ok());
    assert_eq!(
        parse(&format!("sh(pk({g}),1)"), &p, &mut Keys::new(), false).err().as_deref(),
        Some("")
    );
    assert_eq!(
        parse("foo(00)", &p, &mut Keys::new(), false).err().as_deref(),
        Some("'foo(00)' is not a valid descriptor function")
    );
    assert_eq!(
        parse("sh(raw(00))", &p, &mut Keys::new(), false).err().as_deref(),
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
    let path = |split: &[&str]| parse_key_path(split, &mut false);
    let mut apostrophe = false;
    assert_eq!(
        parse_key_path(&["k", "+1", "2h", "3'"], &mut apostrophe),
        Ok(vec![1, 0x8000_0002, 0x8000_0003])
    );
    assert!(apostrophe, "the last hardened element used '");
    assert_eq!(path(&["k", "+-1"]), Err("Key path value '+-1' is not a valid uint32".into()));
    assert_eq!(path(&["k", ""]), Err("Key path value '' is not a valid uint32".into()));
    assert_eq!(
        path(&["k", "4294967296"]),
        Err("Key path value '4294967296' is not a valid uint32".into())
    );
}
