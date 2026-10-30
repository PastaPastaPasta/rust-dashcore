//! [`CoreConnector`]: CH3a's [`BlockConnector`] (CH3 §2.1), the EV attempt's bracket (§3.2)
//! around the serial core ([`SerialCore`], CH3a D73).

use std::sync::{Arc, Mutex, TryLockError};

use dash_chain::BlockRef;
use dash_consensus::coins::CoinsView;
use dash_consensus::payments::MnRewardEra;
use dash_consensus::{
    Amount, Block, BlockId, BlockValidationError, BlockValidationResult, BlsCtx, Tx,
    TxValidationError,
};
use dash_evo::EffectSink;
use dash_kv::ErrorKind;
use dash_params::ChainParams;
use dash_primitives::BlockHash;
use dashd_store::KvEngine;
use dashd_store::flatfile::FlatFs;

use super::digest::{ConnectAttempt, DigestAttempt, DigestSink, DisconnectAttempt, Rewind};
use super::evo::{AttemptId, BlockAt, EvoAttempt, EvoHooks, SpecialTxFailure, SpecialTxRules};
use super::helper::ChainHelper;
use super::serial::{AttemptSteps, Finish, SerialCore, coins_failed};
use crate::chain::{
    AbortPath, AbortSink, BlockConnector, BlockStatusWriter, ConnectCx, ConnectOutcome,
    DisconnectResult, EvoReads, EvoTarget, Fatal, ProposalChecks, VContext, ViewTarget,
};
use crate::checkqueue::CheckQueue;
use crate::coins::{BlockOverlay, CoinsCache};
use crate::sigcache::ScriptCaches;
use crate::store::{CoinsReadError, Failure, GlobalFailStop, GuardedFiles};

/// The active coins (CH3 D2): shared by the connector, CH2b's production `FlushSite` and MP1's
/// coins commands, all on V's thread. The mutex is a V-only leaf, taken with `try_lock` (D50).
pub type ChainstateCell<E> = Arc<Mutex<CoinsCache<E>>>;

/// CH3a's connector: `ConnectBlock` over the active coins or a scratch target, its EV attempt
/// and its undo files.
pub struct CoreConnector<E: KvEngine + 'static, F: FlatFs, H: EvoHooks> {
    coins: ChainstateCell<E>,
    files: Arc<GuardedFiles<F>>,
    hooks: H,
    /// Beside the hooks, never inside them; each attempt opens on a local clone (CH3a D56).
    facade: Arc<H::Facade>,
    core: SerialCore,
    /// The node latch, which an unwinding attempt trips (CH3a D71).
    latch: GlobalFailStop,
    /// The StateDigest log of the attempts, if any (CH3 §2.15).
    digest: Option<Box<dyn DigestSink>>,
}

/// What [`CoreConnector::new`] takes besides the hooks.
pub struct ConnectorParts<E: KvEngine + 'static, F: FlatFs> {
    pub coins: ChainstateCell<E>,
    pub files: Arc<GuardedFiles<F>>,
    /// CS6's queue, built once from `-par` (`CheckQueue::serial()` for `-par=1`; CH3 §2.9).
    pub queue: Arc<CheckQueue>,
    /// The process's one `ScriptCaches`, shared with the mempool (CH3 D10).
    pub caches: Arc<ScriptCaches>,
    pub helper: Box<dyn ChainHelper>,
    /// V's abort path (CH3 §2.1.1).
    pub abort: AbortPath,
    /// `-assumevalid`; `None` for 0.
    pub assume_valid: Option<BlockHash>,
    /// Records each attempt (CH3 §2.15, O8 §8.1); `None` records nothing.
    pub digest: Option<Box<dyn DigestSink>>,
    /// `-consensusxcheck` ([`SerialCore::set_xcheck`]): the node's flag, via `CoinsSetup::xcheck`
    /// (O9b S1); on in the L2 replay and `NodeSut` (CH3a D189).
    pub xcheck: bool,
}

impl<E: KvEngine + 'static, F: FlatFs, H: EvoHooks> CoreConnector<E, F, H> {
    /// # Errors
    ///
    /// [`Fatal`] for parameters the core cannot use.
    pub fn new(
        params: &ChainParams,
        parts: ConnectorParts<E, F>,
        hooks: H,
        facade: Arc<H::Facade>,
    ) -> Result<Self, Fatal> {
        let latch = parts.abort.latch().clone();
        let mut core = SerialCore::new(
            params,
            parts.caches,
            parts.queue,
            parts.helper,
            parts.abort,
            parts.assume_valid,
        )?;
        core.set_xcheck(parts.xcheck);
        Ok(CoreConnector {
            coins: parts.coins,
            files: parts.files,
            hooks,
            facade,
            core,
            latch,
            digest: parts.digest,
        })
    }

    /// Arms a planted bug on the core ([`SerialCore::plant`]).
    #[cfg(feature = "planted-bugs")]
    pub fn plant(&mut self, bug: Option<crate::Planted>) {
        self.core.plant(bug);
    }

    /// The last block's streamed `CBlockUndo` (tests compare it with CS5a's encoder).
    #[cfg(test)]
    pub(crate) fn last_undo(&self) -> &[u8] {
        self.core.last_undo()
    }

    /// The last block's script work: how many transactions built their signature-hash data, and
    /// how many inputs had their spent script copied.
    #[cfg(test)]
    pub(crate) fn last_script_work(&self) -> (usize, usize) {
        self.core.last_script_work()
    }
}

/// The attempt's outcome once `finish_attempt` has applied its log (CH3a D75): an outcome
/// already reported keeps its kind, so V still emits the `BlockChecked` of a system error
/// (v:3431-3432), and the apply failure is logged; after `Connected` or `Invalid` the apply
/// failure is the outcome.
pub(super) fn finished_outcome(
    out: Result<ConnectOutcome, Fatal>,
    finished: Result<(), Fatal>,
) -> Result<ConnectOutcome, Fatal> {
    match (out, finished) {
        (out, Ok(())) => out,
        (out @ (Ok(ConnectOutcome::SystemError(_)) | Err(_)), Err(f)) => {
            tracing::error!("finish_attempt: {}", f.0);
            out
        }
        (Ok(_), Err(f)) => Err(f),
    }
}

/// O8 §8.3 as O8's patch has it (CH3a D137): a failed record fails the recording, never the
/// attempt. The patch records inside the `noexcept` `AttemptScope::Guard`, where a failure
/// reaches `Fail` (through `AttemptScope::Failed`, or from `WriteLine`): an `AbortNode` that
/// starts the shutdown. The patched `ConnectBlock` and `DisconnectBlock` return their own
/// outcome after it ("a recorder failure from here on only fails the recording"). So the
/// failure is reported through V's abort path, which trips the node latch, and the attempt's
/// outcome goes on unchanged: its commit, `InvalidBlockFound` or flush runs, and V stops on the
/// latch after it.
fn report_failed_record(core: &SerialCore, recorded: Result<(), Fatal>) {
    if let Err(f) = recorded {
        core.abort.abort_node(&f.0, None);
    }
}

/// The connect attempt's record (O8 §8.1), once its outcome is decided and before the commit:
/// O8's patch writes its `C` or `R` as `ConnectBlock` returns, before `ConnectTip` flushes the
/// view (`validation.cpp:3446-3448`).
#[expect(clippy::too_many_arguments, reason = "the attempt's parts, as ConnectAttempt holds them")]
fn record_connect(
    digest: &mut Option<Box<dyn DigestSink>>,
    core: &SerialCore,
    cx: &ConnectCx<'_>,
    block: &Block,
    at: &BlockAt,
    just_check: bool,
    active: bool,
    out: &Result<ConnectOutcome, Fatal>,
) {
    let Some(sink) = digest else {
        return;
    };
    // A `C`: its undo is the one this connect streamed (genesis's returns before, `v:2534-2540`),
    // and on the active coins the coinstats index adds its subsidy.
    let connected = !just_check && matches!(out, Ok(ConnectOutcome::Connected));
    let undo = (connected && at.prev.is_some()).then(|| core.last_undo());
    let subsidy = (connected && active).then(|| core.subsidy_of(cx, block, at)).transpose();
    let recorded = subsidy.and_then(|subsidy| {
        let attempt = ConnectAttempt {
            block,
            at,
            just_check,
            active,
            outcome: out,
            undo,
            subsidy,
            script_checks: core.last_script_checks(),
        };
        sink.record(cx, &DigestAttempt::Connect(attempt))
    });
    report_failed_record(core, recorded);
}

/// The disconnect attempt's record (O8 §8.1), before the commit, as the patch writes its `D` when
/// `DisconnectBlock` returns (`DisconnectTip` flushes after, `validation.cpp:3285-3287`). A clean
/// disconnect of the active coins passes the coinstats index's rewind its undo and subsidy.
fn record_disconnect(
    digest: &mut Option<Box<dyn DigestSink>>,
    core: &SerialCore,
    cx: &ConnectCx<'_>,
    block: &Block,
    at: &BlockAt,
    active: bool,
    out: &Result<DisconnectResult, Fatal>,
) {
    let Some(sink) = digest else {
        return;
    };
    let rewinds = active && matches!(out, Ok(DisconnectResult::Ok));
    let subsidy = rewinds.then(|| core.subsidy_of(cx, block, at)).transpose();
    let recorded = subsidy.and_then(|subsidy| {
        let attempt = DisconnectAttempt {
            block,
            at,
            outcome: out,
            rewind: subsidy.map(|subsidy| Rewind {
                undo: core.read_undo(),
                subsidy,
            }),
        };
        sink.record(cx, &DigestAttempt::Disconnect(attempt))
    });
    report_failed_record(core, recorded);
}

// Core: validation.cpp:4952-4967
/// `TestBlockValidity`'s pre-checks on the dummy, in Core's order, each failure with Core's
/// `LogError` line (CH3 §2.14 step 3): the first rejection, if any.
fn pre_checks(
    checks: &dyn ProposalChecks,
    block: &Block,
    prev: BlockId,
    check_pow: bool,
    check_merkle: bool,
) -> Result<Option<BlockValidationError>, Fatal> {
    // NOTE: CheckBlockHeader is called by CheckBlock
    if let Err(err) = checks.contextual_check_block_header(block, prev)? {
        tracing::error!("TestBlockValidity: Consensus::ContextualCheckBlockHeader: {err}");
        return Ok(Some(err));
    }
    if let Err(err) = checks.check_block(block, check_pow, check_merkle) {
        tracing::error!("TestBlockValidity: Consensus::CheckBlock: {err}");
        return Ok(Some(err));
    }
    if let Err(err) = checks.contextual_check_block(block, prev) {
        tracing::error!("TestBlockValidity: Consensus::ContextualCheckBlock: {err}");
        return Ok(Some(err));
    }
    Ok(None)
}

/// The active cell's guard: the V-only leaf (D50) is never contended, so a held or poisoned
/// mutex is a wiring error.
pub(crate) fn lock<E: KvEngine>(
    cell: &Mutex<CoinsCache<E>>,
) -> Result<std::sync::MutexGuard<'_, CoinsCache<E>>, Fatal> {
    cell.try_lock().map_err(|e| match e {
        TryLockError::WouldBlock => Fatal::new("the coins cell is held outside V's leaf rule"),
        TryLockError::Poisoned(_) => Fatal::new("the coins cell is poisoned"),
    })
}

impl<E: KvEngine + 'static, F: FlatFs, H: EvoHooks> BlockConnector for CoreConnector<E, F, H> {
    // Core: validation.cpp:3423-3448 (ConnectTip's view, ConnectBlock, the flush and the evo
    // commit), as CH3 §3.2's bracket
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        mut target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        // What can fail before the attempt opens is resolved first; between `begin_attempt` and
        // `finish_attempt` every failure is a value (D33, CH3a D74).
        let e =
            cx.reader.entry(id).ok_or_else(|| Fatal::new("ConnectBlock: an unindexed block"))?;
        let best = BlockRef {
            id,
            hash: e.hash,
            height: e.height,
        };
        if let ViewTarget::Scratch(view) = &mut target
            && !view.split().0.is::<E>()
        {
            return Err(Fatal::new("a scratch view over another engine"));
        }
        let active = matches!(target, ViewTarget::Active);
        let facade = Arc::clone(&self.facade);
        let mut att =
            self.hooks.begin_attempt(&facade, v, cx, AttemptId::next(), cx.origin, &target)?;
        let unwind = AttemptUnwind(Some(self.latch.clone()));
        let (scratch, mut evo) = target.split();
        // The Active cell after L3 (CH3 §5.2: L-cell is never held while taking it), held until
        // the overlay drops.
        let mut held = None;
        let mut ov = match scratch {
            Some(s) => s.overlay::<E>().ok_or_else(|| Fatal::new("a scratch view changed engine")),
            None => lock(&self.coins).map(|g| BlockOverlay::new(&mut **held.insert(g))),
        };
        // Whether `ConnectBlock` ran, so the attempt has a record (O8 §8.1).
        let mut ran = false;
        // H10 opens over a view only (Opus a3 r1 F4): a failed lock or scratch overlay never
        // begins the evo transaction, so there is none to roll back below.
        let out = match &mut ov {
            Ok(ov) => self.hooks.begin_tx(&mut att, &mut evo).and_then(|()| {
                let mut steps = HookSteps::<H> {
                    hooks: &mut self.hooks,
                    att: &mut att,
                    evo: &mut evo,
                    updates: H::ListUpdates::default(),
                };
                ran = true;
                self.core.connect(&mut steps, cx, ov, &self.files, block, checked, id, st)
            }),
            Err(f) => Err(f.clone()),
        };
        // D33: the log is applied on every outcome, before V acts on it and before the commit.
        let finished = self.hooks.finish_attempt(att);
        // D35: the attempt's removed view ends with it.
        self.core.end_attempt();
        let out = finished_outcome(out, finished);
        if ran && self.digest.is_some() {
            let at = BlockAt {
                hash: e.hash,
                height: e.height,
                prev: e.prev,
                id: Some(id),
            };
            record_connect(&mut self.digest, &self.core, cx, block, &at, false, active, &out);
        }
        let out = match (out, ov) {
            (Ok(ConnectOutcome::Connected), Ok(ov)) => {
                let committed = ov.commit(best).map_err(coins_failed);
                match committed.and_then(|()| self.hooks.commit_tx(&mut evo)) {
                    Ok(()) => Ok(ConnectOutcome::Connected),
                    Err(f) => {
                        self.hooks.rollback_tx(&mut evo);
                        Err(f)
                    }
                }
            }
            // `begin_tx` ran, and failed or the block did not connect (D75).
            (out, Ok(_)) => {
                self.hooks.rollback_tx(&mut evo);
                out
            }
            (out, Err(_)) => out,
        };
        // D71: armed through the commit (Opus a3 r1 F3).
        unwind.disarm();
        out
    }

    // Core: validation.cpp:3276-3288 (DisconnectTip's view, DisconnectBlock, the flush and the
    // evo commit), as CH3 §3.2's bracket; with a scratch target, VerifyDB's DisconnectBlock into
    // its own view (:5112)
    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        mut target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        // As for a connect, what can fail before the attempt opens is resolved first (D74).
        let e =
            cx.reader.entry(id).ok_or_else(|| Fatal::new("DisconnectBlock: an unindexed block"))?;
        let prev = e.prev.ok_or_else(|| Fatal::new("DisconnectBlock: genesis"))?;
        let parent = cx
            .reader
            .entry(prev)
            .ok_or_else(|| Fatal::new("DisconnectBlock: an unindexed parent"))?;
        let best = BlockRef {
            id: prev,
            hash: parent.hash,
            height: parent.height,
        };
        if let ViewTarget::Scratch(view) = &mut target
            && !view.split().0.is::<E>()
        {
            return Err(Fatal::new("a scratch view over another engine"));
        }
        let tip = matches!(target, ViewTarget::Active);
        let facade = Arc::clone(&self.facade);
        let mut att =
            self.hooks.begin_attempt(&facade, v, cx, AttemptId::next(), cx.origin, &target)?;
        let unwind = AttemptUnwind(Some(self.latch.clone()));
        let (scratch, mut evo) = target.split();
        let mut held = None;
        let mut ov = match scratch {
            Some(s) => s.overlay::<E>().ok_or_else(|| Fatal::new("a scratch view changed engine")),
            None => lock(&self.coins).map(|g| BlockOverlay::new(&mut **held.insert(g))),
        };
        // Whether `DisconnectBlock` ran, so the attempt has a record (O8 §8.1).
        let mut ran = false;
        let out = match &mut ov {
            Ok(ov) => self.hooks.begin_tx(&mut att, &mut evo).and_then(|()| {
                let mut steps = HookSteps::<H> {
                    hooks: &mut self.hooks,
                    att: &mut att,
                    evo: &mut evo,
                    updates: H::ListUpdates::default(),
                };
                ran = true;
                self.core.disconnect(&mut steps, cx, ov, &self.files, block, id, tip)
            }),
            Err(f) => Err(f.clone()),
        };
        // D33: the log is applied on every outcome, before V acts on it and before the commit.
        let finished = self.hooks.finish_attempt(att);
        self.core.end_attempt();
        let out = match (out, finished) {
            (out, Ok(())) => out,
            (Err(f), Err(apply)) => {
                tracing::error!("finish_attempt: {}", apply.0);
                Err(f)
            }
            (Ok(_), Err(apply)) => Err(apply),
        };
        if ran && self.digest.is_some() {
            let at = BlockAt {
                hash: e.hash,
                height: e.height,
                prev: e.prev,
                id: Some(id),
            };
            record_disconnect(&mut self.digest, &self.core, cx, block, &at, tip, &out);
        }
        // The commit follows the target and its caller (D36, CH3a D106): DisconnectTip flushes its
        // view only after `DISCONNECT_OK` (v:3281-3287), while VerifyDB's view keeps an unclean
        // disconnect and goes on (v:5113-5122).
        let commit = match out {
            Ok(DisconnectResult::Ok) => true,
            Ok(DisconnectResult::Unclean) => !tip,
            Ok(DisconnectResult::Failed) | Err(_) => false,
        };
        let mut committed_coins = false;
        let out = match (out, ov) {
            (Ok(result), Ok(ov)) if commit => {
                let committed = ov.commit(best).map_err(coins_failed);
                committed_coins = committed.is_ok();
                match committed.and_then(|()| self.hooks.commit_tx(&mut evo)) {
                    Ok(()) => Ok(result),
                    Err(f) => {
                        self.hooks.rollback_tx(&mut evo);
                        Err(f)
                    }
                }
            }
            (out, Ok(_)) => {
                self.hooks.rollback_tx(&mut evo);
                out
            }
            (out, Err(_)) => out,
        };
        // CH2 §2.4.3 item 4: a prefetched coin must not outlive the coins' disconnect. Only the
        // active coins have a prefetch map; a scratch view has none. After the match, whose
        // overlay borrows `held` until it drops.
        if committed_coins && let Some(active) = held.as_mut() {
            active.clear_prefetch();
        }
        // D71: armed through the commit.
        unwind.disarm();
        out
    }

    // Core: validation.cpp:4923-4974 TestBlockValidity, as CH3 §2.14 and §3.2's bracket
    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        block: &Block,
        prev: BlockId,
        check_pow: bool,
        check_merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        // What can fail before the attempt opens is resolved first.
        let p = cx
            .reader
            .entry(prev)
            .ok_or_else(|| Fatal::new("TestBlockValidity: an unindexed parent"))?;
        // v:4936, v:4943-4947: the dummy index, never inserted.
        let at = BlockAt {
            hash: block.hash(),
            height: p.height + 1,
            prev: Some(prev),
            id: None,
        };
        // The EV attempt covers the whole call (CH3 §2.14, CH3a D122); its target is the active
        // coins and evo state, whose overlay is dropped and whose evo transaction rolls back.
        let mut target = ViewTarget::Active;
        let facade = Arc::clone(&self.facade);
        let mut att =
            self.hooks.begin_attempt(&facade, v, cx, AttemptId::next(), cx.origin, &target)?;
        let unwind = AttemptUnwind(Some(self.latch.clone()));
        let (_, mut evo) = target.split();
        // Whether `begin_tx` ran (so `rollback_tx` must), and whether `ConnectBlock` did (so
        // the helper's attempt scope opened, CH3a D125).
        let (mut began, mut connected) = (false, false);
        // The Active cell after L3 (CH3 §5.2), held until the attempt ends.
        let mut held = None;
        let out = (|| {
            // v:4942: `CCoinsViewCache viewNew(&CoinsTip())`, dropped.
            let mut ov = BlockOverlay::new(&mut **held.insert(lock(&self.coins)?));
            // V answered a block whose parent is not its tip before this (CH3a D123).
            if ov.best_block() != Some(p.hash) || block.header().prev_hash() != p.hash {
                return Err(Fatal::new("TestBlockValidity: the parent is not the tip"));
            }
            // v:4937-4940: V's ChainLock state, not the helper: Core asks `chainlocks` itself,
            // so the check records nothing (D16), and its class is INVALID_PREV, not
            // ConnectBlock's CHAINLOCK (v:2518).
            if cx.chainlocks.has_conflicting_chainlock(at.height, &at.hash)? {
                tracing::info!("ERROR: TestBlockValidity: conflicting with chainlock");
                return Ok(ConnectOutcome::Invalid(BlockValidationError::new(
                    BlockValidationResult::InvalidPrev,
                    "bad-chainlock",
                )));
            }
            // v:4950: the evo transaction, rolled back (H10).
            began = true;
            self.hooks.begin_tx(&mut att, &mut evo)?;
            if let Some(err) = pre_checks(checks, block, prev, check_pow, check_merkle)? {
                return Ok(ConnectOutcome::Invalid(err));
            }
            // v:4968: `fChecked` is set only by a `CheckBlock` that passed with both checks on
            // (v:4424-4425), so ConnectBlock's skips it then (CH3a D118).
            let checked = check_pow && check_merkle;
            connected = true;
            let mut steps = HookSteps::<H> {
                hooks: &mut self.hooks,
                att: &mut att,
                evo: &mut evo,
                updates: H::ListUpdates::default(),
            };
            self.core.connect_at(
                &mut steps,
                cx,
                &mut ov,
                &self.files,
                block,
                checked,
                &at,
                Finish::JustCheck,
            )
        })();
        let refused = self.core.take_refused();
        // D33: the log is applied on every outcome; nothing is committed.
        let finished = self.hooks.finish_attempt(att);
        if connected {
            self.core.end_attempt();
        }
        // D75 keeps a reported system error over a failed apply; a proposal's readiness refusal
        // is unreported (CH3a D121), so after one the failed apply is the outcome, D4's AbortNode,
        // as after a verdict.
        let out = match (out, finished) {
            (Ok(ConnectOutcome::SystemError(_)), Err(f)) if refused => Err(f),
            (out, finished) => finished_outcome(out, finished),
        };
        // O8 §4.3: a proposal's `R`, only for a `ConnectBlock` that ran (CH3a D125).
        if connected {
            record_connect(&mut self.digest, &self.core, cx, block, &at, true, true, &out);
        }
        if began {
            self.hooks.rollback_tx(&mut evo);
        }
        // D71: armed through the rollback.
        unwind.disarm();
        out
    }

    fn published_bls(&self) -> Option<BlsCtx> {
        self.hooks.published_bls()
    }

    fn evo_reads(&self) -> &dyn EvoReads {
        self.hooks.evo_reads()
    }
}

/// CH3a D71: armed from `begin_attempt` until the bracket closes (`finish_attempt`, `end_attempt`,
/// then the commit or rollback, if any); dropped armed (an unwind), it trips the node latch before
/// an open attempt drops, so nothing of the attempt is flushed: an unwind in the commit can leave
/// the active coins at the block and evo at its parent in memory, and the latch refuses every
/// chainstate write and block-file mutation after it. It reports nothing: on the tip
/// path the unwinding tip hold reports, and V's own report then finds the latch tripped.
struct AttemptUnwind(Option<GlobalFailStop>);

impl AttemptUnwind {
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for AttemptUnwind {
    fn drop(&mut self) {
        if let Some(latch) = self.0.take() {
            latch.trip(Failure {
                kind: ErrorKind::Other,
                site: "chainstate validation",
                message: "an EV attempt unwound before its commit or rollback returned".to_owned(),
            });
        }
    }
}

/// The serial core's evo side over an open attempt: each step is its hook (CH3 §3.1).
struct HookSteps<'a, 'v, 't, H: EvoHooks + 'v> {
    hooks: &'a mut H,
    att: &'a mut H::Attempt<'v>,
    evo: &'a mut EvoTarget<'t>,
    /// What H4 left for H7.
    updates: H::ListUpdates,
}

impl<'v, H: EvoHooks + 'v> AttemptSteps for HookSteps<'_, 'v, '_, H> {
    fn complete(&self) -> bool {
        H::COMPLETE
    }

    fn bls_ctx(&mut self, params: &ChainParams, at: &BlockAt) -> BlsCtx {
        self.hooks.bls_ctx(self.att, self.evo, params, at)
    }

    fn verify_best_block(&mut self, hash: &BlockHash) -> Result<bool, Fatal> {
        self.hooks.verify_best_block(self.att, self.evo, hash)
    }

    fn process_special_txs(
        &mut self,
        block: &Block,
        at: &BlockAt,
        rules: SpecialTxRules,
        view: &dyn CoinsView<Error = CoinsReadError>,
        subsidy: Amount,
        just_check: bool,
        check_cbtx_merkle_roots: bool,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
        let r = self.hooks.process_special_txs(
            self.att,
            self.evo,
            block,
            at,
            rules,
            view,
            subsidy,
            just_check,
            check_cbtx_merkle_roots,
        )?;
        Ok(r.map(|updates| self.updates = updates))
    }

    fn shared_collateral_spends(
        &mut self,
        tx: &Tx,
        view: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        self.hooks.shared_collateral_spends(self.att, tx, view)
    }

    fn mn_payment_valid(
        &mut self,
        coinbase: &Tx,
        prev: Option<BlockId>,
        subsidy: Amount,
        fees: Amount,
        era: MnRewardEra,
        strict: bool,
    ) -> Result<bool, Fatal> {
        self.hooks.mn_payment_valid(self.att, self.evo, coinbase, prev, subsidy, fees, era, strict)
    }

    fn set_best_block(&mut self, hash: &BlockHash) -> Result<(), Fatal> {
        self.hooks.set_best_block(self.att, self.evo, hash)
    }

    fn commit_bls_scheme(&mut self) {
        self.hooks.commit_bls_scheme(self.att, self.evo);
    }

    fn list_changed(&mut self, undo: bool) {
        self.hooks.list_changed(self.att, &self.updates, undo);
    }

    fn undo_special_txs(&mut self, block: &Block, at: &BlockAt) -> Result<bool, Fatal> {
        let updates = self.hooks.undo_special_txs(self.att, self.evo, block, at)?;
        Ok(updates.map(|updates| self.updates = updates).is_some())
    }

    fn sink(&mut self) -> &mut dyn EffectSink {
        self.att.sink()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D71: only an attempt that unwinds trips the latch, and the trip reports nothing.
    #[test]
    fn an_unwound_attempt_trips_the_latch() {
        let latch = GlobalFailStop::new();
        AttemptUnwind(Some(latch.clone())).disarm();
        assert!(latch.tripped().is_none());
        let unwound = std::panic::catch_unwind(|| {
            let _armed = AttemptUnwind(Some(latch.clone()));
            panic!("a hook panicked");
        });
        assert!(unwound.is_err());
        let f = latch.tripped().expect("tripped");
        assert_eq!(
            (f.site, f.message.as_str()),
            (
                "chainstate validation",
                "an EV attempt unwound before its commit or rollback returned"
            )
        );
    }
}
