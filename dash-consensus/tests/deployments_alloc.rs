//! CS4's per-block reads allocate nothing once the versionbits cache holds every period: v24's
//! `DeploymentActiveAfter` and `ComputeBlockVersion` for each block of a 2000-block regtest chain
//! where v24 (window 100, EHF) starts, locks in and activates. A global allocator counting the
//! test thread's allocations, so this file is a test binary of its own.

#[path = "support/versionbits.rs"]
mod versionbits;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use dash_consensus::BlockId;
use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::deployments::{
    Deployments, EhfSignalSource, EhfSignals, ThresholdState, VERSIONBITS_TOP_BITS,
    VersionBitsCache,
};
use dash_params::DeploymentPos;
use versionbits::{Chain, Network, Run, Scenario};

struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn allocations() -> usize {
    ALLOCATIONS.with(Cell::get)
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // the thread's own count; `try_with`: a thread can still allocate after its locals are gone
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// v24's signal, mined at 200.
struct Signal;

impl EhfSignalSource for Signal {
    fn signals_for_block(&self, block: BlockId) -> EhfSignals {
        if block.0 >= 200 {
            [(13, 200)].into_iter().collect()
        } else {
            EhfSignals::new()
        }
    }
}

#[test]
fn warm_per_block_reads_do_not_allocate() {
    let signalling = VERSIONBITS_TOP_BITS | 1 << 13;
    let scenario = Scenario {
        network: Network::Regtest,
        args: vec![
            "-dip3params=100:100".into(),
            "-testactivationheight=v20@100".into(),
            "-testactivationheight=mn_rr@100".into(),
            "-vbparams=v24:0:9223372036854775807:0:100:80:60:5:1".into(),
        ],
        first_height: 0,
        headers: vec![
            Run {
                count: 200,
                version: VERSIONBITS_TOP_BITS,
                time: versionbits::BASE_TIME,
                step: 150,
            },
            Run {
                count: 1800,
                version: signalling,
                time: versionbits::BASE_TIME + 30_000,
                step: 150,
            },
        ],
        signals: vec![],
        queries: vec![],
    };
    let params = scenario.params().expect("params");
    let chain = Chain::new(0, &scenario.header_fields());
    let cache = VersionBitsCache::new();
    let d = Deployments::new(&params, &chain, &Signal, &cache);
    assert_eq!(d.state(Some(BlockId(1999)), DeploymentPos::V24), ThresholdState::Active);
    let v24 = Deployment::Versionbits(DeploymentPos::V24);
    let per_block = |h: u32| {
        let prev = Some(BlockId(h));
        (d.active_after(prev, v24), d.compute_block_version(prev))
    };
    // warm every period's entry of every deployment
    for h in 0..2000 {
        per_block(h);
    }
    let probe = allocations();
    drop(std::hint::black_box(vec![0u8; 1]));
    assert_eq!(allocations(), probe + 1, "the counter sees the test thread's allocations");
    let before = allocations();
    let mut acc = 0i32;
    for h in 0..2000 {
        let (active, version) = per_block(h);
        acc ^= i32::from(active) ^ version;
    }
    let after = allocations();
    std::hint::black_box(acc);
    assert_eq!(after - before, 0, "allocations on the warm per-block path");
}
