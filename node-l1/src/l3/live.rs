//! OD-L3-18's live witnesses (plan §9), against a real regtest dashd v24 and V under
//! `heavy.lock`; recorded-post fixtures are not these witnesses.
//! - **DIP3 parent** ([`dip3_parent`]): on the census arguments (DIP3 at 2000), Core's
//!   templates to height 1999 on both sides. The body for 1999 passes §3.6's gates (the
//!   baseline); Core's template body for 2000, whose parent both sides report with `dip0003`
//!   active, fails the `dip3_parent` cross-check, naming `g.quorum` and `g.dmn`.

use std::path::Path;

use dash_oracle_sys::ChainOp;
use serde_json::{Value, json};

use super::dashd::Process;
use super::leg::{T0, chain_args};
use super::planes::{self, rpc_hash};
use super::trial::{from_hex, hex, to_hex};
use super::version::{Dashd, private_dir};
use crate::chain::build::BlockPlan;
use crate::chain::mutate::Par;
use crate::chain::node::NodeSide;
use crate::chain::world::Hash;

/// The first height the census arguments activate DIP3 at (`-dip3params=2000:2000`).
pub const DIP3_HEIGHT: u32 = 2000;

/// What the DIP3-parent witness saw: the baseline's gates and the failure at DIP3's height.
#[derive(Debug)]
pub struct Dip3Witness {
    pub baseline: Result<(), String>,
    pub at_dip3: Result<(), String>,
    /// Each side's `dip0003` `active` at the parent of [`DIP3_HEIGHT`].
    pub dashd_active: Option<bool>,
    pub v_active: Option<bool>,
}

/// Run the DIP3-parent witness in a new directory under `dir`.
pub fn dip3_parent(dashd: &Dashd, dir: &Path) -> Result<Dip3Witness, String> {
    let args = chain_args(Par::One);
    let node = Process::start(&dashd.binary, &private_dir(dir, "dashd-dip3")?, &args, T0)?;
    let mut v = NodeSide::with_argv(&args, T0, &private_dir(dir, "v-dip3")?, None)?;
    if let Some(message) = v.down() {
        return Err(format!("V's load was refused: {message}"));
    }
    let rpc = |method: &str, params: Value| node.rpc().ok(method, params);
    let mut baseline = Err("not reached".to_owned());
    for h in 1..=DIP3_HEIGHT {
        let mocktime = T0 + 150 * i64::from(h);
        rpc("setmocktime", json!([mocktime]))?;
        let clock = v.step(&ChainOp::SetTime(mocktime)).map_err(|u| format!("V's clock: {u:?}"))?;
        let template = rpc("generateblock", json!(["raw(51)", [], false]))?;
        let bytes = from_hex(template["hex"].as_str().ok_or("no template hex")?)?;
        let plan = BlockPlan::from_block(&bytes)?;
        let gates = gates(&node, &plan, clock.step.post.deployments.clone())?;
        if h == DIP3_HEIGHT - 1 {
            baseline = planes::body_gates(&plan.prev, &gates.0, &gates.1, &gates.2);
        }
        if h == DIP3_HEIGHT {
            let active = |d: &Value| d["deployments"]["dip0003"]["active"].as_bool();
            let witness = Dip3Witness {
                at_dip3: planes::body_gates(&plan.prev, &gates.0, &gates.1, &gates.2),
                dashd_active: active(&gates.0),
                v_active: active(&gates.1),
                baseline,
            };
            node.stop()?;
            return Ok(witness);
        }
        let reply = node.rpc().call("submitblock", json!([to_hex(&bytes)]))?;
        let step = v.step(&ChainOp::Block(bytes)).map_err(|u| format!("V refused {h}: {u:?}"))?;
        if reply.as_ref().ok() != Some(&Value::Null) || step.step.reply.outcome != Ok(Value::Null) {
            return Err(format!("block {h}: dashd {reply:?}, V {:?}", step.step.reply.outcome));
        }
        let best: Hash = rpc_hash(&rpc("getbestblockhash", json!([]))?)?;
        if best != plan.build().hash {
            return Err(format!("block {h} is not dashd's tip ({})", hex(&best)));
        }
    }
    unreachable!("the loop returns at DIP3's height")
}

/// §3.6's reads for a body on the tip: dashd's `getdeploymentinfo` at its parent, V's
/// deployments there (its post after the last op) and dashd's decode of its first transaction.
fn gates(
    node: &Process,
    plan: &BlockPlan,
    v: Option<Value>,
) -> Result<(Value, Value, Value), String> {
    let dashd = node.rpc().ok("getdeploymentinfo", json!([hex(&plan.prev)]))?;
    let first = to_hex(&dash_consensus::core_serialize(&*plan.txs[0].tx()));
    let decoded = node.rpc().ok("decoderawtransaction", json!([first]))?;
    Ok((dashd, v.unwrap_or_default(), decoded))
}
