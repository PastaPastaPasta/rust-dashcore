//! Bans and discouragement with Core's semantics (`banman.cpp`): a map of banned subnets with
//! expiry, and a rolling bloom filter of discouraged addresses (50,000 entries, 10⁻⁶).

use std::collections::BTreeMap;

use parking_lot::Mutex;

use crate::bloom::RollingBloom;
use crate::lookup::lookup_subnet;
use crate::netaddr::{NetAddr, SubNet};

/// Core's `DEFAULT_MISBEHAVING_BANTIME`: 24 hours.
pub const DEFAULT_BAN_TIME: i64 = 60 * 60 * 24;

/// One ban (Core's `CBanEntry`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BanEntry {
    /// `nCreateTime`.
    pub create_time: i64,
    /// `nBanUntil`.
    pub ban_until: i64,
}

struct Inner {
    banned: BTreeMap<SubNet, BanEntry>,
    discouraged: RollingBloom,
    dirty: bool,
}

/// Core's `BanMan`. Its lock is a leaf; nothing does I/O under it.
pub struct BanList {
    inner: Mutex<Inner>,
    default_ban_time: i64,
}

impl BanList {
    /// An empty list with `-bantime` (`default_ban_time`, seconds) as the default ban; `tweak`
    /// keys the discouragement filter (Core draws it with `GetRand`).
    pub fn new(tweak: u32, default_ban_time: i64) -> Self {
        Self {
            inner: Mutex::new(Inner {
                banned: BTreeMap::new(),
                discouraged: RollingBloom::new(50_000, 0.000_001, tweak),
                dirty: false,
            }),
            default_ban_time,
        }
    }

    /// Core's `Ban(subnet, ban_time_offset, since_unix_epoch)`: an offset ≤ 0 is the default ban
    /// from now. Returns whether the list changed (an existing longer ban wins), in which case
    /// Core dumps the list at once.
    // Core: banman.cpp BanMan::Ban
    pub fn ban(&self, net: SubNet, offset: i64, since_unix_epoch: bool, now: i64) -> bool {
        let (offset, since_unix_epoch) = if offset <= 0 {
            (self.default_ban_time, false)
        } else {
            (offset, since_unix_epoch)
        };
        // Core adds in `int64_t`, which wraps in practice on an absurd offset.
        let entry = BanEntry {
            create_time: now,
            ban_until: if since_unix_epoch {
                0
            } else {
                now
            }
            .wrapping_add(offset),
        };
        let mut inner = self.inner.lock();
        let slot = inner.banned.entry(net).or_default();
        if slot.ban_until < entry.ban_until {
            *slot = entry;
            inner.dirty = true;
            true
        } else {
            false
        }
    }

    /// Core's `Unban(subnet)`.
    pub fn unban(&self, net: &SubNet) -> bool {
        let mut inner = self.inner.lock();
        let removed = inner.banned.remove(net).is_some();
        inner.dirty |= removed;
        removed
    }

    /// Core's `IsBanned(addr)`: an unexpired ban whose subnet matches.
    pub fn is_banned(&self, addr: &NetAddr, now: i64) -> bool {
        self.inner.lock().banned.iter().any(|(net, e)| now < e.ban_until && net.matches(addr))
    }

    /// Core's `IsBanned(subnet)`: an unexpired ban on exactly `net`.
    pub fn is_subnet_banned(&self, net: &SubNet, now: i64) -> bool {
        self.inner.lock().banned.get(net).is_some_and(|e| now < e.ban_until)
    }

    /// Core's `Discourage(addr)`.
    pub fn discourage(&self, addr: &NetAddr) {
        self.inner.lock().discouraged.insert(&addr.addr_bytes());
    }

    /// Core's `IsDiscouraged(addr)`.
    pub fn is_discouraged(&self, addr: &NetAddr) -> bool {
        self.inner.lock().discouraged.contains(&addr.addr_bytes())
    }

    /// Core's `GetBanned`, after `SweepBanned` (which drops invalid and expired entries).
    pub fn banned(&self, now: i64) -> Vec<(SubNet, BanEntry)> {
        let mut inner = self.inner.lock();
        Self::sweep(&mut inner, now);
        Self::entries(&inner)
    }

    fn entries(inner: &Inner) -> Vec<(SubNet, BanEntry)> {
        inner.banned.iter().map(|(n, e)| (n.clone(), e.clone())).collect()
    }

    // Core: banman.cpp BanMan::SweepBanned
    fn sweep(inner: &mut Inner, now: i64) {
        let before = inner.banned.len();
        inner.banned.retain(|net, e| {
            let keep = net.is_valid() && now <= e.ban_until;
            if !keep {
                tracing::debug!(target: "net", "Removed banned node address/subnet: {net}");
            }
            keep
        });
        inner.dirty |= inner.banned.len() != before;
    }

    /// Core's `ClearBanned`.
    pub fn clear(&self) {
        let mut inner = self.inner.lock();
        inner.banned.clear();
        inner.dirty = true;
    }

    /// Marks the list as needing a save (Core sets `m_is_dirty` again when a write fails).
    pub fn mark_dirty(&self) {
        self.inner.lock().dirty = true;
    }

    /// The swept entries to save, taken with the dirty flag cleared in one critical section, or
    /// `None` when nothing changed since the last dump. A failed save must call
    /// [`BanList::mark_dirty`].
    // Core: banman.cpp:47-58 BanMan::DumpBanlist (SweepBanned, m_is_dirty, copy, clear)
    pub fn take_dump(&self, now: i64) -> Option<Vec<(SubNet, BanEntry)>> {
        let mut inner = self.inner.lock();
        Self::sweep(&mut inner, now);
        if !core::mem::take(&mut inner.dirty) {
            return None;
        }
        Some(Self::entries(&inner))
    }

    /// Core's `LoadBanlist`: replaces the list with the entries of a `banlist.json` image that
    /// Core keeps (version 1, a parseable subnet), then sweeps. A file Core cannot parse leaves
    /// the list empty and dirty, so it is rewritten ("Recreating the banlist database").
    // Core: banman.cpp BanMan::LoadBanlist; net_types.cpp BanMapFromJson
    pub fn load(&self, bytes: &[u8], now: i64) {
        let mut banned = BTreeMap::new();
        let ok = parse_banlist(bytes, &mut banned);
        let mut inner = self.inner.lock();
        inner.banned = if ok {
            banned
        } else {
            BTreeMap::new()
        };
        inner.dirty = !ok;
        if !ok {
            tracing::info!(target: "net", "Recreating the banlist database");
        }
        Self::sweep(&mut inner, now);
    }
}

/// Core's `WriteSettings` header: the first key of every settings file it writes.
// Core: util/settings.cpp:133-136 WriteSettings (PACKAGE_NAME "Dash Core")
const SETTINGS_WARNING: &str = "This file is automatically generated and updated by Dash Core. \
Please do not edit this file while the node is running, as any changes might be ignored or \
overwritten.";

/// Core's `banlist.json` image of `entries`, byte for byte: `WriteSettings`'s object (the
/// `_warning_` key, then `banned_nets`) written by UniValue's `write(4, 1)`, each entry's keys in
/// `ToJson`'s order (`version`, `ban_created`, `banned_until`, then `address`), in `CSubNet` order
/// (D13).
// Core: addrdb.cpp:139-150 CBanDB::Write; util/settings.cpp:127-150 WriteSettings;
// net_types.cpp:21-49 CBanEntry::ToJson, BanMapToJson; univalue_write.cpp UniValue::write
pub fn banlist_json(entries: &[(SubNet, BanEntry)]) -> Vec<u8> {
    use core::fmt::Write as _;
    // `write(4, 1)`: the top object's keys at 4 spaces, an entry's braces at 8, its keys at 12.
    // Neither the warning nor a `CSubNet` string holds a character `json_escape` changes.
    let mut s = format!("{{\n    \"_warning_\": \"{SETTINGS_WARNING}\",\n    \"banned_nets\": [\n");
    for (i, (net, e)) in entries.iter().enumerate() {
        let comma = if i + 1 == entries.len() {
            ""
        } else {
            ","
        };
        let _ = write!(
            s,
            concat!(
                "        {{\n",
                "            \"version\": 1,\n",
                "            \"ban_created\": {},\n",
                "            \"banned_until\": {},\n",
                "            \"address\": \"{}\"\n",
                "        }}{}\n",
            ),
            e.create_time, e.ban_until, net, comma,
        );
    }
    // An empty array is `[`, a newline and the closing bracket at the array's own indent.
    s.push_str("    ]\n}\n");
    s.into_bytes()
}

fn parse_banlist(bytes: &[u8], out: &mut BTreeMap<SubNet, BanEntry>) -> bool {
    let Ok(doc) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    let entries: Vec<&serde_json::Value> = match &doc["banned_nets"] {
        serde_json::Value::Array(a) => a.iter().collect(),
        serde_json::Value::Object(o) => o.values().collect(),
        _ => return false,
    };
    for e in entries {
        // `getInt<int>()` throws outside `int`, and the throw refuses the whole file
        // (net_types.cpp:61, addrdb.cpp:175-177).
        let Some(version) = e["version"].as_i64().and_then(|v| i32::try_from(v).ok()) else {
            return false;
        };
        if version != 1 {
            tracing::info!(target: "net", "Dropping entry with unknown version ({version}) from ban list");
            continue;
        }
        let Some(address) = e["address"].as_str() else {
            return false;
        };
        // `LookupSubNet` while init builds `BanMan` (init.cpp:1713), before it trims the
        // reachable networks (init.cpp:1847-1862): every network is reachable then, so an
        // `fc00::/8` entry reads as CJDNS, whatever `-cjdnsreachable` says (P4a D17).
        let Some(net) = lookup_subnet(address, true) else {
            tracing::info!(target: "net", "Dropping entry with unparseable address or subnet ({address}) from ban list");
            continue;
        };
        let (Some(create_time), Some(ban_until)) =
            (e["ban_created"].as_i64(), e["banned_until"].as_i64())
        else {
            return false;
        };
        out.insert(
            net,
            BanEntry {
                create_time,
                ban_until,
            },
        );
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(d: u8) -> NetAddr {
        NetAddr::ipv4([1, 2, 3, d])
    }

    #[test]
    fn ban_semantics() {
        let bans = BanList::new(1, DEFAULT_BAN_TIME);
        let net = SubNet::with_prefix(a(0), 24);
        assert!(bans.ban(net.clone(), 0, false, 1000));
        assert!(bans.is_banned(&a(9), 1000));
        assert!(!bans.is_banned(&a(9), 1000 + DEFAULT_BAN_TIME));
        // A shorter ban does not replace a longer one.
        assert!(!bans.ban(net.clone(), 10, false, 1000));
        assert!(bans.ban(net.clone(), 1000 + DEFAULT_BAN_TIME * 2, true, 1000));
        assert_eq!(bans.banned(1000)[0].1.ban_until, 1000 + DEFAULT_BAN_TIME * 2);
        assert!(bans.take_dump(1000).is_some());
        assert!(bans.unban(&net));
        assert!(bans.banned(1000).is_empty());
    }

    #[test]
    fn sweep_keeps_until_after_expiry() {
        let bans = BanList::new(1, DEFAULT_BAN_TIME);
        bans.ban(SubNet::single(a(1)), 10, false, 0);
        // `now > nBanUntil` sweeps; `now < nBanUntil` bans.
        assert_eq!(bans.banned(10).len(), 1);
        assert!(!bans.is_banned(&a(1), 10));
        assert!(bans.banned(11).is_empty());
    }

    #[test]
    fn banlist_json_round_trip() {
        let bans = BanList::new(1, DEFAULT_BAN_TIME);
        bans.ban(SubNet::with_prefix(a(0), 24), 100, false, 1000);
        bans.ban(SubNet::single(NetAddr::from_ip("2001::1".parse().unwrap())), 100, false, 1000);
        let bytes = banlist_json(&bans.banned(1000));
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("\"address\": \"1.2.3.0/24\""), "{text}");
        let back = BanList::new(2, DEFAULT_BAN_TIME);
        back.load(&bytes, 1000);
        assert_eq!(back.banned(1000), bans.banned(1000));
        assert!(back.take_dump(1000).is_none());
        // A non-contiguous or wrong-family netmask makes the subnet invalid (dropped).
        assert!(lookup_subnet("1.2.3.0/255.0.255.0", true).is_none());
        assert!(lookup_subnet("1.2.3.0/ffff::", true).is_none());
        assert_eq!(lookup_subnet("1.2.3.0/255.255.255.0", true), lookup_subnet("1.2.3.0/24", true));
        // An IPv6 scope id is written as Core's `%N` and read back (D23).
        #[cfg(unix)]
        {
            let scoped = BanList::new(1, DEFAULT_BAN_TIME);
            let link_local = NetAddr::from_ip("fe80::1".parse().unwrap()).with_scope_id(7);
            scoped.ban(SubNet::single(link_local), 100, false, 1000);
            let bytes = banlist_json(&scoped.banned(1000));
            let text = String::from_utf8(bytes.clone()).unwrap();
            assert!(text.contains("\"address\": \"fe80::1%7/128\""), "{text}");
            let reloaded = BanList::new(2, DEFAULT_BAN_TIME);
            reloaded.load(&bytes, 1000);
            assert_eq!(reloaded.banned(1000)[0].0.network().scope_id(), 7);
            assert_eq!(banlist_json(&reloaded.banned(1000)), bytes);
        }
        back.load(b"not json", 1000);
        assert!(back.banned(1000).is_empty());
        assert_eq!(back.take_dump(1000), Some(Vec::new()));
    }

    /// `BanMapFromJson` reads every network `LookupSubNet` does: Tor and I2P names survive a
    /// restart, and an `fc00::/8` entry comes back as CJDNS (P4a D17).
    #[test]
    fn banlist_json_keeps_every_network() {
        use crate::netaddr::Network;
        let bans = BanList::new(1, DEFAULT_BAN_TIME);
        let onion = NetAddr::new(Network::Onion, vec![7; 32]).unwrap();
        let i2p = NetAddr::new(Network::I2p, vec![9; 32]).unwrap();
        let mut fc = [0; 16];
        fc[0] = 0xFC;
        fc[15] = 1;
        let cjdns = NetAddr::new(Network::Cjdns, fc.to_vec()).unwrap();
        for addr in [&onion, &i2p, &cjdns] {
            bans.ban(SubNet::single(addr.clone()), 100, false, 1000);
        }
        let bytes = banlist_json(&bans.banned(1000));
        let text = String::from_utf8(bytes.clone()).unwrap();
        // `CSubNet::ToString` gives no suffix outside IPv4 and IPv6.
        assert!(text.contains(&format!("\"address\": \"{onion}\"")), "{text}");
        assert!(text.contains("\"address\": \"fc00::1\""), "{text}");
        let back = BanList::new(2, DEFAULT_BAN_TIME);
        back.load(&bytes, 1000);
        assert_eq!(back.banned(1000), bans.banned(1000));
        assert!(back.is_banned(&cjdns, 1000));
        assert!(!back.is_banned(&NetAddr::new(Network::Ipv6, fc.to_vec()).unwrap(), 1000));
    }

    #[test]
    fn take_dump_clears_dirty_with_the_snapshot() {
        let bans = BanList::new(1, DEFAULT_BAN_TIME);
        bans.ban(SubNet::single(a(1)), 100, false, 0);
        assert_eq!(bans.take_dump(0).map(|d| d.len()), Some(1));
        assert!(bans.take_dump(0).is_none());
        // A change after a snapshot is not in it, so it stays dirty for the next dump.
        bans.ban(SubNet::single(a(2)), 100, false, 0);
        assert_eq!(bans.take_dump(0).map(|d| d.len()), Some(2));
        // Expiry is a change too.
        assert_eq!(bans.take_dump(101), Some(Vec::new()));
    }

    #[test]
    fn discouragement() {
        let bans = BanList::new(1, DEFAULT_BAN_TIME);
        bans.discourage(&a(5));
        assert!(bans.is_discouraged(&a(5)));
        assert!(!bans.is_discouraged(&a(6)));
    }
}
