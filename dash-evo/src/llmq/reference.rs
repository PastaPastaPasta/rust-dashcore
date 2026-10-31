//! Models of Core's LLMQ caches, for tests, harnesses and comparators only (LQ1 §2.1, §2.5, §6.6;
//! feature `reference`): no production path calls them.
//!
//! - [`LruCache`] and [`PerTypeCache`]: Core's `unordered_lru_cache` and `PerLlmqTypeCache`.
//! - [`ModelMembersFacade`]: Core's `GetAllQuorumMembers` with its two static caches and
//!   `PreComputeQuorumMembers`, over the pure steps of [`super::members`] and
//!   [`super::rotation`].
//! - [`ModelAttempt`]: the facade inside CH7b's attempt discipline (EV addendum §2.6): one overlay
//!   and one ordered effect log per validation attempt, the runtime rows, and the gate as a state
//!   machine, against which §2.6's conformance cases 1–6 run.
//! - [`ModelQuorumManager`]: Core's `GetQuorum` with its quorum-object cache, `ScanQuorums` with
//!   its scan cache, and `SelectQuorumForSigning`, over the facade and the mined-commitment
//!   reader (LQ1 §3.8.3's `ModelMaterializer`), recording Core's trace events.

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_params::LlmqType;
use dash_params::{ChainSetup, LlmqParams};
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric;

use super::chain::BrokenInvariant;
use super::ctx::{LlmqCtx, LlmqError, LlmqSource, SnapshotRows};
use super::members::{
    MemberError, MembersRoute, PrecomputeMode, QuorumMembers, compute_quorum_members, members_gate,
    members_route,
};
use super::mined::MinedCommitmentReader;
use super::params::{dkg_interval, enabled_quorum_params, is_quorum_rotation_enabled};
use super::rotation::compute_by_quarter_rotation;
use super::scan::{
    QuorumMaterializer, ScanCache, ScanCacheStore, ScanCacheUse, ScanEntry, scan_quorums_from,
    scan_store_block,
};
use super::selection::{select_from_scan, select_start_height};
use super::snapshot::QuorumSnapshot;
use super::view::MnListView;
use crate::effect::Effect;

/// Core's `unordered_lru_cache<Key, Value>` with a size from its constructor: every `get`,
/// `exists`, `insert` and `emplace` stamps the entry with the next access counter, and once the
/// map holds more than `2 × max_size` entries it keeps the `max_size` most recently stamped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LruCache<K, V> {
    map: BTreeMap<K, (V, i64)>,
    max_size: usize,
    access_counter: i64,
}

impl<K: Ord + Clone, V: Clone> LruCache<K, V> {
    /// An empty cache of `max_size` (Core asserts it is not zero).
    // Core: v24 unordered_lru_cache.h:30 unordered_lru_cache
    pub fn new(max_size: usize) -> Self {
        assert_ne!(max_size, 0, "Core asserts a non-zero cache size");
        LruCache {
            map: BTreeMap::new(),
            max_size,
            access_counter: 0,
        }
    }

    /// `max_size()`.
    pub fn max_size(&self) -> usize {
        self.max_size
    }

    /// The number of entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// `insert` and `emplace`: an overwrite of the value and its stamp.
    // Core: v24 unordered_lru_cache.h:39 _emplace
    pub fn insert(&mut self, key: K, value: V) {
        self.map.insert(key, (value, self.access_counter));
        self.access_counter += 1;
        self.truncate_if_needed();
    }

    /// `get`: the value, stamped. A miss stamps nothing.
    // Core: v24 unordered_lru_cache.h:61 get
    pub fn get(&mut self, key: &K) -> Option<V> {
        let entry = self.map.get_mut(key)?;
        entry.1 = self.access_counter;
        self.access_counter += 1;
        Some(entry.0.clone())
    }

    /// `exists`: stamps the entry, as `get` does.
    // Core: v24 unordered_lru_cache.h:72 exists
    pub fn exists(&mut self, key: &K) -> bool {
        self.get(key).is_some()
    }

    /// A read with no stamp: not one of Core's operations, for observers that must leave the cache
    /// as it was (EV addendum §4.3's `peek`).
    pub fn peek(&self, key: &K) -> Option<&V> {
        self.map.get(key).map(|(v, _)| v)
    }

    /// `clear`. The access counter keeps counting, as Core's does.
    pub fn clear(&mut self) {
        self.map.clear();
    }

    // Core: v24 unordered_lru_cache.h:93 truncate_if_needed
    fn truncate_if_needed(&mut self) {
        if self.map.len() <= self.max_size * 2 {
            return;
        }
        // Stamps are distinct, so the `max_size` newest are one set, as Core's `nth_element`
        // partition gives.
        let mut stamps: Vec<i64> = self.map.values().map(|(_, s)| *s).collect();
        stamps.sort_unstable_by(|a, b| b.cmp(a));
        let oldest_kept = stamps[self.max_size - 1];
        self.map.retain(|_, (_, s)| *s >= oldest_kept);
    }
}

/// Core's `PerLlmqTypeCache<Value, Key>` after `Init(consensus)`: one [`LruCache`] per configured
/// type, sized `keepOldConnections` (or `keepOldKeys` with `limit_by_connections` false). Lookups
/// of another type miss and its writes are dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PerTypeCache<K, V> {
    caches: BTreeMap<u8, LruCache<K, V>>,
}

impl<K: Ord + Clone, V: Clone> PerTypeCache<K, V> {
    /// `Init(consensus_params, limit_by_connections)`.
    ///
    /// Callers build an [`LlmqCtx`] over `params` first: outside its domain (D9) a cache size can
    /// overflow or be zero.
    // Core: v24 llmq/cache.h:45 PerLlmqTypeCache::Init
    pub fn new(params: &ChainSetup, limit_by_connections: bool) -> Self {
        Self::sized(params, |llmq| {
            if limit_by_connections {
                i32::from(llmq.keep_old_connections)
            } else {
                i32::from(llmq.keep_old_keys)
            }
        })
    }

    /// `Init(consensus_params, size_fn)`: each configured type's cache sized by `size`.
    ///
    /// Callers build an [`LlmqCtx`] over `params` first: outside its domain (D9) a cache size can
    /// overflow or be zero.
    // Core: v24 llmq/cache.h:56 PerLlmqTypeCache::Init
    pub fn sized(params: &ChainSetup, size: impl Fn(&LlmqParams) -> i32) -> Self {
        let mut caches = BTreeMap::new();
        for (llmq_type, llmq) in params.consensus.llmq_params() {
            // `std::map::emplace` keeps the first of a repeated type.
            caches
                .entry(llmq_type.to_base())
                .or_insert_with(|| LruCache::new(usize::try_from(size(llmq)).unwrap_or(0)));
        }
        PerTypeCache {
            caches,
        }
    }

    /// The type's cache, if the chain configures it.
    pub fn cache(&self, llmq_type: LlmqType) -> Option<&LruCache<K, V>> {
        self.caches.get(&llmq_type.to_base())
    }

    /// `get`.
    pub fn get(&mut self, llmq_type: LlmqType, key: &K) -> Option<V> {
        self.caches.get_mut(&llmq_type.to_base())?.get(key)
    }

    /// `insert` and `emplace`.
    pub fn insert(&mut self, llmq_type: LlmqType, key: K, value: V) {
        if let Some(cache) = self.caches.get_mut(&llmq_type.to_base()) {
            cache.insert(key, value);
        }
    }

    /// A read with no stamp ([`LruCache::peek`]).
    pub fn peek(&self, llmq_type: LlmqType, key: &K) -> Option<&V> {
        self.cache(llmq_type)?.peek(key)
    }

    /// `clear(type)`.
    pub fn clear(&mut self, llmq_type: LlmqType) {
        if let Some(cache) = self.caches.get_mut(&llmq_type.to_base()) {
            cache.clear();
        }
    }
}

/// A host for the production scan ([`super::scan_quorums`]) in tests and harnesses: Core's
/// `scanQuorumsCache`, built `PerTypeCache::sized(params, scan_cache_size)`.
impl<Q: Clone> ScanCacheStore<Q> for PerTypeCache<BlockHash, Vec<Q>> {
    fn get(&mut self, llmq_type: LlmqType, store: &BlockHash) -> Option<Vec<Q>> {
        PerTypeCache::get(self, llmq_type, store)
    }
    fn insert(&mut self, llmq_type: LlmqType, store: BlockHash, quorums: Vec<Q>) {
        PerTypeCache::insert(self, llmq_type, store, quorums);
    }
}

/// How Core's `GetAllQuorumMembers` returned (the oracle's MEMBERS trace event).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MembersOutcome {
    /// The gate: no parent, or the type is not enabled at the parent. No cache was touched.
    Gate,
    /// The base cache answered.
    BaseHit,
    /// A rotated index at or above `signingActiveQuorumCount`: no members, nothing inserted.
    IndexOutOfRange,
    /// The indexed cache answered; the base cache now holds the members too.
    IndexHit,
    /// Computed, and inserted into the base cache (and, rotated, every index of the cycle into
    /// the indexed cache).
    Computed,
}

/// A members call's result: the members and how Core's `GetAllQuorumMembers` returned them.
pub type MembersCall<X, E> = Result<(QuorumMembers<X>, MembersOutcome), LlmqError<E>>;

/// Core's `GetAllQuorumMembers` with its two static caches (LQ1 §3.7.4): the model the
/// conformance cases run against, and the reference the oracle comparisons check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelMembersFacade<X> {
    base: PerTypeCache<BlockHash, QuorumMembers<X>>,
    indexed: PerTypeCache<(BlockHash, u16), QuorumMembers<X>>,
}

impl<X: Clone> ModelMembersFacade<X> {
    /// Cold caches for `params`' configured types.
    ///
    /// Callers build an [`LlmqCtx`] over `params` first: outside its domain (D9) a cache size can
    /// overflow or be zero.
    pub fn new(params: &ChainSetup) -> Self {
        ModelMembersFacade {
            base: PerTypeCache::new(params, true),
            indexed: PerTypeCache::new(params, true),
        }
    }

    /// The base cache (`mapQuorumMembers`).
    pub fn base_cache(&self) -> &PerTypeCache<BlockHash, QuorumMembers<X>> {
        &self.base
    }

    /// The indexed cache (`mapIndexedQuorumMembers`).
    pub fn indexed_cache(&self) -> &PerTypeCache<(BlockHash, u16), QuorumMembers<X>> {
        &self.indexed
    }

    /// Core `GetAllQuorumMembers(type, base, reset_cache)`.
    // Core: v24 llmq/utils.cpp:653 GetAllQuorumMembers
    #[allow(clippy::too_many_arguments)]
    pub fn members<E, L: MnListView<Entry = X>>(
        &mut self,
        ctx: &LlmqCtx<'_, E>,
        src: &dyn LlmqSource<L, Error = E>,
        rows: &mut dyn SnapshotRows<Error = E>,
        llmq_type: LlmqType,
        base: BlockId,
        reset_cache: bool,
    ) -> MembersCall<X, E> {
        if !members_gate(ctx, llmq_type, base)? {
            return Ok((Arc::from([]), MembersOutcome::Gate));
        }
        let base_hash = ctx.hash(base)?;
        if reset_cache {
            self.base.clear(llmq_type);
        } else if let Some(members) = self.base.get(llmq_type, &base_hash) {
            return Ok((members, MembersOutcome::BaseHit));
        }

        let members = match members_route(ctx, llmq_type, base)? {
            MembersRoute::NonRotated => compute_quorum_members(ctx, src, llmq_type, base)?,
            MembersRoute::RotatedIndexOutOfRange => {
                return Ok((Arc::from([]), MembersOutcome::IndexOutOfRange));
            }
            MembersRoute::Rotated {
                cycle_base,
                index,
            } => {
                let cycle_hash = ctx.hash(cycle_base)?;
                if reset_cache {
                    self.indexed.clear(llmq_type);
                } else if let Some(members) = self.indexed.get(llmq_type, &(cycle_hash, index)) {
                    self.base.insert(llmq_type, base_hash, members.clone());
                    return Ok((members, MembersOutcome::IndexHit));
                }
                let quorums = compute_by_quarter_rotation(ctx, src, rows, llmq_type, cycle_base)?;
                let members = quorums.get(usize::from(index)).cloned().unwrap_or_default();
                for (i, q) in quorums.into_iter().enumerate() {
                    // Core's index is a `size_t`; a cycle has at most 65536 quorums.
                    if let Ok(i) = u16::try_from(i) {
                        self.indexed.insert(llmq_type, (cycle_hash, i), q);
                    }
                }
                members
            }
        };
        self.base.insert(llmq_type, base_hash, members.clone());
        Ok((members, MembersOutcome::Computed))
    }

    /// The roster a call for `base` would return from a cache, read in Core's lookup order (the
    /// base cache, then a rotated route's indexed cache) with no stamp, no insert and no copy of
    /// an indexed hit into the base cache: CH7b's `MembersFacade::peek` for mnxcheck's hit path
    /// (EV addendum §4.3). `None` is a miss. The caller checks [`members_gate`] first.
    pub fn peek<E>(
        &self,
        ctx: &LlmqCtx<'_, E>,
        llmq_type: LlmqType,
        base: BlockId,
    ) -> Result<Option<QuorumMembers<X>>, LlmqError<E>> {
        let base_hash = ctx.hash(base)?;
        if let Some(members) = self.base.peek(llmq_type, &base_hash) {
            return Ok(Some(members.clone()));
        }
        let MembersRoute::Rotated {
            cycle_base,
            index,
        } = members_route(ctx, llmq_type, base)?
        else {
            return Ok(None);
        };
        let cycle_hash = ctx.hash(cycle_base)?;
        Ok(self.indexed.peek(llmq_type, &(cycle_hash, index)).cloned())
    }

    /// Core `PreComputeQuorumMembers(block, reset_cache)`: [`members`](Self::members) for each
    /// target of `block`, the members discarded (the model of
    /// [`QuorumPrecompute`](super::QuorumPrecompute)).
    pub fn precompute<E, L: MnListView<Entry = X>>(
        &mut self,
        ctx: &LlmqCtx<'_, E>,
        src: &dyn LlmqSource<L, Error = E>,
        rows: &mut dyn SnapshotRows<Error = E>,
        block: BlockId,
        mode: PrecomputeMode,
    ) -> Result<(), LlmqError<E>> {
        precompute_targets(ctx, block, &mut |llmq_type| {
            self.members(ctx, src, rows, llmq_type, block, mode == PrecomputeMode::Undo).map(drop)
        })
    }
}

/// Core's `PreComputeQuorumMembers` loop (EV addendum §2.5): `members(type)` for every type
/// enabled at `block.prev`, in configured order, that rotates at `block` with `block` a cycle
/// base. Predicate reads and member calls interleave as in Core.
// Core: v24 llmq/blockprocessor.cpp:34 PreComputeQuorumMembers
fn precompute_targets<E>(
    ctx: &LlmqCtx<'_, E>,
    block: BlockId,
    members: &mut dyn FnMut(LlmqType) -> Result<(), LlmqError<E>>,
) -> Result<(), LlmqError<E>> {
    // A null `pprev` enables no type.
    let Some(prev) = ctx.chain().prev(block).map_err(MemberError::View)? else {
        return Ok(());
    };
    let height = ctx.height(block)?;
    for (llmq_type, params) in enabled_quorum_params(ctx, prev)? {
        if is_quorum_rotation_enabled(ctx, params, block)?
            && height.is_multiple_of(dkg_interval(params))
        {
            members(llmq_type)?;
        }
    }
    Ok(())
}

/// Snapshot rows keyed as the store keys them: the type's base byte and the cycle-base hash.
pub type ModelRows = BTreeMap<(u8, BlockHash), QuorumSnapshot>;

/// Why the model refused an external call or an attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateClosed {
    /// An attempt is open: the call waits (EV addendum §2.6 case 6). Release, then call again.
    Blocked,
    /// A runtime row write failed and the latch tripped (D4): nothing runs.
    Latched,
}

/// A runtime row write failed while an attempt's log was applied: the latch trips, D4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LatchTripped;

/// An open attempt's identity, from [`ModelAttempt::begin`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttemptId(u64);

#[derive(Clone, Debug)]
struct OpenAttempt {
    id: AttemptId,
    overlay: ModelRows,
    log: Vec<Effect>,
}

/// CH7b's members facade in its attempt discipline (EV addendum §2.6), modelled without threads:
///
/// - **Attempt** calls ([`ModelAttempt::attempt_members`]) share one overlay: a row store goes to
///   the overlay and pushes `Effect::StoreQuorumSnapshot` into the attempt's one log; a row read
///   sees the overlay, then the runtime rows. Members-cache inserts go to the facade's caches at
///   once and stay made, whatever the attempt's outcome.
/// - [`ModelAttempt::release`] applies the whole log, in order and regardless of the outcome, to
///   the runtime rows; a failing write trips the latch once (D4) and gives no verdict.
/// - **External** calls ([`ModelAttempt::external_members`], route B; and
///   [`ModelAttempt::external_snapshot`], route A) return [`GateClosed::Blocked`] while an attempt
///   is open, in place of CH7b's wait; between attempts their row writes reach the runtime rows
///   directly, as V's inline writes do.
/// - The negative controls bypass the discipline on purpose: [`ModelAttempt::direct_members`] and
///   [`ModelAttempt::direct_snapshot`] (no gate, case 6), and
///   [`ModelAttempt::per_call_members`] (a fresh overlay per call, case 1).
#[derive(Clone, Debug)]
pub struct ModelAttempt<X> {
    facade: ModelMembersFacade<X>,
    rows: ModelRows,
    open: Option<OpenAttempt>,
    next_id: u64,
    aborts: u32,
}

impl<X: Clone> ModelAttempt<X> {
    /// Cold caches and no rows, for `params`' configured types.
    ///
    /// Callers build an [`LlmqCtx`] over `params` first: outside its domain (D9) a cache size can
    /// overflow or be zero.
    pub fn new(params: &ChainSetup) -> Self {
        ModelAttempt {
            facade: ModelMembersFacade::new(params),
            rows: ModelRows::new(),
            open: None,
            next_id: 0,
            aborts: 0,
        }
    }

    /// The facade (its caches).
    pub fn facade(&self) -> &ModelMembersFacade<X> {
        &self.facade
    }

    /// The runtime rows.
    pub fn rows(&self) -> &ModelRows {
        &self.rows
    }

    /// Seeds a runtime row, as an earlier applied log would have.
    pub fn seed_row(&mut self, llmq_type: LlmqType, cycle_base: BlockHash, row: QuorumSnapshot) {
        self.rows.insert((llmq_type.to_base(), cycle_base), row);
    }

    /// How many times the latch has called `abort_node`.
    pub fn aborts(&self) -> u32 {
        self.aborts
    }

    /// The open attempt's log so far.
    ///
    /// # Panics
    ///
    /// If `id` is not the open attempt.
    pub fn log(&self, id: AttemptId) -> &[Effect] {
        &self.attempt(id).log
    }

    /// `begin_attempt`: takes the gate exclusively.
    pub fn begin(&mut self) -> Result<AttemptId, GateClosed> {
        self.check_gate()?;
        let id = AttemptId(self.next_id);
        self.next_id += 1;
        self.open = Some(OpenAttempt {
            id,
            overlay: ModelRows::new(),
            log: Vec::new(),
        });
        Ok(id)
    }

    /// A members call inside the open attempt (route C).
    ///
    /// # Panics
    ///
    /// If `id` is not the open attempt.
    #[allow(clippy::too_many_arguments)]
    pub fn attempt_members<E, L: MnListView<Entry = X>>(
        &mut self,
        id: AttemptId,
        ctx: &LlmqCtx<'_, E>,
        src: &dyn LlmqSource<L, Error = E>,
        llmq_type: LlmqType,
        base: BlockId,
        reset_cache: bool,
    ) -> MembersCall<X, E> {
        let open = open_mut(&mut self.open, id);
        let mut rows = AttemptRows::new(&mut open.overlay, &self.rows, &mut open.log);
        self.facade.members(ctx, src, &mut rows, llmq_type, base, reset_cache)
    }

    /// The precompute inside the open attempt (LQ2's `process_block` and `undo_block`).
    ///
    /// # Panics
    ///
    /// If `id` is not the open attempt.
    pub fn attempt_precompute<E, L: MnListView<Entry = X>>(
        &mut self,
        id: AttemptId,
        ctx: &LlmqCtx<'_, E>,
        src: &dyn LlmqSource<L, Error = E>,
        block: BlockId,
        mode: PrecomputeMode,
    ) -> Result<(), LlmqError<E>> {
        let open = open_mut(&mut self.open, id);
        let mut rows = AttemptRows::new(&mut open.overlay, &self.rows, &mut open.log);
        self.facade.precompute(ctx, src, &mut rows, block, mode)
    }

    /// A snapshot-row read inside the open attempt: the overlay, then the runtime rows.
    ///
    /// # Panics
    ///
    /// If `id` is not the open attempt.
    pub fn attempt_snapshot(
        &self,
        id: AttemptId,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Option<QuorumSnapshot> {
        let key = (llmq_type.to_base(), *cycle_base);
        let open = self.attempt(id);
        open.overlay.get(&key).or_else(|| self.rows.get(&key)).cloned()
    }

    /// `release_attempt` after CH7a applies the log, on every outcome (success, failure,
    /// `fJustCheck`, a GBT proposal, VerifyDB): each `StoreQuorumSnapshot` overwrites its runtime
    /// row in order. `write_fails` injects a runtime write failure: the latch trips (one
    /// `abort_node`), no later effect applies, and nothing runs afterwards.
    ///
    /// # Panics
    ///
    /// If `id` is not the open attempt.
    pub fn release(
        &mut self,
        id: AttemptId,
        write_fails: &mut dyn FnMut(&Effect) -> bool,
    ) -> Result<(), LatchTripped> {
        let open = self.open.take_if(|a| a.id == id).expect("the open attempt");
        for effect in open.log {
            if write_fails(&effect) {
                self.aborts += 1;
                return Err(LatchTripped);
            }
            if let Effect::StoreQuorumSnapshot {
                llmq_type,
                cycle_base,
                snapshot,
            } = effect
            {
                self.rows.insert((llmq_type.to_base(), cycle_base), snapshot);
            }
        }
        Ok(())
    }

    /// A members call from outside validation (route B: RPC, signing, DK1, mnxcheck), between
    /// attempts; its row writes reach the runtime rows directly.
    pub fn external_members<E, L: MnListView<Entry = X>>(
        &mut self,
        ctx: &LlmqCtx<'_, E>,
        src: &dyn LlmqSource<L, Error = E>,
        llmq_type: LlmqType,
        base: BlockId,
        reset_cache: bool,
    ) -> Result<MembersCall<X, E>, GateClosed> {
        self.check_gate()?;
        Ok(self.direct_members(ctx, src, llmq_type, base, reset_cache))
    }

    /// `RawRows::snapshot_for_block` from outside validation (route A), between attempts.
    pub fn external_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, GateClosed> {
        self.check_gate()?;
        Ok(self.direct_snapshot(llmq_type, cycle_base))
    }

    /// Negative control (case 6): [`ModelAttempt::external_members`] with neither gate nor latch.
    pub fn direct_members<E, L: MnListView<Entry = X>>(
        &mut self,
        ctx: &LlmqCtx<'_, E>,
        src: &dyn LlmqSource<L, Error = E>,
        llmq_type: LlmqType,
        base: BlockId,
        reset_cache: bool,
    ) -> MembersCall<X, E> {
        let mut rows = RuntimeRows::new(&mut self.rows);
        self.facade.members(ctx, src, &mut rows, llmq_type, base, reset_cache)
    }

    /// Negative control (case 6): [`ModelAttempt::external_snapshot`] with no gate.
    pub fn direct_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Option<QuorumSnapshot> {
        self.rows.get(&(llmq_type.to_base(), *cycle_base)).cloned()
    }

    /// Negative control (case 1): an attempt call over its own fresh overlay (EV0's per-call
    /// stub); its stores still reach the attempt's log.
    ///
    /// # Panics
    ///
    /// If `id` is not the open attempt.
    #[allow(clippy::too_many_arguments)]
    pub fn per_call_members<E, L: MnListView<Entry = X>>(
        &mut self,
        id: AttemptId,
        ctx: &LlmqCtx<'_, E>,
        src: &dyn LlmqSource<L, Error = E>,
        llmq_type: LlmqType,
        base: BlockId,
        reset_cache: bool,
    ) -> MembersCall<X, E> {
        let open = open_mut(&mut self.open, id);
        let mut overlay = ModelRows::new();
        let mut rows = AttemptRows::new(&mut overlay, &self.rows, &mut open.log);
        self.facade.members(ctx, src, &mut rows, llmq_type, base, reset_cache)
    }

    fn attempt(&self, id: AttemptId) -> &OpenAttempt {
        self.open.as_ref().filter(|a| a.id == id).expect("the open attempt")
    }

    fn check_gate(&self) -> Result<(), GateClosed> {
        if self.aborts > 0 {
            Err(GateClosed::Latched)
        } else if self.open.is_some() {
            Err(GateClosed::Blocked)
        } else {
            Ok(())
        }
    }
}

fn open_mut(open: &mut Option<OpenAttempt>, id: AttemptId) -> &mut OpenAttempt {
    open.as_mut().filter(|a| a.id == id).expect("the open attempt")
}

/// An attempt call's rows (EV addendum §2.6 `AttemptRows`): reads see the overlay, then the
/// runtime rows; a store overwrites the overlay and is logged.
struct AttemptRows<'a, E> {
    overlay: &'a mut ModelRows,
    backing: &'a ModelRows,
    log: &'a mut Vec<Effect>,
    _e: PhantomData<fn() -> E>,
}

impl<'a, E> AttemptRows<'a, E> {
    fn new(overlay: &'a mut ModelRows, backing: &'a ModelRows, log: &'a mut Vec<Effect>) -> Self {
        AttemptRows {
            overlay,
            backing,
            log,
            _e: PhantomData,
        }
    }
}

impl<E> SnapshotRows for AttemptRows<'_, E> {
    type Error = E;

    fn get(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, E> {
        let key = (llmq_type.to_base(), *cycle_base);
        Ok(self.overlay.get(&key).or_else(|| self.backing.get(&key)).cloned())
    }

    fn store(
        &mut self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), E> {
        self.overlay.insert((llmq_type.to_base(), *cycle_base), snapshot.clone());
        self.log.push(Effect::StoreQuorumSnapshot {
            llmq_type,
            cycle_base: *cycle_base,
            snapshot: snapshot.clone(),
        });
        Ok(())
    }
}

/// Rows over a [`ModelRows`], written inline: the runtime rows between attempts (route B), and
/// the rows of tests and harnesses that call the pure functions directly.
pub struct RuntimeRows<'a, E> {
    rows: &'a mut ModelRows,
    _e: PhantomData<fn() -> E>,
}

impl<'a, E> RuntimeRows<'a, E> {
    /// Rows over `rows`.
    pub fn new(rows: &'a mut ModelRows) -> Self {
        RuntimeRows {
            rows,
            _e: PhantomData,
        }
    }
}

impl<E> SnapshotRows for RuntimeRows<'_, E> {
    type Error = E;

    fn get(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, E> {
        Ok(self.rows.get(&(llmq_type.to_base(), *cycle_base)).cloned())
    }

    fn store(
        &mut self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), E> {
        self.rows.insert((llmq_type.to_base(), *cycle_base), snapshot.clone());
        Ok(())
    }
}

/// A manager call's rows: each store also records a [`ModelEvent::Row`].
struct RecordedRows<'a, E> {
    rows: &'a mut dyn SnapshotRows<Error = E>,
    events: &'a mut Vec<ModelEvent>,
}

impl<E> SnapshotRows for RecordedRows<'_, E> {
    type Error = E;

    fn get(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, E> {
        self.rows.get(llmq_type, cycle_base)
    }

    fn store(
        &mut self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), E> {
        self.rows.store(llmq_type, cycle_base, snapshot)?;
        self.events.push(ModelEvent::Row {
            llmq_type,
            cycle_base: *cycle_base,
            snapshot: snapshot.clone(),
        });
        Ok(())
    }
}

/// A quorum object of [`ModelQuorumManager`] (Core's `CQuorum`, as far as scans, selection and
/// membership read it).
#[derive(Clone, Debug)]
pub struct ModelQuorum<X> {
    /// The base block.
    pub base: BlockId,
    /// `qc->quorumHash`.
    pub quorum_hash: QuorumHash,
    /// `qc->quorumIndex`.
    pub quorum_index: i16,
    /// The block that mined the commitment.
    pub mined_block: BlockHash,
    /// The members, as the facade gave them when the quorum was built.
    pub members: QuorumMembers<X>,
}

impl<X> ScanEntry for ModelQuorum<X> {
    fn base(&self) -> BlockId {
        self.base
    }
    fn quorum_hash(&self) -> QuorumHash {
        self.quorum_hash
    }
    fn quorum_index(&self) -> i16 {
        self.quorum_index
    }
}

/// How Core's `GetQuorum` returned (the oracle's `GET_QUORUM` event).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GetQuorumOutcome {
    /// The commitment is not mined in the chain: `nullptr`, no cache access.
    NotMined,
    /// The quorum-object cache answered.
    CacheHit,
    /// Built from the mined commitment and inserted into the object cache.
    Built,
    /// No mined commitment record: `nullptr`.
    NoMinedCommitment,
}

/// One event of a [`ModelQuorumManager`] call, in Core's order (the oracle's trace): an event is
/// recorded when its operation completes, so nested ones come first (row, members, `GetQuorum`,
/// scan).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelEvent {
    /// A `StoreSnapshotForBlock` write.
    Row {
        llmq_type: LlmqType,
        cycle_base: BlockHash,
        snapshot: QuorumSnapshot,
    },
    /// A `GetAllQuorumMembers` call.
    Members {
        llmq_type: LlmqType,
        base: BlockHash,
        reset: bool,
        outcome: MembersOutcome,
        n: usize,
    },
    /// A `GetQuorum` call.
    GetQuorum {
        llmq_type: LlmqType,
        base: BlockHash,
        populate_cache: bool,
        chain_overload: bool,
        outcome: GetQuorumOutcome,
    },
    /// A `ScanQuorums` call past its gate and store block.
    Scan {
        llmq_type: LlmqType,
        store: BlockHash,
        cache: ScanCacheUse,
        cache_written: bool,
        n: usize,
    },
}

/// What a [`ModelQuorumManager`] call reads: the reading context, the per-block source, the
/// mined-commitment reader, and the active chain's tip (Core's `GetQuorum` checks a commitment is
/// mined in the active chain).
pub struct ModelViews<'v, 'c, E, L> {
    /// The reading context.
    pub ctx: &'v LlmqCtx<'c, E>,
    /// Lists and coinbases.
    pub src: &'v dyn LlmqSource<L, Error = E>,
    /// Mined commitments.
    pub reader: &'v dyn MinedCommitmentReader<Error = E>,
    /// The active chain's tip.
    pub tip: BlockId,
}

/// Core's `CQuorumManager` as LQ1 reaches it (LQ1 §3.8.3): `GetQuorum` and its quorum-object
/// cache (`keepOldKeys` per type), `ScanQuorums` and its scan cache, `SelectQuorumForSigning`,
/// over a [`ModelMembersFacade`]. A DKG-less node: no contribution build, so no second members
/// access. Every call records its events ([`ModelQuorumManager::take_events`]).
#[derive(Clone, Debug)]
pub struct ModelQuorumManager<X> {
    members: ModelMembersFacade<X>,
    objects: PerTypeCache<BlockHash, ModelQuorum<X>>,
    scans: PerTypeCache<BlockHash, Vec<ModelQuorum<X>>>,
    events: Vec<ModelEvent>,
}

impl<X: Clone> ModelQuorumManager<X> {
    /// Cold caches for `params`' configured types.
    ///
    /// Callers build an [`LlmqCtx`] over `params` first: outside its domain (D9) a cache size can
    /// overflow or be zero.
    // Core: v24 llmq/quorumsman.cpp:43 (mapQuorumsCache), :213 (scanQuorumsCache)
    pub fn new(params: &ChainSetup) -> Self {
        ModelQuorumManager {
            members: ModelMembersFacade::new(params),
            objects: PerTypeCache::new(params, false),
            scans: PerTypeCache::sized(params, |llmq| {
                llmq.max_cycles(i32::from(llmq.keep_old_connections))
                    * (i32::from(llmq.dkg_mining_window_end)
                        - i32::from(llmq.dkg_mining_window_start))
            }),
            events: Vec::new(),
        }
    }

    /// The type's quorum-object and scan cache sizes (`max_size()`), if the chain configures it.
    pub fn cache_sizes(&self, llmq_type: LlmqType) -> Option<(usize, usize)> {
        Some((self.objects.cache(llmq_type)?.max_size(), self.scans.cache(llmq_type)?.max_size()))
    }

    /// The events recorded since the last call, in order.
    pub fn take_events(&mut self) -> Vec<ModelEvent> {
        std::mem::take(&mut self.events)
    }

    /// `GetAllQuorumMembers(type, base, reset_cache)` through the facade, recorded.
    pub fn members<E, L: MnListView<Entry = X>>(
        &mut self,
        views: &ModelViews<'_, '_, E, L>,
        rows: &mut dyn SnapshotRows<Error = E>,
        llmq_type: LlmqType,
        base: BlockId,
        reset_cache: bool,
    ) -> Result<QuorumMembers<X>, LlmqError<E>> {
        let mut rows = RecordedRows {
            rows,
            events: &mut self.events,
        };
        let (members, outcome) =
            self.members.members(views.ctx, views.src, &mut rows, llmq_type, base, reset_cache)?;
        self.events.push(ModelEvent::Members {
            llmq_type,
            base: views.ctx.hash(base)?,
            reset: reset_cache,
            outcome,
            n: members.len(),
        });
        Ok(members)
    }

    /// `PreComputeQuorumMembers(block, reset_cache)` through the facade, each call recorded.
    pub fn precompute<E, L: MnListView<Entry = X>>(
        &mut self,
        views: &ModelViews<'_, '_, E, L>,
        rows: &mut dyn SnapshotRows<Error = E>,
        block: BlockId,
        mode: PrecomputeMode,
    ) -> Result<(), LlmqError<E>> {
        precompute_targets(views.ctx, block, &mut |llmq_type| {
            self.members(views, rows, llmq_type, block, mode == PrecomputeMode::Undo).map(drop)
        })
    }

    /// `GetQuorum(type, base, populate_cache)`, or with `chain_overload` its chain overload bound
    /// to the active chain.
    #[allow(clippy::too_many_arguments)]
    pub fn get_quorum<E: From<BrokenInvariant>, L: MnListView<Entry = X>>(
        &mut self,
        views: &ModelViews<'_, '_, E, L>,
        rows: &mut dyn SnapshotRows<Error = E>,
        llmq_type: LlmqType,
        base: BlockId,
        populate_cache: bool,
        chain_overload: bool,
    ) -> Result<Option<ModelQuorum<X>>, LlmqError<E>> {
        let mut mat = Materializer {
            manager: self,
            views,
            rows,
            chain_overload,
        };
        mat.get_quorum(llmq_type, base, populate_cache)
    }

    /// `ScanQuorums(type, start, count)`, or with `chain_overload` its chain overload (no scan
    /// cache): the gate and store block, the scan cache, [`scan_quorums_from`], the cache write.
    #[allow(clippy::too_many_arguments)]
    // Core: v24 llmq/quorumsman.cpp:171 CQuorumManager::ScanQuorums
    pub fn scan_quorums<E: From<BrokenInvariant>, L: MnListView<Entry = X>>(
        &mut self,
        views: &ModelViews<'_, '_, E, L>,
        rows: &mut dyn SnapshotRows<Error = E>,
        llmq_type: LlmqType,
        start: BlockId,
        count: usize,
        chain_overload: bool,
    ) -> Result<Vec<ModelQuorum<X>>, LlmqError<E>> {
        let ctx = views.ctx;
        let Some(store) = scan_store_block(ctx, llmq_type, start, count)? else {
            return Ok(Vec::new());
        };
        let store_hash = ctx.hash(store)?;
        let cached = if chain_overload {
            None
        } else {
            Some(self.scans.get(llmq_type, &store_hash))
        };
        let cache = match &cached {
            None => ScanCache::Bypass,
            Some(None) => ScanCache::Miss,
            Some(Some(quorums)) => ScanCache::Hit(quorums),
        };
        let mut mat = Materializer {
            manager: self,
            views,
            rows,
            chain_overload,
        };
        let outcome =
            scan_quorums_from(ctx, views.reader, llmq_type, store, count, cache, &mut mat)?;
        let cache_written = outcome.cache_write.is_some();
        if let Some(write) = outcome.cache_write {
            self.scans.insert(llmq_type, store_hash, write);
        }
        self.events.push(ModelEvent::Scan {
            llmq_type,
            store: store_hash,
            cache: outcome.cache,
            cache_written,
            n: outcome.result.len(),
        });
        Ok(outcome.result)
    }

    /// `SelectQuorumForSigning(params, active chain, selectionHash, signHeight, signOffset)`.
    #[allow(clippy::too_many_arguments)]
    // Core: v24 llmq/quorumsman.cpp:804 SelectQuorumForSigning
    pub fn select_quorum_for_signing<E: From<BrokenInvariant>, L: MnListView<Entry = X>>(
        &mut self,
        views: &ModelViews<'_, '_, E, L>,
        rows: &mut dyn SnapshotRows<Error = E>,
        llmq_type: LlmqType,
        selection_hash: &[u8; 32],
        sign_height: i32,
        sign_offset: i32,
    ) -> Result<Option<ModelQuorum<X>>, LlmqError<E>> {
        let ctx = views.ctx;
        let params = ctx.llmq(llmq_type)?;
        let tip_height = ctx.height(views.tip)?;
        let tip_height = i32::try_from(tip_height).map_err(|_| MemberError::Missing(views.tip))?;
        let Some(start) = select_start_height(tip_height, sign_height, sign_offset) else {
            return Ok(None);
        };
        let start = ctx.ancestor_below(views.tip, start)?;
        let rotation_enabled = is_quorum_rotation_enabled(ctx, params, start)?;
        let count = usize::from(params.signing_active_quorum_count);
        let scanned = self.scan_quorums(views, rows, llmq_type, start, count, false)?;
        Ok(select_from_scan(llmq_type, params, rotation_enabled, &scanned, selection_hash).cloned())
    }
}

/// Core's `GetQuorum` over the manager, as the scan's materializer.
struct Materializer<'m, 'v, 'c, E, L, X> {
    manager: &'m mut ModelQuorumManager<X>,
    views: &'m ModelViews<'v, 'c, E, L>,
    rows: &'m mut dyn SnapshotRows<Error = E>,
    chain_overload: bool,
}

impl<E, L, X> Materializer<'_, '_, '_, E, L, X> {
    fn record(
        &mut self,
        llmq_type: LlmqType,
        base: BlockHash,
        populate_cache: bool,
        outcome: GetQuorumOutcome,
    ) {
        self.manager.events.push(ModelEvent::GetQuorum {
            llmq_type,
            base,
            populate_cache,
            chain_overload: self.chain_overload,
            outcome,
        });
    }
}

impl<E: From<BrokenInvariant>, L: MnListView<Entry = X>, X: Clone> QuorumMaterializer
    for Materializer<'_, '_, '_, E, L, X>
{
    type Quorum = ModelQuorum<X>;
    type Error = E;

    // Core: v24 llmq/quorumsman.cpp:529 GetQuorum, :558 (chain overload), :59
    // BuildQuorumFromCommitment
    fn get_quorum(
        &mut self,
        llmq_type: LlmqType,
        base: BlockId,
        populate_cache: bool,
    ) -> Result<Option<ModelQuorum<X>>, LlmqError<E>> {
        let views = self.views;
        let base_hash = views.ctx.hash(base)?;
        let quorum_hash = QuorumHash::from(*base_hash.as_bytes());
        // `HasMinedCommitment`: the mined block is in the (active) chain.
        let mined = views.reader.mined_commitment(llmq_type, &quorum_hash, views.tip);
        if mined.map_err(MemberError::View)?.is_none() {
            self.record(llmq_type, base_hash, populate_cache, GetQuorumOutcome::NotMined);
            return Ok(None);
        }
        if let Some(quorum) = self.manager.objects.get(llmq_type, &base_hash) {
            self.record(llmq_type, base_hash, populate_cache, GetQuorumOutcome::CacheHit);
            return Ok(Some(quorum));
        }
        // `GetMinedCommitment(type, quorumHash)`: the record, read from the tip. A reader that
        // answered the mined check answers it too, so `NoMinedCommitment` needs an inconsistent one.
        let mined = views.reader.mined_commitment(llmq_type, &quorum_hash, views.tip);
        let Some((qc, mined_block)) = mined.map_err(MemberError::View)? else {
            self.record(llmq_type, base_hash, populate_cache, GetQuorumOutcome::NoMinedCommitment);
            return Ok(None);
        };
        let members = self.manager.members(views, self.rows, qc.llmq_type, base, false)?;
        let quorum = ModelQuorum {
            base,
            quorum_hash,
            quorum_index: qc.quorum_index.unwrap_or(0),
            mined_block,
            members,
        };
        // Both of Core's branches insert (size 1 with populate_cache, and after the contributions).
        self.manager.objects.insert(llmq_type, base_hash, quorum.clone());
        self.record(llmq_type, base_hash, populate_cache, GetQuorumOutcome::Built);
        Ok(Some(quorum))
    }
}
