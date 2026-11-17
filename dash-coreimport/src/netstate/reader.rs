//! The byte reader every parser uses: all reads are bounds-checked and typed, and no count read
//! from the input sizes an allocation beyond what the remaining bytes can hold.

use super::error::{FormatError, FormatResult};

/// Core's `MAX_SIZE`: the largest compact size a range-checked read accepts.
const MAX_SIZE: u64 = 0x0200_0000;

/// The most elements a `Vec` reserves before any has been read.
const MAX_PREALLOC: usize = 4096;

pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    what: &'static str,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8], what: &'static str) -> Self {
        Self {
            data,
            pos: 0,
            what,
        }
    }

    /// Starts at `pos` of `data`; positions stay relative to `data`.
    pub(crate) fn at(data: &'a [u8], pos: usize, what: &'static str) -> Self {
        Self {
            data,
            pos: pos.min(data.len()),
            what,
        }
    }

    pub(crate) fn what(&self) -> &'static str {
        self.what
    }

    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub(crate) fn corrupt(&self, reason: &'static str) -> FormatError {
        FormatError::Corrupt {
            what: self.what,
            reason,
        }
    }

    pub(crate) fn take(&mut self, n: usize) -> FormatResult<&'a [u8]> {
        if n > self.remaining() {
            return Err(FormatError::Truncated {
                what: self.what,
                offset: self.pos,
            });
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> FormatResult<[u8; N]> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    pub(crate) fn u8(&mut self) -> FormatResult<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub(crate) fn u16_be(&mut self) -> FormatResult<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub(crate) fn u32_le(&mut self) -> FormatResult<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(crate) fn i32_le(&mut self) -> FormatResult<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    pub(crate) fn u64_le(&mut self) -> FormatResult<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    pub(crate) fn i64_le(&mut self) -> FormatResult<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    pub(crate) fn hash256(&mut self) -> FormatResult<[u8; 32]> {
        self.array()
    }

    /// Core's `bool`: one byte, any non-zero value is true (`ser_readdata8`).
    // Core: serialize.h Unserialize(Stream&, bool&)
    pub(crate) fn bool(&mut self) -> FormatResult<bool> {
        Ok(self.u8()? != 0)
    }

    /// Core's `std::string` and `std::vector<unsigned char>`: a range-checked compact size and
    /// that many bytes. Nothing is allocated for the count, so a corrupt one costs nothing.
    // Core: serialize.h Unserialize(Stream&, std::basic_string&)
    pub(crate) fn byte_vec(&mut self) -> FormatResult<&'a [u8]> {
        let n = self.compact_size(true)?;
        // `n` <= MAX_SIZE here, which fits `usize` on every supported target.
        self.take(n as usize)
    }

    /// Core's `LIMITED_VECTOR(v, limit)` over bytes: the ordinary vector, except that a count above
    /// `limit` is refused before any byte is read.
    // Core: serialize.h LimitedVectorFormatter::Unser
    #[cfg(feature = "full")]
    pub(crate) fn limited_byte_vec(&mut self, limit: usize) -> FormatResult<&'a [u8]> {
        let n = self.compact_size(true)?;
        if n > limit as u64 {
            return Err(self.corrupt("vector length limit exceeded"));
        }
        // `n` <= `limit` here.
        self.take(n as usize)
    }

    pub(crate) fn f64_encoded(&mut self) -> FormatResult<f64> {
        // Core's `EncodedDoubleFormatter`: the IEEE-754 bit pattern as a `uint64_t`.
        Ok(f64::from_bits(self.u64_le()?))
    }

    /// Core's `ReadCompactSize`: canonical encodings only, and with `range_check` at most
    /// `MAX_SIZE`.
    // Core: serialize.h ReadCompactSize
    pub(crate) fn compact_size(&mut self, range_check: bool) -> FormatResult<u64> {
        let (n, min) = match self.u8()? {
            first @ 0..=252 => (u64::from(first), 0),
            253 => (u64::from(u16::from_le_bytes(self.array()?)), 253),
            254 => (u64::from(self.u32_le()?), 0x1_0000),
            255 => (self.u64_le()?, 0x1_0000_0000),
        };
        if n < min {
            return Err(self.corrupt("non-canonical compact size"));
        }
        if range_check && n > MAX_SIZE {
            return Err(self.corrupt("compact size exceeds MAX_SIZE"));
        }
        Ok(n)
    }

    /// A `Vec` capacity for `n` elements of at least `min_size` bytes each still to be read:
    /// never more than the remaining bytes can hold, and at most [`MAX_PREALLOC`] elements, so a
    /// corrupt count reserves little however large the file and the in-memory element are (an
    /// element can be several times its on-disk minimum). The `Vec` grows as items really decode.
    pub(crate) fn capacity_for(&self, n: u64, min_size: usize) -> usize {
        usize::try_from(n)
            .unwrap_or(usize::MAX)
            .min(self.remaining() / min_size.max(1))
            .min(MAX_PREALLOC)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_is_bounded_by_the_bytes_left_and_by_the_clamp() {
        let data = vec![0u8; 1 << 20];
        let r = Reader::new(&data, "test");
        // A count the file cannot back reserves what the bytes can hold, never more than the clamp.
        assert_eq!(r.capacity_for(u64::MAX, 1), MAX_PREALLOC);
        assert_eq!(r.capacity_for(MAX_SIZE, 9), MAX_PREALLOC);
        assert_eq!(r.capacity_for(10, 1), 10);
        assert_eq!(r.capacity_for(u64::MAX, usize::MAX), 0);
        assert_eq!(r.capacity_for(5, 0), 5, "a zero element size does not divide by zero");
        let small = Reader::new(&data[..100], "test");
        assert_eq!(small.capacity_for(u64::MAX, 41), 2);
    }
}
