// SPDX-License-Identifier: MIT

//! The coin statistics index's reader: Core's `LookUpStats` over the height rows and the
//! hash-keyed archive.

use std::sync::Arc;

use dashd_chainstate::subscriber::height_key;
use dashd_store::{Cf, KvRead};

use super::codec;
use super::data::{ARCHIVE, HEIGHT, decode_height_value};
use crate::{BlockRef, CoinStatsEntry, CoinStatsReader, IndexError, IndexStatus, IndexSummary};

/// The coin statistics index's reads, over its store, with progress from `status`.
pub struct StoredCoinStats<R> {
    db: Arc<R>,
    status: Arc<dyn IndexStatus>,
}

impl<R: KvRead + Send + Sync> StoredCoinStats<R> {
    pub fn new(db: Arc<R>, status: Arc<dyn IndexStatus>) -> Self {
        Self {
            db,
            status,
        }
    }

    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, IndexError> {
        self.db.get(cf, key).map_err(|err| IndexError::Io(format!("coinstats read: {err}")))
    }
}

impl<R: KvRead + Send + Sync> IndexStatus for StoredCoinStats<R> {
    fn summary(&self) -> IndexSummary {
        self.status.summary()
    }

    fn block_until_synced(&self) -> bool {
        self.status.block_until_synced()
    }
}

impl<R: KvRead + Send + Sync> CoinStatsReader for StoredCoinStats<R> {
    /// The height row if it names `block`; otherwise, and also when the row is absent (rdashd
    /// deletes height rows above L, which Core never does), the archive row.
    // Core: index/coinstatsindex.cpp:304-321
    fn look_up_stats(&self, block: &BlockRef) -> Result<Option<CoinStatsEntry>, IndexError> {
        if let Some(v) = self.get(HEIGHT, &height_key(block.height, &[]))? {
            let (hash, entry) = decode_height_value(&v)?;
            if hash == block.hash {
                return Ok(Some(entry));
            }
        }
        self.get(ARCHIVE, block.hash.as_bytes())?.map(|v| codec::decode(&v)).transpose()
    }
}

#[cfg(test)]
mod tests {
    use dash_consensus::BlockId;
    use dashd_store::{Commit, ErrorKind, KvBatch, KvEngine, MemoryEngine, StoreError};

    use super::*;
    use crate::BlockHash;
    use crate::coinstats::data::{COINSTATS_DB, encode_height_value};

    struct Status;

    impl IndexStatus for Status {
        fn summary(&self) -> IndexSummary {
            IndexSummary {
                name: "coinstatsindex",
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
            unreachable!()
        }

        fn iter_prefix<'a>(
            &'a self,
            _: Cf,
            _: &[u8],
        ) -> impl Iterator<Item = dashd_store::Result<(Vec<u8>, Vec<u8>)>> + 'a {
            std::iter::empty()
        }
    }

    fn block(k: u8, height: u32) -> BlockRef {
        BlockRef {
            id: BlockId(u32::from(k)),
            hash: BlockHash::from([k; 32]),
            height,
        }
    }

    fn entry(k: u8) -> CoinStatsEntry {
        CoinStatsEntry {
            muhash: [k; 32],
            txouts: u64::from(k),
            total_amount: -i64::from(k),
            ..CoinStatsEntry::default()
        }
    }

    fn reader(rows: &[(Cf, Vec<u8>, Vec<u8>)]) -> StoredCoinStats<MemoryEngine> {
        let engine = MemoryEngine::new(&COINSTATS_DB);
        let mut batch = engine.batch();
        for (cf, k, v) in rows {
            batch.put(*cf, k, v);
        }
        engine.write(batch, Commit::Durable).unwrap();
        StoredCoinStats::new(Arc::new(engine), Arc::new(Status))
    }

    fn height_row(b: &BlockRef, e: &CoinStatsEntry) -> (Cf, Vec<u8>, Vec<u8>) {
        (HEIGHT, height_key(b.height, &[]), encode_height_value(&b.hash, e))
    }

    fn archive_row(b: &BlockRef, e: &CoinStatsEntry) -> (Cf, Vec<u8>, Vec<u8>) {
        (ARCHIVE, b.hash.as_bytes().to_vec(), codec::encode(e).to_vec())
    }

    #[test]
    fn a_height_row_naming_the_block_answers_before_the_archive() {
        let b = block(1, 5);
        let r = reader(&[height_row(&b, &entry(1)), archive_row(&b, &entry(2))]);
        assert_eq!(r.look_up_stats(&b), Ok(Some(entry(1))));
    }

    #[test]
    fn a_height_row_naming_another_block_falls_back_to_the_archive() {
        let (active, stale) = (block(1, 5), block(2, 5));
        let r = reader(&[height_row(&active, &entry(1)), archive_row(&stale, &entry(2))]);
        assert_eq!(r.look_up_stats(&stale), Ok(Some(entry(2))));
        assert_eq!(r.look_up_stats(&active), Ok(Some(entry(1))));
    }

    #[test]
    fn an_absent_height_row_falls_back_to_the_archive() {
        let stale = block(3, 9);
        let r = reader(&[height_row(&block(1, 5), &entry(1)), archive_row(&stale, &entry(3))]);
        assert_eq!(r.look_up_stats(&stale), Ok(Some(entry(3))));
    }

    #[test]
    fn a_block_in_neither_is_a_miss() {
        let r =
            reader(&[height_row(&block(1, 5), &entry(1)), archive_row(&block(3, 9), &entry(3))]);
        assert_eq!(r.look_up_stats(&block(4, 5)), Ok(None));
        assert_eq!(r.look_up_stats(&block(4, 6)), Ok(None));
    }

    #[test]
    fn undecodable_rows_and_failed_reads_are_errors() {
        let b = block(1, 5);
        let short = (HEIGHT, height_key(5, &[]), vec![1; 40]);
        assert!(matches!(reader(&[short]).look_up_stats(&b), Err(IndexError::Corrupt(_))));
        let bad_archive = (ARCHIVE, b.hash.as_bytes().to_vec(), vec![0; 3]);
        assert!(matches!(reader(&[bad_archive]).look_up_stats(&b), Err(IndexError::Corrupt(_))));
        let r = StoredCoinStats::new(Arc::new(Failing), Arc::new(Status));
        assert!(matches!(r.look_up_stats(&b), Err(IndexError::Io(_))));
    }

    #[test]
    fn status_comes_from_the_injected_source() {
        let r = reader(&[]);
        assert_eq!(r.summary(), Status.summary());
        assert!(r.block_until_synced());
    }
}
