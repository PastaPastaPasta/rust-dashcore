use dash_evo::cbtx::{CBTX_VERSION_CLSIG_AND_BALANCE, CoinbaseFacts};
use dash_evo::locks::{ClsigHashSchemes, clsig_hash};
use dash_p2p_core::ClSig;
use dash_primitives::{BlockHash, SpecialPayload, TxType};
use dash_types::Numeric;

use super::Read;

/// Extracts the lock ingested by the handler. The caller samples `schemes` at processing time.
// Core: chainlock/handler.cpp:104 ProcessCoinbaseChainLock
#[expect(
    clippy::too_many_arguments,
    reason = "Core's guards and explicit BLS context are independent inputs"
)]
pub fn coinbase_clsig(
    block_cb: &CoinbaseFacts,
    height: u32,
    v20_at_block: bool,
    synced: bool,
    enabled: bool,
    best_height: i32,
    ancestor_hash: impl FnOnce(u32) -> Option<BlockHash>,
    schemes: ClsigHashSchemes,
) -> Option<(Read<ClSig>, [u8; 32])> {
    if !synced || !enabled || !v20_at_block {
        return None;
    }
    let CoinbaseFacts::First(tx) = block_cb else {
        return None;
    };
    if tx.tx_type() != TxType::CoinbaseCommitment {
        return None;
    }
    let decoded = tx.payload()?.ok()?;
    let SpecialPayload::CoinbaseCommitment(cbtx) = decoded.payload() else {
        return None;
    };
    if !decoded.is_complete() || cbtx.version().to_base() < CBTX_VERSION_CLSIG_AND_BALANCE {
        return None;
    }
    let signature = cbtx.best_cl_signature().as_ref()?.get(schemes.read);
    if !signature.is_valid() {
        return None;
    }
    let height = i32::try_from(height).ok()?;
    let diff = cbtx.best_cl_height_diff()?;
    if height <= 0 || diff >= height as u64 {
        return None;
    }
    let lock_height = height - diff as i32 - 1;
    if lock_height <= best_height {
        return None;
    }
    let hash = ancestor_hash(lock_height as u32)?;
    let sig = signature.to_bytes(schemes.read.scheme());
    let clsig = ClSig::new(lock_height, hash, sig.into());
    let hash = clsig_hash(&clsig, schemes)?;
    Some((
        Read {
            msg: clsig,
            read: schemes.read,
        },
        hash,
    ))
}
