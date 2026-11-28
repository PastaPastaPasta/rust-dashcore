//! Node-side P2P handler limits.

pub const MAX_SIZE: u64 = 0x0200_0000;
pub const MAX_PROTOCOL_MESSAGE_LENGTH: usize = 3 * 1024 * 1024;
pub const MAX_INV_SZ: usize = 50_000;
pub const MAX_GETDATA_SZ: usize = 1_000;
pub const MAX_LOCATOR_SZ: usize = 101;
pub const MAX_ADDR_TO_SEND: usize = 1_000;
pub const MAX_HEADERS_UNCOMPRESSED_RESULT: usize = 2_000;
pub const MAX_HEADERS_COMPRESSED_RESULT: usize = 8_000;
pub const MAX_BLOOM_FILTER_SIZE: usize = 36_000;
pub const MAX_HASH_FUNCS: u32 = 50;
pub const MAX_SCRIPT_ELEMENT_SIZE: usize = 520;
pub const MAX_SIG_SES_ANNS: usize = 100;
pub const MAX_SIG_SHARES_INVS: usize = 200;
pub const MAX_SIG_SHARES: usize = 32;
pub const MAX_BATCHED_SIGS: usize = 400;
pub const MAX_QGETDATA_SIZE: usize = 67;
pub const MAX_DKG_PAYLOAD_SIZE: usize = 1 << 20;
