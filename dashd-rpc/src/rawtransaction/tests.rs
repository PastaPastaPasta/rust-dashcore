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
    assert_eq!((tx.version(), tx.tx_type(), tx.lock_time()), (2, TxType::Spend, 0));
    assert!(tx.extra_payload().is_empty());
    assert_eq!(tx.inputs()[0].sequence(), SEQUENCE_FINAL);
    assert_eq!(tx.inputs()[0].prevout().index(), 1);
    assert_eq!(tx.outputs()[0].value(), 0);
    assert_eq!(tx.outputs()[0].script_pubkey().as_bytes(), [OP_RETURN, 2, 0x00, 0xff]);
    assert_eq!(tx.outputs()[1].value(), 50_000_000);
    assert_eq!(tx.outputs()[1].script_pubkey().as_bytes(), Destination::PkHash([9; 20]).script());

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
        assert_eq!((tx.inputs()[0].sequence(), tx.lock_time()), (want, lock), "{sequence}");
    }
    // A numeric `data` is its text.
    let tx = create(r#"[[], {"data":12}]"#).unwrap();
    assert_eq!(tx.outputs()[0].script_pubkey().as_bytes(), [OP_RETURN, 1, 0x12]);
    assert!(create("[[], []]").unwrap().outputs().is_empty());
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

mod signing {
    //! `combinerawtransaction` and `signrawtransactionwithkey`. The handler tests call the
    //! handlers directly, past the server's argument checks (handler-only); the
    //! `preflight_*` tests go through the server. The DIFF is in rdashd/tests/rpc_diff.rs.

    use std::sync::atomic::{AtomicUsize, Ordering};

    use dash_consensus::script::opcodes::{OP_0, OP_1, OP_CHECKMULTISIG, OP_CHECKSIG};
    use dash_consensus::script::signature_hash;
    use dash_primitives::BlockHash;

    use super::*;
    use crate::blockchain::{
        Aborted, ActiveWindow, BestChainLock, ChainState, ChainTxError, ChainTxPoint,
        DeploymentError, DeploymentReport, HeaderFacts, PublishedChain, TipInfo, UnspentCoin,
    };
    use crate::key_io::{Key, SecretBytes, encode_base58_check};
    use crate::protocol::{RPC_INTERNAL_ERROR, RPC_VERIFY_ERROR};
    use crate::request::JsonRpcRequest;
    use crate::testutil::FakeControl;

    /// The coins tip at each V turn: a read takes the next turn, past the last the last tip.
    #[derive(Default)]
    struct FakeCoins {
        tips: Vec<BTreeMap<OutPoint, Coin>>,
        turns: AtomicUsize,
    }

    impl FakeCoins {
        fn at(tips: Vec<BTreeMap<OutPoint, Coin>>) -> Arc<FakeCoins> {
            Arc::new(FakeCoins {
                tips,
                turns: AtomicUsize::new(0),
            })
        }

        fn turn(&self) -> &BTreeMap<OutPoint, Coin> {
            let t = self.turns.fetch_add(1, Ordering::SeqCst);
            &self.tips[t.min(self.tips.len() - 1)]
        }
    }

    impl ChainState for Arc<FakeCoins> {
        fn tip_info(&self) -> Result<TipInfo, Aborted> {
            unreachable!()
        }
        fn size_on_disk(&self) -> u64 {
            unreachable!()
        }
        fn deployment_info(
            &self,
            _: Option<&BlockHash>,
        ) -> Result<Option<DeploymentReport>, DeploymentError> {
            unreachable!()
        }
        fn chain_tx_final(&self, _: Option<&BlockHash>) -> Result<ChainTxPoint, ChainTxError> {
            unreachable!()
        }
        fn chain_tx_ancestor(
            &self,
            _: &BlockHash,
            _: u32,
        ) -> Result<Option<ChainTxPoint>, Aborted> {
            unreachable!()
        }
        /// One turn per call, as `Writer::txout`.
        fn txout(&self, out: &OutPoint) -> Result<Option<UnspentCoin>, Aborted> {
            Ok(self.turn().get(out).map(|coin| UnspentCoin {
                coin: coin.clone(),
                best_block: BlockHash::from([0; 32]),
                best_height: 0,
            }))
        }
        /// One turn for them all.
        fn txouts(&self, outs: &[OutPoint]) -> Result<Vec<Option<Coin>>, Aborted> {
            let tip = self.turn();
            Ok(outs.iter().map(|out| tip.get(out).cloned()).collect())
        }
        fn best_chainlock(&self) -> Option<BestChainLock> {
            unreachable!()
        }
        fn has_block(&self, _: &BlockHash) -> bool {
            unreachable!()
        }
        fn headers_from(&self, _: &BlockHash, _: usize) -> Result<Vec<HeaderFacts>, Aborted> {
            unreachable!()
        }
        fn active_window(&self, _: Option<u32>, _: u32) -> Result<ActiveWindow, Aborted> {
            unreachable!()
        }
    }

    /// The node: its coins, or none before the chainstate loads.
    struct FakeNode(Option<Arc<FakeCoins>>);

    impl NodeChainState for FakeNode {
        fn chain_state(&self) -> Option<Box<dyn ChainState>> {
            Some(Box::new(Arc::clone(self.0.as_ref()?)))
        }
    }

    /// No published chain (the decode commands are not under test here).
    struct NoChain;

    impl NodeChain for NoChain {
        fn published(&self) -> Option<Box<dyn PublishedChain>> {
            None
        }
        fn chainman(&self) -> Option<Box<dyn crate::blockchain::Chainman>> {
            None
        }
    }

    fn key(n: u8, compressed: bool) -> Key {
        Key {
            secret: SecretBytes::new(&[n; 32]).expect("a valid secret"),
            compressed,
        }
    }

    /// Key `n`'s regtest WIF.
    fn wif(n: u8, compressed: bool) -> String {
        let mut data = vec![regtest().secret_key];
        data.extend([n; 32]);
        if compressed {
            data.push(1);
        }
        encode_base58_check(&data)
    }

    fn pubkey(n: u8) -> Vec<u8> {
        key(n, true).pubkey()
    }

    fn push(data: &[u8]) -> Vec<u8> {
        let mut s = Vec::new();
        push_data(&mut s, data);
        s
    }

    fn p2pkh(n: u8) -> Vec<u8> {
        Destination::PkHash(hash160(&pubkey(n))).script()
    }

    fn multisig(required: u8, keys: &[u8]) -> Vec<u8> {
        let mut s = vec![OP_1 + required - 1];
        for &k in keys {
            push_data(&mut s, &pubkey(k));
        }
        s.extend([OP_1 + keys.len() as u8 - 1, OP_CHECKMULTISIG]);
        s
    }

    fn outpoint(n: u8) -> OutPoint {
        OutPoint::new(TxHash::from([n; 32]), u32::from(n))
    }

    fn unsigned(prevouts: &[OutPoint], outputs: &[i64]) -> Transaction {
        Transaction::new(
            1,
            TxType::Spend,
            prevouts
                .iter()
                .map(|&prevout| TxIn::new(prevout, Vec::new().into(), u32::MAX))
                .collect(),
            outputs.iter().map(|&value| TxOut::from_duffs(value, p2pkh(9).into())).collect(),
            0,
            Vec::new(),
        )
    }

    fn tx_hex(tx: &Transaction) -> String {
        hex(&tx.core_serialize())
    }

    fn decode(hex: &str) -> Transaction {
        decode_hex_tx(hex).expect("decodes").into_raw()
    }

    fn with_script_sigs(tx: &Transaction, script_sigs: &[&[u8]]) -> Transaction {
        let mut inputs = tx.inputs().clone();
        for (txin, s) in inputs.iter_mut().zip(script_sigs) {
            *txin = TxIn::new(txin.prevout(), s.to_vec().into(), txin.sequence());
        }
        Transaction::new(
            tx.version(),
            tx.tx_type(),
            inputs,
            tx.outputs().clone(),
            tx.lock_time(),
            tx.extra_payload().clone(),
        )
    }

    /// Key `k`'s signature of input `n_in` of `tx` under `hash_type`, its byte appended.
    fn sig(k: u8, script_code: &[u8], tx: &Transaction, n_in: usize, hash_type: i32) -> Vec<u8> {
        let mut s =
            crate::sign::sign(&key(k, true), &signature_hash(script_code, tx, n_in, hash_type));
        s.push(hash_type as u8);
        s
    }

    /// The chain's coins: A pays key 1 (P2PKH), B a bare 2-of-3 of keys 1-3, D key 2 (P2PK),
    /// E key 4's uncompressed P2PKH.
    fn chain_coins() -> BTreeMap<OutPoint, Coin> {
        let p2pk = [push(&pubkey(2)), vec![OP_CHECKSIG]].concat();
        let uncompressed = Destination::PkHash(hash160(&key(4, false).pubkey())).script();
        [
            (outpoint(0xa), p2pkh(1)),
            (outpoint(0xb), multisig(2, &[1, 2, 3])),
            (outpoint(0xd), p2pk),
            (outpoint(0xe), uncompressed),
        ]
        .into_iter()
        .map(|(out, script)| (out, Coin::new(5_000, &script, 10, false)))
        .collect()
    }

    fn node() -> FakeNode {
        FakeNode(Some(FakeCoins::at(vec![chain_coins()])))
    }

    fn combine(node: &FakeNode, txs: &[String]) -> Result<String, (i32, String)> {
        let quoted: Vec<String> = txs.iter().map(|t| format!("\"{t}\"")).collect();
        combine_json(node, &format!("[[{}]]", quoted.join(",")))
    }

    fn combine_json(node: &FakeNode, json: &str) -> Result<String, (i32, String)> {
        combinerawtransaction(node, &args(json))
            .map(|v| v.get_str().unwrap().to_owned())
            .map_err(code_and_message)
    }

    fn sign_json(node: &FakeNode, json: &str) -> Result<UniValue, (i32, String)> {
        signrawtransactionwithkey(node, &regtest(), &args(json)).map_err(code_and_message)
    }

    fn signed(node: &FakeNode, tx: &Transaction, keys: &[String], rest: &str) -> UniValue {
        let keys: Vec<String> = keys.iter().map(|k| format!("\"{k}\"")).collect();
        let json = format!(r#"["{}", [{}]{rest}]"#, tx_hex(tx), keys.join(","));
        sign_json(node, &json).unwrap_or_else(|e| panic!("{json}: {e:?}"))
    }

    fn field<'a>(o: &'a UniValue, key: &str) -> &'a UniValue {
        match o {
            UniValue::Obj(members) => find_value(members, key),
            _ => panic!("an object"),
        }
    }

    fn result_tx(result: &UniValue) -> Transaction {
        decode(field(result, "hex").get_str().unwrap())
    }

    /// Signing P2PKH (compressed and uncompressed keys), P2PK and a bare 2-of-3 partly: each
    /// signature over the transaction, the multisig left padded, the result object in Core's
    /// order with the error entry's fields.
    // Core: rpc/rawtransaction.cpp:1097-1190; rpc/rawtransaction_util.cpp:144-153, 253-273
    #[test]
    fn signrawtransactionwithkey_signs_as_core() {
        let tx = unsigned(&[outpoint(0xa), outpoint(0xb), outpoint(0xd), outpoint(0xe)], &[4_000]);
        let result = signed(&node(), &tx, &[wif(1, true), wif(2, true), wif(4, false)], "");
        let s = result_tx(&result);
        let a = sig(1, &p2pkh(1), &tx, 0, SIGHASH_ALL);
        assert_eq!(s.inputs()[0].script_sig().as_bytes(), [push(&a), push(&pubkey(1))].concat());
        let ms = multisig(2, &[1, 2, 3]);
        let b = [vec![OP_0], push(&sig(1, &ms, &tx, 1, 1)), push(&sig(2, &ms, &tx, 1, 1))].concat();
        assert_eq!(s.inputs()[1].script_sig().as_bytes(), b);
        let p2pk = [push(&pubkey(2)), vec![OP_CHECKSIG]].concat();
        assert_eq!(s.inputs()[2].script_sig().as_bytes(), push(&sig(2, &p2pk, &tx, 2, 1)));
        let uncompressed = key(4, false);
        let e_spk = Destination::PkHash(hash160(&uncompressed.pubkey())).script();
        let mut e = crate::sign::sign(&uncompressed, &signature_hash(&e_spk, &tx, 3, 1));
        e.push(1);
        assert_eq!(
            s.inputs()[3].script_sig().as_bytes(),
            [push(&e), push(&uncompressed.pubkey())].concat()
        );
        assert_eq!(result.write(), format!(r#"{{"hex":"{}","complete":true}}"#, tx_hex(&s)));

        // Key 1 alone: the multisig half signed, the uncompressed coin unsigned.
        let result = signed(&node(), &tx, &[wif(1, true), wif(4, true)], "");
        let s = result_tx(&result);
        let half = [vec![OP_0], push(&sig(1, &ms, &tx, 1, 1)), vec![OP_0]].concat();
        assert_eq!(s.inputs()[1].script_sig().as_bytes(), half);
        let txid = TxHash::from([0xb; 32]).to_string();
        let txid_e = TxHash::from([0xe; 32]).to_string();
        assert_eq!(
            field(&result, "errors").write(),
            format!(
                r#"[{{"txid":"{txid}","vout":11,"scriptSig":"{}","sequence":4294967295,"error":"CHECK(MULTI)SIG failing with non-zero signature (possibly need more signatures)"}},{{"txid":"{}","vout":13,"scriptSig":"{}","sequence":4294967295,"error":"Unable to sign input, invalid stack size (possibly missing key)"}},{{"txid":"{txid_e}","vout":14,"scriptSig":"","sequence":4294967295,"error":"Unable to sign input, invalid stack size (possibly missing key)"}}]"#,
                hex(&half),
                TxHash::from([0xd; 32]),
                ""
            )
        );
        assert_eq!(field(&result, "complete").write(), "false");

        // Every sighash name, its byte on the signature; null is ALL.
        for (name, byte) in [
            ("null", 1),
            ("\"ALL\"", 1),
            ("\"NONE\"", 2),
            ("\"SINGLE\"", 3),
            ("\"ALL|ANYONECANPAY\"", 0x81),
            ("\"NONE|ANYONECANPAY\"", 0x82),
            ("\"SINGLE|ANYONECANPAY\"", 0x83),
        ] {
            let one = unsigned(&[outpoint(0xa)], &[1]);
            let result = signed(&node(), &one, &[wif(1, true)], &format!(", null, {name}"));
            let want = [push(&sig(1, &p2pkh(1), &one, 0, byte)), push(&pubkey(1))].concat();
            assert_eq!(result_tx(&result).inputs()[0].script_sig().as_bytes(), want, "{name}");
        }
        // SINGLE without its output: nothing signed.
        let no_out = unsigned(&[outpoint(0xa)], &[]);
        let result = signed(&node(), &no_out, &[wif(1, true)], r#", null, "SINGLE""#);
        assert!(result_tx(&result).inputs()[0].script_sig().as_bytes().is_empty());
        assert_eq!(field(&result, "complete").write(), "false");
    }

    /// prevtxs: an unknown coin given (amount or none), a P2SH coin with its redeem script, a
    /// known coin restated with its own script, and the 547-byte redeem script `AddCScript`
    /// refuses, which leaves its input unsigned without an error (Sol r1 M4).
    // Core: rpc/rawtransaction_util.cpp:156-240; script/signingprovider.cpp:111-120
    #[test]
    fn prevtxs_add_coins_and_scripts_as_core() {
        let redeem = multisig(2, &[1, 2, 3]);
        let p2sh = Destination::ScriptHash(hash160(&redeem)).script();
        let unknown = outpoint(0x5);
        let tx = unsigned(&[unknown, outpoint(0xa)], &[1]);
        let prev = format!(
            r#", [{{"txid":"{}","vout":5,"scriptPubKey":"{}","redeemScript":"{}","amount":1}},
                {{"txid":"{}","vout":10,"scriptPubKey":"{}"}}]"#,
            unknown.txid(),
            hex(&p2sh),
            hex(&redeem),
            outpoint(0xa).txid(),
            hex(&p2pkh(1))
        );
        let result = signed(&node(), &tx, &[wif(1, true), wif(2, true)], &prev);
        assert_eq!(field(&result, "complete").write(), "true");
        let s = result_tx(&result);
        let want = [
            vec![OP_0],
            push(&sig(1, &redeem, &tx, 0, 1)),
            push(&sig(2, &redeem, &tx, 0, 1)),
            push(&redeem),
        ]
        .concat();
        assert_eq!(s.inputs()[0].script_sig().as_bytes(), want);

        let mut big = vec![OP_1];
        for k in 1..=16 {
            push_data(&mut big, &pubkey(k));
        }
        big.extend([OP_1 + 15, OP_CHECKMULTISIG]);
        assert_eq!(big.len(), 547);
        let big_p2sh = Destination::ScriptHash(hash160(&big)).script();
        let tx = unsigned(&[unknown], &[1]);
        let prev = |spk: &[u8]| {
            format!(
                r#", [{{"txid":"{}","vout":5,"scriptPubKey":"{}","redeemScript":"{}"}}]"#,
                unknown.txid(),
                hex(spk),
                hex(&big)
            )
        };
        let result = signed(&node(), &tx, &[wif(1, true)], &prev(&big_p2sh));
        assert!(result_tx(&result).inputs()[0].script_sig().as_bytes().is_empty());
        assert_eq!(
            field(&field(&result, "errors").get_values().unwrap()[0].clone(), "error")
                .get_str()
                .unwrap(),
            "Unable to sign input, invalid stack size (possibly missing key)"
        );
        let json = format!(r#"["{}", ["{}"]{}]"#, tx_hex(&tx), wif(1, true), prev(&p2sh));
        assert_eq!(
            sign_json(&node(), &json).unwrap_err(),
            (RPC_INVALID_PARAMETER, "redeemScript does not match scriptPubKey".into())
        );
    }

    /// Every handler refusal of `signrawtransactionwithkey`, in Core's order: the hex, the keys
    /// (before the chainstate), each prevtx's fields in `std::map` order (scriptPubKey, txid,
    /// vout), then the sighash name. Handler-only: the server's type checks come first.
    // Core: rpc/rawtransaction.cpp:1163-1186; rpc/rawtransaction_util.cpp:156-240; rpc/util.cpp:41-58, 307-317
    #[test]
    fn signrawtransactionwithkey_refuses_as_core() {
        let tx = tx_hex(&unsigned(&[outpoint(0xa)], &[1]));
        let k = wif(1, true);
        let a = outpoint(0xa).txid().to_string();
        let spk = hex(&p2pkh(1));
        let redeem = hex(&multisig(1, &[1]));
        let p2sh = hex(&Destination::ScriptHash(hash160(&multisig(1, &[1]))).script());
        let type_error = |m: &str| (RPC_TYPE_ERROR, m.to_owned());
        let prev = |o: &str| format!(r#"["{tx}", ["{k}"], [{o}]]"#);
        let mismatch = format!(
            "Previous output scriptPubKey mismatch:\n{}\nvs:\n1",
            crate::core_write::script_to_asm_str(&p2pkh(1), false)
        );
        let cases: Vec<(String, (i32, String))> = vec![
            (
                r#"["zz", ["x"]]"#.into(),
                (
                    RPC_DESERIALIZATION_ERROR,
                    "TX decode failed. Make sure the tx has at least one input.".into(),
                ),
            ),
            (
                format!(r#"["{tx}", ["{k}", "x"]]"#),
                (RPC_INVALID_ADDRESS_OR_KEY, "Invalid private key".into()),
            ),
            (
                format!(r#"["{tx}", [5]]"#),
                type_error("JSON value of type number is not of expected type string"),
            ),
            (
                prev("5"),
                (
                    RPC_DESERIALIZATION_ERROR,
                    r#"expected object with {"txid'","vout","scriptPubKey"}"#.into(),
                ),
            ),
            (prev("{}"), type_error("Missing scriptPubKey")),
            (prev(r#"{"scriptPubKey":null,"txid":5}"#), type_error("Missing scriptPubKey")),
            (
                prev(r#"{"scriptPubKey":5}"#),
                type_error(
                    "JSON value of type number for field scriptPubKey is not of expected type string",
                ),
            ),
            (prev(r#"{"scriptPubKey":"51","vout":"x"}"#), type_error("Missing txid")),
            (
                prev(r#"{"scriptPubKey":"51","txid":1}"#),
                type_error(
                    "JSON value of type number for field txid is not of expected type string",
                ),
            ),
            (prev(&format!(r#"{{"scriptPubKey":"51","txid":"{a}"}}"#)), type_error("Missing vout")),
            (
                prev(&format!(r#"{{"scriptPubKey":"51","txid":"{a}","vout":"0"}}"#)),
                type_error(
                    "JSON value of type string for field vout is not of expected type number",
                ),
            ),
            (
                prev(r#"{"scriptPubKey":"51","txid":"00","vout":0}"#),
                (RPC_INVALID_PARAMETER, "txid must be of length 64 (not 2, for '00')".into()),
            ),
            (
                prev(&format!(r#"{{"scriptPubKey":"51","txid":"{a}","vout":1.5}}"#)),
                (RPC_MISC_ERROR, "JSON integer out of range".into()),
            ),
            (
                prev(&format!(r#"{{"scriptPubKey":"51","txid":"{a}","vout":-1}}"#)),
                (RPC_DESERIALIZATION_ERROR, "vout cannot be negative".into()),
            ),
            (
                prev(&format!(r#"{{"scriptPubKey":"","txid":"{a}","vout":0}}"#)),
                (RPC_INVALID_PARAMETER, "scriptPubKey must be hexadecimal string (not '')".into()),
            ),
            (
                prev(&format!(r#"{{"scriptPubKey":"51","txid":"{a}","vout":10}}"#)),
                (RPC_DESERIALIZATION_ERROR, mismatch),
            ),
            // The script is checked before the amount.
            (
                prev(&format!(
                    r#"{{"scriptPubKey":"{spk}","txid":"{a}","vout":10,"amount":null}}"#
                )),
                type_error("Amount is not a number or string"),
            ),
            (
                prev(&format!(r#"{{"scriptPubKey":"{p2sh}","txid":"{a}","vout":1}}"#)),
                type_error("Missing redeemScript"),
            ),
            (
                prev(&format!(
                    r#"{{"scriptPubKey":"{p2sh}","txid":"{a}","vout":1,"redeemScript":null}}"#
                )),
                type_error("Missing redeemScript"),
            ),
            (
                prev(&format!(
                    r#"{{"scriptPubKey":"{p2sh}","txid":"{a}","vout":1,"redeemScript":5}}"#
                )),
                type_error(
                    "JSON value of type number for field redeemScript is not of expected type string",
                ),
            ),
            (
                prev(&format!(
                    r#"{{"scriptPubKey":"{p2sh}","txid":"{a}","vout":1,"redeemScript":"zz"}}"#
                )),
                (
                    RPC_INVALID_PARAMETER,
                    "redeemScript must be hexadecimal string (not 'zz')".into(),
                ),
            ),
            (
                prev(&format!(
                    r#"{{"scriptPubKey":"{p2sh}","txid":"{a}","vout":1,"redeemScript":"51"}}"#
                )),
                (RPC_INVALID_PARAMETER, "redeemScript does not match scriptPubKey".into()),
            ),
            // The prevtxs before the sighash name.
            (format!(r#"["{tx}", ["{k}"], [{{}}], "FOO"]"#), type_error("Missing scriptPubKey")),
            (
                format!(r#"["{tx}", ["{k}"], null, "FOO"]"#),
                (RPC_INVALID_PARAMETER, "FOO is not a valid sighash parameter.".into()),
            ),
            (
                format!(
                    r#"["{tx}", ["{k}"], [{{"scriptPubKey":"{p2sh}","txid":"{a}","vout":1,"redeemScript":"{redeem}"}}], "all"]"#
                ),
                (RPC_INVALID_PARAMETER, "all is not a valid sighash parameter.".into()),
            ),
        ];
        for (json, want) in cases {
            assert_eq!(sign_json(&node(), &json).unwrap_err(), want, "{json}");
        }
        // The keys are read before the chainstate is needed.
        let none = FakeNode(None);
        assert_eq!(
            sign_json(&none, &format!(r#"["{tx}", ["x"]]"#)).unwrap_err(),
            (RPC_INVALID_ADDRESS_OR_KEY, "Invalid private key".into())
        );
        assert_eq!(
            sign_json(&none, &format!(r#"["{tx}", ["{k}"]]"#)).unwrap_err(),
            (RPC_INTERNAL_ERROR, "Node chainman not found".into())
        );
    }

    /// Combining: partial multisig variants into a complete one, a shorter later variant
    /// skipped, an input no variant signs rebuilt (a bare multisig's `00 00`), the first of two
    /// complete alternatives kept, and a zero-input first transaction returned as it is.
    // Core: rpc/rawtransaction.cpp:1011-1095
    #[test]
    fn combinerawtransaction_merges_as_core() {
        let ms = multisig(2, &[1, 2, 3]);
        let tx = unsigned(&[outpoint(0xb), outpoint(0xa)], &[1]);
        let s1 = sig(1, &ms, &tx, 0, SIGHASH_ALL);
        let s3 = sig(3, &ms, &tx, 0, SIGHASH_ALL);
        let v1 = with_script_sigs(&tx, &[&[vec![OP_0], push(&s1), vec![OP_0]].concat()]);
        let v3 = with_script_sigs(&tx, &[&[vec![OP_0], push(&s3)].concat()]);
        let mut short = tx.clone();
        {
            let mut inputs = short.inputs().clone();
            inputs.truncate(1);
            short = dash_primitives::Transaction::new(
                short.version(),
                short.tx_type(),
                inputs,
                short.outputs().clone(),
                short.lock_time(),
                short.extra_payload().clone(),
            );
        }
        let combined =
            decode(&combine(&node(), &[tx_hex(&v3), tx_hex(&short), tx_hex(&v1)]).unwrap());
        let complete = [vec![OP_0], push(&s1), push(&s3)].concat();
        assert_eq!(combined.inputs()[0].script_sig().as_bytes(), complete);
        // A P2PKH input no variant signs stays empty.
        assert!(combined.inputs()[1].script_sig().as_bytes().is_empty());

        // The multisig unsigned everywhere is rebuilt as the dummy and one empty.
        let combined = decode(&combine(&node(), &[tx_hex(&tx)]).unwrap());
        assert_eq!(combined.inputs()[0].script_sig().as_bytes(), [OP_0, OP_0]);

        // Two complete alternatives: the first wins.
        let one = unsigned(&[outpoint(0xa)], &[1]);
        let alt = |hash_type| {
            let s = sig(1, &p2pkh(1), &one, 0, hash_type);
            with_script_sigs(&one, &[&[push(&s), push(&pubkey(1))].concat()])
        };
        let (all, none) = (alt(SIGHASH_ALL), alt(SIGHASH_NONE));
        assert_eq!(combine(&node(), &[tx_hex(&all), tx_hex(&none)]).unwrap(), tx_hex(&all));
        assert_eq!(combine(&node(), &[tx_hex(&none), tx_hex(&all)]).unwrap(), tx_hex(&none));

        // Zero inputs: the first transaction as given.
        let empty = unsigned(&[], &[1]);
        assert_eq!(combine(&node(), &[tx_hex(&empty), tx_hex(&all)]).unwrap(), tx_hex(&empty));
    }

    /// A later variant whose outputs differ, complete over its own form: its signature is
    /// extracted against its own transaction and wins as it is, in the first transaction's
    /// other fields, though it does not verify there (Sol r1 soundness note).
    // Core: script/sign.cpp:257-269, 314-325; rpc/rawtransaction.cpp:1082-1092
    #[test]
    fn a_variant_is_read_against_its_own_transaction() {
        let first = unsigned(&[outpoint(0xa)], &[1]);
        let mut other = unsigned(&[outpoint(0xa)], &[1, 2]);
        {
            let mut inputs = other.inputs().clone();
            inputs[0] =
                dash_primitives::TxIn::new(inputs[0].prevout(), inputs[0].script_sig().clone(), 7);
            other = dash_primitives::Transaction::new(
                other.version(),
                other.tx_type(),
                inputs,
                other.outputs().clone(),
                other.lock_time(),
                other.extra_payload().clone(),
            );
        }
        let s = sig(1, &p2pkh(1), &other, 0, SIGHASH_ALL);
        let script_sig = [push(&s), push(&pubkey(1))].concat();
        let signed_other = with_script_sigs(&other, &[&script_sig]);
        let combined = combine(&node(), &[tx_hex(&first), tx_hex(&signed_other)]).unwrap();
        assert_eq!(combined, tx_hex(&with_script_sigs(&first, &[&script_sig])));
    }

    /// Sol r1 M3 through the command: the stack `[sig3, empty, empty, sig2]` combines to K2's
    /// signature alone, and signing it with no keys gives the same scriptSig.
    // Core: script/sign.cpp:289-303
    #[test]
    fn a_recorded_key_takes_its_stack_item_in_both_commands() {
        let ms = multisig(2, &[1, 2, 3]);
        let tx = unsigned(&[outpoint(0xb)], &[1]);
        let s2 = sig(2, &ms, &tx, 0, SIGHASH_ALL);
        let s3 = sig(3, &ms, &tx, 0, SIGHASH_ALL);
        let raw = with_script_sigs(&tx, &[&[push(&s3), vec![OP_0, OP_0], push(&s2)].concat()]);
        let want = [vec![OP_0], push(&s2), vec![OP_0]].concat();
        let combined = decode(&combine(&node(), &[tx_hex(&raw)]).unwrap());
        assert_eq!(combined.inputs()[0].script_sig().as_bytes(), want);
        let result = signed(&node(), &raw, &[], "");
        assert_eq!(result_tx(&result).inputs()[0].script_sig().as_bytes(), want);
        assert_eq!(field(&result, "complete").write(), "false");
    }

    /// Every handler refusal of `combinerawtransaction`: each element's type and hex in order
    /// (the first failure wins), all before any coin is read, then the empty array, then a
    /// missing coin. Handler-only: the server refuses a non-array first.
    // Core: rpc/rawtransaction.cpp:1035-1078
    #[test]
    fn combinerawtransaction_refuses_as_core() {
        let unknown = tx_hex(&unsigned(&[outpoint(0x5)], &[1]));
        let good = tx_hex(&unsigned(&[outpoint(0xa)], &[1]));
        let decode_failed = |i: usize| {
            (
                RPC_DESERIALIZATION_ERROR,
                format!("TX decode failed for tx {i}. Make sure the tx has at least one input."),
            )
        };
        let cases: Vec<(String, (i32, String))> = vec![
            (
                "[[123]]".into(),
                (RPC_TYPE_ERROR, "JSON value of type number is not of expected type string".into()),
            ),
            (r#"[["zz"]]"#.into(), decode_failed(0)),
            (r#"[["zz", 123]]"#.into(), decode_failed(0)),
            (format!(r#"[["{good}", "zz"]]"#), decode_failed(1)),
            (format!(r#"[["{unknown}", "0"]]"#), decode_failed(1)),
            ("[[]]".into(), (RPC_DESERIALIZATION_ERROR, "Missing transactions".into())),
            (
                format!(r#"[["{unknown}"]]"#),
                (RPC_VERIFY_ERROR, "Input not found or already spent".into()),
            ),
        ];
        for (json, want) in cases {
            assert_eq!(combine_json(&node(), &json).unwrap_err(), want, "{json}");
        }
        // The first missing coin aborts the call after earlier inputs merged.
        let two = tx_hex(&unsigned(&[outpoint(0xa), outpoint(0x5)], &[1]));
        assert_eq!(
            combine_json(&node(), &format!(r#"[["{two}"]]"#)).unwrap_err(),
            (RPC_VERIFY_ERROR, "Input not found or already spent".into())
        );
    }

    /// Sol r1 M1: both commands read every coin in one turn. Here the tip moves after the
    /// first turn (both coins spent), so a read per input would see the first coin and miss
    /// the second.
    // Core: rpc/rawtransaction.cpp:1053-1067; node/coin.cpp:12-24
    #[test]
    fn the_coins_come_from_one_turn() {
        let moving = || FakeNode(Some(FakeCoins::at(vec![chain_coins(), BTreeMap::new()])));
        let tx = unsigned(&[outpoint(0xa), outpoint(0xb), outpoint(0xa)], &[1]);
        let node = moving();
        assert!(combine(&node, &[tx_hex(&tx)]).is_ok());
        assert_eq!(node.0.as_ref().unwrap().turns.load(Ordering::SeqCst), 1);

        let node = moving();
        let result = signed(&node, &tx, &[wif(1, true), wif(2, true)], "");
        assert_eq!(node.0.as_ref().unwrap().turns.load(Ordering::SeqCst), 1);
        assert_eq!(field(&result, "complete").write(), "true", "{}", result.write());
    }

    fn server() -> Arc<RpcServer> {
        let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
        register_rawtransaction_commands(
            &server,
            Arc::new(NoChain),
            Arc::new(node()),
            TxJsonParams {
                key_io: regtest(),
                service: false,
            },
            super::asset_unlocks::seams(&[], &[]),
        );
        server.start();
        server.set_warmup_finished();
        server
    }

    fn call(server: &RpcServer, method: &str, params: &str) -> Result<UniValue, (i32, String)> {
        server
            .execute(&JsonRpcRequest {
                method: method.into(),
                params: args(params),
                ..Default::default()
            })
            .map_err(code_and_message)
    }

    fn wrong_type(positions: &[(&str, &str)]) -> (i32, String) {
        let mut o = UniValue::new_object();
        for (name, ty) in positions {
            let (expected, got) = ty.split_once('/').unwrap();
            o.push_kv_end(
                (*name).to_owned(),
                format!("JSON value of type {got} is not of expected type {expected}").into(),
            );
        }
        (RPC_TYPE_ERROR, format!("Wrong type passed:\n{}", o.write_pretty(4, 0)))
    }

    /// Sol r1 M2 / r2 R2-m1: the server's checks come before the handlers: too few arguments
    /// are -1 with the help, then every top-level type mismatch in argument order (a required
    /// null too) is one wrapped -3, before any hex, key or prevtx is read; optional nulls pass.
    // Core: rpc/util.cpp:525-550, 622-631, 714-757; rpc/rawtransaction.cpp:1017-1023, 1105-1132
    #[test]
    fn preflight_checks_come_first() {
        let server = server();
        let help = |m: &str| dash_rpc_spec::command(m).unwrap().help.to_owned();
        let tx = tx_hex(&unsigned(&[outpoint(0xa)], &[1]));
        assert_eq!(
            call(&server, "combinerawtransaction", "[]").unwrap_err(),
            (RPC_MISC_ERROR, help("combinerawtransaction"))
        );
        assert_eq!(
            call(&server, "signrawtransactionwithkey", r#"["zz"]"#).unwrap_err(),
            (RPC_MISC_ERROR, help("signrawtransactionwithkey"))
        );
        let cases: [(&str, String, (i32, String)); 7] = [
            (
                "combinerawtransaction",
                "[null]".into(),
                wrong_type(&[("Position 1 (txs)", "array/null")]),
            ),
            (
                "combinerawtransaction",
                r#"["zz"]"#.into(),
                wrong_type(&[("Position 1 (txs)", "array/string")]),
            ),
            (
                "signrawtransactionwithkey",
                r#"["zz", null]"#.into(),
                wrong_type(&[("Position 2 (privkeys)", "array/null")]),
            ),
            (
                "signrawtransactionwithkey",
                r#"["zz", 5]"#.into(),
                wrong_type(&[("Position 2 (privkeys)", "array/number")]),
            ),
            (
                "signrawtransactionwithkey",
                "[5, 5]".into(),
                wrong_type(&[
                    ("Position 1 (hexstring)", "string/number"),
                    ("Position 2 (privkeys)", "array/number"),
                ]),
            ),
            (
                "signrawtransactionwithkey",
                format!(r#"["{tx}", [], [5], 1]"#),
                wrong_type(&[("Position 4 (sighashtype)", "string/number")]),
            ),
            (
                "signrawtransactionwithkey",
                format!(r#"["{tx}", [], {{}}]"#),
                wrong_type(&[("Position 3 (prevtxs)", "array/object")]),
            ),
        ];
        for (method, params, want) in cases {
            assert_eq!(call(&server, method, &params).unwrap_err(), want, "{method} {params}");
        }
        // Past the checks: optional nulls, element types, the handlers' own errors.
        assert_eq!(
            call(&server, "signrawtransactionwithkey", r#"["zz", [], null, null]"#).unwrap_err(),
            (
                RPC_DESERIALIZATION_ERROR,
                "TX decode failed. Make sure the tx has at least one input.".into()
            )
        );
        assert_eq!(
            call(&server, "combinerawtransaction", "[[123]]").unwrap_err(),
            (RPC_TYPE_ERROR, "JSON value of type number is not of expected type string".into())
        );
        assert!(call(&server, "combinerawtransaction", &format!(r#"[["{tx}"]]"#)).is_ok());
    }
}

/// `getassetunlockstatuses` over a fake chainman and fake mempool and InstantSend seams.
mod asset_unlocks {
    use std::collections::BTreeMap;

    use super::*;
    use crate::blockchain::tests::unlock_server;
    use crate::protocol::RPC_INTERNAL_ERROR;
    use crate::request::JsonRpcRequest;

    const METHOD: &str = "getassetunlockstatuses";

    fn txid(n: u8) -> TxHash {
        TxHash::from([n; 32])
    }

    /// A mempool holding `unlocks` (index, txids) and InstantSend locking `locked`.
    struct Fake {
        unlocks: BTreeMap<u64, Vec<TxHash>>,
        locked: Vec<TxHash>,
    }

    impl AssetUnlockMempool for Fake {
        fn txids_by_index(&self, index: u64) -> Vec<TxHash> {
            self.unlocks.get(&index).cloned().unwrap_or_default()
        }
    }

    impl IsLockView for Fake {
        fn is_locked(&self, txid: &TxHash) -> bool {
            self.locked.contains(txid)
        }
    }

    pub(super) fn seams(unlocks: &[(u64, &[u8])], locked: &[u8]) -> UnlockSeams {
        let fake = Arc::new(Fake {
            unlocks: unlocks
                .iter()
                .map(|(i, t)| (*i, t.iter().map(|n| txid(*n)).collect()))
                .collect(),
            locked: locked.iter().map(|n| txid(*n)).collect(),
        });
        UnlockSeams {
            mempool: Arc::clone(&fake) as _,
            islocks: fake,
        }
    }

    fn ok(server: &RpcServer, params: &str) -> String {
        server
            .execute(&JsonRpcRequest {
                method: METHOD.into(),
                params: args(params),
                ..Default::default()
            })
            .unwrap()
            .write()
    }

    fn err(server: &RpcServer, params: &str) -> (i32, String) {
        server
            .execute(&JsonRpcRequest {
                method: METHOD.into(),
                params: args(params),
                ..Default::default()
            })
            .map_err(code_and_message)
            .unwrap_err()
    }

    /// The statuses in Core's precedence: the ChainLock's pool, the tip's, the mempool (with
    /// `instantlock` only there), else unknown; the keys in Core's order; one pools read.
    // Core: rpc/rawtransaction.cpp:878-898
    #[test]
    fn statuses_follow_cores_precedence() {
        let (server, live) = unlock_server(seams(&[(4, &[1]), (5, &[2, 3]), (1, &[4])], &[3]));
        live.lock().unwrap().unlock = Some(Ok((Some(vec![1]), Some(vec![1, 2]))));
        let got = ok(&server, r#"[["1", "2", "3", "4", "5"]]"#);
        let want = concat!(
            r#"[{"index":1,"status":"chainlocked"},{"index":2,"status":"mined"},"#,
            r#"{"index":3,"status":"unknown"},"#,
            r#"{"index":4,"status":"mempooled","instantlock":false},"#,
            r#"{"index":5,"status":"mempooled","instantlock":true}]"#
        );
        assert_eq!(got, want);
        assert_eq!(live.lock().unwrap().calls, ["unlock pools None"]);

        // With a height the mempool is not reported.
        live.lock().unwrap().unlock = Some(Ok((Some(vec![1]), None)));
        let got = ok(&server, r#"[["1", "2", "4", "5"], 7]"#);
        let want = concat!(
            r#"[{"index":1,"status":"chainlocked"},{"index":2,"status":"unknown"},"#,
            r#"{"index":4,"status":"unknown"},{"index":5,"status":"unknown"}]"#
        );
        assert_eq!(got, want);
        assert_eq!(live.lock().unwrap().calls[1], "unlock pools Some(7)");

        // No pools at all, and an empty request.
        live.lock().unwrap().unlock = Some(Ok((None, None)));
        assert_eq!(ok(&server, r#"[["3"]]"#), r#"[{"index":3,"status":"unknown"}]"#);
        let pooled = r#"[{"index":1,"status":"mempooled","instantlock":false}]"#;
        assert_eq!(ok(&server, r#"[["1"]]"#), pooled);
        assert_eq!(ok(&server, "[[]]"), "[]");
    }

    /// `ParseUInt64`: an optional single `+`, then the whole rest as an unsigned decimal; the
    /// index prints unsigned.
    // Core: util/strencodings.cpp:234-250, :278-281
    #[test]
    fn indexes_parse_as_parse_uint64() {
        let (server, live) = unlock_server(seams(&[], &[]));
        live.lock().unwrap().unlock = Some(Ok((None, Some(vec![u64::MAX, 1 << 63]))));
        for (text, index) in [
            ("+5", 5),
            ("007", 7),
            ("0", 0),
            ("18446744073709551615", u64::MAX),
            ("9223372036854775808", 1 << 63),
        ] {
            let got = ok(&server, &format!(r#"[["{text}"]]"#));
            assert!(got.starts_with(&format!(r#"[{{"index":{index},"#)), "{text}: {got}");
        }
        let got = ok(&server, r#"[["18446744073709551615"]]"#);
        assert_eq!(got, r#"[{"index":18446744073709551615,"status":"mined"}]"#);
        let invalid = (RPC_INVALID_PARAMETER, "invalid index".to_owned());
        for text in [
            "+-5",
            "++5",
            " 5",
            "5 ",
            "-0",
            "-1",
            "0x10",
            "1e3",
            "",
            "\\u0000",
            "18446744073709551616",
        ] {
            assert_eq!(err(&server, &format!(r#"[["{text}"]]"#)), invalid, "{text:?}");
        }
        // One bad index fails the whole call, wherever it is.
        assert_eq!(err(&server, r#"[["1", "x"]]"#), invalid);
        // A number element is `get_str()`'s bare type error.
        let bare =
            (RPC_TYPE_ERROR, "JSON value of type number is not of expected type string".to_owned());
        assert_eq!(err(&server, "[[101]]"), bare);
        assert_eq!(err(&server, r#"[["1", 2]]"#), bare);
    }

    /// The cap before the height, the height before the pools, the pools before any index.
    #[test]
    fn errors_come_in_cores_order() {
        let (server, live) = unlock_server(seams(&[], &[]));
        let hundred: Vec<String> = (0..100).map(|i| format!(r#""{i}""#)).collect();
        live.lock().unwrap().unlock = Some(Ok((None, None)));
        assert!(ok(&server, &format!("[[{}]]", hundred.join(","))).contains(r#""index":99"#));
        let calls = live.lock().unwrap().calls.len();
        let over = format!("[[{}, \"x\"], -1]", hundred.join(","));
        assert_eq!(err(&server, &over), (RPC_INVALID_PARAMETER, "Up to 100 indexes only".into()));
        assert_eq!(
            err(&server, r#"[["x"], 1.5]"#),
            (RPC_MISC_ERROR, "JSON integer out of range".into())
        );
        assert_eq!(live.lock().unwrap().calls.len(), calls, "no pools read");

        live.lock().unwrap().unlock = Some(Err(PoolsError::OutOfRange));
        let range = (RPC_INVALID_PARAMETER, "Block height out of range".to_owned());
        assert_eq!(err(&server, r#"[["x"], -1]"#), range);
        live.lock().unwrap().unlock = Some(Err(PoolsError::NoTip));
        assert_eq!(err(&server, r#"[["x"]]"#), (RPC_INTERNAL_ERROR, "No blocks in chain".into()));
        let text = "ConstructCreditPool: failed-getcreditpool-index-duplicated";
        live.lock().unwrap().unlock = Some(Err(PoolsError::Failed(text.into())));
        assert_eq!(err(&server, r#"[["x", 1]]"#), (RPC_MISC_ERROR, text.into()));
    }

    /// The server's checks: arity, then each argument's type, before the handler.
    #[test]
    fn preflight_checks_come_first() {
        let (server, live) = unlock_server(seams(&[], &[]));
        let help = dash_rpc_spec::command(METHOD).unwrap().help.to_owned();
        for params in ["[]", r#"[[], 1, 2]"#] {
            assert_eq!(err(&server, params), (RPC_MISC_ERROR, help.clone()), "{params}");
        }
        for (params, position) in [
            ("[null]", "Position 1 (indexes)"),
            (r#"["1"]"#, "Position 1 (indexes)"),
            ("[{}]", "Position 1 (indexes)"),
            (r#"[["1"], "5"]"#, "Position 2 (height)"),
        ] {
            let (code, message) = err(&server, params);
            assert_eq!(code, RPC_TYPE_ERROR, "{params}");
            assert!(message.starts_with("Wrong type passed:"), "{params}: {message}");
            assert!(message.contains(position), "{params}: {message}");
        }
        // An explicit null height is no height.
        live.lock().unwrap().unlock = Some(Ok((None, None)));
        assert_eq!(ok(&server, r#"[["1"], null]"#), r#"[{"index":1,"status":"unknown"}]"#);
        assert_eq!(live.lock().unwrap().calls, ["unlock pools None"]);
        let named = ok(&server, r#"{"indexes": ["1"], "height": 3}"#);
        assert_eq!(named, r#"[{"index":1,"status":"unknown"}]"#);
        assert_eq!(live.lock().unwrap().calls[1], "unlock pools Some(3)");
    }
}
