// SPDX-License-Identifier: MIT

//! Core's 13-field coinstats `DBVal`, without a block hash or key prefix.

use std::array;

use crate::{CoinStatsEntry, IndexError};

/// A uint256 followed by twelve fixed-width, little-endian 64-bit fields.
// Core: index/coinstatsindex.cpp:31-62
pub const DBVAL_SIZE: usize = 32 + 12 * 8;

// Core: index/coinstatsindex.cpp:46-60
pub fn encode(entry: &CoinStatsEntry) -> [u8; DBVAL_SIZE] {
    let mut bytes = [0; DBVAL_SIZE];
    bytes[..32].copy_from_slice(&entry.muhash);
    let fields = [
        entry.txouts,
        entry.bogo_size,
        entry.total_amount as u64,
        entry.total_subsidy as u64,
        entry.total_unspendable as u64,
        entry.prevout_spent as u64,
        entry.new_outputs_ex_coinbase as u64,
        entry.coinbase as u64,
        entry.unspendables_genesis as u64,
        entry.unspendables_bip30 as u64,
        entry.unspendables_scripts as u64,
        entry.unspendables_unclaimed_rewards as u64,
    ];
    for (chunk, field) in bytes[32..].as_chunks_mut::<8>().0.iter_mut().zip(fields) {
        *chunk = field.to_le_bytes();
    }
    bytes
}

// Core: index/coinstatsindex.cpp:46-60
pub fn decode(bytes: &[u8]) -> Result<CoinStatsEntry, IndexError> {
    let bytes: &[u8; DBVAL_SIZE] = bytes.try_into().map_err(|_| {
        IndexError::Corrupt(format!(
            "coinstats DBVal length {}, expected {DBVAL_SIZE}",
            bytes.len()
        ))
    })?;
    let field = |i: usize| u64::from_le_bytes(array::from_fn(|j| bytes[32 + 8 * i + j]));
    Ok(CoinStatsEntry {
        muhash: array::from_fn(|i| bytes[i]),
        txouts: field(0),
        bogo_size: field(1),
        total_amount: field(2) as i64,
        total_subsidy: field(3) as i64,
        total_unspendable: field(4) as i64,
        prevout_spent: field(5) as i64,
        new_outputs_ex_coinbase: field(6) as i64,
        coinbase: field(7) as i64,
        unspendables_genesis: field(8) as i64,
        unspendables_bip30: field(9) as i64,
        unspendables_scripts: field(10) as i64,
        unspendables_unclaimed_rewards: field(11) as i64,
    })
}
