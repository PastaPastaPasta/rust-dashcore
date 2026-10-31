//! `cmpct_reconstruct` (P3 §6.5; a model target, DEC-59 Q1). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::model_target!("cmpct_reconstruct");
