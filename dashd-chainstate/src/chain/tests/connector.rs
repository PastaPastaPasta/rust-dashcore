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
mod proposal;

use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use dash_chain::{Status, Work};
use dash_consensus::coins::{Coin, CoinsView, TxUndo, decode_block_undo};
use dash_consensus::ctx::{Deployment, DeploymentPos, DeploymentView, bls_ctx_for_block};
use dash_consensus::payments::MnRewardEra;
use dash_consensus::{
    Amount, Block, BlockId, BlockValidationError, BlockValidationResult, BlsCtx, Tx,
    TxValidationError, TxValidationResult,
};
use dash_evo::{Effect, EffectSink};
use dash_num::Hash256;
use dash_params::{
    BuriedDeployment, ChainOption, ChainParams, ChainParamsBuilder, NetworkKind, Release,
};
use dash_primitives::{BlockHash, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dashd_store::flatfile::{
    BlockFileInfo, BlockFiles, BlockFilesConfig, BlockFilesState, FlatFs, FlatRead, OsFs,
};

use super::connect::Latched;
use super::support::{
    AbortThenShutdown, Ev, Mined, Rig, RigConfig, SharedFiles, cb_script, install_accept_log,
    levels_on, mine, regtest,
};
use crate::chain::{
    AbortCause, AbortPath, AbortSink, AttemptOrigin, BlockConnector, BlockState, BlockStatusWriter,
    ChainLockView, ConnectCx, ConnectOutcome, DisconnectResult, EvoTarget, Fatal, FlushHost,
    FlushSite, FlushStateMode, GuardScope, LocalShutdown, ProposalChecks, RecordingAbort,
    SiteDecision, VContext, VExit, ViewTarget, check_startup_readiness,
};
use crate::checkqueue::CheckQueue;
use crate::coins::{BlockOverlay, CoinSrc, CoinsBudget, CoinsCache, OutPointKey, Ratio, Salt};
use crate::connect::{
    AttemptId, BlockAt, CORRUPT_BLOCK, ChainHelper, ChainstateCell, ConnectorParts, CoreConnector,
    DISK_SPACE_LOW, DigestSink, EVODB_INCONSISTENCY, EvoAttempt, EvoDigest, EvoHooks,
    FLUSH_UNDO_FAILED, INTERIM_REFUSAL, IsLockRef, MN_PAYEE_CAPABILITY, NodeHelper,
    SPECIAL_VERSION_CBTX, SpecialTxFailure, SpecialTxRules, WRITE_UNDO_FAILED, g1_capability,
    g2_refusal_text, is_governance_height, pre_dip3_special_txs, refusal_text,
    wrong_superblock_start,
};
use crate::sigcache::{CacheSizes, ScriptCaches};
use crate::store::{
    CoinsReadError, Failure, GlobalFailStop, GuardedEngine, GuardedFiles, ProbeEngine,
    RecordingSink,
};

mod queue;

const COIN: Amount = 100_000_000;
const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];
const OP_TRUE: u8 = 0x51;
const APPLY_FAILED: &str = "applying an effect failed";
const TX_FAILED: &str = "the evo transaction failed";
const OTHER_STORE_FAILED: &str = "another store's write failed";
const HOOK_PANICKED: &str = "a hook panicked";
const H8_FAILED: &str = "CheckSharedCollateralSpends failed";

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
    /// `commit_tx` panics (after the coins' commit).
    panic_in_commit: bool,
    /// H9's answer (`UndoSpecialTxsInBlock`).
    undo_special: bool,
    /// H8's `k`-th call (from 1, across the node's attempts) is a `Fatal`.
    fail_h8_at: Option<usize>,
    /// H8's `k`-th call rejects the transaction as `CheckSharedCollateralSpends` does
    /// (`bad-shared-collateral-spend`, `evo/specialtxman.cpp:1826`).
    reject_h8_at: Option<usize>,
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
            panic_in_commit: false,
            undo_special: true,
            fail_h8_at: None,
            reject_h8_at: None,
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
                    && l.inputs.contains(&i.prevout)
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
        for child in store.iter().filter(|l| l.inputs.iter().any(|i| Some(i.hash) == txid)) {
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
    /// H8's calls so far.
    h8_calls: usize,
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
    seen: Arc<Mutex<Seen>>,
}

/// Where a facade's gate would be released: records whether the latch had tripped by then.
impl Drop for TestAttempt<'_> {
    fn drop(&mut self) {
        let mut seen = self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let latched = seen.latch.tripped().is_some();
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

    fn begin_attempt<'v>(
        &mut self,
        _: &'v (),
        v: &'v mut VContext,
        _: &'v ConnectCx<'_>,
        id: AttemptId,
        _: AttemptOrigin,
        _: &ViewTarget<'_>,
    ) -> Result<TestAttempt<'v>, Fatal> {
        self.attempts += 1;
        self.call(Call::BeginAttempt);
        self.seen.lock().expect("seen").evo_steps.clear();
        Ok(TestAttempt {
            id,
            log: Vec::new(),
            _v: v,
            seen: self.seen.clone(),
        })
    }

    fn finish_attempt(&mut self, mut attempt: TestAttempt<'_>) -> Result<(), Fatal> {
        self.call(Call::FinishAttempt);
        if std::mem::take(&mut self.seen.lock().expect("seen").fail_next_apply) {
            return Err(Fatal::new(APPLY_FAILED));
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
        params: &ChainParams,
        at: &BlockAt,
    ) -> BlsCtx {
        self.step(EvoStep::BlsCtx);
        bls_ctx_for_block(params, at.height)
    }

    fn commit_bls_scheme(&mut self, _: &mut TestAttempt<'_>, _: &mut EvoTarget<'_>) {
        self.step(EvoStep::CommitBls);
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
        Ok(self.cfg.verify_best)
    }

    fn process_special_txs(
        &mut self,
        _: &mut TestAttempt<'_>,
        _: &mut EvoTarget<'_>,
        block: &Block,
        _: &BlockAt,
        _: SpecialTxRules,
        _: &dyn CoinsView<Error = CoinsReadError>,
        _: Amount,
        just_check: bool,
        check_cbtx_merkle_roots: bool,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
        self.seen.lock().expect("seen").just_checks.push(just_check);
        if self.cfg.panic_in_h4 && self.attempts > 1 {
            panic!("{HOOK_PANICKED}");
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
            return Err(Fatal::new(H8_FAILED));
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

    fn digest_fields(
        &mut self,
        _: &mut TestAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &BlockAt,
    ) -> Result<EvoDigest, Fatal> {
        Ok(EvoDigest::default())
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
    params: Option<Arc<ChainParams>>,
    steps: StepsCfg,
    helper: Scripted,
    /// Coins in the active view before block 1: (outpoint, output, height, coinbase).
    seeds: Vec<(OutPoint, TxOut, u32, bool)>,
    /// A deployment the core sees active from height 1 (not V).
    extra: Option<Deployment>,
    /// [`Setup::extra`] for disconnects only: a block connected without it is disconnected
    /// with it.
    disconnect_extra: Option<Deployment>,
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
    /// Another store's failure latches while the record is written: the write fails after it.
    Latched,
}

/// The OS file system with the undo-file failure a test arms.
#[derive(Default)]
struct UndoFs {
    os: OsFs,
    fault: Mutex<Option<UndoFault>>,
    /// The node's latch, for [`UndoFault::Latched`].
    latch: Mutex<Option<GlobalFailStop>>,
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
            Some(UndoFault::Latched) => {
                let latch = self.latch.lock().expect("latch").clone().expect("a latch");
                latch.trip(Failure {
                    kind: dashd_store::ErrorKind::Io,
                    site: "coins write",
                    message: OTHER_STORE_FAILED.to_owned(),
                });
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

/// What the harness saw.
#[derive(Default)]
struct Seen {
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
    /// Each connected block's script work, by height: (height, transactions that built their
    /// signature-hash data, inputs whose spent script was copied).
    scripts: Vec<(u32, usize, usize)>,
    /// The connector's check queue.
    queue: Option<Arc<CheckQueue>>,
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

/// [`CoreConnector`] over its own coins and undo files, with what a test changes around it:
/// the coins seeded before block 1, a deployment only the core sees, a block that reads back
/// otherwise.
struct Harness<const C: bool> {
    inner: CoreConnector<ProbeEngine, UndoFs, TestEvo<C>>,
    coins: ChainstateCell<ProbeEngine>,
    seeds: Vec<(OutPoint, TxOut, u32, bool)>,
    extra: Option<Deployment>,
    disconnect_extra: Option<Deployment>,
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
        let inner = &mut self.inner;
        let out = changed_cx(self.extra, self.locked_at, cx, |cx| {
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
    p: &ChainParams,
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
        h8_calls: 0,
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
    let (connector, seen) = if setup.steps.complete {
        harness::<true>(&p, setup, path, &stores)
    } else {
        harness::<false>(&p, setup, path, &stores)
    };
    let flush: Box<dyn FlushSite> = if node_stop {
        Box::new(Refusing(latch))
    } else {
        Box::new(Latched(latch))
    };
    let cfg = RigConfig {
        params: Some(p),
        files: Some(Box::new(stores.blocks.clone())),
        connector: Some(connector),
        flush: Some(flush),
        shutdown: Some(shutdown),
        abort: Some(sink.clone()),
        guard,
        after_genesis,
        min_chain_work,
        ..RigConfig::default()
    };
    (cfg, stores, sink, seen)
}

fn world(setup: Setup) -> World {
    let (cfg, stores, sink, seen) = node(setup);
    let rig = Rig::with(cfg);
    World {
        rig,
        sink,
        seen,
        coins: stores.coins.clone(),
        stores,
    }
}

fn out(value: Amount, script: &[u8]) -> TxOut {
    TxOut {
        value,
        script_pubkey: script.to_vec().into(),
    }
}

fn seed_op(n: u8) -> OutPoint {
    OutPoint {
        hash: TxHash::from([n; 32]),
        index: 0,
    }
}

/// A seeded, mature, non-coinbase coin of `value` to `script`.
fn seed(n: u8, value: Amount, script: &[u8]) -> (OutPoint, TxOut, u32, bool) {
    (seed_op(n), out(value, script), 0, false)
}

fn tx(version: i16, tx_type: u16, inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Arc<Tx> {
    Arc::new(Tx::new(Transaction {
        version,
        tx_type: TxType::new(tx_type),
        inputs,
        outputs,
        lock_time: 0,
        extra_payload: Vec::new(),
    }))
}

fn input(prevout: OutPoint, script_sig: Vec<u8>, sequence: u32) -> TxIn {
    TxIn {
        prevout,
        script_sig: script_sig.into(),
        sequence,
    }
}

/// A transaction spending `prevouts` with empty scriptSigs into one `OP_TRUE` output of `value`.
fn spend(prevouts: &[OutPoint], value: Amount) -> Arc<Tx> {
    let inputs = prevouts.iter().map(|op| input(*op, Vec::new(), u32::MAX)).collect();
    tx(1, 0, inputs, vec![out(value, &[OP_TRUE])])
}

/// The coinbase of the block at `height`: `version` and raw `tx_type`, paying `value`.
fn coinbase_tx(height: u32, version: i16, tx_type: u16, value: Amount) -> Arc<Tx> {
    let null = OutPoint {
        hash: TxHash::from([0; 32]),
        index: u32::MAX,
    };
    tx(
        version,
        tx_type,
        vec![input(null, cb_script(height, 0), u32::MAX)],
        vec![out(value, &[OP_TRUE])],
    )
}

/// The block after `parent` with a version-1 coinbase paying 1 DASH, then `txs`.
fn block_on(p: &ChainParams, parent: &Mined, txs: Vec<Arc<Tx>>) -> Mined {
    with_coinbase(p, parent, coinbase_tx(parent.height + 1, 1, 0, COIN), txs)
}

fn with_coinbase(p: &ChainParams, parent: &Mined, cb: Arc<Tx>, txs: Vec<Arc<Tx>>) -> Mined {
    let mut all = vec![cb];
    all.extend(txs);
    Mined {
        block: mine(p, parent.hash(), parent.time() + 150, all),
        height: parent.height + 1,
    }
}

fn op(tx: &Tx, index: u32) -> OutPoint {
    OutPoint {
        hash: tx.txid(),
        index,
    }
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
fn checked(rig: &Rig) -> Vec<(BlockHash, BlockState)> {
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
    build: impl FnOnce(&ChainParams, &Mined) -> Vec<Mined>,
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
    build: impl FnOnce(&ChainParams, &Mined) -> Vec<Mined>,
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
    let build = |p: &ChainParams, g: &Mined| {
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
    let t0 = OutPoint {
        hash: l.r.txid,
        index: 0,
    };
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
fn early_enforcement() -> Arc<ChainParams> {
    Arc::new(
        ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
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
fn refusal(p: &ChainParams, m: &Mined, capability: &str) -> String {
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
    p: Arc<ChainParams>,
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
    let main = ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0).build().expect("main");
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
fn dip3_at_20() -> Arc<ChainParams> {
    Arc::new(
        ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
            .arg(ChainOption::Dip3Params, "20:20")
            .build()
            .expect("regtest"),
    )
}

/// Blocks 1 to `n` on `genesis`, each with only its coinbase: from block 20 a version-1 CbTx
/// (`nVersion` 3, raw `nType` 5; `evo/cbtx.h`), which `ContextualCheckBlock` requires once DIP0003
/// is active ("bad-cb-type").
fn empty_chain(p: &ChainParams, genesis: &Mined, n: u32) -> Vec<Mined> {
    let mut blocks: Vec<Mined> = Vec::new();
    for _ in 0..n {
        let parent = blocks.last().unwrap_or(genesis);
        let h = parent.height + 1;
        let next = if h < 20 {
            block_on(p, parent, vec![])
        } else {
            let mut cb = coinbase_tx(h, 3, 5, COIN).to_mutable();
            cb.extra_payload = [&1u16.to_le_bytes()[..], &h.to_le_bytes(), &[0; 32]].concat();
            with_coinbase(p, parent, Arc::new(Tx::new(cb)), vec![])
        };
        blocks.push(next);
    }
    blocks
}

/// The startup check over a stopped node's chain, as a build with hooks `H` runs it.
fn startup<H: EvoHooks>(p: &Arc<ChainParams>, exit: &VExit, tip: BlockId) -> Result<(), String> {
    check_startup_readiness::<H>(p, &exit.chain, &[tip]).map_err(|e| e.to_string())
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

    assert_eq!(startup::<TestEvo<false>>(&p, &exit, tip), Ok(()));
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
    assert_eq!(startup::<TestEvo<false>>(&p, &exit, tip), Err(want));
    assert_eq!(startup::<TestEvo<true>>(&p, &exit, tip), Ok(()));
    assert_eq!(startup::<TestEvo<false>>(&p, &exit, id6), Ok(()));
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
    assert_eq!(startup::<TestEvo<false>>(&p, &exit, tip), Ok(()));
    assert_eq!(startup::<TestEvo<true>>(&p, &exit, tip), Ok(()));
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
/// text reported once, or with nothing more when another store's failure had latched; the block
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
        (UndoFault::Latched, BlockFilesState::default(), None, None, 0, 0),
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
        let exit = w.rig.stop();
        match msg {
            Some(msg) => {
                assert_eq!(fatal_text(&exit), Some(msg), "{fault:?}");
                let report = (msg.to_owned(), user.map(str::to_owned));
                assert_eq!(w.sink.reports(), [report], "{fault:?}: one report");
            }
            None => {
                assert!(exit.fatal.is_some(), "{fault:?}");
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
    }
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
        ("chainstate validation", "an EV attempt unwound before its commit or rollback returned")
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
/// guard trips the latch (site "chainstate validation") before the attempt drops, so before a
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

mod disconnect;
