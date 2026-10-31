//! `ConnectBlock` on CS6's check queue through V (CH3 §2.4, §2.9; CH3a slice a4, D127–D130), at
//! `-par=1`, `-par=2`, `-par=16` (the cap) and `-par=-60` (CH3a a9, DEC-323). Core's
//! `CCheckQueueControl` has a queue only with script checks and threads (`v:2649`). Then a script
//! failure is the queue's, reported at `Wait` after the loop and before step 18 (`v:2778-2781`),
//! and an earlier return from the loop ignores it; without threads each transaction's
//! `CheckInputScripts` reports its own (`v:2752-2758`). Every row of §2.4's matrix runs in each
//! mode, with Core's reason and class (`assert_rejected`), then the tip connect's store policy
//! (`fCacheResults = fJustCheck`, `v:2750`) and which transactions build their signature-hash data
//! (`txdata.Init` after the exec probe, `v:2174-2179`).

use dash_consensus::ctx::DeploymentPos;
use dash_consensus::script::{BuriedScriptRules, ScriptFlags, block_script_flags};

use super::super::support::lines_with;
use super::*;
use crate::checkqueue::script_check_threads;

#[expect(
    clippy::duplicate_mod,
    reason = "CS6's unit tests (`checkqueue/script/tests.rs`) load it privately too"
)]
#[path = "../../../../tests/support/signed.rs"]
mod signed;

/// A seeded coin: (outpoint, output, height, coinbase).
type Seed = (OutPoint, TxOut, u32, bool);

/// A seed script that fails: `OP_0`, a false top stack element.
const FALSE: &[u8] = &[0x00];

/// The reason `-par=1` gives a transaction whose `FALSE` script fails (`v:2752-2758`).
const SCRIPT_REASON: &str = "mandatory-script-verify-flag-failed (Script evaluated without error \
                             but finished with a false/empty top stack element)";

/// DEC-323's `-par` values on a host with 64 cores (`init.cpp:1554-1565`): serial, then 1, 15
/// (the cap) and 3 threads beside the caller, the last leaving 60 cores free.
pub(super) fn modes() -> [(&'static str, usize); 4] {
    let mode = |name, par| (name, script_check_threads(par, 64));
    [mode("-par=1", 1), mode("-par=2", 2), mode("-par=16", 16), mode("-par=-60", -60)]
}

/// A failed script's verdict: the transaction's own at `-par=1`, the queue's with threads
/// (`v:2778-2781`). Both are `BLOCK_CONSENSUS`.
pub(super) fn script_failed(threads: usize) -> BlockState {
    let reason = if threads == 0 {
        SCRIPT_REASON
    } else {
        "block-validation-failed"
    };
    reject(BlockValidationResult::Consensus, reason)
}

/// The default setup over `seeds`.
fn seeded(seeds: Vec<Seed>) -> Setup {
    Setup {
        seeds,
        ..Setup::default()
    }
}

/// One matrix row in each mode: `setup` with the mode's queue, `build`'s blocks, rejected as
/// `want(threads)` says.
fn row(
    name: &str,
    setup: impl Fn() -> Setup,
    build: impl Fn(&ChainSetup, &Mined) -> Vec<Mined>,
    want: impl Fn(usize) -> BlockState,
) {
    for (mode, threads) in modes() {
        // Shown with a failure: which row and mode.
        eprintln!("{name} at {mode}");
        assert_rejected(
            Setup {
                script_threads: threads,
                ..setup()
            },
            &build,
            want(threads),
        );
    }
}

/// Seed `n` locked by `FALSE`, and seed `n + 1` by `OP_TRUE`.
fn failing_and_true(n: u8) -> Vec<Seed> {
    vec![seed(n, 50 * COIN, FALSE), seed(n + 1, 50 * COIN, &[OP_TRUE])]
}

/// A transaction spending seed `n`: its script fails when the seed is `FALSE`.
fn spends(n: u8) -> Arc<Tx> {
    spend(&[seed_op(n)], COIN)
}

/// L5: a version-2 spend of seed `n` (height 0) whose relative lock waits five blocks.
fn nonfinal(n: u8) -> Arc<Tx> {
    tx(2, 0, vec![input(seed_op(n), Vec::new(), 5)], vec![out(COIN, &[OP_TRUE])])
}

/// L6: four P2SH seeds from `n` whose redeem scripts are 520 `OP_CHECKMULTISIG`s each, and the
/// spend of the first `inputs` of them: 10,400 sigops an input, against the pre-DIP1 limit of
/// 20,000 (`v:2740-2744`). The redeem script does not hash to the seeds' zero hash, so the spend's
/// scripts fail too (`OP_EQUAL` leaves false).
fn sigops(n: u8, inputs: u8) -> (Vec<Seed>, Arc<Tx>) {
    let mut p2sh = vec![0xa9, 20];
    p2sh.extend([0; 20]);
    p2sh.push(0x87);
    let mut sig = vec![0x4d, 0x08, 0x02];
    sig.extend([0xae; 520]);
    let seeds = (n..n + 4).map(|k| seed(k, COIN, &p2sh)).collect();
    let ins = (n..n + inputs).map(|k| input(seed_op(k), sig.clone(), u32::MAX)).collect();
    (seeds, tx(1, 0, ins, vec![out(COIN, &[OP_TRUE])]))
}

/// The flags `ConnectBlock` checks a block at `height` with (`v:2419-2441`): each buried
/// deployment active at its height or after.
fn flags_at(p: &ChainSetup, height: u32) -> ScriptFlags {
    let at = |dep| height >= p.consensus.deployment_height(dep).to_u32();
    block_script_flags(BuriedScriptRules {
        dersig: at(BuriedDeployment::DerSig),
        cltv: at(BuriedDeployment::Cltv),
        csv: at(BuriedDeployment::Csv),
        bip147: at(BuriedDeployment::Bip147),
    })
}

/// A cache's (lookups, soft erases, inserts) during `f`.
fn ops(cache: &crate::sigcache::SaltedCache, f: impl FnOnce()) -> (usize, usize, usize) {
    let before = cache.ops();
    f();
    let d = cache.ops() - before;
    (d.lookups, d.erases, d.inserts)
}

/// Row 1: a script failure alone. With threads the queue's verdict comes after the loop, logged
/// as Core's `LogPrintf("ERROR: %s: CheckQueue failed\n", __func__)` (INFO, CH3a D65), and the
/// transaction's own line is never logged; at `-par=1` the transaction's line is logged.
#[test]
fn a_script_failure_alone_is_the_transactions_or_the_queues() {
    install_accept_log();
    // An amount no other test spends, so the transaction's log line is this test's.
    let failing = spend(&[seed_op(1)], COIN + 0xa4_01);
    let own = format!("ConnectBlock(): CheckInputScripts on {} failed with", failing.txid());
    for (mode, threads) in modes() {
        assert_rejected(
            Setup {
                seeds: vec![seed(1, 50 * COIN, FALSE)],
                script_threads: threads,
                ..Setup::default()
            },
            |p, g| vec![block_on(p, g, vec![failing.clone()])],
            script_failed(threads),
        );
        // The serial run (first) logged it once; the parallel runs add nothing.
        assert_eq!(lines_with(&own).len(), 1, "{mode}");
    }
    // The line names no transaction, and the capture is the process's: this shows only that some
    // run with threads logged it, which this test's runs do unless another's did first.
    assert!(!lines_with("ERROR: ConnectBlock: CheckQueue failed").is_empty());
}

/// Row 2: a script failure in *i*, then an L2–L6 failure, a shared-collateral reject or the fee
/// total out of range in *j* > *i*: *j*'s reason with threads (the loop returns before `Wait`,
/// whose verdict the control's destructor ignores), *i*'s at `-par=1`. Each of the loop's returns
/// between the control and `Wait` (`v:2698-2743`) has a row.
#[test]
fn a_later_input_failure_wins_over_a_queued_script_failure() {
    let later = |threads, reason| {
        if threads == 0 {
            script_failed(0)
        } else {
            reject(BlockValidationResult::Consensus, reason)
        }
    };
    // L2: a missing coin.
    row(
        "missing after",
        || seeded(failing_and_true(1)),
        |p, g| vec![block_on(p, g, vec![spends(1), spends(9)])],
        |t| later(t, "bad-txns-inputs-missingorspent"),
    );
    // L5: a relative lock not yet met.
    row(
        "nonfinal after",
        || seeded(failing_and_true(1)),
        |p, g| vec![block_on(p, g, vec![spends(1), nonfinal(2)])],
        |t| later(t, "bad-txns-nonfinal"),
    );
    // v:2708-2716: H8 rejects a shared-collateral spend (V24).
    row(
        "shared collateral after",
        || Setup {
            steps: StepsCfg {
                complete: true,
                reject_h8_at: Some(2),
                ..StepsCfg::default()
            },
            extra: Some(Deployment::Versionbits(DeploymentPos::V24)),
            ..seeded(failing_and_true(1))
        },
        |p, g| vec![block_on(p, g, vec![spends(1), spends(2)])],
        |t| later(t, "bad-shared-collateral-spend"),
    );
    // v:2718-2720: the fees so far past `MAX_MONEY`, each in range.
    row(
        "fee total after",
        || {
            let mut seeds = vec![seed(1, 50 * COIN, FALSE)];
            seeds.extend((2..4).map(|n| seed(n, 15_000_000 * COIN, &[OP_TRUE])));
            seeded(seeds)
        },
        |p, g| {
            let txs = vec![spends(1), spend(&[seed_op(2)], 1), spend(&[seed_op(3)], 1)];
            vec![block_on(p, g, txs)]
        },
        |t| later(t, "bad-txns-accumulated-fee-outofrange"),
    );
    // L6: the block's sigops past the limit.
    row(
        "sigops after",
        || {
            let mut seeds = sigops(3, 4).0;
            seeds.push(seed(1, 50 * COIN, FALSE));
            seeded(seeds)
        },
        |p, g| vec![block_on(p, g, vec![spends(1), sigops(3, 4).1])],
        |t| later(t, "bad-blk-sigops"),
    );
}

/// Row 3: an L2–L6 failure in *j* < *i*, then a script failure in *i*: *j*'s reason either way.
#[test]
fn an_earlier_input_failure_wins_in_every_mode() {
    row(
        "nonfinal before",
        || seeded(failing_and_true(1)),
        |p, g| vec![block_on(p, g, vec![nonfinal(2), spends(1)])],
        |_| reject(BlockValidationResult::Consensus, "bad-txns-nonfinal"),
    );
    row(
        "missing before",
        || seeded(failing_and_true(1)),
        |p, g| vec![block_on(p, g, vec![spends(9), spends(1)])],
        |_| reject(BlockValidationResult::Consensus, "bad-txns-inputs-missingorspent"),
    );
}

/// A fault in the loop after a queued script failure (here H8's, `CheckSharedCollateralSpends`
/// under V24, `v:2708-2716`) ends the connect as a system error with threads: the control waits
/// and its verdict is ignored, as for row 2's verdicts (CH3 §2.4). At `-par=1` the failing
/// transaction is rejected before H8 sees the next one.
#[test]
fn a_fault_after_a_queued_script_failure_is_a_system_error() {
    for (mode, threads) in modes() {
        eprintln!("H8's fault at {mode}");
        let setup = Setup {
            steps: StepsCfg {
                complete: true,
                fail_h8_at: Some(2),
                ..StepsCfg::default()
            },
            extra: Some(Deployment::Versionbits(DeploymentPos::V24)),
            seeds: failing_and_true(1),
            script_threads: threads,
            ..Setup::default()
        };
        let build = |p: &ChainSetup, g: &Mined| vec![block_on(p, g, vec![spends(1), spends(2)])];
        if threads == 0 {
            assert_rejected(setup, build, script_failed(0));
        } else {
            assert_system_error(setup, build, H8_FAILED);
        }
    }
}

/// Row 4: the block's sigops pass the limit at a transaction whose own scripts fail: L6 precedes
/// L7 for the same transaction (`v:2740-2761`), so `bad-blk-sigops` either way. With one input
/// the sigops are within the limit and the same scripts fail as row 1 does.
#[test]
fn sigops_precede_the_same_transactions_scripts() {
    row(
        "sigops, failing scripts",
        || seeded(sigops(1, 4).0),
        |p, g| vec![block_on(p, g, vec![sigops(1, 4).1])],
        |_| reject(BlockValidationResult::Consensus, "bad-blk-sigops"),
    );
    row(
        "within the limit, failing scripts",
        || seeded(sigops(1, 1).0),
        |p, g| vec![block_on(p, g, vec![sigops(1, 1).1])],
        script_failed,
    );
}

/// Row 5: a script failure and an InstantSend conflict without a ChainLock (`conflict-tx-lock`,
/// `BLOCK_CHAINLOCK`, step 18): `Wait` precedes the IS loop (`v:2778-2819`).
#[test]
fn a_script_failure_precedes_conflict_tx_lock() {
    row(
        "conflict-tx-lock",
        || Setup {
            seeds: vec![seed(1, 50 * COIN, FALSE)],
            is_store: vec![lock_a()],
            helper: Scripted {
                ibd: Some(false),
                cl_has: false,
                ..Scripted::default()
            },
            ..Setup::default()
        },
        |p, g| vec![block_on(p, g, vec![spends(1)])],
        script_failed,
    );
}

/// Row 6: a script failure and a coinbase that overpays (`bad-cb-amount`, `v:2846-2850`) or
/// misses the masternode payee (`bad-cb-payee`, `v:2860-2865`): both after `Wait`.
#[test]
fn a_script_failure_precedes_the_coinbase_checks() {
    row(
        "bad-cb-amount",
        || seeded(vec![seed(1, 50 * COIN, FALSE)]),
        |p, g| vec![with_coinbase(p, g, coinbase_tx(1, 1, 0, 1_000_000 * COIN), vec![spends(1)])],
        script_failed,
    );
    row(
        "bad-cb-payee",
        || Setup {
            params: Some(early_enforcement()),
            steps: StepsCfg {
                complete: true,
                mn_payee: false,
                ..StepsCfg::default()
            },
            seeds: vec![seed(1, 50 * COIN, FALSE)],
            ..Setup::default()
        },
        |p, g| vec![block_on(p, g, vec![spends(1)])],
        script_failed,
    );
}

/// Row 7: a script failure and a special transaction H4 refuses (`ProcessSpecialTxsInBlock`,
/// `v:2661-2670`), which runs before the loop: H4's reason either way.
#[test]
fn h4_precedes_a_script_failure() {
    row(
        "H4",
        || seeded(failing_and_true(1)),
        |p, g| {
            let pro = tx(
                3,
                1,
                vec![input(seed_op(2), Vec::new(), u32::MAX)],
                vec![out(COIN, &[OP_TRUE])],
            );
            vec![block_on(p, g, vec![spends(1), pro])]
        },
        |_| reject(BlockValidationResult::Consensus, "bad-tx-type-dip3-inactive"),
    );
}

/// Row 8: below `-assumevalid` (`fScriptChecks` false, `v:2542-2571`) no script runs in either mode,
/// so the failing one connects. Its control is built without the queue (`v:2649`), so none over
/// the pool opens, and L7 is skipped whole (`v:2746`): no exec probe, no copy, no signature-hash
/// data. Off its chain the scripts are checked and fail as row 1 does, through a control over the
/// pool with threads.
#[test]
fn assumevalid_skips_the_queue_too() {
    let c = AvChain::new();
    for (mode, threads) in modes() {
        for (av, checked) in [(c.b[2].hash(), false), (c.side[0].hash(), true)] {
            let caches = new_caches();
            let (w, state, ..) = c.world_with(threads, Some(caches.clone()), av, 1, None);
            let seen = w.seen.lock().expect("seen");
            let controls = seen.queue.as_ref().expect("the harness's").controls();
            let exec = caches.exec.cache().ops();
            let exec = (exec.lookups, exec.erases, exec.inserts);
            if checked {
                eprintln!("checked at {mode}");
                assert_verdict(&state, &script_failed(threads));
                assert_eq!(controls, usize::from(threads > 0), "{mode}");
                assert_eq!(exec, (1, 0, 0), "{mode}: probed, a miss");
            } else {
                assert_verdict(&state, &BlockState::Valid);
                assert_eq!(controls, 0, "{mode}: no control over the pool");
                assert_eq!(exec, (0, 0, 0), "{mode}: no probe");
                assert_eq!(seen.scripts, [(1, 0, 0)], "{mode}: no copy, no sig data");
            }
        }
    }
}

/// §2.9's store policy at the tip (`fCacheResults = fJustCheck` = false, `v:2750`), in each mode:
/// a signed spend's checks consult both caches and insert into neither (`cacheSigStore` false;
/// the exec insert needs `cacheFullScriptStore`, `v:2227`), its one transaction builds its
/// signature-hash data once (`v:2179`), and both its inputs' scripts are copied (`v:2194`).
#[test]
fn the_tip_connect_stores_nothing() {
    for (mode, threads) in modes() {
        let s = signed::spend(&[signed::Unlock::Signed, signed::Unlock::Signed], 0xa4);
        let seeds = (0..2u32)
            .map(|i| (op(&s.credit, i), s.credit.outputs()[i as usize].clone(), 0, false))
            .collect();
        let caches = new_caches();
        let w = world(Setup {
            seeds,
            script_threads: threads,
            caches: Some(caches.clone()),
            ..Setup::default()
        });
        let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![Arc::new(s.tx)]);
        let mut state = None;
        let sig = ops(caches.sig.cache(), || {
            let exec = ops(caches.exec.cache(), || state = Some(last_state(&w, &[b1])));
            assert_eq!(exec, (1, 0, 0), "{mode}: one exec probe, a miss, no insert");
        });
        assert_verdict(&state.expect("connected"), &BlockState::Valid);
        assert_eq!(sig, (2, 0, 0), "{mode}: a lookup per signature, no insert");
        assert_eq!(w.seen.lock().expect("seen").scripts, [(1, 1, 2)], "{mode}");
    }
}

/// An exec-cache hit (a transaction the mempool validated under the block's flags) is erased once
/// by the tip connect (`contains(entry, !cacheFullScriptStore)`, `v:2174`) and skips everything
/// after the probe: no coin copy, no check (so a failing script connects), no signature-hash data.
/// A coinbase-only block copies and builds nothing either (`v:2159`); a miss copies its input.
#[test]
fn an_exec_hit_is_erased_once_and_builds_no_sig_data() {
    for (mode, threads) in modes() {
        let caches = new_caches();
        let w = world(Setup {
            seeds: failing_and_true(1),
            script_threads: threads,
            caches: Some(caches.clone()),
            ..Setup::default()
        });
        let (hit, miss) = (spends(1), spends(2));
        let flags = flags_at(&w.rig.p, 1);
        caches.exec.cache().insert(caches.exec.entry(hit.txid().as_bytes(), flags));
        let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![hit]);
        let b2 = block_on(&w.rig.p, &b1, vec![]);
        let b3 = block_on(&w.rig.p, &b2, vec![miss]);
        let exec = ops(caches.exec.cache(), || {
            assert_verdict(&last_state(&w, &[b1]), &BlockState::Valid);
        });
        assert_eq!(exec, (1, 1, 0), "{mode}: the hit, erased once");
        let exec = ops(caches.exec.cache(), || {
            assert_verdict(&last_state(&w, &[b2, b3]), &BlockState::Valid);
        });
        assert_eq!(exec, (1, 0, 0), "{mode}: the coinbase is never probed; the miss once");
        assert_eq!(caches.sig.cache().ops().inserts, 0, "{mode}");
        let scripts = &w.seen.lock().expect("seen").scripts;
        assert_eq!(scripts, &[(1, 0, 0), (2, 0, 0), (3, 1, 1)], "{mode}");
    }
}
