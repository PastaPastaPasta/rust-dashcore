//! `diff_chain_av` (O2b-4 4d): Core and rdashd's `NodeSut` compared op by op on S2 (S1 with the
//! assumevalid line, `node_l1::chain::av`), `chain-v2`. Built with the `chain-diff` feature. Run
//! only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_chain_av");
