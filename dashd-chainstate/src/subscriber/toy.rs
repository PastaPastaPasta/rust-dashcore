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
//! [`toy_expected`] recomputes every non-inexact row from the path to a tip, which the
//! checks compare with the database at L and with the driver's reads at p.

use std::marker::PhantomData;

use dash_chain::{BlockRef, Needs};
use dashd_store::{Cf, CfProfile, CfSpec, DbSchema, KvEngine, KvRead};

use super::SubscriberError;
use super::data::{
    BlockRows, CfRange, ConnectedBlock, DisconnectedBlock, DurableBelow, KeyValue, Row, RowSchema,
    StateRead, SubscriberData, height_key,
};

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
    /// The highest `count` the process reached: monotone across rewinds and restarts.
    pub high_water: u64,
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
pub struct ToyIndex<E = crate::store::ProbeEngine> {
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
    let (want, s) = toy_expected(path);
    let l = path.last().ok_or("an empty path")?.height;
    let heights: Vec<KeyValue> = KvRead::iter_prefix(db, TOY_HEIGHT, &[])
        .filter(|r| r.as_ref().map_or(true, |(k, _)| k[..4] <= l.to_be_bytes()[..]))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let want_heights: Vec<KeyValue> =
        want.iter().filter(|r| r.0 == TOY_HEIGHT).map(|r| (r.1.clone(), r.2.clone())).collect();
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

    fn with_file_floor(&self, at: &ToyState, file_len: u64) -> ToyState {
        ToyState {
            high_water: at.high_water.max(file_len),
            ..*at
        }
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

    fn file_len(&self) -> Result<u64, SubscriberError> {
        Ok(self.file_len)
    }
}
