//! CJ1's D17 builders (`tests/v24/coinjoin.rs`) against dashd v24.0.3: the answers of its
//! mempool admission on the three CoinJoin rows the mempool differential replays.
//!
//! Needs dashd v24.0.3: `DASHD_V24_PATH`, else setup-dashd's cache
//! (`DASHVERSION=24.0.3 python3 contrib/setup-dashd.py`). `SKIP_DASHD_TESTS` skips it.

mod v24;

use serde_json::{Value, json};

use v24::coinjoin::{
    COLLATERAL_AMOUNT, COLLATERAL_FEE, EXCESSIVE_FEE, MAX_RAW_TX_FEE, PRIORITISE_DELTA, amount,
    excessive_collateral, fund, mine, signed_mix,
};
use v24::{Dashd, dashd_binary, hex};

/// dashd's reject reason for the zero-fee mixing tx without a delta.
const MIN_RELAY_FEE_REASON: &str = "min relay fee not met";

/// An RPC amount in duffs.
fn duffs(value: &Value) -> u64 {
    (value.as_f64().expect("an amount") * 1e8).round() as u64
}

fn accept(dashd: &Dashd, tx: &[u8], maxfeerate: Option<u64>) -> Value {
    let params = match maxfeerate {
        Some(rate) => json!([[hex(tx)], rate]),
        None => json!([[hex(tx)]]),
    };
    dashd.call("testmempoolaccept", params)[0].clone()
}

#[test]
fn cj1_d17_rows_on_dashd() {
    let Some(binary) = dashd_binary() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let dashd = Dashd::start_with(&binary, &dir.path().join("dashd"), &["-fallbackfee=0.00001"]);
    dashd.call("createwallet", json!(["cj1"]));
    mine(&dashd, 101);

    let clients = fund(&dashd, 3);
    let excessive = excessive_collateral(&dashd);
    let mix = signed_mix(&dashd, &clients);

    let collateral = &clients[0].collateral;
    let answer = accept(&dashd, collateral, None);
    eprintln!("row a collateral: {answer}");
    assert_eq!(answer["allowed"], json!(true), "{answer}");
    assert_eq!(duffs(&answer["fees"]["base"]), COLLATERAL_FEE);
    assert_eq!(amount(COLLATERAL_FEE), "0.00010000");
    let txid = dashd.call("sendrawtransaction", json!([hex(collateral)]));
    assert_eq!(txid, answer["txid"]);

    let answer = accept(&dashd, &excessive, Some(0));
    eprintln!("row a excessive, maxfeerate=0: {answer}");
    assert_eq!(answer["allowed"], json!(true), "{answer}");
    assert_eq!(duffs(&answer["fees"]["base"]), EXCESSIVE_FEE);
    assert_eq!(amount(EXCESSIVE_FEE), "0.10000001");
    const { assert!(COLLATERAL_FEE == COLLATERAL_AMOUNT) };
    const { assert!(EXCESSIVE_FEE > MAX_RAW_TX_FEE) };
    let answer = accept(&dashd, &excessive, None);
    eprintln!("row a excessive, default maxfeerate: {answer}");
    assert_eq!(answer["allowed"], json!(false), "{answer}");
    assert_eq!(answer["reject-reason"], json!("max-fee-exceeded"));

    let answer = accept(&dashd, &mix, None);
    eprintln!("row c mix without delta: {answer}");
    assert_eq!(answer["allowed"], json!(false), "{answer}");
    assert_eq!(answer["reject-reason"], json!(MIN_RELAY_FEE_REASON));

    let mix_txid = answer["txid"].clone();
    assert_eq!(
        dashd.call("prioritisetransaction", json!([mix_txid, PRIORITISE_DELTA])),
        json!(true)
    );
    let answer = accept(&dashd, &mix, None);
    eprintln!("row b mix with delta: {answer}");
    assert_eq!(answer["allowed"], json!(true), "{answer}");
    assert_eq!(duffs(&answer["fees"]["base"]), 0);
    let sent = dashd.call("sendrawtransaction", json!([hex(&mix)]));
    assert_eq!(sent, mix_txid);
    let entry = dashd.call("getmempoolentry", json!([mix_txid]));
    eprintln!("row b mempool entry: {entry}");
    assert_eq!(duffs(&entry["fees"]["base"]), 0);
    assert_eq!(duffs(&entry["fees"]["modified"]), PRIORITISE_DELTA as u64);
    assert_eq!(amount(duffs(&entry["fees"]["modified"])), "0.10000000");
}
