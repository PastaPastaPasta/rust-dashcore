//! `diff_p2p_selfaddr` (P2a-net BL-203): `diff_p2p_addr`'s scripts out of IBD, with each peer's
//! `addrMe`, so the node advertises its own address. Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_p2p_selfaddr");
