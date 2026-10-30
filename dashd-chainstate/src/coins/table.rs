//! [`CoinsTable`]: the `coins` column family (CH2 §2.6): `txid ‖ VARINT(vout)` → Core's `Coin`
//! bytes, without Core's XOR (DEC-11's own format).
//!
//! Reads go through a [`GuardedRead`] only, the engine or a snapshot of it, so an engine error
//! trips the latch, and a record that is not exactly one coin under one key goes to
//! [`GuardedEngine::corrupt`] (DEC-78, D7): Core reads such a value as missing
//! (`dbwrapper.h:324-326`) and its cursor stops there (`txdb.cpp:265-274`).

use dashd_store::{Cf, KvBatch, KvEngine};

#[cfg(feature = "planted-bugs")]
use super::Planted;
use super::key::{DB_KEY_MAX, OutPointKey};
use super::packed::{DecodeFailure, PackedCoin};
use crate::alloc_tag;
use crate::planted::planted;
use crate::store::{
    CHAINSTATE_SCHEMA, CoinsReadError, GuardedEngine, GuardedRead, GuardedSnapshot,
};

#[derive(Clone, Copy, Debug)]
pub struct CoinsTable {
    cf: Cf,
    #[cfg(feature = "planted-bugs")]
    planted: Option<Planted>,
}

impl Default for CoinsTable {
    fn default() -> Self {
        Self {
            cf: CHAINSTATE_SCHEMA.cf("coins").expect("the chainstate schema has `coins`"),
            #[cfg(feature = "planted-bugs")]
            planted: None,
        }
    }
}

impl CoinsTable {
    pub fn cf(&self) -> Cf {
        self.cf
    }

    #[cfg(feature = "planted-bugs")]
    pub fn plant(&mut self, bug: Planted) {
        self.planted = Some(bug);
    }

    /// The coin at `k`. The read runs under the coins tag, so the engine's buffer is counted in the
    /// op's peak from its allocation (CH2 §2.5 "Ownership"); it is gone when this returns.
    ///
    /// # Errors
    ///
    /// [`CoinsReadError`]: the engine failed, or the stored value is not exactly one coin (kind
    /// `Corruption`). Either way the latch has tripped.
    pub fn get(
        &self,
        db: &impl GuardedRead,
        k: &OutPointKey,
    ) -> Result<Option<PackedCoin>, CoinsReadError> {
        let (buf, n) = encode(k);
        let key = &buf[..n];
        let _tag = alloc_tag::coins();
        let Some(value) = db.read(self.cf, key)? else {
            return Ok(None);
        };
        self.decode(db.engine(), key, &value)
    }

    /// The coins at `ks`, in their order, from one batched read (CH4's prefetch; `MultiGet` on
    /// RocksDB). The read runs under the coins tag as [`CoinsTable::get`]'s does; the keys and the
    /// result's vector are not coins memory.
    ///
    /// # Errors
    ///
    /// As [`CoinsTable::get`]'s, for the first key that fails.
    pub fn multi_get(
        &self,
        db: &impl GuardedRead,
        ks: &[OutPointKey],
    ) -> Result<Vec<Option<PackedCoin>>, CoinsReadError> {
        let encoded: Vec<_> = ks.iter().map(encode).collect();
        let keys: Vec<&[u8]> = encoded.iter().map(|(buf, n)| &buf[..*n]).collect();
        let mut coins = Vec::with_capacity(ks.len());
        let _tag = alloc_tag::coins();
        for (value, key) in db.multi_read(self.cf, &keys)?.into_iter().zip(keys) {
            coins.push(match value {
                Some(v) => self.decode(db.engine(), key, &v)?,
                None => None,
            });
        }
        Ok(coins)
    }

    // Core: txdb.cpp:226-274 CCoinsViewDB::Cursor, CCoinsViewDBCursor
    /// Every coin in `snap`, in the column family's order ([`OutPointKey::db_key`]'s). A record
    /// that does not decode, key or value, goes to [`GuardedEngine::corrupt`] and is the last item,
    /// as an engine error is: Core's cursor stops silently at a bad key and reports a bad value to
    /// its caller (D7).
    pub fn cursor<'a, E: KvEngine>(
        &self,
        snap: &'a GuardedSnapshot<'_, E>,
    ) -> impl Iterator<Item = Result<(OutPointKey, PackedCoin), CoinsReadError>> + 'a {
        let table = *self;
        let mut failed = false;
        snap.scan(self.cf, &[])
            .map_while(move |record| {
                if failed {
                    return None;
                }
                let item = record.and_then(|(key, value)| {
                    let k = OutPointKey::from_db_key(&key)
                        .ok_or_else(|| snap.engine().corrupt(table.cf, &key, "not a coin key"))?;
                    Ok(table.decode(snap.engine(), &key, &value)?.map(|coin| (k, coin)))
                });
                failed = item.is_err();
                Some(item)
            })
            .filter_map(Result::transpose)
    }

    // Core: txdb.cpp:176-179 CCoinsViewDB::EstimateSize
    /// The column family's estimated size on disk: `gettxoutsetinfo`'s `disk_size`.
    ///
    /// # Errors
    ///
    /// [`CoinsReadError`] with the engine's error.
    pub fn estimate_size<E: KvEngine>(&self, db: &GuardedEngine<E>) -> Result<u64, CoinsReadError> {
        db.size_estimate(self.cf)
    }

    /// `value`, stored under `key`, as exactly one coin; anything else is corruption. X21 reads it
    /// as missing, Core's way.
    ///
    /// Inlined, with the failure out of line: with the corruption formatting in its body, a
    /// second caller in one crate (CH4-1's prefetch bench) left the whole decode out of `get`
    /// (coins-fetch-db-1k +1.58%).
    #[inline]
    fn decode<E: KvEngine>(
        &self,
        db: &GuardedEngine<E>,
        key: &[u8],
        value: &[u8],
    ) -> Result<Option<PackedCoin>, CoinsReadError> {
        match PackedCoin::from_db(value) {
            Ok(coin) => Ok(Some(coin)),
            Err(e) => self.undecodable(db, key, &e),
        }
    }

    /// [`CoinsTable::decode`]'s failure.
    #[cold]
    #[inline(never)]
    fn undecodable<E: KvEngine>(
        &self,
        db: &GuardedEngine<E>,
        key: &[u8],
        e: &DecodeFailure,
    ) -> Result<Option<PackedCoin>, CoinsReadError> {
        if planted!(self.planted, X21) {
            return Ok(None);
        }
        Err(db.corrupt(self.cf, key, &e.to_string()))
    }

    pub fn put(&self, b: &mut impl KvBatch, k: &OutPointKey, coin: &[u8]) {
        let (buf, n) = encode(k);
        b.put(self.cf, &buf[..n], coin);
    }

    pub fn erase(&self, b: &mut impl KvBatch, k: &OutPointKey) {
        let (buf, n) = encode(k);
        b.delete(self.cf, &buf[..n]);
    }
}

/// `k`'s database key: the first `n` bytes of the buffer.
fn encode(k: &OutPointKey) -> ([u8; DB_KEY_MAX], usize) {
    let mut buf = [0; DB_KEY_MAX];
    let n = k.db_key(&mut buf);
    (buf, n)
}
