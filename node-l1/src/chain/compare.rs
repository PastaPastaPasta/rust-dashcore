//! The planes (O2b §4): two systems' steps for one op, compared plane by plane. The reply as a
//! whole (class and canonical JSON); the C/R/D records in O9a's stream mode with `org`, less
//! the `H` records and less `org=verifydb` (counted, D17); the A records in strict sequence;
//! the events in order within the op; then the post-state field by field.
//!
//! O2b-3a runs Core alone, so this is exercised Core against Core (`l1_chain`'s replays) and by
//! injected differences; O2b-3b compares Core with `NodeSut` through it.
//!
//! The events plane is strict (the design counts events only Core emits, which needs the node's
//! event set first). So is the records plane (CH3a a10 M-1, M-6): every key it compares is
//! required ([`required`]), so a key missing on either side is a difference, as is any record
//! O9a's comparison does not find clean; only an op with no records on either side is equal.
//!
//! `fx` is left out of that comparison until V writes it, and checked on its own
//! ([`unwritten_held`], OD-L3-74, [`UNWRITTEN`]). The evo keys `mnl`, `qc`, `cp` and `ehf` are
//! compared and required since V writes them (CH7a-5b's H11, OD-L3-77); the census counts the
//! paired `C`s that carried them and any value of Core's outside its era's `null` ([`Skipped`]).

use std::convert::Infallible;

use dash_statedigest::compare::{
    FieldId, Mode, Options, Verdict, compare_attempts, compare_stream,
};
use dash_statedigest::{
    Attempt, Class, FIELDS, FieldSource, Kind, Origin, Outcome, Record, Sparse, Value,
};

use super::step::Step;
use crate::Divergence;

/// What a comparison left out, for the census.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Skipped {
    /// `org=verifydb` outcome records, both sides (D17).
    pub verifydb: u64,
    /// The record pairs [`unwritten_held`] checked (OD-L3-74).
    pub unwritten_checked: u64,
    /// The paired `C`s the records plane checked.
    pub c_paired: u64,
    /// Of those, the `C`s whose evo keys ([`EVO_KEYS`]) both sides wrote (OD-L3-77).
    pub evo_compared: u64,
    /// Core's evo values other than `null` on paired `C`s: none in S0/S1's domain, which ends
    /// before DIP0003 (OD-L3-74 (3)).
    pub evo_non_null: u64,
}

/// The keys V's `DigestLog` does not write yet (CH3a D135, D144; `digest_log.rs` in
/// dashd-chainstate): `fx`, until the effect log is written. Until then the records plane leaves
/// it out of its comparison and checks it on its own ([`unwritten_held`], OD-L3-74).
pub const UNWRITTEN: [(Class, &str); 1] = [(Class::Observation, "fx")];

/// A `C`'s evo keys, which V writes from H11 when its connector's evo hooks are complete
/// (CH7a-5b): compared as every other key (OD-L3-77).
pub const EVO_KEYS: [&str; 4] = ["mnl", "qc", "cp", "ehf"];

/// The golden keys (O2b §4.2, D16): written by the golden extractor and a digest log from
/// genesis, never by Core's patch or by the node's records tap, so left out of the record plane.
pub fn golden_keys() -> Vec<FieldId> {
    outcome_keys().filter(|f| f.spec().source == FieldSource::Golden).collect()
}

fn outcome_keys() -> impl Iterator<Item = FieldId> {
    FIELDS
        .iter()
        .filter(|f| matches!(f.kind, Kind::Connect | Kind::Reject | Kind::Disconnect))
        .filter_map(|f| FieldId::find(f.kind, f.class, f.key))
}

/// The keys the records plane leaves out: the golden keys, [`UNWRITTEN`]'s and `extra`.
fn excluded(extra: &[FieldId]) -> Vec<FieldId> {
    let unwritten = outcome_keys().filter(|f| {
        let spec = f.spec();
        UNWRITTEN.contains(&(spec.class, spec.key))
    });
    let mut keys: Vec<FieldId> = golden_keys().into_iter().chain(unwritten).collect();
    keys.extend_from_slice(extra);
    keys.sort();
    keys.dedup();
    keys
}

/// The keys the records plane requires (CH3a a10 M-1, M-6): of the record kinds in `kinds`, every
/// key Core's patch writes ([`FieldSource::Patch`]) of a class stream mode compares, less
/// `excluded`. The schedule keys (`x.seq`, `x.dbg`) are never compared, so never required.
pub fn required(kinds: &[Kind], excluded: &[FieldId]) -> Vec<FieldId> {
    outcome_keys()
        .filter(|f| {
            let spec = f.spec();
            kinds.contains(&spec.kind)
                && spec.source == FieldSource::Patch
                && Mode::Stream.classes().contains(&spec.class)
                && !excluded.contains(f)
        })
        .collect()
}

/// [`UNWRITTEN`]'s keys on one pair, Core's record first (OD-L3-74): Core's `fx` is `[]`, and the
/// node's is absent or Core's. So the check becomes an equality once V writes it.
pub fn unwritten_held(core: &Outcome, node: &Outcome) -> bool {
    UNWRITTEN.iter().all(|&(class, key)| {
        if FieldId::find(core.kind(), class, key).is_none() {
            return true;
        }
        let (c, n) = (core.o().get(key), node.o().get(key));
        c == Some(&Value::Arr(Vec::new())) && (n.is_none() || n == c)
    })
}

/// One pair's `C` counts ([`Skipped::c_paired`], [`Skipped::evo_compared`],
/// [`Skipped::evo_non_null`]).
fn count_evo(core: &Outcome, node: &Outcome, skipped: &mut Skipped) {
    if core.kind() != Kind::Connect {
        return;
    }
    skipped.c_paired += 1;
    let written = |o: &Outcome| EVO_KEYS.iter().all(|k| o.s().get(k).is_some());
    if written(core) && written(node) {
        skipped.evo_compared += 1;
    }
    let non_null = EVO_KEYS.iter().filter(|k| core.s().get(k).is_some_and(|v| *v != Value::Null));
    skipped.evo_non_null += non_null.count() as u64;
}

/// The records a side contributes, by plane.
struct Planes {
    /// Its outcome records (C/R/D) of compared origins, numbered in stream order.
    outcomes: Vec<(usize, Record)>,
    attempts: Vec<Attempt>,
    sparse: Vec<Sparse>,
}

fn split(records: &[Record], skipped: &mut Skipped) -> Planes {
    let mut planes = Planes {
        outcomes: Vec::new(),
        attempts: Vec::new(),
        sparse: Vec::new(),
    };
    for (at, r) in records.iter().enumerate() {
        match r {
            Record::Header(_) => {}
            Record::Outcome(o) if o.id.org == Origin::Verifydb => skipped.verifydb += 1,
            Record::Outcome(_) => planes.outcomes.push((at, r.clone())),
            Record::Attempt(a) => planes.attempts.push(a.clone()),
            Record::Sparse(s) => planes.sparse.push(s.clone()),
        }
    }
    planes
}

/// The records plane (CH3a a10 M-1, M-6; OD-L3-74): equal when neither side has a record, or
/// when O9a's stream comparison of the two, every compared key required, is clean and every pair
/// holds [`unwritten_held`].
fn records_equal(
    core: &[(usize, Record)],
    node: &[(usize, Record)],
    exclude: &[FieldId],
    skipped: &mut Skipped,
) -> bool {
    if core.is_empty() && node.is_empty() {
        return true;
    }
    let outcome = |(_, r): &(usize, Record)| match r {
        Record::Outcome(o) => Some(o.clone()),
        _ => None,
    };
    if core.len() == node.len() {
        for (c, n) in core.iter().filter_map(outcome).zip(node.iter().filter_map(outcome)) {
            skipped.unwritten_checked += 1;
            count_evo(&c, &n, skipped);
            if !unwritten_held(&c, &n) {
                return false;
            }
        }
    }
    let mut kinds: Vec<Kind> =
        core.iter().chain(node).filter_map(outcome).map(|o| o.kind()).collect();
    kinds.sort_by_key(|k| k.letter());
    kinds.dedup();
    let mut opts = Options::new(Mode::Stream);
    opts.exclude = excluded(exclude);
    opts.require = required(&kinds, &opts.exclude);
    let report = compare_stream::<Infallible>(
        core.iter().cloned().map(Ok),
        node.iter().cloned().map(Ok),
        opts,
    );
    report.is_ok_and(|r| r.verdict == Verdict::Clean)
}

/// Compare one op's steps: the first plane of [`differences`], if any.
pub fn compare(left: &Step, right: &Step, exclude: &[FieldId]) -> Result<Skipped, Divergence> {
    let (skipped, differ) = differences(left, right, exclude);
    match differ.first() {
        None => Ok(skipped),
        Some(&field) => Err(Divergence {
            field,
        }),
    }
}

/// Every plane in which one op's steps differ, in the order they are read; `left` is Core's.
/// `exclude`: keys left out of the record plane beside the golden keys (D16) and
/// [`UNWRITTEN`]'s, which it always leaves out.
pub fn differences(left: &Step, right: &Step, exclude: &[FieldId]) -> (Skipped, Vec<&'static str>) {
    let mut skipped = Skipped::default();
    let mut differ = Vec::new();
    let mut plane = |equal: bool, field: &'static str| {
        if !equal {
            differ.push(field);
        }
    };
    plane(left.reply == right.reply, "reply");

    let lp = split(&left.records, &mut skipped);
    let rp = split(&right.records, &mut skipped);
    plane(records_equal(&lp.outcomes, &rp.outcomes, exclude, &mut skipped), "records");
    plane(compare_attempts(&lp.attempts, &rp.attempts).is_none(), "attempts");
    plane(lp.sparse == rp.sparse, "sparse records");

    plane(left.events == right.events, "events");

    let (l, r) = (&left.post, &right.post);
    plane(l.down == r.down, "post.down");
    plane(l.flush_delta == r.flush_delta, "post.flush_delta");
    plane(l.persisted == r.persisted, "post.persisted");
    plane(l.tip == r.tip, "post.tip");
    plane(l.best_header == r.best_header, "post.best_header");
    plane(l.best_invalid == r.best_invalid, "post.best_invalid");
    plane(l.candidates == r.candidates, "post.candidates");
    plane(l.utxo == r.utxo, "post.utxo");
    plane(l.index == r.index, "post.index");
    plane(l.flushes == r.flushes, "post.flushes");
    plane(l.ibd == r.ibd, "post.ibd");
    plane(l.best_cl == r.best_cl, "post.best_cl");
    plane(l.sporks == r.sporks, "post.sporks");
    plane(l.tips == r.tips, "post.tips");
    plane(l.deployments == r.deployments, "post.deployments");
    (skipped, differ)
}

#[cfg(test)]
mod tests {
    use dash_oracle_sys::{FlushCounts, Post, Reply};
    use dash_statedigest::emit::{self, DepRow, DerivedFields, HeaderFields, RejectResult};
    use dash_statedigest::{Hash256, Ident};

    use super::*;

    fn id(kind: u8) -> Ident {
        Ident {
            h: 3,
            b: Hash256::from_wire_bytes([kind; 32]),
            org: Origin::Tip,
        }
    }

    /// A `C` as the node's records tap writes it: every patch key but [`UNWRITTEN`]'s, the evo
    /// keys `null` as before DIP0003.
    fn node_c() -> Outcome {
        let header = HeaderFields {
            prev: Some(Hash256::from_wire_bytes([1; 32])),
            ver: 0x2000_0000,
            mrkl: Hash256::from_wire_bytes([2; 32]),
            time: 1_700_000_000,
            bits: 0x207f_ffff,
            nonce: 4,
            work: Hash256::from_wire_bytes([5; 32]),
            ntx: 1,
        };
        let mut c = emit::connect(id(b'C'), &header, 7).expect("a record of the spec's shape");
        let dep = [DepRow {
            name: "testdummy",
            state: "defined",
            since: 0,
        }];
        let derived = DerivedFields {
            undo: Some(Hash256::from_wire_bytes([6; 32])),
            bls: true,
            bur: 0x3f,
            dep: Some(&dep),
        };
        emit::with_derived(&mut c, &derived).expect("a record of the spec's shape");
        for key in EVO_KEYS {
            c.s_mut().set(key, Value::Null).expect("a value of the key's shape");
        }
        c
    }

    fn node_r() -> Outcome {
        let invalid = RejectResult::Invalid {
            cls: "consensus",
            why: "bad-cb-amount",
        };
        emit::reject(id(b'R'), invalid, Some("debug"), 8).expect("a record of the spec's shape")
    }

    fn node_d() -> Outcome {
        emit::disconnect(id(b'D'), "ok", 9).expect("a record of the spec's shape")
    }

    /// `o` as Core's patch writes it: with `fx` `[]`.
    fn core(mut o: Outcome) -> Outcome {
        o.o_mut().set("fx", Value::Arr(Vec::new())).expect("a value of the key's shape");
        o
    }

    fn step(records: Vec<Outcome>) -> Step {
        Step {
            reply: Reply {
                checked: Vec::new(),
                outcome: Ok(serde_json::Value::Null),
            },
            records: records.into_iter().map(Record::Outcome).collect(),
            events: Vec::new(),
            post: Post {
                down: true,
                flush_delta: FlushCounts::default(),
                tie: Vec::new(),
                persisted: None,
                tip: None,
                best_header: None,
                best_invalid: None,
                candidates: None,
                utxo: None,
                index: None,
                flushes: None,
                ibd: None,
                best_cl: None,
                sporks: None,
                tips: None,
                deployments: None,
            },
        }
    }

    /// Whether the records plane holds Core's `core` and the node's `node` the same.
    fn same(core: Vec<Outcome>, node: Vec<Outcome>) -> bool {
        !differences(&step(core), &step(node), &[]).1.contains(&"records")
    }

    fn all() -> [Outcome; 3] {
        [node_c(), node_r(), node_d()]
    }

    /// M-6's positive witnesses: a complete `C`, `R` and `D` of Core's equal to itself and to the
    /// node's, [`UNWRITTEN`]'s keys checked apart, each pair counted; the same with differing
    /// schedule keys (`x.seq`, `x.dbg`).
    #[test]
    fn complete_records_are_equal() {
        for o in all() {
            assert!(same(vec![core(o.clone())], vec![core(o.clone())]), "{o:?} Core's itself");
            assert!(same(vec![core(o.clone())], vec![o.clone()]), "{o:?} Core's and the node's");
            let mut later = o.clone();
            later.x_mut().set("seq", Value::Int(70)).expect("a value of the key's shape");
            if o.kind() == Kind::Reject {
                later
                    .x_mut()
                    .set("dbg", Value::string("other"))
                    .expect("a value of the key's shape");
            }
            assert!(same(vec![core(o.clone())], vec![later]), "{o:?}: schedule keys differ");
        }
        let (skipped, _) = differences(&step(all().map(core).into()), &step(all().into()), &[]);
        assert_eq!(skipped.unwritten_checked, 3);
        let evo = (skipped.c_paired, skipped.evo_compared, skipped.evo_non_null);
        assert_eq!(evo, (1, 1, 0), "the C's evo keys");
    }

    /// An op with no record on either side is equal (O9a reports zero pairs as `Incomplete`).
    #[test]
    fn no_records_are_equal() {
        assert!(same(Vec::new(), Vec::new()));
        assert!(!same(vec![core(node_d())], Vec::new()), "one side only");
        assert!(!same(Vec::new(), vec![node_d()]), "the node's only");
    }

    /// M-1: a required key missing on either side or both is a difference, `Incomplete` included.
    #[test]
    fn a_missing_required_key_is_a_difference() {
        let without = |mut o: Outcome, key: &str| {
            o.s_mut().remove(key).expect("the key is present");
            o
        };
        let c = node_c();
        let no_undo = without(c.clone(), "undo");
        assert!(!same(vec![core(c.clone())], vec![no_undo.clone()]), "the node's C");
        assert!(!same(vec![core(no_undo.clone())], vec![c]), "Core's C");
        assert!(!same(vec![core(no_undo.clone())], vec![no_undo]), "both");
        for (o, key) in [(node_r(), "res"), (node_r(), "cls"), (node_d(), "res")] {
            let missing = without(o.clone(), key);
            assert!(!same(vec![core(o.clone())], vec![missing.clone()]), "{key}: the node's");
            assert!(!same(vec![core(missing.clone())], vec![missing]), "{key}: both");
        }
    }

    /// OD-L3-74: Core's `fx` other than `[]`, or missing, and a node `fx` other than Core's, are
    /// each a difference; a node `fx` equal to Core's is not.
    #[test]
    fn the_unwritten_fx_is_held_apart() {
        let hash = Value::hash(&Hash256::from_wire_bytes([9; 32]));
        let fx = Value::Arr(vec![Value::Arr(vec![Value::string("is_rm"), hash.clone(), hash])]);
        for o in all() {
            let mut busy = core(o.clone());
            busy.o_mut().set("fx", fx.clone()).expect("a value of the key's shape");
            assert!(!same(vec![busy], vec![o.clone()]), "{:?}: Core's fx", o.kind());
            let mut stray = o.clone();
            stray.o_mut().set("fx", fx.clone()).expect("a value of the key's shape");
            assert!(!same(vec![core(o.clone())], vec![stray]), "{:?}: a stray fx", o.kind());
            let mut written = o.clone();
            written.o_mut().set("fx", Value::Arr(Vec::new())).expect("a value of the key's shape");
            assert!(same(vec![core(o.clone())], vec![written]), "{:?}: fx written", o.kind());
            assert!(!same(vec![o.clone()], vec![o.clone()]), "{:?}: Core without fx", o.kind());
        }
    }

    /// OD-L3-77: a `C`'s evo keys are compared as every other key: one missing on the node's
    /// side, on Core's or on both, or unequal, is a difference. Equal values outside the `null`
    /// era are equal, and counted.
    #[test]
    fn the_evo_keys_are_compared() {
        let hash = Value::hash(&Hash256::from_wire_bytes([9; 32]));
        let cp = Value::Arr(vec![Value::Int(1), Value::Int(2), Value::Int(3), hash.clone()]);
        for (key, value) in [
            ("mnl", hash),
            ("qc", Value::Arr(Vec::new())),
            ("cp", cp),
            ("ehf", Value::Arr(Vec::new())),
        ] {
            let mut missing = node_c();
            missing.s_mut().remove(key).expect("the key is present");
            assert!(!same(vec![core(node_c())], vec![missing.clone()]), "the node's {key} missing");
            assert!(!same(vec![core(missing.clone())], vec![node_c()]), "Core's {key} missing");
            assert!(!same(vec![core(missing.clone())], vec![missing]), "both sides' {key} missing");
            let mut era = node_c();
            era.s_mut().set(key, value).expect("a value of the key's shape");
            assert!(!same(vec![core(era.clone())], vec![node_c()]), "{key} unequal");
            let (skipped, differ) =
                differences(&step(vec![core(era.clone())]), &step(vec![era]), &[]);
            assert!(!differ.contains(&"records"), "{key}: equal values");
            assert_eq!(skipped.evo_non_null, 1, "{key}: counted");
        }
    }

    /// OD-L3-77: a `C`'s evo keys are required and its `fx` is not. O9a reports a compared key
    /// absent on either side as uncovered even unrequired, so the plane alone cannot tell the
    /// list apart; it is pinned here.
    #[test]
    fn the_evo_keys_are_required_and_fx_is_not() {
        let required = required(&[Kind::Connect], &excluded(&[]));
        for key in EVO_KEYS {
            let id = FieldId::find(Kind::Connect, Class::Semantic, key).expect("a C key");
            assert!(required.contains(&id), "{key} required");
        }
        let fx = FieldId::find(Kind::Connect, Class::Observation, "fx").expect("a C key");
        assert!(!required.contains(&fx), "fx left out");
    }
}
