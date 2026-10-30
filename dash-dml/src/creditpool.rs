//! The credit pool (Core `evo/creditpool.{h,cpp}`, `util/ranges_set.{h,cpp}` and
//! `CheckCreditPoolDiffForBlock` in `evo/specialtxman.cpp`, at v24.0.0-rc.4; EV3-2).
//!
//! - [`RangesSet`]: the used asset-unlock indexes, with v24's wrap-around successor (CONS-10).
//! - [`credit_block_data`] (`GetCreditDataFromBlock`) and [`construct_pool`]
//!   (`ConstructCreditPool`): the pool after a block, from the parent's pool, the block and the
//!   window start's data, in the three limit eras (D13).
//! - [`CreditPoolDiff`] and [`check_block_diff`]: a block's asset locks and unlocks against the
//!   parent's pool, and the CbTx balance (`bad-cbtx-assetlocked-amount`).
//! - The store side (D8, D9): [`connect_records`] builds what connecting a block writes, and
//!   [`pool_after`], [`credit_data_at`], [`balance_at`] and [`credit_pool_info`] read it back.
//!
//! Core builds the pool lazily at its next read and caches it (`creditpool.cpp:122-171, 256-275`);
//! the node builds it when the block connects and stores it, or the exception Core's construction
//! throws (D8). Core re-reads the window start's block from disk (`creditpool.cpp:64-114`); the
//! node stores each block's [`CreditBlockData`] when it connects (D9).
//!
//! Arithmetic is Core's signed `int64`. rc.4 checks the per-block diff's sums (CE4-05): an asset
//! unlock's amount, the session's locks and unlocks, and the total the CbTx must carry. The sums
//! rc.4 leaves unchecked (the block's unlocked amount and the pool construction) wrap here where
//! Core's would overflow (undefined behaviour); no checked chain reaches them (EV3 D6).

mod ranges;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::fmt;

use dash_consensus::payments::{
    MnRewardEra, PaymentError, PaymentParams, masternode_payment, platform_share,
};
use dash_consensus::primitives::ReadErrorKind;
use dash_consensus::script::opcodes::OP_RETURN;
use dash_consensus::subsidy::COIN;
use dash_consensus::{
    Amount, Block, BlockValidationError, BlockValidationResult, Tx, TxRef, TxValidationError,
    TxValidationResult, money_range,
};
use dash_primitives::{BlockHash, SpecialPayload, TxType};

pub use ranges::{RangesDecodeError, RangesSet};

use crate::store::{EvoStore, StoreError};

/// Core's `CCreditPool` after a block.
// Core: evo/creditpool.h:39-58 CCreditPool
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreditPool {
    /// `locked`: the balance, `getcreditpoolinfo`'s `balance`.
    pub locked: Amount,
    /// `currentLimit`: what may be unlocked in the next block.
    pub current_limit: Amount,
    /// `latelyUnlocked`: what was unlocked in the window, `getcreditpoolinfo`'s
    /// `window.unlocked`.
    pub lately_unlocked: Amount,
    /// `indexes`: the asset-unlock indexes already used.
    pub indexes: RangesSet,
}

impl CreditPool {
    /// Core's serialization: the three amounts as little-endian `int64`, then the indexes.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend(self.locked.to_le_bytes());
        out.extend(self.current_limit.to_le_bytes());
        out.extend(self.lately_unlocked.to_le_bytes());
        self.indexes.encode(out);
    }

    /// Core's deserialization, with v24's strict [`RangesSet::decode`].
    pub fn decode(input: &mut &[u8]) -> Result<Self, RangesDecodeError> {
        Ok(Self {
            locked: read_i64(input)?,
            current_limit: read_i64(input)?,
            lately_unlocked: read_i64(input)?,
            indexes: RangesSet::decode(input)?,
        })
    }
}

fn read_i64(input: &mut &[u8]) -> Result<i64, ReadErrorKind> {
    let (head, rest) = input.split_first_chunk::<8>().ok_or(ReadErrorKind::EndOfData)?;
    *input = rest;
    Ok(i64::from_le_bytes(*head))
}

/// The exceptions out of `ConstructCreditPool` and `GetCreditDataFromBlock`, with Core's
/// `what()` (D8). Each is a state the node stores for the block, never a verdict on it: Core's
/// next read of the pool throws it, and the child fails there (`failed-procspectxsinblock`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolError {
    /// The block has no credit data, but the parent's pool has a balance.
    // Core: evo/creditpool.cpp:179-181
    PrevLocked,
    /// The block has no credit data, but the parent's pool has used indexes.
    // Core: evo/creditpool.cpp:182-185
    PrevIndexes,
    /// One of the block's unlock indexes is already used.
    // Core: evo/creditpool.cpp:197-199
    IndexDuplicated,
    /// The limit came out negative (only the windowed pre-WITHDRAWALS rule can do that).
    // Core: evo/creditpool.cpp:238-241
    NegativeLimit(Amount),
    /// An asset unlock of the block fails `GetDataFromUnlockTx`, with its reason.
    // Core: evo/creditpool.cpp:104-106
    UnlockData(UnlockDataReason),
}

/// `GetDataFromUnlockTx`'s reject reasons: a closed set, so a stored [`PoolError`] has a total
/// codec (BL-259).
// Core: evo/creditpool.cpp:33-53 GetDataFromUnlockTx
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnlockDataReason {
    /// The payload does not decode completely.
    Payload,
    /// An output's value is outside `MoneyRange`.
    TxoutOutOfRange,
    /// The fee plus the outputs overflows `int64`.
    Overflow,
}

impl UnlockDataReason {
    /// Core's reject reason.
    pub fn reject_reason(self) -> &'static str {
        match self {
            Self::Payload => "failed-creditpool-unlock-payload",
            Self::TxoutOutOfRange => "failed-creditpool-unlock-txout-outofrange",
            Self::Overflow => "failed-creditpool-unlock-overflow",
        }
    }
}

impl fmt::Display for UnlockDataReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reject_reason())
    }
}

impl fmt::Display for PoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PrevLocked => {
                f.write_str("Failed to create CreditPool but previous block has value")
            }
            Self::PrevIndexes => f.write_str(
                "Failed to create CreditPool but asset unlock transactions already mined",
            ),
            Self::IndexDuplicated => {
                f.write_str("ConstructCreditPool: failed-getcreditpool-index-duplicated")
            }
            Self::NegativeLimit(limit) => {
                // Core's text ends in a newline.
                writeln!(f, "Negative limit for CreditPool: {}.{:08}", limit / COIN, limit % COIN)
            }
            Self::UnlockData(reason) => {
                write!(f, "GetCreditDataFromBlock: GetDataFromUnlockTx failed: {reason}")
            }
        }
    }
}

impl std::error::Error for PoolError {}

/// The pool after a block as the store keeps it (D8): the pool, or the exception Core's
/// construction throws.
pub type PoolState = Result<CreditPool, PoolError>;

/// `LimitAmountLow`.
pub const LIMIT_AMOUNT_LOW: Amount = 100 * COIN;
/// `LimitAmountHigh`.
pub const LIMIT_AMOUNT_HIGH: Amount = 1000 * COIN;
/// `LimitAmountV22`: the WITHDRAWALS era's limit.
pub const LIMIT_AMOUNT_V22: Amount = 2000 * COIN;
/// `MaxUnlockDropPercentV24`.
pub const MAX_UNLOCK_DROP_PERCENT_V24: Amount = 20;
/// `MinUnlockDropV24`.
pub const MIN_UNLOCK_DROP_V24: Amount = 2000 * COIN;

/// `UnlockLimitV24(balance, balance_window_start)`: what the v24 rule lets the next block unlock.
/// Core's `static_assert` bounds the product for any CbTx balance within the money supply; a
/// balance beyond it wraps here.
// Core: evo/creditpool.h:148-158 CCreditPoolManager::UnlockLimitV24
pub const fn unlock_limit_v24(balance: Amount, balance_window_start: Amount) -> Amount {
    let mut allowed_drop = balance_window_start.wrapping_mul(MAX_UNLOCK_DROP_PERCENT_V24) / 100;
    if allowed_drop < MIN_UNLOCK_DROP_V24 {
        allowed_drop = MIN_UNLOCK_DROP_V24;
    }
    let mut limit = allowed_drop.wrapping_sub(balance_window_start.wrapping_sub(balance));
    if limit < 0 {
        limit = 0;
    }
    if limit > balance {
        balance
    } else {
        limit
    }
}

/// What the store keeps per block (D9): the CbTx's credit-pool balance and the amount the block's
/// asset unlocks release (fees included). The window start's reads (`ConstructCreditPool`,
/// `GetBalanceAt`) treat a block without data as zeros, so a DIP0003 block whose coinbase carries
/// no valid CbTx is kept as the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CreditBlockData {
    /// `credit_pool`: the CbTx's `creditPoolBalance`.
    pub balance: Amount,
    /// `unlocked`.
    pub unlocked: Amount,
}

/// `GetCreditDataFromBlock`'s answer: the block's data and its unlock indexes, which only the
/// block's own pool construction reads (they are not stored, D9).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockCredit {
    pub data: CreditBlockData,
    /// The asset-unlock indexes, sorted and deduplicated (Core's `unordered_set`, D10).
    pub indexes: Vec<u64>,
}

/// `GetDataFromUnlockTx`: an asset unlock's amount (its payload's fee plus its outputs) and
/// index, or Core's reject reason.
// Core: evo/creditpool.cpp:33-53 GetDataFromUnlockTx
fn unlock_data(tx: &Tx) -> Result<(Amount, u64), UnlockDataReason> {
    let Some(Ok(decoded)) = tx.payload() else {
        return Err(UnlockDataReason::Payload);
    };
    let SpecialPayload::AssetUnlock(payload) = &decoded.payload else {
        return Err(UnlockDataReason::Payload);
    };
    if !decoded.is_complete() {
        return Err(UnlockDataReason::Payload);
    }
    let mut to_unlock = Amount::from(payload.fee);
    for txout in tx.outputs() {
        if !money_range(txout.value) {
            return Err(UnlockDataReason::TxoutOutOfRange);
        }
        to_unlock = to_unlock.checked_add(txout.value).ok_or(UnlockDataReason::Overflow)?;
    }
    Ok((to_unlock, payload.index))
}

fn is_asset_unlock(tx: &Tx) -> bool {
    tx.is_special_tx_version() && tx.tx_type() == TxType::AssetUnlock
}

/// `GetCreditDataFromBlock(block)`: `None` before DIP0003, or when the coinbase carries no CbTx
/// payload that decodes completely (its type is not checked); `Err` where an asset unlock fails
/// `GetDataFromUnlockTx` (Core throws).
// Core: evo/creditpool.cpp:64-114 GetCreditDataFromBlock
pub fn credit_block_data(
    block: &Block,
    dip3_active_at: bool,
) -> Result<Option<BlockCredit>, PoolError> {
    // There's no CbTx before DIP0003 activation
    if !dip3_active_at {
        return Ok(None);
    }
    let Some(coinbase) = block.txs().first() else {
        return Ok(None);
    };
    if coinbase.extra_payload().is_empty() || !coinbase.is_special_tx_version() {
        // Core logs "empty CbTx for CreditPool"
        return Ok(None);
    }
    // `GetTxPayload<CCbTx>(vExtraPayload)`: the bytes alone, whatever the coinbase's type.
    let mut payload = coinbase.extra_payload();
    let balance = match SpecialPayload::decode(TxType::CoinbaseCommitment, &mut payload) {
        Ok(SpecialPayload::CoinbaseCommitment(cbtx)) if payload.is_empty() => {
            cbtx.credit_pool_balance.unwrap_or(0)
        }
        // Core logs "No valid CbTx"
        _ => return Ok(None),
    };

    let mut unlocked: Amount = 0;
    let mut indexes = BTreeSet::new();
    for tx in block.txs().iter().filter(|tx| is_asset_unlock(tx)) {
        let (amount, index) = unlock_data(tx).map_err(PoolError::UnlockData)?;
        unlocked = unlocked.wrapping_add(amount);
        indexes.insert(index);
    }
    Ok(Some(BlockCredit {
        data: CreditBlockData {
            balance,
            unlocked,
        },
        indexes: indexes.into_iter().collect(),
    }))
}

/// The limit era of the block being built (D13): `DeploymentActiveAt(block, V24)` and
/// `DeploymentActiveAt(block, WITHDRAWALS)`. The caller decides them; EV3 never derives
/// deployments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PoolEras {
    pub v24: bool,
    pub withdrawals: bool,
}

/// `ConstructCreditPool(block, prev)`: the pool after a block where V20 is active, from the
/// parent's pool, the block's credit data and the window start's (the block
/// `CreditPoolPeriodBlocks()` below it, `None` when the chain is shorter or the block has no
/// data).
// Core: evo/creditpool.cpp:173-247 CCreditPoolManager::ConstructCreditPool
pub fn construct_pool(
    prev: &CreditPool,
    block: Option<&BlockCredit>,
    window_start: Option<&CreditBlockData>,
    eras: PoolEras,
) -> Result<CreditPool, PoolError> {
    let Some(block) = block else {
        // If reading of previous block is not successfully, but
        // prev contains credit pool related data, something strange happened
        if prev.locked != 0 {
            return Err(PoolError::PrevLocked);
        }
        if !prev.indexes.is_empty() {
            return Err(PoolError::PrevIndexes);
        }
        return Ok(CreditPool::default());
    };

    // Indexes should not be duplicated since genesis block, but the Unlock Amount
    // of withdrawal transaction is limited only by this window
    let mut indexes = prev.indexes.clone();
    if block.indexes.iter().any(|&index| !indexes.add(index)) {
        return Err(PoolError::IndexDuplicated);
    }

    let distant = window_start.copied().unwrap_or_default();
    let balance = block.data.balance;
    let mut current_limit = balance;
    let lately_unlocked =
        prev.lately_unlocked.wrapping_add(block.data.unlocked).wrapping_sub(distant.unlocked);
    if eras.v24 {
        current_limit = unlock_limit_v24(balance, distant.balance);
    } else if eras.withdrawals {
        current_limit = current_limit.min(LIMIT_AMOUNT_V22);
    } else {
        // Unlock limits in pre-v22 are max(100, min(.10 * assetlockpool, 1000)) inside window
        if current_limit.wrapping_add(lately_unlocked) > LIMIT_AMOUNT_LOW {
            current_limit = LIMIT_AMOUNT_LOW.max(balance / 10).wrapping_sub(lately_unlocked);
            if current_limit < 0 {
                current_limit = 0;
            }
        }
        current_limit = current_limit.min(LIMIT_AMOUNT_HIGH.wrapping_sub(lately_unlocked));
    }

    if current_limit < 0 {
        return Err(PoolError::NegativeLimit(current_limit));
    }

    Ok(CreditPool {
        locked: balance,
        current_limit,
        lately_unlocked,
        indexes,
    })
}

/// `CCreditPoolDiff`'s `platformReward` (D14): when MN_RR is active after the parent, the
/// platform's share of the masternode payment for the subsidy alone, else 0. `Err` where Core's
/// `PlatformShare` asserts (a node fault, D6).
// Core: evo/creditpool.cpp:291-294
pub fn platform_reward(
    mn_rr_active_after_prev: bool,
    height: i32,
    block_subsidy: Amount,
    p: &PaymentParams,
) -> Result<Amount, PaymentError> {
    if !mn_rr_active_after_prev {
        return Ok(0);
    }
    platform_share(masternode_payment(height, block_subsidy, MnRewardEra::EvoReward, p))
}

fn tx_reject(reason: &'static str) -> TxValidationError {
    TxValidationError::new(TxValidationResult::Consensus, reason)
}

/// Core's `CCreditPoolDiff`: one block's (or one block template's) asset locks and unlocks against
/// the parent's pool. The limits and the indexes already used stay the parent's.
///
/// No exception reaches Core's catch in the per-transaction step (`failed-procassetlocksinblock`,
/// `creditpool.cpp:365-368`) or around the block (`failed-getcreditpooldiff`, `:421-424`):
/// `GetTxPayload` catches its own, and the parent's pool was read, or failed, before (D8, D12).
// Core: evo/creditpool.h:71-114, evo/creditpool.cpp:285-395
#[derive(Clone, Debug)]
pub struct CreditPoolDiff {
    pool: CreditPool,
    new_indexes: BTreeSet<u64>,
    session_locked: Amount,
    session_unlocked: Amount,
    platform_reward: Amount,
}

impl CreditPoolDiff {
    /// `CCreditPoolDiff(starter, pindexPrev, …)`, with [`platform_reward`]'s answer.
    pub fn new(starter: CreditPool, platform_reward: Amount) -> Self {
        Self {
            pool: starter,
            new_indexes: BTreeSet::new(),
            session_locked: 0,
            session_unlocked: 0,
            platform_reward,
        }
    }

    /// The parent's pool.
    pub fn pool(&self) -> &CreditPool {
        &self.pool
    }

    /// `GetTotalLocked()`: the balance the block's CbTx must carry, `None` where a step
    /// overflows (Core throws `std::overflow_error`). The unlocks are subtracted first.
    // Core: evo/creditpool.cpp:297-305 CCreditPoolDiff::GetTotalLocked
    pub fn total_locked(&self) -> Option<Amount> {
        // `CheckedAdd(locked, -sessionUnlocked)`: the session's unlocks are never negative.
        self.pool
            .locked
            .checked_sub(self.session_unlocked)
            .and_then(|total| total.checked_add(self.session_locked))
            .and_then(|total| total.checked_add(self.platform_reward))
    }

    // Core: evo/creditpool.cpp:307-325 CCreditPoolDiff::Lock
    fn lock(&mut self, tx: &Tx) -> Result<(), TxValidationError> {
        let is_lock = matches!(
            tx.payload(),
            Some(Ok(d)) if d.is_complete() && matches!(d.payload, SpecialPayload::AssetLock(_))
        );
        if !is_lock {
            return Err(tx_reject("failed-creditpool-lock-payload"));
        }

        match tx.outputs().iter().find(|o| o.script_pubkey.as_bytes().first() == Some(&OP_RETURN)) {
            Some(txout) => {
                self.session_locked = self
                    .session_locked
                    .checked_add(txout.value)
                    .ok_or_else(|| tx_reject("failed-creditpool-lock-overflow"))?;
                Ok(())
            }
            None => Err(tx_reject("failed-creditpool-lock-invalid")),
        }
    }

    // Core: evo/creditpool.cpp:327-349 CCreditPoolDiff::Unlock
    fn unlock(&mut self, tx: &Tx) -> Result<u64, TxValidationError> {
        let (to_unlock, index) =
            unlock_data(tx).map_err(|reason| tx_reject(reason.reject_reason()))?;

        let total = self
            .session_unlocked
            .checked_add(to_unlock)
            .filter(|&total| total <= self.pool.current_limit)
            .ok_or_else(|| tx_reject("failed-creditpool-unlock-too-much"))?;

        if self.pool.indexes.contains(index) || self.new_indexes.contains(&index) {
            return Err(tx_reject("failed-creditpool-unlock-duplicated-index"));
        }

        self.new_indexes.insert(index);
        self.session_unlocked = total;
        Ok(index)
    }

    /// `ProcessLockUnlockTransaction(tx)`: `Ok` with the unlock index recorded, if any, when the
    /// transaction fits the block; else Core's `TX_CONSENSUS` reject. Transactions that are not
    /// special, asset locks or asset unlocks change nothing.
    // Core: evo/creditpool.cpp:351-369 CCreditPoolDiff::ProcessLockUnlockTransaction
    pub fn process_lock_unlock_transaction(
        &mut self,
        tx: &Tx,
    ) -> Result<Option<u64>, TxValidationError> {
        if !tx.is_special_tx_version() {
            return Ok(None);
        }
        match tx.tx_type() {
            TxType::AssetLock => self.lock(tx).map(|()| None),
            TxType::AssetUnlock => self.unlock(tx).map(Some),
            _ => Ok(None),
        }
    }

    /// `ProcessLockUnlockTransactions(txs)`: a package, all or nothing. On a reject the amounts
    /// and the indexes this call added are rolled back; earlier calls' stay (MP1a's form, D12).
    // Core: evo/creditpool.cpp:371-395 CCreditPoolDiff::ProcessLockUnlockTransactions
    pub fn process_lock_unlock_transactions(
        &mut self,
        txs: &[TxRef],
    ) -> Result<(), TxValidationError> {
        let initial_locked = self.session_locked;
        let initial_unlocked = self.session_unlocked;
        let mut inserted_indexes = Vec::new();

        for tx in txs {
            match self.process_lock_unlock_transaction(tx) {
                Ok(inserted) => inserted_indexes.extend(inserted),
                Err(e) => {
                    // Roll back exactly what this invocation changed: amounts are scalar
                    // snapshots, and only the indexes inserted above are erased so that
                    // state committed by earlier packages is left untouched.
                    for index in &inserted_indexes {
                        self.new_indexes.remove(index);
                    }
                    self.session_locked = initial_locked;
                    self.session_unlocked = initial_unlocked;
                    return Err(e);
                }
            }
        }
        Ok(())
    }
}

/// `GetCreditPoolDiffForBlock`: every transaction after the coinbase, in order, against the
/// parent's pool; the first reject becomes the block's (`BLOCK_CONSENSUS`, Core's debug text).
// Core: evo/creditpool.cpp:397-426 GetCreditPoolDiffForBlock
pub fn credit_pool_diff_for_block(
    block: &Block,
    prev_pool: &CreditPool,
    platform_reward: Amount,
) -> Result<CreditPoolDiff, BlockValidationError> {
    let mut diff = CreditPoolDiff::new(prev_pool.clone(), platform_reward);
    for tx in block.txs().iter().skip(1) {
        if let Err(e) = diff.process_lock_unlock_transaction(tx) {
            let debug = format!(
                "Process Lock/Unlock Transaction failed at Credit Pool (tx hash {}) {}",
                tx.txid(),
                e.debug_message()
            );
            return Err(e.into_block(BlockValidationResult::Consensus).with_debug(debug));
        }
    }
    Ok(diff)
}

/// `CheckCreditPoolDiffForBlock`, which `ProcessSpecialTxsInBlock` runs when it read the CbTx
/// (`fCheckCbTxMerkleRoots` and a coinbase of type `TRANSACTION_COINBASE`): before DIP0008 or V20
/// at the block nothing is checked (`Ok(None)`); else the block's diff against the parent's pool,
/// whose total must be the CbTx's `creditPoolBalance`. `Ok(Some(total))` when it is. A total that
/// overflows is `failed-checkcreditpooldiff` (Core's catch of `GetTotalLocked`'s throw).
// Core: evo/specialtxman.cpp:1096-1127 CSpecialTxProcessor::CheckCreditPoolDiffForBlock
pub fn check_block_diff(
    block: &Block,
    cbtx_balance: Amount,
    prev_pool: &CreditPool,
    platform_reward: Amount,
    dip8_active_at: bool,
    v20_active_at: bool,
) -> Result<Option<Amount>, BlockValidationError> {
    if !dip8_active_at || !v20_active_at {
        return Ok(None);
    }
    let diff = credit_pool_diff_for_block(block, prev_pool, platform_reward)?;
    // But it maybe not included yet in previous block yet; in this case value must be 0
    let Some(locked_calculated) = diff.total_locked() else {
        return Err(BlockValidationError::new(
            BlockValidationResult::Consensus,
            "failed-checkcreditpooldiff",
        ));
    };
    if cbtx_balance != locked_calculated {
        return Err(BlockValidationError::new(
            BlockValidationResult::Consensus,
            "bad-cbtx-assetlocked-amount",
        ));
    }
    Ok(Some(locked_calculated))
}

/// Where a block stands, for [`connect_records`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConnectAt {
    /// `DeploymentActiveAt(block, DIP0003)`.
    pub dip3: bool,
    /// `DeploymentActiveAt(block, V20)`.
    pub v20: bool,
    pub eras: PoolEras,
}

/// What connecting a block writes (ask A-1): its credit data where DIP0003 is active (D9) and
/// the pool after it where V20 is (D8).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreditRecords {
    pub data: Option<CreditBlockData>,
    pub pool: Option<PoolState>,
}

/// The records of `block`, given the pool after its parent ([`pool_after`], which is empty
/// where V20 is not active at the parent) and the window start's data ([`credit_data_at`], `None`
/// when the chain is shorter than the window).
///
/// Where `GetCreditDataFromBlock` throws, the pool is that error and no data is written: Core
/// would throw again at every read of the block, and no checked chain holds such a block (an
/// asset unlock's payload and outputs are checked before, `bad-assetunlocktx-payload`,
/// `bad-txns-vout-*`).
pub fn connect_records(
    block: &Block,
    at: ConnectAt,
    prev: &CreditPool,
    window_start: Option<&CreditBlockData>,
) -> CreditRecords {
    let read = credit_block_data(block, at.dip3);
    let data = match &read {
        Ok(Some(credit)) => Some(credit.data),
        Ok(None) => at.dip3.then(CreditBlockData::default),
        Err(_) => None,
    };
    let pool = at.v20.then(|| {
        read.and_then(|credit| construct_pool(prev, credit.as_ref(), window_start, at.eras))
    });
    CreditRecords {
        data,
        pool,
    }
}

/// `GetCreditPool(block)` over the store (D8): the empty pool where V20 is not active at `block`,
/// else the state stored when it connected. A missing record is a store fault.
// Core: evo/creditpool.cpp:122-124 GetFromCache, :256-275 GetCreditPool
pub fn pool_after(
    store: &dyn EvoStore,
    block: &BlockHash,
    v20_active_at: bool,
) -> Result<PoolState, StoreError> {
    if !v20_active_at {
        return Ok(Ok(CreditPool::default()));
    }
    match store.credit_pool(block)? {
        Some(state) => Ok((*state).clone()),
        None => Err(StoreError::inconsistent("no credit pool for a V20 block")),
    }
}

/// `GetCreditDataFromBlock(block)` as the window start's read takes it (D9): zeros before
/// DIP0003, else the stored data. A missing record is a store fault.
// Core: evo/creditpool.cpp:201-212
pub fn credit_data_at(
    store: &dyn EvoStore,
    block: &BlockHash,
    dip3_active_at: bool,
) -> Result<CreditBlockData, StoreError> {
    if !dip3_active_at {
        return Ok(CreditBlockData::default());
    }
    store
        .credit_data(block)?
        .ok_or_else(|| StoreError::inconsistent("no credit data for a DIP0003 block"))
}

/// `GetBalanceAt(block)`: the CbTx balance after `block`, 0 without data.
// Core: evo/creditpool.cpp:249-254 CCreditPoolManager::GetBalanceAt
pub fn balance_at(
    store: &dyn EvoStore,
    block: &BlockHash,
    dip3_active_at: bool,
) -> Result<Amount, StoreError> {
    Ok(credit_data_at(store, block, dip3_active_at)?.balance)
}

/// A block the reads name: its hash and where it stands.
#[derive(Clone, Copy, Debug)]
pub struct BlockAt<'a> {
    pub hash: &'a BlockHash,
    pub height: u32,
    pub dip3: bool,
    pub v20: bool,
}

/// `getcreditpoolinfo`'s data (D24).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreditPoolInfo {
    /// The pool after the block.
    pub pool: CreditPool,
    /// `window.blocks`: `CreditPoolPeriodBlocks()`.
    pub window_blocks: i32,
    /// `window.height` (none for Core's -1) and `window.balance`.
    pub window_start: Option<(u32, Amount)>,
}

/// `getcreditpoolinfo` for `at`, whose window start is `window_start` (the ancestor
/// `window_blocks` below it, `None` when the chain is shorter): `Ok(Err(_))` where Core's
/// `GetCreditPool` throws, which the RPC reports.
// Core: rpc/blockchain.cpp:1789-1823 getcreditpoolinfo
pub fn credit_pool_info(
    store: &dyn EvoStore,
    at: BlockAt<'_>,
    window_start: Option<BlockAt<'_>>,
    window_blocks: i32,
) -> Result<Result<CreditPoolInfo, PoolError>, StoreError> {
    let pool = match pool_after(store, at.hash, at.v20)? {
        Ok(pool) => pool,
        Err(e) => return Ok(Err(e)),
    };
    let window_start = match window_start {
        Some(w) => Some((w.height, balance_at(store, w.hash, w.dip3)?)),
        None => None,
    };
    Ok(Ok(CreditPoolInfo {
        pool,
        window_blocks,
        window_start,
    }))
}
