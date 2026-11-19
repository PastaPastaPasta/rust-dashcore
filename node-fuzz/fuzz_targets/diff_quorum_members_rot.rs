//! `diff_quorum_members_rot` (LQ1b). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_quorum_members_rot");
