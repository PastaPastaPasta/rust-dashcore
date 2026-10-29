//! `packed_coin_roundtrip` (CH2a; a model target, DEC-59 Q1). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::model_target!("packed_coin_roundtrip");
