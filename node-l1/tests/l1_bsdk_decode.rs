//! BS4b: base-sdk's decode domain against Dash Core v24's oracle for `Transaction`, `Block`,
//! `BlockHeader` and the special-transaction payloads of types 1 to 12 (`diff_bsdk_tx_decode`,
//! `diff_bsdk_block_decode`, `diff_bsdk_header_decode`, `diff_payload_decode`).
//!
//! - `rows_agree_with_core`: every named row (`support/bsdk_rows.rs`) is compared like a fuzz
//!   input and must show the expected agreement.
//! - `recorded_rows_match_the_oracle`: Core's verdicts on the rows are committed in
//!   `data/bsdk_decode_recorded.json`; `bsdk_recorded` replays them without the oracle.
//!   Regenerate with `NODE_L1_REGENERATE=1 cargo test -p node-l1 --features oracle --test
//!   l1_bsdk_decode`, then review the diff.
//! - `census`: the rows plus a fixed number of seeded mutations of every seed, all compared. It
//!   fails on a divergence, and unless every evidence class of the rows was reached (with both
//!   outcomes where it has both: all but DDE #1, #4 and the version matrix), every payload type and version
//!   was compared under both BLS scheme flags, and every version a seed names was accepted
//!   under both, and the DDE #10 floor holds (`floor_problems`: every BLS type, version, field and
//!   class compared under both flags). `BS4B_CENSUS_CASES` sets the mutations per seed. Classes: the rows' DDE #1, #3,
//!   #4, #6, #10, the generic rules and the version matrix; the mutated transactions and blocks
//!   keep their versions in `0..=0x7fff`.
//!
//! Output: one line per class and per payload type with counts; no input.

#![deny(clippy::use_debug, clippy::dbg_macro, clippy::print_stdout)]

#[path = "support/bsdk_recorded.rs"]
mod bsdk_recorded;
#[path = "support/bsdk_rows.rs"]
mod bsdk_rows;
#[path = "support/spec.rs"]
mod spec;

/// The library's module under the name the shared support files use.
mod bsdk_decode {
    pub use node_l1::bsdk_decode::*;
}

use std::collections::{BTreeMap, BTreeSet};

use bsdk_decode::{
    Agreement, CoreDecode, CorePayload, Judged, PAYLOAD_TYPES, judge_block, judge_header,
    judge_payload, judge_tx, split_payload_input,
};
use bsdk_recorded::{Recorded, RecordedCore, RecordedRow, input_digest};
use bsdk_rows::{
    BLOCK, Expect, HEADER, MAX_SIZE, PAYLOAD, Row, TX, floor_problems, payload_seeds, rows, seeds,
    versions,
};
use dash_oracle_sys::{ErrorKind, Oracle, Pin};
use dash_types::codec::BaseCodec;
use node_l1::{Outcome, core_payload, target};
use node_sink::sink::record_for_test;
use spec::TxSpec;
use spec::generate::Rng;

const PIN: Pin = Pin::V24_0;

fn core_object(oracle: &Oracle, target: &str, input: &[u8]) -> Option<CoreDecode> {
    let (consumed, reencoded) = match target {
        TX => oracle.tx_decode(input).map(|d| (d.consumed, d.reencoded)),
        BLOCK => oracle.block_decode(input).map(|d| (d.consumed, d.reencoded)),
        HEADER => oracle.header_decode(input).map(|d| (d.consumed, d.reencoded.to_vec())),
        other => panic!("not an object target: {other}"),
    }
    .map_err(|e| assert_eq!(e.kind, ErrorKind::Decode, "{target}: the oracle failed: {e}"))
    .ok()?;
    Some(CoreDecode {
        consumed,
        reencoded,
    })
}

/// Core's verdicts on a payload row, the judge's results and the agreement.
fn payload_verdicts(oracle: &Oracle, input: &[u8]) -> ([Option<CorePayload>; 2], [Judged; 2]) {
    let (tx_type, payload) =
        split_payload_input(input, PAYLOAD_TYPES).expect("a payload row names its type");
    let core =
        core_payload(oracle, tx_type, payload).expect("the oracle decodes a payload or rejects it");
    let judged = [
        judge_payload(tx_type, false, payload, core[0].as_ref()),
        judge_payload(tx_type, true, payload, core[1].as_ref()),
    ];
    (core, judged)
}

fn agreement(judged: &[Judged]) -> Result<Agreement, &'static str> {
    let mut accepted = false;
    for j in judged {
        match j {
            Ok(Agreement::BothAccept) => accepted = true,
            Ok(Agreement::BothReject) => {}
            Err(field) => return Err(field),
        }
    }
    Ok(if accepted {
        Agreement::BothAccept
    } else {
        Agreement::BothReject
    })
}

#[test]
fn rows_agree_with_core() {
    let mut names = BTreeSet::new();
    let mut failures = Vec::new();
    for row in rows() {
        assert!(names.insert((row.target, row.name.clone())), "{}: a duplicate name", row.name);
        let compare = target(row.target).expect("a registered target").compare;
        match (compare(&row.input, PIN), row.expect) {
            (Ok(Outcome::BothAccept), Expect::Accept | Expect::Either)
            | (Ok(Outcome::BothReject), Expect::Reject | Expect::Either) => {}
            (Ok(_), _) => failures.push(format!("{}: not the expected agreement", row.name)),
            (Err(divergence), _) => failures.push(format!("{}: {divergence}", row.name)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// DDE #3 at the size limit, generated rather than recorded: a transaction whose extra payload
/// is `MAX_SIZE - 1` and `MAX_SIZE` bytes is read by both sides; one of `MAX_SIZE + 1` is refused
/// by both on its length.
#[test]
fn extra_payload_at_max_size() {
    let compare = target(TX).expect("the transaction target").compare;
    let max = usize::try_from(MAX_SIZE).expect("a size");
    for (len, want) in
        [(max - 1, Outcome::BothAccept), (max, Outcome::BothAccept), (max + 1, Outcome::BothReject)]
    {
        let input = TxSpec::special(1, vec![0xab; len]).bytes();
        match compare(&input, PIN) {
            Ok(got) => assert_eq!(got, want, "extra payload of {len} bytes"),
            Err(field) => panic!("extra payload of {len} bytes: {field}"),
        }
    }
}

fn recorded_row(oracle: &Oracle, row: &Row) -> RecordedRow {
    let (core, judged) = if row.target == PAYLOAD {
        let (core, judged) = payload_verdicts(oracle, &row.input);
        (core.map(|c| c.as_ref().map(RecordedCore::of_payload)).to_vec(), judged.to_vec())
    } else {
        let core = core_object(oracle, row.target, &row.input);
        let judged = match row.target {
            TX => judge_tx(&row.input, core.as_ref()),
            BLOCK => judge_block(&row.input, core.as_ref()),
            _ => judge_header(&row.input, core.as_ref()),
        };
        (vec![core.as_ref().map(RecordedCore::of_object)], vec![judged])
    };
    let agreed = agreement(&judged).unwrap_or_else(|field| panic!("{}: {field}", row.name));
    RecordedRow {
        target: row.target.into(),
        name: row.name.clone(),
        tag: row.tag.into(),
        input_sha256: input_digest(&row.input),
        agreement: if agreed == Agreement::BothAccept {
            "accept"
        } else {
            "reject"
        }
        .into(),
        core,
    }
}

fn compute() -> Recorded {
    let oracle = Oracle::new(PIN);
    Recorded {
        provenance: format!(
            "Dash Core's decode of the BS4b rows (node-l1/tests/support/bsdk_rows.rs, rows()), computed by the oracle (dash-oracle-sys) at pin {}; regenerate with node-l1's l1_bsdk_decode test.",
            oracle.core_version()
        ),
        rows: rows().iter().map(|row| recorded_row(&oracle, row)).collect(),
    }
}

/// The committed verdicts must equal the oracle's. The provenance line names the oracle build,
/// so it changes with every shim revision and is not compared.
#[test]
fn recorded_rows_match_the_oracle() {
    let computed = compute();
    if std::env::var_os("NODE_L1_REGENERATE").is_some() {
        let json = serde_json::to_string_pretty(&computed).expect("serializes") + "\n";
        std::fs::write(bsdk_recorded::path(), json).expect("writes the recorded verdicts");
        return;
    }
    let mut committed = bsdk_recorded::load();
    committed.provenance.clone_from(&computed.provenance);
    assert!(
        committed == computed,
        "the committed verdicts differ from the oracle; regenerate and review"
    );
}

// ---------------------------------------------------------------------------------------------
// The census.

/// Seeded mutations: a few of truncating, extending, overwriting a window, flipping a bit,
/// inserting, deleting, and setting one byte to a boundary value, after `skip` leading bytes.
fn mutate(rng: &mut Rng, mut bytes: Vec<u8>, skip: usize) -> Vec<u8> {
    for _ in 0..1 + rng.below(3) {
        if bytes.len() <= skip {
            return bytes;
        }
        let at = skip + rng.below((bytes.len() - skip) as u64) as usize;
        match rng.below(7) {
            0 => bytes.truncate(at),
            1 => {
                let n = 1 + rng.below(8) as usize;
                bytes.extend(rng.bytes(n));
            }
            2 => {
                for i in at..(at + 4).min(bytes.len()) {
                    bytes[i] = rng.next_u64() as u8;
                }
            }
            3 => bytes[at] ^= 1 << rng.below(8),
            4 => {
                let n = 1 + rng.below(4) as usize;
                for (i, byte) in rng.bytes(n).into_iter().enumerate() {
                    bytes.insert(at + i, byte);
                }
            }
            5 => {
                let n = (1 + rng.below(6) as usize).min(bytes.len() - at);
                bytes.drain(at..at + n);
            }
            _ => bytes[at] = rng.pick(&[0, 1, 0x7f, 0x80, 0xfc, 0xfd, 0xfe, 0xff]),
        }
    }
    bytes
}

/// The version a payload names, as the seeds' names carry it.
fn payload_version(tx_type: u16, payload: &[u8]) -> Option<u16> {
    match tx_type {
        7..=9 => payload.first().map(|&v| u16::from(v)),
        6 => Some(u16::from_le_bytes(payload.get(6..8)?.try_into().ok()?)),
        _ => Some(u16::from_le_bytes(payload.get(..2)?.try_into().ok()?)),
    }
}

#[derive(Default)]
struct Tally {
    cases: u64,
    accepted: u64,
}

#[derive(Default)]
struct Census {
    /// Per target: (accepted, rejected) compared inputs.
    objects: BTreeMap<&'static str, (u64, u64)>,
    /// Per (type, version, legacy flag).
    payloads: BTreeMap<(u16, u16, bool), Tally>,
    /// Per tag: (rows accepted, rows rejected).
    tags: BTreeMap<&'static str, (u64, u64)>,
    failures: Vec<String>,
}

impl Census {
    fn object(&mut self, target_name: &'static str, input: &[u8]) -> Option<Outcome> {
        let compare = target(target_name).expect("a registered target").compare;
        match compare(input, PIN) {
            Ok(outcome) => {
                let counts = self.objects.entry(target_name).or_default();
                match outcome {
                    Outcome::BothAccept => counts.0 += 1,
                    Outcome::BothReject => counts.1 += 1,
                    Outcome::OutOfDomain | Outcome::StrictFlag | Outcome::Discarded => {
                        panic!("{target_name}: the decode comparisons compare every input")
                    }
                }
                Some(outcome)
            }
            Err(divergence) => {
                let recorded = record_for_test(target_name, input, PIN.name(), divergence.field);
                self.failures.push(format!("{target_name}: {divergence} ({recorded})"));
                None
            }
        }
    }

    fn payload(&mut self, oracle: &Oracle, input: &[u8]) -> Option<Outcome> {
        let (tx_type, payload) = split_payload_input(input, PAYLOAD_TYPES)?;
        let (_, judged) = payload_verdicts(oracle, input);
        let mut accepted = false;
        for (judged, legacy) in judged.into_iter().zip([false, true]) {
            match judged {
                Ok(agreement) => {
                    accepted |= agreement == Agreement::BothAccept;
                    if let Some(version) = payload_version(tx_type, payload) {
                        let tally = self.payloads.entry((tx_type, version, legacy)).or_default();
                        tally.cases += 1;
                        tally.accepted += u64::from(agreement == Agreement::BothAccept);
                    }
                }
                Err(field) => {
                    let recorded = record_for_test(PAYLOAD, input, PIN.name(), field);
                    self.failures.push(format!(
                        "{PAYLOAD}: {field} on the {} flag ({recorded})",
                        if legacy {
                            "legacy"
                        } else {
                            "basic"
                        }
                    ));
                    return None;
                }
            }
        }
        let counts = self.objects.entry(PAYLOAD).or_default();
        if accepted {
            counts.0 += 1;
            Some(Outcome::BothAccept)
        } else {
            counts.1 += 1;
            Some(Outcome::BothReject)
        }
    }

    fn input(
        &mut self,
        oracle: &Oracle,
        target_name: &'static str,
        input: &[u8],
    ) -> Option<Outcome> {
        if target_name == PAYLOAD {
            self.payload(oracle, input)
        } else {
            self.object(target_name, input)
        }
    }
}

/// Whether every transaction `bytes` holds, as base-sdk reads them, has a version in `0..=0x7fff`
/// (an input base-sdk cannot read is not restricted).
fn versions_in_range(target_name: &str, bytes: &[u8]) -> bool {
    match target_name {
        TX => bytes.get(1).is_none_or(|high| high & 0x80 == 0),
        BLOCK => dash_primitives::Block::decode(&mut &bytes[..])
            .map_or(true, |block| block.transactions.iter().all(|tx| tx.version >= 0)),
        _ => true,
    }
}

#[test]
fn census() {
    let cases: u64 =
        std::env::var("BS4B_CENSUS_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let oracle = Oracle::new(PIN);
    let mut census = Census::default();

    // The rows: each shows its expected agreement and counts toward its class.
    for row in rows() {
        let Some(outcome) = census.input(&oracle, row.target, &row.input) else {
            continue;
        };
        let counts = census.tags.entry(row.tag).or_default();
        match (outcome, row.expect) {
            (Outcome::BothAccept, Expect::Accept | Expect::Either) => counts.0 += 1,
            (Outcome::BothReject, Expect::Reject | Expect::Either) => counts.1 += 1,
            _ => census.failures.push(format!("{}: not the expected agreement", row.name)),
        }
    }

    // Seeded mutations of every seed.
    for (index, target_name) in [TX, BLOCK, HEADER, PAYLOAD].into_iter().enumerate() {
        let skip = usize::from(target_name == PAYLOAD);
        let mut rng = Rng::new(0xb54b_0000 + index as u64);
        for (_, seed) in seeds(target_name) {
            census.input(&oracle, target_name, &seed);
            for _ in 0..cases {
                let mutated = mutate(&mut rng, seed.clone(), skip);
                if versions_in_range(target_name, &mutated) {
                    census.input(&oracle, target_name, &mutated);
                }
            }
        }
    }

    // Report.
    for (tag, (accepted, rejected)) in &census.tags {
        eprintln!("rows {tag}: {accepted} accepted, {rejected} rejected");
    }
    for (target_name, (accepted, rejected)) in &census.objects {
        eprintln!("{target_name}: {accepted} accepted, {rejected} rejected");
    }
    for tx_type in 1..=PAYLOAD_TYPES {
        let mut line = Vec::new();
        for (&(_, version, _), basic) in
            census.payloads.iter().filter(|&(&(t, _, legacy), _)| t == tx_type && !legacy)
        {
            let legacy = &census.payloads[&(tx_type, version, true)];
            line.push(format!(
                "v{version} {}/{} basic {}/{} legacy",
                basic.accepted, basic.cases, legacy.accepted, legacy.cases
            ));
        }
        eprintln!("type {tx_type} accepted/compared: {}", line.join(", "));
    }

    assert!(census.failures.is_empty(), "{}", census.failures.join("\n"));

    // Every evidence class was reached, with both outcomes where the class has both.
    for tag in ["DDE#1", "DDE#3", "DDE#4", "DDE#6", "DDE#10", "sweep", "matrix"] {
        let (accepted, rejected) = census.tags.get(tag).copied().unwrap_or_default();
        assert!(accepted > 0, "{tag}: no row accepted");
        assert!(
            rejected > 0 || matches!(tag, "DDE#1" | "DDE#4" | "matrix"),
            "{tag}: no row rejected"
        );
    }
    for target_name in [TX, BLOCK, HEADER, PAYLOAD] {
        let (accepted, rejected) = census.objects.get(target_name).copied().unwrap_or_default();
        assert!(accepted > 0 && rejected > 0, "{target_name}: only one outcome reached");
    }
    // DDE #10: every (type, version, field, class) of the floor was compared under both flags,
    // with the outcome of its field kind.
    let all = rows();
    let judged: Vec<(&Row, [Judged; 2])> = all
        .iter()
        .filter(|row| row.target == PAYLOAD)
        .map(|row| (row, payload_verdicts(&oracle, &row.input).1))
        .collect();
    let problems = floor_problems(&judged);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    // Every payload type at every version, under both flags; every version a seed names accepted
    // under both.
    for tx_type in 1..=PAYLOAD_TYPES {
        let seeded: BTreeSet<u16> = payload_seeds(tx_type).iter().map(|(v, _)| *v).collect();
        for &version in versions(tx_type) {
            for legacy in [false, true] {
                let tally = census.payloads.get(&(tx_type, version, legacy));
                let flag = if legacy {
                    "legacy"
                } else {
                    "basic"
                };
                assert!(
                    tally.is_some_and(|t| t.cases > 0),
                    "type {tx_type} v{version}: never compared on the {flag} flag"
                );
                assert!(
                    !seeded.contains(&version) || tally.is_some_and(|t| t.accepted > 0),
                    "type {tx_type} v{version}: never accepted on the {flag} flag"
                );
            }
        }
    }
}
