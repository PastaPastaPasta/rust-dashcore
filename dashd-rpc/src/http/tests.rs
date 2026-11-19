use std::net::TcpStream;
use std::sync::Barrier;

use super::*;
use crate::net::resolve_numeric;
use crate::testutil::test_args;

fn server(argv: &[&str]) -> HttpServer {
    let t = test_args(&[&["-rpcport=0"], argv].concat());
    HttpServer::init(&t.args, &resolve_numeric, Arc::new(|| false)).unwrap()
}

/// The IPv4 listener.
fn addr(http: &HttpServer) -> SocketAddr {
    *http.local_addrs().iter().find(|a| a.is_ipv4()).unwrap()
}

/// Sends `request` and reads until the server closes, or is quiet for a second once it has
/// answered (a loaded box may take long to answer at all); `Date` is masked.
fn exchange(addr: SocketAddr, request: &[u8]) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    stream.write_all(request).unwrap();
    let mut out = Vec::new();
    let mut buf = [0; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => {
                out.extend_from_slice(b"<closed>");
                break;
            }
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8(out).unwrap();
    text.split("\r\n")
        .map(|l| {
            if l.starts_with("Date: ") {
                "Date: X"
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn echo_handler(http: &HttpServer) {
    http.register_handler("/echo/", false, |mut req, path| {
        let body = req.read_body();
        let reply =
            format!("{:?} {} {path} {}", req.method(), req.uri(), String::from_utf8_lossy(&body));
        req.write_header("X-Test", "1");
        req.write_reply(HTTP_OK, reply.as_bytes());
    });
}

#[test]
fn requests_reach_their_handler_by_prefix() {
    let mut http = server(&[]);
    echo_handler(&http);
    http.register_handler("/exact", true, |mut req, _| req.write_reply(HTTP_OK, b"exact"));
    http.start();
    let a = addr(&http);
    assert_eq!(
        exchange(a, b"POST /echo/x?y HTTP/1.1\r\nContent-Length: 2\r\n\r\nhi"),
        "HTTP/1.1 200 OK\r\nX-Test: 1\r\nDate: X\r\nContent-Length: 21\r\nContent-Type: text/html; charset=ISO-8859-1\r\n\r\nPost /echo/x?y x?y hi"
    );
    assert!(exchange(a, b"GET /exact HTTP/1.1\r\n\r\n").ends_with("\r\n\r\nexact"));
    assert_eq!(
        exchange(a, b"GET /exact/ HTTP/1.1\r\n\r\n"),
        "HTTP/1.1 404 Not Found\r\nDate: X\r\nContent-Length: 0\r\nContent-Type: text/html; charset=ISO-8859-1\r\n\r\n"
    );
    // Core knows GET, POST, HEAD and PUT; libevent lets DELETE through to Core.
    assert_eq!(
        exchange(a, b"DELETE /echo/ HTTP/1.1\r\n\r\n"),
        "HTTP/1.1 405 Method Not Allowed\r\nDate: X\r\nContent-Length: 0\r\nContent-Type: text/html; charset=ISO-8859-1\r\n\r\n"
    );
    // HTTP/1.0 without keep-alive: no length, the connection closes.
    assert_eq!(
        exchange(a, b"GET /exact HTTP/1.0\r\n\r\n"),
        "HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=ISO-8859-1\r\n\r\nexact<closed>"
    );
    http.interrupt();
    http.stop();
}

#[test]
fn malformed_requests_get_error_pages_and_close() {
    let mut http = server(&[]);
    echo_handler(&http);
    http.start();
    let page = |code: u16| {
        let reason = reply::phrase(code);
        let body = format!(
            "<HTML><HEAD>\n<TITLE>{code} {reason}</TITLE>\n</HEAD><BODY>\n<H1>{reason}</H1>\n</BODY></HTML>\n"
        );
        format!(
            "HTTP/1.1 {code} {reason}\r\nContent-Type: text/html\r\nConnection: close\r\nDate: X\r\nContent-Length: {}\r\n\r\n{body}<closed>",
            body.len()
        )
    };
    let a = addr(&http);
    assert_eq!(exchange(a, b"garbage\r\n\r\n"), page(400));
    assert_eq!(exchange(a, b"OPTIONS / HTTP/1.1\r\n\r\n"), page(501));
    let expect = b"POST / HTTP/1.1\r\nExpect: x\r\nContent-Length: 1\r\n\r\n";
    assert_eq!(exchange(a, expect), page(417));
    let huge = b"POST / HTTP/1.1\r\nContent-Length: 99999999999\r\n\r\n";
    assert_eq!(exchange(a, huge), page(413));
    http.interrupt();
    http.stop();
}

#[test]
fn a_dropped_request_answers_500() {
    let mut http = server(&[]);
    http.register_handler("/", false, |_, _| {});
    http.start();
    assert!(
        exchange(addr(&http), b"GET / HTTP/1.1\r\n\r\n").ends_with("\r\n\r\nUnhandled request")
    );
    http.interrupt();
    http.stop();
}

#[test]
fn a_panicking_handler_leaves_its_worker_running() {
    let mut http = server(&["-rpcthreads=1"]);
    http.register_handler("/panic", true, |_, _| panic!("a handler bug"));
    http.register_handler("/", true, |mut req, _| req.write_reply(HTTP_OK, b"ok"));
    http.start();
    let a = addr(&http);
    assert!(exchange(a, b"GET /panic HTTP/1.1\r\n\r\n").contains("\r\n\r\nUnhandled request"));
    // The only worker still runs requests, and stops when asked.
    assert!(exchange(a, b"GET / HTTP/1.1\r\n\r\n").ends_with("\r\n\r\nok"));
    http.interrupt();
    http.stop();
}

/// `-rpcservertimeout` below -1 closes connections at once; -1 and 0 mean no timeout.
#[test]
fn a_negative_server_timeout_closes_at_once() {
    let mut http = server(&["-rpcservertimeout=-5"]);
    http.register_handler("/", true, |mut req, _| req.write_reply(HTTP_OK, b"ok"));
    http.start();
    // As the probe saw it: an idle connection reads end of stream at once.
    let mut idle = TcpStream::connect(addr(&http)).unwrap();
    idle.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    assert_eq!(idle.read(&mut [0; 1]).unwrap(), 0);
    http.stop();
    for none in ["-1", "0"] {
        let http = server(&[&format!("-rpcservertimeout={none}")]);
        assert_eq!(http.shared.timeout, None);
    }
}

/// `evhttp_set_timeout` takes an `int`, so Core's value is cut to 32 bits; the largest timeout
/// left serves requests rather than overflowing the clock.
#[test]
fn the_server_timeout_is_cut_to_32_bits() {
    for (arg, timeout) in [
        ("4294967301", Some(Duration::from_secs(5))),
        ("4294967295", None),
        ("4294967291", Some(Duration::ZERO)),
        ("9223372036854775807", None),
        ("2147483647", Some(Duration::from_secs(2_147_483_647))),
    ] {
        let http = server(&[&format!("-rpcservertimeout={arg}")]);
        assert_eq!(http.shared.timeout, timeout, "{arg}");
    }
    let mut http = server(&["-rpcservertimeout=2147483647"]);
    echo_handler(&http);
    http.start();
    let reply = exchange(addr(&http), b"GET /echo/x HTTP/1.1\r\nConnection: close\r\n\r\n");
    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    http.stop();
}

/// `stop` waits until a reply in flight is written, however slowly its client reads.
#[test]
fn stop_waits_for_a_reply_to_be_written() {
    const SIZE: usize = 8 << 20;
    let mut http = server(&[]);
    http.register_handler("/", true, |mut req, _| req.write_reply(HTTP_OK, &vec![b'x'; SIZE]));
    http.start();
    let mut stream = TcpStream::connect(addr(&http)).unwrap();
    stream.write_all(b"GET / HTTP/1.1\r\nConnection: close\r\n\r\n").unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    // The reply has started, and fills the socket's buffers while nobody reads.
    let mut first = [0u8; 1];
    stream.read_exact(&mut first).unwrap();
    let stopper = std::thread::spawn(move || http.stop());
    std::thread::sleep(Duration::from_millis(200));
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).unwrap();
    stopper.join().unwrap();
    let text = String::from_utf8_lossy(&rest);
    let body = text.split_once("\r\n\r\n").unwrap().1;
    assert_eq!(body.len(), SIZE);
}

/// `stop` interrupts the queue itself: the workers exit without an `interrupt` first.
#[test]
fn stop_alone_stops() {
    let mut http = server(&[]);
    http.start();
    http.stop();
}

#[test]
fn a_full_queue_answers_503() {
    let mut http = server(&["-rpcthreads=1", "-rpcworkqueue=1"]);
    let (entered, release) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
    let (e, r) = (Arc::clone(&entered), Arc::clone(&release));
    http.register_handler("/block", true, move |mut req, _| {
        e.wait();
        r.wait();
        req.write_reply(HTTP_OK, b"done");
    });
    http.register_handler("/", true, |mut req, _| req.write_reply(HTTP_OK, b"ok"));
    http.start();
    let a = addr(&http);
    // These wait for the worker: they ask the server to close once answered.
    let slow = |path: &str| {
        let request = format!("GET {path} HTTP/1.1\r\nConnection: close\r\n\r\n");
        move || exchange(a, request.as_bytes())
    };
    let blocked = std::thread::spawn(slow("/block"));
    entered.wait();
    // The worker is busy: one request waits in the queue, the next is refused.
    let queued = std::thread::spawn(slow("/"));
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while http.shared.queue.state.lock().unwrap().queue.is_empty() {
        assert!(std::time::Instant::now() < deadline, "the request was never queued");
        std::thread::yield_now();
    }
    let refused = exchange(a, b"GET / HTTP/1.1\r\n\r\n");
    release.wait();
    assert_eq!(
        refused,
        "HTTP/1.1 503 Service Unavailable\r\nDate: X\r\nContent-Length: 25\r\nContent-Type: text/html; charset=ISO-8859-1\r\n\r\nWork queue depth exceeded"
    );
    let blocked = blocked.join().unwrap();
    assert!(blocked.ends_with("done<closed>"), "{blocked}");
    assert!(queued.join().unwrap().ends_with("ok<closed>"));
    http.interrupt();
    http.stop();
}

#[test]
fn external_users_have_their_own_queue() {
    let http = server(&["-rpcexternaluser=ext,other"]);
    assert_eq!(http.external_users(), ["ext", "other"]);
    let req = |auth: &str| HttpRequest {
        method: Method::Post,
        uri: "/".into(),
        headers: vec![("Authorization".into(), auth.into())],
        body: Vec::new(),
        peer: "127.0.0.1:1".parse().unwrap(),
        out_headers: Vec::new(),
        reply: None,
        shutdown_requested: Arc::new(|| false),
    };
    // "ext:pw", "other:", "ext", "nope:pw"
    assert!(is_external(&http.shared, &req("Basic ZXh0OnB3")));
    assert!(is_external(&http.shared, &req("Basic  b3RoZXI6 ")));
    assert!(!is_external(&http.shared, &req("Basic ZXh0")));
    assert!(!is_external(&http.shared, &req("Basic bm9wZTpwdw==")));
    assert!(!is_external(&http.shared, &req("Bearer ZXh0OnB3")));
}

#[test]
fn clients_outside_the_allow_list_are_refused() {
    let http = server(&["-rpcallowip=10.0.0.0/8"]);
    http.register_handler("/", true, |mut req, _| req.write_reply(HTTP_OK, b"ok"));
    let request = || Request {
        method: Method::Get,
        uri: "/".into(),
        major: 1,
        minor: 1,
        headers: Vec::new(),
        body: Vec::new(),
        proxy: false,
    };
    let replies = &http.events.as_ref().unwrap().replies;
    let status = |peer: &str| {
        dispatch(&http.shared, request(), peer.parse().unwrap(), http.shared.reply_to(7));
        let (token, Reply(status, _, _)) = replies.recv().unwrap();
        assert_eq!(token, 7);
        status
    };
    // An allowed request is queued, which an interrupted queue refuses.
    http.shared.queue.interrupt();
    assert_eq!(status("10.1.2.3:5"), HTTP_SERVICE_UNAVAILABLE);
    // The loopback networks are always allowed.
    assert_eq!(status("127.9.9.9:5"), HTTP_SERVICE_UNAVAILABLE);
    assert_eq!(status("[::1]:5"), HTTP_SERVICE_UNAVAILABLE);
    assert_eq!(status("[::ffff:10.0.0.1]:5"), HTTP_SERVICE_UNAVAILABLE);
    assert_eq!(status("11.0.0.1:5"), HTTP_FORBIDDEN);
    assert_eq!(status("[::2]:5"), HTTP_FORBIDDEN);
}

#[test]
fn a_bad_allowip_fails_init_with_core_s_message() {
    let t = test_args(&["-rpcport=0", "-rpcallowip=10.0.0.0/33"]);
    let err = HttpServer::init(&t.args, &resolve_numeric, Arc::new(|| false)).err().unwrap();
    assert_eq!(err, InitError::AllowIp(
        "Invalid -rpcallowip subnet specification: 10.0.0.0/33. Valid are a single IP (e.g. 1.2.3.4), a network/netmask (e.g. 1.2.3.4/255.255.255.0) or a network/CIDR (e.g. 1.2.3.4/24).".into()
    ));
}

#[test]
fn binding_follows_rpcbind_only_with_rpcallowip() {
    let port = |http: &HttpServer| {
        http.local_addrs().iter().map(|a| a.ip().to_string()).collect::<Vec<_>>()
    };
    assert_eq!(port(&server(&[])), ["::1", "127.0.0.1"]);
    assert_eq!(port(&server(&["-rpcbind=127.0.0.2"])), ["::1", "127.0.0.1"]);
    assert_eq!(
        port(&server(&["-rpcbind=127.0.0.2", "-rpcbind=[::1]", "-rpcallowip=127.0.0.1"])),
        ["127.0.0.2", "::1"]
    );
    let t = test_args(&["-rpcport=0", "-rpcbind=192.0.2.1", "-rpcallowip=127.0.0.1"]);
    assert_eq!(
        HttpServer::init(&t.args, &resolve_numeric, Arc::new(|| false)).err(),
        Some(InitError::Bind)
    );
}

#[test]
fn interrupted_servers_refuse_and_stop_after_requests_in_flight() {
    let mut http = server(&[]);
    let (entered, release) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
    let (e, r) = (Arc::clone(&entered), Arc::clone(&release));
    http.register_handler("/slow", true, move |mut req, _| {
        e.wait();
        r.wait();
        req.write_reply(HTTP_OK, b"late");
    });
    http.start();
    let a = addr(&http);
    // An idle connection, open before the interrupt.
    let mut idle = TcpStream::connect(a).unwrap();
    let slow = std::thread::spawn(move || exchange(a, b"GET /slow HTTP/1.1\r\n\r\n"));
    entered.wait();
    http.interrupt();
    let refused = exchange(a, b"GET /slow HTTP/1.1\r\n\r\n");
    assert!(
        refused.starts_with(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/html\r\nConnection: close\r\n"
        ),
        "{refused}"
    );
    let stopper = std::thread::spawn(move || http.stop());
    release.wait();
    // The request in flight is answered before the server stops.
    assert!(slow.join().unwrap().contains("\r\n\r\nlate"));
    stopper.join().unwrap();
    // Stopping closed the idle connection and the listeners.
    idle.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    assert_eq!(idle.read(&mut [0; 1]).unwrap(), 0);
    assert!(TcpStream::connect(a).is_err());
}

#[test]
fn replies_close_while_shutting_down() {
    let t = test_args(&["-rpcport=0"]);
    let mut http = HttpServer::init(&t.args, &resolve_numeric, Arc::new(|| true)).unwrap();
    http.register_handler("/", true, |mut req, _| req.write_reply(HTTP_OK, b"bye"));
    http.start();
    assert_eq!(
        exchange(addr(&http), b"GET / HTTP/1.1\r\n\r\n"),
        "HTTP/1.1 200 OK\r\nConnection: close\r\nDate: X\r\nContent-Length: 3\r\nContent-Type: text/html; charset=ISO-8859-1\r\n\r\nbye<closed>"
    );
    http.interrupt();
    http.stop();
}

/// The idle timeout is paused while a worker answers: a call longer than `-rpcservertimeout`
/// still gets its reply (from libevent 2.1.9, `evhttp` stops reading while a request is
/// handled), while an idle connection beside it is closed.
#[test]
fn the_idle_timeout_waits_for_a_running_call() {
    let mut http = server(&["-rpcservertimeout=1"]);
    http.register_handler("/slow", true, |mut req, _| {
        std::thread::sleep(Duration::from_secs(3));
        req.write_reply(HTTP_OK, b"slow");
    });
    http.start();
    let a = addr(&http);
    let mut idle = TcpStream::connect(a).unwrap();
    idle.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let reply = exchange(a, b"GET /slow HTTP/1.1\r\nConnection: close\r\n\r\n");
    assert!(reply.ends_with("\r\n\r\nslow<closed>"), "{reply}");
    assert_eq!(idle.read(&mut [0; 1]).unwrap(), 0);
    http.interrupt();
    http.stop();
}

/// A worker takes the normal queue's requests before the external users' (Dash's second queue
/// runs only when no other request waits).
#[test]
fn normal_requests_run_before_external_ones() {
    let mut http = server(&["-rpcthreads=1", "-rpcexternaluser=ext"]);
    let (started, release) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
    let order = Arc::new(Mutex::new(Vec::new()));
    {
        let (started, release, order) =
            (Arc::clone(&started), Arc::clone(&release), Arc::clone(&order));
        http.register_handler("/", false, move |mut req, path| {
            if path == "block" {
                started.wait();
                release.wait();
            }
            order.lock().unwrap().push(path.to_owned());
            req.write_reply(HTTP_OK, b"");
        });
    }
    http.start();
    let a = addr(&http);
    let send = |path: &'static str, auth: &'static str| {
        std::thread::spawn(move || {
            let request = format!("GET /{path} HTTP/1.1\r\n{auth}Connection: close\r\n\r\n");
            exchange(a, request.as_bytes())
        })
    };
    let queued = |external: bool| loop {
        let state = http.shared.queue.state.lock().unwrap();
        if !(if external {
            &state.external
        } else {
            &state.queue
        })
        .is_empty()
        {
            break;
        }
        drop(state);
        std::thread::sleep(Duration::from_millis(10));
    };
    // The only worker is busy; an external request ("ext:pw") queues first, then a normal one.
    let blocker = send("block", "");
    started.wait();
    let external = send("external", "Authorization: Basic ZXh0OnB3\r\n");
    queued(true);
    let normal = send("normal", "");
    queued(false);
    release.wait();
    for client in [blocker, external, normal] {
        assert!(client.join().unwrap().starts_with("HTTP/1.1 200 OK"));
    }
    assert_eq!(*order.lock().unwrap(), ["block", "normal", "external"]);
    http.interrupt();
    http.stop();
}
