//! The kill smoke's `--indexes` check (IX1-h). After C2–C4 pass, and again at the lap's final
//! tip, rdashd's txindex, basic block filter and coinstats index are compared with dashd.
//!
//! The lap's P2P peer holds only the fixture's served prefix. The reference is `serve-final`,
//! the dashd that holds the whole fixture chain, started with the same three flags. Its index
//! RPCs run only after recovery has passed; it is not read before a kill (plan rule m5). A
//! height is not a block: the fixture reorgs, so both chains are asked by hash, and only for a
//! block on rdashd's recovered active chain that is also on the reference's active chain.
//!
//! `getrawtransaction`'s `confirmations` counts from the answering node's own tip. When the
//! recovered tip and the reference tip differ, that field is left out of the comparison.

use std::collections::{BTreeSet, HashSet};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::baseline::Peer;
use super::checks::RPC_PATIENCE;
use super::fixture::Fixture;
use super::logwrites::Rng;
use super::vm::active_chain;
use crate::NodeRpc;

/// `-txindex`, `-blockfilterindex` and `-coinstatsindex`, on rdashd and the reference dashd.
pub const INDEX_ARGS: [&str; 3] = ["-txindex=1", "-blockfilterindex=1", "-coinstatsindex=1"];

/// `getindexinfo`'s names for those three, in Core's order.
const INDEX_NAMES: [&str; 3] = ["txindex", "coinstatsindex", "basic block filter index"];

/// How many ancestors of the highest comparable block an index check always compares, and how
/// many earlier active-chain blocks it samples besides them.
pub const INDEX_ANCESTORS: usize = 4;
pub const INDEX_SAMPLE: usize = 4;

/// The only RPC error both nodes may share: `getrawtransaction` on the genesis coinbase.
// Core: v24.0.2 src/rpc/rawtransaction.cpp:416-419
const GENESIS_COINBASE_ERROR: &str =
    "The genesis block coinbase is not considered an ordinary transaction and cannot be retrieved";

/// How often the sync wait polls.
const POLL: Duration = Duration::from_millis(200);

/// `tx` as compared. `confirmations` is removed when `tips_differ`: it is not a property of the
/// block, and the reference's tip is the fixture's.
pub fn comparable_tx(mut tx: Value, tips_differ: bool) -> Value {
    if tips_differ && let Some(obj) = tx.as_object_mut() {
        obj.remove("confirmations");
    }
    tx
}

/// The highest block of `active` (tip first) that `on_reference` holds, up to [`INDEX_ANCESTORS`]
/// of its ancestors, then up to [`INDEX_SAMPLE`] earlier ones drawn with `seed`. Empty when the
/// chains share nothing.
pub fn index_blocks(active: &[String], on_reference: &HashSet<String>, seed: u64) -> Vec<String> {
    let mut shared: Vec<String> =
        active.iter().filter(|h| on_reference.contains(h.as_str())).cloned().collect();
    if shared.is_empty() {
        return shared;
    }
    let keep = (1 + INDEX_ANCESTORS).min(shared.len());
    let mut picked: Vec<String> = shared.drain(..keep).collect();
    let mut rng = Rng::new(seed);
    for _ in 0..INDEX_SAMPLE.min(shared.len()) {
        let i = rng.below(shared.len() as u64) as usize;
        picked.push(shared.swap_remove(i));
    }
    picked
}

/// What one poll of the index wait decided. `tip` is this poll's `getblockcount`; `floor` is the
/// height the check has to have reached (C2's, or the reference chain's tip).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexWait {
    /// All three indexes are synced at `tip`, and `tip` is at least `floor`.
    Ready,
    /// Still behind the tip, or the tip has not reached `floor` yet.
    Poll,
    /// At least one index is above the chain's current tip.
    Past {
        name: &'static str,
        at: u64,
        tip: u64,
    },
}

/// The index wait's decision for one poll. A tip that is still moving is read again on the next
/// poll: synced at the count from an earlier poll is not "past" once that count is the tip. Any
/// index above `tip` fails at once, whether or not the other indexes agree.
pub fn index_wait(info: &Value, tip: u64, floor: u64) -> IndexWait {
    if let Some(obj) = info.as_object() {
        for name in INDEX_NAMES {
            if let Some(at) =
                obj.get(name).and_then(|e| e.get("best_block_height")).and_then(Value::as_u64)
                && at > tip
            {
                return IndexWait::Past {
                    name,
                    at,
                    tip,
                };
            }
        }
    }
    match index_height(info) {
        Ok(at) if at == tip && at >= floor => IndexWait::Ready,
        _ => IndexWait::Poll,
    }
}

/// What one index check compared, and any mismatches it found.
#[derive(Debug, Default)]
pub struct CheckReport {
    /// Index RPC answers where both nodes returned `Ok` and were compared.
    pub compared: usize,
    pub failures: Vec<String>,
}

/// The index check at recovered `tip`, against `peer` whose chain tip is `peer_tip`.
pub fn check(
    rdashd: &dyn NodeRpc,
    peer: &Peer,
    fixture: &Fixture,
    tip: &str,
    peer_tip: &str,
    seed: u64,
) -> CheckReport {
    let mut report = CheckReport::default();
    let Some(height) = fixture.rows.get(tip).map(|r| r.height) else {
        report.failures.push(format!("CX: {tip} is no fixture block"));
        return report;
    };
    let Some(peer_height) = fixture.rows.get(peer_tip).map(|r| r.height) else {
        report.failures.push(format!("CX: {peer_tip} is no fixture block"));
        return report;
    };
    let mut rd =
        |method: &str, params: &[Value]| rdashd.call(method, params).map_err(|e| e.to_string());
    let mut dashd = |method: &str, params: &[Value]| {
        peer.call_for(method, params, RPC_PATIENCE).map_err(|e| e.to_string())
    };
    // The reference is read only after rdashd's own indexes have caught up. The recovered tip
    // can still move after C2, so a height at or above C2's is enough, and "past" is only above
    // the tip this poll just read.
    let ours = match wait_for_indexes(&mut rd, height) {
        Ok(info) => info,
        Err(e) => {
            report.failures.push(format!("CX getindexinfo rdashd: {e}"));
            return report;
        }
    };
    let theirs = match wait_for_indexes(&mut dashd, peer_height) {
        Ok(info) => info,
        Err(e) => {
            report.failures.push(format!("CX getindexinfo peer: {e}"));
            return report;
        }
    };
    let tips_differ = tip != peer_tip;
    report.compared += 1;
    if tips_differ {
        // Each index is synced at its own chain's tip. The heights are not the same number.
        same(
            "CX getindexinfo",
            &without_heights(&ours),
            &without_heights(&theirs),
            &mut report.failures,
        );
    } else {
        same("CX getindexinfo", &ours, &theirs, &mut report.failures);
    }
    let on_reference: HashSet<String> = active_chain(fixture, peer_tip).into_iter().collect();
    let blocks = index_blocks(&active_chain(fixture, tip), &on_reference, seed);
    if blocks.is_empty() {
        report.failures.push(format!("CX: no block of {tip} is on the reference chain"));
    }
    for hash in &blocks {
        let is_genesis = fixture.rows.get(hash).is_some_and(|r| r.height == 0);
        compare_block(
            &mut rd,
            &mut dashd,
            hash,
            is_genesis,
            tips_differ,
            &mut report.compared,
            &mut report.failures,
        );
    }
    report
}

/// All three indexes synced at one height, or why not.
fn index_height(info: &Value) -> Result<u64, String> {
    let Some(obj) = info.as_object() else {
        return Err(format!("getindexinfo is {info}"));
    };
    let mut height = None;
    for name in INDEX_NAMES {
        let Some(entry) = obj.get(name) else {
            return Err(format!("no {name} in {info}"));
        };
        if entry.get("synced") != Some(&Value::Bool(true)) {
            return Err(format!("{name} is not synced in {info}"));
        }
        let Some(h) = entry.get("best_block_height").and_then(Value::as_u64) else {
            return Err(format!("{name} has no height in {info}"));
        };
        if let Some(prev) = height
            && prev != h
        {
            return Err(format!("{name} is at {h}, not {prev}"));
        }
        height = Some(h);
    }
    if obj.len() != INDEX_NAMES.len() {
        return Err(format!("getindexinfo has {} entries: {info}", obj.len()));
    }
    height.ok_or_else(|| format!("no indexes in {info}"))
}

fn without_heights(info: &Value) -> Value {
    let mut info = info.clone();
    if let Some(obj) = info.as_object_mut() {
        for entry in obj.values_mut() {
            if let Some(entry) = entry.as_object_mut() {
                entry.remove("best_block_height");
            }
        }
    }
    info
}

/// Poll until the indexes are synced at the current `getblockcount` and that count is at least
/// `floor`. Each poll reads `getindexinfo` first, then `getblockcount`, so a block connected and
/// indexed between the two reads never looks past the tip. Any index height above that count fails
/// at once.
fn wait_for_indexes(
    call: &mut impl FnMut(&str, &[Value]) -> Result<Value, String>,
    floor: u64,
) -> Result<Value, String> {
    let deadline = Instant::now() + RPC_PATIENCE;
    loop {
        let last = match call("getindexinfo", &[]) {
            Ok(info) => match call("getblockcount", &[]) {
                Ok(count) => match count.as_u64() {
                    Some(tip) => match index_wait(&info, tip, floor) {
                        IndexWait::Ready => return Ok(info),
                        IndexWait::Past {
                            name,
                            at,
                            tip,
                        } => return Err(format!("{name} is at {at}, past tip {tip}: {info}")),
                        IndexWait::Poll => format!("tip {tip}: {info}"),
                    },
                    None => format!("getblockcount {count}"),
                },
                Err(e) => e,
            },
            Err(e) => e,
        };
        if Instant::now() >= deadline {
            return Err(format!(
                "indexes not synced at or above {floor} within {} s: {last}",
                RPC_PATIENCE.as_secs()
            ));
        }
        std::thread::sleep(POLL);
    }
}

fn compare_block(
    rdashd: &mut impl FnMut(&str, &[Value]) -> Result<Value, String>,
    peer: &mut impl FnMut(&str, &[Value]) -> Result<Value, String>,
    hash: &str,
    is_genesis: bool,
    tips_differ: bool,
    compared: &mut usize,
    failures: &mut Vec<String>,
) {
    let filter = |v: Value| json!({"filter": v["filter"], "header": v["header"]});
    call_mismatch(
        &format!("CX getblockfilter {hash}"),
        &rdashd("getblockfilter", &[json!(hash)]).map(filter),
        &peer("getblockfilter", &[json!(hash)]).map(filter),
        None,
        compared,
        failures,
    );
    let stats = [json!("muhash"), json!(hash)];
    call_mismatch(
        &format!("CX gettxoutsetinfo {hash}"),
        &rdashd("gettxoutsetinfo", &stats),
        &peer("gettxoutsetinfo", &stats),
        None,
        compared,
        failures,
    );
    let block = match rdashd("getblock", &[json!(hash), json!(1)]) {
        Ok(block) => block,
        Err(e) => {
            failures.push(format!("CX getblock {hash}: {e}"));
            return;
        }
    };
    let Some(txs) = block["tx"].as_array() else {
        failures.push(format!("CX getblock {hash}: no transactions"));
        return;
    };
    for (i, tx) in txs.iter().enumerate() {
        let Some(txid) = tx.as_str() else {
            failures.push(format!("CX getblock {hash}: tx entry {tx}"));
            continue;
        };
        let allow = (is_genesis && i == 0).then_some(GENESIS_COINBASE_ERROR);
        // With the block hash, and without it so the txindex answers.
        for (form, params) in [
            ("", vec![json!(txid), json!(true)]),
            (" in block", vec![json!(txid), json!(true), json!(hash)]),
        ] {
            let ours = rdashd("getrawtransaction", &params).map(|v| comparable_tx(v, tips_differ));
            let theirs = peer("getrawtransaction", &params).map(|v| comparable_tx(v, tips_differ));
            call_mismatch(
                &format!("CX getrawtransaction {txid}{form}"),
                &ours,
                &theirs,
                allow,
                compared,
                failures,
            );
        }
    }
}

fn call_mismatch(
    what: &str,
    ours: &Result<Value, String>,
    theirs: &Result<Value, String>,
    allow_error: Option<&str>,
    compared: &mut usize,
    failures: &mut Vec<String>,
) {
    match (ours, theirs) {
        (Ok(a), Ok(b)) => {
            *compared += 1;
            same(what, a, b, failures);
        }
        (Err(a), Err(b)) if allow_error.is_some_and(|e| a == b && a.contains(e)) => {}
        (Err(a), Err(b)) => failures.push(format!("{what}: rdashd error {a}; peer error {b}")),
        (Ok(a), Err(b)) => failures.push(format!("{what}: rdashd {a}; peer error {b}")),
        (Err(a), Ok(b)) => failures.push(format!("{what}: rdashd error {a}; peer {b}")),
    }
}

fn same(what: &str, ours: &Value, theirs: &Value, failures: &mut Vec<String>) {
    if ours == theirs {
        return;
    }
    let Some((a, b)) = ours.as_object().zip(theirs.as_object()) else {
        failures.push(format!("{what}: rdashd {ours} peer {theirs}"));
        return;
    };
    let keys: BTreeSet<_> = a.keys().chain(b.keys()).collect();
    let missing = Value::Null;
    let mut parts = Vec::new();
    for key in keys {
        if a.get(key) != b.get(key) {
            parts.push(format!(
                "{key}: rdashd {} peer {}",
                a.get(key).unwrap_or(&missing),
                b.get(key).unwrap_or(&missing)
            ));
        }
    }
    if parts.is_empty() {
        failures.push(format!("{what}: rdashd {ours} peer {theirs}"));
    } else {
        failures.push(format!("{what}: {}", parts.join("; ")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(n: usize) -> Vec<String> {
        (0..n).rev().map(|i| format!("b{i:02}")).collect()
    }

    fn info_at(tx: (bool, u64), cs: (bool, u64), bf: (bool, u64)) -> Value {
        json!({
            "txindex": {"synced": tx.0, "best_block_height": tx.1},
            "coinstatsindex": {"synced": cs.0, "best_block_height": cs.1},
            "basic block filter index": {"synced": bf.0, "best_block_height": bf.1},
        })
    }

    fn synced_at(h: u64) -> Value {
        info_at((true, h), (true, h), (true, h))
    }

    #[test]
    fn the_sample_is_the_highest_shared_block_its_ancestors_and_a_seeded_few_earlier() {
        let active = chain(20);
        let on: HashSet<String> = (0..=15).map(|i| format!("b{i:02}")).collect();
        let picked = index_blocks(&active, &on, 5);
        assert_eq!(picked, index_blocks(&active, &on, 5));
        assert_eq!(picked[..=INDEX_ANCESTORS], ["b15", "b14", "b13", "b12", "b11"]);
        assert_eq!(picked.len(), 1 + INDEX_ANCESTORS + INDEX_SAMPLE);
        assert!(
            picked[1 + INDEX_ANCESTORS..]
                .iter()
                .all(|h| on.contains(h) && !picked[..=INDEX_ANCESTORS].contains(h))
        );
        assert_ne!(picked, index_blocks(&active, &on, 9));
        // The recovered tip and its ancestors, when the reference still has them.
        let all: HashSet<String> = active.iter().cloned().collect();
        assert_eq!(
            index_blocks(&active, &all, 1)[..=INDEX_ANCESTORS],
            ["b19", "b18", "b17", "b16", "b15"]
        );
        // Fewer earlier blocks than the ancestor window + sample.
        let short = chain(3);
        let on_short: HashSet<String> = short.iter().cloned().collect();
        assert_eq!(index_blocks(&short, &on_short, 1), ["b02", "b01", "b00"]);
        let mid = chain(7);
        let on_mid: HashSet<String> = mid.iter().cloned().collect();
        assert_eq!(index_blocks(&mid, &on_mid, 1).len(), 7);
        assert!(index_blocks(&active, &HashSet::new(), 1).is_empty());
    }

    #[test]
    fn confirmations_are_compared_only_when_the_tips_are_the_same() {
        let tx = json!({"txid": "aa", "confirmations": 4, "hex": "bb", "vin": [{"txid": "cc"}]});
        assert_eq!(comparable_tx(tx.clone(), false), tx);
        assert_eq!(
            comparable_tx(tx, true),
            json!({"txid": "aa", "hex": "bb", "vin": [{"txid": "cc"}]})
        );
        assert_eq!(comparable_tx(json!("raw"), true), json!("raw"));
    }

    #[test]
    fn index_height_requires_the_three_indexes_synced_at_one_height() {
        assert_eq!(index_height(&synced_at(42)), Ok(42));
        assert!(index_height(&json!("not an object")).is_err());
        assert!(
            index_height(&json!({
                "txindex": {"synced": true, "best_block_height": 10},
                "coinstatsindex": {"synced": true, "best_block_height": 10},
            }))
            .unwrap_err()
            .contains("basic block filter index")
        );
        assert!(
            index_height(&info_at((true, 10), (false, 10), (true, 10)))
                .unwrap_err()
                .contains("not synced")
        );
        assert!(
            index_height(&json!({
                "txindex": {"synced": true},
                "coinstatsindex": {"synced": true, "best_block_height": 10},
                "basic block filter index": {"synced": true, "best_block_height": 10},
            }))
            .unwrap_err()
            .contains("has no height")
        );
        assert!(
            index_height(&info_at((true, 10), (true, 11), (true, 10)))
                .unwrap_err()
                .contains("is at 11, not 10")
        );
        let mut extra = synced_at(10);
        extra["other"] = json!({"synced": true, "best_block_height": 10});
        assert!(index_height(&extra).unwrap_err().contains("4 entries"));
    }

    /// The wait, one poll at a time: `Ok(tip)` when it would return, `Err(name)` when an index
    /// is past that poll's tip, `Err("polling")` if the sequence ends still short.
    fn settle(polls: &[(Value, u64)], floor: u64) -> Result<u64, &'static str> {
        for (info, tip) in polls {
            match index_wait(info, *tip, floor) {
                IndexWait::Ready => return Ok(*tip),
                IndexWait::Past {
                    name,
                    ..
                } => return Err(name),
                IndexWait::Poll => {}
            }
        }
        Err("polling")
    }

    #[test]
    fn the_wait_follows_the_tip_and_fails_at_once_when_any_index_is_past() {
        // Synced at the current count passes, including once the tip is above C2's height.
        assert_eq!(index_wait(&synced_at(10), 10, 10), IndexWait::Ready);
        assert_eq!(index_wait(&synced_at(115), 115, 114), IndexWait::Ready);
        // Behind, not synced yet, or tip below C2's floor keeps polling.
        assert_eq!(index_wait(&synced_at(9), 10, 10), IndexWait::Poll);
        assert_eq!(
            index_wait(&info_at((false, 10), (true, 10), (true, 10)), 10, 10),
            IndexWait::Poll
        );
        assert_eq!(index_wait(&synced_at(114), 114, 115), IndexWait::Poll);
        // All three above tip, or any single index above tip while the others are at or behind it.
        assert_eq!(
            index_wait(&synced_at(11), 10, 10),
            IndexWait::Past {
                name: "txindex",
                at: 11,
                tip: 10
            }
        );
        assert_eq!(
            index_wait(&info_at((true, 11), (true, 10), (true, 10)), 10, 10),
            IndexWait::Past {
                name: "txindex",
                at: 11,
                tip: 10
            }
        );
        assert_eq!(
            index_wait(&info_at((true, 10), (true, 11), (true, 10)), 10, 10),
            IndexWait::Past {
                name: "coinstatsindex",
                at: 11,
                tip: 10
            }
        );
        assert_eq!(
            index_wait(&info_at((false, 9), (true, 10), (false, 12)), 10, 10),
            IndexWait::Past {
                name: "basic block filter index",
                at: 12,
                tip: 10
            }
        );
        // The tip moves after a poll that had not caught up, and the next poll's count is the
        // one the indexes have to meet.
        assert_eq!(
            settle(&[(synced_at(100), 114), (synced_at(114), 115), (synced_at(115), 115)], 114),
            Ok(115)
        );
        assert_eq!(
            settle(
                &[(info_at((false, 114), (true, 114), (true, 114)), 114), (synced_at(115), 115)],
                114
            ),
            Ok(115)
        );
        assert_eq!(settle(&[(synced_at(9), 10), (synced_at(9), 10)], 10), Err("polling"));
    }

    #[test]
    fn wait_for_indexes_reads_getindexinfo_before_getblockcount_and_fails_on_one_ahead() {
        // One index ahead fails immediately on the first poll, and getindexinfo came first.
        let mut seen = Vec::new();
        let mut ahead = |method: &str, _: &[Value]| {
            seen.push(method.to_owned());
            match method {
                "getindexinfo" => Ok(info_at((true, 10), (true, 11), (true, 10))),
                "getblockcount" => Ok(json!(10)),
                other => panic!("unexpected {other}"),
            }
        };
        let err = wait_for_indexes(&mut ahead, 10).unwrap_err();
        assert_eq!(seen, ["getindexinfo", "getblockcount"]);
        assert!(err.contains("coinstatsindex is at 11, past tip 10"), "{err}");

        // A block connected between getindexinfo (114) and getblockcount (115) polls again and
        // succeeds once both read 115.
        let mut seen = Vec::new();
        let mut poll = 0;
        let mut advancing = |method: &str, _: &[Value]| {
            seen.push(method.to_owned());
            match method {
                "getindexinfo" => Ok(synced_at(if poll == 0 {
                    114
                } else {
                    115
                })),
                "getblockcount" => {
                    poll += 1;
                    Ok(json!(115))
                }
                other => panic!("unexpected {other}"),
            }
        };
        let info = wait_for_indexes(&mut advancing, 114).unwrap();
        assert_eq!(info, synced_at(115));
        assert_eq!(seen, ["getindexinfo", "getblockcount", "getindexinfo", "getblockcount"]);
    }

    #[test]
    fn call_mismatch_counts_ok_comparisons_and_only_allows_the_genesis_coinbase_error() {
        let mut compared = 0;
        let mut failures = Vec::new();
        call_mismatch(
            "ok same",
            &Ok(json!({"a": 1})),
            &Ok(json!({"a": 1})),
            None,
            &mut compared,
            &mut failures,
        );
        assert_eq!(compared, 1);
        assert!(failures.is_empty());

        call_mismatch(
            "ok diff",
            &Ok(json!({"a": 1})),
            &Ok(json!({"a": 2})),
            None,
            &mut compared,
            &mut failures,
        );
        assert_eq!(compared, 2);
        assert_eq!(failures, ["ok diff: a: rdashd 1 peer 2"]);
        failures.clear();

        // Equal errors fail by default so a broken RPC never passes silently.
        let same_err: Result<Value, String> = Err("RPC error: -32601 Method not found".into());
        call_mismatch("same err", &same_err, &same_err, None, &mut compared, &mut failures);
        assert_eq!(compared, 2);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("same err: rdashd error"), "{failures:?}");
        failures.clear();

        // Only the genesis coinbase error is allowed, and only when explicitly opted in.
        let genesis_err: Result<Value, String> =
            Err(format!("JSON-RPC error: -5 {GENESIS_COINBASE_ERROR}"));
        call_mismatch(
            "genesis",
            &genesis_err,
            &genesis_err,
            Some(GENESIS_COINBASE_ERROR),
            &mut compared,
            &mut failures,
        );
        assert_eq!(compared, 2);
        assert!(failures.is_empty(), "{failures:?}");

        // Even when genesis coinbase is allowed, an unrelated equal error still fails.
        call_mismatch(
            "genesis other err",
            &same_err,
            &same_err,
            Some(GENESIS_COINBASE_ERROR),
            &mut compared,
            &mut failures,
        );
        assert_eq!(failures.len(), 1);
        failures.clear();

        // One-sided errors and mismatched errors fail without incrementing compared.
        call_mismatch(
            "left err",
            &Err("boom".into()),
            &Ok(json!(1)),
            None,
            &mut compared,
            &mut failures,
        );
        call_mismatch(
            "right err",
            &Ok(json!(1)),
            &Err("boom".into()),
            None,
            &mut compared,
            &mut failures,
        );
        call_mismatch(
            "both diff",
            &Err("a".into()),
            &Err("b".into()),
            Some(GENESIS_COINBASE_ERROR),
            &mut compared,
            &mut failures,
        );
        assert_eq!(compared, 2);
        assert_eq!(failures.len(), 3);
    }

    #[test]
    fn same_reports_differing_keys_and_non_objects() {
        let mut failures = Vec::new();
        same("eq", &json!({"a": 1, "b": 2}), &json!({"b": 2, "a": 1}), &mut failures);
        assert!(failures.is_empty());

        same("scalar", &json!(1), &json!(2), &mut failures);
        assert_eq!(failures, ["scalar: rdashd 1 peer 2"]);
        failures.clear();

        same(
            "obj",
            &json!({"same": 0, "diff": 1, "only_ours": "x"}),
            &json!({"same": 0, "diff": 2, "only_theirs": "y"}),
            &mut failures,
        );
        assert_eq!(
            failures,
            [
                "obj: diff: rdashd 1 peer 2; only_ours: rdashd \"x\" peer null; only_theirs: rdashd null peer \"y\""
            ]
        );
    }
}
