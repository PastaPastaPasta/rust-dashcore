//! Core's script vectors with the caches on and off (CS6): every row of `script_tests.json` and
//! every script row of `tx_valid.json` / `tx_invalid.json` through CS6's `check_input_scripts` in
//! each cache state Core can be in, serially and on a parallel check queue. The uncached serial
//! result must be Core's (CS1's L0 checks it under flag perturbations too), and every other
//! state must give exactly that result: cache hits never change a verdict.
//!
//! The states, in order, over caches shared by the whole file:
//! 1. no caches;
//! 2. signatures stored, scripts not (the mempool's first look), twice: the second run hits the
//!    signature cache and misses the script-execution cache;
//! 3. both stored (the mempool's accept), twice: the second run must hit and read no coin;
//! 4. neither stored (block connection), which soft-erases what it hits;
//! 5. the parallel queue, with and without the caches: the queue's verdict.
//!
//! A shared script-execution cache assumes what Core's does, that a txid commits to the coins it
//! spends. The transaction vectors reuse made-up outpoints with other scripts, so a row that
//! repeats an earlier row's txid and flags with other coins starts a fresh script-execution
//! cache; the signature cache, whose entries commit to everything a verification reads, stays
//! shared.

use std::cell::Cell;
use std::collections::HashMap;

use dash_consensus::Tx;
use dash_consensus::primitives::{decode_tx, decode_tx_exact};
use dash_consensus::script::{CheckInputsError, ScriptFlags, ScriptPolicy};
use dash_core_vectors::cases::{
    TxCase, TxFlags, credit_tx, load_script_tests, load_tx_invalid, load_tx_valid, spend_tx,
};
use dash_core_vectors::flags::{DEFINED_FLAGS, fill_flags};
use dash_core_vectors::read_core_text;
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::checkqueue::script::{ScriptOptions, TxSlot, check_input_scripts};
use dashd_chainstate::sigcache::{CacheSizes, ScriptCaches, ScriptExecCache};

/// One transaction to check: its spent scripts in input order and its flags.
struct Row {
    what: String,
    tx: Tx,
    spent: Vec<Vec<u8>>,
    flags: ScriptFlags,
}

/// The serial result with these caches and store flags, and how many coins it read.
fn serial(
    row: &Row,
    caches: Option<&ScriptCaches>,
    sig_store: bool,
    full_store: bool,
) -> (Result<(), CheckInputsError>, usize) {
    let opts = ScriptOptions {
        flags: row.flags,
        policy: ScriptPolicy::V24_0,
        caches,
        cache_sig_store: sig_store,
        cache_full_script_store: full_store,
    };
    let slot = TxSlot::new(&row.tx);
    let reads = Cell::new(0);
    let spent = |i: usize| {
        reads.set(reads.get() + 1);
        row.spent[i].as_slice()
    };
    (check_input_scripts(&slot, spent, &opts, None), reads.get())
}

struct Harness {
    caches: ScriptCaches,
    queue: CheckQueue,
    /// (txid, flags) → spent scripts, for the shared script-execution cache's assumption.
    seen: HashMap<([u8; 32], u32), Vec<Vec<u8>>>,
    /// Rows whose warm run hit the script-execution cache.
    exec_hits: usize,
    /// How often a row reused another's txid and flags with other coins.
    exec_resets: usize,
}

impl Harness {
    fn new() -> Self {
        Self {
            caches: ScriptCaches::new(CacheSizes::default()).unwrap(),
            queue: CheckQueue::new(3).unwrap(),
            seen: HashMap::new(),
            exec_hits: 0,
            exec_resets: 0,
        }
    }

    fn parallel(&self, row: &Row, caches: Option<&ScriptCaches>) -> Result<bool, CheckInputsError> {
        let opts = ScriptOptions {
            flags: row.flags,
            policy: ScriptPolicy::V24_0,
            caches,
            cache_sig_store: false,
            cache_full_script_store: false,
        };
        let slot = TxSlot::new(&row.tx);
        let (result, ok) = self.queue.control(|control| {
            check_input_scripts(&slot, |i| row.spent[i].as_slice(), &opts, Some(control))
        });
        result.map(|()| ok)
    }

    /// The uncached serial result, after checking that every other state gives the same.
    fn check(&mut self, row: &Row) -> Result<Result<(), CheckInputsError>, String> {
        let key = (*row.tx.txid().as_bytes(), row.flags.bits());
        if self.seen.get(&key).is_some_and(|spent| *spent != row.spent) {
            // Coins Core could never see together: start another script-execution cache.
            self.caches.exec =
                ScriptExecCache::new(CacheSizes::default().script_execution_cache_bytes).unwrap();
            self.seen.clear();
            self.exec_resets += 1;
        }
        self.seen.insert(key, row.spent.clone());

        let (base, _) = serial(row, None, false, false);
        let caches = Some(&self.caches);
        let states = [
            ("signatures stored", serial(row, caches, true, false)),
            ("signature cache warm", serial(row, caches, true, false)),
            ("both stored", serial(row, caches, true, true)),
        ];
        for (state, (result, _)) in &states {
            if *result != base {
                return Err(format!("{state}: {result:?}, uncached {base:?}"));
            }
        }
        let (warm, reads) = serial(row, caches, true, true);
        if warm != base {
            return Err(format!("script-execution cache warm: {warm:?}, uncached {base:?}"));
        }
        if base.is_ok() && !row.tx.is_coinbase() {
            if reads != 0 {
                return Err(format!("a passing transaction missed the warm cache ({reads} reads)"));
            }
            self.exec_hits += 1;
        }
        let (block, _) = serial(row, caches, false, false);
        if block != base {
            return Err(format!("block connection: {block:?}, uncached {base:?}"));
        }
        for caches in [None, caches] {
            let queued = self.parallel(row, caches);
            let want = match base {
                Ok(()) => Ok(true),
                Err(CheckInputsError::Script(_)) => Ok(false),
                Err(e @ CheckInputsError::Flags(_)) => Err(e),
            };
            if queued != want {
                return Err(format!(
                    "parallel (caches {}): {queued:?}, uncached {base:?}",
                    caches.is_some()
                ));
            }
        }
        Ok(base)
    }
}

fn spent_scripts(case: &TxCase, tx: &Tx) -> Vec<Vec<u8>> {
    tx.inputs()
        .iter()
        .enumerate()
        .map(|(i, input)| {
            case.prevout_script(input.prevout.hash.as_bytes(), input.prevout.index)
                .unwrap_or_else(|| {
                    panic!("row {}: bad test (no prevout for input {i})", case.index)
                })
                .to_vec()
        })
        .collect()
}

fn tx_row(case: &TxCase, flags: u32) -> Row {
    let mut cursor = &case.tx[..];
    let tx = match decode_tx(&mut cursor) {
        Ok(tx) => tx.into_value(),
        Err(_) => panic!("row {}: the transaction does not decode", case.index),
    };
    Row {
        what: format!("row {}", case.index),
        spent: spent_scripts(case, &tx),
        tx,
        flags: ScriptFlags::from_bits(flags),
    }
}

fn report(file: &str, failures: &[String]) {
    assert!(failures.is_empty(), "{file}: {} rows:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn script_tests_json_with_and_without_caches() {
    let cases = load_script_tests(&read_core_text("src/test/data/script_tests.json")).unwrap();
    assert_eq!(cases.len(), 1282);
    let mut harness = Harness::new();
    let mut failures = Vec::new();
    for case in &cases {
        let credit = decode_tx_exact(&credit_tx(&case.script_pub_key)).unwrap().into_value();
        let spend = decode_tx_exact(&spend_tx(&case.script_sig, credit.txid().as_bytes()))
            .unwrap()
            .into_value();
        let row = Row {
            what: format!(
                "row {} [{}] [{}]",
                case.index, case.script_sig_text, case.script_pub_key_text
            ),
            tx: spend,
            spent: vec![case.script_pub_key.clone()],
            flags: ScriptFlags::from_bits(fill_flags(case.flags)),
        };
        let error = match harness.check(&row) {
            Err(why) => {
                failures.push(format!("{}: {why}", row.what));
                continue;
            }
            Ok(Ok(())) => "OK",
            Ok(Err(CheckInputsError::Script(f))) => f.error.name(),
            Ok(Err(e)) => panic!("{}: {e:?}", row.what),
        };
        if error != case.expected_error {
            failures.push(format!("{}: {error} (Core: {})", row.what, case.expected_error));
        }
    }
    report("script_tests.json", &failures);
    assert_eq!(harness.exec_resets, 0);
    assert!(harness.exec_hits > 400, "{} script-execution hits", harness.exec_hits);
}

/// `tx_valid.json`: every input passes with every flag but the row's excluded ones.
#[test]
fn tx_valid_json_with_and_without_caches() {
    let cases = load_tx_valid(&read_core_text("src/test/data/tx_valid.json")).unwrap();
    assert_eq!(cases.len(), 83);
    let mut harness = Harness::new();
    let mut failures = Vec::new();
    let mut coinbases = 0;
    for case in &cases {
        let TxFlags::Script(excluded) = case.flags else {
            panic!("tx_valid has no BADTX rows");
        };
        let row = tx_row(case, fill_flags(DEFINED_FLAGS & !excluded));
        coinbases += usize::from(row.tx.is_coinbase());
        match harness.check(&row) {
            Err(why) => failures.push(format!("{}: {why}", row.what)),
            Ok(Err(e)) => failures.push(format!("{}: {e:?}", row.what)),
            Ok(Ok(())) => {}
        }
    }
    report("tx_valid.json", &failures);
    // `CheckInputScripts` skips a coinbase, as Core's does: its rows never reach the cache.
    assert_eq!((harness.exec_hits, coinbases), (81, 2));
    assert!(harness.exec_resets > 0);
}

/// `tx_invalid.json`'s script rows: some input fails under the row's flags.
#[test]
fn tx_invalid_json_with_and_without_caches() {
    let cases = load_tx_invalid(&read_core_text("src/test/data/tx_invalid.json")).unwrap();
    let mut harness = Harness::new();
    let mut failures = Vec::new();
    let mut rows = 0;
    for case in &cases {
        let TxFlags::Script(flags) = case.flags else {
            continue;
        };
        rows += 1;
        let row = tx_row(case, fill_flags(flags));
        match harness.check(&row) {
            Err(why) => failures.push(format!("{}: {why}", row.what)),
            Ok(Ok(())) => failures.push(format!("{}: passes", row.what)),
            Ok(Err(_)) => {}
        }
    }
    assert_eq!(rows, 66);
    report("tx_invalid.json", &failures);
    assert!(harness.exec_resets > 0);
    assert_eq!(harness.exec_hits, 0);
}
