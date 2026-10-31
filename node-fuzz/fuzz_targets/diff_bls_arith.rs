//! `diff_bls_arith` (BS11a-3). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_bls_arith");
