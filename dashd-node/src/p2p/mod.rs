//! The node's network (P3 S6a): SB6's network core, P2a-net's connection manager and P2a-core's
//! processor with P3's block download, over V, in Core's init and shutdown order.
//!
//! - **Step 6** ([`Net::new`], [`Net::check_options`]): the network core, which loads `peers.dat`
//!   and the ban list as `CConnman` and `BanMan` do there; after the spork checks, the port and
//!   socket-address checks and the identity's `-uacomment` checks. The core's events are bound
//!   later: no peer connects before step 12.
//! - **Step 7c** ([`Net::attach`]): `PeerManager::make` and `RegisterValidationInterface(peerman)`
//!   after the chainstate load (init.cpp:2194-2199, D112): the processor over V becomes the
//!   core's events, and the listener goes on V's queue.
//! - **Step 12** ([`Net::start`]): the binds and the `-listenonion` checks, `Discover`, the
//!   `-whitelist` check, the `msghand` thread, then `CConnman::Start`, which listens (BL-327).
//! - **`Interrupt`** ([`Net::interrupt`]): `CConnman::Interrupt` (init.cpp:294): message
//!   processing first, then the network.
//! - **`Shutdown`** ([`Net::stop`]), after the RPC stop and before the scheduler's
//!   (init.cpp:331-354): unregister the listener; close P3's queue, so a V parked in a signal wait
//!   resumes; `StopThreads` (`msghand`, then the dial threads and accept loops); `StopNodes`
//!   (`peers.dat`, the anchors, the peers deleted, once started; the ban list always). V stops
//!   later, in `ChainSteps::shutdown`, so it serves the actor until the actor has joined (P3 §2.6).
//!
//! Dropping a [`Net`] stops it, so a failed init leaves nothing running.

#[cfg(test)]
mod amb;
mod chain;
mod listener;
#[cfg(test)]
mod realv;
#[cfg(test)]
mod realv_peers;
mod tor;

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dash_args::ArgsManager;
use dash_chain::SporkView;
use dash_clock::Clock;
use dash_netaddr::{is_bad_port, parse_u16, split_host_port_valid};
use dash_p2p::ban::DEFAULT_BAN_TIME;
use dash_p2p::local::LocalScore;
use dash_p2p::lookup::lookup_port;
use dash_p2p::permissions::{parse_whitebind, parse_whitelist};
use dash_p2p::proxy::{Proxy, ProxyAddr, is_unix_socket_path};
use dash_p2p::rng::FastRandom;
use dash_p2p::{
    LocalIdentity, NetAddr, NetConfig, NetCore, NetEvents, Network, PeerHandle, Permissions,
    Service, ServiceFlags, SubNet,
};
use dash_params::ChainParams;
use dashd_chainstate::chain::{AbortSink, BlockFiles, ValidationInterface, ValidationSignals};
use dashd_masternode::sporks::{SporkHandler, SporkManager};
use dashd_masternode::sync::MasternodeSync;
use dashd_masternode::sync_net::SyncStatusHandler;
use dashd_net::binds::BindOptions;
use dashd_net::torcontrol::TorControl;
use dashd_net::{ConnMan, ConnManOptions, FileStore, NodeAddressRelay, StartError, SystemResolver};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity, local_services};
use dashd_p2p::processing::download::BlockDownload;
use dashd_p2p::processing::seams::{ChainAccess, OverlayState, SeenChainLocks};
use dashd_p2p::{MessageLoop, PeerManager, ProcessorOptions, Registry, Seams, Shared};

pub use chain::NodeChain;
pub use listener::Listener;

use crate::InitFailure;
use crate::chain::Chain;
use crate::init::checks::NetSetup;
use crate::init::consts::{
    DEFAULT_BLOCKSONLY, DEFAULT_CONNECT_TIMEOUT, DEFAULT_DNSSEED, DEFAULT_LISTEN,
    DEFAULT_LISTEN_ONION, DEFAULT_NAME_LOOKUP, DEFAULT_PEERBLOOMFILTERS, DEFAULT_PROXYRANDOMIZE,
    DEFAULT_WHITELISTFORCERELAY, DEFAULT_WHITELISTRELAY,
};
use crate::init::flags::connect_timeout_ms;
use crate::init::interaction::{Network as ParsedNetwork, parse_network};
use crate::logging::Logger;
use crate::logging::set_thread_name;
use crate::mnsync::{SyncListener, SyncParts};
use crate::ui::Ui;

/// `ParseNetwork`'s answer as the core's network; `None` for `NET_UNROUTABLE` (unknown names).
fn p2p_network(net: ParsedNetwork) -> Option<Network> {
    Some(match net {
        ParsedNetwork::Unroutable => return None,
        ParsedNetwork::Ipv4 => Network::Ipv4,
        ParsedNetwork::Ipv6 => Network::Ipv6,
        ParsedNetwork::Onion => Network::Onion,
        ParsedNetwork::I2p => Network::I2p,
        ParsedNetwork::Cjdns => Network::Cjdns,
    })
}

/// Core's `DEFAULT_FIXEDSEEDS`.
const DEFAULT_FIXEDSEEDS: bool = true;
/// Core's `DEFAULT_PEER_CONNECT_TIMEOUT`, in seconds.
const DEFAULT_PEER_CONNECT_TIMEOUT: i64 = 60;
/// Core's `DEFAULT_MAXSENDBUFFER`, in units of 1000 bytes.
const DEFAULT_MAXSENDBUFFER: i64 = 1000;
/// Core's `DEFAULT_MAXRECEIVEBUFFER`, in units of 1000 bytes.
const DEFAULT_MAXRECEIVEBUFFER: i64 = 5000;
/// Core's `MAX_OUTBOUND_FULL_RELAY_CONNECTIONS`.
const MAX_OUTBOUND_FULL_RELAY_CONNECTIONS: usize = 8;
/// Core's `MAX_BLOCK_RELAY_ONLY_CONNECTIONS`.
const MAX_BLOCK_RELAY_ONLY_CONNECTIONS: usize = 2;
/// Core's `MAX_DESIRED_ONION_CONNECTIONS`.
const MAX_DESIRED_ONION_CONNECTIONS: usize = 2;
/// The socket tasks' workers. Core runs one socket thread (`net`).
const NET_WORKERS: usize = 2;

/// The identity's options and the network core's settings from the arguments: Core's step 6
/// (`fListen`, `fDiscover`, `ignores_incoming_txs`, `GetListenPort`) and step 12 `connOptions`,
/// for the options rdashd takes. Their checks come later in step 6 ([`Net::check_options`]).
/// Step 3 has already refused a non-positive `-peertimeout` (`init/flags.rs`). The core takes its
/// whitelist here; a bad `-whitelist` entry is left out and reported at step 12, where Core
/// parses it ([`Net::start`]).
// Core: init.cpp:1655-1657, :1713, :2606-2618, :2706-2727, net.cpp:150-174, :1888-1889
fn net_config(
    args: &ArgsManager,
    params: &Arc<ChainParams>,
    net: &NetSetup,
) -> Result<(NetConfig, IdentityOptions), InitFailure> {
    let ident = IdentityOptions {
        ua_comments: args.get_args("-uacomment")?,
        v2_transport: net.v2_transport,
        peer_bloom_filters: args.get_bool_arg("-peerbloomfilters", DEFAULT_PEERBLOOMFILTERS)?,
        ..IdentityOptions::new()
    };
    let mut cfg = NetConfig::new(Arc::clone(params), local_services(&ident));
    let max = usize::try_from(net.max_connections).unwrap_or(0);
    cfg.max_connections = max;
    cfg.max_outbound_full_relay = MAX_OUTBOUND_FULL_RELAY_CONNECTIONS.min(max);
    cfg.max_outbound_block_relay =
        MAX_BLOCK_RELAY_ONLY_CONNECTIONS.min(max - cfg.max_outbound_full_relay);
    cfg.max_outbound_onion = MAX_DESIRED_ONION_CONNECTIONS.min(max / 2);
    cfg.peer_timeout = args.get_int_arg("-peertimeout", DEFAULT_PEER_CONNECT_TIMEOUT)?;
    cfg.send_buffer_max = buffer_size(args.get_int_arg("-maxsendbuffer", DEFAULT_MAXSENDBUFFER)?);
    cfg.recv_flood_size =
        buffer_size(args.get_int_arg("-maxreceivebuffer", DEFAULT_MAXRECEIVEBUFFER)?);
    cfg.ban_time = args.get_int_arg("-bantime", DEFAULT_BAN_TIME)?;
    cfg.allow_private_net = net.allow_private_net;
    cfg.dns_seed = args.get_bool_arg("-dnsseed", DEFAULT_DNSSEED)?;
    cfg.fixed_seeds = args.get_bool_arg("-fixedseeds", DEFAULT_FIXEDSEEDS)?;
    cfg.connect = if args.is_arg_set("-connect") {
        let connect = args.get_args("-connect")?;
        Some(if connect == ["0"] {
            Vec::new()
        } else {
            connect
        })
    } else {
        None
    };
    // Core: init.cpp:1657, net.cpp:554-559 (P5a D23)
    cfg.blocks_only = args.get_bool_arg("-blocksonly", DEFAULT_BLOCKSONLY)?;
    cfg.listen = args.get_bool_arg("-listen", DEFAULT_LISTEN)?;
    cfg.discover = args.get_bool_arg("-discover", true)?;
    cfg.listen_port = listen_port(args, params)?;
    // Core: init.cpp:1365-1368 (`nConnectTimeout`, an `int`: a value that narrows to a
    // non-positive one is the default)
    cfg.connect_timeout = Duration::from_millis(
        connect_timeout_ms(args.get_int_arg("-timeout", DEFAULT_CONNECT_TIMEOUT)?).unsigned_abs(),
    );
    // Core: init.cpp:1901 (`fNameLookup`; the core adds `!HaveNameProxy()` per connection)
    cfg.name_lookup = args.get_bool_arg("-dns", DEFAULT_NAME_LOOKUP)?;
    // Every network starts reachable (`g_reachable_nets`); step 6 and step 12 apply `-onlynet`,
    // `-cjdnsreachable`, the onion proxy and `-i2psam` ([`Net::check_options`], [`Net::start`]).
    cfg.reachable_overlays = vec![Network::Onion, Network::I2p, Network::Cjdns];
    // The core takes its whitelist now; Core parses it at step 12, when CJDNS reachability is
    // known, and so does [`Net::start`] for its error.
    cfg.whitelist = whitelist(args, cjdns_reachable(args)?).unwrap_or_default();
    cfg.whitelist_relay = args.get_bool_arg("-whitelistrelay", DEFAULT_WHITELISTRELAY)?;
    cfg.whitelist_force_relay =
        args.get_bool_arg("-whitelistforcerelay", DEFAULT_WHITELISTFORCERELAY)?;
    Ok((cfg, ident))
}

/// Whether step 6 leaves CJDNS reachable: `-cjdnsreachable` is set (Core tests `IsArgSet`, so
/// `-cjdnsreachable=0` counts) and `-onlynet`, if given, names `cjdns`.
// Core: init.cpp:1870-1886
pub(crate) fn cjdns_reachable(args: &ArgsManager) -> Result<bool, InitFailure> {
    Ok(args.is_arg_set("-cjdnsreachable")
        && (!args.is_arg_set("-onlynet")
            || args.get_args("-onlynet")?.iter().any(|n| n.eq_ignore_ascii_case("cjdns"))))
}

/// A `-maxsendbuffer` or `-maxreceivebuffer` value in bytes: Core multiplies the `int64_t` by
/// 1000 and stores the product in an `unsigned int` (`CConnman::Options`), so it wraps modulo
/// 2^32 and a negative value becomes a large limit. Neither option is checked.
fn buffer_size(thousands: i64) -> usize {
    thousands.wrapping_mul(1000) as u32 as usize
}

/// The options whose values step 6 checks as socket addresses, and whether each takes a UNIX
/// socket path (`unix:<path>`).
// Core: init.cpp:1773-1799
const SOCKET_ARGS: &[(&str, bool)] = &[
    ("-i2psam", false),
    ("-onion", true),
    ("-proxy", true),
    ("-rpcbind", false),
    ("-torcontrol", false),
    ("-whitebind", false),
    ("-zmqpubhashblock", true),
    ("-zmqpubhashchainlock", true),
    ("-zmqpubhashgovernanceobject", true),
    ("-zmqpubhashgovernancevote", true),
    ("-zmqpubhashinstantsenddoublespend", true),
    ("-zmqpubhashrecoveredsig", true),
    ("-zmqpubhashtx", true),
    ("-zmqpubhashtxlock", true),
    ("-zmqpubrawblock", true),
    ("-zmqpubrawchainlock", true),
    ("-zmqpubrawchainlocksig", true),
    ("-zmqpubrawgovernancevote", true),
    ("-zmqpubrawgovernanceobject", true),
    ("-zmqpubrawinstantsenddoublespend", true),
    ("-zmqpubrawrecoveredsig", true),
    ("-zmqpubrawtx", true),
    ("-zmqpubrawtxlock", true),
    ("-zmqpubrawtxlocksig", true),
    ("-zmqpubsequence", true),
];

/// Step 6's checks of the ports and socket addresses: a set `-port` or `-rpcport` is a port
/// (`ParseUInt16`, not 0); each [`SOCKET_ARGS`] value and each `-bind` address (before its last
/// `=`) is `SplitHostPort`-valid, or a UNIX socket path where the option takes one.
// Core: init.cpp:1759-1825
fn check_socket_args(args: &ArgsManager) -> Result<(), InitFailure> {
    let invalid = |name: &str, value: &str| {
        InitFailure::Error(format!("Invalid port specified in {name}: '{value}'"))
    };
    for name in ["-port", "-rpcport"] {
        if args.is_arg_set(name) {
            let port = args.get_arg(name, "")?;
            if !parse_u16(&port).is_some_and(|n| n != 0) {
                return Err(invalid(name, &port));
            }
        }
    }
    for &(name, unix_socket) in SOCKET_ARGS {
        for addr in args.get_args(name)? {
            // Core: init.cpp:1806-1810 (HAVE_SOCKADDR_UN; ADDR_PREFIX_UNIX)
            let unix_path = cfg!(unix) && unix_socket && addr.starts_with("unix:");
            if !split_host_port_valid(&addr) && !unix_path {
                return Err(invalid(name, &addr));
            }
        }
    }
    for addr in args.get_args("-bind")? {
        let socket = addr.rfind('=').map_or(addr.as_str(), |at| &addr[..at]);
        if !split_host_port_valid(socket) {
            return Err(invalid("-bind", &addr));
        }
    }
    Ok(())
}

/// Core's `GetListenPort`: the port of the first `-bind` that names one, else of the first
/// `-whitebind` without `NoBan`, else [`port_arg`]. It is the port our addresses are advertised
/// on.
// Core: net.cpp:150-174 GetListenPort
fn listen_port(args: &ArgsManager, params: &ChainParams) -> Result<u16, InitFailure> {
    for arg in args.get_args("-bind")? {
        if let Some(addr) = lookup_port(&arg, 0)
            && addr.port != 0
        {
            return Ok(addr.port);
        }
    }
    for arg in args.get_args("-whitebind")? {
        if let Ok((addr, perms)) = parse_whitebind(&arg)
            && !perms.has(Permissions::NO_BAN)
        {
            return Ok(addr.port);
        }
    }
    port_arg(args, params)
}

/// `-port`, else the chain's default port: the port a `-bind` without one binds on. Step 6 has
/// checked a set `-port`.
// Core: net.cpp:172-173, init.cpp:2628-2630
fn port_arg(args: &ArgsManager, params: &ChainParams) -> Result<u16, InitFailure> {
    // `static_cast<uint16_t>`.
    Ok(args.get_int_arg("-port", i64::from(params.default_port))? as u16)
}

/// Core's `-whitelist` entries (`NetWhitelistPermissions::TryParse`). `cjdns_reachable` is
/// `g_reachable_nets.Contains(NET_CJDNS)`.
// Core: init.cpp:2708-2712
fn whitelist(
    args: &ArgsManager,
    cjdns_reachable: bool,
) -> Result<Vec<(SubNet, Permissions)>, InitFailure> {
    args.get_args("-whitelist")?
        .iter()
        .map(|net| parse_whitelist(net, cjdns_reachable).map_err(InitFailure::Error))
        .collect()
}

/// Core's `BadPortWarning`.
// Core: init.cpp:2632-2639
fn bad_port_warning(prefix: &str, port: u16) -> String {
    format!(
        "{prefix} request to listen on port {port}. This port is considered \"bad\" and thus it is unlikely that any peer will connect to it. See doc/p2p-bad-ports.md for details and a full list."
    )
}

/// Step 12's binds: each `-bind` (`addr[:port]`, by default on `-port` or the chain's port, or
/// `addr[:port]=onion` on the onion target port), each `-whitebind`, `bind_on_any` without
/// either, Core's default onion target (127.0.0.1 on the onion target port) without a `-bind`,
/// then the `-listenonion` checks. `warn` is `InitWarning`, called where Core warns. rdashd
/// starts no Tor control: the checks before `StartTorControl` run, the call does not (BL-327
/// D4).
// Core: init.cpp:2628-2704, torcontrol.cpp:708-713 DefaultOnionServiceTarget
fn bind_options(
    args: &ArgsManager,
    params: &ChainParams,
    warn: &dyn Fn(&str),
) -> Result<BindOptions, InitFailure> {
    let default_bind_port = port_arg(args, params)?;
    let onion_port = args.base_params().onion_service_target_port();
    let mut opts = BindOptions::default();
    for arg in args.get_args("-bind")? {
        let bound = match arg.rfind('=') {
            None => lookup_port(&arg, default_bind_port).map(|addr| {
                if is_bad_port(addr.port) {
                    warn(&bad_port_warning("-bind", addr.port));
                }
                opts.binds.push(addr);
            }),
            Some(at) if &arg[at + 1..] == "onion" => {
                lookup_port(&arg[..at], onion_port).map(|addr| opts.onion_binds.push(addr))
            }
            Some(_) => None,
        };
        if bound.is_none() {
            return Err(InitFailure::Error(format!("Cannot resolve -bind address: '{arg}'")));
        }
    }
    for arg in args.get_args("-whitebind")? {
        opts.white_binds.push(parse_whitebind(&arg).map_err(InitFailure::Error)?);
    }
    opts.bind_on_any =
        args.get_args("-bind")?.is_empty() && args.get_args("-whitebind")?.is_empty();
    // `-port` is ignored when `-bind` or `-whitebind` is given.
    if opts.bind_on_any && args.is_arg_set("-port") && is_bad_port(default_bind_port) {
        warn(&bad_port_warning("-port", default_bind_port));
    }
    if opts.onion_binds.is_empty() && opts.binds.is_empty() {
        opts.onion_binds.push(Service::new(NetAddr::ipv4([127, 0, 0, 1]), onion_port));
    }
    if args.get_bool_arg("-listenonion", DEFAULT_LISTEN_ONION)? {
        let Some(target) = opts.onion_binds.first() else {
            return Err(InitFailure::Error("The automatic Tor onion service requires a dedicated onion bind. Use a specific address such as -bind=127.0.0.1:<port>=onion, or disable the service with -listenonion=0.".into()));
        };
        if opts.onion_binds.iter().any(|b| b.addr.is_bind_any()) {
            return Err(InitFailure::Error("The automatic Tor onion service cannot use a wildcard onion bind because the Tor daemon wouldn't be able to forward incoming connections to us. Use a specific address such as -bind=127.0.0.1:<port>=onion, or disable the service with -listenonion=0.".into()));
        }
        if opts.onion_binds.len() > 1 {
            warn(&format!(
                "More than one onion bind address is provided. Using {target} for the automatically created Tor onion service."
            ));
        }
    }
    Ok(opts)
}

/// The core's events, bound at step 7c: Core's `CConnman` exists from step 6 and gets its
/// message processor only at `Start` (`connOptions.m_msgproc`). Before the binding no peer
/// exists, so only a `wake` (from no one) can arrive, and it has nothing to wake.
#[derive(Default)]
struct LateEvents(OnceLock<Arc<dyn NetEvents>>);

impl LateEvents {
    fn bound(&self) -> &Arc<dyn NetEvents> {
        self.0.get().expect("no peer before step 12, and the processor is bound at 7c")
    }
}

impl NetEvents for LateEvents {
    fn wake(&self) {
        if let Some(e) = self.0.get() {
            e.wake();
        }
    }

    fn initialize(&self, core: &NetCore, peer: &PeerHandle, our_services: ServiceFlags) {
        self.bound().initialize(core, peer, our_services);
    }

    fn finalize(&self, core: &NetCore, peer: &PeerHandle) {
        self.bound().finalize(core, peer);
    }
}

/// What step 7c adds: the processor over V and its listener.
struct Attached {
    shared: Arc<Shared>,
    signals: ValidationSignals,
    listener: Arc<dyn ValidationInterface>,
    /// `g_ds_notification_interface`'s `mn_sync` part (SP1), when the masternode services are on.
    sync_listener: Option<Arc<dyn ValidationInterface>>,
    /// Built at step 7c, run from step 12.
    lp: Option<MessageLoop>,
}

/// The network from step 6 to `Shutdown`.
pub struct Net {
    rt: tokio::runtime::Runtime,
    core: Arc<NetCore>,
    events: Arc<LateEvents>,
    ident: IdentityOptions,
    identity: Option<LocalIdentity>,
    attached: Option<Attached>,
    peerman: Option<PeerManager>,
    connman: Option<Arc<ConnMan>>,
    /// Step 12's Tor control (`-listenonion`).
    tor: Option<TorControl>,
    /// `InterruptTorControl` has run.
    tor_interrupted: std::sync::atomic::AtomicBool,
    stopped: bool,
}

impl Net {
    /// Step 6: the runtime and the network core, which loads `peers.dat` and the ban list from
    /// `data_dir` (Core's `LoadAddrman`, `BanMan`, `CConnman`). [`Net::check_options`] follows
    /// the spork checks.
    ///
    /// # Errors
    ///
    /// Core's init errors for the address files, and a runtime that cannot start.
    // Core: init.cpp:1690-1739
    pub fn new(
        args: &ArgsManager,
        params: &Arc<ChainParams>,
        net: &NetSetup,
        clock: Arc<dyn Clock>,
        data_dir: &Path,
    ) -> Result<Net, InitFailure> {
        let (cfg, ident) = net_config(args, params, net)?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(NET_WORKERS)
            .thread_name("net")
            .on_thread_start(|| set_thread_name("net"))
            .enable_io()
            .enable_time()
            .build()
            .map_err(|e| InitFailure::Exception(e.to_string()))?;
        let events = Arc::new(LateEvents::default());
        let core = NetCore::new(
            cfg,
            rt.handle().clone(),
            Arc::new(FileStore::new(data_dir)),
            Arc::new(SystemResolver::default()),
            clock,
            Arc::clone(&events) as _,
        )
        .map_err(|e| InitFailure::Error(e.to_string()))?;
        // The connection manager's constructor ends with `SetNetworkActive`.
        // Core: init.cpp:1715-1717, net.cpp:3929
        core.set_network_active(args.get_bool_arg("-networkactive", true)?);
        Ok(Net {
            rt,
            core,
            events,
            ident,
            identity: None,
            attached: None,
            peerman: None,
            connman: None,
            tor: None,
            tor_interrupted: std::sync::atomic::AtomicBool::new(false),
            stopped: false,
        })
    }

    /// Step 6's option checks, after the spork manager's: the ports and socket addresses
    /// ([`check_socket_args`]), the identity's `-uacomment` checks, then the reachable networks
    /// and proxies ([`Net::reachability_and_proxies`]).
    ///
    /// # Errors
    ///
    /// Core's init errors for the ports and socket addresses, `-uacomment`, `-onlynet`,
    /// `-proxy`, `-onion` and `-externalip`.
    // Core: init.cpp:1759-1981
    pub fn check_options(
        &mut self,
        args: &ArgsManager,
        logger: &Logger,
    ) -> Result<(), InitFailure> {
        check_socket_args(args)?;
        let identity = local_identity(&self.core.config().params, &self.ident)
            .map_err(|e| InitFailure::Error(e.to_string()))?;
        self.identity = Some(identity);
        self.reachability_and_proxies(args, logger)
    }

    /// The end of step 6: `-onlynet` and `-cjdnsreachable` decide the reachable networks; the
    /// explicit `-dnsseed=1` check; `-proxy` becomes the IPv4, IPv6, CJDNS and name proxy, and
    /// the onion proxy unless `-onion` overrides it; without an onion proxy Tor is unreachable;
    /// then each `-externalip` is a local address. A proxy given by name is looked up only
    /// with `-dns`.
    // Core: init.cpp:1870-1981
    fn reachability_and_proxies(
        &self,
        args: &ArgsManager,
        logger: &Logger,
    ) -> Result<(), InitFailure> {
        let core = &self.core;
        let error = |msg: String| Err(InitFailure::Error(msg));
        let onlynet = args.is_arg_set("-onlynet");
        if onlynet {
            for net in Network::ALL {
                core.set_reachable(net, false);
            }
            for name in args.get_args("-onlynet")? {
                let Some(net) = p2p_network(parse_network(logger, &name)) else {
                    return error(format!("Unknown network specified in -onlynet: '{name}'"));
                };
                core.set_reachable(net, true);
            }
        }
        if !args.is_arg_set("-cjdnsreachable") {
            if onlynet && core.is_reachable(Network::Cjdns) {
                return error("Outbound connections restricted to CJDNS (-onlynet=cjdns) but -cjdnsreachable is not provided".into());
            }
            core.set_reachable(Network::Cjdns, false);
        }
        if args.get_bool_arg("-dnsseed", DEFAULT_DNSSEED)?
            && !core.is_reachable(Network::Ipv4)
            && !core.is_reachable(Network::Ipv6)
        {
            return error("Incompatible options: -dnsseed=1 was explicitly specified, but -onlynet forbids connections to IPv4/IPv6".into());
        }
        let name_lookup = core.config().name_lookup;
        let randomize = args.get_bool_arg("-proxyrandomize", DEFAULT_PROXYRANDOMIZE)?;
        // A `unix:` path, else `host[:port]` (port 9050) looked up as Core's `Lookup`.
        let proxy_for = |arg: &str| -> Option<Proxy> {
            let addr = if is_unix_socket_path(arg) {
                ProxyAddr::Unix(arg.to_owned())
            } else {
                let service = self.rt.block_on(core.lookup_service(arg, 9050, name_lookup))?;
                if !service.addr.is_valid() {
                    return None;
                }
                ProxyAddr::Service(service)
            };
            Some(Proxy {
                addr,
                randomize_credentials: randomize,
            })
        };
        let mut onion_proxy = None;
        let proxy_arg = args.get_arg("-proxy", "")?;
        if !proxy_arg.is_empty() && proxy_arg != "0" {
            let Some(proxy) = proxy_for(&proxy_arg) else {
                return error(format!("Invalid -proxy address or hostname: '{proxy_arg}'"));
            };
            // Core ignores `SetProxy`'s and `SetNameProxy`'s result: `proxy_for` has already
            // refused an invalid proxy, so neither can fail.
            for net in [Network::Ipv4, Network::Ipv6, Network::Cjdns] {
                let _ = core.set_proxy(net, proxy.clone());
            }
            let _ = core.set_name_proxy(proxy.clone());
            onion_proxy = Some(proxy);
        }
        let onlynet_used_with_onion = onlynet && core.is_reachable(Network::Onion);
        let onion_arg = args.get_arg("-onion", "")?;
        if onion_arg == "0" {
            onion_proxy = None;
            if onlynet_used_with_onion {
                return error("Outbound connections restricted to Tor (-onlynet=onion) but the proxy for reaching the Tor network is explicitly forbidden: -onion=0".into());
            }
        } else if !onion_arg.is_empty() {
            let Some(proxy) = proxy_for(&onion_arg) else {
                return error(format!("Invalid -onion address or hostname: '{onion_arg}'"));
            };
            onion_proxy = Some(proxy);
        }
        match onion_proxy {
            Some(proxy) => {
                // Valid, as above.
                let _ = core.set_proxy(Network::Onion, proxy);
            }
            None => {
                // Tor control may give the onion proxy later (P8b).
                let listenonion_disabled =
                    !args.get_bool_arg("-listenonion", DEFAULT_LISTEN_ONION)?;
                if onlynet_used_with_onion && listenonion_disabled {
                    return error("Outbound connections restricted to Tor (-onlynet=onion) but the proxy for reaching the Tor network is not provided: none of -proxy, -onion or -listenonion is given".into());
                }
                core.set_reachable(Network::Onion, false);
            }
        }
        let port = core.config().listen_port;
        for arg in args.get_args("-externalip")? {
            match self.rt.block_on(core.lookup_service(&arg, port, name_lookup)) {
                Some(addr) if addr.addr.is_valid() => {
                    core.add_local(addr, LocalScore::Manual);
                }
                _ => return error(format!("Cannot resolve -externalip address: '{arg}'")),
            }
        }
        Ok(())
    }

    /// Step 7c: the processor over `chain` becomes the core's events, and the listener goes on
    /// V's signals, before step 11's ABC. The registry holds SP0's spork handler over `sporks`
    /// and SP1's `ssc` handler over `sync`'s state; the ChainLocks seam is P5a's stand-in over
    /// its `SPORK_19` (P5a D15), and the overlay reads `SPORK_2` and the sync (SP1 D9). The
    /// sync's network view counts the processor's object requests from here on.
    // Core: init.cpp:2194-2199, :2305
    pub fn attach(
        &mut self,
        chain: &Chain,
        files: Option<Box<dyn BlockFiles>>,
        clock: Arc<dyn Clock>,
        sporks: &Arc<SporkManager>,
        sync: &SyncParts,
    ) {
        self.attach_with(chain, files, clock, Some((sporks, sync)), |access| access);
    }

    /// [`Net::attach`], with the processor reading the chain through `wrap(access)`: the real-V
    /// tests' seam (S6b). Without the masternode services, the registry has no spork or `ssc`
    /// handler and ChainLocks are off ([`Seams::default`]).
    fn attach_with(
        &mut self,
        chain: &Chain,
        files: Option<Box<dyn BlockFiles>>,
        clock: Arc<dyn Clock>,
        masternode: Option<(&Arc<SporkManager>, &SyncParts)>,
        wrap: impl FnOnce(Arc<NodeChain>) -> Arc<dyn ChainAccess>,
    ) {
        let identity = self.identity.take().expect("attached once");
        let v = chain.handle();
        let access = Arc::new(NodeChain::new(v.clone(), files));
        let reads = wrap(Arc::clone(&access));
        // Core: init.cpp:2198 (`ignores_incoming_txs`, P5a D23)
        let options = ProcessorOptions {
            blocks_only: self.core.config().blocks_only,
            ..ProcessorOptions::default()
        };
        let shared = Shared::new(identity, options, Arc::clone(&reads));
        assert!(self.events.0.set(shared.clone()).is_ok(), "attached once");
        let mut seams = Seams {
            block_sync: Box::new(BlockDownload::new()),
            addr_relay: Box::new(NodeAddressRelay::new(reads)),
            ..Seams::default()
        };
        let mut registry = Registry::new();
        if let Some((sporks, sync)) = masternode {
            registry.register(Box::new(SporkHandler::new(Arc::clone(sporks))));
            registry.register(Box::new(SyncStatusHandler::new(Arc::clone(&sync.sync))));
            let view = Arc::clone(sporks);
            seams.chainlocks = Box::new(SeenChainLocks::new(move || view.chainlocks_enabled()));
            seams.overlay = Box::new(NodeOverlay {
                sporks: Arc::clone(sporks),
                sync: Arc::clone(&sync.sync),
                governance_valid: sync.governance_valid,
            });
        }
        let lp = MessageLoop::new(
            Arc::clone(&self.core),
            Arc::clone(&shared),
            registry,
            seams,
            FastRandom::new(),
        );
        if let Some((_, sync)) = masternode {
            sync.net.bind_requests(lp.logic().requested_objects());
        }
        let listener: Arc<dyn ValidationInterface> = Arc::new(Listener {
            shared: Arc::clone(&shared),
            core: Arc::clone(&self.core),
            chain: access,
            view: v.view_handle(),
            clock,
            wake: Listener::wake_on(&shared.signals().queue),
        });
        let signals = chain.signals().clone();
        signals.register(Arc::clone(&listener));
        // Core: init.cpp:2225-2228
        let sync_listener = masternode.map(|(_, sync)| {
            let l: Arc<dyn ValidationInterface> =
                Arc::new(SyncListener::new(Arc::clone(&sync.sync), v.view_handle()));
            signals.register(Arc::clone(&l));
            l
        });
        self.attached = Some(Attached {
            shared,
            signals,
            listener,
            sync_listener,
            lp: Some(lp),
        });
    }

    /// Step 12: the binds ([`bind_options`]), `Discover` when binding on any address, the
    /// `-whitelist` check, then `threadMessageHandler` and `CConnman::Start`: the listeners when
    /// `-listen`, then the dial threads. `ui` shows Core's init warnings and a failed bind's
    /// message box. A panic that stops `msghand` reaches `abort` (Core's `TraceThread`
    /// terminates dashd; D113).
    ///
    /// # Errors
    ///
    /// Core's init errors for the binds, `-listenonion` and `-whitelist`, and `BindListenPort`'s;
    /// a thread that cannot be spawned.
    // Core: init.cpp:2628-2712, :2755, net.cpp:3991-4120
    pub fn start(
        &mut self,
        args: &ArgsManager,
        ui: &Ui,
        abort: Arc<dyn AbortSink>,
    ) -> Result<(), InitFailure> {
        let binds = bind_options(args, &self.core.config().params, &|w| ui.init_warning(w))?;
        // Core: init.cpp:2714-2728
        if args.get_bool_arg("-listenonion", DEFAULT_LISTEN_ONION)? {
            let target = binds.onion_binds[0].clone();
            let rt = self.rt.handle().clone();
            self.tor = Some(tor::start(&rt, &self.core, args, ui.logger(), target)?);
        }
        if binds.bind_on_any {
            // Only every address of the machine when listening on any address.
            self.core.discover(&crate::host::interface_addrs());
        }
        whitelist(args, self.core.is_reachable(Network::Cjdns))?;
        // Core: init.cpp:2746-2758
        if args.is_arg_set("-connect")
            && args.is_arg_set("-dnsseed")
            && args.get_bool_arg("-dnsseed", DEFAULT_DNSSEED)?
            && args.is_arg_set("-proxy")
        {
            tracing::info!("-dnsseed is ignored when -connect is used and -proxy is specified");
        }
        // `-i2psam` stays refused until P8c, so I2P is unreachable.
        // Core: init.cpp:2760-2775
        if args.is_arg_set("-onlynet") && self.core.is_reachable(Network::I2p) {
            return Err(InitFailure::Error(
                "Outbound connections restricted to i2p (-onlynet=i2p) but -i2psam is not provided"
                    .into(),
            ));
        }
        self.core.set_reachable(Network::I2p, false);
        let at = self.attached.as_mut().expect("attached at step 7c");
        let lp = at.lp.take().expect("started once");
        let on_panic = Box::new(move || {
            abort.abort_node("The message handler thread panicked", None);
        });
        let pm = PeerManager::try_spawn(lp, on_panic)
            // `std::thread`'s `system_error`, which `AppInit` catches as an exception.
            .map_err(|e| InitFailure::Exception(e.to_string()))?;
        let dial = at.shared.dial_policy();
        self.peerman = Some(pm);
        let cm = ConnMan::start(
            Arc::clone(&self.core),
            self.rt.handle().clone(),
            dial,
            ConnManOptions {
                binds,
                package_name: crate::PACKAGE_NAME,
                ..ConnManOptions::default()
            },
        )
        .map_err(|e| match e {
            // `Bind`'s message box (`BF_REPORT_ERROR`), then `Start`'s.
            // Core: net.cpp:3943-3947, :4006-4012
            StartError::Bind(e) => {
                ui.init_error(&e.reported);
                InitFailure::Error(e.to_string())
            }
            e => InitFailure::Exception(e.to_string()),
        })?;
        self.connman = Some(Arc::new(cm));
        Ok(())
    }

    /// The connection manager, from step 12.
    pub fn connman(&self) -> Option<&Arc<ConnMan>> {
        self.connman.as_ref()
    }

    /// The processor's shared half, from step 7c.
    pub fn shared(&self) -> Option<&Arc<Shared>> {
        self.attached.as_ref().map(|a| &a.shared)
    }

    /// The network core.
    pub fn core(&self) -> &Arc<NetCore> {
        &self.core
    }

    /// Core's `CConnman::Interrupt`: message processing (`flagInterruptMsgProc`) first, then the
    /// network, the dial loops and new connections.
    // Core: init.cpp:289-295, net.cpp:4144-4166 CConnman::Interrupt
    pub fn interrupt(&self) {
        // Core: init.cpp:290 InterruptTorControl, before the network's.
        if let Some(tor) = &self.tor
            && !self.tor_interrupted.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            tor.interrupt();
        }
        if let Some(at) = &self.attached {
            at.shared.interrupt();
        }
        match &self.connman {
            Some(cm) => cm.interrupt(),
            None => self.core.interrupt(),
        }
    }

    /// `Shutdown`'s network part, once: unregister the listener and close P3's queue, then
    /// Core's `CConnman::Stop` in its two halves. `StopThreads` joins `msghand` first, then the
    /// dial threads and accept loops. The processor's handlers stop after the join, on the loop
    /// that owns them (Core's `InterruptHandlers` and `StopHandlers` run before it, with
    /// msghand still running; no installed handler waits, D112). `StopNodes`
    /// (`NetCore::shutdown`) then dumps `peers.dat` and the anchors if `Start` ran, deletes the
    /// peers, and dumps the ban list, as `~BanMan` does whether or not it ran. Each part runs only
    /// for what init built (ND1-D27).
    // Core: init.cpp:337, :347-348, net.cpp:4168-4210 CConnman::StopThreads, CConnman::StopNodes
    pub fn stop(&mut self) {
        if std::mem::replace(&mut self.stopped, true) {
            return;
        }
        if let Some(at) = &self.attached {
            at.signals.unregister(&at.listener);
            if let Some(l) = &at.sync_listener {
                at.signals.unregister(l);
            }
            at.shared.signals().queue.close();
        }
        self.interrupt();
        // StopThreads.
        if let Some(pm) = self.peerman.take() {
            pm.stop();
        }
        if let Some(mut lp) = self.attached.as_mut().and_then(|at| at.lp.take()) {
            lp.stop_handlers();
        }
        if let Some(cm) = &self.connman {
            cm.stop_threads();
        }
        // StopNodes.
        self.rt.block_on(self.core.shutdown());
        self.connman = None;
        // Core: init.cpp:352 StopTorControl, after `CConnman::Stop`.
        if let Some(tor) = self.tor.take() {
            self.rt.block_on(tor.stop());
        }
    }
}

/// What [`Net::stop`] has left to take down, for the shutdown tests.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Parts {
    /// Step 7c's processor and listener exist.
    pub attached: bool,
    /// `msghand` runs.
    pub msghand: bool,
    /// The connection manager's threads run.
    pub connman: bool,
    /// V's signals hold the listener: its only other owner. A dispatch in flight keeps the old
    /// list (and this count) for its duration; the tests read it with no block in flight.
    pub registered: bool,
    /// V's signals hold SP1's `mn_sync` listener, counted as `registered` is.
    pub sync_registered: bool,
    pub stopped: bool,
}

#[cfg(test)]
impl Net {
    pub(crate) fn parts(&self) -> Parts {
        Parts {
            attached: self.attached.is_some(),
            msghand: self.peerman.is_some(),
            connman: self.connman.is_some(),
            registered: self
                .attached
                .as_ref()
                .is_some_and(|at| Arc::strong_count(&at.listener) > 1),
            sync_registered: self
                .attached
                .as_ref()
                .and_then(|at| at.sync_listener.as_ref())
                .is_some_and(|l| Arc::strong_count(l) > 1),
            stopped: self.stopped,
        }
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        self.stop();
    }
}

/// `AlreadyHave`'s manager state for rdashd (P5a D20). Governance is had while governance is not
/// valid or mnsync's blockchain is not synced, Core's `NetGovernance::AlreadyHave`; rdashd's
/// default is `-disablegovernance=1`, so it is always had until GV1b (SP1 D9, DEC-510).
/// InstantSend is `SPORK_2` read live. Core's `IsInstantSendEnabled` also requires `!fReindex &&
/// !fImporting`, which rdashd cannot be in yet; whoever ports reindex or `-loadblock` adds them,
/// as the `isdlock` inv's blocks-only disconnect (np:4561-4570) reads `AlreadyHave` in IBD too.
struct NodeOverlay {
    sporks: Arc<SporkManager>,
    sync: Arc<MasternodeSync>,
    /// `CGovernanceManager::IsValid()`.
    governance_valid: bool,
}

impl OverlayState for NodeOverlay {
    // Core: governance/net_governance.cpp:273-282; governance/governance.cpp:612-613
    fn governance_ready(&self) -> bool {
        self.governance_valid && self.sync.is_blockchain_synced()
    }

    fn instantsend_enabled(&self) -> bool {
        self.sporks.instantsend_enabled()
    }
}

#[cfg(test)]
mod tests {
    use dash_params::{ChainParamsBuilder, NetworkKind, Release};

    use super::*;
    use crate::init::test_args;

    /// `GetListenPort`: the first `-bind` with a port (not an onion bind, which `Lookup` refuses
    /// whole), else the first `-whitebind` without `noban`, else `-port`, else regtest's 19899.
    // Core: net.cpp:150-174
    #[test]
    fn listen_port_is_cores_get_listen_port() {
        let params = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap();
        let port = |argv: &[&str]| listen_port(&test_args(argv), &params).unwrap();
        assert_eq!(port(&[]), params.default_port);
        assert_eq!(port(&["-port=5"]), 5);
        assert_eq!(port(&["-port=5", "-bind=127.0.0.1", "-bind=127.0.0.1:7"]), 7);
        assert_eq!(port(&["-port=5", "-bind=127.0.0.1:7=onion"]), 5);
        let whitebinds = ["-whitebind=noban@127.0.0.1:8", "-whitebind=relay@127.0.0.1:9"];
        assert_eq!(port(&whitebinds), 9);
        assert_eq!(port(&["-port=5", whitebinds[0]]), 5);
        assert_eq!(port(&["-bind=127.0.0.1:7", whitebinds[1]]), 7);
    }

    /// [`net_config`] on regtest with `argv` and 125 connections.
    fn regtest_net_config(argv: &[&str]) -> NetConfig {
        let params = Arc::new(
            ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap(),
        );
        let setup = NetSetup {
            basic_filter_index: false,
            v2_transport: false,
            compact_filters: false,
            allow_private_net: false,
            user_max_connections: 125,
            max_connections: 125,
            fd: 1024,
        };
        net_config(&test_args(argv), &params, &setup).unwrap().0
    }

    /// The core's `-whitelist` addresses are flipped to CJDNS when step 6 leaves CJDNS reachable
    /// (`-cjdnsreachable`, and `-onlynet` absent or naming it); otherwise they stay IPv6. Only a
    /// single address: a flipped `fc00::/8` is no subnet Core accepts (`CSubNet(addr, mask)`
    /// takes IPv4 and IPv6 only), an init error that `dashd_v24` compares.
    // Core: init.cpp:1880-1887, :2736-2741, netbase.cpp:773 MaybeFlipIPv6toCJDNS,
    // netaddress.cpp:925-931, :997-1017
    #[test]
    fn whitelist_subnets_follow_cjdns_reachability() {
        let cjdns = |argv: &[&str]| -> Vec<bool> {
            regtest_net_config(argv).whitelist.iter().map(|(s, _)| s.network().is_cjdns()).collect()
        };
        assert_eq!(cjdns(&["-cjdnsreachable", "-whitelist=fc00::1"]), [true]);
        assert_eq!(
            cjdns(&["-cjdnsreachable", "-onlynet=cjdns", "-whitelist=noban@fc00::1"]),
            [true]
        );
        assert_eq!(cjdns(&["-whitelist=fc00::1", "-whitelist=fc00::/8"]), [false, false]);
        assert_eq!(cjdns(&["-cjdnsreachable", "-onlynet=ipv4", "-whitelist=fc00::1"]), [false]);
    }

    /// `-maxsendbuffer` and `-maxreceivebuffer` reach the core as Core's `unsigned int` byte
    /// counts, `-bantime` as Core's `BanMan` default; nothing checks them.
    // Core: init.cpp:1713, :2617-2618, net.h:139-140, :1234-1235
    #[test]
    fn buffer_and_ban_options_reach_the_core() {
        let limits = |argv: &[&str]| {
            let c = regtest_net_config(argv);
            (c.send_buffer_max, c.recv_flood_size, c.ban_time)
        };
        assert_eq!(limits(&[]), (1_000_000, 5_000_000, 86_400));
        assert_eq!(
            limits(&["-maxsendbuffer=1", "-maxreceivebuffer=100", "-bantime=60"]),
            (1000, 100_000, 60)
        );
        assert_eq!(limits(&["-maxsendbuffer=x", "-maxreceivebuffer=0", "-bantime=-5"]), (0, 0, -5));
        assert_eq!(limits(&["-maxsendbuffer=-1"]).0, 4_294_966_296);
        assert_eq!(limits(&["-maxreceivebuffer=4294968"]).1, 704);
    }
}
