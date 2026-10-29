//! The masternode payment a block's coinbase must make (EV3 D2; FR-CON-21): Core's
//! `GetBlockTxOuts`, the outputs owed for the next payee of the parent's list, and
//! `IsTransactionValid`, which resolves `dash_evo::payments`' `Ev3Branch::MasternodePayee`.
//!
//! The payment arithmetic is `dash_consensus::payments`' and the output matching is
//! `dash_evo::payments::find_unmatched_masternode_payment`; this module reads the payee's state.

use std::fmt;

use dash_consensus::payments::{
    MnRewardEra, PaymentError, PaymentParams, masternode_payment, operator_reward,
    owner_payout_amounts, platform_share, split_amount_by_shares,
};
use dash_consensus::{Amount, Tx, money_range};
use dash_evo::payments::find_unmatched_masternode_payment;
use dash_primitives::TxOut;

use crate::list::MnList;
use crate::payee::PayeeRules;
use crate::types::EXT_ADDR;

/// `OP_RETURN`: the platform share's output script (`CScript() << OP_RETURN`).
const OP_RETURN: u8 = 0x6a;

/// The reward a block pays out (Core's `GetBlockTxOuts` arguments besides the parent).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockReward {
    /// The block's height (`pindexPrev->nHeight + 1`).
    pub height: i32,
    /// `GetBlockSubsidy(pindex)`.
    pub block_subsidy: Amount,
    /// The block's fees.
    pub fee_reward: Amount,
    /// `GetMnRewardEraAfter(pindexPrev)`.
    pub era: MnRewardEra,
}

/// Where Core asserts or throws instead of answering: a node fault, never a verdict (EV3 D6).
/// No block on a network Core can run reaches one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PayeeError {
    /// `PlatformShare`'s assert, or `SplitAmountByShares`' division by a zero share total.
    Payment(PaymentError),
    /// `assert(MoneyRange(masternodeReward))` after the platform share is taken out.
    MasternodeRewardRange,
}

impl fmt::Display for PayeeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Payment(e) => e.fmt(f),
            Self::MasternodeRewardRange => {
                f.write_str("the masternode reward is out of MoneyRange (Core asserts)")
            }
        }
    }
}

impl std::error::Error for PayeeError {}

impl From<PaymentError> for PayeeError {
    fn from(e: PaymentError) -> Self {
        Self::Payment(e)
    }
}

/// Core's `GetBlockTxOuts` result: the outputs it built and its return value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockTxOuts {
    /// `voutMasternodePaymentsRet`, in Core's order. Where `computed` is false it holds what Core
    /// pushed before returning: the platform share in the EvoReward era, else nothing.
    pub outputs: Vec<TxOut>,
    /// Core's return: false where the list has masternodes but none is payable (every one is
    /// banned).
    pub computed: bool,
}

/// Core's `GetBlockTxOuts`: the outputs a block must pay on top of `list` (the parent's), in
/// Core's order ([`BlockTxOuts`]).
///
/// 1. In the [`MnRewardEra::EvoReward`] era, the platform share of the subsidy's masternode
///    payment, to `OP_RETURN`; it is taken out of the masternode reward. It comes first, so an
///    empty list still owes it.
/// 2. Nothing more for an empty list, nor, with `computed` false, for one whose masternodes are
///    all unpayable.
/// 3. The payee's owner part: split over its collateral shares (each share's reward script, else
///    its refund script) for a shared masternode, else over its payout list (version 3, ExtAddr)
///    or to its payout script; outputs of 0 are left out.
/// 4. The operator's part, when the payee has a non-zero operator reward and an operator payout
///    script, and the part is positive.
///
/// `rules` are the payee rules after the parent (`GetMNPayee(pindexPrev)`).
// Core: masternode/payments.cpp:131-199 CMNPaymentsProcessor::GetBlockTxOuts
pub fn block_tx_outs(
    list: &MnList,
    rules: PayeeRules,
    reward: &BlockReward,
    p: &PaymentParams,
) -> Result<BlockTxOuts, PayeeError> {
    let mut outs = Vec::new();
    let mut masternode_reward = masternode_payment(
        reward.height,
        reward.block_subsidy.wrapping_add(reward.fee_reward),
        reward.era,
        p,
    );

    // Credit Pool doesn't exist before V20. If any part of reward will re-allocated to credit
    // pool before v20 activation these fund will be just permanently lost. Applicable for
    // devnets, regtest, testnet
    if reward.era == MnRewardEra::EvoReward {
        let subsidy_reward = masternode_payment(reward.height, reward.block_subsidy, reward.era, p);
        let platform_reward = platform_share(subsidy_reward)?;
        masternode_reward = masternode_reward.wrapping_sub(platform_reward);
        if !money_range(masternode_reward) {
            return Err(PayeeError::MasternodeRewardRange);
        }
        outs.push(TxOut {
            value: platform_reward,
            script_pubkey: vec![OP_RETURN].into(),
        });
    }

    let done = |outputs, computed| {
        Ok(BlockTxOuts {
            outputs,
            computed,
        })
    };
    if list.counts().total() == 0 {
        // no masternode registered to receive a payment
        return done(outs, true);
    }
    let Some(payee) = list.payee(rules) else {
        return done(outs, false);
    };
    let state = &payee.cold;

    let mut operator_part = 0;
    if payee.operator_reward != 0 && !state.script_operator_payout.is_empty() {
        operator_part = operator_reward(masternode_reward, payee.operator_reward);
        masternode_reward = masternode_reward.wrapping_sub(operator_part);
    }

    let mut pay = |value: Amount, script: &[u8]| {
        if value > 0 {
            outs.push(TxOut {
                value,
                script_pubkey: script.to_vec().into(),
            });
        }
    };
    if state.is_shared() {
        // Shared masternodes split the owner reward by recorded collateral contribution, paying
        // each share's reward script (or its refund script when no reward script is set)
        let amounts: Vec<Amount> = state.shares.iter().map(|share| share.amount).collect();
        let split = split_amount_by_shares(masternode_reward, &amounts)?;
        for (share, amount) in state.shares.iter().zip(split) {
            // Core: evo/providertx.h:80 CCollateralShare::RewardScript
            let script = if share.script_reward.is_empty() {
                &share.script_refund
            } else {
                &share.script_reward
            };
            pay(amount, script.as_bytes());
        }
    } else if state.version >= EXT_ADDR {
        // Core: evo/providertx.h:94 GetOwnerPayouts
        let bps: Vec<u16> = state.payouts.iter().map(|share| share.reward).collect();
        for (share, amount) in
            state.payouts.iter().zip(owner_payout_amounts(masternode_reward, &bps))
        {
            pay(amount, share.script_payout.as_bytes());
        }
    } else {
        // `LegacyPayoutAsList(scriptPayout)`: one payout, the last, so it gets the whole reward.
        pay(masternode_reward, &state.script_payout);
    }
    pay(operator_part, &state.script_operator_payout);

    done(outs, true)
}

/// Core's `IsTransactionValid` from DIP0003 enforcement, which resolves
/// `Ev3Branch::MasternodePayee`: whether `coinbase` pays every output [`block_tx_outs`] owes on
/// top of `list` (the parent's). Valid when Core cannot compute them (no payable masternode;
/// Core logs and accepts, F6). `strict_multiplicity` is V24's: each owed output needs its own
/// coinbase output.
///
/// The DIP0003 gate is `dash_evo::payments::block_payee_valid`'s: it returns the branch only from
/// enforcement.
// Core: masternode/payments.cpp:227-260 CMNPaymentsProcessor::IsTransactionValid
pub fn masternode_payee_valid(
    coinbase: &Tx,
    list: &MnList,
    rules: PayeeRules,
    reward: &BlockReward,
    strict_multiplicity: bool,
    p: &PaymentParams,
) -> Result<bool, PayeeError> {
    let owed = block_tx_outs(list, rules, reward, p)?;
    // Failed to get payees for block at height
    Ok(!owed.computed
        || find_unmatched_masternode_payment(
            &owed.outputs,
            coinbase.outputs(),
            strict_multiplicity,
        )
        .is_none())
}

#[cfg(test)]
mod tests;
