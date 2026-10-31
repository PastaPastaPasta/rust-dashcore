//! The shim replay leg (plan §3.5): after a main leg, one chain session on the leg's arguments
//! runs the leg's exact op list (`SETTIME`, setup blocks, trial blocks and proposals,
//! rollbacks). Core's replies must be dashd's; its best header and index digest V's, op by op,
//! and its best header the model's where the leg checked the model (P7,
//! [`planes::p7_replay`]); its coins digest V's (P5). Its `BlockChecked` class for each submit
//! trial is the runtime class (P4).
//!
//! `best_invalid` and `candidates` join the post at shim ABI 33 (track N's slot-1 series). A
//! linked shim at that ABI or later fails the replay until this leg compares them (OD-L3-15):
//! the model would otherwise stand in for Core where Core can answer.

use std::collections::BTreeMap;

use dash_oracle_sys::{ChainOp, ChainStart, Oracle, Pin};

use super::leg::{LegLog, T0};
use super::planes;
use crate::chain::world::header_hash;

/// The first shim ABI whose post carries `best_invalid` and `candidates`.
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
    if out.abi >= CORE_P7_ABI {
        return Err(format!(
            "the shim is at ABI {}: Core's best_invalid and candidates must be compared (OD-L3-15)",
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
        let v = &logged.v.post;
        planes::p7_replay(i, &r.post, v, logged.model_best_header)?;
        if recorded.contains(&i) {
            let post =
                String::from_utf8(r.raw.post.clone()).map_err(|e| format!("Core's post: {e}"))?;
            out.posts.insert(i, post);
        }
        if r.post.utxo != v.utxo {
            return Err(format!(
                "P5 (replay) diverges at op {i}: Core {:?}, V {:?}",
                r.post.utxo, v.utxo
            ));
        }
        match &logged.op {
            ChainOp::Proposal(_) => {
                out.proposals += 1;
                out.proposal_checked |= !r.reply.checked.is_empty();
            }
            ChainOp::Block(bytes) if logged.trial.is_some() => {
                let head: [u8; 80] = bytes[..80].try_into().map_err(|_| "a short block")?;
                let hash = header_hash(&head);
                let class =
                    r.reply.checked.iter().find(|c| c.0.0 == hash).and_then(|c| c.2.clone());
                out.runtime.insert(i, class);
            }
            _ => {}
        }
    }
    Ok(())
}
