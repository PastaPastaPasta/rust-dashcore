use super::*;
use crate::datadir_import::cutfs::{CutFs, Image, Snap};
use std::collections::BTreeMap;

const NET: &str = "/core/.dashcore/regtest";
const BACKUP: &str = "core-import-19700101T000000Z";
const VARIANTS: u64 = 11;

fn crash_variant(fs: &CutFs, variant: u64) {
    match variant {
        0 => fs.crash(),
        1 => fs.crash_keep(),
        2 => fs.crash_torn_head(),
        s => fs.crash_mixed(s - 3),
    }
}

fn net() -> &'static Path {
    Path::new(NET)
}
fn at(rel: &str) -> PathBuf {
    net().join(rel)
}
fn backup() -> PathBuf {
    at(BACKUP)
}

fn fixture_image() -> Image {
    CutFs::image()
        .with_file(at("chainstate/000003.ldb"), b"chainstate table")
        .with_file(at("chainstate/CURRENT"), b"MANIFEST-000002\n")
        .with_file(at("blocks/index/000004.ldb"), b"block index")
        .with_file(at("blocks/blk00000.dat"), b"blk")
        .with_file(at("blocks/rev00000.dat"), b"rev")
        .with_file(at("evodb/000001.ldb"), b"evodb")
        .with_file(at("llmq/000001.ldb"), b"llmq")
        .with_file(at("indexes/txindex/000001.ldb"), b"txindex")
        .with_file(at("indexes/blockfilter/basic/fltr00000.dat"), b"filter")
        .with_file(at("peers.dat"), b"peers")
        .with_file(at("wallets/wallet.dat"), b"wallet")
        .with_file(at(".lock"), b"")
}
fn fixture() -> CutFs {
    fixture_image().build()
}

type Tree = BTreeMap<PathBuf, Option<Vec<u8>>>;

fn tree(snap: BTreeMap<PathBuf, Snap>) -> Tree {
    snap.into_iter()
        .map(|(path, node)| {
            let data = match node {
                Snap::File {
                    data,
                    ..
                } => Some(data),
                Snap::Dir => None,
                Snap::Symlink(target) => {
                    panic!("symlink {} -> {}", path.display(), target.display())
                }
            };
            (path, data)
        })
        .collect()
}

/// The caller's loop: resume, else begin, then complete.
fn drive(fs: &CutFs) -> Result<PathBuf> {
    match recover(fs, net())? {
        Recovered::Clean if fs.metadata(&at("chainstate")).map_err(io_at(net()))?.is_some() => {
            begin(fs, net())?.complete(fs)
        }
        Recovered::Clean => {
            assert!(fs.metadata(&backup().join(JOURNAL)).unwrap().is_some(), "lost import");
            Ok(backup())
        }
        Recovered::Moved(m) => m.complete(fs),
        Recovered::Finished {
            backup,
        } => Ok(backup),
    }
}

fn finished(image: Image) -> Tree {
    let fs = image.build();
    assert_eq!(drive(&fs).unwrap(), backup());
    tree(fs.snapshot())
}

/// The mutating calls an uncut run of `f` makes on a fork of `fs`.
fn total_ops(fs: &CutFs, f: impl Fn(&CutFs)) -> u64 {
    let fork = fs.fork();
    fork.reset_ops();
    f(&fork);
    fork.ops()
}

/// Every member file is at exactly one of its place and the backup, with its bytes, and every
/// other original file is where it was.
fn assert_nothing_lost(original: &Tree, now: &Tree, ctx: &str) {
    for (path, data) in original {
        let Some(data) = data else {
            continue;
        };
        let rel = path.strip_prefix(NET).unwrap();
        let member = MEMBERS.iter().any(|m| rel.starts_with(m));
        let here = now.get(path).cloned().flatten();
        if !member {
            assert_eq!(here.as_ref(), Some(data), "{} moved ({ctx})", path.display());
            continue;
        }
        let there = now.get(&backup().join(rel)).cloned().flatten();
        match (here, there) {
            (Some(d), None) | (None, Some(d)) => assert_eq!(&d, data, "{} ({ctx})", path.display()),
            (a, b) => panic!("{}: here {a:?}, backup {b:?} ({ctx})", path.display()),
        }
    }
}

fn names(fs: &CutFs, dir: &Path) -> Vec<String> {
    fs.read_dir(dir)
        .unwrap()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

fn state_for(fs: &CutFs, phase: Phase) -> JournalState {
    let mut members = [MemberId::default(); MEMBERS.len()];
    for (id, name) in members.iter_mut().zip(MEMBERS) {
        if let Some(m) = fs.metadata(&at(name)).unwrap() {
            *id = MemberId {
                present: true,
                dev: m.dev,
                ino: m.ino,
            };
        }
    }
    JournalState {
        phase,
        at_ms: 0,
        backup: BACKUP.into(),
        members,
    }
}

/// `fs` with an `Intent` journal for its members as they are now.
fn with_intent(fs: &CutFs) {
    fs.append(&at(JOURNAL), &state_for(fs, Phase::Intent).encode()).unwrap();
}

fn refuses_untouched(fs: &CutFs, check: impl FnOnce(RegtestImportError)) {
    let before = fs.snapshot();
    fs.reset_ops();
    match recover(fs, net()) {
        Err(e) => check(e),
        Ok(r) => panic!("recovered: {r:?}"),
    }
    assert_eq!(fs.ops(), 0, "{:?}", fs.log());
    assert_eq!(fs.snapshot(), before);
}

#[test]
fn every_member_moves_including_indexes() {
    let fs = fixture();
    let original = tree(fs.snapshot());
    let backup_dir = drive(&fs).unwrap();
    assert_eq!(backup_dir, backup());
    for m in MEMBERS {
        assert!(fs.metadata(&at(m)).unwrap().is_none(), "{m} still in place");
        assert!(fs.metadata(&backup().join(m)).unwrap().is_some_and(|m| m.is_dir), "{m}");
    }
    assert_eq!(
        names(&fs, &backup()),
        ["blocks", "chainstate", JOURNAL, "evodb", "indexes", "llmq"]
    );
    assert_eq!(names(&fs, &backup().join("blocks")), ["index"]);
    assert_eq!(names(&fs, &at("blocks")), ["blk00000.dat", "rev00000.dat"]);
    let now = tree(fs.snapshot());
    assert_nothing_lost(&original, &now, "uncut");
    assert_eq!(tree(fs.durable_snapshot()), now, "not all durable");
}

#[test]
fn absent_members_are_recorded_and_skipped() {
    let fs = CutFs::image()
        .with_file(at("chainstate/000003.ldb"), b"chainstate")
        .with_file(at("blocks/blk00000.dat"), b"blk")
        .with_file(at("evodb/000001.ldb"), b"evodb")
        .build();
    let moved = begin(&fs, net()).unwrap();
    let present: Vec<bool> = moved.state().members.iter().map(|m| m.present).collect();
    assert_eq!(present, [true, false, true, false, false]);
    assert!(moved.state().members[1] == MemberId::default());
    moved.complete(&fs).unwrap();
    assert_eq!(names(&fs, &backup()), ["chainstate", JOURNAL, "evodb"]);
    assert_eq!(names(&fs, &at("blocks")), ["blk00000.dat"]);
}

#[test]
fn intent_is_durable_before_the_first_rename() {
    let fs = fixture();
    fs.reset_ops();
    begin(&fs, net()).unwrap();
    let log = fs.log();
    let first = log.iter().position(|l| l == &format!("rename {NET}/chainstate")).unwrap();
    let new = format!("{NET}/{JOURNAL_NEW}");
    let want = [
        format!("append {new}"),
        format!("sync_data {new}"),
        format!("rename {new}"),
        format!("sync_dir {NET}"),
    ];
    let mut rest = log[..first].iter();
    for w in &want {
        assert!(rest.any(|l| l == w), "{w} before the first rename: {log:#?}");
    }
    let total = total_ops(&fixture(), |fs| {
        begin(fs, net()).unwrap();
    });
    for k in 1..=total {
        let fs = fixture();
        fs.cut_after(k);
        let _ = begin(&fs, net());
        fs.crash();
        if fs.metadata(&at("chainstate")).unwrap().is_none() {
            let state = read_journal(&fs, net()).unwrap().expect("a durable journal");
            assert!(state.phase >= Phase::Intent, "k={k}");
        }
    }
}

#[test]
fn each_rename_syncs_both_parents_before_the_next() {
    let fs = fixture();
    fs.reset_ops();
    begin(&fs, net()).unwrap();
    let log = fs.log();
    for m in MEMBERS {
        let src = at(m);
        let dst = backup().join(m);
        let i = log.iter().position(|l| l == &format!("rename {}", src.display())).unwrap();
        let next = log[i + 1..]
            .iter()
            .position(|l| l.starts_with("rename "))
            .map_or(log.len(), |j| i + 1 + j);
        let between = &log[i + 1..next];
        for dir in [src.parent().unwrap(), dst.parent().unwrap()] {
            assert!(
                between.contains(&format!("sync_dir {}", dir.display())),
                "{m}: no sync of {} before the next rename: {between:#?}",
                dir.display()
            );
        }
    }
}

#[test]
fn move_aside_cut_matrix() {
    let base = fixture();
    let original = tree(base.snapshot());
    let expected = finished(fixture_image());
    let total = total_ops(&base, |fs| {
        drive(fs).unwrap();
    });
    assert!(total > 20, "{total}");
    for k in 1..=total {
        for variant in 0..VARIANTS {
            let ctx = format!("k={k} variant={variant}");
            let fs = base.fork();
            fs.cut_after(k);
            let _ = drive(&fs);
            assert!(fs.is_dead(), "{ctx}");
            crash_variant(&fs, variant);
            assert_nothing_lost(&original, &tree(fs.snapshot()), &ctx);
            assert_eq!(drive(&fs).unwrap(), backup(), "{ctx}");
            assert_eq!(tree(fs.snapshot()), expected, "{ctx}");
            assert_eq!(tree(fs.durable_snapshot()), expected, "not durable: {ctx}");
        }
    }
}

#[test]
fn recovery_cut_matrix() {
    let base = fixture();
    let original = tree(base.snapshot());
    let expected = finished(fixture_image());
    let total = total_ops(&base, |fs| {
        drive(fs).unwrap();
    });
    for k1 in 1..total {
        for v1 in [0, 1, 3, 7] {
            let crashed = base.fork();
            crashed.cut_after(k1);
            let _ = drive(&crashed);
            crash_variant(&crashed, v1);
            let resume = total_ops(&crashed, |fs| {
                drive(fs).unwrap();
            });
            for k2 in 1..resume {
                for v2 in [0, 4, 9] {
                    let ctx = format!("k1={k1} v1={v1} k2={k2} v2={v2}");
                    let fs = crashed.fork();
                    fs.cut_after(k2);
                    let _ = drive(&fs);
                    crash_variant(&fs, v2);
                    assert_nothing_lost(&original, &tree(fs.snapshot()), &ctx);
                    assert_eq!(drive(&fs).unwrap(), backup(), "{ctx}");
                    assert_eq!(tree(fs.snapshot()), expected, "{ctx}");
                    assert_eq!(tree(fs.durable_snapshot()), expected, "not durable: {ctx}");
                }
            }
        }
    }
}

#[test]
fn moved_returns_for_the_callers_reindex_step() {
    let fs = fixture();
    let moved = begin(&fs, net()).unwrap();
    assert_eq!(moved.state().phase, Phase::Moved);
    fs.crash();
    let Recovered::Moved(resumed) = recover(&fs, net()).unwrap() else {
        panic!("not Moved");
    };
    assert_eq!(resumed, moved);
    assert_eq!(resumed.backup(), backup());
    fs.reset_ops();
    let again = recover(&fs, net()).unwrap();
    assert_eq!(fs.ops(), 0, "Moved recovery wrote: {:?}", fs.log());
    assert_eq!(again, Recovered::Moved(moved));
}

#[test]
fn complete_then_cleanup_moves_the_journal_into_the_backup() {
    let fs = fixture();
    let moved = begin(&fs, net()).unwrap();
    assert_eq!(moved.complete(&fs).unwrap(), backup());
    assert!(fs.metadata(&at(JOURNAL)).unwrap().is_none());
    let state = JournalState::decode(&fs.read(&backup().join(JOURNAL)).unwrap()).unwrap();
    assert_eq!(state.phase, Phase::Complete);
    assert_eq!(recover(&fs, net()).unwrap(), Recovered::Clean);

    let fs = fixture();
    let mut done = begin(&fs, net()).unwrap().state().clone();
    done.phase = Phase::Complete;
    fs.remove(&at(JOURNAL)).unwrap();
    fs.append(&at(JOURNAL), &done.encode()).unwrap();
    assert_eq!(
        recover(&fs, net()).unwrap(),
        Recovered::Finished {
            backup: backup()
        }
    );
    assert!(fs.metadata(&at(JOURNAL)).unwrap().is_none());
    assert_eq!(fs.read(&backup().join(JOURNAL)).unwrap(), done.encode());
}

#[test]
fn begin_refuses_an_existing_journal() {
    let fs = fixture();
    with_intent(&fs);
    let before = fs.snapshot();
    fs.reset_ops();
    assert!(
        matches!(begin(&fs, net()), Err(RegtestImportError::JournalExists(p)) if p == at(JOURNAL))
    );
    assert_eq!(fs.ops(), 0);
    assert_eq!(fs.snapshot(), before);
}

#[test]
fn begin_refuses_a_member_that_is_not_a_directory() {
    for (image, path) in [
        (CutFs::image().with_file(at("chainstate"), b"file"), at("chainstate")),
        (CutFs::image().with_dir(at("chainstate")).with_file(at("llmq"), b"file"), at("llmq")),
        (
            CutFs::image()
                .with_dir(at("chainstate"))
                .with_dir("/elsewhere")
                .with_symlink(at("evodb"), "/elsewhere"),
            at("evodb"),
        ),
        (CutFs::image().with_dir(at("blocks")), at("chainstate")),
    ] {
        let fs = image.build();
        let before = fs.snapshot();
        fs.reset_ops();
        match begin(&fs, net()) {
            Err(RegtestImportError::Member {
                path: p,
                ..
            }) => assert_eq!(p, path),
            other => panic!("{}: {other:?}", path.display()),
        }
        assert_eq!(fs.ops(), 0);
        assert_eq!(fs.snapshot(), before);
    }
}

#[test]
fn resume_refuses_a_member_at_both_or_neither_path() {
    let fs = fixture();
    with_intent(&fs);
    fs.create_dir_all(&backup().join("evodb")).unwrap();
    refuses_untouched(&fs, |e| {
        assert!(
            matches!(e, RegtestImportError::Member { path, detail } if path == at("evodb") && detail.contains("both"))
        )
    });

    let fs = fixture();
    with_intent(&fs);
    fs.remove(&at("llmq")).unwrap();
    fs.create_dir_all(&backup()).unwrap();
    refuses_untouched(&fs, |e| {
        assert!(
            matches!(e, RegtestImportError::Member { path, detail } if path == at("llmq") && detail.contains("neither"))
        )
    });
}

#[test]
fn resume_refuses_a_changed_member() {
    let fs = fixture();
    with_intent(&fs);
    fs.remove(&at("indexes")).unwrap();
    fs.create_dir_all(&at("indexes")).unwrap();
    fs.create_dir_all(&backup()).unwrap();
    refuses_untouched(&fs, |e| {
        assert!(
            matches!(e, RegtestImportError::Member { path, detail } if path == at("indexes") && detail.contains("changed"))
        )
    });

    let fs = fixture();
    with_intent(&fs);
    fs.create_dir_all(&backup().join("blocks")).unwrap();
    fs.remove(&at("chainstate")).unwrap();
    fs.create_dir_all(&backup().join("chainstate")).unwrap();
    refuses_untouched(&fs, |e| {
        assert!(
            matches!(e, RegtestImportError::Member { path, detail } if path == backup().join("chainstate") && detail.contains("changed"))
        )
    });
}

#[test]
fn resume_refuses_a_foreign_backup_entry() {
    for entry in
        [backup().join("wallet.dat"), backup().join("blocks/blk00000.dat"), backup().join(JOURNAL)]
    {
        let fs = fixture();
        with_intent(&fs);
        fs.create_dir_all(entry.parent().unwrap()).unwrap();
        fs.append(&entry, b"x").unwrap();
        refuses_untouched(&fs, |e| {
            assert!(
                matches!(e, RegtestImportError::ForeignBackupEntry { entry: got, .. } if got == entry)
            )
        });
    }

    let fs = CutFs::image().with_dir(at("chainstate")).build();
    with_intent(&fs);
    fs.create_dir_all(&backup().join("blocks")).unwrap();
    refuses_untouched(&fs, |e| {
        assert!(
            matches!(e, RegtestImportError::ForeignBackupEntry { entry, .. } if entry == backup().join("blocks"))
        )
    });

    let fs = fixture();
    with_intent(&fs);
    fs.append(&backup(), b"a file").unwrap();
    refuses_untouched(&fs, |e| {
        assert!(matches!(e, RegtestImportError::Member { path, .. } if path == backup()))
    });
}

#[test]
fn a_corrupt_journal_refuses_with_nothing_touched() {
    let fs = fixture();
    with_intent(&fs);
    let good = fs.read(&at(JOURNAL)).unwrap();
    for cut in [0, 1, good.len() - 1] {
        let fs = fixture();
        fs.append(&at(JOURNAL), &good[..cut]).unwrap();
        refuses_untouched(&fs, |e| {
            assert!(matches!(e, RegtestImportError::JournalCorrupt { .. }), "{e}")
        });
    }
    for i in 0..good.len() {
        let mut bad = good.clone();
        bad[i] ^= 0x40;
        let fs = fixture();
        fs.append(&at(JOURNAL), &bad).unwrap();
        fs.append(&at(JOURNAL_NEW), b"stray").unwrap();
        refuses_untouched(&fs, |e| {
            assert!(matches!(e, RegtestImportError::JournalCorrupt { .. }), "byte {i}: {e}")
        });
    }
}

#[test]
fn a_stray_new_journal_is_removed() {
    let fs = fixture_image().with_file(at(JOURNAL_NEW), b"half a journal").build();
    assert_eq!(recover(&fs, net()).unwrap(), Recovered::Clean);
    assert!(fs.metadata(&at(JOURNAL_NEW)).unwrap().is_none());
    fs.crash();
    assert!(fs.metadata(&at(JOURNAL_NEW)).unwrap().is_none(), "removal not durable");

    let fs = fixture();
    let moved = begin(&fs, net()).unwrap();
    fs.append(&at(JOURNAL_NEW), b"half a journal").unwrap();
    fs.reset_ops();
    assert_eq!(recover(&fs, net()).unwrap(), Recovered::Moved(moved.clone()));
    assert_eq!(fs.ops(), 0);
    moved.complete(&fs).unwrap();
    assert!(fs.metadata(&at(JOURNAL_NEW)).unwrap().is_none());
    assert_eq!(
        names(&fs, &backup()),
        ["blocks", "chainstate", JOURNAL, "evodb", "indexes", "llmq"]
    );
}

#[test]
fn an_existing_backup_name_gets_the_next_suffix() {
    let fs = fixture_image()
        .with_dir(at(BACKUP))
        .with_file(at(&format!("{BACKUP}-1")), b"a file")
        .build();
    let moved = begin(&fs, net()).unwrap();
    assert_eq!(moved.state().backup, format!("{BACKUP}-2"));
    assert_eq!(moved.complete(&fs).unwrap(), at(&format!("{BACKUP}-2")));

    let mut image = fixture_image().with_dir(at(BACKUP));
    for n in 1..=LAST_SUFFIX {
        image = image.with_dir(at(&format!("{BACKUP}-{n}")));
    }
    let fs = image.build();
    let before = fs.snapshot();
    fs.reset_ops();
    assert!(
        matches!(begin(&fs, net()), Err(RegtestImportError::NoFreeBackupName(p)) if p == net())
    );
    assert_eq!(fs.ops(), 0);
    assert_eq!(fs.snapshot(), before);
}

#[test]
fn earlier_backups_are_left_untouched() {
    let earlier = "core-import-19691231T235959Z";
    let fs = fixture_image()
        .with_file(at(&format!("{earlier}/chainstate/000001.ldb")), b"older")
        .with_file(at(&format!("{earlier}/{JOURNAL}")), b"older journal")
        .with_file(at(&format!("{BACKUP}/chainstate/000009.ldb")), b"same second")
        .build();
    let keep = |fs: &CutFs| -> Tree {
        tree(fs.snapshot())
            .into_iter()
            .filter(|(p, _)| p.starts_with(at(earlier)) || p.starts_with(at(BACKUP)))
            .collect()
    };
    let before = keep(&fs);
    let moved = begin(&fs, net()).unwrap();
    assert_eq!(moved.state().backup, format!("{BACKUP}-1"));
    moved.complete(&fs).unwrap();
    assert_eq!(keep(&fs), before);
}

#[test]
fn journal_round_trips_and_the_layout_is_fixed() {
    let state = JournalState {
        phase: Phase::Moved,
        at_ms: 0x0102_0304_0506_0708,
        backup: "core-import-20261126T081500Z-12".into(),
        members: [
            MemberId {
                present: true,
                dev: 0x11,
                ino: 0x22,
            },
            MemberId::default(),
            MemberId {
                present: true,
                dev: u64::MAX,
                ino: 7,
            },
            MemberId::default(),
            MemberId::default(),
        ],
    };
    let raw = state.encode();
    let n = state.backup.len();
    assert_eq!(raw.len(), 108 + n);
    assert_eq!(&raw[..9], b"CH5RJ1\x01\x01\x01");
    assert_eq!(raw[9..17], 0x0102_0304_0506_0708u64.to_le_bytes());
    assert_eq!(usize::from(raw[17]), n);
    assert_eq!(&raw[18..18 + n], state.backup.as_bytes());
    assert_eq!(raw[18 + n], 5);
    let m0 = &raw[19 + n..36 + n];
    assert_eq!(m0[0], 1);
    assert_eq!(m0[1..9], 0x11u64.to_le_bytes());
    assert_eq!(m0[9..17], 0x22u64.to_le_bytes());
    assert!(raw[36 + n..53 + n].iter().all(|&b| b == 0));
    let crc = crate::leveldb::crc32c::mask(crate::leveldb::crc32c::extend(0, &raw[..104 + n]));
    assert_eq!(raw[104 + n..], crc.to_le_bytes());
    assert_eq!(JournalState::decode(&raw).unwrap(), state);
    for phase in [Phase::Intent, Phase::Complete] {
        let s = JournalState {
            phase,
            ..state.clone()
        };
        assert_eq!(JournalState::decode(&s.encode()).unwrap(), s);
    }
}

#[test]
fn journal_decode_rejects_each_malformed_field() {
    let state = JournalState {
        phase: Phase::Intent,
        at_ms: 1,
        backup: BACKUP.into(),
        members: [MemberId {
            present: true,
            dev: 1,
            ino: 2,
        }; 5],
    };
    let good = state.encode();
    let n = BACKUP.len();
    let resealed = |edit: &dyn Fn(&mut Vec<u8>)| {
        let mut body = good[..good.len() - 4].to_vec();
        edit(&mut body);
        let crc = crate::leveldb::crc32c::mask(crate::leveldb::crc32c::extend(0, &body));
        body.extend_from_slice(&crc.to_le_bytes());
        body
    };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("magic", resealed(&|b| b[0] = b'X')),
        ("version", resealed(&|b| b[6] = 2)),
        ("phase", resealed(&|b| b[7] = 3)),
        ("reindex", resealed(&|b| b[8] = 0)),
        ("long name length", resealed(&|b| b[17] += 1)),
        ("short name length", resealed(&|b| b[17] -= 1)),
        ("name prefix", resealed(&|b| b[18] = b'x')),
        ("name byte", resealed(&|b| b[18 + n - 1] = b'/')),
        ("member count", resealed(&|b| b[18 + n] = 4)),
        ("member flag", resealed(&|b| b[19 + n] = 2)),
        ("absent member with an id", resealed(&|b| b[19 + 17 + n] = 0)),
        ("chainstate absent", resealed(&|b| b[19 + n..19 + 17 + n].fill(0))),
        ("trailing byte", {
            let mut v = good.clone();
            v.push(0);
            v
        }),
        ("empty", Vec::new()),
    ];
    for (what, raw) in cases {
        assert!(JournalState::decode(&raw).is_err(), "{what}");
    }
    let absent = resealed(&|b| b[19 + 17 + n..19 + 34 + n].fill(0));
    assert!(!JournalState::decode(&absent).unwrap().members[1].present);
}

#[test]
fn backup_names_are_utc() {
    assert_eq!(utc_name(0), "core-import-19700101T000000Z");
    assert_eq!(utc_name(1_764_115_200_000), "core-import-20251126T000000Z");
    assert_eq!(utc_name(951_825_599_999), "core-import-20000229T115959Z");
    assert_eq!(utc_name(1_767_225_599_999), "core-import-20251231T235959Z");
    assert_eq!(utc_name(4_107_542_400_000), "core-import-21000301T000000Z");
    for at_ms in [0, 1_764_115_200_000, 4_107_542_400_000] {
        let name = format!("{}-{LAST_SUFFIX}", utc_name(at_ms));
        assert!(journal::valid_backup_name(&name), "{name}");
    }
}

#[test]
fn os_fs_moves_a_real_network_directory() {
    use crate::datadir_import::OsImportFs;
    let dir = tempfile::tempdir().unwrap();
    let net = dir.path().join("regtest");
    for (rel, data) in [
        ("chainstate/CURRENT", &b"MANIFEST-000002\n"[..]),
        ("blocks/index/CURRENT", b"MANIFEST-000002\n"),
        ("blocks/blk00000.dat", b"blk"),
        ("evodb/CURRENT", b"MANIFEST-000002\n"),
        ("indexes/txindex/CURRENT", b"MANIFEST-000002\n"),
    ] {
        let path = net.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }
    let fs = OsImportFs::new();
    let moved = begin(&fs, &net).unwrap();
    assert!(valid_now(&moved.state().backup));
    assert_eq!(recover(&fs, &net).unwrap(), Recovered::Moved(moved.clone()));
    let backup = moved.complete(&fs).unwrap();
    assert_eq!(std::fs::read(backup.join("evodb/CURRENT")).unwrap(), b"MANIFEST-000002\n");
    assert_eq!(
        std::fs::read(backup.join("indexes/txindex/CURRENT")).unwrap(),
        b"MANIFEST-000002\n"
    );
    assert_eq!(std::fs::read(net.join("blocks/blk00000.dat")).unwrap(), b"blk");
    assert!(!net.join("chainstate").exists() && !net.join("blocks/index").exists());
    assert!(!net.join(JOURNAL).exists() && backup.join(JOURNAL).exists());
    assert_eq!(recover(&fs, &net).unwrap(), Recovered::Clean);
}

fn valid_now(name: &str) -> bool {
    journal::valid_backup_name(name)
        && name.len() == BACKUP.len()
        && name.ends_with('Z')
        && name > BACKUP
}

#[test]
fn journal_decode_is_total_and_canonical() {
    let mut rng = 0x5eed_u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let good = fixture_journal();
    let mut accepted = 0;
    for round in 0..200_000u32 {
        let mut raw = if round % 2 == 0 {
            good.clone()
        } else {
            (0..next() % 200).map(|_| next() as u8).collect()
        };
        for _ in 0..next() % 4 {
            if raw.is_empty() {
                break;
            }
            let at = (next() as usize) % raw.len();
            match next() % 3 {
                0 => raw[at] ^= 1 << (next() % 8),
                1 => raw.truncate(at),
                _ => raw.insert(at, next() as u8),
            }
        }
        if let Ok(state) = JournalState::decode(&raw) {
            assert_eq!(state.encode(), raw, "round {round}");
            accepted += 1;
        }
    }
    assert!(accepted > 1000, "{accepted}");
}

fn fixture_journal() -> Vec<u8> {
    let fs = fixture();
    state_for(&fs, Phase::Moved).encode()
}

/// `fs` with `st_dev` raised by one for every path under `under`, as another filesystem or a
/// renumbering remount shows it.
struct Shifted<'a> {
    fs: &'a CutFs,
    under: PathBuf,
}

impl ImportFs for Shifted<'_> {
    fn reflink(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.fs.reflink(src, dst)
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.fs.hard_link(src, dst)
    }
    fn copy(&self, src: &Path, dst: &Path) -> io::Result<u64> {
        self.fs.copy(src, dst)
    }
    fn sync_data(&self, path: &Path) -> io::Result<()> {
        self.fs.sync_data(path)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.fs.sync_dir(path)
    }
    fn rename(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.fs.rename(src, dst)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        self.fs.remove(path)
    }
    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        self.fs.truncate(path, len)
    }
    fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.fs.append(path, data)
    }
    fn metadata(&self, path: &Path) -> io::Result<Option<FileMetadata>> {
        let shift = u64::from(path.starts_with(&self.under));
        Ok(self.fs.metadata(path)?.map(|m| FileMetadata {
            dev: m.dev + shift,
            ..m
        }))
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.fs.read_dir(path)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.fs.read(path)
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.fs.create_dir_all(path)
    }
    fn available_space(&self, path: &Path) -> io::Result<u64> {
        self.fs.available_space(path)
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        self.fs.canonicalize(path)
    }
    fn mount_table(&self) -> io::Result<Option<Vec<u8>>> {
        self.fs.mount_table()
    }
    fn mount_id(&self, path: &Path) -> io::Result<Option<u64>> {
        self.fs.mount_id(path)
    }
    fn now_ms(&self) -> u64 {
        self.fs.now_ms()
    }
}

fn shifted<'a>(fs: &'a CutFs, under: &Path) -> Shifted<'a> {
    Shifted {
        fs,
        under: under.to_owned(),
    }
}

fn begin_refuses_untouched(fs: &CutFs, via: &dyn ImportFs, path: PathBuf, detail: &str) {
    let before = fs.snapshot();
    fs.reset_ops();
    match begin(via, net()) {
        Err(RegtestImportError::Member {
            path: p,
            detail: d,
        }) => assert_eq!((p, d), (path, detail)),
        other => panic!("{}: {other:?}", path.display()),
    }
    assert_eq!(fs.ops(), 0, "{:?}", fs.log());
    assert_eq!(fs.snapshot(), before);
}

#[test]
fn begin_refuses_a_member_on_another_filesystem() {
    for name in MEMBERS {
        let fs = fixture();
        begin_refuses_untouched(&fs, &shifted(&fs, &at(name)), at(name), "on another filesystem");
    }
}

#[test]
fn begin_refuses_a_member_on_another_mount() {
    let fs = CutFs::image()
        .with_file(at("chainstate/CURRENT"), b"MANIFEST-000002\n")
        .with_file("/volume/llmq/000001.ldb", b"llmq")
        .with_bind(at("llmq"), "/volume/llmq")
        .build();
    begin_refuses_untouched(&fs, &fs, at("llmq"), "on another mount");
}

#[test]
fn begin_refuses_a_blocks_that_is_not_a_plain_directory() {
    let fs = CutFs::image()
        .with_file(at("chainstate/CURRENT"), b"MANIFEST-000002\n")
        .with_file("/disk2/blocks/index/000004.ldb", b"block index")
        .with_file("/disk2/blocks/blk00000.dat", b"blk")
        .with_symlink(at("blocks"), "/disk2/blocks")
        .build();
    assert!(fs.metadata(&at("blocks/index")).unwrap().is_some_and(|m| is_dir(&m)));
    begin_refuses_untouched(&fs, &fs, at("blocks"), "not a plain directory");

    let fs = CutFs::image().with_dir(at("chainstate")).with_file(at("blocks"), b"file").build();
    begin_refuses_untouched(&fs, &fs, at("blocks"), "not a plain directory");
}

#[test]
fn a_network_directory_reached_through_a_symlink_imports() {
    let image = CutFs::image()
        .with_file("/disk2/regtest/chainstate/CURRENT", b"MANIFEST-000002\n")
        .with_file("/disk2/regtest/evodb/000001.ldb", b"evodb")
        .with_symlink(NET, "/disk2/regtest");
    let fs = image.build();
    begin(&fs, net()).unwrap().complete(&fs).unwrap();
    assert_eq!(names(&fs, Path::new("/disk2/regtest")), [BACKUP]);
    assert_eq!(names(&fs, &backup()), ["chainstate", JOURNAL, "evodb"]);
}

#[test]
fn recovery_ignores_a_renumbered_filesystem() {
    for moved in [&[][..], &["chainstate"], &["chainstate", "blocks/index", "evodb"]] {
        let fs = fixture();
        with_intent(&fs);
        fs.create_dir_all(&backup().join("blocks")).unwrap();
        for name in moved {
            fs.rename(&at(name), &backup().join(name)).unwrap();
        }
        let renumbered = shifted(&fs, Path::new("/"));
        let Recovered::Moved(m) = recover(&renumbered, net()).unwrap() else {
            panic!("not Moved: {moved:?}");
        };
        assert_eq!(m.state().members, state_for(&fixture(), Phase::Moved).members);
        m.complete(&renumbered).unwrap();
        assert_eq!(tree(fs.snapshot()), finished(fixture_image()), "{moved:?}");
    }
}

#[test]
fn resume_refuses_a_member_now_on_another_filesystem() {
    for name in ["chainstate", "evodb"] {
        let fs = fixture();
        with_intent(&fs);
        let before = fs.snapshot();
        fs.reset_ops();
        match recover(&shifted(&fs, &at(name)), net()) {
            Err(RegtestImportError::Member {
                path,
                detail,
            }) => assert_eq!((path, detail), (at(name), "changed since the import began")),
            other => panic!("{name}: {other:?}"),
        }
        assert_eq!(fs.ops(), 0, "{:?}", fs.log());
        assert_eq!(fs.snapshot(), before);
    }
}

#[test]
fn resume_refuses_a_recorded_inode_that_differs() {
    for (i, name) in MEMBERS.iter().enumerate() {
        let fs = fixture();
        let mut state = state_for(&fs, Phase::Intent);
        state.members[i].ino += 1000;
        fs.append(&at(JOURNAL), &state.encode()).unwrap();
        refuses_untouched(&fs, |e| {
            assert!(
                matches!(e, RegtestImportError::Member { ref path, detail } if *path == at(name) && detail.contains("changed")),
                "{name}: {e:?}"
            )
        });
    }
}

#[test]
fn a_stray_new_journal_that_is_a_directory_or_symlink_is_refused() {
    let not_a_file = |e: RegtestImportError| {
        assert!(
            matches!(e, RegtestImportError::Member { ref path, detail: "a directory or symlink" } if *path == at(JOURNAL_NEW)),
            "{e:?}"
        )
    };
    let dir = || fixture_image().with_file(at(JOURNAL_NEW).join("x"), b"x").build();
    let fs = dir();
    refuses_untouched(&fs, not_a_file);
    begin_refuses_untouched(&fs, &fs, at(JOURNAL_NEW), "a directory or symlink");

    let fs =
        fixture_image().with_dir("/elsewhere").with_symlink(at(JOURNAL_NEW), "/elsewhere").build();
    refuses_untouched(&fs, not_a_file);

    let fs = dir();
    fs.append(&at(JOURNAL), &state_for(&fs, Phase::Intent).encode()).unwrap();
    refuses_untouched(&fs, not_a_file);

    let fs = fixture();
    let moved = begin(&fs, net()).unwrap();
    fs.create_dir_all(&at(JOURNAL_NEW).join("x")).unwrap();
    not_a_file(moved.complete(&fs).unwrap_err());
    assert!(fs.metadata(&at(JOURNAL_NEW).join("x")).unwrap().is_some());
    assert_eq!(read_journal(&fs, net()).unwrap().unwrap().phase, Phase::Moved);
}
