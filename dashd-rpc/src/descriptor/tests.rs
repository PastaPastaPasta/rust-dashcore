use super::*;

#[path = "core_vectors.rs"]
mod core_vectors;

use core_vectors::{CHECKS, DEFAULT, HARDENED, INFER, RANGE, UNPARSABLE};

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
                let got = desc
                    .expand(pos as u32, provider, &mut SigningProvider::default())
                    .unwrap_or_else(|| panic!("line {}", c.line));
                let got: Vec<String> = got.iter().map(|s| hex(s)).collect();
                assert_eq!(got, want, "line {} pos {pos}", c.line);
            }
        }
        if c.flags & HARDENED != 0 {
            assert!(
                pubd.expand(0, &keys_pub, &mut SigningProvider::default()).is_none(),
                "line {}: no private key",
                c.line
            );
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

/// Core's `DoCheck` inference half: each script an expansion gives infers, from the provider
/// the expansion filled, to a descriptor as solvable as the flags say that expands back to the
/// script alone with the same key origins; and the origins' paths are the vector's `paths`.
// Core: src/test/descriptor_tests.cpp:144-153, 245-267
#[test]
fn core_check_vectors_infer_back_with_their_key_origins() {
    let p = mainnet();
    for c in CHECKS {
        let (prv, keys_prv) = parsed(c.prv, &p);
        let (pubd, keys_pub) = parsed(c.pub_, &p);
        let keys = if c.flags & HARDENED != 0 {
            &keys_prv
        } else {
            &keys_pub
        };
        let max = if c.flags & RANGE != 0 {
            c.scripts.len()
        } else {
            3
        };
        let mut left: Vec<&[u32]> = c.paths.to_vec();
        for pos in 0..max as u32 {
            for desc in [&pubd, &prv] {
                let mut out = SigningProvider::default();
                let spks = desc.expand(pos, keys, &mut out).expect("expands");
                for spk in &spks {
                    let inferred = infer_script(spk, Context::Top, &out).expect("top level");
                    assert_eq!(
                        inferred.is_solvable(),
                        c.flags & UNSOLVABLE == 0,
                        "line {}",
                        c.line
                    );
                    let mut again = SigningProvider::default();
                    let back = inferred.expand(0, &Keys::new(), &mut again).expect("expands");
                    assert_eq!(back, std::slice::from_ref(spk), "line {}", c.line);
                    assert_eq!(again.origins, out.origins, "line {}", c.line);
                }
                for origin in out.origins.values() {
                    assert!(
                        c.paths.contains(&origin.path.as_slice()),
                        "line {}: unexpected key path {:?}",
                        c.line,
                        origin.path
                    );
                    left.retain(|path| *path != origin.path.as_slice());
                }
            }
        }
        assert!(left.is_empty(), "line {}: paths not seen {left:?}", c.line);
    }
}

/// Core's `CheckInferDescriptor`: the provider holds the scripts, the public keys and the
/// origins given (`fingerprint/path`, `'` or `h` hardened).
// Core: src/test/descriptor_tests.cpp:281-327, 443-452
#[test]
fn core_infer_vectors_print_as_cores() {
    let p = mainnet();
    for &(line, script, want, scripts, pubkeys) in INFER {
        let mut provider = SigningProvider::default();
        for s in scripts {
            provider.add_script(parse_hex(s).unwrap());
        }
        for &(pubkey, origin) in pubkeys {
            let pubkey = parse_hex(pubkey).unwrap();
            if !origin.is_empty() {
                let split: Vec<&str> = origin.split('/').collect();
                let mut apostrophe = false;
                provider.add_origin(
                    &pubkey,
                    KeyOrigin {
                        fingerprint: parse_hex(split[0]).unwrap().try_into().unwrap(),
                        path: parse_key_path(&split, &mut apostrophe).unwrap(),
                    },
                );
            }
            provider.add_pubkey(pubkey);
        }
        let got = infer_descriptor_with(&parse_hex(script).unwrap(), &provider, &p);
        assert_eq!(got, add_checksum(want.to_string()), "line {line}");
    }
    assert_eq!(INFER.len(), 5);
}

/// The `[fingerprint/path]` origins `scantxoutset`'s inference prints for the scripts its
/// needles expand to: a constant key's own id prefix, a ranged xpub's root id prefix with the
/// position, an origin's fingerprint before the key's path; the first origin under a key
/// stays; `pkh()` and `combo()` make P2PKH scripts infer as `pkh()`, `sh()` and `combo()`
/// make P2SH ones infer as `sh()`; `multi()` alone records no public key, and `addr()` nothing.
// Core: script/descriptor.cpp:211-217, 264-270, 347-360, 675-686, 779-855, 1131-1210
#[test]
fn expansions_infer_with_cores_origins_per_shape() {
    let p = regtest();
    let key = "03a34b99f22c790c4e36b2b3c2c35a36db06226e41c692fc82b8b56ac1c540c5bd";
    let key_fp = hex(&hash160(&parse_hex(key).unwrap())[..4]);
    let xpub = "tpubD6NzVbkrYhZ4WaWSyoBvQwbpLkojyoTZPRsgXELWz3Popb3qkjcJyJUGLnL4qHHoQvao8ESaAstxYSnhyswJ76uZPStJRJCTKvosUCJZL5B";
    let expand = |descs: &[&str], pos: u32| {
        let mut out = SigningProvider::default();
        let mut scripts = Vec::new();
        for d in descs {
            scripts.extend(
                ParsedDescriptor::parse(d, &p, false).unwrap().expand_into(pos, &mut out).unwrap(),
            );
        }
        (scripts, out)
    };
    let infer = |script: &[u8], out: &SigningProvider| {
        let s = infer_descriptor_with(script, out, &p);
        s[..s.len() - 9].to_string()
    };

    // pk(): a constant key behind its own id's fingerprint.
    let (s, out) = expand(&[&format!("pk({key})")], 0);
    assert_eq!(infer(&s[0], &out), format!("pk([{key_fp}]{key})"));
    // The dummy provider (ScriptToUniv) has no origins.
    assert_eq!(infer(&s[0], &SigningProvider::default()), format!("pk({key})"));

    // pkh(): the public key is known, so P2PKH infers as pkh(); alone it is addr().
    let (s, out) = expand(&[&format!("pkh({key})")], 0);
    assert_eq!(infer(&s[0], &out), format!("pkh([{key_fp}]{key})"));
    assert!(infer(&s[0], &SigningProvider::default()).starts_with("addr("));

    // pkh(xpub/0/*): the root's fingerprint, the path and the position.
    let root_fp = {
        let xp = decode_ext_pubkey(xpub, &p).unwrap();
        hex(&hash160(&xp.pubkey.serialize())[..4])
    };
    for pos in [0, 3, 1000] {
        let (s, out) = expand(&[&format!("pkh({xpub}/0/*)")], pos);
        let got = infer(&s[0], &out);
        assert!(got.starts_with(&format!("pkh([{root_fp}/0/{pos}]")), "{got}");
    }
    // A hardened step prints with h, and an origin's fingerprint replaces the root's.
    let (s, out) = expand(&[&format!("pkh([deadbeef/44'/1']{xpub}/0/*)")], 7);
    assert!(infer(&s[0], &out).starts_with("pkh([deadbeef/44h/1h/0/7]"));

    // combo(): P2PK, P2PKH and P2SH-P2PKH.
    let (s, out) = expand(&[&format!("combo({key})")], 0);
    assert_eq!(infer(&s[0], &out), format!("pk([{key_fp}]{key})"));
    assert_eq!(infer(&s[1], &out), format!("pkh([{key_fp}]{key})"));
    assert_eq!(infer(&s[2], &out), format!("sh(pkh([{key_fp}]{key}))"));

    // sh(multi()): the redeem script is known, each key with its origin.
    let key2 = "02df12b7035bdac8e3bab862a3a83d06ea6b17b6753d52edecba9be46f5d09e076";
    let key2_fp = hex(&hash160(&parse_hex(key2).unwrap())[..4]);
    let (s, out) = expand(&[&format!("sh(multi(1,{key},[01020304/5]{key2}))")], 0);
    assert_eq!(infer(&s[0], &out), format!("sh(multi(1,[{key_fp}]{key},[01020304/5]{key2}))"));
    assert_ne!(key2_fp, "01020304");
    // multi() records no public key: a P2PKH of its key stays addr().
    let p2pkh = Destination::PkHash(hash160(&parse_hex(key).unwrap())).script();
    assert!(infer(&p2pkh, &out).starts_with("addr("));

    // The first origin under a key stays, across descriptors expanded into one provider.
    let (s, out) = expand(&[&format!("pk([aabbccdd/1]{key})"), &format!("pkh({key})")], 0);
    assert_eq!(infer(&s[0], &out), format!("pk([aabbccdd/1]{key})"));
    assert_eq!(infer(&s[1], &out), format!("pkh([aabbccdd/1]{key})"));
    let (_, out) = expand(&[&format!("pkh({key})"), &format!("pk([aabbccdd/1]{key})")], 0);
    assert_eq!(infer(&p2pkh, &out), format!("pkh([{key_fp}]{key})"));

    // addr() and raw() record nothing.
    let addr = encode_destination(&Destination::PkHash(hash160(&parse_hex(key).unwrap())), &p);
    let (s, out) = expand(&[&format!("addr({addr})"), &format!("raw({})", hex(&p2pkh))], 0);
    assert_eq!(infer(&s[0], &out), format!("addr({addr})"));
    assert_eq!(infer(&s[1], &out), format!("addr({addr})"));

    // An uncompressed combo() key gives P2PK only, but records its public key.
    let ukey = "04a34b99f22c790c4e36b2b3c2c35a36db06226e41c692fc82b8b56ac1c540c5bd5b8dec5235a0fa8722476c7709c02559e3aa73aa03918ba2d492eea75abea235";
    let ukey_fp = hex(&hash160(&parse_hex(ukey).unwrap())[..4]);
    let (s, out) = expand(&[&format!("combo({ukey})")], 0);
    assert_eq!(s.len(), 1);
    let up2pkh = Destination::PkHash(hash160(&parse_hex(ukey).unwrap())).script();
    assert_eq!(infer(&up2pkh, &out), format!("pkh([{ukey_fp}]{ukey})"));
}

/// A key that does not derive leaves the provider as it was.
#[test]
fn a_failed_expansion_records_nothing() {
    let p = mainnet();
    let c = CHECKS.iter().find(|c| c.flags & HARDENED != 0).expect("a hardened vector");
    let (pubd, keys_pub) = parsed(c.pub_, &p);
    let mut out = SigningProvider::default();
    assert!(pubd.expand(0, &keys_pub, &mut out).is_none());
    assert!(out.origins.is_empty() && out.pubkeys.is_empty() && out.scripts.is_empty());
}
