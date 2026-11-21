//! Stand-in for `dashd_quorums::dkg::seams` (DK1-L1): swapping to it is an import change.

use dash_evo::types::ProTxHash;
use dash_params::{LlmqParams, LlmqType};

use super::quorum_tip::TipRef;

/// The DKG round's quorum connections at the Initialized phase: `EnsureQuorumConnections` for the
/// `llmq_type` quorum at `quorum_base` as a masternode watching per `watch`, then, when
/// `we_are_member`, `AddQuorumProbeConnections`, both over one tip list.
///
/// Never called on V's thread. May make bounded reads of V (Core takes cs_main briefly inside
/// it); holds no NetCore or leaf lock across one.
///
/// Replaced by `dashd_quorums::dkg::seams::DkgConnections` (behind `dkg-runtime`) at DK1-L3.
// Core: llmq/net_dkg.cpp:880-888 at v24.0.0
pub trait DkgConnections: Send + Sync {
    fn on_initialized(
        &self,
        llmq_type: LlmqType,
        params: &LlmqParams,
        quorum_base: &TipRef,
        my_pro_tx: &ProTxHash,
        watch: bool,
        we_are_member: bool,
    );
}

/// No connections: a node without the masternode services.
pub struct NoDkgConnections;

impl DkgConnections for NoDkgConnections {
    fn on_initialized(
        &self,
        _: LlmqType,
        _: &LlmqParams,
        _: &TipRef,
        _: &ProTxHash,
        _: bool,
        _: bool,
    ) {
    }
}
