//! The planes' witnesses over recorded posts (review r1 M-2; plan §9, rev1 §9): every check
//! in [`super::planes`] passes on what a real tier-1 run read (dashd's replies, Core's post
//! from the replay, V's reads and V's own post), and fails on each single difference it exists
//! to catch. The baselines pass V's recorded post where the leg does, and Core's as a second
//! baseline (a10-4 review r2-n1). The fixture's provenance names the run.

use dash_oracle_sys::{BlockAt, ChainHash, Post};
use serde_json::{Value, json};

use super::evo;
use super::inventory::Effects;
use super::model::P7;
use super::planes::{self, FAILED, View};
use super::replay::held;
use super::trial::{from_hex, hex};
use crate::chain::node::{IndexEntry, index_digest};
use crate::chain::world::{Hash, display_hash};

const FIXTURE: &str = include_str!("fixtures/planes-v24.0.0.json");

/// `BLOCK_FAILED_VALID`, `BLOCK_FAILED_CHILD` (chain.h).
const FAILED_VALID: u32 = 32;
const FAILED_CHILD: u32 = 64;

struct Snapshot {
    raw: Value,
    hash: Hash,
    parent: Hash,
    view: View,
    core: Post,
    /// V's post: its recorded plane fields ([`planes::POST_PLANES`]), decoded by the shim's own
    /// `Post` over Core's raw post for the fields no plane reads.
    v_post: Post,
}

impl Snapshot {
    fn dashd(&self, key: &str) -> &Value {
        &self.raw["dashd"][key]
    }

    fn v(&self, key: &str) -> &Value {
        &self.raw["v"][key]
    }

    fn model(&self) -> (u32, Hash) {
        height_hash(&self.raw["model_best_header"])
    }

    /// The model's best header, `best_invalid` and candidates the leg recorded (OD-L3-35).
    fn model_p7(&self) -> P7 {
        let m = &self.raw["model_p7"];
        P7 {
            best_header: self.model(),
            best_invalid: (!m["best_invalid"].is_null()).then(|| height_hash(&m["best_invalid"])),
            candidates: m["candidates"]
                .as_array()
                .expect("candidates")
                .iter()
                .map(height_hash)
                .collect(),
        }
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
            v_post: v_post(raw),
            raw: raw.clone(),
        })
        .collect()
}

fn v_post(raw: &Value) -> Post {
    let mut post = raw["core_post"].clone();
    let v = raw["v"]["post"].as_object().expect("V's recorded post");
    for key in planes::POST_PLANES {
        match v.get(key) {
            Some(value) => post[key] = value.clone(),
            None => {
                post.as_object_mut().expect("Core's post").remove(key);
            }
        }
    }
    serde_json::from_value(post).expect("V's post")
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
        assert_eq!(planes::p3(at, &s.view, &s.v_post, s.model().0), Ok(()), "{what}");
        assert_eq!(planes::p3(at, &s.view, &s.core, s.model().0), Ok(()), "{what}");
        let tip = display(&s.raw["model_tip"]);
        let (v, ledger) =
            (s.v("muhash").as_str().expect("V's"), s.raw["ledger_muhash"].as_str().expect("ours"));
        assert_eq!(planes::p5(&s.view, &tip, v, ledger), Ok(()), "{what}");
        assert_eq!(planes::p6(0, s.dashd("getdeploymentinfo"), &s.v_post), Ok(()), "{what}");
        assert_eq!(planes::p6(0, s.dashd("getdeploymentinfo"), &s.core), Ok(()), "{what}");
        assert_eq!(planes::p7(&s.v_post, s.model()), Ok(()), "{what}");
        assert_eq!(planes::p7(&s.core, s.model()), Ok(()), "{what}");
        assert_eq!(planes::p7_replay(at, &s.core, &s.v_post), Ok(()), "{what}");
        assert_eq!(planes::p7_core(at, &s.core, &s.v_post), Ok(()), "{what}");
        assert_eq!(planes::p7_model(at, &s.core, &s.model_p7()), Ok(()), "{what}");
        assert!(!planes::multi_candidate(&s.core), "{what}: OD-L3-19");
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

/// A recorded `(height, hash)` pair.
fn height_hash(v: &Value) -> (u32, Hash) {
    (v[0].as_u64().expect("a height") as u32, display(&v[1]))
}

/// `post` with `f` applied.
fn edited(post: &Post, f: &dyn Fn(&mut Post)) -> Post {
    let mut post = post.clone();
    f(&mut post);
    post
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
    fails(planes::p7_replay(0, &s.core, &v), "P7 diverges");

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
    fails(planes::p7_replay(0, &r.core, &v), "P7 diverges");

    // Core and V agree, the model's best header does not: a harness error.
    let model = P7 {
        best_header: (height, s.parent),
        ..s.model_p7()
    };
    fails(planes::p7_model(0, &s.core, &model), "harness error");
}

/// P7's Core side (OD-L3-15), from the rollback's recorded posts (the rolled-back block as
/// best_invalid, its parent the tip and the one candidate): each field dropped or altered on V's side; a post without the fields; a
/// tie on work and sequence id (Q-H); two candidates out of order; and the multi-candidate count.
#[test]
fn p7_core_catches_each_field() {
    let r = one("rollback");
    let best = r.core.best_invalid.expect("ABI 33").expect("the rolled-back block");
    assert_eq!(best.1, ChainHash(r.hash), "InvalidateBlock's best_invalid is the block");
    let tip = r.core.candidates.clone().expect("ABI 33");
    assert_eq!(tip.len(), 1, "Core prunes the set to its tip");
    let with = |f: &dyn Fn(&mut Post)| edited(&r.v_post, f);
    let core_side = |f: &dyn Fn(&mut Post)| edited(&r.core, f);
    assert_eq!(planes::p7_core(0, &r.core, &r.v_post), Ok(()));

    // best_invalid: none, absent (before ABI 33), another height, another hash.
    fails(planes::p7_core(0, &r.core, &with(&|v| v.best_invalid = Some(None))), "best_invalid");
    fails(planes::p7_core(0, &r.core, &with(&|v| v.best_invalid = None)), "(ABI 33)");
    fails(planes::p7_core(0, &core_side(&|c| c.best_invalid = None), &r.v_post), "(ABI 33)");
    let moved = BlockAt(best.0 - 1, best.1);
    fails(
        planes::p7_core(0, &r.core, &with(&|v| v.best_invalid = Some(Some(moved)))),
        "best_invalid",
    );
    let other = BlockAt(best.0, ChainHash(r.parent));
    fails(
        planes::p7_core(0, &r.core, &with(&|v| v.best_invalid = Some(Some(other)))),
        "best_invalid",
    );

    // candidates: absent, empty, and each of the five fields altered.
    fails(planes::p7_core(0, &r.core, &with(&|v| v.candidates = None)), "(ABI 33)");
    fails(planes::p7_core(0, &r.core, &with(&|v| v.candidates = Some(Vec::new()))), "candidates");
    let alter = |field: usize| {
        with(&|v| {
            let c = &mut v.candidates.as_mut().expect("ABI 33")[0];
            match field {
                0 => c.0 += 1,
                1 => c.1 = ChainHash(r.hash),
                2 => c.2 += 1,
                3 => c.3 ^= FAILED_CHILD,
                _ => c.4.push('0'),
            }
        })
    };
    for field in 0..5 {
        fails(planes::p7_core(0, &r.core, &alter(field)), "candidates");
    }

    // Two candidates: a second entry below the tip, in and out of order; on equal work and
    // sequence id, a tie.
    let mut lower = tip[0].clone();
    (lower.0, lower.1, lower.4) = (lower.0 - 1, ChainHash(r.hash), "0".repeat(lower.4.len()));
    let two = vec![lower.clone(), tip[0].clone()];
    let pair = core_side(&|c| c.candidates = Some(two.clone()));
    assert!(planes::multi_candidate(&pair), "OD-L3-19 counts it");
    let swapped = with(&|v| v.candidates = Some(vec![tip[0].clone(), lower.clone()]));
    fails(planes::p7_core(0, &pair, &swapped), "candidates");
    let mut twin = tip[0].clone();
    twin.1 = ChainHash(r.hash);
    let tie = core_side(&|c| c.candidates = Some(vec![twin.clone(), tip[0].clone()]));
    fails(planes::p7_core(0, &tie, &tie), "(Q-H)");
}

/// OD-L3-72 n3: a candidate status with a bit outside `INDEX_STATUS_MASK` (Core's
/// `BLOCK_ASSUMED_VALID`, 256) on either side is a harness error, even where both sides agree,
/// before any comparison; V's masked status is Core's.
#[test]
fn p7_core_fails_a_status_bit_outside_the_mask() {
    let r = one("rollback");
    let assumed = |p: &mut Post| {
        for c in p.candidates.iter_mut().flatten() {
            c.3 |= 256;
        }
    };
    let (core, v) = (edited(&r.core, &assumed), edited(&r.v_post, &assumed));
    for (core, v, side) in
        [(&core, &v, "Core's"), (&r.core, &v, "V's"), (&core, &r.v_post, "Core's")]
    {
        let e = planes::p7_core(0, core, v).expect_err("a bit outside the mask");
        assert!(e.starts_with("harness error at op 0: "), "{e}");
        assert!(
            e.contains(side) && e.contains("status bits 0x100 outside INDEX_STATUS_MASK"),
            "{e}"
        );
    }
    assert_eq!(planes::p7_core(0, &r.core, &r.v_post), Ok(()));
}

/// OD-L3-35: the model's `best_invalid` and candidates stand beside Core = V as a harness
/// cross-check. Perturbed on the model's side alone, each fails as a harness error, never as a
/// divergence; a V that differs from Core is P7's divergence, before the model is read.
#[test]
fn p7_model_fails_as_a_harness_error() {
    let r = one("rollback");
    let m = r.model_p7();
    assert!(m.best_invalid.is_some(), "the rollback leaves a best_invalid");
    assert_eq!(planes::p7_model(0, &r.core, &m), Ok(()));
    let harness = |model: P7| fails(planes::p7_model(0, &r.core, &model), "harness error at op 0");
    harness(P7 {
        best_invalid: None,
        ..m.clone()
    });
    let best = m.best_invalid.expect("a best_invalid");
    harness(P7 {
        best_invalid: Some((best.0 + 1, best.1)),
        ..m.clone()
    });
    harness(P7 {
        best_invalid: Some((best.0, r.parent)),
        ..m.clone()
    });
    harness(P7 {
        candidates: Vec::new(),
        ..m.clone()
    });
    let mut moved = m.clone();
    moved.candidates[0].0 += 1;
    harness(moved);
    let mut two = m.clone();
    two.candidates.push((best.0, best.1));
    harness(two);
    // A down post has nothing to compare.
    let mut down = r.core.clone();
    down.down = true;
    assert_eq!(
        planes::p7_model(
            0,
            &down,
            &P7 {
                best_header: (0, [0; 32]),
                best_invalid: None,
                candidates: Vec::new()
            }
        ),
        Ok(())
    );
}

/// The replay reads the model last: with the model wrong as well, a V that differs from Core
/// on P7's best header, index, `best_invalid` or candidates, or on P5, fails as that divergence,
/// not as a harness error; with V = Core, the model alone fails as one.
#[test]
fn the_replay_holds_core_to_v_before_the_model() {
    let r = one("rollback");
    let m = r.model_p7();
    assert_eq!(held(0, &r.core, &r.v_post, Some(&m)), Ok(()));
    assert_eq!(held(0, &r.core, &r.v_post, None), Ok(()));
    let wrong = P7 {
        best_header: (m.best_header.0, r.hash),
        best_invalid: None,
        ..m.clone()
    };
    assert_ne!(wrong.best_header, m.best_header, "the model's best header is wrong too");
    let check = |v: &Post| held(0, &r.core, v, Some(&wrong));
    fails(check(&r.v_post), "harness error at op 0");
    let side = |f: &dyn Fn(&mut Post)| edited(&r.v_post, f);
    fails(check(&side(&|v| v.best_header = Some(None))), "P7 diverges at op 0");
    fails(check(&side(&|v| v.index = Some(ChainHash([0; 32])))), "P7 diverges at op 0");
    fails(check(&side(&|v| v.best_invalid = Some(None))), "P7 diverges at op 0");
    fails(check(&side(&|v| v.candidates = Some(Vec::new()))), "P7 diverges at op 0");
    fails(check(&side(&|v| v.utxo = None)), "P5 (replay) diverges at op 0");
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
