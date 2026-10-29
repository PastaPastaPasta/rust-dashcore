//! L3 for the check queue (CS6, FR-CHS-10, F20; CH3a a9, DEC-323): blocks with several invalid
//! transactions that Dash Core v24 judged at five `-par` values, with 0, 1, 15 and (on the
//! recording host) 7 script-check threads (`tests/data/cs6_checkqueue_v24.json`, see
//! `tests/data/PROVENANCE.md`), replayed through `ConnectBlock`'s transaction loop
//! (`validation.cpp:2687-2781`) built from CS5b's `check_tx_inputs` and CS6's
//! `check_input_scripts`, its check queue and its reports. Each mode must report what Core did:
//! serially the first failing input of the first failing transaction, in parallel
//! `block-validation-failed` unless a later transaction fails `CheckTxInputs` first. So under
//! every `-par` the verdicts are the same, and the reasons are the same under every `-par` with
//! threads and under every `-par` without.
//!
//! As on each node, each case runs first as `TestBlockValidity` (storing in both caches), then
//! as block connection (consulting them), over caches kept from case to case, and a case's
//! mempool transactions go through the mempool's two script checks first.

use std::collections::HashMap;

use dash_consensus::Tx;
use dash_consensus::coins::{Coin, CoinWrite, MemoryCoins, ViewOr};
use dash_consensus::primitives::{Block, OutPoint, decode_block, decode_tx_exact};
use dash_consensus::script::{
    BuriedScriptRules, CheckInputsError, ScriptFlags, ScriptPolicy, block_script_flags,
};
use dash_consensus::tx::check_tx_inputs;
use dash_consensus::validation::{BlockValidationError, BlockValidationResult};
use dash_core_vectors::hex::parse_hex;
use dash_core_vectors::univalue::UniValue;
use dashd_chainstate::checkqueue::script::{
    ScriptOptions, TxSlot, check_input_scripts, check_queue_failed, input_scripts_failed,
};
use dashd_chainstate::checkqueue::{CheckQueue, script_check_threads};
use dashd_chainstate::sigcache::{CacheSizes, ScriptCaches};

fn fixture() -> UniValue {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/cs6_checkqueue_v24.json");
    UniValue::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn str_of<'a>(v: &'a UniValue, key: &str) -> &'a str {
    v.find_value(key).get_str().unwrap_or_else(|e| panic!("{key}: {e}"))
}

fn tx_of(hex: &str) -> Tx {
    decode_tx_exact(&parse_hex(hex)).unwrap().into_value()
}

/// `UpdateCoins`: spends the inputs and adds the outputs at `height`.
fn update_coins(view: &mut MemoryCoins, tx: &Tx, height: u32) {
    if !tx.is_coinbase() {
        for input in tx.inputs() {
            view.write(input.prevout, CoinWrite::Erase);
        }
    }
    for (n, out) in tx.outputs().iter().enumerate() {
        let outpoint = OutPoint {
            hash: tx.txid(),
            index: u32::try_from(n).unwrap(),
        };
        view.write(outpoint, CoinWrite::Put(Coin::from_output(out, height, tx.is_coinbase())));
    }
}

/// The scripts of every output of `txs`: what a case can spend.
fn scripts<'a>(txs: impl Iterator<Item = &'a Tx>) -> HashMap<OutPoint, &'a [u8]> {
    txs.flat_map(|tx| {
        tx.outputs().iter().enumerate().map(move |(n, out)| {
            let outpoint = OutPoint {
                hash: tx.txid(),
                index: u32::try_from(n).unwrap(),
            };
            (outpoint, out.script_pubkey.as_bytes())
        })
    })
    .collect()
}

// Core: validation.cpp:2687-2781 ConnectBlock
/// `ConnectBlock`'s transaction loop and its queue's verdict, as far as inputs and scripts go
/// (the cases have no special transactions, BIP68 locks or sigop excess).
fn connect_block(
    block: &Block,
    funding: &Tx,
    view: &MemoryCoins,
    height: u32,
    queue: &CheckQueue,
    opts: &ScriptOptions<'_>,
) -> Result<(), BlockValidationError> {
    let mut view = view.clone();
    let spent = scripts(std::iter::once(funding).chain(block.txs().iter().map(|tx| &**tx)));
    let slots: Vec<TxSlot<'_>> = block.txs().iter().map(|tx| TxSlot::new(tx)).collect();
    let (result, all_ok) = queue.control(|control| {
        for slot in &slots {
            let tx = slot.tx();
            if !tx.is_coinbase() {
                match check_tx_inputs(tx, &view, height) {
                    Ok(_) => {}
                    Err(ViewOr::Reject(e)) => {
                        return Err(e.into_block(BlockValidationResult::Consensus));
                    }
                    Err(ViewOr::View(e)) => match e {},
                }
                let spent_script = |n: usize| spent[&tx.inputs()[n].prevout];
                check_input_scripts(slot, spent_script, opts, Some(control)).map_err(
                    |e| match e {
                        CheckInputsError::Script(f) => input_scripts_failed(f),
                        CheckInputsError::Flags(f) => panic!("{f}"),
                    },
                )?;
            }
            update_coins(&mut view, tx, height);
        }
        Ok(())
    });
    result?;
    if !all_ok {
        return Err(check_queue_failed());
    }
    Ok(())
}

/// The mempool's script checks of a transaction it accepts (`validation.cpp:1196-1231`):
/// `PolicyScriptChecks` under the standard flags, storing signatures, then
/// `ConsensusScriptChecks` under the tip's block flags, storing both.
fn accept_to_mempool(tx: &Tx, funding: &Tx, caches: &ScriptCaches, flags: ScriptFlags) {
    let funded = scripts(std::iter::once(funding));
    for (flags, full) in [(ScriptFlags::STANDARD, false), (flags, true)] {
        let opts = ScriptOptions {
            flags,
            policy: ScriptPolicy::V24_0,
            caches: Some(caches),
            cache_sig_store: true,
            cache_full_script_store: full,
        };
        let slot = TxSlot::new(tx);
        let spent = |n: usize| funded[&tx.inputs()[n].prevout];
        check_input_scripts(&slot, spent, &opts, None).unwrap();
    }
}

/// What a node reported: `TestBlockValidity`'s and `submitblock`'s reasons (`None`: valid), and
/// the state `ConnectTip` logged, Core's `ValidationState::ToString`.
#[derive(Debug, PartialEq)]
struct Verdict {
    test: Option<String>,
    connect: Option<String>,
    logged: Option<String>,
}

fn verdict(v: &UniValue) -> Verdict {
    let test = v.find_value("test_block_validity");
    let connect = v.find_value("submitblock");
    let logged = v
        .find_value("submitblock_log")
        .get_array()
        .unwrap()
        .iter()
        .filter_map(|l| l.get_str().unwrap().split_once(" failed, "))
        .find(|(head, _)| head.contains("ConnectTip: ConnectBlock "))
        .map(|(_, state)| state.to_owned());
    Verdict {
        test: (!test.is_null()).then(|| {
            let message = test.get_str().unwrap();
            message.strip_prefix("TestBlockValidity failed: ").unwrap().to_owned()
        }),
        connect: (!connect.is_null()).then(|| connect.get_str().unwrap().to_owned()),
        logged,
    }
}

#[test]
fn multi_invalid_blocks_report_cores_result_in_every_mode() {
    let doc = fixture();
    assert_eq!(str_of(&doc, "core"), "/Dash Core:24.0.0/");
    // Every buried script rule is active on regtest from height 1 at most.
    let deployments = doc.find_value("deployments");
    for name in ["bip65", "bip66", "csv", "bip147"] {
        let d = deployments.find_value(name);
        assert!(
            d.find_value("active").get_bool().unwrap()
                && d.find_value("height").get_i64().unwrap() <= 1
        );
    }
    let flags = block_script_flags(BuriedScriptRules {
        dersig: true,
        cltv: true,
        csv: true,
        bip147: true,
    });
    let funding_doc = doc.find_value("funding");
    let funding = tx_of(str_of(funding_doc, "tx"));
    let funding_height =
        u32::try_from(funding_doc.find_value("height").get_i64().unwrap()).unwrap();
    let mut view = MemoryCoins::new();
    update_coins(&mut view, &funding, funding_height);

    let modes = doc.find_value("modes").get_obj().unwrap();
    assert_eq!(modes.len(), 5);
    let cases = doc.find_value("cases").get_array().unwrap();
    assert_eq!(cases.len(), 10);
    // Core's threads on the recording host, from each node's log line, and its online processors.
    let num_cores = usize::try_from(doc.find_value("num_cores").get_i64().unwrap()).unwrap();
    let recorded = doc.find_value("threads");
    let mut runs = Vec::new();
    for (mode, par) in modes {
        let par = par.get_i64().unwrap();
        let threads = script_check_threads(par, num_cores);
        assert_eq!(i64::try_from(threads).ok(), recorded.find_value(mode).get_i64().ok(), "{mode}");
        let queue = CheckQueue::new(threads).unwrap();
        assert_eq!(queue.threads(), threads, "{mode}");
        let caches = ScriptCaches::new(CacheSizes::default()).unwrap();
        let mut reports = Vec::new();
        for case in cases {
            let name = str_of(case, "name");
            let height = u32::try_from(case.find_value("height").get_i64().unwrap()).unwrap();
            let raw = parse_hex(str_of(case, "block"));
            let block = decode_block(&mut &raw[..]).unwrap().into_value();
            for tx in case.find_value("mempool").get_array().unwrap() {
                accept_to_mempool(&tx_of(tx.get_str().unwrap()), &funding, &caches, flags);
            }
            let opts = |store| ScriptOptions {
                flags,
                policy: ScriptPolicy::V24_0,
                caches: Some(&caches),
                cache_sig_store: store,
                cache_full_script_store: store,
            };
            let what = format!("{mode}, {name}");
            // TestBlockValidity: fJustCheck, so both caches store.
            let tested = connect_block(&block, &funding, &view, height, &queue, &opts(true));
            // submitblock: block connection consults the caches without storing.
            let connected = connect_block(&block, &funding, &view, height, &queue, &opts(false));
            if let Err(e) = &connected {
                assert_eq!(e.result(), BlockValidationResult::Consensus, "{what}");
            }
            let uncached = ScriptOptions {
                caches: None,
                ..opts(false)
            };
            let without = connect_block(&block, &funding, &view, height, &queue, &uncached);
            assert_eq!(without, connected, "{what}: without caches");
            let reason = |r: &Result<_, BlockValidationError>| {
                r.as_ref().err().map(|e| e.reject_reason().to_owned())
            };
            let ours = Verdict {
                test: reason(&tested),
                connect: reason(&connected),
                logged: connected.as_ref().err().map(ToString::to_string),
            };
            // [Core's, ours].
            reports.push([verdict(case.find_value("verdicts").find_value(mode)), ours]);
        }
        runs.push((mode.as_str(), queue.has_threads(), reports));
    }

    // DEC-323 as DEC-450 reads it, on Core's reports and then on ours: (a) every mode accepts or
    // rejects each block alike; (b) the modes with threads report alike, and so do the modes
    // without. Then (c): ours are Core's, mode by mode.
    let [serial, threaded] = [false, true]
        .map(|kind| (0..runs.len()).filter(|&r| runs[r].1 == kind).collect::<Vec<usize>>());
    assert!(threaded.len() >= 2, "the modes with threads: {threaded:?}");
    assert_eq!(serial.len(), 2, "the modes without threads: {serial:?}");
    let valid = |v: &Verdict| (v.test.is_none(), v.connect.is_none());
    let base = runs[0].0;
    for (who, side) in [("Core", 0), ("ours", 1)] {
        for (i, case) in cases.iter().enumerate() {
            let name = str_of(case, "name");
            // Run `r`'s report of this case: Core's or ours.
            let report = |r: usize| &runs[r].2[i][side];
            let tip = |mode: &str| str_of(case.find_value("verdicts").find_value(mode), "tip");
            for (r, (mode, _, _)) in runs.iter().enumerate() {
                assert_eq!(
                    valid(report(r)),
                    valid(report(0)),
                    "(a) {who}: {mode} and {base}, {name}"
                );
                if side == 0 {
                    assert_eq!(tip(mode), tip(base), "(a) Core's tip: {mode} and {base}, {name}");
                }
            }
            for same in [&serial, &threaded] {
                let (first, first_mode) = (same[0], runs[same[0]].0);
                for &r in &same[1..] {
                    let mode = runs[r].0;
                    assert_eq!(
                        report(r),
                        report(first),
                        "(b) {who}: {mode} and {first_mode}, {name}"
                    );
                }
            }
        }
    }
    for (mode, _, reports) in &runs {
        for (case, [core, ours]) in cases.iter().zip(reports) {
            assert_eq!(ours, core, "(c) {mode}, {}", str_of(case, "name"));
        }
    }
    // The two kinds of mode disagree wherever a script fails before any later CheckTxInputs
    // failure.
    let of = |case: &UniValue, mode: &str| verdict(case.find_value("verdicts").find_value(mode));
    let differ = cases.iter().filter(|c| of(c, "par1").connect != of(c, "par2").connect).count();
    assert_eq!(differ, 8);
}
