//! `undo_decode` (CH3 §7.1, a5; a model target, DEC-59 Q1). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::model_target!("undo_decode");
