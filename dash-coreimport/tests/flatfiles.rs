//! IM1b: the flat files Dash Core v24 wrote (see
//! `tests/data/flatfiles/v24.0.0-rc.{3,4}/PROVENANCE.md`) parsed by the readers in
//! `netstate::{governance, mncache, netfulfilled, sporks}`, and compared with what the same node
//! answered over RPC (`populated/expected.json`). rc.3 writes `governance.dat` as `-16`, rc.4 as
//! `-17` (BL-420); the readers load both.
//! The key and `dash.conf` files of the same captures are in `keyfiles.rs`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use dash_coreimport::netstate::governance::StoreVersion;
use dash_coreimport::netstate::sporks::SporkMessage;
use dash_coreimport::netstate::{NetAddr, governance, mncache, netfulfilled, sporks};
use dash_network::Network;
use serde_json::Value;

const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

const RC3: &str = "v24.0.0-rc.3";
const RC4: &str = "v24.0.0-rc.4";

/// Every captured set: the Core release, the directory and the network it ran on.
const CAPTURES: [(&str, &str, Network); 6] = [
    (RC3, "empty", Network::Regtest),
    (RC3, "populated", Network::Regtest),
    (RC3, "oracle-mainnet", Network::Mainnet),
    (RC3, "oracle-testnet", Network::Testnet),
    (RC4, "empty", Network::Regtest),
    (RC4, "populated", Network::Regtest),
];

fn data_of(release: &str, dir: &str, file: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/flatfiles")
        .join(release)
        .join(dir)
        .join(file);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A file of the rc.3 capture, the release most vectors are from.
fn data(dir: &str, file: &str) -> Vec<u8> {
    data_of(RC3, dir, file)
}

fn expected_of(release: &str) -> Value {
    serde_json::from_slice(&data_of(release, "populated", "expected.json")).unwrap()
}

fn expected() -> Value {
    expected_of(RC3)
}

/// A uint256 as RPC prints it (reversed) to the internal byte order.
fn hash(display: &str) -> [u8; 32] {
    assert_eq!(display.len(), 64, "{display}");
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let at = 62 - 2 * i;
        *byte = u8::from_str_radix(&display[at..at + 2], 16).unwrap();
    }
    out
}

/// The 20-byte key id of a base58check P2PKH address.
fn key_id(address: &str) -> [u8; 20] {
    let mut num: Vec<u8> = vec![0];
    for c in address.bytes() {
        let mut carry = ALPHABET.iter().position(|a| *a == c).unwrap();
        for byte in num.iter_mut().rev() {
            carry += usize::from(*byte) * 58;
            *byte = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            num.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    // 1 version byte + 20 key id + 4 checksum, and the leading zero the loop starts with.
    let tail = &num[num.len() - 25..];
    tail[1..21].try_into().unwrap()
}

#[test]
fn every_captured_file_parses_and_is_fully_consumed() {
    for (release, dir, network) in CAPTURES {
        let at = format!("{release}/{dir}");
        let g = governance::parse(&data_of(release, dir, "governance.dat"), network).unwrap();
        assert!(g.discarded.is_none(), "{at} governance.dat: {:?}", g.discarded);
        assert_eq!(g.trailing_bytes, 0, "{at} governance.dat");
        assert!(g.value.version_matched, "{at} governance.dat");
        // The release decides the version the file carries.
        let version = if release == RC4 {
            StoreVersion::V17
        } else {
            StoreVersion::V16
        };
        assert_eq!(g.value.version, Some(version), "{at} governance.dat");
        let m = mncache::parse(&data_of(release, dir, "mncache.dat"), network).unwrap();
        assert!(m.discarded.is_none(), "{at} mncache.dat: {:?}", m.discarded);
        assert_eq!(m.trailing_bytes, 0, "{at} mncache.dat");
        assert!(m.value.version_matched, "{at} mncache.dat");
        let n = netfulfilled::parse(&data_of(release, dir, "netfulfilled.dat"), network).unwrap();
        assert!(n.discarded.is_none(), "{at} netfulfilled.dat: {:?}", n.discarded);
        assert_eq!(n.trailing_bytes, 0, "{at} netfulfilled.dat");
        let s = sporks::parse(&data_of(release, dir, "sporks.dat"), network).unwrap();
        assert!(s.discarded.is_none(), "{at} sporks.dat: {:?}", s.discarded);
        assert_eq!(s.trailing_bytes, 0, "{at} sporks.dat");
        assert!(s.value.version_matched, "{at} sporks.dat");
    }
}

#[test]
fn empty_files_hold_nothing() {
    for release in [RC3, RC4] {
        let file = |name: &str| data_of(release, "empty", name);
        let g = governance::parse(&file("governance.dat"), Network::Regtest).unwrap().value;
        assert!(g.objects.is_empty() && g.erased.is_empty() && g.last_masternode_object.is_empty());
        assert!(g.orphan_votes.items.is_empty());
        let m = mncache::parse(&file("mncache.dat"), Network::Regtest).unwrap().value;
        assert!(m.infos.is_empty() && m.used_masternodes.is_empty());
        let n = netfulfilled::parse(&file("netfulfilled.dat"), Network::Regtest).unwrap().value;
        assert!(n.requests.is_empty());
        let s = sporks::parse(&file("sporks.dat"), Network::Regtest).unwrap().value;
        assert!(s.by_hash.is_empty() && s.active.is_empty());
    }
}

#[test]
fn sporks_match_spork_show_and_are_signed_by_the_network_key() {
    let exp = expected();
    let parsed = sporks::parse(&data("populated", "sporks.dat"), Network::Regtest).unwrap().value;
    let mut want: Vec<i64> =
        exp["spork_show"].as_object().unwrap().values().map(|v| v.as_i64().unwrap()).collect();
    let mut got: Vec<i64> = parsed.active.values().map(|m| m.value).collect();
    want.sort_unstable();
    got.sort_unstable();
    assert_eq!(got, want);
    assert_eq!(parsed.active.len(), 5);
    // `mapSporksByHash` keeps every message ever accepted, the superseded ones too.
    assert_eq!(parsed.by_hash.len(), 7);
    let ids: Vec<i32> = parsed.active.keys().copied().collect();
    assert_eq!(ids, [10001, 10016, 10018, 10020, 10022]);
    for (id, message) in &parsed.active {
        assert_eq!(*id, message.id);
        assert_eq!(
            parsed.by_hash.get(&message.hash()),
            Some(message),
            "mapSporksByHash key is GetHash()"
        );
    }
    for (key, message) in &parsed.by_hash {
        assert_eq!(*key, message.hash());
    }
    let key = key_id("yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW");
    for message in parsed.active.values() {
        assert!(
            message.check_signature(&key, SporkMessage::signs_hash(Network::Regtest)),
            "{message:?}"
        );
        let mut forged = message.clone();
        forged.value ^= 1;
        assert!(!forged.check_signature(&key, SporkMessage::signs_hash(Network::Regtest)));
    }
}

#[test]
fn oracle_spork_signatures_verify_on_every_network() {
    for (dir, network, address) in [
        ("oracle-mainnet", Network::Mainnet, "Xgtyuk76vhuFW2iT7UAiHgNdWXCf3J34wh"),
        ("oracle-testnet", Network::Testnet, "yjPtiKh2uwk3bDutTEA2q9mCtXyiZRWn55"),
    ] {
        let parsed = sporks::parse(&data(dir, "sporks.dat"), network).unwrap().value;
        assert!(!parsed.active.is_empty(), "{dir}");
        let key = key_id(address);
        for message in parsed.active.values() {
            assert!(
                message.check_signature(&key, SporkMessage::signs_hash(network)),
                "{dir}: {message:?}"
            );
        }
    }
}

#[test]
fn mncache_matches_the_masternode_list_and_votes() {
    let exp = expected();
    let parsed = mncache::parse(&data("populated", "mncache.dat"), Network::Regtest).unwrap().value;
    let protx: Vec<[u8; 32]> = exp["protx_list_valid"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| hash(h.as_str().unwrap()))
        .collect();
    assert_eq!(parsed.infos.len(), protx.len());
    for h in &protx {
        assert!(parsed.infos.contains_key(h), "no meta for a masternode of protx list");
    }
    // Each masternode's count per proposal is the number of votes the script cast with it; the
    // one further object is the superblock trigger the masternodes vote on themselves, and the
    // deleted proposal (erased from governance) is counted by nobody.
    let mut votes: BTreeMap<([u8; 32], [u8; 32]), i32> = BTreeMap::new();
    for v in exp["votes_cast_by_script"].as_array().unwrap() {
        let key = (hash(v["proTxHash"].as_str().unwrap()), hash(v["object"].as_str().unwrap()));
        *votes.entry(key).or_default() += 1;
    }
    let deleted = hash(exp["proposal_hashes_submitted"]["Proposal_3_deleted"].as_str().unwrap());
    let gov =
        governance::parse(&data("populated", "governance.dat"), Network::Regtest).unwrap().value;
    let triggers: Vec<[u8; 32]> =
        gov.objects.iter().filter(|(_, o)| o.object_type == 2).map(|(h, _)| *h).collect();
    assert_eq!(triggers.len(), 1);
    for meta in parsed.infos.values() {
        for (object, count) in &meta.governance_objects_voted_on {
            assert_ne!(*object, deleted);
            if triggers.contains(object) {
                continue;
            }
            assert_eq!(
                votes.get(&(meta.protx_hash, *object)),
                Some(count),
                "meta count differs from the votes cast"
            );
        }
        let proposals =
            meta.governance_objects_voted_on.keys().filter(|o| !triggers.contains(o)).count();
        let cast = votes.keys().filter(|(p, o)| *p == meta.protx_hash && *o != deleted).count();
        assert_eq!(proposals, cast);
    }
    assert!(parsed.infos.values().any(|m| !m.governance_objects_voted_on.is_empty()));
}

#[test]
fn netfulfilled_peers_are_the_connected_peers() {
    let exp = expected();
    let parsed = netfulfilled::parse(&data("populated", "netfulfilled.dat"), Network::Regtest)
        .unwrap()
        .value;
    assert!(!parsed.requests.is_empty());
    let peers: Vec<String> = exp["getpeerinfo_addr"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["addr"].as_str().unwrap().to_owned())
        .collect();
    for service in parsed.requests.keys() {
        let NetAddr::Ipv4(ip) = service.addr else {
            panic!("{service:?}")
        };
        let text = format!("{}:{}", std::net::Ipv4Addr::from(ip), service.port);
        assert!(peers.contains(&text), "{text} is not a peer of getpeerinfo {peers:?}");
    }
}

#[test]
fn governance_matches_gobject_count_and_the_object_list() {
    // rc.3's `-16` file and rc.4's `-17` file, each against what its own node answered.
    for release in [RC3, RC4] {
        governance_matches_rpc(release);
    }
}

fn governance_matches_rpc(release: &str) {
    let exp = expected_of(release);
    let parsed =
        governance::parse(&data_of(release, "populated", "governance.dat"), Network::Regtest)
            .unwrap()
            .value;
    let count = &exp["gobject_count"];
    assert_eq!(parsed.objects.len() as u64, count["objects_total"].as_u64().unwrap());
    assert_eq!(parsed.erased.len() as u64, count["erased"].as_u64().unwrap());
    let votes: usize = parsed.objects.values().map(|o| o.vote_file.votes.len()).sum();
    assert_eq!(votes as u64, count["votes"].as_u64().unwrap());
    for (display, listed) in exp["gobject_list_all"].as_object().unwrap() {
        let object =
            parsed.objects.get(&hash(display)).unwrap_or_else(|| panic!("{display} missing"));
        assert_eq!(object.collateral_hash, hash(listed["CollateralHash"].as_str().unwrap()));
        assert_eq!(object.object_type as i64, listed["ObjectType"].as_i64().unwrap());
        assert_eq!(object.time, listed["CreationTime"].as_i64().unwrap());
        // The RPC's `DataHex` is `HexStr(vchData)`, and `DataString` is those bytes as text.
        let data_hex: String = object.data.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(data_hex, listed["DataHex"].as_str().unwrap());
        assert_eq!(object.data, listed["DataString"].as_str().unwrap().as_bytes());
    }
    // The masternode list the file embeds is the node's: the same six masternodes.
    let protx: Vec<[u8; 32]> = exp["protx_list_valid"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| hash(h.as_str().unwrap()))
        .collect();
    assert_eq!(parsed.mn_list.len(), protx.len());
}

#[test]
fn testnet_governance_carries_a_real_masternode_list() {
    let loaded =
        governance::parse(&data("oracle-testnet", "governance.dat"), Network::Testnet).unwrap();
    assert!(loaded.discarded.is_none());
    assert!(loaded.value.mn_list.len() > 50, "{}", loaded.value.mn_list.len());
    assert!(loaded.value.mn_list.height() > 0);
}

// ---- hostile and damaged input -------------------------------------------------------------

use bitcoin_hashes::{Hash, sha256d};
use dash_coreimport::netstate::FormatError;

/// `data ‖ SHA256d(data)`: a flat file whose checksum is right.
fn seal(mut data: Vec<u8>) -> Vec<u8> {
    let sum = sha256d::Hash::hash(&data).to_byte_array();
    data.extend_from_slice(&sum);
    data
}

fn body(file: &[u8]) -> Vec<u8> {
    file[..file.len() - 32].to_vec()
}

fn string(out: &mut Vec<u8>, s: &str) {
    assert!(s.len() < 253);
    out.push(s.len() as u8);
    out.extend_from_slice(s.as_bytes());
}

/// The header of a regtest flat file.
fn header(magic_message: &str) -> Vec<u8> {
    let mut out = Vec::new();
    string(&mut out, magic_message);
    out.extend_from_slice(&[0xfc, 0xc1, 0xb7, 0xdc]);
    out
}

type Parser = fn(&[u8], Network) -> Result<Option<FormatError>, FormatError>;

fn parsers() -> [(&'static str, Parser); 4] {
    [
        ("governance.dat", |b, n| governance::parse(b, n).map(|l| l.discarded)),
        ("mncache.dat", |b, n| mncache::parse(b, n).map(|l| l.discarded)),
        ("netfulfilled.dat", |b, n| netfulfilled::parse(b, n).map(|l| l.discarded)),
        ("sporks.dat", |b, n| sporks::parse(b, n).map(|l| l.discarded)),
    ]
}

#[test]
fn every_prefix_and_bit_flip_is_refused_or_discarded_without_panicking() {
    for release in [RC3, RC4] {
        for (file, parse) in parsers() {
            let bytes = data_of(release, "populated", file);
            for len in 0..bytes.len() {
                // A prefix is never a valid file: the checksum no longer matches.
                assert!(
                    parse(&bytes[..len], Network::Regtest).is_err(),
                    "{release} {file} prefix {len}"
                );
            }
            let step = (bytes.len() / 400).max(1);
            for at in (0..bytes.len()).step_by(step) {
                let mut flipped = bytes.clone();
                flipped[at] ^= 0x40;
                assert!(
                    matches!(
                        parse(&flipped, Network::Regtest),
                        Err(FormatError::ChecksumMismatch { .. })
                    ),
                    "{release} {file} flip {at}"
                );
                // With the checksum repaired the damage reaches the object reader.
                let _ = parse(&seal(body(&flipped)), Network::Regtest);
            }
            // Truncating the object (checksum repaired): a clean error or a discarded object.
            for len in (0..bytes.len() - 32).step_by(step) {
                let _ = parse(&seal(bytes[..len].to_vec()), Network::Regtest);
            }
        }
    }
}

#[test]
fn a_file_of_another_network_is_wrong_magic() {
    for (file, parse) in parsers() {
        let bytes = data("populated", file);
        assert!(
            matches!(parse(&bytes, Network::Mainnet), Err(FormatError::WrongMagic { .. })),
            "{file}"
        );
    }
}

#[test]
fn another_flat_files_magic_message_is_corrupt() {
    let mut other = header("magicFulfilledCache");
    other.push(0);
    let file = seal(other);
    assert!(matches!(mncache::parse(&file, Network::Regtest), Err(FormatError::Corrupt { .. })));
}

#[test]
fn a_damaged_object_is_discarded_not_refused() {
    // The envelope is intact, the object ends early: Core clears it and starts empty.
    let good = data("populated", "sporks.dat");
    let cut = seal(good[..good.len() - 32 - 9].to_vec());
    let loaded = sporks::parse(&cut, Network::Regtest).unwrap();
    assert!(loaded.discarded.is_some());
    assert!(loaded.value.by_hash.is_empty() && loaded.value.active.is_empty());
}

#[test]
fn a_foreign_version_string_leaves_the_store_empty() {
    let mut file = header("magicMasternodeCache");
    string(&mut file, "CMasternodeMetaMan-Version-4");
    file.extend_from_slice(&[0xff; 40]); // whatever an older writer put here is not read
    let loaded = mncache::parse(&seal(file), Network::Regtest).unwrap();
    assert!(loaded.discarded.is_none());
    assert!(!loaded.value.version_matched);
    assert!(loaded.value.infos.is_empty());
}

#[test]
fn mncache_keeps_the_first_of_duplicate_masternodes() {
    let meta = |dsq: i64| {
        let mut out = vec![7u8; 32];
        out.extend_from_slice(&dsq.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.push(0); // no governance votes
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0i64.to_le_bytes());
        out.extend_from_slice(&0i64.to_le_bytes());
        out.push(0);
        out.extend_from_slice(&0i32.to_le_bytes());
        out
    };
    let mut file = header("magicMasternodeCache");
    string(&mut file, "CMasternodeMetaMan-Version-5");
    file.push(2);
    file.extend(meta(11));
    file.extend(meta(22));
    file.extend_from_slice(&5i64.to_le_bytes());
    file.push(0);
    let loaded = mncache::parse(&seal(file), Network::Regtest).unwrap();
    assert!(loaded.discarded.is_none());
    assert_eq!(loaded.value.infos.len(), 1);
    assert_eq!(loaded.value.infos[&[7u8; 32]].last_dsq, 11);
    assert_eq!(loaded.value.dsq_count, 5);
}

#[test]
fn mncache_clear_keeps_the_dsq_counter() {
    // No metas, `nDsqCount` 7, and a used-masternodes vector that promises three hashes and holds
    // two: the counter was assigned before the failure, and `Clear()` does not reset it.
    let mut body = header("magicMasternodeCache");
    string(&mut body, "CMasternodeMetaMan-Version-5");
    body.push(0);
    body.extend_from_slice(&7i64.to_le_bytes());
    body.push(3);
    body.extend_from_slice(&[1u8; 32]);
    body.extend_from_slice(&[2u8; 32]);
    let loaded = mncache::parse(&seal(body.clone()), Network::Regtest).unwrap();
    assert!(loaded.discarded.is_some());
    assert!(loaded.value.infos.is_empty() && loaded.value.used_masternodes.is_empty());
    assert_eq!(loaded.value.dsq_count, 7);
    // The same file with its third hash parses whole and keeps all three.
    body.extend_from_slice(&[3u8; 32]);
    let whole = mncache::parse(&seal(body), Network::Regtest).unwrap();
    assert!(whole.discarded.is_none());
    assert_eq!((whole.value.dsq_count, whole.value.used_masternodes.len()), (7, 3));
}

#[test]
fn counts_the_file_cannot_back_end_in_a_discard() {
    // A count of 2^25 masternodes with no bytes behind it.
    let mut file = header("magicMasternodeCache");
    string(&mut file, "CMasternodeMetaMan-Version-5");
    file.extend_from_slice(&[0xfe, 0x00, 0x00, 0x00, 0x02]);
    let loaded = mncache::parse(&seal(file), Network::Regtest).unwrap();
    assert!(loaded.discarded.is_some());
    // A count over MAX_SIZE is refused as corrupt before any element is read.
    let mut file = header("magicMasternodeCache");
    string(&mut file, "CMasternodeMetaMan-Version-5");
    file.extend_from_slice(&[0xfe, 0x01, 0x00, 0x00, 0x02]);
    let loaded = mncache::parse(&seal(file), Network::Regtest).unwrap();
    assert!(loaded.discarded.is_some());
}

#[test]
fn files_over_the_cap_are_refused_before_reading() {
    let big = vec![0u8; sporks::MAX_SPORKS_DAT_SIZE + 1];
    assert!(matches!(sporks::parse(&big, Network::Regtest), Err(FormatError::TooLarge { .. })));
}

// ---- detection -----------------------------------------------------------------------------

use dash_coreimport::netstate::{Format, Version, detect_named, sniff};

#[test]
fn the_flat_files_are_detected_by_name_and_by_content() {
    for (release, dir, network) in CAPTURES {
        for (file, format) in [
            ("governance.dat", Format::GovernanceDat),
            ("mncache.dat", Format::MncacheDat),
            ("netfulfilled.dat", Format::NetfulfilledDat),
            ("sporks.dat", Format::SporksDat),
        ] {
            let bytes = data_of(release, dir, file);
            let named = detect_named(file, &bytes).unwrap();
            assert_eq!(named.format, format, "{dir}/{file}");
            assert_eq!(named.network, Some(network), "{dir}/{file}");
            let sniffed = sniff(&bytes).unwrap();
            assert_eq!(sniffed.format, format, "{dir}/{file}");
            assert_eq!(sniffed.network, Some(network), "{dir}/{file}");
            // A damaged file is never sniffed as something else.
            let mut flipped = bytes.clone();
            let last = flipped.len() - 1;
            flipped[last] ^= 1;
            assert!(sniff(&flipped).is_err(), "{dir}/{file}");
        }
    }
}

// ---- governance.dat details Core's loader decides ------------------------------------------

/// A vote of the disk form: masternode outpoint hash, parent, outcome, signal, time and a
/// signature of `sig` bytes.
fn vote_bytes(outpoint: u8, time: i64, sig: &[u8]) -> Vec<u8> {
    let mut out = vec![outpoint; 32];
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&[9u8; 32]);
    out.extend_from_slice(&1i32.to_le_bytes());
    out.extend_from_slice(&1i32.to_le_bytes());
    out.extend_from_slice(&time.to_le_bytes());
    string_bytes(&mut out, sig);
    out
}

fn string_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    assert!(bytes.len() < 253);
    out.push(bytes.len() as u8);
    out.extend_from_slice(bytes);
}

/// The version string `StoreVersion` is written as.
fn version_string(version: StoreVersion) -> &'static str {
    match version {
        StoreVersion::V16 => "CGovernanceManager-Version-16",
        StoreVersion::V17 => "CGovernanceManager-Version-17",
    }
}

/// The release whose empty capture carries `version`.
fn release_of(version: StoreVersion) -> &'static str {
    match version {
        StoreVersion::V16 => RC3,
        StoreVersion::V17 => RC4,
    }
}

/// What `version` writes between the erased objects and the orphan votes: the `-16` invalid-vote
/// cache (an empty `CacheMap`: 4-byte capacity and a count), which `-17` dropped.
fn invalid_votes(version: StoreVersion) -> &'static [u8] {
    match version {
        StoreVersion::V16 => &[0; 4 + 1],
        StoreVersion::V17 => &[],
    }
}

/// The empty fixture's masternode list: what follows the last (empty) section of a file.
fn empty_gov_tail(version: StoreVersion) -> Vec<u8> {
    let empty = data_of(release_of(version), "empty", "governance.dat");
    let mut prefix = header("magicGovernanceCache");
    string(&mut prefix, version_string(version));
    // erased (1), invalid votes (-16 only), orphans (4 + 1), objects (1), last objects (1)
    empty[prefix.len() + 8 + invalid_votes(version).len()..empty.len() - 32].to_vec()
}

/// A governance file of the current version (`-17`) with the given orphan section bytes
/// (`max_size` first) and objects section, empty otherwise.
fn gov_file(orphans: &[u8], objects: &[u8]) -> Vec<u8> {
    gov_file_of(StoreVersion::V17, orphans, objects)
}

fn gov_file_of(version: StoreVersion, orphans: &[u8], objects: &[u8]) -> Vec<u8> {
    gov_file_with_list(version, orphans, objects, &empty_gov_tail(version))
}

/// [`gov_file_of`] with the bytes of the masternode list at the end.
fn gov_file_with_list(
    version: StoreVersion,
    orphans: &[u8],
    objects: &[u8],
    list: &[u8],
) -> Vec<u8> {
    let mut file = header("magicGovernanceCache");
    string(&mut file, version_string(version));
    file.push(0); // erased
    file.extend_from_slice(invalid_votes(version));
    file.extend_from_slice(orphans);
    file.extend_from_slice(objects);
    file.push(0); // last objects
    file.extend_from_slice(list);
    seal(file)
}

#[test]
fn a_masternode_list_over_its_cap_is_a_bad_file() {
    // `lastMNListForVotingKeys` has no length budget of its own in `dash-dml`; the reader caps its
    // bytes (a hostile entry can spend 144 requested bytes on each, `tests/flatfiles_memory.rs`).
    // Over the cap the file is discarded as a bad one before the list is decoded.
    let file_with_list_of = |len: usize| {
        let mut list = vec![0u8; 32]; // block hash
        list.extend_from_slice(&(-1i32).to_le_bytes());
        list.extend_from_slice(&0u32.to_le_bytes());
        list.push(1); // one entry, which is zeros
        list.resize(len, 0);
        gov_file_with_list(StoreVersion::V17, &[0; 5], &[0], &list)
    };
    let cap = governance::MAX_MN_LIST_SIZE;
    let over = governance::parse(&file_with_list_of(cap + 1), Network::Regtest).unwrap();
    assert!(
        matches!(over.discarded, Some(FormatError::TooLarge { limit, .. }) if limit == cap as u64),
        "{:?}",
        over.discarded
    );
    assert!(over.value.objects.is_empty() && over.value.mn_list.is_default());
    // At the cap the list is read (and these zeros are no masternode), so the file is refused for
    // that reason and not for its size.
    let at = governance::parse(&file_with_list_of(cap), Network::Regtest).unwrap();
    assert!(!matches!(at.discarded, Some(FormatError::TooLarge { .. })), "{:?}", at.discarded);
}

#[test]
fn the_vote_file_drops_repeated_votes_and_recounts() {
    // One object with three votes, two of them the same vote (the signature is not part of the
    // hash) and a stored `nMemoryVotes` of 99: Core's `RebuildIndex` keeps two and counts two.
    let mut object = vec![7u8; 32]; // the map key
    object.extend_from_slice(&[1u8; 32]); // parent
    object.extend_from_slice(&1i32.to_le_bytes());
    object.extend_from_slice(&0i64.to_le_bytes());
    object.extend_from_slice(&[2u8; 32]); // collateral
    object.push(0); // data
    object.extend_from_slice(&1i32.to_le_bytes());
    object.extend_from_slice(&[3u8; 36]); // masternode outpoint
    object.push(0); // signature
    object.extend_from_slice(&0i64.to_le_bytes());
    object.push(0); // expired
    object.push(0); // no current votes
    object.extend_from_slice(&99i32.to_le_bytes());
    object.push(3);
    object.extend(vote_bytes(1, 100, b"first"));
    object.extend(vote_bytes(2, 100, b""));
    object.extend(vote_bytes(1, 100, b"second signature"));
    let mut objects = vec![1u8];
    objects.extend(object);
    let loaded = governance::parse(&gov_file(&[0; 5], &objects), Network::Regtest).unwrap();
    assert!(loaded.discarded.is_none(), "{:?}", loaded.discarded);
    let file = &loaded.value.objects[&[7u8; 32]].vote_file;
    assert_eq!(file.votes.len(), 2);
    assert_eq!(file.memory_votes, 2);
    assert_eq!(file.votes[0].signature, b"first");
}

#[test]
fn the_orphan_vote_capacity_follows_core_through_clear() {
    // A version string of another release: Core's `cmmapOrphanVotes(MAX_CACHE_SIZE)` is untouched.
    let mut file = header("magicGovernanceCache");
    string(&mut file, "CGovernanceManager-Version-15");
    let loaded = governance::parse(&seal(file), Network::Regtest).unwrap();
    assert!(!loaded.value.version_matched);
    assert_eq!(loaded.value.orphan_votes.max_size, governance::MAX_CACHE_SIZE);
    assert_eq!(governance::MAX_CACHE_SIZE, 1_000_000);
    // A file that stores a capacity of 1234 and then fails: `Clear()` empties the list and keeps it.
    let mut orphans = 1234u32.to_le_bytes().to_vec();
    orphans.push(1); // one item, and then nothing
    for version in [StoreVersion::V16, StoreVersion::V17] {
        let mut file = header("magicGovernanceCache");
        string(&mut file, version_string(version));
        file.push(0);
        file.extend_from_slice(invalid_votes(version));
        file.extend_from_slice(&orphans);
        let loaded = governance::parse(&seal(file), Network::Regtest).unwrap();
        assert!(loaded.discarded.is_some(), "{version:?}");
        assert_eq!(loaded.value.orphan_votes.max_size, 1234, "{version:?}");
        assert!(loaded.value.orphan_votes.items.is_empty(), "{version:?}");
    }
}

// ---- governance.dat versions (BL-420) --------------------------------------------------------

#[test]
fn a_governance_file_of_an_unknown_version_is_not_loaded() {
    // Core's `Unserialize` reads the version string and, if it is not the one it writes, calls
    // `Clear()` and returns: the store starts empty and the file is not an error.
    // Core: governance.h GovernanceStore::Unserialize
    for version in [
        "CGovernanceManager-Version-15",
        "CGovernanceManager-Version-18",
        "CGovernanceManager-Version-1",
        "CGovernanceManager-Version-16 ",
        "cgovernancemanager-version-17",
        "",
    ] {
        let mut file = header("magicGovernanceCache");
        string(&mut file, version);
        // What a writer of that version put next is not read, however well formed it looks: here
        // the populated rc.4 file's objects.
        let populated = data_of(RC4, "populated", "governance.dat");
        let after_version = FLAT_HEADER_LEN + 1 + version_string(StoreVersion::V17).len();
        file.extend_from_slice(&populated[after_version..populated.len() - 32]);
        let file = seal(file);
        let loaded = governance::parse(&file, Network::Regtest).unwrap();
        assert!(loaded.discarded.is_none(), "{version:?}: {:?}", loaded.discarded);
        assert!(!loaded.value.version_matched, "{version:?}");
        assert_eq!(loaded.value.version, None, "{version:?}");
        let g = &loaded.value;
        assert!(g.objects.is_empty() && g.erased.is_empty() && g.last_masternode_object.is_empty());
        assert!(g.orphan_votes.items.is_empty() && g.mn_list.is_default(), "{version:?}");
        // Detection says the same: a governance.dat, not of a version the reader loads.
        let named = detect_named("governance.dat", &file).unwrap();
        assert_eq!(named.format, Format::GovernanceDat, "{version:?}");
        assert_eq!(
            named.version,
            Version::Flat {
                current: false
            },
            "{version:?}"
        );
    }
    // Both versions the reader loads are current.
    for (release, version) in [(RC3, StoreVersion::V16), (RC4, StoreVersion::V17)] {
        let file = data_of(release, "populated", "governance.dat");
        let named = detect_named("governance.dat", &file).unwrap();
        assert_eq!(
            named.version,
            Version::Flat {
                current: true
            },
            "{version:?}"
        );
    }
}

#[test]
fn governance_versions_read_to_the_same_store() {
    // `-16` is `-17` plus the invalid-vote cache between the erased objects and the orphan votes.
    // Turn each populated `-16` file into the `-17` form (drop the cache, change the version
    // string, reseal) and the readers must agree on everything else.
    for (dir, network) in [
        ("empty", Network::Regtest),
        ("populated", Network::Regtest),
        ("oracle-mainnet", Network::Mainnet),
        ("oracle-testnet", Network::Testnet),
    ] {
        let v16 = data(dir, "governance.dat");
        let objects_at = FLAT_HEADER_LEN + 1 + version_string(StoreVersion::V16).len();
        let mut cursor = objects_at;
        // The erased map: count, then (hash, expiry time) pairs.
        let erased = usize::from(v16[cursor]);
        assert!(erased < 253, "{dir}");
        cursor += 1 + erased * (32 + 8);
        // The invalid-vote cache: its capacity (1,000,000) and no entries, in every capture.
        let mut cache = governance::MAX_CACHE_SIZE.to_le_bytes().to_vec();
        cache.push(0);
        assert_eq!(&v16[cursor..cursor + 5], &cache[..], "{dir}");
        let mut v17 = v16[..FLAT_HEADER_LEN].to_vec();
        string(&mut v17, version_string(StoreVersion::V17));
        v17.extend_from_slice(&v16[objects_at..cursor]);
        v17.extend_from_slice(&v16[cursor + 5..v16.len() - 32]);
        let v17 = seal(v17);
        let old = governance::parse(&v16, network).unwrap();
        let new = governance::parse(&v17, network).unwrap();
        assert!(old.discarded.is_none() && new.discarded.is_none(), "{dir}");
        assert_eq!(old.value.version, Some(StoreVersion::V16), "{dir}");
        assert_eq!(new.value.version, Some(StoreVersion::V17), "{dir}");
        assert_eq!(old.trailing_bytes, new.trailing_bytes, "{dir}");
        let (a, b) = (&old.value, &new.value);
        assert_eq!(a.erased, b.erased, "{dir}");
        assert_eq!(a.orphan_votes, b.orphan_votes, "{dir}");
        assert_eq!(a.objects, b.objects, "{dir}");
        assert_eq!(a.last_masternode_object, b.last_masternode_object, "{dir}");
        assert!(a.mn_list.is_equal(&b.mn_list), "{dir}");
    }
}

/// The header of `governance.dat` on any network: `magicGovernanceCache` and the 4 network magic bytes.
const FLAT_HEADER_LEN: usize = 1 + "magicGovernanceCache".len() + 4;

#[test]
fn the_object_signature_limit_is_a_version_17_rule() {
    // rc.4 reads `Governance::Object::vchSig` with `LIMITED_VECTOR(…, MAX_SIGNATURE_SIZE)`; the
    // `-16` readers of v23 and rc.3 did not. Core: governance/common.h:37, :85
    let limit = governance::MAX_OBJECT_SIGNATURE_SIZE;
    assert_eq!(limit, 96);
    let object = |sig_len: usize| {
        let mut object = vec![7u8; 32]; // the map key
        object.extend_from_slice(&[1u8; 32]); // parent
        object.extend_from_slice(&1i32.to_le_bytes());
        object.extend_from_slice(&0i64.to_le_bytes());
        object.extend_from_slice(&[2u8; 32]); // collateral
        object.push(0); // data
        object.extend_from_slice(&1i32.to_le_bytes());
        object.extend_from_slice(&[3u8; 36]); // masternode outpoint
        object.push(sig_len as u8); // signature
        object.extend(std::iter::repeat_n(0xab, sig_len));
        object.extend_from_slice(&0i64.to_le_bytes());
        object.push(0); // expired
        object.push(0); // no current votes
        object.extend_from_slice(&0i32.to_le_bytes());
        object.push(0); // no votes
        let mut section = vec![1u8];
        section.extend(object);
        section
    };
    for version in [StoreVersion::V16, StoreVersion::V17] {
        for len in [0, 1, limit - 1, limit] {
            let loaded =
                governance::parse(&gov_file_of(version, &[0; 5], &object(len)), Network::Regtest)
                    .unwrap();
            assert!(loaded.discarded.is_none(), "{version:?} {len}: {:?}", loaded.discarded);
            assert_eq!(loaded.value.objects[&[7u8; 32]].signature.len(), len);
        }
    }
    let over = limit + 1;
    // Over the limit a `-17` object fails to read, so the file is discarded as a bad one and the
    // store starts empty; a `-16` object has no such limit.
    let loaded = governance::parse(
        &gov_file_of(StoreVersion::V17, &[0; 5], &object(over)),
        Network::Regtest,
    )
    .unwrap();
    assert!(
        matches!(loaded.discarded, Some(FormatError::Corrupt { .. })),
        "{:?}",
        loaded.discarded
    );
    assert!(loaded.value.objects.is_empty());
    let loaded = governance::parse(
        &gov_file_of(StoreVersion::V16, &[0; 5], &object(over)),
        Network::Regtest,
    )
    .unwrap();
    assert!(loaded.discarded.is_none(), "{:?}", loaded.discarded);
    assert_eq!(loaded.value.objects[&[7u8; 32]].signature.len(), over);
}

#[test]
fn spork_check_and_remove_follows_core() {
    let parsed = sporks::parse(&data("populated", "sporks.dat"), Network::Regtest).unwrap().value;
    let key = key_id("yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW");
    // Every message of the file, the superseded two included, is validly signed: nothing goes.
    assert_eq!(parsed.check_and_remove(Network::Regtest, &key), parsed);
    // No key set: nothing is checked.
    assert_eq!(parsed.check_and_remove(Network::Regtest, &[0; 20]), parsed);
    // Another network's key: everything fails and both maps empty out.
    let other = key_id("Xgtyuk76vhuFW2iT7UAiHgNdWXCf3J34wh");
    let cleaned = parsed.check_and_remove(Network::Regtest, &other);
    assert!(cleaned.active.is_empty() && cleaned.by_hash.is_empty());
    // One forged active message goes from `active` and from `by_hash` under its computed hash;
    // the entry the file keyed it by does not matter.
    let mut forged = parsed.clone();
    let id = *forged.active.keys().next().unwrap();
    let message = forged.active.get_mut(&id).unwrap();
    let old_hash = message.hash();
    message.value ^= 1;
    let new_hash = message.hash();
    let message = message.clone();
    let moved = forged.by_hash.remove(&old_hash).unwrap();
    assert_ne!(moved, message);
    forged.by_hash.insert(new_hash, message);
    let cleaned = forged.check_and_remove(Network::Regtest, &key);
    assert_eq!(cleaned.active.len(), parsed.active.len() - 1);
    assert!(!cleaned.by_hash.contains_key(&new_hash));
}
