// SPDX-License-Identifier: MIT

//! The transaction index's reader: Core's `FindTx` over rows keyed by a txid prefix.

use std::sync::Arc;

use dash_consensus::Tx;
use dashd_store::flatfile::{BlockFileReader, FlatFilePos as StorePos, FlatFs};
use dashd_store::{ErrorKind, KvEngine, KvRead};

use super::TX;
use super::codec::{self, PREFIX_LEN, TxKey, TxPos};
use crate::{BlockHash, IndexError, IndexStatus, IndexSummary, IndexedTx, TxIndexReader, Txid};

/// The block-file read a lookup verifies each row with.
pub trait TxFiles: Send + Sync {
    /// The transaction at `pos` and the hash of its block's header.
    // Core: index/txindex.cpp:83-99
    fn read_tx(&self, pos: &TxPos) -> Result<(Tx, BlockHash), IndexError>;
}

impl<F: FlatFs> TxFiles for BlockFileReader<F> {
    fn read_tx(&self, pos: &TxPos) -> Result<(Tx, BlockHash), IndexError> {
        let file = u32::try_from(pos.block.file)
            .map_err(|_| IndexError::Corrupt("negative block file number".into()))?;
        let at = StorePos::new(file, pos.block.pos);
        let (header, tx) =
            BlockFileReader::read_tx(self, at, pos.tx_offset).map_err(|err| match err.kind() {
                ErrorKind::Corruption => IndexError::Corrupt(format!("txindex read: {err}")),
                _ => IndexError::Io(format!("txindex read: {err}")),
            })?;
        Ok((tx, header.hash()))
    }
}

/// The transaction index's reads, over its store and the block files.
///
/// A lookup takes the rows under the txid's prefix from one snapshot, then reads them from the
/// block files in descending sequence order. The first whose transaction has the full txid is
/// Core's last append for it; a row holding another transaction is skipped, and a failed read
/// ends the lookup with its error.
///
/// Two outcomes differ from Core only on damaged block files: if the txid's highest row reads
/// back as another transaction, a lower row for the txid can answer where Core misses; and a
/// failed read of a higher row of another txid sharing the prefix fails the lookup.
pub struct StoredTxIndex<E> {
    db: Arc<E>,
    files: Arc<dyn TxFiles>,
    status: Arc<dyn IndexStatus>,
    #[cfg(test)]
    prefix_hook: Option<fn(&Txid) -> [u8; PREFIX_LEN]>,
}

impl<E: KvEngine> StoredTxIndex<E> {
    pub fn new(db: Arc<E>, files: Arc<dyn TxFiles>, status: Arc<dyn IndexStatus>) -> Self {
        Self {
            db,
            files,
            status,
            #[cfg(test)]
            prefix_hook: None,
        }
    }

    fn prefix(&self, txid: &Txid) -> [u8; PREFIX_LEN] {
        #[cfg(test)]
        if let Some(hook) = self.prefix_hook {
            return hook(txid);
        }
        codec::prefix(txid)
    }

    /// The positions of the rows under `txid`'s prefix, highest sequence number first.
    fn positions(&self, txid: &Txid) -> Result<Vec<TxPos>, IndexError> {
        let snapshot = self.db.snapshot();
        let mut rows = snapshot
            .iter_prefix(TX, &self.prefix(txid))
            .map(|row| {
                let (key, value) =
                    row.map_err(|err| IndexError::Io(format!("txindex row read: {err}")))?;
                Ok((codec::decode_seq(&value)?, TxKey::decode(&key)?.pos))
            })
            .collect::<Result<Vec<_>, IndexError>>()?;
        rows.sort_by_key(|&(seq, _)| std::cmp::Reverse(seq));
        Ok(rows.into_iter().map(|(_, pos)| pos).collect())
    }
}

impl<E: KvEngine> IndexStatus for StoredTxIndex<E> {
    fn summary(&self) -> IndexSummary {
        self.status.summary()
    }

    fn block_until_synced(&self) -> bool {
        self.status.block_until_synced()
    }
}

impl<E: KvEngine> TxIndexReader for StoredTxIndex<E> {
    // Core: index/txindex.cpp:76-105
    fn find_tx(&self, txid: &Txid) -> Result<Option<IndexedTx>, IndexError> {
        for pos in self.positions(txid)? {
            let (tx, block_hash) = self.files.read_tx(&pos)?;
            if tx.txid() == *txid {
                return Ok(Some((Arc::new(tx.into_raw()), block_hash)));
            }
        }
        Ok(None)
    }

    // Core: index/txindex.cpp:108-112
    fn has_tx(&self, txid: &Txid) -> Result<bool, IndexError> {
        Ok(self.find_tx(txid)?.is_some())
    }
}

#[cfg(all(test, feature = "crash"))]
mod tests {
    use dash_chain::{BlockRef, FlatFilePos};
    use dash_consensus::primitives::compact_size_len;
    use dash_consensus::{Block, decode_block};
    use dashd_chainstate::flush::SubscriberHub;
    use dashd_chainstate::store::{GlobalFailStop, GuardedEngine, RecordingSink};
    use dashd_chainstate::subscriber::{
        BlockSource, DriverConfig, ScriptedFeeds, SubStep, SubscriberDriver, SubscriberError,
        SyncProgress, TreeIndex,
    };
    use dashd_store::{Commit, KvBatch, MemoryEngine};

    use super::*;
    use crate::txindex::{TXINDEX_DB, TxIndexData};
    use crate::{Transaction, TxPositions};

    const BLOCKS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dashd-v24/blocks");
    const REORG_BLOCKS: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dashd-v24-reorg/blocks");
    /// The reorg fixture's T, in A10 and in B9.
    const T: &str = "5c0a8c69e96690f6e5f9845c23065832da9747e32a1dd375b39a3d70e7ca48de";
    const A10: &str = "69659c49ff6bc5545c61c7f5356b1b88ae7f00add7520f220aa4ad396aed0361";
    const B9: &str = "4d38234ad9bb9f5081c7e1ea77f166d3f8374c1491ca343e47e60b06f9342285";

    /// Each block record of a fixture's `blk00000.dat`: its data position and block.
    fn records(dir: &str) -> Vec<(FlatFilePos, Arc<Block>)> {
        let bytes = std::fs::read(format!("{dir}/blk00000.dat")).unwrap();
        let mut out = Vec::new();
        let mut at = 0;
        while let Some(prefix) = bytes.get(at..at + 8)
            && prefix != [0; 8]
        {
            let size = u32::from_le_bytes(prefix[4..].try_into().unwrap()) as usize;
            let start = at + 8;
            let block = decode_block(&mut &bytes[start..start + size]).unwrap().into_value();
            out.push((
                FlatFilePos {
                    file: 0,
                    pos: start as u32,
                },
                Arc::new(block),
            ));
            at = start + size;
        }
        out
    }

    /// The position of `block`'s transaction `index`.
    fn tx_pos(block: FlatFilePos, data: &Block, index: usize) -> TxPos {
        let txs = data.txs();
        let before: u32 = txs[..index].iter().map(|tx| tx.size()).sum();
        TxPos {
            block,
            tx_offset: compact_size_len(txs.len() as u64) as u32 + before,
        }
    }

    struct Status(IndexSummary);

    impl IndexStatus for Status {
        fn summary(&self) -> IndexSummary {
            self.0.clone()
        }

        fn block_until_synced(&self) -> bool {
            self.0.synced
        }
    }

    fn status() -> Arc<dyn IndexStatus> {
        Arc::new(Status(IndexSummary {
            name: "txindex",
            synced: true,
            best_block_height: 7,
        }))
    }

    fn reader<E: KvEngine>(db: Arc<E>, dir: &str) -> StoredTxIndex<E> {
        StoredTxIndex::new(db, Arc::new(BlockFileReader::open_read_only(dir)), status())
    }

    /// `find_tx`, after checking that `has_tx` gives the same answer.
    fn find<E: KvEngine>(
        index: &StoredTxIndex<E>,
        txid: &Txid,
    ) -> Result<Option<IndexedTx>, IndexError> {
        let found = index.find_tx(txid);
        assert_eq!(index.has_tx(txid), found.clone().map(|f| f.is_some()), "has_tx({txid})");
        found
    }

    fn found(tx: &Transaction, block: BlockHash) -> Result<Option<IndexedTx>, IndexError> {
        Ok(Some((Arc::new(tx.clone()), block)))
    }

    fn put(db: &MemoryEngine, rows: &[([u8; PREFIX_LEN], TxPos, u64)]) {
        let mut batch = db.batch();
        for (prefix, pos, seq) in rows {
            let key = TxKey {
                prefix: *prefix,
                pos: *pos,
            };
            batch.put(TX, &key.encode().unwrap(), &codec::encode_seq(*seq));
        }
        db.write(batch, Commit::Buffered).unwrap();
    }

    /// The reorg fixture's T in A10 and B9, and another transaction of A10.
    struct Reorg {
        t: Txid,
        tx: Transaction,
        a10: (TxPos, BlockHash),
        b9: (TxPos, BlockHash),
        other: TxPos,
    }

    fn reorg() -> Reorg {
        let t: Txid = T.parse().unwrap();
        let holding: Vec<_> = records(REORG_BLOCKS)
            .into_iter()
            .filter_map(|(pos, block)| {
                let index = block.txs().iter().position(|tx| tx.txid() == t)?;
                Some((tx_pos(pos, &block, index), block, index))
            })
            .collect();
        let [(a10_pos, a10, a10_index), (b9_pos, b9, _)] = &holding[..] else {
            panic!("T is in {} blocks", holding.len());
        };
        assert_eq!(a10.hash().to_string(), A10);
        assert_eq!(b9.hash().to_string(), B9);
        assert_ne!(*a10_index, 0);
        Reorg {
            t,
            tx: a10.txs()[*a10_index].raw().clone(),
            a10: (*a10_pos, a10.hash()),
            b9: (*b9_pos, b9.hash()),
            other: tx_pos(a10_pos.block, a10, 0),
        }
    }

    fn missing_file() -> TxPos {
        TxPos {
            block: FlatFilePos {
                file: 9,
                pos: 8,
            },
            tx_offset: 1,
        }
    }

    #[test]
    fn the_highest_sequence_match_wins_whatever_the_key_order() {
        let r = reorg();
        let p = codec::prefix(&r.t);
        assert!(r.a10.0.block.pos < r.b9.0.block.pos, "A10 sorts first by key");
        for (a10_seq, b9_seq, want) in [(5, 3, r.a10.1), (3, 5, r.b9.1)] {
            let db = Arc::new(MemoryEngine::new(&TXINDEX_DB));
            put(&db, &[(p, r.a10.0, a10_seq), (p, r.b9.0, b9_seq)]);
            assert_eq!(find(&reader(db, REORG_BLOCKS), &r.t), found(&r.tx, want));
        }
    }

    #[test]
    fn a_mismatching_higher_row_is_skipped() {
        let r = reorg();
        let p = codec::prefix(&r.t);
        let db = Arc::new(MemoryEngine::new(&TXINDEX_DB));
        put(&db, &[(p, r.b9.0, 1), (p, r.other, 2)]);
        assert_eq!(find(&reader(db, REORG_BLOCKS), &r.t), found(&r.tx, r.b9.1));
        let only_others = Arc::new(MemoryEngine::new(&TXINDEX_DB));
        put(&only_others, &[(p, r.other, 2)]);
        assert_eq!(find(&reader(only_others, REORG_BLOCKS), &r.t), Ok(None));
    }

    #[test]
    fn a_failed_read_of_the_winning_row_is_an_error_not_a_fall_through() {
        let r = reorg();
        let p = codec::prefix(&r.t);
        let past_the_record = TxPos {
            tx_offset: 0x00ff_0000,
            ..r.a10.0
        };
        for (bad, is_io) in [(missing_file(), true), (past_the_record, false)] {
            let db = Arc::new(MemoryEngine::new(&TXINDEX_DB));
            put(&db, &[(p, r.b9.0, 1), (p, bad, 2)]);
            let err = find(&reader(db, REORG_BLOCKS), &r.t).unwrap_err();
            assert_eq!(matches!(err, IndexError::Io(_)), is_io, "{err:?}");
            assert_eq!(matches!(err, IndexError::Corrupt(_)), !is_io, "{err:?}");
        }
        // A failed read below the match is never reached.
        let db = Arc::new(MemoryEngine::new(&TXINDEX_DB));
        put(&db, &[(p, r.b9.0, 3), (p, missing_file(), 2)]);
        assert_eq!(find(&reader(db, REORG_BLOCKS), &r.t), found(&r.tx, r.b9.1));
    }

    #[test]
    fn an_absent_prefix_is_none() {
        let r = reorg();
        let db = Arc::new(MemoryEngine::new(&TXINDEX_DB));
        assert_eq!(find(&reader(Arc::clone(&db), REORG_BLOCKS), &r.t), Ok(None));
        let mut near = codec::prefix(&r.t);
        near[PREFIX_LEN - 1] ^= 1;
        put(&db, &[(near, r.a10.0, 1), (near, missing_file(), 2)]);
        assert_eq!(find(&reader(db, REORG_BLOCKS), &r.t), Ok(None));
    }

    #[test]
    fn undecodable_rows_are_corrupt() {
        let r = reorg();
        let p = codec::prefix(&r.t);
        let key = TxKey {
            prefix: p,
            pos: r.a10.0,
        }
        .encode()
        .unwrap();
        let mut long_key = key.to_vec();
        long_key.push(0);
        for (key, value) in [(key.to_vec(), vec![0x80]), (long_key, codec::encode_seq(1))] {
            let db = Arc::new(MemoryEngine::new(&TXINDEX_DB));
            let mut batch = db.batch();
            batch.put(TX, &key, &value);
            db.write(batch, Commit::Buffered).unwrap();
            let err = find(&reader(db, REORG_BLOCKS), &r.t).unwrap_err();
            assert!(matches!(err, IndexError::Corrupt(_)), "{err:?}");
        }
    }

    #[test]
    fn the_status_comes_from_the_injected_source() {
        let db = Arc::new(MemoryEngine::new(&TXINDEX_DB));
        let index = reader(Arc::clone(&db), REORG_BLOCKS);
        assert_eq!(index.summary(), status().summary());
        assert!(index.block_until_synced());
        let syncing = IndexSummary {
            name: "txindex",
            synced: false,
            best_block_height: 3,
        };
        let files = Arc::new(BlockFileReader::open_read_only(REORG_BLOCKS));
        let index = StoredTxIndex::new(db, files, Arc::new(Status(syncing.clone())));
        assert_eq!(index.summary(), syncing);
        assert!(!index.block_until_synced());
    }

    /// The tree's block at height h is `blocks[h]`, at its fixture position.
    struct Chosen(Vec<(FlatFilePos, Arc<Block>)>);

    impl BlockSource for Chosen {
        fn block(&self, b: &BlockRef) -> Result<Arc<Block>, SubscriberError> {
            Ok(Arc::clone(&self.0[b.height as usize].1))
        }

        fn undo(&self, b: &BlockRef) -> Result<Vec<u8>, SubscriberError> {
            Err(SubscriberError::Blocks(format!("no undo for {}", b.hash)))
        }
    }

    impl TxPositions for Chosen {
        fn data_pos(&self, b: &BlockRef) -> Result<FlatFilePos, IndexError> {
            Ok(self.0[b.height as usize].0)
        }
    }

    /// Genesis, then the first fixture blocks with at least two transactions and none in common.
    fn chosen() -> Arc<Chosen> {
        let mut all = records(BLOCKS).into_iter();
        let mut out = vec![all.next().unwrap()];
        let mut seen = std::collections::HashSet::new();
        for (pos, block) in all {
            if out.len() == 5 {
                break;
            }
            let txids: Vec<_> = block.txs().iter().map(|tx| tx.txid()).collect();
            if txids.len() >= 2 && txids.iter().all(|t| !seen.contains(t)) {
                seen.extend(txids);
                out.push((pos, block));
            }
        }
        assert_eq!(out.len(), 5);
        Arc::new(Chosen(out))
    }

    const SHARED: [u8; PREFIX_LEN] = [0x5a; PREFIX_LEN];

    fn shared(_: &Txid) -> [u8; PREFIX_LEN] {
        SHARED
    }

    type Db = GuardedEngine<MemoryEngine>;

    fn open(
        db: &Arc<Db>,
        tree: &TreeIndex,
        blocks: &Arc<Chosen>,
        chain: Vec<BlockRef>,
    ) -> (SubscriberDriver<TxIndexData<MemoryEngine>>, ScriptedFeeds) {
        let feeds = ScriptedFeeds::new(4, chain);
        let hub =
            Arc::new(SubscriberHub::with_feeds(Arc::new(feeds.clone()), Arc::new(tree.clone())));
        let positions: Arc<dyn TxPositions> = blocks.clone();
        let mut data = TxIndexData::new(&**db, positions).unwrap();
        data.prefix_hook = Some(shared);
        let cfg = DriverConfig {
            journal_budget: 1 << 20,
            unbounded_delete_span: 64,
            d_max: None,
            cadence: 1000,
        };
        let mut d = SubscriberDriver::open(data, Arc::clone(db), hub, blocks.clone(), cfg).unwrap();
        for _ in 0..100 {
            if d.sync_step().unwrap() == SyncProgress::Live {
                return (d, feeds);
            }
        }
        panic!("never live");
    }

    fn drain(d: &mut SubscriberDriver<TxIndexData<MemoryEngine>>) {
        for _ in 0..100 {
            if d.step().unwrap() != SubStep::Progress {
                return;
            }
        }
        panic!("never idle");
    }

    /// Every transaction of every chosen block above genesis is found in its block.
    fn all_found(index: &StoredTxIndex<Db>, blocks: &Chosen, when: &str) {
        for (_, block) in &blocks.0[1..] {
            for tx in block.txs() {
                assert_eq!(find(index, &tx.txid()), found(tx.raw(), block.hash()), "{when}");
            }
        }
    }

    #[test]
    fn a_shared_prefix_in_one_block_is_found_before_and_after_redelivery_and_restart() {
        let blocks = chosen();
        let tree = TreeIndex::new();
        let g = tree.genesis();
        let chain: Vec<_> = std::iter::once(g).chain(tree.branch(g, 4)).collect();
        let db = Arc::new(GuardedEngine::new(
            MemoryEngine::new(&TXINDEX_DB),
            GlobalFailStop::new(),
            Arc::new(RecordingSink::default()),
        ));
        let mut index = reader(Arc::clone(&db), BLOCKS);
        index.prefix_hook = Some(shared);

        let (mut d, feeds) = open(&db, &tree, &blocks, chain.clone());
        let rows = db.iter_prefix(TX, &SHARED).count();
        let txs: usize = blocks.0[1..].iter().map(|(_, b)| b.txs().len()).sum();
        assert_eq!(rows, txs, "every row shares the prefix");
        assert_eq!(db.iter_prefix(TX, &[]).count(), rows);
        all_found(&index, &blocks, "after the first sync");

        for _ in 0..2 {
            feeds.disconnect();
        }
        drain(&mut d);
        for b in &chain[3..] {
            feeds.connect(*b);
        }
        drain(&mut d);
        assert_eq!(db.iter_prefix(TX, &[]).count(), rows, "re-delivery rewrites the same keys");
        all_found(&index, &blocks, "after re-delivery");

        drop(d);
        let (_d, _feeds) = open(&db, &tree, &blocks, chain);
        assert_eq!(db.iter_prefix(TX, &[]).count(), rows);
        all_found(&index, &blocks, "after a restart");
    }
}
