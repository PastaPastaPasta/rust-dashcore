//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! Spork signers as Core's startup reads them.

use crate::prelude::*;

use base58ck::decode_check;
use dash_num::Hash160;
use dash_types::Numeric;

/// The spork signers Core's `CSporkManager` holds after applying
/// `-sporkaddr` and `-minsporkkeys` to the chain's default address.
/// Verifying sporks is not this crate's job.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct SporkSigners {
    /// Signer key IDs; v24 holds one key.
    pub key_ids: Vec<Hash160>,
    /// Signatures a spork needs (v23's `nMinSporkKeys`); `None` at v24.
    pub min_spork_keys: Option<i32>,
    /// Core's startup warning when v24 is given several `-sporkaddr` values.
    pub warning: Option<String>,
}

/// Core's `IsSpace`: the characters `DecodeBase58` skips around an address.
fn is_core_space(c: char) -> bool {
    matches!(c, ' ' | '\x0c' | '\n' | '\r' | '\t' | '\x0b')
}

/// The key ID of `address` if Core's `DecodeDestination` reads it as a P2PKH
/// address with version byte `pubkey_addr` (`CSporkManager::SetSporkAddress`).
///
/// Core rejects a NUL, skips surrounding whitespace, and reads a string
/// starting with the Platform HRP `platform_hrp` (any case) as bech32, never
/// as a key.
pub(crate) fn spork_key_id(
    address: &str,
    pubkey_addr: u8,
    platform_hrp: Option<&str>,
) -> Option<Hash160> {
    if address.contains('\0') {
        return None;
    }
    let bytes = address.as_bytes();
    if platform_hrp.is_some_and(|hrp| {
        bytes.get(..hrp.len()).is_some_and(|p| p.eq_ignore_ascii_case(hrp.as_bytes()))
    }) {
        return None;
    }
    let data = decode_check(address.trim_matches(is_core_space)).ok()?;
    let (&version, hash) = data.split_first()?;
    let hash: [u8; 20] = hash.try_into().ok()?;
    (version == pubkey_addr).then(|| Hash160::from_base(hash))
}
