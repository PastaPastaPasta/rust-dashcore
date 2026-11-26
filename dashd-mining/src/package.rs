//! Core's `TestPackageTransactions` and the Dash package rules of `addPackageTxs` (Core v24.0.2
//! src/node/miner.cpp:392-439, 514-531, 642-798): [`PackageRules`], the real
//! [`PackageChecks`](crate::PackageChecks).
//!
//! Every V read goes through [`TipReads`], which takes the coins (and the LLMQ view) for one
//! check and releases them before the IS and ChainLock hooks run. R6–R8 read the tip list the
//! caller read before the pool's read lock. A read fault stops the rules: every later check
//! fails, and [`PackageRules::finish`] returns it.

#[cfg(test)]
pub(crate) mod tests;

use std::cell::OnceCell;
use std::collections::HashSet;
use std::fmt::Debug;
use std::sync::Arc;

use dash_consensus::coins::CoinsView;
use dash_consensus::deployments::EhfSignals;
use dash_consensus::primitives::{Tx, TxRef};
use dash_consensus::tx::is_final_tx;
use dash_consensus::validation::TxValidationError;
use dash_dml::creditpool::{CreditPool, CreditPoolDiff};
use dash_dml::mnhf::ehf_signal;
use dash_dml::specialtx::{
    SpecialTxFault, shared_collateral_spends, shared_collateral_template_outputs,
};
use dash_dml::{DmlBuildError, DmlMemberSource, MnList, MnListBuilder, NoTrace};
use dash_evo::types::ProTxHash;
use dash_primitives::{SpecialPayload, TxHash, TxType};

use crate::evo::{BlockError, read_fault};
use crate::select::{PackageChecks, set_entries};
use crate::view::{EntryId, MiningSafety, PackageView};

/// `CChainstateHelper::IsInstantSendEnabled` and `IsInstantSendLocked`, as dashd-mempool's
/// `IsAcceptHooks` answers them. The node implements it over those hooks (dashd-mining does not
/// depend on dashd-mempool). Answers its own state; never enters V or the pool lock.
pub trait InstantSendHooks: Send + Sync {
    fn is_enabled(&self) -> bool;
    fn is_locked(&self, txid: &TxHash) -> bool;
}

/// InstantSend disabled: nothing is locked, and `IsTxSafeForMining` is never asked.
#[derive(Clone, Copy, Debug, Default)]
pub struct InstantSendDisabled;

impl InstantSendHooks for InstantSendDisabled {
    fn is_enabled(&self) -> bool {
        false
    }

    fn is_locked(&self, _: &TxHash) -> bool {
        false
    }
}

/// What the package rules read of V's tip in the template's turn. Each call takes what it reads
/// for that call only, so no coins guard is held while an IS or ChainLock hook runs.
pub trait TipReads {
    type CoinsError: Debug;

    /// `CheckSpecialTx(tx, Tip(), GetSpecialTxRules(Tip()), CoinsTip(), check_sigs, state)`.
    /// `check_sigs=false` still verifies `CheckMNHFTx`'s quorum signature, as Core does (Core
    /// v24.0.2 src/node/miner.cpp:401-408).
    ///
    /// # Errors
    ///
    /// A read fault, which ends the template.
    fn check_special_tx(
        &self,
        tx: &Tx,
        check_sigs: bool,
    ) -> Result<Result<(), TxValidationError>, BlockError>;

    /// `f` over `CoinsTip()`.
    ///
    /// # Errors
    ///
    /// The coins cell unavailable.
    fn with_tip_coins<R>(
        &self,
        f: impl FnOnce(&dyn CoinsView<Error = Self::CoinsError>) -> R,
    ) -> Result<R, BlockError>;

    /// `f` over `CoinsTip()` and the template's quorum members, the coins cell first: what
    /// `CMNListBuilder::ApplyTx` reads.
    ///
    /// # Errors
    ///
    /// The coins cell or the LLMQ view unavailable.
    fn with_list_inputs<R>(
        &self,
        f: impl FnOnce(&dyn CoinsView<Error = Self::CoinsError>, &DmlMemberSource<'_>) -> R,
    ) -> Result<R, BlockError>;
}

/// What `addPackageTxs` sets up once (Core v24.0.2 src/node/miner.cpp:514-531), and what
/// `TestPackageTransactions` reads.
pub struct PackageInputs<'r, 'b, T> {
    pub tip: &'r T,
    pub instant_send: &'r dyn InstantSendHooks,
    pub safety: &'r dyn MiningSafety,
    /// `GetListAtChainTip()`, read before the pool's read lock.
    pub dmn_tip_list: &'r MnList,
    /// `nHeight`.
    pub height: u32,
    /// `m_lock_time_cutoff`: the tip's median time past.
    pub lock_time_cutoff: i64,
    /// `GetSpecialTxRules(Tip()).v24`.
    pub v24: bool,
    /// `GetCreditPool(pindexPrev)` where V20 is active after the tip, else `None`.
    pub credit_pool: Option<CreditPool>,
    /// `GetSignalsStage(pindexPrev)`, taken by [`PackageRules::new`].
    pub signals: EhfSignals,
    /// `CreateNewBlock`'s `CMNListBuilder`, where DIP0003 is active after the tip.
    pub builder: Option<MnListBuilder<'b, 'b>>,
}

/// The Dash package rules over one template's selection.
pub struct PackageRules<'r, 'b, T> {
    inputs: PackageInputs<'r, 'b, T>,
    /// `creditPoolDiff`, with a platform reward of 0.
    credit_pool: Option<CreditPoolDiff>,
    signals: SignalBits,
    /// `keyChangedMNs`.
    key_changed: HashSet<ProTxHash>,
    fault: OnceCell<BlockError>,
}

/// What a package changes once it is in the block.
pub struct AcceptedPackage<'b> {
    signals: SignalBits,
    key_changed: Vec<ProTxHash>,
    builder: Option<MnListBuilder<'b, 'b>>,
}

/// The EHF signals' version bits (`signals`, read only for duplicates).
#[derive(Clone, Copy)]
struct SignalBits([u64; 4]);

impl SignalBits {
    fn of(signals: &EhfSignals) -> Self {
        let mut bits = SignalBits([0; 4]);
        for (bit, _) in signals.iter() {
            bits.insert(bit);
        }
        bits
    }

    /// `emplace(bit, 0).second`.
    fn insert(&mut self, bit: u8) -> bool {
        let (word, mask) = (usize::from(bit / 64), 1u64 << (bit % 64));
        let new = self.0[word] & mask == 0;
        self.0[word] |= mask;
        new
    }
}

impl<'r, 'b, T: TipReads> PackageRules<'r, 'b, T> {
    // Core v24.0.2 src/node/miner.cpp:514-531
    pub fn new(mut inputs: PackageInputs<'r, 'b, T>) -> Self {
        Self {
            credit_pool: inputs.credit_pool.take().map(|pool| CreditPoolDiff::new(pool, 0)),
            signals: SignalBits::of(&std::mem::take(&mut inputs.signals)),
            inputs,
            key_changed: HashSet::new(),
            fault: OnceCell::new(),
        }
    }

    /// The list builder with every selected package applied.
    ///
    /// # Errors
    ///
    /// The first read fault: the selection is not Core's and must not be used.
    pub fn finish(self) -> Result<Option<MnListBuilder<'b, 'b>>, BlockError> {
        match self.fault.into_inner() {
            Some(fault) => Err(fault),
            None => Ok(self.inputs.builder),
        }
    }

    fn latch<R>(&self, r: Result<R, BlockError>, failed: R) -> R {
        r.unwrap_or_else(|fault| {
            let _ = self.fault.set(fault);
            failed
        })
    }

    fn faulted(&self) -> bool {
        self.fault.get().is_some()
    }

    fn test_txs<V: PackageView + ?Sized>(
        &self,
        view: &V,
        package: &[EntryId],
    ) -> Result<bool, BlockError> {
        let inputs = &self.inputs;
        for &id in package {
            let entry = view.mining_entry(id);
            let tx: &Tx = entry.tx;
            if !is_final_tx(tx, inputs.height as i32, inputs.lock_time_cutoff) {
                return Ok(false);
            }
            if tx.is_special_tx_version() && inputs.tip.check_special_tx(tx, false)?.is_err() {
                return Ok(false);
            }
            if inputs.v24 && !self.covenant(tx)? {
                return Ok(false);
            }
            if !inputs.instant_send.is_enabled() || inputs.instant_send.is_locked(&entry.txid) {
                continue;
            }
            if !tx.inputs().is_empty() && !inputs.safety.is_tx_safe_for_mining(&entry.txid) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// `CheckSharedCollateralSpends` over the pre-block `CoinsTip()`, then
    /// `CheckSharedCollateralTemplateOutputs`.
    // Core v24.0.2 src/node/miner.cpp:421-428
    fn covenant(&self, tx: &Tx) -> Result<bool, BlockError> {
        let spends = self.inputs.tip.with_tip_coins(|coins| shared_collateral_spends(tx, coins))?;
        let spends = spends.map_err(|fault| match fault {
            SpecialTxFault::Coins(e) => {
                BlockError::Fault(format!("CheckSharedCollateralSpends: coins read: {e:?}"))
            }
            fault => BlockError::Fault(format!("CheckSharedCollateralSpends: {fault:?}")),
        })?;
        Ok(spends.is_ok() && shared_collateral_template_outputs(tx, tx.payload()).is_ok())
    }

    /// The per-entry rules, the list trial and the credit pool over the sorted package; `None`
    /// skips it, with Core's log line.
    // Core v24.0.2 src/node/miner.cpp:710-790
    fn validate<V: PackageView + ?Sized>(
        &mut self,
        view: &V,
        sorted: &[EntryId],
        candidate: EntryId,
    ) -> Result<Option<AcceptedPackage<'b>>, BlockError> {
        let mut signals = self.signals;
        let mut key_changed = Vec::new();
        let mut credit_pool_txs: Vec<TxRef> = Vec::new();
        for &id in sorted {
            let entry = view.mining_entry(id);
            let tx: &Tx = entry.tx;
            if let Some(target) = service_update_target(tx)
                && (self.key_changed.contains(&target) || key_changed.contains(&target))
            {
                tracing::info!(
                    "addPackageTxs: package tx {} skipped, it would follow a key change of \
                     masternode {target}",
                    entry.txid
                );
                return Ok(None);
            }
            if let Some(target) = view.key_change_target(id, self.inputs.dmn_tip_list) {
                key_changed.push(target);
            }
            if let Some(bit) = ehf_signal(tx)
                && !signals.insert(bit)
            {
                tracing::info!(
                    "addPackageTxs: package tx {} skipped due to duplicate EHF signal {bit}",
                    entry.txid
                );
                return Ok(None);
            }
            if tx.is_special_tx_version()
                && matches!(tx.tx_type(), TxType::AssetLock | TxType::AssetUnlock)
            {
                if tx.is_asset_unlock_with_stable_txid()
                    && let Err(state) = self.inputs.tip.check_special_tx(tx, true)?
                {
                    tracing::info!(
                        "addPackageTxs: package tx {} skipped, asset unlock instance not \
                         currently minable: {state}",
                        entry.txid
                    );
                    return Ok(None);
                }
                credit_pool_txs.push(Arc::clone(entry.tx));
            }
        }

        let special = sorted.iter().any(|&id| view.mining_entry(id).tx.is_special_tx_version());
        let builder = match &self.inputs.builder {
            Some(builder) if special => {
                let mut trial = builder.clone();
                if !self.list_trial(view, sorted, &mut trial)? {
                    return Ok(None);
                }
                Some(trial)
            }
            _ => None,
        };

        if let Some(diff) = &mut self.credit_pool
            && !credit_pool_txs.is_empty()
            && let Err(state) = diff.process_lock_unlock_transactions(&credit_pool_txs)
        {
            tracing::info!(
                "addPackageTxs: package tx {} skipped due to credit pool state: {state}",
                view.mining_entry(candidate).txid
            );
            return Ok(None);
        }
        Ok(Some(AcceptedPackage {
            signals,
            key_changed,
            builder,
        }))
    }

    /// Every sorted entry applied to `trial`, a copy of the list as built so far; whether all
    /// apply. `AddMN`/`UpdateMN`'s exception is caught as Core catches it; a store error is a
    /// read fault.
    // Core v24.0.2 src/node/miner.cpp:759-781
    fn list_trial<V: PackageView + ?Sized>(
        &self,
        view: &V,
        sorted: &[EntryId],
        trial: &mut MnListBuilder<'b, 'b>,
    ) -> Result<bool, BlockError> {
        for &id in sorted {
            let entry = view.mining_entry(id);
            let applied = self.inputs.tip.with_list_inputs(|coins, members| {
                trial.apply_tx(entry.tx, coins, members, &mut Vec::new(), &mut NoTrace)
            })?;
            let error = match applied {
                Ok(()) => continue,
                Err(DmlBuildError::Invalid(state)) => state.to_string(),
                Err(DmlBuildError::Exception(e)) => e.to_string(),
                Err(DmlBuildError::Store(e)) => {
                    return Err(read_fault(self.inputs.height, "ApplyTx", e));
                }
            };
            if !error.is_empty() {
                tracing::info!(
                    "addPackageTxs: package tx {} skipped, does not apply to the masternode \
                     list: {error}",
                    entry.txid
                );
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl<'b, T: TipReads> PackageChecks for PackageRules<'_, 'b, T> {
    type Accepted = AcceptedPackage<'b>;

    /// The pending service updates to mine ahead of `ancestors`, each with its unconfirmed
    /// ancestors.
    // Core v24.0.2 src/node/miner.cpp:642-671
    fn service_updates<V: PackageView + ?Sized>(
        &self,
        view: &V,
        ancestors: &[EntryId],
        in_block: &HashSet<EntryId>,
    ) -> Vec<EntryId> {
        let mut updates = Vec::new();
        let mut taken = HashSet::new();
        for &entry in ancestors {
            for update in view.service_updates_before_key_change(entry, self.inputs.dmn_tip_list) {
                if in_block.contains(&update) || taken.contains(&update) {
                    continue;
                }
                let mut package = view.ancestors_no_limits(update);
                package.retain(|id| !in_block.contains(id));
                package.push(update);
                set_entries(view, &mut package);
                let follows_key_change = package.iter().any(|&id| {
                    service_update_target(view.mining_entry(id).tx)
                        .is_some_and(|target| self.key_changed.contains(&target))
                });
                if !follows_key_change
                    && self.test_package_transactions(view, &package)
                    && !view.is_unorderable_service_update(
                        view.mining_entry(update).tx,
                        &package,
                        self.inputs.dmn_tip_list,
                    )
                {
                    for id in package {
                        if taken.insert(id) {
                            updates.push(id);
                        }
                    }
                }
            }
        }
        updates
    }

    // Core v24.0.2 src/node/miner.cpp:392-439
    fn test_package_transactions<V: PackageView + ?Sized>(
        &self,
        view: &V,
        package: &[EntryId],
    ) -> bool {
        !self.faulted() && self.latch(self.test_txs(view, package), false)
    }

    fn validate_package<V: PackageView + ?Sized>(
        &mut self,
        view: &V,
        sorted: &[EntryId],
        candidate: EntryId,
    ) -> Option<AcceptedPackage<'b>> {
        if self.faulted() {
            return None;
        }
        let validated = self.validate(view, sorted, candidate);
        self.latch(validated, None)
    }

    // Core v24.0.2 src/node/miner.cpp:801-805
    fn commit(&mut self, accepted: AcceptedPackage<'b>) {
        self.signals = accepted.signals;
        self.key_changed.extend(accepted.key_changed);
        if let Some(builder) = accepted.builder {
            self.inputs.builder = Some(builder);
        }
    }
}

/// `serviceUpdateTarget`: a ProUpServTx's masternode.
// Core v24.0.2 src/node/miner.cpp:532-536
fn service_update_target(tx: &Tx) -> Option<ProTxHash> {
    if tx.tx_type() != TxType::ProviderUpdateService {
        return None;
    }
    match tx.payload()?.ok().filter(|p| p.is_complete())?.payload() {
        SpecialPayload::ProviderUpdateService(p) => {
            Some(ProTxHash::from(*p.pro_tx_hash().as_bytes()))
        }
        _ => None,
    }
}
