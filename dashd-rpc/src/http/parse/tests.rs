//! Each case's expectation is what libevent 2.1.12 under dashd v24.0.0-rc.3 answered when probed
//! with the same bytes (the probe log is RP0's DIFF-RPC record; `rdashd`'s `rpc_diff` test
//! compares the live servers).

use super::*;

fn read(input: &[u8]) -> Result<Request, Failure> {
    Reader::new(input).read_request(|_, _| {})
}

fn bad(major: i32, minor: i32) -> Result<Request, Failure> {
    Err(Failure::BadRequest {
        major,
        minor,
    })
}

#[test]
fn a_request_line_splits_at_its_first_and_last_space() {
    let r = read(b"POST / HTTP/1.1\r\n\r\n").unwrap();
    assert_eq!((r.method, r.uri.as_str(), r.major, r.minor), (Method::Post, "/", 1, 1));
    // Trailing spaces are dropped; the target keeps inner spaces.
    let r = read(b"GET /a b HTTP/1.0   \r\n\r\n").unwrap();
    assert_eq!((r.uri.as_str(), r.minor), ("/a b", 0));
    // Two spaces after the method: the target is " /", which no handler has.
    assert_eq!(read(b"GET  / HTTP/1.1\r\n\r\n").unwrap().uri, " /");
    // Line ends may be bare LFs.
    assert_eq!(read(b"GET / HTTP/1.1\nHost: x\n\n").unwrap().header("host"), Some("x"));
    // HTTP/0.9 and 1.9 parse; a major version over 1 does not.
    assert_eq!(read(b"GET / HTTP/0.9\r\n\r\n").unwrap().minor, 9);
    assert_eq!(read(b"GET / HTTP/1.9\r\n\r\n").unwrap().minor, 9);
    assert_eq!(read(b"GET / HTTP/2.0\r\n\r\n"), bad(1, 1));
    for line in [
        &b"GET /\r\n\r\n"[..],
        b"GET / HTTP/1\r\n\r\n",
        b"GET / HTTP/1.1x\r\n\r\n",
        b"garbage\r\n\r\n",
        b"GET/ HTTP/1.1\r\n\r\n",
    ] {
        assert_eq!(read(line), bad(1, 1), "{}", String::from_utf8_lossy(line));
    }
}

#[test]
fn methods_are_case_sensitive_and_some_are_not_served() {
    assert_eq!(read(b"DELETE / HTTP/1.1\r\n\r\n").unwrap().method, Method::Delete);
    for line in [
        &b"OPTIONS / HTTP/1.1\r\n\r\n"[..],
        b"post / HTTP/1.1\r\n\r\n",
        b"PATCH / HTTP/1.1\r\n\r\n",
    ] {
        assert!(
            matches!(read(line), Err(Failure::NotImplemented(_))),
            "{}",
            String::from_utf8_lossy(line)
        );
    }
    assert!(matches!(read(b"CONNECT host:1 HTTP/1.1\r\n\r\n"), Err(Failure::NotImplemented(_))));
    assert_eq!(read(b"CONNECT host:99999 HTTP/1.1\r\n\r\n"), bad(1, 1));
}

#[test]
fn targets_parse_as_evhttp_uri_parse_does() {
    let r = read(b"POST http://h:1/ HTTP/1.1\r\n\r\n").unwrap();
    assert!(r.proxy && r.wants_close());
    assert!(!read(b"POST /?x=1 HTTP/1.1\r\n\r\n").unwrap().proxy);
    assert_eq!(read(b"POST http://h:65536/ HTTP/1.1\r\n\r\n"), bad(1, 1));
    // A relative path whose first segment holds a colon does not parse.
    assert_eq!(read(b"POST /a:b HTTP/1.1\r\n\r\n").unwrap().uri, "/a:b");
    assert_eq!(read(b"POST 1a:b HTTP/1.1\r\n\r\n"), bad(1, 1));
}

#[test]
fn headers_are_found_first_and_case_insensitively() {
    let r = read(b"GET / HTTP/1.1\r\nX:  a b \t\r\nx: second\r\nY:\r\n\r\n").unwrap();
    assert_eq!(r.header("X"), Some("a b"));
    assert_eq!(r.header("y"), Some(""));
    // A folded line continues the previous header.
    let r = read(b"GET / HTTP/1.1\r\nX: a\r\n  b\r\n\r\n").unwrap();
    assert_eq!(r.header("x"), Some("a b"));
    assert_eq!(read(b"GET / HTTP/1.1\r\n b\r\n\r\n"), bad(1, 1));
    assert_eq!(read(b"GET / HTTP/1.0\r\nnocolon\r\n\r\n"), bad(1, 0));
    assert_eq!(read(b"GET / HTTP/1.1\r\nX: a\rb\r\n\r\n"), bad(1, 1));
}

#[test]
fn the_header_block_is_limited() {
    let header = |n: usize| {
        let mut req = b"GET / HTTP/1.1\r\nX: ".to_vec();
        req.extend(std::iter::repeat_n(b'a', n));
        req.extend_from_slice(b"\r\n\r\n");
        req
    };
    // "GET / HTTP/1.1" (14) + "X: " (3) + n.
    assert!(read(&header(MAX_HEADERS_SIZE - 17)).is_ok());
    assert_eq!(read(&header(MAX_HEADERS_SIZE - 16)), bad(1, 1));
    let mut line = b"GET /".to_vec();
    line.extend(std::iter::repeat_n(b'a', 10_000));
    line.extend_from_slice(b" HTTP/1.1\r\n\r\n");
    assert_eq!(read(&line), bad(1, 1));
}

#[test]
fn bodies_follow_content_length() {
    let r = read(b"POST / HTTP/1.1\r\nContent-Length: 3\r\n\r\nabcdef").unwrap();
    assert_eq!(r.body, b"abc");
    assert_eq!(read(b"POST / HTTP/1.1\r\nContent-Length: +3\r\n\r\nabc").unwrap().body, b"abc");
    assert_eq!(read(b"POST / HTTP/1.1\r\nContent-Length:  3 \r\n\r\nabc").unwrap().body, b"abc");
    // Without a length there is no body; a GET has none either.
    assert_eq!(read(b"POST / HTTP/1.1\r\n\r\nabc").unwrap().body, b"");
    assert_eq!(read(b"GET / HTTP/1.1\r\nContent-Length: 3\r\n\r\nabc").unwrap().body, b"");
    for cl in ["-1", "x", "3x", ""] {
        let req = format!("POST / HTTP/1.0\r\nContent-Length: {cl}\r\n\r\nabc");
        assert_eq!(read(req.as_bytes()), bad(1, 0), "{cl:?}");
    }
    let req = format!("POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_BODY_SIZE + 1);
    assert!(matches!(read(req.as_bytes()), Err(Failure::TooLarge(_))));
    // A body cut short closes without a reply.
    assert_eq!(read(b"POST / HTTP/1.1\r\nContent-Length: 9\r\n\r\nabc"), Err(Failure::Closed));
}

#[test]
fn chunked_bodies_join_their_chunks() {
    let r = read(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0x2 ext\r\nde\r\n0\r\nT: 1\r\n\r\n").unwrap();
    assert_eq!(r.body, b"abcde");
    for chunks in [&b"z\r\n"[..], b"-1\r\n", b"3x\r\nabc\r\n0\r\n\r\n"] {
        let mut req = b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        req.extend_from_slice(chunks);
        assert!(
            matches!(read(&req), Err(Failure::TooLarge(_))),
            "{}",
            String::from_utf8_lossy(chunks)
        );
    }
    // A chunk-size line that never ends is refused once it outgrows a header block.
    let mut req = b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    req.extend(std::iter::repeat_n(b'0', MAX_HEADERS_SIZE + 1));
    assert!(matches!(read(&req), Err(Failure::TooLarge(_))));
}

#[test]
fn expect_is_answered_before_the_body() {
    let mut asked = Vec::new();
    let input = b"POST / HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: 2\r\n\r\n";
    let mut reader = Reader::new(&input[..]);
    // The body never comes: the read ends, after the callback.
    assert_eq!(reader.read_request(|_, buffered| asked.push(buffered)), Err(Failure::Closed));
    assert_eq!(asked, [false]);
    // With the body already sent, the callback is told so.
    let mut asked = Vec::new();
    let input = b"POST / HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: 2\r\n\r\nab";
    assert!(Reader::new(&input[..]).read_request(|_, buffered| asked.push(buffered)).is_ok());
    assert_eq!(asked, [true]);
    let r = read(b"POST / HTTP/1.1\r\nExpect: something\r\nContent-Length: 2\r\n\r\nab");
    assert!(matches!(r, Err(Failure::ExpectationFailed(_))));
    // HTTP/1.0 ignores Expect.
    assert!(read(b"POST / HTTP/1.0\r\nExpect: something\r\nContent-Length: 2\r\n\r\nab").is_ok());
    let req = format!(
        "POST / HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n",
        MAX_BODY_SIZE + 1
    );
    assert!(matches!(read(req.as_bytes()), Err(Failure::TooLarge(_))));
}

#[test]
fn pipelined_requests_read_one_at_a_time() {
    let input = b"POST / HTTP/1.1\r\nContent-Length: 1\r\n\r\naPOST /x HTTP/1.1\r\n\r\n";
    let mut reader = Reader::new(&input[..]);
    assert_eq!(reader.read_request(|_, _| {}).unwrap().body, b"a");
    assert!(reader.buffered() > 0);
    assert_eq!(reader.read_request(|_, _| {}).unwrap().uri, "/x");
    assert_eq!(reader.read_request(|_, _| {}), Err(Failure::Closed));
}

#[test]
fn connection_headers_decide_keep_alive() {
    let r = read(b"GET / HTTP/1.1\r\nConnection: Close\r\n\r\n").unwrap();
    assert!(r.wants_close() && !r.wants_keepalive());
    let r = read(b"GET / HTTP/1.0\r\nConnection: keep-alive\r\n\r\n").unwrap();
    assert!(!r.wants_close() && r.wants_keepalive());
}

#[test]
fn versions_are_signed_as_sscanf_reads_them() {
    let r = read(b"POST / HTTP/1.-1\r\n\r\n").unwrap();
    assert_eq!((r.major, r.minor), (1, -1));
    let r = read(b"POST / HTTP/-1.1\r\n\r\n").unwrap();
    assert_eq!((r.major, r.minor), (-1, 1));
    let r = read(b"GET / HTTP/+1.+1\r\n\r\n").unwrap();
    assert_eq!((r.major, r.minor), (1, 1));
}

#[test]
fn input_may_arrive_a_byte_at_a_time() {
    let input = b"POST / HTTP/1.1\r\nExpect: 100-continue\r\nTransfer-Encoding: chunked\r\n\r\n\
        3\r\nabc\r\n2\r\nde\r\n0\r\nT: 1\r\n\r\nPOST /x HTTP/1.1\r\nContent-Length: 2\r\n\r\nfg";
    let mut parser = Parser::new();
    let (mut got, mut asked) = (Vec::new(), Vec::new());
    for b in input {
        parser.feed(&[*b]);
        while let Some(r) = parser.next(false, |_, buffered| asked.push(buffered)) {
            got.push(r.unwrap());
        }
    }
    assert_eq!(asked, [false]);
    let got: Vec<_> = got.iter().map(|r| (r.uri.as_str(), r.body.as_slice())).collect();
    assert_eq!(got, [("/", &b"abcde"[..]), ("/x", b"fg")]);
    assert_eq!(parser.next(false, |_, _| {}), None);
    assert_eq!(parser.next(true, |_, _| {}), Some(Err(Failure::Closed)));
    // Input that ends mid-request is dropped, wherever it ends.
    for cut in
        [&b"GET / HTTP/1.1"[..], b"GET / HTTP/1.1\r\nHost: x", b"GET / HTTP/1.1\r\nHost: x\r\n"]
    {
        let mut parser = Parser::new();
        parser.feed(cut);
        assert_eq!(parser.next(false, |_, _| {}), None);
        assert_eq!(parser.next(true, |_, _| {}), Some(Err(Failure::Closed)));
    }
    let mut parser = Parser::new();
    parser.feed(b"POST / HTTP/1.1\r\nContent-Length: 2\r\n\r\na");
    assert_eq!(parser.next(false, |_, _| {}), None);
    assert_eq!(parser.next(true, |_, _| {}), Some(Err(Failure::Closed)));
}

#[test]
fn a_large_body_leaves_no_large_buffer() {
    let body = vec![b'x'; 4 << 20];
    for head in [
        format!("POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes(),
        b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n400000\r\n".to_vec(),
    ] {
        let mut parser = Parser::new();
        parser.feed(&head);
        parser.feed(&body);
        if head.ends_with(b"400000\r\n") {
            parser.feed(b"\r\n0\r\n\r\n");
        }
        parser.feed(b"GET /");
        assert_eq!(parser.next(false, |_, _| {}).unwrap().unwrap().body.len(), body.len());
        assert_eq!(parser.buffered(), 5);
        assert!(parser.buf.capacity() <= BUFFER_KEEP, "{}", parser.buf.capacity());
    }
}
