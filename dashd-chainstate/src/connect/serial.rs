//! CH3a's `ConnectBlock` (CH3 §2.2, §2.3): Core's steps in Core's order over one attempt's
//! [`BlockOverlay`], with the evo side behind [`AttemptSteps`] and the script checks on CS6's check
//! queue (CH3 §2.9, CH3a D127).
//!
//! The bracket around it is the connector's: it opens the attempt and its evo transaction, runs
//! [`SerialCore::connect`], applies the attempt's effects (`finish_attempt`) and only then commits
//! the overlay and the evo transaction (D33). The core returns the verdict and leaves the overlay
//! uncommitted; on [`ConnectOutcome::Connected`] the bracket commits it with the block as its best
//! block (`view.SetBestBlock`, `validation.cpp:2538`, `:2912`).
//!
//! `TestBlockValidity`'s `ConnectBlock(…, fJustCheck = true)` runs the same steps on a dummy
//! [`BlockAt`] with no index id ([`Finish::JustCheck`], CH3 §2.14): every per-block question
//! goes through the block's parent, and it returns at step 21 (`v:2883-2884`).
//!
//! Semantics are Dash Core v24.0.0-rc.3's; `v:N` is `validation.cpp` line N at that tag.

use std::sync::Arc;

use bumpalo::Bump;
use dash_chain::{ChainReader, EntryView, Status};
use dash_consensus::coins::compress::MAX_SPECIAL_SCRIPT_LEN;
use dash_consensus::coins::{CoinsView, ViewOr};
use dash_consensus::connect::{AssumeValidFacts, has_lock_inputs, is_bip30_repeat, script_checks};
use dash_consensus::ctx::{CheckpointView, Deployment, DeploymentPos, DeploymentView};
use dash_consensus::header::HeaderParams;
use dash_consensus::payments::MnRewardEra;
use dash_consensus::pow::block_proof_equivalent_time;
use dash_consensus::primitives::{OutPoint, write_compact_size};
use dash_consensus::script::{
    BuriedScriptRules, CheckInputsError, ScriptFlags, ScriptPolicy, block_script_flags,
};
use dash_consensus::subsidy::{SubsidyError, SubsidyParams, block_subsidy};
use dash_consensus::tx::{
    LockTimeFlags, block_lock_time_flags, check_tx_inputs, max_block_sigops, sequence_locks,
    transaction_sigop_count,
};
use dash_consensus::{
    Amount, Block, BlockId, BlockValidationError, BlockValidationResult, BlsCtx, ChainAncestry,
    ConsensusCtx, HeaderCtx, Tx, TxValidationError, money_range,
};
use dash_evo::EffectSink;
use dash_evo::payments::{
    BlockPayee, BlockValue, Ev3Branch, PaymentsError, PaymentsParams, SuperblockCheck,
    block_payee_valid, block_value_valid, superblock_payee_valid,
};
use dash_kv::AbortSink as _;
use dash_num::CompactTarget;
use dash_params::{BuriedDeployment, ChainParams};
use dash_primitives::BlockHash;
use dashd_store::KvEngine;
#[cfg(feature = "planted-bugs")]
use dashd_store::flatfile::BLOCK_SERIALIZATION_HEADER_SIZE;
use dashd_store::flatfile::{FlatFileError, FlatFs};

use super::evo::{BlockAt, SpecialTxFailure, SpecialTxRules};
use super::guard::{g1_capability, g1_rule, g2_refusal_text, refusal_text};
use super::helper::{ChainHelper, HelperGov};
use super::tx_coins::TxCoins;
use crate::chain::{AbortPath, BlockStatusWriter, ConnectCx, ConnectOutcome, Fatal};
use crate::checkqueue::script::{
    ScriptCheck, ScriptOptions, TxSlot, check_input_scripts_unprobed, check_queue_failed,
    exec_cache_probe, input_scripts_failed,
};
use crate::checkqueue::{CheckQueue, Control};
use crate::coins::{BlockOverlay, CoinSrc, OutPointKey};
use crate::error::CoinsError;
use crate::sigcache::ScriptCaches;
use crate::store::{CoinsReadError, GuardedFiles};

/// `AbortNode`'s text for a `CheckBlock` that finds the stored block mutated (`v:2510`).
pub const CORRUPT_BLOCK: &str =
    "Corrupt block found indicating potential hardware failure; shutting down";

/// `EvoDbInconsistencyMessage()` (`v:1923-1929`) on a node with one chainstate: rdashd has no
/// assumeutxo, so never the dual-chainstate text.
pub const EVODB_INCONSISTENCY: &str = "Found EvoDB inconsistency, you must reindex to continue";

/// Core's `FlushUndoFile` text, as ST2's undo flush reports it (`blockstorage.cpp:581`).
pub const FLUSH_UNDO_FAILED: &str =
    "Flushing undo file to disk failed. This is likely the result of an I/O error.";

/// Core's `FindUndoPos` text when `CheckDiskSpace` fails (`blockstorage.cpp:739`).
pub const DISK_SPACE_LOW: &str = "Disk space is too low!";

/// Core's `WriteUndoDataForBlock` text after a failed `UndoWriteToDisk` (`blockstorage.cpp:784`).
pub const WRITE_UNDO_FAILED: &str = "Failed to write undo data";

/// What D63 refuses at `IsBlockPayeeValid`'s masternode half without complete evo hooks.
pub const MN_PAYEE_CAPABILITY: &str = "masternode payee validation (CH7a)";

/// What D63 refuses at a triggered superblock until EV3 lands.
pub const SUPERBLOCK_CAPABILITY: &str = "superblock validation (EV3, GV1b)";

/// The evo side of one connect attempt as the serial core calls it (CH3 §3.1): hooks H1, H2 and
/// H4–H8 with the attempt and its evo target bound, and the attempt's effect log. The bracket
/// implements it over an [`EvoHooks`](super::EvoHooks) attempt, so the core never sees the
/// attempt's shape (EV addendum §2.6).
pub trait AttemptSteps {
    /// `EvoHooks::COMPLETE`: false lets readiness guard G1 refuse (CH3 §2.17).
    fn complete(&self) -> bool;

    /// H1: the BLS scheme for the block (`v:2490-2491`).
    fn bls_ctx(&mut self, params: &ChainParams, at: &BlockAt) -> BlsCtx;

    /// H2: `m_evoDb.VerifyBestBlock(prev)` (`v:2527`).
    fn verify_best_block(&mut self, prev: &BlockHash) -> Result<bool, Fatal>;

    /// H4: `ProcessSpecialTxsInBlock` (`v:2663`). The hook's list updates stay with the
    /// implementation for H7.
    #[expect(
        clippy::too_many_arguments,
        reason = "Core's ProcessSpecialTxsInBlock arguments without the chain and the state"
    )]
    fn process_special_txs(
        &mut self,
        block: &Block,
        at: &BlockAt,
        rules: SpecialTxRules,
        view: &dyn CoinsView<Error = CoinsReadError>,
        subsidy: Amount,
        just_check: bool,
        check_cbtx_merkle_roots: bool,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal>;

    /// H8: `CheckSharedCollateralSpends(tx, view)` (`v:2712`).
    fn shared_collateral_spends(
        &mut self,
        tx: &Tx,
        view: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal>;

    /// H5: `IsTransactionValid` (`masternode/payments.cpp:397`).
    fn mn_payment_valid(
        &mut self,
        coinbase: &Tx,
        prev: Option<BlockId>,
        subsidy: Amount,
        fees: Amount,
        era: MnRewardEra,
        strict: bool,
    ) -> Result<bool, Fatal>;

    /// H6: `m_evoDb.WriteBestBlock(hash)` (`v:2913`).
    fn set_best_block(&mut self, hash: &BlockHash) -> Result<(), Fatal>;

    /// H1: `bls_scheme_guard.Commit()` (`v:2916`).
    fn commit_bls_scheme(&mut self);

    /// H7: `NotifyMasternodeListChanged` with H4's list updates (`v:2918-2921`), or with H9's
    /// when `undo` (`v:2368-2371`).
    fn list_changed(&mut self, undo: bool);

    /// H9: `UndoSpecialTxsInBlock` (`v:2309`); false is Core's. The hook's list updates stay with
    /// the implementation for H7.
    fn undo_special_txs(&mut self, block: &Block, at: &BlockAt) -> Result<bool, Fatal>;

    /// The attempt's effect log (A1 §2.10.1).
    fn sink(&mut self) -> &mut dyn EffectSink;
}

/// Buffers kept across blocks (CH3 §2.9, §2.10.1).
#[derive(Debug, Default)]
pub struct BlockScratch {
    /// The block's `CBlockUndo`, streamed as `UpdateCoins` spends (CH3 §2.10.1).
    undo: Vec<u8>,
    /// `prevheights` (`v:2726-2729`).
    prev_heights: Vec<u32>,
    /// The block's spent scripts for its queued checks (CH3 §2.9, D51).
    spent: SpentScripts,
    /// One transaction's spent scripts for its inline checks (`-par=1`), freed after it: Core's
    /// serial `CScriptCheck` frees its copy with its input (`v:2194`; CH3a D129).
    inline: SpentScripts,
    /// The rev record a disconnect reads (CH3 §2.10.2).
    pub(super) undo_read: Vec<u8>,
    /// The arena of a transaction's [`TxCoins`], reset before each one (CH3a D187).
    coins: Bump,
    /// How many transactions of the last block built their signature-hash data (tests only).
    #[cfg(test)]
    sig_data: usize,
}

/// CH3 §2.9's `SpentScripts` (D51; CH3a D129): the spent scripts of script-checked inputs,
/// decompressed, copied on an exec-cache miss only (D43), in a [`Bump`] kept across blocks. Core
/// copies each spent `CTxOut` into its `CScriptCheck` (`v:2194`) because `UpdateCoins` spends the
/// coin before the queue runs. The queued checks borrow the block's copies until `Wait`, and
/// [`Self::reset`] takes `&mut`, so it runs only once the control scope has ended; inline checks
/// end with their transaction, whose copies are reset then.
#[derive(Debug, Default)]
struct SpentScripts {
    arena: Bump,
    /// How many inputs had their spent script copied since the last block began (tests only).
    #[cfg(test)]
    copied: std::cell::Cell<usize>,
}

/// The most [`SpentScripts::reset`] keeps across blocks (CH3a D129). A block of standard spends
/// copies under 4 MiB, and a kept chunk it overflows gains one twice its size, so its arena stays
/// under 12 MiB; a larger one, which only nonstandard scripts fill, is freed.
const SPENT_SCRIPTS_KEPT: usize = 16 << 20;

impl SpentScripts {
    /// `tx`'s spent scripts in input order, from the coins Core's `AccessCoin` reads (`v:2184`), a
    /// coin created earlier in the block included.
    fn copy<'a, E: KvEngine>(
        &'a self,
        coins: &TxCoins<'_, E>,
        tx: &Tx,
    ) -> Result<&'a [&'a [u8]], Fatal> {
        let scripts = self.arena.alloc_slice_fill_copy(tx.inputs().len(), &[][..]);
        let mut buf = [0; MAX_SPECIAL_SCRIPT_LEN];
        for (script, input) in scripts.iter_mut().zip(tx.inputs()) {
            let coin = coins.access(&input.prevout)?;
            *script = self.arena.alloc_slice_copy(coin.script_pubkey().expand(&mut buf));
        }
        #[cfg(test)]
        self.copied.set(self.copied.get() + scripts.len());
        Ok(scripts)
    }

    /// Frees the block's copies. `Bump::reset` keeps its last chunk only, so copies that took
    /// several chunks are given one as large as all of them: a block of the same size then
    /// allocates nothing. Past [`SPENT_SCRIPTS_KEPT`] nothing is kept (CH3a D129).
    fn reset(&mut self) {
        let capacity = self.arena.allocated_bytes();
        if capacity > SPENT_SCRIPTS_KEPT {
            self.arena = Bump::new();
            return;
        }
        self.arena.reset();
        if self.arena.allocated_bytes() < capacity {
            self.arena = Bump::with_capacity(capacity);
        }
    }
}

/// How steps 15 and 16 (`v:2661-2768`) ended, inside the control scope.
enum Step {
    /// Every transaction passed L1–L8 (with threads, its scripts queued): the block's fees.
    Done(Amount),
    /// A verdict before the loop's end. It is returned once the control has waited, and the queue's
    /// verdict is ignored (Core's control destructor, CH3 §2.4).
    Stop(ConnectOutcome),
    /// H4's readiness refusal (CH7 D2), naming the capability: refused as [`Step::Stop`] is
    /// returned, once the control has waited, by `refuse` (G1's reporting, CH3a D121).
    Refuse(&'static str),
}

/// Core's `CCheckQueueControl control(fScriptChecks && parallel_script_checks ?
/// &scriptcheckqueue : nullptr)` around `run`: CS6's control over the queue's threads when
/// `parallel`, else the inline one (D41). `run`'s result and the queue's verdict. Inlined, with
/// `run`, so that the serial loop keeps its state in `connect_at`'s frame (DEC-300a).
// Core: validation.cpp:2649, :2778
#[inline(always)]
fn with_control<'s, R>(
    queue: &CheckQueue,
    parallel: bool,
    run: impl FnOnce(&Control<'_, 's, ScriptCheck<'s>>) -> R,
) -> (R, bool) {
    if parallel {
        queue.control(run)
    } else {
        queue.control_inline(run)
    }
}

/// L7 for one non-coinbase transaction with `fScriptChecks` (CH3 §2.3, §2.9): the exec-cache probe
/// first (D43); on a miss, its spent scripts copied from `coins` into `spent`, then checked here
/// (`queue` `None`, Core's `pvChecks == nullptr`) or queued.
// Core: validation.cpp:2752 CheckInputScripts(…, txsdata[i], parallel ? &vChecks : nullptr)
fn input_scripts<'s, E: KvEngine>(
    slot: &'s TxSlot<'s>,
    coins: &TxCoins<'_, E>,
    spent: &'s SpentScripts,
    opts: &ScriptOptions<'s>,
    queue: Option<&Control<'_, 's, ScriptCheck<'s>>>,
) -> Result<Result<(), CheckInputsError>, Fatal> {
    let Some(miss) = exec_cache_probe(slot, opts) else {
        return Ok(Ok(()));
    };
    let scripts = spent.copy(coins, slot.tx())?;
    Ok(check_input_scripts_unprobed(miss, |j| scripts[j], opts, queue))
}

/// Everything `ConnectBlock` reads besides the call's arguments, the attempt and the stores: the
/// parameters derived once, the script caches and check queue, the decision seam, V's abort path,
/// `-assumevalid` and the scratch.
pub struct SerialCore {
    hp: HeaderParams,
    payments: PaymentsParams,
    /// The process's one `ScriptCaches` (CH3 D10).
    caches: Arc<ScriptCaches>,
    /// CS6's queue, `-par`'s threads (CH3 §2.9; CH3a D127).
    queue: Arc<CheckQueue>,
    helper: Box<dyn ChainHelper>,
    pub(super) abort: AbortPath,
    /// `-assumevalid`; `None` is `-assumevalid=0`.
    assume_valid: Option<BlockHash>,
    pub(super) scratch: BlockScratch,
    /// A proposal's `ConnectBlock` ended in an unreported readiness refusal (CH3a D121): set by
    /// [`Self::refuse`], taken by the connector ([`Self::take_refused`]).
    refused: bool,
    /// The last `ConnectBlock`'s `fScriptChecks` (`v:2542-2571`), `None` when it returned before
    /// deciding: the digest seam's observable (CH3a D151). Nothing reads it to decide.
    script_checks: Option<bool>,
    /// `-consensusxcheck` ([`Self::set_xcheck`]).
    xcheck: bool,
    /// A planted bug (CH3a a3's CRASH witness): T12 only.
    #[cfg(feature = "planted-bugs")]
    planted: Option<crate::Planted>,
    /// Planted T12's undo record, written at the next connect's undo write: the record, its
    /// file, its block's height and the parent's hash.
    #[cfg(feature = "planted-bugs")]
    t12_pending: Option<(Vec<u8>, u32, u32, BlockHash)>,
}

// Core: validation.cpp:1886
/// `GetBlockSubsidy` of genesis, `block`: `Params().GenesisBlock().vtx[0]->GetValueOut()`.
fn genesis_subsidy(block: &Block) -> Amount {
    let coinbase = block.txs().first().map(|cb| cb.outputs()).unwrap_or_default();
    coinbase.iter().map(|o| o.value).sum()
}

/// `v:2617-2622`: "make sure old budget is the real one". The block at `nSuperblockStartBlock`
/// must be the one `nSuperblockStartHash` names, on a network that names one (mainnet and
/// testnet; `uint256()` elsewhere).
pub(crate) fn wrong_superblock_start(params: &ChainParams, at: &BlockAt) -> bool {
    let (sb_height, sb_hash) = params.consensus.superblock_start;
    at.height == sb_height.to_u32()
        && sb_hash.as_bytes() != &[0; 32]
        && at.hash.as_bytes() != sb_hash.as_bytes()
}

// Core: validation.cpp:2626-2632 (nLockTimeFlags, GetBlockScriptFlags :2419-2448)
/// The lock-time and script flags of the block after `prev`. Core asks each buried deployment
/// `DeploymentActiveAt(*pindex)`, which for a block on `prev` is `DeploymentActiveAfter(prev)`
/// (`deploymentstatus.h:14-50`: `nHeight >= H` is `pprev->nHeight + 1 >= H`), so the dummy of
/// `TestBlockValidity` needs no index id (CH3 §2.14, CH3a D119). DERSIG, CLTV, CSV and BIP147
/// are asked in Core's order.
fn flags_after(
    deployments: &dyn DeploymentView,
    prev: Option<BlockId>,
) -> (LockTimeFlags, ScriptFlags) {
    let after = |d| deployments.active_after(prev, Deployment::Buried(d));
    let lock = block_lock_time_flags(after(BuriedDeployment::Csv));
    let script = block_script_flags(BuriedScriptRules {
        dersig: after(BuriedDeployment::DerSig),
        cltv: after(BuriedDeployment::Cltv),
        csv: after(BuriedDeployment::Csv),
        bip147: after(BuriedDeployment::Bip147),
    });
    (lock, script)
}

/// How a `ConnectBlock` ends once its checks pass: Core's `fJustCheck` (`v:2883-2884`).
pub(super) enum Finish<'s, 'w> {
    /// `fJustCheck = false`: the indexed block, its `status` and `file` as the call starts; its
    /// undo written and its status raised through `st`, the evo best block set.
    Index {
        status: Status,
        file: Option<i32>,
        st: &'s mut BlockStatusWriter<'w>,
    },
    /// `fJustCheck = true`: `TestBlockValidity`'s dummy returns valid at step 21, before the undo
    /// write; no status, no undo, no evo best block, and the caches store (`v:2750`).
    JustCheck,
}

/// A coins read error: the store has tripped the latch and reported (CH2 §2.4.4, CH3a D60).
pub(super) fn read_failed(e: CoinsReadError) -> Fatal {
    Fatal::reported(format!("coins read: {e}"))
}

// Core: validation.cpp:2598-2601
/// Whether `ConnectBlock` still probes BIP30 for the block after `prev` (CH3 §2.5, CH3a D59):
/// unless `pprev->GetAncestor(BIP34Height)` exists and is the `BIP34Hash` block. That ancestor
/// is `prev`'s, so the BIP34 block itself is probed and the skip starts at the block after it.
fn bip30_probed(params: &ChainParams, chain: &dyn ChainAncestry, prev: BlockId) -> bool {
    let (height, hash) = params.consensus.bip34;
    !chain
        .ancestor(prev, height.to_u32())
        .is_some_and(|a| chain.header(a).hash.as_bytes() == hash.as_bytes())
}

/// A coins mutation error: a read the store reported, or Core's `logic_error`, which V reports.
pub(super) fn coins_failed(e: CoinsError) -> Fatal {
    match e {
        CoinsError::Read(e) => read_failed(e),
        e => Fatal::new(format!("coins: {e}")),
    }
}

fn invalid(result: BlockValidationResult, reason: &'static str) -> ConnectOutcome {
    ConnectOutcome::Invalid(BlockValidationError::new(result, reason))
}

pub(super) fn entry(reader: &ChainReader, id: BlockId) -> Result<EntryView, Fatal> {
    reader.entry(id).ok_or_else(|| Fatal::new(format!("ConnectBlock: no index entry {}", id.0)))
}

/// The block index as the consensus checks walk it, through V's reader: inside the hold every
/// entry a connect asks about is published (the block and its ancestors).
struct ReaderAncestry<'a>(&'a ChainReader);

/// `GetBlockSubsidy(pindex)` for the block after `prev`, as connect reads it: the parent's header
/// through V's reader and `DeploymentActiveAt(pindex, V20)`, which is `DeploymentActiveAfter(prev)`;
/// genesis's reward for `None`. The RPCs (`getblockstats`) read it here too.
// Core: validation.cpp:1884-1889 GetBlockSubsidy
pub fn block_subsidy_after(
    reader: &ChainReader,
    deployments: &dyn DeploymentView,
    prev: Option<BlockId>,
    p: &SubsidyParams,
) -> Result<Amount, SubsidyError> {
    subsidy_after(&ReaderAncestry(reader), deployments, prev, p)
}

/// [`block_subsidy_after`] over any ancestry.
fn subsidy_after(
    chain: &dyn ChainAncestry,
    deployments: &dyn DeploymentView,
    prev: Option<BlockId>,
    p: &SubsidyParams,
) -> Result<Amount, SubsidyError> {
    let v20 = deployments.active_after(prev, Deployment::Buried(BuriedDeployment::V20));
    block_subsidy(prev.map(|id| chain.header(id)).as_ref(), v20, p)
}

impl ChainAncestry for ReaderAncestry<'_> {
    fn header(&self, id: BlockId) -> HeaderCtx {
        let e = crate::chain::index::entry(self.0, id);
        HeaderCtx {
            height: e.height,
            hash: e.hash,
            time: e.header.time,
            bits: CompactTarget::new(e.header.bits),
            version: e.header.version,
            mtp: e.mtp,
            chain_work: e.chain_work,
        }
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        self.0.ancestor(id, height)
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        crate::chain::index::entry(self.0, id).prev
    }
}

/// `SequenceLocks` reads no checkpoint.
struct NoCheckpoints;

impl CheckpointView for NoCheckpoints {
    fn highest_checkpoint_in_index(&self) -> Option<u32> {
        None
    }
}

impl SerialCore {
    /// The core over `params`' rules.
    ///
    /// # Errors
    ///
    /// Parameters the header or payment rules refuse: a wiring error.
    pub fn new(
        params: &ChainParams,
        caches: Arc<ScriptCaches>,
        queue: Arc<CheckQueue>,
        helper: Box<dyn ChainHelper>,
        abort: AbortPath,
        assume_valid: Option<BlockHash>,
    ) -> Result<SerialCore, Fatal> {
        Ok(SerialCore {
            hp: HeaderParams::from_chain_params(params)
                .map_err(|e| Fatal::new(format!("header parameters: {e:?}")))?,
            payments: PaymentsParams::from_chain_params(params)
                .map_err(|e| Fatal::new(format!("payment parameters: {e:?}")))?,
            caches,
            queue,
            helper,
            abort,
            assume_valid,
            scratch: BlockScratch::default(),
            refused: false,
            script_checks: None,
            xcheck: false,
            #[cfg(feature = "planted-bugs")]
            planted: None,
            #[cfg(feature = "planted-bugs")]
            t12_pending: None,
        })
    }

    /// `-consensusxcheck` (A1 §3.10; CH3 §6.2 L1): every read of a transaction's [`TxCoins`] is
    /// also a direct read of the overlay, and a difference is fatal (CH3a D189).
    pub fn set_xcheck(&mut self, on: bool) {
        self.xcheck = on;
    }

    /// Core's `AbortNode(state, msg)` inside `ConnectBlock`: reported here, once, then `state.Error`
    /// (CH3 §2.1.1).
    fn abort_node(&self, msg: &str, user: Option<&str>) -> ConnectOutcome {
        self.abort.abort_node(msg, user);
        ConnectOutcome::SystemError(msg.to_owned())
    }

    /// A readiness refusal (CH3 §2.17, CH3a D69): a system error, reported on the tip path. In
    /// a proposal (`just_check`) it is the RPC's error and nothing reports it: a proposal never
    /// stops the node (CH3a D121).
    fn refuse(&mut self, text: String, just_check: bool) -> ConnectOutcome {
        if just_check {
            self.refused = true;
        } else {
            self.abort.abort_node(&text, Some(&text));
        }
        ConnectOutcome::SystemError(text)
    }

    /// Whether the proposal's `ConnectBlock` that just returned ended in an unreported readiness
    /// refusal, rather than a reported `AbortNode` (CH3a D121); clears it.
    pub(super) fn take_refused(&mut self) -> bool {
        std::mem::take(&mut self.refused)
    }

    // Core: validation.cpp:2542-2571
    /// `fScriptChecks` for the non-genesis block `id` at `height` (CH3 §2.8).
    fn script_checks(&self, cx: &ConnectCx<'_>, id: BlockId, height: u32) -> Result<bool, Fatal> {
        let Some(av) = self.assume_valid.and_then(|h| cx.reader.lookup(&h)) else {
            return Ok(true);
        };
        let best = entry(cx.reader, cx.best_header)?;
        let mut facts = AssumeValidFacts {
            assumed_valid_indexed: true,
            on_assumed_valid_chain: cx.reader.ancestor(av, height) == Some(id),
            on_best_header_chain: cx.reader.ancestor(cx.best_header, height) == Some(id),
            best_header_has_min_work: best.chain_work >= cx.min_chain_work,
            equivalent_time: 0,
        };
        if facts.on_assumed_valid_chain
            && facts.on_best_header_chain
            && facts.best_header_has_min_work
        {
            let chain = ReaderAncestry(cx.reader);
            let (tip, block) = (chain.header(cx.best_header), chain.header(id));
            facts.equivalent_time = block_proof_equivalent_time(&tip, &block, &tip, self.hp.pow());
        }
        Ok(script_checks(&facts))
    }

    /// The attempt is over: the helper's removed view ends (D35, CH3a D76).
    pub(super) fn end_attempt(&mut self) {
        self.helper.end_attempt();
    }

    /// The last block's streamed `CBlockUndo`: for a block the last connect connected, the one
    /// that reached `WriteUndoDataForBlock`'s call site (`v:2888`), where O8's patch hashes it.
    pub(super) fn last_undo(&self) -> &[u8] {
        &self.scratch.undo
    }

    /// The last `ConnectBlock`'s `fScriptChecks`, if it got as far as deciding it.
    pub(super) fn last_script_checks(&self) -> Option<bool> {
        self.script_checks
    }

    /// The undo record the last disconnect read, followed by what the read buffered after it.
    pub(super) fn read_undo(&self) -> &[u8] {
        &self.scratch.undo_read
    }

    // Core: validation.cpp:1884-1889 GetBlockSubsidy
    /// `GetBlockSubsidy(pindex)` for `block` at `at`.
    pub(super) fn subsidy_of(
        &self,
        cx: &ConnectCx<'_>,
        block: &Block,
        at: &BlockAt,
    ) -> Result<Amount, Fatal> {
        if at.prev.is_none() {
            return Ok(genesis_subsidy(block));
        }
        block_subsidy_after(cx.reader, cx.deployments, at.prev, self.payments.subsidy())
            .map_err(|e| Fatal::new(format!("GetBlockSubsidy at {}: {e:?}", at.height)))
    }

    /// The last block's script work: how many transactions built their signature-hash data, and
    /// how many inputs had their spent script copied.
    #[cfg(test)]
    pub(super) fn last_script_work(&self) -> (usize, usize) {
        let scratch = &self.scratch;
        (scratch.sig_data, scratch.spent.copied.get() + scratch.inline.copied.get())
    }

    // Core: validation.cpp:2469-2935 Chainstate::ConnectBlock (fJustCheck = false)
    /// `ConnectBlock(block, state, pindex, view)` for the indexed block `id` over `overlay`, the
    /// attempt's view of its parent's coins: its steps in Core's order (CH3 §2.2), its
    /// `WriteUndoDataForBlock` into `files` and its status writes through `st`. The overlay is
    /// left for the bracket to commit on `Connected` (D33).
    ///
    /// # Errors
    ///
    /// A storage fault or a broken invariant (CH3 §2.1): no verdict.
    #[expect(
        clippy::too_many_arguments,
        reason = "CH3 §2.1's seam arguments, the attempt's steps and its view"
    )]
    pub fn connect<E: KvEngine, F: FlatFs>(
        &mut self,
        steps: &mut dyn AttemptSteps,
        cx: &ConnectCx<'_>,
        overlay: &mut BlockOverlay<'_, E>,
        files: &GuardedFiles<F>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        let e = entry(cx.reader, id)?;
        let at = BlockAt {
            hash: e.hash,
            height: e.height,
            prev: e.prev,
            id: Some(id),
        };
        let finish = Finish::Index {
            status: e.status,
            file: e.file,
            st,
        };
        self.connect_at(steps, cx, overlay, files, block, checked, &at, finish)
    }

    // Core: validation.cpp:2469-2935 Chainstate::ConnectBlock
    /// `ConnectBlock(block, state, pindex, view, fJustCheck)` for the block `at` over `overlay`:
    /// [`SerialCore::connect`]'s steps, or `TestBlockValidity`'s dry run on its dummy
    /// ([`Finish::JustCheck`]: `at.id` is `None`). Every per-block question goes through
    /// `at.prev` (CH3 §2.14).
    ///
    /// # Errors
    ///
    /// A storage fault or a broken invariant (CH3 §2.1): no verdict.
    #[expect(
        clippy::too_many_arguments,
        reason = "CH3 §2.1's seam arguments, the attempt's steps, its view and the finish"
    )]
    pub(super) fn connect_at<E: KvEngine, F: FlatFs>(
        &mut self,
        steps: &mut dyn AttemptSteps,
        cx: &ConnectCx<'_>,
        overlay: &mut BlockOverlay<'_, E>,
        files: &GuardedFiles<F>,
        block: &Block,
        checked: bool,
        at: &BlockAt,
        finish: Finish<'_, '_>,
    ) -> Result<ConnectOutcome, Fatal> {
        let just_check = matches!(finish, Finish::JustCheck);
        debug_assert!(!self.refused, "a proposal's refusal mark was never taken (CH3a D121)");
        self.script_checks = None;
        // CH3a D125: the helper's attempt scope opens with `ConnectBlock`, and `end_attempt`
        // closes it.
        self.helper.begin_attempt(cx, at)?;
        let h = at.height;
        // v:2478-2479
        if block.hash() != at.hash {
            return Err(Fatal::new(format!(
                "ConnectBlock: block {} is not block {} at height {h}",
                block.hash(),
                at.hash
            )));
        }
        // v:2490-2491: the scheme in force while the block is processed (H1).
        let bls = steps.bls_ctx(cx.params, at);

        // v:2505-2514, with PoW and merkle off when `fJustCheck`; skipped for the object whose
        // `CheckBlock` passed with both on (`fChecked`, D42; CH3a D118).
        if !checked
            && let Err(err) =
                dash_consensus::block::check_block(block, &self.hp, !just_check, !just_check)
        {
            if err.result() == BlockValidationResult::Mutated {
                return Ok(self.abort_node(CORRUPT_BLOCK, None));
            }
            tracing::error!("ConnectBlock: Consensus::CheckBlock: {err}");
            return Ok(ConnectOutcome::Invalid(err));
        }

        // v:2516-2519
        if at.prev.is_some() && self.helper.cl_conflict(cx, h, &at.hash)? {
            tracing::info!("ERROR: ConnectBlock: conflicting with chainlock");
            return Ok(invalid(BlockValidationResult::ChainLock, "bad-chainlock"));
        }

        // v:2522-2523: the view is at the parent.
        let prev = at.prev.map(|p| entry(cx.reader, p)).transpose()?;
        let prev_hash = prev.as_ref().map(|p| p.hash);
        if overlay.best_block() != prev_hash {
            return Err(Fatal::new(format!(
                "ConnectBlock: the view's best block {:?} is not the parent {prev_hash:?}",
                overlay.best_block()
            )));
        }
        let txs = block.txs();
        let Some(coinbase) = txs.first() else {
            return Err(Fatal::new("ConnectBlock: a checked block has no coinbase"));
        };

        // Readiness guard G1 (CH3 §2.17, D31, D54): before any evo state is read.
        if !steps.complete()
            && let Some(rule) = g1_rule(cx.deployments, at.prev, Some(coinbase))
        {
            return Ok(self.refuse(refusal_text(cx.params, at, &g1_capability(rule)), just_check));
        }

        // v:2525-2531: Core's own `AbortNode`, a proposal's too (CH3a D121).
        if let Some(prev_hash) = &prev_hash
            && cx.deployments.active_after(at.prev, Deployment::Buried(BuriedDeployment::Dip0003))
            && !steps.verify_best_block(prev_hash)?
        {
            return Ok(self.abort_node(EVODB_INCONSISTENCY, None));
        }

        // v:2536-2540: genesis connects no transaction (its coinbase is unspendable).
        if at.hash.as_bytes() == cx.params.consensus.hash_genesis_block.as_bytes() {
            return Ok(ConnectOutcome::Connected);
        }
        let Some(prev) = prev else {
            return Err(Fatal::new("ConnectBlock: a block other than genesis has no parent"));
        };

        // v:2542-2571: the dummy is no ancestor of the assumevalid block or the best header
        // (`v:2551-2552` compare pointers), so its scripts are checked (CH3 §2.14).
        let script_checks = match at.id {
            Some(id) => self.script_checks(cx, id, h)?,
            None => true,
        };
        self.script_checks = Some(script_checks);

        // v:2590-2612 (CH3 §2.5): no exempt block, and no limit above the BIP34 block. Dash's
        // `IsBIP30Repeat` is false for every index, the dummy's too (v:6785-6789, CH3a D120).
        let enforce_bip30 = !at.id.is_some_and(is_bip30_repeat)
            && bip30_probed(cx.params, &ReaderAncestry(cx.reader), prev.id);
        if enforce_bip30 {
            for tx in txs {
                let txid = tx.txid();
                for o in 0..tx.outputs().len() {
                    let op = OutPoint {
                        hash: txid,
                        index: o as u32,
                    };
                    if overlay.have_coin(&op).map_err(read_failed)? {
                        tracing::info!("ERROR: ConnectBlock(): tried to overwrite transaction");
                        return Ok(invalid(BlockValidationResult::Consensus, "bad-txns-BIP30"));
                    }
                }
            }
        }

        if wrong_superblock_start(cx.params, at) {
            tracing::info!("ERROR: ConnectBlock(): invalid superblock start");
            return Ok(invalid(BlockValidationResult::Consensus, "bad-sb-start"));
        }

        // v:2626-2632 (CH3 §2.6)
        let (lock_flags, flags) = flags_after(cx.deployments, at.prev);

        // v:2641-2659. Core's control (`v:2649`) is over the queue only with script checks and
        // threads; otherwise it is inline (D41), and each transaction's checks run as it is checked.
        let parallel = script_checks && self.queue.has_threads();
        // v:2750-2752 `fCacheResults = fJustCheck`, passed as both `cacheSigStore` and
        // `cacheFullScriptStore`: the block's one store policy. A connect consults the caches and
        // stores nothing; a proposal stores both (CH3 §2.9's store-both, D10).
        let opts = ScriptOptions {
            flags,
            policy: ScriptPolicy::V24_0,
            caches: Some(&self.caches),
            cache_sig_store: just_check,
            cache_full_script_store: just_check,
        };
        let xcheck = self.xcheck;
        #[cfg(feature = "planted-bugs")]
        let planted_l1 = self.planted == Some(crate::Planted::L1);
        let scratch = &mut self.scratch;
        scratch.undo.clear();
        write_compact_size((txs.len() - 1) as u64, &mut scratch.undo);
        #[cfg(test)]
        {
            scratch.sig_data = 0;
            scratch.spent.copied.set(0);
            scratch.inline.copied.set(0);
        }
        let dip0001 =
            cx.deployments.active_after(at.prev, Deployment::Buried(BuriedDeployment::Dip0001));
        let chain = ReaderAncestry(cx.reader);
        let subsidy =
            block_subsidy_after(cx.reader, cx.deployments, at.prev, self.payments.subsidy())
                .map_err(|e| Fatal::new(format!("GetBlockSubsidy at {h}: {e:?}")))?;
        // v:6797-6803 GetSpecialTxRules(pindex->pprev)
        let rules = SpecialTxRules {
            v24: cx.deployments.active_after(at.prev, Deployment::Versionbits(DeploymentPos::V24)),
            evo_shares: cx
                .deployments
                .active_after(at.prev, Deployment::Versionbits(DeploymentPos::EvoShares)),
        };
        let v24 = cx.deployments.active_after(at.prev, Deployment::Versionbits(DeploymentPos::V24));
        let ctx = ConsensusCtx {
            params: cx.params,
            height: h,
            prev: Some(prev.id),
            chain: &chain,
            deployments: cx.deployments,
            checkpoints: &NoCheckpoints,
            bls,
            median_time_past: prev.mtp,
            header_check_time: 0,
        };
        let (step, all_ok) = {
            // v:2643-2648 `txsdata`: sized once, before the control, so that the queued checks
            // borrow their transaction's slot until `Wait`. Inline checks end before their slot
            // does, so they need no per-block slots (CH3a D128).
            let slots: Option<Vec<TxSlot<'_>>> =
                parallel.then(|| txs.iter().map(|tx| TxSlot::new(tx)).collect());
            let (spent, inline) = (&scratch.spent, &mut scratch.inline);
            let (undo, prev_heights) = (&mut scratch.undo, &mut scratch.prev_heights);
            let arena = &mut scratch.coins;
            #[cfg(test)]
            let sig_data = &mut scratch.sig_data;
            let (step, all_ok) = with_control(
                &self.queue,
                parallel,
                #[inline(always)]
                |control| {
                    // Copies: captured by reference, each per-transaction read would go through the
                    // closure's environment (CH3a D128).
                    let (h, flags, lock_flags, v24, script_checks, just_check, xcheck) =
                        (h, flags, lock_flags, v24, script_checks, just_check, xcheck);
                    let max_sigops = max_block_sigops(dip0001);
                    // v:2661-2670: before the UTXO loop, over the parent's coins (CH2 §2.4.2).
                    match steps.process_special_txs(
                        block,
                        at,
                        rules,
                        &*overlay,
                        subsidy,
                        just_check,
                        script_checks,
                    )? {
                        Ok(()) => {}
                        Err(SpecialTxFailure::Invalid(err)) => {
                            tracing::error!(
                                "ConnectBlock(DASH): ProcessSpecialTxsInBlock for block {} failed with {err}",
                                at.hash
                            );
                            return Ok(Step::Stop(ConnectOutcome::Invalid(err)));
                        }
                        // CH7 D2: Core reached no verdict, so there is no "failed" line.
                        Err(SpecialTxFailure::NotReady(capability)) => {
                            return Ok(Step::Refuse(capability));
                        }
                    }

                    // v:2687-2768 (CH3 §2.3)
                    let mut fees: Amount = 0;
                    let mut sigops: u32 = 0;
                    for (i, tx) in txs.iter().enumerate() {
                        let txid = tx.txid();
                        let coinbase = tx.is_coinbase();
                        let coins = if coinbase {
                            TxCoins::none(overlay)
                        } else {
                            // HaveInputs' caching in the parent (`coins.cpp:303-313`), which
                            // `CheckTxInputs` skips for an asset unlock
                            // (`consensus/tx_verify.cpp:165-167`); its answer is
                            // `check_tx_inputs`' to give.
                            if !tx.is_platform_transfer() {
                                overlay.have_inputs(tx).map_err(read_failed)?;
                            }
                            // L1 (CH3 §6.2): each input resolved once, for every read up to
                            // `UpdateCoins`. An asset unlock that got here has no input: the
                            // special transaction checks rejected any
                            // (`bad-assetunlocktx-have-input`).
                            arena.reset();
                            let coins = TxCoins::resolve(overlay, tx, arena, xcheck)?;
                            #[cfg(feature = "planted-bugs")]
                            let coins = coins.planted(planted_l1);
                            coins
                        };
                        if !coinbase {
                            let fee = match check_tx_inputs(tx, &coins, h) {
                                Ok(fee) => fee,
                                Err(ViewOr::View(e)) => return Err(e),
                                Err(ViewOr::Reject(err)) => {
                                    tracing::error!(
                                        "ConnectBlock: Consensus::CheckTxInputs: {txid}, {err}"
                                    );
                                    return Ok(Step::Stop(ConnectOutcome::Invalid(
                                        err.into_block(BlockValidationResult::Consensus),
                                    )));
                                }
                            };
                            // v:2708-2716
                            if v24
                                && let Err(err) = steps.shared_collateral_spends(tx, &*overlay)?
                            {
                                tracing::info!(
                                    "ERROR: ConnectBlock: CheckSharedCollateralSpends: {txid}, {err}"
                                );
                                return Ok(Step::Stop(ConnectOutcome::Invalid(
                                    err.into_block(BlockValidationResult::Consensus),
                                )));
                            }
                            // Each fee is in range, so the sum of two cannot overflow.
                            fees += fee;
                            if !money_range(fees) {
                                tracing::info!(
                                    "ERROR: ConnectBlock: accumulated fee in the block out of range."
                                );
                                return Ok(Step::Stop(invalid(
                                    BlockValidationResult::Consensus,
                                    "bad-txns-accumulated-fee-outofrange",
                                )));
                            }
                            // v:2726-2734
                            prev_heights.clear();
                            for input in tx.inputs() {
                                prev_heights.push(coins.access(&input.prevout)?.height());
                            }
                            if !sequence_locks(tx, lock_flags, prev_heights, &ctx) {
                                tracing::info!(
                                    "ERROR: ConnectBlock: contains a non-BIP68-final transaction"
                                );
                                return Ok(Step::Stop(invalid(
                                    BlockValidationResult::Consensus,
                                    "bad-txns-nonfinal",
                                )));
                            }
                        }

                        // v:2740-2744: every transaction, the coinbase too; Core's unsigned sum wraps.
                        let n = match transaction_sigop_count(tx, &coins, flags) {
                            Ok(n) => n,
                            Err(ViewOr::View(e)) => return Err(e),
                            Err(ViewOr::Reject(_)) => {
                                return Err(Fatal::new(
                                    "GetP2SHSigOpCount: a checked input is spent",
                                ));
                            }
                        };
                        sigops = sigops.wrapping_add(n);
                        if sigops > max_sigops {
                            tracing::info!("ERROR: ConnectBlock(): too many sigops");
                            return Ok(Step::Stop(invalid(
                                BlockValidationResult::Consensus,
                                "bad-blk-sigops",
                            )));
                        }

                        // v:2746-2761 (CH3 §2.9): below -assumevalid neither probe nor copy. Inline,
                        // a failure is the transaction's, reported here; queued, it is the queue's.
                        if !coinbase && script_checks {
                            let checked = if let Some(slots) = &slots {
                                input_scripts(&slots[i], &coins, spent, &opts, Some(control))
                            } else {
                                let slot = TxSlot::new(tx);
                                let checked = input_scripts(&slot, &coins, inline, &opts, None);
                                #[cfg(test)]
                                {
                                    *sig_data += usize::from(slot.is_initialised());
                                }
                                // Its checks have run: the copies go, as Core's do (`v:2194`).
                                inline.reset();
                                checked
                            };
                            match checked? {
                                Ok(()) => {}
                                Err(CheckInputsError::Script(f)) => {
                                    let err = input_scripts_failed(f);
                                    tracing::error!(
                                        "ConnectBlock(): CheckInputScripts on {txid} failed with {err}"
                                    );
                                    return Ok(Step::Stop(ConnectOutcome::Invalid(err)));
                                }
                                // Core's IsValidFlagCombination assert.
                                Err(CheckInputsError::Flags(f)) => {
                                    return Err(Fatal::new(format!("CheckInputScripts: {f}")));
                                }
                            }
                        }

                        // v:2763-2767 UpdateCoins (v:2091-2104), the undo streamed (CH3 §2.10.1).
                        if !coinbase {
                            write_compact_size(tx.inputs().len() as u64, undo);
                            for input in tx.inputs() {
                                let k = OutPointKey::new(&input.prevout);
                                let coin = overlay.spend(&k).map_err(coins_failed)?;
                                let coin = coin.ok_or_else(|| {
                                    Fatal::new("SpendCoin: a checked input is spent")
                                })?;
                                coin.as_ref().write_undo(undo);
                            }
                        }
                        // coins.cpp:118-128 AddCoins(view, tx, height), check_for_overwrite = false
                        for (o, out) in tx.outputs().iter().enumerate() {
                            let k = OutPointKey::new(&OutPoint {
                                hash: txid,
                                index: o as u32,
                            });
                            let src = CoinSrc::Output {
                                out,
                                height: h,
                                coinbase,
                            };
                            overlay.add(&k, src, coinbase).map_err(coins_failed)?;
                        }
                    }
                    Ok(Step::Done(fees))
                },
            );
            #[cfg(test)]
            {
                *sig_data += slots.iter().flatten().filter(|s| s.is_initialised()).count();
            }
            (step, all_ok)
        };
        // The scope has ended, and with it every borrow of the queued checks' copies; inline
        // checks copied nothing into it.
        if parallel {
            self.scratch.spent.reset();
        }
        // An early verdict or a fault: the control waited, and its verdict is ignored.
        let fees = match step? {
            Step::Done(fees) => fees,
            Step::Stop(out) => return Ok(out),
            Step::Refuse(capability) => {
                return Ok(self.refuse(refusal_text(cx.params, at, capability), just_check));
            }
        };
        // v:2778-2781: with threads, a failed script is reported here, before step 18 (CH3 §2.4).
        if !all_ok {
            tracing::info!("ERROR: ConnectBlock: CheckQueue failed");
            return Ok(ConnectOutcome::Invalid(check_queue_failed()));
        }

        // v:2801-2819: DASH: CHECK TRANSACTIONS FOR INSTANTSEND (CH3 D20, D35)
        if !self.helper.ibd(cx)? {
            let has_chainlock = self.helper.cl_has(cx, h, &at.hash)?;
            for tx in txs {
                if !has_lock_inputs(tx) {
                    continue;
                }
                while let Some(lock) = self.helper.is_conflict(tx)? {
                    if has_chainlock {
                        tracing::debug!(
                            "ConnectBlock(DASH): chain-locked transaction {} overrides islock {}",
                            tx.txid(),
                            lock.hash
                        );
                        self.helper.remove_conflicting_is_lock(&lock, steps.sink());
                    } else {
                        tracing::info!(
                            "ERROR: ConnectBlock(DASH): transaction {} conflicts with transaction \
                             lock {}",
                            tx.txid(),
                            lock.txid
                        );
                        return Ok(invalid(BlockValidationResult::ChainLock, "conflict-tx-lock"));
                    }
                }
            }
        }

        // Readiness guard G2 (CH3 §2.17, D31): before the helper's superblock answer.
        if let Some(missing) = self.helper.interim_refusal(cx, h) {
            return Ok(self.refuse(g2_refusal_text(cx.params, at, missing), just_check));
        }
        // v:2841-2850
        let check = SuperblockCheck::for_block(self.helper.sb_required(cx, at)?, rules.v24);
        let mut gov = HelperGov(&mut *self.helper);
        match block_value_valid(coinbase, h, subsidy + fees, check, &self.payments, &mut gov) {
            Ok(BlockValue::Valid) => {}
            Ok(BlockValue::Invalid(why)) => {
                // NOTE: Do not punish, the node might be missing governance data
                tracing::info!("ERROR: ConnectBlock(DASH): {why}");
                return Ok(invalid(BlockValidationResult::Unset, "bad-cb-amount"));
            }
            Ok(BlockValue::Ev3(_)) => {
                let text = refusal_text(cx.params, at, SUPERBLOCK_CAPABILITY);
                return Ok(self.refuse(text, just_check));
            }
            Err(PaymentsError::Subsidy(e)) => {
                return Err(Fatal::new(format!("GetPaymentsLimit at {h}: {e:?}")));
            }
            Err(PaymentsError::Governance(f)) => return Err(f),
        }

        // v:2859-2865
        let era = MnRewardEra::after(
            cx.deployments.active_after(at.prev, Deployment::Buried(BuriedDeployment::V20)),
            cx.deployments.active_after(at.prev, Deployment::Buried(BuriedDeployment::MnRr)),
        );
        let payee =
            block_payee_valid(h, subsidy, fees, era, rules.v24, check, &self.payments, &mut gov)?;
        let payee_valid = match payee {
            BlockPayee::Valid => true,
            // masternode/payments.cpp:397-401: the masternode half first (H5), then the rest.
            BlockPayee::Ev3(Ev3Branch::MasternodePayee {
                block_subsidy,
                fee_reward,
                era,
                strict_multiplicity,
            }) => {
                if !steps.complete() {
                    let text = refusal_text(cx.params, at, MN_PAYEE_CAPABILITY);
                    return Ok(self.refuse(text, just_check));
                }
                steps.mn_payment_valid(
                    coinbase,
                    at.prev,
                    block_subsidy,
                    fee_reward,
                    era,
                    strict_multiplicity,
                )? && match superblock_payee_valid(
                    h,
                    subsidy,
                    fees,
                    check,
                    &self.payments,
                    &mut gov,
                )? {
                    BlockPayee::Valid => true,
                    BlockPayee::Ev3(_) => {
                        let text = refusal_text(cx.params, at, SUPERBLOCK_CAPABILITY);
                        return Ok(self.refuse(text, just_check));
                    }
                }
            }
            BlockPayee::Ev3(Ev3Branch::ValidSuperblock {
                ..
            }) => {
                let text = refusal_text(cx.params, at, SUPERBLOCK_CAPABILITY);
                return Ok(self.refuse(text, just_check));
            }
        };
        if !payee_valid {
            // NOTE: Do not punish, the node might be missing governance data
            tracing::info!(
                "ERROR: ConnectBlock(DASH): couldn't find masternode or superblock payments"
            );
            return Ok(invalid(BlockValidationResult::Unset, "bad-cb-payee"));
        }

        // v:2883-2884
        let Finish::Index {
            status,
            file,
            st,
        } = finish
        else {
            return Ok(ConnectOutcome::Connected);
        };

        // v:2888-2890 WriteUndoDataForBlock (blockstorage.cpp:773-803): only without undo yet.
        if !status.has(Status::HAVE_UNDO) || file.is_none() {
            let file = file.ok_or_else(|| Fatal::new("ConnectBlock: a block without data"))?;
            let file = u32::try_from(file).map_err(|_| Fatal::new("ConnectBlock: nFile < 0"))?;
            let pos = match self.write_undo(files, file, h, &prev.hash) {
                Ok(pos) => pos,
                Err(e) => return self.undo_failed(&e),
            };
            // T12: `HAVE_UNDO` only once the record is written.
            st.set_undo(file as i32, pos)?;
        }

        // v:2899-2902
        st.raise_validity(Status::VALID_SCRIPTS)?;
        // v:2912-2921: the view's best block is the bracket's commit (D33).
        steps.set_best_block(&at.hash)?;
        steps.commit_bls_scheme();
        steps.list_changed(false);
        Ok(ConnectOutcome::Connected)
    }

    // Core: node/blockstorage.cpp:783 WriteUndoDataForBlock (UndoWriteToDisk)
    /// The block's undo record, streamed into the scratch, written into rev file `file`: its data
    /// position. Planted T12 defers it.
    fn write_undo<F: FlatFs>(
        &mut self,
        files: &GuardedFiles<F>,
        file: u32,
        height: u32,
        prev: &BlockHash,
    ) -> Result<u32, FlatFileError> {
        #[cfg(feature = "planted-bugs")]
        if matches!(self.planted, Some(crate::Planted::T12)) {
            return self.t12_defer(files, file, height, *prev);
        }
        let undo = &self.scratch.undo;
        files.mutate(|f| f.write_undo_slice(undo, file, height, prev.as_bytes())).map(|p| p.pos)
    }

    /// A failed `WriteUndoDataForBlock` (CH3 §2.1.1): Core's `AbortNode` text for the step that
    /// failed, reported once, and Core's log lines only (CH3a D77); nothing when another store's
    /// failure had latched.
    fn undo_failed(&self, e: &FlatFileError) -> Result<ConnectOutcome, Fatal> {
        match e {
            // The latch had tripped: that failure has been reported.
            FlatFileError::Failed => Err(Fatal::reported(format!("undo write: {e}"))),
            // blockstorage.cpp:737-739 FindUndoPos's CheckDiskSpace, then its caller's line (:780).
            // ST2's `NoSpace` from a shared file's private copy (rdashd only) is reported the same.
            FlatFileError::NoSpace => {
                let out = self.abort_node(DISK_SPACE_LOW, Some(DISK_SPACE_LOW));
                tracing::error!("ConnectBlock(): FindUndoPos failed");
                Ok(out)
            }
            // blockstorage.cpp:579-582 FlushUndoFile: its AbortNode alone (D66).
            FlatFileError::Io {
                context: FLUSH_UNDO_FAILED,
                ..
            } => Ok(self.abort_node(FLUSH_UNDO_FAILED, None)),
            // blockstorage.cpp:513-515 then :783-784: the record's file does not open. A failed
            // open in `Allocate` is silent in Core (flatfile.cpp:67-73), and the write's open then
            // fails the same way. A failed write, which ST2 cannot tell from an open, and a corrupt
            // position stop the node the same way: declared differences (D77).
            FlatFileError::Io {
                ..
            }
            | FlatFileError::Corrupt {
                ..
            } => {
                tracing::error!("UndoWriteToDisk: OpenUndoFile failed");
                Ok(self.abort_node(WRITE_UNDO_FAILED, None))
            }
        }
    }
}

#[cfg(feature = "planted-bugs")]
impl SerialCore {
    /// Arms a planted bug on this core: T12 (the CRASH suite's core mode, CH3a a3) or L1 (CH3a
    /// D189).
    pub fn plant(&mut self, bug: Option<crate::Planted>) {
        self.planted = bug;
    }

    /// Planted T12 (ST3a's toy's): the block's undo record is not written now but at the next
    /// connect's undo write, first; the position returned for `HAVE_UNDO` is the one it takes
    /// then, past the file's undo size now. A flush in between writes an index entry naming a
    /// record the files do not hold.
    fn t12_defer<F: FlatFs>(
        &mut self,
        files: &GuardedFiles<F>,
        file: u32,
        height: u32,
        prev: BlockHash,
    ) -> Result<u32, FlatFileError> {
        if let Some((undo, file, height, prev)) = self.t12_pending.take() {
            files.mutate(|f| f.write_undo_slice(&undo, file, height, prev.as_bytes()))?;
        }
        let size = files.read(|f| f.file_info(file).map_or(0, |i| i.undo_size));
        self.t12_pending = Some((self.scratch.undo.clone(), file, height, prev));
        Ok(size + BLOCK_SERIALIZATION_HEADER_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use dash_consensus::ctx::HeaderCtx;
    use dash_consensus::subsidy::{block_subsidy_inner, genesis_reward};
    use dash_num::{Arith256, CompactTarget};
    use dash_params::{ChainOption, ChainParamsBuilder, NetworkKind, Release};
    use dash_types::Numeric as _;

    use super::*;
    use crate::chain::deployments::ChainDeployments;

    /// A stand-in chain for heights no test can mine: `BlockId(h)` at height `h`, every hash
    /// distinct and none zero, except the block at `named.0`, whose hash is `named.1`.
    struct Line {
        named: (u32, BlockHash),
    }

    impl ChainAncestry for Line {
        fn header(&self, id: BlockId) -> HeaderCtx {
            let mut hash = [0xa5; 32];
            hash[..4].copy_from_slice(&id.0.to_le_bytes());
            HeaderCtx {
                height: id.0,
                hash: if id.0 == self.named.0 {
                    self.named.1
                } else {
                    BlockHash::from(hash)
                },
                time: 0,
                bits: CompactTarget::new(0),
                version: 0,
                mtp: 0,
                chain_work: Arith256::ZERO,
            }
        }

        fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
            (height <= id.0).then_some(BlockId(height))
        }

        fn prev(&self, id: BlockId) -> Option<BlockId> {
            id.0.checked_sub(1).map(BlockId)
        }
    }

    fn params(network: NetworkKind) -> ChainParams {
        let b = ChainParamsBuilder::new(network, Release::V24_0);
        let b = if network == NetworkKind::Devnet {
            b.devnet_name("a3")
        } else {
            b
        };
        b.build().expect("params")
    }

    /// Whether the block at `h` is probed, on `line`.
    fn probed(p: &ChainParams, line: &Line, h: u32) -> bool {
        bip30_probed(p, line, BlockId(h - 1))
    }

    /// D59: mainnet probes at 951 and skips from 952, testnet probes at 76 and skips from 77;
    /// with another block at the BIP34 height (the negative control) both always probe.
    #[test]
    fn the_bip34_block_is_probed_and_the_skip_starts_after_it() {
        for (network, height) in [(NetworkKind::Mainnet, 951), (NetworkKind::Testnet, 76)] {
            let p = params(network);
            let (h, hash) = p.consensus.bip34;
            assert_eq!(h.to_u32(), height, "{network:?}");
            let on = Line {
                named: (height, BlockHash::from(*hash.as_bytes())),
            };
            let probes: Vec<bool> =
                [height - 1, height, height + 1, height + 1000].map(|b| probed(&p, &on, b)).into();
            assert_eq!(probes, [true, true, false, false], "{network:?}");
            let off = Line {
                named: (height, BlockHash::from([1; 32])),
            };
            assert!([height, height + 1, height + 1000].iter().all(|&b| probed(&p, &off, b)));
        }
    }

    /// CH3a D119: the flags of the block after `prev` are dash-consensus's `GetBlockScriptFlags`
    /// and `nLockTimeFlags` ports asked at the block (`DeploymentActiveAt`), on every network, at
    /// each buried deployment's height ±1 and at the first blocks.
    #[test]
    fn the_flags_after_the_parent_are_the_flags_at_the_block() {
        use dash_consensus::deployments::{
            Deployments, NoEhfSignals, VersionBitsCache, buried_enabled,
        };

        for network in NetworkKind::ALL {
            let p = params(network);
            let line = Line {
                named: (0, BlockHash::from([3; 32])),
            };
            let cache = VersionBitsCache::new();
            let view = Deployments::new(&p, &line, &NoEhfSignals, &cache);
            let mut heights = vec![0, 1, 2];
            for d in BuriedDeployment::ALL.into_iter().filter(|&d| buried_enabled(&p, d)) {
                let h = p.consensus.deployment_height(d).to_u32();
                heights.extend([h.saturating_sub(1), h, h + 1]);
            }
            for h in heights {
                let at = (
                    dash_consensus::connect::lock_time_flags(&view, BlockId(h)),
                    dash_consensus::connect::block_script_flags(&view, BlockId(h)),
                );
                let prev = h.checked_sub(1).map(BlockId);
                assert_eq!(flags_after(&view, prev), at, "{network:?} at {h}");
            }
        }
    }

    /// D59: devnet (BIP34 at 1, no `BIP34Hash`) and regtest (`uint256()`) always probe; regtest's
    /// rig test `a_repeated_unspent_transaction_is_bip30` rejects a repeat at block 2.
    #[test]
    fn devnet_and_regtest_always_probe() {
        for network in [NetworkKind::Devnet, NetworkKind::Regtest] {
            let p = params(network);
            let (h, hash) = p.consensus.bip34;
            assert_eq!((h.to_u32(), *hash.as_bytes()), (1, [0; 32]), "{network:?}");
            let line = Line {
                named: (1, BlockHash::from([2; 32])),
            };
            assert!([1, 2, 3, 10_000].iter().all(|&b| probed(&p, &line, b)), "{network:?}");
        }
    }

    /// CH3a D129: copies that took several chunks leave one chunk as large as all of them, so a
    /// block of the same size allocates nothing; an arena past the cap is freed whole.
    #[test]
    fn spent_scripts_keep_one_chunk_up_to_the_cap() {
        let fill = |s: &SpentScripts, n: usize| {
            for _ in 0..n {
                s.arena.alloc_slice_fill_copy(4096, 0u8);
            }
        };
        let mut s = SpentScripts::default();
        fill(&s, 64);
        let grown = s.arena.allocated_bytes();
        s.reset();
        let kept = s.arena.allocated_bytes();
        assert!(kept >= grown, "{kept} < {grown}");
        fill(&s, 64);
        assert_eq!(s.arena.allocated_bytes(), kept, "the same block again takes no new chunk");
        s.reset();
        assert_eq!(s.arena.allocated_bytes(), kept);

        s.arena.alloc_slice_fill_copy(SPENT_SCRIPTS_KEPT, 0u8);
        assert!(s.arena.allocated_bytes() > SPENT_SCRIPTS_KEPT);
        s.reset();
        assert_eq!(s.arena.allocated_bytes(), 0, "nothing kept past the cap");
    }

    /// A stand-in chain whose every block carries `bits`: `BlockId(h)` at height `h`.
    #[derive(Clone, Copy)]
    struct Bits(u32);

    impl ChainAncestry for Bits {
        fn header(&self, id: BlockId) -> HeaderCtx {
            HeaderCtx {
                height: id.0,
                hash: BlockHash::from([0xa5; 32]),
                time: 0,
                bits: CompactTarget::new(self.0),
                version: 0,
                mtp: 0,
                chain_work: Arith256::ZERO,
            }
        }

        fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
            (height <= id.0).then_some(BlockId(height))
        }

        fn prev(&self, id: BlockId) -> Option<BlockId> {
            id.0.checked_sub(1).map(BlockId)
        }
    }

    fn subsidy_params(p: &ChainParams) -> SubsidyParams {
        let reward = genesis_reward(&p.genesis).expect("a genesis reward");
        SubsidyParams::from_chain_params(p, reward).expect("subsidy parameters")
    }

    /// The subsidy reads the parent's own bits, which regtest's chains never vary: on mainnet the
    /// block at 1,000,000 (parent 999,999, before V20, at a GPU-era difficulty above 3,400) has a
    /// base of 5, four declines and the 10% treasury cut, where the pow limit's would be 25
    /// (review r1 m-1: 334,559,821 against 1,672,799,096, from `val.cpp:1857-1873`).
    // Core: validation.cpp:1815-1876 GetBlockSubsidyHelper, :1884-1889 GetBlockSubsidy
    #[test]
    fn the_subsidy_reads_the_parents_bits() {
        let p = Arc::new(
            ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0).build().unwrap(),
        );
        let sp = subsidy_params(&p);
        // A mainnet-era target (difficulty about 1.2e8).
        let chain = Bits(0x1923_ae4b);
        let deployments = ChainDeployments::over(p.clone(), chain);
        let prev = Some(BlockId(999_999));
        let got = subsidy_after(&chain, &deployments.view(), prev, &sp).unwrap();
        assert_eq!(got, 334_559_821);
        assert_eq!(got, block_subsidy_inner(chain.0, 999_999, false, &sp).unwrap());
        let pow_limit = Bits(0x1e0f_ffff);
        let at_limit = subsidy_after(&pow_limit, &deployments.view(), prev, &sp).unwrap();
        assert_eq!(at_limit, 1_672_799_096);
        assert_eq!(
            subsidy_after(&chain, &deployments.view(), None, &sp).unwrap(),
            sp.config().genesis_reward
        );
    }

    /// `DeploymentActiveAt(block, V20)` is V20 active after the parent: with V20 lowered to 6, the
    /// block at 5 has the pre-V20 subsidy and the block at 6 V20's (a base of 5, a treasury of
    /// 1/5); the budget starts at 1 so both carry a treasury part.
    // Core: validation.cpp:1884-1889 GetBlockSubsidy
    #[test]
    fn the_subsidy_takes_v20_at_the_block() {
        const V20: u32 = 6;
        let p = Arc::new(
            ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
                .arg(ChainOption::Dip3Params, format!("{V20}:{V20}"))
                .arg(ChainOption::TestActivationHeight, format!("v20@{V20}"))
                .arg(ChainOption::TestActivationHeight, format!("mn_rr@{V20}"))
                .arg(ChainOption::BudgetParams, "240:1:1500")
                .build()
                .expect("regtest"),
        );
        let sp = subsidy_params(&p);
        // Regtest's pow limit, which every regtest block carries.
        let chain = Bits(0x207f_ffff);
        let deployments = ChainDeployments::over(p.clone(), chain);
        let at = |height: u32| {
            let prev = Some(BlockId(height - 1));
            subsidy_after(&chain, &deployments.view(), prev, &sp).unwrap()
        };
        let inner =
            |height: u32, v20| block_subsidy_inner(chain.0, height as i32 - 1, v20, &sp).unwrap();
        assert_ne!(inner(V20, false), inner(V20, true), "V20 changes it");
        assert_eq!(at(V20 - 1), inner(V20 - 1, false));
        assert_eq!(at(V20), inner(V20, true));
        assert_eq!(at(V20 + 1), inner(V20 + 1, true));
    }
}
