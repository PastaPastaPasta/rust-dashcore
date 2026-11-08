//! SOCKS5 proxies (Core's `Proxy`, `Socks5` and `ConnectThroughProxy`, P8a): the per-network
//! proxy table that `-proxy` and `-onion` fill, and the client side of RFC 1928 and RFC 1929 as
//! Core speaks it, byte for byte.
//!
//! Core asks the proxy for every destination by name (`ATYP` DOMAINNAME, an IP address as its
//! text), offers user/password authentication only with `-proxyrandomize`, and reads each reply
//! with its own 20 s window that `CConnman::Interrupt` ends (`InterruptSocks5`).

use std::fmt;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::Instant;

use crate::netaddr::{NetAddr, Network, Service};

/// Core's `ADDR_PREFIX_UNIX`: a proxy given as `unix:<path>` is a UNIX domain socket.
pub const ADDR_PREFIX_UNIX: &str = "unix:";

/// Core's `g_socks5_recv_timeout`: each read of a proxy reply waits this long ("ample time for
/// negotiation for very slow proxies such as Tor").
// Core: netbase.cpp:39
pub const SOCKS5_RECV_TIMEOUT: Duration = Duration::from_secs(20);

/// Core's `MAX_WAIT_FOR_IO`: how long a read waits before it checks for an interrupt.
// Core: util/sock.h:32
const MAX_WAIT_FOR_IO: Duration = Duration::from_secs(1);

/// The size of `sockaddr_un::sun_path` (Linux 108, the BSDs and macOS 104).
#[cfg(any(target_os = "linux", target_os = "android"))]
const SUN_PATH_LEN: usize = 108;
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
const SUN_PATH_LEN: usize = 104;

/// Core's `IsUnixSocketPath`: `unix:` and a path that fits `sun_path` with its NUL. Always false
/// where Core has no `HAVE_SOCKADDR_UN` (Windows).
// Core: netbase.cpp:229-245
pub fn is_unix_socket_path(name: &str) -> bool {
    #[cfg(unix)]
    {
        name.strip_prefix(ADDR_PREFIX_UNIX).is_some_and(|path| path.len() < SUN_PATH_LEN)
    }
    #[cfg(not(unix))]
    {
        let _ = name;
        false
    }
}

/// Where a proxy listens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxyAddr {
    /// A TCP address (Core's `Proxy::proxy`).
    Service(Service),
    /// A UNIX domain socket, kept with its `unix:` prefix (Core's `m_unix_socket_path`).
    Unix(String),
}

/// Core's `Proxy`: a valid proxy and whether it gets random credentials (`-proxyrandomize`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proxy {
    /// Where it listens.
    pub addr: ProxyAddr,
    /// Core's `m_randomize_credentials`.
    pub randomize_credentials: bool,
}

impl Proxy {
    /// Core's `Proxy::IsValid`: a valid address, or a `unix:` path that fits `sun_path`.
    // Core: netbase.h:73-77
    pub fn is_valid(&self) -> bool {
        match &self.addr {
            ProxyAddr::Service(s) => s.addr.is_valid(),
            ProxyAddr::Unix(path) => is_unix_socket_path(path),
        }
    }
}

impl fmt::Display for Proxy {
    /// Core's `Proxy::ToString`: `ip:port`, or the `unix:` path.
    // Core: netbase.h:85-89
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.addr {
            ProxyAddr::Service(s) => write!(f, "{s}"),
            ProxyAddr::Unix(path) => f.write_str(path),
        }
    }
}

/// Core's `proxyInfo[NET_MAX]` and `nameProxy`: the proxy for each network and the one names go
/// through. An entry is set only with a valid proxy (`SetProxy` refuses an invalid one).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyTable {
    nets: [Option<Proxy>; Network::COUNT],
    name: Option<Proxy>,
}

impl ProxyTable {
    /// Core's `SetProxy(net, proxy)`: false, and the entry unchanged, for an invalid proxy.
    // Core: netbase.cpp:684-691
    pub fn set(&mut self, net: Network, proxy: Proxy) -> bool {
        if !proxy.is_valid() {
            return false;
        }
        self.nets[net as usize] = Some(proxy);
        true
    }

    /// Core's `SetNameProxy(proxy)`: false, and the entry unchanged, for an invalid proxy.
    // Core: netbase.cpp:702-708
    pub fn set_name_proxy(&mut self, proxy: Proxy) -> bool {
        if !proxy.is_valid() {
            return false;
        }
        self.name = Some(proxy);
        true
    }

    /// Core's `GetProxy(net, …)`.
    // Core: netbase.cpp:693-700
    pub fn get(&self, net: Network) -> Option<&Proxy> {
        self.nets[net as usize].as_ref()
    }

    /// Core's `GetNameProxy`; `is_some()` is `HaveNameProxy`.
    // Core: netbase.cpp:710-721
    pub fn name_proxy(&self) -> Option<&Proxy> {
        self.name.as_ref()
    }

    /// Core's `IsProxy(addr)`: `addr` is the address of a network's proxy entry. Core compares
    /// every one of its `NET_MAX` entries as a `CNetAddr`, so an unset entry or a UNIX socket
    /// proxy counts as the default address `::`, which therefore always matches.
    // Core: netbase.cpp:723-730
    pub fn is_proxy(&self, addr: &NetAddr) -> bool {
        self.nets.iter().any(|p| match p.as_ref().map(|p| &p.addr) {
            Some(ProxyAddr::Service(s)) => s.addr == *addr,
            Some(ProxyAddr::Unix(_)) | None => *addr == NetAddr::default(),
        })
    }
}

/// Core's `ProxyCredentials`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyCredentials {
    /// The user name.
    pub username: String,
    /// The password.
    pub password: String,
}

/// Core's `IntrRecvError`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IntrRecv {
    Ok,
    Timeout,
    Disconnected,
    NetworkError,
    Interrupted,
}

/// Core's `InterruptibleRecv`: reads exactly `buf.len()` bytes within `timeout`, waiting at most
/// [`MAX_WAIT_FOR_IO`] at a time and checking `interrupted` after every read or wait, a
/// successful one included.
// Core: netbase.cpp:313-343
async fn interruptible_recv<S: AsyncRead + Unpin>(
    buf: &mut [u8],
    timeout: Duration,
    sock: &mut S,
    interrupted: &(dyn Fn() -> bool + Sync),
) -> IntrRecv {
    let end = Instant::now() + timeout;
    let mut got = 0;
    let mut now = Instant::now();
    while got < buf.len() && now < end {
        match tokio::time::timeout((end - now).min(MAX_WAIT_FOR_IO), sock.read(&mut buf[got..]))
            .await
        {
            Ok(Ok(0)) => return IntrRecv::Disconnected,
            Ok(Ok(n)) => got += n,
            Ok(Err(_)) => return IntrRecv::NetworkError,
            Err(_) => {}
        }
        if interrupted() {
            return IntrRecv::Interrupted;
        }
        now = Instant::now();
    }
    if got == buf.len() {
        IntrRecv::Ok
    } else {
        IntrRecv::Timeout
    }
}

/// Core's `Socks5ErrorString`.
// Core: netbase.cpp:346-368
fn socks5_error_string(err: u8) -> &'static str {
    match err {
        0x01 => "general failure",
        0x02 => "connection not allowed",
        0x03 => "network unreachable",
        0x04 => "host unreachable",
        0x05 => "connection refused",
        0x06 => "TTL expired",
        0x07 => "protocol error",
        0x08 => "address type not supported",
        _ => "unknown",
    }
}

/// SOCKS version 5.
const SOCKS5: u8 = 0x05;
/// `METHOD` "no authentication required".
const NOAUTH: u8 = 0x00;
/// `METHOD` "username/password".
const USER_PASS: u8 = 0x02;
/// `CMD` CONNECT.
const CONNECT: u8 = 0x01;
/// `ATYP` values.
const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAINNAME: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;

/// One send of Core's (`sock.Send(…) != size` fails).
async fn send<S: AsyncWrite + Unpin>(sock: &mut S, bytes: &[u8]) -> bool {
    sock.write_all(bytes).await.is_ok()
}

/// Core's `Socks5`: asks the proxy on `sock` to connect to `dest:port`, with `auth` when given,
/// and logs each failure as Core does. `interrupted` is `InterruptSocks5`'s flag;
/// `recv_timeout` each read's window ([`SOCKS5_RECV_TIMEOUT`]).
// Core: netbase.cpp:370-507 Socks5
pub async fn socks5<S: AsyncRead + AsyncWrite + Unpin>(
    dest: &str,
    port: u16,
    auth: Option<&ProxyCredentials>,
    sock: &mut S,
    recv_timeout: Duration,
    interrupted: &(dyn Fn() -> bool + Sync),
) -> bool {
    tracing::debug!(target: "net", "SOCKS5 connecting {dest}");
    if dest.len() > 255 {
        tracing::error!("Hostname too long");
        return false;
    }
    let init: &[u8] = if auth.is_some() {
        &[SOCKS5, 0x02, NOAUTH, USER_PASS]
    } else {
        &[SOCKS5, 0x01, NOAUTH]
    };
    if !send(sock, init).await {
        tracing::error!("Error sending to proxy");
        return false;
    }
    let mut ret1 = [0u8; 2];
    if interruptible_recv(&mut ret1, recv_timeout, sock, interrupted).await != IntrRecv::Ok {
        tracing::info!(
            "Socks5() connect to {dest}:{port} failed: InterruptibleRecv() timeout or other failure"
        );
        return false;
    }
    if ret1[0] != SOCKS5 {
        tracing::error!("Proxy failed to initialize");
        return false;
    }
    match (ret1[1], auth) {
        (USER_PASS, Some(auth)) => {
            // RFC 1929.
            if auth.username.len() > 255 || auth.password.len() > 255 {
                tracing::error!("Proxy username or password too long");
                return false;
            }
            let mut msg = vec![0x01, auth.username.len() as u8];
            msg.extend_from_slice(auth.username.as_bytes());
            msg.push(auth.password.len() as u8);
            msg.extend_from_slice(auth.password.as_bytes());
            if !send(sock, &msg).await {
                tracing::error!("Error sending authentication to proxy");
                return false;
            }
            tracing::debug!(target: "proxy", "SOCKS5 sending proxy authentication {}:{}", auth.username, auth.password);
            let mut ret = [0u8; 2];
            if interruptible_recv(&mut ret, recv_timeout, sock, interrupted).await != IntrRecv::Ok {
                tracing::error!("Error reading proxy authentication response");
                return false;
            }
            if ret != [0x01, 0x00] {
                tracing::error!("Proxy authentication unsuccessful");
                return false;
            }
        }
        (NOAUTH, _) => {}
        (method, _) => {
            tracing::error!("Proxy requested wrong authentication method {method:02x}");
            return false;
        }
    }
    // VER, CMD, RSV, ATYP, the name (its length checked above) and the port.
    let mut req = vec![SOCKS5, CONNECT, 0x00, ATYP_DOMAINNAME, dest.len() as u8];
    req.extend_from_slice(dest.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    if !send(sock, &req).await {
        tracing::error!("Error sending to proxy");
        return false;
    }
    let mut ret2 = [0u8; 4];
    match interruptible_recv(&mut ret2, recv_timeout, sock, interrupted).await {
        IntrRecv::Ok => {}
        // A time-out here is a time-out connecting to the peer, common with Tor: no message.
        IntrRecv::Timeout => return false,
        _ => {
            tracing::error!("Error while reading proxy response");
            return false;
        }
    }
    if ret2[0] != SOCKS5 {
        tracing::error!("Proxy failed to accept request");
        return false;
    }
    if ret2[1] != 0x00 {
        // A failure to reach the peer, not a proxy error.
        tracing::info!(
            "Socks5() connect to {dest}:{port} failed: {}",
            socks5_error_string(ret2[1])
        );
        return false;
    }
    if ret2[2] != 0x00 {
        tracing::error!("Error: malformed proxy response");
        return false;
    }
    let mut ret3 = [0u8; 256];
    let recvr = match ret2[3] {
        ATYP_IPV4 => interruptible_recv(&mut ret3[..4], recv_timeout, sock, interrupted).await,
        ATYP_IPV6 => interruptible_recv(&mut ret3[..16], recv_timeout, sock, interrupted).await,
        ATYP_DOMAINNAME => {
            if interruptible_recv(&mut ret3[..1], recv_timeout, sock, interrupted).await
                != IntrRecv::Ok
            {
                tracing::error!("Error reading from proxy");
                return false;
            }
            let n = usize::from(ret3[0]);
            interruptible_recv(&mut ret3[..n], recv_timeout, sock, interrupted).await
        }
        _ => {
            tracing::error!("Error: malformed proxy response");
            return false;
        }
    };
    if recvr != IntrRecv::Ok
        || interruptible_recv(&mut ret3[..2], recv_timeout, sock, interrupted).await != IntrRecv::Ok
    {
        tracing::error!("Error reading from proxy");
        return false;
    }
    tracing::debug!(target: "net", "SOCKS5 connected {dest}");
    true
}

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;
