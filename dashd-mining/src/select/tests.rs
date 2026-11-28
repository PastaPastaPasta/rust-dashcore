use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dash_consensus::primitives::{Transaction, Tx, TxRef, TxType, core_hash_cmp};
use proptest::prelude::*;

use super::*;
use crate::options::FeeRate;
use crate::testkit::{FakeEntry, FakePool};

pub(crate) fn tx(lock_time: u32) -> TxRef {
    Arc::new(Tx::new(Transaction::new(
        1,
        TxType::new(0),
        Vec::new(),
        Vec::new(),
        lock_time,
        Vec::new(),
    )))
}

/// The first test transaction whose raw txid bytes satisfy `pred`.
pub(crate) fn tx_where(pred: impl Fn(&[u8; 32]) -> bool) -> TxRef {
    (0..).map(tx).find(|t| pred(t.txid().as_bytes())).unwrap()
}

struct DagTx {
    tx: TxRef,
    parents: Vec<EntryId>,
    fee: i64,
    modified_fee: i64,
    size: u64,
    sigops: u32,
}

/// Transactions with explicit parents; [`Dag::pool`] fills in Core's cached ancestor fields.
#[derive(Default)]
pub(crate) struct Dag {
    txs: Vec<DagTx>,
}

impl Dag {
    pub fn add(&mut self, parents: &[EntryId], fee: i64, size: u64, sigops: u32) -> EntryId {
        let tx = tx((1 << 30) + self.txs.len() as u32);
        self.add_tx(tx, parents, fee, size, sigops)
    }

    pub fn add_tx(
        &mut self,
        tx: TxRef,
        parents: &[EntryId],
        fee: i64,
        size: u64,
        sigops: u32,
    ) -> EntryId {
        self.txs.push(DagTx {
            tx,
            parents: parents.to_vec(),
            fee,
            modified_fee: fee,
            size,
            sigops,
        });
        EntryId(self.txs.len() as u64 - 1)
    }

    pub fn prioritise(&mut self, id: EntryId, delta: i64) {
        self.txs[id.0 as usize].modified_fee += delta;
    }

    fn ancestors(&self, i: usize) -> HashSet<usize> {
        let mut out = HashSet::new();
        let mut todo: Vec<usize> = self.txs[i].parents.iter().map(|p| p.0 as usize).collect();
        while let Some(p) = todo.pop() {
            if out.insert(p) {
                todo.extend(self.txs[p].parents.iter().map(|q| q.0 as usize));
            }
        }
        out
    }

    pub fn pool(&self) -> FakePool {
        let mut pool = FakePool::new();
        for (i, t) in self.txs.iter().enumerate() {
            let mut entry = FakeEntry::new(t.tx.clone(), t.fee, t.size, t.sigops);
            entry.modified_fee = t.modified_fee;
            entry.parents = t.parents.clone();
            let ancestors = self.ancestors(i);
            for &a in &ancestors {
                let a = &self.txs[a];
                entry.size_with_ancestors += a.size;
                entry.mod_fees_with_ancestors += a.modified_fee;
                entry.sigops_with_ancestors += a.sigops;
            }
            entry.mod_fees_with_ancestors += t.modified_fee - t.fee;
            entry.count_with_ancestors = ancestors.len() as u64 + 1;
            pool.push(entry);
        }
        pool
    }
}

/// A scripted check. `updates` pulls the listed service updates in for any package holding the
/// trigger; `test_package_transactions` fails on any set holding all of one of `test_txs_reject`,
/// and `validate_package` on exactly the sorted packages in `reject`. Calls are recorded.
#[derive(Default)]
struct Script {
    updates: Vec<(EntryId, Vec<EntryId>)>,
    test_txs_reject: Vec<Vec<EntryId>>,
    reject: Vec<Vec<EntryId>>,
    test_txs_calls: RefCell<Vec<Vec<EntryId>>>,
    validated: Vec<Vec<EntryId>>,
}

impl Script {
    fn rejecting(packages: Vec<Vec<EntryId>>) -> Self {
        Self {
            reject: packages,
            ..Self::default()
        }
    }
}

impl PackageChecks for Script {
    type Accepted = ();

    fn service_updates<V: PackageView + ?Sized>(
        &self,
        _: &V,
        ancestors: &[EntryId],
        _: &HashSet<EntryId>,
    ) -> Vec<EntryId> {
        self.updates
            .iter()
            .filter(|(trigger, _)| ancestors.contains(trigger))
            .flat_map(|(_, updates)| updates.iter().copied())
            .collect()
    }

    fn test_package_transactions<V: PackageView + ?Sized>(
        &self,
        _: &V,
        package: &[EntryId],
    ) -> bool {
        let mut call = package.to_vec();
        call.sort();
        self.test_txs_calls.borrow_mut().push(call);
        !self.test_txs_reject.iter().any(|set| set.iter().all(|id| package.contains(id)))
    }

    fn validate_package<V: PackageView + ?Sized>(
        &mut self,
        _: &V,
        sorted: &[EntryId],
        _: EntryId,
    ) -> Option<()> {
        self.validated.push(sorted.to_vec());
        (!self.reject.iter().any(|p| p == sorted)).then_some(())
    }

    fn commit(&mut self, _: ()) {}
}

fn run(pool: &FakePool, checks: &mut Script, max_size: u64) -> (Vec<EntryId>, Selection) {
    let selection =
        add_package_txs(pool, checks, &options(max_size, 40_000, 1000), BlockState::default());
    (selection.entries.iter().map(|s| s.id).collect(), selection)
}

fn options(max_size: u64, max_sigops: u32, rate: i64) -> Options {
    Options {
        block_max_size: max_size,
        block_max_sigops: max_sigops,
        block_min_fee_rate: FeeRate::new(rate),
        ..Options::default()
    }
}

fn select(dag: &Dag, options: &Options) -> Vec<EntryId> {
    let pool = dag.pool();
    add_package_txs(&pool, &mut PassAll, options, BlockState::default())
        .entries
        .iter()
        .map(|s| s.id)
        .collect()
}

/// Core v24.0.2 src/test/miner_tests.cpp:113-232 TestPackageSelection, block positions less
/// the coinbase. One-in one-out transactions are 85 bytes, the two-output one 119.
#[test]
fn test_package_selection() {
    const S: u64 = 85;
    let rate = FeeRate::new(1000);
    let opts = Options::default().clamp(true);
    let mut dag = Dag::default();
    let parent = dag.add(&[], 1000, S, 1);
    let medium = dag.add(&[], 10_000, S, 1);
    let high = dag.add(&[parent], 50_000, S, 1);
    assert_eq!(select(&dag, &opts), vec![parent, high, medium]);

    let free = dag.add(&[high], 0, S, 1);
    let fee_to_use = rate.get_fee(2 * S as u32) - 1;
    let low = dag.add(&[free], fee_to_use, S, 1);
    assert_eq!(select(&dag, &opts), vec![parent, high, medium]);

    dag.txs[low.0 as usize].fee = fee_to_use + 2;
    dag.txs[low.0 as usize].modified_fee = fee_to_use + 2;
    let selected = select(&dag, &opts);
    assert_eq!(selected.len(), 5);
    assert_eq!(selected[3..], [free, low]);

    let free2 = dag.add(&[], 0, S + 34, 1);
    let low2 = dag.add(&[free2], rate.get_fee(S as u32), S, 1);
    let selected = select(&dag, &opts);
    assert!(!selected.contains(&free2) && !selected.contains(&low2));
    assert_eq!(selected.len(), 5);

    dag.add(&[free2], 10_000, S, 1);
    let selected = select(&dag, &opts);
    assert_eq!(selected.len(), 8);
    assert_eq!(selected[7], low2);
}

/// Core v24.0.2 src/node/miner.cpp:615-618: below the minimum, selection returns. `x` pays
/// exactly 1000 duff/kB; `c`'s package (with `a`, whose lone package the checks reject) pays more
/// but scores lower, so it is reached only if the refusal of `x` skipped instead.
#[test]
fn min_fee_refusal_returns() {
    let mut dag = Dag::default();
    let a = dag.add(&[], 500, 100, 1);
    let x = dag.add(&[], 200, 200, 1);
    let c = dag.add(&[a], 0, 100, 1);
    let pool = dag.pool();
    let run = |rate| {
        let mut checks = Script::rejecting(vec![vec![a]]);
        add_package_txs(
            &pool,
            &mut checks,
            &options(1_999_000, 40_000, rate),
            BlockState::default(),
        )
        .entries
        .iter()
        .map(|s| s.id)
        .collect::<Vec<_>>()
    };
    assert_eq!(run(1000), vec![x, a, c]);
    assert_eq!(run(1001), Vec::<EntryId>::new());
}

/// Core v24.0.2 src/node/miner.cpp:377-387: reaching either limit fails.
#[test]
fn test_package_limits_are_exclusive() {
    for (size, sigops, fits) in [(299, 4, true), (300, 4, false), (299, 5, false)] {
        let mut dag = Dag::default();
        dag.add(&[], 10_000, size, sigops);
        assert_eq!(
            select(&dag, &options(1300, 105, 1000)).len(),
            usize::from(fits),
            "{size} {sigops}"
        );
    }
}

/// Core v24.0.2 src/node/miner.cpp:620-636, 800: the counter stops selection only past 1000
/// resource failures with the block within 1000 bytes of full, and only a selected package resets
/// it; a package the checks reject (m:688-694, 791-797) neither counts nor resets.
#[test]
fn consecutive_failures() {
    let case = |filler: u64, before: usize, reject: Option<Skip>, after: usize| {
        let mut dag = Dag::default();
        dag.add(&[], 100 * filler as i64, filler, 1);
        for _ in 0..before {
            dag.add(&[], 60 * 2000, 2000, 1);
        }
        let r = reject.map(|_| dag.add(&[], 50 * 100, 100, 1));
        for _ in 0..after {
            dag.add(&[], 40 * 2000, 2000, 1);
        }
        let small = dag.add(&[], 10 * 100, 100, 1);
        let pool = dag.pool();
        let rejected: Vec<_> = r.map(|r| vec![r]).into_iter().collect();
        let mut checks = match reject {
            Some(Skip::TestTxs) => Script {
                test_txs_reject: rejected,
                ..Script::default()
            },
            _ => Script::rejecting(rejected),
        };
        run(&pool, &mut checks, 10_000).0.contains(&small)
    };
    assert!(!case(8001, 1001, None, 0), "full and past 1000 failures");
    assert!(case(8001, 1000, None, 0), "1000 failures is not past the limit");
    assert!(case(8000, 1001, None, 0), "1000 bytes left is not close to full");
    for skip in [Skip::Validate, Skip::TestTxs] {
        assert!(!case(8001, 1000, Some(skip), 1), "{skip:?} must not reset the counter");
        assert!(case(8001, 999, Some(skip), 1), "{skip:?} must not count");
    }
}

#[derive(Clone, Copy, Debug)]
enum Skip {
    Validate,
    TestTxs,
}

/// Core v24.0.2 src/node/miner.cpp:441-458: the block accounts `GetFee`, selection the modified fee.
#[test]
fn add_to_block_accounts_the_unmodified_fee() {
    let mut dag = Dag::default();
    let prioritised = dag.add(&[], 100, 200, 2);
    dag.prioritise(prioritised, 99_900);
    let plain = dag.add(&[], 10_000, 200, 3);
    let pool = dag.pool();
    let selection = add_package_txs(
        &pool,
        &mut PassAll,
        &options(1_999_000, 40_000, 1000),
        BlockState::default(),
    );
    assert_eq!(
        selection.entries,
        vec![
            Selected {
                id: prioritised,
                fee: 100,
                sigops: 2
            },
            Selected {
                id: plain,
                fee: 10_000,
                sigops: 3
            },
        ]
    );
    assert_eq!(
        selection.state,
        BlockState {
            size: 1400,
            sigops: 105,
            tx_count: 2,
            fees: 10_100
        }
    );
    assert_eq!((selection.packages_selected, selection.descendants_updated), (2, 0));
}

/// Core v24.0.2 src/node/miner.cpp:673-708: the updates go first; if package and updates do not
/// fit together, the updates are dropped and the package goes in alone.
#[test]
fn service_updates_go_first_or_not_at_all() {
    let mut dag = Dag::default();
    let parent = dag.add(&[], 10_000, 100, 1);
    let key_change = dag.add(&[parent], 10_000, 100, 1);
    let update = dag.add(&[], 200, 200, 1);
    let pool = dag.pool();
    for (max_size, expected) in [
        (1_999_000, vec![vec![update, parent, key_change]]),
        (1_400, vec![vec![parent, key_change]]),
    ] {
        let mut checks = Script {
            updates: vec![(key_change, vec![update])],
            ..Script::default()
        };
        let (ids, _) = run(&pool, &mut checks, max_size);
        assert_eq!(checks.validated, expected);
        assert_eq!(ids, expected[0]);
    }
}

/// Core v24.0.2 src/node/miner.cpp:676-681: an update that is already an ancestor adds nothing to
/// the extra size. Counting `parent` too (1000 + 200 + 300 = 1500) would drop the updates.
#[test]
fn an_update_already_an_ancestor_is_not_extra() {
    let mut dag = Dag::default();
    let parent = dag.add(&[], 10_000, 100, 1);
    let key_change = dag.add(&[parent], 12_000, 100, 1);
    let update = dag.add(&[], 0, 200, 1);
    let pool = dag.pool();
    let mut checks = Script {
        updates: vec![(key_change, vec![parent, update])],
        ..Script::default()
    };
    let (ids, selection) = run(&pool, &mut checks, 1500);
    assert!(ids.contains(&update), "{ids:?}");
    assert_eq!((ids.len(), selection.state.size), (3, 1400));
}

/// Core v24.0.2 src/node/miner.cpp:708, 816: the descendants of the pulled-in updates are updated
/// too, so `child` is judged without `update` (1400 + 100 fits under 1600; with it, 1700 does not).
#[test]
fn a_pulled_in_update_frees_its_child() {
    let mut dag = Dag::default();
    let parent = dag.add(&[], 10_000, 100, 1);
    let key_change = dag.add(&[parent], 12_000, 100, 1);
    let update = dag.add(&[], 200, 200, 1);
    let child = dag.add(&[update], 1000, 100, 1);
    let pool = dag.pool();
    let mut checks = Script {
        updates: vec![(key_change, vec![update])],
        ..Script::default()
    };
    let (ids, selection) = run(&pool, &mut checks, 1600);
    assert_eq!(ids, vec![update, parent, key_change, child]);
    assert_eq!((selection.packages_selected, selection.descendants_updated), (2, 1));
}

/// Core v24.0.2 src/node/miner.cpp:686-694: `TestPackageTransactions` sees the ancestors only,
/// not the updates pulled in, so an update it would refuse does not hold the key change back.
#[test]
fn test_package_transactions_sees_the_ancestors_only() {
    let mut dag = Dag::default();
    let parent = dag.add(&[], 10_000, 100, 1);
    let key_change = dag.add(&[parent], 12_000, 100, 1);
    let update = dag.add(&[], 200, 200, 1);
    let pool = dag.pool();
    let mut checks = Script {
        updates: vec![(key_change, vec![update])],
        test_txs_reject: vec![vec![update]],
        ..Script::default()
    };
    let (ids, _) = run(&pool, &mut checks, 1_999_000);
    assert_eq!(ids, vec![update, parent, key_change]);
    assert_eq!(checks.test_txs_calls.into_inner(), vec![vec![parent, key_change]]);
}

/// Core v24.0.2 src/node/miner.cpp:559-574, 688-693: a modified candidate `TestPackageTransactions`
/// refuses is erased and marked failed, so its own mapTx slot, still ahead, is skipped rather than
/// tested again. `child` (own rate 100) is modified once `rich` brings `parent` in, beats `mid`
/// (50) and is refused; its mapTx slot (package rate 10) comes after `mid`.
#[test]
fn a_refused_modified_candidate_is_not_retried_from_the_mempool() {
    let mut dag = Dag::default();
    let parent = dag.add(&[], 1000, 1000, 1);
    let rich = dag.add(&[parent], 100_000, 100, 1);
    let child = dag.add(&[parent], 10_000, 100, 1);
    let mid = dag.add(&[], 5000, 100, 1);
    let pool = dag.pool();
    let mut checks = Script {
        test_txs_reject: vec![vec![child]],
        ..Script::default()
    };
    let (ids, _) = run(&pool, &mut checks, 1_999_000);
    assert_eq!(ids, vec![parent, rich, mid]);
    assert_eq!(
        checks.test_txs_calls.into_inner(),
        vec![vec![parent, rich], vec![child], vec![mid]]
    );
}

/// Core v24.0.2 src/node/miner.cpp:479-483: an entry a failure erased from the modified set comes
/// back from the mempool's cached totals less only the newly added parent. `child`'s first
/// package (`p2`, `child`) is rejected; once `p2` is in, it re-enters with totals that still count
/// `p1`, already in the block: 1200 + 200 misses a 1350 limit, though alone it would fit, and
/// makes a 1450 one, where `child` follows `p2`.
#[test]
fn a_failed_entry_returns_with_cores_stale_totals() {
    let mut dag = Dag::default();
    let p1 = dag.add(&[], 50_000, 100, 1);
    let p2 = dag.add(&[], 100, 100, 1);
    let child = dag.add(&[p1, p2], 20_000, 100, 1);
    let pool = dag.pool();
    for (max_size, expected, size) in
        [(1350, vec![p1, p2], 1200), (1450, vec![p1, p2, child], 1300)]
    {
        let mut checks = Script::rejecting(vec![vec![p2, child]]);
        let (ids, selection) = run(&pool, &mut checks, max_size);
        assert_eq!(ids, expected, "{max_size}");
        assert_eq!(selection.state.size, size, "{max_size}");
    }
}

/// Three children of two prioritised parents whose own rates differ by less than a double can
/// resolve near 1e19: child 0 ties child 1 and child 1 ties child 2 (each settled by txid, which
/// ascends), but child 2 is strictly better than child 0, so the score order is a cycle. Core
/// erases by iterator and assembles a template; removal here must not search by score.
#[test]
fn a_cyclic_score_order_still_selects_each_entry_once() {
    let children = [
        (1_692_150_665_253_044, 5333),
        (1_917_115_004_586_329, 6042),
        (1_889_510_071_551_074, 5955),
    ];
    let rate = |(fee, size): (i64, u64), (other_fee, other_size): (i64, u64)| {
        (fee as f64 * other_size as f64).total_cmp(&(size as f64 * other_fee as f64))
    };
    assert_eq!(rate(children[0], children[1]), Ordering::Equal);
    assert_eq!(rate(children[1], children[2]), Ordering::Equal);
    assert_eq!(rate(children[2], children[0]), Ordering::Greater);

    let mut dag = Dag::default();
    let p1 = dag.add(&[], 1000, 200, 1);
    dag.prioritise(p1, 2_000_000_000_000_000 - 1000);
    let p2 = dag.add(&[], 1000, 200, 1);
    dag.prioritise(p2, 1_900_000_000_000_000 - 1000);
    let bands = [0x00..0x40u8, 0x40..0x80, 0x80..0xc0];
    let kids: Vec<_> = children
        .iter()
        .zip(bands)
        .map(|(&(fee, size), band)| {
            let tx = tx_where(|h| band.contains(&h[0]));
            dag.add_tx(tx, &[p1, p2], fee, size, 1)
        })
        .collect();
    let other = dag.add(&[], 10_000, 200, 1);
    let pool = dag.pool();
    let selection = add_package_txs(
        &pool,
        &mut PassAll,
        &options(1_999_000, 40_000, 1000),
        BlockState::default(),
    );
    let ids: Vec<_> = selection.entries.iter().map(|s| s.id).collect();
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "{ids:?}");
    assert_eq!(unique, vec![p1, p2, kids[0], kids[1], kids[2], other]);
    let at = |id| ids.iter().position(|x| *x == id).unwrap();
    assert!(kids.iter().all(|&k| at(p1) < at(k) && at(p2) < at(k)), "{ids:?}");
}

#[test]
fn print_priority_fee_rate_is_cfeerate_to_string() {
    assert_eq!(fee_rate_string(1000, 250), "0.00004000 DASH/kB");
    assert_eq!(fee_rate_string(150_000_000, 1000), "1.50000000 DASH/kB");
    assert_eq!(fee_rate_string(-5, 1000), "0.-0000005 DASH/kB");
    assert_eq!(fee_rate_string(5, 0), "0.00000000 DASH/kB");
}

/// Rejects every set whose ids sum to `residue` modulo `modulus` (none if `modulus < 2`): a rule
/// that can reject a candidate's package and accept a later, smaller one.
#[derive(Clone, Copy, Debug)]
struct SumRule {
    modulus: u64,
    residue: u64,
}

impl SumRule {
    fn rejects(self, ids: impl Iterator<Item = u64>) -> bool {
        self.modulus >= 2 && ids.sum::<u64>() % self.modulus == self.residue % self.modulus
    }
}

/// `test_package_transactions` and `validate_package` each by its own [`SumRule`].
#[derive(Clone, Copy, Debug)]
struct RejectSum {
    test_txs: SumRule,
    validate: SumRule,
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
        _: &V,
        package: &[EntryId],
    ) -> bool {
        !self.test_txs.rejects(package.iter().map(|id| id.0))
    }

    fn validate_package<V: PackageView + ?Sized>(
        &mut self,
        _: &V,
        sorted: &[EntryId],
        _: EntryId,
    ) -> Option<()> {
        (!self.validate.rejects(sorted.iter().map(|id| id.0))).then_some(())
    }

    fn commit(&mut self, _: ()) {}
}

/// A from-scratch Core selector: every package's totals are recomputed from the DAG at each step,
/// and the best modified package found by a scan. A modified entry's totals are its full ancestor
/// totals less the ancestors added since it last entered the modified set (Core rebuilds an
/// entry a failure erased from the mempool's cached values, miner.cpp:479-484).
fn reference(dag: &Dag, opts: &Options, start: BlockState, reject: RejectSum) -> Selection {
    let n = dag.txs.len();
    let anc: Vec<HashSet<usize>> = (0..n).map(|i| dag.ancestors(i)).collect();
    let txid = |i: usize| *dag.txs[i].tx.txid().as_bytes();
    let full = |i: usize| {
        anc[i].iter().chain([&i]).fold((0u64, 0i64, 0u32), |(s, f, o), &j| {
            let t = &dag.txs[j];
            (s + t.size, f + t.modified_fee, o + t.sigops)
        })
    };
    let totals = |i: usize, sub: Option<&HashSet<usize>>| {
        let (mut s, mut f, mut o) = full(i);
        for &j in sub.into_iter().flatten() {
            let t = &dag.txs[j];
            (s, f, o) = (s - t.size, f - t.modified_fee, o - t.sigops);
        }
        (s, f, o)
    };
    let cmp =
        |a: usize, (a_ps, a_pf, _): (u64, i64, u32), b: usize, (b_ps, b_pf, _): (u64, i64, u32)| {
            let key = |i: usize, ps: u64, pf: i64| {
                let t = &dag.txs[i];
                if (t.modified_fee as f64) * (ps as f64) > (pf as f64) * (t.size as f64) {
                    (pf as f64, ps as f64)
                } else {
                    (t.modified_fee as f64, t.size as f64)
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

    let mut out = Selection {
        entries: Vec::new(),
        state: start,
        packages_selected: 0,
        descendants_updated: 0,
    };
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
        if !fits(&out.state, ps, po) {
            if using_modified {
                modified.remove(&iter);
                failed.insert(iter);
            }
            consecutive += 1;
            if consecutive > 1000 && out.state.size > opts.block_max_size - 1000 {
                break;
            }
            continue;
        }
        let mut package: Vec<usize> =
            anc[iter].iter().copied().filter(|a| !in_block.contains(a)).chain([iter]).collect();
        let ids = |package: &[usize]| package.iter().map(|&p| p as u64).collect::<Vec<_>>();
        if reject.test_txs.rejects(ids(&package).into_iter()) {
            if using_modified {
                modified.remove(&iter);
                failed.insert(iter);
            }
            continue;
        }
        package.sort_by(|&a, &b| {
            anc[a].len().cmp(&anc[b].len()).then_with(|| core_hash_cmp(&txid(a), &txid(b)))
        });
        if reject.validate.rejects(ids(&package).into_iter()) {
            if using_modified {
                modified.remove(&iter);
                failed.insert(iter);
            }
            continue;
        }
        consecutive = 0;
        for &p in &package {
            let t = &dag.txs[p];
            out.entries.push(Selected {
                id: EntryId(p as u64),
                fee: t.fee,
                sigops: t.sigops,
            });
            out.state.size += t.size;
            out.state.sigops += t.sigops;
            out.state.tx_count += 1;
            out.state.fees += t.fee;
            in_block.insert(p);
            modified.remove(&p);
        }
        out.packages_selected += 1;
        for &p in &package {
            for d in (0..n).filter(|&d| (d == p || anc[d].contains(&p)) && !package.contains(&d)) {
                out.descendants_updated += 1;
                modified.entry(d).or_default().insert(p);
            }
        }
    }
    out
}

/// Two parent masks (ANDed), fee, prioritisation delta, size and sigops.
type ArbTx = (u16, u16, i64, i64, u64, u32);

fn arb_case() -> impl Strategy<Value = (Vec<ArbTx>, u64, u32, i64, [u64; 4])> {
    (
        prop::collection::vec(
            (
                any::<u16>(),
                any::<u16>(),
                0i64..3000,
                prop_oneof![3 => Just(0i64), 1 => -1500i64..1500],
                50u64..800,
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
    #[test]
    fn matches_the_from_scratch_reference(
        (txs, max_size, max_sigops, rate, [tm, tr, vm, vr]) in arb_case()
    ) {
        let mut dag = Dag::default();
        for (i, &(m1, m2, fee, delta, size, sigops)) in txs.iter().enumerate() {
            let parents: Vec<_> = (0..i).filter(|j| (m1 & m2) >> j & 1 == 1).map(|j| EntryId(j as u64)).collect();
            let id = dag.add(&parents, fee, size, sigops);
            dag.prioritise(id, delta);
        }
        let opts = options(max_size, max_sigops, rate);
        let pool = dag.pool();
        let mut checks = RejectSum {
            test_txs: SumRule { modulus: tm, residue: tr },
            validate: SumRule { modulus: vm, residue: vr },
        };
        let actual = add_package_txs(&pool, &mut checks, &opts, BlockState::default());
        prop_assert_eq!(actual, reference(&dag, &opts, BlockState::default(), checks));
    }
}
