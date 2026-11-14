//! Core's operator-key read at init step 7c, with secret material kept in locked memory.

use std::fmt;

use dash_args::args::setting_to_string;
use dash_args::{ArgsManager, RuntimeError, SettingsValue};
use dash_pkc::bls::core::{BlsPkElem, BlsSigElem};
use dash_pkc::bls::{BlsScChia, BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSigId};
use dashd_lockedpool::Secure;
use zeroize::{Zeroize, Zeroizing};

pub use dash_pkc::bls::core::{BlsPkValue as OperatorPubKey, BlsSigValue as BlsSig};

const GROUP_ORDER: [u8; 32] = [
    0x73, 0xed, 0xa7, 0x53, 0x29, 0x9d, 0x7d, 0x48, 0x33, 0x39, 0xd8, 0x08, 0x09, 0xa1, 0xd8, 0x05,
    0x53, 0xbd, 0xa4, 0x02, 0xff, 0xfe, 0x5b, 0xfe, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01,
];

/// Why the operator-key read at step 7c failed.
#[derive(Debug, PartialEq, Eq)]
pub enum ActiveMnInitError {
    /// The non-empty argument did not encode a valid, non-zero BLS secret key.
    InvalidKey,
    /// Core's `GetArg` exception for an array or object setting.
    Argument(RuntimeError),
}

impl fmt::Display for ActiveMnInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKey => {
                f.write_str("Invalid masternodeblsprivkey. Please see documentation.")
            }
            Self::Argument(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ActiveMnInitError {}

/// The active masternode's operator key. Only its public key leaves this handle.
///
/// ```compile_fail
/// # use dashd_masternode::active::OperatorKey;
/// fn clone_key(key: &OperatorKey) -> OperatorKey { key.clone() }
/// ```
///
/// ```compile_fail
/// # use dashd_masternode::active::OperatorKey;
/// fn requires_copy<T: Copy>() {}
/// requires_copy::<OperatorKey>();
/// ```
///
/// ```compile_fail
/// # use dashd_masternode::active::OperatorKey;
/// fn display_key(key: &OperatorKey) { let _ = format!("{key}"); }
/// ```
///
/// ```compile_fail
/// # use dashd_masternode::active::OperatorKey;
/// fn serialize_key(key: &OperatorKey) { let _ = serde_json::to_string(key); }
/// ```
///
/// ```compile_fail
/// # use dashd_masternode::active::OperatorKey;
/// fn deserialize_key() { let _ = serde_json::from_str::<OperatorKey>("null"); }
/// ```
pub struct OperatorKey {
    sk: Secure<32>,
    pk: OperatorPubKey,
}

impl fmt::Debug for OperatorKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OperatorKey(<redacted>)")
    }
}

impl OperatorKey {
    /// `CActiveMasternodeManager::Sign(hash, is_legacy)`.
    // Core: active/masternode.cpp:290-294
    pub fn sign(&self, hash: &[u8; 32], legacy: bool) -> BlsSig {
        if self.sk.as_slice() == GROUP_ORDER {
            // Core: dashbls/src/privatekey.cpp:294-316 (multiplication by r gives infinity)
            return BlsSigElem::from_impl(&BlsSig::invalid()).into();
        }
        let signature = if legacy {
            BlsSecretKey::<BlsScChia>::from_bytes(&self.sk)
                .map(Zeroizing::new)
                .map(|scalar| scalar.sign_digest(hash).to_bytes())
        } else {
            BlsSecretKey::<BlsScIetf>::from_bytes(&self.sk)
                .map(Zeroizing::new)
                .map(|scalar| scalar.sign_with(hash, BlsSigId::Basic).to_bytes())
        };
        signature
            .ok()
            .and_then(|bytes| BlsSig::from_canonical(&bytes, BlsSchemeKind::from_legacy(legacy)))
            .unwrap_or_else(BlsSig::invalid)
    }

    /// The basic-scheme signature bytes, independent of the tip's BLS scheme.
    // Core: active/masternode.cpp:297-303
    pub fn sign_basic(&self, hash: &[u8; 32]) -> Vec<u8> {
        self.sign(hash, false).to_bytes(BlsSchemeKind::Basic).to_vec()
    }

    /// `CActiveMasternodeManager::GetPubKey()`.
    // Core: active/masternode.cpp:308-312
    pub fn pubkey(&self) -> OperatorPubKey {
        self.pk.clone()
    }
}

struct SecretSetting(SettingsValue);

impl Zeroize for SecretSetting {
    fn zeroize(&mut self) {
        fn wipe(value: &mut SettingsValue) {
            match value {
                SettingsValue::Str(s) | SettingsValue::Num(s) => s.zeroize(),
                SettingsValue::Arr(values) => values.iter_mut().for_each(wipe),
                SettingsValue::Obj(entries) => {
                    for (key, value) in entries {
                        key.zeroize();
                        wipe(value);
                    }
                }
                SettingsValue::Null | SettingsValue::Bool(_) => {}
            }
        }
        wipe(&mut self.0);
    }
}

/// Read `-masternodeblsprivkey` with Core's `GetArg` semantics. An unset or empty value is
/// inactive; every other value yields a key or Core's init error. The original argument stays
/// in `ArgsManager`, as it does in Core.
// Core: init.cpp:2156-2160; util/system.cpp:523-535
pub fn parse(args: &ArgsManager) -> Result<Option<OperatorKey>, ActiveMnInitError> {
    let setting = Zeroizing::new(SecretSetting(args.get_setting("-masternodeblsprivkey")));
    let Some(text) =
        setting_to_string(&setting.0).map_err(ActiveMnInitError::Argument)?.map(Zeroizing::new)
    else {
        return Ok(None);
    };
    if text.is_empty() {
        return Ok(None);
    }
    let bytes = parse_hex(&text)?;
    let bytes: &[u8; 32] =
        bytes.as_slice().try_into().map_err(|_| ActiveMnInitError::InvalidKey)?;
    // Core: bls/bls.h:106-119; dashbls/src/privatekey.cpp:66-70 (rejects > r)
    if bytes == &[0; 32] || bytes > &GROUP_ORDER {
        return Err(ActiveMnInitError::InvalidKey);
    }
    let pk = if bytes == &GROUP_ORDER {
        // Core: dashbls/src/privatekey.cpp:151-162 (valid infinity from multiplication)
        BlsPkElem::from_impl(&OperatorPubKey::invalid()).into()
    } else {
        let scalar = Zeroizing::new(
            BlsSecretKey::<BlsScIetf>::from_bytes(bytes)
                .map_err(|_| ActiveMnInitError::InvalidKey)?,
        );
        OperatorPubKey::from_set_bytes_basic(&scalar.public_key().to_bytes())
    };
    let key = OperatorKey {
        sk: Secure::from_slice(bytes),
        pk,
    };
    // Core: active/masternode.cpp:60-62
    tracing::info!(
        "MASTERNODE:\n  blsPubKeyOperator legacy: {}\n  blsPubKeyOperator basic: {}\n",
        hex(&key.pk.to_bytes(BlsSchemeKind::Legacy)),
        hex(&key.pk.to_bytes(BlsSchemeKind::Basic)),
    );
    Ok(Some(key))
}

// Core: util/strencodings.cpp:81-97; util/strencodings.h:63-67
fn parse_hex(text: &str) -> Result<Zeroizing<Vec<u8>>, ActiveMnInitError> {
    let mut out = Zeroizing::new(Vec::with_capacity(text.len() / 2));
    let mut bytes = text.bytes();
    while let Some(first) = bytes.next() {
        if b" \x0c\n\r\t\x0b".contains(&first) {
            continue;
        }
        let second = bytes.next().ok_or(ActiveMnInitError::InvalidKey)?;
        let digit = |b: u8| match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err(ActiveMnInitError::InvalidKey),
        };
        out.push(digit(first)? << 4 | digit(second)?);
    }
    Ok(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests;
