//! `diff_scan_quorums` (LQ1a). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_scan_quorums");
