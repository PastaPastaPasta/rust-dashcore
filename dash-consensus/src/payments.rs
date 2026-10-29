//! Masternode payments (CS2 §5): the share of the block value that goes to masternodes, the
//! platform's part of it, and the arithmetic that splits a payee's reward between its operator,
//! owners and collateral shares.
//!
//! Selecting the payee and assembling the coinbase outputs is dash-evo's (EV); it calls these.
//!
//! Which products can leave `int64` (CS2 §5, its bounds table): the block value is a subsidy plus
//! up to `MAX_MONEY` of fees, and a devnet's high-subsidy factor can make the subsidy far larger.
//! - [`masternode_payment`]'s `block_value * permille` and `* 3` cannot overflow on any
//!   configuration Core can run.
//! - [`platform_share`]'s `* 375` can, on a devnet with a factor of about 6.6×10⁷ or more.
//! - [`operator_reward`]'s and [`owner_payout_amounts`]' `reward * bps` can for a reward above
//!   about 9.22×10¹⁴ duffs: a devnet factor of about 2.5×10⁶, and on every network a block whose
//!   fees are near the whole money supply, which `MoneyRange(fees)` still admits.
//!
//! Where a product leaves `int64`, Core's C++ is undefined, and these functions compute what the
//! release binaries do: a wrapping 64-bit multiplication followed by a division that truncates
//! towards zero. That arithmetic is required, not defensive: checked or widened multiplication
//! would give different answers.

use core::fmt;

use dash_params::ChainParams;

use crate::params::{ParamsError, Superblocks, height};
use crate::{Amount, money_range};

/// Which masternode reward rules apply to a block (Core's `MnRewardEra`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MnRewardEra {
    /// Before V20: the reallocation schedule.
    Classic,
    /// V20 without MN_RR: 75% of the block value, no platform share.
    CreditPool,
    /// MN_RR: 75% of the block value, of which [`platform_share`] goes to the credit pool.
    EvoReward,
}

impl MnRewardEra {
    /// Core's `GetMnRewardEraAfter`, from the deployment states after the previous block.
    // Core: validation.cpp:5897 GetMnRewardEraAfter
    pub fn after(v20_active_after: bool, mn_rr_active_after: bool) -> Self {
        if !v20_active_after {
            Self::Classic
        } else if !mn_rr_active_after {
            Self::CreditPool
        } else {
            Self::EvoReward
        }
    }
}

/// The fields of Core's chain parameters the payment schedule reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaymentConfig {
    /// `nMasternodePaymentsIncreaseBlock`.
    pub increase_block: i32,
    /// `nMasternodePaymentsIncreasePeriod`.
    pub increase_period: i32,
    /// `BRRHeight` (regtest `-testactivationheight=brr@<h>`).
    pub brr_height: i32,
    /// `nSuperblockStartBlock` and `nSuperblockCycle`: the cycle sets where the reallocation
    /// starts and how long its periods are.
    pub superblocks: Superblocks,
}

/// Validated payment parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaymentParams {
    config: PaymentConfig,
}

impl PaymentParams {
    /// The parameters of `chain` (CS2 §3.1). No devnet argument affects them.
    pub fn from_chain_params(chain: &ChainParams) -> Result<Self, ParamsError> {
        let c = &chain.consensus;
        Ok(Self {
            config: PaymentConfig {
                increase_block: height(c.masternode_payments_increase_block)?,
                increase_period: c.masternode_payments_increase_period,
                brr_height: height(c.brr_height)?,
                superblocks: Superblocks::from_chain_params(chain)?,
            },
        })
    }

    /// The validated values.
    pub fn config(&self) -> &PaymentConfig {
        &self.config
    }
}

/// Core has no result for these inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaymentError {
    /// `PlatformShare` of a reward whose share leaves `MoneyRange`: Core asserts.
    PlatformShareRange,
    /// `SplitAmountByShares` over several shares whose amounts sum to 0: Core's
    /// `arith_uint256` division throws. Validated share lists never do.
    ZeroShareTotal,
}

impl fmt::Display for PaymentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PlatformShareRange => "PlatformShare out of MoneyRange (Core asserts)",
            Self::ZeroShareTotal => "share amounts sum to 0 (Core throws)",
        })
    }
}

impl std::error::Error for PaymentError {}

/// The reallocation schedule, in tenths of a percent of the block value, one entry per period
/// of three superblock cycles (Core's `vecPeriods`).
const REALLOCATION_PERIODS: [Amount; 19] =
    [513, 526, 533, 540, 546, 552, 557, 562, 567, 572, 577, 582, 585, 588, 591, 594, 597, 599, 600];

/// `vecPeriods.size() - 1`: the schedule stays at its last entry.
const LAST_PERIOD: i64 = REALLOCATION_PERIODS.len() as i64 - 1;

/// Core's `GetMasternodePayment`: the masternodes' share of `block_value` (subsidy plus fees)
/// at `height`.
///
/// Every step truncates on its own, so the "50%" stage is not `block_value / 2`, and the 8th
/// increase period is skipped (quirk 12).
// Core: masternode/payments.cpp:62 GetMasternodePayment
pub fn masternode_payment(
    height: i32,
    block_value: Amount,
    era: MnRewardEra,
    p: &PaymentParams,
) -> Amount {
    let c = &p.config;
    let height = i64::from(height);
    let increase_block = i64::from(c.increase_block);
    let increase_period = i64::from(c.increase_period);

    let mut ret = block_value / 5; // start at 20%
    for k in [0, 1, 2] {
        if height > increase_block + increase_period * k {
            ret += block_value / 20;
        }
    }
    for k in [3, 4, 5, 6, 7, 9] {
        if height > increase_block + increase_period * k {
            ret += block_value / 40;
        }
    }

    let brr_height = i64::from(c.brr_height);
    if height < brr_height {
        // Block reward reallocation is not active yet.
        return ret;
    }
    let superblock_cycle = i64::from(c.superblocks.cycle());
    // The reallocation starts in the cycle after the one it activates in.
    let realloc_start = brr_height - brr_height % superblock_cycle + superblock_cycle;
    if height < realloc_start {
        return ret;
    }

    if era != MnRewardEra::Classic {
        // From V20 the treasury takes 20%, and 75% of the remaining 80% is the masternodes'
        // 60% of the subsidy. The earlier periods no longer apply.
        return block_value.wrapping_mul(3) / 4;
    }

    let realloc_cycle = superblock_cycle * 3;
    // `height >= realloc_start`, so this is in 0..=LAST_PERIOD.
    let period = ((height - realloc_start) / realloc_cycle).min(LAST_PERIOD);
    let permille = REALLOCATION_PERIODS[period as usize];
    block_value.wrapping_mul(permille) / 1000
}

/// Core's `PlatformShare`: the part of the masternode reward that goes to the platform's credit
/// pool in the [`MnRewardEra::EvoReward`] era, 37.5% rounded towards zero, or an error where
/// Core's `assert(MoneyRange(...))` fails.
///
/// Above about 2.46×10¹⁶ duffs (a devnet high-subsidy factor of roughly 6.6×10⁷ or more) the
/// `* 375` leaves `int64`. The release binaries wrap it and carry on whenever the quotient lands
/// inside `MoneyRange` (CS2 §5), so this wraps before the check too.
// Core: masternode/payments.cpp:54 PlatformShare (v23.1.8: 25)
pub fn platform_share(mn_reward: Amount) -> Result<Amount, PaymentError> {
    let share = mn_reward.wrapping_mul(375) / 1000;
    if !money_range(share) {
        return Err(PaymentError::PlatformShareRange);
    }
    Ok(share)
}

/// The operator's part of a masternode reward, `operator_bps` basis points of it rounded towards
/// zero. Core pays it only when the payee has a non-zero operator reward and an operator payout
/// script; the caller checks that.
// Core: masternode/payments.cpp:167 (v23.1.8: 70)
pub fn operator_reward(mn_reward: Amount, operator_bps: u16) -> Amount {
    mn_reward.wrapping_mul(Amount::from(operator_bps)) / 10_000
}

/// The owner payouts of a non-shared masternode (v24 payout lists): each entry but the last gets
/// `reward * payout_bps[i] / 10000` rounded towards zero, and the last gets the remainder.
// Core: masternode/payments.cpp:184-191 (v24 payout lists)
pub fn owner_payout_amounts(reward: Amount, payout_bps: &[u16]) -> Vec<Amount> {
    // MasternodePayoutShare::MAX_REWARD
    const MAX_REWARD: Amount = 10_000;
    let mut paid: Amount = 0;
    let mut amounts = Vec::with_capacity(payout_bps.len());
    for (i, &bps) in payout_bps.iter().enumerate() {
        let amount = if i + 1 == payout_bps.len() {
            reward.wrapping_sub(paid)
        } else {
            reward.wrapping_mul(Amount::from(bps)) / MAX_REWARD
        };
        paid = paid.wrapping_add(amount);
        amounts.push(amount);
    }
    amounts
}

/// Core's `SplitAmountByShares`: `total` split pro rata over a shared masternode's collateral
/// `shares`. Every entry but the last gets `floor(total * share / sum)` and the last the
/// remainder, so the split sums to `total` (DIP-0026).
// Core: evo/providertx_util.cpp:97 SplitAmountByShares
pub fn split_amount_by_shares(
    total: Amount,
    shares: &[Amount],
) -> Result<Vec<Amount>, PaymentError> {
    let share_total = shares.iter().fold(0, |sum: Amount, &share| sum.wrapping_add(share));
    let mut paid: Amount = 0;
    let mut amounts = Vec::with_capacity(shares.len());
    for (i, &share) in shares.iter().enumerate() {
        let amount = if i + 1 == shares.len() {
            total.wrapping_sub(paid)
        } else {
            pro_rata_floor(total, share, share_total)?
        };
        paid = paid.wrapping_add(amount);
        amounts.push(amount);
    }
    Ok(amounts)
}

/// Core's `ProRataFloor`: `total * weight / weight_total` over the bit patterns as unsigned
/// 64-bit values, truncated to the low 64 bits. The product of two 64-bit values fits 128 bits,
/// so `u128` computes exactly what Core's `arith_uint256` does.
// Core: evo/providertx_util.cpp:88 ProRataFloor
fn pro_rata_floor(
    total: Amount,
    weight: Amount,
    weight_total: Amount,
) -> Result<Amount, PaymentError> {
    let product = u128::from(total as u64) * u128::from(weight as u64);
    let quotient =
        product.checked_div(u128::from(weight_total as u64)).ok_or(PaymentError::ZeroShareTotal)?;
    // GetLow64(), then the static_cast to CAmount.
    Ok(quotient as u64 as Amount)
}

#[cfg(test)]
mod tests {
    use dash_params::NetworkKind;

    use super::*;
    use crate::params::test_support::chain;
    use crate::subsidy::COIN;

    // Core: validation.cpp:5897 GetMnRewardEraAfter
    #[test]
    fn era_follows_v20_then_mn_rr() {
        assert_eq!(MnRewardEra::after(false, false), MnRewardEra::Classic);
        assert_eq!(MnRewardEra::after(false, true), MnRewardEra::Classic);
        assert_eq!(MnRewardEra::after(true, false), MnRewardEra::CreditPool);
        assert_eq!(MnRewardEra::after(true, true), MnRewardEra::EvoReward);
    }

    /// Core's `evo_sharedmn_tests/split_amount_by_shares`, ported with its assertions.
    // Core: src/test/evo_sharedmn_tests.cpp:522 (v24.0.0-rc.3)
    #[test]
    fn core_split_amount_by_shares() {
        let shares = [600 * COIN, 300 * COIN, 100 * COIN];

        // Conservation and remainder-to-last for an amount that doesn't divide evenly
        let total = 123_456_789;
        let amounts = split_amount_by_shares(total, &shares).unwrap();
        assert_eq!(amounts.len(), shares.len());
        assert_eq!(amounts[0] + amounts[1] + amounts[2], total);
        assert_eq!(amounts[0], total * 6 / 10);
        assert_eq!(amounts[1], total * 3 / 10);
        assert_eq!(amounts[2], total - amounts[0] - amounts[1]);

        // No 64-bit overflow for realistic values: reward in duffs times a 1000 DASH share
        let big = split_amount_by_shares(566 * COIN, &shares).unwrap();
        assert_eq!(big[0] + big[1] + big[2], 566 * COIN);
        assert_eq!(big[0], 566 * COIN * 6 / 10);
    }

    /// `value * factor / divisor` as the release binaries compute it: a 64-bit wrapping product
    /// (`imul`/`mul`), then a signed division truncating towards zero (the listings in CS2 §5).
    /// Modelled through i128, independently of `wrapping_mul`.
    ///
    /// These are the only expectations in CS2 not produced by Core. The overflowing inputs are
    /// in Core's domain (CS2 §5: devnet factors, and fees near the money supply), but Core's C++
    /// is undefined there and the oracle refuses them, so the reference is the release binaries'
    /// arithmetic (CS2 §6.2).
    fn binaries(value: Amount, factor: i64, divisor: i64) -> Amount {
        (i128::from(value) * i128::from(factor)) as i64 / divisor
    }

    fn overflows(value: Amount, factor: i64) -> bool {
        i64::try_from(i128::from(value) * i128::from(factor)).is_err()
    }

    /// `PlatformShare` around the `* 375` overflow: the binaries wrap, then keep any quotient
    /// inside `MoneyRange` (CS2 §5).
    #[test]
    fn platform_share_wraps_like_the_binaries() {
        let last_exact = i64::MAX / 375;
        // the smallest reward whose wrapped product is non-negative again: 2^64 / 375, rounded up
        let wraps_back = 49_191_317_529_892_138;
        assert!(overflows(wraps_back, 375) && !overflows(last_exact, 375));
        let mut accepted_after_wrap = 0;
        for reward in [last_exact - 1, last_exact, last_exact + 1, last_exact + 2]
            .into_iter()
            .chain([wraps_back - 1, wraps_back, wraps_back + 1, wraps_back + 1_000_000])
            .chain([i64::MIN / 375, i64::MIN / 375 - 1, i64::MAX, i64::MIN])
        {
            let share = binaries(reward, 375, 1000);
            let expected =
                money_range(share).then_some(share).ok_or(PaymentError::PlatformShareRange);
            assert_eq!(platform_share(reward), expected, "{reward}");
            accepted_after_wrap += usize::from(overflows(reward, 375) && expected.is_ok());
        }
        // the cases a checked multiplication would wrongly refuse
        assert!(accepted_after_wrap >= 3, "{accepted_after_wrap}");
    }

    /// The operator's and owners' parts at the multiplication edges (CS2 §6.2): bps 0, 1, 9999
    /// and 10000, and rewards at `i64::MAX / bps` and one above. Rewards above the edge are
    /// admissible (CS2 §5), and the expected values are the release binaries' wrapping results.
    #[test]
    fn operator_and_owner_parts_at_the_edges() {
        for bps in [0u16, 1, 9_999, 10_000] {
            let mut rewards = vec![0, 1, 1_234_567_891, -1_234_567_891];
            if bps != 0 {
                let edge = i64::MAX / i64::from(bps);
                // bps 1: the edge is i64::MAX itself, with nothing above
                rewards.extend([edge, -edge, -edge - 1].into_iter().chain(edge.checked_add(1)));
            }
            for reward in rewards {
                let expected = binaries(reward, i64::from(bps), 10_000);
                assert_eq!(operator_reward(reward, bps), expected, "{reward} {bps}");
            }
        }

        // one owner takes everything; several split with the remainder going to the last
        assert_eq!(owner_payout_amounts(1_000_000_007, &[10_000]), vec![1_000_000_007]);
        for reward in [1_000_000_007, i64::MAX / 3_333, i64::MAX / 3_333 + 1] {
            let first = binaries(reward, 3_333, 10_000);
            let last = reward.wrapping_sub(first.wrapping_mul(2));
            assert_eq!(
                owner_payout_amounts(reward, &[3_333, 3_333, 3_334]),
                vec![first, first, last]
            );
        }
    }

    /// Where Core's `arith_uint256` division would throw.
    #[test]
    fn zero_share_total() {
        assert_eq!(split_amount_by_shares(100, &[0, 0]), Err(PaymentError::ZeroShareTotal));
        assert_eq!(split_amount_by_shares(100, &[5, -5]), Err(PaymentError::ZeroShareTotal));
        // a single share takes the total without dividing; no shares, no amounts
        assert_eq!(split_amount_by_shares(100, &[0]), Ok(vec![100]));
        assert_eq!(split_amount_by_shares(100, &[]), Ok(vec![]));
    }

    #[test]
    fn rejects_what_core_cannot_run() {
        let mut c = chain(NetworkKind::Mainnet);
        c.consensus.superblock_cycle = 0;
        assert_eq!(PaymentParams::from_chain_params(&c), Err(ParamsError::SuperblockCycle));
        let mut c = chain(NetworkKind::Mainnet);
        c.consensus.brr_height = (1u32 << 31).into();
        assert_eq!(PaymentParams::from_chain_params(&c), Err(ParamsError::Height));
    }
}
