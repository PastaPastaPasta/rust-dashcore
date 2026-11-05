//! The quorum object and its caches (CH7 §4.4, D13; DEC-60): Core's `CQuorum` as validation reads
//! it, `CQuorumManager`'s two caches under one mutex, and `GetQuorum`'s chain overload over a
//! capability, which builds a missing object through the facade's members call. QM1a's
//! contributions and warming go through [`QuorumBuildHook`], which reads no validation answer.
//! Route C's lookup for types 7 and 9 is [`AttemptQuorums`] (D4, D92).

use std::ops::Deref;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_dml::blockprocessor::BlockLookup;
use dash_dml::specialtx::quorum::{
    MAX_QUORUM_SCAN, QuorumKey, QuorumLookup, QuorumScan, SeamError,
};
use dash_dml::{Dmn, NodeMemberError, StoreError};
use dash_evo::EffectSink;
use dash_evo::llmq::{
    LlmqCommitment, LlmqCtx, LlmqError, LlmqSource, MemberError, MinedCommitmentReader, MnListView,
    ParamsOutOfDomain, QuorumMaterializer, QuorumMembers, ScanCacheStore, ScanEntry, ScanOverload,
    scan_cache_size, scan_quorums,
};
use dash_params::ChainSetup;
use dash_params::LlmqType;
use dash_pkc::bls::BlsLazyPk;
use dash_primitives::{BlockHash, QuorumHash};

use super::cache::PerTypeLru;
use super::capability::Capability;
use super::facade::MembersFacade;
use super::members::{hash, members};

/// Core's `CQuorum` as validation reads it (`llmq/quorums.h:175-178`): the commitment, its base
/// block, the block that mined it, and the members the facade gave at the build.
#[derive(Debug)]
pub struct NodeQuorum {
    pub llmq_type: LlmqType,
    /// `qc`.
    pub commitment: Arc<LlmqCommitment>,
    /// `m_quorum_base_block_index`.
    pub base: BlockId,
    pub base_hash: BlockHash,
    /// `minedBlockHash`.
    pub mined_block: BlockHash,
    /// `members`.
    pub members: QuorumMembers<Arc<Dmn>>,
}

/// Core's `CQuorumCPtr`: one quorum object, shared by the caches and every caller holding it.
#[derive(Clone, Debug)]
pub struct QuorumPtr(pub Arc<NodeQuorum>);

impl Deref for QuorumPtr {
    type Target = NodeQuorum;

    fn deref(&self) -> &NodeQuorum {
        &self.0
    }
}

impl ScanEntry for QuorumPtr {
    fn base(&self) -> BlockId {
        self.base
    }

    fn quorum_hash(&self) -> QuorumHash {
        self.commitment.quorum_hash
    }

    fn quorum_index(&self) -> i16 {
        self.commitment.quorum_index.unwrap_or(0)
    }
}

/// `CQuorumManager`'s caches (`llmq/quorumsman.h:88-89`) under one mutex, Core's `m_cs_maps`
/// (`:85`). Built with the facade, where Core initializes them on first use (D79, D90).
pub(crate) struct QuorumCaches {
    /// `mapQuorumsCache`, each type's sized `keepOldKeys` (`quorumsman.cpp:42`).
    pub(crate) objects: PerTypeLru<BlockHash, QuorumPtr>,
    /// `scanQuorumsCache`, keyed by the scan's store block (`quorumsman.cpp:211-213`).
    pub(crate) scans: PerTypeLru<BlockHash, Vec<QuorumPtr>>,
}

impl QuorumCaches {
    /// Cold caches for `params`' configured types.
    ///
    /// # Errors
    ///
    /// A configured type whose cache size is below one.
    pub(crate) fn new(params: &ChainSetup) -> Result<Self, ParamsOutOfDomain> {
        Ok(QuorumCaches {
            objects: PerTypeLru::new(params, false)?,
            scans: PerTypeLru::sized(params, |llmq| i64::from(scan_cache_size(llmq)))?,
        })
    }
}

/// D13's hook (D89): what QM1a adds to a build after the members (`quorumsman.cpp:83-98`, the
/// contributions and the cache-populator thread). It reads no validation answer.
pub trait QuorumBuildHook {
    fn built(&self, quorum: &QuorumPtr, populate_cache: bool);
}

/// The hook until QM1a: nothing.
pub struct NoQuorumHook;

impl QuorumBuildHook for NoQuorumHook {
    fn built(&self, _: &QuorumPtr, _: bool) {}
}

/// Core's `GetQuorum(type, pindex, chain, populate_cache)` over one capability (route B's `OnV`
/// or route C's attempt), bound to the chain ending at `tip`: `mined` reads the mined rows
/// through that chain, and a build's members call goes through `cap`, its effects to `effects`.
pub(crate) struct Materializer<'m, 'c, C: Capability + ?Sized, L> {
    pub(crate) cap: &'m C,
    pub(crate) ctx: &'m LlmqCtx<'c, StoreError>,
    pub(crate) src: &'m dyn LlmqSource<L, Error = StoreError>,
    pub(crate) mined: &'m dyn MinedCommitmentReader<Error = StoreError>,
    pub(crate) tip: BlockId,
    pub(crate) effects: &'m mut dyn EffectSink,
    pub(crate) hook: &'m dyn QuorumBuildHook,
}

impl<C: Capability + ?Sized, L: MnListView<Entry = Arc<Dmn>>> QuorumMaterializer
    for Materializer<'_, '_, C, L>
{
    type Quorum = QuorumPtr;
    type Error = StoreError;

    /// `HasQuorum` through the bound chain, the cache, else `BuildQuorumFromCommitment`. The
    /// view is one snapshot, so the build's `GetMinedCommitment` answers what `HasQuorum` read,
    /// and the record read once serves both. The caches' mutex is taken for the lookup and again
    /// for the insert, never across the members call (`:530-531`, `WITH_LOCK` at `:101`).
    // Core: v24 llmq/quorumsman.cpp:539 GetQuorum (chain overload), :58 BuildQuorumFromCommitment
    fn get_quorum(
        &mut self,
        llmq_type: LlmqType,
        base: BlockId,
        populate_cache: bool,
    ) -> Result<Option<QuorumPtr>, LlmqError<StoreError>> {
        let base_hash = hash(self.ctx, base)?;
        let quorum_hash = QuorumHash::from(*base_hash.as_bytes());
        // :546, `HasMinedCommitment(chain)` (`blockprocessor.cpp:650-673`); :62-66
        let mined = self.mined.mined_commitment(llmq_type, &quorum_hash, self.tip);
        let Some((commitment, mined_block)) = mined.map_err(MemberError::View)? else {
            return Ok(None);
        };
        let facade = self.cap.facade();
        // :551
        if let Some(quorum) = facade.quorums().objects.get(llmq_type, &base_hash) {
            return Ok(Some(quorum.clone()));
        }
        let params = self.ctx.llmq(llmq_type)?;
        let made_up = self.src.made_up_lists();
        // :71
        let members = members(self.cap, self.ctx, self.src, self.effects, llmq_type, base, false)?;
        let quorum = QuorumPtr(Arc::new(NodeQuorum {
            llmq_type,
            commitment,
            base,
            base_hash,
            mined_block,
            members,
        }));
        // :76-80: a size-1 quorum the caller caches has no contributions.
        if !(populate_cache && params.size == 1) {
            // :83-98
            self.hook.built(&quorum, populate_cache);
        }
        // :101 (and :78); a quorum whose members stood on a made-up list stays out of the shared
        // cache, as its members do (OD-CP-51).
        if self.src.made_up_lists() == made_up {
            facade.quorums().objects.insert(llmq_type, base_hash, quorum.clone());
        }
        Ok(Some(quorum))
    }
}

/// `scanQuorumsCache` as [`dash_evo::llmq::scan_quorums`]' cached overload reads and writes it:
/// the mutex per access (`quorumsman.cpp:204`, `:275`), so no materializer call runs under it.
/// A scan during which the source made up a list writes nothing (OD-CP-51).
pub(crate) struct FacadeScans<'f> {
    facade: &'f MembersFacade,
    made_up: &'f dyn Fn() -> u64,
    at_start: u64,
}

impl<'f> FacadeScans<'f> {
    /// The scan cache of the facade whose gate `cap` holds, for one scan over a source whose
    /// [`made_up_lists`](dash_evo::llmq::LlmqSource::made_up_lists) `made_up` reads.
    pub(crate) fn new<C: Capability + ?Sized>(cap: &'f C, made_up: &'f dyn Fn() -> u64) -> Self {
        FacadeScans {
            facade: cap.facade(),
            made_up,
            at_start: made_up(),
        }
    }
}

impl ScanCacheStore<QuorumPtr> for FacadeScans<'_> {
    // Core: v24 llmq/quorumsman.cpp:215
    fn get(&mut self, llmq_type: LlmqType, store: &BlockHash) -> Option<Vec<QuorumPtr>> {
        self.facade.quorums().scans.get(llmq_type, store).cloned()
    }

    // Core: v24 llmq/quorumsman.cpp:280-281
    fn insert(&mut self, llmq_type: LlmqType, store: BlockHash, quorums: Vec<QuorumPtr>) {
        if (self.made_up)() == self.at_start {
            self.facade.quorums().scans.insert(llmq_type, store, quorums);
        }
    }
}

/// The chain overload's scan cache: Core never reads or writes `scanQuorumsCache` there
/// (`quorumsman.cpp:237-239`, `:274`), so a call is a bug.
struct NoScanCache;

impl ScanCacheStore<QuorumPtr> for NoScanCache {
    fn get(&mut self, _: LlmqType, _: &BlockHash) -> Option<Vec<QuorumPtr>> {
        unreachable!("a chain-bound scan reads no scan cache")
    }

    fn insert(&mut self, _: LlmqType, _: BlockHash, _: Vec<QuorumPtr>) {
        unreachable!("a chain-bound scan writes no scan cache")
    }
}

/// Route C's quorum manager for types 7 and 9 (D4, D92): the overloads `CheckSpecialTxInner`
/// calls with the active chain while a block connects (`evo/specialtxman.cpp:260`, `:265`),
/// `GetQuorum(type, hash, chain)` and `ScanQuorums(type, start, n, chain)`, over a
/// [`Materializer`] bound to the attempt's capability and to the chain ending at `tip`. A build's
/// effects go to the sink of the call.
pub(crate) struct AttemptQuorums<'m, 'c, C: Capability + ?Sized, L> {
    pub(crate) cap: &'m C,
    pub(crate) ctx: &'m LlmqCtx<'c, StoreError>,
    pub(crate) src: &'m dyn LlmqSource<L, Error = StoreError>,
    pub(crate) mined: &'m dyn MinedCommitmentReader<Error = StoreError>,
    /// `LookupBlockIndex`: any known header, whatever its chain.
    pub(crate) index: &'m dyn BlockLookup,
    /// The bound chain's end: `pindexPrev`, the active chain's tip while the block connects.
    pub(crate) tip: BlockId,
    pub(crate) hook: &'m dyn QuorumBuildHook,
}

impl<'c, C: Capability + ?Sized, L: MnListView<Entry = Arc<Dmn>>> AttemptQuorums<'_, 'c, C, L> {
    /// The materializer over the bound chain, its effects to `effects`.
    fn materializer<'s>(&'s self, effects: &'s mut dyn EffectSink) -> Materializer<'s, 'c, C, L> {
        Materializer {
            cap: self.cap,
            ctx: self.ctx,
            src: self.src,
            mined: self.mined,
            tip: self.tip,
            effects,
            hook: self.hook,
        }
    }
}

impl<C: Capability + ?Sized, L: MnListView<Entry = Arc<Dmn>>> QuorumLookup
    for AttemptQuorums<'_, '_, C, L>
{
    /// `LookupBlockIndex` (`:510`), then the chain overload of `GetQuorum` with `populate_cache`
    /// (`:515`); the key is the commitment's `quorumPublicKey`.
    // Core: v24 llmq/quorumsman.cpp:505 GetQuorum (hash, chain)
    fn get_quorum(
        &mut self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        fx: &mut dyn EffectSink,
    ) -> Result<Option<QuorumKey>, SeamError> {
        let Some((base, _)) = self.index.lookup(&BlockHash::from(*quorum_hash.as_bytes())) else {
            return Ok(None);
        };
        let quorum = self.materializer(fx).get_quorum(llmq_type, base, true);
        Ok(quorum.map_err(seam_error)?.map(|q| {
            let key = &q.commitment.quorum_public_key;
            QuorumKey {
                quorum_hash: q.commitment.quorum_hash,
                public_key: BlsLazyPk::from_value(key.value().clone(), key.scheme()),
            }
        }))
    }

    /// The chain overload: no scan-cache read or write, and each quorum through the chain-bound
    /// materializer (`:237-239`, `:259`, `:274`; OD-CP-3).
    // Core: v24 llmq/quorumsman.cpp:162 ScanQuorums (chain)
    fn scan_quorums(
        &mut self,
        llmq_type: LlmqType,
        at: BlockId,
        n: usize,
        out: &mut QuorumScan,
        fx: &mut dyn EffectSink,
    ) -> Result<(), SeamError> {
        if n > MAX_QUORUM_SCAN {
            return Err(SeamError::ScanCapacity);
        }
        out.clear();
        let overload = ScanOverload::ChainBound;
        let mut mat = self.materializer(fx);
        let scanned = scan_quorums(
            self.ctx,
            self.mined,
            &mut NoScanCache,
            &mut mat,
            llmq_type,
            at,
            n,
            overload,
        )
        .map_err(seam_error)?;
        for quorum in &scanned.quorums {
            out.push(quorum.commitment.quorum_hash)?;
        }
        Ok(())
    }
}

/// A seam's fault, which H4 reports once (D5). `IsQuorumTypeEnabled`'s throw cannot reach here:
/// types 7 and 9 look up the chain's own `llmqTypeMnhf` and `llmqTypePlatform`.
fn seam_error(e: LlmqError<StoreError>) -> SeamError {
    match NodeMemberError::from(e) {
        NodeMemberError::Store(e) => SeamError::Store(e),
        NodeMemberError::UnknownLlmqType(t) => {
            SeamError::Store(StoreError::inconsistent(format!("a quorum lookup of LLMQ type {t}")))
        }
    }
}
