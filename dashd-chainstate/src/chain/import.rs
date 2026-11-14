//! V's half of Core's block import (`ThreadImport`, `LoadExternalBlockFile`): each block's
//! `AcceptBlock` at its scanned position, the genesis retry after a reindex's scan, and
//! `NotifyHeaderTip` where the import calls it. The importing thread reads the files and keeps
//! the out-of-order map.

use std::sync::Arc;

use dash_chain::{AcceptOne, FlatFilePos, Status};
use dash_consensus::Block;
use dash_primitives::BlockHash;

use super::actor::V;
use super::command::{BlockState, ImportKind, Imported};
use super::index::entry;
use super::load::genesis_block;
use super::reply::ReplySender;
use super::seams::Fatal;

impl V {
    /// One block of `LoadExternalBlockFile` (`validation.cpp:5505-5536`, `:5593-5599` at
    /// v24.0.0). A record that is not genesis and whose parent is unknown, or that is stored
    /// already, is answered without an accept; otherwise, and for a child, `AcceptBlock` with
    /// `fRequested`, at `dbp`, running `CheckBlock` itself. Its scope publishes before the reply,
    /// so the importing thread's reader sees what the accept indexed. `Err` is Core's
    /// `state.IsError()`: `AcceptBlock`'s error state, or V's own `AbortNode`, answered as an
    /// `ActivateBestChain`'s is (D240) before V stops. One of Core's asserts drops the reply.
    pub(crate) fn import_block(
        &mut self,
        block: &Arc<Block>,
        hash: BlockHash,
        dbp: Option<FlatFilePos>,
        kind: ImportKind,
        reply: ReplySender<Result<Imported, String>>,
    ) -> Result<(), Fatal> {
        debug_assert_eq!(hash, block.hash(), "the record's hash is the block's X11");
        if kind == ImportKind::Record
            && let Some(known) = self.classify_record(block, &hash)
        {
            reply.send(Ok(known));
            return Ok(());
        }
        let result = match self.accept_block(block, hash, true, dbp, false) {
            Ok(result) => result,
            Err(f) => {
                if f.is_abort_node() {
                    reply.send(Err(f.0.clone()));
                }
                return Err(f);
            }
        };
        // An `AbortNode` V carried past (the block's write or its flush).
        if let Some(f) = self.interrupted.take() {
            reply.send(Err(f.0.clone()));
            return Err(f);
        }
        self.scope_end();
        reply.send(match result.state {
            BlockState::Error(e) => Err(e),
            _ if result.accepted => Ok(Imported::Accepted),
            _ => Ok(Imported::Rejected),
        });
        Ok(())
    }

    /// `LoadExternalBlockFile`'s checks under `cs_main` before a record is read
    /// (`validation.cpp:5509-5520` at v24.0.0): `None` to process it.
    fn classify_record(&self, block: &Block, hash: &BlockHash) -> Option<Imported> {
        let genesis = hash.as_bytes() == self.params.consensus.hash_genesis_block.as_bytes();
        if !genesis && self.reader.lookup(&block.header().prev_hash()).is_none() {
            return Some(Imported::UnknownParent);
        }
        let e = entry(&self.reader, self.reader.lookup(hash)?);
        e.status.has(Status::HAVE_DATA).then_some(Imported::Stored {
            height: e.height,
        })
    }

    /// `ThreadImport`'s `LoadGenesisBlock` after a reindex's scan (`node/blockstorage.cpp:927`,
    /// `validation.cpp:5410-5444` at v24.0.0): nothing when genesis is indexed; otherwise
    /// `AddGenesisBlock` (its write, the index entry and `ReceivedBlockTransactions`), then a
    /// devnet's genesis block. Of the `AcceptBlockHeader` tail Core skips here, only
    /// `Chainlocks::AcceptedBlockHeader` runs, which does nothing unless the best ChainLock names
    /// genesis. `false` where Core's is. An
    /// `AbortNode` answers `false`, then stops V.
    pub(crate) fn load_genesis_retry(&mut self, reply: ReplySender<bool>) -> Result<(), Fatal> {
        let block = genesis_block(&self.params);
        if self.reader.lookup(&block.hash()).is_some() {
            reply.send(true);
            return Ok(());
        }
        let pos = match self.files.save_block(&block, 0, None) {
            Ok(Some(pos)) => pos,
            Ok(None) => {
                if let Some(f) = self.files.refused_by_assert() {
                    return Err(f);
                }
                tracing::error!("AddGenesisBlock: writing genesis block to disk failed (Valid)");
                reply.send(false);
                return Ok(());
            }
            Err(msg) => {
                self.carry_past(Err(Fatal::new(format!("System error: {msg}"))))?;
                let f = self.interrupted.take().expect("an AbortNode carried past");
                reply.send(false);
                return Err(f);
            }
        };
        let accepted = self.chain.accept_header(block.header())?;
        self.apply_cl_late_header(accepted.floor_moved);
        let AcceptOne::Accepted(id) = accepted.one else {
            // Only an index without genesis that is not empty, which no load leaves.
            reply.send(false);
            return Ok(());
        };
        let n_tx = u32::try_from(block.txs().len()).expect("genesis has one transaction");
        self.idx.received_block_transactions(&mut self.chain, &self.reader, id, n_tx, pos)?;
        if self.params.devnet_genesis.is_some() {
            self.idx.set_devnet_pending();
            return match self.load_devnet_genesis() {
                Ok(done) => {
                    reply.send(done.is_ok());
                    Ok(())
                }
                Err(f) => {
                    reply.send(false);
                    Err(f)
                }
            };
        }
        self.scope_end();
        reply.send(true);
        Ok(())
    }

    /// `NotifyHeaderTip` as `LoadExternalBlockFile` calls it (`validation.cpp:5576`, `:5603` at
    /// v24.0.0): nothing on an index with no entry, whose best header is Core's null.
    pub(crate) fn notify_header_tip_import(&mut self) -> bool {
        self.chain.ids().next().is_some() && self.notify_header_tip()
    }
}
