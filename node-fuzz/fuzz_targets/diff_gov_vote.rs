//! `diff_gov_vote` (GV1a): dash-governance's vote checks, and op scripts through Core's
//! governance manager against the node's model of it (tallies, sentinel flags, the best
//! superblock and its payments). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_gov_vote");
