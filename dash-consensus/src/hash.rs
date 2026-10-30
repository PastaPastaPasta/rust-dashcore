//! The crate's hash functions, and the one place that picks their backends.
//!
//! Everything in `dash-consensus` that hashes goes through this module, so a backend change
//! replaces these bodies without touching a caller. SHA-256 is `dash-sha256` (SH1, DEC-28: Dash
//! Core's transforms and runtime dispatch); X11 is base-sdk's `dash-pow`, or `dash-x11` (XH1,
//! DEC-51b) with the `dash-x11` feature (see the manifest); SHA-1 and
//! RIPEMD-160, which only the script opcodes use, are `bitcoin_hashes` (Core's are plain C++
//! too). Hashes are returned in Core's internal order, the order `uint256`/`uint160` store.

/// SHA-256 of `data` (Core `CSHA256`).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    dash_sha256::sha256(data)
}

/// SHA256d of `data` (Core `Hash`).
pub fn sha256d(data: &[u8]) -> [u8; 32] {
    dash_sha256::sha256d(data)
}

/// SHA-1 of `data` (Core `CSHA1`).
pub fn sha1(data: &[u8]) -> [u8; 20] {
    bitcoin_hashes::sha1::Hash::hash(data).to_byte_array()
}

/// RIPEMD-160 of `data` (Core `CRIPEMD160`).
pub fn ripemd160(data: &[u8]) -> [u8; 20] {
    bitcoin_hashes::ripemd160::Hash::hash(data).to_byte_array()
}

/// RIPEMD-160 of the SHA-256 of `data` (Core `CHash160`).
pub fn hash160(data: &[u8]) -> [u8; 20] {
    ripemd160(&sha256(data))
}

/// An incremental SHA256d over data written in pieces, without buffering it (Core's
/// `CHashWriter`). It is also a [`crate::primitives::CoreWrite`] sink.
///
/// `Clone` is part of the contract (CS1's sighash prefix cache forks a midstate): a clone copies
/// the SHA-256 state, the buffered partial block and the length. A replacement backend keeps it.
#[derive(Clone, Default)]
pub struct HashWriter(dash_sha256::Sha256d);

impl HashWriter {
    pub fn new() -> Self {
        Self(dash_sha256::Sha256d::new())
    }

    pub fn input(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    /// SHA256d of everything written, in Core's internal byte order.
    pub fn finish(self) -> [u8; 32] {
        self.0.finalize()
    }
}

/// Core `SHA256D64`: the SHA256d of each 64-byte input, written to the matching output, through
/// Core's SHA-NI, AVX2 eight-way or SSE4.1 four-way transforms as the CPU allows.
///
/// # Panics
///
/// If `outputs` and `inputs` differ in length.
pub fn sha256d_64_many(outputs: &mut [[u8; 32]], inputs: &[[u8; 64]]) {
    dash_sha256::sha256d64(outputs, inputs);
}

/// One merkle level in place: the first half of `hashes` becomes the SHA256d of each pair, and
/// the count is returned (Core's `SHA256D64(hashes[0].begin(), hashes[0].begin(), size / 2)`).
///
/// # Panics
///
/// If `hashes` has an odd length.
pub fn sha256d_64_in_place(hashes: &mut [[u8; 32]]) -> usize {
    dash_sha256::sha256d64_in_place(hashes)
}

/// X11 of `data` (Core `CBlockHeader::GetHash`, `HashX11`). With the `dash-x11` feature, by
/// `dash-x11`'s cached CPU selection, the backend dashcore's `BlockHash` uses (BS15): dash-spv's
/// client start calls `dash_x11::init()` (DEC-61), otherwise the first call detects the CPU.
/// Without it, by base-sdk's `dash-pow`.
pub fn x11(data: &[u8]) -> [u8; 32] {
    #[cfg(any(test, all(feature = "__x11-count", debug_assertions)))]
    x11_calls::record();
    #[cfg(feature = "dash-x11")]
    return dash_x11::x11(data);
    #[cfg(not(feature = "dash-x11"))]
    <dash_pow::X11 as dash_pow::__deps::digest::Digest>::digest(data).into()
}

/// A per-thread count of [`x11`] calls, so tests can show a hash is computed once (CS0 §6.3,
/// DEC-112 O2). Internal and test-only: the `__x11-count` feature exposes it to another crate's
/// tests (`dash-spv`'s bridge), and only in debug builds, so no release build, all features on,
/// emits it.
#[cfg(any(test, all(feature = "__x11-count", debug_assertions)))]
#[doc(hidden)]
pub mod x11_calls {
    use std::cell::Cell;

    thread_local! {
        static CALLS: Cell<u64> = const { Cell::new(0) };
    }

    pub(crate) fn record() {
        CALLS.with(|c| c.set(c.get() + 1));
    }

    /// [`super::x11`] calls on this thread so far.
    pub fn count() -> u64 {
        CALLS.with(Cell::get)
    }
}

#[cfg(test)]
mod tests {
    use dash_core_vectors::hex::hex_str as hex;

    use super::*;

    // Expected digests are the standard test vectors for each function's input "abc" (FIPS 180-4
    // for SHA-1 and SHA-256, the RIPEMD-160 authors' set), so a wrong backend wiring shows here.
    #[test]
    fn functions_match_their_reference_vectors() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(&ripemd160(b"abc")), "8eb208f7e05d987a9b044a8e98c6b087f15a0bfc");
        // HASH160("abc"): RIPEMD-160 of the SHA-256 above, cross-checked with bitcoin_hashes.
        assert_eq!(hash160(b"abc"), bitcoin_hashes::hash160::Hash::hash(b"abc").to_byte_array());
        assert_eq!(sha256d(b"abc"), sha256(&sha256(b"abc")));
        assert_eq!(sha256d(b"abc"), bitcoin_hashes::sha256d::Hash::hash(b"abc").to_byte_array());
    }

    /// X11 of Dash's mainnet genesis header (Core `chainparams.cpp`), whichever backend the
    /// `dash-x11` feature picks: the gate's workspace build turns it on, a build of this crate
    /// alone leaves it off.
    #[test]
    fn x11_hashes_the_mainnet_genesis_header() {
        use dash_core_vectors::hex::{parse_hex, uint256_from_hex, uint256_to_hex};

        let mut header = Vec::with_capacity(80);
        header.extend_from_slice(&1u32.to_le_bytes());
        header.extend_from_slice(&[0; 32]);
        header.extend_from_slice(&uint256_from_hex(
            "e0028eb9648db56b1ac77cf090b99048a8007e2bb64b68f092c03c7f56a662c7",
        ));
        header.extend_from_slice(&1_390_095_618u32.to_le_bytes());
        header.extend_from_slice(&parse_hex("f0ff0f1e"));
        header.extend_from_slice(&28_917_698u32.to_le_bytes());
        assert_eq!(
            uint256_to_hex(&x11(&header)),
            "00000ffd590b1485b3caadc19b22e6379c733355108f107a430458cdf3407ab6"
        );
    }

    #[test]
    fn engine_matches_one_shot() {
        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let mut engine = HashWriter::new();
        for chunk in data.chunks(7) {
            engine.input(chunk);
        }
        assert_eq!(engine.finish(), sha256d(&data));
    }

    #[test]
    fn a_cloned_engine_continues_from_the_midstate() {
        let data: Vec<u8> = (0..=255u8).cycle().take(300).collect();
        // 100 bytes: one full block hashed and 36 bytes buffered.
        let mut prefix = HashWriter::new();
        prefix.input(&data[..100]);
        let mut a = prefix.clone();
        let mut b = prefix;
        a.input(&data[100..]);
        b.input(&data[100..200]);
        assert_eq!(a.finish(), sha256d(&data));
        assert_eq!(b.finish(), sha256d(&data[..200]));
    }

    #[test]
    fn sha256d_64_many_matches_one_shot() {
        let inputs: Vec<[u8; 64]> = (0..19u8).map(|i| [i; 64]).collect();
        let mut outputs = vec![[0u8; 32]; inputs.len()];
        sha256d_64_many(&mut outputs, &inputs);
        for (input, output) in inputs.iter().zip(&outputs) {
            assert_eq!(*output, sha256d(input));
        }
    }
}
