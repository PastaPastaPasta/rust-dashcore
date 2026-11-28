//! `diff_chain_s0` (O2b-4 4a): Core and rdashd's `NodeSut` compared op by op on S0,
//! `chain-v2`. Built with the `chain-diff` feature. Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_chain_s0");
