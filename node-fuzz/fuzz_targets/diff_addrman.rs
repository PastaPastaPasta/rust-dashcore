//! `diff_addrman` (P2a §6.3; O1c). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_addrman");
