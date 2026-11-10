//! Dash Core's CoinJoin rules for the Rust Dash node (node program PLAN row CJ0; PRD FR-CJS-02/05,
//! FR-CJC-02/04): what a mixing masternode and a mixing client check, decide and relay.
//!
//! - [`denom`]: denominations, collateral amounts and the post-V24 rebalance decisions
//!   (`coinjoin/common.{h,cpp}`).
//! - [`pool`]: the session vocabulary both sides share: pool messages, states, timeouts and the
//!   `dsa` rebalance flags.
//! - [`inouts`], [`collateral`]: entry, final-transaction and collateral validation
//!   (`IsValidInOuts`, `IsCollateralValid`); the mempool's test-accept is the node's
//!   [`collateral::MempoolAccept`], implemented by `dashd-coinjoin`.
//! - [`queue`]: queues (`dsq`), the queue manager, the masternode mixing counters, and what a
//!   masternode and a client do with a received queue.
//! - [`dstx`], [`relay`]: mixing transactions (`dstx`): structure, expiry, the DSTX store,
//!   `ValidateDSTX`, and the queue and DSTX relay rules the peer manager (P6a) applies.
//! - [`server`]: the mixing masternode's session.
//! - [`client`]: the client session's transitions; [`rounds`]: mixing rounds.
//!
//! The wire messages are base-sdk's (`dash_p2p_core`, BS9b); this crate adds the rules over them.
//! Lookups the node owns (coins, the mempool's test-accept, the masternode list, the wallet) are
//! traits the caller implements.
//!
//! Shared with the wallets (layer P, SB0 §2.1; CJ0 notes D17). The crate is pure: no I/O,
//! clocks, threads or globals (A1 §1.5, §2.1); time and randomness come in as arguments. Every ported function carries a
//! `// Core:` anchor to Dash Core v24.0.0-rc.3, the one parity reference (DEC-82).

#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

pub mod client;
pub mod collateral;
pub mod denom;
pub mod dstx;
pub mod inouts;
pub mod pool;
pub mod queue;
pub mod relay;
pub mod rounds;
pub mod server;
