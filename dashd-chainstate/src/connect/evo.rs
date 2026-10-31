//! The evo seam of the connector (CH3 §3.1, §3.2; EV addendum §2.6): [`EvoHooks`] and its
//! attempt, and [`NoEvo`], its answers where no evo rule is live.

use std::sync::atomic::{AtomicU64, Ordering};

use dash_consensus::coins::CoinsView;
use dash_consensus::ctx::{DeploymentView, bls_ctx_for_block};
use dash_consensus::payments::MnRewardEra;
use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
use dash_consensus::{
    Amount, Block, BlockId, BlockValidationError, BlockValidationResult, BlsCtx, ChainAncestry, Tx,
    TxValidationError, TxValidationResult,
};
use dash_evo::{Effect, EffectSink};
use dash_num::Hash256;
use dash_params::ChainParams;
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

use crate::chain::{
    AttemptOrigin, ConnectCx, EvoReads, EvoTarget, Fatal, NoEvoReads, VContext, ViewTarget,
};
use crate::store::CoinsReadError;

/// One EV attempt's identity (EV addendum §2.6): unique in the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AttemptId(u64);

impl AttemptId {
    /// A new identity, never handed out before.
    pub fn next() -> AttemptId {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        AttemptId(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// An open EV attempt (CH3 §3.2): every effect of one connector call goes to its one ordered
/// log, which [`EvoHooks::finish_attempt`] applies on every outcome.
pub trait EvoAttempt {
    fn id(&self) -> AttemptId;

    /// The attempt's log (A1 §2.10.1): effects in Core's order.
    fn sink(&mut self) -> &mut dyn EffectSink;
}

/// The block a connector call is about (CH3 §2.14): the connector's internals never take a bare
/// [`BlockId`]. A proposal has no `id` and is never inserted; every per-block question goes
/// through `prev`. Only genesis has no `prev`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockAt {
    /// `block.GetHash()`, nonce included (O8 §4.3).
    pub hash: BlockHash,
    pub height: u32,
    pub prev: Option<BlockId>,
    pub id: Option<BlockId>,
}

// Core: evo/specialtxman.h:49-53 SpecialTxRules; validation.cpp:6797-6803 GetSpecialTxRules
/// The special-transaction rules at a block, from the deployments after its parent (CH3 D40).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SpecialTxRules {
    /// `DEPLOYMENT_V24` active after `pprev`.
    pub v24: bool,
    /// `DEPLOYMENT_EVO_SHARES` active after `pprev`: evonodes may register with shared
    /// collateral and have multiple owner payouts.
    pub evo_shares: bool,
}

/// O8 §5.2's `cp`: the credit pool the block's child sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreditPoolDigest {
    pub locked: Amount,
    pub current_limit: Amount,
    pub lately_unlocked: Amount,
    /// `sha256d(serialize(indexes))`.
    pub indexes_hash: Hash256,
}

/// Hook H11's fields of an O8 `C` record (O8 §5.2), `None` outside their era: all of them
/// under [`NoEvo`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvoDigest {
    /// `mnl`: `CanonicalMNListHash` of the block's new list (DIP0003).
    pub mnl: Option<Hash256>,
    /// `qc`: each mined commitment the block indexed, `(type, quorumHash, commitment hash)`,
    /// ascending (DIP0003).
    pub qc: Option<Vec<(u8, Hash256, Hash256)>>,
    /// `cp` (V20).
    pub cp: Option<CreditPoolDigest>,
    /// `ehf`: the MNHF signals the block leaves behind, `(bit, height)` ascending (V20).
    pub ehf: Option<Vec<(u8, i32)>>,
}

/// The evo half of `ConnectBlock`, `DisconnectBlock` and `TestBlockValidity` (CH3 §3.1): hooks
/// H1, H2 and H4–H11 at Core's sites, with [`NoEvo`] as CH3a's implementation. CH7a implements
/// it, CH7b supplies the LLMQ parts; no hook changes the connector's control flow.
///
/// What hook H4 returns besides success (CH7 D2): Core's verdict, or a readiness refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecialTxFailure {
    /// Core's `state.Invalid`: a verdict.
    Invalid(BlockValidationError),
    /// A readiness refusal carrying the capability this build lacks (CH7 D2, D3): Core reached
    /// no verdict here, so neither does the node. The connector refuses the block as readiness
    /// guard G1 does (`refuse` with `refusal_text`), and logs no "failed" line.
    NotReady(&'static str),
}

/// Every call is one attempt (CH3 §3.2, EV addendum §2.6): the connector opens it from V's
/// context and the [`Facade`](Self::Facade) with [`begin_attempt`](Self::begin_attempt), passes
/// it to every hook, and closes it with [`finish_attempt`](Self::finish_attempt) on every
/// outcome, before V acts on the result and before anything is committed (D33). The hooks that
/// read or write evo state get the attempt's [`EvoTarget`], split from the same [`ViewTarget`]
/// as its coins (D36).
pub trait EvoHooks: Send {
    /// Every hook is implemented and its acceptance passes (CH3 §2.17). False for [`NoEvo`]:
    /// readiness guard G1 then refuses every block from the first evo deployment, and `NoEvo`'s
    /// H4 a special-version CbTx below it (CH7 D57). CH7a sets it true.
    const COMPLETE: bool;

    /// What a complete build still cannot judge in a block already in the chainstate (CH7 D3):
    /// the startup check asks it, when [`COMPLETE`](Self::COMPLETE), for the active tip and the
    /// blocks of recovery's transition. `block` is `None` for a block whose data is not held. It
    /// takes no receiver, as `COMPLETE` does: the node asks it before V starts, when no hooks
    /// exist. The default refuses nothing.
    ///
    /// # Errors
    ///
    /// A fault in the hooks' parameters or reads.
    fn startup_refusal(
        _params: &ChainParams,
        _chain: &dyn ChainAncestry,
        _deployments: &dyn DeploymentView,
        _at: &BlockAt,
        _block: Option<&Block>,
    ) -> Result<Option<&'static str>, String> {
        Ok(None)
    }

    /// What an open attempt borrows besides V's context: CH7b's process-wide `MembersFacade`,
    /// whose `begin_attempt(&'v self, &'v mut VContext, id)` returns an `AttemptHandle<'v>` (EV
    /// addendum §2.6). The connector holds it beside the hooks, never inside them, so every hook
    /// call and [`finish_attempt`](Self::finish_attempt) can borrow the hooks mutably while the
    /// handle is open. A connector that keeps it in an `Arc` field opens each attempt on a local
    /// clone: an attempt that borrowed the field would lock out every `&mut self` method (CH3a
    /// D56). `()` for [`NoEvo`].
    type Facade: Send + Sync;

    /// The open attempt: CH7a's holds the facade's `AttemptHandle`. It borrows the facade and V's
    /// context for as long as it is open, so it cannot outlive the connector call or leave V's
    /// thread, and V opens one attempt at a time; what it keeps beyond those borrows is the
    /// hooks' own. One dropped without [`finish_attempt`](Self::finish_attempt) is an unwind:
    /// slice a3's guard trips the node latch, so nothing of it is committed, and the handle's
    /// drop releases the gate (EV addendum §2.6).
    type Attempt<'v>: EvoAttempt
    where
        Self: 'v;

    /// Core's `MNListUpdates`: what hook H4 or H9 leaves for hook H7.
    type ListUpdates: Default;

    /// Opens the attempt: V's context proves the call is on V, and CH7a takes the facade's gate
    /// exclusively until [`finish_attempt`](Self::finish_attempt) (EV addendum §2.6). The
    /// attempt may keep the call's context, its chain and deployments, as CH7 §4.2.3's
    /// `AttemptBinding` does (D48). Once the
    /// coins view is open, the connector opens the target's evo transaction in it
    /// ([`begin_tx`](Self::begin_tx), H10).
    ///
    /// The attempt holds V's context, so V cannot open a second one while it is open, which
    /// would wait on the gate it holds:
    ///
    /// ```compile_fail,E0499
    /// use dashd_chainstate::chain::{AttemptOrigin, ConnectCx, VContext, ViewTarget};
    /// use dashd_chainstate::connect::{AttemptId, EvoHooks, NoEvo};
    /// fn connect(v: &mut VContext, cx: &ConnectCx<'_>) {
    ///     let (mut hooks, target) = (NoEvo, ViewTarget::Active);
    ///     let first = hooks.begin_attempt(&(), v, cx, AttemptId::next(), AttemptOrigin::Tip, &target);
    ///     let second = hooks.begin_attempt(&(), v, cx, AttemptId::next(), AttemptOrigin::Tip, &target);
    ///     drop((first, second));
    /// }
    /// ```
    ///
    /// Its passing twin, one attempt after the other:
    ///
    /// ```
    /// use dashd_chainstate::chain::{AttemptOrigin, ConnectCx, Fatal, VContext, ViewTarget};
    /// use dashd_chainstate::connect::{AttemptId, EvoHooks, NoEvo};
    /// fn connect(v: &mut VContext, cx: &ConnectCx<'_>) -> Result<(), Fatal> {
    ///     let (mut hooks, target) = (NoEvo, ViewTarget::Active);
    ///     let first = hooks.begin_attempt(&(), v, cx, AttemptId::next(), AttemptOrigin::Tip, &target)?;
    ///     hooks.finish_attempt(first)?;
    ///     let second = hooks.begin_attempt(&(), v, cx, AttemptId::next(), AttemptOrigin::Tip, &target)?;
    ///     hooks.finish_attempt(second)
    /// }
    /// ```
    fn begin_attempt<'v>(
        &mut self,
        facade: &'v Self::Facade,
        v: &'v mut VContext,
        cx: &'v ConnectCx<'_>,
        id: AttemptId,
        origin: AttemptOrigin,
        target: &ViewTarget<'_>,
    ) -> Result<Self::Attempt<'v>, Fatal>;

    /// Applies the attempt's log in emission order, then releases it (CH7a: the facade's
    /// `release_attempt` with the `EffectsApplied` its application mints, through the facade the
    /// attempt borrows). It runs on every outcome, so it takes none: effects survive invalid,
    /// system-error and `fJustCheck` attempts (A1 §2.10.1). A failure to apply is D4's
    /// `AbortNode`, and nothing is committed (D33). Its `Fatal` is unreported (`Fatal::new` or
    /// `Fatal::abort_node`): it never reports through a sink itself, as the connector drops it
    /// after an outcome that has been reported (CH3a D75).
    fn finish_attempt(&mut self, attempt: Self::Attempt<'_>) -> Result<(), Fatal>;

    /// H1: the BLS scheme for the block (`validation.cpp:2490-2491`; `ConnectTip`'s guard
    /// `:3405`, disconnect `:3272-3273`).
    fn bls_ctx(
        &mut self,
        attempt: &mut Self::Attempt<'_>,
        evo: &mut EvoTarget<'_>,
        params: &ChainParams,
        at: &BlockAt,
    ) -> BlsCtx;

    /// H1: keeps the block's scheme once it is connected (`:2916`, `:3477`; disconnect `:3320`).
    fn commit_bls_scheme(&mut self, attempt: &mut Self::Attempt<'_>, evo: &mut EvoTarget<'_>);

    /// The scheme the last committed attempt left, Core's global `bls_legacy_scheme`, which V
    /// publishes as `ChainView.bls` (CH7 D9). `None` keeps the snapshot's
    /// `bls_ctx_for_block(tip + 1)`.
    fn published_bls(&self) -> Option<BlsCtx> {
        None
    }

    /// The evo state V reads at its tip (CH7 D16, D17): `ChainView`'s list and pool,
    /// `ChainQuery::evo_view`. The default is a node with none.
    fn evo_reads(&self) -> &dyn EvoReads {
        &NoEvoReads
    }

    /// H2: `m_evoDb.VerifyBestBlock(hash)` (`:2525-2531`; disconnect `:2287-2292`).
    fn verify_best_block(
        &mut self,
        attempt: &mut Self::Attempt<'_>,
        evo: &mut EvoTarget<'_>,
        hash: &BlockHash,
    ) -> Result<bool, Fatal>;

    /// H4: `ProcessSpecialTxsInBlock` (`:2661-2678`) over the block's view. The inner `Err` is
    /// Core's `state.Invalid`, or a readiness refusal (CH7 D2).
    #[expect(
        clippy::too_many_arguments,
        reason = "Core's ProcessSpecialTxsInBlock arguments, plus the attempt and its target"
    )]
    fn process_special_txs(
        &mut self,
        attempt: &mut Self::Attempt<'_>,
        evo: &mut EvoTarget<'_>,
        block: &Block,
        at: &BlockAt,
        rules: SpecialTxRules,
        view: &dyn CoinsView<Error = CoinsReadError>,
        subsidy: Amount,
        just_check: bool,
        check_cbtx_merkle_roots: bool,
    ) -> Result<Result<Self::ListUpdates, SpecialTxFailure>, Fatal>;

    /// H5: `CMNPaymentsProcessor::IsTransactionValid` (`masternode/payments.cpp:227`, called at
    /// `:397`); `strict` is `rules.v24`. It reads the masternode list at `prev` through the
    /// target, as `VerifyDB` reads it inside its open evo transaction. When it answers false,
    /// Core's `IsBlockPayeeValid` logs "CMNPaymentsProcessor::IsBlockPayeeValid -- ERROR! Invalid
    /// masternode payment detected at height %d: %s"
    /// (`masternode/payments.cpp:399-400`) before `ConnectBlock`'s own line: CH7a's (or EV3's)
    /// implementation emits it here, in Core's order. Under `NoEvo` H5 is never reached (D63).
    #[expect(
        clippy::too_many_arguments,
        reason = "Core's IsTransactionValid arguments, plus the attempt and its target"
    )]
    fn mn_payment_valid(
        &mut self,
        attempt: &mut Self::Attempt<'_>,
        evo: &mut EvoTarget<'_>,
        coinbase: &Tx,
        prev: Option<BlockId>,
        subsidy: Amount,
        fees: Amount,
        era: MnRewardEra,
        strict: bool,
    ) -> Result<bool, Fatal>;

    /// H6: `m_evoDb.WriteBestBlock(hash)` (`:2913`; disconnect `:2366`).
    fn set_best_block(
        &mut self,
        attempt: &mut Self::Attempt<'_>,
        evo: &mut EvoTarget<'_>,
        hash: &BlockHash,
    ) -> Result<(), Fatal>;

    /// H7: `NotifyMasternodeListChanged(undo, …)` (`:2918-2921`; disconnect `:2368-2371`), under
    /// D47's ordering rule (CH3 §2.13).
    fn list_changed(
        &mut self,
        attempt: &mut Self::Attempt<'_>,
        updates: &Self::ListUpdates,
        undo: bool,
    );

    /// H8: `CheckSharedCollateralSpends(tx, view)` (`:2708-2716`), with V24 active. The inner
    /// `Err` is the transaction's invalid state.
    fn shared_collateral_spends(
        &mut self,
        attempt: &mut Self::Attempt<'_>,
        tx: &Tx,
        view: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal>;

    /// What a complete build cannot undo (CH7 D3, D70): `DisconnectBlock` asks it when
    /// [`COMPLETE`](Self::COMPLETE), before any evo state is read, where G1 refuses without it.
    /// The default refuses nothing.
    ///
    /// # Errors
    ///
    /// A fault in the hooks' parameters or reads.
    fn undo_refusal(
        &mut self,
        _attempt: &mut Self::Attempt<'_>,
        _at: &BlockAt,
    ) -> Result<Option<&'static str>, Fatal> {
        Ok(None)
    }

    /// H9: `UndoSpecialTxsInBlock` (`:2309-2313`). `None` is Core's `false`.
    fn undo_special_txs(
        &mut self,
        attempt: &mut Self::Attempt<'_>,
        evo: &mut EvoTarget<'_>,
        block: &Block,
        at: &BlockAt,
    ) -> Result<Option<Self::ListUpdates>, Fatal>;

    /// H10: the evo transaction (`BeginTransaction`, `:3428`, `:3277`), opened as the attempt
    /// opens, after its coins view: an attempt whose view failed (the Active coins' lock, a scratch
    /// overlay) begins none. Core begins it before the view (`:3428` before `:3430`, `:3277` before
    /// `:3279`), which cannot fail there; the reorder changes no verdict.
    fn begin_tx(
        &mut self,
        attempt: &mut Self::Attempt<'_>,
        evo: &mut EvoTarget<'_>,
    ) -> Result<(), Fatal>;

    /// H10: `dbTx->Commit()` (`:3448`, `:3287`), into the target only: a scratch transaction is
    /// never committed to the store (`:5047`). It runs once `finish_attempt` has closed the
    /// attempt (D33), so it takes none. A panic here trips the node latch (CH3a D71).
    fn commit_tx(&mut self, evo: &mut EvoTarget<'_>) -> Result<(), Fatal>;

    /// H10: the transaction dropped uncommitted (`TestBlockValidity`, a failed attempt), after
    /// `finish_attempt`: for each [`begin_tx`](Self::begin_tx) not committed, one that failed or
    /// whose commit failed included, on every path that returns. An attempt that began no
    /// transaction rolls none back. On an unwind neither this nor `commit_tx` runs: the guard
    /// trips the node latch (CH3a D71) and the hooks are not used again.
    fn rollback_tx(&mut self, evo: &mut EvoTarget<'_>);

    /// H11: O8 §5.2's evo fields of the block's `C` record, read through the target. A field
    /// outside its era is `None`; a read that fails is D4's `Fatal`, never an absent field
    /// (CH3a D57).
    fn digest_fields(
        &mut self,
        attempt: &mut Self::Attempt<'_>,
        evo: &mut EvoTarget<'_>,
        at: &BlockAt,
    ) -> Result<EvoDigest, Fatal>;
}

/// The evo hooks where no evo rule is live (CH3 §3.1's `NoEvo` column): exact only below DIP0003,
/// V20, MN_RR, V24 and EVO_SHARES, which readiness guard G1 enforces from slice a3 (CH3 §2.17),
/// and refusing a special-version CbTx there, which it cannot judge (CH7 D57).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoEvo;

/// [`NoEvo`]'s attempt: its log, and V's context for as long as it is open.
pub struct NoEvoAttempt<'v> {
    id: AttemptId,
    log: Vec<Effect>,
    _v: &'v mut VContext,
}

impl EvoAttempt for NoEvoAttempt<'_> {
    fn id(&self) -> AttemptId {
        self.id
    }

    fn sink(&mut self) -> &mut dyn EffectSink {
        &mut self.log
    }
}

impl EvoHooks for NoEvo {
    const COMPLETE: bool = false;

    type Facade = ();

    type Attempt<'v> = NoEvoAttempt<'v>;

    type ListUpdates = ();

    fn begin_attempt<'v>(
        &mut self,
        _: &'v (),
        v: &'v mut VContext,
        _: &'v ConnectCx<'_>,
        id: AttemptId,
        _: AttemptOrigin,
        _: &ViewTarget<'_>,
    ) -> Result<NoEvoAttempt<'v>, Fatal> {
        Ok(NoEvoAttempt {
            id,
            log: Vec::new(),
            _v: v,
        })
    }

    fn finish_attempt(&mut self, attempt: NoEvoAttempt<'_>) -> Result<(), Fatal> {
        no_evo::finish(Some(attempt.id), &attempt.log)
    }

    fn bls_ctx(
        &mut self,
        _: &mut NoEvoAttempt<'_>,
        _: &mut EvoTarget<'_>,
        params: &ChainParams,
        at: &BlockAt,
    ) -> BlsCtx {
        no_evo::bls_ctx(params, at)
    }

    fn commit_bls_scheme(&mut self, _: &mut NoEvoAttempt<'_>, _: &mut EvoTarget<'_>) {}

    fn verify_best_block(
        &mut self,
        _: &mut NoEvoAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &BlockHash,
    ) -> Result<bool, Fatal> {
        no_evo::verify_best_block()
    }

    /// Core's two live checks below DIP0003, in Core's order ([`pre_dip3_special_txs`]).
    fn process_special_txs(
        &mut self,
        _: &mut NoEvoAttempt<'_>,
        _: &mut EvoTarget<'_>,
        block: &Block,
        _: &BlockAt,
        _: SpecialTxRules,
        _: &dyn CoinsView<Error = CoinsReadError>,
        _: Amount,
        _: bool,
        check_cbtx_merkle_roots: bool,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
        Ok(pre_dip3_special_txs(block, check_cbtx_merkle_roots))
    }

    fn mn_payment_valid(
        &mut self,
        _: &mut NoEvoAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &Tx,
        _: Option<BlockId>,
        _: Amount,
        _: Amount,
        _: MnRewardEra,
        _: bool,
    ) -> Result<bool, Fatal> {
        no_evo::mn_payment_valid()
    }

    fn set_best_block(
        &mut self,
        _: &mut NoEvoAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &BlockHash,
    ) -> Result<(), Fatal> {
        no_evo::set_best_block()
    }

    fn list_changed(&mut self, _: &mut NoEvoAttempt<'_>, _: &(), _: bool) {}

    fn shared_collateral_spends(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        _: &Tx,
        _: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        no_evo::shared_collateral_spends(Some(attempt.id))
    }

    fn undo_special_txs(
        &mut self,
        _: &mut NoEvoAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &Block,
        _: &BlockAt,
    ) -> Result<Option<()>, Fatal> {
        Ok(Some(()))
    }

    fn begin_tx(&mut self, _: &mut NoEvoAttempt<'_>, _: &mut EvoTarget<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn commit_tx(&mut self, _: &mut EvoTarget<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn rollback_tx(&mut self, _: &mut EvoTarget<'_>) {}

    fn digest_fields(
        &mut self,
        _: &mut NoEvoAttempt<'_>,
        _: &mut EvoTarget<'_>,
        _: &BlockAt,
    ) -> Result<EvoDigest, Fatal> {
        Ok(EvoDigest::default())
    }
}

/// [`NoEvo`]'s answers, which `bench::NoEvoSteps` gives too, so the connect bench and the CRASH
/// core mode get production's answers (CH3a D91, D95; Opus a3 r1 F5). H4's is
/// [`pre_dip3_special_txs`]; H1's commit and H7 do nothing. A refusal names the attempt where the
/// caller has one.
pub(super) mod no_evo {
    use super::*;

    /// No effect has an applier here: the one CH3a emits, `RemoveConflictingIsLock`, belongs to
    /// the InstantSend owner, and `NodeHelper` never reports a conflict at M3. An effect is
    /// therefore refused rather than dropped. A test that drives the IS-conflict loop to a removal
    /// (slice a3) needs evo hooks with an applier, not `NoEvo`.
    pub fn finish(attempt: Option<AttemptId>, log: &[Effect]) -> Result<(), Fatal> {
        match log.first() {
            None => Ok(()),
            Some(e) => Err(refusal(
                attempt,
                &format!("no applier for a {:?} effect without its owner", e.kind()),
            )),
        }
    }

    /// H1.
    pub fn bls_ctx(params: &ChainParams, at: &BlockAt) -> BlsCtx {
        bls_ctx_for_block(params, at.height)
    }

    /// H2.
    pub fn verify_best_block() -> Result<bool, Fatal> {
        Ok(true)
    }

    /// H5. Unreachable: the connector asks H5 of a complete `EvoHooks` only, and refuses the block
    /// otherwise (CH3a D63). Reached anyway, it fails closed.
    pub fn mn_payment_valid() -> Result<bool, Fatal> {
        Err(Fatal::new("IsTransactionValid without evo (refused before H5, CH3a D63)"))
    }

    /// H6.
    pub fn set_best_block() -> Result<(), Fatal> {
        Ok(())
    }

    /// H8. Unreachable: V24 is a G1 refusal. Reached anyway, it fails closed.
    pub fn shared_collateral_spends(
        attempt: Option<AttemptId>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        Err(refusal(attempt, "CheckSharedCollateralSpends without evo (V24 is refused by G1)"))
    }

    fn refusal(attempt: Option<AttemptId>, text: &str) -> Fatal {
        Fatal::new(match attempt {
            Some(id) => format!("attempt {}: {text}", id.get()),
            None => text.to_owned(),
        })
    }
}

// Core: evo/specialtxman.cpp:816-832 (the top-level CbTx check), :863-875 with :222-228 (the
// special-transaction gate); validation.cpp:2661-2668 calls it
/// `ProcessSpecialTxsInBlock`'s two live checks below DIP0003 (CH3 §2.17, D32, D54), the whole
/// function there once readiness guard G1 holds, in Core's order, but for a special-version CbTx,
/// which `NoEvo` refuses (CH7 D57). Every type test reads the raw `nType`, never the typed
/// coinbase variant, which exists only at version 3 and above.
///
/// - With `check_cbtx_merkle_roots` (`fScriptChecks`), a `vtx[0]` whose raw type is
///   `TRANSACTION_COINBASE`, at any version ([`pre_dip3_cbtx`]).
/// - Then every transaction but a raw-type-5 `vtx[0]`: one with an extra payload field
///   (`nVersion >= 3 && nType != 0`) is `bad-tx-type-dip3-inactive`, `BLOCK_CONSENSUS`, with
///   "Special Transaction check failed (tx hash <txid>) " (`TX_BAD_SPECIAL` carries no debug).
///
/// # Errors
///
/// Core's reject, or [`SPECIAL_VERSION_CBTX`]'s refusal.
pub fn pre_dip3_special_txs(
    block: &Block,
    check_cbtx_merkle_roots: bool,
) -> Result<(), SpecialTxFailure> {
    if check_cbtx_merkle_roots {
        pre_dip3_cbtx(block)?;
    }
    for (i, tx) in block.txs().iter().enumerate() {
        // we validated CCbTx above, starts from the 2nd transaction
        if i == 0 && is_raw_cbtx(tx) {
            continue;
        }
        pre_dip3_special_tx(tx).map_err(SpecialTxFailure::Invalid)?;
    }
    Ok(())
}

fn is_raw_cbtx(tx: &Tx) -> bool {
    tx.tx_type().to_base() == TRANSACTION_COINBASE
}

/// What `NoEvo` lacks to judge a special-version CbTx below DIP0003 (CH7 D57): Core decodes it
/// and runs `CheckCbTx`, the credit-pool diff and the merkle-root checks (`NodeEvo` does).
pub const SPECIAL_VERSION_CBTX: &str = "evo validation (CH7a) for a special-version coinbase";

/// [`pre_dip3_special_txs`]' CbTx check (`evo/specialtxman.cpp:816-832`), run with
/// `fCheckCbTxMerkleRoots`: nothing unless `vtx[0]`'s raw type is `TRANSACTION_COINBASE`. Not a
/// coinbase is `bad-cbtx-invalid` (unreachable after `CheckBlock`'s `bad-cb-missing`); below
/// version 3 Core reads no payload, so `GetTxPayload` fails: `bad-cbtx-payload`. Both
/// `BLOCK_CONSENSUS` with no debug message.
///
/// # Errors
///
/// Those rejects, or [`SPECIAL_VERSION_CBTX`]'s refusal for a version-3 or later coinbase, whose
/// payload Core decodes.
pub fn pre_dip3_cbtx(block: &Block) -> Result<(), SpecialTxFailure> {
    let Some(tx) = block.txs().first().filter(|tx| is_raw_cbtx(tx)) else {
        return Ok(());
    };
    if !tx.is_coinbase() {
        return Err(SpecialTxFailure::Invalid(BlockValidationError::new(
            BlockValidationResult::Consensus,
            "bad-cbtx-invalid",
        )));
    }
    if tx.version() >= 3 {
        return Err(SpecialTxFailure::NotReady(SPECIAL_VERSION_CBTX));
    }
    Err(SpecialTxFailure::Invalid(BlockValidationError::new(
        BlockValidationResult::Consensus,
        "bad-cbtx-payload",
    )))
}

/// [`pre_dip3_special_txs`]' check of one transaction (`evo/specialtxman.cpp:863-875`, `:222-228`)
/// below DIP0003, which G1 guarantees: `CheckSpecialTxInner` refuses an extra payload field.
///
/// # Errors
///
/// `bad-tx-type-dip3-inactive`, with Core's debug message.
#[inline]
pub fn pre_dip3_special_tx(tx: &Tx) -> Result<(), BlockValidationError> {
    // CheckSpecialTxInner: !DeploymentActiveAfter(pindexPrev, DIP0003), which G1 guarantees.
    if !tx.has_extra_payload_field() {
        return Ok(());
    }
    Err(dip3_inactive(tx))
}

/// [`pre_dip3_special_tx`]'s refusal, kept out of the per-transaction loop (DEC-620).
#[cold]
fn dip3_inactive(tx: &Tx) -> BlockValidationError {
    let tx_state =
        TxValidationError::new(TxValidationResult::BadSpecial, "bad-tx-type-dip3-inactive");
    tx_reject("Special Transaction check failed", tx, &tx_state)
}

/// Step 4's block reject for `tx`'s (`stm:941-945`, `:955-958`): BLOCK_CONSENSUS with the
/// transaction's reason, and `what` with its hash and debug message.
pub(crate) fn tx_reject(what: &str, tx: &Tx, tx_state: &TxValidationError) -> BlockValidationError {
    BlockValidationError::new(BlockValidationResult::Consensus, tx_state.reject_reason().to_owned())
        .with_debug(format!("{what} (tx hash {}) {}", tx.txid(), tx_state.debug_message()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_evo_is_incomplete() {
        const { assert!(!NoEvo::COMPLETE) };
    }

    /// D54 below assumevalid (CH3 §2.17): without `fCheckCbTxMerkleRoots` Core runs no CbTx check
    /// (`evo/specialtxman.cpp:816`) and its loop skips a raw-type-5 `vtx[0]` (`:865`), so every
    /// coinbase with raw type 5 passes; with it, versions 1 and 2 are `bad-cbtx-payload` and
    /// `NoEvo` refuses version 3 (D57).
    #[test]
    fn a_raw_type_5_coinbase_passes_without_cbtx_checks() {
        use dash_consensus::primitives::Header;
        use dash_consensus::primitives::{OutPoint, Transaction, TxIn, TxOut};
        use dash_primitives::{BlockHeader, TxHash, TxType};
        for version in [1, 2, 3] {
            let cb = Transaction {
                version,
                tx_type: TxType::new(TRANSACTION_COINBASE),
                inputs: vec![TxIn {
                    prevout: OutPoint {
                        hash: TxHash::from([0; 32]),
                        index: u32::MAX,
                    },
                    script_sig: vec![1, 1].into(),
                    sequence: u32::MAX,
                }],
                outputs: vec![TxOut {
                    value: 1,
                    script_pubkey: vec![0x51].into(),
                }],
                lock_time: 0,
                extra_payload: Vec::new(),
            };
            let block = Block::new(
                Header::new(BlockHeader::default()),
                vec![std::sync::Arc::new(Tx::new(cb))],
            );
            assert_eq!(pre_dip3_special_txs(&block, false), Ok(()), "version {version}");
            let want = if version < 3 {
                SpecialTxFailure::Invalid(BlockValidationError::new(
                    BlockValidationResult::Consensus,
                    "bad-cbtx-payload",
                ))
            } else {
                SpecialTxFailure::NotReady(SPECIAL_VERSION_CBTX)
            };
            assert_eq!(pre_dip3_special_txs(&block, true), Err(want), "version {version}");
        }
    }

    #[test]
    fn attempt_ids_are_unique_and_increasing() {
        let (a, b) = (AttemptId::next(), AttemptId::next());
        assert!(a < b);
    }
}
