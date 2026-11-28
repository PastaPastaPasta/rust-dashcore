//! BL-989: a reorg's lowering commits before its activation (SB14 D7), so a cut between them,
//! or a power loss before the step's Buffered commit is synced, reopens at a committed prefix of
//! the old branch. `open` keeps that tip, and the next step moves it to the fork target. A tip
//! off the top of the `active` rows, or off its fork target's branch, is still corruption.

use super::cl::lock_on;
use super::*;
use crate::step::ActiveDiff;

/// A1…A6 on the pack, committed and synced, their ids, and B3…B8 on A2, heavier and mined only.
fn old_branch() -> (Arc<ChainSetup>, MemHeaderStore, Vec<BlockId>, Vec<Header>) {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = committed(&p, &pack, services(&p));
    let a = mine_n(&p, pack.hash, START, 6);
    real_step(&mut chain, &mut store, &a);
    store.sync_wal().expect("sync");
    let ids = a.iter().map(|h| id_of(&chain, h)).collect();
    let b = mine_n(&p, a[1].hash(), at(2) + 3, 6);
    (p, store, ids, b)
}

/// The store's lowering batches down to `A{k}`, each Durable: its `active` rows above `A{k}`
/// gone and `meta.tip` at it, the step's records never committed.
fn lower_to(store: &mut MemHeaderStore, a: &[BlockId], k: usize) {
    for top in (k..a.len()).rev() {
        let tail = StepTail {
            active: Some(ActiveDiff {
                from_height: ANCHOR + top as u32 + 1,
                new_tip: a[top - 1],
                new_height: ANCHOR + top as u32,
                old_height: ANCHOR + top as u32 + 1,
            }),
            cl: None,
            meta: MetaDelta {
                tip: a[top - 1],
                ..meta_delta(store)
            },
        };
        commit_tail(store, &[], tail);
        store.sync_wal().expect("sync");
    }
}

/// A cut mid-lowering at A3, and one after the lowering's last batch at the fork point A2 (also
/// what a power loss after the whole step boots): `open` keeps the prefix tip, with A6 as
/// `best_header` and fork target and the latch untripped. The next step, with nothing new, lands
/// on A6, and B, delivered again, then wins.
#[test]
fn open_at_an_old_branch_prefix_steps_to_the_fork_target() {
    for k in [3, 2] {
        let (p, mut store, a, b) = old_branch();
        lower_to(&mut store, &a, k);
        let (svc, latch) = latched(&p);
        let mut r = Spv::open(p.clone(), ChainConfig::spv(&p, 10), svc, store.clone())
            .expect("an old-branch prefix opens");
        assert_eq!(r.private_tip().expect("a tip").0.id, a[k - 1], "A{k}");
        assert_eq!(r.best_header(), a[5], "A{k}");
        assert_eq!(r.fork_target(&r.virtual_tip()).expect("a target").id, a[5], "A{k}");
        assert_eq!(r.feeds().feed_snapshot().read().tip.tip_hash, r.private_tip().unwrap().0.hash);

        stages(&mut r, &mut store);
        check(&r, &store);
        assert_eq!(r.private_tip().expect("a tip").0.id, a[5], "A{k}: the next step");
        reopen_same(&p, &r, &store);

        real_step(&mut r, &mut store, &b);
        check(&r, &store);
        assert_eq!(r.private_tip().expect("a tip").0.id, id_of(&r, &b[5]), "A{k}: B wins");
        assert!(!latch.is_tripped(), "A{k}");
        reopen_same(&p, &r, &store);
    }
}

/// At the A3 prefix, `active` rows that the tip does not top are still corruption: a row past it
/// (A4's), and the tip's own row missing.
#[test]
fn open_refuses_a_prefix_tip_off_the_active_top() {
    let (p, mut store, a, _) = old_branch();
    lower_to(&mut store, &a, 3);
    let a4 = a[3];
    let cases: [RowsEdit; 2] =
        [Box::new(move |r| r.push((ANCHOR + 4, a4))), Box::new(|r| _ = r.pop())];
    for edit in cases {
        let tampered = Tampered {
            active: edit,
            ..Tampered::new(&store)
        };
        assert_eq!(
            corruption_in(&p, tampered),
            Corruption::ActiveMismatch {
                height: ANCHOR + 3
            }
        );
    }
}

/// A1…A6 active and C3, C4 on A2 stored, lighter, all synced: C4's id and header.
fn third_branch() -> (Arc<ChainSetup>, MemHeaderStore, BlockId, Header) {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = committed(&p, &pack, services(&p));
    let a = mine_n(&p, pack.hash, START, 6);
    real_step(&mut chain, &mut store, &a);
    let c = mine_n(&p, a[1].hash(), at(2) + 5, 2);
    real_step(&mut chain, &mut store, &c);
    store.sync_wal().expect("sync");
    let c4 = id_of(&chain, &c[1]);
    (p, store, c4, c[1])
}

/// A state no commit order leaves: the tip C4 on a third branch, whose fork target A6 does not
/// descend from it. It is refused with the latch tripped.
#[test]
fn open_refuses_a_tip_on_a_third_branch() {
    let (p, mut store, c4, _) = third_branch();
    let tail = StepTail {
        active: Some(ActiveDiff {
            from_height: ANCHOR + 3,
            new_tip: c4,
            new_height: ANCHOR + 4,
            old_height: ANCHOR + 6,
        }),
        cl: None,
        meta: MetaDelta {
            tip: c4,
            ..meta_delta(&store)
        },
    };
    commit_tail(&mut store, &[], tail);
    assert_eq!(corruption(&p, &store), Corruption::TipOffTargetBranch(c4));
}

/// A state no commit order leaves: A6's rows under a stored ChainLock on C4, whose fork target
/// C4 does not descend from A6. It is refused with the latch tripped.
#[test]
fn open_refuses_rows_against_the_stored_chainlock() {
    let (p, mut store, c4, c4_header) = third_branch();
    let m = meta_delta(&store);
    let cl = ClPersisted {
        best: Some(lock_on(&c4_header, ANCHOR + 4)),
        floor: Some(c4),
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
    assert_eq!(corruption(&p, &store), Corruption::TipOffTargetBranch(m.tip));
}
