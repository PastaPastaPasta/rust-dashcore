//! Additional relay policy for special transactions.

use dash_consensus::Tx;
use dash_primitives::{SpecialPayload, TxType};

/// `IsStandardSpecialTx`, including its successful result for an undecodable payload.
// Core v24.0.0: evo/specialtxman.cpp:1913-1940
pub fn is_standard_special_tx(tx: &Tx) -> Result<(), &'static str> {
    if !tx.is_special_tx_version() || tx.tx_type() != TxType::AssetLock {
        return Ok(());
    }
    if tx.inputs().len() > 100 {
        return Err("assetlocktx-too-many-inputs");
    }
    if tx.size() > 20_480 {
        return Err("assetlocktx-too-big");
    }
    if let Some(Ok(decoded)) = tx.payload()
        && decoded.is_complete()
        && let SpecialPayload::AssetLock(payload) = decoded.payload()
        && payload.version() >= 2
    {
        return Err("assetlocktx-version-2");
    }
    Ok(())
}
