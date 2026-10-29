//! The BS4b rows: named inputs for the four decode targets, built from the seeds in
//! `tests/data/bsdk-seeds` (cut from Core-produced data by `scripts/gen-bsdk-seeds.py`) and
//! from `spec.rs`'s builders. Dash Core judges them: `l1_bsdk_decode` compares every row live
//! and records the verdicts in `tests/data/bsdk_decode_recorded.json`, which `bsdk_recorded`
//! replays without the oracle.
//!
//! The including test declares `mod spec` (`support/spec.rs`) and `mod bsdk_rows`.
//!
//! Every row carries a `tag`, the evidence class it belongs to:
//! - `DDE#1`, `DDE#3`, `DDE#4`, `DDE#6`, `DDE#10`: the rows of A1 §1.4's table that a transaction,
//!   block or payload decoder reaches (`TxOut.value`, the extra payload length, `LlmqType`,
//!   payload consumption, BLS fields). The other rows are P2P (`DDE#2`, `#5`, `#7`, `#8`, `#9`)
//!   and belong to BS7's targets.
//! - `sweep`: the generic rules of DDE-SWEEP.md §2 (CompactSize canonicality, `MAX_SIZE`,
//!   `COMPACTSIZE` field range, signedness).
//! - `matrix`: one payload per type and version.
//!
//! The `DDE#10` rows carry [`Cover`] metadata: the payload type, version, BLS field and class of
//! bytes they put in the field. [`BLS_FIELDS`] lists every field and version that must be
//! covered, and [`floor_problems`] checks that each (type, version, field, class) has a row that
//! both sides judged, under both BLS scheme flags, with the outcome its field kind gives. A valid
//! object is covered in each encoding (basic and legacy), so the version's own scheme and the
//! opposite one are both there; where the two encodings are the same bytes one row covers both.

#![allow(dead_code)]

use std::path::PathBuf;

use crate::bsdk_decode::{Agreement, Judged, PAYLOAD_TYPES};
use dash_pkc::bls::{BLS_SIG_LEN, BlsCtx, BlsSchemeKind};
use dash_primitives::SpecialPayload;
use dash_types::Numeric;

use crate::spec::{MAX_MONEY, TxSpec, block_of, header};

pub const TX: &str = "diff_bsdk_tx_decode";
pub const BLOCK: &str = "diff_bsdk_block_decode";
pub const HEADER: &str = "diff_bsdk_header_decode";
pub const PAYLOAD: &str = "diff_payload_decode";

/// What a row is expected to show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Expect {
    /// Both sides decode it (under at least one BLS flag, for a payload).
    Accept,
    /// Both sides reject it.
    Reject,
    /// Either, but they must agree (BLS rows, whose verdict depends on the flag).
    Either,
}

pub struct Row {
    pub target: &'static str,
    pub name: String,
    pub tag: &'static str,
    pub expect: Expect,
    pub input: Vec<u8>,
    /// The coverage cells the row stands for (the `DDE#10` rows only).
    pub covers: Vec<Cover>,
}

/// The bytes a `DDE#10` row puts in a BLS field.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Class {
    /// Zeros: no object.
    Zero,
    /// A valid point in its basic-scheme encoding.
    Basic,
    /// A valid point in its legacy-scheme encoding. Where the two encodings are the same bytes,
    /// one row covers both classes.
    Legacy,
    /// Bytes canonical in neither scheme.
    Neither,
}

impl Class {
    pub const ALL: [Class; 4] = [Class::Zero, Class::Basic, Class::Legacy, Class::Neither];

    fn label(self) -> &'static str {
        match self {
            Class::Zero => "zero",
            Class::Basic => "valid basic",
            Class::Legacy => "valid legacy",
            Class::Neither => "neither scheme",
        }
    }
}

/// One coverage cell.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Cover {
    pub tx_type: u16,
    pub version: u16,
    /// The field's index among the payload's BLS fields.
    pub field: usize,
    pub class: Class,
}

/// How a payload holds a BLS field, which decides what Core accepts of its bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldKind {
    /// Kept as read until used: any bytes are accepted.
    Lazy,
    /// Read under the process-wide scheme flag: bytes canonical in neither scheme are refused.
    Global,
    /// Read under a fixed scheme, or the one the object's version names: bytes canonical in
    /// neither are refused.
    Eager,
}

/// A BLS field of a payload type and the versions it must be covered at: those at which Core
/// reads the field (types 1 to 4 read the version only at the unknown versions on both ends of
/// their range, and the coinbase commitment reads the signature from version 3).
pub struct BlsField {
    pub tx_type: u16,
    pub field: usize,
    pub kind: FieldKind,
    pub versions: &'static [u16],
}

pub const BLS_FIELDS: &[BlsField] = &[
    BlsField {
        tx_type: 1,
        field: 0,
        kind: FieldKind::Lazy,
        versions: &[1, 2, 3],
    },
    BlsField {
        tx_type: 2,
        field: 0,
        kind: FieldKind::Eager,
        versions: &[1, 2, 3],
    },
    BlsField {
        tx_type: 3,
        field: 0,
        kind: FieldKind::Lazy,
        versions: &[1, 2, 3],
    },
    BlsField {
        tx_type: 4,
        field: 0,
        kind: FieldKind::Eager,
        versions: &[1, 2, 3],
    },
    BlsField {
        tx_type: 5,
        field: 0,
        kind: FieldKind::Global,
        versions: &[3, 4, 5],
    },
    BlsField {
        tx_type: 6,
        field: 0,
        kind: FieldKind::Eager,
        versions: &[0, 1, 2, 3, 4, 5],
    },
    BlsField {
        tx_type: 6,
        field: 1,
        kind: FieldKind::Eager,
        versions: &[0, 1, 2, 3, 4, 5],
    },
    BlsField {
        tx_type: 6,
        field: 2,
        kind: FieldKind::Eager,
        versions: &[0, 1, 2, 3, 4, 5],
    },
    BlsField {
        tx_type: 7,
        field: 0,
        kind: FieldKind::Eager,
        versions: &[0, 1, 2, 3],
    },
    BlsField {
        tx_type: 9,
        field: 0,
        kind: FieldKind::Global,
        versions: &[0, 1, 2, 3],
    },
    BlsField {
        tx_type: 12,
        field: 0,
        kind: FieldKind::Lazy,
        versions: &[0, 1, 2, 3],
    },
];

/// The cells of [`BLS_FIELDS`] that no row covers, or that a row compared with the wrong outcome
/// for its field kind. `judged` is each payload row with the judge's result under the basic, then
/// the legacy BLS scheme flag. Core reads zeros and a valid object in either encoding under both
/// flags; bytes canonical in neither scheme only where the field is lazy. A comparison that
/// diverged covers nothing.
pub fn floor_problems(judged: &[(&Row, [Judged; 2])]) -> Vec<String> {
    let mut problems = Vec::new();
    for spec in BLS_FIELDS {
        for &version in spec.versions {
            for class in Class::ALL {
                let cell = Cover {
                    tx_type: spec.tx_type,
                    version,
                    field: spec.field,
                    class,
                };
                let name = format!(
                    "type {} v{version} field {}: {}",
                    spec.tx_type,
                    spec.field,
                    class.label()
                );
                let hits: Vec<_> =
                    judged.iter().filter(|(row, _)| row.covers.contains(&cell)).collect();
                if hits.is_empty() {
                    problems.push(format!("{name}: no row"));
                }
                let accepts = !(class == Class::Neither && spec.kind != FieldKind::Lazy);
                for (row, flags) in hits {
                    let want = if accepts {
                        Agreement::BothAccept
                    } else {
                        Agreement::BothReject
                    };
                    if flags.iter().any(|j| *j != Ok(want)) {
                        let verb = if accepts {
                            "accepted"
                        } else {
                            "refused"
                        };
                        problems.push(format!(
                            "{name}: row `{}` is not {verb} under both flags",
                            row.name
                        ));
                    }
                }
            }
        }
    }
    problems
}

/// The most a CompactSize length may be (Core's `MAX_SIZE`).
pub const MAX_SIZE: u64 = 0x0200_0000;

/// The versions each special-transaction type is covered at, as the payload names them (the
/// first `u16`; a `u8` for types 7 to 9; the commitment's own version for type 6). The two
/// unknown ends of types 1 to 4 are Core's "version it does not know" path, where it reads the
/// version only.
pub fn versions(tx_type: u16) -> &'static [u16] {
    match tx_type {
        1..=4 => &[0, 1, 2, 3, 4],
        5 | 6 => &[0, 1, 2, 3, 4, 5],
        7..=9 => &[0, 1, 2, 3],
        _ => &[0, 1, 2, 3],
    }
}

fn seed_dir(target: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/bsdk-seeds").join(target)
}

/// The seeds of `target`: (file name, bytes), in name order.
pub fn seeds(target: &str) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(seed_dir(target))
        .expect("the seed directory")
        .map(|e| {
            let e = e.expect("a directory entry");
            (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).expect("a seed"))
        })
        .collect();
    out.sort();
    out
}

/// The payloads of type `tx_type` among the seeds, with the version each names.
pub fn payload_seeds(tx_type: u16) -> Vec<(u16, Vec<u8>)> {
    let prefix = format!("t{tx_type}-p");
    seeds(PAYLOAD)
        .into_iter()
        .filter(|(name, _)| name.starts_with(&prefix))
        .map(|(name, bytes)| {
            let version = name[prefix.len()..].split('-').next().and_then(|v| v.parse().ok());
            (version.expect("a seed name carries its version"), bytes[1..].to_vec())
        })
        .collect()
}

/// A `diff_payload_decode` input.
pub fn payload_input(tx_type: u16, payload: &[u8]) -> Vec<u8> {
    let selector = u8::try_from(tx_type - 1).expect("a type of 1 to 12");
    [&[selector][..], payload].concat()
}

/// Offset of the commitment's own version in a type-6 payload (after the payload version and
/// height); the LLMQ type follows it, then the quorum hash, then the index of indexed versions.
const QC_VERSION: usize = 6;
const QC_INDEX: usize = QC_VERSION + 2 + 1 + 32;

fn is_indexed(version: u16) -> bool {
    version == 2 || version == 4
}

/// `payload` with the version it names set to `version`.
fn with_version(tx_type: u16, payload: &[u8], version: u16) -> Vec<u8> {
    let mut out = payload.to_vec();
    match tx_type {
        7..=9 => out[0] = u8::try_from(version).expect("a u8 version"),
        6 => {
            let from = u16::from_le_bytes([out[QC_VERSION], out[QC_VERSION + 1]]);
            out[QC_VERSION..QC_VERSION + 2].copy_from_slice(&version.to_le_bytes());
            // The index field exists exactly for the indexed versions.
            if is_indexed(from) && !is_indexed(version) {
                out.drain(QC_INDEX..QC_INDEX + 2);
            } else if !is_indexed(from) && is_indexed(version) {
                out.splice(QC_INDEX..QC_INDEX, [0, 0]);
            }
        }
        _ => out[..2].copy_from_slice(&version.to_le_bytes()),
    }
    out
}

fn payload_row(
    tx_type: u16,
    name: String,
    tag: &'static str,
    expect: Expect,
    payload: &[u8],
) -> Row {
    Row {
        target: PAYLOAD,
        name,
        tag,
        expect,
        input: payload_input(tx_type, payload),
        covers: Vec::new(),
    }
}

/// Every BLS field of a payload as base-sdk decoded it: its object's bytes under the basic and
/// the legacy scheme (zeros for an invalid object). A field the payload holds as valid in its
/// own scheme shows its wire bytes in one of the two.
fn bls_fields(payload: &SpecialPayload) -> Vec<[Vec<u8>; 2]> {
    fn both<K: dash_pkc::bls::BlsWirePoint<N>, const N: usize>(
        value: &dash_pkc::bls::BlsValue<K, N>,
    ) -> [Vec<u8>; 2] {
        [
            value.to_bytes(BlsSchemeKind::Basic).to_vec(),
            value.to_bytes(BlsSchemeKind::Legacy).to_vec(),
        ]
    }
    match payload {
        SpecialPayload::ProviderRegister(p) => vec![both(&p.pub_key_operator.clone().get())],
        SpecialPayload::ProviderUpdateRegistrar(p) => vec![both(&p.pub_key_operator.clone().get())],
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
            vec![both(&p.pub_key_operator.clone().get())]
        }
        SpecialPayload::ProviderUpdateService(p) => vec![both(p.sig.value())],
        SpecialPayload::ProviderUpdateRevoke(p) => vec![both(p.sig.value())],
        SpecialPayload::MnhfSignal(p) => vec![both(p.sig.value())],
        SpecialPayload::QuorumCommitment(p) => {
            let c = &p.commitment;
            vec![
                both(c.quorum_public_key.value()),
                both(c.quorum_sig.value()),
                both(c.members_sig.value()),
            ]
        }
        SpecialPayload::CoinbaseCommitment(p) => {
            p.best_cl_signature.iter().map(|s| both(s.get(BlsCtx::BASIC).value())).collect()
        }
        SpecialPayload::AssetUnlock(p) => vec![both(p.quorum_sig.get(BlsCtx::BASIC).value())],
        _ => Vec::new(),
    }
}

/// base-sdk's view of the operator key of a type 1, 3 or 12 payload: the scheme it captured
/// from the version, and whether the key is valid in it (`None` for any other payload).
fn operator_key(tx_type: u16, payload: &[u8]) -> Option<(BlsSchemeKind, bool)> {
    fn of<K: dash_pkc::bls::BlsWirePoint<48>>(
        key: &dash_pkc::bls::BlsLazy<K, 48>,
    ) -> (BlsSchemeKind, bool) {
        (key.scheme(), key.clone().get().is_valid())
    }
    Some(match decoded(tx_type, payload)? {
        SpecialPayload::ProviderRegister(p) => of(&p.pub_key_operator),
        SpecialPayload::ProviderUpdateRegistrar(p) => of(&p.pub_key_operator),
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => of(&p.pub_key_operator),
        _ => return None,
    })
}

fn decoded(tx_type: u16, payload: &[u8]) -> Option<SpecialPayload> {
    let mut cursor = payload;
    SpecialPayload::decode(dash_primitives::TxType::from_base(tx_type), &mut cursor).ok()
}

/// The object of BLS field `field` of `payload` in its basic and legacy encodings, `None` when
/// the payload does not decode or the field is null.
fn object_of(tx_type: u16, payload: &[u8], field: usize) -> Option<[Vec<u8>; 2]> {
    let encodings = bls_fields(&decoded(tx_type, payload)?).into_iter().nth(field)?;
    encodings.iter().any(|b| b.iter().any(|&x| x != 0)).then_some(encodings)
}

/// Where the wire bytes of an object sit in `payload`: the offset and bytes of the encoding that
/// occurs exactly once.
fn placed(payload: &[u8], encodings: &[Vec<u8>; 2]) -> Option<(usize, Vec<u8>)> {
    encodings.iter().find_map(|bytes| {
        let mut at = payload.windows(bytes.len()).enumerate().filter(|(_, w)| *w == &bytes[..]);
        let (first, _) = at.next()?;
        at.next().is_none().then(|| (first, bytes.clone()))
    })
}

/// A payload with a valid object in one BLS field, and where the field sits in it.
struct Witness {
    payload: Vec<u8>,
    at: usize,
    /// The field's wire bytes in `payload`.
    wire: Vec<u8>,
    /// The object in the basic and the legacy encoding.
    encodings: [Vec<u8>; 2],
}

/// A payload of `tx_type` at `version` with a valid object in BLS field `field`: a seed where
/// one carries it (the chains' null fields do not count), otherwise the object of another seed
/// moved to the version by re-encoding the version (`with_version`; for a commitment it also adds
/// or drops the index), except the version 3 coinbase commitment, whose layout differs from the
/// seeds' that hold a signature: the signature is written over the null one before the credit
/// pool balance.
fn witness(tx_type: u16, version: u16, field: usize) -> Option<Witness> {
    let seeds = payload_seeds(tx_type);
    let natural = seeds.iter().filter(|(v, _)| *v == version).find_map(|(_, payload)| {
        let encodings = object_of(tx_type, payload, field)?;
        let (at, wire) = placed(payload, &encodings)?;
        Some(Witness {
            payload: payload.clone(),
            at,
            wire,
            encodings,
        })
    });
    if natural.is_some() {
        return natural;
    }
    let (donor, encodings, wire) = seeds.iter().find_map(|(_, payload)| {
        let encodings = object_of(tx_type, payload, field)?;
        let (_, wire) = placed(payload, &encodings)?;
        Some((payload, encodings, wire))
    })?;
    // The field sits at a fixed place across the versions (a commitment's index aside, which
    // `with_version` handles; the coinbase commitment's from version 4): move the object by
    // re-encoding the version.
    if matches!(tx_type, 6 | 7 | 9 | 12) || (tx_type == 5 && version >= 4) {
        let payload = with_version(tx_type, donor, version);
        let (at, wire) = placed(&payload, &encodings)?;
        return Some(Witness {
            payload,
            at,
            wire,
            encodings,
        });
    }
    // The version 3 coinbase commitment has no seed with a signature: write one over the null
    // signature of the seed, which ends in the credit pool balance.
    if tx_type == 5 {
        let mut payload = seeds.iter().find(|(v, _)| *v == version)?.1.clone();
        let at = payload.len().checked_sub(wire.len() + 8)?;
        payload[at..at + wire.len()].copy_from_slice(&wire);
        assert_eq!(
            placed(&payload, &encodings).map(|p| p.0),
            Some(at),
            "type 5 v{version}: the signature is not where the layout puts it"
        );
        return Some(Witness {
            payload,
            at,
            wire,
            encodings,
        });
    }
    None
}

/// A lazy key's validity is not visible in the comparison with Core (Core writes the raw bytes
/// whatever they are, and the judge patches the codec's zeros back), so the rows record
/// base-sdk's view here, at build time: the scheme it takes from the version is Core's (legacy
/// for version 1 of types 1 and 3, basic otherwise), and zeros and bytes canonical in neither scheme are
/// invalid, and a valid point in the encoding of the scheme the version names is valid. The
/// other scheme's encoding of a point may or may not be valid under the version's scheme (that
/// depends on the point), and is not asserted. Core's own verdict on a key under each version's
/// scheme is EV1's `diff_check_special_tx`.
fn check_lazy_verdict(tx_type: u16, version: u16, classes: &[Class], payload: &[u8], name: &str) {
    let legacy = tx_type != 12 && version == 1;
    let (scheme, valid) = operator_key(tx_type, payload)
        .unwrap_or_else(|| panic!("{name}: the payload does not decode"));
    // The scheme is Core's rule, whatever the bytes: this is what a wrong version-to-scheme
    // choice would change.
    assert_eq!(scheme, BlsSchemeKind::from_legacy(legacy), "{name}: the key's scheme");
    let want = match classes {
        [Class::Zero] | [Class::Neither] => Some(false),
        [Class::Basic] if !legacy => Some(true),
        [Class::Legacy] if legacy => Some(true),
        [Class::Basic, Class::Legacy] => Some(true),
        _ => None,
    };
    if let Some(want) = want {
        assert_eq!(valid, want, "{name}: base-sdk's verdict on the key");
    }
}

/// The `DDE#10` rows: for every BLS field and version of [`BLS_FIELDS`], the field as zeros, as
/// bytes canonical in neither scheme, and as a valid object in each encoding.
fn bls_rows(rows: &mut Vec<Row>) {
    for spec in BLS_FIELDS {
        for &version in spec.versions {
            let w = witness(spec.tx_type, version, spec.field).unwrap_or_else(|| {
                panic!("no witness for type {} v{version} field {}", spec.tx_type, spec.field)
            });
            let mut neither = w.wire.clone();
            neither[0] ^= 0xe0;
            neither[1] ^= 0x5a;
            let [basic, legacy] = &w.encodings;
            let mut cells = vec![
                ("zero", vec![Class::Zero], vec![0; w.wire.len()]),
                ("neither scheme", vec![Class::Neither], neither),
            ];
            if basic == legacy {
                cells.push(("valid", vec![Class::Basic, Class::Legacy], basic.clone()));
            } else {
                cells.push(("valid basic", vec![Class::Basic], basic.clone()));
                cells.push(("valid legacy", vec![Class::Legacy], legacy.clone()));
            }
            for (label, classes, bytes) in cells {
                let mut edited = w.payload.clone();
                edited[w.at..w.at + bytes.len()].copy_from_slice(&bytes);
                let mut row = payload_row(
                    spec.tx_type,
                    format!("type {} v{version} field {}: {label}", spec.tx_type, spec.field),
                    "DDE#10",
                    Expect::Either,
                    &edited,
                );
                if spec.kind == FieldKind::Lazy {
                    check_lazy_verdict(spec.tx_type, version, &classes, &edited, &row.name);
                }
                row.covers = classes
                    .into_iter()
                    .map(|class| Cover {
                        tx_type: spec.tx_type,
                        version,
                        field: spec.field,
                        class,
                    })
                    .collect();
                rows.push(row);
            }
        }
    }
}

fn payload_rows(rows: &mut Vec<Row>) {
    for tx_type in 1..=PAYLOAD_TYPES {
        let seeds = payload_seeds(tx_type);
        let first = &seeds.first().expect("a seed of every type").1;

        // matrix: one payload per version, the seed itself where there is one.
        for &version in versions(tx_type) {
            let (payload, expect, how) = match seeds.iter().find(|(v, _)| *v == version) {
                Some((_, payload)) => (payload.clone(), Expect::Accept, "seed"),
                None => {
                    // A coinbase commitment of the later layout is cut from the latest seed.
                    let template = match tx_type {
                        5 if version >= 4 => seeds.last().map_or(first, |s| &s.1),
                        _ => first,
                    };
                    (with_version(tx_type, template, version), Expect::Either, "edited")
                }
            };
            rows.push(payload_row(
                tx_type,
                format!("type {tx_type} v{version} ({how})"),
                "matrix",
                expect,
                &payload,
            ));
        }

        // DDE#6: bytes after what the decoder reads (kept, and reported by `consumed_all`), and
        // the same payload short of its last byte (a version-0 payload is only its version, so
        // the shortened one still decodes).
        let mut one_per_version: Vec<&(u16, Vec<u8>)> = Vec::new();
        for seed in &seeds {
            if one_per_version.iter().all(|s| s.0 != seed.0) {
                one_per_version.push(seed);
            }
        }
        for (version, payload) in one_per_version.iter().map(|s| (&s.0, &s.1)) {
            for (label, tail) in
                [("one trailing byte", vec![0]), ("two trailing bytes", vec![0xde, 0xad])]
            {
                rows.push(payload_row(
                    tx_type,
                    format!("type {tx_type} v{version} + {label}"),
                    "DDE#6",
                    Expect::Accept,
                    &[payload.as_slice(), &tail].concat(),
                ));
            }
            rows.push(payload_row(
                tx_type,
                format!("type {tx_type} v{version} short of its last byte"),
                "DDE#6",
                if *version == 0 {
                    Expect::Either
                } else {
                    Expect::Reject
                },
                &payload[..payload.len() - 1],
            ));
        }
        // An empty payload.
        rows.push(payload_row(
            tx_type,
            format!("type {tx_type} empty"),
            "DDE#6",
            Expect::Reject,
            &[],
        ));
    }

    bls_rows(rows);

    // DDE#4: the LLMQ type is an open byte (14 named types; Core's decoder takes any).
    let commitment =
        payload_seeds(6).into_iter().find(|(v, _)| *v == 3).expect("a v3 commitment").1;
    for llmq in [0u8, 1, 2, 3, 4, 5, 6, 7, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 0xff] {
        let mut payload = commitment.clone();
        payload[QC_VERSION + 2] = llmq;
        rows.push(payload_row(
            6,
            format!("commitment LLMQ type {llmq}"),
            "DDE#4",
            Expect::Accept,
            &payload,
        ));
    }

    // sweep: the COMPACTSIZE field of a v3 coinbase commitment (bestCLHeightDiff, range-checked
    // against MAX_SIZE), non-canonical and 9-byte forms.
    let cbtx =
        payload_seeds(5).into_iter().find(|(v, _)| *v == 3).expect("a v3 coinbase commitment").1;
    let diff_at = 2 + 4 + 32 + 32;
    let cases: [(&str, Vec<u8>, Expect); 6] = [
        ("at MAX_SIZE", vec![0xfe, 0, 0, 0, 2], Expect::Accept),
        ("above MAX_SIZE", vec![0xfe, 1, 0, 0, 2], Expect::Reject),
        ("smallest 3-byte form (253)", vec![0xfd, 0xfd, 0], Expect::Accept),
        ("non-canonical (252 as 3 bytes)", vec![0xfd, 0xfc, 0], Expect::Reject),
        ("non-canonical (zero as 3 bytes)", vec![0xfd, 0, 0], Expect::Reject),
        ("2^32 in 9 bytes", vec![0xff, 0, 0, 0, 0, 1, 0, 0, 0], Expect::Reject),
    ];
    assert!(cbtx[diff_at] < 0xfd, "the seed's difference is a single byte");
    for (label, field, expect) in cases {
        let old = 1;
        let payload = [&cbtx[..diff_at], &field, &cbtx[diff_at + old..]].concat();
        rows.push(payload_row(
            5,
            format!("coinbase commitment diff {label}"),
            "sweep",
            expect,
            &payload,
        ));
    }

    // sweep: a BLS signature of one repeated byte, with the bytes before it the same, so that
    // it also reads as the same bytes shifted onto its neighbour (a fuzzer found this).
    let unlock = payload_seeds(9).into_iter().next().expect("an asset unlock").1;
    let repeated = [
        (9, "asset unlock", unlock.clone(), unlock.len() - BLS_SIG_LEN),
        (5, "coinbase commitment", cbtx.clone(), diff_at + 1),
    ];
    for (tx_type, label, mut payload, sig_at) in repeated {
        payload[sig_at - 5..sig_at + BLS_SIG_LEN].fill(0x0d);
        rows.push(payload_row(
            tx_type,
            format!("{label} signature of one repeated byte"),
            "sweep",
            Expect::Either,
            &payload,
        ));
    }
}

fn tx_with_values(values: &[i64]) -> Vec<u8> {
    TxSpec {
        outputs: values.iter().map(|&v| (v, vec![0x51])).collect(),
        ..TxSpec::plain()
    }
    .bytes()
}

fn object_rows(rows: &mut Vec<Row>) {
    let mut add =
        |target: &'static str, name: &str, tag: &'static str, expect: Expect, input: Vec<u8>| {
            rows.push(Row {
                target,
                name: name.into(),
                tag,
                expect,
                input,
                covers: Vec::new(),
            });
        };

    // DDE#1: Core reads any int64 as an output value and leaves the range to CheckTransaction.
    let values: [(&str, &[i64]); 8] = [
        ("i64::MIN", &[i64::MIN]),
        ("-1", &[-1]),
        ("0", &[0]),
        ("MAX_MONEY", &[MAX_MONEY]),
        ("MAX_MONEY + 1", &[MAX_MONEY + 1]),
        ("i64::MAX", &[i64::MAX]),
        ("two of MAX_MONEY (the total leaves the range)", &[MAX_MONEY, MAX_MONEY]),
        ("two of i64::MAX (the total overflows)", &[i64::MAX, i64::MAX]),
    ];
    for (label, values) in values {
        add(TX, &format!("output value {label}"), "DDE#1", Expect::Accept, tx_with_values(values));
        add(
            BLOCK,
            &format!("block with output value {label}"),
            "DDE#1",
            Expect::Accept,
            block_of(&[TxSpec::coinbase().bytes(), tx_with_values(values)]),
        );
    }

    // DDE#3: the extra payload is read up to MAX_SIZE; CheckTransaction holds it to 10,000.
    for len in [0usize, 1, 10_000, 10_001, 10_240, 10_241, 100_000] {
        add(
            TX,
            &format!("extra payload of {len} bytes"),
            "DDE#3",
            Expect::Accept,
            TxSpec::special(1, vec![0xab; len]).bytes(),
        );
    }
    let empty = TxSpec::special(1, vec![]).bytes();
    let before_length = &empty[..empty.len() - 1];
    for (label, prefix, expect) in [
        ("MAX_SIZE with no bytes behind it", vec![0xfe, 0, 0, 0, 2], Expect::Reject),
        ("MAX_SIZE + 1", vec![0xfe, 1, 0, 0, 2], Expect::Reject),
        ("2^32 - 1", vec![0xfe, 0xff, 0xff, 0xff, 0xff], Expect::Reject),
        ("non-canonical zero", vec![0xfd, 0, 0], Expect::Reject),
        ("MAX_SIZE in 9 bytes", [&[0xff][..], &MAX_SIZE.to_le_bytes()].concat(), Expect::Reject),
        ("2^32 in 9 bytes", [&[0xff][..], &(1u64 << 32).to_le_bytes()].concat(), Expect::Reject),
        ("u64::MAX in 9 bytes", vec![0xff; 9], Expect::Reject),
    ] {
        add(
            TX,
            &format!("extra payload length {label}"),
            "DDE#3",
            expect,
            [before_length, &prefix].concat(),
        );
    }

    // sweep: counts and scripts of a plain transaction.
    let plain = TxSpec::plain().bytes();
    let mut noncanonical_vin = plain.clone();
    noncanonical_vin.splice(4..5, [0xfd, 1, 0]);
    add(TX, "input count 1 as 3 bytes", "sweep", Expect::Reject, noncanonical_vin);
    let mut huge_vin = plain.clone();
    huge_vin.splice(4..5, [0xfe, 1, 0, 0, 2]);
    add(TX, "input count above MAX_SIZE", "sweep", Expect::Reject, huge_vin);
    add(TX, "truncated by one byte", "sweep", Expect::Reject, plain[..plain.len() - 1].to_vec());
    add(TX, "empty", "sweep", Expect::Reject, vec![]);
    add(TX, "one trailing byte", "sweep", Expect::Accept, [plain.clone(), vec![0]].concat());
    add(
        TX,
        "no inputs, no outputs",
        "sweep",
        Expect::Accept,
        TxSpec {
            inputs: vec![],
            outputs: vec![],
            ..TxSpec::plain()
        }
        .bytes(),
    );
    // A script length above MAX_SIZE in the first input (the byte after the 36-byte outpoint).
    let mut huge_script = plain.clone();
    huge_script.splice(4 + 1 + 36..4 + 1 + 36 + 1, [0xfe, 1, 0, 0, 2]);
    add(TX, "input script length above MAX_SIZE", "sweep", Expect::Reject, huge_script);

    // sweep: the version and type fields at their edges (the payload field exists exactly for
    // version >= 3 with a non-zero type).
    for (version, tx_type, payload) in [
        (2i16, 5u16, false),
        (3, 0, false),
        (3, 12, true),
        (3, 13, true),
        (3, 0xffff, true),
        (0x7fff, 1, true),
    ] {
        let spec = TxSpec {
            version,
            tx_type,
            payload: payload.then(|| vec![1, 2, 3]),
            ..TxSpec::plain()
        };
        add(
            TX,
            &format!("version {version} type {tx_type}"),
            "sweep",
            Expect::Accept,
            spec.bytes(),
        );
    }
    // A transaction whose version needs no payload but carries one: the bytes are trailing.
    add(
        TX,
        "payload bytes behind a version-2 transaction",
        "sweep",
        Expect::Accept,
        TxSpec {
            version: 2,
            tx_type: 5,
            payload: Some(vec![1, 2, 3]),
            ..TxSpec::plain()
        }
        .bytes(),
    );

    // Blocks.
    let coinbase = TxSpec::coinbase().bytes();
    add(BLOCK, "empty block", "sweep", Expect::Accept, block_of(&[]));
    add(BLOCK, "coinbase only", "sweep", Expect::Accept, block_of(std::slice::from_ref(&coinbase)));
    add(BLOCK, "header only", "sweep", Expect::Reject, header());
    add(
        BLOCK,
        "trailing bytes",
        "sweep",
        Expect::Accept,
        [block_of(std::slice::from_ref(&coinbase)), vec![0; 5]].concat(),
    );
    add(
        BLOCK,
        "transaction count above MAX_SIZE",
        "sweep",
        Expect::Reject,
        [header(), vec![0xfe, 1, 0, 0, 2]].concat(),
    );
    add(
        BLOCK,
        "transaction count 1 as 3 bytes",
        "sweep",
        Expect::Reject,
        [header(), vec![0xfd, 1, 0], coinbase.clone()].concat(),
    );
    add(
        BLOCK,
        "second transaction truncated",
        "sweep",
        Expect::Reject,
        block_of(&[coinbase.clone(), TxSpec::plain().bytes()[..20].to_vec()]),
    );
    add(
        BLOCK,
        "special transactions of types 1 to 13",
        "sweep",
        Expect::Accept,
        block_of(&(1..=13).map(|t| TxSpec::special(t, vec![0; 3]).bytes()).collect::<Vec<_>>()),
    );

    // Headers.
    let mut max_fields = header();
    max_fields[..4].copy_from_slice(&i32::MIN.to_le_bytes());
    max_fields[68..80].fill(0xff);
    add(HEADER, "zero header", "sweep", Expect::Accept, header());
    add(HEADER, "extreme fields", "sweep", Expect::Accept, max_fields);
    add(HEADER, "79 bytes", "sweep", Expect::Reject, header()[..79].to_vec());
    add(HEADER, "81 bytes", "sweep", Expect::Accept, [header(), vec![0]].concat());
    add(HEADER, "empty", "sweep", Expect::Reject, vec![]);
}

/// Every BS4b row, in a fixed order.
pub fn rows() -> Vec<Row> {
    let mut rows = Vec::new();
    object_rows(&mut rows);
    payload_rows(&mut rows);
    rows
}
