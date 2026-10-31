//! Core's BIP9 state machine with Dash's dynamic threshold and EHF signal height
//! (`versionbits.cpp`), and its per-period cache (`VersionBitsCache`).

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use dash_params::{Bip9Deployment, DeploymentPos};

use super::Deployments;
use crate::ctx::BlockId;

/// What block version to use for new blocks (pre versionbits).
// Core: versionbits.h:16 VERSIONBITS_LAST_OLD_BLOCK_VERSION
pub const VERSIONBITS_LAST_OLD_BLOCK_VERSION: i32 = 4;
/// What bits to set in version for versionbits blocks.
// Core: versionbits.h:18 VERSIONBITS_TOP_BITS
pub const VERSIONBITS_TOP_BITS: i32 = 0x2000_0000;
/// What bitmask determines whether versionbits is in use.
// Core: versionbits.h:20 VERSIONBITS_TOP_MASK (0xE0000000UL narrowed to int32_t)
pub const VERSIONBITS_TOP_MASK: i32 = 0xE000_0000_u32 as i32;
/// Total bits available for versionbits.
// Core: versionbits.h:22 VERSIONBITS_NUM_BITS
pub const VERSIONBITS_NUM_BITS: i32 = 29;

/// `Consensus::MAX_VERSION_BITS_DEPLOYMENTS` at v24.
// Core: consensus/params.h:47
pub(super) const MAX_VERSION_BITS_DEPLOYMENTS: usize = 3;

/// Core's `ThresholdState`.
// Core: versionbits.h:29 ThresholdState
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThresholdState {
    Defined,
    Started,
    LockedIn,
    Active,
    Failed,
}

impl ThresholdState {
    /// The name `getdeploymentinfo` prints.
    // Core: rpc/blockchain.cpp:1467 get_state_name
    pub const fn name(self) -> &'static str {
        match self {
            Self::Defined => "defined",
            Self::Started => "started",
            Self::LockedIn => "locked_in",
            Self::Active => "active",
            Self::Failed => "failed",
        }
    }
}

/// Core's `BIP9Stats`.
// Core: versionbits.h:43 BIP9Stats
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Bip9Stats {
    /// Length of blocks of the BIP9 signalling period.
    pub period: i32,
    /// Number of blocks with the version bit set required to activate the softfork.
    pub threshold: i32,
    /// Number of blocks elapsed since the beginning of the current period.
    pub elapsed: i32,
    /// Number of blocks with the version bit set since the beginning of the current period.
    pub count: i32,
    /// False if there are not enough blocks left in this period to pass activation threshold.
    pub possible: bool,
}

/// Core's `ThresholdConditionCache`: the state of the block after each key, keyed by the last
/// block of a period (`None` is the null `pindexPrev` of genesis).
// Core: versionbits.h:40 ThresholdConditionCache
type ThresholdConditionCache = BTreeMap<Option<BlockId>, ThresholdState>;

/// Core's `VersionBitsCache`: one [`ThresholdConditionCache`] per deployment, behind one lock.
///
/// Entries are never invalidated; only [`VersionBitsCache::clear`] drops them, as Core's
/// `ChainstateManager` does when it resets. The keys are block-index ids, so a cache belongs to
/// one block-index arena and must be cleared when that arena is replaced.
// Core: versionbits.h:83 VersionBitsCache
#[derive(Debug, Default)]
pub struct VersionBitsCache {
    caches: Mutex<[ThresholdConditionCache; MAX_VERSION_BITS_DEPLOYMENTS]>,
}

impl VersionBitsCache {
    pub fn new() -> Self {
        Self::default()
    }

    // Core: versionbits.cpp:319 VersionBitsCache::Clear
    pub fn clear(&self) {
        for cache in self.lock().iter_mut() {
            cache.clear();
        }
    }

    /// Core's `LOCK(m_mutex)`. A panic while it is held is one of Core's asserts, which abort
    /// the process, so a poisoned lock is not recovered.
    pub(super) fn lock(
        &self,
    ) -> MutexGuard<'_, [ThresholdConditionCache; MAX_VERSION_BITS_DEPLOYMENTS]> {
        self.caches.lock().expect("VersionBitsCache: a panic while the lock was held")
    }
}

/// Core's `AbstractThresholdConditionChecker`: the parameters and condition of one BIP9-style
/// state machine, and its `GetStateFor` over the walk's cache. [`Checker`] is a deployment's
/// (`VersionBitsConditionChecker`), [`WarningBits`] `UpdateTip`'s unknown-bit warning's.
// Core: versionbits.h:58-79 AbstractThresholdConditionChecker
pub(super) trait ThresholdConditionChecker {
    /// The chain the checker evaluates.
    fn deployments(&self) -> &Deployments<'_>;

    // Core: versionbits.h:61 Condition
    fn condition(&self, id: BlockId) -> bool;

    // Core: versionbits.h:62 BeginTime
    fn begin_time(&self) -> i64;

    // Core: versionbits.h:63 SignalHeight
    fn signal_height(&self, prev: Option<BlockId>) -> i32;

    // Core: versionbits.h:64 EndTime
    fn end_time(&self) -> i64;

    // Core: versionbits.h:65 MinActivationHeight (no delay unless overridden)
    fn min_activation_height(&self) -> i32 {
        0
    }

    // Core: versionbits.h:66 Period
    fn period(&self) -> i32;

    // Core: versionbits.h:67 Threshold
    fn threshold(&self, n_attempt: i32) -> i32;

    fn height(&self, id: BlockId) -> i32 {
        self.deployments().height(id)
    }

    /// `GetAncestor(height)` of a non-null index: null below 0 and above the index.
    fn ancestor(&self, id: BlockId, height: i32) -> Option<BlockId> {
        self.deployments().ancestor(id, height)
    }

    // Core: versionbits.cpp:11-31 calculateStartHeight
    fn calculate_start_height(
        &self,
        mut prev: Option<BlockId>,
        mut state: ThresholdState,
        n_period: i32,
        cache: &ThresholdConditionCache,
    ) -> i32 {
        let mut n_start_height = i32::MAX;
        while state == ThresholdState::Started {
            let p =
                prev.expect("calculateStartHeight: STARTED is never cached for a null pindexPrev");
            let height = self.height(p);
            n_start_height = n_start_height.min(height + 1);
            prev = self.ancestor(p, height.wrapping_sub(n_period));
            state =
                *cache.get(&prev).expect("calculateStartHeight: assert(cache_it != cache.end())");
        }
        n_start_height
    }

    // Core: versionbits.cpp:33-135 AbstractThresholdConditionChecker::GetStateFor
    fn get_state_for(
        &self,
        prev: Option<BlockId>,
        cache: &mut ThresholdConditionCache,
    ) -> ThresholdState {
        let n_period = self.period();
        let min_activation_height = self.min_activation_height();
        let n_time_start = self.begin_time();
        let masternode_start_height = self.signal_height(prev);
        let n_time_timeout = self.end_time();

        if n_time_start == Bip9Deployment::ALWAYS_ACTIVE {
            return ThresholdState::Active;
        }
        if n_time_start == Bip9Deployment::NEVER_ACTIVE {
            return ThresholdState::Failed;
        }

        let mut prev = prev.and_then(|p| {
            let height = self.height(p);
            self.ancestor(p, height - (height + 1) % n_period)
        });

        let mut to_compute = Vec::new();
        while !cache.contains_key(&prev) {
            let Some(p) = prev else {
                cache.insert(None, ThresholdState::Defined);
                break;
            };
            let height = self.height(p);
            if height < self.deployments().min_bip9_warning_height() {
                cache.insert(prev, ThresholdState::Defined);
                break;
            }
            if self.deployments().mtp(p) < n_time_start || height < masternode_start_height {
                cache.insert(prev, ThresholdState::Defined);
                break;
            }
            to_compute.push(p);
            prev = self.ancestor(p, height.wrapping_sub(n_period));
        }

        let mut state = cache[&prev];
        if to_compute.is_empty() {
            return state;
        }

        let mut n_start_height = self.calculate_start_height(prev, state, n_period, cache);

        while let Some(p) = to_compute.pop() {
            let mut state_next = state;
            let mtp = self.deployments().mtp(p);
            let height = self.height(p);
            match state {
                ThresholdState::Defined => {
                    if mtp >= n_time_start && height >= masternode_start_height {
                        state_next = ThresholdState::Started;
                        n_start_height = height + 1;
                    }
                }
                ThresholdState::Started => {
                    let mut count = 0;
                    let n_attempt = (height - n_start_height) / n_period;
                    let threshold = self.threshold(n_attempt);
                    let mut count_index = Some(p);
                    let mut i = 0;
                    while count + (n_period - i) >= threshold && i < n_period {
                        let index =
                            count_index.expect("GetStateFor: a period's blocks have parents");
                        if self.condition(index) {
                            count += 1;
                        }
                        count_index = self.deployments().chain.prev(index);
                        i += 1;
                    }
                    assert!(
                        n_start_height > 0 && n_start_height < i32::MAX,
                        "GetStateFor: assert(nStartHeight > 0 && nStartHeight < max)"
                    );
                    if count >= threshold {
                        state_next = ThresholdState::LockedIn;
                    } else if mtp >= n_time_timeout {
                        state_next = ThresholdState::Failed;
                    }
                }
                ThresholdState::LockedIn => {
                    if height + 1 >= min_activation_height {
                        state_next = ThresholdState::Active;
                    }
                }
                ThresholdState::Failed | ThresholdState::Active => {}
            }
            state = state_next;
            cache.insert(Some(p), state);
        }

        state
    }
}

impl ThresholdConditionChecker for Checker<'_, '_> {
    fn deployments(&self) -> &Deployments<'_> {
        self.deployments
    }

    // Core: versionbits.cpp:262-265 Condition
    fn condition(&self, id: BlockId) -> bool {
        let version = self.deployments.chain.header(id).version;
        (version & VERSIONBITS_TOP_MASK) == VERSIONBITS_TOP_BITS
            && (version as u32 & self.mask()) != 0
    }

    // Core: versionbits.cpp:231 BeginTime
    fn begin_time(&self) -> i64 {
        self.deployment.start_time
    }

    // Core: versionbits.cpp:232-246 SignalHeight
    fn signal_height(&self, prev: Option<BlockId>) -> i32 {
        if !self.deployment.use_ehf {
            return 0;
        }
        let signals = self.deployments.ehf_signals_stage(prev);
        // `signals.find(deployment.bit)`: the int bit converts to the map's uint8_t key; no
        // signal is INT_MAX.
        signals.get(self.deployment.bit as u8).unwrap_or(i32::MAX)
    }

    // Core: versionbits.cpp:247 EndTime
    fn end_time(&self) -> i64 {
        self.deployment.timeout
    }

    // Core: versionbits.cpp:248 MinActivationHeight
    fn min_activation_height(&self) -> i32 {
        i32::try_from(self.deployment.min_activation_height.to_u32())
            .expect("min_activation_height is Core's int")
    }

    // Core: versionbits.cpp:249 Period (the int64_t/uint32_t ternary narrowed to int)
    fn period(&self) -> i32 {
        let window = self.deployment.window_size;
        let consensus = &self.deployments.params.consensus;
        (if window != 0 {
            window
        } else {
            i64::from(consensus.miner_confirmation_window)
        }) as i32
    }

    // Core: versionbits.cpp:250-260 Threshold
    fn threshold(&self, n_attempt: i32) -> i32 {
        let d = self.deployment;
        if d.threshold_start == 0 {
            return self.deployments.params.consensus.rule_change_activation_threshold as i32;
        }
        if d.threshold_min == 0 || d.falloff_coeff == 0 {
            return d.threshold_start as i32;
        }
        // `nAttempt * nAttempt * Period(params) / 100` is int arithmetic (quirk 32), then the
        // int64_t division and subtraction.
        let decay = n_attempt.wrapping_mul(n_attempt).wrapping_mul(self.period()) / 100;
        let n_threshold_calc = d.threshold_start.wrapping_sub(i64::from(decay) / d.falloff_coeff);
        d.threshold_min.max(n_threshold_calc) as i32
    }
}

/// Core's `ChainstateManager::m_warningcache`: one [`ThresholdConditionCache`] per version bit,
/// for `UpdateTip`'s unknown-bit warning ([`Deployments::warning_state`]). Like
/// [`VersionBitsCache`], it is keyed by block-index ids and never invalidated: it belongs to one
/// block-index arena, for that arena's lifetime.
// Core: validation.h:936 m_warningcache
#[derive(Debug, Default)]
pub struct WarningCache {
    caches: [ThresholdConditionCache; VERSIONBITS_NUM_BITS as usize],
}

impl WarningCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// `m_warningcache.at(bit)`.
    ///
    /// # Panics
    /// If `bit` is not below `VERSIONBITS_NUM_BITS` (`std::array::at`'s `out_of_range`).
    pub(super) fn at(&mut self, bit: i32) -> &mut ThresholdConditionCache {
        let slot = usize::try_from(bit).ok().and_then(|b| self.caches.get_mut(b));
        slot.expect("m_warningcache.at(bit): a bit below VERSIONBITS_NUM_BITS")
    }
}

/// `WarningBitsConditionChecker`: version bit `bit` set by the network's blocks at or above
/// `MinBIP9WarningHeight` while this release's own versionbits deployments (`ComputeBlockVersion`)
/// do not set it, at the network's `nRuleChangeActivationThreshold` of each
/// `nMinerConfirmationWindow`, with no start time, timeout or signal height.
// Core: validation.cpp:2395-2417 WarningBitsConditionChecker
pub(super) struct WarningBits<'a, 'd> {
    pub(super) deployments: &'d Deployments<'a>,
    pub(super) bit: i32,
}

impl ThresholdConditionChecker for WarningBits<'_, '_> {
    fn deployments(&self) -> &Deployments<'_> {
        self.deployments
    }

    // Core: validation.cpp:2410-2416 Condition
    fn condition(&self, id: BlockId) -> bool {
        let d = self.deployments;
        let header = d.chain.header(id);
        let version = header.version;
        header.height >= d.params.consensus.min_bip9_warning_height.to_u32()
            && (version & VERSIONBITS_TOP_MASK) == VERSIONBITS_TOP_BITS
            && ((version >> self.bit) & 1) != 0
            && ((d.compute_block_version(d.chain.prev(id)) >> self.bit) & 1) == 0
    }

    // Core: validation.cpp:2404 BeginTime
    fn begin_time(&self) -> i64 {
        0
    }

    // Core: validation.cpp:2406 SignalHeight
    fn signal_height(&self, _prev: Option<BlockId>) -> i32 {
        0
    }

    // Core: validation.cpp:2405 EndTime
    fn end_time(&self) -> i64 {
        i64::MAX
    }

    // Core: validation.cpp:2407 Period (the uint32_t narrowed to int)
    fn period(&self) -> i32 {
        self.deployments.params.consensus.miner_confirmation_window as i32
    }

    // Core: validation.cpp:2408 Threshold (the uint32_t narrowed to int)
    fn threshold(&self, _n_attempt: i32) -> i32 {
        self.deployments.params.consensus.rule_change_activation_threshold as i32
    }
}

/// `VersionBitsConditionChecker`: one deployment's parameters and the chain it is evaluated on.
// Core: versionbits.cpp:225 VersionBitsConditionChecker
pub(super) struct Checker<'a, 'd> {
    pub(super) deployments: &'d Deployments<'a>,
    pub(super) deployment: &'d Bip9Deployment,
}

impl<'a, 'd> Checker<'a, 'd> {
    pub(super) fn new(deployments: &'d Deployments<'a>, pos: DeploymentPos) -> Self {
        Self {
            deployments,
            deployment: deployments.bip9(pos),
        }
    }

    // Core: versionbits.cpp:269 Mask
    pub(super) fn mask(&self) -> u32 {
        1u32 << self.deployment.bit
    }

    // Core: versionbits.cpp:137-196 AbstractThresholdConditionChecker::GetStateStatisticsFor
    pub(super) fn get_state_statistics_for(
        &self,
        block: Option<BlockId>,
        cache: &mut ThresholdConditionCache,
        mut signalling_blocks: Option<&mut Vec<bool>>,
    ) -> Bip9Stats {
        let mut stats = Bip9Stats {
            period: self.period(),
            threshold: self.threshold(0),
            ..Bip9Stats::default()
        };

        let Some(block) = block else {
            return stats;
        };
        let height = self.height(block);

        let mut blocks_in_period = 1 + height % stats.period;

        if let Some(signalling) = signalling_blocks.as_deref_mut() {
            signalling.clear();
            signalling.resize(
                usize::try_from(blocks_in_period)
                    .expect("GetStateStatisticsFor: assign() of a negative size"),
                false,
            );
        }

        let end_of_prev_period = self.ancestor(block, height - (height + 1) % stats.period);
        let mut n_attempt = 0;
        let state = self.get_state_for(end_of_prev_period, cache);
        if state == ThresholdState::Started {
            let n_start_height = self.get_state_since_height_for(end_of_prev_period, cache);
            let end =
                end_of_prev_period.expect("GetStateStatisticsFor: STARTED after a non-null block");
            n_attempt = (self.height(end) + 1 - n_start_height) / stats.period;
        }
        stats.threshold = self.threshold(n_attempt);

        let mut elapsed = 0;
        let mut count = 0;
        let mut current = Some(block);
        loop {
            elapsed += 1;
            blocks_in_period -= 1;
            let index = current.expect("GetStateStatisticsFor: the period's blocks have parents");
            if self.condition(index) {
                count += 1;
                if let Some(signalling) = signalling_blocks.as_deref_mut() {
                    signalling[blocks_in_period as usize] = true;
                }
            }
            current = self.deployments.chain.prev(index);
            if blocks_in_period <= 0 {
                break;
            }
        }

        stats.elapsed = elapsed;
        stats.count = count;
        // int: a threshold narrowed from a `-vbparams` value at or above 2^31 wraps here
        stats.possible = stats.period.wrapping_sub(stats.threshold) >= (stats.elapsed - count);
        stats
    }

    // Core: versionbits.cpp:198-221 AbstractThresholdConditionChecker::GetStateSinceHeightFor
    pub(super) fn get_state_since_height_for(
        &self,
        prev: Option<BlockId>,
        cache: &mut ThresholdConditionCache,
    ) -> i32 {
        let start_time = self.begin_time();
        if start_time == Bip9Deployment::ALWAYS_ACTIVE || start_time == Bip9Deployment::NEVER_ACTIVE
        {
            return 0;
        }

        let initial_state = self.get_state_for(prev, cache);

        if initial_state == ThresholdState::Defined {
            return 0;
        }

        let n_period = self.period();

        let prev = prev.expect("GetStateSinceHeightFor: only genesis's null pindexPrev is DEFINED");
        let height = self.height(prev);
        let mut prev = self
            .ancestor(prev, height - (height + 1) % n_period)
            .expect("GetStateSinceHeightFor: Assert(pindexPrev->GetAncestor(...))");

        let mut previous_period_parent =
            self.ancestor(prev, self.height(prev).wrapping_sub(n_period));

        while let Some(parent) = previous_period_parent {
            if self.get_state_for(Some(parent), cache) != initial_state {
                break;
            }
            prev = parent;
            previous_period_parent = self.ancestor(prev, self.height(prev).wrapping_sub(n_period));
        }

        self.height(prev) + 1
    }
}
