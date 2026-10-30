//! Exact conversion of Core's number text to integers (spec §9.4).

use std::fmt;

/// Why a number text was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmountError(pub String);

impl fmt::Display for AmountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AmountError {}

fn refuse<T>(what: &str, text: &str) -> Result<T, AmountError> {
    Err(AmountError(format!("{what}: {text:?}")))
}

/// Duffs from an amount as Core prints it (`ValueFromAmount`): exactly `-?[0-9]+\.[0-9]{8}`.
///
/// Integer arithmetic only. Any other shape (no fraction, fewer or more than eight fraction
/// digits, an exponent, a sign other than `-`, whitespace) is refused, and so is a value outside
/// the `i64` range.
pub fn duffs_from_amount(text: &str) -> Result<i64, AmountError> {
    let b = text.as_bytes();
    let (neg, digits) = match b.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, b),
    };
    let Some(dot) = digits.iter().position(|&c| c == b'.') else {
        return refuse("amount has no decimal point", text);
    };
    let (int_part, frac_part) = (&digits[..dot], &digits[dot + 1..]);
    if int_part.is_empty() || !int_part.iter().all(u8::is_ascii_digit) {
        return refuse("amount integer part is not digits", text);
    }
    if frac_part.len() != 8 || !frac_part.iter().all(u8::is_ascii_digit) {
        return refuse("amount needs exactly eight fraction digits", text);
    }
    let mut mag: u128 = 0;
    for &d in int_part.iter().chain(frac_part) {
        mag = mag
            .checked_mul(10)
            .and_then(|m| m.checked_add(u128::from(d - b'0')))
            .ok_or_else(|| AmountError(format!("amount out of range: {text:?}")))?;
    }
    let signed = if neg {
        0i128.checked_sub_unsigned(mag)
    } else {
        i128::try_from(mag).ok()
    };
    signed
        .and_then(|v| i64::try_from(v).ok())
        .ok_or_else(|| AmountError(format!("amount out of range: {text:?}")))
}

/// An integer from a number text that must be one: `-?[0-9]+` with no fraction or exponent, in
/// the `i64` range. (`getblockstats` and `gettxoutsetinfo` print their counts this way.)
pub fn int_from_number(text: &str) -> Result<i64, AmountError> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return refuse("not an integer", text);
    }
    text.parse::<i64>().or_else(|_| refuse("integer out of range", text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts() {
        for (text, want) in [
            ("0.00000000", 0),
            ("0.00000001", 1),
            ("-0.00000001", -1),
            ("1.00000000", 100_000_000),
            ("25000.00000000", 2_500_000_000_000),
            ("500.12345678", 50_012_345_678),
            ("-12.50000000", -1_250_000_000),
            ("21000000.00000000", 2_100_000_000_000_000),
            ("92233720368.54775807", i64::MAX),
            ("-92233720368.54775808", i64::MIN),
            // Literal reading of the shape: Core never prints these, the regex admits them.
            ("-0.00000000", 0),
            ("007.00000000", 700_000_000),
        ] {
            assert_eq!(duffs_from_amount(text), Ok(want), "{text}");
        }
    }

    #[test]
    fn refused_shapes() {
        for text in [
            "",
            "-",
            "1",
            "1.",
            ".00000001",
            "1.0",
            "1.0000000",
            "1.000000000",
            "1e8",
            "1.00000000e1",
            "+1.00000000",
            " 1.00000000",
            "1.00000000 ",
            "--1.00000000",
            "1,00000000",
            "1.0000000a",
            "0x1.00000000",
            "NaN",
            "92233720368.54775808",
            "-92233720368.54775809",
            "999999999999999999999999999999999999999999.00000000",
        ] {
            assert!(duffs_from_amount(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn integers() {
        assert_eq!(int_from_number("0"), Ok(0));
        assert_eq!(int_from_number("-5"), Ok(-5));
        assert_eq!(int_from_number("9223372036854775807"), Ok(i64::MAX));
        for text in ["", "-", "1.0", "1e3", "9223372036854775808", "+1", "0x1"] {
            assert!(int_from_number(text).is_err(), "{text:?}");
        }
    }
}
