//! Type-dependent evaluation of the quorum connection and PoSe sporks.

use dash_evo::spork::SporkId;
use dash_params::LlmqType;

pub use dash_evo::llmq::eval_spork;

use crate::sporks::SporkManager;

/// Whether spork 21 enables connections between every quorum member.
// Core: llmq/options.cpp:37-40
pub fn all_members_connected(llmq_type: LlmqType, sporks: &SporkManager) -> bool {
    eval_spork(llmq_type, sporks.value(SporkId::SPORK_21_QUORUM_ALL_CONNECTED))
}

/// Whether spork 23 enables quorum proof of service for this type.
// Core: llmq/options.cpp:44-47
pub fn quorum_pose_enabled(llmq_type: LlmqType, sporks: &SporkManager) -> bool {
    eval_spork(llmq_type, sporks.value(SporkId::SPORK_23_QUORUM_POSE))
}
