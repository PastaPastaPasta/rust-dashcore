//! `diff_chain_par_p3a` (O2b-4 4b): `diff_chain_par` with the node's P3a planted (the check
//! queue's verdict for a block whose transaction loop stopped early), S1, `chain-v2`. Built with
//! the `planted` feature. Run only through `node-fuzz/run.sh`, on a manager O2B4-RUN request.
#![no_main]

node_fuzz::l1_target!("diff_chain_par_p3a");
