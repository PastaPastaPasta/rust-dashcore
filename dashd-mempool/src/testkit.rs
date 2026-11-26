//! Scriptable in-memory InstantSend and handle ports for consumer tests, without transaction
//! validation.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use dash_consensus::{Amount, Tx, TxRef};
use dash_dml::MnList;
use dash_primitives::{OutPoint, TxHash};
use dashd_chainstate::chain::{ActorReply, ReplySender, WaitError, reply};
use parking_lot::RwLock;

use crate::api::{MempoolHandle, MempoolRead, RelayRead, TxMempoolInfo};
use crate::is::{IsAcceptHooks, IsMempool};
use crate::types::{
    IsConflict, MempoolAcceptResult, MempoolAcceptResultType, Package, PackageMempoolAcceptResult,
};

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
    time_s: i64,
    fee: Amount,
}

/// One admission request received by [`FakeIsMempool`]'s [`MempoolHandle`].
#[derive(Clone)]
pub struct Submission {
    pub tx: TxRef,
    /// The source peer of [`MempoolHandle::accept_async`]; `None` for local submissions.
    pub from: Option<u64>,
    pub bypass_limits: bool,
    pub test_accept: bool,
}

#[derive(Default)]
struct State {
    entries: BTreeMap<TxHash, Entry>,
    vtx_order: Vec<TxHash>,
    transactions_updated: u32,
    unbroadcast: BTreeSet<TxHash>,
    deltas: BTreeMap<TxHash, Amount>,
    dynamic_memory_usage: usize,
    now_s: i64,
    scripted: BTreeMap<TxHash, VecDeque<MempoolAcceptResult>>,
    parked: VecDeque<(Submission, ReplySender<MempoolAcceptResult>)>,
    submissions: Vec<Submission>,
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

    fn service_updates_before_key_change(
        &self,
        _key_change: &TxHash,
        _tip_list: &MnList,
    ) -> Vec<TxHash> {
        Vec::new()
    }

    fn is_unorderable_service_update(
        &self,
        _tx: &Tx,
        _ancestors: &[TxHash],
        _tip_list: &MnList,
    ) -> bool {
        false
    }

    fn key_change_target(&self, _tx: &Tx, _tip_list: &MnList) -> Option<dash_dml::ProTxHash> {
        None
    }
}

impl RelayRead for State {
    fn info(&self, txid: &TxHash) -> Option<TxMempoolInfo> {
        self.entries.get(txid).map(Entry::info)
    }

    fn info_all(&self) -> Vec<TxMempoolInfo> {
        let mut txids: Vec<_> = self.entries.keys().collect();
        txids.sort_by(|a, b| self.depth_and_score(a, b));
        txids.into_iter().map(|txid| self.entries[txid].info()).collect()
    }

    fn compare_depth_and_score(&self, a: &TxHash, b: &TxHash) -> Ordering {
        match (self.entries.contains_key(a), self.entries.contains_key(b)) {
            (true, true) => self.depth_and_score(a, b),
            (present_a, present_b) => present_a.cmp(&present_b),
        }
    }

    fn parents_entered_after(&self, txid: &TxHash, since_s: i64) -> Vec<TxHash> {
        self.parents(txid)
            .into_iter()
            .filter(|parent| self.entries[parent].time_s > since_s)
            .collect()
    }

    fn asset_unlock_by_instance_hash(&self, instance_hash: &TxHash) -> Option<TxHash> {
        self.entries
            .values()
            .find(|entry| {
                entry.tx.is_asset_unlock_with_stable_txid()
                    && entry.tx.instance_hash() == *instance_hash
            })
            .map(|entry| entry.tx.txid())
    }

    fn unbroadcast(&self) -> Vec<TxHash> {
        self.unbroadcast.iter().copied().collect()
    }

    fn size(&self) -> usize {
        self.entries.len()
    }

    fn dynamic_memory_usage(&self) -> usize {
        self.dynamic_memory_usage
    }

    fn for_each_in_vtxhashes_order(&self, f: &mut dyn FnMut(&TxHash, &TxRef) -> bool) {
        for txid in &self.vtx_order {
            let tx = &self.entries[txid].tx;
            if !f(&tx.instance_hash(), tx) {
                break;
            }
        }
    }
}

impl Entry {
    fn info(&self) -> TxMempoolInfo {
        TxMempoolInfo {
            tx: self.tx.clone(),
            time_s: self.time_s,
        }
    }

    /// `CompareTxMemPoolEntryByScore` (`src/txmempool.h:131-143`, Dash Core v24.0.0) on exact
    /// products, as the pool compares: `Less` when `self` sorts first.
    fn cmp_score(&self, other: &Entry) -> Ordering {
        let mine = i128::from(self.fee) * i128::from(other.tx.size());
        let theirs = i128::from(other.fee) * i128::from(self.tx.size());
        theirs.cmp(&mine).then_with(|| other.tx.txid().cmp(&self.tx.txid()))
    }
}

impl State {
    /// The in-pool txids `txid` spends from, ascending; empty if `txid` is absent.
    fn parents(&self, txid: &TxHash) -> BTreeSet<TxHash> {
        let Some(entry) = self.entries.get(txid) else {
            return BTreeSet::new();
        };
        entry
            .tx
            .raw()
            .inputs()
            .iter()
            .map(|input| input.prevout().txid())
            .filter(|parent| self.entries.contains_key(parent))
            .collect()
    }

    /// `GetCountWithAncestors`: the entry and its in-pool ancestors.
    fn count_with_ancestors(&self, txid: &TxHash) -> usize {
        let mut seen = BTreeSet::from([*txid]);
        let mut pending = vec![*txid];
        while let Some(next) = pending.pop() {
            for parent in self.parents(&next) {
                if seen.insert(parent) {
                    pending.push(parent);
                }
            }
        }
        seen.len()
    }

    /// `DepthAndScoreComparator` over two present entries.
    fn depth_and_score(&self, a: &TxHash, b: &TxHash) -> Ordering {
        self.count_with_ancestors(a)
            .cmp(&self.count_with_ancestors(b))
            .then_with(|| self.entries[a].cmp_score(&self.entries[b]))
    }

    /// Records a submission and replies with its scripted result, or parks it until
    /// [`FakeIsMempool::resolve`].
    fn submit(&mut self, submission: Submission) -> ActorReply<MempoolAcceptResult> {
        let (sender, receiver) = reply();
        self.submissions.push(submission.clone());
        let txid = submission.tx.txid();
        match self.scripted.get_mut(&txid).and_then(VecDeque::pop_front) {
            Some(result) => self.complete(&submission, sender, result),
            None => self.parked.push_back((submission, sender)),
        }
        receiver
    }

    /// Admits a valid non-test submission at `now_s` with its base fee, then replies.
    fn complete(
        &mut self,
        submission: &Submission,
        sender: ReplySender<MempoolAcceptResult>,
        result: MempoolAcceptResult,
    ) {
        if result.result_type == MempoolAcceptResultType::Valid && !submission.test_accept {
            let time_s = self.now_s;
            self.put(submission.tx.clone(), time_s, result.base_fee.unwrap_or_default());
        }
        sender.send(result);
    }

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
        self.unbroadcast.retain(|id| !removed.contains(id));
        let mut i = 0;
        while i < self.vtx_order.len() {
            if removed.contains(&self.vtx_order[i]) {
                self.vtx_order.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Inserts or replaces an entry; a new txid joins the end of the `vTxHashes` order.
    fn put(&mut self, tx: TxRef, time_s: i64, fee: Amount) {
        let txid = tx.txid();
        let entry = Entry {
            tx,
            asset_unlock: None,
            time_s,
            fee,
        };
        if self.entries.insert(txid, entry).is_none() {
            self.vtx_order.push(txid);
        }
    }
}

const _: () = assert!(usize::BITS <= 64);

/// Stores transactions and scripted withdrawal metadata under one lock, behind both
/// [`IsMempool`] and [`MempoolHandle`] (`Arc<FakeIsMempool>` coerces to `Arc<dyn MempoolHandle>`).
/// No transaction validation runs; callers supply a consistent set of input spenders. Ancestry
/// comes from the inputs of resident entries; scores use the scripted fee and the transaction's
/// total size.
///
/// Admissions reply with the next result scripted for their txid ([`FakeIsMempool::script`]); a
/// valid non-test result first admits the transaction at [`FakeIsMempool::set_time`] with the
/// result's base fee. An unscripted admission waits for [`FakeIsMempool::resolve`]; the blocking
/// [`MempoolHandle::accept`] and [`MempoolHandle::test_accept`] then need another thread to
/// resolve it. Dropping the fake disconnects every waiting reply.
#[derive(Default)]
pub struct FakeIsMempool {
    state: RwLock<State>,
}

impl FakeIsMempool {
    pub fn insert(&self, tx: TxRef) {
        self.insert_entry(tx, 0, 0);
    }

    /// Inserts or replaces an entry with its admission time and original fee.
    pub fn insert_entry(&self, tx: TxRef, time_s: i64, fee: Amount) {
        self.state.write().put(tx, time_s, fee);
    }

    /// Swaps a resident entry's transaction for another instance with the same txid, keeping its
    /// time, fee and withdrawal metadata (`ReplaceAssetUnlockInstance`). Returns false if absent.
    pub fn replace_instance(&self, tx: TxRef) -> bool {
        let mut state = self.state.write();
        let Some(entry) = state.entries.get_mut(&tx.txid()) else {
            return false;
        };
        entry.tx = tx;
        true
    }

    /// Queues `result` as the reply to the next admission of `txid`.
    pub fn script(&self, txid: TxHash, result: MempoolAcceptResult) {
        self.state.write().scripted.entry(txid).or_default().push_back(result);
    }

    /// Replies to the earliest waiting admission of `txid`, as a scripted result would.
    /// Returns false if none is waiting.
    pub fn resolve(&self, txid: &TxHash, result: MempoolAcceptResult) -> bool {
        let mut state = self.state.write();
        let Some(at) =
            state.parked.iter().position(|(submission, _)| submission.tx.txid() == *txid)
        else {
            return false;
        };
        let Some((submission, sender)) = state.parked.remove(at) else {
            return false;
        };
        state.complete(&submission, sender, result);
        true
    }

    /// Every admission request so far, in submission order.
    pub fn submissions(&self) -> Vec<Submission> {
        self.state.read().submissions.clone()
    }

    /// The accumulated [`MempoolHandle::prioritise`] and [`MempoolHandle::post_prioritise`]
    /// delta for `txid`.
    pub fn fee_delta(&self, txid: &TxHash) -> Amount {
        self.state.read().deltas.get(txid).copied().unwrap_or_default()
    }

    /// Sets the admission time of later valid admissions.
    pub fn set_time(&self, now_s: i64) {
        self.state.write().now_s = now_s;
    }

    /// Sets the value [`RelayRead::dynamic_memory_usage`] reports.
    pub fn set_dynamic_memory_usage(&self, bytes: usize) {
        self.state.write().dynamic_memory_usage = bytes;
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

impl MempoolHandle for FakeIsMempool {
    fn accept_async(&self, tx: TxRef, from: Option<u64>) -> ActorReply<MempoolAcceptResult> {
        self.state.write().submit(Submission {
            tx,
            from,
            bypass_limits: false,
            test_accept: false,
        })
    }

    fn accept(&self, tx: TxRef, bypass_limits: bool) -> Result<MempoolAcceptResult, WaitError> {
        let reply = self.state.write().submit(Submission {
            tx,
            from: None,
            bypass_limits,
            test_accept: false,
        });
        reply.recv()
    }

    fn test_accept(
        &self,
        tx: TxRef,
        bypass_limits: bool,
    ) -> Result<MempoolAcceptResult, WaitError> {
        let reply = self.state.write().submit(Submission {
            tx,
            from: None,
            bypass_limits,
            test_accept: true,
        });
        reply.recv()
    }

    /// The fake has no package acceptance: it replies `Disconnected`, as a stopped pool does.
    fn process_new_package(
        &self,
        _txns: Package,
        _test_accept: bool,
    ) -> Result<PackageMempoolAcceptResult, WaitError> {
        Err(WaitError::Disconnected)
    }

    fn prioritise(&self, txid: &TxHash, fee_delta: Amount) {
        let mut state = self.state.write();
        let delta = state.deltas.entry(*txid).or_default();
        *delta = delta.saturating_add(fee_delta);
    }

    fn post_prioritise(&self, txid: &TxHash, fee_delta: Amount) {
        self.prioritise(txid, fee_delta);
    }

    fn add_unbroadcast(&self, txid: &TxHash) {
        let mut state = self.state.write();
        if state.entries.contains_key(txid) {
            state.unbroadcast.insert(*txid);
        }
    }

    fn remove_unbroadcast(&self, txid: &TxHash, _unchecked: bool) {
        self.state.write().unbroadcast.remove(txid);
    }

    fn with_relay_read(&self, f: &mut dyn FnMut(&dyn RelayRead)) {
        f(&*self.state.read());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_consensus::TxValidationResult;
    use dash_primitives::{Transaction, TxIn, TxOut, TxType};

    use super::*;

    fn tx(n: u32, inputs: &[OutPoint]) -> TxRef {
        Arc::new(Tx::new(Transaction::new(
            2,
            TxType::new(0),
            inputs.iter().map(|outpoint| TxIn::new(*outpoint, vec![].into(), u32::MAX)).collect(),
            vec![TxOut::from_duffs(1, vec![0x51].into())],
            n,
            vec![],
        )))
    }

    fn result(result_type: MempoolAcceptResultType, base_fee: Amount) -> MempoolAcceptResult {
        MempoolAcceptResult {
            result_type,
            validation_result: TxValidationResult::Unset,
            reason: String::new(),
            debug_message: String::new(),
            vsize: None,
            base_fee: Some(base_fee),
            effective_feerate: None,
            txids_fee_calculations: None,
        }
    }

    fn txids(infos: &[TxMempoolInfo]) -> Vec<TxHash> {
        infos.iter().map(|info| info.tx.txid()).collect()
    }

    /// Ascending ancestor count first, then descending original-fee score, then descending
    /// txid; a prioritisation delta never moves an entry.
    #[test]
    fn info_all_orders_by_depth_then_original_score_then_descending_txid() {
        let fake = Arc::new(FakeIsMempool::default());
        let rich_parent = tx(1, &[]);
        let child = tx(2, &[OutPoint::new(rich_parent.txid(), 0)]);
        let poor = tx(3, &[]);
        let (tie_low, tie_high) = {
            let (a, b) = (tx(4, &[]), tx(5, &[]));
            if a.txid() < b.txid() {
                (a, b)
            } else {
                (b, a)
            }
        };
        assert_eq!(rich_parent.size(), poor.size());
        fake.insert_entry(rich_parent.clone(), 10, 1_000_000);
        fake.insert_entry(child.clone(), 11, 1_000_000_000);
        fake.insert_entry(poor.clone(), 12, 1);
        fake.insert_entry(tie_low.clone(), 13, 500);
        fake.insert_entry(tie_high.clone(), 14, 500);
        let handle: Arc<dyn MempoolHandle> = fake.clone();
        handle.prioritise(&poor.txid(), 10_000_000_000);
        assert_eq!(fake.fee_delta(&poor.txid()), 10_000_000_000);
        let expected =
            [rich_parent.txid(), tie_high.txid(), tie_low.txid(), poor.txid(), child.txid()];
        let all = handle.read(|read| read.info_all());
        assert_eq!(txids(&all), expected);
        assert_eq!(all[0].time_s, 10);
        handle.read(|read| {
            for pair in expected.windows(2) {
                assert_eq!(read.compare_depth_and_score(&pair[0], &pair[1]), Ordering::Less);
                assert_eq!(read.compare_depth_and_score(&pair[1], &pair[0]), Ordering::Greater);
            }
            assert_eq!(read.size(), 5);
        });
    }

    /// Absent sorts before present, two absent compare equal, and every absent read is empty.
    #[test]
    fn compare_depth_and_score_puts_absent_first_and_absent_pairs_equal() {
        let fake = Arc::new(FakeIsMempool::default());
        let present = tx(1, &[]);
        let (absent, other_absent) = (TxHash::from([7; 32]), TxHash::from([8; 32]));
        fake.insert_entry(present.clone(), 5, 100);
        let handle: Arc<dyn MempoolHandle> = fake.clone();
        handle.read(|read| {
            assert_eq!(read.compare_depth_and_score(&absent, &present.txid()), Ordering::Less);
            assert_eq!(read.compare_depth_and_score(&present.txid(), &absent), Ordering::Greater);
            assert_eq!(read.compare_depth_and_score(&absent, &other_absent), Ordering::Equal);
            assert_eq!(
                read.compare_depth_and_score(&present.txid(), &present.txid()),
                Ordering::Equal
            );
            assert!(read.info(&absent).is_none());
            assert!(read.parents_entered_after(&absent, 0).is_empty());
            assert!(read.asset_unlock_by_instance_hash(&present.txid()).is_none());
        });
    }

    /// Parents strictly after `since_s` in ascending txid; unbroadcast only for residents and
    /// cleared on removal; admissions publish before the reply and keep submission order.
    #[test]
    fn relay_reads_and_scripted_admissions() {
        let fake = Arc::new(FakeIsMempool::default());
        let (low, high) = {
            let (a, b) = (tx(1, &[]), tx(2, &[]));
            if a.txid() < b.txid() {
                (a, b)
            } else {
                (b, a)
            }
        };
        let old = tx(3, &[]);
        let child = tx(
            4,
            &[
                OutPoint::new(high.txid(), 0),
                OutPoint::new(old.txid(), 0),
                OutPoint::new(low.txid(), 0),
            ],
        );
        fake.insert_entry(low.clone(), 20, 1);
        fake.insert_entry(high.clone(), 30, 1);
        fake.insert_entry(old.clone(), 10, 1);
        fake.insert_entry(child.clone(), 40, 1);
        let handle: Arc<dyn MempoolHandle> = fake.clone();
        assert_eq!(
            handle.read(|read| read.parents_entered_after(&child.txid(), 10)),
            [low.txid(), high.txid()]
        );
        assert_eq!(
            handle.read(|read| read.parents_entered_after(&child.txid(), 20)),
            [high.txid()]
        );

        handle.add_unbroadcast(&TxHash::from([9; 32]));
        handle.add_unbroadcast(&high.txid());
        handle.add_unbroadcast(&low.txid());
        assert_eq!(handle.read(|read| read.unbroadcast()), [low.txid(), high.txid()]);
        handle.remove_unbroadcast(&low.txid(), true);
        assert_eq!(handle.read(|read| read.unbroadcast()), [high.txid()]);
        let spent = OutPoint::new(TxHash::from([9; 32]), 0);
        let spender = tx(8, &[spent]);
        fake.insert_entry(spender.clone(), 60, 1);
        handle.add_unbroadcast(&spender.txid());
        assert_eq!(fake.remove_conflicts_for_lock(&TxHash::from([10; 32]), &[spent]).len(), 1);
        assert_eq!(handle.read(|read| read.unbroadcast()), [high.txid()]);

        let first = tx(5, &[]);
        let second = tx(6, &[]);
        fake.set_time(50);
        let first_reply = handle.accept_async(first.clone(), Some(3));
        fake.script(second.txid(), result(MempoolAcceptResultType::Invalid, 0));
        let second_reply = handle.accept_async(second.clone(), None);
        assert!(second_reply.try_recv().is_some_and(|r| r.is_ok()));
        assert!(first_reply.try_recv().is_none());
        assert!(fake.resolve(&first.txid(), result(MempoolAcceptResultType::Valid, 7)));
        assert_eq!(first_reply.recv().map(|r| r.result_type), Ok(MempoolAcceptResultType::Valid));
        let info = handle.read(|read| read.info(&first.txid())).map(|info| info.time_s);
        assert_eq!(info, Some(50));
        assert!(handle.read(|read| read.info(&second.txid())).is_none());

        fake.script(second.txid(), result(MempoolAcceptResultType::Valid, 7));
        assert!(handle.test_accept(second.clone(), true).is_ok());
        assert!(handle.read(|read| read.info(&second.txid())).is_none());
        let submitted: Vec<_> = fake
            .submissions()
            .iter()
            .map(|s| (s.tx.txid(), s.from, s.bypass_limits, s.test_accept))
            .collect();
        assert_eq!(
            submitted,
            [
                (first.txid(), Some(3), false, false),
                (second.txid(), None, false, false),
                (second.txid(), None, true, true),
            ]
        );

        let waiting = handle.accept_async(tx(7, &[]), None);
        drop(fake);
        drop(handle);
        assert_eq!(waiting.recv().map(|r| r.result_type), Err(WaitError::Disconnected));
    }

    #[test]
    fn vtxhashes_walk_is_insertion_order_with_swap_remove() {
        let fake = FakeIsMempool::default();
        let spent = OutPoint::new(TxHash::from([7; 32]), 0);
        let (a, b, c, d) = (tx(1, &[spent]), tx(2, &[]), tx(3, &[]), tx(4, &[]));
        for t in [&a, &b, &c, &d] {
            fake.insert(t.clone());
        }
        let walk = |limit: usize| {
            let mut seen = Vec::new();
            fake.with_relay_read(&mut |read| {
                read.for_each_in_vtxhashes_order(&mut |hash, tx| {
                    assert_eq!(*hash, tx.instance_hash());
                    seen.push(tx.txid());
                    seen.len() < limit
                });
            });
            seen
        };
        assert_eq!(walk(usize::MAX), [a.txid(), b.txid(), c.txid(), d.txid()]);
        assert_eq!(walk(2), [a.txid(), b.txid()]);

        let removed = fake.remove_conflicts_for_lock(&TxHash::from([8; 32]), &[spent]);
        assert_eq!(removed.len(), 1);
        assert_eq!(walk(usize::MAX), [d.txid(), b.txid(), c.txid()]);
    }
}
