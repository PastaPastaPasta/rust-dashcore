//! Core's per-block coin statistics (CH3 §2.15, D17, D48): what the coinstats index accumulates
//! for each block it appends (`CoinStatsIndex::CustomAppend`) and how it takes one back
//! (`ReverseBlock`), the `block_info` that `gettxoutsetinfo` reports from two consecutive entries,
//! the seven `getblockstats` fields of O8's golden `stats`, and `hash_serialized_2` over a UTXO set.
//!
//! Inputs are a block, its undo record and its subsidy (`GetBlockSubsidy`, CS2's
//! [`crate::subsidy::block_subsidy`]); nothing here reads a coins view. The connector's digest tap
//! (CH3a) and the coinstats index (IX) share these functions, so the two cannot drift.
//!
//! - **Genesis.** Core never connects the genesis block, so its outputs are never coins. The index
//!   counts the genesis subsidy as unspendable (`total_unspendables_genesis_block`) and adds
//!   nothing to the hash or the output count. `getblockstats` answers for it with an empty undo
//!   record (`GetUndoChecked`): its outputs count and size, as a coinbase's.
//! - **BIP30.** Bitcoin's index skips its two duplicate coinbases (`IsBIP30Unspendable`); Dash's
//!   has no such branch, so `total_unspendables_bip30` stays 0. It is kept because Core's index
//!   entry (`DBVal`) stores it.
//! - **Unclaimed rewards** are what the running totals leave over: spent prevouts plus subsidies
//!   minus new outputs, coinbase outputs and unspendables. On a chain without platform transfers
//!   that is the block's subsidy plus fees minus its coinbase's value; an asset unlock creates
//!   outputs without spending any, and the index counts them as Core does.
//! - **Arithmetic.** Core's counters are `uint64_t` and its amounts `CAmount`. Their sums wrap here
//!   as in the release binaries (signed overflow is undefined in C++; no chain comes near it).

use std::fmt;

use dash_primitives::{BlockHash, OutPoint};

use super::coin::{Coin, CoinRef};
use super::compress::{MAX_SPECIAL_SCRIPT_LEN, is_unspendable};
use super::muhash::MuHash3072;
use super::undo::{BlockUndo, BlockUndoRef};
use super::utxo_hash::HashSerialized2;
use crate::primitives::compact_size_len;
use crate::tx::inputs::asset_unlock_payload_fee;
use crate::{Amount, Block, money_range};

/// `PER_UTXO_OVERHEAD`: `sizeof(COutPoint) + sizeof(uint32_t) + sizeof(bool)` = 36 + 4 + 1.
// Core: rpc/blockchain.cpp:2209
const PER_UTXO_OVERHEAD: i64 = 41;

/// A block's undo record as Core's `CBlockUndo`: one list of spent coins per transaction after the
/// coinbase, each in input order. [`BlockUndo`] and [`BlockUndoRef`] are both one.
pub trait SpentCoins {
    /// `vtxundo.size()`.
    fn tx_count(&self) -> usize;

    /// `vtxundo[tx].vprevout`. `tx` is below [`SpentCoins::tx_count`].
    fn coins(&self, tx: usize) -> impl ExactSizeIterator<Item = CoinRef<'_>>;
}

impl SpentCoins for BlockUndo {
    fn tx_count(&self) -> usize {
        self.txs.len()
    }

    fn coins(&self, tx: usize) -> impl ExactSizeIterator<Item = CoinRef<'_>> {
        self.txs[tx].coins.iter().map(Coin::as_ref)
    }
}

impl SpentCoins for BlockUndoRef<'_> {
    fn tx_count(&self) -> usize {
        self.txs.len()
    }

    fn coins(&self, tx: usize) -> impl ExactSizeIterator<Item = CoinRef<'_>> {
        self.txs[tx].coins.iter().copied()
    }
}

/// A block and an undo record the statistics cannot be computed from. Core computes them only for
/// a connected block and the undo record it wrote for it, so neither happens there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatsError {
    /// The record does not have one entry per transaction after the coinbase, each with one coin
    /// per input: it is not this block's. Core's index would read past it (`vtxundo.at` throws).
    UndoMismatch,
    /// The first transaction is not the block's only coinbase (`CheckBlock` rejects such a block;
    /// Core's index pairs `vtxundo[i - 1]` with transaction `i`).
    CoinbasePosition,
    /// `getblockstats`: the asset unlock at this index has no decodable payload
    /// (`CHECK_NONFATAL(payload)`).
    AssetUnlockPayload {
        tx: usize,
    },
    /// `getblockstats`: the fee of the transaction at this index is outside the money range
    /// (`CHECK_NONFATAL(MoneyRange(txfee))`).
    FeeOutOfRange {
        tx: usize,
    },
}

impl fmt::Display for StatsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UndoMismatch => f.write_str("the undo record is not the block's"),
            Self::CoinbasePosition => {
                f.write_str("the coinbase is not the first transaction alone")
            }
            Self::AssetUnlockPayload {
                tx,
            } => write!(f, "transaction {tx}: an asset unlock without a decodable payload"),
            Self::FeeOutOfRange {
                tx,
            } => write!(f, "transaction {tx}: fee outside the money range"),
        }
    }
}

impl std::error::Error for StatsError {}

/// Checks that `undo` can be paired with `block` as Core's index pairs them: the coinbase first
/// and alone, then one undo entry per transaction with one coin per input.
fn check_pairing(block: &Block, undo: &impl SpentCoins) -> Result<(), StatsError> {
    let txs = block.txs();
    if txs.iter().enumerate().any(|(i, tx)| tx.is_coinbase() != (i == 0)) {
        return Err(StatsError::CoinbasePosition);
    }
    if undo.tx_count() + 1 != txs.len() {
        return Err(StatsError::UndoMismatch);
    }
    if txs.iter().skip(1).enumerate().any(|(i, tx)| undo.coins(i).len() != tx.inputs().len()) {
        return Err(StatsError::UndoMismatch);
    }
    Ok(())
}

/// `CScript::size()` of a coin's script as Core holds it (a compressed form expanded).
fn script_len(coin: &CoinRef<'_>) -> usize {
    let mut buf = [0; MAX_SPECIAL_SCRIPT_LEN];
    coin.script_pubkey().expand(&mut buf).len()
}

/// `GetBogoSize(scriptPubKey)`: 32 (txid) + 4 (index) + 4 (height and coinbase) + 8 (amount)
/// + 2 (script length) + the script.
// Core: kernel/coinstats.cpp:41-49 GetBogoSize
fn bogo_size(script_len: usize) -> u64 {
    50 + script_len as u64
}

/// `GetSerializeSize(CTxOut)` + `PER_UTXO_OVERHEAD`: the amount, the script with its length, and
/// the overhead.
fn utxo_size(script_len: usize) -> i64 {
    (8 + compact_size_len(script_len as u64) + script_len) as i64 + PER_UTXO_OVERHEAD
}

/// The coinstats index's running totals for one block (its `DBVal` without the hash), cumulative
/// from genesis.
// Core: index/coinstatsindex.cpp:31-45 DBVal
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CoinStatsTotals {
    /// `transaction_output_count`: `gettxoutsetinfo`'s `txouts` (golden `txouts`).
    pub transaction_output_count: u64,
    /// `bogo_size`: `gettxoutsetinfo`'s `bogosize`.
    pub bogo_size: u64,
    /// `total_amount` (golden `amount`).
    pub total_amount: Amount,
    pub total_subsidy: Amount,
    /// `total_unspendable_amount` (golden `unspendable`).
    pub total_unspendable_amount: Amount,
    pub total_prevout_spent_amount: Amount,
    pub total_new_outputs_ex_coinbase_amount: Amount,
    pub total_coinbase_amount: Amount,
    pub total_unspendables_genesis_block: Amount,
    /// Always 0 on Dash (see the module documentation).
    pub total_unspendables_bip30: Amount,
    pub total_unspendables_scripts: Amount,
    pub total_unspendables_unclaimed_rewards: Amount,
}

impl CoinStatsTotals {
    /// `gettxoutsetinfo`'s `block_info` for the block these totals are at: each field the
    /// difference from `prev`, the parent's totals (`None` for genesis, which Core compares with
    /// an empty `CCoinsStats`).
    // Core: rpc/blockchain.cpp:1297-1318 gettxoutsetinfo
    pub fn block_info(&self, prev: Option<&CoinStatsTotals>) -> BlockInfo {
        let prev = prev.copied().unwrap_or_default();
        let delta = |now: Amount, then: Amount| now.wrapping_sub(then);
        BlockInfo {
            prevout_spent: delta(self.total_prevout_spent_amount, prev.total_prevout_spent_amount),
            coinbase: delta(self.total_coinbase_amount, prev.total_coinbase_amount),
            new_outputs_ex_coinbase: delta(
                self.total_new_outputs_ex_coinbase_amount,
                prev.total_new_outputs_ex_coinbase_amount,
            ),
            unspendable: delta(self.total_unspendable_amount, prev.total_unspendable_amount),
            unspendables_genesis_block: delta(
                self.total_unspendables_genesis_block,
                prev.total_unspendables_genesis_block,
            ),
            unspendables_scripts: delta(
                self.total_unspendables_scripts,
                prev.total_unspendables_scripts,
            ),
            unspendables_unclaimed_rewards: delta(
                self.total_unspendables_unclaimed_rewards,
                prev.total_unspendables_unclaimed_rewards,
            ),
        }
    }
}

/// `gettxoutsetinfo`'s `block_info` (golden `bi`, in this field order): the amounts of one block.
/// Dash's `unspendables` has no `bip30` entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BlockInfo {
    pub prevout_spent: Amount,
    pub coinbase: Amount,
    pub new_outputs_ex_coinbase: Amount,
    pub unspendable: Amount,
    /// `unspendables.genesis_block`.
    pub unspendables_genesis_block: Amount,
    /// `unspendables.scripts`.
    pub unspendables_scripts: Amount,
    /// `unspendables.unclaimed_rewards`.
    pub unspendables_unclaimed_rewards: Amount,
}

/// The coinstats index's state: the MuHash of the UTXO set and the [`CoinStatsTotals`], from
/// genesis (CH3 D34: a finalized hash cannot seed it, so a run that claims per-block parity starts
/// from [`CoinStatsAccumulator::new`] before genesis).
#[derive(Clone, Debug, Default)]
pub struct CoinStatsAccumulator {
    muhash: MuHash3072,
    totals: CoinStatsTotals,
}

impl CoinStatsAccumulator {
    /// Before genesis: the empty set and zero totals (`CoinStatsIndex`'s members as constructed).
    pub fn new() -> Self {
        Self::default()
    }

    /// A state Core serialized (`DB_MUHASH`) or a scan produced, with its totals (CH3 a12).
    pub fn from_parts(muhash: MuHash3072, totals: CoinStatsTotals) -> Self {
        Self {
            muhash,
            totals,
        }
    }

    pub fn totals(&self) -> &CoinStatsTotals {
        &self.totals
    }

    /// The MuHash state (numerator and denominator).
    pub fn muhash(&self) -> &MuHash3072 {
        &self.muhash
    }

    /// `m_muhash.Finalize(out)`: the set's MuHash in Core's internal byte order (`gettxoutsetinfo
    /// muhash` prints it reversed). Like Core's, it divides the state down to the quotient, which
    /// leaves every later result unchanged.
    pub fn finalize(&mut self) -> [u8; 32] {
        self.muhash.finalize()
    }

    /// `CustomAppend` for `block` at `height`, which spent `undo`'s coins, with `subsidy` =
    /// `GetBlockSubsidy(pindex)`. At height 0 (genesis) the undo record is not read: pass an empty
    /// one. On `Err` nothing changed.
    // Core: index/coinstatsindex.cpp:117-234 CoinStatsIndex::CustomAppend
    pub fn append_block(
        &mut self,
        height: u32,
        block: &Block,
        undo: &impl SpentCoins,
        subsidy: Amount,
    ) -> Result<(), StatsError> {
        if height > 0 {
            check_pairing(block, undo)?;
        }
        let t = &mut self.totals;
        t.total_subsidy = t.total_subsidy.wrapping_add(subsidy);

        // Ignore genesis block
        if height > 0 {
            for (i, tx) in block.txs().iter().enumerate() {
                let (txid, coinbase) = (tx.txid(), tx.is_coinbase());
                // Add the new utxos created from the block
                for (index, out) in (0u32..).zip(tx.outputs()) {
                    let value = out.value;
                    // Skip unspendable coins
                    if is_unspendable(out.script_pubkey.as_bytes()) {
                        t.total_unspendable_amount = t.total_unspendable_amount.wrapping_add(value);
                        t.total_unspendables_scripts =
                            t.total_unspendables_scripts.wrapping_add(value);
                        continue;
                    }
                    let outpoint = OutPoint {
                        hash: txid,
                        index,
                    };
                    self.muhash
                        .insert_coin(&outpoint, &CoinRef::from_output(out, height, coinbase));
                    if coinbase {
                        t.total_coinbase_amount = t.total_coinbase_amount.wrapping_add(value);
                    } else {
                        t.total_new_outputs_ex_coinbase_amount =
                            t.total_new_outputs_ex_coinbase_amount.wrapping_add(value);
                    }
                    t.transaction_output_count = t.transaction_output_count.wrapping_add(1);
                    t.total_amount = t.total_amount.wrapping_add(value);
                    t.bogo_size = t.bogo_size.wrapping_add(bogo_size(out.script_pubkey.len()));
                }

                // The coinbase tx has no undo data since no former output is spent
                if !coinbase {
                    for (coin, input) in undo.coins(i - 1).zip(tx.inputs()) {
                        self.muhash.remove_coin(&input.prevout, &coin);
                        t.total_prevout_spent_amount =
                            t.total_prevout_spent_amount.wrapping_add(coin.value());
                        t.transaction_output_count = t.transaction_output_count.wrapping_sub(1);
                        t.total_amount = t.total_amount.wrapping_sub(coin.value());
                        t.bogo_size = t.bogo_size.wrapping_sub(bogo_size(script_len(&coin)));
                    }
                }
            }
        } else {
            // genesis block
            t.total_unspendable_amount = t.total_unspendable_amount.wrapping_add(subsidy);
            t.total_unspendables_genesis_block =
                t.total_unspendables_genesis_block.wrapping_add(subsidy);
        }

        // If spent prevouts + block subsidy are still a higher amount than new outputs +
        // coinbase + current unspendable amount this means the miner did not claim the full
        // block reward. Unclaimed block rewards are also unspendable.
        let unclaimed = (t.total_prevout_spent_amount.wrapping_add(t.total_subsidy)).wrapping_sub(
            t.total_new_outputs_ex_coinbase_amount
                .wrapping_add(t.total_coinbase_amount)
                .wrapping_add(t.total_unspendable_amount),
        );
        t.total_unspendable_amount = t.total_unspendable_amount.wrapping_add(unclaimed);
        t.total_unspendables_unclaimed_rewards =
            t.total_unspendables_unclaimed_rewards.wrapping_add(unclaimed);
        Ok(())
    }

    /// `ReverseBlock`: above height 0, takes back [`CoinStatsAccumulator::append_block`] of the
    /// same block, undo record and subsidy, so the state is the parent's again (Core then asserts
    /// that it equals the parent's stored entry; the caller compares). Core never reverses
    /// genesis, and its function has no genesis case: at height 0 it takes out the genesis
    /// outputs `append_block` never added and leaves the genesis unspendable amount, as here. On
    /// `Err` nothing changed.
    // Core: index/coinstatsindex.cpp:403-505 CoinStatsIndex::ReverseBlock
    pub fn reverse_block(
        &mut self,
        height: u32,
        block: &Block,
        undo: &impl SpentCoins,
        subsidy: Amount,
    ) -> Result<(), StatsError> {
        check_pairing(block, undo)?;
        let t = &mut self.totals;
        t.total_subsidy = t.total_subsidy.wrapping_sub(subsidy);

        // Remove the new UTXOs that were created from the block
        for (i, tx) in block.txs().iter().enumerate() {
            let (txid, coinbase) = (tx.txid(), tx.is_coinbase());
            for (index, out) in (0u32..).zip(tx.outputs()) {
                let value = out.value;
                // Skip unspendable coins
                if is_unspendable(out.script_pubkey.as_bytes()) {
                    t.total_unspendable_amount = t.total_unspendable_amount.wrapping_sub(value);
                    t.total_unspendables_scripts = t.total_unspendables_scripts.wrapping_sub(value);
                    continue;
                }
                let outpoint = OutPoint {
                    hash: txid,
                    index,
                };
                self.muhash.remove_coin(&outpoint, &CoinRef::from_output(out, height, coinbase));
                if coinbase {
                    t.total_coinbase_amount = t.total_coinbase_amount.wrapping_sub(value);
                } else {
                    t.total_new_outputs_ex_coinbase_amount =
                        t.total_new_outputs_ex_coinbase_amount.wrapping_sub(value);
                }
                t.transaction_output_count = t.transaction_output_count.wrapping_sub(1);
                t.total_amount = t.total_amount.wrapping_sub(value);
                t.bogo_size = t.bogo_size.wrapping_sub(bogo_size(out.script_pubkey.len()));
            }

            // The coinbase tx has no undo data since no former output is spent
            if !coinbase {
                for (coin, input) in undo.coins(i - 1).zip(tx.inputs()) {
                    self.muhash.insert_coin(&input.prevout, &coin);
                    t.total_prevout_spent_amount =
                        t.total_prevout_spent_amount.wrapping_sub(coin.value());
                    t.transaction_output_count = t.transaction_output_count.wrapping_add(1);
                    t.total_amount = t.total_amount.wrapping_add(coin.value());
                    t.bogo_size = t.bogo_size.wrapping_add(bogo_size(script_len(&coin)));
                }
            }
        }

        let unclaimed = (t
            .total_new_outputs_ex_coinbase_amount
            .wrapping_add(t.total_coinbase_amount)
            .wrapping_add(t.total_unspendable_amount))
        .wrapping_sub(t.total_prevout_spent_amount.wrapping_add(t.total_subsidy));
        t.total_unspendable_amount = t.total_unspendable_amount.wrapping_sub(unclaimed);
        t.total_unspendables_unclaimed_rewards =
            t.total_unspendables_unclaimed_rewards.wrapping_sub(unclaimed);
        Ok(())
    }
}

/// The `getblockstats` fields O8's golden `stats` carries, in its order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BlockStats {
    /// `GetBlockSubsidy(&pindex)`.
    pub subsidy: Amount,
    /// The non-coinbase transactions' fees: value in minus value out, and an asset unlock's
    /// payload fee.
    pub totalfee: Amount,
    /// `vtx.size()`.
    pub txs: i64,
    /// Inputs, the coinbase's excluded.
    pub ins: i64,
    /// Outputs, every transaction's.
    pub outs: i64,
    /// `outs - ins`.
    pub utxo_increase: i64,
    /// Every output's serialized size plus `PER_UTXO_OVERHEAD`, less the same of every spent
    /// coin; unspendable outputs included.
    pub utxo_size_inc: i64,
}

/// `getblockstats`' seven golden fields for `block`, which spent `undo`'s coins, with `subsidy` =
/// `GetBlockSubsidy(&pindex)`. For genesis Core reads an empty undo record (`GetUndoChecked`):
/// pass an empty one. (O8's goldens record no `stats` at height 0 by the extractor's choice.)
///
/// The two errors are Core's `CHECK_NONFATAL`s, which Core runs only when a requested statistic
/// loops over the inputs (`loop_inputs`: every one of these but `subsidy`, `txs`, `ins` and
/// `outs`); no connected block fails them.
// Core: rpc/blockchain.cpp:2211-2451 getblockstats (the default, every statistic)
pub fn block_stats(
    block: &Block,
    undo: &impl SpentCoins,
    subsidy: Amount,
) -> Result<BlockStats, StatsError> {
    check_pairing(block, undo)?;
    let (mut inputs, mut outputs, mut utxo_size_inc) = (0i64, 0i64, 0i64);
    let mut totalfee: Amount = 0;
    for (i, tx) in block.txs().iter().enumerate() {
        outputs = outputs.wrapping_add(tx.outputs().len() as i64);
        let mut tx_total_out: Amount = 0;
        for out in tx.outputs() {
            tx_total_out = tx_total_out.wrapping_add(out.value);
            utxo_size_inc = utxo_size_inc.wrapping_add(utxo_size(out.script_pubkey.len()));
        }
        if tx.is_coinbase() {
            continue;
        }
        // Don't count coinbase's fake input
        inputs = inputs.wrapping_add(tx.inputs().len() as i64);

        let mut tx_total_in: Amount = 0;
        for coin in undo.coins(i - 1) {
            tx_total_in = tx_total_in.wrapping_add(coin.value());
            utxo_size_inc = utxo_size_inc.wrapping_sub(utxo_size(script_len(&coin)));
        }
        let mut txfee = tx_total_in.wrapping_sub(tx_total_out);
        if tx.is_platform_transfer() {
            txfee = asset_unlock_payload_fee(tx).ok_or(StatsError::AssetUnlockPayload {
                tx: i,
            })?;
        }
        if !money_range(txfee) {
            return Err(StatsError::FeeOutOfRange {
                tx: i,
            });
        }
        totalfee = totalfee.wrapping_add(txfee);
    }
    Ok(BlockStats {
        subsidy,
        totalfee,
        txs: block.txs().len() as i64,
        ins: inputs,
        outs: outputs,
        utxo_increase: outputs.wrapping_sub(inputs),
        utxo_size_inc,
    })
}

/// `gettxoutsetinfo`'s `hash_type` (`CoinStatsHashType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoinStatsHashType {
    /// `hash_serialized_2`, the default.
    HashSerialized,
    MuHash,
    None,
}

/// What Core's `ComputeUTXOStats` counts over a coins view, without the block it is at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UtxoSetStats {
    /// `nTransactions`: the transactions with an unspent output.
    pub transactions: u64,
    /// `nTransactionOutputs`.
    pub txouts: u64,
    /// `nBogoSize`.
    pub bogosize: u64,
    /// `total_amount`: `None` once Core's `CheckedAdd` overflowed.
    pub total_amount: Option<Amount>,
    /// `hashSerialized`, in Core's internal byte order (RPC prints it reversed); `None` for
    /// [`CoinStatsHashType::None`].
    pub hash: Option<[u8; 32]>,
}

enum ScanHash {
    Serialized(HashSerialized2),
    MuHash(Box<MuHash3072>),
    None,
}

/// Core's `ComputeUTXOStats` cursor loop, fed the view's coins in its key order: `ApplyStats`
/// once per transaction, `ApplyHash` for the hash type, then `FinalizeHash`. `disk_size` is the
/// view's (`EstimateSize`), which the caller reads.
// Core: kernel/coinstats.cpp:101-150 ApplyStats, ComputeUTXOStats; :185-203 PrepareHash,
// FinalizeHash
pub struct UtxoSetScan {
    hash: ScanHash,
    txid: Option<dash_primitives::TxHash>,
    stats: UtxoSetStats,
}

impl UtxoSetScan {
    /// `PrepareHash` for a view at `best_block`.
    pub fn new(hash_type: CoinStatsHashType, best_block: &BlockHash) -> Self {
        let hash = match hash_type {
            CoinStatsHashType::HashSerialized => {
                ScanHash::Serialized(HashSerialized2::new(best_block))
            }
            CoinStatsHashType::MuHash => ScanHash::MuHash(Box::default()),
            CoinStatsHashType::None => ScanHash::None,
        };
        Self {
            hash,
            txid: None,
            stats: UtxoSetStats {
                transactions: 0,
                txouts: 0,
                bogosize: 0,
                total_amount: Some(0),
                hash: None,
            },
        }
    }

    /// The next coin of the view.
    ///
    /// # Panics
    ///
    /// For `hash_serialized_2`, when `outpoint`'s txid is below the previous coin's.
    pub fn push(&mut self, outpoint: &OutPoint, coin: &CoinRef<'_>) {
        if self.txid != Some(outpoint.hash) {
            self.stats.transactions += 1;
            self.txid = Some(outpoint.hash);
        }
        let stats = &mut self.stats;
        stats.txouts += 1;
        stats.total_amount = stats.total_amount.and_then(|t| t.checked_add(coin.value()));
        stats.bogosize += bogo_size(script_len(coin));
        match &mut self.hash {
            ScanHash::Serialized(h) => h.push(outpoint, coin),
            ScanHash::MuHash(m) => m.insert_coin(outpoint, coin),
            ScanHash::None => {}
        }
    }

    /// `FinalizeHash`.
    pub fn finish(self) -> UtxoSetStats {
        let hash = match self.hash {
            ScanHash::Serialized(h) => Some(h.finish()),
            ScanHash::MuHash(mut m) => Some(m.finalize()),
            ScanHash::None => None,
        };
        UtxoSetStats {
            hash,
            ..self.stats
        }
    }
}

/// `hash_serialized_2` of a UTXO set (`gettxoutsetinfo`'s default hash type): the set's best
/// block, then each transaction's coins (`ApplyHash`), under SHA256d, in Core's internal byte
/// order. `coins` come in the coins database's key order: ascending txid bytes, a transaction's
/// outputs together (see [`HashSerialized2`], which also has quirk 33).
///
/// # Panics
///
/// When a txid is below the previous coin's.
// Core: kernel/coinstats.cpp:72-88 ApplyHash (HashWriter), :163 (HASH_SERIALIZED), :185
// PrepareHash, :193 FinalizeHash
pub fn hash_serialized_2<'a>(
    best_block: &BlockHash,
    coins: impl IntoIterator<Item = (&'a OutPoint, CoinRef<'a>)>,
) -> [u8; 32] {
    let mut hash = HashSerialized2::new(best_block);
    for (outpoint, coin) in coins {
        hash.push(outpoint, &coin);
    }
    hash.finish()
}

#[cfg(test)]
mod tests {
    //! Hand-computed statistics of a four-block chain: genesis, a coinbase with an unspendable
    //! output, a spend whose fee the coinbase leaves partly unclaimed, and an asset unlock (an
    //! output without a spent prevout, which Core's index books as a negative unclaimed reward).

    use std::sync::Arc;

    use dash_params::{ChainParamsBuilder, NetworkKind, Release};
    use dash_primitives::{Transaction, TxHash, TxIn, TxOut, TxType};

    use super::*;
    use crate::coins::{TxUndo, TxUndoRef};
    use crate::primitives::Header;
    use crate::subsidy::COIN;
    use crate::{Tx, TxRef};

    /// 25 bytes, stored compressed (`ScriptCompression` form 0): the undo coin expands it back.
    const P2PKH: [u8; 25] = [
        0x76, 0xa9, 0x14, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
        0x88, 0xac,
    ];
    const TRUE: [u8; 1] = [0x51];
    const OP_RETURN: [u8; 1] = [0x6a];
    const SUBSIDY: Amount = 50 * COIN;
    const UNLOCK_FEE: u32 = 1_000;

    fn out(value: Amount, script: &[u8]) -> TxOut {
        TxOut {
            value,
            script_pubkey: script.to_vec().into(),
        }
    }

    fn tx(
        version: i16,
        tx_type: u16,
        inputs: Vec<TxIn>,
        outputs: Vec<TxOut>,
        payload: Vec<u8>,
    ) -> TxRef {
        Arc::new(Tx::new(Transaction {
            version,
            tx_type: TxType::new(tx_type),
            inputs,
            outputs,
            lock_time: 0,
            extra_payload: payload,
        }))
    }

    /// A coinbase whose script carries `height`, so each block's has its own txid.
    fn coinbase(height: u8, outputs: Vec<TxOut>) -> TxRef {
        let input = TxIn {
            prevout: OutPoint {
                hash: TxHash::from([0; 32]),
                index: u32::MAX,
            },
            script_sig: vec![1, height, 0x51].into(),
            sequence: u32::MAX,
        };
        tx(1, 0, vec![input], outputs, vec![])
    }

    fn spend(prevouts: &[OutPoint], outputs: Vec<TxOut>) -> TxRef {
        let inputs = prevouts
            .iter()
            .map(|prevout| TxIn {
                prevout: *prevout,
                script_sig: Default::default(),
                sequence: u32::MAX,
            })
            .collect();
        tx(1, 0, inputs, outputs, vec![])
    }

    /// An asset unlock payload (version, index, fee, requested height, quorum hash, a zero
    /// signature) followed by `trailing` bytes, which `GetTxPayload` refuses.
    fn asset_unlock(fee: u32, trailing: usize, outputs: Vec<TxOut>) -> TxRef {
        let mut payload = vec![1];
        payload.extend(7u64.to_le_bytes());
        payload.extend(fee.to_le_bytes());
        payload.extend(100u32.to_le_bytes());
        payload.extend([0xab; 32]);
        payload.extend([0; 96]);
        payload.extend(vec![0; trailing]);
        tx(3, 9, vec![], outputs, payload)
    }

    fn block(txs: Vec<TxRef>) -> Block {
        let genesis = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
            .build()
            .expect("regtest params")
            .genesis;
        Block::new(Header::new(genesis.header), txs)
    }

    fn outpoint(tx: &TxRef, index: u32) -> OutPoint {
        OutPoint {
            hash: tx.txid(),
            index,
        }
    }

    fn undo(txs: Vec<Vec<Coin>>) -> BlockUndo {
        BlockUndo {
            txs: txs
                .into_iter()
                .map(|coins| TxUndo {
                    coins,
                })
                .collect(),
        }
    }

    /// The chain: each block with its undo record. A block's height is its index and its subsidy
    /// is [`SUBSIDY`].
    fn chain() -> Vec<(Block, BlockUndo)> {
        let genesis = block(vec![coinbase(0, vec![out(SUBSIDY, &TRUE)])]);
        // 49 DASH in two coins, 1 DASH burnt: the whole subsidy.
        let cb1 =
            coinbase(1, vec![out(30 * COIN, &P2PKH), out(19 * COIN, &TRUE), out(COIN, &OP_RETURN)]);
        let b1 = block(vec![cb1.clone()]);
        // Spends 30 DASH for 29: the coinbase claims 45 of 51.
        let t2 = spend(&[outpoint(&cb1, 0)], vec![out(29 * COIN, &TRUE)]);
        let b2 = block(vec![coinbase(2, vec![out(45 * COIN, &TRUE)]), t2]);
        let u2 = undo(vec![vec![Coin::new(30 * COIN, &P2PKH, 1, true)]]);
        // Unlocks 10 DASH; the coinbase claims the subsidy and the unlock's fee.
        let b3 = block(vec![
            coinbase(3, vec![out(SUBSIDY + Amount::from(UNLOCK_FEE), &TRUE)]),
            asset_unlock(UNLOCK_FEE, 0, vec![out(10 * COIN, &TRUE)]),
        ]);
        let u3 = undo(vec![vec![]]);
        vec![(genesis, BlockUndo::default()), (b1, BlockUndo::default()), (b2, u2), (b3, u3)]
    }

    fn empty_muhash() -> [u8; 32] {
        MuHash3072::new().finalize()
    }

    #[test]
    fn totals_and_block_info_by_hand() {
        let chain = chain();
        let mut acc = CoinStatsAccumulator::new();
        let mut infos = Vec::new();
        let mut totals: Vec<CoinStatsTotals> = Vec::new();
        for (height, (block, undo)) in (0u32..).zip(&chain) {
            acc.append_block(height, block, undo, SUBSIDY).expect("the block's own undo");
            infos.push(acc.totals().block_info(totals.last()));
            totals.push(*acc.totals());
        }

        // Genesis: its subsidy is unspendable; no coin, the empty set's hash.
        let t0 = CoinStatsTotals {
            total_subsidy: SUBSIDY,
            total_unspendable_amount: SUBSIDY,
            total_unspendables_genesis_block: SUBSIDY,
            ..CoinStatsTotals::default()
        };
        assert_eq!(totals[0], t0);
        let mut genesis = CoinStatsAccumulator::new();
        genesis.append_block(0, &chain[0].0, &chain[0].1, SUBSIDY).unwrap();
        assert_eq!(genesis.finalize(), empty_muhash());

        // Block 1: two coins (bogo 50 + 25 and 50 + 1), 1 DASH to an unspendable script.
        let t1 = CoinStatsTotals {
            transaction_output_count: 2,
            bogo_size: 126,
            total_amount: 49 * COIN,
            total_subsidy: 2 * SUBSIDY,
            total_unspendable_amount: SUBSIDY + COIN,
            total_coinbase_amount: 49 * COIN,
            total_unspendables_genesis_block: SUBSIDY,
            total_unspendables_scripts: COIN,
            ..CoinStatsTotals::default()
        };
        assert_eq!(totals[1], t1);

        // Block 2: one coin spent (30), two created (45 + 29); 51 claimable, 45 claimed.
        let t2 = CoinStatsTotals {
            transaction_output_count: 3,
            bogo_size: 126 + 51 + 51 - 75,
            total_amount: (49 + 45 + 29 - 30) * COIN,
            total_subsidy: 3 * SUBSIDY,
            total_unspendable_amount: SUBSIDY + COIN + 6 * COIN,
            total_prevout_spent_amount: 30 * COIN,
            total_new_outputs_ex_coinbase_amount: 29 * COIN,
            total_coinbase_amount: (49 + 45) * COIN,
            total_unspendables_genesis_block: SUBSIDY,
            total_unspendables_scripts: COIN,
            total_unspendables_unclaimed_rewards: 6 * COIN,
            ..CoinStatsTotals::default()
        };
        assert_eq!(totals[2], t2);

        // Block 3: 10 DASH unlocked and the fee claimed with nothing spent: the unclaimed
        // reward (spent + subsidy - created) is negative.
        let unlocked = 10 * COIN + Amount::from(UNLOCK_FEE);
        let t3 = CoinStatsTotals {
            transaction_output_count: 5,
            bogo_size: t2.bogo_size + 51 + 51,
            total_amount: t2.total_amount + SUBSIDY + unlocked,
            total_subsidy: 4 * SUBSIDY,
            total_unspendable_amount: t2.total_unspendable_amount - unlocked,
            total_new_outputs_ex_coinbase_amount: (29 + 10) * COIN,
            total_coinbase_amount: t2.total_coinbase_amount + SUBSIDY + Amount::from(UNLOCK_FEE),
            total_unspendables_unclaimed_rewards: 6 * COIN - unlocked,
            ..t2
        };
        assert_eq!(totals[3], t3);

        let bi = |i: &BlockInfo| {
            [
                i.prevout_spent,
                i.coinbase,
                i.new_outputs_ex_coinbase,
                i.unspendable,
                i.unspendables_genesis_block,
                i.unspendables_scripts,
                i.unspendables_unclaimed_rewards,
            ]
        };
        assert_eq!(bi(&infos[0]), [0, 0, 0, SUBSIDY, SUBSIDY, 0, 0]);
        assert_eq!(bi(&infos[1]), [0, 49 * COIN, 0, COIN, 0, COIN, 0]);
        assert_eq!(bi(&infos[2]), [30 * COIN, 45 * COIN, 29 * COIN, 6 * COIN, 0, 0, 6 * COIN]);
        assert_eq!(
            bi(&infos[3]),
            [0, SUBSIDY + Amount::from(UNLOCK_FEE), 10 * COIN, -unlocked, 0, 0, -unlocked]
        );
    }

    #[test]
    fn muhash_is_the_hash_of_the_coins_left() {
        let chain = chain();
        let mut acc = CoinStatsAccumulator::new();
        for (height, (block, undo)) in (0u32..).zip(&chain) {
            acc.append_block(height, block, undo, SUBSIDY).unwrap();
        }
        let txs = |h: usize| chain[h].0.txs();
        let (cb1, cb2, t2, cb3, unlock) =
            (&txs(1)[0], &txs(2)[0], &txs(2)[1], &txs(3)[0], &txs(3)[1]);
        let mut scan = MuHash3072::new();
        for (tx, index, height, coinbase) in [
            (cb1, 1, 1, true),
            (cb2, 0, 2, true),
            (t2, 0, 2, false),
            (cb3, 0, 3, true),
            (unlock, 0, 3, false),
        ] {
            let coin = CoinRef::from_output(&tx.outputs()[index as usize], height, coinbase);
            scan.insert_coin(&outpoint(tx, index), &coin);
        }
        assert_eq!(acc.finalize(), scan.finalize());
        assert_eq!(acc.totals().transaction_output_count, 5);
    }

    #[test]
    fn reverse_takes_each_block_back() {
        let chain = chain();
        let mut acc = CoinStatsAccumulator::new();
        let mut states = vec![(CoinStatsTotals::default(), empty_muhash())];
        for (height, (block, undo)) in (0u32..).zip(&chain) {
            acc.append_block(height, block, undo, SUBSIDY).unwrap();
            states.push((*acc.totals(), acc.clone().finalize()));
        }
        for (height, (block, undo)) in chain.iter().enumerate().skip(1).rev() {
            acc.reverse_block(height as u32, block, undo, SUBSIDY).unwrap();
            let (totals, muhash) = &states[height];
            assert_eq!(acc.totals(), totals, "reversed {height}");
            assert_eq!(&acc.clone().finalize(), muhash, "reversed {height}");
        }
        // Core never reverses genesis; its ReverseBlock has no genesis case, so doing it anyway
        // removes an output that was never added.
        let (genesis, undo) = &chain[0];
        acc.reverse_block(0, genesis, undo, SUBSIDY).unwrap();
        assert_eq!(acc.totals().transaction_output_count, u64::MAX);
        assert_eq!(acc.totals().total_coinbase_amount, -SUBSIDY);
    }

    #[test]
    fn block_stats_by_hand() {
        let chain = chain();
        let stats = |h: usize| block_stats(&chain[h].0, &chain[h].1, SUBSIDY).unwrap();
        // Outputs of 25, 1 and 1 script bytes: 8 + 1 + len + 41 each.
        assert_eq!(
            stats(1),
            BlockStats {
                subsidy: SUBSIDY,
                totalfee: 0,
                txs: 1,
                ins: 0,
                outs: 3,
                utxo_increase: 3,
                utxo_size_inc: 75 + 51 + 51,
            }
        );
        // The spent coin's script is the expanded 25 bytes, not its compressed 21.
        assert_eq!(
            stats(2),
            BlockStats {
                subsidy: SUBSIDY,
                totalfee: COIN,
                txs: 2,
                ins: 1,
                outs: 2,
                utxo_increase: 1,
                utxo_size_inc: 51 + 51 - 75,
            }
        );
        // An asset unlock's fee is its payload's.
        assert_eq!(
            stats(3),
            BlockStats {
                subsidy: SUBSIDY,
                totalfee: Amount::from(UNLOCK_FEE),
                txs: 2,
                ins: 0,
                outs: 2,
                utxo_increase: 2,
                utxo_size_inc: 102,
            }
        );
        // The same from a borrowed record.
        let borrowed = BlockUndoRef {
            txs: chain[2]
                .1
                .txs
                .iter()
                .map(|tx| TxUndoRef {
                    coins: tx.coins.iter().map(Coin::as_ref).collect(),
                })
                .collect(),
        };
        assert_eq!(block_stats(&chain[2].0, &borrowed, SUBSIDY), Ok(stats(2)));
    }

    #[test]
    fn a_record_that_is_not_the_blocks_changes_nothing() {
        let chain = chain();
        let (b2, u2) = &chain[2];
        let coin = Coin::new(30 * COIN, &P2PKH, 1, true);
        let wrong = [
            undo(vec![]),
            undo(vec![vec![], vec![]]),
            undo(vec![vec![]]),
            undo(vec![vec![coin.clone(), coin]]),
        ];
        let mut acc = CoinStatsAccumulator::new();
        for (height, (block, undo)) in (0u32..).zip(&chain[..2]) {
            acc.append_block(height, block, undo, SUBSIDY).unwrap();
        }
        let before = (*acc.totals(), acc.clone().finalize());
        for undo in &wrong {
            assert_eq!(acc.append_block(2, b2, undo, SUBSIDY), Err(StatsError::UndoMismatch));
            assert_eq!(acc.reverse_block(2, b2, undo, SUBSIDY), Err(StatsError::UndoMismatch));
            assert_eq!(block_stats(b2, undo, SUBSIDY), Err(StatsError::UndoMismatch));
        }
        assert_eq!((*acc.totals(), acc.clone().finalize()), before);
        acc.append_block(2, b2, u2, SUBSIDY).unwrap();

        // The coinbase first and alone.
        let txs = b2.txs();
        let swapped = block(vec![txs[1].clone(), txs[0].clone()]);
        let two = block(vec![txs[0].clone(), coinbase(9, vec![])]);
        for bad in [&swapped, &two] {
            assert_eq!(acc.append_block(3, bad, u2, SUBSIDY), Err(StatsError::CoinbasePosition));
            assert_eq!(block_stats(bad, u2, SUBSIDY), Err(StatsError::CoinbasePosition));
        }
        // Genesis's undo record is not read.
        let mut genesis = CoinStatsAccumulator::new();
        assert_eq!(genesis.append_block(0, &chain[0].0, u2, SUBSIDY), Ok(()));
    }

    #[test]
    fn block_stats_of_genesis_read_an_empty_record() {
        // GetUndoChecked returns an empty CBlockUndo at height 0; the outputs count and size.
        let chain = chain();
        let (genesis, undo) = &chain[0];
        assert_eq!(
            block_stats(genesis, undo, SUBSIDY),
            Ok(BlockStats {
                subsidy: SUBSIDY,
                totalfee: 0,
                txs: 1,
                ins: 0,
                outs: 1,
                utxo_increase: 1,
                utxo_size_inc: 8 + 1 + 1 + 41,
            })
        );
    }

    #[test]
    fn block_stats_refuses_what_core_checks_nonfatally() {
        let cb = coinbase(4, vec![out(SUBSIDY, &TRUE)]);
        let bad_payload =
            block(vec![cb.clone(), asset_unlock(UNLOCK_FEE, 1, vec![out(COIN, &TRUE)])]);
        assert_eq!(
            block_stats(&bad_payload, &undo(vec![vec![]]), SUBSIDY),
            Err(StatsError::AssetUnlockPayload {
                tx: 1
            })
        );
        // A fee of 0 is in range here (getblockstats does not apply CheckTxInputs' fee > 0).
        let free = block(vec![cb.clone(), asset_unlock(0, 0, vec![out(COIN, &TRUE)])]);
        assert_eq!(block_stats(&free, &undo(vec![vec![]]), SUBSIDY).unwrap().totalfee, 0);
        // Value in below value out.
        let prevout = OutPoint {
            hash: TxHash::from([5; 32]),
            index: 0,
        };
        let overspend = block(vec![cb, spend(&[prevout], vec![out(2 * COIN, &TRUE)])]);
        let coins = undo(vec![vec![Coin::new(COIN, &TRUE, 1, false)]]);
        assert_eq!(
            block_stats(&overspend, &coins, SUBSIDY),
            Err(StatsError::FeeOutOfRange {
                tx: 1
            })
        );
    }

    /// `UtxoSetScan` is `ComputeUTXOStats`' loop: one transaction per run of a txid, Core's
    /// `GetBogoSize` per output, and each hash type's own hash over the same coins (the legacy
    /// one at the best block, MuHash at none).
    #[test]
    fn a_utxo_set_scan_counts_transactions_by_txid_runs() {
        let best = BlockHash::from([3; 32]);
        let op = |t: u8, index| OutPoint {
            hash: TxHash::from([t; 32]),
            index,
        };
        let coins = [
            (op(1, 0), Coin::new(COIN, &P2PKH, 5, true)),
            (op(1, 2), Coin::new(2 * COIN, &TRUE, 5, true)),
            (op(2, 1), Coin::new(7, &OP_RETURN, 0, false)),
        ];
        let scan = |hash_type| {
            let mut s = UtxoSetScan::new(hash_type, &best);
            for (o, c) in &coins {
                s.push(o, &c.as_ref());
            }
            s.finish()
        };
        let none = scan(CoinStatsHashType::None);
        assert_eq!(
            none,
            UtxoSetStats {
                transactions: 2,
                txouts: 3,
                bogosize: (50 + 25) + (50 + 1) + (50 + 1),
                total_amount: Some(3 * COIN + 7),
                hash: None,
            }
        );
        let legacy = hash_serialized_2(&best, coins.iter().map(|(o, c)| (o, c.as_ref())));
        assert_eq!(scan(CoinStatsHashType::HashSerialized).hash, Some(legacy));
        let mut muhash = MuHash3072::new();
        for (o, c) in &coins {
            muhash.insert_coin(o, &c.as_ref());
        }
        assert_eq!(scan(CoinStatsHashType::MuHash).hash, Some(muhash.finalize()));
        // Core's `CheckedAdd`: an overflowing total is no total.
        let mut s = UtxoSetScan::new(CoinStatsHashType::None, &best);
        let big = Coin::new(Amount::MAX, &TRUE, 1, false);
        s.push(&op(1, 0), &big.as_ref());
        s.push(&op(1, 1), &big.as_ref());
        assert_eq!(s.finish().total_amount, None);
    }
}
