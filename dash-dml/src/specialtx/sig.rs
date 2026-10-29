//! The signatures special transactions carry (EV1 §2.5, §3.18): Core's ECDSA checks
//! (`CHashSigner::VerifyHash`, `VerifyHashCanonical`, `CMessageSigner::VerifyMessage`), the hashes
//! and strings ProTx signatures sign, and the seam BLS verification goes through
//! ([`SigVerifier`], D2).
//!
//! ECDSA recovery is dash-evo's port of `CPubKey::RecoverCompact` on the pinned libsecp256k1
//! (CS1). The preimages hash base-sdk's builders (BS6b), which re-encode the decoded payload, so a
//! payload decoded from a non-canonical wire form signs its canonical re-encoding, as Core's
//! `SerializeHash` of the object does (D23).

use dash_consensus::hash::{HashWriter, sha256d};
use dash_consensus::primitives::Tx;
use dash_consensus::primitives::canonical::write_prevout;
use dash_evo::spork::{COMPACT_SIGNATURE_SIZE, recover_key_id};
use dash_params::ChainParams;
use dash_pkc::bls::{BlsEagerSig, BlsLazyPk, BlsSchemeKind};
use dash_primitives::{
    ProDisTx, ProRegTx, ProUpRegTx, ProUpRevTx, ProUpServTx, ProUpShareTx, ProUpSharedRegTx,
};

use crate::types::KeyId;

/// Core's `CMessageSigner` hash of `message`: SHA256d of `MESSAGE_MAGIC` and the message, each
/// with its compact-size length.
// Core: messagesigner.cpp:39-46; util/message.cpp:25
pub use dash_evo::spork::message_hash;

/// Core's `CHashSigner::VerifyHash`: the key `sig` recovers over `hash` is `key_id`'s.
///
/// The header byte is not range-checked and a high-S signature recovers, as in Core
/// ([`recover_key_id`]).
// Core: messagesigner.cpp:91-107; pubkey.cpp:199-217
#[must_use]
pub fn verify_hash(hash: &[u8; 32], key_id: &KeyId, sig: &[u8]) -> bool {
    recover_key_id(hash, sig).is_some_and(|recovered| recovered == *key_id.as_bytes())
}

/// secp256k1's group order halved, big-endian (`SECP256K1_HALF_ORDER`).
const HALF_ORDER: [u8; 32] = [
    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0x5d, 0x57, 0x6e, 0x73, 0x57, 0xa4, 0x50, 0x1d, 0xdf, 0xe9, 0x2f, 0x46, 0x68, 0x1b, 0x20, 0xa0,
];

/// Core's `CHashSigner::VerifyHashCanonical`: 65 bytes, a header byte `SignCompact` can produce
/// (27 to 34), a low S (compared big-endian with the half order), then [`verify_hash`].
// Core: messagesigner.cpp:58-89
#[must_use]
pub fn verify_hash_canonical(hash: &[u8; 32], key_id: &KeyId, sig: &[u8]) -> bool {
    if sig.len() != COMPACT_SIGNATURE_SIZE || !(27..=34).contains(&sig[0]) {
        return false;
    }
    sig[33..] <= HALF_ORDER[..] && verify_hash(hash, key_id, sig)
}

/// Core's `CMessageSigner::VerifyMessage(keyID, …)`: [`verify_hash`] over [`message_hash`].
// Core: messagesigner.cpp:39-46
#[must_use]
pub fn verify_message(key_id: &KeyId, sig: &[u8], message: &str) -> bool {
    verify_hash(&message_hash(message), key_id, sig)
}

/// A ProTx payload whose signature signs `::SerializeHash(ptx)`: the payload without its
/// signature.
pub trait SignedPayload {
    /// The serialization `SER_GETHASH` writes, signature excluded; `None` for a version base-sdk
    /// cannot encode (0, above 3), which a payload past
    /// [`validated_payload`](super::validated_payload) never has.
    #[must_use]
    fn sig_hash_bytes(&self) -> Option<Vec<u8>>;
}

macro_rules! signed_payload {
    (fallible: $($t:ty),*; infallible: $($u:ty),*) => {
        $(impl SignedPayload for $t {
            fn sig_hash_bytes(&self) -> Option<Vec<u8>> {
                <$t>::sig_hash_bytes(self).ok()
            }
        })*
        $(impl SignedPayload for $u {
            fn sig_hash_bytes(&self) -> Option<Vec<u8>> {
                Some(<$u>::sig_hash_bytes(self))
            }
        })*
    };
}

signed_payload!(fallible: ProRegTx, ProUpServTx, ProUpRegTx, ProUpRevTx;
    infallible: ProUpShareTx, ProUpSharedRegTx);

/// `::SerializeHash(ptx)`, the hash a ProTx signature signs; `None` as
/// [`SignedPayload::sig_hash_bytes`].
// Core: evo/specialtxman.cpp:1132, 1669, 1726
#[must_use]
pub fn payload_sig_hash<T: SignedPayload>(payload: &T) -> Option<[u8; 32]> {
    payload.sig_hash_bytes().map(|bytes| sha256d(&bytes))
}

/// Core's `CProRegTx::MakeSignString`, which an external collateral's key signs as a message: the
/// payouts, the operator reward, the owner and voting addresses and the payload hash, with the
/// network's address prefixes; `None` as [`SignedPayload::sig_hash_bytes`].
// Core: evo/providertx.cpp:370-390
#[must_use]
pub fn make_sign_string(payload: &ProRegTx, params: &ChainParams) -> Option<String> {
    payload.sign_string(&params.addr_params).ok()
}

/// Core's `MakeSharedRegConsentHash(tx)`, which each share owner of a shared registration signs;
/// `None` as [`SignedPayload::sig_hash_bytes`].
// Core: evo/providertx.cpp:330-368
#[must_use]
pub fn shared_reg_consent_hash(payload: &ProRegTx, tx: &Tx) -> Option<[u8; 32]> {
    payload.shared_consent_bytes(tx.raw()).ok().map(|bytes| sha256d(&bytes))
}

/// Core's `CProDisTx::MakeSignHash(tx, sig_count)`; Core verifies with the payload's signature
/// count.
// Core: evo/providertx.cpp:496-523
#[must_use]
pub fn pro_dis_sign_hash(payload: &ProDisTx, tx: &Tx, sig_count: u8) -> [u8; 32] {
    sha256d(&payload.sig_hash_bytes(tx.raw(), sig_count))
}

/// Core's `CalcTxInputsHash(tx)`, which a ProTx's `inputsHash` must equal
/// (`bad-protx-inputs-hash`): SHA256d of every input's prevout in order, with no count.
// Core: evo/specialtx.cpp:10-17; evo/providertx.h:559-566
#[must_use]
pub fn inputs_hash(tx: &Tx) -> [u8; 32] {
    let mut w = HashWriter::new();
    for input in tx.inputs() {
        write_prevout(&input.prevout, &mut w);
    }
    w.finish()
}

/// The bound verifier cannot evaluate a scheme: a fault that aborts the check, never a reject
/// (D2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifyUnavailable;

/// The scheme a BLS verification runs under: Core's global `bls_legacy_scheme` at the call, which
/// the chain state clears once V19 is active after the block's parent. A type of its own, not the
/// [`BlsSchemeKind`] a key or signature carries from its decode (D22), so a payload's decode
/// scheme cannot stand in for it.
// Core: bls/bls.cpp:18; evo/specialtxman.cpp:1043-1047, 1068-1072
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifyScheme(BlsSchemeKind);

impl VerifyScheme {
    /// The verify scheme for a block whose parent has V19 active (basic) or not (legacy).
    #[must_use]
    pub const fn after_v19(v19_active: bool) -> VerifyScheme {
        VerifyScheme(if v19_active {
            BlsSchemeKind::Basic
        } else {
            BlsSchemeKind::Legacy
        })
    }

    /// The scheme the verification evaluates under.
    #[must_use]
    pub const fn kind(self) -> BlsSchemeKind {
        self.0
    }
}

/// BLS verification, injected so the pure crate makes no pairing calls (D2).
pub trait SigVerifier {
    /// Core's `CBLSSignature::VerifyInsecure(pk, hash)`. `scheme` is the verify scheme only;
    /// `sig` and `pk` carry their own decode (D22). `Ok(false)` for an undecodable point, and for
    /// an Exotic point under the basic flag; under the legacy flag, relic's result.
    ///
    /// # Errors
    ///
    /// [`VerifyUnavailable`] when this verifier cannot evaluate `scheme`.
    // Core: bls/bls.cpp:296-312
    fn bls_verify_insecure(
        &self,
        sig: &BlsEagerSig,
        pk: &BlsLazyPk,
        msg: &[u8; 32],
        scheme: VerifyScheme,
    ) -> Result<bool, VerifyUnavailable>;
}

/// A [`SigVerifier`] for tests: a fixed verdict for each scheme, `None` where it is unavailable,
/// and a count of its calls.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, Default)]
pub struct FixedVerifier {
    pub basic: Option<bool>,
    pub legacy: Option<bool>,
    pub calls: std::cell::Cell<usize>,
}

#[cfg(any(test, feature = "test-utils"))]
impl SigVerifier for FixedVerifier {
    fn bls_verify_insecure(
        &self,
        _: &BlsEagerSig,
        _: &BlsLazyPk,
        _: &[u8; 32],
        scheme: VerifyScheme,
    ) -> Result<bool, VerifyUnavailable> {
        self.calls.set(self.calls.get() + 1);
        match scheme.kind() {
            BlsSchemeKind::Basic => self.basic,
            BlsSchemeKind::Legacy => self.legacy,
        }
        .ok_or(VerifyUnavailable)
    }
}

#[cfg(test)]
mod tests;
