//! `diff_type_enabled` (LQ1a). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_type_enabled");
