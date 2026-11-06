//! `diff_sequence_locks` (CS5b). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_sequence_locks");
