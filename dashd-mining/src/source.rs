//! [`ChainTemplateSource`]: the node's [`BlockTemplateSource`] over V. One query turn reads what
//! `CreateNewBlock` reads under `cs_main` (the tip, its deployments, its median time past, the
//! next work, the evo state once DIP0003 is active after the tip, the mempool under its read
//! lock and the governance answer) and assembles the block in [`turn::run`]'s order; the template
//! is then tested on V with `TestBlockValidity`, as Core tests it before `CreateNewBlock`
//! returns.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use dash_args::{ArgsManager, RuntimeError};
use dash_clock::Clock;
use dash_consensus::Block;
use dash_consensus::coins::CoinsView;
use dash_consensus::ctx::{
    BlockId, ChainAncestry, Deployment, DeploymentView as _, bls_ctx_for_block,
};
use dash_consensus::header::HeaderParams;
use dash_consensus::payments::PaymentParams;
use dash_consensus::pow::{PowParams, get_next_work_required};
use dash_consensus::primitives::{TxHash, TxOut, TxRef};
use dash_consensus::subsidy::{SubsidyParams, block_subsidy_inner, genesis_reward};
use dash_dml::blockprocessor::pool::MineablePool;
use dash_dml::creditpool::{CreditPool, PoolState};
use dash_dml::mnhf::GetForBlockError;
use dash_dml::netinfo::NetInfoRules;
use dash_dml::specialtx::SpecialTxRules;
use dash_dml::{DmlParams, MnList, MnListBuilder, StoreError};
use dash_evo::llmq::MemberError;
use dash_params::{BuriedDeployment, ChainSetup, DeploymentPos, Network};
use dash_types::Numeric as _;
use dashd_chainstate::chain::{
    BlockState, ChainHandle, ChainQuery, EvoView, Fatal, NotTested, OnVMembersError,
};
use dashd_chainstate::connect::{SuperblockTemplate, ipv4_optional_active_at};
use dashd_chainstate::evo::node::node_net_info;
use dashd_chainstate::store::CoinsReadError;

use crate::assemble::{self, TipFacts, block_time};
use crate::evo::{
    self, BlockError, EvoChain, Superblock, log_no_masternode_registered, read_fault,
};
use crate::held::HeldLines;
use crate::options::Options;
use crate::package::{InstantSendDisabled, InstantSendHooks, PackageInputs};
use crate::select::{BlockState as Totals, Selected};
use crate::tip::QueryTip;
use crate::turn;
use crate::view::{MiningMempool, MiningSafety, PackageView};
use crate::{BlockTemplate, BlockTemplateSource, LastBlockStats, TemplateError, TemplateTxs};

/// The owner of an evo read V refuses as not built (`StoreError::NotBuilt`).
const EVO_READS_OWNER: &str = "CH7";

/// The node's template source: V's handle, the network, the node clock (ND1's `MockClock` under
/// `setmocktime`) and `-blockversion` ([`block_version_arg`]). One per node: its
/// [`last_block_stats`](BlockTemplateSource::last_block_stats) are Core's process-wide
/// `m_last_block_num_txs` and `m_last_block_size`.
pub struct ChainTemplateSource {
    chain: ChainHandle,
    clock: Arc<dyn Clock>,
    block_version: Option<i32>,
    /// The network's proof-of-work, subsidy and payment parameters, or why they cannot be built
    /// (the connector refuses the same chain parameters at its own construction).
    rules: Result<Arc<Rules>, String>,
    last: Mutex<Option<LastBlockStats>>,
    /// `BlockAssembler::Options`, clamped per call (`node/miner.cpp:205-207`).
    options: Options,
    /// The node's mempool, `None` until one is attached: [`TemplateTxs::Mempool`] then selects
    /// nothing.
    mempool: Option<Arc<dyn MiningMempool>>,
    instant_send: Arc<dyn InstantSendHooks>,
    safety: Arc<dyn MiningSafety>,
    #[cfg(feature = "test-support")]
    between_turns: Mutex<Option<BetweenTurns>>,
}

/// `IsTxSafeForMining` before a ChainLock handler is wired: never asked while InstantSend is
/// disabled.
struct NoSafety;

impl MiningSafety for NoSafety {
    fn is_tx_safe_for_mining(&self, _: &TxHash) -> bool {
        false
    }
}

/// [`ChainTemplateSource::set_between_turns`]'s hook.
#[cfg(feature = "test-support")]
type BetweenTurns = Box<dyn FnMut(&Block) + Send>;

struct Rules {
    pow: PowParams,
    subsidy: SubsidyParams,
    payments: PaymentParams,
    dml: DmlParams,
    /// `DeploymentHeight(V19)`.
    v19_height: i32,
    /// The node's NetInfo rules, `-cjdnsreachable` included, as its connector's.
    net_info: NetInfoRules,
    params: Arc<ChainSetup>,
}

impl Rules {
    fn of(params: Arc<ChainSetup>, cjdns_reachable: bool) -> Result<Rules, String> {
        let pow = *HeaderParams::from_chain_params(&params)
            .map_err(|e| format!("header parameters: {e:?}"))?
            .pow();
        let reward = genesis_reward(&dash_consensus::params::genesis_block(&params))
            .map_err(|e| format!("genesis: {e:?}"))?;
        let subsidy = SubsidyParams::from_chain_params(&params, reward)
            .map_err(|e| format!("subsidy parameters: {e:?}"))?;
        let payments = PaymentParams::from_chain_params(&params)
            .map_err(|e| format!("payment parameters: {e:?}"))?;
        let net_info = node_net_info(&params, cjdns_reachable).map_err(|e| e.0)?;
        let v19_height =
            i32::try_from(params.consensus.deployment_height(BuriedDeployment::V19).to_u32())
                .map_err(|_| "the V19 height is past Core's int".to_owned())?;
        Ok(Rules {
            pow,
            subsidy,
            payments,
            dml: DmlParams::new(&params),
            v19_height,
            net_info,
            params,
        })
    }
}

impl ChainTemplateSource {
    /// The network's subsidy parameters (`GetBlockSubsidy`'s), as the assembler uses them;
    /// `None` for chain parameters the connector refuses.
    pub fn subsidy_params(&self) -> Option<&SubsidyParams> {
        self.rules.as_ref().ok().map(|r| &r.subsidy)
    }

    /// `block_version` is `-blockversion` as [`block_version_arg`] reads it.
    pub fn new(
        chain: ChainHandle,
        params: Arc<ChainSetup>,
        clock: Arc<dyn Clock>,
        block_version: Option<i32>,
    ) -> ChainTemplateSource {
        ChainTemplateSource {
            chain,
            clock,
            block_version,
            rules: Rules::of(params, false).map(Arc::new),
            last: Mutex::new(None),
            options: Options::default(),
            mempool: None,
            instant_send: Arc::new(InstantSendDisabled),
            safety: Arc::new(NoSafety),
            #[cfg(feature = "test-support")]
            between_turns: Mutex::new(None),
        }
    }

    /// `BlockAssembler::Options` as `ApplyArgsManOptions` and the constructor's clamp leave them.
    pub fn with_options(mut self, options: Options) -> Self {
        self.options = options;
        self
    }

    /// The mempool [`TemplateTxs::Mempool`] selects from.
    pub fn with_mempool(mut self, mempool: Arc<dyn MiningMempool>) -> Self {
        self.mempool = Some(mempool);
        self
    }

    /// The InstantSend and ChainLock hooks `TestPackageTransactions` asks.
    pub fn with_instant_send(
        mut self,
        instant_send: Arc<dyn InstantSendHooks>,
        safety: Arc<dyn MiningSafety>,
    ) -> Self {
        self.instant_send = instant_send;
        self.safety = safety;
        self
    }

    /// `-cjdnsreachable` as the node reads it (`false` by default, Core's), for the NetInfo
    /// rules the list after a template's block is built under.
    pub fn cjdns_reachable(mut self, reachable: bool) -> Self {
        if let Ok(rules) = &self.rules {
            self.rules = Rules::of(Arc::clone(&rules.params), reachable).map(Arc::new);
        }
        self
    }

    /// Test support: `hook` runs on every build between the query turn and `TestBlockValidity`,
    /// the window where Core holds `cs_main` and V does not, so a test can connect a block there;
    /// it sees the block the query turn assembled.
    #[cfg(feature = "test-support")]
    pub fn set_between_turns(&self, hook: impl FnMut(&Block) + Send + 'static) {
        *self.between_turns.lock().unwrap_or_else(|p| p.into_inner()) = Some(Box::new(hook));
    }

    /// The network's proof-of-work parameters, for [`grind`](crate::grind) on this source's
    /// templates.
    ///
    /// # Errors
    ///
    /// [`TemplateError::Failed`] where the chain parameters build none, as `create_new_block`.
    pub fn pow(&self) -> Result<&PowParams, TemplateError> {
        self.rules.as_ref().map(|r| &r.pow).map_err(|e| TemplateError::Failed(e.clone()))
    }
}

impl BlockTemplateSource for ChainTemplateSource {
    // Core: v24.0.2 node/miner.cpp:176-363 BlockAssembler::CreateNewBlock
    fn create_new_block(
        &self,
        coinbase_script: &[u8],
        txs: TemplateTxs,
    ) -> Result<BlockTemplate, TemplateError> {
        let rules = self.rules.as_ref().map_err(|e| TemplateError::Failed(e.clone()))?;
        loop {
            let time_start = Instant::now();
            let inputs = TurnInputs {
                now: self.clock.node_now().secs(),
                block_version: self.block_version,
                rules: Arc::clone(rules),
                script: coinbase_script.to_vec(),
                // :251: `generateblock` passes no mempool.
                mempool: match txs {
                    TemplateTxs::Mempool => self.mempool.clone(),
                    TemplateTxs::None => None,
                },
                instant_send: Arc::clone(&self.instant_send),
                safety: Arc::clone(&self.safety),
                options: self.options,
            };
            let held = HeldLines::for_this_thread();
            let turn_held = Arc::clone(&held);
            // A fault of V's own reads stops V after it answers (OD-CP-134 M2, OD-CP-135).
            let answer = self
                .chain
                .query_or_stop(move |q| match turn_held.hold(|| tip_block(q, &inputs)) {
                    Ok(built) => Ok(Ok(built)),
                    Err(BlockError::Refused(e)) => Ok(Err(e)),
                    Err(BlockError::Fault(what)) => Err(Fatal::new(what)),
                })
                .recv();
            if !matches!(answer, Ok(Ok(Ok(_)))) {
                held.replay();
            }
            let built = answer
                .map_err(|_| TemplateError::NoChain)?
                .map_err(|f| TemplateError::Failed(format!("CreateNewBlock: {}", f.0)))??;
            let block = Arc::new(built.block);
            #[cfg(feature = "test-support")]
            if let Some(hook) =
                self.between_turns.lock().unwrap_or_else(|p| p.into_inner()).as_mut()
            {
                hook(&block);
            }
            // :351-354, PoW and merkle off.
            let state = if self.options.test_block_validity {
                let answer =
                    self.chain.test_block_validity(Arc::clone(&block), false, false).recv();
                // A block reached V between the two turns, where Core holds `cs_main` across
                // both: build on the new tip, a serial order Core could have run, and drop this
                // build's lines. The loop ends when V's tip holds still for two turns, or with V.
                if matches!(answer, Ok(Err(NotTested::NotBestPrev))) {
                    continue;
                }
                held.replay();
                match answer {
                    Err(_) => return Err(TemplateError::NoChain),
                    // GBT's lookup answers; this call makes no lookup.
                    Ok(Err(other)) => Some(BlockState::Error(other.to_string())),
                    Ok(Ok(state)) => Some(state),
                }
            } else {
                held.replay();
                None
            };
            // :259-260: the counters are set before TestBlockValidity's verdict, so a refused
            // template sets them too. Here they follow V's answer, so a template rebuilt on a
            // newer tip records them once.
            *self.last.lock().unwrap_or_else(|p| p.into_inner()) = Some(built.stats);
            if let Some(state) = state.filter(|s| !s.is_valid()) {
                return Err(TemplateError::Failed(format!(
                    "CreateNewBlock: TestBlockValidity failed: {state}"
                )));
            }
            let time_2 = Instant::now();
            let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
            // :356-360
            tracing::debug!(
                target: "bench",
                "CreateNewBlock() packages: {:.2}ms ({} packages, {} updated descendants), \
                 validity: {:.2}ms (total {:.2}ms)",
                ms(built.time_1.saturating_duration_since(time_start)),
                built.packages_selected,
                built.descendants_updated,
                ms(time_2.saturating_duration_since(built.time_1)),
                ms(time_2.saturating_duration_since(time_start))
            );
            return Ok(BlockTemplate {
                block: dash_consensus::Block::new(*block.header(), block.txs().to_vec()),
                prev: built.facts.prev,
                height: built.facts.height,
                tx_fees: built.tx_fees,
                tx_sigops: built.tx_sigops,
                prev_bits: built.prev_bits,
                masternode_payments: built.masternode_payments,
                superblock_payments: built.superblock_payments,
            });
        }
    }

    fn last_block_stats(&self) -> Option<LastBlockStats> {
        *self.last.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// What one build hands its V turn.
struct TurnInputs {
    now: i64,
    block_version: Option<i32>,
    rules: Arc<Rules>,
    script: Vec<u8>,
    mempool: Option<Arc<dyn MiningMempool>>,
    instant_send: Arc<dyn InstantSendHooks>,
    safety: Arc<dyn MiningSafety>,
    options: Options,
}

/// One V turn's template: the block and what `CreateNewBlock` sets for it.
struct Built {
    facts: TipFacts,
    /// `pindexPrev->nBits` (`node/miner.cpp:348`).
    prev_bits: u32,
    block: Block,
    stats: LastBlockStats,
    /// `vTxFees`, the coinbase's `-nFees` first (`:191`, `:240`, `:341`, `:444`).
    tx_fees: Vec<i64>,
    /// `vTxSigOps`, the coinbase's legacy count first (`:192`, `:241`, `:349`, `:445`).
    tx_sigops: Vec<i64>,
    /// `FillBlockPayments`' `voutMasternodePaymentsRet` and `voutSuperblockPaymentsRet` (`:338`).
    masternode_payments: Vec<TxOut>,
    superblock_payments: Vec<TxOut>,
    packages_selected: u64,
    descendants_updated: u64,
    /// `time_1`: after the selection (`:257`).
    time_1: Instant,
}

/// `CreateNewBlock`'s block after the tip at `now`, paying the miner's part to `script`, from
/// what it reads under `cs_main`, all in one V turn in [`turn::run`]'s order. A store fault of
/// V's tip list, credit pool or LLMQ view, or of the list build, is a [`BlockError::Fault`], as
/// connect's is (MN0-3 notes 3-D1 (7)).
fn tip_block(q: &ChainQuery<'_>, inputs: &TurnInputs) -> Result<Built, BlockError> {
    let tip = tip_facts(q, inputs.now, inputs.block_version, &inputs.rules)?;
    let ancestry = q.chain.complete().ok_or_else(failed_chain)?;
    let prev = Some(tip.prev);
    let dip3 = q.deployments.active_after(prev, Deployment::Buried(BuriedDeployment::Dip0003));
    let view = q.evo_view();
    let evo = if dip3 {
        let height = tip.facts.height;
        // Core: v24.0.2 masternode/payments.cpp:152 GetListForBlock(pindexPrev)
        let list = view.tip_list().map_err(|e| read_fault(height, "GetListForBlock", e))?;
        // Core: v24.0.2 node/miner.cpp:520 GetCreditPool(pindexPrev), the default below V20
        let pool = view.credit_pool().map_err(|e| read_fault(height, "GetCreditPool", e))?;
        Some(EvoReads {
            list,
            credit_pool: template_pool(pool.as_deref())?,
            // The pool's leaf lock is held only to copy it, in this V turn, never across a read.
            mineable: view.mineable(),
        })
    } else {
        None
    };
    let mut turn = QueryTurn {
        q,
        view,
        inputs,
        ancestry,
        tip,
        evo,
    };
    turn::run(&mut turn, inputs.mempool.as_deref())
}

/// The tip's evo state the turn reads once DIP0003 is active after it.
struct EvoReads {
    list: Arc<MnList>,
    credit_pool: CreditPool,
    mineable: MineablePool,
}

/// [`turn::Turn`] over V's tip in one query turn.
struct QueryTurn<'q> {
    q: &'q ChainQuery<'q>,
    view: EvoView<'q>,
    inputs: &'q TurnInputs,
    ancestry: &'q dyn ChainAncestry,
    tip: Tip,
    evo: Option<EvoReads>,
}

/// [`QueryTurn`]'s block so far.
struct Started<'q> {
    /// The commitments, and the list builder with them and every selected package applied.
    evo: Option<(evo::Started, Option<MnListBuilder<'q, 'q>>)>,
    /// The selected transactions, in block order, with their `vTxFees` and `vTxSigOps`.
    txs: Vec<TxRef>,
    selected: Vec<Selected>,
    /// `nBlockSize`, `nBlockSigOps`, `nBlockTx` and `nFees`.
    state: Totals,
    packages_selected: u64,
    descendants_updated: u64,
}

/// The turn after its CbTx: the block through the CbTx once DIP0003 is active, else the selected
/// transactions alone, and what the payments and the template's counters need.
struct Paying {
    evo: Option<evo::CbTxBlock>,
    commitments: usize,
    txs: Vec<TxRef>,
    selected: Vec<Selected>,
    state: Totals,
    packages_selected: u64,
    descendants_updated: u64,
    time_1: Instant,
}

impl<'q> QueryTurn<'q> {
    /// `f` over the evo state at the tip, under the coins cell then the LLMQ view.
    fn with_evo_chain<R>(
        &self,
        evo: &EvoReads,
        f: impl FnOnce(
            &EvoChain<'_, 'q, dyn CoinsView<Error = CoinsReadError> + '_>,
        ) -> Result<R, BlockError>,
    ) -> Result<R, BlockError> {
        let rules = &self.inputs.rules;
        let height = self.tip.facts.height;
        let bls = bls_ctx_for_block(&rules.params, height);
        self.view
            .with_tip_coins(|coins| {
                self.view.with_template(bls, |qc, parent| {
                    let parent_chainlock = || parent().map_err(|e| store_refusal(height, e));
                    f(&EvoChain {
                        deployments: &self.q.deployments,
                        qc,
                        pool: &evo.mineable,
                        list: &evo.list,
                        coins,
                        ancestry: self.ancestry,
                        dml: &rules.dml,
                        net_info: &rules.net_info,
                        // Core: v24.0.2 node/miner.cpp:120 GetBestChainLock()
                        best_chainlock: self
                            .q
                            .cl
                            .best
                            .as_deref()
                            .map(|cl| (cl.height, cl.sig.clone())),
                        bls,
                        parent_chainlock: &parent_chainlock,
                        v19_height: rules.v19_height,
                        credit_pool: &evo.credit_pool,
                        payments: &rules.payments,
                        addr: &rules.params.addr_params,
                    })
                })
            })
            .map_err(|e| TemplateError::Failed(format!("CreateNewBlock: {}", e.0)))?
            .ok_or_else(|| coins_refusal(height))?
            .map_err(|e| qc_refusal(height, e))?
    }

    /// `GetSpecialTxRules(Tip())`.
    // Core: v24.0.2 validation.cpp:6821-6828
    fn special_tx_rules(&self) -> Result<SpecialTxRules, BlockError> {
        let after = |pos| {
            let deployment = Deployment::Versionbits(pos);
            self.q.deployments.try_active_after(Some(self.tip.prev), deployment).map_err(|e| {
                BlockError::from(TemplateError::Failed(format!("CreateNewBlock: {e}")))
            })
        };
        Ok(SpecialTxRules {
            v24: after(DeploymentPos::V24)?,
            evo_shares: after(DeploymentPos::EvoShares)?,
            ipv4_optional: ipv4_optional_active_at(
                &self.inputs.rules.params,
                self.tip.facts.height,
            ),
        })
    }
}

impl<'q> turn::Turn for QueryTurn<'q> {
    type Started = Started<'q>;
    type CbTx = Paying;
    type Out = Built;

    fn begin(&mut self) -> Result<Started<'q>, BlockError> {
        let mut state = Totals::default();
        let evo = match &self.evo {
            Some(evo) => {
                let (facts, prev) = (&self.tip.facts, self.tip.prev);
                let (started, builder) =
                    self.with_evo_chain(evo, |chain| evo::begin(facts, prev, chain))?;
                state.size = started.stats.size;
                state.tx_count = started.stats.num_txs;
                Some((started, Some(builder)))
            }
            None => None,
        };
        Ok(Started {
            evo,
            txs: Vec::new(),
            selected: Vec::new(),
            state,
            packages_selected: 0,
            descendants_updated: 0,
        })
    }

    fn dmn_tip_list(&mut self) -> Result<Arc<MnList>, BlockError> {
        if self.evo.is_none() {
            // Core's list before DIP0003.
            return Ok(Arc::new(MnList::default_pre_dip3()));
        }
        // Core: v24.0.2 masternode/payments.cpp:450 GetListAtChainTip()
        self.view.dmn_list().map_err(|e| read_fault(self.tip.facts.height, "GetListAtChainTip", e))
    }

    // Core: v24.0.2 node/miner.cpp:205-207, 251-255, 514-531
    fn select(
        &mut self,
        pool: &dyn PackageView,
        dmn_tip_list: &MnList,
        started: &mut Started<'q>,
    ) -> Result<(), BlockError> {
        let (height, prev) = (self.tip.facts.height, self.tip.prev);
        let deployments = &self.q.deployments;
        let after = |d| deployments.active_after(Some(prev), Deployment::Buried(d));
        let rules = &self.inputs.rules;
        let special_tx_rules = self.special_tx_rules()?;
        let empty = MnList::default_pre_dip3();
        let tip = QueryTip {
            q: self.q,
            view: self.q.evo_view(),
            params: &rules.params,
            ancestry: self.ancestry,
            prev,
            height,
            tip_list: self.evo.as_ref().map_or(&empty, |evo| &evo.list),
            rules: special_tx_rules,
            net_info: rules.net_info,
        };
        let signals = self.q.ehf_signals_stage(prev).map_err(|e| match e {
            GetForBlockError::Store(e) => read_fault(height, "GetSignalsStage", e),
            e => TemplateError::Failed(e.to_string()).into(),
        })?;
        let credit_pool = match &self.evo {
            Some(evo) if after(BuriedDeployment::V20) => Some(evo.credit_pool.clone()),
            _ => None,
        };
        let inputs = PackageInputs {
            tip: &tip,
            instant_send: &*self.inputs.instant_send,
            safety: &*self.inputs.safety,
            dmn_tip_list,
            height,
            lock_time_cutoff: self.tip.mtp,
            v24: special_tx_rules.v24,
            credit_pool,
            signals,
            builder: started.evo.as_mut().and_then(|(_, builder)| builder.take()),
        };
        let options = self.inputs.options.clamp(after(BuriedDeployment::Dip0001));
        let picked = turn::select(pool, inputs, &options, started.state)?;
        if let Some((_, builder)) = &mut started.evo {
            *builder = picked.builder;
        }
        started.txs = picked.txs;
        started.selected = picked.selection.entries;
        started.state = picked.selection.state;
        started.packages_selected = picked.selection.packages_selected;
        started.descendants_updated = picked.selection.descendants_updated;
        Ok(())
    }

    fn superblock(&mut self, dmn_tip_list: &MnList) -> Result<SuperblockTemplate, BlockError> {
        self.q
            .superblock_for_template(self.tip.facts.height, dmn_tip_list)
            .map_err(|f| BlockError::Fault(format!("the template's governance read: {}", f.0)))
    }

    // Core: v24.0.2 node/miner.cpp:257-333
    fn cbtx(&mut self, started: Started<'q>) -> Result<Paying, BlockError> {
        let time_1 = Instant::now();
        let state = started.state;
        tracing::info!(
            "CreateNewBlock(): total size {} txs: {} fees: {} sigops {}",
            state.size,
            state.tx_count,
            state.fees,
            state.sigops
        );
        let (evo, commitments) = match (started.evo, &self.evo) {
            (Some((evo_started, builder)), Some(evo)) => {
                let commitments = evo_started.txs.len() - 1;
                let builder = builder.ok_or_else(|| {
                    BlockError::Fault("the package rules kept no list builder".into())
                })?;
                let (facts, prev) = (&self.tip.facts, self.tip.prev);
                let block = self.with_evo_chain(evo, |chain| {
                    evo::cbtx(facts, prev, chain, evo_started, &builder, &started.txs)
                })?;
                (Some(block), commitments)
            }
            _ => (None, 0),
        };
        Ok(Paying {
            evo,
            commitments,
            txs: started.txs,
            selected: started.selected,
            state,
            packages_selected: started.packages_selected,
            descendants_updated: started.descendants_updated,
            time_1,
        })
    }

    // Core: v24.0.2 node/miner.cpp:263-283, :337-349
    fn finish(
        &mut self,
        paying: Paying,
        dmn_tip_list: &MnList,
        superblock: SuperblockTemplate,
    ) -> Result<Built, BlockError> {
        let state = paying.state;
        let stats = LastBlockStats {
            num_txs: state.tx_count,
            size: state.size,
        };
        let rules = &self.inputs.rules;
        let facts = &self.tip.facts;
        let superblock = Superblock::of(superblock, &rules.payments, dmn_tip_list, facts.height);
        let script = &self.inputs.script;
        let (block, masternode_payments, superblock_payments) = match (paying.evo, &self.evo) {
            (Some(block), Some(evo)) => {
                let built = evo::pay(
                    facts,
                    script,
                    &evo.list,
                    &rules.payments,
                    &rules.params.addr_params,
                    block,
                    state.fees,
                    stats,
                    superblock,
                )?;
                (built.block, built.masternode_payments, built.superblock_payments)
            }
            _ => {
                let superblock_payments = superblock.outputs(facts.height)?;
                log_no_masternode_registered();
                let block =
                    assemble::block(facts, script, &paying.txs, state.fees, &superblock_payments);
                (block, Vec::new(), superblock_payments)
            }
        };
        let (tx_fees, tx_sigops) =
            tx_counters(&block.txs()[0], paying.commitments, &paying.selected, state.fees);
        Ok(Built {
            facts: *facts,
            prev_bits: self.tip.prev_bits,
            block,
            stats,
            tx_fees,
            tx_sigops,
            masternode_payments,
            superblock_payments,
            packages_selected: paying.packages_selected,
            descendants_updated: paying.descendants_updated,
            time_1: paying.time_1,
        })
    }
}

/// `GetCreditPool(pindexPrev)` as the template takes it: the default below V20, and a pool that
/// does not build (`PoolError`) is Core's throw out of `CreateNewBlock`.
// Core: v24.0.0-rc.5 node/miner.cpp:520, evo/creditpool.cpp:256
fn template_pool(state: Option<&PoolState>) -> Result<CreditPool, TemplateError> {
    match state {
        None => Ok(CreditPool::default()),
        Some(Ok(pool)) => Ok(pool.clone()),
        Some(Err(e)) => Err(TemplateError::Failed(format!("CreateNewBlock: GetCreditPool: {e}"))),
    }
}

/// A store fault of an evo read in the query turn: `NotBuilt` is the readiness refusal; any other
/// is Core's throw out of `CreateNewBlock`, the RPC's error (OD-CP-20 B-2). A raw engine error was
/// reported where the engine read it.
pub(crate) fn store_refusal(height: u32, e: StoreError) -> TemplateError {
    match e {
        StoreError::NotBuilt(rule) => TemplateError::NotReady {
            height,
            rule,
            owner: EVO_READS_OWNER,
        },
        e => TemplateError::Failed(format!("CreateNewBlock: {e}")),
    }
}

/// A node whose connector keeps no coins: the readiness refusal.
pub(crate) fn coins_refusal(height: u32) -> BlockError {
    TemplateError::NotReady {
        height,
        rule: "the active coins of a node without a coins cache",
        owner: EVO_READS_OWNER,
    }
    .into()
}

/// `with_template`'s own error: a store fault building the LLMQ view is a read fault, as
/// connect's (`read_fault`); any other its throw.
pub(crate) fn qc_refusal(height: u32, e: OnVMembersError) -> BlockError {
    match e {
        OnVMembersError::Members(MemberError::View(e)) => read_fault(height, "the LLMQ view", e),
        e => TemplateError::Failed(format!("CreateNewBlock: {e}")).into(),
    }
}

/// The tip as the template reads it.
struct Tip {
    facts: TipFacts,
    prev: BlockId,
    /// `pindexPrev->nBits` (`node/miner.cpp:348`).
    prev_bits: u32,
    /// `m_lock_time_cutoff`: the tip's median time past (`:217`).
    mtp: i64,
}

/// What `CreateNewBlock` reads under `cs_main` for the block after the tip at `now`.
fn tip_facts(
    q: &ChainQuery<'_>,
    now: i64,
    block_version: Option<i32>,
    rules: &Rules,
) -> Result<Tip, TemplateError> {
    // :194-197: Core asserts a tip; V has one once its chain loads.
    let (tip, _) = q.chain.private_tip().ok_or(TemplateError::NoChain)?;
    let chain = q.chain.complete().ok_or_else(failed_chain)?;
    let prev = Some(tip.id);
    let height = tip.height + 1;
    let read_fault = |e| TemplateError::Failed(format!("CreateNewBlock: {e}"));
    // :209-214: computed even when `-blockversion` replaces it, as Core's is.
    let computed = q.deployments.compute_block_version(prev).map_err(read_fault)?;
    let tip_header = chain.header(tip.id);
    let time = block_time(now, tip_header.mtp);
    // :346, after `UpdateTime`; regtest's recomputation inside `UpdateTime` reads the same.
    let bits = get_next_work_required(chain, tip.id, time, &rules.pow).to_base();
    // :271, the previous block's `nBits` and height.
    let v20 = q.deployments.active_after(prev, Deployment::Buried(BuriedDeployment::V20));
    let prev_height = i32::try_from(tip.height)
        .map_err(|_| TemplateError::Failed(format!("tip height {}", tip.height)))?;
    let subsidy = block_subsidy_inner(tip_header.bits.to_base(), prev_height, v20, &rules.subsidy)
        .map_err(|e| TemplateError::Failed(format!("GetBlockSubsidyInner: {e}")))?;
    Ok(Tip {
        facts: TipFacts {
            prev: tip.hash,
            height,
            version: block_version.unwrap_or(computed),
            time,
            bits,
            subsidy,
        },
        prev: tip.id,
        prev_bits: tip_header.bits.to_base(),
        mtp: tip_header.mtp,
    })
}

fn failed_chain() -> TemplateError {
    TemplateError::Failed("V's chain has a header window".to_owned())
}

/// `-blockversion` as `CreateNewBlock` reads it (`node/miner.cpp:209-214`): off mainnet only,
/// `GetIntArg`'s `int64_t` stored into `nVersion`'s `int32_t` (truncated, as Core's conversion
/// is).
///
/// # Errors
///
/// `GetIntArg`'s, for a setting that is no number.
pub fn block_version_arg(
    args: &ArgsManager,
    params: &ChainSetup,
) -> Result<Option<i32>, RuntimeError> {
    if params.network == Network::Main {
        return Ok(None);
    }
    Ok(args.get_int_arg_opt("-blockversion")?.map(|v| v as i32))
}

/// `vTxFees` and `vTxSigOps`: the coinbase's `-nFees` and legacy count, a zero for each
/// commitment, then each selected transaction's.
// Core: v24.0.2 node/miner.cpp:191-192, :240-241, :341, :349, :444-445
fn tx_counters(
    coinbase: &dash_consensus::primitives::Tx,
    commitments: usize,
    selected: &[Selected],
    fees: i64,
) -> (Vec<i64>, Vec<i64>) {
    let zeros = std::iter::repeat_n(0, commitments);
    let tx_fees = std::iter::once(fees.wrapping_neg())
        .chain(zeros.clone())
        .chain(selected.iter().map(|s| s.fee))
        .collect();
    let tx_sigops = std::iter::once(i64::from(coinbase.legacy_sigop_count()))
        .chain(zeros)
        .chain(selected.iter().map(|s| i64::from(s.sigops)))
        .collect();
    (tx_fees, tx_sigops)
}

#[cfg(test)]
mod tests {
    use dash_dml::StoreError;
    use dash_evo::llmq::MemberError;
    use dashd_chainstate::chain::OnVMembersError;

    use super::{EVO_READS_OWNER, qc_refusal, store_refusal, template_pool, tx_counters};
    use crate::TemplateError;
    use crate::evo::{BlockError, read_fault};

    /// `GetCreditPool`'s pool on the template path: the default below V20 and the pool past it;
    /// one that does not build fails the template, never the default.
    #[test]
    fn a_pool_that_does_not_build_fails_the_template() {
        use dash_dml::creditpool::{CreditPool, PoolError};
        assert_eq!(template_pool(None), Ok(CreditPool::default()));
        let built = CreditPool::default();
        assert_eq!(template_pool(Some(&Ok(built.clone()))), Ok(built));
        let e = PoolError::PrevLocked;
        assert_eq!(
            template_pool(Some(&Err(e.clone()))),
            Err(TemplateError::Failed(format!("CreateNewBlock: GetCreditPool: {e}")))
        );
    }

    /// An evo read V refuses as not built is the readiness refusal. Any other store error of V's
    /// own reads in the turn is a fault V stops on, as connect's (OD-CP-134 M2), whether the
    /// engine reported it (a raw read error) or not (an undecodable row, an inconsistency);
    /// `store_refusal`, for the reads still answered to the caller, keeps it Core's throw.
    #[test]
    fn evo_read_faults_map_to_connects_classes() {
        let failed = |e: StoreError| TemplateError::Failed(format!("CreateNewBlock: {e}"));
        let read = StoreError::read("memory error (Io): injected read failure");
        let undecodable = StoreError::read("undecodable mn_diff row: end of data");
        let inconsistent = StoreError::inconsistent("block-derived evo row rewrite");
        for e in [read, undecodable, inconsistent] {
            assert_eq!(store_refusal(432, e.clone()), failed(e.clone()));
            let fault = BlockError::Fault(format!("the LLMQ view: {e}"));
            let qc = OnVMembersError::Members(MemberError::View(e.clone()));
            assert_eq!(qc_refusal(432, qc), fault);
            assert_eq!(read_fault(432, "the LLMQ view", e), fault);
        }
        let not_ready = TemplateError::NotReady {
            height: 432,
            rule: "the LLMQ reads of a node without evo state",
            owner: EVO_READS_OWNER,
        };
        let not_built = StoreError::NotBuilt("the LLMQ reads of a node without evo state");
        assert_eq!(store_refusal(432, not_built.clone()), not_ready);
        assert_eq!(read_fault(432, "GetListForBlock", not_built.clone()), not_ready.clone().into());
        let qc = OnVMembersError::Members(MemberError::View(not_built));
        assert_eq!(qc_refusal(432, qc), not_ready.into());
    }

    /// `vTxFees[0]` is `-nFees`, `vTxSigOps[0]` the coinbase's legacy count; the commitments
    /// count zero; the selected transactions follow in block order.
    #[test]
    fn the_coinbase_heads_the_fee_and_sigop_lists() {
        use dash_consensus::primitives::{OutPoint, Transaction, Tx, TxHash, TxIn, TxOut, TxType};

        use crate::{EntryId, Selected};
        let script = vec![0x76, 0xa9, 0xac, 0xac];
        let coinbase = Tx::new(Transaction::new(
            3,
            TxType::new(5),
            vec![TxIn::new(OutPoint::new(TxHash::from([0; 32]), u32::MAX), vec![].into(), 0)],
            vec![TxOut::from_duffs(1, script.into())],
            0,
            Vec::new(),
        ));
        assert_eq!(coinbase.legacy_sigop_count(), 2);
        let selected = [(7, 1000, 4), (3, 250, 0)].map(|(id, fee, sigops)| Selected {
            id: EntryId(id),
            fee,
            sigops,
        });
        assert_eq!(
            tx_counters(&coinbase, 2, &selected, 1250),
            (vec![-1250, 0, 0, 1000, 250], vec![2, 0, 0, 4, 0])
        );
        assert_eq!(tx_counters(&coinbase, 0, &[], 0), (vec![0], vec![2]));
    }
}
