//! `getspecialtxes` over a fake chainman: the filter, `skip` and `count`, each verbosity, and
//! the errors in Core's order.

use std::sync::Arc;

use dash_consensus::Block;

use super::super::tests::{
    HASH_7, at, block, block_with, call_method, facts, keys, method_err, reply, rp1b_1c_server,
    spend_hex,
};
use crate::UniValue;
use crate::blockchain::BlockReadError;
use crate::protocol::{
    RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RPC_MISC_ERROR,
    RPC_TYPE_ERROR,
};

/// [`spend_hex`] as a version 3 transaction of type `ty`, with `payload` (hex) as its payload
/// field when it has one (`ty != 0`).
fn special(ty: u8, payload: &str) -> String {
    let spend = spend_hex();
    let mut hex = format!("0300{ty:02x}00{}", &spend[8..]);
    if ty != 0 {
        hex.push_str(&format!("{:02x}{payload}", payload.len() / 2));
    }
    hex
}

/// A coinbase of type 5 (`CbTx`), then a plain spend, a version 3 transaction of type 0, two of
/// type 8 and one of type 1.
fn mixed_block() -> Arc<Block> {
    let cbtx = format!("0100{}{}", "07000000", "44".repeat(32));
    let with_cb = block(Some(&cbtx));
    let txs = [spend_hex(), special(0, ""), special(8, "aa"), special(8, "bb"), special(1, "cc")];
    let rest = block_with(&txs);
    let all = std::iter::once(Arc::clone(&with_cb.txs()[0])).chain(rest.txs()[1..].iter().cloned());
    Arc::new(Block::new(*with_cb.header(), all.collect()))
}

fn ids(v: &UniValue) -> Vec<String> {
    match v {
        UniValue::Arr(items) => items.iter().map(|i| i.get_str().unwrap().to_owned()).collect(),
        _ => panic!("not an array: {}", v.write()),
    }
}

/// `HasExtraPayloadField` and the type pick the transactions; `skip` and `count` window them in
/// block order. Verbosity 0 prints txids and 1 the transactions' hex.
// Core: rpc/blockchain.cpp:2530-2553
#[test]
fn getspecialtxes_filters_by_payload_field_and_type_then_skips_and_counts() {
    let (server, live) = rp1b_1c_server(true);
    let b = mixed_block();
    live.lock().unwrap().block = Some(Ok((facts(1), Arc::clone(&b))));
    let txid = |i: usize| b.txs()[i].txid().to_string();
    let call = |rest: &str| {
        let v = call_method(&server, "getspecialtxes", &format!(r#"["{HASH_7}"{rest}]"#)).unwrap();
        ids(&v)
    };
    // Not the plain spend (1), nor the version 3 transaction of type 0 (2).
    assert_eq!(call(""), [0, 3, 4, 5].map(txid));
    assert_eq!(call(", -1, 10, 0, 0"), [0, 3, 4, 5].map(txid));
    assert_eq!(call(", 8"), [3, 4].map(txid));
    assert_eq!(call(", 8, 10, 1"), [txid(4)]);
    assert_eq!(call(", 8, 1"), [txid(3)]);
    assert_eq!(call(", -1, 2, 1"), [3, 4].map(txid));
    assert!(call(", 8, 0").is_empty());
    assert!(call(", -1, 10, 4").is_empty());
    for ty in ["0", "2", "65544", "-2"] {
        assert!(call(&format!(", {ty}")).is_empty(), "{ty}");
    }
    // `skip + count` wraps past `int`'s range, as dashd's build does (D56): nothing; at
    // `INT_MAX` exactly, everything after the skip.
    assert!(call(", -1, 2147483647, 3").is_empty());
    assert_eq!(call(", -1, 2147483644, 3"), [txid(5)]);
    // Null arguments are their defaults.
    assert_eq!(call(", null, null, null, null"), call(""));

    let hex = call(", 8, 1, 0, 1");
    let bytes = dash_consensus::CoreSerialize::core_serialize(b.txs()[3].raw());
    assert_eq!(hex, [bytes.iter().map(|x| format!("{x:02x}")).collect::<String>()]);
    assert_eq!(live.lock().unwrap().calls.first().unwrap(), &format!("block {HASH_7}"));
}

/// Verbosity 2 is `TxToJSON`: the transaction's `TxToUniv` with its hex, then the block's hash,
/// height, depth and time and the lock fields; a block off the active chain has height −1, no
/// depth or time, and no chainlock.
// Core: rpc/rawtransaction.cpp:75-167 TxToJSON
#[test]
fn getspecialtxes_verbosity_2_is_tx_to_json() {
    let (server, live) = rp1b_1c_server(true);
    let b = mixed_block();
    let mut f = facts(1);
    f.confirmations = 3;
    f.chainlock = true;
    live.lock().unwrap().block = Some(Ok((f.clone(), Arc::clone(&b))));
    let arg = format!(r#"["{HASH_7}", 5, 10, 0, 2]"#);
    let json = call_method(&server, "getspecialtxes", &arg).unwrap();
    let tx = at(&json, "0");
    let mut want = keys(&crate::core_write::tx_to_univ(
        &b.txs()[0],
        None,
        true,
        None,
        crate::core_write::TxVerbosity::Details,
        &super::super::tests::tx_json(),
    ));
    want.extend(
        [
            "blockhash",
            "height",
            "confirmations",
            "time",
            "blocktime",
            "instantlock",
            "instantlock_internal",
            "chainlock",
        ]
        .map(str::to_owned),
    );
    assert_eq!(keys(&tx), want);
    assert_eq!(at(&tx, "hex"), UniValue::from(hex_of(&b, 0)));
    assert_eq!(at(&tx, "blockhash"), UniValue::from(f.hash.to_string()));
    assert_eq!(at(&tx, "height").write(), "1");
    assert_eq!(at(&tx, "confirmations").write(), "3");
    assert_eq!(at(&tx, "time").write(), f.header.time.to_string());
    assert_eq!(at(&tx, "blocktime").write(), f.header.time.to_string());
    // No InstantSend manager yet: `instantlock` is the chainlock (D40).
    assert_eq!(at(&tx, "instantlock").write(), "true");
    assert_eq!(at(&tx, "instantlock_internal").write(), "false");
    assert_eq!(at(&tx, "chainlock").write(), "true");
    assert_eq!(at(&tx, "cbTx.height").write(), "7");

    f.confirmations = -1;
    live.lock().unwrap().block = Some(Ok((f, Arc::clone(&b))));
    let tx = at(&call_method(&server, "getspecialtxes", &arg).unwrap(), "0");
    let tail: Vec<String> = keys(&tx).split_off(keys(&tx).len() - 6);
    assert_eq!(
        tail,
        [
            "blockhash",
            "height",
            "confirmations",
            "instantlock",
            "instantlock_internal",
            "chainlock"
        ]
    );
    assert_eq!(at(&tx, "height").write(), "-1");
    assert_eq!(at(&tx, "confirmations").write(), "0");
    assert_eq!(at(&tx, "instantlock").write(), "false");
    assert_eq!(at(&tx, "chainlock").write(), "false");
}

fn hex_of(b: &Block, i: usize) -> String {
    let bytes = dash_consensus::CoreSerialize::core_serialize(b.txs()[i].raw());
    bytes.iter().map(|x| format!("{x:02x}")).collect()
}

/// The chainman first, then each argument in order, then the block's read.
// Core: rpc/blockchain.cpp:2486-2523; :786-804 GetBlockChecked
#[test]
fn getspecialtxes_errors_come_in_cores_order() {
    let (unloaded, _) = rp1b_1c_server(false);
    assert_eq!(
        method_err(&unloaded, "getspecialtxes", r#"["00", 1, -1]"#),
        reply(RPC_INTERNAL_ERROR, "Node chainman not found")
    );
    let (server, live) = rp1b_1c_server(true);
    let err = |rest: &str| method_err(&server, "getspecialtxes", &format!(r#"["{HASH_7}"{rest}]"#));
    assert_eq!(
        method_err(&server, "getspecialtxes", r#"["00", -1, -1]"#),
        reply(RPC_INVALID_PARAMETER, "blockhash must be of length 64 (not 2, for '00')")
    );
    // `RPCHelpMan`'s argument check.
    assert_eq!(err(r#", "1""#).0, RPC_TYPE_ERROR);
    assert_eq!(err(", 2147483648"), reply(RPC_MISC_ERROR, "JSON integer out of range"));
    assert_eq!(err(", 1, -1, -1"), reply(RPC_INVALID_PARAMETER, "Negative count"));
    assert_eq!(err(r#", 1, 1, -1, 9"#), reply(RPC_INVALID_PARAMETER, "Negative skip"));
    let range = reply(RPC_INVALID_PARAMETER, "Verbosity must be in range 0..2");
    assert_eq!(err(", 1, 1, 1, 3"), range);
    assert_eq!(err(", 1, 1, 1, -1"), range);
    assert!(live.lock().unwrap().calls.is_empty(), "no read before the arguments pass");
    for (read, code, message) in [
        (BlockReadError::NotFound, RPC_INVALID_ADDRESS_OR_KEY, "Block not found"),
        (BlockReadError::Pruned, RPC_MISC_ERROR, "Block not available (pruned data)"),
        (BlockReadError::NotOnDisk, RPC_MISC_ERROR, "Block not found on disk"),
    ] {
        live.lock().unwrap().block = Some(Err(read));
        assert_eq!(err(""), reply(code, message));
    }
}
