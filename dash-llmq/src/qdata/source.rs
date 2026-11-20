//! The seams through which the `qdata` code reads quorums, the dkgdb's encrypted contributions,
//! the local operator key and the enabled masternode list (PLAN-QM1b §3). The node implements
//! them; the fakes in `qdata::fakes` stand in for tests.

use std::sync::Arc;

use dash_consensus::primitives::BlockHash;
use dash_evo::types::ProTxHash;
use dash_params::LlmqType;

use crate::dkg::bls::{BlsPkValue, BlsSchemeKind, BlsSkValue};
use crate::dkg::wire::BlsIesBlobBytes;

/// A built quorum (`CQuorum`) with its key cache. QM1a-3 implements it over its quorum and
/// `QuorumKeys`.
pub trait QuorumData {
    /// The signing threshold, the verification vector's length.
    fn threshold(&self) -> usize;
    /// The number of valid members (`qc->CountValidMembers()`).
    fn valid_count(&self) -> usize;
    /// The length of the valid-members bit vector (`qc->validMembers.size()`).
    fn size(&self) -> usize;
    /// `GetMemberIndex`: the position of `p` in the members, valid or not.
    fn member_index(&self, p: &ProTxHash) -> Option<usize>;
    fn is_valid_member(&self, p: &ProTxHash) -> bool;
    /// The members in the commitment's order.
    fn members(&self) -> &[ProTxHash];
    /// The commitment's valid-members bits.
    fn valid(&self) -> &[bool];
    fn vvec(&self) -> Option<Arc<Vec<BlsPkValue>>>;
    fn has_sk_share(&self) -> bool;
    /// `SetVerificationVector`: false when `v`'s hash under `scheme` is not the commitment's.
    fn set_vvec(&self, v: Vec<BlsPkValue>, scheme: BlsSchemeKind) -> bool;
    /// `SetSecretKeyShare`: false unless `sk` is `me`'s share of the verification vector.
    fn set_sk_share(&self, sk: BlsSkValue, me: &ProTxHash) -> bool;
    /// Takes the quorum's data recovery claim; false when it is already held.
    fn try_claim_recovery(&self) -> bool;
    fn release_recovery(&self);
    fn recovery_running(&self) -> bool;
}

/// The node's quorums. QM1a-3 implements it over the chainstate facade and its caches.
pub trait QuorumDataSource {
    type Q: QuorumData;
    /// The quorum store's read error, which the build propagates (`ReadContributions` throws).
    type Error;
    /// Whether the chain's parameters name the type (`Params().GetLLMQ(t)`).
    fn llmq_registered(&self, t: LlmqType) -> bool;
    /// Whether `h` is a block on the active chain.
    fn active_block(&self, h: &BlockHash) -> bool;
    /// `GetQuorum`: None for a hash with no mined commitment; builds the quorum otherwise, and
    /// fails when the build's read of the stored contributions fails.
    fn quorum(&self, t: LlmqType, h: &BlockHash) -> Result<Option<Self::Q>, Self::Error>;
    /// `GetCachedMutableQuorum`.
    fn cached_quorum(&self, t: LlmqType, h: &BlockHash) -> Option<Self::Q>;
}

/// The dkgdb's encrypted contributions for one member (`GetEncryptedContributions`). Until DK1
/// the node's reader returns None, so a member answers ENCRYPTED_CONTRIBUTIONS_MISSING.
pub trait EncryptedContributions {
    /// The requested member's blob from each valid member's row, one per set bit of `valid`, in
    /// member order. The blobs go on the wire as returned, so each is Core's re-serialization of
    /// the row read under `scheme`: an ephemeral key valid only under the other scheme is the zero
    /// key. None when `p` is not among the base block's quorum members, or when any row is
    /// missing or valid under neither scheme.
    fn get(
        &self,
        t: LlmqType,
        base: &BlockHash,
        valid: &[bool],
        p: &ProTxHash,
        scheme: BlsSchemeKind,
    ) -> Option<Vec<BlsIesBlobBytes>>;
}

/// The local masternode's operator key. A node without one has no implementation (Core's
/// `!m_nodeman`).
pub trait OperatorDecrypt {
    fn pro_tx_hash(&self) -> Option<ProTxHash>;
    /// Decrypts the secret key share in `b` for recipient `idx`.
    fn decrypt_sk(
        &self,
        b: &BlsIesBlobBytes,
        idx: usize,
        scheme: BlsSchemeKind,
    ) -> Option<BlsSkValue>;
}

/// The enabled masternodes at a block (`GetListForBlock(block).GetValidMNsList`).
pub trait ValidMnsAt {
    fn valid_pro_tx_hashes(&self, block: &BlockHash) -> Option<Vec<ProTxHash>>;
}
