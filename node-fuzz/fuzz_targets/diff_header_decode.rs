// EMBARGO-1
//! `diff_header_decode` (CS0 §9.3). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_header_decode");
