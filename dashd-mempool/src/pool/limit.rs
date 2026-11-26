//! The pool's size limit, time expiry and rolling minimum fee: `Expire`, `TrimToSize`,
//! `trackPackageRemoved` and `GetMinFee`, and `LimitMempoolSize`'s pool half.

use std::collections::BTreeSet;

use dash_consensus::TxRef;
use dash_primitives::OutPoint;

use super::PoolState;
use crate::accept::single::fee_rate;
use crate::types::MemPoolRemovalReason;

/// `CTxMemPool::ROLLING_FEE_HALFLIFE`: twelve hours.
// Core: txmempool.h:347 at v24.0.2
pub const ROLLING_FEE_HALFLIFE: i64 = 60 * 60 * 12;

const COIN: i64 = 100_000_000;

/// `lastRollingFeeUpdate`, `blockSinceLastRollingFeeBump` and `rollingMinimumFeeRate`. Core
/// starts `lastRollingFeeUpdate` at `GetTime()`, which nothing reads before `removeForBlock`
/// sets it, since only that sets `blockSinceLastRollingFeeBump`.
// Core: txmempool.h:329-331 at v24.0.2
#[derive(Debug, Default)]
pub(super) struct RollingFee {
    last_update: i64,
    block_since_bump: bool,
    rate: f64,
}

/// `CFeeRate::ToString()`: `<DASH>.<8 digits> DASH/kB`.
// Core: policy/feerate.cpp:39-45 at v24.0.2
pub fn fee_rate_to_string(per_k: i64) -> String {
    format!("{}.{:08} DASH/kB", per_k / COIN, per_k % COIN)
}

/// `llround`: half away from zero, as `f64::round`.
fn llround(x: f64) -> i64 {
    x.round() as i64
}

impl PoolState {
    /// `m_max_size_bytes` as the `size_t` Core passes to `GetMinFee` and `TrimToSize`.
    pub fn max_size(&self) -> usize {
        self.options.max_size_bytes as usize
    }

    /// `Expire(time)`: every entry older than `time` by the entry-time index, except one
    /// InstantSend has locked, goes with its descendants as EXPIRY. Returns how many left.
    // Core: txmempool.cpp:2007-2027 at v24.0.2
    pub fn expire(&mut self, time: i64) -> usize {
        let to_remove: Vec<_> = self
            .entry_time
            .iter()
            .take_while(|(entry_time, _)| *entry_time < time)
            .map(|(_, txid)| *txid)
            .filter(|txid| !self.is_hooks.is_locked(txid))
            .collect();
        let mut stage = BTreeSet::new();
        for txid in to_remove {
            stage.extend(self.calculate_descendants(txid));
        }
        self.remove_staged(&stage, false, MemPoolRemovalReason::Expiry);
        stage.len()
    }

    /// `trackPackageRemoved(rate)`: a removed package's feerate above the rolling minimum
    /// becomes it, and the decay waits for the next block.
    // Core: txmempool.cpp:2081-2087 at v24.0.2
    fn track_package_removed(&mut self, rate: i64) {
        let rolling = self.rolling.get_mut();
        if rate as f64 > rolling.rate {
            rolling.rate = rate as f64;
            rolling.block_since_bump = false;
        }
    }

    /// `TrimToSize(size_limit, &vNoSpendsRemaining)`: while the usage is over `size_limit`,
    /// the first entry of the descendant-score index goes with its descendants as SIZELIMIT,
    /// after its package feerate plus the incremental relay feerate is tracked. Returns the
    /// removed transactions' prevouts whose transaction is not in the pool.
    // Core: txmempool.cpp:2089-2125 at v24.0.2
    pub fn trim_to_size(&mut self, size_limit: usize) -> Vec<OutPoint> {
        let mut removed_count: u32 = 0;
        let mut max_removed = 0;
        let mut no_spends_remaining = Vec::new();
        while !self.entries.is_empty() && self.usage > size_limit {
            let first = self.descendant_score_order().next().expect("a non-empty pool");
            let entry = &self.entries[&first];
            let removed = fee_rate(entry.descendants.modified_fees, entry.descendants.size as i64)
                .wrapping_add(self.options.incremental_relay_feerate);
            self.track_package_removed(removed);
            max_removed = max_removed.max(removed);

            let stage = self.calculate_descendants(first);
            removed_count = removed_count.wrapping_add(stage.len() as u32);
            let txs: Vec<TxRef> = stage.iter().map(|id| self.entries[id].tx.clone()).collect();
            self.remove_staged(&stage, false, MemPoolRemovalReason::SizeLimit);
            for tx in txs {
                for input in tx.inputs() {
                    if !self.exists(&input.prevout().txid()) {
                        no_spends_remaining.push(input.prevout());
                    }
                }
            }
        }
        if max_removed > 0 {
            tracing::debug!(
                target: "mempool",
                "Removed {removed_count} txn, rolling minimum fee bumped to {}",
                fee_rate_to_string(max_removed)
            );
        }
        no_spends_remaining
    }

    /// `GetMinFee(size_limit)` in duffs per kB: the rolling minimum, halved every
    /// `ROLLING_FEE_HALFLIFE` (a quarter of it under a quarter of `size_limit`, half under half)
    /// once a block came after its last bump, read at most every ten seconds; zero once under
    /// half the incremental relay feerate; otherwise at least that feerate.
    // Core: txmempool.cpp:2057-2079 at v24.0.2
    pub fn min_fee(&self, size_limit: usize) -> i64 {
        let mut rolling = self.rolling.lock();
        if !rolling.block_since_bump || rolling.rate == 0.0 {
            return llround(rolling.rate);
        }
        let time = self.now();
        if time > rolling.last_update + 10 {
            let mut halflife = ROLLING_FEE_HALFLIFE as f64;
            if self.usage < size_limit / 4 {
                halflife /= 4.0;
            } else if self.usage < size_limit / 2 {
                halflife /= 2.0;
            }
            rolling.rate /= 2f64.powf((time - rolling.last_update) as f64 / halflife);
            rolling.last_update = time;
            if rolling.rate < self.options.incremental_relay_feerate as f64 / 2.0 {
                rolling.rate = 0.0;
                return 0;
            }
        }
        llround(rolling.rate).max(self.options.incremental_relay_feerate)
    }

    /// `GetMinFee()`: at `m_max_size_bytes`.
    // Core: txmempool.h:669-671 at v24.0.2
    pub fn get_min_fee(&self) -> i64 {
        self.min_fee(self.max_size())
    }

    /// `removeForBlock`'s tail: the rolling minimum starts decaying from now.
    // Core: txmempool.cpp:1348-1349 at v24.0.2
    pub(super) fn rolling_fee_block(&mut self) {
        let now = self.now();
        let rolling = self.rolling.get_mut();
        rolling.last_update = now;
        rolling.block_since_bump = true;
    }

    /// `LimitMempoolSize`'s pool half: `Expire(GetTime() - m_expiry)`, logged when it removed
    /// any, then `TrimToSize(m_max_size_bytes)`. Returns `vNoSpendsRemaining`, which the caller
    /// uncaches from `CoinsTip()`.
    // Core: validation.cpp:396-410 at v24.0.2
    pub fn limit_size(&mut self) -> Vec<OutPoint> {
        let expired = self.expire(self.now().wrapping_sub(self.options.expiry_secs));
        if expired != 0 {
            tracing::debug!(
                target: "mempool",
                "Expired {expired} transactions from the memory pool"
            );
        }
        self.trim_to_size(self.max_size())
    }
}

#[cfg(test)]
#[path = "limit_tests.rs"]
mod tests;
