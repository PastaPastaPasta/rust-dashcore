//! Core's special-transaction contextual checks (EV1, `designs/EV1-specialtx.md`): what Core's
//! `CheckSpecialTx` decides before a block or the mempool accepts a special transaction.
//!
//! Every check returns `Result<(), TxValidationError>` (CS0) with Core's class and a reason from
//! [`reasons`]. EV1-1 lands the reasons and the provider network fields ([`network`]); EV1-2 the
//! validated payloads ([`payload`]), the trivial checks ([`trivial`]) and the signed preimages
//! ([`sig`]); the per-type modules follow in EV1-3 to EV1-5 (EV1 §7). LQ2 lands type 6 ([`qc`],
//! LQ2 D3).

pub mod network;
pub mod payload;
pub mod qc;
pub mod reasons;
pub mod sig;
pub mod trivial;

pub use network::{
    ProviderNetworkFields, check_net_info, check_provider_network_fields,
    is_net_info_trivially_valid,
};
pub use payload::{BlockPayloads, PayloadSlot, ProTxPayload, validated_payload, version_max};
