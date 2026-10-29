//! `key_io.cpp`'s decoders over the chain's Base58 prefixes ([`KeyIoParams`]): addresses
//! (`DecodeDestination`), WIF secrets (`DecodeSecret`) and BIP32 extended keys
//! (`DecodeExtKey`, `DecodeExtPubKey`), with the scripts `script/standard.cpp` builds for them.

use bitcoin_hashes::{Hash as _, HashEngine as _, hash160, hmac, sha512};
use secp256k1::{PublicKey, Scalar, SecretKey};

/// The chain parameters `key_io.cpp` reads from `Params()`: the Base58 prefixes (one byte for
/// addresses and secrets, four for extended keys, as every Dash chain has them) and the
/// Platform HRP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyIoParams {
    pub pubkey_address: u8,
    pub script_address: u8,
    pub secret_key: u8,
    pub ext_public_key: [u8; 4],
    pub ext_secret_key: [u8; 4],
    /// `Bech32PlatformHRP()`; `None` on a chain built before v24, which had none.
    pub platform_hrp: Option<String>,
}

/// A valid `CTxDestination`: `IsValidDestination` is false only for `CNoDestination`, which
/// the decoder answers as `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    PkHash([u8; 20]),
    ScriptHash([u8; 20]),
}

impl Destination {
    /// `GetScriptForDestination`.
    // Core: script/standard.cpp:200-215
    pub fn script(&self) -> Vec<u8> {
        match self {
            Destination::PkHash(h) => {
                let mut s = vec![OP_DUP, OP_HASH160, 20];
                s.extend_from_slice(h);
                s.extend_from_slice(&[OP_EQUALVERIFY, OP_CHECKSIG]);
                s
            }
            Destination::ScriptHash(h) => {
                let mut s = vec![OP_HASH160, 20];
                s.extend_from_slice(h);
                s.push(OP_EQUAL);
                s
            }
        }
    }
}

pub(crate) const OP_0: u8 = 0x00;
pub(crate) const OP_1: u8 = 0x51;
const OP_DUP: u8 = 0x76;
const OP_EQUAL: u8 = 0x87;
const OP_EQUALVERIFY: u8 = 0x88;
const OP_HASH160: u8 = 0xa9;
pub(crate) const OP_CHECKSIG: u8 = 0xac;
pub(crate) const OP_CHECKMULTISIG: u8 = 0xae;

/// `Hash160`: a `CKeyID` of a serialized public key, or a `CScriptID`.
pub(crate) fn hash160(data: &[u8]) -> [u8; 20] {
    hash160::Hash::hash(data).to_byte_array()
}

/// Core's `IsSpace`.
// Core: util/strencodings.h:168-170
fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\x0c' | '\n' | '\r' | '\t' | '\x0b')
}

/// `DecodeBase58Check(str, ret, max_ret_len)`: the payload of a checksummed Base58 string.
/// Leading and trailing whitespace is skipped, as `DecodeBase58` skips it; a NUL fails. A
/// payload over `max_ret_len` fails in Core and has the wrong size for every caller here.
// Core: base58.cpp:38-80, 144-167
fn decode_base58_check(s: &str) -> Option<Vec<u8>> {
    if s.contains('\0') {
        return None;
    }
    let s = s.trim_matches(is_space);
    // Core's decode stops once its output passes the limit; the decoder here has none and is
    // quadratic in its input, so a longer string is refused before it.
    if s.len() > MAX_BASE58_CHARS {
        return None;
    }
    base58ck::decode_check(s).ok()
}

/// The longest Base58 string of at most 82 bytes, the largest limit here (an extended key's 78
/// bytes and its checksum): 82 bytes of 0xff. A longer string decodes to more, which every
/// caller refuses, and Core's decode stops at.
const MAX_BASE58_CHARS: usize = 112;

/// `DecodeDestination(str)` with the chain's parameters: `None` for every string that is no
/// P2PKH or P2SH address of this chain. The error texts Core computes beside are not read by
/// any caller here.
// Core: key_io.cpp:42-104
pub fn decode_destination(s: &str, p: &KeyIoParams) -> Option<Destination> {
    // Note this will be false if it is a valid Bech32 address for a different network.
    let is_bech32 = p.platform_hrp.as_deref().is_some_and(|hrp| {
        s.as_bytes().get(..hrp.len()).is_some_and(|head| head.eq_ignore_ascii_case(hrp.as_bytes()))
    });
    if is_bech32 {
        // A string with the Platform HRP is at best a DIP-18 Platform address: never an L1
        // destination.
        return None;
    }
    let data = decode_base58_check(s)?;
    let hash: [u8; 20] = data.get(1..)?.try_into().ok()?;
    if data[0] == p.pubkey_address {
        Some(Destination::PkHash(hash))
    } else if data[0] == p.script_address {
        Some(Destination::ScriptHash(hash))
    } else {
        None
    }
}

/// A `CKey`: a valid secret and whether its public key is serialized compressed.
#[derive(Clone, Copy)]
pub struct Key {
    pub secret: SecretKey,
    pub compressed: bool,
}

impl Key {
    /// `GetPubKey()`, serialized as the key's compression says.
    pub fn pubkey(&self) -> Vec<u8> {
        let pk = PublicKey::from_secret_key(&self.secret);
        if self.compressed {
            pk.serialize().to_vec()
        } else {
            pk.serialize_uncompressed().to_vec()
        }
    }
}

/// `DecodeSecret(str)`: a WIF key of this chain; `None` where Core's key is not valid.
// Core: key_io.cpp:105-121
pub fn decode_secret(s: &str, p: &KeyIoParams) -> Option<Key> {
    let data = decode_base58_check(s)?;
    let compressed = match data.len() {
        33 => false,
        34 if data[33] == 1 => true,
        _ => return None,
    };
    if data[0] != p.secret_key {
        return None;
    }
    let secret = SecretKey::from_secret_bytes(data[1..33].try_into().ok()?).ok()?;
    Some(Key {
        secret,
        compressed,
    })
}

/// `BIP32_EXTKEY_SIZE`.
const BIP32_EXTKEY_SIZE: usize = 74;

/// The fields `CExtPubKey` and `CExtKey` share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExtHeader {
    depth: u8,
    fingerprint: [u8; 4],
    child: u32,
    chaincode: [u8; 32],
}

impl ExtHeader {
    fn decode(code: &[u8; BIP32_EXTKEY_SIZE]) -> ExtHeader {
        ExtHeader {
            depth: code[0],
            fingerprint: code[1..5].try_into().expect("4 bytes"),
            child: u32::from_be_bytes(code[5..9].try_into().expect("4 bytes")),
            chaincode: code[9..41].try_into().expect("32 bytes"),
        }
    }

    /// The master-key rule both `Decode`s apply: depth 0 has child 0 and fingerprint 0.
    fn master_rule_holds(&self) -> bool {
        self.depth != 0 || (self.child == 0 && self.fingerprint == [0; 4])
    }

    /// The child's header: one deeper, the parent's key id as fingerprint. `None` at depth 255.
    // Core: pubkey.cpp:339-346, key.cpp:379-386 (CExtPubKey::Derive, CExtKey::Derive)
    fn child(&self, parent: &PublicKey, child: u32) -> Option<ExtHeader> {
        let depth = self.depth.checked_add(1)?;
        let id = hash160(&parent.serialize());
        Some(ExtHeader {
            depth,
            fingerprint: id[..4].try_into().expect("4 bytes"),
            child,
            chaincode: [0; 32],
        })
    }
}

/// `BIP32Hash`: HMAC-SHA512 keyed by the chain code over `header || data || BE32(child)`.
// Core: hash.cpp:71-76
fn bip32_hash(chaincode: &[u8; 32], child: u32, header: u8, data: &[u8; 32]) -> [u8; 64] {
    let mut engine = hmac::HmacEngine::<sha512::Hash>::new(chaincode);
    engine.input(&[header]);
    engine.input(data);
    engine.input(&child.to_be_bytes());
    hmac::Hmac::<sha512::Hash>::from_engine(engine).to_byte_array()
}

/// A `CExtPubKey` whose public key is valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtPubKey {
    header: ExtHeader,
    pub pubkey: PublicKey,
}

impl ExtPubKey {
    /// `CExtPubKey::Derive(out, child)` for a non-hardened `child`; `None` where Core's returns
    /// false (depth 255, or a tweak off the curve's order).
    // Core: pubkey.cpp:240-259, 339-346
    pub fn derive(&self, child: u32) -> Option<ExtPubKey> {
        debug_assert_eq!(child >> 31, 0, "a public derivation is never hardened");
        let mut header = self.header.child(&self.pubkey, child)?;
        let ser = self.pubkey.serialize();
        let out =
            bip32_hash(&self.header.chaincode, child, ser[0], ser[1..].try_into().expect("32"));
        header.chaincode = out[32..].try_into().expect("32 bytes");
        let tweak = Scalar::from_be_bytes(out[..32].try_into().expect("32 bytes")).ok()?;
        let pubkey = self.pubkey.add_exp_tweak(&tweak).ok()?;
        Some(ExtPubKey {
            header,
            pubkey,
        })
    }

    /// `BIP32PubkeyProvider::GetExtKey`: this key's depth, fingerprint, child and chain code with
    /// `key`'s secret.
    // Core: script/descriptor.cpp:309-319
    pub fn with_secret(&self, key: &Key) -> ExtKey {
        ExtKey {
            header: self.header,
            secret: key.secret,
        }
    }
}

/// A `CExtKey` whose key is valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtKey {
    header: ExtHeader,
    pub secret: SecretKey,
}

impl ExtKey {
    /// `CExtKey::Neuter()`.
    // Core: key.cpp:400-408
    pub fn neuter(&self) -> ExtPubKey {
        ExtPubKey {
            header: self.header,
            pubkey: PublicKey::from_secret_key(&self.secret),
        }
    }

    /// `CExtKey::Derive(out, child)`, hardened or not; `None` where Core's returns false.
    // Core: key.cpp:291-308, 379-386
    pub fn derive(&self, child: u32) -> Option<ExtKey> {
        let pubkey = PublicKey::from_secret_key(&self.secret);
        let mut header = self.header.child(&pubkey, child)?;
        let out = if child >> 31 == 0 {
            let ser = pubkey.serialize();
            bip32_hash(&self.header.chaincode, child, ser[0], ser[1..].try_into().expect("32"))
        } else {
            bip32_hash(&self.header.chaincode, child, 0, &self.secret.to_secret_bytes())
        };
        header.chaincode = out[32..].try_into().expect("32 bytes");
        let tweak = Scalar::from_be_bytes(out[..32].try_into().expect("32 bytes")).ok()?;
        let secret = self.secret.add_tweak(&tweak).ok()?;
        Some(ExtKey {
            header,
            secret,
        })
    }

    /// The root key as a [`Key`] (`CExtKey::key`, always compressed).
    pub fn key(&self) -> Key {
        Key {
            secret: self.secret,
            compressed: true,
        }
    }
}

/// The 74-byte body of a Base58 extended key with the 4-byte `prefix`.
fn decode_ext(s: &str, prefix: &[u8; 4]) -> Option<[u8; BIP32_EXTKEY_SIZE]> {
    let data = decode_base58_check(s)?;
    if data.len() != 4 + BIP32_EXTKEY_SIZE || data[..4] != prefix[..] {
        return None;
    }
    data[4..].try_into().ok()
}

/// `DecodeExtPubKey(str)`; `None` where Core's `pubkey` is not valid.
// Core: key_io.cpp:136-147, pubkey.cpp:318-325
pub fn decode_ext_pubkey(s: &str, p: &KeyIoParams) -> Option<ExtPubKey> {
    let code = decode_ext(s, &p.ext_public_key)?;
    let header = ExtHeader::decode(&code);
    // `pubkey.Set` of 33 bytes, then `IsFullyValid`: a parse that also holds the length to the
    // header byte.
    let pubkey = parse_fully_valid(&code[41..])?;
    header.master_rule_holds().then_some(ExtPubKey {
        header,
        pubkey,
    })
}

/// `DecodeExtKey(str)`; `None` where Core's `key` is not valid.
// Core: key_io.cpp:159-170, key.cpp:420-427
pub fn decode_ext_key(s: &str, p: &KeyIoParams) -> Option<ExtKey> {
    let code = decode_ext(s, &p.ext_secret_key)?;
    let header = ExtHeader::decode(&code);
    let secret = SecretKey::from_secret_bytes(code[42..].try_into().expect("32 bytes")).ok()?;
    (header.master_rule_holds() && code[41] == 0).then_some(ExtKey {
        header,
        secret,
    })
}

/// `CPubKey::GetLen(header)`: the size a key with this first byte has, 0 for none.
// Core: pubkey.h:59-66
pub(crate) fn pubkey_len(header: u8) -> usize {
    match header {
        2 | 3 => 33,
        4 | 6 | 7 => 65,
        _ => 0,
    }
}

/// `CPubKey(data).IsFullyValid()`: the size the first byte gives, then
/// `secp256k1_ec_pubkey_parse`.
// Core: pubkey.cpp:299-305
pub(crate) fn parse_fully_valid(data: &[u8]) -> Option<PublicKey> {
    let len = data.first().map_or(0, |&h| pubkey_len(h));
    if len == 0 || len != data.len() {
        return None;
    }
    PublicKey::from_slice(data).ok()
}

#[cfg(test)]
mod tests;
