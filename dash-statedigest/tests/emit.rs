//! `emit` on values built in the test: logic inputs, not Core fixtures.

use dash_statedigest::emit::{
    self, DepRow, DerivedFields, GoldenFields, HeaderFields, RejectResult,
};
use dash_statedigest::{Hash256, Ident, Origin, Record, Value, parse_line};

fn hx(n: u8) -> Hash256 {
    Hash256::from_display_bytes([n; 32])
}

fn id(h: i64) -> Ident {
    Ident {
        h,
        b: hx(1),
        org: Origin::Tip,
    }
}

fn header(prev: Option<Hash256>) -> HeaderFields {
    HeaderFields {
        prev,
        ver: -0x7fff_ffff,
        mrkl: hx(2),
        time: u32::MAX,
        bits: 0x207f_ffff,
        nonce: u32::MAX,
        work: hx(3),
        ntx: 1,
    }
}

/// The record's canonical line, parsed back.
fn round_trip(record: Record) -> Record {
    let line = record.to_line();
    let parsed =
        parse_line(&line).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&line)));
    assert_eq!(parsed, record);
    parsed
}

#[test]
fn a_connect_holds_the_header_keys_and_seq_only() {
    let c = emit::connect(id(5), &header(Some(hx(4))), 7).unwrap();
    let Record::Outcome(c) = round_trip(Record::Outcome(c)) else {
        panic!()
    };
    let keys: Vec<&str> = c.s().iter().map(|(f, _)| f.key).collect();
    assert_eq!(keys, ["prev", "ver", "mrkl", "time", "bits", "nonce", "work", "ntx"]);
    assert_eq!(c.s().get("ver"), Some(&Value::Int(-0x7fff_ffff)));
    assert_eq!(c.s().get("time"), Some(&Value::Int(u32::MAX.into())));
    assert!(c.o().get("fx").is_none(), "effects are not observed, so absent");
    assert_eq!(c.x().get("seq"), Some(&Value::Int(7)));

    let genesis = emit::connect(id(0), &header(None), 1).unwrap();
    assert_eq!(genesis.s().get("prev"), Some(&Value::Null));
    round_trip(Record::Outcome(genesis));
}

#[test]
fn rejects_and_disconnects_carry_their_results() {
    let invalid = RejectResult::Invalid {
        cls: "consensus",
        why: "bad-cb-amount",
    };
    let r = emit::reject(id(5), invalid, Some("debug text"), 2).unwrap();
    assert_eq!(r.s().get("cls"), Some(&Value::string("consensus")));
    assert_eq!(r.x().get("dbg"), Some(&Value::string("debug text")));
    round_trip(Record::Outcome(r));
    for result in [
        RejectResult::Error {
            why: "exception",
        },
        RejectResult::Ok,
    ] {
        let r = emit::reject(id(5), result, None, 3).unwrap();
        assert_eq!(r.s().get("cls"), Some(&Value::Null));
        round_trip(Record::Outcome(r));
    }
    let bad = RejectResult::Invalid {
        cls: "no_such_class",
        why: "x",
    };
    assert!(emit::reject(id(5), bad, None, 4).is_err());

    round_trip(Record::Outcome(emit::disconnect(id(5), "unclean", 5).unwrap()));
    assert!(emit::disconnect(id(5), "maybe", 6).is_err());
}

const DEP: [DepRow<'static>; 3] = [
    DepRow {
        name: "testdummy",
        state: "started",
        since: 144,
    },
    DepRow {
        name: "v24",
        state: "defined",
        since: 0,
    },
    DepRow {
        name: "evo_shares",
        state: "failed",
        since: 0,
    },
];

fn golden(stats: Option<[i64; 7]>) -> GoldenFields {
    GoldenFields {
        muhash: Some(hx(9)),
        txouts: 12,
        amount: 50_000,
        unspendable: 7,
        bi: [1, 2, 3, 4, 5, 6, -7],
        stats,
    }
}

/// The derived and golden keys land in FIELDS order between the header keys and `seq`, whatever
/// order they are set in; the evo keys stay absent.
#[test]
fn a_connect_carries_the_derived_and_golden_keys_in_field_order() {
    let mut c = emit::connect(id(5), &header(Some(hx(4))), 7).unwrap();
    emit::with_golden(&mut c, &golden(Some([10, 11, 12, 13, 14, 15, 16]))).unwrap();
    let derived = DerivedFields {
        undo: Some(hx(8)),
        bls: true,
        bur: 0b101,
        dep: Some(&DEP),
    };
    emit::with_derived(&mut c, &derived).unwrap();
    let Record::Outcome(c) = round_trip(Record::Outcome(c)) else {
        panic!()
    };
    let keys: Vec<&str> = c.s().iter().map(|(f, _)| f.key).collect();
    assert_eq!(
        keys,
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
    for evo in ["mnl", "qc", "cp", "ehf"] {
        assert!(c.s().get(evo).is_none(), "{evo} is the evo hooks' key: absent");
    }
    assert_eq!(c.s().get("undo"), Some(&Value::hash(&hx(8))));
    assert_eq!(c.s().get("bls"), Some(&Value::Bool(true)));
    assert_eq!(c.s().get("bur"), Some(&Value::Int(5)));
    let row = |n: &str, s: &str, h: i64| {
        Value::Arr(vec![Value::string(n), Value::string(s), Value::Int(h)])
    };
    assert_eq!(
        c.s().get("dep"),
        Some(&Value::Arr(vec![
            row("testdummy", "started", 144),
            row("v24", "defined", 0),
            row("evo_shares", "failed", 0)
        ]))
    );
    assert_eq!(c.s().get("muhash"), Some(&Value::hash(&hx(9))));
    assert_eq!(c.s().get("txouts"), Some(&Value::Int(12)));
    assert_eq!(c.s().get("unspendable"), Some(&Value::Int(7)));
    let ints = |v: &[i64]| Value::Arr(v.iter().map(|&n| Value::Int(n)).collect());
    assert_eq!(c.s().get("bi"), Some(&ints(&[1, 2, 3, 4, 5, 6, -7])));
    assert_eq!(c.s().get("stats"), Some(&ints(&[10, 11, 12, 13, 14, 15, 16])));
    assert_eq!(c.x().get("seq"), Some(&Value::Int(7)));
}

/// CH3a D165: a writer that finalizes the MuHash at chosen blocks only leaves `muhash` absent at
/// the others, and the record parses back with every other golden key.
#[test]
fn a_golden_without_muhash_leaves_the_key_absent() {
    let mut c = emit::connect(id(5), &header(Some(hx(4))), 7).unwrap();
    let golden = GoldenFields {
        muhash: None,
        ..golden(Some([10, 11, 12, 13, 14, 15, 16]))
    };
    emit::with_golden(&mut c, &golden).unwrap();
    let Record::Outcome(c) = round_trip(Record::Outcome(c)) else {
        panic!()
    };
    assert!(c.s().get("muhash").is_none());
    assert_eq!(c.s().get("txouts"), Some(&Value::Int(12)));
    assert_eq!(c.s().get("amount"), Some(&Value::Int(50_000)));
}

/// Genesis's era nulls are written as `null`; a view without versionbits answers leaves `dep`
/// absent rather than claiming one.
#[test]
fn genesis_nulls_are_null_and_an_unanswered_dep_is_absent() {
    let mut c = emit::connect(id(0), &header(None), 1).unwrap();
    let derived = DerivedFields {
        undo: None,
        bls: true,
        bur: 0,
        dep: None,
    };
    emit::with_derived(&mut c, &derived).unwrap();
    emit::with_golden(&mut c, &golden(None)).unwrap();
    let Record::Outcome(c) = round_trip(Record::Outcome(c)) else {
        panic!()
    };
    assert_eq!(c.s().get("undo"), Some(&Value::Null));
    assert_eq!(c.s().get("stats"), Some(&Value::Null));
    assert!(c.s().get("dep").is_none());
}

/// The keys belong to a `C`: an `R` refuses them, and a value outside a key's shape is refused.
#[test]
fn derived_and_golden_keys_are_refused_outside_their_shapes() {
    let mut r = emit::reject(id(5), RejectResult::Ok, None, 2).unwrap();
    let derived = DerivedFields {
        undo: None,
        bls: false,
        bur: 0,
        dep: None,
    };
    assert!(emit::with_derived(&mut r, &derived).is_err());
    assert!(emit::with_golden(&mut r, &golden(None)).is_err());

    let mut c = emit::connect(id(5), &header(Some(hx(4))), 3).unwrap();
    let bad = [DepRow {
        name: "v24",
        state: "pending",
        since: 0,
    }];
    let derived = DerivedFields {
        dep: Some(&bad),
        ..derived
    };
    assert!(emit::with_derived(&mut c, &derived).is_err());
    let huge = GoldenFields {
        txouts: u64::MAX,
        ..golden(None)
    };
    assert!(emit::with_golden(&mut c, &huge).is_err());
}
