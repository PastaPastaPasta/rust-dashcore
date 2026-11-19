//! `diff_check_special_tx_quorum` (EV1 §6.2, EV1-5b-2): types 7 and 9 in the quorum world. Run
//! only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_check_special_tx_quorum");
