//! Every conversion between dashcore's types and base-sdk's or `dash-consensus`'s (SB0 §5.5,
//! DEC-112 O2, node program task SB12a).
//!
//! The shared crates (`dash-chain`, `dash-evo`, `dash-filters`, `dash-consensus`) take base-sdk
//! types, and the layer rules keep dashcore out of them. dash-spv's managers, the FFI and the
//! Rust consumers speak dashcore. Whatever crosses between the two goes through this module, and
//! nothing else in dash-spv converts.
//!
//! - [`headers`]: block headers, field for field and hashed once: a header enters
//!   `dash-consensus` unhashed and is hashed there, the only place a `dash-consensus` hash can
//!   come from (DEC-224), and leaves with that hash. A header that comes with a hash is hashed
//!   again and checked. Blocks cross the same way.
//! - [`messages`]: `headers`, `headers2`, `cfilter`, `cfheaders`, `mnlistdiff`, `qrinfo`,
//!   `clsig` and `isdlock`, between dashcore's message types and base-sdk's.
//! - [`platform`]: the dashcore shapes Platform's queries return (`latest_masternode_list`,
//!   `get_quorum_at_height`), filled from base-sdk's list entries and commitments.
//!
//! Headers are the hot path and map field by field. Everything else crosses as Core's
//! serialization: one side writes it and the other decodes all of it, so a value either converts
//! exactly or fails with a [`BridgeError`] (base-sdk refuses some bytes dashcore accepts, such as
//! non-canonical BLS points). CV3 and CV4 later shrink this module to the identity, and CV4
//! deletes it (SB0 §13.2).

pub mod headers;
pub mod messages;
pub mod platform;

#[cfg(all(test, feature = "__x11-count", debug_assertions))]
mod hash_once_tests;

use std::fmt::Display;

use dash_types::codec::BaseCodec;
use dashcore::consensus::{deserialize, serialize, Decodable, Encodable};

/// A value that could not cross the bridge.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BridgeError {
    /// base-sdk or `dash-consensus` refused the bytes dashcore wrote.
    #[error("{kind}: base-sdk refuses dashcore's value: {reason}")]
    ToBase {
        /// The message or value kind.
        kind: &'static str,
        /// The decoder's error.
        reason: String,
    },
    /// dashcore refused the bytes base-sdk or `dash-consensus` wrote.
    #[error("{kind}: dashcore refuses base-sdk's value: {reason}")]
    FromBase {
        /// The message or value kind.
        kind: &'static str,
        /// The decoder's error.
        reason: String,
    },
    /// A batch of headers came with a different number of hashes.
    #[error("{headers} headers with {hashes} hashes")]
    HashCount {
        /// The number of headers.
        headers: usize,
        /// The number of hashes.
        hashes: usize,
    },
    /// A header came with a hash that is not its X11.
    #[error("{kind}: the hash given is not the header's X11")]
    WrongHash {
        /// `header` or `block`.
        kind: &'static str,
    },
    /// The hashes given with a `headers2` message do not restore its headers.
    #[error("headers2: header {index} differs from the decompressed one")]
    Headers2Mismatch {
        /// The first header that differs.
        index: usize,
    },
}

/// The base-sdk value of a dashcore value: dashcore writes it, base-sdk decodes every byte.
fn to_base<T: Encodable, U: BaseCodec>(kind: &'static str, value: &T) -> Result<U, BridgeError> {
    decode_all(kind, &serialize(value), U::decode)
}

/// `bytes` decoded by one of base-sdk's decoders, all of them.
fn decode_all<U, E: Display>(
    kind: &'static str,
    bytes: &[u8],
    decode: impl FnOnce(&mut &[u8]) -> Result<U, E>,
) -> Result<U, BridgeError> {
    let mut cursor = bytes;
    let value = decode(&mut cursor).map_err(|e| BridgeError::ToBase {
        kind,
        reason: e.to_string(),
    })?;
    if !cursor.is_empty() {
        return Err(BridgeError::ToBase {
            kind,
            reason: format!("{} bytes left over", cursor.len()),
        });
    }
    Ok(value)
}

/// The dashcore value of a base-sdk value: base-sdk writes it, dashcore decodes every byte.
fn from_base<U: BaseCodec, T: Decodable>(kind: &'static str, value: &U) -> Result<T, BridgeError> {
    let mut bytes = Vec::new();
    value.encode(&mut bytes);
    decode_dashcore(kind, &bytes)
}

/// `bytes` decoded by dashcore, all of them.
fn decode_dashcore<T: Decodable>(kind: &'static str, bytes: &[u8]) -> Result<T, BridgeError> {
    deserialize(bytes).map_err(|e| BridgeError::FromBase {
        kind,
        reason: e.to_string(),
    })
}
