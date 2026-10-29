//! Core's value parsers for init options (`util/moneystr.cpp`, `util/strencodings.cpp`).

/// Core's `COIN`.
pub const COIN: i64 = 100_000_000;
/// Core's `MAX_MONEY`.
pub const MAX_MONEY: i64 = 21_000_000 * COIN;

/// Core's `ParseMoney`: `<digits>[.<digits>]` with at most ten whole digits, trimmed of
/// surrounding whitespace; fractional digits beyond eight are ignored.
// Core: util/moneystr.cpp:42-86
pub fn parse_money(s: &str) -> Option<i64> {
    if s.contains('\0') {
        return None;
    }
    let s = trim_string(s);
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let mut whole = String::new();
    let mut units: i64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'.' {
            i += 1;
            let mut mult = COIN / 10;
            while i < bytes.len() && bytes[i].is_ascii_digit() && mult > 0 {
                units += mult * i64::from(bytes[i] - b'0');
                i += 1;
                mult /= 10;
            }
            break;
        }
        if !c.is_ascii_digit() {
            return None;
        }
        whole.push(char::from(c));
        i += 1;
    }
    if i < bytes.len() || whole.len() > 10 || !(0..=COIN).contains(&units) {
        return None;
    }
    // `LocaleIndependentAtoi` of digits only: empty is 0.
    let whole: i64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let value = whole * COIN + units;
    (0..=MAX_MONEY).contains(&value).then_some(value)
}

/// Core's `TrimString` with its default pattern (`" \f\n\r\t\v"`).
fn trim_string(s: &str) -> &str {
    s.trim_matches([' ', '\x0c', '\n', '\r', '\t', '\x0b'])
}

/// Core's `AmountErrMsg`.
pub fn amount_err_msg(option: &str, value: &str) -> String {
    format!("Invalid amount for -{option}=<amount>: '{value}'")
}

/// Core's `ByteUnit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum ByteUnit {
    Noop = 1,
    K = 1 << 10,
    M = 1 << 20,
}

/// Core's `ParseByteUnits`: a decimal count with an optional `k`/`K`/`m`/`M`/`g`/`G`/`t`/`T`
/// suffix (lower case decimal, upper case binary), else `default` units.
// Core: util/strencodings.cpp:501-546
pub fn parse_byte_units(s: &str, default: ByteUnit) -> Option<u64> {
    let last = *s.as_bytes().last()?;
    let unit: Option<u64> = match last {
        b'k' => Some(1_000),
        b'K' => Some(1 << 10),
        b'm' => Some(1_000_000),
        b'M' => Some(1 << 20),
        b'g' => Some(1_000_000_000),
        b'G' => Some(1 << 30),
        b't' => Some(1_000_000_000_000),
        b'T' => Some(1 << 40),
        _ => None,
    };
    let (digits, mult) = match unit {
        Some(mult) => (&s[..s.len() - 1], mult),
        None => (s, default as u64),
    };
    let n = to_integral_u64(digits)?;
    n.checked_mul(mult)
}

/// Core's `ToIntegral<uint64_t>` (`std::from_chars`): decimal digits only, the whole string.
pub fn to_integral_u64(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Core's `ParseInt32`: optional `+`/`-`, decimal digits, no whitespace, in range.
pub fn parse_int32(s: &str) -> Option<i32> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // `+-1` and `-+1` are not numbers; `strip_prefix` took only one sign.
    let s = s.strip_prefix('+').unwrap_or(s);
    s.parse().ok()
}

/// Core's `IsHexNumber`: an optional `0x`, then at least one hex digit and nothing else.
pub fn is_hex_number(s: &str) -> bool {
    let s = s.strip_prefix("0x").unwrap_or(s);
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Core's `uint256S`, in the blob's own (little-endian) byte order: leading spaces and a `0x`
/// skipped, then the hex digits up to the first other character, read from the last; digits
/// past 64 are dropped from the front, and anything else reads as zero.
// Core: uint256.cpp:20-44 (SetHex), util/strencodings.h:236-239 (IsSpace)
pub fn uint256_s(s: &str) -> [u8; 32] {
    // `IsSpace`: ' ', '\t', '\n', '\v', '\f' and '\r'.
    let s = s.trim_start_matches(|c| matches!(c, ' ' | '\t'..='\r'));
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    let digits: Vec<u8> =
        s.bytes().map_while(|c| char::from(c).to_digit(16).map(|d| d as u8)).collect();
    let mut blob = [0; 32];
    for (byte, pair) in blob.iter_mut().zip(digits.rchunks(2)) {
        *byte = match *pair {
            [hi, lo] => hi << 4 | lo,
            [lo] => lo,
            _ => unreachable!("rchunks(2)"),
        };
    }
    blob
}

/// Core's `CFeeRate::ToString()`: `<DASH>.<8 digits> DASH/kB`.
pub fn fee_rate_string(sat_per_kb: i64) -> String {
    format!("{}.{:08} DASH/kB", sat_per_kb / COIN, sat_per_kb % COIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_money_matches_core_s_util_tests() {
        // Core v24.0.0-rc.3: src/test/util_tests.cpp:352-411 (`util_ParseMoney`).
        for (s, v) in [
            ("0.0", 0),
            (".", 0),
            ("0.", 0),
            (".0", 0),
            (".6789", 6789_0000),
            ("12345.", COIN * 12345),
            ("12345.6789", (COIN / 10000) * 123456789),
            ("10000000.00", COIN * 10000000),
            ("1000000.00", COIN * 1000000),
            ("100000.00", COIN * 100000),
            ("10000.00", COIN * 10000),
            ("1000.00", COIN * 1000),
            ("100.00", COIN * 100),
            ("10.00", COIN * 10),
            ("1.00", COIN),
            ("1", COIN),
            ("   1", COIN),
            ("1   ", COIN),
            ("  1 ", COIN),
            ("0.1", COIN / 10),
            ("0.01", COIN / 100),
            ("0.001", COIN / 1000),
            ("0.0001", COIN / 10000),
            ("0.00001", COIN / 100000),
            ("0.000001", COIN / 1000000),
            ("0.0000001", COIN / 10000000),
            ("0.00000001", COIN / 100000000),
            (" 0.00000001 ", COIN / 100000000),
            ("0.00000001 ", COIN / 100000000),
            (" 0.00000001", COIN / 100000000),
        ] {
            assert_eq!(parse_money(s), Some(v), "{s:?}");
        }
        for s in [
            "100000000.00",
            "0.000000001",
            "",
            " ",
            "  ",
            "..",
            "0..0",
            "1 2",
            " 1 2 ",
            " 1.2 3 ",
            " 1 2.3 ",
            " -1 .2  ",
            "  1 .2  ",
            " +1 .2  ",
            "92233720368.54775808",
            "1\0",
        ] {
            assert_eq!(parse_money(s), None, "{s:?}");
        }
    }

    #[test]
    fn parse_byte_units_matches_core_s_util_tests() {
        // Core v24.0.0-rc.3: src/test/util_tests.cpp:1842-1888 (`util_ParseByteUnits`).
        let noop = ByteUnit::Noop;
        assert_eq!(parse_byte_units("1", noop), Some(1));
        assert_eq!(parse_byte_units("0", noop), Some(0));
        assert_eq!(parse_byte_units("1k", noop), Some(1000));
        assert_eq!(parse_byte_units("1K", noop), Some(1 << 10));
        assert_eq!(parse_byte_units("2m", noop), Some(2_000_000));
        assert_eq!(parse_byte_units("2M", noop), Some(2 << 20));
        assert_eq!(parse_byte_units("3g", noop), Some(3_000_000_000));
        assert_eq!(parse_byte_units("3G", noop), Some(3 << 30));
        assert_eq!(parse_byte_units("4t", noop), Some(4_000_000_000_000));
        assert_eq!(parse_byte_units("4T", noop), Some(4 << 40));
        assert_eq!(parse_byte_units("5", ByteUnit::K), Some(5 << 10));
        assert_eq!(parse_byte_units("020M", noop), Some(20 << 20));
        for s in
            ["", "foo", "123m ", " 123m", "-123m", "+123m", "0.5T", "18446744073709551615g", "1x"]
        {
            assert_eq!(parse_byte_units(s, noop), None, "{s:?}");
        }
    }

    #[test]
    fn small_parsers() {
        assert!(is_hex_number("0x1a") && is_hex_number("ff") && is_hex_number("0"));
        assert!(!is_hex_number("") && !is_hex_number("0x") && !is_hex_number("0xg"));
        assert!(!is_hex_number(" 1") && !is_hex_number("0X1"));
        assert_eq!(parse_int32("1"), Some(1));
        assert_eq!(parse_int32("+1"), Some(1));
        assert_eq!(parse_int32("-1"), Some(-1));
        assert_eq!(parse_int32("+-1"), None);
        assert_eq!(parse_int32(" 1"), None);
        assert_eq!(parse_int32("2147483648"), None);
        assert_eq!(fee_rate_string(1000), "0.00001000 DASH/kB");
        assert_eq!(fee_rate_string(123 * COIN + 4), "123.00000004 DASH/kB");
        assert_eq!(
            amount_err_msg("blockmintxfee", "x"),
            "Invalid amount for -blockmintxfee=<amount>: 'x'"
        );
    }

    /// Core's `uint256S` checks (`src/test/uint256_tests.cpp:99-107`, `:300-310`) and its edges:
    /// the digits read from the last, odd counts, a `0X`, `\v`, and digits past 64.
    #[test]
    fn uint256_s_matches_core() {
        let hex =
            |blob: [u8; 32]| blob.iter().rev().map(|b| format!("{b:02x}")).collect::<String>();
        let r1 = "7d1de5eaf9b156d53208f033b5aa8122d2d2355d5e12292b121156cfdb4a529c";
        for s in [r1.to_owned(), format!("0x{r1}"), format!("   0x{r1}   "), format!("0X{r1}zz")] {
            assert_eq!(hex(uint256_s(&s)), r1, "{s:?}");
        }
        let twelve = format!("{}12", "0".repeat(62));
        for s in ["12", "12\0", "0x12", " 0x12", " 12", "\x0b\t12", "0012"] {
            assert_eq!(hex(uint256_s(s)), twelve, "{s:?}");
        }
        assert_eq!(hex(uint256_s("123")), format!("{}123", "0".repeat(61)));
        for zero in ["", "0", "0x", "x12", "- 12", "0x 12", "0 x12"] {
            assert_eq!(uint256_s(zero), [0; 32], "{zero:?}");
        }
        // 66 digits: the first two are dropped.
        assert_eq!(hex(uint256_s(&format!("ab{r1}"))), r1);
    }
}
