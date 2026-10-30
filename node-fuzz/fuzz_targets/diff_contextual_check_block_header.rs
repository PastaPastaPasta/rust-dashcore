//! `diff_contextual_check_block_header` (CS3). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_contextual_check_block_header");
