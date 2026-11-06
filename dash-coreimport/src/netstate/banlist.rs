//! `banlist.json`: the ban list (`CBanDB` in `addrdb.cpp`, `BanMapFromJson` in `net_types.cpp`)
//! in the settings-file JSON envelope, under the key `banned_nets`. Core v24 ignores the old
//! `banlist.dat`.
//!
//! The reader follows `CBanDB::Read` and `BanMapFromJson` step by step, so it keeps the entries
//! Core keeps and fails on the files Core refuses:
//!
//! 1. the file is read as a settings file ([`super::json::parse_settings_object`]): an object,
//!    top-level keys unique; the `banned_nets` member is looked up in it (a missing key is null);
//! 2. `getValues()`: an array's elements, or an **object's member values in file order**; anything
//!    else fails the file;
//! 3. per entry, in this order: `version` through `getInt<int>` (fails the file if it is not an
//!    integer in range), dropped if it is not 1; `address` through `get_str` (fails the file if
//!    it is not a string); `LookupSubNet` ([`super::subnet`]), and the entry is dropped if the
//!    subnet is invalid; only then `ban_created` and `banned_until` through `getInt<int64_t>`
//!    (fails the file). A bad timestamp on an entry that is dropped earlier is never read;
//! 4. `bans.insert_or_assign(subnet, entry)`: a later entry for the same subnet replaces an earlier
//!    one. The `banmap_t` is a `std::map`, so the entries come out in `CSubNet::operator<` order
//!    ([`Subnet`]'s `Ord`), not in file order.
//!
//! Entry fields are looked up with `operator[]`: the first member of that name, null if the
//! entry is not an object.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use super::error::{FormatError, FormatResult};
use super::json::{JsonStr, MAX_JSON_FILE_SIZE, NULL, parse_settings_object};
use super::subnet::{Subnet, lookup_subnet};

const WHAT: &str = "banlist.json";
const KEY: &str = "banned_nets";
/// `CBanEntry::CURRENT_VERSION`; entries of another version are dropped.
const CURRENT_VERSION: i32 = 1;

/// One ban.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BanEntry {
    pub subnet: Subnet,
    /// The `address` string as written (`1.2.0.0/16`, an onion name, ...). For a subnet that
    /// several entries name, the first entry's string: Core's map keeps its first key, and the key
    /// prints with that entry's scope id (`fe80::1%2`, then `fe80::1%3`, is one ban shown as `%2`)
    /// while the times are the last entry's.
    pub address: JsonStr,
    /// `ban_created`, seconds since the epoch.
    pub ban_created: i64,
    /// `banned_until`, seconds since the epoch.
    pub banned_until: i64,
}

/// A parsed `banlist.json`: the `banmap_t` Core would hold.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Banlist {
    /// One entry per subnet in `banmap_t` order (`CSubNet::operator<`); a later entry for a
    /// subnet has replaced the earlier one's values.
    pub entries: Vec<BanEntry>,
    /// Entries Core drops because their `version` is not 1.
    pub dropped_unknown_version: u64,
    /// Entries Core drops because `LookupSubNet` finds no valid subnet in their `address`.
    pub dropped_invalid_subnet: u64,
    /// Entries that replaced an earlier entry for the same subnet.
    pub replaced_duplicates: u64,
}

fn json_err(message: impl Into<String>) -> FormatError {
    FormatError::Json {
        what: WHAT,
        message: message.into(),
    }
}

/// Parses a `banlist.json` as a node with `-cjdnsreachable` off (the default) reads it.
// Core: addrdb.cpp CBanDB::Read, net_types.cpp BanMapFromJson
pub fn parse(bytes: &[u8]) -> FormatResult<Banlist> {
    parse_with(bytes, false)
}

/// Parses a `banlist.json`; `cjdns_reachable` is the node's `-cjdnsreachable`, which makes
/// `LookupSubNet` read `fc00::/8` addresses as CJDNS.
pub fn parse_with(bytes: &[u8], cjdns_reachable: bool) -> FormatResult<Banlist> {
    let settings = parse_settings_object(bytes, WHAT, MAX_JSON_FILE_SIZE)?;
    let nets = settings.get(KEY).unwrap_or(&NULL);
    let items = nets.values().ok_or_else(|| {
        json_err(format!("{KEY:?} is {}, not an array or object", nets.type_name()))
    })?;

    let mut list = Banlist::default();
    let mut bans: BTreeMap<Subnet, BanEntry> = BTreeMap::new();
    for item in items {
        let version = item.member("version").get_i32().map_err(json_err)?;
        if version != CURRENT_VERSION {
            list.dropped_unknown_version += 1;
            continue;
        }
        let address = item
            .member("address")
            .as_json_str()
            .ok_or_else(|| json_err("entry address is missing or not a string"))?;
        let Some(subnet) = lookup_subnet(address.as_bytes(), cjdns_reachable) else {
            list.dropped_invalid_subnet += 1;
            continue;
        };
        let entry = BanEntry {
            subnet,
            address: address.clone(),
            ban_created: item.member("ban_created").get_i64().map_err(json_err)?,
            banned_until: item.member("banned_until").get_i64().map_err(json_err)?,
        };
        match bans.entry(subnet) {
            Entry::Vacant(slot) => {
                slot.insert(entry);
            }
            Entry::Occupied(mut slot) => {
                // `insert_or_assign` keeps the first `CSubNet` key and assigns the new value, so
                // the ban keeps the first entry's address (`CSubNet::ToString` includes its scope
                // id) and takes the later entry's times.
                let address = std::mem::take(&mut slot.get_mut().address);
                slot.insert(BanEntry {
                    address,
                    ..entry
                });
                list.replaced_duplicates += 1;
            }
        }
    }
    list.entries = bans.into_values().collect();
    Ok(list)
}
