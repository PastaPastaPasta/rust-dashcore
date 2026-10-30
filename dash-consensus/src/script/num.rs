//! Core's `CScriptNum` (`script/script.h:211`, `script/script.cpp:311-384`).
//!
//! Numeric opcodes take operands of at most 4 bytes (5 for CLTV and CSV), so every value is in
//! −2³⁹+1..=2³⁹−1 and every result of one operation fits an `i64` with room to spare. Core asserts
//! against overflow in `+=`, `-=` and unary `-`, and Core's asserts are always on (it cannot be
//! built without them). From the interpreter those asserts are unreachable, so here they are
//! `debug_assert!`s; a caller doing its own arithmetic on large values must check for overflow.
//! Results may exceed 4 bytes: they are valid until a later numeric opcode reads them, which then
//! fails.
//!
//! [`ScriptNumError`] is a unit error without `Display` or `Error`: inside the interpreter it only
//! ever becomes [`ScriptError::UnknownError`](super::ScriptError::UnknownError), as Core's
//! `catch (...)` makes it, and callers that decode numbers themselves map it the same way.

use core::cmp::Ordering;
use core::ops::{Add, BitAnd, Div, Neg, Rem, Sub};

/// Core's `CScriptNum::nDefaultMaxNumSize`.
pub const DEFAULT_MAX_NUM_SIZE: usize = 4;

/// Core's `scriptnum_error`: the operand is longer than allowed or, with `MINIMALDATA`, not
/// minimally encoded. The interpreter maps it to `SCRIPT_ERR_UNKNOWN_ERROR`, as Core's
/// `catch (...)` does.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct ScriptNumError;

/// Core's `CScriptNum`: an `i64` read from and written as a little-endian sign-magnitude number.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct ScriptNum(i64);

impl ScriptNum {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(1);

    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    // Core: script/script.h:230 CScriptNum(const std::vector<unsigned char>&, bool, size_t)
    /// Reads an operand: longer than `max_size`, or not minimally encoded when `require_minimal`
    /// is set, is an error.
    ///
    /// # Panics
    ///
    /// If `max_size` is over 8, which `i64` cannot hold. Core passes 4 and 5.
    pub fn decode(
        vch: &[u8],
        require_minimal: bool,
        max_size: usize,
    ) -> Result<Self, ScriptNumError> {
        assert!(max_size <= 8, "script numbers are read from at most 8 bytes");
        if vch.len() > max_size {
            return Err(ScriptNumError);
        }
        if require_minimal && !Self::is_minimally_encoded(vch, max_size) {
            return Err(ScriptNumError);
        }
        Ok(Self(Self::set_vch(vch)))
    }

    // Core: script/script.h:364 CScriptNum::set_vch
    fn set_vch(vch: &[u8]) -> i64 {
        let Some(&last) = vch.last() else {
            return 0;
        };
        let mut result: i64 = 0;
        for (i, b) in vch.iter().enumerate() {
            result |= i64::from(*b) << (8 * i);
        }
        // If the most significant byte has the sign bit set, clear it and return a negative.
        if last & 0x80 != 0 {
            -(result & !(0x80_i64 << (8 * (vch.len() - 1))))
        } else {
            result
        }
    }

    // Core: script/script.h:330 CScriptNum::serialize
    /// `getvch()`: the minimal little-endian sign-magnitude encoding; zero is the empty vector.
    pub fn encode(self) -> Vec<u8> {
        let value = self.0;
        if value == 0 {
            return Vec::new();
        }
        let neg = value < 0;
        let mut abs = value.unsigned_abs();
        let mut result = Vec::with_capacity(9);
        while abs != 0 {
            result.push((abs & 0xff) as u8);
            abs >>= 8;
        }
        // A set top bit needs one more byte for the sign; otherwise the sign goes into it.
        let top = result.len() - 1;
        if result[top] & 0x80 != 0 {
            result.push(if neg {
                0x80
            } else {
                0
            });
        } else if neg {
            result[top] |= 0x80;
        }
        result
    }

    /// `GetInt64()`.
    pub const fn value(self) -> i64 {
        self.0
    }

    // Core: script/script.h:314 CScriptNum::getint
    /// `getint()`: the value clamped to `i32`.
    pub const fn getint(self) -> i32 {
        if self.0 > i32::MAX as i64 {
            i32::MAX
        } else if self.0 < i32::MIN as i64 {
            i32::MIN
        } else {
            self.0 as i32
        }
    }

    // Core: script/script.cpp:311 CScriptNum::IsMinimallyEncoded
    /// `vch` is at most `max_size` bytes and has no unneeded top byte (negative zero `80` is not
    /// minimal).
    pub fn is_minimally_encoded(vch: &[u8], max_size: usize) -> bool {
        if vch.len() > max_size {
            return false;
        }
        if let Some(&last) = vch.last() {
            // The top byte, without its sign bit, is zero: not minimal unless the next byte's top
            // bit would otherwise read as the sign (+-255 encode as ff00 and ff80).
            if last & 0x7f == 0 && (vch.len() <= 1 || vch[vch.len() - 2] & 0x80 == 0) {
                return false;
            }
        }
        true
    }

    // Core: script/script.cpp:339 CScriptNum::MinimallyEncode
    /// Rewrites `data` in place into the minimal encoding of the same number; returns whether it
    /// changed anything. Any length is accepted (`OP_NUM2BIN`'s and `OP_BIN2NUM`'s operands have
    /// no width limit). An all-zero or negative-zero input becomes the empty vector.
    pub fn minimally_encode(data: &mut Vec<u8>) -> bool {
        let Some(&last) = data.last() else {
            return false;
        };
        // The last byte is not 0x00 or 0x80: already minimal.
        if last & 0x7f != 0 {
            return false;
        }
        // One byte: a zero, which encodes as the empty vector.
        if data.len() == 1 {
            data.clear();
            return true;
        }
        // The next byte has its sign bit set: the top byte is needed.
        if data[data.len() - 2] & 0x80 != 0 {
            return false;
        }
        // Not minimal: find the highest non-zero byte and put the sign there or just above it.
        let mut i = data.len() - 1;
        while i > 0 {
            if data[i - 1] != 0 {
                if data[i - 1] & 0x80 != 0 {
                    data[i] = last;
                    i += 1;
                } else {
                    data[i - 1] |= last;
                }
                data.truncate(i);
                return true;
            }
            i -= 1;
        }
        // All zeros: a zero.
        data.clear();
        true
    }
}

impl PartialEq<i64> for ScriptNum {
    fn eq(&self, rhs: &i64) -> bool {
        self.0 == *rhs
    }
}

impl PartialOrd<i64> for ScriptNum {
    fn partial_cmp(&self, rhs: &i64) -> Option<Ordering> {
        self.0.partial_cmp(rhs)
    }
}

impl Add for ScriptNum {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        debug_assert!(self.0.checked_add(rhs.0).is_some());
        Self(self.0 + rhs.0)
    }
}

impl Sub for ScriptNum {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        debug_assert!(self.0.checked_sub(rhs.0).is_some());
        Self(self.0 - rhs.0)
    }
}

impl Neg for ScriptNum {
    type Output = Self;

    fn neg(self) -> Self {
        debug_assert!(self.0 != i64::MIN);
        Self(-self.0)
    }
}

/// Truncating division, as C++'s `/` on `int64_t`. The interpreter checks for a zero divisor first.
impl Div for ScriptNum {
    type Output = Self;

    fn div(self, rhs: Self) -> Self {
        Self(self.0 / rhs.0)
    }
}

/// The remainder with the dividend's sign, as C++'s `%` on `int64_t`.
impl Rem for ScriptNum {
    type Output = Self;

    fn rem(self, rhs: Self) -> Self {
        Self(self.0 % rhs.0)
    }
}

impl BitAnd<i64> for ScriptNum {
    type Output = Self;

    fn bitand(self, rhs: i64) -> Self {
        Self(self.0 & rhs)
    }
}

impl From<bool> for ScriptNum {
    /// `bn = (a < b)`: Core assigns a `bool` to a `CScriptNum` as 0 or 1.
    fn from(b: bool) -> Self {
        Self(i64::from(b))
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// An independent model of the sign-magnitude encoding, over i128 so nothing overflows.
    fn model_decode(vch: &[u8]) -> i128 {
        let Some(&last) = vch.last() else {
            return 0;
        };
        let mut magnitude: i128 = 0;
        for (i, b) in vch.iter().enumerate() {
            let b = if i == vch.len() - 1 {
                b & 0x7f
            } else {
                *b
            };
            magnitude += i128::from(b) << (8 * i);
        }
        if last & 0x80 != 0 {
            -magnitude
        } else {
            magnitude
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20_000))]

        #[test]
        fn encode_round_trips_and_is_minimal(v in -(1i64 << 39)..(1i64 << 39)) {
            let bytes = ScriptNum::new(v).encode();
            prop_assert!(ScriptNum::is_minimally_encoded(&bytes, 8));
            prop_assert_eq!(model_decode(&bytes), i128::from(v));
            prop_assert_eq!(ScriptNum::decode(&bytes, true, 8), Ok(ScriptNum::new(v)));
        }

        #[test]
        fn decode_matches_the_model(vch in proptest::collection::vec(any::<u8>(), 0..=5)) {
            let n = ScriptNum::decode(&vch, false, 5).unwrap();
            prop_assert_eq!(i128::from(n.value()), model_decode(&vch));
            prop_assert_eq!(ScriptNum::decode(&vch, false, 4).is_err(), vch.len() > 4);
            prop_assert_eq!(
                ScriptNum::decode(&vch, true, 5).is_ok(),
                ScriptNum::is_minimally_encoded(&vch, 5)
            );
        }

        /// MinimallyEncode keeps the value, yields the minimal encoding, and reports a change
        /// exactly when the input was not minimal (any width).
        #[test]
        fn minimally_encode_normalizes(vch in proptest::collection::vec(prop_oneof![Just(0u8), Just(0x80u8), any::<u8>()], 0..40)) {
            let mut data = vch.clone();
            let changed = ScriptNum::minimally_encode(&mut data);
            prop_assert_eq!(changed, data != vch);
            prop_assert_eq!(changed, !ScriptNum::is_minimally_encoded(&vch, usize::MAX));
            prop_assert!(ScriptNum::is_minimally_encoded(&data, usize::MAX));
            // Same number, compared on the model while it fits.
            if vch.len() <= 15 {
                prop_assert_eq!(model_decode(&data), model_decode(&vch));
            }
        }

        #[test]
        fn getint_clamps(v in any::<i64>()) {
            let expected = v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
            prop_assert_eq!(ScriptNum::new(v).getint(), expected);
        }
    }

    #[test]
    fn negative_zero_and_padding_are_not_minimal() {
        assert!(!ScriptNum::is_minimally_encoded(&[0x80], 4));
        assert!(!ScriptNum::is_minimally_encoded(&[0x00], 4));
        assert!(!ScriptNum::is_minimally_encoded(&[0x01, 0x00], 4));
        assert!(ScriptNum::is_minimally_encoded(&[0xff, 0x00], 4));
        assert!(ScriptNum::is_minimally_encoded(&[0xff, 0x80], 4));
        assert_eq!(ScriptNum::decode(&[0x80], false, 4), Ok(ScriptNum::ZERO));
        assert_eq!(ScriptNum::decode(&[0x80], true, 4), Err(ScriptNumError));
    }

    #[test]
    fn division_truncates_toward_zero() {
        let n = |v| ScriptNum::new(v);
        assert_eq!(n(-7) / n(2), n(-3));
        assert_eq!(n(-7) % n(2), n(-1));
        assert_eq!(n(7) % n(-2), n(1));
    }
}
