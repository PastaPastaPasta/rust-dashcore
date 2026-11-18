use std::fmt;

use dash_kv::codec::{CodecError, ValueCodec, read_varint_u32, read_varint_u64, write_varint};

/// Per-file statistics of a blk/rev pair.
// Core: chain.h CBlockFileInfo
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockFileInfo {
    /// Number of blocks stored in the file.
    pub blocks: u32,
    /// Used bytes of the blk file.
    pub size: u32,
    /// Used bytes of the rev file.
    pub undo_size: u32,
    pub height_first: u32,
    pub height_last: u32,
    pub time_first: u64,
    pub time_last: u64,
}

impl BlockFileInfo {
    /// Updates the block count and the height and time ranges (not `size`).
    // Core: chain.h CBlockFileInfo::AddBlock
    pub fn add_block(&mut self, height: u32, time: u64) {
        if self.blocks == 0 || self.height_first > height {
            self.height_first = height;
        }
        if self.blocks == 0 || self.time_first > time {
            self.time_first = time;
        }
        self.blocks += 1;
        if height > self.height_last {
            self.height_last = height;
        }
        if time > self.time_last {
            self.time_last = time;
        }
    }
}

impl fmt::Display for BlockFileInfo {
    // Core: chain.cpp CBlockFileInfo::ToString (the dates are rendered by the caller's logger)
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CBlockFileInfo(blocks={}, size={}, heights={}...{}, time={}...{})",
            self.blocks,
            self.size,
            self.height_first,
            self.height_last,
            self.time_first,
            self.time_last
        )
    }
}

/// Core's serialization: seven `VARINT`s.
impl ValueCodec for BlockFileInfo {
    fn encode_value(&self, out: &mut Vec<u8>) {
        for n in [self.blocks, self.size, self.undo_size, self.height_first, self.height_last] {
            write_varint(out, u64::from(n));
        }
        write_varint(out, self.time_first);
        write_varint(out, self.time_last);
    }

    fn decode_value(mut bytes: &[u8]) -> Result<Self, CodecError> {
        let input = &mut bytes;
        let info = Self {
            blocks: read_varint_u32(input)?,
            size: read_varint_u32(input)?,
            undo_size: read_varint_u32(input)?,
            height_first: read_varint_u32(input)?,
            height_last: read_varint_u32(input)?,
            time_first: read_varint_u64(input)?,
            time_last: read_varint_u64(input)?,
        };
        if !input.is_empty() {
            return Err(CodecError::TrailingBytes(input.len()));
        }
        Ok(info)
    }
}
