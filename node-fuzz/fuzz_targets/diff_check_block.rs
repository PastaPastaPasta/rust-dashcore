//! `diff_check_block` (CS3). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_check_block");
