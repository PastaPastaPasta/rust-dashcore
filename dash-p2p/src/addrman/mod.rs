//! A line-by-line port of Core's `AddrMan` (P2a §3.7; v24.0.0-rc.3 `addrman.cpp`,
//! `addrman_impl.h`, `addrdb.cpp`).
//!
//! The only structural change: operations take `now` from the caller and draw from the
//! instance's own [`FastRandom`](crate::rng::FastRandom), so the port is fuzzable and comparable
//! with the oracle. One mutex per instance (Core's `cs`); every public call is one critical
//! section with `Check()` on both sides, as in Core's `AddrManImpl` wrappers.
//!
//! Parity notes:
//! - `mapInfo` iterates in libstdc++'s `std::unordered_map<int, …>` order ([`stdmap`]), so
//!   [`AddrMan::serialize`] is byte-identical to a libstdc++ dashd in the same state, and the
//!   `vRandom` order after a pruning load matches it too.
//! - `GetChance` uses the exact glibc values of `pow(0.66, 0..=8)`; a negative `nAttempts` (only
//!   from a crafted file) goes to the platform `pow`.

#[cfg(test)]
mod crafted_tests;
mod imp;
mod ser;
mod stdmap;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use parking_lot::Mutex;

pub(crate) use self::ser::{read_anchors, write_anchors};

use self::imp::{AddrInfo, Impl};
use crate::netaddr::{Address, NetAddr, Network, Service};
use crate::netgroup::NetGroupManager;
use crate::rng::{FastRandom, OsRand, RandSource};

/// Core's `AddrMan`.
pub struct AddrMan {
    inner: Mutex<Impl>,
    netgroup: Arc<NetGroupManager>,
    deterministic: bool,
    check_ratio: u32,
    allow_private_net: bool,
    magic: [u8; 4],
    /// Seeds the context of every table built here (Core's `GetRandBytes`).
    rand: Arc<dyn RandSource>,
}

const _: () = {
    const fn send_sync<T: Send + Sync>() {}
    send_sync::<AddrMan>();
};

/// What [`AddrMan::select`] returns: the address and its last try (Core's
/// `std::pair<CAddress, NodeSeconds>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selected {
    /// The address (`nTime` truncated to 32 bits, as Core serializes it).
    pub addr: Address,
    /// `m_last_try`.
    pub last_try: i64,
}

/// Core's `AddressPosition`: where an address sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressPosition {
    /// In the tried table.
    pub tried: bool,
    /// 1 in tried; `nRefCount` in new.
    pub multiplicity: i32,
    /// The bucket (for new entries: the one of the entry's first source).
    pub bucket: i32,
    /// The position in the bucket.
    pub position: i32,
}

/// Every field of a Core `AddrInfo` (`GetEntries`, `GetAddressInfo`, [`AddrMan::canonical`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddrEntry {
    /// The address and port.
    pub service: Service,
    /// `nServices`.
    pub services: dash_p2p_core::ServiceFlags,
    /// `nTime` (full width).
    pub time: i64,
    /// `source`.
    pub source: NetAddr,
    /// `m_last_try`.
    pub last_try: i64,
    /// `m_last_count_attempt`.
    pub last_count_attempt: i64,
    /// `m_last_success`.
    pub last_success: i64,
    /// `nAttempts`.
    pub attempts: i32,
    /// `nRefCount`.
    pub ref_count: i32,
    /// `fInTried`.
    pub in_tried: bool,
    /// `nRandomPos`.
    pub random_pos: i32,
}

/// The whole internal state, for the `diff_addrman` differential and tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Canonical {
    /// `nKey`.
    pub key: [u8; 32],
    /// `nIdCount`.
    pub id_count: i32,
    /// `nNew`.
    pub n_new: i32,
    /// `nTried`.
    pub n_tried: i32,
    /// `mapInfo` as `(id, entry)`, in libstdc++ iteration order.
    pub entries: Vec<(i32, AddrEntry)>,
    /// `vRandom`.
    pub random: Vec<i32>,
    /// `vvNew`, row-major (1024 × 64), −1 for empty.
    pub new_table: Vec<i32>,
    /// `vvTried`, row-major (256 × 64), −1 for empty.
    pub tried_table: Vec<i32>,
    /// `m_tried_collisions`, ascending.
    pub tried_collisions: Vec<i32>,
    /// `m_network_counts` entries that exist: `(network, n_new, n_tried)`.
    pub network_counts: Vec<(Network, usize, usize)>,
    /// `m_last_good`.
    pub last_good: i64,
    /// Keystream bytes `insecure_rand` has produced.
    pub rng_keystream_bytes: u64,
    /// Bits left in `insecure_rand`'s bit buffer.
    pub rng_bitbuf_size: u32,
    /// `insecure_rand`'s bit buffer.
    pub rng_bitbuf: u64,
}

/// `AddrInfo(addr, source)`'s buckets under a key: `GetTriedBucket`, `GetNewBucket(key,
/// source)`, and `GetBucketPosition` in each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Buckets {
    /// `GetTriedBucket`.
    pub tried_bucket: i32,
    /// `GetNewBucket`.
    pub new_bucket: i32,
    /// `GetBucketPosition(key, true, new_bucket)`.
    pub pos_new: i32,
    /// `GetBucketPosition(key, false, tried_bucket)`.
    pub pos_tried: i32,
}

/// The bucket maths for `addr` from `source` under `key` (what the differential checks against
/// Core's `oracle_addrman_bucket`).
// Core: addrman.cpp:48-69 AddrInfo::GetTriedBucket, GetNewBucket, GetBucketPosition
pub fn buckets(
    key: &[u8; 32],
    addr: &Service,
    source: &NetAddr,
    netgroup: &NetGroupManager,
) -> Buckets {
    let info = AddrInfo {
        service: addr.clone(),
        source: source.clone(),
        ..AddrInfo::default()
    };
    let tried_bucket = info.tried_bucket(key, netgroup);
    let new_bucket = info.new_bucket(key, netgroup);
    Buckets {
        tried_bucket,
        new_bucket,
        pos_new: info.bucket_position(key, true, new_bucket),
        pos_tried: info.bucket_position(key, false, tried_bucket),
    }
}

/// `AddrInfo::GetChance(now)` for an entry last tried `since_last_try` seconds ago after
/// `attempts` failed attempts.
// Core: addrman.cpp:96 AddrInfo::GetChance
pub fn chance(since_last_try: i64, attempts: i32) -> f64 {
    let info = AddrInfo {
        attempts,
        ..AddrInfo::default()
    };
    info.chance(since_last_try)
}

/// Why a `peers.dat` image was refused (Core's `DbInconsistentError` and stream errors).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// `InvalidAddrManVersionError`: the file is from a newer, incompatible format.
    Incompatible {
        /// The file's format byte.
        format: u8,
        /// The lowest format able to read it (`compat - INCOMPATIBILITY_BASE`).
        lowest_compatible: u8,
    },
    /// The bytes are truncated or malformed, with Core's `what()` text (bad magic, checksum,
    /// truncation, out-of-range counts, a bad `CAddress` version word, a bad BIP155 length, …).
    Corrupt(String),
    /// `CheckAddrman` failed after the load, with its code.
    Inconsistent(i32),
}

impl LoadError {
    /// Core's `InvalidAddrManVersionError`: the caller backs the file up and starts empty.
    pub fn is_unsupported_version(&self) -> bool {
        matches!(self, Self::Incompatible { .. })
    }
}

impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            // Core: addrman.cpp:262 AddrManImpl::Unserialize (PACKAGE_NAME, FILE_FORMAT = 4).
            // `InvalidAddrManVersionError` is an `ios_base::failure`: libstdc++ appends its
            // category's text.
            Self::Incompatible {
                format,
                lowest_compatible,
            } => write!(
                f,
                "Unsupported format of addrman database: {format}. It is compatible with formats \
                 >={lowest_compatible}, but the maximum supported by this version of Dash Core is \
                 4.: iostream error"
            ),
            Self::Corrupt(e) => f.write_str(e),
            Self::Inconsistent(code) => {
                write!(f, "Corrupt data. Consistency check failed with code {code}")
            }
        }
    }
}

impl std::error::Error for LoadError {}

impl AddrMan {
    /// An empty AddrMan bucketing by `netgroup`. `deterministic` is Core's test mode
    /// (`nKey = uint256{1}`, the zero-key RNG); `check_ratio` is `-checkaddrman`;
    /// `allow_private_net` is `-allowprivatenet` (for `IsRoutable`, `GetNetwork` and
    /// `GetNetClass`); `magic` is the network's message start, which frames `peers.dat`.
    // Core: addrman.cpp:1295 AddrMan::AddrMan
    pub fn new(
        netgroup: NetGroupManager,
        deterministic: bool,
        check_ratio: u32,
        allow_private_net: bool,
        magic: [u8; 4],
    ) -> Self {
        Self::with_rand_source(
            netgroup,
            deterministic,
            check_ratio,
            allow_private_net,
            magic,
            Arc::new(OsRand),
        )
    }

    /// [`AddrMan::new`] with its context seeded from `rand` (Core's `GetRandBytes`), unless
    /// `deterministic`; each [`AddrMan::clear`] seeds a new one from it.
    pub fn with_rand_source(
        netgroup: NetGroupManager,
        deterministic: bool,
        check_ratio: u32,
        allow_private_net: bool,
        magic: [u8; 4],
        rand: Arc<dyn RandSource>,
    ) -> Self {
        let netgroup = Arc::new(netgroup);
        Self {
            inner: Mutex::new(Impl::new(
                Arc::clone(&netgroup),
                deterministic,
                check_ratio,
                allow_private_net,
                &rand,
            )),
            netgroup,
            deterministic,
            check_ratio,
            allow_private_net,
            magic,
            rand,
        }
    }

    fn fresh(&self) -> Impl {
        Impl::new(
            Arc::clone(&self.netgroup),
            self.deterministic,
            self.check_ratio,
            self.allow_private_net,
            &self.rand,
        )
    }

    /// Runs `op` as one critical section with `Check()` before and after.
    fn locked<R>(&self, op: impl FnOnce(&mut Impl) -> R) -> R {
        let mut inner = self.inner.lock();
        inner.check();
        let ret = op(&mut inner);
        inner.check();
        ret
    }

    /// Empties the table: Core constructs a new `AddrMan` (a new key and RNG) after backing up
    /// an unsupported `peers.dat` or dropping an inconsistent one.
    pub fn clear(&self) {
        let fresh = self.fresh();
        *self.inner.lock() = fresh;
    }

    /// Core's `Add`: returns whether at least one address was new.
    // Core: addrman.cpp:1188 AddrManImpl::Add
    pub fn add(&self, addrs: &[Address], source: &NetAddr, time_penalty: i64, now: i64) -> bool {
        self.locked(|m| m.add_(addrs, source, time_penalty, now))
    }

    /// Core's `Good`.
    // Core: addrman.cpp:1197 AddrManImpl::Good
    pub fn good(&self, addr: &Service, now: i64) -> bool {
        self.locked(|m| m.good_(addr, true, now))
    }

    /// Core's `Attempt`.
    // Core: addrman.cpp:1206 AddrManImpl::Attempt
    pub fn attempt(&self, addr: &Service, count_failure: bool, now: i64) {
        self.locked(|m| m.attempt_(addr, count_failure, now))
    }

    /// Core's `Connected`.
    // Core: addrman.cpp:1258 AddrManImpl::Connected
    pub fn connected(&self, addr: &Service, now: i64) {
        self.locked(|m| m.connected_(addr, now))
    }

    /// Core's `SetServices`.
    // Core: addrman.cpp:1266 AddrManImpl::SetServices
    pub fn set_services(&self, addr: &Service, services: dash_p2p_core::ServiceFlags) {
        self.locked(|m| m.set_services_(addr, services))
    }

    /// Core's `Select(new_only, network)`; `None` where Core returns an empty pair.
    // Core: addrman.cpp:1231 AddrManImpl::Select
    pub fn select(&self, new_only: bool, network: Option<Network>, now: i64) -> Option<Selected> {
        self.locked(|m| m.select_(new_only, network, now)).map(|(addr, last_try)| Selected {
            addr,
            last_try,
        })
    }

    /// Core's `GetAddr(max_addresses, max_pct, network, filtered)`.
    // Core: addrman.cpp:1240 AddrManImpl::GetAddr
    pub fn get_addr(
        &self,
        max_addresses: usize,
        max_pct: usize,
        network: Option<Network>,
        filtered: bool,
        now: i64,
    ) -> Vec<Address> {
        self.locked(|m| m.get_addr_(max_addresses, max_pct, network, filtered, now))
    }

    /// Core's `GetEntries(from_tried)` (v24 `getrawaddrman`).
    // Core: addrman.cpp:1249 AddrManImpl::GetEntries
    pub fn get_entries(&self, from_tried: bool) -> Vec<(AddrEntry, AddressPosition)> {
        self.locked(|m| m.get_entries_(from_tried))
    }

    /// Core's `ResolveCollisions`.
    // Core: addrman.cpp:1214 AddrManImpl::ResolveCollisions
    pub fn resolve_collisions(&self, now: i64) {
        self.locked(|m| m.resolve_collisions_(now))
    }

    /// Core's `SelectTriedCollision`.
    // Core: addrman.cpp:1222 AddrManImpl::SelectTriedCollision
    pub fn select_tried_collision(&self) -> Option<Selected> {
        self.locked(|m| m.select_tried_collision_()).map(|(addr, last_try)| Selected {
            addr,
            last_try,
        })
    }

    /// Core's `FindAddressEntry`.
    // Core: addrman.cpp:1274 AddrManImpl::FindAddressEntry
    pub fn find_address_entry(&self, addr: &Service) -> Option<AddressPosition> {
        self.locked(|m| m.find_address_entry_(addr))
    }

    /// Core's `GetAddressInfo`; `None` where Core returns a default `AddrInfo`.
    // Core: addrman.cpp:1283 AddrManImpl::GetAddressInfo
    pub fn get_address_info(&self, addr: &Service) -> Option<AddrEntry> {
        self.locked(|m| m.get_address_info_(addr))
    }

    /// Core's `Size(network, in_new)`.
    // Core: addrman.cpp:1179 AddrManImpl::Size
    pub fn size(&self, network: Option<Network>, in_new: Option<bool>) -> usize {
        self.locked(|m| m.size_(network, in_new))
    }

    /// `peers.dat` in Core's format (`V4_MULTIPORT`, with the asmap checksum): the network
    /// magic, the payload and its SHA-256d, as `DumpPeerAddresses` writes it.
    // Core: addrman.cpp:134 AddrManImpl::Serialize, addrdb.cpp:38 SerializeDB
    pub fn serialize(&self) -> Vec<u8> {
        ser::write_file(&self.magic, &self.inner.lock())
    }

    /// Replaces the contents with a `peers.dat` image read into a newly constructed `AddrMan`:
    /// a new RNG context (seeded from the source, as [`AddrMan::clear`] seeds it) and the key from
    /// the file. On error the current contents are kept. Startup reads into the AddrMan it has
    /// just built instead, keeping its context: [`AddrMan::unserialize`].
    // Core: addrman.cpp:233 AddrManImpl::Unserialize, addrdb.cpp:96 DeserializeDB
    pub fn deserialize(&self, bytes: &[u8]) -> Result<(), LoadError> {
        let mut fresh = self.fresh();
        ser::read_file(&self.magic, bytes, &mut fresh)?;
        *self.inner.lock() = fresh;
        Ok(())
    }

    /// `DeserializeFileDB` into this AddrMan, as `LoadAddrman` reads `peers.dat` into the one it
    /// has just constructed: the table and key come from the image and the RNG context stays
    /// (Core's `Unserialize` reads `nKey` and seeds nothing), so the read draws no source bytes.
    /// On error nothing changes; Core's caller then builds a replacement ([`AddrMan::clear`],
    /// which seeds a new context) or stops.
    ///
    /// # Panics
    ///
    /// If the table is not empty (Core asserts `vRandom.empty()`).
    // Core: addrdb.cpp:197-202 LoadAddrman; addrman.cpp:233-306 AddrManImpl::Unserialize
    pub fn unserialize(&self, bytes: &[u8]) -> Result<(), LoadError> {
        let mut inner = self.inner.lock();
        assert!(inner.random.is_empty(), "unserialize into a non-empty AddrMan");
        // An empty table whose placeholder context and key the read and the swap below replace,
        // built without the constructor's draws.
        let mut table = Impl::with_context(
            FastRandom::deterministic(),
            [0; 32],
            Arc::clone(&self.netgroup),
            self.check_ratio,
            self.allow_private_net,
        );
        ser::read_file(&self.magic, bytes, &mut table)?;
        std::mem::swap(&mut table.rng, &mut inner.rng);
        *inner = table;
        Ok(())
    }

    /// Core's `CheckAddrman()` without the `Check()` dice roll: 0 or the error code (−1…−21).
    pub fn check(&self) -> i32 {
        self.inner.lock().check_addrman()
    }

    /// The whole internal state (no RNG draw).
    pub fn canonical(&self) -> Canonical {
        let m = self.inner.lock();
        Canonical {
            key: m.key,
            id_count: m.id_count,
            n_new: m.n_new,
            n_tried: m.n_tried,
            entries: m.map_info.iter().map(|(id, info)| (id, info.entry())).collect(),
            random: m.random.clone(),
            new_table: m.new.to_vec(),
            tried_table: m.tried.to_vec(),
            tried_collisions: m.tried_collisions.iter().copied().collect(),
            network_counts: m
                .network_counts
                .iter()
                .enumerate()
                .filter_map(|(i, c)| c.map(|c| (NETWORKS[i], c.n_new, c.n_tried)))
                .collect(),
            last_good: m.last_good,
            rng_keystream_bytes: m.rng.keystream_bytes(),
            rng_bitbuf_size: m.rng.bitbuf().0,
            rng_bitbuf: m.rng.bitbuf().1,
        }
    }
}

/// `Network` by index.
const NETWORKS: [Network; Network::COUNT] = [
    Network::Unroutable,
    Network::Ipv4,
    Network::Ipv6,
    Network::Onion,
    Network::I2p,
    Network::Cjdns,
    Network::Internal,
];
