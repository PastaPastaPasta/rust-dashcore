// Copyright (c) 2014-2022 The Bitcoin Core developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or http://www.opensource.org/licenses/mit-license.php.

//! Dash Core's SHA-256: the same transforms, the same runtime dispatch.
//!
//! Global allocator: the gate proves that none comes from this crate only for the products
//! `contrib/node/layers.toml` registers (`[[product]]`, DEC-181); other consumers are out of scope.
//!
//! This crate ports `src/crypto/sha256*.cpp` from Dash Core (v24.0.0-rc.3; v23.1.8 has the
//! same code):
//!
//! | Backend | Core | Used for |
//! |---|---|---|
//! | portable | `sha256::Transform`, `sha256::TransformD64` | fallback everywhere |
//! | `sse4` | `sha256_sse4::Transform` (Intel's assembly) | x86_64 single stream |
//! | `sse41` | `sha256d64_sse41::Transform_4way` | x86_64 4-way double SHA-256 of 64 bytes |
//! | `avx2` | `sha256d64_avx2::Transform_8way` | x86_64 8-way double SHA-256 of 64 bytes |
//! | `x86_shani` | `sha256_x86_shani::Transform`, `sha256d64_x86_shani::Transform_2way` | x86_64 with SHA-NI |
//! | `arm_shani` | `sha256_arm_shani::Transform`, `sha256d64_arm_shani::Transform_2way` | aarch64 with SHA2 |
//!
//! [`Implementation::detect`] picks the backends exactly as Core's `SHA256AutoDetect` does,
//! including its rule that SHA-NI replaces the SSE4.1 and AVX2 multi-way code, and runs Core's
//! self-test on the result. The free functions and [`Sha256::new`] use
//! [`Implementation::auto`], the detection with every backend allowed, computed once.
//!
//! The crate does no I/O and keeps no state besides that one cached detection.
//!
//! # Example
//!
//! ```
//! use dash_sha256::{Sha256d, sha256, sha256d, sha256d64, sha256d64_in_place};
//!
//! let digest: [u8; 32] = sha256(b"abc");            // CSHA256
//! let txid: [u8; 32] = sha256d(b"raw tx");          // Hash / CHash256
//! let mut writer = Sha256d::new();                  // streaming SHA256d, no serialized copy
//! writer.update(b"part 1");
//! writer.update(b"part 2");
//! let _ = writer.finalize();
//!
//! // SHA256D64 of 64-byte inputs: input[i] is two child hashes.
//! let pairs = [[0u8; 64]; 4];
//! let mut parents = [[0u8; 32]; 4];
//! sha256d64(&mut parents, &pairs);
//! assert_eq!(parents[0], sha256d(&pairs[0]));
//!
//! // One merkle level in place, as Core's ComputeMerkleRoot does. The level must have an even
//! // length: as in Core, the caller first runs the mutation check, then duplicates an odd level's
//! // last hash.
//! let mut level = vec![[0u8; 32]; 8];
//! let n = sha256d64_in_place(&mut level);
//! level.truncate(n);
//! ```

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

// The SIMD backends need 64-bit pointers on x86_64 (the SSE4 assembly takes the block count in a
// full register, which the x32 ABI does not define) and little-endian lanes on aarch64 (the
// byte-swapping loads assume them). Other targets, like Core's builds without these, get the
// portable code.
#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
mod aarch64;
mod dispatch;
mod macros;
mod portable;
#[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
mod x86_64;

pub use dispatch::{Implementation, Use};

/// The SHA-256 initial state.
const INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Writes the state words big-endian (Core: the `WriteBE32` calls of `Finalize`).
fn write_state(out: &mut [u8; 32], s: [u32; 8]) {
    for (out, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(s) {
        *out = s.to_be_bytes();
    }
}

/// Streaming SHA-256 (Core: `CSHA256`).
///
/// `Clone` copies the midstate, so a hasher fed a common prefix can be cloned for each suffix.
#[derive(Clone)]
pub struct Sha256 {
    s: [u32; 8],
    buf: [u8; 64],
    bytes: u64,
    imp: &'static Implementation,
}

impl Sha256 {
    /// A hasher using [`Implementation::auto`].
    pub fn new() -> Self {
        Self::with_implementation(Implementation::auto())
    }

    /// A hasher using the given implementation.
    pub fn with_implementation(imp: &'static Implementation) -> Self {
        Sha256 {
            s: INIT,
            buf: [0; 64],
            bytes: 0,
            imp,
        }
    }

    /// Hashes `data` (Core: `CSHA256::Write`).
    pub fn update(&mut self, mut data: &[u8]) {
        let mut bufsize = (self.bytes % 64) as usize;
        if bufsize != 0 && bufsize + data.len() >= 64 {
            // Fill the buffer, and process it.
            let (head, rest) = data.split_at(64 - bufsize);
            self.buf[bufsize..].copy_from_slice(head);
            self.bytes += head.len() as u64;
            data = rest;
            self.imp.transform(&mut self.s, core::slice::from_ref(&self.buf));
            bufsize = 0;
        }
        let (blocks, rest) = data.as_chunks::<64>();
        if !blocks.is_empty() {
            self.imp.transform(&mut self.s, blocks);
            self.bytes += 64 * blocks.len() as u64;
        }
        if !rest.is_empty() {
            // Fill the buffer with what remains.
            self.buf[bufsize..bufsize + rest.len()].copy_from_slice(rest);
            self.bytes += rest.len() as u64;
        }
    }

    /// Returns the digest (Core: `CSHA256::Finalize`).
    pub fn finalize(mut self) -> [u8; 32] {
        const PAD: [u8; 64] = {
            let mut pad = [0; 64];
            pad[0] = 0x80;
            pad
        };
        let sizedesc = (self.bytes << 3).to_be_bytes();
        self.update(&PAD[..1 + ((119 - (self.bytes % 64)) % 64) as usize]);
        self.update(&sizedesc);
        let mut hash = [0; 32];
        write_state(&mut hash, self.s);
        hash
    }

    /// Restarts the hasher, keeping its implementation (Core: `CSHA256::Reset`).
    pub fn reset(&mut self) {
        self.bytes = 0;
        self.s = INIT;
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Sha256").field("bytes", &self.bytes).field("imp", &self.imp).finish()
    }
}

/// Streaming double SHA-256 (Core: `CHash256`).
#[derive(Clone, Debug, Default)]
pub struct Sha256d(Sha256);

impl Sha256d {
    /// A hasher using [`Implementation::auto`].
    pub fn new() -> Self {
        Sha256d(Sha256::new())
    }

    /// A hasher using the given implementation.
    pub fn with_implementation(imp: &'static Implementation) -> Self {
        Sha256d(Sha256::with_implementation(imp))
    }

    /// Hashes `data`.
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data)
    }

    /// Returns SHA-256 of the SHA-256 of everything hashed.
    pub fn finalize(self) -> [u8; 32] {
        let imp = self.0.imp;
        let first = self.0.finalize();
        let mut second = Sha256::with_implementation(imp);
        second.update(&first);
        second.finalize()
    }
}

/// SHA-256 of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

/// Double SHA-256 of `data` (Core: `Hash`).
pub fn sha256d(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256d::new();
    h.update(data);
    h.finalize()
}

/// Double SHA-256 of each 64-byte input (Core: `SHA256D64`), using [`Implementation::auto`].
///
/// This is one merkle tree level: `input[i]` is the concatenation of two child hashes. To hash
/// a level held as `&mut [[u8; 32]]` in place, as Core does, use [`sha256d64_in_place`].
///
/// # Panics
///
/// If `out` and `input` differ in length.
pub fn sha256d64(out: &mut [[u8; 32]], input: &[[u8; 64]]) {
    Implementation::auto().sha256d64(out, input)
}

/// One merkle tree level in place (Core: `ComputeMerkleRoot`'s `SHA256D64` call), using
/// [`Implementation::auto`]. The caller runs the mutation check before duplicating the last
/// hash of an odd level, as Core does; see [`Implementation::sha256d64_in_place`].
///
/// # Panics
///
/// If `hashes` has an odd length.
pub fn sha256d64_in_place(hashes: &mut [[u8; 32]]) -> usize {
    Implementation::auto().sha256d64_in_place(hashes)
}
