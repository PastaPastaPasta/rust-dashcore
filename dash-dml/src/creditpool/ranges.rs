//! Core's `CRangesSet` (`util/ranges_set.{h,cpp}` at v24.0.0-rc.4): the credit pool's used
//! asset-unlock indexes, kept as disjoint half-open ranges, with v24's wrap-around successor
//! (CONS-10) and its strict decoder (EV3 D11).

use std::collections::BTreeSet;
use std::fmt;

use dash_consensus::primitives::{MAX_SIZE, ReadErrorKind, read_compact_size, write_compact_size};

/// `WrappedSuccessor`: the range holding `u64::MAX` stores the end 0.
// Core: util/ranges_set.cpp:13-16 WrappedSuccessor
const fn wrapped_successor(value: u64) -> u64 {
    if value == u64::MAX {
        0
    } else {
        value + 1
    }
}

/// Core's `CRangesSet`: a set of `u64` kept as disjoint half-open ranges.
///
/// `Remove` has no caller outside Core's tests at the pin and is not ported (D11).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RangesSet {
    /// `[begin, end)` pairs in Core's `Range::operator<` order (begin, then end). `end == 0` is
    /// the range that runs to `u64::MAX` inclusive.
    pub(crate) ranges: BTreeSet<(u64, u64)>,
}

/// Why [`RangesSet::decode`] fails: Core's `std::ios_base::failure`s, with their text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangesDecodeError {
    /// The bytes end early, or the count is not a canonical compact size within `MAX_SIZE`
    /// (`ReadCompactSize`).
    Read(ReadErrorKind),
    /// `oversized CRangesSet range count`.
    Oversized,
    /// `unrepresentable full-domain CRangesSet range`: `[0, 0)`.
    FullDomain,
    /// `invalid empty CRangesSet range`: `begin >= end` with `end != 0`.
    Empty,
    /// `noncanonical CRangesSet ranges`: overlapping, unordered or adjacent ranges.
    Noncanonical,
    /// `wrapped CRangesSet range must be last`.
    WrappedNotLast,
}

impl fmt::Display for RangesDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(kind) => fmt::Display::fmt(kind, f),
            Self::Oversized => f.write_str("oversized CRangesSet range count"),
            Self::FullDomain => f.write_str("unrepresentable full-domain CRangesSet range"),
            Self::Empty => f.write_str("invalid empty CRangesSet range"),
            Self::Noncanonical => f.write_str("noncanonical CRangesSet ranges"),
            Self::WrappedNotLast => f.write_str("wrapped CRangesSet range must be last"),
        }
    }
}

impl std::error::Error for RangesDecodeError {}

impl From<ReadErrorKind> for RangesDecodeError {
    fn from(kind: ReadErrorKind) -> Self {
        Self::Read(kind)
    }
}

impl RangesSet {
    /// `DEFAULT_MAX_RANGES`: `MAX_SIZE`.
    // Core: util/ranges_set.h:53
    pub const DEFAULT_MAX_RANGES: u64 = MAX_SIZE as u64;

    /// `Add(value)`: false when `value` is already in the set; else inserts it, merging with the
    /// range that ends at `value` and the one that starts at its successor.
    // Core: util/ranges_set.cpp:27-58 CRangesSet::Add
    pub fn add(&mut self, value: u64) -> bool {
        if self.contains(value) {
            return false;
        }

        let mut new_range = (value, wrapped_successor(value));
        // The range before `lower_bound({value, value})`, if it ends at `value`.
        if let Some(&prev) = self.ranges.range(..(value, value)).next_back()
            && prev.1 == value
        {
            new_range.0 = prev.0;
            self.ranges.remove(&prev);
        }
        // The range at `lower_bound({value, value})`, if it starts at the successor.
        if let Some(&next) = self.ranges.range((value, value)..).next()
            && next.0 == wrapped_successor(value)
        {
            new_range.1 = next.1;
            self.ranges.remove(&next);
        }
        let inserted = self.ranges.insert(new_range);
        debug_assert!(inserted, "Core asserts the merged range is new");
        true
    }

    /// `Contains(value)`.
    // Core: util/ranges_set.cpp:103-111 CRangesSet::Contains
    pub fn contains(&self, value: u64) -> bool {
        if let Some(&(begin, _)) = self.ranges.range((value, value)..).next()
            && begin == value
        {
            return true;
        }
        match self.ranges.range(..(value, value)).next_back() {
            None => false,
            Some(&(begin, end)) => begin <= value && (end == 0 || end > value),
        }
    }

    /// `Size()`: the number of values, as Core's `size_t` (wrapping, as unsigned arithmetic
    /// does; only a set Core cannot decode or build in practice reaches it).
    // Core: util/ranges_set.cpp:92-101 CRangesSet::Size
    pub fn size(&self) -> u64 {
        self.ranges.iter().fold(0u64, |size, &(begin, end)| {
            size.wrapping_add(if end == 0 {
                (u64::MAX - begin).wrapping_add(1)
            } else {
                end.wrapping_sub(begin)
            })
        })
    }

    /// `IsEmpty()`.
    // Core: util/ranges_set.cpp:87-90 CRangesSet::IsEmpty
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// `RangeCount()`: the number of stored ranges.
    // Core: util/ranges_set.h:79 CRangesSet::RangeCount
    pub fn range_count(&self) -> usize {
        self.ranges.len()
    }

    /// The `[begin, end)` ranges in order (`end == 0` for the range that holds `u64::MAX`).
    pub fn ranges(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.ranges.iter().copied()
    }

    /// `Serialize`: the compact-size count, then each `(begin, end)` as two little-endian `u64`.
    // Core: util/ranges_set.h:86-91 CRangesSet::Serialize
    pub fn encode(&self, out: &mut Vec<u8>) {
        write_compact_size(self.ranges.len() as u64, out);
        for &(begin, end) in &self.ranges {
            out.extend(begin.to_le_bytes());
            out.extend(end.to_le_bytes());
        }
    }

    /// `Unserialize`: [`RangesSet::decode_bounded`] with [`RangesSet::DEFAULT_MAX_RANGES`].
    // Core: util/ranges_set.h:127-131 CRangesSet::Unserialize
    pub fn decode(input: &mut &[u8]) -> Result<Self, RangesDecodeError> {
        Self::decode_bounded(input, Self::DEFAULT_MAX_RANGES)
    }

    /// `UnserializeBounded(s, max_ranges)`: only the canonical encoding decodes.
    // Core: util/ranges_set.h:93-125 CRangesSet::UnserializeBounded
    pub fn decode_bounded(input: &mut &[u8], max_ranges: u64) -> Result<Self, RangesDecodeError> {
        let count = read_compact_size(input, true)?;
        if count > max_ranges {
            return Err(RangesDecodeError::Oversized);
        }
        let mut decoded = BTreeSet::new();
        let mut previous_end = 0u64;
        let mut have_previous = false;
        for i in 0..count {
            let begin = read_u64(input)?;
            let end = read_u64(input)?;
            let wrapped_max = end == 0;
            if wrapped_max && begin == 0 {
                return Err(RangesDecodeError::FullDomain);
            }
            if !wrapped_max && begin >= end {
                return Err(RangesDecodeError::Empty);
            }
            // Equality is adjacent and must have been merged; less-than is overlapping or
            // unordered. Both are noncanonical and could make Size() underflow.
            if have_previous && (previous_end == 0 || begin <= previous_end) {
                return Err(RangesDecodeError::Noncanonical);
            }
            if wrapped_max && i + 1 != count {
                return Err(RangesDecodeError::WrappedNotLast);
            }
            previous_end = end;
            have_previous = true;
            decoded.insert((begin, end));
        }
        Ok(Self {
            ranges: decoded,
        })
    }
}

fn read_u64(input: &mut &[u8]) -> Result<u64, ReadErrorKind> {
    let (head, rest) = input.split_first_chunk::<8>().ok_or(ReadErrorKind::EndOfData)?;
    *input = rest;
    Ok(u64::from_le_bytes(*head))
}
