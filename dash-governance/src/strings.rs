//! Core's string helpers the governance rules use: hex, fixed-point amounts, `SplitString`,
//! `ParseHashStr`, `ValueFromAmount`, and the address codec (notes D12).

use dash_consensus::hash::sha256d;
use dash_params::ChainParams;

/// `IsSpace`: the C locale's `isspace`.
// Core: util/strencodings.h:168-170
pub fn is_space(c: u8) -> bool {
    matches!(c, b' ' | 0x0c | b'\n' | b'\r' | b'\t' | 0x0b)
}

/// `HexDigit`.
fn hex_digit(c: u8) -> Option<u8> {
    char::from(c).to_digit(16).and_then(|d| u8::try_from(d).ok())
}

/// `HexStr`: lower-case hex.
pub fn hex_str(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    s
}

/// `uint256::ToString()`: the display (reversed) hex of internal-order bytes.
pub fn hash_hex(internal: &[u8; 32]) -> String {
    let mut be = *internal;
    be.reverse();
    hex_str(&be)
}

/// `TryParseHex`: pairs of hex digits, with `IsSpace` skipped before each pair only.
// Core: util/strencodings.cpp:81-97
pub fn try_parse_hex(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut it = s.iter().copied();
    while let Some(c) = it.next() {
        if is_space(c) {
            continue;
        }
        let c2 = it.next()?;
        out.push((hex_digit(c)? << 4) | hex_digit(c2)?);
    }
    Some(out)
}

/// `ParseHex`: [`try_parse_hex`], or empty on invalid input.
// Core: util/strencodings.h:65-68
pub fn parse_hex(s: &[u8]) -> Vec<u8> {
    try_parse_hex(s).unwrap_or_default()
}

/// `IsHex`: non-empty, even length, hex digits only.
// Core: util/strencodings.cpp:62-68
pub fn is_hex(s: &[u8]) -> bool {
    !s.is_empty() && s.len().is_multiple_of(2) && s.iter().all(|&c| hex_digit(c).is_some())
}

/// `ParseHashStr`: 64 hex digits, as the internal-order bytes `SetHex` stores.
// Core: core_read.cpp:161-168
pub fn parse_hash_str(s: &[u8]) -> Option<[u8; 32]> {
    if s.len() != 64 || !is_hex(s) {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(s.chunks(2).rev()) {
        *slot = (hex_digit(pair[0])? << 4) | hex_digit(pair[1])?;
    }
    Some(out)
}

/// `SplitString(str, sep)`: the pieces between separators, empty ones included; never empty.
// Core: util/string.h:21-24, util/spanparsing.h Split
pub fn split_string(s: &[u8], sep: u8) -> Vec<&[u8]> {
    s.split(|&c| c == sep).collect()
}

/// `COIN`.
pub const COIN: i64 = 100_000_000;

/// `ValueFromAmount(amount).write()`: `[-]units.cccccccc`.
// Core: core_write.cpp:35-46
pub fn value_from_amount(amount: i64) -> String {
    let (quotient, remainder) = (amount / COIN, amount % COIN);
    let sign = if amount < 0 {
        "-"
    } else {
        ""
    };
    format!("{sign}{}.{:08}", quotient.unsigned_abs(), remainder.unsigned_abs())
}

/// `ParseFixedPoint`'s bound: 10^18 - 1.
const UPPER_BOUND: i64 = 1_000_000_000_000_000_000 - 1;

/// `ProcessMantissaDigit`.
// Core: util/strencodings.cpp:336-350
fn process_mantissa_digit(ch: u8, mantissa: &mut i64, tzeros: &mut i32) -> bool {
    if ch == b'0' {
        *tzeros += 1;
    } else {
        for _ in 0..=*tzeros {
            if *mantissa > UPPER_BOUND / 10 {
                return false;
            }
            *mantissa *= 10;
        }
        *mantissa += i64::from(ch - b'0');
        *tzeros = 0;
    }
    true
}

/// `ParseFixedPoint(val, decimals, &out)`.
// Core: util/strencodings.cpp:352-442
pub fn parse_fixed_point(val: &[u8], decimals: i64) -> Option<i64> {
    let mut mantissa: i64 = 0;
    let mut exponent: i64 = 0;
    let mut tzeros: i32 = 0;
    let mut point_ofs: i64 = 0;
    let mut ptr = 0;
    let end = val.len();
    let digit = |p: usize| p < end && val[p].is_ascii_digit();

    let mantissa_sign = ptr < end && val[ptr] == b'-';
    if mantissa_sign {
        ptr += 1;
    }
    if ptr >= end {
        return None;
    }
    if val[ptr] == b'0' {
        ptr += 1;
    } else if (b'1'..=b'9').contains(&val[ptr]) {
        while digit(ptr) {
            if !process_mantissa_digit(val[ptr], &mut mantissa, &mut tzeros) {
                return None;
            }
            ptr += 1;
        }
    } else {
        return None;
    }
    if ptr < end && val[ptr] == b'.' {
        ptr += 1;
        if !digit(ptr) {
            return None;
        }
        while digit(ptr) {
            if !process_mantissa_digit(val[ptr], &mut mantissa, &mut tzeros) {
                return None;
            }
            ptr += 1;
            point_ofs += 1;
        }
    }
    let mut exponent_sign = false;
    if ptr < end && (val[ptr] == b'e' || val[ptr] == b'E') {
        ptr += 1;
        if ptr < end && val[ptr] == b'+' {
            ptr += 1;
        } else if ptr < end && val[ptr] == b'-' {
            exponent_sign = true;
            ptr += 1;
        }
        if !digit(ptr) {
            return None;
        }
        while digit(ptr) {
            if exponent > UPPER_BOUND / 10 {
                return None;
            }
            exponent = exponent * 10 + i64::from(val[ptr] - b'0');
            ptr += 1;
        }
    }
    if ptr != end {
        return None;
    }
    if exponent_sign {
        exponent = -exponent;
    }
    exponent = exponent - point_ofs + i64::from(tzeros);
    if mantissa_sign {
        mantissa = -mantissa;
    }
    exponent += decimals;
    if !(0..18).contains(&exponent) {
        return None;
    }
    for _ in 0..exponent {
        if !(-(UPPER_BOUND / 10)..=UPPER_BOUND / 10).contains(&mantissa) {
            return None;
        }
        mantissa *= 10;
    }
    if !(-UPPER_BOUND..=UPPER_BOUND).contains(&mantissa) {
        return None;
    }
    Some(mantissa)
}

const BASE58: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

fn base58_value(c: u8) -> Option<u32> {
    BASE58.iter().position(|&b| b == c).and_then(|i| u32::try_from(i).ok())
}

/// `DecodeBase58(str, vch, max_ret_len)`, with `ContainsNoNUL`.
// Core: base58.cpp:38-90, 131-137
fn decode_base58(s: &[u8], max_ret_len: usize) -> Option<Vec<u8>> {
    if s.contains(&0) {
        return None;
    }
    let mut p = 0;
    while p < s.len() && is_space(s[p]) {
        p += 1;
    }
    let mut zeroes = 0;
    while p < s.len() && s[p] == b'1' {
        zeroes += 1;
        if zeroes > max_ret_len {
            return None;
        }
        p += 1;
    }
    let size = (s.len() - p) * 733 / 1000 + 1;
    let mut b256 = vec![0u8; size];
    let mut length = 0;
    while p < s.len() && !is_space(s[p]) {
        let mut carry = base58_value(s[p])?;
        let mut i = 0;
        for slot in b256.iter_mut().rev() {
            if carry == 0 && i >= length {
                break;
            }
            carry += 58 * u32::from(*slot);
            *slot = (carry % 256) as u8;
            carry /= 256;
            i += 1;
        }
        length = i;
        if length + zeroes > max_ret_len {
            return None;
        }
        p += 1;
    }
    while p < s.len() && is_space(s[p]) {
        p += 1;
    }
    if p != s.len() {
        return None;
    }
    let mut out = vec![0u8; zeroes];
    out.extend_from_slice(&b256[size - length..]);
    Some(out)
}

/// `DecodeBase58Check(str, vch, max_ret_len)`.
// Core: base58.cpp:148-172
fn decode_base58_check(s: &[u8], max_ret_len: usize) -> Option<Vec<u8>> {
    let mut data = decode_base58(s, max_ret_len + 4)?;
    if data.len() < 4 {
        return None;
    }
    let (payload, check) = data.split_at(data.len() - 4);
    if sha256d(payload)[..4] != *check {
        return None;
    }
    data.truncate(data.len() - 4);
    Some(data)
}

/// `EncodeBase58Check`.
// Core: base58.cpp:92-129, 139-146
fn encode_base58_check(payload: &[u8]) -> String {
    let mut input = payload.to_vec();
    input.extend_from_slice(&sha256d(payload)[..4]);
    let zeroes = input.iter().take_while(|&&b| b == 0).count();
    let mut b58: Vec<u8> = Vec::new(); // little-endian base-58 digits
    for &byte in &input[zeroes..] {
        let mut carry = u32::from(byte);
        for digit in b58.iter_mut() {
            carry += 256 * u32::from(*digit);
            *digit = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            b58.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut s = "1".repeat(zeroes);
    s.extend(b58.iter().rev().map(|&d| char::from(BASE58[usize::from(d)])));
    s
}

/// A `CTxDestination` that `IsValidDestination` accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Destination {
    PubKeyHash([u8; 20]),
    ScriptHash([u8; 20]),
}

impl Destination {
    /// `GetScriptForDestination`.
    // Core: script/standard.cpp:313-341
    pub fn script(&self) -> Vec<u8> {
        match self {
            Destination::PubKeyHash(h) => {
                let mut s = vec![0x76, 0xa9, 0x14];
                s.extend_from_slice(h);
                s.extend_from_slice(&[0x88, 0xac]);
                s
            }
            Destination::ScriptHash(h) => {
                let mut s = vec![0xa9, 0x14];
                s.extend_from_slice(h);
                s.push(0x87);
                s
            }
        }
    }

    /// `EncodeDestination`.
    // Core: key_io.cpp:20-45, 185-188
    pub fn encode(&self, params: &ChainParams) -> String {
        let (prefix, hash) = match self {
            Destination::PubKeyHash(h) => (params.addr_params.pubkey_addr, h),
            Destination::ScriptHash(h) => (params.addr_params.script_addr, h),
        };
        let mut data = vec![prefix];
        data.extend_from_slice(hash);
        encode_base58_check(&data)
    }
}

/// `DecodeDestination(str)` when `IsValidDestination` holds: a base58check P2PKH or P2SH address
/// of the network. A string starting with the Platform HRP (any case) is never one.
// Core: key_io.cpp:42-104
pub fn decode_destination(s: &[u8], params: &ChainParams) -> Option<Destination> {
    if let Some(hrp) = params.platform_hrp {
        let head = &s[..s.len().min(hrp.len())];
        if head.to_ascii_lowercase() == hrp.as_bytes() {
            return None;
        }
    }
    let data = decode_base58_check(s, 21)?;
    let (&prefix, rest) = data.split_first()?;
    let hash: [u8; 20] = rest.try_into().ok()?;
    if prefix == params.addr_params.pubkey_addr {
        Some(Destination::PubKeyHash(hash))
    } else if prefix == params.addr_params.script_addr {
        Some(Destination::ScriptHash(hash))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::chain;
    use dash_params::NetworkKind;

    fn mainnet() -> ChainParams {
        chain(NetworkKind::Mainnet)
    }

    #[test]
    fn hex_helpers() {
        assert_eq!(try_parse_hex(b" 0a 1B"), Some(vec![0x0a, 0x1b]));
        // A space inside a pair is not skipped.
        assert_eq!(try_parse_hex(b"0 a"), None);
        assert_eq!(try_parse_hex(b"abc"), None);
        assert_eq!(parse_hex(b"zz"), Vec::<u8>::new());
        assert!(!is_hex(b""));
        assert!(!is_hex(b"abc"));
        let h =
            parse_hash_str(&[b'0'; 63].iter().chain(b"1").copied().collect::<Vec<_>>()).unwrap();
        assert_eq!(h[0], 1);
        assert_eq!(hash_hex(&h), format!("{}1", "0".repeat(63)));
    }

    #[test]
    fn fixed_point() {
        assert_eq!(parse_fixed_point(b"1", 8), Some(COIN));
        assert_eq!(parse_fixed_point(b"0.00000001", 8), Some(1));
        assert_eq!(parse_fixed_point(b"0.000000001", 8), None);
        assert_eq!(parse_fixed_point(b"1e-8", 8), Some(1));
        assert_eq!(parse_fixed_point(b"9999999999.99999999", 8), Some(999_999_999_999_999_999));
        assert_eq!(parse_fixed_point(b"10000000000", 8), None);
        assert_eq!(parse_fixed_point(b"01", 8), None);
        assert_eq!(parse_fixed_point(b"1.", 8), None);
        assert_eq!(parse_fixed_point(b"-1", 8), Some(-COIN));
        assert_eq!(parse_fixed_point(b"", 8), None);
    }

    #[test]
    fn amounts_and_splits() {
        assert_eq!(value_from_amount(150_000_000), "1.50000000");
        assert_eq!(value_from_amount(-1), "-0.00000001");
        assert_eq!(split_string(b"", b'|'), vec![&b""[..]]);
        assert_eq!(split_string(b"a||b", b'|'), vec![&b"a"[..], b"", b"b"]);
    }

    #[test]
    fn addresses_round_trip_with_spaces() {
        let p = mainnet();
        let dest = Destination::PubKeyHash([7; 20]);
        let s = dest.encode(&p);
        assert!(s.starts_with('X'));
        assert_eq!(decode_destination(s.as_bytes(), &p), Some(dest));
        assert_eq!(decode_destination(format!(" {s}\n").as_bytes(), &p), Some(dest));
        assert_eq!(decode_destination(format!("{s} x").as_bytes(), &p), None);
        assert_eq!(decode_destination(format!("{s}\0").as_bytes(), &p), None);
        let script = Destination::ScriptHash([9; 20]);
        assert_eq!(decode_destination(script.encode(&p).as_bytes(), &p), Some(script));
        assert_eq!(decode_destination(b"DASH1qqq", &p), None);
    }
}
