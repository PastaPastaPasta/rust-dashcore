//! The x86_64 backends.

pub(crate) mod avx2;
pub(crate) mod shani;
pub(crate) mod sse4;
pub(crate) mod sse41;

/// The little-endian word at `offset` of `bytes` (Core: `ReadLE32`).
#[inline(always)]
pub(crate) fn le32(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}
