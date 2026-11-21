//! The per-type quorum connection check that the tip handler calls (QM1b-7's `NetQuorumTip`).

pub use dash_llmq::qdata::{Role, TipRef};
use dash_params::{LlmqParams, LlmqType};

/// CheckQuorumConnections (llmq/net_quorum.cpp v24.0.0 :438-497) for one type at `tip`; applies its
/// RegistryOps in order before returning. Never called on V's thread. May make bounded reads of V
/// (Core takes cs_main briefly inside it); holds no NetCore or leaf lock across one.
pub trait QuorumConnectionsCheck: Send + Sync {
    fn check(&self, llmq_type: LlmqType, params: &LlmqParams, tip: &TipRef, role: &Role);
}

/// The check before P7-W3 installs its own: it does nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoQuorumConnectionsCheck;

impl QuorumConnectionsCheck for NoQuorumConnectionsCheck {
    fn check(&self, _: LlmqType, _: &LlmqParams, _: &TipRef, _: &Role) {}
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_params::{ChainParamsBuilder, Network, Release};
    use dash_primitives::BlockHash;

    use super::*;

    #[test]
    fn the_no_op_check_is_object_safe() {
        let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
        let params =
            *setup.consensus.llmq_params().find(|(t, _)| *t == LlmqType::LlmqTest).unwrap().1;
        let check: Arc<dyn QuorumConnectionsCheck> = Arc::new(NoQuorumConnectionsCheck);
        let tip = TipRef {
            height: 1,
            hash: BlockHash::from([1; 32]),
        };
        check.check(LlmqType::LlmqTest, &params, &tip, &Role::Observer);
    }
}
