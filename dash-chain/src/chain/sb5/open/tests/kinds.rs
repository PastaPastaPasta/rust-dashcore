//! SB4 §9.1's recipe on SPV's ChainLock steps: a timer, a received lock, a late header and a
//! reset-only step. A failed commit publishes, fans out and evicts nothing and leaves the store
//! as it was, and the driver trips the latch, as SB13's writer does (A-33, A-35). A crash after
//! any stage reopens to the old whole step or the new one (A-34).

use super::cl::{cl_services, conflict, lock_on, reopen_same_w, reopen_w};
use super::*;
use crate::chain::cl_tests::enforced;
use crate::chain::sb5::step_tests::{Stage, committed_w, drive};
use crate::{
    ChainError, ChainSnapshot as _, ClActiveState, ClOutcome, FeedSource as _, Handoff,
    IndexAccess as _, Needs, Next, StaticSporks, SubscriberFeed, VirtualTip,
};

const W: u32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Timer,
    ReceivedLock,
    LateHeader,
    ResetOnly,
}

const KINDS: [Kind; 4] = [Kind::Timer, Kind::ReceivedLock, Kind::LateHeader, Kind::ResetOnly];

/// A chain ready for `kind`'s step, its store and latch, and A1…A40. B31…B45 hang off A30, so
/// B45 outweighs A40; the lock is always on A35.
struct Fx {
    chain: Spv,
    store: MemHeaderStore,
    latch: Arc<Latch>,
    a: Vec<Header>,
}

fn a35(a: &[Header]) -> ChainLockRef {
    lock_on(&a[34], ANCHOR + 35)
}

fn full(
    chain: &mut Spv,
    store: &mut MemHeaderStore,
    calls: impl FnOnce(&mut Spv, &mut VirtualTip),
) {
    drive(chain, store, calls, Stage::Done).expect("the step");
}

/// [`fixture`] with its store synced, so that a power loss drops only what follows.
fn synced(kind: Kind) -> Fx {
    let fx = fixture(kind);
    fx.store.sync_wal().expect("sync");
    fx
}

fn fixture(kind: Kind) -> Fx {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let sporks = Arc::new(StaticSporks::for_network(&p, Some(true)));
    let latch = Arc::new(Latch::default());
    let svc = Services {
        sporks: sporks.clone(),
        fail: latch.clone(),
        ..services(&p)
    };
    let (mut chain, mut store) = committed_w(&p, &pack, svc, W);
    let a = mine_n(&p, pack.hash, START, 40);
    let b = mine_n(&p, a[29].hash(), at(30) + 5, 15);
    let lock = a35(&a);
    match kind {
        // B45 active; the lock arrives.
        Kind::ReceivedLock => {
            real_step(&mut chain, &mut store, &a);
            real_step(&mut chain, &mut store, &b);
        }
        // B45 active; the lock arrives before A35, then A31…A40 arrive.
        Kind::LateHeader => {
            real_step(&mut chain, &mut store, &a[..30]);
            real_step(&mut chain, &mut store, &b);
            full(&mut chain, &mut store, |c, vt| {
                assert_eq!(c.apply_chainlock(lock, vt), Ok(ClOutcome::Pending));
            });
        }
        // A40 locked at A35; B arrives while the live spork is off but the observed flag is
        // still on, so acceptance does not refuse B35 and the floor keeps A40 active.
        Kind::Timer => {
            real_step(&mut chain, &mut store, &a);
            full(&mut chain, &mut store, |c, vt| {
                enforced(c.apply_chainlock(lock, vt).expect("applies"));
            });
            sporks.set(false);
            real_step(&mut chain, &mut store, &b);
            assert_eq!(chain.private_tip().expect("a tip").0.id, id_of(&chain, &a[39]));
        }
        // A40 locked at A35; the spork goes off and is observed off, then comes back on.
        Kind::ResetOnly => {
            real_step(&mut chain, &mut store, &a);
            full(&mut chain, &mut store, |c, vt| {
                enforced(c.apply_chainlock(lock, vt).expect("applies"));
            });
            sporks.set(false);
            full(&mut chain, &mut store, |c, _| {
                assert_eq!(c.check_cl_active_state(), ClActiveState::TurnedOff);
            });
            sporks.set(true);
        }
    }
    Fx {
        chain,
        store,
        latch,
        a,
    }
}

/// `kind`'s step calls. The timer observes the spork off, so the floor no longer refuses B45
/// (an observed-flag row and an active change, no status rows); the received lock and the late
/// header disconnect B and mark B31 (status rows, the floor and an active change); the reset
/// clears the lock (the ChainLock rows only).
fn calls(kind: Kind, a: &[Header]) -> impl FnOnce(&mut Spv, &mut VirtualTip) + use<> {
    let (lock, late) = (a35(a), a[30..].to_vec());
    move |c, vt| match kind {
        Kind::Timer => {
            assert_eq!(c.check_cl_active_state(), ClActiveState::TurnedOff);
            assert_eq!(c.enforce_best_chainlock(vt), Ok(None));
        }
        Kind::ReceivedLock => {
            let r = enforced(c.apply_chainlock(lock, vt).expect("applies"));
            assert_eq!((r.marked.len(), vt.disconnected().len()), (1, 15), "B31 is marked");
        }
        Kind::LateHeader => {
            let r = c.accept(batch(&late), None).expect("accept");
            assert!(r.floor_moved.is_some(), "A35 is the floor");
            let r = c.enforce_best_chainlock(vt).expect("enforces").expect("a floor");
            assert_eq!((r.marked.len(), vt.disconnected().len()), (1, 15), "B31 is marked");
        }
        Kind::ResetOnly => {
            assert_eq!(c.check_cl_active_state(), ClActiveState::TurnedOnReset);
        }
    }
}

fn subscribe(chain: &Spv) -> Box<dyn SubscriberFeed> {
    let feeds = chain.feeds();
    let id = feeds.register(Needs::default()).expect("registers");
    let tip = feeds.feed_snapshot().tip();
    match feeds.subscribe_at(id, &tip, &mut || Ok::<(), ()>(())) {
        Ok(Handoff::Live {
            feed,
            ..
        }) => feed,
        other => panic!("subscribe_at: {:?}", other.err()),
    }
}

/// A-33: stage 3 fails mid-rows or at the commit. Nothing is published, fanned out or evicted,
/// the store is as it was, and the latch has tripped. The timer and reset-only steps write only
/// the step's tail.
fn commit_failure(kind: Kind) {
    let modes: &[bool] = match kind {
        Kind::Timer | Kind::ResetOnly => &[false],
        Kind::ReceivedLock | Kind::LateHeader => &[true, false],
    };
    for &rows in modes {
        let Fx {
            mut chain,
            mut store,
            latch,
            a,
        } = fixture(kind);
        let mut feed = subscribe(&chain);
        let (image, snap) = (store.image(), chain.snapshot());
        let (resident, epoch) = (chain.resident.clone(), chain.residency_epoch);
        store.faults(|f| {
            if rows {
                f.fail_rows(1)
            } else {
                f.fail_commit(1)
            }
        });
        let r = drive(&mut chain, &mut store, calls(kind, &a), Stage::Done);
        let ctx = format!("{kind:?}, rows = {rows}");
        assert!(matches!(r, Err(ChainError::Storage(_))), "{ctx}: {r:?}");
        assert!(latch.is_tripped(), "{ctx}");
        assert_eq!(store.image(), image, "{ctx}");
        assert!(Arc::ptr_eq(&chain.snapshot(), &snap), "{ctx}: nothing published");
        assert!(matches!(feed.next(), Next::Empty), "{ctx}: nothing fanned out");
        assert!(resident.is_subset(&chain.resident), "{ctx}: nothing evicted");
        assert_eq!(chain.residency_epoch, epoch, "{ctx}");
    }
}

#[test]
fn spv_step_commit_failure_timer() {
    commit_failure(Kind::Timer);
}

#[test]
fn spv_step_commit_failure_received_lock() {
    commit_failure(Kind::ReceivedLock);
}

#[test]
fn spv_step_commit_failure_late_header() {
    commit_failure(Kind::LateHeader);
}

#[test]
fn spv_step_commit_failure_reset_only() {
    commit_failure(Kind::ResetOnly);
}

/// A-34: each kind's step stops after stage 1, 2, 3 or 4 and the chain is dropped. Before the
/// commit, the reopened chain and store are the old whole step; after it, the new one: the
/// ChainLock rows, observed flag and active change come back only with the status rows written
/// in the same step. A power loss after the cut drops the Buffered commit: the old step again.
#[test]
fn spv_step_crash_cuts() {
    for kind in KINDS {
        let (old, mut new) = (synced(kind), synced(kind));
        let step = drive(&mut new.chain, &mut new.store, calls(kind, &new.a), Stage::Done);
        let to = step.expect("the step").1.id;
        assert_ne!(old.store.image(), new.store.image(), "{kind:?} writes");
        for stop in [Stage::Compute, Stage::Capture, Stage::Commit, Stage::Activate] {
            let Fx {
                mut chain,
                mut store,
                a,
                ..
            } = synced(kind);
            let step = drive(&mut chain, &mut store, calls(kind, &a), stop);
            assert_eq!(step.expect("the stages").1.id, to, "{kind:?}");
            let (p, svc) = (chain.params.clone(), chain.svc.clone());
            drop(chain);
            let crashed = store;
            let want = if stop >= Stage::Commit {
                &new
            } else {
                &old
            };
            assert_eq!(crashed.image(), want.store.image(), "{kind:?} after {stop:?}");
            let r = reopen_w(&p, &svc, &crashed, W);
            holds_the_same(&want.chain, &r);
            assert_eq!(r.best_header(), scan_best(&crashed), "P4");
            drop(r);

            let lost = crashed.power_loss();
            assert_eq!(lost.image(), old.store.image(), "{kind:?} after {stop:?}, power loss");
            let r = reopen_w(&p, &svc, &lost, W);
            holds_the_same(&old.chain, &r);
            assert_eq!(r.best_header(), scan_best(&lost), "P4");
        }
    }
}

/// A-35's latch and reopen halves: SB4's `spv_b110_a120_w10_resident_to_cold` driven by SB5.
/// With the commit failing, the latch trips, B101 stays resident and marked only in the overlay,
/// and the store keeps it unmarked; otherwise B101 is evicted, and the reopened chain reads its
/// committed CONFLICT.
#[test]
fn spv_b110_a120_w10_resident_to_cold_latch() {
    for fail in [true, false] {
        let p = params(NetworkKind::Regtest);
        let pack = regtest_pack(&p);
        let latch = Arc::new(Latch::default());
        let svc = Services {
            fail: latch.clone(),
            ..cl_services(&p)
        };
        let (mut chain, mut store) = committed_w(&p, &pack, svc, W);
        let a = mine_n(&p, pack.hash, START, 120);
        for c in a[..100].chunks(20) {
            real_step(&mut chain, &mut store, c);
        }
        let b = mine_n(&p, a[99].hash(), at(100) + 7, 10);
        real_step(&mut chain, &mut store, &b);
        let b101 = id_of(&chain, &b[0]);
        assert!(chain.shared.arena.imm(b101, |_| ()).is_some());
        let (image, epoch) = (store.image(), chain.snapshot().epoch);
        let unmarked = store.by_id(b101).expect("read").expect("stored").status;

        if fail {
            store.faults(|f| f.fail_commit(1));
        }
        let lock = lock_on(&a[119], ANCHOR + 120);
        let r = drive(
            &mut chain,
            &mut store,
            |c, vt| {
                assert_eq!(c.accept(batch(&a[100..]), None).expect("accept").stop, None);
                let r = enforced(c.apply_chainlock(lock, vt).expect("applies"));
                assert_eq!((r.marked, vt.disconnected().len()), (vec![b101], 10));
            },
            Stage::Done,
        );
        if fail {
            assert!(matches!(r, Err(ChainError::Storage(_))), "{r:?}");
            assert!(latch.is_tripped());
            assert!(chain.shared.arena.imm(b101, |_| ()).is_some(), "B101 is still resident");
            let overlay = chain.writer_status(b101).expect("known");
            assert!(overlay.has(Status::CONFLICT_CHAINLOCK));
            assert_eq!(chain.reader().status(b101), Some(unmarked));
            assert_eq!(store.image(), image);
            assert_eq!(chain.snapshot().epoch, epoch);
            continue;
        }
        assert_eq!(r.expect("the step").1.id, id_of(&chain, &a[119]));
        assert!(!latch.is_tripped());
        assert!(chain.shared.arena.imm(b101, |_| ()).is_none(), "B101 is evicted");
        let r = reopen_same_w(&chain, &store, W);
        assert!(conflict(&r, b101), "the committed record");
    }
}
