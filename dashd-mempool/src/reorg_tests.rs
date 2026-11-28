//! MP1b-R: `MaybeUpdateMempoolForReorg`'s re-add over a real pool and the stage-B fake V. Each
//! test cites the Core v24.0.0 lines it pins (`v:` is validation.cpp, `tm:` txmempool.cpp).

use std::sync::Arc;

use dash_clock::MockClock;
use dash_consensus::coins::Coin;
use dash_consensus::{Tx, TxRef};
use dash_dml::MnList;
use dash_primitives::{OutPoint, TxHash, TxType};
use dashd_chainstate::chain::Fatal;

use super::{Readded, finish_reorg, readd_disconnected};
use crate::accept::stage_b_tests::{
    ACCEPT_TIME, Ev, FakeChain, Recorder, SIG_TRUE, TIP, out, p2sh_true, pay, tx_of,
};
use crate::accept::tests::{outpoint, params};
use crate::options::{MemPoolLimits, MemPoolOptions};
use crate::pool::{EntryMetadata, PoolState};
use crate::types::LockPoints;
use crate::types::MemPoolRemovalReason::{Expiry, Reorg, SizeLimit};

const NON_FINAL: u32 = 0xffff_fffe;

/// A reorg's held pool and V at the new tip.
struct Fx {
    state: PoolState,
    chain: FakeChain,
    events: Vec<Ev>,
}

impl Fx {
    fn new() -> Fx {
        Fx::with_options(MemPoolOptions::default())
    }

    /// A held pool whose `GetTime()` is [`ACCEPT_TIME`].
    fn with_options(options: MemPoolOptions) -> Fx {
        let mut state = PoolState::new(options);
        state.set_clock(Arc::new(MockClock::at(ACCEPT_TIME)));
        Fx {
            state,
            chain: FakeChain::new(),
            events: Vec::new(),
        }
    }

    /// A confirmed coin of 100,000 at height 100 paying P2SH(`OP_TRUE`).
    fn fund(&mut self, byte: u8) -> OutPoint {
        let op = outpoint(byte, 0);
        self.chain.coins.coins.insert(op, Coin::new(100_000, &p2sh_true(), 100, false));
        op
    }

    /// `tx` put in the pool as an entry admitted before the reorg at the tip, with no in-pool
    /// parent and no relative lock.
    fn hold(&mut self, tx: &TxRef) {
        self.hold_with(tx, false);
    }

    fn hold_with(&mut self, tx: &TxRef, spends_coinbase: bool) {
        self.hold_at(tx, spends_coinbase, ACCEPT_TIME);
    }

    fn hold_at(&mut self, tx: &TxRef, spends_coinbase: bool, time: i64) {
        let metadata = EntryMetadata {
            fee: 1_000,
            time,
            spends_coinbase,
            lock_points: LockPoints {
                height: -1,
                time: -1,
                max_input_block: Some(self.chain.tip),
            },
            ..EntryMetadata::default()
        };
        let entry = self.state.new_entry(tx.clone(), metadata);
        self.state.add_unchecked(entry, &MnList::default_pre_dip3());
    }

    /// The re-add of `queued` (`disconnectpool.take()` order), adding at [`ACCEPT_TIME`].
    fn readd(&mut self, queued: &[TxRef]) -> Readded {
        let add = Some((&mut self.chain as &mut dyn crate::accept::AcceptChain, ACCEPT_TIME));
        readd_disconnected(&mut self.state, queued, add, &mut Recorder(&mut self.events))
    }

    fn finish(&mut self, readded: Readded) -> Result<(), Fatal> {
        let mut recorder = Recorder(&mut self.events);
        finish_reorg(&mut self.state, params(), readded, &mut self.chain, &mut recorder)
    }

    fn held(&self, tx: &Tx) -> bool {
        self.state.exists(&tx.txid())
    }

    fn take_events(&mut self) -> Vec<Ev> {
        std::mem::take(&mut self.events)
    }

    fn added(&self, tx: &TxRef, sequence: u64) -> Ev {
        Ev::Added(tx.txid(), tx.instance_hash(), ACCEPT_TIME, sequence)
    }

    fn ancestors(&self, tx: &Tx) -> u64 {
        self.state.get_iter(&tx.txid()).expect("held").ancestors().count
    }

    fn descendants(&self, tx: &Tx) -> u64 {
        self.state.get_iter(&tx.txid()).expect("held").descendants().count
    }
}

fn removed(tx: &Tx, reason: crate::types::MemPoolRemovalReason, sequence: u64) -> Ev {
    Ev::Removed(tx.txid(), reason, sequence)
}

/// A coinbase paying 50,000 to P2SH(`OP_TRUE`).
fn coinbase() -> TxRef {
    let null = OutPoint::new(TxHash::from([0; 32]), u32::MAX);
    tx_of(
        2,
        TxType::Spend,
        &[(null, &[0x01, 0x01][..], u32::MAX)],
        &[(50_000, &p2sh_true())],
        0,
        vec![],
    )
}

/// The re-add (`v:439-458`), iterating `take()` in reverse: the block's coinbase is never
/// offered to ATMP and goes with `removeRecursive(REORG)`, taking its in-pool spender; the
/// other transaction is admitted through `AcceptToMemoryPool` at the drain's time with
/// `bypass_limits` (`TransactionAddedToMempool`, `v:1455`) and joins `vHashUpdate`.
#[test]
fn a_disconnected_transaction_is_readded_and_its_coinbase_is_not() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let tx = pay(&[coin], &[90_000]);
    let cb = coinbase();
    let cb_spender = pay(&[out(&cb, 0)], &[40_000]);
    fx.hold(&cb_spender);
    let sequence = fx.state.sequence();

    let readded = fx.readd(&[tx.clone(), cb.clone()]);
    assert!(readded.fault.is_none());
    assert_eq!(readded.update, vec![tx.txid()]);
    fx.finish(readded).expect("no fault");

    assert_eq!(
        fx.take_events(),
        vec![removed(&cb_spender, Reorg, sequence), fx.added(&tx, sequence + 1), Ev::Flushed]
    );
    assert!(fx.held(&tx) && !fx.held(&cb) && !fx.held(&cb_spender));
    let entry = fx.state.get_iter(&tx.txid()).expect("held");
    assert_eq!((entry.metadata().time, entry.metadata().height), (ACCEPT_TIME, TIP));
    fx.state.check();
}

/// A transaction the new chain made invalid (an input no longer in CoinsTip): ATMP refuses it
/// (`bad-txns-inputs-missingorspent`), the verdict is ignored, and `removeRecursive(REORG)`
/// takes its in-pool descendants (`v:449-458`, `tm:907-935`). The coin it cached is uncached as
/// `AcceptToMemoryPool` does for a refusal (`v:1743-1754`).
#[test]
fn a_transaction_the_new_chain_invalidates_is_removed_with_its_descendants() {
    let mut fx = Fx::new();
    let present = fx.fund(1);
    let tx = pay(&[present, outpoint(2, 0)], &[90_000]);
    let child = pay(&[out(&tx, 0)], &[80_000]);
    let grandchild = pay(&[out(&child, 0)], &[70_000]);
    fx.hold(&child);
    fx.hold(&grandchild);
    let sequence = fx.state.sequence();

    let readded = fx.readd(std::slice::from_ref(&tx));
    assert!(readded.update.is_empty() && readded.fault.is_none());
    fx.finish(readded).expect("no fault");

    let seen = fx.take_events();
    let mut expected = [child.txid(), grandchild.txid()];
    expected.sort();
    let mut expected: Vec<_> =
        expected.iter().zip(sequence..).map(|(t, s)| Ev::Removed(*t, Reorg, s)).collect();
    expected.insert(0, Ev::Flushed);
    assert_eq!(seen, expected);
    assert!(fx.state.is_empty());
    assert!(!fx.chain.coins.cached.contains(&present), "the refusal uncached its coin");
}

/// `UpdateTransactionsFromBlock` (`tm:108-161`, `UpdateForDescendants` `tm:57-106`): a re-added
/// parent is linked to its in-pool children outside `vHashUpdate`, and every descendant of it
/// (through a re-added child too) takes it into its ancestor state while it takes them into its
/// descendant state. A child that is itself in `vHashUpdate` was linked by its own admission and
/// is not counted twice.
#[test]
fn a_readded_parent_takes_in_its_pool_descendants() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let parent = pay(&[coin], &[40_000, 40_000]);
    let sibling = pay(&[out(&parent, 1)], &[30_000]);
    let child = pay(&[out(&parent, 0)], &[30_000]);
    let grandchild = pay(&[out(&child, 0)], &[20_000]);
    let nephew = pay(&[out(&sibling, 0)], &[20_000]);
    for tx in [&child, &grandchild, &nephew] {
        fx.hold(tx);
    }
    assert_eq!(fx.ancestors(&child), 1, "admitted while its parent was mined");

    let readded = fx.readd(&[sibling.clone(), parent.clone()]);
    assert_eq!(readded.update, vec![parent.txid(), sibling.txid()]);
    fx.finish(readded).expect("no fault");

    let state = &fx.state;
    let entry = |tx: &Tx| state.get_iter(&tx.txid()).expect("held");
    assert!(entry(&parent).children().contains(&child.txid()));
    assert!(entry(&child).parents().contains(&parent.txid()));
    assert!(entry(&sibling).children().contains(&nephew.txid()));
    assert_eq!(fx.ancestors(&child), 2);
    assert_eq!(fx.ancestors(&grandchild), 3);
    assert_eq!(fx.ancestors(&nephew), 3);
    assert_eq!(fx.descendants(&parent), 5);
    assert_eq!(fx.descendants(&sibling), 2);
    let sizes: u64 =
        [&parent, &sibling, &child, &grandchild, &nephew].iter().map(|tx| entry(tx).vsize()).sum();
    assert_eq!(entry(&parent).descendants().size, sizes);
    fx.state.check();
}

/// `UpdateForDescendants`' limit check (`tm:97-101`, `:153-160`): a descendant whose ancestor
/// count now exceeds the pool's limit leaves recursively as SIZELIMIT once every entry is
/// updated, after the re-add's admission event.
#[test]
fn a_descendant_over_the_ancestor_limit_leaves_as_sizelimit() {
    let mut fx = Fx::with_options(MemPoolOptions {
        limits: MemPoolLimits {
            ancestor_count: 2,
            ..MemPoolLimits::default()
        },
        ..MemPoolOptions::default()
    });
    let coin = fx.fund(1);
    let parent = pay(&[coin], &[90_000]);
    let child = pay(&[out(&parent, 0)], &[80_000]);
    let grandchild = pay(&[out(&child, 0)], &[70_000]);
    fx.hold(&child);
    fx.hold(&grandchild);
    let sequence = fx.state.sequence();

    let readded = fx.readd(std::slice::from_ref(&parent));
    fx.finish(readded).expect("no fault");

    assert_eq!(
        fx.take_events(),
        vec![
            fx.added(&parent, sequence),
            Ev::Flushed,
            removed(&grandchild, SizeLimit, sequence + 1)
        ]
    );
    assert!(fx.held(&parent) && fx.held(&child) && !fx.held(&grandchild));
    assert_eq!(fx.ancestors(&child), 2);
    fx.state.check();
}

/// Core's order of events (`v:439-524`): each disconnected transaction's outcome in reverse
/// `take()` order (the coinbase's spender, an admission, a refused transaction's descendants),
/// then `removeForReorg`'s removal of an entry no longer final at the new tip; each at the
/// pool's next sequence.
#[test]
fn the_events_follow_cores_order() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let cb = coinbase();
    let cb_spender = pay(&[out(&cb, 0)], &[40_000]);
    let admitted = pay(&[coin], &[90_000]);
    let refused = pay(&[outpoint(2, 0)], &[90_000]);
    let refused_child = pay(&[out(&refused, 0)], &[80_000]);
    let lock = (outpoint(3, 0), &SIG_TRUE[..], NON_FINAL);
    let non_final = tx_of(2, TxType::Spend, &[lock], &[(1_000, &p2sh_true())], TIP + 5, vec![]);
    for tx in [&cb_spender, &refused_child, &non_final] {
        fx.hold(tx);
    }
    let s = fx.state.sequence();

    let readded = fx.readd(&[refused.clone(), admitted.clone(), cb]);
    fx.finish(readded).expect("no fault");

    assert_eq!(
        fx.take_events(),
        vec![
            removed(&cb_spender, Reorg, s),
            fx.added(&admitted, s + 1),
            Ev::Flushed,
            Ev::Flushed,
            removed(&refused_child, Reorg, s + 2),
            removed(&non_final, Reorg, s + 3),
        ]
    );
    assert_eq!(fx.state.entries().keys().copied().collect::<Vec<_>>(), [admitted.txid()]);
    fx.state.check();
}

/// Without `fAddToMempool` (`v:449`), nothing is offered to ATMP: every disconnected transaction
/// takes the removal branch, its in-pool descendants with it.
#[test]
fn a_non_adding_drain_removes_every_disconnected_transaction() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let tx = pay(&[coin], &[90_000]);
    let child = pay(&[out(&tx, 0)], &[80_000]);
    fx.hold(&child);
    let sequence = fx.state.sequence();
    let mut recorder = Recorder(&mut fx.events);
    let readded = readd_disconnected(&mut fx.state, std::slice::from_ref(&tx), None, &mut recorder);
    assert!(readded.update.is_empty());
    fx.finish(readded).expect("no fault");
    assert_eq!(fx.take_events(), vec![removed(&child, Reorg, sequence)]);
    assert!(fx.state.is_empty());
    assert!(fx.chain.flushes.is_empty(), "no ATMP, no PERIODIC flush");
}

/// The m1 fault case, in the re-add: a coins fault in a later transaction's ATMP stops the loop,
/// but what was re-added before it is finished (`UpdateTransactionsFromBlock`), so the pool
/// stays consistent, and its events are kept for the caller; `removeForReorg` does not run.
#[test]
fn a_readd_fault_leaves_a_consistent_pool_and_keeps_its_events() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let faulting = fx.fund(2);
    fx.chain.coins.faults.insert(faulting);
    let parent = pay(&[coin], &[90_000]);
    let child = pay(&[out(&parent, 0)], &[80_000]);
    let stopped = pay(&[faulting], &[90_000]);
    let lock = (outpoint(3, 0), &SIG_TRUE[..], NON_FINAL);
    let non_final = tx_of(2, TxType::Spend, &[lock], &[(1_000, &p2sh_true())], TIP + 5, vec![]);
    fx.hold(&child);
    fx.hold(&non_final);
    let sequence = fx.state.sequence();

    let readded = fx.readd(&[stopped.clone(), parent.clone()]);
    assert_eq!(readded.update, vec![parent.txid()]);
    assert_eq!(readded.fault, Some(Fatal::new("injected coins read fault")));
    assert_eq!(fx.finish(readded), Err(Fatal::new("injected coins read fault")));

    assert_eq!(fx.take_events(), vec![fx.added(&parent, sequence), Ev::Flushed]);
    assert_eq!(fx.chain.flushes.len(), 1, "the faulting ATMP does not flush");
    assert!(fx.held(&non_final), "removeForReorg did not run");
    assert!(!fx.held(&stopped));
    assert_eq!(fx.ancestors(&child), 2, "the re-added parent is linked");
    fx.state.check();
}

/// A PERIODIC flush fault after a VALID re-add: the admission stands, so the re-add is in
/// `vHashUpdate` and `UpdateTransactionsFromBlock` links it before the fault is returned.
#[test]
fn a_flush_fault_after_a_readd_still_links_it() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let parent = pay(&[coin], &[90_000]);
    let child = pay(&[out(&parent, 0)], &[80_000]);
    fx.hold(&child);
    fx.chain.flush_fault = Some(Fatal::new("injected flush fault"));

    let readded = fx.readd(std::slice::from_ref(&parent));
    assert_eq!(readded.update, vec![parent.txid()]);
    assert_eq!(readded.fault, Some(Fatal::new("injected flush fault")));
    assert_eq!(fx.finish(readded), Err(Fatal::new("injected flush fault")));

    assert!(fx.held(&parent));
    assert_eq!(fx.ancestors(&child), 2, "the re-added parent is linked");
    fx.state.check();
}

/// The m1 fault case, in the filter: after the re-add and `UpdateTransactionsFromBlock`, a coins
/// fault in `filter_final_and_mature` applies nothing of `removeForReorg`; the re-add stays, the
/// pool consistent, and its events are kept for the caller.
#[test]
fn a_filter_fault_leaves_a_consistent_pool_and_keeps_its_events() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let parent = pay(&[coin], &[90_000]);
    let child = pay(&[out(&parent, 0)], &[80_000]);
    let lock = (outpoint(3, 0), &SIG_TRUE[..], NON_FINAL);
    let non_final = tx_of(2, TxType::Spend, &[lock], &[(1_000, &p2sh_true())], TIP + 5, vec![]);
    let coinbase_spend = pay(&[outpoint(4, 0)], &[1_000]);
    fx.chain.coins.faults.insert(outpoint(4, 0));
    fx.hold(&child);
    fx.hold(&non_final);
    fx.hold_with(&coinbase_spend, true);
    let sequence = fx.state.sequence();

    let readded = fx.readd(std::slice::from_ref(&parent));
    assert_eq!(fx.finish(readded), Err(Fatal::new("injected coins read fault")));

    assert_eq!(fx.take_events(), vec![fx.added(&parent, sequence), Ev::Flushed]);
    assert!(fx.held(&non_final), "removeForReorg applied nothing");
    assert_eq!(fx.ancestors(&child), 2);
    fx.state.check();
}

/// `AcceptToMemoryPool`'s tail (`v:1757-1759` at v24.0.2): each re-add's ATMP call, a refused one
/// too, ends with `FlushStateToDisk(PERIODIC)` inside the hold, after that call's events and
/// reading the held pool's usage as it stands then; the coinbase, never offered to ATMP, flushes
/// nothing.
#[test]
fn each_readd_atmp_call_flushes_periodic_with_the_held_pools_usage() {
    let mut fx = Fx::new();
    let coin_a = fx.fund(1);
    let coin_b = fx.fund(2);
    let a = pay(&[coin_a], &[90_000]);
    let b = pay(&[coin_b], &[90_000]);
    let refused = pay(&[outpoint(9, 0)], &[90_000]);
    let cb = coinbase();
    let sequence = fx.state.sequence();

    let readded = fx.readd(&[b.clone(), refused, a.clone(), cb]);
    assert!(readded.fault.is_none());
    let after_both = fx.state.dynamic_memory_usage() as u64;
    assert_eq!(
        fx.take_events(),
        vec![
            fx.added(&a, sequence),
            Ev::Flushed,
            Ev::Flushed,
            fx.added(&b, sequence + 1),
            Ev::Flushed,
        ]
    );
    let flushes = fx.chain.flushes.clone();
    assert_eq!(flushes.len(), 3);
    assert!(0 < flushes[0] && flushes[0] < after_both, "{flushes:?}");
    assert_eq!((flushes[1], flushes[2]), (flushes[0], after_both));

    let mut alone = Fx::new();
    let coin = alone.fund(1);
    assert_eq!(coin, coin_a);
    let readded = alone.readd(std::slice::from_ref(&a));
    assert!(readded.fault.is_none());
    assert_eq!(alone.chain.flushes, [flushes[0]], "the usage of a pool holding only `a`");
}

/// `LimitMempoolSize` after `removeForReorg` (`v:519-524` at v24.0.2), over a pool the re-add
/// (with `bypass_limits`) took past its limit: `Expire` takes the entry older than the expiry as
/// EXPIRY, then `TrimToSize` the cheapest package as SIZELIMIT, and the trimmed entry's prevout
/// is uncached through the reorg context (`ReorgCx::uncache`, `v:407-409`).
#[test]
fn the_reorgs_limit_expires_then_trims_and_uncaches_through_the_context() {
    let probe = |hold_old: bool| {
        let mut fx = Fx::new();
        let (cheap, rich, old) = limit_txs(&mut fx);
        fx.hold(&cheap);
        if hold_old {
            fx.hold(&old);
        }
        let readded = fx.readd(std::slice::from_ref(&rich));
        fx.finish(readded).expect("no fault");
        fx.state.dynamic_memory_usage()
    };
    let full = probe(false);
    assert!(probe(true) > full);
    let mut fx = Fx::with_options(MemPoolOptions {
        max_size_bytes: full as i64 - 1,
        ..MemPoolOptions::default()
    });
    let (cheap, rich, old) = limit_txs(&mut fx);
    let expiry = fx.state.options().expiry_secs;
    fx.hold(&cheap);
    fx.hold_at(&old, false, ACCEPT_TIME - expiry - 1);
    fx.chain.coins.cached.insert(cheap.inputs()[0].prevout());
    let sequence = fx.state.sequence();

    let readded = fx.readd(std::slice::from_ref(&rich));
    fx.finish(readded).expect("no fault");

    assert_eq!(
        fx.take_events(),
        vec![
            fx.added(&rich, sequence),
            Ev::Flushed,
            removed(&old, Expiry, sequence + 1),
            removed(&cheap, SizeLimit, sequence + 2),
        ]
    );
    assert_eq!(fx.state.entries().keys().copied().collect::<Vec<_>>(), [rich.txid()]);
    let cached = &fx.chain.coins.cached;
    assert!(!cached.contains(&cheap.inputs()[0].prevout()), "vNoSpendsRemaining is uncached");
    assert!(cached.contains(&rich.inputs()[0].prevout()));
    fx.state.check();
}

/// A held entry paying 1,000, a disconnected transaction paying 10,000, and an unrelated held
/// one to expire.
fn limit_txs(fx: &mut Fx) -> (TxRef, TxRef, TxRef) {
    let rich_coin = fx.fund(2);
    let cheap = pay(&[outpoint(1, 0)], &[99_000]);
    let rich = pay(&[rich_coin], &[90_000]);
    let old = pay(&[outpoint(3, 0)], &[99_000]);
    (cheap, rich, old)
}

/// dashpay/dash#8098: a re-add that meets a missing MNHF row (`EvoDbMissingStateError`) is not
/// admitted: the transaction's in-pool descendants are removed, no PERIODIC flush runs, and the
/// re-add carries no fault.
// Core: validation.cpp:449-451, :1114 at v24.0.3; evo/mnhftx.cpp:286-290
#[test]
fn a_readd_that_meets_a_missing_mnhf_row_is_not_admitted() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let tx = pay(&[coin], &[90_000]);
    let child = pay(&[out(&tx, 0)], &[80_000]);
    fx.hold(&child);
    let sequence = fx.state.sequence();
    fx.chain.rules_fault = Some(dashd_chainstate::chain::AdmissionFault::Unavailable(
        "MNHF signals unavailable for block 00ab".into(),
    ));
    let readded = fx.readd(std::slice::from_ref(&tx));
    assert!(readded.update.is_empty());
    assert!(readded.fault.is_none());
    fx.finish(readded).expect("no fault");
    assert_eq!(fx.take_events(), vec![removed(&child, Reorg, sequence)]);
    assert!(fx.state.is_empty());
    assert!(fx.chain.flushes.is_empty(), "no PERIODIC flush");
}
