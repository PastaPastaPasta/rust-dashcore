//! Core's `AddEntry` for both kinds: the string round trip `SetStateVersion` and
//! `ApplyProUpServTx` use (EV1 §3.16).

use dash_netaddr::{NetAddr, Service, lookup, maybe_flip_ipv6_to_cjdns, split_host_port};
use dash_primitives::{
    AddrV1, NIEntry, NIPurpose, NetInfo, NetInfoV1, NetInfoV2, ServiceV1, ServiceV2,
};
use dash_types::Numeric;

use super::entry::{
    DomainStatus, Entry, SAFE_CHARS_IPV4, SAFE_CHARS_IPV4_6, SAFE_CHARS_RFC1035, TLDS_PRIVACY,
    match_chars, match_suffix, validate_domain, validate_domain_port, validate_service_ext,
    validate_service_mn,
};
use super::{
    MAX_ENTRIES_EXTNETINFO, NetInfoRules, NetInfoStatus, mn_service, purposes_in_read_order,
};

/// Core's `netInfo->AddEntry(purpose, input)`: on [`NetInfoStatus::Success`] the entry is added
/// to `net_info`; otherwise `net_info` is unchanged.
///
/// Core takes a `std::string`; an input that is not UTF-8 fails both of Core's character filters
/// after the purpose check, so the caller maps it to [`NetInfoStatus::BadInput`] there (the
/// bytes form is [`add_entry_bytes`]).
///
/// `net_info`'s purpose groups must be sorted by purpose byte and unique, as a decoded value's are
/// (D41).
pub fn add_entry(
    net_info: &mut NetInfo,
    purpose: NIPurpose,
    input: &str,
    rules: &NetInfoRules,
) -> NetInfoStatus {
    match net_info {
        NetInfo::Legacy(v1) => add_mn(v1, purpose, input, rules),
        NetInfo::Extended(v2) => add_ext(v2, purpose, input, rules),
    }
}

/// [`add_entry`] over Core's byte string (the same precondition).
pub fn add_entry_bytes(
    net_info: &mut NetInfo,
    purpose: NIPurpose,
    input: &[u8],
    rules: &NetInfoRules,
) -> NetInfoStatus {
    match std::str::from_utf8(input) {
        Ok(input) => add_entry(net_info, purpose, input, rules),
        // A non-ASCII byte is in neither filter, whichever side of the split it lands on: in
        // the port it fails the port parse and stays in the host.
        Err(_) => match net_info {
            NetInfo::Legacy(v1) if purpose != NIPurpose::CoreP2p || mn_service(v1).is_some() => {
                NetInfoStatus::MaxLimit
            }
            NetInfo::Extended(_) if matches!(purpose, NIPurpose::Unknown(_)) => {
                NetInfoStatus::MaxLimit
            }
            _ => NetInfoStatus::BadInput,
        },
    }
}

/// Core's `MnNetInfo::AddEntry`.
// Core: evo/netinfo.cpp:307-330
fn add_mn(
    v1: &mut NetInfoV1,
    purpose: NIPurpose,
    input: &str,
    rules: &NetInfoRules,
) -> NetInfoStatus {
    if purpose != NIPurpose::CoreP2p || mn_service(v1).is_some() {
        return NetInfoStatus::MaxLimit;
    }
    let (addr, port) = split_host_port(input, rules.default_port);
    // Contains invalid characters, unlikely to pass Lookup(), fast-fail.
    if !match_chars(addr, SAFE_CHARS_IPV4) {
        return NetInfoStatus::BadInput;
    }
    let Some(service) = lookup(addr, port) else {
        return NetInfoStatus::BadInput;
    };
    let ret = validate_service_mn(&service, rules);
    if ret == NetInfoStatus::Success {
        v1.0 = ServiceV1 {
            addr: AddrV1(service.addr.v1_bytes()),
            port: service.port,
        };
    }
    ret
}

/// Core's `ExtNetInfo::AddEntry`.
// Core: evo/netinfo.cpp:500-552
fn add_ext(
    v2: &mut NetInfoV2,
    purpose: NIPurpose,
    input: &str,
    rules: &NetInfoRules,
) -> NetInfoStatus {
    if matches!(purpose, NIPurpose::Unknown(_)) {
        return NetInfoStatus::MaxLimit;
    }
    // No assumed port: a missing one is 0, which fails as BadPort.
    let (addr, port) = split_host_port(input, 0);

    if !match_chars(addr, SAFE_CHARS_IPV4_6) {
        if !match_chars(addr, SAFE_CHARS_RFC1035) {
            return NetInfoStatus::BadInput;
        }
        if match_suffix(addr, &TLDS_PRIVACY) {
            // A privacy-network name is stored as a service.
            if let Some(netaddr) = NetAddr::set_special(addr) {
                return add_service(v2, purpose, Service::new(netaddr, port), rules);
            }
        } else if port != 0 && validate_domain(addr) == DomainStatus::Success {
            // `DomainPort::Set`: lowercased (domains are case-insensitive).
            let name = addr.to_ascii_lowercase();
            let ret = validate_domain_port(&name, port);
            if ret != NetInfoStatus::Success {
                return ret;
            }
            let entry = NIEntry::Domain {
                name: name.into_bytes(),
                port,
            };
            return process_candidate(v2, purpose, entry);
        }
        // `SetSpecial` or `DomainPort::Set` failed.
        return NetInfoStatus::BadInput;
    }

    let Some(service) = lookup(addr, port) else {
        return NetInfoStatus::BadInput;
    };
    let service = maybe_flip_ipv6_to_cjdns(service, rules.config.cjdns_reachable);
    add_service(v2, purpose, service, rules)
}

/// `ValidateService(service)`, then `ProcessCandidate` with the service's entry.
fn add_service(
    v2: &mut NetInfoV2,
    purpose: NIPurpose,
    service: Service,
    rules: &NetInfoRules,
) -> NetInfoStatus {
    match validate_service_ext(purpose, &service, rules) {
        NetInfoStatus::Success => process_candidate(v2, purpose, service_entry(&service)),
        status => status,
    }
}

/// A valid service as the entry Core stores (`NetInfoEntry{service}`).
fn service_entry(service: &Service) -> NIEntry {
    NIEntry::Service(ServiceV2 {
        addr: service.addr.to_addrv2(),
        port: service.port,
    })
}

/// Core's `ExtNetInfo::ProcessCandidate`. Its duplicate check reads `GetEntries()`, every entry
/// of every group (rc.4 dropped the `m_all_entries` cache it read before; a match anywhere is a
/// match either way).
// Core (rc.4): evo/netinfo.cpp:419-446
fn process_candidate(v2: &mut NetInfoV2, purpose: NIPurpose, candidate: NIEntry) -> NetInfoStatus {
    let view = Entry::read(&candidate);
    debug_assert!(view != Entry::Invalid, "Core asserts IsTriviallyValid");
    debug_assert!(purposes_in_read_order(v2), "purpose groups out of Core's read order");
    if v2.entries.iter().flat_map(|(_, group)| group).any(|e| Entry::read(e) == view) {
        return NetInfoStatus::Duplicate;
    }
    if matches!(view, Entry::Domain(..)) && purpose != NIPurpose::PlatformHttps {
        // Domains only allowed for Platform HTTPS API.
        return NetInfoStatus::BadInput;
    }
    match v2.entries.iter_mut().find(|(p, _)| *p == purpose) {
        Some((_, group)) => {
            if group.len() >= MAX_ENTRIES_EXTNETINFO {
                return NetInfoStatus::MaxLimit;
            }
            let addr = view.to_string_addr();
            if group.iter().any(|e| Entry::read(e).to_string_addr() == addr) {
                return NetInfoStatus::Duplicate;
            }
            group.push(candidate);
        }
        None => {
            // Core's map keeps its keys sorted (D41).
            let at = v2.entries.partition_point(|(p, _)| p.to_base() < purpose.to_base());
            v2.entries.insert(at, (purpose, vec![candidate]));
        }
    }
    NetInfoStatus::Success
}
