//! `banlist.json` against files Dash Core v24.0.0-rc.3 wrote (`tests/data/README.md` says how),
//! and the ban-list edge cases where Core's integer handling decides the outcome.

use dash_p2p::ban::{BanList, DEFAULT_BAN_TIME, banlist_json};
use dash_p2p::{NetAddr, SubNet};

/// The `setmocktime` the fixture's bans were made at.
const NOW: i64 = 1_700_000_000;
/// `regtest/banlist.json` after the `setban` calls in `bans_as_core_made_them`.
const CORE_BANLIST: &[u8] = include_bytes!("data/core_banlist.json");
/// `regtest/banlist.json` from a fresh datadir (`LoadBanlist` recreates it at startup).
const CORE_BANLIST_EMPTY: &[u8] = include_bytes!("data/core_banlist_empty.json");

fn ip(s: &str) -> NetAddr {
    NetAddr::from_ip(s.parse().unwrap())
}

/// The fixture's `setban` calls, in the same order and with the same arguments.
fn bans_as_core_made_them() -> BanList {
    let bans = BanList::new(1, DEFAULT_BAN_TIME);
    // setban 1.2.3.4 add 4102444800 true
    assert!(bans.ban(SubNet::single(ip("1.2.3.4")), 4_102_444_800, true, NOW));
    // setban 2a01:4f8::1 add 3600
    assert!(bans.ban(SubNet::single(ip("2a01:4f8::1")), 3600, false, NOW));
    // setban 10.0.0.0/8 add 1800000000 true
    assert!(bans.ban(SubNet::with_prefix(ip("10.0.0.0"), 8), 1_800_000_000, true, NOW));
    // setban 2001:db8:1::/48 add 4102444802 true
    assert!(bans.ban(SubNet::with_prefix(ip("2001:db8:1::"), 48), 4_102_444_802, true, NOW));
    // setban 192.168.0.0/255.255.0.0 add 1700086400 true
    assert!(bans.ban(SubNet::with_prefix(ip("192.168.0.0"), 16), 1_700_086_400, true, NOW));
    // setban 9.8.7.6/32 add 1750000000 true
    assert!(bans.ban(SubNet::with_prefix(ip("9.8.7.6"), 32), 1_750_000_000, true, NOW));
    // setban 2a02:1234::/ffff:ffff:: add 86400
    assert!(bans.ban(SubNet::with_prefix(ip("2a02:1234::"), 32), 86_400, false, NOW));
    bans
}

/// Our writer gives Core's bytes for the same bans: `_warning_` first, `ToJson`'s key order,
/// `CSubNet` order and UniValue's `write(4, 1)` layout, whatever features `serde_json` is
/// built with.
#[test]
fn writes_core_bytes() {
    let entries = bans_as_core_made_them().take_dump(NOW).unwrap();
    assert_eq!(
        String::from_utf8(banlist_json(&entries)).unwrap(),
        String::from_utf8(CORE_BANLIST.to_vec()).unwrap()
    );
}

/// An empty list is UniValue's empty array, `[` and `]` on separate lines.
#[test]
fn writes_core_empty_bytes() {
    assert_eq!(
        String::from_utf8(banlist_json(&[])).unwrap(),
        String::from_utf8(CORE_BANLIST_EMPTY.to_vec()).unwrap()
    );
}

/// Core's files read back to the same bans, and a dump writes them out unchanged.
#[test]
fn reads_core_files() {
    let bans = BanList::new(2, DEFAULT_BAN_TIME);
    bans.load(CORE_BANLIST, NOW);
    assert_eq!(bans.banned(NOW), bans_as_core_made_them().banned(NOW));
    assert!(bans.take_dump(NOW).is_none());
    assert_eq!(banlist_json(&bans.banned(NOW)), CORE_BANLIST);

    let empty = BanList::new(2, DEFAULT_BAN_TIME);
    empty.load(CORE_BANLIST_EMPTY, NOW);
    assert!(empty.banned(NOW).is_empty());
    assert!(empty.take_dump(NOW).is_none());
}

/// Core: `(since_unix_epoch ? 0 : GetTime()) + offset` in `int64_t` (banman.cpp:149), which
/// wraps in practice: an absurd relative offset bans until a negative time, which no existing
/// entry is shorter than, so nothing changes.
#[test]
fn absurd_offset_wraps() {
    let bans = BanList::new(1, DEFAULT_BAN_TIME);
    let net = SubNet::single(ip("1.2.3.4"));
    assert!(!bans.ban(net.clone(), i64::MAX, false, NOW));
    assert!(!bans.is_banned(&ip("1.2.3.4"), NOW));
    // Core's `m_banned[sub_net]` left a null entry, which the dump's `SweepBanned` erases, and
    // the erase marks the list dirty (banman.cpp SweepBanned).
    assert_eq!(bans.take_dump(NOW), Some(Vec::new()));
    // From the epoch nothing wraps.
    assert!(bans.ban(net, i64::MAX, true, NOW));
    assert_eq!(bans.banned(NOW)[0].1.ban_until, i64::MAX);
}

fn entry(version: &str, address: &str) -> String {
    format!(
        r#"{{"version": {version}, "ban_created": {NOW}, "banned_until": 4102444800, "address": "{address}"}}"#
    )
}

/// Core reads `version` with `getInt<int>()` (net_types.cpp:61): a value outside `int` throws,
/// `CBanDB::Read` returns false (addrdb.cpp:175-177) and `LoadBanlist` recreates the whole list
/// empty and dirty. A version inside `int` other than 1 drops only its own entry.
#[test]
fn version_outside_int_refuses_the_file() {
    let file = |v: &str| {
        format!(r#"{{"banned_nets": [{}, {}]}}"#, entry(v, "1.2.3.4/32"), entry("1", "5.6.7.8/32"))
    };

    let bans = BanList::new(1, DEFAULT_BAN_TIME);
    bans.load(file("4294967297").as_bytes(), NOW);
    assert!(bans.banned(NOW).is_empty());
    assert_eq!(bans.take_dump(NOW), Some(Vec::new()));

    for v in ["2147483648", "-2147483649"] {
        let bans = BanList::new(1, DEFAULT_BAN_TIME);
        bans.load(file(v).as_bytes(), NOW);
        assert!(bans.banned(NOW).is_empty(), "{v}");
    }

    // In range: only that entry goes, and the file is not rewritten.
    for v in ["2", "2147483647", "-2147483648"] {
        let bans = BanList::new(1, DEFAULT_BAN_TIME);
        bans.load(file(v).as_bytes(), NOW);
        let kept = bans.banned(NOW);
        assert_eq!(kept.len(), 1, "{v}");
        assert_eq!(kept[0].0.to_string(), "5.6.7.8/32");
        assert!(bans.take_dump(NOW).is_none(), "{v}");
    }
}
