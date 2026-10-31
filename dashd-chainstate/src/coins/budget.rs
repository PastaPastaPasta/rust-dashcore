//! The coins budget (CH2 §2.5): Core's `nTotalSpace`, the native ledger and Core's size-state
//! arithmetic.
//!
//! The budget `B(t)` is Core's `nTotalSpace` exactly: the coins share plus whatever the mempool
//! leaves of its maximum. The cache's own counter, native Σ ([`Usage::total`]), is the only one
//! (DEC-99, DEC-102); [`core_size_arith`] is Core's `GetCoinsCacheSizeState` arithmetic applied to
//! it. That is Core's arithmetic on another number, so our LARGE and CRITICAL do not fall at Core's
//! blocks (D11), and nothing here claims they do.

use super::layer::ShardSizes;
use crate::caches::CacheSizes;
use crate::tuning::EngineTuning;

/// A fraction of the budget: f, at which active freezes (THRESHOLD). 1/2 by default; PF3 tunes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ratio {
    num: u32,
    den: u32,
}

impl Ratio {
    pub const HALF: Self = Self::new(1, 2);

    /// `num / den`.
    ///
    /// # Panics
    ///
    /// Unless `0 < num < den`.
    pub const fn new(num: u32, den: u32) -> Self {
        assert!(0 < num && num < den, "a ratio strictly between 0 and 1");
        Self {
            num,
            den,
        }
    }

    /// `⌊x · num / den⌋`.
    pub fn of(self, x: u64) -> u64 {
        (u128::from(x) * u128::from(self.num) / u128::from(self.den)) as u64
    }
}

/// Core's `CoinsCacheSizeState` (`validation.h:456-464`), decided on native Σ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CoinsCacheSizeState {
    Ok,
    Large,
    Critical,
}

/// The coin side of the flush decision at a block boundary ([`super::CoinsCache::boundary_state`];
/// rev5, O-R6-m5). b1's `FlushPolicy` maps it onto Core's modes and adds its other causes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoinsBoundary {
    Ok,
    /// Freeze: no F is in flight, and active has reached f·B(t) or a shard of active its trigger.
    Threshold,
    /// [`CoinsCacheSizeState::Large`] on native Σ.
    Large,
    /// [`CoinsCacheSizeState::Critical`] on native Σ.
    Critical,
}

/// [`super::CoinsCache::boundary`]'s result: the state, and the native Σ and B(t) it compared,
/// so the CRITICAL log line reports what the decision saw (BL-90(b)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundaryCheck {
    pub state: CoinsBoundary,
    /// Native Σ, R included while a flush is in flight.
    pub sigma: u64,
    /// B(t).
    pub total: u64,
}

// Core: validation.cpp:2945-2966 Chainstate::GetCoinsCacheSizeState (v24.0.0-rc.3)
/// Core's `GetCoinsCacheSizeState` arithmetic, in Core's `i64`, on the usage it is given:
/// CRITICAL iff `usage > B`, else LARGE iff `usage > max(9B/10, B − 10 MiB)`, else OK. `total_space`
/// is `B(t)` ([`CoinsBudget::total`]). Values past `i64::MAX`, where Core's conversion would wrap,
/// saturate, and so does `9 · B` where Core's would overflow.
pub fn core_size_arith(usage: u64, total_space: u64) -> CoinsCacheSizeState {
    const MAX_BLOCK_COINSDB_USAGE_BYTES: i64 = 10 * 1024 * 1024;
    let cache_size = i64::try_from(usage).unwrap_or(i64::MAX);
    let total = i64::try_from(total_space).unwrap_or(i64::MAX);
    let large = (total.saturating_mul(9) / 10).max(total - MAX_BLOCK_COINSDB_USAGE_BYTES);
    if cache_size > total {
        CoinsCacheSizeState::Critical
    } else if cache_size > large {
        CoinsCacheSizeState::Large
    } else {
        CoinsCacheSizeState::Ok
    }
}

/// The native ledger: the cache's own allocations in bytes, capacity rather than occupancy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub active: u64,
    /// F, while a flush holds it.
    pub frozen: u64,
    pub prefetch: u64,
    /// An open block overlay: its child layer and its `StableBytes`.
    pub overlay: u64,
    /// Long undo coins still alive (`LedgerBox`es), wherever they are.
    pub held_out: u64,
    /// R: the engine's sub-batch reserve while a flush is in flight.
    pub reserve: u64,
    /// The part of active's and F's shard ceilings their tables have not grown to yet
    /// ([`ShardedLayer::unbacked_bytes`](super::layer::ShardedLayer::unbacked_bytes)): the budget
    /// charges a shard its ceiling, so a layer that grows to it freezes and flushes where a
    /// presized one would (CH2c D29).
    pub unbacked: u64,
}

impl Usage {
    /// Native Σ.
    pub fn total(&self) -> u64 {
        self.allocated() + self.reserve + self.unbacked
    }

    /// The cache's allocations alone: native Σ less R and the unbacked ceilings, what the tagging
    /// allocator counts (§6.7).
    pub fn allocated(&self) -> u64 {
        self.active + self.frozen + self.prefetch + self.overlay + self.held_out
    }
}

/// The coins budget's parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoinsBudget {
    /// Core's `m_coinstip_cache_size_bytes`.
    coins_share: u64,
    /// The mempool's maximum (`-maxmempool`), Core's `m_max_size_bytes`.
    max_mempool: u64,
    f: Ratio,
    /// R while a flush is in flight: `EngineTuning::batch_reserve(S)`.
    batch_reserve: u64,
}

impl CoinsBudget {
    /// The node's budget: the coins share from `sizes`, and R from the engine's tuning for
    /// sub-batches of `sub_batch` bytes (§2.5; rev3, G-R3-m2).
    pub fn for_engine<T: EngineTuning>(
        sizes: &CacheSizes,
        max_mempool: u64,
        f: Ratio,
        sub_batch: u64,
        tuning: &T,
    ) -> Self {
        Self::new(sizes.coins, max_mempool, f, tuning.batch_reserve(sub_batch))
    }

    pub fn new(coins_share: u64, max_mempool: u64, f: Ratio, batch_reserve: u64) -> Self {
        Self {
            coins_share,
            max_mempool,
            f,
            batch_reserve,
        }
    }

    pub fn coins_share(&self) -> u64 {
        self.coins_share
    }

    pub fn f(&self) -> Ratio {
        self.f
    }

    // Core: validation.cpp:2952-2953 (nTotalSpace)
    /// B(t): the coins share plus what the mempool leaves of its maximum.
    pub fn total(&self, mempool_usage: u64) -> u64 {
        self.coins_share + self.max_mempool.saturating_sub(mempool_usage)
    }

    /// R: the batch reserve while a flush is in flight, else 0.
    pub fn reserve(&self, flush_in_flight: bool) -> u64 {
        if flush_in_flight {
            self.batch_reserve
        } else {
            0
        }
    }

    /// [`core_size_arith`] of native Σ against B(t).
    pub fn size_state(&self, sigma: u64, mempool_usage: u64) -> CoinsCacheSizeState {
        core_size_arith(sigma, self.total(mempool_usage))
    }

    /// The THRESHOLD bytes: f·B(t).
    pub fn threshold(&self, mempool_usage: u64) -> u64 {
        self.f.of(self.total(mempool_usage))
    }

    /// The prefetch map's headroom: B(t)/16 (A1 §3.4).
    pub fn prefetch_cap(&self, mempool_usage: u64) -> u64 {
        self.total(mempool_usage) / 16
    }

    /// What two layers may take: `B − R − B/16`, with R the batch reserve.
    fn layers(&self, mempool_usage: u64) -> u64 {
        let b = self.total(mempool_usage);
        b.saturating_sub(self.batch_reserve + b / 16)
    }

    /// A new active layer's presize, its ceiling (CH2c D29), beside a frozen layer charged
    /// `frozen` bytes (CH2 §2.3; rev5, O-R6-m6): `min(f·(B − R − B/16), B − R − B/16 − frozen)`,
    /// where the second term is never below 256 buckets a shard. With no F that is the presize
    /// itself, `f·(B − R − B/16)`.
    pub fn presize(&self, mempool_usage: u64, frozen: u64) -> u64 {
        let layers = self.layers(mempool_usage);
        self.f.of(layers).min(layers.saturating_sub(frozen).max(MIN_PRESIZE))
    }

    /// `ResizeCoinsCaches`' new coins share.
    pub(crate) fn set_coins_share(&mut self, coins_share: u64) {
        self.coins_share = coins_share;
    }
}

/// The smallest presize: 256 buckets in every shard.
pub(crate) const MIN_PRESIZE: u64 = ShardSizes {
    k: 8,
    s: 0,
}
.table_bytes();

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    /// Core's arithmetic, as `validation.cpp:2949-2965` writes it, for the comparison.
    fn core(cache_size: i64, max_coins: i64, max_mempool: i64, mempool_usage: i64) -> u8 {
        let total = max_coins + (max_mempool - mempool_usage).max(0);
        let large = ((9 * total) / 10).max(total - 10 * 1024 * 1024);
        if cache_size > total {
            2
        } else if cache_size > large {
            1
        } else {
            0
        }
    }

    fn code(s: CoinsCacheSizeState) -> u8 {
        s as u8
    }

    /// rev5 §7.1 a2: usage ∈ {large − 1, large, large + 1, B − 1, B, B + 1}, with and without a
    /// mempool allowance, at B = 1.75 MiB (`-dbcache=4`) and B_DEC43 (252.75 MiB).
    #[test]
    fn the_size_state_is_cores_arithmetic_at_its_edges() {
        let max_mempool = 300 * MIB;
        for share in [7 * MIB / 4, 252 * MIB + 3 * MIB / 4] {
            for mempool_usage in [max_mempool, max_mempool - 5 * MIB, 0, max_mempool + 1] {
                let budget = CoinsBudget::new(share, max_mempool, Ratio::HALF, 0);
                let b = budget.total(mempool_usage);
                let large = ((9 * b) / 10).max(b.saturating_sub(10 * MIB));
                for usage in [large - 1, large, large + 1, b - 1, b, b + 1, 0, u64::MAX] {
                    let want = core(
                        i64::try_from(usage).unwrap_or(i64::MAX),
                        share as i64,
                        max_mempool as i64,
                        mempool_usage as i64,
                    );
                    assert_eq!(code(budget.size_state(usage, mempool_usage)), want, "{usage} {b}");
                }
                assert_eq!(budget.size_state(large, mempool_usage), CoinsCacheSizeState::Ok);
                assert_eq!(budget.size_state(large + 1, mempool_usage), CoinsCacheSizeState::Large);
                assert_eq!(budget.size_state(b, mempool_usage), CoinsCacheSizeState::Large);
                assert_eq!(budget.size_state(b + 1, mempool_usage), CoinsCacheSizeState::Critical);
            }
        }
        // B = 1.75 MiB with no allowance: LARGE above 1,651,507 (Sol's r4 witness).
        let tiny = CoinsBudget::new(7 * MIB / 4, 0, Ratio::HALF, 0);
        assert_eq!(tiny.total(0), 1_835_008);
        assert_eq!(tiny.size_state(1_651_507, 0), CoinsCacheSizeState::Ok);
        assert_eq!(tiny.size_state(1_651_508, 0), CoinsCacheSizeState::Large);
        // At B_DEC43 the 10 MiB term decides: LARGE above B − 10 MiB.
        let dec43 = CoinsBudget::new(252 * MIB + 3 * MIB / 4, 0, Ratio::HALF, 0);
        let b = dec43.total(0);
        assert_eq!(dec43.size_state(b - 10 * MIB, 0), CoinsCacheSizeState::Ok);
        assert_eq!(dec43.size_state(b - 10 * MIB + 1, 0), CoinsCacheSizeState::Large);
    }

    #[test]
    fn the_budget_follows_the_mempool_allowance() {
        let budget = CoinsBudget::new(100 * MIB, 300 * MIB, Ratio::HALF, 16 * MIB + (64 << 10));
        assert_eq!(budget.total(0), 400 * MIB);
        assert_eq!(budget.total(299 * MIB), 101 * MIB);
        assert_eq!(budget.total(301 * MIB), 100 * MIB);
        assert_eq!(budget.threshold(300 * MIB), 50 * MIB);
        assert_eq!(budget.prefetch_cap(300 * MIB), 100 * MIB / 16);
        assert_eq!(budget.reserve(false), 0);
        assert_eq!(budget.reserve(true), 16 * MIB + (64 << 10));
        // A usage that is OK while the mempool is empty turns CRITICAL as it fills.
        assert_eq!(budget.size_state(150 * MIB, 0), CoinsCacheSizeState::Ok);
        assert_eq!(budget.size_state(150 * MIB, 300 * MIB), CoinsCacheSizeState::Critical);
    }

    /// The presize (CH2 §2.3) and m6's rule beside a grown F, with the bench's numbers.
    #[test]
    fn the_presize_is_what_is_left_beside_f_but_never_below_the_floor() {
        let b = 252 * MIB + 3 * MIB / 4;
        let r = 16 * MIB + (64 << 10);
        let budget = CoinsBudget::new(b, 0, Ratio::HALF, r);
        let full = (b - r - b / 16) / 2;
        assert_eq!(budget.presize(0, 0), full);
        assert_eq!(budget.presize(0, full), full);
        assert_eq!(budget.presize(0, full + 5 * MIB), full - 5 * MIB);
        assert_eq!(budget.presize(0, b), MIN_PRESIZE);
        for f in [Ratio::new(1, 4), Ratio::HALF, Ratio::new(3, 4)] {
            let budget = CoinsBudget::new(b, 0, f, r);
            assert_eq!(budget.presize(0, 0), f.of(b - r - b / 16));
        }
    }
}
