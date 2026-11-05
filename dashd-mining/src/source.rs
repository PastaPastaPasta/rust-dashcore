//! [`ChainTemplateSource`]: the node's [`BlockTemplateSource`] over V. One query turn reads what
//! `CreateNewBlock` reads under `cs_main` (the tip, its deployments, its median time past, the
//! next work and, once DIP0003 is active after the tip, the evo state [`evo::block`] assembles
//! from) and assembles the block; the template is then tested on V with `TestBlockValidity`, as
//! Core tests it before `CreateNewBlock` returns.

use std::sync::{Arc, Mutex};

use dash_args::{ArgsManager, RuntimeError};
use dash_clock::Clock;
use dash_consensus::Block;
use dash_consensus::ctx::{Deployment, DeploymentView as _, bls_ctx_for_block};
use dash_consensus::header::HeaderParams;
use dash_consensus::payments::PaymentParams;
use dash_consensus::pow::{PowParams, get_next_work_required};
use dash_consensus::subsidy::{SubsidyParams, block_subsidy_inner, genesis_reward};
use dash_dml::StoreError;
use dash_dml::creditpool::{CreditPool, PoolState};
use dash_evo::llmq::MemberError;
use dash_params::{BuriedDeployment, ChainSetup, Network};
use dash_types::Numeric as _;
use dashd_chainstate::chain::{BlockState, ChainHandle, ChainQuery, NotTested, OnVMembersError};

use crate::assemble::{self, RESERVED_SIGOPS, RESERVED_SIZE, TipFacts, block_time};
use crate::evo::{self, EvoChain};
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
    #[cfg(feature = "test-support")]
    between_turns: Mutex<Option<BetweenTurns>>,
}

/// [`ChainTemplateSource::set_between_turns`]'s hook.
#[cfg(feature = "test-support")]
type BetweenTurns = Box<dyn FnMut() + Send>;

struct Rules {
    pow: PowParams,
    subsidy: SubsidyParams,
    payments: PaymentParams,
    params: Arc<ChainSetup>,
}

impl Rules {
    fn of(params: Arc<ChainSetup>) -> Result<Rules, String> {
        let pow = *HeaderParams::from_chain_params(&params)
            .map_err(|e| format!("header parameters: {e:?}"))?
            .pow();
        let reward = genesis_reward(&dash_consensus::params::genesis_block(&params))
            .map_err(|e| format!("genesis: {e:?}"))?;
        let subsidy = SubsidyParams::from_chain_params(&params, reward)
            .map_err(|e| format!("subsidy parameters: {e:?}"))?;
        let payments = PaymentParams::from_chain_params(&params)
            .map_err(|e| format!("payment parameters: {e:?}"))?;
        Ok(Rules {
            pow,
            subsidy,
            payments,
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
            rules: Rules::of(params).map(Arc::new),
            last: Mutex::new(None),
            #[cfg(feature = "test-support")]
            between_turns: Mutex::new(None),
        }
    }

    /// Test support: `hook` runs on every build between the query turn and `TestBlockValidity`,
    /// the window where Core holds `cs_main` and V does not, so a test can connect a block there.
    #[cfg(feature = "test-support")]
    pub fn set_between_turns(&self, hook: impl FnMut() + Send + 'static) {
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
    // Core: node/miner.cpp:176-363 BlockAssembler::CreateNewBlock
    fn create_new_block(
        &self,
        coinbase_script: &[u8],
        // An empty mempool selects nothing either way (MP1a).
        _txs: TemplateTxs,
    ) -> Result<BlockTemplate, TemplateError> {
        let rules = self.rules.as_ref().map_err(|e| TemplateError::Failed(e.clone()))?;
        loop {
            let now = self.clock.node_now().secs();
            let block_version = self.block_version;
            let rules = Arc::clone(rules);
            let script = coinbase_script.to_vec();
            let built = self
                .chain
                .query(move |q| tip_block(q, now, block_version, &rules, &script))
                .recv()
                .map_err(|_| TemplateError::NoChain)??;
            let facts = built.facts;
            let block = Arc::new(built.block);
            #[cfg(feature = "test-support")]
            if let Some(hook) =
                self.between_turns.lock().unwrap_or_else(|p| p.into_inner()).as_mut()
            {
                hook();
            }
            // :351-354, PoW and merkle off.
            let state =
                match self.chain.test_block_validity(Arc::clone(&block), false, false).recv() {
                    Err(_) => return Err(TemplateError::NoChain),
                    // A block reached V between the two turns, where Core holds `cs_main` across
                    // both: build on the new tip, a serial order Core could have run. The loop
                    // ends when V's tip holds still for two turns, or with V.
                    Ok(Err(NotTested::NotBestPrev)) => continue,
                    // GBT's lookup answers; this call makes no lookup.
                    Ok(Err(other)) => BlockState::Error(other.to_string()),
                    Ok(Ok(state)) => state,
                };
            // :259-261: the counters are set (and logged) before TestBlockValidity's verdict, so
            // a refused template sets them too. Here they follow V's answer, so a template rebuilt
            // on a newer tip records them once.
            let stats = built.stats;
            *self.last.lock().unwrap_or_else(|p| p.into_inner()) = Some(stats);
            tracing::info!(
                "CreateNewBlock(): total size {} txs: {} fees: 0 sigops {RESERVED_SIGOPS}",
                stats.size,
                stats.num_txs
            );
            if !state.is_valid() {
                return Err(TemplateError::Failed(format!(
                    "CreateNewBlock: TestBlockValidity failed: {state}"
                )));
            }
            return Ok(BlockTemplate {
                block: dash_consensus::Block::new(*block.header(), block.txs().to_vec()),
                prev: facts.prev,
                height: facts.height,
            });
        }
    }

    fn last_block_stats(&self) -> Option<LastBlockStats> {
        *self.last.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// One V turn's template: the block and the counters `CreateNewBlock` sets for it.
struct Built {
    facts: TipFacts,
    block: Block,
    stats: LastBlockStats,
}

/// `CreateNewBlock`'s block after the tip at `now`, paying the miner's part to `script`, from
/// what it reads under `cs_main`, all in one V turn: below DIP0003 the coinbase alone
/// (`assemble::block`), from it [`evo::block`] over V's evo state at the tip.
fn tip_block(
    q: &ChainQuery<'_>,
    now: i64,
    block_version: Option<i32>,
    rules: &Rules,
    script: &[u8],
) -> Result<Built, TemplateError> {
    let (facts, prev) = tip_facts(q, now, block_version, rules)?;
    if !q.deployments.active_after(Some(prev), Deployment::Buried(BuriedDeployment::Dip0003)) {
        // :259-261: an empty mempool's block is the reserved counters.
        return Ok(Built {
            facts,
            block: assemble::block(&facts, script),
            stats: LastBlockStats {
                num_txs: 0,
                size: RESERVED_SIZE,
            },
        });
    }
    let view = q.evo_view();
    let refused = |e: StoreError| store_refusal(facts.height, e);
    // Core: v24.0.0-rc.5 masternode/payments.cpp:152 GetListForBlock(pindexPrev)
    let list = view.tip_list().map_err(refused)?;
    // Core: v24.0.0-rc.5 node/miner.cpp:520 GetCreditPool(pindexPrev), the default below V20
    let credit_pool = template_pool(view.credit_pool().map_err(refused)?.as_deref())?;
    // The pool's leaf lock is held only to copy it, in this V turn, never across a read.
    let pool = view.mineable();
    let bls = bls_ctx_for_block(&rules.params, facts.height);
    let evo = view
        .with_qc(bls, |qc| {
            let chain = EvoChain {
                deployments: &q.deployments,
                qc,
                pool: &pool,
                list: &list,
                chainlock: false,
                superblock_triggered: false,
                credit_pool: &credit_pool,
                payments: &rules.payments,
            };
            evo::block(&facts, prev, script, &chain)
        })
        .map_err(|e| qc_refusal(facts.height, e))??;
    Ok(Built {
        facts,
        block: evo.block,
        stats: evo.stats,
    })
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
fn store_refusal(height: u32, e: StoreError) -> TemplateError {
    match e {
        StoreError::NotBuilt(rule) => TemplateError::NotReady {
            height,
            rule,
            owner: EVO_READS_OWNER,
        },
        e => TemplateError::Failed(format!("CreateNewBlock: {e}")),
    }
}

/// [`store_refusal`] for `with_qc`'s own error.
fn qc_refusal(height: u32, e: OnVMembersError) -> TemplateError {
    match e {
        OnVMembersError::Members(MemberError::View(e)) => store_refusal(height, e),
        e => TemplateError::Failed(format!("CreateNewBlock: {e}")),
    }
}

/// What `CreateNewBlock` reads under `cs_main` for the block after the tip at `now`, with the
/// tip's id.
fn tip_facts(
    q: &ChainQuery<'_>,
    now: i64,
    block_version: Option<i32>,
    rules: &Rules,
) -> Result<(TipFacts, dash_consensus::BlockId), TemplateError> {
    // :194-197: Core asserts a tip; V has one once its chain loads.
    let (tip, _) = q.chain.private_tip().ok_or(TemplateError::NoChain)?;
    let chain = q
        .chain
        .complete()
        .ok_or_else(|| TemplateError::Failed("V's chain has a header window".to_owned()))?;
    let prev = Some(tip.id);
    let height = tip.height + 1;
    // :209-214: computed even when `-blockversion` replaces it, as Core's is.
    let computed = q.deployments.compute_block_version(prev);
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
    Ok((
        TipFacts {
            prev: tip.hash,
            height,
            version: block_version.unwrap_or(computed),
            time,
            bits,
            subsidy,
        },
        tip.id,
    ))
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

#[cfg(test)]
mod tests {
    use dash_dml::StoreError;
    use dash_evo::llmq::MemberError;
    use dashd_chainstate::chain::OnVMembersError;

    use super::{EVO_READS_OWNER, qc_refusal, store_refusal, template_pool};
    use crate::TemplateError;

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

    /// B-2 (OD-CP-20): an evo read V refuses as not built is the readiness refusal; every other
    /// store fault is Core's throw out of `CreateNewBlock`, whether the engine reported it (a raw
    /// read error) or not (an undecodable row, an inconsistency), and the source reports nothing.
    #[test]
    fn evo_read_faults_map_to_b2s_classes() {
        let failed = |e: StoreError| TemplateError::Failed(format!("CreateNewBlock: {e}"));
        let read = StoreError::read("memory error (Io): injected read failure");
        let undecodable = StoreError::read("undecodable mn_diff row: end of data");
        let inconsistent = StoreError::inconsistent("block-derived evo row rewrite");
        for e in [read, undecodable, inconsistent] {
            assert_eq!(store_refusal(432, e.clone()), failed(e.clone()));
            let qc = OnVMembersError::Members(MemberError::View(e.clone()));
            assert_eq!(qc_refusal(432, qc), failed(e));
        }
        let not_ready = TemplateError::NotReady {
            height: 432,
            rule: "the LLMQ reads of a node without evo state",
            owner: EVO_READS_OWNER,
        };
        let not_built = StoreError::NotBuilt("the LLMQ reads of a node without evo state");
        assert_eq!(store_refusal(432, not_built.clone()), not_ready);
        let qc = OnVMembersError::Members(MemberError::View(not_built));
        assert_eq!(qc_refusal(432, qc), not_ready);
    }
}
