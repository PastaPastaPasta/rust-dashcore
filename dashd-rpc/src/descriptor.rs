//! Core's output descriptors (`script/descriptor.cpp`) as `getScriptFromDescriptor` reads them
//! for `generatetodescriptor` and `generateblock`: the checksum, the parser for every function
//! Dash's grammar has (`pk`, `pkh`, `combo`, `multi`, `sortedmulti`, `sh`, `addr`, `raw`), with
//! hex, WIF and BIP32 keys and key origins, and `Expand` at position 0. The string forms, the
//! descriptor cache and inference are not needed here (PLAN-RP1a D16).

use std::collections::HashMap;

use crate::key_io::{
    Destination, ExtPubKey, Key, KeyIoParams, OP_0, OP_1, OP_CHECKMULTISIG, OP_CHECKSIG,
    decode_destination, decode_ext_key, decode_ext_pubkey, decode_secret, hash160,
    parse_fully_valid, pubkey_len,
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
    let mut keys = HashMap::new();
    let desc = parse(descriptor, p, &mut keys).map_err(DescriptorError::Parse)?;
    if desc.is_range() {
        return Err(DescriptorError::Ranged);
    }
    let mut scripts = desc.expand(0, &keys).ok_or(DescriptorError::CannotDerive)?;
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

/// `INPUT_CHARSET`.
const INPUT_CHARSET: &[u8] =
    b"0123456789()[],'/*abcdefgh@:$%{}IJKLMNOPQRSTUVWXYZ&+-.;<=>?!^_|~ijklmnopqrstuvwxyzABCDEFGH`#\"\\ ";
/// `CHECKSUM_CHARSET` (bech32's).
const CHECKSUM_CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

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
            .map(|j| char::from(CHECKSUM_CHARSET[((c >> (5 * (7 - j))) & 31) as usize]))
            .collect(),
    )
}

/// `CheckChecksum(sp, require_checksum=false, error)`: the part before the `#`.
// Core: script/descriptor.cpp:1212-1245
fn check_checksum(sp: &str) -> Result<&str, String> {
    let split: Vec<&str> = sp.split('#').collect();
    if split.len() > 2 {
        return Err("Multiple '#' symbols".into());
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
    Ok(split[0])
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
/// `'` or `h` for hardened.
// Core: script/descriptor.cpp:875-899
fn parse_key_path(split: &[&str]) -> Result<Vec<u32>, String> {
    let mut out = Vec::with_capacity(split.len().saturating_sub(1));
    for elem in &split[1..] {
        let (elem, hardened) = match elem.strip_suffix(['\'', 'h']) {
            Some(e) => (e, true),
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

/// A `PubkeyProvider`. An `OriginPubkeyProvider` derives as the key it wraps, so the origin
/// is checked and dropped.
#[derive(Clone)]
enum PubkeyProvider {
    /// `ConstPubkeyProvider`: the serialized key.
    Const(Vec<u8>),
    /// `BIP32PubkeyProvider`.
    Bip32 {
        root: ExtPubKey,
        path: Vec<u32>,
        derive: DeriveType,
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
        }
    }

    fn is_range(&self) -> bool {
        matches!(self, PubkeyProvider::Bip32 { derive, .. } if *derive != DeriveType::No)
    }

    /// `GetPubKey(pos, arg)` without a cache: `None` where Core's returns false.
    // Core: script/descriptor.cpp:268-270, 346-400
    fn pubkey(&self, pos: u32, keys: &Keys) -> Option<Vec<u8>> {
        let (root, path, derive) = match self {
            PubkeyProvider::Const(k) => return Some(k.clone()),
            PubkeyProvider::Bip32 {
                root,
                path,
                derive,
            } => (root, path, *derive),
        };
        let hardened = derive == DeriveType::Hardened || path.iter().any(|e| e >> 31 != 0);
        let last = if hardened {
            let mut xprv = root.with_secret(keys.get(&hash160(&root.pubkey.serialize()))?);
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
        Some(last.pubkey.serialize().to_vec())
    }
}

/// `ParsePubkeyInner`. Keys are allowed uncompressed in both of Dash's contexts (top level and
/// inside `sh()`), so Core's "Uncompressed keys are not allowed" is unreachable.
// Core: script/descriptor.cpp:902-966
fn parse_pubkey_inner(
    sp: &str,
    p: &KeyIoParams,
    keys: &mut Keys,
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
        Some("*'" | "*h") => DeriveType::Hardened,
        _ => DeriveType::No,
    };
    if derive != DeriveType::No {
        split.pop();
    }
    let path = parse_key_path(&split)?;
    Ok(PubkeyProvider::Bip32 {
        root,
        path,
        derive,
    })
}

/// `ParsePubkey`: an optional `[fingerprint/path]` origin, then the key.
// Core: script/descriptor.cpp:969-1004
fn parse_pubkey(sp: &str, p: &KeyIoParams, keys: &mut Keys) -> Result<PubkeyProvider, String> {
    let origin: Vec<&str> = sp.split(']').collect();
    if origin.len() > 2 {
        return Err("Multiple ']' characters found for a single pubkey".into());
    }
    if origin.len() == 1 {
        return parse_pubkey_inner(sp, p, keys);
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
    if parse_hex(slash[0]).is_none() {
        return Err(format!("Fingerprint '{}' is not hex", slash[0]));
    }
    parse_key_path(&slash)?;
    parse_pubkey_inner(origin[1], p, keys)
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

/// `Parse(descriptor, out, error, require_checksum=false)`.
// Core: script/descriptor.cpp:1247-1255
fn parse(descriptor: &str, p: &KeyIoParams, keys: &mut Keys) -> Result<Descriptor, String> {
    let mut sp = check_checksum(descriptor)?;
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

/// `GetScriptForRawPubKey`.
fn p2pk(pubkey: &[u8]) -> Vec<u8> {
    let mut s = Vec::with_capacity(pubkey.len() + 2);
    push_data(&mut s, pubkey);
    s.push(OP_CHECKSIG);
    s
}

impl Descriptor {
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

    /// `ExpandHelper(pos, ...)` then `MakeScripts`: `None` where a key does not derive.
    // Core: script/descriptor.cpp:659-692 and each descriptor's MakeScripts (714-855)
    fn expand(&self, pos: u32, keys: &Keys) -> Option<Vec<Vec<u8>>> {
        Some(match self {
            Descriptor::Addr(dest) => vec![dest.script()],
            Descriptor::Raw(script) => vec![script.clone()],
            Descriptor::Pk(k) => vec![p2pk(&k.pubkey(pos, keys)?)],
            Descriptor::Pkh(k) => {
                vec![Destination::PkHash(hash160(&k.pubkey(pos, keys)?)).script()]
            }
            Descriptor::Multi {
                threshold,
                keys: providers,
                sorted,
            } => {
                let mut pubkeys =
                    providers.iter().map(|k| k.pubkey(pos, keys)).collect::<Option<Vec<_>>>()?;
                if *sorted {
                    pubkeys.sort();
                }
                let mut s = Vec::new();
                push_small_int(&mut s, *threshold as usize);
                for k in &pubkeys {
                    push_data(&mut s, k);
                }
                push_small_int(&mut s, pubkeys.len());
                s.push(OP_CHECKMULTISIG);
                vec![s]
            }
            Descriptor::Sh(inner) => {
                let sub = inner.expand(pos, keys)?;
                // Core reads `scripts[0]` unchecked.
                debug_assert_eq!(sub.len(), 1, "a sh() subscript is one script");
                vec![Destination::ScriptHash(hash160(&sub[0])).script()]
            }
            Descriptor::Combo(k) => {
                let pubkey = k.pubkey(pos, keys)?;
                let mut scripts = vec![p2pk(&pubkey)];
                if pubkey.len() == 33 {
                    let p2pkh = Destination::PkHash(hash160(&pubkey)).script();
                    scripts.push(p2pkh.clone());
                    scripts.push(Destination::ScriptHash(hash160(&p2pkh)).script());
                }
                scripts
            }
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;
