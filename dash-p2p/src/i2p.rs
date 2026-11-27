//! I2P through a SAM 3.1 proxy (Core's `i2p::sam::Session`, P8c): transient sessions for
//! outbound connections, as `ConnectNode` opens them when no persistent session exists
//! (`-i2pacceptincoming=0`). Requests are Core's, byte for byte; each reply is one line read
//! with Core's `RecvUntilTerminator`, and every failure carries Core's text into its log line.
//!
//! A session owns its control socket. A connection that succeeds keeps its session for the
//! connection's life ([`I2pSocket`]), as Core keeps it in `CNode::m_i2p_sam_session`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use dash_netaddr::I2P_SAM31_PORT;
use parking_lot::Mutex;
use tokio::io::Interest;
use tokio::time::Instant;

use crate::netaddr::{NetAddr, Network, Service};
use crate::netcore::connect_directly;
use crate::proxy::{Proxy, ProxyAddr};
use crate::rng::{RandSource, get_rand_hash};
use crate::sock::{NetSocket, TcpSocket, network_error_string, send_flags};

/// Core's `i2p::sam::MAX_MSG_SIZE`: the longest reply line read from the SAM proxy.
// Core: i2p.h:51
pub const MAX_MSG_SIZE: usize = 65536;

/// Core's `CConnman::MAX_UNUSED_I2P_SESSIONS_SIZE`: the transient sessions kept for reuse.
// Core: net.h:2057
pub const MAX_UNUSED_I2P_SESSIONS_SIZE: usize = 10;

/// Core's `MAX_WAIT_FOR_IO`.
// Core: util/sock.h:32
const MAX_WAIT_FOR_IO: Duration = Duration::from_secs(1);

/// How long a reply may take: the router may be querying the I2P network.
// Core: i2p.cpp:305
const RECV_TIMEOUT: Duration = Duration::from_secs(3 * 60);

/// Core's `std::runtime_error` text.
type Error = String;

/// An established connection with an I2P peer (Core's `i2p::Connection`).
pub struct Connection {
    /// The connected socket.
    pub sock: TcpSocket,
    /// Our I2P address.
    pub me: Service,
}

/// A parsed reply from the SAM proxy (Core's `Session::Reply`), kept as bytes as Core's
/// `std::string` keeps them: a value goes back to the proxy unchanged.
struct Reply {
    full: Vec<u8>,
    /// The request, for error texts (`SESSION CREATE ...` without its key).
    request: String,
    keys: HashMap<Vec<u8>, Option<Vec<u8>>>,
}

impl Reply {
    // Core: i2p.cpp:280-288
    fn get(&self, key: &str) -> Result<&[u8], Error> {
        match self.keys.get(key.as_bytes()) {
            Some(Some(value)) => Ok(value),
            _ => Err(format!("Missing {key}= in the reply to \"{}\"", self.request)),
        }
    }
}

/// Bytes from the proxy as log and error text.
fn text(bytes: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

/// Core's `IOErrorIsPermanent`: anything but a would-block or an interrupted call.
fn is_permanent(e: &io::Error) -> bool {
    !matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted)
}

/// Core's non-blocking `send` (`MSG_NOSIGNAL`) on the socket itself. Through the runtime when
/// it has seen the socket writable, so a `WouldBlock` clears that readiness; otherwise straight
/// to the socket, which the runtime may not have polled yet.
fn send_now(sock: &TcpSocket, data: &[u8]) -> io::Result<usize> {
    let send = || socket2::SockRef::from(sock.stream()).send_with_flags(data, send_flags(false));
    let mut called = false;
    let sent = sock.stream().try_io(Interest::WRITABLE, || {
        called = true;
        send()
    });
    match sent {
        Err(e) if !called && e.kind() == io::ErrorKind::WouldBlock => send(),
        sent => sent,
    }
}

/// Core's non-blocking `recv(MSG_PEEK)` on the socket itself, as [`send_now`] sends. `raw` is
/// a duplicate of `sock`'s descriptor: the same open file, so non-blocking too.
fn peek_now(sock: &TcpSocket, raw: &std::net::TcpStream, buf: &mut [u8]) -> io::Result<usize> {
    let mut called = false;
    let peeked = sock.stream().try_io(Interest::READABLE, || {
        called = true;
        raw.peek(buf)
    });
    match peeked {
        Err(e) if !called && e.kind() == io::ErrorKind::WouldBlock => raw.peek(buf),
        peeked => peeked,
    }
}

/// Core's `Sock::SendComplete`: every byte of `data` within `timeout`, waiting at most
/// [`MAX_WAIT_FOR_IO`] at a time and checking `interrupted` between tries.
// Core: util/sock.cpp:407-446
async fn send_complete(
    sock: &TcpSocket,
    data: &[u8],
    timeout: Duration,
    interrupted: &(dyn Fn() -> bool + Sync),
) -> Result<(), Error> {
    let deadline = Instant::now() + timeout;
    let mut sent = 0;
    loop {
        match send_now(sock, &data[sent..]) {
            Ok(n) if n > 0 => {
                sent += n;
                if sent == data.len() {
                    return Ok(());
                }
            }
            Err(e) if is_permanent(&e) => {
                return Err(format!("send(): {}", network_error_string(&e)));
            }
            _ => {}
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(format!(
                "Send timeout (sent only {sent} of {} bytes before that)",
                data.len()
            ));
        }
        if interrupted() {
            return Err(format!(
                "Send interrupted (sent only {sent} of {} bytes before that)",
                data.len()
            ));
        }
        let wait = (deadline - now).min(MAX_WAIT_FOR_IO);
        let _ = tokio::time::timeout(wait, sock.stream().writable()).await;
    }
}

/// Core's `Sock::RecvUntilTerminator`: one line, without its terminator, never reading past it.
/// Each turn peeks without waiting, then checks the deadline and `interrupted`, then waits at
/// most [`MAX_WAIT_FOR_IO`] for more bytes, in Core's order.
// Core: util/sock.cpp:448-529
async fn recv_until_terminator(
    sock: &TcpSocket,
    terminator: u8,
    timeout: Duration,
    interrupted: &(dyn Fn() -> bool + Sync),
    max_data: usize,
) -> Result<Vec<u8>, Error> {
    let deadline = Instant::now() + timeout;
    let recv_error = |e: &io::Error| format!("recv(): {}", network_error_string(e));
    let raw: std::net::TcpStream =
        socket2::SockRef::from(sock.stream()).try_clone().map_err(|e| recv_error(&e))?.into();
    let mut data = Vec::new();
    loop {
        if data.len() >= max_data {
            return Err(format!("Received too many bytes without a terminator ({})", data.len()));
        }
        let mut buf = [0u8; 512];
        let want = buf.len().min(max_data - data.len());
        match peek_now(sock, &raw, &mut buf[..want]) {
            Ok(0) => return Err("Connection unexpectedly closed by peer".into()),
            Ok(peeked) => {
                let at = buf[..peeked].iter().position(|&b| b == terminator);
                let try_len = at.map_or(peeked, |at| at + 1);
                match (&raw).read(&mut buf[..try_len]) {
                    Ok(read) if read == try_len => {}
                    read => {
                        let read = read.map_or(-1, |r| r as i64);
                        return Err(format!(
                            "recv() returned {read} bytes on attempt to read {try_len} bytes but previous peek claimed {peeked} bytes are available"
                        ));
                    }
                }
                data.extend_from_slice(&buf[..at.unwrap_or(peeked)]);
                if at.is_some() {
                    return Ok(data);
                }
            }
            Err(e) if is_permanent(&e) => return Err(recv_error(&e)),
            Err(_) => {}
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(format!(
                "Receive timeout (received {} bytes without terminator before that)",
                data.len()
            ));
        }
        if interrupted() {
            return Err(format!(
                "Receive interrupted (received {} bytes without terminator before that)",
                data.len()
            ));
        }
        let wait = (deadline - now).min(MAX_WAIT_FOR_IO);
        let _ = tokio::time::timeout(wait, sock.stream().readable()).await;
    }
}

/// Core's `Sock::IsConnected`: a non-blocking one-byte `recv(MSG_PEEK)` on the socket itself,
/// not the runtime's cached readiness; `Err` with Core's text when the other side closed or the
/// socket failed.
// Core: util/sock.cpp:531-554
fn is_connected(sock: &TcpSocket) -> Result<(), Error> {
    let mut byte = [std::mem::MaybeUninit::<u8>::uninit()];
    match socket2::SockRef::from(sock.stream()).peek(&mut byte) {
        Ok(0) => Err("closed".into()),
        Err(e) if is_permanent(&e) => Err(network_error_string(&e)),
        _ => Ok(()),
    }
}

/// Core's `SendRequestAndGetReply`: sends `request` and a newline, reads one reply line and
/// parses its `KEY=VALUE` words: split at every space, each at its first `=`, a word without
/// one has no value, and the first of a repeated key wins. With `check_result_ok`, a reply
/// without `RESULT=OK` is an error.
// Core: i2p.cpp:290-324
async fn send_request_and_get_reply(
    sock: &TcpSocket,
    request: &[u8],
    check_result_ok: bool,
    interrupted: &(dyn Fn() -> bool + Sync),
) -> Result<Reply, Error> {
    send_complete(sock, &[request, b"\n"].concat(), MAX_WAIT_FOR_IO, interrupted).await?;
    // Never log the whole `SESSION CREATE`: a persistent session's holds the private key.
    let request = if request.starts_with(b"SESSION CREATE") {
        "SESSION CREATE ...".to_owned()
    } else {
        text(request).into_owned()
    };
    let full = recv_until_terminator(sock, b'\n', RECV_TIMEOUT, interrupted, MAX_MSG_SIZE).await?;
    let mut keys = HashMap::new();
    for kv in full.split(|&b| b == b' ') {
        let (key, value) = match kv.iter().position(|&b| b == b'=') {
            Some(at) => (&kv[..at], Some(kv[at + 1..].to_vec())),
            None => (kv, None),
        };
        keys.entry(key.to_vec()).or_insert(value);
    }
    let reply = Reply {
        full,
        request,
        keys,
    };
    if check_result_ok && reply.get("RESULT")? != b"OK" {
        return Err(format!("Reply to \"{}\": had a RESULT not equal to OK.", reply.request));
    }
    Ok(reply)
}

/// Core's `DecodeI2PBase64`: Base64 with `-` and `~` for `+` and `/` (`SwapBase64`), decoded as
/// Core's `DecodeBase64` decodes (a multiple of four characters, at most two `=` at the end, no
/// leftover bits).
// Core: i2p.cpp:39-79; util/strencodings.cpp:139-172
fn decode_i2p_base64(i2p_b64: &[u8]) -> Result<Vec<u8>, Error> {
    let error = || format!("Cannot decode Base64: \"{}\"", text(i2p_b64));
    let mut s = i2p_b64;
    if !s.len().is_multiple_of(4) {
        return Err(error());
    }
    for _ in 0..2 {
        if let Some(rest) = s.strip_suffix(b"=") {
            s = rest;
        }
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in s {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'~' => 63,
            _ => return Err(error()),
        };
        acc = ((acc << 6) | u32::from(v)) & 0x1fff;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if bits >= 6 || (acc << (8 - bits)) & 0xff != 0 {
        return Err(error());
    }
    Ok(out)
}

/// Core's `DestBinToAddr`: the `.b32.i2p` address of a destination, its SHA-256.
// Core: i2p.cpp:87-101
fn dest_bin_to_addr(dest: &[u8]) -> NetAddr {
    NetAddr::new(Network::I2p, dash_sha256::sha256(dest).to_vec())
        .expect("a SHA-256 is an I2P address")
}

/// Core's `Session::MyDestination`: the destination at the head of a private key, 387 bytes and
/// the certificate length at bytes 385-386.
// Core: i2p.cpp:373-403
fn my_destination(private_key: &[u8]) -> Result<&[u8], Error> {
    const DEST_LEN_BASE: usize = 387;
    const CERT_LEN_POS: usize = 385;
    if private_key.len() < CERT_LEN_POS + 2 {
        return Err(format!(
            "The private key is too short ({} < {})",
            private_key.len(),
            CERT_LEN_POS + 2
        ));
    }
    let cert_len = u16::from_be_bytes([private_key[CERT_LEN_POS], private_key[CERT_LEN_POS + 1]]);
    let dest_len = DEST_LEN_BASE + usize::from(cert_len);
    if dest_len > private_key.len() {
        return Err(format!(
            "Certificate length ({cert_len}) designates that the private key should be {dest_len} bytes, but it is only {} bytes",
            private_key.len()
        ));
    }
    Ok(&private_key[..dest_len])
}

/// A transient I2P SAM session (Core's `i2p::sam::Session` built with its transient
/// constructor): created on first use, with a destination the router generates.
pub struct Session {
    /// The SAM proxy.
    control_host: Proxy,
    /// `-timeout`, for each connection to the proxy.
    connect_timeout: Duration,
    /// The private key (destination and keys) the router returned.
    private_key: Vec<u8>,
    /// The control socket the session lives on; `None` before creation and after
    /// [`Session::disconnect`].
    control_sock: Option<TcpSocket>,
    /// Our `.b32.i2p` address, port 0.
    my_addr: Service,
    /// The session id; empty when there is no session.
    session_id: String,
}

impl Session {
    /// A transient session through `control_host`, whose connections time out after
    /// `connect_timeout` (`-timeout`); no I/O until first used.
    // Core: i2p.cpp:127-132
    pub fn transient(control_host: Proxy, connect_timeout: Duration) -> Self {
        Self {
            control_host,
            connect_timeout,
            private_key: Vec::new(),
            control_sock: None,
            my_addr: Service::default(),
            session_id: String::new(),
        }
    }

    /// Core's `Session::Connect`: a connection to the I2P peer `to`, or `Err((proxy_error,
    /// why))`. `proxy_error` is `true` when the proxy or the network failed, and `false` when the
    /// peer could not be reached or `to` has a port other than 0 (refused without I/O). `why` is
    /// the text Core logs after "Error connecting to <to>". `rand` draws the session id;
    /// `interrupted` is Core's `interruptNet`.
    // Core: i2p.cpp:219-276
    pub(crate) async fn connect(
        &mut self,
        to: &Service,
        rand: &dyn RandSource,
        interrupted: &(dyn Fn() -> bool + Sync),
    ) -> Result<Connection, (bool, Error)> {
        // SAM 3.1 has no ports: the proxy uses 0.
        if to.port != I2P_SAM31_PORT {
            tracing::debug!(target: "i2p", "Error connecting to {to}, connection refused due to arbitrary port {}", to.port);
            return Err((false, format!("connection refused due to arbitrary port {}", to.port)));
        }
        let mut proxy_error = true;
        match self.try_connect(to, rand, interrupted, &mut proxy_error).await {
            Ok(conn) => Ok(conn),
            Err(e) => {
                tracing::debug!(target: "i2p", "Error connecting to {to}: {e}");
                self.check_control_sock();
                Err((proxy_error, e))
            }
        }
    }

    async fn try_connect(
        &mut self,
        to: &Service,
        rand: &dyn RandSource,
        interrupted: &(dyn Fn() -> bool + Sync),
        proxy_error: &mut bool,
    ) -> Result<Connection, Error> {
        self.create_if_not_created_already(rand, interrupted).await?;
        let session_id = self.session_id.clone();
        let me = self.my_addr.clone();
        let sock = self.hello(interrupted).await?;
        let lookup = format!("NAMING LOOKUP NAME={}", to.addr);
        let lookup_reply =
            send_request_and_get_reply(&sock, lookup.as_bytes(), true, interrupted).await?;
        let dest = lookup_reply.get("VALUE")?;
        let request = [
            b"STREAM CONNECT ID=",
            session_id.as_bytes(),
            b" DESTINATION=",
            dest,
            b" SILENT=false",
        ]
        .concat();
        let connect_reply = send_request_and_get_reply(&sock, &request, false, interrupted).await?;
        match connect_reply.get("RESULT")? {
            b"OK" => {
                return Ok(Connection {
                    sock,
                    me,
                });
            }
            b"INVALID_ID" => {
                self.disconnect();
                return Err("Invalid session id".into());
            }
            b"CANT_REACH_PEER" | b"TIMEOUT" => *proxy_error = false,
            _ => {}
        }
        Err(format!("\"{}\"", text(&connect_reply.full)))
    }

    /// Core's `Session::Hello`: a new connection to the SAM proxy (`Proxy::Connect`, a manual
    /// connection for the logs) that has greeted it. Core takes `-i2psam` as an address only, so
    /// a UNIX socket proxy is never connected.
    // Core: i2p.cpp:326-337; netbase.cpp:652-656
    async fn hello(&self, interrupted: &(dyn Fn() -> bool + Sync)) -> Result<TcpSocket, Error> {
        let stream = match &self.control_host.addr {
            ProxyAddr::Service(service) if self.control_host.is_valid() => {
                connect_directly(service, true, self.connect_timeout).await
            }
            _ => None,
        };
        let Some(sock) = stream.and_then(|s| TcpSocket::new(s).ok()) else {
            return Err(format!("Cannot connect to {}", self.control_host));
        };
        send_request_and_get_reply(&sock, b"HELLO VERSION MIN=3.1 MAX=3.1", true, interrupted)
            .await?;
        Ok(sock)
    }

    /// Core's `CheckControlSock`: a control socket the proxy closed, or that failed, ends the
    /// session.
    // Core: i2p.cpp:339-348
    fn check_control_sock(&mut self) {
        let Some(sock) = &self.control_sock else {
            return;
        };
        if let Err(e) = is_connected(sock) {
            tracing::debug!(target: "i2p", "Control socket error: {e}");
            self.disconnect();
        }
    }

    /// Core's `CreateIfNotCreatedAlready` for a transient session: unless the control socket is
    /// still connected, a new session id (ten hex digits of a random hash), a greeting and
    /// `SESSION CREATE` with a transient destination, whose private key the reply carries.
    // Core: i2p.cpp:405-456
    async fn create_if_not_created_already(
        &mut self,
        rand: &dyn RandSource,
        interrupted: &(dyn Fn() -> bool + Sync),
    ) -> Result<(), Error> {
        if let Some(sock) = &self.control_sock
            && is_connected(sock).is_ok()
        {
            return Ok(());
        }
        // `uint256::GetHex` writes the bytes in reverse.
        let hash = get_rand_hash(rand);
        let session_id: String = hash.iter().rev().take(5).map(|b| format!("{b:02x}")).collect();
        tracing::debug!(target: "i2p", "Creating transient SAM session {session_id} with {}", self.control_host);
        let sock = self.hello(interrupted).await?;
        let request = format!(
            "SESSION CREATE STYLE=STREAM ID={session_id} DESTINATION=TRANSIENT SIGNATURE_TYPE=7 i2cp.leaseSetEncType=4,0 inbound.quantity=1 outbound.quantity=1"
        );
        let reply =
            send_request_and_get_reply(&sock, request.as_bytes(), true, interrupted).await?;
        self.private_key = decode_i2p_base64(reply.get("DESTINATION")?)?;
        self.my_addr =
            Service::new(dest_bin_to_addr(my_destination(&self.private_key)?), I2P_SAM31_PORT);
        self.session_id = session_id;
        self.control_sock = Some(sock);
        tracing::info!(target: "i2p", "Transient SAM session {} created, my address={}", self.session_id, self.my_addr);
        Ok(())
    }

    /// Core's `Session::Disconnect`: closes the control socket, which ends the session at the
    /// proxy.
    // Core: i2p.cpp:479-490
    fn disconnect(&mut self) {
        if self.control_sock.take().is_some() {
            if self.session_id.is_empty() {
                tracing::info!(target: "i2p", "Destroying incomplete SAM session");
            } else {
                tracing::info!(target: "i2p", "Destroying SAM session {}", self.session_id);
            }
        }
        self.session_id.clear();
    }
}

impl Drop for Session {
    // Core: i2p.cpp:134-138
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// A connection to an I2P peer: the SAM stream and the transient session it belongs to, which
/// [`NetSocket::close`] destroys with the socket (Core's `m_i2p_sam_session.reset()` in
/// `CloseSocketDisconnect`).
pub struct I2pSocket {
    sock: TcpSocket,
    session: Mutex<Option<Session>>,
}

impl I2pSocket {
    /// `conn`'s socket, holding `session` until it closes.
    pub fn new(conn: Connection, session: Session) -> Self {
        Self {
            sock: conn.sock,
            session: Mutex::new(Some(session)),
        }
    }
}

impl NetSocket for I2pSocket {
    fn poll_read_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.sock.poll_read_ready(cx)
    }

    fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.sock.try_read(buf)
    }

    fn poll_peek(&self, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        self.sock.poll_peek(cx)
    }

    fn poll_write_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.sock.poll_write_ready(cx)
    }

    fn try_send(&self, buf: &[u8], more: bool) -> io::Result<usize> {
        self.sock.try_send(buf, more)
    }

    fn shutdown_write(&self) -> io::Result<()> {
        self.sock.shutdown_write()
    }

    // Core: net.cpp:546-548
    fn close(&self) {
        self.sock.close();
        drop(self.session.lock().take());
    }
}

/// Core's `m_unused_i2p_sessions`: transient sessions whose connection failed, reused before a
/// new one is created ("creating a session in I2P is not cheap").
// Core: net.h:2019-2026
#[derive(Default)]
pub(crate) struct UnusedSessions(Mutex<std::collections::VecDeque<Session>>);

impl UnusedSessions {
    /// The oldest unused session, or a new transient one through `proxy`.
    // Core: net.cpp:444-453
    pub(crate) fn take_or_create(&self, proxy: &Proxy, connect_timeout: Duration) -> Session {
        let unused = self.0.lock().pop_front();
        unused.unwrap_or_else(|| Session::transient(proxy.clone(), connect_timeout))
    }

    /// Keeps `session` for reuse while fewer than [`MAX_UNUSED_I2P_SESSIONS_SIZE`] wait; drops it
    /// otherwise.
    // Core: net.cpp:455-460
    pub(crate) fn put_back(&self, session: Session) {
        let mut pool = self.0.lock();
        if pool.len() < MAX_UNUSED_I2P_SESSIONS_SIZE {
            pool.push_back(session);
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.lock().len()
    }
}

/// An [`Arc`]'d [`I2pSocket`] as a peer socket.
pub(crate) fn peer_socket(conn: Connection, session: Session) -> Arc<dyn NetSocket> {
    Arc::new(I2pSocket::new(conn, session))
}

#[cfg(test)]
#[path = "i2p_tests.rs"]
mod tests;
