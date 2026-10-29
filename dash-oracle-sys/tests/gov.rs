//! The governance entries (ABI 26, O2a-GOV) on Core's own test data and crafted inputs:
//! `governance::ValidateProposal()` over `proposals_valid.json` and `proposals_invalid.json`
//! exactly as `governance_validators_tests.cpp` drives it, written as GV1a's L0 vectors
//! (`tests/data/gov/proposals.txt`); and the refusals of the object, vote and op entries. The
//! Core-produced regtest state is `gov_regtest.rs`.

mod common;

use dash_core_vectors::cases::{load_proposals_invalid, load_proposals_valid};
use dash_core_vectors::read_core_text;
use dash_core_vectors::univalue::UniValue;
use dash_oracle_sys::{Chain, ErrorKind, GovContext, GovOp, GovOutput, Oracle};

/// An adjusted time after every end_epoch in the files (2025-10-17).
const NOW: i64 = 1_760_659_200;

/// `CreateEncodedProposalObject()`: the legacy `[["proposal", {...}]]` form.
fn legacy(proposal: &UniValue) -> String {
    let inner = UniValue::Arr(vec![UniValue::Str("proposal".into()), proposal.clone()]);
    common::to_hex(UniValue::Arr(vec![inner]).write().as_bytes())
}

fn current(proposal: &UniValue) -> String {
    common::to_hex(proposal.write().as_bytes())
}

/// One `ValidateProposal()` call as an L0 vector line.
fn line(
    oracle: &Oracle,
    name: &str,
    data_hex: &str,
    check_expiration: bool,
    allow_script: bool,
) -> (bool, String, String) {
    let (valid, errors) = oracle
        .gov_proposal_validate(&Chain::main(), data_hex, check_expiration, allow_script, NOW)
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    let text = format!(
        "{name} | expiration {} | script {} | time {NOW} | {data_hex} | {} | {}",
        u8::from(check_expiration),
        u8::from(allow_script),
        if valid {
            "valid"
        } else {
            "invalid"
        },
        if errors.is_empty() {
            "-"
        } else {
            &errors
        },
    );
    (valid, errors, text)
}

#[test]
fn proposal_files_get_core_verdicts() {
    for oracle in Oracle::all() {
        let mut lines = Vec::new();
        let valid =
            load_proposals_valid(&read_core_text("src/test/data/proposals_valid.json")).unwrap();
        for case in &valid {
            let name = format!("valid {}", case.index);
            // governance_validators_tests.cpp valid_proposals_test.
            let (ok, errors, text) = line(
                &oracle,
                &format!("{name} legacy"),
                &legacy(&case.proposal),
                true,
                case.allow_script,
            );
            assert!(!ok, "{name}");
            assert_eq!(errors, "Proposal must be a JSON object;JSON parsing error;", "{name}");
            lines.push(text);
            let (ok, errors, text) =
                line(&oracle, &name, &current(&case.proposal), false, case.allow_script);
            assert!(ok, "{name}: {errors}");
            lines.push(text);
            let (ok, _, text) = line(
                &oracle,
                &format!("{name} expired"),
                &current(&case.proposal),
                true,
                case.allow_script,
            );
            assert!(!ok, "{name}");
            lines.push(text);
        }
        let invalid =
            load_proposals_invalid(&read_core_text("src/test/data/proposals_invalid.json"))
                .unwrap();
        for (i, proposal) in invalid.iter().enumerate() {
            let name = format!("invalid {i}");
            // invalid_proposals_test.
            for (suffix, data, expiration) in [
                (" legacy", legacy(proposal), true),
                (" legacy unexpired", legacy(proposal), false),
                ("", current(proposal), false),
            ] {
                let (ok, _, text) =
                    line(&oracle, &format!("{name}{suffix}"), &data, expiration, false);
                assert!(!ok, "{name}{suffix}");
                lines.push(text);
            }
        }
        // extreme_end_epoch_is_not_expired, as Core's UniValue writes it.
        let extreme = r#"{"end_epoch":9223372036854775807,"name":"extreme-end-epoch","payment_address":"XpG61qAVhdyN7AqVZQsHfJL7AEk4dPVinc","payment_amount":1,"start_epoch":9223372036854775806,"type":1,"url":"https://example.com"}"#;
        let (ok, errors, text) =
            line(&oracle, "extreme end epoch", &common::to_hex(extreme.as_bytes()), true, false);
        assert!(ok, "{errors}");
        lines.push(text);
        common::gov_vectors::check_or_write(&oracle, "proposals.txt", "tests/gov.rs", &lines);
    }
}

#[test]
fn proposal_time_and_flag_refusals() {
    for oracle in Oracle::all() {
        let main = Chain::main();
        // The time bound of every governance entry: SetMockTime() and GetAdjustedTime() + 1 h,
        // and at least 1 (SetMockTime(0) is the wall clock, util/time.cpp:29-33).
        let max = i64::MAX / 1_000_000_000 - 7200;
        for t in [1, max] {
            assert!(oracle.gov_proposal_validate(&main, "", false, false, t).is_ok(), "{t}");
        }
        for t in [-1, 0, max + 1] {
            let err = oracle.gov_proposal_validate(&main, "", false, false, t).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Argument, "{t}");
        }
        // Not hex: Core's own verdict, not a refusal.
        let (ok, errors) = oracle.gov_proposal_validate(&main, "zz", false, false, NOW).unwrap();
        assert!(!ok && !errors.is_empty(), "{errors}");
    }
}

#[test]
fn ops_refusals() {
    for oracle in Oracle::all() {
        let chain = Chain::regtest();
        let ctx = common::genesis_context(&oracle);
        let gov = GovContext::default();
        let run = |ops: &[GovOp]| oracle.gov_ops(&chain, &ctx, &gov, ops);
        // A time outside the bound (the binding cannot write a truncated or unknown op).
        for t in [-1, 0, i64::MAX / 1_000_000_000 - 7199] {
            assert_eq!(run(&[GovOp::Time(t)]).unwrap_err().kind, ErrorKind::Argument, "{t}");
        }
        assert!(run(&[GovOp::Time(1)]).is_ok());
        // An undecodable object or vote is Core's verdict (none), not a refusal.
        assert_eq!(
            run(&[GovOp::Object(vec![1, 2]), GovOp::Vote(vec![3])]).unwrap(),
            vec![GovOutput::Object(None), GovOutput::Vote(None)]
        );
        // No such object; no superblock at the next height.
        let outputs = run(&[GovOp::State([7; 32]), GovOp::Superblock(1)]).unwrap();
        assert_eq!(outputs[0], GovOutput::State(None));
        let GovOutput::Superblock(sb) = &outputs[1] else {
            panic!("{outputs:?}")
        };
        assert!(!sb.triggered && sb.best.is_none() && sb.payments.is_empty());
    }
}
