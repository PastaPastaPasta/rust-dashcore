//! The spork evaluation shared by the DKG connection checks and the signing environment.

use dash_params::LlmqType;

/// `EvalSpork(llmqType, spork_value)` (`llmq/options.cpp:26-35`): a value of 0 enables every
/// type; 1 every type but `LLMQ_100_67`, `LLMQ_400_60` and `LLMQ_400_85`; anything else none.
/// `IsQuorumPoseEnabled` evaluates SPORK_23's value, `IsAllMembersConnectedEnabled` SPORK_21's.
pub fn eval_spork(t: LlmqType, spork_value: i64) -> bool {
    match spork_value {
        0 => true,
        1 => !matches!(t, LlmqType::Llmq100_67 | LlmqType::Llmq400_60 | LlmqType::Llmq400_85),
        _ => false,
    }
}
