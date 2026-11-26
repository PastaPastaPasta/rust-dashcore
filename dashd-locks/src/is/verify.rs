//! Verification of InstantSend locks against the chain's quorums, and verified-lock ingestion
//! (LK1b-s S1c, quiescent).
//!
//! [`NodeQuorumView`] answers dash-evo's [`QuorumScanView`] from V's state in one `OnV` turn, and
//! [`IsIngest::ingest_verified`] is the O2b `ISLOCK` entry: parse, the two-pass
//! [`verify_islock`], then the lock's rows. Ingestion runs only while the InstantSend tip cache
//! equals the view's active tip ([`IngestError::NotQuiescent`]). A view given the cache
//! ([`NodeQuorumView::with_is_tip`]) answers [`QuorumScanView::is_tip_height`] from it, Core's
//! transient split of the two tips (S3.0).

use std::fmt;

use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::{BlockId, BlsCtx};
use dash_evo::llmq::cycle_base_height;
use dash_evo::locks::{
    IslockPassSchemes, IslockSchemes, LockError, QuorumScan, QuorumScanView, QuorumStatus,
    ScanLookup, ScannedQuorum, isdlock_hash, verify_islock,
};
use dash_params::{BuriedDeployment, ChainSetup, LlmqParams, LlmqType};
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_chainstate::chain::{ChainQuery, OnV, OnVBinding, OnVMembersError};
use dashd_chainstate::llmq::quorum::QuorumPtr;

use super::seams::IsStore;
use super::{IsLockHash, IsTipCache, LockedTx};

/// Why [`NodeQuorumView`] gave no answer.
#[derive(Debug)]
pub enum NodeViewError {
    /// A route-B quorum read ([`OnVBinding`]) failed.
    Members(OnVMembersError),
    /// A header read failed; the chain's fail-stop latch has tripped.
    Chain(String),
    /// V's chain has no block where a read needs one.
    Inconsistent(String),
}

impl fmt::Display for NodeViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeViewError::Members(e) => write!(f, "quorum read: {e}"),
            NodeViewError::Chain(e) => write!(f, "header read: {e}"),
            NodeViewError::Inconsistent(e) => write!(f, "quorum view: {e}"),
        }
    }
}

impl std::error::Error for NodeViewError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            NodeViewError::Members(e) => Some(e),
            NodeViewError::Chain(_) | NodeViewError::Inconsistent(_) => None,
        }
    }
}

impl From<OnVMembersError> for NodeViewError {
    fn from(e: OnVMembersError) -> Self {
        NodeViewError::Members(e)
    }
}

fn chain_error(e: impl fmt::Display) -> NodeViewError {
    NodeViewError::Chain(e.to_string())
}

/// The node's [`QuorumScanView`]: V's state in one `OnV` turn, through
/// [`ChainQuery::bind`]. Built per call; valid for that turn only.
///
/// - [`Self::scan`] is `OnVBinding::scan_quorums` at the active chain's block at the start
///   height, with `signingActiveQuorumCount`, as `SelectQuorumForSigning` scans;
/// - [`Self::quorum`] is `OnVBinding::quorum`, Core's `GetQuorum(type, quorumHash)`;
/// - [`Self::block_height`] is any header V knows, on the active chain or not;
/// - [`Self::tip_height`] is V's active tip;
/// - [`Self::is_tip_height`] is the [`IsTipCache`] given by [`Self::with_is_tip`], else V's
///   active tip.
///
/// Each scanned or named quorum carries its mined commitment's key, unchecked, as Core's
/// `BuildQuorumFromCommitment` keeps it ([`ScannedQuorum::mined`]): the rows are V's own
/// validation's.
// Core: llmq/quorumsman.cpp:741-811 (SelectQuorumForSigningStartBlock, SelectQuorumForSigning),
// :158-162 and :179-295 (ScanQuorums), :491-525 (GetQuorum)
pub struct NodeQuorumView<'a> {
    binding: OnVBinding<'a>,
    query: &'a ChainQuery<'a>,
    params: &'a ChainSetup,
    /// V's active tip and its height; `None` before V has a tip.
    tip: Option<(BlockId, u32)>,
    /// The InstantSend manager's cached tip, when given.
    is_tip: Option<&'a IsTipCache>,
}

impl<'a> NodeQuorumView<'a> {
    /// The view over `query` in `on_v`'s turn, for the chain `params`.
    ///
    /// # Errors
    ///
    /// The tip's header could not be read.
    pub fn new(
        on_v: &'a OnV<'a>,
        query: &'a ChainQuery<'a>,
        params: &'a ChainSetup,
    ) -> Result<Self, NodeViewError> {
        let tip = match query.evo_view().tip {
            None => None,
            Some(tip) => {
                let entry = query.chain.reader().try_entry(tip).map_err(chain_error)?;
                let entry = entry.ok_or_else(|| {
                    NodeViewError::Inconsistent(format!("V's tip {tip:?} has no header"))
                })?;
                Some((tip, entry.height))
            }
        };
        Ok(NodeQuorumView {
            binding: query.bind(on_v),
            query,
            params,
            tip,
            is_tip: None,
        })
    }

    /// The view whose [`QuorumScanView::is_tip_height`] reads `cache`, the InstantSend manager's
    /// cached tip.
    pub fn with_is_tip(self, cache: &'a IsTipCache) -> Self {
        NodeQuorumView {
            is_tip: Some(cache),
            ..self
        }
    }

    /// Core's process flag `bls::bls_legacy_scheme` in this turn: V's published scheme.
    pub fn bls(&self) -> BlsCtx {
        self.query.bls()
    }

    fn llmq(&self, llmq_type: LlmqType) -> Result<&LlmqParams, NodeViewError> {
        self.params.consensus.llmq(llmq_type).ok_or_else(|| {
            NodeViewError::Inconsistent(format!(
                "LLMQ type {} is not configured",
                llmq_type.to_base()
            ))
        })
    }

    /// The active chain's block at `height`, at most the tip.
    fn active_at(&self, tip: BlockId, height: u32) -> Result<BlockId, NodeViewError> {
        let reader = self.query.chain.reader();
        reader.try_ancestor(tip, height).map_err(chain_error)?.ok_or_else(|| {
            NodeViewError::Inconsistent(format!("no block at {height} on V's active chain"))
        })
    }

    /// Core's `IsQuorumRotationEnabled(llmqParams, pindex)`: DIP0024 active at least one block
    /// before `pindex`'s cycle start.
    // Core: llmq/options.cpp:50-62
    fn rotation_enabled(
        &self,
        llmq: &LlmqParams,
        block: BlockId,
        height: u32,
    ) -> Result<bool, NodeViewError> {
        if !llmq.use_rotation {
            return Ok(false);
        }
        let cycle_base = cycle_base_height(llmq, height);
        if cycle_base < 1 {
            return Ok(false);
        }
        let before = self.active_at(block, cycle_base - 1)?;
        let dip0024 = Deployment::Buried(BuriedDeployment::Dip0024);
        Ok(self.query.deployments.active_after(Some(before), dip0024))
    }

    /// `ScanQuorums(type, start, count)` with the rotation predicate at `start`.
    fn scan_from(
        &self,
        llmq_type: LlmqType,
        start: BlockId,
        start_height: u32,
        count: usize,
    ) -> Result<ScanLookup, NodeViewError> {
        let llmq = self.llmq(llmq_type)?;
        let rotation_enabled = self.rotation_enabled(llmq, start, start_height)?;
        let quorums = self.binding.scan_quorums(llmq_type, start, count)?;
        Ok(ScanLookup::Held(QuorumScan {
            rotation_enabled,
            entries: quorums.iter().map(scanned).collect(),
        }))
    }
}

fn scanned(q: &QuorumPtr) -> ScannedQuorum {
    ScannedQuorum::mined(q.base, &q.commitment)
}

impl QuorumScanView for NodeQuorumView<'_> {
    type Error = NodeViewError;

    /// V's active tip (`active_chain.Height()`). Before V has a tip it reads 0, as
    /// [`IsTipCache`] does; every scan is then empty and every named quorum absent, so a lock
    /// finds no quorum, as in Core (`quorumsman.cpp:741-752`, `blockprocessor.cpp:644-647`).
    fn tip_height(&self) -> u32 {
        self.tip.map_or(0, |(_, h)| h)
    }

    /// The [`IsTipCache`] (`m_is_manager.GetTipHeight()`, `net_instantsend.cpp:138`), which is
    /// never negative; V's active tip when no cache was given.
    fn is_tip_height(&self) -> u32 {
        self.is_tip.map_or_else(|| self.tip_height(), |c| u32::try_from(c.get()).unwrap_or(0))
    }

    fn scan(&self, llmq_type: LlmqType, start_height: u32) -> Result<ScanLookup, NodeViewError> {
        let Some((tip, tip_height)) = self.tip else {
            return Ok(ScanLookup::Held(QuorumScan::default()));
        };
        if start_height > tip_height {
            return Err(NodeViewError::Inconsistent(format!(
                "a scan at {start_height} above the tip at {tip_height}"
            )));
        }
        let start = self.active_at(tip, start_height)?;
        let count = usize::from(self.llmq(llmq_type)?.signing_active_quorum_count);
        self.scan_from(llmq_type, start, start_height, count)
    }

    // Core: llmq/quorumsman.cpp:158-162
    fn scan_at_tip(&self, llmq_type: LlmqType, count: usize) -> Result<ScanLookup, NodeViewError> {
        let Some((tip, tip_height)) = self.tip else {
            return Ok(ScanLookup::Held(QuorumScan::default()));
        };
        self.scan_from(llmq_type, tip, tip_height, count)
    }

    /// Core's `GetBlockHeight(hash)`: none for the null hash, else the IS manager's height
    /// cache, else `LookupBlockIndex`, any header the node knows. A header's height never
    /// changes and the block index never forgets a header, so the cache gives what the lookup
    /// would, and the view reads the index alone.
    // Core: instantsend/net_instantsend.cpp:40-55, :74-88 (ResolveCycleHeight)
    fn block_height(&self, hash: &BlockHash) -> Result<Option<u32>, NodeViewError> {
        if hash.as_bytes() == &[0u8; 32] {
            return Ok(None);
        }
        let reader = self.query.chain.reader();
        let Some(id) = reader.try_lookup(hash).map_err(chain_error)? else {
            return Ok(None);
        };
        let entry = reader.try_entry(id).map_err(chain_error)?;
        let entry = entry.ok_or_else(|| {
            NodeViewError::Inconsistent(format!("header {id:?} is indexed with no entry"))
        })?;
        Ok(Some(entry.height))
    }

    fn quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<Option<QuorumStatus>, NodeViewError> {
        let quorum = self.binding.quorum(llmq_type, quorum_hash)?;
        Ok(quorum.map(|q| scanned(&q).status().clone()))
    }
}

/// Core's process flag `bls::bls_legacy_scheme`, read afresh at each sample.
pub trait SchemeFlag {
    /// The flag now.
    fn sample(&self) -> BlsCtx;
}

impl<F: Fn() -> BlsCtx> SchemeFlag for F {
    fn sample(&self) -> BlsCtx {
        self()
    }
}

impl SchemeFlag for NodeQuorumView<'_> {
    fn sample(&self) -> BlsCtx {
        self.bls()
    }
}

/// Why [`IsIngest::ingest_verified`] wrote nothing, or did not finish writing.
#[derive(Debug)]
pub enum IngestError<E> {
    /// The InstantSend tip cache is not the view's active tip: the collapsed tip of
    /// [`QuorumScanView::tip_height`] would not be Core's sign-height tip.
    NotQuiescent {
        /// [`IsTipCache::get`].
        cached: i32,
        /// The view's active tip.
        active: u32,
    },
    /// The bytes are not an `isdlock`.
    Parse(String),
    /// The mined height is not a Core `int`.
    MinedHeight(u32),
    /// The lock did not verify: a verdict, or no verdict.
    Verify(LockError<E>),
    /// A store write failed. If `write_mined` failed, the lock row was written.
    Store(dash_kv::StoreError),
}

impl<E: fmt::Display> fmt::Display for IngestError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IngestError::NotQuiescent {
                cached,
                active,
            } => write!(f, "not quiescent: the IS tip cache is {cached}, the active tip {active}"),
            IngestError::Parse(e) => write!(f, "isdlock: {e}"),
            IngestError::MinedHeight(h) => write!(f, "mined height {h} is out of range"),
            IngestError::Verify(e) => write!(f, "{e}"),
            IngestError::Store(e) => write!(f, "InstantSend store: {e}"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for IngestError<E> {}

/// Verified-lock ingestion into the InstantSend store.
pub struct IsIngest<'s, S: IsStore + ?Sized> {
    pub params: &'s ChainSetup,
    pub store: &'s S,
    pub tip_cache: &'s IsTipCache,
}

impl<S: IsStore + ?Sized> IsIngest<'_, S> {
    /// The O2b `ISLOCK` entry, quiescent: the `isdlock` in `bytes` is read, verified against
    /// `view` with [`verify_islock`] and written as Core's `WriteNewISLock` writes it, with its
    /// mined mark when `mined_height` is given (the height Core's `GetTransaction` would give).
    ///
    /// `flag` is sampled three times, in Core's order: when the message is read (the lazy
    /// signature keeps it), then at the start of the current set's pass, then at the start of
    /// the previous set's (LQ3a D21/D22). Ingestion runs between V's operations, so V cannot
    /// connect a block between the samples, and taking all three before the check gives each
    /// pass the flag it would read at its start.
    ///
    /// Trailing bytes after the lock are ignored, as Core's `vRecv >> *islock` ignores them.
    ///
    /// The checks it does not make are its caller's (S1d's NodeSut op, S4): no spork-2 gate, no
    /// known or archived lock dedup, and a second lock for a txid overwrites its `is_tx` row. This
    /// matches O2b's Core hook, which writes through `WriteNewInstantSendLock` directly.
    ///
    /// # Errors
    ///
    /// [`IngestError::NotQuiescent`] before anything is read or written; a parse or
    /// verification failure, with nothing written; a store write's failure.
    // Core: instantsend/net_instantsend.cpp:238-300 (ProcessMessage), :104-160
    // (BuildVerificationBatch), :331-357 (ProcessPendingISLocks); instantsend/instantsend.cpp:107-114
    // (WriteNewISLock)
    pub fn ingest_verified<V: QuorumScanView + ?Sized>(
        &self,
        view: &V,
        flag: &dyn SchemeFlag,
        bytes: &[u8],
        mined_height: Option<u32>,
    ) -> Result<IsLockHash, IngestError<V::Error>> {
        let (cached, active) = (self.tip_cache.get(), view.tip_height());
        if u32::try_from(cached).ok() != Some(active) {
            return Err(IngestError::NotQuiescent {
                cached,
                active,
            });
        }
        let mined_height = mined_height
            .map(|h| i32::try_from(h).map_err(|_| IngestError::MinedHeight(h)))
            .transpose()?;
        let islock =
            LockedTx::decode(&mut &bytes[..]).map_err(|e| IngestError::Parse(e.to_string()))?;
        let read = flag.sample();
        let current = flag.sample();
        let previous = flag.sample();
        let schemes = IslockSchemes {
            read,
            passes: IslockPassSchemes {
                current,
                previous,
            },
        };
        verify_islock(self.params, view, schemes, &islock).map_err(IngestError::Verify)?;
        let hash = isdlock_hash(&islock);
        self.store.write_new(&hash, &islock).map_err(IngestError::Store)?;
        if let Some(height) = mined_height {
            self.store.write_mined(&hash, height).map_err(IngestError::Store)?;
        }
        Ok(hash)
    }
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
