//! `diff_bls_decode` (BS11a-1). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_bls_decode");
