//! `diff_p2p_multi_headers` (P2a §6.2 joint E/F gate; O1c). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_p2p_multi_headers");
