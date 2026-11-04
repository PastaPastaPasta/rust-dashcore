// Written in 2019 by Tammas Blummer.
// SPDX-License-Identifier: CC0-1.0

// The codec below was largely copied from https://github.com/rust-bitcoin/murmel/blob/master/src/blockfilter.rs
// on 11. June 2019 which is licensed under Apache, that file specifically
// was written entirely by Tamas Blummer, who is re-licensing its contents here as CC0.
// SB9 moved it here from `dashcore::bip158` and reworked it over byte slices to follow Core's
// `GCSFilter` (src/blockfilter.cpp) and `util/golombrice.h`.

//! The Golomb-coded set codec of BIP158, as Core's `GCSFilter` implements it.
//!
//! A filter is `CompactSize(N)` followed by `N` Golomb-Rice coded deltas of the sorted element
//! hashes. Each element is hashed with SipHash-2-4 under two keys (for a block filter, the first
//! 16 bytes of the block hash) and mapped into `[0, N * M)` with Core's `FastRange64`.
//!
//! Two read paths exist, and they differ on purpose:
//! * [`check_encoded`] is Core's `GCSFilter(params, encoded, skip_decode_check = false)`: exactly
//!   `N` values and no data after them, or an error.
//! * [`match_any`] and [`match_all`] read lazily, as dashcore's matcher always has (SPV matches
//!   a filter only after its hash checked against the filter header, [`crate::chain`]): a
//!   malformed count means "no elements", and a stream that ends inside the values is an error
//!   only when the match needs them.
//!
//! The matchers read through a [`ByteSource`], one byte at a time and only when the next bits
//! need it: [`match_any_from`] and [`match_all_from`] take any source, so a caller whose filter
//! arrives as a stream (a socket, a file) never has a byte read past the value that decides the
//! match. [`match_any`] and [`match_all`] are the same code over a byte slice.

use core::cmp::Ordering;
use core::fmt;

use crate::siphash::{siphash, siphash_batch};

/// Core's `BASIC_FILTER_P`: the Golomb-Rice parameter of the basic filter.
pub const BASIC_FILTER_P: u8 = 19;
/// Core's `BASIC_FILTER_M`: the inverse false-positive rate of the basic filter.
pub const BASIC_FILTER_M: u64 = 784_931;

/// Core's `MAX_SIZE`, the range check of `ReadCompactSize`.
const MAX_SIZE: u64 = 0x0200_0000;

/// The largest Golomb-Rice parameter accepted. Core only ever uses [`BASIC_FILTER_P`]; the bound
/// is what keeps `n >> p` and `q << p` inside a `u64`, and so the domain of the generic filters
/// dashcore's `GcsFilterReader` and `GcsFilterWriter` always accepted.
const MAX_P: u8 = 63;

/// Byte length of a standard P2PKH scriptPubKey
const P2PKH_SCRIPT_LEN: usize = 25;
/// Byte length of a standard P2SH scriptPubKey
const P2SH_SCRIPT_LEN: usize = 23;

/// Errors of the GCS codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FilterError {
    /// The Golomb-Rice parameter is above 63.
    InvalidParams,
    /// `N * M`, the size of the range the hashes map into, does not fit 64 bits.
    RangeOverflow,
    /// A filter of 2^32 elements or more, Core's "N must be <2^32".
    TooManyElements,
    /// The element count is not a canonical `CompactSize`, or above Core's `MAX_SIZE`.
    BadCount,
    /// The data ends before the filter does.
    Truncated,
    /// Core's "encoded_filter contains excess data".
    ExcessData,
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FilterError::InvalidParams => "Golomb-Rice parameter above 63",
            FilterError::RangeOverflow => "N * M does not fit 64 bits",
            FilterError::TooManyElements => "N must be <2^32",
            FilterError::BadCount => "bad element count",
            FilterError::Truncated => "filter ends early",
            FilterError::ExcessData => "encoded_filter contains excess data",
        })
    }
}

impl std::error::Error for FilterError {}

/// The parameters of one GCS filter, Core's `GCSFilter::Params`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GcsParams {
    k0: u64,
    k1: u64,
    p: u8,
    m: u64,
}

impl GcsParams {
    /// Parameters with the SipHash keys `(k0, k1)`, Golomb-Rice parameter `p` and false-positive
    /// rate `1 / m`.
    ///
    /// # Errors
    ///
    /// [`FilterError::InvalidParams`] when `p` is above 63.
    pub fn new(k0: u64, k1: u64, p: u8, m: u64) -> Result<GcsParams, FilterError> {
        if p > MAX_P {
            return Err(FilterError::InvalidParams);
        }
        Ok(GcsParams {
            k0,
            k1,
            p,
            m,
        })
    }

    /// The basic filter's parameters for the block with this hash, Core's `BlockFilter::BuildParams`:
    /// the keys are the first two little-endian `u64` of the hash in memory order.
    pub fn basic(block_hash: &[u8; 32]) -> GcsParams {
        let word = |at: usize| {
            let mut word = [0u8; 8];
            word.copy_from_slice(&block_hash[at..at + 8]);
            u64::from_le_bytes(word)
        };
        GcsParams {
            k0: word(0),
            k1: word(8),
            p: BASIC_FILTER_P,
            m: BASIC_FILTER_M,
        }
    }
}

/// A set of elements (scriptPubKeys and the like) to hash into a filter or to match against one,
/// bucketed by length so each bucket hashes through the fixed-length SIMD
/// [`siphash_batch`](crate::siphash::siphash_batch). The two standard Dash script sizes (P2PKH =
/// 25, P2SH = 23) get their own buckets; the rest go to `other`. Build once and reuse across
/// filters to avoid re-bucketing per match.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterQuery {
    p2pkh: Vec<[u8; P2PKH_SCRIPT_LEN]>,
    p2sh: Vec<[u8; P2SH_SCRIPT_LEN]>,
    other: Vec<Vec<u8>>,
}

impl FilterQuery {
    /// Creates an empty query.
    pub fn new() -> FilterQuery {
        FilterQuery::default()
    }

    /// Adds one element (e.g. a scriptPubKey), routing it to its length bucket.
    pub fn push(&mut self, element: &[u8]) {
        // The length guards make `copy_from_slice` into the fixed-size array infallible.
        match element.len() {
            P2PKH_SCRIPT_LEN => {
                let mut buf = [0u8; P2PKH_SCRIPT_LEN];
                buf.copy_from_slice(element);
                self.p2pkh.push(buf);
            }
            P2SH_SCRIPT_LEN => {
                let mut buf = [0u8; P2SH_SCRIPT_LEN];
                buf.copy_from_slice(element);
                self.p2sh.push(buf);
            }
            _ => self.other.push(element.to_vec()),
        }
    }

    /// Total number of elements across all buckets.
    pub fn len(&self) -> usize {
        self.p2pkh.len() + self.p2sh.len() + self.other.len()
    }

    /// Whether the query has no elements.
    pub fn is_empty(&self) -> bool {
        self.p2pkh.is_empty() && self.p2sh.is_empty() && self.other.is_empty()
    }

    /// Deduplicates each length bucket (sorting as a side effect). Used by the encoder, since
    /// BIP158 hashes each distinct element once; the matcher tolerates duplicates.
    fn dedup(&mut self) {
        self.p2pkh.sort_unstable();
        self.p2pkh.dedup();
        self.p2sh.sort_unstable();
        self.p2sh.dedup();
        self.other.sort_unstable();
        self.other.dedup();
    }

    /// Hashes each length bucket with `params`' keys (fixed-length buckets via the SIMD
    /// [`siphash_batch`], `other` via the scalar one-shot) and maps the results into `[0, f)`,
    /// returned unsorted. Core's `HashToRange` for every element.
    fn hash_to_range(&self, params: &GcsParams, f: u64) -> Vec<u64> {
        let mut mapped = vec![0u64; self.len()];
        let mut off = 0;

        if !self.p2pkh.is_empty() {
            let end = off + self.p2pkh.len();
            siphash_batch::<P2PKH_SCRIPT_LEN>(
                params.k0,
                params.k1,
                &self.p2pkh,
                &mut mapped[off..end],
            );
            off = end;
        }
        if !self.p2sh.is_empty() {
            let end = off + self.p2sh.len();
            siphash_batch::<P2SH_SCRIPT_LEN>(
                params.k0,
                params.k1,
                &self.p2sh,
                &mut mapped[off..end],
            );
            off = end;
        }
        for e in &self.other {
            mapped[off] = siphash(params.k0, params.k1, e);
            off += 1;
        }

        for m in &mut mapped {
            *m = fast_range64(*m, f);
        }
        mapped
    }
}

impl<S: AsRef<[u8]>> FromIterator<S> for FilterQuery {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> FilterQuery {
        let mut query = FilterQuery::new();
        for element in iter {
            query.push(element.as_ref());
        }
        query
    }
}

/// Core's `FastRange64`: fast reduction of a hash to `[0, n)`.
fn fast_range64(hash: u64, n: u64) -> u64 {
    ((u128::from(hash) * u128::from(n)) >> 64) as u64
}

/// Core's `WriteCompactSize`.
fn write_compact_size(out: &mut Vec<u8>, n: u64) {
    if n < 253 {
        out.push(n as u8);
    } else if n <= 0xffff {
        out.push(253);
        out.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n <= 0xffff_ffff {
        out.push(254);
        out.extend_from_slice(&(n as u32).to_le_bytes());
    } else {
        out.push(255);
        out.extend_from_slice(&n.to_le_bytes());
    }
}

/// Where a filter's bytes come from: a slice, or a stream the caller wraps. Bytes are taken one
/// at a time and only when the bits being read need them, as Core's `BitStreamReader` takes them
/// from its stream, so a matcher that returns early leaves the rest of the source unread.
pub trait ByteSource {
    /// What a failed read gives. The codec's own errors ([`FilterError`]) convert into it.
    type Error: From<FilterError>;

    /// The next byte.
    ///
    /// # Errors
    ///
    /// The source's error when there is no byte to give.
    fn next_byte(&mut self) -> Result<u8, Self::Error>;
}

impl<S: ByteSource> ByteSource for &mut S {
    type Error = S::Error;

    #[inline]
    fn next_byte(&mut self) -> Result<u8, S::Error> {
        (**self).next_byte()
    }
}

/// The bytes of a slice: the source of [`match_any`], [`match_all`] and [`check_encoded`].
/// Reading past its end is [`FilterError::Truncated`].
#[derive(Clone, Debug)]
pub struct SliceSource<'a> {
    data: &'a [u8],
    /// The next byte to give: an index, not a re-sliced `data`, so a byte costs one increment.
    next: usize,
}

impl<'a> SliceSource<'a> {
    /// A source of `data`, read from its start.
    pub fn new(data: &'a [u8]) -> SliceSource<'a> {
        SliceSource {
            data,
            next: 0,
        }
    }

    /// The bytes not read yet.
    pub fn remaining(&self) -> &'a [u8] {
        &self.data[self.next..]
    }
}

impl ByteSource for SliceSource<'_> {
    type Error = FilterError;

    #[inline]
    fn next_byte(&mut self) -> Result<u8, FilterError> {
        let byte = *self.data.get(self.next).ok_or(FilterError::Truncated)?;
        self.next += 1;
        Ok(byte)
    }
}

/// `N` more bytes of `source`, in order.
fn take<S: ByteSource, const N: usize>(source: &mut S) -> Result<[u8; N], S::Error> {
    let mut bytes = [0u8; N];
    for byte in &mut bytes {
        *byte = source.next_byte()?;
    }
    Ok(bytes)
}

/// Core's `ReadCompactSize` with its range check. The whole encoding is read before it is judged.
fn read_compact_size<S: ByteSource>(source: &mut S) -> Result<u64, S::Error> {
    let (n, least) = match source.next_byte()? {
        253 => (u64::from(u16::from_le_bytes(take(source)?)), 253),
        254 => (u64::from(u32::from_le_bytes(take(source)?)), 0x1_0000),
        255 => (u64::from_le_bytes(take(source)?), 0x1_0000_0000),
        n => (u64::from(n), 0),
    };
    if n < least || n > MAX_SIZE {
        return Err(FilterError::BadCount.into());
    }
    Ok(n)
}

/// The count of the lazy matchers: a count that cannot be read is no elements, whether its bytes
/// are no count Core accepts or the source failed on them (as the stream reader always took it).
fn lazy_count<S: ByteSource>(source: &mut S) -> u64 {
    read_compact_size(source).unwrap_or(0)
}

/// `n * m`, the range the hashes of a filter of `n` elements map into.
fn range_of(n: u64, params: &GcsParams) -> Result<u64, FilterError> {
    n.checked_mul(params.m).ok_or(FilterError::RangeOverflow)
}

/// Core's `BitStreamWriter`: bits, most significant first, into a byte vector.
struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    buffer: u8,
    offset: u8,
}

impl<'a> BitWriter<'a> {
    fn new(out: &'a mut Vec<u8>) -> BitWriter<'a> {
        BitWriter {
            out,
            buffer: 0,
            offset: 0,
        }
    }

    /// Writes the low `nbits` bits of `data`.
    fn write(&mut self, data: u64, mut nbits: u8) {
        debug_assert!(nbits <= 64);
        while nbits > 0 {
            let bits = (8 - self.offset).min(nbits);
            self.buffer |= ((data << (64 - nbits)) >> (64 - 8 + self.offset)) as u8;
            self.offset += bits;
            nbits -= bits;
            if self.offset == 8 {
                self.flush();
            }
        }
    }

    /// Writes the bits not yet written, padded with zeros.
    fn flush(&mut self) {
        if self.offset > 0 {
            self.out.push(self.buffer);
            self.buffer = 0;
            self.offset = 0;
        }
    }
}

/// Core's `BitStreamReader` over a [`ByteSource`].
struct BitReader<S> {
    source: S,
    /// The byte being read; its low `left` bits are unread.
    buffer: u8,
    left: u8,
}

impl<S: ByteSource> BitReader<S> {
    fn new(source: S) -> BitReader<S> {
        BitReader {
            source,
            buffer: 0,
            left: 0,
        }
    }

    fn load(&mut self) -> Result<(), S::Error> {
        self.buffer = self.source.next_byte()?;
        self.left = 8;
        Ok(())
    }

    /// Reads `nbits` bits (at most 64), first bit most significant.
    fn read(&mut self, mut nbits: u8) -> Result<u64, S::Error> {
        debug_assert!(nbits <= 64);
        let mut data = 0u64;
        while nbits > 0 {
            if self.left == 0 {
                self.load()?;
            }
            let take = self.left.min(nbits);
            let unread = self.buffer << (8 - self.left);
            data = (data << take) | u64::from(unread >> (8 - take));
            self.left -= take;
            nbits -= take;
        }
        Ok(data)
    }

    /// Reads one-bits up to the next zero-bit (which it consumes), returning how many.
    fn read_unary(&mut self) -> Result<u64, S::Error> {
        let mut ones = 0u64;
        loop {
            if self.left == 0 {
                self.load()?;
            }
            // The unread bits at the top, zeros below them: no run of ones is longer than `left`.
            let run = (self.buffer << (8 - self.left)).leading_ones() as u8;
            ones += u64::from(run);
            if run < self.left {
                self.left -= run + 1;
                return Ok(ones);
            }
            self.left = 0;
        }
    }
}

impl BitReader<SliceSource<'_>> {
    /// Whether bytes remain after the last one this reader loaded.
    fn has_unread_bytes(&self) -> bool {
        !self.source.remaining().is_empty()
    }
}

/// Core's `GolombRiceEncode`: `n` as a unary quotient and a `p`-bit remainder.
fn golomb_rice_encode(writer: &mut BitWriter<'_>, p: u8, n: u64) {
    let mut q = n >> p;
    while q > 0 {
        let nbits = q.min(64);
        writer.write(!0u64, nbits as u8);
        q -= nbits;
    }
    writer.write(0, 1);
    writer.write(n, p);
}

/// Core's `GolombRiceDecode`.
fn golomb_rice_decode<S: ByteSource>(reader: &mut BitReader<S>, p: u8) -> Result<u64, S::Error> {
    let q = reader.read_unary()?;
    let r = reader.read(p)?;
    Ok((q << p).wrapping_add(r))
}

/// Encodes the set `elements` as a filter: Core's `GCSFilter(params, elements)`. BIP158 counts
/// distinct elements, so duplicates fold first, and an empty element is the caller's to skip.
///
/// # Errors
///
/// [`FilterError::TooManyElements`] for 2^32 elements or more, [`FilterError::RangeOverflow`]
/// when `N * M` does not fit 64 bits.
pub fn encode(params: &GcsParams, elements: &mut FilterQuery) -> Result<Vec<u8>, FilterError> {
    elements.dedup();
    let n = u32::try_from(elements.len()).map_err(|_| FilterError::TooManyElements)?;
    let f = range_of(u64::from(n), params)?;

    let mut out = Vec::new();
    write_compact_size(&mut out, u64::from(n));
    if n == 0 {
        return Ok(out);
    }

    let mut mapped = elements.hash_to_range(params, f);
    mapped.sort_unstable();

    let mut writer = BitWriter::new(&mut out);
    let mut last = 0;
    for value in mapped {
        golomb_rice_encode(&mut writer, params.p, value - last);
        last = value;
    }
    writer.flush();
    Ok(out)
}

/// Core's `GCSFilter(params, encoded, skip_decode_check = false)`: the filter holds exactly the
/// `N` values its count says, and nothing after them. Returns `N`.
///
/// # Errors
///
/// [`FilterError::BadCount`] for a count Core's `ReadCompactSize` refuses,
/// [`FilterError::Truncated`] when the data ends inside the values,
/// [`FilterError::ExcessData`] for bytes after them.
pub fn check_encoded(params: &GcsParams, encoded: &[u8]) -> Result<u32, FilterError> {
    let (n, rest) = read_count(encoded)?;
    let mut reader = BitReader::new(SliceSource::new(rest));
    for _ in 0..n {
        golomb_rice_decode(&mut reader, params.p)?;
    }
    if reader.has_unread_bytes() {
        return Err(FilterError::ExcessData);
    }
    Ok(n)
}

/// The element count of a filter and the bit stream after it: what Core's `GCSFilter`
/// constructor reads before it decodes anything, and does even when `skip_decode_check` skips
/// the decode (`ReadFilterFromFile`).
///
/// # Errors
///
/// [`FilterError::BadCount`] for a count Core's `ReadCompactSize` refuses,
/// [`FilterError::TooManyElements`] for one that does not fit 32 bits,
/// [`FilterError::Truncated`] when the data ends inside the count.
pub fn read_count(encoded: &[u8]) -> Result<(u32, &[u8]), FilterError> {
    let mut source = SliceSource::new(encoded);
    let n = read_compact_size(&mut source)?;
    let n = u32::try_from(n).map_err(|_| FilterError::TooManyElements)?;
    Ok((n, source.remaining()))
}

/// Whether any element of `query` is in the filter.
///
/// # Errors
///
/// [`FilterError::Truncated`] when the data ends before a value the match reads,
/// [`FilterError::RangeOverflow`] when `N * M` does not fit 64 bits.
pub fn match_any(
    params: &GcsParams,
    encoded: &[u8],
    query: &FilterQuery,
) -> Result<bool, FilterError> {
    match_any_from(params, SliceSource::new(encoded), query)
}

/// Whether any element of `query` is in the filter [`ByteSource`] gives: [`match_any`] over a
/// stream. It reads the count and then only the values up to the one that decides the result: the
/// first match, or where the sorted query is passed or the values run out. So the source is left
/// at or before the filter's end, never beyond it, and an empty query reads nothing after the
/// count. A count that cannot be read, including because the source failed on it, is no elements.
///
/// # Errors
///
/// The source's error when it fails before a value the match reads, and
/// [`FilterError::RangeOverflow`] (converted) when `N * M` does not fit 64 bits.
pub fn match_any_from<S: ByteSource>(
    params: &GcsParams,
    mut source: S,
    query: &FilterQuery,
) -> Result<bool, S::Error> {
    let n = lazy_count(&mut source);
    // For an empty query set, "any" should be false (no items to match).
    if query.is_empty() || n == 0 {
        return Ok(false);
    }
    // map hashes to [0, n_elements << grp]
    let mut mapped = query.hash_to_range(params, range_of(n, params)?);
    mapped.sort_unstable();

    // find first match in two sorted arrays in one read pass
    let mut reader = BitReader::new(source);
    let mut data = golomb_rice_decode(&mut reader, params.p)?;
    let mut remaining = n - 1;
    for p in mapped {
        loop {
            match data.cmp(&p) {
                Ordering::Equal => return Ok(true),
                Ordering::Less => {
                    if remaining > 0 {
                        data = data.wrapping_add(golomb_rice_decode(&mut reader, params.p)?);
                        remaining -= 1;
                    } else {
                        return Ok(false);
                    }
                }
                Ordering::Greater => break,
            }
        }
    }
    Ok(false)
}

/// Whether every element of `query` is in the filter. An empty query matches.
///
/// # Errors
///
/// [`FilterError::Truncated`] when the data ends before a value the match reads,
/// [`FilterError::RangeOverflow`] when `N * M` does not fit 64 bits.
pub fn match_all(
    params: &GcsParams,
    encoded: &[u8],
    query: &FilterQuery,
) -> Result<bool, FilterError> {
    match_all_from(params, SliceSource::new(encoded), query)
}

/// Whether every element of `query` is in the filter [`ByteSource`] gives: [`match_all`] over a
/// stream, with the reads of [`match_any_from`]: it stops at the first element that is not there,
/// or at the value that matches the last.
///
/// # Errors
///
/// As [`match_any_from`].
pub fn match_all_from<S: ByteSource>(
    params: &GcsParams,
    mut source: S,
    query: &FilterQuery,
) -> Result<bool, S::Error> {
    let n = lazy_count(&mut source);
    if query.is_empty() {
        return Ok(true);
    }
    if n == 0 {
        return Ok(false);
    }
    let mut mapped = query.hash_to_range(params, range_of(n, params)?);
    mapped.sort_unstable();
    mapped.dedup();

    // figure if all mapped are there in one read pass
    let mut reader = BitReader::new(source);
    let mut data = golomb_rice_decode(&mut reader, params.p)?;
    let mut remaining = n - 1;
    for p in mapped {
        loop {
            match data.cmp(&p) {
                Ordering::Equal => break,
                Ordering::Less => {
                    if remaining > 0 {
                        data = data.wrapping_add(golomb_rice_decode(&mut reader, params.p)?);
                        remaining -= 1;
                    } else {
                        return Ok(false);
                    }
                }
                Ordering::Greater => return Ok(false),
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns() -> Vec<Vec<u8>> {
        (0..16u8).map(|i| vec![i * 0x11; 3]).collect()
    }

    fn query<'a>(elements: impl IntoIterator<Item = &'a [u8]>) -> FilterQuery {
        elements.into_iter().collect()
    }

    fn zero_params() -> GcsParams {
        GcsParams::new(0, 0, BASIC_FILTER_P, BASIC_FILTER_M).unwrap()
    }

    #[test]
    fn test_filter() {
        let patterns = patterns();
        let params = zero_params();
        let mut set = query(patterns.iter().map(Vec::as_slice));
        let bytes = encode(&params, &mut set).unwrap();
        assert_eq!(check_encoded(&params, &bytes), Ok(16));

        let any = query([&[0xab, 0xcd, 0xef][..], &[0xee; 3][..]]);
        assert!(match_any(&params, &bytes, &any).unwrap());
        let none = query([&[0xab, 0xcd, 0xef][..], &[0x12, 0x34, 0x56][..]]);
        assert!(!match_any(&params, &bytes, &none).unwrap());

        let all = query(patterns.iter().map(Vec::as_slice));
        assert!(match_all(&params, &bytes, &all).unwrap());
        let mut more = all.clone();
        more.push(&[0xab, 0xcd, 0xef]);
        assert!(!match_all(&params, &bytes, &more).unwrap());
    }

    #[test]
    fn duplicates_fold_before_counting() {
        let params = zero_params();
        let mut doubled = query([&[1u8, 2, 3][..], &[1, 2, 3][..], &[4, 5, 6][..]]);
        let mut single = query([&[1u8, 2, 3][..], &[4, 5, 6][..]]);
        let bytes = encode(&params, &mut doubled).unwrap();
        assert_eq!(bytes, encode(&params, &mut single).unwrap());
        assert_eq!(check_encoded(&params, &bytes), Ok(2));
    }

    #[test]
    fn empty_set_is_one_zero_byte() {
        let params = zero_params();
        let bytes = encode(&params, &mut FilterQuery::new()).unwrap();
        assert_eq!(bytes, [0]);
        assert_eq!(check_encoded(&params, &bytes), Ok(0));
        let q = query([&[1u8][..]]);
        assert!(!match_any(&params, &bytes, &q).unwrap());
        assert!(!match_all(&params, &bytes, &q).unwrap());
        assert!(!match_any(&params, &bytes, &FilterQuery::new()).unwrap());
        assert!(match_all(&params, &bytes, &FilterQuery::new()).unwrap());
    }

    #[test]
    fn check_encoded_is_strict() {
        let params = zero_params();
        let mut set = query(patterns().iter().map(Vec::as_slice));
        let bytes = encode(&params, &mut set).unwrap();

        let mut excess = bytes.clone();
        excess.push(0);
        assert_eq!(check_encoded(&params, &excess), Err(FilterError::ExcessData));
        let truncated = &bytes[..bytes.len() - 1];
        assert_eq!(check_encoded(&params, truncated), Err(FilterError::Truncated));
        assert_eq!(check_encoded(&params, &[]), Err(FilterError::Truncated));
        // 253 with a value below 253 is non-canonical; above MAX_SIZE is refused.
        assert_eq!(check_encoded(&params, &[253, 5, 0]), Err(FilterError::BadCount));
        assert_eq!(check_encoded(&params, &[254, 1, 0, 0, 0x02]), Err(FilterError::BadCount));
        // the lazy path reads a bad count as no elements
        let q = query([&[1u8][..]]);
        assert!(!match_any(&params, &[253, 5, 0], &q).unwrap());
        // and a stream that ends early only fails when the match needs the values
        assert_eq!(
            match_any(&params, truncated, &query(patterns().iter().map(Vec::as_slice))),
            Ok(true)
        );
        let everything = query(patterns().iter().map(Vec::as_slice));
        assert_eq!(match_all(&params, truncated, &everything), Err(FilterError::Truncated));
    }

    #[test]
    fn params_bound_p() {
        assert_eq!(GcsParams::new(0, 0, 64, 1), Err(FilterError::InvalidParams));
        assert!(GcsParams::new(0, 0, 63, 1).is_ok());
    }

    #[test]
    fn bit_stream() {
        let mut out = Vec::new();
        {
            let mut writer = BitWriter::new(&mut out);
            writer.write(0, 1); // 0
            writer.write(2, 2); // 10
            writer.write(6, 3); // 110
            writer.write(11, 4); // 1011
            writer.write(1, 5); // 00001
            writer.write(32, 6); // 100000
            writer.write(7, 7); // 0000111
            writer.flush();
        }
        assert_eq!(
            "01011010110000110000000001110000",
            format!("{:08b}{:08b}{:08b}{:08b}", out[0], out[1], out[2], out[3])
        );
        let mut reader = BitReader::new(SliceSource::new(&out));
        assert_eq!(reader.read(1).unwrap(), 0);
        assert_eq!(reader.read(2).unwrap(), 2);
        assert_eq!(reader.read(3).unwrap(), 6);
        assert_eq!(reader.read(4).unwrap(), 11);
        assert_eq!(reader.read(5).unwrap(), 1);
        assert_eq!(reader.read(6).unwrap(), 32);
        assert_eq!(reader.read(7).unwrap(), 7);
        // 4 bits remained
        assert!(reader.read(5).is_err());
    }

    #[test]
    fn unary_and_golomb_rice_round_trip() {
        for p in [0u8, 1, 7, 19, 32, 33, 63] {
            // the quotient stays small: it costs that many bits
            let mut values = vec![0u64, 1, 2, 255, 256];
            values.extend(
                [(1u64, 5u64), (777, 0), (65, 3)]
                    .into_iter()
                    .filter_map(|(q, r)| q.checked_mul(1 << p)?.checked_add(r)),
            );
            let mut out = Vec::new();
            let mut writer = BitWriter::new(&mut out);
            for &v in &values {
                golomb_rice_encode(&mut writer, p, v);
            }
            writer.flush();
            let mut reader = BitReader::new(SliceSource::new(&out));
            for &v in &values {
                assert_eq!(golomb_rice_decode(&mut reader, p).unwrap(), v, "p {p}");
            }
            assert!(!reader.has_unread_bytes());
        }
    }

    #[test]
    fn a_stream_is_read_only_as_far_as_the_match_needs() {
        let params = zero_params();
        let all = patterns();
        let bytes = encode(&params, &mut query(all.iter().map(Vec::as_slice))).unwrap();
        let taken = |source: &SliceSource| bytes.len() - source.remaining().len();

        // An empty query takes the count and nothing else.
        let mut source = SliceSource::new(&bytes);
        assert_eq!(match_any_from(&params, &mut source, &FilterQuery::new()), Ok(false));
        assert_eq!(taken(&source), 1);
        let mut source = SliceSource::new(&bytes);
        assert_eq!(match_all_from(&params, &mut source, &FilterQuery::new()), Ok(true));
        assert_eq!(taken(&source), 1);

        // A match returns where the matching value ends: the first value of the filter is the
        // cheapest, and the whole filter is taken only for the last one.
        let mut sizes = Vec::new();
        for pattern in &all {
            let mut source = SliceSource::new(&bytes);
            let hit = query([pattern.as_slice()]);
            assert_eq!(match_any_from(&params, &mut source, &hit), Ok(true));
            sizes.push(taken(&source));
        }
        assert!(sizes.iter().min().unwrap() < &bytes.len(), "{sizes:?}");
        assert_eq!(*sizes.iter().max().unwrap(), bytes.len(), "{sizes:?}");
    }

    #[test]
    fn concatenated_filters_are_read_one_at_a_time() {
        let params = zero_params();
        let first = encode(&params, &mut query([&[1u8, 2, 3][..]])).unwrap();
        let second = encode(&params, &mut query([&[4u8, 5, 6][..], &[7, 8, 9][..]])).unwrap();
        let mut both = first.clone();
        both.extend_from_slice(&second);

        let mut source = SliceSource::new(&both);
        assert_eq!(match_any_from(&params, &mut source, &query([&[1u8, 2, 3][..]])), Ok(true));
        assert_eq!(source.remaining(), second);
        // A miss on a one-element filter takes just that filter.
        let mut source = SliceSource::new(&both);
        assert_eq!(match_all_from(&params, &mut source, &query([&[9u8, 9, 9][..]])), Ok(false));
        assert_eq!(source.remaining(), second);
        // Matching every element of the second filter takes it to its end, and the source stops there.
        let both_elements = query([&[4u8, 5, 6][..], &[7, 8, 9][..]]);
        assert_eq!(match_all_from(&params, &mut source, &both_elements), Ok(true));
        assert!(source.remaining().is_empty());
    }

    #[test]
    fn the_generic_domain_is_kept() {
        let element = query([&b"hello"[..]]);
        // Parameters the basic filter never uses: p above 32, M above 32 bits.
        for (p, m) in [(33u8, BASIC_FILTER_M), (19, 1 << 32), (63, 1), (0, 3), (19, 0)] {
            let params = GcsParams::new(0, 0, p, m).unwrap();
            let bytes = encode(&params, &mut element.clone()).unwrap();
            assert_eq!(check_encoded(&params, &bytes), Ok(1), "p {p} m {m}");
            assert!(match_any(&params, &bytes, &element).unwrap(), "p {p} m {m}");
            assert!(match_all(&params, &bytes, &element).unwrap(), "p {p} m {m}");
        }
    }

    #[test]
    fn a_range_that_overflows_is_an_error_not_a_wrap() {
        let params = GcsParams::new(0, 0, 19, u64::MAX).unwrap();
        let two = query([&[1u8][..], &[2][..]]);
        assert_eq!(encode(&params, &mut two.clone()), Err(FilterError::RangeOverflow));
        // A filter that says two elements, read with such an M.
        let bytes = [2u8, 0, 0, 0, 0, 0];
        assert_eq!(match_any(&params, &bytes, &two), Err(FilterError::RangeOverflow));
        assert_eq!(match_all(&params, &bytes, &two), Err(FilterError::RangeOverflow));
        // Nothing to match, nothing to map.
        assert_eq!(match_any(&params, &bytes, &FilterQuery::new()), Ok(false));
        assert_eq!(match_all(&params, &bytes, &FilterQuery::new()), Ok(true));
    }
}
