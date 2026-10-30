//! The cache's tests (CH2 §6.1, §6.2, §6.7): Core's L0 tables, the simulations, cache membership,
//! ownership, the prefetch fence, the budget and the planted bugs. Each test world has its own
//! [`ProbeEngine`], latch and abort sink.

use std::sync::Arc;

use dash_consensus::BlockId;
use dash_consensus::primitives::{BlockHash, TxOut};
use dashd_store::Commit;

#[cfg(feature = "alloc-tag")]
use super::Usage;
use super::layer::{CoinLayerOps, Flags, ShardedLayer};
use super::model::db_coins;
use super::{
    BlockRef, CoinsBudget, CoinsCache, CoinsTable, FrozenLayer, OutPointKey, PackedCoin,
    PrefetchRequest, Ratio, Salt,
};
#[cfg(feature = "alloc-tag")]
use crate::alloc_tag::{self, Tag};
use crate::store::{GuardedEngine, ProbeEngine, RecordingSink};

mod cache;
mod l0;
mod model;
mod overlay;
mod producer;
mod sim;
mod stats_job;
mod table;
#[cfg(feature = "alloc-tag")]
mod witness;

const MIB: u64 = 1 << 20;

/// Core's coins share at `-dbcache=4` with txindex off: 1.75 MiB.
const B_4: u64 = 1_835_008;

/// R at the default `-dbbatchsize` for coins share `b`: S + 64 KiB with S = B/8 (CH2 §2.5).
#[cfg(feature = "alloc-tag")]
const fn batch_reserve(b: u64) -> u64 {
    b / 8 + (64 << 10)
}

type Db = GuardedEngine<ProbeEngine>;

/// SplitMix64: a deterministic stream for the tests.
pub(super) struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self) -> u64 {
        super::hasher::splitmix64(&mut self.0)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// True once in `n`.
    pub fn one_in(&mut self, n: u64) -> bool {
        self.below(n) == 0
    }
}

/// The tagging allocator's `coins` bytes in a fresh domain for this thread (feature `alloc-tag`).
/// Start it before the world, so the cache's first layer counts.
#[cfg(feature = "alloc-tag")]
pub(super) struct Tagged(u8);

#[cfg(feature = "alloc-tag")]
impl Tagged {
    pub fn start() -> Self {
        let d = alloc_tag::new_domain();
        alloc_tag::set_thread_domain(d);
        Self(d)
    }

    pub fn bytes(&self) -> u64 {
        alloc_tag::tagged_bytes(self.0, Tag::Coins) as u64
    }

    /// The most bytes since the last [`Tagged::reset_peak`].
    pub fn peak(&self) -> u64 {
        alloc_tag::peak_bytes(self.0, Tag::Coins) as u64
    }

    pub fn reset_peak(&self) {
        alloc_tag::reset_peak(self.0, Tag::Coins);
    }

    /// §6.7: the allocator's bytes equal native Σ − R less the unbacked ceilings, exactly.
    #[track_caller]
    pub fn check(&self, u: Usage, what: impl std::fmt::Display) {
        assert_eq!(self.bytes(), u.allocated(), "tagged ≠ Σ − R − unbacked {what}: {u:?}");
    }
}

/// A cache over its own database.
pub(super) struct World {
    pub db: Arc<Db>,
    #[cfg_attr(not(feature = "planted-bugs"), allow(dead_code))]
    pub sink: Arc<RecordingSink>,
    pub cache: CoinsCache<ProbeEngine>,
}

impl World {
    pub fn new(budget: CoinsBudget) -> Self {
        let sink = Arc::new(RecordingSink::default());
        let db = Arc::new(GuardedEngine::probe(sink.clone()));
        let cache = CoinsCache::new(db.clone(), budget, Salt::fixed(7), None);
        Self {
            db,
            sink,
            cache,
        }
    }

    /// A world at -dbcache=`mib` with no mempool allowance, f = 1/2 and no batch reserve.
    pub fn with_dbcache(mib: u64) -> Self {
        Self::new(CoinsBudget::new(mib * MIB, 0, Ratio::HALF, 0))
    }

    /// The database's point reads so far.
    pub fn db_reads(&self) -> u64 {
        self.db.inner().reads()
    }

    /// Writes `coin` (or erases `k`, for `None`) straight to the database.
    pub fn db_set(&self, k: &OutPointKey, coin: Option<&PackedCoin>) {
        let table = CoinsTable::default();
        let mut b = self.db.batch();
        match coin {
            Some(c) => table.put(&mut b, k, c.as_bytes()),
            None => table.erase(&mut b, k),
        }
        self.db.write(b, Commit::Buffered).unwrap();
    }

    /// The database's coin at `k`, read without the cache.
    pub fn db_get(&self, k: &OutPointKey) -> Option<PackedCoin> {
        CoinsTable::default().get(&*self.db, k).unwrap()
    }

    /// V's prefetch request for `keys`, stamped with the current generation as V dispatches it
    /// (CH4-1).
    pub fn request(&self, keys: Vec<OutPointKey>) -> PrefetchRequest {
        PrefetchRequest {
            fetch_gen: self.cache.released_gen(),
            keys,
        }
    }

    /// Writes F as W would, drops W's handle and releases F.
    pub fn release(&mut self, f: Arc<FrozenLayer>) {
        write_layer(&self.db, f.coins());
        let generation = f.generation();
        drop(f);
        self.cache.release(generation);
    }

    /// Freezes active at block `height`, then [`World::release`].
    pub fn flush(&mut self, height: u32) {
        let f = self.cache.freeze_at(height, 0);
        self.release(f);
    }
}

impl CoinsCache<ProbeEngine> {
    /// Sets the tip's best block to block `height`, then freezes there (O6: F's tip is the tip's
    /// best block).
    pub(super) fn freeze_at(&mut self, height: u32, mempool_usage: u64) -> Arc<FrozenLayer> {
        self.set_best_block(Some(block(height)));
        self.freeze(mempool_usage)
    }
}

/// Runs `f`, which must panic at `admit_prefetch`'s epoch check (debug builds; DEC-142, O5).
pub(super) fn assert_the_epoch_check_refuses<R>(f: impl FnOnce() -> R) {
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    let msg = caught.err().expect("the epoch check refuses the batch");
    let msg = msg.downcast_ref::<&str>().copied().unwrap_or_default();
    assert!(msg.contains("read before its generation began"), "{msg}");
}

/// A synthetic W: writes a layer's DIRTY entries to the database in one batch (CH2 §2.6's flush,
/// without the commit protocol a3 adds).
pub(super) fn write_layer(db: &Db, layer: &ShardedLayer) {
    super::model::write_dirty(db, layer.iter_dirty()).unwrap();
}

/// A block at `height` (its id and hash derived from the height).
pub(super) fn block(height: u32) -> BlockRef {
    let mut hash = [0; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    BlockRef {
        id: BlockId(height),
        hash: BlockHash::from(hash),
        height,
    }
}

/// An output paying `value` to an empty script, as Core's tests make them.
pub(super) fn output(value: i64) -> TxOut {
    TxOut {
        value,
        script_pubkey: Vec::new().into(),
    }
}

/// Core's test coin: `value` at height 1, not coinbase.
pub(super) fn coin(value: i64) -> PackedCoin {
    PackedCoin::from_output(&output(value), 1, false).unwrap()
}

/// A P2PKH script to the key hash `[fill; 20]`.
pub(super) fn p2pkh_script(fill: u8) -> Vec<u8> {
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend_from_slice(&[fill; 20]);
    script.extend_from_slice(&[0x88, 0xac]);
    script
}

/// A P2PKH output's coin: inline.
pub(super) fn p2pkh(i: u64) -> PackedCoin {
    let out = TxOut {
        value: 1_000 + i as i64,
        script_pubkey: p2pkh_script(i as u8).into(),
    };
    PackedCoin::from_output(&out, 9, false).unwrap()
}

/// A random outpoint of vout `vout`.
pub(super) fn random_key(r: &mut Rng, vout: u32) -> OutPointKey {
    let mut k = [0; 36];
    for w in 0..4 {
        k[w * 8..w * 8 + 8].copy_from_slice(&r.next().to_le_bytes());
    }
    k[32..].copy_from_slice(&vout.to_le_bytes());
    OutPointKey(k)
}
