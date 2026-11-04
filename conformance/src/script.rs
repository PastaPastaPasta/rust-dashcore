//! Independent re-implementations of dashd's script renderings, as of Dash Core v24.0.0-rc.3:
//! `ScriptToAsmStr` and `ScriptToUniv` (`src/core_write.cpp`), `Solver`, `ExtractDestination`
//! and `GetTxnOutputType` (`src/script/standard.cpp`), `GetOpName` and `GetScriptOp`
//! (`src/script/script.cpp`), the strict-DER/hash-type checks of `CheckSignatureEncoding`
//! (`src/script/interpreter.cpp`), and `InferDescriptor(script, DUMMY_SIGNING_PROVIDER)
//! ->ToString()` with its checksum (`src/script/descriptor.cpp`).
//!
//! Like [`crate::render`], nothing here uses `dashcore` or `dash_primitives` script code; only
//! the hash primitives come from `dashcore::hashes`.

use dashcore::hashes::{Hash, hash160};
use hex_conservative::DisplayHex;

use crate::render::{BECH32_CHARSET, Dest, Params, encode_destination};

const OP_0: u8 = 0x00;
const OP_PUSHDATA1: u8 = 0x4c;
const OP_PUSHDATA2: u8 = 0x4d;
const OP_PUSHDATA4: u8 = 0x4e;
const OP_1: u8 = 0x51;
const OP_16: u8 = 0x60;
const OP_RETURN: u8 = 0x6a;
const OP_DUP: u8 = 0x76;
const OP_EQUAL: u8 = 0x87;
const OP_EQUALVERIFY: u8 = 0x88;
const OP_HASH160: u8 = 0xa9;
const OP_CHECKSIG: u8 = 0xac;
const OP_CHECKMULTISIG: u8 = 0xae;
const MAX_SCRIPT_SIZE: usize = 10_000;
const MAX_PUBKEYS_PER_MULTISIG: i64 = 20;

/// `GetScriptOp`: the opcode at `*pc` and its push data, advancing `pc`; `None` when the
/// script ends inside the opcode.
fn get_op<'a>(script: &'a [u8], pc: &mut usize) -> Option<(u8, &'a [u8])> {
    let op = *script.get(*pc)?;
    *pc += 1;
    if op > OP_PUSHDATA4 {
        return Some((op, &[]));
    }
    let rest = &script[*pc..];
    let (size, skip) = match op {
        OP_PUSHDATA1 => (usize::from(*rest.first()?), 1),
        OP_PUSHDATA2 => (usize::from(u16::from_le_bytes(rest.get(..2)?.try_into().ok()?)), 2),
        OP_PUSHDATA4 => (u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize, 4),
        n => (usize::from(n), 0),
    };
    let data = rest.get(skip..)?.get(..size)?;
    *pc += skip + size;
    Some((op, data))
}

/// `GetOpName`.
fn op_name(op: u8) -> &'static str {
    const NAMES: &[(u8, &str)] = &[
        (0x00, "0"),
        (0x4c, "OP_PUSHDATA1"),
        (0x4d, "OP_PUSHDATA2"),
        (0x4e, "OP_PUSHDATA4"),
        (0x4f, "-1"),
        (0x50, "OP_RESERVED"),
        (0x51, "1"),
        (0x52, "2"),
        (0x53, "3"),
        (0x54, "4"),
        (0x55, "5"),
        (0x56, "6"),
        (0x57, "7"),
        (0x58, "8"),
        (0x59, "9"),
        (0x5a, "10"),
        (0x5b, "11"),
        (0x5c, "12"),
        (0x5d, "13"),
        (0x5e, "14"),
        (0x5f, "15"),
        (0x60, "16"),
        (0x61, "OP_NOP"),
        (0x62, "OP_VER"),
        (0x63, "OP_IF"),
        (0x64, "OP_NOTIF"),
        (0x65, "OP_VERIF"),
        (0x66, "OP_VERNOTIF"),
        (0x67, "OP_ELSE"),
        (0x68, "OP_ENDIF"),
        (0x69, "OP_VERIFY"),
        (0x6a, "OP_RETURN"),
        (0x6b, "OP_TOALTSTACK"),
        (0x6c, "OP_FROMALTSTACK"),
        (0x6d, "OP_2DROP"),
        (0x6e, "OP_2DUP"),
        (0x6f, "OP_3DUP"),
        (0x70, "OP_2OVER"),
        (0x71, "OP_2ROT"),
        (0x72, "OP_2SWAP"),
        (0x73, "OP_IFDUP"),
        (0x74, "OP_DEPTH"),
        (0x75, "OP_DROP"),
        (0x76, "OP_DUP"),
        (0x77, "OP_NIP"),
        (0x78, "OP_OVER"),
        (0x79, "OP_PICK"),
        (0x7a, "OP_ROLL"),
        (0x7b, "OP_ROT"),
        (0x7c, "OP_SWAP"),
        (0x7d, "OP_TUCK"),
        (0x7e, "OP_CAT"),
        (0x7f, "OP_SPLIT"),
        (0x80, "OP_NUM2BIN"),
        (0x81, "OP_BIN2NUM"),
        (0x82, "OP_SIZE"),
        (0x83, "OP_INVERT"),
        (0x84, "OP_AND"),
        (0x85, "OP_OR"),
        (0x86, "OP_XOR"),
        (0x87, "OP_EQUAL"),
        (0x88, "OP_EQUALVERIFY"),
        (0x89, "OP_RESERVED1"),
        (0x8a, "OP_RESERVED2"),
        (0x8b, "OP_1ADD"),
        (0x8c, "OP_1SUB"),
        (0x8d, "OP_2MUL"),
        (0x8e, "OP_2DIV"),
        (0x8f, "OP_NEGATE"),
        (0x90, "OP_ABS"),
        (0x91, "OP_NOT"),
        (0x92, "OP_0NOTEQUAL"),
        (0x93, "OP_ADD"),
        (0x94, "OP_SUB"),
        (0x95, "OP_MUL"),
        (0x96, "OP_DIV"),
        (0x97, "OP_MOD"),
        (0x98, "OP_LSHIFT"),
        (0x99, "OP_RSHIFT"),
        (0x9a, "OP_BOOLAND"),
        (0x9b, "OP_BOOLOR"),
        (0x9c, "OP_NUMEQUAL"),
        (0x9d, "OP_NUMEQUALVERIFY"),
        (0x9e, "OP_NUMNOTEQUAL"),
        (0x9f, "OP_LESSTHAN"),
        (0xa0, "OP_GREATERTHAN"),
        (0xa1, "OP_LESSTHANOREQUAL"),
        (0xa2, "OP_GREATERTHANOREQUAL"),
        (0xa3, "OP_MIN"),
        (0xa4, "OP_MAX"),
        (0xa5, "OP_WITHIN"),
        (0xa6, "OP_RIPEMD160"),
        (0xa7, "OP_SHA1"),
        (0xa8, "OP_SHA256"),
        (0xa9, "OP_HASH160"),
        (0xaa, "OP_HASH256"),
        (0xab, "OP_CODESEPARATOR"),
        (0xac, "OP_CHECKSIG"),
        (0xad, "OP_CHECKSIGVERIFY"),
        (0xae, "OP_CHECKMULTISIG"),
        (0xaf, "OP_CHECKMULTISIGVERIFY"),
        (0xb0, "OP_NOP1"),
        (0xb1, "OP_CHECKLOCKTIMEVERIFY"),
        (0xb2, "OP_CHECKSEQUENCEVERIFY"),
        (0xb3, "OP_NOP4"),
        (0xb4, "OP_NOP5"),
        (0xb5, "OP_NOP6"),
        (0xb6, "OP_NOP7"),
        (0xb7, "OP_NOP8"),
        (0xb8, "OP_NOP9"),
        (0xb9, "OP_NOP10"),
        (0xba, "OP_CHECKDATASIG"),
        (0xbb, "OP_CHECKDATASIGVERIFY"),
        (0xff, "OP_INVALIDOPCODE"),
    ];
    NAMES.iter().find(|(o, _)| *o == op).map_or("OP_UNKNOWN", |(_, n)| n)
}

/// `CScriptNum::set_vch` (callers only pass at most 4 bytes, so `getint` never clamps).
fn script_num(v: &[u8]) -> i64 {
    let Some(&last) = v.last() else {
        return 0;
    };
    let mut r: i64 = 0;
    for (i, b) in v.iter().enumerate() {
        r |= i64::from(*b) << (8 * i);
    }
    if last & 0x80 != 0 {
        -(r & !(0x80i64 << (8 * (v.len() - 1))))
    } else {
        r
    }
}

/// `IsValidSignatureEncoding` (BIP66 strict DER).
fn is_valid_signature_encoding(sig: &[u8]) -> bool {
    let n = sig.len();
    if !(9..=73).contains(&n) || sig[0] != 0x30 || usize::from(sig[1]) != n - 3 {
        return false;
    }
    let len_r = usize::from(sig[3]);
    if 5 + len_r >= n {
        return false;
    }
    let len_s = usize::from(sig[5 + len_r]);
    if len_r + len_s + 7 != n || sig[2] != 0x02 || len_r == 0 || sig[4] & 0x80 != 0 {
        return false;
    }
    if len_r > 1 && sig[4] == 0 && sig[5] & 0x80 == 0 {
        return false;
    }
    if sig[len_r + 4] != 0x02 || len_s == 0 || sig[len_r + 6] & 0x80 != 0 {
        return false;
    }
    !(len_s > 1 && sig[len_r + 6] == 0 && sig[len_r + 7] & 0x80 == 0)
}

/// `CheckSignatureEncoding(sig, SCRIPT_VERIFY_STRICTENC)` for a non-empty `sig`, including
/// `IsDefinedHashtypeSignature`.
fn strictenc_signature(sig: &[u8]) -> bool {
    is_valid_signature_encoding(sig) && sig.last().is_some_and(|t| (1..=3).contains(&(t & !0x80)))
}

/// `mapSigHashTypes`.
fn sighash_name(t: u8) -> Option<&'static str> {
    Some(match t {
        0x01 => "ALL",
        0x81 => "ALL|ANYONECANPAY",
        0x02 => "NONE",
        0x82 => "NONE|ANYONECANPAY",
        0x03 => "SINGLE",
        0x83 => "SINGLE|ANYONECANPAY",
        _ => return None,
    })
}

/// `CScript::IsUnspendable`.
fn is_unspendable(script: &[u8]) -> bool {
    script.first() == Some(&OP_RETURN) || script.len() > MAX_SCRIPT_SIZE
}

/// `ScriptToAsmStr(script, attempt_sighash_decode)`.
pub fn asm(script: &[u8], attempt_sighash_decode: bool) -> String {
    let mut out = String::new();
    let mut pc = 0;
    while pc < script.len() {
        if !out.is_empty() {
            out.push(' ');
        }
        let Some((op, data)) = get_op(script, &mut pc) else {
            out.push_str("[error]");
            return out;
        };
        if op > OP_PUSHDATA4 {
            out.push_str(op_name(op));
        } else if data.len() <= 4 {
            out.push_str(&script_num(data).to_string());
        } else if attempt_sighash_decode
            && !is_unspendable(script)
            && strictenc_signature(data)
            && let Some((&t, body)) = data.split_last()
            && let Some(name) = sighash_name(t)
        {
            out.push_str(&format!("{}[{name}]", body.to_lower_hex_string()));
        } else {
            out.push_str(&data.to_lower_hex_string());
        }
    }
    out
}

/// `TxoutType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxoutType {
    Nonstandard,
    Pubkey,
    PubkeyHash,
    ScriptHash,
    Multisig,
    NullData,
}

impl TxoutType {
    /// `GetTxnOutputType`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Nonstandard => "nonstandard",
            Self::Pubkey => "pubkey",
            Self::PubkeyHash => "pubkeyhash",
            Self::ScriptHash => "scripthash",
            Self::Multisig => "multisig",
            Self::NullData => "nulldata",
        }
    }
}

/// `CPubKey::ValidSize`.
fn valid_pubkey_size(v: &[u8]) -> bool {
    let want = match v.first() {
        Some(2 | 3) => 33,
        Some(4 | 6 | 7) => 65,
        _ => return false,
    };
    v.len() == want
}

/// `CheckMinimalPush`.
fn minimal_push(data: &[u8], op: u8) -> bool {
    match data.len() {
        0 => op == OP_0,
        1 if (1..=16).contains(&data[0]) || data[0] == 0x81 => false,
        n @ 1..=75 => usize::from(op) == n,
        76..=255 => op == OP_PUSHDATA1,
        256..=65535 => op == OP_PUSHDATA2,
        _ => true,
    }
}

/// `GetMultisigKeyCount`.
fn multisig_count(op: u8, data: &[u8]) -> Option<i64> {
    let n = if (OP_1..=OP_16).contains(&op) {
        i64::from(op - OP_1 + 1)
    } else if op > OP_0 && op <= OP_PUSHDATA4 {
        // CScriptNum(data, fRequireMinimal = true): at most 4 bytes, minimally encoded.
        let minimal_num = data
            .last()
            .is_none_or(|l| l & 0x7f != 0 || (data.len() > 1 && data[data.len() - 2] & 0x80 != 0));
        if !minimal_push(data, op) || data.len() > 4 || !minimal_num {
            return None;
        }
        script_num(data)
    } else {
        return None;
    };
    (n > 0 && n <= MAX_PUBKEYS_PER_MULTISIG).then_some(n)
}

/// `MatchMultisig`: `(required, pubkeys)`.
fn match_multisig(script: &[u8]) -> Option<(i64, Vec<&[u8]>)> {
    if script.last() != Some(&OP_CHECKMULTISIG) {
        return None;
    }
    let mut pc = 0;
    let (op, data) = get_op(script, &mut pc)?;
    let required = multisig_count(op, data)?;
    let mut keys = Vec::new();
    let (op, data) = loop {
        match get_op(script, &mut pc) {
            Some((_, d)) if valid_pubkey_size(d) => keys.push(d),
            Some(x) => break x,
            None => return None,
        }
    };
    let n = multisig_count(op, data)?;
    (keys.len() as i64 == n && n >= required && pc + 1 == script.len()).then_some((required, keys))
}

/// `CScript::IsPushOnly` from offset `pc`.
fn push_only(script: &[u8], mut pc: usize) -> bool {
    while pc < script.len() {
        match get_op(script, &mut pc) {
            Some((op, _)) if op <= OP_16 => {}
            _ => return false,
        }
    }
    true
}

/// `Solver`: the output type and its solutions.
pub fn solver(script: &[u8]) -> (TxoutType, Vec<Vec<u8>>) {
    let n = script.len();
    if n == 23 && script[0] == OP_HASH160 && script[1] == 0x14 && script[22] == OP_EQUAL {
        return (TxoutType::ScriptHash, vec![script[2..22].to_vec()]);
    }
    if n >= 1 && script[0] == OP_RETURN && push_only(script, 1) {
        return (TxoutType::NullData, Vec::new());
    }
    for size in [65usize, 33] {
        if n == size + 2 && usize::from(script[0]) == size && script[n - 1] == OP_CHECKSIG {
            let key = &script[1..=size];
            if valid_pubkey_size(key) {
                return (TxoutType::Pubkey, vec![key.to_vec()]);
            }
            // Core returns false from MatchPayToPubkey here and goes on.
        }
    }
    if n == 25
        && script[0] == OP_DUP
        && script[1] == OP_HASH160
        && script[2] == 20
        && script[23] == OP_EQUALVERIFY
        && script[24] == OP_CHECKSIG
    {
        return (TxoutType::PubkeyHash, vec![script[3..23].to_vec()]);
    }
    if let Some((required, keys)) = match_multisig(script) {
        let mut sol = vec![vec![required as u8]];
        sol.extend(keys.iter().map(|k| k.to_vec()));
        sol.push(vec![keys.len() as u8]);
        return (TxoutType::Multisig, sol);
    }
    (TxoutType::Nonstandard, Vec::new())
}

fn h20(v: &[u8]) -> [u8; 20] {
    v.try_into().expect("20-byte solution")
}

/// `ExtractDestination`.
pub fn extract_destination(script: &[u8]) -> Option<Dest> {
    let (t, sol) = solver(script);
    match t {
        TxoutType::Pubkey => Some(Dest::PkHash(hash160::Hash::hash(&sol[0]).to_byte_array())),
        TxoutType::PubkeyHash => Some(Dest::PkHash(h20(&sol[0]))),
        TxoutType::ScriptHash => Some(Dest::ScriptHash(h20(&sol[0]))),
        _ => None,
    }
}

/// `GetScriptForDestination`.
fn script_for(d: &Dest) -> Vec<u8> {
    match d {
        Dest::PkHash(h) => {
            [&[OP_DUP, OP_HASH160, 20][..], h, &[OP_EQUALVERIFY, OP_CHECKSIG]].concat()
        }
        Dest::ScriptHash(h) => [&[OP_HASH160, 20][..], h, &[OP_EQUAL]].concat(),
    }
}

/// `DescriptorChecksum`.
fn descriptor_checksum(s: &str) -> Option<String> {
    const INPUT: &str = "0123456789()[],'/*abcdefgh@:$%{}IJKLMNOPQRSTUVWXYZ&+-.;<=>?!^_|~ijklmnopqrstuvwxyzABCDEFGH`#\"\\ ";
    fn poly_mod(c: u64, val: u64) -> u64 {
        let c0 = c >> 35;
        let mut c = ((c & 0x7_ffff_ffff) << 5) ^ val;
        for (bit, k) in [0xf5dee51989u64, 0xa9fdca3312, 0x1bab10e32d, 0x3706b1677a, 0x644d626ffd]
            .iter()
            .enumerate()
        {
            if (c0 >> bit) & 1 == 1 {
                c ^= k;
            }
        }
        c
    }
    let (mut c, mut cls, mut count) = (1u64, 0u64, 0);
    for ch in s.chars() {
        let pos = INPUT.find(ch)? as u64;
        c = poly_mod(c, pos & 31);
        cls = cls * 3 + (pos >> 5);
        count += 1;
        if count == 3 {
            c = poly_mod(c, cls);
            cls = 0;
            count = 0;
        }
    }
    if count > 0 {
        c = poly_mod(c, cls);
    }
    for _ in 0..8 {
        c = poly_mod(c, 0);
    }
    c ^= 1;
    Some((0..8).map(|j| BECH32_CHARSET[((c >> (5 * (7 - j))) & 31) as usize] as char).collect())
}

/// `InferDescriptor(script, DUMMY_SIGNING_PROVIDER)->ToString()` for a scriptPubKey.
pub fn descriptor(script: &[u8], p: Params) -> String {
    let (t, sol) = solver(script);
    // InferPubkey rejects hybrid keys (`CPubKey::IsValidNonHybrid`); the script then falls
    // through to addr() or raw().
    let non_hybrid = |k: &[u8]| matches!(k.first(), Some(2..=4));
    let body = match t {
        TxoutType::Pubkey if non_hybrid(&sol[0]) => format!("pk({})", sol[0].to_lower_hex_string()),
        TxoutType::Multisig if sol[1..sol.len() - 1].iter().all(|k| non_hybrid(k)) => {
            let keys: Vec<String> =
                sol[1..sol.len() - 1].iter().map(|k| k.to_lower_hex_string()).collect();
            format!("multi({},{})", sol[0][0], keys.join(","))
        }
        // P2PKH/P2SH: the dummy provider knows no keys or scripts, so they become addr().
        _ => match extract_destination(script).filter(|d| script_for(d) == script) {
            Some(d) => format!("addr({})", encode_destination(&d, p)),
            None => format!("raw({})", script.to_lower_hex_string()),
        },
    };
    // Every character produced above is in the checksum's input set.
    let sum = descriptor_checksum(&body).expect("descriptor characters");
    format!("{body}#{sum}")
}

/// The fields of `ScriptToUniv(script, out, include_hex, include_address)` other than `hex`,
/// as `(key, value)`.
pub fn script_to_univ(
    script: &[u8],
    include_address: bool,
    p: Params,
) -> Vec<(&'static str, String)> {
    let mut out = vec![("asm", asm(script, false))];
    let (t, _) = solver(script);
    if include_address {
        out.push(("desc", descriptor(script, p)));
        if t != TxoutType::Pubkey
            && let Some(d) = extract_destination(script)
        {
            out.push(("address", encode_destination(&d, p)));
        }
    }
    out.push(("type", t.name().to_string()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> Vec<u8> {
        hex_conservative::decode_to_vec(s).unwrap()
    }

    #[test]
    fn asm_rendering() {
        let p2pkh = h("76a914a2d5dbbd4dc8d0c22e48e4a0a1b3fd47c6c4ab4f88ac");
        assert_eq!(
            asm(&p2pkh, false),
            "OP_DUP OP_HASH160 a2d5dbbd4dc8d0c22e48e4a0a1b3fd47c6c4ab4f OP_EQUALVERIFY OP_CHECKSIG"
        );
        // Small pushes are CScriptNum; OP_0, OP_1NEGATE and OP_N are names; truncated push.
        assert_eq!(asm(&h("0002ff00038180800051604f"), false), "0 255 -32897 0 1 16 -1");
        assert_eq!(asm(&h("0181"), false), "-1");
        assert_eq!(asm(&h("6a05aabbccddee4c"), false), "OP_RETURN aabbccddee [error]");
        assert_eq!(asm(&h("bcff"), false), "OP_UNKNOWN OP_INVALIDOPCODE");
    }

    #[test]
    fn sighash_decoding() {
        // A P2PKH spend and dashd's rendering of it (tests/data/testnet-7090).
        let sig = "3044022049662925674c9fdbc6a19c60251cc40e6b1aa33e86edb90cb34c8cfee74503b20220174c7be0cab5eb09671a26c3534479bd6835219524c2b27bd0d8e250de8f3cd2";
        let key = "0344aa993eb3a0b8140056af5e17bf4f486eedca950ab16269aa9a917591afa8c0";
        let script = h(&format!("47{sig}0121{key}"));
        assert_eq!(asm(&script, true), format!("{sig}[ALL] {key}"));
        assert_eq!(asm(&script, false), format!("{sig}01 {key}"));
        // An undefined hash type is printed raw; so is anything after OP_RETURN.
        assert_eq!(asm(&h(&format!("47{sig}04")), true), format!("{sig}04"));
        assert_eq!(asm(&h(&format!("6a47{sig}01")), true), format!("OP_RETURN {sig}01"));
    }

    #[test]
    fn solver_and_descriptors() {
        let p2pkh = h("76a914a2d5dbbd4dc8d0c22e48e4a0a1b3fd47c6c4ab4f88ac");
        assert_eq!(solver(&p2pkh).0, TxoutType::PubkeyHash);
        let p2sh = h("a914a2d5dbbd4dc8d0c22e48e4a0a1b3fd47c6c4ab4f87");
        assert_eq!(solver(&p2sh).0, TxoutType::ScriptHash);
        let key = "02".to_string() + &"11".repeat(32);
        let p2pk = h(&format!("21{key}ac"));
        assert_eq!(solver(&p2pk).0, TxoutType::Pubkey);
        assert!(descriptor(&p2pk, Params::Main).starts_with(&format!("pk({key})#")));
        let fields = script_to_univ(&p2pk, true, Params::Main);
        assert!(!fields.iter().any(|(k, _)| *k == "address"), "P2PK has no address field");
        let ms = h(&format!("5121{key}21{key}52ae"));
        assert_eq!(solver(&ms).0, TxoutType::Multisig);
        assert!(descriptor(&ms, Params::Main).starts_with(&format!("multi(1,{key},{key})#")));
        assert_eq!(solver(&h("6a0401020304")).0, TxoutType::NullData);
        assert_eq!(solver(&h("6a61")).0, TxoutType::Nonstandard);
        assert!(descriptor(&h("6a"), Params::Main).starts_with("raw(6a)#"));
        // Hybrid keys (0x06/0x07) are pubkey/multisig outputs but not pk()/multi() descriptors.
        let hybrid = "06".to_string() + &"11".repeat(64);
        let p2pk_hybrid = h(&format!("41{hybrid}ac"));
        assert_eq!(solver(&p2pk_hybrid).0, TxoutType::Pubkey);
        assert!(descriptor(&p2pk_hybrid, Params::Main).starts_with(&format!("raw(41{hybrid}ac)#")));
        let ms_hybrid = h(&format!("5121{key}41{hybrid}52ae"));
        assert_eq!(solver(&ms_hybrid).0, TxoutType::Multisig);
        assert!(descriptor(&ms_hybrid, Params::Main).starts_with("raw("));
        assert!(descriptor(&[], Params::Main).starts_with("raw()#"));
    }

    #[test]
    fn descriptor_checksum_matches_core() {
        // Bitcoin Core's doc/descriptors.md, and dashd's rendering in tests/data/testnet-7090.
        assert_eq!(descriptor_checksum("raw(deadbeef)").as_deref(), Some("89f8spxm"));
        assert_eq!(
            descriptor_checksum("addr(yTZg6eePKxbJZyoaC93bVrTUq5vjhFrbst)").as_deref(),
            Some("6z696h5u")
        );
        let p2pk = h("210208757bbcd988510d37a86a8c703f9b57543fa343bcf5bd583d444f213b08df03ac");
        assert_eq!(
            descriptor(&p2pk, Params::Test),
            "pk(0208757bbcd988510d37a86a8c703f9b57543fa343bcf5bd583d444f213b08df03)#nrcfcq6k"
        );
    }
}
