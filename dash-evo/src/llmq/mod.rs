//! LLMQ consensus (A1 §2.5 `llmq`; `designs/LQ1-quorum-members.md`).
//!
//! EV0 lands the shared pieces: the member-source trait EV2b, LQ2, QM1, DK1 and LQ5 call and LQ1
//! implements (DEC-50(10), LQ1 §2.6) with its store-independent error, the mined-commitment
//! reader (EV0 API review Q1), the list views LQ1 reads through (LQ1 §2.2), the fallible
//! chain view (EV addendum §2.2), the snapshot and commitment values the store and the effect
//! channel carry. LQ1a/b add `params`, `modifier`, `score`, `members`, `rotation`, `scan` and
//! `selection` (LQ1 §2.1); SB8 adds `commitment` (Core's `Verify`) and LQ2 `VerifyNull`. LQ2's
//! block processor lives in `dash-dml` (EV addendum §1.5).

mod chain;
mod commitment;
mod ctx;
mod members;
mod mined;
mod modifier;
mod params;
#[cfg(any(test, feature = "reference"))]
pub mod reference;
mod rotation;
mod scan;
mod score;
mod selection;
mod snapshot;
#[cfg(test)]
mod tests;
mod view;

pub use chain::{BrokenInvariant, CompleteChain, LlmqChain};
pub use commitment::{
    BlsCheck, BlsCheckRunner, CommitmentError, CommitmentReject, OperatorKeyView, SerialBlsChecks,
    SigPath, UncachedMembers, VerifiedQuorum, build_commitment_hash, commitment_version,
    queue_mined_signatures, verify_commitment, verify_mined_signatures, verify_null, verify_quorum,
    verify_sizes,
};
pub use ctx::{LlmqCtx, LlmqError, LlmqSource, ParamsOutOfDomain, SnapshotRows};
/// The values of a [`BlsCheck`], and its verdict and a [`BlsCheckRunner`]'s.
pub use dash_pkc::bls::core::{BlsPkValue, BlsSigValue, BlsVerdict};
pub use members::{
    MemberError, MembersRoute, PrecomputeMode, QuorumMemberSource, QuorumMembers, QuorumPrecompute,
    WORK_DIFF_DEPTH, compute_quorum_members, members_gate, members_route, members_uncached,
};
pub use mined::{
    MinedCommitment, MinedCommitmentReader, MinedFamily, MinedIndexEntry, base_through,
};
pub use modifier::{hash_modifier, hash_modifier_from_work_block};
pub use params::{
    TypeEnabledOverrides, cycle_base_height, enabled_quorum_params, eval_spork,
    is_quorum_rotation_enabled, is_quorum_type_enabled, is_quorum_type_enabled_with,
    signer_index_bits,
};
pub use rotation::{
    CycleQuarters, NewQuarters, PREVIOUS_CYCLES, SkipListEncoder, build_quorum_snapshot,
    compute_by_quarter_rotation, compute_quorum_members_from_work_block, decode_skip_list,
    new_quarters, quarters_by_snapshot,
};
pub use scan::{
    QuorumMaterializer, ScanCache, ScanCacheStore, ScanCacheUse, ScanEntry, ScanEvent, ScanOutcome,
    ScanOverload, Scanned, scan_cache_size, scan_quorums, scan_quorums_from, scan_store_block,
};
pub use score::{QuorumModifier, calculate_quorum, quorum_score};
pub use selection::{
    SIGN_HEIGHT_OFFSET, Selection, select_from_scan, select_quorum_for_signing, select_start_height,
};
pub use snapshot::{QuorumSnapshot, SnapshotSkipMode};
pub use view::{AddMnRejected, ListCounts, MnEntryView, MnListView, ScratchMnList};

/// An LLMQ final commitment, Core's `CFinalCommitment`: base-sdk's `Commitment` (its
/// `FinalCommitment` is Core's `CFinalCommitmentTxPayload`), whose BLS fields are A1 §1.4.1's
/// eager wrappers, decoded under the version's scheme (legacy iff version 1 or 2,
/// `llmq/commitment.h` v24:110-113; LQ2 D1).
pub type LlmqCommitment = dash_primitives::Commitment;
