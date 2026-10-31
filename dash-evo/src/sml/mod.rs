//! The simplified masternode list (Core's `CSimplifiedMNList`; SB0 §5.1): the vocabulary SPV and
//! the node share. The node projects it from its masternode list; SPV builds it from `mnlistdiff`.
//!
//! - [`entry`]: [`SmlEntry`] and its hash, the leaf of `merkleRootMNList`.
//! - [`root`]: [`SmlList`] with its cached `merkleRootMNList`, the full reference
//!   [`merkle_root_mn_list`], and `merkleRootQuorums` ([`merkle_root_quorums`]).
//! - [`score`]: masternode scores for a quorum, moved from dashcore's `sml` (SB0 §5.5).
//! - [`view`]: the list as LQ1's [`crate::llmq::MnListView`], SPV's `membersSig` input (SB8).

pub mod entry;
pub mod root;
pub mod score;
pub mod view;

pub use entry::{SML_VERSION_BASIC_BLS, SML_VERSION_EXT_ADDR, SmlEntry, write_net_info};
pub use root::{
    BlockQc, QcHashes, QuorumRootParams, SmlList, merkle_root_mn_list,
    merkle_root_mn_list_from_leaves, merkle_root_quorums, merkle_root_quorums_from_hashes,
};
pub use score::{QuorumScore, ScoredEntry, calculate_quorum, quorum_score, scores_for_quorum};
pub use view::{SmlMember, SmlScratch};
