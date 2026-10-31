//! Lowercase hex and the 32-byte hash newtype.

use core::fmt;

const DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Appends `bytes` as lowercase hex, two digits per byte.
pub fn write_hex(out: &mut Vec<u8>, bytes: &[u8]) {
    for &b in bytes {
        out.push(DIGITS[usize::from(b >> 4)]);
        out.push(DIGITS[usize::from(b & 15)]);
    }
}

pub(crate) fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// Decodes the grammar's `hex` type: an even number of lowercase hex digits (the empty string is
/// valid). Uppercase digits are rejected.
pub fn decode_hex(text: &[u8]) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let (pairs, _) = text.as_chunks::<2>();
    pairs.iter().map(|p| Some(nibble(p[0])? << 4 | nibble(p[1])?)).collect()
}

/// A 256-bit hash in **display order**, as the grammar's `hash` type spells it.
///
/// Core prints a `uint256` with `GetHex()`, which writes the 32 bytes reversed. This type keeps
/// the bytes in the order of the printed text: byte 0 is the first hex pair of the string. That
/// is the reverse of Core's in-memory (and wire) byte order; [`Hash256::from_wire_bytes`] and
/// [`Hash256::to_wire_bytes`] convert. A hash the spec *defines* as `sha256d(bytes)` is that
/// digest read as a `uint256`, so its display bytes are the digest reversed.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash256([u8; 32]);

impl Hash256 {
    /// From bytes already in display order (the order of the printed hex).
    pub const fn from_display_bytes(bytes: [u8; 32]) -> Self {
        Hash256(bytes)
    }

    /// The bytes in display order (the order of the printed hex).
    pub const fn as_display_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// From Core's in-memory byte order (`uint256::begin()..end()`, the wire order): reverses.
    pub fn from_wire_bytes(mut bytes: [u8; 32]) -> Self {
        bytes.reverse();
        Hash256(bytes)
    }

    /// The bytes in Core's in-memory byte order (the wire order): the reverse of display order.
    pub fn to_wire_bytes(&self) -> [u8; 32] {
        let mut bytes = self.0;
        bytes.reverse();
        bytes
    }

    /// Parses exactly 64 lowercase hex digits.
    pub fn from_hex(text: &[u8]) -> Option<Self> {
        if text.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        let (pairs, _) = text.as_chunks::<2>();
        for (dst, pair) in bytes.iter_mut().zip(pairs) {
            *dst = nibble(pair[0])? << 4 | nibble(pair[1])?;
        }
        Some(Hash256(bytes))
    }

    /// Appends the 64 lowercase hex digits (display order).
    pub fn write_hex(&self, out: &mut Vec<u8>) {
        write_hex(out, &self.0);
    }

    /// The 64-digit lowercase hex text (display order).
    pub fn to_hex(&self) -> String {
        let mut out = Vec::with_capacity(64);
        self.write_hex(&mut out);
        // Only ASCII hex digits were written.
        String::from_utf8(out).unwrap_or_default()
    }
}

impl fmt::Display for Hash256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Hash256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash256({})", self.to_hex())
    }
}
