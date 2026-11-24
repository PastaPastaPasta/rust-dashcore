//! `connman_sched` (P2a-net): the node's `opencon` loop over SB6's dial steps, scripted in
//! `dashd-net`'s dial `sim`, against `ThreadOpenConnections`' slot, netgroup and address rules
//! (`dashd_net::sched` lists them and the input layout).

use node_l1::Divergence;

pub fn check(data: &[u8]) -> Result<(), Divergence> {
    dashd_net::sched::check(data).map_err(|field| Divergence {
        field,
    })
}
