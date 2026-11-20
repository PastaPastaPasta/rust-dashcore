// SPDX-License-Identifier: MIT

//! The coin statistics index's data object: Core's `CoinStatsIndex` as a subscriber, with one
//! height row per block, hash-keyed archive rows for blocks that left the chain, and the MuHash
//! state `M` written only with the locator.

use std::marker::PhantomData;
use std::sync::Arc;

use dash_chain::Needs;
use dash_consensus::Block;
use dash_consensus::coins::stats::CoinStatsAccumulator;
use dash_consensus::coins::{BlockUndoRef, MuHash3072};
use dashd_chainstate::subscriber::{
    BlockRows, ConnectedBlock, DisconnectedBlock, DurableBelow, Row, RowSchema, SubscriberData,
    SubscriberError, height_key,
};
use dashd_store::{Cf, CfProfile, CfSpec, DbSchema, KvEngine};

use super::codec::{self, DBVAL_SIZE};
use super::{CoinStatsAdapter, from_parts};
use crate::{BlockHash, BlockRef, CoinStatsEntry, IndexError, SubsidyInputs};

/// The index's own store.
pub static COINSTATS_DB: DbSchema = DbSchema {
    name: "coinstats",
    version: 1,
    cfs: &[
        CfSpec::new("meta", CfProfile::Ordered),
        CfSpec::new("t", CfProfile::Ordered),
        CfSpec::new("s", CfProfile::PointLookup),
    ],
};

/// The locator and `M`.
pub const META: Cf = DbSchema::META;
/// `BE32 height` → `hash32 ‖ DBVal`.
// Core: index/coinstatsindex.cpp:26
pub const HEIGHT: Cf = Cf::new(1);
/// `block_hash` → `DBVal`.
// Core: index/coinstatsindex.cpp:25
pub const ARCHIVE: Cf = Cf::new(2);

// Core: index/base.cpp:27
pub const BEST_BLOCK: &[u8] = b"B";
// Core: index/coinstatsindex.cpp:27
pub const MUHASH: &[u8] = b"M";

pub static COINSTATS_ROWS: RowSchema = RowSchema {
    height_cfs: &[HEIGHT],
    archive_cfs: &[ARCHIVE],
    state_keys: &[(META, MUHASH)],
    keyed_cfs: &[],
    inexact_cfs: &[],
    has_files: false,
    locator: (META, BEST_BLOCK),
};

/// A height row's value: the block hash, then its `DBVal`.
pub const HEIGHT_VALUE_SIZE: usize = 32 + DBVAL_SIZE;

/// Core's in-memory index state, and the height of the block it is at (`None` before genesis).
#[derive(Clone, Debug, Default)]
pub struct CoinStatsState {
    pub height: Option<u32>,
    pub accumulator: CoinStatsAccumulator,
}

pub fn encode_height_value(hash: &BlockHash, entry: &CoinStatsEntry) -> Vec<u8> {
    let mut v = Vec::with_capacity(HEIGHT_VALUE_SIZE);
    v.extend_from_slice(hash.as_bytes());
    v.extend_from_slice(&codec::encode(entry));
    v
}

pub fn decode_height_value(v: &[u8]) -> Result<(BlockHash, CoinStatsEntry), IndexError> {
    if v.len() != HEIGHT_VALUE_SIZE {
        return Err(IndexError::Corrupt(format!(
            "coinstats height row length {}, expected {HEIGHT_VALUE_SIZE}",
            v.len()
        )));
    }
    let (hash, entry) = v.split_at(32);
    let hash: [u8; 32] = hash.try_into().expect("32 bytes");
    Ok((BlockHash::from(hash), codec::decode(entry)?))
}

fn data_err(err: impl std::fmt::Display) -> SubscriberError {
    SubscriberError::Data(format!("coinstats: {err}"))
}

/// Restores the state at L as Core's `CustomInit` does: `M` (the empty set when absent), then
/// the height row at L, which must name L and hold `M`'s MuHash. With no L, `M` alone.
// Core: index/coinstatsindex.cpp:350-392
pub fn restore_state(
    m: Option<&[u8]>,
    l: Option<&BlockRef>,
    height_row: impl FnOnce(u32) -> Result<Option<Vec<u8>>, SubscriberError>,
) -> Result<CoinStatsState, SubscriberError> {
    let muhash = match m {
        None => MuHash3072::new(),
        Some(m) => MuHash3072::from_bytes(m.try_into().map_err(|_| {
            data_err(format!(
                "cannot read the MuHash state: M length {}, expected {}",
                m.len(),
                MuHash3072::SERIALIZED_SIZE
            ))
        })?),
    };
    let Some(l) = l else {
        return Ok(CoinStatsState {
            height: None,
            accumulator: CoinStatsAccumulator::from_parts(muhash, Default::default()),
        });
    };
    // Core: index/coinstatsindex.cpp:304-321 (`LookUpOne`; no archive read at or below L)
    let row = height_row(l.height)?
        .ok_or_else(|| data_err(format!("no height row at L ({})", l.height)))?;
    let (hash, entry) = decode_height_value(&row).map_err(data_err)?;
    if hash != l.hash {
        return Err(data_err(format!(
            "the height row at L ({}) names {hash}, not L {}",
            l.height, l.hash
        )));
    }
    let accumulator = from_parts(muhash, &entry).map_err(|err| {
        data_err(format!("M does not match the height row at L ({}): {err}", l.height))
    })?;
    Ok(CoinStatsState {
        height: Some(l.height),
        accumulator,
    })
}

/// A state at another height than the block needs (the driver hands the state at the block's
/// parent, so this only guards the data object's own contract).
fn expect_height(
    state: &CoinStatsState,
    height: Option<u32>,
    block: &BlockRef,
) -> Result<(), SubscriberError> {
    if state.height != height {
        return Err(data_err(format!(
            "the state is at {:?}, not {height:?}, for {}",
            state.height, block.hash
        )));
    }
    Ok(())
}

/// The entry in the height row at `height`, which must name `hash`.
fn stored_row<E: KvEngine>(
    below: &DurableBelow<'_, E>,
    height: u32,
    hash: &BlockHash,
) -> Result<CoinStatsEntry, SubscriberError> {
    let row = below
        .height_row(HEIGHT, height, &[])?
        .ok_or_else(|| data_err(format!("no height row at {height} below L")))?;
    let (named, entry) = decode_height_value(&row).map_err(data_err)?;
    if named != *hash {
        return Err(data_err(format!("the height row at {height} names {named}, not {hash}")));
    }
    Ok(entry)
}

/// Core's `CoinStatsIndex` as a subscriber's data object.
pub struct CoinStatsData<E> {
    adapter: CoinStatsAdapter,
    engine: PhantomData<fn() -> E>,
}

impl<E> CoinStatsData<E> {
    pub fn new(subsidies: Arc<dyn SubsidyInputs>) -> Self {
        Self {
            adapter: CoinStatsAdapter::new(subsidies),
            engine: PhantomData,
        }
    }
}

/// The block and its spent coins; genesis's undo is never read.
// Core: index/coinstatsindex.cpp:127-130
fn inputs<'a>(
    block: &BlockRef,
    data: Option<&'a Block>,
    undo: Option<&'a [u8]>,
) -> Result<(&'a Block, BlockUndoRef<'a>), SubscriberError> {
    let data = data.ok_or_else(|| data_err(format!("no block data for {}", block.hash)))?;
    if block.height == 0 {
        return Ok((data, BlockUndoRef::default()));
    }
    let mut bytes = undo.ok_or_else(|| data_err(format!("no undo data for {}", block.hash)))?;
    let undo = BlockUndoRef::decode(&mut bytes)
        .map_err(|err| data_err(format!("undo for {}: {err}", block.hash)))?;
    Ok((data, undo))
}

impl<E: KvEngine + 'static> SubscriberData for CoinStatsData<E> {
    type E = E;
    type State = CoinStatsState;

    fn name(&self) -> &'static str {
        "coinstatsindex"
    }

    fn needs(&self) -> Needs {
        Needs {
            block: true,
            undo: true,
        }
    }

    fn start_height(&self) -> u32 {
        0
    }

    fn schema(&self) -> &'static RowSchema {
        &COINSTATS_ROWS
    }

    fn initial_state(&self) -> CoinStatsState {
        CoinStatsState::default()
    }

    // Core: index/coinstatsindex.cpp:117-234
    fn connect(
        &self,
        b: &ConnectedBlock<'_>,
        at_parent: &CoinStatsState,
    ) -> Result<(BlockRows, CoinStatsState), SubscriberError> {
        expect_height(at_parent, b.block.height.checked_sub(1), &b.block)?;
        let (data, undo) = inputs(&b.block, b.data, b.undo)?;
        let mut accumulator = at_parent.accumulator.clone();
        let entry =
            self.adapter.append_block(&mut accumulator, &b.block, data, &undo).map_err(data_err)?;
        let row = Row::Put(
            HEIGHT,
            height_key(b.block.height, &[]),
            encode_height_value(&b.block.hash, &entry),
        );
        Ok((
            BlockRows {
                rows: vec![row],
                file: None,
            },
            CoinStatsState {
                height: Some(b.block.height),
                accumulator,
            },
        ))
    }

    // Core: index/coinstatsindex.cpp:403-505
    fn stage_disconnected(
        &self,
        b: &DisconnectedBlock<'_>,
        at_b: &CoinStatsState,
        below: &DurableBelow<'_, E>,
    ) -> Result<(Vec<Row>, CoinStatsState), SubscriberError> {
        expect_height(at_b, Some(b.block.height), &b.block)?;
        let (data, undo) = inputs(&b.block, b.data, b.undo)?;
        // The archive copies this row (Core's `CopyHeightIndexToHashIndex` fails without it).
        // Core: index/coinstatsindex.cpp:243-255
        stored_row(below, b.block.height, &b.block.hash)?;
        let mut accumulator = at_b.accumulator.clone();
        let entry = self
            .adapter
            .reverse_block(&mut accumulator, &b.block, data, &undo)
            .map_err(data_err)?;
        let parent = b.block.height - 1;
        // Core reads the archive on a hash mismatch (:421-431); rows at or below L are on L's path.
        let stored = stored_row(below, parent, &data.header().prev_hash())?;
        // Core: index/coinstatsindex.cpp:486-502
        if stored != entry {
            return Err(data_err(format!(
                "reversing {} does not give the stored entry at {parent}",
                b.block.hash
            )));
        }
        Ok((
            Vec::new(),
            CoinStatsState {
                height: Some(parent),
                accumulator,
            },
        ))
    }

    // Core: index/coinstatsindex.cpp:236-262
    fn archive(&self, height_rows: &[Row]) -> Vec<Row> {
        height_rows
            .iter()
            .filter_map(|r| match r {
                Row::Put(HEIGHT, _, v) if v.len() == HEIGHT_VALUE_SIZE => {
                    Some(Row::Put(ARCHIVE, v[..32].to_vec(), v[32..].to_vec()))
                }
                _ => None,
            })
            .collect()
    }

    // Core: index/coinstatsindex.cpp:394-401
    fn encode_state(&self, s: &CoinStatsState) -> Vec<Row> {
        vec![Row::Put(META, MUHASH.to_vec(), s.accumulator.muhash().to_bytes().to_vec())]
    }

    fn decode_state(
        &self,
        rows: &[(Cf, Vec<u8>, Option<Vec<u8>>)],
        at_l: &DurableBelow<'_, E>,
    ) -> Result<CoinStatsState, SubscriberError> {
        let [(_, _, m)] = rows else {
            return Err(data_err("one state row expected"));
        };
        restore_state(m.as_deref(), at_l.locator(), |height| at_l.height_row(HEIGHT, height, &[]))
    }

    fn height_ranges(&self, lo_exclusive: u32, hi_inclusive: u32) -> Vec<(Cf, Vec<u8>, Vec<u8>)> {
        if lo_exclusive >= hi_inclusive {
            return Vec::new();
        }
        let start = height_key(lo_exclusive + 1, &[]);
        let end = match hi_inclusive.checked_add(1) {
            Some(h) => height_key(h, &[]),
            None => vec![0xff; 5],
        };
        vec![(HEIGHT, start, end)]
    }

    /// One height row per block whatever the block holds, so the bound is a constant: the row's
    /// key and value, the row itself, the driver's entry vector, and the inline state, which owns
    /// no heap memory. The vector's four slots are measured against std's current first
    /// allocation for one row, not guaranteed by it; the replay test's equality is the tripwire.
    fn max_entry_bytes(&self) -> u64 {
        let row = 4 + HEIGHT_VALUE_SIZE + size_of::<Row>();
        (row + 4 * size_of::<Row>() + size_of::<CoinStatsState>()) as u64
    }
}

#[cfg(test)]
mod tests {
    use dashd_store::MemoryEngine;

    use super::*;

    struct NoSubsidy;

    impl SubsidyInputs for NoSubsidy {
        fn subsidy(&self, _: &BlockRef) -> Result<i64, IndexError> {
            Err(IndexError::Io("no subsidy".into()))
        }
    }

    fn data() -> CoinStatsData<MemoryEngine> {
        CoinStatsData::new(Arc::new(NoSubsidy))
    }

    #[test]
    fn the_schema_and_needs_are_cores() {
        let d = data();
        assert_eq!((d.name(), d.start_height(), d.mandatory()), ("coinstatsindex", 0, true));
        assert_eq!(
            d.needs(),
            Needs {
                block: true,
                undo: true
            }
        );
        assert!(!d.schema().inexact_only());
        assert_eq!(d.schema().state_keys, &[(META, MUHASH)]);
        assert!(d.initial_state().height.is_none());
    }

    #[test]
    fn height_ranges_cover_the_heights_above_lo_through_hi() {
        let d = data();
        assert_eq!(d.height_ranges(3, 7), vec![(HEIGHT, height_key(4, &[]), height_key(8, &[]))]);
        assert_eq!(d.height_ranges(3, 4), vec![(HEIGHT, height_key(4, &[]), height_key(5, &[]))]);
        assert!(d.height_ranges(7, 7).is_empty());
        assert!(d.height_ranges(8, 7).is_empty());
        let (_, start, end) = d.height_ranges(5, u32::MAX).pop().unwrap();
        assert_eq!(start, height_key(6, &[]));
        assert!(end > height_key(u32::MAX, &[]));
    }

    #[test]
    fn archive_copies_height_rows_under_their_hash() {
        let hash = BlockHash::from([7; 32]);
        let entry = CoinStatsEntry {
            txouts: 3,
            ..CoinStatsEntry::default()
        };
        let value = encode_height_value(&hash, &entry);
        let rows = [
            Row::Put(HEIGHT, height_key(9, &[]), value.clone()),
            Row::Put(HEIGHT, height_key(10, &[]), vec![1; 12]),
            Row::Put(META, MUHASH.to_vec(), value.clone()),
        ];
        assert_eq!(
            data().archive(&rows),
            vec![Row::Put(ARCHIVE, hash.as_bytes().to_vec(), codec::encode(&entry).to_vec())]
        );
        assert_eq!(decode_height_value(&value), Ok((hash, entry)));
    }

    // Core: index/coinstatsindex.cpp:350-401
    #[test]
    fn m_is_cores_muhash_serialization_restored_with_the_row_at_l() {
        let d = data();
        let mut state = CoinStatsState {
            height: Some(9),
            accumulator: CoinStatsAccumulator::new(),
        };
        state.accumulator =
            CoinStatsAccumulator::from_parts(MuHash3072::from_data(b"a coin"), Default::default());
        let entry = CoinStatsEntry::from_accumulator(&mut state.accumulator);
        let [Row::Put(META, key, m)] = &d.encode_state(&state)[..] else {
            panic!("one M row");
        };
        assert_eq!(key, MUHASH);
        assert_eq!(m[..], state.accumulator.muhash().to_bytes());
        let l = BlockRef {
            id: dash_consensus::BlockId(9),
            hash: BlockHash::from([1; 32]),
            height: 9,
        };
        let row = encode_height_value(&l.hash, &entry);
        let restored = restore_state(Some(m), Some(&l), |h| {
            assert_eq!(h, 9);
            Ok(Some(row.clone()))
        })
        .unwrap();
        assert_eq!(restored.height, Some(9));
        assert_eq!(restored.accumulator.totals(), state.accumulator.totals());
        let alone = restore_state(Some(m), None, |_| panic!("no row read without L")).unwrap();
        assert_eq!((alone.height, *alone.accumulator.totals()), (None, Default::default()));
        assert_eq!(alone.accumulator.muhash().to_bytes()[..], m[..]);
    }

    #[test]
    fn connect_refuses_a_state_not_at_the_parent() {
        let d = data();
        let block = |height| BlockRef {
            id: dash_consensus::BlockId(height),
            hash: BlockHash::from([2; 32]),
            height,
        };
        for (height, at) in [(0, Some(0)), (3, None), (3, Some(3)), (3, Some(1))] {
            let b = ConnectedBlock {
                block: block(height),
                data: None,
                undo: None,
            };
            let state = CoinStatsState {
                height: at,
                ..CoinStatsState::default()
            };
            let Err(SubscriberError::Data(err)) = d.connect(&b, &state) else {
                panic!("a connect at {height} on a state at {at:?}");
            };
            assert!(err.contains(&format!("the state is at {at:?},")), "{err}");
        }
    }
}
