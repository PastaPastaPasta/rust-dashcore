//! The importing thread's half of Core's block import: `LoadExternalBlockFile`'s checks before a
//! record is read, its out-of-order map and the processing of a record's earlier children, over
//! [`load_external_block_file`]'s scan.
//! V accepts each block ([`ChainHandle::import_block`]) and publishes before it answers, so the
//! next record's checks on its reader see what the last one indexed.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::sync::Arc;
use std::time::Instant;

use dash_chain::{FlatFilePos, Status};
use dash_consensus::primitives::decode_block;
use dash_coreimport::Hash256;
use dash_coreimport::blockfiles::external::{
    ExternalBlockSink, Flow, HeaderStatus, StreamError, load_external_block_file,
};
use dash_params::ChainSetup;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{BlockAtPos, ChainHandle, ImportKind, Imported, ShutdownFlag};

/// `ThreadImport`'s `blocks_with_unknown_parent` (`node/blockstorage.cpp:905` at v24.0.0): the
/// positions of `-reindex` records whose parent was not indexed when they were scanned, by
/// parent, in scan order. It lives across the reindex's files.
#[derive(Debug, Default)]
pub struct UnknownParents {
    by_prev: HashMap<BlockHash, VecDeque<FlatFilePos>>,
}

impl UnknownParents {
    /// The records waiting for a parent.
    pub fn len(&self) -> usize {
        self.by_prev.values().map(VecDeque::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_prev.is_empty()
    }
}

/// Why a file's scan stopped before its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanEnd {
    /// The node is shutting down (`v:5466` at v24.0.0): Core returns without the "Loaded" line.
    Shutdown,
    /// A record's `AcceptBlock` left an error state, or genesis did not activate: Core's
    /// `break` (`v:5531-5533`, `:5549-5551`).
    Stopped,
    /// V stopped.
    Disconnected,
}

/// `-reindex`'s side of a scan: the blk file's number, so each block's position is known, the
/// map and the reads of its children.
struct Reindex<'a> {
    file: i32,
    unknown: &'a mut UnknownParents,
    read: &'a dyn BlockAtPos,
}

/// One external file's [`ExternalBlockSink`]: Core's `LoadExternalBlockFile` with `dbp` and
/// `blocks_with_unknown_parent` under `-reindex`, or neither for `-loadblock`. Core's
/// prune-mode activation for `-loadblock` (`v:5554-5574` at v24.0.0) is CH8's.
pub struct ExternalImport<'a> {
    v: &'a ChainHandle,
    genesis: Hash256,
    reindex: Option<Reindex<'a>>,
    shutdown: &'a dyn ShutdownFlag,
    loaded: u32,
    end: Option<ScanEnd>,
    /// The scanned block was not handed to V: nothing follows it (Core's `continue`).
    skip_after: bool,
}

impl<'a> ExternalImport<'a> {
    /// A `-reindex` scan of blk file `file`, its children read by `read`.
    pub fn reindex(
        v: &'a ChainHandle,
        params: &ChainSetup,
        file: i32,
        unknown: &'a mut UnknownParents,
        read: &'a dyn BlockAtPos,
        shutdown: &'a dyn ShutdownFlag,
    ) -> Self {
        let reindex = Reindex {
            file,
            unknown,
            read,
        };
        Self::new(v, params, Some(reindex), shutdown)
    }

    /// A `-loadblock` file's scan: no positions and no map.
    pub fn load_block(
        v: &'a ChainHandle,
        params: &ChainSetup,
        shutdown: &'a dyn ShutdownFlag,
    ) -> Self {
        Self::new(v, params, None, shutdown)
    }

    fn new(
        v: &'a ChainHandle,
        params: &ChainSetup,
        reindex: Option<Reindex<'a>>,
        shutdown: &'a dyn ShutdownFlag,
    ) -> Self {
        Self {
            v,
            genesis: *params.consensus.hash_genesis_block.as_bytes(),
            reindex,
            shutdown,
            loaded: 0,
            end: None,
            skip_after: false,
        }
    }

    /// Core's `nLoaded`: the blocks `AcceptBlock` accepted.
    pub fn loaded(&self) -> u32 {
        self.loaded
    }

    /// Why the scan stopped early, if it did.
    pub fn end(&self) -> Option<ScanEnd> {
        self.end
    }

    fn at(file: i32, pos: u64) -> FlatFilePos {
        FlatFilePos {
            file,
            // Core's `dbp->nPos = nBlockPos` (`v:5494`) keeps the low 32 bits.
            pos: pos as u32,
        }
    }

    /// A record whose parent is not indexed (`v:5508-5515` at v24.0.0): mapped by its parent
    /// under `-reindex`, and not processed.
    fn out_of_order(&mut self, hash: BlockHash, prev: BlockHash, pos: u64) {
        tracing::debug!(
            target: "reindex",
            "LoadExternalBlockFile: Out of order block {hash}, parent {prev} not known"
        );
        if let Some(r) = self.reindex.as_mut() {
            r.unknown.by_prev.entry(prev).or_default().push_back(Self::at(r.file, pos));
        }
    }

    /// A record already stored (`v:5534-5535` at v24.0.0).
    fn already_had(&self, hash: BlockHash, height: u32) {
        if *hash.as_bytes() != self.genesis && height.is_multiple_of(1000) {
            tracing::debug!(
                target: "reindex",
                "Block Import: already had block {hash} at height {height}"
            );
        }
    }

    /// `NotifyHeaderTip`; `false` once V has stopped.
    fn notify_header_tip(&mut self) -> bool {
        let served = self.v.notify_header_tip().recv().is_ok();
        if !served {
            self.end = Some(ScanEnd::Disconnected);
        }
        served
    }

    /// Core's processing of the earlier children of `hash` (`v:5580-5606` at v24.0.0), breadth
    /// first: each read at its position, accepted at it, and followed by `NotifyHeaderTip`
    /// whether it was read or not.
    fn children_of(&mut self, hash: BlockHash) -> Flow {
        let Some(read) = self.reindex.as_ref().map(|r| r.read) else {
            return Flow::Continue;
        };
        let mut queue = VecDeque::from([hash]);
        while let Some(head) = queue.pop_front() {
            let Some(children) =
                self.reindex.as_mut().and_then(|r| r.unknown.by_prev.remove(&head))
            else {
                continue;
            };
            for pos in children {
                match read.read_block_at(pos) {
                    Ok(block) => {
                        let child = block.hash();
                        tracing::debug!(
                            target: "reindex",
                            "LoadExternalBlockFile: Processing out of order child {child} of {head}"
                        );
                        match self.v.import_block(block, Some(pos), ImportKind::Child).recv() {
                            Ok(Ok(Imported::Accepted)) => {
                                self.loaded += 1;
                                queue.push_back(child);
                            }
                            // Core's `dummy` state: a child's failure does not stop the scan.
                            Ok(_) => {}
                            Err(_) => {
                                self.end = Some(ScanEnd::Disconnected);
                                return Flow::Stop;
                            }
                        }
                    }
                    Err(line) => tracing::error!("{line}"),
                }
                if !self.notify_header_tip() {
                    return Flow::Stop;
                }
            }
        }
        Flow::Continue
    }
}

impl ExternalBlockSink for ExternalImport<'_> {
    fn interrupted(&mut self) -> bool {
        let stop = self.shutdown.shutdown_requested();
        if stop {
            self.end = Some(ScanEnd::Shutdown);
        }
        stop
    }

    // Core: validation.cpp:5506-5536 at v24.0.0
    fn header(&mut self, header: &[u8; 80], hash: &Hash256, pos: u64) -> HeaderStatus {
        let genesis = *hash == self.genesis;
        let hash = BlockHash::from(*hash);
        let prev = BlockHash::from(<[u8; 32]>::try_from(&header[4..36]).expect("32 bytes"));
        let reader = self.v.reader();
        if !genesis && reader.lookup(&prev).is_none() {
            self.out_of_order(hash, prev, pos);
            return HeaderStatus::UnknownParent;
        }
        let Some(e) = reader.lookup(&hash).and_then(|id| reader.entry(id)) else {
            return HeaderStatus::New;
        };
        if !e.status.has(Status::HAVE_DATA) {
            return HeaderStatus::New;
        }
        self.already_had(hash, e.height);
        HeaderStatus::Stored
    }

    // Core: validation.cpp:5521-5533 at v24.0.0
    fn block(&mut self, block: &[u8], hash: &Hash256, pos: u64) -> Flow {
        let block = match decode_block(&mut &block[..]) {
            Ok(decoded) => Arc::new(decoded.into_value()),
            Err(e) => {
                // The scan's deserializer took these bytes; the node's decoder must too.
                debug_assert!(false, "a scanned block that does not decode: {e}");
                tracing::error!(
                    "LoadExternalBlockFile: block {} at {pos} does not decode: {e}",
                    BlockHash::from(*hash)
                );
                self.skip_after = true;
                return Flow::Continue;
            }
        };
        let hash = BlockHash::from(*hash);
        let block_prev = block.header().prev_hash();
        let dbp = self.reindex.as_ref().map(|r| Self::at(r.file, pos));
        match self.v.import_block(block, dbp, ImportKind::Record).recv() {
            Ok(Ok(Imported::Accepted)) => {
                self.loaded += 1;
                Flow::Continue
            }
            Ok(Ok(Imported::Rejected)) => Flow::Continue,
            // Indexed with its data since the reader's check: Core's checks are one `cs_main`
            // section, so this is its "already had" branch.
            Ok(Ok(Imported::Stored {
                height,
            })) => {
                self.already_had(hash, height);
                Flow::Continue
            }
            // V's own checks under `cs_main` agree with the reader's: a parent stays indexed.
            Ok(Ok(Imported::UnknownParent)) => {
                debug_assert!(false, "V found no parent the reader saw");
                self.out_of_order(hash, block_prev, pos);
                self.skip_after = true;
                Flow::Continue
            }
            Ok(Err(_)) => {
                self.end = Some(ScanEnd::Stopped);
                Flow::Stop
            }
            Err(_) => {
                self.end = Some(ScanEnd::Disconnected);
                Flow::Stop
            }
        }
    }

    // Core: validation.cpp:5607-5619 at v24.0.0
    fn unexpected_data(&mut self, offset: u64, error: StreamError) {
        tracing::debug!(
            target: "reindex",
            "LoadExternalBlockFile: unexpected data at file offset 0x{offset:x} - {}. continuing",
            error.what()
        );
    }

    // Core: validation.cpp:5538-5606 at v24.0.0
    fn after(&mut self, hash: &Hash256) -> Flow {
        if std::mem::take(&mut self.skip_after) {
            return Flow::Continue;
        }
        if *hash == self.genesis {
            match self.v.activate_best_chain().recv() {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    self.end = Some(ScanEnd::Stopped);
                    return Flow::Stop;
                }
                Err(_) => {
                    self.end = Some(ScanEnd::Disconnected);
                    return Flow::Stop;
                }
            }
        }
        if !self.notify_header_tip() {
            return Flow::Stop;
        }
        self.children_of(BlockHash::from(*hash))
    }
}

/// How [`load_external_file`] ended, for `ThreadImport`'s check after the file
/// ([`continue_after`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEnd {
    /// The scan ended, at the file's end or on Core's `break`, and "Loaded" was logged.
    Loaded,
    /// Core's return at the check before a record (`v:5466` at v24.0.0): no "Loaded" line. The
    /// node's shutdown flag is set.
    Shutdown,
    /// V stopped while the node's shutdown flag was clear: the import returns early, with no
    /// line (OD-CH5-71).
    Disconnected,
}

/// A `-reindex` blk file's side of [`load_external_file`]: its number, `ThreadImport`'s map,
/// shared across the files, and the reads of the children it maps.
pub struct ReindexFile<'a> {
    pub file: i32,
    pub unknown: &'a mut UnknownParents,
    pub read: &'a dyn BlockAtPos,
}

/// Core's `LoadExternalBlockFile` on the importing thread: the scan of `src` ([`ExternalImport`])
/// with `reindex`'s positions and map, or neither for `-loadblock`, then the "Loaded" line.
///
/// A record's `Err`, or a file's genesis ABC's, ends the scan as Core's `break` and is logged
/// "Loaded"; the caller's check then sees the shutdown its `AbortNode` requested. Core's outer
/// "System error" catch cannot be reached (OD-CH5-72), so it has no arm here. A reply V drops
/// reads the node's shutdown flag once: every stop that drops one requests the shutdown before
/// V's reaper runs (D55), and Core then returns at its next record without the line
/// ([`FileEnd::Shutdown`]); otherwise [`FileEnd::Disconnected`].
// Core: validation.cpp:5445-5624 at v24.0.0
pub fn load_external_file(
    v: &ChainHandle,
    params: &ChainSetup,
    src: impl Read,
    reindex: Option<ReindexFile<'_>>,
    shutdown: &dyn ShutdownFlag,
) -> FileEnd {
    let start = Instant::now();
    let mut sink = match reindex {
        Some(r) => ExternalImport::reindex(v, params, r.file, r.unknown, r.read, shutdown),
        None => ExternalImport::load_block(v, params, shutdown),
    };
    load_external_block_file(src, params.message_start, &mut sink);
    match sink.end() {
        Some(ScanEnd::Shutdown) => FileEnd::Shutdown,
        Some(ScanEnd::Disconnected) if shutdown.shutdown_requested() => FileEnd::Shutdown,
        Some(ScanEnd::Disconnected) => FileEnd::Disconnected,
        Some(ScanEnd::Stopped) | None => {
            tracing::info!(
                "Loaded {} blocks from external file in {}ms",
                sink.loaded(),
                start.elapsed().as_millis()
            );
            FileEnd::Loaded
        }
    }
}

/// `ThreadImport`'s check after each file: whether the import goes on. A requested shutdown logs
/// Core's line and returns early, as does a [`FileEnd::Disconnected`] file, without it.
// Core: node/blockstorage.cpp:917-920, :936-939 at v24.0.0
pub fn continue_after(end: FileEnd, shutdown: &dyn ShutdownFlag) -> bool {
    if end == FileEnd::Disconnected {
        return false;
    }
    if shutdown.shutdown_requested() {
        tracing::info!("Shutdown requested. Exit ThreadImport");
        return false;
    }
    true
}

#[cfg(test)]
mod tests;
