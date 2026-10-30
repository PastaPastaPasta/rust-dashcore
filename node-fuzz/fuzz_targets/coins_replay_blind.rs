//! `coins_replay_blind` (CH2 §6.5, D-c6-10; a model target, DEC-59 Q1). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::model_target!("coins_replay_blind");
