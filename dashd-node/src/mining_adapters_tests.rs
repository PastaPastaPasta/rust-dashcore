//! The adapter over MP1a's real pool: MN1-2's selection property and the R2 tie through it
//! (t1), and the lock-free R1 counter (§3.3 (a), (c), (d)).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use dash_clock::SystemClock;
use dash_consensus::primitives::core_hash_cmp;
use dash_consensus::{Tx, TxRef};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{OutPoint, Transaction, TxIn, TxOut, TxType};
use dashd_chainstate::chain::mempool_turn::{LockOrderGuard, LockRank};
use dashd_chainstate::chain::{MempoolSink as _, Staged, ValidationSignals};
use dashd_mempool::api::MempoolHandle as _;
use dashd_mempool::options::MemPoolOptions;
use dashd_mempool::pool::{EntryMetadata, PoolState};
use dashd_mempool::sink::PoolSink;
use dashd_mempool::types::MemPoolRemovalReason;
use dashd_mining::{
    BlockState, FeeRate, Options, PackageChecks, PassAll, Selection, add_package_txs,
};
use proptest::prelude::*;

use super::*;

const PATIENCE: Duration = Duration::from_secs(30);

fn node_pool() -> Arc<NodeMempool> {
    let clock: Arc<dyn dash_clock::Clock> = Arc::new(SystemClock);
    Arc::new(NodeMempool::new(MemPoolOptions::default(), ValidationSignals::default(), clock))
}

fn tx(inputs: Vec<OutPoint>, outputs: usize, pad: usize) -> TxRef {
    let mut outs = vec![TxOut::from_duffs(1, vec![0x51].into()); outputs];
    outs.push(TxOut::from_duffs(0, vec![0x6a; pad + 1].into()));
    let ins = inputs.into_iter().map(|p| TxIn::new(p, vec![].into(), u32::MAX)).collect();
    Arc::new(Tx::new(Transaction::new(2, TxType::Spend, ins, outs, 0, vec![])))
}

fn coin(n: u32) -> OutPoint {
    OutPoint::new(TxHash::from([0xee; 32]), n)
}

fn add(state: &mut PoolState, tx: &TxRef, fee: i64, sigops: i64) {
    let metadata = EntryMetadata {
        fee,
        sigops,
        ..EntryMetadata::default()
    };
    let entry = state.new_entry(Arc::clone(tx), metadata);
    state.add_unchecked(entry, &MnList::default_pre_dip3());
}

/// `f` over the adapter's view of `mempool`, under one read guard.
fn read<R>(mempool: &Arc<NodeMempool>, f: impl FnOnce(&dyn PackageView) -> R) -> R {
    let (mut f, mut out) = (Some(f), None);
    MiningPool::new(Arc::clone(mempool)).with_read(&mut |view| out = f.take().map(|f| f(view)));
    out.expect("with_read lends one view")
}

fn txid_key(txid: &TxHash) -> u64 {
    u64::from(txid.as_bytes()[0])
}

/// Rejects every set whose txid keys sum to `residue` modulo `modulus` (none if `modulus < 2`).
#[derive(Clone, Copy, Debug)]
struct SumRule {
    modulus: u64,
    residue: u64,
}

impl SumRule {
    fn rejects<'a>(self, txids: impl Iterator<Item = &'a TxHash>) -> bool {
        self.modulus >= 2 && txids.map(txid_key).sum::<u64>() % self.modulus == self.residue
    }
}

#[derive(Clone, Copy, Debug)]
struct RejectSum {
    test_txs: SumRule,
    validate: SumRule,
}

fn txids<V: PackageView + ?Sized>(view: &V, ids: &[EntryId]) -> Vec<TxHash> {
    ids.iter().map(|&id| view.mining_entry(id).txid).collect()
}

impl PackageChecks for RejectSum {
    type Accepted = ();

    fn service_updates<V: PackageView + ?Sized>(
        &self,
        _: &V,
        _: &[EntryId],
        _: &HashSet<EntryId>,
    ) -> Vec<EntryId> {
        Vec::new()
    }

    fn test_package_transactions<V: PackageView + ?Sized>(
        &self,
        view: &V,
        package: &[EntryId],
    ) -> bool {
        !self.test_txs.rejects(txids(view, package).iter())
    }

    fn validate_package<V: PackageView + ?Sized>(
        &mut self,
        view: &V,
        sorted: &[EntryId],
        _: EntryId,
    ) -> Option<()> {
        (!self.validate.rejects(txids(view, sorted).iter())).then_some(())
    }

    fn commit(&mut self, _: ()) {}
}

/// A selection by txid: block order with each fee and sigops, then the totals and counters.
type Picked = (Vec<(TxHash, i64, u32)>, BlockState, u64, u64);

fn picked<V: PackageView + ?Sized>(view: &V, s: Selection) -> Picked {
    let txs = s.entries.iter().map(|e| (view.mining_entry(e.id).txid, e.fee, e.sigops)).collect();
    (txs, s.state, s.packages_selected, s.descendants_updated)
}

/// One pool transaction as the reference sees it: explicit parents, the fee, the modified fee,
/// the virtual size and the sigops.
struct Node {
    txid: TxHash,
    parents: Vec<usize>,
    fee: i64,
    modified: i64,
    size: u64,
    sigops: u32,
}

fn ancestors(dag: &[Node], i: usize) -> HashSet<usize> {
    let mut seen = HashSet::new();
    let mut todo = dag[i].parents.clone();
    while let Some(p) = todo.pop() {
        if seen.insert(p) {
            todo.extend(&dag[p].parents);
        }
    }
    seen
}

/// MN1-2's from-scratch Core selector (`select::tests::reference`), keyed by txid: every
/// package's totals are recomputed from the DAG at each step and the best modified package is
/// found by a scan.
// Core: v24.0.2 src/node/miner.cpp:511-818, src/txmempool.h:158-197
fn reference(dag: &[Node], opts: &Options, reject: RejectSum) -> Picked {
    let n = dag.len();
    let anc: Vec<HashSet<usize>> = (0..n).map(|i| ancestors(dag, i)).collect();
    let txid = |i: usize| *dag[i].txid.as_bytes();
    let full = |i: usize| {
        anc[i].iter().chain([&i]).fold((0u64, 0i64, 0u32), |(s, f, o), &j| {
            (s + dag[j].size, f + dag[j].modified, o + dag[j].sigops)
        })
    };
    let totals = |i: usize, sub: Option<&HashSet<usize>>| {
        let (mut s, mut f, mut o) = full(i);
        for &j in sub.into_iter().flatten() {
            (s, f, o) = (s - dag[j].size, f - dag[j].modified, o - dag[j].sigops);
        }
        (s, f, o)
    };
    let cmp = |a: usize, (a_ps, a_pf, _): (u64, i64, u32), b: usize, (b_ps, b_pf, _)| {
        let key = |i: usize, ps: u64, pf: i64| {
            let t = &dag[i];
            if (t.modified as f64) * (ps as f64) > (pf as f64) * (t.size as f64) {
                (pf as f64, ps as f64)
            } else {
                (t.modified as f64, t.size as f64)
            }
        };
        let ((af, asz), (bf, bsz)) = (key(a, a_ps, a_pf), key(b, b_ps, b_pf));
        let (l, r) = (af * bsz, asz * bf);
        if l == r {
            core_hash_cmp(&txid(a), &txid(b))
        } else if l > r {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    };
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| cmp(a, full(a), b, full(b)));

    let mut state = BlockState::default();
    let (mut out, mut packages, mut updated) = (Vec::new(), 0, 0);
    let (mut in_block, mut failed) = (HashSet::new(), HashSet::new());
    let mut modified: HashMap<usize, HashSet<usize>> = HashMap::new();
    let (mut mi, mut consecutive) = (0, 0);
    let fits = |state: &BlockState, s: u64, o: u32| {
        state.size + s < opts.block_max_size && state.sigops + o < opts.block_max_sigops
    };
    loop {
        if mi >= n && modified.is_empty() {
            break;
        }
        if mi < n {
            let x = order[mi];
            if modified.contains_key(&x) || in_block.contains(&x) || failed.contains(&x) {
                mi += 1;
                continue;
            }
        }
        let best = modified
            .keys()
            .copied()
            .min_by(|&a, &b| cmp(a, totals(a, modified.get(&a)), b, totals(b, modified.get(&b))));
        let (iter, using_modified) = match best {
            Some(m)
                if mi >= n
                    || cmp(m, totals(m, modified.get(&m)), order[mi], full(order[mi]))
                        == Ordering::Less =>
            {
                (m, true)
            }
            _ => {
                mi += 1;
                (order[mi - 1], false)
            }
        };
        let (ps, pf, po) = totals(iter, modified.get(&iter));
        if pf < opts.block_min_fee_rate.get_fee(ps as u32) {
            break;
        }
        let mut fail = |modified: &mut HashMap<usize, HashSet<usize>>| {
            if using_modified {
                modified.remove(&iter);
                failed.insert(iter);
            }
        };
        if !fits(&state, ps, po) {
            fail(&mut modified);
            consecutive += 1;
            if consecutive > 1000 && state.size > opts.block_max_size - 1000 {
                break;
            }
            continue;
        }
        let mut package: Vec<usize> =
            anc[iter].iter().copied().filter(|a| !in_block.contains(a)).chain([iter]).collect();
        let ids = |package: &[usize]| package.iter().map(|&p| dag[p].txid).collect::<Vec<_>>();
        if reject.test_txs.rejects(ids(&package).iter()) {
            fail(&mut modified);
            continue;
        }
        package.sort_by(|&a, &b| {
            anc[a].len().cmp(&anc[b].len()).then_with(|| core_hash_cmp(&txid(a), &txid(b)))
        });
        if reject.validate.rejects(ids(&package).iter()) {
            fail(&mut modified);
            continue;
        }
        consecutive = 0;
        for &p in &package {
            let t = &dag[p];
            out.push((t.txid, t.fee, t.sigops));
            state.size += t.size;
            state.sigops += t.sigops;
            state.tx_count += 1;
            state.fees += t.fee;
            in_block.insert(p);
            modified.remove(&p);
        }
        packages += 1;
        for &p in &package {
            for d in (0..n).filter(|&d| (d == p || anc[d].contains(&p)) && !package.contains(&d)) {
                updated += 1;
                modified.entry(d).or_default().insert(p);
            }
        }
    }
    (out, state, packages, updated)
}

/// Two parent masks (ANDed), fee, prioritisation delta, padding and sigops; a quarter of the
/// fees and paddings are one value, so equal scores meet the R2 tie.
type ArbTx = (u16, u16, i64, i64, usize, u32);

fn arb_case() -> impl Strategy<Value = (Vec<ArbTx>, u64, u32, i64, [u64; 4])> {
    (
        prop::collection::vec(
            (
                any::<u16>(),
                any::<u16>(),
                prop_oneof![3 => 0i64..3000, 1 => Just(1000i64)],
                prop_oneof![3 => Just(0i64), 1 => -1500i64..1500],
                prop_oneof![3 => 0usize..700, 1 => Just(100usize)],
                0u32..30,
            ),
            1..14,
        ),
        1000u64..6000,
        100u32..250,
        0i64..4000,
        [0u64..5, 0u64..5, 0u64..5, 0u64..5],
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// MN1-2's `matches_the_from_scratch_reference` with the pool MP1a's: each transaction spends
    /// its parents' outputs, and the adapter reads the pool's own cached fields and graph.
    // Core: v24.0.2 src/node/miner.cpp:511-818
    #[test]
    fn the_real_pool_matches_the_from_scratch_reference(
        (txs, max_size, max_sigops, rate, [tm, tr, vm, vr]) in arb_case()
    ) {
        let mempool = node_pool();
        let mut dag: Vec<Node> = Vec::new();
        {
            let mut state = mempool.pool().write();
            let per_sigop = u64::from(state.options().bytes_per_sigop);
            for (i, &(m1, m2, fee, delta, pad, sigops)) in txs.iter().enumerate() {
                let parents: Vec<usize> = (0..i).filter(|j| (m1 & m2) >> j & 1 == 1).collect();
                let inputs = parents.iter().map(|&p| OutPoint::new(dag[p].txid, i as u32));
                let t = tx(inputs.chain([coin(i as u32)]).collect(), txs.len(), pad);
                add(&mut state, &t, fee, i64::from(sigops));
                state.prioritise_transaction(t.txid(), delta);
                dag.push(Node {
                    txid: t.txid(),
                    parents,
                    fee,
                    modified: fee + delta,
                    size: u64::from(t.size()).max(u64::from(sigops) * per_sigop),
                    sigops,
                });
            }
        }
        let opts = Options {
            block_max_size: max_size,
            block_max_sigops: max_sigops,
            block_min_fee_rate: FeeRate::new(rate),
            ..Options::default()
        };
        let checks = RejectSum {
            test_txs: SumRule { modulus: tm, residue: tr % tm.max(1) },
            validate: SumRule { modulus: vm, residue: vr % vm.max(1) },
        };
        let actual = read(&mempool, |view| {
            picked(view, add_package_txs(view, &mut checks.clone(), &opts, BlockState::default()))
        });
        prop_assert_eq!(actual, reference(&dag, &opts, checks));
    }
}

/// Equal scores in ascending raw-byte txid order through the adapter (R2), for a pair whose
/// display order is the other one, and `addPackageTxs` takes them in that order.
// Core: v24.0.2 src/txmempool.h:173-175, src/uint256.h:55-59
#[test]
fn equal_scores_are_in_core_hash_order() {
    let reversed = |t: &TxRef| {
        let mut b = *t.txid().as_bytes();
        b.reverse();
        b
    };
    let roots: Vec<TxRef> = (0..8).map(|n| tx(vec![coin(n)], 1, 0)).collect();
    let (a, b) = roots
        .iter()
        .flat_map(|a| roots.iter().map(move |b| (a, b)))
        .find(|(a, b)| {
            core_hash_cmp(a.txid().as_bytes(), b.txid().as_bytes()) == Ordering::Less
                && core_hash_cmp(&reversed(a), &reversed(b)) == Ordering::Greater
        })
        .expect("a pair whose byte and display orders differ");
    let mempool = node_pool();
    {
        let mut state = mempool.pool().write();
        add(&mut state, b, 1_000, 0);
        add(&mut state, a, 1_000, 0);
    }
    let (order, mined) = read(&mempool, |view| {
        let order = txids(view, &view.ancestor_score_iter().collect::<Vec<_>>());
        let opts = Options::default().clamp(true);
        let s = add_package_txs(view, &mut PassAll, &opts, BlockState::default());
        (order, s.entries.iter().map(|e| view.mining_entry(e.id).txid).collect::<Vec<_>>())
    });
    assert_eq!(order, [a.txid(), b.txid()]);
    assert_eq!(mined, [a.txid(), b.txid()]);
}

/// R4 without the entry, R5 with it, and R9, through the adapter's ids.
#[test]
fn the_adapter_maps_ancestors_and_descendants() {
    let mempool = node_pool();
    let p = tx(vec![coin(0)], 2, 0);
    let c = tx(vec![OutPoint::new(p.txid(), 0)], 2, 0);
    let g = tx(vec![OutPoint::new(c.txid(), 0)], 1, 0);
    {
        let mut state = mempool.pool().write();
        for t in [&p, &c, &g] {
            add(&mut state, t, 1_000, 0);
        }
    }
    read(&mempool, |view| {
        let id = |t: &TxRef| view.entry_id(&t.txid()).expect("in the pool");
        let set = |ids: Vec<EntryId>| txids(view, &ids).into_iter().collect::<HashSet<_>>();
        assert_eq!(set(view.ancestors_no_limits(id(&g))), HashSet::from([p.txid(), c.txid()]));
        assert_eq!(set(view.descendants(id(&p))), HashSet::from([p.txid(), c.txid(), g.txid()]));
        assert_eq!(set(view.descendants(id(&g))), HashSet::from([g.txid()]));
        assert!(view.entry_id(&coin(9).txid()).is_none());
        let e = view.mining_entry(id(&c));
        assert_eq!((e.count_with_ancestors, e.mod_fees_with_ancestors), (2, 2_000));
    });
}

/// Plan §2.5 R3: an id from an earlier guard panics, even one in range of this guard.
#[test]
fn an_entry_id_from_another_guard_panics() {
    let mempool = node_pool();
    let (a, b) = (tx(vec![coin(0)], 1, 0), tx(vec![coin(1)], 1, 0));
    {
        let mut state = mempool.pool().write();
        add(&mut state, &a, 1_000, 0);
        add(&mut state, &b, 1_000, 0);
    }
    let old = read(&mempool, |view| view.entry_id(&a.txid()).expect("in the pool"));
    read(&mempool, |view| {
        assert_eq!(view.mining_entry(view.entry_id(&a.txid()).unwrap()).txid, a.txid());
        let foreign = catch_unwind(AssertUnwindSafe(|| view.mining_entry(old).txid));
        assert!(foreign.is_err(), "an earlier guard's id names no entry here");
    });
}

/// §3.3 (a): the counter is read while another thread holds the pool's write lock, and sees the
/// bump that thread made under it.
// Core: v24.0.2 src/txmempool.cpp:417-420, :468
#[test]
fn the_counter_is_read_while_a_writer_holds_the_pool() {
    let mempool = node_pool();
    let adapter = Arc::new(MiningPool::new(Arc::clone(&mempool)));
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let pool = Arc::clone(mempool.pool());
    let writer = thread::spawn(move || {
        let mut state = pool.write();
        add(&mut state, &tx(vec![coin(0)], 1, 0), 1_000, 0);
        held_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    held_rx.recv().unwrap();
    let (read_tx, read_rx) = mpsc::channel();
    let reader = Arc::clone(&adapter);
    thread::spawn(move || read_tx.send(reader.transactions_updated()));
    let read = read_rx.recv_timeout(PATIENCE);
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    assert_eq!(read, Ok(1));
}

/// §3.3 (c): each increment point today moves the counter the adapter reads by Core's amount.
/// The InstantSend lock's bump is `IsMempool::add_transactions_updated`, which dashd-locks calls
/// (dashd-locks/src/is/process.rs:277); the new tip's is V's hold (dashd-chainstate
/// src/chain/abc.rs:1083). Shutdown's (init.cpp:328) has no call yet.
// Core: v24.0.2 src/txmempool.cpp:468, :742, :874, :1878-1900, src/validation.cpp:3222,
// src/instantsend/net_instantsend.cpp:413
#[test]
fn each_increment_point_moves_the_counter_by_cores_amount() {
    let mempool = node_pool();
    let adapter = MiningPool::new(Arc::clone(&mempool));
    let pool = Arc::clone(mempool.pool());
    let mut last = adapter.transactions_updated();
    let mut step = |what: &str, by: u32, f: &mut dyn FnMut()| {
        f();
        let now = adapter.transactions_updated();
        assert_eq!(now.wrapping_sub(last), by, "{what}");
        last = now;
    };
    let p = tx(vec![coin(0)], 1, 0);
    let c = tx(vec![OutPoint::new(p.txid(), 0)], 1, 0);
    step("add", 1, &mut || add(&mut pool.write(), &p, 1_000, 0));
    step("add", 1, &mut || add(&mut pool.write(), &c, 1_000, 0));
    step("prioritise an entry", 1, &mut || pool.prioritise(&c.txid(), 5));
    step("prioritise an unknown txid", 0, &mut || pool.prioritise(&coin(7).txid(), 5));
    step("remove two", 2, &mut || {
        pool.write().remove_recursive(&p, MemPoolRemovalReason::Conflict);
    });
    let [first, second] = [1u8, 2].map(asset_unlock);
    assert_eq!(first.txid(), second.txid());
    assert_ne!(first.instance_hash(), second.instance_hash());
    step("add", 1, &mut || add(&mut pool.write(), &first, 1_000, 0));
    step("replace an asset unlock's instance", 1, &mut || {
        pool.write().replace_asset_unlock_instance(first.txid(), Arc::clone(&second));
    });
    step("InstantSend lock", 1, &mut || IsMempool::add_transactions_updated(&*pool, 1));
    let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let mut sink = PoolSink::new(Arc::clone(&pool), Arc::new(params));
    step("new tip", 1, &mut || {
        let mut hold = sink.begin_hold().expect("the pool's hold");
        hold.add_transactions_updated(1, &mut Staged::default());
    });
}

/// A version 2 asset unlock (stable txid) whose signature bytes are `sig`.
fn asset_unlock(sig: u8) -> TxRef {
    let mut payload = vec![2];
    payload.extend(7u64.to_le_bytes());
    payload.extend(1_000u32.to_le_bytes());
    payload.extend(100u32.to_le_bytes());
    payload.extend([1; 32]);
    payload.extend([sig; 96]);
    let out = TxOut::from_duffs(1_000, vec![0x51].into());
    let unlock = Transaction::new(3, TxType::AssetUnlock, vec![], vec![out], 0, payload);
    let tx = Arc::new(Tx::new(unlock));
    assert!(tx.is_asset_unlock_with_stable_txid());
    tx
}

/// §3.3 (d): the lock-order checker records no pool lock for an R1 read. With the coins rank
/// held, the read is clean, while the pool's read guard panics the checker (debug builds only).
#[cfg(debug_assertions)]
#[test]
fn no_counter_read_takes_the_pool_lock() {
    let adapter = MiningPool::new(node_pool());
    let coins = LockOrderGuard::enter(LockRank::Coins);
    assert_eq!(adapter.transactions_updated(), 0);
    let read = catch_unwind(AssertUnwindSafe(|| adapter.with_read(&mut |_| {})));
    assert!(read.is_err(), "the pool's guard takes the mempool rank");
    drop(coins);
}
