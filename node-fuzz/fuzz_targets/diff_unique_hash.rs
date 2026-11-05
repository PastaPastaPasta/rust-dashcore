//! `diff_unique_hash` (EV2a). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_unique_hash");
