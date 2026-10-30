//! The store's contract, on dash-kv's `MemoryEngine` and on SQLite: `MemHeaderStore`'s checks
//! (SB5 §3.7) for the header step, D44's inclusive `from_height`, the legacy writes, `sync_wal`
//! and the blind recovery.

use dash_chain::{
    ActiveDiff, AnchorState, BlockId, ChainMeta, ClPersisted, Durability, HeaderRecord, HeaderRow,
    HeaderStepSink, HeaderStore, MetaDelta, RecordKind, Status, StepRow, StepTail, Work,
};
use dash_kv::{Commit, KvBatch, KvEngine, MemoryEngine};
use dash_primitives::BlockHash;

use std::sync::atomic::{AtomicBool, Ordering};

use super::hdr::io::OsIo;
use super::legacy::RawHeader;
use super::*;

/// A header whose `prev` is `prev`, distinct per `(height, branch)`.
fn header(prev: [u8; 32], height: u32, branch: u8) -> [u8; 80] {
    let mut h = [0; 80];
    h[0] = 1;
    h[4..36].copy_from_slice(&prev);
    h[36] = branch;
    h[68..72].copy_from_slice(&(1_000 + height).to_le_bytes());
    h[72..76].copy_from_slice(&0x207f_ffffu32.to_le_bytes());
    h[76..80].copy_from_slice(&height.to_le_bytes());
    h
}

/// A trusted hash, distinct per `(height, branch)`; the store never recomputes it (SB5-A2).
fn hash(height: u32, branch: u8) -> [u8; 32] {
    let mut h = [0; 32];
    h[..4].copy_from_slice(&height.to_be_bytes());
    h[4] = branch;
    h[31] = 0xee;
    h
}

fn record(id: u32, height: u32, branch: u8, prev: [u8; 32], kind: RecordKind) -> HeaderRecord {
    let row = HeaderRow {
        hash: hash(height, branch),
        id,
        height,
        header: header(prev, height, branch),
        skip: None,
        status: Status::VALID_TREE,
        chain_work: [0; 32],
        time_max: 1_000 + height,
        kind: match kind {
            RecordKind::Accepted => 0,
            RecordKind::Context => 1,
        },
    };
    HeaderRecord::from_row(&row).expect("decodes")
}

const NO_CL: ClPersisted = ClPersisted {
    best: None,
    floor: None,
    enabled_observed: false,
};

fn meta(tip: u32, next_id: u32) -> MetaDelta {
    MetaDelta {
        next_id: BlockId(next_id),
        tip: BlockId(tip),
        best_header: BlockId(tip),
    }
}

fn candidate() -> ChainMeta {
    ChainMeta {
        schema: SPV_SCHEMA.version,
        anchor: (BlockHash::from(hash(0, 0)), 0),
        anchor_state: AnchorState::Candidate,
        next_id: BlockId(0),
        tip: BlockId(0),
        best_header: BlockId(0),
        sync_seq: 0,
        cl: NO_CL,
    }
}

/// The step rows of a chain of `n` records on `branch` from `parent` (at `from - 1`), ids from
/// `first_id`.
fn chain(
    first_id: u32,
    from: u32,
    n: u32,
    branch: u8,
    parent: Option<(BlockId, [u8; 32])>,
) -> Vec<StepRow> {
    let mut rows = Vec::new();
    let mut prev = parent;
    for i in 0..n {
        let h = from + i;
        let rec =
            record(first_id + i, h, branch, prev.map_or([0; 32], |p| p.1), RecordKind::Accepted);
        rows.extend(rec.clone().step_rows(prev.map(|p| p.0)));
        prev = Some((rec.id, *rec.hash.as_bytes()));
    }
    rows
}

fn step<E: KvEngine + 'static>(
    s: &Store<E>,
    rows: &[StepRow],
    tail: StepTail,
) -> std::result::Result<(), String> {
    let mut st = s.begin_step().unwrap();
    st.rows(rows).map_err(|e| e.to_string())?;
    st.commit(&tail, Durability::Buffered).map_err(|e| e.to_string())
}

fn tail(active: Option<ActiveDiff>, m: MetaDelta) -> StepTail {
    StepTail {
        active,
        cl: None,
        meta: m,
    }
}

fn diff(from_height: u32, new_tip: u32, new_height: u32, old_height: u32) -> Option<ActiveDiff> {
    Some(ActiveDiff {
        from_height,
        new_tip: BlockId(new_tip),
        new_height,
        old_height,
    })
}

fn active<E: KvEngine + 'static>(s: &Store<E>) -> Vec<(u32, u32)> {
    s.active_from(0).unwrap().into_iter().map(|(h, id)| (h, id.0)).collect()
}

/// A0..A5 (ids 0..=5) active.
fn six<E: KvEngine + 'static>(s: Store<E>) -> Store<E> {
    s.reset(candidate()).unwrap();
    step(&s, &chain(0, 0, 6, 0, None), tail(diff(0, 5, 5, 0), meta(5, 6))).unwrap();
    s
}

/// Records a test segment file holds: small, so the tests cross segment boundaries.
pub(super) const TEST_PER: u32 = 8;

/// A store on the OS's files in `dir` with dash-kv's memory engine (never reopened).
pub(super) fn mem_store(dir: &tempfile::TempDir) -> Store<MemoryEngine> {
    Store::open(Layout {
        storage: dir.path().to_path_buf(),
        opener: Box::new(lazy::MemOpener),
        io: std::sync::Arc::new(OsIo::default()),
        per: TEST_PER,
    })
    .unwrap()
}

/// A store on the OS's files and SQLite in `dir` (reopened by calling it again).
pub(super) fn sqlite_store(dir: &tempfile::TempDir) -> Store<dash_kv_sqlite::SqliteEngine> {
    Store::open(Layout {
        storage: dir.path().to_path_buf(),
        opener: Box::new(lazy::SqliteOpener::new(&dir.path().join("spv"))),
        io: std::sync::Arc::new(OsIo::default()),
        per: TEST_PER,
    })
    .unwrap()
}

macro_rules! on_both {
    ($name:ident, |$mk:ident| $body:block) => {
        mod $name {
            use super::*;
            fn run<E: KvEngine + 'static>($mk: &dyn Fn() -> Store<E>) $body
            #[test]
            fn memory() {
                let dir = tempfile::tempdir().unwrap();
                run(&|| super::mem_store(&dir));
            }
            #[test]
            fn sqlite() {
                let dir = tempfile::tempdir().unwrap();
                run(&|| super::sqlite_store(&dir));
            }
        }
    };
}

on_both!(first_step_writes_every_table, |mk| {
    let s = six(mk());
    assert_eq!(active(&s), (0..6).map(|h| (h, h)).collect::<Vec<_>>());
    let m = s.chain_meta().unwrap().unwrap();
    assert_eq!(
        (m.tip, m.next_id, m.anchor_state),
        (BlockId(5), BlockId(6), AnchorState::Candidate)
    );
    assert_eq!(s.max_id().unwrap(), Some(BlockId(5)));
    let r = s.by_hash(&BlockHash::from(hash(3, 0))).unwrap().unwrap();
    assert_eq!((r.id, r.height), (BlockId(3), 3));
    assert_eq!(s.by_id(BlockId(3)).unwrap(), Some(r.clone()));
    assert_eq!(s.active_at(3).unwrap(), Some(BlockId(3)));
    assert_eq!(s.active_at(6).unwrap(), None);
    let mut leaves = [(BlockId(0), Work::from_u64(0)); 4];
    assert_eq!(s.leaves_from(BlockId(0), &mut leaves).unwrap(), 1, "only the tip is a leaf");
    assert_eq!(leaves[0].0, BlockId(5));
    let mut kids = [BlockId(0); 4];
    assert_eq!(s.children_from(&BlockHash::from(hash(2, 0)), BlockId(0), &mut kids).unwrap(), 1);
    assert_eq!(kids[0], BlockId(3));
    let from: Vec<u32> = s.from_height(4).unwrap().iter().map(|r| r.id.0).collect();
    assert_eq!(from, [4, 5]);
    let down: Vec<u32> = s.walk_down(BlockId(5), 3).unwrap().iter().map(|r| r.id.0).collect();
    assert_eq!(down, [5, 4, 3]);
    assert_eq!(s.walk_down(BlockId(1), 9).unwrap().len(), 2, "stops below the lowest record");
});

on_both!(d44_from_height_is_the_lowest_rewritten_height, |mk| {
    let s = six(mk());
    // B4..B6 (ids 6..=8) fork from A3: the fork is at 3, so the lowest rewritten height is 4.
    let b = chain(6, 4, 3, 1, Some((BlockId(3), hash(3, 0))));
    // An exclusive reading (from = 5, fork 4) walks B6, B5 and reaches B4, not the fork A4.
    let err = step(&s, &b, tail(diff(5, 8, 6, 5), meta(8, 9))).unwrap_err();
    assert!(err.contains("not the fork"), "{err}");
    assert_eq!(active(&s).len(), 6, "a refused step writes nothing");
    assert_eq!(s.by_hash(&BlockHash::from(hash(4, 1))).unwrap(), None);
    step(&s, &b, tail(diff(4, 8, 6, 5), meta(8, 9))).unwrap();
    assert_eq!(active(&s), [(0, 0), (1, 1), (2, 2), (3, 3), (4, 6), (5, 7), (6, 8)]);
    // Every branch stays: A4 and A5 are records and A5 is still a leaf.
    assert_eq!(s.by_id(BlockId(5)).unwrap().unwrap().height, 5);
    let mut leaves = [(BlockId(0), Work::from_u64(0)); 4];
    let n = s.leaves_from(BlockId(0), &mut leaves).unwrap();
    assert_eq!(leaves[..n].iter().map(|l| l.0 .0).collect::<Vec<_>>(), [5, 8]);
    // Back to A5 with no rows: a shorter tip deletes the heights above it.
    step(&s, &[], tail(diff(4, 5, 5, 6), meta(5, 9))).unwrap();
    assert_eq!(active(&s), (0..6).map(|h| (h, h)).collect::<Vec<_>>());
});

on_both!(active_diff_checks, |mk| {
    let s = six(mk());
    let next = chain(6, 6, 1, 0, Some((BlockId(5), hash(5, 0))));
    for (d, why) in [
        (diff(6, 6, 6, 4), "not old_height"),
        (diff(5, 3, 3, 5), "above new height"),
        (diff(6, 5, 6, 5), "at height 6"),
    ] {
        let err = step(&s, &next, tail(d, meta(6, 7))).unwrap_err();
        assert!(err.contains(why), "{why}: {err}");
    }
    let err = step(&s, &next, tail(diff(6, 6, 6, 5), meta(5, 7))).unwrap_err();
    assert!(err.contains("not the active top"), "{err}");
    step(&s, &next, tail(diff(6, 6, 6, 5), meta(6, 7))).unwrap();
    assert_eq!(
        s.view().active,
        Bounds {
            lo: Some(0),
            hi: Some(6)
        }
    );
});

on_both!(row_checks_abandon_the_step, |mk| {
    let s = six(mk());
    let dup = record(6, 3, 0, hash(2, 0), RecordKind::Accepted);
    let ctx = record(7, 9, 9, [0; 32], RecordKind::Context);
    for (rows, why) in [
        (vec![StepRow::Record(dup)], "UNIQUE"),
        (vec![StepRow::Record(record(3, 9, 7, [0; 32], RecordKind::Accepted))], "UNIQUE"),
        (vec![StepRow::Status(BlockId(99), Status(Status::VALID_TREE))], "unknown"),
        (vec![StepRow::Record(ctx), StepRow::LeafAdd(BlockId(7))], "context"),
        (vec![StepRow::LeafAdd(BlockId(5))], "hdr_leaf"),
    ] {
        let mut st = s.begin_step().unwrap();
        let err = st.rows(&rows).unwrap_err().to_string();
        assert!(err.contains(why), "{why}: {err}");
        assert!(st.rows(&[]).is_err(), "a failed step refuses more rows");
        assert!(st.commit(&tail(None, meta(5, 9)), Durability::Buffered).is_err());
    }
    // A removal of a non-leaf is a no-op, and a status change commits.
    let conflict = Status(Status::VALID_TREE | Status::CONFLICT_CHAINLOCK);
    let rows = [StepRow::LeafRemove(BlockId(2)), StepRow::Status(BlockId(4), conflict)];
    step(&s, &rows, tail(None, meta(5, 6))).unwrap();
    assert_eq!(s.by_id(BlockId(4)).unwrap().unwrap().status, conflict);
    assert_eq!(s.max_id().unwrap(), Some(BlockId(5)));
    // dash-chain reads the committed max inside `write_header_step`, while its sink (a step)
    // holds the writer lease: the read does not wait for the lease.
    let mut st = s.begin_step().unwrap();
    st.rows(&[StepRow::Record(record(6, 6, 0, hash(5, 0), RecordKind::Accepted))]).unwrap();
    assert_eq!(s.max_id().unwrap(), Some(BlockId(5)), "the committed max, not the step's");
    drop(st);
});

on_both!(chainlock_rows_and_reset, |mk| {
    let s = six(mk());
    let cl = ClPersisted {
        best: Some(dash_chain::ChainLockRef {
            height: 4,
            hash: BlockHash::from(hash(4, 0)),
        }),
        floor: Some(BlockId(4)),
        enabled_observed: true,
    };
    let t = StepTail {
        active: None,
        cl: Some(cl),
        meta: meta(5, 6),
    };
    step(&s, &[], t).unwrap();
    assert_eq!(s.chain_meta().unwrap().unwrap().cl, cl);
    let unknown = ClPersisted {
        floor: Some(BlockId(77)),
        ..cl
    };
    let err = step(
        &s,
        &[],
        StepTail {
            cl: Some(unknown),
            ..t
        },
    )
    .unwrap_err();
    assert!(err.contains("floor"), "{err}");
    // A Candidate fallback clears the header tables.
    s.reset(candidate()).unwrap();
    assert_eq!(active(&s), []);
    assert_eq!(s.max_id().unwrap(), None);
    assert_eq!(s.chain_meta().unwrap().unwrap().cl, NO_CL);
    s.establish().unwrap();
    assert!(s.reset(candidate()).is_err(), "nothing clears an Established store");
});

on_both!(sync_wal_counts_only_newer_buffered_commits, |mk| {
    let s = six(mk());
    let seq = |s: &Store<E>| s.chain_meta().unwrap().unwrap().sync_seq;
    s.sync_wal().unwrap();
    assert_eq!(seq(&s), 1);
    s.sync_wal().unwrap();
    assert_eq!(seq(&s), 1, "nothing Buffered since");
    s.put_metadata("k", b"v").unwrap();
    s.sync_wal().unwrap();
    assert_eq!(seq(&s), 2);
});

on_both!(legacy_headers_are_full_records, |mk| {
    let s = mk();
    let mut prev = [0; 32];
    let headers: Vec<RawHeader> = (100..110)
        .map(|h| {
            let raw = (hash(h, 0), header(prev, h, 0));
            prev = raw.0;
            raw
        })
        .collect();
    s.put_headers(100, &headers[..4]).unwrap();
    s.put_headers(104, &headers[4..]).unwrap();
    // In the files at once, `meta` derived (the start is the anchor, Established).
    assert_eq!(s.headers(103..106).unwrap(), headers[3..6]);
    assert_eq!(s.active_height_of(&hash(105, 0)).unwrap(), Some(105));
    let m = s.chain_meta().unwrap().unwrap();
    assert_eq!(
        (m.anchor.1, m.anchor_state, m.tip, m.next_id),
        (100, AnchorState::Established, BlockId(9), BlockId(10))
    );
    let proof = dash_consensus::pow::block_proof(dash_num::CompactTarget::new(0x207f_ffff));
    let r = s.by_id(BlockId(9)).unwrap().unwrap();
    assert_eq!(r.chain_work, Work::from_u64(10) * proof);
    assert_eq!(r.time_max, 1_109);
    // GetSkipHeight(109) = 97 (odd: clear the two lowest set bits of 108, add one), below the
    // stored heights; GetSkipHeight(108) = 104 (even: clear the lowest set bit).
    assert_eq!(r.skip, None);
    assert_eq!(s.by_id(BlockId(8)).unwrap().unwrap().skip, Some(BlockId(4)));
    let mut leaves = [(BlockId(0), Work::from_u64(0)); 4];
    assert_eq!(s.leaves_from(BlockId(0), &mut leaves).unwrap(), 1);
    assert_eq!(s.headers(103..106).unwrap(), headers[3..6]);
    assert!(s.headers(99..101).is_err() && s.headers(109..111).is_err());
    assert_eq!(s.active_height_of(&hash(105, 0)).unwrap(), Some(105));
    // A gap is refused; restoring a stored header reuses its record.
    assert!(s.put_headers(111, &headers[..1]).is_err());
    assert!(s.put_headers(101, &headers[..1]).is_err(), "stored at another height");
    s.truncate_headers(104).unwrap();
    assert_eq!(
        s.view().active,
        Bounds {
            lo: Some(100),
            hi: Some(104)
        }
    );
    assert_eq!(s.active_height_of(&hash(105, 0)).unwrap(), None, "off the active chain");
    assert!(s.by_hash(&BlockHash::from(hash(105, 0))).unwrap().is_some(), "the record stays");
    assert!(s.truncate_headers(99).is_err());
    s.put_headers(105, &headers[5..7]).unwrap();
    assert_eq!(s.max_id().unwrap(), Some(BlockId(9)), "no new record");
    assert_eq!(s.active_height_of(&hash(106, 0)).unwrap(), Some(106));
});

on_both!(subscribers_carry_their_locators, |mk| {
    let s = mk();
    let mut prev = [0; 32];
    let headers: Vec<RawHeader> = (0..10)
        .map(|h| {
            let raw = (hash(h, 0), header(prev, h, 0));
            prev = raw.0;
            raw
        })
        .collect();
    s.put_headers(0, &headers).unwrap();
    let f = Subscriber::FilterHeaders;
    s.put_sub(f, 0, &[&[1; 32], &[2; 32], &[3; 32]]).unwrap();
    assert_eq!(s.locator(f).unwrap(), Some((hash(2, 0), 2)));
    assert_eq!(s.sub_range(f, 1..3).unwrap(), [vec![2; 32], vec![3; 32]]);
    s.put_sub(Subscriber::Blocks, 7, &[b"seven"]).unwrap();
    assert!(s.sub_range(Subscriber::Blocks, 6..8).is_err(), "below the lowest block");
    s.truncate_sub(f, 1).unwrap();
    assert_eq!(s.locator(f).unwrap(), Some((hash(1, 0), 1)));
    assert!(s.truncate_sub(Subscriber::Blocks, 3).is_err());
    assert_eq!(s.recover().unwrap(), [Recovered::Kept; 3]);
});

// The legacy reorg (`truncate_headers`, then the branch's `put_headers`) leaves the old
// branch's records, which recovery walks: a subscriber still naming the old branch rewinds to
// the fork, before the branch is stored and after.
on_both!(recovery_after_a_legacy_truncation_rewinds_to_the_fork, |mk| {
    let s = mk();
    let chain = |tag: u8, from: u32, to: u32, mut prev: [u8; 32]| -> Vec<RawHeader> {
        (from..to)
            .map(|h| {
                let raw = (hash(h, tag), header(prev, h, tag));
                prev = raw.0;
                raw
            })
            .collect()
    };
    let a = chain(0, 0, 8, [0; 32]);
    s.put_headers(0, &a).unwrap();
    let f = Subscriber::FilterHeaders;
    s.put_sub(f, 0, &[&[1u8; 32][..]; 8]).unwrap();
    assert_eq!(s.locator(f).unwrap(), Some((hash(7, 0), 7)));
    // SB14 D7: the lowering's first batch rewinds the subscribers above the fork itself.
    s.truncate_headers(4).unwrap();
    assert_eq!(s.locator(f).unwrap(), Some((hash(4, 0), 4)));
    assert_eq!(s.recover().unwrap(), [Recovered::Kept; 3]);
    // Rows above the header tip get the locator of the tip below them, A4, and recovery
    // drops them: no header checks them (D35).
    s.put_sub(f, 5, &[&[1u8; 32][..]; 3]).unwrap();
    assert_eq!(s.locator(f).unwrap(), Some((hash(4, 0), 4)));
    assert_eq!(s.recover().unwrap(), [Recovered::Rewound(4), Recovered::Kept, Recovered::Kept]);
    assert_eq!(s.view().sub(f).hi, Some(4));
    // Back on A to 7, the filter headers follow; then the reorg to B5..B7 from A4.
    s.put_headers(5, &a[5..]).unwrap();
    s.put_sub(f, 5, &[&[1u8; 32][..]; 3]).unwrap();
    assert_eq!(s.locator(f).unwrap(), Some((hash(7, 0), 7)));
    s.truncate_headers(4).unwrap();
    assert_eq!(s.view().sub(f).hi, Some(4), "rewound by the lowering");
    s.put_headers(5, &chain(1, 5, 8, a[4].0)).unwrap();
    assert_eq!(s.recover().unwrap(), [Recovered::Kept; 3]);
    assert_eq!(s.view().sub(f).hi, Some(4));
});

#[test]
fn recovery_rewinds_a_stale_branch_and_resets_an_unknown_locator() {
    let dir = tempfile::tempdir().unwrap();
    let s = six(sqlite_store(&dir));
    let f = Subscriber::FilterHeaders;
    let c = Subscriber::Filters;
    s.establish().unwrap();
    s.put_sub(f, 0, &[&[0u8; 32][..]; 6]).unwrap();
    s.put_sub(c, 0, &[&b"f"[..]; 6]).unwrap();
    // Reorg to B4..B6 from A3; the filters still name A5.
    step(
        &s,
        &chain(6, 4, 3, 1, Some((BlockId(3), hash(3, 0)))),
        tail(diff(4, 8, 6, 5), meta(8, 9)),
    )
    .unwrap();
    // And the filter headers name a hash the store never saw (a lost Buffered suffix).
    {
        let mut w = s.lease();
        let mut b = s.db().unwrap().batch();
        b.put(LOCATORS, f.locator_key(), &encode_locator(&[0x99; 32], 5));
        s.commit(&mut w, b, Commit::Buffered).unwrap();
    }
    // The reorg's lowering rewound the filters to A3 itself (D7); recovery resets the unknown
    // locator.
    assert_eq!(s.recover().unwrap(), [Recovered::Reset, Recovered::Kept, Recovered::Kept]);
    assert_eq!(s.view().sub(f), Bounds::default());
    assert_eq!(s.locator(f).unwrap(), None);
    assert_eq!(
        s.view().sub(c),
        Bounds {
            lo: Some(0),
            hi: Some(3)
        }
    );
    assert_eq!(s.locator(c).unwrap(), Some((hash(3, 0), 3)));
    // Reopened, the view is read back from the engine.
    s.sync_wal().unwrap();
    drop(s);
    let again = sqlite_store(&dir);
    assert_eq!(
        again.view().sub(c),
        Bounds {
            lo: Some(0),
            hi: Some(3)
        }
    );
    assert_eq!(again.max_id().unwrap(), Some(BlockId(8)));
}

#[test]
fn sqlite_reopen_keeps_the_store() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = six(sqlite_store(&dir));
        s.put_metadata("last_target_height", b"7").unwrap();
        s.sync_wal().unwrap();
    }
    let s = sqlite_store(&dir);
    assert_eq!(active(&s).len(), 6);
    assert_eq!(s.metadata("last_target_height").unwrap().as_deref(), Some(&b"7"[..]));
    s.clear_all().unwrap();
    assert_eq!(active(&s), []);
    assert_eq!(s.chain_meta().unwrap(), None);
    assert_eq!(s.metadata("last_target_height").unwrap(), None);
}

/// `n` linked legacy headers on `branch` from `from`, the first one's `prev` `prev`.
fn linked(from: u32, n: u32, branch: u8, mut prev: [u8; 32]) -> Vec<RawHeader> {
    (from..from + n)
        .map(|h| {
            let raw = (hash(h, branch), header(prev, h, branch));
            prev = raw.0;
            raw
        })
        .collect()
}

/// The id and height `by_hash` gives, and that `by_id` maps the id back to the hash.
fn id_of<E: KvEngine + 'static>(s: &Store<E>, h: &[u8; 32]) -> (u32, u32) {
    let r = s.by_hash(&BlockHash::from(*h)).unwrap().expect("stored");
    assert_eq!(s.by_id(r.id).unwrap().map(|b| b.hash), Some(BlockHash::from(*h)), "by_id");
    (r.id.0, r.height)
}

/// Every hash in `stored` is found, at a distinct id that `by_id` maps back; the active chain is
/// `active`.
fn check_model<E: KvEngine + 'static>(s: &Store<E>, active: &[RawHeader], stored: &[[u8; 32]]) {
    let hi = active.len() as u32;
    assert_eq!(s.headers(0..hi).unwrap(), active, "the active chain");
    assert_eq!(s.view().active.hi, hi.checked_sub(1));
    let mut ids = BTreeSet::new();
    for h in stored {
        let (id, _) = id_of(s, h);
        assert!(ids.insert(id), "id {id} issued twice");
    }
    for (at, (h, _)) in active.iter().enumerate() {
        assert_eq!(id_of(s, h).1, at as u32);
    }
}

// SB14 §3, the r1 M3 test (r2 m7): a linked run whose first header duplicates an off-chain
// child of the tip is not appended as a new record. The `hdr_prev(tip)` probe sends it to the
// slow path, which re-activates the stored records with their ids.
on_both!(a_linked_run_onto_an_off_chain_child_reactivates_it, |mk| {
    let s = mk();
    let a = linked(0, 6, 0, [0; 32]);
    s.put_headers(0, &a).unwrap();
    s.truncate_headers(3).unwrap();
    let ids: Vec<_> = a.iter().map(|(h, _)| id_of(&s, h).0).collect();
    assert_eq!(s.view().active.hi, Some(3));
    let more = linked(6, 2, 0, a[5].0);
    s.put_headers(4, &[&a[4..], &more[..]].concat()).unwrap();
    for (i, (h, _)) in a.iter().enumerate() {
        assert_eq!(id_of(&s, h), (ids[i], i as u32), "A{i} keeps its id");
    }
    let all: Vec<_> = a.iter().chain(&more).map(|(h, _)| *h).collect();
    check_model(&s, &[&a[..], &more[..]].concat(), &all);
});

/// SB14 §3, the base run (r3 N6): a header-only store has no database and takes its ids from
/// the base run, across a reopen; its first lowering creates SQLite (D4) and materializes the
/// base run, so the reopened ids are the same.
#[test]
fn a_header_only_store_takes_its_ids_from_the_base_run() {
    let dir = tempfile::tempdir().unwrap();
    let a = linked(0, 6, 0, [0; 32]);
    {
        let s = sqlite_store(&dir);
        s.put_headers(0, &a).unwrap();
        s.raise().unwrap();
    }
    assert!(!dir.path().join("spv").exists(), "no database (D4)");
    let s = sqlite_store(&dir);
    assert!(!s.has_db());
    for (i, (h, _)) in a.iter().enumerate() {
        assert_eq!(id_of(&s, h), (i as u32, i as u32), "the base run");
    }
    let b = linked(3, 3, 1, a[2].0);
    s.put_headers(3, &b).unwrap();
    assert!(s.has_db(), "the first lowering created the database");
    s.sync_wal().unwrap();
    drop(s);
    let s = sqlite_store(&dir);
    for (i, (h, _)) in a.iter().enumerate() {
        assert_eq!(id_of(&s, h).0, i as u32, "A{i} keeps its base-run id");
    }
    assert_eq!(id_of(&s, &b[0].0), (6, 3), "the first new id is past the base run");
    let all: Vec<_> = a.iter().chain(&b).map(|(h, _)| *h).collect();
    check_model(&s, &[&a[..3], &b[..]].concat(), &all);
}

/// D5 (0): the first commit that writes `meta` on a header-only store (here `establish`) writes
/// the base run's row too, so the reopened store has its ids.
#[test]
fn the_first_meta_write_carries_the_base_run() {
    let dir = tempfile::tempdir().unwrap();
    let a = linked(0, 6, 0, [0; 32]);
    {
        let s = sqlite_store(&dir);
        s.put_headers(0, &a).unwrap();
        s.raise().unwrap();
        s.establish().unwrap();
        s.sync_wal().unwrap();
    }
    let s = sqlite_store(&dir);
    for (i, (h, _)) in a.iter().enumerate() {
        assert_eq!(id_of(&s, h), (i as u32, i as u32));
    }
    let more = linked(6, 2, 0, a[5].0);
    s.put_headers(6, &more).unwrap();
    assert_eq!(id_of(&s, &more[1].0), (7, 7), "the base run stays open");
}

/// A SQLite slot that does not decode is corruption (a SQLite row is never torn): open refuses
/// with `Layout` rather than trusting an older file slot.
#[test]
fn an_undecodable_sqlite_slot_refuses_the_open() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = six(sqlite_store(&dir));
        let mut w = s.lease();
        let mut b = s.db().unwrap().batch();
        b.put(META, meta_key::HDR_MARKER, b"not a slot");
        s.commit(&mut w, b, Commit::Durable).unwrap();
    }
    let opened = Store::open(Layout {
        storage: dir.path().to_path_buf(),
        opener: Box::new(lazy::SqliteOpener::new(&dir.path().join("spv"))),
        io: std::sync::Arc::new(OsIo::default()),
        per: TEST_PER,
    });
    assert!(
        matches!(&opened, Err(Error::Layout(why)) if why.contains("hdr_marker")),
        "{:?}",
        opened.err()
    );
}

/// D18, Sol r1 #3: once the latch is set, every mutating entry refuses before it writes: the
/// fast append, an overwrite, SQLite-only metadata, subscriber and MN commits, `sync_wal`
/// (before its barrier, with a Buffered commit pending), a raise, a lowering, a step, establish
/// and reset. No file changes.
#[test]
fn every_mutating_entry_refuses_once_latched() {
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    let a = linked(0, 6, 0, [0; 32]);
    s.put_headers(0, &a).unwrap();
    s.put_metadata("k", b"v").unwrap();
    s.sync_wal().unwrap();
    s.put_metadata("k", b"w").unwrap();
    let before = crate::storage::io::files_of(dir.path());
    s.latch.set("a probe");
    let refused = |what: &str, r: Result<()>| {
        assert!(matches!(r, Err(Error::Poisoned(_))), "{what}: {r:?}");
    };
    refused("append", s.put_headers(6, &linked(6, 2, 0, a[5].0)));
    refused("overwrite", s.put_headers(3, &linked(3, 2, 1, a[2].0)));
    refused("metadata", s.put_metadata("k", b"x"));
    refused("subscriber", s.put_sub(Subscriber::FilterHeaders, 0, &[&[1; 32]]));
    refused("mn", s.put_mn_message(2, 0, b"mn"));
    refused("sync_wal", s.sync_wal());
    refused("raise", s.raise());
    refused("lowering", s.truncate_headers(2));
    refused("step", s.begin_step().map(drop));
    refused("establish", s.establish());
    refused("reset", s.reset(candidate()));
    assert_eq!(crate::storage::io::files_of(dir.path()), before, "a latched store writes nothing");
}

/// D18: a latch set while a step is open (an append's failure under the lookup lock) refuses
/// its commit first, before any other check or change to memory.
#[test]
fn a_step_commit_refuses_once_latched() {
    let dir = tempfile::tempdir().unwrap();
    let s = mem_store(&dir);
    let a = linked(0, 6, 0, [0; 32]);
    s.put_headers(0, &a).unwrap();
    assert!(s.hdr().runs.open().is_some());
    let runs = format!("{:?}", s.hdr().runs);
    let mut st = s.begin_step().unwrap();
    // An off-chain record, which closes the open run (D5 (2)) unless the latch refuses first.
    st.rows(&chain(6, 3, 1, 1, Some((BlockId(2), a[2].0)))).unwrap();
    s.latch.set("a probe");
    let r = st.commit(&tail(None, meta(5, 7)), Durability::Buffered);
    assert!(matches!(r, Err(Error::Poisoned(_))), "{r:?}");
    assert_eq!(format!("{:?}", s.hdr().runs), runs);
}

/// D18, Sol r1 #3: a `clear_all` whose segment listing fails latches the store, so no later write
/// trusts a directory it could not read.
#[test]
#[cfg(unix)]
fn a_failed_segment_listing_latches() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    s.put_headers(0, &linked(0, 6, 0, [0; 32])).unwrap();
    s.raise().unwrap();
    let headers = dir.path().join(super::HEADERS_DIR);
    let mode = |m| std::fs::set_permissions(&headers, std::fs::Permissions::from_mode(m)).unwrap();
    mode(0o300);
    let cleared = s.clear_all();
    mode(0o700);
    assert!(cleared.is_err(), "the listing failed");
    assert!(matches!(s.put_metadata("k", b"v"), Err(Error::Poisoned(_))));
}

/// SB14 §3 Concurrency (r3 N4): raises race subscriber commits, then lowerings. Each slot a raise
/// writes carries its snapshot, so the reopened store reads back the chain and rows it left.
#[test]
fn raises_race_subscriber_commits_and_lowerings() {
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    let f = Subscriber::FilterHeaders;
    let mut chain = linked(0, 1, 0, [0; 32]);
    s.put_headers(0, &chain).unwrap();
    let done = AtomicBool::new(false);
    std::thread::scope(|t| {
        t.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                s.raise().unwrap();
            }
        });
        // Header appends, each followed by the subscriber at the new tip.
        for i in 0..200u32 {
            let next = linked(i + 1, 1, 0, chain[i as usize].0);
            s.put_headers(i + 1, &next).unwrap();
            chain.extend(next);
            s.put_sub(f, i + 1, &[&[i as u8; 32][..]]).unwrap();
        }
        // Lowerings (reorgs of 1–20 records) racing the raises: an overwrite (the writer
        // mutex's activation), or a truncation and then a fast-path append, which starts the new
        // run under the lookup lock alone while a raise may hold a snapshot (M1). Either way the
        // new run's row must go with the first slot that covers it (D5 (3)).
        for r in 0..20u32 {
            let tip = chain.len() as u32 - 1;
            let at = tip - (r % 20) - 1;
            let branch = linked(at + 1, 25, 1 + r as u8, chain[at as usize].0);
            if r % 2 == 1 {
                s.truncate_headers(at).unwrap();
            }
            s.put_headers(at + 1, &branch).unwrap();
            chain.truncate(at as usize + 1);
            chain.extend(branch);
            for _ in 0..5 {
                let tip = chain.len() as u32;
                let next = linked(tip, 2, 1 + r as u8, chain[tip as usize - 1].0);
                s.put_headers(tip, &next).unwrap();
                chain.extend(next);
            }
        }
        done.store(true, Ordering::Relaxed);
    });
    s.sync_wal().unwrap();
    let sub = s.view().sub(f);
    drop(s);
    let s = sqlite_store(&dir);
    assert_eq!(s.headers(0..chain.len() as u32).unwrap(), chain);
    assert_eq!(s.view().active.hi, Some(chain.len() as u32 - 1));
    assert_eq!(s.view().sub(f), sub);
    let (_, at) = s.locator(f).unwrap().expect("a locator");
    assert_eq!(s.active_hash(at).unwrap(), s.locator(f).unwrap().map(|l| l.0));
}

/// A xorshift for the property test (seeded, reproducible).
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u32) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as u32
    }
}

/// SB14 §3's property test: random appends, overwrites, reorgs (a truncation, then a branch),
/// clears and reopens, a lowering followed by linear appends among them (r2 M3), on SQLite
/// against a model on the memory engine's store. The oracle checks the active chain, `by_id`,
/// `by_hash` and id uniqueness after every operation.
#[test]
fn random_header_writes_match_the_model() {
    for seed in 1..=8u64 {
        let dir = tempfile::tempdir().unwrap();
        let mdir = tempfile::tempdir().unwrap();
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let mut s = sqlite_store(&dir);
        let m = mem_store(&mdir);
        let mut chain: Vec<RawHeader> = Vec::new();
        let mut stored: Vec<[u8; 32]> = Vec::new();
        for (op, branch) in (0..60).zip(1u8..) {
            let tip = chain.len() as u32;
            let prev = |chain: &[RawHeader], at: u32| match at {
                0 => [0; 32],
                at => chain[at as usize - 1].0,
            };
            match rng.below(10) {
                0..=3 => {
                    let new = linked(tip, 1 + rng.below(12), branch, prev(&chain, tip));
                    for st in [&s as &dyn LegacyWrites, &m] {
                        st.put(tip, &new);
                    }
                    stored.extend(new.iter().map(|(h, _)| *h));
                    chain.extend(new);
                }
                4..=5 if tip > 1 => {
                    // An overwrite from `at`, longer or shorter than what it replaces.
                    let at = 1 + rng.below(tip - 1);
                    let new = linked(at, 1 + rng.below(8), branch, prev(&chain, at));
                    for st in [&s as &dyn LegacyWrites, &m] {
                        st.put(at, &new);
                    }
                    stored.extend(new.iter().map(|(h, _)| *h));
                    // The old records above the new ones stay active (v2's overwrite).
                    chain.splice(at as usize..(at as usize + new.len()).min(chain.len()), new);
                }
                6 if tip > 1 => {
                    let at = rng.below(tip - 1);
                    for st in [&s as &dyn LegacyWrites, &m] {
                        st.truncate(at);
                    }
                    chain.truncate(at as usize + 1);
                }
                7 if op % 3 == 0 => {
                    s.clear_all().unwrap();
                    m.clear_all().unwrap();
                    chain.clear();
                    stored.clear();
                }
                8 => {
                    s.sync_wal().unwrap();
                    drop(s);
                    s = sqlite_store(&dir);
                }
                _ => {
                    s.raise().unwrap();
                    m.raise().unwrap();
                }
            }
            check_model(&s, &chain, &stored);
            check_model(&m, &chain, &stored);
            assert_eq!(active(&s), active(&m), "seed {seed} op {op}: the same ids");
        }
        drop(m);
    }
}

/// The legacy header writes, for either engine's store.
trait LegacyWrites {
    fn put(&self, at: u32, headers: &[RawHeader]);
    fn truncate(&self, at: u32);
}

impl<E: KvEngine + 'static> LegacyWrites for Store<E> {
    fn put(&self, at: u32, headers: &[RawHeader]) {
        self.put_headers(at, headers).unwrap();
    }

    fn truncate(&self, at: u32) {
        self.truncate_headers(at).unwrap();
    }
}

/// The state a subscriber's writes leave: its bounds, locator, rows and the committed tip.
type SubState = (Bounds, Option<([u8; 32], u32)>, Vec<Option<Vec<u8>>>, Option<u32>);

fn sub_state<E: KvEngine + 'static>(s: &Store<E>, sub: Subscriber) -> SubState {
    let bounds = s.view().sub(sub);
    let rows = match (bounds.lo, bounds.hi) {
        (Some(lo), Some(hi)) => {
            (lo..=hi).map(|h| s.sub_range(sub, h..h + 1).ok().map(|mut v| v.remove(0))).collect()
        }
        _ => Vec::new(),
    };
    let marker = s.hdr().committed.tip();
    let locator = s.locator(sub).unwrap();
    // §3: no subscriber row names a height above the marker.
    assert!(locator.is_none_or(|(_, h)| marker.is_some_and(|m| h <= m)), "{locator:?} {marker:?}");
    (bounds, locator, rows, marker)
}

/// 12 durable headers, and `sub` holding `0..top` (the values `[h; 32]`), durable.
fn sub_race_store(dir: &tempfile::TempDir, sub: Subscriber, top: u32) -> Store<SqliteEngine> {
    let s = sqlite_store(dir);
    s.put_headers(0, &linked(0, 12, 0, [0; 32])).unwrap();
    put_values(&s, sub, 0, top);
    s.sync_wal().unwrap();
    s
}

fn put_values<E: KvEngine + 'static>(s: &Store<E>, sub: Subscriber, from: u32, to: u32) {
    let vals: Vec<[u8; 32]> = (from..to).map(|h| [h as u8; 32]).collect();
    let refs: Vec<&[u8]> = vals.iter().map(|v| &v[..]).collect();
    s.put_sub(sub, from, &refs).unwrap();
}

use dash_kv_sqlite::SqliteEngine;

/// D16 (3), Opus-h r1 M1: a lowering that lands after a subscriber commit started, before it
/// takes the lease, is what the commit sees: its bounds and locator are read under the lease,
/// as if the lowering came first. The write at 8 is below the old top, so stale bounds would
/// keep rows 5..=9 the rewind deleted, and a stale locator would name 9.
#[test]
fn a_subscriber_commit_reads_a_lowering_before_its_lease() {
    let f = Subscriber::FilterHeaders;
    let (raced, plain) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let s = sub_race_store(&raced, f, 10);
    s.interleave(Point::Naming, |s| s.truncate_headers(4).unwrap());
    put_values(&s, f, 8, 9);
    let r = sub_race_store(&plain, f, 10);
    r.truncate_headers(4).unwrap();
    put_values(&r, f, 8, 9);
    assert_eq!(sub_state(&s, f), sub_state(&r, f));
}

/// D16 (3), Opus-h r2 m2 (mutant J): a truncation's bounds are read again under the lease. With
/// the lock-free pre-check's bounds, it would publish `hi = 7` over rows 5..=7 the lowering's
/// rewind deleted.
#[test]
fn a_subscriber_truncation_reads_a_lowering_before_its_lease() {
    let f = Subscriber::FilterHeaders;
    let (raced, plain) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let s = sub_race_store(&raced, f, 11);
    s.interleave(Point::Naming, |s| s.truncate_headers(4).unwrap());
    s.truncate_sub(f, 7).unwrap();
    let r = sub_race_store(&plain, f, 11);
    r.truncate_headers(4).unwrap();
    r.truncate_sub(f, 7).unwrap();
    assert_eq!(sub_state(&s, f), sub_state(&r, f));
}

/// Runs `f` the `n`th time (from 0) a caller reaches `at`.
fn at_nth<E: KvEngine + 'static>(
    s: &Store<E>,
    at: Point,
    n: u32,
    f: impl FnOnce(&Store<E>) + Send + 'static,
) {
    s.interleave(at, move |s| match n {
        0 => f(s),
        n => at_nth(s, at, n - 1, f),
    });
}

/// D16 (2), Opus-h r1 M1: a subscriber commit that names a height above the committed tip is
/// built again under the writer mutex, after the raise; a fast append past the raised tip before
/// that build cannot make it name a height above the slot it carries.
#[test]
fn a_slot_carrying_subscriber_commit_names_at_most_its_slot() {
    let f = Subscriber::FilterHeaders;
    let dir = tempfile::tempdir().unwrap();
    let s = sub_race_store(&dir, f, 11);
    let a = linked(0, 16, 0, [0; 32]);
    s.put_headers(12, &a[12..14]).unwrap();
    // Attempt 0 lock-free, attempt 1 raises (to 13), attempt 2 builds with the slot.
    let more = a[14..].to_vec();
    at_nth(&s, Point::Naming, 2, move |s| s.put_headers(14, &more).unwrap());
    put_values(&s, f, 11, 16);
    let (_, locator, _, marker) = sub_state(&s, f);
    assert_eq!((locator.map(|l| l.1), marker), (Some(13), Some(13)));
    assert_eq!(s.view().active.hi, Some(15));
}

/// D16 (3), Opus-h r2 m2 (mutant K): a masternode message is a subscriber commit too. Its named
/// height is capped at the slot it carries, so a fast append past the raised tip before its
/// build does not make it name a height the slot does not cover (`Corrupt`).
#[test]
fn a_slot_carrying_masternode_message_names_at_most_its_slot() {
    let dir = tempfile::tempdir().unwrap();
    let s = sub_race_store(&dir, Subscriber::FilterHeaders, 11);
    let a = linked(0, 16, 0, [0; 32]);
    s.put_headers(12, &a[12..14]).unwrap();
    // Attempt 0 lock-free, attempt 1 raises (to 13), attempt 2 builds with the slot.
    let more = a[14..].to_vec();
    at_nth(&s, Point::Naming, 2, move |s| s.put_headers(14, &more).unwrap());
    s.put_mn_message(15, 1, b"mn").unwrap();
    assert_eq!(s.mn_message(15, 1).unwrap().as_deref(), Some(&b"mn"[..]));
    assert_eq!(s.hdr().committed.tip(), Some(13));
    assert_eq!(s.view().active.hi, Some(15));
}

/// D16 (3), Opus-h r1 M1: a subscriber commit that lands inside a lowering, before its first
/// batch takes the lease, is rewound by it, as if it came first.
#[test]
fn a_lowering_rewinds_a_subscriber_commit_before_its_lease() {
    let f = Subscriber::FilterHeaders;
    let (raced, plain) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let s = sub_race_store(&raced, f, 4);
    s.interleave(Point::LowerBatch(0), move |s| put_values(s, f, 4, 11));
    s.truncate_headers(4).unwrap();
    let r = sub_race_store(&plain, f, 4);
    put_values(&r, f, 4, 11);
    r.truncate_headers(4).unwrap();
    assert_eq!(sub_state(&s, f), sub_state(&r, f));
}

/// D16 (3), Opus-h r1 M1: a subscriber commit between two lowering batches (its locator below
/// the first batch's slot) is rewound by the next batch, before a slot drops below the locator.
#[test]
fn a_later_lowering_batch_rewinds_a_subscriber_commit() {
    let f = Subscriber::FilterHeaders;
    let (raced, plain) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let s = sub_race_store(&raced, f, 4);
    // Batches of three from 11: 9..=11, then 6..=8 (batch 1), then 5.
    s.interleave(Point::LowerBatch(1), move |s| put_values(s, f, 4, 11));
    s.truncate_headers(4).unwrap();
    let r = sub_race_store(&plain, f, 4);
    put_values(&r, f, 4, 11);
    r.truncate_headers(4).unwrap();
    assert_eq!(sub_state(&s, f), sub_state(&r, f));
}

/// D40 (Opus-h r1 M2): a fast append that opens a run after `sync_wal`'s check goes with a
/// Durable slot: nothing is Buffered when `sync_wal` returns.
#[test]
fn a_run_opened_after_sync_wals_check_is_durable() {
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    let a = linked(0, 8, 0, [0; 32]);
    s.put_headers(0, &a).unwrap();
    s.truncate_headers(5).unwrap();
    s.sync_wal().unwrap();
    let b = linked(6, 2, 1, a[5].0);
    s.interleave(Point::SyncWalChecked, move |s| s.put_headers(6, &b).unwrap());
    s.sync_wal().unwrap();
    assert!(!s.hdr().runs.dirty, "the append opened a run");
    assert!(!s.has_buffered());
}

/// D18 (Opus-h r1 M8): an fd-cache eviction whose failed sync latched during a raise's syncs (it
/// consumed the error the raise's own sync would have seen) stops the raise before its slot.
#[test]
fn a_raise_refuses_a_failure_latched_during_its_syncs() {
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    s.put_headers(0, &linked(0, 6, 0, [0; 32])).unwrap();
    s.interleave(Point::RaiseSynced, |s| {
        let _ = s.hdr().note::<()>(Err(std::io::Error::other("an evicted file's sync")));
    });
    assert!(s.raise().is_err());
    assert_eq!(s.hdr().committed, Committed::Empty);
}

/// D18 (Opus-h r1 M11): the fast append checks the latch again under the lookup lock: a write
/// under it may have latched after the entry's lock-free check.
#[test]
fn a_fast_append_refuses_a_latch_set_before_its_lock() {
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    let a = linked(0, 8, 0, [0; 32]);
    s.put_headers(0, &a[..6]).unwrap();
    s.interleave(Point::AppendUnlocked, |s| s.latch.set("a write under the lookup lock"));
    assert!(matches!(s.put_headers(6, &a[6..]), Err(Error::Poisoned(_))));
    assert_eq!(s.view().active.hi, Some(5));
}

/// §3 Raise step 5 (Opus-h r1 M9): a file-slot raise after a Buffered slot-carrying SQLite commit
/// waits for the barrier, so no durable file slot sits over a commit a cut can lose.
#[test]
fn a_file_slot_raise_waits_for_a_buffered_slot_commit() {
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    let a = linked(0, 9, 0, [0; 32]);
    s.put_headers(0, &a[..6]).unwrap();
    // Names 5, above the committed tip: a Buffered commit with the SQLite slot.
    s.put_mn_message(5, 1, b"mn").unwrap();
    assert!(s.has_buffered());
    s.put_headers(6, &a[6..]).unwrap();
    s.raise().unwrap();
    assert!(s.hdr().committed.tip() == Some(8) && !s.has_buffered());
}

/// Two file raises, 0..=5 then 0..=8, on a header-only store. Returns the slot index holding the
/// newer one.
fn two_file_raises(dir: &tempfile::TempDir) -> usize {
    let s = sqlite_store(dir);
    let a = linked(0, 9, 0, [0; 32]);
    s.put_headers(0, &a[..6]).unwrap();
    s.raise().unwrap();
    s.put_headers(6, &a[6..]).unwrap();
    s.raise().unwrap();
    assert!(s.db().is_none(), "file slots only");
    let bytes = std::fs::read(dir.path().join(HEADERS_DIR).join(hdr::TIP_FILE)).unwrap();
    let seq = |i: usize| {
        let at = FILE_SLOT_OFFSETS[i] as usize;
        Slot::decode(&bytes[at..at + SLOT_LEN]).map(|s| s.seq)
    };
    match (seq(0), seq(1)) {
        (Some(a), Some(b)) => usize::from(b > a),
        slots => panic!("a slot is missing: {slots:?}"),
    }
}

/// D2 (Opus-h r1 M13): each file raise writes the slot not holding the newest file `seq`, so a
/// torn newest slot leaves the previous raise.
#[test]
fn a_torn_newest_file_slot_leaves_the_previous_raise() {
    let dir = tempfile::tempdir().unwrap();
    let newest = two_file_raises(&dir);
    let tip = dir.path().join(HEADERS_DIR).join(hdr::TIP_FILE);
    let mut bytes = std::fs::read(&tip).unwrap();
    bytes[FILE_SLOT_OFFSETS[newest] as usize + 3] ^= 0xff;
    std::fs::write(&tip, bytes).unwrap();
    let s = sqlite_store(&dir);
    assert_eq!(s.view().active.hi, Some(5));
    assert_eq!(s.headers(0..6).unwrap(), linked(0, 6, 0, [0; 32]));
}

/// The OS's files, with reads of the slots at the given offsets of `block_headers/tip` failing
/// with the given OS error: EIO is a torn sector failing its ECC.
struct UnreadableSlots(OsIo, Vec<u64>, i32);

struct UnreadableFile(std::fs::File, Vec<u64>, i32);

#[cfg(unix)]
const EIO: i32 = 5;
const EACCES: i32 = 13;

impl hdr::io::HdrFile for UnreadableFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        match self.1.contains(&offset) {
            true => Err(std::io::Error::from_raw_os_error(self.2)),
            false => self.0.read_at(buf, offset),
        }
    }

    fn write_all_at(&self, data: &[u8], offset: u64) -> std::io::Result<()> {
        self.0.write_all_at(data, offset)
    }

    fn sync_data(&self) -> std::io::Result<()> {
        self.0.sync_data()
    }

    fn set_len(&self, len: u64) -> std::io::Result<()> {
        hdr::io::HdrFile::set_len(&self.0, len)
    }
}

impl HdrIo for UnreadableSlots {
    type File = UnreadableFile;

    fn open(&self, path: &Path, create: bool) -> std::io::Result<Option<UnreadableFile>> {
        let fail = match path.ends_with(hdr::TIP_FILE) {
            true => self.1.clone(),
            false => Vec::new(),
        };
        Ok(self.0.open(path, create)?.map(|f| UnreadableFile(f, fail, self.2)))
    }

    fn remove(&self, path: &Path) -> std::io::Result<()> {
        self.0.remove(path)
    }

    fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
        self.0.sync_dir(dir)
    }

    fn create_dir(&self, dir: &Path) -> std::io::Result<bool> {
        self.0.create_dir(dir)
    }

    fn list(&self, dir: &Path) -> std::io::Result<Option<Vec<String>>> {
        self.0.list(dir)
    }
}

/// D43 (Opus-h r1 m2): a file slot that reads back as EIO (torn by a power cut) is invalid, as
/// one that does not decode is: the other slot wins. Both unreadable fail the open as `Io`.
#[test]
#[cfg(unix)]
fn an_unreadable_file_slot_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let newest = two_file_raises(&dir);
    let open = |fail: Vec<u64>| {
        Store::open(Layout {
            storage: dir.path().to_path_buf(),
            opener: Box::new(lazy::SqliteOpener::new(&dir.path().join("spv"))),
            io: std::sync::Arc::new(UnreadableSlots(OsIo::default(), fail, EIO)),
            per: TEST_PER,
        })
    };
    assert!(matches!(open(FILE_SLOT_OFFSETS.to_vec()), Err(Error::Io(_))));
    let s = open(vec![FILE_SLOT_OFFSETS[newest]]).unwrap();
    assert_eq!(s.view().active.hi, Some(5));
    drop(s);
    // The open zeroed it (validator r6 m2): read back again, it does not outrank the winner
    // over the files truncated to 5.
    let tip = std::fs::read(dir.path().join(HEADERS_DIR).join(hdr::TIP_FILE)).unwrap();
    let at = FILE_SLOT_OFFSETS[newest] as usize;
    assert_eq!(tip[at..at + SLOT_LEN], [0; SLOT_LEN]);
    assert_eq!(sqlite_store(&dir).view().active.hi, Some(5));
}

/// D43: only EIO makes a slot invalid; any other read error fails the open, changing nothing.
#[test]
fn a_slot_read_error_other_than_eio_fails_the_open() {
    let dir = tempfile::tempdir().unwrap();
    let newest = two_file_raises(&dir);
    let tip = dir.path().join(HEADERS_DIR).join(hdr::TIP_FILE);
    let before = std::fs::read(&tip).unwrap();
    let opened = Store::open(Layout {
        storage: dir.path().to_path_buf(),
        opener: Box::new(lazy::SqliteOpener::new(&dir.path().join("spv"))),
        io: std::sync::Arc::new(UnreadableSlots(
            OsIo::default(),
            vec![FILE_SLOT_OFFSETS[newest]],
            EACCES,
        )),
        per: TEST_PER,
    });
    assert!(matches!(opened, Err(Error::Io(_))));
    assert_eq!(std::fs::read(&tip).unwrap(), before);
    assert_eq!(sqlite_store(&dir).view().active.hi, Some(8));
}

/// SQLite, counting `write_barrier` calls.
struct Barriers(SqliteEngine, std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl dash_kv::KvRead for Barriers {
    fn get(&self, cf: dash_kv::Cf, key: &[u8]) -> dash_kv::Result<Option<Vec<u8>>> {
        self.0.get(cf, key)
    }

    fn multi_get(&self, cf: dash_kv::Cf, keys: &[&[u8]]) -> dash_kv::Result<Vec<Option<Vec<u8>>>> {
        self.0.multi_get(cf, keys)
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: dash_kv::Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = dash_kv::Result<(Vec<u8>, Vec<u8>)>> + 'a {
        self.0.iter_prefix(cf, prefix)
    }
}

impl KvEngine for Barriers {
    type Batch = <SqliteEngine as KvEngine>::Batch;
    type Snapshot<'a> = <SqliteEngine as KvEngine>::Snapshot<'a>;

    fn schema(&self) -> &'static dash_kv::DbSchema {
        self.0.schema()
    }

    fn batch(&self) -> Self::Batch {
        self.0.batch()
    }

    fn write(&self, batch: Self::Batch, commit: Commit) -> dash_kv::Result<()> {
        self.0.write(batch, commit)
    }

    fn write_barrier(&self, batch: Self::Batch) -> dash_kv::Result<()> {
        self.1.fetch_add(1, Ordering::Relaxed);
        self.0.write_barrier(batch)
    }

    fn sync_wal(&self) -> dash_kv::Result<()> {
        self.0.sync_wal()
    }

    fn flush_memtables(&self) -> dash_kv::Result<()> {
        self.0.flush_memtables()
    }

    fn snapshot(&self) -> Self::Snapshot<'_> {
        self.0.snapshot()
    }

    fn approximate_size(&self, cf: dash_kv::Cf) -> dash_kv::Result<u64> {
        self.0.approximate_size(cf)
    }
}

struct BarrierOpener(lazy::SqliteOpener, std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl DbOpener<Barriers> for BarrierOpener {
    fn open_existing(&self) -> Result<Option<Barriers>> {
        Ok(self.0.open_existing()?.map(|e| Barriers(e, self.1.clone())))
    }

    fn create(&self) -> Result<Barriers> {
        Ok(Barriers(self.0.create()?, self.1.clone()))
    }
}

/// D22 (Opus-h r1 M7): open's commit (the runs it rebuilt, or nothing) is a forced barrier, so it
/// makes durable a Buffered commit an earlier process acknowledged.
#[test]
fn open_commits_through_the_write_barrier() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = sqlite_store(&dir);
        s.put_headers(0, &linked(0, 6, 0, [0; 32])).unwrap();
        s.put_metadata("k", b"v").unwrap();
    }
    let barriers = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let opener = BarrierOpener(lazy::SqliteOpener::new(&dir.path().join("spv")), barriers.clone());
    let s = Store::open(Layout {
        storage: dir.path().to_path_buf(),
        opener: Box::new(opener),
        io: std::sync::Arc::new(OsIo::default()),
        per: TEST_PER,
    })
    .unwrap();
    assert_eq!(barriers.load(Ordering::Relaxed), 1);
    assert_eq!(s.metadata("k").unwrap().as_deref(), Some(&b"v"[..]));
}
