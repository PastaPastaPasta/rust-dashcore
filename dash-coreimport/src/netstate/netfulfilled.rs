//! `netfulfilled.dat`: the requests peers were served recently (`CNetFulfilledRequestManager`,
//! `netfulfilledman.cpp`), in the `CFlatDB` envelope with the magic message
//! `magicFulfilledCache`.
//!
//! The object is `std::map<CService, std::map<std::string, int64_t>>`: for each peer address, the
//! request names and the time each expires. The services are read in the 16-byte form (a
//! `CDataStream` without `ADDRV2_FORMAT`). A repeated key keeps its first entry (`std::map::insert`).

use std::collections::BTreeMap;

use dash_network::Network;

use super::error::FormatResult;
use super::flat::{self, Loaded, Payload};
use super::netaddr::Service;
use super::reader::Reader;

const WHAT: &str = "netfulfilled.dat";

/// Files Core writes hold a few hundred peers; this is far above that.
pub const MAX_NETFULFILLED_DAT_SIZE: usize = 16 << 20;

/// A parsed `netfulfilled.dat`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetFulfilledDat {
    /// Per service, the request name (bytes, compared as `std::string` does) to its expiry in
    /// seconds since the epoch.
    pub requests: BTreeMap<Service, BTreeMap<Vec<u8>, i64>>,
}

impl NetFulfilledDat {
    /// `CNetFulfilledRequestManager::CheckAndRemove`, which Core runs right after a successful
    /// load: drops every request that expired before `now` (`now > expiry`) and every service
    /// left with none.
    // Core: netfulfilledman.cpp CNetFulfilledRequestManager::CheckAndRemove
    pub fn without_expired(&self, now: i64) -> NetFulfilledDat {
        let requests = self
            .requests
            .iter()
            .filter_map(|(service, entries)| {
                let kept: BTreeMap<_, _> = entries
                    .iter()
                    .filter(|(_, expiry)| now <= **expiry)
                    .map(|(name, expiry)| (name.clone(), *expiry))
                    .collect();
                (!kept.is_empty()).then_some((*service, kept))
            })
            .collect();
        NetFulfilledDat {
            requests,
        }
    }
}

impl Payload for NetFulfilledDat {
    const FILE: &'static str = WHAT;
    const MAGIC_MESSAGE: &'static str = "magicFulfilledCache";
    const VERSION_STRINGS: &'static [&'static str] = &[];

    // Core: netfulfilledman.h NetFulfilledRequestStore::SERIALIZE_METHODS
    fn read(&mut self, r: &mut Reader<'_>) -> FormatResult<()> {
        let outer = flat::count(r)?;
        for _ in 0..outer {
            let service = Service::read_v1(r)?;
            let mut entries = BTreeMap::new();
            let inner = flat::count(r)?;
            for _ in 0..inner {
                let name = r.byte_vec()?.to_vec();
                let expiry = r.i64_le()?;
                entries.entry(name).or_insert(expiry);
            }
            self.requests.entry(service).or_insert(entries);
        }
        Ok(())
    }

    fn clear(&mut self) {
        self.requests.clear();
    }
}

/// Parses a `netfulfilled.dat` of `network`.
// Core: flat-database.h CFlatDB<NetFulfilledRequestStore>::CoreRead
pub fn parse(bytes: &[u8], network: Network) -> FormatResult<Loaded<NetFulfilledDat>> {
    flat::load(bytes, network, MAX_NETFULFILLED_DAT_SIZE)
}
