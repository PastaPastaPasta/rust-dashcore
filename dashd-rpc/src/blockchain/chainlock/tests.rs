use super::{ClLookupError, HistoricalChainLock};
use crate::blockchain::tests::{
    HASH_7, call_method, live_server, method_err, reply, unloaded_server,
};
use crate::protocol::{
    RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RPC_MISC_ERROR,
    RPC_TYPE_ERROR,
};

const METHOD: &str = "getchainlockbyheight";

fn found() -> HistoricalChainLock {
    let mut signature = [0; 96];
    signature[0] = 0x8a;
    signature[95] = 0x01;
    HistoricalChainLock {
        block_hash: HASH_7.parse().unwrap(),
        signature,
        carrier_height: 309,
    }
}

/// Core's keys in its order: the request's height, the signed block, the signature's bytes and
/// the carrier's height.
#[test]
fn the_reply_has_cores_keys_in_its_order() {
    let (server, live) = live_server(3);
    live.lock().unwrap().chainlock = Some(Ok(found()));
    let got = call_method(&server, METHOD, "[308]").unwrap().write();
    let sig = format!("8a{}01", "00".repeat(94));
    let want =
        format!(r#"{{"height":308,"blockhash":"{HASH_7}","signature":"{sig}","cbtx_height":309}}"#);
    assert_eq!(got, want);
    let named = call_method(&server, METHOD, r#"{"height":308}"#).unwrap().write();
    assert_eq!(named, want);
    // `-0` is 0.
    let _ = call_method(&server, METHOD, "[-0]").unwrap();
    assert_eq!(live.lock().unwrap().calls, ["chainlock 308", "chainlock 308", "chainlock 0"]);
}

#[test]
fn lookup_failures_are_cores_errors() {
    let (server, live) = live_server(3);
    for (answer, want) in [
        (
            ClLookupError::NotFound,
            reply(RPC_INVALID_ADDRESS_OR_KEY, "Chainlock not found for height"),
        ),
        (
            ClLookupError::ChainChanged,
            reply(RPC_MISC_ERROR, "Chain changed during ChainLock lookup; retry"),
        ),
        (
            ClLookupError::Failed("Historical ChainLock block data unavailable".into()),
            reply(RPC_MISC_ERROR, "Historical ChainLock block data unavailable"),
        ),
    ] {
        live.lock().unwrap().chainlock = Some(Err(answer.clone()));
        assert_eq!(method_err(&server, METHOD, "[5]"), want, "{answer:?}");
    }
}

/// The server's arity and type checks, then `getInt<int>`, then the sign, all before the
/// chainman: a negative height is -8 even with none.
#[test]
fn argument_errors_come_before_the_chainman() {
    let (server, live) = live_server(3);
    let help = dash_rpc_spec::command(METHOD).unwrap().help;
    for params in ["[]", "[1, 2]"] {
        assert_eq!(method_err(&server, METHOD, params), (RPC_MISC_ERROR, help_reply(help)));
    }
    for (params, got) in [("[null]", "null"), (r#"["100"]"#, "string"), ("[true]", "bool")] {
        let (code, message) = method_err(&server, METHOD, params);
        assert_eq!(code, RPC_TYPE_ERROR, "{params}");
        let want = format!("JSON value of type {got} is not of expected type number");
        assert!(message.contains(&want), "{params}: {message}");
        assert!(message.contains("Position 1 (height)"), "{params}: {message}");
    }
    let not_int = reply(RPC_MISC_ERROR, "JSON integer out of range");
    for params in ["[1.5]", "[1e2]", "[2147483648]", "[-2147483649]"] {
        assert_eq!(method_err(&server, METHOD, params), not_int, "{params}");
    }
    let negative = reply(RPC_INVALID_PARAMETER, "height must be non-negative");
    assert_eq!(method_err(&server, METHOD, "[-1]"), negative);
    assert!(live.lock().unwrap().calls.is_empty(), "no lookup");

    let unloaded = unloaded_server();
    assert_eq!(method_err(&unloaded, METHOD, "[-1]"), negative);
    assert_eq!(
        method_err(&unloaded, METHOD, "[0]"),
        reply(RPC_INTERNAL_ERROR, "Node chainman not found")
    );
}

/// The help text as the reply's message writes it.
fn help_reply(help: &str) -> String {
    let mut o = crate::UniValue::new_object();
    o.push_kv_end("code", RPC_MISC_ERROR.into());
    o.push_kv_end("message", help.into());
    o.write()
}
