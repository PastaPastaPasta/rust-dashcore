use super::cutfs::{CutFs, Faults, Snap};
use super::*;
use std::cell::Cell;
use std::collections::BTreeMap;

const BASE: &str = "/core/.dashcore";
const NET: &str = "/core/.dashcore/regtest";
const DEST: &str = "/dest/regtest";
const BEST: [u8; 32] = [0xbb; 32];

fn blk_bytes(n: u32) -> Vec<u8> {
    (0..96u32).map(|i| (n * 31 + i) as u8).collect()
}
fn blk_name(n: u32) -> String {
    format!("blk{n:05}.dat")
}
/// Writes a file, creating its parent dirs.
fn put(fs: &CutFs, path: impl AsRef<Path>, data: &[u8]) {
    let path = path.as_ref();
    fs.create_dir_all(path.parent().unwrap()).unwrap();
    fs.append(path, data).unwrap();
}
fn jpath() -> PathBuf {
    Path::new(DEST).join(STAGING).join(JOURNAL)
}
fn staged_dir() -> PathBuf {
    Path::new(DEST).join(STAGING).join("blocks")
}
fn dest_blocks() -> PathBuf {
    Path::new(DEST).join("blocks")
}
fn src_blk(n: u32) -> PathBuf {
    Path::new(NET).join("blocks").join(blk_name(n))
}

/// A Core source with three blk files, plus the scoped trees, the files the
/// manifest must ignore, and the destination's empty `blocks/`.
fn fixture_image() -> cutfs::Image {
    let mut image = CutFs::image();
    for n in 0..3 {
        image = image.with_file(src_blk(n), blk_bytes(n));
    }
    image
        .with_file(format!("{NET}/blocks/rev00000.dat"), b"rev")
        .with_file(format!("{NET}/blocks/index/CURRENT"), b"MANIFEST-000002\n")
        .with_file(format!("{NET}/chainstate/000003.ldb"), b"chainstate")
        .with_file(format!("{NET}/evodb/000001.ldb"), b"evodb")
        .with_file(format!("{NET}/.lock"), b"")
        .with_file(format!("{NET}/peers.dat"), b"peers")
        .with_file(format!("{NET}/wallets/wallet.dat"), b"wallet")
        .with_file(format!("{NET}/testnet3/blocks/blk00000.dat"), b"other net")
        .with_dir(dest_blocks())
}
fn fixture() -> CutFs {
    fixture_image().build()
}

#[derive(Clone)]
struct Cfg {
    options: ImportOptions,
    faults: Faults,
    l: Option<u32>,
}
impl Cfg {
    fn new(mode: SourceMode, placement: PlacementPolicy, reflink_errno: Option<i32>) -> Self {
        Self {
            options: ImportOptions {
                mode,
                placement,
            },
            faults: Faults {
                reflink_errno,
                ..Faults::default()
            },
            l: Some(2),
        }
    }
    fn reflink() -> Self {
        Self::new(SourceMode::Lock, PlacementPolicy::Reflink, None)
    }
    fn copy() -> Self {
        Self::new(SourceMode::Lock, PlacementPolicy::Reflink, Some(libc::EOPNOTSUPP))
    }
    fn hardlink() -> Self {
        Self::new(SourceMode::Lock, PlacementPolicy::Hardlink, Some(libc::EXDEV))
    }
    fn snapshot() -> Self {
        Self::new(SourceMode::Snapshot, PlacementPolicy::Reflink, Some(libc::EOPNOTSUPP))
    }
    fn hardlinks(&self) -> bool {
        self.options.placement == PlacementPolicy::Hardlink
    }
    fn methods(&self) -> Vec<FileMethod> {
        (0..3)
            .map(|n| {
                let errno = self.faults.reflink_errno.unwrap_or(0);
                let method = match self.faults.reflink_errno {
                    None => Method::Reflink,
                    Some(_) if self.hardlinks() && self.l.is_some_and(|l| n < l) => {
                        Method::Hardlink
                    }
                    Some(_) => Method::Copy,
                };
                FileMethod {
                    name: PathBuf::from(blk_name(n)),
                    method,
                    reflink_errno: errno,
                    link_errno: 0,
                }
            })
            .collect()
    }
}

fn inspection(l: Option<u32>) -> SourceInspection {
    SourceInspection::from_parts(
        PathBuf::from(NET),
        (0..3).map(|n| (n, src_blk(n))).collect(),
        Some(BEST),
        l,
    )
}

/// The `SourceGuard` double (O2-7): a release must find `SourceVerified` (or
/// later) in CutFs's durable image; in hardlink mode it must find `Reported`,
/// so any release by `stage` fails.
struct DurableGuard {
    fs: CutFs,
    hardlink: bool,
    releases: Cell<usize>,
}
impl DurableGuard {
    fn new(fs: &CutFs, cfg: &Cfg) -> Self {
        Self {
            fs: fs.clone(),
            hardlink: cfg.hardlinks(),
            releases: Cell::new(0),
        }
    }
}
impl SourceGuard for DurableGuard {
    fn release(&self) -> Result<(), ImportError> {
        let durable = Journal::open(&jpath(), &self.fs.durable()).expect("durable journal decodes");
        if self.hardlink {
            assert_eq!(durable.last(), Some(Phase::Reported), "hardlink-mode lock released early");
        } else if durable.last().is_none() {
            // Only the O3-2 reset releases with no record: the durable
            // journal is exactly the magic and staging holds nothing else.
            let d = self.fs.durable();
            assert_eq!(d.read(&jpath()).ok().as_deref(), Some(journal::MAGIC), "no durable reset");
            assert_eq!(names(&d, &Path::new(DEST).join(STAGING)), [JOURNAL], "staging not cleared");
        } else {
            assert!(
                durable.last() >= Some(Phase::SourceVerified),
                "lock released before SourceVerified was durable: {:?}",
                durable.last()
            );
        }
        self.releases.set(self.releases.get() + 1);
        Ok(())
    }
}

fn run_with(fs: &CutFs, cfg: &Cfg, guard: &DurableGuard) -> Result<Phase, ImportError> {
    preflight_destination(Path::new(DEST), Path::new(NET), fs)?;
    stage(
        &inspection(cfg.l),
        cfg.options,
        Path::new(BASE),
        Some("regtest"),
        Path::new(DEST),
        fs,
        guard,
    )
}
fn run(fs: &CutFs, cfg: &Cfg) -> Result<Phase, ImportError> {
    run_with(fs, cfg, &DurableGuard::new(fs, cfg))
}
fn fixture_for(cfg: &Cfg) -> CutFs {
    let fs = fixture();
    fs.set_faults(cfg.faults.clone());
    fs
}

/// Path → (bytes, inode) of every file under `root`, in the durable image.
fn files_under(fs: &CutFs, root: &str) -> BTreeMap<PathBuf, (Vec<u8>, u64)> {
    fs.durable_snapshot()
        .into_iter()
        .filter(|(p, _)| p.starts_with(root))
        .filter_map(|(p, s)| match s {
            Snap::File {
                ino,
                data,
            } => Some((p, (data, ino))),
            _ => None,
        })
        .collect()
}
/// Path → bytes of everything under the destination, in the durable image.
fn dest_state(fs: &CutFs) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fs.durable_snapshot()
        .into_iter()
        .filter(|(p, _)| p.starts_with(DEST))
        .map(|(p, s)| match s {
            Snap::File {
                data,
                ..
            } => (p, Some(data)),
            _ => (p, None),
        })
        .collect()
}
fn durable_journal(fs: &CutFs) -> Journal {
    Journal::open(&jpath(), &fs.durable()).unwrap()
}
fn names(fs: &CutFs, dir: &Path) -> Vec<String> {
    fs.read_dir(dir)
        .unwrap_or_default()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}
fn assert_journal_corrupt(result: Result<Phase, ImportError>, from_placed: bool) {
    match result {
        Err(ImportError::JournalCorrupt(text)) => {
            assert!(text.contains(&jpath().display().to_string()), "{text}");
            assert_eq!(text.contains("holds only this unfinished import"), from_placed, "{text}");
        }
        other => panic!("expected JournalCorrupt, got {other:?}"),
    }
}

/// A durable image at `last`, with the staged files in staging, in
/// `dest/blocks`, in both, or in neither.
#[derive(Clone, Copy, PartialEq)]
enum Where {
    Staging,
    Dest,
    Both,
    Neither,
}
fn image_at(cfg: &Cfg, last: Phase, at: Where) -> CutFs {
    let fs = fixture_for(cfg);
    let m0 = SourceManifest::scan(Path::new(NET), &fs).unwrap();
    let binding = source_binding(Path::new(NET), &fs).unwrap();
    let methods = cfg.methods();
    let put = |dir: &Path| {
        fs.create_dir_all(dir).unwrap();
        for m in &methods {
            let src = Path::new(NET).join("blocks").join(&m.name);
            match m.method {
                Method::Hardlink => fs.hard_link(&src, &dir.join(&m.name)).unwrap(),
                _ => {
                    fs.copy(&src, &dir.join(&m.name)).unwrap();
                }
            }
        }
    };
    if last >= Phase::Staged {
        match at {
            Where::Staging => put(&staged_dir()),
            Where::Dest => put(&dest_blocks()),
            Where::Both => {
                put(&staged_dir());
                fs.append(&dest_blocks().join("stray"), b"x").unwrap();
            }
            Where::Neither => {}
        }
    }
    let m1 = SourceManifest::scan(Path::new(NET), &fs).unwrap();
    let mut bytes = journal::MAGIC.to_vec();
    let bodies = [
        RecordBody::Intent(Box::new(Intent {
            source: binding,
            datadir: PathBuf::from(BASE),
            mode: cfg.options.mode,
            placement: cfg.options.placement,
            best_block: Some(BEST),
            last_file: cfg.l,
            m0,
        })),
        RecordBody::Staged(methods.clone()),
        RecordBody::SourceVerified {
            m1_digest: m1.digest,
        },
        RecordBody::Placed,
        RecordBody::ReindexArmed,
        RecordBody::Reported(ReportedStatus::Complete),
    ];
    for body in bodies.into_iter().take(last as usize + 1) {
        bytes.extend_from_slice(&frame(
            &Record {
                at_ms: 0,
                body,
            }
            .encode(),
        ));
    }
    fs.create_dir_all(&Path::new(DEST).join(STAGING)).unwrap();
    fs.append(&jpath(), &bytes).unwrap();
    fs.crash_keep();
    fs
}

/// Cuts `stage` right after the first call whose log line satisfies `pick`,
/// counted from a fresh run.
fn cut_where(
    fs: &CutFs,
    cfg: &Cfg,
    pick: impl Fn(&[String]) -> bool,
) -> Result<Phase, ImportError> {
    let probe = fs.fork();
    probe.reset_ops();
    run(&probe, cfg).unwrap();
    let mut k = 0;
    for n in 1..=probe.ops() {
        let trial = fs.fork();
        trial.cut_after(n);
        let _ = run(&trial, cfg);
        if pick(&trial.log()) {
            k = n;
            break;
        }
    }
    assert!(k > 0, "no cut matched");
    fs.cut_after(k);
    run(fs, cfg)
}
/// The crash variants every cut matrix runs: `crash`, `crash_keep`,
/// `crash_torn_head` and eight seeds of `crash_mixed`.
const VARIANTS: u64 = 11;
fn crash_variant(fs: &CutFs, variant: u64) {
    match variant {
        0 => fs.crash(),
        1 => fs.crash_keep(),
        2 => fs.crash_torn_head(),
        s => fs.crash_mixed(s - 3),
    }
}

fn journal_synced_at(fs: &CutFs, phase: Phase) -> bool {
    durable_journal(fs).last() == Some(phase)
}
/// Cuts right after the journal record for `phase` became durable.
fn cut_after_phase(fs: &CutFs, cfg: &Cfg, phase: Phase) {
    let probe = fs.fork();
    probe.reset_ops();
    run(&probe, cfg).unwrap();
    for n in 1..=probe.ops() {
        let trial = fs.fork();
        trial.cut_after(n);
        let _ = run(&trial, cfg);
        if journal_synced_at(&trial, phase) {
            fs.cut_after(n);
            let _ = run(fs, cfg);
            return;
        }
    }
    panic!("{phase:?} never became durable");
}

// ---- P0-P5 ----

#[test]
fn intent_record_binds_source_and_m0_durably() {
    let cfg = Cfg::reflink();
    let fs = fixture_for(&cfg);
    let m0 = SourceManifest::scan(Path::new(NET), &fs).unwrap();
    let root = fs.metadata(Path::new(NET)).unwrap().unwrap();
    cut_after_phase(&fs, &cfg, Phase::Intent);
    fs.crash();
    let j = durable_journal(&fs);
    assert_eq!(j.phases(), [Phase::Intent]);
    let intent = j.intent().unwrap();
    assert_eq!(intent.source.path, Path::new(NET));
    assert_eq!((intent.source.dev, intent.source.ino), root.id());
    assert_eq!((intent.mode, intent.placement), (SourceMode::Lock, PlacementPolicy::Reflink));
    assert_eq!((intent.best_block, intent.last_file), (Some(BEST), Some(2)));
    assert_eq!(intent.m0, m0);
}

#[test]
fn staged_data_survives_crash_after_staged() {
    for cfg in [Cfg::reflink(), Cfg::copy()] {
        let fs = fixture_for(&cfg);
        cut_after_phase(&fs, &cfg, Phase::Staged);
        fs.crash();
        assert_eq!(durable_journal(&fs).last(), Some(Phase::Staged));
        for n in 0..3 {
            assert_eq!(fs.read(&staged_dir().join(blk_name(n))).unwrap(), blk_bytes(n));
        }
        assert_eq!(durable_journal(&fs).methods().unwrap(), cfg.methods());
        assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed);
    }
}

#[test]
fn short_copy_refuses_without_staged_record() {
    let mut cfg = Cfg::copy();
    cfg.faults.short_copy = true;
    let fs = fixture_for(&cfg);
    let result = run(&fs, &cfg);
    assert!(
        matches!(&result, Err(ImportError::StagedCopyMismatch { path }) if path == Path::new("blocks/blk00000.dat")),
        "{result:?}"
    );
    fs.crash();
    assert_eq!(durable_journal(&fs).phases(), [Phase::Intent]);
}

#[test]
fn source_verified_is_durable_before_lock_release() {
    for cfg in [Cfg::reflink(), Cfg::copy(), Cfg::snapshot()] {
        let fs = fixture_for(&cfg);
        let guard = DurableGuard::new(&fs, &cfg);
        assert_eq!(run_with(&fs, &cfg, &guard).unwrap(), Phase::Placed);
        assert_eq!(guard.releases.get(), 1);
    }
}

#[test]
fn hardlink_mode_never_releases_the_lock() {
    let cfg = Cfg::hardlink();
    let fs = fixture_for(&cfg);
    let guard = DurableGuard::new(&fs, &cfg);
    assert_eq!(run_with(&fs, &cfg, &guard).unwrap(), Phase::Placed);
    assert_eq!(guard.releases.get(), 0);
}

#[test]
fn placed_survives_crash_right_after_append() {
    for cfg in [Cfg::reflink(), Cfg::hardlink()] {
        let fs = fixture_for(&cfg);
        cut_after_phase(&fs, &cfg, Phase::Placed);
        fs.crash();
        assert_eq!(durable_journal(&fs).last(), Some(Phase::Placed));
        assert_eq!(names(&fs, &dest_blocks()), (0..3).map(blk_name).collect::<Vec<_>>());
        assert!(fs.metadata(&staged_dir()).unwrap().is_none());
        assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed);
    }
}

#[test]
fn placed_append_follows_both_parent_syncs() {
    let cfg = Cfg::reflink();
    let fs = fixture_for(&cfg);
    fs.reset_ops();
    run(&fs, &cfg).unwrap();
    let log = fs.log();
    let pos = |line: &str| {
        log.iter().rposition(|l| l == line).unwrap_or_else(|| panic!("{line} missing"))
    };
    let rename = pos(&format!("rename {}", staged_dir().display()));
    let staging_sync = pos(&format!("sync_dir {DEST}/{STAGING}"));
    let dest_sync = pos(&format!("sync_dir {DEST}"));
    let placed = pos(&format!("sync_data {}", jpath().display()));
    assert!(rename < staging_sync && rename < dest_sync, "{log:#?}");
    assert!(staging_sync < placed && dest_sync < placed, "{log:#?}");
    assert!(!log[rename..].iter().any(|l| l == &format!("sync_dir {}", dest_blocks().display())));
}

struct Arm(Result<(), ReindexError>);
impl ReindexArm for Arm {
    fn request_reindex(&self) -> Result<(), ReindexError> {
        self.0.clone()
    }
}

#[test]
fn arm_reindex_appends_only_after_ok() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::Placed, Where::Dest);
    let mut j = Journal::open(&jpath(), &fs).unwrap();
    assert_eq!(arm_reindex(&mut j, &Arm(Ok(())), &fs).unwrap(), Phase::ReindexArmed);
    fs.crash();
    assert_eq!(durable_journal(&fs).last(), Some(Phase::ReindexArmed));
    let mut j = Journal::open(&jpath(), &fs).unwrap();
    assert!(matches!(arm_reindex(&mut j, &Arm(Ok(())), &fs), Err(ImportError::PhaseOrder { .. })));
}

#[test]
fn arm_reindex_stops_on_not_empty_or_wipe() {
    let cfg = Cfg::reflink();
    for (answer, want) in [
        (ReindexError::NotEmpty, "not empty"),
        (ReindexError::WipeInProgress, "wipe"),
        (ReindexError::Other("io".into()), "failed"),
    ] {
        let fs = image_at(&cfg, Phase::Placed, Where::Dest);
        let before = fs.read(&jpath()).unwrap();
        fs.reset_ops();
        let mut j = Journal::open(&jpath(), &fs).unwrap();
        let e = arm_reindex(&mut j, &Arm(Err(answer)), &fs).unwrap_err();
        assert!(e.to_string().contains(want), "{e}");
        assert_eq!(fs.ops(), 0);
        assert_eq!(fs.read(&jpath()).unwrap(), before);
        assert_eq!(j.last(), Some(Phase::Placed));
    }
}

// ---- P5 ----

fn outcome(relation: TipRelation) -> StartOutcome {
    StartOutcome {
        abc_ok: true,
        shutdown: false,
        abort: false,
        relation,
        destination_tip: Some([0xdd; 32]),
    }
}
fn report_on(
    cfg: &Cfg,
    out: StartOutcome,
) -> (CutFs, Result<ImportReport, ImportError>, serde_json::Value) {
    let fs = image_at(cfg, Phase::ReindexArmed, Where::Dest);
    let mut j = Journal::open(&jpath(), &fs).unwrap();
    let result = report(&mut j, out, &fs);
    fs.crash();
    let json =
        serde_json::from_slice(&fs.read(&Path::new(DEST).join("import-report.json")).unwrap())
            .unwrap();
    (fs, result, json)
}
fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn assert_report_fields(json: &serde_json::Value, cfg: &Cfg, fs: &CutFs) {
    let j = durable_journal(fs);
    let intent = j.intent().unwrap();
    assert_eq!(json["source"], NET);
    assert_eq!(json["source_hex"], hex_of(NET.as_bytes()));
    assert_eq!(json["datadir"], BASE);
    assert_eq!(json["mode"], cfg.options.mode.as_str());
    assert_eq!(json["placement"], cfg.options.placement.as_str());
    assert_eq!(json["file_count"], 3);
    assert_eq!(json["bytes"], 3 * 96);
    for (row, m) in json["files"].as_array().unwrap().iter().zip(cfg.methods()) {
        assert_eq!(row["file"], m.name.to_str().unwrap());
        assert_eq!(row["method"], m.method.as_str());
    }
    assert_eq!(json["source_best_block"], "bb".repeat(32));
    assert_eq!(json["source_last_file"], 2);
    assert_eq!(json["destination_tip"], "dd".repeat(32));
    let hex = |d: &[u8; 32]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(json["manifest_m0"], hex(&intent.m0.digest));
    assert_eq!(json["manifest_m1"], hex(&j.m1_digest().unwrap()));
    assert_eq!(json["guarantee"], guarantee(cfg.options.mode, cfg.options.placement));
    assert_eq!(json["not_imported"].as_array().unwrap().len(), NOT_IMPORTED.len());
    assert!(json["phase_ms"].as_object().unwrap().contains_key("ReindexArmed"));
}

#[test]
fn report_equal_appends_complete() {
    for cfg in [Cfg::reflink(), Cfg::hardlink(), Cfg::snapshot()] {
        let (fs, result, json) = report_on(&cfg, outcome(TipRelation::Equal));
        assert_eq!(result.unwrap().status, "complete");
        assert_eq!(json["status"], "complete");
        assert_eq!(json["relation"], "equal");
        assert_report_fields(&json, &cfg, &fs);
        assert_eq!(durable_journal(&fs).reported(), Some(ReportedStatus::Complete));
    }
}

#[test]
fn report_descendant_appends_complete() {
    let cfg = Cfg::copy();
    let (fs, result, json) = report_on(&cfg, outcome(TipRelation::Descendant));
    assert_eq!(result.unwrap().status, "complete");
    assert_eq!(
        (json["status"].as_str(), json["relation"].as_str()),
        (Some("complete"), Some("descendant"))
    );
    assert_report_fields(&json, &cfg, &fs);
    assert_eq!(durable_journal(&fs).last(), Some(Phase::Reported));
}

#[test]
fn report_mismatch_is_terminal_with_error_line() {
    let cfg = Cfg::reflink();
    let (fs, result, json) = report_on(&cfg, outcome(TipRelation::Mismatch));
    let e = result.unwrap_err();
    assert!(
        matches!(&e, ImportError::TipMismatch { report } if report.ends_with("import-report.json"))
    );
    assert!(e.to_string().contains("did not reproduce the source's tip"));
    assert_eq!(json["status"], "mismatch");
    assert_report_fields(&json, &cfg, &fs);
    assert_eq!(durable_journal(&fs).reported(), Some(ReportedStatus::Mismatch));
}

fn assert_incomplete(out: StartOutcome, reason: &str) {
    let cfg = Cfg::reflink();
    let (fs, result, json) = report_on(&cfg, out);
    let r = result.unwrap();
    assert_eq!(r.status, "incomplete");
    assert_eq!(json["status"], "incomplete");
    assert!(json["reason"].as_str().unwrap().contains(reason), "{json}");
    assert_report_fields(&json, &cfg, &fs);
    assert_eq!(durable_journal(&fs).last(), Some(Phase::ReindexArmed));
}

#[test]
fn report_ancestor_is_incomplete() {
    assert_incomplete(outcome(TipRelation::Ancestor), "ancestor");
}

#[test]
fn report_shutdown_is_incomplete() {
    // A shutdown takes precedence over a mismatch.
    assert_incomplete(
        StartOutcome {
            shutdown: true,
            ..outcome(TipRelation::Mismatch)
        },
        "shutdown",
    );
}

#[test]
fn report_abort_is_incomplete() {
    assert_incomplete(
        StartOutcome {
            abort: true,
            abc_ok: false,
            ..outcome(TipRelation::Mismatch)
        },
        "abort",
    );
    assert_incomplete(
        StartOutcome {
            abc_ok: false,
            ..outcome(TipRelation::Equal)
        },
        "ActivateBestChain",
    );
}

#[test]
fn report_requires_reindex_armed() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::Placed, Where::Dest);
    let mut j = Journal::open(&jpath(), &fs).unwrap();
    fs.reset_ops();
    assert!(matches!(
        report(&mut j, outcome(TipRelation::Equal), &fs),
        Err(ImportError::PhaseOrder { .. })
    ));
    assert_eq!(fs.ops(), 0);
}

// ---- recovery rows ----

#[test]
fn resume_intent_clears_staging_and_restarts() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::Intent, Where::Neither);
    fs.create_dir_all(&staged_dir()).unwrap();
    fs.append(&staged_dir().join(blk_name(0)), b"partial").unwrap();
    put(&fs, Path::new(DEST).join(STAGING).join("leftover"), b"x");
    fs.crash_keep();
    assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed);
    fs.crash();
    assert_eq!(names(&fs, &Path::new(DEST).join(STAGING)), [JOURNAL]);
    assert_eq!(fs.read(&dest_blocks().join(blk_name(0))).unwrap(), blk_bytes(0));
    assert_eq!(durable_journal(&fs).phases().len(), 4);
}

#[test]
fn hardlink_crash_before_staged_leaves_source_intact() {
    let cfg = Cfg::hardlink();
    for crash in [CutFs::crash as fn(&CutFs), CutFs::crash_keep] {
        let fs = fixture_for(&cfg);
        let before = files_under(&fs, NET);
        let _ = cut_where(&fs, &cfg, |log| log.iter().any(|l| l.starts_with("hard_link")));
        crash(&fs);
        assert!(durable_journal(&fs).last() == Some(Phase::Intent));
        assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed);
        fs.crash();
        assert_eq!(files_under(&fs, NET), before);
        for n in 0..3 {
            assert_eq!(fs.read(&src_blk(n)).unwrap(), blk_bytes(n));
        }
    }
}

/// R1's order in `begin_intent`: staging is clear and synced before the
/// journal loses `Intent`, so a durable journal with no records never sits
/// beside staged links to the source.
#[test]
fn begin_intent_clears_staged_links_before_reset_cut_matrix() {
    let cfg = Cfg::hardlink();
    let base = image_at(&cfg, Phase::Intent, Where::Neither);
    let nlinks = |fs: &CutFs| -> Vec<u64> {
        (0..3).map(|n| fs.metadata(&src_blk(n)).unwrap().unwrap().nlink).collect()
    };
    let original = nlinks(&base);
    base.create_dir_all(&staged_dir()).unwrap();
    for n in 0..2 {
        base.hard_link(&src_blk(n), &staged_dir().join(blk_name(n))).unwrap();
    }
    base.crash_keep();
    assert_ne!(nlinks(&base), original);
    let reappended = |log: &[String]| {
        let journal = jpath().display().to_string();
        let append = log.iter().position(|l| *l == format!("append {journal}"));
        append.is_some_and(|i| log[i..].contains(&format!("sync_data {journal}")))
    };
    for crash in [CutFs::crash as fn(&CutFs), CutFs::crash_keep] {
        let mut no_records = 0;
        for k in 1.. {
            let fs = base.fork();
            fs.cut_after(k);
            let _ = run(&fs, &cfg);
            let done = reappended(&fs.log());
            crash(&fs);
            if durable_journal(&fs).last().is_none() {
                no_records += 1;
                assert_eq!(names(&fs, &Path::new(DEST).join(STAGING)), [JOURNAL], "k={k}");
                assert_eq!(nlinks(&fs), original, "k={k}");
            }
            assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed, "k={k}");
            if done {
                break;
            }
        }
        assert!(no_records > 0, "no cut left a journal without records");
    }
}

#[test]
fn resume_staged_redoes_p2() {
    for cfg in [Cfg::reflink(), Cfg::hardlink()] {
        let fs = image_at(&cfg, Phase::Staged, Where::Staging);
        let guard = DurableGuard::new(&fs, &cfg);
        assert_eq!(run_with(&fs, &cfg, &guard).unwrap(), Phase::Placed);
        assert_eq!(guard.releases.get(), usize::from(!cfg.hardlinks()));
        fs.crash();
        assert_eq!(durable_journal(&fs).last(), Some(Phase::Placed));
    }
}

fn assert_reset(fs: &CutFs) {
    fs.crash();
    assert_eq!(fs.read(&jpath()).unwrap(), journal::MAGIC);
    assert_eq!(names(fs, &Path::new(DEST).join(STAGING)), [JOURNAL]);
}

#[test]
fn resume_staged_rehashes_staged_files() {
    let cfg = Cfg::copy();
    let edits: [fn(&CutFs); 3] = [
        |fs| fs.remove(&staged_dir().join(blk_name(1))).unwrap(),
        |fs| fs.append(&staged_dir().join("blk00009.dat"), b"extra").unwrap(),
        |fs| fs.append(&staged_dir().join(blk_name(2)), b"!").unwrap(),
    ];
    for edit in edits {
        let fs = image_at(&cfg, Phase::Staged, Where::Staging);
        edit(&fs);
        fs.crash_keep();
        let source = files_under(&fs, NET);
        assert!(matches!(run(&fs, &cfg), Err(ImportError::StagingChanged(_))));
        assert_reset(&fs);
        assert_eq!(files_under(&fs, NET), source);
        assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed);
    }
}

#[test]
fn resume_staged_source_changed_resets() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::Staged, Where::Staging);
    put(&fs, Path::new(NET).join("chainstate/000003.ldb"), b"!");
    fs.crash_keep();
    let guard = DurableGuard::new(&fs, &cfg);
    let result = run_with(&fs, &cfg, &guard);
    assert!(
        matches!(&result, Err(ImportError::SourceChanged(p)) if p == &[PathBuf::from("chainstate/000003.ldb")]),
        "{result:?}"
    );
    assert_eq!(guard.releases.get(), 1);
    assert_reset(&fs);
    assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed);
}

#[test]
fn source_changed_reset_cut_matrix() {
    let cfg = Cfg::copy();
    let base = image_at(&cfg, Phase::Staged, Where::Staging);
    let mut changed = blk_bytes(1);
    changed[0] ^= 0xff;
    base.truncate(&src_blk(1), 0).unwrap();
    base.append(&src_blk(1), &changed).unwrap();
    base.crash_keep();
    let probe = base.fork();
    assert!(matches!(run(&probe, &cfg), Err(ImportError::SourceChanged(_))));
    let n = probe.ops();
    assert!(n >= 4, "the reset makes at least four writes");
    for k in 1..=n {
        for variant in 0..VARIANTS {
            let fs = base.fork();
            fs.cut_after(k);
            let _ = run(&fs, &cfg);
            crash_variant(&fs, variant);
            let j = durable_journal(&fs);
            if j.last() == Some(Phase::Staged) {
                assert_eq!(
                    names(&fs, &staged_dir()).len(),
                    3,
                    "a clear was visible before the reset (k={k})"
                );
            }
            fs.create_dir_all(&dest_blocks()).unwrap();
            let mut result = run(&fs, &cfg);
            if matches!(result, Err(ImportError::SourceChanged(_))) {
                result = run(&fs, &cfg);
            }
            assert_eq!(result.unwrap(), Phase::Placed, "k={k} variant={variant}");
            fs.crash();
            assert_eq!(fs.read(&dest_blocks().join(blk_name(1))).unwrap(), changed);
        }
    }
}

#[test]
fn staged_without_staging_blocks_is_corrupt() {
    let cfg = Cfg::reflink();
    for empty_dir in [false, true] {
        let fs = image_at(&cfg, Phase::Staged, Where::Neither);
        if empty_dir {
            fs.create_dir_all(&staged_dir()).unwrap();
            fs.crash_keep();
        }
        let before = fs.durable_snapshot();
        fs.reset_ops();
        assert_journal_corrupt(run(&fs, &cfg), false);
        assert_eq!(fs.ops(), 0);
        assert_eq!(fs.durable_snapshot(), before);
    }
}

#[test]
fn resume_source_verified_places_blocks() {
    let cfg = Cfg::copy();
    let fs = image_at(&cfg, Phase::SourceVerified, Where::Staging);
    assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed);
    fs.crash();
    assert_eq!(names(&fs, &dest_blocks()), (0..3).map(blk_name).collect::<Vec<_>>());
    assert!(fs.metadata(&staged_dir()).unwrap().is_none());
}

#[test]
fn resume_source_verified_after_landed_rename_appends_placed() {
    let cfg = Cfg::copy();
    let fs = image_at(&cfg, Phase::SourceVerified, Where::Dest);
    fs.reset_ops();
    assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed);
    let log = fs.log();
    assert!(!log.iter().any(|l| l.starts_with("rename")));
    assert!(log.contains(&format!("sync_dir {DEST}/{STAGING}")));
    assert!(log.contains(&format!("sync_dir {DEST}")));
    fs.crash();
    assert_eq!(durable_journal(&fs).last(), Some(Phase::Placed));
}

fn assert_untouched_corrupt(fs: &CutFs, cfg: &Cfg, from_placed: bool) {
    let before = fs.durable_snapshot();
    fs.reset_ops();
    assert_journal_corrupt(run(fs, cfg), from_placed);
    assert_eq!(fs.ops(), 0, "{:?}", fs.log());
    assert_eq!(fs.durable_snapshot(), before);
}

#[test]
fn corrupt_source_verified_both_present() {
    // Also in hardlink mode with a changed linked file: the row wins over
    // the recheck's reset.
    for (cfg, changed) in [(Cfg::copy(), false), (Cfg::hardlink(), true)] {
        let fs = image_at(&cfg, Phase::SourceVerified, Where::Both);
        if changed {
            mutate_linked(&fs);
        }
        assert_untouched_corrupt(&fs, &cfg, false);
    }
}

#[test]
fn corrupt_source_verified_neither_present() {
    // Also in hardlink mode with a changed linked file: the row wins over
    // the recheck's reset.
    for (cfg, changed) in [(Cfg::copy(), false), (Cfg::hardlink(), true)] {
        let fs = image_at(&cfg, Phase::SourceVerified, Where::Neither);
        if changed {
            mutate_linked(&fs);
        }
        assert_untouched_corrupt(&fs, &cfg, false);
    }
}

#[test]
fn resume_placed_returns_placed_then_arms() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::Placed, Where::Dest);
    let j = preflight_destination(Path::new(DEST), Path::new(NET), &fs).unwrap().unwrap();
    assert_eq!(j.last(), Some(Phase::Placed));
    assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed);
    let mut j = Journal::open(&jpath(), &fs).unwrap();
    assert_eq!(arm_reindex(&mut j, &Arm(Ok(())), &fs).unwrap(), Phase::ReindexArmed);
}

#[test]
fn corrupt_placed_missing_blocks() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::Placed, Where::Neither);
    fs.remove(&dest_blocks()).unwrap();
    fs.crash_keep();
    assert_untouched_corrupt(&fs, &cfg, true);
}

#[test]
fn corrupt_placed_empty_blocks() {
    let cfg = Cfg::reflink();
    assert_untouched_corrupt(&image_at(&cfg, Phase::Placed, Where::Neither), &cfg, true);
}

#[test]
fn resume_reindex_armed_runs_report() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::ReindexArmed, Where::Dest);
    put(&fs, Path::new(DEST).join("store/CURRENT"), b"store");
    fs.crash_keep();
    assert_eq!(run(&fs, &cfg).unwrap(), Phase::ReindexArmed);
    let mut j = Journal::open(&jpath(), &fs).unwrap();
    assert_eq!(report(&mut j, outcome(TipRelation::Equal), &fs).unwrap().status, "complete");
    assert!(matches!(run(&fs, &cfg), Err(ImportError::AlreadyImported { .. })));
}

/// A [`StageSource`] that counts its inspections, and panics on one when it
/// has none to give.
struct Counted {
    inspection: Option<SourceInspection>,
    calls: Cell<usize>,
}
impl StageSource for Counted {
    fn net_dir(&self) -> &Path {
        Path::new(NET)
    }
    fn inspection(&self) -> Result<&SourceInspection, ImportError> {
        self.calls.set(self.calls.get() + 1);
        Ok(self.inspection.as_ref().expect("no inspection on a resume past Intent"))
    }
}

/// `stage` inspects the source once on a start at P0 (no journal, or one at
/// `Intent`), and never on a resume from any later row below `Reported`.
#[test]
fn stage_inspects_the_source_only_when_it_begins_at_p0() {
    for cfg in [Cfg::reflink(), Cfg::copy(), Cfg::hardlink(), Cfg::snapshot()] {
        for at in [Some(Phase::Intent), None] {
            let fs = if at == Some(Phase::Intent) {
                image_at(&cfg, Phase::Intent, Where::Neither)
            } else {
                fixture_for(&cfg)
            };
            let src = Counted {
                inspection: Some(inspection(cfg.l)),
                calls: Cell::new(0),
            };
            preflight_destination(Path::new(DEST), Path::new(NET), &fs).unwrap();
            let guard = DurableGuard::new(&fs, &cfg);
            let phase = stage(
                &src,
                cfg.options,
                Path::new(BASE),
                Some("regtest"),
                Path::new(DEST),
                &fs,
                &guard,
            );
            assert_eq!(phase.unwrap(), Phase::Placed);
            assert_eq!(src.calls.get(), 1, "{at:?}");
        }
        let rows = [
            (Phase::Staged, Where::Staging, Phase::Placed),
            (Phase::SourceVerified, Where::Staging, Phase::Placed),
            (Phase::SourceVerified, Where::Dest, Phase::Placed),
            (Phase::Placed, Where::Dest, Phase::Placed),
            (Phase::ReindexArmed, Where::Dest, Phase::ReindexArmed),
        ];
        for (last, at, end) in rows {
            let fs = image_at(&cfg, last, at);
            let src = Counted {
                inspection: None,
                calls: Cell::new(0),
            };
            let guard = DurableGuard::new(&fs, &cfg);
            let phase = stage(
                &src,
                cfg.options,
                Path::new(BASE),
                Some("regtest"),
                Path::new(DEST),
                &fs,
                &guard,
            );
            assert_eq!(phase.unwrap(), end, "{last:?}");
            assert_eq!(src.calls.get(), 0, "{last:?}");
        }
    }
}

#[test]
fn resume_reported_is_already_imported() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::Reported, Where::Dest);
    fs.reset_ops();
    match run(&fs, &cfg) {
        Err(ImportError::AlreadyImported {
            source_dir,
            status,
        }) => {
            assert_eq!(source_dir, Path::new(BASE));
            assert_eq!(status, "complete");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fs.ops(), 0);
}

#[test]
fn resume_reported_mismatch_repeats_error() {
    let cfg = Cfg::reflink();
    let (fs, result, _) = report_on(&cfg, outcome(TipRelation::Mismatch));
    assert!(result.is_err());
    let e = run(&fs, &cfg).unwrap_err();
    assert!(matches!(e, ImportError::TipMismatch { .. }), "{e}");
    let mut j = Journal::open(&jpath(), &fs).unwrap();
    assert!(matches!(
        report(&mut j, outcome(TipRelation::Equal), &fs),
        Err(ImportError::TipMismatch { .. })
    ));
}

#[test]
fn resume_reported_other_source_refused() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::Reported, Where::Dest);
    for n in 0..3 {
        put(&fs, Path::new("/other/regtest/blocks").join(blk_name(n)), &blk_bytes(n));
    }
    fs.crash_keep();
    let other = SourceInspection::from_parts(
        PathBuf::from("/other/regtest"),
        (0..3).map(|n| (n, Path::new("/other/regtest/blocks").join(blk_name(n)))).collect(),
        Some(BEST),
        Some(2),
    );
    assert!(matches!(
        preflight_destination(Path::new(DEST), Path::new("/other/regtest"), &fs),
        Err(ImportError::DestinationNotEmpty(_))
    ));
    let guard = DurableGuard::new(&fs, &cfg);
    assert!(matches!(
        stage(
            &other,
            cfg.options,
            Path::new("/other"),
            Some("regtest"),
            Path::new(DEST),
            &fs,
            &guard
        ),
        Err(ImportError::DestinationNotEmpty(_))
    ));
}

#[test]
fn reported_trailing_corruption_is_ignored() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::Reported, Where::Dest);
    fs.append(&jpath(), &[0xff, 0x00, 0x13, 0x37, 0x42, 9, 9, 9, 9, 9, 9, 9]).unwrap();
    fs.crash_keep();
    let before = fs.read(&jpath()).unwrap();
    let j = Journal::open(&jpath(), &fs).unwrap();
    assert!(j.ignored_tail());
    assert_eq!(j.last(), Some(Phase::Reported));
    assert!(matches!(run(&fs, &cfg), Err(ImportError::AlreadyImported { .. })));
    assert_eq!(fs.read(&jpath()).unwrap(), before);
}

fn mutate_linked(fs: &CutFs) {
    fs.append(&src_blk(0), b"appended by a source -reindex").unwrap();
    fs.crash_keep();
}

#[test]
fn hardlink_resume_source_verified_change_resets() {
    let cfg = Cfg::hardlink();
    for at in [Where::Staging, Where::Dest] {
        let fs = image_at(&cfg, Phase::SourceVerified, at);
        mutate_linked(&fs);
        let guard = DurableGuard::new(&fs, &cfg);
        let result = run_with(&fs, &cfg, &guard);
        assert!(
            matches!(&result, Err(ImportError::SourceChanged(p)) if p == &[PathBuf::from("blocks/blk00000.dat")]),
            "{result:?}"
        );
        assert_eq!(guard.releases.get(), 0);
        assert_reset(&fs);
        assert!(names(&fs, &dest_blocks()).is_empty());
        assert_eq!(fs.metadata(&src_blk(0)).unwrap().unwrap().nlink, 1);
    }
}

#[test]
fn hardlink_resume_placed_change_refuses() {
    let cfg = Cfg::hardlink();
    let fs = image_at(&cfg, Phase::Placed, Where::Dest);
    mutate_linked(&fs);
    assert_untouched_corrupt(&fs, &cfg, true);
}

#[test]
fn hardlink_resume_rechecks_every_phase_past_source_verified() {
    let cfg = Cfg::hardlink();
    for phase in [Phase::Placed, Phase::ReindexArmed] {
        let fs = image_at(&cfg, phase, Where::Dest);
        fs.reset_ops();
        assert_eq!(run(&fs, &cfg).unwrap(), phase);
        assert!(fs.log().iter().any(|l| l == &format!("read {}", src_blk(0).display())));
        mutate_linked(&fs);
        assert_untouched_corrupt(&fs, &cfg, true);
    }
}

#[test]
fn reported_hardlink_destination_starts_after_a_linked_file_changes() {
    let cfg = Cfg::hardlink();
    let fs = image_at(&cfg, Phase::Reported, Where::Dest);
    mutate_linked(&fs);
    let before = fs.durable_snapshot();
    fs.reset_ops();
    match run(&fs, &cfg) {
        Err(ImportError::AlreadyImported {
            source_dir,
            status,
        }) => assert_eq!((source_dir.as_path(), status.as_str()), (Path::new(BASE), "complete")),
        other => panic!("{other:?}"),
    }
    assert!(!fs.log().iter().any(|l| l == &format!("read {}", src_blk(0).display())));
    assert_eq!(fs.ops(), 0);
    let j = Journal::open(&jpath(), &fs).unwrap();
    assert_eq!(import_start(&j, None, None, &fs).unwrap(), Some(Phase::Reported));
    fs.crash();
    assert_eq!(fs.durable_snapshot(), before);
}

#[test]
fn resume_reported_with_other_options_is_already_imported() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::Reported, Where::Dest);
    for options in [
        ImportOptions {
            mode: SourceMode::Snapshot,
            placement: PlacementPolicy::Reflink,
        },
        ImportOptions {
            mode: SourceMode::Lock,
            placement: PlacementPolicy::Hardlink,
        },
    ] {
        fs.reset_ops();
        let other = Cfg {
            options,
            ..cfg.clone()
        };
        match run(&fs, &other) {
            Err(
                e @ ImportError::AlreadyImported {
                    ..
                },
            ) => {
                assert_eq!(
                    e.to_string(),
                    format!("already imported from {BASE} (status complete)")
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fs.ops(), 0);
    }
}

#[test]
fn resume_options_must_match_journal() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::Staged, Where::Staging);
    for options in [
        ImportOptions {
            mode: SourceMode::Snapshot,
            placement: PlacementPolicy::Reflink,
        },
        ImportOptions {
            mode: SourceMode::Lock,
            placement: PlacementPolicy::Hardlink,
        },
    ] {
        fs.reset_ops();
        let other = Cfg {
            options,
            ..cfg.clone()
        };
        let e = run(&fs, &other).unwrap_err();
        assert!(e.to_string().contains("-importsource=lock -importplacement=reflink"), "{e}");
        assert_eq!(fs.ops(), 0);
    }
}

#[test]
fn import_start_requires_matching_source_until_reindex_armed() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::Placed, Where::Dest);
    put(&fs, Path::new("/other/regtest/blocks/x"), b"");
    let j = Journal::open(&jpath(), &fs).unwrap();
    // The network dir is not what -importcoredatadir takes.
    for requested in [None, Some(Path::new("/other")), Some(Path::new(NET))] {
        assert!(matches!(
            import_start(&j, requested, Some("regtest"), &fs),
            Err(ImportError::ImportInProgress(p)) if p == Path::new(BASE)
        ));
    }
    let e = import_start(&j, None, Some("regtest"), &fs).unwrap_err().to_string();
    assert!(e.contains(&format!("restart with -importcoredatadir={BASE}")), "{e}");
    assert_eq!(
        import_start(&j, Some(Path::new("/core/.dashcore/")), Some("regtest"), &fs).unwrap(),
        Some(Phase::Placed)
    );
    for phase in [Phase::ReindexArmed, Phase::Reported] {
        let fs = image_at(&cfg, phase, Where::Dest);
        let j = Journal::open(&jpath(), &fs).unwrap();
        assert_eq!(import_start(&j, None, Some("regtest"), &fs).unwrap(), Some(phase));
    }
}

struct Released;
impl SourceGuard for Released {
    fn release(&self) -> Result<(), ImportError> {
        Ok(())
    }
}

#[test]
fn operator_texts_name_the_base_datadir() {
    for subdir in ["testnet3", "regtest"] {
        let net = Path::new(BASE).join(subdir);
        let dest = Path::new("/dest").join(subdir);
        let mut image = CutFs::image();
        for n in 0..3 {
            image = image.with_file(net.join("blocks").join(blk_name(n)), blk_bytes(n));
        }
        let fs = image
            .with_file(net.join("chainstate/CURRENT"), b"x")
            .with_dir(dest.join("blocks"))
            .with_dir("/elsewhere")
            .build();
        let src = SourceInspection::from_parts(
            net.clone(),
            (0..3).map(|n| (n, net.join("blocks").join(blk_name(n)))).collect(),
            Some(BEST),
            Some(2),
        );
        let options = Cfg::reflink().options;
        assert!(matches!(
            stage(&src, options, Path::new("/elsewhere"), Some(subdir), &dest, &fs, &Released),
            Err(ImportError::InvalidOptions(_))
        ));
        preflight_destination(&dest, &net, &fs).unwrap();
        assert_eq!(
            stage(&src, options, Path::new(BASE), Some(subdir), &dest, &fs, &Released).unwrap(),
            Phase::Placed
        );
        let jp = dest.join(STAGING).join(JOURNAL);
        let mut j = Journal::open(&jp, &fs).unwrap();
        let e = import_start(&j, None, Some(subdir), &fs).unwrap_err();
        assert!(matches!(&e, ImportError::ImportInProgress(p) if p == Path::new(BASE)), "{e}");
        assert!(e.to_string().ends_with(&format!("restart with -importcoredatadir={BASE}")), "{e}");
        // Following the text resumes; the network dir does not.
        assert_eq!(
            import_start(&j, Some(Path::new(BASE)), Some(subdir), &fs).unwrap(),
            Some(Phase::Placed)
        );
        assert!(import_start(&j, Some(&net), Some(subdir), &fs).is_err());
        arm_reindex(&mut j, &Arm(Ok(())), &fs).unwrap();
        report(&mut j, outcome(TipRelation::Equal), &fs).unwrap();
        let e =
            stage(&src, options, Path::new(BASE), Some(subdir), &dest, &fs, &Released).unwrap_err();
        assert!(
            matches!(&e, ImportError::AlreadyImported { source_dir, .. } if source_dir == Path::new(BASE))
        );
        assert_eq!(e.to_string(), format!("already imported from {BASE} (status complete)"));
    }
}

// ---- the crash matrix ----

fn cut_matrix(cfg: Cfg) {
    let base = fixture_for(&cfg);
    let source = files_under(&base, NET);
    let uncut = base.fork();
    assert_eq!(run(&uncut, &cfg).unwrap(), Phase::Placed);
    let n = uncut.ops();
    uncut.crash();
    let want = dest_state(&uncut);
    assert_eq!(durable_journal(&uncut).methods().unwrap(), cfg.methods());
    for k in 1..=n {
        for variant in 0..VARIANTS {
            let fs = base.fork();
            fs.cut_after(k);
            let _ = run(&fs, &cfg);
            assert!(fs.is_dead(), "k={k}");
            crash_variant(&fs, variant);
            assert_eq!(files_under(&fs, NET), source, "source changed at k={k} variant={variant}");
            fs.create_dir_all(&dest_blocks()).unwrap();
            let result = run(&fs, &cfg);
            assert_eq!(result.unwrap(), Phase::Placed, "k={k} variant={variant}");
            fs.crash();
            assert_eq!(dest_state(&fs), want, "k={k} variant={variant}");
            assert_eq!(files_under(&fs, NET), source, "k={k} variant={variant}");
            let m0 = durable_journal(&fs).intent().unwrap().m0.clone();
            let later = SourceManifest::scan(Path::new(NET), &fs).unwrap();
            assert_eq!(m0.check_live(&later, &dest_blocks(), &fs), Ok(()));
        }
    }
}

#[test]
fn cut_matrix_reflink() {
    cut_matrix(Cfg::reflink());
}
#[test]
fn cut_matrix_copy() {
    cut_matrix(Cfg::copy());
}
#[test]
fn cut_matrix_hardlink() {
    cut_matrix(Cfg::hardlink());
}
#[test]
fn cut_matrix_snapshot() {
    cut_matrix(Cfg::snapshot());
}

// ---- journal decoder ----

fn journal_image(bytes: &[u8]) -> CutFs {
    let fs = fixture();
    fs.create_dir_all(&Path::new(DEST).join(STAGING)).unwrap();
    fs.append(&jpath(), bytes).unwrap();
    fs.crash_keep();
    fs
}
fn full_journal(cfg: &Cfg) -> Vec<u8> {
    image_at(cfg, Phase::Reported, Where::Dest).read(&jpath()).unwrap()
}
/// The byte offset where each record starts, and the end.
fn offsets(bytes: &[u8]) -> Vec<usize> {
    let mut out = vec![journal::MAGIC.len()];
    let mut p = journal::MAGIC.len();
    while p < bytes.len() {
        p += 12 + u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap()) as usize;
        out.push(p);
    }
    out
}

#[test]
fn journal_torn_tail_repaired_only_on_resume() {
    let cfg = Cfg::copy();
    let full = full_journal(&cfg);
    let at = offsets(&full);
    for cut in [at[1] + 3, at[1] + 9, at[2] - 1] {
        let torn = &full[..cut];
        let fs = image_at(&cfg, Phase::Intent, Where::Neither);
        fs.truncate(&jpath(), 0).unwrap();
        fs.append(&jpath(), torn).unwrap();
        fs.crash_keep();
        fs.reset_ops();
        let j = Journal::open(&jpath(), &fs).unwrap();
        assert_eq!((j.last(), j.needs_repair()), (Some(Phase::Intent), true));
        preflight_destination(Path::new(DEST), Path::new(NET), &fs).unwrap();
        assert_eq!(fs.ops(), 0);
        assert_eq!(fs.read(&jpath()).unwrap(), torn);
        assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed);
    }
    let mut zero_tail = full[..at[3]].to_vec();
    zero_tail.extend_from_slice(&[0; 9]);
    let j = Journal::decode(&jpath(), &zero_tail).unwrap();
    assert_eq!((j.last(), j.needs_repair()), (Some(Phase::SourceVerified), true));
    let fs = journal_image(&zero_tail);
    let mut j = Journal::open(&jpath(), &fs).unwrap();
    j.repair(&fs).unwrap();
    assert_eq!(fs.read(&jpath()).unwrap(), &full[..at[3]]);
}

#[test]
fn journal_bad_record_mid_file_is_corrupt() {
    let full = full_journal(&Cfg::copy());
    let at = offsets(&full);
    // A flipped payload byte in Staged, a length that overruns into the next
    // record, and a zero-length frame, each followed by more records.
    let mut payload = full.clone();
    payload[at[1] + 20] ^= 1;
    let mut length = full.clone();
    length[at[1]] ^= 0x04;
    let mut zero = full[..at[1]].to_vec();
    zero.extend_from_slice(&[0, 0, 0, 0, 1, 2, 3, 4]);
    zero.extend_from_slice(&full[at[1]..]);
    for bytes in [payload, length, zero] {
        let fs = journal_image(&bytes);
        fs.reset_ops();
        let e = Journal::open(&jpath(), &fs).unwrap_err();
        assert!(
            matches!(&e, ImportError::JournalCorrupt(t) if t.contains(&jpath().display().to_string())),
            "{e}"
        );
        assert_eq!(fs.ops(), 0);
        assert_eq!(fs.read(&jpath()).unwrap(), bytes);
    }
}

#[test]
fn journal_fixed_length_mismatch_is_corrupt() {
    let full = full_journal(&Cfg::copy());
    let at = offsets(&full);
    let mut bytes = full[..at[3]].to_vec();
    let mut placed = Record {
        at_ms: 0,
        body: RecordBody::Placed,
    }
    .encode();
    placed.push(0);
    bytes.extend_from_slice(&frame(&placed));
    assert!(matches!(Journal::decode(&jpath(), &bytes), Err(ImportError::JournalCorrupt(_))));
}

#[test]
fn journal_duplicate_or_skipped_phase_is_corrupt() {
    let full = full_journal(&Cfg::copy());
    let at = offsets(&full);
    let rec = |i: usize| full[at[i]..at[i + 1]].to_vec();
    let magic = journal::MAGIC.to_vec();
    for order in [vec![0, 0], vec![0, 2], vec![0, 1, 1], vec![1], vec![0, 1, 2, 3, 5]] {
        let mut bytes = magic.clone();
        for i in &order {
            bytes.extend_from_slice(&rec(*i));
        }
        assert!(
            matches!(Journal::decode(&jpath(), &bytes), Err(ImportError::JournalCorrupt(_))),
            "{order:?}"
        );
    }
    let fs = journal_image(&full[..at[1]]);
    let mut j = Journal::open(&jpath(), &fs).unwrap();
    for body in [
        RecordBody::SourceVerified {
            m1_digest: [0; 32],
        },
        RecordBody::Placed,
    ] {
        assert!(matches!(
            j.append(Record::new(body, &fs), &fs),
            Err(ImportError::PhaseOrder { .. })
        ));
    }
    assert_eq!(fs.read(&jpath()).unwrap(), &full[..at[1]]);
}

#[test]
fn journal_magic_prefix_or_zeros_is_empty() {
    for bytes in [&b""[..], b"CH6", b"CH6AJ", &[0u8; 6], &[0u8; 40]] {
        let fs = journal_image(bytes);
        let mut j = Journal::open(&jpath(), &fs).unwrap();
        assert!(j.records().is_empty() && j.needs_repair(), "{bytes:?}");
        j.repair(&fs).unwrap();
        assert_eq!(fs.read(&jpath()).unwrap(), journal::MAGIC);
    }
    assert!(matches!(Journal::decode(&jpath(), b"XH6AJ1"), Err(ImportError::JournalCorrupt(_))));
}

#[test]
fn journal_corrupt_text_by_phase() {
    let full = full_journal(&Cfg::copy());
    let at = offsets(&full);
    for (records, from_placed) in [(1, false), (2, false), (3, false), (4, true), (5, true)] {
        let mut bytes = full[..at[records]].to_vec();
        bytes.extend_from_slice(&[7; 12]);
        bytes.extend_from_slice(&full[at[records]..at[records + 1]]);
        match Journal::decode(&jpath(), &bytes) {
            Err(ImportError::JournalCorrupt(t)) => {
                assert_eq!(t.contains("holds only this unfinished import"), from_placed, "{t}");
                assert!(t.contains(&format!("{DEST}/{STAGING}")), "{t}");
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn journal_records_after_each_phase_byte_for_byte() {
    for cfg in [Cfg::reflink(), Cfg::hardlink()] {
        let fs = fixture_for(&cfg);
        let m0 = SourceManifest::scan(Path::new(NET), &fs).unwrap();
        let binding = source_binding(Path::new(NET), &fs).unwrap();
        assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed);
        let mut j = Journal::open(&jpath(), &fs).unwrap();
        arm_reindex(&mut j, &Arm(Ok(())), &fs).unwrap();
        report(&mut j, outcome(TipRelation::Equal), &fs).unwrap();
        fs.crash();
        let m1 = SourceManifest::scan(Path::new(NET), &fs).unwrap();
        let bodies = vec![
            RecordBody::Intent(Box::new(Intent {
                source: binding.clone(),
                datadir: PathBuf::from(BASE),
                mode: cfg.options.mode,
                placement: cfg.options.placement,
                best_block: Some(BEST),
                last_file: Some(2),
                m0: m0.clone(),
            })),
            RecordBody::Staged(cfg.methods()),
            RecordBody::SourceVerified {
                m1_digest: m1.digest,
            },
            RecordBody::Placed,
            RecordBody::ReindexArmed,
            RecordBody::Reported(ReportedStatus::Complete),
        ];
        let mut want = journal::MAGIC.to_vec();
        let mut prefixes = vec![want.clone()];
        for body in bodies {
            want.extend_from_slice(&frame(
                &Record {
                    at_ms: 0,
                    body,
                }
                .encode(),
            ));
            prefixes.push(want.clone());
        }
        assert_eq!(fs.read(&jpath()).unwrap(), want);
        // The durable journal after each phase is exactly that prefix.
        for (i, phase) in [Phase::Intent, Phase::Staged, Phase::SourceVerified, Phase::Placed]
            .into_iter()
            .enumerate()
        {
            let cut = fixture_for(&cfg);
            cut_after_phase(&cut, &cfg, phase);
            cut.crash();
            assert_eq!(cut.read(&jpath()).unwrap(), prefixes[i + 1], "{phase:?}");
        }
    }
}

// ---- manifests ----

#[test]
fn check_placement_accepts_link_delta_only() {
    let cfg = Cfg::hardlink();
    let fs = fixture_for(&cfg);
    let m0 = SourceManifest::scan(Path::new(NET), &fs).unwrap();
    fs.create_dir_all(&staged_dir()).unwrap();
    fs.hard_link(&src_blk(0), &staged_dir().join(blk_name(0))).unwrap();
    let m1 = SourceManifest::scan(Path::new(NET), &fs).unwrap();
    let linked: BTreeSet<PathBuf> = [PathBuf::from("blocks/blk00000.dat")].into();
    assert_eq!(m0.check_placement(&m1, &linked), Ok(()));
    assert_eq!(
        m0.check_placement(&m1, &BTreeSet::new()),
        Err(vec![PathBuf::from("blocks/blk00000.dat")])
    );
    let both: BTreeSet<PathBuf> =
        [PathBuf::from("blocks/blk00000.dat"), PathBuf::from("blocks/blk00001.dat")].into();
    assert_eq!(m0.check_placement(&m1, &both), Err(vec![PathBuf::from("blocks/blk00001.dat")]));
}

#[test]
fn check_live_after_detach() {
    let fs = fixture();
    let m0 = SourceManifest::scan(Path::new(NET), &fs).unwrap();
    fs.create_dir_all(&dest_blocks()).unwrap();
    fs.hard_link(&src_blk(0), &dest_blocks().join(blk_name(0))).unwrap();
    let linked = SourceManifest::scan(Path::new(NET), &fs).unwrap();
    assert_eq!(linked.get(Path::new("blocks/blk00000.dat")).unwrap().nlink, 2);
    assert_eq!(m0.check_live(&linked, &dest_blocks(), &fs), Ok(()));
    // The link-count rule's private copy: copy + rename over the link.
    let private = dest_blocks().join("blk00000.dat.unlink");
    fs.copy(&dest_blocks().join(blk_name(0)), &private).unwrap();
    fs.rename(&private, &dest_blocks().join(blk_name(0))).unwrap();
    let detached = SourceManifest::scan(Path::new(NET), &fs).unwrap();
    assert_eq!(detached.get(Path::new("blocks/blk00000.dat")).unwrap().nlink, 1);
    assert_eq!(fs.read(&src_blk(0)).unwrap(), blk_bytes(0));
    assert_eq!(m0.check_live(&detached, &dest_blocks(), &fs), Ok(()));
    let linked_set: BTreeSet<PathBuf> = [PathBuf::from("blocks/blk00000.dat")].into();
    assert!(m0.check_placement(&detached, &linked_set).is_err());
}

#[test]
fn manifest_refuses_scoped_changes() {
    let edits: [fn(&CutFs); 4] = [
        |fs| {
            let mut b = blk_bytes(1);
            b[3] ^= 1;
            fs.truncate(&src_blk(1), 0).unwrap();
            fs.append(&src_blk(1), &b).unwrap();
        },
        |fs| fs.truncate(&src_blk(2), 10).unwrap(),
        |fs| put(fs, Path::new(NET).join("evodb/000001.ldb"), b""),
        |fs| put(fs, Path::new(NET).join("chainstate/000004.ldb"), b"new"),
    ];
    for edit in edits {
        let fs = fixture();
        let m0 = SourceManifest::scan(Path::new(NET), &fs).unwrap();
        edit(&fs);
        let m1 = SourceManifest::scan(Path::new(NET), &fs).unwrap();
        assert!(m0.check_placement(&m1, &BTreeSet::new()).is_err());
        assert!(m0.check_live(&m1, &dest_blocks(), &fs).is_err());
    }
}

#[test]
fn manifest_ignores_out_of_scope() {
    let fs = fixture();
    let m0 = SourceManifest::scan(Path::new(NET), &fs).unwrap();
    assert!(
        m0.entries
            .iter()
            .all(|e| !e.path.starts_with("wallets") && !e.path.starts_with("testnet3"))
    );
    assert!(m0.get(Path::new("peers.dat")).is_some());
    put(&fs, Path::new(NET).join("wallets/wallet.dat"), b"tx");
    put(&fs, Path::new(NET).join("testnet3/blocks/blk00000.dat"), b"more");
    put(&fs, Path::new(NET).join("backups/b.dat"), b"b");
    assert_eq!(SourceManifest::scan(Path::new(NET), &fs).unwrap(), m0);
}

#[test]
fn manifest_never_opens_lock() {
    let fs = fixture();
    fs.reset_ops();
    let m0 = SourceManifest::scan(Path::new(NET), &fs).unwrap();
    let lock = m0.get(Path::new(".lock")).unwrap();
    assert_eq!(lock.hash, [0; 32]);
    assert!(!fs.log().iter().any(|l| l.ends_with("/.lock")), "{:?}", fs.log());
}

#[test]
fn manifest_refuses_symlinked_scoped_dir() {
    let fs = fixture_image()
        .with_dir("/elsewhere/chainstate")
        .with_symlink(format!("{NET}/llmq"), "/elsewhere/chainstate")
        .build();
    let e = SourceManifest::scan(Path::new(NET), &fs).unwrap_err();
    assert!(e.to_string().contains("symlink"), "{e}");
    let fs = fixture_image().with_symlink(format!("{NET}/blocks/blk00009.dat"), src_blk(0)).build();
    assert!(SourceManifest::scan(Path::new(NET), &fs).is_err());
}

#[test]
fn manifest_decoder_bounds_and_digest() {
    let m0 = SourceManifest::scan(Path::new(NET), &fixture()).unwrap();
    let bytes = m0.encode();
    assert_eq!(SourceManifest::decode(&bytes), Some(m0));
    let mut bad = bytes.clone();
    bad[10] ^= 1;
    assert_eq!(SourceManifest::decode(&bad), None);
    assert_eq!(SourceManifest::decode(&[0xff, 0xff, 0xff, 0x7f]), None);
    assert_eq!(SourceManifest::decode(&bytes[..bytes.len() - 1]), None);
}

// ---- placement ----

fn methods_after(cfg: &Cfg) -> (CutFs, Vec<FileMethod>) {
    let fs = fixture_for(cfg);
    fs.reset_ops();
    assert_eq!(run(&fs, cfg).unwrap(), Phase::Placed);
    let methods = durable_journal(&fs).methods().unwrap().to_vec();
    (fs, methods)
}

#[test]
fn forced_reflink_failure_copies_everything_by_default() {
    let (fs, methods) = methods_after(&Cfg::copy());
    assert!(
        methods.iter().all(|m| m.method == Method::Copy && m.reflink_errno == libc::EOPNOTSUPP)
    );
    assert!(!fs.log().iter().any(|l| l.starts_with("hard_link")));
}

#[test]
fn hardlink_policy_links_below_l_only() {
    let cfg = Cfg::hardlink();
    let (fs, methods) = methods_after(&cfg);
    let got: Vec<Method> = methods.iter().map(|m| m.method).collect();
    assert_eq!(got, [Method::Hardlink, Method::Hardlink, Method::Copy]);
    assert!(methods.iter().all(|m| m.reflink_errno == libc::EXDEV));
    let nlinks: Vec<u64> =
        (0..3).map(|n| fs.metadata(&src_blk(n)).unwrap().unwrap().nlink).collect();
    assert_eq!(nlinks, [2, 2, 1]);
    let m0 = durable_journal(&fs).intent().unwrap().m0.clone();
    assert!(m0.blk_entries().all(|e| e.nlink == 1));
    for n in 0..2 {
        let src = fs.metadata(&src_blk(n)).unwrap().unwrap();
        let dst = fs.metadata(&dest_blocks().join(blk_name(n))).unwrap().unwrap();
        assert_eq!(src.id(), dst.id());
    }
    assert_ne!(
        fs.metadata(&src_blk(2)).unwrap().unwrap().id(),
        fs.metadata(&dest_blocks().join(blk_name(2))).unwrap().unwrap().id()
    );
}

#[test]
fn hardlink_without_l_copies_all() {
    let cfg = Cfg {
        l: None,
        ..Cfg::hardlink()
    };
    let (fs, methods) = methods_after(&cfg);
    assert!(methods.iter().all(|m| m.method == Method::Copy));
    assert!(!fs.log().iter().any(|l| l.starts_with("hard_link")));
}

#[test]
fn link_failure_falls_back_to_copy() {
    for errno in [libc::EPERM, libc::EXDEV, libc::EMLINK] {
        let mut cfg = Cfg::hardlink();
        cfg.faults.link_errno = Some(errno);
        let (fs, methods) = methods_after(&cfg);
        assert!(methods.iter().all(|m| m.method == Method::Copy));
        assert_eq!(methods.iter().map(|m| m.link_errno).collect::<Vec<_>>(), [errno, errno, 0]);
        assert!((0..3).all(|n| fs.metadata(&src_blk(n)).unwrap().unwrap().nlink == 1));
    }
}

#[test]
fn snapshot_never_calls_hard_link() {
    let (fs, methods) = methods_after(&Cfg::snapshot());
    assert!(methods.iter().all(|m| m.method == Method::Copy));
    assert!(!fs.log().iter().any(|l| l.starts_with("hard_link")));
}

#[test]
fn options_validated_before_any_write() {
    let cfg = Cfg::new(SourceMode::Snapshot, PlacementPolicy::Hardlink, Some(libc::EXDEV));
    let fs = fixture_for(&cfg);
    fs.reset_ops();
    let e = stage(
        &inspection(Some(2)),
        cfg.options,
        Path::new(BASE),
        Some("regtest"),
        Path::new(DEST),
        &fs,
        &DurableGuard::new(&fs, &cfg),
    )
    .unwrap_err();
    assert!(matches!(e, ImportError::InvalidOptions(_)));
    assert_eq!(fs.ops(), 0);
    assert!(fs.log().is_empty());
}

#[test]
fn free_space_bound_uses_copy_default() {
    let sizes = [(0, 1000u64), (1, 1000), (2, 1000)];
    let min = dashd_store::flatfile::MIN_DISK_SPACE;
    let headroom = 3000 / 100 * 12;
    for cfg in [Cfg::reflink(), Cfg::copy(), Cfg::snapshot()] {
        assert_eq!(required_space(&sizes, cfg.options, Some(2)), 3000 + headroom + min);
    }
    assert_eq!(required_space(&sizes, Cfg::hardlink().options, Some(2)), 1000 + headroom + min);
    assert_eq!(required_space(&sizes, Cfg::hardlink().options, None), 3000 + headroom + min);
}

#[test]
fn insufficient_space_is_refused() {
    let cfg = Cfg::copy();
    let fs = fixture_for(&cfg);
    fs.set_available_space(1000);
    assert!(matches!(
        run(&fs, &cfg),
        Err(ImportError::InsufficientSpace {
            available: 1000,
            ..
        })
    ));
    assert!(fs.metadata(&staged_dir()).unwrap().is_none());
}

#[test]
fn place_refuses_staged_path_aliasing_source() {
    let cfg = Cfg::copy();
    let fs = fixture_for(&cfg);
    let m0 = SourceManifest::scan(Path::new(NET), &fs).unwrap();
    fs.create_dir_all(&staged_dir()).unwrap();
    fs.hard_link(&src_blk(0), &staged_dir().join(blk_name(0))).unwrap();
    let files: Vec<(u32, PathBuf)> = (0..3).map(|n| (n, src_blk(n))).collect();
    let e = place(&files, Some(2), cfg.options, &m0, Path::new(DEST), &fs).unwrap_err();
    assert!(matches!(e, ImportError::StagingAliasesSource(_)), "{e}");
    assert_eq!(fs.read(&src_blk(0)).unwrap(), blk_bytes(0));
}

#[test]
fn stage_never_writes_to_a_source_inode() {
    // Intent durable, then a crash left staging holding links to the source
    // under the staged names: resume must unlink them, not write through them.
    let cfg = Cfg::copy();
    let fs = image_at(&cfg, Phase::Intent, Where::Neither);
    fs.create_dir_all(&staged_dir()).unwrap();
    for n in 0..3 {
        fs.hard_link(&src_blk(n), &staged_dir().join(blk_name(n))).unwrap();
    }
    fs.crash_keep();
    let source = files_under(&fs, NET);
    fs.reset_ops();
    assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed);
    let writes: Vec<String> = fs
        .log()
        .into_iter()
        .filter(|l| l.starts_with("append") || l.starts_with("truncate") || l.starts_with("copy"))
        .collect();
    assert!(writes.iter().all(|l| !l.contains(NET)), "{writes:?}");
    fs.crash();
    let after = files_under(&fs, NET);
    assert_eq!(
        after.iter().map(|(p, (d, _))| (p, d)).collect::<Vec<_>>(),
        source.iter().map(|(p, (d, _))| (p, d)).collect::<Vec<_>>()
    );
}

// ---- preflight and exclusion ----

#[test]
fn preflight_refuses_non_empty_destination() {
    let cases: [fn(&CutFs); 4] = [
        |fs| fs.append(&dest_blocks().join("blk00000.dat"), b"x").unwrap(),
        |fs| put(fs, Path::new(DEST).join("store/CURRENT"), b"x"),
        |fs| put(fs, Path::new(DEST).join(STAGING).join("partial"), b"x"),
        |fs| put(fs, Path::new(DEST).join("chainstate/CURRENT"), b"x"),
    ];
    for case in cases {
        let fs = fixture();
        case(&fs);
        assert!(matches!(
            preflight_destination(Path::new(DEST), Path::new(NET), &fs),
            Err(ImportError::DestinationNotEmpty(_))
        ));
    }
    let other = image_at(&Cfg::reflink(), Phase::Intent, Where::Neither);
    put(&other, Path::new("/other/regtest/blocks/blk00000.dat"), b"x");
    assert!(matches!(
        preflight_destination(Path::new(DEST), Path::new("/other/regtest"), &other),
        Err(ImportError::DestinationNotEmpty(p)) if p.ends_with(STAGING)
    ));
}

#[test]
fn preflight_allows_missing_or_empty_blocks() {
    let fs = fixture();
    assert!(preflight_destination(Path::new(DEST), Path::new(NET), &fs).unwrap().is_none());
    fs.remove(&dest_blocks()).unwrap();
    assert!(preflight_destination(Path::new(DEST), Path::new(NET), &fs).unwrap().is_none());
}

#[test]
fn preflight_resumes_each_phase_of_same_source() {
    let cfg = Cfg::reflink();
    for (phase, at) in [
        (Phase::Intent, Where::Neither),
        (Phase::Staged, Where::Staging),
        (Phase::SourceVerified, Where::Staging),
        (Phase::SourceVerified, Where::Dest),
        (Phase::Placed, Where::Dest),
        (Phase::ReindexArmed, Where::Dest),
        (Phase::Reported, Where::Dest),
    ] {
        let fs = image_at(&cfg, phase, at);
        if phase >= Phase::Placed {
            put(&fs, Path::new(DEST).join("store/CURRENT"), b"x");
        }
        fs.reset_ops();
        let j = preflight_destination(Path::new(DEST), Path::new(NET), &fs).unwrap().unwrap();
        assert_eq!(j.last(), Some(phase));
        assert_eq!(fs.ops(), 0);
    }
}

#[test]
fn exclusion_refuses_nesting_both_ways() {
    let fs = fixture();
    for (s, d) in
        [(NET, NET), (NET, "/core/.dashcore/regtest/dest"), ("/core/.dashcore/regtest", "/core")]
    {
        assert!(
            matches!(
                check_exclusion(Path::new(s), Path::new(d), &fs),
                Err(ImportError::Overlap { .. })
            ),
            "{s} {d}"
        );
    }
    assert!(check_exclusion(Path::new(NET), Path::new(DEST), &fs).is_ok());
    assert!(check_exclusion(Path::new(NET), Path::new("/core/.dashcore/regtest2"), &fs).is_ok());
}

#[test]
fn exclusion_refuses_symlink_nesting() {
    let fs = fixture_image().with_symlink("/link", NET).with_symlink("/up", "/core").build();
    assert!(check_exclusion(Path::new(NET), Path::new("/link/dest"), &fs).is_err());
    assert!(
        check_exclusion(
            Path::new("/up/.dashcore/regtest"),
            Path::new("/core/.dashcore/regtest/x"),
            &fs
        )
        .is_err()
    );
    assert!(matches!(
        preflight_destination(Path::new("/link/dest"), Path::new(NET), &fs),
        Err(ImportError::Overlap { .. })
    ));
}

#[test]
fn exclusion_refuses_bind_alias() {
    let fs = fixture_image().with_bind("/mnt/alias", "/core/.dashcore").build();
    for d in ["/mnt/alias/regtest/dest", "/mnt/alias"] {
        assert!(check_exclusion(Path::new(NET), Path::new(d), &fs).is_err(), "{d}");
    }
    for target in ["/core/.dashcore/regtest/sub", "/core/.dashcore/regtest/blocks/sub"] {
        let fs = fixture_image().with_bind("/mnt/dest", target).build();
        assert!(check_exclusion(Path::new(NET), Path::new("/mnt/dest"), &fs).is_err(), "{target}");
        assert!(check_exclusion(Path::new("/mnt/dest"), Path::new(NET), &fs).is_err(), "{target}");
    }
}

#[test]
fn blocks_dir_elsewhere_is_refused() {
    let fs = CutFs::image()
        .with_file(format!("{NET}/blocks/index/CURRENT"), b"x")
        .with_file(format!("{NET}/chainstate/CURRENT"), b"x")
        .build();
    assert!(matches!(
        preflight_destination(Path::new(DEST), Path::new(NET), &fs),
        Err(ImportError::BlocksDirElsewhere)
    ));
}

#[test]
fn net_subdir_source_is_refused() {
    let fs = fixture();
    assert_eq!(source_net_dir(Path::new(BASE), Some("regtest"), &fs).unwrap(), Path::new(NET));
    assert!(matches!(
        source_net_dir(Path::new(NET), Some("regtest"), &fs),
        Err(ImportError::NetSubdir(_))
    ));
    let e = source_net_dir(Path::new(NET), Some("regtest"), &fs).unwrap_err().to_string();
    assert!(e.contains("pass the base -datadir"), "{e}");
}

#[test]
fn same_source_spelled_differently_resumes() {
    let cfg = Cfg::reflink();
    for spelling in [
        "/core/.dashcore/regtest/",
        "/core/./.dashcore/regtest",
        "/core/.dashcore/../.dashcore/regtest",
        "core/.dashcore/regtest",
        "/alias/regtest",
    ] {
        let fs = image_at(&cfg, Phase::Staged, Where::Staging);
        fs.add_symlink(Path::new("/alias"), Path::new("/core/.dashcore"));
        let j = preflight_destination(Path::new(DEST), Path::new(spelling), &fs).unwrap().unwrap();
        assert_eq!(j.last(), Some(Phase::Staged), "{spelling}");
        let mut src = inspection(Some(2));
        src.net_dir = PathBuf::from(spelling);
        let guard = DurableGuard::new(&fs, &cfg);
        assert_eq!(
            stage(
                &src,
                cfg.options,
                Path::new(BASE),
                Some("regtest"),
                Path::new(DEST),
                &fs,
                &guard
            )
            .unwrap(),
            Phase::Placed,
            "{spelling}"
        );
    }
}

#[test]
fn replaced_source_dir_is_refused() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::Staged, Where::Staging);
    fs.rename(Path::new(NET), Path::new("/core/.dashcore/regtest.old")).unwrap();
    fs.create_dir_all(&Path::new(NET).join("blocks")).unwrap();
    for n in 0..3 {
        fs.append(&src_blk(n), &blk_bytes(n)).unwrap();
    }
    fs.crash_keep();
    assert!(matches!(
        preflight_destination(Path::new(DEST), Path::new(NET), &fs),
        Err(ImportError::DestinationNotEmpty(_))
    ));
    assert!(matches!(
        stage(
            &inspection(Some(2)),
            cfg.options,
            Path::new(BASE),
            Some("regtest"),
            Path::new(DEST),
            &fs,
            &DurableGuard::new(&fs, &cfg)
        ),
        Err(ImportError::DestinationNotEmpty(_))
    ));
}

#[test]
fn exclusion_refuses_shared_subtree() {
    let fs =
        fixture_image().with_bind(format!("{DEST}/{STAGING}"), format!("{NET}/blocks")).build();
    assert!(matches!(
        check_exclusion(Path::new(NET), Path::new(DEST), &fs),
        Err(ImportError::Overlap { .. })
    ));
    assert!(matches!(
        preflight_destination(Path::new(DEST), Path::new(NET), &fs),
        Err(ImportError::Overlap { .. })
    ));
}

#[test]
fn exclusion_refuses_bind_alias_through_scoped_dirs() {
    // The walk covers both whole trees, so a bind through any dir, scoped or
    // not, is refused both ways.
    for target in
        ["chainstate/deep", "blocks/index", "evodb", "llmq/x/y", "indexes/txindex", "sub", "x/y"]
    {
        let fs = fixture_image().with_bind("/mnt/dest", format!("{NET}/{target}")).build();
        for (source, dest) in [(NET, "/mnt/dest"), ("/mnt/dest", NET)] {
            let r = check_exclusion(Path::new(source), Path::new(dest), &fs);
            assert!(matches!(r, Err(ImportError::Overlap { .. })), "{target} {source}: {r:?}");
        }
    }
    for at in [
        format!("{DEST}/blocks/x"),
        format!("{DEST}/{STAGING}/blocks/x"),
        format!("{DEST}/chainstate/deep"),
        format!("{DEST}/other"),
    ] {
        let fs = fixture_image().with_bind(&at, format!("{NET}/evodb/deep")).build();
        assert!(
            matches!(
                preflight_destination(Path::new(DEST), Path::new(NET), &fs),
                Err(ImportError::Overlap { .. })
            ),
            "{at}"
        );
    }
}

/// A [`CutFs`] whose `read_dir` fails with `errno` on the `deny` dirs, whose
/// `metadata` fails with it on each child of a `no_search` dir, and which has
/// no mount table when `no_mount_table` is set.
struct DenyList {
    inner: CutFs,
    deny: Vec<PathBuf>,
    no_search: Vec<PathBuf>,
    errno: i32,
    no_mount_table: bool,
}
impl DenyList {
    fn new(inner: CutFs, deny: Vec<PathBuf>) -> Self {
        Self {
            inner,
            deny,
            no_search: Vec::new(),
            errno: libc::EACCES,
            no_mount_table: false,
        }
    }
}
macro_rules! delegate {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
        $(fn $name(&self, $($arg: $ty),*) -> io::Result<$ret> {
            self.inner.$name($($arg),*)
        })*
    };
}
impl ImportFs for DenyList {
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        if self.deny.iter().any(|d| d == path) {
            return Err(io::Error::from_raw_os_error(self.errno));
        }
        self.inner.read_dir(path)
    }
    fn metadata(&self, path: &Path) -> io::Result<Option<FileMetadata>> {
        if self.no_search.iter().any(|d| path.parent() == Some(d.as_path())) {
            return Err(io::Error::from_raw_os_error(self.errno));
        }
        self.inner.metadata(path)
    }
    fn mount_table(&self) -> io::Result<Option<Vec<u8>>> {
        if self.no_mount_table {
            return Ok(None);
        }
        self.inner.mount_table()
    }
    delegate! {
        reflink(src: &Path, dst: &Path) -> ();
        hard_link(src: &Path, dst: &Path) -> ();
        copy(src: &Path, dst: &Path) -> u64;
        sync_data(path: &Path) -> ();
        sync_dir(path: &Path) -> ();
        rename(src: &Path, dst: &Path) -> ();
        remove(path: &Path) -> ();
        truncate(path: &Path, len: u64) -> ();
        append(path: &Path, data: &[u8]) -> ();
        read(path: &Path) -> Vec<u8>;
        create_dir_all(path: &Path) -> ();
        available_space(path: &Path) -> u64;
        canonicalize(path: &Path) -> PathBuf;
        mount_id(path: &Path) -> Option<u64>;
        sha256(path: &Path) -> [u8; 32];
    }
}

/// Asserts `r` is `UnreadableDir` naming `path`, as text and as hex.
fn assert_unreadable<T: std::fmt::Debug>(r: Result<T, ImportError>, path: &str) {
    match r {
        Err(ImportError::UnreadableDir {
            path: p,
            path_hex,
            ..
        }) => {
            assert_eq!(p, path);
            assert_eq!(path_hex, hex_of(path.as_bytes()));
        }
        other => panic!("{path}: {other:?}"),
    }
}

#[test]
fn exclusion_skips_unlistable_dirs_only_outside_the_scope() {
    let deny = |dirs: &[String]| {
        let inner = fixture();
        for d in dirs {
            put(&inner, Path::new(d).join("x"), b"x");
        }
        DenyList::new(inner, dirs.iter().map(PathBuf::from).collect())
    };
    let outside = [
        format!("{NET}/lost+found"),
        format!("{NET}/backups"),
        format!("{NET}/x/y"),
        format!("{DEST}/lost+found"),
        format!("{DEST}/other/deep"),
    ];
    let fs = deny(&outside);
    check_exclusion(Path::new(NET), Path::new(DEST), &fs).unwrap();
    assert!(preflight_destination(Path::new(DEST), Path::new(NET), &fs).unwrap().is_none());
    let inside = [
        format!("{NET}/blocks/sub"),
        format!("{NET}/chainstate/sub"),
        format!("{NET}/evodb"),
        format!("{NET}/llmq/x"),
        format!("{NET}/indexes/txindex"),
        format!("{DEST}/blocks/sub"),
        format!("{DEST}/{STAGING}"),
        format!("{DEST}/{STAGING}/blocks"),
    ];
    for d in inside {
        let fs = deny(std::slice::from_ref(&d));
        assert_unreadable(check_exclusion(Path::new(NET), Path::new(DEST), &fs), &d);
    }
    // The scoped subtrees are walked first: `zz`, an alias of `blocks` reached
    // from the root, cannot claim `blocks`'s id and skip the strict rule. An
    // unlistable dir is unlistable by every path.
    let inner = fixture_image().with_bind(format!("{NET}/zz"), format!("{NET}/blocks")).build();
    put(&inner, format!("{NET}/blocks/sub/x"), b"x");
    let fs = DenyList::new(
        inner,
        vec![PathBuf::from(format!("{NET}/blocks/sub")), PathBuf::from(format!("{NET}/zz/sub"))],
    );
    assert_unreadable(
        check_exclusion(Path::new(NET), Path::new(DEST), &fs),
        &format!("{NET}/blocks/sub"),
    );
}

#[test]
fn exclusion_skips_unstatable_children_only_outside_the_scope() {
    // A dir that lists but cannot be searched (mode 0400 or 0600).
    let deny = |dir: &str| {
        let inner = fixture();
        put(&inner, Path::new(dir).join("child/x"), b"x");
        DenyList {
            no_search: vec![PathBuf::from(dir)],
            ..DenyList::new(inner, Vec::new())
        }
    };
    for dir in [format!("{NET}/backups"), format!("{NET}/x/y"), format!("{DEST}/other")] {
        let fs = deny(&dir);
        check_exclusion(Path::new(NET), Path::new(DEST), &fs).unwrap();
        assert!(preflight_destination(Path::new(DEST), Path::new(NET), &fs).unwrap().is_none());
    }
    for dir in [format!("{NET}/blocks/sub"), format!("{NET}/llmq"), format!("{DEST}/blocks")] {
        let fs = deny(&dir);
        let child = format!("{dir}/child");
        assert_unreadable(check_exclusion(Path::new(NET), Path::new(DEST), &fs), &child);
        assert_unreadable(preflight_destination(Path::new(DEST), Path::new(NET), &fs), &child);
    }
}

#[test]
fn exclusion_refuses_other_errors_outside_the_scope() {
    for errno in [libc::EIO, libc::ELOOP] {
        for dir in [format!("{NET}/lost+found"), format!("{DEST}/other/deep")] {
            let inner = fixture();
            put(&inner, Path::new(&dir).join("child/x"), b"x");
            let fs = DenyList {
                errno,
                ..DenyList::new(inner.clone(), vec![PathBuf::from(&dir)])
            };
            assert_unreadable(check_exclusion(Path::new(NET), Path::new(DEST), &fs), &dir);
            assert_unreadable(preflight_destination(Path::new(DEST), Path::new(NET), &fs), &dir);
            let fs = DenyList {
                errno,
                no_search: vec![PathBuf::from(&dir)],
                ..DenyList::new(inner, Vec::new())
            };
            let child = format!("{dir}/child");
            assert_unreadable(check_exclusion(Path::new(NET), Path::new(DEST), &fs), &child);
        }
    }
    let inner = fixture();
    put(&inner, format!("{NET}/backups/x"), b"x");
    let fs = DenyList {
        errno: libc::EPERM,
        ..DenyList::new(inner, vec![PathBuf::from(format!("{NET}/backups"))])
    };
    check_exclusion(Path::new(NET), Path::new(DEST), &fs).unwrap();
}

/// The reviewers' layouts: a bind made from under a dir the walk cannot list,
/// so only the mount table shows the nesting.
fn binds_from_under_unlistable_dirs() -> Vec<(DenyList, &'static str, &'static str)> {
    let deny =
        |image: cutfs::Image, dir: &str| DenyList::new(image.build(), vec![PathBuf::from(dir)]);
    let lost = format!("{NET}/lost+found");
    let wallets = format!("{NET}/wallets");
    vec![
        (
            deny(fixture_image().with_bind("/mnt/dest", format!("{lost}/d")), &lost),
            NET,
            "/mnt/dest",
        ),
        (
            deny(fixture_image().with_bind("/mnt/dest", format!("{wallets}/sub")), &wallets),
            NET,
            "/mnt/dest",
        ),
    ]
}

#[test]
fn exclusion_refuses_bind_from_under_an_unlistable_dir() {
    for (fs, source, dest) in binds_from_under_unlistable_dirs() {
        for (s, d) in [(source, dest), (dest, source)] {
            let r = check_exclusion(Path::new(s), Path::new(d), &fs);
            assert!(matches!(r, Err(ImportError::Overlap { .. })), "{s} {d}: {r:?}");
        }
    }
    // From preflight, both ways round.
    let lost = format!("{NET}/lost+found");
    let image = fixture_image()
        .with_file(format!("{NET}/lost+found/d/blocks/blk00000.dat"), blk_bytes(0))
        .with_bind("/mnt/dest", format!("{lost}/d"));
    let fs = DenyList::new(image.build(), vec![PathBuf::from(&lost)]);
    for (d, s) in [("/mnt/dest", NET), (NET, "/mnt/dest")] {
        let r = preflight_destination(Path::new(d), Path::new(s), &fs);
        assert!(matches!(r, Err(ImportError::Overlap { .. })), "{d} {s}: {r:?}");
    }
}

#[test]
fn exclusion_skips_the_mount_check_without_a_mount_table() {
    // Known limit off Linux: with no mount table, the walk alone cannot see
    // these binds.
    for (fs, source, dest) in binds_from_under_unlistable_dirs() {
        let fs = DenyList {
            no_mount_table: true,
            ..fs
        };
        check_exclusion(Path::new(source), Path::new(dest), &fs).unwrap();
    }
}

#[test]
fn exclusion_refuses_a_source_alias_mounted_under_an_unlistable_destination_dir() {
    // A second mount of the source inside a destination dir the walk cannot
    // list: the mount table's entries under the destination show it.
    let image =
        fixture_image().with_dir("/mnt/n/blocks").with_bind(format!("{DEST}/priv/n"), "/mnt/n");
    let fs = DenyList::new(image.build(), vec![PathBuf::from(format!("{DEST}/priv"))]);
    for (s, d) in [("/mnt/n", DEST), (DEST, "/mnt/n")] {
        let r = check_exclusion(Path::new(s), Path::new(d), &fs);
        assert!(matches!(r, Err(ImportError::Overlap { .. })), "{s} {d}: {r:?}");
    }
    let fs = DenyList {
        deny: Vec::new(),
        ..fs
    };
    assert!(matches!(
        check_exclusion(Path::new("/mnt/n"), Path::new(DEST), &fs),
        Err(ImportError::Overlap { .. })
    ));
}

/// R3-1's layouts: `<DEST>/<sub>` bound from `<NET>/lost+found/x` with
/// `lost+found` unlistable, so neither the walk nor the two located roots see
/// it.
fn binds_below_the_destination() -> Vec<(&'static str, DenyList)> {
    ["import-staging", "blocks", "store", "chainstate", "other"]
        .into_iter()
        .map(|sub| {
            let image = fixture_image()
                .with_dir(format!("{NET}/lost+found/x"))
                .with_bind(format!("{DEST}/{sub}"), format!("{NET}/lost+found/x"));
            (sub, DenyList::new(image.build(), vec![PathBuf::from(format!("{NET}/lost+found"))]))
        })
        .collect()
}

#[test]
fn exclusion_refuses_a_bind_below_the_destination_from_an_unlistable_source_dir() {
    for (sub, fs) in binds_below_the_destination() {
        for (s, d) in [(NET, DEST), (DEST, NET)] {
            let r = check_exclusion(Path::new(s), Path::new(d), &fs);
            assert!(matches!(r, Err(ImportError::Overlap { .. })), "{sub} {s} {d}: {r:?}");
        }
        let r = preflight_destination(Path::new(DEST), Path::new(NET), &fs);
        assert!(matches!(r, Err(ImportError::Overlap { .. })), "{sub} preflight: {r:?}");
    }
    // A mount below the destination exposing a dir above the source.
    let image = fixture_image().with_bind(format!("{DEST}/other/up"), "/core");
    let fs = DenyList::new(image.build(), vec![PathBuf::from(format!("{DEST}/other"))]);
    for (s, d) in [(NET, DEST), (DEST, NET)] {
        let r = check_exclusion(Path::new(s), Path::new(d), &fs);
        assert!(matches!(r, Err(ImportError::Overlap { .. })), "up {s} {d}: {r:?}");
    }
    let r = preflight_destination(Path::new(DEST), Path::new(NET), &fs);
    assert!(matches!(r, Err(ImportError::Overlap { .. })), "up preflight: {r:?}");
}

#[test]
fn exclusion_passes_an_unrelated_mount_below_the_destination() {
    // A disjoint path on the same filesystem.
    let fs =
        fixture_image().with_dir("/srv/x/y").with_bind(format!("{DEST}/other"), "/srv/x").build();
    check_exclusion(Path::new(NET), Path::new(DEST), &fs).unwrap();
    check_exclusion(Path::new(DEST), Path::new(NET), &fs).unwrap();
    assert!(preflight_destination(Path::new(DEST), Path::new(NET), &fs).unwrap().is_none());
    // Another filesystem whose root path matches the source's.
    let fs = fixture_image().with_dir(format!("{DEST}/other")).build();
    let mut table = fs.mount_table().unwrap().unwrap();
    table
        .extend_from_slice(format!("9 1 0:77 {NET} {DEST}/other rw - tmpfs tmpfs rw\n").as_bytes());
    fs.set_faults(Faults {
        mount_table: Some(Ok(table)),
        ..Faults::default()
    });
    check_exclusion(Path::new(NET), Path::new(DEST), &fs).unwrap();
    check_exclusion(Path::new(DEST), Path::new(NET), &fs).unwrap();
    assert!(preflight_destination(Path::new(DEST), Path::new(NET), &fs).unwrap().is_none());
}

/// `fixture` with `extra` mountinfo lines after its own and the given mount
/// ids.
fn with_mounts(extra: &str, ids: Option<Vec<(&str, u64)>>) -> CutFs {
    let fs = fixture_image().with_dir("/mnt/a/b/regtest").build();
    let mut table = fs.mount_table().unwrap().unwrap();
    table.extend_from_slice(extra.as_bytes());
    fs.set_faults(Faults {
        mount_table: Some(Ok(table)),
        mount_ids: Some(
            ids.map(|ids| ids.into_iter().map(|(p, i)| (PathBuf::from(p), i)).collect()),
        ),
        ..Faults::default()
    });
    fs
}

#[test]
fn exclusion_locates_a_root_by_its_mount_id() {
    let dest = "/mnt/a/b/regtest";
    let (maj, min) = mounts::parse(&fixture().mount_table().unwrap().unwrap()).unwrap()[0].dev;
    // `/mnt/a/b` (2) is hidden by the later `/mnt/a` (3), which resolves the
    // destination: a bind of the source's parent that is not reachable, and a
    // harmless one that is.
    let hidden_alias = format!(
        "2 1 {maj}:{min} {BASE} /mnt/a/b rw - cutfs cutfs rw\n3 1 {maj}:{min} /srv/a /mnt/a rw - cutfs cutfs rw\n"
    );
    let top_alias = format!(
        "2 1 {maj}:{min} /srv /mnt/a/b rw - cutfs cutfs rw\n3 1 {maj}:{min} {NET}/x /mnt/a rw - cutfs cutfs rw\n"
    );
    let ids = Some(vec![("/", 1), ("/mnt/a", 3)]);
    for (s, d) in [(NET, dest), (dest, NET)] {
        check_exclusion(Path::new(s), Path::new(d), &with_mounts(&hidden_alias, ids.clone()))
            .unwrap();
        let r = check_exclusion(Path::new(s), Path::new(d), &with_mounts(&top_alias, ids.clone()));
        assert!(matches!(r, Err(ImportError::Overlap { .. })), "{s} {d}: {r:?}");
    }
    let fs = with_mounts(&hidden_alias, ids.clone());
    assert!(preflight_destination(Path::new(dest), Path::new(NET), &fs).unwrap().is_none());
    let fs = with_mounts(&top_alias, ids);
    let r = preflight_destination(Path::new(dest), Path::new(NET), &fs);
    assert!(matches!(r, Err(ImportError::Overlap { .. })), "{r:?}");
}

#[test]
fn exclusion_falls_back_to_the_mount_point_prefix_without_a_mount_id() {
    // Without ids the longest prefix wins, so the hidden `/mnt/a/b` is picked.
    let dest = "/mnt/a/b/regtest";
    let (maj, min) = mounts::parse(&fixture().mount_table().unwrap().unwrap()).unwrap()[0].dev;
    let hidden_alias = format!(
        "2 1 {maj}:{min} {BASE} /mnt/a/b rw - cutfs cutfs rw\n3 1 {maj}:{min} /srv/a /mnt/a rw - cutfs cutfs rw\n"
    );
    let r = check_exclusion(Path::new(NET), Path::new(dest), &with_mounts(&hidden_alias, None));
    assert!(matches!(r, Err(ImportError::Overlap { .. })), "{r:?}");
    let r = check_exclusion(Path::new(NET), Path::new(dest), &with_mounts("", None));
    r.unwrap();
    // The usual layouts work the same without ids.
    let no_ids = |fs: DenyList| {
        fs.inner.set_faults(Faults {
            mount_ids: Some(None),
            ..Faults::default()
        });
        fs
    };
    for (sub, fs) in binds_below_the_destination() {
        let fs = no_ids(fs);
        let r = check_exclusion(Path::new(NET), Path::new(DEST), &fs);
        assert!(matches!(r, Err(ImportError::Overlap { .. })), "{sub}: {r:?}");
    }
    for (fs, source, dest) in binds_from_under_unlistable_dirs() {
        let r = check_exclusion(Path::new(source), Path::new(dest), &no_ids(fs));
        assert!(matches!(r, Err(ImportError::Overlap { .. })), "{source} {dest}: {r:?}");
    }
    let fs = fixture();
    fs.set_faults(Faults {
        mount_ids: Some(None),
        ..Faults::default()
    });
    check_exclusion(Path::new(NET), Path::new(DEST), &fs).unwrap();
}

#[test]
fn exclusion_refuses_a_mount_id_missing_from_the_table() {
    let fs = with_mounts("", Some(vec![("/", 1), (DEST, 99)]));
    for r in [
        check_exclusion(Path::new(NET), Path::new(DEST), &fs),
        preflight_destination(Path::new(DEST), Path::new(NET), &fs).map(|_| ()),
    ] {
        match r {
            Err(e @ ImportError::MountTable(_)) => {
                let text = e.to_string();
                assert!(
                    text.contains("mount id 99 of /dest/regtest is not in the table"),
                    "{text}"
                );
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn mount_table_without_the_root_mount_names_the_chroot_remedy() {
    // In a chroot whose root is not a mount point the table is empty, or
    // holds only mounts made inside it (here `/proc`).
    let proc_only = b"5 1 0:5 / /proc rw - proc proc rw\n".to_vec();
    for (table, ids) in [
        (Vec::new(), None),
        (b"\n".to_vec(), None),
        (proc_only.clone(), Some(None)),
        (proc_only, Some(Some(vec![(PathBuf::from("/"), 1)]))),
    ] {
        let fs = fixture();
        fs.set_faults(Faults {
            mount_table: Some(Ok(table.clone())),
            mount_ids: ids,
            ..Faults::default()
        });
        for r in [
            check_exclusion(Path::new(NET), Path::new(DEST), &fs),
            preflight_destination(Path::new(DEST), Path::new(NET), &fs).map(|_| ()),
        ] {
            match r {
                Err(e @ ImportError::MountTable(_)) => {
                    let text = e.to_string();
                    for want in [
                        "/proc/self/mountinfo",
                        "in a chroot whose root is not a mount point",
                        "run the node outside the chroot",
                        "bind-mount the chroot root onto itself",
                    ] {
                        assert!(text.contains(want), "{want}: {text}");
                    }
                }
                other => panic!("{table:?}: {other:?}"),
            }
        }
    }
}

#[test]
fn exclusion_passes_an_unrelated_destination_on_the_same_filesystem() {
    let fs = fixture_image()
        .with_dir("/dest/other/regtest/blocks")
        .with_bind("/mnt/other", "/dest/other/regtest")
        .with_bind("/mnt/core", BASE)
        .build();
    for dest in [DEST, "/mnt/other", "/core/.dashcore/regtest2", "/core/.dashcore/testnet3"] {
        check_exclusion(Path::new(NET), Path::new(dest), &fs).unwrap();
        check_exclusion(Path::new(dest), Path::new(NET), &fs).unwrap();
        assert!(preflight_destination(Path::new(dest), Path::new(NET), &fs).unwrap().is_none());
    }
}

#[test]
fn exclusion_refuses_an_unreadable_or_garbled_mount_table() {
    for table in [
        Err(libc::EACCES),
        Err(libc::EIO),
        Ok(Vec::new()),
        Ok(b"garbage\n".to_vec()),
        Ok(b"1 0 0:1 / / rw shared:1\n".to_vec()),
        Ok(b"1 0 0:1 /a\\9 / rw - ext4 /dev/x rw\n".to_vec()),
        // No mount holds the paths: `/` is missing.
        Ok(b"1 0 0:1 / /mnt rw - ext4 /dev/x rw\n".to_vec()),
    ] {
        let fs = fixture();
        fs.set_faults(Faults {
            mount_table: Some(table.clone()),
            ..Faults::default()
        });
        for r in [
            check_exclusion(Path::new(NET), Path::new(DEST), &fs),
            preflight_destination(Path::new(DEST), Path::new(NET), &fs).map(|_| ()),
        ] {
            match r {
                Err(e @ ImportError::MountTable(_)) => {
                    let text = e.to_string();
                    assert!(text.contains("/proc/self/mountinfo"), "{text}");
                }
                other => panic!("{table:?}: {other:?}"),
            }
        }
    }
}

#[test]
fn mountinfo_parses_escapes_and_optional_fields() {
    let text = b"22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
36 22 8:1 /srv/a\\040b\\011c\\012d\\134e /mnt/x\\040y rw - ext4 /dev/sda1 rw\n\
37 22 0:42 /@data /data rw,noatime shared:5 master:2 propagate_from:3 unbindable - btrfs /dev/sdb rw,subvol=/@data\n\
38 22 0:43 / /odd - rw - tmpfs tmpfs rw\n";
    let m = mounts::parse(text).unwrap();
    assert_eq!(m.len(), 4);
    assert_eq!(m[0].dev, (8, 1));
    assert_eq!(m[0].root, Path::new("/"));
    assert_eq!(m[1].root, Path::new("/srv/a b\tc\nd\\e"));
    assert_eq!(m[1].mount_point, Path::new("/mnt/x y"));
    assert_eq!(m[2].dev, (0, 42));
    assert_eq!(m[2].root, Path::new("/@data"));
    assert_eq!(m[2].mount_point, Path::new("/data"));
    // A `-` in the options column is not the separator.
    assert_eq!(m[3].mount_point, Path::new("/odd"));
    assert_eq!(m[3].dev, (0, 43));
}

#[test]
fn mountinfo_picks_the_longest_mount_point_prefix() {
    let text = b"1 0 8:1 / / rw - ext4 /dev/sda1 rw\n\
2 1 0:42 /@data /data rw - btrfs /dev/sdb rw\n\
3 2 0:42 /@data/core/blocks /data/core/blocks rw - btrfs /dev/sdb rw\n\
4 1 0:42 /@data/core/lost+found/d /mnt/dest rw - btrfs /dev/sdb rw\n\
5 1 8:1 /home /database rw - ext4 /dev/sda1 rw\n\
6 1 0:50 / /data/core/blocks rw - tmpfs tmpfs rw\n";
    let m = mounts::parse(text).unwrap();
    let at = |p: &str| {
        let (mount, inside) = mounts::locate(&m, Path::new(p), None).unwrap();
        (mount.mount_point.to_str().unwrap().to_owned(), inside)
    };
    assert_eq!(at("/data/core"), ("/data".into(), PathBuf::from("/@data/core")));
    // Component-wise: `/database` is not under `/data`.
    assert_eq!(at("/database/x"), ("/database".into(), PathBuf::from("/home/x")));
    assert_eq!(at("/datab"), ("/".into(), PathBuf::from("/datab")));
    // Nested mounts, and the later of two at one point is on top.
    assert_eq!(at("/data/core/blocks/b"), ("/data/core/blocks".into(), PathBuf::from("/b")));
    assert_eq!(
        at("/mnt/dest/x"),
        ("/mnt/dest".into(), PathBuf::from("/@data/core/lost+found/d/x"))
    );
    assert_eq!(at("/"), ("/".into(), PathBuf::from("/")));
    let nest =
        |a: &str, b: &str| mounts::nest(&m, (Path::new(a), None), (Path::new(b), None)).unwrap();
    assert!(nest("/data/core", "/mnt/dest"));
    assert!(nest("/mnt/dest", "/data/core"));
    assert!(nest("/mnt/dest/x", "/data/core/lost+found/d/x"));
    assert!(!nest("/data/core2", "/mnt/dest"));
    // Same paths inside different filesystems.
    assert!(!nest("/home", "/data/home"));
    assert!(!nest("/data/core/blocks/x", "/x"));
    assert!(nest("/database", "/home/x"));
    // A mount at or below one path exposing the other, either way.
    assert!(nest("/data/core", "/mnt"));
    assert!(nest("/mnt", "/data/core"));
    assert!(nest("/mnt", "/data/core/lost+found/d/e"));
    assert!(!nest("/mnt", "/data/core2"));
    // By mount id: 4 resolves `/mnt/dest/x` whatever the prefixes say, and
    // an id outside the table is an error.
    let (mount, inside) = mounts::locate(&m, Path::new("/mnt/dest/x"), Some(4)).unwrap();
    assert_eq!((mount.id, inside), (4, PathBuf::from("/@data/core/lost+found/d/x")));
    let (mount, _) = mounts::locate(&m, Path::new("/data/core/blocks/b"), Some(3)).unwrap();
    assert_eq!(mount.mount_point, Path::new("/data/core/blocks"));
    assert!(mounts::locate(&m, Path::new("/x"), Some(7)).is_err());
    assert!(mounts::locate(&m, Path::new("/x"), Some(2)).is_err());
}

#[test]
fn mountinfo_rejects_garbled_lines() {
    for text in [
        &b""[..],
        b"\n\n",
        b"1 0 8:1 / / rw ext4 /dev/sda1 rw\n",
        b"1 0 8:1 / / rw - ext4\n",
        b"1 0 8-1 / / rw - ext4 /dev/sda1 rw\n",
        b"x 0 8:1 / / rw - ext4 /dev/sda1 rw\n",
        b"1 0 8:1 / rel rw - ext4 /dev/sda1 rw\n",
        b"1 0 8:1 /a\\08 / rw - ext4 /dev/sda1 rw\n",
        b"1 0 8:1 /a\\4 / rw - ext4 /dev/sda1 rw\n",
        b"1 0 8:1 /a\\777 / rw - ext4 /dev/sda1 rw\n",
    ] {
        assert!(mounts::parse(text).is_err(), "{}", String::from_utf8_lossy(text));
    }
}

#[test]
fn unreadable_store_dir_is_refused_by_name() {
    for store in STORE_DIRS {
        let d = format!("{DEST}/{store}");
        let inner = fixture();
        put(&inner, Path::new(&d).join("CURRENT"), b"x");
        let fs = DenyList::new(inner, vec![PathBuf::from(&d)]);
        match preflight_destination(Path::new(DEST), Path::new(NET), &fs) {
            Err(
                e @ ImportError::UnreadableDir {
                    ..
                },
            ) => {
                let text = e.to_string();
                assert!(text.starts_with(&format!("cannot list {d} (path bytes ")), "{text}");
                assert!(text.contains(&hex_of(d.as_bytes())), "{text}");
            }
            other => panic!("{d}: {other:?}"),
        }
    }
}

#[test]
fn preflight_treats_empty_store_dirs_as_absent() {
    let cfg = Cfg::reflink();
    let fs = fixture_for(&cfg);
    for store in STORE_DIRS {
        fs.create_dir_all(&Path::new(DEST).join(store)).unwrap();
    }
    assert!(preflight_destination(Path::new(DEST), Path::new(NET), &fs).unwrap().is_none());
    assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed);
    let fs = fixture();
    put(&fs, Path::new(DEST).join("chainstate"), b"x");
    assert!(matches!(
        preflight_destination(Path::new(DEST), Path::new(NET), &fs),
        Err(ImportError::DestinationNotEmpty(p)) if p.ends_with("chainstate")
    ));
}

#[test]
fn reflink_errors_outside_the_fallback_set_propagate() {
    for cfg in [Cfg::reflink(), Cfg::hardlink(), Cfg::snapshot()] {
        for errno in [libc::EIO, libc::ENOSPC, libc::EPERM] {
            let mut cfg = cfg.clone();
            cfg.faults.reflink_errno = Some(errno);
            let fs = fixture_for(&cfg);
            fs.reset_ops();
            match run(&fs, &cfg) {
                Err(ImportError::Io(e)) => assert_eq!(e.raw_os_error(), Some(errno)),
                other => panic!("{errno}: {other:?}"),
            }
            assert!(
                !fs.log().iter().any(|l| l.starts_with("hard_link") || l.starts_with("copy")),
                "{errno}: {:?}",
                fs.log()
            );
            let j = Journal::open(&jpath(), &fs).unwrap();
            assert_eq!(j.last(), Some(Phase::Intent));
            assert!((0..3).all(|n| fs.metadata(&src_blk(n)).unwrap().unwrap().nlink == 1));
        }
    }
    let mut cfg = Cfg::reflink();
    cfg.faults.reflink_errno = Some(libc::EINVAL);
    let (_, methods) = methods_after(&cfg);
    assert!(methods.iter().all(|m| m.method == Method::Copy && m.reflink_errno == libc::EINVAL));
}

/// Each destination path the import writes, as a symlink into the source.
fn symlinked_destinations() -> Vec<(PathBuf, PathBuf)> {
    let staging = Path::new(DEST).join(STAGING);
    vec![
        (staging.clone(), Path::new(NET).join("blocks")),
        (staging.join("blocks"), Path::new(NET).join("blocks")),
        (staging.join(JOURNAL), src_blk(0)),
        (dest_blocks(), Path::new(NET).join("blocks")),
        (Path::new(DEST).join("import-report.json"), src_blk(0)),
        (Path::new(DEST).join("import-report.json.tmp"), src_blk(0)),
    ]
}

fn with_symlink_at(fs: &CutFs, path: &Path, target: &Path) {
    fs.remove(path).unwrap();
    fs.create_dir_all(path.parent().unwrap()).unwrap();
    fs.add_symlink(path, target);
    fs.crash_keep();
}

#[test]
fn symlinked_destination_paths_are_refused() {
    let cfg = Cfg::copy();
    for (path, target) in symlinked_destinations() {
        let fs = fixture_for(&cfg);
        with_symlink_at(&fs, &path, &target);
        let source = files_under(&fs, NET);
        let before = fs.durable_snapshot();
        for result in [
            preflight_destination(Path::new(DEST), Path::new(NET), &fs).map(|_| Phase::Intent),
            stage(
                &inspection(cfg.l),
                cfg.options,
                Path::new(BASE),
                Some("regtest"),
                Path::new(DEST),
                &fs,
                &DurableGuard::new(&fs, &cfg),
            ),
        ] {
            assert!(
                matches!(&result, Err(ImportError::DestinationSymlink(p)) if p == &path),
                "{}: {result:?}",
                path.display()
            );
        }
        fs.crash_keep();
        assert_eq!(fs.durable_snapshot(), before, "{}", path.display());
        assert_eq!(files_under(&fs, NET), source);
    }
    // A JOURNAL that is a second link to a source file is refused too.
    let fs = fixture_for(&cfg);
    fs.create_dir_all(&Path::new(DEST).join(STAGING)).unwrap();
    fs.hard_link(&src_blk(0), &jpath()).unwrap();
    fs.crash_keep();
    assert!(matches!(
        preflight_destination(Path::new(DEST), Path::new(NET), &fs),
        Err(ImportError::DestinationSymlink(p)) if p == jpath()
    ));
    // P5 checks too, on a journal already at ReindexArmed.
    for name in ["import-report.json", "import-report.json.tmp"] {
        let fs = image_at(&cfg, Phase::ReindexArmed, Where::Dest);
        with_symlink_at(&fs, &Path::new(DEST).join(name), &src_blk(0));
        let mut j = Journal::open(&jpath(), &fs).unwrap();
        let result = report(&mut j, outcome(TipRelation::Equal), &fs);
        assert!(matches!(result, Err(ImportError::DestinationSymlink(_))), "{name}: {result:?}");
        fs.crash();
        assert_eq!(fs.read(&src_blk(0)).unwrap(), blk_bytes(0));
        assert_eq!(durable_journal(&fs).last(), Some(Phase::ReindexArmed));
    }
}

#[test]
fn hardlink_landed_reset_cut_matrix() {
    let cfg = Cfg::hardlink();
    let base = image_at(&cfg, Phase::SourceVerified, Where::Dest);
    mutate_linked(&base);
    let mutated = base.read(&src_blk(0)).unwrap();
    let probe = base.fork();
    assert!(matches!(run(&probe, &cfg), Err(ImportError::SourceChanged(_))));
    let n = probe.ops();
    for k in 1..=n {
        for variant in 0..VARIANTS {
            let fs = base.fork();
            fs.cut_after(k);
            let _ = run(&fs, &cfg);
            crash_variant(&fs, variant);
            if durable_journal(&fs).last() == Some(Phase::SourceVerified) {
                let staged = names(&fs, &staged_dir()).len();
                let landed = names(&fs, &dest_blocks()).len();
                assert!(
                    (staged == 3) != (landed == 3) && staged + landed == 3,
                    "k={k} variant={variant}: staged {staged}, landed {landed}"
                );
            }
            let mut result = run(&fs, &cfg);
            if matches!(result, Err(ImportError::SourceChanged(_))) {
                result = run(&fs, &cfg);
            }
            assert_eq!(result.unwrap(), Phase::Placed, "k={k} variant={variant}");
            fs.crash();
            assert_eq!(fs.read(&dest_blocks().join(blk_name(0))).unwrap(), mutated);
            assert_eq!(fs.read(&src_blk(0)).unwrap(), mutated);
            assert_eq!(fs.read(&src_blk(1)).unwrap(), blk_bytes(1));
        }
    }
}

/// The same records with a non-zero `at_ms`, so no payload starts with a
/// zero byte.
fn stamped(bytes: &[u8]) -> Vec<u8> {
    let j = Journal::decode(&jpath(), bytes).unwrap();
    let mut out = journal::MAGIC.to_vec();
    for (i, r) in j.records().iter().enumerate() {
        let r = Record {
            at_ms: 1_700_000_000_001 + i as u64,
            ..r.clone()
        };
        out.extend_from_slice(&frame(&r.encode()));
    }
    out
}

#[test]
fn journal_flipped_digest_is_corrupt_mid_file_and_at_end() {
    let full = stamped(&full_journal(&Cfg::copy()));
    let at = offsets(&full);
    // A byte of SourceVerified's M1 digest (after len, hcrc, phase and at_ms).
    let digest_byte = at[2] + 8 + 1 + 8 + 5;
    for len in [full.len(), at[3]] {
        let mut bytes = full[..len].to_vec();
        bytes[digest_byte] ^= 1;
        let fs = journal_image(&bytes);
        assert!(matches!(Journal::open(&jpath(), &fs), Err(ImportError::JournalCorrupt(_))));
        assert_eq!(fs.read(&jpath()).unwrap(), bytes);
    }
}

#[test]
fn journal_short_final_frame_needs_the_next_phase_byte() {
    let full = stamped(&full_journal(&Cfg::copy()));
    let at = offsets(&full);
    // SourceVerified cut short after its valid header is a torn append.
    let torn = full[..at[3] - 3].to_vec();
    let j = Journal::open(&jpath(), &journal_image(&torn)).unwrap();
    assert_eq!(j.last(), Some(Phase::Staged));
    assert!(j.needs_repair());
    // A complete-length final frame whose head is real and whose tail is
    // zeros is not a torn append: only a zero head is.
    let mut zeroed = full[..at[3]].to_vec();
    zeroed.extend_from_slice(&full[at[3]..at[3] + 3]);
    zeroed.resize(at[4], 0);
    assert!(matches!(
        Journal::open(&jpath(), &journal_image(&zeroed)),
        Err(ImportError::JournalCorrupt(_))
    ));
    // Placed's frame, cut short, where SourceVerified is next, is corruption.
    let mut skipped = full[..at[2]].to_vec();
    skipped.extend_from_slice(&full[at[3]..at[4] - 2]);
    assert!(matches!(
        Journal::open(&jpath(), &journal_image(&skipped)),
        Err(ImportError::JournalCorrupt(_))
    ));
}

/// `bytes` with one bit of record `rec`'s length flipped.
fn flip_len(bytes: &[u8], at: &[usize], rec: usize, bit: usize) -> Vec<u8> {
    let mut out = bytes.to_vec();
    out[at[rec] + bit / 8] ^= 1 << (bit % 8);
    out
}

#[test]
fn journal_header_flip_is_corrupt_and_truncates_nothing() {
    let cfg = Cfg::copy();
    let full = full_journal(&cfg);
    let at = offsets(&full);
    // Through ReindexArmed (the reviewer's journal) and through Reported; a
    // single high bit of Intent's or Staged's length, or of a fixed-length
    // record's, mid-file.
    for len in [at[5], at[6]] {
        for (rec, bit) in [(0, 12), (0, 16), (0, 31), (1, 12), (1, 16), (1, 24), (2, 20), (3, 9)] {
            let bytes = flip_len(&full[..len], &at, rec, bit);
            let fs = journal_image(&bytes);
            fs.reset_ops();
            let e = Journal::open(&jpath(), &fs).unwrap_err();
            assert!(matches!(e, ImportError::JournalCorrupt(_)), "rec={rec} bit={bit}: {e}");
            assert!(matches!(
                preflight_destination(Path::new(DEST), Path::new(NET), &fs),
                Err(ImportError::JournalCorrupt(_))
            ));
            // The owning resume path refuses too, and truncates nothing.
            assert!(matches!(run(&fs, &cfg), Err(ImportError::JournalCorrupt(_))));
            assert_eq!(fs.ops(), 0, "rec={rec} bit={bit}");
            fs.crash();
            assert_eq!(fs.read(&jpath()).unwrap(), bytes, "rec={rec} bit={bit}");
        }
    }
    // A complete final header whose crc fails is corrupt as well.
    let bytes = flip_len(&full[..at[2]], &at, 1, 12);
    assert!(matches!(Journal::decode(&jpath(), &bytes), Err(ImportError::JournalCorrupt(_))));
}

#[test]
fn finished_import_with_a_header_flip_is_corrupt_not_in_progress() {
    let cfg = Cfg::reflink();
    let fs = image_at(&cfg, Phase::Reported, Where::Dest);
    let full = fs.read(&jpath()).unwrap();
    let at = offsets(&full);
    let bytes = flip_len(&full, &at, 1, 12);
    fs.truncate(&jpath(), 0).unwrap();
    fs.append(&jpath(), &bytes).unwrap();
    fs.crash_keep();
    let before = fs.durable_snapshot();
    fs.reset_ops();
    let corrupt = |e: ImportError| match e {
        ImportError::JournalCorrupt(t) => {
            assert!(t.contains(&format!("the import journal at {} is corrupt", jpath().display())));
        }
        other => panic!("expected JournalCorrupt, got {other:?}"),
    };
    // A start with no -importcoredatadir opens the journal first.
    corrupt(Journal::open(&jpath(), &fs).unwrap_err());
    corrupt(preflight_destination(Path::new(DEST), Path::new(NET), &fs).unwrap_err());
    corrupt(run(&fs, &cfg).unwrap_err());
    assert_eq!(fs.ops(), 0);
    fs.crash();
    assert_eq!(fs.durable_snapshot(), before);
}

#[test]
fn journal_length_above_the_bound_is_corrupt() {
    let full = full_journal(&Cfg::copy());
    let at = offsets(&full);
    let header = |len: u32, phase: u8| {
        let mut h = len.to_le_bytes().to_vec();
        h.extend_from_slice(&journal::header_crc(len.to_le_bytes(), phase).to_le_bytes());
        h.push(phase);
        h
    };
    // Each header verifies, and the file ends inside the payload it claims,
    // which would be a torn append if the length were in bound.
    for (records, len, phase) in [
        (0, u32::MAX, 0),
        (0, journal::MAX_INTENT as u32 + 1, 0),
        (1, journal::MAX_STAGED as u32 + 1, 1),
        (2, 42, 2),
    ] {
        let mut bytes = full[..at[records]].to_vec();
        bytes.extend_from_slice(&header(len, phase));
        bytes.extend_from_slice(&[0x5a; 40]);
        let fs = journal_image(&bytes);
        fs.reset_ops();
        assert!(
            matches!(Journal::open(&jpath(), &fs), Err(ImportError::JournalCorrupt(_))),
            "len={len} phase={phase}"
        );
        assert_eq!(fs.ops(), 0);
    }
    // In bound, the same shape is a torn append.
    let mut bytes = full[..at[1]].to_vec();
    bytes.extend_from_slice(&header(4096, 1));
    bytes.extend_from_slice(&[0x5a; 40]);
    assert!(Journal::decode(&jpath(), &bytes).unwrap().needs_repair());
}

/// The next sector boundary after `at`.
fn sector_end(at: usize) -> usize {
    (at / journal::SECTOR + 1) * journal::SECTOR
}

#[test]
fn journal_zero_head_final_frame_is_torn() {
    let cfg = Cfg::copy();
    let full = stamped(&full_journal(&cfg));
    let at = offsets(&full);
    let zero = |bytes: &[u8], from: usize, to: usize| {
        let mut out = bytes.to_vec();
        out[from..to].fill(0);
        out
    };
    // Intent's payload crosses a sector boundary; Staged's ends before one.
    let intent_end = sector_end(at[0] + 9);
    assert!(intent_end < at[1] && sector_end(at[1] + 9) > at[2]);
    // (journal, last good phase): the reviewer's zero head of a final Intent
    // and of a final Staged frame, an all-zero header with the rest real, and
    // a valid header whose payload is zero up to the sector end or EOF.
    for (bytes, last) in [
        (zero(&full[..at[1]], at[0], at[0] + 40), None),
        (zero(&full[..at[2]], at[1], at[1] + 9), Some(Phase::Intent)),
        (zero(&full[..at[1]], at[0] + 9, intent_end), None),
        (zero(&full[..at[2]], at[1] + 9, at[2]), Some(Phase::Intent)),
        (zero(&full[..at[3]], at[2], at[3]), Some(Phase::Staged)),
    ] {
        let fs = journal_image(&bytes);
        fs.reset_ops();
        let j = Journal::open(&jpath(), &fs).unwrap();
        assert_eq!((j.last(), j.needs_repair()), (last, true));
        preflight_destination(Path::new(DEST), Path::new(NET), &fs).unwrap();
        assert_eq!(fs.ops(), 0);
        assert_eq!(fs.read(&jpath()).unwrap(), bytes);
        if last == Some(Phase::Staged) {
            // Staging is empty in this image, so repair directly.
            let mut j = j;
            j.repair(&fs).unwrap();
            assert_eq!(fs.read(&jpath()).unwrap(), &full[..at[2]]);
        } else {
            assert_eq!(run(&fs, &cfg).unwrap(), Phase::Placed);
        }
    }
    // Not the final frame: a valid frame follows, so it is corruption.
    for bytes in [zero(&full[..at[3]], at[1], at[1] + 9), zero(&full[..at[3]], at[1] + 9, at[2])] {
        assert!(matches!(Journal::decode(&jpath(), &bytes), Err(ImportError::JournalCorrupt(_))));
    }
    // A final frame that does not verify and whose head is not lost: garbage,
    // a header only partly zero, or a zero payload start that stops short of
    // the sector end and of EOF.
    let mut garbage = full[..at[2]].to_vec();
    garbage[at[1] + 9..at[1] + 30].fill(0x77);
    for bytes in [
        garbage,
        zero(&full[..at[2]], at[1], at[1] + 8),
        zero(&full[..at[2]], at[1], at[1] + 3),
        zero(&full[..at[2]], at[1] + 9, at[1] + 30),
        zero(&full[..at[2]], at[1] + 9, at[2] - 1),
        zero(&full[..at[1]], at[0] + 9, intent_end - 1),
    ] {
        let fs = journal_image(&bytes);
        fs.reset_ops();
        assert!(matches!(Journal::open(&jpath(), &fs), Err(ImportError::JournalCorrupt(_))));
        assert!(matches!(run(&fs, &cfg), Err(ImportError::JournalCorrupt(_))));
        assert_eq!(fs.ops(), 0);
    }
}

fn intent_path_len(r: &Record) -> usize {
    match &r.body {
        RecordBody::Intent(i) => i.source.path.as_os_str().len(),
        _ => unreachable!(),
    }
}

/// A final record whose payload length has a zero low byte, with one bit of
/// its complete header flipped, is corrupt: only an all-zero header is a
/// lost head. So is a final fixed-length record whose valid header is
/// followed by a payload that starts with a zero byte but fails its crc.
#[test]
fn journal_single_header_bit_flip_with_zero_low_length_byte_is_corrupt() {
    let cfg = Cfg::copy();
    let full = stamped(&full_journal(&cfg));
    let at = offsets(&full);
    let records = Journal::decode(&jpath(), &full).unwrap().records().to_vec();
    // An Intent and a Staged payload of exactly 1536 and 256 bytes.
    let mut intent = records[0].clone();
    let path_len = intent_path_len(&intent);
    let base = intent.encode().len() - path_len;
    let RecordBody::Intent(i) = &mut intent.body else {
        unreachable!()
    };
    let pad = 1536 - base - 1;
    i.source.path = PathBuf::from(format!("/{}", String::from_utf8(vec![b'p'; pad]).unwrap()));
    let staged = Record {
        at_ms: 1_700_000_000_002,
        body: RecordBody::Staged(vec![FileMethod {
            name: PathBuf::from("b".repeat(256 - 1 - 8 - 4 - 4 - 1 - 4 - 4)),
            method: Method::Copy,
            reflink_errno: 95,
            link_errno: 0,
        }]),
    };
    let intent_frame = frame(&intent.encode());
    let staged_frame = frame(&staged.encode());
    assert_eq!(&intent_frame[..4], &[0, 6, 0, 0]);
    assert_eq!(&staged_frame[..4], &[0, 1, 0, 0]);
    let mut with_intent = journal::MAGIC.to_vec();
    with_intent.extend_from_slice(&intent_frame);
    let mut with_staged = with_intent.clone();
    with_staged.extend_from_slice(&staged_frame);
    let intent_at = journal::MAGIC.len();
    let staged_at = with_intent.len();
    assert_eq!(
        Journal::decode(&jpath(), &with_staged).unwrap().phases(),
        [Phase::Intent, Phase::Staged]
    );
    // The reviewer's probe: a bit of the header crc, of the length's second
    // byte, and of the phase byte; then every other single bit of the header.
    let mut flips = vec![(5, 0x04), (1, 0x02), (8, 0x01)];
    flips.extend((0..9).flat_map(|byte| (0..8).map(move |bit| (byte, 1u8 << bit))));
    for (bytes, at_rec) in [(&with_intent, intent_at), (&with_staged, staged_at)] {
        for &(byte, bit) in &flips {
            let mut b = bytes.clone();
            b[at_rec + byte] ^= bit;
            assert!(
                matches!(Journal::decode(&jpath(), &b), Err(ImportError::JournalCorrupt(_))),
                "record at {at_rec}, byte {byte}, bit {bit:#x}"
            );
        }
    }
    // A final Placed record stamped with a zero low `at_ms` byte and bit rot
    // further into `at_ms`: the payload starts with zero, but not up to the
    // sector end or EOF.
    let placed = Record {
        at_ms: 0x0102_0304_0506_0700,
        body: RecordBody::Placed,
    };
    let mut bytes = full[..at[3]].to_vec();
    let placed_at = bytes.len();
    bytes.extend_from_slice(&frame(&placed.encode()));
    assert_eq!(Journal::decode(&jpath(), &bytes).unwrap().last(), Some(Phase::Placed));
    bytes[placed_at + 9 + 3] ^= 0x10;
    let fs = journal_image(&bytes);
    fs.reset_ops();
    assert!(matches!(Journal::open(&jpath(), &fs), Err(ImportError::JournalCorrupt(_))));
    assert!(matches!(run(&fs, &cfg), Err(ImportError::JournalCorrupt(_))));
    assert_eq!(fs.ops(), 0);
    assert_eq!(fs.read(&jpath()).unwrap(), bytes);
}

/// Known limits: a final append whose lost data is not one zero run from the
/// frame's head (a middle page lost out of order, or a lost page that ends
/// inside the header) refuses with the cleanup text and truncates nothing.
#[test]
fn journal_out_of_order_final_append_is_corrupt() {
    let cfg = Cfg::copy();
    let full = stamped(&full_journal(&cfg));
    let at = offsets(&full);
    // Intent's payload covers the sector [512, 1024): only that one is lost.
    let mut hole = full[..at[1]].to_vec();
    assert!(at[1] > 1024);
    hole[512..1024].fill(0);
    // A Staged header that starts 4 bytes before a sector boundary (a longer
    // source path moves it there), with the rest of that sector lost: the
    // header is only partly zero.
    let records = Journal::decode(&jpath(), &full).unwrap().records().to_vec();
    let mut intent = records[0].clone();
    let RecordBody::Intent(i) = &mut intent.body else {
        unreachable!()
    };
    let grow = sector_end(at[1]) - 4 - at[1];
    let mut path = i.source.path.as_os_str().to_owned();
    path.push("p".repeat(grow));
    i.source.path = PathBuf::from(path);
    let mut straddle = journal::MAGIC.to_vec();
    straddle.extend_from_slice(&frame(&intent.encode()));
    let staged_at = straddle.len();
    assert_eq!(staged_at % journal::SECTOR, journal::SECTOR - 4);
    straddle.extend_from_slice(&full[at[1]..at[2]]);
    assert_eq!(Journal::decode(&jpath(), &straddle).unwrap().last(), Some(Phase::Staged));
    straddle[staged_at..sector_end(staged_at)].fill(0);
    for bytes in [hole, straddle] {
        let fs = journal_image(&bytes);
        fs.reset_ops();
        let e = Journal::open(&jpath(), &fs).unwrap_err();
        assert!(e.to_string().contains("is corrupt; remove"), "{e}");
        assert!(matches!(run(&fs, &cfg), Err(ImportError::JournalCorrupt(_))));
        assert_eq!(fs.ops(), 0);
        fs.crash();
        assert_eq!(fs.read(&jpath()).unwrap(), bytes);
    }
}

/// A torn-tail scan over crafted overlapping headers stays linear: a 1 MiB
/// journal of a zero header and then back-to-back valid headers, each
/// claiming the rest of the file, refuses within a fixed number of crc steps.
#[test]
fn journal_torn_tail_scan_is_bounded_by_the_input() {
    const LEN: usize = 1 << 20;
    // Length, header crc and payload crc.
    const OVERHEAD: usize = 12;
    let mut raw = journal::MAGIC.to_vec();
    raw.extend([0u8; 9]);
    while raw.len() + OVERHEAD + 9 <= LEN {
        let len = ((LEN - raw.len() - OVERHEAD) as u32).to_le_bytes();
        raw.extend(len);
        raw.extend(journal::header_crc(len, Phase::Intent as u8).to_le_bytes());
        raw.push(Phase::Intent as u8);
    }
    raw.resize(LEN, 0);
    let limit = 16 * LEN as u64;
    journal::crc_steps::start(limit);
    let result = Journal::decode(&jpath(), &raw);
    let (calls, bytes) = journal::crc_steps::finish();
    assert!(matches!(result, Err(ImportError::JournalCorrupt(_))), "{result:?}");
    assert!(calls <= 2 * LEN as u64, "{calls} crc calls");
    assert!(bytes <= ((5 + journal::SCAN_FACTOR) * LEN + LEN) as u64, "{bytes} bytes hashed");
}

#[test]
fn cutfs_torn_head_zeroes_the_page_holding_the_old_end() {
    let cfg = Cfg::copy();
    let full = full_journal(&cfg);
    let at = offsets(&full);
    // The Intent append, from just after the magic, crosses a sector.
    let fs = journal_image(&full[..at[0]]);
    fs.append(&jpath(), &full[at[0]..at[1]]).unwrap();
    fs.crash_torn_head();
    let page_end = (at[0] / cutfs::PAGE + 1) * cutfs::PAGE;
    assert_eq!(cutfs::PAGE, journal::SECTOR);
    assert!(page_end < at[1], "the Intent frame crosses a page");
    let bytes = fs.read(&jpath()).unwrap();
    assert_eq!(&bytes[..at[0]], &full[..at[0]]);
    assert!(bytes[at[0]..page_end].iter().all(|&b| b == 0));
    assert_eq!(&bytes[page_end..], &full[page_end..at[1]]);
    assert_eq!(page_end % journal::SECTOR, 0);
    let j = Journal::open(&jpath(), &fs).unwrap();
    assert_eq!((j.last(), j.needs_repair()), (None, true));
    // A frame inside one page: the whole append reads as zeros.
    let fs = journal_image(&full[..at[1]]);
    fs.append(&jpath(), &full[at[1]..at[2]]).unwrap();
    fs.crash_torn_head();
    let bytes = fs.read(&jpath()).unwrap();
    assert!(bytes[at[1]..].iter().all(|&b| b == 0));
    let j = Journal::open(&jpath(), &fs).unwrap();
    assert_eq!((j.last(), j.needs_repair()), (Some(Phase::Intent), true));
}

#[test]
fn cutfs_removes_a_tree_entry_by_entry() {
    let tree = || {
        CutFs::image()
            .with_file("/a/b/x", b"x")
            .with_file("/a/b/y", b"y")
            .with_file("/a/b/c/z", b"z")
            .build()
    };
    let names_at = |fs: &CutFs, dir: &str| names(fs, Path::new(dir));
    // remove_dir_all unlinks z, c, x, y, then b: five calls.
    let fs = tree();
    fs.reset_ops();
    fs.remove(Path::new("/a/b")).unwrap();
    assert_eq!(fs.ops(), 5);
    // A cut inside the removal leaves a partly emptied tree.
    let fs = tree();
    fs.cut_after(3);
    assert!(fs.remove(Path::new("/a/b")).is_err());
    fs.crash_keep();
    assert_eq!(names_at(&fs, "/a/b"), ["y"]);
    // Unlinks are durable only once their dir is synced.
    let fs = tree();
    fs.remove(Path::new("/a/b")).unwrap();
    fs.crash();
    assert_eq!(names_at(&fs, "/a/b"), ["c", "x", "y"]);
    let fs = tree();
    fs.remove(Path::new("/a/b")).unwrap();
    fs.sync_dir(Path::new("/a")).unwrap();
    fs.crash();
    assert!(names_at(&fs, "/a").is_empty());
}

#[test]
fn arm_and_report_cut_matrix() {
    for cfg in [Cfg::reflink(), Cfg::hardlink()] {
        let base = fixture_for(&cfg);
        assert_eq!(run(&base, &cfg).unwrap(), Phase::Placed);
        base.crash();
        let p4_p5 = |fs: &CutFs| -> Result<ImportReport, ImportError> {
            let mut j = Journal::open(&jpath(), fs)?;
            if j.last() == Some(Phase::Placed) {
                arm_reindex(&mut j, &Arm(Ok(())), fs)?;
            }
            report(&mut j, outcome(TipRelation::Equal), fs)
        };
        let uncut = base.fork();
        uncut.reset_ops();
        assert_eq!(p4_p5(&uncut).unwrap().status, "complete");
        let n = uncut.ops();
        assert!(n >= 6, "append, sync, remove, append, sync, rename, sync, append, sync");
        uncut.crash();
        let want = dest_state(&uncut);
        for k in 1..=n {
            for variant in 0..VARIANTS {
                let fs = base.fork();
                fs.cut_after(k);
                let _ = p4_p5(&fs);
                assert!(fs.is_dead(), "k={k}");
                crash_variant(&fs, variant);
                match run(&fs, &cfg) {
                    Ok(Phase::Placed | Phase::ReindexArmed) => {
                        assert_eq!(p4_p5(&fs).unwrap().status, "complete", "k={k} v={variant}");
                    }
                    Err(ImportError::AlreadyImported {
                        ..
                    }) => {}
                    other => panic!("k={k} variant={variant}: {other:?}"),
                }
                fs.crash();
                assert_eq!(dest_state(&fs), want, "k={k} variant={variant}");
                assert_eq!(durable_journal(&fs).reported(), Some(ReportedStatus::Complete));
            }
        }
    }
}

// ---- OsImportFs ----

/// A 0700 tempdir holding a source with three blk files and an empty
/// destination.
fn os_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let net = dir.path().join("core/regtest");
    std::fs::create_dir_all(net.join("blocks/index")).unwrap();
    std::fs::create_dir_all(net.join("chainstate")).unwrap();
    for n in 0..3 {
        std::fs::write(net.join("blocks").join(blk_name(n)), blk_bytes(n)).unwrap();
    }
    std::fs::write(net.join("blocks/index/CURRENT"), b"x").unwrap();
    std::fs::write(net.join("chainstate/000001.ldb"), b"c").unwrap();
    std::fs::write(net.join(".lock"), b"").unwrap();
    let dest = dir.path().join("dest/regtest");
    std::fs::create_dir_all(dest.join("blocks")).unwrap();
    (dir, net, dest)
}

struct OsGuard(PathBuf);
impl SourceGuard for OsGuard {
    fn release(&self) -> Result<(), ImportError> {
        let j = Journal::open(&self.0, &OsImportFs::new())?;
        assert!(j.last() >= Some(Phase::SourceVerified));
        Ok(())
    }
}

/// Dies (every later call fails) right after the first `hard_link`.
struct DieAfterLink {
    inner: OsImportFs,
    dead: std::sync::atomic::AtomicBool,
}
macro_rules! forward {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
        $(fn $name(&self, $($arg: $ty),*) -> io::Result<$ret> {
            if self.dead.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(io::Error::other("died"));
            }
            self.inner.$name($($arg),*)
        })*
    };
}
impl ImportFs for DieAfterLink {
    fn reflink(&self, _: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.inner.hard_link(src, dst)?;
        self.dead.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    forward! {
        copy(src: &Path, dst: &Path) -> u64;
        sync_data(path: &Path) -> ();
        sync_dir(path: &Path) -> ();
        rename(src: &Path, dst: &Path) -> ();
        remove(path: &Path) -> ();
        truncate(path: &Path, len: u64) -> ();
        append(path: &Path, data: &[u8]) -> ();
        metadata(path: &Path) -> Option<FileMetadata>;
        read_dir(path: &Path) -> Vec<PathBuf>;
        read(path: &Path) -> Vec<u8>;
        create_dir_all(path: &Path) -> ();
        available_space(path: &Path) -> u64;
        canonicalize(path: &Path) -> PathBuf;
        mount_table() -> Option<Vec<u8>>;
        mount_id(path: &Path) -> Option<u64>;
        sha256(path: &Path) -> [u8; 32];
    }
}

/// OsImportFs with FICLONE failing, so hardlink mode really links.
struct NoReflink(OsImportFs);
impl ImportFs for NoReflink {
    fn reflink(&self, _: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))
    }
    fn hard_link(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.0.hard_link(a, b)
    }
    fn copy(&self, a: &Path, b: &Path) -> io::Result<u64> {
        self.0.copy(a, b)
    }
    fn sync_data(&self, p: &Path) -> io::Result<()> {
        self.0.sync_data(p)
    }
    fn sync_dir(&self, p: &Path) -> io::Result<()> {
        self.0.sync_dir(p)
    }
    fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.0.rename(a, b)
    }
    fn remove(&self, p: &Path) -> io::Result<()> {
        self.0.remove(p)
    }
    fn truncate(&self, p: &Path, l: u64) -> io::Result<()> {
        self.0.truncate(p, l)
    }
    fn append(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.0.append(p, d)
    }
    fn metadata(&self, p: &Path) -> io::Result<Option<FileMetadata>> {
        self.0.metadata(p)
    }
    fn read_dir(&self, p: &Path) -> io::Result<Vec<PathBuf>> {
        self.0.read_dir(p)
    }
    fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        self.0.read(p)
    }
    fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        self.0.create_dir_all(p)
    }
    fn available_space(&self, p: &Path) -> io::Result<u64> {
        self.0.available_space(p)
    }
    fn canonicalize(&self, p: &Path) -> io::Result<PathBuf> {
        self.0.canonicalize(p)
    }
    fn mount_table(&self) -> io::Result<Option<Vec<u8>>> {
        self.0.mount_table()
    }
    fn mount_id(&self, p: &Path) -> io::Result<Option<u64>> {
        self.0.mount_id(p)
    }
    fn sha256(&self, p: &Path) -> io::Result<[u8; 32]> {
        self.0.sha256(p)
    }
}

struct NoRelease;
impl SourceGuard for NoRelease {
    fn release(&self) -> Result<(), ImportError> {
        panic!("hardlink mode must not release the lock");
    }
}

#[test]
fn os_hardlink_crash_before_staged_leaves_source_intact() {
    let (_dir, net, dest) = os_fixture();
    let src = SourceInspection::from_parts(
        net.clone(),
        (0..3).map(|n| (n, net.join("blocks").join(blk_name(n)))).collect(),
        Some(BEST),
        Some(2),
    );
    let options = Cfg::hardlink().options;
    let dying = DieAfterLink {
        inner: OsImportFs::new(),
        dead: false.into(),
    };
    assert!(
        stage(&src, options, net.parent().unwrap(), Some("regtest"), &dest, &dying, &NoRelease)
            .is_err()
    );
    let linked = dest.join(STAGING).join("blocks").join(blk_name(0));
    assert_eq!(std::fs::read(&linked).unwrap(), blk_bytes(0));
    let fs = NoReflink(OsImportFs::new());
    assert!(preflight_destination(&dest, &net, &fs).unwrap().is_some());
    assert_eq!(
        stage(&src, options, net.parent().unwrap(), Some("regtest"), &dest, &fs, &NoRelease)
            .unwrap(),
        Phase::Placed
    );
    for n in 0..3 {
        let p = net.join("blocks").join(blk_name(n));
        assert_eq!(std::fs::read(&p).unwrap(), blk_bytes(n), "source {n} changed");
        assert_eq!(fs.metadata(&p).unwrap().unwrap().len, 96);
    }
    use std::os::unix::fs::MetadataExt;
    assert_eq!(std::fs::metadata(net.join("blocks").join(blk_name(0))).unwrap().nlink(), 2);
    assert_eq!(std::fs::metadata(net.join("blocks").join(blk_name(2))).unwrap().nlink(), 1);
}

#[test]
fn os_reflink_or_copy_places_every_file() {
    let (_dir, net, dest) = os_fixture();
    let src = SourceInspection::from_parts(
        net.clone(),
        (0..3).map(|n| (n, net.join("blocks").join(blk_name(n)))).collect(),
        Some(BEST),
        Some(2),
    );
    let fs = OsImportFs::new();
    let guard = OsGuard(dest.join(STAGING).join(JOURNAL));
    assert_eq!(
        stage(
            &src,
            Cfg::reflink().options,
            net.parent().unwrap(),
            Some("regtest"),
            &dest,
            &fs,
            &guard
        )
        .unwrap(),
        Phase::Placed
    );
    let j = Journal::open(&dest.join(STAGING).join(JOURNAL), &fs).unwrap();
    assert!(
        j.methods().unwrap().iter().all(|m| matches!(m.method, Method::Reflink | Method::Copy))
    );
    for n in 0..3 {
        assert_eq!(std::fs::read(dest.join("blocks").join(blk_name(n))).unwrap(), blk_bytes(n));
    }
}

#[test]
fn os_symlinked_import_staging_is_refused() {
    let (dir, net, dest) = os_fixture();
    let src = SourceInspection::from_parts(
        net.clone(),
        (0..3).map(|n| (n, net.join("blocks").join(blk_name(n)))).collect(),
        Some(BEST),
        Some(2),
    );
    let fs = OsImportFs::new();
    let staging = dest.join(STAGING);
    std::os::unix::fs::symlink(net.join("blocks"), &staging).unwrap();
    let guard = OsGuard(staging.join(JOURNAL));
    assert!(matches!(
        preflight_destination(&dest, &net, &fs),
        Err(ImportError::DestinationSymlink(p)) if p == staging
    ));
    assert!(matches!(
        stage(
            &src,
            Cfg::reflink().options,
            net.parent().unwrap(),
            Some("regtest"),
            &dest,
            &fs,
            &guard
        ),
        Err(ImportError::DestinationSymlink(_))
    ));
    std::fs::remove_file(&staging).unwrap();
    std::fs::create_dir(&staging).unwrap();
    let blk = net.join("blocks").join(blk_name(0));
    std::os::unix::fs::symlink(&blk, staging.join(JOURNAL)).unwrap();
    assert!(matches!(
        Journal::open(&staging.join(JOURNAL), &fs),
        Err(ImportError::DestinationSymlink(_))
    ));
    // The OS calls themselves never follow a final symlink.
    let link = staging.join(JOURNAL);
    assert_eq!(fs.append(&link, b"x").unwrap_err().raw_os_error(), Some(libc::ELOOP));
    assert_eq!(fs.truncate(&link, 0).unwrap_err().raw_os_error(), Some(libc::ELOOP));
    assert_eq!(fs.read(&link).unwrap_err().raw_os_error(), Some(libc::ELOOP));
    assert_eq!(fs.sync_data(&link).unwrap_err().raw_os_error(), Some(libc::ELOOP));
    for n in 0..3 {
        assert_eq!(std::fs::read(net.join("blocks").join(blk_name(n))).unwrap(), blk_bytes(n));
    }
    drop(dir);
}

#[test]
fn os_copy_and_reflink_never_open_an_existing_name() {
    let (_dir, net, dest) = os_fixture();
    let fs = OsImportFs::new();
    let blk0 = net.join("blocks").join(blk_name(0));
    let blk1 = net.join("blocks").join(blk_name(1));
    let linked = dest.join("blocks").join(blk_name(0));
    std::fs::hard_link(&blk0, &linked).unwrap();
    assert_eq!(fs.copy(&blk1, &linked).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs.reflink(&blk1, &linked).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&blk0).unwrap(), blk_bytes(0));
    assert_eq!(std::fs::read(&linked).unwrap(), blk_bytes(0));
    use std::os::unix::fs::MetadataExt;
    assert_eq!(std::fs::metadata(&blk0).unwrap().nlink(), 2);
}

struct FailRelease;
impl SourceGuard for FailRelease {
    fn release(&self) -> Result<(), ImportError> {
        Err(ImportError::InvalidOptions("the lock release failed".into()))
    }
}

fn os_inspection(net: &Path) -> SourceInspection {
    SourceInspection::from_parts(
        net.to_owned(),
        (0..3).map(|n| (n, net.join("blocks").join(blk_name(n)))).collect(),
        Some(BEST),
        Some(2),
    )
}

#[test]
fn os_symlinked_network_subdir_stages_and_resumes() {
    let (dir, net, dest) = os_fixture();
    // ~/.dashcore/regtest -> /disk2/rt-data
    let real = dir.path().join("disk2/rt-data");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::rename(&net, &real).unwrap();
    std::os::unix::fs::symlink(&real, &net).unwrap();
    let base = net.parent().unwrap().to_owned();
    let fs = OsImportFs::new();
    let jp = dest.join(STAGING).join(JOURNAL);
    let options = Cfg::reflink().options;
    let src = os_inspection(&source_net_dir(&base, Some("regtest"), &fs).unwrap());
    assert!(preflight_destination(&dest, &net, &fs).unwrap().is_none());
    // The real dir's parent holds no regtest dir, so it is not the datadir.
    let real_parent = std::fs::canonicalize(real.parent().unwrap()).unwrap();
    assert!(matches!(
        stage(&src, options, &real_parent, Some("regtest"), &dest, &fs, &FailRelease),
        Err(ImportError::InvalidOptions(_))
    ));
    assert!(fs.metadata(&dest.join(STAGING)).unwrap().is_none());
    // The process stops right after SourceVerified.
    assert!(stage(&src, options, &base, Some("regtest"), &dest, &fs, &FailRelease).is_err());
    let j = preflight_destination(&dest, &net, &fs).unwrap().unwrap();
    assert_eq!(j.last(), Some(Phase::SourceVerified));
    let canonical_base = std::fs::canonicalize(&base).unwrap();
    assert_eq!(j.intent().unwrap().datadir, canonical_base);
    assert_eq!(j.intent().unwrap().source.path, std::fs::canonicalize(&real).unwrap());
    // Follow the ImportInProgress text.
    let Err(ImportError::ImportInProgress(told)) = import_start(&j, None, Some("regtest"), &fs)
    else {
        panic!("expected ImportInProgress");
    };
    assert_eq!(told, canonical_base);
    assert_eq!(
        import_start(&j, Some(&told), Some("regtest"), &fs).unwrap(),
        Some(Phase::SourceVerified)
    );
    let src = os_inspection(&source_net_dir(&told, Some("regtest"), &fs).unwrap());
    assert_eq!(
        stage(&src, options, &told, Some("regtest"), &dest, &fs, &OsGuard(jp.clone())).unwrap(),
        Phase::Placed
    );
    for n in 0..3 {
        assert_eq!(std::fs::read(dest.join("blocks").join(blk_name(n))).unwrap(), blk_bytes(n));
    }
}

#[test]
fn os_mainnet_parent_of_the_network_dir_is_refused() {
    let (dir, net, dest) = os_fixture();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let main_net = home.join(".dashcore");
    std::fs::rename(&net, &main_net).unwrap();
    let fs = OsImportFs::new();
    let options = Cfg::reflink().options;
    let src = os_inspection(&main_net);
    assert!(preflight_destination(&dest, &main_net, &fs).unwrap().is_none());
    let e = stage(&src, options, &home, None, &dest, &fs, &FailRelease).unwrap_err();
    assert!(matches!(e, ImportError::InvalidOptions(_)), "{e}");
    assert!(e.to_string().contains("does not hold the source network dir"), "{e}");
    assert!(fs.metadata(&dest.join(STAGING)).unwrap().is_none());
    let jp = dest.join(STAGING).join(JOURNAL);
    assert_eq!(
        stage(&src, options, &main_net, None, &dest, &fs, &OsGuard(jp.clone())).unwrap(),
        Phase::Placed
    );
    let j = Journal::open(&jp, &fs).unwrap();
    let canonical = std::fs::canonicalize(&main_net).unwrap();
    assert_eq!(j.intent().unwrap().datadir, canonical);
    assert!(matches!(
        import_start(&j, None, None, &fs),
        Err(ImportError::ImportInProgress(p)) if p == canonical
    ));
    assert_eq!(import_start(&j, Some(&main_net), None, &fs).unwrap(), Some(Phase::Placed));
}

#[test]
fn non_utf8_source_path_never_panics() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let base = dir.path().join(OsStr::from_bytes(b"core-\xff\xfe"));
    let net = base.join("regtest");
    std::fs::create_dir_all(net.join("chainstate")).unwrap();
    std::fs::create_dir_all(net.join("blocks")).unwrap();
    for n in 0..3 {
        std::fs::write(net.join("blocks").join(blk_name(n)), blk_bytes(n)).unwrap();
    }
    std::fs::write(net.join("chainstate/CURRENT"), b"x").unwrap();
    let dest = dir.path().join("dest/regtest");
    std::fs::create_dir_all(dest.join("blocks")).unwrap();
    let src = SourceInspection::from_parts(
        net.clone(),
        (0..3).map(|n| (n, net.join("blocks").join(blk_name(n)))).collect(),
        Some(BEST),
        Some(2),
    );
    let fs = OsImportFs::new();
    let jp = dest.join(STAGING).join(JOURNAL);
    let options = Cfg::reflink().options;
    assert!(preflight_destination(&dest, &net, &fs).unwrap().is_none());
    assert_eq!(
        stage(&src, options, &base, Some("regtest"), &dest, &fs, &OsGuard(jp.clone())).unwrap(),
        Phase::Placed
    );
    let mut j = preflight_destination(&dest, &net, &fs).unwrap().unwrap();
    let canonical_base = std::fs::canonicalize(&base).unwrap();
    let e = import_start(&j, None, Some("regtest"), &fs).unwrap_err();
    assert!(matches!(&e, ImportError::ImportInProgress(p) if p == &canonical_base));
    assert!(e.to_string().contains('\u{fffd}'), "{e}");
    arm_reindex(&mut j, &Arm(Ok(())), &fs).unwrap();
    let r = report(&mut j, outcome(TipRelation::Equal), &fs).unwrap();
    assert_eq!(r.status, "complete");
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dest.join("import-report.json")).unwrap()).unwrap();
    let canonical_net = std::fs::canonicalize(&net).unwrap();
    assert_eq!(json["source"], canonical_net.to_string_lossy().as_ref());
    assert_eq!(json["source_hex"], hex_of(canonical_net.as_os_str().as_bytes()));
    assert_eq!(json["datadir_hex"], hex_of(canonical_base.as_os_str().as_bytes()));
    assert_eq!(json["files"][0]["file_hex"], hex_of(blk_name(0).as_bytes()));
    let e =
        stage(&src, options, &base, Some("regtest"), &dest, &fs, &OsGuard(jp.clone())).unwrap_err();
    assert!(
        matches!(&e, ImportError::AlreadyImported { source_dir, .. } if source_dir == &canonical_base)
    );
    assert!(e.to_string().starts_with("already imported from "), "{e}");
    let j = Journal::open(&jp, &fs).unwrap();
    assert_eq!(import_start(&j, None, None, &fs).unwrap(), Some(Phase::Reported));
}

/// A dir with one file, made unreadable (mode 0000); 0700 again on drop, so
/// the tempdir can be removed.
struct Unreadable(PathBuf);
impl Unreadable {
    fn new(path: PathBuf) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("x"), b"x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        Self(path)
    }
    /// False when this process can list it anyway (root or CAP_DAC_READ_SEARCH).
    fn denies(&self) -> bool {
        std::fs::read_dir(&self.0).is_err()
    }
}
impl Drop for Unreadable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
    }
}

#[test]
fn os_unreadable_dirs_outside_the_import_scope_are_ignored() {
    let (_dir, net, dest) = os_fixture();
    let in_source = Unreadable::new(net.join("lost+found"));
    let in_dest = Unreadable::new(dest.join("lost+found"));
    let backups = Unreadable::new(net.join("backups"));
    if !(in_source.denies() && in_dest.denies() && backups.denies()) {
        eprintln!("skipped: this process can list mode-0000 dirs");
        return;
    }
    let fs = OsImportFs::new();
    assert!(preflight_destination(&dest, &net, &fs).unwrap().is_none());
    let guard = OsGuard(dest.join(STAGING).join(JOURNAL));
    let base = net.parent().unwrap();
    assert_eq!(
        stage(
            &os_inspection(&net),
            Cfg::reflink().options,
            base,
            Some("regtest"),
            &dest,
            &fs,
            &guard
        )
        .unwrap(),
        Phase::Placed
    );
    for n in 0..3 {
        assert_eq!(std::fs::read(dest.join("blocks").join(blk_name(n))).unwrap(), blk_bytes(n));
    }
}

#[test]
fn os_unreadable_dir_in_a_scoped_tree_is_refused_by_name() {
    use std::os::unix::ffi::OsStrExt;
    let (_dir, net, dest) = os_fixture();
    let sub = Unreadable::new(net.join("blocks/sub"));
    if !sub.denies() {
        eprintln!("skipped: this process can list mode-0000 dirs");
        return;
    }
    let fs = OsImportFs::new();
    let named = std::fs::canonicalize(&net).unwrap().join("blocks/sub");
    let path_hex = hex_of(named.as_os_str().as_bytes());
    let check = |e: ImportError| {
        assert!(format!("{e:?}").starts_with("UnreadableDir"), "{e:?}");
        let text = e.to_string();
        assert!(text.contains(&*named.to_string_lossy()), "{text}");
        assert!(text.contains(&path_hex), "{text}");
    };
    check(preflight_destination(&dest, &net, &fs).unwrap_err());
    let guard = OsGuard(dest.join(STAGING).join(JOURNAL));
    let base = net.parent().unwrap();
    check(
        stage(
            &os_inspection(&net),
            Cfg::reflink().options,
            base,
            Some("regtest"),
            &dest,
            &fs,
            &guard,
        )
        .unwrap_err(),
    );
    assert!(fs.metadata(&dest.join(STAGING)).unwrap().is_none());
}

#[test]
fn os_unsearchable_dirs_follow_the_scope_rule() {
    use std::os::unix::fs::PermissionsExt;
    // Mode 0400: the dir lists, but its children cannot be stat'ed.
    let unsearchable = |dir: PathBuf| {
        std::fs::create_dir_all(dir.join("child")).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o400)).unwrap();
        Unreadable(dir)
    };
    let (_dir, net, dest) = os_fixture();
    let backups = unsearchable(net.join("backups"));
    if std::fs::symlink_metadata(backups.0.join("child")).is_ok() {
        eprintln!("skipped: this process can search mode-0400 dirs");
        return;
    }
    assert!(std::fs::read_dir(&backups.0).is_ok());
    let fs = OsImportFs::new();
    assert!(preflight_destination(&dest, &net, &fs).unwrap().is_none());
    let sub = unsearchable(net.join("blocks/sub"));
    let child = std::fs::canonicalize(&net).unwrap().join("blocks/sub/child");
    assert_unreadable(preflight_destination(&dest, &net, &fs), &child.to_string_lossy());
    drop(sub);
}

#[test]
fn os_unreadable_store_dir_is_refused_by_name() {
    let (_dir, net, dest) = os_fixture();
    for store in STORE_DIRS {
        let dir = Unreadable::new(dest.join(store));
        if !dir.denies() {
            eprintln!("skipped: this process can list mode-0000 dirs");
            return;
        }
        let e = preflight_destination(&dest, &net, &OsImportFs::new()).unwrap_err();
        assert!(format!("{e:?}").starts_with("UnreadableDir"), "{e:?}");
        assert!(e.to_string().contains(&*dest.join(store).to_string_lossy()), "{e}");
        drop(dir);
        std::fs::remove_dir_all(dest.join(store)).unwrap();
    }
}

#[test]
fn module_doc_names_real_tests() {
    let doc: String = include_str!("mod.rs")
        .lines()
        .filter(|l| l.starts_with("//!"))
        .collect::<Vec<_>>()
        .join("\n");
    let sources = [include_str!("tests.rs"), include_str!("../../tests/datadir_import.rs")];
    let mut named = 0;
    for line in doc.lines().filter(|l| l.starts_with("//! |") || l.contains("cut_matrix")) {
        let col = if line.starts_with("//! |") {
            line.trim_end_matches('|').rsplit('|').next().unwrap()
        } else {
            line
        };
        for name in col.split('`').skip(1).step_by(2) {
            if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
                continue;
            }
            named += 1;
            let decl = format!("#[test]\nfn {name}() {{");
            assert!(
                sources.iter().any(|s| s.contains(&decl)),
                "the module doc names {name}, which is not a test fn"
            );
        }
    }
    assert!(named > 40, "only {named} test names found");
}

#[cfg(target_os = "linux")]
#[test]
fn os_mount_ids_locate_the_roots_in_the_mount_table() {
    let (_dir, net, dest) = os_fixture();
    let fs = OsImportFs::new();
    let table = mounts::parse(&fs.mount_table().unwrap().unwrap()).unwrap();
    for p in [Path::new("/"), net.as_path(), dest.as_path()] {
        let Some(id) = fs.mount_id(p).unwrap() else {
            eprintln!("skipped: this kernel gives no mount id");
            return;
        };
        let by_id = mounts::locate(&table, p, Some(id)).unwrap();
        assert_eq!(by_id.0.id, id);
        assert!(p.starts_with(&by_id.0.mount_point), "{}", p.display());
    }
    assert_eq!(fs.mount_id(&dest.join("missing")).unwrap_err().kind(), io::ErrorKind::NotFound);
    check_exclusion(&net, &dest, &fs).unwrap();
}
