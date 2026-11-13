// SPDX-License-Identifier: MIT

//! Coin statistics entries and the adapter to the shared consensus accumulator.

pub mod codec;

use std::sync::Arc;

use dash_consensus::Block;
use dash_consensus::coins::MuHash3072;
use dash_consensus::coins::stats::{CoinStatsAccumulator, CoinStatsTotals, SpentCoins};

use crate::{BlockRef, CoinStatsEntry, IndexError, SubsidyInputs};

impl CoinStatsEntry {
    /// Finalizes the accumulator's MuHash and snapshots all running totals.
    // Core: index/coinstatsindex.cpp:212-229
    pub fn from_accumulator(accumulator: &mut CoinStatsAccumulator) -> Self {
        let totals = *accumulator.totals();
        Self {
            muhash: accumulator.finalize(),
            txouts: totals.transaction_output_count,
            bogo_size: totals.bogo_size,
            total_amount: totals.total_amount,
            total_subsidy: totals.total_subsidy,
            total_unspendable: totals.total_unspendable_amount,
            prevout_spent: totals.total_prevout_spent_amount,
            new_outputs_ex_coinbase: totals.total_new_outputs_ex_coinbase_amount,
            coinbase: totals.total_coinbase_amount,
            unspendables_genesis: totals.total_unspendables_genesis_block,
            unspendables_bip30: totals.total_unspendables_bip30,
            unspendables_scripts: totals.total_unspendables_scripts,
            unspendables_unclaimed_rewards: totals.total_unspendables_unclaimed_rewards,
        }
    }

    /// The running totals used to restore the shared accumulator or compute `block_info`.
    // Core: index/coinstatsindex.cpp:377-388
    pub fn totals(&self) -> CoinStatsTotals {
        CoinStatsTotals {
            transaction_output_count: self.txouts,
            bogo_size: self.bogo_size,
            total_amount: self.total_amount,
            total_subsidy: self.total_subsidy,
            total_unspendable_amount: self.total_unspendable,
            total_prevout_spent_amount: self.prevout_spent,
            total_new_outputs_ex_coinbase_amount: self.new_outputs_ex_coinbase,
            total_coinbase_amount: self.coinbase,
            total_unspendables_genesis_block: self.unspendables_genesis,
            total_unspendables_bip30: self.unspendables_bip30,
            total_unspendables_scripts: self.unspendables_scripts,
            total_unspendables_unclaimed_rewards: self.unspendables_unclaimed_rewards,
        }
    }
}

/// Restores the shared accumulator, checking the saved MuHash against the block's entry.
// Core: index/coinstatsindex.cpp:363-388
pub fn from_parts(
    muhash: MuHash3072,
    entry: &CoinStatsEntry,
) -> Result<CoinStatsAccumulator, IndexError> {
    let mut accumulator = CoinStatsAccumulator::from_parts(muhash, entry.totals());
    if accumulator.finalize() != entry.muhash {
        return Err(IndexError::Corrupt("coinstats MuHash does not match the entry".into()));
    }
    Ok(accumulator)
}

/// Supplies the node's per-block subsidy to the shared accumulator.
pub struct CoinStatsAdapter {
    subsidies: Arc<dyn SubsidyInputs>,
}

impl CoinStatsAdapter {
    pub fn new(subsidies: Arc<dyn SubsidyInputs>) -> Self {
        Self {
            subsidies,
        }
    }

    /// Appends a connected block and returns its entry. Errors leave the accumulator unchanged.
    // Core: index/coinstatsindex.cpp:117-229
    pub fn append_block(
        &self,
        accumulator: &mut CoinStatsAccumulator,
        block: &BlockRef,
        data: &Block,
        undo: &impl SpentCoins,
    ) -> Result<CoinStatsEntry, IndexError> {
        let subsidy = self.subsidies.subsidy(block)?;
        accumulator.append_block(block.height, data, undo, subsidy).map_err(|err| {
            IndexError::Corrupt(format!("coinstats append {}: {err}", block.hash))
        })?;
        Ok(CoinStatsEntry::from_accumulator(accumulator))
    }

    /// Reverses a block above genesis and returns its parent's entry.
    /// Errors leave the accumulator unchanged; genesis cannot be disconnected.
    // Core: index/coinstatsindex.cpp:280-298,403-502
    pub fn reverse_block(
        &self,
        accumulator: &mut CoinStatsAccumulator,
        block: &BlockRef,
        data: &Block,
        undo: &impl SpentCoins,
    ) -> Result<CoinStatsEntry, IndexError> {
        if block.height == 0 {
            return Err(IndexError::Corrupt("cannot reverse coinstats genesis".into()));
        }
        let subsidy = self.subsidies.subsidy(block)?;
        accumulator.reverse_block(block.height, data, undo, subsidy).map_err(|err| {
            IndexError::Corrupt(format!("coinstats reverse {}: {err}", block.hash))
        })?;
        Ok(CoinStatsEntry::from_accumulator(accumulator))
    }
}
