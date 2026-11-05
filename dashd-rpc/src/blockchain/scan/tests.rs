//! `scantxoutset` over a fake chainman: the actions, the scan objects' needles and inferred
//! descriptors (each object's own provider, a script's first inference kept), the result's
//! form, the reserver across a running scan and its release on every error, and the errors in
//! Core's order.

use std::sync::{Arc, Condvar, Mutex};

use dash_consensus::coins::Coin;
use dash_primitives::TxHash;
use dash_types::Numeric as _;

use super::super::tests::{call_method, hash, method_err, reply, scan_server};
use super::TxOutSetScan;
use crate::UniValue;
use crate::blockchain::TxOutSetError;
use crate::descriptor::tests::regtest;
use crate::descriptor::{SigningProvider, hex, infer_descriptor_with};
use crate::key_io::{Destination, encode_destination, hash160};
use crate::protocol::{
    RPC_CLIENT_NOT_CONNECTED, RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY,
    RPC_INVALID_PARAMETER, RPC_MISC_ERROR, RPC_TYPE_ERROR,
};
use crate::request::parse_hex;

const KEY: &str = "03a34b99f22c790c4e36b2b3c2c35a36db06226e41c692fc82b8b56ac1c540c5bd";

fn p2pkh(key: &str) -> Vec<u8> {
    Destination::PkHash(hash160(&parse_hex(key).unwrap())).script()
}

fn addr(key: &str) -> String {
    encode_destination(&Destination::PkHash(hash160(&parse_hex(key).unwrap())), &regtest())
}

/// `desc`'s first script inferred over its own expansion, as `start` infers it.
fn inferred(desc: &str) -> String {
    let mut provider = SigningProvider::default();
    let scanobject = UniValue::from(desc.to_owned());
    let scripts =
        crate::util::eval_descriptor_string_or_object(&scanobject, &mut provider, &regtest())
            .unwrap();
    infer_descriptor_with(&scripts[0], &provider, &regtest())
}

fn walk(unspents: Vec<(TxHash, u32, Coin)>) -> TxOutSetScan {
    TxOutSetScan {
        success: true,
        count: 42,
        best_block: hash(9),
        height: 7,
        unspents,
    }
}

// Core: rpc/blockchain.cpp:2722-2740
#[test]
fn status_and_abort_with_no_scan_answer_null_and_false() {
    let (server, live) = scan_server(true);
    assert_eq!(call_method(&server, "scantxoutset", r#"["status"]"#).unwrap(), UniValue::Null);
    assert_eq!(call_method(&server, "scantxoutset", r#"["abort"]"#).unwrap(), false.into());
    // Neither kept the slot.
    live.lock().unwrap().scan = Some(Ok(walk(vec![])));
    assert!(call_method(&server, "scantxoutset", r#"["start", []]"#).is_ok());
}

/// `start`'s result: Core's keys in order, each found coin with its script's inferred
/// descriptor, `(int32_t)` vout and height, and the total.
// Core: rpc/blockchain.cpp:2775-2805
#[test]
fn start_answers_cores_result_form() {
    let (server, live) = scan_server(true);
    let script = p2pkh(KEY);
    let txid = TxHash::from_lendian([0x11; 32]);
    live.lock().unwrap().scan = Some(Ok(walk(vec![
        (txid, 1, Coin::new(150_000_000, &script, 5, true)),
        (txid, 0x8000_0000, Coin::new(1, &script, 0x7fff_ffff, false)),
    ])));
    let params = format!(r#"["start", ["pkh({KEY})"]]"#);
    let got = call_method(&server, "scantxoutset", &params).unwrap().write();
    let desc = inferred(&format!("pkh({KEY})"));
    let want = format!(
        concat!(
            r#"{{"success":true,"txouts":42,"height":7,"bestblock":"{}","unspents":["#,
            r#"{{"txid":"{txid}","vout":1,"scriptPubKey":"{s}","desc":"{d}","amount":1.50000000,"coinbase":true,"height":5}},"#,
            r#"{{"txid":"{txid}","vout":-2147483648,"scriptPubKey":"{s}","desc":"{d}","amount":0.00000001,"coinbase":false,"height":2147483647}}"#,
            r#"],"total_amount":1.50000001}}"#
        ),
        hash(9),
        txid = txid,
        s = hex(&script),
        d = desc,
    );
    assert_eq!(got, want);
    assert!(desc.starts_with("pkh(["), "{desc}");
    let calls = live.lock().unwrap().calls.clone();
    assert_eq!(calls, [format!("scan {} abort=false", hex(&script))]);
}

/// Each scan object has its own provider and a script keeps its first inference: `addr()`
/// before `pkh()` of the same key prints `addr()`, the other way round `pkh([fp]key)`. The
/// needles are a set.
// Core: rpc/blockchain.cpp:2748-2758
#[test]
fn each_scan_object_infers_over_its_own_provider_and_the_first_inference_stays() {
    let script = p2pkh(KEY);
    let coin = Coin::new(7, &script, 3, false);
    let txid = TxHash::from_lendian([0x22; 32]);
    let a = format!("addr({})", addr(KEY));
    let k = format!("pkh({KEY})");
    for (objects, want) in [
        (format!(r#"["{a}", "{k}"]"#), inferred(&a)),
        (format!(r#"["{k}", {{"desc":"{a}"}}, "{k}"]"#), inferred(&k)),
    ] {
        let (server, live) = scan_server(true);
        live.lock().unwrap().scan = Some(Ok(walk(vec![(txid, 0, coin.clone())])));
        let got = call_method(&server, "scantxoutset", &format!(r#"["start", {objects}]"#))
            .unwrap()
            .write();
        assert!(got.contains(&format!(r#""desc":"{want}""#)), "{got}");
        assert_eq!(live.lock().unwrap().calls, [format!("scan {} abort=false", hex(&script))]);
    }
    assert!(inferred(&a).starts_with("addr("));
}

/// An earlier object's expansion is not in a later one's provider: `pkh(K)` records K's origin,
/// yet the P2PK script of `raw()` after it infers as a bare `pk(K)`.
// Core: rpc/blockchain.cpp:2752
#[test]
fn an_earlier_scan_objects_origins_do_not_reach_a_later_one() {
    let p2pk = parse_hex(&format!("21{KEY}ac")).unwrap();
    let (server, live) = scan_server(true);
    let txid = TxHash::from_lendian([0x33; 32]);
    live.lock().unwrap().scan = Some(Ok(walk(vec![(txid, 0, Coin::new(7, &p2pk, 3, false))])));
    let raw = format!("raw({})", hex(&p2pk));
    let params = format!(r#"["start", ["pkh({KEY})", "{raw}"]]"#);
    let got = call_method(&server, "scantxoutset", &params).unwrap().write();
    let want = inferred(&raw);
    assert!(want.starts_with(&format!("pk({KEY})")), "{want}");
    assert!(got.contains(&format!(r#""desc":"{want}""#)), "{got}");
}

/// A ranged scan object's needles: every position of its range.
#[test]
fn a_ranged_scan_objects_positions_are_all_needles() {
    let (server, live) = scan_server(true);
    live.lock().unwrap().scan = Some(Ok(walk(vec![])));
    let tpub = "tpubD6NzVbkrYhZ4WaWSyoBvQwbpLkojyoTZPRsgXELWz3Popb3qkjcJyJUGLnL4qHHoQvao8ESaAstxYSnhyswJ76uZPStJRJCTKvosUCJZL5B";
    let params = format!(r#"["start", [{{"desc":"combo({tpub}/0/*)","range":[1,3]}}]]"#);
    call_method(&server, "scantxoutset", &params).unwrap();
    let calls = live.lock().unwrap().calls.clone();
    // combo() of a compressed key: three scripts at each of three positions.
    assert_eq!(calls[0].split(' ').nth(1).unwrap().split(',').count(), 9);
}

/// A running scan: `status` reads its progress, a second `start` is refused, `abort` sets the
/// flag the walk reads; once it ends the slot and progress are free again, and the next `start`
/// clears the flag before its walk.
// Core: rpc/blockchain.cpp:2596-2622, 2722-2745, 2760
#[test]
fn the_reserver_holds_a_running_scan_and_abort_reaches_its_walk() {
    let (server, live) = scan_server(true);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    {
        let mut l = live.lock().unwrap();
        l.scan = Some(Ok(walk(vec![])));
        l.scan_gate = Some((37, Arc::clone(&gate)));
    }
    let running = {
        let server = Arc::clone(&server);
        std::thread::spawn(move || call_method(&server, "scantxoutset", r#"["start", []]"#))
    };
    while live.lock().unwrap().calls.is_empty() {
        std::thread::yield_now();
    }
    // The walk has entered and set its progress.
    while call_method(&server, "scantxoutset", r#"["status"]"#).unwrap().write()
        != r#"{"progress":37}"#
    {
        std::thread::yield_now();
    }
    assert_eq!(
        method_err(&server, "scantxoutset", r#"["start", []]"#),
        reply(
            RPC_INVALID_PARAMETER,
            r#"Scan already in progress, use action \"abort\" or \"status\""#
        )
    );
    assert_eq!(call_method(&server, "scantxoutset", r#"["abort"]"#).unwrap(), true.into());
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    let done = running.join().unwrap().unwrap().write();
    assert!(done.starts_with(r#"{"success":true"#), "{done}");
    assert_eq!(live.lock().unwrap().scan_aborted, Some(true));
    assert_eq!(call_method(&server, "scantxoutset", r#"["status"]"#).unwrap(), UniValue::Null);
    live.lock().unwrap().scan_gate = None;
    call_method(&server, "scantxoutset", r#"["start", []]"#).unwrap();
    let l = live.lock().unwrap();
    assert_eq!(l.scan_aborted, Some(false));
    assert_eq!(l.calls[1], "scan  abort=false");
}

/// The errors, in Core's order, and the slot released after each: the reserver comes first, then
/// the argument count, the array, each scan object, the chainman, the walk.
// Core: rpc/blockchain.cpp:2741-2774, 2830-2832
#[test]
fn starts_errors_are_cores_and_each_releases_the_slot() {
    let (server, live) = scan_server(true);
    let err = |params: &str| method_err(&server, "scantxoutset", params);
    assert_eq!(
        err(r#"["start"]"#),
        reply(RPC_MISC_ERROR, "scanobjects argument is required for the start action")
    );
    assert_eq!(
        err(r#"["start", null]"#),
        reply(RPC_TYPE_ERROR, "JSON value of type null is not of expected type array")
    );
    assert_eq!(
        err(r#"["start", ["pkh(junk)"]]"#),
        reply(RPC_INVALID_ADDRESS_OR_KEY, "pkh(): key 'junk' is not valid")
    );
    assert_eq!(
        err(r#"["start", [5]]"#),
        reply(RPC_INVALID_PARAMETER, "Scan object needs to be either a string or an object")
    );
    assert_eq!(err(r#"["stop"]"#), reply(RPC_INVALID_PARAMETER, "Invalid action 'stop'"));
    assert_eq!(err(r#"["Start", []]"#), reply(RPC_INVALID_PARAMETER, "Invalid action 'Start'"));
    // The walk's: an unreadable set, then the interruption point.
    assert_eq!(err(r#"["start", []]"#), reply(RPC_INTERNAL_ERROR, "Unable to read UTXO set"));
    live.lock().unwrap().scan = Some(Err(TxOutSetError::Interrupted));
    assert_eq!(err(r#"["start", []]"#), reply(RPC_CLIENT_NOT_CONNECTED, "Shutting down"));
    // None of these kept the slot.
    assert_eq!(call_method(&server, "scantxoutset", r#"["status"]"#).unwrap(), UniValue::Null);
    // A walk was asked for only where every check before it passed.
    assert_eq!(live.lock().unwrap().calls.len(), 2);

    // Before the chain loads: the scan objects are evaluated before the chainman is asked for.
    let (unloaded, _) = scan_server(false);
    let err = |params: &str| method_err(&unloaded, "scantxoutset", params);
    assert_eq!(err(r#"["start", ["pkh(junk)"]]"#).0, RPC_INVALID_ADDRESS_OR_KEY);
    assert_eq!(err(r#"["start", ["raw(00)"]]"#).0, RPC_INTERNAL_ERROR);
    assert_eq!(call_method(&unloaded, "scantxoutset", r#"["status"]"#).unwrap(), UniValue::Null);
}
