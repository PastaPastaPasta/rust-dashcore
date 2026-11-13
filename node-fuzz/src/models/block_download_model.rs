//! `block_download_model` (P3 §6.5): `dashd-p2p`'s in-flight accounting (`BlockDownload`)
//! against a transliteration of Core's structures (`dashd_p2p::sim::block_download_model` lists
//! the compared fields and the input layout).

use node_l1::Divergence;

pub fn check(data: &[u8]) -> Result<(), Divergence> {
    dashd_p2p::sim::block_download_model::check(data).map_err(|field| Divergence {
        field,
    })
}
