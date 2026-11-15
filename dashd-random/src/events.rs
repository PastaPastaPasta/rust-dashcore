//! The events hasher: Core's `CSHA256` (`crypto/sha256.cpp`) kept inside the state's chunk, so
//! its running state lives in locked memory. The midstate is eight native-endian words, then
//! the 64-byte block buffer, then the byte count.

use std::ops::Range;

use sha2::block_api::compress256;
use zeroize::Zeroize as _;

/// SHA-256's initial hash value (FIPS 180-4 §5.3.3).
const IV: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Where the hasher sits in the chunk: the midstate, the block buffer, the byte count.
const MIDSTATE: Range<usize> = 48..80;
const BUF: Range<usize> = 80..144;
const BYTES: Range<usize> = 144..152;
/// The first byte after the hasher.
pub(crate) const END: usize = BYTES.end;

fn bytes(c: &[u8]) -> u64 {
    u64::from_ne_bytes(c[BYTES].try_into().expect("8 bytes"))
}

fn set_bytes(c: &mut [u8], n: u64) {
    c[BYTES].copy_from_slice(&n.to_ne_bytes());
}

/// Core's `Transform(s, chunk, blocks)`: the midstate is compressed in a stack copy, which is
/// zeroed once written back.
fn transform(c: &mut [u8], blocks: &[[u8; 64]]) {
    let mut s = [0u32; 8];
    for (w, b) in s.iter_mut().zip(c[MIDSTATE].as_chunks::<4>().0) {
        *w = u32::from_ne_bytes(*b);
    }
    compress256(&mut s, blocks);
    for (w, b) in s.iter().zip(c[MIDSTATE].as_chunks_mut::<4>().0) {
        b.copy_from_slice(&w.to_ne_bytes());
    }
    s.zeroize();
}

/// `CSHA256::Reset`: the byte count to 0 and the midstate to the initial value. The buffer is
/// left as it is, as in Core.
// Core: crypto/sha256.cpp:745-750
pub(crate) fn reset(c: &mut [u8]) {
    set_bytes(c, 0);
    for (w, b) in IV.iter().zip(c[MIDSTATE].as_chunks_mut::<4>().0) {
        b.copy_from_slice(&w.to_ne_bytes());
    }
}

/// `CSHA256::Write`.
// Core: crypto/sha256.cpp:702-726
pub(crate) fn write(c: &mut [u8], mut data: &[u8]) {
    let mut n = bytes(c);
    let mut bufsize = (n % 64) as usize;
    if bufsize != 0 && bufsize + data.len() >= 64 {
        // Fill the buffer, and process it.
        let take = 64 - bufsize;
        c[BUF][bufsize..].copy_from_slice(&data[..take]);
        n += take as u64;
        data = &data[take..];
        let mut block: [u8; 64] = c[BUF].try_into().expect("64 bytes");
        transform(c, std::slice::from_ref(&block));
        block.zeroize();
        bufsize = 0;
    }
    let (blocks, rest) = data.as_chunks::<64>();
    if !blocks.is_empty() {
        // Process full blocks directly from the input.
        transform(c, blocks);
        n += 64 * blocks.len() as u64;
    }
    if !rest.is_empty() {
        // Fill the buffer with what remains.
        c[BUF][bufsize..bufsize + rest.len()].copy_from_slice(rest);
        n += rest.len() as u64;
    }
    set_bytes(c, n);
}

/// `CSHA256::Finalize`: the padding and the bit length, then the midstate as big-endian words
/// into `hash`.
// Core: crypto/sha256.cpp:728-743
pub(crate) fn finalize(c: &mut [u8], hash: &mut [u8; 32]) {
    const PAD: [u8; 64] = {
        let mut pad = [0; 64];
        pad[0] = 0x80;
        pad
    };
    let n = bytes(c);
    let sizedesc = (n << 3).to_be_bytes();
    write(c, &PAD[..1 + ((119 - (n % 64)) % 64) as usize]);
    write(c, &sizedesc);
    for (out, b) in hash.as_chunks_mut::<4>().0.iter_mut().zip(c[MIDSTATE].as_chunks::<4>().0) {
        *out = u32::from_ne_bytes(*b).to_be_bytes();
    }
}
