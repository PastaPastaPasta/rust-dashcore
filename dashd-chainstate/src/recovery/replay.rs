//! Steps 5–10 of §4.3 as a step machine, one store-call group per [`Recovery::step`], so the seam
//! can stop between any two (ST3a §5.6 item 2).
//!
//! The replay is blind (§3.5.1, I2, I4): it reads no coin, and writes Core's disconnect and connect
//! ops in Core's order, so the last write to every key holds its value at `new` whatever subset of
//! the interrupted flush and of earlier recoveries survived.

use std::collections::VecDeque;
use std::io;
use std::sync::Arc;

use dash_chain::{BlockRef, IndexAccess, Status};
use dash_consensus::BlockId;
use dash_consensus::coins::{BlockUndoRef, is_unspendable};
use dash_consensus::pow::check_proof_of_work;
use dash_consensus::primitives::{
    Block, BlockHash, DecodeError, OutPoint, Tx, TxOut, decode_block,
};
use dash_num::CompactTarget;
use dashd_store::flatfile::{BlockFileReader, FlatFileError, FlatFilePos, FlatFs};
use dashd_store::{Commit, KvBatch, KvEngine};

use super::{
    InitError, Opened, RecoveredChainstate, RecoveryConfig, UNABLE_TO_REPLAY, store_failed,
};
use crate::coins::{CoinSrc, CoinsTable, OutPointKey, PackedCoin};
use crate::flush::{CRASH_MESSAGE, FlushObserver, LogCat, ObsMark, SubscriberHub, log, log_batch};
use crate::planted::planted;
use crate::store::{
    CHAINSTATE_SCHEMA, EFFECT_CFS, Failure, GuardedEngine, GuardedFiles, InitReads, Meta,
    nonempty_wiped_cf, store_pos,
};

/// What step 5 found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerState {
    /// No marker: the coins are at `tip`, `None` for an empty chainstate.
    Consistent {
        tip: Option<BlockRef>,
    },
    /// The marker `[new, old]`: the coins are somewhere on `old → fork → new`. `fork` is
    /// `LCA(old, new)`, `None` when `old` is (a first flush).
    Replay {
        new: BlockRef,
        old: Option<BlockRef>,
        fork: Option<BlockRef>,
    },
}

/// What a step did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryStep {
    /// Step 5's checks passed.
    Checked(MarkerState),
    /// 8a: the block's block and undo data were read, and its ops queued.
    Undo(BlockRef),
    /// 8b: the block was read, and its ops queued.
    Forward(BlockRef),
    /// 8c: a sub-batch was written Buffered, under the marker.
    SubBatch(u32),
    /// 9: `sync_wal`.
    PreFinal,
    /// 9: the final Durable batch: the ops left, the tip and the marker's erase.
    Final,
    /// 10: nothing is left.
    Done,
    /// `-dbcrashratio`'s roll came up after a sub-batch: the driver's `_Exit(0)`, the seam's crash
    /// point. Nothing more is written: every later step returns `Crash` again.
    Crash,
}

#[derive(Clone, Copy, Debug)]
enum At {
    Check,
    /// Read the block `leg` names.
    Block,
    SubBatch,
    PreFinal,
    Final,
    End,
}

/// The next block to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Leg {
    /// Roll back `old`'s ancestor at this height.
    Undo(u32),
    /// Roll forward `new`'s ancestor at this height.
    Forward(u32),
    Done,
}

/// How a step stopped the machine.
#[derive(Clone, Copy, Debug)]
enum Stopped {
    Crashed,
    Failed,
}

/// A blind write to the coins.
enum Op {
    Put(OutPointKey, PackedCoin),
    Erase(OutPointKey),
}

/// Recovery after CH1-node's index load: steps 5, 6, 8, 9 and 10.
pub struct Recovery<'a, E: KvEngine, F: FlatFs> {
    engine: &'a GuardedEngine<E>,
    files: Arc<GuardedFiles<F>>,
    reader: BlockFileReader<F>,
    index: &'a dyn IndexAccess,
    hub: &'a SubscriberHub,
    obs: &'a dyn FlushObserver,
    cfg: RecoveryConfig,
    meta: Meta,
    /// Step 1 read a `head_blocks` value of a whole number of hashes other than two.
    malformed_marker: bool,
    /// The engine's init region: it ends when recovery is dropped.
    _init: InitReads<'a>,
    marker: Option<MarkerState>,
    leg: Leg,
    /// Ops read and not yet in `batch`: a block's ops can pass S, and the sub-batch closes there.
    queued: VecDeque<Op>,
    table: CoinsTable,
    batch: E::Batch,
    sub_batches: u32,
    replayed: Vec<BlockHash>,
    /// `None` once done.
    next: Option<At>,
    stopped: Option<Stopped>,
}

impl<'a, E: KvEngine, F: FlatFs> Recovery<'a, E, F> {
    pub(super) fn new(
        opened: Opened<'a, E, F>,
        index: &'a dyn IndexAccess,
        hub: &'a SubscriberHub,
        cfg: RecoveryConfig,
    ) -> Self {
        let Opened {
            engine,
            obs,
            meta,
            malformed_marker,
            files,
            init,
            ..
        } = opened;
        Self {
            engine,
            reader: files.read(|f| f.reader()),
            files,
            index,
            hub,
            obs,
            cfg,
            meta,
            malformed_marker,
            _init: init,
            marker: None,
            leg: Leg::Done,
            queued: VecDeque::new(),
            table: CoinsTable::default(),
            batch: engine.batch(),
            sub_batches: 0,
            replayed: Vec::new(),
            next: Some(At::Check),
            stopped: None,
        }
    }

    /// The blocks replayed coins-only so far, in order (ST3a R6(b)): the forward leg.
    pub fn replayed(&self) -> &[BlockHash] {
        &self.replayed
    }

    /// Runs the next step.
    ///
    /// # Errors
    ///
    /// [`InitError`]: the node stops with its message.
    ///
    /// # Panics
    ///
    /// Called again after it returned an error.
    pub fn step(&mut self) -> Result<RecoveryStep, InitError> {
        match self.stopped {
            Some(Stopped::Crashed) => return Ok(RecoveryStep::Crash),
            Some(Stopped::Failed) => panic!("Recovery::step after an init error"),
            None => {}
        }
        let Some(at) = self.next.take() else {
            return Ok(RecoveryStep::Done);
        };
        let ran = self.run(at);
        match ran {
            Ok(RecoveryStep::Crash) => self.stopped = Some(Stopped::Crashed),
            Err(_) => self.stopped = Some(Stopped::Failed),
            Ok(_) => {}
        }
        ran
    }

    /// After [`RecoveryStep::Done`].
    pub(super) fn into_recovered(self) -> RecoveredChainstate<F> {
        debug_assert!(self.next.is_none() && self.stopped.is_none(), "recovered before it is done");
        RecoveredChainstate {
            tip: self.tip(),
            replayed: self.replayed,
            meta: self.meta,
            files: self.files,
        }
    }

    fn tip(&self) -> Option<BlockRef> {
        match self.marker? {
            MarkerState::Consistent {
                tip,
            } => tip,
            MarkerState::Replay {
                new,
                ..
            } => Some(new),
        }
    }

    fn run(&mut self, at: At) -> Result<RecoveryStep, InitError> {
        match at {
            At::Check => {
                let state = self.check()?;
                self.marker = Some(state);
                let next = match state {
                    MarkerState::Consistent {
                        ..
                    } => At::End,
                    MarkerState::Replay {
                        old,
                        ..
                    } => {
                        self.leg = self.leg_after(old.map_or(0, |o| o.height));
                        self.settle()
                    }
                };
                self.next = Some(next);
                Ok(RecoveryStep::Checked(state))
            }
            At::Block => {
                let step = match self.leg {
                    Leg::Undo(h) => {
                        let b = self.on_branch(self.old(), h)?;
                        self.roll_back(b)?;
                        self.leg = self.leg_after(h - 1);
                        RecoveryStep::Undo(b)
                    }
                    Leg::Forward(h) => {
                        let (new, _) = self.replay_ends();
                        let b = self.on_branch(self.forward_branch(new, h), h)?;
                        self.roll_forward(b)?;
                        self.replayed.push(b.hash);
                        self.leg = forward_from(h + 1, new);
                        RecoveryStep::Forward(b)
                    }
                    Leg::Done => unreachable!("a block step with both legs done"),
                };
                self.next = Some(self.settle());
                Ok(step)
            }
            At::SubBatch => {
                let i = self.sub_batches;
                let mut b = std::mem::replace(&mut self.batch, self.engine.batch());
                if i == 0 && planted!(self.engine.planted(), T7) {
                    // T7: the marker erased in the first batch.
                    Meta::erase_head_blocks(&mut b);
                }
                self.write("partial", b, Commit::Buffered)?;
                self.sub_batches += 1;
                if self.cfg.crash.roll() {
                    log(self.obs, LogCat::Always, CRASH_MESSAGE.into());
                    return Ok(RecoveryStep::Crash);
                }
                self.next = Some(self.settle());
                Ok(RecoveryStep::SubBatch(i))
            }
            At::PreFinal => {
                // E4, E21: the sub-batches are durable before the batch that finalizes them (T27
                // skips it).
                if !planted!(self.engine.planted(), T27) {
                    self.engine.sync_wal().map_err(store_failed(self.obs, "sync_wal"))?;
                }
                self.next = Some(At::Final);
                Ok(RecoveryStep::PreFinal)
            }
            At::Final => {
                // Core's last batch (`txdb.cpp:166-171`): the ops left, the tip, the marker's
                // erase. R5: nothing else is written, and the index is never touched.
                let (new, _) = self.replay_ends();
                let mut b = std::mem::replace(&mut self.batch, self.engine.batch());
                Meta::put_tip(&mut b, &new.hash);
                Meta::erase_head_blocks(&mut b);
                self.write("final", b, Commit::Durable)?;
                self.meta.tip = Some(new.hash);
                self.meta.head_blocks = None;
                self.next = Some(At::End);
                Ok(RecoveryStep::Final)
            }
            At::End => {
                if planted!(self.engine.planted(), T31)
                    && let Some(tip) = self.tip()
                {
                    // T31: an index record erased; recovery can name only the tip's.
                    let mut b = self.engine.batch();
                    let cf =
                        CHAINSTATE_SCHEMA.cf("block_index").expect("the chainstate schema has it");
                    b.delete(cf, tip.hash.as_bytes());
                    self.write("final", b, Commit::Durable)?;
                }
                self.obs.mark(ObsMark::RecoveryEnd);
                // Step 10: `T_ack := G`. No tip step has a `seq` yet.
                if let Some(tip) = self.tip() {
                    self.hub.recovered(tip);
                }
                Ok(RecoveryStep::Done)
            }
        }
    }

    // Core: validation.cpp:5228-5262 ReplayBlocks (the checks); node/chainstate.cpp:270-276
    /// Step 5. The marker's checks log Core's line for each failure, in Core's order (§3.5.1).
    fn check(&self) -> Result<MarkerState, InitError> {
        if self.malformed_marker {
            return Err(self.unable(&[], "ReplayBlocks(): unknown inconsistent state"));
        }
        let Some((new, old)) = self.meta.head_blocks else {
            return self.consistent();
        };
        log(self.obs, LogCat::Always, "Replaying blocks".into());
        let new = self.block_ref(&new).ok_or_else(|| {
            self.unable(&[], "ReplayBlocks(): reorganization to unknown block requested")
        })?;
        let old = old
            .map(|old| {
                self.block_ref(&old).ok_or_else(|| {
                    self.unable(&[], "ReplayBlocks(): reorganization from unknown block requested")
                })
            })
            .transpose()?;
        // DEC-165(3) audit: no Core text at this point. Nearest Core line: validation.cpp:5256,
        // which asserts the fork, as `LastCommonAncestor` always meets at genesis. Corrupted meta
        // cannot reach it either: both tips are in the index, whose entries all lead to genesis,
        // so only links that end short of it (SPV's cold tier, never a node's index) do.
        let fork = old
            .map(|o| {
                self.index.lca(o.id, new.id).and_then(|id| self.entry(id)).ok_or_else(|| {
                    self.unable(&[], "ReplayBlocks(): no common ancestor of the old and new tips")
                })
            })
            .transpose()?;
        // B1 wrote the evo state at `new` (Core's at `old`).
        if self.meta.evo_tip != Some(new.hash) {
            return Err(self.unable(
                &[],
                "ReplayBlocks(DASH): Found EvoDB inconsistency, you must reindex to continue",
            ));
        }
        // I1: B1 wrote the marker only once `new`'s data was durable. D-b1-g; DEC-165(3) audit:
        // Core's text, at another point. Core has no such check (validation.cpp:5222-5262): its
        // forward leg's read of `new` fails (validation.cpp:5188-5190) with the lines logged
        // here, but only after the undo leg, so a fault there (or a block below `new` without
        // data) is what Core reports first. Here it fails before any write. A marker without a
        // forward leg (a rollback only) needs no data at `new`, as Core never reads it.
        let forward = new.height > fork.map_or(0, |f| f.height);
        if forward && !self.index.status(new.id).is_some_and(|s| s.has(Status::HAVE_DATA)) {
            return Err(self.unable(&[NO_DATA], &rollforward_failed(new)));
        }
        Ok(MarkerState::Replay {
            new,
            old,
            fork,
        })
    }

    /// Step 5 without a marker: without a tip the chainstate is empty but for effects (DEC-202);
    /// with one, the evo state is at it and it is in the index (Core's `LoadChainTip`).
    fn consistent(&self) -> Result<MarkerState, InitError> {
        let Some(hash) = self.meta.tip else {
            // DEC-165(3) audit: no Core text at this point. Nearest Core line:
            // node/chainstate.cpp:272, where a null best block skips `LoadChainTip`; Core does not
            // check an empty chainstate.
            if let Some(evo) = self.meta.evo_tip {
                return Err(InitError::Corruption(format!(
                    "no tip, but the evo state is at {evo}"
                )));
            }
            // DEC-202: no durable tip is a logically empty store. A kill before the first flush
            // can leave effect records (A1 §2.10.1), which stay as they do above a tip: the
            // connects from height 1 rewrite them. The coins and the evo state must be empty.
            let skip: &[&str] = if planted!(self.engine.planted(), NoTipEmpty) {
                &[]
            } else {
                &EFFECT_CFS
            };
            if let Some(cf) =
                nonempty_wiped_cf(self.engine, skip).map_err(store_failed(self.obs, "read"))?
            {
                return Err(InitError::Corruption(format!("no tip, but `{cf}` holds records")));
            }
            return Ok(MarkerState::Consistent {
                tip: None,
            });
        };
        // Core: node/chainstate.cpp:274, `LoadChainTip` fails on an unknown tip before anything
        // reads the evo state.
        let tip = self.block_ref(&hash).ok_or(InitError::UnknownTip(hash))?;
        if self.meta.evo_tip != Some(hash) {
            return Err(InitError::EvoInconsistent);
        }
        Ok(MarkerState::Consistent {
            tip: Some(tip),
        })
    }

    /// 8a: reads `b`'s block and undo data, and queues its disconnect.
    fn roll_back(&mut self, b: BlockRef) -> Result<(), InitError> {
        let at = format!("at {}, hash={}", b.height, b.hash);
        let block =
            self.read_block(b, &format!("ReplayBlocks(): ReadBlockFromDisk() failed {at}"))?;
        log(self.obs, LogCat::Always, format!("Rolling back {} ({})", b.hash, b.height));
        let failed = format!("ReplayBlocks(): DisconnectBlock failed {at}");
        const READ: &str = "DisconnectBlock(): failure reading undo data";
        let Some(pos) = self.index.undo_pos(b.id).and_then(store_pos) else {
            return Err(self.unable(&["UndoReadFromDisk: no undo data available", READ], &failed));
        };
        // Core: node/blockstorage.cpp:560. The checksum binds the record to the index's parent
        // (`pindex->pprev`), not to the one the block file's header names. Genesis is never
        // disconnected, so `b` has a parent.
        let prev = self.on_branch(b, b.height - 1)?;
        let mut bytes = Vec::new();
        // DEC-165(3) audit: Core decodes the record before it compares the checksum
        // (node/blockstorage.cpp:560-571); the store compares first, so a record that fails both
        // is a checksum mismatch here and a deserialize error in Core. Both fail the disconnect.
        self.reader
            .read_undo(pos, prev.hash.as_bytes(), &mut bytes)
            .map_err(|e| self.read_failed(&e, FileRead::Undo, &[READ], &failed))?;
        let undo = BlockUndoRef::decode(&mut &bytes[..]).map_err(|e| {
            // DEC-165(3) audit: Core's `%s` is its deserializer's exception, read from the file
            // rather than from the record (node/blockstorage.cpp:564); the decoder's reason stands
            // in for it.
            let line = format!("UndoReadFromDisk: Deserialize or I/O error - {e}");
            self.unable(&[&line, READ], &failed)
        })?;
        disconnect_ops(&block, &undo, &mut self.queued)
            .map_err(|line| self.unable(&[&line], &failed))
    }

    /// 8b: reads `b`'s block, and queues its connect.
    fn roll_forward(&mut self, b: BlockRef) -> Result<(), InitError> {
        log(self.obs, LogCat::Always, format!("Rolling forward {} ({})", b.hash, b.height));
        let failed = rollforward_failed(b);
        let block = self.read_block(b, &failed)?;
        connect_ops(&block, b.height, &mut self.queued)
            .map_err(|line| self.unable(&[&line], &failed))
    }

    // Core: node/blockstorage.cpp:834-848 ReadBlockFromDisk(CBlock&, const CBlockIndex*)
    /// `b`'s block: [`read_block_at`](Self::read_block_at) its position, and it must hash to
    /// `b`'s hash. Every block recovery reads, on either leg, is read here.
    fn read_block(&self, b: BlockRef, failed: &str) -> Result<Block, InitError> {
        let Some(pos) = self.index.have_data_pos(b.id).and_then(store_pos) else {
            return Err(self.unable(&[NO_DATA], failed));
        };
        let (block, hash) = self.read_block_at(pos, failed)?;
        if hash != b.hash {
            // DEC-165(3) audit: Core's text names the entry by `CBlockIndex::ToString`
            // (chain.cpp:16-20), whose `pprev=%p` is an address no port reproduces; this names
            // the entry by its hash.
            let line = format!(
                "ReadBlockFromDisk(CBlock&, CBlockIndex*): GetHash() doesn't match index for {} at {pos}",
                b.hash
            );
            return Err(self.unable(&[&line], failed));
        }
        Ok(block)
    }

    // Core: node/blockstorage.cpp:805-832 ReadBlockFromDisk(CBlock&, const FlatFilePos&)
    /// The block at `pos` and its hash. Its header must have the proof of work its own `nBits`
    /// claims: the index load checked the hash against the index record's `nBits`
    /// (`txdb.cpp:328-331`), which the block file's need not equal.
    fn read_block_at(
        &self,
        pos: FlatFilePos,
        failed: &str,
    ) -> Result<(Block, BlockHash), InitError> {
        let mut bytes = Vec::new();
        self.reader
            .read_block(pos, &mut bytes)
            .map_err(|e| self.read_failed(&e, FileRead::Block(pos), &[], failed))?;
        let block = decode_block(&mut &bytes[..])
            .map_err(|e| self.unable(&[&FileRead::decode_line(pos, &e)], failed))?
            .into_value();
        let hash = block.hash();
        let bits = CompactTarget::new(block.header().bits());
        if !check_proof_of_work(hash.as_bytes(), bits, &self.cfg.pow) {
            let line = format!("ReadBlockFromDisk: Errors in block header at {pos}");
            return Err(self.unable(&[&line], failed));
        }
        Ok((block, hash))
    }

    /// Moves queued ops into the batch, then names the next step: a batch past S is written (it
    /// holds an op at least, so a sub-batch always makes progress); then the next block; once both
    /// legs are read, the final batch takes what is left, as Core's and W's last batch does.
    fn settle(&mut self) -> At {
        loop {
            if !self.batch.is_empty() && self.batch.size_bytes() as u64 > self.cfg.sub_batch {
                return At::SubBatch;
            }
            let op = self.queued.pop_front();
            if planted!(self.engine.planted(), X8)
                && let Some(Op::Put(k, _) | Op::Erase(k)) = &op
            {
                // X8: the replay reads the key it writes.
                let _ = self.table.get(self.engine, k);
            }
            match op {
                Some(Op::Put(k, coin)) => self.table.put(&mut self.batch, &k, coin.as_bytes()),
                Some(Op::Erase(k)) => self.table.erase(&mut self.batch, &k),
                None if self.leg == Leg::Done => return At::PreFinal,
                None => return At::Block,
            }
        }
    }

    /// Writes `b` with Core's log line (`kind` "partial" or "final").
    fn write(&self, kind: &str, b: E::Batch, commit: Commit) -> Result<(), InitError> {
        log_batch(self.obs, kind, &b);
        self.engine.write(b, commit).map_err(store_failed(self.obs, "write"))
    }

    /// The next block once `old`'s branch is undone down to `height`: its ancestor there while it
    /// is above the fork; else the forward leg's first.
    fn leg_after(&self, height: u32) -> Leg {
        let (new, fork_height) = self.replay_ends();
        if height > fork_height {
            Leg::Undo(height)
        } else {
            // Core: validation.cpp:5288-5289 (the forward leg starts at height 1 after a first
            // flush; genesis is never disconnected)
            forward_from(fork_height + 1, new)
        }
    }

    /// The marker's `new`, and the fork's height (0 without one).
    fn replay_ends(&self) -> (BlockRef, u32) {
        match self.marker {
            Some(MarkerState::Replay {
                new,
                fork,
                ..
            }) => (new, fork.map_or(0, |f| f.height)),
            _ => unreachable!("a replay step without a marker"),
        }
    }

    fn old(&self) -> BlockRef {
        match self.marker {
            Some(MarkerState::Replay {
                old: Some(old),
                ..
            }) => old,
            _ => unreachable!("an undo step without `old`"),
        }
    }

    /// The branch the forward leg reads at `height`: `new`'s; T8 reads `old`'s where it reaches.
    fn forward_branch(&self, new: BlockRef, height: u32) -> BlockRef {
        match self.marker {
            Some(MarkerState::Replay {
                old: Some(old),
                ..
            }) if planted!(self.engine.planted(), T8) && old.height >= height => old,
            _ => new,
        }
    }

    /// `tip`'s ancestor at `height`.
    fn on_branch(&self, tip: BlockRef, height: u32) -> Result<BlockRef, InitError> {
        self.index.ancestor(tip.id, height).and_then(|id| self.entry(id)).ok_or_else(|| {
            // DEC-165(3) audit: no Core text at this point. Nearest Core line: validation.cpp:5291,
            // which asserts the ancestor. Unreachable in a node, whose index entries all lead to
            // genesis.
            let line = format!("ReplayBlocks(): no ancestor of {} at {height}", tip.hash);
            self.unable(&[], &line)
        })
    }

    fn block_ref(&self, hash: &BlockHash) -> Option<BlockRef> {
        self.entry(self.index.lookup(hash)?)
    }

    fn entry(&self, id: BlockId) -> Option<BlockRef> {
        Some(BlockRef {
            id,
            hash: self.index.hash(id)?,
            height: self.index.height(id)?,
        })
    }

    /// Core's `ReplayBlocks` failure: logs the causes, then `detail`.
    fn unable(&self, causes: &[&str], detail: &str) -> InitError {
        for line in causes.iter().chain([&detail]) {
            log(self.obs, LogCat::Error, (*line).into());
        }
        InitError::UnableToReplay {
            detail: detail.into(),
        }
    }

    /// A block-file read that failed: logs Core's lines for `read` failing with `e`, then `more`
    /// and `detail`. Data missing or corrupt cannot be replayed; an I/O error makes durable bytes
    /// unavailable, never absent, so it also stops the node as a failed store call (DEC-148,
    /// DEC-162 (4)).
    fn read_failed(
        &self,
        e: &FlatFileError,
        read: FileRead,
        more: &[&str],
        detail: &str,
    ) -> InitError {
        if let Some(path) = e.open_failed() {
            // Core: flatfile.cpp:45 FlatFileSeq::Open
            log(self.obs, LogCat::Always, format!("Unable to open file {}", path.display()));
        }
        let line = read.line(e);
        let causes: Vec<&str> = [line.as_str()].into_iter().chain(more.iter().copied()).collect();
        let err = self.unable(&causes, detail);
        if let FlatFileError::Io {
            source,
            ..
        } = e
            && !matches!(source.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::NotFound)
            && self.engine.latch().trip(Failure {
                kind: e.kind(),
                site: "block files read",
                message: e.to_string(),
            })
            && !planted!(self.engine.planted(), Unmarked)
        {
            self.obs.mark(ObsMark::StoreFailed {
                op: "read",
            });
            self.obs.mark(ObsMark::Abort {
                message: UNABLE_TO_REPLAY.into(),
            });
        }
        err
    }
}

// Core: validation.cpp:2282-2370 DisconnectBlock (the coins), :2252-2280 ApplyTxInUndo
/// 8a's ops for `block`, per transaction in reverse: erase each spendable output, then put each
/// restored input coin, in reverse input order. `Err` is Core's line for undo data that does not
/// fit the block.
fn disconnect_ops(
    block: &Block,
    undo: &BlockUndoRef<'_>,
    ops: &mut VecDeque<Op>,
) -> Result<(), String> {
    let txs = block.txs();
    if undo.txs.len() + 1 != txs.len() {
        return Err("DisconnectBlock(): block and undo data inconsistent".into());
    }
    for (i, tx) in txs.iter().enumerate().rev() {
        ops.extend(spendable(tx).map(|(k, _)| Op::Erase(k)));
        if i == 0 {
            continue;
        }
        let coins = &undo.txs[i - 1].coins;
        if coins.len() != tx.inputs().len() {
            return Err("DisconnectBlock(): transaction and undo data inconsistent".into());
        }
        for (input, coin) in tx.inputs().iter().zip(coins).rev() {
            // §3.5.2: legacy undo is normalised at import, so recovery never resolves one.
            // DEC-165(3) audit: no Core text at this point. Nearest Core line:
            // validation.cpp:2258, where Core resolves the legacy undo from the transaction's
            // other outputs.
            if coin.height() == 0 {
                return Err(format!(
                    "DisconnectBlock(): undo data for {} has no height",
                    tx.txid()
                ));
            }
            // Stored coin bytes always pack; an unspendable coin packs to none.
            if let Some(coin) = CoinSrc::Bytes(*coin).pack().expect("stored coin bytes pack") {
                ops.push_back(Op::Put(OutPointKey::new(&input.prevout), coin));
            }
        }
    }
    Ok(())
}

// Core: validation.cpp:5180-5219 RollforwardBlock (the coins)
/// 8b's ops for `block` at `height`, per transaction in order: erase every input unless it is the
/// coinbase, then put every spendable output. `Err` is Core's assertion, as a line.
fn connect_ops(block: &Block, height: u32, ops: &mut VecDeque<Op>) -> Result<(), String> {
    for tx in block.txs() {
        let coinbase = tx.is_coinbase();
        if !coinbase {
            ops.extend(tx.inputs().iter().map(|input| Op::Erase(OutPointKey::new(&input.prevout))));
        }
        for (k, out) in spendable(tx) {
            let src = CoinSrc::Output {
                out,
                height,
                coinbase,
            };
            // Core asserts it (`coins.cpp:71`): a null output is no block's. DEC-165(3) audit: no
            // Core text at this point; the nearest Core line is that assertion.
            if let Some(coin) = src.pack().map_err(|e| format!("RollforwardBlock(): {e}"))? {
                ops.push_back(Op::Put(k, coin));
            }
        }
    }
    Ok(())
}

/// `tx`'s spendable outputs, keyed.
fn spendable(tx: &Tx) -> impl Iterator<Item = (OutPointKey, &TxOut)> {
    let txid = tx.txid();
    let spendable = |(_, out): &(usize, &TxOut)| !is_unspendable(out.script_pubkey.as_bytes());
    tx.outputs().iter().enumerate().filter(spendable).map(move |(o, out)| {
        let k = OutPointKey::new(&OutPoint {
            hash: txid,
            index: o as u32,
        });
        (k, out)
    })
}

/// The forward leg from `height`, or none past `new`.
fn forward_from(height: u32, new: BlockRef) -> Leg {
    if height <= new.height {
        Leg::Forward(height)
    } else {
        Leg::Done
    }
}

// Core: chain.h:216-225 (a block without data has the null position), node/blockstorage.cpp:811-812
/// `ReadBlockFromDisk`'s line for a block without data: no file opens the null position.
const NO_DATA: &str = "ReadBlockFromDisk: OpenBlockFile failed for FlatFilePos(nFile=-1, nPos=0)";

/// Which of Core's block-file reads failed, for its line (V's block reads too, b4 M2).
#[derive(Clone, Copy)]
pub(crate) enum FileRead {
    /// `ReadBlockFromDisk` at a position.
    Block(FlatFilePos),
    /// `UndoReadFromDisk`.
    Undo,
}

impl FileRead {
    // Core: node/blockstorage.cpp:818-822
    /// Core's line for a block at `pos` its deserializer throws for: Core's exception for a
    /// count it refuses as the decoder did ([`DecodeError::read_error_kind`]).
    ///
    /// DEC-165(3) audit: for anything else Core's `%s` is its deserializer's exception, read from
    /// the file rather than from the record; the decoder's reason stands in for it.
    pub(crate) fn decode_line(pos: FlatFilePos, e: &DecodeError) -> String {
        let what = match e.read_error_kind() {
            Some(kind) => format!("{kind}: iostream error"),
            None => e.to_string(),
        };
        format!("ReadBlockFromDisk: Deserialize or I/O error - {what} at {pos}")
    }

    // Core: node/blockstorage.cpp:809-822 (blocks), :549-571 (undo); streams.cpp:27
    /// Core's line for this read failing with `e`. A file that did not open, whatever the error
    /// ([`FlatFileError::open_failed`]), is Core's failed `OpenBlockFile` or `OpenUndoFile`. Core
    /// reads an open file through `AutoFile`, whose exception text (with libstdc++'s
    /// `": iostream error"`) is reproduced for the end of the file and for a failed read.
    pub(crate) fn line(self, e: &FlatFileError) -> String {
        let (func, at) = match self {
            Self::Block(pos) => ("ReadBlockFromDisk", format!(" at {pos}")),
            Self::Undo => ("UndoReadFromDisk", String::new()),
        };
        if e.open_failed().is_some() {
            return match self {
                Self::Block(pos) => format!("{func}: OpenBlockFile failed for {pos}"),
                Self::Undo => format!("{func}: OpenUndoFile failed"),
            };
        }
        let what = match e {
            FlatFileError::Corrupt {
                detail,
                ..
            } if detail == STORE_CHECKSUM => return format!("{func}: Checksum mismatch"),
            FlatFileError::Corrupt {
                detail,
                ..
            } if detail.ends_with(STORE_PAST_END) => "AutoFile::read: end of file: iostream error",
            FlatFileError::Io {
                source,
                ..
            } if source.kind() == io::ErrorKind::UnexpectedEof => {
                "AutoFile::read: end of file: iostream error"
            }
            FlatFileError::Io {
                source,
                ..
            } if source.kind() != io::ErrorKind::OutOfMemory => {
                "AutoFile::read: fread failed: iostream error"
            }
            // DEC-165(3) audit: no Core text at this point. Nearest Core line:
            // node/blockstorage.cpp:820. Core reads no record size field, so a size past the
            // limit, a position inside the file header, or a size too large to allocate fails
            // there differently, if at all (BL-33, deviation 6); the store's reason stands in for
            // the exception.
            FlatFileError::Corrupt {
                detail,
                ..
            } => detail,
            _ => &e.to_string(),
        };
        format!("{func}: Deserialize or I/O error - {what}{at}")
    }
}

/// The store's [`FlatFileError::Corrupt`] details that Core's reads also detect (dashd-store
/// `flatfile/blocks.rs`): the undo checksum, and a record that runs past the end of its file.
const STORE_CHECKSUM: &str = "Checksum mismatch";
const STORE_PAST_END: &str = "runs past the end of the file";

// Core: validation.cpp:5189
fn rollforward_failed(b: BlockRef) -> String {
    format!("RollforwardBlock(): ReadBlockFromDisk failed at {}, hash={}", b.height, b.hash)
}
