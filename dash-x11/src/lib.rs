//! Dash's X11 proof-of-work hash, bit-exact with Dash Core's `HashX11`.
//!
//! Global allocator: the gate proves that none comes from this crate only for the products
//! `contrib/node/layers.toml` registers (`[[product]]`, DEC-181); other consumers are out of scope.
//!
//! X11 chains eleven 512-bit hash functions (BLAKE, BMW, Grøstl, Skein, JH, Keccak, Luffa,
//! CubeHash, SHAvite-3, SIMD, ECHO) and keeps the first 32 bytes of the last digest. This crate
//! ports Dash Core's code for it (`src/hash_x11.h` and `src/crypto/x11`, identical in v23.1.8
//! and v24.0.0-rc.3) and adds faster backends:
//!
//! | Backend | Stages | Selected when |
//! |---|---|---|
//! | portable | all, ported from Core's sph code | always available |
//! | `sse` | JH, Luffa, CubeHash (SSE2), SIMD with 128-bit vectors | x86_64 with SSSE3 and SSE4.1 |
//! | `aes` | Grøstl, SHAvite-3 and ECHO with AES-NI (Core: `x86_aesni` for the last two) | and AES-NI |
//! | `avx2` | JH, Luffa, CubeHash, SIMD with 256-bit vectors | and AVX2 enabled by the OS |
//!
//! [`init`] picks the backends at run time, like Core's `SapphireAutoDetect`, checks them with
//! a self-test of Core's digests and caches the choice, falling back to smaller selections if a
//! fast path fails; the returned [`Selection`] says which implementation won and what failed.
//! A node calls it once at startup and logs it (see the README's adoption contract); [`x11`]
//! uses the cached choice. [`Implementation::detect`] is the strict variant: it panics instead
//! of falling back.
//!
//! The crate is `no_std`, does no I/O and keeps no state besides that one cached detection.
//!
//! # Example
//!
//! ```
//! // An 80-byte header; the hash is in Core's byte order (`uint256`'s data).
//! let header = [0u8; 80];
//! let hash: [u8; 32] = dash_x11::x11(&header);
//! // Every backend gives the portable code's hash.
//! assert_eq!(hash, dash_x11::Implementation::standard().x11(&header));
//! ```

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

mod dispatch;
mod portable;
mod self_test;
#[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
mod x86_64;

pub use dispatch::{Implementation, Selection, Stage, Use, init, selection};

/// The X11 hash of `data` (Core: `HashX11`), with [`init`]'s implementation.
///
/// The bytes are in Core's order, that of `uint256`'s data: a block hash as it is serialized.
pub fn x11(data: &[u8]) -> [u8; 32] {
    Implementation::auto().x11(data)
}
