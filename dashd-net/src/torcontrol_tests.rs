use std::sync::Mutex;

use dash_p2p::NetAddr;

use super::*;

fn split(s: &str) -> (String, String) {
    let (a, b) = split_tor_reply_line(s.as_bytes());
    (text(a), text(b))
}

/// Core's `util_SplitTorReplyLine` vectors.
// Core: test/torcontrol_tests.cpp:21-58
#[test]
fn split_tor_reply_line_is_cores() {
    for (input, command, args) in [
        ("PROTOCOLINFO PIVERSION", "PROTOCOLINFO", "PIVERSION"),
        (
            "AUTH METHODS=COOKIE,SAFECOOKIE COOKIEFILE=\"/home/x/.tor/control_auth_cookie\"",
            "AUTH",
            "METHODS=COOKIE,SAFECOOKIE COOKIEFILE=\"/home/x/.tor/control_auth_cookie\"",
        ),
        ("AUTH METHODS=NULL", "AUTH", "METHODS=NULL"),
        ("AUTH METHODS=HASHEDPASSWORD", "AUTH", "METHODS=HASHEDPASSWORD"),
        (
            "VERSION Tor=\"0.2.9.8 (git-a0df013ea241b026)\"",
            "VERSION",
            "Tor=\"0.2.9.8 (git-a0df013ea241b026)\"",
        ),
        (
            "AUTHCHALLENGE SERVERHASH=aaaa SERVERNONCE=bbbb",
            "AUTHCHALLENGE",
            "SERVERHASH=aaaa SERVERNONCE=bbbb",
        ),
        ("COMMAND", "COMMAND", ""),
        ("COMMAND SOME  ARGS", "COMMAND", "SOME  ARGS"),
        ("COMMAND  ARGS", "COMMAND", " ARGS"),
        ("COMMAND   EVEN+more  ARGS", "COMMAND", "  EVEN+more  ARGS"),
    ] {
        assert_eq!(split(input), (command.to_owned(), args.to_owned()), "{input}");
    }
}

fn mapping(s: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    parse_tor_reply_mapping(s).into_iter().collect()
}

fn pairs(p: &[(&str, &[u8])]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut v: Vec<_> = p.iter().map(|(k, v)| (k.as_bytes().to_vec(), v.to_vec())).collect();
    v.sort();
    v
}

/// A mapping's expected pairs.
type Pairs<'a> = &'a [(&'a str, &'a [u8])];

/// Core's `util_ParseTorReplyMapping` vectors.
// Core: test/torcontrol_tests.cpp:60-200
#[test]
fn parse_tor_reply_mapping_is_cores() {
    let cases: &[(&[u8], Pairs)] = &[
        (
            b"METHODS=COOKIE,SAFECOOKIE COOKIEFILE=\"/home/x/.tor/control_auth_cookie\"",
            &[("METHODS", b"COOKIE,SAFECOOKIE"), ("COOKIEFILE", b"/home/x/.tor/control_auth_cookie")],
        ),
        (b"METHODS=NULL", &[("METHODS", b"NULL")]),
        (b"METHODS=HASHEDPASSWORD", &[("METHODS", b"HASHEDPASSWORD")]),
        (b"Tor=\"0.2.9.8 (git-a0df013ea241b026)\"", &[("Tor", b"0.2.9.8 (git-a0df013ea241b026)")]),
        (b"SERVERHASH=aaaa SERVERNONCE=bbbb", &[("SERVERHASH", b"aaaa"), ("SERVERNONCE", b"bbbb")]),
        (b"ServiceID=exampleonion1234", &[("ServiceID", b"exampleonion1234")]),
        (b"PrivateKey=RSA1024:BLOB", &[("PrivateKey", b"RSA1024:BLOB")]),
        (b"ClientAuth=bob:BLOB", &[("ClientAuth", b"bob:BLOB")]),
        (b"Foo=Bar=Baz Spam=Eggs", &[("Foo", b"Bar=Baz"), ("Spam", b"Eggs")]),
        (b"Foo=\"Bar=Baz\"", &[("Foo", b"Bar=Baz")]),
        (b"Foo=\"Bar Baz\"", &[("Foo", b"Bar Baz")]),
        (b"Foo=\"Bar\\ Baz\"", &[("Foo", b"Bar Baz")]),
        (b"Foo=\"Bar\\Baz\"", &[("Foo", b"BarBaz")]),
        (b"Foo=\"Bar\\@Baz\"", &[("Foo", b"Bar@Baz")]),
        (b"Foo=\"Bar\\\"Baz\" Spam=\"\\\"Eggs\\\"\"", &[("Foo", b"Bar\"Baz"), ("Spam", b"\"Eggs\"")]),
        (b"Foo=\"Bar\\\\Baz\"", &[("Foo", b"Bar\\Baz")]),
        (
            b"Foo=\"Bar\\nBaz\\t\" Spam=\"\\rEggs\" Octals=\"\\1a\\11\\17\\18\\81\\377\\378\\400\\2222\" Final=Check",
            &[
                ("Foo", b"Bar\nBaz\t"),
                ("Spam", b"\rEggs"),
                ("Octals", b"\x01a\x09\x0f\x01881\xff\x1f8\x200\x922"),
                ("Final", b"Check"),
            ],
        ),
        (b"Valid=Mapping Escaped=\"Escape\\\\\"", &[("Valid", b"Mapping"), ("Escaped", b"Escape\\")]),
        (b"Valid=Mapping Bare=\"Escape\\\"", &[]),
        (
            b"OneOctal=\"OneEnd\\1\" TwoOctal=\"TwoEnd\\11\"",
            &[("OneOctal", b"OneEnd\x01"), ("TwoOctal", b"TwoEnd\x09")],
        ),
        (b"Null=\"\\0\"", &[("Null", b"\0")]),
        (b"SOME=args,here MORE optional=arguments  here", &[("SOME", b"args,here")]),
        (b"ARGS", &[]),
        (b"MORE ARGS", &[]),
        (b"MORE  ARGS", &[]),
        (b"EVEN more=ARGS", &[]),
        (b"EVEN+more ARGS", &[]),
    ];
    for (input, expected) in cases {
        assert_eq!(mapping(input), pairs(expected), "{}", text(input));
    }
}

/// `ParseHex`: pairs with spaces between them; anything else is empty.
// Core: util/strencodings.cpp:81-97
#[test]
fn parse_hex_is_cores() {
    assert_eq!(parse_hex(b"00ff 1A\t2b"), [0, 0xff, 0x1a, 0x2b]);
    assert_eq!(parse_hex(b""), [] as [u8; 0]);
    assert!(parse_hex(b"abc").is_empty());
    assert!(parse_hex(b"0 0").is_empty());
    assert!(parse_hex(b"zz").is_empty());
    // `IsSpace` includes the vertical tab and form feed.
    assert_eq!(parse_hex(b"00\x0bff\x0c01"), [0, 0xff, 1]);
}

/// The node, as the controller sees it: every call recorded.
#[derive(Default)]
struct FakeHost {
    calls: Mutex<Vec<String>>,
    key: Mutex<Option<Vec<u8>>>,
    cookie: Option<Vec<u8>>,
    onlynet_allows: bool,
    write_fails: bool,
}

impl FakeHost {
    fn calls(&self) -> Vec<String> {
        std::mem::take(&mut self.calls.lock().unwrap())
    }

    fn record(&self, s: String) {
        self.calls.lock().unwrap().push(s);
    }
}

impl TorHost for FakeHost {
    fn set_onion_proxy(&self, proxy: Proxy, reachable: bool) {
        self.record(format!(
            "proxy {proxy} randomize={} reachable={reachable}",
            proxy.randomize_credentials
        ));
    }

    fn onion_allowed_by_onlynet(&self) -> bool {
        self.onlynet_allows
    }

    fn add_local(&self, service: Service) {
        self.record(format!("add_local {service}"));
    }

    fn remove_local(&self, service: &Service) {
        self.record(format!("remove_local {service}"));
    }

    fn lookup_numeric(&self, name: &str, port: u16) -> Service {
        lookup_numeric(name, port)
    }

    fn read_private_key(&self) -> Option<Vec<u8>> {
        self.key.lock().unwrap().clone()
    }

    fn write_private_key(&self, key: &[u8]) -> bool {
        self.record(format!("write_key {}", text(key)));
        *self.key.lock().unwrap() = Some(key.to_vec());
        !self.write_fails
    }

    fn private_key_path(&self) -> String {
        "/dd/regtest/onion_v3_private_key".into()
    }

    fn read_cookie(&self, path: &[u8], max: usize) -> Option<Vec<u8>> {
        self.record(format!("read_cookie {} {max}", text(path)));
        self.cookie.as_ref().map(|c| c[..c.len().min(max)].to_vec())
    }

    fn client_nonce(&self) -> [u8; TOR_NONCE_SIZE] {
        [0x11; TOR_NONCE_SIZE]
    }
}

fn opts(password: &str, ask_socks_port: bool) -> TorOptions {
    TorOptions {
        control: DEFAULT_TOR_CONTROL.into(),
        password: password.into(),
        ask_socks_port,
        default_port: 19899,
        target: Service::new(NetAddr::ipv4([127, 0, 0, 1]), 19896),
    }
}

/// Feeds `reply` (lines with their `\r\n`) and returns what the controller sends.
fn feed(ctl: &mut TorController, reply: &str) -> String {
    let mut out = Vec::new();
    for line in reply.split("\r\n").filter(|l| !l.is_empty()) {
        out.extend(ctl.line(line.as_bytes()));
    }
    text(&out)
}

const ONION_ID: &str = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd";

/// Connects and passes NULL authentication, leaving the `GETINFO` and `ADD_ONION` replies.
fn authenticated(ctl: &mut TorController) {
    ctl.connected();
    feed(ctl, "250-AUTH METHODS=NULL\r\n250 OK\r\n");
    feed(ctl, "250 OK\r\n");
}

/// The whole NULL-authentication session: `PROTOCOLINFO 1`, `AUTHENTICATE`, then `GETINFO
/// net/listeners/socks` and `ADD_ONION NEW:ED25519-V3` with the default port and the target;
/// the onion proxy, the service as a local address, and the key file.
// Core: torcontrol.cpp:347-471, :535-617
#[test]
fn a_null_authenticated_session() {
    let host = Arc::new(FakeHost {
        onlynet_allows: true,
        ..FakeHost::default()
    });
    let mut ctl = TorController::new(opts("", true), host.clone());
    assert_eq!(text(&ctl.connected()), "PROTOCOLINFO 1\r\n");
    let sent = feed(
        &mut ctl,
        "250-PROTOCOLINFO 1\r\n250-AUTH METHODS=NULL\r\n250-VERSION Tor=\"0.4.8.9\"\r\n250 OK\r\n",
    );
    assert_eq!(sent, "AUTHENTICATE\r\n");
    let sent = feed(&mut ctl, "250 OK\r\n");
    assert_eq!(
        sent,
        "GETINFO net/listeners/socks\r\nADD_ONION NEW:ED25519-V3 Port=19899,127.0.0.1:19896\r\n"
    );
    feed(
        &mut ctl,
        "250-net/listeners/socks=\"0.0.0.0:9150\" \"127.0.0.1:9250\" \"[::1]:9350\"\r\n250 OK\r\n",
    );
    assert_eq!(host.calls(), ["proxy 127.0.0.1:9250 randomize=true reachable=true"]);
    feed(
        &mut ctl,
        &format!("250-ServiceID={ONION_ID}\r\n250-PrivateKey=ED25519-V3:KEY\r\n250 OK\r\n"),
    );
    assert_eq!(
        host.calls(),
        ["write_key ED25519-V3:KEY".to_owned(), format!("add_local {ONION_ID}.onion:19899")]
    );
    assert_eq!(ctl.service().to_string(), format!("{ONION_ID}.onion:19899"));
    // A disconnection stops advertising it.
    assert_eq!(ctl.disconnected(), Duration::from_secs(1));
    assert_eq!(host.calls(), [format!("remove_local {ONION_ID}.onion:19899")]);
    assert!(!ctl.service().addr.is_valid());
}

/// A cached key is sent as it is; with `-onion` set, no `GETINFO`.
// Core: torcontrol.cpp:328-333, :456-467
#[test]
fn a_cached_key_and_no_socks_query() {
    let host = Arc::new(FakeHost {
        key: Mutex::new(Some(b"ED25519-V3:CACHED".to_vec())),
        ..FakeHost::default()
    });
    let mut ctl = TorController::new(opts("", false), host);
    ctl.connected();
    feed(&mut ctl, "250-AUTH METHODS=NULL\r\n250 OK\r\n");
    assert_eq!(
        feed(&mut ctl, "250 OK\r\n"),
        "ADD_ONION ED25519-V3:CACHED Port=19899,127.0.0.1:19896\r\n"
    );
}

/// `-torpassword` wins over every other method, its quotes escaped; without `HASHEDPASSWORD` it
/// is not sent at all.
// Core: torcontrol.cpp:572-580
#[test]
fn hashed_password_authentication() {
    let host = Arc::new(FakeHost::default());
    let mut ctl = TorController::new(opts("pa\"ss word", true), host.clone());
    ctl.connected();
    let sent = feed(&mut ctl, "250-AUTH METHODS=NULL,HASHEDPASSWORD\r\n250 OK\r\n");
    assert_eq!(sent, "AUTHENTICATE \"pa\\\"ss word\"\r\n");
    let mut ctl = TorController::new(opts("x", true), host);
    ctl.connected();
    assert_eq!(feed(&mut ctl, "250-AUTH METHODS=NULL,SAFECOOKIE\r\n250 OK\r\n"), "");
}

/// SAFECOOKIE: `AUTHCHALLENGE SAFECOOKIE <client nonce>`, the server's hash checked, then
/// `AUTHENTICATE <client hash>`. The hashes are Tor's HMAC-SHA256s, computed here as the
/// mock server computes them.
// Core: torcontrol.cpp:489-533, :584-600
#[test]
fn safecookie_authentication() {
    let cookie = vec![0xc0; 32];
    let host = Arc::new(FakeHost {
        cookie: Some(cookie.clone()),
        ..FakeHost::default()
    });
    let mut ctl = TorController::new(opts("", true), host.clone());
    ctl.connected();
    let sent = feed(
        &mut ctl,
        "250-AUTH METHODS=COOKIE,SAFECOOKIE COOKIEFILE=\"/run/tor/control.authcookie\"\r\n250 OK\r\n",
    );
    assert_eq!(sent, format!("AUTHCHALLENGE SAFECOOKIE {}\r\n", "11".repeat(32)));
    assert_eq!(host.calls(), ["read_cookie /run/tor/control.authcookie 32"]);
    let server_nonce = [0x22; 32];
    let server_hash = compute_response(TOR_SAFE_SERVERKEY, &cookie, &[0x11; 32], &server_nonce);
    let client_hash = compute_response(TOR_SAFE_CLIENTKEY, &cookie, &[0x11; 32], &server_nonce);
    let reply = format!(
        "250 AUTHCHALLENGE SERVERHASH={} SERVERNONCE={}\r\n",
        hex(&server_hash).to_uppercase(),
        hex(&server_nonce)
    );
    assert_eq!(feed(&mut ctl, &reply), format!("AUTHENTICATE {}\r\n", hex(&client_hash)));
    // A wrong server hash, a short nonce or another reply stop there.
    for reply in [
        format!(
            "250 AUTHCHALLENGE SERVERHASH={} SERVERNONCE={}\r\n",
            "00".repeat(32),
            hex(&server_nonce)
        ),
        // A 31-byte nonce with the hash that matches it: refused for its length alone.
        format!(
            "250 AUTHCHALLENGE SERVERHASH={} SERVERNONCE={}\r\n",
            hex(&compute_response(TOR_SAFE_SERVERKEY, &cookie, &[0x11; 32], &[0x22; 31])),
            "22".repeat(31)
        ),
        "250 OTHER SERVERHASH=00\r\n".into(),
        "250 AUTHCHALLENGE SERVERHASH=\"00\r\n".into(),
        "515 Authentication failed\r\n".into(),
    ] {
        let mut ctl = TorController::new(opts("", true), host.clone());
        ctl.connected();
        feed(&mut ctl, "250-AUTH METHODS=SAFECOOKIE COOKIEFILE=\"/c\"\r\n250 OK\r\n");
        assert_eq!(feed(&mut ctl, &reply), "", "{reply}");
    }
}

/// The cookie is read up to 32 bytes, so a longer file is cut to 32 and used; a shorter one or
/// none is refused.
// Core: torcontrol.cpp:587-599, util/readwritefile.cpp:16-35
#[test]
fn the_cookie_is_read_up_to_32_bytes() {
    for (cookie, challenged) in [
        (Some(vec![1; 33]), true),
        (Some(vec![1; 32]), true),
        (Some(vec![1; 31]), false),
        (None, false),
    ] {
        let host = Arc::new(FakeHost {
            cookie: cookie.clone(),
            ..FakeHost::default()
        });
        let mut ctl = TorController::new(opts("", true), host);
        ctl.connected();
        let sent = feed(&mut ctl, "250-AUTH METHODS=SAFECOOKIE COOKIEFILE=\"/c\"\r\n250 OK\r\n");
        assert_eq!(sent.starts_with("AUTHCHALLENGE"), challenged, "{cookie:?}");
    }
}

/// No usable method, a failed `PROTOCOLINFO` or a failed `AUTHENTICATE` send nothing more.
// Core: torcontrol.cpp:449-471, :601-608
#[test]
fn nothing_more_without_authentication() {
    for (protocolinfo, auth) in [
        ("250-AUTH METHODS=HASHEDPASSWORD\r\n250 OK\r\n", None),
        ("250-AUTH METHODS=COOKIE\r\n250 OK\r\n", None),
        ("250-AUTH\r\n250 OK\r\n", None),
        ("552 Unrecognized\r\n", None),
        ("250-AUTH METHODS=NULL\r\n250 OK\r\n", Some("515 Bad authentication\r\n")),
    ] {
        let host = Arc::new(FakeHost::default());
        let mut ctl = TorController::new(opts("", true), host.clone());
        ctl.connected();
        let sent = feed(&mut ctl, protocolinfo);
        match auth {
            None => assert_eq!(sent, "", "{protocolinfo}"),
            Some(reply) => assert_eq!(feed(&mut ctl, reply), "", "{reply}"),
        }
        assert!(host.calls().is_empty());
    }
}

/// `GETINFO net/listeners/socks`: the last port, or the first `127.0.0.1:` one; quotes of
/// either kind stripped; 127.0.0.1:9050 when nothing usable comes back. The proxy always
/// randomizes credentials, and Tor is reachable unless `-onlynet` excludes it.
// Core: torcontrol.cpp:347-412
#[test]
fn the_onion_proxy_from_getinfo() {
    for (reply, proxy, onlynet_allows) in [
        (
            "250-net/listeners/socks=\"[::1]:9150\" '10.0.0.1:9250'\r\n250 OK\r\n",
            "10.0.0.1:9250",
            true,
        ),
        (
            "250-net/listeners/socks=127.0.0.1:9150 127.0.0.1:9250\r\n250 OK\r\n",
            "127.0.0.1:9150",
            true,
        ),
        ("250-net/listeners/socks=\r\n250 OK\r\n", "127.0.0.1:9050", true),
        ("250-net/listeners/socks=\"\" unix:/run/tor/socks\r\n250 OK\r\n", "127.0.0.1:9050", false),
        ("250-net/listeners/socks=\"[::1]\"\r\n250 OK\r\n", "[::1]:9050", true),
        // A lone quote is no quoted token (`size() >= 2`): kept as the location, which does not
        // parse, so the fallback; a later port replaces it.
        ("250-net/listeners/socks=\"\r\n250 OK\r\n", "127.0.0.1:9050", true),
        ("250-net/listeners/socks=\" 127.0.0.1:9250\r\n250 OK\r\n", "127.0.0.1:9250", true),
        ("510 Unrecognized command\r\n", "127.0.0.1:9050", true),
        ("551 Internal error\r\n", "127.0.0.1:9050", false),
    ] {
        let host = Arc::new(FakeHost {
            onlynet_allows,
            ..FakeHost::default()
        });
        let mut ctl = TorController::new(opts("", true), host.clone());
        authenticated(&mut ctl);
        feed(&mut ctl, reply);
        assert_eq!(
            host.calls(),
            [format!("proxy {proxy} randomize=true reachable={onlynet_allows}")],
            "{reply}"
        );
    }
}

/// `ADD_ONION`'s failures add nothing; a reply without `ServiceID` neither. A key Tor does not
/// return (one we sent) is written again as it was; a failed write still advertises.
// Core: torcontrol.cpp:414-447
#[test]
fn add_onion_replies() {
    for (reply, calls) in [
        ("510 Unrecognized command\r\n", vec![]),
        ("512 Bad arguments\r\n", vec![]),
        ("250-PrivateKey=ED25519-V3:K\r\n250 OK\r\n", vec![]),
        (
            "250-ServiceID=notbase32!\r\n250 OK\r\n",
            vec!["write_key ED25519-V3:CACHED".to_owned(), "add_local [::]:0".into()],
        ),
        (
            &format!("250-ServiceID={ONION_ID}\r\n250 OK\r\n"),
            vec![
                "write_key ED25519-V3:CACHED".to_owned(),
                format!("add_local {ONION_ID}.onion:19899"),
            ],
        ),
    ] {
        let host = Arc::new(FakeHost {
            key: Mutex::new(Some(b"ED25519-V3:CACHED".to_vec())),
            write_fails: true,
            ..FakeHost::default()
        });
        let mut ctl = TorController::new(opts("", false), host.clone());
        authenticated(&mut ctl);
        feed(&mut ctl, reply);
        assert_eq!(host.calls(), calls, "{reply}");
    }
}

/// `readcb`: lines shorter than 4 bytes are skipped, a code that is not a number is 0, `-` and
/// `+` lines gather until the final one, 6xx replies go to no handler, and an unexpected reply
/// is dropped.
// Core: torcontrol.cpp:77-112
#[test]
fn reply_lines() {
    let host = Arc::new(FakeHost::default());
    let mut ctl = TorController::new(opts("", true), host);
    assert!(ctl.line(b"250 OK").is_empty(), "no handler yet");
    ctl.connected();
    assert!(ctl.line(b"25").is_empty());
    assert!(ctl.line(b"650 STATUS_CLIENT NOTICE").is_empty(), "async: the handler stays");
    assert!(ctl.line(b"250-AUTH METHODS=NULL").is_empty());
    assert!(ctl.line(b"250+data").is_empty());
    assert!(ctl.line(b"abc-junk").is_empty());
    // The reply's code is its final line's.
    assert_eq!(text(&ctl.line(b"250 OK")), "AUTHENTICATE\r\n");
    let mut ctl = TorController::new(opts("", true), Arc::new(FakeHost::default()));
    ctl.connected();
    // `+50` is not a code (`ToIntegral` takes `-?[0-9]+`): 0, so PROTOCOLINFO failed.
    assert!(ctl.line(b"250-AUTH METHODS=NULL").is_empty());
    assert!(ctl.line(b"+50 OK").is_empty());
    assert!(ctl.line(b"250 OK").is_empty(), "the handler was used");
}

/// Handlers outlive a connection, as Core's `reply_handlers` do: a reply on the next connection
/// goes to the oldest one.
// Core: torcontrol.cpp:139-194 (Connect does not clear reply_handlers)
#[test]
fn handlers_survive_a_reconnection() {
    let host = Arc::new(FakeHost::default());
    let mut ctl = TorController::new(opts("", true), host);
    ctl.connected();
    ctl.disconnected();
    ctl.connected();
    assert_eq!(feed(&mut ctl, "250-AUTH METHODS=NULL\r\n250 OK\r\n"), "AUTHENTICATE\r\n");
    // The second PROTOCOLINFO handler gets the AUTHENTICATE reply.
    assert_eq!(feed(&mut ctl, "250 OK\r\n"), "");
}

/// The reconnect delay starts at 1 s and grows by 1.5 (in `float`, to whole milliseconds); a
/// connection resets it.
// Core: torcontrol.cpp:54-56, :611-635
#[test]
fn reconnect_backoff() {
    let mut ctl = TorController::new(opts("", true), Arc::new(FakeHost::default()));
    let delays: Vec<u128> = (0..8).map(|_| ctl.disconnected().as_millis()).collect();
    assert_eq!(delays, [1000, 1500, 2250, 3375, 5062, 7593, 11390, 17085]);
    ctl.connected();
    assert_eq!(ctl.disconnected().as_millis(), 1000);
}

/// `ReadBinaryFile`: at most `max` bytes; a directory or a missing file cannot be read.
// Core: util/readwritefile.cpp:16-35
#[test]
fn read_binary_file_is_cores() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("cookie");
    std::fs::write(&file, [7; 40]).unwrap();
    let path = |p: &std::path::Path| p.to_str().unwrap().as_bytes().to_vec();
    assert_eq!(read_binary_file(&path(&file), 32), Some(vec![7; 32]));
    assert_eq!(read_binary_file(&path(&file), 64), Some(vec![7; 40]));
    assert_eq!(read_binary_file(&path(dir.path()), 32), None);
    assert_eq!(read_binary_file(&path(&dir.path().join("none")), 32), None);
}

/// Lines end at `\n` with an optional `\r` (only one); a partial line waits for the rest.
// Core: torcontrol.cpp:84-89 (libevent `EVBUFFER_EOL_CRLF`)
#[test]
fn take_lines_is_libevents_crlf() {
    let mut input = b"250 OK\r\n250-A\n\r\r\n\n2\r50 X\r".to_vec();
    let mut lines = Vec::new();
    take_lines(&mut input, |l| lines.push(l.to_vec()));
    let expected: [&[u8]; 4] = [b"250 OK", b"250-A", b"\r", b""];
    assert_eq!(lines, expected);
    assert_eq!(input, b"2\r50 X\r");
    lines.clear();
    input.extend_from_slice(b"\nrest");
    take_lines(&mut input, |l| lines.push(l.to_vec()));
    let expected: [&[u8]; 1] = [b"2\r50 X"];
    assert_eq!(lines, expected);
    assert_eq!(input, b"rest");
}

/// libevent's classification of a non-blocking connect's error: in progress (Linux refuses a
/// loopback connect this way, later), refused at once (BSD and macOS: the error event), or failed
/// at once (no error event, no reconnection).
// libevent 2.1.12: evutil.c evutil_socket_connect_, bufferevent_sock.c:428-446
#[cfg(unix)]
#[test]
fn connect_errors_are_classified_as_libevent_does() {
    let start = |errno| connect_start(&std::io::Error::from_raw_os_error(errno));
    assert_eq!(start(libc::EINPROGRESS), ConnectStart::InProgress);
    assert_eq!(start(libc::EINTR), ConnectStart::InProgress);
    assert_eq!(start(libc::ECONNREFUSED), ConnectStart::Refused);
    for errno in [libc::ENETUNREACH, libc::EACCES, libc::EADDRNOTAVAIL, libc::EHOSTUNREACH] {
        assert_eq!(start(errno), ConnectStart::Failed, "{errno}");
    }
}

/// A reply's code: `ToIntegral<int>` of its first three bytes, so `+50` and junk are 0 and `-12`
/// is -12; 600 and above are asynchronous (no handler), 599 is a sync reply.
// Core: torcontrol.cpp:92-112, util/strencodings.h:181-190
#[test]
fn reply_codes_and_the_async_threshold() {
    assert_eq!(reply_code(b"250 OK"), 250);
    assert_eq!(reply_code(b"+50 OK"), 0);
    assert_eq!(reply_code(b"-12 X"), -12);
    assert_eq!(reply_code(b"2a0 X"), 0);
    for (first, handled) in [("599 X\r\n", true), ("600 X\r\n", false)] {
        let mut ctl = TorController::new(opts("", true), Arc::new(FakeHost::default()));
        ctl.connected();
        assert_eq!(feed(&mut ctl, first), "");
        // PROTOCOLINFO's handler is still there only after the asynchronous reply.
        let sent = feed(&mut ctl, "250-AUTH METHODS=NULL\r\n250 OK\r\n");
        assert_eq!(sent.is_empty(), handled, "{first}");
    }
}

/// `PROTOCOLINFO`'s methods are the last `AUTH` line's, not a union.
// Core: torcontrol.cpp:546-553
#[test]
fn the_last_auth_line_wins() {
    for (reply, sent) in [
        (
            "250-AUTH METHODS=HASHEDPASSWORD\r\n250-AUTH METHODS=NULL\r\n250 OK\r\n",
            "AUTHENTICATE\r\n",
        ),
        ("250-AUTH METHODS=NULL\r\n250-AUTH METHODS=HASHEDPASSWORD\r\n250 OK\r\n", ""),
    ] {
        let mut ctl = TorController::new(opts("", true), Arc::new(FakeHost::default()));
        ctl.connected();
        assert_eq!(feed(&mut ctl, reply), sent, "{reply}");
    }
}

/// `service_id` is a member: an `ADD_ONION` reply on a later connection without `ServiceID`
/// advertises the earlier one again, with the new key written.
// Core: torcontrol.cpp:418-447
#[test]
fn the_service_id_outlives_its_reply() {
    let host = Arc::new(FakeHost::default());
    let mut ctl = TorController::new(opts("", false), host.clone());
    authenticated(&mut ctl);
    feed(
        &mut ctl,
        &format!("250-ServiceID={ONION_ID}\r\n250-PrivateKey=ED25519-V3:A\r\n250 OK\r\n"),
    );
    ctl.disconnected();
    host.calls();
    authenticated(&mut ctl);
    feed(&mut ctl, "250-PrivateKey=ED25519-V3:B\r\n250 OK\r\n");
    assert_eq!(
        host.calls(),
        ["write_key ED25519-V3:B".to_owned(), format!("add_local {ONION_ID}.onion:19899")]
    );
}

/// SAFECOOKIE's two hashes for a fixed cookie and nonces, computed independently (Python's
/// `hmac`): HMAC-SHA256 keyed with Tor's constant over cookie ‖ client nonce ‖ server nonce.
// Core: torcontrol.cpp:473-498, :519-526
#[test]
fn safecookie_hashes_are_a_fixed_vector() {
    let (cookie, client, server) = ([0xc0; 32], [0x11; 32], [0x22; 32]);
    assert_eq!(
        hex(&compute_response(TOR_SAFE_SERVERKEY, &cookie, &client, &server)),
        "82456907f43cf26e44ba15e98696f8416755c33baebc49dac480f3b3821cf6eb"
    );
    assert_eq!(
        hex(&compute_response(TOR_SAFE_CLIENTKEY, &cookie, &client, &server)),
        "8d8516847c6fde8173a22bcc599e8aa132eac4e5756fed38aac8ce74658e71b0"
    );
}

/// Exactly `MAX_LINE_LENGTH` unfinished bytes are kept (Core compares with `>`) and the line
/// still counts once it ends; one more byte ends the connection.
// Core: torcontrol.cpp:84-119
#[test]
fn a_line_of_the_maximum_length_is_kept() {
    let mut ctl = TorController::new(opts("", true), Arc::new(FakeHost::default()));
    ctl.connected();
    let (mut input, mut pending) = (vec![b'2'; MAX_LINE_LENGTH], Vec::new());
    assert!(readcb(&mut ctl, &mut input, &mut pending));
    assert_eq!(input.len(), MAX_LINE_LENGTH);
    // A continuation line (its fourth byte is not a space), then the rest of the reply.
    input.extend_from_slice(b"\r\n250-AUTH METHODS=NULL\r\n250 OK\r\n");
    assert!(readcb(&mut ctl, &mut input, &mut pending));
    assert_eq!(pending, b"AUTHENTICATE\r\n");
    let mut input = vec![b'2'; MAX_LINE_LENGTH + 1];
    assert!(!readcb(&mut ctl, &mut input, &mut Vec::new()));
}
