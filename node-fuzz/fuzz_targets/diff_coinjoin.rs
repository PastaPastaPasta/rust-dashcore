//! `diff_coinjoin` (O2a-CJ1): dash-coinjoin's session-free rules (denominations, entries,
//! sessions' inputs and outputs, collaterals, queue and broadcast hashes, signatures, time bounds
//! and structure) against Core's src/coinjoin. Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_coinjoin");
