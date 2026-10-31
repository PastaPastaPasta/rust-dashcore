//! Checks `oracle_dml_apply` and `oracle_dml_block` against `protx listdiff` (node-program
//! O2a and O2a-EV acceptance).
//!
//! Reads samples as JSON lines on stdin (written by `scripts/dml-listdiff-check.py`
//! from a reference dashd's RPC) and, for each pin, rebuilds the masternode list of
//! the sampled block from the list of its parent, renders the difference as
//! `protx listdiff` does and compares it with the RPC's. `oracle_dml_block` (`check_sigs`:
//! the provider transactions' signatures; Core's check loop verifies quorum commitments
//! without theirs) must accept the block with the same list, block hash set; the parent
//! list's payee must be one the coinbase pays, and the new list's SML root the CbTx's.
//! `oracle_verify_commitment` with signatures must accept every non-null commitment over the
//! supplied members and reject it with one member replaced by another. Prints one
//! JSON line per sample and pin: `{"height", "pin", "result": "match" | "mismatch" |
//! "error", ...}`; a mismatch names the `failed` legs.
//!
//! Sample fields: `height`, `block` (hex), `first_height`, `headers` (hex, 80 bytes
//! each, ending at the parent), `prev_list` (hex, `CDeterministicMNList` as evodb
//! stores it), `quorums` (`[{llmq_type, quorum_hash (internal byte order, hex),
//! members: [hex CDeterministicMN]}]`), `bls_legacy`, `adjusted_time`, `chain`,
//! `deployments` (`[[DeploymentPos, ThresholdState]]`), `coins` (`[{txid (RPC byte
//! order), vout, height, coinbase, amount, script}]`), `coins_at_tip` (whether the coins
//! view's best block is the tip, so Core re-checks ProRegTx collaterals), `expected` (the
//! RPC's listdiff JSON) and, in O2a-EV samples, `block_hash` (internal byte order),
//! `cbtx_mn_root` (internal byte order, null without a CbTx) and `payees` (the candidates'
//! proTxHashes in internal byte order, null before DIP3 enforcement).

use std::io::{self, BufRead, Write};

use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, ContextCoin, ListEncoding, MnList, Oracle,
    QuorumMembers, ThresholdState, Verdict,
};
use serde_json::{Value, json};

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn hash32(s: &str) -> [u8; 32] {
    unhex(s).try_into().expect("32-byte hash")
}

/// A hash as the RPC prints it (byte-reversed), in internal byte order.
fn rpc_hash(s: &str) -> [u8; 32] {
    let mut h = hash32(s);
    h.reverse();
    h
}

/// The listdiff JSON without what the oracle cannot produce or orders differently:
/// the two heights, `collateralAddress` (a transaction-index lookup) and the order of
/// each array (BuildDiff iterates internal ids, which the RPC's JSON does not carry).
fn normalize(mut diff: Value) -> Value {
    let obj = diff.as_object_mut().expect("listdiff object");
    obj.remove("baseHeight");
    obj.remove("blockHeight");
    let key = |v: &Value| -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Object(o) => o
                .get("proTxHash")
                .and_then(Value::as_str)
                .map_or_else(|| o.keys().next().cloned().unwrap_or_default(), str::to_string),
            other => other.to_string(),
        }
    };
    for (name, list) in obj.iter_mut() {
        let items = list.as_array_mut().unwrap_or_else(|| panic!("{name} is not an array"));
        if name == "addedMNs" {
            for mn in items.iter_mut() {
                let mn = mn.as_object_mut().expect("added entry");
                mn.remove("collateralAddress");
                if let Some(state) = mn.get_mut("state") {
                    drop_legacy_service(state);
                }
            }
        }
        if name == "updatedMNs" {
            for entry in items.iter_mut() {
                for state_diff in entry.as_object_mut().expect("updated entry").values_mut() {
                    drop_legacy_service(state_diff);
                }
            }
        }
        items.sort_by_key(key);
    }
    diff
}

/// v23.1.8 also prints `service`, the deprecated copy of the first `addresses.core_p2p`
/// entry (v24 prints it only with `-deprecatedrpc=service`), `[::]:0` when there is none.
/// Dropped when it is that copy.
fn drop_legacy_service(state: &mut Value) {
    let Some(obj) = state.as_object_mut() else {
        return;
    };
    let first_p2p =
        obj.get("addresses").and_then(|a| a.get("core_p2p")).and_then(|p| p.get(0)).cloned();
    let empty = Value::String("[::]:0".into());
    if obj
        .get("service")
        .is_some_and(|s| Some(s) == first_p2p.as_ref() || (first_p2p.is_none() && *s == empty))
    {
        obj.remove("service");
    }
}

fn check(oracle: &Oracle, sample: &Value) -> Value {
    let field = |name: &str| sample.get(name).unwrap_or_else(|| panic!("sample lacks {name}"));
    let chain = Chain::new(field("chain").as_str().expect("chain"), &[]);
    let headers = unhex(field("headers").as_str().expect("headers"));
    let prev_list = unhex(field("prev_list").as_str().expect("prev_list"));
    // Samples from before the at-tip re-run carry no coins and ran the Unchecked view.
    let coins: Vec<ContextCoin> =
        sample.get("coins").and_then(Value::as_array).map_or_else(Vec::new, |list| {
            list.iter()
                .map(|c| ContextCoin {
                    txid: rpc_hash(c["txid"].as_str().expect("txid")),
                    vout: c["vout"].as_u64().expect("vout") as u32,
                    height: c["height"].as_u64().expect("height") as u32,
                    coinbase: c["coinbase"].as_bool().expect("coinbase"),
                    amount: c["amount"].as_i64().expect("amount"),
                    script_pubkey: unhex(c["script"].as_str().expect("script")),
                })
                .collect()
        });
    let coins = if sample.get("coins_at_tip").and_then(Value::as_bool).unwrap_or(false) {
        CoinsView::AtTip(coins)
    } else {
        CoinsView::Unchecked(coins)
    };
    let deployments =
        sample.get("deployments").and_then(Value::as_array).map_or_else(Vec::new, |list| {
            list.iter()
                .map(|d| {
                    let state = ThresholdState::try_from(d[1].as_u64().expect("state") as u8)
                        .unwrap_or_else(|s| panic!("unknown deployment state {s}"));
                    (d[0].as_u64().expect("deployment") as u8, state)
                })
                .collect()
        });
    let quorums = field("quorums")
        .as_array()
        .expect("quorums")
        .iter()
        .map(|q| QuorumMembers {
            llmq_type: q["llmq_type"].as_u64().expect("llmq_type") as u8,
            quorum_hash: hash32(q["quorum_hash"].as_str().expect("quorum_hash")),
            members: q["members"]
                .as_array()
                .expect("members")
                .iter()
                .map(|m| unhex(m.as_str().unwrap()))
                .collect(),
        })
        .collect();
    let ctx = Context::new(
        field("first_height").as_i64().expect("first_height") as i32,
        headers.chunks(80).map(|h| h.try_into().expect("80-byte header")).collect(),
        field("bls_legacy").as_bool().expect("bls_legacy"),
        deployments,
        field("adjusted_time").as_i64().expect("adjusted_time"),
        // dml_apply runs no header check, the only reader of checkpoints.
        Checkpoints::Disabled,
        coins,
        MnList::Supplied(prev_list.clone()),
        quorums,
    );
    let block = unhex(field("block").as_str().expect("block"));
    let result = match oracle.dml_apply(&chain, &block, &ctx) {
        Err(e) => return json!({"result": "error", "error": e.to_string()}),
        Ok(r) => r,
    };
    let new_list = match (&result.verdict, result.new_list) {
        (Verdict::Accept, Some(list)) => list,
        (verdict, _) => {
            return json!({"result": "error", "error": format!("rejected: {verdict:?}")});
        }
    };
    let ours = match oracle.dml_listdiff_json(&chain, &prev_list, &new_list) {
        Err(e) => return json!({"result": "error", "error": e.to_string()}),
        Ok(json) => normalize(serde_json::from_str(&json).expect("oracle JSON")),
    };
    let expected = normalize(field("expected").clone());
    let mut failed = vec![];
    let mut legs = vec!["listdiff"];
    let mut out = json!({});
    if ours != expected {
        failed.push("listdiff");
        out["oracle"] = ours;
        out["expected"] = expected;
    }
    if sample.get("block_hash").is_some() {
        let applied = result.canonical.expect("accepted dml_apply has canonical bytes");
        if let Err(e) = check_item5(
            oracle,
            &chain,
            sample,
            &block,
            &ctx,
            &applied,
            result.payee,
            &mut legs,
            &mut failed,
            &mut out,
        ) {
            return json!({"result": "error", "error": e.to_string()});
        }
    }
    out["legs"] = json!(legs);
    out["result"] = json!(if failed.is_empty() {
        "match"
    } else {
        "mismatch"
    });
    if !failed.is_empty() {
        out["failed"] = json!(failed);
    }
    out
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The extra payloads of a block's quorum commitment transactions (version 3, type 6), from
/// the raw block: each is a `CFinalCommitmentTxPayload` (nVersion u16, nHeight u32, then the
/// commitment).
fn commitment_payloads(block: &[u8]) -> Vec<&[u8]> {
    let mut at = 80;
    let compact = |at: &mut usize| -> usize {
        let (n, len) = match block[*at] {
            0xfd => (u16::from_le_bytes(block[*at + 1..*at + 3].try_into().unwrap()) as usize, 3),
            0xfe => (u32::from_le_bytes(block[*at + 1..*at + 5].try_into().unwrap()) as usize, 5),
            0xff => (u64::from_le_bytes(block[*at + 1..*at + 9].try_into().unwrap()) as usize, 9),
            n => (n as usize, 1),
        };
        *at += len;
        n
    };
    let mut payloads = Vec::new();
    for _ in 0..compact(&mut at) {
        let version = u16::from_le_bytes([block[at], block[at + 1]]);
        let tx_type = u16::from_le_bytes([block[at + 2], block[at + 3]]);
        at += 4;
        for _ in 0..compact(&mut at) {
            at += 36;
            at += compact(&mut at) + 4;
        }
        for _ in 0..compact(&mut at) {
            at += 8;
            at += compact(&mut at);
        }
        at += 4;
        if version >= 3 && tx_type != 0 {
            let len = compact(&mut at);
            if tx_type == 6 {
                payloads.push(&block[at..at + len]);
            }
            at += len;
        }
    }
    assert_eq!(at, block.len(), "block parse");
    payloads
}

/// `oracle_verify_commitment(check_sigs = true)` on every non-null commitment of the block (the
/// ones the context supplies members for), which `oracle_dml_block` does not do: Core's check
/// loop verifies commitments without signatures (`CheckLLMQCommitment` calls `Verify(...,
/// false)`) and leaves them to the quorum block processor. The control replaces a signer's key
/// with another member's and must be rejected, so the supplied members are the signing ones.
fn check_commitments(
    oracle: &Oracle,
    chain: &Chain,
    block: &[u8],
    ctx: &Context,
    legs: &mut Vec<&'static str>,
    failed: &mut Vec<&'static str>,
    out: &mut Value,
) -> dash_oracle_sys::Result<()> {
    let mut verified = Vec::new();
    for payload in commitment_payloads(block) {
        let qc = &payload[6..];
        let Some(index) =
            ctx.quorums.iter().position(|q| q.llmq_type == qc[2] && q.quorum_hash[..] == qc[3..35])
        else {
            continue; // a null commitment (the context supplies members only for the others)
        };
        if !legs.contains(&"qc_sigs") {
            legs.extend(["qc_sigs", "qc_control"]);
        }
        let verdict = oracle.verify_commitment(chain, qc, ctx, true)?;
        // Only signers' keys enter the aggregate: the signers bitset follows the quorum hash
        // (a non-rotated commitment, as the context supplies), a CompactSize size then the bits.
        let (size, bits) = match qc[35] {
            0xfd => (u16::from_le_bytes([qc[36], qc[37]]) as usize, &qc[38..]),
            n => (n as usize, &qc[36..]),
        };
        let signer = (0..size)
            .find(|i| bits[i / 8] >> (i % 8) & 1 == 1)
            .expect("a non-null commitment has signers");
        let mut changed = ctx.clone();
        let members = &mut changed.quorums[index].members;
        members[signer] = members[(signer + 1) % members.len()].clone();
        let control = oracle.verify_commitment(chain, qc, &changed, true)?;
        if !verdict.is_accept() && !failed.contains(&"qc_sigs") {
            failed.push("qc_sigs");
        }
        if control.is_accept() && !failed.contains(&"qc_control") {
            failed.push("qc_control");
        }
        verified.push(json!({
            "quorum_hash": to_hex(&qc[3..35]),
            "verdict": format!("{verdict:?}"),
            "control_member": signer,
            "control_accepted": control.is_accept(),
        }));
    }
    if !verified.is_empty() {
        out["commitments"] = json!(verified);
    }
    Ok(())
}

/// The O2a-EV legs: `oracle_dml_block` agrees with `oracle_dml_apply`, the payee is one the
/// coinbase pays, the new list's SML root is the CbTx's, and every non-null commitment's
/// signatures verify over the supplied members (with a changed-member control).
#[allow(clippy::too_many_arguments)]
fn check_item5(
    oracle: &Oracle,
    chain: &Chain,
    sample: &Value,
    block: &[u8],
    ctx: &Context,
    applied: &[u8],
    payee: Option<[u8; 32]>,
    legs: &mut Vec<&'static str>,
    failed: &mut Vec<&'static str>,
    out: &mut Value,
) -> dash_oracle_sys::Result<()> {
    check_commitments(oracle, chain, block, ctx, legs, failed, out)?;
    legs.push("dml_block");
    let got = oracle.dml_block(chain, block, ctx, true, false)?;
    let Some((canonical, _)) = got.list.as_ref().filter(|_| got.verdict.is_accept()) else {
        failed.push("dml_block");
        out["dml_block"] = json!(format!("{:?} at {:?}", got.verdict, got.stage));
        return Ok(());
    };
    let block_hash = hash32(sample["block_hash"].as_str().expect("block_hash"));
    if canonical[..32] != block_hash || canonical[32..] != applied[32..] || got.payee != payee {
        failed.push("dml_block");
        out["dml_block"] = json!({"canonical": to_hex(canonical), "applied": to_hex(applied)});
    }
    if let Some(candidates) = sample["payees"].as_array() {
        legs.push("payee");
        let ours = payee.map(|p| to_hex(&p));
        if !candidates.iter().any(|c| c.as_str() == ours.as_deref()) {
            failed.push("payee");
            out["payee"] = json!({"oracle": ours, "candidates": candidates});
        }
    }
    if let Some(root) = sample["cbtx_mn_root"].as_str() {
        legs.push("sml_root");
        let sml = oracle.sml_merkle_root(chain, ListEncoding::Canonical, canonical)?;
        if !sml.verdict.is_accept() || sml.root != hash32(root) {
            failed.push("sml_root");
            out["sml_root"] =
                json!({"oracle": to_hex(&sml.root), "verdict": format!("{:?}", sml.verdict)});
        }
    }
    Ok(())
}

fn main() {
    let oracles: Vec<Oracle> = Oracle::all().collect();
    let stdout = io::stdout();
    for line in io::stdin().lock().lines() {
        let line = line.expect("stdin");
        if line.trim().is_empty() {
            continue;
        }
        let sample: Value = serde_json::from_str(&line).expect("sample JSON");
        for oracle in &oracles {
            let mut out = check(oracle, &sample);
            out["height"] = sample["height"].clone();
            out["pin"] = json!(oracle.pin().name());
            let mut lock = stdout.lock();
            writeln!(lock, "{out}").expect("stdout");
            lock.flush().expect("stdout");
        }
    }
}
