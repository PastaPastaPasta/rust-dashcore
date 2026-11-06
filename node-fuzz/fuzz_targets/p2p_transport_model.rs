//! `p2p_transport_model` (P1; a model target, DEC-59 Q1). Run only through `node-fuzz/run.sh`.
#![no_main]

// Counts the receive buffer's growth for the model's allocation bound.
#[global_allocator]
static ALLOC: node_fuzz::models::CountingAlloc = node_fuzz::models::CountingAlloc;

node_fuzz::model_target!("p2p_transport_model");
