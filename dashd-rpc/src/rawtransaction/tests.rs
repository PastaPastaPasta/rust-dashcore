//! Unit checks of the raw-transaction handlers; the DIFF against dashd is in
//! rdashd/tests/rpc_diff.rs.

use super::*;
use crate::protocol::RPC_MISC_ERROR;

fn num(text: &str) -> UniValue {
    UniValue::from_num_str(text).expect("a number")
}

fn args(json: &str) -> UniValue {
    UniValue::read(json.as_bytes()).expect("JSON")
}

fn code_and_message(e: RpcError) -> (i32, String) {
    let code = e.code();
    let message = match e {
        RpcError::Json {
            message,
            ..
        }
        | RpcError::Type(message)
        | RpcError::Runtime(message) => message,
    };
    (code, message)
}

fn regtest() -> KeyIoParams {
    crate::descriptor::tests::regtest()
}

const TXID: &str = "0101010101010101010101010101010101010101010101010101010101010101";

/// Core's `rpc_parse_monetary_values` cases, plus the sign, exponent and type edges.
// Core: test/rpc_tests.cpp:262-296
#[test]
fn amount_from_value_follows_core() {
    let ok = [
        ("0", 0),
        ("0.00000000", 0),
        ("0.00000001", 1),
        ("0.17622195", 17_622_195),
        ("0.5", 50_000_000),
        ("0.50000000", 50_000_000),
        ("0.89898989", 89_898_989),
        ("1.00000000", 100_000_000),
        ("20999999.9999999", 2_099_999_999_999_990),
        ("20999999.99999999", 2_099_999_999_999_999),
        ("1e-8", 1),
        ("0.1e-7", 1),
        ("0.01e-6", 1),
        ("0.0000000000000000000000000000000000000000000000000000000000000000000000000001e+68", 1),
        ("10000000000000000000000000000000000000000000000000000000000000000e-64", 100_000_000),
        (
            "0.000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000e64",
            100_000_000,
        ),
        ("0.00000001000000", 1),
        ("0.19e-6", 19),
        ("-0", 0),
        ("21000000", 2_100_000_000_000_000),
    ];
    for (text, want) in ok {
        assert_eq!(amount_from_value(&num(text)).unwrap(), want, "{text}");
        assert_eq!(amount_from_value(&UniValue::Str(text.into())).unwrap(), want, "\"{text}\"");
    }
    let invalid = [
        "1e-9",
        "0.000000019",
        "19e-9",
        ".19e-6",
        "92233720368.54775808",
        "1e+11",
        "1e11",
        "93e+9",
        "99999999999",
        "01",
        "1.",
        "",
        " 1",
        "1 ",
        "0x10",
        "1e",
        "--1",
    ];
    for text in invalid {
        let e = code_and_message(amount_from_value(&UniValue::Str(text.into())).unwrap_err());
        assert_eq!(e, (RPC_TYPE_ERROR, "Invalid amount".into()), "{text:?}");
    }
    for text in ["-0.00000001", "21000000.00000001", "9999999999"] {
        let e = code_and_message(amount_from_value(&num(text)).unwrap_err());
        assert_eq!(e, (RPC_TYPE_ERROR, "Amount out of range".into()), "{text}");
    }
    for v in [UniValue::Null, true.into(), UniValue::new_array(), UniValue::new_object()] {
        let e = code_and_message(amount_from_value(&v).unwrap_err());
        assert_eq!(e, (RPC_TYPE_ERROR, "Amount is not a number or string".into()));
    }
}

fn create(json: &str) -> Result<Transaction, (i32, String)> {
    let hex = createrawtransaction(&regtest(), &args(json)).map_err(code_and_message)?;
    let tx = crate::mining::decode_hex_tx(hex.get_str().unwrap()).expect("decodes");
    Ok(tx.raw().clone())
}

/// `CMutableTransaction`'s defaults, the lock-time-selected sequence, an explicit sequence only
/// when it is a number, and the outputs in key order.
// Core: rpc/rawtransaction_util.cpp:24-143
#[test]
fn createrawtransaction_builds_as_core() {
    let address = encode_destination(&Destination::PkHash([9; 20]), &regtest());
    let tx = create(&format!(
        r#"[[{{"txid":"{TXID}","vout":1}}], [{{"data":"00ff"}}, {{"{address}":"0.5"}}]]"#
    ))
    .unwrap();
    assert_eq!((tx.version, tx.tx_type, tx.lock_time), (2, TxType::Spend, 0));
    assert!(tx.extra_payload.is_empty());
    assert_eq!(tx.inputs[0].sequence, SEQUENCE_FINAL);
    assert_eq!(tx.inputs[0].prevout.index, 1);
    assert_eq!(tx.outputs[0].value, 0);
    assert_eq!(tx.outputs[0].script_pubkey.as_bytes(), [OP_RETURN, 2, 0x00, 0xff]);
    assert_eq!(tx.outputs[1].value, 50_000_000);
    assert_eq!(tx.outputs[1].script_pubkey.as_bytes(), Destination::PkHash([9; 20]).script());

    for (sequence, lock, want) in [
        ("null", 0, SEQUENCE_FINAL),
        ("\"5\"", 0, SEQUENCE_FINAL),
        ("true", 1, MAX_SEQUENCE_NONFINAL),
        ("[]", 1, MAX_SEQUENCE_NONFINAL),
        ("{}", 0, SEQUENCE_FINAL),
        ("0", 1, 0),
        ("4294967295", 1, SEQUENCE_FINAL),
    ] {
        let tx = create(&format!(
            r#"[[{{"txid":"{TXID}","vout":0,"sequence":{sequence}}}], {{}}, {lock}]"#
        ))
        .unwrap();
        assert_eq!((tx.inputs[0].sequence, tx.lock_time), (want, lock), "{sequence}");
    }
    // A numeric `data` is its text.
    let tx = create(r#"[[], {"data":12}]"#).unwrap();
    assert_eq!(tx.outputs[0].script_pubkey.as_bytes(), [OP_RETURN, 1, 0x12]);
    assert!(create("[[], []]").unwrap().outputs.is_empty());
}

/// Every refusal in `ConstructTransaction`, with Core's code and text.
// Core: rpc/rawtransaction_util.cpp:24-143
#[test]
fn createrawtransaction_refuses_as_core() {
    let address = encode_destination(&Destination::PkHash([9; 20]), &regtest());
    let input = |extra: &str| format!(r#"[{{"txid":"{TXID}"{extra}}}]"#);
    let param = |e: &str| (RPC_INVALID_PARAMETER, format!("Invalid parameter, {e}"));
    let range = (RPC_MISC_ERROR, "JSON integer out of range".to_owned());
    let cases: Vec<(String, (i32, String))> = vec![
        ("[[], {}, -1]".into(), param("locktime out of range")),
        ("[[], {}, 4294967296]".into(), param("locktime out of range")),
        ("[[], {}, 1.5]".into(), range.clone()),
        ("[[], {}, 1e0]".into(), range.clone()),
        (
            "[[5], {}]".into(),
            (RPC_TYPE_ERROR, "JSON value of type number is not of expected type object".into()),
        ),
        (format!("[{}, {{}}]", input("")), param("missing vout key")),
        (format!("[{}, {{}}]", input(r#","vout":"0""#)), param("missing vout key")),
        (format!("[{}, {{}}]", input(r#","vout":-1"#)), param("vout cannot be negative")),
        (format!("[{}, {{}}]", input(r#","vout":2147483648"#)), range.clone()),
        (format!("[{}, {{}}]", input(r#","vout":0.5"#)), range.clone()),
        (
            format!("[{}, {{}}]", input(r#","vout":0,"sequence":-1"#)),
            param("sequence number is out of range"),
        ),
        (
            format!("[{}, {{}}]", input(r#","vout":0,"sequence":4294967296"#)),
            param("sequence number is out of range"),
        ),
        (
            format!("[{}, {{}}]", input(r#","vout":0,"sequence":9223372036854775808"#)),
            range.clone(),
        ),
        (format!("[{}, {{}}]", input(r#","vout":0,"sequence":1e0"#)), range),
        (
            r#"[[{"vout":0}], {}]"#.into(),
            (RPC_TYPE_ERROR, "JSON value of type null is not of expected type string".into()),
        ),
        (
            r#"[[{"txid":"00","vout":0}], {}]"#.into(),
            (RPC_INVALID_PARAMETER, "txid must be of length 64 (not 2, for '00')".into()),
        ),
        ("[[], null]".into(), param("output argument must be non-null")),
        (
            "[[], 5]".into(),
            (RPC_TYPE_ERROR, "JSON value of type number is not of expected type array".into()),
        ),
        ("[[], [5]]".into(), param("key-value pair not an object as expected")),
        ("[[], [{}]]".into(), param("key-value pair must contain exactly one key")),
        (
            r#"[[], [{"data":"00","data":"01"}]]"#.into(),
            param("key-value pair must contain exactly one key"),
        ),
        (r#"[[], [{"data":"00"}, {"data":"01"}]]"#.into(), param("duplicate key: data")),
        (r#"[[], {"data":"00","data":"01"}]"#.into(), param("duplicate key: data")),
        (
            r#"[[], {"data":""}]"#.into(),
            (RPC_INVALID_PARAMETER, "Data must be hexadecimal string (not '')".into()),
        ),
        (
            r#"[[], {"data":null}]"#.into(),
            (RPC_INVALID_PARAMETER, "Data must be hexadecimal string (not '')".into()),
        ),
        (
            r#"[[], {"data":true}]"#.into(),
            (RPC_INVALID_PARAMETER, "Data must be hexadecimal string (not '1')".into()),
        ),
        (
            r#"[[], {"data":"0g"}]"#.into(),
            (RPC_INVALID_PARAMETER, "Data must be hexadecimal string (not '0g')".into()),
        ),
        (
            r#"[[], {"nope":1}]"#.into(),
            (RPC_INVALID_ADDRESS_OR_KEY, "Invalid Dash address: nope".into()),
        ),
        (
            format!(r#"[[], {{"{address}":1,"{address}":2}}]"#),
            param(&format!("duplicated address: {address}")),
        ),
        (
            format!(r#"[[], {{"{address}":null}}]"#),
            (RPC_TYPE_ERROR, "Amount is not a number or string".into()),
        ),
    ];
    for (json, want) in cases {
        assert_eq!(create(&json).unwrap_err(), want, "{json}");
    }
}

/// `decodescript`: no `address` for P2PK, no `p2sh` for P2SH, the empty script allowed, and
/// `ParseHexV`'s "argument" text.
// Core: rpc/rawtransaction.cpp:986-1006
#[test]
fn decodescript_follows_core() {
    let p = regtest();
    let decode = |hex: &str| decodescript(&p, &args(&format!(r#"["{hex}"]"#)));
    let keys = |v: &UniValue| match v {
        UniValue::Obj(fields) => fields.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
        _ => panic!("an object"),
    };
    let p2pk = format!("21{}ac", hex(&[2; 33]));
    assert_eq!(keys(&decode(&p2pk).unwrap()), ["asm", "desc", "type", "p2sh"]);
    let p2pkh = hex(&Destination::PkHash([3; 20]).script());
    assert_eq!(keys(&decode(&p2pkh).unwrap()), ["asm", "desc", "address", "type", "p2sh"]);
    let p2sh = hex(&Destination::ScriptHash([4; 20]).script());
    assert_eq!(keys(&decode(&p2sh).unwrap()), ["asm", "desc", "address", "type"]);
    let empty = decode("").unwrap();
    assert_eq!(keys(&empty), ["asm", "desc", "type", "p2sh"]);
    assert_eq!(
        code_and_message(decode("0").unwrap_err()),
        (RPC_INVALID_PARAMETER, "argument must be hexadecimal string (not '0')".into())
    );
}
