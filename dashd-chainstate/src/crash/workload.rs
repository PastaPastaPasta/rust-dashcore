//! `chainstate_crash`'s workloads (CH2 §6.5, node-fuzz): seeded runs of §6.4's operation families
//! on [`ChainSut`], with or without b3's test subscriber, for ST3a's `sweep`. The sweep crashes
//! each once (weighted toward flushes and recoveries), with p = 0.2 a fault and p = 0.1 a nested
//! crash, and judges every invariant the suite checks, FA1 and FA3–FA6 included. The PR smoke is
//! `tests/crash_subscriber.rs`'s; node-fuzz's target wraps the same generator (b3).
//!
//! Left out, each with its own exhaustive scenarios: `invalidateblock` and `-reindex-chainstate`
//! (S10, S12, DEC-196), pruning (block files hold about 120 blocks; S3p, S11–S14), `-dbcrashratio`
//! (C9), the corrupt coin (FA7) and planted bugs.

use dashd_store::faulty::{Scenario, Seeded};

use super::blind::{SUB_BATCHES, connect, reorg};
use super::{ChainConfig, ChainOp, ChainSut, SubConfig, SubMode};

/// A seeded workload: a durable base of 2–8 blocks, then 3–10 steps, and a closing flush. A
/// step connects, reorgs, flushes, flushes while V connects or rewinds, accepts a block ahead,
/// makes the periodic index-only write, or runs the subscriber. Half the workloads carry a
/// subscriber: Synced with a backlog of 0–3, or Syncing with a cadence of 1–30, and `D_max` 2–9
/// or the default 96.
pub fn chainstate_crash(rng: &mut Seeded) -> Scenario<ChainSut> {
    let index = rng.chance(500).then(|| {
        let mut sub = if rng.chance(500) {
            SubConfig::synced(rng.below(4) as u32)
        } else {
            SubConfig::syncing(1 + rng.below(30) as u32)
        };
        if rng.chance(500) {
            sub.d_max = 2 + rng.below(8) as u32;
        }
        sub
    });
    let syncing = matches!(
        index,
        Some(SubConfig {
            mode: SubMode::Syncing { .. },
            ..
        })
    );
    let cfg = ChainConfig {
        sub_batch: SUB_BATCHES[rng.below(3) as usize],
        index,
        ..ChainConfig::default()
    };
    let mut h = 2 + rng.below(7) as u32;
    let mut ops = vec![ChainOp::Connect(h), ChainOp::Flush];
    for _ in 0..3 + rng.below(8) {
        match rng.below(12) {
            0..=2 => ops.push(connect(rng, &mut h, 6)),
            3 | 4 if h > 1 => ops.push(reorg(rng, &mut h, 4)),
            5 => ops.push(ChainOp::Flush),
            6 => {
                let d = 1 + rng.below(3) as u32;
                h += d;
                ops.push(ChainOp::FlushDuring(d));
            }
            7 if h > 1 => {
                let d = 1 + rng.below(u64::from((h - 1).min(3))) as u32;
                h -= d;
                ops.push(ChainOp::FlushRewind(d));
            }
            3 | 4 | 7 | 8 => ops.push(ChainOp::Accept(1 + rng.below(2) as u32)),
            9 => ops.push(ChainOp::IndexWrite),
            _ if syncing && rng.chance(300) => ops.push(ChainOp::SubTick),
            _ => ops.push(ChainOp::SubSync(1 + rng.below(20) as u32)),
        }
    }
    ops.push(ChainOp::Flush);
    Scenario::new("chainstate_crash", cfg, ops)
}
