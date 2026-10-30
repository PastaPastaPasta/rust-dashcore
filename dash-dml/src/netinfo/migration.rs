//! `SetStateVersion`'s NetInfo step (EV1 §3.16.1): the conversion EV2b's builder installs
//! ([`migrate`]) and its read-only reject for EV1 ([`migration_reject`]); one code path (EV1 D9,
//! D24).

use dash_primitives::{AddrV1, NIEntry, NIPurpose, NetInfo, NetInfoV1, NetInfoV2, ServiceV1};

use super::entry::Entry;
use super::{EXT_CURRENT_VERSION, NetInfoRules, NetInfoStatus, add_entry, is_empty, mn_service};
use crate::MnType;
use crate::specialtx::reasons::BAD_PROTX_NETINFO_VERSION;
use crate::types::EXT_ADDR;

/// The reason `SetStateVersion(state, target, mn_type)` fails with, or `None` when it succeeds;
/// `cur_version` is the state's `nVersion` when it is entered. Only the NetInfo steps can fail.
///
/// `net_info`'s purpose groups must be sorted by purpose byte and unique, as a decoded value's are
/// (D41).
///
/// Core asserts `target > 0` (`MakeNetInfo`); a ProTx of version 0 is refused before any caller
/// gets here, and this reads it as a legacy target.
// Core: evo/specialtxman.cpp:33-42 AddNetInfoEntries, :51-106 SetStateVersion
pub fn migration_reject(
    net_info: &NetInfo,
    cur_version: u16,
    target: u16,
    mn_type: MnType,
    rules: &NetInfoRules,
) -> Option<&'static str> {
    migrate(net_info, cur_version, target, mn_type, rules).err()
}

/// `SetStateVersion`'s NetInfo step: `Ok(None)` when the version and the NetInfo kind already
/// match (Core's early return, `specialtxman.cpp` v24:74-76), `Ok(Some(converted))` with the
/// NetInfo of `target`'s kind holding the carried entries, or the reject.
// Core: evo/specialtxman.cpp:74-104 SetStateVersion
pub(crate) fn migrate(
    net_info: &NetInfo,
    cur_version: u16,
    target: u16,
    mn_type: MnType,
    rules: &NetInfoRules,
) -> Result<Option<NetInfo>, &'static str> {
    let needs_extended = i32::from(target) >= EXT_ADDR;
    let stores_platform = matches!(net_info, NetInfo::Extended(_));
    if cur_version == target && stores_platform == needs_extended {
        return Ok(None);
    }
    // Core: evo/netinfo.cpp:277-284 MakeNetInfo
    let mut converted = if needs_extended {
        NetInfo::Extended(NetInfoV2 {
            version: EXT_CURRENT_VERSION,
            entries: Vec::new(),
        })
    } else {
        NetInfo::Legacy(NetInfoV1(ServiceV1 {
            addr: AddrV1([0; 16]),
            port: 0,
        }))
    };
    let mut carry = |purpose| add_entries(&mut converted, net_info, purpose, rules);
    let ok = if needs_extended {
        carry(NIPurpose::CoreP2p)
            && if stores_platform {
                carry(NIPurpose::PlatformP2p) && carry(NIPurpose::PlatformHttps)
            } else {
                // The scalar Platform ports are not carried over.
                !(mn_type == MnType::Evo && !is_empty(net_info))
            }
    } else {
        carry(NIPurpose::CoreP2p)
    };
    if ok {
        Ok(Some(converted))
    } else {
        Err(BAD_PROTX_NETINFO_VERSION)
    }
}

/// Core's `AddNetInfoEntries(converted, purpose, old->GetEntries(purpose))`: every entry's
/// `ToStringAddrPort()` added in order, until one fails.
fn add_entries(
    converted: &mut NetInfo,
    old: &NetInfo,
    purpose: NIPurpose,
    rules: &NetInfoRules,
) -> bool {
    let carry = |converted: &mut NetInfo, entry: Entry<'_>| {
        add_entry(converted, purpose, entry.to_string_addr_port().as_str(), rules)
            == NetInfoStatus::Success
    };
    match old {
        // Core: evo/netinfo.cpp:332-341 MnNetInfo::GetEntries
        NetInfo::Legacy(v1) => match mn_service(v1) {
            Some(service) if purpose == NIPurpose::CoreP2p => {
                carry(converted, Entry::Service(service))
            }
            _ => true,
        },
        // Core: evo/netinfo.cpp:554-565 ExtNetInfo::GetEntries
        NetInfo::Extended(v2) => {
            let group = v2.entries.iter().find(|(p, _)| *p == purpose);
            let entries: &[NIEntry] = group.map_or(&[], |(_, g)| g);
            entries.iter().all(|e| carry(converted, Entry::read(e)))
        }
    }
}
