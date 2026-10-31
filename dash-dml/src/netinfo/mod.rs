//! Core's masternode network info rules (EV1 §3.16, `evo/netinfo.cpp`): `Validate()` for both
//! kinds ([`validate`], which the codec takes as [`CoreNetInfoValidity`]), `AddEntry`
//! ([`add_entry`]) and the read-only reject of `SetStateVersion`'s NetInfo step
//! ([`migration_reject`]).
//!
//! Core reads the chain (`Params()`, `MainParams()`) and one node global (`g_reachable_nets`)
//! here; [`NetInfoRules`] carries them as plain data (EV1-1 D33, D34). Since rc.4 the
//! masternode rules no longer read `fAllowPrivateNet` (CE4-02).
//!
//! The values are base-sdk's: a decoded `ExtNetInfo` holds its purpose groups in Core's map order
//! (sorted, each purpose once), which is the order these functions walk (D41). rc.4 reads only
//! that order ([`purpose_follows`], CE4-01), and so does base-sdk's decoder.
//! An entry is read as Core's `NetInfoEntry` would read its wire form: a service whose address is
//! not valid, or a domain that fails `DomainPort::Validate`, is the invalid entry.

mod add;
mod entry;
mod migration;

use std::collections::BTreeSet;

use dash_netaddr::Service;
use dash_params::{ChainSetup, Network};
use dash_primitives::{NIEntry, NIPurpose, NetInfo, NetInfoV1, NetInfoV2};
use dash_types::Numeric;

use crate::codec::NetInfoValidity;

pub use add::{add_entry, add_entry_bytes};
pub use entry::{AddrStr, DOMAIN_MAX_LEN, NETINFO_ADDR_MAX};
pub(crate) use migration::migrate;
pub use migration::migration_reject;

use entry::{Entry, validate_domain_port, validate_service_ext, validate_service_mn};

/// Core's `MAX_ENTRIES_EXTNETINFO`: the most entries one purpose holds.
// Core: evo/netinfo.h:20
pub const MAX_ENTRIES_EXTNETINFO: usize = 4;

/// Core's `ExtNetInfo::CURRENT_VERSION`.
// Core: evo/netinfo.h:378
const EXT_CURRENT_VERSION: u8 = 1;

/// Core's `NetInfoStatus`, in Core's order.
// Core: evo/netinfo.h:27-41
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum NetInfoStatus {
    /// Managing entries.
    BadInput,
    Duplicate,
    MaxLimit,
    /// Validation.
    BadAddress,
    BadPort,
    BadType,
    NotRoutable,
    Malformed,
    Success,
}

/// The node setting Core's NetInfo rules read from a global: whether CJDNS is reachable
/// (`g_reachable_nets.Contains(NET_CJDNS)`, read by `MaybeFlipIPv6toCJDNS` in `AddEntry`).
/// `-allowprivatenet` is not one since rc.4 (CE4-02, [`NetInfoRules`]'s routability).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetInfoConfig {
    /// Whether CJDNS is a reachable network (Core: `-cjdnsreachable`, default false; init.cpp
    /// removes `NET_CJDNS` from `g_reachable_nets` unless the option is given, with any value).
    pub cjdns_reachable: bool,
}

/// Everything Core's NetInfo rules read outside the value: `Params()`'s network id, default port
/// and `RequireRoutableExternalIP()`, `MainParams()`'s three ports, and the [`NetInfoConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetInfoRules {
    /// `IsNodeOnMainnet()`: `Params().NetworkIDString() == "main"`.
    pub(crate) on_mainnet: bool,
    /// `Params().GetDefaultPort()` (`MnNetInfo::AddEntry`).
    pub(crate) default_port: u16,
    /// `Params().RequireRoutableExternalIP()`.
    pub(crate) require_routable: bool,
    /// `MainParams().GetDefaultPort()`.
    pub(crate) main_default_port: u16,
    /// `MainParams().GetDefaultPlatformP2PPort()`.
    pub(crate) main_platform_p2p_port: u16,
    /// `MainParams().GetDefaultPlatformHTTPPort()`.
    pub(crate) main_platform_http_port: u16,
    pub(crate) config: NetInfoConfig,
}

impl NetInfoRules {
    /// The rules of the chain `params`, with `main` as Core's `MainParams()`.
    ///
    /// # Panics
    ///
    /// If `main` is not a mainnet parameter set.
    pub fn new(params: &ChainSetup, main: &ChainSetup, config: NetInfoConfig) -> Self {
        assert_eq!(main.network, Network::Main, "MainParams() is mainnet's");
        Self {
            on_mainnet: params.network == Network::Main,
            default_port: params.default_port,
            require_routable: params.require_routable_external_ip,
            main_default_port: main.default_port,
            main_platform_p2p_port: main.default_platform_p2p_port,
            main_platform_http_port: main.default_platform_http_port,
            config,
        }
    }

    /// Core's `IsNodeOnMainnet()`.
    pub fn on_mainnet(&self) -> bool {
        self.on_mainnet
    }

    /// `MainParams()`'s default, Platform P2P and Platform HTTP ports.
    pub fn main_ports(&self) -> (u16, u16, u16) {
        (self.main_default_port, self.main_platform_p2p_port, self.main_platform_http_port)
    }

    /// `service.IsRFC1918() || !service.IsRoutable()` negated: an RFC1918 address is never
    /// routable here, whatever `-allowprivatenet` says (CE4-02; rc.3 called `IsRoutable()`
    /// alone, which `fAllowPrivateNet` relaxes).
    // Core (rc.4): evo/netinfo.cpp:294, 456
    fn is_routable(&self, s: &Service) -> bool {
        s.addr.is_routable(false)
    }
}

/// Core's `netInfo->Validate() == NetInfoStatus::Success`, the codec's [`NetInfoValidity`] in
/// node builds (EV2 §1.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreNetInfoValidity {
    pub rules: NetInfoRules,
}

impl NetInfoValidity for CoreNetInfoValidity {
    fn is_valid(&self, net_info: &NetInfo) -> bool {
        validate(net_info, &self.rules) == NetInfoStatus::Success
    }
}

/// Core's `netInfo->Validate()`.
///
/// `net_info`'s purpose groups must be sorted by purpose byte and unique, as a decoded value's are
/// (D41).
pub fn validate(net_info: &NetInfo, rules: &NetInfoRules) -> NetInfoStatus {
    match net_info {
        NetInfo::Legacy(v1) => validate_mn(v1, rules),
        NetInfo::Extended(v2) => validate_ext(v2, rules),
    }
}

/// Whether `purpose` may follow `last` in an `ExtNetInfo` read: a known purpose above the one
/// before it, so a repeat fails too. Otherwise rc.4's `ExtNetInfo::Unserialize` throws, which
/// fails the ProTx payload (`bad-protx-payload`), the evodb record or the SML entry that holds
/// it (CE4-01). rc.3 read any order, kept a repeated purpose's first group and left an unknown
/// purpose to `Validate()`.
// Core (rc.4): evo/netinfo.h:419-437
pub fn purpose_follows(purpose: u8, last: Option<u8>) -> bool {
    !matches!(NIPurpose::from_base(purpose), NIPurpose::Unknown(_))
        && last.is_none_or(|last| purpose > last)
}

/// The service an `MnNetInfo` holds, or `None` for its empty (invalid) entry: Core reads the
/// legacy service and keeps it only if it is valid (D42).
// Core: evo/netinfo.h:343-349 MnNetInfo::Unserialize
pub(crate) fn mn_service(v1: &NetInfoV1) -> Option<Service> {
    let service = Service::from_v1(&v1.0);
    service.addr.is_valid().then_some(service)
}

/// The value Core holds after reading `net_info` from a payload (EV2b D7): a legacy service
/// that is not valid becomes the empty service (`MnNetInfo::Unserialize` keeps the entry only if
/// valid), and an extended entry Core's reader would mark invalid becomes the invalid entry
/// (`NetInfoEntry::Unserialize`). Anything else is unchanged; the function is idempotent.
// Core: evo/netinfo.h:243-263 NetInfoEntry::Unserialize, :343-349 MnNetInfo::Unserialize
pub(crate) fn as_core_reads(net_info: &NetInfo) -> NetInfo {
    match net_info {
        NetInfo::Legacy(v1) if mn_service(v1).is_none() => {
            NetInfo::Legacy(NetInfoV1(dash_primitives::ServiceV1 {
                addr: dash_primitives::AddrV1([0; 16]),
                port: 0,
            }))
        }
        NetInfo::Legacy(_) => net_info.clone(),
        NetInfo::Extended(v2) => NetInfo::Extended(NetInfoV2 {
            version: v2.version,
            entries: v2
                .entries
                .iter()
                .map(|(purpose, group)| {
                    let group = group
                        .iter()
                        .map(|entry| match Entry::read(entry) {
                            Entry::Invalid => NIEntry::Invalid,
                            _ => entry.clone(),
                        })
                        .collect();
                    (*purpose, group)
                })
                .collect(),
        }),
    }
}

/// Core's `netInfo->IsEmpty()`.
// Core: evo/netinfo.h:355, :442
pub fn is_empty(net_info: &NetInfo) -> bool {
    match net_info {
        NetInfo::Legacy(v1) => mn_service(v1).is_none(),
        NetInfo::Extended(v2) => v2.version == EXT_CURRENT_VERSION && v2.entries.is_empty(),
    }
}

/// Core's `netInfo->HasEntries(purpose)`.
///
/// `net_info`'s purpose groups must be sorted by purpose byte and unique, as a decoded value's are
/// (D41).
// Core: evo/netinfo.h:354; evo/netinfo.cpp:581-586
pub fn has_entries(net_info: &NetInfo, purpose: NIPurpose) -> bool {
    match net_info {
        NetInfo::Legacy(v1) => purpose == NIPurpose::CoreP2p && mn_service(v1).is_some(),
        NetInfo::Extended(v2) => {
            !matches!(purpose, NIPurpose::Unknown(_))
                && v2.entries.iter().any(|(p, g)| *p == purpose && !g.is_empty())
        }
    }
}

/// Core's `netInfo->GetPrimary()`: the first `CORE_P2P` service, or `CService{}` (`[::]:0`).
///
/// `net_info`'s purpose groups must be sorted by purpose byte and unique, as a decoded value's are
/// (D41).
// Core: evo/netinfo.cpp:343-349, :566-579
pub fn primary(net_info: &NetInfo) -> Service {
    let service = match net_info {
        NetInfo::Legacy(v1) => mn_service(v1),
        NetInfo::Extended(v2) => {
            let group = v2.entries.iter().find(|(p, _)| *p == NIPurpose::CoreP2p);
            match group.and_then(|(_, g)| g.first()).map(Entry::read) {
                Some(Entry::Service(service)) => Some(service),
                _ => None,
            }
        }
    };
    service.unwrap_or_default()
}

/// Core's `MnNetInfo::Validate`.
// Core: evo/netinfo.cpp:351-357
pub fn validate_mn(v1: &NetInfoV1, rules: &NetInfoRules) -> NetInfoStatus {
    match mn_service(v1) {
        None => NetInfoStatus::Malformed,
        Some(service) => validate_service_mn(&service, rules),
    }
}

/// Core's `ExtNetInfo::Validate`: the structural checks, then for each purpose in map order all
/// of its checks before the next purpose (M6).
///
/// `v2`'s purposes must be known and ascending, as a value Core reads has them (D41, CE4-01);
/// rc.4 dropped the unknown-purpose check this made unreachable.
// Core (rc.4): evo/netinfo.cpp:585-623
pub fn validate_ext(v2: &NetInfoV2, rules: &NetInfoRules) -> NetInfoStatus {
    debug_assert!(purposes_in_read_order(v2), "purpose groups out of Core's read order");
    if v2.version == 0 || v2.version > EXT_CURRENT_VERSION || v2.entries.is_empty() {
        return NetInfoStatus::Malformed;
    }
    if has_addr_port_duplicates(v2) {
        return NetInfoStatus::Duplicate;
    }
    for (purpose, entries) in &v2.entries {
        if entries.is_empty() {
            return NetInfoStatus::Malformed;
        }
        if entries.len() > MAX_ENTRIES_EXTNETINFO {
            return NetInfoStatus::MaxLimit;
        }
        if has_addr_duplicates(entries) {
            return NetInfoStatus::Duplicate;
        }
        for entry in entries {
            let status = match Entry::read(entry) {
                Entry::Invalid => NetInfoStatus::Malformed,
                Entry::Service(service) => validate_service_ext(*purpose, &service, rules),
                Entry::Domain(..) if *purpose != NIPurpose::PlatformHttps => {
                    NetInfoStatus::BadInput
                }
                Entry::Domain(name, port) => validate_domain_port(name, port),
            };
            if status != NetInfoStatus::Success {
                return status;
            }
        }
    }
    NetInfoStatus::Success
}

/// Whether `v2`'s purpose groups are in the order rc.4 reads them: known, sorted by purpose
/// byte, each purpose once (D41, [`purpose_follows`]). Values Core reads and [`add_entry`] keep
/// it; the functions here assume it.
pub(crate) fn purposes_in_read_order(v2: &NetInfoV2) -> bool {
    let mut last = None;
    v2.entries.iter().all(|(purpose, _)| {
        let purpose = purpose.to_base();
        let follows = purpose_follows(purpose, last);
        last = Some(purpose);
        follows
    })
}

/// Core's `ExtNetInfo::HasAddrPortDuplicates`: two equal entries anywhere (two invalid entries are
/// equal), stopping at the first failed insert as Core's `std::set` does.
// Core: evo/netinfo.cpp:383-393
fn has_addr_port_duplicates(v2: &NetInfoV2) -> bool {
    let mut seen = BTreeSet::new();
    v2.entries.iter().flat_map(|(_, entries)| entries).any(|e| !seen.insert(Entry::read(e)))
}

/// Core's `ExtNetInfo::HasAddrDuplicates`: two entries of one purpose with the same
/// `ToStringAddr()`, compared whole (D15). Only called with at most
/// [`MAX_ENTRIES_EXTNETINFO`] entries.
// Core: evo/netinfo.cpp:401-412
fn has_addr_duplicates(entries: &[NIEntry]) -> bool {
    let mut seen: [AddrStr; MAX_ENTRIES_EXTNETINFO] = Default::default();
    for (i, entry) in entries.iter().enumerate() {
        seen[i] = Entry::read(entry).to_string_addr();
        if seen[..i].contains(&seen[i]) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests;
