//! One golden line per record kind, and the lines' typed content.
//!
//! The lines in `data/golden.jsonl` are written by hand from the grammar of spec §3-§4: they are
//! grammar examples, not Core fixtures (no value in them was produced by Dash Core). They pin the
//! canonical form byte for byte: each line must parse and re-encode to itself.

use dash_statedigest::{
    Attempt, Chain, Class, Effect, FileKind, Hash256, Header, Ident, Kind, Lines, Origin, Outcome,
    Query, Record, Value, Writer, parse_line,
};

const GOLDEN: &[u8] = include_bytes!("data/golden.jsonl");

fn lines() -> Vec<Vec<u8>> {
    GOLDEN.split_inclusive(|&b| b == b'\n').map(<[u8]>::to_vec).collect()
}

fn hx(n: u64) -> Hash256 {
    let mut bytes = [0u8; 32];
    bytes[24..].copy_from_slice(&n.to_be_bytes());
    Hash256::from_display_bytes(bytes)
}

fn record(index: usize) -> Record {
    parse_line(&lines()[index]).unwrap_or_else(|e| panic!("golden line {index}: {e}"))
}

#[test]
fn every_golden_line_round_trips_byte_for_byte() {
    let mut kinds = Vec::new();
    for (no, result) in Lines::new(GOLDEN) {
        let record = result.unwrap_or_else(|e| panic!("line {no}: {e}"));
        assert_eq!(record.to_line(), lines()[no - 1], "line {no}");
        kinds.push(record.kind());
    }
    for kind in
        [Kind::Header, Kind::Connect, Kind::Reject, Kind::Disconnect, Kind::Attempt, Kind::Sparse]
    {
        assert!(kinds.contains(&kind), "no golden line of kind {kind:?}");
    }
    assert_eq!(kinds.len(), 17);
}

#[test]
fn header_lines() {
    let Record::Header(h) = record(0) else {
        panic!("not an H")
    };
    assert_eq!(h.file, FileKind::Digest);
    assert_eq!(h.src, Writer::Dashd);
    assert_eq!(h.chain, Chain::Regtest);
    assert_eq!(h.ver, b"v24.0.0rc3-0364ed626e");
    assert_eq!(h.run, b"4711-1760227200123456");
    let Record::Header(g) = record(2) else {
        panic!("not an H")
    };
    assert_eq!((g.file, g.src), (FileKind::Golden, Writer::Extract));
}

/// The oracle shim's in-memory sink (node-program O2b): a chain session's records open with an
/// `H` of file `memory` (Core's `src/statedigest.cpp` `InitMemory`).
#[test]
fn a_memory_header_round_trips() {
    let line = [
        &br#"{"k":"H","v":1,"file":"memory","src":"dashd","ver":"v24.0.0-unk","chain":"regtest","run":"oracle"}"#[..],
        b"\n",
    ]
    .concat();
    let record = parse_line(&line).unwrap_or_else(|e| panic!("{e}"));
    let Record::Header(h) = &record else {
        panic!("not an H")
    };
    assert_eq!((h.file, h.src, h.chain), (FileKind::Memory, Writer::Dashd, Chain::Regtest));
    assert_eq!(record.to_line(), line);
}

#[test]
fn connect_line_fields_and_effects() {
    let Record::Outcome(c) = record(3) else {
        panic!("not an outcome")
    };
    assert_eq!(c.kind(), Kind::Connect);
    assert_eq!(
        c.id,
        Ident {
            h: 101,
            b: hx(0xb101),
            org: Origin::Tip
        }
    );
    assert_eq!(c.s().get("ntx"), Some(&Value::Int(2)));
    assert_eq!(c.s().get("bls"), Some(&Value::Bool(false)));
    assert_eq!(c.s().get("muhash"), None, "a patch record has no golden keys");
    assert_eq!(c.x().get("seq"), Some(&Value::Int(57)));
    let fx = Effect::list_from_value(c.o().get("fx").expect("fx")).expect("effects");
    assert_eq!(fx.len(), 5);
    assert_eq!(
        fx[0],
        Effect::Snap {
            quorum_type: 104,
            cycle_base: hx(0xb101),
            snapshot: hx(0x5e01)
        }
    );
    assert_eq!(
        fx[4],
        Effect::MnChanged {
            undo: false
        }
    );
    for e in &fx {
        assert_eq!(Effect::try_from(&e.to_value()).as_ref(), Ok(e));
    }
}

#[test]
fn genesis_nulls_are_present_and_null() {
    let Record::Outcome(c) = record(4) else {
        panic!("not an outcome")
    };
    for key in ["prev", "undo", "mnl", "qc", "cp", "ehf"] {
        assert_eq!(c.s().get(key), Some(&Value::Null), "{key}");
    }
    assert_eq!(c.s().get("stats"), None);
}

#[test]
fn absent_is_not_null() {
    let merged = match record(5) {
        Record::Outcome(c) => c,
        _ => panic!("not an outcome"),
    };
    assert_eq!(merged.s().get("mnl"), Some(&Value::Null));
    assert_eq!(merged.s().get("qc"), Some(&Value::Arr(vec![])));
    assert_eq!(merged.s().get("txouts"), Some(&Value::Int(101)));
    assert_eq!(merged.s().len(), 22);

    let Record::Outcome(partial) = record(6) else {
        panic!("not an outcome")
    };
    assert_eq!(partial.id.org, Origin::Replay);
    assert_eq!(partial.s().len(), 3);
    assert_eq!(partial.s().get("mnl"), None);
    assert!(partial.o().is_empty() && partial.x().is_empty());
    assert_eq!(Record::Outcome(partial).to_line(), lines()[6]);
}

#[test]
fn reject_and_disconnect_lines() {
    let Record::Outcome(r) = record(7) else {
        panic!("not an outcome")
    };
    assert_eq!(r.kind(), Kind::Reject);
    assert_eq!(r.s().get("why"), Some(&Value::string("bad-cb-amount")));
    assert_eq!(
        r.x().get("dbg").and_then(Value::as_str),
        Some("coinbase pays too much (actual=1 vs limit=0)")
    );

    let Record::Outcome(ok) = record(8) else {
        panic!("not an outcome")
    };
    assert_eq!(ok.id.org, Origin::Proposal);
    assert_eq!(ok.s().get("cls"), Some(&Value::Null));
    assert_eq!(ok.x().get("dbg"), Some(&Value::Null));

    // Escapes in a string: byte 0xe9, a backslash and a quote.
    let Record::Outcome(err) = record(9) else {
        panic!("not an outcome")
    };
    assert_eq!(err.x().get("dbg").and_then(Value::as_bytes), Some(&b"std::bad_alloc \xe9\\\""[..]));

    let Record::Outcome(d) = record(10) else {
        panic!("not an outcome")
    };
    assert_eq!(d.kind(), Kind::Disconnect);
    assert_eq!(d.id.org, Origin::Verifydb);
    assert_eq!(d.s().get("res"), Some(&Value::string("ok")));
}

#[test]
fn attempt_line_has_every_query_form() {
    let Record::Attempt(a) = record(12) else {
        panic!("not an A")
    };
    assert_eq!(a.t, 1_760_227_300);
    assert_eq!(a.q.len(), 13);
    assert_eq!(a.q[1], Query::ClBest(None));
    assert!(matches!(&a.q[2], Query::ClBest(Some(b)) if b.height == 100 && b.sig.len() == 96));
    assert!(matches!(
        &a.q[5],
        Query::IsConflict {
            conflict: None,
            ..
        }
    ));
    assert!(matches!(
        &a.q[6],
        Query::IsConflict {
            conflict: Some(_),
            ..
        }
    ));
    assert!(matches!(
        &a.q[10],
        Query::SbPayments {
            payments: None,
            ..
        }
    ));
    assert!(
        matches!(&a.q[11], Query::SbPayments { payments: Some(p), .. } if p.len() == 2 && p[0].amount == 1_000_000_000 && p[1].script == [0x51])
    );
    assert!(matches!(&a.q[12], Query::SbPayments { payments: Some(p), .. } if p.is_empty()));
    for q in &a.q {
        assert_eq!(Query::try_from(&q.to_value()).as_ref(), Ok(q));
    }
    assert_eq!(a.x().get("seq"), Some(&Value::Int(57)));
}

#[test]
fn sparse_payload_keeps_number_text() {
    let Record::Sparse(s) = record(14) else {
        panic!("not an S")
    };
    assert_eq!(s.rpc, b"creditpool");
    assert_eq!(s.args(), [Value::Int(101)]);
    let res = s.res();
    assert_eq!(res.get(b"credit_pool_balance"), Some(&Value::Num("50.12345678".into())));
    assert_eq!(res.get(b"next_limit"), Some(&Value::Num("1e3".into())));
    assert_eq!(res.get(b"zeta"), Some(&Value::Num("0".into())));
    let a = res.get(b"obj").and_then(|o| o.get(b"a")).and_then(Value::as_arr).expect("a");
    assert_eq!(a[2], Value::Num("-0".into()));
    assert_eq!(a[5], Value::Str(b"s\xe9\n".to_vec()));
}

#[test]
fn building_a_record_gives_the_golden_bytes() {
    // The rdashd-style partial record (line 7): only the keys the writer computes.
    let mut c = Outcome::connect(Ident {
        h: 102,
        b: hx(0xb102),
        org: Origin::Replay,
    });
    c.s_mut().set("work", Value::hash(&hx(0x1cb))).unwrap();
    c.s_mut().set("ver", Value::Int(536_870_912)).unwrap();
    c.s_mut().set("prev", Value::hash(&hx(0xb101))).unwrap();
    assert_eq!(Record::Outcome(c).to_line(), lines()[6], "set order does not matter");

    let mut d = Outcome::disconnect(Ident {
        h: 104,
        b: hx(0xb105),
        org: Origin::Replay,
    });
    d.s_mut().set("res", Value::string("unclean")).unwrap();
    d.o_mut().set_effects(&[]).unwrap();
    d.x_mut().set("seq", Value::Int(62)).unwrap();
    assert_eq!(Record::Outcome(d).to_line(), lines()[11]);

    let mut a = Attempt::new(
        Ident {
            h: 102,
            b: hx(0xb102),
            org: Origin::Proposal,
        },
        1_760_227_301,
    );
    a.x_mut().set("seq", Value::Int(58)).unwrap();
    assert_eq!(Record::Attempt(a).to_line(), lines()[13]);

    let h = Header {
        file: FileKind::Decisions,
        src: Writer::Dashd,
        ver: b"v24.0.0rc3-0364ed626e".to_vec(),
        chain: Chain::Regtest,
        run: b"4711-1760227200123456".to_vec(),
    };
    assert_eq!(Record::Header(h).to_line(), lines()[1]);
}

#[test]
fn record_accessors_are_generic() {
    let r = record(3);
    assert_eq!(r.kind(), Kind::Connect);
    assert_eq!(r.block(), Some((101, &hx(0xb101))));
    assert_eq!(r.origin(), Some(Origin::Tip));
    assert!(r.fields(Class::Semantic).is_some());
    assert_eq!(r.fields(Class::Schedule).and_then(|f| f.get("seq")), Some(&Value::Int(57)));
    let h = record(0);
    assert!(h.block().is_none() && h.fields(Class::Semantic).is_none());
    let a = record(12);
    assert!(a.fields(Class::Semantic).is_none() && a.fields(Class::Schedule).is_some());
}

#[test]
fn lines_iterator_reports_line_numbers_and_absolute_offsets() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&lines()[0]);
    let second = buf.len();
    buf.extend_from_slice(b"{\"k\":\"Q\"}\n");
    buf.extend_from_slice(&lines()[1]);
    let items: Vec<_> = Lines::new(&buf).collect();
    assert_eq!(items.len(), 3);
    assert!(items[0].1.is_ok() && items[2].1.is_ok());
    assert_eq!(items[1].0, 2);
    let err = items[1].1.as_ref().unwrap_err();
    assert_eq!(err.offset, second + 5, "offset of the kind letter in the buffer");
}
