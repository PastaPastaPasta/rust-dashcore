//! O2b-4 4b's witness rule (m3), shared by the unplanted witnesses (`l1_chain_diff`) and the
//! planted ones (`l1_chain_planted`): what every directed witness asserts of its run.

use dash_chain::Status;
use dash_oracle_sys::{ChainReport, Pin, ffi};
use node_l1::chain::core::Schedule;
use node_l1::chain::intent::Program;
use node_l1::chain::run::{ChainPlant, End, Run};
use node_l1::chain::{Mode, Runner};

/// A witness's program, parsed.
pub fn program(text: &str) -> Program {
    Program::parse(text).expect("a witness program")
}

/// One Diff-mode run of `text` on `schedule` with `plant` armed in the node, held to the rule:
/// the oracle at ABI 36 or later (checked at compile time, so it fails and never skips), no
/// partial refusal,
/// no tie discard and no op the node refused.
pub fn witness(schedule: Schedule, plant: ChainPlant, text: &str) -> Run {
    const { assert!(ffi::ABI_VERSION >= 36, "the witnesses need the oracle at ABI 36") };
    let mut runner = Runner::with_plant(Pin::V24_0, schedule, Mode::Diff, plant).expect("session");
    let run = runner.run(&program(text)).expect("the shim");
    assert!(run.partial.is_empty(), "no partial refusal: {:?}", run.partial);
    assert_eq!(run.end.discard(), None, "no tie discard: {:?}", run.end);
    assert!(!matches!(run.end, End::Unsupported(_)), "no unsupported op: {:?}", run.end);
    run
}

/// Core's step `at` of `run`.
pub fn core_report(run: &Run, at: usize) -> ChainReport {
    ChainReport::decode(run.raws[at].clone())
}

/// How many of `failed`'s stored records carry `FAILED_VALID`.
pub fn stored_failed(failed: &[(dash_oracle_sys::ChainHash, u32)]) -> usize {
    failed.iter().filter(|(_, bits)| bits & Status::FAILED_VALID != 0).count()
}

/// A reply's BIP22 string or error message, else its JSON.
pub fn reply_text(outcome: &Result<serde_json::Value, dash_oracle_sys::RpcError>) -> String {
    match outcome {
        Ok(serde_json::Value::String(s)) => s.clone(),
        Ok(value) => value.to_string(),
        Err(e) => e.message.clone(),
    }
}

/// The report index of the block step Core rejects with a reason starting `reason`.
pub fn rejected_block(run: &Run, reason: &str) -> Option<usize> {
    (1..run.raws.len()).find(|&i| {
        run.kinds[i] == "block"
            && reply_text(&core_report(run, i).reply.outcome).starts_with(reason)
    })
}
