//! The field table against spec §5: keys, order, classes and sources.

use dash_statedigest::{Class, FIELDS, FieldSource, Kind, group, lookup};

fn keys(kind: Kind, class: Class) -> Vec<&'static str> {
    group(kind, class).iter().map(|f| f.key).collect()
}

#[test]
fn keys_are_in_spec_order() {
    assert_eq!(
        keys(Kind::Connect, Class::Semantic),
        [
            "prev",
            "ver",
            "mrkl",
            "time",
            "bits",
            "nonce",
            "work",
            "ntx",
            "undo",
            "mnl",
            "qc",
            "cp",
            "ehf",
            "bls",
            "bur",
            "dep",
            "muhash",
            "txouts",
            "amount",
            "unspendable",
            "bi",
            "stats"
        ]
    );
    assert_eq!(keys(Kind::Connect, Class::Observation), ["fx"]);
    assert_eq!(keys(Kind::Connect, Class::Schedule), ["seq"]);
    assert_eq!(keys(Kind::Reject, Class::Semantic), ["res", "cls", "why"]);
    assert_eq!(keys(Kind::Reject, Class::Observation), ["fx"]);
    assert_eq!(keys(Kind::Reject, Class::Schedule), ["seq", "dbg"]);
    assert_eq!(keys(Kind::Disconnect, Class::Semantic), ["res"]);
    assert_eq!(keys(Kind::Disconnect, Class::Observation), ["fx"]);
    assert_eq!(keys(Kind::Disconnect, Class::Schedule), ["seq"]);
    assert_eq!(keys(Kind::Attempt, Class::Schedule), ["seq"]);
    assert!(keys(Kind::Attempt, Class::Semantic).is_empty());
    assert!(keys(Kind::Header, Class::Semantic).is_empty());
    assert_eq!(FIELDS.len(), 34);
}

#[test]
fn groups_are_contiguous_and_cover_the_table() {
    let mut covered = 0;
    for kind in [Kind::Connect, Kind::Reject, Kind::Disconnect, Kind::Attempt] {
        for class in [Class::Semantic, Class::Observation, Class::Schedule] {
            let g = group(kind, class);
            covered += g.len();
            assert!(g.iter().all(|f| f.kind == kind && f.class == class));
            let first = FIELDS.iter().position(|f| f.kind == kind && f.class == class);
            if let Some(first) = first {
                assert_eq!(&FIELDS[first..first + g.len()], g, "{kind:?}.{class:?} is contiguous");
            }
        }
    }
    assert_eq!(covered, FIELDS.len());
    let mut seen = std::collections::BTreeSet::new();
    for f in FIELDS {
        assert!(seen.insert((f.kind, f.class, f.key)), "duplicate {f:?}");
        assert!(f.key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
    }
}

#[test]
fn golden_fields_are_exactly_the_statistics_of_5_3() {
    let golden: Vec<_> = group(Kind::Connect, Class::Semantic)
        .iter()
        .filter(|f| f.source == FieldSource::Golden)
        .map(|f| f.key)
        .collect();
    assert_eq!(golden, ["muhash", "txouts", "amount", "unspendable", "bi", "stats"]);
    assert!(
        FIELDS
            .iter()
            .filter(|f| f.class != Class::Semantic || f.kind != Kind::Connect)
            .all(|f| f.source == FieldSource::Patch)
    );
}

#[test]
fn nullable_keys_are_the_era_conditioned_ones() {
    let nullable: Vec<_> =
        FIELDS.iter().filter(|f| f.shape.nullable).map(|f| (f.kind, f.key)).collect();
    assert_eq!(
        nullable,
        [
            (Kind::Connect, "prev"),
            (Kind::Connect, "undo"),
            (Kind::Connect, "mnl"),
            (Kind::Connect, "qc"),
            (Kind::Connect, "cp"),
            (Kind::Connect, "ehf"),
            (Kind::Connect, "stats"),
            (Kind::Reject, "cls"),
            (Kind::Reject, "why"),
            (Kind::Reject, "dbg"),
        ]
    );
}

#[test]
fn lookup_finds_by_kind_class_and_key() {
    assert!(lookup(Kind::Connect, Class::Semantic, "qc").is_some());
    assert!(lookup(Kind::Connect, Class::Observation, "qc").is_none());
    assert!(lookup(Kind::Reject, Class::Schedule, "dbg").is_some());
    assert!(lookup(Kind::Disconnect, Class::Schedule, "dbg").is_none());
    assert_eq!(Class::Semantic.key(), "s");
    assert_eq!(Class::Observation.key(), "o");
    assert_eq!(Class::Schedule.key(), "x");
}
