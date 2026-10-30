//! The `dm-log-writes` reader and replayer over synthetic logs written here in the kernel's layout
//! (`dm-log-writes.c`). Synthetic: these pin the format and the replay arithmetic; a log the
//! kernel wrote is checked by the privileged self-test (ST3b S2).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use dash_testkit::crash::image::Replayer;
use dash_testkit::crash::logwrites::{
    DISCARD, Draw, Entry, FLUSH, FUA, Log, LogError, MAGIC, MARK, Part, Point, PointKind,
    SPLIT_UNIT, points, replay_order, units,
};

const BLOCK: usize = 4096;
const DEVICE_BLOCKS: u64 = 16;

/// One synthetic bio: (sector, blocks, flags, fill byte or mark name).
enum Op {
    Write(u64, u64, u64, u8),
    Flush,
    Discard(u64, u64),
    Mark(&'static str),
}

/// A log in the kernel's layout: the super block, then each entry's block and its data blocks.
fn log_bytes(ops: &[Op]) -> Vec<u8> {
    let mut out = vec![0u8; BLOCK];
    out[0..8].copy_from_slice(&MAGIC.to_le_bytes());
    out[8..16].copy_from_slice(&1u64.to_le_bytes());
    out[16..24].copy_from_slice(&(ops.len() as u64).to_le_bytes());
    out[24..28].copy_from_slice(&(BLOCK as u32).to_le_bytes());
    for op in ops {
        let mut head = vec![0u8; BLOCK];
        let (sector, n, flags, data_len, data) = match *op {
            Op::Write(s, n, f, fill) => (s, n, f, 0, vec![fill; n as usize * BLOCK]),
            Op::Flush => (0, 0, FLUSH, 0, vec![]),
            Op::Discard(s, n) => (s, n, DISCARD, 0, vec![]),
            Op::Mark(name) => {
                head[32..32 + name.len()].copy_from_slice(name.as_bytes());
                (0, 0, MARK, name.len() as u64, vec![])
            }
        };
        for (at, v) in [sector, n, flags, data_len].into_iter().enumerate() {
            head[at * 8..at * 8 + 8].copy_from_slice(&v.to_le_bytes());
        }
        out.extend(head);
        out.extend(data);
    }
    out
}

/// The device after `ops[..prefix]` and then `extra` and `part` in log order, as the replay should
/// leave it.
fn model(ops: &[Op], prefix: usize, extra: &[usize], part: Option<&Part>) -> Vec<u8> {
    let mut dev = vec![0u8; DEVICE_BLOCKS as usize * BLOCK];
    let mut later: Vec<usize> = extra.iter().copied().chain(part.map(|p| p.entry)).collect();
    later.sort_unstable();
    let order: Vec<usize> = (0..prefix).chain(later).collect();
    for i in order {
        match ops[i] {
            Op::Write(s, _, _, fill) if part.is_some_and(|p| p.entry == i) => {
                for &b in &part.expect("matched").blocks {
                    let at = (s + b) as usize * BLOCK;
                    dev[at..at + BLOCK].fill(fill);
                }
            }
            Op::Write(s, n, _, fill) => {
                dev[s as usize * BLOCK..(s + n) as usize * BLOCK].fill(fill);
            }
            Op::Discard(s, n) => dev[s as usize * BLOCK..(s + n) as usize * BLOCK].fill(0),
            Op::Flush | Op::Mark(_) => {}
        }
    }
    dev
}

/// A workload with every entry kind: writes before and after flushes, a pre-flush write, FUA
/// writes, an overwrite, a discard and two marks.
fn workload() -> Vec<Op> {
    vec![
        Op::Write(0, 2, 0, 0x11),
        Op::Mark("start"),
        Op::Write(4, 1, 0, 0x22),
        Op::Write(5, 3, 0, 0x33),
        Op::Flush,
        Op::Write(1, 1, 0, 0x44),
        Op::Write(8, 1, FUA, 0x55),
        Op::Write(9, 2, 0, 0x66),
        Op::Write(12, 1, FLUSH | FUA, 0x77),
        Op::Discard(4, 2),
        Op::Write(5, 1, 0, 0x88),
        Op::Mark("a1"),
        Op::Write(14, 2, 0, 0x99),
    ]
}

fn write_file(path: &Path, bytes: &[u8]) {
    File::create(path).unwrap().write_all(bytes).unwrap();
}

/// A zeroed image of the device's size.
fn zero_image(path: &Path) -> File {
    let f =
        OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path).unwrap();
    f.set_len(DEVICE_BLOCKS * BLOCK as u64).unwrap();
    f
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

#[test]
fn the_index_reads_every_entry_and_mark() {
    let ops = workload();
    let log = Log::read(std::io::Cursor::new(log_bytes(&ops))).unwrap();
    assert_eq!(log.sector_size, BLOCK as u32);
    assert_eq!(log.entries.len(), ops.len());
    assert_eq!(log.mark("start"), Some(2));
    assert_eq!(log.mark("a1"), Some(12));
    assert_eq!(log.marks().collect::<Vec<_>>(), [(2, "start"), (12, "a1")]);
    let e = &log.entries[8];
    assert_eq!((e.sector, e.nr_sectors, e.is(FLUSH), e.is(FUA)), (12, 1, true, true));
    assert!(!log.entries[4].has_data() && !log.entries[9].has_data());
}

#[test]
fn flush_and_fua_points_are_the_durable_prefixes() {
    let log = Log::read(std::io::Cursor::new(log_bytes(&workload()))).unwrap();
    let got: Vec<(PointKind, usize)> =
        points(&log, 2, Draw::default()).into_iter().map(|p| (p.kind, p.prefix)).collect();
    // The flush at 4 and the pre-flush write at 8 make what precedes them durable; the FUA writes at
    // 6 and 8 are durable themselves.
    assert_eq!(
        got,
        [(PointKind::Flush, 4), (PointKind::Fua, 7), (PointKind::Flush, 8), (PointKind::Fua, 9)]
    );
    // Nothing before the start mark.
    assert!(points(&log, 9, Draw::default()).is_empty());
}

#[test]
fn every_prefix_replays_to_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let ops = workload();
    let log_path = dir.path().join("log");
    write_file(&log_path, &log_bytes(&ops));
    let log = Log::open(&log_path).unwrap();
    let mut r =
        Replayer::new(&log, File::open(&log_path).unwrap(), zero_image(&dir.path().join("img")));
    for prefix in 0..=ops.len() {
        let snap = dir.path().join(format!("p{prefix}"));
        let point = Point {
            kind: PointKind::Rand,
            prefix,
            extra: vec![],
            part: None,
        };
        r.snapshot(&point, &snap).unwrap();
        assert_eq!(read(&snap), model(&ops, prefix, &[], None), "prefix {prefix}");
    }
    assert_eq!(r.applied(), ops.len());
}

#[test]
fn torn_points_keep_the_fua_writes_and_replay_their_subset() {
    let dir = tempfile::tempdir().unwrap();
    let ops = workload();
    let log_path = dir.path().join("log");
    write_file(&log_path, &log_bytes(&ops));
    let log = Log::open(&log_path).unwrap();
    let draw = Draw {
        torn: 64,
        seed: 7,
        ..Draw::default()
    };
    let torn: Vec<Point> =
        points(&log, 2, draw).into_iter().filter(|p| p.kind == PointKind::Torn).collect();
    assert!(torn.len() > 4, "distinct subsets: {}", torn.len());
    assert_eq!(points(&log, 2, draw), points(&log, 2, draw), "seeded");
    let windows = [(2, 4), (4, 8), (8, ops.len())];
    let mut r =
        Replayer::new(&log, File::open(&log_path).unwrap(), zero_image(&dir.path().join("img")));
    for (n, p) in torn.iter().enumerate() {
        let &(a, b) = windows.iter().find(|w| w.0 == p.prefix).expect("a window start");
        assert!(p.extra.iter().all(|&i| (a..b).contains(&i)), "{p:?}");
        for i in a..b {
            if log.entries[i].is(FUA) {
                assert!(p.extra.contains(&i), "FUA entry {i} kept: {p:?}");
            }
        }
        let snap = dir.path().join(format!("t{n}"));
        r.snapshot(p, &snap).unwrap();
        assert_eq!(read(&snap), model(&ops, p.prefix, &p.extra, None), "{p:?}");
    }
}

/// ST3b r1 m-5: a split point keeps part of one multi-block, non-FUA write of its window (the first
/// k blocks or a seeded subset, never none or all) with a torn subset of the rest, and replays it.
#[test]
fn split_points_keep_part_of_one_write_and_replay_it() {
    let dir = tempfile::tempdir().unwrap();
    let ops = workload();
    let log_path = dir.path().join("log");
    write_file(&log_path, &log_bytes(&ops));
    let log = Log::open(&log_path).unwrap();
    let draw = Draw {
        rand: 16,
        torn: 16,
        split: 64,
        seed: 7,
    };
    let all = points(&log, 2, draw);
    let without = points(
        &log,
        2,
        Draw {
            split: 0,
            ..draw
        },
    );
    // Split points are drawn last and listed last (ST3b r2 n-1): every other point keeps its state
    // and its index. Drawn first, they would change the rand and torn draws.
    assert_eq!(all[..without.len()], without[..]);
    let split: Vec<Point> = all[without.len()..].to_vec();
    assert!(split.iter().all(|p| p.kind == PointKind::Split));
    assert!(split.windows(2).all(|w| w[0].prefix <= w[1].prefix));
    // Replayed in prefix order, the split points among the rest.
    let order = replay_order(&all);
    assert!(order.windows(2).all(|w| all[w[0]].prefix <= all[w[1]].prefix));
    assert_eq!(order.len(), all.len());
    assert!(split.len() > 8, "distinct splits: {}", split.len());
    let windows = [(2, 4), (4, 8), (8, ops.len())];
    // The multi-block writes without FUA: 3 (3 blocks), 7 and 12 (2 each).
    let (mut prefixes, mut entries) = (Vec::new(), Vec::new());
    let mut r =
        Replayer::new(&log, File::open(&log_path).unwrap(), zero_image(&dir.path().join("img")));
    for (n, p) in split.iter().enumerate() {
        let &(a, b) = windows.iter().find(|w| w.0 == p.prefix).expect("a window start");
        let part = p.part.as_ref().expect("a part");
        assert!((a..b).contains(&part.entry) && [3, 7, 12].contains(&part.entry), "{p:?}");
        let blocks = log.entries[part.entry].nr_sectors;
        assert!(!part.blocks.is_empty() && (part.blocks.len() as u64) < blocks, "{p:?}");
        assert!(
            part.blocks.windows(2).all(|w| w[0] < w[1])
                && part.blocks[part.blocks.len() - 1] < blocks
        );
        assert!(!p.extra.contains(&part.entry), "{p:?}");
        assert!(p.extra.iter().all(|&i| (a..b).contains(&i)), "{p:?}");
        for i in a..b {
            if log.entries[i].is(FUA) {
                assert!(p.extra.contains(&i), "FUA entry {i} kept: {p:?}");
            }
        }
        prefixes.push(p.prefix);
        entries.push((part.entry, part.blocks.clone()));
        let snap = dir.path().join(format!("s{n}"));
        r.snapshot(p, &snap).unwrap();
        assert_eq!(read(&snap), model(&ops, p.prefix, &p.extra, Some(part)), "{p:?}");
    }
    // Every window and every subset shape of the 3-block write is drawn: a lone tail block is no
    // first-k part, so seeing one shows the subset draw.
    for w in windows {
        assert!(prefixes.contains(&w.0), "window {w:?}");
    }
    assert!(entries.contains(&(3, vec![0])) && entries.contains(&(3, vec![0, 1])));
    assert!(entries.iter().any(|(e, b)| *e == 3 && b[0] > 0), "{entries:?}");
}

/// ST3b r2: on a 512 B-sector log a split tears only between the device's 4 KiB units: a write
/// within one unit (ext4's superblock block) never splits, and every part is whole units.
#[test]
fn split_points_tear_whole_4k_units_of_the_device() {
    let entry = |sector, nr_sectors, flags, mark: Option<&str>| Entry {
        sector,
        nr_sectors,
        flags,
        data_offset: 0,
        mark: mark.map(str::to_owned),
    };
    let per = SPLIT_UNIT / 512;
    let log = Log {
        sector_size: 512,
        entries: vec![
            entry(0, 0, MARK, Some("start")),
            entry(0, 0, FLUSH, None),
            // One aligned unit (a superblock block): never split.
            entry(0, per, 0, None),
            // Sectors 6..26: units 0 (2 sectors), 1, 2, 3 (2 sectors).
            entry(6, 20, 0, None),
            entry(0, 0, FLUSH, None),
        ],
    };
    let groups = units(&log.entries[3], 512);
    assert_eq!(groups, [0..2, 2..10, 10..18, 18..20]);
    assert_eq!(units(&log.entries[2], 512).len(), 1);
    let draw = Draw {
        split: 64,
        seed: 3,
        ..Draw::default()
    };
    let split: Vec<Point> =
        points(&log, 1, draw).into_iter().filter(|p| p.kind == PointKind::Split).collect();
    assert!(split.len() > 4, "{}", split.len());
    for p in &split {
        let part = p.part.as_ref().expect("a part");
        assert_eq!(part.entry, 3, "{p:?}");
        let whole: Vec<u64> = groups
            .iter()
            .filter(|g| (*g).clone().any(|b| part.blocks.contains(&b)))
            .flat_map(|g| g.clone())
            .collect();
        assert_eq!(part.blocks, whole, "whole units only: {p:?}");
        assert!(!part.blocks.is_empty() && part.blocks.len() < 20, "{p:?}");
    }
}

#[test]
fn malformed_logs_are_refused() {
    let ops = workload();
    let good = log_bytes(&ops);
    let read = |bytes: Vec<u8>| Log::read(std::io::Cursor::new(bytes));
    let mut bad_magic = good.clone();
    bad_magic[0] ^= 1;
    assert!(matches!(read(bad_magic), Err(LogError::Super(_))));
    let mut bad_version = good.clone();
    bad_version[8] = 2;
    assert!(matches!(read(bad_version), Err(LogError::Super(_))));
    let short = good[..good.len() - 1].to_vec();
    assert!(matches!(read(short), Err(LogError::Truncated { .. })));
    let mut bad_flags = good.clone();
    bad_flags[BLOCK + 16] = 0x80;
    assert!(matches!(
        read(bad_flags),
        Err(LogError::Entry {
            index: 0,
            ..
        })
    ));
}
