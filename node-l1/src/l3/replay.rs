//! The shim replay leg (plan §3.5): after a main leg, one chain session on the leg's arguments
//! runs the leg's exact op list (`SETTIME`, setup blocks, trial blocks and proposals,
//! rollbacks). Core's replies must be dashd's; its best header and index digest V's, op by op
//! (P7, [`planes::p7_replay`]); its `best_invalid` and `candidates` V's (P7's Core side,
//! [`planes::p7_core`], OD-L3-15); its coins digest V's (P5). Only then is Core held to the
//! model, where the leg ran P7 on it (a harness cross-check, [`planes::p7_model`], OD-L3-35),
//! so a model error never hides a divergence; a done leg that ran ops but checked the model on
//! none is a harness error. Its `BlockChecked` class for each submit trial is the runtime class (P4).

use std::collections::BTreeMap;

use dash_oracle_sys::{ChainOp, ChainStart, Oracle, Pin, Post};

use super::leg::{LegLog, T0};
use super::model::P7;
use super::planes;
use crate::chain::world::header_hash;

/// The first shim ABI whose post carries `best_invalid` and `candidates`: P7's Core side needs
/// it.
pub const CORE_P7_ABI: i32 = 33;

/// A replay's result.
#[derive(Debug, Default)]
pub struct Replayed {
    pub abi: i32,
    /// Core's `BlockChecked` class for each submit trial's block, by op index (`None`: no
    /// invalid signal for it).
    pub runtime: BTreeMap<usize, Option<String>>,
    /// Whether any proposal's reply carried a `BlockChecked` (Q-B's first record).
    pub proposal_checked: bool,
    pub proposals: usize,
    pub seconds: f64,
    pub error: Option<String>,
    /// Core's post (the shim's JSON) at each op the leg recorded.
    pub posts: BTreeMap<usize, String>,
    /// The ops whose up post held more than one candidate (OD-L3-19), Core's (V's are equal by
    /// P7).
    pub multi_candidate_ops: Vec<usize>,
    /// The ops whose post Core was held to the model on ([`planes::p7_model`]).
    pub model_checked: usize,
}

/// The replay's planes on Core's post after op `at`: V's on P7 ([`planes::p7_replay`],
/// [`planes::p7_core`]) and P5 first, and only then the model's where the leg recorded it
/// ([`planes::p7_model`]), so a model error never hides a divergence.
pub fn held(at: usize, core: &Post, v: &Post, model: Option<&P7>) -> Result<(), String> {
    planes::p7_replay(at, core, v)?;
    planes::p7_core(at, core, v)?;
    if core.utxo != v.utxo {
        return Err(format!(
            "P5 (replay) diverges at op {at}: Core {:?}, V {:?}",
            core.utxo, v.utxo
        ));
    }
    match model {
        Some(model) => planes::p7_model(at, core, model),
        None => Ok(()),
    }
}

/// Replay `log` on a new chain session (this thread must hold none).
pub fn replay(log: &LegLog) -> Replayed {
    let mut out = Replayed::default();
    let start = std::time::Instant::now();
    if let Err(e) = run(log, &mut out) {
        out.error = Some(e);
    }
    out.seconds = start.elapsed().as_secs_f64();
    out
}

fn run(log: &LegLog, out: &mut Replayed) -> Result<(), String> {
    let oracle = Oracle::new(Pin::V24_0);
    out.abi = oracle.abi_version();
    if out.abi < CORE_P7_ABI {
        return Err(format!(
            "the shim is at ABI {}: P7's Core side needs best_invalid and candidates (ABI {CORE_P7_ABI}, OD-L3-15)",
            out.abi
        ));
    }
    let args: Vec<&str> = log.chain_args.iter().map(String::as_str).collect();
    let mut session =
        match oracle.chain_session(&args, T0, None).map_err(|e| format!("chain_session: {e}"))? {
            ChainStart::Started(s) => s,
            ChainStart::Refused(m) => {
                return Err(format!("the replay's genesis load was refused: {m}"));
            }
        };
    let recorded: Vec<usize> =
        log.recorded.iter().filter_map(|r| r["op"].as_u64()).map(|i| i as usize).collect();
    for (i, logged) in log.ops.iter().enumerate() {
        let r = session.step(&logged.op).map_err(|e| format!("replay op {i}: {e}"))?;
        if r.reply.outcome != logged.dashd {
            return Err(format!(
                "P1 (replay) diverges at op {i}: dashd {:?}, Core {:?}",
                logged.dashd, r.reply.outcome
            ));
        }
        held(i, &r.post, &logged.v.post, logged.model_p7.as_ref())?;
        out.model_checked += usize::from(logged.model_p7.is_some());
        if planes::multi_candidate(&r.post) {
            out.multi_candidate_ops.push(i);
        }
        if recorded.contains(&i) {
            let post =
                String::from_utf8(r.raw.post.clone()).map_err(|e| format!("Core's post: {e}"))?;
            out.posts.insert(i, post);
        }
        match &logged.op {
            ChainOp::Proposal(_) => {
                out.proposals += 1;
                out.proposal_checked |= !r.reply.checked.is_empty();
            }
            ChainOp::Block(_) | ChainOp::Header(_) if logged.trial.is_some() => {
                let head: [u8; 80] = match &logged.op {
                    ChainOp::Header(head) => *head,
                    ChainOp::Block(bytes) => bytes[..80].try_into().map_err(|_| "a short block")?,
                    _ => unreachable!(),
                };
                let hash = header_hash(&head);
                let class =
                    r.reply.checked.iter().find(|c| c.0.0 == hash).and_then(|c| c.2.clone());
                out.runtime.insert(i, class);
            }
            _ => {}
        }
    }
    if out.model_checked == 0 && !log.ops.is_empty() && log.error.is_none() {
        return Err(format!(
            "harness error: the leg ran {} ops and recorded the model's P7 fields on none",
            log.ops.len()
        ));
    }
    Ok(())
}
