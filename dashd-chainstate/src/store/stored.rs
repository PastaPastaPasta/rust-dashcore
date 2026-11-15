//! What a node's storage holds for its next load, read before any load logic (CH3a a10; O2b D37).
//! The shim's `Persisted()` reads the same records from Core's databases between a teardown and
//! the load; the session's comparison digests both (node-l1).

use dash_chain::BlockIndexRecord;
use dash_consensus::primitives::BlockHash;
use dashd_store::{KvEngine, KvRead as _, StoreError};

use super::{CHAINSTATE_SCHEMA, CoinsReadError, GuardedEngine, Meta};

/// The `meta` records and every `block_index` record, header-only entries included, in key order:
/// the hash's internal bytes, as Core's LevelDB orders its `DB_BLOCK_INDEX` keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub meta: Meta,
    pub index: Vec<(BlockHash, BlockIndexRecord)>,
}

impl Stored {
    /// Reads `db` as it is.
    ///
    /// # Errors
    ///
    /// The engine failed, or a record does not decode: an image is never empty or partial.
    pub fn read<E: KvEngine>(db: &GuardedEngine<E>) -> Result<Stored, StoreError> {
        let meta = Meta::read(db).map_err(CoinsReadError::into_store_error)?;
        let cf = CHAINSTATE_SCHEMA.cf("block_index").expect("the chainstate schema has it");
        let index = db
            .iter_prefix(cf, &[])
            .map(|r| {
                let (key, value) = r?;
                let corrupt = |detail: &str| db.corrupt(cf, &key, detail).into_store_error();
                let hash = <[u8; 32]>::try_from(key.as_slice())
                    .map_err(|_| corrupt(&format!("a {}-byte key", key.len())))?;
                let record =
                    BlockIndexRecord::decode(&value).map_err(|e| corrupt(&e.to_string()))?;
                Ok((BlockHash::from(hash), record))
            })
            .collect::<Result<_, StoreError>>()?;
        Ok(Stored {
            meta,
            index,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_chain::Status;
    use dashd_store::{Commit, ErrorKind, KvBatch as _};

    use super::*;
    use crate::store::{ProbeEngine, RecordingSink};

    fn db() -> GuardedEngine<ProbeEngine> {
        GuardedEngine::probe(Arc::new(RecordingSink::default()))
    }

    fn record(height: u32, status: u32) -> BlockIndexRecord {
        let header = {
            let base: dash_primitives::BlockHeader = Default::default();
            dash_primitives::BlockHeader::new(
                base.version(),
                base.prev_hash(),
                base.merkle_root(),
                base.time(),
                base.bits(),
                height,
            )
        };
        BlockIndexRecord {
            height,
            status: Status(status),
            n_tx: 0,
            file: None,
            data_pos: None,
            undo_pos: None,
            header,
        }
    }

    #[test]
    fn the_read_is_every_record_in_key_order_and_the_markers() {
        let db = db();
        assert_eq!(
            Stored::read(&db).unwrap(),
            Stored {
                meta: Meta::default(),
                index: Vec::new(),
            },
            "absent markers read as none"
        );
        let cf = CHAINSTATE_SCHEMA.cf("block_index").unwrap();
        // A header-only entry is stored like any other, and keys sort by their bytes.
        let records = [([2; 32], record(1, Status::VALID_TREE)), ([1; 32], record(0, 0))];
        let mut b = db.batch();
        for (hash, r) in &records {
            b.put(cf, hash, &r.to_bytes());
        }
        let tip = BlockHash::from([2; 32]);
        Meta::put_tip(&mut b, &tip);
        Meta::put_head_blocks(&mut b, &tip, None);
        db.write(b, Commit::Durable).unwrap();
        let stored = Stored::read(&db).unwrap();
        assert_eq!(stored.meta.tip, Some(tip));
        assert_eq!(stored.meta.head_blocks, Some((tip, None)));
        assert_eq!(stored.meta.evo_tip, None);
        let keys: Vec<_> = stored.index.iter().map(|(h, _)| *h.as_bytes()).collect();
        assert_eq!(keys, [[1; 32], [2; 32]]);
        assert_eq!(stored.index[1].1, records[0].1);
    }

    #[test]
    fn a_read_error_or_an_undecodable_record_fails_the_read() {
        let db = db();
        let cf = CHAINSTATE_SCHEMA.cf("block_index").unwrap();
        let mut b = db.batch();
        b.put(cf, &[1; 32], &record(0, 0).to_bytes());
        b.put(cf, &[2; 32], &[0xff]);
        db.write(b, Commit::Durable).unwrap();
        let err = Stored::read(&db).unwrap_err();
        assert_eq!(err.kind(), Some(ErrorKind::Corruption), "{err}");

        let db = self::db();
        db.inner().fail_reads(Some(ErrorKind::Io));
        let err = Stored::read(&db).unwrap_err();
        assert_eq!(err.kind(), Some(ErrorKind::Io), "{err}");
    }
}
