//! Injected signing facts and effects shared by the signing and sig-share managers.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dash_evo::locks::wire::RecoveredSig;
use dash_evo::types::ProTxHash;
use dash_params::{LlmqParams, LlmqType};

use crate::dkg::bls::{BlsCtx, BlsPkValue, BlsSchemeKind, BlsSigValue, BlsSkValue};
use crate::dkg::conn::eval_spork;
use crate::dkg::wire::{BatchedSigShares, QuorumHash, SigSesAnn, SigShare, SigSharesInv};

#[cfg(any(test, feature = "testkit"))]
pub mod fakes;

/// Core's `NodeId`.
pub type PeerId = i64;
/// Leave room for nodes that have not reached the chain tip (`l/quorumsman.h:211-214`).
pub const SIGN_HEIGHT_OFFSET: i32 = 8;

/// A built quorum (`CQuorum`), with members in the commitment's order.
pub trait SigningQuorum {
    fn llmq_type(&self) -> LlmqType;
    fn quorum_hash(&self) -> &QuorumHash;
    fn params(&self) -> &LlmqParams;
    fn members(&self) -> &[ProTxHash];
    fn is_member(&self, p: &ProTxHash) -> bool;
    fn is_valid_member(&self, p: &ProTxHash) -> bool;
    fn member_index(&self, p: &ProTxHash) -> Option<u16>;
    fn has_verification_vector(&self) -> bool;
    fn quorum_public_key(&self) -> &BlsPkValue;
    /// Invalid, absent or non-member shares return `None`.
    fn pub_key_share(&self, member: u16) -> Option<BlsPkValue>;
    /// An owned, zeroized secret-key copy; invalid or absent returns `None`.
    fn sk_share(&self) -> Option<BlsSkValue>;
}

/// Fallible `CQuorumManager` reads; errors must not become missing quorums.
pub trait QuorumLookup {
    type Quorum: SigningQuorum;
    type Error: core::fmt::Debug;
    fn get_quorum(
        &self,
        t: LlmqType,
        h: &QuorumHash,
    ) -> Result<Option<Arc<Self::Quorum>>, Self::Error>;
    /// Core's active quorum window (`l/signing.cpp:701-712`).
    fn is_quorum_active(&self, t: LlmqType, h: &QuorumHash) -> Result<bool, Self::Error>;
    fn select_quorum_for_signing(
        &self,
        t: LlmqType,
        selection_hash: &[u8; 32],
        sign_height: i32,
        sign_offset: i32,
    ) -> Result<Option<Arc<Self::Quorum>>, Self::Error>;
}

/// Select at the tip, with Core's default verification offset.
// Core: v24.0.0 l/quorumsman.h:222-223 SelectQuorumForSigning
pub fn select_for_signing_default<L: QuorumLookup + ?Sized>(
    l: &L,
    t: LlmqType,
    id: &[u8; 32],
) -> Result<Option<Arc<L::Quorum>>, L::Error> {
    l.select_quorum_for_signing(t, id, -1, SIGN_HEIGHT_OFFSET)
}

/// The recovered-signature predicates the sig-share manager reads.
pub trait RecSigReads {
    type Error: core::fmt::Debug;
    /// Platform compares `(type, id, msg)`; other types compare `(type, id)`.
    fn has_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, Self::Error>;
    fn has_for_session(&self, sign_hash: &[u8; 32]) -> Result<bool, Self::Error>;
}

/// One fully connected peer, with the MNAUTH identity if verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerFacts {
    pub id: PeerId,
    pub verified_pro_tx: Option<ProTxHash>,
    pub common_version: i32,
}

/// Connection-manager facts sampled for a call.
pub trait SigSharesNet {
    /// Intra-quorum and observer peers; order is irrelevant (`l/signing_shares.cpp:640,1107`).
    fn quorum_nodes(&self, t: LlmqType, h: &QuorumHash) -> Vec<PeerId>;
    /// Fully connected peers in connman order (`l/signing_shares.cpp:1171,1437`).
    fn fully_connected(&self) -> Vec<PeerFacts>;
    fn is_banned(&self, p: PeerId) -> bool;
}

/// Local signing identity and permission, read per call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignerFacts {
    pub my_pro_tx: Option<ProTxHash>,
    pub signing_allowed: bool,
}

/// Operator-key signing for the single-member path (`l/signing_shares.cpp:1578-1581`).
pub trait OperatorSigner {
    fn sign(&self, hash: &[u8; 32], scheme: BlsSchemeKind) -> Option<BlsSigValue>;
}

/// SPORK_21's global activation (intake) and raw value (per-type routing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SporkFacts {
    pub spork21_active: bool,
    pub spork21_value: i64,
}

impl SporkFacts {
    // Core: v24.0.0 l/options.cpp:37-40 IsAllMembersConnectedEnabled
    pub fn all_members_connected(&self, t: LlmqType) -> bool {
        eval_spork(t, self.spork21_value)
    }
}

/// Injected milliseconds since the epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Now {
    pub ms: i64,
}

impl Now {
    // Core: v24.0.0 l/signing_shares.cpp:1386-1391 CSigSharesManager::Cleanup (GetTime seconds)
    pub fn secs(&self) -> i64 {
        self.ms.div_euclid(1000)
    }
}

/// LQ5 recovery owns all updates to the manager's single recovery counter.
pub trait ShareRecovery {
    fn try_recover(
        &self,
        params: &LlmqParams,
        members: &[ProTxHash],
        shares: &BTreeMap<u16, BlsSigValue>,
        counter: &mut u32,
    ) -> Recovered;
    fn recovery_member_order(&self, members: &[ProTxHash], id: &[u8; 32]) -> Vec<usize>;
}

/// A recovery result; SG1 performs the requested self-verification.
#[allow(clippy::large_enum_variant, reason = "the shared recovery contract owns the BLS value")]
pub enum Recovered {
    NotEnough,
    Failed,
    Sig {
        sig: BlsSigValue,
        self_verify: bool,
    },
}

/// An asynchronous request to sign, optionally using a specific quorum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignRequest {
    pub llmq_type: LlmqType,
    pub id: [u8; 32],
    pub msg_hash: [u8; 32],
    pub quorum_hash: Option<QuorumHash>,
    pub allow_resign: bool,
    pub allow_diff_msg: bool,
}

/// Facts supplied to each sig-share entry point.
pub struct SharesEnv<'a, L: QuorumLookup, R: RecSigReads, N: SigSharesNet, S: ShareRecovery> {
    pub lookup: &'a L,
    pub recsigs: &'a R,
    pub net: &'a N,
    pub recovery: &'a S,
    pub signer: &'a SignerFacts,
    pub sporks: SporkFacts,
    pub now: Now,
    pub bls: BlsCtx,
}

/// Actions for the runtime to apply after a pure manager call.
pub enum SharesEffect {
    Ban {
        peer: PeerId,
        mark_shares_banned: bool,
    },
    Recovered(RecoveredSig),
    Send {
        peer: PeerId,
        msgs: Vec<SigMsg>,
    },
}

/// Per-peer message families in Core's packing order.
// Core: v24.0.0 l/signing_shares.cpp:1201-1297 CSigSharesManager::SendMessages
pub enum SigMsg {
    SesAnn(Vec<SigSesAnn>),
    GetSigShares(Vec<SigSharesInv>),
    BSigShares(Vec<BatchedSigShares>),
    SigSharesInv(Vec<SigSharesInv>),
    SigShare(Vec<SigShare>),
}

/// A batch quorum's wire type byte (`LlmqType::to_base()`) and hash.
/// The byte preserves type identity while permitting deterministic ordered maps.
// Core: v24.0.0 l/signing_shares.cpp:587; l/net_signing.cpp:425 ProcessPendingSigShares
pub type QuorumKey = (u8, QuorumHash);

/// A received share with the read context captured at intake. Its signature is decoded under
/// `read`, never under the verifier's check context.
// Core: v24.0.0 src/bls/bls.h:412-438, :495-508 CBLSLazyWrapper
#[derive(Clone, Debug)]
pub struct CapturedShare {
    pub share: SigShare,
    pub read: BlsCtx,
}

/// A collected verification batch, shared by intake, verification and worker ticks.
pub struct ShareBatch<Q: SigningQuorum> {
    /// Per-source shares in collection order (`l/signing_shares.cpp:546-567`).
    pub by_source: BTreeMap<PeerId, Vec<CapturedShare>>,
    /// One entry per key occurring in `by_source`; used only for lookup (:583-603).
    pub quorums: BTreeMap<QuorumKey, Arc<Q>>,
}

/// The outcome of SG1b-2's `collect_to_verify`. Lookup failures stay in the `Err` arm.
pub enum Collected<Q: SigningQuorum> {
    /// No shares were collected for verification, including when every pending share was
    /// already known and was dropped.
    Empty,
    /// Every batch quorum was found; `more_work` is Core's continuation flag.
    Ready {
        batch: ShareBatch<Q>,
        more_work: bool,
    },
    /// A batch quorum was not found. The drained shares stay out of pending and the whole batch
    /// is dropped: nothing is verified, processed or banned, and collection stops for this pass.
    // declared difference (DEC-1633): v24.0.0 l/signing_shares.cpp:592-600, l/net_signing.cpp:367-369
    MissingQuorum {
        key: QuorumKey,
    },
}

/// Bad-source attribution and the count of shares pushed to the verifier.
pub struct BatchVerdict {
    /// Every share from these sources is discarded (`l/net_signing.cpp:448-454`).
    pub bad_sources: BTreeSet<PeerId>,
    /// `verifyCount` (:436), excluding recovered-session skips.
    pub pushed: usize,
}

/// Storage errors and missing verification inputs; Core asserts on missing public-key shares.
#[derive(Debug)]
pub enum VerifyError<E> {
    RecSig(E),
    /// Only for an inconsistent batch; collection reports [`Collected::MissingQuorum`] instead.
    MissingQuorum(QuorumKey),
    MissingPubKeyShare {
        key: QuorumKey,
        member: u16,
    },
}

/// The verification half of `ProcessPendingSigShares` (`l/net_signing.cpp:398-446`).
/// Per source, per share in order:
/// 1. Skip if `recsigs.has_for_signing(type, id, msg)` is true, before decoding (:410-412).
/// 2. Decode the signature under the share's captured `read` context; an invalid value marks
///    the source bad and stops it (:416-423).
/// 3. Look up `(type.to_base(), hash)` and its member's public-key share (:425-426); absence returns
///    `MissingPubKeyShare` where Core asserts (:428-433).
/// 4. Verify under `check`, the loaded view's check context; any failure marks the source bad. Pass all shares from sources absent from
///    `bad_sources` to `process_verified`, including shares skipped in step 1 (:457).
pub trait ShareVerifier {
    fn verify_batch<Q: SigningQuorum, R: RecSigReads>(
        &self,
        batch: &ShareBatch<Q>,
        recsigs: &R,
        check: BlsCtx,
    ) -> Result<BatchVerdict, VerifyError<R::Error>>;
}

/// `PendingSignatureData` (`l/signing_shares.cpp:1553-1557`), queued for a worker tick.
pub struct PendingSign<Q: SigningQuorum> {
    pub quorum: Arc<Q>,
    pub id: [u8; 32],
    pub msg_hash: [u8; 32],
}
