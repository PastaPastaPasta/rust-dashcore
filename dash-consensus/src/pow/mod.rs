//! Proof of work (Core `pow.cpp`, CS2 §3): the `nBits` the next block must carry, Core's
//! `CheckProofOfWork`, and the `nBits` rule of `ContextualCheckBlockHeader`.
//!
//! Core's `arith_uint256` operators map onto `Arith256` as CS2 §3.7 lists. Two of them need care:
//! `*=` with an `int64_t` resolves to `operator*=(uint32_t)`, so the factor is truncated mod
//! 2^32, and division by an integer resolves to `operator/=(uint64_t)` (`div_u64`). Both are
//! written out where they occur.

mod btc;
mod dgw;
mod kgw;
mod params;

use dash_num::{Arith256, CompactTarget};
use dash_types::Numeric;
pub use params::{PowConfig, PowParams};

use crate::fp;
use crate::{BlockId, ChainAncestry, HeaderCtx};

/// Core's `GetNextWorkRequired`: the `nBits` the block after `last` must carry.
///
/// `new_time` is the candidate block's `nTime`, the only field of `pblock` Core reads.
///
/// Core asserts that the chain has the blocks it walks to. Under [`ChainAncestry`]'s contract
/// that always holds; if an implementation breaks the contract, debug builds panic and release
/// builds return the powLimit compact, Core's answer for a chain too short to retarget
/// (CS2 §3.8).
// The three retargets are `#[inline]`, so they inline here whichever codegen unit rustc puts
// them in. Out of line, or with KGW's loop sharing registers with a BTC call, KGW's loop spills
// more: +0.97% instructions on PF0's chain-accept-kgw-2000 (CH3a D102).
// Core: pow.cpp:170 GetNextWorkRequired (v23.1.8: 167)
pub fn get_next_work_required(
    chain: &dyn ChainAncestry,
    last: BlockId,
    new_time: u32,
    p: &PowParams,
) -> CompactTarget {
    let c = &p.config;
    let tip = chain.header(last);
    let height = i64::from(tip.height);

    // Core: pow.cpp:177 (v23.1.8: 174); only devnets set nMinimumDifficultyBlocks
    if height < i64::from(c.minimum_difficulty_blocks) {
        return p.pow_limit_bits;
    }
    if height + 1 < i64::from(c.kgw_height) {
        return btc::next_work(chain, last, new_time, p);
    }
    // Core: pow.cpp:187 (v23.1.8: 184). BTC has its own min-difficulty rule, so this one only
    // follows it.
    if c.no_retargeting {
        return p.pow_limit_bits;
    }
    if c.allow_min_difficulty_blocks {
        let (new_time, last_time) = (i64::from(new_time), i64::from(tip.time));
        // Core: pow.cpp:193 (v23.1.8: 190), the last block is more than 2 hours old
        if new_time > last_time + 2 * 60 * 60 {
            return p.pow_limit_bits;
        }
        // Core: pow.cpp:197 (v23.1.8: 194), the last block is more than 4 spacings old
        if new_time > last_time + c.target_spacing * 4 {
            // `* 10` is `operator*(uint32_t)`, which wraps mod 2^256 (quirk 8).
            let target = tip.bits.expand().value * 10u32;
            if target > c.pow_limit {
                return p.pow_limit_bits;
            }
            return target.compact(false);
        }
    }
    if height + 1 < i64::from(c.dgw_height) {
        return kgw::next_work(chain, last, p);
    }
    dgw::next_work(chain, last, p)
}

/// Core's `CheckProofOfWork`: `bits` is a positive, non-overflowing target no easier than
/// powLimit, and `hash` (internal byte order) does not exceed it.
// Core: pow.cpp:239 CheckProofOfWork (v23.1.8: 236)
pub fn check_proof_of_work(hash: &[u8; 32], bits: CompactTarget, p: &PowParams) -> bool {
    let target = bits.expand();
    if target.negative
        || target.value == Arith256::ZERO
        || target.overflow
        || target.value > p.config.pow_limit
    {
        return false;
    }
    Arith256::from_lendian(*hash) <= target.value
}

/// Core's `GetBlockProof`: the work a block with `bits` contributes, `2^256 / (target + 1)`; 0
/// for a negative, overflowing or zero target.
///
/// `2^256` does not fit in 256 bits, so Core computes `~target / (target + 1) + 1`, which is
/// equal because `2^256 >= target + 1`. No `nBits` expands to `2^256 - 1`, so `target + 1` never
/// wraps to zero.
// Core: chain.cpp:132-145 GetBlockProof
pub fn block_proof(bits: CompactTarget) -> Arith256 {
    let target = bits.expand();
    if target.negative || target.overflow || target.value == Arith256::ZERO {
        return Arith256::ZERO;
    }
    (!target.value / (target.value + Arith256::ONE)) + Arith256::ONE
}

/// Core's `GetBlockProofEquivalentTime(to, from, tip, params)`: how many seconds of blocks at
/// `tip`'s difficulty, one per `nPowTargetSpacing`, the chain work between `from` and `to` is
/// worth. Positive when `to` has more work than `from`, else negative (or 0 when they are equal).
///
/// Core's arithmetic, verbatim: the work difference times `arith_uint256(nPowTargetSpacing)` is a
/// full 256-bit product that wraps mod 2^256, the division truncates, and a quotient wider than
/// 63 bits clamps to `±i64::MAX` (never `i64::MIN`).
///
/// `ConnectBlock`'s assumevalid decision calls it with the best header as `to` and `tip`
/// (`validation.cpp:2569`; [`crate::connect::script_checks`]).
///
/// # Panics
///
/// When `tip`'s `nBits` has no work ([`block_proof`] is 0: a negative, overflowing or zero
/// target), where Core's division throws `uint_error`. No header that passed
/// [`check_proof_of_work`] has such bits.
// Core: chain.cpp:147-160 GetBlockProofEquivalentTime
pub fn block_proof_equivalent_time(
    to: &HeaderCtx,
    from: &HeaderCtx,
    tip: &HeaderCtx,
    p: &PowParams,
) -> i64 {
    let (r, sign) = if to.chain_work > from.chain_work {
        (to.chain_work - from.chain_work, 1)
    } else {
        (from.chain_work - to.chain_work, -1)
    };
    // `arith_uint256(params.nPowTargetSpacing)`: the `uint64_t` constructor; PowParams keeps the
    // spacing positive.
    let spacing = Arith256::from_u64(p.config.target_spacing as u64);
    let r = (r * spacing)
        .checked_div(block_proof(tip.bits))
        .expect("GetBlockProof(tip) is 0: Core's division throws uint_error");
    if r.bits() > 63 {
        return sign * i64::MAX;
    }
    sign * r.low_u64() as i64
}

/// The `nBits` rule of Core's `ContextualCheckBlockHeader` for the block at `height`: its
/// `header_bits` must equal `expected`, the result of [`get_next_work_required`].
///
/// On mainnet at heights up to `dash-params`' tolerance pin (68589) Core only requires the two to
/// be within 50% of each other in `ConvertBitsToDouble` terms
/// ([`fp::within_pre_dgw_tolerance`]). CS3 owns the reject (`bad-diffbits`).
// Core: validation.cpp:4476-4489 ContextualCheckBlockHeader (v23.1.8: 4110-4123)
pub fn nbits_acceptable(
    height: u32,
    header_bits: CompactTarget,
    expected: CompactTarget,
    p: &PowParams,
) -> bool {
    if p.config.tolerance_applies(height) {
        return fp::within_pre_dgw_tolerance(header_bits.to_base(), expected.to_base());
    }
    header_bits == expected
}

/// Core's `operator/=(uint64_t)` and `operator/(const base_uint&, uint64_t)`, which C++ picks for
/// a division by any integer (CS2 §3.7). Like Core, a divisor that fits 32 bits (every one KGW
/// and DGW use at Core's parameters) is divided limb by limb over 32-bit limbs with 64-bit
/// division; above that Core falls back to its general division (live for DGW's `/=
/// nTargetTimespan` on a devnet spacing of 178,956,971 or more), and this to long division over
/// 64-bit limbs with 128-bit division. All are exact floor division, the same quotient as
/// dash-num's general 256-bit division at a fraction of its cost: KGW divides once per block of
/// its window, DGW 24 times per header. Core throws on a zero divisor; no caller passes one, and
/// this returns 0 like dash-num.
// Core: arith_uint256.cpp:77 (v23.1.8: 84)
fn div_u64(a: Arith256, d: u64) -> Arith256 {
    if d == 0 {
        return Arith256::ZERO;
    }
    let le = a.to_lendian();
    let mut quotient = [0u8; 32];
    if let Ok(d) = u32::try_from(d) {
        let d = u64::from(d);
        let mut rem: u64 = 0;
        for limb in (0..8).rev() {
            let mut word = [0u8; 4];
            word.copy_from_slice(&le[limb * 4..limb * 4 + 4]);
            let cur = (rem << 32) | u64::from(u32::from_le_bytes(word));
            // `rem < d`, so `cur / d` fits 32 bits
            quotient[limb * 4..limb * 4 + 4].copy_from_slice(&((cur / d) as u32).to_le_bytes());
            rem = cur % d;
        }
    } else {
        let d = u128::from(d);
        let mut rem: u128 = 0;
        for limb in (0..4).rev() {
            let mut word = [0u8; 8];
            word.copy_from_slice(&le[limb * 8..limb * 8 + 8]);
            let cur = (rem << 64) | u128::from(u64::from_le_bytes(word));
            // `rem < d`, so `cur / d` fits 64 bits
            quotient[limb * 8..limb * 8 + 8].copy_from_slice(&((cur / d) as u64).to_le_bytes());
            rem = cur % d;
        }
    }
    Arith256::from_lendian(quotient)
}

/// Where Core asserts on the chain it walks (a missing `pprev` or `GetAncestor`): unreachable
/// under [`ChainAncestry`]'s contract (CS2 §3.8).
#[cold]
fn broken_chain(p: &PowParams) -> CompactTarget {
    debug_assert!(false, "ChainAncestry broke its contract: a block Core walks to is missing");
    p.pow_limit_bits
}

#[cfg(test)]
mod tests {
    use dash_params::Network;

    use super::*;
    use crate::params::test_support::{chain, header_ctx};

    #[test]
    fn block_proof_edges() {
        // Negative, overflowing and zero targets contribute no work (chain.cpp:137-138).
        for bits in [0x0000_0000, 0x0180_0001, 0x2301_0000, 0x0100_0000, 0x2200_0100] {
            assert_eq!(block_proof(CompactTarget::new(bits)), Arith256::ZERO, "{bits:#010x}");
        }
        // Mainnet genesis (bits 0x1e0ffff0): `getblockheader` reports chainwork 0x100010.
        assert_eq!(block_proof(CompactTarget::new(0x1e0f_fff0)), Arith256::from_u64(0x10_0010));
        // A target of 1 contributes 2^256 / 2.
        assert_eq!(block_proof(CompactTarget::new(0x0101_0000)), Arith256::ONE.wrapping_shl(255));
    }

    /// A header with `work` and `bits`, the two fields `block_proof_equivalent_time` reads.
    fn worked(work: Arith256, bits: u32) -> HeaderCtx {
        HeaderCtx {
            chain_work: work,
            ..header_ctx(0, 0, bits)
        }
    }

    /// Bits whose block proof is 2 (regtest's powLimit, a target just under 2^255) and
    /// 0x100010 (mainnet genesis).
    const PROOF_2: u32 = 0x207f_ffff;
    const PROOF_GENESIS: u32 = 0x1e0f_fff0;

    /// `chain.cpp:147-160` by hand: (difference × 150) / proof, signed by direction, truncated
    /// toward zero before the sign is applied.
    #[test]
    fn block_proof_equivalent_time_by_hand() {
        let p = PowParams::from_chain_params(&chain(Network::Main)).unwrap();
        assert_eq!(p.config().target_spacing, 150);
        assert_eq!(block_proof(CompactTarget::new(PROOF_2)), Arith256::from_u64(2));
        let w = Arith256::from_u64;
        let time = |to: u64, from: u64, bits: u32| {
            let tip = worked(w(0), bits);
            block_proof_equivalent_time(&worked(w(to), bits), &worked(w(from), bits), &tip, &p)
        };
        assert_eq!(time(1_000, 400, PROOF_2), 600 * 150 / 2);
        assert_eq!(time(400, 1_000, PROOF_2), -45_000);
        // Equal work: Core takes the `else` branch (sign -1) and returns -0 = 0.
        assert_eq!(time(7, 7, PROOF_2), 0);
        // 10^7 × 150 / 1,048,592 = 1430.5: truncated, and the sign applied after.
        assert_eq!(time(10_000_000, 0, PROOF_GENESIS), 1_430);
        assert_eq!(time(0, 10_000_000, PROOF_GENESIS), -1_430);
        // 8064 blocks at the tip's difficulty are exactly two weeks; one more is past them.
        let g = 0x10_0010;
        assert_eq!(time(8_064 * g, 0, PROOF_GENESIS), 1_209_600);
        assert_eq!(time(8_065 * g, 0, PROOF_GENESIS), 1_209_750);
        // The tip's bits are the divisor, not `to`'s or `from`'s.
        let tip = worked(w(0), PROOF_GENESIS);
        let (to, from) = (worked(w(8_064 * g), PROOF_2), worked(w(0), PROOF_2));
        assert_eq!(block_proof_equivalent_time(&to, &from, &tip, &p), 1_209_600);
    }

    /// The clamp at 63 bits, its sign, and the 256-bit product's wrap.
    #[test]
    fn block_proof_equivalent_time_clamps_and_wraps() {
        let p = PowParams::from_chain_params(&chain(Network::Main)).unwrap();
        let tip = worked(Arith256::ZERO, PROOF_2);
        let time = |to: Arith256, from: Arith256| {
            block_proof_equivalent_time(&worked(to, PROOF_2), &worked(from, PROOF_2), &tip, &p)
        };
        // r = difference × 75 here. The largest difference whose r fits 63 bits is returned
        // as is; one more clamps to i64::MAX, and to -i64::MAX the other way.
        let fits = (i64::MAX / 75) as u64;
        let (zero, w) = (Arith256::ZERO, Arith256::from_u64);
        assert_eq!(time(w(fits), zero), 9_223_372_036_854_775_800);
        assert_eq!(time(zero, w(fits)), -9_223_372_036_854_775_800);
        assert_eq!(time(w(fits + 1), zero), i64::MAX);
        assert_eq!(time(zero, w(fits + 1)), -i64::MAX);
        assert_eq!(time(Arith256::MAX, zero), i64::MAX);
        // 2^255 × 150 = 2^256 × 75 wraps to 0, so a difference that would clamp gives 0.
        let half = Arith256::ONE.wrapping_shl(255);
        assert_eq!(time(half, zero), 0);
        assert_eq!(time(zero, half), 0);
    }

    #[test]
    #[should_panic(expected = "GetBlockProof(tip) is 0")]
    fn block_proof_equivalent_time_of_a_tip_without_work_panics() {
        let p = PowParams::from_chain_params(&chain(Network::Main)).unwrap();
        let (a, b) = (worked(Arith256::from_u64(5), 0), worked(Arith256::ZERO, 0));
        block_proof_equivalent_time(&a, &b, &b, &p);
    }

    /// A chain that breaks `ChainAncestry`'s contract: no block has a `prev` or an ancestor.
    struct Orphans(Vec<HeaderCtx>);

    impl ChainAncestry for Orphans {
        fn header(&self, id: BlockId) -> HeaderCtx {
            self.0[id.0 as usize]
        }
        fn ancestor(&self, _id: BlockId, _height: u32) -> Option<BlockId> {
            None
        }
        fn prev(&self, _id: BlockId) -> Option<BlockId> {
            None
        }
    }

    fn orphan_at(height: u32) -> Orphans {
        Orphans(vec![header_ctx(height, 1_700_000_000, 0x1b04_04cb)])
    }

    /// The heights where Core walks the chain: DGW's `pprev` and the BTC retarget's
    /// `GetAncestor` (576 is mainnet's first retarget).
    const WALKS: [u32; 2] = [2_000_000, 575];

    /// Where Core would assert, debug builds panic (CS2 §3.8).
    #[test]
    #[cfg(debug_assertions)]
    fn broken_chain_panics_in_debug() {
        let p = PowParams::from_chain_params(&chain(Network::Main)).unwrap();
        for height in WALKS {
            let chain = orphan_at(height);
            let walk = std::panic::catch_unwind(|| {
                get_next_work_required(&chain, BlockId(0), 1_700_000_150, &p)
            });
            let message = walk.expect_err("no panic").downcast::<&str>().unwrap();
            assert!(message.contains("ChainAncestry broke its contract"), "{height}: {message}");
        }
    }

    /// Release builds return the powLimit compact instead.
    #[test]
    #[cfg(not(debug_assertions))]
    fn broken_chain_is_pow_limit_in_release() {
        let p = PowParams::from_chain_params(&chain(Network::Main)).unwrap();
        for height in WALKS {
            let got = get_next_work_required(&orphan_at(height), BlockId(0), 1_700_000_150, &p);
            assert_eq!(got, p.pow_limit_bits(), "{height}");
        }
    }

    /// The limb division equals dash-num's general division.
    #[test]
    fn div_u64_matches_arith256_division() {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..20_000 {
            let mut be = [0u8; 32];
            for chunk in be.chunks_mut(8) {
                chunk.copy_from_slice(&next().to_be_bytes());
            }
            let a = Arith256::from_bendian(be) >> (next() % 256) as u32;
            let around_u32 = [u64::from(u32::MAX) - 1, u64::from(u32::MAX), 1 << 32];
            for d in [1, 2, 3, 24, 25, 3600, 86_400, next() >> (next() % 64), u64::MAX, next()]
                .into_iter()
                .chain(around_u32)
            {
                assert_eq!(div_u64(a, d), a / d, "{a:?} / {d}");
            }
        }
        assert_eq!(div_u64(Arith256::MAX, 0), Arith256::ZERO);
    }

    /// `nbits_acceptable` is exact everywhere but mainnet at or below 68589, where nearby bits
    /// pass (Core's own evidence for the boundary is quirk 9's test).
    #[test]
    fn nbits_acceptable_branches() {
        let (a, b) = (CompactTarget::new(0x1b04_04cb), CompactTarget::new(0x1b04_04cc));
        let main = PowParams::from_chain_params(&chain(Network::Main)).unwrap();
        let test = PowParams::from_chain_params(&chain(Network::Testnet3)).unwrap();
        assert!(nbits_acceptable(68_589, a, b, &main));
        assert!(nbits_acceptable(68_589, a, a, &main));
        assert!(!nbits_acceptable(68_590, a, b, &main));
        assert!(nbits_acceptable(68_590, a, a, &main));
        assert!(!nbits_acceptable(1, a, b, &test));
        assert!(nbits_acceptable(1, a, a, &test));
    }
}
