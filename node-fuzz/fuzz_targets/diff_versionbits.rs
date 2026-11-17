//! `diff_versionbits` (CS4). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_versionbits");
