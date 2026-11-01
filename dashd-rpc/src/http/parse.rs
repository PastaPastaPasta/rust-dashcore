//! Reading a request as libevent 2.1.12's `evhttp` reads it (`http.c`), which Core's HTTP server
//! runs on: the request line, headers within Core's 8192-byte limit, and the body by
//! `Content-Length` or chunks. What `evhttp` accepts, refuses with an error page, or drops
//! without a reply decides what clients see, so the rules here follow it rather than RFC 9112.

/// The request line and headers may hold at most this many bytes, line ends excluded.
// Core: httpserver.cpp:42
pub const MAX_HEADERS_SIZE: usize = 8192;
/// A body may hold at most this many bytes (Core's `MAX_SIZE`).
// Core: serialize.h:33, httpserver.cpp:389
pub const MAX_BODY_SIZE: u64 = 0x0200_0000;

/// The input buffer kept between requests; what a large body grew beyond it is given back.
const BUFFER_KEEP: usize = 64 * 1024;

/// The method of a request (`evhttp_cmd_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Head,
    Put,
    Delete,
    Options,
    Trace,
    Connect,
    Patch,
    /// A name `evhttp` does not know (methods are case-sensitive).
    Unknown,
}

impl Method {
    fn parse(name: &[u8]) -> Method {
        match name {
            b"GET" => Method::Get,
            b"POST" => Method::Post,
            b"HEAD" => Method::Head,
            b"PUT" => Method::Put,
            b"DELETE" => Method::Delete,
            b"OPTIONS" => Method::Options,
            b"TRACE" => Method::Trace,
            b"CONNECT" => Method::Connect,
            b"PATCH" => Method::Patch,
            _ => Method::Unknown,
        }
    }

    /// The methods an `evhttp` server accepts by default; others get `501`.
    pub fn allowed(self) -> bool {
        matches!(self, Method::Get | Method::Post | Method::Head | Method::Put | Method::Delete)
    }

    /// `evhttp_method_may_have_body`: other methods' bodies are read as the next request.
    fn may_have_body(self) -> bool {
        matches!(self, Method::Post | Method::Put | Method::Patch)
    }
}

/// A request as read off the connection.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    /// The request target, exactly as sent (path and query).
    pub uri: String,
    /// The version as `sscanf("%d")` reads it: a component may be negative.
    pub major: i32,
    pub minor: i32,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// An absolute `http(s)://` target: `evhttp` takes it for a proxy request and closes the
    /// connection after replying.
    pub proxy: bool,
}

impl Request {
    /// `evhttp_find_header`: the first header named `name`, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }

    /// The client asked to close the connection after this reply (`evhttp_is_connection_close`).
    pub fn wants_close(&self) -> bool {
        self.proxy || self.header("Connection").is_some_and(|c| c.eq_ignore_ascii_case("close"))
    }

    /// An HTTP/1.0 client asked to keep the connection (`evhttp_is_connection_keepalive`).
    pub fn wants_keepalive(&self) -> bool {
        self.header("Connection").is_some_and(|c| c.eq_ignore_ascii_case("keep-alive"))
    }
}

pub fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// Why no request could be read. Each failure but [`Failure::Closed`] is answered with an error
/// page and the connection is closed.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// The client closed the connection, the read timed out or failed: `evhttp` closes without
    /// a reply.
    Closed,
    /// A malformed or oversized request line or header: `400`.
    BadRequest {
        major: i32,
        minor: i32,
    },
    /// A method `evhttp` refuses: `501`.
    NotImplemented(Request),
    /// `Expect` other than `100-continue`: `417`.
    ExpectationFailed(Request),
    /// A body over the limit or a malformed chunk: `413`.
    TooLarge(Request),
}

/// A connection's input as it arrives, read into requests without blocking: the bytes not
/// consumed yet, and how far the request they start has been read. Whole lines are consumed as
/// they arrive, so input that trickles in is not read twice.
#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
    state: State,
}

#[derive(Default)]
enum State {
    /// The request line.
    #[default]
    Line,
    /// The headers, and the request line and header bytes so far.
    Headers(Request, usize),
    Body(Body),
}

/// A request whose headers are read, waiting for its body.
struct Body {
    req: Request,
    /// The request line and header bytes, which a chunked body's trailer adds to.
    headers_size: usize,
    state: BodyState,
}

enum BodyState {
    /// `Content-Length` bytes.
    Length(usize),
    /// Chunks so far, and where in the next one the input is.
    Chunks(Vec<u8>, Chunk),
}

enum Chunk {
    /// A chunk-size line.
    Size,
    /// A chunk's bytes.
    Data(usize),
    /// The trailer after the last chunk, and its headers so far.
    Trailer(Vec<(String, String)>),
}

/// A line found in the input, as `evbuffer_readln(EVBUFFER_EOL_CRLF)` returns it: without its
/// `\n` and a `\r` before it.
enum Line<'a> {
    /// The line, and how many bytes it took.
    Found(&'a [u8], usize),
    /// No whole line yet, and the limit check failed.
    TooLong,
    /// No whole line yet.
    More,
}

/// The first line in `buf`. `too_long(unread)` is asked while no whole line is there.
fn first_line(buf: &[u8], too_long: impl Fn(usize) -> bool) -> Line<'_> {
    match buf.iter().position(|&b| b == b'\n') {
        Some(at) => {
            let line = &buf[..at];
            Line::Found(line.strip_suffix(b"\r").unwrap_or(line), at + 1)
        }
        None if too_long(buf.len()) => Line::TooLong,
        None => Line::More,
    }
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    /// Adds input read off the connection.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes read but not consumed (a pipelined request).
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// The next request in the input fed so far: `None` while more input is needed, `eof` when
    /// no more will come. `before_body` runs once the headers are read and before the body is,
    /// with whether input is already buffered (for `100 Continue`).
    // libevent: http.c evhttp_read_firstline, evhttp_read_header, evhttp_get_body
    pub fn next(
        &mut self,
        eof: bool,
        mut before_body: impl FnMut(&Request, bool),
    ) -> Option<Result<Request, Failure>> {
        // Input that ends mid-request, or an idle connection that closes, is dropped.
        let more = || eof.then_some(Err(Failure::Closed));
        let bad = |major, minor| {
            Some(Err(Failure::BadRequest {
                major,
                minor,
            }))
        };
        loop {
            match &mut self.state {
                State::Line => {
                    if self.buf.is_empty() {
                        return more();
                    }
                    let (line, used) = match first_line(&self.buf, |n| n > MAX_HEADERS_SIZE) {
                        Line::More => return more(),
                        Line::TooLong => return bad(1, 1),
                        Line::Found(l, _) if l.len() > MAX_HEADERS_SIZE => return bad(1, 1),
                        Line::Found(l, used) => (l, used),
                    };
                    let size = line.len();
                    let Some(req) = parse_request_line(line) else {
                        return bad(1, 1);
                    };
                    self.buf.drain(..used);
                    self.state = State::Headers(req, size);
                }
                State::Headers(req, size) => {
                    match read_headers(&mut self.buf, &mut req.headers, size) {
                        None => return more(),
                        Some(Ok(())) => {}
                        Some(Err(())) => return bad(req.major, req.minor),
                    }
                    let State::Headers(req, headers_size) = std::mem::take(&mut self.state) else {
                        unreachable!("reading headers");
                    };
                    match body_state(req, &self.buf, &mut before_body) {
                        Ok((req, state)) => {
                            self.state = State::Body(Body {
                                req,
                                headers_size,
                                state,
                            });
                        }
                        Err(done) => return Some(done),
                    }
                }
                State::Body(body) => {
                    let done = match &mut body.state {
                        BodyState::Length(n) => {
                            if self.buf.len() < *n {
                                return more();
                            }
                            // The body takes the input's allocation, the rest a new one.
                            let rest = self.buf.split_off(*n);
                            std::mem::replace(&mut self.buf, rest)
                        }
                        BodyState::Chunks(chunks, at) => {
                            match read_chunks(&mut self.buf, chunks, at, &mut body.headers_size) {
                                None => return more(),
                                Some(Ok(())) => std::mem::take(chunks),
                                Some(Err(())) => {
                                    let State::Body(body) = std::mem::take(&mut self.state) else {
                                        unreachable!("reading a body");
                                    };
                                    return Some(Err(Failure::TooLarge(body.req)));
                                }
                            }
                        }
                    };
                    let State::Body(body) = std::mem::take(&mut self.state) else {
                        unreachable!("reading a body");
                    };
                    let mut req = body.req;
                    req.body = done;
                    // A large chunked body leaves a large buffer, which an idle connection would keep.
                    self.buf.shrink_to(BUFFER_KEEP);
                    return Some(Ok(req));
                }
            }
        }
    }
}

/// How the body of `req` is read, or the request itself when it has none (`Err(Ok)`) or is
/// refused (`Err(Err)`). `rest` is the input after the headers.
fn body_state(
    req: Request,
    rest: &[u8],
    before_body: &mut impl FnMut(&Request, bool),
) -> Result<(Request, BodyState), Result<Request, Failure>> {
    if !req.method.allowed() {
        return Err(Err(Failure::NotImplemented(req)));
    }
    if !req.method.may_have_body() {
        return Err(Ok(req));
    }
    let (major, minor) = (req.major, req.minor);
    let chunked =
        req.header("Transfer-Encoding").is_some_and(|t| t.eq_ignore_ascii_case("chunked"));
    let length = if chunked {
        None
    } else {
        match body_length(&req) {
            Err(()) => {
                return Err(Err(Failure::BadRequest {
                    major,
                    minor,
                }));
            }
            // An incoming request with neither a length nor chunks has no body.
            Ok(None) | Ok(Some(0)) => return Err(Ok(req)),
            Ok(Some(n)) => Some(n),
        }
    };
    if (major, minor) >= (1, 1)
        && let Some(expect) = req.header("Expect")
    {
        if !expect.eq_ignore_ascii_case("100-continue") {
            return Err(Err(Failure::ExpectationFailed(req)));
        }
        if length.is_some_and(|n| n > MAX_BODY_SIZE) {
            return Err(Err(Failure::TooLarge(req)));
        }
        before_body(&req, !rest.is_empty());
    }
    match length {
        Some(n) if n > MAX_BODY_SIZE => Err(Err(Failure::TooLarge(req))),
        Some(n) => Ok((req, BodyState::Length(n as usize))),
        None => Ok((req, BodyState::Chunks(Vec::new(), Chunk::Size))),
    }
}

/// Consumes header lines into `headers` up to the empty line, adding their bytes to `size`:
/// `None` while more input is needed, `Some(Err)` for a malformed or oversized block.
// libevent: http.c evhttp_parse_headers_
fn read_headers(
    buf: &mut Vec<u8>,
    headers: &mut Vec<(String, String)>,
    size: &mut usize,
) -> Option<Result<(), ()>> {
    loop {
        let (line, used) = match first_line(buf, |unread| *size + unread > MAX_HEADERS_SIZE) {
            Line::More => return None,
            Line::TooLong => return Some(Err(())),
            Line::Found(l, used) => (l, used),
        };
        *size += line.len();
        if *size > MAX_HEADERS_SIZE {
            return Some(Err(()));
        }
        if line.is_empty() {
            buf.drain(..used);
            return Some(Ok(()));
        }
        let line = c_string(line);
        if line.starts_with(b" ") || line.starts_with(b"\t") {
            // A continuation of the previous header's value.
            let Some((_, value)) = headers.last_mut() else {
                return Some(Err(()));
            };
            let more = rtrim_lws(trim_start(line, b" \t"));
            let mut joined = value.as_bytes().to_vec();
            joined.push(b' ');
            joined.extend_from_slice(more);
            if !header_value_ok(&joined) {
                return Some(Err(()));
            }
            *value = String::from_utf8_lossy(&joined).into_owned();
        } else {
            let Some(colon) = line.iter().position(|&b| b == b':') else {
                return Some(Err(()));
            };
            let value = rtrim_lws(trim_start(&line[colon + 1..], b" "));
            if !header_value_ok(value) {
                return Some(Err(()));
            }
            headers.push((
                String::from_utf8_lossy(&line[..colon]).into_owned(),
                String::from_utf8_lossy(value).into_owned(),
            ));
        }
        buf.drain(..used);
    }
}

/// Consumes chunks into `body` as far as they have arrived: `None` while more input is needed,
/// `Some(Err)` for a malformed chunk. A chunk-size line longer than a header block fails too:
/// libevent buffers one without limit (RP0-D13).
// libevent: http.c evhttp_handle_chunked_read, evhttp_read_trailer
fn read_chunks(
    buf: &mut Vec<u8>,
    body: &mut Vec<u8>,
    at: &mut Chunk,
    headers_size: &mut usize,
) -> Option<Result<(), ()>> {
    loop {
        match at {
            Chunk::Data(n) => {
                let n = *n;
                if buf.len() < n {
                    return None;
                }
                body.extend(buf.drain(..n));
                *at = Chunk::Size;
            }
            Chunk::Trailer(trailer) => return read_headers(buf, trailer, headers_size),
            Chunk::Size => {
                let (line, used) = match first_line(buf, |n| n > MAX_HEADERS_SIZE) {
                    Line::More => return None,
                    Line::TooLong => return Some(Err(())),
                    Line::Found(l, used) => (c_string(l), used),
                };
                if line.is_empty() {
                    buf.drain(..used);
                    continue;
                }
                let (n, rest) = strtoll(line, 16);
                if rest == line || !(rest.is_empty() || rest[0] == b' ') || n < 0 {
                    return Some(Err(()));
                }
                let n = n as u64;
                if body.len() as u64 + n > MAX_BODY_SIZE {
                    return Some(Err(()));
                }
                buf.drain(..used);
                *at = if n == 0 {
                    Chunk::Trailer(Vec::new())
                } else {
                    Chunk::Data(n as usize)
                };
            }
        }
    }
}

/// A blocking reader over [`Parser`], for tests.
#[cfg(test)]
pub struct Reader<R> {
    input: R,
    parser: Parser,
}

#[cfg(test)]
impl<R: std::io::Read> Reader<R> {
    pub fn new(input: R) -> Reader<R> {
        Reader {
            input,
            parser: Parser::new(),
        }
    }

    pub fn buffered(&self) -> usize {
        self.parser.buffered()
    }

    pub fn read_request(
        &mut self,
        mut before_body: impl FnMut(&Request, bool),
    ) -> Result<Request, Failure> {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let eof = match self.input.read(&mut chunk) {
                Ok(0) | Err(_) => true,
                Ok(n) => {
                    self.parser.feed(&chunk[..n]);
                    false
                }
            };
            if let Some(done) = self.parser.next(eof, &mut before_body) {
                return done;
            }
        }
    }
}

/// `evhttp_parse_request_line`: trailing spaces dropped; the method ends at the first space,
/// the version starts after the last one, and the target is what lies between.
// libevent: http.c evhttp_parse_request_line (2.1.12)
fn parse_request_line(line: &[u8]) -> Option<Request> {
    let line = c_string(line);
    let mut len = line.len();
    while len > 0 && line[len - 1] == b' ' {
        len -= 1;
    }
    let line = &line[..len];
    if line.len() < "GET / HTTP/1.0".len() {
        return None;
    }
    let sp = line.iter().position(|&b| b == b' ')?;
    let (method, rest) = (&line[..sp], &line[sp + 1..]);
    let last = rest.iter().rposition(|&b| b == b' ')?;
    if last == 0 {
        return None;
    }
    let (uri, version) = (&rest[..last], &rest[last + 1..]);
    let (major, minor) = parse_http_version(version)?;
    let method = Method::parse(method);
    let proxy = if method == Method::Connect {
        parse_authority(uri)?;
        false
    } else {
        parse_uri_nonconformant(uri)?
    };
    Some(Request {
        method,
        uri: String::from_utf8_lossy(uri).into_owned(),
        major,
        minor,
        headers: Vec::new(),
        body: Vec::new(),
        proxy,
    })
}

/// `evhttp_parse_http_version`: `sscanf("HTTP/%d.%d%c")` must match exactly two numbers, and
/// the major version may be at most 1. Either may be negative (`HTTP/1.-1`), and is kept so.
fn parse_http_version(v: &[u8]) -> Option<(i32, i32)> {
    let rest = v.strip_prefix(b"HTTP/")?;
    let (major, rest) = scanf_d(rest)?;
    let rest = rest.strip_prefix(b".")?;
    let (minor, rest) = scanf_d(rest)?;
    if !rest.is_empty() || major > 1 {
        return None;
    }
    Some((major, minor))
}

/// `sscanf`'s `%d`: optional white space and sign, then digits.
fn scanf_d(s: &[u8]) -> Option<(i32, &[u8])> {
    let s = trim_start(s, b" \t\n\r\x0b\x0c");
    let (n, rest) = strtoll(s, 10);
    (rest.len() < s.len())
        .then_some((n.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32, rest))
}

/// `evhttp_uri_parse_with_flags(EVHTTP_URI_NONCONFORMANT)`: whether the target parses, and
/// whether it is an absolute `http`/`https` URI, which `evhttp` treats as a proxy request.
// libevent: http.c evhttp_uri_parse_with_flags
fn parse_uri_nonconformant(uri: &[u8]) -> Option<bool> {
    let mut rest = uri;
    let mut scheme: Option<&[u8]> = None;
    if let Some(colon) = scan_scheme(rest) {
        scheme = Some(&rest[..colon]);
        rest = &rest[colon + 1..];
    }
    let mut has_authority = false;
    if let Some(after) = rest.strip_prefix(b"//") {
        let end =
            after.iter().position(|&b| matches!(b, b'/' | b'?' | b'#')).unwrap_or(after.len());
        parse_authority(&after[..end])?;
        rest = &after[end..];
        has_authority = true;
    }
    let path_end = rest.iter().position(|&b| b == b'#' || b == b'?').unwrap_or(rest.len());
    let path = &rest[..path_end];
    if has_authority && !path.is_empty() && path[0] != b'/' {
        return None;
    }
    if scheme.is_none() {
        // The first segment of a relative path may not hold a colon.
        let segment = path.split(|&b| b == b'/').next().unwrap_or_default();
        if segment.contains(&b':') {
            return None;
        }
    }
    let is_http =
        scheme.is_some_and(|s| s.eq_ignore_ascii_case(b"http") || s.eq_ignore_ascii_case(b"https"));
    Some(is_http && has_authority)
}

/// `scan_scheme`: the colon ending `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`.
fn scan_scheme(s: &[u8]) -> Option<usize> {
    if !s.first()?.is_ascii_alphabetic() {
        return None;
    }
    let end =
        s.iter().position(|&b| !(b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')))?;
    (s[end] == b':').then_some(end)
}

/// `parse_authority`: `[userinfo@]host[:port]` with a numeric port of at most 65535.
fn parse_authority(a: &[u8]) -> Option<()> {
    let hostport = a.iter().rposition(|&b| b == b'@').map_or(a, |at| &a[at + 1..]);
    let port_colon = if hostport.first() == Some(&b'[') {
        let close = hostport.iter().position(|&b| b == b']')?;
        match &hostport[close + 1..] {
            [] => None,
            [b':', ..] => Some(close + 1),
            _ => return None,
        }
    } else {
        hostport.iter().rposition(|&b| b == b':')
    };
    if let Some(colon) = port_colon {
        let port = &hostport[colon + 1..];
        if !port.iter().all(u8::is_ascii_digit)
            || port
                .iter()
                .try_fold(0u32, |n, &d| {
                    let n = n * 10 + u32::from(d - b'0');
                    (n <= 65535).then_some(n)
                })
                .is_none()
        {
            return None;
        }
    }
    Some(())
}

/// `Content-Length`, as `evhttp_get_body_length` reads it: `Ok(None)` for no body.
fn body_length(req: &Request) -> Result<Option<u64>, ()> {
    let Some(text) = req.header("Content-Length") else {
        return Ok(None);
    };
    let (n, rest) = strtoll(text.as_bytes(), 10);
    if text.is_empty() || !rest.is_empty() || n < 0 {
        return Err(());
    }
    Ok(Some(n as u64))
}

/// `evutil_strtoll`: white space, a sign and digits in `base` (with an `0x` prefix for 16),
/// saturating; returns the value and the unparsed rest (all of `s` when no digit was read).
fn strtoll(s: &[u8], base: u32) -> (i64, &[u8]) {
    let t = trim_start(s, b" \t\n\r\x0b\x0c");
    let (neg, mut t) = match t.first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if base == 16
        && (t.starts_with(b"0x") || t.starts_with(b"0X"))
        && t.get(2).is_some_and(u8::is_ascii_hexdigit)
    {
        t = &t[2..];
    }
    let digits = t.iter().take_while(|b| char::from(**b).is_digit(base)).count();
    if digits == 0 {
        return (0, s);
    }
    let mut n: i64 = 0;
    for &d in &t[..digits] {
        let d = i64::from(char::from(d).to_digit(base).expect("a digit"));
        n = n.saturating_mul(i64::from(base)).saturating_add(d);
    }
    (
        if neg {
            n.saturating_neg()
        } else {
            n
        },
        &t[digits..],
    )
}

/// The bytes before the first NUL: `evhttp` handles lines as C strings.
fn c_string(line: &[u8]) -> &[u8] {
    line.iter().position(|&b| b == 0).map_or(line, |nul| &line[..nul])
}

fn trim_start<'a>(s: &'a [u8], set: &[u8]) -> &'a [u8] {
    let start = s.iter().position(|b| !set.contains(b)).unwrap_or(s.len());
    &s[start..]
}

/// `evutil_rtrim_lws_`.
fn rtrim_lws(s: &[u8]) -> &[u8] {
    let end = s.iter().rposition(|b| !b" \t\r\n".contains(b)).map_or(0, |i| i + 1);
    &s[..end]
}

/// `evhttp_header_is_valid_value`: a CR or LF must start a folded line.
fn header_value_ok(v: &[u8]) -> bool {
    v.iter()
        .enumerate()
        .all(|(i, &b)| !(b == b'\r' || b == b'\n') || matches!(v.get(i + 1), Some(b' ' | b'\t')))
}

#[cfg(test)]
mod tests;
