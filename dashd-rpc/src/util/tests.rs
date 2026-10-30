use super::*;
use crate::descriptor::tests::regtest;

/// `name(params)` on regtest, as its JSON reply or its error's code and message.
fn call(
    handler: fn(&KeyIoParams, &UniValue) -> Result<UniValue, RpcError>,
    params: &str,
) -> Result<String, (i32, String)> {
    let params = UniValue::read(params.as_bytes()).unwrap();
    handler(&regtest(), &params).map(|v| v.write()).map_err(|e| {
        let obj = e.to_object();
        (e.code(), param_str(&obj, "message"))
    })
}

fn param_str(obj: &UniValue, key: &str) -> String {
    match obj {
        UniValue::Obj(entries) => {
            entries.iter().find(|(k, _)| k == key).unwrap().1.get_str().unwrap().to_owned()
        }
        _ => panic!("{}", obj.write()),
    }
}

/// The test framework's `descsum_create`.
fn descsum_create(desc: &str) -> String {
    format!("{desc}#{}", get_descriptor_checksum(desc))
}

/// `deriveaddresses` as `rpc_deriveaddresses.py` calls it.
fn derive(desc: &str, range: Option<&str>) -> Result<String, (i32, String)> {
    let desc = UniValue::from(desc.to_owned()).write();
    match range {
        Some(range) => call(deriveaddresses, &format!("[{desc},{range}]")),
        None => call(deriveaddresses, &format!("[{desc}]")),
    }
}

const TPRV: &str = "tprv8ZgxMBicQKsPd7Uf69XL1XwhmjHopUGep8GuEiJDZmbQz6o58LninorQAfcKZWARbtRtfnLcJ5MQ2AtHcQJCCRUcMRvmDUjyEmNUWwx8UbK";
const TPUB: &str = "tpubD6NzVbkrYhZ4WaWSyoBvQwbpLkojyoTZPRsgXELWz3Popb3qkjcJyJUGLnL4qHHoQvao8ESaAstxYSnhyswJ76uZPStJRJCTKvosUCJZL5B";

// Core: test/functional/rpc_deriveaddresses.py:15-71
#[test]
fn core_deriveaddresses_functional_test() {
    let err = |code: i32, msg: &str| Err((code, msg.to_owned()));
    assert_eq!(derive("a", None), err(-5, "Missing checksum"));
    let descriptor = descsum_create(&format!("pkh({TPRV}/1/1/0)"));
    let address = "yZTyMdEJjZWJi6CwY6g3WurLESH3UsWrrM";
    assert_eq!(derive(&descriptor, None), Ok(format!(r#"["{address}"]"#)));
    assert_eq!(derive(&descriptor[..descriptor.len() - 9], None), err(-5, "Missing checksum"));
    let descriptor_pubkey = descsum_create(&format!("pkh({TPUB}/1/1/0)"));
    assert_eq!(derive(&descriptor_pubkey, None), Ok(format!(r#"["{address}"]"#)));
    let ranged = format!("pkh({TPRV}/1/1/*)#77vpsvm5");
    assert_eq!(
        derive(&ranged, Some("[1, 2]")),
        Ok(r#"["ydccVGNV2EcEouAxbbgdu8pi8gkdaqkiav","yMENst4XYP3ZSNvsCEm587GbSSXZUfhpWG"]"#.into())
    );
    assert_eq!(
        derive(&ranged, Some("2")),
        Ok(format!(
            r#"["{address}","ydccVGNV2EcEouAxbbgdu8pi8gkdaqkiav","yMENst4XYP3ZSNvsCEm587GbSSXZUfhpWG"]"#
        ))
    );
    assert_eq!(
        derive(&descsum_create(&format!("pkh({TPRV}/1/1/0)")), Some("[0, 2]")),
        err(-8, "Range should not be specified for an un-ranged descriptor")
    );
    let ranged = descsum_create(&format!("pkh({TPRV}/1/1/*)"));
    assert_eq!(derive(&ranged, None), err(-8, "Range must be specified for a ranged descriptor"));
    assert_eq!(derive(&ranged, Some("10000000000")), err(-8, "End of range is too high"));
    assert_eq!(derive(&ranged, Some("[1000000000, 2000000000]")), err(-8, "Range is too large"));
    assert_eq!(
        derive(&ranged, Some("[2, 0]")),
        err(-8, "Range specified as [begin,end] must not have begin after end")
    );
    assert_eq!(
        derive(&ranged, Some("[-1, 0]")),
        err(-8, "Range should be greater or equal than 0")
    );
    assert_eq!(
        derive(&descsum_create(&format!("combo({TPRV}/1/1/0)")), None),
        Ok(r#"["yZTyMdEJjZWJi6CwY6g3WurLESH3UsWrrM","yZTyMdEJjZWJi6CwY6g3WurLESH3UsWrrM","93EpXofs6W7eNiuj4gu2LJh8L8opowW1jz"]"#.into())
    );
    assert_eq!(
        derive(&ranged, Some("[2147483647, 2147483647]")),
        Ok(r#"["yUQ4ms3snAAXkWF1AzfnYrxAxgmNVhrydU"]"#.into())
    );
    assert_eq!(
        derive(&descsum_create(&format!("pkh({TPUB}/1'/1/0)")), None),
        err(-5, "Cannot derive script without private keys")
    );
    assert_eq!(
        derive(&descsum_create(&format!("multi(1,{TPUB}/1/1/0,{TPUB}/1/1/1)")), None),
        err(-5, "Descriptor does not have a corresponding address")
    );
}

/// A present range counts even when null: Core tests `params.size()`, then parses only a
/// non-null range. A range that is neither a number nor a pair of numbers is refused first.
// Core: rpc/output_script.cpp:233-252, rpc/util.cpp:1126-1138
#[test]
fn deriveaddresses_counts_a_null_range_and_checks_the_range_first() {
    let ranged = descsum_create(&format!("pkh({TPRV}/1/1/*)"));
    assert_eq!(
        derive(&ranged, Some("null")),
        Ok(r#"["yZTyMdEJjZWJi6CwY6g3WurLESH3UsWrrM"]"#.into())
    );
    let fixed = descsum_create(&format!("pkh({TPRV}/1/1/0)"));
    assert_eq!(
        derive(&fixed, Some("null")),
        Err((-8, "Range should not be specified for an un-ranged descriptor".into()))
    );
    let range_error = Err((-8, "Range must be specified as end or as [begin,end]".into()));
    assert_eq!(derive("a", Some(r#""x""#)), range_error);
    assert_eq!(derive(&ranged, Some("[1]")), range_error);
    assert_eq!(derive(&ranged, Some(r#"[1, "2"]"#)), range_error);
    // A hardened step from a tpub fails at the first position, so a range wrongly let through
    // answers at once instead of deriving a million addresses.
    let underivable = descsum_create(&format!("pkh({TPUB}/*')"));
    assert_eq!(derive(&underivable, Some("[5,1000005]")), Err((-8, "Range is too large".into())));
    assert_eq!(derive("pk(\u{1})", None), Err((-5, "Missing checksum".into())));
}

/// `rpc_getdescriptorinfo.py`'s `test_desc`: the same answer with and without the checksum,
/// the descriptor printed with it, and the three flags.
// Core: test/functional/rpc_getdescriptorinfo.py:21-62
#[test]
fn core_getdescriptorinfo_functional_test() {
    let info = |desc: &str| {
        call(getdescriptorinfo, &format!("[{}]", UniValue::from(desc.to_owned()).write()))
    };
    assert_eq!(info(""), Err((-5, "'' is not a valid descriptor function".into())));
    assert_eq!(
        info("pk(\u{1})#abc"),
        Err((-5, "Expected 8 character checksum, not 3 characters".into()))
    );
    let cases: &[(&str, bool)] = &[
        ("pk(0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798)", false),
        ("pkh(02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5)", false),
        ("sh(pkh(03fff97bd5755eeea420453a14355235d382f6472f8568a18b2f057a1460297556))", false),
        ("combo(0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798)", false),
        (
            "multi(1,022f8bde4d1a07209355b4a7250a5c5128e88b84bddc619ab7cba8d569b240efe4,025cbdf0646e5db4eaa398f365f2ea7a0e3d419b7e0330e39ce92bddedcac4f9bc)",
            false,
        ),
        (
            "sh(multi(2,022f01e5e15cca351daff3843fb70f3c2f0a1bdd05e5af888a67784ef3e10a2a01,03acd484e2f0c7f65309ad178a9f559abde09796974c57e714c35f110dfc27ccbe))",
            false,
        ),
        (
            "sh(multi(2,03a0434d9e47f3c86235477c7b1ae6ae5d3442d49b1943c2b752a68e2a47e247c7,03774ae7f858a9411e5ef4246b70c65aac5649980be5c17891bbec17895da008cb,03d01115d548e7561b15c38f004d734633687cf4419620095bc5b0f47070afe85a))",
            false,
        ),
        (
            "sh(multi(1,03f28773c2d975288bc7d1d205c3748651b075fbc6610e58cddeeddf8f19405aa8,03499fdf9e895e719cfd64e67f07d38e3226aa7b63678949e6e49b241a60e823e4,02d7924d4f7d43ea965a465ae3095ff41131e5946f3c85f79e44adbcf8e27e080e))",
            false,
        ),
        (
            "pk(tpubD6NzVbkrYhZ4WaWSyoBvQwbpLkojyoTZPRsgXELWz3Popb3qkjcJyJUGLnL4qHHoQvao8ESaAstxYSnhyswJ76uZPStJRJCTKvosUCJZL5B)",
            false,
        ),
        (
            "pkh(tpubD6NzVbkrYhZ4WaWSyoBvQwbpLkojyoTZPRsgXELWz3Popb3qkjcJyJUGLnL4qHHoQvao8ESaAstxYSnhyswJ76uZPStJRJCTKvosUCJZL5B/1h/2)",
            false,
        ),
        (
            "pkh([d34db33f/44h/0h/0h]tpubD6NzVbkrYhZ4WaWSyoBvQwbpLkojyoTZPRsgXELWz3Popb3qkjcJyJUGLnL4qHHoQvao8ESaAstxYSnhyswJ76uZPStJRJCTKvosUCJZL5B/1/*)",
            true,
        ),
        (
            "sh(multi(1,tpubD6NzVbkrYhZ4WaWSyoBvQwbpLkojyoTZPRsgXELWz3Popb3qkjcJyJUGLnL4qHHoQvao8ESaAstxYSnhyswJ76uZPStJRJCTKvosUCJZL5B/1/0/*,tpubD6NzVbkrYhZ4WaWSyoBvQwbpLkojyoTZPRsgXELWz3Popb3qkjcJyJUGLnL4qHHoQvao8ESaAstxYSnhyswJ76uZPStJRJCTKvosUCJZL5B/0/0/*))",
            true,
        ),
    ];
    for &(desc, isrange) in cases {
        let got = info(desc).unwrap();
        assert_eq!(got, info(&descsum_create(desc)).unwrap(), "{desc}");
        let want = format!(
            r#"{{"descriptor":"{}","checksum":"{}","isrange":{isrange},"issolvable":true,"hasprivatekeys":false}}"#,
            descsum_create(desc),
            get_descriptor_checksum(desc)
        );
        assert_eq!(got, want);
    }
    // A WIF key: private keys, printed as its public key; addr() and raw() are unsolvable.
    let got = info("pkh(cU4zhap7nPJAWeMFu4j6jLrfPmqakDAzy8zn8Fhb3oEevdm4e5Lc)").unwrap();
    assert!(got.contains(r#""hasprivatekeys":true"#) && !got.contains("cU4z"), "{got}");
    for desc in ["addr(yZTyMdEJjZWJi6CwY6g3WurLESH3UsWrrM)", "raw(51)", "sh(raw(51))"] {
        match info(desc) {
            Ok(got) => assert!(got.contains(r#""issolvable":false"#), "{got}"),
            Err((code, msg)) => {
                assert_eq!((code, msg.as_str()), (-5, "Can only have raw() at top level"))
            }
        }
    }
}

/// `createmultisig`: `HexToPubKey`'s and `AddAndGetMultisigDestination`'s errors in order, and
/// a descriptor inferred with only the redeem script known: `sh(multi(...))` of the keys as
/// given, or `addr()` when a key is hybrid (`InferPubkey` refuses it).
// Core: rpc/output_script.cpp:99-153, rpc/util.cpp:217-275, script/descriptor.cpp:1131-1210
#[test]
fn createmultisig_errors_and_descriptors_are_cores() {
    let ms = |params: &str| call(createmultisig, params);
    let invalid = |code: i32, msg: &str| Err((code, msg.to_owned()));
    let g = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
    let g_full = "0479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8";
    let g_hybrid = format!("06{}", &g_full[2..]);
    assert_eq!(ms(r#"[1, ["zz"]]"#), invalid(-5, "Pubkey \"zz\" must be a hex string"));
    assert_eq!(
        ms(r#"[1, ["00"]]"#),
        invalid(-5, "Pubkey \"00\" must have a length of either 33 or 65 bytes")
    );
    let bad = format!("05{}", &g[2..]);
    assert_eq!(
        ms(&format!(r#"[1, ["{bad}"]]"#)),
        invalid(-5, &format!("Pubkey \"{bad}\" must be cryptographically valid."))
    );
    assert_eq!(
        ms(&format!(r#"[0, ["{g}"]]"#)),
        invalid(-8, "a multisignature address must require at least one key to redeem")
    );
    assert_eq!(
        ms(&format!(r#"[2, ["{g}"]]"#)),
        invalid(-8, "not enough keys supplied (got 1 keys, but need at least 2 to redeem)")
    );
    let keys = |key: &str, n: usize| vec![format!("\"{key}\""); n].join(",");
    assert_eq!(
        ms(&format!("[1, [{}]]", keys(g, 21))),
        invalid(
            -8,
            "Number of keys involved in the multisignature address creation > 20\nReduce the number"
        )
    );
    assert_eq!(
        ms(&format!("[1, [{}]]", keys(g, 16))),
        invalid(-8, "redeemScript exceeds size limit: 547 > 520")
    );
    assert_eq!(
        ms(r#"[1, [1]]"#),
        invalid(-3, "JSON value of type number is not of expected type string")
    );

    let p2sh_of = |redeem: &str| {
        let hash = hash160(&parse_hex(redeem).unwrap());
        encode_destination(&Destination::ScriptHash(hash), &regtest())
    };
    let got = ms(&format!(r#"[2, ["{g}", "{g_full}"]]"#)).unwrap();
    let redeem = format!("5221{g}41{g_full}52ae");
    let want = format!(
        r#"{{"address":"{}","redeemScript":"{redeem}","descriptor":"{}"}}"#,
        p2sh_of(&redeem),
        descsum_create(&format!("sh(multi(2,{g},{g_full}))"))
    );
    assert_eq!(got, want);
    let redeem = format!("5121{g}41{g_hybrid}52ae");
    let got = ms(&format!(r#"[1, ["{g}", "{g_hybrid}"]]"#)).unwrap();
    let want_desc = descsum_create(&format!("addr({})", p2sh_of(&redeem)));
    assert!(got.contains(&format!(r#""descriptor":"{want_desc}""#)), "{got}");
}

/// `validateaddress`: an L1 address, a DIP-18 Platform address with its credit script, and a
/// failure's error and locations.
// Core: rpc/output_script.cpp:26-97
#[test]
fn validateaddress_answers_cores_three_shapes() {
    let v = |address: &str| call(validateaddress, &format!(r#"["{address}"]"#)).unwrap();
    assert_eq!(
        v("yZTyMdEJjZWJi6CwY6g3WurLESH3UsWrrM"),
        format!(
            r#"{{"isvalid":true,"address":"yZTyMdEJjZWJi6CwY6g3WurLESH3UsWrrM","scriptPubKey":"{}","isscript":false}}"#,
            hex(&decode_destination_with_error("yZTyMdEJjZWJi6CwY6g3WurLESH3UsWrrM", &regtest())
                .unwrap()
                .script())
        )
    );
    let platform =
        encode_platform_destination(&crate::key_io::PlatformDestination::P2sh([7; 20]), &regtest());
    assert_eq!(
        v(&platform),
        format!(
            r#"{{"isvalid":true,"isplatform":true,"address":"{platform}","scriptPubKey":"a914{}87","isscript":true}}"#,
            hex(&[7; 20])
        )
    );
    assert_eq!(
        v("0"),
        r#"{"isvalid":false,"error_locations":[],"error":"Not a valid Bech32m or Base58 encoding"}"#
    );
    assert_eq!(
        v("x"),
        r#"{"isvalid":false,"error_locations":[],"error":"Invalid checksum or length of Base58 address"}"#
    );
    // A Platform address with one character changed: bech32's located error.
    let mut broken = platform.clone().into_bytes();
    let last = broken.len() - 1;
    broken[last] = if broken[last] == b'q' {
        b'p'
    } else {
        b'q'
    };
    let got = v(std::str::from_utf8(&broken).unwrap());
    assert!(
        got.starts_with(r#"{"isvalid":false,"error_locations":["#) && !got.contains("[]"),
        "{got}"
    );
}

/// `verifymessage`'s replies for each `MessageVerificationResult`, and
/// `signmessagewithprivkey`'s, from the FT's key.
// Core: rpc/signmessage.cpp:35-48, :90-101; test/functional/rpc_signmessagewithprivkey.py
#[test]
fn the_message_commands_map_cores_results() {
    let key = "cU4zhap7nPJAWeMFu4j6jLrfPmqakDAzy8zn8Fhb3oEevdm4e5Lc";
    let address = "yeMpGzMj3rhtnz48XsfpB8itPHhHtgxLc3";
    let message = "This is just a test message";
    let signature =
        "ICzMhjIUmmXcPWy2+9nw01zQMawo+s5FIy6F7VMkL+TmIeNq1j3AMEuw075os29kh5KYLbysKkDlDD+EAqERBd4=";
    assert_eq!(
        call(signmessagewithprivkey, &format!(r#"["{key}", "{message}"]"#)),
        Ok(format!(r#""{signature}""#))
    );
    assert_eq!(
        call(signmessagewithprivkey, r#"["x", "m"]"#),
        Err((-5, "Invalid private key".into()))
    );
    let verify =
        |a: &str, s: &str, m: &str| call(verifymessage, &format!(r#"["{a}", "{s}", "{m}"]"#));
    assert_eq!(verify(address, signature, message), Ok("true".into()));
    assert_eq!(verify(address, signature, "other"), Ok("false".into()));
    assert_eq!(verify(address, "AAAA", message), Ok("false".into()));
    assert_eq!(verify("x", signature, message), Err((-5, "Invalid address".into())));
    assert_eq!(verify(address, "!", message), Err((-3, "Malformed base64 encoding".into())));
    let p2sh = encode_destination(&Destination::ScriptHash([1; 20]), &regtest());
    assert_eq!(
        verify(&p2sh, signature, message),
        Err((-3, "Address does not refer to key".into()))
    );
}

/// `EvalDescriptorStringOrObject` over a JSON scan object, as hex scripts or the error's code
/// and message.
fn eval(scanobject: &str, provider: &mut SigningProvider) -> Result<Vec<String>, (i32, String)> {
    let scanobject = UniValue::read(scanobject.as_bytes()).unwrap();
    eval_descriptor_string_or_object(&scanobject, provider, &regtest())
        .map(|scripts| scripts.iter().map(|s| hex(s)).collect())
        .map_err(|e| (e.code(), param_str(&e.to_object(), "message")))
}

/// The P2PKH script of `deriveaddresses`' address for `desc` at `pos`.
fn derived_script(desc: &str, pos: u32) -> String {
    let desc = ParsedDescriptor::parse(desc, &regtest(), false).unwrap();
    hex(&desc.expand(pos).unwrap()[0])
}

// Core: rpc/util.cpp:1184-1215
#[test]
fn a_scan_object_expands_over_its_range() {
    let ranged = format!("pkh({TPUB}/1/1/*)");
    let mut provider = SigningProvider::default();
    // A string: [0, 1000], no checksum needed.
    let all = eval(&format!(r#""{ranged}""#), &mut provider).unwrap();
    assert_eq!(all.len(), 1001);
    assert_eq!(all[0], derived_script(&ranged, 0));
    assert_eq!(all[1000], derived_script(&ranged, 1000));
    // An object: no range is [0, 1000]; a null one too; `end` is [0, end]; [begin, end].
    for (range, want) in [("", 0..=1000), (r#","range":null"#, 0..=1000), (r#","range":5"#, 0..=5)]
    {
        let got = eval(&format!(r#"{{"desc":"{ranged}"{range}}}"#), &mut provider).unwrap();
        let want: Vec<String> = want.map(|i| derived_script(&ranged, i)).collect();
        assert_eq!(got, want, "{range}");
    }
    let got = eval(&format!(r#"{{"desc":"{ranged}","range":[2,4]}}"#), &mut provider).unwrap();
    assert_eq!(got, (2..=4).map(|i| derived_script(&ranged, i)).collect::<Vec<_>>());
    let got = eval(&format!(r#"{{"desc":"{ranged}","range":[7,7]}}"#), &mut provider).unwrap();
    assert_eq!(got, [derived_script(&ranged, 7)]);
    // With a checksum, and the first "desc" of duplicates.
    let summed = descsum_create(&ranged);
    let got =
        eval(&format!(r#"{{"desc":"{summed}","desc":1,"range":[3,3]}}"#), &mut provider).unwrap();
    assert_eq!(got, [derived_script(&ranged, 3)]);
    // An unranged descriptor expands at 0 alone, whatever valid range it is given; combo()
    // gives its three scripts.
    let fixed = format!("pkh({TPUB}/1/1/9)");
    let got = eval(&format!(r#"{{"desc":"{fixed}","range":[3,9]}}"#), &mut provider).unwrap();
    assert_eq!(got, [derived_script(&fixed, 0)]);
    let key = "03a34b99f22c790c4e36b2b3c2c35a36db06226e41c692fc82b8b56ac1c540c5bd";
    assert_eq!(eval(&format!(r#""combo({key})""#), &mut provider).unwrap().len(), 3);
    // A hardened step from an xprv derives.
    let hardened = format!("pkh({TPRV}/0'/*')");
    assert_eq!(
        eval(&format!(r#"{{"desc":"{hardened}","range":1}}"#), &mut provider).unwrap().len(),
        2
    );
}

// Core: rpc/util.cpp:1126-1153, 1184-1215
#[test]
fn a_scan_objects_errors_are_cores() {
    let mut provider = SigningProvider::default();
    let ranged = format!("pkh({TPUB}/1/1/*)");
    let err = |code: i32, msg: &str| Err((code, msg.to_string()));
    let no_desc = err(-8, "Descriptor needs to be provided in scan object");
    assert_eq!(eval(r#"{"range":5}"#, &mut provider), no_desc);
    assert_eq!(eval(r#"{"desc":null}"#, &mut provider), no_desc);
    let shape = err(-8, "Scan object needs to be either a string or an object");
    for v in ["5", "true", "null", r#"["pk(00)"]"#] {
        assert_eq!(eval(v, &mut provider), shape, "{v}");
    }
    // `get_str` of a non-string desc is UniValue's type error.
    let (code, msg) = eval(r#"{"desc":5}"#, &mut provider).unwrap_err();
    assert_eq!(
        (code, msg.as_str()),
        (-3, "JSON value of type number is not of expected type string")
    );
    // The range is checked before the descriptor is parsed, and for an unranged one too.
    for (range, msg) in [
        ("[3,2]", "Range specified as [begin,end] must not have begin after end"),
        (r#""5""#, "Range must be specified as end or as [begin,end]"),
        ("[1]", "Range must be specified as end or as [begin,end]"),
        ("[1,2,3]", "Range must be specified as end or as [begin,end]"),
        // `end` alone is [0, end], so a negative end is too high (its sign bit).
        ("-1", "End of range is too high"),
        ("[-1,5]", "Range should be greater or equal than 0"),
        ("2147483648", "End of range is too high"),
        ("[5,2147483648]", "End of range is too high"),
        ("1000000", "Range is too large"),
        ("[5,1000005]", "Range is too large"),
    ] {
        for desc in ["pkh(junk)", ranged.as_str(), "raw(00)"] {
            let obj = format!(r#"{{"desc":"{desc}","range":{range}}}"#);
            assert_eq!(eval(&obj, &mut provider), err(-8, msg), "{obj}");
        }
    }
    // Just inside each bound (an unranged descriptor, so nothing derives a million times).
    for range in ["999999", "[5,1000004]", "[0,2147483647]", "[2147483647,2147483647]"] {
        let obj = format!(r#"{{"desc":"raw(00)","range":{range}}}"#);
        let got = eval(&obj, &mut provider);
        if range == "[0,2147483647]" {
            assert_eq!(got, err(-8, "Range is too large"), "{obj}");
        } else {
            assert_eq!(got, Ok(vec!["00".to_string()]), "{obj}");
        }
    }
    // A parse error is RPC_INVALID_ADDRESS_OR_KEY with Parse's message.
    assert_eq!(eval(r#""pkh(junk)""#, &mut provider), err(-5, "pkh(): key 'junk' is not valid"));
    assert_eq!(eval(r#""raw(00)#00000000""#, &mut provider).unwrap_err().0, -5);
    // A hardened step from a tpub cannot derive: the descriptor as given, quoted.
    let underivable = format!("pkh({TPUB}/*')");
    assert_eq!(
        eval(&format!(r#""{underivable}""#), &mut provider),
        err(-5, &format!("Cannot derive script without private keys: '{underivable}'"))
    );
}

/// The expansions fill the provider given, which the inference then reads.
#[test]
fn a_scan_objects_expansions_fill_its_provider() {
    let mut provider = SigningProvider::default();
    let key = "03a34b99f22c790c4e36b2b3c2c35a36db06226e41c692fc82b8b56ac1c540c5bd";
    let p2pkh = eval(&format!(r#""pkh({key})""#), &mut provider).unwrap();
    let script = parse_hex(&p2pkh[0]).unwrap();
    let desc = crate::descriptor::infer_descriptor_with(&script, &provider, &regtest());
    assert!(desc.starts_with("pkh(["), "{desc}");
}
