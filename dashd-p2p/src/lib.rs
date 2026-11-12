//! The Rust Dash node's message processor (P2a-core; SB0 §12.1): Core's `PeerManagerImpl` and
//! `ThreadMessageHandler` over SB6's [`dash_p2p`] network core.
//!
//! - [`fingerprint`]: who the node is on the wire (DEC-83), built into SB6's `LocalIdentity`.
//! - [`processing`]: the actor loop, `ProcessMessages`/`SendMessages` in Core's order, the
//!   74-command registry with P2a's M2 stubs, misbehaviour, headers sync, block announcements,
//!   outbound eviction and the stale tip, `getpeerinfo`'s state stats, and the seams later tasks
//!   fill.
//! - `sim` (feature `sim`): a virtual clock, a header-arena chain and scripted in-memory peers.
//!
//! Core file map: `net.cpp` `ThreadMessageHandler` → [`processing::peerman`];
//! `net_processing.cpp` `ProcessMessages`, `ProcessMessage`, `SendMessages`, `ProcessGetData`
//! → [`processing::logic`]; `Misbehaving`, `MaybeDiscourageAndDisconnect`,
//! `MaybePunishNodeForBlock` → [`processing::misbehavior`]; `ProcessHeadersMessage`,
//! `MaybeSendGetHeaders`, the block `inv` path and `SendMessages` sections 6–10 →
//! [`processing::sync`], with the `headers`/`headers2` reads (`Uncompress`) in
//! [`processing::headers`]; `ConsiderEviction`, `EvictExtraOutboundPeers`,
//! `CheckForStaleTipAndEvictPeers` → [`processing::eviction`]; `GetNodeStateStats` →
//! [`processing::stats`]; `protocol.cpp` `allNetMessageTypes`
//! and the extension dispatch → [`processing::registry`]; `init.cpp` user agent and services →
//! [`fingerprint`]. Every ported function carries a `// Core:` anchor at v24.0.0-rc.3 (DEC-82).

#![forbid(unsafe_code)]

pub mod fingerprint;
pub mod processing;
#[cfg(feature = "sim")]
pub mod sim;

pub use processing::logic::{PeerLogic, ProcessorOptions, Seams};
pub use processing::peerman::{MessageLoop, PeerManager, Shared};
pub use processing::registry::{HandlerCx, MessageProcessingResult, NetHandler, Registry};
pub use processing::seams::{ChainAccess, TreeRead};
