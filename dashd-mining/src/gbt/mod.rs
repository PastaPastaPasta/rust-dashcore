//! Core's `getblocktemplate` without its JSON (PLAN-MN1 §3): [`GbtEngine::handle`] answers a
//! parsed [`GbtRequest`] with a typed [`GbtTemplate`] or a [`GbtError`], and the RPC layer encodes
//! both. The GBT mutex stands for the `cs_main` Core holds across the call; it is released only
//! for the longpoll wait, as Core releases `cs_main`.

mod cache;
mod fields;
mod longpoll;

use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use dash_clock::Clock;
use dash_consensus::Block;
use dash_consensus::ctx::DeploymentPos;
use dash_consensus::deployments::ThresholdState;
use dash_consensus::header::HeaderParams;
use dash_consensus::params::Superblocks;
use dash_consensus::payments::PaymentParams;
use dash_consensus::pow::PowParams;
use dash_params::ChainSetup;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{BlockState, ChainHandle, NotTested, WaitEnd, WaitError};

pub use fields::{GbtTemplate, GbtTx, gbt_vb_name, update_time};

use crate::{BlockTemplateSource, MiningMempool, NodeStatus, TemplateError};

use cache::GbtCache;

/// The request's `mode` (`rpc/mining.cpp:679-694`).
#[derive(Clone)]
pub enum Mode {
    /// `"template"`, or no mode.
    Template,
    /// `"proposal"` with its decoded `data` (`rpc/mining.cpp:697-724`).
    Proposal(Arc<Block>),
    /// Any other mode string, or a mode that is not a string.
    Invalid,
}

impl fmt::Debug for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::Template => f.write_str("Template"),
            Mode::Proposal(block) => f.debug_tuple("Proposal").field(&block.hash()).finish(),
            Mode::Invalid => f.write_str("Invalid"),
        }
    }
}

/// The request's `longpollid`, as the RPC layer parses it (`rpc/mining.cpp:765-779`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LongPoll {
    /// JSON null or absent: no wait.
    None,
    /// A string: `ParseHashV` of its first 64 characters and `LocaleIndependentAtoi<int64_t>` of
    /// the rest, wrapped to `unsigned int`.
    Id {
        tip: BlockHash,
        tx_updated: u32,
    },
    /// Any other JSON value: V's tip and the cache's `nTransactionsUpdatedLast`.
    NonString,
}

/// A parsed `getblocktemplate` request.
#[derive(Clone, Debug)]
pub struct GbtRequest {
    pub mode: Mode,
    /// `setClientRules`.
    pub rules: BTreeSet<String>,
    pub longpoll: LongPoll,
}

/// A `getblocktemplate` answer.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(clippy::large_enum_variant, reason = "one answer per call, moved once into the encoder")]
pub enum GbtReply {
    Template(GbtTemplate),
    /// The proposal's verdict, which the RPC layer maps with `BIP22ValidationResult`.
    Proposal(ProposalOutcome),
}

/// `propose_block`'s answer: `TestBlockValidity`'s state, or the answer Core gives without it.
pub type ProposalOutcome = Result<BlockState, NotTested>;

/// Why `getblocktemplate` made no template, in Core's check order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GbtError {
    /// `RPC_INVALID_PARAMETER` "Invalid mode" (`rpc/mining.cpp:736-737`).
    InvalidMode,
    /// `EnsureConnman` (`rpc/mining.cpp:739`, `rpc/server_util.cpp:137`).
    P2pDisabled,
    /// `RPC_CLIENT_NOT_CONNECTED` (`rpc/mining.cpp:742-744`).
    NotConnected,
    /// `RPC_CLIENT_IN_INITIAL_DOWNLOAD` (`rpc/mining.cpp:746-748`).
    InIbd,
    /// `RPC_CLIENT_IN_INITIAL_DOWNLOAD` (`rpc/mining.cpp:750-753`).
    Syncing,
    /// `EnsureMemPool` (`rpc/mining.cpp:757`, `rpc/server_util.cpp:50`).
    MempoolDisabled,
    /// `RPC_CLIENT_NOT_CONNECTED` "Shutting down" after a longpoll (`rpc/mining.cpp:802-803`).
    ShuttingDown,
    /// `RPC_INVALID_PARAMETER` for an active deployment the client did not list and that is not
    /// `gbt_force` (`rpc/mining.cpp:918-923`): its name.
    RuleRequired(String),
    /// `CreateNewBlock`'s throw (`RPC_MISC_ERROR`), or V gone (`EnsureChainman`'s
    /// `RPC_INTERNAL_ERROR`, `rpc/server_util.cpp:87-90`), with [`Self::message`]'s text.
    Template(TemplateError),
}

impl GbtError {
    /// The JSON-RPC error code Core answers with (`rpc/protocol.h`).
    pub fn code(&self) -> i32 {
        match self {
            GbtError::InvalidMode | GbtError::RuleRequired(_) => -8,
            GbtError::P2pDisabled => -31,
            GbtError::NotConnected | GbtError::ShuttingDown => -9,
            GbtError::InIbd | GbtError::Syncing => -10,
            GbtError::MempoolDisabled => -33,
            GbtError::Template(TemplateError::NoChain) => -32603,
            GbtError::Template(_) => -1,
        }
    }

    /// Core's message text, `package_name` standing for `PACKAGE_NAME`. For a [`TemplateError`]:
    /// the readiness refusal as `generate*` words it, Core's throw as is, and `EnsureChainman`'s
    /// text for V gone.
    pub fn message(&self, package_name: &str) -> String {
        match self {
            GbtError::InvalidMode => "Invalid mode".to_owned(),
            GbtError::P2pDisabled => {
                "Error: Peer-to-peer functionality missing or disabled".to_owned()
            }
            GbtError::NotConnected => format!("{package_name} is not connected!"),
            GbtError::InIbd => {
                format!("{package_name} is in initial sync and waiting for blocks...")
            }
            GbtError::Syncing => format!("{package_name} is syncing with network..."),
            GbtError::MempoolDisabled => "Mempool disabled or instance not found".to_owned(),
            GbtError::ShuttingDown => "Shutting down".to_owned(),
            GbtError::RuleRequired(name) => {
                format!("Support for '{name}' rule requires explicit client support")
            }
            GbtError::Template(e) => e.to_string(),
        }
    }
}

impl From<TemplateError> for GbtError {
    fn from(e: TemplateError) -> Self {
        GbtError::Template(e)
    }
}

/// What one V query turn reads for the refusals and the longpoll (`rpc/mining.cpp:741-754`,
/// `:777`), under one `cs_main`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipTurn {
    /// `ActiveChain().Tip()->GetBlockHash()`.
    pub tip: BlockHash,
    /// `ActiveChain().Height()`.
    pub height: u32,
    /// `IsInitialBlockDownload()`.
    pub ibd: bool,
}

/// The fields turn's input: the cached block's parent and its `nTime` before `UpdateTime`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldsAt {
    pub prev: BlockHash,
    pub time: u32,
    /// `NodeClock::now()`, the mock time under `setmocktime`.
    pub now: i64,
}

/// The fields turn's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldsTurn {
    /// V's tip is no longer the cached block's parent: the cache check runs again on it.
    Moved(BlockHash),
    At(TipFields),
}

/// What the fields read from V at the cached block's parent (`pindexPrev`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipFields {
    /// `pindexPrev->GetMedianTimePast()`.
    pub mtp: i64,
    /// `nTime` after `UpdateTime` ([`update_time`]).
    pub time: u32,
    /// `GetNextWorkRequired(pindexPrev, pblock)` at that time where the chain allows
    /// minimum-difficulty blocks (`node/miner.cpp:59-61`); `None` elsewhere.
    pub min_difficulty_bits: Option<u32>,
    /// `m_versionbitscache.State(pindexPrev, pos)` for each deployment the release has.
    pub versionbits: Vec<(DeploymentPos, ThresholdState)>,
    /// `DeploymentActiveAfter(pindexPrev, DEPLOYMENT_DIP0001)`.
    pub dip0001: bool,
}

/// The engine's side of V.
pub trait GbtChain: Send + Sync {
    /// One query turn: V's tip and `IsInitialBlockDownload()`.
    ///
    /// # Errors
    ///
    /// [`TemplateError::NoChain`] when V is gone or has no tip.
    fn tip_turn(&self) -> Result<TipTurn, TemplateError>;

    /// One query turn at `at.prev` for the template's fields; [`FieldsTurn::Moved`] if V's tip is
    /// another block.
    ///
    /// # Errors
    ///
    /// [`TemplateError::NoChain`] when V is gone, or a read V cannot answer.
    fn fields_turn(&self, at: FieldsAt) -> Result<FieldsTurn, TemplateError>;

    /// `g_best_block_cv`'s wait ([`TipWatch::wait_while`]).
    ///
    /// [`TipWatch::wait_while`]: dashd_chainstate::chain::TipWatch::wait_while
    ///
    /// # Errors
    ///
    /// [`WaitError::WaitOnVFromNoWaitTask`] from a no-wait task.
    fn wait_tip_while(
        &self,
        deadline: Instant,
        keep_waiting: &mut dyn FnMut(Option<BlockHash>) -> bool,
    ) -> Result<WaitEnd, WaitError>;

    /// The proposal's lookup and `TestBlockValidity` under one V hold
    /// ([`ChainHandle::propose_block`]).
    ///
    /// # Errors
    ///
    /// [`TemplateError::NoChain`] when V is gone.
    fn propose_block(&self, block: Arc<Block>) -> Result<ProposalOutcome, TemplateError>;
}

/// [`GbtChain`] over V's handle.
pub struct ChainGbt {
    chain: ChainHandle,
    pow: Arc<PowParams>,
    allow_min_difficulty: bool,
    /// The release's version-bits deployments.
    positions: Arc<[DeploymentPos]>,
}

impl ChainGbt {
    /// # Errors
    ///
    /// The chain parameters build no proof-of-work parameters.
    pub fn new(chain: ChainHandle, params: &ChainSetup) -> Result<ChainGbt, String> {
        let pow = *HeaderParams::from_chain_params(params)
            .map_err(|e| format!("header parameters: {e:?}"))?
            .pow();
        Ok(ChainGbt {
            chain,
            pow: Arc::new(pow),
            allow_min_difficulty: params.consensus.pow_allow_min_difficulty_blocks,
            positions: params.consensus.deployments.iter().map(|(pos, _)| pos).collect(),
        })
    }
}

impl GbtChain for ChainGbt {
    fn tip_turn(&self) -> Result<TipTurn, TemplateError> {
        self.chain
            .query(|q| {
                let (tip, _) = q.chain.private_tip()?;
                Some(TipTurn {
                    tip: tip.hash,
                    height: tip.height,
                    ibd: q.is_ibd(),
                })
            })
            .recv()
            .ok()
            .flatten()
            .ok_or(TemplateError::NoChain)
    }

    fn fields_turn(&self, at: FieldsAt) -> Result<FieldsTurn, TemplateError> {
        let pow = Arc::clone(&self.pow);
        let allow_min_difficulty = self.allow_min_difficulty;
        let positions = Arc::clone(&self.positions);
        self.chain
            .query(move |q| fields::tip_fields(q, at, &pow, allow_min_difficulty, &positions))
            .recv()
            .map_err(|_| TemplateError::NoChain)?
    }

    fn wait_tip_while(
        &self,
        deadline: Instant,
        keep_waiting: &mut dyn FnMut(Option<BlockHash>) -> bool,
    ) -> Result<WaitEnd, WaitError> {
        self.chain.tip_watch().wait_while(deadline, keep_waiting)
    }

    fn propose_block(&self, block: Arc<Block>) -> Result<ProposalOutcome, TemplateError> {
        self.chain.propose_block(block).recv().map_err(|_| TemplateError::NoChain)
    }
}

/// The chain parameters `getblocktemplate` reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GbtParams {
    /// `IsTestChain()`: testnet, devnet and regtest.
    pub is_test_chain: bool,
    /// `nSuperblockStartBlock` and `CSuperblock::IsValidBlockHeight`'s schedule.
    pub superblocks: Superblocks,
    /// `nMasternodePaymentsStartBlock`.
    pub masternode_payments_start: i32,
    /// The release's version-bits deployments, in position order.
    pub deployments: Vec<VbDeployment>,
}

/// A version-bits deployment as `getblocktemplate` reads it: `VersionBitsDeploymentInfo[pos]`
/// and `vDeployments[pos].bit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VbDeployment {
    pub pos: DeploymentPos,
    pub name: &'static str,
    pub gbt_force: bool,
    pub bit: u8,
}

impl GbtParams {
    /// # Errors
    ///
    /// The chain parameters build no payment parameters.
    pub fn of(params: &ChainSetup) -> Result<GbtParams, String> {
        let payments = PaymentParams::from_chain_params(params)
            .map_err(|e| format!("payment parameters: {e:?}"))?;
        let masternode_payments_start =
            i32::try_from(params.consensus.masternode_payments_start_block.to_u32())
                .map_err(|_| "the masternode payments start is past Core's int".to_owned())?;
        let deployments = params
            .consensus
            .deployments
            .iter()
            .map(|(pos, d)| {
                Ok(VbDeployment {
                    pos,
                    name: pos.name(),
                    gbt_force: pos.gbt_force(),
                    bit: u8::try_from(d.bit)
                        .ok()
                        .filter(|&bit| u32::from(bit) < u32::BITS)
                        .ok_or_else(|| format!("{} has bit {}", pos.name(), d.bit))?,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(GbtParams {
            is_test_chain: params.is_test_chain,
            superblocks: payments.config().superblocks,
            masternode_payments_start,
            deployments,
        })
    }
}

/// The longpoll's steady-clock steps: the first deadline after the call, then each extension
/// after a deadline with no mempool update (`rpc/mining.cpp:785`, `:797`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LongpollTimes {
    pub first: Duration,
    pub step: Duration,
}

impl Default for LongpollTimes {
    fn default() -> Self {
        LongpollTimes {
            first: Duration::from_secs(60),
            step: Duration::from_secs(10),
        }
    }
}

/// What the engine is built from.
pub struct GbtParts {
    pub chain: Arc<dyn GbtChain>,
    pub source: Arc<dyn BlockTemplateSource>,
    /// `node.mempool`: `None` answers [`GbtError::MempoolDisabled`].
    pub mempool: Option<Arc<dyn MiningMempool>>,
    pub status: Arc<dyn NodeStatus>,
    /// `GetTime()` and `NodeClock::now()`.
    pub clock: Arc<dyn Clock>,
    pub params: GbtParams,
}

/// `getblocktemplate`'s engine: one per node, as Core's function statics are one per process.
pub struct GbtEngine {
    parts: GbtParts,
    longpoll_times: LongpollTimes,
    /// The GBT mutex over Core's statics.
    cache: Mutex<GbtCache>,
}

impl GbtEngine {
    pub fn new(parts: GbtParts) -> GbtEngine {
        GbtEngine {
            parts,
            longpoll_times: LongpollTimes::default(),
            cache: Mutex::new(GbtCache::default()),
        }
    }

    /// Test support: shorter longpoll steps.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_longpoll_times(mut self, times: LongpollTimes) -> GbtEngine {
        self.longpoll_times = times;
        self
    }

    /// Core `getblocktemplate` (`rpc/mining.cpp:672-985` at v24.0.2) on a parsed request.
    /// Blocking: call it from an RPC worker thread, never from V's.
    ///
    /// # Errors
    ///
    /// [`GbtError`], in Core's check order.
    pub fn handle(&self, req: GbtRequest) -> Result<GbtReply, GbtError> {
        match req.mode {
            // Core: v24.0.2 rpc/mining.cpp:697-724; V's `propose_block` runs the duplicate lookup,
            // the not-best-prev answer and `TestBlockValidity` with PoW off and merkle on.
            Mode::Proposal(block) => {
                return Ok(GbtReply::Proposal(self.parts.chain.propose_block(block)?));
            }
            // :736-737
            Mode::Invalid => return Err(GbtError::InvalidMode),
            Mode::Template => {}
        }
        // Core: v24.0.2 rpc/mining.cpp:739
        if !self.parts.status.has_connman() {
            return Err(GbtError::P2pDisabled);
        }
        let mut cache = self.lock();
        let turn = self.parts.chain.tip_turn()?;
        self.refusals(&turn)?;
        // Core: v24.0.2 rpc/mining.cpp:757
        let mempool = self.parts.mempool.as_deref().ok_or(GbtError::MempoolDisabled)?;
        let mut tip = turn.tip;
        let watch = match req.longpoll {
            LongPoll::None => None,
            LongPoll::Id {
                tip,
                tx_updated,
            } => Some((tip, tx_updated)),
            LongPoll::NonString => Some((turn.tip, cache.tx_updated_last)),
        };
        if let Some((watched, tx_updated)) = watch {
            drop(cache);
            longpoll::wait(&self.parts, mempool, watched, tx_updated, self.longpoll_times)?;
            cache = self.lock();
            tip = self.parts.chain.tip_turn()?.tip;
        }
        // Core holds `cs_main` from the cache check to the reply; a tip that moved between the two
        // turns reruns the check on it, a serial order Core could have run.
        loop {
            let now = self.parts.clock.node_now().secs();
            cache.refresh(tip, mempool, &*self.parts.source, now)?;
            match fields::answer(&mut cache, &self.parts, now, &req.rules)? {
                fields::Answer::Template(t) => return Ok(GbtReply::Template(*t)),
                fields::Answer::Moved(moved) => tip = moved,
            }
        }
    }

    /// The refusal block (`rpc/mining.cpp:741-754`), off test chains only.
    fn refusals(&self, turn: &TipTurn) -> Result<(), GbtError> {
        if self.parts.params.is_test_chain {
            return Ok(());
        }
        if self.parts.status.peer_count() == 0 {
            return Err(GbtError::NotConnected);
        }
        if turn.ibd {
            return Err(GbtError::InIbd);
        }
        let next = i64::from(turn.height) + 1;
        let superblock = i32::try_from(next)
            .is_ok_and(|h| self.parts.params.superblocks.is_superblock_height(h));
        if !self.parts.status.mn_synced() && superblock {
            return Err(GbtError::Syncing);
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, GbtCache> {
        self.cache.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests;
