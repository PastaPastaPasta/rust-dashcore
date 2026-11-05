//! [`BlockOverlay`]: Core's child view over the tip (CH2 §2.4.2), `CCoinsViewCache view(&CoinsTip())`
//! (connect at `validation.cpp:3430`, disconnect at `:3279`).
//!
//! Mutations take `&mut self` and reads `&self`, alternating as Core's connect loop does. The child
//! layer holds only what the block itself did: coins it added or restored, and tombstones of coins
//! it spent. Unlike Core's child, it keeps **no copy** of a coin it read (rev5, O-R6-m3; §2.5
//! "Ownership"): a read resolves child → active → F → prefetch → database each time, and a borrow it
//! returns ends at the next `&mut` call, so nothing the child holds points into a parent layer.
//! [`BlockOverlay::have_inputs`] caches each input in active, as Core's parent `FetchCoin` does, so
//! the `&self` checks that follow hit memory. A database hit that a `&self` read cannot cache goes
//! to `scratch` ([`StableBytes`]), counted as overlay usage and freed with the overlay.
//!
//! The overlay starts empty: special-transaction processing, which runs before the UTXO loop, sees
//! the `pprev` state (quirk 26). Dropping it discards the block's changes (a failed connect).

use std::mem::ManuallyDrop;

use dash_chain::BlockRef;
use dash_consensus::coins::{CoinRef, CoinsView};
use dash_consensus::primitives::{BlockHash, OutPoint, Tx};
use dashd_store::KvEngine;

use super::budget::Usage;
use super::cache::{Below, CoinSrc, CoinsCache, check_layer};
use super::key::OutPointKey;
use super::layer::{CoinLayerOps, Flags, ShardedLayer};
use super::packed::{PackedCoin, decode_trusted};
use super::stable::StableBytes;
use crate::alloc_tag;
use crate::error::CoinsError;
use crate::planted::planted;
use crate::store::CoinsReadError;

pub struct BlockOverlay<'c, E: KvEngine> {
    tip: &'c mut CoinsCache<E>,
    /// Returned to the tip when the overlay drops, for the next block (CH2c D36). Only `Drop`
    /// takes it out.
    child: ManuallyDrop<ShardedLayer>,
    scratch: StableBytes,
}

impl<'c, E: KvEngine> BlockOverlay<'c, E> {
    pub fn new(tip: &'c mut CoinsCache<E>) -> Self {
        let child = ManuallyDrop::new(tip.take_child());
        Self {
            tip,
            child,
            scratch: StableBytes::default(),
        }
    }

    // Core: coins.cpp:303-313 HaveInputs
    /// `HaveInputs`: whether every input of a non-coinbase `tx` is unspent. Each input is cached in
    /// active as clean, as Core's parent `FetchCoin` caches it; the child keeps no copy.
    ///
    /// # Errors
    ///
    /// [`CoinsReadError`] from the database.
    pub fn have_inputs(&mut self, tx: &Tx) -> Result<bool, CoinsReadError> {
        if tx.is_coinbase() {
            return Ok(true);
        }
        for input in &tx.raw().inputs {
            let k = OutPointKey::new(&input.prevout);
            let have = match self.child.get(&k) {
                Some(e) => !e.flags.is_spent(),
                None if planted!(self.tip.planted, X30) => self.have_coin(&input.prevout)?,
                None => self.tip.fetch(&k)?.is_some(),
            };
            if !have {
                return Ok(false);
            }
        }
        Ok(true)
    }

    // Core: coins.cpp:70-108 AddCoin (on the child)
    /// `AddCoin` on the child. FRESH marking reads the child's own entry, as Core's child does.
    /// With `possible_overwrite` false, a coin unspent in the child, or with no child entry unspent
    /// in active or F, is an error (CH2 §3.2).
    ///
    /// # Errors
    ///
    /// [`CoinsError::OverwriteUnspent`] (Core's `logic_error`); [`CoinsError::ValueRange`] for a
    /// null output.
    pub fn add(
        &mut self,
        k: &OutPointKey,
        coin: CoinSrc<'_>,
        possible_overwrite: bool,
    ) -> Result<(), CoinsError> {
        let Some(coin) = coin.pack()? else {
            return Ok(());
        };
        let own = self.child.get(k).map(|e| e.flags);
        let mut fresh = false;
        if !possible_overwrite {
            let unspent = match own {
                Some(flags) => !flags.is_spent(),
                None => self.tip.have_in_cache(k),
            };
            if unspent {
                return Err(CoinsError::OverwriteUnspent);
            }
            fresh = !own.is_some_and(Flags::is_dirty);
        }
        let mut flags = own.unwrap_or(Flags::NONE).without(Flags::SPENT).union(Flags::DIRTY);
        if fresh {
            flags = flags | Flags::FRESH;
        }
        self.child.upsert(k, flags, coin.as_bytes());
        Ok(())
    }

    // Core: coins.cpp:129-149 SpendCoin (with moveout, on the child)
    /// `SpendCoin` with moveout, for the block's undo: the coin spent, or `None` if there was no
    /// unspent coin (Core returns false, or true with a spent coin; the undo gets nothing either
    /// way).
    ///
    /// A coin the child owns moves out (a long coin's box without a copy). Any other coin is
    /// **copied**, and the parent is untouched until [`BlockOverlay::commit`]. Either way the child
    /// then holds Core's result: no entry for a FRESH coin, else a DIRTY tombstone. A long coin
    /// comes back in a `LedgerBox`, counted in `Usage::held_out` until it drops.
    ///
    /// # Errors
    ///
    /// [`CoinsError::Read`] from the database.
    pub fn spend(&mut self, k: &OutPointKey) -> Result<Option<PackedCoin>, CoinsError> {
        let coin = match self.child.get(k).map(|e| e.flags) {
            Some(flags) => {
                let tombstone = (!flags.contains(Flags::FRESH))
                    .then(|| flags.union(Flags::DIRTY).union(Flags::SPENT));
                self.child.take(k, tombstone).and_then(|(_, coin)| coin)
            }
            None => {
                let coin = {
                    let Some(bytes) = self.tip.fetch_bytes(k)? else {
                        return Ok(None);
                    };
                    let _tag = alloc_tag::coins();
                    PackedCoin::from_trusted(bytes)
                };
                self.child.upsert(k, Flags::DIRTY | Flags::SPENT, &[]);
                Some(coin)
            }
        };
        Ok(coin.map(|c| c.held(self.tip.held_out())))
    }

    // Core: coins.cpp:178-180 SetBestBlock, then :254-265 Flush (the child's: BatchWrite into the
    // tip, erase = true)
    /// Merges the child into the tip (CH2 §3.2's table), sets the tip's best block to `best`, the
    /// block's, and drops the overlay. Every Core flush of a block's view sets its best block first
    /// (`validation.cpp:2365`, `:2538`, `:2912`), so it is an argument here: a commit cannot leave
    /// the tip's unset (O4).
    ///
    /// ```compile_fail,E0061
    /// # use dashd_chainstate::coins::BlockOverlay;
    /// # use dashd_store::KvEngine;
    /// fn connect<E: KvEngine>(overlay: BlockOverlay<'_, E>) {
    ///     overlay.commit().unwrap(); // no best block: does not compile
    /// }
    /// ```
    ///
    /// ```
    /// # use dashd_chainstate::coins::{BlockOverlay, BlockRef};
    /// # use dashd_store::KvEngine;
    /// fn connect<E: KvEngine>(overlay: BlockOverlay<'_, E>, best: BlockRef) {
    ///     overlay.commit(best).unwrap();
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// [`CoinsError::FreshMisapplied`] (Core's `logic_error`): fatal; the tip is then partly
    /// merged, as Core's is.
    pub fn commit(mut self, best: BlockRef) -> Result<(), CoinsError> {
        self.tip.merge(&mut self.child, best)
    }

    /// The tip's native ledger with this overlay's child layer and scratch in it.
    pub fn usage(&self) -> Usage {
        Usage {
            overlay: self.child.allocated_bytes() + self.scratch.allocated_bytes(),
            ..self.tip.usage()
        }
    }

    /// The child layer: the block's own entries.
    pub fn child(&self) -> &ShardedLayer {
        &self.child
    }

    pub fn tip(&self) -> &CoinsCache<E> {
        self.tip
    }

    /// The tip's [`CoinsCache::sanity_check`], and the same rules over the child.
    ///
    /// # Errors
    ///
    /// The first entry that breaks a rule.
    pub fn sanity_check(&self) -> Result<(), String> {
        self.tip.sanity_check()?;
        check_layer("child", &self.child, true)
    }

    /// The unspent coin at `k` through every layer, without caching anything.
    fn resolve(&self, k: &OutPointKey) -> Result<Option<CoinRef<'_>>, CoinsReadError> {
        if let Some(e) = self.child.get(k).or_else(|| self.tip.cached(k)) {
            return Ok((!e.flags.is_spent()).then(|| decode_trusted(e.coin)));
        }
        Ok(match self.tip.peek_below(k)? {
            None => None,
            Some(Below::Prefetched(coin)) => Some(coin.as_ref()),
            Some(Below::Read(coin)) => Some(decode_trusted(self.scratch.push(coin.as_bytes()))),
        })
    }
}

/// Returns the child, emptied, to the tip: the next block's overlay reuses its tables, committed
/// or not, rather than allocating 64 new ones (CH2c D36). The child moves out with nothing left in
/// its place (CH2c D49), so it needs no `Option`, and no check, at each coin it touches, and the
/// drop allocates nothing of its own.
impl<E: KvEngine> Drop for BlockOverlay<'_, E> {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the child is taken once, here, and nothing reads `self.child` after: the
        // overlay is being dropped, and `ManuallyDrop` keeps its own drop from running.
        let child = unsafe { ManuallyDrop::take(&mut self.child) };
        self.tip.keep_child(child);
    }
}

impl<E: KvEngine> CoinsView for BlockOverlay<'_, E> {
    type Error = CoinsReadError;

    // Core: coins.cpp:61-68 GetCoin, :153-160 AccessCoin (on the child)
    fn coin(&self, outpoint: &OutPoint) -> Result<Option<CoinRef<'_>>, CoinsReadError> {
        self.resolve(&OutPointKey::new(outpoint))
    }

    // Core: coins.cpp:162-165 HaveCoin (on the child)
    /// Nothing is cached, not even a database hit (BIP30's output probes miss).
    fn have_coin(&self, outpoint: &OutPoint) -> Result<bool, CoinsReadError> {
        let k = OutPointKey::new(outpoint);
        if let Some(e) = self.child.get(&k).or_else(|| self.tip.cached(&k)) {
            return Ok(!e.flags.is_spent());
        }
        Ok(self.tip.peek_below(&k)?.is_some())
    }

    // Core: coins.cpp:172-176 GetBestBlock (on the child)
    /// The tip's: the block's own is set only at [`BlockOverlay::commit`], as `ConnectBlock` sets
    /// it last (`validation.cpp:2912`) and reads it only before (`:2523`). Core's child caches the
    /// parent's hash; the tip cannot change under the overlay, so reading it each time is the same.
    fn best_block(&self) -> Option<BlockHash> {
        self.tip.best_block().map(|b| b.hash)
    }
}
