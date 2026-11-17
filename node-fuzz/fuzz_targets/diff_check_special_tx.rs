//! `diff_check_special_tx` (EV1 §6.2, EV1-3). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_check_special_tx");
