//! `diff_p2p_inv` (P2a §6.2; P2a-core): inventories announced by one peer. Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_p2p_inv");
