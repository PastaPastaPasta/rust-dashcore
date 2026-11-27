// SPDX-License-Identifier: MIT

//! The basic block filter index's reader: Core's `LookupFilter`, `LookupFilterHeader`,
//! `LookupFilterRange` and `LookupFilterHashRange` over the height rows, the hash-keyed archive
//! and the `fltr?????.dat` files.

use std::sync::Arc;

use dashd_chainstate::subscriber::{FilePos, height_key};
use dashd_store::{Cf, KvRead};

use super::codec::{DbVal, HeightValue};
use super::data::{ARCHIVE, HEIGHT};
use super::files::FilterFiles;
use crate::{
    Ancestry, BlockFilter, BlockRef, FilterHash, FilterHeader, FilterIndexReader, IndexError,
    IndexStatus, IndexSummary,
};

/// The basic filter index's reads, over its store and files, with progress from `status`.
pub struct StoredFilters<R> {
    db: Arc<R>,
    files: FilterFiles,
    status: Arc<dyn IndexStatus>,
}

impl<R: KvRead + Send + Sync> StoredFilters<R> {
    pub fn new(db: Arc<R>, files: FilterFiles, status: Arc<dyn IndexStatus>) -> Self {
        Self {
            db,
            files,
            status,
        }
    }

    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, IndexError> {
        self.db.get(cf, key).map_err(|err| IndexError::Io(format!("block filter read: {err}")))
    }

    /// The height row if it names `block`; otherwise, and also when the row is absent (rdashd
    /// deletes height rows above L, which Core never does), the archive row.
    // Core: index/blockfilterindex.cpp:362-378, :417-433
    fn resolve(&self, block: &BlockRef, row: Option<Vec<u8>>) -> Result<Option<DbVal>, IndexError> {
        if let Some(v) = row {
            let value = HeightValue::decode(&v)?;
            if value.block_hash == block.hash {
                return Ok(Some(value.filter));
            }
        }
        self.get(ARCHIVE, block.hash.as_bytes())?.map(|v| DbVal::decode(&v)).transpose()
    }

    fn entry(&self, block: &BlockRef) -> Result<Option<DbVal>, IndexError> {
        let row = self.get(HEIGHT, &height_key(block.height, &[]))?;
        self.resolve(block, row)
    }

    /// The entries from `start` through `stop` along `stop`'s ancestry, in height order.
    // Core: index/blockfilterindex.cpp:380-436
    fn entries(
        &self,
        start: u32,
        stop: &BlockRef,
        ancestry: &dyn Ancestry,
    ) -> Result<Option<Vec<DbVal>>, IndexError> {
        if start > stop.height {
            return Ok(None);
        }
        let mut blocks = Vec::with_capacity((stop.height - start) as usize + 1);
        for height in start..stop.height {
            match ancestry.ancestor(stop, height) {
                Some(block) => blocks.push(block),
                None => return Ok(None),
            }
        }
        blocks.push(*stop);
        let keys: Vec<Vec<u8>> = blocks.iter().map(|b| height_key(b.height, &[])).collect();
        let keys: Vec<&[u8]> = keys.iter().map(Vec::as_slice).collect();
        let rows = self
            .db
            .multi_get(HEIGHT, &keys)
            .map_err(|err| IndexError::Io(format!("block filter range read: {err}")))?;
        let mut entries = Vec::with_capacity(blocks.len());
        for (block, row) in blocks.iter().zip(rows) {
            match self.resolve(block, row)? {
                Some(entry) => entries.push(entry),
                None => return Ok(None),
            }
        }
        Ok(Some(entries))
    }

    /// The filter at the entry's position, whose hash must be the entry's. It is keyed by the
    /// block hash stored in the file, and its elements are not decoded.
    // Core: index/blockfilterindex.cpp:189-218
    fn read_filter(&self, entry: &DbVal) -> Result<BlockFilter, IndexError> {
        let pos = FilePos {
            file: u32::try_from(entry.pos.file)
                .map_err(|_| IndexError::Corrupt("negative filter file number".into()))?,
            offset: entry.pos.pos,
        };
        let (block_hash, encoded) = self
            .files
            .read(pos)
            .map_err(|err| IndexError::Io(format!("block filter file read at {pos:?}: {err}")))?;
        if dash_filters::filter_hash(&encoded) != entry.filter_hash {
            return Err(IndexError::Corrupt(format!("filter checksum mismatch at {pos:?}")));
        }
        BlockFilter::from_encoded(&block_hash, encoded, true)
            .map_err(|err| IndexError::Corrupt(format!("filter at {pos:?}: {err}")))
    }
}

impl<R: KvRead + Send + Sync> IndexStatus for StoredFilters<R> {
    fn summary(&self) -> IndexSummary {
        self.status.summary()
    }

    fn block_until_synced(&self) -> bool {
        self.status.block_until_synced()
    }
}

impl<R: KvRead + Send + Sync> FilterIndexReader for StoredFilters<R> {
    // Core: index/blockfilterindex.cpp:439-447
    fn filter(&self, block: &BlockRef) -> Result<Option<BlockFilter>, IndexError> {
        self.entry(block)?.map(|entry| self.read_filter(&entry)).transpose()
    }

    /// Uncached: Core's checkpoint header cache is dashd-p2p's `FilterServer`
    /// (`dashd-p2p/src/processing/serve_filters.rs`), the one caller that uses it.
    // Core: index/blockfilterindex.cpp:449-477
    fn filter_header(&self, block: &BlockRef) -> Result<Option<FilterHeader>, IndexError> {
        Ok(self.entry(block)?.map(|entry| entry.header))
    }

    // Core: index/blockfilterindex.cpp:479-508
    fn filter_range(
        &self,
        start: u32,
        stop: &BlockRef,
        ancestry: &dyn Ancestry,
    ) -> Result<Option<Vec<BlockFilter>>, IndexError> {
        self.entries(start, stop, ancestry)?
            .map(|entries| entries.iter().map(|entry| self.read_filter(entry)).collect())
            .transpose()
    }

    // Core: index/blockfilterindex.cpp:510-524
    fn filter_hash_range(
        &self,
        start: u32,
        stop: &BlockRef,
        ancestry: &dyn Ancestry,
    ) -> Result<Option<Vec<FilterHash>>, IndexError> {
        Ok(self
            .entries(start, stop, ancestry)?
            .map(|entries| entries.iter().map(|entry| entry.filter_hash).collect()))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use dash_consensus::BlockId;
    use dash_types::Numeric as _;
    use dashd_store::flatfile::OsFs;
    use dashd_store::{Commit, ErrorKind, KvBatch, KvEngine, MemoryEngine, StoreError};

    use super::*;
    use crate::BlockHash;
    use crate::blockfilter::codec::FilterPos;
    use crate::blockfilter::data::BLOCKFILTER_DB;

    struct Status;

    impl IndexStatus for Status {
        fn summary(&self) -> IndexSummary {
            IndexSummary {
                name: "basic block filter index",
                synced: true,
                best_block_height: 7,
            }
        }

        fn block_until_synced(&self) -> bool {
            true
        }
    }

    struct Failing;

    impl KvRead for Failing {
        fn get(&self, _: Cf, _: &[u8]) -> dashd_store::Result<Option<Vec<u8>>> {
            Err(StoreError::engine("test", ErrorKind::Io, "a failed read"))
        }

        fn multi_get(&self, _: Cf, _: &[&[u8]]) -> dashd_store::Result<Vec<Option<Vec<u8>>>> {
            Err(StoreError::engine("test", ErrorKind::Io, "a failed read"))
        }

        fn iter_prefix<'a>(
            &'a self,
            _: Cf,
            _: &[u8],
        ) -> impl Iterator<Item = dashd_store::Result<(Vec<u8>, Vec<u8>)>> + 'a {
            std::iter::empty()
        }
    }

    /// Blocks `k` at their heights, each the next one's parent.
    struct Chain(Vec<BlockRef>);

    impl Ancestry for Chain {
        fn ancestor(&self, stop: &BlockRef, height: u32) -> Option<BlockRef> {
            assert_eq!(Some(stop), self.0.last());
            self.0.iter().find(|b| b.height == height).copied()
        }
    }

    fn block(k: u8, height: u32) -> BlockRef {
        BlockRef {
            id: BlockId(u32::from(k)),
            hash: BlockHash::from([k; 32]),
            height,
        }
    }

    fn entry(k: u8) -> DbVal {
        DbVal {
            filter_hash: FilterHash::from_lendian([k; 32]),
            header: FilterHeader::from_lendian([k.wrapping_add(100); 32]),
            pos: FilterPos {
                file: 0,
                pos: u64::from(k),
            },
        }
    }

    fn files() -> FilterFiles {
        FilterFiles::new(Arc::new(OsFs::new()), PathBuf::from("/nonexistent/blockfilter/basic"))
    }

    fn reader(rows: &[(Cf, Vec<u8>, Vec<u8>)]) -> StoredFilters<MemoryEngine> {
        let engine = MemoryEngine::new(&BLOCKFILTER_DB);
        let mut batch = engine.batch();
        for (cf, k, v) in rows {
            batch.put(*cf, k, v);
        }
        engine.write(batch, Commit::Durable).unwrap();
        StoredFilters::new(Arc::new(engine), files(), Arc::new(Status))
    }

    fn height_row(b: &BlockRef, e: &DbVal) -> (Cf, Vec<u8>, Vec<u8>) {
        let value = HeightValue {
            block_hash: b.hash,
            filter: *e,
        };
        (HEIGHT, height_key(b.height, &[]), value.encode().unwrap())
    }

    fn archive_row(b: &BlockRef, e: &DbVal) -> (Cf, Vec<u8>, Vec<u8>) {
        (ARCHIVE, b.hash.as_bytes().to_vec(), e.encode().unwrap())
    }

    #[test]
    fn a_height_row_naming_the_block_answers_before_the_archive() {
        let b = block(1, 5);
        let r = reader(&[height_row(&b, &entry(1)), archive_row(&b, &entry(2))]);
        assert_eq!(r.filter_header(&b), Ok(Some(entry(1).header)));
    }

    #[test]
    fn a_height_row_naming_another_block_or_absent_falls_back_to_the_archive() {
        let (active, stale, above) = (block(1, 5), block(2, 5), block(3, 9));
        let r = reader(&[
            height_row(&active, &entry(1)),
            archive_row(&stale, &entry(2)),
            archive_row(&above, &entry(3)),
        ]);
        assert_eq!(r.filter_header(&active), Ok(Some(entry(1).header)));
        assert_eq!(r.filter_header(&stale), Ok(Some(entry(2).header)));
        assert_eq!(r.filter_header(&above), Ok(Some(entry(3).header)));
        assert_eq!(r.filter_header(&block(4, 5)), Ok(None));
        assert_eq!(r.filter_header(&block(4, 6)), Ok(None));
        assert_eq!(r.filter(&block(4, 5)), Ok(None));
    }

    /// A stale branch 0-1-2'-3' over the active rows 0-1-2-3: the range mixes height and
    /// archive rows in height order, and a gap anywhere is a miss.
    #[test]
    fn hash_ranges_follow_the_stop_blocks_ancestry() {
        let active: Vec<BlockRef> = (0..4).map(|h| block(h, u32::from(h))).collect();
        let stale = vec![active[0], active[1], block(12, 2), block(13, 3)];
        let mut rows: Vec<_> = active.iter().map(|b| height_row(b, &entry(b.id.0 as u8))).collect();
        rows.extend(stale[2..].iter().map(|b| archive_row(b, &entry(b.id.0 as u8))));
        let r = reader(&rows);
        let hashes =
            |chain: &[BlockRef]| chain.iter().map(|b| entry(b.id.0 as u8).filter_hash).collect();
        for chain in [&active, &stale] {
            let ancestry = Chain(chain.clone());
            let stop = chain.last().unwrap();
            for start in 0..=3 {
                let got = r.filter_hash_range(start, stop, &ancestry);
                assert_eq!(got, Ok(Some(hashes(&chain[start as usize..]))), "from {start}");
            }
            assert_eq!(r.filter_hash_range(4, stop, &ancestry), Ok(None), "start above stop");
        }

        let unknown = vec![active[0], block(21, 1), block(22, 2)];
        let off = Chain(unknown.clone());
        assert_eq!(r.filter_hash_range(0, &unknown[2], &off), Ok(None));
        assert_eq!(r.filter_hash_range(1, &unknown[2], &off), Ok(None));
        let gap = Chain(vec![active[0], active[2], active[3]]);
        assert_eq!(r.filter_hash_range(0, &active[3], &gap), Ok(None), "no ancestor at 1");
        assert_eq!(r.filter_hash_range(2, &active[3], &gap), Ok(Some(hashes(&active[2..]))));
        assert_eq!(r.filter_range(0, &unknown[2], &off), Ok(None));
    }

    #[test]
    fn undecodable_rows_unreadable_files_and_failed_reads_are_errors() {
        let b = block(1, 5);
        let chain = Chain(vec![b]);
        let short = (HEIGHT, height_key(5, &[]), vec![1; 40]);
        let r = reader(&[short]);
        assert!(matches!(r.filter_header(&b), Err(IndexError::Corrupt(_))));
        assert!(matches!(r.filter_hash_range(5, &b, &chain), Err(IndexError::Corrupt(_))));
        let bad_archive = (ARCHIVE, b.hash.as_bytes().to_vec(), vec![0; 3]);
        assert!(matches!(reader(&[bad_archive]).filter(&b), Err(IndexError::Corrupt(_))));

        let r = reader(&[height_row(&b, &entry(1))]);
        assert!(matches!(r.filter(&b), Err(IndexError::Io(_))), "no filter file");
        assert!(matches!(r.filter_range(5, &b, &chain), Err(IndexError::Io(_))));

        let r = StoredFilters::new(Arc::new(Failing), files(), Arc::new(Status));
        assert!(matches!(r.filter(&b), Err(IndexError::Io(_))));
        assert!(matches!(r.filter_header(&b), Err(IndexError::Io(_))));
        assert!(matches!(r.filter_range(5, &b, &chain), Err(IndexError::Io(_))));
        assert!(matches!(r.filter_hash_range(5, &b, &chain), Err(IndexError::Io(_))));
    }

    #[test]
    fn status_comes_from_the_injected_source() {
        let r = reader(&[]);
        assert_eq!(r.summary(), Status.summary());
        assert!(r.block_until_synced());
    }
}
