//! `diff_gov` (GV1a): dash-governance's proposal validator, object hashes, local validity,
//! collateral check and trigger parse against Core's src/governance. Run only through
//! `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_gov");
