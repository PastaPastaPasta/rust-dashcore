//! Print the pinned oracle's `oracle_chainparams_json` for a list of chain selections.
//!
//! BS5's `dash-params` corpus generator (`contrib/node/bs5-chainparams/run.py`) runs this to
//! check its Core-linked dump tool against the pinned archives. Input: a JSON file holding an
//! array of `{"name", "chain", "args"}`. Output: one JSON object per line and pin,
//! `{"pin", "name", "params"}` on success or `{"pin", "name", "error"}` with the shim's
//! message (Core's exception text, or `ParseParameters: <error>`).
//!
//! `cargo run -p dash-oracle-sys --features oracle --example chainparams_cases -- <cases.json>`

use dash_oracle_sys::{Chain, Oracle};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    name: String,
    chain: String,
    args: Vec<String>,
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: chainparams_cases <cases.json>");
    let text = std::fs::read_to_string(&path).expect("read cases");
    let cases: Vec<Case> = serde_json::from_str(&text).expect("parse cases");
    for oracle in Oracle::all() {
        for case in &cases {
            let args: Vec<&str> = case.args.iter().map(String::as_str).collect();
            let chain = Chain::new(&case.chain, &args);
            let mut line = serde_json::json!({ "pin": oracle.pin().name(), "name": case.name });
            match oracle.chainparams_json(&chain) {
                Ok(json) => {
                    line["params"] = serde_json::from_str(&json).expect("oracle JSON");
                }
                Err(e) => line["error"] = e.message.into(),
            }
            println!("{line}");
        }
    }
}
