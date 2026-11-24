// SPDX-License-Identifier: MIT

//! The basic block filter index through CH2's crash model (`dashd_chainstate::crash`): the real
//! data object, driver and `fltr?????.dat` files as the `blockfilter-` subject on the chainstate
//! SUT's synthetic blocks, files of two or three entries each, every crash point of the forward
//! run, both crash kinds.
//!
//! At each boot, R11c checks the store at L: every height row on L's path holds the uncrashed
//! run's filter hash and header for its block, no height row survives above L, and every archive
//! row holds its block's. The files hold, at each position a height row up to L or an archive row
//! references, that block's hash and filter, and `P` is at or past every such entry at or below
//! L. After the catch-up, the driver's reads of every block up to V's tip equal the uncrashed
//! run's.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use dash_chain::BlockRef;
use dash_consensus::coins::BlockUndo;
use dash_consensus::{Block, BlockId};
use dash_filters::FilterHeader;
use dash_primitives::BlockHash;
use dash_types::Numeric as _;
use dashd_chainstate::crash::{
    ChainConfig, ChainSut, CrashReader, CrashSubject, SUB_STORE, SubConfig, SubMode, SubjectFiles,
    ops, register_subject, synth,
};
use dashd_chainstate::subscriber::{
    BlockSource, FilePos, SubscriberData, SubscriberError, height_key,
};
use dashd_indexes::blockfilter::codec::{DbVal, HeightValue};
use dashd_indexes::blockfilter::data::{
    ARCHIVE, BLOCKFILTER_DB, BLOCKFILTER_ROWS, FILTER_POS, HEIGHT, META, decode_pos,
};
use dashd_indexes::blockfilter::files::encode_entry;
use dashd_indexes::blockfilter::{BlockFilterData, FilterFiles, FilterState, build_filter};
use dashd_store::faulty::{
    CrashKind, CrashWindow, EventSel, Fault, FaultKind, FaultOutcome, FaultPlan, FaultyEngine,
    LocatorKind, Mark, Mode, OpKind, Scenario, TraceEntry, World, explore, forward_trace,
};
use dashd_store::{Cf, DbSchema, KvRead};

const DIR: &str = "/dd/indexes/blockfilter/basic";
/// Two or three synthetic entries per file.
const FILE_SIZE: u64 = 160;
const CHUNK_SIZE: u64 = 64;
/// The files the sweeps name, more than they write: a harness bound. A fault on a later file is
/// not the subject's, so the oracle flags it.
const FILES: u32 = 24;

struct FilterSubject;

static SUBJECT: FilterSubject = FilterSubject;

fn files(world: &World) -> FilterFiles {
    FilterFiles::new(world.fs(), PathBuf::from(DIR)).with_sizes(FILE_SIZE, CHUNK_SIZE)
}

fn paths() -> &'static [&'static str] {
    static PATHS: OnceLock<Vec<&'static str>> = OnceLock::new();
    PATHS.get_or_init(|| {
        let f = FilterFiles::new(Arc::new(dashd_store::flatfile::OsFs::new()), PathBuf::from(DIR));
        (0..FILES).map(|n| &*f.path(n).to_string_lossy().into_owned().leak()).collect()
    })
}

fn synth_block(b: &BlockRef) -> Result<Arc<synth::GenBlock>, String> {
    synth::lookup(&b.hash).ok_or_else(|| format!("no synthetic block {}", b.hash))
}

struct SynthBlocks;

impl BlockSource for SynthBlocks {
    fn block(&self, b: &BlockRef) -> Result<Arc<Block>, SubscriberError> {
        let g = synth_block(b).map_err(SubscriberError::Blocks)?;
        Ok(Arc::new(Block::new(*g.block.header(), g.block.txs().to_vec())))
    }

    fn undo(&self, b: &BlockRef) -> Result<Vec<u8>, SubscriberError> {
        Ok(synth_block(b).map_err(SubscriberError::Blocks)?.undo.as_slice().to_vec())
    }
}

/// Genesis to the synthetic block `hash`.
fn synth_path(hash: &BlockHash) -> Result<Vec<BlockRef>, String> {
    let mut path = Vec::new();
    let mut at = *hash;
    loop {
        let g = synth::lookup(&at).ok_or_else(|| format!("no synthetic block {at}"))?;
        path.push(BlockRef {
            id: BlockId(g.height),
            hash: at,
            height: g.height,
        });
        if g.height == 0 {
            path.reverse();
            return Ok(path);
        }
        at = g.parent;
    }
}

/// An uncrashed run's filter and header for a block.
#[derive(Clone)]
struct Want {
    encoded: Vec<u8>,
    header: FilterHeader,
}

static REFERENCE: LazyLock<Mutex<HashMap<BlockHash, Want>>> = LazyLock::new(Default::default);

/// The uncrashed filter and header of the synthetic block `hash`.
fn want(hash: &BlockHash) -> Result<Want, String> {
    if let Some(w) = REFERENCE.lock().expect("unpoisoned").get(hash) {
        return Ok(w.clone());
    }
    let mut prev = FilterHeader::from_lendian([0; 32]);
    for b in synth_path(hash)? {
        if let Some(w) = REFERENCE.lock().expect("unpoisoned").get(&b.hash) {
            prev = w.header;
            continue;
        }
        let g = synth_block(&b)?;
        let undo = match b.height {
            0 => BlockUndo::default(),
            _ => BlockUndo::decode(&mut g.undo.as_slice()).map_err(|e| e.to_string())?,
        };
        let filter = build_filter(&g.block, &undo).map_err(|e| e.to_string())?;
        let w = Want {
            header: filter.header(&prev),
            encoded: filter.encoded().to_vec(),
        };
        prev = w.header;
        REFERENCE.lock().expect("unpoisoned").insert(b.hash, w);
    }
    REFERENCE.lock().expect("unpoisoned").get(hash).cloned().ok_or("no reference".into())
}

fn check_value(hash: &BlockHash, v: &DbVal, what: &str) -> Option<String> {
    let w = match want(hash) {
        Ok(w) => w,
        Err(e) => return Some(e),
    };
    (v.header != w.header || v.filter_hash != dash_filters::filter_hash(&w.encoded))
        .then(|| format!("{what} of {hash}: {v:?}"))
}

/// Every height row on `path` names its block with the uncrashed hash and header.
fn check_rows(
    path: &[BlockRef],
    read: impl Fn(u32) -> Result<Option<Vec<u8>>, String>,
) -> Option<String> {
    path.iter().find_map(|b| {
        let row = match read(b.height) {
            Ok(Some(row)) => row,
            got => return Some(format!("the height row at {}: {got:?}", b.height)),
        };
        match HeightValue::decode(&row) {
            Ok(v) if v.block_hash == b.hash => {
                check_value(&b.hash, &v.filter, &format!("the height row at {}", b.height))
            }
            got => Some(format!("the height row at {}: {got:?}", b.height)),
        }
    })
}

/// The archive rows: their blocks and values.
fn archive(raw: &FaultyEngine) -> Result<Vec<(BlockHash, DbVal)>, String> {
    raw.iter_prefix(ARCHIVE, &[])
        .map(|row| {
            let (k, v) = row.map_err(|e| format!("a read: {e}"))?;
            let hash: [u8; 32] =
                k.as_slice().try_into().map_err(|_| format!("an archive key of {}", k.len()))?;
            let v = DbVal::decode(&v).map_err(|e| format!("an archive row: {e}"))?;
            Ok((BlockHash::from(hash), v))
        })
        .collect()
}

fn pos(v: &DbVal) -> FilePos {
    FilePos {
        file: v.pos.file as u32,
        offset: v.pos.pos,
    }
}

/// The filter files at L.
struct FilterFileFaults(FilterFiles);

impl SubjectFiles for FilterFileFaults {
    /// Each referenced entry reads back as its block's, `P` is past every entry at or below L,
    /// and no two blocks share a position.
    fn check_at_l(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
        let mut refs = Vec::new();
        for b in path {
            match raw.get(HEIGHT, &height_key(b.height, &[])) {
                Ok(Some(row)) => match HeightValue::decode(&row) {
                    Ok(v) => refs.push((b.hash, v.filter, true)),
                    Err(e) => return Some(e.to_string()),
                },
                got => return Some(format!("the height row at {}: {got:?}", b.height)),
            }
        }
        match archive(raw) {
            Ok(a) => refs.extend(a.into_iter().map(|(h, v)| (h, v, false))),
            Err(e) => return Some(e),
        }
        let p = match raw.get(META, FILTER_POS) {
            Ok(Some(p)) => decode_pos(&p).map_err(|e| e.to_string()),
            got => Err(format!("P: {got:?}")),
        };
        let p = match p {
            Ok(p) => p,
            Err(e) => return Some(e),
        };
        let mut seen = HashMap::new();
        for (hash, v, at_or_below_l) in refs {
            let at = pos(&v);
            let w = match want(&hash) {
                Ok(w) => w,
                Err(e) => return Some(e),
            };
            match self.0.read(at) {
                Ok((h, encoded)) if h == hash && encoded == w.encoded => {}
                got => return Some(format!("the entry of {hash} at {at:?}: {got:?}")),
            }
            let end = FilePos {
                offset: at.offset + encode_entry(&hash, &w.encoded).len() as u64,
                ..at
            };
            if at_or_below_l && end > p {
                return Some(format!("P {p:?} below the entry of {hash} ending at {end:?}"));
            }
            if seen.insert(at, hash).is_some_and(|other| other != hash) {
                return Some(format!("two blocks' rows reference {at:?}"));
            }
        }
        None
    }
}

impl CrashSubject for FilterSubject {
    type State = FilterState;
    type Files = FilterFileFaults;

    fn prefix(&self) -> &'static str {
        "blockfilter-"
    }
    fn name(&self) -> &'static str {
        "basic block filter index"
    }
    fn locator(&self) -> (Cf, &'static [u8]) {
        BLOCKFILTER_ROWS.locator
    }
    fn db(&self) -> &'static DbSchema {
        &BLOCKFILTER_DB
    }
    fn data(
        &self,
        world: &World,
    ) -> Result<Box<dyn SubscriberData<E = FaultyEngine, State = FilterState>>, SubscriberError>
    {
        let engine = world.engine(SUB_STORE, &BLOCKFILTER_DB);
        let data = BlockFilterData::<FaultyEngine>::new(&*engine, world.fs(), PathBuf::from(DIR))
            .map_err(|e| SubscriberError::Data(e.to_string()))?;
        Ok(Box::new(data.with_file_sizes(FILE_SIZE, CHUNK_SIZE)))
    }
    fn blocks(&self) -> Arc<dyn BlockSource> {
        Arc::new(SynthBlocks)
    }
    fn files(&self, world: &World) -> Option<FilterFileFaults> {
        Some(FilterFileFaults(files(world)))
    }
    fn file_paths(&self) -> &'static [&'static str] {
        paths()
    }
    /// Every height row up to L, and every archive row, holds the uncrashed hash and header.
    fn check_at_l(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
        if let Some(e) =
            check_rows(path, |h| raw.get(HEIGHT, &height_key(h, &[])).map_err(|e| e.to_string()))
        {
            return Some(e);
        }
        match archive(raw) {
            Ok(a) => a.iter().find_map(|(h, v)| check_value(h, v, "the archive row")),
            Err(e) => Some(e),
        }
    }
    /// No height row above L after the open's blind rewind.
    fn check_rewound(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
        let l = path.last()?.height;
        raw.iter_prefix(HEIGHT, &[]).find_map(|row| match row {
            Ok((k, _)) => {
                let h = u32::from_be_bytes(k[..4].try_into().expect("4 bytes"));
                (h > l).then(|| format!("a height row at {h} above L = {l}"))
            }
            Err(e) => Some(format!("a read: {e}")),
        })
    }
    /// The driver's reads of every block up to V's tip equal the uncrashed run's.
    fn check_tip(&self, reader: &dyn CrashReader, path: &[BlockRef]) -> Option<String> {
        check_rows(path, |h| reader.read(HEIGHT, &height_key(h, &[])).map_err(|e| e.to_string()))
    }
}

fn scenario(name: &'static str, mode: SubMode, d_max: u32, operations: &str) -> Scenario<ChainSut> {
    register_subject(&SUBJECT);
    Scenario::new(
        name,
        ChainConfig {
            index: Some(SubConfig::with_subject(&SUBJECT, mode, d_max)),
            ..ChainConfig::default()
        },
        ops(operations),
    )
}

/// The forward run's appends: the file number of each, in order.
fn appended_files(trace: &dashd_store::faulty::Trace) -> Vec<(u64, usize)> {
    trace
        .entries
        .iter()
        .filter_map(|e| match e {
            TraceEntry::Event {
                id,
                op: OpKind::WriteAt,
                detail,
                ..
            } => Some((id.index, paths().iter().position(|p| p == detail)?)),
            _ => None,
        })
        .collect()
}

/// The exhaustive sweep: a clean forward run that commits the locator kinds `kinds` and rotates
/// through files, then every crash point with both crash kinds.
fn sweep(sc: &Scenario<ChainSut>, kinds: &[LocatorKind]) {
    assert_eq!(sc.window, CrashWindow::all());
    let (trace, violations) = forward_trace(sc);
    assert!(violations.is_empty(), "{}: forward: {violations:?}", sc.name);
    let written: HashSet<LocatorKind> = trace
        .marks()
        .filter_map(|m| match m {
            Mark::Locator {
                store,
                kind,
                ..
            } if *store == SUB_STORE => Some(*kind),
            _ => None,
        })
        .collect();
    for kind in kinds {
        assert!(written.contains(kind), "{}: no {kind:?} locator in {written:?}", sc.name);
    }
    let files: Vec<usize> = appended_files(&trace).into_iter().map(|a| a.1).collect();
    assert!(files.windows(2).any(|w| w[1] == w[0] + 1), "{}: no rotation: {files:?}", sc.name);
    assert!(files.iter().max() < Some(&(FILES as usize - 1)), "{}: {files:?}", sc.name);
    let report = explore(
        sc,
        Mode::Exhaustive {
            max_branches_per_image: 4096,
        },
    );
    eprintln!("{}: {}", sc.name, report.summary());
    assert!(report.is_clean(), "{}: {}", sc.name, report.summary());
    assert_eq!(report.truncated, 0);
    let events = trace.entries.iter().filter(|e| matches!(e, TraceEntry::Event { .. })).count();
    for kind in [CrashKind::ProcessKill, CrashKind::PowerLoss] {
        let images = report.per_image.iter().filter(|(_, k, ..)| *k == kind).count();
        assert_eq!(images, events, "{}: every crash point for {kind:?}", sc.name);
    }
}

/// Live from genesis: commits at each flush, and a reorg above L whose archives reference
/// entries in earlier files while the new branch appends into later ones.
#[test]
fn blockfilter_synced_crash_sweep() {
    sweep(
        &scenario(
            "blockfilter-synced",
            SubMode::Synced {
                backlog: 0,
            },
            96,
            "c2;f;c2;r2:3;f",
        ),
        &[LocatorKind::CommitStep],
    );
}

/// A reorg below L: the rewind commit archives (F, L] and writes the current `P`, and the
/// appends after it go past the archived entries.
#[test]
fn blockfilter_rewind_then_append_crash_sweep() {
    sweep(
        &scenario(
            "blockfilter-rewind",
            SubMode::Synced {
                backlog: 2,
            },
            96,
            "c4;f;r2:3;f;c1;f",
        ),
        &[LocatorKind::Rewind],
    );
}

/// A catch-up with `D_max` 3: entries refused at the limit are retried at the same position
/// across rotations, cadence commits and a reorg below L.
#[test]
fn blockfilter_syncing_rejected_then_retried_crash_sweep() {
    sweep(
        &scenario(
            "blockfilter-syncing",
            SubMode::Syncing {
                cadence: 5,
            },
            3,
            "c5;f;c5;s11;k;s2;r8:3;s20;f",
        ),
        &[LocatorKind::Cadence, LocatorKind::Rewind],
    );
}

/// File faults, each its own forward run swept at every crash point with both kinds: a filter
/// write not applied, torn after one byte, or applied before it failed, at the first append into
/// a rotated file and at the last; and a failed file sync. Each aborts with the subject's own
/// message, and recovery skips what the failed call left.
#[test]
fn blockfilter_file_faults_fail_closed() {
    let sc = |name| {
        scenario(
            name,
            SubMode::Synced {
                backlog: 0,
            },
            96,
            "c2;f;c2;r2:3;f",
        )
    };
    let (trace, _) = forward_trace(&sc("blockfilter-faults"));
    let appends = appended_files(&trace);
    let rotated = appends.iter().find(|a| a.1 == 1).expect("a rotation").0;
    let last = appends.last().expect("appends").0;
    let own = |detail: &str| paths().contains(&detail);
    let syncs: Vec<u64> = trace
        .entries
        .iter()
        .filter_map(|e| match e {
            TraceEntry::Event {
                id,
                op: OpKind::SyncData,
                detail,
                ..
            } if own(detail) => Some(id.index),
            _ => None,
        })
        .collect();
    let kinds: [(&str, Vec<u64>, &[FaultOutcome]); 2] = [
        (
            "write",
            vec![rotated, last],
            &[FaultOutcome::NotApplied, FaultOutcome::Partial(1), FaultOutcome::Applied],
        ),
        ("sync", syncs, &[FaultOutcome::NotApplied, FaultOutcome::Applied]),
    ];
    for (kind, at, outcomes) in kinds {
        assert!(!at.is_empty(), "no {kind} event");
        let mut sc = sc(kind);
        sc.faults = FaultPlan(
            at.iter()
                .flat_map(|&i| {
                    outcomes.iter().map(move |&outcome| Fault {
                        at: EventSel::Index(i),
                        kind: FaultKind::Io,
                        outcome,
                    })
                })
                .collect(),
        );
        let report = explore(
            &sc,
            Mode::Exhaustive {
                max_branches_per_image: 4096,
            },
        );
        eprintln!("blockfilter-faults {kind} ({} events): {}", at.len(), report.summary());
        assert!(report.is_clean(), "{kind}: {}", report.summary());
        assert_eq!(report.truncated, 0);
    }
}

/// The subject is registered, so its repro lines' `index=` forms parse back to it.
#[test]
fn blockfilter_index_forms_parse() {
    register_subject(&SUBJECT);
    for (form, mode) in [
        (
            "blockfilter-synced:0:96",
            SubMode::Synced {
                backlog: 0,
            },
        ),
        (
            "blockfilter-syncing:5:3",
            SubMode::Syncing {
                cadence: 5,
            },
        ),
    ] {
        let parsed: SubConfig = form.parse().expect(form);
        let d_max = if form.ends_with(":3") {
            3
        } else {
            96
        };
        assert_eq!(parsed, SubConfig::with_subject(&SUBJECT, mode, d_max), "{form}");
        assert_eq!(parsed.to_string(), form);
    }
}
