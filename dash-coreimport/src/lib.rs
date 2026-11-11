//! Read-only readers for a Dash Core datadir (A1 §2.21).
//!
//! This first slice covers what the ST1 storage bake-off needs:
//! - [`blockfiles`]: `blk?????.dat` / `rev?????.dat` record scanning with Core's `-reindex`
//!   semantics, a header index over the block files, and the best chain to an anchor hash;
//! - [`tx`]: a zero-copy block and transaction reader (Core's wire layout, DIP2 payloads);
//! - [`coin`]: Core's `Coin` serialization (amount and script compression) and `CBlockUndo`;
//! - `evodb`: masternode lists rebuilt from Core's evodb rows, as `GetListForBlock` does;
//! - [`netstate`]: detection and parsing of Core's net-state files (`peers.dat`, `anchors.dat`,
//!   `banlist.json`, `mempool.dat`, `fee_estimates.dat`, `settings.json`);
//! - [`trace`]: the UTXO churn trace format and its [`extract`](trace::extract)or, which replays
//!   the chain against an in-memory UTXO set and checks every spent coin against Core's undo data.
//!
//! Everything here only reads the source datadir.

pub mod blockfiles;
pub mod coin;
pub mod config;
mod error;
#[cfg(feature = "full")]
pub mod evodb;
pub mod keys;
pub mod netstate;
pub mod trace;
pub mod tx;

pub use error::{Error, Result};

/// A 32-byte hash in internal (little-endian) byte order.
pub type Hash256 = [u8; 32];

/// The display (RPC) form of a hash: byte-reversed hex.
pub fn hash_to_hex(hash: &Hash256) -> String {
    hash.iter().rev().map(|b| format!("{b:02x}")).collect()
}

/// Parses the display (RPC) form of a hash.
pub fn hash_from_hex(hex: &str) -> Option<Hash256> {
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().rev().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_hex_round_trip() {
        // Dash mainnet genesis.
        let hex = "00000ffd590b1485b3caadc19b22e6379c733355108f107a430458cdf3407ab6";
        let hash = hash_from_hex(hex).unwrap();
        assert_eq!(hash[31], 0x00);
        assert_eq!(hash[0], 0xb6);
        assert_eq!(hash_to_hex(&hash), hex);
        assert_eq!(hash_from_hex("00"), None);
    }
}
