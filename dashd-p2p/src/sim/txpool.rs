//! A scriptable transaction pool for deterministic P2P simulations.

use std::cmp::Ordering;
use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use dash_consensus::{TxRef, TxValidationResult};
use dash_p2p::PeerId;
use dash_primitives::TxHash;
use dash_types::Numeric;
use parking_lot::Mutex;

use crate::processing::compact::CompactTxSource;
use crate::processing::seams::{AcceptResult, AcceptResultType, AcceptWait, TxInfo, TxPool};

struct Entry {
    info: TxInfo,
    ancestors: u64,
    fee: i64,
    virtual_size: u32,
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    unbroadcast: BTreeSet<TxHash>,
    replies: VecDeque<AcceptResult>,
    submissions: Vec<(TxRef, Option<PeerId>)>,
    time_s: i64,
    dynamic_memory_usage: usize,
    unbroadcast_removals: Vec<(TxHash, bool)>,
}

impl State {
    fn entry(&self, txid: &TxHash) -> Option<&Entry> {
        self.entries.iter().find(|e| e.info.tx.txid() == *txid)
    }

    fn insert(&mut self, info: TxInfo, ancestors: u64, fee: i64, virtual_size: u32) {
        let entry = Entry {
            info,
            ancestors,
            fee,
            virtual_size,
        };
        if let Some(old) =
            self.entries.iter_mut().find(|e| e.info.tx.txid() == entry.info.tx.txid())
        {
            let time_s = old.info.time_s;
            *old = entry;
            old.info.time_s = time_s;
        } else {
            self.entries.push(entry);
        }
    }

    fn remove(&mut self, txid: &TxHash) {
        if let Some(i) = self.entries.iter().position(|e| e.info.tx.txid() == *txid) {
            self.entries.swap_remove(i);
        }
        self.unbroadcast.remove(txid);
    }
}

/// A shared fake with scripted admission results and explicit ancestor counts and fees.
/// It performs no validation. Unscripted admission succeeds at the configured time with
/// one ancestor (itself) and zero fee. Clones observe the same state.
#[derive(Clone, Default)]
pub struct SimTxPool(Arc<Mutex<State>>);

impl SimTxPool {
    /// Insert an entry using its serialized size as the virtual-size default.
    pub fn insert(&self, info: TxInfo, ancestors: u64, fee: i64) {
        let virtual_size = info.tx.size();
        self.insert_with_virtual_size(info, ancestors, fee, virtual_size);
    }

    /// Insert an entry in compact-reconstruction order with Core's virtual size.
    pub fn insert_with_virtual_size(
        &self,
        info: TxInfo,
        ancestors: u64,
        fee: i64,
        virtual_size: u32,
    ) {
        self.0.lock().insert(info, ancestors, fee, virtual_size);
    }

    /// Remove an entry, using swap-remove for compact-reconstruction order.
    pub fn remove(&self, txid: &TxHash) {
        self.0.lock().remove(txid);
    }

    /// Set the admission time of subsequently accepted transactions.
    pub fn set_time(&self, time_s: i64) {
        self.0.lock().time_s = time_s;
    }

    /// Set the memory usage returned to the relay log.
    pub fn set_dynamic_memory_usage(&self, bytes: usize) {
        self.0.lock().dynamic_memory_usage = bytes;
    }

    /// Unbroadcast removals in call order, including their unchecked flag.
    pub fn unbroadcast_removals(&self) -> Vec<(TxHash, bool)> {
        self.0.lock().unbroadcast_removals.clone()
    }

    /// Queue the next admission decision. Only `Valid` mutates pool membership.
    pub fn push_result(&self, result: AcceptResult) {
        self.0.lock().replies.push_back(result);
    }

    /// Submissions in call order, with their source peer (including absent sources).
    pub fn submissions(&self) -> Vec<(TxRef, Option<PeerId>)> {
        self.0.lock().submissions.clone()
    }

    /// Add an existing stable txid to the unbroadcast set.
    pub fn add_unbroadcast(&self, txid: TxHash) {
        let mut state = self.0.lock();
        if state.entry(&txid).is_some() {
            state.unbroadcast.insert(txid);
        }
    }
}

/// Core v24.0.0 `src/txmempool.cpp:1495-1513` and `src/txmempool.h:131-142`.
fn compare(a: &Entry, b: &Entry) -> Ordering {
    a.ancestors.cmp(&b.ancestors).then_with(|| {
        let f1 = a.fee as f64 * f64::from(b.virtual_size);
        let f2 = b.fee as f64 * f64::from(a.virtual_size);
        f2.partial_cmp(&f1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.info.tx.txid().to_lendian().cmp(&a.info.tx.txid().to_lendian()))
    })
}

impl CompactTxSource for SimTxPool {
    fn for_each_in_vtxhashes_order(&self, f: &mut dyn FnMut(&TxHash, &TxRef) -> bool) {
        let state = self.0.lock();
        for entry in &state.entries {
            if !f(&entry.info.tx.instance_hash(), &entry.info.tx) {
                break;
            }
        }
    }
}

impl TxPool for SimTxPool {
    fn accept(&self, tx: TxRef, from: Option<PeerId>) -> AcceptWait {
        let mut state = self.0.lock();
        state.submissions.push((Arc::clone(&tx), from));
        let result = state.replies.pop_front().unwrap_or(AcceptResult {
            result_type: AcceptResultType::Valid,
            validation_result: TxValidationResult::Unset,
            reason: String::new(),
            debug_message: String::new(),
            vsize: Some(i64::from(tx.size())),
            base_fee: Some(0),
        });
        if result.result_type == AcceptResultType::Valid {
            let virtual_size =
                result.vsize.unwrap_or_else(|| i64::from(tx.size())).try_into().unwrap_or(u32::MAX);
            let time_s = state.time_s;
            state.insert(
                TxInfo {
                    tx,
                    time_s,
                },
                1,
                result.base_fee.unwrap_or(0),
                virtual_size,
            );
        }
        AcceptWait::ready(result)
    }

    fn info(&self, txid: &TxHash) -> Option<TxInfo> {
        self.0.lock().entry(txid).map(|e| e.info.clone())
    }

    fn asset_unlock_by_instance_hash(&self, hash: &TxHash) -> Option<TxHash> {
        self.0.lock().entries.iter().find_map(|e| {
            (e.info.tx.is_asset_unlock_with_stable_txid() && e.info.tx.instance_hash() == *hash)
                .then(|| e.info.tx.txid())
        })
    }

    fn exists(&self, txid: &TxHash) -> bool {
        self.0.lock().entry(txid).is_some()
    }

    fn recent_parents(&self, txid: &TxHash, since_s: i64) -> Vec<TxHash> {
        let state = self.0.lock();
        let Some(entry) = state.entry(txid) else {
            return Vec::new();
        };
        let mut parents: Vec<_> = entry
            .info
            .tx
            .raw()
            .inputs()
            .iter()
            .filter_map(|input| {
                let parent = state.entry(&input.prevout().txid())?;
                (parent.info.time_s > since_s).then(|| parent.info.tx.txid())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        parents.sort_by_key(TxHash::to_lendian);
        parents
    }

    fn info_all(&self) -> Vec<TxInfo> {
        let state = self.0.lock();
        let mut entries: Vec<_> = state.entries.iter().collect();
        entries.sort_by(|a, b| compare(a, b));
        entries.into_iter().map(|e| e.info.clone()).collect()
    }

    fn compare_depth_and_score(&self, a: &TxHash, b: &TxHash) -> Ordering {
        let state = self.0.lock();
        match (state.entry(a), state.entry(b)) {
            (Some(a), Some(b)) => compare(a, b),
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }

    fn unbroadcast(&self) -> Vec<TxHash> {
        let mut ids: Vec<_> = self.0.lock().unbroadcast.iter().copied().collect();
        ids.sort_by_key(TxHash::to_lendian);
        ids
    }

    fn remove_unbroadcast(&self, txid: &TxHash, unchecked: bool) {
        let mut state = self.0.lock();
        state.unbroadcast_removals.push((*txid, unchecked));
        state.unbroadcast.remove(txid);
    }

    fn size(&self) -> usize {
        self.0.lock().entries.len()
    }

    fn dynamic_memory_usage(&self) -> usize {
        self.0.lock().dynamic_memory_usage
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Reverse;

    use dash_consensus::Tx;
    use dash_primitives::{OutPoint, Transaction, TxIn, TxType};

    use super::*;
    use crate::processing::seams::{
        CoinJoinObjects, IsLockLookup, NoCoinJoinObjects, NoIsLocks, NoTxPool,
    };

    fn tx(tag: u32, parents: &[TxHash]) -> TxRef {
        tx_with_script(tag, parents, &[])
    }

    fn tx_with_script(tag: u32, parents: &[TxHash], script: &[u8]) -> TxRef {
        let inputs = parents
            .iter()
            .map(|h| TxIn::new(OutPoint::new(*h, 0), script.to_vec().into(), u32::MAX))
            .collect();
        Arc::new(Tx::new(Transaction::new(1, TxType::Spend, inputs, vec![], tag, vec![])))
    }

    #[tokio::test]
    async fn admission_results_preserve_sources_and_only_valid_mutates() {
        let pool = SimTxPool::default();
        let a = tx(1, &[]);
        let b = tx(2, &[]);
        pool.set_time(42);
        let accepted = pool.accept(a.clone(), None);
        assert!(pool.exists(&a.txid()), "mutation precedes the ready reply");
        assert_eq!(
            accepted.await,
            AcceptResult {
                result_type: AcceptResultType::Valid,
                validation_result: TxValidationResult::Unset,
                reason: String::new(),
                debug_message: String::new(),
                vsize: Some(i64::from(a.size())),
                base_fee: Some(0),
            }
        );
        pool.add_unbroadcast(a.txid());
        for validation_result in
            [TxValidationResult::MissingInputs, TxValidationResult::NotStandard]
        {
            let result = AcceptResult {
                result_type: AcceptResultType::Invalid,
                validation_result,
                reason: "policy".into(),
                debug_message: "validation detail".into(),
                vsize: None,
                base_fee: None,
            };
            pool.push_result(result.clone());
            assert_eq!(pool.accept(b.clone(), Some(7)).await, result);
            assert!(!pool.exists(&b.txid()));
        }
        let result = AcceptResult {
            result_type: AcceptResultType::MempoolEntry,
            validation_result: TxValidationResult::Unset,
            reason: String::new(),
            debug_message: String::new(),
            vsize: Some(i64::from(a.size())),
            base_fee: Some(17),
        };
        pool.push_result(result.clone());
        pool.set_time(99);
        assert_eq!(pool.accept(a.clone(), None).await, result);
        assert_eq!(pool.info(&a.txid()).unwrap().time_s, 42);
        let _ = pool.accept(b.clone(), None).await;
        assert!(pool.exists(&a.txid()));
        assert_eq!(pool.unbroadcast(), [a.txid()]);
        assert_eq!(pool.info(&b.txid()).unwrap().time_s, 99);
        assert_eq!(pool.size(), 2);
        assert_eq!(pool.dynamic_memory_usage(), 0);
        pool.set_dynamic_memory_usage(1234);
        assert_eq!(pool.clone().dynamic_memory_usage(), 1234);
        assert_eq!(
            pool.submissions().iter().map(|(_, p)| *p).collect::<Vec<_>>(),
            [None, Some(7), Some(7), None, None]
        );
    }

    #[test]
    fn ordering_parents_unbroadcast_and_compact_walk() {
        let pool = SimTxPool::default();
        let a = tx(1, &[]);
        let b = tx(2, &[]);
        let c = tx(3, &[a.txid(), b.txid(), a.txid()]);
        for (tx, time_s, ancestors, fee) in
            [(a.clone(), 10, 1, 100), (b.clone(), 11, 1, 200), (c.clone(), 12, 3, 10000)]
        {
            pool.insert(
                TxInfo {
                    tx,
                    time_s,
                },
                ancestors,
                fee,
            );
        }
        assert_eq!(
            pool.info_all().iter().map(|i| i.tx.txid()).collect::<Vec<_>>(),
            [b.txid(), a.txid(), c.txid()]
        );
        assert_eq!(pool.compare_depth_and_score(&b.txid(), &a.txid()), Ordering::Less);
        assert_eq!(pool.compare_depth_and_score(&c.txid(), &b.txid()), Ordering::Greater);
        let absent = tx(4, &[]).txid();
        assert_eq!(pool.compare_depth_and_score(&absent, &a.txid()), Ordering::Less);
        assert_eq!(pool.compare_depth_and_score(&a.txid(), &absent), Ordering::Greater);
        assert_eq!(pool.compare_depth_and_score(&absent, &absent), Ordering::Equal);
        assert_eq!(pool.recent_parents(&c.txid(), 10), [b.txid()]);
        let mut parents = vec![a.txid(), b.txid()];
        parents.sort_by_key(TxHash::to_lendian);
        assert_eq!(pool.recent_parents(&c.txid(), 9), parents);
        assert!(pool.recent_parents(&absent, 0).is_empty());
        pool.add_unbroadcast(b.txid());
        pool.add_unbroadcast(a.txid());
        pool.add_unbroadcast(absent);
        assert_eq!(pool.unbroadcast(), parents);
        pool.remove_unbroadcast(&a.txid(), false);
        pool.remove_unbroadcast(&absent, true);
        assert_eq!(pool.unbroadcast_removals(), [(a.txid(), false), (absent, true)]);
        assert_eq!(pool.unbroadcast(), [b.txid()]);
        let mut compact = Vec::new();
        pool.for_each_in_vtxhashes_order(&mut |h, _| {
            compact.push(*h);
            false
        });
        assert_eq!(compact, [a.instance_hash()]);
        pool.remove(&a.txid());
        compact.clear();
        pool.for_each_in_vtxhashes_order(&mut |h, _| {
            compact.push(*h);
            true
        });
        assert_eq!(compact, [c.instance_hash(), b.instance_hash()]);
        assert_eq!(pool.asset_unlock_by_instance_hash(&b.instance_hash()), None);
    }

    #[test]
    fn equal_scores_use_descending_txids_and_instances_replace_in_place() {
        let pool = SimTxPool::default();
        let a = tx(1, &[]);
        let b = tx(2, &[]);
        for tx in [a.clone(), b.clone()] {
            pool.insert(
                TxInfo {
                    tx,
                    time_s: 5,
                },
                1,
                10,
            );
        }
        let mut ids = vec![a.txid(), b.txid()];
        ids.sort_by_key(|h| Reverse(h.to_lendian()));
        assert_eq!(pool.info_all().iter().map(|i| i.tx.txid()).collect::<Vec<_>>(), ids);
        assert_eq!(pool.compare_depth_and_score(&a.txid(), &a.txid()), Ordering::Equal);

        let unlock = |signature: u8| {
            let mut payload = vec![2];
            payload.extend_from_slice(&7u64.to_le_bytes());
            payload.extend_from_slice(&1000u32.to_le_bytes());
            payload.extend_from_slice(&100u32.to_le_bytes());
            payload.extend_from_slice(&[0xab; 32]);
            payload.extend_from_slice(&[signature; 96]);
            Arc::new(Tx::new(Transaction::new(3, TxType::new(9), vec![], vec![], 0, payload)))
        };
        let old = unlock(1);
        let new = unlock(2);
        assert_eq!(old.txid(), new.txid());
        assert_ne!(old.instance_hash(), new.instance_hash());
        pool.insert(
            TxInfo {
                tx: old.clone(),
                time_s: 10,
            },
            1,
            1,
        );
        pool.insert(
            TxInfo {
                tx: new.clone(),
                time_s: 20,
            },
            1,
            1,
        );
        assert_eq!(pool.size(), 3);
        assert_eq!(pool.info(&new.txid()).unwrap().time_s, 10);
        assert_eq!(pool.asset_unlock_by_instance_hash(&old.instance_hash()), None);
        assert_eq!(pool.asset_unlock_by_instance_hash(&new.instance_hash()), Some(new.txid()));
        let mut hashes = Vec::new();
        pool.for_each_in_vtxhashes_order(&mut |h, _| {
            hashes.push(*h);
            true
        });
        assert_eq!(hashes, [a.instance_hash(), b.instance_hash(), new.instance_hash()]);
    }

    #[test]
    fn virtual_size_can_reverse_raw_byte_feerate_order() {
        let pool = SimTxPool::default();
        let a = tx(1, &[]);
        let b = tx(2, &[]);
        assert_eq!(a.size(), b.size());
        pool.insert_with_virtual_size(
            TxInfo {
                tx: a.clone(),
                time_s: 0,
            },
            1,
            200,
            800,
        );
        pool.insert_with_virtual_size(
            TxInfo {
                tx: b.clone(),
                time_s: 0,
            },
            1,
            100,
            200,
        );
        let fee_a = 200.0;
        let fee_b = 100.0;
        let virtual_a = 800.0;
        let virtual_b = 200.0;
        assert!(fee_a / f64::from(a.size()) > fee_b / f64::from(b.size()));
        assert!(fee_a / virtual_a < fee_b / virtual_b);
        let expected = [b.txid(), a.txid()];
        assert_eq!(pool.info_all().iter().map(|i| i.tx.txid()).collect::<Vec<_>>(), expected);
        assert_eq!(pool.compare_depth_and_score(&b.txid(), &a.txid()), Ordering::Less);
        assert_eq!(pool.compare_depth_and_score(&a.txid(), &b.txid()), Ordering::Greater);
    }

    #[test]
    fn rounded_fee_products_use_the_hash_tie_break_over_exact_rates() {
        let pool = SimTxPool::default();
        let a = tx_with_script(1, &[TxHash::default()], &[]);
        let b = tx_with_script(2, &[TxHash::default()], &[0]);
        assert_eq!((a.size(), b.size()), (51, 52));
        let hash_order = b.txid().to_lendian().cmp(&a.txid().to_lendian());
        let offset = if hash_order == Ordering::Less {
            -1
        } else {
            1
        };
        let fee_a = 20_000_000_000_000 * i64::from(a.size()) + offset;
        let fee_b = 20_000_000_000_000 * i64::from(b.size()) + offset;
        assert_eq!(fee_a as f64 * f64::from(b.size()), fee_b as f64 * f64::from(a.size()));
        let exact_order = (i128::from(fee_b) * i128::from(a.size()))
            .cmp(&(i128::from(fee_a) * i128::from(b.size())));
        assert_eq!(exact_order, hash_order.reverse());
        for (tx, fee) in [(a.clone(), fee_a), (b.clone(), fee_b)] {
            pool.insert(
                TxInfo {
                    tx,
                    time_s: 0,
                },
                1,
                fee,
            );
        }
        assert_eq!(pool.compare_depth_and_score(&a.txid(), &b.txid()), hash_order);
        let mut ids = vec![a.txid(), b.txid()];
        ids.sort_by_key(|h| Reverse(h.to_lendian()));
        assert_eq!(pool.info_all().iter().map(|i| i.tx.txid()).collect::<Vec<_>>(), ids);
    }

    #[tokio::test]
    async fn defaults_are_empty_and_refuse_admission() {
        let pool = NoTxPool;
        let tx = tx(1, &[]);
        assert!(matches!(
            pool.accept(tx.clone(), None).await,
            AcceptResult {
                result_type: AcceptResultType::Invalid,
                validation_result: TxValidationResult::NoMempool,
                vsize: None,
                base_fee: None,
                ..
            }
        ));
        assert_eq!(pool.size(), 0);
        assert_eq!(pool.dynamic_memory_usage(), 0);
        assert!(!pool.exists(&tx.txid()));
        assert!(pool.info(&tx.txid()).is_none());
        assert!(pool.asset_unlock_by_instance_hash(&tx.instance_hash()).is_none());
        assert!(pool.info_all().is_empty());
        assert!(pool.recent_parents(&tx.txid(), 0).is_empty());
        assert!(pool.unbroadcast().is_empty());
        pool.remove_unbroadcast(&tx.txid(), false);
        pool.remove_unbroadcast(&tx.txid(), true);
        pool.for_each_in_vtxhashes_order(&mut |_, _| panic!("empty pool"));
        assert_eq!(pool.compare_depth_and_score(&tx.txid(), &tx.txid()), Ordering::Equal);
        assert!(NoIsLocks.islock_hash_by_txid(&tx.txid()).is_none());
        assert!(NoIsLocks.islock_by_hash(&[0; 32]).is_none());
        assert!(NoCoinJoinObjects.dstx_by_hash(&tx.txid()).is_none());
        assert!(NoCoinJoinObjects.queue_by_hash(&[0; 32]).is_none());
    }
}
