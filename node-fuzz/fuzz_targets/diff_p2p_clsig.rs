//! `diff_p2p_clsig` (O2c). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_p2p_clsig");
