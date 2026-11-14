//! Masternode connection selection and quorum connection sporks.

mod sets;
pub mod sporks;

pub use sets::*;

use dash_consensus::hash::sha256d;

/// Returns the proTxHash that initiates the connection, with hashes in Core's stored byte order.
// Core: llmq/utils.cpp:732-752
pub fn deterministic_outbound(a: [u8; 32], b: [u8; 32]) -> [u8; 32] {
    // Core: uint256.h:55-59 compares the stored bytes with memcmp.
    let (lo, hi) = if a < b {
        (a, b)
    } else {
        (b, a)
    };
    let mut data = [0; 96];
    data[..32].copy_from_slice(&lo);
    data[32..64].copy_from_slice(&hi);
    data[64..].copy_from_slice(&a);
    let h1 = sha256d(&data);
    data[64..].copy_from_slice(&b);
    let h2 = sha256d(&data);
    if h1 < h2 {
        a
    } else {
        b
    }
}
