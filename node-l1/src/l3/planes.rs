//! The planes' checks (plan §3.3, §3.6), pure: each takes what a leg read from dashd after an
//! op and V's post (or Core's, in the replay), so recorded posts drive their witnesses
//! (review r1 M-2). The leg fetches; these decide.

use dash_oracle_sys::{BlockAt, ChainHash, Post};
use serde_json::{Value, json};

use super::inventory::Effects;
use super::trial::hex;
use crate::chain::shape::Outcome;
use crate::chain::world::{Hash, display_hash};

/// `BLOCK_HAVE_DATA`, `BLOCK_FAILED_VALID | BLOCK_FAILED_CHILD` (chain.h).
pub const HAVE_DATA: u32 = 8;
pub const FAILED: u32 = 32 | 64;

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
    let row = tips
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["hash"] == hex(hash)))
        .ok_or_else(|| format!("{} is known but no chain tip", hex(hash)))?;
    Ok(Effects {
        known,
        failed: row["status"] == "invalid",
        stored,
    })
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

/// P7 in the replay (M-11): Core's best header and index digest are V's; where the leg
/// recorded the model's best header, Core and V agreeing on another is a harness error.
pub fn p7_replay(
    at: usize,
    core: &Post,
    v: &Post,
    model: Option<(u32, Hash)>,
) -> Result<(), String> {
    if core.best_header != v.best_header || core.index != v.index {
        return Err(format!(
            "P7 diverges at op {at}: Core best header {:?} index {:?}, V {:?} {:?}",
            core.best_header, core.index, v.best_header, v.index
        ));
    }
    if let Some((height, hash)) = model
        && core.best_header != Some(Some(block_at(height, hash)))
    {
        return Err(format!(
            "harness error at op {at}: the model's best header ({height}, {}) disagrees with Core and V {:?}",
            hex(&hash),
            core.best_header
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
    for name in ["dip0003", "v24"] {
        if active(dashd, name) != Some(false) || active(v, name) != Some(false) {
            return Err(format!(
                "§3.6: {name} at {}: dashd {:?}, V {:?} (V's evo hooks run DIP3-gated bodies there: \
                 dashd-chainstate evo/ev1.rs and evo/node.rs)",
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
