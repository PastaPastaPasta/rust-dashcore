//! Undo records Dash Core wrote (CS5a), through the `diff_undo_decode` and `diff_coin_decode`
//! comparisons on every linked pin: the regtest chains `contrib/node/coins-undo-vectors.py` recorded,
//! and every record of mainnet's `rev00000.dat` (blocks 1–1300). Each spent coin's database
//! serialization goes through `diff_coin_decode` too.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::use_debug,
    clippy::dbg_macro,
    clippy::print_stdout
)]

#[path = "../../dash-consensus/tests/support/core_undo.rs"]
mod core_undo;

use dash_consensus::coins::decode_block_undo;
use dash_oracle_sys::Pin;
use node_l1::{Compared, Outcome, compare_block_undo, compare_coin_decode};

use core_undo::{mainnet_undo_records, regtest};

/// Compares `bytes` on every linked pin and requires that each decodes.
fn accepted(
    compare: fn(&[u8], Pin) -> Compared,
    bytes: &[u8],
    what: &str,
    failures: &mut Vec<String>,
) {
    for pin in Pin::ALL {
        match compare(bytes, pin) {
            Ok(Outcome::BothAccept) => {}
            Ok(Outcome::BothReject | Outcome::StrictFlag) => {
                failures.push(format!("{what}: rejected on {}", pin.name()))
            }
            Ok(Outcome::OutOfDomain | Outcome::Discarded) => {
                failures.push(format!("{what}: not compared on {}", pin.name()))
            }
            Err(divergence) => failures.push(format!("{what}: {divergence} ({})", pin.name())),
        }
    }
}

#[test]
fn core_records_agree_with_core() {
    let mut records: Vec<(String, Vec<u8>)> = Vec::new();
    for v in regtest("v24.0") {
        records.push((format!("regtest v24.0 height {}", v.height), v.undo));
    }
    for (i, (undo, _)) in mainnet_undo_records().into_iter().enumerate() {
        records.push((format!("mainnet record {i}"), undo));
    }

    let mut failures = Vec::new();
    let mut coins = 0;
    for (what, undo) in &records {
        accepted(compare_block_undo, undo, what, &mut failures);
        let Ok(decoded) = decode_block_undo(&mut &undo[..]) else {
            failures.push(format!("{what}: does not decode"));
            continue;
        };
        for coin in decoded.value().txs.iter().flat_map(|tx| &tx.coins) {
            accepted(compare_coin_decode, &coin.serialize(), what, &mut failures);
            coins += 1;
        }
    }
    node_sink::report(&format!(
        "core undo records: {} records, {coins} coins, {} failures",
        records.len(),
        failures.len()
    ));
    assert!(failures.is_empty(), "{}", failures.join("; "));
    assert!(records.len() > 1300 && coins > 0);
}
