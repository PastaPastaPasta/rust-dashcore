//! Collateral validation: Core's `CoinJoin::IsCollateralValid` and `ATMPIfSaneFee`
//! (`src/coinjoin/coinjoin.cpp`). The mempool's test-accept is the node's: [`MempoolAccept`],
//! implemented by `dashd-coinjoin`.

use dash_consensus::money_range;
use dash_primitives::Transaction;

use crate::denom::{COIN, COLLATERAL_AMOUNT};
use crate::inouts::{CoinView, is_pay_to_public_key_hash, usable_coin};

// Core: coinjoin/coinjoin.cpp:25 DEFAULT_MAX_RAW_TX_FEE
/// The most a CoinJoin transaction may pay in fees, 0.1 DASH.
pub const MAX_RAW_TX_FEE: i64 = COIN / 10;

/// The node's mempool, as the CoinJoin checks submit to it (Core's
/// `ChainstateManager::ProcessTransaction`).
pub trait MempoolAccept {
    /// `ProcessTransaction(tx, /*test_accept=*/true)`: the base fees when the mempool would
    /// accept `tx`, `None` when it would not.
    ///
    /// [`is_collateral_valid`] applies the amount-range checks itself, so an implementation need
    /// not repeat them to keep that answer Core's. Everything else ATMP checks (scripts,
    /// standardness, conflicts, fee rate) is this implementation's, and the answer is only
    /// Core's if it is faithful to `ProcessTransaction` against a full coins view.
    fn test_accept(&self, tx: &Transaction) -> Option<i64>;
}

// Core: coinjoin/coinjoin.cpp:439 ATMPIfSaneFee
/// `ATMPIfSaneFee(tx, /*test_accept=*/true)`: the mempool would accept `tx` and its base fees are
/// at most [`MAX_RAW_TX_FEE`]. The submission itself (`test_accept == false`) is the node's: it
/// runs this first and then submits.
pub fn test_accept_if_sane_fee<M: MempoolAccept + ?Sized>(mempool: &M, tx: &Transaction) -> bool {
    mempool.test_accept(tx).is_some_and(|fee| fee <= MAX_RAW_TX_FEE)
}

// Core: coinjoin/coinjoin.cpp:458 CoinJoin::IsCollateralValid
/// `IsCollateralValid`: a collateral has outputs, no lock time, only P2PKH or unspendable
/// outputs, spends usable coins (see [`CoinView`]), pays at least [`COLLATERAL_AMOUNT`] in fees,
/// and passes [`test_accept_if_sane_fee`]. Checked in Core's order.
///
/// Every amount and running sum is held to Core's `MoneyRange` as ATMP does (outputs:
/// `consensus/tx_check.cpp:38-45` `CheckTransaction`; inputs: `consensus/tx_verify.cpp:188-191`
/// `CheckTxInputs`), so the sums cannot overflow whatever [`MempoolAccept`] does. Core returns
/// false for such a transaction too, from inside the test-accept (CJ0 notes D12).
pub fn is_collateral_valid<C: CoinView + MempoolAccept + ?Sized>(
    ctx: &C,
    tx: &Transaction,
) -> bool {
    if tx.outputs.is_empty() || tx.lock_time != 0 {
        return false;
    }

    let mut value_out: i64 = 0;
    for txout in &tx.outputs {
        // Core: consensus/tx_check.cpp:38-45 (bad-txns-vout-negative/-toolarge, -txouttotal-toolarge)
        if !money_range(txout.value) {
            return false;
        }
        value_out += txout.value;
        if !money_range(value_out) {
            return false;
        }
        let script = txout.script_pubkey.as_bytes();
        if !is_pay_to_public_key_hash(script) && !dash_consensus::coins::is_unspendable(script) {
            return false;
        }
    }

    let mut value_in: i64 = 0;
    for txin in &tx.inputs {
        let Some(coin) = usable_coin(ctx, &txin.prevout) else {
            return false;
        };
        // Core: consensus/tx_verify.cpp:188-191 (bad-txns-inputvalues-outofrange)
        if !money_range(coin.value) {
            return false;
        }
        value_in += coin.value;
        if !money_range(value_in) {
            return false;
        }
    }

    // Collateral transactions pay a small fee to the miners.
    if value_in - value_out < COLLATERAL_AMOUNT {
        return false;
    }

    test_accept_if_sane_fee(ctx, tx)
}
