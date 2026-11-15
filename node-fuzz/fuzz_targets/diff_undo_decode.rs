//! `diff_undo_decode` (CS5a). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_undo_decode");
