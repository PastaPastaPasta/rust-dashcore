//! Core's `CNetFulfilledRequestManager` (`netfulfilledman.{h,cpp}`): the requests each peer
//! address was served or asked recently, with an expiry, so that a node neither asks the same
//! peer twice nor is penalised for it (FR-P2P-15).
//!
//! The keys are the peer's `CService` (`pnode->addr`: address and port) and Core's request
//! names; the expiry is `GetTime() + FulfilledRequestExpireTime()` (1 h on mainnet, 5 min
//! elsewhere). Its file is `netfulfilled.dat` in rdashd's format ([`crate::flatdb`]):
//! `"rdashd-netfulfilled" ‖ message start ‖ "NetFulfilledRequestStore-1" ‖ Core's map ‖
//! SHA256d`. The map is Core's `READWRITE(mapFulfilledRequests)` byte for byte, with each
//! service in the 16-byte v1 form (`CFlatDB`'s stream has no `ADDRV2_FORMAT`), so a Tor, I2P or
//! CJDNS key comes back as `[::]` with its port after a restart, as in Core (SP1 D5); the same
//! body is what `dash-coreimport` reads in Core's file.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_clock::Clock;
use dash_netaddr::netaddr::{NetAddr, Service};
use parking_lot::Mutex;

use crate::flatdb::{
    self, FlatObject, Layout, Own, ReadError, push_compact_size, push_str, read_array,
    read_compact_size, read_i64, read_str,
};

/// The file's name in the network's data directory (Core's).
pub const FILE_NAME: &str = "netfulfilled.dat";

/// The format version (rdashd's; Core's object has none).
pub const VERSION: &str = "NetFulfilledRequestStore-1";

/// Core's request names (the literals of `node/sync_manager.cpp` and `governance/
/// net_governance.cpp`; `allow-sync-<ms>` is [`crate::sync_manager`]'s).
pub mod request {
    /// `SyncFinished` marks every peer; the tick disconnects a peer that has it.
    pub const FULL_SYNC: &str = "full-sync";
    /// The tick asked the peer for its mempool.
    pub const MEMPOOL_SYNC: &str = "mempool-sync";
    /// The tick sent the peer `govsync(0, empty bloom)`.
    pub const GOVERNANCE_SYNC: &str = "governance-sync";
}

/// `NetFulfilledRequestStore`: per service, each request name (`std::string`, compared as
/// bytes) and the time it expires.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetFulfilledStore {
    pub requests: BTreeMap<Service, BTreeMap<Vec<u8>, i64>>,
}

/// `CService`'s serialization without `ADDRV2_FORMAT`: the 16-byte form and a big-endian port.
// Core: netaddress.h CService::SERIALIZE_METHODS, CNetAddr::SerializeV1Array
fn write_service(out: &mut Vec<u8>, s: &Service) {
    out.extend_from_slice(&s.addr.v1_bytes());
    out.extend_from_slice(&s.port.to_be_bytes());
}

// Core: netaddress.h CNetAddr::UnserializeV1Array (SetLegacyIPv6)
fn read_service(data: &mut &[u8]) -> Result<Service, ReadError> {
    let addr = NetAddr::from_legacy_ipv6(read_array(data)?);
    Ok(Service::new(addr, u16::from_be_bytes(read_array(data)?)))
}

impl FlatObject for NetFulfilledStore {
    const FILE: &'static str = FILE_NAME;
    /// Unlike Core's `"magicFulfilledCache"`, so neither reads the other's file.
    const MAGIC: &'static str = "rdashd-netfulfilled";

    // Core: netfulfilledman.h NetFulfilledRequestStore::SERIALIZE_METHODS
    fn write(&self, out: &mut Vec<u8>) {
        push_str(out, VERSION.as_bytes());
        push_compact_size(out, self.requests.len());
        for (service, entries) in &self.requests {
            write_service(out, service);
            push_compact_size(out, entries.len());
            for (name, expiry) in entries {
                push_str(out, name);
                out.extend_from_slice(&expiry.to_le_bytes());
            }
        }
    }

    /// Core's map `Unserialize` clears the map, then inserts each item: of two equal keys the
    /// first stays (two v1-collapsed services, or a repeated name).
    // Core: serialize.h Unserialize(std::map)
    fn read(&mut self, data: &mut &[u8]) -> Result<(), ReadError> {
        if read_str(data)? != VERSION.as_bytes() {
            return Ok(());
        }
        self.requests.clear();
        for _ in 0..read_compact_size(data)? {
            let service = read_service(data)?;
            let mut entries = BTreeMap::new();
            for _ in 0..read_compact_size(data)? {
                let name = read_str(data)?.to_vec();
                let expiry = read_i64(data)?;
                entries.entry(name).or_insert(expiry);
            }
            self.requests.entry(service).or_insert(entries);
        }
        Ok(())
    }

    fn clear(&mut self) {
        self.requests.clear();
    }

    // Core: netfulfilledman.cpp:87-90 NetFulfilledRequestStore::ToString
    fn summary(&self) -> String {
        format!("Nodes with fulfilled requests: {}", self.requests.len())
    }
}

/// Core's `CNetFulfilledRequestManager`.
pub struct NetFulfilledRequestManager {
    clock: Arc<dyn Clock>,
    /// `Params().FulfilledRequestExpireTime()`.
    expire_time: i64,
    store: Mutex<NetFulfilledStore>,
    is_valid: AtomicBool,
}

impl NetFulfilledRequestManager {
    /// An empty manager; `expire_time` is the network's `nFulfilledRequestExpireTime`.
    pub fn new(clock: Arc<dyn Clock>, expire_time: i64) -> NetFulfilledRequestManager {
        NetFulfilledRequestManager {
            clock,
            expire_time,
            store: Mutex::default(),
            is_valid: AtomicBool::new(false),
        }
    }

    fn now(&self) -> i64 {
        self.clock.node_now().0
    }

    /// `IsValid()`: `LoadCache` succeeded, so the destructor stores the file.
    pub fn is_valid(&self) -> bool {
        self.is_valid.load(Ordering::SeqCst)
    }

    /// `LoadCache(load_cache)`: loads `path` (then `CheckAndRemove`), or, when the node starts
    /// without its caches (`-reindex`, or no tip yet), stores the manager over it. Returns
    /// `IsValid()`; false is Core's `"Failed to load fulfilled requests cache from %s"` (or
    /// `"Failed to clear …"`). An error is a failed write, which Core also reports as false.
    // Core: netfulfilledman.cpp:21-29 LoadCache; init.cpp:2275-2281
    pub fn load_cache(&self, path: &Path, message_start: [u8; 4], load: bool) -> bool {
        self.load_cache_as::<Own>(path, message_start, load)
    }

    fn load_cache_as<L: Layout<NetFulfilledStore>>(
        &self,
        path: &Path,
        message_start: [u8; 4],
        load: bool,
    ) -> bool {
        let valid = if load {
            let mut store = std::mem::take(&mut *self.store.lock());
            let loaded = flatdb::load_as::<_, L>(path, message_start, &mut store);
            *self.store.lock() = store;
            loaded.is_ok()
        } else {
            self.store_as::<L>(path, message_start)
        };
        self.is_valid.store(valid, Ordering::SeqCst);
        if valid && load {
            self.check_and_remove();
        }
        valid
    }

    /// `m_db->Store(*this)` (the destructor's, when [`Self::is_valid`]): false when the file on
    /// disk is one Core would refuse, or the write failed (logged as `CoreWrite`'s error).
    // Core: netfulfilledman.cpp:15-19; flat-database.h:192-205 CFlatDB::Store
    pub fn store(&self, path: &Path, message_start: [u8; 4]) -> bool {
        self.store_as::<Own>(path, message_start)
    }

    fn store_as<L: Layout<NetFulfilledStore>>(&self, path: &Path, message_start: [u8; 4]) -> bool {
        let snapshot = self.store.lock().clone();
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

    /// `AddFulfilledRequest(addr, strRequest)`: expires `FulfilledRequestExpireTime()` from now.
    // Core: netfulfilledman.cpp:31-35
    pub fn add_fulfilled_request(&self, addr: &Service, request: &str) {
        let expiry = self.now() + self.expire_time;
        self.store
            .lock()
            .requests
            .entry(addr.clone())
            .or_default()
            .insert(request.as_bytes().to_vec(), expiry);
    }

    /// `HasFulfilledRequest(addr, strRequest)`: recorded and not yet expired (`expiry > now`).
    // Core: netfulfilledman.cpp:37-45
    pub fn has_fulfilled_request(&self, addr: &Service, request: &str) -> bool {
        let now = self.now();
        self.store
            .lock()
            .requests
            .get(addr)
            .and_then(|e| e.get(request.as_bytes()))
            .is_some_and(|expiry| *expiry > now)
    }

    /// `RemoveAllFulfilledRequests(addr)`.
    // Core: netfulfilledman.cpp:47-55
    pub fn remove_all_fulfilled_requests(&self, addr: &Service) {
        self.store.lock().requests.remove(addr);
    }

    /// `CheckAndRemove()`: drops every request that expired before now (`now > expiry`; one
    /// expiring now stays, though [`Self::has_fulfilled_request`] no longer sees it) and every
    /// service left with none.
    // Core: netfulfilledman.cpp:57-79
    pub fn check_and_remove(&self) {
        let now = self.now();
        self.store.lock().requests.retain(|_, entries| {
            entries.retain(|_, expiry| now <= *expiry);
            !entries.is_empty()
        });
    }

    /// `DoMaintenance()`, every minute from init step 10a: [`Self::check_and_remove`] unless a
    /// shutdown was requested.
    // Core: netfulfilledman.cpp:92-97; init.cpp:2394
    pub fn do_maintenance(&self, shutdown_requested: bool) {
        if shutdown_requested {
            return;
        }
        self.check_and_remove();
    }

    /// The store, for tests and diagnostics.
    pub fn snapshot(&self) -> NetFulfilledStore {
        self.store.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use dash_clock::MockClock;

    use super::*;

    const T0: i64 = 1_700_000_000;

    fn addr(n: u8) -> Service {
        Service::new(NetAddr::ipv4([10, 0, 0, n]), 9999)
    }

    fn manager(expire: i64) -> (Arc<MockClock>, NetFulfilledRequestManager) {
        let clock = Arc::new(MockClock::at(T0));
        (clock.clone(), NetFulfilledRequestManager::new(clock, expire))
    }

    /// `Has` is `expiry > now`; `CheckAndRemove` drops only `now > expiry`.
    // Core: netfulfilledman.cpp:31-79
    #[test]
    fn requests_expire_after_the_network_s_time() {
        let (clock, m) = manager(300);
        m.add_fulfilled_request(&addr(1), request::FULL_SYNC);
        m.add_fulfilled_request(&addr(2), request::MEMPOOL_SYNC);
        clock.set_mocktime(T0 + 299);
        assert!(m.has_fulfilled_request(&addr(1), request::FULL_SYNC));
        assert!(!m.has_fulfilled_request(&addr(1), request::MEMPOOL_SYNC));
        assert!(!m.has_fulfilled_request(&addr(3), request::FULL_SYNC));
        clock.set_mocktime(T0 + 300);
        assert!(!m.has_fulfilled_request(&addr(1), request::FULL_SYNC), "expiry == now");
        m.check_and_remove();
        assert_eq!(m.snapshot().requests.len(), 2, "kept while expiry == now");
        // Re-adding moves the expiry.
        m.add_fulfilled_request(&addr(2), request::MEMPOOL_SYNC);
        clock.set_mocktime(T0 + 301);
        m.do_maintenance(true);
        assert_eq!(m.snapshot().requests.len(), 2, "not during shutdown");
        m.do_maintenance(false);
        assert_eq!(m.snapshot().requests.keys().collect::<Vec<_>>(), [&addr(2)]);
        m.remove_all_fulfilled_requests(&addr(2));
        assert!(m.snapshot().requests.is_empty());
    }

    #[test]
    fn mainnet_requests_last_an_hour() {
        let (clock, m) = manager(3600);
        m.add_fulfilled_request(&addr(1), request::GOVERNANCE_SYNC);
        clock.set_mocktime(T0 + 3599);
        assert!(m.has_fulfilled_request(&addr(1), request::GOVERNANCE_SYNC));
        clock.set_mocktime(T0 + 3600);
        assert!(!m.has_fulfilled_request(&addr(1), request::GOVERNANCE_SYNC));
    }

    /// `LoadCache(false)` is `Store(*this)` with no `CheckAndRemove`: an expired request a manager
    /// holds stays, in memory and in the file.
    // Core: netfulfilledman.cpp:21-29
    #[test]
    fn a_clear_stores_the_manager_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let ms = [1, 2, 3, 4];
        let (clock, m) = manager(300);
        m.add_fulfilled_request(&addr(1), request::FULL_SYNC);
        clock.set_mocktime(T0 + 301);
        assert!(m.load_cache(&path, ms, false));
        assert_eq!(m.snapshot().requests.len(), 1, "nothing dropped in memory");
        let file = flatdb::decode::<NetFulfilledStore>(&std::fs::read(&path).unwrap(), ms).unwrap();
        assert_eq!(file, m.snapshot(), "the store as it is");
    }

    /// `LoadCache(true)` loads then drops what expired; `LoadCache(false)` stores the empty
    /// manager; a file Core refuses fails the load and is left as it is.
    // Core: netfulfilledman.cpp:15-29; flat-database.h:158-205
    #[test]
    fn load_cache_outcomes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let ms = [1, 2, 3, 4];
        let (clock, m) = manager(300);
        assert!(!m.is_valid());
        assert!(m.load_cache(&path, ms, true), "missing: start empty");
        assert!(m.is_valid());
        m.add_fulfilled_request(&addr(1), request::FULL_SYNC);
        clock.set_mocktime(T0 + 200);
        m.add_fulfilled_request(&addr(2), request::FULL_SYNC);
        assert!(m.store(&path, ms));

        clock.set_mocktime(T0 + 301);
        let back = NetFulfilledRequestManager::new(clock.clone(), 300);
        assert!(back.load_cache(&path, ms, true));
        assert_eq!(
            back.snapshot().requests.keys().collect::<Vec<_>>(),
            [&addr(2)],
            "expired dropped"
        );

        let other_net = NetFulfilledRequestManager::new(clock.clone(), 300);
        assert!(!other_net.load_cache(&path, [9, 9, 9, 9], true));
        assert!(!other_net.is_valid());
        assert!(!other_net.store(&path, [9, 9, 9, 9]), "Store verifies first");
        assert_eq!(
            flatdb::decode::<NetFulfilledStore>(&std::fs::read(&path).unwrap(), ms)
                .unwrap()
                .requests
                .len(),
            2
        );

        let fresh = NetFulfilledRequestManager::new(clock, 300);
        assert!(fresh.load_cache(&path, ms, false));
        assert!(
            flatdb::decode::<NetFulfilledStore>(&std::fs::read(&path).unwrap(), ms)
                .unwrap()
                .requests
                .is_empty()
        );

        std::fs::write(&path, b"corrupt").unwrap();
        let (_, bad) = manager(300);
        assert!(!bad.load_cache(&path, ms, true));
        assert!(!bad.load_cache(&path, ms, false), "a file Core refuses is not cleared");
        assert_eq!(std::fs::read(&path).unwrap(), b"corrupt");
    }

    /// Keys are v1 services: an onion address comes back as `[::]` with its port, and two such
    /// keys keep the first one's requests.
    // Core: netaddress.h CNetAddr::SerializeV1Array; serialize.h Unserialize(std::map)
    #[test]
    fn non_ip_keys_collapse_in_the_file() {
        let ms = [1, 2, 3, 4];
        let onion = |b: u8| {
            Service::new(
                NetAddr::new(dash_netaddr::netaddr::Network::Onion, vec![b; 32]).unwrap(),
                9999,
            )
        };
        let mut store = NetFulfilledStore::default();
        store.requests.entry(onion(1)).or_default().insert(b"a".to_vec(), 5);
        store.requests.entry(onion(2)).or_default().insert(b"b".to_vec(), 6);
        let back: NetFulfilledStore = flatdb::decode(&flatdb::encode(&store, ms), ms).unwrap();
        let unspecified = Service::new(NetAddr::from_legacy_ipv6([0; 16]), 9999);
        assert_eq!(back.requests.keys().collect::<Vec<_>>(), [&unspecified]);
        assert_eq!(back.requests[&unspecified].len(), 1, "the first service's map");
    }
}
