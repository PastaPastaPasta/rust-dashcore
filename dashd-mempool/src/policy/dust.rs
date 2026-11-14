//! Dust thresholds in duffs, with fee rates in duffs per kB.

use dash_consensus::Amount;
use dash_consensus::coins::is_unspendable;
use dash_consensus::primitives::compact_size_len;
use dash_primitives::TxOut;

/// `GetDustThreshold`.
// Core v24.0.0: policy/policy.cpp:29-46
pub fn get_dust_threshold(output: &TxOut, dust_relay_fee: Amount) -> Amount {
    let script = output.script_pubkey().as_bytes();
    if is_unspendable(script) {
        return 0;
    }
    let size = 8 + compact_size_len(script.len() as u64) + script.len() + 148;
    get_fee(dust_relay_fee, size as u32)
}

/// `IsDust`: equality with the threshold is spendable.
// Core v24.0.0: policy/policy.cpp:48-50
pub fn is_dust(output: &TxOut, dust_relay_fee: Amount) -> bool {
    output.value() < get_dust_threshold(output, dust_relay_fee)
}

// Core v24.0.0: policy/feerate.cpp:23-37 CFeeRate::GetFee
fn get_fee(duffs_per_kb: Amount, size: u32) -> Amount {
    // Core multiplies the integers before its binary64 division and ceil.
    let fee = (duffs_per_kb.wrapping_mul(i64::from(size)) as f64 / 1000.0).ceil() as Amount;
    if fee == 0 && size != 0 {
        duffs_per_kb.signum()
    } else {
        fee
    }
}
