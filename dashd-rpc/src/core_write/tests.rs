use super::*;
use crate::descriptor::get_descriptor_checksum;
use crate::request::{find_value, get_int_i64, parse_hex};

const G: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

fn script(hex: &str) -> Vec<u8> {
    parse_hex(hex).unwrap()
}

/// A direct push of `data` (at most 75 bytes).
fn push(data: &[u8]) -> Vec<u8> {
    let mut s = vec![u8::try_from(data.len()).unwrap()];
    s.extend_from_slice(data);
    s
}

/// `script_GetScriptAsm`'s cases without sighash decoding.
// Core: src/test/script_tests.cpp:1245-1273
#[test]
fn core_get_script_asm_vectors() {
    assert_eq!(script_to_asm_str(&[0xb1], false), "OP_CHECKLOCKTIMEVERIFY");
    let der_sig = "304502207fa7a6d1e0ee81132a269ad84e68d695483745cde8b541e3bf630749894e342a022100c1f7ab20e13e22fb95281a870f3dcf38d782e53023ee313d741ad0cfbc0c5090";
    let pubkey = "03b0da749730dc9b4b1f4a14d6902877a92541f5368778853d9c4a0cb7802dcfb2";
    for hashtype in ["00", "80", "01", "02", "03", "81", "82", "83"] {
        let mut s = push(&script(&format!("{der_sig}{hashtype}")));
        s.extend(push(&script(pubkey)));
        assert_eq!(script_to_asm_str(&s, false), format!("{der_sig}{hashtype} {pubkey}"));
    }
}

/// `script_GetScriptAsm`'s sighash-decoding cases: a strictly encoded signature prints its body
/// and the sighash name; hash types 0x00 and 0x80 fail `STRICTENC` and print raw.
// Core: src/test/script_tests.cpp:1247-1263
#[test]
fn core_get_script_asm_sighash_decode_vectors() {
    assert_eq!(script_to_asm_str(&[0xb1], true), "OP_CHECKLOCKTIMEVERIFY");
    let der_sig = "304502207fa7a6d1e0ee81132a269ad84e68d695483745cde8b541e3bf630749894e342a022100c1f7ab20e13e22fb95281a870f3dcf38d782e53023ee313d741ad0cfbc0c5090";
    let pubkey = "03b0da749730dc9b4b1f4a14d6902877a92541f5368778853d9c4a0cb7802dcfb2";
    for (hashtype, printed) in [
        ("00", "00"),
        ("80", "80"),
        ("01", "[ALL]"),
        ("02", "[NONE]"),
        ("03", "[SINGLE]"),
        ("81", "[ALL|ANYONECANPAY]"),
        ("82", "[NONE|ANYONECANPAY]"),
        ("83", "[SINGLE|ANYONECANPAY]"),
    ] {
        let mut s = push(&script(&format!("{der_sig}{hashtype}")));
        s.extend(push(&script(pubkey)));
        assert_eq!(script_to_asm_str(&s, true), format!("{der_sig}{printed} {pubkey}"));
    }
}

/// `IsUnspendable` turns the decode off for the whole script (an `OP_RETURN` first, or more than
/// `MAX_SCRIPT_SIZE` bytes), and a push that is not strict DER prints raw.
// Core: core_write.cpp:125-140
#[test]
fn sighash_decode_skips_unspendable_scripts_and_non_der_pushes() {
    let sig = script(
        "304502207fa7a6d1e0ee81132a269ad84e68d695483745cde8b541e3bf630749894e342a022100c1f7ab20e13e22fb95281a870f3dcf38d782e53023ee313d741ad0cfbc0c509001",
    );
    let body = hex(&sig[..sig.len() - 1]);
    assert_eq!(script_to_asm_str(&push(&sig), true), format!("{body}[ALL]"));

    let mut op_return = vec![OP_RETURN];
    op_return.extend(push(&sig));
    assert_eq!(script_to_asm_str(&op_return, true), format!("OP_RETURN {}", hex(&sig)));

    let mut long = push(&sig);
    while long.len() <= 10_000 {
        long.push(0x61);
    }
    assert!(script_to_asm_str(&long, true).starts_with(&format!("{} OP_NOP", hex(&sig))));
    long.truncate(10_000);
    assert!(script_to_asm_str(&long, true).starts_with(&format!("{body}[ALL] OP_NOP")));

    let mut bad_der = sig.clone();
    bad_der[0] = 0x31;
    assert_eq!(script_to_asm_str(&push(&bad_der), true), hex(&bad_der));
}

/// Pushes of at most 4 bytes print as `CScriptNum`s (sign bit, no minimality), longer ones as
/// hex; `OP_0` is "0", `OP_1NEGATE` "-1", small integers their number; an unknown opcode is
/// `OP_UNKNOWN`; a push that runs past the end ends with `[error]`.
#[test]
fn asm_prints_numbers_names_and_errors_as_core() {
    assert_eq!(script_to_asm_str(&[], false), "");
    assert_eq!(script_to_asm_str(&[0x00, 0x4f, 0x51, 0x60], false), "0 -1 1 16");
    assert_eq!(script_to_asm_str(&script("0181"), false), "-1");
    assert_eq!(script_to_asm_str(&script("0180"), false), "0");
    assert_eq!(script_to_asm_str(&script("02ff00"), false), "255");
    assert_eq!(script_to_asm_str(&script("04ffffffff"), false), "-2147483647");
    assert_eq!(script_to_asm_str(&script("04ffffff7f"), false), "2147483647");
    assert_eq!(script_to_asm_str(&script("050102030405"), false), "0102030405");
    assert_eq!(script_to_asm_str(&script("4c0101"), false), "1");
    assert_eq!(script_to_asm_str(&script("4d0100ff"), false), "-127");
    assert_eq!(script_to_asm_str(&script("4e0100000005"), false), "5");
    assert_eq!(script_to_asm_str(&script("babb"), false), "OP_CHECKDATASIG OP_CHECKDATASIGVERIFY");
    assert_eq!(
        script_to_asm_str(&[0x7e, 0x7f, 0xc0, 0xff], false),
        "OP_CAT OP_SPLIT OP_UNKNOWN OP_INVALIDOPCODE"
    );
    assert_eq!(script_to_asm_str(&script("76a902"), false), "OP_DUP OP_HASH160 [error]");
    assert_eq!(script_to_asm_str(&script("4c"), false), "[error]");
    assert_eq!(script_to_asm_str(&script("4d01"), false), "[error]");
}

/// `script_standard_Solver_success` / `_failure`'s shapes.
// Core: src/test/script_standard_tests.cpp:20-144
#[test]
fn solver_types_are_cores() {
    let key = script(G);
    let mut p2pk = push(&key);
    p2pk.push(0xac);
    assert_eq!(solver(&p2pk), TxoutType::Pubkey);
    let p2pkh = script("76a91400112233445566778899aabbccddeeff0011223388ac");
    assert_eq!(solver(&p2pkh), TxoutType::PubkeyHash);
    let p2sh = script("a91400112233445566778899aabbccddeeff0011223387");
    assert_eq!(solver(&p2sh), TxoutType::ScriptHash);
    let mut multi = vec![0x51];
    multi.extend(push(&key));
    multi.extend(push(&key));
    multi.extend([0x52, 0xae]);
    assert_eq!(solver(&multi), TxoutType::Multisig);
    assert_eq!(solver(&[0x6a]), TxoutType::NullData);
    assert_eq!(solver(&script("6a0400010203")), TxoutType::NullData);
    assert_eq!(solver(&script("6a0050")), TxoutType::NullData);

    let mut bad = push(&[1; 30]);
    bad.push(0xac);
    assert_eq!(solver(&bad), TxoutType::Nonstandard);
    let mut bad = vec![0x76, 0xa9];
    bad.extend(push(&key));
    bad.extend([0x88, 0xac]);
    assert_eq!(solver(&bad), TxoutType::Nonstandard);
    let mut bad = vec![0xa9];
    bad.extend(push(&[1; 21]));
    bad.push(0x87);
    assert_eq!(solver(&bad), TxoutType::Nonstandard);
    for (m, n) in [(0x00, 0x51), (0x52, 0x51), (0x51, 0x52)] {
        let mut bad = vec![m];
        bad.extend(push(&key));
        bad.extend([n, 0xae]);
        assert_eq!(solver(&bad), TxoutType::Nonstandard, "{m:02x} {n:02x}");
    }
    assert_eq!(solver(&[0x51, 0x51, 0xae]), TxoutType::Nonstandard);
    assert_eq!(solver(&script("6a014b93")), TxoutType::Nonstandard);
    assert_eq!(solver(&[]), TxoutType::Nonstandard);
}

/// `MatchMultisig` reads keys with `GetOp`, so an `OP_PUSHDATA1/2/4` key push counts, while its
/// counts stay minimal (`OP_N`, or a direct one-byte push of 17..20).
// Core: script/standard.cpp:95-113, :74-93
#[test]
fn multisig_keys_may_use_any_push_form() {
    let key = script(G);
    let mut pd1 = vec![0x51, 0x4c, 33];
    pd1.extend(&key);
    pd1.extend([0x51, 0xae]);
    assert_eq!(solver(&pd1), TxoutType::Multisig);
    let mut pd2 = vec![0x51, 0x4d, 33, 0];
    pd2.extend(&key);
    pd2.extend([0x51, 0xae]);
    assert_eq!(solver(&pd2), TxoutType::Multisig);
    let mut pd4 = vec![0x51, 0x4e, 33, 0, 0, 0];
    pd4.extend(&key);
    pd4.extend([0x51, 0xae]);
    assert_eq!(solver(&pd4), TxoutType::Multisig);
    let mut count_pd1 = vec![0x51];
    count_pd1.extend(push(&key));
    count_pd1.extend([0x4c, 1, 1, 0xae]);
    assert_eq!(solver(&count_pd1), TxoutType::Nonstandard);
    let mut truncated = vec![0x51, 0x4c, 34];
    truncated.extend(&key);
    truncated.extend([0x51, 0xae]);
    assert_eq!(solver(&truncated), TxoutType::Nonstandard);
}

/// `ScriptToUniv` with hex and address: P2PK has no `address`; a hybrid key is no `pk()`, and
/// its script is not its destination's, so `raw()`; without the flags only `asm` and `type`.
// Core: core_write.cpp:159-178
#[test]
fn script_to_univ_fields_are_cores() {
    let p = crate::descriptor::tests::mainnet();
    let desc = |d: &str| format!("{d}#{}", get_descriptor_checksum(d));
    let mut p2pk = push(&script(G));
    p2pk.push(0xac);
    assert_eq!(
        script_to_univ(&p2pk, true, true, &p).write(),
        format!(
            r#"{{"asm":"{G} OP_CHECKSIG","desc":"{}","hex":"21{G}ac","type":"pubkey"}}"#,
            desc(&format!("pk({G})"))
        )
    );

    let p2pkh = "76a91400112233445566778899aabbccddeeff0011223388ac";
    let address = encode_destination(&extract_destination(&script(p2pkh)).unwrap(), &p);
    assert_eq!(
        script_to_univ(&script(p2pkh), true, true, &p).write(),
        format!(
            r#"{{"asm":"OP_DUP OP_HASH160 00112233445566778899aabbccddeeff00112233 OP_EQUALVERIFY OP_CHECKSIG","desc":"{}","hex":"{p2pkh}","address":"{address}","type":"pubkeyhash"}}"#,
            desc(&format!("addr({address})"))
        )
    );

    let hybrid = format!("41{}{}ac", "06", "11".repeat(64));
    let out = script_to_univ(&script(&hybrid), true, true, &p).write();
    assert!(out.contains(&format!(r#""desc":"{}""#, desc(&format!("raw({hybrid})")))), "{out}");
    assert!(out.ends_with(r#""type":"pubkey"}"#), "{out}");

    assert_eq!(
        script_to_univ(&[0x6a], false, false, &p).write(),
        r#"{"asm":"OP_RETURN","type":"nulldata"}"#
    );
}

/// Core's dash-tx goldens: `TxToUniv(tx, uint256(), entry)` (hex included, no undo) under
/// mainnet's parameters, for every `*.json` the util test compares; each golden's own `hex` is
/// the transaction.
// Core: test/util/data/bitcoin-util-test.json, bitcoin-tx.cpp OutputTxJSON
#[test]
fn core_dash_tx_json_goldens() {
    let p = TxJsonParams {
        key_io: crate::descriptor::tests::mainnet(),
        service: false,
    };
    let names = [
        "blanktxv1",
        "blanktxv2",
        "tt-delin1-out",
        "tt-delout1-out",
        "tt-locktime317000-out",
        "txcreate1",
        "txcreate2",
        "txcreatescript1",
        "txcreatescript2",
        "txcreatesignv1",
        "txcreateoutpubkey1",
        "txcreatedata1",
        "txcreatedata2",
        "txcreatedata_seq0",
        "txcreatedata_seq1",
        "txcreatemultisig1",
        "txcreatemultisig2",
    ];
    for name in names {
        let text = dash_core_vectors::read_core_text(&format!("test/util/data/{name}.json"));
        let golden = UniValue::read(text.as_bytes()).expect("Core's JSON");
        let UniValue::Obj(fields) = &golden else {
            panic!("{name}: an object")
        };
        let hex =
            fields.iter().find(|(k, _)| k == "hex").expect("a hex field").1.get_str().unwrap();
        let tx = crate::mining::decode_hex_tx(hex).expect("Core's transaction");
        let got = tx_to_univ(&tx, None, true, None, TxVerbosity::Details, &p);
        assert_eq!(got.write(), golden.write(), "{name}");
    }
}

/// dashd v24.0.0-rc.4's `decoderawtransaction` (`TxToUniv` without hex) for every special
/// transaction type and payload version Core's chains carry, under each network's parameters and
/// with and without `-deprecatedrpc=service` (tests/data/txtouniv/PROVENANCE.md).
// Core: rpc/rawtransaction.cpp:956
#[test]
fn dashd_decoderawtransaction_fixtures() {
    let (rows, transfers) = check_decoderawtransaction_fixture("decoderawtransaction-v24.0.0-rc.4");
    assert_eq!(rows, 221);
    assert!(transfers > 0);
}

/// The same dashd's replies for crafted transactions: payload shapes no chain carries (an invalid
/// network entry, an empty ProUpShareTx reward script, a fractional operator reward), each a
/// chain transaction with one field edited (PROVENANCE.md, "Crafted transactions").
#[test]
fn dashd_decoderawtransaction_crafted_fixtures() {
    let (rows, _) = check_decoderawtransaction_fixture("decoderawtransaction-crafted-v24.0.0-rc.4");
    assert_eq!(rows, 5);
}

/// Compares every row of `tests/data/txtouniv/<name>.jsonl.gz` and returns how many rows, and
/// how many Platform transfers, it checked.
fn check_decoderawtransaction_fixture(name: &str) -> (usize, usize) {
    use std::io::BufRead as _;
    let path = format!("{}/tests/data/txtouniv/{name}.jsonl.gz", env!("CARGO_MANIFEST_DIR"));
    let file = std::fs::File::open(path).expect("the fixture");
    let (mut rows, mut transfers) = (0, 0);
    for line in std::io::BufReader::new(flate2::read::GzDecoder::new(file)).lines() {
        let row = UniValue::read(line.unwrap().as_bytes()).expect("a JSON line");
        let field = |name: &str| match &row {
            UniValue::Obj(fields) => fields.iter().find(|(k, _)| k == name).unwrap().1.clone(),
            _ => panic!("an object"),
        };
        let key_io = match field("net").get_str().unwrap() {
            "main" => crate::descriptor::tests::mainnet(),
            _ => crate::descriptor::tests::regtest(),
        };
        let p = TxJsonParams {
            key_io,
            service: field("service").get_bool().unwrap(),
        };
        let reply = UniValue::read(field("reply").get_str().unwrap().as_bytes()).unwrap();
        let UniValue::Obj(reply) = reply else {
            panic!("a reply object")
        };
        let want = find_value(&reply, "result");
        let UniValue::Obj(reply_result) = want else {
            panic!("a result object")
        };
        let hex = field("hex").get_str().unwrap().to_owned();
        let tx = crate::mining::decode_hex_tx(&hex).expect("dashd's transaction");
        let got = tx_to_univ(&tx, None, false, None, TxVerbosity::Details, &p);
        assert_eq!(got.write(), want.write(), "{} {}", field("bucket").write(), hex);
        rows += 1;
        // A Platform transfer (an asset unlock, which has no inputs) pays its payload's fee,
        // not inputs less outputs: with (empty) undo data, `fee` is `assetUnlockTx.fee`.
        if tx.is_platform_transfer() {
            let undo = TxUndo {
                coins: vec![],
            };
            let got = tx_to_univ(&tx, None, false, Some(&undo), TxVerbosity::Details, &p);
            let UniValue::Obj(unlock) = find_value(reply_result, "assetUnlockTx") else {
                panic!("an assetUnlockTx object")
            };
            let fee = get_int_i64(find_value(unlock, "fee")).unwrap();
            let UniValue::Obj(got) = got else {
                panic!("an object")
            };
            assert_eq!(find_value(&got, "fee").write(), amount(fee).write(), "{hex}");
            transfers += 1;
        }
    }
    (rows, transfers)
}
