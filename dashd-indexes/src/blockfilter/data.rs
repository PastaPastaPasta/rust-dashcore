// SPDX-License-Identifier: MIT

//! The basic block filter index's data object: Core's `BlockFilterIndex` as a subscriber, with
//! one height row per block, hash-keyed archive rows for blocks that left the chain, the next
//! filter position `P` written only with the locator, and the filters in `fltr?????.dat`.

use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;

use dash_chain::Needs;
use dash_consensus::coins::BlockUndo;
use dash_consensus::primitives::{read_varint_u32, read_varint_u64, write_varint};
use dash_filters::FilterHeader;
use dash_types::Numeric as _;
use dashd_chainstate::subscriber::{
    BlockRows, CfRange, ConnectedBlock, DisconnectedBlock, DurableBelow, FileAppend, FilePos, Row,
    RowSchema, StateRead, SubscriberData, SubscriberError, height_key,
};
use dashd_store::flatfile::FlatFs;
use dashd_store::{Cf, CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine};

use super::build_filter;
use super::codec::{DbVal, FilterPos, HeightValue};
use super::files::{FilterFiles, encode_entry};
use crate::{BlockHash, IndexError};

/// The index's own store.
pub static BLOCKFILTER_DB: DbSchema = DbSchema {
    name: "blockfilter",
    version: 1,
    cfs: &[
        CfSpec::new("meta", CfProfile::Ordered),
        CfSpec::new("t", CfProfile::Ordered),
        CfSpec::new("s", CfProfile::PointLookup),
    ],
};

/// The locator, `P` and `V`.
pub const META: Cf = DbSchema::META;
/// `BE32 height` → `hash32 ‖ DBVal`.
// Core: index/blockfilterindex.cpp:34
pub const HEIGHT: Cf = Cf::new(1);
/// `block_hash` → `DBVal`.
// Core: index/blockfilterindex.cpp:33
pub const ARCHIVE: Cf = Cf::new(2);

// Core: index/base.cpp:27
pub const BEST_BLOCK: &[u8] = b"B";
// Core: index/blockfilterindex.cpp:35
pub const FILTER_POS: &[u8] = b"P";
/// Outside the row schema: the data object checks and writes it before the driver opens.
// Core: index/blockfilterindex.cpp:36
pub const VERSION: &[u8] = b"V";
// Core: index/blockfilterindex.h:33
pub const CURRENT_VERSION: i32 = 2;

pub static BLOCKFILTER_ROWS: RowSchema = RowSchema {
    height_cfs: &[HEIGHT],
    archive_cfs: &[ARCHIVE],
    state_keys: &[(META, FILTER_POS)],
    keyed_cfs: &[],
    inexact_cfs: &[],
    has_files: true,
    locator: (META, BEST_BLOCK),
};

/// The longest height value: three hashes and a `FlatFilePos` of two VARINTs.
pub const HEIGHT_VALUE_MAX: usize = 32 * 3 + 5 + 10;

/// The block at a state: the parent header the next connect chains from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterTip {
    pub height: u32,
    pub hash: BlockHash,
    pub header: FilterHeader,
}

/// Core's `m_next_filter_pos`, a high-water mark, and the block the state is at (`None` before
/// genesis).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FilterState {
    pub next_pos: FilePos,
    pub tip: Option<FilterTip>,
}

/// `VARINT(nFile) ‖ VARINT(nPos)`, Core's `FlatFilePos`; an offset Core's `unsigned int`
/// cannot hold encodes as a longer VARINT.
// Core: flatfile.h:19-22
pub fn encode_pos(pos: FilePos) -> Vec<u8> {
    let mut v = Vec::with_capacity(15);
    write_varint(u64::from(pos.file), &mut v);
    write_varint(pos.offset, &mut v);
    v
}

pub fn decode_pos(mut v: &[u8]) -> Result<FilePos, IndexError> {
    let corrupt =
        |e: String| IndexError::Corrupt(format!("cannot read the next filter position: {e}"));
    let file = read_varint_u32(&mut v).map_err(|e| corrupt(e.to_string()))?;
    let offset = read_varint_u64(&mut v).map_err(|e| corrupt(e.to_string()))?;
    if file > i32::MAX as u32 || !v.is_empty() {
        return Err(corrupt("a bad position".into()));
    }
    Ok(FilePos {
        file,
        offset,
    })
}

fn data_err(err: impl std::fmt::Display) -> SubscriberError {
    SubscriberError::Data(format!("basic block filter index: {err}"))
}

fn decode_version(v: &[u8]) -> Option<i32> {
    Some(i32::from_le_bytes(v.try_into().ok()?))
}

/// The tip a height row names, which must be `hash`.
fn tip_from_row(height: u32, row: &[u8], hash: &BlockHash) -> Result<FilterTip, SubscriberError> {
    let value = HeightValue::decode(row).map_err(data_err)?;
    if value.block_hash != *hash {
        return Err(data_err(format!(
            "the height row at {height} names {}, not {hash}",
            value.block_hash
        )));
    }
    Ok(FilterTip {
        height,
        hash: *hash,
        header: value.filter.header,
    })
}

/// Core's `BlockFilterIndex` for the basic filter as a subscriber's data object.
pub struct BlockFilterData<E> {
    files: FilterFiles,
    engine: PhantomData<fn() -> E>,
}

impl<E> BlockFilterData<E> {
    /// Checks `V` on the index's own store (never the chainstate's, whose failures this index
    /// must not share): missing, unreadable or older wipes the rows and writes the current
    /// version Durable, higher is refused. The filter files in `dir` are kept; the open's floor
    /// appends past them. Call it before the driver's open, on the store the driver will use.
    // Core: index/blockfilterindex.cpp:111-146
    pub fn new(
        engine: &impl KvEngine,
        fs: Arc<dyn FlatFs>,
        dir: PathBuf,
    ) -> Result<Self, IndexError> {
        if engine.schema().name != BLOCKFILTER_DB.name {
            return Err(IndexError::Corrupt(format!(
                "the block filter index on the {} store, not its own",
                engine.schema().name
            )));
        }
        let io = |what: &str, e: &dyn std::fmt::Display| {
            IndexError::Io(format!("basic block filter index {what}: {e}"))
        };
        let version = engine.get(META, VERSION).map_err(|e| io("version read", &e))?;
        match version.as_deref().and_then(decode_version) {
            Some(v) if v > CURRENT_VERSION => {
                return Err(IndexError::Corrupt(format!(
                    "basic block filter index version {v} is too high (expected <= {CURRENT_VERSION})"
                )));
            }
            Some(v) if v == CURRENT_VERSION => {}
            _ => {
                let mut b = engine.batch();
                b.delete(META, BEST_BLOCK);
                b.delete(META, FILTER_POS);
                for cf in [HEIGHT, ARCHIVE] {
                    b.delete_range(cf, &[], &[0xff; 33]);
                }
                b.put(META, VERSION, &CURRENT_VERSION.to_le_bytes());
                engine.write(b, Commit::Durable).map_err(|e| io("wipe", &e))?;
            }
        }
        fs.create_dir_all(&dir).map_err(|e| io("directory", &e))?;
        Ok(Self {
            files: FilterFiles::new(fs, dir),
            engine: PhantomData,
        })
    }

    /// Other file and chunk sizes, for tests.
    pub fn with_file_sizes(mut self, max_file_size: u64, chunk_size: u64) -> Self {
        self.files = self.files.with_sizes(max_file_size, chunk_size);
        self
    }

    pub fn files(&self) -> &FilterFiles {
        &self.files
    }
}

impl<E: KvEngine + 'static> SubscriberData for BlockFilterData<E> {
    type E = E;
    type State = FilterState;

    // Core: index/blockfilterindex.cpp:108
    fn name(&self) -> &'static str {
        "basic block filter index"
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
        &BLOCKFILTER_ROWS
    }

    fn initial_state(&self) -> FilterState {
        FilterState::default()
    }

    // Core: index/blockfilterindex.cpp:266-311
    fn connect(
        &self,
        b: &ConnectedBlock<'_>,
        at_parent: &FilterState,
    ) -> Result<(BlockRows, FilterState), SubscriberError> {
        let block =
            b.data.ok_or_else(|| data_err(format!("no block data for {}", b.block.hash)))?;
        let (prev_header, undo) = match (b.block.height, at_parent.tip) {
            (0, None) => (FilterHeader::from_lendian([0; 32]), BlockUndo::default()),
            (h, Some(tip)) if h > 0 => {
                let expected = block.header().prev_hash();
                if tip.height + 1 != h || tip.hash != expected {
                    return Err(data_err(format!(
                        "previous block header belongs to unexpected block {}; expected {expected}",
                        tip.hash
                    )));
                }
                let mut bytes =
                    b.undo.ok_or_else(|| data_err(format!("no undo data for {}", b.block.hash)))?;
                let undo = BlockUndo::decode(&mut bytes)
                    .map_err(|e| data_err(format!("undo for {}: {e}", b.block.hash)))?;
                (tip.header, undo)
            }
            (h, tip) => {
                return Err(data_err(format!("a connect at {h} on the state at {tip:?}")));
            }
        };
        let filter = build_filter(block, &undo).map_err(data_err)?;
        let bytes = encode_entry(&b.block.hash, filter.encoded());
        let at = self.files.start(at_parent.next_pos, bytes.len() as u64);
        let header = filter.header(&prev_header);
        let value = HeightValue {
            block_hash: b.block.hash,
            filter: DbVal {
                filter_hash: filter.hash(),
                header,
                pos: FilterPos {
                    file: i32::try_from(at.file).map_err(data_err)?,
                    pos: at.offset,
                },
            },
        };
        let mut v = Vec::with_capacity(HEIGHT_VALUE_MAX);
        v.extend_from_slice(&value.encode().map_err(data_err)?);
        let append = FileAppend {
            file: at.file,
            offset: at.offset,
            bytes,
        };
        let state = FilterState {
            next_pos: append.end(),
            tip: Some(FilterTip {
                height: b.block.height,
                hash: b.block.hash,
                header,
            }),
        };
        Ok((
            BlockRows {
                rows: vec![Row::Put(HEIGHT, height_key(b.block.height, &[]), v)],
                file: Some(append),
            },
            state,
        ))
    }

    /// The state at b's parent from the height row below b; the position is kept.
    // Core: index/blockfilterindex.cpp:313-359
    fn stage_disconnected(
        &self,
        b: &DisconnectedBlock<'_>,
        at_b: &FilterState,
        below: &DurableBelow<'_, E>,
    ) -> Result<(Vec<Row>, FilterState), SubscriberError> {
        let h = b.block.height;
        let row = |height| {
            below
                .height_row(HEIGHT, height, &[])?
                .ok_or_else(|| data_err(format!("no height row at {height} below L")))
        };
        tip_from_row(h, &row(h)?, &b.block.hash)?;
        let parent = h.checked_sub(1).ok_or_else(|| data_err("genesis is never disconnected"))?;
        let parent_row = row(parent)?;
        let parent_hash = HeightValue::decode(&parent_row).map_err(data_err)?.block_hash;
        if let Some(block) = b.data
            && block.header().prev_hash() != parent_hash
        {
            return Err(data_err(format!(
                "the height row at {parent} names {parent_hash}, not {}'s parent",
                b.block.hash
            )));
        }
        Ok((
            Vec::new(),
            FilterState {
                next_pos: at_b.next_pos,
                tip: Some(tip_from_row(parent, &parent_row, &parent_hash)?),
            },
        ))
    }

    // Core: index/blockfilterindex.cpp:313-339
    fn archive(&self, height_rows: &[Row]) -> Vec<Row> {
        height_rows
            .iter()
            .filter_map(|r| match r {
                Row::Put(HEIGHT, _, v) if v.len() > 32 => {
                    Some(Row::Put(ARCHIVE, v[..32].to_vec(), v[32..].to_vec()))
                }
                _ => None,
            })
            .collect()
    }

    /// Core never lowers `m_next_filter_pos`, so a later append never overwrites bytes an
    /// archive row references.
    // Core: index/blockfilterindex.cpp:341-359
    fn with_high_water(&self, at: &FilterState, latest: &FilterState) -> FilterState {
        FilterState {
            next_pos: at.next_pos.max(latest.next_pos),
            tip: at.tip,
        }
    }

    fn with_file_floor(&self, at: &FilterState, end: FilePos) -> FilterState {
        FilterState {
            next_pos: at.next_pos.max(end),
            tip: at.tip,
        }
    }

    fn file_pos(&self, s: &FilterState) -> Option<FilePos> {
        Some(s.next_pos)
    }

    // Core: index/blockfilterindex.cpp:165-187, :356
    fn encode_state(&self, s: &FilterState) -> Vec<Row> {
        vec![Row::Put(META, FILTER_POS.to_vec(), encode_pos(s.next_pos))]
    }

    /// `P` (the first position when absent), and the header at L from its height row.
    // Core: index/blockfilterindex.cpp:148-162, :279-291
    fn decode_state(
        &self,
        rows: &[StateRead],
        at_l: &DurableBelow<'_, E>,
    ) -> Result<FilterState, SubscriberError> {
        let [(_, _, p)] = rows else {
            return Err(data_err("one state row expected"));
        };
        let next_pos = p.as_deref().map(decode_pos).transpose().map_err(data_err)?;
        let tip = match at_l.locator() {
            None => None,
            Some(l) => {
                let row = at_l
                    .height_row(HEIGHT, l.height, &[])?
                    .ok_or_else(|| data_err(format!("no height row at L ({})", l.height)))?;
                Some(tip_from_row(l.height, &row, &l.hash)?)
            }
        };
        Ok(FilterState {
            next_pos: next_pos.unwrap_or_default(),
            tip,
        })
    }

    fn height_ranges(&self, lo_exclusive: u32, hi_inclusive: u32) -> Vec<CfRange> {
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

    /// One height row per block, its value allocated at [`HEIGHT_VALUE_MAX`]; the rest as
    /// coinstats' bound, measured against std's first allocation for one row.
    fn max_entry_bytes(&self) -> u64 {
        let row = 4 + HEIGHT_VALUE_MAX + size_of::<Row>();
        (row + 4 * size_of::<Row>() + size_of::<FilterState>()) as u64
    }

    fn sync_files(&mut self) -> Result<(), SubscriberError> {
        self.files.sync().map_err(|e| data_err(format!("filter file sync: {e}")))
    }

    fn append_file(&mut self, append: &FileAppend) -> Result<(), SubscriberError> {
        self.files.append(append).map_err(|e| data_err(format!("filter file write: {e}")))
    }

    fn file_len(&self) -> Result<FilePos, SubscriberError> {
        self.files.end().map_err(|e| data_err(format!("filter file length: {e}")))
    }

    fn file_size(&self, n: u32) -> Result<Option<u64>, SubscriberError> {
        self.files.len(n).map_err(|e| data_err(format!("filter file {n} length: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use dashd_store::MemoryEngine;

    use super::*;

    fn data() -> BlockFilterData<MemoryEngine> {
        BlockFilterData {
            files: FilterFiles::new(Arc::new(dashd_store::flatfile::OsFs::new()), PathBuf::new()),
            engine: PhantomData,
        }
    }

    #[test]
    fn the_schema_and_needs_are_cores() {
        let d = data();
        assert_eq!(
            (d.name(), d.start_height(), d.mandatory()),
            ("basic block filter index", 0, true)
        );
        assert_eq!(
            d.needs(),
            Needs {
                block: true,
                undo: true
            }
        );
        assert!(d.schema().has_files && !d.schema().inexact_only());
        assert_eq!(d.schema().state_keys, &[(META, FILTER_POS)]);
        assert_eq!(d.initial_state(), FilterState::default());
        assert_eq!(d.file_pos(&d.initial_state()), Some(FilePos::default()));
    }

    // Core: flatfile.h:19-22; serialize.h:404-424
    #[test]
    fn p_round_trips_as_cores_flat_file_pos() {
        for (file, offset, core) in [
            (0, 0, &[0x00, 0x00][..]),
            (1, 0x7f, &[0x01, 0x7f]),
            (2, 0x80, &[0x02, 0x80, 0x00]),
            (0, 0xff_ffff, &[0x00, 0x86, 0xfe, 0xfe, 0x7f]),
        ] {
            let pos = FilePos {
                file,
                offset,
            };
            assert_eq!(encode_pos(pos), core, "{pos:?}");
            assert_eq!(decode_pos(core).unwrap(), pos);
        }
        for bad in [&[][..], &[0][..], &[0, 0, 0], &[0x87, 0xff, 0xff, 0xff, 0x7f, 0]] {
            assert!(decode_pos(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_high_water_and_floor_never_lower_the_position() {
        let d = data();
        let at = |file, offset| FilterState {
            next_pos: FilePos {
                file,
                offset,
            },
            tip: None,
        };
        assert_eq!(d.with_high_water(&at(0, 50), &at(0, 90)), at(0, 90));
        assert_eq!(d.with_high_water(&at(0, 90), &at(0, 50)), at(0, 90));
        assert_eq!(d.with_high_water(&at(0, 900), &at(1, 0)), at(1, 0));
        let end = FilePos {
            file: 1,
            offset: 7,
        };
        assert_eq!(d.with_file_floor(&at(0, 900), end), at(1, 7));
        assert_eq!(d.with_file_floor(&at(2, 0), end), at(2, 0));
    }

    #[test]
    fn archive_copies_height_rows_under_their_hash() {
        let value = HeightValue {
            block_hash: BlockHash::from([7; 32]),
            filter: DbVal {
                filter_hash: FilterHeader::from_lendian([1; 32]),
                header: FilterHeader::from_lendian([2; 32]),
                pos: FilterPos {
                    file: 3,
                    pos: 400,
                },
            },
        };
        let v = value.encode().unwrap();
        let rows = [
            Row::Put(HEIGHT, height_key(9, &[]), v.clone()),
            Row::Put(META, FILTER_POS.to_vec(), v),
        ];
        assert_eq!(
            data().archive(&rows),
            vec![Row::Put(ARCHIVE, vec![7; 32], value.filter.encode().unwrap())]
        );
    }

    #[test]
    fn height_ranges_cover_the_heights_above_lo_through_hi() {
        let d = data();
        assert_eq!(d.height_ranges(3, 7), vec![(HEIGHT, height_key(4, &[]), height_key(8, &[]))]);
        assert!(d.height_ranges(7, 7).is_empty());
        let (_, start, end) = d.height_ranges(5, u32::MAX).pop().unwrap();
        assert_eq!(start, height_key(6, &[]));
        assert!(end > height_key(u32::MAX, &[]));
    }
}
