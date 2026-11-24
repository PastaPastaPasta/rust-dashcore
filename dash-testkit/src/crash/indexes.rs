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

/// `-txindex`, `-blockfilterindex` and `-coinstatsindex`, on rdashd and both dashds.
pub const INDEX_ARGS: [&str; 3] = ["-txindex=1", "-blockfilterindex=1", "-coinstatsindex=1"];

/// `getindexinfo`'s names for those three, in Core's order.
const INDEX_NAMES: [&str; 3] = ["txindex", "coinstatsindex", "basic block filter index"];

/// Earlier active-chain blocks an index check samples besides the highest comparable one.
pub const INDEX_SAMPLE: usize = 4;

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

/// The highest block of `active` (tip first) that `on_reference` holds, then up to
/// [`INDEX_SAMPLE`] earlier ones drawn with `seed`. Empty when the chains share nothing.
pub fn index_blocks(active: &[String], on_reference: &HashSet<String>, seed: u64) -> Vec<String> {
    let mut shared: Vec<String> =
        active.iter().filter(|h| on_reference.contains(h.as_str())).cloned().collect();
    if shared.is_empty() {
        return shared;
    }
    let tip = shared.remove(0);
    let mut picked = vec![tip];
    let mut rng = Rng::new(seed);
    for _ in 0..INDEX_SAMPLE.min(shared.len()) {
        let i = rng.below(shared.len() as u64) as usize;
        picked.push(shared.swap_remove(i));
    }
    picked
}

/// What one poll of the index wait decided. `synced` is the height when all three indexes are
/// synced and agree; `tip` is this poll's `getblockcount`; `floor` is the height the check has
/// to have reached (C2's, or the reference chain's tip).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexWait {
    /// Synced at `tip`, and `tip` is at least `floor`.
    Ready,
    /// Still behind the tip, or the tip has not reached `floor` yet.
    Poll,
    /// The indexes are synced above the chain's current tip.
    Past {
        at: u64,
        tip: u64,
    },
}

/// The index wait's decision for one poll. A tip that is still moving is read again on the next
/// poll: synced at the count from an earlier poll is not "past" once that count is the tip.
pub fn index_wait(synced: Option<u64>, tip: u64, floor: u64) -> IndexWait {
    match synced {
        Some(at) if at > tip => IndexWait::Past {
            at,
            tip,
        },
        Some(at) if at == tip && at >= floor => IndexWait::Ready,
        _ => IndexWait::Poll,
    }
}

/// The index check at recovered `tip`, against `peer` whose chain tip is `peer_tip`.
pub fn check(
    rdashd: &dyn NodeRpc,
    peer: &Peer,
    fixture: &Fixture,
    tip: &str,
    peer_tip: &str,
    seed: u64,
) -> Vec<String> {
    let mut failures = Vec::new();
    let Some(height) = fixture.rows.get(tip).map(|r| r.height) else {
        failures.push(format!("CX: {tip} is no fixture block"));
        return failures;
    };
    let Some(peer_height) = fixture.rows.get(peer_tip).map(|r| r.height) else {
        failures.push(format!("CX: {peer_tip} is no fixture block"));
        return failures;
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
            failures.push(format!("CX getindexinfo rdashd: {e}"));
            return failures;
        }
    };
    let theirs = match wait_for_indexes(&mut dashd, peer_height) {
        Ok(info) => info,
        Err(e) => {
            failures.push(format!("CX getindexinfo peer: {e}"));
            return failures;
        }
    };
    let tips_differ = tip != peer_tip;
    if tips_differ {
        // Each index is synced at its own chain's tip. The heights are not the same number.
        same("CX getindexinfo", &without_heights(&ours), &without_heights(&theirs), &mut failures);
    } else {
        same("CX getindexinfo", &ours, &theirs, &mut failures);
    }
    let on_reference: HashSet<String> = active_chain(fixture, peer_tip).into_iter().collect();
    let blocks = index_blocks(&active_chain(fixture, tip), &on_reference, seed);
    if blocks.is_empty() {
        failures.push(format!("CX: no block of {tip} is on the reference chain"));
    }
    for hash in &blocks {
        compare_block(&mut rd, &mut dashd, hash, tips_differ, &mut failures);
    }
    failures
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
/// `floor`. The count is read on every poll. An index height above that count fails.
fn wait_for_indexes(
    call: &mut impl FnMut(&str, &[Value]) -> Result<Value, String>,
    floor: u64,
) -> Result<Value, String> {
    let deadline = Instant::now() + RPC_PATIENCE;
    loop {
        let last = match (call("getblockcount", &[]), call("getindexinfo", &[])) {
            (Ok(count), Ok(info)) => match count.as_u64() {
                Some(tip) => match index_wait(index_height(&info).ok(), tip, floor) {
                    IndexWait::Ready => return Ok(info),
                    IndexWait::Past {
                        at,
                        tip,
                    } => return Err(format!("indexes synced at {at}, past {tip}: {info}")),
                    IndexWait::Poll => info.to_string(),
                },
                None => format!("getblockcount {count}"),
            },
            (Err(e), _) => e,
            (_, Err(e)) => e,
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
    tips_differ: bool,
    failures: &mut Vec<String>,
) {
    let filter = |v: &Value| json!({"filter": v["filter"].clone(), "header": v["header"].clone()});
    match (rdashd("getblockfilter", &[json!(hash)]), peer("getblockfilter", &[json!(hash)])) {
        (Ok(a), Ok(b)) => {
            same(&format!("CX getblockfilter {hash}"), &filter(&a), &filter(&b), failures)
        }
        (a, b) => call_mismatch(&format!("CX getblockfilter {hash}"), &a, &b, failures),
    }
    let stats = [json!("muhash"), json!(hash)];
    call_mismatch(
        &format!("CX gettxoutsetinfo {hash}"),
        &rdashd("gettxoutsetinfo", &stats),
        &peer("gettxoutsetinfo", &stats),
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
    for tx in txs {
        let Some(txid) = tx.as_str() else {
            failures.push(format!("CX getblock {hash}: tx entry {tx}"));
            continue;
        };
        // With the block hash, and without it so the txindex answers.
        for (form, params) in [
            ("", vec![json!(txid), json!(true)]),
            (" in block", vec![json!(txid), json!(true), json!(hash)]),
        ] {
            let ours = rdashd("getrawtransaction", &params).map(|v| comparable_tx(v, tips_differ));
            let theirs = peer("getrawtransaction", &params).map(|v| comparable_tx(v, tips_differ));
            call_mismatch(&format!("CX getrawtransaction {txid}{form}"), &ours, &theirs, failures);
        }
    }
}

fn call_mismatch(
    what: &str,
    ours: &Result<Value, String>,
    theirs: &Result<Value, String>,
    failures: &mut Vec<String>,
) {
    match (ours, theirs) {
        (Ok(a), Ok(b)) => same(what, a, b, failures),
        (Err(a), Err(b)) if a == b => {}
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

    #[test]
    fn the_sample_is_the_highest_shared_block_and_a_seeded_few_earlier() {
        let active = chain(20);
        let on: HashSet<String> = (0..=15).map(|i| format!("b{i:02}")).collect();
        let picked = index_blocks(&active, &on, 5);
        assert_eq!(picked, index_blocks(&active, &on, 5));
        assert_eq!(picked[0], "b15");
        assert_eq!(picked.len(), 1 + INDEX_SAMPLE);
        assert!(picked[1..].iter().all(|h| on.contains(h) && h != "b15"));
        assert_ne!(picked, index_blocks(&active, &on, 9));
        // The recovered tip, when the reference still has it.
        let all: HashSet<String> = active.iter().cloned().collect();
        assert_eq!(index_blocks(&active, &all, 1)[0], "b19");
        // Fewer earlier blocks than the sample.
        let short = chain(3);
        let on_short: HashSet<String> = short.iter().cloned().collect();
        assert_eq!(index_blocks(&short, &on_short, 1).len(), 3);
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

    /// The wait, one poll at a time: `Ok(tip)` when it would return, `Err("past")` on a height
    /// above that poll's tip, `Err("polling")` if the sequence ends still short.
    fn settle(polls: &[(u64, Option<u64>)], floor: u64) -> Result<u64, &'static str> {
        for &(tip, synced) in polls {
            match index_wait(synced, tip, floor) {
                IndexWait::Ready => return Ok(tip),
                IndexWait::Past {
                    ..
                } => return Err("past"),
                IndexWait::Poll => {}
            }
        }
        Err("polling")
    }

    #[test]
    fn the_wait_follows_the_tip_it_reads_on_each_poll() {
        // Synced at the current count passes, including once the tip is above C2's height.
        assert_eq!(index_wait(Some(10), 10, 10), IndexWait::Ready);
        assert_eq!(index_wait(Some(115), 115, 114), IndexWait::Ready);
        // Behind, or not synced yet, keeps polling. So does a tip that has not reached C2.
        assert_eq!(index_wait(Some(9), 10, 10), IndexWait::Poll);
        assert_eq!(index_wait(None, 10, 10), IndexWait::Poll);
        assert_eq!(index_wait(Some(114), 114, 115), IndexWait::Poll);
        // Above the current tip fails. Above C2's height does not, once it is the tip.
        assert_eq!(
            index_wait(Some(11), 10, 10),
            IndexWait::Past {
                at: 11,
                tip: 10
            }
        );
        // The tip moves after a poll that had not caught up, and the next poll's count is the
        // one the indexes have to meet. A count frozen from the first poll would call 115 past.
        assert_eq!(settle(&[(114, Some(100)), (115, Some(114)), (115, Some(115))], 114), Ok(115));
        assert_eq!(settle(&[(114, None), (115, Some(115))], 114), Ok(115));
        assert_eq!(settle(&[(10, Some(11))], 10), Err("past"));
        assert_eq!(settle(&[(10, Some(9)), (10, Some(9))], 10), Err("polling"));
    }
}
