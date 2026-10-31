//! F7 over SB4 C6's ChainLock state: `open` restores the rows and the observed flag (D-C6-3),
//! reproduces cold and resident marks and the fork target (A-30's and R6-M1's reopen halves),
//! and sets `max_cold_leaf_work`, so a later `fork_target` still reads the store's leaves
//! (D-C6-4).

use super::*;
use crate::chain::cl_tests::enforced;
use crate::chain::sb5::step_tests::{Stage, committed_w, drive};
use crate::{AnchorPack, FloorRelation, ForkTarget, StaticSporks, VirtualTip};

/// Regtest's services with ChainLocks observed on.
pub(super) fn cl_services(p: &Arc<ChainSetup>) -> Services {
    Services {
        sporks: Arc::new(StaticSporks::for_network(p, Some(true))),
        ..services(p)
    }
}

/// Regtest at window `w` with ChainLocks observed on, its pack committed.
fn cl_chain(w: u32) -> (Arc<ChainSetup>, AnchorPack, Spv, MemHeaderStore) {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (chain, store) = committed_w(&p, &pack, cl_services(&p), w);
    (p, pack, chain, store)
}

pub(super) fn reopen_w(p: &Arc<ChainSetup>, svc: &Services, store: &MemHeaderStore, w: u32) -> Spv {
    Spv::open(p.clone(), ChainConfig::spv(p, w), svc.clone(), store.clone()).expect("open")
}

/// [`reopen_w`], checked against `chain` ([`holds_the_same`]) and its `best_header` against
/// P4's scan.
pub(super) fn reopen_same_w(chain: &Spv, store: &MemHeaderStore, w: u32) -> Spv {
    let b = reopen_w(&chain.params, &chain.svc, store, w);
    holds_the_same(chain, &b);
    assert_eq!(b.best_header(), scan_best(store), "P4: best_header");
    b
}

/// [`reopen_same_w`] with [`same`]'s strict comparison, for a chain whose tip only rose and
/// whose `best_header` no mark walked back.
fn reopen_exact_w(chain: &Spv, store: &MemHeaderStore, w: u32) -> Spv {
    let b = reopen_w(&chain.params, &chain.svc, store, w);
    same(chain, &b);
    assert_eq!(b.best_header(), scan_best(store), "P4: best_header");
    b
}

/// One SPV step with ChainLocks (SB4 §9.1): one `VirtualTip`, `accept` (when there are headers),
/// the step's calls, then the recipe's remaining stages.
fn cl_step(
    chain: &mut Spv,
    store: &mut MemHeaderStore,
    headers: &[Header],
    calls: impl FnOnce(&mut Spv, &mut VirtualTip),
) -> (VirtualTip, ForkTarget) {
    let calls = |c: &mut Spv, vt: &mut VirtualTip| {
        if !headers.is_empty() {
            assert_eq!(c.accept(batch(headers), None).expect("accept").stop, None);
        }
        calls(c, vt);
    };
    drive(chain, store, calls, Stage::Done).expect("the step")
}

pub(super) fn lock_on(h: &Header, height: u32) -> ChainLockRef {
    ChainLockRef {
        height,
        hash: h.hash(),
    }
}

pub(super) fn conflict(chain: &Spv, id: BlockId) -> bool {
    let e = chain.reader().try_entry(id).expect("read").expect("held");
    e.status.has(Status::CONFLICT_CHAINLOCK)
}

/// D-C6-3: the lock's best and floor and the observed flag come back through
/// `restore_cl_state`, and the reopened snapshot publishes them.
#[test]
fn open_restores_the_chainlock_rows_and_observed_flag() {
    let (p, pack, mut chain, mut store) = cl_chain(10);
    let a = mine_n(&p, pack.hash, START, 30);
    cl_step(&mut chain, &mut store, &a, |chain, vt| {
        enforced(chain.apply_chainlock(lock_on(&a[24], ANCHOR + 25), vt).expect("applies"));
    });
    assert!(chain.cl_enabled_observed());
    let b = reopen_exact_w(&chain, &store, 10);
    let floor = id_of(&b, &a[24]);
    assert_eq!(b.cl_floor(), Some(floor));
    assert!(b.cl_enabled_observed());
    let snap = b.snapshot();
    assert_eq!(snap.cl_floor.map(|f| f.id), Some(floor));
    assert!(snap.cl_enabled_observed);
}

/// A `meta.cl.floor` at or above `next_id` is refused (`restore_cl_state`'s `UnknownBlock`).
#[test]
fn open_refuses_a_chainlock_floor_not_held() {
    let (p, chain, mut store, _) = trunk10();
    let gone = BlockId(chain.next_id + 3);
    drop(chain);
    let m = meta_delta(&store);
    let cl = ClPersisted {
        best: None,
        floor: Some(gone),
        enabled_observed: true,
    };
    commit_tail(
        &mut store,
        &[],
        StepTail {
            active: None,
            cl: Some(cl),
            meta: m,
        },
    );
    assert_eq!(corruption(&p, &store), Corruption::ClFloor(gone));
}

/// SB4's A-21 chain (A100, B110 active, C105 stale, all from A80) at window `w`, then the lock
/// on A100 with A81…A100 in one step. Returns the chain, its store and the three branches.
fn three_branches(w: u32) -> (Spv, MemHeaderStore, [Vec<Header>; 3]) {
    let (p, pack, mut chain, mut store) = cl_chain(w);
    let a = mine_n(&p, pack.hash, START, 100);
    let c = mine_n(&p, a[79].hash(), at(80) + 3, 25);
    let b = mine_n(&p, a[79].hash(), at(80) + 5, 30);
    for chunk in a[..80].chunks(20) {
        real_step(&mut chain, &mut store, chunk);
    }
    real_step(&mut chain, &mut store, &c);
    real_step(&mut chain, &mut store, &b);
    assert_eq!(chain.private_tip().expect("a tip").0.id, id_of(&chain, &b[29]));
    let (vt, target) = cl_step(&mut chain, &mut store, &a[80..], |chain, vt| {
        let r = enforced(chain.apply_chainlock(lock_on(&a[99], ANCHOR + 100), vt).expect("lock"));
        assert_eq!((r.disconnected.len(), vt.disconnected().len()), (0, 30), "DEC-187");
    });
    let a100 = id_of(&chain, &a[99]);
    assert_eq!(
        (vt.cursor(), target.id, target.floor),
        (id_of(&chain, &a[79]), a100, FloorRelation::Descends)
    );
    (chain, store, [a, b, c])
}

/// R6-M1 across a restart, every branch resident (w = 40): the reopened chain has the marks
/// (B81 and C81 by enforcement, B82…B110 too), the floor, the tip A100 as its fork target, and
/// E5's CONFLICT-eligible B110 as `best_header`. Extending C after the reopen still gives A100.
#[test]
fn r6m1_reopen_a100() {
    let (chain, mut store, [a, b, c]) = three_branches(40);
    let mut r = reopen_same_w(&chain, &store, 40);
    drop(chain);
    let (a100, b110) = (id_of(&r, &a[99]), id_of(&r, &b[29]));
    assert!(r.shared.arena.imm(id_of(&r, &c[0]), |_| ()).is_some(), "C81 is resident");
    assert!(conflict(&r, id_of(&r, &b[0])) && conflict(&r, id_of(&r, &c[0])));
    assert!(b.iter().all(|h| conflict(&r, id_of(&r, h))));
    assert!(!c[1..].iter().any(|h| conflict(&r, id_of(&r, h))));
    assert_eq!(r.cl_floor(), Some(a100));
    assert_eq!(r.best_header(), b110, "E5: CONFLICT is eligible");

    let more = mine_n(&r.params, c[24].hash(), at(105) + 3, 5);
    let (_, target) = cl_step(&mut r, &mut store, &more, |_, _| {});
    assert_eq!(target.id, a100);
    reopen_same_w(&r, &store, 40);
}

/// R6-M1 with the window below the branches' depth (w = 8): B81 and C81 are cold, their marks
/// come back from the committed records, and `fork_target` over cold reads still gives A100
/// after C is extended past B.
#[test]
fn r6m1_reopen_cold() {
    let (chain, mut store, [a, b, c]) = three_branches(8);
    let mut r = reopen_same_w(&chain, &store, 8);
    drop(chain);
    let (a100, b81, c81) = (id_of(&r, &a[99]), id_of(&r, &b[0]), id_of(&r, &c[0]));
    assert!(r.shared.arena.imm(b81, |_| ()).is_none() && r.shared.arena.imm(c81, |_| ()).is_none());
    assert!(conflict(&r, b81) && conflict(&r, c81));
    assert!(!conflict(&r, id_of(&r, &c[24])));

    let more = mine_n(&r.params, c[24].hash(), at(105) + 3, 10);
    let (_, target) = cl_step(&mut r, &mut store, &more, |_, _| {});
    assert_eq!(target.id, a100, "C115 outweighs B110 but C81 is marked");
    reopen_same_w(&r, &store, 8);
}

/// A-30's reopen half: SB4's `spv_cold_mark_b81_c81` stepped through `write_header_step`. The
/// marks the enforcement set on cold B81 and C81 through the overlay are in the reopened
/// chain's cold reads, and its fork target is A100.
#[test]
fn a30_cold_marks_reopen() {
    let (p, pack, mut chain, mut store) = cl_chain(10);
    let a = mine_n(&p, pack.hash, START, 100);
    let c = mine_n(&p, a[79].hash(), at(80) + 3, 30);
    let b = mine_n(&p, a[79].hash(), at(80) + 5, 40);
    for chunk in a.chunks(20) {
        real_step(&mut chain, &mut store, chunk);
    }
    real_step(&mut chain, &mut store, &c[..25]);
    for chunk in b[..30].chunks(10) {
        real_step(&mut chain, &mut store, chunk);
    }
    let (b81, c81) = (id_of(&chain, &b[0]), id_of(&chain, &c[0]));
    assert!(chain.shared.arena.imm(b81, |_| ()).is_none());
    let (_, target) = cl_step(&mut chain, &mut store, &[], |chain, vt| {
        let r = enforced(chain.apply_chainlock(lock_on(&a[99], ANCHOR + 100), vt).expect("lock"));
        assert_eq!(r.marked, vec![c81, b81]);
        assert_eq!(chain.accept(batch(&c[25..]), None).expect("accept").stop, None);
    });
    let a100 = id_of(&chain, &a[99]);
    assert_eq!(target.id, a100);

    let r = reopen_same_w(&chain, &store, 10);
    for id in [b81, c81, id_of(&r, &b[29])] {
        assert!(conflict(&r, id), "{id:?}");
    }
    assert!(!conflict(&r, id_of(&r, &c[29])));
    assert_eq!(r.fork_target(&r.virtual_tip()).expect("a target").id, a100);
}

/// D-C6-4: a cold leaf that only a lock lets win. C6…C45 on A5 left the window when A60 became
/// the tip; after a reopen, a lock on the cold C40 disconnects A to A5 and marks A6. Only the
/// store's leaves hold C45, so the target is C45 only if `open` set `max_cold_leaf_work`.
#[test]
fn reopen_keeps_the_cold_leaf_scan() {
    let (p, pack, mut chain, mut store) = cl_chain(10);
    let a = mine_n(&p, pack.hash, START, 60);
    let c = mine_n(&p, a[4].hash(), at(5) + 3, 40);
    real_step(&mut chain, &mut store, &a[..40]);
    real_step(&mut chain, &mut store, &c);
    real_step(&mut chain, &mut store, &a[40..]);

    let mut r = reopen_exact_w(&chain, &store, 10);
    drop(chain);
    let (a5, c45) = (id_of(&r, &a[4]), id_of(&r, &c[39]));
    assert_eq!(r.private_tip().expect("a tip").0.id, id_of(&r, &a[59]));
    assert!(r.shared.arena.imm(c45, |_| ()).is_none(), "C45 is cold");
    let c45_work = store.by_id(c45).expect("read").expect("stored").chain_work;
    assert!(*r.max_cold_leaf_work.lock() >= c45_work);
    let leaves = store.faults(|f| f.reads(ReadKind::Leaves));
    let (vt, target) = cl_step(&mut r, &mut store, &[], |chain, vt| {
        let r = enforced(chain.apply_chainlock(lock_on(&c[34], ANCHOR + 40), vt).expect("lock"));
        assert_eq!((r.disconnected.len(), vt.disconnected().len()), (0, 55), "DEC-187");
    });
    assert_eq!(vt.cursor(), a5);
    assert!(store.faults(|f| f.reads(ReadKind::Leaves)) > leaves, "the store's leaves were read");
    assert_eq!((target.id, target.floor), (c45, FloorRelation::Descends));
    assert_eq!(r.private_tip().expect("a tip").0.id, c45);
    reopen_same_w(&r, &store, 10);
}
