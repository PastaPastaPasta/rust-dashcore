//! `ParseScript` from Dash Core's `core_read.cpp`: the language the script and transaction
//! vector files use to write scripts.
//!
//! Tokens are separated by space, tab or newline. A token is, in this order:
//! - a decimal number (optional `-`), pushed like `CScript << int64_t`
//!   (so `0` is `OP_0`, `-1` is `OP_1NEGATE`, `1..16` are `OP_1..OP_16`, anything else a minimal
//!   `CScriptNum` data push). Only `-0xFFFFFFFF..=0xFFFFFFFF` is accepted;
//! - `0x` followed by a non-empty, even-length hex string, inserted as raw bytes (not pushed);
//! - a single-quoted string, pushed as data with the smallest push opcode (no whitespace inside);
//! - an opcode name, with or without the `OP_` prefix.
//!
//! The opcode names Core recognises are those of `GetOpName` for `OP_RESERVED` and
//! `OP_NOP..=MAX_OPCODE` (which is `OP_CHECKDATASIGVERIFY` on Dash). `OP_0..OP_16`,
//! `OP_1NEGATE` and the PUSHDATA opcodes are *not* names here (use numbers or `0x`), and the
//! aliases `OP_NOP2`/`OP_NOP3` do not exist: `GetOpName(0xb1)` is `OP_CHECKLOCKTIMEVERIFY`.

use std::fmt;

use crate::hex::{is_hex, parse_hex};

/// `GetOpName` for the opcodes `ParseScript` can name.
pub const OPCODE_NAMES: &[(u8, &str)] = &[
    (0x50, "OP_RESERVED"),
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
];

// ---------------------------------------------------------------------------------------------
// Reference ports of the parser and the sighash scriptCode serializer (moved from dashcore's
// `tests/core_vectors.rs`, where the O4 review wrote them). They are independent of the node's
// script system, which tests against them.
// ---------------------------------------------------------------------------------------------

/// Core's `GetScriptOp` (`script/script.cpp:259`), reduced to what `SerializeScriptCode` uses: the
/// opcode at `*pc`, with `*pc` moved past the opcode and its size field but, for a push, only past
/// the data when the whole push is present. On failure `*pc` is left where Core leaves it, which is
/// what `SerializeScriptCode` relies on for a truncated trailing push: just after the opcode when
/// its 1/2/4-byte size field is incomplete (Core reads none of it), after the size field when the
/// data is short.
pub fn reference_get_op(script: &[u8], pc: &mut usize) -> Option<u8> {
    if *pc >= script.len() {
        return None;
    }
    let opcode = script[*pc];
    *pc += 1;
    if opcode <= 0x4e {
        let size = if opcode < 0x4c {
            opcode as usize
        } else {
            let width = match opcode {
                0x4c => 1,
                0x4d => 2,
                _ => 4,
            };
            let field = script.get(*pc..*pc + width)?;
            *pc += width;
            field.iter().rev().fold(0usize, |n, b| n << 8 | *b as usize)
        };
        if script.len() - *pc < size {
            return None;
        }
        *pc += size;
    }
    Some(opcode)
}

/// A line-by-line port of Core's `CTransactionSignatureSerializer::SerializeScriptCode`
/// (`script/interpreter.cpp:1366`): the compact-size length followed by the script code with every
/// `OP_CODESEPARATOR` opcode (not data that happens to contain 0xab) removed.
///
/// For a script that ends in a truncated push Core writes the length of the *whole* script minus
/// the separators but only the bytes up to the point where `GetScriptOp` failed, so the pre-image
/// is shorter than its own length prefix says (the data of the truncated push is dropped; quirk
/// 39). The port keeps that, which is why it returns the serialization and not a script.
pub fn reference_serialize_script_code(script: &[u8]) -> Vec<u8> {
    let (mut pc, mut separators) = (0, 0);
    while let Some(opcode) = reference_get_op(script, &mut pc) {
        if opcode == 0xab {
            separators += 1;
        }
    }
    let mut out = Vec::new();
    push_compact_size(&mut out, script.len() - separators);
    let (mut pc, mut begin) = (0, 0);
    while let Some(opcode) = reference_get_op(script, &mut pc) {
        if opcode == 0xab {
            out.extend_from_slice(&script[begin..pc - 1]);
            begin = pc;
        }
    }
    if begin != script.len() {
        out.extend_from_slice(&script[begin..pc]);
    }
    out
}

/// Core's `WriteCompactSize`.
pub(crate) fn push_compact_size(out: &mut Vec<u8>, n: usize) {
    let n = n as u64;
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

/// Why [`parse_script`] failed. Core throws `std::runtime_error`; the harness treats it as a bad
/// test (or aborts the run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseScriptError {
    /// "decimal numeric value only allowed in the range -0xFFFFFFFF...0xFFFFFFFF"
    NumberOutOfRange(String),
    /// "unknown opcode"
    UnknownOpcode(String),
}

impl fmt::Display for ParseScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NumberOutOfRange(w) => write!(
                f,
                "script parse error: decimal numeric value only allowed in the range \
                 -0xFFFFFFFF...0xFFFFFFFF: {w}"
            ),
            Self::UnknownOpcode(w) => write!(f, "script parse error: unknown opcode: {w}"),
        }
    }
}

impl std::error::Error for ParseScriptError {}

fn opcode_by_name(word: &str) -> Option<u8> {
    for &(op, name) in OPCODE_NAMES {
        if name == word {
            return Some(op);
        }
        if let Some(short) = name.strip_prefix("OP_")
            && short == word
        {
            return Some(op);
        }
    }
    None
}

/// `CScriptNum::serialize`
fn script_num_bytes(value: i64) -> Vec<u8> {
    if value == 0 {
        return Vec::new();
    }
    let neg = value < 0;
    let mut abs = value.unsigned_abs();
    let mut out = Vec::new();
    while abs != 0 {
        out.push((abs & 0xff) as u8);
        abs >>= 8;
    }
    if out.last().is_some_and(|b| b & 0x80 != 0) {
        out.push(if neg {
            0x80
        } else {
            0x00
        });
    } else if neg {
        *out.last_mut().expect("non-zero value has bytes") |= 0x80;
    }
    out
}

/// `CScript::operator<<(const std::vector<unsigned char>&)`: the smallest push opcode for the
/// length, with no `OP_N` shortcuts.
pub fn push_data(script: &mut Vec<u8>, data: &[u8]) {
    const OP_PUSHDATA1: u8 = 0x4c;
    const OP_PUSHDATA2: u8 = 0x4d;
    const OP_PUSHDATA4: u8 = 0x4e;
    let len = data.len();
    if len < OP_PUSHDATA1 as usize {
        script.push(len as u8);
    } else if len <= 0xff {
        script.push(OP_PUSHDATA1);
        script.push(len as u8);
    } else if len <= 0xffff {
        script.push(OP_PUSHDATA2);
        script.extend_from_slice(&(len as u16).to_le_bytes());
    } else {
        script.push(OP_PUSHDATA4);
        script.extend_from_slice(&(len as u32).to_le_bytes());
    }
    script.extend_from_slice(data);
}

/// `CScript::operator<<(int64_t)` (`push_int64`)
fn push_int64(script: &mut Vec<u8>, n: i64) {
    const OP_1NEGATE: u8 = 0x4f;
    const OP_1: u8 = 0x51;
    if n == -1 || (1..=16).contains(&n) {
        script.push((n + (OP_1 as i64 - 1)) as u8);
        debug_assert!(n != -1 || script.last() == Some(&OP_1NEGATE));
    } else if n == 0 {
        script.push(0x00);
    } else {
        push_data(script, &script_num_bytes(n));
    }
}

/// `ToIntegral<int64_t>` over a string already known to be `-?[0-9]+`: `std::from_chars`
/// semantics, so leading zeros are fine and overflow is a failure.
fn to_integral_i64(word: &str) -> Option<i64> {
    word.parse::<i64>().ok()
}

/// `SplitString(s, " \t\n")` followed by dropping the empty pieces `ParseScript` ignores.
fn words(s: &str) -> impl Iterator<Item = &str> {
    s.split([' ', '\t', '\n']).filter(|w| !w.is_empty())
}

/// `ParseScript`
pub fn parse_script(s: &str) -> Result<Vec<u8>, ParseScriptError> {
    let mut result = Vec::new();
    for w in words(s) {
        let is_number = w.bytes().all(|b| b.is_ascii_digit())
            || (w.starts_with('-') && w.len() > 1 && w[1..].bytes().all(|b| b.is_ascii_digit()));
        if is_number {
            match to_integral_i64(w) {
                Some(n) if (-0xffff_ffff..=0xffff_ffff).contains(&n) => {
                    push_int64(&mut result, n);
                }
                _ => return Err(ParseScriptError::NumberOutOfRange(w.to_owned())),
            }
        } else if w.len() > 2 && w.starts_with("0x") && is_hex(&w[2..]) {
            result.extend_from_slice(&parse_hex(&w[2..]));
        } else if w.len() >= 2 && w.starts_with('\'') && w.ends_with('\'') {
            push_data(&mut result, &w.as_bytes()[1..w.len() - 1]);
        } else {
            match opcode_by_name(w) {
                Some(op) => result.push(op),
                None => return Err(ParseScriptError::UnknownOpcode(w.to_owned())),
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex::hex_str;

    fn ps(s: &str) -> String {
        hex_str(&parse_script(s).unwrap())
    }

    #[test]
    fn numbers_push_like_cscript() {
        assert_eq!(ps("0 1 16 -1"), "0051604f");
        assert_eq!(ps("17"), "0111");
        assert_eq!(ps("-2"), "0182");
        assert_eq!(ps("127 128 255 256"), "017f".to_owned() + "028000" + "02ff00" + "020001");
        assert_eq!(ps("-128 -129"), "028080".to_owned() + "028180");
        assert_eq!(ps("4294967295 -4294967295"), "05ffffffff00".to_owned() + "05ffffffff80");
        // Leading zeros are fine (`std::from_chars`), and `-0` is `OP_0`.
        assert_eq!(ps("007 -0"), "5700");
    }

    #[test]
    fn number_range_is_enforced() {
        assert!(matches!(parse_script("4294967296"), Err(ParseScriptError::NumberOutOfRange(_))));
        assert!(matches!(parse_script("-4294967296"), Err(ParseScriptError::NumberOutOfRange(_))));
        assert!(matches!(
            parse_script("99999999999999999999"),
            Err(ParseScriptError::NumberOutOfRange(_))
        ));
    }

    #[test]
    fn hex_is_inserted_raw() {
        assert_eq!(ps("0x51 0x0102"), "510102");
        // Not valid raw hex: odd length, empty, or non-hex.
        for bad in ["0x", "0x5", "0x5g", "0X51", "+1", "-", "--1", "1x"] {
            assert!(
                matches!(parse_script(bad), Err(ParseScriptError::UnknownOpcode(_))),
                "{bad:?} must not parse"
            );
        }
    }

    #[test]
    fn quoted_strings_use_smallest_push() {
        assert_eq!(ps("'abc'"), "03616263");
        // The empty string is OP_0, a single byte is not shortened to OP_N.
        assert_eq!(ps("''"), "00");
        assert_eq!(ps("'a'"), "0161");
        let long = format!("'{}'", "a".repeat(76));
        assert_eq!(&ps(&long)[..4], "4c4c");
        let longer = format!("'{}'", "a".repeat(256));
        assert_eq!(&ps(&longer)[..6], "4d0001");
        // A lone quote is an opcode lookup.
        assert!(parse_script("'").is_err());
    }

    #[test]
    fn opcode_names_follow_getopname() {
        assert_eq!(ps("DUP OP_DUP HASH160 EQUALVERIFY CHECKSIG"), "7676a988ac");
        assert_eq!(ps("CHECKLOCKTIMEVERIFY CHECKSEQUENCEVERIFY"), "b1b2");
        assert_eq!(ps("CHECKDATASIG CHECKDATASIGVERIFY SPLIT NUM2BIN BIN2NUM"), "babb7f8081");
        assert_eq!(ps("RESERVED NOP"), "5061");
        // Names that Core does not recognise.
        for bad in [
            "NOP2",
            "OP_NOP2",
            "NOP3",
            "OP_0",
            "OP_1",
            "OP_1NEGATE",
            "PUSHDATA1",
            "OP_TRUE",
            "OP_FALSE",
        ] {
            assert!(parse_script(bad).is_err(), "{bad} must not be a name");
        }
    }

    #[test]
    fn whitespace_is_space_tab_newline_only() {
        assert_eq!(ps("  1\t2\n\n3  "), "515253");
        assert!(parse_script("1\r2").is_err());
        assert_eq!(ps(""), "");
    }

    #[test]
    fn reference_serialize_script_code_edge_cases() {
        // Expected values follow from reading `SerializeScriptCode` and `GetScriptOp` at the pinned
        // commit, not from running Core.
        let cases: [(&[u8], &[u8]); 7] = [
            // Separators are removed, wherever they are.
            (&[0xab, 0x51, 0xab, 0xab, 0x52], &[0x02, 0x51, 0x52]),
            (&[], &[0x00]),
            // 0xab inside push data is data.
            (&[0x02, 0xab, 0xab], &[0x03, 0x02, 0xab, 0xab]),
            // A separator, then a PUSHDATA1 without its size byte: the prefix is 3 - 1, and the
            // opcode is the last byte written.
            (&[0xab, 0x51, 0x4c], &[0x02, 0x51, 0x4c]),
            // PUSHDATA2 whose size field is cut: `GetScriptOp` fails before the size field, so the
            // opcode is written and the lone size byte is not.
            (&[0x51, 0x4d, 0x01], &[0x03, 0x51, 0x4d]),
            // A push with only some of its data: the opcode is written, the data is not.
            (&[0xab, 0x02, 0x01], &[0x02, 0x02]),
            // Nothing after the failing opcode matters, and a separator after it is never seen.
            (&[0x51, 0x03, 0x01, 0xab], &[0x04, 0x51, 0x03]),
        ];
        for (script, expected) in cases {
            assert_eq!(reference_serialize_script_code(script), expected, "{script:02x?}");
        }
    }
}
