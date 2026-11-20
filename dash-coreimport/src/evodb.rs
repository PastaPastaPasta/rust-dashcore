//! Core's masternode lists from its `evodb/` records (EV4-serve D11; A1 §2.21).
//!
//! Core keeps a block's deterministic masternode list as the diff from its parent (`dmn_D4`), every
//! 576 blocks and at the block after its initial snapshot also as a full snapshot (`dmn_S3`), keyed
//! by block hash, and rebuilds a block's list by walking back to a snapshot and applying the diffs
//! forward (`GetListForBlock`). [`EvoDbImage`] holds those rows and [`EvoDbImage::list_at`] does
//! the same walk, with EV2a's decoders (`dash_dml::codec::core_disk`) and `ApplyDiff`.
//!
//! The rows come in as `(key, value)` pairs: reading them out of LevelDB is CH6b's. Other evodb
//! rows (mined commitments, MNHF signals, the credit pool, quorum snapshots) are other importers'
//! and are skipped here.

use std::collections::BTreeMap;

use dash_dml::MnList;
use dash_dml::codec::core_disk::{decode_diff_d4, decode_list_s3};
use dash_primitives::BlockHash;

use crate::{Error, Hash256, Result, hash_to_hex};

// Core: evo/deterministicmns.cpp:31-34
const DB_LIST_SNAPSHOT: &[u8] = b"dmn_S3";
const DB_LIST_DIFF: &[u8] = b"dmn_D4";
const DB_LIST_DIFF_LEGACY: &[u8] = b"dmn_D3";

/// The block index the walk follows: a block's height and its parent's hash (`None` at genesis).
pub trait Ancestry {
    /// The block's height and parent, or `None` for a block the index does not hold.
    fn header(&self, block: &Hash256) -> Option<(i32, Option<Hash256>)>;
}

/// The masternode-list rows of a Core evodb.
#[derive(Clone, Debug, Default)]
pub struct EvoDbImage {
    snapshots: BTreeMap<Hash256, Vec<u8>>,
    diffs: BTreeMap<Hash256, Vec<u8>>,
}

impl EvoDbImage {
    /// The image of an evodb's `(key, value)` records. Keys are Core's `CDBWrapper` encoding of
    /// `std::pair<std::string, uint256>`: the CompactSize-prefixed name, then the block hash.
    /// `dmn_D3` rows (diffs v24 migrates at startup) are refused: open the datadir with Core
    /// v24 once first.
    // Core: evo/deterministicmns.cpp:1018 IsMigrationRequired, :1044 MigrateLegacyDiffs
    pub fn from_records<K, V>(records: impl IntoIterator<Item = (K, V)>) -> Result<Self>
    where
        K: AsRef<[u8]>,
        V: Into<Vec<u8>>,
    {
        let mut image = EvoDbImage::default();
        for (key, value) in records {
            let key = key.as_ref();
            let Some((&len, rest)) = key.split_first() else {
                continue;
            };
            let Some(name) = rest.get(..usize::from(len)) else {
                continue;
            };
            let rows = match name {
                DB_LIST_SNAPSHOT => &mut image.snapshots,
                DB_LIST_DIFF => &mut image.diffs,
                DB_LIST_DIFF_LEGACY => {
                    return Err(Error::Inconsistent(
                        "evodb holds legacy dmn_D3 diffs: start it once with Dash Core v24".into(),
                    ));
                }
                _ => continue,
            };
            let block: Hash256 = rest[usize::from(len)..].try_into().map_err(|_| {
                Error::malformed(
                    "evodb key",
                    format!(
                        "{} with a {}-byte suffix",
                        String::from_utf8_lossy(name),
                        key.len() - 1 - usize::from(len)
                    ),
                )
            })?;
            rows.insert(block, value.into());
        }
        Ok(image)
    }

    /// `block`'s masternode list as Core's `GetListForBlock` rebuilds it from these rows: the
    /// default list below `dip3_height`; else back through `ancestry` to the nearest snapshot,
    /// then each diff applied forward. A block with neither row ends the walk as Core's "initial
    /// snapshot", an empty list at that block. Core reads each value and ignores what follows it;
    /// a value with trailing bytes is refused here, as a damaged record, and so is a snapshot that
    /// names another block or height than its key and the index give, or whose registration
    /// counter is not above every entry's id.
    // Core: evo/deterministicmns.cpp:826 CDeterministicMNManager::GetListForBlockInternal
    pub fn list_at(
        &self,
        ancestry: &impl Ancestry,
        block: &Hash256,
        dip3_height: i32,
    ) -> Result<MnList> {
        let unknown = |hash: &Hash256| {
            Error::Inconsistent(format!("block {} is not in the block index", hash_to_hex(hash)))
        };
        let (height, _) = ancestry.header(block).ok_or_else(|| unknown(block))?;
        if height < dip3_height {
            return Ok(MnList::default_pre_dip3());
        }

        let mut pending = Vec::new();
        let (mut hash, mut height) = (*block, height);
        let mut list = loop {
            if let Some(bytes) = self.snapshots.get(&hash) {
                let list = decode_whole("dmn_S3", &hash, bytes, decode_list_s3)?;
                check_snapshot(&list, &hash, height)?;
                break list;
            }
            let Some(bytes) = self.diffs.get(&hash) else {
                break MnList::new(BlockHash::from(hash), height, 0);
            };
            pending.push((hash, height, bytes));
            let (_, prev) = ancestry.header(&hash).ok_or_else(|| unknown(&hash))?;
            let prev = prev.ok_or_else(|| {
                Error::Inconsistent(format!("a dmn_D4 row for genesis {}", hash_to_hex(&hash)))
            })?;
            height = ancestry.header(&prev).ok_or_else(|| unknown(&prev))?.0;
            hash = prev;
        };
        for (hash, height, bytes) in pending.into_iter().rev() {
            let diff = decode_whole("dmn_D4", &hash, bytes, decode_diff_d4)?;
            list.apply_diff(BlockHash::from(hash), height, &diff).map_err(|e| {
                Error::Inconsistent(format!(
                    "dmn_D4 of block {} does not apply: {}",
                    hash_to_hex(&hash),
                    e.core_message()
                ))
            })?;
        }
        Ok(list)
    }
}

/// Refuses a `dmn_S3` list that is not the one Core wrote under `block`: Core writes a list under
/// its own block hash (`deterministicmns.cpp` v24:722-724), at its block's height, and gives each
/// new masternode the counter as its id (`specialtxman.cpp` v24:365), which `AddMN` then bumps
/// past it (`deterministicmns.cpp` v24:512-515), so every id is below the counter. Core's
/// canonical-list check refuses the same (`snapshot.cpp` v24:121).
fn check_snapshot(list: &MnList, block: &Hash256, height: i32) -> Result<()> {
    let damaged = |detail: String| {
        Err(Error::Inconsistent(format!("dmn_S3 of block {}: {detail}", hash_to_hex(block))))
    };
    if list.block_hash().as_bytes() != block {
        return damaged(format!(
            "the list names block {}",
            hash_to_hex(list.block_hash().as_bytes())
        ));
    }
    if list.height() != height {
        return damaged(format!("the list is at height {}, the block at {height}", list.height()));
    }
    let total = u64::from(list.total_registered());
    if let Some(dmn) = list.iter().find(|dmn| dmn.internal_id >= total) {
        return damaged(format!(
            "masternode {} has id {}, not below the registered count {total}",
            dmn.pro_tx_hash, dmn.internal_id
        ));
    }
    Ok(())
}

/// Decodes a whole row value with `decode`.
fn decode_whole<T>(
    what: &'static str,
    block: &Hash256,
    mut bytes: &[u8],
    decode: impl FnOnce(&mut &[u8]) -> std::result::Result<T, dash_dml::codec::CodecError>,
) -> Result<T> {
    let value = decode(&mut bytes)
        .map_err(|e| Error::malformed(what, format!("block {}: {}", hash_to_hex(block), e.0)))?;
    if !bytes.is_empty() {
        return Err(Error::malformed(
            what,
            format!("block {}: {} trailing bytes", hash_to_hex(block), bytes.len()),
        ));
    }
    Ok(value)
}
