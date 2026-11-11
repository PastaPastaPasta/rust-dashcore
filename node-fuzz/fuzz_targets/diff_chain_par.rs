//! `diff_chain_par` (O2b-4 4a): Core and rdashd's `NodeSut` compared op by op on S1 (-par=4),
//! `chain-v2`. Built with the `chain-diff` feature. Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_chain_par");
