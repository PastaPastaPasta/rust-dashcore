//! Generated records: encode -> parse -> encode identity, typed conversions, and byte-level
//! mutations of valid lines. A deterministic generator keeps the crate free of dev-dependencies
//! and the failures reproducible (every case derives from a fixed seed).

use dash_statedigest::{
    Attempt, BestChainLock, Body, Chain, Class, DEPLOYMENT_STATES, Effect, FileKind, Hash256,
    Header, Ident, IsConflictPair, Kind, Lines, Origin, Outcome, ParseErrorKind, Query, Record,
    SbPayment, Shape, Sparse, Value, Writer, group, parse_line,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len() as u64) as usize]
    }

    fn int(&mut self) -> i64 {
        match self.below(8) {
            0 => 0,
            1 => i64::MAX,
            2 => i64::MIN,
            3 => -1,
            4 => self.below(1000) as i64,
            5 => -(self.below(1_000_000) as i64),
            _ => self.next() as i64,
        }
    }

    fn hash(&mut self) -> Hash256 {
        let mut b = [0u8; 32];
        for chunk in b.chunks_mut(8) {
            chunk.copy_from_slice(&self.next().to_le_bytes()[..chunk.len()]);
        }
        Hash256::from_display_bytes(b)
    }

    /// Any bytes, with printable ASCII, quotes and backslashes over-represented.
    fn bytes(&mut self, max: u64) -> Vec<u8> {
        let n = self.below(max + 1);
        (0..n)
            .map(|_| match self.below(6) {
                0 => self.pick(&[b'"', b'\\', b'/', 0x00, 0x1f, 0x7f, 0x80, 0xff, b'\n', b'\r']),
                1 => self.next() as u8,
                _ => 0x20 + self.below(0x5f) as u8,
            })
            .collect()
    }

    fn hex_bytes(&mut self, max: u64) -> Vec<u8> {
        let n = self.below(max + 1);
        (0..n).map(|_| self.next() as u8).collect()
    }
}

fn effect(r: &mut Rng) -> Effect {
    match r.below(6) {
        0 => Effect::Snap {
            quorum_type: r.int(),
            cycle_base: r.hash(),
            snapshot: r.hash(),
        },
        1 => Effect::IsRm {
            islock: r.hash(),
            txid: r.hash(),
        },
        2 => Effect::QcUnmine {
            quorum_type: r.int(),
            quorum_hash: r.hash(),
        },
        3 => Effect::QcRemine {
            quorum_type: r.int(),
            quorum_hash: r.hash(),
        },
        4 => Effect::BanReset {
            pro_tx_hash: r.hash(),
            height: r.int(),
            ok: r.chance(1, 2),
        },
        _ => Effect::MnChanged {
            undo: r.chance(1, 2),
        },
    }
}

fn query(r: &mut Rng) -> Query {
    match r.below(9) {
        0 => Query::ClConflict {
            height: r.int(),
            hash: r.hash(),
            answer: r.chance(1, 2),
        },
        1 if r.chance(1, 2) => Query::ClBest(None),
        1 => Query::ClBest(Some(BestChainLock {
            height: r.int(),
            hash: r.hash(),
            sig: {
                let mut sig = [0u8; 96];
                sig.iter_mut().for_each(|b| *b = r.next() as u8);
                sig
            },
        })),
        2 => Query::Ibd(r.chance(1, 2)),
        3 => Query::ClHas {
            height: r.int(),
            hash: r.hash(),
            answer: r.chance(1, 2),
        },
        4 => Query::IsConflict {
            txid: r.hash(),
            conflict: r.chance(1, 2).then(|| IsConflictPair {
                islock: r.hash(),
                conflicting_txid: r.hash(),
            }),
        },
        5 => Query::SbRequired {
            height: r.int(),
            answer: r.chance(1, 2),
        },
        6 => Query::GovValid(r.chance(1, 2)),
        7 => Query::SbTriggered {
            height: r.int(),
            answer: r.chance(1, 2),
        },
        _ => Query::SbPayments {
            height: r.int(),
            payments: r.chance(2, 3).then(|| {
                (0..r.below(4))
                    .map(|_| SbPayment {
                        amount: r.int(),
                        script: r.hex_bytes(30),
                    })
                    .collect()
            }),
        },
    }
}

/// A value of the shape (the null possibility included).
fn field_value(r: &mut Rng, shape: &Shape) -> Value {
    if shape.nullable && r.chance(1, 5) {
        return Value::Null;
    }
    match shape.body {
        Body::Int => Value::Int(r.int()),
        Body::U32 => Value::Int(i64::from(r.next() as u32)),
        Body::Bool => Value::Bool(r.chance(1, 2)),
        Body::Str => Value::Str(r.bytes(40)),
        Body::Hash => Value::hash(&r.hash()),
        Body::Qc => {
            let mut keys: Vec<(i64, Hash256)> =
                (0..r.below(5)).map(|_| (r.below(4) as i64, r.hash())).collect();
            keys.sort();
            keys.dedup();
            Value::Arr(
                keys.into_iter()
                    .map(|(t, q)| {
                        Value::Arr(vec![Value::Int(t), Value::hash(&q), Value::hash(&r.hash())])
                    })
                    .collect(),
            )
        }
        Body::Cp => Value::Arr(vec![
            Value::Int(r.int()),
            Value::Int(r.int()),
            Value::Int(r.int()),
            Value::hash(&r.hash()),
        ]),
        Body::Ehf => {
            let mut bits: Vec<i64> = (0..r.below(5)).map(|_| r.below(32) as i64).collect();
            bits.sort_unstable();
            bits.dedup();
            Value::Arr(
                bits.into_iter()
                    .map(|b| Value::Arr(vec![Value::Int(b), Value::Int(r.int())]))
                    .collect(),
            )
        }
        Body::Dep => Value::Arr(
            (0..r.below(4))
                .map(|_| {
                    Value::Arr(vec![
                        Value::Str(r.bytes(12)),
                        Value::string(r.pick(DEPLOYMENT_STATES)),
                        Value::Int(r.int()),
                    ])
                })
                .collect(),
        ),
        Body::Ints(n) => Value::Arr((0..n).map(|_| Value::Int(r.int())).collect()),
        Body::Effects => {
            let list: Vec<Effect> = (0..r.below(5)).map(|_| effect(r)).collect();
            Effect::list_to_value(&list)
        }
        Body::Enum(names) => Value::string(r.pick(names)),
    }
}

fn number_text(r: &mut Rng) -> String {
    let mut s = String::new();
    if r.chance(1, 4) {
        s.push('-');
    }
    if r.chance(1, 4) {
        s.push('0');
    } else {
        s.push(char::from(b'1' + r.below(9) as u8));
        for _ in 0..r.below(20) {
            s.push(char::from(b'0' + r.below(10) as u8));
        }
    }
    if r.chance(1, 2) {
        s.push('.');
        for _ in 0..=r.below(8) {
            s.push(char::from(b'0' + r.below(10) as u8));
        }
    }
    if r.chance(1, 5) {
        s.push(r.pick(&['e', 'E']));
        if r.chance(1, 2) {
            s.push(r.pick(&['+', '-']));
        }
        for _ in 0..=r.below(3) {
            s.push(char::from(b'0' + r.below(10) as u8));
        }
    }
    s
}

/// A payload (`payload`: numbers are `Num`) or an argument (numbers are `Int`).
fn free_value(r: &mut Rng, payload: bool, depth: u32) -> Value {
    let top = if depth >= 4 {
        5
    } else {
        7
    };
    match r.below(top) {
        0 => Value::Null,
        1 => Value::Bool(r.chance(1, 2)),
        2 | 3 if payload => Value::Num(number_text(r)),
        2 | 3 => Value::Int(r.int()),
        4 | 5 => Value::Str(r.bytes(20)),
        _ if r.chance(1, 2) => {
            Value::Arr((0..r.below(4)).map(|_| free_value(r, payload, depth + 1)).collect())
        }
        _ => {
            let mut keys: Vec<Vec<u8>> = (0..r.below(4)).map(|_| r.bytes(6)).collect();
            keys.sort();
            keys.dedup();
            Value::Obj(keys.into_iter().map(|k| (k, free_value(r, payload, depth + 1))).collect())
        }
    }
}

fn ident(r: &mut Rng) -> Ident {
    Ident {
        h: r.int(),
        b: r.hash(),
        org: r.pick(Origin::ALL),
    }
}

fn record(r: &mut Rng) -> Record {
    match r.below(10) {
        0 => Record::Header(Header {
            file: r.pick(FileKind::ALL),
            src: r.pick(Writer::ALL),
            ver: r.bytes(30),
            chain: r.pick(Chain::ALL),
            run: r.bytes(30),
        }),
        1 | 2 => {
            let mut a = Attempt::new(ident(r), r.int());
            a.q = (0..r.below(7)).map(|_| query(r)).collect();
            if r.chance(3, 4) {
                a.x_mut().set("seq", Value::Int(r.int())).unwrap();
            }
            Record::Attempt(a)
        }
        3 => {
            let args = (0..r.below(4)).map(|_| free_value(r, false, 1)).collect();
            let s = Sparse::new(r.int(), r.hash(), r.bytes(20), args, free_value(r, true, 0))
                .expect("generated payloads are valid");
            Record::Sparse(s)
        }
        _ => {
            let kind = r.pick(&[Kind::Connect, Kind::Reject, Kind::Disconnect]);
            let id = ident(r);
            let mut o = match kind {
                Kind::Connect => Outcome::connect(id),
                Kind::Reject => Outcome::reject(id),
                _ => Outcome::disconnect(id),
            };
            for class in [Class::Semantic, Class::Observation, Class::Schedule] {
                for spec in group(kind, class) {
                    if r.chance(2, 3) {
                        let v = field_value(r, &spec.shape);
                        let mut fields = match class {
                            Class::Semantic => o.s_mut(),
                            Class::Observation => o.o_mut(),
                            Class::Schedule => o.x_mut(),
                        };
                        fields.set(spec.key, v).expect("generated values have their shape");
                    }
                }
            }
            Record::Outcome(o)
        }
    }
}

fn corpus(seed: u64, n: usize) -> Vec<Record> {
    let mut r = Rng(seed);
    (0..n).map(|_| record(&mut r)).collect()
}

#[test]
fn generated_records_round_trip() {
    let records = corpus(0x5eed_0001, 4000);
    let mut kinds = [0usize; 6];
    for rec in &records {
        let line = rec.to_line();
        let parsed =
            parse_line(&line).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&line)));
        assert_eq!(&parsed, rec, "{}", String::from_utf8_lossy(&line));
        assert_eq!(parsed.to_line(), line);
        kinds[rec.kind() as usize] += 1;
    }
    assert!(kinds.iter().all(|&n| n > 100), "every kind is generated: {kinds:?}");

    // The same records through the iterator over one buffer.
    let buf: Vec<u8> = records.iter().flat_map(Record::to_line).collect();
    let mut seen = 0;
    for ((no, parsed), rec) in Lines::new(&buf).zip(&records) {
        seen += 1;
        assert_eq!(no, seen);
        assert_eq!(&parsed.unwrap(), rec);
    }
    assert_eq!(seen, records.len());
}

#[test]
fn absent_keys_survive_a_round_trip() {
    // Every subset of a small group, including the empty one.
    let specs = group(Kind::Reject, Class::Semantic);
    for mask in 0..(1u32 << specs.len()) {
        let mut r = Rng(u64::from(mask) + 1);
        let id = Ident {
            h: 5,
            b: r.hash(),
            org: Origin::Tip,
        };
        let mut o = Outcome::reject(id);
        for (i, spec) in specs.iter().enumerate() {
            if mask >> i & 1 == 1 {
                o.s_mut().set(spec.key, field_value(&mut r, &spec.shape)).unwrap();
            }
        }
        let rec = Record::Outcome(o);
        let parsed = parse_line(&rec.to_line()).unwrap();
        assert_eq!(parsed, rec);
        let Record::Outcome(p) = parsed else {
            unreachable!()
        };
        for (i, spec) in specs.iter().enumerate() {
            assert_eq!(p.s().get(spec.key).is_some(), mask >> i & 1 == 1, "{}", spec.key);
        }
    }
}

#[test]
fn typed_effects_and_queries_convert_losslessly() {
    let mut r = Rng(0x5eed_0002);
    for _ in 0..3000 {
        let e = effect(&mut r);
        assert_eq!(Effect::try_from(&e.to_value()), Ok(e));
        let q = query(&mut r);
        assert_eq!(Query::try_from(&q.to_value()), Ok(q));
    }
}

#[test]
fn hostile_byte_strings_round_trip() {
    // Every byte value, alone and in a run, as a header string and as a payload key and value.
    let all: Vec<u8> = (0..=255u8).collect();
    let h = Header {
        file: FileKind::Golden,
        src: Writer::Extract,
        ver: all.clone(),
        chain: Chain::Main,
        run: b"\"\\\"\\\\u0041".to_vec(),
    };
    let line = Record::Header(h.clone()).to_line();
    assert!(line.iter().all(|&b| b == b'\n' || (0x20..=0x7e).contains(&b)), "ASCII output only");
    assert_eq!(parse_line(&line), Ok(Record::Header(h)));
    for b in 0..=255u8 {
        let res = Value::obj([(vec![b], Value::Str(vec![b, b]))]);
        let s = Sparse::new(0, Hash256::from_display_bytes([b; 32]), vec![b], vec![], res).unwrap();
        let line = Record::Sparse(s.clone()).to_line();
        assert_eq!(parse_line(&line), Ok(Record::Sparse(s)), "byte {b:#04x}");
    }
}

/// Applies one random edit.
fn mutate(r: &mut Rng, line: &[u8]) -> Vec<u8> {
    const INTERESTING: &[u8] = b"{}[]\",:\\ \t\r\n.-+eE0123456789abcdefuxkn";
    let mut out = line.to_vec();
    let at = r.below(out.len() as u64) as usize;
    match r.below(7) {
        0 => out[at] = r.next() as u8,
        1 => out[at] = r.pick(INTERESTING),
        2 => {
            out.remove(at);
        }
        3 => out.insert(at, r.pick(INTERESTING)),
        4 => out.insert(at, r.next() as u8),
        5 => out.truncate(at),
        _ => {
            let to = (at + 1 + r.below(8) as usize).min(out.len());
            let piece = out[at..to].to_vec();
            out.splice(at..at, piece);
        }
    }
    out
}

#[test]
fn strictness_holds_by_construction_under_mutation() {
    // `parse_line` re-encodes what it parsed and refuses a mismatch with `NotCanonical`. That
    // net must never be what catches a bad line: every mutation is decided by the grammar rules.
    let mut r = Rng(0x5eed_0003);
    let golden = include_bytes!("data/golden.jsonl");
    let mut lines: Vec<Vec<u8>> =
        golden.split_inclusive(|&b| b == b'\n').map(<[u8]>::to_vec).collect();
    lines.extend(corpus(0x5eed_0004, 300).iter().map(Record::to_line));
    let (mut accepted, mut rejected) = (0, 0);
    for line in &lines {
        for _ in 0..200 {
            let bad = mutate(&mut r, line);
            match parse_line(&bad) {
                Ok(rec) => {
                    accepted += 1;
                    assert_eq!(rec.to_line(), bad);
                }
                Err(e) => {
                    rejected += 1;
                    assert_ne!(
                        e.kind,
                        ParseErrorKind::NotCanonical,
                        "{}",
                        String::from_utf8_lossy(&bad)
                    );
                    assert!(e.offset <= bad.len(), "offset {} beyond {}", e.offset, bad.len());
                }
            }
        }
    }
    assert!(
        rejected > accepted * 5,
        "mutations are mostly invalid ({accepted} accepted, {rejected} rejected)"
    );
}

#[test]
fn every_prefix_of_a_line_is_refused_cleanly() {
    let golden = include_bytes!("data/golden.jsonl");
    for line in golden.split_inclusive(|&b| b == b'\n') {
        for len in 0..line.len() {
            let e = parse_line(&line[..len]).expect_err("a proper prefix is not a line");
            assert_ne!(e.kind, ParseErrorKind::NotCanonical);
            assert!(e.offset <= len);
        }
        assert!(parse_line(line).is_ok());
    }
}
