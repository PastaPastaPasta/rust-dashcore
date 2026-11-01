//! The planes' witnesses over recorded posts (review r1 M-2; plan §9, rev1 §9): every check
//! in [`super::planes`] passes on what a real tier-1 run read (dashd's replies, Core's post
//! from the replay, V's reads), and fails on each single difference it exists to catch. The
//! fixture's provenance names the run.

use dash_oracle_sys::{BlockAt, ChainHash, Post};
use serde_json::{Value, json};

use super::evo;
use super::inventory::Effects;
use super::planes::{self, FAILED, View};
use super::trial::{from_hex, hex};
use crate::chain::node::{IndexEntry, index_digest};
use crate::chain::world::{Hash, display_hash};

const FIXTURE: &str = include_str!("fixtures/planes-v24.0.0-rc.4.json");

/// `BLOCK_FAILED_VALID`, `BLOCK_FAILED_CHILD` (chain.h).
const FAILED_VALID: u32 = 32;
const FAILED_CHILD: u32 = 64;

struct Snapshot {
    raw: Value,
    hash: Hash,
    parent: Hash,
    view: View,
    core: Post,
}

impl Snapshot {
    fn dashd(&self, key: &str) -> &Value {
        &self.raw["dashd"][key]
    }

    fn v(&self, key: &str) -> &Value {
        &self.raw["v"][key]
    }

    fn model(&self) -> (u32, Hash) {
        let m = &self.raw["model_best_header"];
        (m[0].as_u64().expect("a height") as u32, display(&m[1]))
    }

    fn effects(&self) -> Result<Effects, String> {
        let flag = |k: &str| self.dashd(k).as_bool().expect("a recorded flag");
        planes::dashd_effects(flag("known"), flag("stored"), &self.view.tips, &self.hash)
    }

    fn status(&self) -> Option<u32> {
        self.v("status").as_u64().map(|s| s as u32)
    }
}

fn display(v: &Value) -> Hash {
    v.as_str().and_then(display_hash).expect("a display hash")
}

fn snapshots() -> Vec<Snapshot> {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("the fixture");
    fixture["snapshots"]
        .as_array()
        .expect("snapshots")
        .iter()
        .map(|raw| Snapshot {
            hash: display(&raw["hash"]),
            parent: display(&raw["parent"]),
            view: View::from_json(&raw["dashd"]["view"]),
            core: serde_json::from_value(raw["core_post"].clone()).expect("Core's post"),
            raw: raw.clone(),
        })
        .collect()
}

fn one(what: &str) -> Snapshot {
    snapshots()
        .into_iter()
        .find(|s| {
            s.raw["what"] == what || s.raw["trial"].as_str().is_some_and(|t| t.ends_with(what))
        })
        .unwrap_or_else(|| panic!("a recorded snapshot {what}"))
}

/// The fixture covers each kind of op the planes judge.
#[test]
fn the_fixture_holds_each_kind_of_op() {
    let all = snapshots();
    let effects: Vec<Effects> = all.iter().map(|s| s.effects().expect("effects")).collect();
    let has = |known, stored, failed| {
        effects.contains(&Effects {
            known,
            failed,
            stored,
        })
    };
    assert!(has(false, false, false), "a proposal or a check reject");
    assert!(has(true, false, true), "a contextual reject");
    assert!(has(true, true, true), "a ConnectBlock reject or a rollback");
    assert!(has(true, true, false), "an accept");
    assert!(all.iter().any(|s| s.raw["what"] == "rollback"));
    assert!(all.iter().any(|s| s.v("index").as_array().is_some_and(|i| !i.is_empty())));
}

/// Every plane passes on every recorded op (the baselines).
#[test]
fn every_plane_passes_on_the_recorded_ops() {
    for s in snapshots() {
        let at = s.raw["op"].as_u64().expect("an op") as usize;
        let what = format!("{} {}", s.raw["trial"], s.raw["what"]);
        let reply = |side: &Value| planes::outcome_from_json(side);
        assert_eq!(
            planes::p1(at, &reply(s.dashd("reply")), &reply(s.v("reply"))),
            Ok(()),
            "{what}"
        );
        assert_eq!(
            planes::p2(&s.hash, s.effects().expect("effects"), s.status()),
            Ok(()),
            "{what}"
        );
        assert_eq!(planes::p3(at, &s.view, &s.core, s.model().0), Ok(()), "{what}");
        let tip = display(&s.raw["model_tip"]);
        let (v, ledger) =
            (s.v("muhash").as_str().expect("V's"), s.raw["ledger_muhash"].as_str().expect("ours"));
        assert_eq!(planes::p5(&s.view, &tip, v, ledger), Ok(()), "{what}");
        assert_eq!(planes::p6(0, s.dashd("getdeploymentinfo"), &s.core), Ok(()), "{what}");
        assert_eq!(planes::p7(&s.core, s.model()), Ok(()), "{what}");
        assert_eq!(planes::p7_replay(at, &s.core, &s.core, Some(s.model())), Ok(()), "{what}");
        let parent = s.dashd("getdeploymentinfo_parent");
        let v_parent = s.v("deployments_parent");
        assert_eq!(
            planes::body_gates(&s.parent, parent, v_parent, s.dashd("first_decoded")),
            Ok(()),
            "{what}"
        );
        assert_eq!(planes::mnsync_unsynced(s.dashd("mnsync")), Ok(()), "{what}");
        let counts = s.v("evo_counts").as_object().expect("V's counters");
        let counts = counts.iter().map(|(k, n)| (leak(k), n.as_u64().expect("a count"))).collect();
        assert_eq!(evo::gate_check(&counts, at), Ok(()), "{what}");
    }
}

fn leak(key: &str) -> &'static str {
    dashd_chainstate::evo::gates::KEYS.iter().copied().find(|k| *k == key).expect("a counted key")
}

fn fails(result: Result<(), String>, plane: &str) {
    let e = result.expect_err("the witness must fail");
    assert!(e.contains(plane), "expected {plane}: {e}");
}

/// P1: a V reply that is not dashd's.
#[test]
fn p1_catches_a_different_reply() {
    let s = one("cb_amount.reg@Reg/par1+cold");
    let dashd = planes::outcome_from_json(s.dashd("reply"));
    fails(planes::p1(0, &dashd, &Ok(Value::Null)), "P1 diverges");
    fails(planes::p1(0, &dashd, &Ok(json!("bad-cb-amount "))), "P1 diverges");
}

/// P2: V's status bits, and dashd's tip row, each decide.
#[test]
fn p2_catches_each_effect() {
    for what in
        ["cb_amount.reg@Reg/par1+cold", "cb_height.reg@Reg/par1+cold", "control.reg@Reg/par1+cold"]
    {
        let s = one(what);
        let dashd = s.effects().expect("effects");
        let status = s.status().expect("a V entry");
        fails(planes::p2(&s.hash, dashd, Some(status ^ FAILED_VALID)), "P2 diverges");
        fails(planes::p2(&s.hash, dashd, Some(status ^ planes::HAVE_DATA)), "P2 diverges");
        fails(planes::p2(&s.hash, dashd, None), "P2 diverges");
        let mut tips = s.view.tips.clone();
        for row in tips.as_array_mut().expect("tips") {
            if row["hash"] == hex(&s.hash) {
                row["status"] = json!(if dashd.failed {
                    "valid-fork"
                } else {
                    "invalid"
                });
            }
        }
        let flipped = planes::dashd_effects(true, dashd.stored, &tips, &s.hash).expect("effects");
        fails(planes::p2(&s.hash, flipped, Some(status)), "P2 diverges");
    }
    let p = one("cb_amount.reg@Reg/par1+prop");
    fails(planes::p2(&p.hash, p.effects().expect("effects"), Some(FAILED_VALID)), "P2 diverges");
}

/// P3: each compared field, and neither side's tips may be empty or missing.
#[test]
fn p3_catches_each_field() {
    let s = one("cb_height.reg@Reg/par1+cold");
    let header = s.model().0;
    let mut short = s.core.clone();
    short.tips.as_mut().expect("tips").pop();
    fails(planes::p3(0, &s.view, &short, header), "P3 diverges");
    let mut none = s.core.clone();
    none.tips = None;
    fails(planes::p3(0, &s.view, &none, header), "no tips");
    let mut empty = (s.view.clone(), s.core.clone());
    empty.0.tips = json!([]);
    empty.1.tips = Some(Vec::new());
    fails(planes::p3(0, &empty.0, &empty.1, header), "P3 diverges");
    let mut status = s.core.clone();
    status.tips.as_mut().expect("tips")[0]["status"] = json!("headers-only");
    fails(planes::p3(0, &s.view, &status, header), "P3 diverges");
    let mut headers = s.view.clone();
    headers.info["headers"] = json!(header + 1);
    fails(planes::p3(0, &headers, &s.core, header), "P3 diverges");
    fails(planes::p3(0, &s.view, &s.core, header + 1), "P3 diverges");
    let mut ibd = s.core.clone();
    ibd.ibd = ibd.ibd.map(|b| !b);
    fails(planes::p3(0, &s.view, &ibd, header), "P3 diverges");
    let mut tip = s.core.clone();
    tip.tip = tip.tip.map(|BlockAt(h, _)| BlockAt(h, ChainHash(s.hash)));
    fails(planes::p3(0, &s.view, &tip, header), "P3 diverges");
}

/// P5: each of the three MuHashes, and the coins' block.
#[test]
fn p5_catches_each_digest() {
    let s = one("control.reg@Reg/par1+cold");
    let tip = display(&s.raw["model_tip"]);
    let (v, ledger) =
        (s.v("muhash").as_str().expect("V's"), s.raw["ledger_muhash"].as_str().expect("ours"));
    let other = "00".repeat(32);
    fails(planes::p5(&s.view, &tip, &other, ledger), "P5 diverges");
    fails(planes::p5(&s.view, &tip, v, &other), "P5 diverges");
    let mut dashd = s.view.clone();
    dashd.txoutset["muhash"] = json!(other);
    fails(planes::p5(&dashd, &tip, v, ledger), "P5 diverges");
    fails(planes::p5(&s.view, &s.parent, v, ledger), "P5: gettxoutsetinfo at");
}

/// P6: one deployment's state, and a missing reply.
#[test]
fn p6_catches_a_deployment() {
    let s = one("cb_amount.reg@Reg/par1+cold");
    let mut post = s.core.clone();
    let deployments = post.deployments.as_mut().expect("deployments");
    deployments["deployments"]["v24"]["active"] = json!(true);
    fails(planes::p6(0, s.dashd("getdeploymentinfo"), &post), "P6 diverges");
    fails(planes::p6(0, &Value::Null, &s.core), "P6 diverges");
}

/// P7 (M-11): V's best header at the right height with another hash; one index entry's
/// failure bit (FAILED_CHILD for FAILED_VALID) through the index digest alone; and a model
/// that disagrees with both Core and V.
#[test]
fn p7_catches_the_hash_the_index_and_the_model() {
    let s = one("cb_height.reg@Reg/par1+cold");
    let (height, _) = s.model();
    let mut v = s.core.clone();
    v.best_header = Some(Some(BlockAt(height as i32, ChainHash(s.parent))));
    fails(planes::p7(&v, s.model()), "P7: V's best header");
    fails(planes::p7_replay(0, &s.core, &v, Some(s.model())), "P7 diverges");

    // The index digest, from V's recorded entries: Core's exactly, and one status bit off.
    let r = one("rollback");
    let entries: Vec<IndexEntry> = r
        .v("index")
        .as_array()
        .expect("V's index")
        .iter()
        .map(|e| IndexEntry {
            hash: dash_primitives::BlockHash::from(
                <[u8; 32]>::try_from(from_hex(e[0].as_str().expect("hex")).expect("hex"))
                    .expect("32 bytes"),
            ),
            height: e[1].as_u64().expect("a height") as u32,
            status: e[2].as_u64().expect("a status") as u32,
            has_txs: e[3].as_bool().expect("a flag"),
        })
        .collect();
    assert_eq!(Some(index_digest(&entries)), r.core.index, "V's entries give Core's digest");
    let mut flipped = entries.clone();
    let case = display(&r.raw["hash"]);
    let other = flipped
        .iter_mut()
        .find(|e| e.status & FAILED_VALID != 0 && e.hash.as_bytes()[..] != case[..])
        .expect("a failed entry of an earlier case");
    other.status = (other.status & !FAILED) | FAILED_CHILD;
    let mut v = r.core.clone();
    v.index = Some(index_digest(&flipped));
    assert_eq!(v.best_header, r.core.best_header, "the best header alone is unchanged");
    fails(planes::p7_replay(0, &r.core, &v, Some(r.model())), "P7 diverges");

    // Core and V agree, the model does not: a harness error.
    let model = (height, s.parent);
    fails(planes::p7_replay(0, &s.core, &s.core, Some(model)), "harness error");
}

/// §3.6 (M-12/M-13): DIP3 or V24 active at the parent on either side, or a decoded CbTx.
#[test]
fn the_body_gates_catch_each_side() {
    let s = one("control.reg@Reg/par1+cold");
    let (dashd, v, first) =
        (s.dashd("getdeploymentinfo_parent"), s.v("deployments_parent"), s.dashd("first_decoded"));
    for name in ["dip0003", "v24"] {
        let mut active = dashd.clone();
        active["deployments"][name]["active"] = json!(true);
        fails(planes::body_gates(&s.parent, &active, v, first), &format!("§3.6: {name}"));
        fails(planes::body_gates(&s.parent, dashd, &active, first), &format!("§3.6: {name}"));
        let mut missing = dashd.clone();
        missing["deployments"].as_object_mut().expect("deployments").remove(name);
        fails(planes::body_gates(&s.parent, &missing, v, first), &format!("§3.6: {name}"));
    }
    let mut cbtx = first.clone();
    cbtx["cbTx"] = json!({"version": 3});
    fails(planes::body_gates(&s.parent, dashd, v, &cbtx), "decoded CbTx");
}

/// The manifest's mnsync (rev1 §9): synced, missing and malformed are harness errors.
#[test]
fn mnsync_must_read_not_synced() {
    let s = one("control.reg@Reg/par1+cold");
    let status = s.dashd("mnsync");
    let mut synced = status.clone();
    synced["IsSynced"] = json!(true);
    fails(planes::mnsync_unsynced(&synced), "mnsync is synced");
    let mut missing = status.clone();
    missing.as_object_mut().expect("status").remove("IsSynced");
    fails(planes::mnsync_unsynced(&missing), "without IsSynced");
    let mut malformed = status.clone();
    malformed["IsSynced"] = json!("false");
    fails(planes::mnsync_unsynced(&malformed), "IsSynced is");
}

/// M-14's leg half: a recorded op with a gated counter at 1 fails, naming the key and site.
#[test]
fn an_entered_gate_fails_the_op() {
    let s = one("control.reg@Reg/par1+cold");
    let counts = s.v("evo_counts").as_object().expect("V's counters");
    let mut counts: std::collections::BTreeMap<&'static str, u64> =
        counts.iter().map(|(k, n)| (leak(k), n.as_u64().expect("a count"))).collect();
    assert!(counts["w.stx"] > 0, "the recorded hook is live");
    counts.insert("g.cbtx", 1);
    fails(evo::gate_check(&counts, 9), "g.cbtx = 1 at dashd-chainstate evo/ev1.rs");
}
