//! [`ChainTemplateSource`]: the node's [`BlockTemplateSource`] over V. One query turn reads what
//! `CreateNewBlock` reads under `cs_main` (the tip, its deployments, its median time past and the
//! next work); the template is then tested on V with `TestBlockValidity`, as Core tests it before
//! `CreateNewBlock` returns.

use std::sync::{Arc, Mutex};

use dash_args::{ArgsManager, RuntimeError};
use dash_clock::Clock;
use dash_consensus::ctx::{Deployment, DeploymentView as _};
use dash_consensus::header::HeaderParams;
use dash_consensus::pow::{PowParams, get_next_work_required};
use dash_consensus::subsidy::{SubsidyParams, block_subsidy_inner, genesis_reward};
use dash_params::{BuriedDeployment, ChainSetup, Network};
use dash_types::Numeric as _;
use dashd_chainstate::chain::{BlockState, ChainHandle, ChainQuery, NotTested};
use dashd_chainstate::connect::g1_rule;

use crate::assemble::{self, RESERVED_SIGOPS, RESERVED_SIZE, TipFacts, block_time};
use crate::{BlockTemplate, BlockTemplateSource, LastBlockStats, TemplateError, TemplateTxs};

/// The slice that assembles the blocks G1 refuses here.
const NOT_READY_OWNER: &str = "MN0-2";

/// The node's template source: V's handle, the network, the node clock (ND1's `MockClock` under
/// `setmocktime`) and `-blockversion` ([`block_version_arg`]). One per node: its
/// [`last_block_stats`](BlockTemplateSource::last_block_stats) are Core's process-wide
/// `m_last_block_num_txs` and `m_last_block_size`.
pub struct ChainTemplateSource {
    chain: ChainHandle,
    clock: Arc<dyn Clock>,
    block_version: Option<i32>,
    /// The network's proof-of-work and subsidy parameters, or why they cannot be built (the
    /// connector refuses the same chain parameters at its own construction).
    rules: Result<Rules, String>,
    last: Mutex<Option<LastBlockStats>>,
    #[cfg(feature = "test-support")]
    between_turns: Mutex<Option<BetweenTurns>>,
}

/// [`ChainTemplateSource::set_between_turns`]'s hook.
#[cfg(feature = "test-support")]
type BetweenTurns = Box<dyn FnMut() + Send>;

#[derive(Clone, Copy)]
struct Rules {
    pow: PowParams,
    subsidy: SubsidyParams,
}

impl Rules {
    fn of(params: &ChainSetup) -> Result<Rules, String> {
        let pow = *HeaderParams::from_chain_params(params)
            .map_err(|e| format!("header parameters: {e:?}"))?
            .pow();
        let reward = genesis_reward(&dash_consensus::params::genesis_block(params))
            .map_err(|e| format!("genesis: {e:?}"))?;
        let subsidy = SubsidyParams::from_chain_params(params, reward)
            .map_err(|e| format!("subsidy parameters: {e:?}"))?;
        Ok(Rules {
            pow,
            subsidy,
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
            rules: Rules::of(&params),
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
        let rules = *self.rules.as_ref().map_err(|e| TemplateError::Failed(e.clone()))?;
        loop {
            let now = self.clock.node_now().secs();
            let block_version = self.block_version;
            let facts = self
                .chain
                .query(move |q| tip_facts(q, now, block_version, &rules))
                .recv()
                .map_err(|_| TemplateError::NoChain)??;
            let block = Arc::new(assemble::block(&facts, coinbase_script));
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
            // :259-261: an empty mempool's block is the reserved counters, set (and logged)
            // before TestBlockValidity's verdict, so a refused template sets them too. Here they
            // follow V's answer, so a template rebuilt on a newer tip records them once.
            *self.last.lock().unwrap_or_else(|p| p.into_inner()) = Some(LastBlockStats {
                num_txs: 0,
                size: RESERVED_SIZE,
            });
            tracing::info!(
                "CreateNewBlock(): total size {RESERVED_SIZE} txs: 0 fees: 0 sigops {RESERVED_SIGOPS}"
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

/// What `CreateNewBlock` reads under `cs_main`, in one V turn, for the block after the tip at
/// `now`; G1's refusal where it holds.
fn tip_facts(
    q: &ChainQuery<'_>,
    now: i64,
    block_version: Option<i32>,
    rules: &Rules,
) -> Result<TipFacts, TemplateError> {
    // :194-197: Core asserts a tip; V has one once its chain loads.
    let (tip, _) = q.chain.private_tip().ok_or(TemplateError::NoChain)?;
    let chain = q
        .chain
        .complete()
        .ok_or_else(|| TemplateError::Failed("V's chain has a header window".to_owned()))?;
    let prev = Some(tip.id);
    let height = tip.height + 1;
    if let Some(rule) = g1_rule(&q.deployments, prev) {
        return Err(TemplateError::NotReady {
            height,
            rule,
            owner: NOT_READY_OWNER,
        });
    }
    // :209-214: computed even when `-blockversion` replaces it, as Core's is.
    let computed = q.deployments.compute_block_version(prev);
    let tip_header = chain.header(tip.id);
    let time = block_time(now, tip_header.mtp);
    // :346, after `UpdateTime`; regtest's recomputation inside `UpdateTime` reads the same.
    let bits = get_next_work_required(chain, tip.id, time, &rules.pow).to_base();
    // :271, the previous block's `nBits` and height. G1 refuses V20 above, so this is false.
    let v20 = q.deployments.active_after(prev, Deployment::Buried(BuriedDeployment::V20));
    let prev_height = i32::try_from(tip.height)
        .map_err(|_| TemplateError::Failed(format!("tip height {}", tip.height)))?;
    let subsidy = block_subsidy_inner(tip_header.bits.to_base(), prev_height, v20, &rules.subsidy)
        .map_err(|e| TemplateError::Failed(format!("GetBlockSubsidyInner: {e}")))?;
    Ok(TipFacts {
        prev: tip.hash,
        height,
        version: block_version.unwrap_or(computed),
        time,
        bits,
        subsidy,
    })
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
