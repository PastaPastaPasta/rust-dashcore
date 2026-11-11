//! V tests.

// Test gates and waits use raw channels; LO-9's ban is for the crate's own wait paths.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]

mod abc;
mod actor;
mod attempt;
mod blocks;
mod connect;
mod connector;
mod deployments;
#[cfg(feature = "statedigest")]
mod digest_tap;
mod dmn;
mod ehf;
mod ehf_faults;
mod evo_chain;
mod evo_chain_cl;
mod evo_edges;
mod failure;
mod flush;
mod h4_qc_members;
mod index;
mod llmq;
mod llmq_lookup;
mod llmq_members;
mod llmq_quorum;
mod locks;
mod mineable;
mod node_evo;
mod node_evo_bls;
mod node_evo_facade;
mod node_evo_lists;
mod node_stores;
mod on_v;
mod on_v_binding;
mod payloads;
mod proposal_faults;
mod queue_hooks;
mod read_ahead;
mod read_faults;
mod rotated_reconnect;
mod route_a_mined;
mod routes;
mod scan_routes;
mod scheme_guard;
// CH7a-5b: rig A/B, whose connectors write state-hash records.
#[cfg(all(feature = "statedigest", not(any(miri, loom))))]
mod scratch_state_hash;
mod signals;
mod stop;
mod support;
mod undo_mined;
mod v19_window;
mod verify;
mod view;
