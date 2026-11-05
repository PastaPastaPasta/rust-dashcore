//! `connect_disconnect_roundtrip` (CH3 §7.1, D48; CH3a a5): a random small regtest block over a
//! random UTXO set, connected and disconnected by the real core, with random damage to the coins
//! or the undo record in between, against a reference of Core's `DisconnectBlock`. The model is
//! [`dashd_chainstate::crash::connect_disconnect_roundtrip`], which reaches the core.

use node_l1::Divergence;

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    dashd_chainstate::crash::connect_disconnect_roundtrip(data).map_err(|field| Divergence {
        field,
    })
}
