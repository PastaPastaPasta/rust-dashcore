//! Writing a reply as libevent 2.1.12's `evhttp_send_reply` and `evhttp_send_error` write it:
//! the status line in the request's version, the handler's headers in order, then `Date`,
//! `Content-Length` and a default `Content-Type` where `evhttp` adds them.

use std::time::{SystemTime, UNIX_EPOCH};

use super::parse::{Method, Request, find_header};

/// The reason phrase `evhttp` sends for `code`.
// libevent: http.c evhttp_response_phrase_internal
pub fn phrase(code: u16) -> &'static str {
    match code {
        100 => "Continue",
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Request Entity Too Large",
        417 => "Expectation Failed",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Unknown Status Code",
    }
}

/// What the request said about the connection, for the reply's headers.
#[derive(Clone, Copy, Debug, Default)]
pub struct Peer {
    pub major: i32,
    pub minor: i32,
    pub method: Option<Method>,
    pub keepalive: bool,
    pub close: bool,
    pub proxy: bool,
}

impl Peer {
    pub fn of(req: &Request) -> Peer {
        Peer {
            major: req.major,
            minor: req.minor,
            method: Some(req.method),
            keepalive: req.wants_keepalive(),
            close: req.wants_close(),
            proxy: req.proxy,
        }
    }
}

/// The bytes of a reply, and whether `evhttp` closes the connection after it
/// (`evhttp_make_header_response`, `evhttp_send_done`).
pub fn render(
    peer: &Peer,
    status: u16,
    mut headers: Vec<(String, String)>,
    body: &[u8],
) -> (Vec<u8>, bool) {
    let needs_body = !matches!(status, 100..=199 | 204 | 304)
        && !matches!(peer.method, Some(Method::Head | Method::Connect));
    let add_missing = |headers: &mut Vec<(String, String)>, name: &str, value: String| {
        if find_header(headers, name).is_none() {
            headers.push((name.to_owned(), value));
        }
    };
    if peer.major == 1 {
        if peer.minor >= 1 {
            add_missing(&mut headers, "Date", http_date(SystemTime::now()));
        }
        if peer.minor == 0 && peer.keepalive {
            headers.push(("Connection".into(), "keep-alive".into()));
        }
        if (peer.minor >= 1 || peer.keepalive)
            && needs_body
            && find_header(&headers, "Transfer-Encoding").is_none()
        {
            add_missing(&mut headers, "Content-Length", body.len().to_string());
        }
    }
    if needs_body {
        add_missing(&mut headers, "Content-Type", "text/html; charset=ISO-8859-1".into());
    }
    if peer.close && !peer.proxy {
        headers.retain(|(k, _)| !k.eq_ignore_ascii_case("Connection"));
        headers.push(("Connection".into(), "close".into()));
    }
    let close = ((peer.major, peer.minor) < (1, 1) && !peer.keepalive)
        || peer.close
        || find_header(&headers, "Connection").is_some_and(|c| c.eq_ignore_ascii_case("close"));
    let mut out = format!("HTTP/{}.{} {} {}\r\n", peer.major, peer.minor, status, phrase(status));
    for (k, v) in &headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let mut out = out.into_bytes();
    // `evhttp` writes the body even where it leaves out its length (HEAD).
    out.extend_from_slice(body);
    (out, close)
}

/// `evhttp_send_error`: an HTML page; the connection closes after it. A request whose major
/// or minor version is 0 is answered as HTTP/1.1 (`evhttp_send_page_`).
pub fn error_page(peer: &Peer, code: u16) -> Vec<u8> {
    let reason = phrase(code);
    let body = format!(
        "<HTML><HEAD>\n<TITLE>{code} {reason}</TITLE>\n</HEAD><BODY>\n<H1>{reason}</H1>\n</BODY></HTML>\n"
    );
    let mut peer = *peer;
    if peer.major == 0 || peer.minor == 0 {
        peer.major = 1;
        peer.minor = 1;
    }
    let headers =
        vec![("Content-Type".into(), "text/html".into()), ("Connection".into(), "close".into())];
    render(&peer, code, headers, body.as_bytes()).0
}

/// `evhttp_maybe_add_date_header`: RFC 1123 in GMT.
pub fn http_date(t: SystemTime) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 {
        mp + 3
    } else {
        mp - 9
    };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DAYS[days.rem_euclid(7) as usize],
        day,
        MONTHS[(month - 1) as usize],
        year,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
