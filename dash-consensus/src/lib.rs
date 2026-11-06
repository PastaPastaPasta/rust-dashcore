//! Dash Core's consensus layer for the Rust Dash node (node program, A1 §2.4).
//!
//! Global allocator: the gate proves that none comes from this crate only for the products
//! `contrib/node/layers.toml` registers (`[[product]]`, DEC-181); other consumers are out of scope.
//!
//! The crate is pure: no I/O, clocks, threads or globals (A1 §1.5, §2.1). Every ported function
//! carries a `// Core:` anchor to Dash Core v24.0.0-rc.3 (DEC-82: v24 only; A1 Appendix E).
//!
//! - [`primitives`]: Core's canonical serialization ([`core_serialize`], the only encoder node
//!   code uses for transactions, headers and blocks), the [`Tx`], [`Header`] and [`Block`]
//!   wrappers with their cached hashes, the decoders, and [`CoreBytes`].
//! - [`merkle`]: Core's `ComputeMerkleRoot`, with its mutation flag.
//! - [`validation`]: [`ValidationError`] with Core's result classes, reject reasons and debug
//!   messages.
//! - [`ctx`]: [`ConsensusCtx`] and the chain, deployment and checkpoint views later tasks
//!   implement.
//! - [`deployments`]: buried and versionbits deployments (Dash's dynamic threshold and EHF
//!   signal height), `ComputeBlockVersion` and `getdeploymentinfo`'s report (CS4).
//! - [`coins`]: Core's `Coin` and undo-record serializations, the amount and script compression,
//!   [`coins::CoinsView`], and the coin statistics of the coinstats index and `getblockstats`.
//! - [`hash`]: the crate's hash functions, the one place that picks their backends.
//! - [`pow`]: `GetNextWorkRequired` (BTC retarget, KGW, DGW, the min-difficulty rules),
//!   `CheckProofOfWork` and the `nBits` rule of `ContextualCheckBlockHeader`.
//! - [`subsidy`]: the block subsidy and its treasury share, and the superblock payments limit.
//! - [`payments`]: the masternode payment schedule, the platform share and the owner/operator
//!   reward arithmetic.
//! - [`params`]: what their parameter views are built from: `dash-params`' `ChainSetup` and the
//!   devnet arguments.
//! - [`fp`]: Core's consensus floating point, which [`pow`] and [`subsidy`] call.
//! - [`script`]: Dash Core's script system: the interpreter with the DIP0020 opcodes and
//!   `OP_CHECKDATASIG`, script numbers, signature and public-key encoding checks, the ECDSA
//!   consensus layer and sigop counting (CS1).
//! - [`tx`], [`header`], [`block`]: Core's context-free and contextual transaction, header and
//!   block checks (`CheckTransaction`, `ContextualCheckTransaction`, `IsFinalTx`,
//!   `CheckBlockHeader`, `ContextualCheckBlockHeader`, `CheckBlock`, `ContextualCheckBlock`),
//!   with Core's reject reasons (CS3).
//! - [`connect`]: `ConnectBlock`'s pure rules: the block's script and lock-time flags, the BIP30
//!   exemption, the assumevalid decision and `HasLockInputs` (CH3a).
//!
//! See `ARCHITECTURE.md` for the rules other crates follow when they use these types.
//!
//! ## Floating point
//!
//! Core computes the pre-V20 subsidy, KGW's horizon and the mainnet ≤ 68589 `nBits` tolerance in
//! `double`. That code is in [`fp`], the only module allowed float arithmetic: the crate denies it
//! everywhere else, `clippy.toml` bans the `f32`/`f64` methods that call libm,
//! `tests/fp_confinement.rs` keeps `f32`/`f64` and their ordering from leaving `fp` and `%`
//! (libm's `fmod`) out of it, and `contrib/node/check-no-libm.sh` refuses any libm call in the
//! compiled crate (CS2 §2.2, §2.6).

#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

pub mod block;
pub mod coins;
pub mod connect;
pub mod ctx;
pub mod deployments;
// The only module with floating point (CS2 §2.6): the crate denies it everywhere else.
#[allow(clippy::float_arithmetic)]
pub mod fp;
pub mod hash;
pub mod header;
pub mod merkle;
pub mod params;
pub mod payments;
pub mod pow;
pub mod primitives;
pub mod script;
pub mod subsidy;
pub mod tx;
pub mod validation;

pub use ctx::{BlockId, BlsCtx, ChainAncestry, ConsensusCtx, HeaderCtx};
pub use params::ParamsError;
pub use primitives::{
    Block, CoreBytes, CoreSerialize, CoreWrite, DecodeError, DecodeErrorKind, Decoded, Header, Tx,
    TxRef, core_bytes, core_serialize, decode_block, decode_header, decode_tx, decode_tx_exact,
};
pub use validation::{
    BlockValidationError, BlockValidationResult, TxValidationError, TxValidationResult,
    ValidationError,
};

/// Core's `CAmount`: a signed amount of duffs.
pub type Amount = i64;

/// Core's `MAX_MONEY`.
// Core: consensus/amount.h:26 MAX_MONEY
pub const MAX_MONEY: Amount = 21_000_000 * subsidy::COIN;

/// Core's `MoneyRange`.
// Core: consensus/amount.h:27 MoneyRange
pub fn money_range(value: Amount) -> bool {
    (0..=MAX_MONEY).contains(&value)
}

/// Core's `FormatMoney`: `<coins>.<duffs>` with the trailing zeros trimmed down to two decimals,
/// and a leading `-` for a negative amount. Reject debug messages print amounts with it.
// Core: util/moneystr.cpp:16 FormatMoney
pub fn format_money(n: Amount) -> String {
    let (mut quotient, mut remainder) = (n / subsidy::COIN, n % subsidy::COIN);
    if n < 0 {
        quotient = -quotient;
        remainder = -remainder;
    }
    let mut s = format!("{quotient}.{remainder:08}");
    // Right-trim zeros while the character two to the left is still a digit.
    while s.ends_with('0') && s.as_bytes()[s.len() - 3].is_ascii_digit() {
        s.pop();
    }
    if n < 0 {
        s.insert(0, '-');
    }
    s
}
