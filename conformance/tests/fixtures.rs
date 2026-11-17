//! End-to-end checks of the harness on real testnet blocks captured from a dashd v24 oracle:
//! clean blocks must produce no mismatches, and each injected fault must be caught and
//! attributed to the right side.

use dash_conformance::check::{BlockJob, Context, OracleBlock, check_block};
use dash_conformance::report::{INFORMATIONAL_CHECKS, Mismatch, Side, Stats};
use dash_conformance::split::split_block;
use dash_conformance::verbose;
use serde_json::Value;

struct Fixture {
    bytes: Vec<u8>,
    oracle: OracleBlock,
    verbose: Value,
}

fn load_named(name: &str) -> Fixture {
    let path = format!("{}/tests/data/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let v: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    Fixture {
        bytes: hex_conservative::decode_to_vec(v["hex"].as_str().unwrap()).unwrap(),
        oracle: OracleBlock::from_json(&v["getblock1"]).unwrap(),
        verbose: v["getblock2"].clone(),
    }
}

fn load(height: u32) -> Fixture {
    load_named(&format!("testnet-{height}"))
}

fn run_on(
    net: &str,
    bytes: &[u8],
    oracle: Option<&OracleBlock>,
    verbose: Option<&Value>,
) -> (Vec<Mismatch>, Stats) {
    let mut stats = Stats::default();
    let job = BlockJob {
        bytes,
        height: oracle.map(|o| o.height),
        oracle,
        verbose,
        ..Default::default()
    };
    let ctx = Context {
        net: net.into(),
        source: "test",
    };
    let m = check_block(&ctx, &job, &mut stats);
    (m, stats)
}

fn run(
    bytes: &[u8],
    oracle: Option<&OracleBlock>,
    verbose: Option<&Value>,
) -> (Vec<Mismatch>, Stats) {
    run_on("testnet", bytes, oracle, verbose)
}

/// Runs a regtest fixture with a modified `getblock 2` view.
fn run_regtest(f: &Fixture, verbose: &Value) -> Vec<Mismatch> {
    run_on("regtest", &f.bytes, Some(&f.oracle), Some(verbose)).0
}

/// Whether `m` has a finding of `check` against `side` for `field`.
fn has_field(m: &[Mismatch], check: &str, side: Side, field: &str) -> bool {
    m.iter().any(|x| x.check == check && x.side == side && x.field.as_deref() == Some(field))
}

/// The index of the first transaction whose `getblock 2` view has `key`.
fn tx_with(verbose: &Value, key: &str) -> usize {
    verbose["tx"].as_array().unwrap().iter().position(|t| t.get(key).is_some()).unwrap()
}

fn has(m: &[Mismatch], check: &str, side: Side) -> bool {
    m.iter().any(|x| x.check == check && x.side == side)
}

/// Every scalar leaf (or empty container) path of a JSON value.
fn leaf_paths(v: &Value, prefix: String, out: &mut Vec<String>) {
    match v {
        Value::Object(m) if !m.is_empty() => {
            for (k, v) in m {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                leaf_paths(v, p, out);
            }
        }
        Value::Array(a) if !a.is_empty() => {
            for (i, v) in a.iter().enumerate() {
                leaf_paths(v, format!("{prefix}[{i}]"), out);
            }
        }
        _ => out.push(prefix),
    }
}

/// A JSON pointer for a dotted path with `[i]` indices.
fn pointer(path: &str) -> String {
    path.split('.')
        .flat_map(|part| part.split('['))
        .map(|seg| format!("/{}", seg.trim_end_matches(']')))
        .collect()
}

#[test]
fn clean_blocks_have_no_mismatches() {
    for h in [7000, 7090] {
        let f = load(h);
        let (m, stats) = run(&f.bytes, Some(&f.oracle), Some(&f.verbose));
        assert!(m.is_empty(), "height {h}: {m:#?}");
        // Every leaf of the oracle's JSON is classified (so none can be silently skipped).
        for tx in f.verbose["tx"].as_array().unwrap() {
            let mut paths = Vec::new();
            leaf_paths(tx, String::new(), &mut paths);
            for p in paths {
                let leaf = tx.pointer(&pointer(&p)).unwrap();
                let class = verbose::classify_leaf(&p, leaf);
                assert!(class.is_some(), "height {h}: {p} is unclassified");
            }
        }
        assert_eq!(stats.blocks, 1);
        assert_eq!(stats.verbose_blocks, 1);
        assert_eq!(stats.oracle_hash_checks, 1);
        assert_eq!(stats.txs, f.oracle.txids.len() as u64);
    }
    let (_, stats) = run(&load(7090).bytes, None, None);
    assert_eq!(stats.special_txs.get(&1), Some(&1));
    assert_eq!(stats.special_txs.get(&5), Some(&1));
}

#[test]
fn oracle_disagreements_are_attributed_to_both_libraries() {
    let f = load(7090);
    let mut oracle = f.oracle.clone();
    oracle.hash[0] ^= 1;
    oracle.txids[2][0] ^= 1;
    oracle.time += 1;
    let (m, _) = run(&f.bytes, Some(&oracle), None);
    assert!(has(&m, "block.hash", Side::Both), "{m:#?}");
    assert!(has(&m, "tx.txid", Side::Both));
    assert!(has(&m, "tx.txid_ref", Side::Harness), "the reference rule disagrees too");
    assert!(m.iter().any(|x| x.check == "header.field" && x.field.as_deref() == Some("time")));
    let bad_tx = m.iter().find(|x| x.check == "tx.txid").unwrap();
    assert_eq!(bad_tx.tx_index, Some(2));
    assert!(bad_tx.tx_hex.is_some());
}

#[test]
fn verbose_json_fields_are_compared() {
    let f = load(7090);
    let protx = f.verbose["tx"].as_array().unwrap().iter().position(|t| t["type"] == 1).unwrap();
    let mut verbose = f.verbose.clone();
    verbose["tx"][protx]["proRegTx"]["collateralIndex"] = 4.into();
    verbose["tx"][0]["cbTx"]["height"] = 1.into();
    verbose["tx"][1]["qcTx"]["commitment"]["llmqType"] = 99.into();
    verbose["tx"][1]["qcTx"]["commitment"]["signers"] = "01".into();
    // a derivable oracle value that no library emits (e.g. a field added by a new version)
    verbose["tx"][0]["cbTx"]["merkleRootAssetUnlocks"] = "00".into();
    // vout entries the libraries do not have
    let vouts = verbose["tx"][0]["vout"].as_array_mut().unwrap();
    let extra_vout = format!("vout[{}].valueSat", vouts.len());
    vouts.push(serde_json::json!({"valueSat": 1}));
    let (m, _) = run(&f.bytes, Some(&f.oracle), Some(&verbose));
    let found = |check: &str, field: &str| {
        m.iter()
            .any(|x| x.check == check && x.side == Side::Both && x.field.as_deref() == Some(field))
    };
    for field in [
        "proRegTx.collateralIndex",
        "cbTx.height",
        "qcTx.commitment.llmqType",
        "qcTx.commitment.signers",
    ] {
        assert!(found("verbose.field", field), "{field}: {m:#?}");
    }
    for field in ["cbTx.merkleRootAssetUnlocks", extra_vout.as_str()] {
        for side in [Side::Dashcore, Side::BaseSdk] {
            assert!(
                m.iter().any(|x| x.check == "verbose.missing"
                    && x.side == side
                    && x.field.as_deref() == Some(field)),
                "{field} {side:?}: {m:#?}"
            );
        }
    }
    // values a library emits that the oracle lacks are reported too
    let protx = &mut verbose["tx"].as_array_mut().unwrap()[protx];
    protx["proRegTx"].as_object_mut().unwrap().remove("inputsHash");
    let (m, _) = run(&f.bytes, Some(&f.oracle), Some(&verbose));
    let inputs_hash = m.iter().find(|x| x.field.as_deref() == Some("proRegTx.inputsHash"));
    let inputs_hash = inputs_hash.expect("reported");
    assert_eq!((inputs_hash.side, inputs_hash.expected.as_deref()), (Side::Both, Some("<absent>")));
}

/// Rewrites the `nType` of transaction `index` (Core-style: high 16 bits of the version).
fn set_tx_type(bytes: &mut [u8], index: usize, ty: u16) {
    let spans = split_block(bytes).unwrap();
    let at = spans.txs[index].range.start + 2;
    bytes[at..at + 2].copy_from_slice(&ty.to_le_bytes());
}

/// A payload a library keeps as raw bytes is reported against it as `payload.unsupported`:
/// the transaction is in a valid chain, so dashd interprets the payload. Both libraries read
/// the payload by its length, so the block and its other transactions still decode.
#[test]
fn unknown_special_type_is_unsupported_by_both_libraries() {
    let f = load(7090);
    let protx = split_block(&f.bytes).unwrap().txs.iter().position(|t| t.tx_type == 1).unwrap();
    // Type 13 is unknown to both libraries (and to Core v24). Type 10 (ProDisTx) is known to
    // both, but a ProRegTx payload does not decode as one: dashcore keeps it as raw bytes, and
    // base-sdk reports the decode failure Core's `GetTxPayload` would also give.
    for (ty, base_sdk) in [(13, "payload.unsupported"), (10, "payload.decode")] {
        let mut bytes = f.bytes.clone();
        set_tx_type(&mut bytes, protx, ty);
        let (m, _) = run(&bytes, None, None);
        for (side, check) in [(Side::Dashcore, "payload.unsupported"), (Side::BaseSdk, base_sdk)] {
            let found = m.iter().find(|x| x.check == check && x.side == side);
            let found = found.unwrap_or_else(|| panic!("type {ty}: {m:#?}"));
            assert_eq!(found.tx_index, Some(protx));
        }
        // Nothing else, apart from the header's merkle root, which the new txid breaks (and
        // base-sdk's `Block::check()` therefore rejects), for the libraries and for the
        // dash_consensus leg, which keeps such a payload as bytes. Regenerated at the
        // BS6a pin by running this test (both types report all of these but
        // `payload.decode`, which type 10 adds).
        let expected = [
            "payload.unsupported",
            "payload.decode",
            "block.merkle_root",
            "block.check",
            "consensus.merkle_root",
        ];
        let other = m.iter().filter(|x| !expected.contains(&x.check.as_str()));
        assert_eq!(other.count(), 0, "type {ty}: {m:#?}");
    }
}

#[test]
fn payload_length_prefix_is_enforced() {
    // Append one byte inside the ProRegTx's vExtraPayload and bump its length prefix: Core
    // reads the payload by length (and would then reject the trailing byte in CheckProRegTx,
    // but the wire format itself stays aligned).
    let f = load(7090);
    let spans = split_block(&f.bytes).unwrap();
    let span = spans.txs.iter().find(|t| t.tx_type == 1).unwrap();
    let payload = span.payload.clone().unwrap();
    let compact = |n: usize| match n {
        0..0xfd => vec![n as u8],
        _ => [&[0xfd][..], &(n as u16).to_le_bytes()].concat(),
    };
    let len = payload.len();
    let mut bytes = f.bytes[..payload.start - compact(len).len()].to_vec();
    bytes.extend_from_slice(&compact(len + 1));
    bytes.extend_from_slice(&f.bytes[payload.clone()]);
    bytes.push(0xee);
    bytes.extend_from_slice(&f.bytes[payload.end..]);
    let (m, _) = run(&bytes, None, None);
    assert!(split_block(&bytes).is_ok());
    // dashcore keeps a payload it cannot consume exactly as raw bytes, as Core's
    // `GetTxPayload` refuses one; base-sdk fails its decode.
    assert!(!has(&m, "block.decode", Side::Dashcore), "{m:#?}");
    assert!(has(&m, "payload.unsupported", Side::Dashcore), "{m:#?}");
    assert!(has(&m, "payload.decode", Side::BaseSdk), "{m:#?}");
}

/// BS2 review r1 finding 4: wrong oracle endpoints and owner/voting addresses on a pre-v24 Evo
/// ProRegTx (legacy netinfo, so dashd derives the platform arrays from the payload's ports).
/// Both libraries model all of these, so every change is reported against both.
#[test]
fn wrong_endpoints_and_key_addresses_are_reported() {
    let f = load_named("regtest-feature_protx_version-201");
    assert!(run_regtest(&f, &f.verbose).is_empty());
    let protx = tx_with(&f.verbose, "proRegTx");
    let mut verbose = f.verbose.clone();
    let p = &mut verbose["tx"][protx]["proRegTx"];
    p["addresses"] = serde_json::json!({
        "core_p2p": ["127.0.0.1:1"], "platform_p2p": ["127.0.0.1:2"], "platform_https": ["127.0.0.1:3"]
    });
    p["ownerAddress"] = "wrong".into();
    p["votingAddress"] = "wrong".into();
    let m = run_regtest(&f, &verbose);
    for field in [
        "addresses.core_p2p[0]",
        "addresses.platform_p2p[0]",
        "addresses.platform_https[0]",
        "ownerAddress",
        "votingAddress",
    ] {
        let field = format!("proRegTx.{field}");
        assert!(has_field(&m, "verbose.field", Side::Both, &field), "{field}: {m:#?}");
    }
    assert_eq!(m.len(), 5, "{m:#?}");
}

/// `service` and the platform ports exist only with -deprecatedrpc=service; when the oracle
/// prints them they are compared like any other field.
#[test]
fn deprecated_service_fields_are_compared_when_present() {
    let f = load_named("regtest-feature_protx_version-201");
    let protx = tx_with(&f.verbose, "proRegTx");
    let mut verbose = f.verbose.clone();
    let p = &mut verbose["tx"][protx]["proRegTx"];
    p["service"] = p["addresses"]["core_p2p"][0].clone();
    p["platformP2PPort"] = 12774.into();
    p["platformHTTPPort"] = 12775.into();
    assert!(run_regtest(&f, &verbose).is_empty());
    let p = &mut verbose["tx"][protx]["proRegTx"];
    p["service"] = "127.0.0.1:1".into();
    p["platformHTTPPort"] = 1.into();
    let m = run_regtest(&f, &verbose);
    for field in ["proRegTx.service", "proRegTx.platformHTTPPort"] {
        assert!(has_field(&m, "verbose.field", Side::Both, field), "{field}: {m:#?}");
    }
    assert_eq!(m.len(), 2, "{m:#?}");
    // A port the oracle prints is compared even without `service` next to it.
    let p = &mut verbose["tx"][protx]["proRegTx"];
    p.as_object_mut().unwrap().remove("service");
    let m = run_regtest(&f, &verbose);
    assert!(has_field(&m, "verbose.field", Side::Both, "proRegTx.platformHTTPPort"), "{m:#?}");
    assert_eq!(m.len(), 1, "{m:#?}");
}

/// BS2 review r1 finding 4: an oracle object the harness does not know fails the gate instead
/// of disappearing.
#[test]
fn unknown_oracle_objects_are_harness_gaps() {
    assert!(INFORMATIONAL_CHECKS.iter().all(|c| !["harness-gap", "verbose.render"].contains(c)));
    let f = load(7090);
    let mut verbose = f.verbose.clone();
    verbose["tx"][0]["newPayload"] = serde_json::json!({"height": 9001});
    verbose["tx"][0]["emptyPayload"] = serde_json::json!({});
    verbose["tx"][1]["qcTx"]["commitment"]["newField"] = "00".into();
    let (m, _) = run(&f.bytes, Some(&f.oracle), Some(&verbose));
    for (tx, field) in
        [(0, "newPayload.height"), (0, "emptyPayload"), (1, "qcTx.commitment.newField")]
    {
        let gap = m.iter().find(|x| x.check == "harness-gap" && x.field.as_deref() == Some(field));
        let gap = gap.unwrap_or_else(|| panic!("{field}: {m:#?}"));
        assert_eq!((gap.side, gap.tx_index), (Side::Harness, Some(tx)));
        assert!(!gap.is_informational());
    }
    assert_eq!(m.len(), 3, "{m:#?}");
}

/// The share fields dashd prints for a shared ProRegTx are covered: amounts and scripts are
/// class A, compared with both libraries' decoded shares, and the share addresses are checked
/// as renderings of the oracle's scripts.
#[test]
fn share_fields_are_compared_or_reported() {
    let f = load_named("regtest-feature_masternode_shares-273");
    let base = run_regtest(&f, &f.verbose);
    let protx = tx_with(&f.verbose, "proRegTx");
    let mut verbose = f.verbose.clone();
    let share = &mut verbose["tx"][protx]["proRegTx"]["shares"][0];
    share["amount"] = 1.into();
    share["refundScript"] = "76a914000000000000000000000000000000000000000088ac".into();
    share["rewardScript"] = "a914000000000000000000000000000000000000000087".into();
    let m = run_regtest(&f, &verbose);
    let found = |m: &[Mismatch], field: &str| {
        m.iter()
            .find(|x| x.check == "verbose.field" && x.field.as_deref() == Some(field))
            .map(|x| (x.side, x.expected.clone()))
    };
    for (field, new) in [
        ("amount", "1"),
        ("refundScript", "76a914000000000000000000000000000000000000000088ac"),
        ("rewardScript", "a914000000000000000000000000000000000000000087"),
    ] {
        let field = format!("proRegTx.shares[0].{field}");
        assert_eq!(found(&base, &field), None, "{field}");
        assert_eq!(found(&m, &field), Some((Side::Both, Some(new.to_string()))), "{field}");
    }
    // The oracle's addresses no longer match its scripts.
    for field in ["proRegTx.shares[0].refundAddress", "proRegTx.shares[0].rewardAddress"] {
        assert!(has_field(&m, "verbose.render", Side::Harness, field), "{field}: {m:#?}");
    }
    assert!(!m.iter().any(|x| x.side == Side::BaseSdk && x.check.starts_with("verbose")));
}

/// Renderings of the script bytes are checked against the oracle's own hex, so a wrong
/// rendering is a harness finding, while different script bytes are the libraries' finding.
#[test]
fn script_renderings_are_checked() {
    let f = load(7090);
    let mut verbose = f.verbose.clone();
    let spk = &mut verbose["tx"][0]["vout"][0]["scriptPubKey"];
    spk["asm"] = "OP_DUP".into();
    spk["address"] = "yWrong".into();
    spk["desc"] = "raw()#00000000".into();
    spk["type"] = "nonstandard".into();
    verbose["tx"][0]["vout"][0]["value"] = 1.into();
    let (m, _) = run(&f.bytes, Some(&f.oracle), Some(&verbose));
    for field in ["asm", "address", "desc", "type"] {
        let field = format!("vout[0].scriptPubKey.{field}");
        assert!(has_field(&m, "verbose.render", Side::Harness, &field), "{field}: {m:#?}");
    }
    assert!(has_field(&m, "verbose.render", Side::Harness, "vout[0].value"), "{m:#?}");
    assert_eq!(m.len(), 5, "{m:#?}");

    // Change one byte of the coinbase's first output script in the block itself: the oracle
    // still renders its own (consistent) script, so only the libraries are reported.
    let hex = f.verbose["tx"][0]["vout"][0]["scriptPubKey"]["hex"].as_str().unwrap();
    let script = hex_conservative::decode_to_vec(hex).unwrap();
    let span = split_block(&f.bytes).unwrap().txs[0].range.clone();
    let at =
        span.start + f.bytes[span.clone()].windows(script.len()).position(|w| w == script).unwrap();
    let mut bytes = f.bytes.clone();
    bytes[at + script.len() - 3] ^= 1;
    let (m, _) = run(&bytes, None, Some(&f.verbose));
    assert!(has_field(&m, "verbose.field", Side::Both, "vout[0].scriptPubKey.hex"), "{m:#?}");
    assert!(!m.iter().any(|x| x.check == "verbose.render" || x.check == "harness-gap"), "{m:#?}");
}

/// Every committed fixture's oracle JSON is fully classified and fully rendered: no fixture
/// produces a harness finding other than the oracle-txid rule.
#[test]
fn fixtures_have_no_harness_gaps() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    for entry in std::fs::read_dir(dir).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        let name = name.trim_end_matches(".json");
        let net = name.split('-').next().unwrap();
        let f = load_named(name);
        let (m, _) = run_on(net, &f.bytes, Some(&f.oracle), Some(&f.verbose));
        let harness: Vec<_> =
            m.iter().filter(|x| x.side == Side::Harness && x.check != "tx.txid_rule").collect();
        assert!(harness.is_empty(), "{name}: {harness:#?}");
    }
}

/// Block-level fields of `getblock 2` are classified and checked against the block bytes.
#[test]
fn block_level_fields_are_checked() {
    let f = load(7090);
    let mut verbose = f.verbose.clone();
    verbose["newBlockField"] = 1.into();
    verbose["bits"] = "1d00ffff".into();
    verbose["nTx"] = 99.into();
    verbose["cbTx"]["height"] = 1.into();
    verbose.as_object_mut().unwrap().remove("merkleroot");
    let (m, _) = run(&f.bytes, Some(&f.oracle), Some(&verbose));
    let gap = m.iter().find(|x| x.check == "harness-gap").expect("gap");
    assert_eq!((gap.field.as_deref(), gap.tx_index), (Some("newBlockField"), None));
    for field in ["bits", "nTx", "cbTx", "merkleroot"] {
        assert!(has_field(&m, "verbose.block", Side::Harness, field), "{field}: {m:#?}");
    }
    assert_eq!(m.len(), 5, "{m:#?}");
}

/// The stable-txid asset unlock: dashd prints `instanceHash` and the payload fee as `fee`;
/// both are checked, including that `instanceHash` is present.
#[test]
fn asset_unlock_instance_hash_and_fee_are_checked() {
    let f = load_named("regtest-feature_asset_locks-969");
    let base = run_regtest(&f, &f.verbose);
    let unlock = tx_with(&f.verbose, "assetUnlockTx");
    let mut verbose = f.verbose.clone();
    verbose["tx"][unlock]["fee"] = 0.5.into();
    verbose["tx"][unlock].as_object_mut().unwrap().remove("instanceHash");
    let m = run_regtest(&f, &verbose);
    let new: Vec<_> = m.iter().filter(|x| !base.contains(x)).collect();
    for field in ["fee", "instanceHash"] {
        assert!(has_field(&m, "verbose.render", Side::Harness, field), "{field}: {new:#?}");
    }
    assert_eq!(new.len(), 2, "{new:#?}");
}

/// The fixture names (`tests/data/<name>.json`), sorted.
fn fixture_names() -> Vec<String> {
    let dir = format!("{}/tests/data", env!("CARGO_MANIFEST_DIR"));
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            e.unwrap().file_name().into_string().unwrap().strip_suffix(".json").map(str::to_owned)
        })
        .collect();
    names.sort();
    names
}

/// The dash_consensus leg (CS0 §9.2) checks every fixture block and agrees with the oracle on
/// each, the asset-lock chain's stable-txid asset unlock included.
#[test]
fn the_consensus_leg_agrees_on_every_fixture() {
    let mut stable = 0;
    for name in fixture_names() {
        let f = load_named(&name);
        let net = name.split('-').next().unwrap();
        let (m, stats) = run_on(net, &f.bytes, Some(&f.oracle), None);
        assert_eq!(stats.consensus_blocks, 1, "{name}");
        let leg: Vec<&Mismatch> = m.iter().filter(|x| x.side == Side::DashConsensus).collect();
        assert!(leg.is_empty(), "{name}: {leg:#?}");
        let block = dash_consensus::decode_block(&mut &f.bytes[..]).unwrap();
        stable += block.value().txs().iter().filter(|tx| tx.txid() != tx.instance_hash()).count();
    }
    // The leg agreeing is only worth something while the fixtures still hold a transaction
    // whose txid differs from its instance hash (A1 §2.25).
    assert_eq!(stable, 1, "the stable-txid asset unlock of regtest-feature_asset_locks-969");
}

/// The leg reports damage against itself: a reference hash, txid and root that differ, trailing
/// bytes, a block cut short and a mutated block.
#[test]
fn the_consensus_leg_reports_damage() {
    let f = load(7090);
    let mut oracle = f.oracle.clone();
    oracle.hash[0] ^= 1;
    oracle.txids[0][0] ^= 1;
    oracle.merkle_root[0] ^= 1;
    let (m, _) = run(&f.bytes, Some(&oracle), None);
    for check in ["consensus.hash", "consensus.txid", "consensus.merkle_root"] {
        assert!(has(&m, check, Side::DashConsensus), "{check}");
    }

    // One byte more: the block decodes and leaves it, so its serialization is shorter.
    let mut longer = f.bytes.clone();
    longer.push(0);
    let (m, _) = run(&longer, Some(&f.oracle), None);
    for check in
        ["consensus.decode", "consensus.serialize", "consensus.core_bytes", "consensus.size"]
    {
        assert!(has(&m, check, Side::DashConsensus), "{check}");
    }
    let shorter = &f.bytes[..f.bytes.len() - 1];
    assert!(has(&run(shorter, Some(&f.oracle), None).0, "consensus.decode", Side::DashConsensus));

    // The coinbase twice: two equal leaves, Core's mutated merkle tree.
    let coinbase = &f.bytes[split_block(&f.bytes).unwrap().txs[0].range.clone()];
    let mut twice = f.bytes[..80].to_vec();
    twice.push(2);
    twice.extend_from_slice(coinbase);
    twice.extend_from_slice(coinbase);
    assert!(has(&run(&twice, None, None).0, "consensus.mutated", Side::DashConsensus));
}
