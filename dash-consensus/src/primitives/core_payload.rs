//! The node-side payload side-car: a decoded special payload together with the BLS objects the
//! node reads from it (T6 §3).
//!
//! Every node site reads a provider payload's operator key and operator signature through
//! [`CorePayload::operator_key`] and [`CorePayload::operator_sig`], from the one object cached
//! with the decode ([`Tx::payload`](super::Tx::payload)), so a key resolved by an earlier check is
//! the key later checks, comparisons and clones see. This form delegates to the base-sdk
//! payload's own fields.

use core::ops::Deref;

use dash_pkc::bls::{BlsEagerSig, BlsLazyPk, BlsSchemeKind};
use dash_primitives::{PayloadDecode, PayloadError, ProTxVersion, SpecialPayload, TxType};

/// A decoded special payload and its BLS side-car.
#[derive(Clone, Debug)]
pub struct CorePayload {
    decode: PayloadDecode,
}

impl CorePayload {
    /// Decodes the extra payload of a `tx_type` transaction from the front of `extra`, as
    /// base-sdk's `Transaction::decode_payload` does; unread bytes are not an error here (see
    /// [`Self::is_complete`]).
    ///
    /// # Errors
    ///
    /// The `PayloadError` of a recognised type that fails to decode.
    pub fn decode(tx_type: TxType, extra: &[u8]) -> Result<CorePayload, PayloadError> {
        let mut cursor = extra;
        let payload = SpecialPayload::decode(tx_type, &mut cursor)?;
        let consumed = extra.len() - cursor.len();
        Ok(Self::new(PayloadDecode::new(payload, consumed, extra.len())))
    }

    /// Wraps a base-sdk decode without changing its value or decode accounting.
    pub fn new(decode: PayloadDecode) -> Self {
        Self {
            decode,
        }
    }

    /// `GetTxPayload`'s "stream empty" condition: the decoder read every byte.
    pub fn is_complete(&self) -> bool {
        self.decode.is_complete()
    }

    /// The decoded special payload.
    pub fn payload(&self) -> &SpecialPayload {
        self.decode.payload()
    }

    /// The operator key of a ProRegTx, ProUpRegTx or ProUpSharedRegTx.
    pub fn operator_key(&self) -> Option<&BlsLazyPk> {
        match self.payload() {
            SpecialPayload::ProviderRegister(p) => Some(p.pub_key_operator()),
            SpecialPayload::ProviderUpdateRegistrar(p) => Some(p.pub_key_operator()),
            SpecialPayload::ProviderUpdateSharedRegistrar(p) => Some(p.pub_key_operator()),
            _ => None,
        }
    }

    /// The operator signature of a ProUpServTx or ProUpRevTx.
    pub fn operator_sig(&self) -> Option<&BlsEagerSig> {
        match self.payload() {
            SpecialPayload::ProviderUpdateService(p) => Some(p.sig()),
            SpecialPayload::ProviderUpdateRevoke(p) => Some(p.sig()),
            _ => None,
        }
    }
}

impl Deref for CorePayload {
    type Target = PayloadDecode;

    fn deref(&self) -> &Self::Target {
        &self.decode
    }
}

/// The scheme a provider payload's BLS fields (types 1 to 4) decode and encode under: legacy for
/// version 1 only (`nVersion == ProTxVersion::LegacyBLS`).
#[must_use]
pub const fn protx_key_scheme(version: ProTxVersion) -> BlsSchemeKind {
    BlsSchemeKind::from_legacy(matches!(version, ProTxVersion::LegacyBls))
}
