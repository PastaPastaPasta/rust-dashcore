use std::sync::Arc;

use dash_chain::EntryView;
use dash_dml::{Dmn, ProTxHash};
use dash_params::ChainSetup;
use dash_params::LlmqType;
use dash_primitives::BlockHash;
use dash_types::Numeric;
use dashd_chainstate::chain::ChainQuery;
use dashd_masternode::active::{ActiveMasternode, ActiveState};
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_rpc::UniValue;
use dashd_rpc::evo::{
    BlockArg, BlockFailure, EvoError, MnListAt, MnListDiffFacts, MnListRequest, NodeEvo,
    QuorumFacts, QuorumListFacts, SmlDiffFacts,
};
use dashd_rpc::masternode::{ActiveMnStatus, NodeMasternode};
use dashd_rpc::protocol::RpcError;

use super::ChainSlot;
pub(crate) use recsig::RpcSigning;
pub(crate) use sign::RpcSign;

mod protx;
pub(crate) mod quorum;
pub(crate) mod recsig;
mod sign;
#[cfg(all(test, feature = "dkg-runtime"))]
pub(crate) use quorum::{ConnRead, write_dkg_status};
pub(crate) mod sml;

impl ChainSlot {
    pub(super) fn set_llmq_types(&self, params: &ChainSetup) {
        let _ = self.llmq_types.set(params.consensus.llmqs.iter().map(Numeric::to_base).collect());
    }

    pub fn set_meta(&self, meta: Arc<MasternodeMetaMan>) {
        let _ = self.meta.set(meta);
    }

    /// Step 7c on a masternode or an observer: what `quorum dkgstatus` reads outside V.
    pub fn set_dkg_status(&self, source: Arc<dyn DkgStatusSource>) {
        let _ = self.dkg.set(source);
    }

    /// Step 7c on a masternode: the active masternode `masternode status` reads.
    pub fn set_active(&self, active: Arc<ActiveMasternode>) {
        let _ = self.active.set(active);
    }
}

fn active_status(state: &ActiveState) -> ActiveMnStatus {
    ActiveMnStatus {
        outpoint: state.outpoint(),
        service: state.service.clone(),
        pro_tx_hash: state.pro_tx_hash.map(ProTxHash::from),
        state: state.state_string(),
        status: state.status(),
    }
}

impl NodeMasternode for ChainSlot {
    fn active_mn(&self) -> Option<ActiveMnStatus> {
        Some(active_status(&self.active.get()?.snapshot()))
    }

    /// The list at V's tip, in one V turn.
    fn tip_mn(&self, pro_tx_hash: &ProTxHash) -> Result<Option<Arc<Dmn>>, EvoError> {
        let writer = self.slot().clone().ok_or(EvoError::Unavailable)?;
        let pro_tx_hash = *pro_tx_hash;
        let read = writer.chain.query(move |q| {
            let (tip, _) = q.chain.private_tip().ok_or(EvoError::Unavailable)?;
            Ok(q.evo_view().list_for_block(tip.id)?.get(&pro_tx_hash).cloned())
        });
        read.recv().map_err(|_| EvoError::Unavailable)?
    }
}

/// What `quorum dkgstatus` reads outside V: the context's `CDKGDebugManager`, the active
/// masternode's proTxHash and `CConnman`'s nodes.
pub trait DkgStatusSource: Send + Sync {
    /// The `session` entries of `dkgdbgman->ToJson(detail)`, each with the fields the node
    /// serves.
    // Core: v24.0.0 src/rpc/quorums.cpp:363-367, src/llmq/debug.cpp:126-152
    fn sessions(&self, detail: i32) -> UniValue;
    /// `nodeman->GetProTxHash()` on a masternode, the null hash before its `Init`; `None` without
    /// `active_ctx`.
    // Core: v24.0.0 src/rpc/quorums.cpp:374
    fn active_pro_tx(&self) -> Option<[u8; 32]>;
    /// The verified proTxHashes of the nodes `ForEachNode` visits, in table order.
    // Core: v24.0.0 src/rpc/quorums.cpp:400-405
    fn connected_pro_txs(&self) -> Vec<[u8; 32]>;
    /// `IsAllMembersConnectedEnabled(type)`, spork 21 now.
    // Core: v24.0.0 src/llmq/utils.cpp:754-756
    fn all_members_connected(&self, llmq_type: LlmqType) -> bool;
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
    fn quorum_info(
        &self,
        llmq_type: u8,
        hash: BlockHash,
        include_sk_share: bool,
    ) -> Result<QuorumFacts, EvoError> {
        quorum::info(self, llmq_type, hash, include_sk_share)
    }
    fn dkg_status(&self, detail: i32) -> Result<UniValue, EvoError> {
        quorum::dkg_status(self, detail)
    }
    fn quorum_member_of(
        &self,
        pro_tx_hash: ProTxHash,
        count: Option<i32>,
    ) -> Result<Option<Vec<QuorumListFacts>>, EvoError> {
        quorum::member_of(self, pro_tx_hash, count)
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
        let error = table
            .execute(&JsonRpcRequest {
                method: "masternode".into(),
                params: UniValue::Arr(vec!["status".into()]),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(
            error.to_object(),
            RpcError::new(RPC_INTERNAL_ERROR, "This node does not run an active masternode.")
                .to_object()
        );
    }

    /// `masternode status` reads each state of the active masternode as Core's
    /// `GetStateString()`, `GetStatus()`, `GetOutPoint()`, `GetService()` and `GetProTxHash()`.
    // Core: v24.0.2 src/active/masternode.cpp:67-110
    #[test]
    fn active_status_reads_each_state() {
        use dash_primitives::{OutPoint, TxHash};
        use dashd_masternode::active::State;
        let null = format!("{}-4294967295", "0".repeat(64));
        for (state, text, status) in [
            (State::WaitingForProtx, "WAITING_FOR_PROTX", "Waiting for ProTx to appear on-chain"),
            (State::PoseBanned, "POSE_BANNED", "Masternode was PoSe banned"),
            (State::Removed, "REMOVED", "Masternode removed from list"),
            (State::OperatorKeyChanged, "OPERATOR_KEY_CHANGED", "Operator key changed or revoked"),
            (State::ProtxIpChanged, "PROTX_IP_CHANGED", "IP address specified in ProTx changed"),
            (State::Ready, "READY", "Ready"),
            (
                State::SomeError,
                "ERROR",
                "Error. Local address does not match the address from ProTx",
            ),
        ] {
            let mut active = ActiveState {
                state,
                error: "Local address does not match the address from ProTx".into(),
                ..ActiveState::default()
            };
            let read = active_status(&active);
            assert_eq!((read.state, read.status.as_str()), (text, status));
            let outpoint = format!("{}-{}", read.outpoint.txid(), read.outpoint.index());
            assert_eq!((outpoint, read.pro_tx_hash), (null.clone(), None));
            assert_eq!(read.service.to_string(), "[::]:0");
            active.outpoint = Some(OutPoint::new(TxHash::from([3; 32]), 1));
            active.pro_tx_hash = Some([4; 32]);
            active.service = dash_netaddr::Service {
                addr: dash_netaddr::NetAddr::ipv4([127, 0, 0, 1]),
                port: 19899,
            };
            let read = active_status(&active);
            assert_eq!(read.outpoint, OutPoint::new(TxHash::from([3; 32]), 1));
            assert_eq!(read.pro_tx_hash, Some(ProTxHash::from([4; 32])));
            assert_eq!(read.service.to_string(), "127.0.0.1:19899");
        }
    }

    /// DK1-L3 r1 m1: a build without `dkg-runtime` answers `quorum dkgstatus` as before L3
    /// registered it, the `quorum` family's unknown-subcommand error, and never a Core-shaped
    /// object without `session`.
    #[cfg(not(feature = "dkg-runtime"))]
    #[test]
    fn quorum_dkgstatus_without_a_dkg_source_answers_as_an_unknown_subcommand() {
        let chain = Arc::new(ChainSlot::default());
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
            DeprecatedRpc::default(),
            "regtest".into(),
        );
        table.start();
        table.set_warmup_finished();
        let quorum = |params: Vec<UniValue>| {
            table
                .execute(&JsonRpcRequest {
                    method: "quorum".into(),
                    params: UniValue::Arr(params),
                    ..Default::default()
                })
                .unwrap_err()
                .to_object()
        };
        let unknown = RpcError::new(-8, "Must be a valid command").to_object();
        assert_eq!(quorum(vec!["nosuchcommand".into()]), unknown);
        assert_eq!(quorum(vec!["dkgstatus".into()]), unknown);
        assert_eq!(quorum(vec!["dkgstatus".into(), 2.into()]), unknown);
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
            slot.mn_list(MnListRequest::Height {
                height: Some(0),
                mn_type: dashd_rpc::evo::MnListType::Registered,
                detailed: false
            })
            .unwrap_err(),
            EvoError::InvalidHeight
        );
        slot.detach();
        assert!(v.stop().is_some_and(|exit| exit.fatal.is_none()));
        assert!(abort.messages().is_empty());
    }

    /// Without evo state V drops the `OnV` turn, and `protx diff` still answers its argument
    /// and lookup errors in Core's order before `Unavailable`.
    // Core: rpc/evo.cpp:1899-1903, evo/smldiff.cpp:159-193
    #[test]
    fn protx_diff_without_evo_state_answers_arguments_first() {
        let params = regtest();
        let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_296_688_662));
        let abort = Arc::new(RecordingAbort::default());
        let mut v =
            Chain::start(fresh(&params, &clock, &abort, Box::new(MemBlockFiles::default())))
                .unwrap();
        startup_over(&mut v, Box::new(NullConnector), Box::new(NoFlush));
        imported(&mut v);
        assert!(v.handle().on_v(|_, _| ()).recv().is_err(), "V has no evo state");
        let slot = ChainSlot::default();
        slot.attach(v.handle(), v.signals(), None, None, None);
        let arg = |name, value: &str, syntax| BlockArg {
            name,
            value_str: value.into(),
            syntax,
        };
        let hash_arg =
            |name, hash: BlockHash| arg(name, &hash.to_string(), Ok(BlockRef::Hash(hash)));
        let syntax = RpcError::Runtime("block syntax fixture".into());
        let bool_error = RpcError::new(-8, "extended fixture");
        let unknown = BlockHash::from([0xab; 32]);
        let genesis = v.handle().view().snap.tip.tip_hash;
        let cases = [
            (
                arg("baseBlock", "0", Ok(BlockRef::Height(0))),
                hash_arg("block", genesis),
                Ok(false),
                EvoError::Block {
                    name: "baseBlock",
                    value_str: "0".into(),
                    failure: BlockFailure::BadArg,
                },
            ),
            (
                hash_arg("baseBlock", genesis),
                arg("block", "zz", Err(syntax.clone())),
                Err(bool_error.clone()),
                EvoError::Param(syntax),
            ),
            (
                hash_arg("baseBlock", unknown),
                hash_arg("block", genesis),
                Err(bool_error.clone()),
                EvoError::Param(bool_error),
            ),
            (
                hash_arg("baseBlock", unknown),
                hash_arg("block", genesis),
                Ok(true),
                hash_arg("baseBlock", unknown).failure(BlockFailure::NotFound(unknown)),
            ),
            (
                hash_arg("baseBlock", genesis),
                hash_arg("block", unknown),
                Ok(true),
                hash_arg("block", unknown).failure(BlockFailure::NotFound(unknown)),
            ),
            (
                hash_arg("baseBlock", genesis),
                hash_arg("block", genesis),
                Ok(true),
                EvoError::Unavailable,
            ),
        ];
        for (base, block, extended, want) in cases {
            assert_eq!(slot.sml_diff(base, block, extended).unwrap_err(), want);
        }
        slot.detach();
        assert!(v.stop().is_some_and(|exit| exit.fatal.is_none()));
        assert!(abort.messages().is_empty());
    }
}
