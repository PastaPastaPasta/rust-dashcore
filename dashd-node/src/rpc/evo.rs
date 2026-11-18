use std::sync::Arc;

use dash_chain::EntryView;
use dash_params::ChainSetup;
use dash_primitives::BlockHash;
use dash_types::Numeric;
use dashd_chainstate::chain::ChainQuery;
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_rpc::evo::{
    BlockArg, BlockFailure, EvoError, MnListAt, MnListDiffFacts, MnListRequest, NodeEvo,
    QuorumFacts, QuorumListFacts, SmlDiffFacts,
};
use dashd_rpc::protocol::RpcError;

use super::ChainSlot;

mod protx;
mod quorum;
mod sml;

impl ChainSlot {
    pub(super) fn set_llmq_types(&self, params: &ChainSetup) {
        let _ = self.llmq_types.set(params.consensus.llmqs.iter().map(Numeric::to_base).collect());
    }

    pub fn set_meta(&self, meta: Arc<MasternodeMetaMan>) {
        let _ = self.meta.set(meta);
    }
}

impl NodeEvo for ChainSlot {
    fn permissive_bool(&self) -> bool {
        self.deprecated.get().is_some_and(|d| d.permissive_bool)
    }
    fn mn_list(&self, request: MnListRequest) -> Result<MnListAt, EvoError> {
        protx::list(self, request)
    }
    fn mn_list_diff(&self, base: BlockArg, block: BlockArg) -> Result<MnListDiffFacts, EvoError> {
        protx::list_diff(self, base, block)
    }
    fn sml_diff(
        &self,
        base: BlockArg,
        block: BlockArg,
        extended: Result<bool, RpcError>,
    ) -> Result<SmlDiffFacts, EvoError> {
        sml::diff(self, base, block, extended)
    }
    fn quorum_list(&self, count: Option<i32>) -> Result<Vec<QuorumListFacts>, EvoError> {
        quorum::list(self, count)
    }
    fn quorum_list_extended(&self, height: Option<i32>) -> Result<Vec<QuorumListFacts>, EvoError> {
        quorum::list_extended(self, height)
    }
    fn llmq_type_known(&self, llmq_type: u8) -> bool {
        self.llmq_types.get().is_some_and(|types| types.contains(&llmq_type))
    }
    fn quorum_info(&self, llmq_type: u8, hash: BlockHash) -> Result<QuorumFacts, EvoError> {
        quorum::info(self, llmq_type, hash)
    }
}

fn block_hash(q: &ChainQuery<'_>, arg: &BlockArg) -> Result<BlockHash, EvoError> {
    let tip = q.chain.private_tip().ok_or(EvoError::Unavailable)?.0.height as i32;
    arg.hash(tip, |height| {
        q.chain
            .ancestor_private(height as u32)
            .and_then(|id| q.chain.reader().entry(id))
            .map(|e| e.hash)
    })
}

fn block_index(
    q: &ChainQuery<'_>,
    arg: &BlockArg,
    hash: BlockHash,
    genesis_alias: bool,
) -> Result<EntryView, EvoError> {
    let reader = q.chain.reader();
    let id = if genesis_alias && hash == BlockHash::from([0; 32]) {
        q.chain.ancestor_private(0)
    } else {
        reader.lookup(&hash)
    };
    id.and_then(|id| reader.entry(id)).ok_or_else(|| arg.failure(BlockFailure::NotFound(hash)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::Chain;
    use crate::chain::tests::{fresh, imported, regtest, startup_over};
    use crate::logging::Logger;
    use crate::rpc::{DeprecatedRpc, command_table};
    use crate::scheduler::Scheduler;
    use crate::shutdown::ShutdownState;
    use dash_clock::{Clock, MockClock};
    use dash_dml::ProTxHash;
    use dash_params::{ChainParamsBuilder, Network, Release};
    use dashd_chainstate::chain::{MemBlockFiles, NoFlush, NullConnector, RecordingAbort};
    use dashd_rpc::UniValue;
    use dashd_rpc::evo::BlockRef;
    use dashd_rpc::protocol::RPC_INTERNAL_ERROR;
    use dashd_rpc::request::JsonRpcRequest;

    #[test]
    fn node_registers_evo_and_connects_its_options() {
        let chain = Arc::new(ChainSlot::default());
        assert!(!chain.permissive_bool());
        let meta = Arc::new(MasternodeMetaMan::new());
        chain.set_meta(Arc::clone(&meta));
        assert!(Arc::ptr_eq(chain.meta.get().unwrap(), &meta));
        let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
        let table = command_table(
            &params,
            &Arc::new(Logger::new()),
            &Arc::new(ShutdownState::new()),
            &Arc::new(MockClock::at(1_700_000_000)),
            (true, &Scheduler::new()),
            &chain,
            &Arc::default(),
            &Arc::default(),
            &Arc::default(),
            DeprecatedRpc {
                permissive_bool: true,
                ..Default::default()
            },
            "regtest".into(),
        );
        table.start();
        table.set_warmup_finished();
        assert!(chain.permissive_bool());
        assert!(chain.llmq_type_known(100));
        assert!(!chain.llmq_type_known(255));
        for (family, sub) in [("protx", "list"), ("quorum", "list")] {
            let error = table
                .execute(&JsonRpcRequest {
                    method: family.into(),
                    params: UniValue::Arr(vec![sub.into()]),
                    ..Default::default()
                })
                .unwrap_err();
            assert_eq!(error.code(), RPC_INTERNAL_ERROR);
        }
    }
    #[test]
    fn adapters_validate_in_v_before_unavailable_data_reads() {
        let params = regtest();
        let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_296_688_662));
        let abort = Arc::new(RecordingAbort::default());
        let mut v =
            Chain::start(fresh(&params, &clock, &abort, Box::new(MemBlockFiles::default())))
                .unwrap();
        startup_over(&mut v, Box::new(NullConnector), Box::new(NoFlush));
        imported(&mut v);
        let slot = ChainSlot::default();
        slot.attach(v.handle(), v.signals(), None, None, None);
        let arg = |name, value: &str, syntax| BlockArg {
            name,
            value_str: value.into(),
            syntax,
        };
        let bad = RpcError::Runtime("target syntax fixture".into());
        let bool_error = RpcError::new(-8, "extended fixture");
        let unknown_a = BlockHash::from([0xab; 32]);
        let unknown_b = BlockHash::from([0xcd; 32]);
        let base = || arg("baseBlock", &unknown_a.to_string(), Ok(BlockRef::Hash(unknown_a)));
        let block = || arg("block", &unknown_b.to_string(), Ok(BlockRef::Hash(unknown_b)));
        assert_eq!(
            slot.sml_diff(
                arg("baseBlock", "0", Ok(BlockRef::Height(0))),
                arg("block", "zz", Err(bad.clone())),
                Err(bool_error.clone())
            )
            .unwrap_err(),
            EvoError::Block {
                name: "baseBlock",
                value_str: "0".into(),
                failure: BlockFailure::BadArg
            }
        );
        assert_eq!(
            slot.sml_diff(base(), arg("block", "zz", Err(bad.clone())), Err(bool_error.clone()))
                .unwrap_err(),
            EvoError::Param(bad)
        );
        assert_eq!(
            slot.sml_diff(base(), block(), Err(bool_error.clone())).unwrap_err(),
            EvoError::Param(bool_error)
        );
        assert_eq!(
            slot.sml_diff(base(), block(), Ok(true)).unwrap_err(),
            base().failure(BlockFailure::NotFound(unknown_a))
        );
        let zero =
            || arg("baseBlock", &"0".repeat(64), Ok(BlockRef::Hash(BlockHash::from([0; 32]))));
        assert_eq!(
            slot.sml_diff(zero(), block(), Ok(false)).unwrap_err(),
            block().failure(BlockFailure::NotFound(unknown_b))
        );
        assert_eq!(
            slot.mn_list_diff(base(), arg("block", "zz", Err(RpcError::Runtime("syntax".into()))))
                .unwrap_err(),
            base().failure(BlockFailure::BadArg)
        );
        let genesis = v.handle().view().snap.tip.tip_hash;
        assert_eq!(
            slot.sml_diff(
                zero(),
                arg("block", &genesis.to_string(), Ok(BlockRef::Hash(genesis))),
                Ok(false)
            )
            .unwrap_err(),
            EvoError::Unavailable
        );
        assert_eq!(
            slot.mn_list(MnListRequest::Info {
                pro_tx_hash: ProTxHash::from([1; 32]),
                block: Some(unknown_b)
            })
            .unwrap_err(),
            EvoError::Block {
                name: "blockHash",
                value_str: unknown_b.to_string(),
                failure: BlockFailure::NotFound(unknown_b)
            }
        );
        assert_eq!(
            slot.mn_list(MnListRequest::Height(Some(0))).unwrap_err(),
            EvoError::InvalidHeight
        );
        slot.detach();
        assert!(v.stop().is_some_and(|exit| exit.fatal.is_none()));
        assert!(abort.messages().is_empty());
    }
}
