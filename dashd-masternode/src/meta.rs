//! Core's `CMasternodeMetaMan` (`masternode/meta.{h,cpp}`): what the node keeps about
//! masternodes besides the deterministic list (FR-EVO-10): the CoinJoin queue counters, the
//! outbound connection statistics, the governance objects each one voted on, and the platform
//! ban (DIP-0031), whose reset by a ProUpServTx is a chain effect ([`dash_evo::Effect`]).
//!
//! Its file is `mncache.dat` in rdashd's format ([`crate::flatdb`]):
//! `"rdashd-mncache" ‖ message start ‖ "MasternodeMetaStore-1" ‖ Core's object ‖ SHA256d`, where
//! Core's object is `MasternodeMetaStore::Serialize` after its own version string: the meta infos
//! in key order, `nDsqCount` and the used masternodes, oldest first. It is the body
//! `dash-coreimport` reads in Core's `mncache.dat` (`CMasternodeMetaMan-Version-5`).
//!
//! The callers are other tasks' (CoinJoin: the `dsq` counters and used masternodes; governance:
//! the votes; the masternode connections: the outbound statistics; AM1: `platformban`); SP1 ports
//! the store, its file and the reset effect.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_evo::Effect;
use dash_evo::types::ProTxHash;
use dash_p2p_core::PlatformBan;
use parking_lot::Mutex;

use crate::flatdb::{
    self, FlatObject, Layout, Own, ReadError, capacity, push_compact_size, push_str, read_array,
    read_bool, read_compact_size, read_i32, read_i64, read_str,
};

/// The file's name in the network's data directory (Core's).
pub const FILE_NAME: &str = "mncache.dat";

/// The format version (Core's `SERIALIZATION_VERSION_STRING` role; Core's is
/// `"CMasternodeMetaMan-Version-5"`).
pub const VERSION: &str = "MasternodeMetaStore-1";

/// `MASTERNODE_MAX_FAILED_OUTBOUND_ATTEMPTS`.
// Core: masternode/meta.cpp:13
pub const MAX_FAILED_OUTBOUND_ATTEMPTS: i32 = 5;
/// `MASTERNODE_MAX_MIXING_TXES`.
// Core: masternode/meta.cpp:14
pub const MAX_MIXING_TXES: i32 = 5;
/// `SeenBanInventorySize`: twice the expected evonodes (DIP-0028).
// Core: masternode/meta.h:200
pub const SEEN_BAN_INVENTORY_SIZE: usize = 900;

/// A governance object's hash (internal byte order).
pub type GovHash = [u8; 32];

/// Core's `CMasternodeMetaInfo`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MasternodeMetaInfo {
    pub protx_hash: ProTxHash,
    /// `m_last_dsq`: the `dsq` count of its last queue broadcast.
    pub last_dsq: i64,
    pub mixing_tx_count: i32,
    /// `mapGovernanceObjectsVotedOn`.
    pub governance_objects_voted_on: BTreeMap<GovHash, i32>,
    pub outbound_attempt_count: i32,
    pub last_outbound_attempt: i64,
    pub last_outbound_success: i64,
    /// `m_platform_ban`: under a platform ban by a `platformban` message.
    pub platform_ban: bool,
    /// `m_platform_ban_updated`: the height the ban was applied or removed at.
    pub platform_ban_updated: i32,
}

impl MasternodeMetaInfo {
    fn new(protx_hash: ProTxHash) -> MasternodeMetaInfo {
        MasternodeMetaInfo {
            protx_hash,
            ..MasternodeMetaInfo::default()
        }
    }

    // Core: masternode/meta.h:57-62 SERIALIZE_METHODS
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.protx_hash.as_bytes());
        out.extend_from_slice(&self.last_dsq.to_le_bytes());
        out.extend_from_slice(&self.mixing_tx_count.to_le_bytes());
        push_compact_size(out, self.governance_objects_voted_on.len());
        for (hash, n) in &self.governance_objects_voted_on {
            out.extend_from_slice(hash);
            out.extend_from_slice(&n.to_le_bytes());
        }
        out.extend_from_slice(&self.outbound_attempt_count.to_le_bytes());
        out.extend_from_slice(&self.last_outbound_attempt.to_le_bytes());
        out.extend_from_slice(&self.last_outbound_success.to_le_bytes());
        out.push(u8::from(self.platform_ban));
        out.extend_from_slice(&self.platform_ban_updated.to_le_bytes());
    }

    fn read(data: &mut &[u8]) -> Result<MasternodeMetaInfo, ReadError> {
        let protx_hash = ProTxHash::from(read_array::<32>(data)?);
        let last_dsq = read_i64(data)?;
        let mixing_tx_count = read_i32(data)?;
        let mut governance_objects_voted_on = BTreeMap::new();
        for _ in 0..read_compact_size(data)? {
            let hash = read_array::<32>(data)?;
            let n = read_i32(data)?;
            governance_objects_voted_on.entry(hash).or_insert(n);
        }
        Ok(MasternodeMetaInfo {
            protx_hash,
            last_dsq,
            mixing_tx_count,
            governance_objects_voted_on,
            outbound_attempt_count: read_i32(data)?,
            last_outbound_attempt: read_i64(data)?,
            last_outbound_success: read_i64(data)?,
            platform_ban: read_bool(data)?,
            platform_ban_updated: read_i32(data)?,
        })
    }

    /// `AddGovernanceVote`: the count goes up, from 0 if the object is new.
    // Core: masternode/meta.cpp:56-62
    fn add_governance_vote(&mut self, hash: GovHash) {
        *self.governance_objects_voted_on.entry(hash).or_insert(0) += 1;
    }

    /// `SetPlatformBan(is_banned, height)`: refuses an older height, and an unban at the height
    /// already recorded.
    // Core: masternode/meta.h:73-84
    pub fn set_platform_ban(&mut self, is_banned: bool, height: i32) -> bool {
        if height < self.platform_ban_updated {
            return false;
        }
        if height == self.platform_ban_updated && !is_banned {
            return false;
        }
        self.platform_ban = is_banned;
        self.platform_ban_updated = height;
        true
    }
}

/// `MasternodeMetaStore`: what `mncache.dat` holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MasternodeMetaStore {
    /// `metaInfos`.
    pub infos: BTreeMap<ProTxHash, MasternodeMetaInfo>,
    /// `nDsqCount`.
    pub dsq_count: i64,
    /// `m_used_masternodes` (a FIFO); `m_used_masternodes_set` is rebuilt from it.
    pub used_masternodes: VecDeque<ProTxHash>,
}

impl FlatObject for MasternodeMetaStore {
    const FILE: &'static str = FILE_NAME;
    /// Unlike Core's `"magicMasternodeCache"`, so neither reads the other's file.
    const MAGIC: &'static str = "rdashd-mncache";

    // Core: masternode/meta.h:102-114 MasternodeMetaStore::Serialize
    fn write(&self, out: &mut Vec<u8>) {
        push_str(out, VERSION.as_bytes());
        push_compact_size(out, self.infos.len());
        for info in self.infos.values() {
            info.write(out);
        }
        out.extend_from_slice(&self.dsq_count.to_le_bytes());
        push_compact_size(out, self.used_masternodes.len());
        for hash in &self.used_masternodes {
            out.extend_from_slice(hash.as_bytes());
        }
    }

    /// Core clears the infos, returns on another version string, then reads into temporaries
    /// that it moves in after the last read; `nDsqCount` is read in place, so a later failure
    /// leaves what was read there (`Clear` does not reset it). A repeated protx hash keeps its
    /// first entry (`emplace`).
    // Core: masternode/meta.h:116-138 MasternodeMetaStore::Unserialize
    fn read(&mut self, data: &mut &[u8]) -> Result<(), ReadError> {
        self.infos.clear();
        if read_str(data)? != VERSION.as_bytes() {
            return Ok(());
        }
        let n = read_compact_size(data)?;
        let mut infos = Vec::with_capacity(capacity(n));
        for _ in 0..n {
            infos.push(MasternodeMetaInfo::read(data)?);
        }
        self.dsq_count = read_i64(data)?;
        let n = read_compact_size(data)?;
        let mut used = VecDeque::with_capacity(capacity(n));
        for _ in 0..n {
            used.push_back(ProTxHash::from(read_array::<32>(data)?));
        }
        for info in infos {
            self.infos.entry(info.protx_hash).or_insert(info);
        }
        self.used_masternodes = used;
        Ok(())
    }

    // Core: masternode/meta.h:140-147 MasternodeMetaStore::Clear
    fn clear(&mut self) {
        self.infos.clear();
        self.used_masternodes.clear();
    }

    // Core: masternode/meta.cpp:260-265 MasternodeMetaStore::ToString
    fn summary(&self) -> String {
        format!(
            "Masternodes: meta infos object count: {}, nDsqCount: {}, used masternodes count: {}",
            self.infos.len(),
            self.dsq_count,
            self.used_masternodes.len()
        )
    }
}

/// Core's `unordered_lru_cache<uint256, PlatformBanMessage>` of `SeenBanInventorySize`: past
/// twice its size it keeps the most recently used half. Every access counts as a use (`get`
/// and `exists` too), and the counters are unique, so what it keeps is determined.
// Core: unordered_lru_cache.h
#[derive(Debug, Default)]
struct SeenBans {
    map: HashMap<[u8; 32], (PlatformBan, i64)>,
    counter: i64,
}

impl SeenBans {
    fn tick(&mut self) -> i64 {
        let t = self.counter;
        self.counter += 1;
        t
    }

    // Core: unordered_lru_cache.h _emplace, truncate_if_needed
    fn emplace(&mut self, key: [u8; 32], value: PlatformBan) {
        let t = self.tick();
        self.map.insert(key, (value, t));
        if self.map.len() <= 2 * SEEN_BAN_INVENTORY_SIZE {
            return;
        }
        let mut uses: Vec<i64> = self.map.values().map(|(_, t)| *t).collect();
        uses.sort_unstable_by(|a, b| b.cmp(a));
        let oldest_kept = uses[SEEN_BAN_INVENTORY_SIZE - 1];
        self.map.retain(|_, (_, t)| *t >= oldest_kept);
    }

    fn get(&mut self, key: &[u8; 32]) -> Option<PlatformBan> {
        let t = self.tick();
        self.map.get_mut(key).map(|(v, used)| {
            *used = t;
            v.clone()
        })
    }
}

#[derive(Debug, Default)]
struct State {
    store: MasternodeMetaStore,
    /// `m_used_masternodes_set`.
    used_set: HashSet<ProTxHash>,
    seen_bans: SeenBans,
}

impl State {
    /// `GetMetaInfo`: the entry, made if missing.
    // Core: masternode/meta.cpp:83-91
    fn info(&mut self, protx: &ProTxHash) -> &mut MasternodeMetaInfo {
        self.store.infos.entry(*protx).or_insert_with(|| MasternodeMetaInfo::new(*protx))
    }

    /// `GetMetaInfoOrDefault`: the entry, or a default one (never stored).
    // Core: masternode/meta.cpp:70-75
    fn info_or_default(&self, protx: &ProTxHash) -> MasternodeMetaInfo {
        self.store.infos.get(protx).cloned().unwrap_or_default()
    }
}

/// Core's `CMasternodeMetaMan`.
#[derive(Debug, Default)]
pub struct MasternodeMetaMan {
    state: Mutex<State>,
    is_valid: AtomicBool,
}

impl MasternodeMetaMan {
    pub fn new() -> MasternodeMetaMan {
        MasternodeMetaMan::default()
    }

    /// `IsValid()`: `LoadCache` succeeded, so the destructor stores the file.
    pub fn is_valid(&self) -> bool {
        self.is_valid.load(Ordering::SeqCst)
    }

    /// `LoadCache(load_cache)`: loads `path`, or stores the empty manager over it when the node
    /// starts without its caches. Returns `IsValid()`; false is Core's `"Failed to load
    /// masternode cache from %s"` (or `"Failed to clear …"`).
    // Core: masternode/meta.cpp:31-36 LoadCache; init.cpp:2283-2289
    pub fn load_cache(&self, path: &Path, message_start: [u8; 4], load: bool) -> bool {
        self.load_cache_as::<Own>(path, message_start, load)
    }

    fn load_cache_as<L: Layout<MasternodeMetaStore>>(
        &self,
        path: &Path,
        message_start: [u8; 4],
        load: bool,
    ) -> bool {
        let valid = if load {
            let mut st = self.state.lock();
            let loaded = flatdb::load_as::<_, L>(path, message_start, &mut st.store);
            st.used_set = st.store.used_masternodes.iter().copied().collect();
            loaded.is_ok()
        } else {
            self.store_as::<L>(path, message_start)
        };
        self.is_valid.store(valid, Ordering::SeqCst);
        valid
    }

    /// `m_db->Store(*this)` (the destructor's, when [`Self::is_valid`]).
    // Core: masternode/meta.cpp:25-29; flat-database.h:192-205 CFlatDB::Store
    pub fn store(&self, path: &Path, message_start: [u8; 4]) -> bool {
        self.store_as::<Own>(path, message_start)
    }

    fn store_as<L: Layout<MasternodeMetaStore>>(
        &self,
        path: &Path,
        message_start: [u8; 4],
    ) -> bool {
        let snapshot = self.state.lock().store.clone();
        flatdb::log_write(flatdb::store_as::<_, L>(&snapshot, path, message_start))
    }

    /// [`Self::load_cache`] of a file in Core's layout (test-only, for the L1 comparison).
    #[cfg(any(test, feature = "core-layout"))]
    pub fn load_cache_core_layout(&self, path: &Path, message_start: [u8; 4], load: bool) -> bool {
        self.load_cache_as::<flatdb::core_layout::Core>(path, message_start, load)
    }

    /// [`Self::store`] in Core's layout (test-only, for the L1 comparison).
    #[cfg(any(test, feature = "core-layout"))]
    pub fn store_core_layout(&self, path: &Path, message_start: [u8; 4]) -> bool {
        self.store_as::<flatdb::core_layout::Core>(path, message_start)
    }

    /// The store, for tests, diagnostics and RPC.
    pub fn snapshot(&self) -> MasternodeMetaStore {
        self.state.lock().store.clone()
    }

    /// `GetInfo`: a copy of the entry, or a default one.
    // Core: masternode/meta.cpp:77-81
    pub fn info(&self, protx: &ProTxHash) -> MasternodeMetaInfo {
        self.state.lock().info_or_default(protx)
    }

    /// `IsMixingThresholdExceeded(protx_hash, mn_count)`: the masternode mixed within the last
    /// fifth of `mn_count` queues. An unknown masternode never has.
    // Core: masternode/meta.cpp:93-108
    pub fn is_mixing_threshold_exceeded(&self, protx: &ProTxHash, mn_count: i32) -> bool {
        let st = self.state.lock();
        let Some(info) = st.store.infos.get(protx) else {
            tracing::debug!(target: "coinjoin", "DSQUEUE -- node {protx} is logged");
            return false;
        };
        let last_dsq = info.last_dsq;
        let threshold = last_dsq + i64::from(mn_count / 5);
        tracing::debug!(
            target: "coinjoin",
            "DSQUEUE -- mn: {protx} last_dsq: {last_dsq}  dsq_threshold: {threshold}  nDsqCount: {}",
            st.store.dsq_count
        );
        last_dsq != 0 && threshold > st.store.dsq_count
    }

    /// `AllowMixing`: the masternode's `last_dsq` becomes the next queue count, and its mixing
    /// count resets.
    // Core: masternode/meta.cpp:110-116
    pub fn allow_mixing(&self, protx: &ProTxHash) {
        let mut st = self.state.lock();
        st.store.dsq_count += 1;
        let dsq = st.store.dsq_count;
        let info = st.info(protx);
        info.last_dsq = dsq;
        info.mixing_tx_count = 0;
    }

    /// `DisallowMixing`.
    // Core: masternode/meta.cpp:118-122
    pub fn disallow_mixing(&self, protx: &ProTxHash) {
        self.state.lock().info(protx).mixing_tx_count += 1;
    }

    /// `IsValidForMixingTxes`: at most [`MAX_MIXING_TXES`].
    // Core: masternode/meta.cpp:124-128
    pub fn is_valid_for_mixing_txes(&self, protx: &ProTxHash) -> bool {
        self.state.lock().info_or_default(protx).mixing_tx_count <= MAX_MIXING_TXES
    }

    /// `AddGovernanceVote(proTxHash, nGovernanceObjectHash)`.
    // Core: masternode/meta.cpp:130-134
    pub fn add_governance_vote(&self, protx: &ProTxHash, object: GovHash) {
        self.state.lock().info(protx).add_governance_vote(object);
    }

    /// `RemoveGovernanceObject`: from every masternode's votes.
    // Core: masternode/meta.cpp:136-142
    pub fn remove_governance_object(&self, object: &GovHash) {
        for info in self.state.lock().store.infos.values_mut() {
            info.governance_objects_voted_on.remove(object);
        }
    }

    /// `SetLastOutboundAttempt`: the time, and one more attempt.
    // Core: masternode/meta.h:70; meta.cpp:151-155
    pub fn set_last_outbound_attempt(&self, protx: &ProTxHash, t: i64) {
        let mut st = self.state.lock();
        let info = st.info(protx);
        info.last_outbound_attempt = t;
        info.outbound_attempt_count += 1;
    }

    /// `SetLastOutboundSuccess`: the time, and the attempts reset.
    // Core: masternode/meta.h:71; meta.cpp:157-161
    pub fn set_last_outbound_success(&self, protx: &ProTxHash, t: i64) {
        let mut st = self.state.lock();
        let info = st.info(protx);
        info.last_outbound_success = t;
        info.outbound_attempt_count = 0;
    }

    /// `GetLastOutboundAttempt`.
    // Core: masternode/meta.cpp:163-167
    pub fn last_outbound_attempt(&self, protx: &ProTxHash) -> i64 {
        self.state.lock().info_or_default(protx).last_outbound_attempt
    }

    /// `GetLastOutboundSuccess`.
    // Core: masternode/meta.cpp:169-173
    pub fn last_outbound_success(&self, protx: &ProTxHash) -> i64 {
        self.state.lock().info_or_default(protx).last_outbound_success
    }

    /// `OutboundFailedTooManyTimes`: more than [`MAX_FAILED_OUTBOUND_ATTEMPTS`].
    // Core: masternode/meta.cpp:175-179
    pub fn outbound_failed_too_many_times(&self, protx: &ProTxHash) -> bool {
        self.state.lock().info_or_default(protx).outbound_attempt_count
            > MAX_FAILED_OUTBOUND_ATTEMPTS
    }

    /// `IsPlatformBanned`.
    // Core: masternode/meta.cpp:181-185
    pub fn is_platform_banned(&self, protx: &ProTxHash) -> bool {
        self.state.lock().info_or_default(protx).platform_ban
    }

    /// `ResetPlatformBan(proTxHash, nHeight)`: only for a masternode with an entry, through
    /// [`MasternodeMetaInfo::set_platform_ban`]'s height rule.
    // Core: masternode/meta.cpp:187-195
    pub fn reset_platform_ban(&self, protx: &ProTxHash, height: i32) -> bool {
        self.state
            .lock()
            .store
            .infos
            .get_mut(protx)
            .is_some_and(|i| i.set_platform_ban(false, height))
    }

    /// Applies the chain's [`Effect::ResetPlatformBan`] (EV2b emits it from `ProcessBlock` for a
    /// ProUpServTx revive, before the block can fail; CH7a's router applies it in emission order
    /// and regardless of the block's result, A1 §2.10.1). Other effects are not this store's;
    /// the result is `ResetPlatformBan`'s, which Core ignores.
    // Core: evo/deterministicmns.cpp:741-748
    pub fn apply_effect(&self, effect: &Effect) -> bool {
        match effect {
            Effect::ResetPlatformBan {
                pro_tx_hash,
                height,
            } => self.reset_platform_ban(pro_tx_hash, *height),
            _ => false,
        }
    }

    /// `SetPlatformBan(inv_hash, msg)` for a `platformban` message banning `protx` at
    /// `requested_height` (the message's fields; AM1 decodes it): the ban, and the message
    /// remembered under `inv_hash` when the ban took.
    // Core: masternode/meta.cpp:197-208
    pub fn set_platform_ban(
        &self,
        inv_hash: [u8; 32],
        protx: &ProTxHash,
        requested_height: i32,
        msg: PlatformBan,
    ) -> bool {
        let mut st = self.state.lock();
        let took = st.info(protx).set_platform_ban(true, requested_height);
        if took {
            st.seen_bans.emplace(inv_hash, msg);
        }
        took
    }

    /// `AlreadyHavePlatformBan` (a use of the cache entry).
    // Core: masternode/meta.cpp:210-214
    pub fn already_have_platform_ban(&self, inv_hash: &[u8; 32]) -> bool {
        self.state.lock().seen_bans.get(inv_hash).is_some()
    }

    /// `GetPlatformBan`.
    // Core: masternode/meta.cpp:216-225
    pub fn platform_ban(&self, inv_hash: &[u8; 32]) -> Option<PlatformBan> {
        self.state.lock().seen_bans.get(inv_hash)
    }

    /// `AddUsedMasternode`: once.
    // Core: masternode/meta.cpp:227-234
    pub fn add_used_masternode(&self, protx: &ProTxHash) {
        let mut st = self.state.lock();
        if st.used_set.insert(*protx) {
            st.store.used_masternodes.push_back(*protx);
        }
    }

    /// `RemoveUsedMasternodes(count)`: the oldest `count`.
    // Core: masternode/meta.cpp:236-246
    pub fn remove_used_masternodes(&self, count: usize) {
        let mut st = self.state.lock();
        for _ in 0..count {
            let Some(front) = st.store.used_masternodes.pop_front() else {
                break;
            };
            st.used_set.remove(&front);
        }
    }

    /// `GetUsedMasternodesCount`.
    // Core: masternode/meta.cpp:248-252
    pub fn used_masternodes_count(&self) -> usize {
        self.state.lock().store.used_masternodes.len()
    }

    /// `IsUsedMasternode`.
    // Core: masternode/meta.cpp:254-258
    pub fn is_used_masternode(&self, protx: &ProTxHash) -> bool {
        self.state.lock().used_set.contains(protx)
    }
}

#[cfg(test)]
mod tests {
    use dash_types::codec::BaseCodec;

    use super::*;

    fn protx(n: u8) -> ProTxHash {
        ProTxHash::from([n; 32])
    }

    fn ban(n: u8, height: i32) -> PlatformBan {
        let mut bytes = vec![n; 32];
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&[0; 32 + 96]);
        PlatformBan::decode(&mut bytes.as_slice()).unwrap()
    }

    /// `SetPlatformBan`'s height rule: an older height never applies; at the recorded height a
    /// ban applies again but an unban does not.
    // Core: masternode/meta.h:73-84
    #[test]
    fn platform_ban_heights_follow_cores_rule() {
        let m = MasternodeMetaMan::new();
        let p = protx(1);
        assert!(!m.reset_platform_ban(&p, 10), "no entry: nothing to reset");
        assert!(m.snapshot().infos.is_empty(), "and none made");
        assert!(m.set_platform_ban([1; 32], &p, 10, ban(1, 10)));
        assert!(m.is_platform_banned(&p));
        assert!(!m.set_platform_ban([2; 32], &p, 9, ban(1, 9)), "older height");
        assert!(m.set_platform_ban([3; 32], &p, 10, ban(1, 10)), "a ban at the same height");
        assert!(!m.reset_platform_ban(&p, 10), "an unban at the same height");
        assert!(!m.apply_effect(&Effect::ResetPlatformBan {
            pro_tx_hash: p,
            height: 9,
        }));
        assert!(m.is_platform_banned(&p));
        assert!(m.apply_effect(&Effect::ResetPlatformBan {
            pro_tx_hash: p,
            height: 11,
        }));
        assert!(!m.is_platform_banned(&p));
        assert_eq!(m.info(&p).platform_ban_updated, 11);
        // A refused ban is not remembered as seen.
        assert!(m.already_have_platform_ban(&[1; 32]));
        assert!(!m.already_have_platform_ban(&[2; 32]));
        assert_eq!(m.platform_ban(&[3; 32]), Some(ban(1, 10)));
    }

    /// The seen-ban cache keeps the 900 most recently used entries once it passes 1800; a
    /// lookup counts as a use.
    // Core: unordered_lru_cache.h truncate_if_needed
    #[test]
    fn seen_bans_truncate_to_the_most_recently_used_half() {
        let m = MasternodeMetaMan::new();
        let key = |i: usize| {
            let mut k = [0u8; 32];
            k[..8].copy_from_slice(&(i as u64).to_le_bytes());
            k
        };
        for i in 0..2 * SEEN_BAN_INVENTORY_SIZE {
            assert!(m.set_platform_ban(key(i), &protx(1), 1, ban(1, 1)));
        }
        assert!(m.already_have_platform_ban(&key(0)), "key 0 is now the newest use");
        m.set_platform_ban(key(2 * SEEN_BAN_INVENTORY_SIZE), &protx(1), 1, ban(1, 1));
        let st = m.state.lock();
        assert_eq!(st.seen_bans.map.len(), SEEN_BAN_INVENTORY_SIZE);
        drop(st);
        assert!(m.already_have_platform_ban(&key(0)));
        assert!(m.already_have_platform_ban(&key(2 * SEEN_BAN_INVENTORY_SIZE)));
        assert!(!m.already_have_platform_ban(&key(1)));
        assert!(!m.already_have_platform_ban(&key(SEEN_BAN_INVENTORY_SIZE + 1)));
        assert!(m.already_have_platform_ban(&key(SEEN_BAN_INVENTORY_SIZE + 2)));
    }

    // Core: masternode/meta.cpp:93-128
    #[test]
    fn mixing_counters() {
        let m = MasternodeMetaMan::new();
        let (a, b) = (protx(1), protx(2));
        assert!(!m.is_mixing_threshold_exceeded(&a, 100), "unknown");
        m.disallow_mixing(&a);
        assert!(!m.is_mixing_threshold_exceeded(&a, 100), "last_dsq 0");
        m.allow_mixing(&a);
        assert_eq!((m.info(&a).last_dsq, m.info(&a).mixing_tx_count), (1, 0));
        // threshold = 1 + 10 / 5 = 3 > nDsqCount
        assert!(m.is_mixing_threshold_exceeded(&a, 10));
        m.allow_mixing(&b);
        m.allow_mixing(&b);
        assert!(!m.is_mixing_threshold_exceeded(&a, 10), "3 > 3 is false");
        assert!(m.is_mixing_threshold_exceeded(&a, 15));
        for _ in 0..MAX_MIXING_TXES {
            m.disallow_mixing(&a);
        }
        assert!(m.is_valid_for_mixing_txes(&a), "5 is still valid");
        m.disallow_mixing(&a);
        assert!(!m.is_valid_for_mixing_txes(&a));
        assert!(m.is_valid_for_mixing_txes(&protx(9)), "unknown, default");
    }

    // Core: masternode/meta.cpp:151-179; meta.h:70-71
    #[test]
    fn outbound_attempts() {
        let m = MasternodeMetaMan::new();
        let p = protx(1);
        for t in 1..=i64::from(MAX_FAILED_OUTBOUND_ATTEMPTS) {
            m.set_last_outbound_attempt(&p, t);
        }
        assert!(!m.outbound_failed_too_many_times(&p), "5 attempts");
        m.set_last_outbound_attempt(&p, 6);
        assert!(m.outbound_failed_too_many_times(&p));
        assert_eq!(m.last_outbound_attempt(&p), 6);
        m.set_last_outbound_success(&p, 7);
        assert!(!m.outbound_failed_too_many_times(&p));
        assert_eq!((m.last_outbound_success(&p), m.info(&p).outbound_attempt_count), (7, 0));
        assert_eq!(m.last_outbound_attempt(&protx(2)), 0);
    }

    // Core: masternode/meta.cpp:130-142, :227-258; meta.cpp:56-62
    #[test]
    fn governance_votes_and_used_masternodes() {
        let m = MasternodeMetaMan::new();
        m.add_governance_vote(&protx(1), [7; 32]);
        m.add_governance_vote(&protx(1), [7; 32]);
        m.add_governance_vote(&protx(2), [7; 32]);
        m.add_governance_vote(&protx(2), [8; 32]);
        assert_eq!(m.info(&protx(1)).governance_objects_voted_on[&[7; 32]], 2);
        m.remove_governance_object(&[7; 32]);
        assert!(m.info(&protx(1)).governance_objects_voted_on.is_empty());
        assert_eq!(m.info(&protx(2)).governance_objects_voted_on.len(), 1);

        for n in [3, 4, 3, 5] {
            m.add_used_masternode(&protx(n));
        }
        assert_eq!(m.used_masternodes_count(), 3, "once each");
        m.remove_used_masternodes(2);
        assert!(!m.is_used_masternode(&protx(3)) && !m.is_used_masternode(&protx(4)));
        assert!(m.is_used_masternode(&protx(5)));
        m.remove_used_masternodes(10);
        assert_eq!(m.used_masternodes_count(), 0);
    }

    /// The store round-trips; a duplicate used masternode in the file stays in the queue and once
    /// in the set; another version string loads empty but keeps reading nothing else.
    // Core: masternode/meta.h:102-147
    #[test]
    fn the_store_round_trips_and_reloads_the_used_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let ms = [1, 2, 3, 4];
        let m = MasternodeMetaMan::new();
        assert!(m.load_cache(&path, ms, true), "missing: recreated");
        m.allow_mixing(&protx(1));
        m.add_governance_vote(&protx(2), [9; 32]);
        m.set_platform_ban([1; 32], &protx(2), 5, ban(2, 5));
        m.set_last_outbound_attempt(&protx(3), 77);
        m.add_used_masternode(&protx(4));
        assert!(m.store(&path, ms));
        let back = MasternodeMetaMan::new();
        assert!(back.load_cache(&path, ms, true));
        assert_eq!(back.snapshot(), m.snapshot());
        assert!(back.is_used_masternode(&protx(4)));
        assert!(!back.already_have_platform_ban(&[1; 32]), "the seen bans are not stored");

        let mut store = m.snapshot();
        store.used_masternodes.push_back(protx(4));
        std::fs::write(&path, flatdb::encode(&store, ms)).unwrap();
        let dup = MasternodeMetaMan::new();
        assert!(dup.load_cache(&path, ms, true));
        assert_eq!(dup.used_masternodes_count(), 2);
        dup.remove_used_masternodes(1);
        assert!(!dup.is_used_masternode(&protx(4)), "the set drops it with the first copy");

        // `load_cache(false)` stores the empty manager over the file.
        let fresh = MasternodeMetaMan::new();
        assert!(fresh.load_cache(&path, ms, false));
        assert_eq!(
            flatdb::decode::<MasternodeMetaStore>(&std::fs::read(&path).unwrap(), ms).unwrap(),
            MasternodeMetaStore::default()
        );
    }

    #[test]
    fn another_version_loads_empty_and_a_cut_body_keeps_the_dsq_count() {
        let mut store = MasternodeMetaStore {
            dsq_count: 42,
            ..MasternodeMetaStore::default()
        };
        store.infos.insert(protx(1), MasternodeMetaInfo::new(protx(1)));
        let mut body = Vec::new();
        store.write(&mut body);
        // Another version string: cleared infos, nothing else read, the load succeeds.
        let mut other = Vec::new();
        push_str(&mut other, b"MasternodeMetaStore-0");
        other.extend_from_slice(&body[1 + VERSION.len()..]);
        let mut target = MasternodeMetaStore {
            dsq_count: 7,
            ..store.clone()
        };
        assert_eq!(target.read(&mut other.as_slice()), Ok(()));
        assert!(target.infos.is_empty());
        assert_eq!(target.dsq_count, 7);
        // Cut inside the used list: `nDsqCount` was read in place before the throw.
        // The used list's count says one, and the hash is missing.
        let mut cut = body.clone();
        *cut.last_mut().unwrap() = 1;
        let mut target = MasternodeMetaStore::default();
        assert_eq!(target.read(&mut cut.as_slice()), Err(ReadError::EndOfData));
        assert_eq!(target.dsq_count, 42);
        assert!(target.infos.is_empty());
    }
}
