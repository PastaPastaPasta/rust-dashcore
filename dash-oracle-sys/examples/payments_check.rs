//! Checks `oracle_block_value_payee` and `oracle_credit_pool_step` against mined blocks
//! (node-program O2a-GV acceptance).
//!
//! Reads samples as JSON lines on stdin, written by `scripts/payments-check.py` from a
//! reference dashd's RPC, and prints one JSON result line per payments sample or credit-pool
//! step: `{"height", "result": "match" | "mismatch" | "skipped" | "error", "legs", "failed"?,
//! "skipped"?}`. `skipped`: the legs that ran matched, but the superblock legs did not run
//! because the coinbase's superblock payments could not be told apart (testnet miners split
//! their reward, or a payment has no address).
//!
//! Payments samples (`kind: "payments"`): `chain`, `height`, `tip_header` (hex, the parent's),
//! `block` (hex), `fee` (duffs), `prev_list` (hex, the parent's list as evodb stores it, null
//! before DIP3), `deployments`, `adjusted_time`, `bls_legacy`, `superblock`, `enforced` (DIP3
//! enforcement), and for a superblock with a list, `voters` (`[collateral (internal byte
//! order), vout, weight]` reaching the funding threshold) and `addresses` (the coinbase's
//! output addresses). Each sample runs these legs:
//! - `ibd`: not synced, governance loaded, no triggers: Core accepts (`NoCheck`, R4 F3);
//! - `synced`: synced, governance loaded. Off superblock heights, no triggers: accepted with a
//!   check level other than `NoCheck`. At a superblock with a list, the trigger rebuilt from
//!   the coinbase's superblock payments, funded by `voters`: accepted, triggered, and
//!   `GetSuperblockPayments()` equals those outputs. At a superblock without a list, Core
//!   cannot see a trigger: `bad-cb-amount`, "no triggered superblock detected";
//! - `masternode` (from DIP3 enforcement): `GetBlockTxOuts()` succeeds and each of its outputs
//!   matches a distinct coinbase output (miners order outputs differently);
//! - crafted rejects at real blocks: `payee_missing` (the first masternode output removed:
//!   `bad-cb-payee`), `regular_overpay` (off superblocks after the budgets, one duff above the
//!   reward: "only regular blocks are allowed at this height"), `budget_overpay` (before the
//!   superblocks: "old budgets are not activated yet", "old budgets are disabled" in a window
//!   when synced, "block is not in old budget cycle window"), and at superblocks with a list
//!   `sb_unfunded` (no votes: "no triggered superblock detected"), `sb_wrong` (a payment one
//!   duff off: "invalid superblock detected"), `sb_over_max` (above the superblock limit:
//!   "exceeded superblock max value"), `sb_unloaded` (governance not loaded: accepted) and
//!   `sb_chainlocked` (synced but chainlocked: `NoCheck`, accepted).
//!
//! Credit-pool lines: `credit-start` (`chain`, `height`, `prev` pool or null for the empty
//! one, `deployments`), 576 `credit-prime` lines (`height`, `block`) below it, then one
//! `credit-step` per block (`height`, `block`, `expected` pool from `getcreditpoolinfo`). Each
//! step runs with the 576 headers below it and the window-start block on disk, must accept
//! with `expected == CbTx creditPoolBalance` and give `getcreditpoolinfo`'s pool; its pool is
//! the next step's prev.

use std::collections::VecDeque;
use std::io::{self, BufRead, Write};

use dash_oracle_sys::payments::{VOTE_OUTCOME_YES, VOTE_SIGNAL_FUNDING};
use dash_oracle_sys::{
    BlockValuePayee, Chain, Checkpoints, CoinsView, Context, CreditPool, GovVote, MnList, Oracle,
    PaymentsContext, Pin, StepContext, SuperblockCheck, ThresholdState, Trigger, TxOut, Verdict,
};
use dashcore::Block;
use dashcore::consensus::{deserialize, serialize};
use serde_json::{Value, json};

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn deployments(v: &Value) -> Vec<(u8, ThresholdState)> {
    v.as_array()
        .expect("deployments")
        .iter()
        .map(|d| {
            let state =
                ThresholdState::try_from(d[1].as_u64().expect("state") as u8).expect("state");
            (d[0].as_u64().expect("pos") as u8, state)
        })
        .collect()
}

fn context(sample: &Value, headers: Vec<[u8; 80]>, first_height: i32, list: MnList) -> Context {
    Context::new(
        first_height,
        headers,
        sample["bls_legacy"].as_bool().unwrap_or(false),
        deployments(&sample["deployments"]),
        sample["adjusted_time"].as_i64().unwrap_or(0),
        Checkpoints::Disabled,
        CoinsView::Unchecked(vec![]),
        list,
        vec![],
    )
}

fn outs(block: &Block) -> Vec<TxOut> {
    block.txdata[0]
        .output
        .iter()
        .map(|o| TxOut {
            value: o.value as i64,
            script_pubkey: o.script_pubkey.to_bytes(),
        })
        .collect()
}

/// A verdict's reject reason, or "accept".
fn reason(v: &Verdict) -> String {
    match v {
        Verdict::Accept => "accept".into(),
        Verdict::Reject(r) => r.reason.clone(),
    }
}

/// `out` with `result` (`match` unless some leg `failed`) and the failed legs.
fn with_result(mut out: Value, failed: Vec<Value>) -> Value {
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

struct Legs {
    legs: Vec<&'static str>,
    failed: Vec<Value>,
    /// Why the superblock legs did not run: the coinbase's superblock payments could not be
    /// told apart (the harness's limit, not a verdict).
    skipped: Option<String>,
}

impl Legs {
    fn check(&mut self, leg: &'static str, ok: bool, detail: impl FnOnce() -> Value) {
        self.legs.push(leg);
        if !ok {
            self.failed.push(json!({"leg": leg, "detail": detail()}));
        }
    }

    /// A crafted block rejected with `reason`, and, for `bad-cb-amount`, `value_error` naming
    /// `why`.
    fn reject(&mut self, leg: &'static str, r: &BlockValuePayee, expected: &str, why: &str) {
        let ok = reason(&r.verdict) == expected && r.value_error.contains(why);
        self.check(
            leg,
            ok,
            || json!({"verdict": reason(&r.verdict), "value_error": r.value_error}),
        );
    }
}

fn payments(oracle: &Oracle, sample: &Value) -> Result<Value, String> {
    let chain = Chain::new(sample["chain"].as_str().expect("chain"), &[]);
    let height = sample["height"].as_i64().expect("height") as i32;
    let header: [u8; 80] =
        unhex(sample["tip_header"].as_str().expect("tip_header")).try_into().expect("80 bytes");
    let list = match sample["prev_list"].as_str() {
        Some(list) => MnList::Supplied(unhex(list)),
        None => MnList::Absent,
    };
    let has_list = matches!(list, MnList::Supplied(_));
    let ctx = context(sample, vec![header], height - 1, list);
    let raw = unhex(sample["block"].as_str().expect("block"));
    let block: Block = deserialize(&raw).map_err(|e| e.to_string())?;
    let fee = sample["fee"].as_i64().expect("fee");
    let coinbase = outs(&block);
    let value_out: i64 = coinbase.iter().map(|o| o.value).sum();
    let runner = Runner {
        oracle,
        chain,
        ctx,
    };
    let base = PaymentsContext {
        fee_reward: fee,
        synced: false,
        best_chainlock_height: None,
        governance_loaded: true,
        triggers: vec![],
    };
    let synced = PaymentsContext {
        synced: true,
        ..base.clone()
    };
    let mut legs = Legs {
        legs: vec![],
        failed: vec![],
        skipped: None,
    };

    let ibd = runner.run(&raw, &base)?;
    legs.check(
        "ibd",
        ibd.verdict.is_accept() && ibd.check == SuperblockCheck::NoCheck,
        || json!({"verdict": reason(&ibd.verdict), "value_error": ibd.value_error}),
    );
    let reward = ibd.block_subsidy + fee;
    let superblock = sample["superblock"].as_bool().expect("superblock");
    let enforced = sample["enforced"].as_bool().expect("enforced");

    if enforced {
        let matched = match_outputs(&coinbase, &ibd.masternode);
        legs.check(
            "masternode",
            ibd.outputs_computed && ibd.txouts_ok && matched.is_some(),
            || {
                json!({"outputs_computed": ibd.outputs_computed, "txouts_ok": ibd.txouts_ok,
                       "expected": format!("{:?}", ibd.masternode)})
            },
        );
        if let Some(&first) = matched.as_ref().and_then(|m| m.first()) {
            let mut crafted = block.clone();
            crafted.txdata[0].output.remove(first);
            let r = runner.run(&serialize(&crafted), &base)?;
            legs.reject("payee_missing", &r, "bad-cb-payee", "");
        }
    }

    // One duff above what the block may pay off superblocks (the block itself if it pays more
    // already, as old-budget blocks do).
    let overpay = |extra: i64| -> Vec<u8> {
        if value_out > reward {
            return raw.clone();
        }
        let mut crafted = block.clone();
        crafted.txdata[0].output[0].value += (reward - value_out + extra) as u64;
        serialize(&crafted)
    };
    let main = sample["chain"] == "main";
    let budget_start = if main {
        328008
    } else {
        4100
    };
    let superblock_start = if main {
        614820
    } else {
        4200
    };
    if height < budget_start {
        let r = runner.run(&overpay(1), &base)?;
        legs.reject("budget_overpay", &r, "bad-cb-amount", "old budgets are not activated yet");
    } else if height < superblock_start {
        let (cycle, window) = if main {
            (16616, 100)
        } else {
            (50, 10)
        };
        let r = runner.run(&overpay(1), &synced)?;
        if height % cycle < window {
            legs.reject("budget_overpay", &r, "bad-cb-amount", "old budgets are disabled");
            // R4 F3 / MNO-12: syncing, a window block may pay anything.
            let r = runner.run(&overpay(1), &base)?;
            legs.check("budget_window_ibd", r.verdict.is_accept(), || json!(reason(&r.verdict)));
        } else {
            legs.reject(
                "budget_overpay",
                &r,
                "bad-cb-amount",
                "block is not in old budget cycle window",
            );
        }
    } else if !superblock {
        let r = runner.run(&overpay(1), &base)?;
        legs.reject(
            "regular_overpay",
            &r,
            "bad-cb-amount",
            "only regular blocks are allowed at this height",
        );
    }

    // Synced, a block that pays above the reward is rejected (an old-budget block, or a
    // superblock with no trigger Core can see).
    let expected = if value_out > reward {
        "bad-cb-amount"
    } else {
        "accept"
    };
    if !superblock {
        let s = runner.run(&raw, &synced)?;
        legs.check("synced", reason(&s.verdict) == expected && s.check != SuperblockCheck::NoCheck, || {
            json!({"verdict": reason(&s.verdict), "check": format!("{:?}", s.check), "value_error": s.value_error})
        });
    } else if !has_list || sample["voters"].as_array().is_none_or(Vec::is_empty) {
        let s = runner.run(&raw, &synced)?;
        legs.check(
            "synced",
            reason(&s.verdict) == expected
                && (expected == "accept"
                    || s.value_error.contains("no triggered superblock detected")),
            || json!({"verdict": reason(&s.verdict), "value_error": s.value_error}),
        );
    } else {
        superblock_legs(&mut legs, &runner, sample, &block, &ibd, reward, &synced)?;
    }

    let mut out = with_result(json!({"legs": legs.legs, "superblock": superblock}), legs.failed);
    if let Some(why) = legs.skipped {
        if out["result"] == "match" {
            out["result"] = json!("skipped");
        }
        out["skipped"] = json!(why);
    }
    Ok(out)
}

/// The entry on one sample's chain and context.
struct Runner<'a> {
    oracle: &'a Oracle,
    chain: Chain,
    ctx: Context,
}

impl Runner<'_> {
    fn run(&self, raw: &[u8], pay: &PaymentsContext) -> Result<BlockValuePayee, String> {
        self.oracle
            .block_value_payee(&self.chain, raw, &self.ctx, pay)
            .map_err(|e| format!("{e:?}"))
    }
}

/// A superblock coinbase's outputs: the miner's index, the superblock payments' indexes and
/// their `(address, value)`.
struct SuperblockOutputs {
    miner: usize,
    indexes: Vec<usize>,
    payments: Vec<(String, i64)>,
}

/// The superblock payments are the outputs left after the masternode outputs Core expects (one
/// matching output each; pre-enforcement superblocks may pay none) and the miner's. Miners
/// order outputs differently, so the miner's is the one paying what FillBlockPayments() leaves
/// it (reward - masternode payments), else the only one within that. Errs when neither tells
/// it, or a payment has no address.
fn superblock_outputs(
    coinbase: &[TxOut],
    addresses: &[Option<String>],
    masternode: &[TxOut],
    reward: i64,
) -> Result<SuperblockOutputs, String> {
    let mn = match_outputs(coinbase, masternode).unwrap_or_default();
    let miner_value = reward - mn.iter().map(|&i| coinbase[i].value).sum::<i64>();
    let rest: Vec<usize> = (0..coinbase.len()).filter(|i| !mn.contains(i)).collect();
    let mut miner: Vec<usize> =
        rest.iter().copied().filter(|&i| coinbase[i].value == miner_value).collect();
    if miner.len() != 1 {
        miner = rest.iter().copied().filter(|&i| coinbase[i].value <= miner_value).collect();
    }
    if miner.len() != 1 {
        return Err(format!("cannot tell the miner output among {miner:?}"));
    }
    let indexes: Vec<usize> = rest.into_iter().filter(|&i| i != miner[0]).collect();
    let mut payments = Vec::new();
    for &i in &indexes {
        let address = addresses[i].clone().ok_or_else(|| format!("output {i} has no address"))?;
        payments.push((address, coinbase[i].value));
    }
    Ok(SuperblockOutputs {
        miner: miner[0],
        indexes,
        payments,
    })
}

/// The superblock legs: the trigger rebuilt from the coinbase and its crafted variants.
fn superblock_legs(
    legs: &mut Legs,
    runner: &Runner,
    sample: &Value,
    block: &Block,
    ibd: &BlockValuePayee,
    reward: i64,
    synced: &PaymentsContext,
) -> Result<(), String> {
    let height = sample["height"].as_i64().expect("height") as i32;
    let coinbase = outs(block);
    let addresses: Vec<Option<String>> = sample["addresses"]
        .as_array()
        .expect("addresses")
        .iter()
        .map(|a| a.as_str().map(str::to_string))
        .collect();
    let SuperblockOutputs {
        miner,
        indexes,
        payments,
    } = match superblock_outputs(&coinbase, &addresses, &ibd.masternode, reward) {
        Ok(found) => found,
        Err(why) => {
            legs.skipped = Some(why);
            return Ok(());
        }
    };
    if payments.is_empty() {
        // A superblock height no trigger funded (testnet mines many): the block reward, untriggered.
        let s = runner.run(&serialize(block), synced)?;
        legs.check("synced", s.verdict.is_accept() && !s.triggered, || json!(reason(&s.verdict)));
        return Ok(());
    }
    let votes: Vec<GovVote> = sample["voters"]
        .as_array()
        .expect("voters")
        .iter()
        .map(|v| GovVote {
            collateral_txid: unhex(v[0].as_str().expect("collateral"))
                .try_into()
                .expect("32 bytes"),
            collateral_vout: v[1].as_u64().expect("vout") as u32,
            signal: VOTE_SIGNAL_FUNDING,
            outcome: VOTE_OUTCOME_YES,
        })
        .collect();
    let with = |trigger: Trigger| PaymentsContext {
        triggers: vec![trigger],
        ..synced.clone()
    };
    let raw = serialize(block);
    let funded = with(Trigger::superblock(height, &payments, votes.clone()));
    let s = runner.run(&raw, &funded)?;
    let expected: Vec<TxOut> = indexes.iter().map(|&i| coinbase[i].clone()).collect();
    let ok = s.verdict.is_accept()
        && s.triggered
        && s.superblock == expected
        && s.check != SuperblockCheck::NoCheck;
    legs.check("synced", ok, || {
        json!({"verdict": reason(&s.verdict), "value_error": s.value_error, "triggered": s.triggered,
               "superblock": s.superblock.len(), "expected": expected.len()})
    });

    let r = runner.run(&raw, &with(Trigger::superblock(height, &payments, vec![])))?;
    legs.reject("sb_unfunded", &r, "bad-cb-amount", "no triggered superblock detected");
    let mut wrong = payments.clone();
    wrong[0].1 += 1;
    let r = runner.run(&raw, &with(Trigger::superblock(height, &wrong, votes.clone())))?;
    legs.reject("sb_wrong", &r, "bad-cb-amount", "invalid superblock detected");
    let unloaded = PaymentsContext {
        governance_loaded: false,
        ..funded.clone()
    };
    let r = runner.run(&raw, &unloaded)?;
    legs.check("sb_unloaded", r.verdict.is_accept(), || json!(reason(&r.verdict)));
    let chainlocked = PaymentsContext {
        best_chainlock_height: Some(height),
        ..funded.clone()
    };
    let r = runner.run(&raw, &chainlocked)?;
    legs.check(
        "sb_chainlocked",
        r.verdict.is_accept() && r.check == SuperblockCheck::NoCheck,
        || json!(reason(&r.verdict)),
    );
    // blockReward + GetPaymentsLimit() is the most a superblock may pay, synced or not.
    let limit = runner
        .oracle
        .superblock_payments_limit(&runner.chain, height)
        .map_err(|e| format!("{e:?}"))?;
    let value_out: i64 = coinbase.iter().map(|o| o.value).sum();
    // The miner's output absorbs the change (output 0 may be the masternode's).
    let paying = |total: i64| {
        let mut crafted = block.clone();
        crafted.txdata[0].output[miner].value = (coinbase[miner].value + total - value_out) as u64;
        serialize(&crafted)
    };
    let r = runner.run(
        &paying(reward + limit),
        &PaymentsContext {
            synced: false,
            ..funded.clone()
        },
    )?;
    legs.check(
        "sb_at_max",
        r.verdict.is_accept(),
        || json!({"verdict": reason(&r.verdict), "value_error": r.value_error}),
    );
    let r = runner.run(
        &paying(reward + limit + 1),
        &PaymentsContext {
            synced: false,
            ..funded
        },
    )?;
    legs.reject("sb_over_max", &r, "bad-cb-amount", "exceeded superblock max value");
    Ok(())
}

/// The coinbase output matching each expected output, one distinct output each (Core's v24
/// strict multiplicity; FindUnmatchedMasternodePayment), or None if one is missing.
fn match_outputs(coinbase: &[TxOut], expected: &[TxOut]) -> Option<Vec<usize>> {
    let mut used = Vec::with_capacity(expected.len());
    for e in expected {
        let i = (0..coinbase.len()).find(|i| !used.contains(i) && coinbase[*i] == *e)?;
        used.push(i);
    }
    Some(used)
}

/// The rolling state of a chained credit-pool run.
struct CreditRun {
    chain: Chain,
    sample: Value,
    /// The last 576 blocks (raw), oldest first, with their height.
    window: VecDeque<(i32, Vec<u8>)>,
    prev: CreditPool,
}

fn pool_json(p: &CreditPool) -> Value {
    json!({"locked": p.locked, "current_limit": p.current_limit, "lately_unlocked": p.lately_unlocked})
}

fn credit_step(oracle: &Oracle, run: &mut CreditRun, line: &Value) -> Result<Value, String> {
    let height = line["height"].as_i64().expect("height") as i32;
    let raw = unhex(line["block"].as_str().expect("block"));
    let first = run.window.front().map(|(h, _)| *h).ok_or("no primed blocks")?;
    if first != height - 576 || run.window.len() != 576 {
        return Err(format!("the window starts at {first}, not {}", height - 576));
    }
    let headers: Vec<[u8; 80]> =
        run.window.iter().map(|(_, b)| b[..80].try_into().expect("header")).collect();
    let ctx = context(&run.sample, headers, first, MnList::Absent);
    let step = StepContext {
        prev: run.prev.clone(),
        blocks: vec![run.window[0].1.clone()],
    };
    let r = oracle.credit_pool_step(&run.chain, &raw, &ctx, &step).map_err(|e| format!("{e:?}"))?;
    let block: Block = deserialize(&raw).map_err(|e| e.to_string())?;
    let cbtx_balance = block.txdata[0]
        .special_transaction_payload
        .as_ref()
        .and_then(|p| p.clone().to_coinbase_payload().ok())
        .and_then(|cb| cb.asset_locked_amount)
        .map(|v| v as i64);
    let expected = &line["expected"];
    let mut failed = vec![];
    if !r.verdict.is_accept() {
        failed.push(json!({"leg": "verdict", "detail": reason(&r.verdict)}));
    }
    if r.expected.is_some() && r.expected != cbtx_balance {
        failed.push(json!({"leg": "cbtx_balance", "oracle": r.expected, "cbtx": cbtx_balance}));
    }
    match &r.pool {
        Some(pool) if pool_json(pool) == *expected => {}
        other => failed
            .push(json!({"leg": "pool", "oracle": other.as_ref().map(pool_json), "rpc": expected})),
    }
    if let Some(pool) = r.pool {
        run.prev = pool;
    }
    run.window.pop_front();
    run.window.push_back((height, raw));
    Ok(with_result(json!({"checked": r.checked, "has_expected": r.expected.is_some()}), failed))
}

fn main() {
    let oracle = Oracle::new(Pin::V24_0);
    let stdout = io::stdout();
    let mut credit: Option<CreditRun> = None;
    for line in io::stdin().lock().lines() {
        let line = line.expect("stdin");
        if line.trim().is_empty() {
            continue;
        }
        let sample: Value = serde_json::from_str(&line).expect("sample JSON");
        let out = match sample["kind"].as_str().expect("kind") {
            "payments" => Some(payments(&oracle, &sample)),
            "credit-start" => {
                let prev = match &sample["prev"] {
                    Value::Null => CreditPool::default(),
                    p => CreditPool {
                        locked: p["locked"].as_i64().expect("locked"),
                        current_limit: p["current_limit"].as_i64().expect("current_limit"),
                        lately_unlocked: p["lately_unlocked"].as_i64().expect("lately_unlocked"),
                        indexes: Default::default(),
                    },
                };
                credit = Some(CreditRun {
                    chain: Chain::new(sample["chain"].as_str().expect("chain"), &[]),
                    sample: sample.clone(),
                    window: VecDeque::new(),
                    prev,
                });
                None
            }
            "credit-prime" => {
                let run = credit.as_mut().expect("credit-start first");
                let raw = unhex(sample["block"].as_str().expect("block"));
                run.window.push_back((sample["height"].as_i64().expect("height") as i32, raw));
                None
            }
            "credit-step" => {
                Some(credit_step(&oracle, credit.as_mut().expect("credit-start first"), &sample))
            }
            other => panic!("unknown sample kind {other}"),
        };
        let Some(out) = out else {
            continue;
        };
        let mut out = out.unwrap_or_else(|e| json!({"result": "error", "error": e}));
        out["height"] = sample["height"].clone();
        let mut lock = stdout.lock();
        writeln!(lock, "{out}").expect("stdout");
        lock.flush().expect("stdout");
    }
}
