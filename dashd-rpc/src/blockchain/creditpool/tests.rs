use super::{CreditPoolFacts, CreditPoolReadError};
use crate::blockchain::tests::{
    HASH_7, call_method, live_server, method_err, reply, unloaded_server,
};
use crate::protocol::{RPC_INTERNAL_ERROR, RPC_INVALID_PARAMETER, RPC_MISC_ERROR, RPC_TYPE_ERROR};

const COIN: i64 = 100_000_000;

fn facts(window_start: Option<(u32, i64)>) -> CreditPoolFacts {
    CreditPoolFacts {
        height: 250,
        hash: HASH_7.parse().unwrap(),
        locked: 12 * COIN + 3,
        current_limit: 5 * COIN,
        lately_unlocked: COIN / 2,
        window_blocks: 100,
        window_start,
    }
}

#[test]
fn the_reply_has_cores_keys_in_its_order() {
    let (server, live) = live_server(3);
    live.lock().unwrap().credit = Some(Ok(facts(Some((150, 7 * COIN)))));
    let got = call_method(&server, "getcreditpoolinfo", "[250]").unwrap().write();
    let want = format!(
        r#"{{"height":250,"blockhash":"{HASH_7}","balance":12.00000003,"currentlimit":5.00000000,"window":{{"blocks":100,"height":150,"balance":7.00000000,"unlocked":0.50000000}}}}"#
    );
    assert_eq!(got, want);
    assert_eq!(live.lock().unwrap().calls, ["credit Some(250)"]);
}

/// A chain shorter than the window: Core's -1 and a 0 balance.
#[test]
fn without_a_window_start_the_window_is_minus_one_and_zero() {
    let (server, live) = live_server(3);
    live.lock().unwrap().credit = Some(Ok(facts(None)));
    let got = call_method(&server, "getcreditpoolinfo", "[]").unwrap().write();
    assert!(
        got.ends_with(
            r#""window":{"blocks":100,"height":-1,"balance":0.00000000,"unlocked":0.50000000}}"#
        ),
        "{got}"
    );
    // No argument (and an explicit null) is the tip.
    let _ = call_method(&server, "getcreditpoolinfo", "[null]").unwrap();
    assert_eq!(live.lock().unwrap().calls, ["credit None", "credit None"]);
}

#[test]
fn errors_are_cores() {
    let (server, live) = live_server(3);
    live.lock().unwrap().credit = Some(Err(CreditPoolReadError::OutOfRange));
    assert_eq!(
        method_err(&server, "getcreditpoolinfo", "[-1]"),
        reply(RPC_INVALID_PARAMETER, "Block height out of range")
    );
    // What `GetCreditPool` throws reaches the caller as `ExecuteCommand`'s runtime error.
    let text = "ConstructCreditPool: failed-getcreditpool-index-duplicated";
    live.lock().unwrap().credit = Some(Err(CreditPoolReadError::Failed(text.into())));
    assert_eq!(method_err(&server, "getcreditpoolinfo", "[1]"), reply(RPC_MISC_ERROR, text));
    // `getInt<int>`'s errors come before any read.
    let calls = live.lock().unwrap().calls.len();
    let (code, _) = method_err(&server, "getcreditpoolinfo", r#"["1"]"#);
    assert_eq!(code, RPC_TYPE_ERROR);
    assert_eq!(
        method_err(&server, "getcreditpoolinfo", "[2147483648]"),
        reply(RPC_MISC_ERROR, "JSON integer out of range")
    );
    assert_eq!(live.lock().unwrap().calls.len(), calls, "no read after a bad height");
}

#[test]
fn without_a_chainman_it_is_cores_error() {
    let server = unloaded_server();
    assert_eq!(
        method_err(&server, "getcreditpoolinfo", "[]"),
        reply(RPC_INTERNAL_ERROR, "Node chainman not found")
    );
}
