//! IM1a: the Core net-state readers against files a real Dash Core v24.0.0-rc.3 regtest `dashd`
//! wrote (`tests/data/netstate/v24.0.0-rc.3`, provenance in `PROVENANCE.md`), with the node's own
//! RPC output (`expected.json`) as ground truth; and the readers' behaviour on truncated,
//! corrupted and oversize inputs (derived from those files).

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;

use bitcoin_hashes::{Hash, sha256d};
use dash_coreimport::netstate::subnet::Subnet;
use dash_coreimport::netstate::{
    Address, Detected, Format, FormatError, NetAddr, Value as JValue, Version, anchors, banlist,
    detect_named, fee_estimates, mempool, peers, read_file_capped, settings, sniff,
};
use dash_coreimport::{hash_from_hex, hash_to_hex};
use dash_network::Network;
use serde_json::Value;

const NET: Network = Network::Regtest;

fn dir(which: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/netstate/v24.0.0-rc.3").join(which)
}

fn file(which: &str, name: &str) -> Vec<u8> {
    std::fs::read(dir(which).join(name)).unwrap()
}

fn expected() -> Value {
    serde_json::from_slice(&file("populated", "expected.json")).unwrap()
}

/// Replaces the trailing SHA256d of an `addrdb` envelope after the body was edited.
fn rechecksum(mut bytes: Vec<u8>) -> Vec<u8> {
    bytes.truncate(bytes.len() - 32);
    let sum = sha256d::Hash::hash(&bytes).to_byte_array();
    bytes.extend_from_slice(&sum);
    bytes
}

fn base32(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'a'..=b'z' => c - b'a',
            b'2'..=b'7' => c - b'2' + 26,
            _ => panic!("not base32"),
        };
        acc = (acc << 5) | u32::from(v);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

/// The address `getrawaddrman`/`listbanned` print, as the reader's type.
fn addr_from_text(text: &str, network: &str) -> NetAddr {
    match network {
        "ipv4" => NetAddr::Ipv4(text.parse::<Ipv4Addr>().unwrap().octets()),
        "ipv6" => NetAddr::Ipv6(text.parse::<Ipv6Addr>().unwrap().octets()),
        "onion" => {
            let name = text.strip_suffix(".onion").unwrap();
            NetAddr::Onion(base32(name)[..32].try_into().unwrap())
        }
        "i2p" => {
            let name = text.strip_suffix(".b32.i2p").unwrap();
            NetAddr::I2p(base32(name)[..32].try_into().unwrap())
        }
        other => panic!("network {other}"),
    }
}

/// `(bucket, address, port, source, time, services)` of each `getrawaddrman` row of a table.
fn raw_rows(table: &Value) -> BTreeSet<(u32, String, u16, String)> {
    table
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, row)| {
            let bucket = key.split('/').next().unwrap().parse().unwrap();
            let net = row["network"].as_str().unwrap();
            let src_net = row["source_network"].as_str().unwrap();
            let a = addr_from_text(row["address"].as_str().unwrap(), net);
            let s = addr_from_text(row["source"].as_str().unwrap(), src_net);
            (bucket, format!("{a:?}"), row["port"].as_u64().unwrap() as u16, format!("{s:?}"))
        })
        .collect()
}

// ---- peers.dat ----

#[test]
fn peers_dat_matches_getrawaddrman() {
    let bytes = file("populated", "peers.dat");
    let p = peers::parse(&bytes, NET).unwrap();
    assert_eq!((p.format, p.lowest_compatible), (4, 4));
    assert_eq!(p.new_bucket_count, 1024);
    assert_eq!(p.asmap_checksum, [0; 32]);
    assert!(p.bucketing_restorable(&[0; 32]));
    assert!(!p.bucketing_restorable(&[1; 32]));
    assert_eq!((p.trailing_bytes, p.ignored_bucket_refs), (0, 0));

    let raw = &expected()["rawaddrman"];
    // The tried table: Core's bucket is derived, so compare the entries only.
    let tried: BTreeSet<_> = p
        .tried
        .iter()
        .map(|e| (format!("{:?}", e.address.addr), e.address.port, format!("{:?}", e.source)))
        .collect();
    let want_tried: BTreeSet<_> =
        raw_rows(&raw["tried"]).into_iter().map(|(_, a, port, s)| (a, port, s)).collect();
    assert_eq!(tried, want_tried);
    assert_eq!(p.tried.len(), 10);

    // The new table: every row of `getrawaddrman` is one placement (bucket, entry).
    let placed: BTreeSet<_> = p
        .bucket_refs
        .iter()
        .map(|r| {
            let e = &p.new[r.index as usize];
            (r.bucket, format!("{:?}", e.address.addr), e.address.port, format!("{:?}", e.source))
        })
        .collect();
    assert_eq!(placed, raw_rows(&raw["new"]));
    assert_eq!(p.new.len(), 34);

    assert!(p.new.iter().all(|e| e.last_success == 0));
    assert!(p.tried.iter().all(|e| e.last_success == 1_417_713_337));
    for e in p.new.iter().chain(&p.tried) {
        assert!(e.address.v2, "Core v24 stores every address in the BIP155 form");
        assert_eq!(e.address.services, 1);
        assert_eq!(e.address.time, 1_417_713_337);
        // Core stamps a tried entry's last success when `Good` moves it.
        assert!(e.last_success == 0 || e.last_success == 1_417_713_337);
        assert_eq!(e.attempts, 0);
        assert!(e.address.addr.is_valid());
    }
    // The second port of one IPv4 address is a distinct entry (V4_MULTIPORT).
    let multi: Vec<_> =
        p.new.iter().filter(|e| e.address.addr == NetAddr::Ipv4([21, 1, 1, 1])).collect();
    assert_eq!(
        multi.iter().map(|e| e.address.port).collect::<BTreeSet<_>>(),
        BTreeSet::from([9999, 19999])
    );
    assert!(p.new.iter().any(|e| matches!(e.address.addr, NetAddr::Onion(_))));
    assert!(p.new.iter().any(|e| matches!(e.address.addr, NetAddr::I2p(_))));
}

#[test]
fn peers_dat_empty_node() {
    let p = peers::parse(&file("empty", "peers.dat"), NET).unwrap();
    assert_eq!((p.format, p.lowest_compatible), (4, 4));
    assert!(p.new.is_empty() && p.tried.is_empty() && p.bucket_refs.is_empty());
    assert_eq!(p.new_bucket_count, 1024);
}

#[test]
fn peers_dat_other_networks_and_magic() {
    let bytes = file("populated", "peers.dat");
    assert!(matches!(
        peers::parse(&bytes, Network::Mainnet),
        Err(FormatError::WrongMagic { found, .. }) if found == [0xfc, 0xc1, 0xb7, 0xdc]
    ));
}

#[test]
fn peers_dat_format_versions() {
    let bytes = file("populated", "peers.dat");
    let with = |format: u8, compat: u8| {
        let mut b = bytes.clone();
        b[4] = format;
        b[5] = compat;
        rechecksum(b)
    };
    // Format 3 is the same layout as 4 (multiport is a bucketing difference).
    let v3 = peers::parse(&with(3, 32 + 3), NET).unwrap();
    assert_eq!((v3.format, v3.lowest_compatible, v3.new.len(), v3.tried.len()), (3, 3, 34, 10));
    // A newer format that declares itself readable by format 4 is read as format 4.
    let v5 = peers::parse(&with(5, 32 + 4), NET).unwrap();
    assert_eq!((v5.format, v5.lowest_compatible, v5.new.len()), (5, 4, 34));
    // A newer format that is not backward compatible is refused: Core moves the file aside.
    assert_eq!(
        peers::parse(&with(5, 32 + 5), NET),
        Err(FormatError::UnsupportedVersion {
            what: "peers.dat",
            version: 5
        })
    );
    // A compat byte below the 32 base never came from Core.
    assert!(matches!(peers::parse(&with(4, 31), NET), Err(FormatError::Corrupt { .. })));
}

/// Writes the pre-BIP155 layout (formats 0 to 2: v1 addresses, 16-byte sources) of a parsed
/// file. This is a model of Core's `Serialize` for the old formats, not a Core-produced vector:
/// Core v24 only writes format 4, and no older release is available to capture.
fn encode_v1(p: &peers::PeersDat, format: u8) -> Vec<u8> {
    fn addr16(a: &NetAddr) -> [u8; 16] {
        match a {
            NetAddr::Ipv4(v4) => {
                let mut o = [0u8; 16];
                o[10] = 0xff;
                o[11] = 0xff;
                o[12..].copy_from_slice(v4);
                o
            }
            NetAddr::Ipv6(v6) => *v6,
            other => panic!("{other:?} has no v1 form"),
        }
    }
    let mut out = vec![0xfc, 0xc1, 0xb7, 0xdc, format, 32 + format.min(2)];
    out.extend_from_slice(&p.key);
    out.extend_from_slice(&(p.new.len() as i32).to_le_bytes());
    out.extend_from_slice(&(p.tried.len() as i32).to_le_bytes());
    let buckets: i32 = if format >= 1 {
        1024 ^ (1 << 30)
    } else {
        1024
    };
    out.extend_from_slice(&buckets.to_le_bytes());
    for e in p.new.iter().chain(&p.tried) {
        out.extend_from_slice(&220_000u32.to_le_bytes());
        out.extend_from_slice(&e.address.time.to_le_bytes());
        out.extend_from_slice(&e.address.services.to_le_bytes());
        out.extend_from_slice(&addr16(&e.address.addr));
        out.extend_from_slice(&e.address.port.to_be_bytes());
        out.extend_from_slice(&addr16(&e.source));
        out.extend_from_slice(&e.last_success.to_le_bytes());
        out.extend_from_slice(&e.attempts.to_le_bytes());
    }
    for bucket in 0..1024u32 {
        let refs: Vec<_> = p.bucket_refs.iter().filter(|r| r.bucket == bucket).collect();
        out.extend_from_slice(&(refs.len() as i32).to_le_bytes());
        for r in refs {
            out.extend_from_slice(&(r.index as i32).to_le_bytes());
        }
    }
    if format >= 2 {
        out.extend_from_slice(&p.asmap_checksum);
    }
    let sum = sha256d::Hash::hash(&out).to_byte_array();
    out.extend_from_slice(&sum);
    out
}

#[test]
fn peers_dat_pre_bip155_layouts() {
    // Keep the IPv4 and IPv6 entries: the old layout cannot hold the others.
    let mut p = peers::parse(&file("populated", "peers.dat"), NET).unwrap();
    let keep = |e: &peers::AddrInfo| matches!(e.address.addr, NetAddr::Ipv4(_) | NetAddr::Ipv6(_));
    let old_new = std::mem::take(&mut p.new);
    let index: BTreeMap<usize, usize> = old_new
        .iter()
        .enumerate()
        .filter(|(_, e)| keep(e))
        .enumerate()
        .map(|(new_i, (old_i, _))| (old_i, new_i))
        .collect();
    p.new = old_new.into_iter().filter(keep).collect();
    p.bucket_refs = p
        .bucket_refs
        .iter()
        .filter_map(|r| {
            index.get(&(r.index as usize)).map(|&i| peers::BucketRef {
                bucket: r.bucket,
                index: i as u32,
            })
        })
        .collect();
    for format in [0u8, 1, 2] {
        let parsed = peers::parse(&encode_v1(&p, format), NET).unwrap();
        assert_eq!((parsed.format, parsed.lowest_compatible), (format, format.min(2)));
        let as_v1 = |list: &[peers::AddrInfo]| -> Vec<peers::AddrInfo> {
            list.iter()
                .map(|e| peers::AddrInfo {
                    address: Address {
                        v2: false,
                        ..e.address
                    },
                    ..*e
                })
                .collect()
        };
        assert_eq!(parsed.new, as_v1(&p.new), "format {format}");
        assert_eq!(parsed.tried, as_v1(&p.tried), "format {format}");
        assert_eq!(parsed.bucket_refs, p.bucket_refs, "format {format}");
        assert!(parsed.new.iter().all(|e| !e.address.v2));
        // A v1 file carries no asmap checksum before format 2.
        assert_eq!(parsed.asmap_checksum, p.asmap_checksum);
    }
    // The BIP155 disk word is refused in a stream that predates BIP155.
    let mut bad = encode_v1(&p, 2);
    bad[4 + 2 + 32 + 12..][..4].copy_from_slice(&(1u32 << 29).to_le_bytes());
    assert!(peers::parse(&rechecksum(bad), NET).is_err());
}

#[test]
fn peers_dat_count_checks() {
    let bytes = file("populated", "peers.dat");
    // nNew at offset 4 + 2 + 32, nTried after it.
    let with = |off: usize, v: i32| {
        let mut b = bytes.clone();
        b[off..off + 4].copy_from_slice(&v.to_le_bytes());
        rechecksum(b)
    };
    let (n_new, n_tried) = (4 + 2 + 32, 4 + 2 + 32 + 4);
    for bad in [-1, 65_537, i32::MAX, i32::MIN] {
        assert!(
            matches!(peers::parse(&with(n_new, bad), NET), Err(FormatError::Corrupt { .. })),
            "{bad}"
        );
    }
    for bad in [-1, 16_385, i32::MAX] {
        assert!(
            matches!(peers::parse(&with(n_tried, bad), NET), Err(FormatError::Corrupt { .. })),
            "{bad}"
        );
    }
    // A count the file cannot back is a truncation, not an allocation.
    assert!(peers::parse(&with(n_new, 65_536), NET).is_err());
}

#[test]
fn peers_dat_bucket_refs_out_of_range_are_ignored() {
    let p = peers::parse(&file("populated", "peers.dat"), NET).unwrap();
    // Build a file with one bucket holding an in-range index and two out-of-range ones.
    let mut b = file("populated", "peers.dat");
    b.truncate(b.len() - 32);
    // Find the bucket section: it follows the entries; rebuild it from the parse.
    let entries_end = {
        let mut sizeof_buckets = 0usize;
        for bucket in 0..1024u32 {
            sizeof_buckets += 4 + 4 * p.bucket_refs.iter().filter(|r| r.bucket == bucket).count();
        }
        b.len() - 32 - sizeof_buckets
    };
    b.truncate(entries_end);
    for bucket in 0..1024u32 {
        if bucket == 7 {
            b.extend_from_slice(&3i32.to_le_bytes());
            for idx in [0i32, 34, -1] {
                b.extend_from_slice(&idx.to_le_bytes());
            }
        } else {
            b.extend_from_slice(&0i32.to_le_bytes());
        }
    }
    b.extend_from_slice(&[0; 32]);
    b.extend_from_slice(&[0; 32]);
    let q = peers::parse(&rechecksum(b), NET).unwrap();
    assert_eq!(
        q.bucket_refs,
        vec![peers::BucketRef {
            bucket: 7,
            index: 0
        }]
    );
    assert_eq!(q.ignored_bucket_refs, 2);
}

// ---- anchors.dat ----

#[test]
fn anchors_dat_matches_getpeerinfo() {
    let a = anchors::parse(&file("populated", "anchors.dat"), NET).unwrap();
    assert_eq!(a.addresses.len(), 2);
    assert_eq!(a.trailing_bytes, 0);
    let peers_info = expected()["peerinfo"].clone();
    let want: BTreeSet<(String, u16)> = peers_info
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let (host, port) = p["addr"].as_str().unwrap().rsplit_once(':').unwrap();
            let net = if host.ends_with(".onion") {
                "onion"
            } else {
                "ipv4"
            };
            (format!("{:?}", addr_from_text(host, net)), port.parse().unwrap())
        })
        .collect();
    let got: BTreeSet<_> = a.addresses.iter().map(|x| (format!("{:?}", x.addr), x.port)).collect();
    assert_eq!(got, want);
    assert!(a.addresses.iter().all(|x| x.v2));
    assert!(a.addresses.iter().any(|x| matches!(x.addr, NetAddr::Onion(_)) && x.port == 8333));
}

#[test]
fn anchors_dat_empty_node() {
    let a = anchors::parse(&file("empty", "anchors.dat"), NET).unwrap();
    assert!(a.addresses.is_empty());
}

#[test]
fn anchors_dat_is_not_a_peers_dat_and_back() {
    let (a, p) = (file("populated", "anchors.dat"), file("populated", "peers.dat"));
    assert!(peers::parse(&a, NET).is_err());
    assert!(anchors::parse(&p, NET).is_err());
}

#[test]
fn anchors_dat_v1_entry_and_bad_word() {
    // One entry in the pre-BIP155 disk form is accepted (anchors are read with ADDRV2 allowed).
    let mut b = vec![0xfc, 0xc1, 0xb7, 0xdc, 1];
    b.extend_from_slice(&220_000u32.to_le_bytes());
    b.extend_from_slice(&7u32.to_le_bytes());
    b.extend_from_slice(&1u64.to_le_bytes());
    b.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 8, 8, 4, 4]);
    b.extend_from_slice(&9999u16.to_be_bytes());
    let sum = sha256d::Hash::hash(&b).to_byte_array();
    b.extend_from_slice(&sum);
    let a = anchors::parse(&b, NET).unwrap();
    assert_eq!(
        a.addresses,
        vec![Address {
            time: 7,
            services: 1,
            addr: NetAddr::Ipv4([8, 8, 4, 4]),
            port: 9999,
            v2: false
        }]
    );
    // Any other version word is refused.
    b[5..9].copy_from_slice(&(1u32 << 28).to_le_bytes());
    assert!(anchors::parse(&rechecksum(b), NET).is_err());
}

// ---- banlist.json ----

fn ban_json(entries: &[&str]) -> String {
    format!(r#"{{"banned_nets":[{}]}}"#, entries.join(","))
}

fn ban(address: &str, created: &str, until: &str) -> String {
    format!(
        r#"{{"version":1,"ban_created":{created},"banned_until":{until},"address":"{address}"}}"#
    )
}

#[test]
fn banlist_matches_listbanned() {
    let list = banlist::parse(&file("populated", "banlist.json")).unwrap();
    assert_eq!(list.dropped_unknown_version, 0);
    assert_eq!(list.dropped_invalid_subnet, 0);
    assert_eq!(list.replaced_duplicates, 0);
    let want = expected()["listbanned"].clone();
    let got: Vec<_> = list
        .entries
        .iter()
        .map(|e| (e.address.to_str().unwrap(), e.ban_created, e.banned_until))
        .collect();
    let mut want: Vec<_> = want
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            (
                b["address"].as_str().unwrap().to_owned(),
                b["ban_created"].as_i64().unwrap(),
                b["banned_until"].as_i64().unwrap(),
            )
        })
        .collect();
    // `listbanned` and the file both iterate Core's ordered ban map.
    let mut got_sorted = got.clone();
    got_sorted.sort();
    want.sort();
    let want_ref: Vec<_> = want.iter().map(|(a, c, u)| (a.as_str(), *c, *u)).collect();
    assert_eq!(got_sorted, want_ref);
    let by_text: BTreeMap<_, _> =
        list.entries.iter().map(|e| (e.address.to_str().unwrap(), e.subnet)).collect();
    assert_eq!(
        by_text["1.2.0.0/16"],
        Subnet::V4 {
            network: [1, 2, 0, 0],
            mask: [255, 255, 0, 0]
        }
    );
    assert_eq!(
        by_text["192.168.0.6/32"],
        Subnet::V4 {
            network: [192, 168, 0, 6],
            mask: [255; 4]
        }
    );
    let Subnet::V6 {
        network,
        mask,
    } = by_text["2a01:4f8:1c0c:8212::/64"]
    else {
        panic!("ipv6 ban")
    };
    assert_eq!(network, "2a01:4f8:1c0c:8212::".parse::<Ipv6Addr>().unwrap().octets());
    assert_eq!(mask[..8], [255; 8]);
    assert_eq!(mask[8..], [0; 8]);
    let onion = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion";
    let key = base32(&onion[..onion.len() - ".onion".len()]);
    assert_eq!(by_text[onion], Subnet::Onion(key[..32].try_into().unwrap()));
}

#[test]
fn banlist_empty_and_rules() {
    assert!(banlist::parse(&file("empty", "banlist.json")).unwrap().entries.is_empty());
    let p = |s: &str| banlist::parse(s.as_bytes());
    let entry = |v: i64| {
        format!(
            r#"{{"banned_nets":[{{"version":{v},"ban_created":1,"banned_until":2,"address":"1.2.3.4/32"}}]}}"#
        )
    };
    assert_eq!(p(&entry(1)).unwrap().entries.len(), 1);
    // Another version is dropped, not an error; a dropped entry is only `{"version":2}` long.
    let dropped = p(&entry(2)).unwrap();
    assert_eq!((dropped.entries.len(), dropped.dropped_unknown_version), (0, 1));
    let tiny = p(r#"{"banned_nets":[{"version":2}]}"#).unwrap();
    assert_eq!((tiny.entries.len(), tiny.dropped_unknown_version), (0, 1));
    // A version outside int is an error in Core (getInt<int>).
    assert!(p(&entry(1 << 40)).is_err());
    // Missing key, wrong shape, missing or ill-typed field: `BanMapFromJson` throws.
    for bad in [
        "{}",
        r#"{"banned_nets":3}"#,
        r#"{"banned_nets":null}"#,
        r#"{"banned_nets":[3]}"#,
        r#"{"banned_nets":[{"ban_created":1,"banned_until":2,"address":"1.2.3.4"}]}"#,
        r#"{"banned_nets":[{"version":"1","ban_created":1,"banned_until":2,"address":"1.2.3.4"}]}"#,
        r#"{"banned_nets":[{"version":1,"ban_created":1,"address":"1.2.3.4"}]}"#,
        r#"{"banned_nets":[{"version":1,"ban_created":1.5,"banned_until":2,"address":"1.2.3.4"}]}"#,
        r#"{"banned_nets":[{"version":1,"ban_created":1,"banned_until":2,"address":4}]}"#,
        r#"{"banned_nets":[{"version":1,"ban_created":1,"banned_until":2}]}"#,
        "[]",
        "",
    ] {
        assert!(matches!(p(bad), Err(FormatError::Json { .. })), "{bad:?}");
    }
    // Timestamps are `getInt<int64_t>`: -0 is 0, an exponent or fraction or overflow throws.
    assert_eq!(
        p(&ban_json(&[&ban("1.2.3.4", "-0", "9223372036854775807")])).unwrap().entries[0]
            .ban_created,
        0
    );
    for bad_ts in ["1e3", "1.0", "9223372036854775808", "\"1\"", "null", "true"] {
        assert!(p(&ban_json(&[&ban("1.2.3.4", bad_ts, "2")])).is_err(), "{bad_ts}");
    }
    // A bad netmask is an invalid subnet, which Core drops (not an error).
    let odd = p(&ban_json(&[&ban("1.2.3.4/255.0.255.0", "1", "2")])).unwrap();
    assert_eq!((odd.entries.len(), odd.dropped_invalid_subnet), (0, 1));
}

/// Sol r1 F3: an entry with an invalid address is dropped before its timestamps are read, so it
/// cannot fail the file or hide the valid bans around it.
#[test]
fn banlist_invalid_address_is_dropped_before_its_timestamps_are_read() {
    let bad_ts = r#"{"version":1,"address":"invalid","ban_created":"x","banned_until":null}"#;
    let list =
        banlist::parse(ban_json(&[bad_ts, &ban("1.2.3.4/32", "1", "2")]).as_bytes()).unwrap();
    assert_eq!(list.dropped_invalid_subnet, 1);
    assert_eq!(list.entries.len(), 1);
    assert_eq!(list.entries[0].address.to_str(), Some("1.2.3.4/32"));
    // The same bad timestamps on a valid address fail the file, as in Core.
    let bad_valid = r#"{"version":1,"address":"5.6.7.8","ban_created":"x","banned_until":null}"#;
    assert!(
        banlist::parse(ban_json(&[bad_valid, &ban("1.2.3.4/32", "1", "2")]).as_bytes()).is_err()
    );
    // An unknown version is dropped before even the address is read.
    let v2 = r#"{"version":2,"address":7}"#;
    assert!(banlist::parse(ban_json(&[v2]).as_bytes()).unwrap().entries.is_empty());
    // A missing address is an error, an unreadable one is dropped.
    assert!(banlist::parse(ban_json(&[r#"{"version":1}"#]).as_bytes()).is_err());
}

/// Sol r1 F2: a repeated field inside an entry reads as its first occurrence (`findKey`), and an
/// object-form `banned_nets` is walked in file order, the later entry for a subnet replacing the
/// earlier (`insert_or_assign`).
#[test]
fn banlist_duplicate_fields_and_object_order() {
    let dup = r#"{"version":1,"address":"1.2.3.4","ban_created":1,"banned_until":4102444800,"banned_until":2,"version":2}"#;
    let list = banlist::parse(ban_json(&[dup]).as_bytes()).unwrap();
    assert_eq!(list.entries[0].banned_until, 4_102_444_800);
    assert_eq!(list.dropped_unknown_version, 0);

    let z = ban("1.2.3.4", "1", "9");
    let a = ban("1.2.3.4", "1", "2");
    let object = format!(r#"{{"banned_nets":{{"z":{z},"a":{a}}}}}"#);
    let list = banlist::parse(object.as_bytes()).unwrap();
    assert_eq!(list.entries.len(), 1);
    assert_eq!(list.entries[0].banned_until, 2, "file order: z first, a last");
    assert_eq!(list.replaced_duplicates, 1);
    // Duplicate object keys are kept (UniValue keeps every member) and walked in order.
    let object = format!(r#"{{"banned_nets":{{"k":{z},"k":{a}}}}}"#);
    assert_eq!(banlist::parse(object.as_bytes()).unwrap().entries[0].banned_until, 2);
    // The same in an array, and a different mask is a different subnet.
    let list =
        banlist::parse(ban_json(&[&z, &a, &ban("1.2.3.4/24", "1", "5")]).as_bytes()).unwrap();
    assert_eq!(list.entries.len(), 2);
    // In `banmap_t` order the shorter mask (/24) comes first.
    assert_eq!(list.entries[0].banned_until, 5);
    assert_eq!(list.entries[1].banned_until, 2);
    // Top-level duplicates are refused, as `ReadSettings` does.
    assert!(banlist::parse(br#"{"banned_nets":[],"banned_nets":[]}"#).is_err());
}

/// The address classes `LookupSubNet` takes, and the ones it refuses.
#[test]
fn banlist_subnet_classes() {
    let one = |addr: &str| banlist::parse(ban_json(&[&ban(addr, "1", "2")]).as_bytes()).unwrap();
    for (addr, valid) in [
        ("::ffff:1.2.3.4", true),
        ("::ffff:1.2.3.4/32", true),
        ("::ffff:1.2.3.4/120", false),
        ("fd87:d87e:eb43::1", true),
        ("fd6b:88c0:8724::1", false),
        ("fc00::1", true),
        ("2a01:4f8::/32", true),
        ("2001:db8::/32", true),
        ("1.2.3.4/33", false),
        ("1.2.3.4/-1", false),
        ("1.2.3.4/", false),
        ("[1.2.3.4]", true),
        ("0x7f.1", true),
        // glibc `getaddrinfo`: a numeric scope id is accepted on any IPv6 address, digits only and
        // within 32 bits; an IPv4 address takes none.
        ("2001:db8::1%5", true),
        ("fe80::1%1", true),
        ("::ffff:1.2.3.4%3", true),
        ("[fe80::1%1]", true),
        ("fe80::1%0", true),
        ("fe80::1%4294967295", true),
        ("fe80::1%4294967296", false),
        ("fe80::1%", false),
        ("fe80::1%+1", false),
        ("fe80::1%1x", false),
        ("1.2.3.4%1", false),
        ("::1%lo", false),
        ("2001:db8::1%lo", false),
        ("fe80::1%no-such-interface0", false),
        // glibc `inet_aton`: a `0x` prefix needs a hex digit.
        ("0x1", true),
        ("0x1.0x2", true),
        ("0x", false),
        ("0X", false),
        ("0x.1", false),
        ("1.0x", false),
        ("example.com", false),
        ("", false),
    ] {
        let list = one(addr);
        assert_eq!(list.entries.len(), usize::from(valid), "{addr:?}");
        assert_eq!(list.dropped_invalid_subnet, u64::from(!valid), "{addr:?}");
    }
    // IPv4-in-IPv6 is the IPv4 subnet.
    assert_eq!(
        one("::ffff:1.2.3.4").entries[0].subnet,
        Subnet::V4 {
            network: [1, 2, 3, 4],
            mask: [255; 4]
        }
    );
    // `-cjdnsreachable` reads fc00::/8 as CJDNS (single hosts only).
    let cjdns =
        banlist::parse_with(ban_json(&[&ban("fc00::1", "1", "2")]).as_bytes(), true).unwrap();
    assert!(matches!(cjdns.entries[0].subnet, Subnet::Cjdns(_)));
    let cjdns =
        banlist::parse_with(ban_json(&[&ban("fc00::1/8", "1", "2")]).as_bytes(), true).unwrap();
    assert_eq!(cjdns.dropped_invalid_subnet, 1);
    // Onion and I2P: a valid name is a subnet; a bad checksum or a mask is not.
    let onion = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion";
    let i2p = "ukeu3k5oycgaauneqgtnvselmt4yemvoilkln7jpvamvfx7dnkdq.b32.i2p";
    assert!(matches!(one(onion).entries[0].subnet, Subnet::Onion(_)));
    assert!(matches!(one(i2p).entries[0].subnet, Subnet::I2p(_)));
    assert_eq!(one(&format!("{onion}/24")).dropped_invalid_subnet, 1);
    let flipped = onion.replacen("pg6m", "pg6n", 1);
    assert_eq!(one(&flipped).dropped_invalid_subnet, 1);
}

/// An interface name is a scope only for a link-local or link-local-multicast address, and only
/// when the importing host has the interface (`lo` exists on every Linux host).
#[cfg(target_os = "linux")]
#[test]
fn banlist_scope_by_interface_name_is_a_host_input() {
    let one = |addr: &str| banlist::parse(ban_json(&[&ban(addr, "1", "2")]).as_bytes()).unwrap();
    for (addr, valid) in [
        ("fe80::1%lo", true),
        ("ff02::1%lo", true),
        ("ff01::1%lo", true),
        ("ff05::1%lo", false),
        ("fec0::1%lo", false),
        ("2001:db8::1%lo", false),
    ] {
        assert_eq!(one(addr).entries.len(), usize::from(valid), "{addr:?}");
    }
}

/// Sol r1 F3 for scoped addresses: an entry whose address is no subnet is dropped before its
/// timestamps are read, but a scoped address that glibc accepts is a subnet, so its bad
/// timestamps fail the file.
#[test]
fn banlist_scope_decides_whether_the_timestamps_are_read() {
    let bad = |addr: &str| ban_json(&[&ban(addr, r#""x""#, "2")]);
    assert!(banlist::parse(bad("fe80::1%4294967296").as_bytes()).is_ok(), "no subnet: dropped");
    assert!(banlist::parse(bad("fe80::1%1").as_bytes()).is_err(), "a subnet: timestamps read");
    assert!(banlist::parse(bad("0x").as_bytes()).is_ok(), "no subnet: dropped");
    assert!(banlist::parse(bad("0x1").as_bytes()).is_err(), "a subnet: timestamps read");
    // A scope id is not part of the subnet: the same host with and without it is one ban.
    let list = banlist::parse(
        ban_json(&[&ban("fe80::1%1", "1", "2"), &ban("fe80::1%2", "1", "3")]).as_bytes(),
    )
    .unwrap();
    assert_eq!(list.entries.len(), 1);
    assert_eq!(list.replaced_duplicates, 1);
    assert_eq!(list.entries[0].banned_until, 3);
    // Opus-high n2: `insert_or_assign` keeps the first key, which prints with the first entry's
    // scope id; the times are the later entry's.
    assert_eq!(list.entries[0].address.to_str(), Some("fe80::1%1"));
    let list = banlist::parse(
        ban_json(&[
            &ban("fe80::1%2", "1", "2"),
            &ban("fe80::1%3", "5", "6"),
            &ban("fe80::1", "7", "8"),
        ])
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(list.entries.len(), 1);
    assert_eq!(list.replaced_duplicates, 2);
    assert_eq!(list.entries[0].address.to_str(), Some("fe80::1%2"));
    assert_eq!((list.entries[0].ban_created, list.entries[0].banned_until), (7, 8));
}

/// `banmap_t` is a `std::map`: the entries come out in `CSubNet::operator<` order (class,
/// address bytes, netmask), whatever the file order.
#[test]
fn banlist_entries_are_in_banmap_order() {
    let onion = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion";
    let i2p = "ukeu3k5oycgaauneqgtnvselmt4yemvoilkln7jpvamvfx7dnkdq.b32.i2p";
    let json = ban_json(&[
        &ban(i2p, "1", "2"),
        &ban("2001:db8::1", "1", "2"),
        &ban(onion, "1", "2"),
        &ban("10.0.0.0/16", "1", "2"),
        &ban("9.0.0.0/8", "1", "2"),
        &ban("10.0.0.0/8", "1", "2"),
        &ban("1.2.3.4", "1", "2"),
    ]);
    let list = banlist::parse(json.as_bytes()).unwrap();
    let order: Vec<_> = list
        .entries
        .iter()
        .map(|e| match e.subnet {
            Subnet::V4 {
                network,
                mask,
            } => format!("v4 {network:?} {mask:?}"),
            Subnet::V6 {
                ..
            } => "v6".to_owned(),
            Subnet::Onion(_) => "onion".to_owned(),
            Subnet::I2p(_) => "i2p".to_owned(),
            Subnet::Cjdns(_) => "cjdns".to_owned(),
        })
        .collect();
    assert_eq!(
        order,
        [
            "v4 [1, 2, 3, 4] [255, 255, 255, 255]",
            "v4 [9, 0, 0, 0] [255, 0, 0, 0]",
            // Same network, the shorter mask sorts first (memcmp of the netmask).
            "v4 [10, 0, 0, 0] [255, 0, 0, 0]",
            "v4 [10, 0, 0, 0] [255, 255, 0, 0]",
            "v6",
            "onion",
            "i2p",
        ]
    );
}

// ---- settings.json ----

fn num(text: &str) -> JValue {
    JValue::Num(text.to_owned())
}

#[test]
fn settings_json() {
    let s = settings::parse(&file("populated", "settings.json")).unwrap();
    assert!(!s.empty_file);
    let Some(JValue::Arr(wallets)) = s.get("wallet") else {
        panic!("wallet array")
    };
    let names: Vec<_> =
        wallets.iter().map(|w| w.as_json_str().unwrap().to_str().unwrap()).collect();
    assert_eq!(names, ["im1a", "im1a2"]);
    let warning = s.get("_warning_").unwrap().as_json_str().unwrap();
    assert!(warning.to_str().unwrap().contains("automatically generated"));
    let e = settings::parse(&file("empty", "settings.json")).unwrap();
    assert_eq!(e.values.len(), 1);
    // No bytes at all: no settings.
    assert!(settings::parse(b"").unwrap().empty_file);
}

#[test]
fn settings_json_rejections() {
    for bad in [
        &b"   "[..],
        b"[]",
        b"3",
        b"{",
        b"{\"a\":1} x",
        b"{\"a\":1,\"a\":2}",
        b"{\"a\":\xff}",
        b"{\"a\":[}",
        b"{\"a\":1,}",
        b"{\"a\":[1,]}",
        b"{\"a\":01}",
        b"{\"a\":1.}",
        b"{\"a\":.5}",
        b"{\"a\":+1}",
        b"{\"a\":1e}",
        b"{\"a\":tru}",
        b"{\"a\":'x'}",
        b"{a:1}",
        b"{\"a\":\"\\x\"}",
        b"{\"a\":\"\\ud800\"}",
        b"{\"a\":\"\\udc00\"}",
        b"{\"a\":\"\x01\"}",
        b"\xef\xbb\xbf{}",
        b"{\"a\" 1}",
        b"{\"a\":1 \"b\":2}",
    ] {
        assert!(matches!(settings::parse(bad), Err(FormatError::Json { .. })), "{bad:?}");
    }
    assert!(settings::parse(b" \n\t\r{ \"a\" : null }\n").is_ok());
    // Only space, tab, CR and LF are whitespace.
    assert!(settings::parse(b"\x0c{}").is_err());
    assert!(settings::parse(b"{}\x0b").is_err());
}

/// UniValue's depth limit is 512 open containers.
#[test]
fn settings_json_depth_is_cores_512() {
    let nested = |arrays: usize| format!("{{\"a\":{}{}}}", "[".repeat(arrays), "]".repeat(arrays));
    assert!(settings::parse(nested(511).as_bytes()).is_ok(), "object + 511 arrays = 512");
    assert!(matches!(settings::parse(nested(512).as_bytes()), Err(FormatError::Json { .. })));
    // Unbounded nesting is refused rather than recursed into.
    assert!(matches!(settings::parse(nested(100_000).as_bytes()), Err(FormatError::Json { .. })));
}

/// Opus-high m3: UniValue keeps its work stack on the heap, so depth costs no thread stack. A
/// document 512 deep is read and one 513 deep is refused on a 64 KiB stack, in a debug build too
/// (an overflow would abort the whole test process), and the tree is dropped on that thread.
#[test]
fn json_depth_512_and_513_on_a_small_stack() {
    const STACK: usize = 64 * 1024;
    // Each document has `depth` containers open at its deepest point, the root object included.
    let arrays =
        |depth: usize| format!("{{\"a\":{}{}}}", "[".repeat(depth - 1), "]".repeat(depth - 1));
    let objects = |depth: usize| {
        format!("{}{}", "{\"k\":".repeat(depth - 1), "{}".to_string() + &"}".repeat(depth - 1))
    };
    let mixed = |depth: usize| {
        let mut s = String::from("{\"m\":");
        for i in 1..depth {
            s.push_str(if i % 2 == 0 {
                "{\"k\":"
            } else {
                "["
            });
        }
        s.push('0');
        for i in (1..depth).rev() {
            s.push_str(if i % 2 == 0 {
                "}"
            } else {
                "]"
            });
        }
        s.push('}');
        s
    };
    for (shape, build) in [
        ("arrays", &arrays as &(dyn Fn(usize) -> String + Sync)),
        ("objects", &objects),
        ("mixed", &mixed),
    ] {
        for (depth, accepted) in [(512usize, true), (513, false)] {
            let doc = build(depth);
            let outcome = std::thread::Builder::new()
                .stack_size(STACK)
                .spawn(move || {
                    let settings_ok = settings::parse(doc.as_bytes()).is_ok();
                    let banlist_ok = banlist::parse(doc.as_bytes()).is_ok();
                    let sniffed = sniff(doc.as_bytes()).is_ok();
                    (settings_ok, banlist_ok, sniffed)
                })
                .unwrap()
                .join()
                .unwrap();
            // A settings object is a settings object at any depth; a ban list needs its key.
            assert_eq!(outcome.0, accepted, "{shape} {depth}: settings");
            assert!(!outcome.1, "{shape} {depth}: no `banned_nets` key is an error either way");
            assert_eq!(outcome.2, accepted, "{shape} {depth}: sniff");
        }
    }
}

/// Sol r1 F2: only top-level duplicates are an error; nested ones are kept and the first wins.
#[test]
fn settings_json_nested_duplicate_keys() {
    let s = settings::parse(br#"{"a":{"x":1,"x":2},"b":[{"y":"first","y":"second"}]}"#).unwrap();
    let a = s.get("a").unwrap();
    assert_eq!(a.member("x"), &num("1"));
    assert_eq!(a.as_object().unwrap().entries().len(), 2, "both members are kept");
    assert_eq!(a.as_object().unwrap().entries()[1].1, num("2"));
    assert_eq!(
        s.get("b").unwrap().values().unwrap()[0].member("y").as_json_str().unwrap().to_str(),
        Some("first")
    );
    // A lookup on a non-object, or of a missing key, is null.
    assert_eq!(a.member("missing"), &JValue::Null);
    assert_eq!(num("1").member("x"), &JValue::Null);
    // Top-level order is file order.
    let s = settings::parse(br#"{"z":1,"a":2}"#).unwrap();
    let keys: Vec<_> = s.values.iter().map(|(k, _)| k.to_str().unwrap()).collect();
    assert_eq!(keys, ["z", "a"]);
}

/// Sol r1 F6: numbers stay as written; conversion happens where a setting asks for it.
#[test]
fn settings_json_numbers_are_kept_as_written() {
    let s = settings::parse(
        br#"{"huge":1e400,"big":18446744073709551617,"neg":-0,"frac":-12.50E+3,"ok":42,"min":-9223372036854775808}"#,
    )
    .unwrap();
    assert_eq!(s.get("huge"), Some(&num("1e400")));
    assert_eq!(s.get("big"), Some(&num("18446744073709551617")));
    assert_eq!(s.get("neg"), Some(&num("-0")));
    assert_eq!(s.get("frac"), Some(&num("-12.50E+3")));
    // getInt<int64_t>: whole text must be an integer in range.
    assert_eq!(s.get("ok").unwrap().get_i64(), Ok(42));
    assert_eq!(s.get("ok").unwrap().get_i32(), Ok(42));
    assert_eq!(s.get("neg").unwrap().get_i64(), Ok(0));
    assert_eq!(s.get("min").unwrap().get_i64(), Ok(i64::MIN));
    assert!(s.get("min").unwrap().get_i32().is_err());
    for text in ["huge", "big", "frac"] {
        assert!(s.get(text).unwrap().get_i64().is_err(), "{text}");
    }
    // A string is not a number.
    assert!(JValue::Null.get_i64().unwrap_err().contains("not of expected type num"));
}

/// The string filter of `univalue_utffilter.h`: surrogate pairs, overlong forms and code points
/// past U+10FFFF are read as Core reads them.
#[test]
fn settings_json_strings_follow_the_utf8_filter() {
    let value = |raw: &[u8]| {
        let mut doc = b"{\"k\":\"".to_vec();
        doc.extend_from_slice(raw);
        doc.extend_from_slice(b"\"}");
        settings::parse(&doc).map(|s| s.get("k").unwrap().as_json_str().unwrap().clone())
    };
    assert_eq!(value(b"a\\u00e9\\n\\/").unwrap().as_bytes(), "aé\n/".as_bytes());
    assert_eq!(value(b"\\ud83d\\ude00").unwrap().as_bytes(), "😀".as_bytes());
    assert_eq!(value("😀".as_bytes()).unwrap().as_bytes(), "😀".as_bytes());
    // \u0000 is a NUL byte in the string.
    assert_eq!(value(b"\\u0000").unwrap().as_bytes(), [0]);
    // An overlong two-byte NUL is accepted and stored in its shortest form.
    assert_eq!(value(b"\xc0\x80").unwrap().as_bytes(), [0]);
    assert_eq!(value(b"\xc1\xbf").unwrap().as_bytes(), [0x7f]);
    // A surrogate pair written as two UTF-8 encoded surrogates joins.
    assert_eq!(value(b"\xed\xa0\xbd\xed\xb8\x80").unwrap().as_bytes(), "😀".as_bytes());
    // Code points past U+10FFFF (up to 0x1FFFFF) pass and are not valid `str`.
    let past = value(b"\xf7\xbf\xbf\xbf").unwrap();
    assert_eq!(past.as_bytes(), [0xf7, 0xbf, 0xbf, 0xbf]);
    assert_eq!(past.to_str(), None);
    // Invalid sequences and lone surrogates are errors.
    for bad in [
        &b"\x80"[..],
        b"\xc3",
        b"\xc3\x28",
        b"\xf8\x88\x80\x80\x80",
        b"\xed\xa0\xbd",
        b"\xed\xb8\x80",
    ] {
        assert!(value(bad).is_err(), "{bad:?}");
    }
}

// ---- mempool.dat ----

/// A `mempool.dat` with `txs` ten-byte transactions `(time, fee delta)`, then `deltas` and
/// `unbroadcast` as given (`std::map` / `std::set` serialization, repeats included).
fn craft_mempool(
    txs: &[(i64, i64)],
    deltas: &[([u8; 32], i64)],
    unbroadcast: &[[u8; 32]],
) -> Vec<u8> {
    let mut b = 1u64.to_le_bytes().to_vec();
    b.extend_from_slice(&(txs.len() as u64).to_le_bytes());
    for (time, delta) in txs {
        // version 1, no inputs, no outputs, lock time 0
        b.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        b.extend_from_slice(&time.to_le_bytes());
        b.extend_from_slice(&delta.to_le_bytes());
    }
    b.push(deltas.len() as u8);
    for (txid, delta) in deltas {
        b.extend_from_slice(txid);
        b.extend_from_slice(&delta.to_le_bytes());
    }
    b.push(unbroadcast.len() as u8);
    for txid in unbroadcast {
        b.extend_from_slice(txid);
    }
    b
}

#[test]
fn mempool_dat_matches_rpc() {
    let bytes = file("populated", "mempool.dat");
    let m = mempool::parse(&bytes).unwrap();
    let exp = expected();
    let pool = exp["rawmempool"].as_object().unwrap();
    assert_eq!(m.txs.len(), pool.len());
    assert_eq!(m.trailing_bytes, 0);
    let mut unbroadcast_want = BTreeSet::new();
    for tx in &m.txs {
        let id = hash_to_hex(&tx.txid);
        let row = &pool[&id];
        assert_eq!(tx.time, row["time"].as_i64().unwrap());
        assert!(bytes.windows(tx.raw.len()).any(|w| w == tx.raw));
        let entry = &exp["mempool_entries"][&id];
        let sats = |s: &Value| (s.as_str().unwrap().parse::<f64>().unwrap() * 1e8).round() as i64;
        assert_eq!(tx.fee_delta, sats(&entry["fees"]["modified"]) - sats(&entry["fees"]["base"]));
        if row["unbroadcast"].as_bool().unwrap() {
            unbroadcast_want.insert(tx.txid);
        }
    }
    assert_eq!(m.txs.iter().map(|t| t.fee_delta).filter(|d| *d == 12_345).count(), 1);
    assert_eq!(m.unbroadcast, unbroadcast_want);
    // The two prioritisetransaction calls on transactions that are not in the pool.
    assert_eq!(m.deltas.len(), 2);
    assert_eq!(m.deltas[&hash_from_hex(&"ab".repeat(32)).unwrap()], -777);
    assert_eq!(m.deltas[&hash_from_hex(&"cd".repeat(32)).unwrap()], 1 << 40);
    // The staged load of an intact file is the strict parse.
    let loaded = mempool::load(&bytes).unwrap();
    assert_eq!(loaded.outcome, mempool::Outcome::Complete);
    assert_eq!(loaded.dump, m);
}

#[test]
fn mempool_dat_empty_and_versions() {
    let bytes = file("empty", "mempool.dat");
    let m = mempool::parse(&bytes).unwrap();
    assert!(m.txs.is_empty() && m.deltas.is_empty() && m.unbroadcast.is_empty());
    assert_eq!(mempool::version(&bytes), Ok(1));
    // Version 2 (the obfuscated format of later releases) and anything else is refused; Core
    // applies nothing then, so `load` is an error too.
    for v in [0u64, 2, 3, u64::MAX] {
        let mut b = bytes.clone();
        b[..8].copy_from_slice(&v.to_le_bytes());
        let want = FormatError::UnsupportedVersion {
            what: "mempool.dat",
            version: v,
        };
        assert_eq!(mempool::parse(&b), Err(want.clone()));
        assert_eq!(mempool::load(&b), Err(want));
    }
    // A count the file cannot back does not allocate; the first missing record stops the load.
    let mut b = bytes.clone();
    b[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(
        mempool::parse(&b),
        Err(FormatError::Truncated { .. } | FormatError::Corrupt { .. })
    ));
    let loaded = mempool::load(&b).unwrap();
    assert!(loaded.dump.txs.is_empty());
    assert!(matches!(
        loaded.outcome,
        mempool::Outcome::Stopped {
            phase: mempool::Phase::Transactions,
            ..
        }
    ));
    // No count at all: Core's `file >> total_txns_to_load` throws before applying anything.
    assert!(mempool::load(&bytes[..12]).is_err());
}

/// Sol r1 F4: what `LoadMempool` has applied when a later structure is damaged.
#[test]
fn mempool_dat_retained_prefix_before_damage() {
    let bytes = file("populated", "mempool.dat");
    let full = mempool::parse(&bytes).unwrap();
    assert_eq!(full.txs.len(), 4);
    let offset = |raw: &[u8]| raw.as_ptr() as usize - bytes.as_ptr() as usize;
    // The end of record `i`: its transaction, then the time and the fee delta.
    let end_of = |i: usize| offset(full.txs[i].raw) + full.txs[i].raw.len() + 16;

    // Cut inside the third record, at every byte: the first two records are Core's, and so are
    // the empty map and set (their effects come after the loop).
    for cut in end_of(1) + 1..end_of(2) {
        let loaded = mempool::load(&bytes[..cut]).unwrap();
        assert_eq!(loaded.dump.txs, full.txs[..2], "cut {cut}");
        assert!(loaded.dump.deltas.is_empty() && loaded.dump.unbroadcast.is_empty());
        assert_eq!(loaded.dump.trailing_bytes, cut - end_of(1));
        let mempool::Outcome::Stopped {
            phase,
            error,
        } = loaded.outcome
        else {
            panic!("stopped at {cut}")
        };
        assert_eq!(phase, mempool::Phase::Transactions);
        assert_eq!(mempool::parse(&bytes[..cut]), Err(error), "the strict parse reports the same");
    }
    // A record whose transaction is complete but whose time is cut is not a complete record.
    let cut = end_of(1) + full.txs[2].raw.len() + 4;
    assert_eq!(mempool::load(&bytes[..cut]).unwrap().dump.txs.len(), 2);

    // Cut inside the map: all records are kept, the incomplete map is not applied.
    let map_start = end_of(3);
    for cut in map_start + 1..map_start + 1 + 40 * 2 {
        let loaded = mempool::load(&bytes[..cut]).unwrap();
        assert_eq!(loaded.dump.txs, full.txs, "cut {cut}");
        assert!(loaded.dump.deltas.is_empty() && loaded.dump.unbroadcast.is_empty(), "cut {cut}");
        assert!(matches!(
            loaded.outcome,
            mempool::Outcome::Stopped {
                phase: mempool::Phase::Deltas,
                ..
            }
        ));
    }
    // Cut inside the unbroadcast set: the complete map is applied, the incomplete set is not.
    let set_start = map_start + 1 + 40 * 2;
    for cut in set_start + 1..bytes.len() {
        let loaded = mempool::load(&bytes[..cut]).unwrap();
        assert_eq!(loaded.dump.txs, full.txs, "cut {cut}");
        assert_eq!(loaded.dump.deltas, full.deltas, "cut {cut}");
        assert!(loaded.dump.unbroadcast.is_empty());
        assert!(matches!(
            loaded.outcome,
            mempool::Outcome::Stopped {
                phase: mempool::Phase::Unbroadcast,
                ..
            }
        ));
    }
    // Garbage after a complete file is ignored and counted.
    let mut tail = bytes.clone();
    tail.extend_from_slice(b"junk");
    let loaded = mempool::load(&tail).unwrap();
    assert_eq!((loaded.outcome, loaded.dump.trailing_bytes), (mempool::Outcome::Complete, 4));
    // A corrupt record: the transaction cannot be decoded (an input count past MAX_SIZE).
    let mut corrupt = bytes[..end_of(1)].to_vec();
    corrupt.extend_from_slice(&[1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    corrupt.extend_from_slice(&bytes[end_of(1)..]);
    let loaded = mempool::load(&corrupt).unwrap();
    assert_eq!(loaded.dump.txs, full.txs[..2]);
    assert!(matches!(loaded.outcome, mempool::Outcome::Stopped { .. }));
}

/// Sol r1 F5: `mapDeltas` keeps the first value for a txid and the unbroadcast list is a set;
/// the transaction records are not deduplicated (each delta is applied before admission).
#[test]
fn mempool_dat_duplicates_follow_core_containers() {
    let (a, b, c) = ([0xaa; 32], [0xbb; 32], [0xcc; 32]);
    let bytes = craft_mempool(&[(5, 7), (5, 11)], &[(a, 7), (a, 11), (b, 5), (a, 13)], &[c, b, c]);
    let m = mempool::parse(&bytes).unwrap();
    assert_eq!(m.deltas.len(), 2);
    assert_eq!(m.deltas[&a], 7, "std::map::insert keeps the first value");
    assert_eq!(m.deltas[&b], 5);
    assert_eq!(m.unbroadcast, BTreeSet::from([b, c]));
    // Two records of the same (empty) transaction both stay, with their own deltas.
    assert_eq!(m.txs.len(), 2);
    assert_eq!(m.txs[0].txid, m.txs[1].txid);
    assert_eq!((m.txs[0].fee_delta, m.txs[1].fee_delta), (7, 11));
    assert_eq!(m.trailing_bytes, 0);
}

#[test]
fn mempool_dat_huge_counts_stay_small() {
    // count = u64::MAX with a first transaction announcing 2^25 outputs (MAX_SIZE): the reads fail
    // on the missing data and the reservation stays at the clamp.
    let mut b = 1u64.to_le_bytes().to_vec();
    b.extend_from_slice(&u64::MAX.to_le_bytes());
    b.extend_from_slice(&[1, 0, 0, 0]); // version
    b.push(0); // no inputs
    b.extend_from_slice(&[0xfe, 0, 0, 0, 2]); // 2^25 outputs
    b.resize(1 << 20, 0);
    let loaded = mempool::load(&b).unwrap();
    assert!(loaded.dump.txs.is_empty());
    assert!(matches!(loaded.outcome, mempool::Outcome::Stopped { .. }));
    // 1 MiB of file could back 40,000 minimal records; the clamp reserves 4,096 at most.
    assert_eq!(loaded.dump.txs.capacity(), 4096);
    // A small file backs few: 100 bytes hold at most three 26-byte records.
    let mut small = 1u64.to_le_bytes().to_vec();
    small.extend_from_slice(&u64::MAX.to_le_bytes());
    small.resize(100, 0);
    let loaded = mempool::load(&small).unwrap();
    assert!(matches!(loaded.outcome, mempool::Outcome::Stopped { .. }));
    assert_eq!(loaded.dump.txs.capacity(), (100 - 16) / 26);
}

// ---- fee_estimates.dat ----

/// R2-M1: `nan-decay/fee_estimates.dat` is what a real Core v24.0.0-rc.3 wrote after loading a file
/// whose three decay words were NaN (PROVENANCE.md): Core accepts a NaN decay and keeps it.
#[test]
fn fee_estimates_dat_nan_decay_written_by_core() {
    let bytes = file("nan-decay", "fee_estimates.dat");
    let f = fee_estimates::parse(&bytes).unwrap();
    assert!(f.medium.decay.is_nan() && f.short.decay.is_nan() && f.long.decay.is_nan());
    // The rest of the statistics are the ordinary captured file's.
    let ordinary = fee_estimates::parse(&file("populated", "fee_estimates.dat")).unwrap();
    assert_eq!(f.buckets, ordinary.buckets);
    assert_eq!(f.medium.conf_avg, ordinary.medium.conf_avg);
    assert_eq!(f.long.feerate_avg, ordinary.long.feerate_avg);
    let found = detect_named("fee_estimates.dat", &bytes).unwrap();
    assert_eq!(found.format, Format::FeeEstimatesDat);
    assert_eq!(sniff(&bytes).unwrap(), found);
}

#[test]
fn fee_estimates_dat_matches_estimaterawfee() {
    let f = fee_estimates::parse(&file("populated", "fee_estimates.dat")).unwrap();
    let exp = expected();
    assert_eq!((f.version_required, f.version_that_wrote), (140_100, 240_000));
    assert_eq!(f.best_seen_height as u64, exp["blockcount"].as_u64().unwrap());
    assert!(f.historical_first <= f.historical_best && f.historical_best <= f.best_seen_height);
    assert_eq!(f.trailing_bytes, 0);
    for (name, stats, periods) in
        [("medium", &f.medium, 24), ("short", &f.short, 12), ("long", &f.long, 42)]
    {
        let rpc = &exp["estimaterawfee"]["6"][name];
        let decay: f64 = rpc["decay"].as_str().unwrap().parse().unwrap();
        assert_eq!(stats.decay, decay, "{name}");
        assert_eq!(u64::from(stats.scale), rpc["scale"].as_u64().unwrap(), "{name}");
        assert_eq!(stats.conf_avg.len(), periods, "{name}");
        assert_eq!(stats.fail_avg.len(), periods, "{name}");
        assert!(
            stats.conf_avg.iter().chain(&stats.fail_avg).all(|row| row.len() == f.buckets.len())
        );
        assert_eq!(stats.feerate_avg.len(), f.buckets.len());
        assert_eq!(stats.tx_ct_avg.len(), f.buckets.len());
    }
    // The bucket boundaries are Core's: 1000 x 1.05^k up to 1e7, then infinity.
    assert_eq!(f.buckets[0], 1000.0);
    assert!(f.buckets.windows(2).all(|w| w[0] < w[1]));
    assert!(f.buckets.last().unwrap().is_infinite() || *f.buckets.last().unwrap() > 1e7);
    // `estimaterawfee` prints the bucket range its pass/fail is in: the range ends are buckets.
    let end = exp["estimaterawfee"]["6"]["medium"]["pass"]["endrange"].as_f64().unwrap();
    assert!(f.buckets.iter().any(|b| (*b - end).abs() < 0.5), "{end}");
    // The data is not all zero (fee history was recorded).
    assert!(f.medium.tx_ct_avg.iter().any(|x| *x > 0.0));
}

#[test]
fn fee_estimates_dat_empty_node() {
    let f = fee_estimates::parse(&file("empty", "fee_estimates.dat")).unwrap();
    assert_eq!((f.best_seen_height, f.historical_first, f.historical_best), (0, 0, 0));
    assert!(f.medium.tx_ct_avg.iter().all(|x| *x == 0.0));
}

#[test]
fn fee_estimates_dat_rules() {
    let good = file("populated", "fee_estimates.dat");
    let patch = |off: usize, bytes: &[u8]| {
        let mut b = good.clone();
        b[off..off + bytes.len()].copy_from_slice(bytes);
        b
    };
    // Version words: required newer than v24's or older than 0.14.1.
    for v in [240_001i32, i32::MAX, 140_099, 0, -1] {
        assert!(matches!(
            fee_estimates::parse(&patch(0, &v.to_le_bytes())),
            Err(FormatError::UnsupportedVersion { .. })
        ));
    }
    // historical_first > historical_best; best_seen < historical_best.
    assert!(matches!(
        fee_estimates::parse(&patch(12, &u32::MAX.to_le_bytes())),
        Err(FormatError::Corrupt { .. })
    ));
    assert!(matches!(
        fee_estimates::parse(&patch(8, &1u32.to_le_bytes())),
        Err(FormatError::Corrupt { .. })
    ));
    // Bucket count outside 2..=1000 (offset 20, one-byte compact size).
    for n in [0u8, 1] {
        assert!(matches!(fee_estimates::parse(&patch(20, &[n])), Err(FormatError::Corrupt { .. })));
    }
    // A bucket count that disagrees with the stats' vectors.
    assert!(matches!(
        fee_estimates::parse(&patch(20, &[0xfd, 0xe8, 0x03])),
        Err(FormatError::Corrupt { .. } | FormatError::Truncated { .. })
    ));
    // Decay of the first stats block: out of range is refused as `TxConfirmStats::Read` does
    // (`decay <= 0 || decay >= 1`), and a NaN passes there (both comparisons are false), so it
    // does here.
    let n_buckets = fee_estimates::parse(&good).unwrap().buckets.len();
    let decay_off = 20 + 1 + 8 * n_buckets;
    let nan = fee_estimates::parse(&patch(decay_off, &f64::NAN.to_bits().to_le_bytes())).unwrap();
    assert!(nan.medium.decay.is_nan());
    for d in [0.0f64, 1.0, -0.5, 2.0, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(
            matches!(
                fee_estimates::parse(&patch(decay_off, &d.to_bits().to_le_bytes())),
                Err(FormatError::Corrupt { .. })
            ),
            "{d}"
        );
    }
    // Scale zero, and a scale x periods beyond one week of blocks.
    assert!(matches!(
        fee_estimates::parse(&patch(decay_off + 8, &0u32.to_le_bytes())),
        Err(FormatError::Corrupt { .. })
    ));
    assert!(matches!(
        fee_estimates::parse(&patch(decay_off + 8, &1000u32.to_le_bytes())),
        Err(FormatError::Corrupt { .. })
    ));
}

// ---- detection ----

#[test]
fn detection_of_every_format_and_version() {
    for which in ["empty", "populated"] {
        let first_addrv2 = if which == "empty" {
            None
        } else {
            Some(true)
        };
        let cases = [
            (
                "peers.dat",
                Format::PeersDat,
                Some(NET),
                Version::Peers {
                    format: 4,
                    lowest_compatible: 4,
                },
            ),
            (
                "anchors.dat",
                Format::AnchorsDat,
                Some(NET),
                Version::Anchors {
                    first_addrv2,
                },
            ),
            ("banlist.json", Format::BanlistJson, None, Version::Json),
            (
                "mempool.dat",
                Format::MempoolDat,
                None,
                Version::Mempool {
                    version: 1,
                },
            ),
            (
                "fee_estimates.dat",
                Format::FeeEstimatesDat,
                None,
                Version::FeeEstimates {
                    required: 140_100,
                    wrote: 240_000,
                },
            ),
            ("settings.json", Format::SettingsJson, None, Version::Json),
        ];
        for (name, format, network, version) in cases {
            let want = Detected {
                format,
                network,
                version,
            };
            let bytes = file(which, name);
            assert_eq!(detect_named(name, &bytes), Ok(want), "{which}/{name}");
            assert_eq!(sniff(&bytes), Ok(want), "sniff {which}/{name}");
            assert_eq!(Format::from_file_name(name), Some(format));
            assert_eq!(format.file_name(), name);
        }
    }
    // The ban list is told from settings by its key alone.
    assert_eq!(sniff(b"{\"banned_nets\":[]}").unwrap().format, Format::BanlistJson);
    assert_eq!(sniff(b"{\"x\":[]}").unwrap().format, Format::SettingsJson);
}

#[test]
fn detection_refusals() {
    assert_eq!(detect_named("wallet.dat", b"x"), Err(FormatError::Unrecognized));
    assert_eq!(sniff(b""), Err(FormatError::Unrecognized));
    assert_eq!(sniff(&[0u8; 100]), Err(FormatError::Unrecognized));
    let mut peers = file("populated", "peers.dat");
    // Mainnet's magic on a regtest file is a known magic, and the file then fails to parse as mainnet.
    peers[..4].copy_from_slice(&Network::Mainnet.magic().to_le_bytes());
    assert_eq!(detect_named("peers.dat", &peers).unwrap().network, Some(Network::Mainnet));
    peers[..4].copy_from_slice(&[1, 2, 3, 4]);
    assert!(matches!(detect_named("peers.dat", &peers), Err(FormatError::WrongMagic { .. })));
    // A mempool v2 is detected by name (its version is reported) and refused by parse.
    let mut m = file("empty", "mempool.dat");
    m[..8].copy_from_slice(&2u64.to_le_bytes());
    assert_eq!(
        detect_named("mempool.dat", &m).unwrap().version,
        Version::Mempool {
            version: 2
        }
    );
}

// ---- truncation, corruption, size ----

/// Files up to this size are cut at **every** byte; larger ones at a stride of `len / 400` plus
/// the last byte (cutting every byte of a 300 KiB file would parse 300 KiB squared bytes).
const EXHAUSTIVE_PREFIX_LIMIT: usize = 64 << 10;

/// Every strict prefix of a file with a trailing checksum or a required final field is an error.
fn assert_every_prefix_fails(name: &str, bytes: &[u8], parse: &dyn Fn(&[u8]) -> bool) {
    let step = if bytes.len() <= EXHAUSTIVE_PREFIX_LIMIT {
        1
    } else {
        (bytes.len() / 400).max(1)
    };
    let mut cut = 0;
    while cut < bytes.len() {
        assert!(!parse(&bytes[..cut]), "{name}: prefix of {cut}/{} parsed", bytes.len());
        cut += step;
    }
    assert!(!parse(&bytes[..bytes.len() - 1]), "{name}: one byte short parsed");
}

#[test]
fn truncation() {
    for which in ["empty", "populated"] {
        assert_every_prefix_fails("peers", &file(which, "peers.dat"), &|b| {
            peers::parse(b, NET).is_ok()
        });
        assert_every_prefix_fails("anchors", &file(which, "anchors.dat"), &|b| {
            anchors::parse(b, NET).is_ok()
        });
        assert_every_prefix_fails("mempool", &file(which, "mempool.dat"), &|b| {
            mempool::parse(b).is_ok()
        });
        assert_every_prefix_fails("fees", &file(which, "fee_estimates.dat"), &|b| {
            fee_estimates::parse(b).is_ok()
        });
        // JSON: cut inside the document at every byte (the final newline is whitespace).
        for name in ["settings.json", "banlist.json"] {
            let bytes = file(which, name);
            let end = bytes.trim_ascii_end().len();
            for cut in 1..end {
                let part = &bytes[..cut];
                assert!(settings::parse(part).is_err(), "{name} prefix {cut}");
                assert!(banlist::parse(part).is_err(), "{name} prefix {cut}");
            }
        }
    }
}

#[test]
fn corruption_never_panics_and_is_caught_where_a_checksum_exists() {
    // The checksummed files: bits 0, 3 and 7 of every byte are flipped, one at a time. A flip is
    // an error (the data or the checksum no longer match, or the magic changed). The other five
    // bit positions are covered for `anchors.dat`, which is small.
    for (name, bytes, parse, bits) in [
        (
            "peers",
            file("populated", "peers.dat"),
            (|b: &[u8]| peers::parse(b, NET).is_ok()) as fn(&[u8]) -> bool,
            &[0u32, 3, 7][..],
        ),
        (
            "anchors",
            file("populated", "anchors.dat"),
            |b| anchors::parse(b, NET).is_ok(),
            &[0, 1, 2, 3, 4, 5, 6, 7][..],
        ),
    ] {
        for i in 0..bytes.len() {
            for &bit in bits {
                let mut b = bytes.clone();
                b[i] ^= 1 << bit;
                assert!(!parse(&b), "{name}: flip of byte {i} bit {bit} parsed");
            }
        }
    }
    // The files without a checksum may still parse after a flip (bits 1 and 6 of every byte);
    // they must not panic.
    for (bytes, parse) in [
        (
            file("populated", "mempool.dat"),
            (|b: &[u8]| mempool::parse(b).is_ok()) as fn(&[u8]) -> bool,
        ),
        (file("populated", "banlist.json"), |b| banlist::parse(b).is_ok()),
        (file("populated", "settings.json"), |b| settings::parse(b).is_ok()),
    ] {
        for i in 0..bytes.len() {
            for bit in [1, 6] {
                let mut b = bytes.clone();
                b[i] ^= 1 << bit;
                let _ = parse(&b);
            }
        }
    }
    // fee_estimates.dat is large: one flip (bit `i % 8`) at a stride of 1009 bytes and in each of
    // the first 400 bytes (the headers).
    let fees = file("populated", "fee_estimates.dat");
    for i in (0..fees.len()).step_by(1009).chain(0..400) {
        let mut b = fees.clone();
        b[i] ^= 1 << (i % 8);
        let _ = fee_estimates::parse(&b);
    }
}

#[test]
fn oversize_inputs_are_refused_before_parsing() {
    let big = |n: usize| vec![0u8; n];
    let too_large = |r: Result<(), FormatError>| matches!(r, Err(FormatError::TooLarge { .. }));
    assert!(too_large(peers::parse(&big(peers::MAX_PEERS_DAT_SIZE + 1), NET).map(drop)));
    assert!(too_large(anchors::parse(&big(anchors::MAX_ANCHORS_DAT_SIZE + 1), NET).map(drop)));
    assert!(too_large(
        fee_estimates::parse(&big(fee_estimates::MAX_FEE_ESTIMATES_SIZE + 1)).map(drop)
    ));
    assert!(too_large(
        settings::parse(&big(dash_coreimport::netstate::MAX_JSON_FILE_SIZE + 1)).map(drop)
    ));
    assert!(too_large(
        banlist::parse(&big(dash_coreimport::netstate::MAX_JSON_FILE_SIZE + 1)).map(drop)
    ));
}

#[test]
fn read_file_capped_stops_at_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.dat");
    let f = std::fs::File::create(&path).unwrap();
    f.set_len(10_000).unwrap();
    drop(f);
    assert_eq!(read_file_capped(&path, 10_000, "peers.dat").unwrap().len(), 10_000);
    assert!(matches!(
        read_file_capped(&path, 9_999, "peers.dat"),
        Err(dash_coreimport::netstate::LoadError::Format(FormatError::TooLarge { .. }))
    ));
    assert!(matches!(
        read_file_capped(&dir.path().join("missing"), 10, "x"),
        Err(dash_coreimport::netstate::LoadError::Io { .. })
    ));
}

#[test]
fn crafted_counts_do_not_allocate_or_loop() {
    // Anchors: a huge count with nothing behind it.
    let mut b = vec![0xfc, 0xc1, 0xb7, 0xdc, 0xfe, 0xff, 0xff, 0xff, 0x01];
    b.extend_from_slice(&[0; 32]);
    assert!(anchors::parse(&b, NET).is_err());
    // A non-canonical compact size and one over MAX_SIZE.
    let mut b = vec![0xfc, 0xc1, 0xb7, 0xdc, 0xfd, 0x01, 0x00];
    b.extend_from_slice(&[0; 32]);
    assert!(matches!(anchors::parse(&b, NET), Err(FormatError::Corrupt { .. })));
    let mut b = vec![0xfc, 0xc1, 0xb7, 0xdc, 0xfe, 0x01, 0x00, 0x00, 0x02];
    b.extend_from_slice(&[0; 32]);
    assert!(matches!(anchors::parse(&b, NET), Err(FormatError::Corrupt { .. })));
    // peers.dat with 2^31-1 buckets and no bucket data.
    let p = file("empty", "peers.dat");
    let nb_off = 4 + 2 + 32 + 8;
    let mut b = p.clone();
    b[nb_off..nb_off + 4].copy_from_slice(&(i32::MAX ^ (1 << 30)).to_le_bytes());
    assert!(matches!(peers::parse(&rechecksum(b), NET), Err(FormatError::Truncated { .. })));
    // A bucket claiming 2^31-1 entries.
    let mut b = p.clone();
    b.truncate(nb_off + 4);
    b.extend_from_slice(&i32::MAX.to_le_bytes());
    b.extend_from_slice(&[0; 32]);
    b.extend_from_slice(&[0; 32]);
    assert!(matches!(peers::parse(&rechecksum(b), NET), Err(FormatError::Truncated { .. })));
}

/// The filesystem checks work on the opened descriptor (Sol r1 F1, DEC-137).
#[cfg(unix)]
mod file_access {
    use std::os::unix::fs::symlink;
    use std::path::Path;
    use std::process::Command;
    use std::sync::mpsc;
    use std::time::Duration;

    use dash_coreimport::netstate::{FormatError, LoadError, read_file_capped};

    /// Runs `read_file_capped` on another thread and fails the test if it does not return: a
    /// blocking open of a FIFO would hang here.
    fn read_within_deadline(path: &Path, limit: usize) -> Result<Vec<u8>, LoadError> {
        let (tx, rx) = mpsc::channel();
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(read_file_capped(&path, limit, "peers.dat"));
        });
        rx.recv_timeout(Duration::from_secs(20)).expect("read_file_capped blocked")
    }

    fn mkfifo(path: &Path) {
        assert!(Command::new("mkfifo").arg(path).status().unwrap().success());
    }

    fn is_not_regular(r: Result<Vec<u8>, LoadError>) -> bool {
        matches!(&r, Err(LoadError::Io { source, .. }) if source.kind() == std::io::ErrorKind::InvalidInput)
    }

    #[test]
    fn a_fifo_in_the_files_place_is_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.dat");
        // The file as it was: a regular file.
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(read_within_deadline(&path, 10).unwrap(), b"abc");
        // The file swapped for a FIFO (what an attacker with write access to the directory does
        // between any look at the path and the open): no writer, so a blocking open would hang.
        std::fs::remove_file(&path).unwrap();
        mkfifo(&path);
        assert!(is_not_regular(read_within_deadline(&path, 10)));
        // And through a symlink to a FIFO.
        let link = dir.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(is_not_regular(read_within_deadline(&link, 10)));
        // With a writer present the open also returns at once and the type is still refused.
        let _writer = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
        assert!(is_not_regular(read_within_deadline(&path, 10)));
    }

    #[test]
    fn devices_directories_and_dangling_links_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        // A character device that never ends is refused, not read up to the limit.
        assert!(is_not_regular(read_within_deadline(Path::new("/dev/zero"), usize::MAX)));
        assert!(is_not_regular(read_within_deadline(dir.path(), 10)));
        let dangling = dir.path().join("dangling");
        symlink(dir.path().join("missing"), &dangling).unwrap();
        assert!(matches!(read_within_deadline(&dangling, 10), Err(LoadError::Io { .. })));
    }

    #[test]
    fn a_symlink_to_a_regular_file_is_followed_as_fopen_does() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"abc").unwrap();
        let link = dir.path().join("link");
        symlink(&target, &link).unwrap();
        assert_eq!(read_within_deadline(&link, 10).unwrap(), b"abc");
    }

    #[test]
    fn the_largest_limit_neither_panics_nor_wraps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(read_within_deadline(&path, usize::MAX).unwrap(), b"abc");
        assert!(matches!(
            read_within_deadline(&path, 2),
            Err(LoadError::Format(FormatError::TooLarge { .. }))
        ));
    }
}

/// Sol r1 F7: JSON detection is a structural hint; the named reader has the final say.
#[test]
fn json_detection_is_structural() {
    use dash_coreimport::netstate::MAX_SETTINGS_FILE_SIZE;

    assert_eq!(sniff(br#"{"banned_nets":42}"#).unwrap().format, Format::BanlistJson);
    assert!(banlist::parse(br#"{"banned_nets":42}"#).is_err(), "the reader refuses it");
    // A settings object may hold that key; the hint calls it a ban list.
    let both = br#"{"banned_nets":[],"wallet":["a"]}"#;
    assert_eq!(sniff(both).unwrap().format, Format::BanlistJson);
    assert!(settings::parse(both).is_ok());
    // Top-level duplicates are no settings file at all.
    assert_eq!(sniff(br#"{"a":1,"a":2}"#), Err(FormatError::Unrecognized));

    // settings.json has its own, smaller cap; an empty file is no settings, also by name.
    let mut big = b"{\"a\":\"".to_vec();
    big.resize(MAX_SETTINGS_FILE_SIZE, b'x');
    big.extend_from_slice(b"\"}");
    assert!(matches!(settings::parse(&big), Err(FormatError::TooLarge { .. })));
    assert!(matches!(sniff(&big), Err(FormatError::Unrecognized)));
    assert_eq!(detect_named("settings.json", b"").unwrap().version, Version::Json);
    assert!(detect_named("banlist.json", b"").is_err());

    // A binary file that starts with whitespace and `{` is not JSON: a fee-estimates version word
    // of 162592 is the bytes `20 7B 02 00`.
    let mut fee = file("populated", "fee_estimates.dat");
    fee[..4].copy_from_slice(&162_592u32.to_le_bytes());
    assert_eq!(&fee[..2], b" {");
    assert_eq!(sniff(&fee).unwrap().format, Format::FeeEstimatesDat);
}
