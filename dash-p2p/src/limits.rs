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

#[cfg(test)]
mod tests {
    #[test]
    fn matches_base_sdk_limits() {
        assert_eq!(super::MAX_SIZE, dash_p2p_core::limits::MAX_SIZE);
        assert_eq!(
            super::MAX_PROTOCOL_MESSAGE_LENGTH,
            dash_p2p_core::limits::MAX_PROTOCOL_MESSAGE_LENGTH
        );
        assert_eq!(super::MAX_INV_SZ, dash_p2p_core::limits::MAX_INV_SZ);
        assert_eq!(super::MAX_GETDATA_SZ, dash_p2p_core::limits::MAX_GETDATA_SZ);
        assert_eq!(super::MAX_LOCATOR_SZ, dash_p2p_core::limits::MAX_LOCATOR_SZ);
        assert_eq!(super::MAX_ADDR_TO_SEND, dash_p2p_core::limits::MAX_ADDR_TO_SEND);
        assert_eq!(
            super::MAX_HEADERS_UNCOMPRESSED_RESULT,
            dash_p2p_core::limits::MAX_HEADERS_UNCOMPRESSED_RESULT
        );
        assert_eq!(
            super::MAX_HEADERS_COMPRESSED_RESULT,
            dash_p2p_core::limits::MAX_HEADERS_COMPRESSED_RESULT
        );
        assert_eq!(super::MAX_BLOOM_FILTER_SIZE, dash_p2p_core::limits::MAX_BLOOM_FILTER_SIZE);
        assert_eq!(super::MAX_HASH_FUNCS, dash_p2p_core::limits::MAX_HASH_FUNCS);
        assert_eq!(super::MAX_SCRIPT_ELEMENT_SIZE, dash_p2p_core::limits::MAX_SCRIPT_ELEMENT_SIZE);
        assert_eq!(super::MAX_SIG_SES_ANNS, dash_p2p_core::MAX_SIG_SES_ANNS);
        assert_eq!(super::MAX_SIG_SHARES_INVS, dash_p2p_core::MAX_SIG_SHARES_INVS);
        assert_eq!(super::MAX_SIG_SHARES, dash_p2p_core::MAX_SIG_SHARES);
        assert_eq!(super::MAX_BATCHED_SIGS, dash_p2p_core::MAX_BATCHED_SIGS);
        assert_eq!(super::MAX_QGETDATA_SIZE, dash_p2p_core::QuorumDataRequest::SIZE);
        assert_eq!(super::MAX_DKG_PAYLOAD_SIZE, 1 << 20);
    }
}
