//! Core's output descriptors (`script/descriptor.cpp`): the checksum, the parser for every
//! function Dash's grammar has (`pk`, `pkh`, `combo`, `multi`, `sortedmulti`, `sh`, `addr`,
//! `raw`), with hex, WIF and BIP32 keys and key origins, and `Expand`, as
//! `getScriptFromDescriptor` (`generatetodescriptor`, `generateblock`) and `deriveaddresses` read
//! them; the public string form with its key origins and apostrophe flag, `IsSolvable`, the
//! `FlatSigningProvider` an expansion fills, and `InferDescriptor` over a signing provider
//! (`createmultisig`'s redeem script, `scantxoutset`'s expansions). The private and normalized
//! strings and the descriptor cache are not needed.

use std::collections::HashMap;

use crate::core_write::get_op;
use crate::key_io::{
    Destination, ExtPubKey, Key, KeyIoParams, OP_0, OP_1, OP_CHECKMULTISIG, OP_CHECKSIG,
    decode_destination, decode_ext_key, decode_ext_pubkey, decode_secret, encode_destination,
    encode_ext_pubkey, extract_destination, hash160, match_pay_to_pubkey, parse_fully_valid,
    pubkey_len, valid_size,
};
use crate::request::parse_hex;

/// `MAX_PUBKEYS_PER_MULTISIG`.
const MAX_PUBKEYS_PER_MULTISIG: usize = 20;
/// `MAX_SCRIPT_ELEMENT_SIZE`.
const MAX_SCRIPT_ELEMENT_SIZE: usize = 520;

/// Why `getScriptFromDescriptor` gave no script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DescriptorError {
    /// `Parse` failed with this error (possibly empty, as Core's can be): the function returns
    /// false.
    Parse(String),
    /// The descriptor is ranged: RPC_INVALID_PARAMETER.
    Ranged,
    /// `Expand` failed: RPC_INVALID_ADDRESS_OR_KEY "Cannot derive script without private keys".
    CannotDerive,
    /// A failed `CHECK_NONFATAL`, its condition: "Internal bug detected".
    Check(&'static str),
}

/// `getScriptFromDescriptor`: the descriptor's script, parsed without a required checksum and
/// expanded at position 0; of a `combo()`'s scripts, the P2PKH one.
// Core: rpc/mining.cpp:190-222
pub fn script_from_descriptor(
    descriptor: &str,
    p: &KeyIoParams,
) -> Result<Vec<u8>, DescriptorError> {
    let desc = ParsedDescriptor::parse(descriptor, p, false).map_err(DescriptorError::Parse)?;
    if desc.is_range() {
        return Err(DescriptorError::Ranged);
    }
    let mut scripts = desc.expand(0).ok_or(DescriptorError::CannotDerive)?;
    if scripts.is_empty() || scripts.len() > 4 {
        return Err(DescriptorError::Check("scripts.size() > 0 && scripts.size() <= 4"));
    }
    let pick = match scripts.len() {
        1 => 0,
        // Bitcoin's P2WPKH slot; Dash's combo() gives 1 or 3 scripts.
        4 => 2,
        _ => 1,
    };
    Ok(scripts.swap_remove(pick))
}

/// `FlatSigningProvider::keys`: the secrets the descriptor's WIF and xprv keys carry, by
/// `CKeyID`. The first key under an id stays (`emplace`).
type Keys = HashMap<[u8; 20], Key>;

/// A `KeyOriginInfo`: the fingerprint and path a derived key came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyOrigin {
    pub fingerprint: [u8; 4],
    pub path: Vec<u32>,
}

/// The public part of a `FlatSigningProvider`, what `InferDescriptor` reads: the scripts by
/// `CScriptID`, and the public keys and key origins by `CKeyID`. `Expand` fills it, and as
/// Core's `emplace` and `Merge` do, the first entry under an id stays.
#[derive(Clone, Debug, Default)]
pub struct SigningProvider {
    scripts: HashMap<[u8; 20], Vec<u8>>,
    pubkeys: HashMap<[u8; 20], Vec<u8>>,
    origins: HashMap<[u8; 20], KeyOrigin>,
}

impl SigningProvider {
    /// `scripts.emplace(CScriptID(script), script)`.
    pub fn add_script(&mut self, script: Vec<u8>) {
        self.scripts.entry(hash160(&script)).or_insert(script);
    }

    /// `pubkeys.emplace(pubkey.GetID(), pubkey)`.
    pub fn add_pubkey(&mut self, pubkey: Vec<u8>) {
        self.pubkeys.entry(hash160(&pubkey)).or_insert(pubkey);
    }

    /// `origins.emplace(pubkey.GetID(), {pubkey, origin})`.
    pub fn add_origin(&mut self, pubkey: &[u8], origin: KeyOrigin) {
        self.origins.entry(hash160(pubkey)).or_insert(origin);
    }

    /// `Merge`: `std::map::merge`, which keeps an entry already under an id.
    // Core: script/signingprovider.cpp:58-65
    fn merge(&mut self, other: SigningProvider) {
        for (id, script) in other.scripts {
            self.scripts.entry(id).or_insert(script);
        }
        for (id, pubkey) in other.pubkeys {
            self.pubkeys.entry(id).or_insert(pubkey);
        }
        for (id, origin) in other.origins {
            self.origins.entry(id).or_insert(origin);
        }
    }
}

/// `INPUT_CHARSET`.
const INPUT_CHARSET: &[u8] =
    b"0123456789()[],'/*abcdefgh@:$%{}IJKLMNOPQRSTUVWXYZ&+-.;<=>?!^_|~ijklmnopqrstuvwxyzABCDEFGH`#\"\\ ";

// Core: script/descriptor.cpp:84-94
fn poly_mod(mut c: u64, val: u64) -> u64 {
    let c0 = c >> 35;
    c = ((c & 0x7_ffff_ffff) << 5) ^ val;
    for (bit, k) in [0xf5dee51989, 0xa9fdca3312, 0x1bab10e32d, 0x3706b1677a, 0x644d626ffd]
        .into_iter()
        .enumerate()
    {
        if c0 & (1 << bit) != 0 {
            c ^= k;
        }
    }
    c
}

/// `DescriptorChecksum(span)`: `None` where Core's is empty (a character outside the input
/// set).
// Core: script/descriptor.cpp:96-141
pub fn descriptor_checksum(span: &str) -> Option<String> {
    let mut c = 1u64;
    let mut cls = 0u64;
    let mut clscount = 0;
    for ch in span.bytes() {
        let pos = INPUT_CHARSET.iter().position(|&x| x == ch)? as u64;
        c = poly_mod(c, pos & 31);
        cls = cls * 3 + (pos >> 5);
        clscount += 1;
        if clscount == 3 {
            c = poly_mod(c, cls);
            cls = 0;
            clscount = 0;
        }
    }
    if clscount > 0 {
        c = poly_mod(c, cls);
    }
    for _ in 0..8 {
        c = poly_mod(c, 0);
    }
    c ^= 1;
    Some(
        (0..8)
            .map(|j| char::from(crate::bech32::CHARSET[((c >> (5 * (7 - j))) & 31) as usize]))
            .collect(),
    )
}

/// `CheckChecksum(sp, require_checksum, error, out_checksum)`: the part before the `#`, and the
/// checksum computed over it.
// Core: script/descriptor.cpp:1212-1245
fn check_checksum(sp: &str, require_checksum: bool) -> Result<(&str, String), String> {
    let split: Vec<&str> = sp.split('#').collect();
    if split.len() > 2 {
        return Err("Multiple '#' symbols".into());
    }
    if split.len() == 1 && require_checksum {
        return Err("Missing checksum".into());
    }
    if let Some(given) = split.get(1)
        && given.len() != 8
    {
        return Err(format!("Expected 8 character checksum, not {} characters", given.len()));
    }
    let checksum = descriptor_checksum(split[0]).ok_or("Invalid characters in payload")?;
    if let Some(given) = split.get(1)
        && *given != checksum
    {
        return Err(format!(
            "Provided checksum '{given}' does not match computed checksum '{checksum}'"
        ));
    }
    Ok((split[0], checksum))
}

/// `GetDescriptorChecksum`: the checksum of the part before any `#`, or empty where
/// `CheckChecksum` fails.
// Core: script/descriptor.cpp:1257-1264
pub fn get_descriptor_checksum(descriptor: &str) -> String {
    check_checksum(descriptor, false).map(|(_, checksum)| checksum).unwrap_or_default()
}

/// `AddChecksum`.
// Core: script/descriptor.cpp:143
fn add_checksum(s: String) -> String {
    let checksum = descriptor_checksum(&s).expect("a descriptor's own string is in the input set");
    s + "#" + &checksum
}

/// The hardened marker a key's `apostrophe` flag selects.
fn hardened_marker(apostrophe: bool) -> char {
    if apostrophe {
        '\''
    } else {
        'h'
    }
}

/// `FormatHDKeypath`: each element as `/n`, a hardened one with `'` or `h`.
// Core: util/bip32.cpp:54-62
fn format_hd_keypath(path: &[u32], apostrophe: bool) -> String {
    let mut ret = String::new();
    for &i in path {
        ret += &format!("/{}", i & 0x7FFF_FFFF);
        if i >> 31 != 0 {
            ret.push(hardened_marker(apostrophe));
        }
    }
    ret
}

/// `spanparsing::Const`.
// Core: util/spanparsing.cpp:15-22
fn konst(s: &str, sp: &mut &str) -> bool {
    match sp.strip_prefix(s) {
        Some(rest) => {
            *sp = rest;
            true
        }
        None => false,
    }
}

/// `spanparsing::Func`: `name(...)` spanning all of `sp`; `sp` becomes the inside.
// Core: util/spanparsing.cpp:24-31
fn func(name: &str, sp: &mut &str) -> bool {
    let b = sp.as_bytes();
    if b.len() >= name.len() + 2
        && b[name.len()] == b'('
        && b[b.len() - 1] == b')'
        && sp.starts_with(name)
    {
        *sp = &sp[name.len() + 1..sp.len() - 1];
        return true;
    }
    false
}

/// `spanparsing::Expr`: the prefix of `sp` up to a `,` or `)` outside parentheses.
// Core: util/spanparsing.cpp:33-50
fn expr<'a>(sp: &mut &'a str) -> &'a str {
    let mut level = 0u32;
    let mut end = sp.len();
    for (i, c) in sp.bytes().enumerate() {
        match c {
            b'(' => level += 1,
            b')' if level > 0 => level -= 1,
            b')' | b',' if level == 0 => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    let (ret, rest) = sp.split_at(end);
    *sp = rest;
    ret
}

/// `ParseUInt32`: an optional `+`, then decimal digits only, in range.
// Core: util/strencodings.cpp:234-249
fn parse_u32(s: &str) -> Option<u32> {
    let digits = s.strip_prefix('+').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// `ParseKeyPath`: the elements after the first, each a uint32 below 2^31 with an optional
/// `'` or `h` for hardened. Each hardened element sets `apostrophe` to whether it used `'`.
// Core: script/descriptor.cpp:875-899
fn parse_key_path(split: &[&str], apostrophe: &mut bool) -> Result<Vec<u32>, String> {
    let mut out = Vec::with_capacity(split.len().saturating_sub(1));
    for elem in &split[1..] {
        let (elem, hardened) = match elem.strip_suffix(['\'', 'h']) {
            Some(e) => {
                *apostrophe = elem.ends_with('\'');
                (e, true)
            }
            None => (*elem, false),
        };
        let Some(p) = parse_u32(elem) else {
            return Err(format!("Key path value '{elem}' is not a valid uint32"));
        };
        if p > 0x7FFF_FFFF {
            return Err(format!("Key path value {p} is out of range"));
        }
        out.push(p | (u32::from(hardened) << 31));
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeriveType {
    No,
    Unhardened,
    Hardened,
}

/// A `PubkeyProvider`.
#[derive(Clone)]
enum PubkeyProvider {
    /// `ConstPubkeyProvider`: the serialized key.
    Const(Vec<u8>),
    /// `BIP32PubkeyProvider`.
    Bip32 {
        root: ExtPubKey,
        path: Vec<u32>,
        derive: DeriveType,
        /// Whether the string form writes hardened steps with `'` rather than `h`.
        apostrophe: bool,
    },
    /// `OriginPubkeyProvider`: a key with its `[fingerprint/path]` origin, which derives as
    /// the key it wraps.
    Origin {
        fingerprint: [u8; 4],
        path: Vec<u32>,
        apostrophe: bool,
        key: Box<PubkeyProvider>,
    },
}

impl PubkeyProvider {
    /// `GetSize()`.
    fn size(&self) -> usize {
        match self {
            PubkeyProvider::Const(k) => k.len(),
            PubkeyProvider::Bip32 {
                ..
            } => 33,
            PubkeyProvider::Origin {
                key,
                ..
            } => key.size(),
        }
    }

    fn is_range(&self) -> bool {
        match self {
            PubkeyProvider::Const(_) => false,
            PubkeyProvider::Bip32 {
                derive,
                ..
            } => *derive != DeriveType::No,
            PubkeyProvider::Origin {
                key,
                ..
            } => key.is_range(),
        }
    }

    /// `ToString()`: the hex key; the xpub (of an xprv too), its path and `/*`; an origin in
    /// brackets before its key. `'` or `h` as the key was written.
    // Core: script/descriptor.cpp:204-207, 220, 274, 412-425
    fn to_string(&self, p: &KeyIoParams) -> String {
        match self {
            PubkeyProvider::Const(k) => hex(k),
            PubkeyProvider::Bip32 {
                root,
                path,
                derive,
                apostrophe,
            } => {
                let mut ret = encode_ext_pubkey(root, p) + &format_hd_keypath(path, *apostrophe);
                if *derive != DeriveType::No {
                    ret += "/*";
                    if *derive == DeriveType::Hardened {
                        ret.push(hardened_marker(*apostrophe));
                    }
                }
                ret
            }
            PubkeyProvider::Origin {
                fingerprint,
                path,
                apostrophe,
                key,
            } => {
                format!(
                    "[{}{}]{}",
                    hex(fingerprint),
                    format_hd_keypath(path, *apostrophe),
                    key.to_string(p)
                )
            }
        }
    }

    /// `GetPubKey(pos, arg)` without a cache: the key and its origin, `None` where Core's
    /// returns false. A constant key's origin is its own id's first four bytes and no path; a
    /// BIP32 key's is its root's, with the path and the position; an origin goes before the
    /// key's, its fingerprint replacing it.
    // Core: script/descriptor.cpp:211-217, 264-270, 347-400
    fn pubkey(&self, pos: u32, keys: &Keys) -> Option<(Vec<u8>, KeyOrigin)> {
        let (root, path, derive) = match self {
            PubkeyProvider::Const(k) => {
                let fingerprint = hash160(k)[..4].try_into().expect("4 of 20 bytes");
                return Some((
                    k.clone(),
                    KeyOrigin {
                        fingerprint,
                        path: Vec::new(),
                    },
                ));
            }
            PubkeyProvider::Origin {
                fingerprint,
                path,
                key,
                ..
            } => {
                let (pubkey, inner) = key.pubkey(pos, keys)?;
                let origin = KeyOrigin {
                    fingerprint: *fingerprint,
                    path: path.iter().chain(&inner.path).copied().collect(),
                };
                return Some((pubkey, origin));
            }
            PubkeyProvider::Bip32 {
                root,
                path,
                derive,
                ..
            } => (root, path, *derive),
        };
        let root_id = hash160(&root.pubkey.serialize());
        let mut origin = KeyOrigin {
            fingerprint: root_id[..4].try_into().expect("4 of 20 bytes"),
            path: path.clone(),
        };
        match derive {
            DeriveType::No => {}
            DeriveType::Unhardened => origin.path.push(pos),
            DeriveType::Hardened => origin.path.push(pos | 0x8000_0000),
        }
        let hardened = derive == DeriveType::Hardened || path.iter().any(|e| e >> 31 != 0);
        let last = if hardened {
            let mut xprv = root.with_secret(keys.get(&root_id)?);
            for &entry in path {
                xprv = xprv.derive(entry)?;
            }
            match derive {
                DeriveType::No => xprv,
                DeriveType::Unhardened => xprv.derive(pos)?,
                DeriveType::Hardened => xprv.derive(pos | 0x8000_0000)?,
            }
            .neuter()
        } else {
            let mut xpub = *root;
            for &entry in path {
                xpub = xpub.derive(entry)?;
            }
            match derive {
                DeriveType::Unhardened => xpub.derive(pos)?,
                _ => xpub,
            }
        };
        Some((last.pubkey.serialize().to_vec(), origin))
    }
}

/// `ParsePubkeyInner`. Keys are allowed uncompressed in both of Dash's contexts (top level and
/// inside `sh()`), so Core's "Uncompressed keys are not allowed" is unreachable. `apostrophe`
/// is the flag the origin's path left; a `*'`/`*h` suffix, then each hardened path element,
/// overwrites it, so the last one written wins.
// Core: script/descriptor.cpp:902-966
fn parse_pubkey_inner(
    sp: &str,
    p: &KeyIoParams,
    keys: &mut Keys,
    apostrophe: &mut bool,
) -> Result<PubkeyProvider, String> {
    let mut split: Vec<&str> = sp.split('/').collect();
    let s = split[0];
    if s.is_empty() {
        return Err("No key provided".into());
    }
    if split.len() == 1 {
        if let Some(data) = parse_hex(s) {
            // `CPubKey(data)`: valid when the first byte's size is the data's.
            let valid = pubkey_len(data[0]) == data.len();
            if valid && !matches!(data[0], 2..=4) {
                return Err("Hybrid public keys are not allowed".into());
            }
            if parse_fully_valid(&data).is_some() {
                return Ok(PubkeyProvider::Const(data));
            }
            return Err(format!("Pubkey '{s}' is invalid"));
        }
        if let Some(key) = decode_secret(s, p) {
            let pubkey = key.pubkey();
            keys.entry(hash160(&pubkey)).or_insert(key);
            return Ok(PubkeyProvider::Const(pubkey));
        }
    }
    let extkey = decode_ext_key(s, p);
    let extpubkey = decode_ext_pubkey(s, p);
    let root = match (extkey, extpubkey) {
        (Some(xprv), _) => {
            let xpub = xprv.neuter();
            keys.entry(hash160(&xpub.pubkey.serialize())).or_insert(xprv.key());
            xpub
        }
        (None, Some(xpub)) => xpub,
        (None, None) => return Err(format!("key '{s}' is not valid")),
    };
    let derive = match split.last().copied() {
        Some("*") => DeriveType::Unhardened,
        Some(last @ ("*'" | "*h")) => {
            *apostrophe = last == "*'";
            DeriveType::Hardened
        }
        _ => DeriveType::No,
    };
    if derive != DeriveType::No {
        split.pop();
    }
    let path = parse_key_path(&split, apostrophe)?;
    Ok(PubkeyProvider::Bip32 {
        root,
        path,
        derive,
        apostrophe: *apostrophe,
    })
}

/// `ParsePubkey`: an optional `[fingerprint/path]` origin, then the key.
// Core: script/descriptor.cpp:969-1004
fn parse_pubkey(sp: &str, p: &KeyIoParams, keys: &mut Keys) -> Result<PubkeyProvider, String> {
    let origin: Vec<&str> = sp.split(']').collect();
    if origin.len() > 2 {
        return Err("Multiple ']' characters found for a single pubkey".into());
    }
    let mut apostrophe = false;
    if origin.len() == 1 {
        return parse_pubkey_inner(sp, p, keys, &mut apostrophe);
    }
    let Some(info) = origin[0].strip_prefix('[') else {
        let got = origin[0].chars().next().unwrap_or(']');
        return Err(format!(
            "Key origin start '[ character expected but not found, got '{got}' instead"
        ));
    };
    let slash: Vec<&str> = info.split('/').collect();
    if slash[0].len() != 8 {
        return Err(format!(
            "Fingerprint is not 4 bytes ({} characters instead of 8 characters)",
            slash[0].len()
        ));
    }
    let Some(fingerprint) = parse_hex(slash[0]) else {
        return Err(format!("Fingerprint '{}' is not hex", slash[0]));
    };
    let path = parse_key_path(&slash, &mut apostrophe)?;
    let key = parse_pubkey_inner(origin[1], p, keys, &mut apostrophe)?;
    Ok(PubkeyProvider::Origin {
        fingerprint: fingerprint.try_into().expect("8 hex digits"),
        path,
        apostrophe,
        key: Box::new(key),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    Top,
    P2sh,
}

/// A parsed descriptor (`DescriptorImpl` and its subclasses).
#[derive(Clone)]
enum Descriptor {
    Addr(Destination),
    Raw(Vec<u8>),
    Pk(PubkeyProvider),
    Pkh(PubkeyProvider),
    Multi {
        threshold: u32,
        keys: Vec<PubkeyProvider>,
        sorted: bool,
    },
    Sh(Box<Descriptor>),
    Combo(PubkeyProvider),
}

/// `ParseScript`. `Err("")` is Core's null result with the error untouched.
// Core: script/descriptor.cpp:1007-1129
fn parse_script(
    sp: &mut &str,
    ctx: Context,
    p: &KeyIoParams,
    keys: &mut Keys,
) -> Result<Descriptor, String> {
    let mut e = expr(sp);
    let top = ctx == Context::Top;
    if func("pk", &mut e) {
        let key = parse_pubkey(e, p, keys).map_err(|err| format!("pk(): {err}"))?;
        return Ok(Descriptor::Pk(key));
    }
    if func("pkh", &mut e) {
        let key = parse_pubkey(e, p, keys).map_err(|err| format!("pkh(): {err}"))?;
        return Ok(Descriptor::Pkh(key));
    }
    if func("combo", &mut e) {
        if !top {
            return Err("Can only have combo() at top level".into());
        }
        let key = parse_pubkey(e, p, keys).map_err(|err| format!("combo(): {err}"))?;
        return Ok(Descriptor::Combo(key));
    }
    let sorted = func("sortedmulti", &mut e);
    if sorted || func("multi", &mut e) {
        let threshold = expr(&mut e);
        let Some(thres) = parse_u32(threshold) else {
            return Err(format!("Multi threshold '{threshold}' is not valid"));
        };
        let mut providers = Vec::new();
        let mut script_size = 0;
        while !e.is_empty() {
            if !konst(",", &mut e) {
                let got = e.chars().next().expect("non-empty");
                return Err(format!("Multi: expected ',', got '{got}'"));
            }
            let arg = expr(&mut e);
            let pk = parse_pubkey(arg, p, keys).map_err(|err| format!("Multi: {err}"))?;
            script_size += pk.size() + 1;
            providers.push(pk);
        }
        let n = providers.len();
        if n == 0 || n > MAX_PUBKEYS_PER_MULTISIG {
            return Err(format!(
                "Cannot have {n} keys in multisig; must have between 1 and {MAX_PUBKEYS_PER_MULTISIG} keys, inclusive"
            ));
        } else if thres < 1 {
            return Err(format!("Multisig threshold cannot be {thres}, must be at least 1"));
        } else if thres as usize > n {
            return Err(format!(
                "Multisig threshold cannot be larger than the number of keys; threshold is {thres} but only {n} keys specified"
            ));
        }
        if top && n > 3 {
            return Err(format!(
                "Cannot have {n} pubkeys in bare multisig; only at most 3 pubkeys"
            ));
        }
        if !top && script_size + 3 > MAX_SCRIPT_ELEMENT_SIZE {
            return Err(format!(
                "P2SH script is too large, {} bytes is larger than {MAX_SCRIPT_ELEMENT_SIZE} bytes",
                script_size + 3
            ));
        }
        return Ok(Descriptor::Multi {
            threshold: thres,
            keys: providers,
            sorted,
        });
    }
    if func("sh", &mut e) {
        if !top {
            return Err("Can only have sh() at top level".into());
        }
        let inner = parse_script(&mut e, Context::P2sh, p, keys)?;
        if !e.is_empty() {
            return Err(String::new());
        }
        return Ok(Descriptor::Sh(Box::new(inner)));
    }
    if func("addr", &mut e) {
        if !top {
            return Err("Can only have addr() at top level".into());
        }
        return decode_destination(e, p)
            .map(Descriptor::Addr)
            .ok_or_else(|| "Address is not valid".into());
    }
    if func("raw", &mut e) {
        if !top {
            return Err("Can only have raw() at top level".into());
        }
        let script = parse_hex(e).ok_or("Raw script is not hex")?;
        return Ok(Descriptor::Raw(script));
    }
    if !top {
        return Err("A function is needed within P2SH".into());
    }
    Err(format!("'{e}' is not a valid descriptor function"))
}

/// `Parse(descriptor, out, error, require_checksum)`.
// Core: script/descriptor.cpp:1247-1255
fn parse(
    descriptor: &str,
    p: &KeyIoParams,
    keys: &mut Keys,
    require_checksum: bool,
) -> Result<Descriptor, String> {
    let (mut sp, _) = check_checksum(descriptor, require_checksum)?;
    let desc = parse_script(&mut sp, Context::Top, p, keys)?;
    if !sp.is_empty() {
        return Err(String::new());
    }
    Ok(desc)
}

/// `CScript() << data`, for data below `OP_PUSHDATA1` bytes.
fn push_data(script: &mut Vec<u8>, data: &[u8]) {
    debug_assert!(data.len() < 0x4c);
    script.push(data.len() as u8);
    script.extend_from_slice(data);
}

/// `CScript() << n` (`push_int64`) for 0 ≤ n ≤ 20.
fn push_small_int(script: &mut Vec<u8>, n: usize) {
    match n {
        0 => script.push(OP_0),
        1..=16 => script.push(OP_1 + n as u8 - 1),
        _ => push_data(script, &[n as u8]),
    }
}

/// `GetScriptForMultisig`: the threshold, each key, their count, then `OP_CHECKMULTISIG`.
// Core: script/standard.cpp:217-227
pub(crate) fn multisig_script(threshold: usize, pubkeys: &[Vec<u8>]) -> Vec<u8> {
    let mut s = Vec::new();
    push_small_int(&mut s, threshold);
    for k in pubkeys {
        push_data(&mut s, k);
    }
    push_small_int(&mut s, pubkeys.len());
    s.push(OP_CHECKMULTISIG);
    s
}

/// `GetScriptForRawPubKey`.
fn p2pk(pubkey: &[u8]) -> Vec<u8> {
    let mut s = Vec::with_capacity(pubkey.len() + 2);
    push_data(&mut s, pubkey);
    s.push(OP_CHECKSIG);
    s
}

impl Descriptor {
    /// `ToStringHelper(PUBLIC)` without the checksum: `name(extra,keys...,subscript)`.
    // Core: script/descriptor.cpp:588-614 and each descriptor's ToStringExtra
    fn to_string_inner(&self, p: &KeyIoParams) -> String {
        let keys = |providers: &[PubkeyProvider]| {
            providers.iter().map(|k| k.to_string(p)).collect::<Vec<_>>().join(",")
        };
        match self {
            Descriptor::Addr(dest) => format!("addr({})", encode_destination(dest, p)),
            Descriptor::Raw(script) => format!("raw({})", hex(script)),
            Descriptor::Pk(k) => format!("pk({})", k.to_string(p)),
            Descriptor::Pkh(k) => format!("pkh({})", k.to_string(p)),
            Descriptor::Combo(k) => format!("combo({})", k.to_string(p)),
            Descriptor::Multi {
                threshold,
                keys: providers,
                sorted,
            } => {
                let name = if *sorted {
                    "sortedmulti"
                } else {
                    "multi"
                };
                format!("{name}({threshold},{})", keys(providers))
            }
            Descriptor::Sh(inner) => format!("sh({})", inner.to_string_inner(p)),
        }
    }

    /// `IsSolvable()`: false for `addr()` and `raw()`. An `sh()` asks its inner descriptor as
    /// Core's does, though neither parsing nor inference puts `addr()` or `raw()` below the top.
    // Core: script/descriptor.cpp:557-563, 723, 747
    fn is_solvable(&self) -> bool {
        match self {
            Descriptor::Addr(_) | Descriptor::Raw(_) => false,
            Descriptor::Sh(inner) => inner.is_solvable(),
            _ => true,
        }
    }

    fn is_range(&self) -> bool {
        match self {
            Descriptor::Addr(_) | Descriptor::Raw(_) => false,
            Descriptor::Pk(k) | Descriptor::Pkh(k) | Descriptor::Combo(k) => k.is_range(),
            Descriptor::Multi {
                keys,
                ..
            } => keys.iter().any(PubkeyProvider::is_range),
            Descriptor::Sh(inner) => inner.is_range(),
        }
    }

    /// `ExpandHelper(pos, ...)` then `MakeScripts`: `None` where a key does not derive. `out`
    /// gets each key's origin, `pkh()`'s and `combo()`'s public key, `sh()`'s subscript and
    /// `combo()`'s P2PKH script; all of a level's keys derive before any is recorded, and
    /// `out` is unchanged where a key does not derive.
    // Core: script/descriptor.cpp:659-692 and each descriptor's MakeScripts (714-855)
    fn expand(&self, pos: u32, keys: &Keys, out: &mut SigningProvider) -> Option<Vec<Vec<u8>>> {
        let derive = |providers: &[&PubkeyProvider], out: &mut SigningProvider| {
            let entries =
                providers.iter().map(|k| k.pubkey(pos, keys)).collect::<Option<Vec<_>>>()?;
            Some(
                entries
                    .into_iter()
                    .map(|(pubkey, origin)| {
                        out.add_origin(&pubkey, origin);
                        pubkey
                    })
                    .collect::<Vec<_>>(),
            )
        };
        Some(match self {
            Descriptor::Addr(dest) => vec![dest.script()],
            Descriptor::Raw(script) => vec![script.clone()],
            Descriptor::Pk(k) => vec![p2pk(&derive(&[k], out)?[0])],
            Descriptor::Pkh(k) => {
                let pubkey = derive(&[k], out)?.swap_remove(0);
                let script = Destination::PkHash(hash160(&pubkey)).script();
                out.add_pubkey(pubkey);
                vec![script]
            }
            Descriptor::Multi {
                threshold,
                keys: providers,
                sorted,
            } => {
                let mut pubkeys = derive(&providers.iter().collect::<Vec<_>>(), out)?;
                if *sorted {
                    pubkeys.sort();
                }
                vec![multisig_script(*threshold as usize, &pubkeys)]
            }
            Descriptor::Sh(inner) => {
                // Core expands the subscript into its own provider and merges it in on success.
                let mut sub_out = SigningProvider::default();
                let mut sub = inner.expand(pos, keys, &mut sub_out)?;
                out.merge(sub_out);
                // Core reads `scripts[0]` unchecked.
                debug_assert_eq!(sub.len(), 1, "a sh() subscript is one script");
                let subscript = sub.swap_remove(0);
                let script = Destination::ScriptHash(hash160(&subscript)).script();
                out.add_script(subscript);
                vec![script]
            }
            Descriptor::Combo(k) => {
                let pubkey = derive(&[k], out)?.swap_remove(0);
                let mut scripts = vec![p2pk(&pubkey)];
                if pubkey.len() == 33 {
                    let p2pkh = Destination::PkHash(hash160(&pubkey)).script();
                    out.add_script(p2pkh.clone());
                    scripts.push(p2pkh.clone());
                    scripts.push(Destination::ScriptHash(hash160(&p2pkh)).script());
                }
                out.add_pubkey(pubkey);
                scripts
            }
        })
    }
}

/// A parsed descriptor with the secrets its keys carried (`Parse`'s `FlatSigningProvider`).
pub struct ParsedDescriptor {
    desc: Descriptor,
    keys: Keys,
}

impl ParsedDescriptor {
    /// `Parse(descriptor, out, error, require_checksum)`: `Err` holds Core's error, possibly
    /// empty.
    // Core: script/descriptor.cpp:1247-1255
    pub fn parse(
        descriptor: &str,
        p: &KeyIoParams,
        require_checksum: bool,
    ) -> Result<ParsedDescriptor, String> {
        let mut keys = HashMap::new();
        let desc = parse(descriptor, p, &mut keys, require_checksum)?;
        Ok(ParsedDescriptor {
            desc,
            keys,
        })
    }

    /// `Descriptor::ToString()`: the public form with its checksum.
    // Core: script/descriptor.cpp:616-621
    pub fn to_string(&self, p: &KeyIoParams) -> String {
        add_checksum(self.desc.to_string_inner(p))
    }

    pub fn is_range(&self) -> bool {
        self.desc.is_range()
    }

    pub fn is_solvable(&self) -> bool {
        self.desc.is_solvable()
    }

    /// Whether any key was a WIF or an xprv (`provider.keys.size() > 0`).
    pub fn has_private_keys(&self) -> bool {
        !self.keys.is_empty()
    }

    /// `Expand(pos, ...)`: the scripts at `pos`, `None` where a key does not derive.
    pub fn expand(&self, pos: u32) -> Option<Vec<Vec<u8>>> {
        self.desc.expand(pos, &self.keys, &mut SigningProvider::default())
    }

    /// `Expand(pos, provider, scripts, out)`: as [`expand`](Self::expand), filling `out`.
    // Core: script/descriptor.cpp:694-697
    pub fn expand_into(&self, pos: u32, out: &mut SigningProvider) -> Option<Vec<Vec<u8>>> {
        self.desc.expand(pos, &self.keys, out)
    }
}

/// `InferDescriptor(GetScriptForDestination(ScriptHash(redeem)), keystore)` for a keystore that
/// holds only `redeem` (`createmultisig`'s `FillableSigningProvider`), as its `ToString()`.
/// createmultisig's redeem is always a multisig of at most 520 bytes, so only `sh(multi(...))`
/// is reached, or `addr()` when a hybrid key makes the multisig inference fail.
// Core: rpc/output_script.cpp:149-151; script/descriptor.cpp:1266-1269
pub fn infer_p2sh_descriptor(redeem: &[u8], p: &KeyIoParams) -> String {
    let mut keystore = SigningProvider::default();
    keystore.add_script(redeem.to_vec());
    infer_descriptor_with(&Destination::ScriptHash(hash160(redeem)).script(), &keystore, p)
}

/// `InferDescriptor(script, DUMMY_SIGNING_PROVIDER)->ToString()`, as `ScriptToUniv` prints a
/// script's `desc`: with no keys or scripts known, `pk()` and `multi()` of non-hybrid keys,
/// `addr()` for a script that is its destination's, else `raw()`.
// Core: script/descriptor.cpp:1266-1269; core_write.cpp:165
pub fn infer_descriptor(script: &[u8], p: &KeyIoParams) -> String {
    infer_descriptor_with(script, &SigningProvider::default(), p)
}

/// `InferDescriptor(script, provider)->ToString()`.
// Core: script/descriptor.cpp:1266-1269
pub fn infer_descriptor_with(script: &[u8], provider: &SigningProvider, p: &KeyIoParams) -> String {
    let desc = infer_script(script, Context::Top, provider).expect("the top level always infers");
    add_checksum(desc.to_string_inner(p))
}

/// `InferPubkey`: a constant key unless hybrid (`IsValidNonHybrid`), behind its origin where
/// the provider knows one (printed with `h`). Uncompressed keys are allowed in both of Dash's
/// contexts.
// Core: script/descriptor.cpp:1131-1147
fn infer_pubkey(pubkey: &[u8], provider: &SigningProvider) -> Option<PubkeyProvider> {
    if !matches!(pubkey.first(), Some(2..=4)) {
        return None;
    }
    let key = PubkeyProvider::Const(pubkey.to_vec());
    Some(match provider.origins.get(&hash160(pubkey)) {
        Some(origin) => PubkeyProvider::Origin {
            fingerprint: origin.fingerprint,
            path: origin.path.clone(),
            apostrophe: false,
            key: Box::new(key),
        },
        None => key,
    })
}

/// `InferScript` over `Solver`'s shapes: `pk()`; `pkh()` of a public key the provider knows;
/// `multi()`; at the top, `sh()` of a script the provider knows that infers below it; then
/// `addr()` or `raw()`.
// Core: script/descriptor.cpp:1149-1210
fn infer_script(script: &[u8], ctx: Context, provider: &SigningProvider) -> Option<Descriptor> {
    if let Some(key) = match_pay_to_pubkey(script)
        && let Some(key) = infer_pubkey(key, provider)
    {
        return Some(Descriptor::Pk(key));
    }
    if let Some(id) = match_pay_to_pubkey_hash(script)
        && let Some(pubkey) = provider.pubkeys.get(&id)
        && let Some(key) = infer_pubkey(pubkey, provider)
    {
        return Some(Descriptor::Pkh(key));
    }
    if let Some((threshold, pubkeys)) = match_multisig(script)
        && let Some(keys) = pubkeys.into_iter().map(|k| infer_pubkey(k, provider)).collect()
    {
        return Some(Descriptor::Multi {
            threshold,
            keys,
            sorted: false,
        });
    }
    if ctx == Context::Top
        && let Some(Destination::ScriptHash(id)) = extract_destination(script)
        && let Some(subscript) = provider.scripts.get(&id)
        && let Some(sub) = infer_script(subscript, Context::P2sh, provider)
    {
        return Some(Descriptor::Sh(Box::new(sub)));
    }
    if ctx != Context::Top {
        return None;
    }
    if let Some(dest) = extract_destination(script)
        && dest.script() == script
    {
        return Some(Descriptor::Addr(dest));
    }
    Some(Descriptor::Raw(script.to_vec()))
}

/// `MatchPayToPubkeyHash`: the key id of a P2PKH script (not of a P2PK one, whose destination
/// is a key hash too).
// Core: script/standard.cpp:39-46
fn match_pay_to_pubkey_hash(script: &[u8]) -> Option<[u8; 20]> {
    match extract_destination(script) {
        Some(Destination::PkHash(id)) if match_pay_to_pubkey(script).is_none() => Some(id),
        _ => None,
    }
}

/// `MatchMultisig`: the threshold, the validly sized keys (pushes of any form, as `GetOp` reads
/// them), their count (each count `OP_1`..`OP_16` or a minimal one-byte push of 17..20), then
/// `OP_CHECKMULTISIG`.
// Core: script/standard.cpp:53-113
pub(crate) fn match_multisig(script: &[u8]) -> Option<(u32, Vec<&[u8]>)> {
    let (&OP_CHECKMULTISIG, mut rest) = script.split_last()? else {
        return None;
    };
    let required = multisig_key_count(&mut rest)?;
    let mut keys = Vec::new();
    loop {
        let mut next = rest;
        match get_op(&mut next) {
            Some((_, key)) if valid_size(key) => keys.push(key),
            _ => break,
        }
        rest = next;
    }
    let n_keys = multisig_key_count(&mut rest)?;
    (rest.is_empty() && keys.len() == n_keys as usize && n_keys >= required)
        .then_some((required, keys))
}

/// `GetMultisigKeyCount` of the next op: `OP_1`..`OP_16`, or a minimal one-byte push of 17..20
/// (`IsValidMultisigKeyCount`).
// Core: script/standard.cpp:53-75
fn multisig_key_count(rest: &mut &[u8]) -> Option<u32> {
    let (n, tail) = match *rest {
        [op @ OP_1..=0x60, tail @ ..] => (u32::from(op - OP_1 + 1), tail),
        [1, n @ 17..=20, tail @ ..] => (u32::from(*n), tail),
        _ => return None,
    };
    *rest = tail;
    Some(n)
}

/// `HexStr`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
pub(crate) mod tests;
