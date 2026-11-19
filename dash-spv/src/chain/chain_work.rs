//! Chain work calculation for determining the best chain
//!
//! This module handles the calculation of cumulative proof of work,
//! which is used to determine the chain with the most work (best chain).

use dashcore::{Header as BlockHeader, Target, Work};
use std::ops::Add;

/// Represents cumulative chain work as a 256-bit integer.
///
/// Backed by [`Work`] (and so by `dash_num::Arith256`); addition wraps on
/// overflow. The byte form is the 32-byte big-endian integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChainWork(Work);

impl ChainWork {
    /// Create a new ChainWork with zero work
    pub fn zero() -> Self {
        Self::from_bytes([0u8; 32])
    }

    /// Calculate work from a single header
    pub fn from_header(header: &BlockHeader) -> Self {
        Self(header.work())
    }

    /// Calculate work from a target, like Dash Core's `GetBlockProof`
    pub fn from_target(target: Target) -> Self {
        Self(target.to_work())
    }

    /// Create ChainWork from accumulated work at a given height plus a new header
    ///
    /// IMPORTANT: This is a temporary approximation that returns only the work from
    /// the current header. For accurate cumulative work calculation, callers should
    /// track the actual cumulative work by summing individual block work values.
    ///
    /// TODO: This function should be refactored to accept the previous cumulative work
    /// as a parameter, or callers should maintain cumulative work separately.
    pub fn from_height_and_header(_height: u32, header: &BlockHeader) -> Self {
        // Currently returns only the work from the current header
        // This is incorrect for cumulative work but better than adding height bytes
        // which has no relation to proof-of-work
        Self::from_header(header)
    }

    /// Add the work from a header to this cumulative work
    pub fn add_header(self, header: &BlockHeader) -> Self {
        self.combine(Self::from_header(header))
    }

    /// Add two ChainWork values
    pub fn combine(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }

    /// Get the work as a big-endian byte array
    pub fn to_bytes(self) -> [u8; 32] {
        self.0.to_be_bytes()
    }

    /// Create from a big-endian byte array
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Work::from_be_bytes(bytes))
    }

    /// Check if this work is zero
    pub fn is_zero(&self) -> bool {
        *self == Self::zero()
    }

    /// Create ChainWork from a big-endian hex string
    pub fn from_hex(hex: &str) -> Result<Self, String> {
        // Remove 0x prefix if present
        let hex = hex.strip_prefix("0x").unwrap_or(hex);

        // Parse hex string to bytes
        let bytes =
            hex_conservative::decode_to_vec(hex).map_err(|e| format!("Invalid hex: {}", e))?;

        let work: [u8; 32] = bytes.try_into().map_err(|bytes: Vec<u8>| {
            format!("Invalid work length: expected 32 bytes, got {}", bytes.len())
        })?;

        Ok(Self::from_bytes(work))
    }
}

impl Default for ChainWork {
    fn default() -> Self {
        Self::zero()
    }
}

impl Add for ChainWork {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        self.combine(other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashcore::blockdata::constants::genesis_block;
    use dashcore::Network;

    #[test]
    fn test_chain_work_comparison() {
        let work1 = ChainWork::dummy(0);
        let work2 = ChainWork::dummy(1);

        assert!(work1 < work2);
        assert!(work2 > work1);
        assert_eq!(work1, work1);
    }

    #[test]
    fn test_chain_work_addition() {
        let work1 = ChainWork::dummy(100);
        let work2 = ChainWork::dummy(200);

        let sum = work1.add(work2);
        assert_eq!(sum.to_bytes()[31], 44); // 100 + 200 = 300, which is 44 + 256
        assert_eq!(sum.to_bytes()[30], 1); // Carry
    }

    #[test]
    fn test_chain_work_from_header() {
        let genesis = genesis_block(Network::Mainnet).header;
        let work = ChainWork::from_header(&genesis);
        assert!(!work.is_zero());
    }

    #[test]
    fn test_chain_work_ordering() {
        let works: Vec<ChainWork> = (0..5).map(ChainWork::dummy).collect();

        for i in 0..4 {
            assert!(works[i] < works[i + 1]);
        }
    }

    #[test]
    fn test_chain_work_from_target_precision() {
        // Test that lower targets (harder to mine) produce more work
        // Target with leading zeros (harder)
        let mut harder_target_bytes = [0u8; 32];
        harder_target_bytes[8] = 0xff; // 00000000 00000000 ff...
        let harder_target = Target::from_be_bytes(harder_target_bytes);

        // Target with fewer leading zeros (easier)
        let mut easier_target_bytes = [0u8; 32];
        easier_target_bytes[4] = 0xff; // 00000000 ff...
        let easier_target = Target::from_be_bytes(easier_target_bytes);

        let harder_work = ChainWork::from_target(harder_target);
        let easier_work = ChainWork::from_target(easier_target);

        // Harder target should produce more work
        assert!(harder_work > easier_work, "Harder target (lower value) should produce more work");

        // Test that work values are significantly different
        // (not just by 1 byte as in the old implementation)
        let diff_position = harder_work
            .to_bytes()
            .iter()
            .zip(easier_work.to_bytes().iter())
            .position(|(a, b)| a != b)
            .expect("Work values should differ");

        assert!(
            diff_position < 30,
            "Work values should differ in significant bytes, not just the least significant"
        );
    }

    #[test]
    fn test_chain_work_granularity() {
        // Test that similar targets produce slightly different work values
        let mut target1_bytes = [0u8; 32];
        target1_bytes[10] = 0x10;
        target1_bytes[11] = 0x00;
        let target1 = Target::from_be_bytes(target1_bytes);

        let mut target2_bytes = [0u8; 32];
        target2_bytes[10] = 0x10;
        target2_bytes[11] = 0x01; // Slightly different
        let target2 = Target::from_be_bytes(target2_bytes);

        let work1 = ChainWork::from_target(target1);
        let work2 = ChainWork::from_target(target2);

        // Works should be different
        assert_ne!(work1, work2, "Similar targets should produce different work values");

        // Target2 is slightly higher (easier), so should have slightly less work
        assert!(work1 > work2, "Lower target should produce more work");
    }

    /// Pins the byte form: a 32-byte big-endian integer, compared numerically.
    #[test]
    fn test_chain_work_byte_layout_is_big_endian() {
        let bytes: [u8; 32] = core::array::from_fn(|i| i as u8);
        assert_eq!(ChainWork::from_bytes(bytes).to_bytes(), bytes);

        let one = ChainWork::dummy(1);

        // The most significant byte comes first, across both 128-bit halves.
        let mut high_half = [0u8; 32];
        high_half[15] = 1;
        let mut low_half = [0u8; 32];
        low_half[16..].fill(0xff);
        assert!(ChainWork::from_bytes(high_half) > ChainWork::from_bytes(low_half));
        assert_eq!(
            ChainWork::from_bytes(low_half) + one,
            ChainWork::from_bytes(high_half),
            "carry crosses from the low half into the high half"
        );
    }

    #[test]
    fn test_chain_work_from_hex() {
        let hex = "000000000000000000000000000000000000000000000000000000000010001f";
        let work = ChainWork::from_hex(hex).unwrap();
        assert_eq!(work.to_bytes()[29..], [0x10, 0x00, 0x1f]);
        assert_eq!(ChainWork::from_hex(&format!("0x{hex}")).unwrap(), work);

        assert!(ChainWork::from_hex("0x00").is_err());
        assert!(ChainWork::from_hex(&format!("{hex}00")).is_err());
        assert!(ChainWork::from_hex(&hex.replace('f', "g")).is_err());
    }

    #[test]
    fn test_chain_work_addition_wraps() {
        let max = ChainWork::from_bytes([0xff; 32]);
        assert_eq!(max + ChainWork::dummy(1), ChainWork::zero());
        assert!(ChainWork::zero().is_zero());
        assert!(ChainWork::default().is_zero());
    }

    /// Dash Core reports `chainwork` 0x100010 for the mainnet genesis block.
    #[test]
    fn test_chain_work_matches_core_for_genesis() {
        let genesis = genesis_block(Network::Mainnet).header;
        let expected =
            ChainWork::from_hex("0000000000000000000000000000000000000000000000000000000000100010")
                .unwrap();
        assert_eq!(ChainWork::from_header(&genesis), expected);
        assert_eq!(
            ChainWork::zero().add_header(&genesis).add_header(&genesis),
            expected + expected
        );
    }
}
