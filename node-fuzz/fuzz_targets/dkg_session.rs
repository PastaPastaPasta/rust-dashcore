//! `dkg_session` (LQ4 §4.3 T5; a model target, DEC-59 Q1). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::model_target!("dkg_session");
