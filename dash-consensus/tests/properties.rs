//! Properties over arbitrary inputs: nothing derived from input panics (with debug assertions,
//! which check `fp`'s domain claims), and the payout splits keep their remainder-to-last
//! structure (DIP-0026), so they sum to their total.

mod common;

use common::{VecChain, header_ctx};
use dash_consensus::payments::{
    MnRewardEra, PaymentParams, masternode_payment, operator_reward, owner_payout_amounts,
    platform_share, split_amount_by_shares,
};
use dash_consensus::pow::{
    PowParams, check_proof_of_work, get_next_work_required, nbits_acceptable,
};
use dash_consensus::subsidy::{
    SubsidyParams, block_subsidy, block_subsidy_helper, superblock_payments_limit,
};
use dash_consensus::{BlockId, ChainAncestry};
use dash_num::{Arith256, CompactTarget};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_types::Numeric;
use proptest::prelude::*;

/// Core's four networks: the views take the mainnet tolerance, the subsidy's network paths and
/// whether the devnet arguments are allowed from them.
fn network() -> impl Strategy<Value = NetworkKind> {
    prop::sample::select(NetworkKind::ALL.to_vec())
}

/// Mainnet's parameters as `network`, with that network's hard-coded pins and, on a devnet,
/// the devnet arguments (Core's defaults elsewhere, as anything else is a parameter error).
fn chain_as(
    network: NetworkKind,
    minimum_difficulty_blocks: i32,
    high_subsidy: (i32, i32),
) -> ChainParams {
    let build = |n| ChainParamsBuilder::new(n, Release::V24_0).build().expect("Core's parameters");
    let mut chain = build(NetworkKind::Mainnet);
    chain.network = network;
    chain.pins = build(network).pins;
    if network == NetworkKind::Devnet {
        let c = &mut chain.consensus;
        c.minimum_difficulty_blocks = minimum_difficulty_blocks;
        (c.high_subsidy_blocks, c.high_subsidy_factor) = high_subsidy;
    }
    chain
}

/// Arbitrary proof-of-work chain parameters, mostly ones Core can run with.
fn pow_chain() -> impl Strategy<Value = ChainParams> {
    (
        any::<[u8; 32]>(),
        prop_oneof![1i64..=600, Just(86_400i64), 1i64..=200_000, Just(i64::MAX / 72)],
        prop_oneof![Just(86_400i64), 1i64..=1_000_000],
        0u32..=700,
        0u32..=900,
        any::<bool>(),
        any::<bool>(),
        -5i32..=700,
        network(),
    )
        .prop_map(
            |(limit, spacing, timespan, kgw, dgw, min_diff, no_retarget, mdb, network)| {
                let mut chain = chain_as(network, mdb, (0, 1));
                let c = &mut chain.consensus;
                c.pow_limit = Arith256::from_bendian(limit);
                c.pow_target_spacing = spacing;
                c.pow_target_timespan = timespan;
                c.pow_kgw_height = kgw.into();
                c.pow_dgw_height = dgw.into();
                c.pow_allow_min_difficulty_blocks = min_diff;
                c.pow_no_retargeting = no_retarget;
                chain
            },
        )
}

/// `(nTime, nBits)` for a chain from genesis (so `ChainAncestry`'s contract holds), some of the
/// bits repeating the powLimit compact so the min-difficulty walk runs.
fn time_bits(pow_limit_bits: u32) -> impl Strategy<Value = Vec<(u32, u32)>> {
    prop::collection::vec(
        (any::<u32>(), prop_oneof![any::<u32>(), Just(pow_limit_bits), Just(0x1d00_ffffu32)]),
        1..700,
    )
}

/// The sum Core's `CAmount` additions give in the release binaries.
fn wrapping_sum(amounts: &[i64]) -> i64 {
    amounts.iter().fold(0, |sum, a| sum.wrapping_add(*a))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// GetNextWorkRequired never panics, on any of its paths (KGW over arbitrary windows
    /// included), and the `nBits` rule accepts what it returns.
    #[test]
    fn next_work_never_panics(
        (chain, headers) in pow_chain().prop_flat_map(|c| {
            let limit_bits = c.consensus.pow_limit.compact(false).to_base();
            (Just(c), time_bits(limit_bits))
        }),
        new_time in any::<u32>(),
    ) {
        let Ok(p) = PowParams::from_chain_params(&chain) else { return Ok(()) };
        let chain = VecChain::new(0, headers);
        for tip in [chain.tip(), BlockId(chain.tip().0 / 2), BlockId(0)] {
            let bits = get_next_work_required(&chain, tip, new_time, &p);
            prop_assert!(nbits_acceptable(tip.0 + 1, bits, bits, &p));
            let header = chain.header(tip).bits;
            let _ = nbits_acceptable(tip.0 + 1, header, bits, &p);
        }
    }

    #[test]
    fn check_proof_of_work_never_panics(hash in any::<[u8; 32]>(), bits in any::<u32>(), chain in pow_chain()) {
        if let Ok(p) = PowParams::from_chain_params(&chain) {
            let _ = check_proof_of_work(&hash, CompactTarget::new(bits), &p);
        }
    }

    #[test]
    fn subsidy_never_panics(
        bits in any::<u32>(),
        height in any::<i32>(),
        v20 in any::<bool>(),
        interval in prop_oneof![Just(150i32), Just(210_240), 1i32..=1_000_000],
        high_subsidy in (-10i32..=400_000, any::<i32>()),
        budget_start in 0u32..=i32::MAX as u32,
        network in network(),
        min_difficulty in any::<bool>(),
        pow_limit in any::<[u8; 32]>(),
        sb_start in 0u32..=i32::MAX as u32,
        sb_cycle in 1i32..=20_000,
        v20_height in 0u32..=i32::MAX as u32,
        genesis_reward in any::<i64>(),
    ) {
        let mut chain = chain_as(network, 0, high_subsidy);
        let c = &mut chain.consensus;
        c.subsidy_halving_interval = interval;
        c.budget_payments_start_block = budget_start.into();
        c.pow_allow_min_difficulty_blocks = min_difficulty;
        c.pow_limit = Arith256::from_bendian(pow_limit);
        c.superblock_start.0 = sb_start.into();
        c.superblock_cycle = sb_cycle;
        c.v20_height = v20_height.into();
        let p = SubsidyParams::from_chain_params(&chain, genesis_reward).unwrap();
        let _ = block_subsidy_helper(bits, height, v20, &p);
        let _ = block_subsidy(Some(&header_ctx(height as u32, 0, bits)), v20, &p);
        let _ = superblock_payments_limit(height, &p);
    }

    #[test]
    fn payments_never_panic(
        height in any::<i32>(),
        value in any::<i64>(),
        era in prop_oneof![Just(MnRewardEra::Classic), Just(MnRewardEra::CreditPool), Just(MnRewardEra::EvoReward)],
        brr in 0u32..=i32::MAX as u32,
        cycle in 1i32..=20_000,
    ) {
        let mut chain = chain_as(NetworkKind::Mainnet, 0, (0, 1));
        chain.consensus.brr_height = brr.into();
        chain.consensus.superblock_cycle = cycle;
        let p = PaymentParams::from_chain_params(&chain).unwrap();
        let _ = masternode_payment(height, value, era, &p);
        let _ = platform_share(value);
        let _ = operator_reward(value, u16::MAX);
    }

    /// The owner payouts sum to the reward, whatever the shares (the last takes the remainder).
    #[test]
    fn owner_payouts_sum_to_reward(reward in any::<i64>(), bps in prop::collection::vec(any::<u16>(), 1..8)) {
        let amounts = owner_payout_amounts(reward, &bps);
        prop_assert_eq!(amounts.len(), bps.len());
        prop_assert_eq!(wrapping_sum(&amounts), reward);
    }

    /// SplitAmountByShares sums to the total for a non-empty share list, returns nothing for an
    /// empty one, and only an all-zero share sum has no answer (CS2 §5).
    #[test]
    fn shares_sum_to_total(total in any::<i64>(), shares in prop::collection::vec(any::<i64>(), 0..8)) {
        match split_amount_by_shares(total, &shares) {
            Ok(amounts) if shares.is_empty() => prop_assert!(amounts.is_empty()),
            Ok(amounts) => {
                prop_assert_eq!(amounts.len(), shares.len());
                prop_assert_eq!(wrapping_sum(&amounts), total);
            }
            Err(_) => {
                prop_assert!(shares.len() > 1);
                prop_assert_eq!(wrapping_sum(&shares), 0);
            }
        }
    }
}
