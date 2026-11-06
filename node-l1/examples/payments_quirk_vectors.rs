//! Writes `dash-evo/tests/data/quirk-14-v24.0.json`: Core's value check (`IsBlockValueValid`,
//! `oracle_block_value_payee`) on the payments fixture's budget and superblock blocks, which
//! `dash-evo/tests/quirk_14_old_budget_and_superblock_checks.rs` replays without the oracle.
//!
//! - The old-budget samples (before, in and outside a budget window), paying the mined coinbase,
//!   the reward and a duff more: in the window, unbounded where no superblock check is required.
//! - Mainnet's and testnet's superblocks (testnet's paid an empty budget: its coinbase gains a
//!   1 DASH payment to the miner's address), paying the mined coinbase and a duff over the
//!   superblock maximum, with no trigger, the trigger, one paying a duff more, and one repeating
//!   its first payment: the payments are checked only synced, not ChainLocked at the height, and
//!   with governance loaded.
//!
//! Each in initial sync, synced, ChainLocked at the height and just below, and synced without
//! governance; testnet's also with V24 active. Our build must agree with Core on every vector;
//! the governance answers are `governance_probe`'s.
//!
//! `cargo run -p node-l1 --features oracle --example payments_quirk_vectors`

use dash_dml::testkit::replay::mainnet;
use dash_oracle_sys::{Oracle, Pin, ThresholdState};
use node_l1::payments::{
    Case, Payments, compare_case_record, governance_probe, sample_index, superblock_case,
};
use serde_json::{Value, json};

fn index(name: &str) -> usize {
    sample_index(name).expect("a fixture sample")
}

fn superblock(name: &str) -> (Case, Payments) {
    superblock_case(index(name)).expect("a superblock sample")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A change to a trigger's payments.
type Edit = fn(&mut Payments);

/// The node states a vector is recorded in.
const STATES: [&str; 5] = ["ibd", "synced", "locked", "locked-below", "no-governance"];

fn in_state(mut case: Case, state: &str) -> Case {
    let h = case.sample_height();
    case.pay.synced = state != "ibd";
    case.pay.governance_loaded = state != "no-governance";
    case.pay.best_chainlock_height = match state {
        "locked" => Some(h),
        "locked-below" => Some(h - 1),
        _ => None,
    };
    case
}

/// `case`'s coinbase changed to pay `total`: an increase on its first output, a decrease taken
/// from its outputs in turn.
fn paying(mut case: Case, total: i64) -> Case {
    let paid: i64 = case.coinbase.outputs.iter().map(|o| o.value).sum();
    let mut cut = paid - total;
    if cut < 0 {
        case.coinbase.outputs[0].value -= cut;
    }
    for o in &mut case.coinbase.outputs {
        let take = cut.clamp(0, o.value);
        o.value -= take;
        cut -= take;
    }
    case
}

/// The vector of `case`, if Core gives a result; ours must agree with it.
fn vector(name: String, case: &Case) -> Option<Value> {
    let fail = |d: node_l1::Divergence| -> ! { panic!("{name}: divergence {d}") };
    let Some(core) = compare_case_record(case).unwrap_or_else(|d| fail(d)).1 else {
        eprintln!("{name}: out of Core's domain");
        return None;
    };
    let gov = governance_probe(case).unwrap_or_else(|d| fail(d))?;
    let s = case.sample();
    let outs = |outs: Vec<(i64, &[u8])>| -> Vec<Value> {
        outs.into_iter().map(|(value, script)| json!([value, hex(script)])).collect()
    };
    Some(json!({
        "name": name,
        "chain": if std::ptr::eq(s.net.params, mainnet()) { "main" } else { "test" },
        "height": s.height,
        "block_reward": core.block_subsidy + case.pay.fee_reward,
        "synced": case.pay.synced,
        "chainlocked": case.pay.best_chainlock_height.is_some_and(|cl| cl >= s.height),
        "v24": case.v24 == ThresholdState::Active,
        "governance_loaded": case.pay.governance_loaded,
        "triggered": gov.triggered,
        "superblock": outs(gov.superblock.iter().map(|o| (o.value, &o.script_pubkey[..])).collect()),
        "outputs": outs(
            case.coinbase.outputs.iter().map(|o| (o.value, o.script_pubkey.as_bytes())).collect()
        ),
        "check": format!("{:?}", core.check),
        "value_valid": core.value_valid,
        "value_error": core.value_error,
    }))
}

fn main() -> std::io::Result<()> {
    let oracle = Oracle::new(Pin::V24_0);
    let mut vectors = Vec::new();
    let mut skipped = 0;
    let mut push = |name: String, case: Case| match vector(name, &case) {
        Some(v) => vectors.push(v),
        None => skipped += 1,
    };

    for sample in ["pre-budget", "budget-window", "budget-outside"] {
        let reward = {
            let case = Case::mined(index(sample)).expect("the sample");
            let (_, core) = compare_case_record(&case).expect("agreement");
            core.expect("a record").block_subsidy + case.pay.fee_reward
        };
        for state in STATES {
            for (paid, total) in
                [("mined", None), ("reward", Some(reward)), ("reward+1", Some(reward + 1))]
            {
                let mut case = Case::mined(index(sample)).expect("the sample");
                if let Some(total) = total {
                    case = paying(case, total);
                }
                push(format!("{sample} {state} pays {paid}"), in_state(case, state));
            }
        }
    }

    for sample in ["superblock", "testnet-superblock"] {
        let (base, _) = superblock(sample);
        let s = base.sample();
        let (_, core) = compare_case_record(&base).expect("agreement");
        let reward = core.expect("a record").block_subsidy + base.pay.fee_reward;
        let limit = oracle
            .superblock_payments_limit(&(s.net.chain)(), s.height)
            .expect("Core's payments limit");
        let v24s: &[ThresholdState] = if std::ptr::eq(s.net.params, mainnet()) {
            &[ThresholdState::Defined]
        } else {
            &[ThresholdState::Defined, ThresholdState::Active]
        };
        let triggers: [(&str, Edit); 4] = [
            ("no trigger", |_| {}),
            ("trigger", |_| {}),
            ("trigger a duff more", |p| p[0].1 += 1),
            ("trigger repeating its first", |p| p.push(p[0].clone())),
        ];
        for &v24 in v24s {
            for state in STATES {
                for (t, (trigger, edit)) in triggers.iter().enumerate() {
                    for over_max in [false, true] {
                        let (mut case, mut payments) = superblock(sample);
                        if t > 0 {
                            edit(&mut payments);
                            case.trigger = Some(payments);
                        }
                        if over_max {
                            case = paying(case, reward + limit + 1);
                        }
                        case.v24 = v24;
                        let paid = if over_max {
                            "a duff over the maximum"
                        } else {
                            "mined"
                        };
                        push(
                            format!("{sample} {state} v24 {v24:?} {trigger} pays {paid}"),
                            in_state(case, state),
                        );
                    }
                }
            }
        }
    }

    let path = format!("{}/../dash-evo/tests/data/quirk-14-v24.0.json", env!("CARGO_MANIFEST_DIR"));
    let mut text = String::from(
        "{\"comment\": \"Quirk 14: Core's IsBlockValueValid (oracle_block_value_payee, v24.0.0-rc.3) \
         on the payments fixture's budget and superblock blocks. Written by `cargo run -p node-l1 \
         --features oracle --example payments_quirk_vectors`; replayed by \
         dash-evo/tests/quirk_14_old_budget_and_superblock_checks.rs.\",\n\"vectors\": [\n",
    );
    let lines: Vec<String> = vectors.iter().map(Value::to_string).collect();
    text += &lines.join(",\n");
    text += "\n]}\n";
    std::fs::write(&path, text)?;
    eprintln!("quirk 14: {} vectors, {skipped} skipped", vectors.len());
    Ok(())
}
