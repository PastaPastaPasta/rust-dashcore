//! [`NetCore`]: the connection table, the per-peer queues and Core's send accounting (SB0 §3.1).
//!
//! Each connection has a reader and a writer task on the host's runtime. The reader feeds the
//! transport and queues completed messages (`ReceiveMsgBytes`, `MarkReceivedMsgsForProcessing`);
//! the writer drains the send queue through the transport with non-blocking sends
//! (`SocketSendData`). One sweep task plays Core's `DisconnectNodes` and `InactivityCheck` on the
//! socket loop's cadence. Nothing else runs in the background.

use std::collections::VecDeque;
use std::future::{Future, poll_fn};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use dash_clock::Clock;
use dash_p2p_core::{CommandString, ServiceFlags};
use dash_p2p_transport::{MAX_GARBAGE_LEN, OutMsg, Transport, V2Transport};
use dash_params::ChainParams;
use parking_lot::{Mutex, RwLock};
use tokio::runtime::Handle;
use tokio::sync::{Notify, Semaphore};
use tokio::task::{JoinHandle, JoinSet};

use crate::addrman::{AddrMan, LoadError, read_anchors, write_anchors};
use crate::ban::{BanList, banlist_json};
use crate::capture::MessageCapture;
use crate::conn::{
    ConnectionType, PauseState, Peer, PeerHandle, PeerId, PeerInit, PeerSnapshot, Permissions, Why,
};
use crate::dial::{AddedNode, DialState, Grant};
use crate::local::{LocalHost, LocalScore, TimeData};
use crate::netaddr::{Address, NetAddr, Network, Service, SubNet};
use crate::netgroup::NetGroupManager;
use crate::proxy::{Proxy, ProxyAddr, ProxyCredentials, ProxyTable, SOCKS5_RECV_TIMEOUT, socks5};
use crate::rng::{FastRandom, OsRand, RandSource, StreamRand, get_rand};
use crate::seeds::{Resolver, default_port_for, lookup};
use crate::siphash::SipHasher;
use crate::sock::{NetSocket, TcpSocket};
use crate::store::{NetBlob, NetStore};
pub(crate) use dash_netaddr::split_host_port;

/// Core's `MAX_BLOCK_RELAY_ONLY_ANCHORS` (`net.cpp:67`).
const MAX_BLOCK_RELAY_ONLY_ANCHORS: usize = 2;
/// The log target of Core's `NET_NETCONN` lines, printed under `-debug=net` or `-debug=netconn`
/// (BL-327 D14).
pub(crate) const NETCONN: &str = "net|netconn";
/// Core's `RANDOMIZER_ID_NETGROUP`.
const RANDOMIZER_ID_NETGROUP: u64 = 0x6c0e_dd80_36ef_4036;
/// Core's `RANDOMIZER_ID_LOCALHOSTNONCE`.
const RANDOMIZER_ID_LOCALHOSTNONCE: u64 = 0xd93e_69e2_bbfa_5735;
/// Core's `TIMEOUT_INTERVAL`: 20 minutes, in seconds.
const TIMEOUT_INTERVAL: i64 = 20 * 60;
/// The socket loop's idle cadence: Core's `SELECT_TIMEOUT_MILLISECONDS` on POSIX. A mark does
/// not wake the sweep, so a peer we disconnect lingers from the first pass after the mark and is
/// removed by the first pass at least 100 ms later. Any peer's read or send runs a pass, as each
/// is a socket event that ends Core's wait, so on a node with traffic both come within ms; on an
/// idle node they are up to 500 ms apart, unless the peer's socket turns readable (D43).
// Core: net.cpp:111-120
const SWEEP_INTERVAL: Duration = Duration::from_millis(500);
/// Core's disconnect linger.
const LINGER: Duration = Duration::from_millis(100);

/// A received message, copied out of the socket buffer (Core's `CNetMessage`; DEC-51h).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawMsg {
    /// The message type; the empty type for a V2 short ID Core reserves without a message.
    pub cmd: CommandString,
    /// The payload, an owned copy.
    pub payload: Vec<u8>,
    /// The socket-read time in microseconds on the node clock (`m_time`).
    pub recv_us: i64,
    /// Bytes the message took on the wire (`m_raw_message_size`), the unit of `fPauseRecv`.
    pub raw_size: usize,
}

/// One message taken from a peer's receive queues (Core's `PollMessage`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Polled {
    /// The message.
    pub msg: RawMsg,
    /// Whether either receive queue still holds a message (Core's `more`, which feeds
    /// `fMoreWork`).
    pub more: bool,
}

/// The network core's options: Core's `CConnman::Options` plus the per-network parameters.
/// Build with [`NetConfig::new`] (the host's services, Core's defaults otherwise) and set fields.
#[derive(Clone)]
#[non_exhaustive]
pub struct NetConfig {
    /// The network's parameters (message start, default port, seeds, group rule).
    pub params: Arc<ChainParams>,
    /// Our services (`nLocalServices`), as the host builds them at init (P2a §3.3.3; Core's are
    /// `NODE_NETWORK | NODE_BLOOM | NODE_HEADERS_COMPRESSED | NODE_P2P_V2` on a full node with the
    /// defaults). `NODE_P2P_V2` here is `-v2transport`: we dial V2 to peers that advertise it and
    /// accept V2 inbound only with it.
    pub local_services: ServiceFlags,
    /// `-maxsendbuffer` × 1000: `fPauseSend` is set above it (`nSendBufferMaxSize`).
    pub send_buffer_max: usize,
    /// `-maxreceivebuffer` × 1000: `fPauseRecv` is set above it (`m_recv_flood_size`).
    pub recv_flood_size: usize,
    /// Serve quorum-priority messages first (Core's two receive queues). SPV turns it off for
    /// one FIFO queue per peer (D12).
    pub priority_queues: bool,
    /// Queue a V1 retry when an outbound V2 connection closes before the peer sent a byte
    /// (Core's `m_reconnections`). Only `dial_tick`'s loops open the queue; a host that runs
    /// none (SPV, D16) turns it off, or the queue grows for the life of the process (D45).
    pub v1_retry: bool,
    /// `nMaxConnections`, after the descriptor clamp.
    pub max_connections: usize,
    /// `m_max_outbound_full_relay`.
    pub max_outbound_full_relay: usize,
    /// `m_max_outbound_block_relay`.
    pub max_outbound_block_relay: usize,
    /// `nMaxFeeler`.
    pub max_feeler: usize,
    /// `nMaxAddnode`.
    pub max_addnode: usize,
    /// Dash's onion full-relay target (`m_max_outbound_onion`).
    pub max_outbound_onion: usize,
    /// `-timeout`: the TCP connect timeout (`nConnectTimeout`, 5000 ms).
    pub connect_timeout: Duration,
    /// `-peertimeout`, in seconds: inactivity checks start this long after connect.
    pub peer_timeout: i64,
    /// `-allowprivatenet`: RFC 1918 addresses count as routable.
    pub allow_private_net: bool,
    /// `-onlynet`: the reachable networks (`g_reachable_nets`); empty for all. The initial
    /// state of [`NetCore::is_reachable`], which [`NetCore::set_reachable`] changes.
    pub only_net: Vec<Network>,
    /// The overlays (Onion, I2P, CJDNS) the host routes itself. Core makes Onion reachable
    /// only with an onion proxy, I2P with `-i2psam` and CJDNS with `-cjdnsreachable`; none
    /// is reachable by default (D27).
    pub reachable_overlays: Vec<Network>,
    /// `-blocksonly`: at shutdown, any peer without the Relay permission is an anchor (D28).
    pub blocks_only: bool,
    /// `-bantime`, in seconds.
    pub ban_time: i64,
    /// `-whitelist`: permissions by subnet for inbound peers.
    pub whitelist: Vec<(SubNet, Permissions)>,
    /// `-whitelistrelay`: implicit whitelist permissions include `Relay`. The host resolves
    /// Core's parameter interactions before setting it: unless `-whitelistrelay` is given,
    /// `-blocksonly=1` makes it false, else `-whitelistforcerelay=1` makes it true (soft-sets in
    /// that order, the first one winning; init.cpp:1058-1070; P4a D4).
    pub whitelist_relay: bool,
    /// `-whitelistforcerelay`: implicit whitelist permissions include `ForceRelay`.
    pub whitelist_force_relay: bool,
    /// We run an active masternode (Core's `m_active_masternode`): fresh inbound peers are
    /// protected from eviction until their first non-`mnauth` message.
    pub active_masternode: bool,
    /// `-checkaddrman`: the ratio of AddrMan consistency checks (0 = never).
    pub check_addrman: u32,
    /// `-dnsseed`.
    pub dns_seed: bool,
    /// Core's `fNameLookup && !HaveNameProxy()` (`-dns`, and no name proxy): whether a named
    /// connection target may be looked up in DNS. DNS seeds always may.
    // Core: net.cpp:405 ConnectNode; init.cpp:1877
    pub name_lookup: bool,
    /// `-forcednsseed`.
    pub force_dns_seed: bool,
    /// `-fixedseeds`.
    pub fixed_seeds: bool,
    /// `-seednode` destinations (queued as ADDR_FETCH at start).
    pub seed_nodes: Vec<String>,
    /// `-connect` destinations; `Some(empty)` is `-connect=0` (no automatic outbound).
    pub connect: Option<Vec<String>>,
    /// `-addnode` destinations.
    pub added_nodes: Vec<String>,
    /// `-listen` (the address-advertisement rules read it).
    pub listen: bool,
    /// `-discover`.
    pub discover: bool,
    /// Our listen port (`GetListenPort`).
    pub listen_port: u16,
    /// `-externalip` addresses.
    pub external_ip: Vec<Service>,
    /// `-maxtimeadjustment`, in seconds: the bound of the peers' median offset.
    pub max_time_adjustment: i64,
    /// `-asmap` bits as Core's `DecodeAsmap` returns them, empty when none is loaded.
    pub asmap: Vec<bool>,
    /// The proxies (`-proxy`, `-onion`; Core's `proxyInfo` and `nameProxy`, P8a): the initial
    /// state of [`NetCore::proxy`], which [`NetCore::set_proxy`] changes. Connections to a
    /// network with a proxy go through it, and our `version` zeroes `addr_you` for a peer whose
    /// address is a proxy's (`IsProxy`).
    pub proxies: ProxyTable,
    /// `-capturemessages`: where each pushed and each processed message is recorded (P4a D10).
    pub capture: Option<Arc<dyn MessageCapture>>,
    /// Deterministic randomness for tests and simulations (`test-utils` only): AddrMan's Core
    /// test mode (`nKey = uint256{1}`, the zero-key context) and every other draw from the public
    /// zero-key stream (D26, D38). V2 session keys, MNAUTH challenges, ping nonces and the
    /// netgroup seeds become predictable and equal on every such core, so a build without
    /// `test-utils` has no such setting.
    #[cfg(any(test, feature = "test-utils"))]
    pub deterministic: bool,
    /// A harness's `GetRandBytes` ([`NetConfig::with_rand_source`]).
    rand: Option<Arc<dyn RandSource>>,
    /// The sweep leaves `InactivityCheck` to [`NetCore::run_inactivity_checks`]
    /// ([`NetConfig::with_manual_inactivity_checks`]).
    manual_inactivity_checks: bool,
}

impl NetConfig {
    /// Core's defaults for `params`, with the host's `local_services`.
    pub fn new(params: Arc<ChainParams>, local_services: ServiceFlags) -> Self {
        let listen_port = params.default_port;
        Self {
            params,
            local_services,
            send_buffer_max: 1000 * 1000,
            recv_flood_size: 5000 * 1000,
            priority_queues: true,
            v1_retry: true,
            max_connections: 125,
            max_outbound_full_relay: 8,
            max_outbound_block_relay: 2,
            max_feeler: 1,
            max_addnode: 8,
            max_outbound_onion: 2,
            connect_timeout: Duration::from_millis(5000),
            peer_timeout: 60,
            allow_private_net: false,
            only_net: Vec::new(),
            reachable_overlays: Vec::new(),
            blocks_only: false,
            ban_time: crate::ban::DEFAULT_BAN_TIME,
            whitelist: Vec::new(),
            whitelist_relay: true,
            whitelist_force_relay: false,
            active_masternode: false,
            check_addrman: 0,
            dns_seed: true,
            name_lookup: true,
            force_dns_seed: false,
            fixed_seeds: true,
            seed_nodes: Vec::new(),
            connect: None,
            added_nodes: Vec::new(),
            listen: true,
            discover: true,
            listen_port,
            external_ip: Vec::new(),
            max_time_adjustment: crate::local::DEFAULT_MAX_TIME_ADJUSTMENT,
            asmap: Vec::new(),
            proxies: ProxyTable::default(),
            capture: None,
            #[cfg(any(test, feature = "test-utils"))]
            deterministic: false,
            rand: None,
            manual_inactivity_checks: false,
        }
    }

    /// The sweep stops running `InactivityCheck`; the harness runs it with
    /// [`NetCore::run_inactivity_checks`] on its own tick, as O1c's sessions drive Core's (D35).
    #[cfg(feature = "test-utils")]
    pub fn with_manual_inactivity_checks(mut self) -> Self {
        self.manual_inactivity_checks = true;
        self
    }

    /// Serves every random byte the core draws from `rand`, in Core's order (D38): the oracle's
    /// stream ([`StreamRand::from_seed`]) for exact sessions.
    #[cfg(feature = "test-utils")]
    pub fn with_rand_source(mut self, rand: Arc<dyn RandSource>) -> Self {
        self.rand = Some(rand);
        self
    }

    /// The source the core draws from: a harness's, the zero-key stream when
    /// `deterministic` (`test-utils`), else the operating system's.
    fn rand_source(&self) -> Arc<dyn RandSource> {
        match &self.rand {
            Some(rand) => Arc::clone(rand),
            None if self.is_deterministic() => Arc::new(StreamRand::with_key([0; 32])),
            None => Arc::new(OsRand),
        }
    }

    /// `deterministic`; always false without `test-utils`.
    fn is_deterministic(&self) -> bool {
        #[cfg(any(test, feature = "test-utils"))]
        return self.deterministic;
        #[cfg(not(any(test, feature = "test-utils")))]
        false
    }
}

/// What the network core tells its processor. Every method must return quickly and never wait on
/// the processor (P2a §2.2.1, invariant (i)). A method that panics on the core's sweep or I/O
/// tasks aborts the process, as an exception out of Core's socket thread terminates it (D44);
/// on the host's own task (`attach`, `shutdown`) the panic reaches the host.
pub trait NetEvents: Send + Sync {
    /// A peer has messages to process (Core's `WakeMessageHandler`).
    fn wake(&self);

    /// Core's `InitializeNode(node, our_services)`, called synchronously on the dialling or
    /// accepting task before the peer enters the connection table; for outbound peers it pushes
    /// our `version` through `core` (P2a §2.2, core M-3). `our_services` adds `NODE_BLOOM` for
    /// inbound peers with the bloom-filter permission, as Core's `AcceptConnection` does.
    fn initialize(&self, core: &NetCore, peer: &PeerHandle, our_services: ServiceFlags);

    /// Core's `FinalizeNode`, called once after the peer has left the connection table.
    fn finalize(&self, core: &NetCore, peer: &PeerHandle);
}

/// Where a connection goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// An address from AddrMan, an anchor or a seed.
    Addr(Address),
    /// A destination string (`-connect`, `-addnode`, `-seednode`, a DNS seed name): Core's
    /// `pszDest`, resolved at connect time.
    Dest(String),
}

/// What to connect to (Core's `OpenNetworkConnection` arguments).
#[derive(Debug)]
pub struct OpenRequest {
    /// The address or name.
    pub target: Target,
    /// The connection type.
    pub conn_type: ConnectionType,
    /// Count a failed connect against the address in AddrMan (`fCountFailure`).
    pub count_failure: bool,
    /// The slot the connection holds (`grantOutbound`), released when it closes.
    pub grant: Option<Grant>,
    /// Use the V2 transport.
    pub use_v2: bool,
    /// Core's `masternode_connection` flag (P7).
    pub masternode: bool,
    /// Core's `masternode_probe_connection` flag (P7).
    pub masternode_probe: bool,
}

impl OpenRequest {
    /// Core's `OpenNetworkConnection` takes the name as a C string (`pszDest`), and every caller
    /// with a name passes `c_str()`, so a name is cut at its first NUL: `addnode … onetry`,
    /// `-connect`, `-seednode`, the added-node thread, `AddConnection` and the reconnections
    /// connect to the name before it, while `AddNode` and `RemoveAddedNode` keep and compare the
    /// whole string.
    // Core: rpc/net.cpp:358; net.cpp:2063, 2823, 2933, 3385, 5001
    pub fn cut_dest_at_nul(&mut self) {
        if let Target::Dest(dest) = &mut self.target
            && let Some(nul) = dest.find('\0')
        {
            dest.truncate(nul);
        }
    }
}

/// A connected socket to add to the table (the common tail of Core's `ConnectNode` and
/// `CreateNodeFromAcceptedSocket`). `sim` and the listener use it directly.
pub struct Attach {
    /// The socket.
    pub sock: Arc<dyn NetSocket>,
    /// The remote address.
    pub addr: Address,
    /// Our end (`addrBind`).
    pub addr_bind: Service,
    /// The destination string for an outbound connection by name.
    pub dest: Option<String>,
    /// The connection type.
    pub conn_type: ConnectionType,
    /// `m_inbound_onion`.
    pub inbound_onion: bool,
    /// `m_permission_flags`.
    pub permissions: Permissions,
    /// Use the V2 transport (as responder for inbound, which also accepts V1).
    pub use_v2: bool,
    /// The slot the connection holds.
    pub grant: Option<Grant>,
    /// Core's `masternode_connection` flag.
    pub masternode: bool,
    /// Core's `masternode_probe_connection` flag.
    pub masternode_probe: bool,
}

/// A connected socket and our end of it (Core's `GetBindAddress`).
struct Connected {
    sock: Arc<dyn NetSocket>,
    addr_bind: Service,
}

impl Connected {
    /// A TCP connection, with Core's socket options ([`TcpSocket::new`]).
    fn tcp(stream: tokio::net::TcpStream) -> Option<Self> {
        let addr_bind = stream.local_addr().map(Service::from_socket_addr).unwrap_or_default();
        Some(Self {
            sock: Arc::new(TcpSocket::new(stream).ok()?),
            addr_bind,
        })
    }
}

/// Why [`NetCore::open`] did not add a connection. Each is a silent return in Core's
/// `OpenNetworkConnection` or a `ConnectNode` failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpenError {
    /// Shutting down, or the network is inactive.
    Interrupted,
    /// The address is banned or discouraged.
    Banned,
    /// Already connected to that address or name.
    AlreadyConnected,
    /// Our own listen port on a local address.
    Local,
    /// The name did not resolve, or resolved to an invalid address.
    Resolve(String),
    /// The TCP connect failed or timed out.
    Connect(String),
}

/// Why [`NetCore::new`] failed (Core's `LoadAddrman` init errors).
#[derive(Debug)]
#[non_exhaustive]
pub enum InitError {
    /// `peers.dat` is corrupt: Core's `"Invalid or corrupt peers.dat (%s). If you believe this
    /// is a bug, please report it to %s. As a workaround, you can move the file (%s) out of the
    /// way (rename, move, or delete) to have a new one created on the next start."`.
    CorruptPeers {
        /// The load error (`e.what()`).
        error: String,
        /// The store's location, quoted as Core's `fs::quoted`.
        quoted_path: String,
    },
    /// An unsupported `peers.dat` could not be moved aside: Core's "Failed to rename invalid
    /// peers.dat file. Please move or delete it and try again.".
    RenamePeers(std::io::Error),
}

/// Core's `PACKAGE_BUGREPORT` (`configure.ac` `AC_INIT`).
const PACKAGE_BUGREPORT: &str = "https://github.com/dashpay/dash/issues";

impl core::fmt::Display for InitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::CorruptPeers {
                error,
                quoted_path,
            } => write!(
                f,
                "Invalid or corrupt peers.dat ({error}). If you believe this is a bug, please report it to {PACKAGE_BUGREPORT}. As a workaround, you can move the file ({quoted_path}) out of the way (rename, move, or delete) to have a new one created on the next start."
            ),
            Self::RenamePeers(_) => write!(
                f,
                "Failed to rename invalid peers.dat file. Please move or delete it and try again."
            ),
        }
    }
}

impl std::error::Error for InitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RenamePeers(e) => Some(e),
            Self::CorruptPeers {
                ..
            } => None,
        }
    }
}

/// Core's `g_reachable_nets` after init's `-onlynet` and overlay rules: every network, or only
/// the `-onlynet` ones (`RemoveAll` drops the unroutable and internal ones too), and an overlay
/// only when the host routes it (D27).
// Core: init.cpp:1870-1886 (-onlynet, CJDNS), :1972 (Onion), :2774 (I2P)
fn initial_reachable(only_net: &[Network], overlays: &[Network]) -> [bool; Network::COUNT] {
    Network::ALL.map(|net| {
        let overlay = matches!(net, Network::Onion | Network::I2p | Network::Cjdns);
        (only_net.is_empty() || only_net.contains(&net)) && (!overlay || overlays.contains(&net))
    })
}

/// The network core: one per process and network (SB0 §3.1).
pub struct NetCore {
    cfg: NetConfig,
    rt: Handle,
    store: Arc<dyn NetStore>,
    pub(crate) resolver: Arc<dyn Resolver>,
    clock: Arc<dyn Clock>,
    events: Arc<dyn NetEvents>,
    me: Weak<NetCore>,

    nodes: RwLock<PeerSnapshot>,
    next_id: AtomicI64,
    network_active: AtomicBool,
    interrupted: AtomicBool,
    /// Core's `fAddressesInitialized`: set by `start_addresses`, cleared by the first `shutdown`.
    addresses_initialized: AtomicBool,
    ibd_complete: AtomicBool,
    seeds: (u64, u64),
    netgroup: NetGroupManager,
    addrman: AddrMan,
    banlist: BanList,
    /// Core's `dump_mutex` in `DumpBanlist`: one ban-list save at a time, so an older snapshot
    /// never lands after a newer one.
    dump_mutex: Mutex<()>,
    /// Core's `GetRandBytes` for every draw here (D38).
    rand: Arc<dyn RandSource>,
    network_counts: Mutex<[usize; Network::COUNT]>,
    /// Core's `m_reconnections`: V1 retries, opened by the `addcon` loop.
    pub(crate) reconnections: Mutex<VecDeque<OpenRequest>>,
    pub(crate) outbound_slots: Arc<Semaphore>,
    pub(crate) addnode_slots: Arc<Semaphore>,
    /// Wakes the dial drivers' waits on interrupt (Core's `interruptNet`), and only then.
    pub(crate) interrupt_wake: Notify,
    /// The dial loops' state (P2a §3.8).
    pub(crate) dial: Mutex<DialState>,
    pub(crate) sweep_wake: Arc<Notify>,
    /// One admission at a time across listening sockets (see [`NetCore::accept`]).
    #[cfg(feature = "inbound")]
    pub(crate) admission: Mutex<()>,
    /// Runs in [`NetCore::accept`] between the count and the attach, so a test can hold two
    /// accepts there.
    #[cfg(all(test, feature = "inbound"))]
    pub(crate) admission_hook: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// The sweep task, joined by `shutdown` (Core joins `threadSocketHandler`).
    sweep_task: Mutex<Option<JoinHandle<()>>>,
    /// Held across [`NetCore::shutdown`], so a second call waits for the first. A cancelled call
    /// leaves the tasks it was joining in place for the next (`Taken`).
    shutdown_lock: tokio::sync::Mutex<()>,
    /// Every peer's reader and writer, spawned under the table's write lock so that `shutdown`,
    /// which takes the table first, stops each one.
    io_tasks: Mutex<JoinSet<()>>,
    /// `attach` calls in progress, and their end: `shutdown` waits for a rejected attach's
    /// `finalize`.
    attaching: AtomicUsize,
    attach_done: Notify,
    pub(crate) local: Mutex<LocalHost>,
    pub(crate) timedata: Mutex<TimeData>,
    pub(crate) clock_warning: AtomicBool,
    /// Core's `g_reachable_nets`, by network index.
    reachable: RwLock<[bool; Network::COUNT]>,
    /// Core's `proxyInfo` and `nameProxy`.
    proxies: RwLock<ProxyTable>,
    /// The counter of `ConnectThroughProxy`'s random credentials (a process-wide
    /// `static std::atomic_int` in Core; one core per process here).
    socks5_credentials: AtomicI32,
}

impl NetCore {
    /// The core for one network: loads AddrMan and the ban list from `store` and starts the
    /// sweep task on `rt`. Per-peer tasks run on `rt` too. `anchors.dat` is not read yet and
    /// `shutdown` dumps nothing until [`NetCore::start_addresses`].
    ///
    /// # Errors
    ///
    /// A corrupt `peers.dat`, or one of an unsupported version that `store` cannot back up.
    /// A store that cannot load an image is Core's unopenable file: a fresh AddrMan or ban list.
    // Core: addrdb.cpp:192-236 LoadAddrman
    pub fn new(
        cfg: NetConfig,
        rt: Handle,
        store: Arc<dyn NetStore>,
        resolver: Arc<dyn Resolver>,
        clock: Arc<dyn Clock>,
        events: Arc<dyn NetEvents>,
    ) -> Result<Arc<Self>, InitError> {
        // Draws in init.cpp's order (D38): AddrMan and its resets, BanMan, then CConnman's seeds.
        let rand = cfg.rand_source();
        let netgroup = NetGroupManager::new(cfg.asmap.clone(), cfg.allow_private_net);
        let addrman = AddrMan::with_rand_source(
            netgroup.clone(),
            cfg.is_deterministic(),
            cfg.check_addrman,
            cfg.allow_private_net,
            cfg.params.message_start,
            rand.clone(),
        );
        // Core: addrdb.cpp:194-231 LoadAddrman (every reset writes a fresh peers.dat at once)
        let start = Instant::now();
        // `fs::quoted` (`util/fs.h`): `std::quoted(s, '"', '&')`.
        let quoted_path = format!(
            "\"{}\"",
            store.location(NetBlob::Peers).replace('&', "&&").replace('"', "&\"")
        );
        // Core: addrdb.cpp:125-128 — an unopenable file is `DbNotFoundError`, as a missing one.
        match store.load(NetBlob::Peers).unwrap_or(None) {
            None => {
                // Core constructs a new AddrMan here too (a new key).
                addrman.clear();
                tracing::info!(target: "net", "Creating peers.dat because the file was not found ({quoted_path})");
                save_peers(&*store, &addrman);
            }
            // Core reads into the AddrMan it has just built, keeping its context (no draw).
            Some(bytes) => match addrman.unserialize(&bytes) {
                Ok(()) => {
                    // `Size()` runs the consistency check twice, which draws from the RNG.
                    let n = addrman.size(None, None);
                    let ms = start.elapsed().as_millis();
                    tracing::info!(target: "net", "Loaded {n} addresses from peers.dat  {ms}ms");
                }
                Err(e @ LoadError::Inconsistent(_)) => {
                    addrman.clear();
                    tracing::info!(target: "net", "Creating peers.dat because of invalid or corrupt file ({e})");
                    save_peers(&*store, &addrman);
                }
                Err(e) if e.is_unsupported_version() => {
                    // Core: addrdb.cpp:219-220 (init stops)
                    store.backup(NetBlob::Peers).map_err(InitError::RenamePeers)?;
                    addrman.clear();
                    tracing::info!(target: "net", "Creating new peers.dat because the file version was not compatible ({quoted_path}). Original backed up to peers.dat.bak");
                    save_peers(&*store, &addrman);
                }
                Err(e) => {
                    return Err(InitError::CorruptPeers {
                        error: e.to_string(),
                        quoted_path,
                    });
                }
            },
        }
        // Core: banman.h:94 `m_discouraged`, whose `reset()` draws `GetRand<unsigned int>()`
        // (common/bloom.cpp:385-387); banman.cpp BanMan::BanMan (LoadBanlist; a missing file is
        // recreated)
        let banlist = BanList::new(get_rand(&rand, u64::from(u32::MAX)) as u32, cfg.ban_time);
        // Core: addrdb.cpp:166-171 — an unreadable file fails `CBanDB::Read`, so it is recreated.
        let bans = store.load(NetBlob::Bans).unwrap_or_else(|e| {
            let path = store.location(NetBlob::Bans);
            tracing::info!(target: "net", "Cannot load banlist {path}: {e}");
            None
        });
        banlist.load(bans.as_deref().unwrap_or_default(), clock.node_now().0);
        // Core: init.cpp:1715 `make_unique<CConnman>(GetRand<uint64_t>(), GetRand<uint64_t>(), …)`.
        // GCC evaluates the arguments right to left, so nSeed1 is drawn first (D38).
        let seed1 = get_rand(&rand, u64::MAX);
        let seeds = (get_rand(&rand, u64::MAX), seed1);
        let outbound =
            (cfg.max_outbound_full_relay + cfg.max_outbound_block_relay + cfg.max_feeler)
                .min(cfg.max_connections);
        let addnode = cfg.max_addnode;
        let reachable = initial_reachable(&cfg.only_net, &cfg.reachable_overlays);
        let proxies = cfg.proxies.clone();
        let core = Arc::new_cyclic(|me| Self {
            cfg,
            rt,
            store,
            resolver,
            clock,
            events,
            me: me.clone(),
            nodes: RwLock::new(Arc::from(Vec::new())),
            next_id: AtomicI64::new(0),
            network_active: AtomicBool::new(true),
            interrupted: AtomicBool::new(false),
            addresses_initialized: AtomicBool::new(false),
            ibd_complete: AtomicBool::new(false),
            seeds,
            netgroup,
            addrman,
            banlist,
            dump_mutex: Mutex::new(()),
            rand,
            network_counts: Mutex::new([0; Network::COUNT]),
            reconnections: Mutex::new(VecDeque::new()),
            outbound_slots: Arc::new(Semaphore::new(outbound)),
            addnode_slots: Arc::new(Semaphore::new(addnode)),
            interrupt_wake: Notify::new(),
            dial: Mutex::new(DialState::default()),
            sweep_wake: Arc::new(Notify::new()),
            #[cfg(feature = "inbound")]
            admission: Mutex::new(()),
            #[cfg(all(test, feature = "inbound"))]
            admission_hook: std::sync::OnceLock::new(),
            local: Mutex::new(LocalHost::default()),
            timedata: Mutex::new(TimeData::default()),
            clock_warning: AtomicBool::new(false),
            sweep_task: Mutex::new(None),
            shutdown_lock: tokio::sync::Mutex::new(()),
            io_tasks: Mutex::new(JoinSet::new()),
            attaching: AtomicUsize::new(0),
            attach_done: Notify::new(),
            reachable: RwLock::new(reachable),
            proxies: RwLock::new(proxies),
            socks5_credentials: AtomicI32::new(0),
        });
        // Core: init.cpp AppInitMain (-externalip)
        for addr in core.cfg.external_ip.clone() {
            core.add_local(addr, LocalScore::Manual);
        }
        // Core: net.cpp CConnman::Start (-seednode as ADDR_FETCH, -addnode)
        {
            let v2 = core.cfg.local_services.0 & ServiceFlags::NODE_P2P_V2.0 != 0;
            let mut dial = core.dial.lock();
            dial.addr_fetches.extend(core.cfg.seed_nodes.iter().cloned());
            dial.added.extend(core.cfg.added_nodes.iter().map(|n| AddedNode {
                node: n.clone(),
                use_v2: v2,
            }));
        }
        core.dump_bans();
        *core.sweep_task.lock() = Some(core.rt.spawn(abort_on_panic(sweep(Arc::downgrade(&core)))));
        Ok(core)
    }

    /// `CConnman::Start` once the binds succeeded: reads (and removes) `anchors.dat` unless
    /// under `-connect`, then sets `fAddressesInitialized`, so `shutdown` dumps `peers.dat`, the
    /// ban list and the anchors. A host that fails to bind does not call it, and the files stay
    /// as they were. A second call does nothing.
    // Core: net.cpp:4025-4038 CConnman::Start
    pub fn start_addresses(&self) {
        let mut dial = self.dial.lock();
        if self.addresses_initialized.load(Ordering::SeqCst) {
            return;
        }
        if self.use_addrman_outgoing() {
            let anchors = self.read_anchors();
            tracing::info!(target: "net", "{} block-relay-only anchors will be tried for connections.", anchors.len());
            dial.anchors = anchors;
        }
        self.addresses_initialized.store(true, Ordering::SeqCst);
    }

    /// The options the core was built with.
    pub fn config(&self) -> &NetConfig {
        &self.cfg
    }

    /// The node clock.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// The connection table in insertion order; rebuilt on change, so this is a cheap copy.
    pub fn peers(&self) -> PeerSnapshot {
        self.nodes.read().clone()
    }

    /// The peer with id `id`, if connected.
    pub fn peer(&self, id: PeerId) -> Option<PeerHandle> {
        self.nodes.read().iter().find(|p| p.id() == id).cloned()
    }

    /// Core's `CNode::PollMessage`: one message from the peer's quorum-priority or normal
    /// receive queue (100 quorum messages per normal one while both hold messages), updating
    /// `fPauseRecv`. Never blocks.
    pub fn pop(&self, peer: &PeerHandle) -> Option<Polled> {
        let polled = peer.poll_message(self.cfg.recv_flood_size)?;
        // Core: net_processing.cpp:5921-5923, as the message is taken for processing.
        self.capture(peer, polled.msg.cmd.as_str(), &polled.msg.payload, true);
        Some(polled)
    }

    /// `-capturemessages` (P4a D10), stamped with the core's mockable clock.
    fn capture(&self, peer: &PeerHandle, cmd: &str, payload: &[u8], incoming: bool) {
        if let Some(capture) = &self.cfg.capture {
            capture.capture(self.clock.node_now_micros(), peer.addr(), cmd, payload, incoming);
        }
    }

    /// Core's `CConnman::PushMessage`: charges the message's retained memory to the peer's send
    /// size, sets `fPauseSend` when the size plus the transport's charge exceeds
    /// `-maxsendbuffer`, and **always** appends the message. Never blocks, never rejects. Returns
    /// `fPauseSend` after the push. Safe from any thread (overlay pushes).
    pub fn push(&self, peer: &PeerHandle, msg: OutMsg) -> PauseState {
        // Core: net.cpp:4862-4864
        self.capture(peer, msg.command.as_str(), &msg.payload, false);
        let (pause, wake) = peer.push_msg(msg, self.cfg.send_buffer_max);
        if wake {
            peer.writer_wake.notify_one();
        }
        pause
    }

    /// Core's `DisconnectNode(NodeId)`: sets `fDisconnect` on the connected peer `peer`; the
    /// sweep lingers and removes it on its next pass (P2a §3.8.7). A peer already removed is not
    /// found, as Core's `ForNode` finds no node, and marking it would change nothing.
    // Core: net.cpp:4603-4610 CConnman::DisconnectNode(NodeId)
    pub fn disconnect(&self, peer: PeerId, why: Why) {
        if let Some(p) = self.peer(peer)
            && p.mark_disconnect()
        {
            tracing::debug!(target: "net", "disconnecting peer={peer}: {why:?}");
        }
    }

    /// Core's `DisconnectNode(const std::string&)` (the `disconnectnode` RPC by address): marks
    /// the first peer in the table, not already marked, whose `m_addr_name` is `name`. False
    /// when there is none. `-logips` is not an option of the core: the line never names it.
    // Core: net.cpp:4578-4585 CConnman::DisconnectNode; net.h:1692-1695, 1712-1731
    pub fn disconnect_by_name(&self, name: &str) -> bool {
        self.mark_first(|p| p.addr_name() == name, "disconnect by address matched")
    }

    /// Core's `DisconnectNode(NodeId)` (the `disconnectnode` RPC by id): as
    /// [`NetCore::disconnect_by_name`], matching the id. Unlike [`NetCore::disconnect`], the
    /// processor's, it reports whether a peer not already marked was found.
    // Core: net.cpp:4606-4613 CConnman::DisconnectNode
    pub fn disconnect_by_id(&self, id: PeerId) -> bool {
        self.mark_first(|p| p.id() == id, "disconnect by id")
    }

    /// `WithNodeMutable`: `FindNodeMutable` skips the peers already marked.
    fn mark_first(&self, matches: impl Fn(&PeerHandle) -> bool, log: &str) -> bool {
        let peers = self.nodes.read();
        let Some(p) = peers.iter().find(|p| !p.disconnect_requested() && matches(p)) else {
            return false;
        };
        p.mark_disconnect();
        tracing::debug!(target: NETCONN, "{log} peer={}; disconnecting", p.id());
        true
    }

    /// Core's `MaybeDiscourageAndDisconnect` once misbehaviour reached the threshold: `noban`
    /// and manual peers are kept; a local address is only disconnected; otherwise the address
    /// is discouraged and **every** peer on it is disconnected (`DisconnectNode(addr)`).
    /// Returns whether the peer was disconnected.
    ///
    /// It takes the handle the processor holds, not an id: Core's message handler keeps a
    /// reference on the node through `ProcessMessages` and `SendMessages`, so a peer that the
    /// socket thread removed meanwhile (it closed after misbehaving) is still discouraged.
    // Core: net_processing.cpp:5833-5870 PeerManagerImpl::MaybeDiscourageAndDisconnect;
    // net.cpp:3696-3712 ThreadMessageHandler
    pub fn discourage(&self, p: &PeerHandle) -> bool {
        let peer = p.id();
        if p.has_permission(Permissions::NO_BAN) {
            tracing::info!(target: "net", "Warning: not punishing noban peer {peer}!");
            return false;
        }
        if p.conn_type() == ConnectionType::Manual {
            tracing::info!(target: "net", "Warning: not punishing manually connected peer {peer}!");
            return false;
        }
        if p.addr().service.addr.is_local() {
            let kind = if p.inbound_onion() {
                "inbound onion"
            } else {
                "local"
            };
            tracing::debug!(target: "net", "Warning: disconnecting but not discouraging {kind} peer {peer}!");
            p.mark_disconnect();
            return true;
        }
        tracing::debug!(target: "net", "Disconnecting and discouraging peer {peer}!");
        self.banlist.discourage(&p.addr().service.addr);
        self.disconnect_subnet(&SubNet::single(p.addr().service.addr.clone()));
        true
    }

    /// Core's `DisconnectNode(const CSubNet&)`: marks every peer whose address the subnet
    /// matches. Returns whether any was.
    // Core: net.cpp:4587-4599 CConnman::DisconnectNode
    pub fn disconnect_subnet(&self, net: &SubNet) -> bool {
        let mut any = false;
        for p in self.peers().iter().filter(|p| net.matches(&p.addr().service.addr)) {
            tracing::debug!(target: "net", "disconnect by subnet matched peer={}; disconnecting", p.id());
            p.mark_disconnect();
            any = true;
        }
        any
    }

    /// Core's `setban add`: `BanMan::Ban(net, offset, since_unix_epoch)` (an offset ≤ 0 is
    /// `-bantime` from now), the ban list saved at once, then every peer in `net` disconnected.
    pub fn ban(&self, net: SubNet, offset: i64, since_unix_epoch: bool) {
        if self.banlist.ban(net.clone(), offset, since_unix_epoch, self.clock.node_now().0) {
            self.dump_bans();
        }
        self.disconnect_subnet(&net);
    }

    /// Core's `setban remove`: unbans `net` and saves the ban list. Returns whether it was banned.
    pub fn unban(&self, net: &SubNet) -> bool {
        let removed = self.banlist.unban(net);
        if removed {
            self.dump_bans();
        }
        removed
    }

    // Core: banman.cpp:47-69 BanMan::DumpBanlist
    fn dump_bans(&self) {
        let _dump = self.dump_mutex.lock();
        let Some(entries) = self.banlist.take_dump(self.clock.node_now().0) else {
            return;
        };
        let start = Instant::now();
        if let Err(e) = self.store.save(NetBlob::Bans, &banlist_json(&entries)) {
            tracing::warn!(target: "net", "Failed to dump banlist: {e}");
            self.banlist.mark_dirty();
        }
        tracing::debug!(
            target: "net",
            "Flushed {} banned node addresses/subnets to disk  {}ms",
            entries.len(),
            start.elapsed().as_millis()
        );
    }

    /// Core's `ClearBanned` (`setban clear`): empties the ban list and saves it.
    pub fn clear_banned(&self) {
        self.banlist.clear();
        self.dump_bans();
    }

    /// The address manager.
    pub fn addrman(&self) -> &AddrMan {
        &self.addrman
    }

    /// Bans and discouragement. Changes made directly are not saved until the next dump; use
    /// [`NetCore::ban`] and [`NetCore::unban`] for Core's immediate save.
    pub fn banlist(&self) -> &BanList {
        &self.banlist
    }

    /// The net-group manager (`GetGroup`, `GetMappedAS`).
    pub fn netgroup(&self) -> &NetGroupManager {
        &self.netgroup
    }

    /// Sets Core's IBD service cache (`g_initial_block_download_completed`, set from
    /// `UpdatedBlockTip`), read by [`NetCore::desirable_services`].
    pub fn set_ibd_complete(&self, done: bool) {
        self.ibd_complete.store(done, Ordering::SeqCst);
    }

    /// Core's `GetDesirableServiceFlags(services)`: `NODE_NETWORK_LIMITED` after IBD for a peer
    /// that offers it without `NODE_NETWORK`, else `NODE_NETWORK`.
    // Core: protocol.cpp:252-258 GetDesirableServiceFlags
    pub fn desirable_services(&self, services: ServiceFlags) -> ServiceFlags {
        if services.0 & ServiceFlags::NODE_NETWORK_LIMITED.0 != 0
            && self.ibd_complete.load(Ordering::SeqCst)
        {
            ServiceFlags::NODE_NETWORK_LIMITED
        } else {
            ServiceFlags::NODE_NETWORK
        }
    }

    /// Core's `HasAllDesirableServiceFlags(services)`.
    pub fn has_all_desirable_services(&self, services: ServiceFlags) -> bool {
        let want = self.desirable_services(services);
        services.0 & want.0 == want.0
    }

    /// Core's `g_reachable_nets.Contains(net)`. It starts as [`NetConfig::only_net`] and
    /// [`NetConfig::reachable_overlays`] say, and [`NetCore::set_reachable`] changes it.
    pub fn is_reachable(&self, net: Network) -> bool {
        self.reachable.read()[net as usize]
    }

    /// Core's `g_reachable_nets.Add(net)` (`reachable`) or `Remove(net)`: init's `-onlynet`,
    /// `-cjdnsreachable`, onion-proxy and `-i2psam` rules, and Tor control's onion proxy.
    // Core: init.cpp:1870-1886, :1972, :2774; torcontrol.cpp (P8b)
    pub fn set_reachable(&self, net: Network, reachable: bool) {
        self.reachable.write()[net as usize] = reachable;
    }

    /// Core's `GetProxy(net)`: the proxy connections to `net` go through.
    pub fn proxy(&self, net: Network) -> Option<Proxy> {
        self.proxies.read().get(net).cloned()
    }

    /// Core's `GetNameProxy`: the proxy a name that does not resolve goes to.
    pub fn name_proxy(&self) -> Option<Proxy> {
        self.proxies.read().name_proxy().cloned()
    }

    /// Core's `SetProxy(net, proxy)` ([`ProxyTable::set`]): false for an invalid proxy.
    pub fn set_proxy(&self, net: Network, proxy: Proxy) -> bool {
        self.proxies.write().set(net, proxy)
    }

    /// Core's `SetNameProxy(proxy)` ([`ProxyTable::set_name_proxy`]): false for an invalid
    /// proxy.
    pub fn set_name_proxy(&self, proxy: Proxy) -> bool {
        self.proxies.write().set_name_proxy(proxy)
    }

    /// Core's `IsProxy(addr)` ([`ProxyTable::is_proxy`]).
    pub fn is_proxy(&self, addr: &NetAddr) -> bool {
        self.proxies.read().is_proxy(addr)
    }

    /// Core's `GetDeterministicRandomizer(id)`: SipHash keyed with this core's two seeds, with
    /// `id` written.
    pub fn deterministic_randomizer(&self, id: u64) -> SipHasher {
        SipHasher::new(self.seeds.0, self.seeds.1).write_u64(id)
    }

    /// Core's `CalculateKeyedNetGroup`.
    pub fn keyed_net_group(&self, addr: &NetAddr) -> u64 {
        self.deterministic_randomizer(RANDOMIZER_ID_NETGROUP)
            .write(&self.netgroup.group(addr))
            .finish()
    }

    /// The source of every random byte (Core's `GetRandBytes`): the OS, the zero-key stream when
    /// `deterministic` (`test-utils`), or the source a harness set (D38).
    pub fn rand(&self) -> &Arc<dyn RandSource> {
        &self.rand
    }

    /// Core's `GetRand(max)` from [`NetCore::rand`].
    pub fn get_rand(&self, max: u64) -> u64 {
        get_rand(&self.rand, max)
    }

    /// Core's `FastRandomContext()` on [`NetCore::rand`]: seeded at its first draw.
    pub fn new_rng(&self) -> FastRandom {
        FastRandom::lazy(Arc::clone(&self.rand))
    }

    /// `f` on a fresh [`NetCore::new_rng`] context, as Core's `FastRandomContext()` temporaries
    /// and `GetRand` calls draw. A caller whose Core context lives across draws keeps its own.
    pub fn with_rng<R>(&self, f: impl FnOnce(&mut FastRandom) -> R) -> R {
        f(&mut self.new_rng())
    }

    /// Core's `GenerateRandomGarbage`: a V2 transport's garbage from its own context.
    // Core: net.cpp:1062-1068 GenerateRandomGarbage
    fn generate_random_garbage(&self) -> Vec<u8> {
        let mut rng = self.new_rng();
        let mut garbage = vec![0u8; rng.randrange(MAX_GARBAGE_LEN as u64 + 1) as usize];
        rng.fill(&mut garbage);
        garbage
    }

    /// Takes a queued V1 retry (Core's `m_reconnections`), for the `addcon` loop. Empty when
    /// [`NetConfig::v1_retry`] is off.
    pub fn take_reconnection(&self) -> Option<OpenRequest> {
        self.reconnections.lock().pop_front()
    }

    /// Core's `OpenNetworkConnection` + `ConnectNode`: resolves a name, connects directly with
    /// `-timeout`, through the network's proxy or, for a name that did not resolve, through the
    /// name proxy, then [`NetCore::attach`]es the socket. A name is cut at its first NUL
    /// ([`OpenRequest::cut_dest_at_nul`]).
    // Core: net.cpp:3586-3665 CConnman::OpenNetworkConnection; net.cpp:370-520 ConnectNode
    pub async fn open(&self, mut req: OpenRequest) -> Result<PeerHandle, OpenError> {
        debug_assert_ne!(req.conn_type, ConnectionType::Inbound);
        req.cut_dest_at_nul();
        if self.interrupted() || !self.network_active() {
            return Err(OpenError::Interrupted);
        }
        let now = self.clock.node_now().0;
        let resolved = match &req.target {
            Target::Addr(addr) => {
                let ip = &addr.service.addr;
                if self.banlist.is_discouraged(ip) || self.banlist.is_banned(ip, now) {
                    return Err(OpenError::Banned);
                }
                if self.already_connected_to_address(addr) {
                    return Err(OpenError::AlreadyConnected);
                }
                if addr.service.port == self.cfg.listen_port && self.is_local(ip) {
                    return Err(OpenError::Local);
                }
                if self.exists_service(&addr.service) {
                    tracing::info!(target: "net", "Failed to open new connection, already connected");
                    return Err(OpenError::AlreadyConnected);
                }
                Some(addr.clone())
            }
            Target::Dest(dest) => {
                if self.peers().iter().any(|p| !p.disconnect_requested() && p.addr_name() == dest) {
                    return Err(OpenError::AlreadyConnected);
                }
                self.resolve_dest(dest).await?
            }
        };
        // Core: net.cpp:430-484
        let (addr, connected) = match resolved {
            Some(addr) if addr.service.addr.is_valid() => {
                let mut proxy_failed = false;
                let net = addr.service.addr.network(self.cfg.allow_private_net);
                let connected = match self.proxy(net) {
                    // I2P goes through its SAM session (P8c); rdashd sets no I2P proxy yet.
                    Some(_) if net == Network::I2p => None,
                    Some(proxy) => {
                        tracing::debug!(target: "proxy", "Using proxy: {proxy} to connect to {}:{}", addr.service.addr, addr.service.port);
                        let host = addr.service.addr.to_string();
                        self.connect_through_proxy(
                            &proxy,
                            &host,
                            addr.service.port,
                            &mut proxy_failed,
                        )
                        .await
                    }
                    None => {
                        let manual = req.conn_type == ConnectionType::Manual;
                        self.connect_directly(&addr.service, manual).await.and_then(Connected::tcp)
                    }
                };
                // An attempt counts unless the proxy itself failed.
                if !proxy_failed {
                    self.addrman.attempt(&addr.service, req.count_failure, self.clock.node_now().0);
                }
                (addr, connected)
            }
            unresolved => {
                let connected = match (&req.target, self.name_proxy()) {
                    (Target::Dest(dest), Some(proxy)) => {
                        let port = default_port_for(dest, self.cfg.params.default_port);
                        let (host, port) = split_host_port(dest, port);
                        self.connect_through_proxy(&proxy, host, port, &mut false).await
                    }
                    _ => None,
                };
                (unresolved.unwrap_or_default(), connected)
            }
        };
        let Some(connected) = connected else {
            let to = match &req.target {
                Target::Dest(dest) if !addr.service.addr.is_valid() => dest.clone(),
                _ => addr.service.to_string(),
            };
            return Err(OpenError::Connect(format!("no connection to {to}")));
        };
        let dest = match req.target {
            Target::Dest(d) => Some(d),
            Target::Addr(_) => None,
        };
        Ok(self.attach(Attach {
            sock: connected.sock,
            addr,
            addr_bind: connected.addr_bind,
            dest,
            conn_type: req.conn_type,
            inbound_onion: false,
            permissions: Permissions::NONE,
            use_v2: req.use_v2,
            grant: req.grant,
            masternode: req.masternode,
            masternode_probe: req.masternode_probe,
        }))
    }

    /// Core's `ConnectDirectly`: a TCP connection to `dest` within `-timeout`, logged as Core's
    /// `LogConnectFailure` logs it (always for a `manual` connection, under `-debug=net`
    /// otherwise). Tor and I2P have no socket address: Core's failed `CreateSock`.
    // Core: netbase.cpp:564-650 LogConnectFailure, ConnectToSocket, ConnectDirectly
    async fn connect_directly(
        &self,
        dest: &Service,
        manual: bool,
    ) -> Option<tokio::net::TcpStream> {
        let Some(sa) = dest.to_socket_addr() else {
            tracing::error!(target: "net", "Cannot create a socket for connecting to {dest}");
            return None;
        };
        match tokio::time::timeout(self.cfg.connect_timeout, tokio::net::TcpStream::connect(sa))
            .await
        {
            Ok(Ok(stream)) => Some(stream),
            Ok(Err(e)) => {
                let e = crate::sock::network_error_string(&e);
                if manual {
                    tracing::info!("connect() to {dest} failed after wait: {e}");
                } else {
                    tracing::debug!(target: "net", "connect() to {dest} failed after wait: {e}");
                }
                None
            }
            Err(_) => {
                tracing::debug!(target: "net", "connection attempt to {dest} timed out");
                None
            }
        }
    }

    /// Core's `ConnectThroughProxy`: connects to `proxy` (`Proxy::Connect`, a manual connection
    /// for the logs) and asks it for `dest:port`, with fresh credentials under
    /// `-proxyrandomize`. `proxy_failed` is set when the proxy itself cannot be reached.
    // Core: netbase.cpp:732-758 ConnectThroughProxy, :652-682 Proxy::Connect
    async fn connect_through_proxy(
        &self,
        proxy: &Proxy,
        dest: &str,
        port: u16,
        proxy_failed: &mut bool,
    ) -> Option<Connected> {
        let interrupted = || self.interrupted();
        // Core counts credentials only once the proxy is connected.
        let auth = || {
            proxy.randomize_credentials.then(|| {
                let n = self.socks5_credentials.fetch_add(1, Ordering::SeqCst).to_string();
                ProxyCredentials {
                    username: n.clone(),
                    password: n,
                }
            })
        };
        match &proxy.addr {
            ProxyAddr::Service(service) => {
                let Some(mut stream) = self.connect_directly(service, true).await else {
                    *proxy_failed = true;
                    return None;
                };
                let auth = auth();
                socks5(dest, port, auth.as_ref(), &mut stream, SOCKS5_RECV_TIMEOUT, &interrupted)
                    .await
                    .then(|| Connected::tcp(stream))
                    .flatten()
            }
            ProxyAddr::Unix(path) => {
                #[cfg(unix)]
                {
                    let Some(mut stream) = self.connect_unix(path).await else {
                        *proxy_failed = true;
                        return None;
                    };
                    let auth = auth();
                    let auth = auth.as_ref();
                    if !socks5(dest, port, auth, &mut stream, SOCKS5_RECV_TIMEOUT, &interrupted)
                        .await
                    {
                        return None;
                    }
                    Some(Connected {
                        sock: Arc::new(crate::sock::UnixSocket::new(stream).ok()?),
                        // Core: GetBindAddress of a UNIX socket is not an IP address.
                        addr_bind: Service::default(),
                    })
                }
                #[cfg(not(unix))]
                {
                    let _ = (path, auth);
                    *proxy_failed = true;
                    None
                }
            }
        }
    }

    /// `Proxy::Connect` for a UNIX socket proxy: Core's `ConnectToSocket` on the path after
    /// `unix:`, a manual connection for the logs.
    // Core: netbase.cpp:658-678
    #[cfg(unix)]
    async fn connect_unix(&self, path: &str) -> Option<tokio::net::UnixStream> {
        let path = path.strip_prefix(crate::proxy::ADDR_PREFIX_UNIX).unwrap_or(path);
        match tokio::time::timeout(self.cfg.connect_timeout, tokio::net::UnixStream::connect(path))
            .await
        {
            Ok(Ok(stream)) => Some(stream),
            Ok(Err(e)) => {
                tracing::info!(
                    "connect() to {path} failed: {}",
                    crate::sock::network_error_string(&e)
                );
                None
            }
            Err(_) => {
                tracing::debug!(target: "net", "connection attempt to {path} timed out");
                None
            }
        }
    }

    /// Core's name branch of `ConnectNode`: `Lookup(dest, GetDefaultPort(dest), fNameLookup &&
    /// !HaveNameProxy(), 256)` (Tor and I2P names never reach the resolver), shuffled, each
    /// flipped to CJDNS when it is one; Core keeps the **last** address after checking each for
    /// validity and an existing connection. `None` when nothing resolved: Core then connects
    /// through the name proxy, if there is one.
    // Core: net.cpp:401-424
    pub(crate) async fn resolve_dest(&self, dest: &str) -> Result<Option<Address>, OpenError> {
        let (host, port) =
            split_host_port(dest, default_port_for(dest, self.cfg.params.default_port));
        let allow_lookup = self.cfg.name_lookup && self.name_proxy().is_none();
        let mut resolved = lookup(&*self.resolver, host, 256, allow_lookup).await;
        if resolved.is_empty() {
            if self.name_proxy().is_some() {
                return Ok(None);
            }
            return Err(OpenError::Resolve(dest.to_owned()));
        }
        // Core: net.cpp:405-407 `Shuffle(…, FastRandomContext())`
        self.new_rng().shuffle(&mut resolved);
        let mut addr = Address::default();
        for r in resolved {
            addr = Address {
                service: self.maybe_flip_ipv6_to_cjdns(Service::new(r, port)),
                services: ServiceFlags::NONE,
                time: 0,
            };
            if !addr.service.addr.is_valid() {
                tracing::debug!(target: "net", "Resolver returned invalid address {} for {dest}", addr.service);
                return Err(OpenError::Resolve(dest.to_owned()));
            }
            if self.exists_service(&addr.service) {
                tracing::info!(target: "net", "Not opening a connection to {dest}, already connected to {}", addr.service);
                return Err(OpenError::AlreadyConnected);
            }
        }
        Ok(Some(addr))
    }

    /// Core's `Lookup(name, port_default, allow_lookup)`: `SplitHostPort`, then the first address
    /// `LookupIntern` gives (a Tor or I2P name, an IP literal, or the resolver's first answer
    /// when `allow_lookup`). For init's `-proxy`, `-onion` and `-externalip`.
    // Core: netbase.cpp:194-217
    pub async fn lookup_service(
        &self,
        name: &str,
        port_default: u16,
        allow_lookup: bool,
    ) -> Option<Service> {
        if name.is_empty() || name.contains('\0') {
            return None;
        }
        let (host, port) = split_host_port(name, port_default);
        let addr = lookup(&*self.resolver, host, 1, allow_lookup).await.into_iter().next()?;
        Some(Service::new(addr, port))
    }

    /// Core's `ExistsNode(CService)`: `FindNode` skips peers being disconnected.
    fn exists_service(&self, s: &Service) -> bool {
        self.peers().iter().any(|p| !p.disconnect_requested() && p.addr().service == *s)
    }

    /// Core's `AlreadyConnectedToAddress`: `FindNode(CService)`, an exact `ip:port` match
    /// among peers not being disconnected.
    // Core: net.cpp:342-346; net.h:1683-1710 FindNode
    pub(crate) fn already_connected_to_address(&self, addr: &Address) -> bool {
        self.exists_service(&addr.service)
    }

    /// Adds a connected socket to the table: Core's `CNode` construction, `InitializeNode`, the
    /// `m_nodes` insert and the per-network count, then the reader and writer tasks.
    // Core: net.cpp:490-516 ConnectNode; net.cpp:3631-3660 OpenNetworkConnection
    pub fn attach(&self, a: Attach) -> PeerHandle {
        self.attach_inner(a, false).unwrap_or_else(|p| p)
    }

    /// [`NetCore::attach`]; `Err` with the finalized peer when the core was interrupted (by
    /// `interrupt` or `shutdown`) before the peer was listed.
    pub(crate) fn attach_inner(
        &self,
        a: Attach,
        prefer_evict: bool,
    ) -> Result<PeerHandle, PeerHandle> {
        let _attaching = Attaching::enter(self);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let local_nonce = self
            .deterministic_randomizer(RANDOMIZER_ID_LOCALHOSTNONCE)
            .write_u64(id as u64)
            .finish();
        let ip = a.addr.service.addr.clone();
        let network = if a.inbound_onion {
            Network::Onion
        } else {
            ip.net_class(self.cfg.allow_private_net)
        };
        let inbound = a.conn_type == ConnectionType::Inbound;
        let magic = dash_p2p_transport::Magic::from_bytes(self.cfg.params.message_start);
        let transport = if a.use_v2 {
            // Core: net.cpp:4724-4731 MakeTransport, in the CNode constructor
            Transport::V2(Box::new(V2Transport::with_rand(
                magic,
                !inbound,
                |out, strong| self.rand.rand_bytes(out, strong),
                || self.generate_random_garbage(),
            )))
        } else {
            Transport::v1(magic)
        };
        let mut our_services = self.cfg.local_services;
        if inbound && a.permissions.has(Permissions::BLOOM_FILTER) {
            our_services |= ServiceFlags::NODE_BLOOM;
        }
        let peer = Arc::new(Peer::new(PeerInit {
            id,
            addr_name: a.dest.clone().unwrap_or_else(|| a.addr.service.to_string()),
            keyed_net_group: self.keyed_net_group(&ip),
            addr: a.addr,
            addr_bind: a.addr_bind,
            dest: a.dest,
            conn_type: a.conn_type,
            inbound_onion: a.inbound_onion,
            permissions: a.permissions,
            network,
            local_nonce,
            connected: self.clock.node_now().0,
            masternode: a.masternode,
            masternode_probe: a.masternode_probe,
            use_v2: a.use_v2,
            transport,
            sock: Some(a.sock.clone()),
            grant: a.grant,
            prefer_evict,
        }));
        self.events.initialize(self, &peer, our_services);
        {
            let mut nodes = self.nodes.write();
            // A connect that finished after `shutdown` took the table: undo, as Core's joined
            // connection threads never reach this point.
            if self.interrupted() {
                drop(nodes);
                peer.close_socket(false);
                peer.grant.lock().take();
                self.events.finalize(self, &peer);
                return Err(peer);
            }
            let mut v: Vec<PeerHandle> = nodes.iter().cloned().collect();
            v.push(peer.clone());
            *nodes = v.into();
            if peer.conn_type().is_manual_or_full_outbound() {
                self.network_counts.lock()[ip.network(self.cfg.allow_private_net) as usize] += 1;
            }
            let mut tasks = self.io_tasks.lock();
            while tasks.try_join_next().is_some() {}
            tasks.spawn_on(
                abort_on_panic(reader(self.me.clone(), peer.clone(), a.sock.clone())),
                &self.rt,
            );
            tasks.spawn_on(abort_on_panic(writer(self.me.clone(), peer.clone(), a.sock)), &self.rt);
        }
        peer.writer_wake.notify_one();
        // Core writes the wakeup pipe once the node is listed, and a pass follows (D43).
        // Core: net.cpp:2021-2022, 3663-3664
        self.sweep_wake.notify_one();
        Ok(peer)
    }

    /// The managed connection count for `net` (Core's `m_network_conn_counts`).
    pub fn network_count(&self, net: Network) -> usize {
        self.network_counts.lock()[net as usize]
    }

    /// `setnetworkactive`: when false, the sweep marks every connection and no new one opens.
    /// Core logs the call with `LogPrintf`: no `[net:info] ` prefix.
    // Core: net.cpp:3890-3892
    pub fn set_network_active(&self, active: bool) {
        tracing::info!("SetNetworkActive: {active}");
        self.network_active.store(active, Ordering::SeqCst);
    }

    /// Whether the network is active.
    pub fn network_active(&self) -> bool {
        self.network_active.load(Ordering::SeqCst)
    }

    /// Core's `interruptNet()`: stops the dial loops and new connections.
    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
        self.interrupt_wake.notify_waiters();
        self.sweep_wake.notify_one();
        // Core's socket thread stops at `interruptNet`: no peer reads or sends after it. A peer
        // attached later is refused (`attach_inner` checks under the table lock).
        // Core: net.cpp:2446-2533 SocketHandler; net.cpp:2541-2555 ThreadSocketHandler
        for p in self.peers().iter() {
            p.reader_wake.notify_one();
            p.writer_wake.notify_one();
        }
    }

    /// Whether [`NetCore::interrupt`] was called.
    pub fn interrupted(&self) -> bool {
        self.interrupted.load(Ordering::SeqCst)
    }

    /// Core's `DumpAddresses`: saves `peers.dat` (the host's scheduler calls it every 15
    /// minutes) and a dirty ban list.
    // Core: net.cpp DumpAddresses; banman.cpp DumpBanlist
    pub fn dump(&self) {
        save_peers(&*self.store, &self.addrman);
        self.dump_bans();
    }

    /// Core's `StopThreads` then `StopNodes`: waits for the sweep (Core's socket thread) to
    /// finish the pass it is in, dumps through the store, then closes every connection and
    /// finalizes each peer. It returns once every peer's reader and writer have stopped and every
    /// removal and in-flight `attach`, its `finalize` included, has finished, so `finalize`
    /// never runs on two tasks at once here. Calls run one at a time; a later one only waits.
    /// The host has already stopped its processor and its dial and accept loops (D31). The store
    /// is called on this task; a host whose store blocks calls this from a blocking context.
    // Core: net.cpp:4168-4185 CConnman::StopThreads; net.cpp:4187-4210 CConnman::StopNodes
    pub async fn shutdown(&self) {
        let _one = self.shutdown_lock.lock().await;
        self.interrupt();
        // Declared after `_one`, so a cancelled call puts the tasks back before the next call
        // can take the lock.
        let mut sweep = Taken::new(&self.sweep_task);
        if let Some(task) = sweep.get().as_mut() {
            let _ = task.await;
        }
        sweep.done();
        self.release_dial_grants();
        if self.addresses_initialized.swap(false, Ordering::SeqCst) {
            self.dump_and_anchors();
        }
        let nodes = core::mem::replace(&mut *self.nodes.write(), Arc::from(Vec::new()));
        for p in nodes.iter() {
            self.remove_peer(p);
        }
        let mut io = Taken::new(&self.io_tasks);
        io.get().shutdown().await;
        io.done();
        // `~BanMan` dumps whether or not the addresses were loaded, after connman stopped.
        // Core: banman.cpp:23-26 BanMan::~BanMan; init.cpp:348, 365-366 Shutdown
        self.dump_bans();
        loop {
            let done = self.attach_done.notified();
            if self.attaching.load(Ordering::SeqCst) == 0 {
                return;
            }
            done.await;
        }
    }

    /// `StopNodes`' first part: `DumpAddresses`, then the anchors.
    fn dump_and_anchors(&self) {
        save_peers(&*self.store, &self.addrman);
        if self.use_addrman_outgoing() {
            // Anchor connections are only dumped during clean shutdown.
            // Core: net.cpp:3295 GetCurrentBlockRelayOnlyConns; net.cpp:554 IsBlockRelayOnly
            let mut anchors: Vec<Address> = (self.peers().iter())
                .filter(|p| {
                    (self.cfg.blocks_only && !p.has_permission(Permissions::RELAY))
                        || p.conn_type() == ConnectionType::BlockRelay
                })
                .map(|p| p.addr().clone())
                .collect();
            anchors.truncate(MAX_BLOCK_RELAY_ONLY_ANCHORS);
            tracing::info!(target: "net", "Flush {} outbound block-relay-only peer addresses to anchors.dat", anchors.len());
            let bytes = write_anchors(&self.cfg.params.message_start, &anchors);
            if let Err(e) = self.store.save(NetBlob::Anchors, &bytes) {
                tracing::warn!(target: "net", "Failed to write anchors.dat: {e}");
            }
        }
    }

    /// Core's `m_use_addrman_outgoing`: no `-connect`.
    fn use_addrman_outgoing(&self) -> bool {
        self.cfg.connect.is_none()
    }

    /// Core's `ReadAnchors`: an unreadable image is no anchors, and the image is removed after
    /// reading. At most [`MAX_BLOCK_RELAY_ONLY_ANCHORS`] are kept.
    // Core: addrdb.cpp:239-250 ReadAnchors; net.cpp:4025-4031 CConnman::Start
    fn read_anchors(&self) -> Vec<Address> {
        let mut anchors = match self.store.load(NetBlob::Anchors) {
            Ok(Some(bytes)) => match read_anchors(&self.cfg.params.message_start, &bytes) {
                Ok(a) => {
                    tracing::info!(target: "net", "Loaded {} addresses from \"anchors.dat\"", a.len());
                    a
                }
                Err(_) => Vec::new(),
            },
            Ok(None) | Err(_) => Vec::new(),
        };
        if let Err(e) = self.store.remove(NetBlob::Anchors) {
            tracing::warn!(target: "net", "Failed to remove anchors.dat: {e}");
        }
        anchors.truncate(MAX_BLOCK_RELAY_ONLY_ANCHORS);
        anchors
    }

    /// The common tail of removing a peer that this caller took out of the table: Core's
    /// socket close, grant release, per-network count and `FinalizeNode`.
    fn remove_peer(&self, p: &PeerHandle) {
        p.close_socket(false);
        p.clear_recv();
        p.grant.lock().take();
        if p.conn_type().is_manual_or_full_outbound() {
            let net = p.addr().service.addr.network(self.cfg.allow_private_net);
            let mut counts = self.network_counts.lock();
            counts[net as usize] = counts[net as usize].saturating_sub(1);
        }
        self.events.finalize(self, p);
    }

    /// Core's `DisconnectNodes`: marks every peer when the network is inactive, lingers on peers
    /// we disconnected (shutting the write side once the queue is flushed), and removes them.
    /// Returns the earliest linger deadline of a lingering peer whose socket is readable and
    /// not receive-paused: Core's socket loop polls without a timeout while one exists, so it
    /// is removed at its deadline, not on a later 500 ms pass (D43).
    // Core: net.cpp:2067-2201 CConnman::DisconnectNodes; net.cpp:2376-2415 only_poll
    fn disconnect_nodes(&self) -> Option<Instant> {
        let nodes = self.peers();
        if !self.network_active() {
            for p in nodes.iter() {
                // Core: net.cpp:2103 (`NET_NETCONN`)
                if p.mark_disconnect() {
                    tracing::debug!(
                        target: NETCONN,
                        "Network not active, dropping peer={}",
                        p.id()
                    );
                }
            }
        }
        let steady = self.clock.steady();
        let mut candidates = Vec::new();
        let mut spin: Option<Instant> = None;
        for p in nodes.iter().filter(|p| p.disconnect_requested()) {
            if !p.other_side_disconnected() {
                let until = *p.linger_until.lock().get_or_insert(steady + LINGER);
                if steady < until {
                    if p.linger_readable.load(Ordering::SeqCst) && !p.pause_recv() {
                        spin = Some(spin.map_or(until, |s| s.min(until)));
                    }
                    if !p.socket_shutdown.load(Ordering::SeqCst) && p.send_queue_empty() {
                        if let Some(sock) = p.sock.lock().as_ref() {
                            let _ = sock.shutdown_write();
                        }
                        p.socket_shutdown.store(true, Ordering::SeqCst);
                        // Their close now hangs the socket up, which the lingering reader sees.
                        p.reader_wake.notify_one();
                    }
                    continue;
                }
            }
            candidates.push(p.clone());
        }
        if candidates.is_empty() {
            return spin;
        }
        // Take the peers out under the write lock, so a concurrent `shutdown` and this sweep
        // never both remove (and finalize) the same peer.
        let removed: Vec<PeerHandle> = {
            let mut table = self.nodes.write();
            let (gone, kept): (Vec<PeerHandle>, Vec<PeerHandle>) =
                table.iter().cloned().partition(|p| candidates.iter().any(|c| Arc::ptr_eq(p, c)));
            *table = kept.into();
            gone
        };
        for p in removed {
            // A `LogPrintf`, without a category (BL-327 D14); there is no `nRefCount` to print.
            tracing::info!(
                "ThreadSocketHandler -- removing node: peer={} fInbound={} m_masternode_connection={} m_masternode_iqr_connection={}",
                p.id(),
                u8::from(p.is_inbound()),
                u8::from(p.masternode_connection()),
                u8::from(p.masternode_iqr())
            );
            if self.cfg.v1_retry && p.transport.lock().should_reconnect_v1() {
                tracing::debug!(target: "net", "retrying with v1 transport protocol for peer={}", p.id());
                let grant = p.grant.lock().take();
                self.reconnections.lock().push_back(OpenRequest {
                    target: match p.dest() {
                        Some(d) => Target::Dest(d.to_owned()),
                        None => Target::Addr(p.addr().clone()),
                    },
                    conn_type: p.conn_type(),
                    count_failure: false,
                    grant,
                    use_v2: false,
                    masternode: p.masternode_connection(),
                    masternode_probe: p.masternode_probe(),
                });
            }
            self.remove_peer(&p);
        }
        spin
    }

    /// Core's `InactivityCheck` for every connected peer.
    // Core: net.cpp:2293-2333 ShouldRunInactivityChecks, InactivityCheck
    pub(crate) fn inactivity_checks(&self) {
        let now = self.clock.node_now().0;
        for p in self.peers().iter().filter(|p| !p.disconnect_requested()) {
            if let Some(why) = self.inactivity(p, now) {
                tracing::debug!(target: "net", "{why}");
                p.mark_disconnect();
            }
        }
    }

    /// Test hook: one `InactivityCheck` pass over every connected peer, now, on the caller (with
    /// [`NetConfig::with_manual_inactivity_checks`], the only one).
    #[cfg(feature = "test-utils")]
    pub fn run_inactivity_checks(&self) {
        self.inactivity_checks();
    }

    /// Why `InactivityCheck` drops `p` at `now`, if it does (Core's log line).
    pub(crate) fn inactivity(&self, p: &PeerHandle, now: i64) -> Option<String> {
        if p.connected() + self.cfg.peer_timeout >= now {
            return None;
        }
        let (last_send, last_recv) = (p.last_send(), p.last_recv());
        Some(if last_recv == 0 || last_send == 0 {
            format!(
                "socket no message in first {} seconds, {} {} peer={}",
                self.cfg.peer_timeout,
                u8::from(last_recv != 0),
                u8::from(last_send != 0),
                p.id()
            )
        } else if now > last_send + TIMEOUT_INTERVAL {
            format!("socket sending timeout: {}s peer={}", now - last_send, p.id())
        } else if now > last_recv + TIMEOUT_INTERVAL {
            format!("socket receive timeout: {}s peer={}", now - last_recv, p.id())
        } else if !p.successfully_connected() {
            if p.transport_info().kind == dash_p2p_transport::TransportKind::Detecting {
                format!("V2 handshake timeout peer={}", p.id())
            } else {
                format!("version handshake timeout peer={}", p.id())
            }
        } else {
            return None;
        })
    }
}

/// What [`NetCore::shutdown`] took from `slot` to join. Dropped before [`Taken::done`] (the
/// `shutdown` future was cancelled), it puts what is left back, so the next call joins it.
struct Taken<'a, T: Default> {
    slot: &'a Mutex<T>,
    taken: Option<T>,
}

impl<'a, T: Default> Taken<'a, T> {
    fn new(slot: &'a Mutex<T>) -> Self {
        let taken = Some(core::mem::take(&mut *slot.lock()));
        Taken {
            slot,
            taken,
        }
    }

    fn get(&mut self) -> &mut T {
        self.taken.as_mut().expect("taken until done")
    }

    /// Joined: nothing to put back.
    fn done(mut self) {
        self.taken = None;
    }
}

impl<T: Default> Drop for Taken<'_, T> {
    fn drop(&mut self) {
        if let Some(left) = self.taken.take() {
            *self.slot.lock() = left;
        }
    }
}

/// Counts an `attach` in progress for [`NetCore::shutdown`].
struct Attaching<'a>(&'a NetCore);

impl<'a> Attaching<'a> {
    fn enter(core: &'a NetCore) -> Self {
        core.attaching.fetch_add(1, Ordering::SeqCst);
        Attaching(core)
    }
}

impl Drop for Attaching<'_> {
    fn drop(&mut self) {
        if self.0.attaching.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.attach_done.notify_waiters();
        }
    }
}

/// Core's `TraceThread` around the socket thread: a panic on the sweep or on a peer's reader or
/// writer, a host callback's included, logs `"net" raised an exception` and aborts the process,
/// as Core's rethrow out of the thread terminates it. Tokio would otherwise catch the panic and
/// end only that task, leaving peers unswept and their grants held (D44).
// Core: util/thread.cpp:16-27 util::TraceThread; util/system.cpp:783-788 PrintExceptionContinue
fn abort_on_panic(
    task: impl Future<Output = ()> + Send + 'static,
) -> impl Future<Output = ()> + Send {
    let mut task = Box::pin(task);
    poll_fn(move |cx| match std::panic::catch_unwind(AssertUnwindSafe(|| task.as_mut().poll(cx))) {
        Ok(poll) => poll,
        Err(panic) => {
            let what = panic
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("unknown exception");
            tracing::error!(target: "net", "\"net\" raised an exception\n{what}");
            std::process::abort();
        }
    })
}

/// The sweep: Core's socket loop's `InactivityCheck` (at most once a second) then
/// `DisconnectNodes`, every pass. A pass runs every 500 ms (Core's select timeout) and after a
/// socket event: any peer's read or send (Core's wait returns on any socket's readiness and on
/// the wakeup pipe a push to an empty queue writes), an `accept` or `attach` (a listening
/// socket's readiness; the pipe once a node is listed), a peer's close or read error, a flushed
/// send queue on a marked peer, a lingering peer turning readable, an inbound eviction, or
/// `interrupt`. While a lingering peer
/// is readable and not receive-paused, the next pass is at its linger deadline (D43).
async fn sweep(core: Weak<NetCore>) {
    let Some(wake) = core.upgrade().map(|c| c.sweep_wake.clone()) else {
        return;
    };
    let mut last_inactivity = None;
    loop {
        let Some(c) = core.upgrade() else {
            return;
        };
        // `shutdown` removes what is left.
        // Core: net.cpp:2655 ThreadSocketHandler `while (!interruptNet)`
        if c.interrupted() {
            return;
        }
        let woken = wake.notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        let steady = c.clock.steady();
        if !c.cfg.manual_inactivity_checks
            && last_inactivity.is_none_or(|t| steady.duration_since(t) > Duration::from_secs(1))
        {
            c.inactivity_checks();
            last_inactivity = Some(steady);
        }
        let wait = c.disconnect_nodes().map_or(SWEEP_INTERVAL, |until| {
            SWEEP_INTERVAL.min(until.saturating_duration_since(c.clock.steady()))
        });
        drop(c);
        let _ = tokio::time::timeout(wait, woken).await;
    }
}

/// Socket errors Core does not treat as a disconnect: `WSAEWOULDBLOCK`, `WSAEMSGSIZE`,
/// `WSAEINTR` and `WSAEINPROGRESS`.
// Core: net.cpp:1756, 2620
pub(crate) fn is_transient(e: &std::io::Error) -> bool {
    if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) {
        return true;
    }
    #[cfg(unix)]
    if matches!(e.raw_os_error(), Some(libc::EMSGSIZE | libc::EINPROGRESS)) {
        return true;
    }
    false
}

/// The reader: Core's `SocketRecvData` on readiness, 64 KiB per read, stopped while
/// `fPauseRecv`, while a partial send left bytes queued (Core skips receiving in that
/// round, so a slow reader gets TCP back-pressure), or once the peer is marked.
// Core: net.cpp:2470-2490 SocketHandlerConnected; net.cpp:2588-2640 SocketRecvData
async fn reader(core: Weak<NetCore>, peer: PeerHandle, sock: Arc<dyn NetSocket>) {
    let mut buf = vec![0u8; 0x10000];
    loop {
        if core.upgrade().is_none_or(|c| c.interrupted()) {
            return;
        }
        if peer.disconnect_requested() {
            return linger_watch(&core, &peer, &*sock).await;
        }
        if peer.pause_recv() || peer.send_backlogged() {
            peer.reader_wake.notified().await;
            continue;
        }
        tokio::select! {
            // An error is reported by the read below.
            _ = poll_fn(|cx| sock.poll_read_ready(cx)) => {}
            () = peer.reader_wake.notified() => continue,
        }
        if peer.disconnect_requested() {
            return linger_watch(&core, &peer, &*sock).await;
        }
        let Some(core) = core.upgrade().filter(|c| !c.interrupted()) else {
            return;
        };
        match sock.try_read(&mut buf) {
            Ok(0) => {
                if !peer.disconnect_requested() {
                    tracing::debug!(target: "net", "socket closed for peer={}", peer.id());
                }
                peer.close_socket(true);
                core.sweep_wake.notify_one();
                return;
            }
            Ok(n) => {
                let now_us = core.clock.node_now_micros();
                match peer.receive_bytes(
                    &buf[..n],
                    now_us,
                    core.cfg.recv_flood_size,
                    core.cfg.priority_queues,
                ) {
                    Ok(queued) => {
                        if queued {
                            core.events.wake();
                        }
                        // A V2 handshake step may have produced bytes to send.
                        peer.writer_wake.notify_one();
                        // Receive readiness ends Core's wait, and a pass follows (D43).
                        core.sweep_wake.notify_one();
                    }
                    Err(e) => {
                        tracing::debug!(target: "net", "{e} peer={}", peer.id());
                        peer.close_socket(false);
                        core.sweep_wake.notify_one();
                        return;
                    }
                }
            }
            Err(e) if is_transient(&e) => {}
            Err(e) => {
                if !peer.disconnect_requested() {
                    tracing::debug!(target: "net", "socket recv error for peer={}: {e}", peer.id());
                }
                peer.close_socket(true);
                core.sweep_wake.notify_one();
                return;
            }
        }
    }
}

/// A marked peer's reader for the rest of its linger. Core takes a marked node out of its
/// receive set, but an error event still reads it (`node_err_set`). Under epoll that is
/// `EPOLLERR` (a reset or a socket error) or `EPOLLHUP`, which needs both directions shut: their
/// close once the sweep has shut our write side. A read there that ends the stream ends the
/// linger (`fOtherSideDisconnected`) and the next pass removes the peer. This peeks and never
/// consumes: bytes waiting, or their close before our shutdown, make the socket readable
/// (`fHasRecvData`), which keeps Core's loop polling until the linger ends; the sweep then
/// shuts our side at once and removes the peer at its deadline (D43).
// Core: net.cpp:2486-2490, 2522-2529, 2620-2640 SocketRecvData; util/sock.cpp:238
async fn linger_watch(core: &Weak<NetCore>, peer: &PeerHandle, sock: &dyn NetSocket) {
    loop {
        if peer.sock.lock().is_none() || core.upgrade().is_none_or(|c| c.interrupted()) {
            return;
        }
        let peeked = tokio::select! {
            r = poll_fn(|cx| sock.poll_peek(cx)) => r,
            () = peer.reader_wake.notified() => continue,
        };
        match peeked {
            // Their close alone is no hang-up, and bytes are left to the close at the linger's
            // end; either makes the socket readable, which keeps Core's loop polling (D43).
            Ok(0) if !peer.socket_shutdown.load(Ordering::SeqCst) => {
                linger_readable(core, peer).await;
                continue;
            }
            Ok(0) => {}
            Ok(_) => {
                linger_readable(core, peer).await;
                continue;
            }
            Err(e) if is_transient(&e) => continue,
            Err(_) => {}
        }
        let Some(core) = core.upgrade() else {
            return;
        };
        peer.close_socket(true);
        core.sweep_wake.notify_one();
        return;
    }
}

/// Marks a lingering peer readable, wakes the sweep (which shuts our side if the queue is
/// flushed), then waits for the sweep's shutdown or the socket's close.
async fn linger_readable(core: &Weak<NetCore>, peer: &PeerHandle) {
    let woken = peer.reader_wake.notified();
    if !peer.linger_readable.swap(true, Ordering::SeqCst)
        && let Some(c) = core.upgrade()
    {
        c.sweep_wake.notify_one();
    }
    woken.await;
}

/// The writer: Core's `SocketSendData` whenever the queue has bytes and the socket can take
/// them; idle until a push or the reader wakes it.
async fn writer(core: Weak<NetCore>, peer: PeerHandle, sock: Arc<dyn NetSocket>) {
    loop {
        let Some(c) = core.upgrade().filter(|c| !c.interrupted()) else {
            return;
        };
        if peer.sock.lock().is_none() {
            return;
        }
        let was_paused = peer.pause_send();
        let sent = peer.send_data(c.cfg.send_buffer_max, c.clock.node_now().0, |buf, more| {
            Some(sock.try_send(buf, more))
        });
        if sent.failed {
            // `fDisconnect` is set; the sweep lingers and removes the peer.
            c.sweep_wake.notify_one();
            return;
        }
        if was_paused && !peer.pause_send() {
            // Core's handler re-checks `fPauseSend` every 100 ms; ours sleeps until woken.
            c.events.wake();
        }
        if sent.bytes > 0 || (!sent.data_left && peer.disconnect_requested()) {
            // Core sends inside a pass: woken by a push to an empty queue (the wakeup pipe) or by
            // send readiness; a flushed marked peer's sweep can shut the write side now (D43).
            // Core: net.cpp:2423, 4896-4904
            c.sweep_wake.notify_one();
        }
        drop(c);
        if sent.data_left {
            peer.set_send_backlogged(sent.bytes > 0);
            let mut blocked = false;
            tokio::select! {
                _ = poll_fn(|cx| {
                    let r = sock.poll_write_ready(cx);
                    if r.is_pending() && !blocked {
                        // The socket is not writable: Core's next round receives.
                        blocked = true;
                        peer.set_send_backlogged(false);
                    }
                    r
                }) => {}
                () = peer.writer_wake.notified() => {}
            }
        } else {
            peer.set_send_backlogged(false);
            peer.writer_wake.notified().await;
        }
    }
}

/// Core's `DumpPeerAddresses`: a failed write is logged, not fatal.
// Core: addrdb.cpp:181-187 DumpPeerAddresses
fn save_peers(store: &dyn NetStore, addrman: &AddrMan) {
    if let Err(e) = store.save(NetBlob::Peers, &addrman.serialize()) {
        tracing::warn!(target: "net", "Failed to write peers.dat: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dial::loop_tests::{Net, regtest};
    use crate::sock::MemSocket;

    /// D43: while a lingering peer's socket is readable (bytes or their FIN) and not
    /// receive-paused, the sweep's next pass is at its linger deadline, as Core's `only_poll`
    /// keeps the loop polling until then; otherwise the 500 ms pass.
    // Core: net.cpp:2376-2415 HasUnpausedReceivableNode, only_poll
    #[tokio::test(start_paused = true)]
    async fn a_readable_lingering_peer_is_swept_at_its_deadline() {
        // The steady clock stands still: the linger deadline is real time, which a loaded box
        // could run past between the two passes (BL-244).
        let net = Net::held(regtest(), |_| {});
        let (ours, theirs) = MemSocket::pair();
        let peer = net.attach_with(
            Address {
                service: Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19899),
                ..Address::default()
            },
            ConnectionType::OutboundFullRelay,
            |a| a.sock = Arc::new(ours),
        );
        peer.mark_disconnect();
        assert_eq!(net.core.disconnect_nodes(), None, "an idle linger waits for the pass");
        theirs.inject(&[0; 24]);
        // The lingering reader's peek marks the peer readable; wait for that, not a number of
        // scheduler turns (paused time: the bound costs no real time).
        tokio::time::timeout(Duration::from_secs(60), async {
            while !peer.linger_readable.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the lingering reader saw the bytes");
        assert_eq!(net.core.disconnect_nodes(), peer.linger_until());
    }

    #[test]
    fn host_port() {
        assert_eq!(split_host_port("seed.dash.org", 9999), ("seed.dash.org", 9999));
        assert_eq!(split_host_port("1.2.3.4:19999", 9999), ("1.2.3.4", 19999));
        assert_eq!(split_host_port("[::1]:5", 9999), ("::1", 5));
        assert_eq!(split_host_port("::1", 9999), ("::1", 9999));
    }
}
