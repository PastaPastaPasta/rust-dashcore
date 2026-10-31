//! Core's `GetValidatedPayload<T>` and `ProTxVersion::GetMax` (EV1 §3.2), and a block's payloads,
//! decoded once ([`BlockPayloads`], EV1 D6).

use dash_consensus::primitives::{Block, Tx};
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_primitives::{PayloadDecode, PayloadError, SpecialPayload, TxType};

use super::reasons::{BAD_PROTX_PAYLOAD, BAD_PROTX_TYPE, BAD_PROTX_VERSION};

/// A transaction's decoded payload, as [`Tx::payload`] gives it: `None` when the transaction has
/// no payload field (`!HasExtraPayloadField()`), else the decode. [`PayloadDecode::is_complete`]
/// is `GetTxPayload`'s "stream empty" condition (`specialtx.h:17-28`).
pub type PayloadSlot<'a> = Option<Result<&'a PayloadDecode, &'a PayloadError>>;

/// The payload slot of each transaction of a block, by position (EV1 D6). The caller builds it
/// once per block and hands the slots to the special-transaction checks and the list build, so no
/// payload is decoded twice. Each [`Tx`] caches its own decode, so building it decodes each
/// payload at most once and later [`Tx::payload`] calls return the same slot.
#[derive(Debug)]
pub struct BlockPayloads<'a> {
    slots: Vec<PayloadSlot<'a>>,
}

impl<'a> BlockPayloads<'a> {
    /// Decodes the payload of every transaction of `block`.
    pub fn new(block: &'a Block) -> Self {
        Self::from_txs(block.txs().iter().map(|tx| &**tx))
    }

    /// Decodes the payload of every transaction of `txs`, in order.
    pub fn from_txs(txs: impl IntoIterator<Item = &'a Tx>) -> Self {
        BlockPayloads {
            slots: txs.into_iter().map(Tx::payload).collect(),
        }
    }

    /// The slot of transaction `index`.
    ///
    /// # Panics
    ///
    /// When `index` is not a transaction of the block.
    pub fn get(&self, index: usize) -> PayloadSlot<'a> {
        self.slots[index]
    }

    /// The number of transactions.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the block has no transactions.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// Core's `ProTxVersion::GetMax(is_basic_scheme_active, is_extended_addr)`: the highest ProTx
/// version a block may carry, `ExtAddr` (3) once V19 and V24 are active, `BasicBLS` (2) under
/// V19 alone, else `LegacyBLS` (1).
// Core: evo/types.h:23-35
pub const fn version_max(v19: bool, v24: bool) -> u16 {
    match (v19, v24) {
        (true, true) => 3,
        (true, false) => 2,
        (false, _) => 1,
    }
}

/// A ProTx payload `GetValidatedPayload<T>` reads (types 1 to 4 and 10 to 12).
pub trait ProTxPayload: Sized {
    /// `T::SPECIALTX_TYPE`.
    const TX_TYPE: TxType;

    /// The payload when the decode is of this type.
    fn of(payload: &SpecialPayload) -> Option<&Self>;

    /// `nVersion`.
    fn version(&self) -> u16;

    /// `T::IsTriviallyValid`.
    fn is_trivially_valid(&self) -> Result<(), TxValidationError>;
}

fn consensus(reason: &'static str) -> TxValidationError {
    TxValidationError::new(TxValidationResult::Consensus, reason)
}

/// Core's `GetValidatedPayload<T>`: the payload of a transaction of type `tx_type` whose payload
/// decodes in full, of a version the block allows, and trivially valid. `v19` is
/// `DeploymentActiveAfter(pindexPrev, V19)` and `v24` is `rules.v24`.
///
/// A provider payload of which Core reads only the version (base-sdk's `UnknownProTxVersion`,
/// version 0 or above 3, D20) is rejected with `bad-protx-version`: above the maximum by the
/// version step, version 0 by `IsTriviallyValid`'s first step. With bytes after the version it
/// does not decode in full, so it is `bad-protx-payload`.
// Core: evo/specialtxman.cpp:1157-1183
pub fn validated_payload<'a, T: ProTxPayload>(
    tx_type: TxType,
    payload: PayloadSlot<'a>,
    v19: bool,
    v24: bool,
) -> Result<&'a T, TxValidationError> {
    if tx_type != T::TX_TYPE {
        return Err(consensus(BAD_PROTX_TYPE));
    }
    let decoded = match payload {
        Some(Ok(decoded)) if decoded.is_complete() => &decoded.payload,
        _ => return Err(consensus(BAD_PROTX_PAYLOAD)),
    };
    if let SpecialPayload::UnknownProTxVersion {
        ..
    } = decoded
    {
        return Err(consensus(BAD_PROTX_VERSION));
    }
    // base-sdk decodes by `tx_type`, so another variant is not reachable here.
    let p = T::of(decoded).ok_or_else(|| consensus(BAD_PROTX_PAYLOAD))?;
    if p.version() > version_max(v19, v24) {
        return Err(consensus(BAD_PROTX_VERSION));
    }
    p.is_trivially_valid()?;
    Ok(p)
}

#[cfg(test)]
pub(crate) mod tests;
