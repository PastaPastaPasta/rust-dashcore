use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use dash_args::strencodings::locale_independent_atoi;
use dash_consensus::BlockValidationError;
use dash_consensus::primitives::TxOut;
use dash_consensus::validation::BlockValidationResult;
use dash_num::Arith256;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::NotTested;
use dashd_mining::TemplateError;
use dashd_mining::gbt::{GbtError, GbtRequest, GbtTemplate, GbtTx, LongPoll, Mode};

use super::{BlockTemplates, GbtAnswer, parse_longpoll, register_block_template_command};
use crate::UniValue;
use crate::mining::{ValidationState, decode_hex_blk, decode_hex_tx};
use crate::protocol::{
    RPC_DESERIALIZATION_ERROR, RPC_INVALID_PARAMETER, RPC_TYPE_ERROR, RPC_VERIFY_ERROR, RpcError,
};
use crate::request::JsonRpcRequest;
use crate::server::RpcServer;
use crate::testutil::FakeControl;

/// The engine: every request it receives (as its `Debug` text) and the answer it gives.
struct FakeTemplates {
    requests: Mutex<Vec<String>>,
    answer: Result<GbtAnswer, GbtError>,
}

impl BlockTemplates for FakeTemplates {
    fn get_block_template(&self, request: GbtRequest) -> Result<GbtAnswer, GbtError> {
        self.requests.lock().unwrap().push(format!("{request:?}"));
        self.answer.clone()
    }
}

fn server(answer: Result<GbtAnswer, GbtError>) -> (RpcServer, Arc<FakeTemplates>) {
    let fake = Arc::new(FakeTemplates {
        requests: Mutex::new(Vec::new()),
        answer,
    });
    let server = RpcServer::new(Arc::new(FakeControl::default()));
    register_block_template_command(
        &server,
        Arc::clone(&fake) as _,
        crate::descriptor::tests::regtest(),
    );
    server.start();
    server.set_warmup_finished();
    (server, fake)
}

/// The reply as written, or the error's code and message; and the requests the engine received.
fn call_with(
    answer: Result<GbtAnswer, GbtError>,
    params: &str,
) -> (Result<String, (i32, String)>, Vec<String>) {
    let (server, fake) = server(answer);
    let reply = server.execute(&JsonRpcRequest {
        method: "getblocktemplate".into(),
        params: UniValue::read(params.as_bytes()).unwrap(),
        ..Default::default()
    });
    let reply = reply.map(|v| v.write()).map_err(|e: RpcError| (e.code(), e.to_object().write()));
    let requests = fake.requests.lock().unwrap().clone();
    (reply, requests)
}

/// The request the engine received for `params`; the engine answers an accepted proposal.
fn received(params: &str) -> String {
    let (reply, requests) = call_with(Ok(GbtAnswer::Proposal(Ok(ValidationState::Valid))), params);
    assert_eq!(reply, Ok("null".into()), "{params}");
    let [request] = requests.try_into().expect("one request");
    request
}

/// The error for `params`, which the engine never receives.
fn refused(params: &str) -> (i32, String) {
    let (reply, requests) = call_with(Err(GbtError::InvalidMode), params);
    assert!(requests.is_empty(), "{params}: {requests:?}");
    reply.unwrap_err()
}

fn error(code: i32, message: &str) -> (i32, String) {
    (code, format!(r#"{{"code":{code},"message":"{message}"}}"#))
}

fn request(mode: Mode, rules: &[&str], longpoll: LongPoll) -> String {
    let rules: BTreeSet<String> = rules.iter().map(|&r| r.to_owned()).collect();
    format!(
        "{:?}",
        GbtRequest {
            mode,
            rules,
            longpoll,
        }
    )
}

fn hash(n: u64) -> BlockHash {
    format!("{n:064x}").parse().unwrap()
}

fn block_hex() -> String {
    let header = format!("00000020{}{}01000000ffff7f2000000000", "00".repeat(32), "00".repeat(32));
    let coinbase = format!(
        "0100000001{}ffffffff0251ffffffffff01{}0151{}",
        "00".repeat(32),
        "00".repeat(8),
        "00".repeat(4)
    );
    format!("{header}01{coinbase}")
}

#[test]
fn no_request_object_asks_for_a_template_with_no_rules() {
    let template = request(Mode::Template, &[], LongPoll::None);
    for params in ["[]", "[null]", "[{}]", r#"[{"mode": null}]"#, r#"[{"mode": "template"}]"#] {
        assert_eq!(received(params), template, "{params}");
    }
}

#[test]
fn a_request_that_is_not_an_object_is_refused_by_its_type() {
    for (params, ty) in [("[5]", "number"), (r#"["template"]"#, "string"), ("[[]]", "array")] {
        let message = format!(
            r#"Wrong type passed:\n{{\n    \"Position 1 (template_request)\": \"JSON value of type {ty} is not of expected type object\"\n}}"#
        );
        assert_eq!(refused(params), error(RPC_TYPE_ERROR, &message), "{params}");
    }
}

/// Core collects the rules before it refuses an unknown mode, and reads no longpoll id for it.
#[test]
fn an_unknown_mode_string_reads_the_rules_but_not_the_longpoll_id() {
    assert_eq!(
        received(r#"[{"mode": "foo", "rules": ["b", "a"], "longpollid": "zz"}]"#),
        request(Mode::Invalid, &["a", "b"], LongPoll::None)
    );
    assert_eq!(received(r#"[{"mode": ""}]"#), request(Mode::Invalid, &[], LongPoll::None));
    assert_eq!(
        refused(r#"[{"mode": "foo", "rules": [1]}]"#),
        error(RPC_TYPE_ERROR, "JSON value of type number is not of expected type string")
    );
}

/// Core throws "Invalid mode" for a mode that is not a string before it reads anything else.
#[test]
fn a_mode_that_is_not_a_string_is_invalid_before_the_rules() {
    for mode in ["5", "true", "[]", "{}"] {
        let params = format!(r#"[{{"mode": {mode}, "rules": [1], "longpollid": "zz"}}]"#);
        assert_eq!(received(&params), request(Mode::Invalid, &[], LongPoll::None), "{params}");
    }
}

#[test]
fn a_proposal_carries_its_block_and_reads_neither_rules_nor_longpoll_id() {
    let hex = block_hex();
    let block = Arc::new(decode_hex_blk(&hex).unwrap());
    let params =
        format!(r#"[{{"mode": "proposal", "data": "{hex}", "rules": [1], "longpollid": "zz"}}]"#);
    assert_eq!(received(&params), request(Mode::Proposal(block), &[], LongPoll::None));
}

#[test]
fn a_proposal_without_decodable_string_data_is_refused() {
    let missing = error(RPC_TYPE_ERROR, "Missing data String key for proposal");
    for data in ["", r#", "data": null"#, r#", "data": 5"#, r#", "data": ["00"]"#] {
        let params = format!(r#"[{{"mode": "proposal"{data}}}]"#);
        assert_eq!(refused(&params), missing, "{params}");
    }
    let undecodable = error(RPC_DESERIALIZATION_ERROR, "Block decode failed");
    let hex = block_hex();
    for data in ["", "zz", "0", &hex[..hex.len() - 2]] {
        let params = format!(r#"[{{"mode": "proposal", "data": "{data}"}}]"#);
        assert_eq!(refused(&params), undecodable, "{params}");
    }
}

#[test]
fn the_rules_are_a_set_of_strings_and_any_other_value_is_ignored() {
    assert_eq!(
        received(r#"[{"rules": ["v20", "csv", "v20", ""]}]"#),
        request(Mode::Template, &["", "csv", "v20"], LongPoll::None)
    );
    for rules in [r#""csv""#, "5", r#"{"csv": 1}"#, "null"] {
        let params = format!(r#"[{{"rules": {rules}}}]"#);
        assert_eq!(received(&params), request(Mode::Template, &[], LongPoll::None), "{params}");
    }
    for (element, ty) in
        [("null", "null"), ("1", "number"), ("true", "bool"), (r#"["csv"]"#, "array")]
    {
        let params = format!(r#"[{{"rules": ["csv", {element}]}}]"#);
        let message = format!("JSON value of type {ty} is not of expected type string");
        assert_eq!(refused(&params), error(RPC_TYPE_ERROR, &message), "{params}");
    }
}

#[test]
fn the_longpoll_id_is_a_hash_and_a_count_or_any_other_value() {
    let id = |s: &str| received(&format!(r#"[{{"rules": ["csv"], "longpollid": {s}}}]"#));
    let template = |longpoll| request(Mode::Template, &["csv"], longpoll);
    assert_eq!(received(r#"[{"rules": ["csv"]}]"#), template(LongPoll::None));
    assert_eq!(id("null"), template(LongPoll::None));
    for other in ["5", "true", "[]", r#"{"a": "b"}"#] {
        assert_eq!(id(other), template(LongPoll::NonString), "{other}");
    }
    let tip = format!("{:064x}", 0xabcdu64);
    let longpoll = |tx_updated| {
        template(LongPoll::Id {
            tip: hash(0xabcd),
            tx_updated,
        })
    };
    assert_eq!(id(&format!(r#""{tip}42""#)), longpoll(42));
    assert_eq!(id(&format!(r#""{tip}""#)), longpoll(0));
    assert_eq!(id(&format!(r#""{}-1""#, tip.to_uppercase())), longpoll(u32::MAX));
}

/// `ParseHashV`'s length check on the first 64 characters comes first, then its hex check.
#[test]
fn a_longpoll_id_without_a_hash_prefix_is_refused() {
    let short = "a".repeat(63);
    for (lp, message) in [
        ("", "longpollid must be of length 64 (not 0, for '')".to_owned()),
        ("abc", "longpollid must be of length 64 (not 3, for 'abc')".to_owned()),
        (&short, format!("longpollid must be of length 64 (not 63, for '{short}')")),
    ] {
        let params = format!(r#"[{{"longpollid": "{lp}"}}]"#);
        assert_eq!(refused(&params), error(RPC_INVALID_PARAMETER, &message), "{params}");
    }
    let prefix = format!("zz{}", "0".repeat(62));
    assert_eq!(
        refused(&format!(r#"[{{"longpollid": "{prefix}12"}}]"#)),
        error(
            RPC_INVALID_PARAMETER,
            &format!("longpollid must be hexadecimal string (not '{prefix}')")
        )
    );
    let split = format!("{}\u{e9}", "a".repeat(63));
    assert_eq!(
        parse_longpoll(&UniValue::Str(split)).unwrap_err().to_object().write(),
        format!(
            r#"{{"code":-8,"message":"longpollid must be hexadecimal string (not '{}{}')"}}"#,
            "a".repeat(63),
            char::REPLACEMENT_CHARACTER
        )
    );
}

/// `LocaleIndependentAtoi<int64_t>` of the characters after the hash, wrapped to `unsigned int`.
#[test]
fn the_longpoll_count_is_locale_independent_atoi_wrapped() {
    let tip = format!("{:064x}", 7u64);
    for (rest, tx_updated) in [
        (" 12", 12),
        ("+5", 5),
        ("-1", u32::MAX),
        ("99999999999999999999", u32::MAX),
        ("-99999999999999999999", 0),
        ("7abc", 7),
        ("", 0),
        ("+-1", 0),
        ("++1", 0),
        ("--1", 0),
        ("-", 0),
        ("+", 0),
        ("-0", 0),
        ("00012", 12),
        (" -3", u32::MAX - 2),
        ("12 ", 12),
        ("\t\n\x0b\x0c\r 9 \t", 9),
        ("1 2", 1),
        ("\u{a0}5", 0),
        ("0x10", 0),
        ("1e3", 1),
        ("4294967296", 0),
        ("4294967297", 1),
        ("9223372036854775807", u32::MAX),
        ("9223372036854775808", u32::MAX),
        ("-9223372036854775808", 0),
        ("-9223372036854775809", 0),
    ] {
        assert_eq!(
            parse_longpoll(&UniValue::Str(format!("{tip}{rest}"))).unwrap(),
            LongPoll::Id {
                tip: hash(7),
                tx_updated
            },
            "{rest:?}"
        );
    }
    for (s, n) in [
        ("99999999999999999999", i64::MAX),
        ("-99999999999999999999", i64::MIN),
        ("9223372036854775807", i64::MAX),
        ("-9223372036854775808", i64::MIN),
        ("-1", -1),
        ("\x0b-42x", -42),
    ] {
        assert_eq!(locale_independent_atoi(s), n, "{s:?}");
    }
}

#[test]
fn every_engine_error_reaches_the_client_with_its_code_and_message() {
    for (e, code, message) in [
        (GbtError::InvalidMode, -8, "Invalid mode"),
        (GbtError::P2pDisabled, -31, "Error: Peer-to-peer functionality missing or disabled"),
        (GbtError::NotConnected, -9, "rdashd is not connected!"),
        (GbtError::InIbd, -10, "rdashd is in initial sync and waiting for blocks..."),
        (GbtError::Syncing, -10, "rdashd is syncing with network..."),
        (GbtError::MempoolDisabled, -33, "Mempool disabled or instance not found"),
        (GbtError::ShuttingDown, -9, "Shutting down"),
        (
            GbtError::RuleRequired("v20".into()),
            -8,
            "Support for 'v20' rule requires explicit client support",
        ),
        (
            GbtError::Template(TemplateError::NotReady {
                height: 12,
                rule: "a rule",
                owner: "a slice",
            }),
            -1,
            "block 12 needs a rule, which is not served until a slice",
        ),
        (GbtError::Template(TemplateError::Failed("bad-cb-amount".into())), -1, "bad-cb-amount"),
        (GbtError::Template(TemplateError::NoChain), -32603, "Node chainman not found"),
    ] {
        let (reply, requests) = call_with(Err(e.clone()), "[]");
        assert_eq!(reply, Err(error(code, message)), "{e:?}");
        assert_eq!(requests, [request(Mode::Template, &[], LongPoll::None)]);
    }
}

#[test]
fn a_proposal_answers_bip22_or_its_untested_string() {
    let hex = block_hex();
    let params = format!(r#"[{{"mode": "proposal", "data": "{hex}"}}]"#);
    let invalid = |reason: &'static str| {
        Ok(ValidationState::Invalid(BlockValidationError::new(
            BlockValidationResult::Consensus,
            reason,
        )))
    };
    for (outcome, expected) in [
        (Ok(ValidationState::Valid), Ok("null".to_owned())),
        (invalid("bad-txnmrklroot"), Ok(r#""bad-txnmrklroot""#.to_owned())),
        (invalid(""), Ok(r#""rejected""#.to_owned())),
        (Ok(ValidationState::Error("disk full".into())), Err(error(RPC_VERIFY_ERROR, "disk full"))),
        (Err(NotTested::Duplicate), Ok(r#""duplicate""#.to_owned())),
        (Err(NotTested::DuplicateInvalid), Ok(r#""duplicate-invalid""#.to_owned())),
        (Err(NotTested::DuplicateInconclusive), Ok(r#""duplicate-inconclusive""#.to_owned())),
        (Err(NotTested::NotBestPrev), Ok(r#""inconclusive-not-best-prevblk""#.to_owned())),
    ] {
        let (reply, _) = call_with(Ok(GbtAnswer::Proposal(outcome.clone())), &params);
        assert_eq!(reply, expected, "{outcome:?}");
    }
}

/// A version 1 transaction spending output 0 of `prev` (32 times the byte) to `OP_TRUE`.
fn tx_hex(prev: &str) -> String {
    format!(
        "0100000001{}0000000000ffffffff01e8030000000000000151{}",
        prev.repeat(32),
        "00".repeat(4)
    )
}

fn tx(prev: &str, txid: u64, depends: Vec<u32>, fee: i64, sigops: i64) -> GbtTx {
    GbtTx {
        tx: Arc::new(decode_hex_tx(&tx_hex(prev)).unwrap()),
        txid: format!("{txid:064x}").parse().unwrap(),
        depends,
        fee,
        sigops,
    }
}

fn p2pkh(byte: u8) -> Vec<u8> {
    [&[0x76, 0xa9, 20][..], &[byte; 20], &[0x88, 0xac]].concat()
}

fn p2sh(byte: u8) -> Vec<u8> {
    [&[0xa9, 20][..], &[byte; 20], &[0x87]].concat()
}

fn template() -> GbtTemplate {
    GbtTemplate {
        version: 0x2000_0002,
        rules: vec!["csv".into(), "dip0001".into(), "!testdummy".into()],
        vbavailable: vec![("v24".into(), 12), ("!testdummy".into(), 28)],
        vbrequired: 0,
        previousblockhash: hash(0xb),
        transactions: vec![tx("aa", 0x11, vec![], 1000, 1), tx("bb", 0x22, vec![1, 1], 2500, 4)],
        coinbasevalue: 50_000_002_500,
        longpollid: (hash(0xc), 7),
        target: Arith256::from_u64(0xffff).wrapping_shl(208),
        mintime: 1_700_000_001,
        sigoplimit: 40_000,
        sizelimit: 2_000_000,
        curtime: 1_700_000_100,
        bits: 0x1d00_ffff,
        previousbits: 0x0400_1234,
        height: 1234,
        masternode: vec![
            TxOut::from_duffs(1_000_000_000, p2pkh(0x11).into()),
            TxOut::from_duffs(5, vec![0x51].into()),
        ],
        masternode_payments_started: true,
        masternode_payments_enforced: true,
        superblock: vec![TxOut::from_duffs(200_000, p2sh(0x22).into())],
        superblocks_started: false,
        superblocks_enabled: true,
        coinbase_payload: vec![0x03, 0x00, 0xd2, 0x04],
    }
}

fn encoded(t: GbtTemplate) -> String {
    call_with(Ok(GbtAnswer::Template(t)), "[]").0.unwrap()
}

#[test]
fn a_template_is_encoded_in_core_key_order_and_forms() {
    let expected = [
        r#"{"capabilities":["proposal"],"version":536870914,"rules":["csv","dip0001","!testdummy"],"#,
        r#""vbavailable":{"v24":12,"!testdummy":28},"vbrequired":0,"#,
        r#""previousblockhash":"000000000000000000000000000000000000000000000000000000000000000b","#,
        r#""transactions":[{"data":""#,
        &tx_hex("aa"),
        r#"","hash":"0000000000000000000000000000000000000000000000000000000000000011","#,
        r#""depends":[],"fee":1000,"sigops":1},{"data":""#,
        &tx_hex("bb"),
        r#"","hash":"0000000000000000000000000000000000000000000000000000000000000022","#,
        r#""depends":[1,1],"fee":2500,"sigops":4}],"coinbaseaux":{},"coinbasevalue":50000002500,"#,
        r#""longpollid":"000000000000000000000000000000000000000000000000000000000000000c7","#,
        r#""target":"00000000ffff0000000000000000000000000000000000000000000000000000","#,
        r#""mintime":1700000001,"mutable":["time","transactions","prevblock"],"#,
        r#""noncerange":"00000000ffffffff","sigoplimit":40000,"sizelimit":2000000,"#,
        r#""curtime":1700000100,"bits":"1d00ffff","previousbits":"04001234","height":1234,"#,
        r#""masternode":[{"payee":"yMsgnH1xKGa85n4bq2imrZbG2KgrmGttAV","#,
        r#""script":"76a914111111111111111111111111111111111111111188ac","amount":1000000000},"#,
        r#"{"payee":"","script":"51","amount":5}],"#,
        r#""masternode_payments_started":true,"masternode_payments_enforced":true,"#,
        r#""superblock":[{"payee":"8hXwaDEiKTCZiRjbGZWN9Q9Bmz8WtQiJQm","#,
        r#""script":"a914222222222222222222222222222222222222222287","amount":200000}],"#,
        r#""superblocks_started":false,"superblocks_enabled":true,"coinbase_payload":"0300d204"}"#,
    ]
    .concat();
    assert_eq!(encoded(template()), expected);
}

#[test]
fn a_template_with_empty_lists_encodes_them_empty() {
    let t = GbtTemplate {
        version: 2,
        rules: vec!["csv".into()],
        vbavailable: vec![],
        transactions: vec![],
        coinbasevalue: 0,
        longpollid: (hash(0xc), 0),
        target: Arith256::from_u64(0x7f_ffff).wrapping_shl(232),
        bits: 0x207f_ffff,
        previousbits: 0x207f_ffff,
        masternode: vec![],
        masternode_payments_started: false,
        superblock: vec![],
        coinbase_payload: vec![],
        ..template()
    };
    let expected = [
        r#"{"capabilities":["proposal"],"version":2,"rules":["csv"],"vbavailable":{},"#,
        r#""vbrequired":0,"#,
        r#""previousblockhash":"000000000000000000000000000000000000000000000000000000000000000b","#,
        r#""transactions":[],"coinbaseaux":{},"coinbasevalue":0,"#,
        r#""longpollid":"000000000000000000000000000000000000000000000000000000000000000c0","#,
        r#""target":"7fffff0000000000000000000000000000000000000000000000000000000000","#,
        r#""mintime":1700000001,"mutable":["time","transactions","prevblock"],"#,
        r#""noncerange":"00000000ffffffff","sigoplimit":40000,"sizelimit":2000000,"#,
        r#""curtime":1700000100,"bits":"207fffff","previousbits":"207fffff","height":1234,"#,
        r#""masternode":[],"masternode_payments_started":false,"#,
        r#""masternode_payments_enforced":true,"superblock":[],"#,
        r#""superblocks_started":false,"superblocks_enabled":true,"coinbase_payload":""}"#,
    ]
    .concat();
    assert_eq!(encoded(t), expected);
}
