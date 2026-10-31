//! `p2p_decode_model` (SB6; a model target, DEC-59 Q1). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::model_target!("p2p_decode_model");
