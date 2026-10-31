//! `diff_payload_<type>` (BS6b) without libFuzzer: the public seeds replayed, a proptest twin
//! over mutated seeds, and the asset-unlock stable txid's boundary rows against Core's.
//!
//! The release profile is v24 only (DEC-82), so everything runs on the v24.0 pin. Output rules
//! as for `l1_twins` (CS0 §9.5): a failure names the target, the field and the pin, and the input
//! goes to the private sink when `DASH_EMBARGO_DIR` is set.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::use_debug,
    clippy::dbg_macro,
    clippy::print_stdout
)]

#[path = "../../dash-consensus/tests/support/gen.rs"]
mod generate;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::LazyLock;

use dash_consensus::hash::sha1;
use dash_core_vectors::hex::try_parse_hex;
use dash_oracle_sys::{Oracle, Pin};
use dash_primitives::{SpecialPayload, Transaction, TxType};
use dash_types::codec::BaseCodec;
use dash_types::{Hashable, Numeric};
use node_l1::{Outcome, Target, target};
use node_sink::sink::{OpaqueSeed, record_for_test};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

use generate::{Rng, mutate, proptest_config};

const PIN: Pin = Pin::V24_0;

/// A payload target with its public seeds, `node-l1/seeds/diff_payload_<type>`.
type Seeded = (&'static Target, Vec<Vec<u8>>);

/// An edit of an asset unlock, and whether Core's txid of the result is stable.
type Row = (&'static str, bool, fn(&mut Transaction));

static SEEDS: LazyLock<Vec<Seeded>> = LazyLock::new(|| {
    (1..=12)
        .map(|tx_type| {
            let name = format!("diff_payload_{tx_type}");
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds").join(&name);
            let mut files: Vec<_> = std::fs::read_dir(&dir)
                .map(|entries| entries.filter_map(|e| std::fs::read(e.ok()?.path()).ok()).collect())
                .unwrap_or_default();
            files.sort();
            (target(&name).unwrap_or_else(|| panic!("{name} is not registered")), files)
        })
        .collect()
});

/// Compares `bytes` on the v24.0 pin; a divergence is recorded privately and fails.
fn agree(target: &Target, bytes: &[u8]) -> Result<Outcome, String> {
    (target.compare)(bytes, PIN).map_err(|divergence| {
        let recorded = record_for_test(target.name, bytes, PIN.name(), divergence.field);
        format!("{}: {divergence} differs on {} ({recorded})", target.name, PIN.name())
    })
}

/// Every public seed decodes as Core decodes it, and every target has seeds Core accepts.
#[test]
fn public_seeds_agree_with_core() {
    for (target, seeds) in SEEDS.iter() {
        assert!(!seeds.is_empty(), "{}: no public seeds", target.name);
        let mut accepted = 0;
        for seed in seeds {
            match agree(target, seed) {
                Ok(outcome) => accepted += usize::from(outcome == Outcome::BothAccept),
                Err(failure) => panic!("{failure}"),
            }
        }
        assert!(accepted > 0, "{}: Core accepts none of its seeds", target.name);
    }
}

/// The supported (type, version) matrix; for a quorum commitment, (payload version, commitment
/// version).
fn supported() -> Vec<(u16, Vec<u16>)> {
    let mut rows: Vec<(u16, Vec<u16>)> = Vec::new();
    rows.extend((1..=4).flat_map(|t| (1..=3).map(move |v| (t, vec![v]))));
    rows.extend((1..=4).map(|v| (5, vec![v])));
    rows.extend((1..=4).map(|v| (6, vec![1, v])));
    rows.extend(
        [(7, 1), (8, 1), (8, 2), (9, 1), (9, 2), (10, 1), (11, 1), (12, 1)]
            .map(|(t, v)| (t, vec![v])),
    );
    rows.sort();
    rows
}

/// The BLS fields of a payload of `tx_type` at `version`, and whether each is a public key.
fn bls_fields(tx_type: u16, version: u16) -> &'static [(&'static str, bool)] {
    match tx_type {
        1 | 3 | 12 => &[("pubKeyOperator", true)],
        2 | 4 | 7 | 9 => &[("sig", false)],
        5 if version >= 3 => &[("bestCLSignature", false)],
        6 => &[("quorumPublicKey", true), ("quorumSig", false), ("membersSig", false)],
        _ => &[],
    }
}

/// The layout version of a payload: the version, and for a quorum commitment the commitment's.
fn layout(tx_type: u16, payload: &[u8]) -> Vec<u16> {
    let le = |at: usize| payload.get(at..at + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
    match tx_type {
        7..=9 => vec![payload.first().copied().map_or(0, u16::from)],
        6 => vec![le(0), le(6)],
        _ => vec![le(0)],
    }
}

/// The raw operator key of a provider payload, which base-sdk keeps as read.
fn operator_key(tx_type: u16, payload: &[u8]) -> Option<[u8; 48]> {
    match SpecialPayload::decode(TxType::new(tx_type), &mut &payload[..]).ok()? {
        SpecialPayload::ProviderRegister(p) => Some(p.pub_key_operator.to_bytes()),
        SpecialPayload::ProviderUpdateRegistrar(p) => Some(p.pub_key_operator.to_bytes()),
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => Some(p.pub_key_operator.to_bytes()),
        _ => None,
    }
}

/// The offset of BLS field `field` in `payload`: an operator key where base-sdk decodes it,
/// a signature or quorum key counted from the payload's end.
fn field_offset(tx_type: u16, version: u16, field: &str, payload: &[u8]) -> Option<usize> {
    let n = payload.len();
    match (tx_type, field) {
        (1 | 3 | 12, _) => {
            let key = operator_key(tx_type, payload)?;
            payload.windows(48).position(|w| w == key)
        }
        (5, _) => n.checked_sub(
            8 + if version >= 4 {
                32
            } else {
                0
            } + 96,
        ),
        (6, "quorumPublicKey") => n.checked_sub(272),
        (6, "quorumSig") => n.checked_sub(192),
        _ => n.checked_sub(96),
    }
}

/// The seeds cover every supported type and version, and each BLS field holds a seed for every
/// class label and (legacy, basic) outcome pair of base-sdk's `bls_wrapper.json5` under each flag
/// Core reads it with (both for the global-flag CbTx and asset-unlock signatures). Every seed
/// `COVERAGE.json` names is a file, named by the SHA-1 of its contents, that is its field's base
/// seed with the sample in the field, which this test locates on its own.
#[test]
fn seeds_cover_every_field_and_outcome_pair() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds");
    let read = |tx_type: u16, name: &str| -> Vec<u8> {
        let bytes = std::fs::read(dir.join(format!("diff_payload_{tx_type}")).join(name))
            .unwrap_or_default();
        let digest: String = sha1(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(digest, name, "diff_payload_{tx_type}: seed {name} is missing or altered");
        bytes
    };

    // Every supported version has a seed under each flag.
    for (tx_type, version) in supported() {
        let found: BTreeSet<u8> = std::fs::read_dir(dir.join(format!("diff_payload_{tx_type}")))
            .into_iter()
            .flatten()
            .filter_map(|e| std::fs::read(e.ok()?.path()).ok())
            .filter_map(|seed| {
                let (flag, payload) = seed.split_first()?;
                (layout(tx_type, payload) == version).then_some(*flag)
            })
            .collect();
        assert_eq!(found, BTreeSet::from([0, 1]), "type {tx_type} v{version:?}: seeds by flag");
    }

    let text = std::fs::read_to_string(dir.join("COVERAGE.json")).unwrap_or_default();
    let cov: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    let as_u16 = |v: &serde_json::Value| v.as_u64().and_then(|n| u16::try_from(n).ok());
    let versions = |v: &serde_json::Value| -> Vec<u16> {
        v.as_array().into_iter().flatten().filter_map(as_u16).collect()
    };
    // The samples by (label, legacy outcome, basic outcome).
    let source = |kind: &str| -> BTreeMap<String, Vec<u8>> {
        cov["source"][kind]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| {
                let key = serde_json::json!([p[0], p[1], p[2]]).to_string();
                (key, try_parse_hex(p[3].as_str().unwrap_or_default()).unwrap_or_default())
            })
            .collect()
    };
    let (pk, sig) = (source("pk"), source("sig"));
    // The (label, outcome pair) counts of bls_wrapper.json5.
    assert_eq!((pk.len(), sig.len()), (50, 74), "source outcome pairs");

    let rows = cov["fields"].as_array().cloned().unwrap_or_default();
    let mut expected_rows = 0;
    let mut cells = 0;
    for (tx_type, version) in supported() {
        let flags: &[(&str, u8)] = if matches!(tx_type, 5 | 9) {
            &[("basic", 0), ("legacy", 1)]
        } else {
            &[("basic", 0)]
        };
        for &(field, is_key) in bls_fields(tx_type, version[0]) {
            for &(flag, flag_byte) in flags {
                expected_rows += 1;
                let label = format!("type {tx_type} v{version:?} {field} ({flag})");
                let row = rows.iter().find(|r| {
                    as_u16(&r["type"]) == Some(tx_type)
                        && versions(&r["version"]) == version
                        && r["field"] == field
                        && r["flag"] == flag
                });
                let Some(row) = row else {
                    panic!("{label}: no seeds");
                };
                let base = read(tx_type, row["base"].as_str().unwrap_or_default());
                assert_eq!(base.first(), Some(&flag_byte), "{label}: base flag");
                assert_eq!(layout(tx_type, &base[1..]), version, "{label}: base version");
                let Some(offset) = field_offset(tx_type, version[0], field, &base[1..]) else {
                    panic!("{label}: field not found");
                };
                let samples = if is_key {
                    &pk
                } else {
                    &sig
                };
                let mut pairs = BTreeSet::new();
                for cell in row["seeds"].as_array().into_iter().flatten() {
                    let key = serde_json::json!([cell["label"], cell["legacy"], cell["basic"]])
                        .to_string();
                    let Some(sample) = samples.get(&key) else {
                        panic!("{label}: {key} is not a source pair");
                    };
                    let seed = read(tx_type, cell["seed"].as_str().unwrap_or_default());
                    let mut want = base.clone();
                    want[1 + offset..1 + offset + sample.len()].copy_from_slice(sample);
                    assert_eq!(seed, want, "{label}: {key} is not the base with the sample");
                    pairs.insert(key);
                    cells += 1;
                }
                assert_eq!(pairs.len(), samples.len(), "{label}: outcome pairs");
            }
        }
    }
    assert_eq!(rows.len(), expected_rows, "COVERAGE.json rows");
    assert_eq!(cells, 2252, "seeded (field, flag, label, outcome pair) cells");
}

proptest! {
    #![proptest_config(proptest_config(1024))]

    #[test]
    fn diff_payload(seed in any::<u64>().prop_map(OpaqueSeed)) {
        let mut rng = Rng::new(seed.0);
        let (target, seeds) = &SEEDS[rng.below(SEEDS.len() as u64) as usize];
        let base = seeds[rng.below(seeds.len() as u64) as usize].clone();
        agree(target, &mutate(&mut rng, base)).map_err(TestCaseError::fail)?;
    }
}

/// `diff_payload` case 32220, from the `diff_payload_1` seed ba58ae6f: under flag 0 a version 3
/// ProRegTx whose `ExtNetInfo` purposes read 0, 1, 0. rc.4 refuses the repeated purpose (CE4-01,
/// `netinfo.h` rc.4:419-437); base-sdk read it first-wins before 029b40b, a decode verdict
/// divergence the proptest met in about one run in twelve.
const REPEATED_PURPOSE: &str = "0003000100000053a19f69a003ff355d81bb516a13be9434db376b857d3a2407a5f0a2bc4f0aa50200000001\
    0300010101047f0000014e5201010101047f000001523a00010101047f000001562267d7a27dc009acffb853\
    07af299128f4a0df6108f6c2a09280d3e803abf0ccb2705329f1fa2092f4fc07d1b9f1a71cdcb38f55e6b7f9\
    fef6c87f4334a0505758eb0cd58ba210784291bedc330102e9395f39772080a1e1c50000011976a914dcb9b4\
    653fac29513b4af8fb8a663956640862dc88ac1027000000000000000000000000008a586ab66c976730a9c7\
    67b10367843ca42f37bf51172e7a5d12d59b51dcb467524e0000000000000000000000000000000000004120\
    b04b8f2fb5d0b254d760dc26a872133ef2ee2bb3cd1b187db9aa3e415cbd4d315da57912b722151fbb2afff1\
    f5163e3b7b9cfa078423f0012407338a14ec4bd5";

#[test]
fn repeated_purpose_agrees_with_core() {
    let Some(bytes) = try_parse_hex(REPEATED_PURPOSE) else {
        panic!("REPEATED_PURPOSE is not hex");
    };
    assert_eq!(bytes.len(), 328);
    let target = target("diff_payload_1").unwrap_or_else(|| panic!("diff_payload_1"));
    match agree(target, &bytes) {
        Ok(outcome) => assert!(outcome == Outcome::BothReject, "Core refuses the payload"),
        Err(failure) => panic!("{failure}"),
    }
}

/// The asset unlocks of `dash/tests/data/core_v24_regtest.json` (dashd v24.0.0-rc.3).
fn asset_unlocks() -> Vec<Transaction> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../dash/tests/data/core_v24_regtest.json");
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let vectors: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    vectors["transactions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|tx| tx["shape"][0] == 9)
        .filter_map(|tx| {
            let raw = try_parse_hex(tx["hex"].as_str()?)?;
            Transaction::decode(&mut &raw[..]).ok()
        })
        .collect()
}

/// base-sdk's txid and instance hash of `tx` are Core's `GetHash()` and `GetInstanceHash()`.
fn hashes_match_core(label: &str, tx: &Transaction) -> bool {
    let mut raw = Vec::new();
    tx.encode(&mut raw);
    let Ok(core) = Oracle::new(PIN).tx_decode(&raw) else {
        panic!("{label}: Core does not decode it");
    };
    assert_eq!(tx.hash().to_lendian(), core.txid, "{label}: txid");
    assert_eq!(tx.instance_hash().to_lendian(), core.instance_hash, "{label}: instance hash");
    core.txid != core.instance_hash
}

/// The stable txid's boundary rows: the payload version, the payload size, the transaction
/// version and type. Core keys the rule on these bytes alone; the txid is stable exactly when
/// it differs from the instance hash.
#[test]
fn asset_unlock_stable_txid_matches_core() {
    let unlocks = asset_unlocks();
    assert_eq!(unlocks.len(), 4);
    for (n, tx) in unlocks.iter().enumerate() {
        let version = tx.extra_payload[0];
        let stable = hashes_match_core(&format!("unlock {n}"), tx);
        assert_eq!(stable, version >= 2, "unlock {n}: v{version} stable");
        assert_eq!(tx.has_stable_txid(), stable, "unlock {n}: has_stable_txid");
        let rows: [Row; 8] = [
            ("payload v1", false, |tx| tx.extra_payload[0] = 1),
            ("payload v2", true, |tx| tx.extra_payload[0] = 2),
            ("payload v3", true, |tx| tx.extra_payload[0] = 3),
            ("payload v255", true, |tx| tx.extra_payload[0] = 255),
            ("payload 144 bytes", false, |tx| {
                tx.extra_payload.pop();
            }),
            ("payload 146 bytes", false, |tx| tx.extra_payload.push(0)),
            ("asset lock type", false, |tx| tx.tx_type = TxType::AssetLock),
            ("version 2", false, |tx| tx.version = 2),
        ];
        for (row, expected, edit) in rows {
            let mut edited = tx.clone();
            edit(&mut edited);
            let label = format!("unlock {n}, {row}");
            assert_eq!(hashes_match_core(&label, &edited), expected, "{label}: stable");
            assert_eq!(edited.has_stable_txid(), expected, "{label}: has_stable_txid");
        }
    }
}
