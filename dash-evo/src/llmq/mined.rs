//! Mined commitments (A1 §2.10.2; LQ1 §2.5 and §3.8): the reader LQ1's scans and the node's
//! callers read through, with an error of the implementor's (EV addendum §1.2, §2.2). Only
//! holders of complete ancestry implement it (the node, test kits); SPV builds its scan from the
//! authenticated set (SB0 §5.2 step 6).

use std::sync::Arc;

use dash_consensus::{BlockId, ChainAncestry};
use dash_params::LlmqType;
use dash_primitives::{BlockHash, QuorumHash};

use super::LlmqCommitment;
use super::chain::BrokenInvariant;

/// A mined commitment and the hash of the block that mined it (Core `q_mc`'s value).
pub type MinedCommitment = (Arc<LlmqCommitment>, BlockHash);

/// Which of Core's two mined-commitment height indexes a record is in (`llmq/blockprocessor.cpp`
/// v24:299-309, 437-441).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MinedFamily {
    /// `BuildInversedHeightKey`: commitments of a type for which rotation is not enabled.
    NonRotated,
    /// `BuildInversedHeightKeyIndexed`: rotated commitments with this quorum index.
    Rotated {
        quorum_index: i16,
    },
}

/// One row of Core's mined-commitment height index: the mining height and the base's height
/// (the row's value).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MinedIndexEntry {
    /// The height of the block that mined the commitment.
    pub mined_height: u32,
    /// The height of the quorum base.
    pub base_height: u32,
}

/// Mined commitments (A1 §2.10.2; LQ1 §2.5 and §3.8). The single commitment-read trait: LQ1's
/// `MinedCommitmentIndex` is retired in its favour (EV0 API review Q1).
///
/// Two levels:
/// - **The index rows** ([`MinedCommitmentReader::mined_index`]), what Core iterates: the rows
///   of the chain ending at `chain_end`, under a height cap. Core's index holds exactly the active
///   chain's rows (undo erases them), so Core's reads are `chain_end = tip` reads.
/// - **The ancestry-resolved reads** (`mined_until`, `nth_mined_by_index`, `newest_by_index`),
///   provided over it with `chain_end = at`, the cap `h(at)` and bases mapped through `at`. They
///   equal Core's for an active-chain `at` or the block being connected, which is every Core call
///   site except one: the qrinfo builder (below).
///
/// **The qrinfo exception** (EV0 API review r1, M2). `BuildQuorumRotationInfo` checks the base
/// blocks against the active chain but not `blockRequestHash` (`llmq/snapshot.cpp` v24:105, 141).
/// For the request block `S` it reads the **active chain's** index under the cap
/// `h(S)`, maps each base through `S->GetAncestor`, then reads `GetMinedCommitment` with no chain
/// context (v24:191-197). The port is `mined_index(type, Rotated { i }, tip, h(S), 1)`, the base
/// `chain().ancestor(S, base_height)`, then `mined_commitment(type, hash(base), tip)`; for a
/// stale `S` that differs from `nth_mined_by_index(type, S, i, 0)`, and a base above the fork maps
/// to a block of `S`'s branch, whose commitment read is `None` (Core's request failure).
pub trait MinedCommitmentReader: Sync {
    /// The reader's error. It reports a broken ancestry invariant ([`base_through`]) without naming
    /// a store error, so the trait is shared (EV addendum §2.2); the node's is `StoreError`.
    type Error: From<BrokenInvariant>;

    /// The block index `at` and `chain_end` arguments resolve against.
    fn chain(&self) -> &dyn ChainAncestry;

    /// `GetMinedCommitment(type, quorumHash)` (`llmq/blockprocessor.cpp` v24:675):
    /// the commitment and the hash of the block that mined it, if a block in `at`'s ancestry
    /// mined one. `None` is Core's `{null, ZERO}`.
    ///
    /// `at` is the chain end the caller resolves against: the tip (Core's own read, including
    /// qrinfo's), the end of the chain a v24 `GetQuorum` overload is bound to, or the block being
    /// connected. **Never the quorum base**: a commitment is mined after its base, so `at = base`
    /// always gives `None`.
    fn mined_commitment(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        at: BlockId,
    ) -> Result<Option<MinedCommitment>, Self::Error>;

    /// The index rows Core iterates (`GetMinedCommitmentsUntilBlock` v24:687,
    /// `GetLastMinedCommitmentsByQuorumIndexUntilBlock` v24:731): rows of `family` for
    /// `llmq_type` whose mining block is `chain_end` or one of its ancestors, mined at heights
    /// `1..=min(max_mined_height, h(chain_end))` (Core stops before height 0), newest first, at
    /// most `limit`.
    fn mined_index(
        &self,
        llmq_type: LlmqType,
        family: MinedFamily,
        chain_end: BlockId,
        max_mined_height: u32,
        limit: usize,
    ) -> Result<Vec<MinedIndexEntry>, Self::Error>;

    /// `GetMinedCommitmentsUntilBlock(type, at, max)`: the base blocks of the newest `max`
    /// commitments of a **non-rotated** type mined at heights `1..=h(at)` in `at`'s ancestry,
    /// newest first.
    fn mined_until(
        &self,
        llmq_type: LlmqType,
        at: BlockId,
        max: usize,
    ) -> Result<Vec<BlockId>, Self::Error> {
        let rows =
            self.mined_index(llmq_type, MinedFamily::NonRotated, at, height(self, at), max)?;
        rows.iter().map(|row| base_through(self.chain(), at, row.base_height)).collect()
    }

    /// `GetLastMinedCommitmentsByQuorumIndexUntilBlock(type, at, index, nth)`: the base of the
    /// `nth` newest commitment (0 = newest) of a **rotated** type with quorum index `index`,
    /// mined at heights `1..=h(at)` in `at`'s ancestry.
    fn nth_mined_by_index(
        &self,
        llmq_type: LlmqType,
        at: BlockId,
        index: i16,
        nth: usize,
    ) -> Result<Option<BlockId>, Self::Error> {
        let family = MinedFamily::Rotated {
            quorum_index: index,
        };
        let rows =
            self.mined_index(llmq_type, family, at, height(self, at), nth.saturating_add(1))?;
        rows.get(nth).map(|row| base_through(self.chain(), at, row.base_height)).transpose()
    }

    /// The newest `k` bases for one index in one walk (LQ1 lever L6): equal to
    /// `nth_mined_by_index` for `nth = 0, 1, …` up to the first `None`.
    fn newest_by_index(
        &self,
        llmq_type: LlmqType,
        at: BlockId,
        index: i16,
        k: usize,
    ) -> Result<Vec<BlockId>, Self::Error> {
        let family = MinedFamily::Rotated {
            quorum_index: index,
        };
        let rows = self.mined_index(llmq_type, family, at, height(self, at), k)?;
        rows.iter().map(|row| base_through(self.chain(), at, row.base_height)).collect()
    }
}

/// `h(at)`.
fn height<R: MinedCommitmentReader + ?Sized>(reader: &R, at: BlockId) -> u32 {
    reader.chain().header(at).height
}

/// Core's `pindex->GetAncestor(quorumHeight)`, which Core asserts exists. Missing, it is
/// corruption here ([`BrokenInvariant`]), not a panic: a base is never above its mining block.
pub fn base_through<E: From<BrokenInvariant>>(
    chain: &dyn ChainAncestry,
    through: BlockId,
    base_height: u32,
) -> Result<BlockId, E> {
    chain
        .ancestor(through, base_height)
        .ok_or_else(|| BrokenInvariant("mined commitment base above the queried block").into())
}
