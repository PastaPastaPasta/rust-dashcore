//! `diff_chain` (O2b-3a: Core alone until O2b-3b). Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_chain");
