//! SP1's `netfulfilled.dat` and `mncache.dat` bodies against files Core wrote (SP1 D3): each
//! body is read by SP1's codec and written back byte for byte, and SP1's reading agrees with
//! IM1b's reader of Core's file (`dash-coreimport`). The files are dashd v24.0.0-rc.4's (SP1's
//! mnsync captures, `tests/fixtures/dashd-v24-rc4/`, and IM1b's BL-420 empty and seven-node
//! populated captures) and v24.0.0-rc.3's (IM1b's regtest, mainnet and testnet captures); IM1b's
//! are under `dash-coreimport/tests/data/flatfiles/`. The crafted files dashd clears
//! (`tests/fixtures/dashd-v24-rc4/clear/`) check SP1's failures and their `CoreRead` lines.

use std::path::PathBuf;

use dash_coreimport::netstate::{mncache, netfulfilled};
use dash_network::Network;
use dashd_masternode::flatdb::{self, FlatObject, LoadError};
use dashd_masternode::meta::{self, MasternodeMetaStore};
use dashd_masternode::netfulfilled::{self as nf, NetFulfilledStore};

/// Core's `MasternodeMetaStore::SERIALIZATION_VERSION_STRING`.
// Core: masternode/meta.cpp:12
const CORE_META_VERSION: &[u8] = b"CMasternodeMetaMan-Version-5";

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every Core file named `file`, with its network.
fn files(file: &str) -> Vec<(PathBuf, Network)> {
    let mut out = Vec::new();
    for name in [
        "mnsync-disabled",
        "mnsync-four-peers",
        "mnsync-blocksonly",
        "mnsync-governance",
        "mnsync-allow-sync",
    ] {
        let path = manifest().join(format!("tests/fixtures/dashd-v24-rc4/{name}.{file}"));
        out.push((path, Network::Regtest));
    }
    let rc4 = manifest().join("../dash-coreimport/tests/data/flatfiles/v24.0.0-rc.4");
    for dir in ["empty", "populated"] {
        out.push((rc4.join(dir).join(file), Network::Regtest));
    }
    let rc3 = manifest().join("../dash-coreimport/tests/data/flatfiles/v24.0.0-rc.3");
    for (dir, net) in [
        ("empty", Network::Regtest),
        ("populated", Network::Regtest),
        ("oracle-mainnet", Network::Mainnet),
        ("oracle-testnet", Network::Testnet),
    ] {
        out.push((rc3.join(dir).join(file), net));
    }
    out
}

/// The object's bytes in Core's `CFlatDB` file: after the magic message and the message start,
/// before the SHA256d.
fn core_body(bytes: &[u8]) -> &[u8] {
    let magic = usize::from(bytes[0]);
    assert!(magic < 0xfd);
    &bytes[1 + magic + 4..bytes.len() - 32]
}

fn with_version(version: &[u8], rest: &[u8]) -> Vec<u8> {
    let mut out = vec![u8::try_from(version.len()).unwrap()];
    out.extend_from_slice(version);
    out.extend_from_slice(rest);
    out
}

/// Reads `rdashd_body` into `T` and writes it back; the bytes after the version string.
fn round_trip<T: FlatObject>(rdashd_body: &[u8], version: &str) -> (T, Vec<u8>) {
    let mut obj = T::default();
    let mut data = rdashd_body;
    assert_eq!(obj.read(&mut data), Ok(()));
    assert!(data.is_empty(), "the whole body is read");
    let mut out = Vec::new();
    obj.write(&mut out);
    (obj, out[1 + version.len()..].to_vec())
}

#[test]
fn netfulfilled_bodies_round_trip_and_match_im1b() {
    let mut nonempty = 0;
    for (path, net) in files("netfulfilled.dat") {
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let body = core_body(&bytes);
        let (ours, back) = round_trip::<NetFulfilledStore>(
            &with_version(nf::VERSION.as_bytes(), body),
            nf::VERSION,
        );
        assert_eq!(back, body, "{}: written back", path.display());
        let core = netfulfilled::parse(&bytes, net).unwrap().value;
        let entries = |m: Vec<&std::collections::BTreeMap<Vec<u8>, i64>>| {
            let mut v: Vec<_> = m.into_iter().cloned().collect();
            v.sort();
            v
        };
        assert_eq!(
            entries(ours.requests.values().collect()),
            entries(core.requests.values().collect()),
            "{}",
            path.display()
        );
        let ports = |v: Vec<u16>| {
            let mut v = v;
            v.sort_unstable();
            v
        };
        assert_eq!(
            ports(ours.requests.keys().map(|s| s.port).collect()),
            ports(core.requests.keys().map(|s| s.port).collect()),
            "{}",
            path.display()
        );
        nonempty += usize::from(!ours.requests.is_empty());
    }
    assert!(nonempty >= 5, "the vectors hold requests");
}

#[test]
fn mncache_bodies_round_trip_and_match_im1b() {
    let (mut infos, mut votes, mut outbound, mut resets) = (0, 0, 0, 0);
    for (path, net) in files("mncache.dat") {
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let body = core_body(&bytes);
        let rest =
            body.strip_prefix(&with_version(CORE_META_VERSION, &[])[..]).expect("Core's version");
        let (ours, back) = round_trip::<MasternodeMetaStore>(
            &with_version(meta::VERSION.as_bytes(), rest),
            meta::VERSION,
        );
        assert_eq!(back, rest, "{}: written back", path.display());
        let core = mncache::parse(&bytes, net).unwrap().value;
        assert!(core.version_matched);
        assert_eq!(ours.dsq_count, core.dsq_count, "{}", path.display());
        let used: Vec<[u8; 32]> = ours.used_masternodes.iter().map(|h| *h.as_bytes()).collect();
        assert_eq!(used, core.used_masternodes, "{}", path.display());
        assert_eq!(ours.infos.len(), core.infos.len(), "{}", path.display());
        for (protx, info) in &ours.infos {
            let c = &core.infos[protx.as_bytes()];
            assert_eq!(
                (
                    info.last_dsq,
                    info.mixing_tx_count,
                    &info.governance_objects_voted_on,
                    info.outbound_attempt_count,
                    info.last_outbound_attempt,
                    info.last_outbound_success,
                    info.platform_ban,
                    info.platform_ban_updated,
                ),
                (
                    c.last_dsq,
                    c.mixing_tx_count,
                    &c.governance_objects_voted_on,
                    c.outbound_attempt_count,
                    c.last_outbound_attempt,
                    c.last_outbound_success,
                    c.platform_ban,
                    c.platform_ban_updated,
                ),
                "{}",
                path.display()
            );
        }
        infos += ours.infos.len();
        votes += ours.infos.values().map(|i| i.governance_objects_voted_on.len()).sum::<usize>();
        outbound += ours.infos.values().filter(|i| i.last_outbound_success > 0).count();
        // `ResetPlatformBan`'s state: not banned, with the height it was reset at.
        resets +=
            ours.infos.values().filter(|i| !i.platform_ban && i.platform_ban_updated > 0).count();
    }
    // rc.4 populated: 6 infos, 30 votes; rc.3 populated: 6, 23; rc.3 testnet oracle: 103 infos,
    // 5 with an outbound success, 2 reset platform bans.
    assert_eq!((infos, votes, outbound, resets), (115, 86, 5, 2), "the vectors' states");
}

/// The regtest message start.
const REGTEST: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

/// `bytes`, a Core file, in SP1's envelope: SP1's magic, and SP1's version string before the
/// object (Core's has none, `core_version` `None`) or instead of `core_version`. An object that
/// does not start with `core_version` stays as it is, as does a file that fails before its
/// object (shorter than its hash, or a magic Core cannot read).
fn sp1_file<T: FlatObject>(bytes: &[u8], version: &str, core_version: Option<&[u8]>) -> Vec<u8> {
    if bytes.len() < 32 || bytes[0] >= 0xfd {
        return bytes.to_vec();
    }
    let body = core_body(bytes);
    let object = match core_version {
        None => with_version(version.as_bytes(), body),
        Some(core) => match body.strip_prefix(&with_version(core, &[])[..]) {
            Some(rest) => with_version(version.as_bytes(), rest),
            None => body.to_vec(),
        },
    };
    let mut out = with_version(T::MAGIC.as_bytes(), &REGTEST);
    out.extend_from_slice(&object);
    let hash = dash_consensus::hash::sha256d(&out);
    out.extend_from_slice(&hash);
    out
}

/// Core's `SporkStore::SERIALIZATION_VERSION_STRING`.
// Core: spork.cpp:21
const CORE_SPORK_VERSION: &[u8] = b"CSporkManager-Version-3";

/// A Core `sporks.dat` whose by-hash map holds one entry, in SP1's envelope: SP1's magic and
/// version, and the spork list without Core's hash key (SP0 D3).
fn sp1_spork_file(bytes: &[u8]) -> Vec<u8> {
    let body = core_body(bytes);
    let rest = body.strip_prefix(&with_version(CORE_SPORK_VERSION, &[1])[..]).expect("one spork");
    let mut out = with_version(b"rdashd-sporks", &REGTEST);
    out.extend_from_slice(&with_version(b"SporkStore-1", &[1]));
    out.extend_from_slice(&rest[32..]);
    let hash = dash_consensus::hash::sha256d(&out);
    out.extend_from_slice(&hash);
    out
}

/// Core's clear of a file with a compact size over `MAX_SIZE`, at `MAX_SIZE` with no data after
/// it, or non-canonical (in the 9-byte form too, where that check comes first), in each place one
/// is read (the magic and version strings, the outer and inner netfulfilled maps and a request
/// name, the infos, a vote map and the used masternodes); of a spork signature at, just over and
/// far over its 65-byte `LIMITED_VECTOR` bound; and of a file shorter than its hash: SP1 fails
/// the same read, recreates the file, and logs dashd's `CoreRead` line.
// Core: serialize.h:38-42, :292-322 ReadCompactSize, :1000-1031 LimitedVectorFormatter;
// flat-database.h:75-155 CFlatDB::CoreRead
#[test]
fn sizes_core_refuses_clear_with_dashds_line() {
    let dir = manifest().join("tests/fixtures/dashd-v24-rc4/clear");
    let table = std::fs::read_to_string(dir.join("coreread-clear.txt")).unwrap();
    let mut cases = Vec::new();
    for row in table.lines().filter(|l| !l.starts_with('#')) {
        let [case, file, line] = row.split('\t').collect::<Vec<_>>()[..] else {
            panic!("row {row:?}");
        };
        let bytes = std::fs::read(dir.join(format!("{case}.dat"))).unwrap();
        let err = match file {
            "netfulfilled.dat" => flatdb::decode::<NetFulfilledStore>(
                &sp1_file::<NetFulfilledStore>(&bytes, nf::VERSION, None),
                REGTEST,
            )
            .map(drop),
            "mncache.dat" => flatdb::decode::<MasternodeMetaStore>(
                &sp1_file::<MasternodeMetaStore>(&bytes, meta::VERSION, Some(CORE_META_VERSION)),
                REGTEST,
            )
            .map(drop),
            "sporks.dat" => {
                dashd_masternode::sporks::file::decode(&sp1_spork_file(&bytes), REGTEST).map(drop)
            }
            _ => panic!("file {file}"),
        }
        .expect_err(case);
        assert_eq!(err.core_read_error().as_deref(), Some(line), "{case}");
        assert_eq!(err.recreates(), !matches!(err, LoadError::Short), "{case}");
        cases.push(case.to_owned());
    }
    assert_eq!(cases.len(), 18, "{cases:?}");
}
