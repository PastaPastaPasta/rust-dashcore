//! RP3a-P1's package tests: `ProcessNewPackage` over the stage-B kit's real pool and fake V. Each
//! test cites the Core v24.0.2 lines it pins (`v:` is validation.cpp, `p:` policy/packages.cpp,
//! `tm:` txmempool.cpp); the scenarios marked `rpc_packages` follow
//! test/functional/rpc_packages.py at v24.0.2 at the mempool API.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use dash_consensus::TxRef;
use dash_consensus::script::ScriptFlags;
use dash_consensus::validation::TxValidationResult;
use dash_primitives::{OutPoint, TxHash, TxType};

use super::StatelessRecord;
use super::finalize::FEE_ESTIMATION_CALLS;
use super::package::{check_package, process_new_package};
use super::single::{fee_rate, mempool_tx, success};
use super::stage_b_tests::{
    ACCEPT_TIME, Ev, Fx, SIG_TRUE, TEST_ACCEPT, out, p2sh_true, pay, tx_of, unlock,
};
use super::tests::outpoint;
use crate::options::{MemPoolLimits, MemPoolOptions};
use crate::policy::dust::get_fee;
use crate::types::{
    MemPoolRemovalReason, MempoolAcceptResult, MempoolAcceptResultType, PackageMempoolAcceptResult,
    PackageValidationResult, PackageValidationState,
};

const OP_TRUE: u8 = 0x51;
const OP_NOP: u8 = 0x61;
const OP_NOP1: u8 = 0xb0;

impl Fx {
    /// `ProcessNewPackage(txns, test_accept)` at [`ACCEPT_TIME`], the reply run as MP1a-4's
    /// dispatch runs it: the result and the completion's uncache list.
    fn package(
        &mut self,
        txns: &[TxRef],
        test_accept: bool,
    ) -> (PackageMempoolAcceptResult, Vec<OutPoint>) {
        let records: Vec<StatelessRecord> = {
            let state = self.pool.pool.read();
            txns.iter().map(|tx| StatelessRecord::new(tx, state.options())).collect()
        };
        let got = Arc::new(Mutex::new(None));
        let sink = got.clone();
        let completion = process_new_package(
            &self.pool,
            &mut self.chain,
            txns,
            &records,
            ACCEPT_TIME,
            test_accept,
            Box::new(move |result| *sink.lock().expect("unpoisoned") = Some(result)),
        )
        .expect("no fault");
        assert!(got.lock().expect("unpoisoned").is_none(), "the reply waits for the dispatch");
        (completion.reply)();
        let result = got.lock().expect("unpoisoned").take().expect("replied");
        (result, completion.uncache)
    }

    fn sequence(&self) -> u64 {
        self.pool.pool.read().sequence()
    }
}

fn state(result: PackageValidationResult, reason: &str, debug: &str) -> PackageValidationState {
    PackageValidationState {
        result,
        reason: reason.to_owned(),
        debug_message: debug.to_owned(),
    }
}

fn policy(reason: &str) -> PackageValidationState {
    state(PackageValidationResult::Policy, reason, "")
}

fn tx_failed() -> PackageValidationState {
    state(PackageValidationResult::Tx, "transaction failed", "")
}

/// A package-wide refusal: its state, no transaction results.
fn refused(reason: PackageValidationState) -> PackageMempoolAcceptResult {
    PackageMempoolAcceptResult {
        state: reason,
        tx_results: BTreeMap::new(),
    }
}

fn results(rows: &[(&TxRef, MempoolAcceptResult)]) -> BTreeMap<TxHash, MempoolAcceptResult> {
    rows.iter().map(|(tx, result)| (tx.txid(), result.clone())).collect()
}

/// `Success(vsize, fees, CFeeRate(modified fees, vsize), {txid})`: a transaction's own feerate.
fn own(tx: &TxRef, fees: i64, modified_fees: i64) -> MempoolAcceptResult {
    let vsize = i64::from(tx.size());
    success(vsize, fees, fee_rate(modified_fees, vsize), tx.txid())
}

/// `Success` with the package feerate over `package` (`v:1351-1356`).
fn at_package_rate(
    tx: &TxRef,
    fees: i64,
    package: &[&TxRef],
    package_fees: i64,
) -> MempoolAcceptResult {
    let size: i64 = package.iter().map(|tx| i64::from(tx.size())).sum();
    MempoolAcceptResult {
        txids_fee_calculations: Some(package.iter().map(|tx| tx.txid()).collect()),
        ..success(i64::from(tx.size()), fees, fee_rate(package_fees, size), tx.txid())
    }
}

/// A chain of `n` spends from `from` (worth `value`), each paying 1,000 in fees.
fn chain_of(from: OutPoint, value: i64, n: usize) -> Vec<TxRef> {
    let mut txs: Vec<TxRef> = Vec::new();
    let (mut prev, mut value) = (from, value);
    for _ in 0..n {
        value -= 1_000;
        let tx = pay(&[prev], &[value]);
        prev = out(&tx, 0);
        txs.push(tx);
    }
    txs
}

/// Each of `txns` test-accepted alone, then admitted: rpc_packages' `testres_single`.
fn one_by_one(fx: &mut Fx, txns: &[TxRef]) -> Vec<MempoolAcceptResult> {
    txns.iter()
        .map(|tx| {
            let (result, _) = fx.submit(tx, TEST_ACCEPT);
            fx.accept(tx);
            result
        })
        .collect()
}

fn in_order(result: &PackageMempoolAcceptResult, txns: &[TxRef]) -> Vec<MempoolAcceptResult> {
    txns.iter().map(|tx| result.tx_results[&tx.txid()].clone()).collect()
}

/// `CheckPackage` (`p:18-66`) through `AcceptMultipleTransactions` (`v:1487`) and `AcceptPackage`
/// (`v:1575`): more than 25 transactions, more than 101 kvB for more than one transaction (one
/// alone passes on to its own size checks), a child before its parent, and two transactions
/// spending one input (the same transaction twice included) are `PCKG_POLICY` refusals with no
/// transaction result and no coin read.
#[test]
fn check_package_reasons() {
    let mut fx = Fx::new();
    let many: Vec<TxRef> = (0..26).map(|n| pay(&[outpoint(n, 0)], &[90_000])).collect();
    let big = |byte| {
        let script = vec![OP_TRUE; 60_000];
        tx_of(
            2,
            TxType::Spend,
            &[(outpoint(byte, 0), &SIG_TRUE[..], u32::MAX)],
            &[(1_000, &script)],
            0,
            vec![],
        )
    };
    let chain = chain_of(fx.fund(1), 100_000, 2);
    let coin = fx.fund(2);
    let (tx1, tx2) = (pay(&[coin], &[90_000]), pay(&[coin], &[80_000]));
    let cases: [(Vec<TxRef>, &str); 5] = [
        (many.clone(), "package-too-many-transactions"),
        (vec![big(3), big(4)], "package-too-large"),
        (vec![chain[1].clone(), chain[0].clone()], "package-not-sorted"),
        (vec![tx1.clone(), tx1.clone()], "conflict-in-package"),
        (vec![tx1.clone(), tx2.clone()], "conflict-in-package"),
    ];
    for (txns, reason) in cases {
        assert_eq!(check_package(&txns), Err(reason));
        for test_accept in [true, false] {
            assert_eq!(
                fx.package(&txns, test_accept),
                (refused(policy(reason)), vec![]),
                "{reason}"
            );
        }
    }
    assert_eq!(check_package(&many[..25]), Ok(()));
    assert_eq!(check_package(&[big(3)]), Ok(()));
    assert!(fx.chain.coins.cached.is_empty());
    assert_eq!(fx.take_events(), vec![]);
}

/// `AcceptPackage`'s shape checks (`v:1577-1608`): anything but one child after some of its
/// parents is `package-not-child-with-parents`; a child input neither from a package parent nor
/// confirmed is `package-not-child-with-unconfirmed-parents`. Every child input CoinsTip had not
/// cached is listed for uncaching first, package-internal ones included (`v:1593-1597`); the
/// confirmed-input reads then stop at the first miss (`v:1601-1603`).
#[test]
fn child_with_parents_reasons() {
    let mut fx = Fx::new();
    let (a, b) = (pay(&[fx.fund(1)], &[90_000]), pay(&[fx.fund(2)], &[90_000]));
    let chain = chain_of(fx.fund(3), 100_000, 3);
    for txns in [vec![a.clone(), b.clone()], vec![a.clone()], chain.clone()] {
        assert_eq!(
            fx.package(&txns, false),
            (refused(policy("package-not-child-with-parents")), vec![])
        );
    }

    let confirmed = fx.fund(4);
    let missing = outpoint(5, 0);
    let child = pay(&[out(&a, 0), missing, confirmed], &[150_000]);
    let (result, uncache) = fx.package(&[a.clone(), child.clone()], false);
    assert_eq!(result, refused(policy("package-not-child-with-unconfirmed-parents")));
    assert_eq!(uncache, vec![out(&a, 0), missing, confirmed]);
    assert!(fx.chain.coins.cached.is_empty(), "no read past the missing input");

    let child = pay(&[out(&a, 0), confirmed, missing], &[150_000]);
    let (result, uncache) = fx.package(&[a.clone(), child], false);
    assert_eq!(result, refused(policy("package-not-child-with-unconfirmed-parents")));
    assert_eq!(uncache, vec![out(&a, 0), confirmed, missing]);
    assert!(fx.chain.coins.cached.contains(&confirmed));
    assert!(!fx.held(&a));
}

/// MN0 (1): a test accept (`PackageTestAccept`, `v:1775-1776`) of a child whose only input is its
/// parent's output, which only the package overlay holds (`PackageAddTransaction`, `v:1503`,
/// `tm:1977-1982`): each transaction is reported at its own feerate with its own txid
/// (`v:1546-1552`), the pool and CoinsTip never see the overlay, nothing is signalled, and every
/// input CoinsTip had not cached is uncached. The overlay coin is at MEMPOOL_HEIGHT, so a child's
/// one-block relative lock on it is `non-BIP68-final`, which ends the package as `PCKG_TX` with
/// only that result (`v:1498-1502`).
#[test]
fn test_accept_reads_package_parents_from_the_overlay() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let parent = pay(&[coin], &[90_000]);
    let child = pay(&[out(&parent, 0)], &[80_000]);
    let (alone, _) = fx.submit(&child, TEST_ACCEPT);
    assert_eq!(alone.validation_result, TxValidationResult::MissingInputs);
    fx.chain.coins.cached.clear();

    let (result, uncache) = fx.package(&[parent.clone(), child.clone()], true);
    let want = PackageMempoolAcceptResult {
        state: PackageValidationState::default(),
        tx_results: results(&[
            (&parent, own(&parent, 10_000, 10_000)),
            (&child, own(&child, 10_000, 10_000)),
        ]),
    };
    assert_eq!(result, want);
    assert_eq!(uncache, vec![coin, out(&parent, 0)]);
    assert!(!fx.held(&parent) && !fx.held(&child));
    assert_eq!(fx.take_events(), vec![]);
    assert!(!fx.chain.coins.cached.contains(&out(&parent, 0)));
    assert!(!fx.chain.coins.coins.contains_key(&out(&parent, 0)));

    let locked = tx_of(
        2,
        TxType::Spend,
        &[(out(&parent, 0), &SIG_TRUE[..], 1)],
        &[(80_000, &p2sh_true())],
        0,
        vec![],
    );
    let (result, _) = fx.package(&[parent.clone(), locked.clone()], true);
    assert_eq!(result.state, tx_failed());
    assert_eq!(result.tx_results.len(), 1);
    let failed = &result.tx_results[&locked.txid()];
    assert_eq!(
        (failed.validation_result, failed.reason.as_str()),
        (TxValidationResult::PrematureSpend, "non-BIP68-final")
    );
}

/// MN0 (2): a submission refused as `PCKG_TX` after its parent was admitted on its own
/// (`v:1659-1679`): the result keeps the parent's success and the child's failure, the parent
/// stays (a partial submission, signalled), and `coins_to_uncache` is Core's, in Core's order with
/// its repeats: the child's uncached inputs (`v:1593-1597`), the parent's input (its `PreChecks`),
/// then the child's package input again (its own `PreChecks`); the confirmed input was cached by
/// `AcceptPackage`'s read and is not listed twice. The dispatch uncaches them all (`v:1781-1786`).
#[test]
fn refused_submission_uncaches_cores_list() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let nonstandard = fx.fund_with(2, 100_000, 100, &[OP_NOP1, OP_TRUE]);
    let parent = pay(&[coin], &[90_000]);
    let child = tx_of(
        2,
        TxType::Spend,
        &[(out(&parent, 0), &SIG_TRUE[..], u32::MAX), (nonstandard, &[][..], u32::MAX)],
        &[(150_000, &p2sh_true())],
        0,
        vec![],
    );
    let sequence = fx.sequence();
    let (result, uncache) = fx.package(&[parent.clone(), child.clone()], false);
    assert_eq!(result.state, tx_failed());
    let failed = &result.tx_results[&child.txid()];
    assert_eq!(
        (failed.validation_result, failed.reason.as_str()),
        (TxValidationResult::InputsNotStandard, "bad-txns-nonstandard-inputs")
    );
    assert_eq!(result.tx_results[&parent.txid()], own(&parent, 10_000, 10_000));
    assert_eq!(result.tx_results.len(), 2);
    assert_eq!(uncache, vec![out(&parent, 0), nonstandard, coin, out(&parent, 0)]);
    assert!(fx.held(&parent) && !fx.held(&child));
    assert_eq!(
        fx.take_events(),
        vec![Ev::Added(parent.txid(), parent.instance_hash(), ACCEPT_TIME, sequence)]
    );
}

/// The pool usage after admitting `txs` in order (each funded by [`Fx::fund`] from `funds`), less
/// `removed` once removed.
fn usage_of(txs: &[&TxRef], removed: Option<&TxRef>, funds: &[u8]) -> usize {
    let mut fx = Fx::new();
    funds.iter().for_each(|byte| {
        fx.fund(*byte);
    });
    txs.iter().for_each(|tx| fx.accept(tx));
    if let Some(tx) = removed {
        fx.pool.pool.write().remove_recursive(tx, MemPoolRemovalReason::Manual);
    }
    fx.pool.pool.read().dynamic_memory_usage()
}

/// MN0 (3): a submission through `SubmitPackage` that ends in `LimitMempoolSize`
/// (`v:1291-1370`). The parent alone is trimmed at once (`mempool full`, `v:1283-1286`), so it
/// and its child are validated together at the package feerate; their `Finalize` does not trim
/// (`v:1283`) and the closing trim evicts the cheaper resident entry instead. Every removal is
/// signalled before the first admission, each with its sequence, then each package transaction
/// in package order with the next ones (`v:1357-1360`); both report the package feerate over both
/// txids; the evicted entry's coin is uncached (`v:407-409`) and the completion uncaches nothing.
#[test]
fn package_trim_signals_removals_then_admissions_in_core_order() {
    let other = pay(&[outpoint(3, 0)], &[1_000; 40]);
    let parent = pay(&[outpoint(1, 0)], &[99_850]);
    let child = pay(&[out(&parent, 0)], &[9_850]);
    let with_other = usage_of(&[&other, &parent], None, &[1, 3]);
    let package_only = usage_of(&[&other, &parent, &child], Some(&other), &[1, 3]);
    assert!(package_only < with_other);
    let mut fx = Fx::with_options(MemPoolOptions {
        max_size_bytes: with_other as i64 - 1,
        ..MemPoolOptions::default()
    });
    fx.fund(1);
    let other_coin = fx.fund(3);
    fx.accept(&other);
    fx.take_events();
    let sequence = fx.sequence();

    let (result, uncache) = fx.package(&[parent.clone(), child.clone()], false);
    let package = [&parent, &child];
    let want = PackageMempoolAcceptResult {
        state: PackageValidationState::default(),
        tx_results: results(&[
            (&parent, at_package_rate(&parent, 150, &package, 90_150)),
            (&child, at_package_rate(&child, 90_000, &package, 90_150)),
        ]),
    };
    assert_eq!(result, want);
    assert_eq!(uncache, vec![]);
    assert_eq!(
        fx.take_events(),
        vec![
            Ev::Removed(parent.txid(), MemPoolRemovalReason::SizeLimit, sequence),
            Ev::Removed(other.txid(), MemPoolRemovalReason::SizeLimit, sequence + 1),
            Ev::Added(parent.txid(), parent.instance_hash(), ACCEPT_TIME, sequence + 2),
            Ev::Added(child.txid(), child.instance_hash(), ACCEPT_TIME, sequence + 3),
        ]
    );
    assert!(fx.held(&parent) && fx.held(&child) && !fx.held(&other));
    assert!(!fx.chain.coins.cached.contains(&other_coin));
    let other_rate = fee_rate(60_000, i64::from(other.size()));
    assert_eq!(fx.pool.pool.read().get_min_fee(), other_rate + 1_000);
    fx.pool.pool.read().check();
}

/// m1: a submission whose own transactions the closing `LimitMempoolSize` evicts (`v:1342-1370`).
/// Parent `a` is admitted on its own; parent `b` is trimmed at once by its own `Finalize`
/// (`mempool full`) and the child misses `b`'s output, so `b` and the child go to
/// `AcceptMultipleTransactions` together. `SubmitPackage` adds both, and the closing trim takes `b`
/// (the lowest descendant score) with the child. Both are `mempool full` and the package
/// `PCKG_TX`, with no `added` for either, while `a` stays admitted (a partial submission). The
/// result is invalid, so the completion uncaches all of Core's list (`v:1781-1786`): the child's
/// uncached inputs (`v:1593-1597`), each `PreChecks`' in order (`a`, `b`, the child alone, then
/// `b` and the child in the package), and the trim uncached `b`'s coin (`v:407-409`).
#[test]
fn package_evicted_by_its_closing_trim_is_mempool_full() {
    let a = pay(&[outpoint(1, 0)], &[50_000]);
    let b = pay(&[outpoint(2, 0)], &[99_850]);
    let child = pay(&[out(&a, 0), out(&b, 0)], &[144_850]);
    let with_b = usage_of(&[&a, &b], None, &[1, 2]);
    assert!(usage_of(&[&a, &b, &child], Some(&b), &[1, 2]) < with_b);
    let mut fx = Fx::with_options(MemPoolOptions {
        max_size_bytes: with_b as i64 - 1,
        ..MemPoolOptions::default()
    });
    let (coin_a, coin_b) = (fx.fund(1), fx.fund(2));
    let sequence = fx.sequence();

    let (result, uncache) = fx.package(&[a.clone(), b.clone(), child.clone()], false);
    let full = |tx: &TxRef| {
        let failed = &result.tx_results[&tx.txid()];
        (failed.result_type, failed.validation_result, failed.reason.clone())
    };
    let mempool_full = (
        MempoolAcceptResultType::Invalid,
        TxValidationResult::MempoolPolicy,
        "mempool full".to_owned(),
    );
    assert_eq!(result.state, tx_failed());
    assert_eq!(result.tx_results[&a.txid()], own(&a, 50_000, 50_000));
    assert_eq!((full(&b), full(&child)), (mempool_full.clone(), mempool_full));
    assert_eq!(result.tx_results.len(), 3);
    let (a0, b0) = (out(&a, 0), out(&b, 0));
    assert_eq!(uncache, vec![a0, b0, coin_a, coin_b, a0, b0, coin_b, a0, b0]);

    let mut trimmed = [&b, &child];
    trimmed.sort_by_key(|tx| tx.txid());
    assert_eq!(
        fx.take_events(),
        vec![
            Ev::Added(a.txid(), a.instance_hash(), ACCEPT_TIME, sequence),
            Ev::Removed(b.txid(), MemPoolRemovalReason::SizeLimit, sequence + 1),
            Ev::Removed(trimmed[0].txid(), MemPoolRemovalReason::SizeLimit, sequence + 2),
            Ev::Removed(trimmed[1].txid(), MemPoolRemovalReason::SizeLimit, sequence + 3),
        ]
    );
    assert!(fx.held(&a) && !fx.held(&b) && !fx.held(&child));
    assert!(fx.chain.coins.cached.contains(&coin_a) && !fx.chain.coins.cached.contains(&coin_b));
    let package_rate = fee_rate(5_150, i64::from(b.size() + child.size()));
    assert_eq!(fx.pool.pool.read().get_min_fee(), package_rate + 1_000);
    fx.pool.pool.read().check();
}

/// m1: `Finalize` leaves a package submission out of fee estimation (`v:1250-1256`): with the tip
/// current, a single accept and a parent admitted on its own in `AcceptPackage` reach
/// `processTransaction` valid, and a parent and child submitted at the package feerate reach it
/// invalid, though the parent spends no pool entry. A test accept reaches it not at all.
#[test]
fn package_submission_is_left_out_of_fee_estimation() {
    let (mut fx, _) = raised_minimum(MemPoolOptions::default());
    fx.chain.current_for_fee_estimation = true;
    let calls = || FEE_ESTIMATION_CALLS.take();
    calls();
    let single = pay(&[fx.fund(2)], &[90_000]);
    fx.accept(&single);
    assert_eq!(calls(), vec![(single.txid(), true)]);

    let parent = pay(&[fx.fund(1)], &[99_500]);
    let rich = pay(&[out(&parent, 0)], &[89_500]);
    let package = [parent.clone(), rich.clone()];
    assert_eq!(fx.package(&package, true).0.state, tx_failed());
    assert_eq!(calls(), vec![]);
    assert_eq!(fx.package(&package, false).0.state, PackageValidationState::default());
    assert_eq!(calls(), vec![(parent.txid(), false), (rich.txid(), false)]);

    let alone = pay(&[fx.fund(3)], &[90_000]);
    let kid = pay(&[out(&alone, 0)], &[80_000]);
    assert_eq!(
        fx.package(&[alone.clone(), kid.clone()], false).0.state,
        PackageValidationState::default()
    );
    assert_eq!(calls(), vec![(alone.txid(), true), (kid.txid(), false)]);
}

/// A pool of `options` whose rolling minimum is a trimmed 1,000-duff 85-byte spend's feerate plus
/// the incremental relay feerate (`tm:2089-2125`), with no entry left.
fn raised_minimum(options: MemPoolOptions) -> (Fx, i64) {
    let mut fx = Fx::with_options(options);
    let seed = pay(&[fx.fund(9)], &[99_000]);
    fx.accept(&seed);
    {
        let mut state = fx.pool.pool.write();
        state.trim_to_size(0);
        state.take_pending_events();
    }
    fx.take_events();
    let minimum = fee_rate(1_000, 85) + 1_000;
    assert_eq!(fx.pool.pool.read().get_min_fee(), minimum);
    (fx, minimum)
}

/// CPFP (`v:1062-1076`, `:1506-1518`): a parent above the relay floor but below the pool's
/// minimum is refused alone (`mempool min fee not met`). A test accept checks each transaction at
/// its own feerate, so the parent ends it; a submission skips the per-transaction minimum under
/// `package_feerates` and checks the package's: a child paying too little is
/// `package-fee-too-low` with no transaction result, one paying enough carries the parent in,
/// both reporting the package feerate over both txids.
#[test]
fn cpfp_by_package_feerate() {
    let (mut fx, minimum) = raised_minimum(MemPoolOptions::default());
    let parent = pay(&[fx.fund(1)], &[99_500]);
    assert!(get_fee(1_000, 85) < 500 && 500 < get_fee(minimum, 85));
    let poor = pay(&[out(&parent, 0)], &[99_000]);
    let rich = pay(&[out(&parent, 0)], &[89_500]);

    let (result, _) = fx.package(&[parent.clone(), rich.clone()], true);
    assert_eq!(result.state, tx_failed());
    let failed = &result.tx_results[&parent.txid()];
    assert_eq!(failed.reason, "mempool min fee not met");
    assert_eq!(result.tx_results.len(), 1);

    assert!(1_000 < get_fee(minimum, 170));
    let (result, _) = fx.package(&[parent.clone(), poor.clone()], false);
    assert_eq!(result, refused(policy("package-fee-too-low")));
    assert!(!fx.held(&parent));
    assert_eq!(fx.take_events(), vec![]);

    let sequence = fx.sequence();
    let (result, uncache) = fx.package(&[parent.clone(), rich.clone()], false);
    let package = [&parent, &rich];
    let want = PackageMempoolAcceptResult {
        state: PackageValidationState::default(),
        tx_results: results(&[
            (&parent, at_package_rate(&parent, 500, &package, 10_500)),
            (&rich, at_package_rate(&rich, 10_000, &package, 10_500)),
        ]),
    };
    assert_eq!(result, want);
    assert_eq!(uncache, vec![]);
    assert_eq!(
        fx.take_events(),
        vec![
            Ev::Added(parent.txid(), parent.instance_hash(), ACCEPT_TIME, sequence),
            Ev::Added(rich.txid(), rich.instance_hash(), ACCEPT_TIME, sequence + 1),
        ]
    );
    fx.pool.pool.read().check();
}

/// `PackageMempoolChecks` (`v:1141-1157`, `CheckPackageLimits` `tm:205-232`): limits each
/// transaction meets alone, the package together exceeds; the debug message is CheckPackageLimits'
/// error, "possibly" prefixed past the parent count.
#[test]
fn package_mempool_limits() {
    let limited = |limits: MemPoolLimits| {
        let mut fx = Fx::with_options(MemPoolOptions {
            limits,
            ..MemPoolOptions::default()
        });
        let grandparent = pay(&[fx.fund(1)], &[90_000]);
        fx.accept(&grandparent);
        let parent = pay(&[out(&grandparent, 0)], &[80_000]);
        let child = pay(&[out(&parent, 0)], &[70_000]);
        let (alone, _) = fx.submit(&parent, TEST_ACCEPT);
        assert_eq!(alone.result_type, MempoolAcceptResultType::Valid, "{alone:?}");
        (fx.package(&[parent, child], true).0, grandparent)
    };
    let (result, _) = limited(MemPoolLimits {
        ancestor_count: 2,
        ..MemPoolLimits::default()
    });
    let debug = "too many unconfirmed parents [limit: 2]";
    assert_eq!(
        result,
        refused(state(PackageValidationResult::Policy, "package-mempool-limits", debug))
    );
    let (result, grandparent) = limited(MemPoolLimits {
        descendant_count: 2,
        ..MemPoolLimits::default()
    });
    let debug = format!("possibly too many descendants for tx {} [limit: 2]", grandparent.txid());
    assert_eq!(
        result,
        refused(state(PackageValidationResult::Policy, "package-mempool-limits", &debug))
    );
}

/// `CheckPackageWithAssetUnlocks` (`v:1468-1480`): two unlocks of one index; and
/// `CheckPackageAssetUnlockEvictions` (`v:1159-1184`): a package transaction that is, or spends,
/// a descendant of another claimant of a package unlock's index, in a test accept and (before any
/// parent is admitted alone, `v:1612-1614`) a submission. An unrelated package with the fresher
/// unlock is not refused for it.
#[test]
fn asset_unlock_conflicting_package() {
    let mut fx = Fx::new();
    let reason = "assetunlock-conflicting-package";
    let duplicate = [unlock(1, 5, 150, 1), unlock(1, 5, 151, 2)];
    for test_accept in [true, false] {
        assert_eq!(fx.package(&duplicate, test_accept), (refused(policy(reason)), vec![]));
    }

    let held = unlock(1, 7, 150, 1);
    fx.accept(&held);
    let child = pay(&[out(&held, 0)], &[9_000]);
    fx.accept(&child);
    fx.take_events();
    let fresher = unlock(1, 7, 151, 2);
    let spender = pay(&[out(&child, 0)], &[8_000]);
    assert_eq!(fx.package(&[fresher.clone(), spender], true).0, refused(policy(reason)));
    let both = pay(&[out(&child, 0), out(&fresher, 0)], &[17_000]);
    let (result, uncache) = fx.package(&[child.clone(), fresher.clone(), both], false);
    assert_eq!(result, refused(policy(reason)));
    assert_eq!(uncache, vec![out(&child, 0), out(&fresher, 0)]);
    assert!(fx.held(&held) && fx.held(&child) && !fx.held(&fresher));
    assert_eq!(fx.take_events(), vec![]);

    let unrelated = pay(&[fx.fund(1)], &[90_000]);
    let (result, _) = fx.package(&[fresher.clone(), unrelated], true);
    assert_eq!(result.state, PackageValidationState::default());
    assert_eq!(result.tx_results.len(), 2);
}

/// rpc_packages `test_independent` (`v:1498-1502`, `:1532-1538`): independent transactions get
/// the results they get alone, in any order; a missing input ends the test accept with only that
/// result; a script failure comes after every `PreChecks`, so the others are reported in full.
#[test]
fn rpc_packages_independent() {
    let mut fx = Fx::new();
    let txns: Vec<TxRef> = (1..=3).map(|byte| pay(&[fx.fund(byte)], &[90_000])).collect();
    let alone: Vec<_> = txns.iter().map(|tx| fx.submit(tx, TEST_ACCEPT).0).collect();
    let (result, _) = fx.package(&txns, true);
    assert_eq!(result.state, PackageValidationState::default());
    assert_eq!(in_order(&result, &txns), alone);
    let reversed: Vec<TxRef> = txns.iter().rev().cloned().collect();
    assert_eq!(fx.package(&reversed, true).0, result);

    let garbage = pay(&[outpoint(0x77, 5)], &[1_000]);
    let (result, _) = fx.package(&[txns.clone(), vec![garbage.clone()]].concat(), true);
    assert_eq!(result.state, tx_failed());
    assert_eq!(result.tx_results.len(), 1);
    assert_eq!(
        result.tx_results[&garbage.txid()].validation_result,
        TxValidationResult::MissingInputs
    );

    let coin = fx.fund(4);
    let bad_sig = tx_of(
        2,
        TxType::Spend,
        &[(coin, &[0x01, 0x52][..], u32::MAX)],
        &[(90_000, &p2sh_true())],
        0,
        vec![],
    );
    let (bad_alone, _) = fx.submit(&bad_sig, TEST_ACCEPT);
    assert!(bad_alone.reason.starts_with("mandatory-script-verify-flag-failed"), "{bad_alone:?}");
    let all = [txns.clone(), vec![bad_sig.clone()]].concat();
    let (result, _) = fx.package(&all, true);
    assert_eq!(result.state, tx_failed());
    assert_eq!(in_order(&result, &all), [alone, vec![bad_alone]].concat());
    assert!(txns.iter().all(|tx| !fx.held(tx)));
}

/// rpc_packages `test_chain`: a chain of 25 reversed is `package-not-sorted`; in order its test
/// accept equals each transaction's test accept before its own admission.
#[test]
fn rpc_packages_chain() {
    let mut fx = Fx::new();
    let chain = chain_of(fx.fund(1), 100_000, 25);
    let reversed: Vec<TxRef> = chain.iter().rev().cloned().collect();
    assert_eq!(fx.package(&reversed, true).0, refused(policy("package-not-sorted")));
    let (result, _) = fx.package(&chain, true);
    assert_eq!(result.state, PackageValidationState::default());
    assert_eq!(in_order(&result, &chain), one_by_one(&mut fx, &chain));
}

/// rpc_packages `test_multiple_children`: a parent and its two children, in either order of the
/// children, each child refused alone.
#[test]
fn rpc_packages_multiple_children() {
    let mut fx = Fx::new();
    let parent = pay(&[fx.fund(1)], &[45_000, 45_000]);
    let a = pay(&[out(&parent, 0)], &[40_000]);
    let b = pay(&[out(&parent, 1)], &[40_000]);
    for child in [&a, &b] {
        assert_ne!(fx.submit(child, TEST_ACCEPT).0.result_type, MempoolAcceptResultType::Valid);
    }
    let ab = [parent.clone(), a.clone(), b.clone()];
    let (result, _) = fx.package(&ab, true);
    let (ba, _) = fx.package(&[parent.clone(), b.clone(), a.clone()], true);
    assert_eq!(result, ba);
    assert!(result.tx_results.values().all(|r| r.result_type == MempoolAcceptResultType::Valid));
    assert_eq!(in_order(&result, &ab), one_by_one(&mut fx, &ab));
}

/// rpc_packages `test_multiple_parents`: 2, 10 and 24 parents of one child, in any parent order.
#[test]
fn rpc_packages_multiple_parents() {
    for n in [2, 10, 24] {
        let mut fx = Fx::new();
        let parents: Vec<TxRef> = (1..=n).map(|byte| pay(&[fx.fund(byte)], &[90_000])).collect();
        let inputs: Vec<OutPoint> = parents.iter().map(|tx| out(tx, 0)).collect();
        let child = pay(&inputs, &[90_000 * i64::from(n) - 10_000]);
        let package = [parents.clone(), vec![child.clone()]].concat();
        let (result, _) = fx.package(&package, true);
        let shuffled = [parents.iter().rev().cloned().collect(), vec![child.clone()]].concat();
        assert_eq!(fx.package(&shuffled, true).0, result);
        assert!(
            result.tx_results.values().all(|r| r.result_type == MempoolAcceptResultType::Valid)
        );
        assert_eq!(in_order(&result, &package), one_by_one(&mut fx, &package));
    }
}

/// rpc_packages `test_conflicting`: the same transaction twice, or two spends of one coin, each
/// valid alone.
#[test]
fn rpc_packages_conflicting() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let (tx1, tx2) = (pay(&[coin], &[90_000]), pay(&[coin], &[80_000]));
    for tx in [&tx1, &tx2] {
        assert_eq!(fx.submit(tx, TEST_ACCEPT).0.result_type, MempoolAcceptResultType::Valid);
    }
    for package in [[tx1.clone(), tx1.clone()], [tx1.clone(), tx2.clone()]] {
        assert_eq!(fx.package(&package, true).0, refused(policy("conflict-in-package")));
    }
}

/// rpc_packages `test_submit_child_with_parents`: 1, 2 and 24 parents paying their own way, with
/// and without some already in the pool. Each new transaction is admitted on its own
/// (`v:1652-1658`) at its own feerate, signalled in package order; a held parent is its entry
/// (`MempoolTx`, `v:1648-1650`). The test accept and the pool's entries agree on size and fees.
#[test]
fn rpc_packages_submit_child_with_parents() {
    for (n, partial) in [(1, false), (1, true), (2, false), (2, true), (24, false), (24, true)] {
        let mut fx = Fx::new();
        let parents: Vec<TxRef> = (1..=n).map(|byte| pay(&[fx.fund(byte)], &[90_000])).collect();
        let presubmitted: Vec<&TxRef> = parents.iter().step_by(2).filter(|_| partial).collect();
        presubmitted.iter().for_each(|tx| fx.accept(tx));
        fx.take_events();
        let inputs: Vec<OutPoint> = parents.iter().map(|tx| out(tx, 0)).collect();
        let child = pay(&inputs, &[90_000 * i64::from(n) - 10_000]);
        let package = [parents.clone(), vec![child.clone()]].concat();
        let (tested, _) = fx.package(&package, true);
        let sequence = fx.sequence();

        let (result, uncache) = fx.package(&package, false);
        assert_eq!(result.state, PackageValidationState::default());
        assert_eq!(uncache, vec![]);
        let mut events = Vec::new();
        for tx in &package {
            let got = &result.tx_results[&tx.txid()];
            if presubmitted.iter().any(|held| held.txid() == tx.txid()) {
                assert_eq!(*got, mempool_tx(i64::from(tx.size()), 10_000));
            } else {
                assert_eq!(*got, own(tx, 10_000, 10_000));
                let s = sequence + events.len() as u64;
                events.push(Ev::Added(tx.txid(), tx.instance_hash(), ACCEPT_TIME, s));
            }
            let state = fx.pool.pool.read();
            let entry = state.get_iter(&tx.txid()).expect("held");
            assert_eq!(
                (got.vsize, got.base_fee),
                (Some(entry.vsize() as i64), Some(entry.metadata().fee))
            );
            if let Some(t) = tested
                .tx_results
                .get(&tx.txid())
                .filter(|t| t.result_type == MempoolAcceptResultType::Valid)
            {
                assert_eq!((got.vsize, got.base_fee), (t.vsize, t.base_fee));
            }
        }
        assert_eq!(result.tx_results.len(), package.len());
        assert_eq!(fx.take_events(), events, "{n} parents, partial {partial}");
    }
}

/// rpc_packages `test_submit_package_with_modified_fees`: a zero-fee parent prioritised to pay
/// its way and one at the relay feerate are each admitted alone, so each effective feerate covers
/// only its own transaction (`ApplyDelta`, `v:1039-1040`).
#[test]
fn rpc_packages_modified_fees() {
    let mut fx = Fx::new();
    let rich = pay(&[fx.fund(1)], &[100_000]);
    let relay_fee = get_fee(1_000, 85);
    let poor = pay(&[fx.fund(2)], &[100_000 - relay_fee]);
    fx.pool.pool.write().prioritise_transaction(rich.txid(), 10_000);
    let child = pay(&[out(&rich, 0), out(&poor, 0)], &[200_000 - relay_fee - 10_000]);
    let (result, _) = fx.package(&[rich.clone(), poor.clone(), child.clone()], false);
    let want = PackageMempoolAcceptResult {
        state: PackageValidationState::default(),
        tx_results: results(&[
            (&rich, own(&rich, 0, 10_000)),
            (&poor, own(&poor, relay_fee, relay_fee)),
            (&child, own(&child, 10_000, 10_000)),
        ]),
    };
    assert_eq!(result, want);
}

/// MN0 (4): a `PCKG_MEMPOOL_ERROR` result is invalid, so its coins are uncached
/// (`v:1781-1786`). A child whose non-push scriptSig passes the policy flags but not the tip's
/// block flags reaches "BUG! PolicyScriptChecks succeeded but ConsensusScriptChecks failed"
/// (`v:1305-1314`): its failure is recorded, and, as in Core, its submission carries on, so it is
/// held and signalled while its result stays the failure.
#[test]
fn mempool_error_uncaches() {
    let (mut fx, _) = raised_minimum(MemPoolOptions {
        require_standard: false,
        ..MemPoolOptions::default()
    });
    let coin = fx.fund(1);
    fx.chain.block_flags = ScriptFlags::MANDATORY_V24_0.union(ScriptFlags::SIGPUSHONLY);
    let bare = [OP_TRUE];
    let ins = |prevout, sig: &'static [u8]| [(prevout, sig, u32::MAX)];
    let parent =
        tx_of(2, TxType::Spend, &ins(coin, &SIG_TRUE), &[(49_750, &bare[..]); 2], 0, vec![]);
    let child = tx_of(
        2,
        TxType::Spend,
        &ins(out(&parent, 0), &[OP_NOP]),
        &[(19_875, &bare[..]); 2],
        0,
        vec![],
    );
    let sequence = fx.sequence();
    let (result, uncache) = fx.package(&[parent.clone(), child.clone()], false);
    let what = format!(
        "BUG! PolicyScriptChecks succeeded but ConsensusScriptChecks failed: {}",
        child.txid()
    );
    assert_eq!(result.state, state(PackageValidationResult::MempoolError, &what, ""));
    let failed = &result.tx_results[&child.txid()];
    assert_eq!(failed.result_type, MempoolAcceptResultType::Invalid);
    assert!(failed.reason.starts_with("mandatory-script-verify-flag-failed"), "{failed:?}");
    assert_eq!(result.tx_results[&parent.txid()].result_type, MempoolAcceptResultType::Valid);
    assert_eq!(uncache, vec![out(&parent, 0), coin, out(&parent, 0), out(&parent, 0)]);
    assert!(fx.held(&parent) && fx.held(&child));
    assert_eq!(
        fx.take_events(),
        vec![
            Ev::Added(parent.txid(), parent.instance_hash(), ACCEPT_TIME, sequence),
            Ev::Added(child.txid(), child.instance_hash(), ACCEPT_TIME, sequence + 1),
        ]
    );
}

/// `AcceptPackage`'s held-transaction branch (`v:1641-1650`): a fresher instance of a held
/// stable-txid unlock is refreshed in place (`TryAssetUnlockRefresh`) and final, its child then
/// admitted alone; a stale instance is a final failure (`PCKG_TX`), and the child is still
/// admitted alone, so the package is partly submitted.
#[test]
fn accept_package_refreshes_a_held_unlock() {
    let mut fx = Fx::new();
    let held = unlock(2, 7, 160, 1);
    fx.accept(&held);
    fx.take_events();
    let child = pay(&[out(&held, 0)], &[9_000]);
    let fresher = unlock(2, 7, 170, 2);
    assert_eq!(fresher.txid(), held.txid());
    let (vsize, fee) = {
        let state = fx.pool.pool.read();
        let entry = state.get_iter(&held.txid()).expect("held");
        (entry.vsize() as i64, entry.metadata().fee)
    };
    let sequence = fx.sequence();
    let (result, _) = fx.package(&[fresher.clone(), child.clone()], false);
    assert_eq!(result.state, PackageValidationState::default());
    let refreshed = &result.tx_results[&held.txid()];
    assert_eq!((refreshed.vsize, refreshed.base_fee), (Some(vsize), Some(fee)));
    assert_eq!(result.tx_results[&child.txid()], own(&child, 1_000, 1_000));
    assert_eq!(
        fx.take_events(),
        vec![
            Ev::Added(held.txid(), fresher.instance_hash(), ACCEPT_TIME, sequence),
            Ev::Added(child.txid(), child.instance_hash(), ACCEPT_TIME, sequence + 1),
        ]
    );

    let mut fx = Fx::new();
    fx.accept(&held);
    let stale = unlock(2, 7, 155, 3);
    let (result, _) = fx.package(&[stale, child.clone()], false);
    assert_eq!(result.state, tx_failed());
    assert_eq!(result.tx_results[&held.txid()].reason, "assetunlock-stale-instance");
    assert_eq!(result.tx_results[&child.txid()].result_type, MempoolAcceptResultType::Valid);
    assert!(fx.held(&child));
}
