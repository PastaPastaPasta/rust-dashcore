//! `diff_p2p_headers` (P2a §6.2; P2a-core): runs of `headers` from one peer. Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_p2p_headers");
