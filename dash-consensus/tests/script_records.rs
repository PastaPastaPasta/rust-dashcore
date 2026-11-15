//! L0 (CS1 §7.1): the `EvalScript` runs Core's own DIP0020 and CHECKDATASIG unit tests make,
//! recorded by O4 from Dash Core v24.0.0-rc.3 (`test-vectors/core/<commit>/transcribed/`, see its
//! `PROVENANCE.md`).
//!
//! Each record is `EvalScript(stack, script, flags, BaseSignatureChecker(), SigVersion::BASE)`: the
//! initial stack, the outcome, the error name and the final stack, which Core records on failure
//! too. The port must reproduce all of it.

#[path = "support/script.rs"]
mod script_support;

use dash_consensus::script::{NoSignatureChecker, eval_script};
use dash_core_vectors::hex::hex_str;
use script_support::{boolean, bytes, flags, show, stack, transcribed};

/// Runs every record of `name` and returns how many there were.
fn run(name: &str) -> usize {
    let records = transcribed(name);
    let mut failures = Vec::new();
    for (i, r) in records.iter().enumerate() {
        let (script, flags, ok) = (bytes(r, "script"), flags(r), boolean(r, "ok"));
        let error = r.find_value("error").get_str().unwrap();
        let expected_stack = stack(r, "result_stack");

        let mut got = stack(r, "stack");
        let result = eval_script(&mut got, &script, flags, &NoSignatureChecker);
        let got_error = result.err().map_or("OK", |e| e.name());
        if result.is_ok() != ok || got_error != error || got != expected_stack {
            failures.push(format!(
                "record {i}: script {} flags {:#x}: expected {error} {}, got {got_error} {}",
                hex_str(&script),
                flags.bits(),
                show(&expected_stack),
                show(&got),
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{name}: {} of {} records differ:\n{}",
        failures.len(),
        records.len(),
        failures.join("\n")
    );
    records.len()
}

#[test]
fn dip0020_opcode_records() {
    assert_eq!(run("dip0020opcodes_tests"), 4512);
}

#[test]
fn checkdatasig_records() {
    assert_eq!(run("checkdatasig_tests"), 214);
}
