//! The quorum and credit-pool seams of types 7 and 9 (EV1 §2.3, EV1-5b-2): Core's
//! `CQuorumManager::GetQuorum` and `ScanQuorums`, and the asset-unlock index set.

use dash_consensus::ctx::BlockId;
use dash_evo::effect::EffectSink;
use dash_params::AVAILABLE_LLMQS;
use dash_pkc::bls::BlsLazyPk;
use dash_primitives::{LlmqType, QuorumHash};
use dash_types::Numeric;

use crate::creditpool::RangesSet;
use crate::store::StoreError;

/// The largest `signingActiveQuorumCount` in the LLMQ table, over every type a chain can select.
// Core: llmq/params.h:138-511
pub const fn max_signing_active_quorum_count() -> usize {
    let mut max = 0;
    let mut i = 0;
    while i < AVAILABLE_LLMQS.len() {
        let count = AVAILABLE_LLMQS[i].signing_active_quorum_count;
        if count > max {
            max = count;
        }
        i += 1;
    }
    max as usize
}

/// The most quorums one scan of types 7 and 9 asks for: type 9's `signingActiveQuorumCount + 1`
/// over every type (D14). [`QuorumScan`] holds that many, so no scan truncates.
// Core: evo/assetlocktx.cpp:122-123
pub const MAX_QUORUM_SCAN: usize = max_signing_active_quorum_count() + 1;

/// A quorum `GetQuorum` found: its hash and `qc->quorumPublicKey`, read as the commitment
/// decoded it.
#[derive(Clone, Debug)]
pub struct QuorumKey {
    pub quorum_hash: QuorumHash,
    pub public_key: BlsLazyPk,
}

/// The hashes one `ScanQuorums` returned, newest first, in a buffer of [`MAX_QUORUM_SCAN`].
#[derive(Clone, Debug)]
pub struct QuorumScan {
    hashes: [QuorumHash; MAX_QUORUM_SCAN],
    len: usize,
}

impl Default for QuorumScan {
    fn default() -> Self {
        QuorumScan {
            hashes: [QuorumHash::ZERO; MAX_QUORUM_SCAN],
            len: 0,
        }
    }
}

impl QuorumScan {
    /// Appends `hash`.
    ///
    /// # Errors
    ///
    /// [`SeamError::ScanCapacity`] when the buffer holds [`MAX_QUORUM_SCAN`] already; it keeps
    /// what it holds.
    pub fn push(&mut self, hash: QuorumHash) -> Result<(), SeamError> {
        let slot = self.hashes.get_mut(self.len).ok_or(SeamError::ScanCapacity)?;
        *slot = hash;
        self.len += 1;
        Ok(())
    }

    /// The hashes, newest first.
    #[must_use]
    pub fn as_slice(&self) -> &[QuorumHash] {
        &self.hashes[..self.len]
    }

    /// Empties the buffer.
    pub fn clear(&mut self) {
        self.len = 0;
    }
}

/// A quorum seam that gave no answer. It aborts the attempt, as a store fault does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeamError {
    /// A scan for more quorums than [`MAX_QUORUM_SCAN`]: no LLMQ type asks for that, so it is a
    /// caller's error, never a short answer (D14).
    ScanCapacity,
    /// The quorum store failed a read.
    Store(StoreError),
}

/// Core's quorum manager as types 7 and 9 read it (D1-f). Each call records its effects to `fx`
/// in Core's call order.
pub trait QuorumLookup {
    /// `GetQuorum(llmqType, quorumHash)`: the quorum, if the manager has it.
    ///
    /// # Errors
    ///
    /// [`SeamError`]: no answer.
    // Core: llmq/quorums.cpp CQuorumManager::GetQuorum
    fn get_quorum(
        &mut self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        fx: &mut dyn EffectSink,
    ) -> Result<Option<QuorumKey>, SeamError>;

    /// `ScanQuorums(llmqType, pindexStart, n)`: the hashes of the newest `n` quorums of
    /// `llmq_type` at `at`, newest first, into `out` (emptied first). Every hash Core returns is
    /// written.
    ///
    /// # Errors
    ///
    /// [`SeamError::ScanCapacity`] when `n` is above [`MAX_QUORUM_SCAN`], before writing
    /// anything; [`SeamError::Store`] on a store fault.
    // Core: llmq/quorums.cpp CQuorumManager::ScanQuorums
    fn scan_quorums(
        &mut self,
        llmq_type: LlmqType,
        at: BlockId,
        n: usize,
        out: &mut QuorumScan,
        fx: &mut dyn EffectSink,
    ) -> Result<(), SeamError>;
}

/// The asset-unlock indexes the credit pool has seen: Core's `CRangesSet::Contains` on the pool
/// at `pindexPrev` (block path). The mempool path passes none.
// Core: evo/assetlocktx.cpp:214
pub trait UnlockIndexes {
    /// Whether `index` was unlocked already.
    fn contains(&self, index: u64) -> bool;
}

impl UnlockIndexes for RangesSet {
    fn contains(&self, index: u64) -> bool {
        RangesSet::contains(self, index)
    }
}

/// A [`QuorumLookup`] for tests: quorums newest first, each with its type, and the calls made.
/// `GetQuorum` finds by type and hash; `ScanQuorums` returns the first `n` of the type, whatever
/// `at` is.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Clone, Debug, Default)]
pub struct TestQuorums {
    pub quorums: Vec<(LlmqType, QuorumKey)>,
    /// Each call: `("get" or "scan", type, n)`, `n` 0 for a get.
    pub calls: Vec<(&'static str, LlmqType, usize)>,
}

#[cfg(any(test, feature = "test-utils"))]
impl QuorumLookup for TestQuorums {
    fn get_quorum(
        &mut self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        _fx: &mut dyn EffectSink,
    ) -> Result<Option<QuorumKey>, SeamError> {
        self.calls.push(("get", llmq_type, 0));
        Ok(self
            .quorums
            .iter()
            .find(|(t, q)| *t == llmq_type && q.quorum_hash == *quorum_hash)
            .map(|(_, q)| q.clone()))
    }

    fn scan_quorums(
        &mut self,
        llmq_type: LlmqType,
        _at: BlockId,
        n: usize,
        out: &mut QuorumScan,
        _fx: &mut dyn EffectSink,
    ) -> Result<(), SeamError> {
        self.calls.push(("scan", llmq_type, n));
        if n > MAX_QUORUM_SCAN {
            return Err(SeamError::ScanCapacity);
        }
        out.clear();
        for (_, q) in self.quorums.iter().filter(|(t, _)| *t == llmq_type).take(n) {
            out.push(q.quorum_hash)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D14: 33 at the pin (`llmq_60_75`, 32 signing quorums), and room for every type's
    /// `signingActiveQuorumCount + 1`.
    #[test]
    fn scan_holds_every_types_count() {
        assert_eq!(MAX_QUORUM_SCAN, 33);
        for p in &AVAILABLE_LLMQS {
            let n = usize::try_from(p.signing_active_quorum_count).expect("not negative") + 1;
            assert!(n <= MAX_QUORUM_SCAN, "{}", p.name);
        }
    }

    /// The buffer takes [`MAX_QUORUM_SCAN`] hashes and refuses the next, keeping them.
    #[test]
    fn scan_never_truncates() {
        let mut scan = QuorumScan::default();
        for i in 0..MAX_QUORUM_SCAN {
            scan.push(QuorumHash::from_hex_lossy(&format!("{i:064x}"))).expect("room");
        }
        assert_eq!(scan.push(QuorumHash::ZERO), Err(SeamError::ScanCapacity));
        assert_eq!(scan.as_slice().len(), MAX_QUORUM_SCAN);
        scan.clear();
        assert!(scan.as_slice().is_empty());
    }
}
