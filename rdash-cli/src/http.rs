//! The HTTP/1.1 POST dash-cli makes, with libevent's `evhttp` client semantics.
//!
//! Core's dash-cli drives libevent's HTTP client (`CallRPC` in src/bitcoin-cli.cpp). Only what
//! dash-cli can observe is reproduced: the request bytes, and how a reply or a failure maps to
//! an HTTP status (0 on any failure), an `evhttp_request_error` code and a body.
//!
//! The mapping, checked against the stock binary (see `tests/xcli`):
//! - a refused or timed-out connection fails with no error code;
//! - a name that does not resolve, a closed connection and a short body fail with `EOF reached`;
//! - an unparsable status line or header fails with `invalid header`;
//! - no data for the timeout (`-rpcclienttimeout`) fails with `timeout reached`.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// libevent's `EVREQ_HTTP_*` codes.
const EVREQ_HTTP_TIMEOUT: i32 = 0;
const EVREQ_HTTP_EOF: i32 = 1;
const EVREQ_HTTP_INVALID_HEADER: i32 = 2;
const EVREQ_HTTP_BUFFER_ERROR: i32 = 3;
const EVREQ_HTTP_DATA_TOO_LONG: i32 = 5;

/// Core: `HTTPReply`, the structure `request_done` fills in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpReply {
    pub status: i32,
    /// An `EVREQ_HTTP_*` code, or -1 when the error callback did not run.
    pub error: i32,
    pub body: Vec<u8>,
}

impl HttpReply {
    fn failed(error: i32) -> Self {
        HttpReply {
            status: 0,
            error,
            body: Vec::new(),
        }
    }
}

/// What a reply may cost in memory. libevent's client has no bounds here (`max_headers_size` and
/// `max_body_size` are `EV_SIZE_MAX`; a stock `dash-cli` reads a 16 MiB header line, a million
/// headers, any body), so a hostile or impersonated server can use all the memory of this
/// process. These limits are far above what dashd sends (a handful of headers under 1 KiB; the
/// largest replies are `getblock` with verbosity 2 and wallet listings, at most a few hundred
/// MiB) and fail like libevent's own bounds: `EVREQ_HTTP_DATA_TOO_LONG`. They count bytes
/// received, not bytes announced, so a short body behind a huge `Content-Length` is still the
/// EOF it is for stock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// One status, header or chunk-size line, without its line end.
    pub line: usize,
    /// Header lines of the reply, interim replies and trailer together.
    pub header_count: usize,
    /// Status and header lines together, trailer included.
    pub header_bytes: usize,
    /// The body: `Content-Length`, the chunks together, or everything up to the close.
    pub body: usize,
}

impl Limits {
    pub const DEFAULT: Limits = Limits {
        line: 64 * 1024,
        header_count: 1000,
        header_bytes: 256 * 1024,
        body: 1 << 30,
    };
}

/// Core: `http_errorstring`.
pub fn http_error_string(code: i32) -> &'static str {
    match code {
        EVREQ_HTTP_TIMEOUT => "timeout reached",
        EVREQ_HTTP_EOF => "EOF reached",
        EVREQ_HTTP_INVALID_HEADER => "error while reading header, or invalid header",
        EVREQ_HTTP_BUFFER_ERROR => "error encountered while reading or writing",
        4 => "request was canceled",
        EVREQ_HTTP_DATA_TOO_LONG => "response body is larger than allowed",
        _ => "unknown",
    }
}

/// Core: `evhttp_uriencode(str, size, /*space_as_plus=*/false)`: everything but the unreserved
/// characters is percent-encoded, with upper-case hex.
pub fn uri_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len());
    for &b in input {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// One POST request.
pub struct Request<'a> {
    pub host: &'a str,
    pub port: u16,
    pub endpoint: &'a str,
    /// Sent in this order, before the `Content-Length` header the client adds.
    pub headers: &'a [(&'a str, String)],
    pub body: &'a [u8],
    /// `-rpcclienttimeout`, as the idle timeout of the connection.
    pub timeout: Duration,
}

/// Make the request. Never fails: a failure is a reply with status 0.
pub fn post(req: &Request<'_>) -> HttpReply {
    // `bufferevent_socket_connect_hostname` resolves synchronously and connects to the first
    // address only. A name that does not resolve is reported as an EOF.
    let addr = match (req.host, req.port).to_socket_addrs().ok().and_then(|mut a| a.next()) {
        Some(addr) => addr,
        None => return HttpReply::failed(EVREQ_HTTP_EOF),
    };
    let mut stream = match TcpStream::connect_timeout(&addr, req.timeout) {
        Ok(stream) => stream,
        // `evhttp_connection_cb`: a timeout and ECONNREFUSED are cleaned up without an error
        // callback; any other connect error (unreachable network or host) is an EOF.
        Err(e) => {
            return HttpReply::failed(match e.kind() {
                std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::WouldBlock => -1,
                _ => EVREQ_HTTP_EOF,
            });
        }
    };
    let _ = stream.set_read_timeout(Some(req.timeout));
    let _ = stream.set_write_timeout(Some(req.timeout));

    let mut head = format!("POST {} HTTP/1.1\r\n", req.endpoint);
    for (name, value) in req.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", req.body.len()));
    let mut wire = head.into_bytes();
    wire.extend_from_slice(req.body);
    if let Err(e) = stream.write_all(&wire) {
        return HttpReply::failed(io_error_code(&e));
    }

    match read_reply(&mut Reader::new(stream)) {
        Ok(reply) => reply,
        Err(code) => HttpReply::failed(code),
    }
}

fn io_error_code(e: &std::io::Error) -> i32 {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => EVREQ_HTTP_TIMEOUT,
        _ => EVREQ_HTTP_EOF,
    }
}

/// A byte stream that can also be polled without waiting.
trait Source: Read {
    /// Read what is there already: `Some(0)` if the peer has closed, `None` if nothing is.
    fn try_read(&mut self, buf: &mut [u8]) -> std::io::Result<Option<usize>>;
}

impl Source for TcpStream {
    fn try_read(&mut self, buf: &mut [u8]) -> std::io::Result<Option<usize>> {
        self.set_nonblocking(true)?;
        let read = self.read(buf);
        self.set_nonblocking(false)?;
        match read {
            Ok(n) => Ok(Some(n)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
impl Source for &[u8] {
    fn try_read(&mut self, _buf: &mut [u8]) -> std::io::Result<Option<usize>> {
        Ok(None)
    }
}

/// A buffered reader that reports failures as `EVREQ_HTTP_*` codes.
struct Reader<R> {
    inner: R,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    limits: Limits,
}

impl<R: Source> Reader<R> {
    fn new(inner: R) -> Self {
        Self::with_limits(inner, Limits::DEFAULT)
    }

    fn with_limits(inner: R, limits: Limits) -> Self {
        Reader {
            inner,
            buf: Vec::new(),
            pos: 0,
            eof: false,
            limits,
        }
    }

    /// Read more bytes; `Ok(false)` at EOF.
    fn fill(&mut self) -> Result<bool, i32> {
        if self.eof {
            return Ok(false);
        }
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        let mut chunk = [0u8; 8192];
        loop {
            match self.inner.read(&mut chunk) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(false);
                }
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    return Ok(true);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(io_error_code(&e)),
            }
        }
    }

    /// Whether the peer has closed already, found without waiting. Bytes that are there are
    /// kept.
    fn peer_closed(&mut self) -> Result<bool, i32> {
        let mut chunk = [0u8; 8192];
        match self.inner.try_read(&mut chunk) {
            Ok(Some(0)) => {
                self.eof = true;
                Ok(true)
            }
            Ok(Some(n)) => {
                self.buf.extend_from_slice(&chunk[..n]);
                Ok(false)
            }
            Ok(None) => Ok(false),
            Err(e) => Err(io_error_code(&e)),
        }
    }

    /// `evbuffer_readln(EVBUFFER_EOL_CRLF)`: a line ends at LF, with a CR before it dropped. The
    /// result is a C string, so it ends at the first NUL. A line longer than the limit is
    /// `DATA_TOO_LONG`, found as soon as the bytes that are there cannot be a shorter line.
    fn read_line(&mut self) -> Result<Vec<u8>, i32> {
        loop {
            if let Some(i) = self.buf[self.pos..].iter().position(|&b| b == b'\n') {
                let mut line = self.buf[self.pos..self.pos + i].to_vec();
                self.pos += i + 1;
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.len() > self.limits.line {
                    return Err(EVREQ_HTTP_DATA_TOO_LONG);
                }
                if let Some(nul) = line.iter().position(|&b| b == 0) {
                    line.truncate(nul);
                }
                return Ok(line);
            }
            // No line end yet: the CR that may end the line is the one byte more.
            if self.buf.len() - self.pos > self.limits.line.saturating_add(1) {
                return Err(EVREQ_HTTP_DATA_TOO_LONG);
            }
            if !self.fill()? {
                return Err(EVREQ_HTTP_EOF);
            }
        }
    }

    /// `n` more bytes of the body, or an EOF error; `DATA_TOO_LONG` once the body in `out` would
    /// pass the limit.
    fn read_exact_n(&mut self, n: usize, out: &mut Vec<u8>) -> Result<(), i32> {
        let mut left = n;
        while left > 0 {
            if self.pos == self.buf.len() && !self.fill()? {
                return Err(EVREQ_HTTP_EOF);
            }
            let take = left.min(self.buf.len() - self.pos);
            if out.len().saturating_add(take) > self.limits.body {
                return Err(EVREQ_HTTP_DATA_TOO_LONG);
            }
            out.extend_from_slice(&self.buf[self.pos..self.pos + take]);
            self.pos += take;
            left -= take;
        }
        Ok(())
    }

    /// Everything up to EOF, within the body limit.
    fn read_to_end(&mut self, out: &mut Vec<u8>) -> Result<(), i32> {
        loop {
            let rest = &self.buf[self.pos..];
            if out.len().saturating_add(rest.len()) > self.limits.body {
                return Err(EVREQ_HTTP_DATA_TOO_LONG);
            }
            out.extend_from_slice(rest);
            self.pos = self.buf.len();
            if !self.fill()? {
                return Ok(());
            }
        }
    }
}

/// C `isspace` in the C locale.
fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `strtoll(text, &end, base)` for base 10 or 16 (glibc): the value, saturating, and the rest.
/// With no digits nothing is consumed and the value is 0.
fn strtoll(text: &[u8], base: u32) -> (i64, &[u8]) {
    let mut at = text.iter().take_while(|&&b| is_c_space(b)).count();
    let negative = matches!(text.get(at), Some(b'-'));
    if matches!(text.get(at), Some(b'-' | b'+')) {
        at += 1;
    }
    if base == 16
        && matches!(text.get(at..at + 2), Some([b'0', b'x' | b'X']))
        && text.get(at + 2).is_some_and(u8::is_ascii_hexdigit)
    {
        at += 2;
    }
    let digits = text[at..].iter().take_while(|&&b| char::from(b).is_digit(base)).count();
    if digits == 0 {
        return (0, text);
    }
    let mut value: i128 = 0;
    for &b in &text[at..at + digits] {
        let d = i128::from(char::from(b).to_digit(base).unwrap_or(0));
        value = (value * i128::from(base) + d).min(i128::from(u64::MAX));
    }
    let value = if negative {
        -value
    } else {
        value
    };
    let value = value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
    (value, &text[at + digits..])
}

/// glibc `atoi`: `(int)strtol(text, NULL, 10)`.
fn atoi(text: &[u8]) -> i32 {
    strtoll(text, 10).0 as i32
}

/// `sscanf("%d")` on `text`: the value (stored as an `int`) and the rest. Leading white space is
/// skipped; at least one digit is needed.
fn scan_int(text: &[u8]) -> Option<(i32, &[u8])> {
    let skip = text.iter().take_while(|&&b| is_c_space(b)).count();
    let signed = skip + usize::from(matches!(text.get(skip), Some(b'-' | b'+')));
    if !text.get(signed).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let (value, rest) = strtoll(&text[skip..], 10);
    Some((value as i32, rest))
}

/// Core (libevent): `evhttp_parse_http_version`, `sscanf(version, "HTTP/%d.%d%c") == 2 &&
/// major <= 1`: a character after the minor number is read by `%c` and makes it 3.
fn valid_http_version(version: &[u8]) -> bool {
    let Some(rest) = version.strip_prefix(b"HTTP/") else {
        return false;
    };
    let Some((major, rest)) = scan_int(rest) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(b".") else {
        return false;
    };
    let Some((_minor, rest)) = scan_int(rest) else {
        return false;
    };
    rest.is_empty() && major <= 1
}

/// Core (libevent): `evhttp_parse_response_line`; the status code, or `None` if the line is
/// invalid.
fn parse_status_line(line: &[u8]) -> Option<i32> {
    let space = line.iter().position(|&b| b == b' ')?;
    let (protocol, rest) = (&line[..space], &line[space + 1..]);
    let number = rest.split(|&b| b == b' ').next().unwrap_or(rest);
    if !valid_http_version(protocol) {
        return None;
    }
    // A code of 0 is invalid.
    Some(atoi(number)).filter(|&code| code != 0)
}

type Headers = Vec<(Vec<u8>, Vec<u8>)>;

fn is_lws(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

fn trim_end_lws(mut text: &[u8]) -> &[u8] {
    while let [rest @ .., last] = text {
        if !is_lws(*last) {
            break;
        }
        text = rest;
    }
    text
}

/// Core (libevent): `evhttp_header_is_valid_value`: a CR or LF must be followed by a blank
/// (a folded line); at the end of the value it is not.
fn valid_header_value(value: &[u8]) -> bool {
    let is_eol = |b: &u8| *b == b'\r' || *b == b'\n';
    let mut at = 0;
    while let Some(off) = value[at..].iter().position(is_eol) {
        at += off;
        at += value[at..].iter().take_while(|&b| is_eol(b)).count();
        if !value.get(at).is_some_and(|&b| is_lws(b)) {
            return false;
        }
    }
    true
}

/// Core (libevent): `evhttp_parse_headers_`, appending to `headers`. A bad header is
/// `EVREQ_HTTP_INVALID_HEADER`; too many headers or header bytes (`header_bytes` counts the
/// status line too, and is carried from one interim reply to the next) is `DATA_TOO_LONG`.
fn read_headers<R: Source>(
    r: &mut Reader<R>,
    headers: &mut Headers,
    header_bytes: &mut usize,
) -> Result<(), i32> {
    loop {
        let line = r.read_line()?;
        *header_bytes = header_bytes.saturating_add(line.len());
        if *header_bytes > r.limits.header_bytes {
            return Err(EVREQ_HTTP_DATA_TOO_LONG);
        }
        let Some(&first) = line.first() else {
            return Ok(());
        };
        if is_lws(first) {
            // `evhttp_append_to_last_header`: the stripped line is joined with one space.
            let Some(last) = headers.last_mut() else {
                return Err(EVREQ_HTTP_INVALID_HEADER);
            };
            let start = line.iter().take_while(|&&b| is_lws(b)).count();
            last.1.push(b' ');
            last.1.extend_from_slice(trim_end_lws(&line[start..]));
            continue;
        }
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            return Err(EVREQ_HTTP_INVALID_HEADER);
        };
        let key = &line[..colon];
        // `strspn(value, " ")` and `evutil_rtrim_lws_`.
        let value = &line[colon + 1..];
        let value = &value[value.iter().take_while(|&&b| b == b' ').count()..];
        let value = trim_end_lws(value);
        if key.iter().any(|&b| b == b'\r' || b == b'\n') || !valid_header_value(value) {
            return Err(EVREQ_HTTP_INVALID_HEADER);
        }
        headers.push((key.to_vec(), value.to_vec()));
        if headers.len() > r.limits.header_count {
            return Err(EVREQ_HTTP_DATA_TOO_LONG);
        }
    }
}

/// `evhttp_find_header`: the first match, ASCII case-insensitive.
fn header<'a>(headers: &'a Headers, name: &str) -> Option<&'a [u8]> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name.as_bytes())).map(|(_, v)| v.as_slice())
}

fn read_reply<R: Source>(r: &mut Reader<R>) -> Result<HttpReply, i32> {
    // The headers of an interim `100 Continue` stay in the request, and the real reply's are
    // added behind them.
    let mut headers = Headers::new();
    let mut header_bytes = 0;
    let status = loop {
        let line = r.read_line()?;
        header_bytes += line.len();
        let status = parse_status_line(&line).ok_or(EVREQ_HTTP_INVALID_HEADER)?;
        read_headers(r, &mut headers, &mut header_bytes)?;
        if status == 100 {
            // `evhttp_start_read_` defers parsing of bytes that are already buffered. If the
            // peer closed along with them, the close is seen first and fails the request.
            if r.pos < r.buf.len() && r.peer_closed()? {
                return Err(EVREQ_HTTP_EOF);
            }
            continue;
        }
        break status;
    };

    let mut body = Vec::new();
    let no_body = status == 204 || status == 304 || (100..200).contains(&status);
    if no_body {
        // No body, whatever the headers say.
    } else if header(&headers, "Transfer-Encoding")
        .is_some_and(|v| v.eq_ignore_ascii_case(b"chunked"))
    {
        read_chunked(r, &mut body, &mut headers, &mut header_bytes)?;
    } else {
        // `evhttp_get_body_length`.
        match (header(&headers, "Content-Length"), header(&headers, "Connection")) {
            (Some(length), _) => {
                let n = parse_content_length(length).ok_or(EVREQ_HTTP_INVALID_HEADER)?;
                r.read_exact_n(usize::try_from(n).unwrap_or(usize::MAX), &mut body)?;
            }
            // A server that keeps the connection open and gives no length has no body.
            (None, Some(connection)) if !connection.eq_ignore_ascii_case(b"close") => {}
            // No length: the body runs to the end of the connection.
            (None, _) => r.read_to_end(&mut body)?,
        }
    }
    Ok(HttpReply {
        status,
        error: -1,
        body,
    })
}

/// libevent `evhttp_get_body_length`'s Content-Length check: a non-negative number, whole.
fn parse_content_length(text: &[u8]) -> Option<u64> {
    if text.is_empty() {
        return None;
    }
    let (value, rest) = strtoll(text, 10);
    if rest.is_empty() {
        u64::try_from(value).ok()
    } else {
        None
    }
}

/// Core (libevent): `evhttp_handle_chunked_read`. A chunk size that cannot be read is
/// `DATA_CORRUPTED`, which `evhttp_read_body` reports as `EVREQ_HTTP_DATA_TOO_LONG`. A size that
/// overflows saturates (`strtoll`) and then meets the body limit, which counts the chunks
/// together; stock has no limit here (the connection's `max_body_size` is unset).
fn read_chunked<R: Source>(
    r: &mut Reader<R>,
    body: &mut Vec<u8>,
    headers: &mut Headers,
    header_bytes: &mut usize,
) -> Result<(), i32> {
    loop {
        let line = r.read_line()?;
        if line.is_empty() {
            continue;
        }
        let (size, rest) = strtoll(&line, 16);
        // `error = (*endp != '\0' && *endp != ' ') || ntoread < 0`
        if !(rest.is_empty() || rest[0] == b' ') || size < 0 {
            return Err(EVREQ_HTTP_DATA_TOO_LONG);
        }
        if size == 0 {
            // The trailer: header lines, added to the reply's, up to an empty line. A bad one is
            // `DATA_CORRUPTED` too.
            return match read_headers(r, headers, header_bytes) {
                Err(EVREQ_HTTP_INVALID_HEADER) => Err(EVREQ_HTTP_DATA_TOO_LONG),
                other => other,
            };
        }
        // A size beyond `usize` is only reachable on a narrow target; the bytes that arrive
        // are what the body limit counts.
        r.read_exact_n(usize::try_from(size).unwrap_or(usize::MAX), body)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(wire: &[u8]) -> Result<HttpReply, i32> {
        read_reply(&mut Reader::new(wire))
    }

    #[test]
    fn content_length_body() {
        let r = reply(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhiXX").unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"hi"[..]));
    }

    #[test]
    fn short_body_is_eof() {
        assert_eq!(reply(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhi"), Err(EVREQ_HTTP_EOF));
    }

    #[test]
    fn http10_reads_to_close() {
        let r = reply(b"HTTP/1.0 200 OK\r\n\r\nbody").unwrap();
        assert_eq!(r.body, b"body");
    }

    #[test]
    fn chunked_body() {
        let r = reply(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n1\r\n!\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(r.body, b"hi!");
    }

    #[test]
    fn invalid_lines() {
        assert_eq!(reply(b"hello world\r\n\r\n"), Err(EVREQ_HTTP_INVALID_HEADER));
        assert_eq!(reply(b"HTTP/2 200 OK\r\n\r\n"), Err(EVREQ_HTTP_INVALID_HEADER));
        assert_eq!(reply(b"HTTP/1.1 0 OK\r\n\r\n"), Err(EVREQ_HTTP_INVALID_HEADER));
        assert_eq!(reply(b"HTTP/1.1 200 OK\r\nbroken\r\n\r\n"), Err(EVREQ_HTTP_INVALID_HEADER));
        assert_eq!(reply(b""), Err(EVREQ_HTTP_EOF));
    }

    #[test]
    fn libevent_header_and_status_quirks() {
        let ok = |wire: &[u8]| reply(wire).map(|r| (r.status, r.body));
        // keep-alive without a length: no body
        assert_eq!(ok(b"HTTP/1.1 200 OK\r\nConnection: keep-alive\r\n\r\nhi"), Ok((200, vec![])));
        assert_eq!(
            ok(b"HTTP/1.1 200 OK\r\nConnection: Close\r\n\r\nhi"),
            Ok((200, b"hi".to_vec()))
        );
        // only spaces lead a value; a fold joins with one space
        assert_eq!(
            ok(b"HTTP/1.1 200 OK\r\nContent-Length:\t2\r\n\r\nhi"),
            Ok((200, b"hi".to_vec()))
        );
        assert_eq!(
            ok(b"HTTP/1.1 200 OK\r\nTransfer-Encoding:\tchunked\r\n\r\n0\r\n\r\n"),
            Ok((200, b"0\r\n\r\n".to_vec()))
        );
        assert_eq!(
            ok(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n 7\r\n\r\nhi"),
            Err(EVREQ_HTTP_INVALID_HEADER)
        );
        // a bare CR in a value; NUL ends a line
        assert_eq!(ok(b"HTTP/1.1 200 OK\r\nX-A: a\rb\r\n\r\n"), Err(EVREQ_HTTP_INVALID_HEADER));
        assert_eq!(ok(b"HTTP/1.1 200 OK\r\n\0junk\r\nhi"), Ok((200, b"hi".to_vec())));
        // headers of a 100 stay
        assert_eq!(
            ok(b"HTTP/1.1 100 Continue\r\nContent-Length: 5\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi"),
            Err(EVREQ_HTTP_EOF)
        );
        // version, code
        assert_eq!(ok(b"HTTP/1.2 200 OK\r\nContent-Length: 0\r\n\r\n"), Ok((200, vec![])));
        assert_eq!(ok(b"HTTP/2.0 200 OK\r\n\r\n"), Err(EVREQ_HTTP_INVALID_HEADER));
        assert_eq!(ok(b"HTTP/1.1 4294967496 OK\r\nContent-Length: 0\r\n\r\n"), Ok((200, vec![])));
        assert_eq!(ok(b"HTTP/1.1 \xc2\xa0200 OK\r\n\r\n"), Err(EVREQ_HTTP_INVALID_HEADER));
        // a folded first trailer line joins the last header
        assert_eq!(
            ok(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n x\r\n\r\n"),
            Ok((200, vec![]))
        );
    }

    fn small(wire: &[u8], limits: Limits) -> Result<HttpReply, i32> {
        read_reply(&mut Reader::with_limits(wire, limits))
    }

    const TINY: Limits = Limits {
        line: 32,
        header_count: 3,
        header_bytes: 200,
        body: 8,
    };

    #[test]
    fn line_limit() {
        let limits = Limits {
            line: 16,
            ..TINY
        };
        // 16 bytes is the longest line; the CR before the LF is not part of it
        let wire = |n: usize| format!("HTTP/1.1 200 OK\r\nX: {}\r\n\r\n", "a".repeat(n));
        assert!(small(wire(13).as_bytes(), limits).is_ok());
        assert_eq!(small(wire(14).as_bytes(), limits).err(), Some(EVREQ_HTTP_DATA_TOO_LONG));
        // no line end at all stops at the limit instead of buffering on
        assert_eq!(small(&[b'a'; 1000], limits).err(), Some(EVREQ_HTTP_DATA_TOO_LONG));
        // the NUL cut comes after the length check: the whole line counts
        let nul = format!("HTTP/1.1 200 OK\r\n\0{}\r\n\r\n", "a".repeat(20));
        assert_eq!(small(nul.as_bytes(), limits).err(), Some(EVREQ_HTTP_DATA_TOO_LONG));
    }

    /// Delivers its pieces one `read` at a time.
    struct Pieces(std::collections::VecDeque<Vec<u8>>);

    impl Read for Pieces {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let Some(mut piece) = self.0.pop_front() else {
                return Ok(0);
            };
            let n = piece.len().min(buf.len());
            buf[..n].copy_from_slice(&piece[..n]);
            if n < piece.len() {
                self.0.push_front(piece.split_off(n));
            }
            Ok(n)
        }
    }

    impl Source for Pieces {
        fn try_read(&mut self, _buf: &mut [u8]) -> std::io::Result<Option<usize>> {
            Ok(None)
        }
    }

    #[test]
    fn unterminated_line_limit_allows_the_cr_of_a_full_line() {
        let limits = Limits {
            line: 16,
            ..TINY
        };
        let line = |pieces: &[&[u8]]| {
            let mut r =
                Reader::with_limits(Pieces(pieces.iter().map(|p| p.to_vec()).collect()), limits);
            r.read_line()
        };
        // 16 bytes and the CR are buffered, the LF comes in the next read
        assert_eq!(line(&[b"aaaaaaaaaaaaaaaa\r", b"\n"]), Ok(vec![b'a'; 16]));
        // one more byte before the line end cannot be a line of 16
        assert_eq!(line(&[b"aaaaaaaaaaaaaaaaa", b"\n"]), Err(EVREQ_HTTP_DATA_TOO_LONG));
        assert_eq!(line(&[b"aaaaaaaaaaaaaaaa\r\r", b"\n"]), Err(EVREQ_HTTP_DATA_TOO_LONG));
        // 16 bytes, no line end yet: wait for more
        assert_eq!(line(&[b"aaaaaaaaaaaaaaaa", b"\r\n"]), Ok(vec![b'a'; 16]));
    }

    #[test]
    fn header_count_limit() {
        let wire = |n: usize| {
            let mut w = b"HTTP/1.1 200 OK\r\n".to_vec();
            for _ in 0..n {
                w.extend_from_slice(b"A: b\r\n");
            }
            w.extend_from_slice(b"\r\n");
            w
        };
        assert!(small(&wire(3), TINY).is_ok());
        assert_eq!(small(&wire(4), TINY).err(), Some(EVREQ_HTTP_DATA_TOO_LONG));
    }

    #[test]
    fn header_bytes_limit_spans_interim_replies_and_trailer() {
        let limits = Limits {
            header_bytes: 26,
            ..TINY
        };
        // 15 for the status line, 4 for each header
        assert!(small(b"HTTP/1.1 200 OK\r\nA: b\r\nC: d\r\n\r\n", limits).is_ok());
        assert_eq!(
            small(b"HTTP/1.1 200 OK\r\nA: b\r\nC: d\r\nE: f\r\n\r\n", limits).err(),
            Some(EVREQ_HTTP_DATA_TOO_LONG)
        );
        // an interim reply is charged to the same budget: 21 + 4 + 15 > 26
        assert!(small(b"HTTP/1.1 100 Continue\r\nA: b\r\n\r\n", limits).is_err());
        assert_eq!(
            small(b"HTTP/1.1 100 Continue\r\nA: b\r\n\r\nHTTP/1.1 200 OK\r\n\r\n", limits).err(),
            Some(EVREQ_HTTP_DATA_TOO_LONG)
        );
        // and so is the trailer: 15 + 26 + 13
        let chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nT: 0123456789\r\n\r\n";
        let at = |header_bytes| Limits {
            header_bytes,
            ..TINY
        };
        assert!(small(chunked, at(54)).is_ok());
        assert_eq!(small(chunked, at(53)).err(), Some(EVREQ_HTTP_DATA_TOO_LONG));
    }

    #[test]
    fn body_limit_counts_received_bytes() {
        // exactly at the limit, with a length, chunked, and to the close
        assert!(small(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n12345678", TINY).is_ok());
        assert_eq!(
            small(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n123456789", TINY).err(),
            Some(EVREQ_HTTP_DATA_TOO_LONG)
        );
        let chunked = |a: &str, b: &str| {
            format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{a}\r\n0\r\n\r\n{b}")
        };
        assert!(small(chunked("4\r\n1234\r\n4\r\n5678", "").as_bytes(), TINY).is_ok());
        assert_eq!(
            small(chunked("4\r\n1234\r\n5\r\n56789", "").as_bytes(), TINY).err(),
            Some(EVREQ_HTTP_DATA_TOO_LONG)
        );
        assert!(small(b"HTTP/1.0 200 OK\r\n\r\n12345678", TINY).is_ok());
        assert_eq!(
            small(b"HTTP/1.0 200 OK\r\n\r\n123456789", TINY).err(),
            Some(EVREQ_HTTP_DATA_TOO_LONG)
        );
        // a huge announced length behind a short body is the EOF it is for stock
        assert_eq!(
            small(b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999\r\n\r\nhi", TINY).err(),
            Some(EVREQ_HTTP_EOF)
        );
        // a chunk size that overflows saturates and meets the limit, or the EOF
        let huge =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nFFFFFFFFFFFFFFFFFFFF\r\nhi";
        assert_eq!(small(huge, TINY).err(), Some(EVREQ_HTTP_EOF));
        let negative = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n-1\r\nhi";
        assert_eq!(small(negative, TINY).err(), Some(EVREQ_HTTP_DATA_TOO_LONG));
    }

    #[test]
    fn uri_encoding_keeps_only_unreserved() {
        assert_eq!(uri_encode(b"a-b.c_d~E9"), "a-b.c_d~E9");
        assert_eq!(uri_encode(b"w a/b+\xc3\xa9"), "w%20a%2Fb%2B%C3%A9");
    }
}
