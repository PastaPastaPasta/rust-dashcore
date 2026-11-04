//! The planes' checks (plan §3.3, §3.6), pure: each takes what a leg read from dashd after an
//! op and V's post (or Core's, in the replay), so recorded posts drive their witnesses
//! (review r1 M-2). The leg fetches; these decide.

use dash_oracle_sys::{BlockAt, Candidate, ChainHash, Post};
use serde_json::{Value, json};

use super::inventory::{Effects, ParentState};
use super::model::P7;
use super::trial::hex;
use crate::chain::node::INDEX_STATUS_MASK;
use crate::chain::shape::Outcome;
use crate::chain::world::{Hash, display_hash};

/// `BLOCK_HAVE_DATA`, `BLOCK_FAILED_VALID | BLOCK_FAILED_CHILD` (chain.h).
pub const HAVE_DATA: u32 = 8;
pub const FAILED: u32 = 32 | 64;
/// `BLOCK_FAILED_VALID`, `BLOCK_VALID_MASK`, `BLOCK_VALID_SCRIPTS` (chain.h).
pub const FAILED_VALID: u32 = 32;
const VALID_MASK: u32 = 7;
const VALID_SCRIPTS: u32 = 5;

/// What dashd shows after an op, for P3 and P5: `getchaintips`, `getbestblockhash`,
/// `getblockchaininfo` and `gettxoutsetinfo muhash`, as returned.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub tips: Value,
    pub best: Value,
    pub info: Value,
    pub txoutset: Value,
}

impl View {
    pub fn json(&self) -> Value {
        json!({
            "getchaintips": self.tips,
            "getbestblockhash": self.best,
            "getblockchaininfo": self.info,
            "gettxoutsetinfo": self.txoutset,
        })
    }

    pub fn from_json(v: &Value) -> View {
        View {
            tips: v["getchaintips"].clone(),
            best: v["getbestblockhash"].clone(),
            info: v["getblockchaininfo"].clone(),
            txoutset: v["gettxoutsetinfo"].clone(),
        }
    }
}

pub fn rpc_hash(value: &Value) -> Result<Hash, String> {
    value.as_str().and_then(display_hash).ok_or_else(|| format!("not a block hash: {value}"))
}

pub fn block_at(height: u32, hash: Hash) -> BlockAt {
    BlockAt(i32::try_from(height).expect("a regtest height"), ChainHash(hash))
}

/// P1: V's reply is dashd's.
pub fn p1(at: usize, dashd: &Outcome, v: &Outcome) -> Result<(), String> {
    if v != dashd {
        return Err(format!("P1 diverges at op {at}: dashd {dashd:?}, V {v:?}"));
    }
    Ok(())
}

/// dashd's effects for `hash`: whether `getblockheader` knew it, whether `getblock` served
/// its data, and whether its `getchaintips` row is `invalid`.
pub fn dashd_effects(
    known: bool,
    stored: bool,
    tips: &Value,
    hash: &Hash,
) -> Result<Effects, String> {
    if !known {
        return Ok(Effects::NONE);
    }
    let row =
        tip_row(tips, hash).ok_or_else(|| format!("{} is known but no chain tip", hex(hash)))?;
    Ok(Effects {
        known,
        failed: row["status"] == "invalid",
        stored,
    })
}

/// `hash`'s `getchaintips` row, if it is a tip.
pub fn tip_row<'t>(tips: &'t Value, hash: &Hash) -> Option<&'t Value> {
    tips.as_array().and_then(|rows| rows.iter().find(|r| r["hash"] == hex(hash)))
}

/// Whether a proposal's answer for an indexed block says it failed (rpc/mining.cpp:711-716:
/// `duplicate` for VALID_SCRIPTS, `duplicate-invalid` for a failure flag,
/// `duplicate-inconclusive` otherwise).
pub fn failed_by_answer(answer: &Value) -> Result<bool, String> {
    match answer.as_str() {
        Some("duplicate-invalid") => Ok(true),
        Some("duplicate" | "duplicate-inconclusive") => Ok(false),
        _ => Err(format!("an indexed block's proposal answered {answer}")),
    }
}

/// P2: V's index entry for the block (its status, `None` for no entry) shows dashd's effects.
pub fn p2(hash: &Hash, dashd: Effects, v_status: Option<u32>) -> Result<(), String> {
    let v = v_status.map_or(Effects::NONE, |status| Effects {
        known: true,
        failed: status & FAILED != 0,
        stored: status & HAVE_DATA != 0,
    });
    if v != dashd {
        return Err(format!("P2 diverges for {}: dashd {dashd:?}, V {v:?}", hex(hash)));
    }
    Ok(())
}

/// `getchaintips` in V's post's order: by height descending, then hash.
fn sorted_tips(tips: &[Value]) -> Vec<Value> {
    let mut tips = tips.to_vec();
    tips.sort_by_key(|t| {
        (std::cmp::Reverse(t["height"].as_u64()), t["hash"].as_str().map(str::to_owned))
    });
    tips
}

/// P3: the chain tips, the tip, the best header's height and IBD, dashd vs V's post; and the
/// headers count against the model's best header. Neither side's tips may be missing.
pub fn p3(at: usize, view: &View, post: &Post, model_header: u32) -> Result<(), String> {
    let dashd =
        view.tips.as_array().ok_or_else(|| format!("P3 at op {at}: getchaintips {}", view.tips))?;
    let v = post.tips.as_ref().ok_or_else(|| format!("P3 at op {at}: V's post has no tips"))?;
    if dashd.is_empty() || sorted_tips(dashd) != sorted_tips(v) {
        return Err(format!("P3 diverges at op {at}: getchaintips {dashd:?}, V {v:?}"));
    }
    let best = rpc_hash(&view.best)?;
    let headers = view.info["headers"].as_u64();
    let v_header = match &post.best_header {
        Some(Some(BlockAt(height, _))) => u64::try_from(*height).ok(),
        _ => None,
    };
    if post.tip.as_ref().map(|t| t.1.0) != Some(best)
        || headers.is_none()
        || headers != v_header
        || headers != Some(u64::from(model_header))
        || view.info["initialblockdownload"].as_bool().is_none()
        || view.info["initialblockdownload"].as_bool() != post.ibd
    {
        return Err(format!(
            "P3 diverges at op {at}: dashd {}, V tip {:?} header {:?} ibd {:?}, model header {model_header}",
            view.info, post.tip, post.best_header, post.ibd
        ));
    }
    Ok(())
}

/// P5: dashd's MuHash at the tip, V's coins' and the ledger's (display hex).
pub fn p5(view: &View, tip: &Hash, v: &str, ledger: &str) -> Result<(), String> {
    if rpc_hash(&view.txoutset["bestblock"])? != *tip {
        return Err(format!(
            "P5: gettxoutsetinfo at {}, the tip is {}",
            view.txoutset["bestblock"],
            hex(tip)
        ));
    }
    let dashd = view.txoutset["muhash"].as_str().ok_or("gettxoutsetinfo: no muhash")?;
    if dashd != v || v != ledger {
        return Err(format!("P5 diverges: dashd {dashd}, V {v}, ledger {ledger}"));
    }
    Ok(())
}

/// P6: `getdeploymentinfo` at the tip vs V's post.
pub fn p6(height: u32, dashd: &Value, post: &Post) -> Result<(), String> {
    if dashd.is_null() || Some(dashd) != post.deployments.as_ref() {
        return Err(format!("P6 diverges at {height}: dashd {dashd}, V {:?}", post.deployments));
    }
    Ok(())
}

/// P7 after an op: V's best header is the model's exactly.
pub fn p7(post: &Post, (height, hash): (u32, Hash)) -> Result<(), String> {
    if post.best_header != Some(Some(block_at(height, hash))) {
        return Err(format!(
            "P7: V's best header {:?} is not the model's ({height}, {})",
            post.best_header,
            hex(&hash)
        ));
    }
    Ok(())
}

/// P7's Core side in the replay (M-11, OD-L3-15): Core's `best_invalid` and `candidates`
/// (`m_best_invalid` and `setBlockIndexCandidates`, shim ABI 33) are V's exactly, each candidate
/// as `[height, hash, nSequenceId, nStatus, chain work]` in the set's order. An up post
/// on either side without them is a shim or V before ABI 33. A candidate status with a bit
/// outside `INDEX_STATUS_MASK` is a harness error (OD-L3-72 n3): the shim masks Core's and V
/// posts its own unmasked, and neither digest nor comparison would see such a bit
/// (`BLOCK_ASSUMED_VALID`, which only assumeutxo sets). A run of equal work and sequence id,
/// which Core orders by address and both sides by hash (Q-H), fails: none forms in a10's legs,
/// which never restart.
pub fn p7_core(at: usize, core: &Post, v: &Post) -> Result<(), String> {
    for (side, post) in [("Core", core), ("V", v)] {
        if !post.down && (post.best_invalid.is_none() || post.candidates.is_none()) {
            return Err(format!(
                "P7 at op {at}: {side}'s up post has no best_invalid or candidates (ABI 33)"
            ));
        }
        let outside = post.candidates.iter().flatten().find(|c| c.3 & !INDEX_STATUS_MASK != 0);
        if let Some(c) = outside {
            return Err(format!(
                "harness error at op {at}: {side}'s candidate {c:?} has status bits {:#x} outside \
                 INDEX_STATUS_MASK {INDEX_STATUS_MASK:#x} (OD-L3-72 n3)",
                c.3 & !INDEX_STATUS_MASK
            ));
        }
    }
    if core.best_invalid != v.best_invalid {
        return Err(format!(
            "P7 diverges at op {at}: Core best_invalid {:?}, V {:?}",
            core.best_invalid, v.best_invalid
        ));
    }
    if let Some(run) = core.candidates.as_deref().and_then(|c| c.windows(2).find(|w| tied(w))) {
        return Err(format!(
            "P7 at op {at}: Core's candidates {run:?} tie on work and sequence id (Q-H)"
        ));
    }
    if core.candidates != v.candidates {
        return Err(format!(
            "P7 diverges at op {at}: Core candidates {:?}, V {:?}",
            core.candidates, v.candidates
        ));
    }
    Ok(())
}

fn tied(pair: &[Candidate]) -> bool {
    (&pair[0].4, pair[0].2) == (&pair[1].4, pair[1].2)
}

/// P7's model cross-check in the replay (OD-L3-35), once [`p7_replay`], [`p7_core`] and P5
/// held Core to V: the model's best header, `best_invalid` and candidates (height and hash, in
/// order) are Core's, or the run fails as a harness error, never as a divergence. A down post
/// has nothing to compare.
pub fn p7_model(at: usize, core: &Post, model: &P7) -> Result<(), String> {
    if core.down {
        return Ok(());
    }
    let (height, hash) = model.best_header;
    if core.best_header != Some(Some(block_at(height, hash))) {
        return Err(format!(
            "harness error at op {at}: the model's best header ({height}, {}) disagrees with Core and V {:?}",
            hex(&hash),
            core.best_header
        ));
    }
    let best = model.best_invalid.map(|(height, hash)| block_at(height, hash));
    let candidates: Vec<BlockAt> =
        model.candidates.iter().map(|(height, hash)| block_at(*height, *hash)).collect();
    let core_candidates: Option<Vec<BlockAt>> =
        core.candidates.as_ref().map(|c| c.iter().map(|c| BlockAt(c.0, c.1)).collect());
    if core.best_invalid != Some(best) || core_candidates.as_ref() != Some(&candidates) {
        return Err(format!(
            "harness error at op {at}: the model's best_invalid {best:?} and candidates \
             {candidates:?} disagree with Core and V ({:?}, {core_candidates:?})",
            core.best_invalid
        ));
    }
    Ok(())
}

/// Whether an up post holds more than one candidate (OD-L3-19's tripwire: Core prunes the set
/// to the tip after each op, so any such post voids the order-equivalence claim).
pub fn multi_candidate(post: &Post) -> bool {
    !post.down && post.candidates.as_ref().is_some_and(|c| c.len() > 1)
}

/// P7 in the replay (M-11): Core's best header and index digest are V's.
pub fn p7_replay(at: usize, core: &Post, v: &Post) -> Result<(), String> {
    if core.best_header != v.best_header || core.index != v.index {
        return Err(format!(
            "P7 diverges at op {at}: Core best header {:?} index {:?}, V {:?} {:?}",
            core.best_header, core.index, v.best_header, v.index
        ));
    }
    Ok(())
}

/// §3.6's per-body deployment gates for a block on `parent`: DIP3 and V24 inactive on both
/// sides (each side's `getdeploymentinfo` there), and dashd decodes no CbTx in its first
/// transaction. A failed gate names the side and V's site.
pub fn body_gates(
    parent: &Hash,
    dashd: &Value,
    v: &Value,
    first_decoded: &Value,
) -> Result<(), String> {
    let active = |d: &Value, name: &str| d["deployments"][name]["active"].as_bool();
    // The counted E-evo keys each gate shuts; dip0003's is their `dip3_parent` cross-check.
    for (name, keys) in [("dip0003", "g.quorum, g.dmn (dip3_parent)"), ("v24", "g.coll")] {
        if active(dashd, name) != Some(false) || active(v, name) != Some(false) {
            return Err(format!(
                "§3.6: {name} at {}: dashd {:?}, V {:?}; E-evo {keys} (V's evo hooks run \
                 DIP3-gated bodies there: dashd-chainstate evo/ev1.rs and evo/node.rs)",
                hex(parent),
                active(dashd, name),
                active(v, name)
            ));
        }
    }
    if first_decoded.get("cbTx").is_some() {
        return Err(format!(
            "§3.6: a decoded CbTx in a body on {} (V's site: evo/ev1.rs DmlChecks::check_cbtx)",
            hex(parent)
        ));
    }
    Ok(())
}

/// The parent's state as V's index shows it (m-4): its status, and whether a block marked
/// BLOCK_FAILED_VALID is an ancestor of it (Core's failed set, as the index records it).
pub fn v_parent_state(status: u32, failed_ancestor: bool) -> ParentState {
    if status & FAILED != 0 {
        ParentState::Failed
    } else if status & VALID_MASK < VALID_SCRIPTS && failed_ancestor {
        ParentState::UnmarkedFailedAncestor
    } else {
        ParentState::Clean
    }
}

/// Whether dashd shows the parent `prev` failed (m-4): `getblockheader`'s confirmations put it
/// on the active chain (never failed), or its `getchaintips` row says. A parent off the active
/// chain that is no tip has no RPC view of its status: a harness error.
pub fn dashd_parent_failed(prev: &Hash, header: &Value, tips: &Value) -> Result<bool, String> {
    let confirmations = header["confirmations"]
        .as_i64()
        .ok_or_else(|| format!("m-4: getblockheader {} has no confirmations", hex(prev)))?;
    if confirmations >= 0 {
        return Ok(false);
    }
    let row = tips
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["hash"] == hex(prev)))
        .ok_or_else(|| {
            format!(
                "m-4: dashd shows no status for the parent {} (off the active chain, no tip)",
                hex(prev)
            )
        })?;
    Ok(row["status"] == "invalid")
}

/// m-4 (plan §3.5 step 3, M-9): the parent's state the binder uses is what both sides show, not
/// only the model's prediction. dashd shows failed or not; V's index shows all three states.
pub fn parent_state(
    prev: &Hash,
    model: ParentState,
    v: ParentState,
    dashd_failed: bool,
) -> Result<ParentState, String> {
    if v != model || dashd_failed != (model == ParentState::Failed) {
        return Err(format!(
            "m-4: the parent {}: model {model:?}, V {v:?}, dashd failed {dashd_failed}",
            hex(prev)
        ));
    }
    Ok(model)
}

/// The manifest's mnsync check (plan §3.2, D49): dashd must report not synced, and anything
/// but a boolean `IsSynced` is a harness error.
pub fn mnsync_unsynced(status: &Value) -> Result<(), String> {
    match super::dashd::is_synced(status)? {
        false => Ok(()),
        true => Err("mnsync is synced".into()),
    }
}

/// A funding read is chain-only (rev5 §9): `gettxout` must pass `include_mempool = false`, so
/// the mempool overlay never answers it.
pub fn chain_only(method: &str, params: &Value) -> Result<(), String> {
    if method == "gettxout" && params.get(2) != Some(&Value::Bool(false)) {
        return Err(format!("a funding read through the mempool overlay: gettxout {params}"));
    }
    Ok(())
}

/// The fields of a post the planes read, as the shim's JSON spells them.
pub const POST_PLANES: [&str; 8] =
    ["tip", "best_header", "best_invalid", "candidates", "index", "ibd", "tips", "deployments"];

/// A post's [`POST_PLANES`] in the shim's JSON (display-hex hashes, `[height, hash]` blocks; an
/// absent field left out, a null best header null), so a recorded V post decodes as a [`Post`]
/// (a10-4 review r2-n1).
pub fn post_planes(post: &Post) -> Value {
    let at = |b: &BlockAt| json!([b.0, hex(&b.1.0)]);
    let mut out = serde_json::Map::new();
    if let Some(tip) = &post.tip {
        out.insert("tip".into(), at(tip));
    }
    if let Some(best) = &post.best_header {
        out.insert("best_header".into(), best.as_ref().map_or(Value::Null, at));
    }
    if let Some(best) = &post.best_invalid {
        out.insert("best_invalid".into(), best.as_ref().map_or(Value::Null, at));
    }
    if let Some(candidates) = &post.candidates {
        let entry = |c: &Candidate| json!([c.0, hex(&c.1.0), c.2, c.3, c.4]);
        out.insert("candidates".into(), candidates.iter().map(entry).collect());
    }
    if let Some(index) = &post.index {
        out.insert("index".into(), json!(hex(&index.0)));
    }
    if let Some(ibd) = post.ibd {
        out.insert("ibd".into(), json!(ibd));
    }
    if let Some(tips) = &post.tips {
        out.insert("tips".into(), json!(tips));
    }
    if let Some(deployments) = &post.deployments {
        out.insert("deployments".into(), deployments.clone());
    }
    Value::Object(out)
}

/// An op's reply as recorded: `{"result": …}` or `{"error": {"code", "message"}}`.
pub fn outcome_json(outcome: &Outcome) -> Value {
    match outcome {
        Ok(v) => json!({"result": v}),
        Err(e) => json!({"error": {"code": e.code, "message": e.message}}),
    }
}

pub fn outcome_from_json(v: &Value) -> Outcome {
    match v.get("error") {
        Some(e) => Err(dash_oracle_sys::RpcError {
            code: e["code"].as_i64(),
            message: e["message"].as_str().unwrap_or_default().to_owned(),
        }),
        None => Ok(v["result"].clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREV: Hash = [7; 32];

    /// m-4: V's index statuses read as the binder's three parent states.
    #[test]
    fn v_shows_each_parent_state() {
        let tree = 2;
        let scripts = VALID_SCRIPTS | HAVE_DATA;
        assert_eq!(v_parent_state(scripts | FAILED_VALID, false), ParentState::Failed);
        assert_eq!(v_parent_state(tree | 64, true), ParentState::Failed);
        assert_eq!(v_parent_state(tree, true), ParentState::UnmarkedFailedAncestor);
        assert_eq!(v_parent_state(scripts, true), ParentState::Clean);
        assert_eq!(v_parent_state(tree, false), ParentState::Clean);
    }

    /// m-4: dashd's view of the parent, and where it has none.
    #[test]
    fn dashd_shows_a_failed_parent_or_refuses() {
        let tips = |status: &str| json!([{"hash": hex(&PREV), "status": status}]);
        let active = json!({"confirmations": 3});
        let off = json!({"confirmations": -1});
        assert_eq!(dashd_parent_failed(&PREV, &active, &tips("invalid")), Ok(false));
        assert_eq!(dashd_parent_failed(&PREV, &off, &tips("invalid")), Ok(true));
        assert_eq!(dashd_parent_failed(&PREV, &off, &tips("valid-fork")), Ok(false));
        let e = dashd_parent_failed(&PREV, &off, &json!([])).expect_err("no view");
        assert!(e.contains("no status for the parent"), "{e}");
        assert!(dashd_parent_failed(&PREV, &json!({}), &json!([])).is_err());
    }

    /// m-4: the model's state stands only where both sides show it.
    #[test]
    fn the_parent_state_needs_both_sides() {
        use ParentState::{Clean, Failed, UnmarkedFailedAncestor};
        assert_eq!(parent_state(&PREV, Clean, Clean, false), Ok(Clean));
        assert_eq!(parent_state(&PREV, Failed, Failed, true), Ok(Failed));
        assert_eq!(
            parent_state(&PREV, UnmarkedFailedAncestor, UnmarkedFailedAncestor, false),
            Ok(UnmarkedFailedAncestor)
        );
        for (model, v, dashd) in [
            (Clean, UnmarkedFailedAncestor, false),
            (Clean, Clean, true),
            (Failed, Failed, false),
            (UnmarkedFailedAncestor, Clean, false),
        ] {
            let e = parent_state(&PREV, model, v, dashd).expect_err("a disagreement");
            assert!(e.starts_with("m-4: the parent"), "{e}");
        }
    }
}
