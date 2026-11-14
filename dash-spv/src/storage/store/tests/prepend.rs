use std::collections::{BTreeMap, BTreeSet};
use std::sync::{mpsc, Arc};
use std::thread;

use dash_kv::faulty::{
    EventSel, Fault, FaultKind, FaultOutcome, Mark, MarkKind, World, WorldConfig,
};
use dashcore::block::Header as CoreHeader;
use dashcore::consensus::deserialize;

use super::*;
use crate::bridge::headers::hashed_header_to_consensus;
use crate::storage::io::files_of as all_files;
use crate::storage::store::crash::{FaultyOpener, PER};
use crate::storage::store::hdr::io::FaultyIo;
use crate::types::HashedBlockHeader;

fn files_of(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    all_files(dir).into_iter().filter(|(p, _)| !p.to_string_lossy().ends_with("-shm")).collect()
}

fn x11_chain(from: u32, n: u32, prev: [u8; 32]) -> Vec<RawHeader> {
    x11_branch(from, n, prev, 0)
}

fn x11_branch(from: u32, n: u32, mut prev: [u8; 32], branch: u8) -> Vec<RawHeader> {
    (from..from + n)
        .map(|height| {
            let bytes = header(prev, height, branch);
            let h: CoreHeader = deserialize(&bytes).unwrap();
            prev = h.block_hash().to_byte_array();
            (prev, bytes)
        })
        .collect()
}

fn ids<E: KvEngine + 'static, F: HdrIo>(s: &Store<E, F>) -> Vec<(u32, u32)> {
    s.active_from(0).unwrap().into_iter().map(|(h, id)| (h, id.0)).collect()
}

fn assert_chain<E: KvEngine, F: HdrIo>(s: &Store<E, F>, start: u32, raw: &[RawHeader]) {
    assert_eq!(s.view().active.lo, Some(start));
    assert_eq!(s.headers(start..start + raw.len() as u32).unwrap(), raw);
    for (i, (hash, _)) in raw.iter().enumerate() {
        assert_eq!(s.active_height_of(hash).unwrap(), Some(start + i as u32));
    }
    assert_eq!(s.view().meta.unwrap().anchor, (BlockHash::from(raw[0].0), start));
}

#[test]
fn a_prepend_lowers_the_start() {
    let raw = x11_chain(75, 55, [0; 32]);
    let dir = tempfile::tempdir().unwrap();
    let s = mem_store(&dir);
    s.put_headers(100, &raw[25..]).unwrap();
    let old = ids(&s);
    s.put_headers(75, &raw[..25]).unwrap();
    assert_chain(&s, 75, &raw);
    assert_eq!(&ids(&s)[25..], old);
    assert_eq!(
        s.hdr().runs.runs()[0],
        Run {
            h0: 75,
            id0: 30,
            len: Some(25)
        }
    );
}

#[test]
fn a_prepend_across_segments() {
    for (p, s, t) in [(75, 100, 129), (101, 103, 103)] {
        let raw = x11_chain(p, t - p + 1, [0; 32]);
        let dir = tempfile::tempdir().unwrap();
        let store = sqlite_store(&dir);
        store.put_headers(s, &raw[(s - p) as usize..]).unwrap();
        store.put_headers(p, &raw[..(s - p) as usize]).unwrap();
        drop(store);
        assert_chain(&sqlite_store(&dir), p, &raw);
    }
}

#[test]
fn a_prepend_keeps_the_index_above_the_old_start() {
    let raw = x11_chain(20, 110, [0; 32]);
    let dir = tempfile::tempdir().unwrap();
    let s = mem_store(&dir);
    s.put_headers(100, &raw[80..]).unwrap();
    s.put_headers(60, &raw[40..80]).unwrap();
    s.put_headers(20, &raw[..40]).unwrap();
    assert_chain(&s, 20, &raw);
}

#[test]
fn a_changed_header_is_refused_by_the_re_hash() {
    let mut raw = x11_chain(75, 55, [0; 32]);
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    s.put_headers(100, &raw[25..]).unwrap();
    s.sync_wal().unwrap();
    let before = files_of(dir.path());
    raw[5].1[76] ^= 1;
    assert!(matches!(s.put_headers(75, &raw[..25]), Err(Error::Refused(_))));
    assert_eq!(s.view().active.lo, Some(100));
    assert!(files_of(dir.path()) == before, "persistent files changed");
    assert!(!dir.path().join("spv/kv.sqlite").exists());
}

#[test]
fn a_prepend_must_join_the_start() {
    let raw = x11_chain(75, 55, [0; 32]);
    let wrong = x11_chain(75, 25, [1; 32]);
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    s.put_headers(100, &raw[25..]).unwrap();
    s.sync_wal().unwrap();
    let before = files_of(dir.path());
    for (p, batch) in [(75, &wrong[..]), (75, &raw[..24]), (75, &raw[..26])] {
        assert!(matches!(s.put_headers(p, batch), Err(Error::Refused(_))));
        assert!(files_of(dir.path()) == before, "persistent files changed");
        assert_eq!(s.view().active.lo, Some(100));
    }
    s.put_headers(75, &raw[..25]).unwrap();
}

#[test]
fn a_prepend_materializes_base_runs() {
    for sqlite in [false, true] {
        let raw = x11_chain(75, 55, [0; 32]);
        let dir = tempfile::tempdir().unwrap();
        let s = sqlite_store(&dir);
        s.put_headers(100, &raw[25..]).unwrap();
        s.sync_wal().unwrap();
        if sqlite {
            s.put_sub(Subscriber::FilterHeaders, 99, &[b"row"]).unwrap();
        }
        assert_eq!(dir.path().join("spv/kv.sqlite").exists(), sqlite);
        assert!(s.rows_read().meta.is_none());
        assert!(s.hdr().runs.implicit);
        let old = ids(&s);
        s.put_headers(75, &raw[..25]).unwrap();
        let check = |s: &Store<dash_kv_sqlite::SqliteEngine>| {
            assert_chain(s, 75, &raw);
            assert_eq!(&ids(s)[25..], old);
            assert_eq!(
                s.hdr().runs.runs()[1],
                Run {
                    h0: 100,
                    id0: 0,
                    len: Some(30)
                }
            );
            assert_eq!(s.hdr().runs.next_id, 55);
            assert_eq!(s.rows_read().meta.unwrap().anchor.1, 75);
        };
        check(&s);
        drop(s);
        check(&sqlite_store(&dir));
    }
}

#[test]
fn repeated_prepends_keep_ids() {
    let raw = x11_chain(50, 85, [0; 32]);
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    s.put_headers(100, &raw[50..80]).unwrap();
    s.put_headers(75, &raw[25..50]).unwrap();
    s.put_headers(130, &raw[80..]).unwrap();
    assert_eq!(s.active_id(130), Some(BlockId(55)));
    s.put_headers(50, &raw[..25]).unwrap();
    let before = ids(&s);
    assert_eq!(before.iter().map(|(_, id)| id).collect::<BTreeSet<_>>().len(), 85);
    assert_eq!(s.view().meta.unwrap().anchor.1, 50);
    drop(s);
    let s = sqlite_store(&dir);
    assert_eq!(ids(&s), before);
    s.truncate_headers(80).unwrap();
    let branch = x11_branch(81, 3, raw[30].0, 1);
    s.put_headers(81, &branch).unwrap();
    assert_eq!(s.active_id(81), Some(BlockId(85)));
    assert!(s.truncate_headers(49).is_err());
    s.put_headers(45, &x11_chain(45, 5, [0; 32])).unwrap_err();
}

#[test]
fn a_prepend_with_explicit_runs_and_stored_meta() {
    let raw = x11_chain(75, 55, [0; 32]);
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    s.put_headers(100, &raw[25..]).unwrap();
    s.truncate_headers(120).unwrap();
    s.put_headers(121, &raw[46..]).unwrap();
    let old = ids(&s);
    let next = s.view().meta.unwrap().next_id.0;
    s.put_headers(75, &raw[..25]).unwrap();
    assert_eq!(s.active_id(75), Some(BlockId(next)));
    assert_eq!(&ids(&s)[25..], old);
    assert_eq!(s.rows_read().meta.unwrap().anchor.1, 75);
    drop(s);
    assert_chain(&sqlite_store(&dir), 75, &raw);
}

#[test]
fn a_prepend_near_id_exhaustion_is_refused() {
    let raw = x11_chain(97, 33, [0; 32]);
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    s.put_headers(100, &raw[3..]).unwrap();
    let m = s.view().meta.unwrap();
    s.begin_step()
        .unwrap()
        .commit(&tail(None, meta(m.tip.0, u32::MAX - 2)), Durability::Durable)
        .unwrap();
    let before = files_of(dir.path());
    let old = ids(&s);
    let m = s.view().meta;
    assert!(matches!(s.put_headers(97, &raw[..3]), Err(Error::Refused(_))));
    assert!(files_of(dir.path()) == before, "persistent files changed");
    assert_eq!(s.view().meta, m);
    assert_eq!(ids(&s), old);
    assert_eq!(s.view().active.lo, Some(100));
    s.put_headers(98, &raw[1..3]).unwrap();
    assert_eq!(s.view().meta.unwrap().next_id.0, u32::MAX);
}

#[test]
fn an_implicit_run_reservation_checks_id_arithmetic() {
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    s.put_headers(100, &x11_chain(100, 30, [0; 32])).unwrap();
    assert_eq!(s.hdr().runs.prepend_ids(u32::MAX, 129), None);
}

#[test]
fn a_prepend_keeps_work_and_time_ordered() {
    let mut raw = x11_chain(10, 1120, [0; 32]);
    let mut prev = [0; 32];
    for (i, (hash, bytes)) in raw.iter_mut().enumerate() {
        bytes[4..36].copy_from_slice(&prev);
        if i == 0 {
            bytes[68..72].copy_from_slice(&10_000u32.to_le_bytes());
        }
        *hash = deserialize::<CoreHeader>(bytes).unwrap().block_hash().to_byte_array();
        prev = *hash;
    }
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    s.put_headers(1100, &raw[1090..]).unwrap();
    let branch = x11_branch(1120, 10, raw[1109].0, 1);
    s.put_headers(1120, &branch).unwrap();
    let active_hash = branch[9].0;
    let leaf_hash = raw[1119].0;
    let before_active = s.by_hash(&active_hash.into()).unwrap().unwrap();
    let before_leaf = s.by_hash(&leaf_hash.into()).unwrap().unwrap();
    let delta = raw[..1090].iter().fold(Work::from_u64(0), |w, (_, h)| w + hdr::proof(h));
    let time = raw[..1090].iter().map(|(_, h)| hdr::time_and_bits(h).0).max().unwrap();
    assert!(time > before_active.time_max);
    assert!(time > before_leaf.time_max);
    s.put_headers(10, &raw[..1090]).unwrap();
    let (hash, bytes) = s.headers(500..501).unwrap()[0];
    let hashed: HashedBlockHeader = deserialize(&[&bytes[..], &hash[..]].concat()).unwrap();
    assert!(hashed_header_to_consensus(&hashed).is_ok());
    let check = |s: &Store<dash_kv_sqlite::SqliteEngine>| {
        for (hash, before) in
            [(active_hash, before_active.clone()), (leaf_hash, before_leaf.clone())]
        {
            let after = s.by_hash(&hash.into()).unwrap().unwrap().to_row();
            let before = before.to_row();
            assert_eq!(
                Work::from_bendian(after.chain_work),
                Work::from_bendian(before.chain_work) + delta
            );
            assert_eq!(after.time_max, before.time_max.max(time));
        }
    };
    check(&s);
    drop(s);
    check(&sqlite_store(&dir));
}

#[test]
fn a_prepend_refuses_offchain_roots_with_different_ancestry() {
    let a = x11_chain(75, 55, [0; 32]);
    let b = x11_chain(75, 55, [1; 32]);
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    s.put_headers(100, &a[25..]).unwrap();
    s.put_headers(100, &b[25..]).unwrap();
    s.sync_wal().unwrap();
    let old = ids(&s);
    let old_meta = s.view().meta;
    let before = files_of(dir.path());
    assert!(matches!(s.put_headers(75, &b[..25]), Err(Error::Refused(_))));
    assert!(files_of(dir.path()) == before, "persistent files changed");
    assert_eq!(s.view().meta, old_meta);
    assert_eq!(ids(&s), old);
    drop(s);
    let s = sqlite_store(&dir);
    assert_eq!(s.view().active.lo, Some(100));
    assert_eq!(s.view().meta, old_meta);
    assert_eq!(ids(&s), old);
    assert!(matches!(s.put_headers(75, &b[..25]), Err(Error::Refused(_))));
}

#[test]
fn covered_requires_both_committed_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let s = mem_store(&dir);
    s.put_headers(100, &x11_chain(100, 30, [0; 32])).unwrap();
    s.sync_wal().unwrap();
    assert!(!s.covered(99));
    assert!(s.covered(100));
    assert!(s.covered(129));
    assert!(!s.covered(130));
    let committed = s.hdr().committed;
    s.put_sub(Subscriber::FilterHeaders, 99, &[b"below"]).unwrap();
    assert_eq!(s.locator(Subscriber::FilterHeaders).unwrap(), None);
    s.put_sub(Subscriber::FilterHeaders, 100, &[b"inside"]).unwrap();
    assert_eq!(s.hdr().committed, committed);
}

#[test]
fn a_prepend_commits_a_fast_append_from_its_snapshot() {
    let raw = x11_chain(75, 60, [0; 32]);
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    s.put_headers(100, &raw[25..55]).unwrap();
    let append = raw[55..].to_vec();
    s.interleave(Point::PrependPublished, move |s| s.put_headers(130, &append).unwrap());
    s.put_headers(75, &raw[..25]).unwrap();
    assert_eq!(s.active_id(130), Some(BlockId(55)));
    drop(s);
    assert_chain(&sqlite_store(&dir), 75, &raw);
}

fn faulty_store(world: &World) -> Store<dash_kv::faulty::FaultyEngine, FaultyIo> {
    Store::open(Layout {
        storage: PathBuf::from(dash_kv::faulty::DATADIR),
        opener: Box::new(FaultyOpener {
            world: world.clone(),
            recovery: dash_kv::faulty::Recovery::Prefix,
        }),
        io: Arc::new(FaultyIo(world.fs())),
        per: PER,
    })
    .unwrap()
}

fn waits_for_a_prepend(establish: bool) {
    let world = World::new(WorldConfig {
        faults: vec![Fault {
            at: EventSel::AfterMark {
                mark: MarkKind::FlushBegin,
                occurrence: 0,
                n: 0,
            },
            kind: FaultKind::Io,
            outcome: FaultOutcome::NotApplied,
        }],
        ..WorldConfig::default()
    });
    let raw = x11_chain(4, 20, [0; 32]);
    let s = Arc::new(faulty_store(&world));
    s.put_headers(16, &raw[12..]).unwrap();
    s.sync_wal().unwrap();
    s.put_sub(Subscriber::FilterHeaders, 15, &[b"row"]).unwrap();
    let second = s.clone();
    let (join_tx, join_rx) = mpsc::channel();
    let world_for_mark = world.clone();
    s.interleave(Point::PrependPublished, move |s| {
        let (tx, rx) = mpsc::channel();
        let waiting = tx.clone();
        s.interleave(Point::WriterWaiting, move |_| waiting.send(true).unwrap());
        let worker = thread::spawn(move || {
            let result = if establish {
                second.establish()
            } else {
                second.put_sub(Subscriber::FilterHeaders, 15, &[b"row"])
            };
            let _ = tx.send(false);
            result
        });
        let waited = rx.recv().unwrap();
        join_tx.send((worker, waited)).unwrap();
        let world = world_for_mark.clone();
        s.interleave(Point::RaiseSynced, move |_| {
            world.mark(Mark::FlushBegin {
                flush: 1,
                old: None,
                new: [0; 32],
            })
        });
    });
    let prepend = s.put_headers(4, &raw[..12]);
    let (worker, waited) = join_rx.recv().unwrap();
    let second = worker.join().unwrap();
    let fault_fired = world.fault_fired();
    drop(s);
    world.crash_in_place(dash_kv::faulty::CrashKind::ProcessKill, Box::new(KeepAll));
    let db = world.engine_with("spv", &SPV_SCHEMA, dash_kv::faulty::Recovery::Prefix);
    assert!(
        db.get(META, meta_key::ANCHOR).unwrap().is_none(),
        "boot image names the uncommitted prefix"
    );
    let s = faulty_store(&world);
    assert_eq!(s.view().active.lo, Some(16));
    assert_eq!(s.locator(Subscriber::FilterHeaders).unwrap(), None);
    assert_eq!(s.sub_range(Subscriber::FilterHeaders, 15..16).unwrap(), [b"row".to_vec()]);
    assert!(s.rows_read().meta.is_none());
    assert!(s.hdr().runs.runs().iter().all(|r| r.h0 >= 16));
    s.recover().unwrap();
    assert_eq!(s.sub_range(Subscriber::FilterHeaders, 15..16).unwrap(), [b"row".to_vec()]);
    assert!(prepend.is_err());
    assert!(second.is_err());
    assert!(waited);
    assert!(fault_fired);
}

struct KeepAll;
impl dash_kv::faulty::Decider for KeepAll {
    fn choose(&mut self, _: &dash_kv::faulty::Var) -> u32 {
        0
    }
}

#[test]
fn a_subscriber_commit_below_the_start_waits_for_a_prepend() {
    waits_for_a_prepend(false);
}

#[test]
fn establish_waits_for_a_prepend() {
    waits_for_a_prepend(true);
}

#[test]
fn a_prepend_racing_a_raise() {
    let raw = x11_chain(75, 55, [0; 32]);
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(sqlite_store(&dir));
    s.put_headers(100, &raw[25..]).unwrap();
    let second = s.clone();
    let prefix = raw[..25].to_vec();
    let (join_tx, join_rx) = mpsc::channel();
    s.interleave(Point::RaiseSynced, move |s| {
        let (tx, rx) = mpsc::channel();
        s.interleave(Point::WriterWaiting, move |_| tx.send(()).unwrap());
        let worker = thread::spawn(move || second.put_headers(75, &prefix));
        rx.recv().unwrap();
        join_tx.send(worker).unwrap();
    });
    s.raise().unwrap();
    join_rx.recv().unwrap().join().unwrap().unwrap();
    let hw = s.hw();
    let (_, file_seq) = hw.file_slot.unwrap();
    assert!(file_seq < hw.seq);
    let mut bytes = [0; SLOT_LEN];
    hw.tip
        .as_ref()
        .unwrap()
        .read_full(&mut bytes, FILE_SLOT_OFFSETS[hw.file_slot.unwrap().0])
        .unwrap();
    let slot = Slot::decode(&bytes).unwrap();
    assert_eq!(slot.seq, file_seq);
    assert!(matches!(
        slot.committed,
        Committed::Range {
            start: 100,
            tip: 129,
            ..
        }
    ));
    drop(hw);
    drop(s);
    assert_chain(&sqlite_store(&dir), 75, &raw);
}

#[test]
fn a_prepend_reservation_refusal_does_not_poison() {
    let raw = x11_chain(98, 33, [0; 32]);
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    s.put_headers(100, &raw[2..32]).unwrap();
    s.truncate_headers(128).unwrap();
    s.put_headers(129, &raw[31..32]).unwrap();
    let m = s.view().meta.unwrap();
    s.begin_step()
        .unwrap()
        .commit(&tail(None, meta(m.tip.0, u32::MAX - 2)), Durability::Durable)
        .unwrap();
    let append = raw[32..].to_vec();
    s.interleave(Point::PrependChecked, move |s| s.put_headers(130, &append).unwrap());
    assert!(matches!(s.put_headers(98, &raw[..2]), Err(Error::Refused(_))));
    assert_eq!(s.view().active.lo, Some(100));
    assert!(s.halted().is_ok());
    s.sync_wal().unwrap();
}

#[test]
fn a_prepend_is_refused_on_a_dash_chain_store() {
    for (context, override_id) in [(false, 0), (false, 5), (true, 0)] {
        let raw = x11_chain(75, 55, [0; 32]);
        let dir = tempfile::tempdir().unwrap();
        let s = sqlite_store(&dir);
        s.put_headers(100, &raw[25..]).unwrap();
        let m = s.view().meta.unwrap();
        let mut row = s.by_id(BlockId(override_id)).unwrap().unwrap().to_row();
        let record = if context {
            let sibling = x11_branch(99, 1, raw[23].0, 1)[0];
            row.hash = sibling.0;
            row.header = sibling.1;
            row.height = 99;
            row.id = 30;
            row.kind = 1;
            HeaderRecord::from_row(&row).unwrap()
        } else {
            row.status |= Status::CONFLICT_CHAINLOCK;
            HeaderRecord::from_row(&row).unwrap()
        };
        let mut step = s.begin_step().unwrap();
        if context {
            step.rows(&[StepRow::Record(record)]).unwrap();
        } else {
            step.rows(&[StepRow::Status(BlockId(override_id), Status(record.to_row().status))])
                .unwrap();
        }
        step.commit(&tail(None, meta(m.tip.0, 31)), Durability::Durable).unwrap();
        if override_id == 5 {
            assert!(s.hdr().anchor.is_none());
        }
        assert!(matches!(s.put_headers(75, &raw[..25]), Err(Error::Refused(_))));
        drop(s);
        let s = sqlite_store(&dir);
        if override_id == 5 {
            assert!(s.hdr().anchor.is_none());
        }
        assert!(matches!(s.put_headers(75, &raw[..25]), Err(Error::Refused(_))));
    }
}

#[test]
fn a_prepend_commits_only_the_file_snapshot() {
    let raw = x11_chain(75, 60, [0; 32]);
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    s.put_headers(100, &raw[25..55]).unwrap();
    let append = raw[55..].to_vec();
    s.interleave(Point::RaiseSynced, move |s| s.put_headers(130, &append).unwrap());
    s.put_headers(75, &raw[..25]).unwrap();
    assert_eq!(s.view().active.hi, Some(134));
    drop(s);
    let s = sqlite_store(&dir);
    assert_chain(&s, 75, &raw[..55]);
    assert_eq!(s.view().active.hi, Some(129));
    let meta = s.rows_read().meta.unwrap();
    assert_eq!(meta.tip, s.active_id(129).unwrap());
    assert_eq!(meta.next_id, BlockId(55));
}

#[test]
fn a_disconnected_prepend_with_valid_hashes_is_refused() {
    let mut raw = x11_chain(75, 55, [0; 32]);
    let disconnected = x11_branch(80, 1, [1; 32], 1)[0];
    raw[5] = disconnected;
    let dir = tempfile::tempdir().unwrap();
    let s = sqlite_store(&dir);
    s.put_headers(100, &raw[25..]).unwrap();
    s.sync_wal().unwrap();
    let before = files_of(dir.path());
    assert!(matches!(s.put_headers(75, &raw[..25]), Err(Error::Refused(_))));
    assert!(files_of(dir.path()) == before);
    assert_eq!(s.view().active.lo, Some(100));
    drop(s);
    assert!(matches!(sqlite_store(&dir).put_headers(75, &raw[..25]), Err(Error::Refused(_))));
}
