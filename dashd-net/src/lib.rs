//! The Rust Dash node's connection manager and address relay (P2a-net; P2a §7.3 phases D and F,
//! reshaped by SB0 §3 and §12.1) over SB6's [`dash_p2p`] network core and P2a-core's
//! [`dashd_p2p`] processor.
//!
//! - [`driver`]: the `opencon`, `addcon` and `dnsseed` OS threads driving SB6's sans-io dial
//!   steps ([`dash_p2p::NetCore::dial_tick`]).
//! - [`connman`]: `CConnman::Start`, the thread half of `Stop`, `addconnection`, `addnode
//!   onetry`, `setnetworkactive` and `GetNodeStats`.
//! - [`binds`]: `InitBinds` (v24's fatal bind rules) and the accept loop.
//! - [`stats`]: Core's `CNodeStats`.
//! - [`resolver`]: the system DNS resolver (`LookupHost`).
//! - [`store`]: `peers.dat`, `anchors.dat` and `banlist.json` files through SB6's `NetStore`.
//! - [`relay`]: `RelayAddress`, `PushAddress`, `MaybeSendAddr` and the `getaddr` response cache
//!   behind P2a-core's `AddressRelay` seam.
//! - [`bans`]: the node half of `setban`, `listbanned` and the legacy `banlist.dat` warning (P4a).
//! - [`capture`]: `-capturemessages`' files (P4a).
//! - [`torcontrol`]: Tor control: the onion proxy and the automatic onion service (P8b).
//! - `sim` (feature `sim`): the dial loops under a virtual clock with in-memory connections;
//!   `sched`, the `connman_sched` model check over it.
//!
//! Core file map: `net.cpp` `ThreadOpenConnections`, `ThreadOpenAddedConnections`,
//! `ThreadDNSAddressSeed` (steps in SB6's `dial`) → [`driver`]; `CConnman::Start`,
//! `AddConnection`, `SetNetworkActive` → [`connman`]; `InitBinds`, `Bind`, `BindListenPort`,
//! `AcceptConnection` → [`binds`]; `CNode::CopyStats`, `GetNodeStats` → [`stats`];
//! `addrdb.cpp` `SerializeFileDB` → [`store`]; `netbase.cpp` `LookupHost` → [`resolver`];
//! `net_processing.cpp` `RelayAddress`, `PushAddress`, `MaybeSendAddr`, the `getaddr` branch
//! and `net.cpp` `GetAddresses` → [`relay`]. Every ported function carries a `// Core:` anchor at v24.0.0-rc.3 (DEC-82).

#![forbid(unsafe_code)]

pub mod bans;
pub mod binds;
pub mod capture;
pub mod connman;
pub mod driver;
pub mod relay;
pub mod resolver;
#[cfg(feature = "sim")]
pub mod sched;
#[cfg(feature = "sim")]
pub mod sim;
pub mod stats;
pub mod store;
pub mod torcontrol;

pub use connman::{ConnMan, ConnManOptions, StartError};
pub use relay::NodeAddressRelay;
pub use resolver::SystemResolver;
pub use stats::NodeStats;
pub use store::FileStore;
