//! Core's `util/message.cpp` (RP1a-3a D26): signing a message with a key and verifying a
//! signature against an address, with Core's Base64 (`util/strencodings.cpp`).

use bitcoin_hashes::{Hash as _, sha256d};
use secp256k1::Message;
use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};

use crate::key_io::{Destination, Key, KeyIoParams, decode_destination, hash160};

/// `MESSAGE_MAGIC`.
const MESSAGE_MAGIC: &str = "DarkCoin Signed Message:\n";

/// `MessageHash`: SHA256d of the magic and the message, each serialized as a string (its
/// CompactSize length, then its bytes).
// Core: util/message.cpp:77-83
pub fn message_hash(message: &str) -> [u8; 32] {
    let mut data = Vec::with_capacity(MESSAGE_MAGIC.len() + message.len() + 10);
    for s in [MESSAGE_MAGIC, message] {
        write_compact_size(&mut data, s.len() as u64);
        data.extend_from_slice(s.as_bytes());
    }
    sha256d::Hash::hash(&data).to_byte_array()
}

/// `WriteCompactSize`.
fn write_compact_size(out: &mut Vec<u8>, n: u64) {
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend_from_slice(&n.to_le_bytes());
        }
    }
}

/// `MessageVerificationResult`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageVerification {
    InvalidAddress,
    AddressNoKey,
    MalformedSignature,
    PubkeyNotRecovered,
    NotSigned,
    Ok,
}

/// `MessageVerify`.
// Core: util/message.cpp:27-58
pub fn message_verify(
    address: &str,
    signature: &str,
    message: &str,
    p: &KeyIoParams,
) -> MessageVerification {
    let Some(destination) = decode_destination(address, p) else {
        return MessageVerification::InvalidAddress;
    };
    let Destination::PkHash(hash) = destination else {
        return MessageVerification::AddressNoKey;
    };
    let Some(signature) = decode_base64(signature) else {
        return MessageVerification::MalformedSignature;
    };
    let Some(pubkey) = recover_compact(&message_hash(message), &signature) else {
        return MessageVerification::PubkeyNotRecovered;
    };
    if hash160(&pubkey) != hash {
        return MessageVerification::NotSigned;
    }
    MessageVerification::Ok
}

/// `MessageSign`: the Base64 of `CKey::SignCompact` (RFC 6979, no extra entropy).
// Core: util/message.cpp:60-75, key.cpp CKey::SignCompact
pub fn message_sign(key: &Key, message: &str) -> String {
    let signature = RecoverableSignature::sign_ecdsa_recoverable(
        Message::from_digest(message_hash(message)),
        &key.secret,
    );
    let (recid, compact) = signature.serialize_compact();
    let mut out = Vec::with_capacity(65);
    out.push(27 + u8::from(recid) + 4 * u8::from(key.compressed));
    out.extend_from_slice(&compact);
    encode_base64(&out)
}

/// `CPubKey::RecoverCompact`: the serialized key that made a 65-byte compact signature of
/// `hash`. The header byte is read as Core reads it: `(header - 27) & 3` is the recovery id and
/// bit 2 the compression, with no range check.
// Core: pubkey.cpp CPubKey::RecoverCompact
fn recover_compact(hash: &[u8; 32], signature: &[u8]) -> Option<Vec<u8>> {
    let [header, compact @ ..] = signature else {
        return None;
    };
    if compact.len() != 64 {
        return None;
    }
    let v = header.wrapping_sub(27);
    let recid = RecoveryId::try_from(i32::from(v & 3)).ok()?;
    let signature = RecoverableSignature::from_compact(compact, recid).ok()?;
    let pubkey = signature.recover_ecdsa(Message::from_digest(*hash)).ok()?;
    Some(if v & 4 != 0 {
        pubkey.serialize().to_vec()
    } else {
        pubkey.serialize_uncompressed().to_vec()
    })
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `EncodeBase64`, padded.
// Core: util/strencodings.cpp:120-137
pub fn encode_base64(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(char::from(BASE64[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    while !out.len().is_multiple_of(4) {
        out.push('=');
    }
    out
}

/// `DecodeBase64`: a length that is a multiple of 4, at most two `=` at the end, Base64
/// characters before them and no stray bits.
// Core: util/strencodings.cpp:139-172
pub fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let mut s = s.as_bytes();
    if !s.len().is_multiple_of(4) {
        return None;
    }
    for _ in 0..2 {
        if let [rest @ .., b'='] = s {
            s = rest;
        }
    }
    let values = s
        .iter()
        .map(|&c| BASE64.iter().position(|&b| b == c).map(|v| v as u8))
        .collect::<Option<Vec<u8>>>()?;
    crate::bech32::convert_bits(&values, 6, 8, false)
}

#[cfg(test)]
mod tests;
