//! The node-side payload side-car: a decoded special payload together with the BLS objects the
//! node reads from it (T6 §3).
//!
//! Every node site reads a provider payload's operator key and operator signature through
//! [`CorePayload::operator_key`] and [`CorePayload::operator_sig`], from the one object cached
//! with the decode ([`Tx::payload`](super::Tx::payload)), so a key resolved by an earlier check is
//! the key later checks, comparisons and clones see. base-sdk holds both fields as bytes; the
//! side-car builds each object once, at decode, and refuses the payloads Core's payload read
//! refuses.

use core::ops::Deref;

use dash_pkc::bls::{BlsEager, BlsEagerSig, BlsLazyPk, BlsSchemeKind, BlsSchemeSource};
use dash_primitives::{
    OperatorKey, OperatorSig, PayloadDecode, PayloadError, ProTxVersion, SpecialPayload, TxType,
};

/// A decoded special payload and its BLS side-car.
#[derive(Clone, Debug)]
pub struct CorePayload {
    decode: PayloadDecode,
    bls: PayloadBls,
}

/// The BLS objects of one decoded payload.
#[derive(Clone, Debug)]
struct PayloadBls {
    operator_key: Option<BlsLazyPk>,
    operator_sig: Option<BlsEagerSig>,
}

impl CorePayload {
    /// Decodes the extra payload of a `tx_type` transaction from the front of `extra`, as
    /// base-sdk's `Transaction::decode_payload` does, and builds its side-car; unread bytes are
    /// not an error here (see [`Self::is_complete`]).
    ///
    /// # Errors
    ///
    /// The `PayloadError` of a recognised type that fails to decode, and Core's error for a
    /// payload whose operator signature Core's payload read refuses.
    pub fn decode(tx_type: TxType, extra: &[u8]) -> Result<CorePayload, PayloadError> {
        let mut cursor = extra;
        let payload = SpecialPayload::decode(tx_type, &mut cursor)?;
        let consumed = extra.len() - cursor.len();
        let bls = PayloadBls {
            operator_key: provider_operator_key(&payload),
            operator_sig: provider_operator_sig(tx_type, &payload)?,
        };
        Ok(Self {
            decode: PayloadDecode::new(payload, consumed, extra.len()),
            bls,
        })
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
        self.bls.operator_key.as_ref()
    }

    /// The operator signature of a ProUpServTx or ProUpRevTx.
    pub fn operator_sig(&self) -> Option<&BlsEagerSig> {
        self.bls.operator_sig.as_ref()
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

/// The operator key object of a ProRegTx, ProUpRegTx or ProUpSharedRegTx.
fn provider_operator_key(payload: &SpecialPayload) -> Option<BlsLazyPk> {
    Some(match payload {
        SpecialPayload::ProviderRegister(p) => {
            pro_tx_operator_key(&p.pub_key_operator(), p.version())
        }
        SpecialPayload::ProviderUpdateRegistrar(p) => {
            pro_tx_operator_key(&p.pub_key_operator(), p.version())
        }
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
            shared_operator_key(&p.pub_key_operator())
        }
        _ => return None,
    })
}

/// The operator key of a ProRegTx or ProUpRegTx of `version`: its bytes under the version's
/// scheme (Core's `CBLSLazyWrapper::Unserialize`).
#[must_use]
pub fn pro_tx_operator_key(key: &OperatorKey, version: ProTxVersion) -> BlsLazyPk {
    BlsLazyPk::from_bytes(key.as_bytes().as_bytes(), version_source(version))
}

/// The operator key of a ProUpSharedRegTx: its bytes under the basic scheme always.
#[must_use]
pub fn shared_operator_key(key: &OperatorKey) -> BlsLazyPk {
    BlsLazyPk::from_bytes(key.as_bytes().as_bytes(), BlsSchemeSource::AlwaysBasic)
}

/// The operator signature object of a ProUpServTx or ProUpRevTx under the version's scheme (Core's
/// `CBLSWrapper::Unserialize`).
fn provider_operator_sig(
    tx_type: TxType,
    payload: &SpecialPayload,
) -> Result<Option<BlsEagerSig>, PayloadError> {
    let (sig, version) = match payload {
        SpecialPayload::ProviderUpdateService(p) => (p.sig(), p.version()),
        SpecialPayload::ProviderUpdateRevoke(p) => (p.sig(), p.version()),
        _ => return Ok(None),
    };
    pro_tx_operator_sig(&sig, version)
        .map(Some)
        .ok_or_else(|| PayloadError::new(tx_type, "malleable BLS object".into()))
}

/// The operator signature of a ProUpServTx or ProUpRevTx of `version`: its bytes under the
/// version's scheme, or under the other scheme when canonical only there (Core's
/// `CBLSWrapper::Unserialize`); `None` when Core's payload read refuses it.
#[must_use]
pub fn pro_tx_operator_sig(sig: &OperatorSig, version: ProTxVersion) -> Option<BlsEagerSig> {
    BlsEager::decode(sig.as_bytes().as_bytes(), version_source(version)).ok()
}

const fn version_source(version: ProTxVersion) -> BlsSchemeSource {
    BlsSchemeSource::Version(protx_key_scheme(version))
}
