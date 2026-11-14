//! `diff_mnsync` (BL-451b). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_mnsync");
