//! `diff_dml_apply` (EV2b). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_dml_apply");
