//! `MemHeaderStore` (SB5 §3.7): the step commit, layers, power loss and faults; P3's `hdr_leaf`
//! half and P5 against a model of random steps, failures and power losses at random cuts
//! (§10.3).

use std::collections::{BTreeMap, BTreeSet};

use dash_primitives::BlockHeader;
use dash_types::Numeric as _;
use proptest::prelude::*;
use proptest::sample::Index;

use super::*;
use crate::{CHAIN_SCHEMA, ChainLockRef, ClPersisted, MetaDelta, RecordKind, Status};

/// A hash no other `n` produces.
fn hash(n: u64) -> BlockHash {
    let mut h = [0u8; 32];
    h[..8].copy_from_slice(&n.to_le_bytes());
    h[31] = 1;
    BlockHash::from(h)
}

fn record(id: u32, height: u32, prev: BlockHash, n: u64, kind: RecordKind) -> HeaderRecord {
    HeaderRecord {
        id: BlockId(id),
        hash: hash(n),
        height,
        header: BlockHeader {
            prev_hash: prev,
            time: n as u32,
            ..BlockHeader::default()
        },
        skip: None,
        status: Status(0),
        chain_work: crate::Work::from(u64::from(height)),
        time_max: n as u32,
        kind,
    }
}

const ANCHOR_H: u32 = 10;

fn meta() -> ChainMeta {
    ChainMeta {
        schema: CHAIN_SCHEMA,
        anchor: (hash(2), ANCHOR_H),
        anchor_state: AnchorState::Candidate,
        next_id: BlockId(0),
        tip: BlockId(2),
        best_header: BlockId(2),
        sync_seq: 0,
        cl: ClPersisted {
            best: None,
            floor: None,
            enabled_observed: false,
        },
    }
}

/// The anchor pack: context ids 0 and 1 below the anchor, id 2 (hashes `n` = id).
fn pack() -> Vec<HeaderRecord> {
    vec![
        record(0, ANCHOR_H - 2, hash(u64::MAX), 0, RecordKind::Context),
        record(1, ANCHOR_H - 1, hash(0), 1, RecordKind::Context),
        record(2, ANCHOR_H, hash(1), 2, RecordKind::Accepted),
    ]
}

fn tail(active: Option<ActiveDiff>, next_id: u32, tip: u32) -> StepTail {
    let tip = BlockId(tip);
    StepTail {
        active,
        cl: None,
        meta: MetaDelta {
            next_id: BlockId(next_id),
            tip,
            best_header: tip,
        },
    }
}

type Recs = Vec<(HeaderRecord, Option<BlockId>)>;

fn step(s: &mut MemHeaderStore, recs: Recs, t: StepTail, d: Durability) -> Result<(), StoreError> {
    s.begin_step();
    for (r, prev) in recs {
        s.rows(&r.step_rows(prev).collect::<Vec<_>>())?;
    }
    s.commit_step(&t, d)
}

/// A Buffered step of `rows` that leaves the tip at the anchor.
fn commit_rows(s: &mut MemHeaderStore, rows: &[StepRow]) -> Result<(), StoreError> {
    s.begin_step();
    s.rows(rows)?;
    s.commit_step(&tail(None, 3, 2), Durability::Buffered)
}

/// `n` accepted records extending `parent` (id, hash, height), from id `first` with hashes
/// `n0..`.
fn line(parent: (BlockId, BlockHash, u32), first: u32, n0: u64, n: u32) -> Recs {
    let (mut prev, mut prev_hash, mut h) = parent;
    (0..n)
        .map(|i| {
            h += 1;
            let r = record(first + i, h, prev_hash, n0 + u64::from(i), RecordKind::Accepted);
            let out = (r.clone(), Some(prev));
            (prev, prev_hash) = (r.id, r.hash);
            out
        })
        .collect()
}

/// `n` records on the anchor (ids `first..`, hashes `n0..`) and the tail making them active.
fn grow(first: u32, n0: u64, n: u32) -> (Recs, StepTail) {
    let tip = first + n - 1;
    let d = ActiveDiff {
        from_height: ANCHOR_H + 1,
        new_tip: BlockId(tip),
        new_height: ANCHOR_H + n,
        old_height: ANCHOR_H,
    };
    (line((BlockId(2), hash(2), ANCHOR_H), first, n0, n), tail(Some(d), tip + 1, tip))
}

/// A store holding the pack, committed Durable, with the anchor active.
fn opened() -> MemHeaderStore {
    let mut s = MemHeaderStore::new();
    s.reset(meta()).unwrap();
    let recs = pack().into_iter().map(|r| (r, None)).collect();
    let d = ActiveDiff {
        from_height: ANCHOR_H,
        new_tip: BlockId(2),
        new_height: ANCHOR_H,
        old_height: 0,
    };
    step(&mut s, recs, tail(Some(d), 3, 2), Durability::Durable).unwrap();
    s
}

fn ids(recs: &[HeaderRecord]) -> Vec<u32> {
    recs.iter().map(|r| r.id.0).collect()
}

/// Every leaf as `(id, chain_work)`, read in pages of `k` from the id after each full page's
/// last.
fn paged_leaves(s: &MemHeaderStore, k: usize) -> Vec<(BlockId, Work)> {
    paged(k, (BlockId(0), Work::ZERO), |&(id, _)| id, |from, out| s.leaves_from(from, out))
}

/// Every child of `prev`, read in pages of `k`.
fn paged_children(s: &MemHeaderStore, prev: &BlockHash, k: usize) -> Vec<BlockId> {
    paged(k, BlockId(0), |&id| id, |from, out| s.children_from(prev, from, out))
}

fn paged<T: Copy>(
    k: usize,
    zero: T,
    id: impl Fn(&T) -> BlockId,
    read: impl Fn(BlockId, &mut [T]) -> Result<usize, StoreError>,
) -> Vec<T> {
    let (mut out, mut page, mut from) = (Vec::new(), vec![zero; k], BlockId(0));
    loop {
        let n = read(from, &mut page).unwrap();
        out.extend_from_slice(&page[..n]);
        match page[..n].last() {
            Some(last) => from = BlockId(id(last).0 + 1),
            None => return out,
        }
    }
}

/// A page from the leaf and `prev` indexes is the full read's run from `from`, filled past a
/// record a fault hides; past the last, a page is empty.
#[test]
fn pages_match_the_full_reads() {
    let mut s = opened();
    let (recs, t) = grow(3, 3, 6);
    step(&mut s, recs, t, Durability::Buffered).unwrap();
    // Siblings of 4 on 3, each its own leaf.
    let side: Vec<_> = (0..5)
        .map(|k| {
            let r = record(9 + k, ANCHOR_H + 2, hash(3), 100 + u64::from(k), RecordKind::Accepted);
            (r, Some(BlockId(3)))
        })
        .collect();
    step(&mut s, side, tail(None, 14, 8), Durability::Buffered).unwrap();
    let full: Vec<_> = s.leaves().unwrap().iter().map(|r| (r.id, r.chain_work)).collect();
    let kids = ids(&s.children(&hash(3)).unwrap());
    assert_eq!((full.len(), kids.len()), (6, 6));
    for k in 1..=7 {
        assert_eq!(paged_leaves(&s, k), full, "leaves in pages of {k}");
        let paged: Vec<_> = paged_children(&s, &hash(3), k).iter().map(|id| id.0).collect();
        assert_eq!(paged, kids, "children in pages of {k}");
    }
    s.faults(|f| f.corrupt(BlockId(10), Corrupt::Gone));
    let mut page = [BlockId(0); 3];
    assert_eq!(s.children_from(&hash(3), BlockId(5), &mut page).unwrap(), 3);
    assert_eq!(page, [BlockId(9), BlockId(11), BlockId(12)], "filled past the hidden record");
    assert_eq!(s.children_from(&hash(3), BlockId(13), &mut page).unwrap(), 1, "short at the end");
    assert_eq!(s.children_from(&hash(3), BlockId(14), &mut page).unwrap(), 0, "empty past it");
    s.faults(|f| f.short_pages(2));
    let shown: Vec<_> = full.into_iter().filter(|&(id, _)| id != BlockId(10)).collect();
    assert_eq!(paged_leaves(&s, 7), shown, "short pages before the end");
}

#[test]
fn empty_store_has_no_meta() {
    let s = MemHeaderStore::new();
    assert_eq!(s.chain_meta().unwrap(), None);
    assert_eq!(s.max_id().unwrap(), None);
    assert!(s.leaves().unwrap().is_empty());
}

/// The pack lands with the anchor the only leaf and the only active row; reads follow §3.7.
#[test]
fn pack_then_queries() {
    let mut s = opened();
    let m = s.chain_meta().unwrap().unwrap();
    assert_eq!((m.next_id, m.tip), (BlockId(3), BlockId(2)));
    assert_eq!(ids(&s.leaves().unwrap()), [2], "context records are never leaves");
    assert_eq!(s.active_from(0).unwrap(), [(ANCHOR_H, BlockId(2))]);
    let (recs, t) = grow(3, 3, 4);
    step(&mut s, recs, t, Durability::Buffered).unwrap();
    assert_eq!(ids(&s.walk_down(BlockId(6), 3).unwrap()), [6, 5, 4]);
    assert_eq!(ids(&s.walk_down(BlockId(6), 100).unwrap()), [6, 5, 4, 3, 2, 1, 0], "to the pack");
    assert!(s.walk_down(BlockId(99), 3).unwrap().is_empty());
    assert_eq!(ids(&s.from_height(ANCHOR_H + 3).unwrap()), [5, 6]);
    assert_eq!(ids(&s.children(&hash(3)).unwrap()), [4]);
    assert_eq!(s.by_hash(&hash(5)).unwrap().map(|r| r.id), Some(BlockId(5)));
    assert_eq!(s.active_at(ANCHOR_H + 2).unwrap(), Some(BlockId(4)));
    assert_eq!(
        s.active_from(ANCHOR_H + 3).unwrap(),
        [(ANCHOR_H + 3, BlockId(5)), (ANCHOR_H + 4, BlockId(6))]
    );
    assert_eq!(s.max_id().unwrap(), Some(BlockId(6)));
    assert_eq!(ids(&s.leaves().unwrap()), [6]);
}

/// A step's rows are invisible until `commit_step`; `begin_step` discards a step left open.
#[test]
fn rows_invisible_until_commit() {
    let mut s = opened();
    let reader = s.clone();
    let (recs, _) = grow(3, 3, 1);
    s.begin_step();
    s.rows(&recs[0].0.clone().step_rows(Some(BlockId(2))).collect::<Vec<_>>()).unwrap();
    assert_eq!(reader.by_id(BlockId(3)).unwrap(), None);
    assert_eq!(ids(&reader.leaves().unwrap()), [2]);
    commit_rows(&mut s, &[]).unwrap();
    assert_eq!(reader.by_id(BlockId(3)).unwrap(), None, "the discarded step never lands");
}

/// Every failure leaves the committed tables byte-identical (SB4 A-33).
#[test]
fn commit_is_all_or_nothing() {
    let mut s = opened();
    let before = s.image();
    let (ext, t) = grow(3, 3, 2);
    let d = t.active.unwrap();
    let fails = |s: &mut MemHeaderStore, recs: Recs, t: StepTail, why: &str| {
        let e = step(s, recs, t, Durability::Buffered).expect_err(why);
        assert!(e.what().contains(why), "{why}: {e:?}");
        assert_eq!(s.image(), before, "{why}");
    };

    s.faults(|f| f.fail_commit(1));
    fails(&mut s, ext.clone(), t, "injected commit");
    s.faults(|f| f.fail_rows(2));
    fails(&mut s, ext.clone(), t, "injected rows");
    let mut dup = ext.clone();
    dup.push((record(5, ANCHOR_H + 1, hash(2), 3, RecordKind::Accepted), Some(BlockId(2))));
    fails(&mut s, dup, tail(Some(d), 6, 4), "UNIQUE");
    let stale = ActiveDiff {
        old_height: ANCHOR_H + 7,
        ..d
    };
    fails(
        &mut s,
        ext.clone(),
        StepTail {
            active: Some(stale),
            ..t
        },
        "not old_height",
    );
    // These fail after the walk rewrote `active`, which rolls back.
    fails(&mut s, ext.clone(), tail(Some(d), 5, 3), "not the active top");
    let short = ActiveDiff {
        new_height: ANCHOR_H + 3,
        ..d
    };
    fails(
        &mut s,
        ext.clone(),
        StepTail {
            active: Some(short),
            ..t
        },
        "at height",
    );
    let below = ActiveDiff {
        new_height: ANCHOR_H - 1,
        ..d
    };
    fails(
        &mut s,
        ext.clone(),
        StepTail {
            active: Some(below),
            ..t
        },
        "above new height",
    );
    let gap = ActiveDiff {
        from_height: ANCHOR_H + 2,
        ..d
    };
    fails(
        &mut s,
        ext.clone(),
        StepTail {
            active: Some(gap),
            ..t
        },
        "is not active",
    );

    step(&mut s, ext, t, Durability::Buffered).unwrap();
    assert_ne!(s.image(), before, "each fault fired once");

    // A branch 5, 6 from the anchor whose diff claims the fork at 11, where 3 is active.
    let before = s.image();
    let branch = line((BlockId(2), hash(2), ANCHOR_H), 5, 50, 2);
    let wrong_fork = ActiveDiff {
        from_height: ANCHOR_H + 2,
        new_tip: BlockId(6),
        new_height: ANCHOR_H + 2,
        old_height: ANCHOR_H + 2,
    };
    let err = step(&mut s, branch.clone(), tail(Some(wrong_fork), 7, 6), Durability::Buffered)
        .unwrap_err();
    assert!(err.what().contains("not the fork"), "{err:?}");
    assert_eq!(s.image(), before);
    let right = ActiveDiff {
        from_height: ANCHOR_H + 1,
        ..wrong_fork
    };
    step(&mut s, branch, tail(Some(right), 7, 6), Durability::Buffered).unwrap();
    assert_eq!(
        s.active_from(0).unwrap(),
        [(ANCHOR_H, BlockId(2)), (ANCHOR_H + 1, BlockId(5)), (ANCHOR_H + 2, BlockId(6))]
    );
    assert_eq!(ids(&s.leaves().unwrap()), [4, 6]);
}

/// The active walk refuses context records and records it cannot reach.
#[test]
fn active_walk_refusals() {
    let mut s = opened();
    let before = s.image();
    let into_pack = ActiveDiff {
        from_height: ANCHOR_H + 1,
        new_tip: BlockId(1),
        new_height: ANCHOR_H + 1,
        old_height: ANCHOR_H,
    };
    let unknown = ActiveDiff {
        new_tip: BlockId(9),
        ..into_pack
    };
    let no_fork = ActiveDiff {
        from_height: 0,
        new_tip: BlockId(2),
        new_height: ANCHOR_H,
        old_height: ANCHOR_H,
    };
    for (d, why) in [(into_pack, "kind 1"), (unknown, "no hdr row"), (no_fork, "fork height None")]
    {
        s.begin_step();
        let e = s.commit_step(&tail(Some(d), 3, d.new_tip.0), Durability::Buffered).unwrap_err();
        assert!(e.what().contains(why), "{why}: {e:?}");
        assert_eq!(s.image(), before);
    }
    // A step that disconnects to an ancestor: no walk, the new tip is the fork.
    let (recs, t) = grow(3, 3, 2);
    step(&mut s, recs, t, Durability::Buffered).unwrap();
    let back = ActiveDiff {
        from_height: ANCHOR_H + 2,
        new_tip: BlockId(3),
        new_height: ANCHOR_H + 1,
        old_height: ANCHOR_H + 2,
    };
    step(&mut s, Vec::new(), tail(Some(back), 5, 3), Durability::Buffered).unwrap();
    assert_eq!(s.active_from(0).unwrap(), [(ANCHOR_H, BlockId(2)), (ANCHOR_H + 1, BlockId(3))]);
}

/// The first active diff starts at the lowest accepted record and claims nothing was active.
#[test]
fn first_active_walk_refusals() {
    let anchor = (BlockId(2), hash(2), ANCHOR_H);
    let d = ActiveDiff {
        from_height: ANCHOR_H,
        new_tip: BlockId(4),
        new_height: ANCHOR_H + 2,
        old_height: 0,
    };
    let above = ActiveDiff {
        from_height: ANCHOR_H + 1,
        ..d
    };
    let claims_old = ActiveDiff {
        old_height: ANCHOR_H,
        ..d
    };
    for (d, why) in [(above, "stops above accepted BlockId(2)"), (claims_old, "nothing active")] {
        let mut s = MemHeaderStore::new();
        s.reset(meta()).unwrap();
        let before = s.image();
        let recs = pack().into_iter().map(|r| (r, None)).chain(line(anchor, 3, 3, 2)).collect();
        let e = step(&mut s, recs, tail(Some(d), 5, 4), Durability::Buffered).unwrap_err();
        assert!(!e.is_corrupt() && e.what().contains(why), "{why}: {e:?}");
        assert_eq!(s.image(), before);
    }
    let mut s = MemHeaderStore::new();
    s.reset(meta()).unwrap();
    let recs = pack().into_iter().map(|r| (r, None)).chain(line(anchor, 3, 3, 2)).collect();
    step(&mut s, recs, tail(Some(d), 5, 4), Durability::Buffered).unwrap();
    assert_eq!(s.active_from(0).unwrap().len(), 3);
}

/// A status row updates a stored record; one for an unknown id fails the step.
#[test]
fn status_rows() {
    let mut s = opened();
    commit_rows(&mut s, &[StepRow::Status(BlockId(1), Status(130))]).unwrap();
    assert_eq!(s.by_id(BlockId(1)).unwrap().map(|r| r.status), Some(Status(130)));
    let rows = [StepRow::Status(BlockId(0), Status(7)), StepRow::Status(BlockId(9), Status(1))];
    assert!(commit_rows(&mut s, &rows).is_err());
    assert_eq!(s.by_id(BlockId(0)).unwrap().map(|r| r.status), Some(Status(0)), "rolled back");
}

/// `LeafAdd` of an existing leaf, a context record or an unknown id fails; `LeafRemove` of a
/// non-leaf is a no-op.
#[test]
fn leaf_row_rules() {
    let mut s = opened();
    for bad in
        [StepRow::LeafAdd(BlockId(2)), StepRow::LeafAdd(BlockId(1)), StepRow::LeafAdd(BlockId(9))]
    {
        assert!(commit_rows(&mut s, std::slice::from_ref(&bad)).is_err(), "{bad:?}");
    }
    commit_rows(&mut s, &[StepRow::LeafRemove(BlockId(1)), StepRow::LeafRemove(BlockId(9))])
        .unwrap();
    assert_eq!(ids(&s.leaves().unwrap()), [2]);
}

/// The tail's ChainLock state and meta fields land; `establish` and `reset` are Durable.
#[test]
fn tail_meta_and_anchor_state() {
    let mut s = opened();
    let cl = ClPersisted {
        best: Some(ChainLockRef {
            height: ANCHOR_H,
            hash: hash(2),
        }),
        floor: Some(BlockId(2)),
        enabled_observed: true,
    };
    s.begin_step();
    s.commit_step(
        &StepTail {
            cl: Some(cl),
            ..tail(None, 9, 2)
        },
        Durability::Buffered,
    )
    .unwrap();
    s.establish().unwrap();
    let s = s.power_loss();
    let m = s.chain_meta().unwrap().unwrap();
    assert_eq!((m.cl, m.next_id, m.anchor_state), (cl, BlockId(9), AnchorState::Established));
    s.reset(meta()).unwrap();
    let s = s.power_loss();
    assert_eq!(s.chain_meta().unwrap(), Some(meta()));
    assert_eq!(s.max_id().unwrap(), None, "reset clears the header tables");
}

/// Power loss keeps the durable layer only; `sync_wal` covers the Buffered commits before it and
/// is a no-op when nothing is newer.
#[test]
fn power_loss_and_sync_wal() {
    let mut s = opened();
    s.faults(|f| f.fail_commit(1));
    s.sync_wal().unwrap();
    assert_eq!(s.chain_meta().unwrap().unwrap().sync_seq, 0, "nothing to cover");
    let (recs, t) = grow(3, 3, 1);
    let e = step(&mut s, recs.clone(), t, Durability::Buffered).unwrap_err();
    assert!(e.what().contains("injected commit"), "the no-op sync is no transaction: {e:?}");
    step(&mut s, recs.clone(), t, Durability::Buffered).unwrap();
    let mut s = s.power_loss();
    assert_eq!(s.by_id(BlockId(3)).unwrap(), None);
    assert_eq!(s.chain_meta().unwrap().unwrap().next_id, BlockId(3));
    assert_eq!(s.active_from(0).unwrap(), [(ANCHOR_H, BlockId(2))]);
    step(&mut s, recs, t, Durability::Buffered).unwrap();
    s.sync_wal().unwrap();
    assert_eq!((s.chain_meta().unwrap().unwrap().sync_seq, s.buffered_steps()), (1, 0));
    s.sync_wal().unwrap();
    let s = s.power_loss();
    assert_eq!(s.chain_meta().unwrap().unwrap().sync_seq, 1);
    assert!(s.by_id(BlockId(3)).unwrap().is_some());
}

/// A power loss can keep any prefix of the Buffered steps, which is then durable.
#[test]
fn power_loss_at_a_cut() {
    let mut s = opened();
    let (recs, _) = grow(3, 3, 3);
    for (i, r) in recs.into_iter().enumerate() {
        let tip = r.0.id;
        let d = ActiveDiff {
            from_height: r.0.height,
            new_tip: tip,
            new_height: r.0.height,
            old_height: r.0.height - 1,
        };
        step(&mut s, vec![r], tail(Some(d), tip.0 + 1, tip.0), Durability::Buffered).unwrap();
        assert_eq!(s.buffered_steps(), i + 1);
    }
    let s = s.power_loss_keeping(2);
    assert_eq!(s.chain_meta().unwrap().unwrap().tip, BlockId(4));
    assert_eq!(s.max_id().unwrap(), Some(BlockId(4)));
    assert_eq!(s.buffered_steps(), 0);
    let s = s.power_loss();
    assert_eq!(s.max_id().unwrap(), Some(BlockId(4)), "the kept steps are durable");
}

/// P5's witness: ids 3 and 4 committed Buffered are lost and reissued to other headers; the
/// durable rows never held them.
#[test]
fn ids_reissued_after_lost_suffix() {
    let mut s = opened();
    let (recs, t) = grow(3, 100, 2);
    step(&mut s, recs, t, Durability::Buffered).unwrap();
    let mut s = s.power_loss();
    let next = s.chain_meta().unwrap().unwrap().next_id;
    assert!(Some(next) > s.max_id().unwrap(), "open's n4 check holds");
    let (recs, t) = grow(next.0, 200, 2);
    step(&mut s, recs, t, Durability::Durable).unwrap();
    let s = s.power_loss();
    assert_eq!(s.by_id(BlockId(3)).unwrap().map(|r| r.hash), Some(hash(200)));
    assert_eq!(s.by_hash(&hash(100)).unwrap(), None);
}

#[test]
#[should_panic(expected = "a MemHeaderStore handle outlived power_loss")]
fn power_loss_invalidates_the_handle() {
    let s = opened();
    let _new = s.power_loss();
    let _ = s.by_id(BlockId(2));
}

#[test]
#[should_panic(expected = "a MemHeaderStore handle outlived power_loss")]
fn power_loss_invalidates_other_handles() {
    let s = opened();
    let other = s.clone();
    let _new = s.power_loss_keeping(0);
    let _ = other.clone();
}

/// A failed `rows` abandons the step: committing it panics.
#[test]
#[should_panic(expected = "commit_step without an open step")]
fn failed_rows_abandon_the_step() {
    let mut s = opened();
    s.faults(|f| f.fail_rows(1));
    assert!(commit_rows(&mut s, &[]).is_err());
    let _ = s.commit_step(&tail(None, 3, 2), Durability::Buffered);
}

/// The `n`-th read of a kind fails once; other kinds are unaffected.
#[test]
fn fail_nth_read() {
    let s = opened();
    s.faults(|f| f.fail_read(ReadKind::ById, 2));
    assert!(s.by_id(BlockId(2)).is_ok());
    assert!(s.by_hash(&hash(2)).is_ok());
    assert!(s.by_id(BlockId(2)).is_err_and(|e| !e.is_corrupt()), "a failed read is not corrupt");
    assert!(s.by_id(BlockId(2)).is_ok());
    for kind in [ReadKind::ChainMeta, ReadKind::MaxId, ReadKind::Leaves, ReadKind::WalkDown] {
        s.faults(|f| f.fail_read(kind, 1));
    }
    assert!(s.chain_meta().is_err() && s.max_id().is_err() && s.leaves().is_err());
    assert!(s.walk_down(BlockId(2), 2).is_err());
}

/// Corrupt reads: a bad skip, and a parent the store does not hold, where a walk then stops; a
/// record under another id, and one every read skips.
#[test]
fn corrupt_records() {
    let s = opened();
    s.faults(|f| {
        f.corrupt(BlockId(2), Corrupt::BadSkip);
        f.corrupt(BlockId(1), Corrupt::MissingParent);
    });
    assert_eq!(s.by_id(BlockId(2)).unwrap().unwrap().skip, Some(BlockId(2)));
    let r = s.leaves().unwrap().pop().unwrap();
    assert_eq!(r.skip, Some(BlockId(2)), "every read path");
    let r = s.by_hash(&hash(1)).unwrap().unwrap();
    assert_eq!(s.by_hash(&r.header.prev_hash).unwrap(), None);
    assert_eq!(ids(&s.walk_down(BlockId(2), 9).unwrap()), [2, 1]);

    let s = opened();
    s.faults(|f| {
        f.corrupt(BlockId(2), Corrupt::Id(BlockId(7)));
        f.corrupt(BlockId(1), Corrupt::Gone);
    });
    assert_eq!(s.by_id(BlockId(2)).unwrap().unwrap().id, BlockId(7));
    assert_eq!(s.by_id(BlockId(1)).unwrap(), None);
    assert_eq!(s.by_hash(&hash(1)).unwrap(), None);
    assert!(!ids(&s.from_height(0).unwrap()).contains(&1));
    assert_eq!(ids(&s.walk_down(BlockId(2), 9).unwrap()), [7]);
}

/// A stored row that does not decode is corruption, not a failed read (D68).
#[test]
fn an_undecodable_row_is_corrupt() {
    let s = opened();
    let mut row = s.image().hdr[&BlockId(2)];
    row.kind = 7;
    let e = decode(&row).unwrap_err();
    assert!(e.is_corrupt() && e.what().contains("hdr row 2"), "{e:?}");
}

// ---- P3 (`hdr_leaf`) and P5 against a model ----

#[derive(Clone, Debug)]
enum Op {
    Step {
        parents: Vec<Index>,
        marks: Vec<(Index, u32)>,
        tip: Option<Index>,
        durable: bool,
        fail: Option<Fail>,
    },
    SyncWal,
    /// Keeps a prefix of the Buffered steps, chosen by the index.
    PowerLoss(Index),
}

/// How a step fails: before it changes anything, after its rows, or after its tail rewrote
/// `active`; the last two roll back.
#[derive(Clone, Copy, Debug)]
enum Fail {
    Commit,
    Rows,
    /// A trailing status row for an unknown id.
    Row,
    /// A `meta.tip` that is not the active top, checked last.
    Tail,
}

fn op() -> impl Strategy<Value = Op> {
    let fail = prop_oneof![
        16 => Just(None),
        1 => Just(Some(Fail::Commit)),
        1 => Just(Some(Fail::Rows)),
        1 => Just(Some(Fail::Row)),
        1 => Just(Some(Fail::Tail)),
    ];
    let step = (
        prop::collection::vec(any::<Index>(), 0..5),
        prop::collection::vec((any::<Index>(), 0..256u32), 0..3),
        prop::option::of(any::<Index>()),
        prop::bool::weighted(0.2),
        fail,
    )
        .prop_map(|(parents, marks, tip, durable, fail)| Op::Step {
            parents,
            marks,
            tip,
            durable,
            fail,
        });
    prop_oneof![8 => step, 1 => Just(Op::SyncWal), 1 => any::<Index>().prop_map(Op::PowerLoss)]
}

#[derive(Clone, Copy, Debug)]
struct MRec {
    height: u32,
    parent: Option<BlockId>,
    accepted: bool,
    hash: BlockHash,
}

/// The committed state the store must hold.
#[derive(Clone, Debug)]
struct Model {
    recs: BTreeMap<BlockId, MRec>,
    active: BTreeMap<u32, BlockId>,
    next_id: u32,
    tip: BlockId,
}

impl Model {
    fn new() -> Model {
        let recs = pack()
            .iter()
            .map(|r| {
                let parent = r.id.0.checked_sub(1).map(BlockId);
                let accepted = r.kind == RecordKind::Accepted;
                (
                    r.id,
                    MRec {
                        height: r.height,
                        parent,
                        accepted,
                        hash: r.hash,
                    },
                )
            })
            .collect();
        let active = BTreeMap::from([(ANCHOR_H, BlockId(2))]);
        Model {
            recs,
            active,
            next_id: 3,
            tip: BlockId(2),
        }
    }

    fn accepted(&self) -> Vec<BlockId> {
        self.recs.iter().filter(|(_, r)| r.accepted).map(|(&id, _)| id).collect()
    }

    /// Leaves by brute force: accepted records with no accepted child.
    fn leaves(&self) -> Vec<BlockId> {
        let parents: BTreeSet<_> =
            self.recs.values().filter(|r| r.accepted).filter_map(|r| r.parent).collect();
        self.accepted().into_iter().filter(|id| !parents.contains(id)).collect()
    }

    /// The model after one step, its rows and its active diff. `n` numbers new hashes.
    fn plan(
        &self,
        n: &mut u64,
        parents: Vec<Index>,
        marks: Vec<(Index, u32)>,
        tip: Option<Index>,
    ) -> (Model, Vec<StepRow>, Option<ActiveDiff>) {
        let mut next = self.clone();
        let mut rows = Vec::new();
        for p in parents {
            let accepted = next.accepted();
            let parent = accepted[p.index(accepted.len())];
            let MRec {
                height,
                hash: prev_hash,
                ..
            } = next.recs[&parent];
            *n += 1;
            let r = record(next.next_id, height + 1, prev_hash, *n, RecordKind::Accepted);
            let m = MRec {
                height: r.height,
                parent: Some(parent),
                accepted: true,
                hash: r.hash,
            };
            next.recs.insert(r.id, m);
            next.next_id += 1;
            rows.extend(r.step_rows(Some(parent)));
        }
        let all: Vec<_> = next.recs.keys().copied().collect();
        for (i, st) in marks {
            rows.push(StepRow::Status(all[i.index(all.len())], Status(st)));
        }
        let active = tip.map(|i| {
            let accepted = next.accepted();
            let new_tip = accepted[i.index(accepted.len())];
            let (mut fork, mut path) = (new_tip, Vec::new());
            while next.active.get(&next.recs[&fork].height) != Some(&fork) {
                path.push(fork);
                fork = next.recs[&fork].parent.expect("accepted records descend from the anchor");
            }
            let d = ActiveDiff {
                from_height: next.recs[&fork].height + 1,
                new_tip,
                new_height: next.recs[&new_tip].height,
                old_height: *next.active.keys().last().unwrap(),
            };
            next.active.retain(|&h, _| h < d.from_height);
            next.active.extend(path.iter().map(|id| (next.recs[id].height, *id)));
            next.tip = new_tip;
            d
        });
        (next, rows, active)
    }
}

/// Leaves by brute force over the store's own rows: accepted rows no accepted row names as
/// prev (P3).
fn stored_leaves(s: &MemHeaderStore) -> Vec<BlockId> {
    let all = s.from_height(0).unwrap();
    let accepted: Vec<_> = all.iter().filter(|r| r.kind == RecordKind::Accepted).collect();
    let mut leaves: Vec<_> = accepted
        .iter()
        .filter(|r| !accepted.iter().any(|c| c.header.prev_hash == r.hash))
        .map(|r| r.id)
        .collect();
    leaves.sort();
    leaves
}

/// P5: a durable id always names the same header, in every later state; `add` records the
/// current rows as durable.
fn check_durable_ids(
    s: &MemHeaderStore,
    ever: &mut BTreeMap<BlockId, BlockHash>,
    add: bool,
) -> Result<(), TestCaseError> {
    for r in s.from_height(0).unwrap() {
        match ever.get(&r.id) {
            Some(h) => prop_assert_eq!(*h, r.hash, "durable id {:?} reissued", r.id),
            None if add => {
                ever.insert(r.id, r.hash);
            }
            None => {}
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// Random steps (branches, marks, reorgs), streamed in batches of 3, committed Buffered or
    /// Durable, some failing, with `sync_wal` and power losses at random cuts between them.
    /// After every operation the store equals the model: `hdr_leaf` = brute force over the rows
    /// (P3), `active` = the model's chain. No durable id is reissued, a reopened store's
    /// `next_id` is above every stored id, and every power loss reopens from a fresh handle (P5).
    #[test]
    fn store_model(ops in prop::collection::vec(op(), 1..40)) {
        let mut s = opened();
        let mut model = Model::new();
        // The durable model, then the model after each Buffered step since.
        let (mut durable, mut suffix) = (model.clone(), Vec::<Model>::new());
        let mut ever = BTreeMap::new();
        check_durable_ids(&s, &mut ever, true)?;
        let mut n = 1_000u64;
        for op in ops {
            match op {
                Op::Step { parents, marks, tip, durable: is_durable, fail } => {
                    let (next, mut rows, active) = model.plan(&mut n, parents, marks, tip);
                    let mut t = tail(active, next.next_id, next.tip.0);
                    match fail {
                        Some(Fail::Row) => rows.push(StepRow::Status(BlockId(u32::MAX), Status(0))),
                        Some(Fail::Tail) => t.meta.tip = BlockId(u32::MAX),
                        Some(Fail::Rows) => s.faults(|f| f.fail_rows(1)),
                        Some(Fail::Commit) | None => {}
                    }
                    let before = s.image();
                    s.begin_step();
                    // At least one batch, so that an injected `rows` failure fires.
                    let mut batches: Vec<&[StepRow]> = rows.chunks(3).collect();
                    if batches.is_empty() {
                        batches.push(&[]);
                    }
                    let streamed = batches.into_iter().try_for_each(|batch| s.rows(batch));
                    if matches!(fail, Some(Fail::Commit)) {
                        s.faults(|f| f.fail_commit(1));
                    }
                    let d = if is_durable { Durability::Durable } else { Durability::Buffered };
                    let committed = streamed.and_then(|()| s.commit_step(&t, d));
                    prop_assert_eq!(committed.is_ok(), fail.is_none());
                    match committed {
                        Ok(()) => {
                            model = next;
                            if is_durable {
                                (durable, suffix) = (model.clone(), Vec::new());
                            } else {
                                suffix.push(model.clone());
                            }
                        }
                        Err(_) => prop_assert_eq!(s.image(), before),
                    }
                    check_durable_ids(&s, &mut ever, is_durable && fail.is_none())?;
                }
                Op::SyncWal => {
                    s.sync_wal().unwrap();
                    (durable, suffix) = (model.clone(), Vec::new());
                    check_durable_ids(&s, &mut ever, true)?;
                }
                Op::PowerLoss(i) => {
                    prop_assert_eq!(s.buffered_steps(), suffix.len());
                    let keep = i.index(suffix.len() + 1);
                    s = s.power_loss_keeping(keep);
                    model = if keep == 0 { durable.clone() } else { suffix[keep - 1].clone() };
                    (durable, suffix) = (model.clone(), Vec::new());
                    let meta = s.chain_meta().unwrap().unwrap();
                    prop_assert!(Some(meta.next_id) > s.max_id().unwrap());
                    prop_assert_eq!((meta.next_id.0, meta.tip), (model.next_id, model.tip));
                    check_durable_ids(&s, &mut ever, true)?;
                }
            }
            let leaves: Vec<_> = s.leaves().unwrap().iter().map(|r| r.id).collect();
            prop_assert_eq!(&leaves, &stored_leaves(&s));
            prop_assert_eq!(&leaves, &model.leaves());
            let paged: Vec<_> = paged_leaves(&s, 2).iter().map(|&(id, _)| id).collect();
            prop_assert_eq!(&paged, &leaves);
            let active: Vec<_> = model.active.iter().map(|(&h, &id)| (h, id)).collect();
            prop_assert_eq!(s.active_from(0).unwrap(), active);
            prop_assert_eq!(s.max_id().unwrap(), model.recs.keys().last().copied());
            let hashes: BTreeMap<_, _> =
                s.from_height(0).unwrap().iter().map(|r| (r.id, r.hash)).collect();
            let want: BTreeMap<_, _> = model.recs.iter().map(|(&id, r)| (id, r.hash)).collect();
            prop_assert_eq!(hashes, want);
        }
    }
}

/// `HeaderStore::walk_down`'s default (SB5-A18: by id, then each parent by hash) answers as
/// `MemHeaderStore`'s own walk does.
#[test]
fn the_default_walk_down_matches() {
    struct ByDefault(MemHeaderStore);

    impl HeaderStore for ByDefault {
        fn by_hash(&self, h: &BlockHash) -> Result<Option<HeaderRecord>, StoreError> {
            self.0.by_hash(h)
        }

        fn by_id(&self, id: BlockId) -> Result<Option<HeaderRecord>, StoreError> {
            self.0.by_id(id)
        }

        fn active_at(&self, height: u32) -> Result<Option<BlockId>, StoreError> {
            self.0.active_at(height)
        }

        fn from_height(&self, h: u32) -> Result<Vec<HeaderRecord>, StoreError> {
            self.0.from_height(h)
        }

        fn leaves_from(
            &self,
            from: BlockId,
            out: &mut [(BlockId, Work)],
        ) -> Result<usize, StoreError> {
            self.0.leaves_from(from, out)
        }

        fn children_from(
            &self,
            prev: &BlockHash,
            from: BlockId,
            out: &mut [BlockId],
        ) -> Result<usize, StoreError> {
            self.0.children_from(prev, from, out)
        }

        fn chain_meta(&self) -> Result<Option<ChainMeta>, StoreError> {
            self.0.chain_meta()
        }

        fn active_from(&self, lo: u32) -> Result<Vec<(u32, BlockId)>, StoreError> {
            self.0.active_from(lo)
        }

        fn max_id(&self) -> Result<Option<BlockId>, StoreError> {
            self.0.max_id()
        }
    }

    let mut s = opened();
    let (recs, t) = grow(3, 3, 4);
    step(&mut s, recs, t, Durability::Buffered).unwrap();
    let d = ByDefault(s.clone());
    for (from, n) in [(6, 0), (6, 1), (6, 3), (6, 100), (2, 5), (99, 3)] {
        let from = BlockId(from);
        assert_eq!(d.walk_down(from, n).unwrap(), s.walk_down(from, n).unwrap(), "{from:?} {n}");
    }
}
