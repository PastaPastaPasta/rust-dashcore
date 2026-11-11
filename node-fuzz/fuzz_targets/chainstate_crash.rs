//! `chainstate_crash` (CH2 §6.5, b3; a model target, DEC-59 Q1). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::model_target!("chainstate_crash");
