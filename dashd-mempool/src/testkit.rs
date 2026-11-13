//! Scriptable in-memory InstantSend ports for consumer tests, without transaction validation.

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::{Tx, TxRef};
use dash_primitives::{OutPoint, TxHash};
use parking_lot::RwLock;

use crate::api::MempoolRead;
use crate::is::{IsAcceptHooks, IsMempool};
use crate::types::IsConflict;

/// Supplies configured answers by candidate txid; unconfigured answers are false or absent.
#[derive(Default)]
pub struct FakeIsHooks {
    pub enabled: bool,
    pub conflicts: BTreeMap<TxHash, IsConflict>,
    pub waiting: BTreeSet<TxHash>,
    pub locked: BTreeSet<TxHash>,
}

impl IsAcceptHooks for FakeIsHooks {
    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn conflicting_lock(&self, tx: &Tx) -> Option<IsConflict> {
        self.conflicts.get(&tx.txid()).copied()
    }

    fn is_waiting_for_tx(&self, txid: &TxHash) -> bool {
        self.waiting.contains(txid)
    }

    fn is_locked(&self, txid: &TxHash) -> bool {
        self.locked.contains(txid)
    }
}

struct Entry {
    tx: TxRef,
    asset_unlock: Option<(u64, i64)>,
}

#[derive(Default)]
struct State {
    entries: BTreeMap<TxHash, Entry>,
    transactions_updated: u32,
}

impl MempoolRead for State {
    fn get(&self, txid: &TxHash) -> Option<TxRef> {
        self.entries.get(txid).map(|entry| entry.tx.clone())
    }

    fn spender_of(&self, outpoint: &OutPoint) -> Option<TxRef> {
        self.entries
            .values()
            .find(|entry| entry.tx.raw().inputs().iter().any(|input| input.prevout() == *outpoint))
            .map(|entry| entry.tx.clone())
    }

    fn asset_unlock_txids_by_index(&self, index: u64) -> Vec<TxHash> {
        self.entries
            .iter()
            .filter(|(_, entry)| entry.asset_unlock.is_some_and(|(i, _)| i == index))
            .map(|(txid, _)| *txid)
            .collect()
    }

    fn pending_asset_unlock_amount(&self) -> i64 {
        // At most usize::MAX resident entries, each <= i64::MAX: on <=64-bit targets their
        // nonnegative sum is below 2^127, so i128 preserves the exact total before capping.
        let total: i128 = self
            .entries
            .values()
            .filter_map(|entry| entry.asset_unlock.map(|(_, amount)| i128::from(amount)))
            .sum();
        i64::try_from(total).unwrap_or(i64::MAX)
    }
}

impl State {
    fn remove_recursive(&mut self, txid: TxHash) {
        let mut removed = BTreeSet::from([txid]);
        loop {
            let before = removed.len();
            let children: Vec<_> = self
                .entries
                .iter()
                .filter(|(_, entry)| {
                    entry
                        .tx
                        .raw()
                        .inputs()
                        .iter()
                        .any(|input| removed.contains(&input.prevout().txid()))
                })
                .map(|(id, _)| *id)
                .collect();
            removed.extend(children);
            if removed.len() == before {
                break;
            }
        }
        self.entries.retain(|id, _| !removed.contains(id));
    }
}

const _: () = assert!(usize::BITS <= 64);

/// Stores transactions and scripted withdrawal metadata under one lock.
/// No transaction validation runs; callers supply a consistent set of input spenders.
#[derive(Default)]
pub struct FakeIsMempool {
    state: RwLock<State>,
}

impl FakeIsMempool {
    pub fn insert(&self, tx: TxRef) {
        self.state.write().entries.insert(
            tx.txid(),
            Entry {
                tx,
                asset_unlock: None,
            },
        );
    }

    /// Assigns a withdrawal index and amount without requiring a signed special payload.
    /// Returns false if the transaction is absent or the amount is negative.
    pub fn set_asset_unlock(&self, txid: &TxHash, index: u64, amount: i64) -> bool {
        if amount < 0 {
            return false;
        }
        let mut state = self.state.write();
        let Some(entry) = state.entries.get_mut(txid) else {
            return false;
        };
        entry.asset_unlock = Some((index, amount));
        true
    }

    pub fn transactions_updated(&self) -> u32 {
        self.state.read().transactions_updated
    }
}

impl IsMempool for FakeIsMempool {
    fn get(&self, txid: &TxHash) -> Option<TxRef> {
        self.state.read().get(txid)
    }

    fn exists(&self, txid: &TxHash) -> bool {
        self.state.read().exists(txid)
    }

    fn spender_of(&self, outpoint: &OutPoint) -> Option<TxRef> {
        self.state.read().spender_of(outpoint)
    }

    fn asset_unlock_txids_by_index(&self, index: u64) -> Vec<TxHash> {
        self.state.read().asset_unlock_txids_by_index(index)
    }

    fn pending_asset_unlock_amount(&self) -> i64 {
        self.state.read().pending_asset_unlock_amount()
    }

    fn remove_conflicts_for_lock(&self, txid: &TxHash, inputs: &[OutPoint]) -> Vec<TxRef> {
        let mut state = self.state.write();
        let roots: BTreeMap<_, _> = inputs
            .iter()
            .filter_map(|input| state.spender_of(input))
            .filter(|tx| tx.txid() != *txid)
            .map(|tx| (tx.txid(), tx))
            .collect();
        for root in roots.values() {
            state.remove_recursive(root.txid());
        }
        if let Some((index, _)) = state.entries.get(txid).and_then(|entry| entry.asset_unlock) {
            for conflict in state.asset_unlock_txids_by_index(index) {
                if conflict != *txid {
                    state.remove_recursive(conflict);
                }
            }
        }
        roots.into_values().collect()
    }

    fn add_transactions_updated(&self, n: u32) {
        let mut state = self.state.write();
        state.transactions_updated = state.transactions_updated.wrapping_add(n);
    }

    fn with_read(&self, f: &mut dyn FnMut(&dyn MempoolRead)) {
        f(&*self.state.read());
    }
}
