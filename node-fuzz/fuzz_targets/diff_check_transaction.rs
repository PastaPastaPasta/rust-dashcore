//! `diff_check_transaction` (CS3). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_check_transaction");
