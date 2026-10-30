//! `disconnect_pool` (CH3b b1; a model target, DEC-59 Q1). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::model_target!("disconnect_pool");
