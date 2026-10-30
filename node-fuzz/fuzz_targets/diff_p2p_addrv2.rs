//! `diff_p2p_addrv2` (P2a §6.2; P2a-net): `addrv2` messages from up to four peers. Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_p2p_addrv2");
