//! `diff_p2p_multi_blocks` (P3 block download; honest compact blocks only). Run only through `node-fuzz/run.sh`.

#![no_main]

node_fuzz::l1_target!("diff_p2p_multi_blocks");
