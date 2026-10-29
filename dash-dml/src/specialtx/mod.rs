//! Core's special-transaction contextual checks (EV1, `designs/EV1-specialtx.md`): what Core's
//! `CheckSpecialTx` decides before a block or the mempool accepts a special transaction.
//!
//! Every check returns `Result<(), TxValidationError>` (CS0) with Core's class and a reason from
//! [`reasons`]. EV1-1 lands the reasons and the provider network fields ([`network`]); the
//! payload, trivial, per-type and signature modules follow in EV1-2 to EV1-5 (EV1 §7). LQ2 lands
//! type 6 ([`qc`], LQ2 D3).

pub mod network;
pub mod qc;
pub mod reasons;

pub use network::{
    ProviderNetworkFields, check_net_info, check_provider_network_fields,
    is_net_info_trivially_valid,
};
