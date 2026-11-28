//! The real pool behind V's seams: [`PoolSink`]'s holds, [`IsMempool`]'s lock removal and the
//! compact-block walk, with their events read back through a started signals thread.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use dash_consensus::coins::Coin;
use dash_consensus::ctx::ChainAncestry;
use dash_consensus::{BlockId, Tx, TxRef};
use dash_dml::MnList;
use dash_dml::testkit::MemChain;
use dash_dml::testkit::replay::mainnet;
use dash_pkc::bls::BlsGlobalSig;
use dash_primitives::{
    AssetUnlock, OutPoint, QuorumHash, Transaction, TxHash, TxIn, TxOut, TxType,
};
use dash_types::codec::BaseCodec;
use dashd_chainstate::chain::{
    Blocking, DisconnectPool, Fatal, MempoolSink, RecordingAbort, RemovalReason, ReorgCx,
    SignalsThread, Staged, TipFacts, ValidationInterface, ValidationSignals,
};

use super::PoolSink;
use crate::is::IsMempool;
use crate::pool::{EntryMetadata, Mempool, PoolState};
use crate::types::LockPoints;

const OP_TRUE: u8 = 0x51;
const NON_FINAL: u32 = 0xffff_fffe;

fn outpoint(byte: u8) -> OutPoint {
    OutPoint::new(TxHash::from([byte; 32]), 0)
}

/// A version-2 spend of `inputs` (`(outpoint, nSequence)`) with two `OP_TRUE` outputs, `tag`
/// making it distinct.
fn spend(inputs: &[(OutPoint, u32)], lock_time: u32, tag: i64) -> TxRef {
    Arc::new(Tx::new(Transaction::new(
        2,
        TxType::Spend,
        inputs.iter().map(|&(prevout, seq)| TxIn::new(prevout, vec![].into(), seq)).collect(),
        vec![TxOut::from_duffs(1_000 + tag, vec![OP_TRUE].into()); 2],
        lock_time,
        vec![],
    )))
}

fn child(parent: &Tx, tag: i64) -> TxRef {
    spend(&[(OutPoint::new(parent.txid(), 0), u32::MAX)], 0, tag)
}

/// A version-`version` asset unlock of `index` requested at `requested`, paying `amount`.
fn unlock_v(version: u8, index: u64, requested: u32, amount: i64) -> TxRef {
    let sig = BlsGlobalSig::decode(&[0; 96]).expect("decodes");
    let p = AssetUnlock::new(version, index, 1_000, requested, QuorumHash::from([1; 32]), sig);
    let mut payload = Vec::new();
    p.encode(&mut payload);
    Arc::new(Tx::new(Transaction::new(
        3,
        TxType::AssetUnlock,
        vec![],
        vec![TxOut::from_duffs(amount, vec![OP_TRUE].into())],
        0,
        payload,
    )))
}

fn unlock(index: u64, requested: u32, amount: i64) -> TxRef {
    unlock_v(2, index, requested, amount)
}

/// The lock points of a transaction without relative locks: `maxInputBlock` genesis.
fn unlocked(chain: &MemChain) -> LockPoints {
    LockPoints {
        height: -1,
        time: -1,
        max_input_block: Some(chain.genesis()),
    }
}

fn add_with(state: &mut PoolState, tx: &TxRef, lock_points: LockPoints, spends_coinbase: bool) {
    let metadata = EntryMetadata {
        fee: 1_000,
        lock_points,
        spends_coinbase,
        ..EntryMetadata::default()
    };
    let entry = state.new_entry(tx.clone(), metadata);
    state.add_unchecked(entry, &MnList::default_pre_dip3());
}

fn add(state: &mut PoolState, tx: &TxRef) {
    add_with(state, tx, LockPoints::default(), false);
}

/// Each removal a listener saw: its reason, sequence and txid.
#[derive(Default)]
struct Removals(Mutex<Vec<(String, u64, TxHash)>>);

impl ValidationInterface for Removals {
    fn transaction_removed_from_mempool(&self, tx: &TxRef, reason: RemovalReason, sequence: u64) {
        self.0.lock().expect("removals").push((format!("{reason:?}"), sequence, tx.txid()));
    }
}

/// A started signals thread with a [`Removals`] listener.
struct Listened {
    signals: ValidationSignals,
    removals: Arc<Removals>,
    _thread: SignalsThread,
}

impl Listened {
    fn new() -> Listened {
        let signals = ValidationSignals::default();
        let removals = Arc::new(Removals::default());
        signals.register(removals.clone());
        let thread = signals.start(Arc::new(RecordingAbort::default())).expect("signals thread");
        Listened {
            signals,
            removals,
            _thread: thread,
        }
    }

    /// `staged` spliced as V's release does, then what the listener saw since the last call.
    fn published(&self, staged: &mut Staged) -> Vec<(String, u64, TxHash)> {
        self.signals.splice(staged);
        self.seen()
    }

    fn seen(&self) -> Vec<(String, u64, TxHash)> {
        self.signals.sync(&mut Blocking).expect("the signals thread runs");
        std::mem::take(&mut *self.removals.0.lock().expect("removals"))
    }
}

fn sink(pool: &Arc<Mempool>) -> PoolSink {
    PoolSink::new(pool.clone(), Arc::new(mainnet().clone()))
}

fn reasons(seen: &[(String, u64, TxHash)]) -> BTreeSet<(String, TxHash)> {
    seen.iter().map(|(reason, _, txid)| (reason.clone(), *txid)).collect()
}

fn ev(reason: RemovalReason, txid: TxHash) -> (String, TxHash) {
    (format!("{reason:?}"), txid)
}

/// The sequences of `seen` are the pool's next ones, in order.
fn consecutive(seen: &[(String, u64, TxHash)]) -> bool {
    seen.windows(2).all(|w| w[1].1 == w[0].1 + 1)
}

/// `removeForBlock` (`txmempool.cpp:1321-1353` at v24.0.0): a mined entry leaves with no
/// notification (`removeUnchecked`'s `reason != BLOCK`, `:483-485`) and its in-pool children stay;
/// each block transaction's conflicts leave recursively as CONFLICT, in the block's order; the
/// removals are staged in the order they occur.
#[test]
fn block_removal_stages_conflicts_and_keeps_mined_entries_silent() {
    let pool = Arc::new(Mempool::default());
    let listened = Listened::new();
    let mined = spend(&[(outpoint(1), u32::MAX)], 0, 1);
    let mined_child = child(&mined, 2);
    let rival = spend(&[(outpoint(2), u32::MAX)], 0, 3);
    let rival_child = child(&rival, 4);
    let rival_grandchild = child(&rival_child, 5);
    let other = spend(&[(outpoint(3), u32::MAX)], 0, 6);
    let late_rival = spend(&[(outpoint(3), u32::MAX)], 0, 7);
    {
        let mut state = pool.write();
        for tx in [&mined, &mined_child, &rival, &rival_child, &rival_grandchild, &late_rival] {
            add(&mut state, tx);
        }
    }
    let block_spend = spend(&[(outpoint(2), u32::MAX)], 0, 8);
    let mut s = sink(&pool);
    let mut held = s.begin_hold().expect("a hold");
    let mut staged = Staged::default();
    let tip_list = Arc::new(MnList::default_pre_dip3());
    held.remove_for_block_at(&[mined.clone(), block_spend, other], 7, &tip_list, &mut staged);
    drop(held);

    let seen = listened.published(&mut staged);
    let conflicts: Vec<_> = seen.iter().map(|(_, _, txid)| *txid).collect();
    assert_eq!(conflicts.last(), Some(&late_rival.txid()), "the block's third transaction last");
    assert_eq!(
        reasons(&seen),
        BTreeSet::from([
            ev(RemovalReason::Conflict, rival.txid()),
            ev(RemovalReason::Conflict, rival_child.txid()),
            ev(RemovalReason::Conflict, rival_grandchild.txid()),
            ev(RemovalReason::Conflict, late_rival.txid()),
        ])
    );
    assert!(consecutive(&seen), "{seen:?}");
    let state = pool.read();
    assert_eq!(state.entries().keys().copied().collect::<Vec<_>>(), [mined_child.txid()]);
    state.check();
}

/// `removeExpiredAssetUnlock(nHeight)` (`txmempool.cpp:1355-1369` at v24.0.0): a version-1
/// unlock leaves as EXPIRY once its `requestedHeight + HEIGHT_DIFF_EXPIRING` (48) is below the
/// connected height, not at it; a version-2 unlock (a stable txid) has no expiry row
/// (`:677-684`) and stays.
#[test]
fn asset_unlock_expires_after_its_expiry_height() {
    let pool = Arc::new(Mempool::default());
    let listened = Listened::new();
    let au = unlock_v(1, 7, 100, 5_000);
    let stable = unlock(8, 100, 5_000);
    add(&mut pool.write(), &au);
    add(&mut pool.write(), &stable);
    let mut s = sink(&pool);
    let mut held = s.begin_hold().expect("a hold");
    let mut staged = Staged::default();
    held.remove_expired_asset_unlock(148, &mut staged);
    assert!(staged.is_empty(), "at the expiry height it stays");
    held.remove_expired_asset_unlock(149, &mut staged);
    drop(held);
    let seen = listened.published(&mut staged);
    assert_eq!(reasons(&seen), BTreeSet::from([ev(RemovalReason::Expiry, au.txid())]));
    assert_eq!(pool.read().entries().keys().copied().collect::<Vec<_>>(), [stable.txid()]);
}

/// V's reorg context over a MemChain: the active tip, its coins, and an outpoint whose read fails.
struct Cx {
    chain: MemChain,
    tip: BlockId,
    coins: BTreeMap<OutPoint, Coin>,
    fault: Option<OutPoint>,
}

impl Cx {
    fn at(&self, height: u32) -> BlockId {
        self.chain.ancestor(self.tip, height).expect("on the active chain")
    }
}

impl TipFacts for Cx {
    fn tip(&self) -> Option<BlockId> {
        Some(self.tip)
    }
    fn ancestry(&self) -> &dyn ChainAncestry {
        &self.chain
    }
}

impl ReorgCx for Cx {
    fn chain_contains(&self, block: BlockId) -> bool {
        self.chain.ancestor(self.tip, self.chain.header(block).height) == Some(block)
    }
    fn access_coin(&mut self, outpoint: &OutPoint) -> Result<Option<Coin>, Fatal> {
        if self.fault == Some(*outpoint) {
            return Err(Fatal::new("injected coins read fault"));
        }
        Ok(self.coins.get(outpoint).cloned())
    }
    fn uncache(&mut self, _: &[OutPoint]) -> Result<(), Fatal> {
        Ok(())
    }
}

/// A reorg's pool: the descendants of a disconnected transaction, a transaction final only at the
/// old tip's child with its child, a spend of a coinbase immature at the new tip's child, an
/// entry whose lock points name a side-branch block, and an untouched entry.
struct Reorged {
    pool: Arc<Mempool>,
    cx: Cx,
    disconnected: TxRef,
    descendants: [TxRef; 2],
    non_final: [TxRef; 2],
    immature: TxRef,
    stale: TxRef,
    stale_points: LockPoints,
    kept: TxRef,
}

/// The confirmed coin [`Reorged::stale`] spends with a 3-block relative lock.
const CONFIRMED: u8 = 20;
const COINBASE: u8 = 21;

impl Reorged {
    /// The new tip is at 129 (the old one, 130, disconnected).
    fn new() -> Reorged {
        Reorged::with_pool(Mempool::default())
    }

    /// [`Reorged::new`] over `pool`, empty, one second after its entries' time 0, so the drain's
    /// `LimitMempoolSize` expires none of them.
    fn with_pool(pool: Mempool) -> Reorged {
        pool.set_clock(Arc::new(dash_clock::MockClock::at(1)));
        let mut chain = MemChain::new();
        let tip = chain.extend(chain.genesis(), 129);
        let fork = chain.ancestor(tip, 100).expect("main chain");
        let side = chain.extend(fork, 20);
        let coins = BTreeMap::from([
            (outpoint(CONFIRMED), Coin::new(10_000, &[OP_TRUE], 122, false)),
            (outpoint(COINBASE), Coin::new(10_000, &[OP_TRUE], 31, true)),
        ]);
        let cx = Cx {
            chain,
            tip,
            coins,
            fault: None,
        };

        let disconnected = spend(&[(outpoint(10), u32::MAX)], 0, 0);
        let d1 = child(&disconnected, 1);
        let d2 = child(&d1, 2);
        let n1 = spend(&[(outpoint(11), NON_FINAL)], 130, 3);
        let n2 = child(&n1, 4);
        let immature = spend(&[(outpoint(COINBASE), u32::MAX)], 0, 5);
        let stale = spend(&[(outpoint(CONFIRMED), 3)], 0, 6);
        let stale_points = LockPoints {
            height: 110,
            time: -1,
            max_input_block: cx.chain.ancestor(side, 110),
        };
        let kept = spend(&[(outpoint(12), u32::MAX)], 0, 7);
        let pool = Arc::new(pool);
        {
            let mut state = pool.write();
            let open = unlocked(&cx.chain);
            for tx in [&d1, &d2, &n1, &n2, &kept] {
                add_with(&mut state, tx, open, false);
            }
            add_with(&mut state, &immature, open, true);
            add_with(&mut state, &stale, stale_points, false);
        }
        Reorged {
            pool,
            cx,
            disconnected,
            descendants: [d1, d2],
            non_final: [n1, n2],
            immature,
            stale,
            stale_points,
            kept,
        }
    }

    fn disconnect_pool(&self) -> Box<dyn DisconnectPool> {
        let mut s = sink(&self.pool);
        let mut pool = s.new_disconnect_pool();
        assert!(pool.add_from_block(std::slice::from_ref(&self.disconnected)).is_empty());
        pool
    }

    fn lock_points(&self, tx: &Tx) -> LockPoints {
        self.pool.read().get_iter(&tx.txid()).expect("resident").metadata().lock_points
    }
}

/// `MaybeUpdateMempoolForReorg` without `fAddToMempool` (`validation.cpp:425-525` at v24.0.0):
/// `removeRecursive(tx, REORG)` takes a disconnected transaction's in-pool descendants
/// (`:449-458`); `removeForReorg` (`txmempool.cpp:937-956`) then removes, with descendants, an
/// entry no longer final at the new tip (`v:482`) and a spend of a coinbase now immature
/// (`v:504-515`), and refreshes the lock points of an entry whose `maxInputBlock` left the chain
/// (`v:492-500`); every removal is REORG and staged.
#[test]
fn reorg_removes_descendants_and_filtered_entries_and_refreshes_lock_points() {
    let mut r = Reorged::new();
    let listened = Listened::new();
    let mut s = sink(&r.pool);
    let mut held = s.begin_hold().expect("a hold");
    let mut staged = Staged::default();
    held.maybe_update_for_reorg_at(r.disconnect_pool(), None, &mut r.cx, &mut staged)
        .expect("no fault");
    drop(held);

    let seen = listened.published(&mut staged);
    let [d1, d2] = &r.descendants;
    let [n1, n2] = &r.non_final;
    assert_eq!(
        reasons(&seen[..2]),
        BTreeSet::from([ev(RemovalReason::Reorg, d1.txid()), ev(RemovalReason::Reorg, d2.txid())]),
        "the disconnected transaction's descendants first"
    );
    assert_eq!(
        reasons(&seen),
        [d1, d2, n1, n2, &r.immature]
            .iter()
            .map(|tx| ev(RemovalReason::Reorg, tx.txid()))
            .collect::<BTreeSet<_>>()
    );
    assert!(consecutive(&seen), "{seen:?}");
    let resident: BTreeSet<_> = r.pool.read().entries().keys().copied().collect();
    assert_eq!(resident, BTreeSet::from([r.stale.txid(), r.kept.txid()]));
    let fresh = LockPoints {
        height: 124,
        time: -1,
        max_input_block: Some(r.cx.at(122)),
    };
    assert_ne!(r.stale_points, fresh);
    assert_eq!(r.lock_points(&r.stale), fresh);
    assert_eq!(r.lock_points(&r.kept), unlocked(&r.cx.chain));
    r.pool.read().check();
}

const FAULT: u8 = 22;

/// A coins read fault in the filter (`ReorgCx::access_coin`, `coins.cpp:355-369`) fails the hold
/// before `removeForReorg` applies anything: no lock points refreshed, no filtered entry removed.
/// The faulting entry is the last the filter visits, so every other verdict is already in hand.
/// The REORG removals before the filter stay applied, the pool consistent, and since V discards
/// the hold's staged stream (§8.2) they are queued to the pool's queue instead; dropping the hold
/// releases the guard (MP1a-8 review m1).
#[test]
fn reorg_filter_fault_applies_nothing_and_queues_the_removals_before_it() {
    let listened = Listened::new();
    let mut r =
        Reorged::with_pool(Mempool::with_signals(Default::default(), listened.signals.clone()));
    let last = r.pool.read().entries().keys().copied().max().expect("entries");
    let faulting = (100..)
        .map(|tag| spend(&[(outpoint(FAULT), u32::MAX)], 0, tag))
        .find(|tx| tx.txid() > last)
        .expect("a txid after every entry");
    add_with(&mut r.pool.write(), &faulting, unlocked(&r.cx.chain), true);
    r.cx.fault = Some(outpoint(FAULT));
    let before: BTreeSet<_> = r.pool.read().entries().keys().copied().collect();
    assert_eq!(before.last(), Some(&faulting.txid()), "the filter reaches the fault last");
    let mut s = sink(&r.pool);
    let mut held = s.begin_hold().expect("a hold");
    assert!(r.pool.state_lock().try_read().is_none(), "the hold owns the write guard");
    let mut staged = Staged::default();
    let fault = held.maybe_update_for_reorg_at(r.disconnect_pool(), None, &mut r.cx, &mut staged);
    assert_eq!(fault, Err(Fatal::new("injected coins read fault")));
    assert!(staged.is_empty(), "nothing for V to discard");
    drop(held);
    assert!(r.pool.state_lock().try_write().is_some(), "released with the hold");

    let resident: BTreeSet<_> = r.pool.read().entries().keys().copied().collect();
    let [d1, d2] = &r.descendants;
    let expected: BTreeSet<_> =
        before.iter().copied().filter(|t| *t != d1.txid() && *t != d2.txid()).collect();
    assert_eq!(resident, expected, "only the descendant removal before the filter ran");
    let [n1, n2] = &r.non_final;
    for tx in [n1, n2, &r.immature] {
        assert!(resident.contains(&tx.txid()), "a filtered entry stays");
    }
    assert_eq!(r.lock_points(&r.stale), r.stale_points);
    let seen = listened.seen();
    assert_eq!(
        reasons(&seen),
        BTreeSet::from([ev(RemovalReason::Reorg, d1.txid()), ev(RemovalReason::Reorg, d2.txid())])
    );
    assert!(consecutive(&seen), "{seen:?}");
    r.pool.read().check();
}

/// An adding drain (`tip_list` given) runs its re-add in the admission turn V lends through
/// `ReorgCx::with_turn`; a context without one is Core's assert, before anything is applied.
#[test]
fn an_adding_drain_without_an_admission_turn_is_fatal() {
    let mut r = Reorged::new();
    let before: BTreeSet<_> = r.pool.read().entries().keys().copied().collect();
    let mut s = sink(&r.pool);
    let mut held = s.begin_hold().expect("a hold");
    let mut staged = Staged::default();
    let tip_list = Arc::new(MnList::default_pre_dip3());
    let fault = held.maybe_update_for_reorg_at(
        r.disconnect_pool(),
        Some(&tip_list),
        &mut r.cx,
        &mut staged,
    );
    assert_eq!(fault, Err(Fatal::assert("an adding reorg drain without its admission turn")));
    drop(held);
    assert!(staged.is_empty());
    assert_eq!(r.pool.read().entries().keys().copied().collect::<BTreeSet<_>>(), before);
}

/// The hold owns the pool's write guard from `begin_hold` until V drops it, and publishes the
/// usage telemetry on release; the sampled `DynamicMemoryUsage` reads under a short read guard.
#[test]
fn a_hold_owns_the_write_guard_until_dropped() {
    let pool = Arc::new(Mempool::default());
    let mut s = sink(&pool);
    assert!(s.has_mempool());
    let held = s.begin_hold().expect("a hold");
    assert!(pool.state_lock().try_read().is_none());
    drop(held);
    let usage = s.dynamic_memory_usage();
    assert_eq!(usage, pool.read().dynamic_memory_usage() as u64);
    add(&mut pool.write(), &spend(&[(outpoint(9), u32::MAX)], 0, 1));
    let held = s.begin_hold().expect("a hold");
    let held_usage = held.dynamic_memory_usage();
    drop(held);
    assert_eq!(pool.usage_telemetry(), held_usage, "published on release");
    assert!(held_usage > usage);
}

/// `RemoveMempoolConflictsForLock`'s locked section (`instantsend/net_instantsend.cpp:474-505` at
/// v24.0.0): the spenders of the lock's inputs other than the locked transaction leave with their
/// descendants, then the other unlocks of a held locked unlock's index, all as CONFLICT queued to
/// the node's signals before the guard is released; the returned snapshot is the spender roots.
#[test]
fn lock_removal_removes_other_spenders_and_index_conflicts() {
    let listened = Listened::new();
    let pool = Mempool::with_signals(Default::default(), listened.signals.clone());
    let locked = unlock(7, 100, 5_000);
    let same_index = unlock(7, 100, 6_000);
    let other_index = unlock(8, 100, 6_000);
    let spender = spend(&[(outpoint(1), u32::MAX)], 0, 1);
    let spender_child = child(&spender, 2);
    let unrelated = spend(&[(outpoint(2), u32::MAX)], 0, 3);
    {
        let mut state = pool.write();
        for tx in [&locked, &same_index, &other_index, &spender, &spender_child, &unrelated] {
            add(&mut state, tx);
        }
    }
    let port: &dyn IsMempool = &pool;
    let roots = port.remove_conflicts_for_lock(&locked.txid(), &[outpoint(1), outpoint(3)]);
    assert_eq!(roots.iter().map(|tx| tx.txid()).collect::<Vec<_>>(), [spender.txid()]);
    let seen = listened.seen();
    assert_eq!(
        reasons(&seen),
        BTreeSet::from([
            ev(RemovalReason::Conflict, spender.txid()),
            ev(RemovalReason::Conflict, spender_child.txid()),
            ev(RemovalReason::Conflict, same_index.txid()),
        ])
    );
    assert_eq!(seen.last().map(|s| s.2), Some(same_index.txid()), "the index conflicts last");
    assert!(consecutive(&seen), "{seen:?}");
    let resident: BTreeSet<_> = pool.read().entries().keys().copied().collect();
    assert_eq!(resident, BTreeSet::from([locked.txid(), other_index.txid(), unrelated.txid()]));
    assert_eq!(pool.usage_telemetry(), pool.read().dynamic_memory_usage() as u64);
    assert!(pool.take_pending_events().is_empty(), "queued, not left pending");
}

/// The overlapping-root case (`net_instantsend.cpp:480-490`): the snapshot is taken before any
/// removal, so a spender that descends from another spender of the lock's inputs is returned too,
/// and each removal is notified once.
#[test]
fn lock_removal_returns_overlapping_roots_and_notifies_once() {
    let listened = Listened::new();
    let pool = Mempool::with_signals(Default::default(), listened.signals.clone());
    let parent = spend(&[(outpoint(1), u32::MAX)], 0, 1);
    let child_root = child(&parent, 2);
    let grandchild = child(&child_root, 3);
    let locked =
        spend(&[(outpoint(1), u32::MAX), (OutPoint::new(parent.txid(), 0), u32::MAX)], 0, 4);
    {
        let mut state = pool.write();
        for tx in [&parent, &child_root, &grandchild] {
            add(&mut state, tx);
        }
    }
    let inputs = [outpoint(1), OutPoint::new(parent.txid(), 0)];
    let roots = IsMempool::remove_conflicts_for_lock(&pool, &locked.txid(), &inputs);
    assert_eq!(
        roots.iter().map(|tx| tx.txid()).collect::<BTreeSet<_>>(),
        BTreeSet::from([parent.txid(), child_root.txid()])
    );
    let seen = listened.seen();
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert_eq!(
        reasons(&seen),
        [&parent, &child_root, &grandchild]
            .iter()
            .map(|tx| ev(RemovalReason::Conflict, tx.txid()))
            .collect::<BTreeSet<_>>()
    );
    assert!(pool.read().is_empty());
}

/// `PartiallyDownloadedBlock::InitData`'s walk (`blockencodings.cpp:112-139` at v24.0.0) over
/// `vTxHashes`: insertion order, a removal swapping the last entry into its slot
/// (`txmempool.cpp:726-733`), and the walk stops when the caller says so.
#[test]
fn compact_walk_follows_vtxhashes_order_and_stops() {
    let pool = Mempool::default();
    let txs: Vec<_> =
        (0..4).map(|n| spend(&[(outpoint(n + 1), u32::MAX)], 0, i64::from(n))).collect();
    {
        let mut state = pool.write();
        for tx in &txs {
            add(&mut state, tx);
        }
        state.remove_recursive(&txs[1], crate::types::MemPoolRemovalReason::Manual);
    }
    let mut order = Vec::new();
    pool.for_each_in_vtxhashes_order(&mut |instance, tx| {
        assert_eq!(*instance, tx.instance_hash());
        order.push(tx.txid());
        true
    });
    assert_eq!(order, [txs[0].txid(), txs[3].txid(), txs[2].txid()]);
    let expected: Vec<_> = pool.read().vtx_hashes().iter().map(|(_, txid)| *txid).collect();
    assert_eq!(order, expected);

    let mut first = Vec::new();
    pool.for_each_in_vtxhashes_order(&mut |_, tx| {
        first.push(tx.txid());
        false
    });
    assert_eq!(first, [txs[0].txid()]);
}
