//! Stand-in for `dashd_quorums::tip` (QM1b-6 declares it, QM1b-7's `NetQuorumTip` calls it):
//! swapping to it is an import change. `NetQuorumTip` owns Core's trigger, guards and order
//! (llmq/net_quorum.cpp:377-404 at v24.0.2) and calls [`QuorumConnectionsCheck::check`] once per
//! LLMQ type in `llmqs` order.

use dash_params::{LlmqParams, LlmqType};
use dash_primitives::BlockHash;

/// mnconn's role. `dash_llmq::qdata::Role` cannot be named in rdashd's build: `qdata` names
/// `sigenv`, which DEC-1667's ratchet holds out of rdashd until BL-895.
///
/// Replaced by `dashd_quorums::tip::Role` (`qdata::Role`, re-exported) after QM1b-6.
pub use dashd_masternode::mnconn::Role;

/// A new tip as `CheckQuorumConnections` receives it: `dash_llmq::qdata::TipRef`'s fields, which
/// rdashd's build cannot name for the reason [`Role`] gives.
///
/// Replaced by `dashd_quorums::tip::TipRef` (`qdata::TipRef`, re-exported) after QM1b-6.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipRef {
    pub height: i32,
    pub hash: BlockHash,
}

/// Core's `NetQuorum::CheckQuorumConnections(llmqParams, pindexNew)` for `llmq_type`, Core's
/// `llmqParams.type`: applies its registry ops in order, with leaf locks only. The caller never
/// passes `Role::None`.
///
/// Never called on V's thread. May make bounded reads of V (Core takes cs_main briefly inside
/// it); holds no NetCore or leaf lock across one.
///
/// Replaced by `dashd_quorums::tip::QuorumConnectionsCheck` after QM1b-6.
// Core: llmq/net_quorum.cpp:438-497
pub trait QuorumConnectionsCheck {
    fn check(&self, llmq_type: LlmqType, params: &LlmqParams, tip: &TipRef, role: &Role);
}
