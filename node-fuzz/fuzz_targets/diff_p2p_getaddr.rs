//! `diff_p2p_getaddr` (P2a §6.2; P2a-net): `getaddr` requests over a seeded AddrMan, across the reply cache's expiry. Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_p2p_getaddr");
