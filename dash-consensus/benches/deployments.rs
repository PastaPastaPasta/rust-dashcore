//! CS4's benchmarks: the versionbits reads v24 makes for every block, on a 4032-block regtest
//! chain where v24 (window 100, thresholds 80 -> 60 over 5, EHF) starts, locks in by 7 of 10
//! signalling blocks once its EHF signal is mined at 200, and activates.
//!
//! - `deployments/per_block_warm`: for each of the 4032 blocks, what Core asks per block once the
//!   cache holds every period: `DeploymentActiveAfter(V24)` (`ConnectBlock`'s
//!   `DeploymentActiveAt`), `DeploymentActiveAfter(EVO_SHARES)` (`GetSpecialTxRules`) and
//!   `ComputeBlockVersion` (the unknown-versions warning, the miner). One cache lookup per
//!   deployment, no allocation.
//! - `deployments/first_walk_cold`: v24's state at the tip with an empty cache, the walk back to
//!   the start and forward through every period (a node's first query after start-up).

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::deployments::{
    Deployments, EhfSignalSource, EhfSignals, ThresholdState, VERSIONBITS_TOP_BITS,
    VersionBitsCache,
};
use dash_consensus::{BlockId, ChainAncestry, HeaderCtx};
use dash_num::{Arith256, CompactTarget};
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, DeploymentPos, Network, Release};
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

const BLOCKS: u32 = 4032;
const SIGNAL_HEIGHT: u32 = 200;

struct Chain(Vec<HeaderCtx>);

impl Chain {
    fn new() -> Self {
        let mut headers: Vec<HeaderCtx> = Vec::with_capacity(BLOCKS as usize);
        for height in 0..BLOCKS {
            let time = 1_417_713_337 + 150 * height;
            let signals = height >= SIGNAL_HEIGHT && height % 10 < 7;
            let version = if signals {
                VERSIONBITS_TOP_BITS | 1 << 13
            } else {
                VERSIONBITS_TOP_BITS
            };
            // GetMedianTimePast() of increasing times: the middle one of the last 11 (or fewer)
            let low = height.saturating_sub(10);
            let count = height - low + 1;
            let mtp = 1_417_713_337 + 150 * i64::from(low + count / 2);
            headers.push(HeaderCtx {
                height,
                hash: BlockHash::from([0; 32]),
                time,
                bits: CompactTarget::new(0x207f_ffff),
                version,
                mtp,
                chain_work: Arith256::ZERO,
            });
        }
        Self(headers)
    }
}

impl ChainAncestry for Chain {
    fn header(&self, id: BlockId) -> HeaderCtx {
        self.0[id.0 as usize]
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        (height <= id.0).then_some(BlockId(height))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        id.0.checked_sub(1).map(BlockId)
    }
}

/// v24's EHF signal, mined at `SIGNAL_HEIGHT`.
struct Signal;

impl EhfSignalSource for Signal {
    fn signals_for_block(&self, block: BlockId) -> EhfSignals {
        if block.0 >= SIGNAL_HEIGHT {
            [(13, SIGNAL_HEIGHT as i32)].into_iter().collect()
        } else {
            EhfSignals::new()
        }
    }
}

fn params() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, "100:100")
        .arg(ChainOption::TestActivationHeight, "v20@100")
        .arg(ChainOption::TestActivationHeight, "mn_rr@100")
        .arg(ChainOption::VbParams, "v24:0:9223372036854775807:0:100:80:60:5:1")
        .build()
        .expect("regtest")
}

fn deployments(c: &mut Criterion) {
    let params = params();
    let chain = Chain::new();
    let tip = Some(BlockId(BLOCKS - 1));
    let v24 = Deployment::Versionbits(DeploymentPos::V24);
    let evo = Deployment::Versionbits(DeploymentPos::EvoShares);

    let warm = VersionBitsCache::new();
    let d = Deployments::new(&params, &chain, &Signal, &warm);
    assert_eq!(d.state(tip, DeploymentPos::V24), ThresholdState::Active, "v24 activates");
    for h in 0..BLOCKS {
        d.compute_block_version(Some(BlockId(h)));
    }

    let mut group = c.benchmark_group("deployments");
    group.throughput(Throughput::Elements(u64::from(BLOCKS)));
    group.bench_function("per_block_warm", |b| {
        b.iter(|| {
            let mut acc = 0i32;
            for h in 0..BLOCKS {
                let prev = Some(BlockId(h));
                acc ^= i32::from(d.active_after(prev, v24));
                acc ^= i32::from(d.active_after(prev, evo));
                acc ^= d.compute_block_version(prev);
            }
            black_box(acc)
        })
    });
    group.throughput(Throughput::Elements(1));
    group.bench_function("first_walk_cold", |b| {
        b.iter(|| {
            let cold = VersionBitsCache::new();
            let d = Deployments::new(&params, &chain, &Signal, &cold);
            black_box(d.state(black_box(tip), DeploymentPos::V24))
        })
    });
    group.finish();
}

criterion_group!(benches, deployments);
criterion_main!(benches);
