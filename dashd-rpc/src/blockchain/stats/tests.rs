use std::sync::Arc;

use super::{percentiles_by_size, truncated_median};
use crate::UniValue;
use crate::blockchain::tests::{
    HASH_7, SPEND_OUT, block, block_with, call_method, facts, keys, live_server, method_err, reply,
    spend_hex, spend_undo,
};
use crate::protocol::{
    RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RPC_MISC_ERROR,
};

/// Core's `rpc_getblockstats_calculate_percentiles_by_size`, case for case.
// Core: test/rpc_tests.cpp:411-485
#[test]
fn core_percentiles_by_size_vectors() {
    let mut scores: Vec<(i64, i64)> =
        (0..100).map(|_| (1, 1)).chain((0..100).map(|_| (2, 1))).collect();
    assert_eq!(percentiles_by_size(&mut scores, 200), [1, 1, 1, 2, 2]);
    // More pairs, two of them overlapping two percentiles each.
    let mut scores = vec![(1, 9), (2, 16), (4, 50), (5, 10), (9, 15)];
    assert_eq!(percentiles_by_size(&mut scores, 100), [2, 2, 4, 4, 9]);
    // The same, with one of the overlapping pairs split in two.
    let mut scores = vec![(1, 9), (2, 11), (2, 5), (4, 50), (5, 10), (9, 15)];
    assert_eq!(percentiles_by_size(&mut scores, 100), [2, 2, 4, 4, 9]);
    // One transaction spanning every percentile.
    let mut scores = vec![(1, 100), (2, 1), (3, 1), (3, 1), (999_999, 1)];
    assert_eq!(percentiles_by_size(&mut scores, 104), [1; 5]);
    // No scores leaves zeros; percentiles past the last score take its feerate.
    assert_eq!(percentiles_by_size(&mut [], 10), [0; 5]);
    assert_eq!(percentiles_by_size(&mut [(3, 1), (7, 1)], 10), [3, 7, 7, 7, 7]);
}

#[test]
fn truncated_median_rounds_the_middle_pair_toward_zero() {
    assert_eq!(truncated_median(&mut []), 0);
    assert_eq!(truncated_median(&mut [5, 1, 3]), 3);
    assert_eq!(truncated_median(&mut [4, 1, 3, 10]), 3);
    assert_eq!(truncated_median(&mut [-3, 0]), -1);
}

/// Core's statistic names in the order `getblockstats` prints them all.
const ALL: [&str; 27] = [
    "avgfee",
    "avgfeerate",
    "avgtxsize",
    "blockhash",
    "feerate_percentiles",
    "height",
    "ins",
    "maxfee",
    "maxfeerate",
    "maxtxsize",
    "medianfee",
    "mediantime",
    "mediantxsize",
    "minfee",
    "minfeerate",
    "mintxsize",
    "outs",
    "subsidy",
    "time",
    "total_out",
    "total_size",
    "totalfee",
    "txs",
    "utxo_increase",
    "utxo_size_inc",
    "utxo_increase_actual",
    "utxo_size_inc_actual",
];

#[test]
fn getblockstats_sums_a_block_and_its_undo_as_core_does() {
    let (server, live) = live_server(8);
    let f = facts(7);
    live.lock().unwrap().block = Some(Ok((f.clone(), block_with(&[spend_hex()]))));
    live.lock().unwrap().undo = Some(spend_undo(3));
    live.lock().unwrap().subsidy = Some(5_000_000_000);

    let json = call_method(&server, "getblockstats", "[7]").unwrap();
    assert_eq!(keys(&json), ALL);
    let size = (spend_hex().len() / 2) as i64;
    let fee = 10_000;
    let rate = fee / size;
    // The coinbase pays 0 to OP_TRUE: 10 bytes plus PER_UTXO_OVERHEAD (41). The spend pays
    // and spends a P2PKH output: 34 + 41 bytes each way.
    let want = format!(
        concat!(
            r#"{{"avgfee":{fee},"avgfeerate":{rate},"avgtxsize":{size},"blockhash":"{hash}","#,
            r#""feerate_percentiles":[{rate},{rate},{rate},{rate},{rate}],"height":7,"ins":1,"#,
            r#""maxfee":{fee},"maxfeerate":{rate},"maxtxsize":{size},"medianfee":{fee},"#,
            r#""mediantime":1296688601,"mediantxsize":{size},"minfee":{fee},"minfeerate":{rate},"#,
            r#""mintxsize":{size},"outs":2,"subsidy":5000000000,"time":1296688602,"#,
            r#""total_out":{out},"total_size":{size},"totalfee":{fee},"txs":2,"utxo_increase":1,"#,
            r#""utxo_size_inc":51,"utxo_increase_actual":1,"utxo_size_inc_actual":51}}"#
        ),
        fee = fee,
        rate = rate,
        size = size,
        hash = f.hash,
        out = SPEND_OUT,
    );
    assert_eq!(json.write(), want);
    // A height is read from the active chain; the block and its undo by the hash it names.
    assert_eq!(
        live.lock().unwrap().calls,
        [format!("block {HASH_7}"), format!("undo {HASH_7}"), format!("subsidy {HASH_7}")]
    );

    // By hash: `LookupBlockIndex` first, then the same reads.
    live.lock().unwrap().indexed = true;
    live.lock().unwrap().calls.clear();
    let by_hash = call_method(&server, "getblockstats", &format!(r#"["{HASH_7}"]"#)).unwrap();
    assert_eq!(by_hash.write(), want);
    assert_eq!(live.lock().unwrap().calls[0], format!("lookup {HASH_7}"));

    // A selection prints in `std::set` order, each name once.
    let picked = call_method(
        &server,
        "getblockstats",
        r#"[7, ["txs", "avgfee", "txs", "feerate_percentiles"]]"#,
    )
    .unwrap();
    assert_eq!(
        picked.write(),
        format!(
            r#"{{"avgfee":{fee},"feerate_percentiles":[{rate},{rate},{rate},{rate},{rate}],"txs":2}}"#
        )
    );
    for stat in ALL {
        let one = call_method(&server, "getblockstats", &format!(r#"[7, ["{stat}"]]"#)).unwrap();
        let UniValue::Obj(fields) = &json else {
            panic!()
        };
        let want = &fields.iter().find(|(k, _)| k == stat).unwrap().1;
        assert_eq!(one.write(), format!(r#"{{"{stat}":{}}}"#, want.write()), "{stat}");
    }
    // The first invalid name in set order is the error.
    for stats in [
        r#"["asdfghjkl"]"#,
        r#"["minfee", "asdfghjkl"]"#,
        r#"["asdfghjkl", "minfee"]"#,
        r#"["minfee", "asdfghjkl", "maxfee"]"#,
    ] {
        assert_eq!(
            method_err(&server, "getblockstats", &format!("[7, {stats}]")),
            reply(RPC_INVALID_PARAMETER, "Invalid selected statistic 'asdfghjkl'"),
            "{stats}"
        );
    }
    assert_eq!(
        method_err(&server, "getblockstats", r#"[7, ["minfee", "aaaasdfghjkl"]]"#),
        reply(RPC_INVALID_PARAMETER, "Invalid selected statistic 'aaaasdfghjkl'")
    );
}

#[test]
fn getblockstats_refuses_as_core_in_its_order() {
    let (server, live) = live_server(8);
    assert_eq!(
        method_err(&server, "getblockstats", "[8]"),
        reply(RPC_INVALID_PARAMETER, "Target block height 8 after current tip 7")
    );
    assert_eq!(
        method_err(&server, "getblockstats", "[-1]"),
        reply(RPC_INVALID_PARAMETER, "Target block height -1 is negative")
    );
    // An unknown hash fails before the stats are read or any block is.
    assert_eq!(
        method_err(&server, "getblockstats", &format!(r#"["{HASH_7}", [1]]"#)),
        reply(RPC_INVALID_ADDRESS_OR_KEY, "Block not found")
    );
    assert_eq!(live.lock().unwrap().calls, [format!("lookup {HASH_7}")]);
    // A stat that is not a string is get_str's type error, before the block is read.
    assert_eq!(method_err(&server, "getblockstats", "[7, [1]]").0, crate::protocol::RPC_TYPE_ERROR);

    let f = facts(7);
    live.lock().unwrap().block = Some(Ok((f, block_with(&[spend_hex()]))));
    live.lock().unwrap().subsidy = Some(1);
    live.lock().unwrap().undo_pruned = true;
    assert_eq!(
        method_err(&server, "getblockstats", "[7]"),
        reply(RPC_MISC_ERROR, "Undo data not available (pruned data)")
    );
    live.lock().unwrap().undo_pruned = false;
    assert_eq!(
        method_err(&server, "getblockstats", "[7]"),
        reply(RPC_MISC_ERROR, "Can't read undo data from disk")
    );
    // Undo data short of a transaction is `vtxundo.at(i - 1)`'s out_of_range, for the
    // statistics that read the inputs only.
    live.lock().unwrap().undo = Some(dash_consensus::coins::BlockUndo {
        txs: vec![],
    });
    assert_eq!(
        method_err(&server, "getblockstats", "[7]"),
        reply(
            RPC_MISC_ERROR,
            "vector::_M_range_check: __n (which is 0) >= this->size() (which is 0)"
        )
    );
    let sized = call_method(&server, "getblockstats", r#"[7, ["total_size", "ins"]]"#).unwrap();
    assert_eq!(sized.write(), format!(r#"{{"ins":1,"total_size":{}}}"#, spend_hex().len() / 2));

    // `GetBlockChecked`'s errors, as getblock's.
    live.lock().unwrap().block = Some(Err(crate::blockchain::BlockReadError::Pruned));
    assert_eq!(
        method_err(&server, "getblockstats", "[7]"),
        reply(RPC_MISC_ERROR, "Block not available (pruned data)")
    );
}

#[test]
fn getblockstats_of_the_genesis_block_reads_no_undo() {
    let (server, live) = live_server(1);
    live.lock().unwrap().block = Some(Ok((facts(0), block(None))));
    live.lock().unwrap().subsidy = Some(50);
    let json = call_method(&server, "getblockstats", "[0]").unwrap();
    // The genesis coinbase does not change the UTXO set's counts.
    assert_eq!(
        crate::request::find_value(
            match &json {
                UniValue::Obj(f) => f,
                _ => panic!(),
            },
            "utxo_size_inc"
        )
        .write(),
        "51"
    );
    let UniValue::Obj(fields) = &json else {
        panic!()
    };
    let get = |k: &str| crate::request::find_value(fields, k).write();
    assert_eq!(
        [get("utxo_increase"), get("utxo_increase_actual"), get("utxo_size_inc_actual")],
        ["1", "0", "0"]
    );
    assert_eq!(
        [get("minfee"), get("minfeerate"), get("mintxsize"), get("avgfee"), get("txs")],
        ["0", "0", "0", "0", "1"]
    );
    assert!(!live.lock().unwrap().calls.iter().any(|c| c.starts_with("undo")));
    let _ = Arc::clone(&live);
}

#[test]
fn pruneblockchain_is_refused_outside_prune_mode() {
    let (server, _) = live_server(1);
    assert_eq!(
        method_err(&server, "pruneblockchain", "[1]"),
        reply(RPC_MISC_ERROR, "Cannot prune blocks because node is not in prune mode.")
    );
    // The height's type is the argument check's, before the handler runs.
    assert_eq!(
        method_err(&server, "pruneblockchain", r#"["1"]"#).0,
        crate::protocol::RPC_TYPE_ERROR
    );
    let unloaded = crate::blockchain::tests::unloaded_server();
    assert_eq!(
        method_err(&unloaded, "pruneblockchain", "[1]"),
        reply(RPC_INTERNAL_ERROR, "Node chainman not found")
    );
}

/// The first asset unlock (a Platform transfer) in RP1a-2c's dashd fixtures, with its
/// `assetUnlockTx.fee` and `extraPayloadSize` as dashd printed them.
fn asset_unlock() -> (String, i64, usize) {
    use std::io::BufRead as _;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/txtouniv/decoderawtransaction-v24.0.0-rc.4.jsonl.gz"
    );
    let file = std::fs::File::open(path).expect("the fixture");
    for line in std::io::BufReader::new(flate2::read::GzDecoder::new(file)).lines() {
        let row = UniValue::read(line.unwrap().as_bytes()).expect("a JSON line");
        let UniValue::Obj(row) = row else {
            panic!("an object")
        };
        let hex = crate::request::find_value(&row, "hex").get_str().unwrap().to_owned();
        let tx = crate::mining::decode_hex_tx(&hex).expect("dashd's transaction");
        if !tx.is_platform_transfer() {
            continue;
        }
        let reply = crate::request::find_value(&row, "reply").get_str().unwrap();
        let UniValue::Obj(reply) = UniValue::read(reply.as_bytes()).unwrap() else {
            panic!()
        };
        let UniValue::Obj(result) = crate::request::find_value(&reply, "result") else {
            panic!()
        };
        let UniValue::Obj(unlock) = crate::request::find_value(result, "assetUnlockTx") else {
            panic!("an assetUnlockTx")
        };
        let fee = crate::request::get_int_i64(crate::request::find_value(unlock, "fee")).unwrap();
        let size =
            crate::request::get_int_u64(crate::request::find_value(result, "extraPayloadSize"))
                .unwrap() as usize;
        return (hex, fee, size);
    }
    panic!("the fixture has an asset unlock")
}

/// A Platform transfer pays its payload's fee, not inputs less outputs (it has no inputs); a
/// payload `GetTxPayload` cannot read whole is `CHECK_NONFATAL(payload)`.
// Core: rpc/blockchain.cpp:2380-2386
#[test]
fn getblockstats_takes_a_platform_transfers_fee_from_its_payload() {
    let (server, live) = live_server(8);
    let (hex, fee, size) = asset_unlock();
    live.lock().unwrap().block = Some(Ok((facts(7), block_with(std::slice::from_ref(&hex)))));
    live.lock().unwrap().undo = Some(dash_consensus::coins::BlockUndo {
        txs: vec![dash_consensus::coins::TxUndo {
            coins: vec![],
        }],
    });
    live.lock().unwrap().subsidy = Some(1);
    let json = call_method(&server, "getblockstats", r#"[7, ["totalfee", "minfee", "ins"]]"#);
    assert_eq!(json.unwrap().write(), format!(r#"{{"ins":0,"minfee":{fee},"totalfee":{fee}}}"#));

    // One byte past the payload: the length grows by one and a zero follows.
    assert!(size < 0xfc, "a one-byte length");
    let at = hex.len() - 2 * size - 2;
    let longer = format!("{}{:02x}{}00", &hex[..at], size + 1, &hex[at + 2..]);
    live.lock().unwrap().block = Some(Ok((facts(7), block_with(&[longer]))));
    assert_eq!(
        method_err(&server, "getblockstats", "[7]"),
        reply(RPC_MISC_ERROR, "Internal bug detected: payload")
    );
    // Without the fee statistics the payload is not read.
    let txs = call_method(&server, "getblockstats", r#"[7, ["txs"]]"#).unwrap();
    assert_eq!(txs.write(), r#"{"txs":2}"#);
}

/// An unspendable output grows `utxo_increase` and `utxo_size_inc` but not their `_actual`
/// counterparts: it never enters the UTXO set.
// Core: rpc/blockchain.cpp:2333-2341
#[test]
fn getblockstats_leaves_unspendable_outputs_out_of_the_actual_utxo_counts() {
    let (server, live) = live_server(8);
    // The spend, with a second output: 0 duffs to `OP_RETURN 21` (8 + 1 + 3 bytes, plus 41).
    let spend = spend_hex();
    let (head, tail) = spend.split_at(spend.len() - 8);
    let outputs_at = head.len() - 2 * (8 + 1 + 25) - 2;
    let spend = format!(
        "{}02{}{}036a0121{tail}",
        &head[..outputs_at],
        &head[outputs_at + 2..],
        "00".repeat(8)
    );
    live.lock().unwrap().block = Some(Ok((facts(7), block_with(&[spend]))));
    live.lock().unwrap().undo = Some(spend_undo(3));
    live.lock().unwrap().subsidy = Some(1);
    let json = call_method(
        &server,
        "getblockstats",
        r#"[7, ["utxo_increase", "utxo_increase_actual", "utxo_size_inc", "utxo_size_inc_actual"]]"#,
    );
    // The coinbase's 51 bytes, the P2PKH output's 75 in and out, the OP_RETURN's 53.
    assert_eq!(
        json.unwrap().write(),
        r#"{"utxo_increase":2,"utxo_increase_actual":1,"utxo_size_inc":104,"utxo_size_inc_actual":51}"#
    );
}
