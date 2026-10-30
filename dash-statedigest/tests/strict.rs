//! The strict parser's rejections: one test per rule of spec §3, with the error kind and the byte
//! offset it is reported at. Every accepted case also re-encodes to the same bytes.

use dash_statedigest::{
    FieldError, Hash256, Ident, Lines, Origin, Outcome, ParseError, ParseErrorKind as K, Record,
    Sparse, Value, decode_hex, parse_line,
};

const B: &str = "00000000000000000000000000000000000000000000000000000000000000b1";

fn c_line(s: &str, o: &str, x: &str) -> Vec<u8> {
    format!("{{\"k\":\"C\",\"h\":1,\"b\":\"{B}\",\"org\":\"tip\",\"s\":{s},\"o\":{o},\"x\":{x}}}\n")
        .into_bytes()
}

/// A valid `C` line with the given `s` object.
fn c_s(s: &str) -> Vec<u8> {
    c_line(s, "{}", "{\"seq\":2}")
}

fn s_line(args: &str, res: &str) -> Vec<u8> {
    format!("{{\"k\":\"S\",\"h\":1,\"b\":\"{B}\",\"rpc\":\"x\",\"args\":{args},\"res\":{res}}}\n")
        .into_bytes()
}

fn ok(line: &[u8]) -> Record {
    let record =
        parse_line(line).unwrap_or_else(|e| panic!("{}: {e}", String::from_utf8_lossy(line)));
    assert_eq!(record.to_line(), line, "accepted lines re-encode to themselves");
    record
}

fn fails(line: &[u8]) -> ParseError {
    match parse_line(line) {
        Ok(r) => panic!("accepted {}: {r:?}", String::from_utf8_lossy(line)),
        Err(e) => e,
    }
}

fn find(line: &[u8], needle: &str) -> usize {
    let n = needle.as_bytes();
    line.windows(n.len()).position(|w| w == n).unwrap_or_else(|| panic!("no {needle:?}"))
}

/// The error for `line` is `kind` at the first byte of `needle`.
fn rejects(line: &[u8], kind: K, needle: &str) {
    let e = fails(line);
    assert_eq!(e.kind, kind, "{}", String::from_utf8_lossy(line));
    assert_eq!(e.offset, find(line, needle), "{}", String::from_utf8_lossy(line));
}

#[test]
fn the_base_lines_are_accepted() {
    ok(&c_s("{\"ver\":1}"));
    ok(&c_s("{}"));
    ok(&s_line("[1,\"a\"]", "{\"a\":1}"));
}

#[test]
fn whitespace_is_rejected_everywhere() {
    let l = c_s("{\"ver\":1}");
    let mut with_space = l.clone();
    with_space.insert(0, b' ');
    assert_eq!(
        fails(&with_space),
        ParseError {
            offset: 0,
            kind: K::Whitespace
        }
    );
    rejects(&c_s("{\"ver\": 1}"), K::Whitespace, " 1");
    rejects(&c_s("{\"ver\":1, \"mrkl\":1}"), K::Whitespace, " \"mrkl");
    rejects(&c_s("{ \"ver\":1}"), K::Whitespace, " \"ver");
    rejects(&c_s("{\"ver\":1 }"), K::Whitespace, " }");
    rejects(&c_s("{\"ver\"\t:1}"), K::Whitespace, "\t");
    let trailing = [&l[..l.len() - 1], b" \n"].concat();
    assert_eq!(fails(&trailing).kind, K::Whitespace);
    assert_eq!(fails(&trailing).offset, l.len() - 1);
    rejects(&s_line("[1, 2]", "{}"), K::Whitespace, " 2");
    rejects(&s_line("[]", "{\"a\": 1}"), K::Whitespace, " 1");
}

#[test]
fn bom_and_carriage_return_are_rejected() {
    let l = c_s("{}");
    let bom = [&[0xef, 0xbb, 0xbf][..], &l].concat();
    assert_eq!(
        fails(&bom),
        ParseError {
            offset: 0,
            kind: K::Bom
        }
    );
    let crlf = [&l[..l.len() - 1], b"\r\n"].concat();
    assert_eq!(
        fails(&crlf),
        ParseError {
            offset: l.len() - 1,
            kind: K::CarriageReturn
        }
    );
    let mid = c_s("{\"ver\":1\r}");
    rejects(&mid, K::CarriageReturn, "\r");
}

#[test]
fn only_the_canonical_escapes_exist() {
    let rline = |dbg: &str| {
        format!("{{\"k\":\"R\",\"h\":1,\"b\":\"{B}\",\"org\":\"tip\",\"s\":{{}},\"o\":{{}},\"x\":{{\"dbg\":\"{dbg}\"}}}}\n")
            .into_bytes()
    };
    // Accepted: verbatim text, \" \\ and lowercase \u00xx for bytes that cannot be verbatim.
    ok(&rline("plain text ~"));
    ok(&rline("q\\\" b\\\\ \\u0000\\u000a\\u000d\\u001f\\u007f\\u0080\\u00ff"));
    for bad in
        ["\\n", "\\t", "\\r", "\\b", "\\f", "\\/", "\\u0041", "\\u0022", "\\u005c", "\\u007e"]
    {
        let l = rline(bad);
        let e = fails(&l);
        assert!(matches!(e.kind, K::BadEscape(_)), "{bad}: {e}");
        assert_eq!(e.offset, find(&l, bad), "{bad}");
    }
    for bad in ["\\u00E9", "\\u00eF", "\\u0100", "\\u1234", "\\u00zz", "\\u00e", "\\u", "\\x41"] {
        let l = rline(bad);
        let e = fails(&l);
        assert!(matches!(e.kind, K::BadEscape(_)), "{bad}: {e}");
        assert_eq!(e.offset, find(&l, bad), "{bad}");
    }
    // A key may not be escaped either.
    let escaped_key = c_s("{\"v\\u0065r\":1}");
    assert!(matches!(fails(&escaped_key).kind, K::BadEscape(_)));
}

#[test]
fn raw_bytes_outside_printable_ascii_are_rejected() {
    for raw in [0x00u8, 0x01, 0x1f, 0x7f, 0x80, 0xc3, 0xff] {
        let mut l = c_s("{}");
        let at = find(&l, "\"seq\":2");
        l.splice(at + 1..at + 2, [raw]);
        // The key `seq` is damaged: the string reader meets the raw byte first.
        let e = fails(&l);
        assert_eq!(e.kind, K::RawByteInString(raw));
        assert_eq!(e.offset, at + 1);
    }
    let unterminated = b"{\"k\":\"C\n".to_vec();
    assert_eq!(fails(&unterminated).kind, K::UnterminatedString);
}

#[test]
fn integer_grammar() {
    rejects(&c_s("{\"ver\":01}"), K::BadInteger("leading zero"), "01}");
    rejects(&c_s("{\"ver\":-01}"), K::BadInteger("leading zero"), "01}");
    rejects(&c_s("{\"ver\":00}"), K::BadInteger("leading zero"), "00}");
    rejects(&c_s("{\"ver\":-0}"), K::BadInteger("-0"), "-0");
    let l = c_s("{\"ver\":-}");
    assert_eq!(
        fails(&l),
        ParseError {
            offset: find(&l, "-}") + 1,
            kind: K::BadInteger("a digit is required")
        }
    );
    rejects(
        &c_s("{\"ver\":+1}"),
        K::UnexpectedByte {
            found: b'+',
            expected: "a value",
        },
        "+1",
    );
    rejects(&c_s("{\"ver\":9223372036854775808}"), K::IntegerOutOfRange, "9223");
    rejects(&c_s("{\"ver\":-9223372036854775809}"), K::IntegerOutOfRange, "-9223");
    rejects(&c_s("{\"ver\":18446744073709551616}"), K::IntegerOutOfRange, "1844");
    rejects(&c_s("{\"ver\":99999999999999999999999}"), K::IntegerOutOfRange, "9999");
    // The extremes are fine and round-trip.
    let r = ok(&c_s("{\"ver\":9223372036854775807,\"time\":-9223372036854775808}"));
    let Record::Outcome(c) = r else {
        unreachable!()
    };
    assert_eq!(c.s().get("ver"), Some(&Value::Int(i64::MAX)));
    assert_eq!(c.s().get("time"), Some(&Value::Int(i64::MIN)));
    ok(&c_s("{\"ver\":0,\"time\":-1}"));
}

#[test]
fn floats_are_rejected_outside_s_payloads() {
    rejects(&c_s("{\"ver\":1.5}"), K::FloatNotAllowed, ".5");
    rejects(&c_s("{\"ver\":1.0}"), K::FloatNotAllowed, ".0");
    rejects(&c_s("{\"ver\":1e3}"), K::FloatNotAllowed, "e3");
    rejects(&c_s("{\"ver\":1E3}"), K::FloatNotAllowed, "E3");
    rejects(&c_s("{\"ver\":-0.0}"), K::FloatNotAllowed, ".0");
    // In a header-ish top-level integer, and in S arguments.
    let l = format!(
        "{{\"k\":\"C\",\"h\":1.5,\"b\":\"{B}\",\"org\":\"tip\",\"s\":{{}},\"o\":{{}},\"x\":{{}}}}\n"
    );
    rejects(l.as_bytes(), K::FloatNotAllowed, ".5");
    rejects(&s_line("[1.5]", "{}"), K::FloatNotAllowed, ".5");
    rejects(&s_line("[[2e1]]", "{}"), K::FloatNotAllowed, "e1");
}

#[test]
fn payload_numbers_are_kept_as_text_and_checked_against_the_grammar() {
    let line = s_line("[]", "[0,-0,1,-1.5,0.0,10.5e-3,1E+5,123456789012345678901234567890,0e0]");
    let Record::Sparse(s) = ok(&line) else {
        unreachable!()
    };
    let items = s.res().as_arr().unwrap();
    assert!(items.iter().all(|v| matches!(v, Value::Num(_))), "every payload number is a Num");
    assert_eq!(items[1], Value::Num("-0".into()));
    for (bad, at) in [
        ("01", "01"),
        ("1.", "1."),
        (".5", ".5"),
        ("1e", "1e"),
        ("1e+", "1e+"),
        ("-", "-]"),
        ("1-2", "1-2"),
        ("0x10", "x10"),
        ("1.2.3", "1.2.3"),
    ] {
        let l = s_line("[]", &format!("[{bad}]"));
        let e = fails(&l);
        let kind_ok = matches!(e.kind, K::BadNumber | K::UnexpectedByte { .. });
        assert!(kind_ok, "{bad}: {e}");
        assert_eq!(e.offset, find(&l, at), "{bad}");
    }
    rejects(
        &s_line("[]", "[NaN]"),
        K::UnexpectedByte {
            found: b'N',
            expected: "a value",
        },
        "NaN",
    );
    rejects(
        &s_line("[]", "[+1]"),
        K::UnexpectedByte {
            found: b'+',
            expected: "a value",
        },
        "+1",
    );
}

#[test]
fn duplicate_keys_are_rejected() {
    rejects(&c_s("{\"ver\":1,\"ver\":2}"), K::DuplicateKey, "\"ver\":2");
    let dup_mrkl = format!("{{\"mrkl\":\"{B}\",\"mrkl\":\"{B}\"}}");
    rejects(&c_s(&dup_mrkl), K::DuplicateKey, &format!("\"mrkl\":\"{B}\"}}"));
    // A repeated top-level key.
    let l = format!("{{\"k\":\"C\",\"h\":1,\"h\":2,\"b\":\"{B}\"}}\n");
    rejects(l.as_bytes(), K::DuplicateKey, "\"h\":2");
    let l = format!(
        "{{\"k\":\"C\",\"h\":1,\"b\":\"{B}\",\"org\":\"tip\",\"s\":{{}},\"o\":{{}},\"x\":{{}},\"s\":{{}}}}\n"
    );
    assert_eq!(fails(l.as_bytes()).kind, K::DuplicateKey);
    // Payload objects, at any depth, and S arguments.
    rejects(&s_line("[]", "{\"a\":1,\"a\":2}"), K::DuplicateKey, "\"a\":2");
    rejects(&s_line("[]", "[{\"a\":{\"b\":1,\"b\":1}}]"), K::DuplicateKey, "\"b\":1}");
    rejects(&s_line("[{\"a\":1,\"a\":2}]", "{}"), K::DuplicateKey, "\"a\":2");
}

#[test]
fn unknown_keys_are_rejected() {
    rejects(&c_s("{\"nope\":1}"), K::UnknownKey, "\"nope\"");
    rejects(&c_s("{\"ver\":1,\"nope\":1}"), K::UnknownKey, "\"nope\"");
    // A key of another sub-object or another kind.
    rejects(&c_line("{}", "{\"seq\":1}", "{}"), K::UnknownKey, "\"seq\"");
    rejects(&c_line("{}", "{}", "{\"fx\":[]}"), K::UnknownKey, "\"fx\"");
    rejects(&c_line("{\"res\":\"ok\"}", "{}", "{}"), K::UnknownKey, "\"res\"");
    rejects(&c_line("{}", "{}", "{\"dbg\":null}"), K::UnknownKey, "\"dbg\"");
    // Case matters.
    rejects(&c_s("{\"Ver\":1}"), K::UnknownKey, "\"Ver\"");
    // An extra top-level key, before the closing brace.
    let l = format!(
        "{{\"k\":\"C\",\"h\":1,\"b\":\"{B}\",\"org\":\"tip\",\"s\":{{}},\"o\":{{}},\"x\":{{}},\"extra\":1}}\n"
    );
    rejects(l.as_bytes(), K::UnknownKey, "\"extra\"");
    // `org` belongs to C, R, D and A only; S has no `org`.
    let l = format!(
        "{{\"k\":\"S\",\"h\":1,\"b\":\"{B}\",\"org\":\"tip\",\"rpc\":\"x\",\"args\":[],\"res\":{{}}}}\n"
    );
    rejects(l.as_bytes(), K::UnknownKey, "\"org\"");
}

#[test]
fn keys_must_follow_spec_order() {
    // Inside a sub-object.
    rejects(&c_s(&format!("{{\"mrkl\":\"{B}\",\"ver\":1}}")), K::KeyOutOfOrder, "\"ver\"");
    rejects(&c_s("{\"bur\":1,\"bls\":true}"), K::KeyOutOfOrder, "\"bls\"");
    rejects(&c_line("{}", "{}", "{\"dbg\":null,\"seq\":1}"), K::UnknownKey, "\"dbg\"");
    let r = |x: &str| {
        format!("{{\"k\":\"R\",\"h\":1,\"b\":\"{B}\",\"org\":\"tip\",\"s\":{{}},\"o\":{{}},\"x\":{x}}}\n")
            .into_bytes()
    };
    rejects(&r("{\"dbg\":null,\"seq\":1}"), K::KeyOutOfOrder, "\"seq\"");
    ok(&r("{\"seq\":1,\"dbg\":null}"));
    // Between the top-level keys.
    let swapped_hb = format!(
        "{{\"k\":\"C\",\"b\":\"{B}\",\"h\":1,\"org\":\"tip\",\"s\":{{}},\"o\":{{}},\"x\":{{}}}}\n"
    );
    rejects(swapped_hb.as_bytes(), K::KeyOutOfOrder, "\"b\"");
    let swapped_so = format!(
        "{{\"k\":\"C\",\"h\":1,\"b\":\"{B}\",\"org\":\"tip\",\"o\":{{}},\"s\":{{}},\"x\":{{}}}}\n"
    );
    rejects(swapped_so.as_bytes(), K::KeyOutOfOrder, "\"o\":{},\"s\"");
    let a_swapped = format!(
        "{{\"k\":\"A\",\"h\":1,\"b\":\"{B}\",\"t\":5,\"org\":\"tip\",\"q\":[],\"x\":{{}}}}\n"
    );
    rejects(a_swapped.as_bytes(), K::KeyOutOfOrder, "\"t\"");
    // `k` first.
    let k_late = format!("{{\"h\":1,\"k\":\"C\",\"b\":\"{B}\"}}\n");
    assert_eq!(
        fails(k_late.as_bytes()),
        ParseError {
            offset: 1,
            kind: K::MissingKey("k")
        }
    );
}

#[test]
fn required_top_level_keys_cannot_be_skipped() {
    let no_x =
        format!("{{\"k\":\"C\",\"h\":1,\"b\":\"{B}\",\"org\":\"tip\",\"s\":{{}},\"o\":{{}}}}\n");
    rejects(no_x.as_bytes(), K::MissingKey("x"), "}\n");
    let no_o =
        format!("{{\"k\":\"C\",\"h\":1,\"b\":\"{B}\",\"org\":\"tip\",\"s\":{{}},\"x\":{{}}}}\n");
    rejects(no_o.as_bytes(), K::KeyOutOfOrder, "\"x\"");
    let no_org =
        format!("{{\"k\":\"D\",\"h\":1,\"b\":\"{B}\",\"s\":{{}},\"o\":{{}},\"x\":{{}}}}\n");
    rejects(no_org.as_bytes(), K::KeyOutOfOrder, "\"s\"");
    assert_eq!(
        fails(b"{}\n").kind,
        K::UnexpectedByte {
            found: b'}',
            expected: "a string"
        }
    );
    let no_res = format!("{{\"k\":\"S\",\"h\":1,\"b\":\"{B}\",\"rpc\":\"x\",\"args\":[]}}\n");
    rejects(no_res.as_bytes(), K::MissingKey("res"), "}\n");
}

fn line(kind: char, s: &str, o: &str, x: &str) -> Vec<u8> {
    format!(
        "{{\"k\":\"{kind}\",\"h\":1,\"b\":\"{B}\",\"org\":\"tip\",\"s\":{s},\"o\":{o},\"x\":{x}}}\n"
    )
    .into_bytes()
}

/// The line is refused with `WrongShape` for `key`, at the first byte of `needle`.
fn bad_shape(line: &[u8], key: &str, needle: &str) {
    let e = fails(line);
    match &e.kind {
        K::WrongShape {
            key: k,
            ..
        } => assert_eq!(*k, key, "{e}"),
        other => panic!("{other:?} for {}", String::from_utf8_lossy(line)),
    }
    assert_eq!(e.offset, find(line, needle), "{}", String::from_utf8_lossy(line));
}

#[test]
fn values_must_have_the_shape_of_their_key() {
    let upper = B.replace("b1", "B1");
    let short = &B[1..];
    let long = format!("0{B}");
    for v in [
        format!("\"{upper}\""),
        format!("\"{short}\""),
        format!("\"{long}\""),
        "5".to_string(),
        "null".to_string(),
        "true".to_string(),
        "[]".to_string(),
        "\"0x00\"".to_string(),
    ] {
        // The value is followed by the `}` that closes `s`; the offset is its first byte.
        bad_shape(&c_s(&format!("{{\"mrkl\":{v}}}")), "mrkl", &format!("{v}}}"));
    }
    // `prev` may be null (genesis), `mrkl` may not.
    ok(&c_s("{\"prev\":null}"));
    // u32 fields.
    ok(&c_s("{\"bits\":0,\"nonce\":4294967295}"));
    bad_shape(&c_s("{\"bits\":4294967296}"), "bits", "4294967296");
    bad_shape(&c_s("{\"nonce\":-1}"), "nonce", "-1");
    bad_shape(&c_s("{\"bls\":\"true\"}"), "bls", "\"true\"");
    bad_shape(&c_s("{\"bls\":null}"), "bls", "null");
    bad_shape(&c_s("{\"ver\":null}"), "ver", "null");
    bad_shape(&c_s("{\"ver\":\"1\"}"), "ver", "\"1\"");
    bad_shape(&c_s("{\"ver\":[1]}"), "ver", "[1]");
    bad_shape(&c_s("{\"ver\":{}}"), "ver", "{}");
}

#[test]
fn compound_fields_are_checked() {
    let h1 = format!("\"{}\"", "0".repeat(63) + "1");
    let h2 = format!("\"{}\"", "0".repeat(63) + "2");
    // qc: strictly ascending by type, then quorum hash; rows are [int, hash, hash].
    ok(&c_s(&format!("{{\"qc\":[[100,{h1},{h1}],[100,{h2},{h1}],[101,{h1},{h1}]]}}")));
    ok(&c_s("{\"qc\":[]}"));
    ok(&c_s("{\"qc\":null}"));
    for bad in [
        format!("[[101,{h1},{h1}],[100,{h1},{h1}]]"),
        format!("[[100,{h2},{h1}],[100,{h1},{h1}]]"),
        format!("[[100,{h1},{h1}],[100,{h1},{h2}]]"),
        format!("[[100,{h1}]]"),
        format!("[[\"100\",{h1},{h1}]]"),
        format!("[[100,{h1},5]]"),
        format!("[{h1}]"),
        "{}".to_string(),
    ] {
        bad_shape(&c_s(&format!("{{\"qc\":{bad}}}")), "qc", &bad);
    }
    // cp.
    ok(&c_s(&format!("{{\"cp\":[1,2,3,{h1}]}}")));
    bad_shape(&c_s(&format!("{{\"cp\":[1,2,{h1}]}}")), "cp", "[1,2,");
    bad_shape(&c_s("{\"cp\":[1,2,3,4]}"), "cp", "[1,2,3,4]");
    // ehf: ascending bit.
    ok(&c_s("{\"ehf\":[[1,5],[2,3],[28,90]]}"));
    ok(&c_s("{\"ehf\":[]}"));
    bad_shape(&c_s("{\"ehf\":[[2,3],[1,5]]}"), "ehf", "[[2,3]");
    bad_shape(&c_s("{\"ehf\":[[2,3],[2,5]]}"), "ehf", "[[2,3]");
    bad_shape(&c_s("{\"ehf\":[[2]]}"), "ehf", "[[2]]");
    // dep: [name, state, since] with a known state.
    ok(&c_s("{\"dep\":[[\"v24\",\"locked_in\",96],[\"x\",\"failed\",0]]}"));
    bad_shape(&c_s("{\"dep\":[[\"v24\",\"LOCKED_IN\",96]]}"), "dep", "[[\"v24\"");
    bad_shape(&c_s("{\"dep\":[[\"v24\",\"active\"]]}"), "dep", "[[\"v24\"");
    bad_shape(&c_s("{\"dep\":[[\"v24\",\"active\",\"1\"]]}"), "dep", "[[\"v24\"");
    // bi and stats: seven integers; stats may be null.
    ok(&c_s("{\"bi\":[0,0,0,0,0,0,0],\"stats\":null}"));
    bad_shape(&c_s("{\"bi\":[0,0,0,0,0,0]}"), "bi", "[0,0,0,0,0,0]");
    bad_shape(&c_s("{\"bi\":[0,0,0,0,0,0,0,0]}"), "bi", "[0,0,0,0,0,0,0,0]");
    bad_shape(&c_s("{\"bi\":null}"), "bi", "null");
    bad_shape(&c_s("{\"stats\":[0,0,0,0,0,0,\"0\"]}"), "stats", "[0,0,0,0,0,0,\"");
    bad_shape(&c_s("{\"muhash\":5}"), "muhash", "5}");
}

#[test]
fn effect_and_enum_fields_are_checked() {
    let h = format!("\"{}\"", "0".repeat(63) + "1");
    ok(&line(
        'C',
        "{}",
        &format!(
            "{{\"fx\":[[\"is_rm\",{h},{h}],[\"ban_reset\",{h},5,false],[\"mn_changed\",true]]}}"
        ),
        "{}",
    ));
    for bad in [
        "[[\"nope\"]]".to_string(),
        "[[]]".to_string(),
        "[\"mn_changed\"]".to_string(),
        "[[\"mn_changed\"]]".to_string(),
        "[[\"mn_changed\",1]]".to_string(),
        "[[\"mn_changed\",true,true]]".to_string(),
        format!("[[\"is_rm\",{h}]]"),
        format!("[[\"snap\",\"1\",{h},{h}]]"),
        format!("[[\"qc_unmine\",1,\"{}\"]]", "A".repeat(64)),
        "null".to_string(),
        "{\"a\":1}".to_string(),
    ] {
        bad_shape(&line('C', "{}", &format!("{{\"fx\":{bad}}}"), "{}"), "fx", &bad);
    }
    ok(&line('R', "{\"res\":\"error\",\"cls\":\"chainlock\",\"why\":\"exception\"}", "{}", "{}"));
    bad_shape(&line('R', "{\"res\":\"fail\"}", "{}", "{}"), "res", "\"fail\"");
    bad_shape(&line('R', "{\"res\":null}", "{}", "{}"), "res", "null");
    bad_shape(&line('R', "{\"cls\":\"BLOCK_CONSENSUS\"}", "{}", "{}"), "cls", "\"BLOCK");
    bad_shape(&line('R', "{\"why\":7}", "{}", "{}"), "why", "7}");
    bad_shape(&line('R', "{}", "{}", "{\"dbg\":7}"), "dbg", "7}");
    bad_shape(&line('R', "{}", "{}", "{\"seq\":null}"), "seq", "null");
    bad_shape(&line('D', "{\"res\":\"invalid\"}", "{}", "{}"), "res", "\"invalid\"");
    ok(&line('D', "{\"res\":\"failed\"}", "{}", "{}"));
}

#[test]
fn identity_and_decision_fields_are_checked() {
    let l = format!(
        "{{\"k\":\"C\",\"h\":\"1\",\"b\":\"{B}\",\"org\":\"tip\",\"s\":{{}},\"o\":{{}},\"x\":{{}}}}\n"
    );
    bad_shape(l.as_bytes(), "h", "\"1\"");
    let l = format!(
        "{{\"k\":\"C\",\"h\":1,\"b\":\"{}\",\"org\":\"tip\",\"s\":{{}},\"o\":{{}},\"x\":{{}}}}\n",
        B.to_uppercase()
    );
    bad_shape(l.as_bytes(), "b", &format!("\"{}", B.to_uppercase()));
    for org in ["TIP", "other", "", "tip "] {
        let l = format!(
            "{{\"k\":\"C\",\"h\":1,\"b\":\"{B}\",\"org\":\"{org}\",\"s\":{{}},\"o\":{{}},\"x\":{{}}}}\n"
        );
        bad_shape(l.as_bytes(), "org", &format!("\"{org}\""));
    }
    let a = |t: &str, q: &str| {
        format!(
            "{{\"k\":\"A\",\"h\":1,\"b\":\"{B}\",\"org\":\"tip\",\"t\":{t},\"q\":{q},\"x\":{{}}}}\n"
        )
        .into_bytes()
    };
    ok(&a("1760227300", "[[\"ibd\",true]]"));
    bad_shape(&a("\"1\"", "[]"), "t", "\"1\"");
    for bad in [
        "[[\"ibd\"]]",
        "[[\"ibd\",1]]",
        "[[\"nope\",true]]",
        "[[\"cl_best\"]]",
        "[[\"cl_best\",1,2]]",
        "[[\"cl_best\",1,\"00\",\"abc\"]]",
        "[[\"is_conflict\",\"00\",null]]",
        "[[\"sb_payments\",1,[[1]]]]",
        "[[\"sb_payments\",1,[[1,\"0g\"]]]]",
        "[[\"sb_payments\",1,[[1,\"AB\"]]]]",
        "[[\"cl_has\",1,2,true]]",
        "null",
        "{}",
    ] {
        let line = a("1", bad);
        // Only the arity/type problems are checked here, not each hash.
        bad_shape(&line, "q", bad);
    }
}

#[test]
fn headers() {
    let h = |v: &str, file: &str, src: &str, chain: &str| {
        format!("{{\"k\":\"H\",\"v\":{v},\"file\":\"{file}\",\"src\":\"{src}\",\"ver\":\"v\",\"chain\":\"{chain}\",\"run\":\"1-2\"}}\n").into_bytes()
    };
    ok(&h("1", "digest", "rdashd", "devnet"));
    let l = h("2", "digest", "dashd", "main");
    rejects(&l, K::UnsupportedVersion, "2,");
    bad_shape(&h("\"1\"", "digest", "dashd", "main"), "v", "\"1\"");
    bad_shape(&h("1", "Digest", "dashd", "main"), "file", "\"Digest\"");
    bad_shape(&h("1", "digest", "core", "main"), "src", "\"core\"");
    bad_shape(&h("1", "digest", "dashd", "mainnet"), "chain", "\"mainnet\"");
    let no_run = b"{\"k\":\"H\",\"v\":1,\"file\":\"digest\",\"src\":\"dashd\",\"ver\":\"v\",\"chain\":\"main\"}\n";
    rejects(no_run, K::MissingKey("run"), "}\n");
}

#[test]
fn unknown_kinds_are_rejected() {
    for k in ["Z", "c", "CC", "", "h"] {
        let l = format!("{{\"k\":\"{k}\",\"h\":1}}\n");
        rejects(l.as_bytes(), K::UnknownKind, &format!("\"{k}\""));
    }
    let l = b"{\"k\":\"C\\u0043\"}\n";
    assert!(matches!(fails(l).kind, K::BadEscape(_)));
    assert_eq!(
        fails(b"{\"k\":1}\n").kind,
        K::UnexpectedByte {
            found: b'1',
            expected: "a string"
        }
    );
}

#[test]
fn nothing_may_follow_the_line() {
    let l = c_s("{}");
    let with = |tail: &[u8]| [&l[..], tail].concat();
    assert_eq!(
        fails(&with(b"x")),
        ParseError {
            offset: l.len(),
            kind: K::TrailingBytes
        }
    );
    assert_eq!(
        fails(&with(b"\n")),
        ParseError {
            offset: l.len(),
            kind: K::TrailingBytes
        }
    );
    assert_eq!(
        fails(&[&l[..], &l[..]].concat()),
        ParseError {
            offset: l.len(),
            kind: K::TrailingBytes
        }
    );
    let no_nl = &l[..l.len() - 1];
    assert_eq!(
        fails(no_nl),
        ParseError {
            offset: no_nl.len(),
            kind: K::MissingTerminator
        }
    );
    let extra_brace = [no_nl, b"}\n"].concat();
    assert_eq!(
        fails(&extra_brace),
        ParseError {
            offset: no_nl.len(),
            kind: K::TrailingBytes
        }
    );
    let comma = [no_nl, b",\n"].concat();
    assert_eq!(
        fails(&comma),
        ParseError {
            offset: no_nl.len(),
            kind: K::TrailingBytes
        }
    );
    assert_eq!(fails(b"").kind, K::UnexpectedEnd);
    assert_eq!(
        fails(b"\n"),
        ParseError {
            offset: 0,
            kind: K::BlankLine
        }
    );
    assert_eq!(
        fails(b"[]\n").kind,
        K::UnexpectedByte {
            found: b'[',
            expected: "`{`"
        }
    );
}

#[test]
fn payload_object_keys_are_sorted_bytewise() {
    ok(&s_line("[]", "{}"));
    ok(&s_line("[]", "{\"B\":1,\"a\":1,\"ab\":1,\"b\":1}"));
    ok(&s_line("[]", "{\"a\":1,\"\\u0080\":1}"));
    ok(&s_line("[]", "[{\"a\":{\"b\":1,\"c\":2}},{\"a\":{}}]"));
    rejects(&s_line("[]", "{\"b\":1,\"a\":2}"), K::UnsortedKeys, "\"a\":2");
    rejects(&s_line("[]", "{\"a\":1,\"B\":1}"), K::UnsortedKeys, "\"B\"");
    rejects(&s_line("[]", "{\"ab\":1,\"a\":1}"), K::UnsortedKeys, "\"a\":1}");
    rejects(&s_line("[]", "{\"\\u0080\":1,\"a\":1}"), K::UnsortedKeys, "\"a\"");
    rejects(&s_line("[]", "[{\"a\":{\"c\":1,\"b\":2}}]"), K::UnsortedKeys, "\"b\":2");
    rejects(&s_line("[{\"b\":1,\"a\":1}]", "{}"), K::UnsortedKeys, "\"a\":1}");
}

#[test]
fn nesting_is_bounded() {
    let nested = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
    ok(&s_line("[]", &nested(129)));
    let l = s_line("[]", &nested(130));
    assert_eq!(fails(&l).kind, K::TooDeep);
    ok(&s_line(&format!("[{}]", nested(128)), "{}"));
    assert_eq!(fails(&s_line(&format!("[{}]", nested(129)), "{}")).kind, K::TooDeep);
    // The same limits as the constructor.
    let deep = |n: usize| (0..n).fold(Value::Arr(vec![]), |v, _| Value::Arr(vec![v]));
    let b = Hash256::from_hex(B.as_bytes()).unwrap();
    assert!(Sparse::new(1, b, "x", vec![], deep(128)).is_ok());
    assert!(Sparse::new(1, b, "x", vec![], deep(129)).is_err());
    assert!(Sparse::new(1, b, "x", vec![deep(127)], Value::Arr(vec![])).is_ok());
    assert!(Sparse::new(1, b, "x", vec![deep(128)], Value::Arr(vec![])).is_err());
}

#[test]
fn lines_reports_blank_lines_and_a_missing_final_newline() {
    let good = c_s("{}");
    let mut buf = good.clone();
    buf.push(b'\n');
    buf.extend_from_slice(&good);
    buf.extend_from_slice(&good[..10]);
    let items: Vec<_> = Lines::new(&buf).collect();
    assert_eq!(items.len(), 4);
    assert!(items[0].1.is_ok() && items[2].1.is_ok());
    assert_eq!(
        items[1].1,
        Err(ParseError {
            offset: good.len(),
            kind: K::BlankLine
        })
    );
    assert_eq!(items[3].0, 4);
    assert_eq!(
        items[3].1,
        Err(ParseError {
            offset: buf.len(),
            kind: K::MissingTerminator
        })
    );
    assert_eq!(Lines::new(b"").count(), 0);
    assert_eq!(Lines::new(b"\n").next().map(|(n, r)| (n, r.is_err())), Some((1, true)));
}

#[test]
fn sparse_constructor_checks_the_payload_rules() {
    let b = Hash256::from_hex(B.as_bytes()).unwrap();
    let num = |t: &str| Value::Num(t.into());
    let obj = |keys: &[&str]| Value::obj(keys.iter().map(|k| (*k, Value::Null)));
    assert!(Sparse::new(1, b, "x", vec![Value::Int(1), Value::string("a")], num("1.50")).is_ok());
    assert!(Sparse::new(1, b, "x", vec![], Value::Int(1)).is_err(), "payload numbers are Num");
    assert!(Sparse::new(1, b, "x", vec![num("1")], Value::Null).is_err(), "Num is res-only");
    assert!(Sparse::new(1, b, "x", vec![], num("1.")).is_err());
    assert!(Sparse::new(1, b, "x", vec![], num("01")).is_err());
    assert!(Sparse::new(1, b, "x", vec![], obj(&["a", "b"])).is_ok());
    assert!(Sparse::new(1, b, "x", vec![], obj(&["b", "a"])).is_err());
    assert!(Sparse::new(1, b, "x", vec![], obj(&["a", "a"])).is_err());
    assert!(Sparse::new(1, b, "x", vec![obj(&["b", "a"])], Value::Null).is_err());
    // What it accepts, the parser accepts back.
    let s = Sparse::new(
        7,
        b,
        "creditpool",
        vec![Value::Int(7)],
        Value::obj([("n", num("0.5")), ("s", Value::Str(vec![0xff, b'"']))]),
    )
    .unwrap();
    let line = Record::Sparse(s.clone()).to_line();
    assert_eq!(parse_line(&line), Ok(Record::Sparse(s)));
}

#[test]
fn field_maps_validate_on_set() {
    let id = Ident {
        h: 1,
        b: Hash256::from_hex(B.as_bytes()).unwrap(),
        org: Origin::Tip,
    };
    let mut c = Outcome::connect(id);
    assert_eq!(c.s_mut().set("nope", Value::Null), Err(FieldError::UnknownKey));
    assert_eq!(
        c.s_mut().set("res", Value::string("ok")),
        Err(FieldError::UnknownKey),
        "R/D key on C"
    );
    assert!(matches!(c.s_mut().set("ver", Value::string("1")), Err(FieldError::Shape(_))));
    assert!(matches!(c.s_mut().set("mrkl", Value::Null), Err(FieldError::Shape(_))));
    assert_eq!(c.s_mut().set("ver", Value::Int(1)), Ok(None));
    assert_eq!(c.s_mut().set("ver", Value::Int(2)), Ok(Some(Value::Int(1))));
    c.s_mut().set("prev", Value::Null).unwrap();
    assert_eq!(c.s().get("prev"), Some(&Value::Null));
    assert_eq!(c.s().get("mrkl"), None);
    assert_eq!(c.s_mut().remove("prev"), Some(Value::Null));
    assert_eq!(c.s().get("prev"), None, "absent after remove");
    assert_eq!(
        c.o_mut().set("seq", Value::Int(1)),
        Err(FieldError::UnknownKey),
        "seq is a schedule key"
    );
    assert_eq!(c.s().specs().len(), 22);
    let keys: Vec<_> = c.s().iter().map(|(spec, _)| spec.key).collect();
    assert_eq!(keys, ["ver"]);
}

#[test]
fn hashes_are_in_display_order() {
    let h = Hash256::from_hex(B.as_bytes()).unwrap();
    assert_eq!(h.as_display_bytes()[31], 0xb1);
    assert_eq!(h.to_wire_bytes()[0], 0xb1);
    assert_eq!(Hash256::from_wire_bytes(h.to_wire_bytes()), h);
    assert_eq!(h.to_hex(), B);
    assert_eq!(format!("{h}"), B);
    assert!(Hash256::from_hex(B.to_uppercase().as_bytes()).is_none());
    assert!(Hash256::from_hex(&B.as_bytes()[1..]).is_none());
    assert_eq!(decode_hex(b"00ff"), Some(vec![0, 255]));
    assert_eq!(decode_hex(b""), Some(vec![]));
    assert_eq!(decode_hex(b"0"), None);
    assert_eq!(decode_hex(b"0F"), None);
    assert_eq!(decode_hex(b"0g"), None);
}

#[test]
fn cl_best_signature_is_96_bytes() {
    let h = "0".repeat(64);
    let a = |sig: &str| {
        format!("{{\"k\":\"A\",\"h\":1,\"b\":\"{B}\",\"org\":\"tip\",\"t\":1,\"q\":[[\"cl_best\",5,\"{h}\",\"{sig}\"]],\"x\":{{}}}}\n").into_bytes()
    };
    ok(&a(&"ab".repeat(96)));
    for bad in ["", &"ab".repeat(95), &"ab".repeat(97), &"AB".repeat(96)] {
        let l = a(bad);
        bad_shape(&l, "q", "[[\"cl_best\"");
    }
}

#[test]
fn truncated_literals_are_reported_where_they_stop() {
    let l = s_line("[]", "[tr]");
    assert_eq!(
        fails(&l),
        ParseError {
            offset: find(&l, "]}"),
            kind: K::UnexpectedByte {
                found: b']',
                expected: "a value"
            }
        }
    );
    let l = s_line("[]", "[nul,1]");
    rejects(
        &l,
        K::UnexpectedByte {
            found: b',',
            expected: "a value",
        },
        ",1]",
    );
    let l = s_line("[]", "[nulx]");
    rejects(
        &l,
        K::UnexpectedByte {
            found: b'x',
            expected: "a value",
        },
        "x]",
    );
    let cut = b"{\"k\":\"H\",\"v\":1,\"file\":nu";
    assert_eq!(
        fails(cut),
        ParseError {
            offset: cut.len(),
            kind: K::UnexpectedEnd
        }
    );
}
