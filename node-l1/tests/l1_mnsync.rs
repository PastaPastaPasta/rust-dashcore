//! SP1's masternode sync, fulfilled requests and meta cache against Dash Core v24's in-process
//! oracle (BL-451, DEC-626): the sync progressions SP1's dashd transcripts walk and the D-row
//! corners as fixed scripts, every netfulfilled.dat and mncache.dat Dash Core wrote that the tree
//! holds (and edits of them) loaded by both, and random scripts of each kind (the BL-451b fuzz
//! targets' twins). Expected values are Core's own, from the oracle: a comparison asserts only
//! agreement and the outcome class.

use std::path::{Path, PathBuf};

use dash_oracle_sys::addrman::{NET_IPV4, NetAddr, Service};
use dash_oracle_sys::{FlatFile, FulfilledOp, MetaOp, Oracle, Pin, SyncBlock, SyncOp};
use dashd_masternode::flatdb::core_layout::decode_with_magic;
use dashd_masternode::meta::MasternodeMetaStore;
use dashd_masternode::netfulfilled::NetFulfilledStore;
use node_l1::Outcome;
use node_l1::mnsync::{
    FlatInput, compare_flatdb, compare_flatdb_load, compare_mncache, compare_mncache_script,
    compare_mnsync, compare_mnsync_script, compare_netfulfilled, compare_netfulfilled_script,
    core_flat_files, flat_input, flatdb_seed_inputs, fulfilled_script, mncache_seed_inputs,
    mnsync_seed_inputs, netfulfilled_seed_inputs, network, platform_ban_message, sync_script,
};
use proptest::prelude::*;

const PIN: Pin = Pin::V24_0;
/// [`node_l1::mnsync::NETWORKS`]' selectors.
const MAINNET: u8 = 0;
const TESTNET: u8 = 1;
const REGTEST: u8 = 2;
const DEVNET: u8 = 3;
/// BIP155's network id for Tor v3 (`NetAddr`'s ids are BIP155's; IPv4's is 1 in both).
const BIP155_TORV3: u8 = 4;

fn agree(result: node_l1::Compared) -> Outcome {
    result.unwrap_or_else(|d| panic!("diverges: {d}"))
}

/* ---- the sync ---- */

fn block(height: i32, tag: u8) -> SyncBlock {
    SyncBlock {
        height,
        hash: [tag; 32],
    }
}

fn tip(tip: Option<SyncBlock>, new: SyncBlock, initial_download: bool) -> SyncOp {
    SyncOp::UpdatedBlockTip {
        tip,
        new,
        initial_download,
    }
}

fn sync_rows() -> Vec<(&'static str, Vec<SyncOp>)> {
    let a = block(1, 1);
    let b = block(2, 2);
    vec![
        // mnsync-four-peers / -disabled: headers then a tip at the best header, out of IBD,
        // then the tick's two switches 6 s and 30 s on.
        (
            "sync to finished",
            vec![
                SyncOp::AcceptedBlockHeader(a),
                SyncOp::NotifyHeaderTip {
                    block: Some(a),
                    initial_download: false,
                },
                tip(Some(a), a, false),
                SyncOp::Time(1_000_006),
                SyncOp::SwitchToNextAsset,
                SyncOp::BumpAttempt,
                SyncOp::Time(1_000_036),
                SyncOp::SwitchToNextAsset,
                SyncOp::AcceptedBlockHeader(b),
                tip(Some(b), b, false),
            ],
        ),
        // mnsync-allow-sync: `mnsync reset` (Reset(true)) after FINISHED, and again.
        (
            "reset after finished",
            vec![
                SyncOp::SwitchToNextAsset,
                SyncOp::SwitchToNextAsset,
                SyncOp::Reset {
                    force: true,
                    notify: true,
                },
                SyncOp::Reset {
                    force: true,
                    notify: false,
                },
            ],
        ),
        // An unforced reset within MASTERNODE_SYNC_RESET_SECONDS of the last tip update is
        // skipped; at 900 s it goes ahead.
        (
            "unforced reset window",
            vec![
                tip(Some(a), a, false),
                SyncOp::Time(1_000_899),
                SyncOp::Reset {
                    force: false,
                    notify: true,
                },
                SyncOp::Time(1_000_900),
                SyncOp::Reset {
                    force: false,
                    notify: true,
                },
            ],
        ),
        // Back in IBD after the BLOCKCHAIN switch: "switched too early", Reset(true).
        (
            "ibd after the switch",
            vec![SyncOp::SwitchToNextAsset, tip(Some(a), a, true), tip(None, a, true)],
        ),
        // Reached the best header, then a tip behind it: Reset(true); a missing best header
        // returns before the comparison.
        (
            "best header lost",
            vec![
                tip(Some(a), a, false),
                tip(Some(b), a, false),
                tip(None, b, false),
                tip(Some(b), b, false),
            ],
        ),
        // Synced: notifications only stamp the tip time; bumps are no-ops.
        (
            "synced ignores the chain",
            vec![
                SyncOp::SwitchToNextAsset,
                SyncOp::SwitchToNextAsset,
                SyncOp::Time(1_000_100),
                SyncOp::BumpAssetLastTime,
                SyncOp::AcceptedBlockHeader(a),
                SyncOp::NotifyHeaderTip {
                    block: None,
                    initial_download: true,
                },
                tip(Some(b), a, false),
                SyncOp::SwitchToNextAsset,
            ],
        ),
    ]
}

#[test]
fn sync_progressions_agree_on_every_network() {
    for selector in [MAINNET, TESTNET, REGTEST, DEVNET] {
        for (name, ops) in sync_rows() {
            let outcome = compare_mnsync_script(selector, 1_000_000, &ops, PIN)
                .unwrap_or_else(|d| panic!("{name} on {selector}: {d}"));
            assert_eq!(outcome, Outcome::BothAccept, "{name}");
        }
    }
}

/// An unforced reset before any tip update, within 900 s of the initial last-update time 0, is
/// skipped; at 900 s it goes ahead.
#[test]
fn early_unforced_reset_agrees() {
    let reset = SyncOp::Reset {
        force: false,
        notify: true,
    };
    let ops = vec![
        SyncOp::SwitchToNextAsset,
        reset.clone(),
        SyncOp::Time(899),
        reset.clone(),
        SyncOp::Time(900),
        reset,
    ];
    for selector in [MAINNET, REGTEST] {
        assert_eq!(agree(compare_mnsync_script(selector, 100, &ops, PIN)), Outcome::BothAccept);
    }
}

#[test]
fn sync_refusals_are_out_of_domain() {
    // A time outside the oracle's 1..2^33.
    let ops = [SyncOp::Time(0)];
    assert_eq!(agree(compare_mnsync_script(REGTEST, 1_000_000, &ops, PIN)), Outcome::OutOfDomain);
    assert_eq!(agree(compare_mnsync(&[], PIN)), Outcome::OutOfDomain);
}

/* ---- the fulfilled requests ---- */

fn ipv4(last: u8, port: u16) -> (Service, dash_netaddr::Service) {
    let core = Service::new(
        NetAddr {
            network: NET_IPV4,
            bytes: vec![10, 0, 0, last],
        },
        port,
    );
    let ours = dash_netaddr::Service::new(dash_netaddr::NetAddr::ipv4([10, 0, 0, last]), port);
    (core, ours)
}

fn onion(tag: u8, port: u16) -> (Service, dash_netaddr::Service) {
    let core = Service::new(
        NetAddr {
            network: BIP155_TORV3,
            bytes: vec![tag; 32],
        },
        port,
    );
    #[allow(clippy::expect_used)]
    let addr =
        dash_netaddr::NetAddr::new(dash_netaddr::Network::Onion, vec![tag; 32]).expect("onion");
    (core, dash_netaddr::Service::new(addr, port))
}

fn loopback(port: u16) -> (Service, dash_netaddr::Service) {
    let mut bytes = [0u8; 16];
    bytes[15] = 1;
    let core = Service::new(
        NetAddr {
            network: dash_oracle_sys::addrman::NET_IPV6,
            bytes: bytes.to_vec(),
        },
        port,
    );
    #[allow(clippy::expect_used)]
    let addr =
        dash_netaddr::NetAddr::new(dash_netaddr::Network::Ipv6, bytes.to_vec()).expect("ipv6");
    let ours = dash_netaddr::Service::new(addr, port);
    (core, ours)
}

fn with_peer(
    peer: &(Service, dash_netaddr::Service),
    op: impl FnOnce(Service) -> FulfilledOp,
) -> (FulfilledOp, dash_netaddr::Service) {
    (op(peer.0.clone()), peer.1.clone())
}

/// D4's expiry corners at the network's `nFulfilledRequestExpireTime` (3600 s on mainnet, 300 s
/// elsewhere), and D5's v1 collapse of Tor keys in the store.
#[test]
fn fulfilled_corners_agree() {
    let p = ipv4(1, 9999);
    let q = ipv4(1, 19999);
    for selector in [MAINNET, TESTNET, REGTEST, DEVNET] {
        let expire = i64::from(network(selector).1.fulfilled_request_expire_time);
        let start = 1_000_000;
        let time = |t: i64| (FulfilledOp::Time(t), p.1.clone());
        let ops = vec![
            with_peer(&p, |s| FulfilledOp::Add(s, "full-sync".into())),
            with_peer(&q, |s| FulfilledOp::Add(s, "mempool-sync".into())),
            time(start + expire - 1),
            with_peer(&p, |s| FulfilledOp::Has(s, "full-sync".into())),
            with_peer(&q, |s| FulfilledOp::Has(s, "full-sync".into())),
            time(start + expire),
            with_peer(&p, |s| FulfilledOp::Has(s, "full-sync".into())),
            (FulfilledOp::CheckAndRemove, p.1.clone()),
            with_peer(&p, |s| FulfilledOp::Add(s, "governance-sync".into())),
            time(start + expire + 1),
            (FulfilledOp::DoMaintenance, p.1.clone()),
            with_peer(&q, FulfilledOp::RemoveAll),
        ];
        assert_eq!(
            agree(compare_netfulfilled_script(selector, start, &ops, PIN)),
            Outcome::BothAccept
        );
    }
    // Two onion peers share the v1 key [::]:port in the store; a third differs by its port.
    let (a, b, c) = (onion(1, 9999), onion(2, 9999), onion(1, 9998));
    let ops = vec![
        with_peer(&a, |s| FulfilledOp::Add(s, "full-sync".into())),
        with_peer(&b, |s| FulfilledOp::Add(s, "mempool-sync".into())),
        with_peer(&c, |s| FulfilledOp::Add(s, "full-sync".into())),
        with_peer(&b, |s| FulfilledOp::Has(s, "full-sync".into())),
    ];
    assert_eq!(
        agree(compare_netfulfilled_script(REGTEST, 1_000_000, &ops, PIN)),
        Outcome::BothAccept
    );
}

/// Core's key order across networks (`(network, bytes)`, then the port: every IPv4 address before
/// `::1`, whose v1 bytes sort first), and the file Core writes for them reloaded: the onions
/// re-keyed to `[::]` and re-sorted, and of two that collapse, the first stays.
#[test]
fn fulfilled_keys_order_and_reload_agree() {
    let peers = [onion(2, 9999), loopback(9999), ipv4(1, 19999), onion(1, 9999), ipv4(1, 9999)];
    let ops: Vec<_> = peers
        .iter()
        .enumerate()
        .map(|(i, p)| {
            with_peer(p, |s| FulfilledOp::Add(s, ["full-sync", "mempool-sync"][i % 2].into()))
        })
        .collect();
    let start = 1_000_000;
    assert_eq!(agree(compare_netfulfilled_script(REGTEST, start, &ops, PIN)), Outcome::BothAccept);
    let script: Vec<FulfilledOp> = ops.iter().map(|(op, _)| op.clone()).collect();
    let (chain, params) = network(REGTEST);
    let body = dash_oracle_sys::Oracle::new(PIN)
        .netfulfilled_step(chain, start, &script)
        .expect("the script runs")
        .store;
    let mut file = Vec::new();
    file.push(19);
    file.extend(b"magicFulfilledCache");
    file.extend(params.message_start);
    file.extend(&body);
    let hash = dash_consensus::hash::sha256d(&file);
    file.extend(hash);
    let load = FlatInput {
        selector: REGTEST,
        kind: FlatFile::NetFulfilled,
        mocktime: start,
        file: Some(file),
    };
    assert_eq!(agree(compare_flatdb_load(&load, PIN)), Outcome::BothAccept);
}

/* ---- the meta cache ---- */

/// D6's corners: the mixing threshold and counters, votes, outbound statistics, a ban and its
/// reset, the used-masternodes FIFO, and the seen-ban LRU past 1800 entries.
#[test]
fn meta_corners_agree() {
    let (a, b) = ([1u8; 32], [2u8; 32]);
    let obj = [9u8; 32];
    let mut ops = vec![
        MetaOp::MixingThreshold(a, 10),
        MetaOp::AllowMixing(a),
        MetaOp::AllowMixing(b),
        MetaOp::MixingThreshold(a, 4),
        MetaOp::MixingThreshold(a, 5),
        MetaOp::MixingThreshold(a, 10),
        MetaOp::MixingThreshold(a, i32::MAX),
        MetaOp::MixingThreshold(a, -5),
    ];
    for _ in 0..6 {
        ops.extend([MetaOp::DisallowMixing(a), MetaOp::ValidForMixing(a)]);
    }
    ops.extend([
        MetaOp::AddGovernanceVote(a, obj),
        MetaOp::AddGovernanceVote(a, obj),
        MetaOp::AddGovernanceVote(b, obj),
        MetaOp::GetInfo(a),
        MetaOp::RemoveGovernanceObject(obj),
        MetaOp::GetInfo(a),
        MetaOp::GetInfo([7; 32]),
    ]);
    for t in 1..=6 {
        ops.extend([MetaOp::SetLastOutboundAttempt(b, t), MetaOp::OutboundFailedTooManyTimes(b)]);
    }
    ops.extend([
        MetaOp::SetLastOutboundSuccess(b, 7),
        MetaOp::LastOutboundAttempt(b),
        MetaOp::LastOutboundSuccess(b),
        MetaOp::OutboundFailedTooManyTimes(b),
        MetaOp::ResetPlatformBan(a, 5),
        MetaOp::SetPlatformBan([3; 32], platform_ban_message(a, 10, [4; 32])),
        MetaOp::SetPlatformBan([4; 32], platform_ban_message(a, 9, [4; 32])),
        MetaOp::SetPlatformBan([5; 32], platform_ban_message(a, 10, [4; 32])),
        MetaOp::IsPlatformBanned(a),
        MetaOp::GetPlatformBan([3; 32]),
        MetaOp::GetPlatformBan([4; 32]),
        MetaOp::AlreadyHavePlatformBan([5; 32]),
        MetaOp::ResetPlatformBan(a, 9),
        MetaOp::ResetPlatformBan(a, 11),
        MetaOp::IsPlatformBanned(a),
        MetaOp::GetInfo(a),
    ]);
    for h in [a, b, a, [3; 32], [4; 32]] {
        ops.push(MetaOp::AddUsed(h));
    }
    ops.extend([
        MetaOp::UsedCount,
        MetaOp::RemoveUsed(2),
        MetaOp::IsUsed(a),
        MetaOp::IsUsed([3; 32]),
        MetaOp::RemoveUsed(10),
        MetaOp::UsedCount,
    ]);
    assert_eq!(agree(compare_mncache_script(REGTEST, &ops, PIN)), Outcome::BothAccept);

    // 1801 bans on distinct masternodes: past twice its 900 the LRU keeps the newest 900.
    let inventory = |i: u32| {
        let mut inv = [0u8; 32];
        inv[..4].copy_from_slice(&i.to_le_bytes());
        inv
    };
    let mut lru = Vec::new();
    for i in 0..1801u32 {
        let inv = inventory(i);
        let mut protx = inv;
        protx[31] = 1;
        lru.push(MetaOp::SetPlatformBan(inv, platform_ban_message(protx, 1, [0; 32])));
    }
    for i in [0u32, 1, 900, 901, 1800] {
        lru.push(MetaOp::AlreadyHavePlatformBan(inventory(i)));
    }
    assert_eq!(agree(compare_mncache_script(MAINNET, &lru, PIN)), Outcome::BothAccept);

    // The accesses that decide the survivors: after 1800 bans, a lookup of the oldest, a read of
    // the next and a re-set of the third (at a later height, which re-emplaces it) make them the
    // most recently used, so the 1801st keeps them where it cuts the untouched 3 and 4.
    let ban = |i: u32, height: i32| {
        let mut protx = inventory(i);
        protx[31] = 1;
        MetaOp::SetPlatformBan(inventory(i), platform_ban_message(protx, height, [0; 32]))
    };
    let mut touched: Vec<MetaOp> = (0..1800).map(|i| ban(i, 1)).collect();
    touched.extend([
        MetaOp::AlreadyHavePlatformBan(inventory(0)),
        MetaOp::GetPlatformBan(inventory(1)),
        ban(2, 2),
        ban(1800, 1),
    ]);
    for i in 0..5u32 {
        touched.push(MetaOp::AlreadyHavePlatformBan(inventory(i)));
        touched.push(MetaOp::GetPlatformBan(inventory(i)));
    }
    assert_eq!(agree(compare_mncache_script(MAINNET, &touched, PIN)), Outcome::BothAccept);
}

/* ---- the flat files ---- */

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[test]
fn core_files_load_alike() {
    let files = core_flat_files();
    assert!(files.len() >= 23, "{} files", files.len());
    for (path, selector, kind) in files {
        #[allow(clippy::expect_used)]
        let bytes = std::fs::read(&path).expect("read");
        // The seam reads Core's own file (the cross-check of D3 against Core's layout).
        let ms = network(selector).1.message_start;
        match kind {
            FlatFile::NetFulfilled => {
                assert!(decode_with_magic::<NetFulfilledStore>(&bytes, ms).is_ok())
            }
            FlatFile::MnCache => {
                assert!(decode_with_magic::<MasternodeMetaStore>(&bytes, ms).is_ok())
            }
        }
        // Before every expiry, between, and after all of them; and on the wrong network.
        for mocktime in [1, 1_700_000_000, 1 << 33] {
            let load = FlatInput {
                selector,
                kind,
                mocktime,
                file: Some(bytes.clone()),
            };
            let outcome = compare_flatdb_load(&load, PIN)
                .unwrap_or_else(|d| panic!("{} at {mocktime}: {d}", path.display()));
            assert_eq!(outcome, Outcome::BothAccept, "{}", path.display());
        }
        let other = FlatInput {
            selector: selector ^ 1,
            kind,
            mocktime: 1,
            file: Some(bytes.clone()),
        };
        assert_eq!(
            agree(compare_flatdb_load(&other, PIN)),
            Outcome::BothReject,
            "{}",
            path.display()
        );
        // On the wrong network with a wrong magic message too: the magic is checked first.
        let mut both = bytes.clone();
        both[1] ^= 0x20;
        let n = both.len() - 32;
        let hash = dash_consensus::hash::sha256d(&both[..n]);
        both[n..].copy_from_slice(&hash);
        let both = FlatInput {
            file: Some(both),
            ..other
        };
        assert_eq!(
            agree(compare_flatdb_load(&both, PIN)),
            Outcome::BothReject,
            "{}",
            path.display()
        );
        // Each byte flipped once, and each length cut, on a sample of positions.
        for i in (0..bytes.len()).step_by(bytes.len().div_ceil(64).max(1)) {
            let mut flipped = bytes.clone();
            flipped[i] ^= 0x80;
            let load = FlatInput {
                selector,
                kind,
                mocktime: 1,
                file: Some(flipped),
            };
            agree(compare_flatdb_load(&load, PIN));
            let load = FlatInput {
                file: Some(bytes[..i].to_vec()),
                ..load
            };
            agree(compare_flatdb_load(&load, PIN));
        }
    }
}

/// The 18 crafted files dashd cleared (D17; `clear/PROVENANCE.md`): Core reads them as
/// `IncorrectFormat` (or, the short one, `HashReadError`), and so does the node, with the same
/// object left and the same file written over them.
#[test]
fn cleared_files_load_alike() {
    let dir = root().join("dashd-masternode/tests/fixtures/dashd-v24-rc4/clear");
    let mut n = 0;
    #[allow(clippy::expect_used)]
    for entry in std::fs::read_dir(&dir).expect("clear dir") {
        #[allow(clippy::expect_used)]
        let path = entry.expect("entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        let kind = match name.split('-').next() {
            Some("nf") => FlatFile::NetFulfilled,
            Some("mn") => FlatFile::MnCache,
            _ => continue,
        };
        #[allow(clippy::expect_used)]
        let bytes = std::fs::read(&path).expect("read");
        let load = FlatInput {
            selector: REGTEST,
            kind,
            mocktime: 1,
            file: Some(bytes),
        };
        let outcome = compare_flatdb_load(&load, PIN).unwrap_or_else(|d| panic!("{name}: {d}"));
        let expected = if name == "nf-short.dat" {
            Outcome::BothReject
        } else {
            Outcome::BothAccept
        };
        assert_eq!(outcome, expected, "{name}");
        n += 1;
    }
    assert_eq!(n, 15);
}

/// A byte-form comparison.
type Compare = fn(&[u8], Pin) -> node_l1::Compared;

/// The generated loads reach both outcomes, and the oracle refuses none of the generated inputs
/// (each generator keeps to the shim's domain, so a refusal is a drift between the two sides'
/// encodings, not a skip).
#[test]
fn generated_inputs_compare() {
    let mut loads = std::collections::BTreeMap::<String, usize>::new();
    for seed in 0..600u64 {
        let load = flat_input(&splitmix(seed, 96));
        let outcome =
            compare_flatdb_load(&load, PIN).unwrap_or_else(|d| panic!("seed {seed}: {d}"));
        assert_ne!(outcome, Outcome::OutOfDomain, "seed {seed}");
        *loads.entry(format!("{outcome:?}")).or_default() += 1;
    }
    assert!(
        loads.get("BothAccept") > Some(&100) && loads.get("BothReject") > Some(&100),
        "{loads:?}"
    );
    let scripts: [(&str, Compare); 3] = [
        ("sync", compare_mnsync),
        ("netfulfilled", compare_netfulfilled),
        ("mncache", compare_mncache),
    ];
    for (name, compare) in scripts {
        let skipped = (0..300u64)
            .filter(|&seed| agree(compare(&splitmix(seed, 160), PIN)) == Outcome::OutOfDomain)
            .count();
        assert_eq!(skipped, 0, "{name}: {skipped} of 300 out of domain");
    }
}

/// The populated `mncache.dat` Core's own `CMasternodeMetaMan` and `CFlatDB` write (through the
/// oracle) for SP1's codec vectors (D17's gap: no dashd capture holds a live platform ban, a
/// nonzero `nMixingTxCount` or a used-masternodes queue): the store a script leaves, loaded from
/// its Core-layout file and stored back by the manager's destructor.
fn core_written_mncache() -> Vec<u8> {
    let (a, b, c) = ([0xa1u8; 32], [0xb2u8; 32], [0xc3u8; 32]);
    let ops = [
        MetaOp::AllowMixing(a),
        MetaOp::AllowMixing(b),
        MetaOp::DisallowMixing(a),
        MetaOp::DisallowMixing(a),
        MetaOp::AddGovernanceVote(a, [0x11; 32]),
        MetaOp::AddGovernanceVote(a, [0x11; 32]),
        MetaOp::AddGovernanceVote(b, [0x22; 32]),
        MetaOp::SetLastOutboundAttempt(c, 1_700_000_000),
        MetaOp::SetLastOutboundSuccess(c, 1_700_000_005),
        MetaOp::SetLastOutboundAttempt(c, 1_700_000_010),
        MetaOp::SetPlatformBan([0x01; 32], platform_ban_message(b, 120, [0x33; 32])),
        MetaOp::SetPlatformBan([0x02; 32], platform_ban_message(c, 130, [0x33; 32])),
        MetaOp::ResetPlatformBan(c, 140),
        MetaOp::AddUsed(a),
        MetaOp::AddUsed(b),
        MetaOp::AddUsed(c),
        MetaOp::AddUsed(a),
        MetaOp::RemoveUsed(1),
    ];
    let oracle = dash_oracle_sys::Oracle::new(PIN);
    let (chain, params) = network(REGTEST);
    let body = oracle.mncache_step(chain, &ops).expect("the script runs").store;
    let mut file = Vec::new();
    file.push(20);
    file.extend(b"magicMasternodeCache");
    file.extend(params.message_start);
    file.extend(&body);
    let hash = dash_consensus::hash::sha256d(&file);
    file.extend(hash);
    let load =
        oracle.flatdb_load(chain, FlatFile::MnCache, 1_700_000_000, Some(&file)).expect("loads");
    assert_eq!((load.read, load.loaded), (dash_oracle_sys::ReadResult::Ok, true));
    assert_eq!(load.store, body);
    load.file_after.expect("the destructor stored it")
}

/// `dashd-masternode/tests/fixtures/oracle-v24/populated.mncache.dat` is what the pinned Core
/// writes now. `BL451_WRITE_VECTORS=1` rewrites it.
#[test]
fn core_written_vector_is_current() {
    let path = root().join("dashd-masternode/tests/fixtures/oracle-v24/populated.mncache.dat");
    let bytes = core_written_mncache();
    if std::env::var_os("BL451_WRITE_VECTORS").is_some() {
        std::fs::write(&path, &bytes).expect("write the vector");
    }
    assert_eq!(std::fs::read(&path).expect("the vector"), bytes);
    let load = FlatInput {
        selector: REGTEST,
        kind: FlatFile::MnCache,
        mocktime: 1,
        file: Some(bytes),
    };
    assert_eq!(agree(compare_flatdb_load(&load, PIN)), Outcome::BothAccept);
}

/* ---- the fuzz targets (BL-451b) ---- */

/// Each target with its public seeds.
fn targets() -> [(&'static str, Compare, Vec<Vec<u8>>); 4] {
    [
        ("diff_mnsync", compare_mnsync, mnsync_seed_inputs()),
        ("diff_netfulfilled", compare_netfulfilled, netfulfilled_seed_inputs()),
        ("diff_mncache", compare_mncache, mncache_seed_inputs()),
        ("diff_flatdb_load", compare_flatdb, flatdb_seed_inputs()),
    ]
}

#[test]
fn registered_on_v24() {
    for (name, compare, _) in targets() {
        let t = node_l1::target(name).expect("registered");
        assert_eq!(t.pins, &[PIN]);
        assert_eq!(t.compare as usize, compare as usize, "{name}");
        // An empty input has no network to pick, except for a load: no file on mainnet.
        let empty = if name == "diff_flatdb_load" {
            Outcome::BothAccept
        } else {
            Outcome::OutOfDomain
        };
        assert_eq!(agree((t.compare)(&[], PIN)), empty, "{name}");
    }
}

/// Each target's public seeds (`node-fuzz/public-seeds`, written by the `mnsync_seeds` example)
/// are current.
#[test]
fn public_seeds_are_current() {
    use bitcoin_hashes::sha256;
    for (target, _, seeds) in targets() {
        let dir = root().join("node-fuzz/public-seeds").join(target);
        let mut names = std::collections::BTreeSet::new();
        for input in &seeds {
            let digest = sha256::Hash::hash(input).to_byte_array();
            let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
            let path = dir.join(&name);
            assert_eq!(std::fs::read(&path).ok().as_ref(), Some(input), "{}", path.display());
            names.insert(name);
        }
        let on_disk = std::fs::read_dir(&dir).expect("the seed directory").count();
        assert_eq!(on_disk, names.len(), "stale seeds in {}: rerun mnsync_seeds", dir.display());
    }
}

/// Every seed compares (none diverges, the oracle refuses none), and the flat-file seeds reach
/// both verdicts.
#[test]
fn seeds_compare() {
    for (target, compare, seeds) in targets() {
        let mut outcomes = std::collections::BTreeSet::new();
        for (i, input) in seeds.iter().enumerate() {
            let outcome = compare(input, PIN).unwrap_or_else(|d| panic!("{target} seed {i}: {d}"));
            assert_ne!(outcome, Outcome::OutOfDomain, "{target} seed {i}");
            outcomes.insert(format!("{outcome:?}"));
        }
        if target == "diff_flatdb_load" {
            assert_eq!(outcomes.len(), 2, "{target}: {outcomes:?}");
        }
    }
}

/// On every network some generated `netfulfilled.dat` seed keeps a request through Core's
/// `LoadCache(true)`, whose `CheckAndRemove()` drops expired ones: the seeds load a live store,
/// not only an emptied one (BL-451b r0).
#[test]
fn generated_fulfilled_seeds_keep_a_request() {
    let generated = FlatFile::NetFulfilled as u8 | 2 << 1;
    let mut kept = std::collections::BTreeSet::new();
    for seed in flatdb_seed_inputs().iter().filter(|s| s.get(1) == Some(&generated)) {
        let load = flat_input(seed);
        let chain = &network(load.selector).0;
        let core = Oracle::new(PIN)
            .flatdb_load(chain, load.kind, load.mocktime, load.file.as_deref())
            .expect("the oracle loads a generated store");
        // The body opens with the peer count (a compact size).
        if core.loaded && core.store.first().is_some_and(|&peers| peers > 0) {
            kept.insert(load.selector);
        }
    }
    assert_eq!(kept, (0..4).collect(), "networks whose generated stores load a request");
}

/// The fulfilled-request walk seed steps to one second before its requests' expiry, to it and one
/// second past it: where `HasFulfilledRequest()` turns false and where `CheckAndRemove()` prunes.
#[test]
fn fulfilled_walk_seeds_straddle_the_expiry() {
    let mut walks = 0;
    for seed in netfulfilled_seed_inputs() {
        let expire = i64::from(network(seed[0]).1.fulfilled_request_expire_time);
        let (start, ops) = fulfilled_script(&seed[1..], expire);
        let times: Vec<i64> = ops
            .iter()
            .filter_map(|(op, _)| match op {
                FulfilledOp::Time(t) => Some(t - start - expire),
                _ => None,
            })
            .collect();
        if times.len() == 3 {
            assert_eq!(times, [-1, 0, 1], "network {}", seed[0]);
            walks += 1;
        }
    }
    assert_eq!(walks, 4, "one walk per network");
}

/// The sync seeds move the clock: on every network they step it by 6, 31, 899, 900 and 901 s
/// (around the 900 s reset window), not only by 0 s.
#[test]
fn sync_seeds_step_the_clock() {
    let mut steps = std::collections::BTreeMap::<u8, std::collections::BTreeSet<i64>>::new();
    for seed in mnsync_seed_inputs() {
        let (start, ops) = sync_script(&seed[1..]);
        let mut now = start;
        for op in ops {
            if let SyncOp::Time(t) = op {
                steps.entry(seed[0]).or_default().insert(t - now);
                now = t;
            }
        }
    }
    for selector in 0..4 {
        let seen = steps.get(&selector).cloned().unwrap_or_default();
        for want in [6, 31, 899, 900, 901] {
            assert!(seen.contains(&want), "network {selector}: no {want} s step in {seen:?}");
        }
    }
}

/// Splitmix64 bytes from `seed`.
fn splitmix(seed: u64, n: usize) -> Vec<u8> {
    let mut state = seed;
    (0..n.div_ceil(8))
        .flat_map(|_| {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            (z ^ (z >> 31)).to_le_bytes()
        })
        .take(n)
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    /// The fuzz targets' twins: random inputs never diverge, and the oracle refuses none
    /// (only an empty input has no network to pick).
    #[test]
    fn random_sync_scripts_agree(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let outcome = compare_mnsync(&bytes, PIN);
        prop_assert!(outcome == Ok(Outcome::BothAccept) || bytes.is_empty(), "{outcome:?}");
    }

    #[test]
    fn random_fulfilled_scripts_agree(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let outcome = compare_netfulfilled(&bytes, PIN);
        prop_assert!(outcome == Ok(Outcome::BothAccept) || bytes.is_empty(), "{outcome:?}");
    }

    #[test]
    fn random_meta_scripts_agree(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let outcome = compare_mncache(&bytes, PIN);
        prop_assert!(outcome == Ok(Outcome::BothAccept) || bytes.is_empty(), "{outcome:?}");
    }

    #[test]
    fn random_flat_files_agree(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let outcome = compare_flatdb(&bytes, PIN);
        prop_assert!(matches!(outcome, Ok(Outcome::BothAccept | Outcome::BothReject)), "{outcome:?}");
    }
}
