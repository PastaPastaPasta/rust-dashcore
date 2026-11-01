//! [`RefCoinsCache`]: Core's `CCoinsViewCache`, ported line by line over an ordered map (CH2 §6.5):
//! the twin the coins model (`coins_cache_model`, `coins_overlay_diff`) checks the cache against.
//! It keeps Core's flags, Core's one-layer `Flush` and Core's `logic_error`s, and nothing of the
//! cache's own: no F, prefetch map, budget or byte layout. A tip twin reads [`MemoryCoins`]; a
//! child twin reads the tip twin through [`Over`].
//!
//! Core is the reference: `coins.cpp:44-297`, identical at both pins.

use std::collections::BTreeMap;

use dash_consensus::coins::{Coin, CoinWrite, CoinsView as _, MemoryCoins};
use dash_consensus::primitives::BlockHash;

use super::OutPointKey;

/// Core's `CCoinsCacheEntry`: the coin, `None` once spent (Core's `Coin::Clear`), and its flags.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefEntry {
    pub coin: Option<Coin>,
    pub dirty: bool,
    pub fresh: bool,
}

/// Core's `std::logic_error`s in the coins cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefLogicError {
    /// "Attempted to overwrite an unspent coin (when possible_overwrite is false)".
    OverwriteUnspent,
    /// "FRESH flag misapplied to coin that exists in parent cache".
    FreshMisapplied,
}

/// What a twin reads below it: Core's `CCoinsView::GetCoin`, `Some` where Core returns true, and
/// `GetBestBlock`, `None` for the null hash.
pub trait RefBase {
    fn get_coin(&mut self, k: &OutPointKey) -> Option<Coin>;
    fn best_block(&mut self) -> Option<BlockHash>;
}

impl RefBase for MemoryCoins {
    // Core: txdb.cpp:90-92 CCoinsViewDB::GetCoin (v23.1.8 :81-83)
    fn get_coin(&mut self, k: &OutPointKey) -> Option<Coin> {
        let Ok(coin) = self.coin(&k.outpoint());
        coin.map(|c| c.to_coin())
    }

    // Core: txdb.cpp:98-103 CCoinsViewDB::GetBestBlock
    fn best_block(&mut self) -> Option<BlockHash> {
        dash_consensus::coins::CoinsView::best_block(self)
    }
}

/// A twin over the view below it: the parent a child twin reads.
pub struct Over<'a> {
    pub cache: &'a mut RefCoinsCache,
    pub base: &'a mut dyn RefBase,
}

impl RefBase for Over<'_> {
    // Core: coins.cpp:61-68 GetCoin
    fn get_coin(&mut self, k: &OutPointKey) -> Option<Coin> {
        let e = self.cache.fetch_coin(self.base, k)?;
        e.coin.clone()
    }

    fn best_block(&mut self) -> Option<BlockHash> {
        self.cache.best_block(self.base)
    }
}

/// Core's `CCoinsViewCache` over a [`RefBase`] passed to each call.
#[derive(Clone, Debug, Default)]
pub struct RefCoinsCache {
    map: BTreeMap<OutPointKey, RefEntry>,
    best: Option<BlockHash>,
}

impl RefCoinsCache {
    pub fn new() -> Self {
        Self::default()
    }

    // Core: coins.cpp:44-59 FetchCoin
    fn fetch_coin(&mut self, base: &mut dyn RefBase, k: &OutPointKey) -> Option<&mut RefEntry> {
        if !self.map.contains_key(k) {
            let tmp = base.get_coin(k)?;
            let mut entry = RefEntry::default();
            if tmp.is_spent() {
                // The parent only has an empty entry for this outpoint.
                entry.fresh = true;
            } else {
                entry.coin = Some(tmp);
            }
            self.map.insert(*k, entry);
        }
        self.map.get_mut(k)
    }

    // Core: coins.cpp:70-108 AddCoin
    /// `AddCoin`. An unspendable output is not added.
    ///
    /// # Errors
    ///
    /// [`RefLogicError::OverwriteUnspent`].
    ///
    /// # Panics
    ///
    /// On a spent coin, as Core asserts.
    pub fn add_coin(
        &mut self,
        k: &OutPointKey,
        coin: Coin,
        possible_overwrite: bool,
    ) -> Result<(), RefLogicError> {
        assert!(!coin.is_spent());
        if coin.as_ref().script_pubkey().is_unspendable() {
            return Ok(());
        }
        let e = self.map.entry(*k).or_default();
        let mut fresh = false;
        if !possible_overwrite {
            if e.coin.is_some() {
                return Err(RefLogicError::OverwriteUnspent);
            }
            fresh = !e.dirty;
        }
        e.coin = Some(coin);
        e.dirty = true;
        e.fresh |= fresh;
        Ok(())
    }

    // Core: coins.cpp:129-149 SpendCoin
    /// `SpendCoin` with moveout: `None` where Core returns false, else the moved-out coin (`None`
    /// inside for a spent one).
    pub fn spend_coin(&mut self, base: &mut dyn RefBase, k: &OutPointKey) -> Option<Option<Coin>> {
        let e = self.fetch_coin(base, k)?;
        let coin = e.coin.take();
        if e.fresh {
            self.map.remove(k);
        } else {
            e.dirty = true;
        }
        Some(coin)
    }

    // Core: coins.cpp:153-160 AccessCoin
    /// `AccessCoin`: the coin, `None` for Core's empty coin.
    pub fn access_coin(&mut self, base: &mut dyn RefBase, k: &OutPointKey) -> Option<Coin> {
        self.fetch_coin(base, k).and_then(|e| e.coin.clone())
    }

    // Core: coins.cpp:162-165 HaveCoin
    pub fn have_coin(&mut self, base: &mut dyn RefBase, k: &OutPointKey) -> bool {
        self.fetch_coin(base, k).is_some_and(|e| e.coin.is_some())
    }

    // Core: coins.cpp:167-170 HaveCoinInCache
    pub fn have_coin_in_cache(&self, k: &OutPointKey) -> bool {
        self.map.get(k).is_some_and(|e| e.coin.is_some())
    }

    // Core: coins.cpp:172-176 GetBestBlock
    /// `GetBestBlock`: this cache's best block, else the base's, which a null hash caches as
    /// Core's does.
    pub fn best_block(&mut self, base: &mut dyn RefBase) -> Option<BlockHash> {
        if self.best.is_none() {
            self.best = base.best_block();
        }
        self.best
    }

    // Core: coins.cpp:178-180 SetBestBlock
    pub fn set_best_block(&mut self, best: Option<BlockHash>) {
        self.best = best;
    }

    // Core: coins.cpp:182-252 BatchWrite (erase = true)
    /// `BatchWrite` of a child's map into this cache, in the child's key order. On a `logic_error`
    /// this cache is left partly merged, as Core's is.
    ///
    /// # Errors
    ///
    /// [`RefLogicError::FreshMisapplied`].
    pub fn batch_write(&mut self, child: Self) -> Result<(), RefLogicError> {
        for (k, it) in child.map {
            if !it.dirty {
                continue;
            }
            match self.map.get_mut(&k) {
                None => {
                    // Ignore an entry both spent and FRESH in the child.
                    if !(it.fresh && it.coin.is_none()) {
                        self.map.insert(
                            k,
                            RefEntry {
                                coin: it.coin,
                                dirty: true,
                                fresh: it.fresh,
                            },
                        );
                    }
                }
                Some(us) => {
                    if it.fresh && us.coin.is_some() {
                        return Err(RefLogicError::FreshMisapplied);
                    }
                    if us.fresh && it.coin.is_none() {
                        // The grandparent has no entry and the coin is spent.
                        self.map.remove(&k);
                    } else {
                        us.coin = it.coin;
                        us.dirty = true;
                    }
                }
            }
        }
        self.best = child.best;
        Ok(())
    }

    // Core: coins.cpp:254-265 Flush, into txdb.cpp:113-174 CCoinsViewDB::BatchWrite (v23.1.8
    // :104-165)
    /// `Flush` into the database twin: each DIRTY entry written (a spent one erased), the best
    /// block set, the map emptied.
    ///
    /// # Panics
    ///
    /// With no best block, as `CCoinsViewDB::BatchWrite` asserts (`txdb.cpp:119`).
    pub fn flush(&mut self, db: &mut MemoryCoins) {
        for (k, e) in std::mem::take(&mut self.map) {
            if e.dirty {
                let change = match e.coin {
                    Some(coin) => CoinWrite::Put(coin),
                    None => CoinWrite::Erase,
                };
                db.write(k.outpoint(), change);
            }
        }
        db.set_best_block(Some(self.best.expect("a flush at a best block")));
    }

    // Core: coins.cpp:284-297 Uncache
    pub fn uncache(&mut self, k: &OutPointKey) {
        if self.map.get(k).is_some_and(|e| !e.dirty && !e.fresh) {
            self.map.remove(k);
        }
    }

    /// The entries, in key order.
    pub fn entries(&self) -> impl Iterator<Item = (&OutPointKey, &RefEntry)> {
        self.map.iter()
    }

    /// The entry at `k`, for the checks.
    pub fn entry(&self, k: &OutPointKey) -> Option<&RefEntry> {
        self.map.get(k)
    }
}
