//! SB4 C6: the ChainLock state (§7.1, §7.5), the node's `enforce_block` composite (§9.1,
//! A-26, A-39's SB4 half), `ResetBlockFailureFlags`' index half (§7.4), and SPV's step recipe
//! with `VirtualTip`, `fork_target` and the floor refusal (§6.2, §9.1; A-20…A-23, A-29's SPV
//! halves). The cold cases (A-21b, A-30, A-35) are in `spv_tests`; Core's vectors are replayed
//! in `scenario_tests::cl`.

use std::sync::Arc;

use dash_params::Network;

use super::tests::{add, branch, connect_all};
use super::*;
use crate::chainlock::{
    ChainLockRef, ClActiveState, ClOutcome, ClPersisted, EnforceReport, FloorRelation, ForkTarget,
    ResetReport,
};
use crate::config::StaticSporks;
use crate::error::ApplyError;
use crate::host::{TipHost, VirtualTip};
use crate::testing::{params, services};

const T: u32 = 1_700_000_000;

/// A chain from regtest genesis whose SPORK_19 the test sets; ChainLocks start off.
fn chain(mode: ChainMode) -> (HeaderChain, Arc<StaticSporks>) {
    let p = params(Network::Regtest);
    let sporks = Arc::new(StaticSporks::for_network(&p, Some(false)));
    let svc = Services {
        sporks: sporks.clone(),
        ..services(&p)
    };
    let cfg = match mode {
        ChainMode::Node => ChainConfig::node(&p),
        ChainMode::Spv => ChainConfig::spv(&p, 10_000),
    };
    let mut chain = HeaderChain::new_genesis(p, cfg, svc, NoStore).expect("a valid configuration");
    if mode == ChainMode::Node {
        chain.init_active_tip(BlockId(0)).expect("Core LoadChainTip");
    }
    (chain, sporks)
}

/// A lock on `id` at its own height.
fn lock(chain: &HeaderChain, id: BlockId) -> ChainLockRef {
    let e = chain.reader().entry(id).expect("resident");
    ChainLockRef {
        height: e.height,
        hash: e.hash,
    }
}

fn conflict(chain: &HeaderChain, id: BlockId) -> bool {
    chain.writer_status(id).expect("known").has(Status::CONFLICT_CHAINLOCK)
}

fn tip(chain: &HeaderChain) -> BlockId {
    chain.private_tip().expect("a tip").0.id
}

/// SPV's `set_active_tip(to)` (§9.1 stage 4) after its plan, then stage 5.
fn spv_activate(chain: &mut HeaderChain, to: BlockId) -> TipChange {
    chain.tip_plan(to).expect("a plan");
    let change = chain.set_active_tip(to).expect("stage 4");
    chain.records_committed();
    change
}

/// One SPV step (§9.1's recipe, stages 2-3 being SB5's): one `VirtualTip` for the step's calls,
/// then `fork_target` against it, and stage 4 to the target.
fn spv_step(
    chain: &mut HeaderChain,
    calls: impl FnOnce(&mut HeaderChain, &mut VirtualTip),
) -> (VirtualTip, ForkTarget, TipChange) {
    let mut vt = chain.virtual_tip();
    calls(chain, &mut vt);
    let target = chain.fork_target(&vt).expect("a target");
    let change = spv_activate(chain, target.id);
    (vt, target, change)
}

/// The enforcement of an outcome that moved the floor.
pub(super) fn enforced(out: ClOutcome) -> EnforceReport {
    match out {
        ClOutcome::Floor {
            enforced: Some(r),
            ..
        } => r,
        out => panic!("{out:?}"),
    }
}

// ---- the ChainLock state (§7.1, §7.5) -------------------------------------------------------

/// `UpdateBestChainlock` with the lookup before it: stale at or below the best height, the best
/// lock recorded before the lookup's outcome (`chainlock/chainlock.cpp:106-132`), the floor only
/// for a known block at the lock's height. CH1-A5: `record_chainlock` does not enforce.
#[test]
fn record_chainlock_follows_update_best_chainlock() {
    let (mut chain, _) = chain(ChainMode::Node);
    let a = branch(&mut chain, BlockId(0), 1, 10, T);
    let a5 = lock(&chain, a[4]);
    assert!(matches!(
        chain.record_chainlock(a5),
        ClOutcome::Floor { id, enforced: None } if id == a[4]
    ));
    assert_eq!(chain.cl_floor(), Some(a[4]));
    assert_eq!(chain.record_chainlock(a5), ClOutcome::Stale);
    assert_eq!(chain.record_chainlock(lock(&chain, a[2])), ClOutcome::Stale);

    let unknown = ChainLockRef {
        height: 7,
        hash: BlockHash::from([0xee; 32]),
    };
    assert_eq!(chain.record_chainlock(unknown), ClOutcome::Pending);
    let mismatch = ChainLockRef {
        height: 9,
        hash: lock(&chain, a[5]).hash,
    };
    assert_eq!(chain.record_chainlock(mismatch), ClOutcome::HeightMismatch);
    assert_eq!(
        chain.cl_state(),
        ClPersisted {
            best: Some(mismatch),
            floor: Some(a[4]),
            enabled_observed: false,
        },
        "the best lock moves, the floor stays"
    );
    // A known header at the lock's height: the late header moves the floor (§7.1).
    let pending = ChainLockRef {
        height: 11,
        hash: crate::testing::fake_hash(50),
    };
    assert_eq!(chain.record_chainlock(pending), ClOutcome::Pending);
    let late = add(&mut chain, a[9], 50, T + 10);
    assert_eq!(chain.accepted_block_header(late), Ok(Some(late)));
    assert_eq!(chain.cl_floor(), Some(late));
}

/// CH1-A5: SPV's lookups can fail, so it uses `apply_chainlock`.
#[test]
#[should_panic(expected = "record_chainlock is the node's")]
fn record_chainlock_is_the_nodes() {
    let (mut chain, _) = chain(ChainMode::Spv);
    let l = lock(&chain, BlockId(0));
    chain.record_chainlock(l);
}

/// `CheckActiveState` (§7.5): off → on resets the best lock and the floor, on → off keeps them.
#[test]
fn check_cl_active_state_transitions() {
    let (mut chain, sporks) = chain(ChainMode::Node);
    let a = branch(&mut chain, BlockId(0), 1, 5, T);
    assert_eq!(chain.check_cl_active_state(), ClActiveState::Unchanged);
    chain.record_chainlock(lock(&chain, a[3]));
    sporks.set(true);
    assert_eq!(chain.check_cl_active_state(), ClActiveState::TurnedOnReset);
    assert_eq!((chain.cl_best, chain.cl_floor()), (None, None));
    assert!(chain.cl_enabled_observed());
    assert_eq!(chain.check_cl_active_state(), ClActiveState::Unchanged);

    chain.record_chainlock(lock(&chain, a[4]));
    sporks.set(false);
    assert_eq!(chain.check_cl_active_state(), ClActiveState::TurnedOff);
    assert_eq!(chain.cl_floor(), Some(a[4]), "on → off clears nothing");
    assert!(!chain.cl_enabled_observed());
    // A lock below the kept best one is stale until the next reset (A-29 (i)).
    assert_eq!(chain.record_chainlock(lock(&chain, a[2])), ClOutcome::Stale);
    sporks.set(true);
    assert_eq!(chain.check_cl_active_state(), ClActiveState::TurnedOnReset);
    assert!(matches!(chain.record_chainlock(lock(&chain, a[2])), ClOutcome::Floor { .. }));
}

/// CH1-A7: `None` only when skipped (observed off, or no floor); a no-op enforcement is `Some`.
#[test]
fn enforce_best_chainlock_is_some_on_a_noop() {
    let (mut chain, sporks) = chain(ChainMode::Node);
    let a = branch(&mut chain, BlockId(0), 1, 5, T);
    connect_all(&mut chain, &a);
    let mut v = Recorder::default();
    assert_eq!(chain.enforce_best_chainlock(&mut v), Ok(None), "observed off");
    sporks.set(true);
    chain.check_cl_active_state();
    assert_eq!(chain.enforce_best_chainlock(&mut v), Ok(None), "no floor");
    chain.record_chainlock(lock(&chain, a[4]));
    let r = chain.enforce_best_chainlock(&mut v).expect("enforces").expect("not skipped");
    assert_eq!((r.floor, r.marked.len(), r.disconnected.len()), (a[4], 0, 0));
    // A connected header-only block is not `IsValid()` here: the reset runs and reconsiders it.
    let reset = r.reset.expect("validation.cpp:4074-4076");
    assert_eq!((reset.reconsidered, reset.promoted), (vec![a[4]], Vec::new()));
}

// ---- the node's composite (§9.1, A-26, A-39) ------------------------------------------------

/// A node host that steps the chain, probes the tip mutex at each `DisconnectTip`, and checks
/// that `conflicting_marked` already reads its block marked (mi-4 K).
#[derive(Default)]
struct Recorder {
    steps: Vec<TipStep>,
    /// Per disconnect: the mutex held (from this thread and from another), and the published
    /// tip.
    probes: Vec<(bool, bool, BlockId)>,
    marked: Vec<(BlockId, BlockId, bool)>,
    /// A-39: `conflicting_marked` calls the mutex-taking `publish_status`.
    reenter: bool,
    /// §9.1: `disconnect_tip` connects this block instead of stepping down.
    connect: Option<BlockId>,
    /// D-C6-1: `disconnect_tip` panics after its step.
    panic: bool,
}

impl TipHost<NoStore, Permanent> for Recorder {
    type Error = ChainError;

    fn tip(&self, chain: &HeaderChain) -> BlockId {
        tip(chain)
    }

    fn contains(&self, chain: &HeaderChain, id: BlockId) -> Result<bool, ChainError> {
        Ok(chain.active_contains_private(id))
    }

    fn disconnect_tip(&mut self, chain: &mut HeaderChain) -> Result<(), ChainError> {
        if let Some(id) = self.connect {
            return chain.connect_tip(id).map(|_| ());
        }
        let state = &chain.shared.readers.tip().state;
        let here = state.try_lock().is_none();
        let there = std::thread::scope(|s| s.spawn(|| state.try_lock().is_none()).join())
            .expect("the probe");
        self.probes.push((here, there, chain.snapshot().tip.tip));
        self.steps.push(chain.disconnect_tip()?);
        assert!(!self.panic, "the host fails after one step");
        Ok(())
    }

    fn conflicting_marked(
        &mut self,
        chain: &mut HeaderChain,
        marked: BlockId,
        was_in_chain: bool,
    ) -> Result<(), ChainError> {
        // Core :4121 marks `b` before :4122-4148 (host.rs): CH1-node's candidate re-add filters
        // on the mark, so the hook must already read it.
        assert!(conflict(chain, marked), "{marked:?} is marked before the hook");
        self.marked.push((marked, tip(chain), was_in_chain));
        if self.reenter {
            chain.publish_status();
        }
        Ok(())
    }
}

/// A-26 (ii) / R4-M2: an enforcement reorg is one critical section. The mutex is held at every
/// disconnect, each disconnect takes its own `seq`, readers keep the pre-enforcement snapshot,
/// and one snapshot is stored when `enforce_block` returns, with the tip at the fork trailing
/// the floor and `best_header` walked back to it. The marks are not dirty (D15); the reset of the
/// header-only locked block is (D18, `validation.cpp:4074-4076`).
#[test]
fn node_enforce_block_is_one_critical_section() {
    let (mut chain, sporks) = chain(ChainMode::Node);
    let a = branch(&mut chain, BlockId(0), 1, 100, T);
    let b = branch(&mut chain, a[89], 2000, 15, T + 200);
    connect_all(&mut chain, &a[..90]);
    connect_all(&mut chain, &b);
    sporks.set(true);
    chain.check_cl_active_state();
    chain.record_chainlock(lock(&chain, a[99]));
    chain.take_dirty();
    let before = chain.snapshot();

    let mut v = Recorder::default();
    let r = chain.enforce_best_chainlock(&mut v).expect("enforces").expect("not skipped");
    assert_eq!(r.marked, vec![b[0]]);
    let mut down = b.clone();
    down.reverse();
    assert_eq!(r.disconnected, down);
    assert_eq!(v.marked, vec![(b[0], a[89], true)], "Core's UpdatedBlockTip(80, null)");
    assert!(v.probes.iter().all(|&(here, there, _)| here && there), "held throughout");
    assert!(v.probes.iter().all(|&(.., t)| t == b[14]), "readers see the pre-walk tip");
    let seqs: Vec<u64> = v.steps.iter().map(|s| s.seq()).collect();
    assert_eq!(seqs, (before.tip_seq + 1..=before.tip_seq + 15).collect::<Vec<_>>());

    let after = chain.snapshot();
    assert_eq!(after.epoch, before.epoch + 1, "one snapshot store");
    assert_eq!((after.tip.tip, after.tip_seq), (a[89], before.tip_seq + 15));
    assert_eq!((after.cl_floor.map(|f| f.id), chain.best_header()), (Some(a[99]), a[89]));
    assert!(b.iter().all(|&id| conflict(&chain, id)));
    assert_eq!(
        r.reset,
        Some(ResetReport {
            reconsidered: vec![a[99]],
            promoted: Vec::new(),
        })
    );
    let dirty: Vec<BlockId> = chain.dirty.ids().to_vec();
    assert_eq!(dirty, vec![a[99]], "only the reset's write is dirty");

    // V's ABC then connects A91..A100 one published step each, trailing the floor until A100;
    // best_header stays the fork (§6.1 quirk 2).
    for &id in &a[90..] {
        let snap = chain.connect_tip(id).map(|_| chain.snapshot()).expect("connects");
        assert_eq!(snap.tip.tip, id);
    }
    assert_eq!(chain.best_header(), a[89]);
    // A no-op pass after the fixed point still stores one snapshot (the composite's).
    let epoch = chain.snapshot().epoch;
    let r = chain.enforce_best_chainlock(&mut Recorder::default()).expect("enforces");
    assert_eq!(r.map(|r| r.marked.len()), Some(0));
    assert_eq!(chain.snapshot().epoch, epoch + 1);
}

/// Core's first `m_best_header` walk-back (`validation.cpp:4089-4091`), in the orphan race where
/// it alone acts: the tip is A5, B6 then L6 arrive on it at equal work, so the best header stays
/// on the first-seen B6, off the active chain. A lock on L6 marks B6 without a disconnect (the
/// `:4107` walk-back never runs), and Core moves the best header back to A5.
#[test]
fn enforce_walks_back_an_off_chain_best_header() {
    let (mut chain, sporks) = chain(ChainMode::Node);
    sporks.set(true);
    chain.check_cl_active_state();
    let a = branch(&mut chain, BlockId(0), 1, 5, T);
    connect_all(&mut chain, &a);
    let b6 = add(&mut chain, a[4], 100, T + 50);
    let l6 = add(&mut chain, a[4], 200, T + 60);
    assert_eq!(chain.best_header(), b6, "the first seen wins the equal-work tie");
    chain.record_chainlock(lock(&chain, l6));
    let mut v = Recorder::default();
    let r = chain.enforce_best_chainlock(&mut v).expect("enforces").expect("a floor");
    assert_eq!((r.marked, r.disconnected.len()), (vec![b6], 0));
    assert_eq!(v.marked, vec![(b6, a[4], false)]);
    assert_eq!(chain.best_header(), a[4], "validation.cpp:4089-4091");
}

/// The same race in SPV, enforcing against a [`VirtualTip`] held at A5: the best header walks
/// back to A5 and the target is L6.
#[test]
fn spv_enforce_walks_back_an_off_chain_best_header() {
    let (mut chain, sporks) = chain(ChainMode::Spv);
    sporks.set(true);
    chain.check_cl_active_state();
    let a = branch(&mut chain, BlockId(0), 1, 5, T);
    spv_activate(&mut chain, a[4]);
    let b6 = add(&mut chain, a[4], 100, T + 50);
    let l6 = add(&mut chain, a[4], 200, T + 60);
    assert_eq!(chain.best_header(), b6);
    let mut vt = chain.virtual_tip();
    let r = enforced(chain.apply_chainlock(lock(&chain, l6), &mut vt).expect("applies"));
    assert_eq!((r.marked, vt.disconnected()), (vec![b6], &[][..]));
    assert_eq!(chain.best_header(), a[4], "validation.cpp:4089-4091");
    assert_eq!(chain.fork_target(&vt).expect("a target").id, l6);
}

/// A node on B1…B3 with ChainLocks on and the floor on the header-only A3, A1 being B1's sibling.
fn locked_off_tip() -> (HeaderChain, Vec<BlockId>, Vec<BlockId>) {
    let (mut chain, sporks) = chain(ChainMode::Node);
    let a = branch(&mut chain, BlockId(0), 1, 3, T);
    let b = branch(&mut chain, BlockId(0), 100, 3, T + 1);
    connect_all(&mut chain, &b);
    sporks.set(true);
    chain.check_cl_active_state();
    chain.record_chainlock(lock(&chain, a[2]));
    (chain, a, b)
}

/// §9.1: `connect_tip` is refused inside `enforce_block`, and the composite state is put back.
#[test]
fn connect_inside_enforce_block_is_refused() {
    let (mut chain, a, b) = locked_off_tip();
    let epoch = chain.snapshot().epoch;
    let r = chain.enforce_best_chainlock(&mut Recorder {
        connect: Some(a[0]),
        ..Recorder::default()
    });
    assert!(matches!(r, Err(ApplyError::Host(ChainError::Internal(_)))), "{r:?}");
    assert!(chain.composite.is_none());
    assert_eq!(chain.snapshot().epoch, epoch + 1, "stored also after an error");
    assert_eq!(tip(&chain), b[2]);
    chain.connect_tip(a[0]).expect_err("prev is not the tip");
}

/// A host that panics mid-walk: the `Composite` guard puts the stepped state back into the
/// mutex and trips the latch (D-C6-1), so neither the writer nor the mutex keeps the vacant
/// state.
#[test]
fn enforce_block_unwind_restores_the_tip_state() {
    let (mut chain, _, b) = locked_off_tip();
    let seq = chain.snapshot().tip_seq;
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = chain.enforce_best_chainlock(&mut Recorder {
            panic: true,
            ..Recorder::default()
        });
    }));
    assert!(unwound.is_err());
    assert!(chain.composite.is_none());
    assert!(chain.svc.fail.is_tripped(), "the unwind is a fail-stop");
    assert_eq!(tip(&chain), b[1], "the step stands");
    let step = chain.disconnect_tip().expect("the mutex holds the real state");
    assert_eq!((step.seq(), tip(&chain)), (seq + 2, b[0]));
}

/// A-39's SB4 half: a host callback that calls a mutex-taking API panics in debug builds.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "publish_status inside enforce_block")]
fn reentrant_publish_status_panics() {
    let (mut chain, ..) = locked_off_tip();
    let mut v = Recorder {
        reenter: true,
        ..Recorder::default()
    };
    let _ = chain.enforce_best_chainlock(&mut v);
}

// ---- ResetBlockFailureFlags (§7.4) ----------------------------------------------------------

/// Loop 1 clears every not-`IsValid()` descendant-or-self; loop 2 walks to genesis, clearing
/// FAILED (and CONFLICT only with `ignore_chainlocks`) and promoting `FAILED_CHILD` children.
/// Every write is dirty; `m_failed_blocks` follows.
#[test]
fn reset_failure_flags_follows_core() {
    for ignore in [false, true] {
        let (mut chain, _) = chain(ChainMode::Node);
        let a = branch(&mut chain, BlockId(0), 1, 6, T);
        let side = add(&mut chain, a[1], 100, T + 50);
        let set = |c: &mut HeaderChain, id, bits| {
            c.modify(id, |g| g.status = Status(g.status.bits() | bits));
        };
        set(&mut chain, a[1], Status::CONFLICT_CHAINLOCK);
        set(&mut chain, side, Status::FAILED_CHILD);
        set(&mut chain, a[3], Status::FAILED_VALID);
        set(&mut chain, a[4], Status::FAILED_CHILD);
        chain.failed.insert(a[3]);
        chain.take_dirty();

        let r = chain.reset_failure_flags(a[3], ignore);
        // Loop 1: a[3] and its descendants (all header-only, so not IsValid()); loop 2: a[1].
        assert_eq!(r.reconsidered, vec![a[3], a[4], a[5], a[1]]);
        // a[1]'s children: a[2] has no FAILED_CHILD, side does.
        assert_eq!(r.promoted, vec![side]);
        let s = |id| chain.writer_status(id).expect("known");
        assert!(!s(a[3]).has(Status::FAILED_MASK) && !s(a[4]).has(Status::FAILED_MASK));
        assert_eq!(s(a[1]).has(Status::CONFLICT_CHAINLOCK), !ignore);
        assert_eq!(s(side).bits() & Status::FAILED_MASK, Status::FAILED_MASK);
        assert!(!chain.failed.contains(a[3]) && chain.failed.contains(side));
        let mut dirty = chain.dirty.ids().to_vec();
        dirty.sort();
        let mut want = vec![a[1], a[3], a[4], a[5], side];
        want.sort();
        assert_eq!(dirty, want);
    }
}

/// D18: SPV never resets.
#[test]
#[should_panic(expected = "SPV never resets failure flags")]
fn spv_never_resets_failure_flags() {
    let (mut chain, _) = chain(ChainMode::Spv);
    chain.reset_failure_flags(BlockId(0), false);
}

// ---- SPV: the step recipe, VirtualTip and fork_target (§6.2, §9.1) ---------------------------

/// Sol r6's three branches from 80 (A-21, A-21a, A-20): A100 headers, C81…C105 stale, B81…B110
/// active (SPV's active chain is header-most-work). A lock on A100 in one step marks C81 and B81,
/// moves the cursor (not the tip) down B110…B81 and walks `best_header` back to A80, as Core
/// does; C82…C105 stay unflagged. The target is A100, never C105 (it fails (a) and (b′)), and
/// the step publishes once. The fixed-point variant (DEC-167) re-runs enforcement on the same
/// `VirtualTip` until a pass changes nothing and ends in the same state.
#[test]
fn spv_b110_a100_walkback() {
    for fixed_point in [false, true] {
        let (mut chain, sporks) = chain(ChainMode::Spv);
        let a = branch(&mut chain, BlockId(0), 1, 100, T);
        let c = branch(&mut chain, a[79], 1000, 25, T + 300);
        let b = branch(&mut chain, a[79], 2000, 30, T + 400);
        spv_activate(&mut chain, b[29]);
        sporks.set(true);
        let (published, epoch) = (chain.reader(), chain.snapshot().epoch);
        let a100 = lock(&chain, a[99]);

        let (vt, target, change) = spv_step(&mut chain, |chain, vt| {
            let out = chain.apply_chainlock(a100, vt).expect("applies");
            let ClOutcome::Floor {
                id,
                enforced: Some(r),
            } = out
            else {
                panic!("{out:?}");
            };
            assert_eq!(id, a[99]);
            assert_eq!(r.marked, vec![c[0], b[0]], "id order (D9)");
            assert_eq!((r.disconnected.len(), vt.disconnected().len()), (0, 30), "DEC-187");
            assert_eq!(r.reset, None, "SPV never resets (D18)");
            // Nothing is published before stage 4.
            assert_eq!(published.snapshot().tip.tip, b[29]);
            assert!(
                !published.entry(b[5]).expect("resident").status.has(Status::CONFLICT_CHAINLOCK)
            );
            if fixed_point {
                let state = |c: &HeaderChain| {
                    let marks: Vec<bool> = c.ids().map(|id| conflict(c, id)).collect();
                    (marks, c.best_header(), c.cl_floor())
                };
                let mut passes = 1;
                loop {
                    let before = state(chain);
                    chain.check_cl_active_state();
                    chain.enforce_best_chainlock(vt).expect("enforces").expect("a floor");
                    passes += 1;
                    if state(chain) == before {
                        break;
                    }
                }
                assert_eq!(passes, 2, "the second pass changes nothing");
            }
        });
        // The later passes' notifications are not counted (A-21a).
        assert_eq!((vt.cursor(), &vt.notified_tips()[..2]), (a[79], &[b[29], a[79]][..]));
        assert_eq!(vt.disconnected().first(), Some(&b[29]));
        assert_eq!(
            target,
            ForkTarget {
                id: a[99],
                floor: FloorRelation::Descends,
            }
        );
        assert_eq!(change.snap.epoch, epoch + 1, "one publication");
        assert_eq!((change.snap.tip.tip, chain.best_header()), (a[99], a[79]));
        assert!(b.iter().all(|&id| conflict(&chain, id)));
        assert!(conflict(&chain, c[0]) && !c[1..].iter().any(|&id| conflict(&chain, id)));
        assert!(!a.iter().any(|&id| conflict(&chain, id)));
    }
}

/// A-22 and A-23's fixed counterexample (A-23's every-store proptest is SB5's, D-C6-8): a lock on
/// the active A100 marks nothing, and a heavier branch forking at 95 is never the target while
/// ChainLocks are observed on; stage 4 refuses it.
/// Observed off, the floor is kept but not applied: the target is B110 and the published
/// snapshot (flag off, floor A100) passes §9.1's predicate (A-29 (iii)).
#[test]
fn spv_lock_on_active_heavier_below() {
    let (mut chain, sporks) = chain(ChainMode::Spv);
    let a = branch(&mut chain, BlockId(0), 1, 100, T);
    spv_activate(&mut chain, a[99]);
    sporks.set(true);
    let a100 = lock(&chain, a[99]);
    let (vt, target, _) = spv_step(&mut chain, |chain, vt| {
        let out = chain.apply_chainlock(a100, vt).expect("applies");
        assert!(
            matches!(out, ClOutcome::Floor { enforced: Some(ref r), .. } if r.marked.is_empty())
        );
    });
    assert_eq!(
        (vt.notified_tips().len(), target.id, target.floor),
        (0, a[99], FloorRelation::Descends)
    );
    let b = branch(&mut chain, a[94], 2000, 15, T + 300);
    let (_, target, _) = spv_step(&mut chain, |_, _| {});
    assert_eq!(target.id, a[99], "B110 conflicts with the floor (b′)");
    assert_eq!(chain.best_header(), b[14]);
    chain.tip_plan(b[14]).expect("a plan");
    refused(&mut chain, b[14]);

    sporks.set(false);
    let (_, target, change) = spv_step(&mut chain, |chain, _| {
        assert_eq!(chain.check_cl_active_state(), ClActiveState::TurnedOff);
    });
    assert_eq!(
        target,
        ForkTarget {
            id: b[14],
            floor: FloorRelation::Unenforced,
        }
    );
    let snap = change.snap;
    assert_eq!((snap.tip.tip, snap.cl_enabled_observed), (b[14], false));
    assert_eq!(snap.cl_floor.map(|f| f.id), Some(a[99]));
}

/// §6.2's refusal: a debug assertion, `ConflictsWithFloor` in release.
fn refused(chain: &mut HeaderChain, to: BlockId) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| chain.set_active_tip(to)));
    #[cfg(debug_assertions)]
    assert!(r.is_err(), "debug builds assert");
    #[cfg(not(debug_assertions))]
    assert_eq!(
        r.expect("release builds return").err(),
        Some(ChainError::ConflictsWithFloor {
            to,
            floor: chain.cl_floor().expect("a floor"),
        })
    );
}

/// A-29 (ix) and (x) in SPV, which cannot record while off nor marks a lock on its active chain:
/// A81…A95 active, a lock on B100 pending on its unknown header; the B headers arrive late, move
/// the floor, and the step enforces it: A81…A95 marked, the target B110. Then off → on resets,
/// and a lock on A90, lower than the kept B100, marks B81 and leaves the kept A81 mark: the
/// target A80 trails the floor (DEC-136), as Core's tip does (`a29-x-kept-mark-trailing`).
#[test]
fn spv_spork19_reset_then_lower_lock() {
    let (mut chain, sporks) = chain(ChainMode::Spv);
    let a = branch(&mut chain, BlockId(0), 1, 95, T);
    spv_activate(&mut chain, a[94]);
    sporks.set(true);
    let b100 = ChainLockRef {
        height: 100,
        hash: crate::testing::fake_hash(2019),
    };
    let (_, target, _) = spv_step(&mut chain, |chain, vt| {
        assert_eq!(chain.apply_chainlock(b100, vt), Ok(ClOutcome::Pending));
    });
    assert_eq!((target.id, target.floor), (a[94], FloorRelation::NoFloor));

    let (vt, target, _) = spv_step(&mut chain, |chain, vt| {
        let b = branch(chain, a[79], 2000, 30, T + 400);
        let floor = chain.accepted_block_header(b[19]).expect("the late header");
        assert_eq!(floor, Some(b[19]));
        let r = chain.enforce_best_chainlock(vt).expect("enforces").expect("a floor");
        assert_eq!((r.marked, vt.disconnected().len()), (vec![a[80]], 15));
    });
    let b110 = chain.best_header();
    assert_eq!((vt.cursor(), target.id, target.floor), (a[79], b110, FloorRelation::Descends));
    assert!(a[80..].iter().all(|&id| conflict(&chain, id)));

    sporks.set(false);
    spv_step(&mut chain, |chain, _| {
        chain.check_cl_active_state();
    });
    sporks.set(true);
    let a90 = lock(&chain, a[89]);
    let (vt, target, change) = spv_step(&mut chain, |chain, vt| {
        let out = chain.apply_chainlock(a90, vt).expect("applies");
        assert!(
            matches!(
                out,
                ClOutcome::Floor {
                    enforced: Some(_),
                    ..
                }
            ),
            "{out:?}"
        );
    });
    let b81 = chain.reader().ancestor(b110, 81).expect("B81");
    assert!(conflict(&chain, b81) && conflict(&chain, a[80]), "the kept mark stays");
    assert_eq!(vt.notified_tips(), &[a[79]]);
    assert_eq!(
        target,
        ForkTarget {
            id: a[79],
            floor: FloorRelation::Trails,
        }
    );
    let snap = change.snap;
    assert_eq!((snap.tip.tip, snap.cl_floor.map(|f| f.id)), (a[79], Some(a[89])));

    // A-37 while the tip trails the floor: a timer step's walk re-marks B81, already
    // conflicting, so the step writes no status row (n8's empty step, no commit) and stage 4
    // moves nothing.
    let mut vt = chain.virtual_tip();
    let r = chain.enforce_best_chainlock(&mut vt).expect("enforces").expect("a floor");
    assert_eq!((r.marked, vt.disconnected().len()), (vec![b81], 0));
    assert_eq!(chain.overlay_marks().len(), 0, "an unchanged status is no change");
    assert!(chain.dirty.ids().is_empty());
    let target = chain.fork_target(&vt).expect("a target");
    assert_eq!(target.id, a[79]);
    let change = spv_activate(&mut chain, target.id);
    assert!(change.disconnected.is_empty() && change.connected.is_empty());
    assert_eq!(change.snap.tip_seq, snap.tip_seq);
}

/// A-29 (iv): SPV restores its observed flag at `open` and does not reset while the spork stays
/// on; the restored state rides on a status snapshot.
#[test]
fn spv_restores_cl_state_without_reset() {
    let (mut chain, sporks) = chain(ChainMode::Spv);
    let a = branch(&mut chain, BlockId(0), 1, 10, T);
    spv_activate(&mut chain, a[9]);
    let cl = ClPersisted {
        best: Some(lock(&chain, a[7])),
        floor: Some(a[7]),
        enabled_observed: true,
    };
    let epoch = chain.snapshot().epoch;
    chain.restore_cl_state(cl).expect("restores");
    let snap = chain.snapshot();
    assert_eq!((snap.epoch, snap.cl_enabled_observed), (epoch + 1, true));
    assert_eq!(snap.cl_floor.map(|f| f.id), Some(a[7]));
    sporks.set(true);
    assert_eq!(chain.check_cl_active_state(), ClActiveState::Unchanged);
    assert_eq!(chain.cl_state(), cl);
    let unknown = ClPersisted {
        floor: Some(BlockId(999)),
        ..cl
    };
    assert_eq!(chain.restore_cl_state(unknown), Err(ChainError::UnknownBlock(BlockId(999))));
    // Refused inside a hold, before anything is restored.
    let other = ClPersisted {
        floor: Some(a[2]),
        ..cl
    };
    let mut hold = chain.hold_tip();
    assert!(matches!(hold.chain().restore_cl_state(other), Err(ChainError::Internal(_))));
    assert_eq!(hold.chain().cl_state(), cl);
    drop(hold);

    let (mut node, _) = self::chain(ChainMode::Node);
    assert!(matches!(node.restore_cl_state(cl), Err(ChainError::Config(_))));
    let vt = VirtualTip {
        cursor: BlockId(0),
        disconnected: Vec::new(),
        notified_tips: Vec::new(),
    };
    assert!(matches!(node.fork_target(&vt), Err(ChainError::Config(_))));
}

/// A-37's empty step: a timer step with nothing to enforce targets the tip, and stage 4 to it
/// runs no step.
#[test]
fn spv_quiet_timer_step_moves_nothing() {
    let (mut chain, sporks) = chain(ChainMode::Spv);
    let a = branch(&mut chain, BlockId(0), 1, 10, T);
    spv_activate(&mut chain, a[9]);
    sporks.set(true);
    let a10 = lock(&chain, a[9]);
    spv_step(&mut chain, |chain, vt| {
        chain.apply_chainlock(a10, vt).expect("applies");
    });
    let seq = chain.snapshot().tip_seq;
    let (vt, target, change) = spv_step(&mut chain, |chain, vt| {
        chain.check_cl_active_state();
        let r = chain.enforce_best_chainlock(vt).expect("enforces").expect("a floor");
        assert!(r.marked.is_empty() && vt.disconnected().is_empty());
    });
    assert_eq!((vt.cursor(), target.id), (a[9], a[9]));
    assert!(change.disconnected.is_empty() && change.connected.is_empty());
    assert_eq!(change.snap.tip_seq, seq);
}

// ---- A-27: fork_target against SB0's predicate ----------------------------------------------

mod brute_force {
    use proptest::prelude::*;

    use super::*;

    /// SB0's predicate by brute force over every entry: the most-work entry `c` (lower id on a
    /// tie) with (a) no FAILED or CONFLICT entry on its path down to the cursor's chain, and
    /// (b′) no conflict with the floor while observed. The cursor's own chain always qualifies.
    fn brute(chain: &HeaderChain, cursor: BlockId) -> BlockId {
        let r = chain.reader();
        let floor = chain.cl_floor.filter(|_| chain.cl_enabled_observed());
        let on_cursor = |x: BlockId| {
            let h = r.entry(x).expect("resident").height;
            r.ancestor(cursor, h) == Some(x)
        };
        let conflicts = |x: BlockId, f: BlockRef| {
            let h = r.entry(x).expect("resident").height;
            r.ancestor(f.id, h) != Some(x) && r.ancestor(x, f.height) != Some(f.id)
        };
        let eligible = |c: BlockId| {
            let mut x = c;
            while !on_cursor(x) {
                if chain
                    .writer_status(x)
                    .expect("known")
                    .has(Status::FAILED_MASK | Status::CONFLICT_CHAINLOCK)
                {
                    return false;
                }
                x = r.entry(x).expect("resident").prev.expect("above the root");
            }
            floor.is_none_or(|f| !conflicts(c, f))
        };
        let on_chain = |c: BlockId| on_cursor(c) && floor.is_none_or(|f| !conflicts(c, f));
        chain
            .ids()
            .filter(|&c| eligible(c) || on_chain(c))
            .max_by_key(|&c| (r.entry(c).expect("resident").chain_work, std::cmp::Reverse(c)))
            .expect("genesis qualifies")
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// A-27: random trees (each header on a random earlier one), random marks, a random lock
        /// and spork state, a random enforcement on one shared cursor: the leaf answer equals the
        /// brute-force scan relative to that cursor.
        #[test]
        fn prop_ft_leaves_vs_brute_force(
            parents in prop::collection::vec(any::<prop::sample::Index>(), 1..60),
            marks in prop::collection::vec((any::<prop::sample::Index>(), any::<bool>()), 0..6),
            locked in any::<Option<prop::sample::Index>>(),
            on in any::<bool>(),
            enforce in any::<bool>(),
            active in any::<prop::sample::Index>(),
        ) {
            let (mut chain, sporks) = chain(ChainMode::Spv);
            let mut ids = vec![BlockId(0)];
            for (k, p) in parents.iter().enumerate() {
                let prev = ids[p.index(ids.len())];
                ids.push(add(&mut chain, prev, k as u64 + 1, T + k as u32));
            }
            spv_activate(&mut chain, ids[active.index(ids.len())]);
            for (i, failed) in &marks {
                let bits = if *failed { Status::FAILED_VALID } else { Status::CONFLICT_CHAINLOCK };
                let id = ids[i.index(ids.len())];
                if id != BlockId(0) {
                    chain.set_status(id, |s| Status(s.bits() | bits)).expect("marks");
                }
            }
            sporks.set(on);
            chain.check_cl_active_state();
            let mut vt = chain.virtual_tip();
            if let Some(l) = locked {
                let cl = lock(&chain, ids[l.index(ids.len())]);
                // D18: SPV's enforcement never meets a FAILED locked block.
                if !chain.writer_status(chain.shared.lookup(&cl.hash).expect("held"))
                    .expect("known").has(Status::FAILED_MASK)
                {
                    match enforce {
                        true => drop(chain.apply_chainlock(cl, &mut vt).expect("applies")),
                        false => drop(chain.update_best_chainlock(cl).expect("resident")),
                    }
                }
            }
            let got = chain.fork_target(&vt).expect("a target").id;
            prop_assert_eq!(got, brute(&chain, vt.cursor()));
        }
    }
}
