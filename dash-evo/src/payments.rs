//! Core's coinbase value and payee checks (`CMNPaymentsProcessor::IsBlockValueValid`,
//! `IsOldBudgetBlockValueValid` and `IsBlockPayeeValid`; CH3a slice a1, EV3's first part): every
//! branch that does not need governance data, with the governance flags read through
//! [`GovernanceView`].
//!
//! The branches that do (EV3) are seams: where Core asks `IsValidSuperblock` or the masternode
//! payee check (`IsTransactionValid` from DIP0003 enforcement: `GetBlockTxOuts` and
//! `FindUnmatchedMasternodePayment`), these functions stop and return [`Ev3Branch`], which names
//! the check and carries Core's arguments for it. They never guess its answer.
//!
//! EV3 resolves the seams. [`valid_superblock`] (with [`superblock_valid`], `CSuperblock::IsValid`)
//! answers [`Ev3Branch::ValidSuperblock`] here. [`Ev3Branch::MasternodePayee`] reads the masternode
//! list, so `dash_dml::payments` answers it, with [`find_unmatched_masternode_payment`] from
//! here (EV3 D2).
//!
//! `ConnectBlock` calls the value check, then the payee check, and rejects with
//! [`BAD_CB_AMOUNT`] or [`BAD_CB_PAYEE`] (`BLOCK_RESULT_UNSET`: no punishment, the node might lack
//! governance data). The value check's message ([`BlockValue::Invalid`]) is Core's `strError`,
//! which Core only logs; the reject reason is always [`BAD_CB_AMOUNT`].
//!
//! Amounts are `CAmount`s. Core's `blockReward + GetPaymentsLimit(...)` and `blockSubsidy +
//! feeReward` wrap here as in the release binaries (no reachable block comes near it).

use std::fmt;

use dash_consensus::payments::MnRewardEra;
use dash_consensus::subsidy::{
    SubsidyError, SubsidyParams, genesis_reward, superblock_payments_limit,
};
use dash_consensus::{Amount, ParamsError, Tx};
use dash_params::ChainSetup;
use dash_primitives::TxOut;

/// `ConnectBlock`'s reject reason when the value check fails.
// Core: validation.cpp:2849
pub const BAD_CB_AMOUNT: &str = "bad-cb-amount";

/// `ConnectBlock`'s reject reason when the payee check fails.
// Core: validation.cpp:2864
pub const BAD_CB_PAYEE: &str = "bad-cb-payee";

/// Core's `SuperBlockCheckType`: how far a block's superblock is checked.
// Core: masternode/payments.h:66-70 SuperBlockCheckType
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SuperblockCheck {
    /// For ChainLocked blocks, or before the node is synced: the superblock maximum only at
    /// superblock heights, and no bound at all inside an old budget window.
    NoCheck,
    /// Before V24: a superblock may pay the same payee twice with one output.
    AllowDuplicates,
    /// V24: each payment needs its own output (`is_v24`).
    DisallowDuplicates,
}

impl SuperblockCheck {
    /// `ConnectBlock`'s choice: [`SuperblockCheck::NoCheck`] unless `required`
    /// (`IsSuperblockValidationRequired(pindex)`: the block is above the best ChainLock and the
    /// node is synced), then by whether V24 is active at the block.
    // Core: validation.cpp:2841-2843
    pub fn for_block(required: bool, v24: bool) -> Self {
        if !required {
            Self::NoCheck
        } else if v24 {
            Self::DisallowDuplicates
        } else {
            Self::AllowDuplicates
        }
    }

    /// `is_v24`, which Core passes to `IsValidSuperblock`.
    pub fn is_v24(self) -> bool {
        self == Self::DisallowDuplicates
    }
}

/// The governance state the checks read (Core's `m_superblocks`, the governance manager, at the
/// chain tip). GV1a and GV1b implement it over the governance state (EV3 D3).
///
/// [`GovernanceView::sb_triggered`] is not a pure query in Core: `IsSuperblockTriggered` calls
/// `UpdateSentinelVariables(tip_mn_list)` on each trigger for the height until it finds a funded
/// one, which rewrites the trigger's cached funding, endorsement and validity flags and can mark
/// it for deletion (`governance/superblock.cpp:577-615`, `governance/object.cpp:964-997`). So the
/// queries take `&mut self`, as `ChainHelper`'s do (CH3 §3.3, which also records the answers);
/// `ConnectBlock` asks it from both checks at a superblock height. [`GovernanceView::gov_valid`]
/// and [`GovernanceView::sb_payments`] change nothing.
///
/// The queries are fallible, with an error type the implementor chooses: an error stops the check
/// and is returned to the caller unchanged, e.g. a recorded replay's decision miss.
pub trait GovernanceView {
    /// The implementor's error: what a query that cannot be answered returns.
    type Error;

    /// `m_superblocks.IsValid()`: the governance data is loaded. Without it Core accepts what
    /// it cannot check.
    fn gov_valid(&mut self) -> Result<bool, Self::Error>;

    /// `IsSuperblockTriggered(tip_mn_list, height)`: at a superblock height, the first trigger
    /// for `height` whose cached funding flag is set after its update (see the trait's
    /// documentation). Off a superblock height it must answer false and change nothing, as Core
    /// does before it locks or updates anything (`governance/superblock.cpp:581-583`). The
    /// checks ask it at every height Core does, superblock or not, so a recorded decision
    /// sequence (Core's `sb_triggered` record at that early return) replays in order.
    fn sb_triggered(&mut self, height: u32) -> Result<bool, Self::Error>;

    /// The payments of the best superblock for `height` (`GetBestSuperblock`: the trigger for
    /// `height` with the most absolute FUNDING yes votes, `None` when none has more than 0),
    /// which `IsValidSuperblock` checks. It need not be the trigger
    /// [`GovernanceView::sb_triggered`] found. EV3 reads it; the governance-free branches never
    /// do.
    // Core: governance/superblock.cpp:555-575 GetBestSuperblockInternal, :622-631
    // IsValidSuperblock
    fn sb_payments(&mut self, height: u32) -> Result<Option<Vec<TxOut>>, Self::Error>;
}

/// Why the value check has no answer: Core's own gap ([`SubsidyError`]) or the governance view's
/// error (`E`, a [`GovernanceView::Error`]), returned unchanged.
#[derive(Debug, PartialEq, Eq)]
pub enum PaymentsError<E> {
    /// `GetPaymentsLimit` has no answer for the height.
    Subsidy(SubsidyError),
    /// A governance query failed.
    Governance(E),
}

impl<E: fmt::Display> fmt::Display for PaymentsError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Subsidy(e) => fmt::Display::fmt(e, f),
            Self::Governance(e) => fmt::Display::fmt(e, f),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for PaymentsError<E> {}

/// A budget cycle or window Core's `nBlockHeight % nBudgetPaymentsCycleBlocks` cannot run with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaymentsParamsError {
    /// The subsidy parameters' error.
    Params(ParamsError),
    /// `nBudgetPaymentsCycleBlocks` is not positive (Core divides by it).
    BudgetCycle,
}

impl fmt::Display for PaymentsParamsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Params(e) => e.fmt(f),
            Self::BudgetCycle => f.write_str("the budget payments cycle is not positive"),
        }
    }
}

impl std::error::Error for PaymentsParamsError {}

impl From<ParamsError> for PaymentsParamsError {
    fn from(e: ParamsError) -> Self {
        Self::Params(e)
    }
}

/// The consensus parameters the checks read, from one [`ChainSetup`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaymentsParams {
    /// `nBudgetPaymentsStartBlock`, `nSuperblockStartBlock` and `nSuperblockCycle`, and the
    /// subsidy formula `GetPaymentsLimit` uses.
    subsidy: SubsidyParams,
    /// `nBudgetPaymentsCycleBlocks`, positive.
    budget_cycle: i32,
    /// `nBudgetPaymentsWindowBlocks`.
    budget_window: i32,
    /// `DIP0003EnforcementHeight`.
    dip0003_enforcement: u32,
}

impl PaymentsParams {
    /// The parameters of `chain` (with its `-budgetparams`, when a regtest's).
    pub fn from_chain_params(chain: &ChainSetup) -> Result<Self, PaymentsParamsError> {
        let subsidy = SubsidyParams::from_chain_params(
            chain,
            genesis_reward(&dash_consensus::params::genesis_block(chain))?,
        )?;
        let c = &chain.consensus;
        if c.budget_payments_cycle_blocks <= 0 {
            return Err(PaymentsParamsError::BudgetCycle);
        }
        Ok(Self {
            subsidy,
            budget_cycle: c.budget_payments_cycle_blocks,
            budget_window: c.budget_payments_window_blocks,
            dip0003_enforcement: c.dip0003_enforcement.0.to_u32(),
        })
    }

    /// `nBudgetPaymentsStartBlock`.
    pub fn budget_start(&self) -> i32 {
        self.subsidy.config().budget_payments_start
    }

    /// `nSuperblockStartBlock`.
    pub fn superblock_start(&self) -> i32 {
        self.subsidy.config().superblocks.start()
    }

    /// `nBudgetPaymentsCycleBlocks`.
    pub fn budget_cycle(&self) -> i32 {
        self.budget_cycle
    }

    /// `nBudgetPaymentsWindowBlocks`.
    pub fn budget_window(&self) -> i32 {
        self.budget_window
    }

    /// `DIP0003EnforcementHeight`.
    pub fn dip0003_enforcement(&self) -> u32 {
        self.dip0003_enforcement
    }

    /// The subsidy parameters (`GetPaymentsLimit`'s).
    pub fn subsidy(&self) -> &SubsidyParams {
        &self.subsidy
    }

    /// `DeploymentDIP0003Enforced(height, params)`.
    // Core: deploymentstatus.h:66-69 DeploymentDIP0003Enforced
    fn dip0003_enforced(&self, height: u32) -> bool {
        height >= self.dip0003_enforcement
    }
}

/// A check Core makes with governance data or the masternode list, which EV3 implements. The
/// fields are Core's arguments for it, besides the coinbase, the height and the governance state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ev3Branch {
    /// `IsTransactionValid` from DIP0003 enforcement: `GetBlockTxOuts(pindexPrev, block_subsidy,
    /// fee_reward, era)`, then `FindUnmatchedMasternodePayment(.., strict_multiplicity)`. Core
    /// accepts when it cannot compute the payments and rejects ([`BAD_CB_PAYEE`]) when one is
    /// unmatched; when every one is matched, the check goes on with [`superblock_payee_valid`].
    // Core: masternode/payments.cpp:227-260 IsTransactionValid
    MasternodePayee {
        block_subsidy: Amount,
        fee_reward: Amount,
        era: MnRewardEra,
        strict_multiplicity: bool,
    },
    /// `IsValidSuperblock(active_chain, tip_mn_list, coinbase, height, block_reward, is_v24)`,
    /// over the best superblock for the height ([`GovernanceView::sb_payments`]; none is
    /// invalid). Valid: the check passes. Invalid: the value check fails with
    /// [`invalid_superblock_error`], the payee check with [`BAD_CB_PAYEE`].
    ValidSuperblock {
        block_reward: Amount,
        is_v24: bool,
    },
}

/// The value check's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockValue {
    Valid,
    /// Core's `strError` (logged; the reject reason is [`BAD_CB_AMOUNT`]).
    Invalid(String),
    /// Decided by EV3's [`Ev3Branch::ValidSuperblock`].
    Ev3(Ev3Branch),
}

/// The payee check's answer: no governance-free branch rejects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockPayee {
    Valid,
    /// Decided by EV3's check.
    Ev3(Ev3Branch),
}

/// The value check's `strError` when the triggered superblock is invalid.
// Core: masternode/payments.cpp:384
pub fn invalid_superblock_error(height: u32) -> String {
    format!("invalid superblock detected at height {height}")
}

/// `coinbase pays too much at height %d (actual=%d vs limit=%d), <why>`.
fn pays_too_much(height: u32, actual: Amount, limit: Amount, why: &str) -> String {
    format!("coinbase pays too much at height {height} (actual={actual} vs limit={limit}), {why}")
}

/// `isBlockRewardValueMet`, with its `strError` when not met.
fn within(height: u32, actual: Amount, limit: Amount, why: &str) -> BlockValue {
    if actual <= limit {
        BlockValue::Valid
    } else {
        BlockValue::Invalid(pays_too_much(height, actual, limit, why))
    }
}

/// Core's `IsBlockValueValid`: whether the coinbase of the block at `height` pays no more than
/// `block_reward` (the subsidy plus the fees), or, at a superblock height, than the superblock
/// allows.
///
/// - Below `nBudgetPaymentsStartBlock`: the reward bound.
/// - Below `nSuperblockStartBlock`: [`old_budget_block_value_valid`].
/// - Off superblock heights: the reward bound.
/// - At a superblock height: above `block_reward + GetPaymentsLimit(height)`, invalid. Then,
///   without governance data or with [`SuperblockCheck::NoCheck`], valid; without a triggered
///   superblock, the reward bound; with one, [`Ev3Branch::ValidSuperblock`].
///
/// `Err` where Core's `GetPaymentsLimit` has no answer ([`PaymentsError::Subsidy`]: a height above
/// Core's `int`, or a superblock at height 0), or where a governance query fails
/// ([`PaymentsError::Governance`]; the check stops at that query).
///
/// # Panics
///
/// When an output value or their sum is outside the money range (see [`Tx::value_out`]):
/// `CheckBlock` (`bad-txns-vout-toolarge`, `bad-txns-txouttotal-toolarge`) rejects such a
/// coinbase before `ConnectBlock` gets here.
// Core: masternode/payments.cpp:313-388 CMNPaymentsProcessor::IsBlockValueValid
pub fn block_value_valid<G: GovernanceView + ?Sized>(
    coinbase: &Tx,
    height: u32,
    block_reward: Amount,
    check: SuperblockCheck,
    p: &PaymentsParams,
    gov: &mut G,
) -> Result<BlockValue, PaymentsError<G::Error>> {
    let actual = coinbase.value_out();
    let h = i64::from(height);

    if h < i64::from(p.budget_start()) {
        // old budget system is not activated yet, just make sure we do not exceed the regular
        // block reward
        return Ok(within(
            height,
            actual,
            block_reward,
            "exceeded block reward, old budgets are not activated yet",
        ));
    } else if h < i64::from(p.superblock_start()) {
        // superblocks are not enabled yet, check if we can pass old budget rules
        return Ok(old_budget_block_value_valid(coinbase, height, block_reward, check, p));
    }

    let sb_height =
        i32::try_from(height).map_err(|_| PaymentsError::Subsidy(SubsidyError::Unreachable))?;
    let superblock_max = block_reward.wrapping_add(
        superblock_payments_limit(sb_height, p.subsidy()).map_err(PaymentsError::Subsidy)?,
    );

    if !p.subsidy().config().superblocks.is_superblock_height(sb_height) {
        // can't possibly be a superblock, so lets just check for block reward limits
        return Ok(within(
            height,
            actual,
            block_reward,
            "exceeded block reward, only regular blocks are allowed at this height",
        ));
    }

    // bail out in case superblock limits were exceeded
    if actual > superblock_max {
        return Ok(BlockValue::Invalid(pays_too_much(
            height,
            actual,
            superblock_max,
            "exceeded superblock max value",
        )));
    }

    if !gov.gov_valid().map_err(PaymentsError::Governance)? {
        // not enough data for full checks but at least we know that the superblock limits were
        // honored
        return Ok(BlockValue::Valid);
    }

    if check == SuperblockCheck::NoCheck {
        return Ok(BlockValue::Valid);
    }

    if !gov.sb_triggered(height).map_err(PaymentsError::Governance)? {
        // we are on a valid superblock height but a superblock was not triggered: revert to
        // block reward limits in this case
        return Ok(within(
            height,
            actual,
            block_reward,
            "exceeded block reward, no triggered superblock detected",
        ));
    }

    // this actually also checks for correct payees and not only amount
    Ok(BlockValue::Ev3(Ev3Branch::ValidSuperblock {
        block_reward,
        is_v24: check.is_v24(),
    }))
}

/// Core's `IsOldBudgetBlockValueValid`: the value check from `nBudgetPaymentsStartBlock` to
/// `nSuperblockStartBlock`. Inside each cycle's budget window Core has no budget data: it accepts
/// any value with [`SuperblockCheck::NoCheck`] (historical mainnet blocks there paid budgets) and
/// the reward bound otherwise. Outside the window, the reward bound. Outside the range, invalid.
///
/// # Panics
///
/// When an output value or their sum is outside the money range (see [`Tx::value_out`]).
// Core: masternode/payments.cpp:262-300 CMNPaymentsProcessor::IsOldBudgetBlockValueValid
pub fn old_budget_block_value_valid(
    coinbase: &Tx,
    height: u32,
    block_reward: Amount,
    check: SuperblockCheck,
    p: &PaymentsParams,
) -> BlockValue {
    let actual = coinbase.value_out();
    let h = i64::from(height);

    if h < i64::from(p.budget_start()) {
        return BlockValue::Invalid(format!(
            "Incorrect block {height}, old budgets are not activated yet"
        ));
    }

    if h >= i64::from(p.superblock_start()) {
        return BlockValue::Invalid(format!(
            "Incorrect block {height}, old budgets are no longer active"
        ));
    }

    // we are still using budgets, but we have no data about them anymore, all we know is the
    // predefined budget cycle and window
    let offset = h % i64::from(p.budget_cycle);
    if offset < i64::from(p.budget_window) {
        // NOTE: old budget system is disabled since 12.1
        if check == SuperblockCheck::NoCheck {
            // historical mainnet blocks in this window do pay old budgets and we have no data to
            // validate them with, so rely on online nodes (all networks)
            return BlockValue::Valid;
        }
        // no old budget blocks should be accepted here on mainnet, testnet/devnet/regtest
        // should produce regular blocks only
        return within(
            height,
            actual,
            block_reward,
            "exceeded block reward, old budgets are disabled",
        );
    }
    within(
        height,
        actual,
        block_reward,
        "exceeded block reward, block is not in old budget cycle window",
    )
}

/// Core's `IsBlockPayeeValid`: whether the coinbase of the block at `height` pays the masternode
/// and the triggered superblock. From DIP0003 enforcement the masternode payee check comes first
/// ([`Ev3Branch::MasternodePayee`]); below it, or once EV3's check has passed, the rest is
/// [`superblock_payee_valid`].
///
/// `Err` when a governance query fails; the check stops at that query.
// Core: masternode/payments.cpp:392-441 CMNPaymentsProcessor::IsBlockPayeeValid
#[allow(clippy::too_many_arguments)]
pub fn block_payee_valid<G: GovernanceView + ?Sized>(
    height: u32,
    block_subsidy: Amount,
    fee_reward: Amount,
    era: MnRewardEra,
    strict_multiplicity: bool,
    check: SuperblockCheck,
    p: &PaymentsParams,
    gov: &mut G,
) -> Result<BlockPayee, G::Error> {
    // Check for correct masternode payment
    // Core: masternode/payments.cpp:227-234 IsTransactionValid
    if p.dip0003_enforced(height) {
        return Ok(BlockPayee::Ev3(Ev3Branch::MasternodePayee {
            block_subsidy,
            fee_reward,
            era,
            strict_multiplicity,
        }));
    }
    // can't verify historical blocks here
    superblock_payee_valid(height, block_subsidy, fee_reward, check, p, gov)
}

/// The part of Core's `IsBlockPayeeValid` after the masternode payee check passed: without
/// governance data, below `nSuperblockStartBlock`, or with [`SuperblockCheck::NoCheck`], valid;
/// with a superblock triggered at `height`, [`Ev3Branch::ValidSuperblock`] with the subsidy plus
/// the fees; without one, valid.
///
/// `Err` when a governance query fails; the check stops at that query.
// Core: masternode/payments.cpp:405-440 CMNPaymentsProcessor::IsBlockPayeeValid
pub fn superblock_payee_valid<G: GovernanceView + ?Sized>(
    height: u32,
    block_subsidy: Amount,
    fee_reward: Amount,
    check: SuperblockCheck,
    p: &PaymentsParams,
    gov: &mut G,
) -> Result<BlockPayee, G::Error> {
    if !gov.gov_valid()? {
        // governance data is either incomplete or non-existent
        return Ok(BlockPayee::Valid);
    }

    if i64::from(height) < i64::from(p.superblock_start()) {
        // We are still using budgets, but we have no data about them anymore, we can only check
        // masternode payments.
        return Ok(BlockPayee::Valid);
    }

    // superblocks started
    if check == SuperblockCheck::NoCheck {
        return Ok(BlockPayee::Valid);
    }

    if gov.sb_triggered(height)? {
        // should also pay MN
        return Ok(BlockPayee::Ev3(Ev3Branch::ValidSuperblock {
            block_reward: block_subsidy.wrapping_add(fee_reward),
            is_v24: check.is_v24(),
        }));
    }
    Ok(BlockPayee::Valid)
}

/// Core's `FindUnmatchedMasternodePayment`: the index of the first `expected` output that
/// `actual` does not pay, or `None` when every one is paid.
///
/// Without `strict_multiplicity` (before V24) an output may pay several expected outputs, so
/// each is looked up in all of `actual`. With it, each expected output consumes the first
/// unconsumed equal output: an expected output listed twice needs two coinbase outputs. Equal
/// means the same value and script bytes (`CTxOut::operator==`).
// Core: masternode/payments.cpp:24-52 FindUnmatchedMasternodePayment
pub fn find_unmatched_masternode_payment(
    expected: &[TxOut],
    actual: &[TxOut],
    strict_multiplicity: bool,
) -> Option<usize> {
    if !strict_multiplicity {
        return expected.iter().position(|txout| !actual.contains(txout));
    }

    let mut consumed = vec![false; actual.len()];
    for (i, txout) in expected.iter().enumerate() {
        let found = (0..actual.len()).find(|&j| !consumed[j] && actual[j] == *txout);
        match found {
            Some(j) => consumed[j] = true,
            None => return Some(i),
        }
    }
    None
}

/// Core's `CSuperblock::IsValid`: whether the coinbase of the block at `height` pays the
/// superblock `payments` (the best trigger's, in order; [`GovernanceView::sb_payments`]) and
/// gives the miner and the masternodes no more than `block_reward`.
///
/// - Not a superblock height: invalid.
/// - Fewer outputs than payments: invalid.
/// - The payments above `GetPaymentsLimit(height)`: invalid.
/// - The coinbase above `block_reward` plus the payments: invalid.
/// - Each payment, in order, must equal (script and amount) an output at or after the previous
///   payment's output; from V24 (`is_v24`) strictly after it, so equal payments need distinct
///   outputs.
///
/// The payments' sum and `block_reward` plus it are `CAmount` additions whose overflow is
/// undefined in Core; they wrap here, as in the release binaries. `ParsePaymentSchedule` bounds
/// each amount (`MoneyRange`) but not their sum.
///
/// `Err` where Core's `GetPaymentsLimit` has no answer (a superblock at height 0, or a height
/// above Core's `int`).
///
/// # Panics
///
/// When an output value or their sum is outside the money range (see [`Tx::value_out`]):
/// `CheckBlock` rejects such a coinbase first.
// Core: governance/superblock.cpp:248-337 CSuperblock::IsValid
pub fn superblock_valid(
    coinbase: &Tx,
    height: u32,
    payments: &[TxOut],
    block_reward: Amount,
    is_v24: bool,
    p: &PaymentsParams,
) -> Result<bool, SubsidyError> {
    let height = i32::try_from(height).map_err(|_| SubsidyError::Unreachable)?;
    if !p.subsidy().config().superblocks.is_superblock_height(height) {
        // Block invalid, incorrect block height
        return Ok(false);
    }

    // We require an exact match (including order) between the expected superblock payments and
    // the payments actually in the block.
    let outputs = coinbase.outputs();
    if outputs.len() < payments.len() {
        // Block invalid, too few superblock payments
        return Ok(false);
    }

    // payments should not exceed limit
    let total = payments.iter().fold(0, |sum: Amount, payment| sum.wrapping_add(payment.value));
    if total > superblock_payments_limit(height, p.subsidy())? {
        return Ok(false);
    }

    // miner and masternodes should not get more than they would usually get
    if coinbase.value_out() > block_reward.wrapping_add(total) {
        return Ok(false);
    }

    // Core's `nVoutIndex`: the output the previous payment matched.
    let mut matched: Option<usize> = None;
    for payment in payments {
        // From V24 on, start past the previously matched output so each expected payment
        // consumes a distinct vout. Before V24 the scan restarted at the previously matched
        // index (inclusive).
        let start = match matched {
            Some(j) if is_v24 => j + 1,
            Some(j) => j,
            None => 0,
        };
        match (start..outputs.len()).find(|&j| outputs[j] == *payment) {
            Some(j) => matched = Some(j),
            // Superblock payment not found!
            None => return Ok(false),
        }
    }
    Ok(true)
}

/// `IsValidSuperblock(...)`, which resolves [`Ev3Branch::ValidSuperblock`]: the best superblock's
/// payments for `height` ([`GovernanceView::sb_payments`]) checked by [`superblock_valid`];
/// invalid when there is no best superblock.
///
/// The value check maps invalid to [`BlockValue::Invalid`] with [`invalid_superblock_error`],
/// the payee check to [`BAD_CB_PAYEE`].
///
/// # Panics
///
/// As [`superblock_valid`].
// Core: governance/superblock.cpp:622-631 SuperblockManager::IsValidSuperblock
pub fn valid_superblock<G: GovernanceView + ?Sized>(
    coinbase: &Tx,
    height: u32,
    block_reward: Amount,
    is_v24: bool,
    p: &PaymentsParams,
    gov: &mut G,
) -> Result<bool, PaymentsError<G::Error>> {
    match gov.sb_payments(height).map_err(PaymentsError::Governance)? {
        Some(payments) => superblock_valid(coinbase, height, &payments, block_reward, is_v24, p)
            .map_err(PaymentsError::Subsidy),
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    //! Every branch of the three functions, with a scripted [`GovernanceView`] that records what
    //! it was asked, on mainnet's parameters (and the boundaries on testnet, devnet and regtest).
    //! Heights come from the parameters; the literal values in `parameters_are_cores` are
    //! chainparams.cpp's, to show which heights the tables reach.

    use dash_consensus::subsidy::COIN;
    use dash_params::{ChainOption, ChainParamsBuilder, Network, Release};
    use dash_primitives::{Transaction, TxType};

    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Query {
        Valid,
        Triggered(u32),
        Payments(u32),
    }

    /// Governance data as scripted: loaded or not, a superblock triggered at every height or at
    /// none.
    struct Script {
        valid: bool,
        triggered: bool,
        queries: Vec<Query>,
    }

    impl Script {
        fn new(valid: bool, triggered: bool) -> Self {
            Self {
                valid,
                triggered,
                queries: Vec::new(),
            }
        }

        fn take(&mut self) -> Vec<Query> {
            std::mem::take(&mut self.queries)
        }
    }

    impl GovernanceView for Script {
        type Error = std::convert::Infallible;

        fn gov_valid(&mut self) -> Result<bool, Self::Error> {
            self.queries.push(Query::Valid);
            Ok(self.valid)
        }

        fn sb_triggered(&mut self, height: u32) -> Result<bool, Self::Error> {
            self.queries.push(Query::Triggered(height));
            Ok(self.triggered)
        }

        fn sb_payments(&mut self, height: u32) -> Result<Option<Vec<TxOut>>, Self::Error> {
            self.queries.push(Query::Payments(height));
            Ok(None)
        }
    }

    /// The query a [`Failing`] view could not answer.
    #[derive(Debug, PartialEq, Eq)]
    struct Miss(Query);

    /// Governance data that answers yes to everything until query number `fail_at` (from 0), which
    /// fails, like a recorded replay's decision miss.
    struct Failing {
        fail_at: usize,
        queries: Vec<Query>,
    }

    impl Failing {
        fn new(fail_at: usize) -> Self {
            Self {
                fail_at,
                queries: Vec::new(),
            }
        }

        fn ask(&mut self, query: Query) -> Result<bool, Miss> {
            self.queries.push(query);
            if self.queries.len() - 1 == self.fail_at {
                Err(Miss(query))
            } else {
                Ok(true)
            }
        }
    }

    impl GovernanceView for Failing {
        type Error = Miss;

        fn gov_valid(&mut self) -> Result<bool, Miss> {
            self.ask(Query::Valid)
        }

        fn sb_triggered(&mut self, height: u32) -> Result<bool, Miss> {
            self.ask(Query::Triggered(height))
        }

        fn sb_payments(&mut self, height: u32) -> Result<Option<Vec<TxOut>>, Miss> {
            self.ask(Query::Payments(height)).map(|_| None)
        }
    }

    const ALL_CHECKS: [SuperblockCheck; 3] = [
        SuperblockCheck::NoCheck,
        SuperblockCheck::AllowDuplicates,
        SuperblockCheck::DisallowDuplicates,
    ];

    /// The subsidy plus fees of every table: any amount works, the checks only compare with it.
    const REWARD: Amount = 3 * COIN + 12_345;

    /// The checks that bound a block: `SuperblockCheck::for_block` when validation is required.
    const REQUIRED_CHECKS: [SuperblockCheck; 2] =
        [SuperblockCheck::AllowDuplicates, SuperblockCheck::DisallowDuplicates];

    /// Mainnet's budget and superblock cycle, in blocks.
    const MAINNET_CYCLE: u32 = 16_616;

    fn chain(network: Network) -> ChainSetup {
        let mut builder = ChainParamsBuilder::new(network, Release::V24_0);
        if network == Network::Devnet {
            builder = builder.arg(ChainOption::Devnet, "ch3a");
        }
        builder.build().expect("Core's parameters")
    }

    fn params(network: Network) -> PaymentsParams {
        PaymentsParams::from_chain_params(&chain(network)).expect("payments parameters")
    }

    /// A coinbase paying `value` in two outputs (`GetValueOut` sums them).
    fn coinbase(value: Amount) -> Tx {
        let out = |value| TxOut {
            value,
            script_pubkey: vec![0x51].into(),
        };
        Tx::new(Transaction {
            version: 3,
            tx_type: TxType::new(5),
            inputs: vec![],
            outputs: vec![out(value / 2), out(value - value / 2)],
            lock_time: 0,
            extra_payload: vec![],
        })
    }

    fn value(
        actual: Amount,
        height: u32,
        check: SuperblockCheck,
        p: &PaymentsParams,
        gov: &mut Script,
    ) -> BlockValue {
        block_value_valid(&coinbase(actual), height, REWARD, check, p, gov).expect("an int height")
    }

    fn invalid(height: u32, actual: Amount, limit: Amount, why: &str) -> BlockValue {
        BlockValue::Invalid(format!(
            "coinbase pays too much at height {height} (actual={actual} vs limit={limit}), {why}"
        ))
    }

    /// `REWARD` is valid at `height` and `REWARD + 1` is over the bound, for `why`.
    fn reward_is_the_bound(
        height: u32,
        check: SuperblockCheck,
        why: &str,
        p: &PaymentsParams,
        gov: &mut Script,
    ) {
        assert_eq!(value(REWARD, height, check, p, gov), BlockValue::Valid);
        assert_eq!(
            value(REWARD + 1, height, check, p, gov),
            invalid(height, REWARD + 1, REWARD, why)
        );
    }

    fn budget_start(p: &PaymentsParams) -> u32 {
        u32::try_from(p.budget_start()).unwrap()
    }

    fn superblock_start(p: &PaymentsParams) -> u32 {
        u32::try_from(p.superblock_start()).unwrap()
    }

    /// The first superblock height at or after the start.
    fn first_superblock(p: &PaymentsParams) -> u32 {
        let c = p.subsidy().config().superblocks;
        let start = c.start();
        let first = start + (c.cycle() - start % c.cycle()) % c.cycle();
        u32::try_from(first).unwrap()
    }

    /// The first height of the first budget window at or after the budget start.
    fn first_window(p: &PaymentsParams) -> u32 {
        let (start, cycle) = (p.budget_start(), p.budget_cycle());
        let first = if start % cycle < p.budget_window() {
            start
        } else {
            start + cycle - start % cycle
        };
        u32::try_from(first).unwrap()
    }

    #[test]
    fn parameters_are_cores() {
        // (budget start, cycle, window, superblock start, superblock cycle, DIP3 enforcement)
        for (network, want) in [
            (Network::Main, (328_008, 16_616, 100, 614_820, 16_616, 1_047_200)),
            (Network::Testnet3, (4_100, 50, 10, 4_200, 24, 7_300)),
            (Network::Devnet, (4_100, 50, 10, 4_200, 24, 2)),
            (Network::Regtest, (1_000, 50, 10, 1_500, 20, 500)),
        ] {
            let p = params(network);
            let have = (
                p.budget_start(),
                p.budget_cycle(),
                p.budget_window(),
                p.superblock_start(),
                p.subsidy().config().superblocks.cycle(),
                p.dip0003_enforcement(),
            );
            assert_eq!(have, want, "{network:?}");
        }
        let mut broken = chain(Network::Main);
        broken.consensus.budget_payments_cycle_blocks = 0;
        assert_eq!(
            PaymentsParams::from_chain_params(&broken),
            Err(PaymentsParamsError::BudgetCycle)
        );
    }

    #[test]
    fn superblock_check_type_is_connect_blocks() {
        use SuperblockCheck::*;
        assert_eq!(SuperblockCheck::for_block(false, false), NoCheck);
        assert_eq!(SuperblockCheck::for_block(false, true), NoCheck);
        assert_eq!(SuperblockCheck::for_block(true, false), AllowDuplicates);
        assert_eq!(SuperblockCheck::for_block(true, true), DisallowDuplicates);
        assert_eq!(ALL_CHECKS.map(SuperblockCheck::is_v24), [false, false, true]);
    }

    #[test]
    fn below_the_budget_start_the_reward_bounds_the_value() {
        let p = params(Network::Main);
        let mut gov = Script::new(true, true);
        let h = budget_start(&p) - 1;
        let why = "exceeded block reward, old budgets are not activated yet";
        for check in ALL_CHECKS {
            for height in [0, 1, h] {
                assert_eq!(value(0, height, check, &p, &mut gov), BlockValue::Valid);
                reward_is_the_bound(height, check, why, &p, &mut gov);
            }
        }
        assert_eq!(gov.take(), []);
    }

    #[test]
    fn old_budget_windows_are_unbounded_under_no_check() {
        let p = params(Network::Main);
        let mut gov = Script::new(true, true);
        let window = first_window(&p);
        assert_eq!(window, 20 * MAINNET_CYCLE);
        let window_last = window + u32::try_from(p.budget_window()).unwrap() - 1;
        // The last old-budget height is in a window too: 614819 = 37 * 16616 + 27.
        let last = superblock_start(&p) - 1;
        let last_window = last - last % MAINNET_CYCLE;
        assert!(last - last_window < 100);
        let disabled = "exceeded block reward, old budgets are disabled";
        for height in [window, window_last, last_window, last] {
            // NoCheck accepts any value (quirk 14).
            let huge = 1_000_000 * COIN;
            assert_eq!(
                value(huge, height, SuperblockCheck::NoCheck, &p, &mut gov),
                BlockValue::Valid
            );
            for check in REQUIRED_CHECKS {
                reward_is_the_bound(height, check, disabled, &p, &mut gov);
            }
        }
        // Outside the windows, from the budget start on.
        let outside = "exceeded block reward, block is not in old budget cycle window";
        let budget = budget_start(&p);
        assert_ne!(budget % MAINNET_CYCLE, 0, "mainnet's budget start is outside a window");
        for height in [budget, window - 1, window_last + 1, last_window - 1] {
            for check in ALL_CHECKS {
                reward_is_the_bound(height, check, outside, &p, &mut gov);
            }
        }
        assert_eq!(gov.take(), []);
    }

    #[test]
    fn old_budget_check_outside_its_range() {
        // IsBlockValueValid never calls it there; Core's function still answers.
        let p = params(Network::Main);
        let below = budget_start(&p) - 1;
        let from = superblock_start(&p);
        for check in ALL_CHECKS {
            assert_eq!(
                old_budget_block_value_valid(&coinbase(0), below, REWARD, check, &p),
                BlockValue::Invalid(format!(
                    "Incorrect block {below}, old budgets are not activated yet"
                ))
            );
            assert_eq!(
                old_budget_block_value_valid(&coinbase(0), from, REWARD, check, &p),
                BlockValue::Invalid(format!(
                    "Incorrect block {from}, old budgets are no longer active"
                ))
            );
        }
    }

    #[test]
    fn from_the_superblock_start_regular_heights_are_bounded_by_the_reward() {
        let p = params(Network::Main);
        let mut gov = Script::new(true, true);
        let start = superblock_start(&p);
        let first = first_superblock(&p);
        assert_eq!((start, first), (614_820, 38 * MAINNET_CYCLE));
        let why = "exceeded block reward, only regular blocks are allowed at this height";
        for height in [start, first - 1, first + 1, first + MAINNET_CYCLE - 1] {
            for check in ALL_CHECKS {
                reward_is_the_bound(height, check, why, &p, &mut gov);
            }
        }
        // Not a superblock height before the start either: 37 * 16616 is old-budget era.
        let before = 37 * MAINNET_CYCLE;
        assert!(i64::from(before) < i64::from(p.superblock_start()));
        assert_eq!(
            value(REWARD + 1, before, SuperblockCheck::NoCheck, &p, &mut gov),
            BlockValue::Valid,
            "37 * 16616 is in an old budget window"
        );
        assert_eq!(gov.take(), []);
    }

    #[test]
    fn superblock_heights_take_every_governance_branch() {
        let p = params(Network::Main);
        for height in [first_superblock(&p), first_superblock(&p) + MAINNET_CYCLE] {
            let limit = superblock_payments_limit(height as i32, p.subsidy()).unwrap();
            assert!(limit > 0);
            let max = REWARD + limit;

            // Above the superblock maximum: invalid, whatever governance says.
            for (gov_valid, triggered) in [(false, false), (true, false), (true, true)] {
                let mut gov = Script::new(gov_valid, triggered);
                for check in ALL_CHECKS {
                    assert_eq!(
                        value(max + 1, height, check, &p, &mut gov),
                        invalid(height, max + 1, max, "exceeded superblock max value")
                    );
                }
                assert_eq!(gov.take(), []);
            }

            // Governance data not loaded: the maximum alone.
            let mut gov = Script::new(false, true);
            for check in ALL_CHECKS {
                assert_eq!(value(max, height, check, &p, &mut gov), BlockValue::Valid);
                assert_eq!(gov.take(), [Query::Valid]);
            }

            // Loaded, NoCheck: the maximum alone.
            let mut gov = Script::new(true, true);
            assert_eq!(
                value(max, height, SuperblockCheck::NoCheck, &p, &mut gov),
                BlockValue::Valid
            );
            assert_eq!(gov.take(), [Query::Valid]);

            for check in REQUIRED_CHECKS {
                // Not triggered: back to the reward.
                let mut gov = Script::new(true, false);
                let why = "exceeded block reward, no triggered superblock detected";
                reward_is_the_bound(height, check, why, &p, &mut gov);
                assert_eq!(gov.take(), [Query::Valid, Query::Triggered(height)].repeat(2));

                // Triggered: EV3's IsValidSuperblock, with the reward (not the maximum).
                let mut gov = Script::new(true, true);
                let seam = BlockValue::Ev3(Ev3Branch::ValidSuperblock {
                    block_reward: REWARD,
                    is_v24: check.is_v24(),
                });
                for actual in [0, REWARD + 1, max] {
                    assert_eq!(value(actual, height, check, &p, &mut gov), seam);
                    assert_eq!(gov.take(), [Query::Valid, Query::Triggered(height)]);
                }
            }
        }
        assert_eq!(
            invalid_superblock_error(631_408),
            "invalid superblock detected at height 631408"
        );
    }

    #[test]
    fn heights_core_has_no_int_for() {
        let p = params(Network::Main);
        let mut gov = Script::new(true, true);
        let height = u32::try_from(i32::MAX).unwrap() + 1;
        assert_eq!(
            block_value_valid(&coinbase(0), height, REWARD, SuperblockCheck::NoCheck, &p, &mut gov),
            Err(PaymentsError::Subsidy(SubsidyError::Unreachable))
        );
    }

    #[test]
    #[should_panic(expected = "GetValueOut: value out of range")]
    fn a_coinbase_check_block_rejects_panics() {
        let p = params(Network::Main);
        let mut gov = Script::new(true, true);
        let _ = value(dash_consensus::MAX_MONEY + 2, 1, SuperblockCheck::NoCheck, &p, &mut gov);
    }

    #[test]
    fn payee_below_dip3_enforcement_checks_the_superblock_only() {
        let p = params(Network::Main);
        let (subsidy, fee) = (2 * COIN, 7_000);
        let era = MnRewardEra::Classic;
        let payee = |height, check, gov: &mut Script| {
            block_payee_valid(height, subsidy, fee, era, false, check, &p, gov).unwrap()
        };
        let superblock = first_superblock(&p);
        let last_unenforced = p.dip0003_enforcement() - 1;
        assert!(superblock < last_unenforced);

        for check in ALL_CHECKS {
            // Governance data not loaded: valid, nothing else asked.
            for height in [0, superblock - 1, superblock, last_unenforced] {
                let mut gov = Script::new(false, true);
                assert_eq!(payee(height, check, &mut gov), BlockPayee::Valid);
                assert_eq!(gov.take(), [Query::Valid]);
            }
            // Loaded, below the superblock start: valid.
            for height in [0, superblock_start(&p) - 1] {
                let mut gov = Script::new(true, true);
                assert_eq!(payee(height, check, &mut gov), BlockPayee::Valid);
                assert_eq!(gov.take(), [Query::Valid]);
            }
        }

        // From the superblock start, at any height (the trigger decides, not the height).
        for height in [superblock_start(&p), superblock, last_unenforced] {
            let mut gov = Script::new(true, true);
            assert_eq!(payee(height, SuperblockCheck::NoCheck, &mut gov), BlockPayee::Valid);
            assert_eq!(gov.take(), [Query::Valid]);
            for check in REQUIRED_CHECKS {
                let mut gov = Script::new(true, false);
                assert_eq!(payee(height, check, &mut gov), BlockPayee::Valid);
                assert_eq!(gov.take(), [Query::Valid, Query::Triggered(height)]);
                let mut gov = Script::new(true, true);
                assert_eq!(
                    payee(height, check, &mut gov),
                    BlockPayee::Ev3(Ev3Branch::ValidSuperblock {
                        block_reward: subsidy + fee,
                        is_v24: check.is_v24(),
                    })
                );
                assert_eq!(gov.take(), [Query::Valid, Query::Triggered(height)]);
            }
        }
    }

    #[test]
    fn payee_from_dip3_enforcement_asks_ev3_first() {
        let p = params(Network::Main);
        let enforced = p.dip0003_enforcement();
        for (era, strict) in [(MnRewardEra::CreditPool, false), (MnRewardEra::EvoReward, true)] {
            for check in ALL_CHECKS {
                let mut gov = Script::new(true, true);
                assert_eq!(
                    block_payee_valid(enforced, 5, 6, era, strict, check, &p, &mut gov).unwrap(),
                    BlockPayee::Ev3(Ev3Branch::MasternodePayee {
                        block_subsidy: 5,
                        fee_reward: 6,
                        era,
                        strict_multiplicity: strict,
                    })
                );
                // Governance is asked only after EV3's check passed.
                assert_eq!(gov.take(), []);
                assert_eq!(
                    superblock_payee_valid(enforced, 5, 6, check, &p, &mut gov).unwrap(),
                    if check == SuperblockCheck::NoCheck {
                        BlockPayee::Valid
                    } else {
                        BlockPayee::Ev3(Ev3Branch::ValidSuperblock {
                            block_reward: 11,
                            is_v24: check.is_v24(),
                        })
                    }
                );
            }
        }
    }

    #[test]
    fn a_governance_error_stops_the_check_and_is_returned_unchanged() {
        let p = params(Network::Main);
        let height = first_superblock(&p);
        let limit = superblock_payments_limit(height as i32, p.subsidy()).unwrap();
        let check = SuperblockCheck::AllowDuplicates;
        let era = MnRewardEra::Classic;
        assert!(height < p.dip0003_enforcement());

        // The value check, at the maximum (so the bound passes): each failing query ends it there.
        for (fail_at, queries) in
            [(0, vec![Query::Valid]), (1, vec![Query::Valid, Query::Triggered(height)])]
        {
            let mut gov = Failing::new(fail_at);
            assert_eq!(
                block_value_valid(&coinbase(REWARD + limit), height, REWARD, check, &p, &mut gov),
                Err(PaymentsError::Governance(Miss(queries[fail_at])))
            );
            assert_eq!(gov.queries, queries);

            // The payee check, below DIP3 enforcement, asks the same two queries.
            let mut gov = Failing::new(fail_at);
            assert_eq!(
                block_payee_valid(height, 2 * COIN, 7_000, era, false, check, &p, &mut gov),
                Err(Miss(queries[fail_at]))
            );
            assert_eq!(gov.queries, queries);
        }

        // One query past the last: nothing fails, and the answer is the usual one.
        let mut gov = Failing::new(2);
        assert_eq!(
            block_value_valid(&coinbase(REWARD + limit), height, REWARD, check, &p, &mut gov),
            Ok(BlockValue::Ev3(Ev3Branch::ValidSuperblock {
                block_reward: REWARD,
                is_v24: false,
            }))
        );
        assert_eq!(gov.queries, [Query::Valid, Query::Triggered(height)]);
    }

    /// The boundaries on the other networks: each gate's last and first height.
    #[test]
    fn other_networks_boundaries() {
        for network in [Network::Testnet3, Network::Devnet, Network::Regtest] {
            let p = params(network);
            let budget = budget_start(&p);
            let start = superblock_start(&p);
            let superblock = first_superblock(&p);
            let check = SuperblockCheck::AllowDuplicates;
            // The answers are fixed and the queries aren't asserted, so each call gets a fresh view.
            let gov = || Script::new(true, false);
            let over = |height| value(REWARD + 1, height, check, &p, &mut gov());
            let reason = |height| match over(height) {
                BlockValue::Invalid(e) => e.rsplit(", ").next().unwrap().to_owned(),
                other => panic!("{network:?} {height}: {other:?}"),
            };
            assert_eq!(reason(budget - 1), "old budgets are not activated yet", "{network:?}");
            let window = first_window(&p);
            assert_eq!(reason(window), "old budgets are disabled", "{network:?}");
            assert_eq!(
                value(REWARD + 1, window, SuperblockCheck::NoCheck, &p, &mut gov()),
                BlockValue::Valid
            );
            assert_eq!(reason(start - 1), "block is not in old budget cycle window", "{network:?}");
            assert_eq!(reason(superblock + 1), "only regular blocks are allowed at this height");
            assert_eq!(reason(superblock), "no triggered superblock detected", "{network:?}");
            let limit = superblock_payments_limit(superblock as i32, p.subsidy()).unwrap();
            assert_eq!(
                value(REWARD + limit + 1, superblock, check, &p, &mut gov()),
                invalid(
                    superblock,
                    REWARD + limit + 1,
                    REWARD + limit,
                    "exceeded superblock max value"
                )
            );
        }
    }

    // EV3: `FindUnmatchedMasternodePayment`, `CSuperblock::IsValid` and `IsValidSuperblock`.

    fn out(value: Amount, script: u8) -> TxOut {
        TxOut {
            value,
            script_pubkey: vec![0x76, script].into(),
        }
    }

    /// A coinbase with exactly `outputs`.
    fn paying(outputs: Vec<TxOut>) -> Tx {
        Tx::new(Transaction {
            version: 3,
            tx_type: TxType::new(5),
            inputs: vec![],
            outputs,
            lock_time: 0,
            extra_payload: vec![],
        })
    }

    /// Governance data whose best superblock pays `payments` (`None`: no best superblock).
    struct Best(Option<Vec<TxOut>>, Vec<Query>);

    impl GovernanceView for Best {
        type Error = std::convert::Infallible;

        fn gov_valid(&mut self) -> Result<bool, Self::Error> {
            self.1.push(Query::Valid);
            Ok(true)
        }

        fn sb_triggered(&mut self, height: u32) -> Result<bool, Self::Error> {
            self.1.push(Query::Triggered(height));
            Ok(true)
        }

        fn sb_payments(&mut self, height: u32) -> Result<Option<Vec<TxOut>>, Self::Error> {
            self.1.push(Query::Payments(height));
            Ok(self.0.clone())
        }
    }

    // Core: masternode/payments.cpp:24-52 FindUnmatchedMasternodePayment
    #[test]
    fn unmatched_masternode_payment() {
        let (a, b) = (out(10, 1), out(20, 2));
        let only_a = std::slice::from_ref(&a);
        for strict in [false, true] {
            assert_eq!(find_unmatched_masternode_payment(&[], only_a, strict), None);
            // Order does not matter.
            let both = [a.clone(), b.clone()];
            let swapped = [b.clone(), a.clone()];
            assert_eq!(find_unmatched_masternode_payment(&both, &swapped, strict), None);
            // The first expected output not paid.
            assert_eq!(find_unmatched_masternode_payment(&both, only_a, strict), Some(1));
            assert_eq!(find_unmatched_masternode_payment(&swapped, only_a, strict), Some(0));
            // A duff or a script byte off is no payment.
            assert_eq!(
                find_unmatched_masternode_payment(only_a, &[out(9, 1), out(11, 1)], strict),
                Some(0)
            );
            assert_eq!(find_unmatched_masternode_payment(only_a, &[out(10, 3)], strict), Some(0));
            // Two outputs pay two equal expected outputs in either mode.
            assert_eq!(
                find_unmatched_masternode_payment(
                    &[a.clone(), a.clone()],
                    &[a.clone(), a.clone()],
                    strict
                ),
                None
            );
        }
        // One output pays two equal expected outputs only before V24.
        let twice = [a.clone(), a.clone()];
        assert_eq!(find_unmatched_masternode_payment(&twice, only_a, false), None);
        assert_eq!(find_unmatched_masternode_payment(&twice, only_a, true), Some(1));
        let thrice = [a.clone(), b.clone(), a.clone()];
        assert_eq!(
            find_unmatched_masternode_payment(&thrice, &[b.clone(), a.clone()], true),
            Some(2)
        );
    }

    /// `CSuperblock::IsValid`'s branches on mainnet's first superblock.
    // Core: governance/superblock.cpp:248-337 CSuperblock::IsValid
    #[test]
    fn superblock_is_valid() {
        let p = params(Network::Main);
        let h = first_superblock(&p);
        let limit = superblock_payments_limit(h as i32, p.subsidy()).unwrap();
        let (x, y) = (out(100, 1), out(200, 2));
        let payments = [x.clone(), y.clone()];
        let miner = |extra: Amount| out(REWARD + extra, 9);
        let valid = |coinbase: Vec<TxOut>, payments: &[TxOut], v24: bool| {
            superblock_valid(&paying(coinbase), h, payments, REWARD, v24, &p).unwrap()
        };
        for v24 in [false, true] {
            assert!(valid(vec![miner(0), x.clone(), y.clone()], &payments, v24));
            // Outputs between and around the payments are the miner's and the masternodes'.
            assert!(valid(vec![x.clone(), miner(-1), y.clone(), out(1, 8)], &payments, v24));
            // Not a superblock height.
            let coinbase = paying(vec![miner(0), x.clone(), y.clone()]);
            assert!(!superblock_valid(&coinbase, h + 1, &payments, REWARD, v24, &p).unwrap());
            assert!(!superblock_valid(&coinbase, h - 1, &payments, REWARD, v24, &p).unwrap());
            // Too few outputs for the payments, even when they would match.
            assert!(!valid(vec![x.clone()], &payments, v24));
            assert!(!valid(vec![x.clone()], &[x.clone(), x.clone()], v24));
            // The coinbase pays a duff more than the reward plus the payments.
            assert!(!valid(vec![miner(1), x.clone(), y.clone()], &payments, v24));
            // A payment missing, a duff short, or out of order.
            assert!(!valid(vec![miner(0), x.clone(), out(1, 1)], &payments, v24));
            assert!(!valid(vec![miner(1), x.clone(), out(199, 2)], &payments, v24));
            assert!(!valid(vec![miner(0), y.clone(), x.clone()], &payments, v24));
            // The payments may total the limit, not a duff more.
            let at = [out(limit - 1, 1), out(1, 2)];
            assert!(valid(vec![miner(0), at[0].clone(), at[1].clone()], &at, v24));
            let over = [out(limit, 1), out(1, 2)];
            assert!(!valid(vec![miner(-1), over[0].clone(), over[1].clone()], &over, v24));
            // As many outputs as payments is enough (`nOutputs - nPayments < 0` fails only on
            // fewer): the coinbase pays exactly the trigger.
            assert!(valid(vec![x.clone(), y.clone()], &payments, v24));
            assert!(valid(vec![x.clone()], std::slice::from_ref(&x), v24));
            // No payments: the reward bound.
            assert!(valid(vec![miner(0)], &[], v24));
            assert!(!valid(vec![miner(1)], &[], v24));
        }
        // Two equal payments: one output pays both before V24 (the scan restarts at the matched
        // output), each needs its own from V24.
        let twice = [x.clone(), x.clone()];
        assert!(valid(vec![miner(100), x.clone()], &twice, false));
        assert!(!valid(vec![miner(100), x.clone()], &twice, true));
        assert!(valid(vec![miner(0), x.clone(), x.clone()], &twice, true));
        // An equal payment after another one does not reuse the earlier output either way.
        let xyx = [x.clone(), y.clone(), x.clone()];
        assert!(!valid(vec![miner(100), x.clone(), y.clone()], &xyx, false));
        assert!(valid(vec![miner(0), x.clone(), y.clone(), x.clone()], &xyx, true));
        // A height Core's `int` cannot hold has no answer.
        assert_eq!(
            superblock_valid(&paying(vec![]), u32::MAX, &[], REWARD, true, &p),
            Err(SubsidyError::Unreachable)
        );
    }

    // Core: governance/superblock.cpp:622-631 SuperblockManager::IsValidSuperblock
    #[test]
    fn valid_superblock_reads_the_best_superblock() {
        let p = params(Network::Main);
        let h = first_superblock(&p);
        let x = out(100, 1);
        let coinbase = paying(vec![out(REWARD, 9), x.clone()]);
        let mut gov = Best(None, vec![]);
        assert_eq!(valid_superblock(&coinbase, h, REWARD, true, &p, &mut gov), Ok(false));
        assert_eq!(gov.1, [Query::Payments(h)]);
        let mut gov = Best(Some(vec![x.clone()]), vec![]);
        assert_eq!(valid_superblock(&coinbase, h, REWARD, true, &p, &mut gov), Ok(true));
        let mut gov = Best(Some(vec![x, out(1, 1)]), vec![]);
        assert_eq!(valid_superblock(&coinbase, h, REWARD, true, &p, &mut gov), Ok(false));
        // A failing query stops the check with its error.
        let mut failing = Failing::new(0);
        assert_eq!(
            valid_superblock(&coinbase, h, REWARD, true, &p, &mut failing),
            Err(PaymentsError::Governance(Miss(Query::Payments(h))))
        );
    }
}
