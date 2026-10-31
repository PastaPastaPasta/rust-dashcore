//! `InitBinds`, `Bind` and `BindListenPort` (P2a §3.8.9, v24 `net.cpp:3768-3989`), and the
//! accept loop that hands each accepted socket to SB6's `CreateNodeFromAcceptedSocket`
//! ([`NetCore::accept`]).
//!
//! v24 only (DEC-82): `bind_failure_fatal` holds. Any failed `-bind`, `-whitebind` or onion bind,
//! or a failed `0.0.0.0` default bind, stops startup; only the default `[::]` bind may fail.

use std::sync::Arc;

use dash_p2p::local::LocalScore;
use dash_p2p::sock::network_error_string;
use dash_p2p::{Address, NetCore, Permissions, Service, ServiceFlags, TcpSocket};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::TcpListener;
use tokio::sync::watch;

/// Core's `PACKAGE_NAME`, the default product name in the bind error text
/// ([`crate::ConnManOptions::package_name`]).
pub const PACKAGE_NAME: &str = "Dash Core";

/// What to bind (Core's `CConnman::Options` `vBinds`, `vWhiteBinds`, `onion_binds`,
/// `bind_on_any`).
#[derive(Clone, Debug, Default)]
pub struct BindOptions {
    /// `-bind` addresses (not `=onion`).
    pub binds: Vec<Service>,
    /// `-whitebind` addresses with their permissions.
    pub white_binds: Vec<(Service, Permissions)>,
    /// `-bind=…=onion` addresses.
    pub onion_binds: Vec<Service>,
    /// No `-bind` or `-whitebind` was given: bind `[::]` and `0.0.0.0` on the listen port.
    pub bind_on_any: bool,
}

/// One listening socket (Core's `ListenSocket`).
#[derive(Debug)]
pub struct Listener {
    /// The bound socket.
    pub socket: TcpListener,
    /// The permissions accepted peers get (`-whitebind`).
    pub permissions: Permissions,
    /// Core's `m_onion_binds` (every `-bind=…=onion` address): an accepted connection whose
    /// local address is one of them is an inbound onion peer.
    pub onion_binds: Arc<[Service]>,
}

/// `InitBinds` failed; startup stops with Core's `"Failed to listen on any port. Use -listen=0 if
/// you want this."`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindError {
    /// The error `Bind` reported first (`BF_REPORT_ERROR`).
    pub reported: String,
}

impl core::fmt::Display for BindError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Failed to listen on any port. Use -listen=0 if you want this.")
    }
}

impl std::error::Error for BindError {}

/// How long the accept loop backs off after an accept error: Core's loop waits for socket
/// events at most `SELECT_TIMEOUT_MILLISECONDS` per pass.
const ACCEPT_ERROR_BACKOFF: std::time::Duration = std::time::Duration::from_millis(50);

/// Core's `SOMAXCONN` backlog.
#[cfg(unix)]
const BACKLOG: i32 = libc::SOMAXCONN;
#[cfg(not(unix))]
const BACKLOG: i32 = 0x7fff_ffff;

/// Core's `BindListenPort`: a listening socket with `SO_REUSEADDR` and, for IPv6, `IPV6_V6ONLY`.
/// Must run inside the runtime that will own the listener. The error is Core's `strError`, with
/// `package_name` as Core's `PACKAGE_NAME`.
// Core: net.cpp:3768-3841 CConnman::BindListenPort
fn bind_listen_port(addr: &Service, package_name: &str) -> Result<TcpListener, String> {
    // Core's `GetSockAddr`: a CJDNS address as the IPv6 address it is, with the scope id.
    let Some(sa) = addr.to_socket_addr() else {
        let e = format!("Error: Bind address family for {addr} not supported");
        tracing::error!(target: "net", "{e}");
        return Err(e);
    };
    let sock =
        Socket::new(Domain::for_address(sa), Type::STREAM, Some(Protocol::TCP)).map_err(|e| {
            let e = format!(
                "Couldn't open socket for incoming connections (socket returned error {})",
                network_error_string(&e)
            );
            tracing::error!(target: "net", "{e}");
            e
        })?;
    // Allow binding if the port is still in TIME_WAIT state after the program was closed and
    // restarted.
    if let Err(e) = sock.set_reuse_address(true) {
        let e = network_error_string(&e);
        tracing::info!("Error setting SO_REUSEADDR on socket: {e}, continuing anyway");
    }
    // Core: `if (addrBind.IsIPv6())`, which a CJDNS address is not.
    if addr.addr.is_ipv6()
        && let Err(e) = sock.set_only_v6(true)
    {
        let e = network_error_string(&e);
        tracing::info!("Error setting IPV6_V6ONLY on socket: {e}, continuing anyway");
    }
    if let Err(e) = sock.bind(&sa.into()) {
        let e = if e.kind() == std::io::ErrorKind::AddrInUse {
            format!(
                "Unable to bind to {addr} on this computer. {package_name} is probably already running."
            )
        } else {
            let e = network_error_string(&e);
            format!("Unable to bind to {addr} on this computer (bind returned error {e})")
        };
        tracing::error!(target: "net", "{e}");
        return Err(e);
    }
    tracing::info!("Bound to {addr}");
    let listening = sock
        .listen(BACKLOG)
        .and_then(|()| sock.set_nonblocking(true))
        .and_then(|()| TcpListener::from_std(sock.into()));
    listening.map_err(|e| {
        let e = network_error_string(&e);
        let e = format!("Listening for incoming connections failed (listen returned error {e})");
        tracing::error!(target: "net", "{e}");
        e
    })
}

/// Core's `Bind`: binds, then advertises a routable bound address as `LOCAL_BIND` unless
/// `dont_advertise` (`BF_DONT_ADVERTISE`). The error is `BindListenPort`'s.
// Core: net.cpp:3938-3955 CConnman::Bind
fn bind(
    core: &NetCore,
    addr: &Service,
    dont_advertise: bool,
    permissions: Permissions,
    onion_binds: &Arc<[Service]>,
    package_name: &str,
) -> Result<Listener, String> {
    let addr = core.maybe_flip_ipv6_to_cjdns(addr.clone());
    let socket = bind_listen_port(&addr, package_name)?;
    let cfg = core.config();
    if addr.addr.is_routable(cfg.allow_private_net)
        && cfg.discover
        && !dont_advertise
        && !permissions.has(Permissions::NO_BAN)
    {
        core.add_local(addr, LocalScore::Bind);
    }
    Ok(Listener {
        socket,
        permissions,
        onion_binds: Arc::clone(onion_binds),
    })
}

/// Core's `InitBinds` with v24's rules; `package_name` words the bind errors. Must run inside the
/// host's runtime (the listeners register with it).
///
/// # Errors
///
/// [`BindError`] when a bind Core treats as fatal fails.
// Core: net.cpp:3957-3989 CConnman::InitBinds
pub fn init_binds(
    core: &NetCore,
    opts: &BindOptions,
    package_name: &str,
) -> Result<Vec<Listener>, BindError> {
    let fatal = |reported| BindError {
        reported,
    };
    let onion: Arc<[Service]> = opts.onion_binds.as_slice().into();
    let mut out = Vec::new();
    for addr in &opts.binds {
        out.push(bind(core, addr, false, Permissions::NONE, &onion, package_name).map_err(fatal)?);
    }
    for (addr, perms) in &opts.white_binds {
        out.push(bind(core, addr, false, *perms, &onion, package_name).map_err(fatal)?);
    }
    for addr in &opts.onion_binds {
        out.push(bind(core, addr, true, Permissions::NONE, &onion, package_name).map_err(fatal)?);
    }
    if opts.bind_on_any {
        let port = core.config().listen_port;
        // Don't consider errors to bind on IPv6 "::" fatal because the host OS may not have
        // IPv6 support and the user did not explicitly ask us to bind on that.
        let ipv6_any = Service::from_socket_addr((std::net::Ipv6Addr::UNSPECIFIED, port).into());
        out.extend(bind(core, &ipv6_any, false, Permissions::NONE, &onion, package_name).ok());
        let ipv4_any = Service::from_socket_addr((std::net::Ipv4Addr::UNSPECIFIED, port).into());
        out.push(
            bind(core, &ipv4_any, false, Permissions::NONE, &onion, package_name).map_err(fatal)?,
        );
    }
    Ok(out)
}

/// Accepts on `listener` until the core is interrupted or `stop` is set, then closes the
/// listener: Core's `AcceptConnection` per socket, then SB6's `CreateNodeFromAcceptedSocket`.
/// `refuse_unsynced` is Core's `m_active_masternode && !IsBlockchainSynced()`, read per
/// connection.
// Core: net.cpp:1863-1878 CConnman::AcceptConnection
pub async fn accept_loop(
    core: Arc<NetCore>,
    listener: Listener,
    refuse_unsynced: Arc<dyn Fn() -> bool + Send + Sync>,
    mut stop: watch::Receiver<bool>,
) {
    while !core.interrupted() {
        let accepted = tokio::select! {
            biased;
            _ = stop.wait_for(|stopped| *stopped) => break,
            a = listener.socket.accept() => a,
        };
        let accepted = accepted.and_then(|(stream, peer_sa)| {
            let addr_bind = stream.local_addr().ok();
            Ok((TcpSocket::new(stream)?, peer_sa, addr_bind))
        });
        let (sock, peer_sa, addr_bind) = match accepted {
            Ok(a) => a,
            Err(e) => {
                // A `LogPrintf`, without a category (BL-327 D21).
                tracing::info!("socket error accept failed: {}", network_error_string(&e));
                // Out of descriptors or buffers stays an error until something closes: don't
                // spin on it.
                if e.kind() != std::io::ErrorKind::ConnectionAborted {
                    tokio::select! {
                        _ = stop.wait_for(|stopped| *stopped) => break,
                        () = tokio::time::sleep(ACCEPT_ERROR_BACKOFF) => {}
                    }
                }
                continue;
            }
        };
        // Core: `addr_bind{MaybeFlipIPv6toCJDNS(GetBindAddress(*sock))}`; an empty `CService`
        // when `getsockname` fails.
        let addr_bind = core
            .maybe_flip_ipv6_to_cjdns(addr_bind.map(Service::from_socket_addr).unwrap_or_default());
        let addr = Address {
            service: core.maybe_flip_ipv6_to_cjdns(Service::from_socket_addr(peer_sa)),
            services: ServiceFlags::NONE,
            time: 0,
        };
        // Core: net.cpp:1977 `inbound_onion`
        let inbound_onion = listener.onion_binds.contains(&addr_bind);
        core.accept(
            Arc::new(sock),
            addr,
            addr_bind,
            listener.permissions,
            inbound_onion,
            refuse_unsynced(),
        );
    }
}
