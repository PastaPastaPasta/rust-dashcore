//! Core's `CCoinsView` (`coins.h`): [`CoinsView`], the read side consensus code uses, and the
//! shape of `BatchWrite` adapted to the node's sub-batched flush ([`CoinsBatch`]), with an
//! in-memory reference view ([`MemoryCoins`]).
//!
//! Reads return a [`CoinRef`] borrowed from the view, so a view can hand out Core's coin bytes
//! without decoding them into an owned [`Coin`].
//!
//! Writes are blind: a [`CoinWrite`] overwrites or erases its key without reading it, as Core's
//! `CCoinsViewDB::BatchWrite` does. Startup replay and a subscriber's rewind depend on that
//! (A1 §3.5.5, ST3a §5.6 item 13).

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fmt;

use dash_primitives::{BlockHash, OutPoint, TxHash};

use super::coin::{Coin, CoinRef};

/// A view of the unspent outputs at one block.
///
/// Reads are fallible (DEC-77). [`CoinsView::Error`] is a storage read failure and never a
/// missing coin: Core's `CCoinsViewErrorCatcher` aborts the process on one before any verdict is
/// formed (`coins.cpp:355-370`, v24.0.0-rc.3 `:356-379`), so a caller propagates it and never
/// turns it into a validation result. Views without storage use [`Infallible`].
///
/// Implementer contract (the type system enforces it only where the error type cannot be built
/// elsewhere): return an `Error` only for a storage read failure, after the node-wide fail-stop
/// latch has tripped (CH2 §2.4.4, §2.10), and never for a missing coin.
///
/// Core's database read (`CDBWrapper::Read`) reads a stored value that fails to decode as no coin,
/// not as an error; a view that reports it as `Error` instead declares that deviation.
// Core: coins.h:172 CCoinsView
pub trait CoinsView {
    /// A storage read failure.
    type Error: fmt::Debug;

    /// `GetCoin`: the unspent coin at `outpoint`, or `None` when there is none. A returned coin is
    /// never [`CoinRef::is_spent`].
    // Core: coins.h:179 CCoinsView::GetCoin
    fn coin(&self, outpoint: &OutPoint) -> Result<Option<CoinRef<'_>>, Self::Error>;

    /// `HaveCoin`: whether `outpoint` is unspent. Core's default reads the coin; a database view
    /// overrides it with a key lookup (`CCoinsViewDB::HaveCoin`), which is true for a value that
    /// does not decode. A cache answers through the coin it fetches (`CCoinsViewCache::HaveCoin`,
    /// `coins.cpp:162`), so it must not forward to its base's `have_coin`.
    // Core: coins.cpp:19 CCoinsView::HaveCoin
    fn have_coin(&self, outpoint: &OutPoint) -> Result<bool, Self::Error> {
        Ok(self.coin(outpoint)?.is_some())
    }

    /// `GetBestBlock`: the block whose state this view represents, `None` where Core returns the
    /// null hash (an empty view, or a database between the flush's first and last batch). Every
    /// view holds it in memory, so it cannot fail.
    // Core: coins.h:185 CCoinsView::GetBestBlock
    fn best_block(&self) -> Option<BlockHash>;
}

impl<T: CoinsView + ?Sized> CoinsView for &T {
    type Error = T::Error;

    fn coin(&self, outpoint: &OutPoint) -> Result<Option<CoinRef<'_>>, Self::Error> {
        (**self).coin(outpoint)
    }

    fn have_coin(&self, outpoint: &OutPoint) -> Result<bool, Self::Error> {
        (**self).have_coin(outpoint)
    }

    fn best_block(&self) -> Option<BlockHash> {
        (**self).best_block()
    }
}

/// The failure of a consensus check that reads a [`CoinsView`]: the view's read error, or the
/// check's rejection (CH2 §2.4.4).
///
/// A [`ViewOr::View`] is never turned into a rejection: Core aborts on a coins read error before
/// it forms a verdict, so the caller propagates it as fatal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewOr<E, R> {
    /// The view could not be read.
    View(E),
    /// The check rejected.
    Reject(R),
}

/// Core's `MAX_OUTPUTS_PER_BLOCK`: `MaxBlockSize()` (2,000,000) over the size of a null `CTxOut`
/// (an 8-byte value and an empty script's 1-byte length).
// Core: coins.cpp:342 MAX_OUTPUTS_PER_BLOCK
pub const MAX_OUTPUTS_PER_BLOCK: u32 = 2_000_000 / 9;

/// Core's `AccessByTxid`: the unspent output of `txid` with the lowest index below
/// [`MAX_OUTPUTS_PER_BLOCK`]. Core's `ApplyTxInUndo` uses it to fill in a zero-height undo entry's
/// height and coinbase flag. It reads up to that many keys.
// Core: coins.cpp:344 AccessByTxid
pub fn access_by_txid<'v, V: CoinsView + ?Sized>(
    view: &'v V,
    txid: &TxHash,
) -> Result<Option<CoinRef<'v>>, V::Error> {
    for index in 0..MAX_OUTPUTS_PER_BLOCK {
        if let Some(coin) = view.coin(&OutPoint {
            hash: *txid,
            index,
        })? {
            return Ok(Some(coin));
        }
    }
    Ok(None)
}

/// What Core's `CCoinsViewDB::BatchWrite` writes for one DIRTY cache entry: the coin when it is
/// unspent, an erase when it is spent. Neither reads the key.
// Core: txdb.cpp:141-149 CCoinsViewDB::BatchWrite (v23.1.8 :132-140)
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoinWrite {
    /// Store the coin's `Coin::Serialize` bytes, replacing whatever the key holds.
    Put(Coin),
    /// Erase the key, whether or not it holds a coin.
    Erase,
}

impl CoinWrite {
    /// The write for a DIRTY entry holding `coin`: Core erases a spent coin.
    pub fn for_entry(coin: Coin) -> CoinWrite {
        if coin.is_spent() {
            CoinWrite::Erase
        } else {
            CoinWrite::Put(coin)
        }
    }
}

/// The coins of one `BatchWrite`: a cache layer's DIRTY entries, and the block whose state they
/// complete.
///
/// Core writes them as one marker batch (`DB_HEAD_BLOCKS = [best_block, old tip]`), then the
/// entries in sub-batches of `-dbbatchsize`, then a last batch that sets `DB_BEST_BLOCK`. The
/// node's flush keeps the order but moves the marker into its first durable batch with the
/// index and evo state (A1 §3.5.4), so the changes are kept apart from the markers here: the
/// flush splits [`CoinsBatch::changes`] into sub-batches in any order and writes
/// [`CoinsBatch::best_block`] in its last batch. Preconditions, as for a cache layer's entries: no
/// two changes name the same outpoint, and the best block is not null.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoinsBatch {
    pub changes: Vec<(OutPoint, CoinWrite)>,
    pub best_block: BlockHash,
}

/// Core's coins database over an ordered map of Core's coin bytes: the reference view for model
/// tests and the `coins_cache_model` fuzz target.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryCoins {
    coins: BTreeMap<OutPoint, Vec<u8>>,
    best_block: Option<BlockHash>,
}

impl MemoryCoins {
    pub fn new() -> MemoryCoins {
        MemoryCoins::default()
    }

    /// Applies one change without reading the key.
    ///
    /// # Panics
    ///
    /// On a [`CoinWrite::Put`] of a spent coin, which Core's `Coin::Serialize` asserts against.
    pub fn write(&mut self, outpoint: OutPoint, change: CoinWrite) {
        match change {
            CoinWrite::Put(coin) => {
                self.coins.insert(outpoint, coin.serialize());
            }
            CoinWrite::Erase => {
                self.coins.remove(&outpoint);
            }
        }
    }

    /// Stores `bytes` as the value at `outpoint` as they are, as a database may hold them: for
    /// models of a store whose value does not decode (Core reads it as no coin, while
    /// `HaveCoin` finds the key).
    pub fn put_raw(&mut self, outpoint: OutPoint, bytes: Vec<u8>) {
        self.coins.insert(outpoint, bytes);
    }

    /// Sets the block whose state the view represents (Core's `DB_BEST_BLOCK`).
    pub fn set_best_block(&mut self, best_block: Option<BlockHash>) {
        self.best_block = best_block;
    }

    /// Core's `CCoinsViewDB::BatchWrite`, all at once: every change, then the best block.
    ///
    /// # Panics
    ///
    /// If the best block is null, as Core asserts.
    // Core: txdb.cpp:113 CCoinsViewDB::BatchWrite, :119 assert (v23.1.8 :104, :110)
    pub fn batch_write(&mut self, batch: CoinsBatch) {
        assert!(!batch.best_block.is_null(), "BatchWrite: null best block");
        debug_assert!(
            batch.changes.iter().map(|(op, _)| op).collect::<std::collections::BTreeSet<_>>().len()
                == batch.changes.len(),
            "BatchWrite: an outpoint changes twice"
        );
        for (outpoint, change) in batch.changes {
            self.write(outpoint, change);
        }
        self.best_block = Some(batch.best_block);
    }

    pub fn len(&self) -> usize {
        self.coins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.coins.is_empty()
    }

    /// The stored coin bytes in Core's `COutPoint` order: txid bytes, then index.
    pub fn iter(&self) -> impl Iterator<Item = (&OutPoint, &[u8])> {
        self.coins.iter().map(|(outpoint, bytes)| (outpoint, &bytes[..]))
    }
}

impl CoinsView for MemoryCoins {
    type Error = Infallible;

    /// A stored value that fails to decode is no coin, as Core's database read has it.
    // Core: txdb.cpp:90 CCoinsViewDB::GetCoin (v23.1.8 :81); dbwrapper.h:324-326 CDBWrapper::Read
    // (v23.1.8 :314-316)
    fn coin(&self, outpoint: &OutPoint) -> Result<Option<CoinRef<'_>>, Infallible> {
        Ok(self.coins.get(outpoint).and_then(|bytes| CoinRef::decode(&mut &bytes[..]).ok()))
    }

    /// `CCoinsViewDB::HaveCoin` is `Exists(key)`: true for a stored value that
    /// [`CoinsView::coin`] cannot decode.
    // Core: txdb.cpp:94 CCoinsViewDB::HaveCoin (v23.1.8 :85)
    fn have_coin(&self, outpoint: &OutPoint) -> Result<bool, Infallible> {
        Ok(self.coins.contains_key(outpoint))
    }

    fn best_block(&self) -> Option<BlockHash> {
        self.best_block
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_types::Numeric as _;

    fn outpoint(byte: u8, index: u32) -> OutPoint {
        OutPoint {
            hash: TxHash::from_lendian([byte; 32]),
            index,
        }
    }

    #[test]
    fn batch_writes_are_blind_overwrites_and_erases() {
        let mut view = MemoryCoins::new();
        assert_eq!(view.best_block(), None);
        let a = Coin::new(1, &[0x51], 5, false);
        let b = Coin::new(2, &[0x52], 6, true);
        let best = BlockHash::from_lendian([9; 32]);
        view.batch_write(CoinsBatch {
            changes: vec![
                (outpoint(1, 0), CoinWrite::Put(a.clone())),
                // Erasing a key that is not there is fine.
                (outpoint(2, 0), CoinWrite::Erase),
                (outpoint(1, 1), CoinWrite::for_entry(b.clone())),
            ],
            best_block: best,
        });
        let Ok(stored) = view.coin(&outpoint(1, 0));
        assert_eq!(stored.map(|c| c.to_coin()), Some(a.clone()));
        assert_eq!(view.iter().next().map(|(_, bytes)| bytes.to_vec()), Some(a.serialize()));
        assert_eq!(view.have_coin(&outpoint(1, 1)), Ok(true));
        assert_eq!(view.have_coin(&outpoint(2, 0)), Ok(false));
        assert_eq!(view.best_block(), Some(best));

        // Overwrite, and erase through a spent entry.
        view.write(outpoint(1, 1), CoinWrite::Put(Coin::new(3, &[], 7, false)));
        view.write(outpoint(1, 0), CoinWrite::for_entry(Coin::new(-1, &[], 0, false)));
        assert_eq!(view.len(), 1);
        let Ok(stored) = view.coin(&outpoint(1, 1));
        assert_eq!(stored.map(|c| c.value()), Some(3));
    }

    /// Core's database read: a stored value that does not decode is no coin (`CDBWrapper::Read`
    /// catches the exception), while `HaveCoin` is a key lookup and finds it; trailing bytes
    /// after a coin are ignored.
    #[test]
    fn an_undecodable_value_is_no_coin_but_its_key_exists() {
        let mut view = MemoryCoins::new();
        // Code, amount, then a raw script of 1 byte with nothing behind it.
        view.put_raw(outpoint(4, 0), vec![0x02, 0x09, 0x07]);
        let Ok(coin) = view.coin(&outpoint(4, 0));
        assert!(coin.is_none());
        assert_eq!(view.have_coin(&outpoint(4, 0)), Ok(true));
        // The default `have_coin`, a cache's, goes through the coin and reports it missing.
        struct Cache<'v>(&'v MemoryCoins);
        impl CoinsView for Cache<'_> {
            type Error = Infallible;
            fn coin(&self, op: &OutPoint) -> Result<Option<CoinRef<'_>>, Infallible> {
                self.0.coin(op)
            }
            fn best_block(&self) -> Option<BlockHash> {
                self.0.best_block()
            }
        }
        assert_eq!(Cache(&view).have_coin(&outpoint(4, 0)), Ok(false));

        let mut bytes = Coin::new(7, &[0x51], 3, false).serialize();
        bytes.extend([0xee; 4]);
        view.put_raw(outpoint(4, 1), bytes);
        let Ok(coin) = view.coin(&outpoint(4, 1));
        assert_eq!(coin.map(|c| c.value()), Some(7));
    }

    #[test]
    fn access_by_txid_finds_the_lowest_unspent_index() {
        let mut view = MemoryCoins::new();
        let Ok(none) = access_by_txid(&view, &TxHash::from_lendian([1; 32]));
        assert!(none.is_none());
        view.write(outpoint(1, 4), CoinWrite::Put(Coin::new(4, &[], 8, true)));
        view.write(outpoint(1, 2), CoinWrite::Put(Coin::new(2, &[], 8, true)));
        view.write(outpoint(3, 0), CoinWrite::Put(Coin::new(9, &[], 8, true)));
        let Ok(found) = access_by_txid(&view, &TxHash::from_lendian([1; 32]));
        assert_eq!(found.map(|c| c.value()), Some(2));
        // Through a reference.
        let by_ref: &dyn CoinsView<Error = Infallible> = &view;
        assert_eq!(by_ref.have_coin(&outpoint(3, 0)), Ok(true));
    }

    #[test]
    fn the_output_bound_is_cores() {
        assert_eq!(MAX_OUTPUTS_PER_BLOCK, 222_222);
    }
}
