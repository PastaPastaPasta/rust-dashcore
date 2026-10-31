//! CH4-4: the connect read-ahead (DEC-835; PLAN-CH4 §CH4-4, A1 §3.4).
//!
//! Along ABC's path a reader thread reads the blocks `ConnectTip` is about to read
//! (`v:3414-3422`): it decodes each and checks its proof of work and hash, as `ReadBlockFromDisk`
//! does, then runs `CheckBlock` with its PoW and merkle checks on, as `ConnectBlock` would
//! (`v:2511`), all off V. V takes a block from it only when the block's hash and file position
//! are V's index entry's and the read and `CheckBlock` passed. That block reaches `ConnectBlock`
//! as one whose `CheckBlock` passed (CH3 D42's `fChecked`). V reads everything else itself: a
//! read error, a failed check, a mismatch, or a block not read yet. So
//! `AbortNode("Failed to read block")` and `CheckBlock`'s verdict arise at Core's block, through
//! Core's path. Under xcheck V also reads each block it takes, and a difference in the bytes or
//! the verdict stops it.
//!
//! The reader never waits on V (LO-9), only for work or for room. Its queue holds at most
//! [`DEPTH`] blocks ahead of the tip and stops taking blocks at [`BYTES`]. V waits only for
//! the block it needs, while the reader is reading that very block. A disconnect, an invalid
//! block, `invalidateblock`, `reconsiderblock` and a change of path move the epoch on: the reader
//! drops what it had queued and read for the old epoch.

use std::collections::VecDeque;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use dash_chain::{ChainReader, EntryView, FlatFilePos, Status};
use dash_consensus::header::HeaderParams;
use dash_consensus::primitives::{BlockHash, core_bytes};
use dash_consensus::{Block, BlockId};
use dash_params::ChainParams;

use super::index::entry;
use super::reply::{NodeJoinHandle, spawn_thread};
use super::seams::{BlockSource, Fatal};

/// How far ahead of the tip the reader reads, in blocks (Q8's `prefetch_depth`; A1 §3.4's block
/// LRU).
pub const DEPTH: u32 = 64;

/// The read blocks' serialized bytes at which the reader starts no further read (A1 §3.4's
/// 128 MiB). The block in flight can take the queue past it, by at most one block's size, and
/// decoded blocks take more memory than their bytes (PF5's budget).
pub const BYTES: u64 = 128 << 20;

/// V's read-ahead (CH4-4). The node runs a reader thread; tests, benches and the seams leave it
/// off unless they ask for it.
#[derive(Clone, Debug)]
pub struct ReadAheadSetup {
    threaded: bool,
    xcheck: bool,
    depth: u32,
    bytes: u64,
    #[cfg(test)]
    probe: Option<Arc<Probe>>,
}

impl Default for ReadAheadSetup {
    fn default() -> Self {
        Self::off()
    }
}

impl ReadAheadSetup {
    /// V reads every block itself, as Core's `ConnectTip` does.
    pub fn off() -> Self {
        Self {
            threaded: false,
            xcheck: false,
            depth: DEPTH,
            bytes: BYTES,
            #[cfg(test)]
            probe: None,
        }
    }

    /// A reader thread reads ahead. `xcheck` is `-consensusxcheck` (Q13): V reads each block it
    /// takes again, and compares.
    pub fn threaded(xcheck: bool) -> Self {
        Self {
            threaded: true,
            xcheck,
            ..Self::off()
        }
    }

    /// The queue's bounds (Q8).
    #[cfg(test)]
    pub(crate) fn with_bounds(self, depth: u32, bytes: u64) -> Self {
        Self {
            depth,
            bytes,
            ..self
        }
    }

    #[cfg(test)]
    pub(crate) fn probed(self, probe: &Arc<Probe>) -> Self {
        Self {
            probe: Some(probe.clone()),
            ..self
        }
    }
}

/// A block on the path, as V's index entry places it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Want {
    id: BlockId,
    hash: BlockHash,
    pos: FlatFilePos,
}

impl Want {
    /// `e`'s block where `ReadBlockFromDisk` would read it: none without data.
    fn of(e: &EntryView) -> Option<Want> {
        let file = e.file.filter(|_| e.status.has(Status::HAVE_DATA))?;
        Some(Want {
            id: e.id,
            hash: e.hash,
            pos: FlatFilePos {
                file,
                pos: e.data_pos,
            },
        })
    }
}

/// A block the reader is done with: `None` if it failed to read it, or `CheckBlock` failed it.
struct Entry {
    want: Want,
    block: Option<Arc<Block>>,
}

impl Entry {
    fn size(&self) -> u64 {
        self.block.as_ref().map_or(0, |b| u64::from(b.size()))
    }
}

#[derive(Default)]
struct State {
    epoch: u64,
    /// The blocks still to read, in path order.
    wants: VecDeque<Want>,
    /// The block in flight, and the epoch it was taken in.
    reading: Option<(BlockId, u64)>,
    /// The blocks read, in path order.
    ready: VecDeque<Entry>,
    stop: bool,
}

struct Shared {
    state: Mutex<State>,
    /// The reader waits on it for work or room; V for the block in flight.
    changed: Condvar,
    source: Arc<dyn BlockSource>,
    hp: HeaderParams,
    depth: usize,
    bytes: u64,
    #[cfg(test)]
    probe: Option<Arc<Probe>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(&self, st: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.changed.wait(st).unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes the state, and wakes whoever waits on it.
    fn update(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.lock());
        self.changed.notify_all();
    }

    fn has_room(&self, st: &State) -> bool {
        st.ready.len() < self.depth && st.ready.iter().map(Entry::size).sum::<u64>() < self.bytes
    }

    /// Waits until no block is in flight and the reader can start none: a test's settled point.
    #[cfg(test)]
    fn settle<'a>(&self, mut st: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        while st.reading.is_some() || (!st.wants.is_empty() && self.has_room(&st)) {
            st = self.wait(st);
        }
        st
    }

    /// The reader's loop: the next block wanted, read and checked outside the lock.
    fn run(&self) {
        let mut st = self.lock();
        loop {
            if st.stop {
                return;
            }
            let next = if self.has_room(&st) {
                st.wants.pop_front()
            } else {
                None
            };
            let Some(want) = next else {
                st = self.wait(st);
                continue;
            };
            let epoch = st.epoch;
            st.reading = Some((want.id, epoch));
            drop(st);
            let block = self.read_checked(&want);
            st = self.lock();
            st.reading = None;
            if st.epoch == epoch {
                st.ready.push_back(Entry {
                    want,
                    block,
                });
                #[cfg(test)]
                self.count(|c| c.max_ready = c.max_ready.max(st.ready.len()));
            }
            self.changed.notify_all();
        }
    }

    /// `ReadBlockFromDisk`, then `CheckBlock` with its PoW and merkle checks on: the block if it
    /// is `want`'s and both passed. A panic fails the block, which V then reads itself.
    fn read_checked(&self, want: &Want) -> Option<Arc<Block>> {
        #[cfg(test)]
        self.count(|c| c.reads += 1);
        panic::catch_unwind(AssertUnwindSafe(|| {
            let read = self.source.read_block(want.pos, &want.hash).ok();
            let block = read.filter(|b| b.hash() == want.hash)?;
            #[cfg(test)]
            if self.probe.as_ref().is_some_and(|p| p.unchecked) {
                return Some(block);
            }
            dash_consensus::block::check_block(&block, &self.hp, true, true).ok()?;
            Some(block)
        }))
        .ok()
        .flatten()
    }

    #[cfg(test)]
    fn count(&self, f: impl FnOnce(&mut Counts)) {
        if let Some(p) = &self.probe {
            f(&mut p.counts.lock().unwrap_or_else(PoisonError::into_inner));
        }
    }
}

/// The reader thread, and the state it shares with V.
struct Running {
    shared: Arc<Shared>,
    thread: NodeJoinHandle<()>,
}

/// V's side of the read-ahead.
pub(crate) struct ReadAhead {
    setup: ReadAheadSetup,
    /// The files' reads and the checks' parameters, until the reader starts with them.
    source: Option<(Arc<dyn BlockSource>, HeaderParams)>,
    running: Option<Running>,
    /// The last block handed to the reader in this epoch, and its height.
    planned: Option<(BlockId, u32)>,
}

impl ReadAhead {
    /// V's read-ahead over `source`, the files' reads. Without them, or without `params`'
    /// header parameters, V reads every block itself.
    pub(crate) fn new(
        setup: ReadAheadSetup,
        source: Option<Arc<dyn BlockSource>>,
        params: &ChainParams,
    ) -> ReadAhead {
        let hp = HeaderParams::from_chain_params(params).ok();
        ReadAhead {
            source: source.filter(|_| setup.threaded).zip(hp),
            setup,
            running: None,
            planned: None,
        }
    }

    /// The reader's state, if it runs.
    fn shared(&self) -> Option<&Shared> {
        self.running.as_ref().map(|r| &*r.shared)
    }

    /// [`Self::shared`], the reader started at the first call. A thread that does not start
    /// leaves V reading every block itself.
    fn start(&mut self) -> Option<&Shared> {
        if self.running.is_none()
            && let Some((source, hp)) = self.source.take()
        {
            let shared = Arc::new(Shared {
                state: Mutex::default(),
                changed: Condvar::new(),
                source,
                hp,
                depth: self.setup.depth as usize,
                bytes: self.setup.bytes,
                #[cfg(test)]
                probe: self.setup.probe.clone(),
            });
            let reader = shared.clone();
            match spawn_thread("dashd-readahead", move || reader.run()) {
                Ok(thread) => {
                    self.running = Some(Running {
                        shared,
                        thread,
                    })
                }
                Err(e) => tracing::debug!("read-ahead: no reader thread ({e}); V reads blocks"),
            }
        }
        self.shared()
    }

    /// Hands the reader the blocks toward `most_work` that follow those it has, counting from
    /// height `next`, the block V connects next: up to height `end` and at most the depth of
    /// blocks from `next`, in path order, stopping at a block without data. A plan whose last
    /// block no longer leads to `most_work` moves the epoch on first. Within one ABC the
    /// most-work block changes only after an invalid block (`v:3778-3798`), which moves the epoch
    /// itself; this move also covers an ABC after one a shutdown cut short.
    pub(crate) fn plan(&mut self, reader: &ChainReader, next: u32, most_work: BlockId, end: u32) {
        let on_path = |(id, height)| reader.ancestor(most_work, height) == Some(id);
        let from = match self.planned {
            Some(p) if on_path(p) => next.max(p.1 + 1),
            Some(_) => {
                self.advance_epoch();
                next
            }
            None => next,
        };
        let end = end.min(next.saturating_add(self.setup.depth.saturating_sub(1)));
        if from > end {
            return;
        }
        let Some(shared) = self.start() else {
            return;
        };
        let wants: Vec<Want> = (from..=end)
            .map_while(|h| Want::of(&entry(reader, reader.ancestor(most_work, h)?)))
            .collect();
        let Some(last) = wants.last() else {
            return;
        };
        let last = (last.id, from + wants.len() as u32 - 1);
        shared.update(|st| st.wants.extend(wants));
        self.planned = Some(last);
    }

    /// The block of `e` as the reader left it, when its hash and position are `e`'s and its read
    /// and `CheckBlock` passed. The reader's queue drops the blocks before it, which V has
    /// passed. A block in flight is waited for; one not started is V's to read, and the reader
    /// skips it.
    pub(crate) fn take(&self, e: &EntryView) -> Option<Arc<Block>> {
        let shared = self.shared()?;
        let want = Want::of(e)?;
        let mut st = shared.lock();
        loop {
            #[cfg(test)]
            if shared.probe.as_ref().is_some_and(|p| p.settle) {
                st = shared.settle(st);
            }
            if let Some(k) = st.ready.iter().position(|r| r.want.id == want.id) {
                let r = st.ready.drain(..=k).next_back().expect("k is in the queue");
                drop(st);
                shared.changed.notify_all();
                let block = r.block.filter(|_| r.want == want);
                #[cfg(test)]
                shared.count(|c| match &block {
                    Some(_) => c.hits += 1,
                    None => c.fallbacks += 1,
                });
                return block;
            }
            if st.reading == Some((want.id, st.epoch)) {
                st = shared.wait(st);
                continue;
            }
            if let Some(k) = st.wants.iter().position(|w| w.id == want.id) {
                st.wants.drain(..=k);
            }
            #[cfg(test)]
            shared.count(|c| c.fallbacks += 1);
            return None;
        }
    }

    /// Moves the epoch on: the reader drops what it queued and read, and finishes a block in
    /// flight for nothing.
    pub(crate) fn advance_epoch(&mut self) {
        self.planned = None;
        let Some(shared) = self.shared() else {
            return;
        };
        shared.update(|st| {
            st.epoch += 1;
            st.wants.clear();
            st.ready.clear();
        });
        #[cfg(test)]
        shared.count(|c| c.epochs += 1);
    }

    /// Under xcheck (Q13), V's own read of a block it took: the same bytes, and `CheckBlock`
    /// with its PoW and merkle checks on passes it, as the reader's did. A difference stops V.
    pub(crate) fn xcheck(
        &self,
        read: impl FnOnce() -> Result<Arc<Block>, Fatal>,
        taken: &Block,
    ) -> Result<(), Fatal> {
        let Some(shared) = self.shared().filter(|_| self.setup.xcheck) else {
            return Ok(());
        };
        let own = read()?;
        if core_bytes(&*own) != core_bytes(taken) {
            return Err(Fatal::new(format!(
                "consensusxcheck: the read-ahead's block {} differs from V's read",
                own.hash()
            )));
        }
        if let Err(err) = dash_consensus::block::check_block(&own, &shared.hp, true, true) {
            return Err(Fatal::new(format!(
                "consensusxcheck: the read-ahead passed block {}, which CheckBlock fails: {err}",
                own.hash()
            )));
        }
        Ok(())
    }

    /// Waits for the reader to finish what it can ([`Shared::settle`]).
    #[cfg(test)]
    pub(crate) fn settle(&self) {
        if let Some(s) = self.shared() {
            drop(s.settle(s.lock()));
        }
    }

    /// Stops the reader and waits for it: it holds the files' reads, so it ends before they
    /// close (PLAN-CH4 §7 risk 4). V stops it on its own thread, which carries no LO-9 marker.
    /// On a marked thread the join refuses at once, and the reader holds the files' reads until
    /// it ends on its own, after the read it has in flight.
    pub(crate) fn stop(&mut self) {
        self.source = None;
        if let Some(Running {
            shared,
            thread,
        }) = self.running.take()
        {
            shared.update(|st| st.stop = true);
            let _ = thread.join();
        }
    }
}

impl Drop for ReadAhead {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A test's view of the reader: what it did, and whether V lets it finish what it can before
/// each take, which makes every read-ahead run read the same blocks.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct Probe {
    pub(crate) settle: bool,
    /// A planted bug: the reader passes every block it reads, unchecked.
    pub(crate) unchecked: bool,
    counts: Mutex<Counts>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    /// Blocks the reader read.
    pub(crate) reads: u32,
    /// Blocks V took from it.
    pub(crate) hits: u32,
    /// Blocks V read itself.
    pub(crate) fallbacks: u32,
    pub(crate) epochs: u32,
    /// The most blocks the queue held.
    pub(crate) max_ready: usize,
}

#[cfg(test)]
impl Probe {
    pub(crate) fn settled() -> Arc<Probe> {
        Arc::new(Probe {
            settle: true,
            ..Probe::default()
        })
    }

    /// [`Probe::settled`] with the planted bug [`Probe::unchecked`].
    pub(crate) fn unchecked() -> Arc<Probe> {
        Arc::new(Probe {
            settle: true,
            unchecked: true,
            ..Probe::default()
        })
    }

    pub(crate) fn counts(&self) -> Counts {
        *self.counts.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
