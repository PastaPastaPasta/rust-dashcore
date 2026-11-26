//! CJ1-W6's D17 differential: the CoinJoin server's mempool (`NodeMempool`) over rdashd's live
//! pool against dashd v24 on one chain. dashd builds the rows as `cj1_builders_dashd` does
//! (collateral, excessive collateral, the zero-fee mix without and with the +0.1 DASH delta);
//! its blocks go to an in-process V. Each row's answer on both is compared: whether ATMP takes
//! it, its base fee, `ATMPIfSaneFee`'s verdict, and the modified fee of what was submitted.
//!
//! Needs dashd v24: `DASHD_V24_PATH`, else setup-dashd's cache (see `tests/v24/mod.rs`).
//! `SKIP_DASHD_TESTS` skips it.

mod v24;

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use dash_coinjoin::collateral::{MempoolAccept, test_accept_if_sane_fee};
use dash_coinjoin::dstx::txid;
use dash_consensus::primitives::{decode_block, decode_tx_exact};
use dash_primitives::Transaction;
use dashd_coinjoin::env::AsyncMempool;
use dashd_node::coinjoin::mempool::NodeMempool;
use dashd_node::logging::Logger;
use dashd_node::sut::{Harness, NodeSut};
use dashd_node::ui::Ui;
use serde_json::{Value, json};
use v24::coinjoin::{
    COLLATERAL_FEE, EXCESSIVE_FEE, MAX_RAW_TX_FEE, PRIORITISE_DELTA, excessive_collateral, fund,
    mine, signed_mix,
};
use v24::{Dashd, T0, dashd_binary, hex, unhex};

/// One row: its name, the transaction, and the priority delta given before it.
struct Row {
    name: &'static str,
    tx: Vec<u8>,
    delta: Option<i64>,
}

/// A node's answer on one row.
#[derive(Debug, PartialEq, Eq)]
struct Answer {
    /// ATMP's `test_accept` with no fee-rate cap.
    allowed: bool,
    /// Its base fee, when allowed.
    base: Option<i64>,
    /// `ATMPIfSaneFee(tx, true)`: allowed with a base fee within `maxTxFee`.
    sane: bool,
    /// The pool entry's modified fee after the submit of a sane row.
    modified: Option<i64>,
}

fn duffs(value: &Value) -> i64 {
    (value.as_f64().expect("an amount") * 1e8).round() as i64
}

/// dashd's answer. `ATMPIfSaneFee` has no RPC: its verdict is Core's test over the RPC's.
// Core: coinjoin/coinjoin.cpp:440-456
fn on_dashd(dashd: &Dashd, row: &Row) -> Answer {
    let id = dashd.call("decoderawtransaction", json!([hex(&row.tx)]))["txid"].clone();
    if let Some(delta) = row.delta {
        assert_eq!(dashd.call("prioritisetransaction", json!([id, delta])), json!(true));
    }
    let test = dashd.call("testmempoolaccept", json!([[hex(&row.tx)], 0]))[0].clone();
    eprintln!("dashd {}: {test}", row.name);
    let allowed = test["allowed"] == json!(true);
    let base = allowed.then(|| duffs(&test["fees"]["base"]));
    let sane = base.is_some_and(|fee| fee <= MAX_RAW_TX_FEE as i64);
    let modified = sane.then(|| {
        assert_eq!(dashd.call("sendrawtransaction", json!([hex(&row.tx)])), id);
        let entry = dashd.call("getmempoolentry", json!([id]));
        eprintln!("dashd {} entry: {entry}", row.name);
        duffs(&entry["fees"]["modified"])
    });
    Answer {
        allowed,
        base,
        sane,
        modified,
    }
}

/// rdashd's answer through the server's mempool: its awaited prioritise, test-accept and submit,
/// and the blocking sane-fee test the server's collateral checks make.
fn on_rdashd(sut: &NodeSut, mempool: &NodeMempool, row: &Row) -> Answer {
    let tx = decode(&row.tx);
    let id = txid(&tx);
    if let Some(delta) = row.delta {
        assert_eq!(wait(AsyncMempool::prioritise(mempool, id, delta)), Some(()));
    }
    let base = wait(AsyncMempool::test_accept(mempool, tx.clone())).expect("the pool answers");
    assert_eq!(MempoolAccept::test_accept(mempool, &tx), base, "{}: blocking", row.name);
    let sane = test_accept_if_sane_fee(mempool, &tx);
    let modified = sane.then(|| {
        assert_eq!(wait(AsyncMempool::submit(mempool, tx)), Some(true), "{}", row.name);
        sut.pool_modified_fee(&id).expect("pooled")
    });
    Answer {
        allowed: base.is_some(),
        base,
        sane,
        modified,
    }
}

fn decode(bytes: &[u8]) -> Transaction {
    decode_tx_exact(bytes).expect("a transaction").into_value().raw().clone()
}

/// Polls `f` to completion on this thread.
fn wait<F: Future>(f: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut f = pin!(f);
    loop {
        if let Poll::Ready(out) = f.as_mut().poll(&mut Context::from_waker(&waker)) {
            return out;
        }
        std::thread::park();
    }
}

/// dashd's blocks 1..=tip through V's `ProcessNewBlock`, until V's tip is dashd's.
fn sync(dashd: &Dashd, sut: &NodeSut) {
    let v = sut.chain().expect("V runs");
    let height = dashd.call("getblockcount", json!([])).as_u64().unwrap();
    for h in 1..=height {
        let hash = dashd.call("getblockhash", json!([h]));
        let raw = unhex(dashd.call("getblock", json!([hash, 0])).as_str().unwrap());
        let block = Arc::new(decode_block(&mut raw.as_slice()).expect("a block").into_value());
        let r = v.process_block(block, true, None, true).recv().expect("V");
        assert!(r.accepted && r.state.is_valid(), "V at {h}: {r:?}");
    }
    let tip = dashd.call("getbestblockhash", json!([]));
    let deadline = Instant::now() + Duration::from_secs(60);
    while json!(v.view().snap.tip.tip_hash.to_string()) != tip {
        assert!(Instant::now() < deadline, "V reaches dashd's tip {tip}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn cj1_d17_server_mempool_matches_dashd() {
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
    assert_eq!(dashd.call("getrawmempool", json!([])), json!([]), "every parent is mined");

    let sut_dir = dir.path().join("rdashd");
    std::fs::create_dir_all(&sut_dir).unwrap();
    let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
    let harness = Harness {
        live_mempool: true,
        ..Harness::default()
    };
    let sut = NodeSut::with_harness(&sut_dir, &["-regtest", "-printtoconsole=0"], T0, ui, harness)
        .expect("the SUT starts");
    sync(&dashd, &sut);
    let mempool = sut.coinjoin_mempool().expect("the pool and coins are loaded");

    let rows = [
        Row {
            name: "a collateral",
            tx: clients[0].collateral.clone(),
            delta: None,
        },
        Row {
            name: "a excessive collateral",
            tx: excessive,
            delta: None,
        },
        Row {
            name: "c mix without delta",
            tx: mix.clone(),
            delta: None,
        },
        Row {
            name: "b mix with delta",
            tx: mix,
            delta: Some(PRIORITISE_DELTA),
        },
    ];
    let core: Vec<Answer> = rows.iter().map(|row| on_dashd(&dashd, row)).collect();
    let ours: Vec<Answer> = rows.iter().map(|row| on_rdashd(&sut, &mempool, row)).collect();
    for (n, row) in rows.iter().enumerate() {
        eprintln!("{}: dashd {:?}, rdashd {:?}", row.name, core[n], ours[n]);
        assert_eq!(ours[n], core[n], "{}", row.name);
    }
    let fee = |fee: u64| Some(fee as i64);
    assert_eq!(
        core,
        [
            Answer {
                allowed: true,
                base: fee(COLLATERAL_FEE),
                sane: true,
                modified: fee(COLLATERAL_FEE),
            },
            Answer {
                allowed: true,
                base: fee(EXCESSIVE_FEE),
                sane: false,
                modified: None,
            },
            Answer {
                allowed: false,
                base: None,
                sane: false,
                modified: None,
            },
            Answer {
                allowed: true,
                base: Some(0),
                sane: true,
                modified: Some(PRIORITISE_DELTA),
            },
        ],
        "the rows are D17's"
    );
}
