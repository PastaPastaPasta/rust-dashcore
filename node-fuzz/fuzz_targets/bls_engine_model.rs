//! `bls_engine_model` (BS11a-3; a model target, DEC-59 Q1). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::model_target!("bls_engine_model");
