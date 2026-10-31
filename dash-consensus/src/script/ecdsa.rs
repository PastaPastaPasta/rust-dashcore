//! The ECDSA consensus layer: Core's `CPubKey::Verify`, `CPubKey::CheckLowS`, the public-key shape
//! rule `CPubKey::GetLen`/`IsValid`, and `ecdsa_signature_parse_der_lax` (`pubkey.{h,cpp}`).
//!
//! Signatures are parsed with a safe-Rust port of the lax DER parser in Core's `pubkey.cpp`, not
//! with `secp256k1-sys`'s copy: the two differ in their length-of-length limit (Core rejects 4 or
//! more length bytes, `secp256k1-sys` 8 or more; unobservable for signatures of at most 520 bytes,
//! but the port removes the question). Public-key parsing, S normalization and verification are
//! libsecp256k1's, as in Core; §2 of the CS1 design records the library versions (CS1 §4.7).

use secp256k1::ecdsa::Signature;
use secp256k1::{Message, PublicKey};

/// A public key as Core's `CPubKey` holds it: valid exactly when the header byte's
/// `CPubKey::GetLen` equals the length (02/03 → 33 bytes; 04/06/07 → 65 bytes).
///
/// Hybrid keys (06/07) are valid here; libsecp256k1 accepts them in verification when the y parity
/// matches the header, as in Core.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct PubKeyRef<'a>(&'a [u8]);

impl<'a> PubKeyRef<'a> {
    // Core: pubkey.h:88 CPubKey::Set, pubkey.h:179 CPubKey::IsValid
    /// `CPubKey(bytes).IsValid()`.
    pub fn new(bytes: &'a [u8]) -> Option<Self> {
        let header = *bytes.first()?;
        (pubkey_len(header) == bytes.len()).then_some(Self(bytes))
    }

    pub fn as_bytes(self) -> &'a [u8] {
        self.0
    }
}

// Core: pubkey.h:59 CPubKey::GetLen
/// The serialized length a public key with this header byte has, or 0 for an invalid header.
const fn pubkey_len(header: u8) -> usize {
    match header {
        2 | 3 => 33,
        4 | 6 | 7 => 65,
        _ => 0,
    }
}

// Core: pubkey.cpp:40 ecdsa_signature_parse_der_lax
/// Parses `sig` the way Core's lax DER parser does and returns the 64-byte compact signature
/// (r ‖ s, big-endian), or `None` where Core returns 0.
///
/// Supported violations: negative integers, excessive padding, garbage at the end and overly long
/// length descriptors. When either r or s is 2²⁵⁶ or more (longer than 32 bytes once leading zeros
/// are dropped) or at least the curve order, **both** are zeroed: Core's "correctly-parsed but
/// invalid signature", which no key verifies.
pub fn parse_der_lax(input: &[u8]) -> Option<[u8; 64]> {
    let inputlen = input.len();
    let mut pos = 0usize;

    // Sequence tag byte.
    if pos == inputlen || input[pos] != 0x30 {
        return None;
    }
    pos += 1;

    // Sequence length bytes: skipped, never checked.
    if pos == inputlen {
        return None;
    }
    let mut lenbyte = usize::from(input[pos]);
    pos += 1;
    if lenbyte & 0x80 != 0 {
        lenbyte -= 0x80;
        if lenbyte > inputlen - pos {
            return None;
        }
        pos += lenbyte;
    }

    // Integer tag byte and length for R.
    if pos == inputlen || input[pos] != 0x02 {
        return None;
    }
    pos += 1;
    let rlen = integer_len(input, &mut pos)?;
    let rpos = pos;
    pos += rlen;

    // Integer tag byte and length for S.
    if pos == inputlen || input[pos] != 0x02 {
        return None;
    }
    pos += 1;
    let slen = integer_len(input, &mut pos)?;
    let spos = pos;

    let mut tmpsig = [0u8; 64];
    let mut overflow = !copy_integer(&input[rpos..rpos + rlen], &mut tmpsig[..32]);
    overflow |= !copy_integer(&input[spos..spos + slen], &mut tmpsig[32..]);
    // secp256k1_ecdsa_signature_parse_compact fails when r or s is at least the curve order.
    if overflow || Signature::from_compact(&tmpsig).is_err() {
        // Overwrite the result with a correctly-parsed but invalid signature.
        tmpsig = [0u8; 64];
    }
    Some(tmpsig)
}

/// One DER integer length (short form, or long form with at most 3 significant length bytes after
/// leading zeros). Leaves `pos` at the integer's first byte and checks that the integer fits.
fn integer_len(input: &[u8], pos: &mut usize) -> Option<usize> {
    let inputlen = input.len();
    if *pos == inputlen {
        return None;
    }
    let mut lenbyte = usize::from(input[*pos]);
    *pos += 1;
    let len = if lenbyte & 0x80 != 0 {
        lenbyte -= 0x80;
        if lenbyte > inputlen - *pos {
            return None;
        }
        while lenbyte > 0 && input[*pos] == 0 {
            *pos += 1;
            lenbyte -= 1;
        }
        if lenbyte >= 4 {
            return None;
        }
        let mut len = 0usize;
        while lenbyte > 0 {
            len = (len << 8) + usize::from(input[*pos]);
            *pos += 1;
            lenbyte -= 1;
        }
        len
    } else {
        lenbyte
    };
    if len > inputlen - *pos {
        return None;
    }
    Some(len)
}

/// Copies a big-endian integer, without its leading zeros, right-aligned into the 32-byte `out`.
/// Returns false (an overflow) when more than 32 bytes remain.
fn copy_integer(mut value: &[u8], out: &mut [u8]) -> bool {
    while let [0, rest @ ..] = value {
        value = rest;
    }
    if value.len() > 32 {
        return false;
    }
    out[32 - value.len()..].copy_from_slice(value);
    true
}

/// `ecdsa_signature_parse_der_lax` as Core calls it: into a signature object. Every compact form
/// [`parse_der_lax`] returns parses, the all-zero one included.
fn parse_der_lax_signature(sig: &[u8]) -> Option<Signature> {
    Signature::from_compact(&parse_der_lax(sig)?).ok()
}

// Core: pubkey.cpp:182 CPubKey::Verify
/// `CPubKey(pubkey).Verify(hash, sig)`: parse the key (hybrid keys with matching parity are
/// accepted), parse the signature laxly, normalize S to the lower half, verify. `msg` is the
/// 32-byte hash in `uint256` byte order. Any parse failure is `false`.
pub fn verify_lax(pubkey: PubKeyRef<'_>, msg: &[u8; 32], sig: &[u8]) -> bool {
    let Ok(key) = PublicKey::from_slice(pubkey.as_bytes()) else {
        return false;
    };
    let Some(mut signature) = parse_der_lax_signature(sig) else {
        return false;
    };
    // libsecp256k1's verification requires low S, which Bitcoin never enforced: normalize first.
    signature.normalize_s();
    secp256k1::ecdsa::verify(&signature, Message::from_digest(*msg), &key).is_ok()
}

// Core: pubkey.cpp:348 CPubKey::CheckLowS (v23.1.8: 320)
/// The signature parses laxly and normalizing S would not change it. A signature whose r or s
/// overflows parses as all zeros and therefore counts as low-S (quirk 7).
pub fn check_low_s(sig: &[u8]) -> bool {
    let Some(mut signature) = parse_der_lax_signature(sig) else {
        return false;
    };
    // `secp256k1_ecdsa_signature_normalize` returns whether it changed S; the crate does not
    // expose that, so compare the encodings.
    let before = signature.serialize_compact();
    signature.normalize_s();
    signature.serialize_compact() == before
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use secp256k1::SecretKey;

    use super::*;

    /// The secp256k1 group order n, big-endian.
    const ORDER: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36,
        0x41, 0x41,
    ];

    fn der(r: &[u8], s: &[u8]) -> Vec<u8> {
        let mut v = vec![0x30, (4 + r.len() + s.len()) as u8, 0x02, r.len() as u8];
        v.extend_from_slice(r);
        v.extend([0x02, s.len() as u8]);
        v.extend_from_slice(s);
        v
    }

    fn signed(msg: &[u8; 32]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let key = SecretKey::from_secret_bytes([0x11; 32]).unwrap();
        let pk = PublicKey::from_secret_key(&key);
        let sig = secp256k1::ecdsa::sign(Message::from_digest(*msg), &key);
        (
            pk.serialize().to_vec(),
            pk.serialize_uncompressed().to_vec(),
            sig.serialize_der().to_vec(),
        )
    }

    proptest! {
        /// CS1 §7.2: `check_low_s(normalize(sig))` holds, and `check_low_s` accepts exactly the
        /// signatures normalising leaves unchanged. Random (R, S) pairs are high S half the time.
        #[test]
        fn check_low_s_accepts_exactly_the_normalized(r in any::<[u8; 32]>(), s in any::<[u8; 32]>()) {
            let compact = [r, s].concat();
            let Ok(sig) = Signature::from_compact(&compact) else {
                return Ok(()); // a scalar at or above n
            };
            let mut normalized = sig;
            normalized.normalize_s();
            prop_assert!(check_low_s(&normalized.serialize_der()));
            prop_assert_eq!(check_low_s(&sig.serialize_der()), normalized == sig);
        }
    }

    #[test]
    fn pubkey_shape_follows_get_len() {
        assert!(PubKeyRef::new(&[0x02; 33]).is_some());
        assert!(PubKeyRef::new(&[0x03; 33]).is_some());
        for header in [4u8, 6, 7] {
            let k = vec![header; 65];
            assert!(PubKeyRef::new(&k).is_some());
            assert!(PubKeyRef::new(&k[..33]).is_none());
        }
        assert!(PubKeyRef::new(&[0x02; 65]).is_none());
        assert!(PubKeyRef::new(&[0x05; 33]).is_none());
        assert!(PubKeyRef::new(&[]).is_none());
    }

    #[test]
    fn strict_der_round_trips_through_the_lax_parser() {
        let (compressed, uncompressed, sig) = signed(&[7; 32]);
        let compact = parse_der_lax(&sig).unwrap();
        assert_eq!(compact, Signature::from_der(&sig).unwrap().serialize_compact());
        for key in [&compressed, &uncompressed] {
            assert!(verify_lax(PubKeyRef::new(key).unwrap(), &[7; 32], &sig));
            assert!(!verify_lax(PubKeyRef::new(key).unwrap(), &[8; 32], &sig));
        }
        assert!(check_low_s(&sig));
    }

    #[test]
    fn high_s_verifies_after_normalization_and_is_not_low() {
        let (key, _, sig) = signed(&[9; 32]);
        let low = Signature::from_der(&sig).unwrap().serialize_compact();
        // s' = n - s with a borrow over the big-endian bytes.
        let mut high_s = [0u8; 32];
        let mut borrow = 0i16;
        for i in (0..32).rev() {
            let d = i16::from(ORDER[i]) - i16::from(low[32 + i]) - borrow;
            high_s[i] = d.rem_euclid(256) as u8;
            borrow = i16::from(d < 0);
        }
        let mut r = low[..32].to_vec();
        if r[0] & 0x80 != 0 {
            r.insert(0, 0);
        }
        let mut s = high_s.to_vec();
        if s[0] & 0x80 != 0 {
            s.insert(0, 0);
        }
        let high = der(&r, &s);
        assert!(!check_low_s(&high));
        assert!(verify_lax(PubKeyRef::new(&key).unwrap(), &[9; 32], &high));
    }

    #[test]
    fn overflowing_scalars_zero_both() {
        let one = [1u8];
        // r = n: overflows, so both are zeroed and nothing verifies; it still parses, and counts as
        // low-S.
        let sig = der(&[&[0u8][..], &ORDER].concat(), &one);
        assert_eq!(parse_der_lax(&sig), Some([0; 64]));
        assert!(check_low_s(&sig));
        // 33 significant bytes overflow as well.
        let sig = der(&[0x01; 33], &one);
        assert_eq!(parse_der_lax(&sig), Some([0; 64]));
    }

    #[test]
    fn lax_violations_parse() {
        // Negative R (top bit set without padding), excess padding and trailing garbage.
        let sig = [der(&[0x80, 0x01], &[0x00, 0x00, 0x05]), vec![0xde, 0xad]].concat();
        let compact = parse_der_lax(&sig).unwrap();
        assert_eq!(&compact[30..32], &[0x80, 0x01]);
        assert_eq!(compact[63], 0x05);
        // A long-form R length with leading zero length bytes.
        let sig = [0x30, 0x00, 0x02, 0x84, 0x00, 0x00, 0x00, 0x01, 0x07, 0x02, 0x01, 0x09];
        let compact = parse_der_lax(&sig).unwrap();
        assert_eq!((compact[31], compact[63]), (7, 9));
        // Four significant length bytes are rejected.
        let sig = [0x30, 0x00, 0x02, 0x84, 0x01, 0x00, 0x00, 0x00];
        assert_eq!(parse_der_lax(&sig), None);
        // Truncations.
        assert_eq!(parse_der_lax(&[]), None);
        assert_eq!(parse_der_lax(&[0x30]), None);
        assert_eq!(parse_der_lax(&[0x30, 0x00, 0x02, 0x05, 0x01]), None);
        assert_eq!(parse_der_lax(&[0x31, 0x00]), None);
    }
}
