// The values D1 reads from an operator key and an operator signature, compiled once against each
// side's dash-pkc (`include!`d where `dash_pkc` names that side's crate). Every probe runs on a
// clone, so no probe sees another's effect on the object.

use dash_pkc::bls::{
    BlsEagerSig, BlsLazyPk, BlsScIetf, BlsSchemeKind, BlsSchemeSource, BlsSecretKey,
};
use serde_json::{Value, json};

pub fn hx(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn scheme_name(scheme: BlsSchemeKind) -> &'static str {
    if scheme == BlsSchemeKind::Legacy {
        "legacy"
    } else {
        "basic"
    }
}

const SCHEMES: [(&str, BlsSchemeKind); 2] =
    [("legacy", BlsSchemeKind::Legacy), ("basic", BlsSchemeKind::Basic)];

/// A valid basic key, the stand-in for a masternode state's key in the `==` probes.
fn state_key_bytes() -> [u8; 48] {
    BlsSecretKey::<BlsScIetf>::from_ikm(&[0x5a; 32]).expect("key material").public_key().to_bytes()
}

/// The operator-key probes of T6 §5 D1: fresh `to_bytes()`, the `get()` value, `to_bytes_as`
/// both schemes on a fresh object and after `get()`, `is_default`, and `==` against masternode
/// state keys of both schemes.
pub fn key(k: &BlsLazyPk) -> Value {
    let fresh = k.clone().to_bytes();
    let value = k.clone().get();
    let resolved = k.clone();
    let _ = resolved.get();
    let state =
        BlsLazyPk::from_bytes(&state_key_bytes(), BlsSchemeSource::Version(BlsSchemeKind::Basic))
            .get();
    let others = [
        ("state_basic", BlsLazyPk::from_value(state.clone(), BlsSchemeKind::Basic)),
        ("state_legacy", BlsLazyPk::from_value(state, BlsSchemeKind::Legacy)),
        (
            "own_bytes_basic",
            BlsLazyPk::from_bytes(&fresh, BlsSchemeSource::Version(BlsSchemeKind::Basic)),
        ),
        (
            "own_bytes_legacy",
            BlsLazyPk::from_bytes(&fresh, BlsSchemeSource::Version(BlsSchemeKind::Legacy)),
        ),
        ("default_basic", BlsLazyPk::new(BlsSchemeKind::Basic)),
        ("default_legacy", BlsLazyPk::new(BlsSchemeKind::Legacy)),
    ];
    let mut eq = serde_json::Map::new();
    for (name, other) in &others {
        eq.insert(format!("fresh_{name}"), json!(k.clone() == other.clone()));
        eq.insert(format!("resolved_{name}"), json!(resolved.clone() == other.clone()));
    }
    let mut as_fresh = serde_json::Map::new();
    let mut as_resolved = serde_json::Map::new();
    let mut get_as = serde_json::Map::new();
    for (name, scheme) in SCHEMES {
        as_fresh.insert(name.into(), json!(hx(&k.clone().to_bytes_as(scheme))));
        as_resolved.insert(name.into(), json!(hx(&resolved.clone().to_bytes_as(scheme))));
        get_as.insert(name.into(), json!(hx(&value.to_bytes(scheme))));
    }
    json!({
        "scheme": scheme_name(k.scheme()),
        "fresh": hx(&fresh),
        "get_valid": value.is_valid(),
        "get": get_as,
        "fresh_as": as_fresh,
        "resolved": hx(&resolved.clone().to_bytes()),
        "resolved_as": as_resolved,
        "is_default": k.clone() == BlsLazyPk::new(k.scheme()),
        "eq": eq,
        "print": hx(&k.clone().get().to_bytes(k.scheme())),
    })
}

/// The operator-signature probes: the eager value's validity, scheme and bytes under both schemes.
pub fn sig(s: &BlsEagerSig) -> Value {
    json!({
        "scheme": scheme_name(s.scheme()),
        "valid": s.is_valid(),
        "legacy": hx(&s.to_bytes(BlsSchemeKind::Legacy)),
        "basic": hx(&s.to_bytes(BlsSchemeKind::Basic)),
    })
}
