//! `cmpct_reconstruct` (P3 §6.5): `cmpctblock` and `blocktxn` against `dashd-p2p`'s
//! `PartiallyDownloadedBlock`, checked by invariants (`dashd_p2p::sim::cmpct_reconstruct` lists
//! them and the input layout).

use node_l1::Divergence;

pub fn check(data: &[u8]) -> Result<(), Divergence> {
    dashd_p2p::sim::cmpct_reconstruct::check(data).map_err(|field| Divergence {
        field,
    })
}
