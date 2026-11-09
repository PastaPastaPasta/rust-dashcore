//! CH3a's `ConnectBlock` through V (CH3 §2.2, §2.3, §2.17, §3.2, §7.1; slice a3): one minimal
//! block per reject string, each system error's one report (CH3 §2.1.1), and the connected
//! block's undo, status and coins.
//!
//! The connector is [`CoreConnector`] with [`TestEvo`] as its hooks (CH3a D61), inside
//! [`Harness`], which seeds the spent coins into the active coins before block 1 (so no test
//! mines a hundred blocks for coinbase maturity) and can show the core a deployment V does not
//! have or a block that reads back otherwise.
//!
//! [`proposal`] is CH3a a6's `TestBlockValidity` through V (CH3 §2.14) on the same harness, and
//! `digest` (feature `statedigest`) a7-1's StateDigest log and recorded replays on it.

#[cfg(feature = "statedigest")]
mod digest;
mod event_order;
mod proposal;
mod read_ahead;

use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use dash_chain::{FeedSource, FlatFilePos, Handoff, Needs, Status, Work};
use dash_consensus::coins::{Coin, CoinsView, TxUndo, decode_block_undo};
use dash_consensus::ctx::{Deployment, DeploymentPos, DeploymentView, bls_ctx_for_block};
use dash_consensus::payments::MnRewardEra;
use dash_consensus::{
    Amount, Block, BlockId, BlockValidationError, BlockValidationResult, BlsCtx, ChainAncestry, Tx,
    TxValidationError, TxValidationResult,
};
use dash_evo::{Effect, EffectSink};
use dash_num::Hash256;
use dash_params::LlmqType;
use dash_params::{
    BuriedDeployment, ChainOption, ChainParamsBuilder, ChainSetup, Network, Release,
};
use dash_primitives::{BlockHash, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dashd_store::flatfile::{
    BlockFileInfo, BlockFiles, BlockFilesConfig, BlockFilesState, FlatFs, FlatRead, OsFs,
};

use super::connect::Latched;
use super::failure::connect_lines;
use super::support::{
    AbortThenShutdown, CheckedAt, Ev, Mined, Rig, RigConfig, SharedFiles, assert_logged_before,
    cb_script, install_accept_log, levels_on, lines_of, lines_on, mine, regtest,
};
use crate::chain::verify::CORRUPTED_BLOCK_DB;
use crate::chain::{
    AbortCause, AbortPath, AbortSink, AttemptOrigin, BlockConnector, BlockState, BlockStatusWriter,
    ChainLockSig, ChainLockView, ConnectCx, ConnectOutcome, DisconnectResult, EvoTarget, Fatal,
    FlushHost, FlushSite, FlushStateMode, GuardScope, LocalShutdown, ProposalChecks,
    ReadAheadSetup, RecordingAbort, SchemeGuardHolder, ScratchParent, ScratchView, SiteDecision,
    VContext, VExit, ValidationInterface, ViewTarget, check_startup_readiness,
};
use crate::checkqueue::CheckQueue;
use crate::coins::{BlockOverlay, CoinSrc, CoinsBudget, CoinsCache, OutPointKey, Ratio, Salt};
use crate::connect::UNWOUND;
use crate::connect::{
    AttemptId, BlockAt, CORRUPT_BLOCK, ChainHelper, ChainstateCell, ClBest, ConnectorParts,
    CoreConnector, DISK_SPACE_LOW, DigestSink, EVODB_INCONSISTENCY, EvoAttempt, EvoDigest,
    EvoHooks, FLUSH_UNDO_FAILED, INTERIM_REFUSAL, IsLockRef, MN_PAYEE_CAPABILITY, NodeHelper,
    SPECIAL_VERSION_CBTX, SpecialTxFailure, SpecialTxRules, WRITE_UNDO_FAILED, g1_capability,
    g2_refusal_text, is_governance_height, pre_dip3_special_txs, refusal_text,
    wrong_superblock_start,
};
use crate::llmq::rows::RuntimeRows;
use crate::sigcache::{CacheSizes, ScriptCaches};
use crate::store::held::{HeldSink, fail_on_w};
use crate::store::sites::{
    BLOCK_FILES_WRITE_SITE, FLUSH_SITE, READ_SITE, UNWIND_SITE, W_PANIC_SITE,
};
use crate::store::{
    CoinsReadError, Failure, GlobalFailStop, GuardedEngine, GuardedFiles, HookTry, ProbeEngine,
    RecordingSink,
};

mod queue;

const COIN: Amount = 100_000_000;

/// The coinbase value that marks a stored block [`TestEvo`]'s startup check refuses: only a
/// check that reads the block sees it.
const MARKED: Amount = COIN - 1;

/// [`TestEvo`]'s refusal of a [`MARKED`] block.
const MARKED_BLOCK: &str = "a marked block";
const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];
const OP_TRUE: u8 = 0x51;
const APPLY_FAILED: &str = "applying an effect failed";
/// [`Seen::fail_next_undo`]'s `Fatal`.
const UNDO_FATAL: &str = "H9 failed";
/// [`Seen::fail_next_undo_latched`]'s report, made as it trips the latch.
const UNDO_READ_REPORTED: &str = "H9's read failed";
/// [`Seen::evo_read_in_next_undo`]'s and [`Seen::evo_read_in_next_h4`]'s store, as its `Fatal`
/// names it.
const EVO_READ: &str = "llmq snapshot read";
/// `UndoSpecialTxsInBlock`'s catch line before its FAILURE (`specialtxman.cpp:1088` at v24.0.0)
/// on the rig, whose V19 is at height 1: basic.
const UNDO_CAUGHT_SCHEME: &str =
    "CSpecialTxProcessor::UndoSpecialTxsInBlock -- bls_legacy_scheme=0";
/// The start of `UndoSpecialTxsInBlock`'s FAILURE line (`specialtxman.cpp:1089` at v24.0.0).
const UNDO_FAILURE: &str = "CSpecialTxProcessor::UndoSpecialTxsInBlock -- FAILURE!";
/// The rig's line for an uncommitted exit of a tip or walk scheme guard
/// ([`EvoHooks::exit_scheme_guard`]), followed by its `log`.
const GUARD_EXIT: &str = "rig: the scheme guard's exit, log=";
/// The rig's line for an attempt the connector marks as ending in one of Core's asserts or a coins
/// read fault ([`EvoHooks::mark_unlogged`]).
const UNLOGGED: &str = "rig: the attempt's guard logs no revert";
/// The rig's line for the scheme an undo's catch restores ([`EvoHooks::undo_caught`]),
/// followed by `bls_legacy_scheme`'s value.
const UNDO_CAUGHT_HOOK: &str = "rig: the undo's catch restores bls_legacy_scheme=";
/// [`UNDO_CAUGHT_SCHEME`] under legacy.
const UNDO_CAUGHT_LEGACY: &str =
    "CSpecialTxProcessor::UndoSpecialTxsInBlock -- bls_legacy_scheme=1";

/// Regtest with V19 at height 2: the rig's b1 is the last block without it.
fn v19_at_2() -> Arc<ChainSetup> {
    let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::TestActivationHeight, "v19@2")
        .build()
        .expect("v19@2");
    Arc::new(p)
}
const TX_FAILED: &str = "the evo transaction failed";
const OTHER_STORE_FAILED: &str = "another store's write failed";
/// A site [`kept_site`](crate::flush::kept_site) classes as `Store`: another store's write.
const OTHER_STORE_SITE: &str = "coins write";

/// W's engine text at [`UndoFault::WLatched`], and the `AbortNode` text W reports for it.
const W_FAILED: &str = "memory error (Io): injected W write";
const W_REPORTED: &str = "System error while flushing: memory error (Io): injected W write";
const HOOK_PANICKED: &str = "a hook panicked";
/// W's panic text at [`UndoFault::WPanicked`].
const W_PANICKED: &str = "the chainstate flush thread panicked";
/// [`StepsCfg::trip_then_panic_in_h4`]'s first failure, a block file's.
const FILE_FAILED: &str = "Failed to write block";
const H8_FAILED: &str = "CheckSharedCollateralSpends failed";
const H11_FAILED: &str = "H11: injected failure";
const FINISH_FAILED: &str = "finish_attempt: injected failure after H11";

/// The evo side under test: `NoEvo`'s answers (its pre-DIP3 checks) when incomplete, scripted
/// answers when complete.
#[derive(Clone, Copy)]
struct StepsCfg {
    complete: bool,
    verify_best: bool,
    mn_payee: bool,
    /// Applying effect `k` of an attempt with effects fails (`k` = the log's length: the
    /// release), as D4's `AbortNode`.
    fail_apply: Option<usize>,
    /// The bracket step that fails in every attempt after genesis's (CH3 §3.2).
    fail_tx: Option<TxStep>,
    /// H4 panics (mid-attempt).
    panic_in_h4: bool,
    /// H4 returns this readiness refusal (CH7 D2).
    not_ready_in_h4: Option<&'static str>,
    /// H4 asks V24's versionbits `DeploymentActiveAfter` of its block (the dummy's child in a
    /// proposal).
    ask_after_block_in_h4: bool,
    /// `commit_tx` panics (after the coins' commit).
    panic_in_commit: bool,
    /// H4 panics in an attempt on a scratch target (VerifyDB's level-4 connect).
    panic_in_scratch_h4: bool,
    /// H9 panics in an attempt on a scratch target (VerifyDB's level-3 disconnect).
    panic_in_scratch_undo: bool,
    /// H9 panics in an attempt on the active target past genesis's (ABC's `DisconnectTip`).
    panic_in_tip_undo: bool,
    /// H4, past genesis's attempt, trips the latch at [`BLOCK_FILES_WRITE_SITE`] and reports
    /// [`FILE_FAILED`] through the node's sink, then panics.
    trip_then_panic_in_h4: bool,
    /// H9's answer (`UndoSpecialTxsInBlock`).
    undo_special: bool,
    /// H8's `k`-th call (from 1, across the node's attempts) is a `Fatal`.
    fail_h8_at: Option<usize>,
    /// [`fail_h8_at`](Self::fail_h8_at)'s `Fatal` is one of Core's asserts, not an `AbortNode`.
    h8_asserts: bool,
    /// H8's `k`-th call rejects the transaction as `CheckSharedCollateralSpends` does
    /// (`bad-shared-collateral-spend`, `evo/specialtxman.cpp:1826`).
    reject_h8_at: Option<usize>,
    /// W's chainstate write fails while V verifies the evo best block, its report held at
    /// [`Seen::held`]; the verification then fails (BL-554).
    w_fails_at_verify: bool,
    /// Hook H11 fails past genesis (CH7a-5b X3).
    h11_fails: bool,
    /// `finish_attempt` fails in each attempt whose H11 ran past genesis (CH7a-5b X1).
    fail_finish_after_h11: bool,
}

/// The evo transaction's fallible steps.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TxStep {
    Begin,
    Commit,
}

impl Default for StepsCfg {
    fn default() -> Self {
        StepsCfg {
            complete: false,
            verify_best: true,
            mn_payee: true,
            fail_apply: None,
            fail_tx: None,
            panic_in_h4: false,
            not_ready_in_h4: None,
            ask_after_block_in_h4: false,
            panic_in_commit: false,
            panic_in_scratch_h4: false,
            panic_in_scratch_undo: false,
            panic_in_tip_undo: false,
            trip_then_panic_in_h4: false,
            undo_special: true,
            fail_h8_at: None,
            h8_asserts: false,
            reject_h8_at: None,
            w_fails_at_verify: false,
            h11_fails: false,
            fail_finish_after_h11: false,
        }
    }
}

/// What the bracket and the helper were asked, in order, across both (CH3 §3.2, §3.3).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Call {
    BeginAttempt,
    BeginTx,
    SetBestBlock,
    FinishAttempt,
    CommitTx,
    RollbackTx,
    /// Hook H11 (CH7a-5b X1).
    DigestFields,
    Ibd,
    ClHas,
    /// `is_conflict`'s answer.
    IsConflict(Option<Hash256>),
    Remove(Hash256),
    EndAttempt,
}

impl Call {
    /// Asked of the helper (the IS loop), not of the bracket.
    fn is_helper(&self) -> bool {
        matches!(self, Call::Ibd | Call::ClHas | Call::IsConflict(_) | Call::Remove(_))
    }
}

/// The hooks' steps a disconnect's tests follow (CH3 §2.12): H1's two, H9 and H7.
#[derive(Clone, Copy, Debug, PartialEq)]
enum EvoStep {
    BlsCtx,
    CommitBls,
    UndoSpecial,
    /// H7, with its `undo` flag.
    ListChanged(bool),
}

/// A lock in [`Seen::is_store`]: its reference and the locked transaction's inputs.
#[derive(Clone, Debug)]
struct IsLock {
    r: IsLockRef,
    inputs: Vec<OutPoint>,
}

impl IsLock {
    /// A lock on a transaction (`txid` from `n`) spending `inputs`.
    fn on(n: u8, inputs: &[OutPoint]) -> IsLock {
        IsLock {
            r: IsLockRef {
                hash: Hash256::from([n; 32]),
                txid: TxHash::from([n.wrapping_add(100); 32]),
            },
            inputs: inputs.to_vec(),
        }
    }
}

/// Core's `ConflictingISLockIfAny` over `store` less `removed`: per input of `tx` in order, the
/// lock holding that outpoint for another transaction (`evo/chainhelper.cpp:79-85`).
fn is_conflict_in(store: &[IsLock], removed: &[Hash256], tx: &Tx) -> Option<IsLockRef> {
    tx.inputs().iter().find_map(|i| {
        store
            .iter()
            .find(|l| {
                !removed.contains(&l.r.hash)
                    && l.r.txid != tx.txid()
                    && l.inputs.contains(&i.prevout())
            })
            .map(|l| l.r)
    })
}

/// Core's removal closure (`RemoveChainedInstantSendLocks`, `instantsend/instantsend.cpp:339-350`):
/// the lock and every lock on a transaction spending an output of a transaction in the closure.
fn is_closure(store: &[IsLock], hash: Hash256) -> Vec<Hash256> {
    let mut out = vec![hash];
    let mut k = 0;
    while let Some(&h) = out.get(k) {
        let txid = store.iter().find(|l| l.r.hash == h).map(|l| l.r.txid);
        for child in store.iter().filter(|l| l.inputs.iter().any(|i| Some(i.txid()) == txid)) {
            if !out.contains(&child.r.hash) {
                out.push(child.r.hash);
            }
        }
        k += 1;
    }
    out
}

/// Evo hooks for the connector's tests (CH3a D61): `COMPLETE` as `C`, `NoEvo`'s pre-DIP3 checks
/// when incomplete, scripted H2 and H5 answers, an evo best block staged by `set_best_block` and
/// committed by `commit_tx`, an applier that removes each `RemoveConflictingIsLock`'s closure from
/// the scripted InstantSend store, and the failures [`StepsCfg`] injects.
struct TestEvo<const C: bool> {
    cfg: StepsCfg,
    seen: Arc<Mutex<Seen>>,
    /// `set_best_block`'s block, until `commit_tx` or `rollback_tx`.
    staged: Option<BlockHash>,
    /// Attempts begun: genesis's is the first.
    attempts: usize,
    /// Whether the last attempt begun is on a scratch target.
    scratch: bool,
    /// H8's calls so far.
    h8_calls: usize,
    /// The open scheme guard committed.
    guard_committed: bool,
}

impl<const C: bool> TestEvo<C> {
    fn call(&self, c: Call) {
        self.seen.lock().expect("seen").calls.push(c);
    }

    fn step(&self, s: EvoStep) {
        self.seen.lock().expect("seen").evo_steps.push(s);
    }

    /// Whether `step` fails in this attempt (never in genesis's).
    fn fails(&self, step: TxStep) -> bool {
        self.attempts > 1 && self.cfg.fail_tx == Some(step)
    }
}

struct TestAttempt<'v> {
    id: AttemptId,
    log: Vec<Effect>,
    _v: &'v mut VContext,
    /// The attempt's deployments (a proposal's answer for its dummy index).
    deployments: &'v dyn DeploymentView,
    seen: Arc<Mutex<Seen>>,
}

/// Where a facade's gate would be released: records whether the latch had tripped by then,
/// without waiting for its report, as the facade's release does not (BL-710).
impl Drop for TestAttempt<'_> {
    fn drop(&mut self) {
        let mut seen = self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let latched = seen.latch.tripped_now().is_some();
        seen.dropped_latched.push(latched);
    }
}

impl EvoAttempt for TestAttempt<'_> {
    fn id(&self) -> AttemptId {
        self.id
    }

    fn sink(&mut self) -> &mut dyn EffectSink {
        &mut self.log
    }
}

impl<const C: bool> EvoHooks for TestEvo<C> {
    const COMPLETE: bool = C;

    type Facade = ();

    type Attempt<'v> = TestAttempt<'v>;

    type ListUpdates = ();

    fn startup_refusal(
        _: &ChainSetup,
        _: &dyn ChainAncestry,
        _: &dyn DeploymentView,
        _: &BlockAt,
        block: Option<&Block>,
    ) -> Result<Option<&'static str>, String> {
        let coinbase = block.and_then(|b| b.txs().first());
        let marked = coinbase
            .is_some_and(|cb| cb.to_mutable().outputs().iter().any(|o| o.value() == MARKED));
        Ok(marked.then_some(MARKED_BLOCK))
    }

    fn begin_attempt<'v>(
        &mut self,
        _: &'v (),
        v: &'v mut VContext,
        cx: &'v ConnectCx<'_>,
        id: AttemptId,
        _: AttemptOrigin,
        target: &ViewTarget<'_>,
    ) -> Result<TestAttempt<'v>, Fatal> {
        self.attempts += 1;
        self.scratch = matches!(target, ViewTarget::Scratch(_));
        self.call(Call::BeginAttempt);
        self.seen.lock().expect("seen").evo_steps.clear();
        Ok(TestAttempt {
            id,
            log: Vec::new(),
            _v: v,
            deployments: cx.deployments,
            seen: self.seen.clone(),
        })
    }

    fn finish_attempt(&mut self, mut attempt: TestAttempt<'_>) -> Result<(), Fatal> {
        self.call(Call::FinishAttempt);
        if std::mem::take(&mut self.seen.lock().expect("seen").finish_after_h11) {
            return Err(Fatal::new(FINISH_FAILED));
        }
        let armed = {
            let mut seen = self.seen.lock().expect("seen");
            let first = std::mem::take(&mut seen.undo_after_a_report);
            let w_panic = seen.undo_after_an_assert.take().map(|sink| (seen.latch.clone(), sink));
            std::mem::take(&mut seen.fail_next_apply)
                .then(|| (seen.undo_rows.take(), first, w_panic))
        };
        match armed {
            Some((None, ..)) => return Err(Fatal::new(APPLY_FAILED)),
            // An undo's snapshot write, through the node's rows (OD-CP-18).
            Some((Some(rows), first, w_panic)) => {
                let snapshot = dash_evo::llmq::QuorumSnapshot {
                    active_quorum_members: vec![true],
                    skip_list_mode: 0,
                    skip_list: vec![],
                };
                let base = BlockHash::from([1; 32]);
                if first {
                    // Route B's or D's write, which the rows' store reports as it fails.
                    let reported = rows.write(LlmqType::LlmqTestDip0024, &base, &snapshot);
                    reported.expect_err("the store's injected failure");
                }
                if let Some((latch, sink)) = w_panic {
                    // W's panic, which its thread's end reports (BL-586).
                    let panicked = Failure {
                        kind: dashd_store::ErrorKind::Other,
                        site: W_PANIC_SITE,
                        message: W_PANICKED.to_owned(),
                    };
                    if latch.trip(panicked) {
                        sink.abort_node(W_PANICKED, None);
                    }
                }
                let written = rows.write_on_undo(LlmqType::LlmqTestDip0024, &base, &snapshot);
                return Err(written.expect_err("the store's injected failure"));
            }
            None => {}
        }
        // Only an attempt with effects fails (not genesis's at init).
        if attempt.log.is_empty() {
            return Ok(());
        }
        let mut seen = self.seen.lock().expect("seen");
        for (k, e) in std::mem::take(&mut attempt.log).into_iter().enumerate() {
            if self.cfg.fail_apply == Some(k) {
                return Err(Fatal::new(APPLY_FAILED));
            }
            let Effect::RemoveConflictingIsLock {
                lock_hash,
            } = &e
            else {
                panic!("an effect only the IS loop emits: {e:?}");
            };
            let gone = is_closure(&seen.is_store, *lock_hash);
            seen.is_store.retain(|l| !gone.contains(&l.r.hash));
            seen.effects.push(e);
        }
        match self.cfg.fail_apply {
            Some(_) => Err(Fatal::new(APPLY_FAILED)),
            None => Ok(()),
        }
    }

    fn bls_ctx(
        &mut self,
        _: &mut TestAttempt<'_>,
        _: &mut EvoTarget<'_>,
        params: &ChainSetup,
        at: &BlockAt,
    ) -> BlsCtx {
        self.step(EvoStep::BlsCtx);
        bls_ctx_for_block(params, at.height)
    }

    fn commit_bls_scheme(&mut self, _: &mut TestAttempt<'_>, _: &mut EvoTarget<'_>) {
        self.step(EvoStep::CommitBls);
    }

    fn open_scheme_guard(&mut self, _: SchemeGuardHolder) {
        self.guard_committed = false;
    }

    fn commit_scheme_guard(&mut self) {
        self.guard_committed = true;
    }

    /// The rig's own line for each uncommitted exit, so a test reads its place among Core's
    /// lines.
    fn exit_scheme_guard(&mut self, log: bool) {
        if !std::mem::take(&mut self.guard_committed) {
            tracing::info!("{GUARD_EXIT}{log}");
        }
    }

    /// The rig's own line for an attempt whose guard's revert Core never logs.
    fn mark_unlogged(&mut self, _: &mut TestAttempt<'_>) {
        tracing::info!("{UNLOGGED}");
    }

    /// The rig's own line for the scheme the connector says the undo was entered with.
    fn undo_caught(&mut self, entered: BlsCtx) {
        tracing::info!("{UNDO_CAUGHT_HOOK}{}", u8::from(entered == BlsCtx::LEGACY));
    }

    /// Legacy, which regtest's snapshot never gives (V19 is active from height 1).
    fn published_bls(&self) -> Option<BlsCtx> {
        Some(BlsCtx::LEGACY)
    }

    fn verify_best_block(
        &mut self,
        _: &mut TestAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &BlockHash,
    ) -> Result<bool, Fatal> {
        if self.cfg.w_fails_at_verify {
            let mut seen = self.seen.lock().expect("seen");
            let held = seen.held.clone().expect("a held sink");
            seen.w = Some(w_fails(&seen.latch, held));
            return Ok(false);
        }
        Ok(self.cfg.verify_best)
    }

    fn process_special_txs(
        &mut self,
        att: &mut TestAttempt<'_>,
        _: &mut EvoTarget<'_>,
        block: &Block,
        at: &BlockAt,
        _: SpecialTxRules,
        view: &dyn CoinsView<Error = CoinsReadError>,
        _: Amount,
        just_check: bool,
        check_cbtx_merkle_roots: bool,
        _: &mut ClBest<'_>,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
        self.seen.lock().expect("seen").just_checks.push(just_check);
        if self.cfg.ask_after_block_in_h4 {
            let v24 = Deployment::Versionbits(DeploymentPos::V24);
            att.deployments.active_after(at.id, v24);
        }
        if (self.cfg.panic_in_h4 && self.attempts > 1)
            || (self.cfg.panic_in_scratch_h4 && self.scratch)
        {
            panic!("{HOOK_PANICKED}");
        }
        if self.cfg.trip_then_panic_in_h4 && self.attempts > 1 {
            let (latch, sink) = {
                let seen = self.seen.lock().expect("seen");
                (seen.latch.clone(), seen.sink.clone().expect("the node's sink"))
            };
            let first = Failure {
                kind: dashd_store::ErrorKind::Io,
                site: BLOCK_FILES_WRITE_SITE,
                message: FILE_FAILED.to_owned(),
            };
            assert!(latch.trip(first), "the first failure");
            sink.abort_node(FILE_FAILED, None);
            panic!("{HOOK_PANICKED}");
        }
        let read = self.seen.lock().expect("seen").read_in_next_h4.take();
        if let Some(H4Read {
            db,
            coins,
            nested,
            refuse_apply,
        }) = read
        {
            self.seen.lock().expect("seen").fail_next_apply = refuse_apply;
            if nested {
                // `NodeEvo`'s `CMNHFManager::ProcessBlock` scope inside H4's.
                let scope = HookTry::open();
                let read = failing_evo_read(&db);
                if let Some(what) = scope.caught() {
                    tracing::info!("CMNHFManager::ProcessBlock -- failed: {what}");
                    let reason = "failed-proc-mnhf-inblock";
                    let err = BlockValidationError::new(BlockValidationResult::Consensus, reason);
                    return Ok(Err(SpecialTxFailure::Invalid(err)));
                }
                read?;
            } else if coins {
                db.inner().fail_reads(Some(dashd_store::ErrorKind::Io));
                let absent = OutPoint::new(TxHash::from([0x87; 32]), 0);
                view.coin(&absent).map_err(crate::connect::read_failed)?;
            } else {
                failing_evo_read(&db)?;
            }
        }
        if let Some(capability) = self.cfg.not_ready_in_h4 {
            return Ok(Err(SpecialTxFailure::NotReady(capability)));
        }
        if C {
            return Ok(Ok(()));
        }
        Ok(pre_dip3_special_txs(block, check_cbtx_merkle_roots))
    }

    fn mn_payment_valid(
        &mut self,
        _: &mut TestAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &Tx,
        _: Option<BlockId>,
        _: Amount,
        _: Amount,
        _: MnRewardEra,
        _: bool,
    ) -> Result<bool, Fatal> {
        Ok(self.cfg.mn_payee)
    }

    fn set_best_block(
        &mut self,
        _: &mut TestAttempt<'_>,
        _: &mut EvoTarget<'_>,
        hash: &BlockHash,
    ) -> Result<(), Fatal> {
        self.call(Call::SetBestBlock);
        self.staged = Some(*hash);
        Ok(())
    }

    fn list_changed(&mut self, _: &mut TestAttempt<'_>, _: &(), undo: bool) {
        self.step(EvoStep::ListChanged(undo));
    }

    fn shared_collateral_spends(
        &mut self,
        _: &mut TestAttempt<'_>,
        _: &Tx,
        _: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        self.h8_calls += 1;
        if self.cfg.fail_h8_at == Some(self.h8_calls) {
            return Err(if self.cfg.h8_asserts {
                Fatal::assert(H8_FAILED)
            } else {
                Fatal::new(H8_FAILED)
            });
        }
        if self.cfg.reject_h8_at == Some(self.h8_calls) {
            let reason = "bad-shared-collateral-spend";
            return Ok(Err(TxValidationError::new(TxValidationResult::Consensus, reason)));
        }
        Ok(Ok(()))
    }

    fn undo_special_txs(
        &mut self,
        _: &mut TestAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &Block,
        _: &BlockAt,
    ) -> Result<Option<()>, Fatal> {
        self.step(EvoStep::UndoSpecial);
        if (self.cfg.panic_in_scratch_undo && self.scratch)
            || (self.cfg.panic_in_tip_undo && !self.scratch && self.attempts > 1)
        {
            panic!("{HOOK_PANICKED}");
        }
        let evo_read = self.seen.lock().expect("seen").evo_read_in_next_undo.take();
        if let Some(db) = evo_read {
            failing_evo_read(&db)?;
        }
        if std::mem::take(&mut self.seen.lock().expect("seen").fail_next_undo) {
            return Err(Fatal::new(UNDO_FATAL));
        }
        if std::mem::take(&mut self.seen.lock().expect("seen").fail_next_undo_always) {
            return Err(Fatal::abort_node(UNDO_FATAL));
        }
        let panicked = {
            let mut seen = self.seen.lock().expect("seen");
            std::mem::take(&mut seen.fail_next_undo_after_a_panic)
                .then(|| (seen.latch.clone(), seen.sink.clone().expect("the node's sink")))
        };
        if let Some((latch, sink)) = panicked {
            // W's panic, which its thread's end reports (BL-586).
            let panic = Failure {
                kind: dashd_store::ErrorKind::Other,
                site: W_PANIC_SITE,
                message: W_PANICKED.to_owned(),
            };
            assert!(latch.trip(panic), "the first failure");
            sink.abort_node(W_PANICKED, None);
            return Err(Fatal::new(UNDO_FATAL));
        }
        let latched = {
            let mut seen = self.seen.lock().expect("seen");
            std::mem::take(&mut seen.fail_next_undo_latched)
                .then(|| (seen.latch.clone(), seen.sink.clone().expect("the node's sink")))
        };
        if let Some((latch, sink)) = latched {
            let read = Failure {
                kind: dashd_store::ErrorKind::Io,
                site: READ_SITE,
                message: UNDO_READ_REPORTED.to_owned(),
            };
            assert!(latch.trip(read), "the first failure");
            sink.abort_node(UNDO_READ_REPORTED, None);
            return Err(Fatal::new(UNDO_FATAL));
        }
        Ok(self.cfg.undo_special.then_some(()))
    }

    fn begin_tx(&mut self, _: &mut TestAttempt<'_>, _: &mut EvoTarget<'_>) -> Result<(), Fatal> {
        self.call(Call::BeginTx);
        self.staged = None;
        if self.fails(TxStep::Begin) {
            return Err(Fatal::new(TX_FAILED));
        }
        Ok(())
    }

    fn commit_tx(&mut self, target: &mut EvoTarget<'_>) -> Result<(), Fatal> {
        self.call(Call::CommitTx);
        if self.cfg.panic_in_commit && self.attempts > 1 {
            panic!("{HOOK_PANICKED}");
        }
        if self.fails(TxStep::Commit) {
            return Err(Fatal::new(TX_FAILED));
        }
        let staged = self.staged.take();
        // A scratch target's transaction is never committed to the store (`v:5047`).
        if matches!(target, EvoTarget::Active) {
            let mut seen = self.seen.lock().expect("seen");
            seen.evo_best = staged.or(seen.evo_best);
        }
        Ok(())
    }

    fn rollback_tx(&mut self, _: &mut EvoTarget<'_>) {
        self.call(Call::RollbackTx);
        self.staged = None;
    }

    /// Scripted (CH7a-5b): [`h11_fields`] past genesis, or the injected failures.
    fn digest_fields(
        &mut self,
        _: &mut TestAttempt<'_>,
        _: &mut EvoTarget<'_>,
        at: &BlockAt,
    ) -> Result<EvoDigest, Fatal> {
        self.call(Call::DigestFields);
        if at.prev.is_none() {
            return Ok(EvoDigest::default());
        }
        if self.cfg.h11_fails {
            return Err(Fatal::new(H11_FAILED));
        }
        if self.cfg.fail_finish_after_h11 {
            self.seen.lock().expect("seen").finish_after_h11 = true;
        }
        Ok(h11_fields(at.height))
    }
}

/// 32 bytes from `n` up, no two alike, so a byte-order slip shows.
fn spread(n: u8) -> [u8; 32] {
    std::array::from_fn(|i| n.wrapping_add(u8::try_from(i).expect("< 32")))
}

/// The first byte of the scripted `mnl` at `height`: the value moves with the block.
fn h11_mnl_seed(height: u32) -> u8 {
    0x80_u8.wrapping_add(u8::try_from(height).expect("a scripted chain's height"))
}

/// The scripted H11's fields at `height` past genesis (CH7a-5b): every key in its era, `qc` with
/// two rows and `ehf` with one; `mnl` follows the height, and every hash's bytes are distinct.
fn h11_fields(height: u32) -> EvoDigest {
    let h = |n: u8| Hash256::from(spread(n));
    EvoDigest {
        mnl: Some(h(h11_mnl_seed(height))),
        qc: Some(vec![(1, h(0x01), h(0x21)), (100, h(0x41), h(0x61))]),
        cp: Some(crate::connect::CreditPoolDigest {
            locked: 7,
            current_limit: 6,
            lately_unlocked: 2,
            indexes_hash: h(0xa1),
        }),
        ehf: Some(vec![(10, 5)]),
    }
}

/// O8's queries with scripted answers; everything else as `NodeHelper` answers below M5. The IS
/// queries read [`Seen::is_store`] as an IS owner's helper would (D35): `is_conflict` excludes the
/// attempt's removed view, which `remove_conflicting_is_lock` extends by the lock's closure and
/// `end_attempt` drops (D76).
#[derive(Default)]
struct Scripted {
    cl_conflict_at: Option<u32>,
    /// `Some` overrides V's IBD latch.
    ibd: Option<bool>,
    cl_has: bool,
    interim_at: Option<u32>,
    /// The next ChainLock query (`v:2516`) stages W's store failure on W's own thread
    /// ([`w_fails`], its report to [`Seen::held`]) and then fails with [`CL_VIEW_FAILED`], a
    /// `Fatal` nobody reported: past every flush-side check of the connect before it.
    cl_fails_after_w: bool,
    /// The harness's record (set by [`harness`]).
    seen: Option<Arc<Mutex<Seen>>>,
    /// This attempt's removed view.
    removed: Vec<Hash256>,
}

impl Scripted {
    fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
        self.seen.as_ref().expect("set by the harness").lock().expect("seen")
    }

    fn call(&self, c: Call) {
        self.seen().calls.push(c);
    }

    /// Counts one query (CH3 §2.14's digest scope).
    fn asked(&self) {
        self.seen().queries += 1;
    }
}

impl ChainHelper for Scripted {
    fn cl_conflict(&mut self, _: &ConnectCx<'_>, h: u32, _: &BlockHash) -> Result<bool, Fatal> {
        self.asked();
        if std::mem::take(&mut self.cl_fails_after_w) {
            let mut seen = self.seen();
            let held = seen.held.clone().expect("a held sink");
            seen.w = Some(w_fails(&seen.latch, held));
            return Err(Fatal::new(CL_VIEW_FAILED));
        }
        Ok(self.cl_conflict_at == Some(h))
    }

    fn ibd(&mut self, cx: &ConnectCx<'_>) -> Result<bool, Fatal> {
        self.asked();
        self.call(Call::Ibd);
        Ok(self.ibd.unwrap_or(cx.is_ibd))
    }

    fn cl_has(&mut self, _: &ConnectCx<'_>, _: u32, _: &BlockHash) -> Result<bool, Fatal> {
        self.asked();
        self.call(Call::ClHas);
        Ok(self.cl_has)
    }

    fn cl_best(&mut self, _: &ConnectCx<'_>) -> Result<Option<Arc<ChainLockSig>>, Fatal> {
        self.asked();
        Ok(None)
    }

    fn is_conflict(&mut self, tx: &Tx) -> Result<Option<IsLockRef>, Fatal> {
        self.asked();
        let lock = is_conflict_in(&self.seen().is_store, &self.removed, tx);
        self.call(Call::IsConflict(lock.map(|l| l.hash)));
        Ok(lock)
    }

    fn remove_conflicting_is_lock(&mut self, lock: &IsLockRef, sink: &mut dyn EffectSink) {
        self.asked();
        self.call(Call::Remove(lock.hash));
        let closure = is_closure(&self.seen().is_store, lock.hash);
        self.removed.extend(closure);
        sink.emit(Effect::RemoveConflictingIsLock {
            lock_hash: lock.hash,
        });
    }

    fn end_attempt(&mut self) {
        self.call(Call::EndAttempt);
        self.removed.clear();
    }

    fn sb_required(&mut self, _: &ConnectCx<'_>, _: &BlockAt) -> Result<bool, Fatal> {
        self.asked();
        Ok(false)
    }

    fn gov_valid(&mut self) -> Result<bool, Fatal> {
        self.asked();
        Ok(false)
    }

    fn sb_triggered(&mut self, _: u32) -> Result<bool, Fatal> {
        self.asked();
        Ok(false)
    }

    fn sb_payments(&mut self, _: u32) -> Result<Option<Vec<TxOut>>, Fatal> {
        self.asked();
        Ok(None)
    }

    fn interim_refusal(&mut self, _: &ConnectCx<'_>, h: u32) -> Option<&'static str> {
        self.asked();
        (self.interim_at == Some(h)).then_some(INTERIM_REFUSAL)
    }
}

/// V's deployments plus `extra`, active from height 1: G1's and H2's inputs without the header
/// rules V checks at accept.
struct Plus<'a> {
    base: &'a dyn DeploymentView,
    extra: Deployment,
    reader: &'a dash_chain::ChainReader,
}

impl DeploymentView for Plus<'_> {
    fn active_after(&self, prev: Option<BlockId>, d: Deployment) -> bool {
        self.base.active_after(prev, d) || (d == self.extra && prev.is_some())
    }

    fn active_at(&self, id: BlockId, d: Deployment) -> bool {
        self.base.active_at(id, d)
            || (d == self.extra && self.reader.entry(id).is_some_and(|e| e.height > 0))
    }

    /// The base's: `extra` changes no `dep` row a digest of these tests reads.
    fn versionbits(
        &self,
        prev: Option<BlockId>,
        pos: DeploymentPos,
    ) -> Option<dash_consensus::ctx::VersionbitsAnswer> {
        self.base.versionbits(prev, pos)
    }
}

/// V's ChainLock view plus a lock that conflicts with every block at `height` (CH3 §2.14): the
/// one view a proposal's pre-check and the tip path's `NodeHelper` both read.
struct ConflictAt<'a> {
    base: &'a dyn ChainLockView,
    height: u32,
}

impl ChainLockView for ConflictAt<'_> {
    fn has_chainlock(&self, height: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        self.base.has_chainlock(height, hash)
    }

    fn has_conflicting_chainlock(&self, height: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        Ok(height == self.height || self.base.has_conflicting_chainlock(height, hash)?)
    }

    fn best_height(&self) -> i32 {
        self.base.best_height().max(self.height as i32)
    }

    /// The base's: the conflicting lock carries no signature for step 10 to match.
    fn best_chainlock(&self) -> Option<Arc<ChainLockSig>> {
        self.base.best_chainlock()
    }
}

/// `f` over `cx` as a test changes it: `extra` active from height 1, and a ChainLock at
/// `locked_at` that every block there conflicts with.
fn changed_cx<R>(
    extra: Option<Deployment>,
    locked_at: Option<u32>,
    cx: &ConnectCx<'_>,
    f: impl FnOnce(&ConnectCx<'_>) -> R,
) -> R {
    let plus = extra.map(|extra| Plus {
        base: cx.deployments,
        extra,
        reader: cx.reader,
    });
    let locks = locked_at.map(|height| ConflictAt {
        base: cx.chainlocks,
        height,
    });
    let cx = ConnectCx {
        deployments: plus.as_ref().map_or(cx.deployments, |p| p as &dyn DeploymentView),
        chainlocks: locks.as_ref().map_or(cx.chainlocks, |l| l as &dyn ChainLockView),
        ..*cx
    };
    f(&cx)
}

/// What a test changes in the harness.
#[derive(Default)]
struct Setup {
    params: Option<Arc<ChainSetup>>,
    steps: StepsCfg,
    helper: Scripted,
    /// Coins in the active view before block 1: (outpoint, output, height, coinbase).
    seeds: Vec<(OutPoint, TxOut, u32, bool)>,
    /// A deployment the core sees active from height 1 (not V).
    extra: Option<Deployment>,
    /// [`Setup::extra`] for disconnects only: a block connected without it is disconnected
    /// with it.
    disconnect_extra: Option<Deployment>,
    /// What VerifyDB's scratch connects (level 4) meet that the tip's did not.
    scratch_connect: Option<ScratchConnect>,
    /// A ChainLock at this height the connector's view holds (not V's chain): every block at
    /// the height conflicts with it.
    locked_at: Option<u32>,
    /// The block the core reads for this hash instead, with `checked` false: a stored block
    /// that reads back corrupt.
    substitute: Option<(BlockHash, Arc<Block>)>,
    /// A stopped node's stores, which a restart reopens.
    stores: Option<Stores>,
    /// The scripted InstantSend store's locks.
    is_store: Vec<IsLock>,
    /// The undo files' state when the node opens them.
    undo_state: BlockFilesState,
    /// `-assumevalid`.
    assume_valid: Option<BlockHash>,
    /// `-minimumchainwork`.
    min_chain_work: Option<Work>,
    /// The production helper instead of [`Scripted`].
    node_helper: bool,
    /// The readiness guard's scope; default the network's.
    guard: Option<GuardScope>,
    /// The rig's clock, in seconds after genesis; default a day (IBD).
    after_genesis: Option<i64>,
    /// CS6's queue: `-par`'s additional script-check threads (`script_check_threads`); 0 is
    /// `-par=1`, `CheckQueue::serial()`.
    script_threads: usize,
    /// The connector's script caches; default new ones.
    caches: Option<Arc<ScriptCaches>>,
    /// The connector's digest sink (CH3a a7-1); default none.
    digest: Option<Box<dyn DigestSink>>,
    /// Replaces the helper the setup chose ([`Scripted`] or `NodeHelper`), given it: a recorder
    /// around it, or a recorded replay's (CH3a a7-1).
    wrap_helper: Option<WrapHelper>,
    /// The node's stop as ND1 and CH2b wire it (CH3a D137): the connector's `AbortNode` starts the
    /// node's shutdown, as ND1's `NodeAbort` does, and the flush side refuses every call once the
    /// latch has tripped ([`Refusing`]). Default: the connector's reports go to the sink only, and
    /// the flush side is [`Latched`].
    node_stop: bool,
    /// The flush side instead of the one [`Setup::node_stop`] picks.
    flush: Option<Box<dyn FlushSite>>,
    /// V's listener, and its read-ahead (default off).
    listener: Option<Arc<dyn ValidationInterface>>,
    read_ahead: ReadAheadSetup,
    /// The connector without `-consensusxcheck`, which the matrix has on (CH3a D189).
    no_xcheck: bool,
    /// A bug planted in the connector's core.
    #[cfg(feature = "planted-bugs")]
    planted: Option<crate::Planted>,
}

/// CH2b's flush side once the latch has tripped: every call fails, its failure reported by
/// whoever tripped the latch (`NodeFlush`'s `service`, which each call runs first, L-3). Until
/// then, and at V's stop, it is [`Latched`].
struct Refusing(GlobalFailStop);

impl Refusing {
    fn check(&self) -> Result<(), Fatal> {
        self.0.check().map_err(|f| Fatal::reported(f.0.message))
    }
}

impl FlushSite for Refusing {
    fn at_site(&mut self, _: FlushStateMode, _: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        self.check()?;
        Ok(SiteDecision::nothing())
    }

    fn after_step(&mut self, _: SiteDecision, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.check()
    }

    fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.check()
    }

    fn abort(&mut self, cause: AbortCause, host: &mut FlushHost<'_>) -> bool {
        Latched(self.0.clone()).abort(cause, host)
    }
}

/// Where [`FailsAtTip`] fails a tip step's flush (`v:3315`, `:3468` at v24.0.0): the site's
/// decision, the freeze after the step, or the in-hold drain's answer to W once the step's
/// publish found a subscriber's queue full (BL-559, [`stalled_feed`]).
#[derive(Clone, Copy, Debug)]
enum TipFlush {
    AtSite,
    AfterStep,
    Drain,
}

/// A subscriber at the tip whose queue holds one step and is never read: the next step fills it,
/// and the one after it waits in V's in-hold drain. Held while the test runs.
fn stalled_feed(rig: &Rig) -> impl Sized + use<> {
    let feeds = rig.v.query(|q| q.chain.feeds()).recv().expect("V");
    let id = feeds.register(Needs::default()).expect("registers");
    let tip = FeedSource::<()>::snapshot(&feeds).tip();
    let Ok(Handoff::Live {
        feed,
        ..
    }) = feeds.subscribe_at(id, &tip, &mut || Ok::<(), ()>(()))
    else {
        panic!("subscribe_at")
    };
    (feeds, feed)
}

/// A world for a [`FailsAtTip`] test: one whose subscriber's queue is full at the next step for
/// [`TipFlush::Drain`] (its feed returned), a plain one otherwise.
fn tip_flush_world(
    n: u8,
    at: TipFlush,
) -> (World, Mined, Arc<Mutex<Option<TipFlush>>>, impl Sized) {
    let (flush, arm) = FailsAtTip::armed();
    let setup = Setup {
        flush: Some(flush),
        ..Setup::default()
    };
    let stalls = matches!(at, TipFlush::Drain);
    let (w, b1, _) = disconnect::spent_with(n, setup, |cfg| RigConfig {
        feed_capacity: stalls.then_some(1),
        ..cfg
    });
    let feed = stalls.then(|| stalled_feed(&w.rig));
    (w, b1, arm, feed)
}

/// The `AbortNode` text of [`FailsAtTip`]'s flush failure (`v:3128` at v24.0.0).
const TIP_FLUSH_FAILED: &str = "System error while flushing: rig: the tip step's write failed";

/// [`Latched`], failing the next tip step's flush where the test arms it: the step's
/// `IF_NEEDED` site, or the freeze after the step. Its side reports the failure, as W's
/// `AbortNode` does, and V meets the failure reported (BL-559).
struct FailsAtTip {
    inner: Latched,
    arm: Arc<Mutex<Option<TipFlush>>>,
    after_step: bool,
    drain: bool,
}

impl FailsAtTip {
    /// The flush side, and the arm a test sets.
    fn armed() -> (Box<dyn FlushSite>, Arc<Mutex<Option<TipFlush>>>) {
        let arm = Arc::new(Mutex::new(None));
        let site = FailsAtTip {
            inner: Latched(GlobalFailStop::new()),
            arm: arm.clone(),
            after_step: false,
            drain: false,
        };
        (Box::new(site), arm)
    }

    fn fail() -> Fatal {
        tracing::error!("*** {TIP_FLUSH_FAILED}");
        Fatal::flush_failed(TIP_FLUSH_FAILED)
    }
}

impl FlushSite for FailsAtTip {
    fn at_site(&mut self, m: FlushStateMode, h: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        let decided = self.inner.at_site(m, h)?;
        if m == FlushStateMode::IfNeeded {
            match self.arm.lock().expect("arm").take() {
                Some(TipFlush::AtSite) => return Err(FailsAtTip::fail()),
                Some(TipFlush::AfterStep) => self.after_step = true,
                Some(TipFlush::Drain) => self.drain = true,
                None => {}
            }
        }
        Ok(decided)
    }

    fn after_step(&mut self, d: SiteDecision, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        if std::mem::take(&mut self.after_step) {
            return Err(FailsAtTip::fail());
        }
        self.inner.after_step(d, h)
    }

    fn service(&mut self, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.inner.service(h)
    }

    fn answer_due(&mut self, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        if std::mem::take(&mut self.drain) {
            return Err(FailsAtTip::fail());
        }
        self.inner.answer_due(h)
    }

    fn abort(&mut self, cause: AbortCause, h: &mut FlushHost<'_>) -> bool {
        self.inner.abort(cause, h)
    }
}

/// BL-786 (e), OD-CP-182 R2 (CH3a D192): `ConnectTip`'s scheme guard (`validation.cpp:3416` at
/// v24.0.0) commits only once its flush has returned (`:3467-3469`, `Commit()` at `:3488`), so a
/// flush that fails at the step's site, in the freeze after it or in the in-hold drain's answer
/// to W (BL-559) exits the guard uncommitted, once, after the flush's report. The rig's hooks
/// log each uncommitted exit with its `log`.
#[test]
fn a_connect_tips_flush_failure_exits_its_scheme_guard() {
    install_accept_log();
    let logs = format!("{GUARD_EXIT}true");
    let flushed = format!("*** {TIP_FLUSH_FAILED}");
    for (n, at) in [(180, TipFlush::AtSite), (181, TipFlush::AfterStep), (192, TipFlush::Drain)] {
        let (mut w, b1, arm, _feed) = tip_flush_world(n, at);
        let v = w.rig.v_thread();
        let p = w.rig.p.clone();
        let b2 = block_on(&p, &b1, vec![]);
        assert!(w.rig.submit(&b2).accepted);
        let b3 = block_on(&p, &b2, vec![]);
        *arm.lock().expect("arm") = Some(at);
        let case = format!("{at:?}");
        w.rig.submit_stopping(&b3, &case);
        drop(w.rig.stop());
        assert_eq!(levels_on(v, &logs).len(), 1, "{case}: one exit");
        disconnect::assert_in_order(v, &[flushed.as_str(), logs.as_str()], &case);
    }
}

/// [`Setup::wrap_helper`].
type WrapHelper = Box<dyn FnOnce(Box<dyn ChainHelper>) -> Box<dyn ChainHelper>>;

/// An undo-file failure [`UndoFs`] injects (CH3 §2.1.1's undo rows).
#[derive(Clone, Copy, Debug, PartialEq)]
enum UndoFault {
    /// `CheckDiskSpace` finds no room: ST2's `NoSpace`.
    NoSpace,
    /// A rev file's write fails.
    Write,
    /// A rev file's `fdatasync` fails.
    Flush,
    /// Another failure latches at the site, unreported, while the record is written: the write
    /// fails after it.
    Latched(&'static str),
    /// W's chainstate write fails while the record is written: it latches, W reports, and the
    /// write fails after it (BL-546 r1 M-1).
    WLatched,
    /// As [`WLatched`](Self::WLatched), on W's own thread, its report deferred and held (BL-554).
    WHeld,
    /// W's panic while the record is written: it latches at [`W_PANIC_SITE`], W reports it as
    /// its thread's end does, and the write fails after it (BL-586).
    WPanicked,
}

/// The OS file system with the undo-file failure a test arms.
#[derive(Default)]
struct UndoFs {
    os: OsFs,
    fault: Mutex<Option<UndoFault>>,
    /// The node's latch, for [`UndoFault::Latched`].
    latch: Mutex<Option<GlobalFailStop>>,
    /// The node's sink, for W's report at [`UndoFault::WLatched`] and [`UndoFault::WPanicked`].
    sink: Mutex<Option<Arc<RecordingAbort>>>,
    /// W's way to `sink` at [`UndoFault::WHeld`], and W's thread once it ran.
    held: Mutex<Option<Arc<HeldSink>>>,
    w: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl UndoFs {
    fn arm(&self, fault: UndoFault) {
        *self.fault.lock().expect("fault") = Some(fault);
    }

    /// The armed fault, for a rev file.
    fn fault(&self, path: &Path) -> Option<UndoFault> {
        let rev = path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("rev"));
        rev.then(|| *self.fault.lock().expect("fault")).flatten()
    }

    /// W's failure `failure` before the write: it trips the latch and, if it won, reports
    /// `report`; the write then fails as the latch refuses it.
    fn w_reports(&self, failure: Failure, report: &str) -> io::Result<()> {
        let latch = self.latch.lock().expect("latch").clone().expect("a latch");
        if latch.trip(failure) {
            let sink = self.sink.lock().expect("sink").clone().expect("a sink");
            sink.abort_node(report, None);
        }
        Err(io::Error::other("a write after the latch"))
    }
}

impl FlatFs for UndoFs {
    fn link_count(&self, path: &Path) -> io::Result<Option<u64>> {
        self.os.link_count(path)
    }
    fn len(&self, path: &Path) -> io::Result<Option<u64>> {
        self.os.len(path)
    }
    fn create(&self, path: &Path) -> io::Result<()> {
        self.os.create(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FlatRead + '_>> {
        self.os.open_read(path)
    }
    fn read_exact_at(&self, path: &Path, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.os.read_exact_at(path, offset, buf)
    }
    fn write_all_at(&self, path: &Path, offset: u64, data: &[u8]) -> io::Result<()> {
        match self.fault(path) {
            Some(UndoFault::Write) => Err(io::Error::other("injected write failure")),
            Some(UndoFault::Latched(site)) => {
                let latch = self.latch.lock().expect("latch").clone().expect("a latch");
                latch.trip(Failure {
                    kind: dashd_store::ErrorKind::Io,
                    site,
                    message: OTHER_STORE_FAILED.to_owned(),
                });
                Err(io::Error::other("a write after the latch"))
            }
            Some(UndoFault::WLatched) => self.w_reports(
                Failure {
                    kind: dashd_store::ErrorKind::Io,
                    site: "chainstate write",
                    message: W_FAILED.to_owned(),
                },
                W_REPORTED,
            ),
            Some(UndoFault::WPanicked) => self.w_reports(
                Failure {
                    kind: dashd_store::ErrorKind::Other,
                    site: W_PANIC_SITE,
                    message: W_PANICKED.to_owned(),
                },
                W_PANICKED,
            ),
            Some(UndoFault::WHeld) => {
                let latch = self.latch.lock().expect("latch").clone().expect("a latch");
                let held = self.held.lock().expect("held").clone().expect("a held sink");
                *self.w.lock().expect("w") = Some(w_fails(&latch, held));
                Err(io::Error::other("a write after the latch"))
            }
            _ => self.os.write_all_at(path, offset, data),
        }
    }
    fn allocate(&self, path: &Path, offset: u64, len: u64) {
        self.os.allocate(path, offset, len);
    }
    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        self.os.truncate(path, len)
    }
    fn sync_data(&self, path: &Path) -> io::Result<()> {
        if self.fault(path) == Some(UndoFault::Flush) {
            return Err(io::Error::other("injected sync failure"));
        }
        self.os.sync_data(path)
    }
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.os.sync_dir(dir)
    }
    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.os.copy(from, to)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.os.rename(from, to)
    }
    fn remove(&self, path: &Path) -> io::Result<bool> {
        self.os.remove(path)
    }
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        self.os.create_dir_all(dir)
    }
    fn available_space(&self, dir: &Path) -> io::Result<u64> {
        if *self.fault.lock().expect("fault") == Some(UndoFault::NoSpace) {
            return Ok(0);
        }
        self.os.available_space(dir)
    }
}

/// What outlives a node: the active coins, the undo files' directory and file system, and V's
/// block files.
#[derive(Clone)]
struct Stores {
    coins: ChainstateCell<ProbeEngine>,
    dir: Arc<tempfile::TempDir>,
    fs: Arc<UndoFs>,
    blocks: SharedFiles,
}

impl Stores {
    fn new() -> Stores {
        let db = Arc::new(GuardedEngine::probe(Arc::new(RecordingSink::default())));
        let budget = CoinsBudget::new(8 << 20, 0, Ratio::HALF, 0);
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::DirBuilder::new().mode(0o700).create(dir.path().join("blocks")).expect("dir");
        Stores {
            coins: Arc::new(Mutex::new(CoinsCache::new(db, budget, Salt::fixed(7), None))),
            dir: Arc::new(dir),
            fs: Arc::default(),
            blocks: SharedFiles::default(),
        }
    }

    /// The undo files under a process's `latch`, from `state`. A restart starts from the
    /// setup's state: these tests write no undo after one.
    fn undo_files(
        &self,
        latch: &GlobalFailStop,
        state: BlockFilesState,
    ) -> Arc<GuardedFiles<UndoFs>> {
        *self.fs.latch.lock().expect("latch") = Some(latch.clone());
        let files = BlockFiles::with_fs(
            Arc::clone(&self.fs),
            BlockFilesConfig::new(self.dir.path().join("blocks"), MAGIC),
            state,
        );
        Arc::new(GuardedFiles::new(files, latch.clone()))
    }
}

/// V's block files in a world: the blocks [`SharedFiles`] holds, and the undo records the
/// connector wrote, read as production reads them (`UndoReadFromDisk`, VerifyDB's level 2).
struct WorldFiles {
    blocks: SharedFiles,
    undo: Arc<GuardedFiles<UndoFs>>,
}

impl crate::chain::BlockFiles for WorldFiles {
    fn save_block(
        &mut self,
        block: &Arc<Block>,
        height: u32,
        known: Option<FlatFilePos>,
    ) -> Result<Option<FlatFilePos>, String> {
        self.blocks.save_block(block, height, known)
    }

    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        crate::chain::BlockFiles::read_block(&self.blocks, pos, hash)
    }

    fn open_block_file(&self, file: i32) -> bool {
        self.blocks.open_block_file(file)
    }

    fn current_usage(&self) -> u64 {
        self.blocks.current_usage()
    }

    fn scan_already_pruned_files(&mut self, have_pruned: bool) -> Result<(), String> {
        self.blocks.scan_already_pruned_files(have_pruned)
    }

    fn read_undo(
        &self,
        pos: FlatFilePos,
        parent: &BlockHash,
    ) -> Result<dash_consensus::coins::BlockUndo, String> {
        let at = crate::store::store_pos(pos).ok_or("UndoReadFromDisk: OpenUndoFile failed")?;
        let reader = self.undo.read(|f| f.reader());
        let mut buf = Vec::new();
        reader
            .read_undo_record(at, parent.as_bytes(), &mut buf, |u| u.to_block_undo())
            .map_err(|e| format!("UndoReadFromDisk: {e}"))
    }

    fn read_ahead(&self) -> Option<Arc<dyn crate::chain::BlockSource>> {
        self.blocks.read_ahead()
    }
}

/// [`Seen::read_in_next_h4`]: an evo row from `db`, as [`Seen::evo_read_in_next_undo`]'s H9
/// reads, or a coin through H4's view over it (`coins`), with its reads failing, inside a nested
/// scope as `NodeEvo`'s `CMNHFManager::ProcessBlock` call (`nested`); then, if `refuse_apply`,
/// [`Seen::fail_next_apply`] for the attempt.
struct H4Read {
    db: Arc<GuardedEngine<ProbeEngine>>,
    coins: bool,
    nested: bool,
    refuse_apply: bool,
}

/// An evo row read from `db` with its reads failing, its error as the evo stores return it
/// (`evo::node`'s `store_fault`): a plain `Fatal`, not a coins read's class.
fn failing_evo_read(db: &GuardedEngine<ProbeEngine>) -> Result<(), Fatal> {
    use dashd_store::KvRead as _;
    db.inner().fail_reads(Some(dashd_store::ErrorKind::Io));
    let cf = crate::store::CHAINSTATE_SCHEMA.cf("llmq_snapshot").expect("the schema has it");
    match db.get(cf, b"snapshot") {
        Err(e) => Err(Fatal::new(format!("{EVO_READ}: {e}"))),
        Ok(_) => Ok(()),
    }
}

/// What the harness saw.
#[derive(Default)]
struct Seen {
    /// H11 ran in the open attempt and its `finish_attempt` is to fail
    /// ([`StepsCfg::fail_finish_after_h11`]).
    finish_after_h11: bool,
    /// The effects `finish_attempt` applied, in order.
    effects: Vec<Effect>,
    /// The undo bytes of each connected block, by height.
    undo: Vec<(u32, Vec<u8>)>,
    /// The bracket's and the helper's calls, in order.
    calls: Vec<Call>,
    /// The scripted InstantSend store, less the closures of the removals applied.
    is_store: Vec<IsLock>,
    /// The evo best block `commit_tx` committed.
    evo_best: Option<BlockHash>,
    /// The last attempt's [`EvoStep`]s, in order.
    evo_steps: Vec<EvoStep>,
    /// The node's latch.
    latch: GlobalFailStop,
    /// For each dropped attempt, whether the latch had tripped when it dropped.
    dropped_latched: Vec<bool>,
    /// The scripted helper's queries, every method but `end_attempt`.
    queries: usize,
    /// The connector's script caches.
    caches: Option<Arc<ScriptCaches>>,
    /// Each H4 call's `fJustCheck` (`v:2663-2664`), in order.
    just_checks: Vec<bool>,
    /// Armed by a test: the next `finish_attempt` fails, with effects or none.
    fail_next_apply: bool,
    /// With [`Seen::fail_next_apply`], the failure is an undo's snapshot write in these rows, whose
    /// store fails it (OD-CP-18).
    undo_rows: Option<Arc<RuntimeRows>>,
    /// With [`Seen::undo_rows`], another route's write in the rows fails first, reported by their
    /// store, so the latch refuses the undo's (OD-CP-29).
    undo_after_a_report: bool,
    /// With [`Seen::undo_rows`], W panics first and reports it through this sink, so the latch
    /// refuses the undo's write with the assert (BL-775 Sol r1).
    undo_after_an_assert: Option<Arc<RecordingAbort>>,
    /// Armed by a test: the next H9 is a `Fatal`.
    fail_next_undo: bool,
    /// Armed by a test: the next H9 reads an evo row from this engine with its reads failing,
    /// and returns the error as the evo stores do (`evo::node`'s `store_fault`): a plain `Fatal`,
    /// not a coins read's class.
    evo_read_in_next_undo: Option<Arc<GuardedEngine<ProbeEngine>>>,
    /// Armed by a test: the next H4's failing read.
    read_in_next_h4: Option<H4Read>,
    /// Armed by a test: the next H9 is [`Seen::fail_next_undo`]'s text as an `AbortNode`, which V
    /// reports latched or not (H9's `Fatal::abort_node` class). No `undo_block` verdict gives H9
    /// this class today (OD-CP-153).
    fail_next_undo_always: bool,
    /// Armed by a test: the next H9 trips the node latch and reports through the node's sink, as a
    /// store reports the failure it latches, then is [`Seen::fail_next_undo`]'s `Fatal`.
    fail_next_undo_latched: bool,
    /// Armed by a test: the next H9 meets W's panic, which trips the node latch and reports
    /// through the node's sink as W's thread's end does, then is [`Seen::fail_next_undo`]'s
    /// `Fatal` (BL-775 Sol r1).
    fail_next_undo_after_a_panic: bool,
    /// Each connected block's script work, by height: (height, transactions that built their
    /// signature-hash data, inputs whose spent script was copied).
    scripts: Vec<(u32, usize, usize)>,
    /// The connector's check queue.
    queue: Option<Arc<CheckQueue>>,
    /// The node's sink, for [`StepsCfg::trip_then_panic_in_h4`]'s report.
    sink: Option<Arc<RecordingAbort>>,
    /// W's way to the node's sink at [`StepsCfg::w_fails_at_verify`] and
    /// [`Scripted::cl_fails_after_w`], and W's thread once it ran.
    held: Option<Arc<HeldSink>>,
    w: Option<std::thread::JoinHandle<()>>,
}

impl Seen {
    /// The calls of the last attempt (from its `begin_attempt`).
    fn last_attempt(&self) -> &[Call] {
        let from = self.calls.iter().rposition(|c| *c == Call::BeginAttempt).expect("an attempt");
        &self.calls[from..]
    }

    /// The bracket's calls of the last attempt, in order.
    fn last_bracket(&self) -> Vec<Call> {
        self.last_attempt().iter().copied().filter(|c| !c.is_helper()).collect()
    }
}

/// What a scratch connect meets: a deployment only it sees, or a system error that is not a
/// readiness refusal, returned before the core runs.
#[derive(Clone, Copy)]
enum ScratchConnect {
    Extra(Deployment),
    SystemError(&'static str),
}

/// [`CoreConnector`] over its own coins and undo files, with what a test changes around it:
/// the coins seeded before block 1, a deployment only the core sees, a block that reads back
/// otherwise.
struct Harness<const C: bool> {
    inner: CoreConnector<ProbeEngine, UndoFs, TestEvo<C>>,
    coins: ChainstateCell<ProbeEngine>,
    seeds: Vec<(OutPoint, TxOut, u32, bool)>,
    extra: Option<Deployment>,
    disconnect_extra: Option<Deployment>,
    scratch_connect: Option<ScratchConnect>,
    locked_at: Option<u32>,
    substitute: Option<(BlockHash, Arc<Block>)>,
    seen: Arc<Mutex<Seen>>,
}

impl<const C: bool> BlockConnector for Harness<C> {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        let height = cx.reader.entry(id).expect("indexed").height;
        if height == 1 && !self.seeds.is_empty() {
            let mut coins = self.coins.lock().expect("coins");
            let best = coins.best_block().expect("genesis is connected");
            let mut ov = BlockOverlay::new(&mut coins);
            for (op, out, height, coinbase) in self.seeds.drain(..) {
                let src = CoinSrc::Output {
                    out: &out,
                    height,
                    coinbase,
                };
                ov.add(&OutPointKey::new(&op), src, false).expect("seeded");
            }
            ov.commit(best).expect("seeded");
        }
        let (block, checked) = match &self.substitute {
            Some((hash, read)) if *hash == block.hash() => (&**read, false),
            _ => (block, checked),
        };
        let mut extra = self.extra;
        if matches!(target, ViewTarget::Scratch(_)) {
            match self.scratch_connect {
                Some(ScratchConnect::Extra(d)) => extra = Some(d),
                Some(ScratchConnect::SystemError(msg)) => {
                    return Ok(ConnectOutcome::SystemError(msg.to_owned()));
                }
                None => {}
            }
        }
        let inner = &mut self.inner;
        let out = changed_cx(extra, self.locked_at, cx, |cx| {
            inner.connect_block(cx, v, target, block, checked, id, st)
        })?;
        // The genesis block has no undo (v:2537-2543).
        if out == ConnectOutcome::Connected && height > 0 {
            let undo = self.inner.last_undo().to_vec();
            let mut seen = self.seen.lock().expect("seen");
            seen.undo.push((height, undo));
            let (sig_data, copied) = self.inner.last_script_work();
            seen.scripts.push((height, sig_data, copied));
        }
        Ok(out)
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        let inner = &mut self.inner;
        changed_cx(self.extra.or(self.disconnect_extra), self.locked_at, cx, |cx| {
            inner.disconnect_block(cx, v, target, block, id)
        })
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        block: &Block,
        prev: BlockId,
        pow: bool,
        merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        let inner = &mut self.inner;
        changed_cx(self.extra, self.locked_at, cx, |cx| {
            inner.test_block_validity(cx, v, checks, block, prev, pow, merkle)
        })
    }

    fn published_bls(&self) -> Option<BlsCtx> {
        self.inner.published_bls()
    }

    fn verify_scratch(
        &mut self,
        parent: ScratchParent,
    ) -> Result<Option<Box<dyn ScratchView>>, Fatal> {
        self.inner.verify_scratch(parent)
    }

    fn take_scratch_refusal(&mut self) -> Option<String> {
        self.inner.take_scratch_refusal()
    }

    fn take_undo_fault(&mut self) -> Option<Fatal> {
        self.inner.take_undo_fault()
    }

    fn open_scheme_guard(&mut self, holder: SchemeGuardHolder) {
        self.inner.open_scheme_guard(holder);
    }

    fn commit_scheme_guard(&mut self) {
        self.inner.commit_scheme_guard();
    }

    fn exit_scheme_guard(&mut self, log: bool) {
        self.inner.exit_scheme_guard(log);
    }
}

/// A rig over the harness, its abort sink, what the harness saw and the active coins.
struct World {
    rig: Rig,
    sink: Arc<RecordingAbort>,
    seen: Arc<Mutex<Seen>>,
    coins: ChainstateCell<ProbeEngine>,
    stores: Stores,
}

fn harness<const C: bool>(
    p: &ChainSetup,
    mut setup: Setup,
    path: AbortPath,
    stores: &Stores,
) -> (Box<dyn BlockConnector>, Arc<Mutex<Seen>>) {
    let caches = setup.caches.take().unwrap_or_else(new_caches);
    let queue = Arc::new(CheckQueue::new(setup.script_threads).expect("queue"));
    let seen = Arc::new(Mutex::new(Seen {
        is_store: std::mem::take(&mut setup.is_store),
        latch: path.latch().clone(),
        caches: Some(caches.clone()),
        queue: Some(queue.clone()),
        ..Seen::default()
    }));
    setup.helper.seen = Some(seen.clone());
    let helper: Box<dyn ChainHelper> = if setup.node_helper {
        Box::new(NodeHelper)
    } else {
        Box::new(setup.helper)
    };
    let parts = ConnectorParts {
        coins: stores.coins.clone(),
        files: stores.undo_files(path.latch(), std::mem::take(&mut setup.undo_state)),
        queue,
        caches,
        helper: match setup.wrap_helper.take() {
            Some(wrap) => wrap(helper),
            None => helper,
        },
        abort: path,
        assume_valid: setup.assume_valid,
        digest: setup.digest.take(),
        // The matrix's connects compare every memo read with the overlay (CH3a D189); the
        // production path, xcheck off, is the rigs', the CRASH model's and the benches'.
        xcheck: !setup.no_xcheck,
    };
    let hooks = TestEvo::<C> {
        cfg: setup.steps,
        seen: seen.clone(),
        staged: None,
        attempts: 0,
        scratch: false,
        h8_calls: 0,
        guard_committed: false,
    };
    let inner = CoreConnector::new(p, parts, hooks, Arc::new(())).expect("connector");
    #[cfg(feature = "planted-bugs")]
    let inner = {
        let mut inner = inner;
        inner.plant(setup.planted);
        inner
    };
    let h = Harness {
        inner,
        coins: stores.coins.clone(),
        seeds: setup.seeds,
        extra: setup.extra,
        disconnect_extra: setup.disconnect_extra,
        scratch_connect: setup.scratch_connect,
        locked_at: setup.locked_at,
        substitute: setup.substitute,
        seen: seen.clone(),
    };
    (Box::new(h), seen)
}

/// A node over `setup`'s stores (new ones by default): the harness, the rig's configuration
/// around it, and what the world keeps.
fn node(mut setup: Setup) -> (RigConfig, Stores, Arc<RecordingAbort>, Arc<Mutex<Seen>>) {
    let p = setup.params.take().unwrap_or_else(regtest);
    let stores = setup.stores.take().unwrap_or_else(Stores::new);
    let sink = Arc::new(RecordingAbort::default());
    *stores.fs.sink.lock().expect("sink") = Some(sink.clone());
    *stores.fs.held.lock().expect("held") = Some(Arc::new(HeldSink::new(sink.clone())));
    let latch = GlobalFailStop::new();
    let shutdown = Arc::new(LocalShutdown::default());
    let node_stop = setup.node_stop;
    let reports: Arc<dyn AbortSink> = if node_stop {
        Arc::new(AbortThenShutdown {
            sink: sink.clone(),
            shutdown: shutdown.clone(),
        })
    } else {
        sink.clone()
    };
    let path = AbortPath::new(reports, latch.clone());
    let (guard, after_genesis, min_chain_work) =
        (setup.guard, setup.after_genesis, setup.min_chain_work);
    let (flush, listener) = (setup.flush.take(), setup.listener.take());
    let read_ahead = std::mem::take(&mut setup.read_ahead);
    let (connector, seen) = if setup.steps.complete {
        harness::<true>(&p, setup, path, &stores)
    } else {
        harness::<false>(&p, setup, path, &stores)
    };
    seen.lock().expect("seen").sink = Some(sink.clone());
    let undo = stores.undo_files(&latch, BlockFilesState::default());
    let flush = flush.unwrap_or_else(|| -> Box<dyn FlushSite> {
        if node_stop {
            Box::new(Refusing(latch))
        } else {
            Box::new(Latched(latch))
        }
    });
    let cfg = RigConfig {
        params: Some(p),
        files: Some(Box::new(WorldFiles {
            blocks: stores.blocks.clone(),
            undo,
        })),
        connector: Some(connector),
        flush: Some(flush),
        shutdown: Some(shutdown),
        abort: Some(sink.clone()),
        guard,
        after_genesis,
        min_chain_work,
        listener,
        read_ahead,
        ..RigConfig::default()
    };
    (cfg, stores, sink, seen)
}

fn world(setup: Setup) -> World {
    world_with(setup, |cfg| cfg)
}

/// [`world`] with the rig's other seams `rig` sets: ChainLocks, InstantSend's store.
fn world_with(setup: Setup, rig: impl FnOnce(RigConfig) -> RigConfig) -> World {
    let (cfg, stores, sink, seen) = node(setup);
    let rig = Rig::with(rig(cfg));
    World {
        rig,
        sink,
        seen,
        coins: stores.coins.clone(),
        stores,
    }
}

fn out(value: Amount, script: &[u8]) -> TxOut {
    TxOut::from_duffs(value, script.to_vec().into())
}

fn seed_op(n: u8) -> OutPoint {
    OutPoint::new(TxHash::from([n; 32]), 0)
}

/// A seeded, mature, non-coinbase coin of `value` to `script`.
fn seed(n: u8, value: Amount, script: &[u8]) -> (OutPoint, TxOut, u32, bool) {
    (seed_op(n), out(value, script), 0, false)
}

fn tx(version: i16, tx_type: u16, inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Arc<Tx> {
    Arc::new(Tx::new(Transaction::new(
        version,
        TxType::new(tx_type),
        inputs,
        outputs,
        0,
        Vec::new(),
    )))
}

fn input(prevout: OutPoint, script_sig: Vec<u8>, sequence: u32) -> TxIn {
    TxIn::new(prevout, script_sig.into(), sequence)
}

/// A transaction spending `prevouts` with empty scriptSigs into one `OP_TRUE` output of `value`.
fn spend(prevouts: &[OutPoint], value: Amount) -> Arc<Tx> {
    let inputs = prevouts.iter().map(|op| input(*op, Vec::new(), u32::MAX)).collect();
    tx(1, 0, inputs, vec![out(value, &[OP_TRUE])])
}

/// The coinbase of the block at `height`: `version` and raw `tx_type`, paying `value`.
fn coinbase_tx(height: u32, version: i16, tx_type: u16, value: Amount) -> Arc<Tx> {
    let null = OutPoint::new(TxHash::from([0; 32]), u32::MAX);
    tx(
        version,
        tx_type,
        vec![input(null, cb_script(height, 0), u32::MAX)],
        vec![out(value, &[OP_TRUE])],
    )
}

/// The block after `parent` with a version-1 coinbase paying 1 DASH, then `txs`.
fn block_on(p: &ChainSetup, parent: &Mined, txs: Vec<Arc<Tx>>) -> Mined {
    with_coinbase(p, parent, coinbase_tx(parent.height + 1, 1, 0, COIN), txs)
}

fn with_coinbase(p: &ChainSetup, parent: &Mined, cb: Arc<Tx>, txs: Vec<Arc<Tx>>) -> Mined {
    let mut all = vec![cb];
    all.extend(txs);
    Mined {
        block: mine(p, parent.hash(), parent.time() + 150, all),
        height: parent.height + 1,
    }
}

fn op(tx: &Tx, index: u32) -> OutPoint {
    OutPoint::new(tx.txid(), index)
}

/// The block the active coins are at.
fn coins_at(coins: &ChainstateCell<ProbeEngine>) -> Option<BlockHash> {
    coins.lock().expect("coins").best_block().map(|b| b.hash)
}

/// The block `hash` at `height`, a proposal: no parent or index id.
fn block_at(hash: BlockHash, height: u32) -> BlockAt {
    BlockAt {
        hash,
        height,
        prev: None,
        id: None,
    }
}

/// [`block_at`] for `m`.
fn at(m: &Mined) -> BlockAt {
    block_at(m.hash(), m.height)
}

/// The parent of the last of `blocks` (`genesis` for one block).
fn parent<'a>(genesis: &'a Mined, blocks: &'a [Mined]) -> &'a Mined {
    blocks.len().checked_sub(2).map_or(genesis, |i| &blocks[i])
}

/// The states `BlockChecked` reported, in order.
pub(super) fn checked(rig: &Rig) -> Vec<(BlockHash, BlockState)> {
    rig.rec
        .take()
        .into_iter()
        .filter_map(|e| match e {
            Ev::Checked(h, s) => Some((h, s)),
            _ => None,
        })
        .collect()
}

fn reject(result: BlockValidationResult, reason: &str) -> BlockState {
    BlockState::Invalid(BlockValidationError::new(result, reason.to_owned()))
}

/// Submits `blocks` in order; the last one's `BlockChecked` state.
fn last_state(w: &World, blocks: &[Mined]) -> BlockState {
    for m in blocks {
        w.rig.submit(m);
    }
    let states = checked(&w.rig);
    let (hash, state) = states.last().cloned().expect("a BlockChecked");
    assert_eq!(hash, blocks.last().expect("a block").hash());
    state
}

/// The `AbortNode` text V stopped on, if it did.
fn fatal_text(exit: &VExit) -> Option<&str> {
    exit.fatal.as_ref().map(|f| f.0.as_str())
}

/// [`last_state`], which must be `Invalid`: its error.
fn last_invalid(w: &World, blocks: &[Mined]) -> BlockValidationError {
    match last_state(w, blocks) {
        BlockState::Invalid(err) => err,
        other => panic!("{other:?}, want Invalid"),
    }
}

/// `got` is `want`: for an `Invalid` one, Core's result and reason (not its debug message).
fn assert_verdict(got: &BlockState, want: &BlockState) {
    match (got, want) {
        (BlockState::Invalid(got), BlockState::Invalid(want)) => {
            assert_eq!((got.result(), got.reject_reason()), (want.result(), want.reject_reason()));
        }
        _ => assert_eq!(got, want),
    }
}

/// New script caches of the default sizes.
fn new_caches() -> Arc<ScriptCaches> {
    Arc::new(ScriptCaches::new(CacheSizes::default()).expect("caches"))
}

/// A block that fails `ConnectBlock`: invalid with Core's result and reason, `FAILED_VALID`, no
/// undo, the tip at its parent, nothing reported (CH3 §2.2's table).
fn assert_rejected(
    setup: Setup,
    build: impl FnOnce(&ChainSetup, &Mined) -> Vec<Mined>,
    want: BlockState,
) {
    let w = world(setup);
    let blocks = build(&w.rig.p, &w.rig.genesis);
    let state = last_state(&w, &blocks);
    let last = blocks.last().expect("a block");
    assert_verdict(&state, &want);
    let status = w.rig.status(last);
    assert!(status.has(Status::FAILED_VALID), "{status:?}");
    assert!(!status.has(Status::HAVE_UNDO));
    w.rig.assert_no_abort();
    assert!(w.sink.messages().is_empty(), "{:?}", w.sink.messages());
    // Nothing of the attempt is committed (D33).
    assert_eq!(coins_at(&w.coins), Some(parent(&w.rig.genesis, &blocks).hash()));
}

/// A block whose connect is a system error: `msg` reported once, V stopped by it, the block left
/// `HAVE_DATA` without undo or a failure bit (CH3 §2.1.1, §2.17).
fn assert_system_error(
    setup: Setup,
    build: impl FnOnce(&ChainSetup, &Mined) -> Vec<Mined>,
    msg: &str,
) {
    let mut w = world(setup);
    let blocks = build(&w.rig.p, &w.rig.genesis);
    let (last, before) = blocks.split_last().expect("a block");
    for m in before {
        w.rig.accept(m);
    }
    w.rig.submit_stopping(last, "");
    let id = w.rig.id(last);
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), Some(msg));
    assert_eq!(w.sink.messages(), [msg], "one report");
    let status = exit.chain.reader().entry(id).expect("indexed").status;
    assert!(status.has(Status::HAVE_DATA), "{status:?}");
    assert!(!status.has(Status::HAVE_UNDO) && !status.has(Status::FAILED_VALID), "{status:?}");
    assert_eq!(coins_at(&w.coins), Some(parent(&w.rig.genesis, &blocks).hash()));
}

#[test]
fn a_block_connects_with_its_undo_and_valid_scripts() {
    let w = world(Setup {
        seeds: vec![seed(1, 50 * COIN, &[OP_TRUE])],
        ..Setup::default()
    });
    let p = w.rig.p.clone();
    let t1 = spend(&[seed_op(1)], 49 * COIN);
    let b1 = block_on(&p, &w.rig.genesis, vec![t1.clone()]);
    // A chained spend inside the next block.
    let t2 = spend(&[op(&t1, 0)], 48 * COIN);
    let t3 = spend(&[op(&t2, 0)], 47 * COIN);
    let b2 = block_on(&p, &b1, vec![t2, t3]);
    assert!(!w.rig.status(&w.rig.genesis).has(Status::HAVE_UNDO));
    for m in [&b1, &b2] {
        w.rig.accept(m);
        assert!(w.rig.status(m).has(Status::HAVE_UNDO));
        assert!(w.rig.status(m).is_valid(Status::VALID_SCRIPTS));
    }
    assert_eq!(
        checked(&w.rig).into_iter().map(|(_, s)| s).collect::<Vec<_>>(),
        [BlockState::Valid, BlockState::Valid]
    );
    // The streamed undo is Core's `serialize(CBlockUndo)`: one entry per non-coinbase
    // transaction, each spent coin in input order (v:2747-2755).
    let seen = w.seen.lock().expect("seen");
    let heights: Vec<u32> = seen.undo.iter().map(|(h, _)| *h).collect();
    assert_eq!(heights, [1, 2]);
    let coin = |value, height| TxUndo {
        coins: vec![Coin::new(value, &[OP_TRUE], height, false)],
    };
    let want = [vec![coin(50 * COIN, 0)], vec![coin(49 * COIN, 1), coin(48 * COIN, 2)]];
    for ((_, bytes), want) in seen.undo.iter().zip(want) {
        let mut input = bytes.as_slice();
        let decoded = decode_block_undo(&mut input).expect("a CBlockUndo");
        assert!(input.is_empty() && decoded.wire_is_canonical());
        assert_eq!(decoded.value().txs, want);
    }
    w.rig.assert_no_abort();
    assert!(w.sink.messages().is_empty());
    assert_eq!(coins_at(&w.coins), Some(b2.hash()));
}

#[test]
fn spending_a_missing_coin_is_check_tx_inputs_reason() {
    assert_rejected(
        Setup::default(),
        |p, g| vec![block_on(p, g, vec![spend(&[seed_op(9)], COIN)])],
        reject(BlockValidationResult::Consensus, "bad-txns-inputs-missingorspent"),
    );
}

/// xcheck reaches the memo (CH3a D189). Planted L1 answers a read of input 1 with input 0's coin,
/// so a spend of 30 and 20 paying out 55 reads 60 in. With the matrix's xcheck V stops on the
/// `consensusxcheck` Fatal with nothing committed; without it the invalid block connects. Unplanted
/// it is `CheckTxInputs`' `bad-txns-in-belowout`.
#[cfg(feature = "planted-bugs")]
#[test]
fn xcheck_catches_a_memo_that_answers_the_wrong_input() {
    let setup = |planted, no_xcheck| Setup {
        seeds: vec![seed(1, 30 * COIN, &[OP_TRUE]), seed(2, 20 * COIN, &[OP_TRUE])],
        planted,
        no_xcheck,
        ..Setup::default()
    };
    let build = |p: &ChainSetup, g: &Mined| {
        vec![block_on(p, g, vec![spend(&[seed_op(1), seed_op(2)], 55 * COIN)])]
    };
    assert_rejected(
        setup(None, false),
        build,
        reject(BlockValidationResult::Consensus, "bad-txns-in-belowout"),
    );

    let w = world(setup(Some(crate::Planted::L1), true));
    let blocks = build(&w.rig.p, &w.rig.genesis);
    w.rig.accept(&blocks[0]);
    assert_eq!(coins_at(&w.coins), Some(blocks[0].hash()));
    drop(w);

    let mut w = world(setup(Some(crate::Planted::L1), false));
    let blocks = build(&w.rig.p, &w.rig.genesis);
    w.rig.submit_stopping(&blocks[0], "");
    let exit = w.rig.stop();
    let text = fatal_text(&exit).unwrap_or_default();
    assert!(text.starts_with("consensusxcheck: the memo's coin for"), "{text}");
    assert_eq!(w.sink.messages(), [text], "one report");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(w.rig.genesis.hash()));
    assert_eq!(coins_at(&w.coins), Some(w.rig.genesis.hash()));
}

#[test]
fn an_immature_coinbase_spend_is_premature() {
    let mut s = seed(1, 50 * COIN, &[OP_TRUE]);
    s.3 = true;
    assert_rejected(
        Setup {
            seeds: vec![s],
            ..Setup::default()
        },
        |p, g| vec![block_on(p, g, vec![spend(&[seed_op(1)], COIN)])],
        reject(BlockValidationResult::Consensus, "bad-txns-premature-spend-of-coinbase"),
    );
}

#[test]
fn a_repeated_unspent_transaction_is_bip30() {
    assert_rejected(
        Setup {
            seeds: vec![seed(1, 50 * COIN, &[OP_TRUE])],
            ..Setup::default()
        },
        |p, g| {
            let t = spend(&[seed_op(1)], COIN);
            let b1 = block_on(p, g, vec![t.clone()]);
            let b2 = block_on(p, &b1, vec![t]);
            vec![b1, b2]
        },
        reject(BlockValidationResult::Consensus, "bad-txns-BIP30"),
    );
}

#[test]
fn inputs_beyond_max_money_are_out_of_range() {
    let big = 15_000_000 * COIN;
    assert_rejected(
        Setup {
            seeds: vec![seed(1, big, &[OP_TRUE]), seed(2, big, &[OP_TRUE])],
            ..Setup::default()
        },
        |p, g| vec![block_on(p, g, vec![spend(&[seed_op(1), seed_op(2)], COIN)])],
        reject(BlockValidationResult::Consensus, "bad-txns-inputvalues-outofrange"),
    );
}

#[test]
fn outputs_above_inputs_are_in_below_out() {
    assert_rejected(
        Setup {
            seeds: vec![seed(1, COIN, &[OP_TRUE])],
            ..Setup::default()
        },
        |p, g| vec![block_on(p, g, vec![spend(&[seed_op(1)], 2 * COIN)])],
        reject(BlockValidationResult::Consensus, "bad-txns-in-belowout"),
    );
}

#[test]
fn fees_beyond_max_money_are_out_of_range() {
    assert_rejected(
        Setup {
            seeds: vec![
                seed(1, 15_000_000 * COIN, &[OP_TRUE]),
                seed(2, 15_000_000 * COIN, &[OP_TRUE]),
            ],
            ..Setup::default()
        },
        |p, g| vec![block_on(p, g, vec![spend(&[seed_op(1)], 1), spend(&[seed_op(2)], 1)])],
        reject(BlockValidationResult::Consensus, "bad-txns-accumulated-fee-outofrange"),
    );
}

#[test]
fn a_relative_lock_not_yet_met_is_nonfinal() {
    assert_rejected(
        Setup {
            seeds: vec![seed(1, 50 * COIN, &[OP_TRUE])],
            ..Setup::default()
        },
        |p, g| {
            // Version 2, BIP68 on: five blocks after a coin of height 0, in block 1.
            let t = tx(2, 0, vec![input(seed_op(1), Vec::new(), 5)], vec![out(COIN, &[OP_TRUE])]);
            vec![block_on(p, g, vec![t])]
        },
        reject(BlockValidationResult::Consensus, "bad-txns-nonfinal"),
    );
}

/// Each input's relative lock counts from its own coin's height (`prevheights`, v:2726-2734):
/// in block 2, a seed of height 0 and a coin of height 1, the older first, each locked for two
/// blocks. The seed's lock is met (0 + 2 - 1 < 2) and the younger coin's is not (1 + 2 - 1 = 2).
#[test]
fn each_relative_lock_counts_from_its_own_coin() {
    assert_rejected(
        Setup {
            seeds: vec![seed(1, 50 * COIN, &[OP_TRUE]), seed(2, 50 * COIN, &[OP_TRUE])],
            ..Setup::default()
        },
        |p, g| {
            let young = spend(&[seed_op(2)], COIN);
            let b1 = block_on(p, g, vec![Arc::clone(&young)]);
            let inputs =
                vec![input(seed_op(1), Vec::new(), 2), input(op(&young, 0), Vec::new(), 2)];
            let b2 = block_on(p, &b1, vec![tx(2, 0, inputs, vec![out(COIN, &[OP_TRUE])])]);
            vec![b1, b2]
        },
        reject(BlockValidationResult::Consensus, "bad-txns-nonfinal"),
    );
}

#[test]
fn p2sh_sigops_beyond_the_block_limit_are_too_many() {
    // OP_HASH160 <20 bytes> OP_EQUAL; each redeem script is 520 OP_CHECKMULTISIGs (20 each).
    let mut p2sh = vec![0xa9, 20];
    p2sh.extend([0; 20]);
    p2sh.push(0x87);
    let mut sig = vec![0x4d, 0x08, 0x02];
    sig.extend([0xae; 520]);
    let seeds = (1..=4).map(|n| seed(n, COIN, &p2sh)).collect();
    assert_rejected(
        Setup {
            seeds,
            ..Setup::default()
        },
        move |p, g| {
            let inputs = (1..=4).map(|n| input(seed_op(n), sig.clone(), u32::MAX)).collect();
            vec![block_on(p, g, vec![tx(1, 0, inputs, vec![out(COIN, &[OP_TRUE])])])]
        },
        reject(BlockValidationResult::Consensus, "bad-blk-sigops"),
    );
}

#[test]
fn a_failing_script_is_mandatory_flag_failed() {
    assert_rejected(
        Setup {
            seeds: vec![seed(1, 50 * COIN, &[0x00])],
            ..Setup::default()
        },
        |p, g| vec![block_on(p, g, vec![spend(&[seed_op(1)], COIN)])],
        reject(
            BlockValidationResult::Consensus,
            "mandatory-script-verify-flag-failed (Script evaluated without error but finished \
             with a false/empty top stack element)",
        ),
    );
}

#[test]
fn an_overpaying_coinbase_is_bad_cb_amount() {
    assert_rejected(
        Setup::default(),
        |p, g| vec![with_coinbase(p, g, coinbase_tx(1, 1, 0, 1_000_000 * COIN), vec![])],
        reject(BlockValidationResult::Unset, "bad-cb-amount"),
    );
}

/// D54: below DIP3 a raw-type-5 coinbase of version 1 or 2 has no payload Core can read.
#[test]
fn a_classical_raw_type_5_coinbase_is_bad_cbtx_payload() {
    for version in [1, 2] {
        assert_rejected(
            Setup::default(),
            |p, g| vec![with_coinbase(p, g, coinbase_tx(1, version, 5, COIN), vec![])],
            reject(BlockValidationResult::Consensus, "bad-cbtx-payload"),
        );
    }
}

/// D32: a special transaction below DIP3.
#[test]
fn a_special_transaction_below_dip3_is_refused_by_the_gate() {
    let w = world(Setup {
        seeds: vec![seed(1, 50 * COIN, &[OP_TRUE])],
        ..Setup::default()
    });
    let pro = tx(3, 1, vec![input(seed_op(1), Vec::new(), u32::MAX)], vec![out(COIN, &[OP_TRUE])]);
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![pro.clone()]);
    let err = last_invalid(&w, &[b1]);
    assert_eq!(err.result(), BlockValidationResult::Consensus);
    assert_eq!(err.reject_reason(), "bad-tx-type-dip3-inactive");
    assert_eq!(
        err.debug_message(),
        format!("Special Transaction check failed (tx hash {}) ", pro.txid())
    );
}

/// The capability of the readiness refusal the tests' H4 returns (CH7 D2).
const NOT_READY: &str = "special-tx checks (EV1)";

/// The connector's "failed" line for H4's verdicts, on the block `m`.
fn h4_failed_lines(m: &Mined) -> Vec<String> {
    super::support::lines_with(&format!("ProcessSpecialTxsInBlock for block {} failed", m.hash()))
}

/// CH7 D2 on the tip path: H4's `NotReady` is G1's refusal, reported once (the node stops, the
/// block keeps `HAVE_DATA` and gets no failure mark), with the capability in D69's text and no
/// "failed" line, as Core reached no verdict. H4's `Invalid` keeps that line.
#[test]
fn an_h4_readiness_refusal_is_refused_on_the_tip_path_without_a_failed_line() {
    super::support::install_accept_log();
    // Blocks no other test builds: the lines are read by hash, process-wide.
    let p = regtest();
    let b1 = with_coinbase(&p, &Mined::genesis(&p), coinbase_tx(1, 1, 0, 7 * COIN + 501), vec![]);
    let msg = refusal(&p, &b1, NOT_READY);
    let refused = b1.clone();
    assert_system_error(
        Setup {
            steps: StepsCfg {
                not_ready_in_h4: Some(NOT_READY),
                ..StepsCfg::default()
            },
            ..Setup::default()
        },
        move |_, _| vec![refused],
        &msg,
    );
    assert_eq!(h4_failed_lines(&b1), Vec::<String>::new());

    let w = world(Setup {
        seeds: vec![seed(1, 50 * COIN, &[OP_TRUE])],
        ..Setup::default()
    });
    let pro =
        tx(3, 1, vec![input(seed_op(1), Vec::new(), u32::MAX)], vec![out(COIN + 501, &[OP_TRUE])]);
    let invalid = block_on(&w.rig.p, &w.rig.genesis, vec![pro]);
    let err = last_invalid(&w, std::slice::from_ref(&invalid));
    assert_eq!(err.reject_reason(), "bad-tx-type-dip3-inactive");
    assert_eq!(h4_failed_lines(&invalid).len(), 1);
}

#[test]
fn a_conflicting_chainlock_is_bad_chainlock() {
    assert_rejected(
        Setup {
            helper: Scripted {
                cl_conflict_at: Some(1),
                ..Scripted::default()
            },
            ..Setup::default()
        },
        |p, g| vec![block_on(p, g, vec![])],
        reject(BlockValidationResult::ChainLock, "bad-chainlock"),
    );
}

/// Lock A on a transaction spending seed 1; lock B on one spending seed 2.
fn lock_a() -> IsLock {
    IsLock::on(7, &[seed_op(1)])
}

fn lock_b() -> IsLock {
    IsLock::on(9, &[seed_op(2)])
}

fn removal(l: &IsLock) -> Effect {
    Effect::RemoveConflictingIsLock {
        lock_hash: l.r.hash,
    }
}

/// Seeds 1 and 2, `locks` in the InstantSend store, outside IBD, with or without a ChainLock.
fn is_setup(locks: Vec<IsLock>, cl_has: bool) -> Setup {
    Setup {
        seeds: vec![seed(1, 50 * COIN, &[OP_TRUE]), seed(2, 50 * COIN, &[OP_TRUE])],
        is_store: locks,
        helper: Scripted {
            ibd: Some(false),
            cl_has,
            ..Scripted::default()
        },
        ..Setup::default()
    }
}

/// The helper's IS calls of the last attempt, in order.
fn is_calls(w: &World) -> Vec<Call> {
    let seen = w.seen.lock().expect("seen");
    seen.last_attempt().iter().copied().filter(Call::is_helper).collect()
}

/// The bracket's calls of the last attempt, in order.
fn bracket(w: &World) -> Vec<Call> {
    w.seen.lock().expect("seen").last_bracket()
}

const CONNECTED: [Call; 6] = [
    Call::BeginAttempt,
    Call::BeginTx,
    Call::SetBestBlock,
    Call::FinishAttempt,
    Call::EndAttempt,
    Call::CommitTx,
];

const NOT_CONNECTED: [Call; 5] =
    [Call::BeginAttempt, Call::BeginTx, Call::FinishAttempt, Call::EndAttempt, Call::RollbackTx];

/// CH3 §3.3 `[A, null]`: a ChainLocked block whose transaction conflicts with lock A. The queries
/// are Core's (`v:2801-2819`): `ibd`, `cl_has`, then `is_conflict` per loop condition; one removal
/// effect, applied with the attempt; the block connects. In IBD only `ibd` is asked.
#[test]
fn a_chainlock_removes_a_conflicting_islock() {
    let a = lock_a();
    for ibd in [false, true] {
        let mut setup = is_setup(vec![a.clone()], true);
        setup.helper.ibd = Some(ibd);
        let w = world(setup);
        let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![spend(&[seed_op(1)], COIN)]);
        w.rig.accept(&b1);
        let (calls, effects) = if ibd {
            (vec![Call::Ibd], vec![])
        } else {
            let calls = vec![
                Call::Ibd,
                Call::ClHas,
                Call::IsConflict(Some(a.r.hash)),
                Call::Remove(a.r.hash),
                Call::IsConflict(None),
            ];
            (calls, vec![removal(&a)])
        };
        assert_eq!(is_calls(&w), calls, "ibd {ibd}");
        let seen = w.seen.lock().expect("seen");
        assert_eq!(seen.effects, effects, "ibd {ibd}");
        assert_eq!(seen.is_store.len(), usize::from(ibd), "ibd {ibd}");
        assert_eq!(seen.evo_best, Some(b1.hash()));
    }
}

/// CH3 §3.3 `[A, B, null]`: two locks, one per input, removed in Core's order (the inputs').
#[test]
fn a_chainlock_removes_one_islock_per_input_in_order() {
    let (a, b) = (lock_a(), lock_b());
    let w = world(is_setup(vec![b.clone(), a.clone()], true));
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![spend(&[seed_op(1), seed_op(2)], COIN)]);
    w.rig.accept(&b1);
    assert_eq!(
        is_calls(&w),
        [
            Call::Ibd,
            Call::ClHas,
            Call::IsConflict(Some(a.r.hash)),
            Call::Remove(a.r.hash),
            Call::IsConflict(Some(b.r.hash)),
            Call::Remove(b.r.hash),
            Call::IsConflict(None),
        ]
    );
    let seen = w.seen.lock().expect("seen");
    assert_eq!(seen.effects, [removal(&a), removal(&b)]);
    assert!(seen.is_store.is_empty());
}

/// CH3 §3.3 "No ChainLock": the `[A, null]` block without a ChainLock is `CHAINLOCK`
/// `conflict-tx-lock` at the first conflict; nothing is removed.
#[test]
fn an_islock_conflict_without_a_chainlock_is_conflict_tx_lock() {
    let a = lock_a();
    let w = world(is_setup(vec![a.clone()], false));
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![spend(&[seed_op(1)], COIN)]);
    let err = last_invalid(&w, &[b1]);
    assert_eq!(
        (err.result(), err.reject_reason()),
        (BlockValidationResult::ChainLock, "conflict-tx-lock")
    );
    assert_eq!(is_calls(&w), [Call::Ibd, Call::ClHas, Call::IsConflict(Some(a.r.hash))]);
    assert_eq!(bracket(&w), NOT_CONNECTED);
    let seen = w.seen.lock().expect("seen");
    assert!(seen.effects.is_empty());
    assert_eq!(seen.is_store.len(), 1);
}

/// CH3 §3.3 "A failed attempt after a removal": `[A, null]`, then the block fails at step 19
/// (`bad-cb-amount`). The removal is still applied (Core's removal is synchronous), nothing else
/// is committed, the removed view ends with the attempt (D76), and the next attempt's
/// `is_conflict` no longer returns A.
#[test]
fn a_removal_survives_its_failed_attempt() {
    let a = lock_a();
    let w = world(is_setup(vec![a.clone()], true));
    let p = w.rig.p.clone();
    let bad = with_coinbase(
        &p,
        &w.rig.genesis,
        coinbase_tx(1, 1, 0, 1_000_000 * COIN),
        vec![spend(&[seed_op(1)], COIN)],
    );
    let err = last_invalid(&w, std::slice::from_ref(&bad));
    assert_eq!(err.reject_reason(), "bad-cb-amount");
    assert_eq!(
        is_calls(&w),
        [
            Call::Ibd,
            Call::ClHas,
            Call::IsConflict(Some(a.r.hash)),
            Call::Remove(a.r.hash),
            Call::IsConflict(None),
        ]
    );
    assert_eq!(bracket(&w), NOT_CONNECTED);
    {
        let seen = w.seen.lock().expect("seen");
        assert_eq!(seen.effects, [removal(&a)]);
        assert!(seen.is_store.is_empty());
        assert_eq!(seen.evo_best, None);
    }
    assert_eq!(coins_at(&w.coins), Some(w.rig.genesis.hash()));

    let good = block_on(&p, &w.rig.genesis, vec![spend(&[seed_op(1)], COIN)]);
    w.rig.accept(&good);
    assert_eq!(is_calls(&w), [Call::Ibd, Call::ClHas, Call::IsConflict(None)]);
    assert_eq!(bracket(&w), CONNECTED);
    assert_eq!(w.seen.lock().expect("seen").effects, [removal(&a)]);
}

/// CH3 §3.3 "Chained children" (D35): T is locked by L; C spends `T:0` and seed 2 and is locked
/// by Lc, a chained child of L. A ChainLocked block holds X, which spends T's input (seed 1),
/// and Z, which spends seed 2. X removes L, and the closure takes Lc: Z's `is_conflict` is null,
/// one removal effect, as in Core (`instantsend/instantsend.cpp:339-350`).
#[test]
fn a_removal_takes_the_chained_children() {
    let l = lock_a();
    let t0 = OutPoint::new(l.r.txid, 0);
    let lc = IsLock::on(9, &[t0, seed_op(2)]);
    let w = world(is_setup(vec![lc.clone(), l.clone()], true));
    let x = spend(&[seed_op(1)], COIN);
    let z = spend(&[seed_op(2)], 2 * COIN);
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![x, z]);
    w.rig.accept(&b1);
    assert_eq!(
        is_calls(&w),
        [
            Call::Ibd,
            Call::ClHas,
            Call::IsConflict(Some(l.r.hash)),
            Call::Remove(l.r.hash),
            Call::IsConflict(None),
            Call::IsConflict(None),
        ]
    );
    let seen = w.seen.lock().expect("seen");
    assert_eq!(seen.effects, [removal(&l)]);
    assert!(seen.is_store.is_empty(), "the closure applied");
}

/// CH3 §3.2's bracket on each outcome but a failed step (D33, D75, D76): `finish_attempt`, then
/// `end_attempt`, on every path; the evo transaction is committed on `Connected` only, after the
/// coins, and rolled back on `Invalid` and on a system error.
#[test]
fn the_bracket_commits_a_connected_block_only() {
    let w = world(Setup::default());
    let p = w.rig.p.clone();
    let b1 = block_on(&p, &w.rig.genesis, vec![]);
    w.rig.accept(&b1);
    assert_eq!(bracket(&w), CONNECTED);
    assert_eq!(w.seen.lock().expect("seen").evo_best, Some(b1.hash()));

    let bad = with_coinbase(&p, &b1, coinbase_tx(2, 1, 0, 1_000_000 * COIN), vec![]);
    last_invalid(&w, &[bad]);
    assert_eq!(bracket(&w), NOT_CONNECTED);
    assert_eq!(w.seen.lock().expect("seen").evo_best, Some(b1.hash()));
    drop(w);

    // A readiness refusal: a reported system error.
    let mut w = world(Setup {
        extra: Some(Deployment::Buried(BuriedDeployment::Dip0003)),
        ..Setup::default()
    });
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
    w.rig.submit_stopping(&b1, "");
    w.rig.stop();
    assert_eq!(w.sink.messages().len(), 1, "one report");
    assert_eq!(bracket(&w), NOT_CONNECTED);
    assert_eq!(w.seen.lock().expect("seen").evo_best, None);
}

/// CH3 §3.2's fault injection at the evo transaction (D75): a failed `begin_tx` is the attempt's
/// `Fatal`, and the attempt still finishes and rolls back; a failed `commit_tx` (after the coins'
/// commit, Core's order at `v:3446-3448`) rolls back too. Each stops V with one report, the tip
/// at the parent and no evo best block committed.
#[test]
fn a_failed_evo_transaction_step_rolls_back() {
    for (step, calls) in [
        (TxStep::Begin, &NOT_CONNECTED[..]),
        (
            TxStep::Commit,
            &[
                Call::BeginAttempt,
                Call::BeginTx,
                Call::SetBestBlock,
                Call::FinishAttempt,
                Call::EndAttempt,
                Call::CommitTx,
                Call::RollbackTx,
            ][..],
        ),
    ] {
        let mut w = world(Setup {
            steps: StepsCfg {
                fail_tx: Some(step),
                ..StepsCfg::default()
            },
            ..Setup::default()
        });
        let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
        w.rig.submit_stopping(&b1, &format!("{step:?}"));
        let exit = w.rig.stop();
        assert_eq!(fatal_text(&exit), Some(TX_FAILED), "{step:?}");
        assert_eq!(w.sink.messages(), [TX_FAILED], "{step:?}: one report");
        assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(w.rig.genesis.hash()));
        assert_eq!(bracket(&w), calls, "{step:?}");
        assert_eq!(w.seen.lock().expect("seen").evo_best, None, "{step:?}");
        // Core flushes the coins before it commits the evo transaction (v:3446-3448).
        let coins = if step == TxStep::Begin {
            &w.rig.genesis
        } else {
            &b1
        };
        assert_eq!(coins_at(&w.coins), Some(coins.hash()), "{step:?}");
    }
}

/// D74 with CH3 §5.2's order: the Active coins' lock is taken after `begin_attempt` (L-cell is
/// never held while taking L3), and a held one is a value inside the bracket: the attempt
/// finishes, one report, and the unwind guard never fires. Without a view no evo transaction is
/// begun, so none is rolled back (Opus a3 r1 F4).
#[test]
fn a_held_coins_lock_fails_inside_the_bracket() {
    let mut w = world(Setup::default());
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
    let held = w.coins.lock().expect("coins");
    w.rig.submit_stopping(&b1, "");
    drop(held);
    let exit = w.rig.stop();
    let msg = "the coins cell is held outside V's leaf rule";
    assert_eq!(fatal_text(&exit), Some(msg));
    assert_eq!(w.sink.messages(), [msg], "one report");
    assert_eq!(bracket(&w), [Call::BeginAttempt, Call::FinishAttempt, Call::EndAttempt]);
    let latch = w.seen.lock().expect("seen").latch.clone();
    let text = latch.tripped().map(|f| f.message.clone()).unwrap_or_default();
    assert!(!text.contains("unwound"), "{text}");
}

/// D75: when the attempt's log fails to apply after a verdict, a reported system error keeps its
/// kind (one report, the refusal's; `BlockChecked` with it, v:3431-3432), and after `Invalid` the
/// failed application is the `Fatal` (one report; the verdict does not stand: no `FAILED_VALID`,
/// no `BlockChecked`). Each attempt rolls back.
#[test]
fn a_failed_application_after_a_verdict_is_reported_once() {
    let a = lock_a();
    for refused in [true, false] {
        let mut setup = is_setup(vec![a.clone()], true);
        setup.steps.fail_apply = Some(0);
        if refused {
            setup.helper.interim_at = Some(1);
        }
        let mut w = world(setup);
        let p = w.rig.p.clone();
        let value = if refused {
            COIN
        } else {
            1_000_000 * COIN
        };
        let b1 = with_coinbase(
            &p,
            &w.rig.genesis,
            coinbase_tx(1, 1, 0, value),
            vec![spend(&[seed_op(1)], COIN)],
        );
        let msg = if refused {
            g2_refusal_text(&p, &at(&b1), INTERIM_REFUSAL)
        } else {
            APPLY_FAILED.to_owned()
        };
        w.rig.submit_stopping(&b1, &format!("refused {refused}"));
        let id = w.rig.id(&b1);
        let exit = w.rig.stop();
        assert_eq!(fatal_text(&exit), Some(msg.as_str()));
        assert_eq!(w.sink.messages(), std::slice::from_ref(&msg), "refused {refused}: one report");
        let want = if refused {
            vec![(b1.hash(), BlockState::Error(msg))]
        } else {
            vec![]
        };
        assert_eq!(checked(&w.rig), want, "refused {refused}");
        let status = exit.chain.reader().entry(id).expect("indexed").status;
        assert!(!status.has(Status::FAILED_VALID), "refused {refused}: {status:?}");
        assert_eq!(bracket(&w), NOT_CONNECTED, "refused {refused}");
        assert!(w.seen.lock().expect("seen").effects.is_empty(), "refused {refused}");
    }
}

/// `-dip3params` with enforcement below activation (at V20's default height, the highest
/// activation Core's assert allows: chainparams.cpp:964): from block 1 Core asks the masternode
/// list (H5) while DIP0003, and so G1, is off.
fn early_enforcement() -> Arc<ChainSetup> {
    Arc::new(
        ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
            .arg(ChainOption::Dip3Params, "432:1")
            .build()
            .expect("regtest"),
    )
}

#[test]
fn an_unpaid_masternode_is_bad_cb_payee() {
    assert_rejected(
        Setup {
            params: Some(early_enforcement()),
            steps: StepsCfg {
                complete: true,
                mn_payee: false,
                ..StepsCfg::default()
            },
            ..Setup::default()
        },
        |p, g| vec![block_on(p, g, vec![])],
        reject(BlockValidationResult::Unset, "bad-cb-payee"),
    );
}

/// The refusal text of the block `m` would be, on `p`.
fn refusal(p: &ChainSetup, m: &Mined, capability: &str) -> String {
    refusal_text(p, &at(m), capability)
}

/// D63: without complete evo hooks the masternode half is refused, not guessed.
#[test]
fn the_masternode_payee_without_evo_is_refused() {
    let p = early_enforcement();
    let b1 = block_on(&p, &Mined::genesis(&p), vec![]);
    let msg = refusal(&p, &b1, MN_PAYEE_CAPABILITY);
    assert_system_error(
        Setup {
            params: Some(p),
            ..Setup::default()
        },
        move |_, _| vec![b1],
        &msg,
    );
}

/// CH7 D58: `CoreConnector` hands V its hooks' scheme, which V publishes as `ChainView.bls` from
/// its first view on.
#[test]
fn the_view_publishes_the_core_connectors_hooks_scheme() {
    let w = world(Setup::default());
    let b1 = block_on(&w.rig.p, &w.rig.genesis, Vec::new());
    w.rig.accept(&b1);
    let view = w.rig.v.view();
    assert_eq!((view.bls, view.snap.bls), (BlsCtx::LEGACY, BlsCtx::BASIC));
    w.rig.assert_no_abort();
}

/// G1 at step 5a for a live deployment (CH3 §2.17); below DIP0003 a special-version coinbase is
/// the hooks' to judge, and `NoEvo`'s H4 refuses it with the same text G1 gave (CH7 D57).
#[test]
fn g1_refuses_an_evo_block_without_complete_hooks() {
    let p = regtest();
    let g = Mined::genesis(&p);
    let b1 = block_on(&p, &g, vec![]);
    let cbtx = with_coinbase(&p, &g, coinbase_tx(1, 3, 5, COIN), vec![]);
    for (extra, block, capability) in [
        (Some(Deployment::Buried(BuriedDeployment::Dip0003)), b1, g1_capability("DIP0003")),
        (None, cbtx, SPECIAL_VERSION_CBTX.to_owned()),
    ] {
        let msg = refusal(&p, &block, &capability);
        assert_system_error(
            Setup {
                extra,
                ..Setup::default()
            },
            move |_, _| vec![block],
            &msg,
        );
    }
    assert_eq!(SPECIAL_VERSION_CBTX, g1_capability("a special-version coinbase"));
}

/// G2 at step 19: the interim helper's refusal.
#[test]
fn g2_refuses_where_the_helper_cannot_judge() {
    let p = regtest();
    let b1 = block_on(&p, &Mined::genesis(&p), vec![]);
    let msg = g2_refusal_text(&p, &at(&b1), INTERIM_REFUSAL);
    assert!(
        msg.ends_with("(ALLOWED-DIFFS D18); it connects once the tip is older than -maxtipage")
    );
    assert_system_error(
        Setup {
            helper: Scripted {
                interim_at: Some(1),
                ..Scripted::default()
            },
            ..Setup::default()
        },
        move |_, _| vec![b1],
        &msg,
    );
}

/// G2 through the production helper (CH3 §2.17, D31): `NodeHelper` refuses a governance height
/// no ChainLock covers on a live network out of IBD; the same block connects under the declared
/// regime (regtest's) and in IBD.
#[test]
fn g2_through_node_helper_refuses_only_live_and_out_of_ibd() {
    let mut p = (*regtest()).clone();
    // Height 1 inside an old-budget payment window (`masternode/payments.cpp:262-295`).
    p.consensus.budget_payments_start_block = 1u32.into();
    let p = Arc::new(p);
    assert!(is_governance_height(&p, 1));
    let g = Mined::genesis(&p);
    let b1 = block_on(&p, &g, vec![]);
    let msg = g2_refusal_text(&p, &at(&b1), INTERIM_REFUSAL);
    // Ten minutes after genesis, its tip is younger than -maxtipage: out of IBD.
    let setup = |guard, after_genesis| Setup {
        params: Some(p.clone()),
        node_helper: true,
        guard: Some(guard),
        after_genesis,
        ..Setup::default()
    };
    let live = b1.clone();
    assert_system_error(setup(GuardScope::Live, Some(600)), move |_, _| vec![live], &msg);
    for (guard, after_genesis) in [(GuardScope::Declared, Some(600)), (GuardScope::Live, None)] {
        let w = world(setup(guard, after_genesis));
        w.rig.accept(&b1);
        w.rig.assert_no_abort();
    }
}

/// The chain of the assumevalid rows (CH3 §2.8, `v:2542-2571`): b1 spends a seeded coin whose
/// script fails, b2 and b3 follow it, and `side` branches from genesis, four headers long. The
/// target spacing is raised so that two headers ahead are over two weeks of proof-equivalent time:
/// condition 5 (`v:2568`) lets b1 skip under b3 and under `side`'s last header.
struct AvChain {
    p: Arc<ChainSetup>,
    b: [Mined; 3],
    side: [Mined; 4],
}

impl AvChain {
    fn new() -> AvChain {
        let mut p = (*regtest()).clone();
        p.consensus.pow_target_spacing = 1_000_000;
        p.consensus.pow_target_timespan = 10_000_000;
        let p = Arc::new(p);
        let g = Mined::genesis(&p);
        let b1 = block_on(&p, &g, vec![spend(&[seed_op(1)], COIN)]);
        let b2 = block_on(&p, &b1, vec![]);
        let b3 = block_on(&p, &b2, vec![]);
        let s1 = block_on(&p, &g, vec![]);
        let s2 = block_on(&p, &s1, vec![]);
        let s3 = block_on(&p, &s2, vec![]);
        let s4 = block_on(&p, &s3, vec![]);
        AvChain {
            p,
            b: [b1, b2, b3],
            side: [s1, s2, s3, s4],
        }
    }

    /// b1's verdict with `-assumevalid` at `av` and `-minimumchainwork` at `min_work`, after the
    /// headers of b1–b3, then of `side`'s first `side_len`; and the hash and work of the best
    /// header its connect sees.
    fn verdict(
        &self,
        av: BlockHash,
        side_len: usize,
        min_work: Option<Work>,
    ) -> (BlockState, BlockHash, Work) {
        self.verdict_with(0, av, side_len, min_work)
    }

    /// [`AvChain::verdict`] over a queue with `script_threads`.
    fn verdict_with(
        &self,
        script_threads: usize,
        av: BlockHash,
        side_len: usize,
        min_work: Option<Work>,
    ) -> (BlockState, BlockHash, Work) {
        let (_, state, best, work) = self.world_with(script_threads, None, av, side_len, min_work);
        (state, best, work)
    }

    /// [`AvChain::verdict_with`] over `caches` (default new ones), with its world.
    fn world_with(
        &self,
        script_threads: usize,
        caches: Option<Arc<ScriptCaches>>,
        av: BlockHash,
        side_len: usize,
        min_work: Option<Work>,
    ) -> (World, BlockState, BlockHash, Work) {
        let w = world(Setup {
            params: Some(self.p.clone()),
            seeds: vec![seed(1, 50 * COIN, &[0x00])],
            assume_valid: Some(av),
            min_chain_work: min_work,
            script_threads,
            caches,
            ..Setup::default()
        });
        w.rig.headers(&self.b);
        w.rig.headers(&self.side[..side_len]);
        // Read before b1's verdict: an invalid b1 walks a best header it leads to back to the
        // tip (`InvalidChainFound`).
        let best = w.rig.v.query(|q| q.chain.best_header()).recv().expect("served");
        let best = w.rig.v.reader().entry(best).expect("indexed");
        let state = last_state(&w, &self.b[..1]);
        (w, state, best.hash, best.chain_work)
    }
}

/// b1's verdict when its scripts are checked.
fn av_checked() -> BlockState {
    reject(
        BlockValidationResult::Consensus,
        "mandatory-script-verify-flag-failed (Script evaluated without error but finished with a \
         false/empty top stack element)",
    )
}

/// CH3 §2.8 through the connector (`v:2542-2571`): `-assumevalid` naming a descendant of the block
/// on the best header chain, more than two weeks of proof-equivalent time ahead, skips its
/// scripts, so a failing one connects; named on another branch (condition 2, `v:2551`), the
/// scripts are checked.
#[test]
fn assumevalid_skips_scripts_on_its_chain_only() {
    let c = AvChain::new();
    assert_verdict(&c.verdict(c.b[2].hash(), 1, None).0, &BlockState::Valid);
    assert_verdict(&c.verdict(c.side[0].hash(), 1, None).0, &av_checked());
}

/// Condition 3 (`v:2552`, Opus a3 r1 F1): `-assumevalid` at b3, which b1 is an ancestor of
/// (conditions 1 and 2), but `side`'s headers pass b3's work, so the best header does not lead to
/// b1: its scripts are checked. Measured from `side`'s last header, three headers' work past b1
/// (over two weeks), condition 5 alone would skip them.
#[test]
fn assumevalid_checks_scripts_off_the_best_header_chain() {
    let c = AvChain::new();
    let (state, best, _) = c.verdict(c.b[2].hash(), 4, None);
    assert_eq!(best, c.side[3].hash(), "the best header is on side");
    assert_verdict(&state, &av_checked());
}

/// Condition 4 (`v:2553`, Opus a3 r1 F1): `-assumevalid` at b3, the best header, two headers'
/// work (over two weeks) ahead of b1 (conditions 1, 2, 3 and 5). With `-minimumchainwork` above
/// b3's work, b1's scripts are checked; equal to it (Core's `>=`), they are skipped.
#[test]
fn assumevalid_skips_scripts_from_the_minimum_chain_work() {
    let c = AvChain::new();
    let (_, best, b3_work) = c.verdict(c.b[2].hash(), 1, None);
    assert_eq!(best, c.b[2].hash(), "b3 is the best header");
    let (above, ..) = c.verdict(c.b[2].hash(), 1, Some(b3_work + Work::from_u64(1)));
    assert_verdict(&above, &av_checked());
    let (equal, ..) = c.verdict(c.b[2].hash(), 1, Some(b3_work));
    assert_verdict(&equal, &BlockState::Valid);
}

/// v:2525-2531: the evo store behind the parent is an `AbortNode`.
#[test]
fn an_evo_store_behind_the_parent_is_evodb_inconsistency() {
    assert_system_error(
        Setup {
            steps: StepsCfg {
                complete: true,
                verify_best: false,
                ..StepsCfg::default()
            },
            extra: Some(Deployment::Buried(BuriedDeployment::Dip0003)),
            ..Setup::default()
        },
        |p, g| vec![block_on(p, g, vec![])],
        EVODB_INCONSISTENCY,
    );
}

/// v:2505-2514: a stored block that reads back mutated is an `AbortNode`, not a verdict.
#[test]
fn a_mutated_stored_block_is_corrupt() {
    let p = regtest();
    let g = Mined::genesis(&p);
    let t = spend(&[seed_op(1)], COIN);
    let b1 = block_on(&p, &g, vec![t.clone()]);
    // The last transaction twice: the same merkle root (CVE-2012-2459), so the same header.
    let mut txs = b1.block.txs().to_vec();
    txs.push(t);
    let read = Arc::new(Block::new(*b1.block.header(), txs));
    assert_eq!(read.hash(), b1.hash());
    assert_system_error(
        Setup {
            seeds: vec![seed(1, 50 * COIN, &[OP_TRUE])],
            substitute: Some((b1.hash(), read)),
            ..Setup::default()
        },
        move |_, _| vec![b1],
        CORRUPT_BLOCK,
    );
}

/// v:2617-2622 ("bad-sb-start") at its predicate: mainnet names its superblock start block, at a
/// height no rig mines; regtest names none. The test below drives the reject.
#[test]
fn only_the_named_block_starts_mainnet_superblocks() {
    let main = ChainParamsBuilder::new(Network::Main, Release::V24_0).build().expect("main");
    let (height, named) = main.consensus.superblock_start;
    let other = BlockHash::from([1; 32]);
    let named = BlockHash::from(*named.as_bytes());
    let h = height.to_u32();
    assert!(wrong_superblock_start(&main, &block_at(other, h)));
    assert!(!wrong_superblock_start(&main, &block_at(named, h)));
    assert!(!wrong_superblock_start(&main, &block_at(other, h + 1)));
    // `nSuperblockStartHash` is `uint256()` on regtest: nothing is checked.
    let reg = regtest();
    let reg_start = reg.consensus.superblock_start.0.to_u32();
    assert!(!wrong_superblock_start(&reg, &block_at(other, reg_start)));
}

/// v:2617-2622 through the connector: regtest moved to name a superblock start block, at
/// height 1, that no mined block is.
#[test]
fn a_block_other_than_the_named_superblock_start_is_bad_sb_start() {
    let mut p = (*regtest()).clone();
    p.consensus.superblock_start = (1u32.into(), Hash256::from([7; 32]));
    assert_rejected(
        Setup {
            params: Some(Arc::new(p)),
            ..Setup::default()
        },
        |p, g| vec![block_on(p, g, vec![])],
        reject(BlockValidationResult::Consensus, "bad-sb-start"),
    );
}

/// Regtest with DIP0003 active and enforced from block 20 (`-dip3params=20:20`).
fn dip3_at_20() -> Arc<ChainSetup> {
    Arc::new(
        ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
            .arg(ChainOption::Dip3Params, "20:20")
            .build()
            .expect("regtest"),
    )
}

/// Blocks 1 to `n` on `genesis`, each with only its coinbase: from block 20 a version-1 CbTx
/// (`nVersion` 3, raw `nType` 5; `evo/cbtx.h`), which `ContextualCheckBlock` requires once DIP0003
/// is active ("bad-cb-type").
fn empty_chain(p: &ChainSetup, genesis: &Mined, n: u32) -> Vec<Mined> {
    let mut blocks: Vec<Mined> = Vec::new();
    for _ in 0..n {
        let parent = blocks.last().unwrap_or(genesis);
        let h = parent.height + 1;
        let next = if h < 20 {
            block_on(p, parent, vec![])
        } else {
            let mut cb = coinbase_tx(h, 3, 5, COIN).to_mutable();
            cb = dash_primitives::Transaction::new(
                cb.version(),
                cb.tx_type(),
                cb.inputs().clone(),
                cb.outputs().clone(),
                cb.lock_time(),
                [&1u16.to_le_bytes()[..], &h.to_le_bytes(), &[0; 32]].concat(),
            );
            with_coinbase(p, parent, Arc::new(Tx::new(cb)), vec![])
        };
        blocks.push(next);
    }
    blocks
}

/// The startup check over a stopped node's chain and block files, as a build with hooks `H` runs
/// it.
fn startup<H: EvoHooks>(
    p: &Arc<ChainSetup>,
    exit: &VExit,
    stores: &Stores,
    tip: BlockId,
) -> Result<(), String> {
    check_startup_readiness::<H>(p, &exit.chain, &stores.blocks, &[tip]).map_err(|e| e.to_string())
}

/// CH3 §2.17 "G1 boundary" and "G1 restart": an incomplete build connects blocks 1–19 and
/// refuses block 20 with one report; reopened on the same stores, its startup check passes at
/// tip 19 and init's ABC refuses block 20 again, the same way.
#[test]
fn g1_refuses_the_dip3_block_and_again_after_a_restart() {
    let p = dip3_at_20();
    let mut w = world(Setup {
        params: Some(p.clone()),
        ..Setup::default()
    });
    let blocks = empty_chain(&p, &w.rig.genesis, 20);
    for m in &blocks[..19] {
        w.rig.accept(m);
    }
    let (b19, b20) = (&blocks[18], &blocks[19]);
    let msg = refusal(&p, b20, &g1_capability("DIP0003"));
    w.rig.submit_stopping(b20, "");
    let (tip, id20) = (w.rig.id(b19), w.rig.id(b20));
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), Some(msg.as_str()));
    assert_eq!(w.sink.messages(), std::slice::from_ref(&msg), "one report");
    let status = exit.chain.reader().entry(id20).expect("indexed").status;
    assert!(status.has(Status::HAVE_DATA), "{status:?}");
    assert!(!status.has(Status::HAVE_UNDO) && !status.has(Status::FAILED_VALID), "{status:?}");
    assert_eq!(coins_at(&w.coins), Some(b19.hash()));

    assert_eq!(startup::<TestEvo<false>>(&p, &exit, &w.stores, tip), Ok(()));
    let (mut cfg, _, sink, _) = node(Setup {
        params: Some(p.clone()),
        stores: Some(w.stores.clone()),
        ..Setup::default()
    });
    cfg.aborts_before_init = true;
    let mut rig =
        Rig::restart_with(exit, Box::new(w.stores.blocks.clone()), tip, cfg).expect("V starts");
    let exit = rig.stop();
    assert_eq!(sink.messages(), [msg], "refused again");
    let status = exit.chain.reader().entry(id20).expect("indexed").status;
    assert!(!status.has(Status::HAVE_UNDO) && !status.has(Status::FAILED_VALID), "{status:?}");
    assert_eq!(coins_at(&w.coins), Some(b19.hash()));
}

/// CH3 §2.17 "G1 startup refusal": a chainstate a complete build extended past DIP0003 does not
/// start under an incomplete one (InitError with the refusal text); a complete build starts.
#[test]
fn g1_refuses_to_start_on_a_chainstate_past_what_the_build_can_judge() {
    let p = dip3_at_20();
    let mut w = world(Setup {
        params: Some(p.clone()),
        steps: StepsCfg {
            complete: true,
            ..StepsCfg::default()
        },
        ..Setup::default()
    });
    let blocks = empty_chain(&p, &w.rig.genesis, 21);
    for m in &blocks {
        w.rig.accept(m);
    }
    let last = blocks.last().expect("a block");
    let (tip, id6) = (w.rig.id(last), w.rig.id(&blocks[5]));
    let exit = w.rig.stop();
    let want = refusal(&p, last, &g1_capability("DIP0003"));
    assert_eq!(startup::<TestEvo<false>>(&p, &exit, &w.stores, tip), Err(want));
    assert_eq!(startup::<TestEvo<true>>(&p, &exit, &w.stores, tip), Ok(()));
    assert_eq!(startup::<TestEvo<false>>(&p, &exit, &w.stores, id6), Ok(()));
}

/// CH7-notes D67: a complete build's startup check reads the stored block from DIP0003 on, and
/// only there: a [`MARKED`] tip at block 20 is refused, one at block 19 is not, unread. A tip
/// that does not read is judged without its data (RP1a D47); a block before it is VerifyDB's
/// corrupted block database (Opus r1 n-1). Only the tip is excused: with block 21 read, an
/// unreadable tip 22 passes (CH7a-4a r2 n-2).
#[test]
fn the_startup_check_reads_the_block_from_dip3_on() {
    let p = dip3_at_20();
    let mut w = world(Setup {
        params: Some(p.clone()),
        steps: StepsCfg {
            complete: true,
            ..StepsCfg::default()
        },
        ..Setup::default()
    });
    let blocks = empty_chain(&p, &w.rig.genesis, 18);
    let b18 = blocks.last().expect("a block");
    let b19 = with_coinbase(&p, b18, coinbase_tx(19, 1, 0, MARKED), vec![]);
    let mut cb = coinbase_tx(20, 3, 5, MARKED).to_mutable();
    cb = dash_primitives::Transaction::new(
        cb.version(),
        cb.tx_type(),
        cb.inputs().clone(),
        cb.outputs().clone(),
        cb.lock_time(),
        [&1u16.to_le_bytes()[..], &20u32.to_le_bytes(), &[0; 32]].concat(),
    );
    let b20 = with_coinbase(&p, &b19, Arc::new(Tx::new(cb)), vec![]);
    let mut cb = coinbase_tx(21, 3, 5, 0).to_mutable();
    cb = dash_primitives::Transaction::new(
        cb.version(),
        cb.tx_type(),
        cb.inputs().clone(),
        cb.outputs().clone(),
        cb.lock_time(),
        [&1u16.to_le_bytes()[..], &21u32.to_le_bytes(), &[0; 32]].concat(),
    );
    let b21 = with_coinbase(&p, &b20, Arc::new(Tx::new(cb)), vec![]);
    let mut cb = coinbase_tx(22, 3, 5, 0).to_mutable();
    cb = dash_primitives::Transaction::new(
        cb.version(),
        cb.tx_type(),
        cb.inputs().clone(),
        cb.outputs().clone(),
        cb.lock_time(),
        [&1u16.to_le_bytes()[..], &22u32.to_le_bytes(), &[0; 32]].concat(),
    );
    let b22 = with_coinbase(&p, &b21, Arc::new(Tx::new(cb)), vec![]);
    for m in blocks.iter().chain([&b19, &b20, &b21, &b22]) {
        w.rig.accept(m);
    }
    let (id19, id20, id21) = (w.rig.id(&b19), w.rig.id(&b20), w.rig.id(&b21));
    let id22 = w.rig.id(&b22);
    let exit = w.rig.stop();
    let want = refusal(&p, &b20, MARKED_BLOCK);
    assert_eq!(startup::<TestEvo<true>>(&p, &exit, &w.stores, id20), Err(want));
    assert_eq!(startup::<TestEvo<true>>(&p, &exit, &w.stores, id19), Ok(()));
    // From block files that hold none: the marked tip is judged unread, and VerifyDB, next,
    // fails the load; recovery's block 20 before the tip 21 fails it here.
    let unread = |ids: &[BlockId]| {
        check_startup_readiness::<TestEvo<true>>(&p, &exit.chain, &SharedFiles::default(), ids)
            .map_err(|e| e.to_string())
    };
    assert_eq!(unread(&[id20]), Ok(()));
    assert_eq!(unread(&[id20, id21]), Err(CORRUPTED_BLOCK_DB.to_owned()));
    let without_22 = Missing {
        files: &w.stores.blocks,
        missing: b22.hash(),
    };
    let tip_unread =
        check_startup_readiness::<TestEvo<true>>(&p, &exit.chain, &without_22, &[id21, id22]);
    assert!(tip_unread.is_ok(), "{tip_unread:?}");
}

/// `files` without `missing`'s data, for the startup check, which only reads.
struct Missing<'f> {
    files: &'f SharedFiles,
    missing: BlockHash,
}

impl crate::chain::seams::BlockFiles for Missing<'_> {
    fn save_block(
        &mut self,
        _: &Arc<Block>,
        _: u32,
        _: Option<FlatFilePos>,
    ) -> Result<Option<FlatFilePos>, String> {
        unreachable!("the startup check writes no block")
    }

    fn scan_already_pruned_files(&mut self, _: bool) -> Result<(), String> {
        unreachable!("the startup check scans no block file")
    }

    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        if *hash == self.missing {
            return Err(format!("no block at {pos:?}"));
        }
        crate::chain::seams::BlockFiles::read_block(self.files, pos, hash)
    }

    fn open_block_file(&self, file: i32) -> bool {
        self.files.open_block_file(file)
    }

    fn current_usage(&self) -> u64 {
        self.files.current_usage()
    }
}

/// CH7 D57: G1 reads no coinbase at startup. A special-version coinbase below DIP0003, which a
/// complete build connected, does not stop an incomplete build's start. Should that build
/// connect the block again (a reorg back over it, `-checklevel=4`), `NoEvo`'s H4 refuses it with
/// `SPECIAL_VERSION_CBTX`, never a verdict; undoing it below DIP0003 reads no evo state.
#[test]
fn a_special_version_coinbase_below_dip3_does_not_refuse_a_start() {
    let p = regtest();
    let mut w = world(Setup {
        steps: StepsCfg {
            complete: true,
            ..StepsCfg::default()
        },
        ..Setup::default()
    });
    let b1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 3, 5, COIN), vec![]);
    w.rig.accept(&b1);
    let tip = w.rig.id(&b1);
    let exit = w.rig.stop();
    assert_eq!(startup::<TestEvo<false>>(&p, &exit, &w.stores, tip), Ok(()));
    assert_eq!(startup::<TestEvo<true>>(&p, &exit, &w.stores, tip), Ok(()));
}

/// D32 at index 0: a special-version coinbase of another type than CbTx is the gate's, not G1's.
#[test]
fn a_special_coinbase_below_dip3_is_refused_by_the_gate() {
    let w = world(Setup::default());
    let cb = coinbase_tx(1, 3, 1, COIN);
    let b1 = with_coinbase(&w.rig.p, &w.rig.genesis, cb.clone(), vec![]);
    let err = last_invalid(&w, &[b1]);
    assert_eq!(
        (err.result(), err.reject_reason(), err.debug_message()),
        (
            BlockValidationResult::Consensus,
            "bad-tx-type-dip3-inactive",
            format!("Special Transaction check failed (tx hash {}) ", cb.txid()).as_str()
        )
    );
}

/// CH3 §3.2's fault injection (D33): applying the attempt's effects fails before the first, between
/// the two, and at the release. Each time V stops on that `Fatal`, reported once, with the tip,
/// the active coins and the effects applied before the failure where they were: nothing of the
/// attempt is committed. (The undo position and `HAVE_UNDO` precede the effects, in memory only:
/// the latch blocks their flush. The restart half arrives with CH2b's production flush site.)
#[test]
fn a_failed_effect_application_commits_nothing() {
    for k in 0..=2 {
        let (a, b) = (lock_a(), lock_b());
        let mut setup = is_setup(vec![a.clone(), b.clone()], true);
        setup.steps.fail_apply = Some(k);
        let mut w = world(setup);
        let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![spend(&[seed_op(1), seed_op(2)], COIN)]);
        w.rig.submit_stopping(&b1, &format!("k {k}"));
        let exit = w.rig.stop();
        assert_eq!(fatal_text(&exit), Some(APPLY_FAILED), "k {k}");
        assert_eq!(w.sink.messages(), [APPLY_FAILED], "k {k}: one report");
        assert_eq!(
            exit.chain.private_tip().map(|(t, _)| t.hash),
            Some(w.rig.genesis.hash()),
            "k {k}"
        );
        assert_eq!(coins_at(&w.coins), Some(w.rig.genesis.hash()), "k {k}");
        let want: Vec<Effect> = [&a, &b][..k].iter().map(|l| removal(l)).collect();
        assert_eq!(w.seen.lock().expect("seen").effects, want, "k {k}");
        let rolled_back = [
            Call::BeginAttempt,
            Call::BeginTx,
            Call::SetBestBlock,
            Call::FinishAttempt,
            Call::EndAttempt,
            Call::RollbackTx,
        ];
        assert_eq!(bracket(&w), rolled_back, "k {k}");
        assert_eq!(w.seen.lock().expect("seen").evo_best, None, "k {k}");
        let mut coins = w.coins.lock().expect("coins");
        let unspent = coins.fetch(&OutPointKey::new(&seed_op(1))).expect("reads").is_some();
        assert!(unspent, "k {k}: the spent coin is still unspent");
    }
}

/// CH3 §2.1.1's undo rows (D60, D66, D77): a failed `WriteUndoDataForBlock` stops V with Core's
/// text reported once, or with nothing more when another store's failure had latched, V then
/// naming that failure's text (W's report, CH3b D221); the block
/// keeps `HAVE_DATA` without undo or a failure bit, the attempt rolls back, and the log has Core's
/// lines only: "ConnectBlock(): FindUndoPos failed" after a space check, the open line after a
/// failed write, none after a failed flush.
#[test]
fn a_failed_undo_write_stops_v_as_core_does() {
    install_accept_log();
    const FIND: &str = "ConnectBlock(): FindUndoPos failed";
    const OPEN: &str = "UndoWriteToDisk: OpenUndoFile failed";
    // Block 1's data is in file 0, the last block of a file the node has moved past: its undo
    // write finalizes and flushes the rev file (`blockstorage.cpp:790-792`).
    let moved_past = BlockFilesState {
        infos: vec![
            BlockFileInfo {
                height_last: 1,
                ..BlockFileInfo::default()
            },
            BlockFileInfo::default(),
        ],
        last_file: 1,
    };
    // (fault, the files' state, the report and its user message, the two lines' counts)
    let space = Some(DISK_SPACE_LOW);
    for (fault, state, msg, user, find, open) in [
        (UndoFault::NoSpace, BlockFilesState::default(), space, space, 1, 0),
        (UndoFault::Write, BlockFilesState::default(), Some(WRITE_UNDO_FAILED), None, 0, 1),
        (UndoFault::Flush, moved_past, Some(FLUSH_UNDO_FAILED), None, 0, 0),
        (UndoFault::Latched(OTHER_STORE_SITE), BlockFilesState::default(), None, None, 0, 0),
        (UndoFault::WLatched, BlockFilesState::default(), Some(W_REPORTED), None, 0, 0),
    ] {
        let mut w = world(Setup {
            undo_state: state,
            ..Setup::default()
        });
        w.stores.fs.arm(fault);
        let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
        w.rig.submit_stopping(&b1, &format!("{fault:?}"));
        let id = w.rig.id(&b1);
        // The lines on this world's V: other tests log the same lines beside it.
        let lines = |line| levels_on(w.rig.v_thread(), line).len();
        let logged = (lines(FIND), lines(OPEN));
        let v = w.rig.v_thread();
        let exit = w.rig.stop();
        match msg {
            Some(msg) => {
                assert_eq!(fatal_text(&exit), Some(msg), "{fault:?}");
                let report = (msg.to_owned(), user.map(str::to_owned));
                assert_eq!(w.sink.reports(), [report], "{fault:?}: one report");
                // V's reading of the latch names the report (CH3b D221).
                let latched =
                    w.seen.lock().expect("seen").latch.tripped().expect("latched").clone();
                assert_eq!(crate::flush::abort_text(latched), msg, "{fault:?}");
            }
            None => {
                assert_eq!(fatal_text(&exit), Some(OTHER_STORE_FAILED), "{fault:?}");
                assert!(w.sink.messages().is_empty(), "{fault:?}: {:?}", w.sink.messages());
                let latch = w.seen.lock().expect("seen").latch.clone();
                let latched = latch.tripped().map(|f| f.message.clone());
                assert_eq!(latched.as_deref(), Some(OTHER_STORE_FAILED));
            }
        }
        let status = exit.chain.reader().entry(id).expect("indexed").status;
        assert!(status.has(Status::HAVE_DATA), "{fault:?}: {status:?}");
        assert!(!status.has(Status::HAVE_UNDO), "{fault:?}: {status:?}");
        assert!(!status.has(Status::FAILED_VALID), "{fault:?}: {status:?}");
        assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(w.rig.genesis.hash()));
        assert_eq!(coins_at(&w.coins), Some(w.rig.genesis.hash()), "{fault:?}");
        assert_eq!(bracket(&w), NOT_CONNECTED, "{fault:?}");
        assert_eq!(logged, (find, open), "{fault:?}");
        if fault == UndoFault::WLatched {
            // V's exit has joined V: its line is logged.
            let line = format!("ProcessNewBlock: ActivateBestChain failed ({W_REPORTED})");
            assert_eq!(levels_on(v, &line).len(), 1, "{fault:?}");
        }
    }
}

/// W's chainstate write failing on W's own thread (BL-554): it latches with [`W_FAILED`], and
/// W's report, [`W_REPORTED`], goes to `held`.
fn w_fails(latch: &GlobalFailStop, held: Arc<HeldSink>) -> std::thread::JoinHandle<()> {
    let failure = Failure {
        kind: dashd_store::ErrorKind::Io,
        site: "chainstate write",
        message: W_FAILED.to_owned(),
    };
    fail_on_w(latch, held, failure, W_REPORTED.to_owned())
}

/// The index of the first line in `lines` that `found` accepts.
fn line_at(lines: &[String], found: impl Fn(&str) -> bool) -> usize {
    lines.iter().position(|l| found(l)).unwrap_or_else(|| panic!("not logged: {lines:#?}"))
}

/// BL-554 (Sol r1 M-1, CH3b D222): an undo write refused because W's write failed meanwhile,
/// on W's own thread, logs only after W's report. W's report is held: V, its trip lost, waits for
/// it with no `ConnectTip` line; released, W's `***` line, then `ConnectTip`'s line, then
/// `ProcessNewBlock`'s.
// Core: validation.cpp:2245-2249, :3437-3443, :4905-4908
#[test]
fn a_refused_undo_write_logs_after_ws_report() {
    install_accept_log();
    let mut w = world(Setup::default());
    let held = w.stores.fs.held.lock().expect("held").clone().expect("a held sink");
    let latch = w.seen.lock().expect("seen").latch.clone();
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
    let v = w.rig.v_thread();
    let connect_tip = |l: &str| l.starts_with("ConnectTip: ConnectBlock");
    let release = held.arm();
    w.stores.fs.arm(UndoFault::WHeld);
    let reply = w.rig.v.process_block(b1.block.clone(), true, None, true);
    let on_w = held.parked(&latch, v);
    let early = lines_on(v).iter().any(|l| connect_tip(l));
    release.release();
    assert_eq!(reply.recv().map(|_| ()), Err(crate::chain::reply::WaitError::Disconnected));
    let w_thread = w.stores.fs.w.lock().expect("w").take().expect("W ran");
    w_thread.join().expect("W reports");
    let on_w = on_w.expect("W reached its report, and V's refused write waits for it");
    assert!(!early, "V logged before W's report");
    let lines = lines_of(&[on_w, v]);
    let reported = line_at(&lines, |l| l == format!("*** {W_REPORTED}"));
    let tip_line = line_at(&lines, connect_tip);
    let pnb = format!("ProcessNewBlock: ActivateBestChain failed ({W_REPORTED})");
    assert!(reported < tip_line && tip_line < line_at(&lines, |l| l == pnb), "{lines:#?}");
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), Some(W_REPORTED));
    assert_eq!(w.sink.messages(), [W_REPORTED], "one report");
}

/// BL-586 (Sol r1 B-1, K2): an undo write refused because W panicked meanwhile, past the
/// connect's last flush-side check. The refusal carries W's panic as the assert it is, reported:
/// W's report is the only one, V logs no `ProcessNewBlock` failure line, and V stops on the
/// assert: no `ConnectTip` line either (BL-673).
// Core: util/check.h:49-51; validation.cpp:4905-4908; validation.cpp:3442-3448
#[test]
fn a_w_panic_first_read_at_a_refused_undo_write_reports_once() {
    install_accept_log();
    let mut w = world(Setup::default());
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
    let v = w.rig.v_thread();
    w.stores.fs.arm(UndoFault::WPanicked);
    w.rig.submit_stopping(&b1, "a refused undo write");
    let fatal = w.rig.stop().fatal.expect("V stopped on the latch");
    assert_eq!(w.sink.reports(), [(W_PANICKED.to_owned(), None)], "W's report only");
    let lines = lines_on(v);
    assert!(!lines.iter().any(|l| l.starts_with("ConnectTip:")), "{lines:#?}");
    assert!(!lines.iter().any(|l| l.starts_with("ProcessNewBlock:")), "{lines:#?}");
    assert_eq!(fatal.0, W_PANICKED);
    assert!(!fatal.is_abort_node() && fatal.is_reported(), "{fatal:?}");
}

/// BL-750: the `BlockChecked` calls for `m` are one `Error(msg)` after exactly `reports`, and
/// V's `ConnectTip` line with `msg` follows it, then `ProcessNewBlock`'s.
fn assert_checked_before_the_line(
    checked: &CheckedAt,
    v: std::thread::ThreadId,
    m: &Mined,
    msg: &str,
    reports: &[&str],
) {
    let calls = checked.calls_for(m.hash());
    assert_eq!(calls.len(), 1, "{msg}: one BlockChecked: {calls:?}");
    let call = &calls[0];
    assert_eq!(call.state, BlockState::Error(msg.into()), "{msg}");
    assert_eq!(call.reports, reports, "{msg}: the reports before the callback");
    let lines = lines_on(v);
    let (tip, pnb) = connect_lines(m, msg);
    let tip_line = line_at(&lines, |l| l == tip);
    assert!(call.lines <= tip_line, "{msg}: the line came before the callback: {lines:#?}");
    assert!(tip_line < line_at(&lines, |l| l == pnb), "{lines:#?}");
}

/// BL-750: an undo write the latch refused after W's store failure, which latched on W's own
/// thread while V was inside `ConnectBlock` (its report held), is Core's `AbortNode(state, ...)`
/// in `WriteUndoDataForBlock` then `false`: after W's report (the only one), one
/// `BlockChecked(Error)` with W's reported text (CH3b D221's funnel), then `ConnectTip`'s line,
/// then `ProcessNewBlock`'s. The attempt rolls back and the active tip stays at genesis.
// Core: node/blockstorage.cpp:786-787; validation.cpp:2899-2900, :3443, :3447, :4910-4912
#[test]
fn a_refused_undo_write_after_ws_store_failure_checks_the_block_with_ws_text() {
    install_accept_log();
    let mut w = world(Setup::default());
    let held = w.stores.fs.held.lock().expect("held").clone().expect("a held sink");
    let latch = w.seen.lock().expect("seen").latch.clone();
    let checked = CheckedAt::on(&w.rig.signals, w.sink.clone());
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
    let v = w.rig.v_thread();
    let release = held.arm();
    w.stores.fs.arm(UndoFault::WHeld);
    let reply = w.rig.v.process_block(b1.block.clone(), true, None, true);
    let on_w = held.parked(&latch, v);
    let early = checked.calls_for(b1.hash());
    release.release();
    assert_eq!(reply.recv().map(|_| ()), Err(crate::chain::reply::WaitError::Disconnected));
    let w_thread = w.stores.fs.w.lock().expect("w").take().expect("W ran");
    w_thread.join().expect("W reports");
    on_w.expect("W reached its report, and V's refused write waits for it");
    assert!(early.is_empty(), "V checked the block before W's report: {early:?}");
    assert_checked_before_the_line(&checked, v, &b1, W_REPORTED, &[W_REPORTED]);
    let id = w.rig.id(&b1);
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), Some(W_REPORTED));
    assert_eq!(w.sink.messages(), [W_REPORTED], "one report");
    let status = exit.chain.reader().entry(id).expect("indexed").status;
    assert!(status.has(Status::HAVE_DATA), "{status:?}");
    assert!(!status.has(Status::HAVE_UNDO) && !status.has(Status::FAILED_VALID), "{status:?}");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(w.rig.genesis.hash()));
    assert_eq!(coins_at(&w.coins), Some(w.rig.genesis.hash()));
    assert_eq!(bracket(&w), NOT_CONNECTED, "the attempt rolled back");
}

/// BL-750's controls and siblings: each failed or refused undo write gives the block one
/// `BlockChecked(Error)` before `ConnectTip`'s line, after the reports made by then: on space,
/// the write's own `AbortNode`; after a store failure W latched and reported, W's text; after
/// another failure that latched unreported (a store's, the flush's own, a block file's: each
/// non-assert class), its text and no report. After W's panic (an assert: Core's process had
/// ended) none follows, and no line.
// Core: node/blockstorage.cpp:782-787; validation.cpp:2899-2900, :3443, :3447;
// util/check.h:49-51
#[test]
fn an_undo_write_checks_the_block_unless_an_assert_came_first() {
    install_accept_log();
    // (fault, the callback's and the line's text, the reports before the callback)
    let cases: [(UndoFault, Option<&str>, &[&str]); 6] = [
        (UndoFault::NoSpace, Some(DISK_SPACE_LOW), &[DISK_SPACE_LOW]),
        (UndoFault::WLatched, Some(W_REPORTED), &[W_REPORTED]),
        (UndoFault::Latched(OTHER_STORE_SITE), Some(OTHER_STORE_FAILED), &[]),
        (UndoFault::Latched(FLUSH_SITE), Some(OTHER_STORE_FAILED), &[]),
        (UndoFault::Latched(BLOCK_FILES_WRITE_SITE), Some(OTHER_STORE_FAILED), &[]),
        (UndoFault::WPanicked, None, &[W_PANICKED]),
    ];
    for (fault, msg, reports) in cases {
        let mut w = world(Setup::default());
        let checked = CheckedAt::on(&w.rig.signals, w.sink.clone());
        w.stores.fs.arm(fault);
        let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
        let v = w.rig.v_thread();
        w.rig.submit_stopping(&b1, &format!("{fault:?}"));
        match msg {
            Some(msg) => assert_checked_before_the_line(&checked, v, &b1, msg, reports),
            None => {
                assert_eq!(checked.calls_for(b1.hash()), [], "{fault:?}: no BlockChecked");
                let lines = lines_on(v);
                assert!(!lines.iter().any(|l| l.starts_with("ConnectTip:")), "{lines:#?}");
            }
        }
        w.rig.stop();
        assert_eq!(w.sink.messages(), reports, "{fault:?}");
        assert_eq!(coins_at(&w.coins), Some(w.rig.genesis.hash()), "{fault:?}");
    }
}

/// BL-750's control for BL-685's exception: a block whose transaction re-creates an unspent coin
/// the cache holds is Core's `std::logic_error` in `AddCoin`, thrown inside `ConnectBlock`
/// before `BlockChecked` (`v:3443`) is reached. V reports it before `ConnectTip`'s line, and no
/// `BlockChecked` follows. Regtest's BIP34 block is b1 here, so b2 skips BIP30's probe
/// (`v:2598-2601`) and reaches `AddCoin`.
// Core: coins.cpp:70-82; validation.cpp:2598-2601, :3442-3448
#[test]
fn a_coins_logic_error_in_connect_block_checks_no_block() {
    install_accept_log();
    let mut p = (*regtest()).clone();
    let g = Mined::genesis(&p);
    let b1 = block_on(&p, &g, vec![]);
    p.consensus.bip34.1 = Hash256::from(*b1.hash().as_bytes());
    let t = spend(&[seed_op(1)], COIN);
    let b2 = block_on(&p, &b1, vec![t.clone()]);
    let mut again = seed(2, COIN, &[OP_TRUE]);
    again.0 = op(&t, 0);
    let mut w = world(Setup {
        params: Some(Arc::new(p)),
        seeds: vec![seed(1, 50 * COIN, &[OP_TRUE]), again],
        ..Setup::default()
    });
    let checked = CheckedAt::on(&w.rig.signals, w.sink.clone());
    w.rig.accept(&b1);
    let v = w.rig.v_thread();
    w.rig.submit_stopping(&b2, "OverwriteUnspent");
    let msg = format!("coins: {}", crate::error::CoinsError::OverwriteUnspent);
    assert_eq!(checked.calls_for(b2.hash()), [], "no BlockChecked");
    let lines = lines_on(v);
    let (tip, pnb) = connect_lines(&b2, &msg);
    assert_logged_before(&lines, &format!("*** {msg}"), &tip);
    assert_logged_before(&lines, &tip, &pnb);
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), Some(msg.as_str()));
    assert_eq!(w.sink.messages(), [msg], "one report");
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
}

/// BL-554 (Sol r2 M-3, CH3b D222): V's own `AbortNode` inside `ConnectBlock` (the evo store
/// behind the parent, `v:2525-2531`), its trip losing to W's failure, reports after W's report.
/// W fails while V verifies the evo best block, past V's earlier reads of the latch, and holds
/// its report: V waits for it before its own; released, W's `***` line, V's, then
/// `ConnectTip`'s line.
// Core: validation.cpp:2525-2531, :3437-3443; shutdown.cpp:27-37
#[test]
fn vs_losing_abort_node_reports_after_ws_report() {
    install_accept_log();
    let mut w = world(Setup {
        steps: StepsCfg {
            complete: true,
            w_fails_at_verify: true,
            ..StepsCfg::default()
        },
        extra: Some(Deployment::Buried(BuriedDeployment::Dip0003)),
        ..Setup::default()
    });
    let held = Arc::new(HeldSink::new(w.sink.clone()));
    w.seen.lock().expect("seen").held = Some(held.clone());
    let latch = w.seen.lock().expect("seen").latch.clone();
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![]);
    let v = w.rig.v_thread();
    let release = held.arm();
    let reply = w.rig.v.process_block(b1.block.clone(), true, None, true);
    let on_w = held.parked(&latch, v);
    let early = w.sink.messages();
    release.release();
    assert_eq!(reply.recv().map(|_| ()), Err(crate::chain::reply::WaitError::Disconnected));
    let w_thread = w.seen.lock().expect("seen").w.take().expect("W ran");
    w_thread.join().expect("W reports");
    let on_w = on_w.expect("W reached its report, and V's losing AbortNode waits for it");
    assert!(early.is_empty(), "V reported before W: {early:?}");
    assert_eq!(w.sink.messages(), [W_REPORTED, EVODB_INCONSISTENCY]);
    let lines = lines_of(&[on_w, v]);
    let ws = line_at(&lines, |l| l == format!("*** {W_REPORTED}"));
    let vs = line_at(&lines, |l| l == format!("*** {EVODB_INCONSISTENCY}"));
    let tip_line = line_at(&lines, |l| l.starts_with("ConnectTip: ConnectBlock"));
    assert!(ws < vs && vs < tip_line, "{lines:#?}");
    assert!(w.rig.stop().fatal.is_some());
}

/// [`Scripted::cl_fails_after_w`]'s `Fatal`.
const CL_VIEW_FAILED: &str = "the ChainLock view failed";

/// [`Latched`], whose [`FlushSite::latched`] answers without waiting, as `NodeFlush`'s does
/// (`tripped_now`): only its `abort` waits for a pending report (CH3b D222).
struct LatchedNow(GlobalFailStop);

impl FlushSite for LatchedNow {
    fn at_site(&mut self, _: FlushStateMode, _: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        Ok(SiteDecision::nothing())
    }

    fn after_step(&mut self, _: SiteDecision, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn abort(&mut self, cause: AbortCause, host: &mut FlushHost<'_>) -> bool {
        Latched(self.0.clone()).abort(cause, host)
    }

    fn latched(&self) -> Option<Fatal> {
        self.0.tripped_now().cloned().map(crate::flush::latched_fatal)
    }
}

/// BL-710 (Sol plan r1, CH3b D222): a `Fatal` nobody reported from `ConnectBlock` (the helper's
/// ChainLock query's, [`Scripted::cl_fails_after_w`]), W's store failure latching meanwhile on
/// W's own thread with its report held. V's latch read where the `Fatal` arose waits for W's
/// report, with no `ConnectTip` line and no report of V's; released, W's `***` line, then
/// `ConnectTip`'s line and `ProcessNewBlock`'s. W's is the one report (L-3).
// Core: validation.cpp:3442-3448, :4910-4912; shutdown.cpp:27-38
#[test]
fn a_store_failure_latched_first_keeps_its_one_report_before_the_connect_tip_line() {
    install_accept_log();
    let (mut cfg, _stores, sink, seen) = node(Setup {
        helper: Scripted {
            cl_fails_after_w: true,
            ..Scripted::default()
        },
        ..Setup::default()
    });
    let held = Arc::new(HeldSink::new(sink.clone()));
    let latch = {
        let mut seen = seen.lock().expect("seen");
        seen.held = Some(held.clone());
        seen.latch.clone()
    };
    cfg.flush = Some(Box::new(LatchedNow(latch.clone())));
    let mut rig = Rig::with(cfg);
    let b1 = block_on(&rig.p, &rig.genesis, vec![]);
    let v = rig.v_thread();
    let release = held.arm();
    let reply = rig.v.process_block(b1.block.clone(), true, None, true);
    let on_w = held.parked(&latch, v);
    let early = (sink.messages(), lines_on(v).iter().any(|l| l.starts_with("ConnectTip:")));
    release.release();
    assert_eq!(reply.recv().map(|_| ()), Err(crate::chain::reply::WaitError::Disconnected));
    let w_thread = seen.lock().expect("seen").w.take().expect("W ran");
    w_thread.join().expect("W reports");
    let on_w = on_w.expect("W reached its report, and V's read at the fault waits for it");
    assert_eq!(early, (vec![], false), "V went on before W's report");
    assert_eq!(sink.messages(), [W_REPORTED], "W's report only");
    let lines = lines_of(&[on_w, v]);
    let reported = line_at(&lines, |l| l == format!("*** {W_REPORTED}"));
    let tip_line = line_at(&lines, |l| {
        l == format!("ConnectTip: ConnectBlock {} failed, {CL_VIEW_FAILED}", b1.hash())
    });
    let pnb = format!("ProcessNewBlock: ActivateBestChain failed ({CL_VIEW_FAILED})");
    assert!(reported < tip_line && tip_line < line_at(&lines, |l| l == pnb), "{lines:#?}");
    // BL-750: the query's `Fatal` is the `Err` arm's, which no `BlockChecked` follows.
    assert_eq!(checked(&rig), [], "no BlockChecked");
    assert_eq!(fatal_text(&rig.stop()), Some(CL_VIEW_FAILED));
}

/// ND1's header-chain fail-stop (`dashd-node`'s `ReadFailStop`): a read failure goes to the
/// node's sink, which here records it (ND1's logs it and aborts the process).
struct ToSink(Arc<RecordingAbort>, std::sync::atomic::AtomicBool);

impl dash_chain::FailStop for ToSink {
    fn read_failed(&self, what: &str) {
        self.1.store(true, std::sync::atomic::Ordering::SeqCst);
        crate::chain::AbortSink::read_failed(&*self.0, what);
    }

    fn is_tripped(&self) -> bool {
        self.1.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// What [`unwound_connect`] leaves.
struct Unwound {
    seen: Arc<Mutex<Seen>>,
    /// The block the active coins are at.
    coins: Option<BlockHash>,
    genesis: BlockHash,
    b1: BlockHash,
}

/// Connects b1 on a node whose hooks panic as `steps` says and whose queue has `script_threads`,
/// with the tip hold reporting to the abort sink: the panic reaches V's joiner, the one report is
/// the hold's read failure, and the guard has tripped the latch (D71). The active coins' cell,
/// which the attempt held, is left poisoned, so a later lock is a `Fatal`.
fn unwound_connect(steps: StepsCfg, script_threads: usize) -> Unwound {
    let (mut cfg, stores, sink, seen) = node(Setup {
        steps,
        script_threads,
        ..Setup::default()
    });
    cfg.fail = Some(Arc::new(ToSink(sink.clone(), false.into())));
    let mut rig = Rig::with(cfg);
    let b1 = block_on(&rig.p, &rig.genesis, vec![]);
    rig.submit_stopping(&b1, "");
    let joined = rig.join.take().expect("running").join().expect("an unmarked joiner");
    assert!(joined.is_err(), "the panic reaches V's joiner");
    assert_eq!(
        sink.messages(),
        ["Error reading from database: a tip hold unwound inside the tip lock"],
        "one report"
    );
    let (latch, evo_best) = {
        let seen = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (seen.latch.clone(), seen.evo_best)
    };
    assert_eq!(evo_best, None, "no evo commit");
    let f = latch.tripped().expect("tripped");
    assert_eq!(
        (f.site, f.message.as_str()),
        (UNWIND_SITE, "an EV attempt unwound before its commit or rollback returned")
    );
    let Err(poisoned) = stores.coins.lock() else {
        panic!("the cell is poisoned");
    };
    Unwound {
        seen,
        coins: poisoned.into_inner().best_block().map(|b| b.hash),
        genesis: rig.genesis.hash(),
        b1: b1.hash(),
    }
}

/// D71 through the connector: a hook that panics mid-attempt unwinds through the bracket. The
/// guard trips the latch (at [`UNWIND_SITE`]) before the attempt drops, so before a
/// facade's gate is released, and nothing of the attempt is committed: no `finish_attempt`, no
/// evo commit, the coins and the tip at the parent. The one report is the tip hold's read
/// failure (in ND1 the process aborts there); V's own report of the panic then finds the latch
/// tripped through this rig's latch-aware flush site (L-3), and the panic reaches V's joiner.
/// H4 runs inside the check queue's control (`v:2649-2670`), so with threads the panic unwinds
/// through CS6's control too, with the same outcome. That describes the code's structure; this
/// test cannot tell it from H4 outside the scope, with no check queued before H4 (CH3a D71).
#[test]
fn a_hook_panicking_mid_attempt_trips_the_latch_before_the_attempt_drops() {
    for (mode, threads) in queue::modes() {
        let steps = StepsCfg {
            panic_in_h4: true,
            ..StepsCfg::default()
        };
        let u = unwound_connect(steps, threads);
        let seen = u.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let latched = seen.dropped_latched.last();
        assert_eq!(latched, Some(&true), "{mode}: latched before the attempt dropped");
        assert_eq!(seen.last_attempt(), [Call::BeginAttempt, Call::BeginTx], "{mode}");
        assert_eq!(u.coins, Some(u.genesis), "{mode}");
    }
}

/// D71 through the commit (Opus a3 r1 F3): the guard stays armed until the commit returns, so a
/// `commit_tx` that panics after the coins' commit trips the latch too, with the same one report.
/// The attempt was finished, and dropped, before the commit. The active coins are at the block in
/// memory and evo at its parent; the tripped latch refuses every chainstate write and block-file
/// mutation after them.
#[test]
fn a_commit_tx_panicking_trips_the_latch() {
    let u = unwound_connect(
        StepsCfg {
            panic_in_commit: true,
            ..StepsCfg::default()
        },
        0,
    );
    let seen = u.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(seen.dropped_latched.last(), Some(&false), "the attempt finished");
    assert_eq!(seen.last_bracket(), CONNECTED);
    assert_eq!(u.coins, Some(u.b1));
}

/// D71 on the tip path's disconnect: H9 panics inside ABC's `DisconnectTip` on a reorg. The tip
/// hold covers it, so the one report is the hold's read failure, never the guard's [`UNWOUND`],
/// and the guard has tripped the latch at [`UNWIND_SITE`].
#[test]
fn a_hook_panicking_in_a_tip_disconnect_reports_once() {
    let (mut cfg, _stores, sink, seen) = node(Setup {
        steps: StepsCfg {
            panic_in_tip_undo: true,
            ..StepsCfg::default()
        },
        ..Setup::default()
    });
    cfg.fail = Some(Arc::new(ToSink(sink.clone(), false.into())));
    let mut rig = Rig::with(cfg);
    let p = rig.p.clone();
    let b1 = block_on(&p, &rig.genesis, vec![]);
    rig.accept(&b1);
    let s1 = with_coinbase(&p, &rig.genesis, coinbase_tx(1, 1, 0, COIN - 2), vec![]);
    let s2 = block_on(&p, &s1, vec![]);
    assert!(rig.submit(&s1).accepted);
    rig.submit_stopping(&s2, "the reorg's disconnect unwinds");
    assert_eq!(rig.join_panicked(), HOOK_PANICKED);
    assert_eq!(
        sink.reports(),
        [("Error reading from database: a tip hold unwound inside the tip lock".to_owned(), None)],
        "the hold's one report"
    );
    let seen = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let f = seen.latch.tripped().expect("tripped");
    assert_eq!((f.site, f.message.as_str()), (UNWIND_SITE, UNWOUND));
    assert_eq!(seen.evo_steps.last(), Some(&EvoStep::UndoSpecial), "H9 unwound");
}

mod disconnect;
mod verify_db;
