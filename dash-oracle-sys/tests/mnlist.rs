//! Smoke differential tests of the masternode-list entry points (ABI 8, node-program
//! O2a-EV): each against what Dash Core's own unit tests compute, recorded by Core
//! (dash-core-vectors `transcribed/evo_snapshot_tests.json`), and the committed
//! unique-property vectors (`golden/unique-property-hash.json`).

mod common;

use bitcoin_hashes::{Hash, hash160, sha256d};
use common::{hex, to_hex, transcribed};
use dash_oracle_sys::{
    BlsKind, BlsMode, Chain, ErrorKind, ListEncoding, ListOp, Oracle, UniqueProperty,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One record of `transcribed/evo_snapshot_tests.json` (see test-vectors' PROVENANCE.md).
#[derive(Deserialize)]
struct SnapshotRecord {
    kind: String,
    case: String,
    /// list: Core's evodb bytes
    disk: Option<String>,
    /// list, read: `SerializeCanonicalMNList` bytes
    canonical: Option<String>,
    /// list, read: `CanonicalMNListHash` (internal byte order)
    hash: Option<String>,
    /// read: the bytes given to `UnserializeCanonicalMNList`
    input: Option<String>,
    accepted: Option<bool>,
    trailing: Option<u64>,
    error: Option<String>,
    /// diff: the lists, canonical, and `SerializeCanonicalMNListDiff(from.BuildDiff(to))`
    from: Option<String>,
    to: Option<String>,
    /// diff, state_diff: Core's serialization of the diff
    diff: Option<String>,
    /// state_diff: the states (SER_DISK)
    old: Option<String>,
    new: Option<String>,
}

/// evo_snapshot_tests run on mainnet parameters (`BasicTestingSetup`): their masternodes use
/// mainnet's default port, which the canonical reader's netinfo validation checks.
fn core_tests_chain() -> Chain {
    Chain::main()
}

fn snapshot_records(kind: &str) -> Vec<SnapshotRecord> {
    let records: Vec<SnapshotRecord> = transcribed::<SnapshotRecord>("evo_snapshot_tests.json")
        .into_iter()
        .filter(|r| r.kind == kind)
        .collect();
    assert!(!records.is_empty(), "no {kind} records");
    records
}

/// A read record's refusal at the pin. The records come from the rc.3 tree; since rc.4 Core's
/// `ExtNetInfo` reader refuses a repeated or out-of-order purpose itself (netinfo.h:431-433),
/// before the canonical-encoding check, and nested_mn_map_wire_order_is_canonical expects that
/// message for the two netinfo cases (evo_snapshot_tests.cpp:1019-1021). Records made at rc.4
/// carry it themselves, and this override must then go.
// Interim until test-vectors/core is re-transcribed at rc.4 (`regen-core-vectors.sh`), which
// base-sdk's R6 re-pin does not do (DEC-472): the assert below trips on the first rc.4 record.
fn core_error(record: &SnapshotRecord) -> Option<&str> {
    match record.case.as_str() {
        "netinfo_reversed_entries" | "netinfo_duplicate_entry" => {
            assert_eq!(
                record.error.as_deref(),
                Some("noncanonical MN object encoding: iostream error"),
                "{}: not the rc.3 record; drop the override",
                record.case
            );
            Some("ExtNetInfo: purpose unknown or out of order: iostream error")
        }
        _ => record.error.as_deref(),
    }
}

fn field<'a>(record: &'a SnapshotRecord, value: &'a Option<String>, name: &str) -> &'a str {
    value.as_deref().unwrap_or_else(|| panic!("{}: no {name}", record.case))
}

/// The lists the tests build: from Core's evodb bytes the oracle writes Core's canonical
/// bytes and hash; the canonical bytes read back to themselves (except the pre-DIP0003
/// default list, whose height -1 the reader refuses).
#[test]
fn canonical_lists_match_core_tests() {
    let chain = core_tests_chain();
    for oracle in Oracle::all() {
        for record in snapshot_records("list") {
            let canonical = hex(field(&record, &record.canonical, "canonical"));
            let hash = field(&record, &record.hash, "hash");
            let from_disk = oracle
                .mnlist_canonical(
                    &chain,
                    ListEncoding::Disk,
                    &hex(field(&record, &record.disk, "disk")),
                )
                .unwrap_or_else(|e| panic!("{} {}: {e}", oracle.pin(), record.case));
            assert_eq!(to_hex(&from_disk.canonical), to_hex(&canonical), "{}", record.case);
            assert_eq!(to_hex(&from_disk.hash), hash, "{}", record.case);
            assert_eq!(
                sha256d::Hash::hash(&canonical).to_byte_array(),
                from_disk.hash,
                "{}",
                record.case
            );

            let read = oracle.mnlist_canonical(&chain, ListEncoding::Canonical, &canonical);
            if record.case == "default_list" {
                let e = read.expect_err("the reader refuses height -1");
                assert_eq!(e.kind, ErrorKind::Decode);
                assert_eq!(e.message, "negative canonical MN-list height: iostream error");
                continue;
            }
            let read = read.unwrap_or_else(|e| panic!("{}: {e}", record.case));
            assert_eq!(read.canonical, canonical, "{}", record.case);
            assert_eq!(to_hex(&read.hash), hash, "{}", record.case);
            // Core's evodb bytes of the list it read decode to the same list again.
            let again = oracle.mnlist_canonical(&chain, ListEncoding::Disk, &read.disk).unwrap();
            assert_eq!(again.canonical, canonical, "{}", record.case);
        }
    }
}

/// Every byte string the tests give Core's canonical reader: the same verdict and, when
/// accepted, the same re-encoding and hash; a refusal carries Core's message.
#[test]
fn canonical_reads_match_core_tests() {
    let chain = core_tests_chain();
    for oracle in Oracle::all() {
        let (mut accepted, mut refused) = (0, 0);
        for record in snapshot_records("read") {
            let input = hex(field(&record, &record.input, "input"));
            let result = oracle.mnlist_canonical(&chain, ListEncoding::Canonical, &input);
            if record.accepted == Some(true) {
                assert_eq!(record.trailing, Some(0), "{}", record.case);
                let list = result.unwrap_or_else(|e| panic!("{}: {e}", record.case));
                assert_eq!(to_hex(&list.canonical), field(&record, &record.canonical, "canonical"));
                assert_eq!(to_hex(&list.hash), field(&record, &record.hash, "hash"));
                accepted += 1;
            } else {
                let e = result.expect_err(&record.case);
                assert_eq!(e.kind, ErrorKind::Decode, "{}: {e}", record.case);
                assert_eq!(Some(e.message.as_str()), core_error(&record), "{}", record.case);
                refused += 1;
            }
        }
        assert!(accepted >= 1 && refused >= 7, "{accepted} accepted, {refused} refused");
    }
}

/// The oracle's own refusals: bytes after a list, an unknown encoding never reaches Core.
#[test]
fn canonical_list_arguments() {
    let chain = core_tests_chain();
    let record =
        snapshot_records("list").into_iter().find(|r| r.case == "synthetic_forward").unwrap();
    for oracle in Oracle::all() {
        let mut canonical = hex(record.canonical.as_deref().unwrap());
        canonical.push(0);
        let e = oracle.mnlist_canonical(&chain, ListEncoding::Canonical, &canonical).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Trailing, "{e}");
        let e =
            oracle.mnlist_canonical(&chain, ListEncoding::Canonical, &canonical[..40]).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Decode, "{e}");
        let mut disk = disk_list(5, &[]);
        disk.push(0);
        let e = oracle.mnlist_canonical(&chain, ListEncoding::Disk, &disk).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Trailing, "{e}");
    }
}

/// `golden/unique-property-hash.json`: values of every property type and class EV2 §2.2
/// names, with the bytes Core hashes and `GetUniquePropertyHash`, from the oracle.
#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct UniqueRecord {
    label: String,
    kind: String,
    legacy: bool,
    value: String,
    preimage: String,
    hash: String,
}

#[derive(Serialize, Deserialize)]
struct UniqueVectors {
    source: String,
    records: Vec<UniqueRecord>,
}

const VECTORS: &str = "golden/unique-property-hash.json";

fn kind_name(kind: UniqueProperty) -> &'static str {
    match kind {
        UniqueProperty::Collateral => "collateral",
        UniqueProperty::Service => "service",
        UniqueProperty::Domain => "domain",
        UniqueProperty::KeyId => "key_id",
        UniqueProperty::PlatformNodeId => "platform_node_id",
        UniqueProperty::OperatorKey {
            ..
        } => "operator_key",
    }
}

fn kind_of(name: &str, legacy: bool) -> UniqueProperty {
    match name {
        "collateral" => UniqueProperty::Collateral,
        "service" => UniqueProperty::Service,
        "domain" => UniqueProperty::Domain,
        "key_id" => UniqueProperty::KeyId,
        "platform_node_id" => UniqueProperty::PlatformNodeId,
        "operator_key" => UniqueProperty::OperatorKey {
            legacy,
        },
        other => panic!("unknown kind {other}"),
    }
}

/// ADDRv2 serialization: network id, CompactSize address length (< 253 here), address,
/// big-endian port.
fn addrv2(net: u8, addr: &[u8], port: u16) -> Vec<u8> {
    let mut out = vec![net, addr.len() as u8];
    out.extend_from_slice(addr);
    out.extend_from_slice(&port.to_be_bytes());
    out
}

fn domain(name: &str, port: u16) -> Vec<u8> {
    let mut out = vec![name.len() as u8];
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&port.to_be_bytes());
    out
}

/// A legacy-encoded BLS point in the basic encoding: the sign moves from bit 7 (legacy) to
/// bit 5 and the compression bit 7 is set; a signature (G2) also writes x's two Fp2 halves in
/// the other order (the flags stay on byte 0). Core decodes both to one point.
fn basic_encoding(legacy: &[u8]) -> Vec<u8> {
    let mut basic = match legacy.len() {
        48 => legacy.to_vec(),
        96 => [&legacy[48..], &legacy[..48]].concat(),
        n => panic!("{n}-byte BLS point"),
    };
    basic[0] = 0x80 | (legacy[0] & 0x80) >> 2 | basic[0] & 0x1f;
    if basic.len() == 96 {
        basic[48] = legacy[0] & 0x1f;
    }
    basic
}

/// The vector inputs. The operator key is the first legacy key of Core's
/// evo_simplifiedmns_tests (transcribed), in both encodings.
fn unique_inputs(oracle: &Oracle) -> Vec<(&'static str, UniqueProperty, Vec<u8>)> {
    let legacy_key = hex(
        "81fc56c400756e2f053ea015d7a61606f668a10f5ee0808f86df9443778021b802174b1d9cdb0cde3c3a4fe5803d02b5",
    );
    let basic_key = basic_encoding(&legacy_key);
    for (legacy, key) in [(true, &legacy_key), (false, &basic_key)] {
        let decoded = oracle.bls_decode(BlsKind::PublicKey, BlsMode::Eager, legacy, key).unwrap();
        assert!(
            decoded.valid && &decoded.reserialized == key,
            "Core decodes and keeps the key (legacy {legacy})"
        );
    }
    let bytes = |range: std::ops::RangeInclusive<u8>| range.collect::<Vec<u8>>();
    let mut collateral = bytes(1..=32);
    collateral.extend_from_slice(&1u32.to_le_bytes());
    let mut cjdns = vec![0xfc];
    cjdns.extend(bytes(1..=15));
    let mut v4_mapped = vec![0; 10];
    v4_mapped.extend([0xff, 0xff, 1, 2, 3, 4]);
    let key_id = bytes(0x11..=0x24);
    use UniqueProperty::*;
    vec![
        ("collateral", Collateral, collateral),
        ("collateral_null", Collateral, [vec![0; 32], vec![0xff; 4]].concat()),
        ("service_ipv4", Service, addrv2(1, &[1, 2, 3, 4], 9999)),
        (
            "service_ipv6",
            Service,
            addrv2(2, &[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], 9999),
        ),
        ("service_ipv6_v4_mapped", Service, addrv2(2, &v4_mapped, 9999)),
        ("service_null", Service, addrv2(2, &[0; 16], 0)),
        ("service_torv3", Service, addrv2(4, &bytes(1..=32), 9999)),
        ("service_i2p", Service, addrv2(5, &bytes(33..=64), 0)),
        ("service_cjdns", Service, addrv2(6, &cjdns, 9999)),
        ("service_unknown_network", Service, addrv2(0x2a, &[1, 2, 3, 4], 9999)),
        ("domain", Domain, domain("evo5.example.org", 443)),
        ("domain_null", Domain, domain("", 0)),
        ("key_id", KeyId, key_id.clone()),
        ("platform_node_id_same_bytes", PlatformNodeId, key_id),
        ("key_id_null", KeyId, vec![0; 20]),
        (
            "operator_key_legacy",
            OperatorKey {
                legacy: true,
            },
            legacy_key.clone(),
        ),
        (
            "operator_key_basic",
            OperatorKey {
                legacy: false,
            },
            basic_key,
        ),
        (
            "operator_key_legacy_bytes_basic_flag",
            OperatorKey {
                legacy: false,
            },
            legacy_key,
        ),
        (
            "operator_key_null",
            OperatorKey {
                legacy: true,
            },
            vec![0; 48],
        ),
        (
            "operator_key_undecodable",
            OperatorKey {
                legacy: true,
            },
            vec![0xff; 48],
        ),
    ]
}

fn unique_records(oracle: &Oracle) -> Vec<UniqueRecord> {
    unique_inputs(oracle)
        .into_iter()
        .map(|(label, kind, value)| {
            let got = oracle
                .unique_property_hash(kind, &value)
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            UniqueRecord {
                label: label.into(),
                kind: kind_name(kind).into(),
                legacy: matches!(
                    kind,
                    UniqueProperty::OperatorKey {
                        legacy: true
                    }
                ),
                value: to_hex(&value),
                preimage: to_hex(&got.preimage),
                hash: to_hex(&got.hash),
            }
        })
        .collect()
}

/// The committed vectors are what the oracle computes now (`DASH_ORACLE_REGENERATE=1`
/// rewrites them), every hash is SHA256d of its preimage, and the preimages have the
/// layouts EV2 §2.2 keys by.
#[test]
fn unique_property_vectors() {
    let oracle = Oracle::new(dash_oracle_sys::Pin::V24_0);
    let records = unique_records(&oracle);
    if std::env::var("DASH_ORACLE_REGENERATE").is_ok_and(|v| v == "1") {
        let vectors = UniqueVectors {
            source: format!(
                "oracle_unique_property_hash (GetUniquePropertyHash), Core {}",
                oracle.pin()
            ),
            records,
        };
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../test-vectors/golden/unique-property-hash.json"
        );
        std::fs::write(path, serde_json::to_string_pretty(&vectors).unwrap() + "\n").unwrap();
        return;
    }
    let committed: UniqueVectors =
        serde_json::from_str(&dash_core_vectors::read_vector_text(VECTORS)).unwrap();
    assert_eq!(committed.records, records, "the oracle no longer computes the committed vectors");

    let by_label = |label: &str| records.iter().find(|r| r.label == label).unwrap();
    for r in &records {
        let preimage = hex(&r.preimage);
        assert_eq!(to_hex(&sha256d::Hash::hash(&preimage).to_byte_array()), r.hash, "{}", r.label);
        // Every value but a service is hashed as given, so its hash is SHA256d of the bytes
        // unique_inputs built, whatever the oracle computes.
        if r.kind != "service" {
            assert_eq!(r.preimage, r.value, "{}", r.label);
        }
        let again =
            oracle.unique_property_hash(kind_of(&r.kind, r.legacy), &hex(&r.value)).unwrap();
        assert_eq!(to_hex(&again.hash), r.hash);
    }
    // ADDRv1-compatible services hash their 18-byte ADDRv1 form, the others their ADDRv2 form.
    assert_eq!(by_label("service_ipv4").preimage, "00000000000000000000ffff01020304270f");
    assert_eq!(by_label("service_ipv6").preimage.len(), 36);
    for label in ["service_torv3", "service_i2p", "service_cjdns"] {
        assert_eq!(by_label(label).preimage, by_label(label).value, "{label}");
    }
    // Quirk 23: a key id and a platformNodeID with equal bytes share a slot; one point in two
    // encodings does not; the scheme a lazy key is read under does not change its bytes.
    assert_eq!(by_label("key_id").hash, by_label("platform_node_id_same_bytes").hash);
    assert_ne!(by_label("operator_key_legacy").hash, by_label("operator_key_basic").hash);
    assert_eq!(
        by_label("operator_key_legacy").hash,
        by_label("operator_key_legacy_bytes_basic_flag").hash
    );
}

#[test]
fn unique_property_arguments() {
    for oracle in Oracle::all() {
        let e = oracle.unique_property_hash(UniqueProperty::KeyId, &[0; 21]).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Trailing, "{e}");
        let e = oracle.unique_property_hash(UniqueProperty::Collateral, &[0; 35]).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Decode, "{e}");
        let e = oracle
            .unique_property_hash(UniqueProperty::Service, &addrv2(1, &[1, 2, 3], 1))
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Decode, "{e}");
    }
}

fn snapshot_record(kind: &str, case: &str) -> SnapshotRecord {
    snapshot_records(kind)
        .into_iter()
        .find(|r| r.case == case)
        .unwrap_or_else(|| panic!("no {case}"))
}

/// The state diff the tests build. Their states are ProTx version 3, which does not serialize
/// `scriptPayout`, yet the test's in-memory states differ in it, so Core's diff of them carries
/// a field (bit 0x40 of the VARINT field set, after netInfo) that the serialized states cannot
/// show: the oracle's diff of the bytes is Core's diff without it, and applying it to the old
/// state gives the new one.
#[test]
fn state_diff_matches_core_tests() {
    for oracle in Oracle::all() {
        let record = snapshot_record("state_diff", "evo_extaddr_address_change");
        let new = hex(field(&record, &record.new, "new"));
        let got = oracle.state_diff(&hex(field(&record, &record.old, "old")), &new).unwrap();
        let recorded = field(&record, &record.diff, "diff");
        // Field set VARINT 0x84f6 (0x2f6 + 0x80 continuation) loses 0x40 and the script
        // OP_RETURN [5, 1] that follows netInfo.
        let without_script_payout =
            recorded.replacen("84f6", "84b6", 1).replacen("01bb046a020501", "01bb", 1);
        assert_eq!(to_hex(&got.diff), without_script_payout);
        assert_eq!(got.applied, new);

        let unchanged = oracle.state_diff(&new, &new).unwrap();
        assert_eq!(unchanged.diff, [0], "no field set: VARINT(0)");
        let e = oracle.state_diff(&new, &new[..new.len() - 1]).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Decode, "{e}");
    }
}

/// `BuildDiff` of the tests' lists is Core's canonical diff (also the evodb bytes at v24),
/// and `ApplyDiff` of it at block H(11), height 99, rebuilds the list Core reconstructed.
#[test]
fn list_diff_matches_core_tests() {
    let chain = core_tests_chain();
    let record = snapshot_record("diff", "diff_chain");
    let (from, to) =
        (hex(field(&record, &record.from, "from")), hex(field(&record, &record.to, "to")));
    let diff = hex(field(&record, &record.diff, "diff"));
    let reconstructed = snapshot_record("list", "diff_chain_reconstructed");
    let mut block_hash = [0u8; 32];
    block_hash[0] = 11;
    for oracle in Oracle::all() {
        let got = oracle.mnlist_diff(&chain, ListEncoding::Canonical, &from, &to).unwrap();
        assert_eq!(got.canonical, diff);
        assert_eq!(got.disk, diff);
        for encoding in [ListEncoding::Canonical, ListEncoding::Disk] {
            let applied = oracle
                .mnlist_apply_diff(
                    &chain,
                    ListEncoding::Canonical,
                    &from,
                    encoding,
                    &diff,
                    &block_hash,
                    99,
                )
                .unwrap();
            assert_eq!(
                to_hex(&applied.canonical),
                field(&reconstructed, &reconstructed.canonical, "canonical")
            );
            assert_eq!(to_hex(&applied.hash), field(&reconstructed, &reconstructed.hash, "hash"));
        }
        // The diff adds a masternode the target already has: AddMN's duplicate check throws.
        let e = oracle
            .mnlist_apply_diff(
                &chain,
                ListEncoding::Canonical,
                &to,
                ListEncoding::Canonical,
                &diff,
                &block_hash,
                99,
            )
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Exception, "{e}");
        let e = oracle
            .mnlist_apply_diff(
                &chain,
                ListEncoding::Canonical,
                &from,
                ListEncoding::Canonical,
                &diff[1..],
                &block_hash,
                99,
            )
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Decode, "{e}");
        // A disk diff adding a masternode with the reserved internal id (AddMN asserts on it).
        let state = state_bytes(&StateFields {
            version: 2,
            registered: 1,
            last_paid: 0,
            consecutive: 0,
            confirmed: &[0; 32],
            owner: [1; 20],
            operator: &[0; 48],
            voting: &[2; 20],
            service: &[0; 18],
        });
        let mut reserved = vec![1];
        reserved.extend(dmn_bytes(0, &[7; 32], &state, 0));
        reserved.splice(33..34, varint(u64::MAX));
        reserved.extend([0, 0]);
        let e = oracle
            .mnlist_apply_diff(
                &chain,
                ListEncoding::Canonical,
                &from,
                ListEncoding::Disk,
                &reserved,
                &block_hash,
                99,
            )
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Argument, "{e}");
    }
}

/// Core's VARINT (serialize.h WriteVarInt): base 128, most significant group first, each
/// group but the last offset by one.
fn varint(mut n: u64) -> Vec<u8> {
    let mut out = vec![(n & 0x7f) as u8];
    while n > 0x7f {
        n = (n >> 7) - 1;
        out.push((n & 0x7f) as u8 | 0x80);
    }
    out.reverse();
    out
}

/// The masternodes of a canonical list of the tests (each proTxHash is `CollidingH`:
/// 8 bytes 0xa5, a suffix, zeros), split at their proTxHashes: (suffix, CDeterministicMN
/// bytes). The lists hold fewer than 253 masternodes.
fn test_mns(canonical: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let starts: Vec<usize> = (41..canonical.len().saturating_sub(31))
        .filter(|&i| canonical[i..i + 8] == [0xa5; 8] && canonical[i + 9..i + 32] == [0; 23])
        .collect();
    assert_eq!(starts.len(), canonical[40] as usize, "one proTxHash per masternode");
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| {
            let end = starts.get(n + 1).copied().unwrap_or(canonical.len());
            (canonical[start + 8], canonical[start..end].to_vec())
        })
        .collect()
}

/// A `CDeterministicMN`'s state: after proTxHash, a one-byte VARINT internal id, the
/// collateral and the operator reward, before nType.
fn state_of(dmn: &[u8]) -> &[u8] {
    &dmn[32 + 1 + 36 + 2..dmn.len() - 2]
}

/// Core's order inside one 64-bit collision bucket (dash-mnlist-champ-order): newest first,
/// an update moves a masternode to the front, a removal leaves the rest in place.
#[test]
fn core_order_of_colliding_masternodes() {
    let chain = core_tests_chain();
    let base =
        test_mns(&hex(snapshot_record("list", "diff_chain_base").canonical.as_deref().unwrap()));
    let target =
        test_mns(&hex(snapshot_record("list", "diff_chain_target").canonical.as_deref().unwrap()));
    let dmn = |mns: &[(u8, Vec<u8>)], suffix: u8| {
        mns.iter().find(|(s, _)| *s == suffix).unwrap().1.clone()
    };
    let pro_tx_hash = |suffix: u8| -> [u8; 32] { dmn(&base, suffix)[..32].try_into().unwrap() };
    let add = |dmn: Vec<u8>| ListOp::Add {
        dmn,
        bump_total_count: true,
    };
    // Suffixes 1, 2, 3 are the base list's; 8 is the masternode the target adds.
    let mut ops: Vec<ListOp> = [1, 2, 3].map(|s| add(dmn(&base, s))).into();
    ops.push(add(dmn(&target, 8)));
    let suffixes = |ops: &[ListOp], oracle: &Oracle| -> Vec<u8> {
        let got = oracle.mnlist_core_order(&chain, ops).unwrap();
        let mut sorted = got.order.clone();
        sorted.sort();
        let canonical = test_mns(&got.canonical);
        assert_eq!(
            canonical.iter().map(|(_, d)| d[..32].to_vec()).collect::<Vec<_>>(),
            sorted.iter().map(|h| h.to_vec()).collect::<Vec<_>>(),
            "canonical bytes are sorted"
        );
        got.order.iter().map(|h| h[8]).collect()
    };
    for oracle in Oracle::all() {
        let mut ops = ops.clone();
        assert_eq!(suffixes(&ops, &oracle), [8, 3, 2, 1]);
        ops.push(ListOp::Update {
            pro_tx_hash: pro_tx_hash(2),
            state: state_of(&dmn(&target, 2)).to_vec(),
        });
        assert_eq!(suffixes(&ops, &oracle), [2, 8, 3, 1]);
        ops.push(ListOp::Remove {
            pro_tx_hash: pro_tx_hash(3),
        });
        assert_eq!(suffixes(&ops, &oracle), [2, 8, 1]);
        ops.push(add(dmn(&base, 3)));
        assert_eq!(suffixes(&ops, &oracle), [3, 2, 8, 1]);

        // Core's throws name the operation; a malformed payload is the caller's error.
        ops.push(add(dmn(&base, 3)));
        let e = oracle.mnlist_core_order(&chain, &ops).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Exception, "{e}");
        assert!(e.message.starts_with("operation 7: "), "{e}");
        let e = oracle
            .mnlist_core_order(
                &chain,
                &[ListOp::Remove {
                    pro_tx_hash: [7; 32],
                }],
            )
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Exception, "{e}");
        let mut short = dmn(&base, 1);
        short.pop();
        let e = oracle
            .mnlist_core_order(
                &chain,
                &[ListOp::Add {
                    dmn: short,
                    bump_total_count: true,
                }],
            )
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Argument, "{e}");
    }
}

/// Core's transcribed simplified-list entries (`evo_simplifiedmns_tests.json`): their wire
/// form, `CalcHash` and the root of the 15 (display order in the record).
fn sml_records() -> (Vec<Value>, String) {
    let records: Vec<Value> = transcribed::<Value>("evo_simplifiedmns_tests.json");
    let root = records.iter().find(|r| r["kind"] == "merkle_root").unwrap()["merkle_root"]
        .as_str()
        .unwrap()
        .into();
    (records.into_iter().filter(|r| r["kind"] == "entry").collect(), root)
}

fn display(hash: &[u8; 32]) -> String {
    let mut reversed = *hash;
    reversed.reverse();
    to_hex(&reversed)
}

/// A masternode as evodb stores it: proTxHash, VARINT internal id (< 128), a collateral
/// of its own, no operator reward, the state, nType.
fn dmn_bytes(id: u8, pro_tx_hash: &[u8], state: &[u8], mn_type: u16) -> Vec<u8> {
    let mut out = pro_tx_hash.to_vec();
    out.push(id);
    out.extend([id + 1; 32]);
    out.extend(0u32.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend_from_slice(state);
    out.extend(mn_type.to_le_bytes());
    out
}

/// The fields of a version 1 (legacy BLS) or 2 state that matter here, the rest null.
struct StateFields<'a> {
    version: i32,
    registered: i32,
    last_paid: i32,
    consecutive: i32,
    confirmed: &'a [u8],
    owner: [u8; 20],
    operator: &'a [u8],
    voting: &'a [u8],
    /// ADDRv1 CService: 16-byte address, big-endian port.
    service: &'a [u8],
}

/// `CDeterministicMNState` (SER_DISK) for versions 1 and 2: the integers, revocation reason,
/// confirmed hashes, keys, MnNetInfo, empty payout scripts, platformNodeID and ports.
fn state_bytes(f: &StateFields) -> Vec<u8> {
    let mut out = Vec::new();
    for v in [f.version, f.registered, f.last_paid, f.consecutive, 0, -1, -1] {
        out.extend(v.to_le_bytes());
    }
    out.extend(0u16.to_le_bytes());
    out.extend_from_slice(f.confirmed);
    out.extend([0; 32]);
    out.extend(f.owner);
    out.extend_from_slice(f.operator);
    out.extend_from_slice(f.voting);
    out.extend_from_slice(f.service);
    out.extend([0, 0]);
    out.extend([0; 20]);
    out.extend([0; 4]);
    out
}

/// A list as evodb stores it: null block hash, height, registered count, the masternodes.
fn disk_list(height: i32, mns: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0; 32];
    out.extend(height.to_le_bytes());
    out.extend((mns.len() as u32).to_le_bytes());
    out.push(mns.len() as u8);
    for mn in mns {
        out.extend_from_slice(mn);
    }
    out
}

/// The 15 masternodes behind Core's transcribed entries (version 1), read from their wire
/// form: version, proRegTxHash, confirmedHash, service, operator key, voting key, isValid.
fn sml_test_mns() -> Vec<Vec<u8>> {
    let (entries, _) = sml_records();
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let wire = hex(e["wire"].as_str().unwrap());
            assert_eq!(wire.len(), 2 + 32 + 32 + 18 + 48 + 20 + 1);
            let state = state_bytes(&StateFields {
                version: 1,
                registered: 1,
                last_paid: 0,
                consecutive: 0,
                confirmed: &wire[34..66],
                owner: [i as u8 + 1; 20],
                operator: &wire[84..132],
                voting: &wire[132..152],
                service: &wire[66..84],
            });
            dmn_bytes(i as u8, &wire[2..34], &state, 0)
        })
        .collect()
}

/// Core's entries: each masternode's simplified entry and `CalcHash`, and the list's
/// `merkleRootMNList`, equal what evo_simplifiedmns_tests computed.
#[test]
fn sml_matches_core_tests() {
    let (entries, root) = sml_records();
    let mns = sml_test_mns();
    for oracle in Oracle::all() {
        for (record, dmn) in entries.iter().zip(&mns) {
            let (wire, hash) = oracle.sml_entry_hash(dmn).unwrap();
            assert_eq!(to_hex(&wire), record["wire"].as_str().unwrap());
            assert_eq!(display(&hash), record["entry_hash"].as_str().unwrap());
        }
        let list = disk_list(100, &mns);
        let got = oracle.sml_merkle_root(&Chain::main(), ListEncoding::Disk, &list).unwrap();
        assert!(got.verdict.is_accept());
        assert_eq!(display(&got.root), root);
        // CSimplifiedMNList sorts by proRegTxHash (memcmp of the internal bytes).
        let mut sorted: Vec<&Value> = entries.iter().collect();
        sorted.sort_by_key(|e| hex(e["wire"].as_str().unwrap())[2..34].to_vec());
        let leaves: Vec<&str> = sorted.iter().map(|e| e["entry_hash"].as_str().unwrap()).collect();
        assert_eq!(got.entry_hashes.iter().map(display).collect::<Vec<_>>(), leaves);

        let mut trailing = mns[0].clone();
        trailing.push(0);
        assert_eq!(oracle.sml_entry_hash(&trailing).unwrap_err().kind, ErrorKind::Trailing);
        assert_eq!(oracle.sml_entry_hash(&mns[0][..40]).unwrap_err().kind, ErrorKind::Decode);
    }
}

/// The stateful merkle-root call runs once: a forced `ORACLE_E_BUFFER` retry gets the kept
/// output, equal to a call whose buffer fits, and only for the same call (another list's
/// call drops it). A second run would only show for a mutated tree (Core's cached reason),
/// which unique proRegTxHashes rule out, so this checks the served output and its key.
#[test]
fn sml_merkle_root_buffer_retry_runs_once() {
    let mns = sml_test_mns();
    let lists = [disk_list(100, &mns), disk_list(100, &mns[..7])];
    for oracle in Oracle::all() {
        let fitted: Vec<_> = lists
            .iter()
            .map(|l| oracle.sml_merkle_root(&Chain::main(), ListEncoding::Disk, l).unwrap())
            .collect();
        assert_ne!(fitted[0].root, fitted[1].root);
        for _ in 0..2 {
            for (list, fitted) in lists.iter().zip(&fitted) {
                let retried = oracle
                    .sml_merkle_root_sized(&Chain::main(), ListEncoding::Disk, list, 0)
                    .unwrap();
                assert_eq!(&retried, fitted);
            }
        }
    }
}

/// A regular masternode R (last paid 1) and an Evo E paid at the list's height 100 with one
/// consecutive payment: while V19 is active and MN_RR is not, Core pays E again and
/// projects `E E E R E`; with MN_RR it pays the least recently paid, R.
#[test]
fn payee_follows_the_deployments() {
    let chain = Chain::new(
        "regtest",
        &[
            "-dip3params=20:30",
            "-testactivationheight=v19@50",
            "-testactivationheight=v20@150",
            "-testactivationheight=mn_rr@200",
        ],
    );
    let legacy_key = hex(
        "81fc56c400756e2f053ea015d7a61606f668a10f5ee0808f86df9443778021b802174b1d9cdb0cde3c3a4fe5803d02b5",
    );
    let basic_key = basic_encoding(&legacy_key);
    let service = |n: u8| {
        let mut s = vec![0; 10];
        s.extend([0xff, 0xff, 1, 1, 1, n, 0x4e, 0x1f]);
        s
    };
    let mn = |id: u8, version: i32, mn_type: u16, last_paid: i32, consecutive: i32, key: &[u8]| {
        let state = state_bytes(&StateFields {
            version,
            registered: 10,
            last_paid,
            consecutive,
            confirmed: &[id; 32],
            owner: [id; 20],
            operator: key,
            voting: &[id; 20],
            service: &service(id),
        });
        dmn_bytes(id, &[id; 32], &state, mn_type)
    };
    let regular = mn(1, 1, 0, 1, 0, &legacy_key);
    let list = |consecutive: i32| {
        disk_list(100, &[regular.clone(), mn(2, 2, 1, 100, consecutive, &basic_key)])
    };
    let (r, e) = ([1u8; 32], [2u8; 32]);
    for oracle in Oracle::all() {
        let payee =
            |prev: i32| oracle.mnlist_payee(&chain, ListEncoding::Disk, &list(1), prev).unwrap();
        assert_eq!(payee(100), Some(e), "V19, no MN_RR: the Evo node again");
        assert_eq!(payee(199), Some(r), "MN_RR: the least recently paid");
        assert_eq!(payee(10), Some(r), "before V19: the least recently paid");
        let projected =
            oracle.mnlist_projected(&chain, ListEncoding::Disk, &list(1), 100, 9).unwrap();
        assert_eq!(projected, [e, e, e, r, e], "clamped to the weighted count 5");
        assert_eq!(
            oracle.mnlist_projected(&chain, ListEncoding::Disk, &list(1), 199, 9).unwrap(),
            [r, e]
        );
        assert!(
            oracle
                .mnlist_projected(&chain, ListEncoding::Disk, &list(1), 100, -1)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            oracle.mnlist_projected(&chain, ListEncoding::Disk, &list(1), 100, 2).unwrap()[0],
            payee(100).unwrap()
        );

        // Negative consecutive payments: Core would repeat E 4 - c times before MN_RR.
        let e_arg =
            oracle.mnlist_projected(&chain, ListEncoding::Disk, &list(-1), 100, 9).unwrap_err();
        assert_eq!(e_arg.kind, ErrorKind::Argument, "{e_arg}");
        assert_eq!(
            oracle.mnlist_projected(&chain, ListEncoding::Disk, &list(-1), 199, 9).unwrap(),
            [r, e]
        );
        assert_eq!(
            oracle.mnlist_payee(&chain, ListEncoding::Disk, &list(1), -1).unwrap_err().kind,
            ErrorKind::Argument
        );
        assert_eq!(
            oracle.mnlist_payee(&chain, ListEncoding::Disk, &disk_list(100, &[]), 100).unwrap(),
            None
        );
    }
}

/// `special-tx-signatures.json`: provider transactions with the keys their Core-made
/// signatures verify under (tests/data/PROVENANCE.md).
#[derive(Deserialize)]
struct SignedTx {
    txid: String,
    network: String,
    #[serde(rename = "type")]
    tx_type: u16,
    raw: String,
    signer: Value,
}

/// The key ids whose compact ECDSA signatures in `raw` (65-byte windows with a header
/// byte 27-34) Core-verify over `hash`: `RecoverCompact`, then Hash160 of the key.
fn recovered_key_ids(raw: &[u8], hash: &[u8; 32]) -> Vec<String> {
    use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
    let message = secp256k1::Message::from_digest(*hash);
    raw.windows(65)
        .filter(|w| (27..=34).contains(&w[0]))
        .filter_map(|w| {
            let header = i32::from(w[0]) - 27;
            let sig =
                RecoverableSignature::from_compact(&w[1..], RecoveryId::try_from(header & 3).ok()?)
                    .ok()?;
            let key = sig.recover(message).ok()?;
            let bytes = if header & 4 != 0 {
                key.serialize().to_vec()
            } else {
                key.serialize_uncompressed().to_vec()
            };
            Some(to_hex(&hash160::Hash::hash(&bytes).to_byte_array()))
        })
        .collect()
}

/// Core-made signatures of every provider type verify over the oracle's sign hashes: ECDSA
/// signatures recover to their signers' key ids over the hash Core checks for that type
/// (ProRegTx: the sign-string message, or the consent hash when shared; ProDisTx: its sign
/// hash; the others: the payload hash), BLS operator signatures (the payload's last 96
/// bytes) verify over the payload hash.
#[test]
fn special_tx_sign_hashes_verify_core_signatures() {
    let records: Vec<SignedTx> = common::load_records("special-tx-signatures.json");
    for oracle in Oracle::all() {
        let mut checked = std::collections::BTreeMap::<u16, usize>::new();
        for r in &records {
            let chain = if r.network == "mainnet" {
                Chain::main()
            } else {
                Chain::regtest()
            };
            let raw = hex(&r.raw);
            let got = oracle
                .special_tx_sign_hash(&chain, &raw)
                .unwrap_or_else(|e| panic!("{}: {e}", r.txid));
            if let Some(operator) = r.signer["operator"].as_str() {
                let sig: [u8; 96] = raw[raw.len() - 96..].try_into().unwrap();
                let key: [u8; 48] = hex(operator).try_into().unwrap();
                // The record's flag is the masternode's key encoding; the payload's version
                // picks the signature's and the height (V19) the scheme Core verifies under, so
                // after V19 a legacy key (and a legacy signature) verify as basic.
                let legacy = r.signer["operator_legacy"].as_bool().unwrap();
                let verifies = |legacy: bool, key: &[u8], sig: &[u8]| {
                    oracle
                        .bls_verify(
                            legacy,
                            key.try_into().unwrap(),
                            sig.try_into().unwrap(),
                            &got.payload_hash,
                        )
                        .unwrap_or(false)
                };
                let signed = verifies(legacy, &key, &sig)
                    || legacy
                        && [sig.to_vec(), basic_encoding(&sig)]
                            .iter()
                            .any(|sig| verifies(false, &basic_encoding(&key), sig));
                assert!(signed, "{}", r.txid);
            } else {
                let signed = match (r.tx_type, &got.sign_message, got.consent_hash) {
                    (1, _, Some(consent)) | (10, _, Some(consent)) => consent,
                    (1, Some((_, message)), None) => *message,
                    (3 | 11 | 12, None, None) => got.payload_hash,
                    other => panic!("{}: unexpected hashes {other:?}", r.txid),
                };
                let recovered = recovered_key_ids(&raw, &signed);
                match r.signer["key_ids"].as_array() {
                    // The record lists every share owner of a shared masternode: its ProUpRegTx
                    // carries the shareIndex owner's signature, a ProDisTx the actor's alone or
                    // every owner's. Elsewhere each listed key signed.
                    Some(ids) => {
                        let signed = ids
                            .iter()
                            .filter(|id| recovered.iter().any(|k| Some(k.as_str()) == id.as_str()))
                            .count();
                        let unilateral = matches!(r.tx_type, 10 | 11) && signed == 1;
                        assert!(
                            unilateral || signed == ids.len(),
                            "{}: {signed} of {ids:?}",
                            r.txid
                        );
                    }
                    // Shared registrations: the share owners, whose key ids the payload's payout
                    // scripts hold (any 65-byte window recovers to some key, so count only
                    // those).
                    None => {
                        let owners = |ids: Vec<String>| {
                            ids.into_iter().filter(|id| r.raw.contains(id.as_str())).count()
                        };
                        assert!(owners(recovered) >= 2, "{}", r.txid);
                        let wrong = recovered_key_ids(&raw, &got.payload_hash);
                        assert_eq!(owners(wrong), 0, "{}: the check bites", r.txid);
                    }
                }
            }
            *checked.entry(r.tx_type).or_default() += 1;
        }
        assert_eq!(checked.keys().copied().collect::<Vec<_>>(), [1, 2, 3, 4, 10, 11, 12]);
        assert_eq!(checked.values().sum::<usize>(), 117);

        let coinbase = &records[0].raw;
        let mut not_protx = hex(coinbase);
        not_protx[2] = 5;
        let e = oracle.special_tx_sign_hash(&Chain::regtest(), &not_protx).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Argument, "{e}");
    }
}
