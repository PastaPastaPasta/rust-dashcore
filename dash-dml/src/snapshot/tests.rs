//! Core rc.4's vectors and verdicts (`tests/data/evo_snapshot`, made by
//! `contrib/node/o9b-evo-snapshot`; see its PROVENANCE.md): every input decoded here must get
//! the verdict Core's `test_dash_o9b` gave it, the bytes re-encoded and the hashes included.

use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, DynBitset, LlmqType};
use dash_types::Numeric;

use super::codec::{
    read_mined_commitment, read_rotation_snapshot, write_mined_commitment, write_rotation_snapshot,
};
use super::*;
use crate::codec::{
    MAX_MNS, MAX_PAYOUT_SHARES, Reader, canonical_mn_list_hash, decode_canonical_diff,
    decode_canonical_list, encode_canonical_diff, encode_canonical_list_unvalidated,
};
use crate::netinfo::{CoreNetInfoValidity, NetInfoConfig, NetInfoRules};

const VECTORS: &str = include_str!("../../tests/data/evo_snapshot/vectors.txt");

/// Core's `netInfo->Validate()` under test_dash's `BasicTestingSetup`: mainnet, no
/// `-cjdnsreachable`.
fn net() -> CoreNetInfoValidity {
    let main = ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0)
        .build()
        .expect("Core's parameters");
    CoreNetInfoValidity {
        rules: NetInfoRules::new(&main, &main, NetInfoConfig::default()),
    }
}

/// `uint256::ToString`: the bytes reversed, in hex.
fn core_hex(bytes: &[u8; 32]) -> String {
    bytes.iter().rev().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    core_hex(&dash_consensus::hash::sha256(bytes))
}

fn hex_decode(hex: &str) -> Vec<u8> {
    assert!(hex.len().is_multiple_of(2), "odd hex");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

/// The verdict `test_dash_o9b`'s `Judge` prints, computed here.
fn verdict(kind: &str, bytes: &[u8]) -> String {
    let mut rest = bytes;
    let net = net();
    let result = (|| -> Result<(Vec<u8>, String), CodecError> {
        let mut out = Vec::new();
        let extra = match kind {
            "snapshot" => {
                let snapshot = EvoSnapshot::decode(&mut rest, &net)?;
                snapshot.encode(&mut out);
                let hash = snapshot.hash(&net)?;
                format!(
                    " {} {}",
                    core_hex(hash.as_bytes()),
                    core_hex(canonical_mn_list_hash(&snapshot.mn_list).as_bytes())
                )
            }
            _ if kind.starts_with("reconstruct:") => {
                let budget = kind["reconstruct:".len()..].parse().expect("a budget");
                let snapshot = EvoSnapshot::decode(&mut rest, &net)?;
                let lists = snapshot.reconstruct_historical_mn_lists(&net, budget)?;
                snapshot.encode(&mut out);
                format!(" {}", lists.len())
            }
            "mnlist" => {
                let list = decode_canonical_list(&mut rest, &net)?;
                encode_canonical_list_unvalidated(&list, &mut out);
                format!(" {}", core_hex(canonical_mn_list_hash(&list).as_bytes()))
            }
            "mndiff" => {
                let mut budget = max_historical_mn_operations();
                encode_canonical_diff(
                    &decode_canonical_diff(&mut rest, &mut budget, &net)?,
                    &mut out,
                );
                String::new()
            }
            "commitment" => {
                write_mined_commitment(
                    &read_mined_commitment(&mut Reader::new(&mut rest, None))?,
                    &mut out,
                );
                String::new()
            }
            _ => {
                let llmq_type = kind.strip_prefix("rotation:").expect("a known kind");
                let llmq_type = LlmqType::from_base(llmq_type.parse().expect("a type"));
                snapshot_llmq_params(llmq_type)?;
                write_rotation_snapshot(
                    &read_rotation_snapshot(&mut Reader::new(&mut rest, None))?,
                    &mut out,
                );
                String::new()
            }
        };
        Ok((out, extra))
    })();
    match result {
        Ok((out, extra)) => format!("ok {} {}{extra}", rest.len(), sha256_hex(&out)),
        Err(e) => format!("fail ios_base::failure {e}"),
    }
}

/// The AddMN messages whose key value is normalized away (O9b-notes, S6a ruling 1).
const KEYED_MESSAGES: &[&str] = &[
    "with a duplicate keyIDOwner=",
    "with a duplicate share ownerKeyID=",
    "with a duplicate pubKeyOperator=",
    "with a duplicate platformNodeID=",
];

/// Core's verdict in this crate's terms: libstdc++'s `ios_base::failure` appends ": iostream
/// error" to `what()` (twice where `Validate` rethrows a replay error's `what()`), and the stream
/// names itself in its end-of-data text.
fn normalized(core: &str) -> String {
    let mut core = core;
    while let Some(inner) = core.strip_suffix(": iostream error") {
        core = inner;
    }
    // AddMN's key values (a Base58 address, a BLS key's string form), which this crate's
    // messages leave out (`MnListError::core_message`): only the value is dropped.
    for key in KEYED_MESSAGES {
        if let Some(at) = core.find(key) {
            core = &core[..at + key.len() - 1];
        }
    }
    match core.find("::read(): end of data") {
        Some(_) if core.starts_with("fail ios_base::failure ") => {
            "fail ios_base::failure end of data".to_owned()
        }
        _ => core.to_owned(),
    }
}

/// The dump: `<name> <kind> <hex> <verdict>`.
fn vectors() -> impl Iterator<Item = (&'static str, &'static str, Vec<u8>, &'static str)> {
    VECTORS.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).map(|line| {
        let mut fields = line.splitn(4, ' ');
        let mut next = || fields.next().expect("a field");
        let (name, kind, hex, verdict) = (next(), next(), next(), next());
        (name, kind, hex_decode(hex), verdict)
    })
}

/// Test 1: every Core vector gets Core's verdict here, its re-encoding (byte-exact: the
/// re-encoding's hash is Core's, and an accepted input re-encodes to itself) and its hashes
/// included.
#[test]
fn core_vectors_round_trip() {
    let mut seen = 0;
    for (name, kind, bytes, core) in vectors() {
        let ours = verdict(kind, &bytes);
        assert_eq!(ours, normalized(core), "{name}");
        if ours.starts_with("ok 0 ") {
            assert_eq!(
                ours.split(' ').nth(2),
                Some(sha256_hex(&bytes).as_str()),
                "{name}: not canonical"
            );
        }
        seen += 1;
    }
    assert!(seen > 10, "the dump has {seen} vectors");
}

/// Core's `populated_v3_golden_value`: the hash Core's own test pins for `SyntheticSnapshot()`.
#[test]
fn populated_v3_golden_value() {
    let (_, _, bytes, _) = vectors().find(|v| v.0 == "populated").expect("the populated vector");
    let snapshot = EvoSnapshot::decode(&mut bytes.as_slice(), &net()).expect("decodes");
    assert_eq!(
        core_hex(snapshot.hash(&net()).expect("valid").as_bytes()),
        "5b5d496a66d93775a6a6d1badf2ea2d5bbb25b58031fe0dd2c3c670674c6908d"
    );
}

#[test]
fn table_derived_bounds() {
    assert_eq!(max_historical_mn_operations(), max_historical_mn_lists() * 4_096);
}

/// `Validate`'s commitment-size ceiling (`snapshot.cpp` rc.4:48-53), which the decoder's bounds
/// keep decoded input from reaching: a constructed snapshot's commitment at the ceiling passes
/// it, and one past it is refused with Core's text.
#[test]
fn validate_bounds_constructed_commitment_sizes() {
    let sized = |bits: usize| {
        let mut snapshot = populated();
        let qc = &mut snapshot.quorums[0].active_commitments[0].commitment;
        let bitset = DynBitset {
            num_bits: bits as u64,
            data: vec![0; bits.div_ceil(8)],
        };
        (qc.signers, qc.valid_members) = (bitset.clone(), bitset);
        snapshot.validate(false, &net())
    };
    assert!(sized(MAX_QUORUM_SIZE).is_ok());
    assert_eq!(
        sized(MAX_QUORUM_SIZE + 1).unwrap_err().to_string(),
        "invalid evo quorum commitment sizes"
    );
}

// ----------------------------------------------------------------------------------------------
// Crafted inputs: defined once here, judged by Core (`gen_crafted` writes them for
// `test_dash_o9b`'s judge mode; `verdicts.txt` holds Core's verdict and each input's hash).

const VERDICTS: &str = include_str!("../../tests/data/evo_snapshot/verdicts.txt");

/// A byte writer for crafted inputs.
#[derive(Clone, Default)]
struct W(Vec<u8>);

impl W {
    fn u8(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    fn u16(mut self, v: u16) -> Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn i32(mut self, v: i32) -> Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn u32(mut self, v: u32) -> Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn i64(mut self, v: i64) -> Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    /// The test fixtures' `H(value)`: a hash whose first byte is `value`.
    fn h(mut self, value: u8) -> Self {
        let mut hash = [0; 32];
        hash[0] = value;
        self.0.extend(hash);
        self
    }
    fn compact(mut self, n: u64) -> Self {
        dash_consensus::primitives::write_compact_size(n, &mut self.0);
        self
    }
    fn varint(mut self, n: u64) -> Self {
        dash_consensus::primitives::write_varint(n, &mut self.0);
        self
    }
    fn zeros(mut self, n: usize) -> Self {
        self.0.extend(std::iter::repeat_n(0, n));
        self
    }
    fn raw(mut self, bytes: &[u8]) -> Self {
        self.0.extend_from_slice(bytes);
        self
    }
    fn done(self) -> Vec<u8> {
        self.0
    }
}

/// The sections of a snapshot after its MN list, each replaceable.
struct Sections {
    list: W,
    quorums: W,
    history: W,
    modifiers: W,
    credit: W,
    signals: W,
}

impl Default for Sections {
    /// The smallest valid snapshot: base `H(42)`, an empty list at height 500, nothing else.
    fn default() -> Self {
        Sections {
            list: W::default().h(42).i32(500).u32(0).compact(0),
            quorums: W::default().compact(0),
            history: W::default().compact(0),
            modifiers: W::default().compact(0),
            credit: W::default().i64(0).i64(0).i64(0).compact(0),
            signals: W::default().compact(0),
        }
    }
}

impl Sections {
    fn bytes(self) -> Vec<u8> {
        W::default()
            .u16(EVO_SNAPSHOT_VERSION)
            .h(42)
            .raw(&self.list.0)
            .raw(&self.quorums.0)
            .raw(&self.history.0)
            .raw(&self.modifiers.0)
            .raw(&self.credit.0)
            .raw(&self.signals.0)
            .done()
    }
}

/// The IETF (basic-scheme) compressed encoding of the BLS12-381 G1 generator.
const G1_GENERATOR: &str = "97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb";

/// A mined commitment of `version` for LLMQ type `llmq_type` with `size`-bit bitsets and the
/// given public key; null BLS signatures.
fn commitment(version: u16, llmq_type: u8, size: u64, pk: &[u8]) -> W {
    let w = W::default().h(11).h(131).u16(version).u8(llmq_type).h(11);
    let w = if matches!(version, 2 | 4) {
        w.u16(0)
    } else {
        w
    };
    let bytes = size.div_ceil(8) as usize;
    w.compact(size)
        .zeros(bytes)
        .compact(size)
        .zeros(bytes)
        .raw(pk)
        .zeros(32)
        .zeros(96)
        .zeros(96)
        .h(51)
}

fn llmq(llmq_type: u8) -> &'static dash_params::LlmqParams {
    snapshot_llmq_params(LlmqType::from_base(llmq_type)).expect("a table type")
}

/// The populated vector (Core's `SyntheticSnapshot()`), decoded.
fn populated() -> EvoSnapshot {
    let (_, _, bytes, _) = vectors().find(|v| v.0 == "populated").expect("the populated vector");
    EvoSnapshot::decode(&mut bytes.as_slice(), &net()).expect("decodes")
}

fn encoded(snapshot: &EvoSnapshot) -> Vec<u8> {
    let mut out = Vec::new();
    snapshot.encode(&mut out);
    out
}

const MAX_MN_COMPACT_ITEMS_USIZE: usize = crate::codec::MAX_MN_COMPACT_ITEMS as usize;

/// Every crafted case: `(name, kind, bytes)`. Names are unique; each bound has a case at its
/// limit (refused later, or accepted) and one past it.
fn crafted() -> Vec<(&'static str, &'static str, Vec<u8>)> {
    let snap = |f: &dyn Fn(&mut Sections)| {
        let mut s = Sections::default();
        f(&mut s);
        s.bytes()
    };
    let max_history = max_historical_mn_lists() as u64;
    let test = llmq(100);
    let test_active = test.signing_active_quorum_count as u64;
    let test_safety = snapshot_commitment_count(test, false).unwrap() as u64 - test_active;
    let generator: Vec<u8> = hex_decode(G1_GENERATOR);
    let mut cases: Vec<(&'static str, &'static str, Vec<u8>)> = vec![
        ("minimal", "snapshot", snap(&|_| {})),
        ("version_2", "snapshot", W::default().u16(2).done()),
        ("version_4", "snapshot", W::default().u16(4).h(42).done()),
        (
            "list_height_negative",
            "snapshot",
            snap(&|s| s.list = W::default().h(42).i32(-1).u32(0).compact(0)),
        ),
        (
            "mn_count_at_limit",
            "snapshot",
            W::default().u16(3).h(42).h(42).i32(500).u32(0).compact(100_000).done(),
        ),
        (
            "mn_count_over",
            "snapshot",
            W::default().u16(3).h(42).h(42).i32(500).u32(0).compact(100_001).done(),
        ),
        (
            "mn_count_noncanonical",
            "snapshot",
            W::default().u16(3).h(42).h(42).i32(500).u32(0).u8(0xfd).u16(1).done(),
        ),
        (
            "base_mismatch",
            "snapshot",
            snap(&|s| s.list = W::default().h(43).i32(500).u32(0).compact(0)),
        ),
        (
            "quorum_count_at_limit",
            "snapshot",
            snap(&|s| s.quorums = W::default().compact(dash_params::AVAILABLE_LLMQS.len() as u64)),
        ),
        (
            "quorum_count_over",
            "snapshot",
            snap(&|s| {
                s.quorums = W::default().compact(dash_params::AVAILABLE_LLMQS.len() as u64 + 1)
            }),
        ),
        (
            "history_count_at_limit",
            "snapshot",
            snap(&|s| s.history = W::default().compact(max_history)),
        ),
        (
            "history_count_over",
            "snapshot",
            snap(&|s| s.history = W::default().compact(max_history + 1)),
        ),
        (
            "modifier_count_at_limit",
            "snapshot",
            snap(&|s| s.modifiers = W::default().compact(MAX_MODIFIERS as u64)),
        ),
        (
            "modifier_count_over",
            "snapshot",
            snap(&|s| s.modifiers = W::default().compact(MAX_MODIFIERS as u64 + 1)),
        ),
        (
            "ranges_at_limit",
            "snapshot",
            snap(&|s| s.credit = W::default().i64(0).i64(0).i64(0).compact(MAX_RANGES)),
        ),
        (
            "ranges_over",
            "snapshot",
            snap(&|s| s.credit = W::default().i64(0).i64(0).i64(0).compact(MAX_RANGES + 1)),
        ),
        (
            "ranges_one",
            "snapshot",
            snap(&|s| {
                s.credit = W::default()
                    .i64(0)
                    .i64(0)
                    .i64(0)
                    .compact(1)
                    .raw(&7u64.to_le_bytes())
                    .raw(&9u64.to_le_bytes())
            }),
        ),
        (
            "ranges_adjacent",
            "snapshot",
            snap(&|s| {
                s.credit = W::default()
                    .i64(0)
                    .i64(0)
                    .i64(0)
                    .compact(2)
                    .raw(&7u64.to_le_bytes())
                    .raw(&9u64.to_le_bytes())
                    .raw(&9u64.to_le_bytes())
                    .raw(&10u64.to_le_bytes())
            }),
        ),
        (
            "credit_locked_negative",
            "snapshot",
            snap(&|s| s.credit = W::default().i64(-1).i64(0).i64(0).compact(0)),
        ),
        (
            "credit_locked_max_money",
            "snapshot",
            snap(&|s| {
                s.credit = W::default().i64(dash_consensus::MAX_MONEY).i64(0).i64(0).compact(0)
            }),
        ),
        (
            "credit_locked_over_max_money",
            "snapshot",
            snap(&|s| {
                s.credit = W::default().i64(dash_consensus::MAX_MONEY + 1).i64(0).i64(0).compact(0)
            }),
        ),
        (
            "credit_limit_at_locked",
            "snapshot",
            snap(&|s| s.credit = W::default().i64(5).i64(5).i64(0).compact(0)),
        ),
        (
            "credit_limit_over_locked",
            "snapshot",
            snap(&|s| s.credit = W::default().i64(5).i64(6).i64(0).compact(0)),
        ),
        (
            "credit_unlocked_negative",
            "snapshot",
            snap(&|s| s.credit = W::default().i64(5).i64(0).i64(-1).compact(0)),
        ),
        (
            "signals_at_limit",
            "snapshot",
            snap(&|s| s.signals = W::default().compact(MAX_MNHF_SIGNALS as u64)),
        ),
        (
            "signals_over",
            "snapshot",
            snap(&|s| s.signals = W::default().compact(MAX_MNHF_SIGNALS as u64 + 1)),
        ),
        (
            "signals_all_bits",
            "snapshot",
            snap(&|s| {
                let mut w = W::default().compact(MAX_MNHF_SIGNALS as u64);
                for bit in 0..MAX_MNHF_SIGNALS as u8 {
                    w = w.u8(bit).i32(i32::from(bit));
                }
                s.signals = w;
            }),
        ),
        (
            "signal_repeated",
            "snapshot",
            snap(&|s| s.signals = W::default().compact(2).u8(2).i32(1).u8(2).i32(1)),
        ),
        (
            "signal_descending",
            "snapshot",
            snap(&|s| s.signals = W::default().compact(2).u8(3).i32(1).u8(2).i32(1)),
        ),
        ("signal_bit_28", "snapshot", snap(&|s| s.signals = W::default().compact(1).u8(28).i32(1))),
        ("signal_bit_29", "snapshot", snap(&|s| s.signals = W::default().compact(1).u8(29).i32(1))),
        (
            "signal_height_at_base",
            "snapshot",
            snap(&|s| s.signals = W::default().compact(1).u8(0).i32(500)),
        ),
        (
            "signal_height_over_base",
            "snapshot",
            snap(&|s| s.signals = W::default().compact(1).u8(0).i32(501)),
        ),
        (
            "signal_height_negative",
            "snapshot",
            snap(&|s| s.signals = W::default().compact(1).u8(0).i32(-1)),
        ),
        (
            "quorum_unknown_type",
            "snapshot",
            snap(&|s| s.quorums = W::default().compact(1).u8(0).u8(0)),
        ),
        (
            "quorum_rotation_flag_2",
            "snapshot",
            snap(&|s| s.quorums = W::default().compact(1).u8(0).u8(2)),
        ),
        (
            "quorum_empty_test",
            "snapshot",
            snap(&|s| {
                s.quorums = W::default().compact(1).u8(100).u8(0).compact(0).compact(0).compact(0)
            }),
        ),
        (
            "quorum_rotation_on_plain_type",
            "snapshot",
            snap(&|s| {
                s.quorums = W::default().compact(1).u8(100).u8(1).compact(0).compact(0).compact(0)
            }),
        ),
        (
            "quorum_duplicate_type",
            "snapshot",
            snap(&|s| {
                s.quorums = W::default()
                    .compact(2)
                    .u8(100)
                    .u8(0)
                    .compact(0)
                    .compact(0)
                    .compact(0)
                    .u8(100)
                    .u8(0)
                    .compact(0)
                    .compact(0)
                    .compact(0)
            }),
        ),
        (
            "quorum_types_descending",
            "snapshot",
            snap(&|s| {
                s.quorums = W::default()
                    .compact(2)
                    .u8(101)
                    .u8(0)
                    .compact(0)
                    .compact(0)
                    .compact(0)
                    .u8(100)
                    .u8(0)
                    .compact(0)
                    .compact(0)
                    .compact(0)
            }),
        ),
        (
            "active_at_limit",
            "snapshot",
            snap(&|s| s.quorums = W::default().compact(1).u8(100).u8(0).compact(test_active)),
        ),
        (
            "active_over",
            "snapshot",
            snap(&|s| s.quorums = W::default().compact(1).u8(100).u8(0).compact(test_active + 1)),
        ),
        (
            "safety_at_limit",
            "snapshot",
            snap(&|s| {
                s.quorums = W::default().compact(1).u8(100).u8(0).compact(0).compact(test_safety)
            }),
        ),
        (
            "safety_over",
            "snapshot",
            snap(&|s| {
                s.quorums =
                    W::default().compact(1).u8(100).u8(0).compact(0).compact(test_safety + 1)
            }),
        ),
        (
            "rotation_snapshots_at_limit",
            "snapshot",
            snap(&|s| {
                s.quorums = W::default()
                    .compact(1)
                    .u8(103)
                    .u8(1)
                    .compact(0)
                    .compact(0)
                    .compact(ROTATION_CYCLES as u64)
            }),
        ),
        (
            "rotation_snapshots_over",
            "snapshot",
            snap(&|s| {
                s.quorums = W::default()
                    .compact(1)
                    .u8(103)
                    .u8(1)
                    .compact(0)
                    .compact(0)
                    .compact(ROTATION_CYCLES as u64 + 1)
            }),
        ),
        ("commitment_valid", "commitment", commitment(3, 100, 8, &[0; 48]).done()),
        ("commitment_indexed_valid", "commitment", commitment(4, 103, 8, &[0; 48]).done()),
        ("commitment_unknown_version", "commitment", commitment(7, 100, 8, &[0; 48]).done()),
        (
            "commitment_signers_at_limit",
            "commitment",
            W::default().h(11).h(131).u16(3).u8(100).h(11).compact(MAX_QUORUM_SIZE as u64).done(),
        ),
        (
            "commitment_signers_over",
            "commitment",
            W::default()
                .h(11)
                .h(131)
                .u16(3)
                .u8(100)
                .h(11)
                .compact(MAX_QUORUM_SIZE as u64 + 1)
                .done(),
        ),
        (
            "commitment_signers_empty",
            "commitment",
            W::default().h(11).h(131).u16(3).u8(100).h(11).compact(0).done(),
        ),
        (
            "commitment_valid_members_over",
            "commitment",
            W::default()
                .h(11)
                .h(131)
                .u16(3)
                .u8(100)
                .h(11)
                .compact(8)
                .u8(0)
                .compact(MAX_QUORUM_SIZE as u64 + 1)
                .done(),
        ),
        (
            "commitment_valid_members_mismatch",
            "commitment",
            W::default()
                .h(11)
                .h(131)
                .u16(3)
                .u8(100)
                .h(11)
                .compact(8)
                .u8(0)
                .compact(16)
                .zeros(2)
                .done(),
        ),
        (
            "commitment_padding_bit",
            "commitment",
            W::default().h(11).h(131).u16(3).u8(100).h(11).compact(7).u8(0x80).done(),
        ),
        (
            "commitment_top_bit_in_range",
            "commitment",
            commitment(3, 100, 8, &[0; 48])
                .done()
                .into_iter()
                .enumerate()
                .map(|(i, b)| {
                    if i == 32 + 32 + 2 + 1 + 32 + 1 {
                        0x80
                    } else {
                        b
                    }
                })
                .collect(),
        ),
        ("commitment_truncated", "commitment", {
            let mut b = commitment(3, 100, 8, &[0; 48]).done();
            b.pop();
            b
        }),
        ("commitment_pk_basic_in_basic", "commitment", commitment(3, 100, 8, &generator).done()),
        ("commitment_pk_basic_in_legacy", "commitment", commitment(1, 100, 8, &generator).done()),
        ("commitment_pk_all_ff", "commitment", commitment(3, 100, 8, &[0xff; 48]).done()),
        (
            "rotation_valid",
            "rotation:103",
            W::default().h(1).h(2).i32(0).compact(8).u8(5).compact(1).i32(3).done(),
        ),
        (
            "rotation_unknown_type",
            "rotation:0",
            W::default().h(1).h(2).i32(0).compact(0).compact(0).done(),
        ),
        (
            "rotation_bitset_at_limit",
            "rotation:103",
            W::default().h(1).h(2).i32(0).compact(MAX_MNS as u64).done(),
        ),
        (
            "rotation_bitset_over",
            "rotation:103",
            W::default().h(1).h(2).i32(0).compact(MAX_MNS as u64 + 1).done(),
        ),
        (
            "rotation_bitset_padding_bit",
            "rotation:103",
            W::default().h(1).h(2).i32(0).compact(3).u8(0x08).done(),
        ),
        (
            "rotation_skip_list_at_limit",
            "rotation:103",
            W::default().h(1).h(2).i32(0).compact(0).compact(MAX_SKIPLIST_ENTRIES as u64).done(),
        ),
        (
            "rotation_skip_list_over",
            "rotation:103",
            W::default()
                .h(1)
                .h(2)
                .i32(0)
                .compact(0)
                .compact(MAX_SKIPLIST_ENTRIES as u64 + 1)
                .done(),
        ),
        ("mndiff_empty", "mndiff", W::default().compact(0).compact(0).compact(0).done()),
        ("mndiff_additions_at_limit", "mndiff", W::default().compact(MAX_MNS as u64).done()),
        ("mndiff_additions_over", "mndiff", W::default().compact(MAX_MNS as u64 + 1).done()),
        (
            "mndiff_updates_at_limit",
            "mndiff",
            W::default().compact(0).compact(MAX_MNS as u64).done(),
        ),
        (
            "mndiff_updates_over",
            "mndiff",
            W::default().compact(0).compact(MAX_MNS as u64 + 1).done(),
        ),
        (
            "mndiff_removals_at_limit",
            "mndiff",
            W::default().compact(0).compact(0).compact(MAX_MNS as u64).done(),
        ),
        (
            "mndiff_removals_over",
            "mndiff",
            W::default().compact(0).compact(0).compact(MAX_MNS as u64 + 1).done(),
        ),
        (
            "mndiff_removals_ascending",
            "mndiff",
            W::default().compact(0).compact(0).compact(2).varint(1).varint(2).done(),
        ),
        (
            "mndiff_removal_repeated",
            "mndiff",
            W::default().compact(0).compact(0).compact(2).varint(1).varint(1).done(),
        ),
        (
            "mndiff_removal_varint_too_large",
            "mndiff",
            W::default().compact(0).compact(0).compact(1).raw(&[0xff; 10]).done(),
        ),
        (
            "mndiff_update_order",
            "mndiff",
            W::default()
                .compact(0)
                .compact(2)
                .varint(2)
                .varint(0)
                .varint(1)
                .varint(0)
                .compact(0)
                .done(),
        ),
        (
            "mndiff_update_empty_fields",
            "mndiff",
            W::default().compact(0).compact(1).varint(2).varint(0).compact(0).done(),
        ),
        (
            "mndiff_update_operator_without_version",
            "mndiff",
            W::default()
                .compact(0)
                .compact(1)
                .varint(2)
                .varint(u64::from(crate::diff::MnStateDiff::PUB_KEY_OPERATOR))
                .done(),
        ),
        ("mnlist_empty", "mnlist", W::default().h(42).i32(0).u32(0).compact(0).done()),
    ];

    // The history's cumulative operation budget: removals cost a byte each, so whole diffs of
    // them reach it without any masternode. Core charges each section's count before reading it.
    let ops = max_historical_mn_operations() as u64;
    let full = MAX_MNS as u64;
    let diff_head =
        |w: W, prev: u8, block: u8, height: i32| w.h(prev).h(block).i32(height).u32(0).h(1);
    let removals = |mut w: W, n: u64| {
        w = w.compact(0).compact(0).compact(n);
        for id in 0..n {
            w = w.varint(id);
        }
        w
    };
    let budget_case = |extra: u64| {
        snap(&|s| {
            let whole = ops / full;
            let mut w = W::default().compact(whole + 1);
            for i in 0..whole {
                w = removals(diff_head(w, 42 + i as u8, 43 + i as u8, 499 - i as i32), full);
            }
            // The last diff's removals count reaches the budget exactly, or passes it by one. At
            // the budget the read goes on, into the sections after the history, whose zeros read
            // as repeated removal ids.
            w = diff_head(w, 42 + whole as u8, 43 + whole as u8, 499 - whole as i32)
                .compact(0)
                .compact(0)
                .compact(ops - whole * full + extra);
            s.history = w;
        })
    };
    cases.push(("history_operations_at_budget", "snapshot", budget_case(0)));
    cases.push(("history_operations_over_budget", "snapshot", budget_case(1)));

    // Validate's matrix, on Core's populated snapshot.
    let base = populated();
    let mutate = |f: &dyn Fn(&mut EvoSnapshot)| {
        let mut s = base.clone();
        f(&mut s);
        encoded(&s)
    };
    let plain = |s: &mut EvoSnapshot| -> usize {
        s.quorums.iter().position(|q| q.llmq_type.to_base() == 100).expect("LLMQ_TEST")
    };
    let rotated = |s: &mut EvoSnapshot| -> usize {
        s.quorums.iter().position(|q| q.llmq_type.to_base() == 103).expect("LLMQ_TEST_DIP0024")
    };
    cases.extend([
        ("populated_reencoded", "snapshot", mutate(&|_| {})),
        (
            "commitment_index_out_of_range",
            "snapshot",
            mutate(&|s| {
                let q = rotated(s);
                s.quorums[q].active_commitments[0].commitment.quorum_index = Some(2);
            }),
        ),
        (
            "commitment_index_negative",
            "snapshot",
            mutate(&|s| {
                let q = rotated(s);
                s.quorums[q].active_commitments[0].commitment.quorum_index = Some(-1);
            }),
        ),
        (
            "commitment_index_duplicate",
            "snapshot",
            mutate(&|s| {
                let q = rotated(s);
                s.quorums[q].active_commitments[1].commitment.quorum_index = Some(0);
            }),
        ),
        (
            "commitment_version_5",
            "snapshot",
            mutate(&|s| {
                let q = plain(s);
                s.quorums[q].active_commitments[0].commitment.version = 5;
            }),
        ),
        (
            "commitment_rotation_version_mismatch",
            "snapshot",
            mutate(&|s| {
                let q = plain(s);
                let c = &mut s.quorums[q].active_commitments[0].commitment;
                c.version = 4;
                c.quorum_index = Some(0);
            }),
        ),
        (
            "commitment_type_mismatch",
            "snapshot",
            mutate(&|s| {
                let q = plain(s);
                s.quorums[q].active_commitments[0].commitment.llmq_type = LlmqType::from_base(101);
            }),
        ),
        (
            "commitment_base_hash_mismatch",
            "snapshot",
            mutate(&|s| {
                let q = plain(s);
                s.quorums[q].active_commitments[0].commitment.quorum_hash =
                    dash_primitives::QuorumHash::from([9; 32]);
            }),
        ),
        (
            "commitment_null_mined_hash",
            "snapshot",
            mutate(&|s| {
                let q = plain(s);
                s.quorums[q].active_commitments[0].mined_block_hash =
                    dash_primitives::BlockHash::default();
            }),
        ),
        (
            "commitment_duplicate_base_hash",
            "snapshot",
            mutate(&|s| {
                let q = plain(s);
                let c = s.quorums[q].active_commitments[0].clone();
                s.quorums[q].safety_commitments[0] = c;
            }),
        ),
        (
            "modifier_missing",
            "snapshot",
            mutate(&|s| {
                s.quorum_modifiers.pop();
            }),
        ),
        (
            "modifier_extra",
            "snapshot",
            mutate(&|s| {
                let mut m = s.quorum_modifiers[0].clone();
                m.work_block_hash = dash_primitives::BlockHash::from([7; 32]);
                s.quorum_modifiers.push(m);
            }),
        ),
        (
            "modifier_null",
            "snapshot",
            mutate(&|s| {
                s.quorum_modifiers[0].modifier = dash_num::Hash256::from([0; 32]);
            }),
        ),
        (
            "modifier_duplicate",
            "snapshot",
            mutate(&|s| {
                let m = s.quorum_modifiers[0].clone();
                s.quorum_modifiers.push(m);
            }),
        ),
        (
            "modifier_unknown_type",
            "snapshot",
            mutate(&|s| {
                s.quorum_modifiers[0].llmq_type = LlmqType::from_base(0);
            }),
        ),
        (
            "history_missing_target",
            "snapshot",
            mutate(&|s| {
                let low = s.historical_mn_list_diffs.iter().map(|h| h.height).min().unwrap();
                s.historical_mn_list_diffs.retain(|h| h.height != low);
            }),
        ),
        (
            "history_hash_mismatch",
            "snapshot",
            mutate(&|s| {
                s.historical_mn_list_diffs[0].canonical_list_hash =
                    dash_num::Hash256::from([5; 32]);
            }),
        ),
        (
            "history_counter_grows",
            "snapshot",
            mutate(&|s| {
                s.historical_mn_list_diffs[0].total_registered_count =
                    s.mn_list.total_registered() + 1;
            }),
        ),
        (
            "history_height_at_base",
            "snapshot",
            mutate(&|s| {
                let h = s.mn_list.height();
                s.historical_mn_list_diffs.iter_mut().max_by_key(|e| e.height).unwrap().height = h;
            }),
        ),
        (
            "rotation_mode_4",
            "snapshot",
            mutate(&|s| {
                let q = rotated(s);
                s.quorums[q].rotation_snapshots[0].snapshot.skip_list_mode = 4;
            }),
        ),
        (
            "rotation_mode_3",
            "snapshot",
            mutate(&|s| {
                let q = rotated(s);
                s.quorums[q].rotation_snapshots[0].snapshot.skip_list_mode = 3;
            }),
        ),
        (
            "rotation_skip_first_negative",
            "snapshot",
            mutate(&|s| {
                let q = rotated(s);
                s.quorums[q].rotation_snapshots[0].snapshot.skip_list = vec![-1, 2];
            }),
        ),
        (
            "rotation_skip_later_negative",
            "snapshot",
            mutate(&|s| {
                let q = rotated(s);
                s.quorums[q].rotation_snapshots[0].snapshot.skip_list = vec![1, -2];
            }),
        ),
        (
            "rotation_null_cycle",
            "snapshot",
            mutate(&|s| {
                let q = rotated(s);
                s.quorums[q].rotation_snapshots[0].cycle_base_block_hash =
                    dash_primitives::BlockHash::default();
            }),
        ),
        (
            "signal_above_list_height",
            "snapshot",
            mutate(&|s| {
                s.mnhf_signals.insert(3, s.mn_list.height() + 1);
            }),
        ),
    ]);
    // A canonical list with its counter at the highest internal id.
    let mut low_counter = encoded(&base);
    low_counter[2 + 32 + 32 + 4..2 + 32 + 32 + 8].copy_from_slice(&7u32.to_le_bytes());
    cases.push(("list_counter_at_max_id", "snapshot", low_counter));
    // The two regular masternodes, which encode to the same length, swapped.
    let mns: Vec<Vec<u8>> = base
        .mn_list
        .iter()
        .map(|dmn| {
            let mut out = Vec::new();
            crate::codec::write_dmn(dmn, &mut out);
            out
        })
        .collect();
    let mut sorted: Vec<&Vec<u8>> = mns.iter().collect();
    sorted.sort();
    cases.push((
        "list_order_swapped",
        "snapshot",
        swap_adjacent(encoded(&base), sorted[1], sorted[2]),
    ));
    // The plain type's two active commitments, which encode to the same length, swapped.
    let q = base.quorums.iter().find(|q| q.llmq_type.to_base() == 100).expect("LLMQ_TEST");
    let mut actives: Vec<Vec<u8>> = q
        .active_commitments
        .iter()
        .map(|c| {
            let mut out = Vec::new();
            write_mined_commitment(c, &mut out);
            out
        })
        .collect();
    actives.sort();
    cases.push((
        "commitments_swapped",
        "snapshot",
        swap_adjacent(encoded(&base), &actives[0], &actives[1]),
    ));

    // Historical updates. The first replayed diff (the highest) gets the update.
    let update = |id: u64, fields: u32, f: &dyn Fn(&mut crate::types::MnCold)| {
        let mut cold = crate::types::MnCold::core_default();
        f(&mut cold);
        (
            id,
            crate::diff::MnStateDiff::from_parts(
                fields,
                crate::types::MnHot::default(),
                std::sync::Arc::new(cold),
            ),
        )
    };
    let with_updates = |updates: Vec<(u64, crate::diff::MnStateDiff)>| {
        let mut s = base.clone();
        s.historical_mn_list_diffs[0].diff.updated = updates;
        encoded(&s)
    };
    let port = crate::diff::MnStateDiff::PLATFORM_P2P_PORT;
    // A version-3 (ExtAddr) masternode given a platform port: Core serializes neither the state's
    // nor any list's port at that version, so every hash still matches (STRICTER).
    cases.push((
        "history_update_hidden_port",
        "snapshot",
        with_updates(vec![update(5, port, &|c| c.platform_p2p_port = 26656)]),
    ));
    cases.push((
        "history_update_missing_id",
        "snapshot",
        with_updates(vec![update(3, 0, &|_| {})]),
    ));
    // Two failing updates: Core applies them in hash-map order (CLASS_ONLY).
    cases.push((
        "history_updates_two_missing",
        "snapshot",
        with_updates(vec![update(3, 0, &|_| {}), update(4, 0, &|_| {})]),
    ));

    // AddMN's duplicate-property refusals through the canonical list reader.
    let mut dmns: Vec<crate::types::Dmn> =
        base.mn_list.iter().map(|d| crate::types::Dmn::clone(d)).collect();
    dmns.sort_by_key(|d| d.pro_tx_hash);
    let list_with = |f: &dyn Fn(&mut [crate::types::Dmn])| {
        let mut dmns = dmns.clone();
        f(&mut dmns);
        let mut w = W::default().h(42).i32(500).u32(10).compact(dmns.len() as u64);
        for dmn in &dmns {
            crate::codec::write_dmn(dmn, &mut w.0);
        }
        w.done()
    };
    cases.push((
        "mnlist_duplicate_owner",
        "mnlist",
        list_with(&|d| {
            let owner = d[0].cold.key_id_owner;
            std::sync::Arc::make_mut(&mut d[2].cold).key_id_owner = owner;
        }),
    ));
    cases.push((
        "mnlist_duplicate_collateral",
        "mnlist",
        list_with(&|d| d[2].collateral = d[0].collateral),
    ));

    // The per-masternode CompactSize budget, through a version-1 masternode's payout script: the
    // rest of the fixture's masternode takes 4 of it.
    for (name, len) in [
        ("mnlist_script_at_budget", MAX_MN_COMPACT_ITEMS_USIZE - 4),
        ("mnlist_script_over_budget", MAX_MN_COMPACT_ITEMS_USIZE - 3),
    ] {
        cases.push((
            name,
            "mnlist",
            list_with(&|d| {
                std::sync::Arc::make_mut(&mut d[2].cold).script_payout = vec![0x6a; len]
            }),
        ));
    }

    // Hash-prefix runs: copies of one regular masternode, all with the fixture's colliding
    // 64-bit prefix, each with its own unique properties.
    // A copy of a regular masternode with its own identity and unique properties; `prefix` fills
    // the hash's leading 64 bits.
    let fresh = |prefix: u8, i: u8| {
        let mut dmn = dmns[1].clone();
        let mut hash = [0u8; 32];
        hash[..8].fill(prefix);
        hash[8] = 0x10 + i;
        dmn.pro_tx_hash = crate::ProTxHash::from(hash);
        dmn.internal_id = 100 + u64::from(i);
        dmn.collateral.index = 100 + u32::from(i);
        let cold = std::sync::Arc::make_mut(&mut dmn.cold);
        let mut owner = *cold.key_id_owner.as_bytes();
        owner[19] = 0x80 + i;
        cold.key_id_owner = owner.into();
        if let dash_primitives::NetInfo::Legacy(v1) = &*cold.net_info {
            let mut v1 = v1.clone();
            v1.0.addr.0[15] = 0x40 + i;
            cold.net_info = std::sync::Arc::new(dash_primitives::NetInfo::Legacy(v1));
        }
        dmn
    };
    let run = |n: u8| {
        let mut w = W::default().h(42).i32(500).u32(200).compact(u64::from(n));
        for i in 0..n {
            crate::codec::write_dmn(&fresh(0xa5, i), &mut w.0);
        }
        w.done()
    };
    cases.push(("mnlist_prefix_run_at_limit", "mnlist", run(8)));
    cases.push(("mnlist_prefix_run_over", "mnlist", run(9)));

    // A public key canonical in the basic scheme only, in a legacy (version 1) commitment: Core
    // decodes it to the invalid object, which re-encodes as zeros.
    let legacy = dash_pkc::bls::BlsSchemeKind::from_legacy(true);
    let basic_only = (1u8..=255)
        .map(|seed| {
            let sk = dash_pkc::bls::BlsSecretKey::<dash_pkc::bls::BlsScIetf>::from_ikm(&[seed; 32])
                .expect("a key");
            sk.public_key().to_bytes()
        })
        .find(|pk| {
            dash_pkc::bls::BlsEagerPk::decode(pk, dash_pkc::bls::BlsSchemeSource::Version(legacy))
                .is_ok_and(|v| v.to_bytes(legacy) != *pk)
        })
        .expect("a key canonical only in the basic scheme");
    cases.push((
        "commitment_pk_basic_only_in_legacy",
        "commitment",
        commitment(1, 100, 8, &basic_only).done(),
    ));
    cases.push((
        "commitment_pk_basic_only_in_basic",
        "commitment",
        commitment(3, 100, 8, &basic_only).done(),
    ));

    // The reconstruction record budget, at the populated history's record count and one under
    // it: each entry charges the list it replays from plus its additions.
    let history =
        super::codec::sorted_by(&base.historical_mn_list_diffs, super::codec::history_key);
    let mut len = base.mn_list.len();
    let records: usize = history
        .iter()
        .map(|entry| {
            let charged = len + entry.diff.added.len();
            len = charged - entry.diff.removed.len();
            charged
        })
        .sum();
    let budget = |n: usize| &*format!("reconstruct:{n}").leak();
    cases.push(("reconstruct_at_record_budget", budget(records), encoded(&base)));
    cases.push(("reconstruct_over_record_budget", budget(records - 1), encoded(&base)));

    // The diff chain's other terms, each alone: on the newest entry (whose previous block is
    // the base) or the oldest (which no entry links to).
    let at_end = |newest: bool, f: &dyn Fn(&mut HistoricalMnListDiff)| {
        mutate(&|s| {
            let entry = if newest {
                s.historical_mn_list_diffs.iter_mut().max_by_key(|e| e.height)
            } else {
                s.historical_mn_list_diffs.iter_mut().min_by_key(|e| e.height)
            };
            f(entry.expect("a history"));
        })
    };
    let newest_target = base.historical_mn_list_diffs.iter().max_by_key(|e| e.height).unwrap();
    let (newest_block, base_block) = (newest_target.block_hash, base.base_block_hash);
    cases.extend([
        (
            "history_previous_unlinked",
            "snapshot",
            at_end(true, &|e| e.previous_block_hash = BlockHash::from([77; 32])),
        ),
        (
            "history_target_null",
            "snapshot",
            at_end(false, &|e| e.block_hash = BlockHash::default()),
        ),
        ("history_target_is_base", "snapshot", at_end(false, &|e| e.block_hash = base_block)),
        ("history_target_repeated", "snapshot", at_end(false, &|e| e.block_hash = newest_block)),
        ("history_height_negative", "snapshot", at_end(false, &|e| e.height = -1)),
        (
            "history_list_hash_null",
            "snapshot",
            at_end(true, &|e| e.canonical_list_hash = dash_num::Hash256::from([0; 32])),
        ),
        (
            "history_addition_reuses_identity",
            "snapshot",
            at_end(false, &|e| e.diff.added = vec![std::sync::Arc::new(dmns[1].clone())]),
        ),
        // A new masternode whose internal id the entry's counter does not cover: refused by the
        // invariants checked after the diff is applied.
        (
            "history_addition_over_counter",
            "snapshot",
            at_end(false, &|e| e.diff.added = vec![std::sync::Arc::new(fresh(0x3c, 0))]),
        ),
        // Each field within the update's own budget, the updated masternode over its budget.
        (
            "history_update_over_budget",
            "snapshot",
            with_updates(vec![update(
                dmns[2].internal_id,
                crate::diff::MnStateDiff::SCRIPT_PAYOUT,
                &|c| c.script_payout = vec![0x6a; MAX_MN_COMPACT_ITEMS_USIZE - 3],
            )]),
        ),
        (
            "rotation_snapshot_on_plain_type",
            "snapshot",
            mutate(&|s| {
                let r = rotated(s);
                let entry = s.quorums[r].rotation_snapshots[0].clone();
                let q = plain(s);
                s.quorums[q].rotation_snapshots = vec![entry];
            }),
        ),
        (
            "rotation_work_not_a_target",
            "snapshot",
            mutate(&|s| {
                let q = rotated(s);
                s.quorums[q].rotation_snapshots[0].work_block_hash = BlockHash::from([99; 32]);
            }),
        ),
    ]);
    // Two updates claiming one new owner key: the refusal names whichever Core applies second,
    // in hash-map order (CLASS_ONLY).
    let owner: [u8; 20] = [0x5e; 20];
    cases.push((
        "history_updates_duplicate_owner",
        "snapshot",
        with_updates(vec![
            update(5, crate::diff::MnStateDiff::KEY_ID_OWNER, &|c| c.key_id_owner = owner.into()),
            update(7, crate::diff::MnStateDiff::KEY_ID_OWNER, &|c| c.key_id_owner = owner.into()),
        ]),
    ));
    // The rotated type's first two rotation snapshots swapped.
    let q = base.quorums.iter().find(|q| q.llmq_type.to_base() == 103).expect("LLMQ_TEST_DIP0024");
    let entries: Vec<Vec<u8>> = q
        .rotation_snapshots
        .iter()
        .map(|e| {
            let mut out = Vec::new();
            write_rotation_snapshot(e, &mut out);
            out
        })
        .collect();
    cases.push((
        "rotation_snapshots_swapped",
        "snapshot",
        swap_adjacent(encoded(&base), &entries[0], &entries[1]),
    ));

    // The payout-share ceiling, on a version-3 masternode's payouts: `OP_TRUE` shares.
    let paid = dmns.iter().position(|d| d.cold.version >= 3).expect("a version-3 masternode");
    let share = <dash_primitives::PayoutShare as dash_types::codec::BaseCodec>::decode(
        &mut &[0x01, 0x51, 0x10, 0x27][..],
    )
    .expect("a payout share");
    for (name, n) in [
        ("mnlist_payouts_at_limit", MAX_PAYOUT_SHARES),
        ("mnlist_payouts_over", MAX_PAYOUT_SHARES + 1),
    ] {
        cases.push((
            name,
            "mnlist",
            list_with(&|d| {
                let cold = std::sync::Arc::make_mut(&mut d[paid].cold);
                cold.payouts = vec![share.clone(); n];
            }),
        ));
    }
    // Diff additions out of internal-id order, and with one id twice.
    let additions = |ids: [u64; 2]| {
        let mut w = W::default().compact(2);
        for (i, id) in ids.into_iter().enumerate() {
            let mut dmn = fresh(0x3c, i as u8);
            dmn.internal_id = id;
            crate::codec::write_dmn(&dmn, &mut w.0);
        }
        w.compact(0).compact(0).done()
    };
    cases.push(("mndiff_additions_descending", "mndiff", additions([9, 8])));
    cases.push(("mndiff_addition_id_repeated", "mndiff", additions([8, 8])));
    // Both bitsets at the ceiling, every byte present.
    cases.push((
        "commitment_full_size",
        "commitment",
        commitment(3, 100, MAX_QUORUM_SIZE as u64, &[0; 48]).done(),
    ));
    cases
}

/// `bytes` with the adjacent records `a` then `b` swapped.
fn swap_adjacent(mut bytes: Vec<u8>, a: &[u8], b: &[u8]) -> Vec<u8> {
    let joined = [a, b].concat();
    let at = bytes.windows(joined.len()).position(|w| w == joined).expect("adjacent records");
    bytes[at..at + joined.len()].copy_from_slice(&[b, a].concat());
    bytes
}

/// Inputs Core accepts and this crate refuses: its stricter state domain (O9b-notes; a state
/// holding a field its version cannot store, which no Core write site leaves). `(name, ours)`.
const STRICTER: &[(&str, &str)] =
    &[("history_update_hidden_port", "cannot store its platformP2PPort")];

/// Inputs with several failures Core reaches in hash-map order: only the verdict's class
/// (accepted or refused) is compared.
const CLASS_ONLY: &[&str] = &["history_updates_two_missing", "history_updates_duplicate_owner"];

/// The stricter domain, pinned: Core accepts each STRICTER input and this crate refuses it.
#[test]
fn stricter_domain_refuses_what_core_accepts() {
    let cases = crafted();
    for (name, ours) in STRICTER {
        let (_, kind, bytes) = cases.iter().find(|c| c.0 == *name).expect("a crafted case");
        assert!(core_verdict(name, kind, bytes).starts_with("ok 0 "), "{name}: Core accepts it");
        let verdict = verdict(kind, bytes);
        assert!(verdict.starts_with("fail ") && verdict.contains(ours), "{name}: {verdict}");
    }
}

/// A repeated rotation cycle (`snapshot.cpp` rc.4:306), which strict canonical order keeps
/// decoded input from reaching: refused when Validate runs without the order requirement.
#[test]
fn validate_refuses_constructed_repeated_cycle() {
    let mut snapshot = populated();
    let q = snapshot.quorums.iter().position(|q| q.rotation_enabled).expect("a rotated type");
    let snapshots = &mut snapshot.quorums[q].rotation_snapshots;
    snapshots[1].cycle_base_block_hash = snapshots[0].cycle_base_block_hash;
    assert_eq!(
        snapshot.validate(false, &net()).unwrap_err().to_string(),
        "invalid evo quorum rotation snapshot"
    );
}

/// Writes the crafted inputs for `test_dash_o9b`'s judge mode to `$O9B_CRAFTED`.
#[test]
#[ignore = "generator for contrib/node/o9b-evo-snapshot; run by hand"]
#[expect(
    clippy::disallowed_methods,
    reason = "a test-only generator; the environment names its output"
)]
fn gen_crafted() {
    use std::fmt::Write as _;
    let path = std::env::var("O9B_CRAFTED").expect("O9B_CRAFTED names the output");
    let mut out = String::new();
    for (name, kind, bytes) in crafted() {
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        writeln!(out, "{name} {kind} {hex}").unwrap();
    }
    std::fs::write(path, out).expect("writes");
}

/// Core's verdict for the crafted case `name`, after checking the input, and the kind it was
/// judged as, are the ones Core judged.
fn core_verdict(name: &str, kind: &str, bytes: &[u8]) -> String {
    let line = VERDICTS
        .lines()
        .find(|l| l.split(' ').next() == Some(name))
        .unwrap_or_else(|| panic!("no Core verdict for {name}"));
    let mut fields = line.splitn(4, ' ');
    let (_, core_kind, hash, verdict) =
        (fields.next(), fields.next().unwrap(), fields.next().unwrap(), fields.next().unwrap());
    assert_eq!(core_kind, kind, "{name}: not the kind Core judged");
    assert_eq!(hash, sha256_hex(bytes), "{name}: not the input Core judged");
    normalized(verdict)
}

/// Test 2: every crafted case gets Core's verdict here.
#[test]
fn crafted_verdicts_match_core() {
    let cases = crafted();
    let mut names = std::collections::BTreeSet::new();
    for (name, kind, bytes) in &cases {
        assert!(names.insert(*name), "{name} twice");
        let (ours, core) = (verdict(kind, bytes), core_verdict(name, kind, bytes));
        if STRICTER.iter().any(|(n, _)| n == name) {
            continue;
        } else if CLASS_ONLY.contains(name) {
            assert_eq!(ours.split(' ').next(), core.split(' ').next(), "{name}");
        } else {
            assert_eq!(ours, core, "{name}");
        }
    }
    assert_eq!(VERDICTS.lines().filter(|l| !l.is_empty()).count(), cases.len(), "stale verdicts");
}
