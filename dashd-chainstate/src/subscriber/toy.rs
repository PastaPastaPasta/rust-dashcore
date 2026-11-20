//! A toy subscriber with every row class (CH2 §2.9): the drivers' tests' and the CRASH suite's.
//!
//! - height: `h → hash ‖ sum`, one row per height (blockfilterindex's `DB_BLOCK_HEIGHT`);
//! - archive: `hash → hash ‖ sum`, for blocks that left the chain (`DB_BLOCK_HASH`);
//! - keyed: `last ‖ (h mod 2) → hash`, mutated per block and undone from the row two below
//!   (coinstatsindex-style keyed state, IX2);
//! - inexact: `hash → h`, never deleted (txindex);
//! - state: the count, the running sum (MuHash's role) and a monotone high-water mark (the flat
//!   file position's role, O-R2-M2).
//!
//! [`ToyFiles`] adds a staged write per block to numbered flat files, rotating to the next file
//! at a per-file limit, as blockfilterindex's `fltr?????.dat`.
//!
//! [`toy_expected`] recomputes every non-inexact row from the path to a tip, which the
//! checks compare with the database at L and with the driver's reads at p.

use std::io;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_chain::{BlockRef, Needs};
use dashd_store::flatfile::FlatFs;
use dashd_store::{Cf, CfProfile, CfSpec, DbSchema, KvEngine, KvRead};

use super::SubscriberError;
use super::data::{
    BlockRows, CfRange, ConnectedBlock, DisconnectedBlock, DurableBelow, FileAppend, FilePos,
    KeyValue, Row, RowSchema, StateRead, SubscriberData, height_key,
};
use crate::store::ProbeEngine;

pub static TOY_DB: DbSchema = DbSchema {
    name: "toyindex",
    version: 1,
    cfs: &[
        CfSpec::new("meta", CfProfile::Ordered),
        CfSpec::new("height", CfProfile::Ordered),
        CfSpec::new("archive", CfProfile::PointLookup),
        CfSpec::new("keyed", CfProfile::PointLookup),
        CfSpec::new("inexact", CfProfile::PointLookup),
    ],
};

pub const TOY_META: Cf = Cf::new(0);
pub const TOY_HEIGHT: Cf = Cf::new(1);
pub const TOY_ARCHIVE: Cf = Cf::new(2);
pub const TOY_KEYED: Cf = Cf::new(3);
pub const TOY_INEXACT: Cf = Cf::new(4);
const STATE: &[u8] = b"state";
/// The driver's locator key.
pub const TOY_BEST: &[u8] = b"best";

static TOY_ROWS: RowSchema = RowSchema {
    height_cfs: &[TOY_HEIGHT],
    archive_cfs: &[TOY_ARCHIVE],
    state_keys: &[(TOY_META, STATE)],
    keyed_cfs: &[TOY_KEYED],
    inexact_cfs: &[TOY_INEXACT],
    has_files: false,
    locator: (TOY_META, TOY_BEST),
};

/// The toy's locator-committed state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToyState {
    pub count: u64,
    pub sum: u64,
    /// The highest count for `ToyIndex`, or the next append's position for `ToyFiles`, packed
    /// by `pack`.
    pub high_water: u64,
}

/// A file position as one `u64`, `file << 32 | offset`, whose order is the positions' order for
/// offsets below 2^32.
pub fn pack(p: FilePos) -> u64 {
    debug_assert!(p.offset <= u64::from(u32::MAX), "a toy file offset of 4 GiB or more");
    (u64::from(p.file) << 32) | p.offset
}

/// [`pack`]'s inverse.
pub fn unpack(v: u64) -> FilePos {
    FilePos {
        file: (v >> 32) as u32,
        offset: v & u64::from(u32::MAX),
    }
}

fn with_floor(at: &ToyState, end: FilePos) -> ToyState {
    ToyState {
        high_water: at.high_water.max(pack(end)),
        ..*at
    }
}

impl ToyState {
    fn encode(&self) -> Vec<u8> {
        [self.count, self.sum, self.high_water].iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    fn decode(v: &[u8]) -> Option<Self> {
        let w = |i: usize| Some(u64::from_be_bytes(v.get(i * 8..i * 8 + 8)?.try_into().ok()?));
        (v.len() == 24).then_some(())?;
        Some(Self {
            count: w(0)?,
            sum: w(1)?,
            high_water: w(2)?,
        })
    }
}

fn part(b: &BlockRef) -> u64 {
    u64::from_le_bytes(b.hash.as_bytes()[..8].try_into().expect("8 bytes"))
}

fn height_value(b: &BlockRef, s: &ToyState) -> Vec<u8> {
    let mut v = b.hash.as_bytes().to_vec();
    v.extend_from_slice(&s.sum.to_be_bytes());
    v
}

/// The archive row a block that left the chain at state `s` has.
pub fn toy_archived(b: &BlockRef, s: &ToyState) -> (Vec<u8>, Vec<u8>) {
    (b.hash.as_bytes().to_vec(), height_value(b, s))
}

fn keyed_key(height: u32) -> Vec<u8> {
    vec![b'l', (height % 2) as u8]
}

/// The toy on an engine `E`. `start_height` 0: genesis is indexed.
#[derive(Debug)]
pub struct ToyIndex<E = ProbeEngine> {
    pub mandatory: bool,
    /// The file length `file_len` reports (the high-water floor at open).
    pub file_len: u64,
    engine: PhantomData<fn() -> E>,
}

impl<E> Clone for ToyIndex<E> {
    fn clone(&self) -> Self {
        Self::with(self.mandatory, self.file_len)
    }
}

impl<E> ToyIndex<E> {
    pub fn with(mandatory: bool, file_len: u64) -> Self {
        Self {
            mandatory,
            file_len,
            engine: PhantomData,
        }
    }

    pub fn mandatory() -> Self {
        Self::with(true, 0)
    }
}

/// A row the toy expects: its column family, key and value.
pub type ToyRow = (Cf, Vec<u8>, Vec<u8>);

/// Every height, keyed and state row for the chain `path` (genesis first), and the state.
pub fn toy_expected(path: &[BlockRef]) -> (Vec<ToyRow>, ToyState) {
    let mut rows = Vec::new();
    let mut s = ToyState::default();
    for b in path {
        s = step(s, b);
        rows.push((TOY_HEIGHT, height_key(b.height, &[]), height_value(b, &s)));
    }
    for b in path.iter().rev().take(2) {
        rows.push((TOY_KEYED, keyed_key(b.height), b.hash.as_bytes().to_vec()));
    }
    (rows, s)
}

/// The database's rows at L, checked against `path` (genesis to L): height rows at heights up
/// to L exactly the path's, keyed rows the path's, the state's count and sum the path's and its
/// high-water at least the count, and the locator L. Height rows above L are p's (Buffered as
/// processed) and [`toy_rows_above`]'s to check.
///
/// # Errors
///
/// The first mismatch.
pub fn toy_check_at_l(db: &impl KvRead, path: &[BlockRef]) -> Result<ToyState, String> {
    check_at_l(db, path, false)
}

fn check_at_l(db: &impl KvRead, path: &[BlockRef], files: bool) -> Result<ToyState, String> {
    let (want, s) = toy_expected(path);
    let l = path.last().ok_or("an empty path")?.height;
    let mut heights: Vec<KeyValue> = KvRead::iter_prefix(db, TOY_HEIGHT, &[])
        .filter(|r| r.as_ref().map_or(true, |(k, _)| k[..4] <= l.to_be_bytes()[..]))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let want_heights: Vec<KeyValue> =
        want.iter().filter(|r| r.0 == TOY_HEIGHT).map(|r| (r.1.clone(), r.2.clone())).collect();
    if files {
        for (_, value) in &mut heights {
            value.truncate(40);
        }
    }
    if heights != want_heights {
        return Err(format!(
            "height rows: {} in the DB, {} on the path to L",
            heights.len(),
            want_heights.len()
        ));
    }
    for (cf, k, v) in want.iter().filter(|r| r.0 == TOY_KEYED) {
        let got = db.get(*cf, k).map_err(|e| e.to_string())?;
        if got.as_deref() != Some(&v[..]) {
            return Err(format!("keyed row {k:?}: {got:?}, want {v:?}"));
        }
    }
    let got = db.get(TOY_META, STATE).map_err(|e| e.to_string())?;
    let got = got.as_deref().and_then(ToyState::decode).ok_or("no state")?;
    if (got.count, got.sum) != (s.count, s.sum) || got.high_water < got.count {
        return Err(format!("state {got:?}, want {s:?}"));
    }
    let best = db.get(TOY_META, TOY_BEST).map_err(|e| e.to_string())?;
    let tip = path.last().ok_or("an empty path")?;
    if best.as_deref().and_then(|v| v.get(5..)) != Some(tip.hash.as_bytes()) {
        return Err(format!("locator {best:?}, want {}", tip.hash));
    }
    Ok(got)
}

/// The heights of the height rows above `l`.
pub fn toy_rows_above(db: &impl KvRead, l: u32) -> Vec<u32> {
    KvRead::iter_prefix(db, TOY_HEIGHT, &[])
        .map(|r| u32::from_be_bytes(r.expect("reads").0[..4].try_into().expect("4 bytes")))
        .filter(|&h| h > l)
        .collect()
}

fn step(s: ToyState, b: &BlockRef) -> ToyState {
    let count = s.count + 1;
    ToyState {
        count,
        sum: s.sum.wrapping_add(part(b)),
        high_water: s.high_water.max(count),
    }
}

impl<E: KvEngine + 'static> SubscriberData for ToyIndex<E> {
    type E = E;
    type State = ToyState;

    fn name(&self) -> &'static str {
        "toyindex"
    }

    fn needs(&self) -> Needs {
        Needs::default()
    }

    fn mandatory(&self) -> bool {
        self.mandatory
    }

    fn start_height(&self) -> u32 {
        0
    }

    fn schema(&self) -> &'static RowSchema {
        &TOY_ROWS
    }

    fn initial_state(&self) -> ToyState {
        ToyState::default()
    }

    fn connect(
        &self,
        b: &ConnectedBlock<'_>,
        at_parent: &ToyState,
    ) -> Result<(BlockRows, ToyState), SubscriberError> {
        let s = step(*at_parent, &b.block);
        let hash = b.block.hash.as_bytes();
        let rows = vec![
            Row::Put(TOY_HEIGHT, height_key(b.block.height, &[]), height_value(&b.block, &s)),
            Row::Put(TOY_KEYED, keyed_key(b.block.height), hash.to_vec()),
            Row::Put(TOY_INEXACT, hash.to_vec(), b.block.height.to_be_bytes().to_vec()),
        ];
        Ok((
            BlockRows {
                rows,
                file: None,
            },
            s,
        ))
    }

    fn stage_disconnected(
        &self,
        b: &DisconnectedBlock<'_>,
        at_b: &ToyState,
        below: &DurableBelow<'_, Self::E>,
    ) -> Result<(Vec<Row>, ToyState), SubscriberError> {
        let h = b.block.height;
        let undo = match h.checked_sub(2) {
            Some(h2) => {
                let v = below.height_row(TOY_HEIGHT, h2, &[])?.ok_or_else(|| {
                    SubscriberError::Data(format!("no height row at {h2} below L"))
                })?;
                Row::Put(TOY_KEYED, keyed_key(h), v[..32].to_vec())
            }
            None => Row::Delete(TOY_KEYED, keyed_key(h)),
        };
        let parent = ToyState {
            count: at_b.count - 1,
            sum: at_b.sum.wrapping_sub(part(&b.block)),
            high_water: at_b.high_water,
        };
        Ok((vec![undo], parent))
    }

    fn archive(&self, height_rows: &[Row]) -> Vec<Row> {
        height_rows
            .iter()
            .filter_map(|r| match r {
                Row::Put(TOY_HEIGHT, _, v) => {
                    Some(Row::Put(TOY_ARCHIVE, v[..32].to_vec(), v.clone()))
                }
                _ => None,
            })
            .collect()
    }

    fn with_high_water(&self, at: &ToyState, latest: &ToyState) -> ToyState {
        ToyState {
            high_water: at.high_water.max(latest.high_water),
            ..*at
        }
    }

    fn with_file_floor(&self, at: &ToyState, end: FilePos) -> ToyState {
        with_floor(at, end)
    }

    fn encode_state(&self, s: &ToyState) -> Vec<Row> {
        vec![Row::Put(TOY_META, STATE.to_vec(), s.encode())]
    }

    fn decode_state(
        &self,
        rows: &[StateRead],
        _: &DurableBelow<'_, Self::E>,
    ) -> Result<ToyState, SubscriberError> {
        match rows {
            [(_, _, Some(v))] => {
                ToyState::decode(v).ok_or_else(|| SubscriberError::Data("a bad toy state".into()))
            }
            [(_, _, None)] => Err(SubscriberError::Data("a locator without its state".into())),
            _ => Err(SubscriberError::Data("the toy has one state key".into())),
        }
    }

    fn height_ranges(&self, lo_exclusive: u32, hi_inclusive: u32) -> Vec<CfRange> {
        let start = height_key(lo_exclusive.saturating_add(1), &[]);
        let end = match hi_inclusive.checked_add(1) {
            Some(h) => height_key(h, &[]),
            None => vec![0xff; 5],
        };
        if lo_exclusive >= hi_inclusive {
            return Vec::new();
        }
        vec![(TOY_HEIGHT, start, end)]
    }

    fn max_entry_bytes(&self) -> u64 {
        1024
    }

    fn file_len(&self) -> Result<FilePos, SubscriberError> {
        Ok(FilePos {
            file: 0,
            offset: self.file_len,
        })
    }
}

static TOY_FILE_ROWS: RowSchema = RowSchema {
    has_files: true,
    ..TOY_ROWS
};

/// A toy subscriber with a staged append-only file effect: file 0 at `path`, file `n` beside it
/// ([`ToyFiles::file_path`]).
pub struct ToyFiles<E = ProbeEngine> {
    base: ToyIndex<E>,
    fs: Arc<dyn FlatFs>,
    path: PathBuf,
    /// A file's byte limit: an append that would pass it goes to the next file (Core's
    /// `MAX_FLTR_FILE_SIZE`). `None`: one file.
    limit: Option<u64>,
    /// Appends write three bytes and fail ([`ToyFiles::fail_appends`]), shared by the clones.
    fail_append: Arc<AtomicBool>,
    /// File syncs fail ([`ToyFiles::fail_syncs`]), shared by the clones.
    fail_sync: Arc<AtomicBool>,
}

impl<E> Clone for ToyFiles<E> {
    fn clone(&self) -> Self {
        Self {
            base: self.base.clone(),
            fs: Arc::clone(&self.fs),
            path: self.path.clone(),
            limit: self.limit,
            fail_append: Arc::clone(&self.fail_append),
            fail_sync: Arc::clone(&self.fail_sync),
        }
    }
}

impl<E> ToyFiles<E> {
    pub fn new(fs: Arc<dyn FlatFs>, path: PathBuf) -> Self {
        Self {
            base: ToyIndex::mandatory(),
            fs,
            path,
            limit: None,
            fail_append: Arc::default(),
            fail_sync: Arc::default(),
        }
    }

    /// From now on, this toy's appends and its clones' write three bytes and fail (`on`), or
    /// succeed.
    pub fn fail_appends(&self, on: bool) {
        self.fail_append.store(on, Ordering::Relaxed);
    }

    /// From now on, this toy's file syncs and its clones' fail (`on`), or succeed.
    pub fn fail_syncs(&self, on: bool) {
        self.fail_sync.store(on, Ordering::Relaxed);
    }

    fn sync_data(&self, n: u32) -> Result<(), SubscriberError> {
        if self.fail_sync.load(Ordering::Relaxed) {
            return Err(SubscriberError::Data("toy file sync failed".into()));
        }
        self.fs.sync_data(&self.file_path(n)).map_err(Self::io)
    }

    /// Files of at most `limit` bytes each.
    pub fn rotating(fs: Arc<dyn FlatFs>, path: PathBuf, limit: u64) -> Self {
        Self {
            limit: Some(limit),
            ..Self::new(fs, path)
        }
    }

    /// File `n`'s path: `path` for file 0, `<stem>.<n>.<ext>` beside it for the others.
    pub fn file_path(&self, n: u32) -> PathBuf {
        toy_file_path(&self.path, n)
    }

    fn io(e: io::Error) -> SubscriberError {
        SubscriberError::Data(format!("toy file: {e}"))
    }

    /// Logged and ignored on failure, as Core's `DirectoryCommit` ignores one and DEC-212 has it
    /// for the block files. Core's filter index never syncs its directory, so the toy is at least
    /// as strict as Core.
    // Core v24.0.0: util/fs_helpers.cpp:153-166
    fn sync_dir(&self) {
        if let Some(parent) = self.path.parent()
            && let Err(e) = self.fs.sync_dir(parent)
        {
            tracing::warn!("toy file directory sync of {}: {e}", parent.display());
        }
    }

    fn len(&self, n: u32) -> Result<Option<u64>, SubscriberError> {
        self.fs.len(&self.file_path(n)).map_err(Self::io)
    }

    /// Where the next block's bytes go after `at`: at `at`, or at the next file's start when
    /// they would pass the limit.
    // Core v24.0.0: index/blockfilterindex.cpp:228-246
    fn start(&self, at: FilePos, len: u64) -> FilePos {
        // The toy's own: Core rotates whenever the entry does not fit, at offset 0 too.
        match self.limit {
            Some(limit) if at.offset > 0 && at.offset + len > limit => FilePos {
                file: at.file + 1,
                offset: 0,
            },
            _ => at,
        }
    }

    pub fn check_at_l(&self, db: &impl KvRead, path: &[BlockRef]) -> Result<ToyState, String> {
        let state = check_at_l(db, path, true)?;
        self.check_files(db)?;
        Ok(state)
    }

    /// Every height and archive row's bytes read back from the file position it references.
    ///
    /// # Errors
    ///
    /// The first row whose bytes differ or do not read.
    pub fn check_files(&self, db: &impl KvRead) -> Result<(), String> {
        for cf in [TOY_HEIGHT, TOY_ARCHIVE] {
            for row in db.iter_prefix(cf, &[]) {
                let (_, value) = row.map_err(|e| e.to_string())?;
                let at = value
                    .get(40..48)
                    .and_then(|v| v.try_into().ok())
                    .map(|v| unpack(u64::from_be_bytes(v)))
                    .ok_or("no toy file position")?;
                let mut bytes = [0; 40];
                self.fs
                    .read_exact_at(&self.file_path(at.file), at.offset, &mut bytes)
                    .map_err(|e| format!("toy file bytes at {at:?}: {e}"))?;
                if value.get(..40) != Some(bytes.as_slice()) {
                    return Err(format!("toy file bytes differ at {at:?}"));
                }
            }
        }
        Ok(())
    }
}

/// [`ToyFiles::file_path`] for file 0 at `path`.
pub fn toy_file_path(path: &Path, n: u32) -> PathBuf {
    if n == 0 {
        return path.to_path_buf();
    }
    let ext = path.extension().map_or(String::new(), |e| format!(".{}", e.to_string_lossy()));
    path.with_extension(format!("{n}{ext}"))
}

impl<E: KvEngine + 'static> SubscriberData for ToyFiles<E> {
    type E = E;
    type State = ToyState;

    fn name(&self) -> &'static str {
        "toyfiles"
    }

    fn needs(&self) -> Needs {
        Needs::default()
    }

    fn mandatory(&self) -> bool {
        self.base.mandatory
    }

    fn start_height(&self) -> u32 {
        0
    }

    fn schema(&self) -> &'static RowSchema {
        &TOY_FILE_ROWS
    }

    fn initial_state(&self) -> ToyState {
        ToyState::default()
    }

    fn connect(
        &self,
        b: &ConnectedBlock<'_>,
        at_parent: &ToyState,
    ) -> Result<(BlockRows, ToyState), SubscriberError> {
        let (mut rows, mut state) = self.base.connect(b, at_parent)?;
        let bytes = height_value(&b.block, &state);
        let at = self.start(unpack(at_parent.high_water), bytes.len() as u64);
        for row in &mut rows.rows {
            if let Row::Put(TOY_HEIGHT, _, value) = row {
                value.extend_from_slice(&pack(at).to_be_bytes());
            }
        }
        let append = FileAppend {
            file: at.file,
            offset: at.offset,
            bytes,
        };
        state.high_water = pack(append.end());
        rows.file = Some(append);
        Ok((rows, state))
    }

    fn stage_disconnected(
        &self,
        b: &DisconnectedBlock<'_>,
        at_b: &ToyState,
        below: &DurableBelow<'_, Self::E>,
    ) -> Result<(Vec<Row>, ToyState), SubscriberError> {
        self.base.stage_disconnected(b, at_b, below)
    }

    fn archive(&self, height_rows: &[Row]) -> Vec<Row> {
        self.base.archive(height_rows)
    }

    fn with_high_water(&self, at: &ToyState, latest: &ToyState) -> ToyState {
        ToyState {
            high_water: at.high_water.max(latest.high_water),
            ..*at
        }
    }

    fn with_file_floor(&self, at: &ToyState, end: FilePos) -> ToyState {
        with_floor(at, end)
    }

    fn file_pos(&self, s: &ToyState) -> Option<FilePos> {
        Some(unpack(s.high_water))
    }

    fn encode_state(&self, s: &ToyState) -> Vec<Row> {
        self.base.encode_state(s)
    }

    fn decode_state(
        &self,
        rows: &[StateRead],
        below: &DurableBelow<'_, Self::E>,
    ) -> Result<ToyState, SubscriberError> {
        self.base.decode_state(rows, below)
    }

    fn height_ranges(&self, lo_exclusive: u32, hi_inclusive: u32) -> Vec<CfRange> {
        self.base.height_ranges(lo_exclusive, hi_inclusive)
    }

    fn max_entry_bytes(&self) -> u64 {
        1024
    }

    fn file_size(&self, n: u32) -> Result<Option<u64>, SubscriberError> {
        self.len(n)
    }

    fn append_file(&mut self, append: &FileAppend) -> Result<(), SubscriberError> {
        let end = self.file_len()?;
        if append.file > end.file + 1 || (append.file > 0 && self.limit.is_none()) {
            return Err(SubscriberError::Data(format!("toy file {} skips a file", append.file)));
        }
        if append.offset < self.len(append.file)?.unwrap_or(0) {
            return Err(SubscriberError::Data("toy file append would overwrite bytes".into()));
        }
        let path = self.file_path(append.file);
        // Core commits the last file before it moves to the next; the next one's entry is
        // durable before its bytes, so no file outlives the one before it (`file_len`'s scan).
        if append.file > end.file {
            self.sync_data(end.file)?;
            self.fs.create(&path).map_err(Self::io)?;
            self.sync_dir();
        }
        if self.fail_append.load(Ordering::Relaxed) {
            self.fs
                .write_all_at(&path, append.offset, &append.bytes[..append.bytes.len().min(3)])
                .map_err(Self::io)?;
            return Err(SubscriberError::Data("toy file append failed".into()));
        }
        self.fs.write_all_at(&path, append.offset, &append.bytes).map_err(Self::io)
    }

    /// The last file and the directory, once the files hold any bytes.
    fn sync_files(&mut self) -> Result<(), SubscriberError> {
        let end = self.file_len()?;
        if end != FilePos::default() {
            self.sync_data(end.file)?;
            self.sync_dir();
        }
        Ok(())
    }

    /// The last file that exists, and its length: rotation leaves no gap (`append_file`).
    fn file_len(&self) -> Result<FilePos, SubscriberError> {
        let mut end = FilePos::default();
        let mut n = 0;
        while let Some(len) = self.len(n)? {
            end = FilePos {
                file: n,
                offset: len,
            };
            n += 1;
            if self.limit.is_none() {
                break;
            }
        }
        Ok(end)
    }
}

static TOY_INEXACT_ROWS: RowSchema = RowSchema {
    height_cfs: &[],
    archive_cfs: &[],
    state_keys: &[],
    keyed_cfs: &[],
    inexact_cfs: &[TOY_INEXACT],
    has_files: false,
    locator: (TOY_META, TOY_BEST),
};

/// An inexact-only toy whose state counts appends without encoding state rows.
pub struct ToyInexact<E = ProbeEngine> {
    pub staged_file: Option<FileAppend>,
    engine: PhantomData<fn() -> E>,
}

impl<E> Default for ToyInexact<E> {
    fn default() -> Self {
        Self {
            staged_file: None,
            engine: PhantomData,
        }
    }
}

impl<E: KvEngine + 'static> SubscriberData for ToyInexact<E> {
    type E = E;
    type State = ToyState;

    fn name(&self) -> &'static str {
        "toyinexact"
    }
    fn needs(&self) -> Needs {
        Needs::default()
    }
    fn start_height(&self) -> u32 {
        0
    }
    fn schema(&self) -> &'static RowSchema {
        &TOY_INEXACT_ROWS
    }
    fn initial_state(&self) -> ToyState {
        ToyState::default()
    }

    fn connect(
        &self,
        b: &ConnectedBlock<'_>,
        at: &ToyState,
    ) -> Result<(BlockRows, ToyState), SubscriberError> {
        let count = at.high_water + 1;
        Ok((
            BlockRows {
                rows: vec![Row::Put(
                    TOY_INEXACT,
                    b.block.hash.as_bytes().to_vec(),
                    count.to_be_bytes().to_vec(),
                )],
                file: self.staged_file.clone(),
            },
            ToyState {
                count,
                high_water: count,
                ..ToyState::default()
            },
        ))
    }

    fn stage_disconnected(
        &self,
        _: &DisconnectedBlock<'_>,
        at: &ToyState,
        _: &DurableBelow<'_, E>,
    ) -> Result<(Vec<Row>, ToyState), SubscriberError> {
        Ok((Vec::new(), *at))
    }
    fn archive(&self, _: &[Row]) -> Vec<Row> {
        Vec::new()
    }
    fn with_high_water(&self, at: &ToyState, latest: &ToyState) -> ToyState {
        ToyState {
            high_water: at.high_water.max(latest.high_water),
            ..*at
        }
    }
    fn encode_state(&self, _: &ToyState) -> Vec<Row> {
        Vec::new()
    }
    fn decode_state(
        &self,
        _: &[StateRead],
        _: &DurableBelow<'_, E>,
    ) -> Result<ToyState, SubscriberError> {
        Ok(self.initial_state())
    }
    fn height_ranges(&self, _: u32, _: u32) -> Vec<CfRange> {
        Vec::new()
    }
    fn max_entry_bytes(&self) -> u64 {
        0
    }
}
