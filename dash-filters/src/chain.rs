// SPDX-License-Identifier: MIT

//! The filter-header chain (BIP157, Core's `BlockFilter::GetHash` and `ComputeHeader`).
//!
//! A filter's *hash* is the SHA256d of its encoded bytes. Its *header* chains it to the one
//! before: `header = SHA256d(hash || previous header)`. A node serves the headers (`cfheaders`
//! carries the hashes and the header before them), and an SPV client holds only headers: it
//! recomputes the chain from the hashes, compares the result with the headers it trusts, and
//! only then checks each downloaded filter against its header and matches it (SB0 §4.8).
//!
//! The 32 bytes are the hash in Core's memory order, the order the wire carries.

use core::fmt;

use dash_num::Hash256;
use dash_types::Numeric as _;

/// A filter hash: SHA256d of the encoded filter.
pub type FilterHash = Hash256;
/// A filter header: SHA256d of a filter hash and the previous filter header.
pub type FilterHeader = Hash256;

/// Core's `BlockFilter::GetHash`: the hash of an encoded filter.
pub fn filter_hash(encoded: &[u8]) -> FilterHash {
    FilterHash::from_lendian(dash_sha256::sha256d(encoded))
}

/// Core's `BlockFilter::ComputeHeader`: the header of a filter with this hash that follows
/// `prev_header`. The first filter follows the all-zero header.
pub fn filter_header(hash: &FilterHash, prev_header: &FilterHeader) -> FilterHeader {
    let mut data = [0u8; 64];
    data[..32].copy_from_slice(hash.as_bytes());
    data[32..].copy_from_slice(prev_header.as_bytes());
    FilterHeader::from_lendian(dash_sha256::sha256d(&data))
}

/// The headers that follow `prev_header` for these filter hashes, in order.
pub fn extend_headers(prev_header: &FilterHeader, hashes: &[FilterHash]) -> Vec<FilterHeader> {
    let mut prev = *prev_header;
    hashes
        .iter()
        .map(|hash| {
            prev = filter_header(hash, &prev);
            prev
        })
        .collect()
}

/// A link of the chain that does not hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChainError {
    /// The header the hashes give differs from the expected one.
    Mismatch {
        /// The position in the batch of the first link that fails (0 for a single filter).
        index: usize,
        /// The header the hashes give.
        computed: FilterHeader,
        /// The header that was expected.
        expected: FilterHeader,
    },
    /// There are not as many expected headers as hashes.
    Length {
        /// The number of filter hashes.
        hashes: usize,
        /// The number of expected headers.
        headers: usize,
    },
}

impl fmt::Display for ChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChainError::Mismatch {
                index,
                computed,
                expected,
            } => write!(f, "filter header {index} is {computed}, expected {expected}"),
            ChainError::Length {
                hashes,
                headers,
            } => write!(f, "{hashes} filter hashes but {headers} filter headers"),
        }
    }
}

impl std::error::Error for ChainError {}

/// Checks that `hashes` extend `prev_header` to exactly `expected`, one header per hash: every
/// link, not only the last, so a bad hash is reported at its own position.
pub fn check_links(
    prev_header: &FilterHeader,
    hashes: &[FilterHash],
    expected: &[FilterHeader],
) -> Result<(), ChainError> {
    if hashes.len() != expected.len() {
        return Err(ChainError::Length {
            hashes: hashes.len(),
            headers: expected.len(),
        });
    }
    let mut prev = *prev_header;
    for (index, (hash, want)) in hashes.iter().zip(expected).enumerate() {
        prev = check_link(index, hash, &prev, want)?;
    }
    Ok(())
}

/// One link of the chain: the header `hash` gives after `prev_header`, which must be `expected`.
fn check_link(
    index: usize,
    hash: &FilterHash,
    prev_header: &FilterHeader,
    expected: &FilterHeader,
) -> Result<FilterHeader, ChainError> {
    let computed = filter_header(hash, prev_header);
    if computed == *expected {
        Ok(computed)
    } else {
        Err(ChainError::Mismatch {
            index,
            computed,
            expected: *expected,
        })
    }
}

/// Checks a downloaded filter against the header the chain holds for it: the header it computes
/// from `prev_header` must equal `expected`. A filter that passes is the one the chain commits
/// to and can be matched.
pub fn check_filter(
    encoded: &[u8],
    prev_header: &FilterHeader,
    expected: &FilterHeader,
) -> Result<(), ChainError> {
    check_link(0, &filter_hash(encoded), prev_header, expected).map(drop)
}
