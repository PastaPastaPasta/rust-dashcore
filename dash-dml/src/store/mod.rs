//! The evo store (A1 §2.5, §2.10.2; EV2 §2.10; LQ1 §2.5): what the chainstate keeps for the evo
//! layer, and the transactional delta one block writes.
//!
//! Two read shapes, never "latest" (A1 §2.10.2):
//! - **Per-block records keyed by block hash** (the MN-list diff and snapshot, the credit pool,
//!   the MNHF signals) and **per-cycle rows keyed by the cycle-base hash** (rotation snapshots).
//!   A hash names one chain position, so these reads are ancestry-correct by construction.
//!   Resolving "the value after block `at`" walks `at`'s ancestry and applies the owner's rules
//!   (deployment gates, fall-backs, diff application), so it is the owner's function over this
//!   store: EV2a's `resolve_list(at, chain, …)`, EV3's credit-pool and MNHF resolvers.
//! - **Mined commitments** (`dash_evo`'s [`MinedCommitmentReader`], which only holders of
//!   complete ancestry implement, with this store's error): the index rows of the chain ending at
//!   a given block under a height cap, which is what Core iterates, and reads resolved through
//!   `at`'s ancestry over them (`GetMinedCommitmentsUntilBlock(pindex)`, `llmq/blockprocessor.cpp`
//!   v24:687-729). Records on other branches are kept and returned only for a chain they are on.
//!
//! The list, pool and MNHF records are never erased: disconnects move the evo tip and leave
//! them (EV2 §2.10, §4), so a reorg back reconnects without recomputing storage, and reads for a
//! disconnected block still resolve. The mined-commitment rows are erased on undo, as Core
//! erases them inside its evodb transaction ([`EvoDelta::erased_mined`]; CH7 Q1); the
//! in-memory [`MemEvoStore`] keeps them, and its ancestry-resolved reads hide them. Store errors
//! are fail-stop node errors, never block verdicts (A1 I5).

mod delta;
mod mem;
mod resolve;

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use dash_evo::llmq::{
    BrokenInvariant, MemberError, MinedCommitmentReader, QuorumMemberSource, QuorumSnapshot,
};
use dash_primitives::{BlockHash, LlmqType};

use crate::creditpool::{CreditBlockData, PoolState};
use crate::mnhf::EhfSignals;
use crate::{Dmn, MnList, MnListDiffRecord};

pub use delta::{EvoDelta, MinedCommitmentRecord, MinedErase};
pub use mem::{MemEvoStore, MemEvoView};
pub use resolve::{CacheHint, ListCache, LruListCache, NoListCache, resolve_list};

/// A local store fault. **Fail-stop:** CH7a turns it into a node error and an abort (A1 I5),
/// never a block verdict and never `failed-procspectxsinblock` (manager ruling, LQ1 D4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The engine could not read a record (I/O error, engine-detected corruption, an undecodable
    /// record).
    Read {
        what: Cow<'static, str>,
    },
    /// A record an established invariant requires is missing, or a rewrite of an existing key
    /// differs from what is stored (Core's `WriteDerived` mismatch, `evodb.h` v24:150-205).
    /// Core's text for these is "Found EvoDB inconsistency, you must reindex".
    Inconsistent {
        what: Cow<'static, str>,
    },
}

impl StoreError {
    /// A [`StoreError::Read`].
    pub fn read(what: impl Into<Cow<'static, str>>) -> Self {
        StoreError::Read {
            what: what.into(),
        }
    }

    /// A [`StoreError::Inconsistent`].
    pub fn inconsistent(what: impl Into<Cow<'static, str>>) -> Self {
        StoreError::Inconsistent {
            what: what.into(),
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Read {
                what,
            } => write!(f, "evo store read failed: {what}"),
            StoreError::Inconsistent {
                what,
            } => {
                write!(f, "Found EvoDB inconsistency, you must reindex: {what}")
            }
        }
    }
}

impl std::error::Error for StoreError {}

/// A mined commitment's base above the block that mined it: [`StoreError::Inconsistent`].
impl From<BrokenInvariant> for StoreError {
    fn from(e: BrokenInvariant) -> Self {
        StoreError::inconsistent(e.0)
    }
}

/// The node's member source: the DML's entries and this store's error (EV addendum §2.2).
///
/// The object lifetime `'a` is explicit so a source can borrow its attempt: CH7b's
/// `AttemptHandle<'v>` and the capability-scoped adapters of EV addendum §2.6 (DEC-117) live only
/// as long as the attempt or V command that made them. Callers take `&DmlMemberSource<'_>`.
///
/// ```
/// # use std::sync::Arc;
/// # use dash_consensus::BlockId;
/// # use dash_dml::store::{DmlMemberSource, EvoStore, MemEvoStore, StoreError};
/// # use dash_dml::{Dmn, testkit::MemChain};
/// # use dash_evo::EffectSink;
/// # use dash_evo::llmq::{MemberError, QuorumMemberSource, QuorumMembers};
/// # use dash_primitives::LlmqType;
/// /// A source borrowing its attempt's view.
/// struct AttemptBound<'v>(&'v dyn EvoStore);
/// impl QuorumMemberSource for AttemptBound<'_> {
///     type Entry = Arc<Dmn>;
///     type Error = StoreError;
///     fn members(&self, _: LlmqType, _: BlockId, _: &mut dyn EffectSink)
///         -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
///         Ok(Vec::new().into())
///     }
/// }
/// fn walk(source: &DmlMemberSource<'_>) -> usize {
///     source.members(LlmqType::LlmqTest, BlockId(0), &mut Vec::new()).unwrap().len()
/// }
/// let chain = MemChain::new();
/// let store = MemEvoStore::new();
/// let view = store.view(&chain);
/// assert_eq!(walk(&AttemptBound(&view)), 0);
/// // Boxed within the attempt's scope, as an attempt holds it.
/// let held: Box<DmlMemberSource<'_>> = Box::new(AttemptBound(&view));
/// assert_eq!(walk(&*held), 0);
/// ```
///
/// A source cannot outlive the attempt it borrows, whatever lifetime its holder names:
///
/// ```compile_fail,E0597
/// # use std::sync::Arc;
/// # use dash_consensus::BlockId;
/// # use dash_dml::store::{DmlMemberSource, EvoStore, MemEvoStore, StoreError};
/// # use dash_dml::{Dmn, testkit::MemChain};
/// # use dash_evo::EffectSink;
/// # use dash_evo::llmq::{MemberError, QuorumMemberSource, QuorumMembers};
/// # use dash_primitives::LlmqType;
/// # struct AttemptBound<'v>(&'v dyn EvoStore);
/// # impl QuorumMemberSource for AttemptBound<'_> {
/// #     type Entry = Arc<Dmn>;
/// #     type Error = StoreError;
/// #     fn members(&self, _: LlmqType, _: BlockId, _: &mut dyn EffectSink)
/// #         -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
/// #         Ok(Vec::new().into())
/// #     }
/// # }
/// let escaped: Box<DmlMemberSource<'_>> = {
///     let chain = MemChain::new();
///     let store = MemEvoStore::new();
///     let view = store.view(&chain);
///     Box::new(AttemptBound(&view))
/// };
/// ```
pub type DmlMemberSource<'a> = dyn QuorumMemberSource<Entry = Arc<Dmn>, Error = StoreError> + 'a;

/// What node callers act on when the member source gives no answer (EV2 §2.11; LQ1 §2.6's
/// mapping table): EV0's two classes, re-joined from the shared [`MemberError`] (EV addendum
/// §2.3, node column).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeMemberError {
    /// A local store fault, or an input the computation needs missing for a connected block.
    ///
    /// **Fail-stop:** the caller returns it as a node error, never as a block verdict and never
    /// as `failed-procspectxsinblock`. This is the manager's ruling (LQ1
    /// deviation D4). Core's precedent is v24's typed `EvoDbInconsistencyError` → `state.Error`
    /// (`evo/specialtxman.cpp` v24:989-992); Core's other store exceptions, the DML's included,
    /// still reach `failed-procspectxsinblock` (v24:993-996).
    ///
    /// An absent snapshot row is **not** this error: the rows interface returns `Ok(None)` and
    /// the rotation breaks (quirks 28a, 28e, young chains), as in Core. Unavailable block data is
    /// not one either (`CoinbaseFacts::BlockDataUnavailable`, LQ1 §3.7.3). The member computation
    /// reads no commitment; a mined commitment missing in a scan is Core's `GetQuorum` nullptr,
    /// a value (LQ1 §3.8.1), not this error.
    Store(StoreError),
    /// Core's `IsQuorumTypeEnabled` throws `runtime_error` for an unknown type
    /// (`validation.cpp` v24:6707-6708); Core's catch maps it to
    /// `failed-procspectxsinblock`. Unreachable from block validation: EV1/LQ2 reject unknown
    /// types first (`bad-qc-commitment-type`).
    UnknownLlmqType(u8),
}

/// What a [`StoreError::Inconsistent`] from a [`MemberError::Missing`] begins with.
pub const MEMBERS_MISSING: &str = "MN list or coinbase missing for a connected block";

/// The node's mapping (EV addendum §2.3): the node's views hold every input of a connected
/// block, so a [`MemberError::Missing`] is store corruption, D4 like a read fault.
impl From<MemberError<StoreError>> for NodeMemberError {
    fn from(e: MemberError<StoreError>) -> Self {
        match e {
            MemberError::Missing(at) => NodeMemberError::Store(StoreError::inconsistent(format!(
                "{MEMBERS_MISSING} (block id {})",
                at.0
            ))),
            MemberError::View(e) => NodeMemberError::Store(e),
            MemberError::UnknownLlmqType(t) => NodeMemberError::UnknownLlmqType(t),
        }
    }
}

impl From<StoreError> for NodeMemberError {
    fn from(e: StoreError) -> Self {
        NodeMemberError::Store(e)
    }
}

impl fmt::Display for NodeMemberError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeMemberError::Store(e) => write!(f, "quorum members: {e}"),
            NodeMemberError::UnknownLlmqType(t) => write!(f, "unknown LLMQ type {t}"),
        }
    }
}

impl std::error::Error for NodeMemberError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            NodeMemberError::Store(e) => Some(e),
            NodeMemberError::UnknownLlmqType(_) => None,
        }
    }
}

/// The DML slice (EV2 §2.10): the per-block rows `resolve_list` reconstructs lists from.
pub trait DmlReader: Sync {
    /// The `mn_snapshot` row of `block`: the list after it, written every 576 blocks and at the
    /// first DIP3 block.
    fn mn_list_snapshot(&self, block: &BlockHash) -> Result<Option<Arc<MnList>>, StoreError>;

    /// The `mn_diff` row of `block`, present for every DIP3-active block that was connected
    /// (kept after a disconnect).
    fn mn_list_diff(&self, block: &BlockHash) -> Result<Option<Arc<MnListDiffRecord>>, StoreError>;
}

/// The evo store (A1 §2.5): one coherent read view of the evo records, bound to a block index.
///
/// The chainstate implements it over `chainstate/` (CH7a's `EvoReadView`: the active and frozen
/// layers' deltas over a DB snapshot, valid across reorgs because every record is keyed by
/// block hash and never mutated in place); tests use [`MemEvoView`].
pub trait EvoStore: DmlReader + MinedCommitmentReader<Error = StoreError> {
    /// `meta.evo_tip` (A1 §3.5): the block the evo state is at, `None` before the first block.
    fn best_block(&self) -> Result<Option<BlockHash>, StoreError>;

    /// The credit pool after `block`, or the exception Core's construction throws (EV3 D8):
    /// present for every V20-active block that was connected.
    fn credit_pool(&self, block: &BlockHash) -> Result<Option<Arc<PoolState>>, StoreError>;

    /// The credit data of `block` (EV3 D9): present for every DIP3-active block that was
    /// connected, except one whose data Core cannot read. Core would throw at each read, and the
    /// store answers a fault, never a verdict: no connectable DIP3-active block holds such data,
    /// because `CheckSpecialTxInner` rejects an unreadable unlock payload
    /// (`bad-assetunlocktx-payload`) and `CheckTransaction` an out-of-range output on every
    /// DIP3-active block, before V20 included.
    fn credit_data(&self, block: &BlockHash) -> Result<Option<CreditBlockData>, StoreError>;

    /// The MNHF signals row of `block` (EV3 D15): present for every V20-active block that was
    /// connected.
    fn mnhf_signals(&self, block: &BlockHash) -> Result<Option<EhfSignals>, StoreError>;

    /// `GetSnapshotForBlock(type, cycleBase)`: the rotation snapshot row, keyed by hash, so
    /// ancestry-free. `Ok(None)` means no row, a normal outcome (quirks 28a and 28e, young
    /// chains), never an error (LQ1 §2.3). Rows are overwritten unconditionally, never erased.
    fn quorum_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<Arc<QuorumSnapshot>>, StoreError>;
}

#[cfg(test)]
mod tests {
    use dash_consensus::BlockId;

    use super::*;

    /// EV addendum §2.4 (shared part): the node maps a missing input of a connected block to a
    /// store inconsistency with Core's text, D4; a view fault passes through unchanged.
    #[test]
    fn a_missing_input_is_a_node_store_inconsistency() {
        let e = NodeMemberError::from(MemberError::Missing(BlockId(7)));
        let NodeMemberError::Store(StoreError::Inconsistent {
            what,
        }) = &e
        else {
            panic!("Missing must map to Store(Inconsistent), got {e:?}");
        };
        assert!(what.contains("block id 7"));
        assert!(e.to_string().contains("Found EvoDB inconsistency, you must reindex"));
        let read = StoreError::read("injected");
        assert_eq!(
            NodeMemberError::from(MemberError::View(read.clone())),
            NodeMemberError::Store(read)
        );
        assert_eq!(
            NodeMemberError::from(MemberError::<StoreError>::UnknownLlmqType(250)),
            NodeMemberError::UnknownLlmqType(250)
        );
        assert!(matches!(StoreError::from(BrokenInvariant("x")), StoreError::Inconsistent { .. }));
    }
}
