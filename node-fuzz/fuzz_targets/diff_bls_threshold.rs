//! `diff_bls_threshold` (BS11b). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_bls_threshold");
