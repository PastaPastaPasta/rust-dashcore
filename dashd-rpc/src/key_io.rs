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

/// `DecodeBase58(str, ret, max_ret_len)`: Base58 without a checksum. Whitespace around the
/// string is skipped; the decode stops, failing, once its output passes `max_ret_len`.
// Core: base58.cpp:38-80, 127-133
fn decode_base58(s: &str, max_ret_len: usize) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let s = s.as_bytes();
    if s.contains(&0) {
        return None;
    }
    let space = |c: &u8| is_space(char::from(*c));
    let mut rest = &s[s.iter().take_while(|c| space(c)).count()..];
    let zeroes = rest.iter().take_while(|&&c| c == b'1').count();
    if zeroes > max_ret_len {
        return None;
    }
    rest = &rest[zeroes..];
    // Core sizes the buffer by the rest of the string; the output never passes
    // `max_ret_len + 1` bytes before the decode fails, so that bounds it too.
    let size = (rest.len() * 733 / 1000 + 1).min(max_ret_len + 2);
    let mut b256 = vec![0u8; size];
    let mut length = 0;
    while let Some(&c) = rest.first().filter(|c| !space(c)) {
        let mut carry = u32::try_from(ALPHABET.iter().position(|&a| a == c)?).expect("< 58");
        let mut i = 0;
        for b in b256.iter_mut().rev() {
            if carry == 0 && i >= length {
                break;
            }
            carry += 58 * u32::from(*b);
            *b = (carry % 256) as u8;
            carry /= 256;
            i += 1;
        }
        debug_assert_eq!(carry, 0);
        length = i;
        if length + zeroes > max_ret_len {
            return None;
        }
        rest = &rest[1..];
    }
    if !rest.iter().all(space) {
        return None;
    }
    let mut out = vec![0; zeroes];
    out.extend_from_slice(&b256[size - length..]);
    Some(out)
}

/// `DecodeDestination(str)` with the chain's parameters: `None` for every string that is no
/// P2PKH or P2SH address of this chain.
pub fn decode_destination(s: &str, p: &KeyIoParams) -> Option<Destination> {
    decode_destination_with_error(s, p).ok()
}

/// `DecodeDestination(str, error_msg, &error_locations)`: the destination, or Core's error text
/// and the offsets it blames (only a Bech32 string's errors have offsets).
// Core: key_io.cpp:42-102
pub fn decode_destination_with_error(
    s: &str,
    p: &KeyIoParams,
) -> Result<Destination, (String, Vec<i32>)> {
    let err = |msg: &str| Err((msg.to_owned(), Vec::new()));
    // Note this will be false if it is a valid Bech32 address for a different network.
    let is_bech32 = p.platform_hrp.as_deref().is_some_and(|hrp| {
        s.as_bytes().get(..hrp.len()).unwrap_or(s.as_bytes()).eq_ignore_ascii_case(hrp.as_bytes())
    });
    if !is_bech32 {
        let Some(data) = decode_base58_check(s).filter(|d| d.len() <= 21) else {
            // Without the checksum, with a much larger limit.
            return match decode_base58(s, 100) {
                None => err("Not a valid Bech32m or Base58 encoding"),
                Some(_) => err("Invalid checksum or length of Base58 address"),
            };
        };
        let hash: Option<[u8; 20]> = data.get(1..).and_then(|h| h.try_into().ok());
        return match (data.first(), hash) {
            (Some(&v), Some(h)) if v == p.pubkey_address => Ok(Destination::PkHash(h)),
            (Some(&v), Some(h)) if v == p.script_address => Ok(Destination::ScriptHash(h)),
            // The prefix matched, so the length was wrong.
            (Some(&v), _) if v == p.pubkey_address || v == p.script_address => {
                err("Invalid length for Base58 address")
            }
            _ => err("Invalid prefix for Base58-encoded address"),
        };
    }
    // Dash has no Bech32m L1 destinations: a string with the Platform HRP can only be a DIP-18
    // Platform address. Decode it anyway to tell the user why it got rejected.
    let platform = decode_platform_destination(s, p);
    let (error, locations) = crate::bech32::locate_errors(s.as_bytes());
    if !error.is_empty() {
        return Err((error, locations));
    }
    match platform {
        Ok(_) => err("This is a Dash Platform address, not a Dash Core address"),
        Err(e) => err(e),
    }
}

/// `EncodeDestination`.
// Core: key_io.cpp:25-37, 185-188
pub fn encode_destination(dest: &Destination, p: &KeyIoParams) -> String {
    let (prefix, hash) = match dest {
        Destination::PkHash(h) => (p.pubkey_address, h),
        Destination::ScriptHash(h) => (p.script_address, h),
    };
    let mut data = vec![prefix];
    data.extend_from_slice(hash);
    encode_base58_check(&data)
}

/// `EncodeBase58Check`, for this file's payloads (at most an extended key's 78 bytes).
// Core: base58.cpp:135-142
pub(crate) fn encode_base58_check(data: &[u8]) -> String {
    base58ck::Base58CkString::encode(data).expect("a key_io payload is short").as_str().to_owned()
}

/// `ExtractDestination`: the address of a P2PK, P2PKH or P2SH script.
// Core: script/standard.cpp:151-183 ExtractDestination, :111-149 Solver
pub fn extract_destination(script: &[u8]) -> Option<Destination> {
    if let Some(key) = match_pay_to_pubkey(script) {
        return Some(Destination::PkHash(hash160(key)));
    }
    match script {
        [OP_HASH160, 20, h @ .., OP_EQUAL] if h.len() == 20 => {
            Some(Destination::ScriptHash(h.try_into().ok()?))
        }
        [OP_DUP, OP_HASH160, 20, h @ .., OP_EQUALVERIFY, OP_CHECKSIG] if h.len() == 20 => {
            Some(Destination::PkHash(h.try_into().ok()?))
        }
        _ => None,
    }
}

/// DIP-18's type bytes and payload size.
const DIP18_TYPE_BYTE_P2PKH: u8 = 0xb0;
const DIP18_TYPE_BYTE_P2SH: u8 = 0x80;
const DIP18_PAYLOAD_SIZE: usize = 21;

/// A valid `PlatformDestination` (DIP-18): a Platform address over a key or script hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformDestination {
    P2pkh([u8; 20]),
    P2sh([u8; 20]),
}

impl PlatformDestination {
    /// `GetScriptForPlatformDestination`: the credit output script an asset lock carries for it.
    // Core: key_io.cpp:292-301
    pub fn script(&self) -> Vec<u8> {
        self.l1().script()
    }

    /// The L1 destination of the same hash.
    pub fn l1(&self) -> Destination {
        match *self {
            PlatformDestination::P2pkh(h) => Destination::PkHash(h),
            PlatformDestination::P2sh(h) => Destination::ScriptHash(h),
        }
    }
}

/// `DecodePlatformDestination(str, params, error_str)`.
// Core: key_io.cpp:259-284
pub fn decode_platform_destination(
    s: &str,
    p: &KeyIoParams,
) -> Result<PlatformDestination, &'static str> {
    use crate::bech32::{Encoding, convert_bits, decode};
    let dec = decode(s.as_bytes()).ok_or("Invalid bech32m encoding")?;
    if dec.encoding != Encoding::Bech32m {
        return Err("DIP-18 Platform addresses require bech32m checksum");
    }
    if p.platform_hrp.as_deref() != Some(dec.hrp.as_str()) {
        return Err("Invalid Platform HRP for the selected network");
    }
    let payload =
        convert_bits(&dec.data, 5, 8, false).ok_or("Invalid Platform address payload encoding")?;
    if payload.len() != DIP18_PAYLOAD_SIZE {
        return Err("Invalid Platform address payload length");
    }
    let hash: [u8; 20] = payload[1..].try_into().expect("21 bytes");
    match payload[0] {
        DIP18_TYPE_BYTE_P2PKH => Ok(PlatformDestination::P2pkh(hash)),
        DIP18_TYPE_BYTE_P2SH => Ok(PlatformDestination::P2sh(hash)),
        _ => Err("Unknown DIP-18 type byte"),
    }
}

/// `EncodePlatformDestination`.
///
/// # Panics
/// On a chain without a Platform HRP, which never decodes one.
// Core: key_io.cpp:217-257
pub fn encode_platform_destination(dest: &PlatformDestination, p: &KeyIoParams) -> String {
    let (type_byte, hash) = match dest {
        PlatformDestination::P2pkh(h) => (DIP18_TYPE_BYTE_P2PKH, h),
        PlatformDestination::P2sh(h) => (DIP18_TYPE_BYTE_P2SH, h),
    };
    let mut payload = vec![type_byte];
    payload.extend_from_slice(hash);
    let values = crate::bech32::convert_bits(&payload, 8, 5, true).expect("padding never fails");
    let hrp = p.platform_hrp.as_deref().expect("a chain with Platform addresses");
    crate::bech32::encode(crate::bech32::Encoding::Bech32m, hrp, &values)
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

/// `EncodeExtPubKey`: the prefix, then `CExtPubKey::Encode`'s 74 bytes, in Base58Check.
// Core: key_io.cpp:149-157, pubkey.cpp:307-316
pub fn encode_ext_pubkey(key: &ExtPubKey, p: &KeyIoParams) -> String {
    let h = &key.header;
    let mut data = p.ext_public_key.to_vec();
    data.push(h.depth);
    data.extend_from_slice(&h.fingerprint);
    data.extend_from_slice(&h.child.to_be_bytes());
    data.extend_from_slice(&h.chaincode);
    data.extend_from_slice(&key.pubkey.serialize());
    encode_base58_check(&data)
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

/// `CPubKey::ValidSize`: a size its first byte gives.
pub(crate) fn valid_size(key: &[u8]) -> bool {
    !key.is_empty() && pubkey_len(key[0]) == key.len()
}

/// `MatchPayToPubkey`: a 65- or 33-byte push of a validly sized key, then `OP_CHECKSIG`.
// Core: script/standard.cpp:39-50
pub(crate) fn match_pay_to_pubkey(script: &[u8]) -> Option<&[u8]> {
    match script {
        [n @ (33 | 65), key @ .., OP_CHECKSIG]
            if usize::from(*n) == key.len() && valid_size(key) =>
        {
            Some(key)
        }
        _ => None,
    }
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
