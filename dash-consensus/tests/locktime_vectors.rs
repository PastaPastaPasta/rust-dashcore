//! BIP68, BIP112 and BIP113 against Dash Core's verdicts on a regtest chain
//! (`data/locktime/regtest-v24.0.json`, written by `contrib/node/locktime-vectors.py`; see
//! `data/PROVENANCE.md`): each transaction, judged for the block after the chain's tip, must get
//! Core's mempool verdict (`testmempoolaccept`) and block verdict (`TestBlockValidity`), rejected
//! by the same rule.
//!
//! The node's side: [`is_final_tx`] with the median time past as the cutoff, [`sequence_locks`],
//! and [`verify_script`] with a checker whose `CheckSequence` is Core's (the transaction
//! signature checker itself arrives with CS1 phase B; these scripts check no signature).

#[path = "support/checks.rs"]
mod checks;

use dash_consensus::script::interpreter::{
    SEQUENCE_LOCKTIME_DISABLE_FLAG, SEQUENCE_LOCKTIME_MASK, SEQUENCE_LOCKTIME_TYPE_FLAG,
};
use dash_consensus::script::{ScriptFlags, ScriptNum, SignatureChecker, verify_script};
use dash_consensus::tx::{
    block_lock_time_flags, calculate_sequence_locks, is_final_tx, sequence_locks,
};
use dash_consensus::{Tx, decode_tx_exact};
use dash_core_vectors::hex::try_parse_hex;
use dash_params::BuriedDeployment;
use serde::Deserialize;

use checks::{Network, Window};

#[derive(Deserialize)]
struct Fixture {
    headers: Vec<String>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    tx: String,
    prev_heights: Vec<u32>,
    spent: Vec<Spent>,
    mempool: Mempool,
    block: Block,
}

#[derive(Deserialize)]
struct Spent {
    script: String,
}

#[derive(Deserialize)]
struct Mempool {
    allowed: bool,
    reason: Option<String>,
}

#[derive(Deserialize)]
struct Block {
    valid: bool,
    reason: Option<String>,
}

fn unhex(text: &str) -> Vec<u8> {
    try_parse_hex(text).expect("hex")
}

/// Core's `GenericTransactionSignatureChecker::CheckSequence` for input `index`; no signature
/// checks.
struct SequenceChecker<'a> {
    tx: &'a Tx,
    index: usize,
}

impl SignatureChecker for SequenceChecker<'_> {
    // Core: script/interpreter.cpp:1602 GenericTransactionSignatureChecker::CheckSequence
    fn check_sequence(&self, sequence: ScriptNum) -> bool {
        let tx_sequence = i64::from(self.tx.inputs()[self.index].sequence);
        if (i32::from(self.tx.version()) as u32) < 2 {
            return false;
        }
        if tx_sequence & i64::from(SEQUENCE_LOCKTIME_DISABLE_FLAG) != 0 {
            return false;
        }
        let mask = i64::from(SEQUENCE_LOCKTIME_TYPE_FLAG | SEQUENCE_LOCKTIME_MASK);
        let type_flag = i64::from(SEQUENCE_LOCKTIME_TYPE_FLAG);
        let (tx_masked, masked) = (tx_sequence & mask, sequence.value() & mask);
        if (tx_masked < type_flag) != (masked < type_flag) {
            return false;
        }
        masked <= tx_masked
    }
}

/// The first script failure under `flags`.
fn scripts(tx: &Tx, spent: &[Spent], flags: ScriptFlags) -> Result<(), &'static str> {
    for (index, (input, spent)) in tx.inputs().iter().zip(spent).enumerate() {
        let checker = SequenceChecker {
            tx,
            index,
        };
        verify_script(input.script_sig.as_bytes(), &unhex(&spent.script), flags, &checker)
            .map_err(|e| e.message())?;
    }
    Ok(())
}

/// The node's mempool and block reasons for `case`, `None` when accepted, in Core's order:
/// finality, then the sequence locks, then the scripts (under the standard flags, reported
/// as mandatory when the mandatory flags fail too).
fn ours(window: &Window, case: &Case) -> (Option<String>, Option<String>) {
    let tx = decode_tx_exact(&unhex(&case.tx)).expect("Core's transaction decodes").into_value();
    let ctx = window.ctx(0);
    let nonfinal = Some("TestBlockValidity failed: bad-txns-nonfinal".to_owned());
    if !is_final_tx(&tx, ctx.height as i32, ctx.median_time_past) {
        return (Some("non-final".into()), nonfinal);
    }
    let mut heights = case.prev_heights.clone();
    if !sequence_locks(&tx, block_lock_time_flags(true), &mut heights, &ctx) {
        return (Some("non-BIP68-final".into()), nonfinal);
    }
    if scripts(&tx, &case.spent, ScriptFlags::STANDARD).is_err() {
        let reason = match scripts(&tx, &case.spent, ScriptFlags::MANDATORY_V24_0) {
            Err(message) => format!("mandatory-script-verify-flag-failed ({message})"),
            Ok(()) => "non-mandatory-script-verify-flag".into(),
        };
        return (Some(reason), Some("TestBlockValidity failed: block-validation-failed".into()));
    }
    (None, None)
}

#[test]
fn lock_times_reproduce_cores_verdicts() {
    let path = format!("{}/tests/data/locktime/regtest-v24.0.json", env!("CARGO_MANIFEST_DIR"));
    let fixture: Fixture =
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("JSON");
    let headers =
        fixture.headers.iter().map(|h| unhex(h).try_into().expect("80-byte headers")).collect();
    let window = Window::from_headers(Network::Regtest, 0, headers, None);
    let csv = window.params().consensus.deployment_height(BuriedDeployment::Csv).to_u32();
    assert!(csv <= window.tip() + 1, "CSV is active at the block after the tip");

    let mut differing = Vec::new();
    for case in &fixture.cases {
        assert_eq!(case.mempool.allowed, case.mempool.reason.is_none(), "{}", case.name);
        assert_eq!(case.block.valid, case.block.reason.is_none(), "{}", case.name);
        let core = (case.mempool.reason.clone(), case.block.reason.clone());
        if ours(&window, case) != core {
            differing.push(case.name.as_str());
        }
    }
    assert!(differing.is_empty(), "the node differs from Core on {differing:?}");
    // Core's verdicts on a time lock ending one second before the cutoff and exactly at it, and
    // the node's lock time there: the cutoff is the median time past the block is judged at.
    let ctx = window.ctx(0);
    for (name, allowed, before_cutoff) in [
        ("time lock met exactly, ending one second before the cutoff", true, 1),
        ("time lock on the later coin ending at the cutoff", false, 0),
    ] {
        let case = fixture.cases.iter().find(|c| c.name == name).expect(name);
        assert_eq!(case.mempool.allowed, allowed, "{name}");
        let tx =
            decode_tx_exact(&unhex(&case.tx)).expect("Core's transaction decodes").into_value();
        let mut heights = case.prev_heights.clone();
        let locks = calculate_sequence_locks(&tx, block_lock_time_flags(true), &mut heights, &ctx);
        assert_eq!(locks.min_time, ctx.median_time_past - before_cutoff, "{name}");
    }
    let rejected = fixture.cases.iter().filter(|c| !c.mempool.allowed).count();
    assert!(rejected >= 15 && fixture.cases.len() - rejected >= 15);
}
