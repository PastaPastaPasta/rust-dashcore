//! Core's HTTP server (`httpserver.cpp`) without libevent: the `-rpcallowip` list, the
//! `-rpcbind` listeners, one event thread (Core's `ThreadHTTP`) that accepts every connection,
//! reads requests as `evhttp` reads them ([`parse`]) and writes replies as it writes them
//! ([`reply`]), and the work queue whose `-rpcthreads` workers run the handlers.
//!
//! The event thread waits on every socket at once with non-blocking I/O (mio, RP0-D7), so an
//! idle or half-sent connection costs its descriptor and buffers, never a thread. A connection
//! handles one request at a time, as `evhttp` does: the thread reads a request, hands it to a
//! worker, and reads the next one once the worker's reply is written.

pub mod parse;
pub mod reply;

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use mio::{Events, Interest, Poll, Token, Waker};
use socket2::{Domain, Protocol, Socket, Type};

pub use parse::Method;
use parse::{Failure, Parser, Request};
use reply::Peer;

use crate::auth::{decode_base64, trim};
use crate::net::{
    Resolve, Subnet, addr_port_string, canonical, is_bind_any, lookup_subnet, split_host_port,
};
use crate::protocol::*;
use crate::request::sanitize_uri;

/// `-rpcthreads` default.
pub const DEFAULT_HTTP_THREADS: i64 = 4;
/// `-rpcworkqueue` and `-rpcexternalworkqueue` default.
pub const DEFAULT_HTTP_WORKQUEUE: i64 = 16;
/// `-rpcservertimeout` default, in seconds.
pub const DEFAULT_HTTP_SERVER_TIMEOUT: i64 = 30;

/// A request handler: the request and the URI after the handler's prefix.
pub type Handler = Arc<dyn Fn(HttpRequest, &str) + Send + Sync>;

/// Whether the node is shutting down (Core's `ShutdownRequested`): replies then close the
/// connection.
pub type ShutdownRequested = Arc<dyn Fn() -> bool + Send + Sync>;

/// Why the server could not start (`InitHTTPServer` returning false; the cause is logged).
#[derive(Debug, PartialEq, Eq)]
pub enum InitError {
    /// An `-rpcallowip` value is not a subnet: the message box text Core shows.
    AllowIp(String),
    /// No address could be bound.
    Bind,
}

struct PathHandler {
    prefix: String,
    exact: bool,
    handler: Handler,
}

/// What a worker sends back to the connection: status, headers, body.
struct Reply(u16, Vec<(String, String)>, Vec<u8>);

/// Where a request's reply goes: the event thread, for connection `conn`.
struct ReplyTo {
    conn: usize,
    sender: mpsc::Sender<(usize, Reply)>,
    waker: Arc<Waker>,
}

/// A request handed to a handler (Core's `HTTPRequest`). Dropping it without a reply answers
/// `500 Unhandled request`.
pub struct HttpRequest {
    method: Method,
    uri: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    peer: SocketAddr,
    out_headers: Vec<(String, String)>,
    reply: Option<ReplyTo>,
    shutdown_requested: ShutdownRequested,
}

impl HttpRequest {
    pub fn method(&self) -> Method {
        self.method
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// The client's address.
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// `GetPeer().ToStringAddrPort()`.
    pub fn peer_string(&self) -> String {
        addr_port_string(&self.peer)
    }

    /// `GetHeader`: the first header named `name`, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        parse::find_header(&self.headers, name)
    }

    /// `ReadBody`: the body, once.
    pub fn read_body(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.body)
    }

    /// `WriteHeader`: a reply header, after those written before.
    pub fn write_header(&mut self, name: &str, value: &str) {
        self.out_headers.push((name.to_owned(), value.to_owned()));
    }

    /// `WriteReply`. A reply while the node shuts down closes the connection.
    // Core: httpserver.cpp:676-705
    pub fn write_reply(&mut self, status: u16, body: &[u8]) {
        let to = self.reply.take().expect("one reply per request");
        if (self.shutdown_requested)() {
            self.write_header("Connection", "close");
        }
        // The server may be gone; the reply is then dropped, as `evhttp` drops it.
        let reply = Reply(status, std::mem::take(&mut self.out_headers), body.to_vec());
        if to.sender.send((to.conn, reply)).is_ok() {
            let _ = to.waker.wake();
        }
    }
}

impl Drop for HttpRequest {
    fn drop(&mut self) {
        if self.reply.is_some() {
            tracing::info!("~HTTPRequest: Unhandled request");
            self.write_reply(HTTP_INTERNAL_SERVER_ERROR, b"Unhandled request");
        }
    }
}

/// A request and the handler to run it (Core's `HTTPWorkItem`).
struct WorkItem {
    req: HttpRequest,
    path: String,
    handler: Handler,
}

/// Core's `WorkQueue`, with Dash's second queue for `-rpcexternaluser` requests, which run
/// when no other request waits.
struct WorkQueue {
    state: Mutex<QueueState>,
    cond: Condvar,
    max_depth: usize,
    external_depth: usize,
}

struct QueueState {
    queue: VecDeque<Box<WorkItem>>,
    external: VecDeque<Box<WorkItem>>,
    running: bool,
}

impl WorkQueue {
    fn new(max_depth: usize, external_depth: usize) -> WorkQueue {
        WorkQueue {
            state: Mutex::new(QueueState {
                queue: VecDeque::new(),
                external: VecDeque::new(),
                running: true,
            }),
            cond: Condvar::new(),
            max_depth,
            external_depth,
        }
    }

    /// Queues `item`, or gives it back when the queue is full or interrupted.
    fn enqueue(&self, item: Box<WorkItem>, external: bool) -> Result<(), Box<WorkItem>> {
        let mut state = self.state.lock().expect("not poisoned");
        if !state.running {
            return Err(item);
        }
        let (queue, depth) = if external {
            (&mut state.external, self.external_depth)
        } else {
            (&mut state.queue, self.max_depth)
        };
        if queue.len() >= depth {
            return Err(item);
        }
        queue.push_back(item);
        self.cond.notify_one();
        Ok(())
    }

    /// A worker: runs items until the queue is interrupted and empty.
    fn run(&self) {
        loop {
            let item = {
                let mut state = self.state.lock().expect("not poisoned");
                while state.running && state.queue.is_empty() && state.external.is_empty() {
                    state = self.cond.wait(state).expect("not poisoned");
                }
                if let Some(item) = state.queue.pop_front() {
                    item
                } else if let Some(item) = state.external.pop_front() {
                    tracing::info!("HTTP: Calling handler for external user...");
                    item
                } else {
                    return;
                }
            };
            // Core's worker dies with the process on an uncaught exception; here the request is
            // answered "Unhandled request" as it unwinds and the worker carries on (RP0-D12), so
            // a later stop never waits on a queue nobody runs.
            let WorkItem {
                req,
                path,
                handler,
            } = *item;
            if std::panic::catch_unwind(AssertUnwindSafe(|| handler(req, &path))).is_err() {
                tracing::info!("Error: an RPC handler panicked");
            }
        }
    }

    fn interrupt(&self) {
        self.state.lock().expect("not poisoned").running = false;
        self.cond.notify_all();
    }
}

/// What the event thread and the workers share.
struct Shared {
    handlers: RwLock<Vec<PathHandler>>,
    allow: Vec<Subnet>,
    queue: WorkQueue,
    external_users: Vec<String>,
    /// The idle timeout of a connection: `None` for none, zero to close it at once.
    timeout: Option<Duration>,
    shutdown_requested: ShutdownRequested,
    /// Set by `interrupt`: requests are answered `503` (`http_reject_request_cb`).
    rejecting: AtomicBool,
    /// Set by `stop`: the event thread closes the listeners, waits for the requests in flight
    /// and returns.
    stopping: AtomicBool,
    /// Wakes the event thread for a reply or `stop`.
    waker: Arc<Waker>,
    replies: mpsc::Sender<(usize, Reply)>,
}

impl Shared {
    fn reply_to(&self, conn: usize) -> ReplyTo {
        ReplyTo {
            conn,
            sender: self.replies.clone(),
            waker: Arc::clone(&self.waker),
        }
    }
}

/// The HTTP server: init binds, `start` serves, `interrupt` refuses new requests, `stop`
/// waits for those in flight and closes everything.
pub struct HttpServer {
    shared: Arc<Shared>,
    /// The event thread's state, until `start` hands it over.
    events: Option<EventLoop>,
    bound: Vec<SocketAddr>,
    threads: i64,
    workers: Vec<JoinHandle<()>>,
    event_thread: Option<JoinHandle<()>>,
}

impl HttpServer {
    /// Core's `InitHTTPServer`: the allow list, the listeners and the work queue.
    // Core: httpserver.cpp:456-508
    pub fn init(
        args: &ArgsManager,
        resolve: &Resolve,
        shutdown_requested: ShutdownRequested,
    ) -> Result<HttpServer, InitError> {
        let allow = init_allow_list(args, resolve)?;
        // `evhttp_set_timeout` takes an `int`: Core's 64-bit value is cut to its low 32 bits.
        let timeout =
            args.get_int_arg("-rpcservertimeout", DEFAULT_HTTP_SERVER_TIMEOUT).unwrap_or(0) as i32;
        let (listeners, bound) = bind_addresses(args, resolve);
        if listeners.is_empty() {
            tracing::info!("Unable to bind any endpoint for RPC server");
            return Err(InitError::Bind);
        }
        let poll = Poll::new().map_err(|e| {
            tracing::info!("couldn't create evhttp. Exiting. ({e})");
            InitError::Bind
        })?;
        let waker = Waker::new(poll.registry(), WAKE).map_err(|e| {
            tracing::info!("couldn't create evhttp. Exiting. ({e})");
            InitError::Bind
        })?;
        tracing::debug!(target: "http", "Initialized HTTP server");
        let int_arg = |name, default| args.get_int_arg(name, default).unwrap_or(default);
        let depth = int_arg("-rpcworkqueue", DEFAULT_HTTP_WORKQUEUE).max(1);
        let (mut external_depth, mut external_users) = (0, Vec::new());
        let external = args.get_arg("-rpcexternaluser", "").unwrap_or_default();
        if !external.is_empty() {
            external_depth = int_arg("-rpcexternalworkqueue", DEFAULT_HTTP_WORKQUEUE).max(1);
            external_users = external.split(',').map(str::to_owned).collect();
        }
        tracing::debug!(
            target: "http",
            "creating work queue of depth {depth} external_depth {external_depth}"
        );
        let (replies, receiver) = mpsc::channel();
        Ok(HttpServer {
            shared: Arc::new(Shared {
                handlers: RwLock::new(Vec::new()),
                allow,
                queue: WorkQueue::new(depth as usize, external_depth as usize),
                external_users,
                // `evhttp_set_timeout`: -1 and 0 leave connections without a timeout, a value
                // below -1 is a timeout already past, which closes each connection at once
                // (dashd v24 probed: no close in 200 s at 0 and -1, at once at -5).
                timeout: match timeout {
                    -1..=0 => None,
                    t if t < -1 => Some(Duration::ZERO),
                    t => Some(Duration::from_secs(t as u64)),
                },
                shutdown_requested,
                rejecting: AtomicBool::new(false),
                stopping: AtomicBool::new(false),
                waker: Arc::new(waker),
                replies,
            }),
            events: Some(EventLoop {
                poll,
                next: listeners.len(),
                listeners,
                replies: receiver,
                conns: BTreeMap::new(),
                stalled: false,
            }),
            bound,
            threads: int_arg("-rpcthreads", DEFAULT_HTTP_THREADS).max(1),
            workers: Vec::new(),
            event_thread: None,
        })
    }

    /// The addresses listened on.
    pub fn local_addrs(&self) -> &[SocketAddr] {
        &self.bound
    }

    /// `-rpcexternaluser`'s names.
    pub fn external_users(&self) -> &[String] {
        &self.shared.external_users
    }

    /// `RegisterHTTPHandler`.
    pub fn register_handler(
        &self,
        prefix: &str,
        exact: bool,
        handler: impl Fn(HttpRequest, &str) + Send + Sync + 'static,
    ) {
        tracing::debug!(target: "http", "Registering HTTP handler for {prefix} (exactmatch {})", u8::from(exact));
        self.shared.handlers.write().expect("not poisoned").push(PathHandler {
            prefix: prefix.to_owned(),
            exact,
            handler: Arc::new(handler),
        });
    }

    /// `UnregisterHTTPHandler`.
    pub fn unregister_handler(&self, prefix: &str, exact: bool) {
        let mut handlers = self.shared.handlers.write().expect("not poisoned");
        if let Some(i) = handlers.iter().position(|h| h.prefix == prefix && h.exact == exact) {
            tracing::debug!(target: "http", "Unregistering HTTP handler for {prefix} (exactmatch {})", u8::from(exact));
            handlers.remove(i);
        }
    }

    /// `StartHTTPServer`: the workers and the event thread.
    pub fn start(&mut self) {
        tracing::info!("Starting HTTP server with {} worker threads", self.threads);
        for i in 0..self.threads {
            let shared = Arc::clone(&self.shared);
            self.workers.push(spawn(format!("httpworker.{i}"), move || shared.queue.run()));
        }
        if let Some(events) = self.events.take() {
            let shared = Arc::clone(&self.shared);
            self.event_thread = Some(spawn("http".into(), move || events.run(&shared)));
        }
    }

    /// `InterruptHTTPServer`: requests are refused with `503` and the workers finish what is
    /// queued.
    pub fn interrupt(&self) {
        tracing::debug!(target: "http", "Interrupting HTTP server");
        self.shared.rejecting.store(true, Ordering::SeqCst);
        self.shared.queue.interrupt();
    }

    /// `StopHTTPServer`: joins the workers, then the event thread closes the listeners, waits
    /// for the requests in flight and closes every connection.
    // Core: httpserver.cpp:556-595
    pub fn stop(&mut self) {
        tracing::debug!(target: "http", "Stopping HTTP server");
        // Idempotent: the workers only exit once the queue is interrupted.
        self.shared.queue.interrupt();
        tracing::debug!(target: "http", "Waiting for HTTP worker threads to exit");
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        self.shared.stopping.store(true, Ordering::SeqCst);
        let _ = self.shared.waker.wake();
        tracing::debug!(target: "http", "Waiting for HTTP event thread to exit");
        if let Some(thread) = self.event_thread.take() {
            let _ = thread.join();
        }
        // A server never started closes its listeners here.
        self.events = None;
        tracing::debug!(target: "http", "Stopped HTTP server");
    }
}

fn spawn(name: String, f: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
    std::thread::Builder::new().name(name).spawn(f).expect("a thread starts")
}

/// Core's `InitHTTPAllowList`: `127.0.0.0/8` and `::1` always, then `-rpcallowip`.
// Core: httpserver.cpp:235-255
fn init_allow_list(args: &ArgsManager, resolve: &Resolve) -> Result<Vec<Subnet>, InitError> {
    let mut allow = vec![
        Subnet::with_prefix(Ipv4Addr::LOCALHOST.into(), 8).expect("a valid prefix"),
        Subnet::single(Ipv6Addr::LOCALHOST.into()),
    ];
    for text in args.get_args("-rpcallowip").unwrap_or_default() {
        let Some(subnet) = lookup_subnet(&text, resolve) else {
            return Err(InitError::AllowIp(format!(
                "Invalid -rpcallowip subnet specification: {text}. Valid are a single IP (e.g. 1.2.3.4), a network/netmask (e.g. 1.2.3.4/255.255.255.0) or a network/CIDR (e.g. 1.2.3.4/24)."
            )));
        };
        allow.push(subnet);
    }
    let allowed: String = allow.iter().map(|s| format!("{s} ")).collect();
    tracing::debug!(target: "http", "Allowing HTTP connections from: {allowed}");
    Ok(allow)
}

/// Core's `HTTPBindAddresses`: loopback unless both `-rpcallowip` and `-rpcbind` are set.
// Core: httpserver.cpp:393-436
fn bind_addresses(
    args: &ArgsManager,
    resolve: &Resolve,
) -> (Vec<mio::net::TcpListener>, Vec<SocketAddr>) {
    let port = args
        .get_int_arg("-rpcport", i64::from(args.base_params().rpc_port()))
        .unwrap_or_default() as u16;
    let (allowip, rpcbind) = (args.is_arg_set("-rpcallowip"), args.is_arg_set("-rpcbind"));
    let endpoints: Vec<(String, u16)> = if !(allowip && rpcbind) {
        if allowip {
            tracing::info!(
                "WARNING: option -rpcallowip was specified without -rpcbind; this doesn't usually make sense"
            );
        }
        if rpcbind {
            tracing::info!(
                "WARNING: option -rpcbind was ignored because -rpcallowip was not specified, refusing to allow everyone to connect"
            );
        }
        vec![("::1".into(), port), ("127.0.0.1".into(), port)]
    } else {
        args.get_args("-rpcbind")
            .unwrap_or_default()
            .iter()
            .map(|b| split_host_port(b, port))
            .collect()
    };
    let mut listeners = Vec::new();
    let mut bound = Vec::new();
    for (host, port) in endpoints {
        tracing::info!("Binding RPC on address {host} port {port}");
        match bind(&host, port, resolve) {
            Some(listener) => {
                if host.is_empty() || resolve(&host, false).is_some_and(is_bind_any) {
                    tracing::info!(
                        "WARNING: the RPC server is not safe to expose to untrusted networks such as the public internet"
                    );
                }
                bound.push(listener.local_addr().expect("a bound listener has an address"));
                listeners.push(listener);
            }
            None => tracing::info!("Binding RPC on address {host} port {port} failed."),
        }
    }
    (listeners, bound)
}

/// `evhttp_bind_socket_with_handle`: an IPv6 listener takes IPv6 only, the address is reusable,
/// and accepting does not block.
fn bind(host: &str, port: u16, resolve: &Resolve) -> Option<mio::net::TcpListener> {
    let ip = if host.is_empty() {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        resolve(host, true)?
    };
    let addr = SocketAddr::new(ip, port);
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP)).ok()?;
    if addr.is_ipv6() {
        socket.set_only_v6(true).ok()?;
    }
    socket.set_reuse_address(true).ok()?;
    socket.bind(&addr.into()).ok()?;
    socket.listen(128).ok()?;
    socket.set_nonblocking(true).ok()?;
    Some(mio::net::TcpListener::from_std(TcpListener::from(socket)))
}

/// The token of the event thread's waker; a listener's token is its index, and a connection's
/// is the next number after the listeners', never reused, so a late reply finds no connection
/// rather than another one.
const WAKE: Token = Token(usize::MAX);

/// A connection keeps buffers up to this size between requests; a larger one, left by a large
/// body or reply, is given back once its request is done, as libevent frees its buffers.
const BUFFER_KEEP: usize = 64 * 1024;

/// The event thread's state (Core's `eventBase` and `eventHTTP`).
struct EventLoop {
    poll: Poll,
    listeners: Vec<mio::net::TcpListener>,
    replies: mpsc::Receiver<(usize, Reply)>,
    conns: BTreeMap<usize, Conn>,
    /// The next connection's token.
    next: usize,
    /// An accept failed for want of descriptors: connections may wait in a backlog that no new
    /// readiness event will announce.
    stalled: bool,
}

/// A connection, as the event thread keeps it.
struct Conn {
    stream: mio::net::TcpStream,
    peer: SocketAddr,
    parser: Parser,
    /// Bytes to write, of which the first `sent` are written.
    out: Vec<u8>,
    sent: usize,
    /// The request a worker is answering, as its reply is to be written.
    awaiting: Option<Peer>,
    /// Close once `out` is written.
    closing: bool,
    /// The client closed its side.
    eof: bool,
    /// The last read or write, which the idle timeout counts from.
    active: Instant,
}

impl EventLoop {
    /// Core's `ThreadHTTP`: until `stop`, then until no request is in flight.
    fn run(mut self, shared: &Shared) {
        let mut events = Events::with_capacity(1024);
        for (i, listener) in self.listeners.iter_mut().enumerate() {
            let _ = self.poll.registry().register(listener, Token(i), Interest::READABLE);
        }
        loop {
            let due = shared
                .timeout
                .and_then(|t| self.conns.values().filter_map(|c| c.deadline(t)).min());
            let wait = due.map(|due| due.saturating_duration_since(Instant::now()));
            if let Err(e) = self.poll.poll(&mut events, wait)
                && e.kind() != io::ErrorKind::Interrupted
            {
                tracing::info!("HTTP: event loop failed: {e}");
                return;
            }
            for event in &events {
                let Token(token) = event.token();
                if event.token() == WAKE {
                    continue;
                }
                if token < self.listeners.len() {
                    self.accept(shared, token);
                } else {
                    self.drive(shared, token);
                }
            }
            // The replies there are now: one sent while these are handled (a pipelined request
            // refused at once) waits for the next round, so no connection holds the thread.
            let replies: Vec<_> = self.replies.try_iter().collect();
            for (token, reply) in replies {
                if let Some(conn) = self.conns.get_mut(&token) {
                    conn.reply(reply);
                    self.drive(shared, token);
                }
            }
            if let Some(t) = shared.timeout {
                let now = Instant::now();
                let expired: Vec<usize> = self
                    .conns
                    .iter()
                    .filter(|(_, c)| c.deadline(t).is_some_and(|d| d <= now))
                    .map(|(token, _)| *token)
                    .collect();
                for token in expired {
                    self.close(token);
                }
            }
            if self.stalled {
                self.stalled = false;
                for i in 0..self.listeners.len() {
                    self.accept(shared, i);
                }
            }
            if shared.stopping.load(Ordering::SeqCst) {
                if !self.listeners.is_empty() {
                    self.listeners.clear();
                    let active = self.conns.values().filter(|c| c.in_flight()).count();
                    if active != 0 {
                        tracing::debug!(target: "http", "Waiting for {active} connections to stop HTTP server");
                    }
                }
                if !self.conns.values().any(Conn::in_flight) {
                    for token in self.conns.keys().copied().collect::<Vec<_>>() {
                        self.close(token);
                    }
                    return;
                }
            }
        }
    }

    /// Takes every connection waiting on listener `i`.
    fn accept(&mut self, shared: &Shared, i: usize) {
        loop {
            let (mut stream, peer) = match self.listeners[i].accept() {
                Ok(accepted) => accepted,
                Err(e) => match e.kind() {
                    io::ErrorKind::WouldBlock => return,
                    io::ErrorKind::Interrupted
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::ConnectionReset => continue,
                    // Out of descriptors: try again once a round has passed.
                    _ => {
                        tracing::debug!(target: "http", "HTTP: cannot accept a connection: {e}");
                        self.stalled = true;
                        return;
                    }
                },
            };
            // A timeout already past (`-rpcservertimeout` below -1) closes it at once.
            if shared.timeout == Some(Duration::ZERO) {
                continue;
            }
            let token = self.next;
            self.next += 1;
            let interest = Interest::READABLE | Interest::WRITABLE;
            if let Err(e) = self.poll.registry().register(&mut stream, Token(token), interest) {
                tracing::info!("HTTP: cannot serve a connection from {peer}: {e}");
                continue;
            }
            self.conns.insert(
                token,
                Conn {
                    stream,
                    peer,
                    parser: Parser::new(),
                    out: Vec::new(),
                    sent: 0,
                    awaiting: None,
                    closing: false,
                    eof: false,
                    active: Instant::now(),
                },
            );
        }
    }

    /// Moves connection `token` on, closing it when it is done.
    fn drive(&mut self, shared: &Shared, token: usize) {
        if let Some(conn) = self.conns.get_mut(&token)
            && !drive(shared, token, conn)
        {
            self.close(token);
        }
    }

    fn close(&mut self, token: usize) {
        if let Some(mut conn) = self.conns.remove(&token) {
            let _ = self.poll.registry().deregister(&mut conn.stream);
            let _ = conn.stream.shutdown(Shutdown::Both);
        }
    }
}

impl Conn {
    /// A worker's reply to the request in flight.
    fn reply(&mut self, Reply(status, headers, body): Reply) {
        let Some(peer) = self.awaiting.take() else {
            return;
        };
        let (bytes, close) = reply::render(&peer, status, headers, &body);
        self.out.extend_from_slice(&bytes);
        self.closing |= close;
        self.active = Instant::now();
    }

    /// Whether a request is in flight until its reply is written (Core's `HTTPRequestTracker`
    /// counts a request until `evhttp` has sent it).
    fn in_flight(&self) -> bool {
        self.awaiting.is_some() || self.sent < self.out.len()
    }

    /// When the idle timeout `t` closes the connection: never while a worker answers it, nor
    /// when the time is past what a clock holds.
    fn deadline(&self, t: Duration) -> Option<Instant> {
        if self.awaiting.is_some() {
            return None;
        }
        self.active.checked_add(t)
    }
}

/// Moves a connection on as far as it goes without blocking: writes what is pending, then reads
/// requests and hands them to the workers. `false` when the connection is to be closed.
fn drive(shared: &Shared, token: usize, conn: &mut Conn) -> bool {
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if conn.sent < conn.out.len() {
            match conn.stream.write(&conn.out[conn.sent..]) {
                Ok(0) => return false,
                Ok(n) => {
                    conn.sent += n;
                    conn.active = Instant::now();
                    if conn.sent == conn.out.len() {
                        conn.sent = 0;
                        conn.out.clear();
                        conn.out.shrink_to(BUFFER_KEEP);
                    }
                    continue;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return true,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
        }
        if conn.closing {
            return false;
        }
        if conn.awaiting.is_some() {
            return true;
        }
        let out = &mut conn.out;
        let next = conn.parser.next(conn.eof, |req, buffered| {
            // `evhttp_send_continue` when the client waits for it.
            if !buffered {
                out.extend_from_slice(
                    format!("HTTP/{}.{} 100 Continue\r\n\r\n", req.major, req.minor).as_bytes(),
                );
            }
        });
        match next {
            Some(Ok(req)) => handle(shared, token, conn, req),
            Some(Err(Failure::Closed)) => return false,
            Some(Err(failure)) => {
                conn.out.extend_from_slice(&failure_page(failure));
                conn.closing = true;
            }
            // A `100 Continue` to write first.
            None if !conn.out.is_empty() => {}
            None => match conn.stream.read(&mut chunk) {
                Ok(0) => conn.eof = true,
                Ok(n) => {
                    conn.parser.feed(&chunk[..n]);
                    conn.active = Instant::now();
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return true,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            },
        }
    }
}

/// The error page for a request that could not be read; the connection closes after it.
fn failure_page(failure: Failure) -> Vec<u8> {
    let (peer, status) = match failure {
        Failure::Closed => return Vec::new(),
        Failure::BadRequest {
            major,
            minor,
        } => {
            let peer = Peer {
                major,
                minor,
                ..Peer::default()
            };
            (peer, HTTP_BAD_REQUEST)
        }
        Failure::NotImplemented(req) => (Peer::of(&req), 501),
        Failure::ExpectationFailed(req) => (Peer::of(&req), 417),
        Failure::TooLarge(req) => (Peer::of(&req), 413),
    };
    reply::error_page(&peer, status)
}

/// A request read off connection `token`: refused while the server is interrupted, otherwise
/// dispatched, its reply to come back to the connection.
fn handle(shared: &Shared, token: usize, conn: &mut Conn, req: Request) {
    let peer = Peer::of(&req);
    if shared.rejecting.load(Ordering::SeqCst) {
        tracing::debug!(target: "http", "Rejecting request while shutting down");
        conn.out.extend_from_slice(&reply::error_page(&peer, HTTP_SERVICE_UNAVAILABLE));
        conn.closing = true;
        return;
    }
    conn.awaiting = Some(peer);
    dispatch(shared, req, conn.peer, shared.reply_to(token));
}

/// Core's `http_request_cb`: the allow list, the method, the handler, the work queue. The reply
/// goes to `reply`, from here or from a worker.
// Core: httpserver.cpp:276-374
fn dispatch(shared: &Shared, req: Request, peer: SocketAddr, reply: ReplyTo) {
    let mut hreq = HttpRequest {
        method: req.method,
        uri: req.uri,
        headers: req.headers,
        body: req.body,
        peer,
        out_headers: Vec::new(),
        reply: Some(reply),
        shutdown_requested: Arc::clone(&shared.shutdown_requested),
    };
    let peer_text = addr_port_string(&peer);
    if !shared.allow.iter().any(|s| s.matches(canonical(peer.ip()))) {
        tracing::debug!(target: "http", "HTTP request from {peer_text} rejected: Client network is not allowed RPC access");
        hreq.write_reply(HTTP_FORBIDDEN, b"");
        return;
    }
    let method = match hreq.method {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Head => "HEAD",
        Method::Put => "PUT",
        _ => {
            tracing::debug!(target: "http", "HTTP request from {peer_text} rejected: Unknown HTTP request method");
            hreq.write_reply(HTTP_BAD_METHOD, b"");
            return;
        }
    };
    let uri: String = sanitize_uri(&hreq.uri).chars().take(100).collect();
    tracing::debug!(target: "http", "Received a {method} request for {uri} from {peer_text}");
    let found = {
        let handlers = shared.handlers.read().expect("not poisoned");
        handlers.iter().find_map(|h| {
            let matched = if h.exact {
                hreq.uri == h.prefix
            } else {
                hreq.uri.as_bytes().starts_with(h.prefix.as_bytes())
            };
            matched.then(|| (Arc::clone(&h.handler), hreq.uri[h.prefix.len()..].to_owned()))
        })
    };
    let Some((handler, path)) = found else {
        hreq.write_reply(HTTP_NOT_FOUND, b"");
        return;
    };
    let external = is_external(shared, &hreq);
    let item = Box::new(WorkItem {
        req: hreq,
        path,
        handler,
    });
    if let Err(mut item) = shared.queue.enqueue(item, external) {
        let (what, setting, text) = if external {
            (
                "http work queue depth of externals",
                "-rpcexternalworkqueue=",
                "Work queue depth of externals exceeded",
            )
        } else {
            ("http work queue depth", "-rpcworkqueue=", "Work queue depth exceeded")
        };
        tracing::info!(
            "WARNING: request rejected because {what} exceeded, it can be increased with the {setting} setting"
        );
        item.req.write_reply(HTTP_SERVICE_UNAVAILABLE, text.as_bytes());
    }
}

/// Whether the request's Basic credentials name an `-rpcexternaluser`.
fn is_external(shared: &Shared, req: &HttpRequest) -> bool {
    if shared.external_users.is_empty() {
        return false;
    }
    let Some(encoded) =
        req.header("authorization").and_then(|a| a.as_bytes().strip_prefix(b"Basic "))
    else {
        return false;
    };
    let Some(user_pass) = decode_base64(trim(encoded)) else {
        return false;
    };
    let Some(colon) = user_pass.iter().position(|&b| b == b':') else {
        return false;
    };
    let user = String::from_utf8_lossy(&user_pass[..colon]);
    shared.external_users.iter().any(|u| *u == user)
}

#[cfg(test)]
mod tests;
