//! `diff_p2p_addr` (P2a §6.2; P2a-net): `addr` messages from up to four peers. Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_p2p_addr");
