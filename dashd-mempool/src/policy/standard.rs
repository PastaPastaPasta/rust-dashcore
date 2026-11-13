//! Output, transaction and input standardness, in Core's check order.

use dash_consensus::Tx;
use dash_consensus::coins::CoinsView;
use dash_consensus::coins::compress::MAX_SPECIAL_SCRIPT_LEN;
use dash_consensus::script::{
    NoSignatureChecker, ScriptFlags, eval_script, is_push_only, script_sigop_count,
};
use dash_primitives::{SpecialPayload, TxType};

use super::consts::{
    MAX_P2SH_SIGOPS, MAX_STANDARD_SCRIPTSIG_SIZE, MAX_STANDARD_TX_SIZE, TX_MAX_STANDARD_VERSION,
};
use super::dust::is_dust;
use super::solver::{TxoutType, solver};

/// `IsStandard`, including the type even when the policy rejects it.
// Core v24.0.0: policy/policy.cpp:52-74
pub fn is_standard(script: &[u8], max_datacarrier_bytes: Option<u32>) -> (bool, TxoutType) {
    let (kind, solutions) = solver(script);
    let standard = match kind {
        TxoutType::Nonstandard => false,
        TxoutType::Multisig => {
            let m = solutions[0][0];
            let n = solutions[solutions.len() - 1][0];
            (1..=3).contains(&n) && (1..=n).contains(&m)
        }
        TxoutType::NullData => {
            max_datacarrier_bytes.is_some_and(|max| script.len() <= max as usize)
        }
        _ => true,
    };
    (standard, kind)
}

/// `IsStandardTx`, returning Core's exact first reject reason. The minimum size is a
/// separate ATMP check; it is not part of `IsStandardTx`.
// Core v24.0.0: policy/policy.cpp:76-168
pub fn is_standard_tx(
    tx: &Tx,
    max_datacarrier_bytes: Option<u32>,
    permit_bare_multisig: bool,
    dust_relay_fee: i64,
) -> Result<(), &'static str> {
    if tx.version() > TX_MAX_STANDARD_VERSION || tx.version() < 1 {
        return Err("version");
    }
    if tx.size() >= MAX_STANDARD_TX_SIZE {
        return Err("tx-size");
    }
    for input in tx.inputs() {
        let script = input.script_sig().as_bytes();
        if script.len() > MAX_STANDARD_SCRIPTSIG_SIZE as usize {
            return Err("scriptsig-size");
        }
        if !is_push_only(script) {
            return Err("scriptsig-not-pushonly");
        }
    }
    let mut data_outputs = 0;
    for (index, output) in tx.outputs().iter().enumerate() {
        let script = output.script_pubkey().as_bytes();
        let (standard, kind) = is_standard(script, max_datacarrier_bytes);
        if !standard {
            if tx.is_special_tx_version()
                && tx.tx_type() == TxType::ProviderRegister
                && is_shared_collateral_script(script)
                && let Some(Ok(decoded)) = tx.payload()
                && decoded.is_complete()
                && let SpecialPayload::ProviderRegister(p) = decoded.payload()
                && !p.shares().is_empty()
                && p.collateral_hash().is_null()
                && u64::from(p.collateral_index()) == index as u64
            {
                continue;
            }
            return Err("scriptpubkey");
        }
        if kind == TxoutType::NullData {
            data_outputs += 1;
        } else if kind == TxoutType::Multisig && !permit_bare_multisig {
            return Err("bare-multisig");
        } else if is_dust(output, dust_relay_fee) {
            return Err("dust");
        }
    }
    if data_outputs > 1 {
        return Err("multi-op-return");
    }
    Ok(())
}

/// `AreInputsStandard`. Storage errors propagate separately from the policy verdict.
/// A missing coin has Core's empty, nonstandard script; callers normally check existence first.
// Core v24.0.0: policy/policy.cpp:170-205
pub fn are_inputs_standard<V: CoinsView + ?Sized>(tx: &Tx, coins: &V) -> Result<bool, V::Error> {
    if tx.is_coinbase() {
        return Ok(true);
    }
    let mut expanded = [0; MAX_SPECIAL_SCRIPT_LEN];
    for input in tx.inputs() {
        let Some(coin) = coins.coin(&input.prevout())? else {
            return Ok(false);
        };
        let script_pubkey = coin.script_pubkey();
        let script = script_pubkey.expand(&mut expanded);
        match solver(script).0 {
            TxoutType::Nonstandard => {
                if tx.is_special_tx_version()
                    && tx.tx_type() == TxType::ProviderDissolve
                    && is_shared_collateral_script(script)
                {
                    continue;
                }
                return Ok(false);
            }
            TxoutType::ScriptHash => {
                let mut stack = Vec::new();
                if eval_script(
                    &mut stack,
                    input.script_sig().as_bytes(),
                    ScriptFlags::NONE,
                    &NoSignatureChecker,
                )
                .is_err()
                {
                    return Ok(false);
                }
                let Some(redeem_script) = stack.last() else {
                    return Ok(false);
                };
                if script_sigop_count(redeem_script, true) > MAX_P2SH_SIGOPS {
                    return Ok(false);
                }
            }
            _ => {}
        }
    }
    Ok(true)
}

// Core v24.0.0: evo/sharedcollateral.h:19-28 IsSharedCollateralScript
fn is_shared_collateral_script(script: &[u8]) -> bool {
    script == [0x04, b'D', b'S', b'H', b'C', 0x75, 0x51]
}
