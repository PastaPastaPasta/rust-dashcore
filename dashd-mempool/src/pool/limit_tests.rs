//! MP1b-G2: `Expire`, `TrimToSize`, `trackPackageRemoved` and `GetMinFee` on the pool
//! (`txmempool.cpp:2007-2027, :2057-2125` at v24.0.2).

use std::collections::BTreeSet;
use std::sync::Arc;

use dash_clock::MockClock;
use dash_consensus::{Tx, TxRef};
use dash_dml::MnList;
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};

use super::{ROLLING_FEE_HALFLIFE, fee_rate_to_string};
use crate::accept::single::fee_rate;
use crate::is::IsAcceptHooks;
use crate::options::MemPoolOptions;
use crate::pool::{EntryMetadata, PendingEvent, PoolState};
use crate::types::IsConflict;
use crate::types::MemPoolRemovalReason::{self as Reason, Expiry, SizeLimit};

const NOW: i64 = 1_700_000_000;
const INCREMENTAL: i64 = 1_000;

fn tx(n: u32, inputs: &[OutPoint]) -> TxRef {
    Arc::new(Tx::new(Transaction::new(
        2,
        TxType::new(0),
        inputs.iter().map(|p| TxIn::new(*p, vec![].into(), u32::MAX)).collect(),
        vec![TxOut::from_duffs(1, vec![0x51].into()); 2],
        n,
        vec![],
    )))
}

fn out(tx: &Tx, n: u32) -> OutPoint {
    OutPoint::new(tx.txid(), n)
}

fn external(n: u8) -> OutPoint {
    OutPoint::new(TxHash::from([n; 32]), 0)
}

fn pool_at(options: MemPoolOptions) -> (PoolState, Arc<MockClock>) {
    let mut pool = PoolState::new(options);
    let clock = Arc::new(MockClock::at(NOW));
    pool.set_clock(clock.clone());
    (pool, clock)
}

fn add(pool: &mut PoolState, tx: &TxRef, fee: i64, time: i64) {
    let entry = pool.new_entry(
        tx.clone(),
        EntryMetadata {
            fee,
            time,
            ..Default::default()
        },
    );
    pool.add_unchecked(entry, &MnList::default_pre_dip3());
    pool.check();
}

fn removed(pool: &mut PoolState) -> Vec<(TxHash, Reason)> {
    pool.take_pending_events()
        .into_iter()
        .map(|event| {
            let PendingEvent::TransactionRemovedFromMempool {
                tx,
                reason,
                ..
            } = event;
            (tx.txid(), reason)
        })
        .collect()
}

fn in_txid_order(txs: &[&TxRef], reason: Reason) -> Vec<(TxHash, Reason)> {
    let ids: BTreeSet<_> = txs.iter().map(|tx| tx.txid()).collect();
    ids.into_iter().map(|id| (id, reason)).collect()
}

/// InstantSend with `IsLocked` true for these txids.
struct Locked(BTreeSet<TxHash>);

impl IsAcceptHooks for Locked {
    fn is_enabled(&self) -> bool {
        true
    }
    fn conflicting_lock(&self, _: &Tx) -> Option<IsConflict> {
        None
    }
    fn is_waiting_for_tx(&self, _: &TxHash) -> bool {
        false
    }
    fn is_locked(&self, txid: &TxHash) -> bool {
        self.0.contains(txid)
    }
}

/// `Expire` (`tm:2007-2027`): the entries older than the cutoff by entry time go with their
/// descendants (a newer child included) as EXPIRY, in one `RemoveStaged`, so in txid order; a
/// locked one stays and so does its newer child (`tm:2013-2016`); one exactly at the cutoff
/// stays. It returns the number removed.
#[test]
fn expire_removes_old_entries_with_descendants_and_keeps_locked_ones() {
    let (mut pool, _) = pool_at(MemPoolOptions::default());
    let old = tx(1, &[external(1)]);
    let old_child = tx(2, &[out(&old, 0)]);
    let locked = tx(3, &[external(3)]);
    let locked_child = tx(4, &[out(&locked, 0)]);
    let at_cutoff = tx(5, &[external(5)]);
    let fresh = tx(6, &[external(6)]);
    add(&mut pool, &old, 1_000, NOW - 100);
    add(&mut pool, &old_child, 1_000, NOW);
    add(&mut pool, &locked, 1_000, NOW - 100);
    add(&mut pool, &locked_child, 1_000, NOW);
    add(&mut pool, &at_cutoff, 1_000, NOW - 50);
    add(&mut pool, &fresh, 1_000, NOW);
    pool.is_hooks = Arc::new(Locked(BTreeSet::from([locked.txid()])));

    assert_eq!(pool.expire(NOW - 50), 2);
    assert_eq!(removed(&mut pool), in_txid_order(&[&old, &old_child], Expiry));
    let kept: BTreeSet<_> = pool.entries().keys().copied().collect();
    let want = [&locked, &locked_child, &at_cutoff, &fresh].map(|tx| tx.txid());
    assert_eq!(kept, BTreeSet::from(want));
    pool.check();
    assert_eq!(pool.expire(NOW - 50), 0);
    assert!(removed(&mut pool).is_empty());
}

/// `LimitMempoolSize`'s expiry (`v:401`) cuts at `GetTime() - m_expiry`, the pool clock's now.
#[test]
fn limit_size_expires_at_now_minus_the_expiry() {
    let (mut pool, clock) = pool_at(MemPoolOptions {
        expiry_secs: 60,
        ..MemPoolOptions::default()
    });
    let older = tx(1, &[external(1)]);
    let at = tx(2, &[external(2)]);
    add(&mut pool, &older, 1_000, NOW - 61);
    add(&mut pool, &at, 1_000, NOW - 60);
    assert!(pool.limit_size().is_empty());
    assert_eq!(removed(&mut pool), in_txid_order(&[&older], Expiry));
    clock.set_mocktime(NOW + 1);
    pool.limit_size();
    assert_eq!(removed(&mut pool), in_txid_order(&[&at], Expiry));
    assert!(pool.is_empty());
}

/// `CompareTxMemPoolEntryByDescendantScore` (`tmh:82-122`): an entry scores the higher of its
/// own feerate and its package's with descendants, so a cheap parent of a rich child (CPFP)
/// outranks a mid-fee single; equal scores put the later entry first.
#[test]
fn the_descendant_score_index_is_cores_order() {
    let (mut pool, _) = pool_at(MemPoolOptions::default());
    let parent = tx(1, &[external(1)]);
    let child = tx(2, &[out(&parent, 0)]);
    let single = tx(3, &[external(3)]);
    let early = tx(4, &[external(4)]);
    let late = tx(5, &[external(5)]);
    add(&mut pool, &parent, 10, NOW);
    add(&mut pool, &child, 100_000, NOW);
    add(&mut pool, &single, 5_000, NOW);
    add(&mut pool, &early, 1_000, NOW - 1);
    add(&mut pool, &late, 1_000, NOW);
    let order: Vec<_> = pool.descendant_score_order().collect();
    assert_eq!(order, [&late, &early, &single, &parent, &child].map(|tx| tx.txid()));
}

/// `TrimToSize` (`tm:2089-2125`): only what brings the usage under the limit goes, the first of
/// the descendant-score index each time, with its descendants as SIZELIMIT; each removal tracks
/// its package feerate plus the incremental relay feerate (`tm:2081-2087`), the rolling minimum
/// then the highest of them, read back before any block without decay.
#[test]
fn trim_evicts_the_lowest_descendant_score_package_and_bumps_the_rolling_fee() {
    let (mut pool, clock) = pool_at(MemPoolOptions::default());
    let parent = tx(1, &[external(1)]);
    let child = tx(2, &[out(&parent, 0)]);
    let single = tx(3, &[external(3)]);
    add(&mut pool, &parent, 10, NOW);
    add(&mut pool, &child, 100_000, NOW);
    add(&mut pool, &single, 5_000, NOW);
    assert_eq!(pool.get_min_fee(), 0);

    let usage = pool.dynamic_memory_usage();
    assert!(pool.trim_to_size(usage).is_empty(), "at the limit nothing goes");
    let no_spends = pool.trim_to_size(usage - 1);
    assert_eq!(removed(&mut pool), in_txid_order(&[&single], SizeLimit));
    assert_eq!(no_spends, vec![external(3)]);
    let single_rate = fee_rate(5_000, single.size().into()) + INCREMENTAL;
    assert_eq!(pool.get_min_fee(), single_rate);
    pool.check();

    let package = pool.get_iter(&parent.txid()).unwrap().descendants();
    let package_rate = fee_rate(package.modified_fees, package.size as i64) + INCREMENTAL;
    assert!(package_rate > single_rate);
    let no_spends = pool.trim_to_size(0);
    assert_eq!(removed(&mut pool), in_txid_order(&[&parent, &child], SizeLimit));
    // Every input of the stage whose transaction left the pool, the child's parent included.
    let mut want = vec![external(1), out(&parent, 0)];
    if child.txid() < parent.txid() {
        want.reverse();
    }
    assert_eq!(no_spends, want);
    assert!(pool.is_empty());
    clock.set_mocktime(NOW + 10 * ROLLING_FEE_HALFLIFE);
    assert_eq!(pool.get_min_fee(), package_rate, "no decay before a block");
}

/// `vNoSpendsRemaining` (`tm:2117-2122`): a trimmed transaction's input whose transaction is
/// still in the pool is not returned; its other inputs are.
#[test]
fn trim_returns_only_prevouts_whose_transaction_left_the_pool() {
    let (mut pool, _) = pool_at(MemPoolOptions::default());
    let rich = tx(1, &[external(1)]);
    let cheap = tx(2, &[out(&rich, 0), external(2)]);
    add(&mut pool, &rich, 100_000, NOW);
    add(&mut pool, &cheap, 10, NOW);
    let no_spends = pool.trim_to_size(pool.dynamic_memory_usage() - 1);
    assert_eq!(removed(&mut pool), in_txid_order(&[&cheap], SizeLimit));
    assert_eq!(no_spends, vec![external(2)]);
    assert!(pool.exists(&rich.txid()));
}

/// `GetMinFee` (`tm:2057-2079`): no decay until `removeForBlock` (`tm:1348-1349`) has come
/// after the last bump, and none within ten seconds of the last update; then halving every
/// `ROLLING_FEE_HALFLIFE`, a quarter of it while the usage is under a quarter of the limit and
/// half under half; at least the incremental relay feerate while it lasts, then zero once under
/// half of it, and zero stays. A later bump stops the decay again until the next block, which
/// restarts it from the block's time.
#[test]
fn the_rolling_minimum_decays_by_cores_halflife_steps() {
    let (mut pool, clock) = pool_at(MemPoolOptions::default());
    let list = MnList::default_pre_dip3();
    assert_eq!(pool.dynamic_memory_usage(), 0);
    let (quarter, half, full) = (usize::MAX, 2, 0);
    pool.track_package_removed(100_000);
    clock.set_mocktime(NOW + 100 * ROLLING_FEE_HALFLIFE);
    assert_eq!(pool.min_fee(quarter), 100_000, "no block since the bump");

    let mut t = NOW + 100 * ROLLING_FEE_HALFLIFE;
    pool.remove_for_block(&[], &list);
    clock.set_mocktime(t + 10);
    assert_eq!(pool.min_fee(quarter), 100_000, "within ten seconds");
    t += ROLLING_FEE_HALFLIFE / 4;
    clock.set_mocktime(t);
    assert_eq!(pool.min_fee(quarter), 50_000);
    t += ROLLING_FEE_HALFLIFE / 2;
    clock.set_mocktime(t);
    assert_eq!(pool.min_fee(half), 25_000);
    t += ROLLING_FEE_HALFLIFE;
    clock.set_mocktime(t);
    assert_eq!(pool.min_fee(full), 12_500);
    t += 4 * ROLLING_FEE_HALFLIFE;
    clock.set_mocktime(t);
    assert_eq!(pool.min_fee(full), INCREMENTAL, "781.25 is raised to the incremental feerate");
    let partial = t + 1_000;
    clock.set_mocktime(partial);
    assert_eq!(pool.min_fee(full), INCREMENTAL);
    let decayed = 781.25 / 2f64.powf(1_000.0 / ROLLING_FEE_HALFLIFE as f64);
    assert!(decayed > 500.0);
    assert_eq!(pool.rolling.lock().rate, decayed);
    t = partial + ROLLING_FEE_HALFLIFE;
    clock.set_mocktime(t);
    assert_eq!(pool.min_fee(full), 0, "under half the incremental feerate");
    assert_eq!(pool.rolling.lock().rate, 0.0);
    clock.set_mocktime(t + 1);
    assert_eq!(pool.min_fee(full), 0);

    pool.track_package_removed(80_000);
    clock.set_mocktime(t + 2 * ROLLING_FEE_HALFLIFE);
    assert_eq!(pool.min_fee(full), 80_000, "a bump waits for the next block");
    let block = t + 3 * ROLLING_FEE_HALFLIFE;
    clock.set_mocktime(block);
    pool.remove_for_block(&[], &list);
    clock.set_mocktime(block + ROLLING_FEE_HALFLIFE);
    assert_eq!(pool.min_fee(full), 40_000, "decay from the block's time");
    pool.track_package_removed(30_000);
    assert_eq!(pool.min_fee(full), 40_000, "a lower package does not bump");
}

/// `CFeeRate::ToString` (`policy/feerate.cpp:39-45`) for `TrimToSize`'s log line.
#[test]
fn fee_rates_print_as_core_prints_them() {
    assert_eq!(fee_rate_to_string(1_000), "0.00001000 DASH/kB");
    assert_eq!(fee_rate_to_string(123_456_789), "1.23456789 DASH/kB");
    assert_eq!(fee_rate_to_string(0), "0.00000000 DASH/kB");
}
