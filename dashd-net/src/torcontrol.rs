//! Core's Tor control (`torcontrol.cpp`, P8b): the control-port connection, authentication
//! (`NULL`, `SAFECOOKIE`, `HASHEDPASSWORD`), the onion proxy from `GETINFO
//! net/listeners/socks`, and the onion service from `ADD_ONION`, advertised as a local address.
//!
//! [`TorController`] is Core's `TorController` without I/O: it takes the control port's bytes
//! and returns the commands to send, and reaches the node through [`TorHost`]. [`start`] runs it
//! on a task as Core's `torcontrol` thread runs its event loop: connect, read, reconnect with
//! backoff, and stop when nothing is left to wait for.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use bitcoin_hashes::{Hash, HashEngine, Hmac, HmacEngine, sha256};
use dash_p2p::local::LocalScore;
use dash_p2p::netaddr::Network;
use dash_p2p::proxy::{Proxy, ProxyAddr};
use dash_p2p::{NetCore, Service};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// `DEFAULT_TOR_CONTROL_PORT`.
pub const DEFAULT_TOR_CONTROL_PORT: u16 = 9051;
/// `DEFAULT_TOR_CONTROL`, `-torcontrol`'s default.
pub const DEFAULT_TOR_CONTROL: &str = "127.0.0.1:9051";
/// The cookie's size (control-spec.txt).
const TOR_COOKIE_SIZE: usize = 32;
/// The SAFECOOKIE client and server nonces' size.
const TOR_NONCE_SIZE: usize = 32;
/// The HMAC key of the server's SAFECOOKIE hash.
const TOR_SAFE_SERVERKEY: &str = "Tor safe cookie authentication server-to-controller hash";
/// The HMAC key of the client's SAFECOOKIE hash.
const TOR_SAFE_CLIENTKEY: &str = "Tor safe cookie authentication controller-to-server hash";
/// The first reconnect delay, in seconds.
const RECONNECT_TIMEOUT_START: f32 = 1.0;
/// The reconnect delay's growth factor.
const RECONNECT_TIMEOUT_EXP: f32 = 1.5;
/// The most bytes of an unfinished line the connection keeps.
const MAX_LINE_LENGTH: usize = 100_000;
/// Tor's default SOCKS port, for the onion proxy.
const DEFAULT_TOR_SOCKS_PORT: u16 = 9050;

/// A reply from Tor (`TorControlReply`): its status code and each line's text after the code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TorReply {
    /// The status code (`0` when the line does not start with a number).
    pub code: i32,
    /// Each line after `<code><sep>`.
    pub lines: Vec<Vec<u8>>,
}

/// Core's `SplitTorReplyLine`: `AUTH METHODS=…` as `AUTH` and `METHODS=…`.
// Core: torcontrol.cpp:203-214
pub fn split_tor_reply_line(s: &[u8]) -> (&[u8], &[u8]) {
    let ptr = s.iter().position(|&c| c == b' ').unwrap_or(s.len());
    (&s[..ptr], s.get(ptr + 1..).unwrap_or_default())
}

/// Core's `ParseTorReplyMapping`: `KEY=VALUE` pairs, a value bare or a quoted string with C
/// escapes, up to the first word without `=`. Empty when the text ends inside a pair.
// Core: torcontrol.cpp:222-313
pub fn parse_tor_reply_mapping(s: &[u8]) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut mapping = BTreeMap::new();
    let mut ptr = 0;
    while ptr < s.len() {
        let mut key = Vec::new();
        let mut value = Vec::new();
        while ptr < s.len() && s[ptr] != b'=' && s[ptr] != b' ' {
            key.push(s[ptr]);
            ptr += 1;
        }
        if ptr == s.len() {
            // Unexpected end of line.
            return BTreeMap::new();
        }
        if s[ptr] == b' ' {
            // The rest is an OptArguments.
            break;
        }
        ptr += 1;
        if ptr < s.len() && s[ptr] == b'"' {
            ptr += 1;
            let mut escape_next = false;
            while ptr < s.len() && (escape_next || s[ptr] != b'"') {
                // Repeated backslashes are read in pairs.
                escape_next = s[ptr] == b'\\' && !escape_next;
                value.push(s[ptr]);
                ptr += 1;
            }
            if ptr == s.len() {
                return BTreeMap::new();
            }
            ptr += 1;
            value = unescape(&value);
        } else {
            // A bare value holds anything but a space, `=` included.
            while ptr < s.len() && s[ptr] != b' ' {
                value.push(s[ptr]);
                ptr += 1;
            }
        }
        if ptr < s.len() && s[ptr] == b' ' {
            ptr += 1;
        }
        mapping.insert(key, value);
    }
    mapping
}

/// A quoted string's escapes (control-spec 2.1.1): `\n`, `\t`, `\r`, octal of at most three
/// digits (two when the first is above 3), and any other character as itself.
// Core: torcontrol.cpp:259-301
fn unescape(value: &[u8]) -> Vec<u8> {
    let octal = |c: u8| (b'0'..=b'7').contains(&c);
    let mut out = Vec::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        if value[i] != b'\\' {
            out.push(value[i]);
            i += 1;
            continue;
        }
        // A closing quote cannot follow an odd run of backslashes, so one character follows.
        i += 1;
        let Some(&c) = value.get(i) else {
            break;
        };
        match c {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'0'..=b'7' => {
                let mut j = 1;
                while j < 3 && value.get(i + j).copied().is_some_and(octal) {
                    j += 1;
                }
                if j == 3 && c > b'3' {
                    j -= 1;
                }
                let val = value[i..i + j]
                    .iter()
                    .fold(0u8, |v, d| v.wrapping_mul(8).wrapping_add(d - b'0'));
                out.push(val);
                i += j - 1;
            }
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

/// Core's `ComputeResponse`: HMAC-SHA256 under `key` of the cookie and both nonces.
// Core: torcontrol.cpp:489-498
fn compute_response(key: &str, cookie: &[u8], client_nonce: &[u8], server_nonce: &[u8]) -> Vec<u8> {
    let mut engine = HmacEngine::<sha256::Hash>::new(key.as_bytes());
    engine.input(cookie);
    engine.input(client_nonce);
    engine.input(server_nonce);
    Hmac::from_engine(engine).to_byte_array().to_vec()
}

/// `readcb`'s reply code: a line's first three bytes as `ToIntegral<int>` (`-?[0-9]+`, so not
/// `+50`), else 0.
// Core: torcontrol.cpp:92, util/strencodings.h:181-190
fn reply_code(s: &[u8]) -> i32 {
    std::str::from_utf8(&s[..3.min(s.len())])
        .ok()
        .filter(|c| !c.starts_with('+'))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

/// Core's `HexStr`.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Core's `ParseHex`: pairs of hex digits, spaces allowed between pairs; empty on anything else.
// Core: util/strencodings.cpp:81-97 TryParseHex
fn parse_hex(s: &[u8]) -> Vec<u8> {
    let digit = |c: u8| (c as char).to_digit(16);
    let mut out = Vec::new();
    let mut it = s.iter().copied();
    while let Some(c1) = it.next() {
        if c1.is_ascii_whitespace() || c1 == 0x0b {
            continue;
        }
        let Some(c2) = it.next() else {
            return Vec::new();
        };
        match (digit(c1), digit(c2)) {
            (Some(h), Some(l)) => out.push((h << 4 | l) as u8),
            _ => return Vec::new(),
        }
    }
    out
}

/// Core's `SanitizeString` with `SAFE_CHARS_DEFAULT`.
fn sanitize(s: &[u8]) -> String {
    s.iter()
        .filter(|c| c.is_ascii_alphanumeric() || b" .,;-_/:?@()".contains(c))
        .map(|&c| c as char)
        .collect()
}

fn text(s: &[u8]) -> String {
    String::from_utf8_lossy(s).into_owned()
}

/// What the controller changes in the node, and the files it reads and writes.
pub trait TorHost: Send + Sync {
    /// `SetProxy(NET_ONION, …)`, and `g_reachable_nets.Add(NET_ONION)` when `reachable`.
    fn set_onion_proxy(&self, proxy: Proxy, reachable: bool);
    /// Whether `-onlynet` allows Tor: unset, or one of its values parses as `onion` (Core parses
    /// them again each time, with `ParseNetwork`'s log line for `tor`).
    fn onion_allowed_by_onlynet(&self) -> bool;
    /// `AddLocal(service, LOCAL_MANUAL)`.
    fn add_local(&self, service: Service);
    /// `RemoveLocal(service)`.
    fn remove_local(&self, service: &Service);
    /// Core's `LookupNumeric(name, port)`: an IP or onion literal, else an invalid service.
    fn lookup_numeric(&self, name: &str, port: u16) -> Service;
    /// `ReadBinaryFile` of the onion key file: `None` when it cannot be read.
    fn read_private_key(&self) -> Option<Vec<u8>>;
    /// `WriteBinaryFile` of the onion key file.
    fn write_private_key(&self, key: &[u8]) -> bool;
    /// The onion key file's path, for the logs.
    fn private_key_path(&self) -> String;
    /// `ReadBinaryFile(path, TOR_COOKIE_SIZE)`: at most that many bytes, `None` when it cannot
    /// be read.
    fn read_cookie(&self, path: &[u8], max: usize) -> Option<Vec<u8>>;
    /// `GetRandBytes` for the SAFECOOKIE client nonce.
    fn client_nonce(&self) -> [u8; TOR_NONCE_SIZE];
}

/// The options Core's `TorController` reads.
#[derive(Clone)]
pub struct TorOptions {
    /// `-torcontrol`.
    pub control: String,
    /// `-torpassword`.
    pub password: String,
    /// `-onion` is unset or empty (`GetArg("-onion", "") == ""`): ask Tor for its SOCKS port.
    pub ask_socks_port: bool,
    /// The chain's default port: the onion service's virtual port.
    pub default_port: u16,
    /// Where the onion service sends connections (the first onion bind).
    pub target: Service,
}

/// The reply handlers Core binds to each command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handler {
    ProtocolInfo,
    AuthChallenge,
    Auth,
    GetSocks,
    AddOnion,
}

/// Core's `TorController` and its connection's line state (`TorControlConnection`'s `message`
/// and `reply_handlers`, which survive a reconnection as Core's do).
pub struct TorController {
    opts: TorOptions,
    host: Arc<dyn TorHost>,
    message: TorReply,
    handlers: VecDeque<Handler>,
    reconnect_timeout: f32,
    private_key: Vec<u8>,
    service_id: Vec<u8>,
    service: Service,
    cookie: Vec<u8>,
    client_nonce: Vec<u8>,
}

impl TorController {
    /// The controller before its first connection, with the cached key read (`ReadBinaryFile`).
    // Core: torcontrol.cpp:315-334
    pub fn new(opts: TorOptions, host: Arc<dyn TorHost>) -> Self {
        let mut ctl = TorController {
            opts,
            host,
            message: TorReply::default(),
            handlers: VecDeque::new(),
            reconnect_timeout: RECONNECT_TIMEOUT_START,
            private_key: Vec::new(),
            service_id: Vec::new(),
            service: Service::default(),
            cookie: Vec::new(),
            client_nonce: Vec::new(),
        };
        if let Some(key) = ctl.host.read_private_key() {
            tracing::debug!(target: "tor", "Reading cached private key from {}", ctl.host.private_key_path());
            ctl.private_key = key;
        }
        ctl
    }

    /// The advertised onion service, invalid until `ADD_ONION` succeeds.
    pub fn service(&self) -> &Service {
        &self.service
    }

    /// The delay before the next reconnection.
    fn reconnect_delay(&self) -> Duration {
        Duration::from_millis((f64::from(self.reconnect_timeout) * 1000.0) as u64)
    }

    /// `RemoveLocal` of the advertised service, when there is one.
    fn withdraw_service(&self) {
        if self.service.addr.is_valid() {
            self.host.remove_local(&self.service);
        }
    }

    /// `TorControlConnection::Command`: the command line, and its handler queued.
    fn command(&mut self, out: &mut Vec<u8>, cmd: &[u8], handler: Handler) {
        out.extend_from_slice(cmd);
        out.extend_from_slice(b"\r\n");
        self.handlers.push_back(handler);
    }

    /// `connected_cb`: the backoff resets and `PROTOCOLINFO 1` goes out.
    // Core: torcontrol.cpp:611-617
    pub fn connected(&mut self) -> Vec<u8> {
        self.reconnect_timeout = RECONNECT_TIMEOUT_START;
        let mut out = Vec::new();
        self.command(&mut out, b"PROTOCOLINFO 1", Handler::ProtocolInfo);
        out
    }

    /// `disconnected_cb`: the service stops being advertised; returns the delay before the next
    /// connection, and grows it.
    // Core: torcontrol.cpp:619-635
    pub fn disconnected(&mut self) -> Duration {
        self.withdraw_service();
        self.service = Service::default();
        tracing::debug!(target: "tor", "Not connected to Tor control port {}, trying to reconnect", self.opts.control);
        let delay = self.reconnect_delay();
        self.reconnect_timeout *= RECONNECT_TIMEOUT_EXP;
        delay
    }

    /// `readcb` for one line (without its end of line): a final line dispatches the reply to the
    /// oldest handler. Returns the commands the handler sends.
    // Core: torcontrol.cpp:77-112
    pub fn line(&mut self, s: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        if s.len() < 4 {
            return out;
        }
        self.message.code = reply_code(s);
        self.message.lines.push(s[4..].to_vec());
        if s[3] != b' ' {
            return out;
        }
        let reply = std::mem::take(&mut self.message);
        if reply.code >= 600 {
            // Asynchronous notifications (unused).
        } else if let Some(handler) = self.handlers.pop_front() {
            self.dispatch(handler, &reply, &mut out);
        } else {
            tracing::debug!(target: "tor", "Received unexpected sync reply {}", reply.code);
        }
        out
    }

    fn dispatch(&mut self, handler: Handler, reply: &TorReply, out: &mut Vec<u8>) {
        match handler {
            Handler::ProtocolInfo => self.protocolinfo(reply, out),
            Handler::AuthChallenge => self.authchallenge(reply, out),
            Handler::Auth => self.auth(reply, out),
            Handler::GetSocks => self.get_socks(reply),
            Handler::AddOnion => self.add_onion(reply),
        }
    }

    // Core: torcontrol.cpp:535-609
    fn protocolinfo(&mut self, reply: &TorReply, out: &mut Vec<u8>) {
        if reply.code != 250 {
            tracing::info!("tor: Requesting protocol info failed");
            return;
        }
        let mut methods = BTreeSet::new();
        let mut cookiefile = Vec::new();
        for s in &reply.lines {
            let (kind, args) = split_tor_reply_line(s);
            if kind == b"AUTH" {
                let m = parse_tor_reply_mapping(args);
                if let Some(v) = m.get(&b"METHODS"[..]) {
                    methods = split_string(v, b',').into_iter().collect();
                }
                if let Some(v) = m.get(&b"COOKIEFILE"[..]) {
                    cookiefile = v.clone();
                }
            } else if kind == b"VERSION" {
                let m = parse_tor_reply_mapping(args);
                if let Some(v) = m.get(&b"Tor"[..]) {
                    tracing::debug!(target: "tor", "Connected to Tor version {}", text(v));
                }
            }
        }
        for s in &methods {
            tracing::debug!(target: "tor", "Supported authentication method: {}", text(s));
        }
        let has = |m: &[u8]| methods.contains(m);
        if !self.opts.password.is_empty() {
            if has(b"HASHEDPASSWORD") {
                tracing::debug!(target: "tor", "Using HASHEDPASSWORD authentication");
                let password = self.opts.password.replace('"', "\\\"");
                let cmd = format!("AUTHENTICATE \"{password}\"");
                self.command(out, cmd.as_bytes(), Handler::Auth);
            } else {
                tracing::info!(
                    "tor: Password provided with -torpassword, but HASHEDPASSWORD authentication is not available"
                );
            }
        } else if has(b"NULL") {
            tracing::debug!(target: "tor", "Using NULL authentication");
            self.command(out, b"AUTHENTICATE", Handler::Auth);
        } else if has(b"SAFECOOKIE") {
            let file = text(&cookiefile);
            tracing::debug!(target: "tor", "Using SAFECOOKIE authentication, reading cookie authentication from {file}");
            match self.host.read_cookie(&cookiefile, TOR_COOKIE_SIZE) {
                Some(cookie) if cookie.len() == TOR_COOKIE_SIZE => {
                    self.cookie = cookie;
                    self.client_nonce = self.host.client_nonce().to_vec();
                    let cmd = format!("AUTHCHALLENGE SAFECOOKIE {}", hex(&self.client_nonce));
                    self.command(out, cmd.as_bytes(), Handler::AuthChallenge);
                }
                Some(_) => tracing::info!(
                    "tor: Authentication cookie {file} is not exactly {TOR_COOKIE_SIZE} bytes, as is required by the spec"
                ),
                None => tracing::info!(
                    "tor: Authentication cookie {file} could not be opened (check permissions)"
                ),
            }
        } else if has(b"HASHEDPASSWORD") {
            tracing::info!(
                "tor: The only supported authentication mechanism left is password, but no password provided with -torpassword"
            );
        } else {
            tracing::info!("tor: No supported authentication method");
        }
    }

    // Core: torcontrol.cpp:500-533
    fn authchallenge(&mut self, reply: &TorReply, out: &mut Vec<u8>) {
        if reply.code != 250 {
            tracing::info!("tor: SAFECOOKIE authentication challenge failed");
            return;
        }
        tracing::debug!(target: "tor", "SAFECOOKIE authentication challenge successful");
        // A final line always exists.
        let (kind, args) = split_tor_reply_line(&reply.lines[0]);
        if kind != b"AUTHCHALLENGE" {
            tracing::info!("tor: Invalid reply to AUTHCHALLENGE");
            return;
        }
        let m = parse_tor_reply_mapping(args);
        if m.is_empty() {
            tracing::info!("tor: Error parsing AUTHCHALLENGE parameters: {}", sanitize(args));
            return;
        }
        let field = |k: &[u8]| parse_hex(m.get(k).map(Vec::as_slice).unwrap_or_default());
        let server_hash = field(b"SERVERHASH");
        let server_nonce = field(b"SERVERNONCE");
        tracing::debug!(target: "tor", "AUTHCHALLENGE ServerHash {} ServerNonce {}", hex(&server_hash), hex(&server_nonce));
        if server_nonce.len() != TOR_NONCE_SIZE {
            tracing::info!("tor: ServerNonce is not 32 bytes, as required by spec");
            return;
        }
        let computed =
            compute_response(TOR_SAFE_SERVERKEY, &self.cookie, &self.client_nonce, &server_nonce);
        if computed != server_hash {
            tracing::info!(
                "tor: ServerHash {} does not match expected ServerHash {}",
                hex(&server_hash),
                hex(&computed)
            );
            return;
        }
        let client =
            compute_response(TOR_SAFE_CLIENTKEY, &self.cookie, &self.client_nonce, &server_nonce);
        self.command(out, format!("AUTHENTICATE {}", hex(&client)).as_bytes(), Handler::Auth);
    }

    // Core: torcontrol.cpp:449-471
    fn auth(&mut self, reply: &TorReply, out: &mut Vec<u8>) {
        if reply.code != 250 {
            tracing::info!("tor: Authentication failed");
            return;
        }
        tracing::debug!(target: "tor", "Authentication successful");
        // The onion proxy, unless `-onion` names one.
        if self.opts.ask_socks_port {
            self.command(out, b"GETINFO net/listeners/socks", Handler::GetSocks);
        }
        if self.private_key.is_empty() {
            // Explicitly request the key type (bitcoin#9214).
            self.private_key = b"NEW:ED25519-V3".to_vec();
        }
        // The virtual port is always the default port, so that a node on another port is not
        // decloaked.
        let mut cmd = b"ADD_ONION ".to_vec();
        cmd.extend_from_slice(&self.private_key);
        cmd.extend_from_slice(
            format!(" Port={},{}", self.opts.default_port, self.opts.target).as_bytes(),
        );
        self.command(out, &cmd, Handler::AddOnion);
        cmd.fill(0);
    }

    // Core: torcontrol.cpp:347-412
    fn get_socks(&mut self, reply: &TorReply) {
        // Only when `-onion` is unset.
        let mut socks_location = String::new();
        if reply.code == 250 {
            for line in &reply.lines {
                let Some(list) = line.strip_prefix(b"net/listeners/socks=") else {
                    continue;
                };
                for port in split_string(list, b' ') {
                    let mut port = &port[..];
                    if port.is_empty() {
                        continue;
                    }
                    if (port[0] == b'"' || port[0] == b'\'')
                        && port.len() >= 2
                        && port[port.len() - 1] == port[0]
                    {
                        port = &port[1..port.len() - 1];
                        if port.is_empty() {
                            continue;
                        }
                    }
                    socks_location = text(port);
                    if port.starts_with(b"127.0.0.1:") {
                        // Prefer localhost: ignore the other ports.
                        break;
                    }
                }
            }
            if socks_location.is_empty() {
                tracing::info!("tor: Get SOCKS port command returned nothing");
            } else {
                tracing::debug!(target: "tor", "Get SOCKS port command yielded {socks_location}");
            }
        } else if reply.code == 510 {
            tracing::info!(
                "tor: Get SOCKS port command failed with unrecognized command (You probably should upgrade Tor)"
            );
        } else {
            tracing::info!("tor: Get SOCKS port command failed; error code {}", reply.code);
        }
        let mut resolved = if socks_location.is_empty() {
            Service::default()
        } else {
            self.host.lookup_numeric(&socks_location, DEFAULT_TOR_SOCKS_PORT)
        };
        if !resolved.addr.is_valid() {
            // The old behaviour.
            resolved = self.host.lookup_numeric("127.0.0.1", DEFAULT_TOR_SOCKS_PORT);
        }
        tracing::debug!(target: "tor", "Configuring onion proxy for {resolved}");
        let proxy = Proxy {
            addr: ProxyAddr::Service(resolved),
            randomize_credentials: true,
        };
        // Without -proxy or -onion, Tor is unreachable until now; -onlynet may still exclude it.
        let reachable = self.host.onion_allowed_by_onlynet();
        self.host.set_onion_proxy(proxy, reachable);
    }

    // Core: torcontrol.cpp:414-447
    fn add_onion(&mut self, reply: &TorReply) {
        match reply.code {
            250 => {}
            510 => {
                tracing::info!(
                    "tor: Add onion failed with unrecognized command (You probably need to upgrade Tor)"
                );
                return;
            }
            code => {
                tracing::info!("tor: Add onion failed; error code {code}");
                return;
            }
        }
        tracing::debug!(target: "tor", "ADD_ONION successful");
        for s in &reply.lines {
            let m = parse_tor_reply_mapping(s);
            if let Some(v) = m.get(&b"ServiceID"[..]) {
                self.service_id = v.clone();
            }
            if let Some(v) = m.get(&b"PrivateKey"[..]) {
                self.private_key.fill(0);
                self.private_key = v.clone();
            }
        }
        if self.service_id.is_empty() {
            tracing::info!("tor: Error parsing ADD_ONION parameters:");
            for s in &reply.lines {
                tracing::info!("    {}", sanitize(s));
            }
            return;
        }
        let id = text(&self.service_id);
        self.service = self.host.lookup_numeric(&format!("{id}.onion"), self.opts.default_port);
        tracing::info!("Got tor service ID {id}, advertising service {}", self.service);
        let path = self.host.private_key_path();
        if self.host.write_private_key(&self.private_key) {
            tracing::debug!(target: "tor", "Cached service private key to {path}");
        } else {
            tracing::info!("tor: Error writing service private key to {path}");
        }
        self.host.add_local(self.service.clone());
        // The onion is requested: the connection stays open.
    }
}

impl Drop for TorController {
    // Core: torcontrol.cpp:336-345
    fn drop(&mut self) {
        self.withdraw_service();
        self.private_key.fill(0);
        self.cookie.fill(0);
    }
}

/// Core's `SplitString(s, sep)`: every piece, empty ones included.
fn split_string(s: &[u8], sep: u8) -> Vec<Vec<u8>> {
    s.split(|&c| c == sep).map(<[u8]>::to_vec).collect()
}

/// [`TorHost`] over the network core, with the files and lookups the node provides.
pub struct CoreTorHost<F> {
    core: Arc<NetCore>,
    files: F,
}

/// The node's half of [`TorHost`]: the onion key file, the cookie file, `-onlynet` and the
/// random nonce.
pub trait TorFiles: Send + Sync {
    /// See [`TorHost::onion_allowed_by_onlynet`].
    fn onion_allowed_by_onlynet(&self) -> bool;
    /// See [`TorHost::read_private_key`].
    fn read_private_key(&self) -> Option<Vec<u8>>;
    /// See [`TorHost::write_private_key`].
    fn write_private_key(&self, key: &[u8]) -> bool;
    /// See [`TorHost::private_key_path`].
    fn private_key_path(&self) -> String;
}

impl<T: TorFiles + ?Sized> TorFiles for Arc<T> {
    fn onion_allowed_by_onlynet(&self) -> bool {
        (**self).onion_allowed_by_onlynet()
    }

    fn read_private_key(&self) -> Option<Vec<u8>> {
        (**self).read_private_key()
    }

    fn write_private_key(&self, key: &[u8]) -> bool {
        (**self).write_private_key(key)
    }

    fn private_key_path(&self) -> String {
        (**self).private_key_path()
    }
}

impl<F: TorFiles> CoreTorHost<F> {
    /// The host over `core`.
    pub fn new(core: Arc<NetCore>, files: F) -> Self {
        CoreTorHost {
            core,
            files,
        }
    }
}

impl<F: TorFiles> TorHost for CoreTorHost<F> {
    fn set_onion_proxy(&self, proxy: Proxy, reachable: bool) {
        self.core.set_proxy(Network::Onion, proxy);
        if reachable {
            self.core.set_reachable(Network::Onion, true);
        }
    }

    fn onion_allowed_by_onlynet(&self) -> bool {
        self.files.onion_allowed_by_onlynet()
    }

    fn add_local(&self, service: Service) {
        self.core.add_local(service, LocalScore::Manual);
    }

    fn remove_local(&self, service: &Service) {
        self.core.remove_local(service);
    }

    fn lookup_numeric(&self, name: &str, port: u16) -> Service {
        lookup_numeric(name, port)
    }

    fn read_private_key(&self) -> Option<Vec<u8>> {
        self.files.read_private_key()
    }

    fn write_private_key(&self, key: &[u8]) -> bool {
        self.files.write_private_key(key)
    }

    fn private_key_path(&self) -> String {
        self.files.private_key_path()
    }

    fn read_cookie(&self, path: &[u8], max: usize) -> Option<Vec<u8>> {
        read_binary_file(path, max)
    }

    fn client_nonce(&self) -> [u8; TOR_NONCE_SIZE] {
        let mut nonce = [0; TOR_NONCE_SIZE];
        getrandom::fill(&mut nonce).expect("the OS random source");
        nonce
    }
}

/// Core's `LookupNumeric(name, port)`: `Lookup` without the resolver, or an invalid service.
// Core: netbase.cpp:219-226
pub fn lookup_numeric(name: &str, port: u16) -> Service {
    dash_p2p::lookup::lookup_port(name, port).unwrap_or_default()
}

/// Core's `ReadBinaryFile(path, max)`: at most `max` bytes; `None` when the file cannot be
/// opened or read.
// Core: util/readwritefile.cpp:16-35
pub fn read_binary_file(path: &[u8], max: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    #[cfg(unix)]
    let path =
        std::path::Path::new(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(path));
    #[cfg(not(unix))]
    let path = std::path::PathBuf::from(text(path));
    let file = std::fs::File::open(path).ok()?;
    let mut out = Vec::new();
    file.take(max as u64).read_to_end(&mut out).ok()?;
    Some(out)
}

/// The running Tor control: Core's `torcontrol` thread.
pub struct TorControl {
    interrupt: watch::Sender<bool>,
    task: JoinHandle<()>,
}

/// Core's `StartTorControl`: the controller on its own task of `rt`, connecting to
/// `opts.control` with `name_lookup` (`fNameLookup`).
// Core: torcontrol.cpp:663-687
pub fn start(
    rt: &tokio::runtime::Handle,
    core: Arc<NetCore>,
    opts: TorOptions,
    host: Arc<dyn TorHost>,
    name_lookup: bool,
) -> TorControl {
    let (interrupt, rx) = watch::channel(false);
    let task = rt.spawn(run(core, opts, host, name_lookup, rx));
    TorControl {
        interrupt,
        task,
    }
}

impl TorControl {
    /// Core's `InterruptTorControl`: the loop stops at its next wait.
    // Core: torcontrol.cpp:689-697
    pub fn interrupt(&self) {
        tracing::info!("tor: Thread interrupt");
        self.interrupt.send_replace(true);
    }

    /// Whether the loop has ended on its own (nothing left to wait for) or after an interrupt.
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// Core's `StopTorControl`: waits for the loop, which drops the controller (and with it the
    /// advertised service).
    // Core: torcontrol.cpp:699-706
    pub async fn stop(self) {
        self.interrupt.send_replace(true);
        let _ = self.task.await;
    }
}

/// How `TorControlConnection::Connect` went.
enum Attempt {
    /// The lookup failed: no connection exists, so the loop has nothing to wait for.
    NoAddress,
    /// `bufferevent_socket_connect` failed at once: the connection's events stay armed with no
    /// socket, so the loop waits for the interrupt.
    Failed,
    /// The connect started (or was already refused: the error event).
    Started(std::io::Result<TcpStream>),
}

/// `TorControlConnection::Connect`: the lookup, then libevent's non-blocking connect, after
/// Core's log line when either fails.
// Core: torcontrol.cpp:139-173
async fn connect(core: &NetCore, center: &str, name_lookup: bool) -> Attempt {
    let Some(service) = core.lookup_service(center, DEFAULT_TOR_CONTROL_PORT, name_lookup).await
    else {
        tracing::info!("tor: Failed to look up control center {center}");
        return Attempt::NoAddress;
    };
    let Some(sa) = service.to_socket_addr() else {
        tracing::info!("tor: Error parsing socket address {center}");
        return Attempt::NoAddress;
    };
    match start_connect(sa) {
        Some(started) => Attempt::Started(started),
        None => {
            tracing::info!("tor: Error connecting to address {center}");
            Attempt::Failed
        }
    }
}

/// libevent's `bufferevent_socket_connect` (with `evutil_socket_connect_`): `None` when the
/// socket cannot be made or the connect fails at once ([`ConnectStart::Failed`]); otherwise the
/// connection in progress, or the error event already due ([`ConnectStart::Refused`]).
fn start_connect(sa: std::net::SocketAddr) -> Option<std::io::Result<TcpStream>> {
    let socket =
        socket2::Socket::new(socket2::Domain::for_address(sa), socket2::Type::STREAM, None).ok()?;
    socket.set_nonblocking(true).ok()?;
    if let Err(e) = socket.connect(&sa.into()) {
        match connect_start(&e) {
            ConnectStart::InProgress => {}
            ConnectStart::Refused => return Some(Err(e)),
            ConnectStart::Failed => return None,
        }
    }
    TcpStream::from_std(socket.into()).ok().map(Ok)
}

/// How a non-blocking connect that returned an error started: `evutil_socket_connect_`'s 0, 2
/// and -1.
#[derive(Debug, PartialEq, Eq)]
enum ConnectStart {
    /// `EVUTIL_ERR_CONNECT_RETRIABLE`: the result comes when the socket turns writable.
    InProgress,
    /// `EVUTIL_ERR_CONNECT_REFUSED` (BSD and macOS refuse a loopback connect at once): the error
    /// event, which libevent defers to the event loop (`BEV_OPT_DEFER_CALLBACKS`), so Core logs
    /// it after the constructor's cached-key line, as here.
    Refused,
    /// Anything else: `bufferevent_socket_connect` returns -1.
    Failed,
}

// libevent 2.1.12: evutil.c evutil_socket_connect_, bufferevent_sock.c:428-446,
// bufferevent.c:291-294; util-internal.h EVUTIL_ERR_CONNECT_RETRIABLE / _REFUSED
fn connect_start(e: &std::io::Error) -> ConnectStart {
    #[cfg(unix)]
    let in_progress = matches!(e.raw_os_error(), Some(libc::EINPROGRESS | libc::EINTR));
    // WSAEINTR, WSAEINVAL, WSAEWOULDBLOCK, WSAEINPROGRESS.
    #[cfg(windows)]
    let in_progress = matches!(e.raw_os_error(), Some(10004 | 10022 | 10035 | 10036));
    #[cfg(not(any(unix, windows)))]
    let in_progress = e.kind() == std::io::ErrorKind::WouldBlock;
    if in_progress {
        ConnectStart::InProgress
    } else if e.kind() == std::io::ErrorKind::ConnectionRefused {
        ConnectStart::Refused
    } else {
        ConnectStart::Failed
    }
}

/// The connect's outcome: the socket turns writable once it is connected or has failed.
async fn established(stream: TcpStream) -> std::io::Result<TcpStream> {
    stream.writable().await?;
    match stream.take_error()? {
        Some(e) => Err(e),
        None => Ok(stream),
    }
}

/// Waits for `fut` unless the loop is interrupted first (`event_base_loopbreak`).
async fn unless_interrupted<T>(
    interrupt: &mut watch::Receiver<bool>,
    fut: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        _ = interrupt.wait_for(|i| *i) => None,
        t = fut => Some(t),
    }
}

/// The event loop: Core's `TorControlThread` (`event_base_dispatch` until it has nothing to wait
/// for, or `loopbreak`).
// Core: torcontrol.cpp:66-194, :315-334, :619-657
async fn run(
    core: Arc<NetCore>,
    opts: TorOptions,
    host: Arc<dyn TorHost>,
    name_lookup: bool,
    mut interrupt: watch::Receiver<bool>,
) {
    let center = opts.control.clone();
    // Connection attempts start at once, before the cached key is read.
    let mut attempt = connect(&core, &center, name_lookup).await;
    if !matches!(attempt, Attempt::Started(_)) {
        tracing::info!("tor: Initiating connection to Tor control port {center} failed");
    }
    let mut ctl = TorController::new(opts, host);
    loop {
        let started = match attempt {
            Attempt::NoAddress => return,
            Attempt::Failed => {
                let _ = interrupt.wait_for(|i| *i).await;
                return;
            }
            Attempt::Started(started) => started,
        };
        let established = match started {
            Ok(stream) => match unless_interrupted(&mut interrupt, established(stream)).await {
                None => return,
                Some(established) => established,
            },
            Err(e) => Err(e),
        };
        let delay = match established {
            Err(_) => {
                tracing::debug!(target: "tor", "Error connecting to Tor control socket");
                ctl.disconnected()
            }
            Ok(stream) => {
                tracing::debug!(target: "tor", "Successfully connected!");
                match connection(&mut ctl, stream, &mut interrupt).await {
                    Closed::Interrupted | Closed::LineTooLong => return,
                    Closed::Eof => {
                        tracing::debug!(target: "tor", "End of stream");
                        ctl.disconnected()
                    }
                    Closed::Error => {
                        tracing::debug!(target: "tor", "Error connecting to Tor control socket");
                        ctl.disconnected()
                    }
                }
            }
        };
        if unless_interrupted(&mut interrupt, tokio::time::sleep(delay)).await.is_none() {
            return;
        }
        // `Reconnect`.
        attempt = connect(&core, &center, name_lookup).await;
        if !matches!(attempt, Attempt::Started(_)) {
            tracing::info!("tor: Re-initiating connection to Tor control port {center} failed");
        }
    }
}

/// How a control connection ended.
enum Closed {
    Interrupted,
    Eof,
    Error,
    /// `MAX_LINE_LENGTH`: Core frees the connection without its disconnect callback, so nothing
    /// is left to wait for.
    LineTooLong,
}

/// One control connection: `connected_cb`, then `readcb` per read until it ends.
// Core: torcontrol.cpp:77-137
async fn connection(
    ctl: &mut TorController,
    mut stream: TcpStream,
    interrupt: &mut watch::Receiver<bool>,
) -> Closed {
    let mut pending = ctl.connected();
    let mut input = Vec::new();
    let mut buf = vec![0; 4096];
    loop {
        if !pending.is_empty() {
            let write = stream.write_all(&pending);
            match unless_interrupted(interrupt, write).await {
                None => return Closed::Interrupted,
                Some(Err(_)) => return Closed::Error,
                Some(Ok(())) => pending.fill(0),
            }
            pending.clear();
        }
        let n = match unless_interrupted(interrupt, stream.read(&mut buf)).await {
            None => return Closed::Interrupted,
            Some(Ok(0)) => return Closed::Eof,
            Some(Ok(n)) => n,
            Some(Err(_)) => return Closed::Error,
        };
        input.extend_from_slice(&buf[..n]);
        if !readcb(ctl, &mut input, &mut pending) {
            tracing::info!("tor: Disconnecting because MAX_LINE_LENGTH exceeded");
            return Closed::LineTooLong;
        }
    }
}

/// `readcb` on what has been read: each whole line to the controller and its commands to
/// `pending`; false when the unfinished rest is over `MAX_LINE_LENGTH` (exactly that is kept).
// Core: torcontrol.cpp:84-119
fn readcb(ctl: &mut TorController, input: &mut Vec<u8>, pending: &mut Vec<u8>) -> bool {
    take_lines(input, |line| pending.extend(ctl.line(line)));
    input.len() <= MAX_LINE_LENGTH
}

/// `evbuffer_readln(EVBUFFER_EOL_CRLF)` until no line is left: each line of `input` to `each`,
/// without its `\n` and an optional `\r` before it, leaving the partial line.
// Core: torcontrol.cpp:84-89
fn take_lines(input: &mut Vec<u8>, mut each: impl FnMut(&[u8])) {
    let mut start = 0;
    while let Some(end) = input[start..].iter().position(|&c| c == b'\n') {
        let line = &input[start..start + end];
        each(line.strip_suffix(b"\r").unwrap_or(line));
        start += end + 1;
    }
    input.drain(..start);
}

#[cfg(test)]
#[path = "torcontrol_tests.rs"]
mod tests;
