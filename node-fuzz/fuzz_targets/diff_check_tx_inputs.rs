//! `diff_check_tx_inputs` (CS5b). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_check_tx_inputs");
