//! The P2P actor's mempool: dashd-p2p's [`TxPool`] and [`CompactTxSource`] over MP1a's
//! [`MempoolHandle`].

use std::cmp::Ordering as CmpOrdering;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_consensus::{TxRef, TxValidationResult};
use dash_p2p::PeerId;
use dash_primitives::TxHash;
use dashd_chainstate::chain::WaitError;
use dashd_mempool::api::{MempoolHandle, TxMempoolInfo};
use dashd_mempool::types::{MempoolAcceptResult, MempoolAcceptResultType};
use dashd_p2p::processing::compact::CompactTxSource;
use dashd_p2p::processing::seams::{
    AcceptResult, AcceptResultType, AcceptWait, TxInfo, TxIntakeHooks, TxPool,
};

/// [`TxPool`] and [`CompactTxSource`] over one [`MempoolHandle`]. Once an admission reply fails
/// (the pool shut down), it is closed for good: that admission and every later one answer
/// [`NoTxPool`](dashd_p2p::processing::seams::NoTxPool)'s result, and `enabled()` is false.
pub struct NodeTxPool {
    handle: Arc<dyn MempoolHandle>,
    closed: Arc<AtomicBool>,
}

impl NodeTxPool {
    pub fn new(handle: Arc<dyn MempoolHandle>) -> Self {
        Self {
            handle,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// [`TxIntakeHooks`] over one [`MempoolHandle`]: `ValidateDSTX`'s `PrioritiseTransaction`,
/// posted to the pool's admission FIFO ahead of the DSTX's own admission, so msghand never waits
/// on the pool's worker or V for it.
// Core: net_processing.cpp:3716, :4959 at v24.0.2
pub struct NodeTxIntake(pub Arc<dyn MempoolHandle>);

impl TxIntakeHooks for NodeTxIntake {
    fn prioritize_transaction(&self, txid: &TxHash, delta: i64) {
        self.0.post_prioritise(txid, delta);
    }
}

fn no_mempool() -> AcceptResult {
    AcceptResult {
        result_type: AcceptResultType::Invalid,
        validation_result: TxValidationResult::NoMempool,
        reason: "no-mempool".into(),
        debug_message: String::new(),
        vsize: None,
        base_fee: None,
    }
}

fn accept_result(r: MempoolAcceptResult) -> AcceptResult {
    AcceptResult {
        result_type: match r.result_type {
            MempoolAcceptResultType::Valid => AcceptResultType::Valid,
            MempoolAcceptResultType::Invalid => AcceptResultType::Invalid,
            MempoolAcceptResultType::MempoolEntry => AcceptResultType::MempoolEntry,
            MempoolAcceptResultType::Unavailable => AcceptResultType::Unavailable,
        },
        validation_result: r.validation_result,
        reason: r.reason,
        debug_message: r.debug_message,
        vsize: r.vsize,
        base_fee: r.base_fee,
    }
}

fn tx_info(info: TxMempoolInfo) -> TxInfo {
    TxInfo {
        tx: info.tx,
        time_s: info.time_s,
    }
}

impl CompactTxSource for NodeTxPool {
    fn for_each_in_vtxhashes_order(&self, f: &mut dyn FnMut(&TxHash, &TxRef) -> bool) {
        self.handle.with_relay_read(&mut |read| read.for_each_in_vtxhashes_order(f));
    }
}

impl TxPool for NodeTxPool {
    fn enabled(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
    }

    fn accept(&self, tx: TxRef, from: Option<PeerId>) -> AcceptWait {
        if self.closed.load(Ordering::Acquire) {
            return AcceptWait::ready(no_mempool());
        }
        let reply = self.handle.accept_async(tx, from.and_then(|id| u64::try_from(id).ok()));
        let closed = Arc::clone(&self.closed);
        AcceptWait(Box::pin(async move {
            match reply.await {
                Ok(result) => accept_result(result),
                Err(e) => {
                    debug_assert_eq!(e, WaitError::Disconnected, "msghand carries no no-wait mark");
                    closed.store(true, Ordering::Release);
                    no_mempool()
                }
            }
        }))
    }

    fn info(&self, txid: &TxHash) -> Option<TxInfo> {
        self.handle.read(|read| read.info(txid)).map(tx_info)
    }

    fn asset_unlock_by_instance_hash(&self, instance_hash: &TxHash) -> Option<TxHash> {
        self.handle.read(|read| read.asset_unlock_by_instance_hash(instance_hash))
    }

    fn exists(&self, txid: &TxHash) -> bool {
        self.handle.read(|read| read.exists(txid))
    }

    fn recent_parents(&self, txid: &TxHash, since_s: i64) -> Vec<TxHash> {
        self.handle.read(|read| read.parents_entered_after(txid, since_s))
    }

    fn info_all(&self) -> Vec<TxInfo> {
        self.handle.read(|read| read.info_all()).into_iter().map(tx_info).collect()
    }

    fn compare_depth_and_score(&self, a: &TxHash, b: &TxHash) -> CmpOrdering {
        self.handle.read(|read| read.compare_depth_and_score(a, b))
    }

    fn unbroadcast(&self) -> Vec<TxHash> {
        self.handle.read(|read| read.unbroadcast())
    }

    fn remove_unbroadcast(&self, txid: &TxHash, unchecked: bool) {
        self.handle.remove_unbroadcast(txid, unchecked);
    }

    fn size(&self) -> usize {
        self.handle.read(|read| read.size())
    }

    fn dynamic_memory_usage(&self) -> usize {
        self.handle.read(|read| read.dynamic_memory_usage())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::sync::Mutex;
    use std::task::{Context, Poll, Waker};

    use dash_consensus::{Amount, Tx};
    use dash_primitives::{OutPoint, Transaction, TxIn, TxOut, TxType};
    use dashd_chainstate::chain::{ActorReply, reply};
    use dashd_mempool::api::RelayRead;
    use dashd_mempool::testkit::FakeIsMempool;
    use dashd_mempool::types::{Package, PackageMempoolAcceptResult};

    use super::*;

    pub(in crate::p2p) fn tx(n: u32, inputs: &[OutPoint]) -> TxRef {
        Arc::new(Tx::new(Transaction::new(
            2,
            TxType::new(0),
            inputs.iter().map(|outpoint| TxIn::new(*outpoint, vec![].into(), u32::MAX)).collect(),
            vec![TxOut::from_duffs(1, vec![0x51].into())],
            n,
            vec![],
        )))
    }

    pub(in crate::p2p) fn result(
        result_type: MempoolAcceptResultType,
        validation_result: TxValidationResult,
    ) -> MempoolAcceptResult {
        MempoolAcceptResult {
            result_type,
            validation_result,
            reason: format!("{result_type:?}-reason"),
            debug_message: format!("{validation_result:?}-debug"),
            vsize: Some(191),
            base_fee: Some(2_500),
            effective_feerate: Some(13_089),
            txids_fee_calculations: Some(vec![TxHash::from([3; 32])]),
        }
    }

    /// A [`FakeIsMempool`] whose admissions of the txids in `dead` reply `Disconnected`, as a
    /// pool that shut down does; it counts the reads that reach it.
    #[derive(Default)]
    pub(in crate::p2p) struct Dying {
        pub(in crate::p2p) fake: FakeIsMempool,
        pub(in crate::p2p) dead: Mutex<Vec<TxHash>>,
        pub(in crate::p2p) reads: Mutex<usize>,
        pub(in crate::p2p) removed: Mutex<Vec<(TxHash, bool)>>,
        pub(in crate::p2p) posted: Mutex<Vec<(TxHash, Amount)>>,
    }

    impl MempoolHandle for Dying {
        fn accept_async(&self, tx: TxRef, from: Option<u64>) -> ActorReply<MempoolAcceptResult> {
            if self.dead.lock().unwrap().contains(&tx.txid()) {
                let _ = self.fake.accept_async(Arc::clone(&tx), from).try_recv();
                let (sender, receiver) = reply();
                drop(sender);
                return receiver;
            }
            self.fake.accept_async(tx, from)
        }

        fn accept(&self, tx: TxRef, bypass_limits: bool) -> Result<MempoolAcceptResult, WaitError> {
            self.fake.accept(tx, bypass_limits)
        }

        fn test_accept(
            &self,
            tx: TxRef,
            bypass_limits: bool,
        ) -> Result<MempoolAcceptResult, WaitError> {
            self.fake.test_accept(tx, bypass_limits)
        }

        fn process_new_package(
            &self,
            txns: Package,
            test_accept: bool,
        ) -> Result<PackageMempoolAcceptResult, WaitError> {
            self.fake.process_new_package(txns, test_accept)
        }

        fn prioritise(&self, txid: &TxHash, fee_delta: Amount) {
            self.fake.prioritise(txid, fee_delta);
        }

        fn post_prioritise(&self, txid: &TxHash, fee_delta: Amount) {
            self.posted.lock().unwrap().push((*txid, fee_delta));
            self.fake.post_prioritise(txid, fee_delta);
        }

        fn add_unbroadcast(&self, txid: &TxHash) {
            self.fake.add_unbroadcast(txid);
        }

        fn remove_unbroadcast(&self, txid: &TxHash, unchecked: bool) {
            self.removed.lock().unwrap().push((*txid, unchecked));
            self.fake.remove_unbroadcast(txid, unchecked);
        }

        fn with_relay_read(&self, f: &mut dyn FnMut(&dyn RelayRead)) {
            *self.reads.lock().unwrap() += 1;
            self.fake.with_relay_read(f);
        }
    }

    fn poll_now(mut wait: AcceptWait) -> Option<AcceptResult> {
        let mut cx = Context::from_waker(Waker::noop());
        match wait.0.as_mut().poll(&mut cx) {
            Poll::Ready(result) => Some(result),
            Poll::Pending => None,
        }
    }

    fn pool() -> (Arc<Dying>, NodeTxPool) {
        let dying = Arc::new(Dying::default());
        let pool = NodeTxPool::new(Arc::clone(&dying) as _);
        (dying, pool)
    }

    /// Every result type and validation result comes through field for field; the fields
    /// `AcceptResult` lacks are dropped.
    #[test]
    fn each_result_type_maps_field_for_field() {
        let (dying, pool) = pool();
        let cases = [
            (MempoolAcceptResultType::Valid, AcceptResultType::Valid, TxValidationResult::Unset),
            (
                MempoolAcceptResultType::Invalid,
                AcceptResultType::Invalid,
                TxValidationResult::Consensus,
            ),
            (
                MempoolAcceptResultType::Invalid,
                AcceptResultType::Invalid,
                TxValidationResult::MissingInputs,
            ),
            (
                MempoolAcceptResultType::MempoolEntry,
                AcceptResultType::MempoolEntry,
                TxValidationResult::Unset,
            ),
            (
                MempoolAcceptResultType::Unavailable,
                AcceptResultType::Unavailable,
                TxValidationResult::Unset,
            ),
        ];
        for (n, (from, to, validation)) in cases.into_iter().enumerate() {
            let t = tx(n as u32, &[]);
            let scripted = result(from, validation);
            dying.fake.script(t.txid(), scripted.clone());
            let got = poll_now(pool.accept(t, Some(1))).expect("a scripted reply is ready");
            assert_eq!(
                got,
                AcceptResult {
                    result_type: to,
                    validation_result: validation,
                    reason: scripted.reason,
                    debug_message: scripted.debug_message,
                    vsize: Some(191),
                    base_fee: Some(2_500),
                }
            );
        }
        assert!(pool.enabled());
    }

    /// A reply that waits keeps the wait pending until the pool resolves it.
    #[test]
    fn an_unresolved_admission_waits_for_the_pool() {
        let (dying, pool) = pool();
        let t = tx(1, &[]);
        let mut wait = pool.accept(Arc::clone(&t), None);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(wait.0.as_mut().poll(&mut cx).is_pending());
        let missing = result(MempoolAcceptResultType::Invalid, TxValidationResult::MissingInputs);
        assert!(dying.fake.resolve(&t.txid(), missing));
        let Poll::Ready(got) = wait.0.as_mut().poll(&mut cx) else {
            panic!("resolved");
        };
        assert_eq!(got.result_type, AcceptResultType::Invalid);
        assert_eq!(got.validation_result, TxValidationResult::MissingInputs);
    }

    /// `from` reaches the handle as the peer's u64; a negative id (an orphan retried from a
    /// block) and `None` reach it as `None`.
    #[test]
    fn from_reaches_the_handle_as_the_peer_s_u64() {
        let (dying, pool) = pool();
        for (n, from) in [Some(7), Some(0), Some(-1), None].into_iter().enumerate() {
            let t = tx(n as u32, &[]);
            dying.fake.script(
                t.txid(),
                result(MempoolAcceptResultType::Valid, TxValidationResult::Unset),
            );
            assert!(poll_now(pool.accept(t, from)).is_some());
        }
        let froms: Vec<_> = dying.fake.submissions().iter().map(|s| s.from).collect();
        assert_eq!(froms, [Some(7), Some(0), None, None]);
    }

    /// A reply the pool dropped (`Disconnected`) answers `NoTxPool`'s result and closes the
    /// adapter: `enabled()` is false and a later admission never reaches the handle.
    #[test]
    fn disconnected_answers_no_mempool_and_closes() {
        let (dying, pool) = pool();
        let first = tx(1, &[]);
        dying.dead.lock().unwrap().push(first.txid());
        let got = poll_now(pool.accept(Arc::clone(&first), Some(4))).expect("ready");
        assert_eq!(got, no_mempool());
        assert!(!pool.enabled());
        assert_eq!(dying.fake.submissions().len(), 1);
        let second = tx(2, &[]);
        dying.fake.script(
            second.txid(),
            result(MempoolAcceptResultType::Valid, TxValidationResult::Unset),
        );
        assert_eq!(poll_now(pool.accept(second, Some(4))), Some(no_mempool()));
        assert_eq!(dying.fake.submissions().len(), 1);
    }

    /// `info` and `info_all` keep the admission time and the pool's order.
    #[test]
    fn info_and_info_all_keep_time_and_order() {
        let (dying, pool) = pool();
        let parent = tx(1, &[]);
        let child = tx(2, &[OutPoint::new(parent.txid(), 0)]);
        let other = tx(3, &[]);
        dying.fake.insert_entry(Arc::clone(&child), 30, 1_000_000);
        dying.fake.insert_entry(Arc::clone(&parent), 10, 1);
        dying.fake.insert_entry(Arc::clone(&other), 20, 1_000);
        let got: Vec<_> = pool.info_all().iter().map(|i| (i.tx.txid(), i.time_s)).collect();
        assert_eq!(got, [(other.txid(), 20), (parent.txid(), 10), (child.txid(), 30)]);
        let info = pool.info(&child.txid()).expect("present");
        assert_eq!((info.tx.txid(), info.time_s), (child.txid(), 30));
        assert!(pool.info(&TxHash::from([9; 32])).is_none());
        assert!(pool.exists(&parent.txid()));
        assert!(!pool.exists(&TxHash::from([9; 32])));
    }

    /// The compact walk keeps the fake's order and stops at the first `false`.
    #[test]
    fn the_compact_walk_keeps_order_and_stops_early() {
        let (dying, pool) = pool();
        let txs: Vec<_> = (0..4).map(|n| tx(n, &[])).collect();
        for t in [&txs[2], &txs[0], &txs[3], &txs[1]] {
            dying.fake.insert(Arc::clone(t));
        }
        let walk = |limit: usize| {
            let mut seen = Vec::new();
            pool.for_each_in_vtxhashes_order(&mut |hash, tx| {
                assert_eq!(*hash, tx.instance_hash());
                seen.push(tx.txid());
                seen.len() < limit
            });
            seen
        };
        let order: Vec<_> = [2, 0, 3, 1].iter().map(|&i| txs[i].txid()).collect();
        assert_eq!(walk(usize::MAX), order);
        assert_eq!(walk(2), order[..2]);
        assert_eq!(*dying.reads.lock().unwrap(), 2);
    }

    /// `ValidateDSTX`'s delta reaches the handle as a post, never the waiting `prioritise`, and a
    /// real pool records it for the DSTX's admission.
    // Core: net_processing.cpp:3716 at v24.0.2
    #[test]
    fn the_intake_hook_posts_the_dstx_delta_to_the_pool() {
        let dying = Arc::new(Dying::default());
        let hook = NodeTxIntake(Arc::clone(&dying) as _);
        let txid = tx(1, &[]).txid();
        hook.prioritize_transaction(&txid, 10_000_000);
        assert_eq!(*dying.posted.lock().unwrap(), [(txid, 10_000_000)]);
        assert_eq!(dying.fake.fee_delta(&txid), 10_000_000);

        let pool = Arc::new(dashd_mempool::pool::Mempool::default());
        NodeTxIntake(Arc::clone(&pool) as _).prioritize_transaction(&txid, 10_000_000);
        assert_eq!(pool.read().deltas().get(&txid), Some(&10_000_000));
    }

    /// The other reads, and `remove_unbroadcast`, reach the handle.
    #[test]
    fn the_other_reads_reach_the_handle() {
        let (dying, pool) = pool();
        let (low, high) = {
            let (a, b) = (tx(1, &[]), tx(2, &[]));
            if a.txid() < b.txid() {
                (a, b)
            } else {
                (b, a)
            }
        };
        let child = tx(3, &[OutPoint::new(high.txid(), 0), OutPoint::new(low.txid(), 0)]);
        dying.fake.insert_entry(Arc::clone(&low), 20, 1);
        dying.fake.insert_entry(Arc::clone(&high), 30, 1);
        dying.fake.insert_entry(Arc::clone(&child), 40, 1);
        dying.fake.set_dynamic_memory_usage(12_345);
        *dying.reads.lock().unwrap() = 0;

        assert_eq!(pool.recent_parents(&child.txid(), 10), [low.txid(), high.txid()]);
        assert_eq!(pool.recent_parents(&child.txid(), 20), [high.txid()]);
        assert_eq!(pool.compare_depth_and_score(&low.txid(), &child.txid()), CmpOrdering::Less);
        let absent = TxHash::from([9; 32]);
        assert_eq!(pool.compare_depth_and_score(&absent, &low.txid()), CmpOrdering::Less);
        assert!(pool.asset_unlock_by_instance_hash(&low.txid()).is_none());
        assert_eq!(pool.size(), 3);
        assert_eq!(pool.dynamic_memory_usage(), 12_345);
        assert_eq!(*dying.reads.lock().unwrap(), 7);

        dying.add_unbroadcast(&low.txid());
        dying.add_unbroadcast(&high.txid());
        assert_eq!(pool.unbroadcast(), [low.txid(), high.txid()]);
        pool.remove_unbroadcast(&low.txid(), true);
        assert_eq!(*dying.removed.lock().unwrap(), [(low.txid(), true)]);
        assert_eq!(pool.unbroadcast(), [high.txid()]);
    }
}
