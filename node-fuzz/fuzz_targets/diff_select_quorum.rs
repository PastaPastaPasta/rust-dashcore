//! `diff_select_quorum` (LQ1a). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_select_quorum");
