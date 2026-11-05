//! The devnet leg's start (plan §7.2, OD-L3-22): Core builds a devnet's block 1 itself
//! (`FindDevNetGenesisBlock`, 50 DASH to `OP_RETURN`, chainparams.cpp:646) and accepts it when
//! it loads a fresh datadir (validation.cpp:5428-5436). With Core's default `nHighSubsidyBlocks`
//! of 0 the devnet's fixed base (5 DASH) applies from height 1, so ConnectBlock refuses Core's
//! own block 1 (`bad-cb-amount`) and nothing can follow it (`devnet-genesis`, v:4325-4329). Both
//! sides of the leg therefore run [`ARGS`]; op 0 checks both start on block 1.

use serde_json::{Value, json};

use super::dashd::Process;
use super::planes::{FAILED, HAVE_DATA, rpc_hash};
use super::trial::hex;
use crate::chain::node::NodeSide;
use crate::chain::world::Hash;

/// The devnet's name and OD-L3-22's subsidy arguments, on both sides.
pub const ARGS: [&str; 3] = ["-devnet=l3", "-highsubsidyblocks=1", "-highsubsidyfactor=1"];

/// Block 1 of `-devnet=l3`, as dashd v24.0.0-rc.4 (sha256 e1be2564…) built and served it in the
/// devnet genesis probes of 10-31 (A10-5-NOTES.md): v4, time 1417713338, nonce 1.
pub const BLOCK_1: &str = "1cbf05316c03a1216e3d7db9380da0aed84c61e0733b75335095c02daa4fb121";

/// dashd's chain tips once its load has settled: polled until a height-1 tip shows, so the
/// startup's ActivateBestChain has connected or refused block 1.
pub fn dashd_tips(dashd: &Process) -> Result<Vec<Value>, String> {
    let start = std::time::Instant::now();
    loop {
        let tips = dashd.rpc().ok("getchaintips", json!([]))?;
        let tips = tips.as_array().cloned().ok_or("getchaintips: not an array")?;
        if tips.iter().any(|t| t["height"] == 1) {
            return Ok(tips);
        }
        if start.elapsed() > std::time::Duration::from_secs(60) {
            return Err(format!("dashd shows no block 1 after 60 s: {tips:?}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// `BLOCK_VALID_MASK`, `BLOCK_VALID_SCRIPTS` (chain.h).
const VALID_MASK: u32 = 7;
const VALID_SCRIPTS: u32 = 5;

/// V's block 1, its hash and status from its index, once V's load has settled on it: polled
/// until it is failed or connected (VALID_SCRIPTS), since V's startup activation may still be
/// running when the load returns.
pub fn v_block_1(v: &NodeSide) -> Result<(Hash, u32), String> {
    let start = std::time::Instant::now();
    loop {
        let entries = v.index().ok_or("V's index is not readable")?;
        let ones: Vec<_> = entries.iter().filter(|e| e.height == 1).collect();
        if let [e] = ones.as_slice()
            && (e.status & FAILED != 0 || e.status & VALID_MASK >= VALID_SCRIPTS)
        {
            return Ok((*e.hash.as_bytes(), e.status));
        }
        if ones.len() > 1 || start.elapsed() > std::time::Duration::from_secs(60) {
            let found: Vec<_> = ones.iter().map(|e| (hex(e.hash.as_bytes()), e.status)).collect();
            return Err(format!("V has not settled on one block 1: {found:?}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Op 0 (OD-L3-22): V loaded [`ARGS`]' subsidy parameters, and both tips are [`BLOCK_1`].
pub fn op0(dashd: &Process, v: &NodeSide) -> Result<(), String> {
    let c = &v.params().consensus;
    if (c.high_subsidy_blocks, c.high_subsidy_factor) != (1, 1) {
        return Err(format!(
            "op 0: V's highsubsidyblocks/factor are {}/{}, not 1/1",
            c.high_subsidy_blocks, c.high_subsidy_factor
        ));
    }
    let want = rpc_hash(&json!(BLOCK_1))?;
    let tips = dashd_tips(dashd)?;
    let best = rpc_hash(&dashd.rpc().ok("getbestblockhash", json!([]))?)?;
    let active = tips.iter().any(|t| t["height"] == 1 && t["status"] == "active");
    if best != want || !active || tips.len() != 1 {
        return Err(format!("op 0: dashd's tip is {} (tips {tips:?}), not block 1", hex(&best)));
    }
    let (hash, status) = v_block_1(v)?;
    if hash != want || status & FAILED != 0 || status & VALID_MASK < VALID_SCRIPTS {
        return Err(format!("op 0: V's block 1 is {} with status {status}", hex(&hash)));
    }
    Ok(())
}

/// The default-args negative (OD-L3-22's regression witness, O10-11's devnet-genesis
/// negative): dashd marks its block 1 invalid with `bad-cb-amount` and stays at genesis, and V
/// does the same: its block 1 is the same block, stored and marked failed, and V's log gives the
/// same reason for it. `v_log` is V's log lines, read after [`v_block_1`] settled; `dashd_log`
/// dashd's debug.log.
pub fn default_args_refuse_block_1(
    dashd: &Process,
    dashd_log: &str,
    v: &NodeSide,
    v_log: &[String],
) -> Result<(), String> {
    let want = rpc_hash(&json!(BLOCK_1))?;
    let tips = dashd_tips(dashd)?;
    let row = tips.iter().find(|t| t["height"] == 1).expect("a height-1 tip");
    let count = dashd.rpc().ok("getblockcount", json!([]))?;
    if rpc_hash(&row["hash"])? != want || row["status"] != "invalid" || count != 0 {
        return Err(format!("dashd: block 1 {row}, block count {count}"));
    }
    let reason = format!("ConnectTip: ConnectBlock {BLOCK_1} failed, bad-cb-amount");
    if !dashd_log.contains(&reason) {
        return Err(format!("dashd's debug.log has no \"{reason}\""));
    }
    let (hash, status) = v_block_1(v)?;
    if hash != want || status & FAILED == 0 || status & HAVE_DATA == 0 {
        return Err(format!("V's block 1 is {} with status {status}", hex(&hash)));
    }
    if !v_log.iter().any(|l| l.contains(&reason)) {
        return Err(format!("V's log has no \"{reason}\": {v_log:?}"));
    }
    Ok(())
}
