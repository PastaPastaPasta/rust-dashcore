//! Virtual transaction size under the pool's fixed sigop pricing option.

use dash_consensus::Tx;

/// `GetVirtualTransactionSize(size, sigops, bytes_per_sigop)`. The option has already
/// undergone Core's unsigned conversion during argument parsing.
// Core v24.0.0: policy/policy.cpp:207-210
pub fn get_virtual_transaction_size(size: i64, sigops: i64, bytes_per_sigop: u32) -> i64 {
    size.max(sigops.wrapping_mul(i64::from(bytes_per_sigop)))
}

/// `GetVirtualTransactionSize(tx, sigops, bytes_per_sigop)`.
// Core v24.0.0: policy/policy.cpp:212-215
pub fn get_virtual_transaction_size_for_tx(tx: &Tx, sigops: i64, bytes_per_sigop: u32) -> i64 {
    get_virtual_transaction_size(i64::from(tx.size()), sigops, bytes_per_sigop)
}
