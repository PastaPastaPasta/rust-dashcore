//! L1 differential of the node's masternode list (`dash-dml`, EV2a) against Dash Core v24's
//! in-process oracle (O2a-EV, ABI 8). It covers:
//! - canonical list bytes and `CanonicalMNListHash`;
//! - the verdicts of Core's strict canonical reader;
//! - Core's evodb list and diff encodings;
//! - `BuildDiff`, `ApplyDiff` and the state diffs;
//! - the CHAMP iteration order after random mutation histories;
//! - the unique-property keys.
//!
//! Inputs are the lists Core produced in `dash-dml/tests/data/codec/` (provenance in its
//! README) and lists built here. They are mainnet-shaped, so the chain is mainnet: Core's strict
//! reader checks each entry's network info against the selected network's parameters.
//! The codec runs EV1's port of that check (`core_net_info`), so every verdict is compared.

use std::sync::Arc;

use dash_dml::codec::{
    canonical_mn_list_hash, core_disk, decode_canonical_diff, decode_canonical_list,
};
use dash_dml::testkit::list_disk_bytes;
use dash_dml::testkit::{dmn_bytes, dmn_with, state_bytes, state_diff_bytes};
use dash_dml::{KeyId, MnList, MnStateDiff, MnType, OperatorKey, ProTxHash, UniqueKey};
use dash_oracle_sys::{Chain, ErrorKind, ListEncoding, ListOp, Oracle, UniqueProperty};
use dash_pkc::bls::{BlsSchemeKind, BlsSchemeSource};
use dash_primitives::{AddrV2, BlockHash, OutPoint, PlatformNodeId, ServiceV2, TxHash};
use node_l1::dml::{
    canonical, canonical_diff, class, compare_diff_outputs, compare_list_outputs,
    compare_unique_hash, compare_unique_outputs, core_net_info, core_text, reader_text,
    unique_property, unique_seeds,
};
use node_l1::{Outcome, TARGETS};

const LISTS: &str = include_str!("../../dash-dml/tests/data/codec/lists.txt");
const VERDICTS: &str = include_str!("../../dash-dml/tests/data/codec/verdicts.txt");
const BUDGET: &str = include_str!("../../dash-dml/tests/data/codec/budget.txt");

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn records(text: &str) -> impl Iterator<Item = Vec<&str>> {
    text.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).map(|l| l.split(' ').collect())
}

/// The Core-produced lists by name, decoded, with their canonical bytes.
fn lists() -> Vec<(String, MnList, Vec<u8>)> {
    records(LISTS)
        .map(|f| {
            let bytes = unhex(f[2]);
            let list =
                decode_canonical_list(&mut bytes.as_slice(), core_net_info()).expect("Core's list");
            (f[1].to_string(), list, bytes)
        })
        .collect()
}

/// Core reads each list back to the same bytes and hash, and writes the evodb encoding that our
/// importer reads as the same list.
#[test]
fn canonical_lists_match_the_oracle() {
    let chain = Chain::main();
    for oracle in Oracle::all() {
        for (name, list, bytes) in lists() {
            let core = oracle
                .mnlist_canonical(&chain, ListEncoding::Canonical, &bytes)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(core.canonical, canonical(&list), "{name}");
            assert_eq!(&core.hash, canonical_mn_list_hash(&list).as_bytes(), "{name}");
            let imported = core_disk::decode_list_s3(&mut core.disk.as_slice()).expect(&name);
            assert!(imported.is_equal(&list), "{name}: the evodb list");
            assert_eq!(canonical(&imported), core.canonical, "{name}");
        }
    }
}

/// The canonical list reader's verdicts on seeded mutations match Core's strict reader's: the
/// same acceptance, the same list, and Core's refusal text.
#[test]
fn canonical_reader_verdicts_match_the_oracle() {
    let chain = Chain::main();
    for oracle in Oracle::all() {
        let (mut accepted, mut refused) = (0, 0);
        for f in records(VERDICTS).filter(|f| f[0] == "canon") {
            let bytes = unhex(f[1]);
            let mut input = bytes.as_slice();
            let ours =
                decode_canonical_list(&mut input, core_net_info()).map_err(|e| e.0).and_then(|l| {
                    if input.is_empty() {
                        Ok(l)
                    } else {
                        Err("trailing".into())
                    }
                });
            let core = oracle.mnlist_canonical(&chain, ListEncoding::Canonical, &bytes);
            match (&ours, &core) {
                (Ok(list), Ok(core)) => {
                    assert_eq!(canonical(list), core.canonical, "{}", f[1]);
                    assert_eq!(&core.hash, canonical_mn_list_hash(list).as_bytes());
                    accepted += 1;
                }
                (Err(ours), Err(e)) if e.kind == ErrorKind::Decode => {
                    let core = core_text(&e.message);
                    if reader_text(core) {
                        assert_eq!(class(ours), class(core), "{}", f[1]);
                    }
                    refused += 1;
                }
                (Err(t), Err(e)) if e.kind == ErrorKind::Trailing && t == "trailing" => {
                    refused += 1
                }
                _ => panic!("{}: ours {ours:?}, Core {core:?}", f[1]),
            }
        }
        assert!(accepted > 50 && refused > 50, "accepted {accepted}, refused {refused}");
        eprintln!("{:?} verdicts: {accepted} accepted, {refused} refused", oracle.pin());
    }
}

/// The nested CompactSize budget cases (EV2a fix r1): the oracle gives the probe's verdicts,
/// and ours are Core's, with Core's text wherever the reader owns it.
#[test]
fn nested_budget_verdicts_match_the_oracle() {
    let chain = Chain::main();
    for oracle in Oracle::all() {
        let mut budget_refusals = 0;
        for f in records(BUDGET) {
            let (name, bytes) = (f[1], unhex(f[2]));
            let mut input = bytes.as_slice();
            let ours = decode_canonical_list(&mut input, core_net_info()).map_err(|e| e.0);
            assert!(input.is_empty() || ours.is_err(), "{name}: trailing bytes");
            match (&ours, oracle.mnlist_canonical(&chain, ListEncoding::Canonical, &bytes)) {
                (Ok(list), Ok(core)) => {
                    assert_eq!(f[3], "ok", "{name}: the probe refused it");
                    assert_eq!(hex(&core.hash), f[4], "{name}: the probe's hash");
                    assert_eq!(&core.hash, canonical_mn_list_hash(list).as_bytes(), "{name}");
                }
                (Err(ours), Err(e)) if e.kind == ErrorKind::Decode => {
                    let core = core_text(&e.message);
                    assert_eq!(
                        f[3..].join(" "),
                        format!("err {}", e.message),
                        "{name}: the probe's text"
                    );
                    if reader_text(core) {
                        assert_eq!(ours, core, "{name}");
                    }
                    budget_refusals += usize::from(core.contains("budget exceeded"));
                }
                (ours, core) => panic!("{name}: ours {ours:?}, Core {core:?}"),
            }
        }
        assert!(budget_refusals >= 7, "{budget_refusals} budget refusals");
    }
}

/// `BuildDiff` between every ordered pair of lists is Core's in both encodings. `ApplyDiff` of
/// it to the first list gives Core's list, or both refuse. Every state diff of an entry in
/// both lists is Core's, and so is the state it produces.
#[test]
fn diffs_and_their_application_match_the_oracle() {
    let chain = Chain::main();
    let lists = lists();
    for oracle in Oracle::all() {
        let (mut applied, mut states) = (0, 0);
        for (from_name, from, from_bytes) in &lists {
            for (to_name, to, to_bytes) in &lists {
                let what = format!("{from_name} -> {to_name}");
                let diff = from.diff_to(to);
                let ours = canonical_diff(&diff);
                let core = oracle
                    .mnlist_diff(&chain, ListEncoding::Canonical, from_bytes, to_bytes)
                    .unwrap_or_else(|e| panic!("{what}: {e}"));
                assert_eq!(ours, core.canonical, "{what}: canonical diff");
                let imported = core_disk::decode_diff_d4(&mut core.disk.as_slice()).expect(&what);
                assert_eq!(canonical_diff(&imported), ours, "{what}: the evodb diff");
                let decoded = decode_canonical_diff(
                    &mut ours.as_slice(),
                    &mut { usize::MAX },
                    core_net_info(),
                )
                .expect(&what);

                let block = BlockHash::from([0x5a; 32]);
                let mut ours = from.clone();
                let ours = ours.apply_diff(block, to.height(), &decoded).map(|()| ours);
                let core = oracle.mnlist_apply_diff(
                    &chain,
                    ListEncoding::Canonical,
                    from_bytes,
                    ListEncoding::Canonical,
                    &core.canonical,
                    block.as_bytes(),
                    to.height(),
                );
                match (ours, core) {
                    (Ok(list), Ok(core)) => {
                        assert_eq!(canonical(&list), core.canonical, "{what}: applied");
                        assert_eq!(&core.hash, canonical_mn_list_hash(&list).as_bytes());
                        applied += 1;
                    }
                    (Err(_), Err(e)) => assert_eq!(e.kind, ErrorKind::Exception, "{what}: {e}"),
                    (ours, core) => panic!("{what}: ours {ours:?}, Core {core:?}"),
                }

                for b in to.iter() {
                    let Some(a) = from.get(&b.pro_tx_hash) else {
                        continue;
                    };
                    let state = MnStateDiff::between(a, b);
                    let core = oracle
                        .state_diff(&state_bytes(a), &state_bytes(b))
                        .unwrap_or_else(|e| panic!("{what}: {e}"));
                    assert_eq!(state_diff_bytes(&state), core.diff, "{what}: state diff");
                    let (hot, cold) = state.apply(&a.hot, &a.cold);
                    let mut after = (**a).clone();
                    after.hot = hot;
                    after.cold = cold;
                    assert_eq!(state_bytes(&after), core.applied, "{what}: applied state");
                    states += 1;
                }
            }
        }
        assert!(applied >= lists.len() && states > 20, "applied {applied}, states {states}");
    }
}

/// Splitmix64: deterministic, so a failing history is reproducible from its index.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Core's `ForEachMN` order after random histories of additions, updates and removals over keys
/// that share their 64-bit hash (collision buckets: newest first, and an update moves its key to
/// the front) is our `core_order()`, and the canonical bytes are ours.
#[test]
fn core_order_matches_the_oracle_over_histories() {
    let chain = Chain::main();
    let oracle = Oracle::new(dash_oracle_sys::Pin::V24_0);
    // Eight masternodes in two hash classes of three and two single ones.
    let key = |n: u8| {
        let mut bytes = [n; 32];
        bytes[..8].copy_from_slice(
            &match n {
                1..=3 => 0x0101_0101_0101_0101u64,
                4..=6 => 0x8000_0000_0000_0002u64,
                _ => u64::from(n) << 56,
            }
            .to_le_bytes(),
        );
        ProTxHash::from(bytes)
    };
    let mn = |n: u8, id: u64| {
        dmn_with(
            n,
            if n.is_multiple_of(2) {
                MnType::Evo
            } else {
                MnType::Regular
            },
            |d, _| {
                d.pro_tx_hash = key(n);
                d.internal_id = id;
            },
        )
    };
    let mut seed = 0x0e72_a000_u64;
    for history in 0..200 {
        let mut ours = MnList::new(BlockHash::from([0; 32]), 0, 0);
        let mut ops = Vec::new();
        for step in 0..24 {
            let n = (next(&mut seed) % 8) as u8 + 1;
            let pro_tx = key(n);
            match ours.get(&pro_tx).cloned() {
                None => {
                    // Without a bump the new member takes a free id below the counter, as a
                    // reloaded list's members do; with none free it must bump.
                    let total = u64::from(ours.total_registered());
                    let free = (0..total).find(|id| ours.iter().all(|d| d.internal_id != *id));
                    let bump = free.is_none() || !next(&mut seed).is_multiple_of(4);
                    let id = if bump {
                        total
                    } else {
                        free.expect("free id")
                    };
                    let dmn = mn(n, id);
                    ours.try_add(Arc::clone(&dmn), bump).expect("addable");
                    ops.push(ListOp::Add {
                        dmn: dmn_bytes(&dmn),
                        bump_total_count: bump,
                    });
                }
                Some(_) if next(&mut seed).is_multiple_of(3) => {
                    ours.remove(&pro_tx).expect("removable");
                    ops.push(ListOp::Remove {
                        pro_tx_hash: *pro_tx.as_bytes(),
                    });
                }
                Some(old) => {
                    let mut hot = old.hot;
                    hot.last_paid_height = step + 1;
                    ours.update_hot(&pro_tx, hot).expect("updatable");
                    let new = ours.get(&pro_tx).expect("present");
                    ops.push(ListOp::Update {
                        pro_tx_hash: *pro_tx.as_bytes(),
                        state: state_bytes(new),
                    });
                }
            }
        }
        let core = oracle
            .mnlist_core_order(&chain, &ops)
            .unwrap_or_else(|e| panic!("history {history}: {e}"));
        let order: Vec<[u8; 32]> = ours.core_order().iter().map(|p| *p.as_bytes()).collect();
        assert_eq!(order, core.order, "history {history}: Core's iteration order");
        // The position is the oracle's empty list's; the rest is the list's own.
        assert_eq!(canonical(&ours)[36..], core.canonical[36..], "history {history}");
    }
}

/// Our unique-property keys are the bytes Core's `GetUniquePropertyHash` hashes, for every
/// property type and the collision classes EV2 §2.2 names (quirk 23).
#[test]
fn unique_property_keys_match_the_oracle() {
    let oracle = Oracle::new(dash_oracle_sys::Pin::V24_0);
    let check = |label: &str, kind: UniqueProperty, value: &[u8], ours: UniqueKey| {
        let core =
            oracle.unique_property_hash(kind, value).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(ours.as_bytes(), core.preimage.as_slice(), "{label}");
    };
    let mut value = (1..=32).collect::<Vec<u8>>();
    value.extend_from_slice(&1u32.to_le_bytes());
    let outpoint = OutPoint {
        hash: TxHash::from(<[u8; 32]>::try_from(&value[..32]).unwrap()),
        index: 1,
    };
    check("collateral", UniqueProperty::Collateral, &value, UniqueKey::collateral(&outpoint));
    let null = OutPoint {
        hash: TxHash::from([0; 32]),
        index: u32::MAX,
    };
    let null_bytes = [vec![0; 32], vec![0xff; 4]].concat();
    check("collateral_null", UniqueProperty::Collateral, &null_bytes, UniqueKey::collateral(&null));

    let id: [u8; 20] = std::array::from_fn(|i| 0x11 + i as u8);
    check("key_id", UniqueProperty::KeyId, &id, UniqueKey::key_id(&KeyId::from(id)));
    let platform = UniqueKey::platform_node_id(&PlatformNodeId::from(id));
    check("platform_node_id", UniqueProperty::PlatformNodeId, &id, platform);

    let addrv2 = |net: u8, addr: &[u8], port: u16| {
        let mut out = vec![net, addr.len() as u8];
        out.extend_from_slice(addr);
        out.extend_from_slice(&port.to_be_bytes());
        out
    };
    let ipv6 = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    let mut cjdns = [0u8; 16];
    cjdns[0] = 0xfc;
    let tor: [u8; 32] = std::array::from_fn(|i| i as u8 + 1);
    let i2p: [u8; 32] = std::array::from_fn(|i| i as u8 + 33);
    for (label, addr, net, raw, port) in [
        ("service_ipv4", AddrV2::Ipv4([1, 2, 3, 4]), 1, vec![1, 2, 3, 4], 9999),
        ("service_ipv6", AddrV2::Ipv6(ipv6), 2, ipv6.to_vec(), 9999),
        ("service_torv3", AddrV2::TorV3(tor), 4, tor.to_vec(), 9999),
        ("service_i2p", AddrV2::I2p(i2p), 5, i2p.to_vec(), 0),
        ("service_cjdns", AddrV2::Cjdns(cjdns), 6, cjdns.to_vec(), 9999),
    ] {
        let service = ServiceV2 {
            addr,
            port,
        };
        check(
            label,
            UniqueProperty::Service,
            &addrv2(net, &raw, port),
            UniqueKey::service(&service),
        );
    }
    let mut domain = vec![16u8];
    domain.extend_from_slice(b"evo5.example.org");
    domain.extend_from_slice(&443u16.to_be_bytes());
    check("domain", UniqueProperty::Domain, &domain, UniqueKey::domain(b"evo5.example.org", 443));

    // Core's evo_simplifiedmns_tests' first legacy key and its basic encoding; the legacy bytes
    // read under the basic flag; the default key; bytes no scheme decodes.
    let legacy = unhex(
        "81fc56c400756e2f053ea015d7a61606f668a10f5ee0808f86df9443778021b802174b1d9cdb0cde3c3a4fe5803d02b5",
    );
    let mut basic = legacy.clone();
    basic[0] = 0x80 | (legacy[0] & 0x80) >> 2 | legacy[0] & 0x1f;
    for (label, bytes, is_legacy) in [
        ("operator_key_legacy", legacy.clone(), true),
        ("operator_key_basic", basic, false),
        ("operator_key_legacy_bytes_basic_flag", legacy, false),
        ("operator_key_null", vec![0; 48], true),
        ("operator_key_undecodable", vec![0xff; 48], true),
    ] {
        let scheme = BlsSchemeKind::from_legacy(is_legacy);
        let key = OperatorKey::from_bytes(
            &<[u8; 48]>::try_from(bytes.as_slice()).unwrap(),
            BlsSchemeSource::Version(scheme),
        );
        let kind = UniqueProperty::OperatorKey {
            legacy: is_legacy,
        };
        check(label, kind, &bytes, UniqueKey::operator_key(&key));
    }
}

/// Full mainnet lists imported from Core's evodb (`DASH_DML_CORE_LISTS`, the file
/// `dash-dml`'s `imported_lists_match_core_at_full_size` reads): the oracle reads each and
/// gives our canonical bytes and hash.
#[test]
#[ignore = "needs DASH_DML_CORE_LISTS"]
#[allow(clippy::disallowed_methods, reason = "the environment names the input file")]
fn imported_mainnet_lists_match_the_oracle() {
    let chain = Chain::main();
    let oracle = Oracle::new(dash_oracle_sys::Pin::V24_0);
    let path = std::env::var("DASH_DML_CORE_LISTS").expect("DASH_DML_CORE_LISTS");
    let text = std::fs::read_to_string(path).expect("lists");
    let mut checked = 0;
    for line in text.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let disk = unhex(f[0]);
        let imported = core_disk::decode_list_s3(&mut disk.as_slice()).expect("evodb list");
        let core =
            oracle.mnlist_canonical(&chain, ListEncoding::Disk, &disk).expect("Core reads it");
        assert_eq!(canonical(&imported), core.canonical, "list {checked}");
        assert_eq!(&core.hash, canonical_mn_list_hash(&imported).as_bytes(), "list {checked}");
        let read = oracle
            .mnlist_canonical(&chain, ListEncoding::Canonical, &core.canonical)
            .expect("Core's strict reader accepts it");
        assert_eq!(read.hash, core.hash);
        checked += 1;
    }
    assert!(checked > 0);
    println!("{checked} imported mainnet lists match the oracle");
}

/// Every EV2a fuzz target agrees with Core on each of its public seeds
/// (`node-fuzz/public-seeds`, written by the `dml_seeds` example), and each compares more than
/// it skips.
#[test]
fn fuzz_targets_agree_on_their_public_seeds() {
    // The diff target's base lists are all of Core's.
    assert_eq!(node_l1::dml::core_lists().len(), lists().len());
    let seeds = concat!(env!("CARGO_MANIFEST_DIR"), "/../node-fuzz/public-seeds");
    for target in TARGETS
        .iter()
        .filter(|t| t.name.starts_with("diff_mnlist_") || t.name == "diff_unique_hash")
    {
        for &pin in target.pins {
            let mut outcomes = std::collections::BTreeMap::<String, usize>::new();
            let dir = format!("{seeds}/{}", target.name);
            for entry in std::fs::read_dir(&dir).expect(&dir) {
                let path = entry.expect("entry").path();
                let input = std::fs::read(&path).expect("seed");
                let outcome = (target.compare)(&input, pin).unwrap_or_else(|d| {
                    panic!("{}: {} diverges: {d}", target.name, path.display())
                });
                *outcomes.entry(format!("{outcome:?}")).or_default() += 1;
            }
            eprintln!("{} {:?}: {outcomes:?}", target.name, pin);
            let count = |o: Outcome| outcomes.get(&format!("{o:?}")).copied().unwrap_or(0);
            assert!(
                count(Outcome::BothAccept) + count(Outcome::BothReject)
                    > count(Outcome::OutOfDomain),
                "{}: {outcomes:?}",
                target.name
            );
        }
    }
}

/// `list` rebuilt member by member in `order` (each `AddMN(dmn, false)`, as Core's evodb reader
/// does) under `total` registrations, with `change` applied to each member.
fn rebuilt(
    list: &MnList,
    order: &[ProTxHash],
    total: u32,
    change: impl Fn(&mut dash_dml::Dmn),
) -> MnList {
    let mut out = MnList::new(*list.block_hash(), list.height(), total);
    for pro_tx in order {
        let mut dmn = (**list.get(pro_tx).expect("a member")).clone();
        change(&mut dmn);
        out.try_add(Arc::new(dmn), false).expect("addable");
    }
    out
}

/// The list comparison sees every part of Core's evodb output: Core's outputs for `collide`
/// with the evodb bytes of a list that differs only in its counter, one internal id, or its
/// collision bucket's order (the same members, canonical bytes and hash, so `IsEqual` holds) are
/// refused for the evodb list.
#[test]
fn list_outputs_guard_the_counter_ids_and_order() {
    let chain = Chain::main();
    let oracle = Oracle::new(dash_oracle_sys::Pin::V24_0);
    let (_, list, bytes) = lists().into_iter().find(|(n, ..)| n == "collide").expect("collide");
    let core = oracle.mnlist_canonical(&chain, ListEncoding::Canonical, &bytes).expect("Core");
    assert_eq!(compare_list_outputs(&list, &core), Ok(()));
    // Core's evodb reader reverses the collision bucket of the bytes Core wrote, and ours too.
    let reread = oracle.mnlist_canonical(&chain, ListEncoding::Disk, &core.disk).expect("Core");
    assert_ne!(reread.disk, core.disk);
    let imported = core_disk::decode_list_s3(&mut core.disk.as_slice()).expect("ours");
    assert_eq!(compare_list_outputs(&imported, &reread), Ok(()));

    // Rebuilt newest first, a bucket comes back in Core's order, so the rebuild is faithful;
    // rebuilt in Core's order (as an evodb reader does), every bucket is reversed.
    let order: Vec<ProTxHash> = list.core_order().to_vec();
    let total = list.total_registered();
    let mut newest_first = order.clone();
    newest_first.reverse();
    assert_eq!(list_disk_bytes(&rebuilt(&list, &newest_first, total, |_| {})), core.disk);
    let first = order[0];
    let perturbed = [
        ("counter", rebuilt(&list, &newest_first, total + 1, |_| {})),
        (
            "internal id",
            // An unused id above the counter, which `AddMN(dmn, false)` admits as Core's does.
            rebuilt(&list, &newest_first, total, |d| {
                if d.pro_tx_hash == first {
                    d.internal_id = u64::from(total) + 5;
                }
            }),
        ),
        ("collision order", rebuilt(&list, &order, total, |_| {})),
    ];
    for (what, other) in perturbed {
        let disk = list_disk_bytes(&other);
        assert_ne!(disk, core.disk, "{what}: the perturbation shows in the evodb bytes");
        if what == "collision order" {
            assert!(other.is_equal(&list), "IsEqual cannot see it");
            assert_eq!(canonical(&other), core.canonical, "nor can the canonical bytes");
        }
        let altered = dash_oracle_sys::CanonicalList {
            disk,
            ..core.clone()
        };
        let field = compare_list_outputs(&list, &altered).map_err(|d| d.field);
        assert_eq!(field, Err("evodb list"), "{what}");
    }
}

/// Core VARINT of a small id, CompactSize of a small count.
fn small(n: u64) -> u8 {
    assert!(n < 0x80, "small enough for one byte");
    n as u8
}

/// The diff comparison sees the evodb bytes as written: Core's `BuildDiff` outputs with the
/// evodb row's updates in another order (which our evodb reader, sorting by id, would read back
/// to the same diff) are refused for the evodb diff.
#[test]
fn diff_outputs_guard_the_update_order() {
    let chain = Chain::main();
    let oracle = Oracle::new(dash_oracle_sys::Pin::V24_0);
    let lists = lists();
    let (from, to, diff) = lists
        .iter()
        .flat_map(|a| lists.iter().map(move |b| (a, b)))
        .map(|(a, b)| (a, b, a.1.diff_to(&b.1)))
        .find(|(_, _, d)| d.updated.len() >= 2 && d.updated.iter().all(|(id, _)| *id < 0x80))
        .expect("a pair with two updates");
    let core = oracle
        .mnlist_diff(&chain, ListEncoding::Canonical, &from.2, &to.2)
        .expect("Core's BuildDiff");
    assert_eq!(compare_diff_outputs(&diff, &core), Ok(()));

    let write = |updated: &[(u64, MnStateDiff)]| {
        let mut added = diff.added.clone();
        added.sort_by_key(|d| (d.internal_id, d.pro_tx_hash));
        let mut out = vec![small(added.len() as u64)];
        added.iter().for_each(|d| out.extend(dmn_bytes(d)));
        out.push(small(updated.len() as u64));
        for (id, state) in updated {
            out.push(small(*id));
            out.extend(state_diff_bytes(state));
        }
        out.push(small(diff.removed.len() as u64));
        diff.removed.iter().for_each(|id| out.push(small(*id)));
        out
    };
    let mut sorted = diff.updated.clone();
    sorted.sort_by_key(|(id, _)| *id);
    assert_eq!(write(&sorted), core.disk, "the hand-written row is Core's in Core's order");
    sorted.reverse();
    let reordered = write(&sorted);
    assert_ne!(reordered, core.disk);
    let read = core_disk::decode_diff_d4(&mut reordered.as_slice()).expect("readable");
    assert_eq!(canonical_diff(&read), core.disk, "a read-back normalizes the order away");
    let altered = dash_oracle_sys::ListDiff {
        disk: reordered,
        ..core.clone()
    };
    assert_eq!(compare_diff_outputs(&diff, &altered).map_err(|d| d.field), Err("evodb diff"));
}

/// The unique-key comparison checks Core's hash itself: an altered hash with Core's pre-image
/// is refused for the hash.
#[test]
fn unique_outputs_guard_the_hash() {
    let oracle = Oracle::new(dash_oracle_sys::Pin::V24_0);
    let id = [0x11u8; 20];
    let core = oracle.unique_property_hash(UniqueProperty::KeyId, &id).expect("Core");
    let ours = UniqueKey::key_id(&KeyId::from(id));
    assert_eq!(compare_unique_outputs(&ours, &core), Ok(()));
    let mut altered = core.clone();
    altered.hash[31] ^= 1;
    assert_eq!(compare_unique_outputs(&ours, &altered).map_err(|d| d.field), Err("hash"));
}

/// Each `diff_unique_hash` seed names the property it was written for (the legacy scheme flag in
/// bit 7 included), is committed, and is accepted by both sides.
#[test]
fn unique_seeds_reach_their_property() {
    use bitcoin_hashes::sha256;
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../node-fuzz/public-seeds/diff_unique_hash");
    let seeds = unique_seeds();
    assert!(seeds.iter().any(|(p, _)| *p
        == UniqueProperty::OperatorKey {
            legacy: true
        }));
    for (property, input) in seeds {
        assert_eq!(unique_property(input[0]), property, "{}", hex(&input));
        let name = hex(&sha256::Hash::hash(&input).to_byte_array()[..8]);
        assert_eq!(std::fs::read(format!("{dir}/{name}")).ok(), Some(input.clone()), "{name}");
        let outcome = compare_unique_hash(&input, dash_oracle_sys::Pin::V24_0);
        assert_eq!(outcome, Ok(Outcome::BothAccept), "{}", hex(&input));
    }
}
