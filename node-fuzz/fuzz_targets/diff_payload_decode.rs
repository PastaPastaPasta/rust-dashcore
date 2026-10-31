// EMBARGO-1
//! `diff_payload_decode` (BS4b). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_payload_decode");
