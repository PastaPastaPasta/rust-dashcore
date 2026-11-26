//! `NetQuorum`'s connection check as `dashd_quorums::tip::NetQuorumTip` calls it: QM1b-7's
//! handler owns Core's trigger, guards and order (llmq/net_quorum.cpp:377-404 at v24.0.0) and
//! calls [`QuorumConnectionsCheck::check`] once per LLMQ type in `llmqs` order, the only path
//! that calls it.

pub use dashd_quorums::Role;
pub use dashd_quorums::tip::{QuorumConnectionsCheck, TipRef};
