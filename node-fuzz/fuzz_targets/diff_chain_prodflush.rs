//! `diff_chain_prodflush` (O2b-4 4c): Core and rdashd's `NodeSut` compared op by op on S0p (S0 at
//! `-dbcache=4`, the node flushing in its Production regime and Core mirroring it, leg B,
//! `node_l1::chain::legb`), `chain-v2`. Built with the `chain-diff` feature. Run only through
//! `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_chain_prodflush");
