//! `diff_p2p_getdata` (P2a §6.2; P2a-core): inventories one peer asks for. Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_p2p_getdata");
