use std::cell::Cell;

use dash_consensus::primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dash_consensus::script::ScriptFailureKind;

use super::*;
use crate::checkqueue::CheckQueue;
use crate::sigcache::CacheSizes;

#[path = "../../../tests/support/signed.rs"]
mod signed;

use signed::{Spend, Unlock, spend};

const EVAL_FALSE: &str = "mandatory-script-verify-flag-failed (Script evaluated without error but \
                          finished with a false/empty top stack element)";

fn caches() -> ScriptCaches {
    ScriptCaches::new(CacheSizes {
        signature_cache_bytes: 1 << 16,
        script_execution_cache_bytes: 1 << 16,
    })
    .unwrap()
}

fn opts(flags: ScriptFlags, caches: Option<&ScriptCaches>, store: bool) -> ScriptOptions<'_> {
    ScriptOptions {
        flags,
        policy: ScriptPolicy::V24_0,
        caches,
        cache_sig_store: store,
        cache_full_script_store: store,
    }
}

/// Runs `check_input_scripts` serially with a fresh slot: the result, how many spent scripts it
/// read, and whether it built the slot's signature-hash data.
fn serial(s: &Spend, opts: &ScriptOptions<'_>) -> (Result<(), CheckInputsError>, usize, bool) {
    let slot = TxSlot::new(&s.tx);
    let reads = Cell::new(0);
    let spent = |i: usize| {
        reads.set(reads.get() + 1);
        s.spent[i].as_slice()
    };
    let result = check_input_scripts(&slot, spent, opts, None);
    (result, reads.get(), slot.is_initialised())
}

/// The same through `queue`: the call's result and the queue's verdict.
fn queued(
    queue: &CheckQueue,
    s: &Spend,
    opts: &ScriptOptions<'_>,
) -> (Result<(), CheckInputsError>, bool) {
    let slot = TxSlot::new(&s.tx);
    queue.control(|control| {
        check_input_scripts(&slot, |i| s.spent[i].as_slice(), opts, Some(control))
    })
}

fn failure(result: Result<(), CheckInputsError>) -> InputScriptFailure {
    match result {
        Err(CheckInputsError::Script(f)) => f,
        other => panic!("expected a script failure, got {other:?}"),
    }
}

#[test]
fn a_coinbase_is_never_checked() {
    let coinbase = || {
        Tx::new(Transaction {
            version: 2,
            tx_type: TxType::new(0),
            inputs: vec![TxIn {
                prevout: OutPoint {
                    hash: TxHash::from([0; 32]),
                    index: u32::MAX,
                },
                script_sig: vec![0x51, 0x51].into(),
                sequence: u32::MAX,
            }],
            outputs: vec![TxOut {
                value: 50,
                script_pubkey: vec![0x51].into(),
            }],
            lock_time: 0,
            extra_payload: Vec::new(),
        })
    };
    let s = Spend {
        credit: coinbase(),
        tx: coinbase(),
        spent: Vec::new(),
    };
    assert!(s.tx.is_coinbase());
    let caches = caches();
    let (result, reads, init) = serial(&s, &opts(ScriptFlags::STANDARD, Some(&caches), true));
    assert!(result.is_ok());
    assert_eq!((reads, init), (0, false));
}

/// `CheckInputScripts`' script-execution cache: stored only when asked and every script passed,
/// keyed on the flags, consulted before `txdata.Init` and before any coin is read.
#[test]
fn the_script_execution_cache_skips_what_already_passed() {
    let caches = caches();
    let s = spend(&[Unlock::Signed, Unlock::Signed, Unlock::Signed], 1);
    let flags = ScriptFlags::MANDATORY_V24_0;
    // Block connection neither stores nor hits.
    assert_eq!(serial(&s, &opts(flags, Some(&caches), false)).1, 3);
    assert_eq!(serial(&s, &opts(flags, Some(&caches), false)).1, 3);
    // The mempool stores; then every later check under the same flags hits.
    let (result, reads, init) = serial(&s, &opts(flags, Some(&caches), true));
    assert_eq!((result.is_ok(), reads, init), (true, 3, true));
    let (result, reads, init) = serial(&s, &opts(flags, Some(&caches), true));
    assert_eq!((result.is_ok(), reads, init), (true, 0, false));
    // Other flags are another entry.
    assert_eq!(serial(&s, &opts(ScriptFlags::STANDARD, Some(&caches), false)).1, 3);
    // A hit without store soft-erases: the entry still hits until it is overwritten.
    assert_eq!(serial(&s, &opts(flags, Some(&caches), false)).1, 0);
    assert_eq!(serial(&s, &opts(flags, Some(&caches), false)).1, 0);
    let entry = caches.exec.entry(s.tx.txid().as_bytes(), flags);
    assert!(caches.exec.cache().contains(entry, false));
}

#[test]
fn a_failing_transaction_is_never_stored() {
    let caches = caches();
    let s = spend(&[Unlock::Signed, Unlock::WrongDigest], 2);
    let opts = opts(ScriptFlags::MANDATORY_V24_0, Some(&caches), true);
    for _ in 0..2 {
        let (result, reads, _) = serial(&s, &opts);
        assert_eq!(failure(result).n_in, 1);
        assert_eq!(reads, 2);
    }
    let entry = caches.exec.entry(s.tx.txid().as_bytes(), ScriptFlags::MANDATORY_V24_0);
    assert!(!caches.exec.cache().contains(entry, false));
}

/// With `pvChecks` Core inserts nothing in the script-execution cache: the queue has not run yet.
#[test]
fn queued_checks_are_never_stored_as_executed() {
    let caches = caches();
    let queue = CheckQueue::new(2).unwrap();
    let s = spend(&[Unlock::Signed, Unlock::Signed], 3);
    let opts = opts(ScriptFlags::MANDATORY_V24_0, Some(&caches), true);
    let (result, ok) = queued(&queue, &s, &opts);
    assert!(result.is_ok() && ok);
    assert_eq!(serial(&s, &opts).1, 2, "the parallel run stored no script-execution entry");
    // A stored entry is still honoured in parallel mode: nothing is queued.
    let slot = TxSlot::new(&s.tx);
    let (result, ok) = queue.control(|control| {
        check_input_scripts(&slot, |_| unreachable!("a hit reads no coin"), &opts, Some(control))
    });
    assert!(result.is_ok() && ok);
    assert!(!slot.is_initialised());
}

/// `ConnectBlock`'s report for a transaction with failing inputs: in serial mode the first failing
/// input's reason, in parallel mode `block-validation-failed`; with the caches on or off.
#[test]
fn each_mode_reports_what_core_reports() {
    let caches = caches();
    let parallel = CheckQueue::new(3).unwrap();
    let inline = CheckQueue::serial();
    let s = spend(&[Unlock::Signed, Unlock::WrongDigest, Unlock::Signed, Unlock::WrongDigest], 4);
    for cached in [None, Some(&caches)] {
        for store in [false, true] {
            let opts = opts(ScriptFlags::MANDATORY_V24_0, cached, store);
            let f = failure(serial(&s, &opts).0);
            assert_eq!(
                (f.kind, f.error, f.n_in),
                (ScriptFailureKind::Mandatory, ScriptError::EvalFalse, 1)
            );
            let block = input_scripts_failed(f);
            assert_eq!(block.result(), BlockValidationResult::Consensus);
            assert_eq!(block.reject_reason(), EVAL_FALSE);
            assert_eq!(block.debug_message(), "");
            // A control without threads is serial mode: checks run as they are added.
            let (result, ok) = queued(&inline, &s, &opts);
            assert_eq!(failure(result).n_in, 1);
            assert!(ok, "serial mode adds nothing to the queue");
            let (result, ok) = queued(&parallel, &s, &opts);
            assert!(result.is_ok());
            assert!(!ok);
        }
    }
    let block = check_queue_failed();
    assert_eq!(block.result(), BlockValidationResult::Consensus);
    assert_eq!(block.reject_reason(), "block-validation-failed");
    assert_eq!(block.debug_message(), "");
}

/// Under the standard flags a wrong signature first fails NULLFAIL; the re-run without the
/// non-mandatory flags fails too, so it is a consensus failure reported with NULLFAIL's message.
#[test]
fn the_reported_error_is_the_first_runs() {
    let caches = caches();
    let s = spend(&[Unlock::WrongDigest], 5);
    for cached in [None, Some(&caches)] {
        let f = failure(serial(&s, &opts(ScriptFlags::STANDARD, cached, true)).0);
        assert_eq!((f.kind, f.error), (ScriptFailureKind::Mandatory, ScriptError::SigNullFail));
        assert_eq!(
            input_scripts_failed(f).reject_reason(),
            format!("mandatory-script-verify-flag-failed ({})", ScriptError::SigNullFail.message())
        );
    }
}

#[test]
fn invalid_flags_are_refused_in_every_mode() {
    let caches = caches();
    let parallel = CheckQueue::new(2).unwrap();
    let s = spend(&[Unlock::Signed], 6);
    let flags = ScriptFlags::CLEANSTACK;
    for cached in [None, Some(&caches)] {
        let opts = opts(flags, cached, true);
        assert!(matches!(serial(&s, &opts).0, Err(CheckInputsError::Flags(_))));
        assert!(matches!(queued(&parallel, &s, &opts).0, Err(CheckInputsError::Flags(_))));
    }
}

/// Sol r1's case (D24): B spends A's output with a wrong signature. After A has passed and been
/// stored, B is still checked as itself, in both modes: rejected, and never stored. Its slot's
/// signature-hash data is B's, the only data a slot can be built with (`TxSlot`'s compile-fail
/// example), so A's data can never stand in for B. It must pass in release too (`cargo test
/// --release`), where the `debug_assert!` that r1 relied on vanished.
#[test]
fn a_transaction_is_checked_and_stored_as_itself() {
    let caches = caches();
    let flags = ScriptFlags::MANDATORY_V24_0;
    let store = opts(flags, Some(&caches), true);
    let a = spend(&[Unlock::Signed], 24);
    let b = spend(&[Unlock::WrongDigest], 24);
    assert_eq!(a.tx.inputs()[0].prevout, b.tx.inputs()[0].prevout);
    assert_ne!(a.tx.txid(), b.tx.txid());
    let stored = |s: &Spend| {
        caches.exec.cache().contains(caches.exec.entry(s.tx.txid().as_bytes(), flags), false)
    };

    let slot = TxSlot::new(&a.tx);
    check_input_scripts(&slot, |i| a.spent[i].as_slice(), &store, None).unwrap();
    assert!(stored(&a));

    let slot = TxSlot::new(&b.tx);
    let f = failure(check_input_scripts(&slot, |i| b.spent[i].as_slice(), &store, None));
    assert!(slot.is_initialised(), "B missed the script-execution cache and ran");
    assert!(std::ptr::eq(slot.data().tx(), b.tx.raw()));
    assert_eq!(input_scripts_failed(f).reject_reason(), EVAL_FALSE);
    assert!(!stored(&b));
    // In parallel mode nothing is stored either way (D14): the verdict is what counts.
    let (result, ok) = queued(&CheckQueue::new(2).unwrap(), &b, &store);
    assert!(result.is_ok() && !ok);
    assert!(!stored(&b));
}

/// The script-execution cache's calls during `f`: lookups, soft erases and inserts.
fn exec_ops(caches: &ScriptCaches, f: impl FnOnce()) -> (usize, usize, usize) {
    let before = caches.exec.cache().ops();
    f();
    let d = caches.exec.cache().ops() - before;
    (d.lookups, d.erases, d.inserts)
}

/// CH3 D43's pair, hit then miss: a hit is soft-erased once by the probe and reads no coin; a miss
/// erases nothing, and the unprobed checks after it never consult the cache again. The full
/// `check_input_scripts` is the same pair: one lookup, and one erase per hit.
#[test]
fn a_probe_soft_erases_a_hit_once_and_unprobed_never_probes() {
    let caches = caches();
    let flags = ScriptFlags::MANDATORY_V24_0;
    let (hit, miss) = (spend(&[Unlock::Signed, Unlock::Signed], 30), spend(&[Unlock::Signed], 31));
    // The mempool's storing run.
    assert!(serial(&hit, &opts(flags, Some(&caches), true)).0.is_ok());
    let tip = opts(flags, Some(&caches), false);
    let parallel = CheckQueue::new(2).unwrap();

    let slot = TxSlot::new(&hit.tx);
    assert_eq!(exec_ops(&caches, || assert!(exec_cache_probe(&slot, &tip).is_none())), (1, 1, 0));
    assert!(!slot.is_initialised(), "a hit builds no signature-hash data");

    for queue in [&CheckQueue::serial(), &parallel] {
        let slot = TxSlot::new(&miss.tx);
        let mut probed = None;
        assert_eq!(exec_ops(&caches, || probed = exec_cache_probe(&slot, &tip)), (1, 0, 0));
        let probed = probed.expect("a miss");
        let ops = exec_ops(&caches, || {
            let (result, ok) = queue.control(|control| {
                check_input_scripts_unprobed(
                    probed,
                    |i| miss.spent[i].as_slice(),
                    &tip,
                    Some(control),
                )
            });
            assert!(result.is_ok() && ok);
        });
        assert_eq!(ops, (0, 0, 0), "unprobed never consults the script-execution cache");
        assert!(slot.is_initialised());
    }

    // check_input_scripts: one erase per hit, and a miss erases nothing.
    for _ in 0..2 {
        assert_eq!(exec_ops(&caches, || assert_eq!(serial(&hit, &tip).1, 0)), (1, 1, 0));
    }
    assert_eq!(exec_ops(&caches, || assert_eq!(serial(&miss, &tip).1, 1)), (1, 0, 0));
    // A storing caller's hit is kept, not erased.
    let store = opts(flags, Some(&caches), true);
    assert_eq!(exec_ops(&caches, || assert!(exec_cache_probe(&slot, &store).is_none())), (1, 0, 0));
    // With the caches off there is nothing to hit.
    let off = opts(flags, None, false);
    assert!(exec_cache_probe(&TxSlot::new(&hit.tx), &off).is_some());
}

/// The store matrix (CH3 §2.9, CS6 D14): with `cache_sig_store` the signatures are inserted in
/// either mode, but the script-execution entry only by a serial run (`cacheFullScriptStore &&
/// !pvChecks`, `v:2227`), so a parallel run with both flags set inserts no entry; a run without
/// them inserts nothing. Driven through the pair, as `ConnectBlock` drives it.
#[test]
fn the_store_flags_decide_what_each_mode_inserts() {
    let flags = ScriptFlags::MANDATORY_V24_0;
    let serial_queue = CheckQueue::serial();
    let parallel = CheckQueue::new(3).unwrap();
    for (n, (queue, store)) in
        [(&serial_queue, true), (&serial_queue, false), (&parallel, true), (&parallel, false)]
            .into_iter()
            .enumerate()
    {
        let caches = caches();
        let s = spend(&[Unlock::Signed, Unlock::Signed, Unlock::Signed], 40 + n as u32);
        let opts = opts(flags, Some(&caches), store);
        let slot = TxSlot::new(&s.tx);
        let (sig, exec) = (caches.sig.cache().ops(), caches.exec.cache().ops());
        let hashes = caches.exec.cache().hashes();
        let probed = exec_cache_probe(&slot, &opts).expect("a miss");
        let (result, ok) = queue.control(|control| {
            check_input_scripts_unprobed(probed, |i| s.spent[i].as_slice(), &opts, Some(control))
        });
        // The probe's entry is the one inserted: hashed once (Opus a4 r1 N3).
        assert_eq!(caches.exec.cache().hashes() - hashes, 1, "{n}");
        assert!(result.is_ok() && ok);
        let (sig, exec) = (caches.sig.cache().ops() - sig, caches.exec.cache().ops() - exec);
        let mode = if queue.has_threads() {
            "parallel"
        } else {
            "serial"
        };
        let exec_insert = store && !queue.has_threads();
        assert_eq!(
            sig.inserts,
            if store {
                3
            } else {
                0
            },
            "{mode}, store {store}: signatures"
        );
        assert_eq!(exec.inserts, usize::from(exec_insert), "{mode}, store {store}: execution");
        let entry = caches.exec.entry(s.tx.txid().as_bytes(), flags);
        assert_eq!(
            caches.exec.cache().contains(entry, false),
            exec_insert,
            "{mode}, store {store}"
        );
    }
}

/// D41: `control_inline` on a queue with threads is serial mode: the checks run as they are
/// added, the first failure is the transaction's, and the verdict is true, as `-par=1` gives.
#[test]
fn control_inline_on_a_threaded_queue_is_serial_mode() {
    let caches = caches();
    let parallel = CheckQueue::new(3).unwrap();
    let s = spend(&[Unlock::Signed, Unlock::WrongDigest, Unlock::Signed], 50);
    let opts = opts(ScriptFlags::MANDATORY_V24_0, Some(&caches), false);
    let slot = TxSlot::new(&s.tx);
    let (result, ok) = parallel.control_inline(|control| {
        assert!(!control.is_parallel());
        check_input_scripts(&slot, |i| s.spent[i].as_slice(), &opts, Some(control))
    });
    assert_eq!(failure(result).n_in, 1);
    assert!(ok, "nothing was queued");
    assert_eq!(failure(serial(&s, &opts).0).n_in, 1);
}
