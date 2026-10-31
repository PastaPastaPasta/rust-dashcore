//! `diff_bls_decode` (BS11a-1): `dash-pkc`'s eager and lazy BLS decode against Core's
//! `CBLSWrapper::Unserialize` and `CBLSLazyWrapper::Unserialize` then `Get()`, through the
//! oracle's `oracle_bls_decode` (A1 §1.4.1): the arbiter of the legacy decode (BS11a D1).
//!
//! Input layout: byte 0 selects the object and how its bytes are built ([`BlsInput::read`]):
//! - bits 0-1: the kind (0 public key, 1 and 3 signature, 2 secret key);
//! - bit 2: lazy (public keys and signatures; Core has no lazy secret key);
//! - bit 3: the legacy flag;
//! - bits 4-7: the class ([`PointClass`], [`SkClass`]), which builds the object's bytes from the
//!   rest. Class 0 takes them as they are.
//!
//! The classes put the fuzzer on the outcome table's rows, which random bytes rarely reach: a
//! point in the subgroup, on the curve, off it, at a special x, and flag-only encodings; a
//! secret key next to the group order or at either end of its range. The fuzzer's bits pick
//! the three flag bits of every point encoding. A G2 x is laid out as the flag reads it (`c0 ||
//! c1` under the legacy flag, as relic reads it; `c1 || c0` under the basic one), or as the other
//! scheme does when bit 2 of the rest's first byte is set: bytes canonical only in the other
//! scheme are the invalid object.
//!
//! Compared: the verdict (Core throws, or both decode), the validity and the re-serialization
//! under the same flag (zeros when invalid; for a lazy object, after `Get()`).

use std::ops::Range;

use blst::BLST_ERROR;
use dash_oracle_sys::{BlsKind, BlsMode, Oracle, Pin};
use dash_pkc::bls::{
    BLS_PK_LEN, BLS_SIG_LEN, BLS_SK_LEN, BlsDecodeError, BlsEager, BlsLazy, BlsSchemeKind,
    BlsSchemeSource, BlsWire, BlsWirePoint, Fr, G1, G2,
};

use crate::{Compared, Outcome, check, core_verdict, mismatched_verdicts};

/// One base-field element's length.
const FP: usize = BLS_PK_LEN;
/// The base field's modulus p, big-endian.
const P: [u8; FP] = unhex(
    "1a0111ea397fe69a4b1ba7b6434bacd764774b84f38512bf6730d2a0f6b0f6241eabfffeb153ffffb9feffffffffaaab",
);
/// The group order r, big-endian.
pub const R: [u8; BLS_SK_LEN] =
    unhex("73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000001");

/// The bits of a point encoding's first byte that are flags in either scheme.
const FLAGS: u8 = 0xe0;
/// The basic scheme's compression flag.
pub const COMPRESSED: u8 = 0x80;
/// How many x a curve search tries before it keeps the last.
const SEARCH: usize = 64;

/// One `oracle_bls_decode` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlsInput {
    pub kind: BlsKind,
    pub mode: BlsMode,
    pub legacy: bool,
    /// At most the object's length: Core's longer reads end in trailing bytes, which the
    /// target does not compare.
    pub bytes: Vec<u8>,
}

/// How a point's bytes are built (byte 0 bits 4-7, modulo the number of classes). Byte 0 of
/// the rest gives the three flag bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointClass {
    /// The rest, as it is.
    Raw,
    /// A subgroup point: the generator times a key derived from the rest.
    Subgroup,
    /// The first x from the rest (below p) on the curve, as blst judges it (G1's x = 0 is on
    /// the curve, outside the group); for G2 optionally in the base field or imaginary.
    OnCurve,
    /// As [`Self::OnCurve`], off the curve.
    OffCurve,
    /// Each field element special: 0, small, p − 1, p, p + 1, p plus a small value, or the
    /// largest 381-bit value.
    SpecialX,
    /// All zero but the first byte, which the rest gives whole, and one stray byte.
    FlagsOnly,
}

/// How a secret key's bytes are built (byte 0 bits 4-7, modulo the number of classes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkClass {
    /// The rest, as it is.
    Raw,
    /// r plus a signed byte.
    NearOrder,
    /// A byte from either end of the range: the value, or 2^256 − 1 minus it.
    Ends,
    /// The rest, made smaller than r.
    BelowOrder,
}

impl PointClass {
    const ALL: [Self; 6] =
        [Self::Raw, Self::Subgroup, Self::OnCurve, Self::OffCurve, Self::SpecialX, Self::FlagsOnly];

    pub(crate) fn from_selector(class: usize) -> Self {
        Self::ALL[class % Self::ALL.len()]
    }
}

impl SkClass {
    const ALL: [Self; 4] = [Self::Raw, Self::NearOrder, Self::Ends, Self::BelowOrder];

    pub(crate) fn from_selector(class: usize) -> Self {
        Self::ALL[class % Self::ALL.len()]
    }
}

impl BlsInput {
    /// The call `selector` and `rest` describe (module documentation).
    pub fn read(selector: u8, rest: &[u8]) -> Self {
        let kind = match selector & 3 {
            0 => BlsKind::PublicKey,
            2 => BlsKind::SecretKey,
            _ => BlsKind::Signature,
        };
        let mode = if selector & 4 != 0 && kind != BlsKind::SecretKey {
            BlsMode::Lazy
        } else {
            BlsMode::Eager
        };
        let legacy = selector & 8 != 0;
        let class = usize::from(selector >> 4);
        let bytes = match kind {
            BlsKind::PublicKey => {
                point_bytes(&G1_CURVE, PointClass::from_selector(class), rest, legacy)
            }
            BlsKind::Signature => {
                point_bytes(&G2_CURVE, PointClass::from_selector(class), rest, legacy)
            }
            BlsKind::SecretKey => sk_bytes(SkClass::from_selector(class), rest),
        };
        Self {
            kind,
            mode,
            legacy,
            bytes,
        }
    }
}

/// `diff_bls_decode`.
pub fn compare_bls_decode(bytes: &[u8], pin: Pin) -> Compared {
    let Some((&selector, rest)) = bytes.split_first() else {
        return Ok(Outcome::OutOfDomain);
    };
    compare_input(&BlsInput::read(selector, rest), pin)
}

/// One call compared: the verdict, the validity and the re-serialization.
pub fn compare_input(input: &BlsInput, pin: Pin) -> Compared {
    let core = core_verdict(Oracle::new(pin).bls_decode(
        input.kind,
        input.mode,
        input.legacy,
        &input.bytes,
    ));
    let (core, (valid, reserialized)) = match (core, decode(input)) {
        (Ok(core), Ok(ours)) => (core, ours),
        (core, ours) => return mismatched_verdicts(core, ours),
    };
    check(valid == core.valid, "valid")?;
    check(reserialized == core.reserialized, "reserialization")?;
    Ok(Outcome::BothAccept)
}

/// Our decode: the validity and the re-serialization, or an error where Core throws (the
/// stream's end included).
pub fn decode(input: &BlsInput) -> Result<(bool, Vec<u8>), BlsDecodeError> {
    let scheme = BlsSchemeKind::from_legacy(input.legacy);
    let bytes = &input.bytes;
    match (input.kind, input.mode) {
        (BlsKind::PublicKey, BlsMode::Eager) => eager::<G1, BLS_PK_LEN>(bytes, scheme),
        (BlsKind::Signature, BlsMode::Eager) => eager::<G2, BLS_SIG_LEN>(bytes, scheme),
        (BlsKind::SecretKey, _) => eager::<Fr, BLS_SK_LEN>(bytes, scheme),
        (BlsKind::PublicKey, BlsMode::Lazy) => lazy::<G1, BLS_PK_LEN>(bytes, scheme),
        (BlsKind::Signature, BlsMode::Lazy) => lazy::<G2, BLS_SIG_LEN>(bytes, scheme),
    }
}

fn eager<K: BlsWire<N>, const N: usize>(
    bytes: &[u8],
    scheme: BlsSchemeKind,
) -> Result<(bool, Vec<u8>), BlsDecodeError> {
    let bytes = bytes.try_into().map_err(|_| BlsDecodeError)?;
    let object = BlsEager::<K, N>::decode(bytes, BlsSchemeSource::Version(scheme))?;
    Ok((object.is_valid(), object.to_bytes(scheme).to_vec()))
}

/// `Unserialize`, `Get()`, then `Serialize` under the same flag, as the oracle does.
fn lazy<K: BlsWirePoint<N>, const N: usize>(
    bytes: &[u8],
    scheme: BlsSchemeKind,
) -> Result<(bool, Vec<u8>), BlsDecodeError> {
    let bytes = bytes.try_into().map_err(|_| BlsDecodeError)?;
    let object = BlsLazy::<K, N>::from_bytes(bytes, BlsSchemeSource::Version(scheme));
    let valid = object.get().is_valid();
    Ok((valid, object.to_bytes_as(scheme).to_vec()))
}

/// A group's basic encoding, through blst: blst's uncompress tests that x is a field element
/// and on the curve, not the subgroup.
pub(crate) struct Curve {
    pub(crate) len: usize,
    /// The compressed generator multiple for a key derived from 32 bytes.
    subgroup_point: fn(&[u8; 32]) -> Vec<u8>,
    /// blst's verdict on a compressed encoding.
    uncompress: fn(&[u8]) -> BLST_ERROR,
}

fn verdict<T>(result: Result<T, BLST_ERROR>) -> BLST_ERROR {
    result.err().unwrap_or(BLST_ERROR::BLST_SUCCESS)
}

// `key_gen` fails only for fewer than 32 bytes of key material.
pub(crate) const G1_CURVE: Curve = Curve {
    len: BLS_PK_LEN,
    subgroup_point: |ikm| {
        blst::min_pk::SecretKey::key_gen(ikm, &[])
            .map_or_else(|_| Vec::new(), |sk| sk.sk_to_pk().compress().to_vec())
    },
    uncompress: |bytes| verdict(blst::min_pk::PublicKey::uncompress(bytes)),
};

pub(crate) const G2_CURVE: Curve = Curve {
    len: BLS_SIG_LEN,
    subgroup_point: |ikm| {
        blst::min_sig::SecretKey::key_gen(ikm, &[])
            .map_or_else(|_| Vec::new(), |sk| sk.sk_to_pk().compress().to_vec())
    },
    uncompress: |bytes| verdict(blst::min_sig::PublicKey::uncompress(bytes)),
};

/// blst's verdict on a compressed basic encoding of `kind`'s group (G1 for public keys, else
/// G2).
pub fn uncompress_verdict(kind: BlsKind, bytes: &[u8]) -> BLST_ERROR {
    let curve = if kind == BlsKind::PublicKey {
        &G1_CURVE
    } else {
        &G2_CURVE
    };
    (curve.uncompress)(bytes)
}

/// Whether blst's verdict puts x on the curve; `None` when x is not a field element.
pub fn on_curve(verdict: BLST_ERROR) -> Option<bool> {
    match verdict {
        BLST_ERROR::BLST_SUCCESS | BLST_ERROR::BLST_POINT_NOT_IN_GROUP => Some(true),
        BLST_ERROR::BLST_POINT_NOT_ON_CURVE => Some(false),
        _ => None,
    }
}

/// `N` bytes of `rest` from `at`, zero-padded.
pub(crate) fn take<const N: usize>(rest: &[u8], at: usize) -> [u8; N] {
    let mut out = [0u8; N];
    let src = rest.get(at..).unwrap_or_default();
    let n = src.len().min(N);
    out[..n].copy_from_slice(&src[..n]);
    out
}

/// At most `n` bytes from the front of `rest`.
fn prefix(rest: &[u8], n: usize) -> Vec<u8> {
    rest[..rest.len().min(n)].to_vec()
}

pub(crate) fn byte(rest: &[u8], at: usize) -> u8 {
    rest.get(at).copied().unwrap_or(0)
}

/// `x + delta`, big-endian, wrapping.
pub(crate) fn add(x: &mut [u8], delta: i16) {
    let mut carry = i32::from(delta);
    for b in x.iter_mut().rev() {
        let sum = i32::from(*b) + carry;
        *b = sum.rem_euclid(256) as u8;
        carry = sum.div_euclid(256);
        if carry == 0 {
            break;
        }
    }
}

/// A special base-field element, picked by `pick`: 0, a small value, p − 1, p, p + 1, p plus a
/// small value, or the largest 381-bit value.
fn special_fp(pick: u8) -> [u8; FP] {
    let small = i16::from(pick >> 3);
    let near = |base: [u8; FP], delta: i16| {
        let mut x = base;
        add(&mut x, delta);
        x
    };
    match pick & 7 {
        0 => [0; FP],
        1 | 2 => near([0; FP], small),
        3 => near(P, -1),
        4 => P,
        5 => near(P, 1),
        6 => near(P, small),
        _ => {
            let mut x = [0xff; FP];
            x[0] = 0x1f;
            x
        }
    }
}

/// The encoding of x with the three flag bits of `flags`.
pub(crate) fn with_flags(mut x: Vec<u8>, flags: u8) -> Vec<u8> {
    if let Some(first) = x.first_mut() {
        *first = (*first & !FLAGS) | (flags & FLAGS);
    }
    x
}

/// x from the rest, each field element below p (its top byte at most 0x0f), and the element a
/// search steps. For G2, bit 0 of `flags` zeroes c1 (x in the base field) and bit 1 c0; the
/// search steps a nonzero element.
pub(crate) fn below_p(curve: &Curve, rest: &[u8], flags: u8) -> (Vec<u8>, Range<usize>) {
    let mut x = rest.get(1..).unwrap_or_default().to_vec();
    x.resize(curve.len, 0);
    for element in x.chunks_mut(FP) {
        element[0] &= 0x0f;
    }
    let mut step = curve.len - FP..curve.len;
    if curve.len == 2 * FP {
        if flags & 1 != 0 {
            x[..FP].fill(0);
        } else if flags & 2 != 0 {
            x[FP..].fill(0);
            step = 0..FP;
        }
    }
    (x, step)
}

/// The first x from `x` (incrementing the field element at `step`) that blst puts on the curve
/// (`on_curve`) or off it; after [`SEARCH`] tries, the last one.
pub(crate) fn search(curve: &Curve, mut x: Vec<u8>, step: Range<usize>, want: bool) -> Vec<u8> {
    for _ in 0..SEARCH {
        if on_curve((curve.uncompress)(&with_flags(x.clone(), COMPRESSED))) == Some(want) {
            break;
        }
        add(&mut x[step.clone()], 1);
    }
    x
}

/// x, given in the basic layout, laid out as the flag reads it with the three flag bits of
/// `flags`: the legacy layout of a G2 x is `c0 || c1`, and bit 2 of `flags` picks the other
/// scheme's layout.
pub(crate) fn laid_out(mut x: Vec<u8>, flags: u8, legacy: bool) -> Vec<u8> {
    if legacy != (flags & 4 != 0) && x.len() == 2 * FP {
        // The flags leave c1's top byte before it moves.
        x[0] &= !FLAGS;
        x.rotate_left(FP);
    }
    with_flags(x, flags)
}

pub(crate) fn point_bytes(curve: &Curve, class: PointClass, rest: &[u8], legacy: bool) -> Vec<u8> {
    let flags = byte(rest, 0);
    let laid_out = |x: Vec<u8>| laid_out(x, flags, legacy);
    match class {
        PointClass::Raw => prefix(rest, curve.len),
        PointClass::Subgroup => laid_out((curve.subgroup_point)(&take(rest, 1))),
        PointClass::OnCurve | PointClass::OffCurve => {
            let (x, step) = below_p(curve, rest, flags);
            laid_out(search(curve, x, step, class == PointClass::OnCurve))
        }
        PointClass::SpecialX => with_flags(
            (0..curve.len / FP).flat_map(|i| special_fp(byte(rest, 1 + i))).collect(),
            flags,
        ),
        PointClass::FlagsOnly => {
            let mut x = vec![0u8; curve.len];
            x[0] = flags;
            let at = usize::from(byte(rest, 1)) % curve.len;
            if at != 0 {
                x[at] = byte(rest, 2);
            }
            x
        }
    }
}

pub(crate) fn sk_bytes(class: SkClass, rest: &[u8]) -> Vec<u8> {
    match class {
        SkClass::Raw => prefix(rest, BLS_SK_LEN),
        SkClass::NearOrder => {
            let mut x = R;
            add(&mut x, i16::from(byte(rest, 0) as i8));
            x.to_vec()
        }
        SkClass::Ends => {
            let low = byte(rest, 1);
            let mut x = [0u8; BLS_SK_LEN];
            if byte(rest, 0) & 1 == 0 {
                x[BLS_SK_LEN - 1] = low;
            } else {
                x = [0xff; BLS_SK_LEN];
                x[BLS_SK_LEN - 1] = !low;
            }
            x.to_vec()
        }
        SkClass::BelowOrder => {
            let mut x: [u8; BLS_SK_LEN] = take(rest, 0);
            x[0] %= R[0];
            x.to_vec()
        }
    }
}

const fn hex_digit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("hex digit"),
    }
}

pub(crate) const fn unhex<const N: usize>(s: &str) -> [u8; N] {
    let s = s.as_bytes();
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        out[i] = hex_digit(s[2 * i]) << 4 | hex_digit(s[2 * i + 1]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_values() {
        assert_eq!(special_fp(4), P);
        let mut p_minus_1 = P;
        add(&mut p_minus_1, -1);
        assert_eq!(special_fp(3), p_minus_1);
        assert_eq!(special_fp(5)[47], P[47].wrapping_add(1));
        let mut r = [0u8; 32];
        add(&mut r, -1);
        assert_eq!(r, [0xff; 32]);
    }

    #[test]
    fn near_order_reaches_r() {
        assert_eq!(sk_bytes(SkClass::NearOrder, &[0]), R.to_vec());
        let mut below = R;
        add(&mut below, -1);
        assert_eq!(sk_bytes(SkClass::NearOrder, &[0xff]), below.to_vec());
    }
}
