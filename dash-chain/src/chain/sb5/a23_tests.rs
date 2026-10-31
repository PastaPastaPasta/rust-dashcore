//! SB4 A-23's every-store property, which SB4 hands to SB5 (its `cl_tests` note; SB0 §4.1 as
//! amended by E2): every reader-visible SPV snapshot satisfies §9.1's floor predicate, with the
//! flag and the floor read from the same snapshot, and a committed step stores exactly one
//! snapshot (n8), in stage 4 and nowhere else in the step. A hook observes every store, not
//! only each step's last; stage 4 runs through `activate_segmented`, so a multi-segment step is
//! observed too.

use std::sync::{Arc, Mutex};

use dash_params::Network;
use proptest::prelude::*;

use super::hold_tests::{T, lock_at};
use super::segmented_tests::bref;
use crate::chain::tests::branch;
use crate::config::{ChainConfig, Services, StaticSporks};
use crate::reader::{ChainReader, IndexAccess};
use crate::snapshot::ChainReadSnapshot;
use crate::store::NoStore;
use crate::testing::{params, services};
use crate::{BlockId, HeaderChain};

#[derive(Clone, Debug)]
enum Op {
    /// `n` headers on the `pick`-th known block (stage 1, before the step's `VirtualTip`).
    Extend {
        pick: usize,
        n: u64,
    },
    /// A lock on the `pick`-th known block.
    Lock {
        pick: usize,
    },
    /// SPORK_19 set, then `check_cl_active_state` (off → on keeps the marks).
    Spork(bool),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (any::<usize>(), 1..8u64).prop_map(|(pick, n)| Op::Extend { pick, n }),
        2 => any::<usize>().prop_map(|pick| Op::Lock { pick }),
        1 => any::<bool>().prop_map(Op::Spork),
    ]
}

/// §9.1's floor predicate on one snapshot, its flag and floor from that snapshot: ChainLocks
/// observed off, no floor, or a tip that does not conflict with the floor (the tip on the
/// floor's path or trailing it: (b′), DEC-136).
fn floor_ok(s: &ChainReadSnapshot, r: &ChainReader) -> bool {
    let Some(f) = s.cl_floor.filter(|_| s.cl_enabled_observed) else {
        return true;
    };
    let t = s.tip.tip;
    let th = r.height(t).expect("resident");
    r.ancestor(t, f.height) == Some(f.id) || r.ancestor(f.id, th) == Some(t)
}

/// Runs `steps`, each a batch of ops then §9.1's recipe: one `VirtualTip` for the step's calls,
/// `fork_target` against it, stage 4 to the target, stage 5.
fn run(steps: Vec<Vec<Op>>) {
    let p = params(Network::Regtest);
    let sporks = Arc::new(StaticSporks::for_network(&p, Some(false)));
    let svc = Services {
        sporks: sporks.clone(),
        ..services(&p)
    };
    let mut cfg = ChainConfig::spv(&p, 10_000);
    cfg.feeds.segment_events = 2;
    let mut c = HeaderChain::new_genesis(p, cfg, svc, NoStore).expect("a valid configuration");
    let stored = Arc::new(Mutex::new(Vec::new()));
    let observe = {
        let stored = stored.clone();
        move |s: &Arc<ChainReadSnapshot>| stored.lock().expect("stores").push(s.clone())
    };
    let count = || stored.lock().expect("stores").len();
    *c.feeds().handle.shared.faults.on_store.lock() = Some(Box::new(observe));
    let reader = c.reader();
    let mut known = vec![BlockId(0)];
    let mut k0 = 1;
    for batch in steps {
        let seen = count();
        for op in &batch {
            if let &Op::Extend {
                pick,
                n,
            } = op
            {
                let from = known[pick % known.len()];
                known.extend(branch(&mut c, from, k0, n, T + k0 as u32));
                k0 += n;
            }
        }
        let mut vt = c.virtual_tip();
        for op in &batch {
            match *op {
                Op::Extend {
                    ..
                } => {}
                Op::Lock {
                    pick,
                } => {
                    let lock = lock_at(&reader, known[pick % known.len()]);
                    c.apply_chainlock(lock, &mut vt).expect("applies");
                }
                Op::Spork(on) => {
                    sporks.set(on);
                    c.check_cl_active_state();
                }
            }
        }
        // Stage 1 builds no plan (DEC-187): each sub-step of stage 4 plans its own.
        let target = c.fork_target(&vt).expect("a target");
        assert_eq!(count(), seen, "no store before stage 4");
        c.activate_segmented(bref(&c, target.id), &mut || {}).expect("stage 4");
        // Stage 4 publishes once, a quiet step too (SB4's `set_active_tip`).
        assert_eq!(count(), seen + 1, "one store per committed step");
        c.records_committed();
        assert_eq!(count(), seen + 1, "no store after stage 4");
        let s = stored.lock().expect("stores")[seen].clone();
        assert!(floor_ok(&s, &reader), "a reader-visible tip conflicts with the floor: {s:?}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// `prop_spv_tip_never_conflicts_floor`: random branch trees, locks, SPORK_19 toggles and
    /// batches of them per step.
    #[test]
    fn prop_spv_tip_never_conflicts_floor(
        steps in prop::collection::vec(prop::collection::vec(op(), 1..4), 1..12)
    ) {
        run(steps);
    }
}
