// SPDX-License-Identifier: MIT

//! The transaction index's data object: one inexact row per transaction, stamped with an append
//! sequence number so that a reader can choose Core's last append among rows sharing a prefix.

use std::marker::PhantomData;
use std::sync::Arc;

use dash_chain::Needs;
use dash_consensus::primitives::compact_size_len;
use dashd_chainstate::subscriber::{
    BlockRows, CfRange, ConnectedBlock, DisconnectedBlock, DurableBelow, Row, RowSchema, StateRead,
    SubscriberData, SubscriberError,
};
use dashd_store::{Commit, KvBatch, KvEngine};

use super::codec::{self, PREFIX_LEN, TxKey, TxPos};
use super::{EPOCH, META, TX, TXINDEX_ROWS};
use crate::{IndexError, TxPositions, Txid};

/// The sequence bits below the epoch: 2^40 appends per data object (2^40 - 1 in the last epoch).
pub const SEQ_BITS: u32 = 40;

/// The highest epoch: 2^24 opens, so that `epoch << SEQ_BITS` fits a `u64`.
pub const MAX_EPOCH: u32 = (1 << (64 - SEQ_BITS)) - 1;

/// Core's `TxIndex` as a subscriber's data object. Its state is the next append sequence number.
pub struct TxIndexData<E> {
    positions: Arc<dyn TxPositions>,
    epoch: u32,
    #[cfg(test)]
    pub(super) prefix_hook: Option<fn(&Txid) -> [u8; PREFIX_LEN]>,
    engine: PhantomData<fn() -> E>,
}

impl<E> TxIndexData<E> {
    /// Reads the epoch row and writes its successor Durable. Call it before the driver's open,
    /// on the store the driver will use: every append of this process then outranks every row
    /// an earlier process left, whether or not that row was durable.
    pub fn new(
        engine: &impl KvEngine,
        positions: Arc<dyn TxPositions>,
    ) -> Result<Self, IndexError> {
        let stored = engine
            .get(META, EPOCH)
            .map_err(|err| IndexError::Io(format!("txindex epoch read: {err}")))?
            .as_deref()
            .map(codec::decode_epoch)
            .transpose()?
            .unwrap_or(0);
        let epoch = stored.checked_add(1).filter(|&e| e <= MAX_EPOCH).ok_or_else(|| {
            IndexError::Corrupt(format!(
                "txindex epoch {stored} reached its bound {MAX_EPOCH}; rebuild the index"
            ))
        })?;
        let mut batch = engine.batch();
        batch.put(META, EPOCH, &codec::encode_epoch(epoch));
        engine
            .write(batch, Commit::Durable)
            .map_err(|err| IndexError::Io(format!("txindex epoch write: {err}")))?;
        Ok(Self {
            positions,
            epoch,
            #[cfg(test)]
            prefix_hook: None,
            engine: PhantomData,
        })
    }

    /// This process's epoch, as now stored.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    fn prefix(&self, txid: &Txid) -> [u8; PREFIX_LEN] {
        #[cfg(test)]
        if let Some(hook) = self.prefix_hook {
            return hook(txid);
        }
        codec::prefix(txid)
    }
}

fn data_err(err: impl std::fmt::Display) -> SubscriberError {
    SubscriberError::Data(format!("txindex: {err}"))
}

impl<E: KvEngine + 'static> SubscriberData for TxIndexData<E> {
    type E = E;
    type State = u64;

    fn name(&self) -> &'static str {
        "txindex"
    }

    fn needs(&self) -> Needs {
        Needs {
            block: true,
            undo: false,
        }
    }

    fn start_height(&self) -> u32 {
        0
    }

    fn schema(&self) -> &'static RowSchema {
        &TXINDEX_ROWS
    }

    fn initial_state(&self) -> u64 {
        u64::from(self.epoch) << SEQ_BITS
    }

    // Core: index/txindex.cpp:58-72
    fn connect(
        &self,
        b: &ConnectedBlock<'_>,
        at_parent: &u64,
    ) -> Result<(BlockRows, u64), SubscriberError> {
        if b.block.height == 0 {
            return Ok((BlockRows::default(), *at_parent));
        }
        let seq = *at_parent;
        let next = seq
            .checked_add(1)
            .filter(|_| seq >> SEQ_BITS == u64::from(self.epoch))
            .ok_or_else(|| {
                data_err(format!(
                    "sequence {seq:#x} is outside epoch {} (2^{SEQ_BITS} appends per process)",
                    self.epoch
                ))
            })?;
        let block =
            b.data.ok_or_else(|| data_err(format!("no block data for {}", b.block.hash)))?;
        let data_pos = self
            .positions
            .data_pos(&b.block)
            .map_err(|err| SubscriberError::Blocks(format!("txindex: {err}")))?;
        let value = codec::encode_seq(seq);
        let txs = block.txs();
        let mut tx_offset = u32::try_from(compact_size_len(txs.len() as u64)).map_err(data_err)?;
        let mut rows = Vec::with_capacity(txs.len());
        for tx in txs {
            let key = TxKey {
                prefix: self.prefix(&tx.txid()),
                pos: TxPos {
                    block: data_pos,
                    tx_offset,
                },
            };
            rows.push(Row::Put(TX, key.encode().map_err(data_err)?.to_vec(), value.clone()));
            tx_offset = tx_offset
                .checked_add(tx.size())
                .ok_or_else(|| data_err(format!("tx offset overflow in {}", b.block.hash)))?;
        }
        Ok((
            BlockRows {
                rows,
                file: None,
            },
            next,
        ))
    }

    // Core: index/base.h:136 (the default CustomRewind: rows stay)
    fn stage_disconnected(
        &self,
        _: &DisconnectedBlock<'_>,
        at_b: &u64,
        _: &DurableBelow<'_, E>,
    ) -> Result<(Vec<Row>, u64), SubscriberError> {
        // The seq never falls: a disconnect keeps the state it had.
        Ok((Vec::new(), *at_b))
    }

    fn archive(&self, _: &[Row]) -> Vec<Row> {
        Vec::new()
    }

    fn with_high_water(&self, at: &u64, latest: &u64) -> u64 {
        (*at).max(*latest)
    }

    fn encode_state(&self, _: &u64) -> Vec<Row> {
        Vec::new()
    }

    fn decode_state(
        &self,
        _: &[StateRead],
        _: &DurableBelow<'_, E>,
    ) -> Result<u64, SubscriberError> {
        Ok(self.initial_state())
    }

    fn height_ranges(&self, _: u32, _: u32) -> Vec<CfRange> {
        Vec::new()
    }

    fn max_entry_bytes(&self) -> u64 {
        0
    }
}

#[cfg(test)]
mod tests {
    use dash_chain::BlockRef;
    use dash_consensus::{Block, BlockId, decode_block};
    use dashd_store::{KvRead, MemoryEngine};

    use super::*;
    use crate::FlatFilePos;
    use crate::txindex::TXINDEX_DB;

    struct Fixed(FlatFilePos);

    impl TxPositions for Fixed {
        fn data_pos(&self, _: &BlockRef) -> Result<FlatFilePos, IndexError> {
            Ok(self.0)
        }
    }

    const POS: FlatFilePos = FlatFilePos {
        file: 3,
        pos: 0x1234,
    };

    fn data(engine: &MemoryEngine) -> TxIndexData<MemoryEngine> {
        TxIndexData::new(engine, Arc::new(Fixed(POS))).unwrap()
    }

    fn stored_epoch(engine: &MemoryEngine) -> Option<u32> {
        engine.get(META, EPOCH).unwrap().map(|v| codec::decode_epoch(&v).unwrap())
    }

    /// The fixture's first block with at least three transactions, connected at height 1.
    fn block_with_txs() -> (Block, u32) {
        let blk: &[u8] = include_bytes!("../../tests/fixtures/dashd-v24/blocks/blk00000.dat");
        let mut at = 0;
        loop {
            let size = u32::from_le_bytes(blk[at + 4..at + 8].try_into().unwrap()) as usize;
            let block = decode_block(&mut &blk[at + 8..at + 8 + size]).unwrap().into_value();
            if block.txs().len() >= 3 {
                return (block, 1);
            }
            at += 8 + size;
        }
    }

    fn connect(
        d: &TxIndexData<MemoryEngine>,
        block: &Block,
        height: u32,
        at: u64,
    ) -> Result<(BlockRows, u64), SubscriberError> {
        let b = ConnectedBlock {
            block: BlockRef {
                id: BlockId(height),
                hash: block.hash(),
                height,
            },
            data: Some(block),
            undo: None,
        };
        d.connect(&b, &at)
    }

    #[test]
    fn each_construction_bumps_the_epoch_durably() {
        let engine = MemoryEngine::new(&TXINDEX_DB);
        assert_eq!(stored_epoch(&engine), None);
        let first = data(&engine);
        assert_eq!((first.epoch(), first.initial_state()), (1, 1 << SEQ_BITS));
        engine.simulate_power_loss(0);
        assert_eq!(stored_epoch(&engine), Some(1), "the epoch write is durable");
        let second = data(&engine);
        assert_eq!((second.epoch(), second.initial_state()), (2, 2 << SEQ_BITS));
        engine.simulate_power_loss(0);
        assert_eq!(stored_epoch(&engine), Some(2));
    }

    #[test]
    fn a_failed_epoch_write_fails_the_open() {
        let engine = MemoryEngine::new(&TXINDEX_DB);
        drop(data(&engine));
        engine.inject_failure(dashd_store::ErrorKind::Io);
        let err = TxIndexData::<MemoryEngine>::new(&engine, Arc::new(Fixed(POS))).err();
        assert!(matches!(err, Some(IndexError::Io(m)) if m.contains("epoch write")));
        assert_eq!(stored_epoch(&engine), Some(1), "the failed open bumped nothing");
    }

    #[test]
    fn the_epoch_bound_is_an_explicit_error() {
        let engine = MemoryEngine::new(&TXINDEX_DB);
        let put = |epoch: u32| {
            let mut b = engine.batch();
            b.put(META, EPOCH, &codec::encode_epoch(epoch));
            engine.write(b, Commit::Durable).unwrap();
        };
        assert_eq!(MAX_EPOCH, 0x00ff_ffff);
        put(MAX_EPOCH - 1);
        let last = data(&engine);
        assert_eq!(last.epoch(), MAX_EPOCH);
        assert_eq!(last.initial_state(), u64::from(MAX_EPOCH) << SEQ_BITS);
        let err = TxIndexData::<MemoryEngine>::new(&engine, Arc::new(Fixed(POS))).err();
        assert!(matches!(err, Some(IndexError::Corrupt(m)) if m.contains("bound")));
        assert_eq!(stored_epoch(&engine), Some(MAX_EPOCH), "a refused open writes nothing");
        put(u32::MAX);
        assert!(TxIndexData::<MemoryEngine>::new(&engine, Arc::new(Fixed(POS))).is_err());
    }

    #[test]
    fn a_bad_epoch_row_is_corrupt() {
        let engine = MemoryEngine::new(&TXINDEX_DB);
        let mut b = engine.batch();
        b.put(META, EPOCH, &[0, 0, 1]);
        engine.write(b, Commit::Durable).unwrap();
        let err = TxIndexData::<MemoryEngine>::new(&engine, Arc::new(Fixed(POS))).err();
        assert!(matches!(err, Some(IndexError::Corrupt(_))), "{err:?}");
    }

    #[test]
    fn genesis_is_skipped_without_consuming_a_sequence() {
        let engine = MemoryEngine::new(&TXINDEX_DB);
        let d = data(&engine);
        let (block, _) = block_with_txs();
        let at = d.initial_state();
        let (rows, next) = connect(&d, &block, 0, at).unwrap();
        assert_eq!(rows, BlockRows::default());
        assert_eq!(next, at);
    }

    #[test]
    fn every_row_is_stamped_and_offsets_follow_core() {
        let engine = MemoryEngine::new(&TXINDEX_DB);
        let d = data(&engine);
        let (block, height) = block_with_txs();
        let at = d.initial_state() + 7;
        let (rows, next) = connect(&d, &block, height, at).unwrap();
        assert_eq!(next, at + 1);
        assert_eq!(rows.file, None);
        assert_eq!(rows.rows.len(), block.txs().len());
        let mut offset = compact_size_len(block.txs().len() as u64) as u32;
        for (row, tx) in rows.rows.iter().zip(block.txs()) {
            let Row::Put(cf, key, value) = row else {
                panic!("a delete: {row:?}");
            };
            assert_eq!(*cf, TX);
            let key = TxKey::decode(key).unwrap();
            assert_eq!(key.prefix, codec::prefix(&tx.txid()));
            assert_eq!(key.pos.block, POS);
            assert_eq!(key.pos.tx_offset, offset);
            assert_eq!(codec::decode_seq(value).unwrap(), at);
            offset += tx.size();
        }
        assert_eq!(80 + offset, block.size(), "the offsets cover the block after its header");
    }

    #[test]
    fn a_shared_prefix_in_one_block_keeps_every_row() {
        let engine = MemoryEngine::new(&TXINDEX_DB);
        let mut d = data(&engine);
        d.prefix_hook = Some(|_| [0x5a; PREFIX_LEN]);
        let (block, height) = block_with_txs();
        let (rows, _) = connect(&d, &block, height, d.initial_state()).unwrap();
        let mut batch = engine.batch();
        for row in &rows.rows {
            let Row::Put(cf, key, value) = row else {
                panic!("a delete: {row:?}");
            };
            batch.put(*cf, key, value);
        }
        engine.write(batch, Commit::Buffered).unwrap();
        let stored: Vec<_> =
            engine.iter_prefix(TX, &[0x5a; PREFIX_LEN]).collect::<Result<_, _>>().unwrap();
        assert_eq!(stored.len(), block.txs().len());
        assert!(stored.len() >= 3);
        let offsets: Vec<_> =
            stored.iter().map(|(k, _)| TxKey::decode(k).unwrap().pos.tx_offset).collect();
        assert!(offsets.windows(2).all(|w| w[0] < w[1]), "{offsets:?}");
    }

    #[test]
    fn appends_are_bounded_per_epoch() {
        let engine = MemoryEngine::new(&TXINDEX_DB);
        let d = data(&engine);
        let (block, height) = block_with_txs();
        let base = d.initial_state();
        let last = base | ((1 << SEQ_BITS) - 1);
        let (_, next) = connect(&d, &block, height, last).unwrap();
        assert_eq!(next, base + (1 << SEQ_BITS));
        for at in [next, base - 1, 0, u64::MAX] {
            let err = connect(&d, &block, height, at).err();
            assert!(matches!(err, Some(SubscriberError::Data(ref m)) if m.contains("outside")));
        }
    }

    #[test]
    fn the_high_water_is_the_max_and_the_schema_is_inexact_only() {
        let engine = MemoryEngine::new(&TXINDEX_DB);
        let d = data(&engine);
        let base = d.initial_state();
        assert_eq!(d.with_high_water(&(base + 2), &(base + 5)), base + 5);
        assert_eq!(d.with_high_water(&(base + 5), &(base + 2)), base + 5);
        assert!(d.encode_state(&base).is_empty());
        assert!(d.height_ranges(0, 10).is_empty());
        assert!(d.archive(&[]).is_empty());
        assert_eq!(d.max_entry_bytes(), 0);
        assert_eq!((d.name(), d.start_height()), ("txindex", 0));
        assert!(d.schema().inexact_only());
        assert_eq!(
            d.needs(),
            Needs {
                block: true,
                undo: false
            }
        );
    }
}
