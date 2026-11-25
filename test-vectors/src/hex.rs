//! Hex helpers with the exact semantics of Dash Core's `util/strencodings` and `uint256`.
//!
//! These are deliberately *not* the usual strict hex decoders: the vector files and Core's test
//! harness rely on Core's behaviour (whitespace skipping, "invalid input yields an empty vector",
//! `uint256::SetHex` truncating and zero-padding).

/// `HexDigit`: the value of an ASCII hex digit, or `None`.
pub fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `IsSpace`: C locale `isspace`, as in `util/strencodings.h`.
pub fn is_space(c: u8) -> bool {
    matches!(c, b' ' | 0x0c | b'\n' | b'\r' | b'\t' | 0x0b)
}

/// `IsHex`: non-empty, even length, only hex digits (no whitespace, no `0x`).
pub fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.len().is_multiple_of(2) && s.bytes().all(|c| hex_digit(c).is_some())
}

/// `TryParseHex`: skips whitespace between byte pairs, fails on a stray nibble or a non-hex digit.
pub fn try_parse_hex(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len() / 2);
    let mut i = 0;
    while i < b.len() {
        if is_space(b[i]) {
            i += 1;
            continue;
        }
        let c1 = hex_digit(b[i]);
        i += 1;
        if i == b.len() {
            return None;
        }
        let c2 = hex_digit(b[i]);
        i += 1;
        out.push((c1? << 4) | c2?);
    }
    Some(out)
}

/// `ParseHex`: like [`try_parse_hex`] but yields an empty vector for invalid input, as Core does.
pub fn parse_hex(s: &str) -> Vec<u8> {
    try_parse_hex(s).unwrap_or_default()
}

/// `HexStr`: lower-case hex.
pub fn hex_str(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0xf) as usize] as char);
    }
    s
}

/// `uint256::SetHex`, returned in *internal* byte order (the order the value is serialized in).
///
/// The string is the big-endian "display" form. Leading whitespace and a `0x` prefix are skipped,
/// parsing stops at the first non-hex character, strings shorter than 64 digits are zero-padded on
/// the most significant side, and digits beyond the 64th (counting from the end of the digit run)
/// are ignored.
pub fn uint256_from_hex(s: &str) -> [u8; 32] {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && is_space(b[i]) {
        i += 1;
    }
    if i + 1 < b.len() && b[i] == b'0' && b[i + 1].eq_ignore_ascii_case(&b'x') {
        i += 2;
    }
    let mut digits = 0;
    while i + digits < b.len() && hex_digit(b[i + digits]).is_some() {
        digits += 1;
    }
    let digit = |k: usize| hex_digit(b[i + k]).expect("counted as a hex digit");
    let mut out = [0u8; 32];
    let mut p = 0;
    while digits > 0 && p < 32 {
        digits -= 1;
        out[p] = digit(digits);
        if digits > 0 {
            digits -= 1;
            out[p] |= digit(digits) << 4;
            p += 1;
        }
    }
    out
}

/// `uint256::GetHex`: the display form of an internal-order value.
pub fn uint256_to_hex(internal: &[u8; 32]) -> String {
    let mut rev = *internal;
    rev.reverse();
    hex_str(&rev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_matches_core() {
        assert_eq!(parse_hex("00ff"), vec![0x00, 0xff]);
        assert_eq!(parse_hex("00 ff\n10"), vec![0x00, 0xff, 0x10]);
        // A stray nibble, or any non-hex digit, makes Core return an empty vector.
        assert_eq!(parse_hex("0"), Vec::<u8>::new());
        assert_eq!(parse_hex("0g"), Vec::<u8>::new());
        assert_eq!(parse_hex("00f"), Vec::<u8>::new());
        assert_eq!(parse_hex(""), Vec::<u8>::new());
    }

    #[test]
    fn is_hex_matches_core() {
        assert!(is_hex("00"));
        assert!(!is_hex(""));
        assert!(!is_hex("0"));
        assert!(!is_hex("0 "));
        assert!(!is_hex("0x00"));
    }

    #[test]
    fn uint256_set_hex_matches_core() {
        let one = uint256_from_hex("1");
        assert_eq!(one[0], 1);
        assert!(one[1..].iter().all(|&b| b == 0));
        assert_eq!(uint256_from_hex("0x01"), one);
        assert_eq!(uint256_from_hex("  01zz"), one);
        let abc = uint256_from_hex("abc");
        assert_eq!(&abc[..2], &[0xbc, 0x0a]);
        let full = format!("{:064x}", 0x0102u32);
        assert_eq!(uint256_to_hex(&uint256_from_hex(&full)), full);
        // Digits beyond 64 are dropped from the most significant side.
        let long = format!("ff{}", "00".repeat(31) + "01");
        assert_eq!(uint256_from_hex(&long), one);
    }
}
