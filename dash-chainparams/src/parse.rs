//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! Dash Core's string to integer conversions for chain-params options.

/// Reads `std::from_chars` digits for a signed type at the start of `s`.
///
/// Returns the value (`None` on overflow of `i64`) and the bytes read, or
/// `None` when `s` holds no digits after an optional `-`.
fn from_chars(s: &[u8]) -> Option<(Option<i64>, usize)> {
    let negative = s.first() == Some(&b'-');
    let start = usize::from(negative);
    let digits = s[start..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let value = s[start..start + digits].iter().try_fold(0_i64, |v, &c| {
        let (v, digit) = (v.checked_mul(10)?, i64::from(c - b'0'));
        if negative {
            v.checked_sub(digit)
        } else {
            v.checked_add(digit)
        }
    });
    Some((value, start + digits))
}

/// Drops one leading `+`, as `strtol` does; `None` for `+-`.
fn strip_plus(b: &[u8]) -> Option<&[u8]> {
    if b.starts_with(b"+-") {
        return None;
    }
    Some(b.strip_prefix(b"+").unwrap_or(b))
}

/// Core's `ParseInt64` (`ParseIntegral`): one optional leading `+` (not `+-`),
/// then the whole rest must be `from_chars` digits within range.
pub(crate) fn parse_i64(s: &str) -> Option<i64> {
    let b = strip_plus(s.as_bytes())?;
    match from_chars(b) {
        Some((value, read)) if read == b.len() => value,
        _ => None,
    }
}

/// Core's `ParseInt32`: [`parse_i64`] restricted to `int32_t`.
pub(crate) fn parse_i32(s: &str) -> Option<i32> {
    parse_i64(s).and_then(|v| i32::try_from(v).ok())
}

/// Core's `LocaleIndependentAtoi<int64_t>`, behind `GetIntArg`.
///
/// Trims ASCII whitespace, accepts one `+` (`+-` is 0), parses the leading
/// digits, ignores the rest, and saturates on overflow. No digits give 0.
pub(crate) fn atoi_i64(s: &str) -> i64 {
    let s = s.trim_matches(|c| matches!(c, ' ' | '\x0c' | '\n' | '\r' | '\t' | '\x0b'));
    let Some(b) = strip_plus(s.as_bytes()) else {
        return 0;
    };
    match from_chars(b) {
        Some((Some(v), _)) => v,
        Some((None, _)) if b.first() == Some(&b'-') => i64::MIN,
        Some((None, _)) => i64::MAX,
        None => 0,
    }
}

/// Core's `GetIntArg` stored into an `int`: [`atoi_i64`], then the
/// implementation-defined (modular) conversion to `int`.
pub(crate) fn int_arg(s: &str) -> i32 {
    atoi_i64(s) as i32
}

/// Core's `InterpretBool`, behind `GetBoolArg` for a string setting.
///
/// Empty is true; otherwise `LocaleIndependentAtoi<int>` is compared to 0.
/// Saturating to `int` keeps the sign, so the `int64_t` value decides alike.
pub(crate) fn interpret_bool(s: &str) -> bool {
    s.is_empty() || atoi_i64(s) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_i64_as_core() {
        let cases: [(&str, Option<i64>); 18] = [
            ("0", Some(0)),
            ("-0", Some(0)),
            ("+7", Some(7)),
            ("007", Some(7)),
            ("9223372036854775807", Some(i64::MAX)),
            ("-9223372036854775808", Some(i64::MIN)),
            ("9223372036854775808", None),
            ("", None),
            ("+", None),
            ("-", None),
            ("+-1", None),
            ("++1", None),
            ("-+1", None),
            (" 1", None),
            ("1 ", None),
            ("1x", None),
            ("0x7", None),
            ("2.5", None),
        ];
        for (s, want) in cases {
            assert_eq!(parse_i64(s), want, "{s:?}");
        }
    }

    #[test]
    fn parse_i32_range() {
        let cases: [(&str, Option<i32>); 4] = [
            ("2147483647", Some(i32::MAX)),
            ("2147483648", None),
            ("-2147483648", Some(i32::MIN)),
            ("-2147483649", None),
        ];
        for (s, want) in cases {
            assert_eq!(parse_i32(s), want, "{s:?}");
        }
    }

    #[test]
    fn atoi_as_core() {
        let cases: [(&str, i64); 8] = [
            (" 12abc", 12),
            ("\t-5\n", -5),
            ("+-5", 0),
            ("+5", 5),
            ("abc", 0),
            ("", 0),
            ("99999999999999999999", i64::MAX),
            ("-99999999999999999999", i64::MIN),
        ];
        for (s, want) in cases {
            assert_eq!(atoi_i64(s), want, "{s:?}");
        }
    }

    #[test]
    fn int_arg_wraps() {
        let cases: [(&str, i32); 4] = [
            ("4294967297", 1),
            ("99999999999", 1_215_752_191),
            ("2147483648", i32::MIN),
            ("-1", -1),
        ];
        for (s, want) in cases {
            assert_eq!(int_arg(s), want, "{s:?}");
        }
    }

    #[test]
    fn interpret_bool_as_core() {
        let cases: [(&str, bool); 6] = [
            ("", true),
            ("0", false),
            ("1", true),
            (" 2x", true),
            ("x", false),
            ("4294967296", true),
        ];
        for (s, want) in cases {
            assert_eq!(interpret_bool(s), want, "{s:?}");
        }
    }
}
