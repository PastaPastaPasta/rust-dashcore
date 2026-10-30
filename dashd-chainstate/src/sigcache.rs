//! Core's signature cache and script-execution cache (A1 §3.3; CS6): `script/sigcache.{h,cpp}`,
//! the script-execution cache in `validation.cpp` (`g_scriptExecutionCache`, `:2112-2131`, read and
//! written by `CheckInputScripts`, `:2166-2176` and `:2228-2231`), their sizes
//! (`kernel/validation_cache_sizes.h`, `node/validation_cache_args.cpp`) and `cuckoocache.h`'s
//! erase semantics.
//!
//! Both caches are sets of salted hashes, so a hit can only say "this exact (signature hash,
//! public key, signature), or this exact (txid, flags), passed before". [`CachingVerifier`]
//! memoises `CPubKey::Verify`, a deterministic function, and the script-execution cache skips a
//! transaction only when the same transaction passed under the same flags; neither can change a
//! verdict (`verdicts_never_depend_on_the_cache` and the tests in `tests/`).
//!
//! **Layout (D4).** Core's `CuckooCache` stores the 32-byte salted SHA-256s; [`SaltedCache`]
//! stores the same 32 bytes (A1 §3.3), in 8-way buckets split over up to 64 mutex-guarded shards,
//! so the same `-maxsigcachesize` budget holds about as many entries as Core's. A false hit needs
//! two entries to agree on all 256 bits, as in Core.

use std::fmt;
use std::sync::{Mutex, PoisonError};

use dash_consensus::script::{PubKeyRef, ScriptFlags, SigVerifier, verify_lax};
use dash_sha256::Sha256;

/// `DEFAULT_MAX_SIG_CACHE_BYTES`: the default `-maxsigcachesize`, 32 MiB, shared half and half
/// by the two caches.
// Core: script/sigcache.h:18 DEFAULT_MAX_SIG_CACHE_BYTES
pub const DEFAULT_MAX_SIG_CACHE_BYTES: usize = 32 << 20;

/// Core's `kernel::ValidationCacheSizes`: the byte budget of each cache.
// Core: kernel/validation_cache_sizes.h:14 ValidationCacheSizes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheSizes {
    pub signature_cache_bytes: usize,
    pub script_execution_cache_bytes: usize,
}

impl Default for CacheSizes {
    fn default() -> Self {
        Self {
            signature_cache_bytes: DEFAULT_MAX_SIG_CACHE_BYTES / 2,
            script_execution_cache_bytes: DEFAULT_MAX_SIG_CACHE_BYTES / 2,
        }
    }
}

impl CacheSizes {
    /// Core's `ApplyArgsManOptions` for `-maxsigcachesize=<n>` (MiB): `n` clamped to
    /// `[0, i64::MAX >> 20]`, converted to bytes and split in half; the default without the
    /// option.
    // Core: node/validation_cache_args.cpp:21 ApplyArgsManOptions
    pub fn from_max_sig_cache_size(max_size_mib: Option<i64>) -> Self {
        let Some(max_size) = max_size_mib else {
            return Self::default();
        };
        // Multiply first, divide after, as Core does, to avoid truncation.
        let upper_limit = i64::MAX / (1 << 20);
        let each = max_size.clamp(0, upper_limit) * (1 << 20) / 2;
        // Non-negative and at most i64::MAX / 2.
        let each = usize::try_from(each).unwrap_or(usize::MAX);
        Self {
            signature_cache_bytes: each,
            script_execution_cache_bytes: each,
        }
    }
}

/// Why a cache could not be set up: Core's `InitError("Unable to allocate memory for
/// -maxsigcachesize: …")`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheError {
    /// More than `u32::MAX` of Core's 32-byte elements: `CuckooCache::setup_bytes` refuses it
    /// (`cuckoocache.h:368`), and so do we, whatever our own layout could hold.
    TooLarge {
        bytes: usize,
    },
    /// The allocation failed (Core's `table.resize` throws `std::bad_alloc` instead, D5).
    Alloc {
        bytes: usize,
    },
    /// The OS random source failed to give the salt.
    Rng,
}

impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge {
                bytes,
            }
            | Self::Alloc {
                bytes,
            } => write!(f, "Unable to allocate memory for a {bytes}-byte validation cache"),
            Self::Rng => f.write_str("the OS random source failed"),
        }
    }
}

impl std::error::Error for CacheError {}

/// A cache key: `SHA256(nonce ‖ nonce ‖ data)`, Core's `uint256` entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CacheEntry([u8; 32]);

const WAYS: usize = 8;
const MAX_SHARDS: usize = 64;

/// The size of Core's `CuckooCache` element, a `uint256`.
const CORE_ELEMENT_BYTES: usize = 32;

/// One set of the set-associative table. `live` and `erasable` are bitmasks over the ways.
#[derive(Clone, Copy, Default)]
struct Bucket {
    keys: [[u8; 32]; WAYS],
    live: u8,
    erasable: u8,
    /// The next way to evict when every way is live and none is erasable (round robin).
    hand: u8,
}

impl Bucket {
    fn find(&self, key: &[u8; 32]) -> Option<usize> {
        (0..WAYS).find(|&w| self.live & (1 << w) != 0 && self.keys[w] == *key)
    }

    // Core: cuckoocache.h:399 cache::insert
    /// Inserts `key`, or keeps it if present. A free way is taken first, then an erasable one,
    /// then the round-robin victim: the cache only ever forgets, so eviction never matters for a
    /// verdict.
    fn insert(&mut self, key: [u8; 32]) {
        let way = if let Some(w) = self.find(&key) {
            // Core's `please_keep`: an element inserted again is no longer erasable.
            w
        } else if self.live != u8::MAX {
            self.live.trailing_ones() as usize
        } else if self.erasable != 0 {
            self.erasable.trailing_zeros() as usize
        } else {
            let w = usize::from(self.hand);
            self.hand = (self.hand + 1) % WAYS as u8;
            w
        };
        self.keys[way] = key;
        self.live |= 1 << way;
        self.erasable &= !(1 << way);
    }
}

/// Core's salted `CuckooCache::cache<uint256, SignatureCacheHasher>` (D4): a concurrent set of
/// [`CacheEntry`]s with Core's soft erase.
pub struct SaltedCache {
    /// SHA-256 after the 64-byte salt: Core writes its 32-byte nonce twice "to force the hasher
    /// to process this chunk", so each entry starts from a midstate.
    salted: Sha256,
    shards: Box<[Mutex<Box<[Bucket]>>]>,
    buckets_per_shard: usize,
    #[cfg(test)]
    counts: test_counts::Counts,
}

/// What the unit tests count on a cache: a soft erase cannot be seen otherwise, because Core's
/// `allow_erase` only marks a slot, and marking it twice looks like marking it once.
#[cfg(test)]
pub(crate) mod test_counts {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The cache's calls so far.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(crate) struct Ops {
        /// `contains` calls.
        pub lookups: usize,
        /// Hits with `erase`: Core's `allow_erase` (`cuckoocache.h:480-483`).
        pub erases: usize,
        pub inserts: usize,
    }

    impl std::ops::Sub for Ops {
        type Output = Ops;

        fn sub(self, before: Ops) -> Ops {
            Ops {
                lookups: self.lookups - before.lookups,
                erases: self.erases - before.erases,
                inserts: self.inserts - before.inserts,
            }
        }
    }

    #[derive(Default)]
    pub(super) struct Counts {
        lookups: AtomicUsize,
        erases: AtomicUsize,
        inserts: AtomicUsize,
        hashes: AtomicUsize,
    }

    impl Counts {
        pub(super) fn lookup(&self, erased: bool) {
            self.lookups.fetch_add(1, Ordering::Relaxed);
            self.erases.fetch_add(usize::from(erased), Ordering::Relaxed);
        }

        pub(super) fn insert(&self) {
            self.inserts.fetch_add(1, Ordering::Relaxed);
        }

        pub(super) fn hash(&self) {
            self.hashes.fetch_add(1, Ordering::Relaxed);
        }

        /// Entries hashed so far.
        pub(super) fn hashes(&self) -> usize {
            self.hashes.load(Ordering::Relaxed)
        }

        pub(super) fn get(&self) -> Ops {
            Ops {
                lookups: self.lookups.load(Ordering::Relaxed),
                erases: self.erases.load(Ordering::Relaxed),
                inserts: self.inserts.load(Ordering::Relaxed),
            }
        }
    }
}

impl SaltedCache {
    /// A cache in about `bytes` bytes, salted with `nonce`, as `setup_bytes` sizes Core's (never
    /// fewer than one bucket of 8 entries; Core's floor is 2).
    ///
    /// # Errors
    ///
    /// [`CacheError::TooLarge`] where Core's `setup_bytes` returns `nullopt`, and
    /// [`CacheError::Alloc`] when the table cannot be allocated.
    // Core: cuckoocache.h:365 cache::setup_bytes, :337 cache::setup
    pub fn with_bytes(bytes: usize, nonce: [u8; 32]) -> Result<Self, CacheError> {
        if bytes / CORE_ELEMENT_BYTES > u32::MAX as usize {
            return Err(CacheError::TooLarge {
                bytes,
            });
        }
        let buckets = (bytes / size_of::<Bucket>()).max(1);
        let shards = MAX_SHARDS.min(1 << buckets.ilog2());
        let buckets_per_shard = buckets / shards;
        let mut table = Vec::new();
        table.try_reserve_exact(shards).map_err(|_| CacheError::Alloc {
            bytes,
        })?;
        for _ in 0..shards {
            let mut shard = Vec::new();
            shard.try_reserve_exact(buckets_per_shard).map_err(|_| CacheError::Alloc {
                bytes,
            })?;
            shard.resize(buckets_per_shard, Bucket::default());
            table.push(Mutex::new(shard.into_boxed_slice()));
        }
        let mut salted = Sha256::new();
        salted.update(&nonce);
        salted.update(&nonce);
        Ok(Self {
            salted,
            shards: table.into_boxed_slice(),
            buckets_per_shard,
            #[cfg(test)]
            counts: test_counts::Counts::default(),
        })
    }

    /// The entries the cache can hold.
    pub fn capacity(&self) -> usize {
        self.shards.len() * self.buckets_per_shard * WAYS
    }

    /// The bytes its table takes (Core's `approx_size_bytes`).
    pub fn table_bytes(&self) -> usize {
        self.shards.len() * self.buckets_per_shard * size_of::<Bucket>()
    }

    /// The salted hash of `parts`, concatenated.
    fn entry(&self, parts: &[&[u8]]) -> CacheEntry {
        #[cfg(test)]
        self.counts.hash();
        let mut hasher = self.salted.clone();
        for part in parts {
            hasher.update(part);
        }
        CacheEntry(hasher.finalize())
    }

    fn bucket<R>(&self, entry: CacheEntry, f: impl FnOnce(&mut Bucket) -> R) -> R {
        let word = |i: usize| u64::from_le_bytes(entry.0[8 * i..8 * i + 8].try_into().unwrap());
        // The first word picks the shard and the second the bucket (a multiply-shift reduction),
        // so the two choices use independent bits of a uniform hash.
        let shard = (word(0) as usize) & (self.shards.len() - 1);
        let index = ((u128::from(word(1)) * self.buckets_per_shard as u128) >> 64) as usize;
        // Nothing panics while a shard is held, so a poisoned lock holds a consistent table.
        let mut buckets = self.shards[shard].lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut buckets[index])
    }

    /// Core's `contains(e, erase)`: whether `entry` is present. With `erase`, a hit marks it
    /// erasable: Core's `allow_erase`, which frees the slot for a later insert but keeps the
    /// entry visible until something overwrites it.
    // Core: cuckoocache.h:476 cache::contains
    pub fn contains(&self, entry: CacheEntry, erase: bool) -> bool {
        let hit = self.bucket(entry, |b| match b.find(&entry.0) {
            Some(w) => {
                if erase {
                    b.erasable |= 1 << w;
                }
                true
            }
            None => false,
        });
        #[cfg(test)]
        self.counts.lookup(hit && erase);
        hit
    }

    /// Core's `insert(e)`.
    pub fn insert(&self, entry: CacheEntry) {
        #[cfg(test)]
        self.counts.insert();
        self.bucket(entry, |b| b.insert(entry.0));
    }

    /// The calls counted so far (unit tests only).
    #[cfg(test)]
    pub(crate) fn ops(&self) -> test_counts::Ops {
        self.counts.get()
    }

    /// How many entries it has hashed (unit tests only).
    #[cfg(test)]
    pub(crate) fn hashes(&self) -> usize {
        self.counts.hashes()
    }
}

impl fmt::Debug for SaltedCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SaltedCache")
            .field("shards", &self.shards.len())
            .field("capacity", &self.capacity())
            .finish_non_exhaustive()
    }
}

fn random_nonce() -> Result<[u8; 32], CacheError> {
    let mut nonce = [0; 32];
    getrandom::fill(&mut nonce).map_err(|_| CacheError::Rng)?;
    Ok(nonce)
}

/// Core's `CSignatureCache`: entries are `SHA256(nonce ‖ nonce ‖ sighash ‖ pubkey ‖ sig)`, where
/// `sig` has no hash-type byte.
#[derive(Debug)]
pub struct SigCache(SaltedCache);

impl SigCache {
    /// `InitSignatureCache(bytes)` with a fresh random salt, logging Core's line.
    ///
    /// # Errors
    ///
    /// See [`SaltedCache::with_bytes`]; [`CacheError::Rng`] when there is no salt.
    // Core: script/sigcache.cpp:82 InitSignatureCache
    pub fn new(bytes: usize) -> Result<Self, CacheError> {
        let cache = Self::with_nonce(bytes, random_nonce()?)?;
        log_setup("signature cache", bytes, &cache.0);
        Ok(cache)
    }

    /// A cache salted with `nonce`. The node's caches are these: init step 4a (`init.cpp:1546-1552`)
    /// sizes them from `-maxsigcachesize`, draws each salt and passes them in; tests and benches
    /// pass a fixed salt, and [`Self::new`] draws its own.
    ///
    /// # Errors
    ///
    /// See [`SaltedCache::with_bytes`].
    pub fn with_nonce(bytes: usize, nonce: [u8; 32]) -> Result<Self, CacheError> {
        SaltedCache::with_bytes(bytes, nonce).map(Self)
    }

    // Core: script/sigcache.cpp:47 CSignatureCache::ComputeEntry
    pub fn entry(&self, sighash: &[u8; 32], pubkey: &[u8], sig: &[u8]) -> CacheEntry {
        self.0.entry(&[sighash, pubkey, sig])
    }

    pub fn cache(&self) -> &SaltedCache {
        &self.0
    }
}

/// Core's `g_scriptExecutionCache`: entries are `SHA256(nonce ‖ nonce ‖ txid ‖ flags)`, the
/// flags as the 4 little-endian bytes of Core's `unsigned int` on x86-64.
#[derive(Debug)]
pub struct ScriptExecCache(SaltedCache);

impl ScriptExecCache {
    /// `InitScriptExecutionCache(bytes)` with a fresh random salt, logging Core's line.
    ///
    /// # Errors
    ///
    /// See [`SaltedCache::with_bytes`]; [`CacheError::Rng`] when there is no salt.
    // Core: validation.cpp:2115 InitScriptExecutionCache
    pub fn new(bytes: usize) -> Result<Self, CacheError> {
        let cache = Self::with_nonce(bytes, random_nonce()?)?;
        log_setup("script execution cache", bytes, &cache.0);
        Ok(cache)
    }

    /// A cache salted with `nonce`. The node's caches are these: init step 4a (`init.cpp:1546-1552`)
    /// sizes them from `-maxsigcachesize`, draws each salt and passes them in; tests and benches
    /// pass a fixed salt, and [`Self::new`] draws its own.
    ///
    /// # Errors
    ///
    /// See [`SaltedCache::with_bytes`].
    pub fn with_nonce(bytes: usize, nonce: [u8; 32]) -> Result<Self, CacheError> {
        SaltedCache::with_bytes(bytes, nonce).map(Self)
    }

    /// The entry of a transaction (`txid` in Core's internal byte order) under `flags`.
    // Core: validation.cpp:2172 CheckInputScripts (hashCacheEntry)
    pub fn entry(&self, txid: &[u8; 32], flags: ScriptFlags) -> CacheEntry {
        self.0.entry(&[txid, &flags.bits().to_le_bytes()])
    }

    /// Only [`check_input_scripts`](crate::checkqueue::script::check_input_scripts) reads and
    /// stores verdicts (D24).
    pub(crate) fn cache(&self) -> &SaltedCache {
        &self.0
    }
}

fn log_setup(what: &str, requested: usize, cache: &SaltedCache) {
    tracing::info!(
        "Using {} MiB out of {} MiB requested for {what}, able to store {} elements",
        cache.table_bytes() >> 20,
        requested >> 20,
        cache.capacity()
    );
}

/// Both caches, built from one [`CacheSizes`] (Core's `init.cpp:1546-1551`).
#[derive(Debug)]
pub struct ScriptCaches {
    pub sig: SigCache,
    pub exec: ScriptExecCache,
}

impl ScriptCaches {
    /// # Errors
    ///
    /// See [`SigCache::new`].
    // Core: init.cpp:1546 (InitSignatureCache, InitScriptExecutionCache)
    pub fn new(sizes: CacheSizes) -> Result<Self, CacheError> {
        Ok(Self {
            sig: SigCache::new(sizes.signature_cache_bytes)?,
            exec: ScriptExecCache::new(sizes.script_execution_cache_bytes)?,
        })
    }
}

/// Core's `CachingTransactionSignatureChecker::VerifySignature`: the signature-cache hook of
/// CS1's transaction checker.
///
/// With `store`, a verified signature is inserted and a hit is kept; without it (block
/// connection), nothing is inserted and a hit is soft-erased, since the block will not need it
/// again.
#[derive(Clone, Copy, Debug)]
pub struct CachingVerifier<'c> {
    cache: &'c SigCache,
    store: bool,
}

impl<'c> CachingVerifier<'c> {
    pub fn new(cache: &'c SigCache, store: bool) -> Self {
        Self {
            cache,
            store,
        }
    }
}

impl SigVerifier for CachingVerifier<'_> {
    // Core: script/sigcache.cpp:93 CachingTransactionSignatureChecker::VerifySignature
    fn verify(&self, sig: &[u8], pubkey: PubKeyRef<'_>, sighash: &[u8; 32]) -> bool {
        let entry = self.cache.entry(sighash, pubkey.as_bytes(), sig);
        if self.cache.0.contains(entry, !self.store) {
            return true;
        }
        if !verify_lax(pubkey, sighash, sig) {
            return false;
        }
        if self.store {
            self.cache.0.insert(entry);
        }
        true
    }
}

#[cfg(test)]
mod tests;
