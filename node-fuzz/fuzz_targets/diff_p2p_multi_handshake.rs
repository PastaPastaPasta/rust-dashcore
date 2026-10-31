//! `diff_p2p_multi_handshake` (P2a §6.2 phase C; O1c). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_p2p_multi_handshake");
