//! `coins_cache_model` (CH2a; a model target, DEC-59 Q1). Run only through `node-fuzz/run.sh`.
//!
//! The binary installs the tagging allocator, so the model checks the coins budget's bytes
//! against the allocator's after every op (CH2 §6.7).
#![no_main]

#[global_allocator]
static ALLOC: dashd_chainstate::alloc_tag::TaggingAllocator =
    dashd_chainstate::alloc_tag::TaggingAllocator;

node_fuzz::model_target!("coins_cache_model");
