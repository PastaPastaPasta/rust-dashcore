//! Quorum scans (LQ1 §3.8): Core's `ScanQuorums` split at its cache, resolved through the start
//! block's ancestry (§3.8.2). [`scan_quorums`] composes the pieces in Core's order (LQ3b-2):
//! [`scan_store_block`], the cache lookup, [`scan_quorums_from`] with the materializer, then the
//! cache write. The quorum manager hosts the cache ([`ScanCacheStore`]) and the materializer.

use dash_consensus::BlockId;
use dash_params::LlmqParams;
use dash_primitives::{BlockHash, LlmqType, QuorumHash};

use super::ctx::{LlmqCtx, LlmqError};
use super::members::MemberError;
use super::mined::MinedCommitmentReader;
use super::params::{dkg_interval, is_quorum_type_enabled};

/// A quorum a scan returns: Core's `CQuorumCPtr`, as far as scans and selection read it.
pub trait ScanEntry {
    /// The quorum's base block (`m_quorum_base_block_index`).
    fn base(&self) -> BlockId;
    /// `qc->quorumHash`.
    fn quorum_hash(&self) -> QuorumHash;
    /// `qc->quorumIndex`: 0 for a commitment version that carries none, as Core's default.
    fn quorum_index(&self) -> i16;
}

/// Core `CQuorumManager::GetQuorum(type, base, populate_cache)` (v24 `llmq/quorumsman.cpp:529`)
/// or, bound to an explicit chain, its chain overload (:558). QM1 implements it (LQ1 §3.8.3): the
/// mined check, the quorum-object cache, and on a miss `BuildQuorumFromCommitment`, whose members
/// come from the process-wide members facade. Every effect it makes stays made whatever the scan
/// does next.
pub trait QuorumMaterializer {
    /// The quorum objects it returns and caches.
    type Quorum: ScanEntry + Clone;
    /// Its views' error: the reader's, so a scan has one error type (EV addendum §2.2).
    type Error;

    /// `Ok(None)` exactly where Core's `GetQuorum` returns `nullptr` in the same state (not mined
    /// in the bound chain; no mined commitment record). `Err` only for a view fault (fail-stop).
    fn get_quorum(
        &mut self,
        llmq_type: LlmqType,
        base: BlockId,
        populate_cache: bool,
    ) -> Result<Option<Self::Quorum>, LlmqError<Self::Error>>;
}

/// Core's `pindexStore` (LQ1 §3.8.1 steps 1–2): the block whose hash keys the scan cache and up to
/// which commitments are read. `None` where Core returns `{}` before its cache: a count of 0, the
/// type not enabled at `start` (not its parent), or the cycle's previous mining window below
/// genesis.
// Core: v24 llmq/quorumsman.cpp:171 CQuorumManager::ScanQuorums (store block)
pub fn scan_store_block<E>(
    ctx: &LlmqCtx<'_, E>,
    llmq_type: LlmqType,
    start: BlockId,
    count: usize,
) -> Result<Option<BlockId>, LlmqError<E>> {
    if count == 0 || !is_quorum_type_enabled(ctx, llmq_type, Some(start))? {
        return Ok(None);
    }
    let params = ctx.llmq(llmq_type)?;
    let height = i64::from(ctx.height(start)?);
    let interval = i64::from(dkg_interval(params));
    let cycle_start = height - height % interval;
    let mining_start = cycle_start + i64::from(params.dkg_mining_window_start);
    let mining_end = cycle_start + i64::from(params.dkg_mining_window_end);
    let store = if height < mining_start {
        if mining_end < interval {
            return Ok(None);
        }
        mining_end - interval
    } else if height > mining_end {
        mining_end
    } else {
        return Ok(Some(start));
    };
    let store = u32::try_from(store).map_err(|_| MemberError::Missing(start))?;
    ctx.ancestor_below(start, store).map(Some)
}

/// The scan cache's answer for `hash(store)` (LQ1 §3.8.1 step 3).
#[derive(Clone, Copy, Debug)]
pub enum ScanCache<'a, Q> {
    /// v24's chain overload: no lookup and no write.
    Bypass,
    /// Looked up, absent.
    Miss,
    /// The cached quorums.
    Hit(&'a [Q]),
}

/// How a scan used its cache (the oracle's `SCAN` event).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanCacheUse {
    /// The chain overload.
    Bypass,
    /// A miss.
    Miss,
    /// A hit that satisfied the request.
    HitSatisfied,
    /// A hit continued by a scan.
    HitContinued,
    /// A continuation from a cached quorum whose base has no parent: Core returns `{}`.
    ContinuationBail,
}

/// What [`scan_quorums_from`] gives the facade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanOutcome<Q> {
    /// What Core returns.
    pub result: Vec<Q>,
    /// What Core emplaces under `hash(store)`, if anything.
    pub cache_write: Option<Vec<Q>>,
    /// How the cache was used.
    pub cache: ScanCacheUse,
}

impl<Q> ScanOutcome<Q> {
    fn empty(cache: ScanCacheUse) -> Self {
        ScanOutcome {
            result: Vec::new(),
            cache_write: None,
            cache,
        }
    }
}

/// Core's `ScanQuorums` from its cache lookup on (LQ1 §3.8.1 steps 3–6): the continuation, the
/// bases of the commitments mined up to the scan block, and `mat.get_quorum` for **every** new
/// base, in order, before the result is truncated to `count`. A missing quorum returns `{}` and
/// writes nothing; the builds before it keep their effects.
// Core: v24 llmq/quorumsman.cpp:171 CQuorumManager::ScanQuorums (from the cache lookup)
pub fn scan_quorums_from<R, M>(
    ctx: &LlmqCtx<'_, R::Error>,
    reader: &R,
    llmq_type: LlmqType,
    store: BlockId,
    count: usize,
    cache: ScanCache<'_, M::Quorum>,
    mat: &mut M,
) -> Result<ScanOutcome<M::Quorum>, LlmqError<R::Error>>
where
    R: MinedCommitmentReader + ?Sized,
    M: QuorumMaterializer<Error = R::Error>,
{
    let params = ctx.llmq(llmq_type)?;
    // `static_cast<size_t>(keepOldConnections)`: positive in every table.
    let keep = usize::try_from(params.keep_old_connections).unwrap_or(usize::MAX);
    let (mut result, from, scan, use_cache) = match cache {
        ScanCache::Bypass => (Vec::new(), store, count.max(keep), ScanCacheUse::Bypass),
        ScanCache::Miss => (Vec::new(), store, count.max(keep), ScanCacheUse::Miss),
        ScanCache::Hit(cached) if cached.len() >= count => {
            return Ok(ScanOutcome {
                result: cached[..count].to_vec(),
                cache_write: None,
                cache: ScanCacheUse::HitSatisfied,
            });
        }
        ScanCache::Hit(cached) => {
            let from = match cached.last() {
                None => store,
                Some(oldest) => match ctx.chain().prev(oldest.base()).map_err(MemberError::View)? {
                    None => return Ok(ScanOutcome::empty(ScanCacheUse::ContinuationBail)),
                    Some(prev) => prev,
                },
            };
            (cached.to_vec(), from, count - cached.len(), ScanCacheUse::HitContinued)
        }
    };

    let bases = if params.use_rotation {
        mined_indexed_until(reader, llmq_type, from, scan, params.signing_active_quorum_count)
    } else {
        reader.mined_until(llmq_type, from, scan)
    }
    .map_err(MemberError::View)?;
    result.reserve(bases.len());
    for base in bases {
        // Populate the object cache for the keepOldConnections most recent quorums only.
        let populate_cache = result.len() < keep;
        match mat.get_quorum(llmq_type, base, populate_cache)? {
            Some(quorum) => result.push(quorum),
            None => return Ok(ScanOutcome::empty(use_cache)),
        }
    }

    let cache_write = (use_cache != ScanCacheUse::Bypass && !result.is_empty())
        .then(|| result[..result.len().min(keep)].to_vec());
    result.truncate(count);
    Ok(ScanOutcome {
        result,
        cache_write,
        cache: use_cache,
    })
}

/// Which of Core's `ScanQuorums` overloads a scan is (OD-CP-3: the one production scan for both).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanOverload {
    /// `ScanQuorums(type, start, count)`: reads and writes the scan cache. What
    /// `SelectQuorumForSigning` and so every lock and CbTx ChainLock check calls.
    Cached,
    /// `ScanQuorums(type, start, count, chain)`: no cache read or write, and every quorum taken
    /// through the materializer bound to `chain` (Core's `GetQuorum` chain overload). The asset
    /// unlock check's scan.
    ChainBound,
}

/// Core's `scanQuorumsCache` (`unordered_lru_cache` per type, keyed by the store block's hash),
/// as [`scan_quorums`] reads and writes it. The node's quorum manager hosts it, sized per type by
/// [`scan_cache_size`]; a type with no cache misses and drops writes, as Core's
/// `PerLlmqTypeCache` does.
pub trait ScanCacheStore<Q> {
    /// `scanQuorumsCache.get(type, hash(store))`: a hit stamps the entry most recently used.
    fn get(&mut self, llmq_type: LlmqType, store: &BlockHash) -> Option<Vec<Q>>;
    /// `scanQuorumsCache.emplace(type, hash(store), quorums)`: an overwrite, stamped.
    fn insert(&mut self, llmq_type: LlmqType, store: BlockHash, quorums: Vec<Q>);
}

/// The scan cache's per-type size: `max_cycles(keepOldConnections) × (dkgMiningWindowEnd −
/// dkgMiningWindowStart)`, every block of the mining windows of the cycles kept.
// Core: v24.0.0-rc.4 llmq/quorumsman.cpp:211-214
pub fn scan_cache_size(llmq: &LlmqParams) -> i32 {
    llmq.max_cycles(llmq.keep_old_connections)
        * (llmq.dkg_mining_window_end - llmq.dkg_mining_window_start)
}

/// How a scan past its store block used the cache: the oracle's `SCAN` event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScanEvent {
    /// The store block, whose hash keys the cache.
    pub store: BlockId,
    /// The cache use.
    pub cache: ScanCacheUse,
    /// Whether the scan wrote the cache.
    pub cache_written: bool,
    /// The number of quorums the scan returned.
    pub n: usize,
}

/// What [`scan_quorums`] returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scanned<Q> {
    /// Core's result: at most `count` quorums, newest first.
    pub quorums: Vec<Q>,
    /// `None` where Core returns `{}` before its cache ([`scan_store_block`]'s `None`).
    pub event: Option<ScanEvent>,
}

/// Core's `ScanQuorums` (LQ1 §3.8.1), either overload: [`scan_store_block`], then for
/// [`ScanOverload::Cached`] the cache lookup under the store block's hash, [`scan_quorums_from`],
/// and the cache write it asks for. With [`ScanOverload::ChainBound`], `mat` must be bound to the
/// chain Core's call names; `cache` is not touched.
///
/// `reader` is read at the scan block, as Core's `GetMinedCommitments*UntilBlock`; `mat` decides
/// what Core's `GetQuorum` decides (mined in the chain, the object cache, the build), and the
/// effects of its builds stay made whatever the scan returns.
#[allow(clippy::too_many_arguments, reason = "Core's arguments and the three stores it reads")]
// Core: v24.0.0-rc.4 llmq/quorumsman.cpp:170-286 CQuorumManager::ScanQuorums, :155-168 overloads
pub fn scan_quorums<R, M, C>(
    ctx: &LlmqCtx<'_, R::Error>,
    reader: &R,
    cache: &mut C,
    mat: &mut M,
    llmq_type: LlmqType,
    start: BlockId,
    count: usize,
    overload: ScanOverload,
) -> Result<Scanned<M::Quorum>, LlmqError<R::Error>>
where
    R: MinedCommitmentReader + ?Sized,
    M: QuorumMaterializer<Error = R::Error>,
    C: ScanCacheStore<M::Quorum> + ?Sized,
{
    let Some(store) = scan_store_block(ctx, llmq_type, start, count)? else {
        return Ok(Scanned {
            quorums: Vec::new(),
            event: None,
        });
    };
    let (store_hash, cached) = match overload {
        ScanOverload::ChainBound => (None, None),
        ScanOverload::Cached => {
            let hash = ctx.hash(store)?;
            let cached = cache.get(llmq_type, &hash);
            (Some(hash), Some(cached))
        }
    };
    let lookup = match &cached {
        None => ScanCache::Bypass,
        Some(None) => ScanCache::Miss,
        Some(Some(quorums)) => ScanCache::Hit(quorums),
    };
    let outcome = scan_quorums_from(ctx, reader, llmq_type, store, count, lookup, mat)?;
    let cache_written = outcome.cache_write.is_some();
    if let (Some(hash), Some(write)) = (store_hash, outcome.cache_write) {
        cache.insert(llmq_type, hash, write);
    }
    Ok(Scanned {
        event: Some(ScanEvent {
            store,
            cache: outcome.cache,
            cache_written,
            n: outcome.result.len(),
        }),
        quorums: outcome.result,
    })
}

/// Core `GetMinedCommitmentsIndexedUntilBlock(type, at, max)`: round `r` takes the `r`-th newest
/// commitment of each quorum index, in index order, until a round finds none; the concatenation
/// is cut at `max`, inside a round if need be.
// Core: v24 llmq/blockprocessor.cpp:836 GetMinedCommitmentsIndexedUntilBlock, :819
// GetLastMinedCommitmentsPerQuorumIndexUntilBlock
fn mined_indexed_until<R: MinedCommitmentReader + ?Sized>(
    reader: &R,
    llmq_type: LlmqType,
    at: BlockId,
    max: usize,
    signing_active_quorum_count: i32,
) -> Result<Vec<BlockId>, R::Error> {
    let mut bases = Vec::new();
    let mut cycle = 0;
    while bases.len() < max {
        let mut round = Vec::new();
        for index in 0..signing_active_quorum_count {
            // Core passes the `int` index as an `int16_t` quorum index.
            let index = index as i16;
            if let Some(base) = reader.nth_mined_by_index(llmq_type, at, index, cycle)? {
                round.push(base);
            }
        }
        if round.is_empty() {
            break;
        }
        let take = (max - bases.len()).min(round.len());
        bases.extend_from_slice(&round[..take]);
        cycle += 1;
    }
    Ok(bases)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use dash_consensus::ChainAncestry;
    use dash_consensus::ctx::{Deployment, DeploymentView};
    use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
    use dash_types::Numeric;

    use super::*;
    use crate::bls::bls_ctx_after;
    use crate::llmq::chain::{BrokenInvariant, CompleteChain};
    use crate::llmq::mined::{MinedCommitment, MinedFamily, MinedIndexEntry};
    use crate::testkit::MemChain;

    const TEST: LlmqType = LlmqType::LlmqTest;
    const ROTATED: LlmqType = LlmqType::LlmqTestDip0024;
    const TIP: u32 = 120;

    /// Buried deployments at their heights on a linear chain.
    struct Buried<'a> {
        chain: &'a MemChain,
        params: &'a ChainParams,
    }

    impl DeploymentView for Buried<'_> {
        fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
            let next = prev.map_or(0, |p| self.chain.header(p).height + 1);
            match deployment {
                Deployment::Buried(d) => {
                    next >= self.params.consensus.deployment_height(d).to_u32()
                }
                Deployment::Versionbits(_) => false,
            }
        }
        fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
            self.active_after(self.chain.prev(block), deployment)
        }
    }

    /// Index rows `(family, mined height, base height)` on a linear chain.
    struct Reader<'a> {
        chain: &'a MemChain,
        rows: Vec<(MinedFamily, u32, u32)>,
    }

    impl MinedCommitmentReader for Reader<'_> {
        type Error = BrokenInvariant;
        fn chain(&self) -> &dyn ChainAncestry {
            self.chain
        }
        fn mined_commitment(
            &self,
            _: LlmqType,
            _: &QuorumHash,
            _: BlockId,
        ) -> Result<Option<MinedCommitment>, BrokenInvariant> {
            Ok(None)
        }
        fn mined_index(
            &self,
            _: LlmqType,
            family: MinedFamily,
            chain_end: BlockId,
            max_mined_height: u32,
            limit: usize,
        ) -> Result<Vec<MinedIndexEntry>, BrokenInvariant> {
            let cap = max_mined_height.min(self.chain.header(chain_end).height);
            let mut rows: Vec<MinedIndexEntry> = self
                .rows
                .iter()
                .filter(|(f, mined, _)| *f == family && (1..=cap).contains(mined))
                .map(|&(_, mined_height, base_height)| MinedIndexEntry {
                    mined_height,
                    base_height,
                })
                .collect();
            rows.sort_by_key(|row| std::cmp::Reverse(row.mined_height));
            rows.truncate(limit);
            Ok(rows)
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Q(BlockId);

    impl ScanEntry for Q {
        fn base(&self) -> BlockId {
            self.0
        }
        fn quorum_hash(&self) -> QuorumHash {
            QuorumHash::from([0; 32])
        }
        fn quorum_index(&self) -> i16 {
            0
        }
    }

    /// Records its calls; `None` for the bases in `missing`.
    #[derive(Default)]
    struct Mat {
        calls: Vec<(u32, bool)>,
        missing: BTreeSet<u32>,
    }

    struct Fixture {
        params: ChainParams,
        chain: MemChain,
        tip: BlockId,
    }

    impl Fixture {
        fn new() -> Fixture {
            let mut chain = MemChain::new();
            let tip = chain.extend(chain.genesis(), TIP);
            Fixture {
                params: ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
                    .build()
                    .expect("regtest"),
                chain,
                tip,
            }
        }
        fn at(&self, h: u32) -> BlockId {
            self.chain.ancestor(self.tip, h).expect("held")
        }
        fn h(&self, id: BlockId) -> u32 {
            self.chain.header(id).height
        }
        /// LLMQ_TEST commitments for the bases 24, 48, 72 and 96, mined at base + 10.
        fn reader(&self) -> Reader<'_> {
            Reader {
                chain: &self.chain,
                rows: [24, 48, 72, 96].map(|b| (MinedFamily::NonRotated, b + 10, b)).to_vec(),
            }
        }
        /// A scan of LLMQ_TEST from `store`, its bases as heights and the materializer's calls.
        fn scan(
            &self,
            store: u32,
            count: usize,
            cache: ScanCache<'_, Q>,
            mat: &mut Mat,
        ) -> ScanOutcome<Q> {
            let view = CompleteChain::<BrokenInvariant>::new(&self.chain);
            let deployments = Buried {
                chain: &self.chain,
                params: &self.params,
            };
            let ctx = LlmqCtx::new(
                &self.params,
                &view,
                &deployments,
                bls_ctx_after(&deployments, Some(self.tip)),
            )
            .expect("in domain");
            let reader = self.reader();
            let mut mat = Adapter {
                fixture: self,
                mat,
            };
            scan_quorums_from(&ctx, &reader, TEST, self.at(store), count, cache, &mut mat)
                .expect("held")
        }
        fn heights(&self, qs: &[Q]) -> Vec<u32> {
            qs.iter().map(|q| self.h(q.0)).collect()
        }
    }

    struct Adapter<'a> {
        fixture: &'a Fixture,
        mat: &'a mut Mat,
    }

    impl QuorumMaterializer for Adapter<'_> {
        type Quorum = Q;
        type Error = BrokenInvariant;
        fn get_quorum(
            &mut self,
            _: LlmqType,
            base: BlockId,
            populate_cache: bool,
        ) -> Result<Option<Q>, LlmqError<BrokenInvariant>> {
            let h = self.fixture.h(base);
            self.mat.calls.push((h, populate_cache));
            Ok((!self.mat.missing.contains(&h)).then_some(Q(base)))
        }
    }

    #[test]
    fn the_store_block_follows_the_mining_window() {
        let f = Fixture::new();
        let view = CompleteChain::<BrokenInvariant>::new(&f.chain);
        let deployments = Buried {
            chain: &f.chain,
            params: &f.params,
        };
        let ctx =
            LlmqCtx::new(&f.params, &view, &deployments, bls_ctx_after(&deployments, Some(f.tip)))
                .expect("in domain");
        // LLMQ_TEST: interval 24, mining window 10..=18.
        let store = |start, count| {
            scan_store_block(&ctx, TEST, f.at(start), count).expect("held").map(|s| f.h(s))
        };
        assert_eq!(store(30, 2), Some(18), "before the window: the previous cycle's window end");
        assert_eq!(store(34, 2), Some(34), "in the window: the start");
        assert_eq!(store(42, 2), Some(42));
        assert_eq!(store(43, 2), Some(42), "after the window: its end");
        assert_eq!(store(47, 2), Some(42));
        assert_eq!(store(5, 2), None, "the previous window is below genesis");
        assert_eq!(store(30, 0), None, "a count of 0");
        let unconfigured = scan_store_block(&ctx, LlmqType::LlmqDevnet, f.at(30), 2);
        assert_eq!(unconfigured, Err(MemberError::UnknownLlmqType(LlmqType::LlmqDevnet.to_base())));
    }

    #[test]
    fn a_miss_scans_keep_old_connections_and_caches_them() {
        let f = Fixture::new();
        let mut mat = Mat::default();
        let out = f.scan(114, 2, ScanCache::Miss, &mut mat);
        assert_eq!(out.cache, ScanCacheUse::Miss);
        assert_eq!(f.heights(&out.result), [96, 72]);
        assert_eq!(f.heights(&out.cache_write.expect("written")), [96, 72, 48]);
        assert_eq!(mat.calls, [(96, true), (72, true), (48, true)], "every base is built");

        // More than keepOldConnections: the cache keeps 3 and the rest are built unpopulated.
        let mut mat = Mat::default();
        let out = f.scan(114, 5, ScanCache::Miss, &mut mat);
        assert_eq!(f.heights(&out.result), [96, 72, 48, 24]);
        assert_eq!(f.heights(&out.cache_write.expect("written")), [96, 72, 48]);
        assert_eq!(mat.calls, [(96, true), (72, true), (48, true), (24, false)]);

        // The chain overload scans the same and writes nothing.
        let mut mat = Mat::default();
        let out = f.scan(114, 2, ScanCache::Bypass, &mut mat);
        assert_eq!((out.cache, out.cache_write), (ScanCacheUse::Bypass, None));
        assert_eq!(f.heights(&out.result), [96, 72]);
        assert_eq!(mat.calls.len(), 3);

        // Nothing mined at or below the store block: an empty result, no write.
        let mut mat = Mat::default();
        let out = f.scan(18, 2, ScanCache::Miss, &mut mat);
        assert_eq!((out.result, out.cache_write), (vec![], None));
        assert!(mat.calls.is_empty());
    }

    #[test]
    fn a_hit_is_cut_or_continued_below_its_oldest_quorum() {
        let f = Fixture::new();
        let cached = [Q(f.at(96)), Q(f.at(72)), Q(f.at(48))];

        let mut mat = Mat::default();
        let out = f.scan(114, 2, ScanCache::Hit(&cached), &mut mat);
        assert_eq!(out.cache, ScanCacheUse::HitSatisfied);
        assert_eq!((f.heights(&out.result), out.cache_write), (vec![96, 72], None));
        assert!(mat.calls.is_empty());

        // Two cached, three wanted: one more from the parent of the oldest base; the
        // continuation's population counts the cached quorums.
        let mut mat = Mat::default();
        let out = f.scan(114, 3, ScanCache::Hit(&cached[..2]), &mut mat);
        assert_eq!(out.cache, ScanCacheUse::HitContinued);
        assert_eq!(f.heights(&out.result), [96, 72, 48]);
        assert_eq!(f.heights(&out.cache_write.expect("written")), [96, 72, 48]);
        assert_eq!(mat.calls, [(48, true)]);

        let mut mat = Mat::default();
        let out = f.scan(114, 4, ScanCache::Hit(&cached), &mut mat);
        assert_eq!(f.heights(&out.result), [96, 72, 48, 24]);
        assert_eq!(f.heights(&out.cache_write.expect("written")), [96, 72, 48]);
        assert_eq!(mat.calls, [(24, false)]);

        // An empty cached list continues from the store block.
        let mut mat = Mat::default();
        let out = f.scan(114, 2, ScanCache::Hit(&[]), &mut mat);
        assert_eq!(out.cache, ScanCacheUse::HitContinued);
        assert_eq!(f.heights(&out.result), [96, 72]);
        assert_eq!(f.heights(&out.cache_write.expect("written")), [96, 72]);

        // An oldest base at genesis: no parent, Core returns `{}`.
        let mut mat = Mat::default();
        let genesis = [Q(f.chain.genesis())];
        let out = f.scan(114, 2, ScanCache::Hit(&genesis), &mut mat);
        assert_eq!(out, ScanOutcome::empty(ScanCacheUse::ContinuationBail));
        assert!(mat.calls.is_empty());
    }

    #[test]
    fn a_missing_quorum_returns_nothing_and_keeps_earlier_builds() {
        let f = Fixture::new();
        let mut mat = Mat {
            missing: [72].into(),
            ..Mat::default()
        };
        let out = f.scan(114, 2, ScanCache::Miss, &mut mat);
        assert_eq!(out, ScanOutcome::empty(ScanCacheUse::Miss));
        assert_eq!(mat.calls, [(96, true), (72, true)], "stops at the missing one");
    }

    #[test]
    fn rotated_bases_come_in_rounds_by_quorum_index() {
        let f = Fixture::new();
        let rotated = |i| MinedFamily::Rotated {
            quorum_index: i,
        };
        // Index 0 in the cycles at 24 and 48, index 1 only at 48 (base 49).
        let reader = Reader {
            chain: &f.chain,
            rows: vec![(rotated(0), 36, 24), (rotated(0), 60, 48), (rotated(1), 61, 49)],
        };
        let bases = |max| {
            let bases = mined_indexed_until(&reader, ROTATED, f.at(TIP), max, 2).expect("held");
            f.heights(&bases.into_iter().map(Q).collect::<Vec<_>>())
        };
        assert_eq!(bases(10), [48, 49, 24]);
        assert_eq!(bases(3), [48, 49, 24]);
        assert_eq!(bases(1), [48], "cut inside a round");
        assert_eq!(bases(0), Vec::<u32>::new());
    }
}
