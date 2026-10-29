//! The network core shared by the Rust Dash node and dash-spv (SB0 §3.1; PLAN SB6).
//!
//! One [`NetCore`] per process and network owns:
//! - the connection table (Core's `m_nodes`), with per-peer receive queues bounded by
//!   `-maxreceivebuffer` (`fPauseRecv`) and per-peer send queues under Core's soft
//!   `-maxsendbuffer` limit (`fPauseSend`; [`NetCore::push`] never rejects a message);
//! - the per-peer reader and writer tasks, spawned on the host's tokio [`Handle`];
//! - framing and transport through `dash-p2p-transport` (P1);
//! - the handshake functions ([`handshake`]), the pre-handshake gates ([`gates`]) and the decode
//!   policy ([`policy`]), with [`LocalIdentity`] as a parameter;
//! - ping and the inactivity timeouts ([`ping`]) and addr intake ([`addr`]);
//! - [`addrman::AddrMan`] (Core's bucket math) and [`ban::BanList`] (ban and discourage),
//!   persisted through [`NetStore`];
//! - the dial steps as sans-io functions returning a [`dial::DialPlan`] ([`dial`]);
//! - DNS and fixed seeds ([`seeds`]), resolved through the host's [`seeds::Resolver`];
//! - SOCKS5 proxies per network ([`proxy`]), for Tor and `-proxy`;
//! - with the `inbound` feature, the listener, inbound limits and `AttemptToEvictConnection`,
//!   whose ties follow libstdc++'s `std::sort` ([`stdsort`]).
//!
//! `NetCore` moves raw frames: each processor (the node's `PeerManager` in `dashd-p2p`, SPV's
//! `SpvRouter` in dash-spv) decodes a payload once, after the gates, into its own type. The
//! crate creates no thread, runtime or pool and does no blocking I/O of its own (SB0 rule L5):
//! sockets run on the injected runtime, DNS goes through the injected resolver and persistence
//! through the injected store.
//!
//! Every ported function carries a `// Core: <file>:<line> <Function>` anchor into Dash Core
//! v24.0.0-rc.3, the sole parity reference (DEC-82).
//!
//! [`Handle`]: tokio::runtime::Handle

// One audited exception: `lookup::numeric_host` calls the platform's `getaddrinfo` (P4a D20).
#![deny(unsafe_code)]

pub mod addr;
pub mod addrman;
pub mod ban;
pub mod bloom;
pub mod capture;
pub mod conn;
pub mod dial;
pub mod gates;
pub mod handshake;
#[cfg(feature = "inbound")]
pub mod inbound;
pub mod local;
pub mod lookup;
pub mod netaddr;
mod netcore;
pub mod netgroup;
#[cfg(feature = "inbound")]
pub mod permissions;
pub mod ping;
pub mod policy;
pub mod proxy;
pub mod rng;
pub mod seeds;
pub mod siphash;
pub mod sock;
pub mod stdsort;
pub mod store;

#[cfg(test)]
mod names_tests;
#[cfg(test)]
mod proxy_open_tests;

pub use conn::{
    ConnectionType, PauseState, Peer, PeerHandle, PeerId, PeerSnapshot, Permissions, Why,
};
pub use dash_clock::Clock;
pub use dash_p2p_core::{CommandString, ProtocolVersion, ServiceFlags};
pub use dash_p2p_transport::OutMsg;
pub use dial::{DialLoop, DialPlan, DialPolicy, Grant};
pub use handshake::LocalIdentity;
pub use netaddr::{Address, NetAddr, Network, Service, SubNet};
pub use netcore::{
    Attach, InitError, NetConfig, NetCore, NetEvents, OpenError, OpenRequest, Polled, RawMsg,
    Target,
};
pub use policy::Step;
pub use seeds::Resolver;
#[cfg(unix)]
pub use sock::UnixSocket;
pub use sock::{MemSocket, NetSocket, TcpSocket};
pub use store::{NetBlob, NetStore};

const _: () = {
    const fn send_sync<T: Send + Sync>() {}
    send_sync::<NetCore>();
    send_sync::<Peer>();
    send_sync::<NetConfig>();
};
