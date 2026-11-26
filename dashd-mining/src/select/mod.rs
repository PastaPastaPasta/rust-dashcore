//! Core's `BlockAssembler::addPackageTxs` (Core v24.0.2 src/node/miner.cpp:511-818): ancestor
//! fee-rate package selection. The Dash package rules run behind [`PackageChecks`].

mod modified;
mod sort;

#[cfg(test)]
pub(crate) mod tests;

use std::cmp::Ordering;
use std::collections::HashSet;

use dash_consensus::subsidy::COIN;

use crate::options::Options;
use crate::view::{EntryId, PackageView};

pub(crate) use modified::AncestorScore;
use modified::ModifiedSet;
use sort::{set_entries, sort_for_block};

/// The block's running totals (`nBlockSize`, `nBlockSigOps`, `nBlockTx`, `nFees`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockState {
    pub size: u64,
    pub sigops: u32,
    pub tx_count: u64,
    pub fees: i64,
}

impl Default for BlockState {
    /// `resetBlock` (Core v24.0.2 src/node/miner.cpp:103-114): the coinbase's reserve.
    fn default() -> Self {
        Self {
            size: 1000,
            sigops: 100,
            tx_count: 0,
            fees: 0,
        }
    }
}

/// One `AddToBlock`: the entry, its `vTxFees` (the unmodified fee) and `vTxSigOps` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selected {
    pub id: EntryId,
    pub fee: i64,
    pub sigops: u32,
}

/// What `addPackageTxs` leaves behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// Block order.
    pub entries: Vec<Selected>,
    pub state: BlockState,
    pub packages_selected: u64,
    pub descendants_updated: u64,
}

/// The Dash package rules, called where Core runs them. Every entry list is a `setEntries`:
/// distinct entries in `core_hash_cmp` txid order, except `sorted`, which is block order.
pub trait PackageChecks {
    /// What [`PackageChecks::validate_package`] accepted, applied by
    /// [`PackageChecks::commit`] once the package is in.
    type Accepted;

    /// The pending service updates to mine ahead of the package `ancestors` (the candidate
    /// included), each with its unconfirmed ancestors (Core v24.0.2 src/node/miner.cpp:642-671).
    /// None of them may be in `in_block`: Core skips in-block updates and their in-block ancestors
    /// (:654, 660).
    fn service_updates<V: PackageView + ?Sized>(
        &self,
        view: &V,
        ancestors: &[EntryId],
        in_block: &HashSet<EntryId>,
    ) -> Vec<EntryId>;

    /// `TestPackageTransactions` (Core v24.0.2 src/node/miner.cpp:392-439).
    fn test_package_transactions<V: PackageView + ?Sized>(
        &self,
        view: &V,
        package: &[EntryId],
    ) -> bool;

    /// The per-package checks over the sorted package (Core v24.0.2 src/node/miner.cpp:710-797);
    /// `None` skips the package. `candidate` is `iter`.
    fn validate_package<V: PackageView + ?Sized>(
        &mut self,
        view: &V,
        sorted: &[EntryId],
        candidate: EntryId,
    ) -> Option<Self::Accepted>;

    /// Core v24.0.2 src/node/miner.cpp:801-805.
    fn commit(&mut self, accepted: Self::Accepted);
}

/// No service updates; every package passes.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, Default)]
pub struct PassAll;

#[cfg(any(test, feature = "test-support"))]
impl PackageChecks for PassAll {
    type Accepted = ();

    fn service_updates<V: PackageView + ?Sized>(
        &self,
        _: &V,
        _: &[EntryId],
        _: &HashSet<EntryId>,
    ) -> Vec<EntryId> {
        Vec::new()
    }

    fn test_package_transactions<V: PackageView + ?Sized>(&self, _: &V, _: &[EntryId]) -> bool {
        true
    }

    fn validate_package<V: PackageView + ?Sized>(
        &mut self,
        _: &V,
        _: &[EntryId],
        _: EntryId,
    ) -> Option<()> {
        Some(())
    }

    fn commit(&mut self, _: ()) {}
}

const MAX_CONSECUTIVE_FAILURES: i64 = 1000;

/// Selects from `view` into a block that starts at `start`, under the clamped `options`.
pub fn add_package_txs<V: PackageView + ?Sized, K: PackageChecks>(
    view: &V,
    checks: &mut K,
    options: &Options,
    start: BlockState,
) -> Selection {
    let mut assembler = Assembler {
        view,
        options,
        in_block: HashSet::new(),
        selection: Selection {
            entries: Vec::new(),
            state: start,
            packages_selected: 0,
            descendants_updated: 0,
        },
    };
    assembler.add_package_txs(checks);
    assembler.selection
}

struct Assembler<'a, V: ?Sized> {
    view: &'a V,
    options: &'a Options,
    in_block: HashSet<EntryId>,
    selection: Selection,
}

impl<V: PackageView + ?Sized> Assembler<'_, V> {
    // Core v24.0.2 src/node/miner.cpp:511-818
    fn add_package_txs<K: PackageChecks>(&mut self, checks: &mut K) {
        let view = self.view;
        let mut modified = ModifiedSet::default();
        let mut failed = HashSet::new();
        let mut mi = view.ancestor_score_iter().peekable();
        let mut consecutive_failed = 0i64;

        while mi.peek().is_some() || !modified.is_empty() {
            if let Some(&next) = mi.peek()
                && (modified.contains(next)
                    || self.in_block.contains(&next)
                    || failed.contains(&next))
            {
                mi.next();
                continue;
            }

            let best = modified.best();
            let (iter, modit) = match (mi.peek().copied(), best) {
                (None, best) => (best.expect("loop condition").0, best),
                (Some(next), Some((id, m)))
                    if m.score.cmp(&AncestorScore::of(&view.mining_entry(next)))
                        == Ordering::Less =>
                {
                    (id, best)
                }
                (Some(next), _) => {
                    mi.next();
                    (next, None)
                }
            };
            debug_assert!(!self.in_block.contains(&iter));

            let (package_size, package_fees, package_sigops) = match modit {
                Some((_, m)) => (
                    m.score.size_with_ancestors,
                    m.score.mod_fees_with_ancestors,
                    m.sigops_with_ancestors,
                ),
                None => {
                    let e = view.mining_entry(iter);
                    (e.size_with_ancestors, e.mod_fees_with_ancestors, e.sigops_with_ancestors)
                }
            };
            let mut skip = |modified: &mut ModifiedSet| {
                if modit.is_some() {
                    modified.remove(iter);
                    failed.insert(iter);
                }
            };

            if package_fees < self.options.block_min_fee_rate.get_fee(package_size as u32) {
                return;
            }

            if !self.test_package(package_size, package_sigops) {
                skip(&mut modified);
                consecutive_failed += 1;
                if consecutive_failed > MAX_CONSECUTIVE_FAILURES
                    && self.selection.state.size > self.options.block_max_size.wrapping_sub(1000)
                {
                    break;
                }
                continue;
            }

            // :638-641 onlyUnconfirmed (:365-375)
            let mut ancestors = view.ancestors_no_limits(iter);
            ancestors.retain(|id| !self.in_block.contains(id));
            ancestors.push(iter);
            set_entries(view, &mut ancestors);

            let mut updates = checks.service_updates(view, &ancestors, &self.in_block);
            debug_assert!(updates.iter().all(|id| !self.in_block.contains(id)));
            set_entries(view, &mut updates);
            // :673-685
            if !updates.is_empty() {
                let in_ancestors: HashSet<_> = ancestors.iter().copied().collect();
                let (extra_size, extra_sigops) = updates
                    .iter()
                    .filter(|id| !in_ancestors.contains(id))
                    .map(|id| view.mining_entry(*id))
                    .fold((0u64, 0u32), |(size, sigops), e| {
                        (size.wrapping_add(e.tx_size), sigops.wrapping_add(e.sigops))
                    });
                if !self.test_package(
                    package_size.wrapping_add(extra_size),
                    package_sigops.wrapping_add(extra_sigops),
                ) {
                    updates.clear();
                }
            }

            if !checks.test_package_transactions(view, &ancestors) {
                skip(&mut modified);
                continue;
            }

            // :696-708: the sorted entries are `ancestors ∪ serviceUpdates`.
            let in_updates: HashSet<_> = updates.iter().copied().collect();
            let mut sorted = updates;
            sort_for_block(view, &mut sorted);
            let mut rest: Vec<_> =
                ancestors.into_iter().filter(|id| !in_updates.contains(id)).collect();
            sort_for_block(view, &mut rest);
            sorted.extend(rest);

            let Some(accepted) = checks.validate_package(view, &sorted, iter) else {
                skip(&mut modified);
                continue;
            };

            consecutive_failed = 0;
            checks.commit(accepted);
            for &id in &sorted {
                self.add_to_block(id);
                modified.remove(id);
            }
            self.selection.packages_selected += 1;
            self.selection.descendants_updated +=
                update_packages_for_added(view, &sorted, &mut modified);
        }
    }

    // Core v24.0.2 src/node/miner.cpp:377-387
    fn test_package(&self, package_size: u64, package_sigops: u32) -> bool {
        let state = &self.selection.state;
        state.size.wrapping_add(package_size) < self.options.block_max_size
            && state.sigops.wrapping_add(package_sigops) < self.options.block_max_sigops
    }

    // Core v24.0.2 src/node/miner.cpp:441-458
    fn add_to_block(&mut self, id: EntryId) {
        let e = self.view.mining_entry(id);
        let state = &mut self.selection.state;
        self.selection.entries.push(Selected {
            id,
            fee: e.fee,
            sigops: e.sigops,
        });
        state.size = state.size.wrapping_add(e.tx_size);
        state.tx_count += 1;
        state.sigops = state.sigops.wrapping_add(e.sigops);
        state.fees = state.fees.wrapping_add(e.fee);
        self.in_block.insert(id);
        if self.options.print_priority {
            tracing::info!(
                "fee rate {} txid {}",
                fee_rate_string(e.modified_fee, e.tx_size),
                e.txid
            );
        }
    }
}

/// Core v24.0.2 src/node/miner.cpp:463-488. Returns `nDescendantsUpdated`.
fn update_packages_for_added<V: PackageView + ?Sized>(
    view: &V,
    added: &[EntryId],
    modified: &mut ModifiedSet,
) -> u64 {
    let already: HashSet<_> = added.iter().copied().collect();
    let mut updated = 0;
    for &it in added {
        for desc in view.descendants(it) {
            if already.contains(&desc) {
                continue;
            }
            updated += 1;
            modified.update_for_parent_inclusion(view, desc, it);
        }
    }
    updated
}

/// `CFeeRate(fee, num_bytes).ToString()` (Core v24.0.2 src/policy/feerate.cpp:12-21, 39-45).
fn fee_rate_string(fee: i64, tx_size: u64) -> String {
    let size = i64::from(tx_size as u32);
    let per_k = if size > 0 {
        fee.wrapping_mul(1000) / size
    } else {
        0
    };
    format!("{}.{:08} DASH/kB", per_k / COIN, per_k % COIN)
}
