//! `diff_chain_p3b` (O2b-4 4b): `diff_chain_s0` with the node's P3b planted (`InvalidateBlock`'s
//! walked descendants left out of the dirty set), S0, `chain-v2`. Built with the `planted`
//! feature. Run only through `node-fuzz/run.sh`, on a manager O2B4-RUN request.
#![no_main]

node_fuzz::l1_target!("diff_chain_p3b");
