//! The SAM 3.1 client against scripted proxies on 127.0.0.1: Core's request bytes, each reply
//! it accepts or refuses and the text of each failure, every failure mid-handshake with the
//! session and pool it leaves, reply bounds and split reads, the port rule, `proxy_error`, the
//! interrupt and the reply timeout (paused time), and the unused-session pool.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::sleep;

use super::*;
use crate::proxy_open_tests::{DeadPort, NOW, new_core, proxy_at, request};
use crate::rng::StreamRand;
use crate::{Address, OpenError, ServiceFlags, Target};

const PEER: &str = "zsxwyo6qcn3chqzwxnseusqgsnuw3maqnztkiypyfxtya4snkoka.b32.i2p";
const HELLO: &str = "HELLO VERSION MIN=3.1 MAX=3.1";
const HELLO_OK: &str = "HELLO REPLY RESULT=OK VERSION=3.1";

fn never() -> bool {
    false
}

/// Core's `EncodeBase64` with I2P's `-` and `~`.
fn i2p_base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-~";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    while !out.len().is_multiple_of(4) {
        out.push('=');
    }
    out
}

/// A transient private key: a 387-byte destination with a 4-byte certificate, then key bytes.
fn private_key() -> Vec<u8> {
    let mut key: Vec<u8> = (0..387u32).map(|i| (i * 7) as u8).collect();
    key[385..387].copy_from_slice(&4u16.to_be_bytes());
    key.extend_from_slice(&[5, 0, 4, 0]);
    key.extend(std::iter::repeat_n(0x42, 100));
    key
}

/// Our address for [`private_key`]: the SHA-256 of its destination.
fn my_addr() -> Service {
    Service::new(dest_bin_to_addr(&private_key()[..391]), 0)
}

/// One step of the mock's script for a connection.
#[derive(Clone, Debug)]
enum Step {
    /// Read one request line, then send this reply line.
    Reply(Vec<u8>),
    /// Read one request line, then close without a reply.
    Drop,
    /// Close now, reading nothing more.
    Close,
    /// Wait until the mock has closed connection `n` (in accept order).
    AwaitClosed(usize),
}

/// What the mock does on one connection, in order.
type Script = Vec<Step>;

/// Recorded after a connection's last line when the client closed it.
const EOF: &[u8] = b"<EOF>";

#[derive(Default)]
struct MockState {
    /// Each connection's request lines, raw, with their terminators.
    lines: Mutex<Vec<Vec<Vec<u8>>>>,
    /// Whether the mock's end of each connection is closed.
    closed: Mutex<Vec<bool>>,
}

/// A scripted SAM proxy: each accepted connection takes the next script and records the lines
/// it received. A connection whose script ran out stays open, reading (a stream's peer bytes are
/// not recorded), and records [`EOF`] once the client closes it.
struct MockSam {
    state: Arc<MockState>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockSam {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(s: TcpStream, script: Script, index: usize, state: Arc<MockState>) {
    let mut s = BufReader::new(s);
    let ran_out = 'script: {
        for step in script {
            let reply = match step {
                Step::Close => break 'script false,
                Step::AwaitClosed(n) => {
                    while !state.closed.lock().get(n).copied().unwrap_or(false) {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    continue;
                }
                Step::Reply(reply) => Some(reply),
                Step::Drop => None,
            };
            let mut line = Vec::new();
            if s.read_until(b'\n', &mut line).await.unwrap_or(0) == 0 {
                break 'script false;
            }
            state.lines.lock()[index].push(line);
            let Some(reply) = reply else {
                break 'script false;
            };
            if s.write_all(&[&reply[..], b"\n"].concat()).await.is_err() {
                break 'script false;
            }
        }
        true
    };
    if ran_out {
        let mut buf = [0u8; 512];
        while matches!(s.read(&mut buf).await, Ok(n) if n > 0) {}
        state.lines.lock()[index].push(EOF.to_vec());
    }
    drop(s);
    state.closed.lock()[index] = true;
}

impl MockSam {
    async fn start(scripts: Vec<Script>) -> (Self, Proxy) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = Service::from_socket_addr(listener.local_addr().unwrap());
        let state = Arc::new(MockState::default());
        let shared = Arc::clone(&state);
        let task = tokio::spawn(async move {
            let mut open = Vec::new();
            for script in scripts {
                let Ok((s, _)) = listener.accept().await else {
                    return;
                };
                let index = {
                    let mut lines = shared.lines.lock();
                    lines.push(Vec::new());
                    shared.closed.lock().push(false);
                    lines.len() - 1
                };
                open.push(tokio::spawn(serve(s, script, index, Arc::clone(&shared))));
            }
            std::future::pending::<()>().await;
        });
        let proxy = Proxy {
            addr: ProxyAddr::Service(at),
            randomize_credentials: false,
        };
        (
            Self {
                state,
                task,
            },
            proxy,
        )
    }

    /// The lines as text (EOF as `<EOF>`).
    fn lines(&self) -> Vec<Vec<String>> {
        let lines = self.state.lines.lock();
        lines.iter().map(|c| c.iter().map(|l| text(l).into_owned()).collect()).collect()
    }

    fn raw_lines(&self) -> Vec<Vec<Vec<u8>>> {
        self.state.lines.lock().clone()
    }

    /// Waits up to 10 s until the mock's end of every connection but those in `open` is closed:
    /// the client closed each socket it no longer holds.
    async fn wait_closed_except(&self, open: &[usize]) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        let done = || {
            let closed = self.state.closed.lock();
            closed.iter().enumerate().all(|(i, c)| *c || open.contains(&i))
        };
        while !done() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        done()
    }
}

fn ok(reply: &str) -> Step {
    Step::Reply(reply.as_bytes().to_vec())
}

/// The control connection's script: HELLO and a transient SESSION CREATE that returns
/// [`private_key`].
fn control() -> Script {
    vec![
        ok(HELLO_OK),
        ok(&format!("SESSION STATUS RESULT=OK DESTINATION={}", i2p_base64(&private_key()))),
    ]
}

/// A stream connection's script: HELLO, the lookup, then `STREAM STATUS RESULT=<result>`.
fn stream(result: &str) -> Script {
    vec![
        ok(HELLO_OK),
        ok(&format!("NAMING REPLY RESULT=OK NAME={PEER} VALUE=peerdest~-AA")),
        ok(&format!("STREAM STATUS RESULT={result}")),
    ]
}

fn peer(port: u16) -> Service {
    Service::new(NetAddr::set_special(PEER).unwrap(), port)
}

/// The session id the zero-key stream draws first, as `GetRandHash().GetHex().substr(0, 10)`.
fn first_session_id() -> String {
    let hash = get_rand_hash(&StreamRand::with_key([0; 32]));
    hash.iter().rev().take(5).map(|b| format!("{b:02x}")).collect()
}

async fn connect(
    session: &mut Session,
    to: &Service,
    rand: &StreamRand,
) -> Result<Connection, bool> {
    session.connect(to, rand, &never).await.map_err(|(proxy_error, _)| proxy_error)
}

const T: Duration = Duration::from_millis(5000);

/// A connection that succeeds: Core's requests byte for byte on the control socket and on the
/// stream socket, `me` is the SHA-256 of the transient destination, and the stream carries the
/// peer's bytes. The control socket stays open for the session's life.
// Core: i2p.cpp:219-258, :326-337, :405-456
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transient_connect_sends_core_bytes() {
    let (sam, proxy) = MockSam::start(vec![control(), stream("OK")]).await;
    let mut session = Session::transient(proxy, T);
    let rand = StreamRand::with_key([0; 32]);
    let conn = connect(&mut session, &peer(0), &rand).await.ok().unwrap();
    assert_eq!(conn.me, my_addr());
    assert!(conn.me.to_string().ends_with(".b32.i2p:0"));
    let id = first_session_id();
    assert_eq!(id.len(), 10);
    assert_eq!(
        sam.lines(),
        [
            vec![
                format!("{HELLO}\n"),
                format!(
                    "SESSION CREATE STYLE=STREAM ID={id} DESTINATION=TRANSIENT SIGNATURE_TYPE=7 i2cp.leaseSetEncType=4,0 inbound.quantity=1 outbound.quantity=1\n"
                ),
            ],
            vec![
                format!("{HELLO}\n"),
                format!("NAMING LOOKUP NAME={PEER}\n"),
                format!("STREAM CONNECT ID={id} DESTINATION=peerdest~-AA SILENT=false\n"),
            ],
        ]
    );
    assert_eq!(session.session_id, id);
    assert!(is_connected(session.control_sock.as_ref().unwrap()).is_ok());
}

/// A session is created once: a second connect reuses its id over a live control socket.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_session_is_reused() {
    let (sam, proxy) =
        MockSam::start(vec![control(), stream("CANT_REACH_PEER"), stream("OK")]).await;
    let mut session = Session::transient(proxy, T);
    let rand = StreamRand::with_key([0; 32]);
    assert_eq!(connect(&mut session, &peer(0), &rand).await.err(), Some(false));
    connect(&mut session, &peer(0), &rand).await.ok().unwrap();
    let lines = sam.lines();
    assert_eq!(lines.len(), 3);
    assert!(lines[2][2].starts_with(&format!("STREAM CONNECT ID={} ", first_session_id())));
}

/// Core's port rule: a port other than 0 is refused before any I/O, and it is the peer's
/// failure (`proxy_error` false), so AddrMan counts the attempt.
// Core: i2p.cpp:221-227
#[tokio::test]
async fn a_port_other_than_zero_is_refused_without_io() {
    let (sam, proxy) = MockSam::start(vec![]).await;
    let mut session = Session::transient(proxy, T);
    let rand = StreamRand::with_key([0; 32]);
    assert_eq!(connect(&mut session, &peer(8333), &rand).await.err(), Some(false));
    assert!(sam.lines().is_empty());
    assert!(session.control_sock.is_none());
}

/// `proxy_error` by failure: an unreachable proxy, a bad greeting, a lookup that fails and a
/// stream refusal other than the peer's are the proxy's (true); `CANT_REACH_PEER` and `TIMEOUT`
/// are the peer's (false).
// Core: i2p.cpp:229, :266-268
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_error_by_failure() {
    let cases: [(Vec<Script>, bool); 6] = [
        (vec![control(), stream("CANT_REACH_PEER")], false),
        (vec![control(), stream("TIMEOUT")], false),
        (vec![control(), stream("I2P_ERROR")], true),
        (vec![control(), stream("INVALID_KEY")], true),
        (vec![vec![ok("HELLO REPLY RESULT=NOVERSION")]], true),
        (vec![control(), vec![ok(HELLO_OK), ok("NAMING REPLY RESULT=KEY_NOT_FOUND")]], true),
    ];
    for (scripts, expected) in cases {
        let what = format!("{scripts:?}");
        let (_sam, proxy) = MockSam::start(scripts).await;
        let mut session = Session::transient(proxy, T);
        let rand = StreamRand::with_key([0; 32]);
        assert_eq!(connect(&mut session, &peer(0), &rand).await.err(), Some(expected), "{what}");
    }
    // Nothing listens: Core's `Cannot connect to <proxy>`, a proxy error.
    let closed = DeadPort::new();
    let mut session = Session::transient(proxy_at(&closed.at, false), T);
    let rand = StreamRand::with_key([0; 32]);
    assert_eq!(connect(&mut session, &peer(0), &rand).await.err(), Some(true));
}

/// Each failure's text, as Core's `runtime_error` carries it into "Error connecting to".
// Core: i2p.cpp:263, :270, :280-288, :318-321, :331
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failure_texts() {
    let closed = DeadPort::new();
    let mut session = Session::transient(proxy_at(&closed.at, false), T);
    let rand = StreamRand::with_key([0; 32]);
    let e = session.try_connect(&peer(0), &rand, &never, &mut true).await.err().unwrap();
    assert_eq!(e, format!("Cannot connect to {}", closed.at));

    let cases: [(Vec<Script>, String); 6] = [
        (
            vec![vec![ok("HELLO REPLY RESULT=NOVERSION")]],
            format!("Reply to \"{HELLO}\": had a RESULT not equal to OK."),
        ),
        (vec![vec![ok("HELLO REPLY")]], format!("Missing RESULT= in the reply to \"{HELLO}\"")),
        (
            vec![vec![ok(HELLO_OK), ok("SESSION STATUS RESULT=DUPLICATED_ID")]],
            "Reply to \"SESSION CREATE ...\": had a RESULT not equal to OK.".to_owned(),
        ),
        (
            vec![vec![ok(HELLO_OK), ok("SESSION STATUS RESULT=OK DESTINATION=a+b=")]],
            "Cannot decode Base64: \"a+b=\"".to_owned(),
        ),
        (
            vec![control(), vec![ok(HELLO_OK), ok("NAMING REPLY RESULT=OK VALUE")]],
            format!("Missing VALUE= in the reply to \"NAMING LOOKUP NAME={PEER}\""),
        ),
        (
            vec![control(), stream("CANT_REACH_PEER MESSAGE=x")],
            "\"STREAM STATUS RESULT=CANT_REACH_PEER MESSAGE=x\"".to_owned(),
        ),
    ];
    for (scripts, expected) in cases {
        let (_sam, proxy) = MockSam::start(scripts).await;
        let mut session = Session::transient(proxy, T);
        let e = session.try_connect(&peer(0), &rand, &never, &mut true).await.err().unwrap();
        assert_eq!(e, expected);
    }
}

/// A short private key and a certificate longer than the key: Core's `MyDestination` texts.
// Core: i2p.cpp:383-400
#[test]
fn my_destination_checks_the_key_length() {
    assert_eq!(my_destination(&[0; 386]).unwrap_err(), "The private key is too short (386 < 387)");
    let mut key = vec![0; 390];
    key[385..387].copy_from_slice(&4u16.to_be_bytes());
    assert_eq!(
        my_destination(&key).unwrap_err(),
        "Certificate length (4) designates that the private key should be 391 bytes, but it is only 390 bytes"
    );
    assert_eq!(my_destination(&private_key()).unwrap().len(), 391);
}

/// I2P Base64: `-` and `~` for `+` and `/`, Core's padding and leftover-bit rules.
#[test]
fn i2p_base64_decodes_as_core() {
    assert_eq!(decode_i2p_base64(b"-~--").unwrap(), [0xfb, 0xff, 0xbe]);
    assert_eq!(decode_i2p_base64(b"YQ==").unwrap(), b"a");
    for bad in ["+/++", "YQ=", "YR==", "Y===", "abc"] {
        assert!(decode_i2p_base64(bad.as_bytes()).is_err(), "{bad}");
    }
    assert_eq!(decode_i2p_base64(b"\xff===").unwrap_err(), "Cannot decode Base64: \"\u{fffd}===\"");
    let key = private_key();
    assert_eq!(decode_i2p_base64(i2p_base64(&key).as_bytes()).unwrap(), key);
}

/// The reply to `request` when the proxy answers `reply` (raw bytes, then `\n`).
async fn parsed(request: &[u8], reply: &[u8]) -> Result<Reply, Error> {
    let (ours, mut theirs) = pair().await;
    theirs.write_all(&[reply, b"\n"].concat()).await.unwrap();
    let reply = send_request_and_get_reply(&ours, request, true, &never).await;
    let mut sent = vec![0; request.len() + 1];
    theirs.read_exact(&mut sent).await.unwrap();
    assert_eq!(sent, [request, b"\n"].concat(), "the request as sent");
    reply
}

/// A reply line is parsed into words split at every space, each at its first `=`, as Core's
/// `spanparsing::Split` and `emplace` parse it: a word without `=` has no value, an empty word
/// or key is a key, an empty value is a value, the first of a repeated key wins (even without a
/// value), and nothing is trimmed.
// Core: i2p.cpp:309-322
#[tokio::test]
async fn reply_parsing() {
    let reply = parsed(b"X", b"A RESULT=OK B=1 B=2 C D=x=y E= =F  K K=v").await.unwrap();
    assert_eq!(reply.get("B").unwrap(), b"1");
    assert_eq!(reply.get("D").unwrap(), b"x=y");
    assert_eq!(reply.get("E").unwrap(), b"");
    assert_eq!(reply.get("").unwrap(), b"F");
    assert_eq!(reply.get("C").unwrap_err(), "Missing C= in the reply to \"X\"");
    assert_eq!(reply.get("K").unwrap_err(), "Missing K= in the reply to \"X\"");
    assert!(reply.keys.contains_key(&b"A"[..]) && reply.keys.len() == 8, "{:?}", reply.keys);
    assert_eq!(reply.full, b"A RESULT=OK B=1 B=2 C D=x=y E= =F  K K=v");
    let not_ok = |request: &str| format!("Reply to \"{request}\": had a RESULT not equal to OK.");
    for (line, error) in [
        (&b""[..], "Missing RESULT= in the reply to \"X\"".to_owned()),
        (b"RESULT", "Missing RESULT= in the reply to \"X\"".to_owned()),
        (b"RESULT=OK\r", not_ok("X")),
        (b"RESULT=ok", not_ok("X")),
        (b" RESULT=OK", "".to_owned()),
        (b"RESULT=NO RESULT=OK", not_ok("X")),
    ] {
        let got = parsed(b"X", line).await.err().unwrap_or_default();
        assert_eq!(got, error, "{}", text(line));
    }
    assert_eq!(
        parsed(b"SESSION CREATE x", b"RESULT=NO").await.err().unwrap(),
        not_ok("SESSION CREATE ...")
    );
    // Bytes that are not UTF-8 stay as they are; only error text is lossy.
    let reply = parsed(b"X", b"RESULT=OK V=\xff\xfe").await.unwrap();
    assert_eq!(reply.get("V").unwrap(), b"\xff\xfe");
    assert_eq!(parsed(b"\xffX", b"RESULT=NO").await.err().unwrap(), not_ok("\u{fffd}X"));
}

/// A connected pair on 127.0.0.1: our socket and the proxy's end.
async fn pair() -> (TcpSocket, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ours = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (theirs, _) = listener.accept().await.unwrap();
    (TcpSocket::new(ours).unwrap(), theirs)
}

/// `RecvUntilTerminator`: never past the terminator, so the next reply is intact; a line
/// longer than `MAX_MSG_SIZE` and a closed socket fail with Core's texts.
// Core: util/sock.cpp:466-509
#[tokio::test]
async fn reads_stop_at_the_terminator() {
    let (ours, mut theirs) = pair().await;
    theirs.write_all(b"first\nsecond\n").await.unwrap();
    let read = |max| recv_until_terminator(&ours, b'\n', RECV_TIMEOUT, &never, max);
    assert_eq!(read(MAX_MSG_SIZE).await.unwrap(), b"first");
    assert_eq!(read(MAX_MSG_SIZE).await.unwrap(), b"second");
    theirs.write_all(&[b'x'; 600]).await.unwrap();
    assert_eq!(read(512).await.unwrap_err(), "Received too many bytes without a terminator (512)");
    drop(theirs);
    assert_eq!(read(88).await.unwrap_err(), "Received too many bytes without a terminator (88)");
    assert_eq!(read(MAX_MSG_SIZE).await.unwrap_err(), "Connection unexpectedly closed by peer");
}

/// A reply that never ends times out after Core's three minutes, with the bytes it got.
// Core: i2p.cpp:305; util/sock.cpp:512-517
#[tokio::test(start_paused = true)]
async fn a_reply_times_out_after_three_minutes() {
    let (ours, mut theirs) = pair().await;
    theirs.write_all(b"HELLO REPLY").await.unwrap();
    let start = Instant::now();
    let e = recv_until_terminator(&ours, b'\n', RECV_TIMEOUT, &never, MAX_MSG_SIZE).await;
    assert_eq!(
        e.unwrap_err(),
        "Receive timeout (received 11 bytes without terminator before that)"
    );
    assert!(start.elapsed() >= RECV_TIMEOUT);
}

/// `interruptNet` in Core's order: each turn peeks without waiting, then checks the deadline
/// and the interrupt, and only then waits. So a set interrupt ends a read with nothing pending
/// at once (no `MAX_WAIT_FOR_IO` wait first, under paused time), after the bytes already there;
/// a line already there is still read whole, even past the deadline; and a spent deadline wins
/// over a set interrupt.
// Core: util/sock.cpp:466-528
#[tokio::test(start_paused = true)]
async fn the_interrupt_ends_a_wait() {
    let (ours, mut theirs) = pair().await;
    let flag = AtomicBool::new(true);
    let interrupted = || flag.load(Ordering::SeqCst);
    let read = || recv_until_terminator(&ours, b'\n', RECV_TIMEOUT, &interrupted, MAX_MSG_SIZE);
    let start = Instant::now();
    assert_eq!(
        read().await.unwrap_err(),
        "Receive interrupted (received 0 bytes without terminator before that)"
    );
    theirs.write_all(b"HELLO REPLY RESULT=OK\nHEL").await.unwrap();
    // The bytes are on the socket before the runtime has been told.
    assert_eq!(read().await.unwrap(), b"HELLO REPLY RESULT=OK");
    assert_eq!(
        read().await.unwrap_err(),
        "Receive interrupted (received 3 bytes without terminator before that)"
    );
    let spent = || recv_until_terminator(&ours, b'\n', Duration::ZERO, &interrupted, MAX_MSG_SIZE);
    assert_eq!(
        spent().await.unwrap_err(),
        "Receive timeout (received 0 bytes without terminator before that)"
    );
    theirs.write_all(b"X\n").await.unwrap();
    assert_eq!(spent().await.unwrap(), b"X");
    assert_eq!(start.elapsed(), Duration::ZERO);
    flag.store(false, Ordering::SeqCst);
    let late = async {
        sleep(Duration::from_millis(1500)).await;
        flag.store(true, Ordering::SeqCst);
    };
    let (e, ()) = tokio::join!(read(), late);
    assert_eq!(
        e.unwrap_err(),
        "Receive interrupted (received 0 bytes without terminator before that)"
    );
    // Seen at the end of the wait that the interrupt fell in: at most `MAX_WAIT_FOR_IO` late.
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_millis(1500) && waited <= Duration::from_millis(2000),
        "{waited:?}"
    );
}

/// A reply that arrives one byte per read, with a terminator split from its line and then the
/// next line's bytes after it, reads as Core reads it: each line whole, nothing past it. EOF
/// before the terminator fails with Core's text.
// Core: util/sock.cpp:466-509
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_split_one_byte_per_read() {
    let (ours, mut theirs) = pair().await;
    let writer = tokio::spawn(async move {
        for &b in b"NAMING REPLY RESULT=OK VALUE=d\nSTREAM STATUS RESULT=OK\nHELLO REP" {
            theirs.write_all(&[b]).await.unwrap();
            theirs.flush().await.unwrap();
            sleep(Duration::from_millis(1)).await;
        }
        theirs
    });
    let read = || recv_until_terminator(&ours, b'\n', RECV_TIMEOUT, &never, MAX_MSG_SIZE);
    assert_eq!(read().await.unwrap(), b"NAMING REPLY RESULT=OK VALUE=d");
    assert_eq!(read().await.unwrap(), b"STREAM STATUS RESULT=OK");
    drop(writer.await.unwrap());
    assert_eq!(read().await.unwrap_err(), "Connection unexpectedly closed by peer");
    let (ours, mut theirs) = pair().await;
    theirs.write_all(b"AB").await.unwrap();
    let rest = async {
        sleep(Duration::from_millis(50)).await;
        theirs.write_all(b"\nC\n").await.unwrap();
    };
    let read = || recv_until_terminator(&ours, b'\n', RECV_TIMEOUT, &never, MAX_MSG_SIZE);
    let (line, ()) = tokio::join!(read(), rest);
    assert_eq!(line.unwrap(), b"AB");
    assert_eq!(read().await.unwrap(), b"C");
}

/// `MAX_MSG_SIZE`: a reply of `MAX_MSG_SIZE - 1` bytes and its terminator is read, the last
/// peek cut to the one byte left; one of `MAX_MSG_SIZE` bytes fails before its terminator. An
/// empty line is a reply with no words.
// Core: i2p.h:51; util/sock.cpp:466-474
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replies_are_bounded_by_max_msg_size() {
    for (len, expected) in [
        (MAX_MSG_SIZE - 1, Ok(MAX_MSG_SIZE - 1)),
        (
            MAX_MSG_SIZE,
            Err(format!("Received too many bytes without a terminator ({MAX_MSG_SIZE})")),
        ),
        (0, Ok(0)),
    ] {
        let (ours, mut theirs) = pair().await;
        let writer = tokio::spawn(async move {
            let mut line = vec![b'x'; len];
            line.extend_from_slice(b"\nnext\n");
            theirs.write_all(&line).await.unwrap();
            theirs
        });
        let read = || recv_until_terminator(&ours, b'\n', RECV_TIMEOUT, &never, MAX_MSG_SIZE);
        assert_eq!(read().await.map(|l| l.len()), expected, "{len}");
        if expected.is_ok() {
            assert_eq!(read().await.unwrap(), b"next", "{len}: nothing past the terminator");
        }
        drop(writer.await.unwrap());
    }
}

/// A connect that the interrupt ends while the proxy is silent: the proxy's failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_connect_is_a_proxy_error() {
    let (_sam, proxy) = MockSam::start(vec![vec![]]).await;
    let mut session = Session::transient(proxy, T);
    let rand = StreamRand::with_key([0; 32]);
    let interrupted = || true;
    let r = session.connect(&peer(0), &rand, &interrupted).await;
    assert_eq!(r.err().map(|(proxy_error, _)| proxy_error), Some(true));
}

/// A control socket the proxy closes during a connect ends the session at the failed connect
/// (`CheckControlSock`, before the next use), so the next connect creates a new one with a new
/// id.
// Core: i2p.cpp:271-274, :339-348, :405-410
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_control_socket_ends_the_session() {
    let mut closing = control();
    closing.push(Step::Close);
    let (sam, proxy) = MockSam::start(vec![
        closing,
        vec![
            ok(HELLO_OK),
            ok("NAMING REPLY RESULT=OK VALUE=d"),
            Step::AwaitClosed(0),
            ok("STREAM STATUS RESULT=I2P_ERROR"),
        ],
        control(),
        stream("OK"),
    ])
    .await;
    let mut session = Session::transient(proxy, T);
    let rand = StreamRand::with_key([0; 32]);
    assert_eq!(connect(&mut session, &peer(0), &rand).await.err(), Some(true));
    assert!(session.control_sock.is_none() && session.session_id.is_empty());
    connect(&mut session, &peer(0), &rand).await.ok().unwrap();
    let lines = sam.lines();
    assert_eq!(lines.len(), 4);
    assert_ne!(lines[0][1], lines[2][1], "a second SESSION CREATE, with a new id");
}

/// `INVALID_ID` destroys the session so the next use creates a new one.
// Core: i2p.cpp:260-264
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_id_destroys_the_session() {
    let (_sam, proxy) = MockSam::start(vec![control(), stream("INVALID_ID")]).await;
    let mut session = Session::transient(proxy, T);
    let rand = StreamRand::with_key([0; 32]);
    assert_eq!(connect(&mut session, &peer(0), &rand).await.err(), Some(true));
    assert!(session.control_sock.is_none() && session.session_id.is_empty());
}

/// The pool keeps at most `MAX_UNUSED_I2P_SESSIONS_SIZE` sessions and hands back the oldest
/// first; an empty pool makes a new transient session.
// Core: net.cpp:444-460; net.h:2057
#[test]
fn the_unused_pool_is_capped() {
    let pool = UnusedSessions::default();
    let proxy = |port| Proxy {
        addr: ProxyAddr::Service(Service::new(NetAddr::ipv4([127, 0, 0, 1]), port)),
        randomize_credentials: false,
    };
    for port in 1..=12 {
        pool.put_back(Session::transient(proxy(port), T));
    }
    assert_eq!(pool.len(), MAX_UNUSED_I2P_SESSIONS_SIZE);
    assert_eq!(pool.take_or_create(&proxy(99), T).control_host, proxy(1));
    for _ in 1..MAX_UNUSED_I2P_SESSIONS_SIZE {
        pool.take_or_create(&proxy(99), T);
    }
    assert_eq!(pool.len(), 0);
    assert_eq!(pool.take_or_create(&proxy(99), T).control_host, proxy(99));
}

/// The connection keeps its session: the control socket stays open while the peer's socket is
/// open, and closing that socket (Core's `CloseSocketDisconnect`) destroys the session.
// Core: net.cpp:511, :546-548
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_peer_socket_holds_its_session() {
    let (sam, proxy) = MockSam::start(vec![control(), stream("OK")]).await;
    let mut session = Session::transient(proxy, T);
    let rand = StreamRand::with_key([0; 32]);
    let conn = connect(&mut session, &peer(0), &rand).await.ok().unwrap();
    let sock = I2pSocket::new(conn, session);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(sam.lines().iter().all(|l| l.last().map(String::as_str) != Some("<EOF>")));
    sock.close();
    let deadline = Instant::now() + Duration::from_secs(10);
    let both_closed = || sam.lines().iter().all(|l| l.last().map(String::as_str) == Some("<EOF>"));
    while !both_closed() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(both_closed(), "{:?}", sam.lines());
}

fn i2p_entry(port: u16) -> Address {
    Address {
        service: peer(port),
        services: ServiceFlags::NODE_NETWORK,
        time: NOW as u32,
    }
}

/// `ConnectNode`'s I2P branch with no persistent session: a failed connect puts its transient
/// session in the pool and the next connect takes it (one `SESSION CREATE`); the peer's failure
/// counts as an AddrMan attempt; a success binds the peer to our I2P address and keeps the
/// session out of the pool.
// Core: net.cpp:437-482
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_takes_a_transient_session_from_the_pool() {
    let (sam, proxy) =
        MockSam::start(vec![control(), stream("CANT_REACH_PEER"), stream("OK")]).await;
    let (core, _) = new_core(|cfg| {
        cfg.connect_timeout = T;
        assert!(cfg.proxies.set(Network::I2p, proxy));
    });
    let entry = i2p_entry(0);
    core.addrman().add(std::slice::from_ref(&entry), &NetAddr::ipv4([5, 6, 7, 8]), 0, NOW);
    let r = core.open(request(Target::Addr(entry.clone()))).await;
    assert!(matches!(r, Err(OpenError::Connect(_))), "{r:?}");
    assert_eq!(core.addrman().get_address_info(&entry.service).unwrap().last_try, NOW);
    assert_eq!(core.unused_i2p_sessions.len(), 1);
    let connected = core.open(request(Target::Addr(entry))).await.unwrap();
    assert_eq!(connected.addr_bind(), &my_addr());
    assert_eq!(core.unused_i2p_sessions.len(), 0);
    let creates = sam.lines().iter().flatten().filter(|l| l.starts_with("SESSION CREATE")).count();
    assert_eq!(creates, 1);
}

/// The attempt rule through the SAM proxy: a proxy that cannot be reached records no attempt; a
/// port other than 0 is the peer's failure and records one. Either way the session goes back to
/// the pool, as Core returns every transient session whose connect failed.
// Core: net.cpp:454-460, :476-481; i2p.cpp:223-229
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_attempt_rule_through_the_sam_proxy() {
    let closed = DeadPort::new();
    for (port, attempted) in [(0, false), (8333, true)] {
        let (core, _) =
            new_core(|cfg| assert!(cfg.proxies.set(Network::I2p, proxy_at(&closed.at, false))));
        let entry = i2p_entry(port);
        core.addrman().add(std::slice::from_ref(&entry), &NetAddr::ipv4([5, 6, 7, 8]), 0, NOW);
        let r = core.open(request(Target::Addr(entry.clone()))).await;
        assert!(matches!(r, Err(OpenError::Connect(_))), "{r:?}");
        let info = core.addrman().get_address_info(&entry.service).unwrap();
        assert_eq!(info.last_try == NOW, attempted, "port {port}");
        assert_eq!(core.unused_i2p_sessions.len(), 1, "port {port}");
    }
}

/// One mid-handshake failure: what the proxy does, then the text Core logs after "Error
/// connecting to <peer>: ", `proxy_error`, and whether the session (its control socket and id)
/// is still alive after the failed connect.
struct Failure {
    what: &'static str,
    scripts: Vec<Script>,
    text: String,
    proxy_error: bool,
    alive: bool,
}

/// Every failure at each step of a transient connect (HELLO, SESSION CREATE, NAMING LOOKUP, the
/// stream's HELLO, STREAM CONNECT), each as an error RESULT and as a dropped socket, plus a
/// control socket closed during the connect and an unreachable proxy. For each, through
/// [`Session::connect`]: Core's text, `proxy_error`, and the session left alive only when its
/// control socket still is; every socket the client no longer holds is closed (no leak). Then
/// through `NetCore::open`: the session is back in the pool in that state, and AddrMan counts
/// the attempt unless the proxy failed.
// Core: i2p.cpp:219-276, :290-337, :339-348, :405-456; net.cpp:444-481
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_mid_handshake_failure() {
    let closed_by_peer = || "Connection unexpectedly closed by peer".to_owned();
    let not_ok = |request: &str| format!("Reply to \"{request}\": had a RESULT not equal to OK.");
    let naming = format!("NAMING REPLY RESULT=OK NAME={PEER} VALUE=peerdest~-AA");
    let stream_with = |last: Vec<Step>| [vec![ok(HELLO_OK), ok(&naming)], last].concat();
    let id = first_session_id();
    let stream_request = format!("STREAM CONNECT ID={id} DESTINATION=peerdest~-AA SILENT=false");
    let mut closing = control();
    closing.push(Step::Close);
    let rows = vec![
        Failure {
            what: "HELLO refused",
            scripts: vec![vec![ok("HELLO REPLY RESULT=NOVERSION")]],
            text: not_ok(HELLO),
            proxy_error: true,
            alive: false,
        },
        Failure {
            what: "HELLO dropped",
            scripts: vec![vec![Step::Drop]],
            text: closed_by_peer(),
            proxy_error: true,
            alive: false,
        },
        Failure {
            what: "SESSION CREATE refused",
            scripts: vec![vec![ok(HELLO_OK), ok("SESSION STATUS RESULT=DUPLICATED_DEST")]],
            text: not_ok("SESSION CREATE ..."),
            proxy_error: true,
            alive: false,
        },
        Failure {
            what: "SESSION CREATE dropped",
            scripts: vec![vec![ok(HELLO_OK), Step::Drop]],
            text: closed_by_peer(),
            proxy_error: true,
            alive: false,
        },
        Failure {
            what: "SESSION CREATE without a destination",
            scripts: vec![vec![ok(HELLO_OK), ok("SESSION STATUS RESULT=OK")]],
            text: "Missing DESTINATION= in the reply to \"SESSION CREATE ...\"".to_owned(),
            proxy_error: true,
            alive: false,
        },
        Failure {
            what: "SESSION CREATE with a short key",
            scripts: vec![vec![ok(HELLO_OK), ok("SESSION STATUS RESULT=OK DESTINATION=AAAA")]],
            text: "The private key is too short (3 < 387)".to_owned(),
            proxy_error: true,
            alive: false,
        },
        Failure {
            what: "stream HELLO refused",
            scripts: vec![control(), vec![ok("HELLO REPLY RESULT=I2P_ERROR")]],
            text: not_ok(HELLO),
            proxy_error: true,
            alive: true,
        },
        Failure {
            what: "stream HELLO dropped",
            scripts: vec![control(), vec![Step::Drop]],
            text: closed_by_peer(),
            proxy_error: true,
            alive: true,
        },
        Failure {
            what: "NAMING LOOKUP refused",
            scripts: vec![control(), vec![ok(HELLO_OK), ok("NAMING REPLY RESULT=KEY_NOT_FOUND")]],
            text: not_ok(&format!("NAMING LOOKUP NAME={PEER}")),
            proxy_error: true,
            alive: true,
        },
        Failure {
            what: "NAMING LOOKUP dropped",
            scripts: vec![control(), vec![ok(HELLO_OK), Step::Drop]],
            text: closed_by_peer(),
            proxy_error: true,
            alive: true,
        },
        Failure {
            what: "STREAM CONNECT dropped",
            scripts: vec![control(), stream_with(vec![Step::Drop])],
            text: closed_by_peer(),
            proxy_error: true,
            alive: true,
        },
        Failure {
            what: "STREAM CONNECT without a RESULT",
            scripts: vec![control(), stream_with(vec![ok("STREAM STATUS")])],
            text: format!("Missing RESULT= in the reply to \"{stream_request}\""),
            proxy_error: true,
            alive: true,
        },
        Failure {
            what: "STREAM CONNECT CANT_REACH_PEER",
            scripts: vec![control(), stream("CANT_REACH_PEER")],
            text: "\"STREAM STATUS RESULT=CANT_REACH_PEER\"".to_owned(),
            proxy_error: false,
            alive: true,
        },
        Failure {
            what: "STREAM CONNECT TIMEOUT",
            scripts: vec![control(), stream("TIMEOUT")],
            text: "\"STREAM STATUS RESULT=TIMEOUT\"".to_owned(),
            proxy_error: false,
            alive: true,
        },
        Failure {
            what: "STREAM CONNECT I2P_ERROR",
            scripts: vec![control(), stream("I2P_ERROR")],
            text: "\"STREAM STATUS RESULT=I2P_ERROR\"".to_owned(),
            proxy_error: true,
            alive: true,
        },
        Failure {
            what: "STREAM CONNECT INVALID_ID",
            scripts: vec![control(), stream("INVALID_ID")],
            text: "Invalid session id".to_owned(),
            proxy_error: true,
            alive: false,
        },
        Failure {
            what: "control socket closed during the connect",
            scripts: vec![
                closing,
                stream_with(vec![Step::AwaitClosed(0), ok("STREAM STATUS RESULT=I2P_ERROR")]),
            ],
            text: "\"STREAM STATUS RESULT=I2P_ERROR\"".to_owned(),
            proxy_error: true,
            alive: false,
        },
    ];
    for row in rows {
        let what = row.what;
        let (sam, proxy) = MockSam::start(row.scripts.clone()).await;
        let mut session = Session::transient(proxy, T);
        let rand = StreamRand::with_key([0; 32]);
        let (proxy_error, text) = session.connect(&peer(0), &rand, &never).await.err().unwrap();
        assert_eq!((proxy_error, text.as_str()), (row.proxy_error, row.text.as_str()), "{what}");
        assert_eq!(session.control_sock.is_some(), row.alive, "{what}: control socket");
        assert_eq!(session.session_id.is_empty(), !row.alive, "{what}: session id");
        let open: &[usize] = if row.alive {
            &[0]
        } else {
            &[]
        };
        assert!(sam.wait_closed_except(open).await, "{what}: a socket left open");
        drop(session);
        assert!(sam.wait_closed_except(&[]).await, "{what}: the session's socket left open");

        let (_sam, proxy) = MockSam::start(row.scripts).await;
        let (core, _) = new_core(|cfg| {
            cfg.connect_timeout = T;
            assert!(cfg.proxies.set(Network::I2p, proxy.clone()));
        });
        let entry = i2p_entry(0);
        core.addrman().add(std::slice::from_ref(&entry), &NetAddr::ipv4([5, 6, 7, 8]), 0, NOW);
        let r = core.open(request(Target::Addr(entry.clone()))).await;
        assert!(matches!(r, Err(OpenError::Connect(_))), "{what}: {r:?}");
        let info = core.addrman().get_address_info(&entry.service).unwrap();
        assert_eq!(info.last_try == NOW, !row.proxy_error, "{what}: attempt");
        assert_eq!(core.unused_i2p_sessions.len(), 1, "{what}: pool");
        let pooled = core.unused_i2p_sessions.take_or_create(&proxy, T);
        assert_eq!(pooled.control_sock.is_some(), row.alive, "{what}: pooled control socket");
    }
    let closed = DeadPort::new();
    let mut session = Session::transient(proxy_at(&closed.at, false), T);
    let rand = StreamRand::with_key([0; 32]);
    let e = session.connect(&peer(0), &rand, &never).await.err().unwrap();
    assert_eq!(e, (true, format!("Cannot connect to {}", closed.at)));
    assert!(session.control_sock.is_none() && session.session_id.is_empty());
}

/// The lookup's `VALUE` goes into `STREAM CONNECT` byte for byte, as Core's `std::string`
/// carries it, even when it is not UTF-8.
// Core: i2p.cpp:247-251
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_destination_goes_back_unchanged() {
    let lookup = Step::Reply(b"NAMING REPLY RESULT=OK VALUE=\xff\xfe~-".to_vec());
    let (sam, proxy) = MockSam::start(vec![
        control(),
        vec![ok(HELLO_OK), lookup, ok("STREAM STATUS RESULT=CANT_REACH_PEER")],
    ])
    .await;
    let mut session = Session::transient(proxy, T);
    let rand = StreamRand::with_key([0; 32]);
    assert_eq!(connect(&mut session, &peer(0), &rand).await.err(), Some(false));
    let id = first_session_id();
    let expected =
        [b"STREAM CONNECT ID=", id.as_bytes(), b" DESTINATION=\xff\xfe~- SILENT=false\n"];
    assert_eq!(sam.raw_lines()[1][2], expected.concat());
}
