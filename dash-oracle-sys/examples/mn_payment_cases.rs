//! Print the pinned oracle's `oracle_mn_payment` at mainnet, classic reward era.
//!
//! BS5's corpus generator (`contrib/node/bs5-chainparams/run.py`) runs this for the BRR table
//! fixture. Input: a JSON file holding an array of `{"height", "block_value"}`. Output: one JSON
//! object per line and pin, `{"pin", "height", "block_value", "payment"}`, or `"error"` with the
//! shim's message.
//!
//! `cargo run -p dash-oracle-sys --features oracle --example mn_payment_cases -- <cases.json>`

use dash_oracle_sys::{Chain, Oracle, RewardEra};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    height: i32,
    block_value: i64,
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: mn_payment_cases <cases.json>");
    let text = std::fs::read_to_string(&path).expect("read cases");
    let cases: Vec<Case> = serde_json::from_str(&text).expect("parse cases");
    for oracle in Oracle::all() {
        for case in &cases {
            let mut line = serde_json::json!({
                "pin": oracle.pin().name(),
                "height": case.height,
                "block_value": case.block_value,
            });
            match oracle.mn_payment(
                &Chain::main(),
                case.height,
                case.block_value,
                RewardEra::Classic,
            ) {
                Ok(payment) => line["payment"] = payment.into(),
                Err(e) => line["error"] = e.message.into(),
            }
            println!("{line}");
        }
    }
}
