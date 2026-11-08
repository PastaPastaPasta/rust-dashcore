//! `coins_replay_blind`'s windows (CH2 §6.5, Opus N1): seeded workloads of connects, reorgs and
//! connect-then-disconnect windows inside one flush, for ST3a's `sweep` over [`ChainSut`]. The
//! sweep crashes each once (weighted toward flushes and recoveries) on a seeded subset of the
//! pending batches, with p = 0.1 a nested crash in recovery, and judges recovery against the
//! reference ([`ChainRef`](super::ChainRef)), and a nested recovery by R7a (idempotence).
//! The PR smoke is `tests/crash.rs`'s; node-fuzz's target wraps the same generator (b3).

use dashd_store::faulty::{Scenario, Seeded};

use super::{ChainConfig, ChainOp, ChainSut};

/// W's and recovery's coin sub-batch bounds: one-batch, a few blocks' coins per sub-batch, and
/// about one block's.
pub(super) const SUB_BATCHES: [Option<u64>; 3] = [None, Some(1000), Some(250)];

/// A seeded workload: a durable base of 2–8 blocks, then 2–5 steps, and a closing flush. A step
/// connects, reorgs, flushes, flushes while V connects, or connects then reorgs away some of the
/// blocks it connected (at most one below them), so one window holds blocks connected and
/// disconnected.
pub fn coins_replay_blind(rng: &mut Seeded) -> Scenario<ChainSut> {
    let cfg = ChainConfig {
        sub_batch: SUB_BATCHES[rng.below(3) as usize],
        ..ChainConfig::default()
    };
    let mut h = 2 + rng.below(7) as u32;
    let mut ops = vec![ChainOp::Connect(h), ChainOp::Flush];
    for _ in 0..2 + rng.below(4) {
        match rng.below(8) {
            0..=2 => ops.push(connect(rng, &mut h, 4)),
            3 | 4 => ops.push(reorg(rng, &mut h, 3)),
            5 => ops.push(ChainOp::Flush),
            6 => {
                let d = 1 + rng.below(3) as u32;
                h += d;
                ops.push(ChainOp::FlushDuring(d));
            }
            _ => {
                let n = 1 + rng.below(3) as u32;
                h += n;
                ops.push(ChainOp::Connect(n));
                ops.push(reorg(rng, &mut h, n + 1));
            }
        }
    }
    ops.push(ChainOp::Flush);
    Scenario::new("coins_replay_blind", cfg, ops)
}

/// `c n` with `n` in `1..=max`.
pub(super) fn connect(rng: &mut Seeded, h: &mut u32, max: u32) -> ChainOp {
    let n = 1 + rng.below(u64::from(max)) as u32;
    *h += n;
    ChainOp::Connect(n)
}

/// `r fork:len`: a fork 1 to `depth` blocks below the tip (block 1 at the lowest), and a new
/// branch one or two blocks longer than what it disconnects, so it outworks the old.
pub(super) fn reorg(rng: &mut Seeded, h: &mut u32, depth: u32) -> ChainOp {
    let d = 1 + rng.below(u64::from(depth.min(*h - 1))) as u32;
    let fork = *h - d;
    let len = d + 1 + rng.below(2) as u32;
    *h = fork + len;
    ChainOp::Reorg {
        fork,
        len,
    }
}
