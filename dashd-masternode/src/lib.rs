//! The Rust Dash node's masternode-network services (A1 §2.6 `dashd-masternode`).
//!
//! - [`sporks`]: the spork manager (SP0), Core's `CSporkManager` over `dash_evo::spork`: the
//!   signer key, the store and its file, the `spork`/`getsporks` handler for P2a-core's registry,
//!   and the chain's [`dash_chain::SporkView`].
//! - [`sync`] and [`sync_manager`]: the masternode sync (SP1), Core's `CMasternodeSync` and the
//!   `SyncManager` tick, with the `ssc` handler and the network view in `sync_net` (feature
//!   `net`).
//! - [`netfulfilled`]: Core's `CNetFulfilledRequestManager` and `netfulfilled.dat` (SP1).
//! - [`meta`]: Core's `CMasternodeMetaMan` and `mncache.dat`, with the platform-ban reset (SP1).
//! - [`flatdb`]: Core's `CFlatDB` for rdashd's own flat files.
//!
//! Every ported function carries a `// Core:` anchor. SP0's are at v24.0.0-rc.3 (DEC-82; the
//! spork code is unchanged up to the oracle pin); SP1's are at the pin 375c64d2317 (v24.0.0-rc.4,
//! whose sync, netfulfilled and meta sources are the tag's).

#![forbid(unsafe_code)]

pub mod flatdb;
pub mod meta;
pub mod netfulfilled;
pub mod sporks;
pub mod sync;
pub mod sync_manager;
#[cfg(feature = "net")]
pub mod sync_net;
