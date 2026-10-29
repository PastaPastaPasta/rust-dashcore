//! CS6's caches and check queue against Dash Core's `CheckInputScripts` (the oracle's, serial and
//! uncached): every row of `script_tests.json`, every script row of `tx_valid.json` /
//! `tx_invalid.json`, and signed P2PKH spends whose inputs fail in different ways, under the
//! standard and the block flags. In each cache state (none; signatures stored, then hit; both
//! stored, then the script-execution cache hit; block connection's lookups) CS6's serial result
//! is Core's reject reason and debug message, and the parallel queue's verdict is Core's.
//!
//! Output: on a difference, the row and both results; the inputs are Core's public vectors and
//! harness keys.

#[path = "../../dashd-chainstate/tests/support/signed.rs"]
mod signed;

use std::collections::HashMap;

use dash_consensus::Tx;
use dash_consensus::primitives::{core_bytes, decode_tx, decode_tx_exact};
use dash_consensus::script::{CheckInputsError, ScriptFlags, ScriptPolicy};
use dash_core_vectors::cases::{
    TxFlags, credit_tx, load_script_tests, load_tx_invalid, load_tx_valid, spend_tx,
};
use dash_core_vectors::flags::{DEFINED_FLAGS, fill_flags};
use dash_core_vectors::read_core_text;
use dash_oracle_sys::{ErrorKind, Oracle, SpentOutput, Verdict};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::checkqueue::script::{ScriptOptions, TxSlot, check_input_scripts};
use dashd_chainstate::sigcache::{CacheSizes, ScriptCaches, ScriptExecCache};
use signed::{Unlock, spend};

const SIZES: CacheSizes = CacheSizes {
    signature_cache_bytes: 1 << 20,
    script_execution_cache_bytes: 1 << 16,
};

/// The outputs a transaction spends: amount and script.
type Spent = Vec<(i64, Vec<u8>)>;

/// One transaction: its bytes as Core reads them, the outputs it spends and the flags.
struct Row {
    what: String,
    bytes: Vec<u8>,
    tx: Tx,
    spent: Spent,
    flags: u32,
}

/// Core's verdict as CS6 reports it: `None` for a pass, else the reason and debug message.
type Report = Option<(String, String)>;

struct Diff {
    oracle: Oracle,
    caches: ScriptCaches,
    queue: CheckQueue,
    seen: HashMap<([u8; 32], u32), Spent>,
    compared: usize,
    rejected: usize,
    refused: usize,
}

impl Diff {
    fn new(oracle: Oracle) -> Self {
        Self {
            oracle,
            caches: ScriptCaches::new(SIZES).unwrap(),
            queue: CheckQueue::new(2).unwrap(),
            seen: HashMap::new(),
            compared: 0,
            rejected: 0,
            refused: 0,
        }
    }

    fn opts(
        &self,
        flags: u32,
        cached: bool,
        sig_store: bool,
        full_store: bool,
    ) -> ScriptOptions<'_> {
        ScriptOptions {
            flags: ScriptFlags::from_bits(flags),
            policy: ScriptPolicy::V24_0,
            caches: cached.then_some(&self.caches),
            cache_sig_store: sig_store,
            cache_full_script_store: full_store,
        }
    }

    fn serial(&self, row: &Row, opts: &ScriptOptions<'_>) -> Report {
        let slot = TxSlot::new(&row.tx);
        match check_input_scripts(&slot, |n| row.spent[n].1.as_slice(), opts, None) {
            Ok(()) => None,
            Err(CheckInputsError::Script(f)) => {
                let e = f.into_tx_error();
                Some((e.reject_reason().to_owned(), e.debug_message().to_owned()))
            }
            Err(CheckInputsError::Flags(f)) => panic!("{}: {f}", row.what),
        }
    }

    fn parallel(&self, row: &Row, opts: &ScriptOptions<'_>) -> bool {
        let slot = TxSlot::new(&row.tx);
        let (result, ok) = self.queue.control(|control| {
            check_input_scripts(&slot, |n| row.spent[n].1.as_slice(), opts, Some(control))
        });
        result.is_ok() && ok
    }

    /// Compares `row` in every state; `Err` describes the first difference.
    fn compare(&mut self, row: &Row) -> Result<(), String> {
        let spent: Vec<SpentOutput<'_>> = row
            .spent
            .iter()
            .map(|(amount, script)| SpentOutput {
                amount: *amount,
                script_pubkey: script,
            })
            .collect();
        let core: Report = match self.oracle.check_input_scripts(&row.bytes, &spent, row.flags) {
            Ok(Verdict::Accept) => None,
            Ok(Verdict::Reject(r)) => {
                assert!(!r.reason_truncated && !r.debug_truncated, "{}", row.what);
                Some((r.reason, r.debug_message))
            }
            // The adapter refuses spent outputs no coins view can hold (CS1's contract).
            Err(e) if e.kind == ErrorKind::Argument => {
                self.refused += 1;
                return Ok(());
            }
            Err(e) => panic!("{}: {e}", row.what),
        };
        // The vectors reuse made-up outpoints: rows Core could never see together get another
        // script-execution cache.
        let key = (*row.tx.txid().as_bytes(), row.flags);
        if self.seen.get(&key).is_some_and(|s| *s != row.spent) {
            self.caches.exec = ScriptExecCache::new(SIZES.script_execution_cache_bytes).unwrap();
            self.seen.clear();
        }
        self.seen.insert(key, row.spent.clone());

        let states = [
            ("uncached", false, false, false),
            ("signatures stored", true, true, false),
            ("signature cache warm", true, false, false),
            ("both stored", true, true, true),
            ("script-execution cache warm", true, true, true),
            ("block connection", true, false, false),
        ];
        for (state, cached, sig, full) in states {
            let ours = self.serial(row, &self.opts(row.flags, cached, sig, full));
            if ours != core {
                return Err(format!("{state}: CS6 {ours:?}, Core {core:?}"));
            }
        }
        for cached in [false, true] {
            let ours = self.parallel(row, &self.opts(row.flags, cached, false, false));
            if ours != core.is_none() {
                return Err(format!("parallel (cached {cached}): CS6 {ours}, Core {core:?}"));
            }
        }
        self.compared += 1;
        self.rejected += usize::from(core.is_some());
        Ok(())
    }
}

fn check(diff: &mut Diff, rows: impl IntoIterator<Item = Row>) {
    let failures: Vec<String> = rows
        .into_iter()
        .filter_map(|row| diff.compare(&row).err().map(|why| format!("{}: {why}", row.what)))
        .collect();
    assert!(
        failures.is_empty(),
        "{} rows differ from Core:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn script_tests_json_matches_core_in_every_cache_state() {
    let cases = load_script_tests(&read_core_text("src/test/data/script_tests.json")).unwrap();
    for oracle in Oracle::all() {
        let mut diff = Diff::new(oracle);
        let rows = cases.iter().map(|case| {
            let credit = decode_tx_exact(&credit_tx(&case.script_pub_key)).unwrap().into_value();
            let bytes = spend_tx(&case.script_sig, credit.txid().as_bytes());
            Row {
                what: format!(
                    "row {} [{}] [{}]",
                    case.index, case.script_sig_text, case.script_pub_key_text
                ),
                tx: decode_tx_exact(&bytes).unwrap().into_value(),
                bytes,
                spent: vec![(0, case.script_pub_key.clone())],
                flags: fill_flags(case.flags),
            }
        });
        check(&mut diff, rows);
        // Four rows spend an output no coins view holds (OP_RETURN first, or too long).
        assert_eq!((diff.compared, diff.rejected, diff.refused), (1278, 538, 4));
    }
}

#[test]
fn tx_vectors_match_core_in_every_cache_state() {
    let valid = load_tx_valid(&read_core_text("src/test/data/tx_valid.json")).unwrap();
    let invalid = load_tx_invalid(&read_core_text("src/test/data/tx_invalid.json")).unwrap();
    for oracle in Oracle::all() {
        let mut diff = Diff::new(oracle);
        let rows = [(&valid, true), (&invalid, false)].into_iter().flat_map(|(cases, valid)| {
            cases.iter().filter_map(move |case| {
                let TxFlags::Script(flags) = case.flags else {
                    return None;
                };
                // tx_valid lists the flags it excludes.
                let flags = fill_flags(if valid {
                    DEFINED_FLAGS & !flags
                } else {
                    flags
                });
                let tx = decode_tx(&mut &case.tx[..]).unwrap().into_value();
                let spent = tx
                    .inputs()
                    .iter()
                    .map(|i| {
                        let script = case
                            .prevout_script(i.prevout.hash.as_bytes(), i.prevout.index)
                            .unwrap();
                        (0, script.to_vec())
                    })
                    .collect();
                Some(Row {
                    what: format!(
                        "{} row {}",
                        if valid {
                            "tx_valid"
                        } else {
                            "tx_invalid"
                        },
                        case.index
                    ),
                    bytes: case.tx.clone(),
                    tx,
                    spent,
                    flags,
                })
            })
        });
        check(&mut diff, rows);
        assert_eq!((diff.compared, diff.rejected, diff.refused), (149, 66, 0));
    }
}

/// Multi-input P2PKH spends with inputs failing different ways, under the standard flags (where
/// a failed signature is NULLFAIL first, so Core reports the first run's error after the
/// mandatory re-run) and the block flags.
#[test]
fn signed_spends_match_core_in_every_cache_state() {
    let bad_der = |pubkey: [u8; 33]| {
        let mut script_sig = vec![72, 0x30, 0x45];
        script_sig.extend([0x01; 70]);
        script_sig.push(33);
        script_sig.extend(pubkey);
        script_sig
    };
    let unlocks: Vec<Vec<Unlock>> = vec![
        vec![Unlock::Signed; 5],
        vec![Unlock::Signed, Unlock::WrongDigest, Unlock::Signed],
        vec![Unlock::WrongDigest, Unlock::Raw(bad_der(signed::pubkey(1)))],
        vec![Unlock::Signed, Unlock::Raw(bad_der(signed::pubkey(1))), Unlock::WrongDigest],
        vec![Unlock::Signed, Unlock::Raw(vec![0x51])],
        vec![Unlock::Signed, Unlock::Raw(Vec::new())],
        vec![Unlock::Signed; 130],
    ];
    for oracle in Oracle::all() {
        let mut diff = Diff::new(oracle);
        let flag_sets = [ScriptFlags::STANDARD.bits(), ScriptFlags::MANDATORY_V24_0.bits()];
        let rows = unlocks.iter().enumerate().flat_map(|(i, unlocks)| {
            let s = spend(unlocks, u32::try_from(i).unwrap());
            let bytes = core_bytes(s.tx.raw()).as_slice().to_vec();
            flag_sets.map(|flags| Row {
                what: format!("spend {i} under {flags:#x}"),
                bytes: bytes.clone(),
                tx: decode_tx_exact(&bytes).unwrap().into_value(),
                spent: s.spent.iter().map(|script| (10_000, script.clone())).collect(),
                flags,
            })
        });
        check(&mut diff, rows);
        assert_eq!((diff.compared, diff.rejected), (14, 10));
    }
}
