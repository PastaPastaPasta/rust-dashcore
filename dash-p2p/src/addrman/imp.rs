//! `AddrInfo` and `AddrManImpl` (Core `addrman_impl.h`, `addrman.cpp`), one function per Core
//! function, in Core's order and with Core's names in the anchors.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use dash_p2p_core::ServiceFlags;

use super::stdmap::StdMap;
use super::{AddrEntry, AddressPosition};
use crate::netaddr::{Address, NetAddr, Network, Service};
use crate::netgroup::NetGroupManager;
use crate::rng::{FastRandom, RandSource, get_rand};

/// `ADDRMAN_TRIED_BUCKET_COUNT` (`addrman_impl.h:28`).
pub(super) const TRIED_BUCKET_COUNT: usize = 1 << 8;
/// `ADDRMAN_NEW_BUCKET_COUNT` (`addrman_impl.h:31`).
pub(super) const NEW_BUCKET_COUNT: usize = 1 << 10;
/// `ADDRMAN_BUCKET_SIZE` (`addrman_impl.h:34`).
pub(super) const BUCKET_SIZE: usize = 1 << 6;
/// `ADDRMAN_TRIED_BUCKETS_PER_GROUP` (`addrman.cpp:30`).
const TRIED_BUCKETS_PER_GROUP: u64 = 8;
/// `ADDRMAN_NEW_BUCKETS_PER_SOURCE_GROUP` (`addrman.cpp:32`).
const NEW_BUCKETS_PER_SOURCE_GROUP: u64 = 64;
/// `ADDRMAN_NEW_BUCKETS_PER_ADDRESS` (`addrman.cpp:34`).
pub(super) const NEW_BUCKETS_PER_ADDRESS: i32 = 8;
/// `ADDRMAN_HORIZON` (`addrman.cpp:36`), seconds.
const HORIZON: i64 = 30 * 24 * 3600;
/// `ADDRMAN_RETRIES` (`addrman.cpp:38`).
const RETRIES: i32 = 3;
/// `ADDRMAN_MAX_FAILURES` (`addrman.cpp:40`).
const MAX_FAILURES: i32 = 10;
/// `ADDRMAN_MIN_FAIL` (`addrman.cpp:42`), seconds.
const MIN_FAIL: i64 = 7 * 24 * 3600;
/// `ADDRMAN_REPLACEMENT` (`addrman.cpp:44`), seconds.
const REPLACEMENT: i64 = 4 * 3600;
/// `ADDRMAN_SET_TRIED_COLLISION_SIZE` (`addrman.cpp:46`).
const SET_TRIED_COLLISION_SIZE: usize = 10;
/// `ADDRMAN_TEST_WINDOW` (`addrman.cpp:48`), seconds.
const TEST_WINDOW: i64 = 40 * 60;
/// `CAddress::TIME_INIT` (`protocol.h:400`).
pub(super) const TIME_INIT: i64 = 100_000_000;

/// `pow(0.66, n)` for `n` in 0..=8 as glibc computes it (each is the correctly rounded value of
/// the exact power of the double `0.66`; checked against glibc 2.43 and exact rationals).
const CHANCE_TABLE: [f64; 9] = [
    f64::from_bits(0x3ff0_0000_0000_0000),
    f64::from_bits(0x3fe5_1eb8_51eb_851f),
    f64::from_bits(0x3fdb_e0de_d288_ce71),
    f64::from_bits(0x3fd2_6655_9f6e_c5b1),
    f64::from_bits(0x3fc8_49a4_38d9_e63c),
    f64::from_bits(0x3fc0_07a4_b4e1_bbcc),
    f64::from_bits(0x3fb5_28cf_2c34_3555),
    f64::from_bits(0x3fab_ee30_301b_f47b),
    f64::from_bits(0x3fa2_6f1f_cdd5_02a3),
];

/// Core's `pow(0.66, n)` for `n ≤ 8` (`GetChance`). Non-negative `n` reads the table; a
/// negative `nAttempts` (only from a crafted `peers.dat`) calls the platform `pow`, which is
/// glibc's on Linux, as dashd's is.
pub(super) fn pow_066(n: i32) -> f64 {
    match usize::try_from(n) {
        Ok(i) => CHANCE_TABLE[i],
        Err(_) => 0.66f64.powf(f64::from(n)),
    }
}

/// `HashWriter` for the cheap hashes: SHA-256d over Core's serialization of the operands.
pub(super) struct HashWriter(Vec<u8>);

impl HashWriter {
    pub(super) fn new() -> Self {
        Self(Vec::with_capacity(96))
    }

    pub(super) fn raw(mut self, bytes: &[u8]) -> Self {
        self.0.extend_from_slice(bytes);
        self
    }

    /// A `std::vector<unsigned char>`: CompactSize length, then the bytes.
    pub(super) fn vec(mut self, bytes: &[u8]) -> Self {
        super::ser::write_compact_size(&mut self.0, bytes.len() as u64);
        self.0.extend_from_slice(bytes);
        self
    }

    /// `HashWriter::GetCheapHash`: the first 8 bytes of SHA-256d, little-endian.
    pub(super) fn cheap(&self) -> u64 {
        let hash = dash_sha256::sha256d(&self.0);
        u64::from_le_bytes(hash[..8].try_into().expect("8 bytes"))
    }
}

/// Core's `AddrInfo`: a `CAddress` with AddrMan's statistics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AddrInfo {
    /// The `CService` part of the `CAddress`.
    pub(super) service: Service,
    /// `nTime` (`NodeSeconds`; serialized as a lossy `uint32_t`).
    pub(super) time: i64,
    /// `nServices`.
    pub(super) services: ServiceFlags,
    /// `m_last_try` (memory only).
    pub(super) last_try: i64,
    /// `m_last_count_attempt` (memory only).
    pub(super) last_count_attempt: i64,
    /// `source`.
    pub(super) source: NetAddr,
    /// `m_last_success`.
    pub(super) last_success: i64,
    /// `nAttempts`.
    pub(super) attempts: i32,
    /// `nRefCount` (memory only).
    pub(super) ref_count: i32,
    /// `fInTried` (memory only).
    pub(super) in_tried: bool,
    /// `nRandomPos`.
    pub(super) random_pos: i32,
}

impl Default for AddrInfo {
    /// `AddrInfo()`: a default `CAddress` (`[::]:0`, `TIME_INIT`, `NODE_NONE`).
    fn default() -> Self {
        Self {
            service: Service::default(),
            time: TIME_INIT,
            services: ServiceFlags::NONE,
            last_try: 0,
            last_count_attempt: 0,
            source: NetAddr::default(),
            last_success: 0,
            attempts: 0,
            ref_count: 0,
            in_tried: false,
            random_pos: -1,
        }
    }
}

impl AddrInfo {
    /// `AddrInfo(addrIn, addrSource)`.
    pub(super) fn new(addr: &Address, source: &NetAddr) -> Self {
        Self {
            service: addr.service.clone(),
            time: i64::from(addr.time),
            services: addr.services,
            source: source.clone(),
            ..Self::default()
        }
    }

    /// The `CAddress` slice (`nTime` truncated to Core's `uint32_t` wire width).
    pub(super) fn address(&self) -> Address {
        Address {
            service: self.service.clone(),
            services: self.services,
            time: self.time as u32,
        }
    }

    pub(super) fn entry(&self) -> AddrEntry {
        AddrEntry {
            service: self.service.clone(),
            services: self.services,
            time: self.time,
            source: self.source.clone(),
            last_try: self.last_try,
            last_count_attempt: self.last_count_attempt,
            last_success: self.last_success,
            attempts: self.attempts,
            ref_count: self.ref_count,
            in_tried: self.in_tried,
            random_pos: self.random_pos,
        }
    }

    // Core: addrman.cpp:50 AddrInfo::GetTriedBucket
    pub(super) fn tried_bucket(&self, key: &[u8; 32], netgroup: &NetGroupManager) -> i32 {
        let hash1 = HashWriter::new().raw(key).vec(&self.service.key()).cheap();
        let hash2 = HashWriter::new()
            .raw(key)
            .vec(&netgroup.group(&self.service.addr))
            .raw(&(hash1 % TRIED_BUCKETS_PER_GROUP).to_le_bytes())
            .cheap();
        (hash2 % TRIED_BUCKET_COUNT as u64) as i32
    }

    // Core: addrman.cpp:57 AddrInfo::GetNewBucket
    pub(super) fn new_bucket_from(
        &self,
        key: &[u8; 32],
        src: &NetAddr,
        netgroup: &NetGroupManager,
    ) -> i32 {
        let source_group = netgroup.group(src);
        let hash1 = HashWriter::new()
            .raw(key)
            .vec(&netgroup.group(&self.service.addr))
            .vec(&source_group)
            .cheap();
        let hash2 = HashWriter::new()
            .raw(key)
            .vec(&source_group)
            .raw(&(hash1 % NEW_BUCKETS_PER_SOURCE_GROUP).to_le_bytes())
            .cheap();
        (hash2 % NEW_BUCKET_COUNT as u64) as i32
    }

    /// `GetNewBucket(nKey, netgroupman)`: by the entry's own source.
    // Core: addrman_impl.h:81 AddrInfo::GetNewBucket
    pub(super) fn new_bucket(&self, key: &[u8; 32], netgroup: &NetGroupManager) -> i32 {
        self.new_bucket_from(key, &self.source, netgroup)
    }

    // Core: addrman.cpp:65 AddrInfo::GetBucketPosition
    pub(super) fn bucket_position(&self, key: &[u8; 32], f_new: bool, bucket: i32) -> i32 {
        let tag = if f_new {
            b'N'
        } else {
            b'K'
        };
        let hash1 = HashWriter::new()
            .raw(key)
            .raw(&[tag])
            .raw(&bucket.to_le_bytes())
            .vec(&self.service.key())
            .cheap();
        (hash1 % BUCKET_SIZE as u64) as i32
    }

    // Core: addrman.cpp:71 AddrInfo::IsTerrible
    pub(super) fn is_terrible(&self, now: i64) -> bool {
        if now - self.last_try <= 60 {
            // never remove things tried in the last minute
            return false;
        }
        if self.time > now + 10 * 60 {
            // came in a flying DeLorean
            return true;
        }
        if now - self.time > HORIZON {
            // not seen in recent history
            return true;
        }
        if self.last_success == 0 && self.attempts >= RETRIES {
            // tried N times and never a success
            return true;
        }
        if now - self.last_success > MIN_FAIL && self.attempts >= MAX_FAILURES {
            // N successive failures in the last week
            return true;
        }
        false
    }

    // Core: addrman.cpp:96 AddrInfo::GetChance
    pub(super) fn chance(&self, now: i64) -> f64 {
        let mut chance = 1.0f64;
        // deprioritize very recent attempts away
        if now - self.last_try < 10 * 60 {
            chance *= 0.01;
        }
        // deprioritize 66% after each failed attempt, but at most 1/28th
        chance *= pow_066(self.attempts.min(8));
        chance
    }
}

/// `AddrManImpl::NewTriedCount`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct NewTriedCount {
    pub(super) n_new: usize,
    pub(super) n_tried: usize,
}

/// Core's `AddrManImpl` (the state behind `cs`).
pub(super) struct Impl {
    /// `insecure_rand`.
    pub(super) rng: FastRandom,
    /// `nKey`.
    pub(super) key: [u8; 32],
    /// `nIdCount`.
    pub(super) id_count: i32,
    /// `mapInfo`, iterated in libstdc++'s order.
    pub(super) map_info: StdMap<AddrInfo>,
    /// `mapAddr` (never iterated).
    pub(super) map_addr: HashMap<Service, i32>,
    /// `vRandom`.
    pub(super) random: Vec<i32>,
    /// `nTried`.
    pub(super) n_tried: i32,
    /// `vvTried`, flat: `bucket * BUCKET_SIZE + position`.
    pub(super) tried: Box<[i32]>,
    /// `nNew`.
    pub(super) n_new: i32,
    /// `vvNew`, flat.
    pub(super) new: Box<[i32]>,
    /// `m_last_good`.
    pub(super) last_good: i64,
    /// `m_tried_collisions`.
    pub(super) tried_collisions: BTreeSet<i32>,
    /// `m_consistency_check_ratio`.
    pub(super) check_ratio: u32,
    /// `m_netgroupman`.
    pub(super) netgroup: Arc<NetGroupManager>,
    /// `-allowprivatenet`, for `GetNetwork`, `IsRoutable` and `GetNetClass`.
    pub(super) allow_private_net: bool,
    /// `m_network_counts`: `None` where Core's map has no entry (`Select`, `Size` and
    /// `CheckAddrman`'s −20 tell the difference).
    pub(super) network_counts: [Option<NewTriedCount>; Network::COUNT],
}

pub(super) fn slot_of(bucket: i32, position: i32) -> usize {
    bucket as usize * BUCKET_SIZE + position as usize
}

impl Impl {
    // Core: addrman.cpp:111 AddrManImpl::AddrManImpl
    pub(super) fn new(
        netgroup: Arc<NetGroupManager>,
        deterministic: bool,
        check_ratio: u32,
        allow_private_net: bool,
        rand: &Arc<dyn RandSource>,
    ) -> Self {
        // Core's member `insecure_rand{deterministic}`; `nKey`'s `rand256` seeds it at once.
        let mut rng = if deterministic {
            FastRandom::deterministic()
        } else {
            FastRandom::lazy(Arc::clone(rand))
        };
        let key = if deterministic {
            let mut one = [0u8; 32];
            one[0] = 1;
            one
        } else {
            rng.rand256()
        };
        // `mapAddr`'s `CServiceHash` then takes its two SipHash salts, a `GetRand<uint64_t>()`
        // each, deterministic or not (netaddress.h:576-578; members initialize in declaration
        // order, addrman_impl.h:158-193). Core only looks up through that map, so the salts show
        // nowhere but as two draws on the source; `map_addr` keeps Rust's hasher.
        for _ in 0..2 {
            get_rand(rand, u64::MAX);
        }
        Self::with_context(rng, key, netgroup, check_ratio, allow_private_net)
    }

    /// An empty table on `rng` and `key`, drawing nothing.
    pub(super) fn with_context(
        rng: FastRandom,
        key: [u8; 32],
        netgroup: Arc<NetGroupManager>,
        check_ratio: u32,
        allow_private_net: bool,
    ) -> Self {
        Self {
            rng,
            key,
            id_count: 0,
            map_info: StdMap::default(),
            map_addr: HashMap::new(),
            random: Vec::new(),
            n_tried: 0,
            tried: vec![-1; TRIED_BUCKET_COUNT * BUCKET_SIZE].into_boxed_slice(),
            n_new: 0,
            new: vec![-1; NEW_BUCKET_COUNT * BUCKET_SIZE].into_boxed_slice(),
            last_good: 1,
            tried_collisions: BTreeSet::new(),
            check_ratio,
            netgroup,
            allow_private_net,
            network_counts: [None; Network::COUNT],
        }
    }

    /// `GetNetwork()` of an address under this AddrMan's `-allowprivatenet`.
    pub(super) fn network_of(&self, addr: &NetAddr) -> Network {
        addr.network(self.allow_private_net)
    }

    /// `m_network_counts[net]` (`operator[]`: creates the entry).
    pub(super) fn counts_mut(&mut self, net: Network) -> &mut NewTriedCount {
        self.network_counts[net as usize].get_or_insert_with(NewTriedCount::default)
    }

    pub(super) fn info(&self, id: i32) -> &AddrInfo {
        self.map_info.get(id).expect("id in mapInfo")
    }

    pub(super) fn info_mut(&mut self, id: i32) -> &mut AddrInfo {
        self.map_info.get_mut(id).expect("id in mapInfo")
    }

    // Core: addrman.cpp:410 AddrManImpl::Find
    pub(super) fn find(&self, addr: &Service) -> Option<i32> {
        let id = *self.map_addr.get(addr)?;
        self.map_info.contains(id).then_some(id)
    }

    // Core: addrman.cpp:423 AddrManImpl::Create
    fn create(&mut self, addr: &Address, source: &NetAddr) -> i32 {
        let id = self.id_count;
        self.id_count += 1;
        let mut info = AddrInfo::new(addr, source);
        info.random_pos = self.random.len() as i32;
        self.map_info.insert_new(id, info);
        self.map_addr.insert(addr.service.clone(), id);
        self.random.push(id);
        self.n_new += 1;
        let net = self.network_of(&addr.service.addr);
        self.counts_mut(net).n_new += 1;
        id
    }

    // Core: addrman.cpp:439 AddrManImpl::SwapRandom
    pub(super) fn swap_random(&mut self, pos1: usize, pos2: usize) {
        if pos1 == pos2 {
            return;
        }
        assert!(pos1 < self.random.len() && pos2 < self.random.len());
        let id1 = self.random[pos1];
        let id2 = self.random[pos2];
        self.info_mut(id1).random_pos = pos2 as i32;
        self.info_mut(id2).random_pos = pos1 as i32;
        self.random[pos1] = id2;
        self.random[pos2] = id1;
    }

    // Core: addrman.cpp:463 AddrManImpl::Delete
    pub(super) fn delete(&mut self, id: i32) {
        let info = self.map_info.get(id).expect("Delete of an id not in mapInfo");
        assert!(!info.in_tried);
        assert!(info.ref_count == 0);
        let random_pos = info.random_pos as usize;
        let net = self.network_of(&info.service.addr);
        let service = info.service.clone();
        self.swap_random(random_pos, self.random.len() - 1);
        let counts = self.counts_mut(net);
        counts.n_new = counts.n_new.wrapping_sub(1);
        self.random.pop();
        self.map_addr.remove(&service);
        self.map_info.remove(id);
        self.n_new -= 1;
    }

    // Core: addrman.cpp:480 AddrManImpl::ClearNew
    fn clear_new(&mut self, bucket: i32, position: i32) {
        // if there is an entry in the specified bucket, delete it.
        let id_delete = self.new[slot_of(bucket, position)];
        if id_delete != -1 {
            let info = self.info_mut(id_delete);
            assert!(info.ref_count > 0);
            info.ref_count -= 1;
            let ref_count = info.ref_count;
            self.new[slot_of(bucket, position)] = -1;
            if ref_count == 0 {
                self.delete(id_delete);
            }
        }
    }

    // Core: addrman.cpp:498 AddrManImpl::MakeTried
    fn make_tried(&mut self, id: i32) {
        let key = self.key;
        let netgroup = Arc::clone(&self.netgroup);

        // remove the entry from all new buckets
        let start_bucket = self.info(id).new_bucket(&key, &netgroup);
        for n in 0..NEW_BUCKET_COUNT as i32 {
            let bucket = (start_bucket + n) % NEW_BUCKET_COUNT as i32;
            let pos = self.info(id).bucket_position(&key, true, bucket);
            if self.new[slot_of(bucket, pos)] == id {
                self.new[slot_of(bucket, pos)] = -1;
                let info = self.info_mut(id);
                info.ref_count -= 1;
                if info.ref_count == 0 {
                    break;
                }
            }
        }
        self.n_new -= 1;
        let net = self.network_of(&self.info(id).service.addr);
        let counts = self.counts_mut(net);
        counts.n_new = counts.n_new.wrapping_sub(1);

        assert!(self.info(id).ref_count == 0);

        // which tried bucket to move the entry to
        let k_bucket = self.info(id).tried_bucket(&key, &netgroup);
        let k_bucket_pos = self.info(id).bucket_position(&key, false, k_bucket);

        // first make space to add it (the existing tried entry there is moved to new, deleting
        // whatever is there).
        let id_evict = self.tried[slot_of(k_bucket, k_bucket_pos)];
        if id_evict != -1 {
            assert!(self.map_info.contains(id_evict));

            // Remove the to-be-evicted item from the tried set.
            self.info_mut(id_evict).in_tried = false;
            self.tried[slot_of(k_bucket, k_bucket_pos)] = -1;
            self.n_tried -= 1;
            let old_net = self.network_of(&self.info(id_evict).service.addr);
            let counts = self.counts_mut(old_net);
            counts.n_tried = counts.n_tried.wrapping_sub(1);

            // find which new bucket it belongs to
            let u_bucket = self.info(id_evict).new_bucket(&key, &netgroup);
            let u_bucket_pos = self.info(id_evict).bucket_position(&key, true, u_bucket);
            self.clear_new(u_bucket, u_bucket_pos);
            assert!(self.new[slot_of(u_bucket, u_bucket_pos)] == -1);

            // Enter it into the new set again.
            self.info_mut(id_evict).ref_count = 1;
            self.new[slot_of(u_bucket, u_bucket_pos)] = id_evict;
            self.n_new += 1;
            self.counts_mut(old_net).n_new += 1;
        }
        assert!(self.tried[slot_of(k_bucket, k_bucket_pos)] == -1);

        self.tried[slot_of(k_bucket, k_bucket_pos)] = id;
        self.n_tried += 1;
        self.info_mut(id).in_tried = true;
        self.counts_mut(net).n_tried += 1;
    }

    // Core: addrman.cpp:557 AddrManImpl::AddSingle
    fn add_single(
        &mut self,
        addr: &Address,
        source: &NetAddr,
        time_penalty: i64,
        now: i64,
    ) -> bool {
        if !addr.service.addr.is_routable(self.allow_private_net) {
            return false;
        }

        let found = self.find(&addr.service);

        // Do not set a penalty for a source's self-announcement
        let time_penalty = if addr.service.addr == *source {
            0
        } else {
            time_penalty
        };
        let addr_time = i64::from(addr.time);

        let id = match found {
            Some(id) => {
                let info = self.info_mut(id);
                // periodically update nTime
                let currently_online = now - addr_time < 24 * 3600;
                let update_interval = if currently_online {
                    3600
                } else {
                    24 * 3600
                };
                if info.time < addr_time - update_interval - time_penalty {
                    info.time = (addr_time - time_penalty).max(0);
                }

                // add services
                info.services |= addr.services;

                // do not update if no new information is present
                if addr_time <= info.time {
                    return false;
                }

                // do not update if the entry was already in the "tried" table
                if info.in_tried {
                    return false;
                }

                // do not update if the max reference count is reached
                if info.ref_count == NEW_BUCKETS_PER_ADDRESS {
                    return false;
                }

                // stochastic test: previous nRefCount == N: 2^N times harder to increase it
                if info.ref_count > 0 {
                    let factor = 1u64 << info.ref_count;
                    if self.rng.randrange(factor) != 0 {
                        return false;
                    }
                }
                id
            }
            None => {
                let id = self.create(addr, source);
                let info = self.info_mut(id);
                info.time = (info.time - time_penalty).max(0);
                id
            }
        };

        let key = self.key;
        let u_bucket = self.info(id).new_bucket_from(&key, source, &self.netgroup);
        let u_bucket_pos = self.info(id).bucket_position(&key, true, u_bucket);
        let occupant = self.new[slot_of(u_bucket, u_bucket_pos)];
        let mut insert = occupant == -1;
        if occupant != id {
            if !insert {
                let existing = self.info(occupant);
                if existing.is_terrible(now)
                    || (existing.ref_count > 1 && self.info(id).ref_count == 0)
                {
                    // Overwrite the existing new table entry.
                    insert = true;
                }
            }
            if insert {
                self.clear_new(u_bucket, u_bucket_pos);
                self.info_mut(id).ref_count += 1;
                self.new[slot_of(u_bucket, u_bucket_pos)] = id;
            } else if self.info(id).ref_count == 0 {
                self.delete(id);
            }
        }
        insert
    }

    // Core: addrman.cpp:632 AddrManImpl::Good_
    pub(super) fn good_(&mut self, addr: &Service, test_before_evict: bool, time: i64) -> bool {
        self.last_good = time;

        // if not found, bail out
        let Some(id) = self.find(addr) else {
            return false;
        };

        // update info
        let info = self.info_mut(id);
        info.last_success = time;
        info.last_try = time;
        info.attempts = 0;
        // nTime is not updated here, to avoid leaking information about
        // currently-connected peers.

        // if it is already in the tried set, don't do anything else
        if info.in_tried {
            return false;
        }

        // if it is not in new, something bad happened
        if info.ref_count <= 0 {
            return false;
        }

        // which tried bucket to move the entry to
        let key = self.key;
        let tried_bucket = self.info(id).tried_bucket(&key, &self.netgroup);
        let tried_bucket_pos = self.info(id).bucket_position(&key, false, tried_bucket);

        // Will moving this address into tried evict another entry?
        if test_before_evict && self.tried[slot_of(tried_bucket, tried_bucket_pos)] != -1 {
            if self.tried_collisions.len() < SET_TRIED_COLLISION_SIZE {
                self.tried_collisions.insert(id);
            }
            false
        } else {
            // move nId to the tried tables
            self.make_tried(id);
            true
        }
    }

    // Core: addrman.cpp:690 AddrManImpl::Add_
    pub(super) fn add_(
        &mut self,
        addrs: &[Address],
        source: &NetAddr,
        time_penalty: i64,
        now: i64,
    ) -> bool {
        let mut added = 0;
        for addr in addrs {
            if self.add_single(addr, source, time_penalty, now) {
                added += 1;
            }
        }
        added > 0
    }

    // Core: addrman.cpp:702 AddrManImpl::Attempt_
    pub(super) fn attempt_(&mut self, addr: &Service, count_failure: bool, time: i64) {
        // if not found, bail out
        let Some(id) = self.find(addr) else {
            return;
        };
        let last_good = self.last_good;
        let info = self.info_mut(id);

        // update info
        info.last_try = time;
        if count_failure && info.last_count_attempt < last_good {
            info.last_count_attempt = time;
            // int arithmetic: nAttempts comes from peers.dat unchecked and wraps as in Core.
            info.attempts = info.attempts.wrapping_add(1);
        }
    }

    // Core: addrman.cpp:722 AddrManImpl::Select_
    pub(super) fn select_(
        &mut self,
        new_only: bool,
        network: Option<Network>,
        now: i64,
    ) -> Option<(Address, i64)> {
        if self.random.is_empty() {
            return None;
        }

        let mut new_count = self.n_new as usize;
        let mut tried_count = self.n_tried as usize;

        if let Some(net) = network {
            let counts = self.network_counts[net as usize]?;
            new_count = counts.n_new;
            tried_count = counts.n_tried;
        }

        if new_only && new_count == 0 {
            return None;
        }
        if new_count.wrapping_add(tried_count) == 0 {
            return None;
        }

        // Decide if we are going to search the new or tried table
        // If either option is viable, use a 50% chance to choose
        let search_tried = if new_only || tried_count == 0 {
            false
        } else if new_count == 0 {
            true
        } else {
            self.rng.randbool()
        };

        let bucket_count = if search_tried {
            TRIED_BUCKET_COUNT
        } else {
            NEW_BUCKET_COUNT
        };

        // Loop through the addrman table until we find an appropriate entry
        let mut chance_factor = 1.0f64;
        loop {
            // Pick a bucket, and an initial position in that bucket.
            let bucket = self.rng.randrange(bucket_count as u64) as usize;
            let initial_position = self.rng.randrange(BUCKET_SIZE as u64) as usize;

            // Iterate over the positions of that bucket, starting at the initial one,
            // and looping around.
            let mut node_id = -1;
            let mut found = false;
            for i in 0..BUCKET_SIZE {
                let position = (initial_position + i) % BUCKET_SIZE;
                node_id = self.get_entry(search_tried, bucket, position);
                if node_id != -1 {
                    match network {
                        Some(net) => {
                            let info = self.map_info.get(node_id);
                            if info.is_some_and(|info| self.network_of(&info.service.addr) == net) {
                                found = true;
                                break;
                            }
                        }
                        None => {
                            found = true;
                            break;
                        }
                    }
                }
            }

            // If the bucket is entirely empty, start over with a (likely) different one.
            if !found {
                continue;
            }

            // Find the entry to return.
            let info = self.info(node_id);
            let chance = info.chance(now);
            let result = (info.address(), info.last_try);

            // With probability GetChance() * chance_factor, return the entry.
            let draw = self.rng.randbits(30) as f64;
            if draw < chance_factor * chance * f64::from(1u32 << 30) {
                return Some(result);
            }

            // Otherwise start over with a (likely) different bucket, and increased chance factor.
            chance_factor *= 1.2;
        }
    }

    // Core: addrman.cpp:798 AddrManImpl::GetEntry
    fn get_entry(&self, use_tried: bool, bucket: usize, position: usize) -> i32 {
        if use_tried {
            if position < BUCKET_SIZE && bucket < TRIED_BUCKET_COUNT {
                return self.tried[bucket * BUCKET_SIZE + position];
            }
        } else if position < BUCKET_SIZE && bucket < NEW_BUCKET_COUNT {
            return self.new[bucket * BUCKET_SIZE + position];
        }
        -1
    }

    // Core: addrman.cpp:815 AddrManImpl::GetAddr_
    pub(super) fn get_addr_(
        &mut self,
        max_addresses: usize,
        max_pct: usize,
        network: Option<Network>,
        filtered: bool,
        now: i64,
    ) -> Vec<Address> {
        let mut nodes = self.random.len();
        if max_pct != 0 {
            // size_t arithmetic: wraps as in Core.
            nodes = max_pct.wrapping_mul(nodes) / 100;
        }
        if max_addresses != 0 {
            nodes = nodes.min(max_addresses);
        }

        // gather a list of random nodes, skipping those of low quality
        let mut addresses = Vec::new();
        let mut n = 0usize;
        while n < self.random.len() {
            if addresses.len() >= nodes {
                break;
            }

            let rnd_pos = self.rng.randrange((self.random.len() - n) as u64) as usize + n;
            self.swap_random(n, rnd_pos);
            let info = self.info(self.random[n]);

            // Filter by network (optional)
            let skip_net = network
                .is_some_and(|net| info.service.addr.net_class(self.allow_private_net) != net);
            // Filter for quality
            if !skip_net && !(info.is_terrible(now) && filtered) {
                addresses.push(info.address());
            }
            n += 1;
        }
        addresses
    }

    // Core: addrman.cpp:853 AddrManImpl::GetEntries_
    pub(super) fn get_entries_(&self, from_tried: bool) -> Vec<(AddrEntry, AddressPosition)> {
        let bucket_count = if from_tried {
            TRIED_BUCKET_COUNT
        } else {
            NEW_BUCKET_COUNT
        };
        let mut infos = Vec::new();
        for bucket in 0..bucket_count {
            for position in 0..BUCKET_SIZE {
                let id = self.get_entry(from_tried, bucket, position);
                if id >= 0 {
                    let info = self.info(id);
                    let location = AddressPosition {
                        tried: from_tried,
                        multiplicity: if from_tried {
                            1
                        } else {
                            info.ref_count
                        },
                        bucket: bucket as i32,
                        position: position as i32,
                    };
                    infos.push((info.entry(), location));
                }
            }
        }
        infos
    }

    // Core: addrman.cpp:877 AddrManImpl::Connected_
    pub(super) fn connected_(&mut self, addr: &Service, time: i64) {
        // if not found, bail out
        let Some(id) = self.find(addr) else {
            return;
        };
        let info = self.info_mut(id);
        // update info
        let update_interval = 20 * 60;
        if time - info.time > update_interval {
            info.time = time;
        }
    }

    // Core: addrman.cpp:896 AddrManImpl::SetServices_
    pub(super) fn set_services_(&mut self, addr: &Service, services: ServiceFlags) {
        // if not found, bail out
        let Some(id) = self.find(addr) else {
            return;
        };
        // update info
        self.info_mut(id).services = services;
    }

    // Core: addrman.cpp:912 AddrManImpl::GetAddressInfo_
    pub(super) fn get_address_info_(&self, addr: &Service) -> Option<AddrEntry> {
        self.find(addr).map(|id| self.info(id).entry())
    }

    // Core: addrman.cpp:923 AddrManImpl::ResolveCollisions_
    pub(super) fn resolve_collisions_(&mut self, now: i64) {
        // Good_ with test_before_evict = false never inserts, so iterating a snapshot of the
        // ordered set visits exactly what Core's erase-while-iterating loop visits.
        let ids: Vec<i32> = self.tried_collisions.iter().copied().collect();
        for id_new in ids {
            let mut erase_collision = false;

            // If id_new not found in mapInfo remove it from m_tried_collisions
            if !self.map_info.contains(id_new) {
                erase_collision = true;
            } else {
                let key = self.key;
                let info_new = self.info(id_new);

                // Which tried bucket to move the entry to.
                let tried_bucket = info_new.tried_bucket(&key, &self.netgroup);
                let tried_bucket_pos = info_new.bucket_position(&key, false, tried_bucket);
                let id_old = self.tried[slot_of(tried_bucket, tried_bucket_pos)];
                if !info_new.service.addr.is_valid() {
                    // id_new may no longer map to a valid address
                    erase_collision = true;
                } else if id_old != -1 {
                    // The position in the tried bucket is not empty

                    // Get the to-be-evicted address that is being tested
                    let info_old = self.info(id_old);
                    let current_time = now;
                    let new_service = info_new.service.clone();

                    if current_time - info_old.last_success < REPLACEMENT {
                        // Has successfully connected in last X hours
                        erase_collision = true;
                    } else if current_time - info_old.last_try < REPLACEMENT {
                        // attempted to connect and failed in last X hours

                        // Give address at least 60 seconds to successfully connect
                        if current_time - info_old.last_try > 60 {
                            // Replaces an existing address already in the tried table with the
                            // new address
                            self.good_(&new_service, false, current_time);
                            erase_collision = true;
                        }
                    } else if current_time - info_new.last_success > TEST_WINDOW {
                        // If the collision hasn't resolved in some reasonable amount of time,
                        // just evict the old entry -- we must not be able to
                        // connect to it for some reason.
                        self.good_(&new_service, false, current_time);
                        erase_collision = true;
                    }
                } else {
                    // Collision is not actually a collision anymore
                    let new_service = info_new.service.clone();
                    self.good_(&new_service, false, now);
                    erase_collision = true;
                }
            }

            if erase_collision {
                self.tried_collisions.remove(&id_new);
            }
        }
    }

    // Core: addrman.cpp:986 AddrManImpl::SelectTriedCollision_
    pub(super) fn select_tried_collision_(&mut self) -> Option<(Address, i64)> {
        if self.tried_collisions.is_empty() {
            return None;
        }

        // Selects a random element from m_tried_collisions
        let index = self.rng.randrange(self.tried_collisions.len() as u64) as usize;
        let id_new = *self.tried_collisions.iter().nth(index).expect("index < size");

        // If id_new not found in mapInfo remove it from m_tried_collisions
        if !self.map_info.contains(id_new) {
            self.tried_collisions.remove(&id_new);
            return None;
        }

        let key = self.key;
        let new_info = self.info(id_new);

        // which tried bucket to move the entry to
        let tried_bucket = new_info.tried_bucket(&key, &self.netgroup);
        let tried_bucket_pos = new_info.bucket_position(&key, false, tried_bucket);

        // A tried slot, once filled, is only ever replaced, so the colliding slot is occupied.
        let info_old = self.info(self.tried[slot_of(tried_bucket, tried_bucket_pos)]);
        Some((info_old.address(), info_old.last_try))
    }

    // Core: addrman.cpp:1014 AddrManImpl::FindAddressEntry_
    pub(super) fn find_address_entry_(&self, addr: &Service) -> Option<AddressPosition> {
        let info = self.info(self.find(addr)?);
        let key = &self.key;
        Some(if info.in_tried {
            let bucket = info.tried_bucket(key, &self.netgroup);
            AddressPosition {
                tried: true,
                multiplicity: 1,
                bucket,
                position: info.bucket_position(key, false, bucket),
            }
        } else {
            let bucket = info.new_bucket(key, &self.netgroup);
            AddressPosition {
                tried: false,
                multiplicity: info.ref_count,
                bucket,
                position: info.bucket_position(key, true, bucket),
            }
        })
    }

    // Core: addrman.cpp:1037 AddrManImpl::Size_
    pub(super) fn size_(&self, net: Option<Network>, in_new: Option<bool>) -> usize {
        let Some(net) = net else {
            return match in_new {
                Some(true) => self.n_new as usize,
                Some(false) => self.n_tried as usize,
                None => self.random.len(),
            };
        };
        match (self.network_counts[net as usize], in_new) {
            (None, _) => 0,
            (Some(c), Some(true)) => c.n_new,
            (Some(c), Some(false)) => c.n_tried,
            (Some(c), None) => c.n_new.wrapping_add(c.n_tried),
        }
    }

    /// Core's `Check()`: with `-checkaddrman=<ratio>`, a `randrange(ratio)` draw on every call
    /// (none at ratio 1), and `CheckAddrman` one time in `ratio`.
    ///
    /// # Panics
    ///
    /// When the consistency check fails (Core asserts).
    // Core: addrman.cpp:1059 AddrManImpl::Check
    pub(super) fn check(&mut self) {
        // Run consistency checks 1 in m_consistency_check_ratio times if enabled
        if self.check_ratio == 0 {
            return;
        }
        if self.rng.randrange(u64::from(self.check_ratio)) >= 1 {
            return;
        }
        let err = self.check_addrman();
        if err != 0 {
            panic!("ADDRMAN CONSISTENCY CHECK FAILED!!! err={err}");
        }
    }

    // Core: addrman.cpp:1074 AddrManImpl::CheckAddrman
    pub(super) fn check_addrman(&self) -> i32 {
        let mut set_tried: std::collections::HashSet<i32> = Default::default();
        let mut map_new: HashMap<i32, i32> = HashMap::new();
        let mut local_counts: HashMap<Network, NewTriedCount> = HashMap::new();

        if self.random.len() != (self.n_tried as usize).wrapping_add(self.n_new as usize) {
            return -7;
        }

        for (n, info) in self.map_info.iter() {
            let net = self.network_of(&info.service.addr);
            if info.in_tried {
                if info.last_success == 0 {
                    return -1;
                }
                if info.ref_count != 0 {
                    return -2;
                }
                set_tried.insert(n);
                local_counts.entry(net).or_default().n_tried += 1;
            } else {
                if info.ref_count < 0 || info.ref_count > NEW_BUCKETS_PER_ADDRESS {
                    return -3;
                }
                if info.ref_count == 0 {
                    return -4;
                }
                map_new.insert(n, info.ref_count);
                local_counts.entry(net).or_default().n_new += 1;
            }
            if self.map_addr.get(&info.service) != Some(&n) {
                return -5;
            }
            if info.random_pos < 0
                || info.random_pos as usize >= self.random.len()
                || self.random[info.random_pos as usize] != n
            {
                return -14;
            }
            if info.last_try < 0 {
                return -6;
            }
            if info.last_success < 0 {
                return -8;
            }
        }

        if set_tried.len() != self.n_tried as usize {
            return -9;
        }
        if map_new.len() != self.n_new as usize {
            return -10;
        }

        for n in 0..TRIED_BUCKET_COUNT {
            for i in 0..BUCKET_SIZE {
                let id = self.tried[n * BUCKET_SIZE + i];
                if id != -1 {
                    if !set_tried.contains(&id) {
                        return -11;
                    }
                    let Some(info) = self.map_info.get(id) else {
                        return -17;
                    };
                    if info.tried_bucket(&self.key, &self.netgroup) != n as i32 {
                        return -17;
                    }
                    if info.bucket_position(&self.key, false, n as i32) != i as i32 {
                        return -18;
                    }
                    set_tried.remove(&id);
                }
            }
        }

        for n in 0..NEW_BUCKET_COUNT {
            for i in 0..BUCKET_SIZE {
                let id = self.new[n * BUCKET_SIZE + i];
                if id != -1 {
                    if !map_new.contains_key(&id) {
                        return -12;
                    }
                    match self.map_info.get(id) {
                        Some(info)
                            if info.bucket_position(&self.key, true, n as i32) == i as i32 => {}
                        _ => return -19,
                    }
                    let left = map_new.get_mut(&id).expect("checked above");
                    *left -= 1;
                    if *left == 0 {
                        map_new.remove(&id);
                    }
                }
            }
        }

        if !set_tried.is_empty() {
            return -13;
        }
        if !map_new.is_empty() {
            return -15;
        }
        if self.key == [0u8; 32] {
            return -16;
        }

        // It's possible that m_network_counts may have all-zero entries that local_counts
        // doesn't have if addrs from a network were being added and then removed again in the
        // past.
        let present = self.network_counts.iter().filter(|c| c.is_some()).count();
        if present < local_counts.len() {
            return -20;
        }
        for (i, count) in self.network_counts.iter().enumerate() {
            let Some(count) = count else {
                continue;
            };
            let local = local_counts
                .iter()
                .find(|(net, _)| **net as usize == i)
                .map(|(_, c)| *c)
                .unwrap_or_default();
            if local != *count {
                return -21;
            }
        }

        0
    }
}
