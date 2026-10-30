//! Dash Core's authoritative evo state for the Rust Dash node (node program, A1 §2.5; EV
//! addendum §1): the deterministic masternode list (EV2, `designs/EV2-dml.md`), the evo store and
//! its fail-stop error, the per-block records, the credit pool and the MNHF signals.
//!
//! Global allocator: the gate proves that none comes from this crate only for the products
//! `contrib/node/layers.toml` registers (`[[product]]`, DEC-181); other consumers are out of scope.
//!
//! Node-only (layer Pn): everything here depends on a complete chain and a complete evodb. What
//! the node and SPV share (the views LQ1 reads through, the member-source trait, the effect
//! channel, the BLS context, `ProTxHash`/`MnType`) is `dash_evo`; SPV never names this crate.
//!
//! The crate is pure: no I/O, clocks, threads or globals (A1 §1.5, §2.1). Every ported function
//! carries a `// Core:` anchor to Dash Core, the one parity reference (DEC-82; A1 Appendix E):
//! v24.0.0-rc.4 since DEC-458. The rules rc.4 changed (EV-RC4) cite rc.4 (`// Core (rc.4):`, or
//! the module says so); the other anchors keep their v24.0.0-rc.3 lines.
//!
//! EV0 lands the types, the read API (EV2 §2.3-§2.6, §2.8, §2.10) and LQ1's list views over
//! them (LQ1 §2.2.4; EV addendum §3 commit 1); EV2a and EV2b add the mutations, `core_order`, the
//! diff functions, the codecs, `resolve_list` and the block builder in the modules EV2 §2.1
//! lists.
//!
//! - The DML: [`MnList`], [`Dmn`], the unique-property keys, the diffs and the per-block DML
//!   records; [`SameRecord`] compares them.
//! - [`store`]: the evo store ([`store::EvoStore`], ancestry-resolved for mined commitments), the
//!   transactional per-block [`store::EvoDelta`], the in-memory [`store::MemEvoStore`], and the
//!   node's member-error mapping ([`store::NodeMemberError`]).
//! - The simplified list (EV4-serve): [`Dmn::to_sml_entry`], [`MnList::project_sml`] and
//!   `merkleRootMNList` kept across blocks ([`SmlRootCache`]); [`mnlistdiff`], the `mnlistdiff`
//!   message's builder and encoder.
//! - [`payments`]: the masternode payment a coinbase owes (EV3).
//! - [`creditpool`], [`mnhf`]: the per-block records EV3 computes.
//! - [`netinfo`], [`specialtx`]: Core's NetInfo rules and the special-transaction checks (EV1,
//!   `designs/EV1-specialtx.md`).
//! - `testkit` (feature `test-utils`): masternode builders.
//!
//! See `ARCHITECTURE.md` for the rules other crates follow when they use these types.

#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

pub mod blockprocessor;
mod build;
pub mod codec;
pub mod creditpool;
mod diff;
mod index;
mod list;
pub mod mnhf;
pub mod mnlistdiff;
pub mod netinfo;
mod order;
mod payee;
pub mod payments;
mod pose;
mod process;
mod record;
#[cfg(any(test, feature = "xcheck"))]
pub mod reference;
mod scan;
mod sml;
pub mod snapshot;
pub mod specialtx;
pub mod store;
#[cfg(any(test, feature = "test-utils"))]
pub mod testkit;
mod trace;
mod types;
mod unique;
mod views;

pub use build::{
    BuiltList, DML_PREV_LIST_MISMATCH, DmlBuildError, DmlException, DmlParams, DmlRules,
    FAILED_PROCSPECTXSINBLOCK, NoCoins, PrevBlock, build_new_list_from_block, payload,
    payload_entries, unknown_pro_tx_version,
};
pub use dash_evo::types::{MnType, ProTxHash, RawMnType};
pub use diff::{DmlDelta, MnListDiff, MnListDiffRecord, MnStateDiff};
pub use list::{InvariantViolation, MnList, MnListError};
pub use payee::PayeeRules;
pub use process::{MnListChange, ProcessedList, process_block, undo_block};
pub use record::SameRecord;
pub use sml::{SmlRootCache, merkle_root_mn_list_full};
pub use store::{
    DmlMemberSource, DmlReader, EvoDelta, EvoStore, MemEvoStore, NodeMemberError, StoreError,
};
pub use trace::{DmlEvent, DmlTrace, NoTrace};
pub use types::{
    Counts, Dmn, KeyId, MnCold, MnHot, OperatorKey, OperatorPoint, PayeeKey, Provenance,
    ScriptBytes,
};
pub use unique::{UniqueKey, UniqueMap, UniqueSlot};
pub use views::UsedSet;

#[cfg(any(test, feature = "test-utils"))]
pub(crate) use types::ColdDerived;
