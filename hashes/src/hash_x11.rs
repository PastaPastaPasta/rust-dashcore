// Rust Dash Library
// Originally written in 2014 by
//     Dmitrii Golubev <dmitrii.golubev@dash.org>
//     For Dash
// Updated for Dash in 2022 by
//     The Dash Core Developers
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the CC0 Public Domain Dedication
// along with this software.
// If not, see <http://creativecommons.org/publicdomain/zero/1.0/>.
//

//! An implementation of a hash engine to support the X11 hash.
//!
//! The hash is computed by [`dash-x11`](https://docs.rs/dash-x11), a Rust port of Dash Core's
//! code that picks its SIMD backends at run time. The `x11-c` feature selects the C
//! `rs-x11-hash` library for 80-byte headers (it cannot hash any other length); both give
//! Core's digests ([`backend`] says which one is in use).

use core::ops::Index;
use core::slice::SliceIndex;
use core::str;
use std::io;
use std::string::String;
use std::vec::Vec;

use crate::{FromSliceError, HashEngine as _};

/// X11 of `data`, by the backend the features select.
///
/// With `x11-c`, `rs-x11-hash` computes headers only: it is handed a pointer and no length and
/// always reads 80 bytes, so for any other length it would read out of bounds or hash a prefix.
/// `dash-x11` hashes every other length, and every length without `x11-c`.
fn x11(data: &[u8]) -> [u8; 32] {
    #[cfg(all(feature = "__x11-count", debug_assertions))]
    x11_calls::record();
    #[cfg(feature = "x11-c")]
    if let Ok(header) = <&[u8; 80]>::try_from(data) {
        return rs_x11_hash::get_x11_hash(header);
    }
    dash_x11::x11(data)
}

/// A per-thread count of X11 computations, so a consumer's tests can show a header is hashed
/// once. Internal and test-only: the `__x11-count` feature, and only in debug builds, so no
/// release build emits it.
#[cfg(all(feature = "__x11-count", debug_assertions))]
#[doc(hidden)]
pub mod x11_calls {
    use std::cell::Cell;

    std::thread_local! {
        static CALLS: Cell<u64> = const { Cell::new(0) };
    }

    pub(super) fn record() {
        CALLS.with(|c| c.set(c.get() + 1));
    }

    /// X11 computations on this thread so far.
    pub fn count() -> u64 {
        CALLS.with(Cell::get)
    }
}

/// Names the backend that computes [`Hash`]es, for logs and benchmark provenance.
///
/// For `dash-x11` this is the implementation its CPU detection chose, as `dash_x11::Selection`
/// prints it, including any fallback after a failed self-test. The detection runs on the first
/// call if nothing was hashed yet. With `x11-c`, headers are the C library's and the rest is
/// `dash-x11`'s.
pub fn backend() -> String {
    let selection = dash_x11::init();
    if cfg!(feature = "x11-c") {
        format!("rs-x11-hash (80-byte headers), dash-x11 {selection} (other lengths)")
    } else {
        format!("dash-x11 {selection}")
    }
}

/// Output of the X11 hash function.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Hash([u8; 32]);

crate::hex_fmt_impl!(<Hash as crate::Hash>::DISPLAY_BACKWARD, 32, Hash);
crate::serde_impl!(Hash, 32);
crate::bincode_impl!(Hash, 32);
crate::borrow_slice_impl!(Hash);

impl str::FromStr for Hash {
    type Err = hex_conservative::DecodeFixedLengthBytesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes: [u8; 32] = hex_conservative::decode_to_array(s)?;
        bytes.reverse();
        Ok(Hash(bytes))
    }
}

impl AsRef<[u8; 32]> for Hash {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl<I: SliceIndex<[u8]>> Index<I> for Hash {
    type Output = I::Output;

    #[inline]
    fn index(&self, index: I) -> &Self::Output {
        &self.0[index]
    }
}

impl From<Hash> for [u8; 32] {
    fn from(hash: Hash) -> [u8; 32] {
        hash.0
    }
}

impl crate::Hash for Hash {
    type Engine = HashEngine;
    type Bytes = [u8; 32];

    const LEN: usize = 32;
    const DISPLAY_BACKWARD: bool = true;

    fn from_engine(e: HashEngine) -> Self {
        Hash(e.midstate())
    }

    fn from_slice(sl: &[u8]) -> Result<Self, FromSliceError> {
        // `FromSliceError` is only constructible upstream, so a hash of the
        // same size checks the length.
        crate::sha256d::Hash::from_slice(sl).map(|h| Hash(h.to_byte_array()))
    }

    fn to_byte_array(self) -> [u8; 32] {
        self.0
    }

    fn as_byte_array(&self) -> &[u8; 32] {
        &self.0
    }

    fn from_byte_array(bytes: [u8; 32]) -> Self {
        Hash(bytes)
    }

    fn all_zeros() -> Self {
        Hash([0; 32])
    }
}

/// An X11 hashing engine. X11 is not incremental, so the input is buffered.
#[derive(Clone)]
pub struct HashEngine {
    buf: Vec<u8>,
}

impl Default for HashEngine {
    /// An engine with room for a block header, X11's usual input.
    fn default() -> Self {
        HashEngine {
            buf: Vec::with_capacity(80),
        }
    }
}

impl crate::HashEngine for HashEngine {
    type MidState = [u8; 32];

    const BLOCK_SIZE: usize = 32;

    fn midstate(&self) -> [u8; 32] {
        x11(self.buf.as_slice())
    }

    fn input(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    fn n_bytes_hashed(&self) -> usize {
        self.buf.len()
    }
}

impl io::Write for HashEngine {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.input(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::string::ToString;
    use std::vec::Vec;

    use super::{backend, Hash, HashEngine};
    use crate::{Hash as _, HashEngine as _};

    /// The mainnet and testnet genesis headers and the hashes Dash Core's `chainparams.cpp`
    /// asserts (`dash-x11/tests/data/known-hashes.json`, whose PROVENANCE.md says where each
    /// comes from).
    const GENESIS: [(&str, &str); 2] = [
        (
            "010000000000000000000000000000000000000000000000000000000000000000000000c762a6567f3cc092f0684bb62b7e00a84890b990f07cc71a6bb58d64b98e02e0022ddb52f0ff0f1ec23fb901",
            "00000ffd590b1485b3caadc19b22e6379c733355108f107a430458cdf3407ab6",
        ),
        (
            "010000000000000000000000000000000000000000000000000000000000000000000000c762a6567f3cc092f0684bb62b7e00a84890b990f07cc71a6bb58d64b98e02e0dee1e352f0ff0f1ec3c927e6",
            "00000bafbc94add76cb75e2ec92894837288a481e5c005f6563d91623bf8bc2c",
        ),
    ];

    fn header(hex: &str) -> [u8; 80] {
        hex_conservative::decode_to_array(hex).unwrap()
    }

    #[test]
    fn genesis_hashes() {
        for (hex, want) in GENESIS {
            assert_eq!(Hash::hash(&header(hex)).to_string(), want);
        }
    }

    #[test]
    fn engine_buffers_its_input() {
        let raw = header(GENESIS[0].0);
        let mut engine = HashEngine::default();
        for chunk in raw.chunks(7) {
            engine.input(chunk);
        }
        assert_eq!(engine.n_bytes_hashed(), 80);
        assert_eq!(Hash::from_engine(engine), Hash::hash(&raw));
    }

    /// The backend the features select gives the portable code's digest for the lengths
    /// X11's BLAKE stage treats differently (empty, one block, two final blocks, many blocks).
    #[test]
    fn backend_matches_portable_code() {
        let input: Vec<u8> = (0..300u32).map(|i| (i * 7 + 3) as u8).collect();
        for len in [0, 1, 80, 111, 112, 128, 129, 300] {
            let data = &input[..len];
            let want = dash_x11::Implementation::standard().x11(data);
            assert_eq!(<[u8; 32]>::from(Hash::hash(data)), want, "{len} bytes");
        }
    }

    #[test]
    fn backend_is_named() {
        let name = backend();
        assert!(name.contains("dash-x11 '"), "{name}");
        assert_eq!(name.contains("rs-x11-hash"), cfg!(feature = "x11-c"), "{name}");
    }
}
