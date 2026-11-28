//! A pinned view of the header chain: one publication's snapshot and the reader below it
//! (PLAN-r1 §6, r0 §2.11 item 1).

use std::sync::Arc;

use dash_chain::{
    AncestryError, AnchoredAncestry, BlockId, ChainError, ChainReadSnapshot, ChainReader,
    EntryView, MTP_UNKNOWN,
};
use dash_consensus::HeaderCtx;
use dash_num::CompactTarget;
use dash_primitives::BlockHash;
use parking_lot::RwLock;

use super::ChainGen;

/// Core's `nMedianTimeSpan`.
const MEDIAN_TIME_SPAN: usize = 11;

/// A generation's guard: each pinned read shares it, and a replacement takes it once, to mark
/// the generation retired before its store is reset. A waiting replacement holds back reads
/// that start after it, so it waits on those in flight only.
#[derive(Default)]
pub(crate) struct GenGuard(
    RwLock<bool>,
    #[cfg(test)] parking_lot::Mutex<Option<Box<dyn FnOnce() + Send>>>,
);

impl GenGuard {
    pub(crate) fn retire(&self) {
        *self.0.write() = true;
    }

    /// Tests: a `retire()` holds the guard, or waits for the reads in flight.
    #[cfg(test)]
    pub(crate) fn retiring(&self) -> bool {
        self.0.is_locked_exclusive()
    }
}

/// The chain as one publication left it, usable on any thread. Heights resolve on the
/// snapshot's chain only; ancestry by id is a branch's own, so the live reader answers it for
/// the pinned tip. No read panics. A failed or contradictory store read is
/// [`AncestryError::Storage`], and has tripped the client's fail-stop.
///
/// `BlockId`s are generation-scoped. [`AncestryError::Retired`] is answered, with nothing
/// tripped, (a) by every read on a pin whose generation was replaced, and (b) for an id this
/// pin's generation has not issued. An id from an older generation that this generation has
/// reissued resolves to this generation's block, with no error. A caller that keeps a `BlockId`
/// beyond the pin it came from must record the [`ChainGen`] it was read at, and must never pass
/// it to a pin whose `gen` differs. On a `gen` change it drops every saved id before its first
/// read on the new pin.
///
/// A caller must not read a pin while it holds a store lock: the guard is outermost in the
/// store's lock order (D52), and a waiting replacement parks new reads behind it.
#[derive(Clone)]
pub struct PinnedChain {
    reader: ChainReader,
    guard: Arc<GenGuard>,
    pub snap: Arc<ChainReadSnapshot>,
    pub gen: ChainGen,
}

impl PinnedChain {
    pub(super) fn new(reader: &ChainReader, gen: ChainGen, guard: Arc<GenGuard>) -> Self {
        Self {
            snap: reader.snapshot(),
            reader: reader.clone(),
            guard,
            gen,
        }
    }

    /// `CChain::operator[]` on the snapshot's chain: its active view, and below the view's base
    /// the ancestor of the view's lowest entry. `None` above the tip and below the floor.
    pub fn at_height(&self, h: u32) -> Result<Option<BlockId>, AncestryError> {
        let v = &self.snap.active;
        self.read(|| {
            if !self.snap.has_tip() || h > v.height() || h < self.reader.floor() {
                return Ok(None);
            }
            if let Some(id) = v.at(h) {
                return Ok(Some(id));
            }
            let base = v.at(v.base()).unwrap_or(self.snap.tip.tip);
            self.reader.try_ancestor(base, h).map_err(storage)
        })
    }

    /// Core `LookupBlockIndex`: the resident index, then the store.
    pub fn lookup(&self, hash: &BlockHash) -> Result<Option<BlockId>, AncestryError> {
        self.read(|| self.reader.try_lookup(hash).map_err(storage))
    }

    /// `id`'s entry, resident or cold: a cold entry has [`MTP_UNKNOWN`], no block data and its
    /// committed status.
    pub fn entry(&self, id: BlockId) -> Result<EntryView, AncestryError> {
        self.read(|| self.view(id))
    }

    /// This generation's latest publication.
    pub(super) fn repin(&self) -> PinnedChain {
        PinnedChain::new(&self.reader, self.gen, Arc::clone(&self.guard))
    }

    /// Core `LastCommonAncestor`: `None` for an unknown id, or branches that meet below the
    /// floor.
    pub(super) fn lca(&self, a: BlockId, b: BlockId) -> Result<Option<BlockId>, AncestryError> {
        self.read(|| self.reader.try_lca(a, b).map_err(storage))
    }

    /// Tests: `f` runs once, in the next read of this generation, holding its guard.
    #[cfg(test)]
    pub(super) fn on_read(&self, f: impl FnOnce() + Send + 'static) {
        *self.guard.1.lock() = Some(Box::new(f));
    }

    /// `f` under the generation's guard, unless it is retired. The one guard of a read: a
    /// second, under a waiting replacement, would deadlock.
    fn read<T>(&self, f: impl FnOnce() -> Result<T, AncestryError>) -> Result<T, AncestryError> {
        let retired = self.guard.0.read();
        if *retired {
            return Err(AncestryError::Retired);
        }
        #[cfg(test)]
        {
            let hook = self.guard.1.lock().take();
            if let Some(hook) = hook {
                hook();
            }
        }
        f()
    }

    /// An issued id with no record has tripped the fail-stop as corrupt; an id this generation
    /// never issued (`Ok(None)` from the store) is `Retired`.
    fn view(&self, id: BlockId) -> Result<EntryView, AncestryError> {
        self.reader.try_entry(id).map_err(storage)?.ok_or(AncestryError::Retired)
    }

    /// `GetMedianTimePast` from the records when the entry's own is unknown (a cold entry):
    /// unknown when the floor ends the eleven times above genesis.
    fn mtp(&self, e: &EntryView) -> Result<i64, AncestryError> {
        if e.mtp != MTP_UNKNOWN {
            return Ok(e.mtp);
        }
        let mut times = vec![e.header.time()];
        let mut prev = e.prev;
        while let Some(id) = prev.filter(|_| times.len() < MEDIAN_TIME_SPAN) {
            let p = self.view(id)?;
            times.push(p.header.time());
            prev = p.prev;
        }
        if times.len() < MEDIAN_TIME_SPAN && e.height as usize >= times.len() {
            return Ok(MTP_UNKNOWN);
        }
        times.sort_unstable();
        Ok(times[times.len() / 2] as i64)
    }
}

impl std::fmt::Debug for PinnedChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinnedChain")
            .field("gen", &self.gen)
            .field("snap", &self.snap)
            .finish_non_exhaustive()
    }
}

impl AnchoredAncestry for PinnedChain {
    fn floor(&self) -> u32 {
        self.reader.floor()
    }

    fn header(&self, id: BlockId) -> Result<HeaderCtx, AncestryError> {
        self.read(|| {
            let e = self.view(id)?;
            Ok(HeaderCtx {
                height: e.height,
                hash: e.hash,
                time: e.header.time(),
                bits: CompactTarget::new(e.header.bits()),
                version: e.header.version(),
                mtp: self.mtp(&e)?,
                chain_work: e.chain_work,
            })
        })
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, AncestryError> {
        self.read(|| {
            if height < self.floor() {
                return Err(AncestryError::BelowFloor {
                    height,
                });
            }
            self.view(id)?;
            self.reader.try_ancestor(id, height).map_err(storage)
        })
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, AncestryError> {
        self.read(|| match self.view(id)? {
            EntryView {
                prev: None,
                height,
                ..
            } if height > 0 => Err(AncestryError::BelowFloor {
                height: height - 1,
            }),
            e => Ok(e.prev),
        })
    }
}

fn storage(e: ChainError) -> AncestryError {
    match e {
        ChainError::Storage(e) => AncestryError::Storage(e),
        e => AncestryError::Storage(dash_chain::StoreError::Corrupt(e.to_string())),
    }
}
