//! The shared-collateral covenant (EV1 §3.17): the template script is anyone-can-spend at the
//! script layer, so two consensus rules guard it. Only a ProDisTx spends a template output, and
//! only a shared ProRegTx's own collateral output pays one.

use core::fmt::Debug;

use dash_consensus::coins::CoinsView;
use dash_consensus::coins::compress::MAX_SPECIAL_SCRIPT_LEN;
use dash_consensus::primitives::Tx;
use dash_consensus::validation::TxValidationError;
use dash_primitives::{ProRegTx, TxType};

use super::payload::{PayloadSlot, ProTxPayload};
use super::reasons::{BAD_SHARED_COLLATERAL_CREATE, BAD_SHARED_COLLATERAL_SPEND};
use super::{CheckResult, SpecialTxFault, consensus};

/// `SharedCollateralScript()`: `0x04 "DSHC" OP_DROP OP_TRUE`.
// Core: evo/sharedcollateral.h:19-23
pub const SHARED_COLLATERAL_SCRIPT: [u8; 7] = [0x04, b'D', b'S', b'H', b'C', 0x75, 0x51];

/// `IsSharedCollateralScript`: the template exactly, never a prefix.
// Core: evo/sharedcollateral.h:25-28
pub fn is_shared_collateral_script(script: &[u8]) -> bool {
    script == SHARED_COLLATERAL_SCRIPT
}

/// Core's `CheckSharedCollateralSpends`: no input but a ProDisTx's spends an unspent template
/// output. `coins` is the view the inputs are read from (`AccessCoin`: a missing coin reads as
/// spent).
// Core (rc.4): evo/specialtxman.cpp:1875-1891
pub fn shared_collateral_spends<E: Debug>(
    tx: &Tx,
    coins: &dyn CoinsView<Error = E>,
) -> CheckResult<E> {
    if tx.is_coinbase() {
        return Ok(Ok(()));
    }
    let dissolve = tx.is_special_tx_version() && tx.tx_type() == TxType::ProviderDissolve;
    let mut expanded = [0u8; MAX_SPECIAL_SCRIPT_LEN];
    for input in tx.inputs() {
        let Some(coin) = coins.coin(&input.prevout).map_err(SpecialTxFault::Coins)? else {
            continue;
        };
        if is_shared_collateral_script(coin.script_pubkey().expand(&mut expanded)) && !dissolve {
            return Ok(consensus(BAD_SHARED_COLLATERAL_SPEND));
        }
    }
    Ok(Ok(()))
}

/// Core's `CheckSharedCollateralTemplateOutputs`: an output pays the template only as the
/// collateral output of a shared ProRegTx with an internal collateral. `payload` is the
/// transaction's slot; Core's `GetTxPayload` reads it whole or not at all.
// Core (rc.4): evo/specialtxman.cpp:1893-1910
pub fn shared_collateral_template_outputs(
    tx: &Tx,
    payload: PayloadSlot<'_>,
) -> Result<(), TxValidationError> {
    for (i, out) in tx.outputs().iter().enumerate() {
        if !is_shared_collateral_script(out.script_pubkey.as_bytes()) {
            continue;
        }
        if !tx.is_special_tx_version() || tx.tx_type() != TxType::ProviderRegister {
            return consensus(BAD_SHARED_COLLATERAL_CREATE);
        }
        let p = match payload {
            Some(Ok(decoded)) if decoded.is_complete() => ProRegTx::of(&decoded.payload),
            _ => None,
        };
        let Some(p) = p else {
            return consensus(BAD_SHARED_COLLATERAL_CREATE);
        };
        if p.shares.is_empty()
            || !p.collateral_hash.is_null()
            || u64::from(p.collateral_index) != i as u64
        {
            return consensus(BAD_SHARED_COLLATERAL_CREATE);
        }
    }
    Ok(())
}
