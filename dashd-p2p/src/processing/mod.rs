//! The node's message processor (P2a §2.3 `processing/`): Core's `PeerManagerImpl` over SB6's
//! network core.

pub mod blocks;
pub mod bloom;
pub mod compact;
pub mod download;
pub mod eviction;
pub mod headers;
pub mod logic;
pub mod merkle;
pub mod misbehavior;
pub mod peer;
pub mod peerman;
pub mod registry;
pub mod relay;
pub mod seams;
pub mod serve;
pub mod serve_blocks;
pub mod signals;
pub mod solicit;
pub mod stats;
pub mod stubs;
pub mod sync;
pub mod txrequest;
