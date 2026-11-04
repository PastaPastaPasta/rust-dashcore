//! Core's message handling with test peers (ABI 10): a node built as Core's unit tests
//! build one (`TestingSetup`, `ConnmanTestMsg`, `PeerManager`) with ConnMan's production
//! options, peers attached through a socket that feeds their own V1 or V2 transport, messages
//! sent by them, and passes of `ThreadMessageHandler` and ticks of Core's timers run at a mock
//! time (`src/test/oracle/oracle.h`, "peer sessions").
//!
//! Core keeps the node in process-wide globals, so one session may exist per process and no
//! other oracle call may run while it does. [`PeerSession`] holds its pin's lock exclusively for
//! its lifetime (so the chain the session selected is never mistaken for an oracle network):
//! another thread's oracle call of the pin waits until the session is freed, a call on the
//! session's own thread is refused ([`ErrorKind::SessionLive`]), and the shim refuses a second
//! session. If the session's teardown fails, Core's state is unknown and every later call of the
//! pin fails with [`ErrorKind::Poisoned`].

use super::addrman::{
    take_bool, take_hash, take_le, take_list, take_optional, take_string, take_u8, take_vec,
};
use super::*;

use std::ptr::NonNull;
use std::sync::RwLockWriteGuard;

/// `ConnectionType` (`src/node/connection_types.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ConnType {
    Inbound = 0,
    OutboundFullRelay = 1,
    Manual = 2,
    Feeler = 3,
    BlockRelay = 4,
    AddrFetch = 5,
}

/// The node's transport for a peer, and the peer's (oracle.h `ORACLE_TRANSPORT_*`). An inbound
/// peer's follows `NODE_P2P_V2` in our services; a V2 outbound peer needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum Transport {
    V1 = 1,
    V2 = 2,
    /// Inbound only: the node's V2 transport detects a V1 peer.
    V2DetectingV1Peer = 3,
}

/// When the peer reads what the node writes (oracle.h `ORACLE_SEND_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum SendMode {
    /// Everything, at once.
    Eager = 0,
    /// Only through [`PeerSession::drain`].
    Controlled = 1,
}

/// `oracle_peer_session_tick`: `CConnman::InactivityCheck` on every node.
pub const TICK_INACTIVITY: u32 = 1;
/// `oracle_peer_session_tick`: `PeerManager::CheckForStaleTipAndEvictPeers`.
pub const TICK_MAINTENANCE: u32 = 2;

/// How a session starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionConfig {
    pub chain: Chain,
    /// Our service bits (`NODE_*`).
    pub our_services: u64,
    /// The session's clock (seconds, 1 ..= 2^33) until a pass moves it.
    pub mocktime: i64,
    /// Keys the stream every draw on the calling thread comes from.
    pub rng_seed: u64,
    /// Serialized blocks connected in order before the first peer (each must become the tip).
    pub blocks: Vec<Vec<u8>>,
    /// The chainstate's `nMinimumChainWork` (`-minimumchainwork`, big-endian as its hex reads)
    /// instead of the chain's default.
    pub min_chain_work: Option<[u8; 32]>,
    /// The chainstate's `max_tip_age` in seconds (`-maxtipage`) instead of Core's 6 h: how old
    /// the tip may be for `IsInitialBlockDownload` to latch to false (validation.cpp:1989).
    pub max_tip_age: Option<i64>,
    /// `-checkblockindex`: Core's block-tree consistency check after every block-index change
    /// (on in regtest dashd and Core's tests). Off changes no session's output, only its cost
    /// (`check_block_index_changes_no_transcript`).
    pub check_block_index: bool,
}

/// `oracle_substitution`: before a message is queued, `len` bytes at `dst_offset` of its
/// payload become the bytes at `src_offset` of the last `src_command` message the node sent
/// to `src_peer` (e.g. our ping nonce, echoed in a pong).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Substitution {
    pub dst_offset: u32,
    pub src_peer: i64,
    pub src_command: String,
    pub src_offset: u32,
    pub len: u32,
}

/// A block reference in a snapshot: hash (uint256 byte order) and height, or zero and -1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockRef {
    pub hash: [u8; 32],
    pub height: i32,
}

/// net_processing's `Peer` (oracle.h `oracle_peer_session_outcome`). Durations are
/// microseconds unless named otherwise.
#[derive(Clone, Debug, PartialEq)]
pub struct PeerData {
    pub misbehavior_score: i32,
    pub should_discourage: bool,
    pub their_services: u64,
    pub starting_height: i32,
    pub ping_nonce_sent: u64,
    pub ping_start_us: i64,
    pub ping_queued: bool,
    pub wants_recsigs: bool,
    pub blocks_for_inv_relay: Vec<[u8; 32]>,
    pub blocks_for_headers_relay: Vec<[u8; 32]>,
    pub continuation_block: [u8; 32],
    pub addr_relay_enabled: bool,
    pub addrs_to_send: Vec<Address>,
    pub has_addr_known: bool,
    pub getaddr_sent: bool,
    pub getaddr_recvd: bool,
    pub wants_addrv2: bool,
    pub next_addr_send_us: i64,
    pub next_local_addr_send_us: i64,
    pub addr_token_bucket: f64,
    pub addr_token_timestamp_us: i64,
    pub addr_rate_limited: u64,
    pub addr_processed: u64,
    pub inv_triggered_getheaders_before_sync: bool,
    /// Microseconds (0: none sent).
    pub last_getheaders: i64,
    pub wants_dsq: bool,
    pub can_tx_relay: bool,
    pub relay_txs: bool,
}

/// net_processing's `CNodeState`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeStateData {
    pub best_known_block: BlockRef,
    pub last_unknown_block: [u8; 32],
    pub last_common_block: BlockRef,
    pub best_header_sent: BlockRef,
    pub unconnecting_headers: i32,
    pub sync_started: bool,
    pub headers_sync_timeout_us: i64,
    pub preferred_download: bool,
    pub prefer_headers: bool,
    pub prefer_headers_compressed: bool,
    pub chain_sync_timeout_s: i64,
    pub chain_sync_work_header: BlockRef,
    pub chain_sync_sent_getheaders: bool,
    pub chain_sync_protect: bool,
    pub last_block_announcement: i64,
    pub inbound: bool,
}

/// What the node did with one peer since the last [`PeerSession::peer_outcome`] call, then
/// the peer's state (for a removed peer, as it was just before `FinalizeNode`).
#[derive(Clone, Debug, PartialEq)]
pub struct PeerOutcome {
    /// Messages pushed to the peer, in order: (command, payload).
    pub sent: Vec<(String, Vec<u8>)>,
    /// `Misbehaving: peer=<id> ...` log lines.
    pub misbehaving: Vec<String>,
    /// `FinalizeNode` ran: the peer id is no longer valid.
    pub removed: bool,
    pub disconnect: bool,
    pub successfully_connected: bool,
    pub version: i32,
    pub common_version: i32,
    /// The node's view of the peer's services.
    pub services: u64,
    /// The node's transport (`TransportProtocolType`: 0 detecting, 1 V1, 2 V2).
    pub transport: u8,
    pub pause_send: bool,
    /// The queued messages' charge (`m_send_memusage`).
    pub send_queue_memusage: u64,
    /// The transport's charge (`GetSendMemoryUsage`).
    pub transport_memusage: u64,
    pub queued_messages: u64,
    /// `nSendBytes`: bytes the node wrote.
    pub send_bytes: u64,
    /// `fPauseRecv`: the queued messages exceed the receive flood size, so the node does not read.
    pub pause_recv: bool,
    /// Bytes in the peer's socket the node has not read.
    pub peer_unread_bytes: u64,
    /// Bytes the peer has read.
    pub peer_read_bytes: u64,
    /// Messages the peer's transport has decoded.
    pub peer_decoded_messages: u64,
    pub peer: Option<PeerData>,
    pub state: Option<NodeStateData>,
}

/// An `AddrMan`, `AddTimeData` or X11 call the node made (oracle.h call log).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Add {
        addrs: Vec<Address>,
        source: NetAddr,
        time_penalty: i64,
        added: bool,
    },
    Good {
        addr: Service,
        time: i64,
        moved: bool,
    },
    Attempt {
        addr: Service,
        count_failure: bool,
        time: i64,
    },
    Connected {
        addr: Service,
        time: i64,
    },
    SetServices {
        addr: Service,
        services: u64,
    },
    TimeData {
        ip: NetAddr,
        offset: i64,
    },
    /// `CBlockHeader::GetHash` computed this header hash (internal byte order), once per call,
    /// in a node logging with `-debug`: hashes only `BCLog::NET` lines compute are included
    /// (oracle.h names the sites).
    X11 {
        hash: [u8; 32],
    },
}

/// PeerManager's process-wide state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGlobals {
    pub sync_started: i32,
    pub preferred_download_peers: i32,
    pub outbound_peers_protected: i32,
    pub best_height: i32,
    pub initial_sync_finished: bool,
    pub last_tip_update_s: i64,
    pub stale_tip_check_time_s: i64,
    pub last_block_inv_triggering_headers_sync: [u8; 32],
}

/// The session's draws and calls since the last [`PeerSession::session_outcome`], its globals
/// and its tip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionOutcome {
    pub draws: Vec<Draw>,
    pub calls: Vec<Call>,
    pub globals: SessionGlobals,
    pub tip: BlockRef,
}

/// A live session. Dropping it stops the node and removes its data directory.
pub struct PeerSession {
    oracle: Oracle,
    handle: NonNull<ffi::OraclePeerSession>,
    // Dropped in this order: the thread's mark first, then the lock.
    _hold: Hold,
    state: RwLockWriteGuard<'static, PinState>,
}

/// How oracle_last_error() starts after a session's teardown failed (oracle.h).
const TEARDOWN_FAILED: &str = "teardown failed";

impl Drop for PeerSession {
    fn drop(&mut self) {
        unsafe { (self.oracle.api.peer_session_free)(self.handle.as_ptr()) }
        // Free leaves this thread's last error empty unless the teardown failed (oracle.h).
        let error = self.oracle.last_error();
        if error.starts_with(TEARDOWN_FAILED) {
            self.state.poisoned = Some(error);
        }
    }
}

impl Oracle {
    /// Start a session. `Ok(None)` if a block of `config.blocks` does not become the tip.
    ///
    /// Blocks while another thread uses this pin; [`ErrorKind::SessionLive`] if the calling
    /// thread holds a session already (one per process).
    pub fn peer_session_new(&self, config: &SessionConfig) -> Result<Option<PeerSession>> {
        match self.pin.held() {
            Held::None => {}
            Held::Session => return Err(session_live(self.pin)),
            held => unreachable!("a session start inside another oracle call ({held:?})"),
        }
        let mut state = self.pin.state().write().unwrap_or_else(|e| e.into_inner());
        state.usable()?;
        // The session selects its own chain; the next oracle call selects the network again.
        state.chain = None;
        let hold = Hold::new(self.pin, Held::Session);
        let (name, mut args) = chain_cstrings(&config.chain)?;
        if let Some(work) = config.min_chain_work {
            let hex: String = work.iter().map(|b| format!("{b:02x}")).collect();
            args.push(CString::new(format!("-minimumchainwork={hex}")).expect("no NUL"));
        }
        if let Some(age) = config.max_tip_age {
            args.push(CString::new(format!("-maxtipage={age}")).expect("no NUL"));
        }
        let check = u8::from(config.check_block_index);
        args.push(CString::new(format!("-checkblockindex={check}")).expect("no NUL"));
        let argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
        let mut fixture = Vec::new();
        write_compact_size(&mut fixture, config.blocks.len() as u64);
        config.blocks.iter().for_each(|b| fixture.extend(b));
        let mut handle = std::ptr::null_mut();
        let started = self
            .status(unsafe {
                (self.api.peer_session_new)(
                    name.as_ptr(),
                    argv.as_ptr(),
                    argv.len(),
                    config.our_services,
                    config.mocktime,
                    config.rng_seed,
                    fixture.as_ptr(),
                    fixture.len(),
                    &mut handle,
                )
            })
            .inspect_err(|e| {
                // A failed start tore its session down; that teardown failing poisons the pin.
                if e.message.starts_with(TEARDOWN_FAILED) {
                    state.poisoned = Some(e.message.clone());
                }
            })?;
        Ok(started.then(|| PeerSession {
            oracle: *self,
            handle: NonNull::new(handle).expect("the shim returned a handle"),
            _hold: hold,
            state,
        }))
    }
}

impl PeerSession {
    /// Attach a V1 peer that reads at once at `addr` (distinct per session); returns its
    /// `NodeId`.
    pub fn add_peer(&mut self, conn_type: ConnType, addr: &Service) -> Result<i64> {
        self.add_peer_with(conn_type, Transport::V1, SendMode::Eager, addr)
    }

    /// Attach a peer at `addr` (distinct per session); returns its `NodeId`.
    pub fn add_peer_with(
        &mut self,
        conn_type: ConnType,
        transport: Transport,
        send_mode: SendMode,
        addr: &Service,
    ) -> Result<i64> {
        let mut bytes = Vec::new();
        addr.encode(&mut bytes);
        let mut peer = -1;
        self.oracle.ok(unsafe {
            (self.oracle.api.peer_session_add_peer)(
                self.handle.as_ptr(),
                conn_type as c_int,
                transport as c_int,
                send_mode as c_int,
                bytes.as_ptr(),
                bytes.len(),
                &mut peer,
            )
        })?;
        Ok(peer)
    }

    /// The peer reads at most `max_bytes` of what the node writes; returns the bytes written.
    pub fn drain(&mut self, peer: i64, max_bytes: u64) -> Result<u64> {
        let mut written = 0;
        self.oracle.ok(unsafe {
            (self.oracle.api.peer_session_drain)(
                self.handle.as_ptr(),
                peer,
                max_bytes,
                &mut written,
            )
        })?;
        Ok(written)
    }

    /// `peer` sends `(command, payload)`, after applying `subs`.
    pub fn deliver(
        &mut self,
        peer: i64,
        command: &str,
        payload: &[u8],
        subs: &[Substitution],
    ) -> Result<()> {
        let argument = |message: String| Error {
            kind: ErrorKind::Argument,
            message,
        };
        let command =
            CString::new(command).map_err(|_| argument(format!("NUL in command {command:?}")))?;
        let subs = subs
            .iter()
            .map(|s| {
                let mut src_command = [0 as c_char; 13];
                if s.src_command.len() > 12 || s.src_command.contains('\0') {
                    return Err(argument(format!("marker command {:?}", s.src_command)));
                }
                for (d, b) in src_command.iter_mut().zip(s.src_command.bytes()) {
                    *d = b as c_char;
                }
                Ok(ffi::OracleSubstitution {
                    dst_offset: s.dst_offset,
                    src_peer: s.src_peer,
                    src_command,
                    src_offset: s.src_offset,
                    len: s.len,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.oracle.ok(unsafe {
            (self.oracle.api.peer_session_deliver)(
                self.handle.as_ptr(),
                peer,
                command.as_ptr(),
                payload.as_ptr(),
                payload.len(),
                subs.as_ptr(),
                subs.len(),
            )
        })
    }

    /// One `ThreadMessageHandler` iteration at `mocktime`, then removal of disconnected peers.
    pub fn send_pass(&mut self, mocktime: i64) -> Result<()> {
        self.oracle
            .ok(unsafe { (self.oracle.api.peer_session_send_pass)(self.handle.as_ptr(), mocktime) })
    }

    /// Core's timers at `mocktime` (`what`: [`TICK_INACTIVITY`] | [`TICK_MAINTENANCE`]), then
    /// removal of disconnected peers.
    pub fn tick(&mut self, mocktime: i64, what: u32) -> Result<()> {
        self.oracle.ok(unsafe {
            (self.oracle.api.peer_session_tick)(self.handle.as_ptr(), mocktime, what)
        })
    }

    /// `oracle_peer_session_outcome`'s bytes for `peer` (-1: the session).
    pub fn outcome_raw(&mut self, peer: i64) -> Result<Vec<u8>> {
        self.outcome_raw_with_capacity(peer, 16 * 1024)
    }

    /// [`PeerSession::outcome_raw`] with a first buffer of `capacity` bytes. Output that does
    /// not fit is collected by a second call, which the shim answers from what the first one
    /// took (the outcome's logs are not drained twice).
    pub fn outcome_raw_with_capacity(&mut self, peer: i64, capacity: usize) -> Result<Vec<u8>> {
        self.oracle.with_buffer(capacity, |out, len| unsafe {
            (self.oracle.api.peer_session_outcome)(self.handle.as_ptr(), peer, out, len)
        })
    }

    pub fn peer_outcome(&mut self, peer: i64) -> Result<PeerOutcome> {
        if peer < 0 {
            return Err(Error {
                kind: ErrorKind::Argument,
                message: format!("peer {peer}"),
            });
        }
        let raw = self.outcome_raw(peer)?;
        Ok(PeerOutcome::decode(&raw).expect("the shim's peer outcome format"))
    }

    pub fn session_outcome(&mut self) -> Result<SessionOutcome> {
        let raw = self.outcome_raw(-1)?;
        Ok(SessionOutcome::decode(&raw).expect("the shim's session outcome format"))
    }
}

fn take_block(r: &mut &[u8]) -> Option<BlockRef> {
    Some(BlockRef {
        hash: take_hash(r)?,
        height: take_le(r)?,
    })
}

impl PeerData {
    fn decode(r: &mut &[u8]) -> Option<Self> {
        Some(Self {
            misbehavior_score: take_le(r)?,
            should_discourage: take_bool(r)?,
            their_services: take_le(r)?,
            starting_height: take_le(r)?,
            ping_nonce_sent: take_le(r)?,
            ping_start_us: take_le(r)?,
            ping_queued: take_bool(r)?,
            wants_recsigs: take_bool(r)?,
            blocks_for_inv_relay: take_list(r, take_hash)?,
            blocks_for_headers_relay: take_list(r, take_hash)?,
            continuation_block: take_hash(r)?,
            addr_relay_enabled: take_bool(r)?,
            addrs_to_send: take_list(r, Address::decode)?,
            has_addr_known: take_bool(r)?,
            getaddr_sent: take_bool(r)?,
            getaddr_recvd: take_bool(r)?,
            wants_addrv2: take_bool(r)?,
            next_addr_send_us: take_le(r)?,
            next_local_addr_send_us: take_le(r)?,
            addr_token_bucket: f64::from_bits(take_le(r)?),
            addr_token_timestamp_us: take_le(r)?,
            addr_rate_limited: take_le(r)?,
            addr_processed: take_le(r)?,
            inv_triggered_getheaders_before_sync: take_bool(r)?,
            last_getheaders: take_le(r)?,
            wants_dsq: take_bool(r)?,
            can_tx_relay: take_bool(r)?,
            relay_txs: take_bool(r)?,
        })
    }
}

impl NodeStateData {
    fn decode(r: &mut &[u8]) -> Option<Self> {
        Some(Self {
            best_known_block: take_block(r)?,
            last_unknown_block: take_hash(r)?,
            last_common_block: take_block(r)?,
            best_header_sent: take_block(r)?,
            unconnecting_headers: take_le(r)?,
            sync_started: take_bool(r)?,
            headers_sync_timeout_us: take_le(r)?,
            preferred_download: take_bool(r)?,
            prefer_headers: take_bool(r)?,
            prefer_headers_compressed: take_bool(r)?,
            chain_sync_timeout_s: take_le(r)?,
            chain_sync_work_header: take_block(r)?,
            chain_sync_sent_getheaders: take_bool(r)?,
            chain_sync_protect: take_bool(r)?,
            last_block_announcement: take_le(r)?,
            inbound: take_bool(r)?,
        })
    }
}

impl PeerOutcome {
    pub fn decode(mut r: &[u8]) -> Option<Self> {
        let r = &mut r;
        let outcome = Self {
            sent: take_list(r, |r| Some((take_string(r)?, take_vec(r)?)))?,
            misbehaving: take_list(r, take_string)?,
            removed: take_bool(r)?,
            disconnect: take_bool(r)?,
            successfully_connected: take_bool(r)?,
            version: take_le(r)?,
            common_version: take_le(r)?,
            services: take_le(r)?,
            transport: take_u8(r)?,
            pause_send: take_bool(r)?,
            send_queue_memusage: take_le(r)?,
            transport_memusage: take_le(r)?,
            queued_messages: take_le(r)?,
            send_bytes: take_le(r)?,
            pause_recv: take_bool(r)?,
            peer_unread_bytes: take_le(r)?,
            peer_read_bytes: take_le(r)?,
            peer_decoded_messages: take_le(r)?,
            peer: take_optional(r, PeerData::decode)?,
            state: take_optional(r, NodeStateData::decode)?,
        };
        r.is_empty().then_some(outcome)
    }
}

impl Call {
    fn decode(r: &mut &[u8]) -> Option<Self> {
        Some(match take_u8(r)? {
            0 => Call::Add {
                addrs: take_list(r, Address::decode)?,
                source: NetAddr::decode(r)?,
                time_penalty: take_le(r)?,
                added: take_bool(r)?,
            },
            1 => Call::Good {
                addr: Service::decode(r)?,
                time: take_le(r)?,
                moved: take_bool(r)?,
            },
            2 => Call::Attempt {
                addr: Service::decode(r)?,
                count_failure: take_bool(r)?,
                time: take_le(r)?,
            },
            3 => Call::Connected {
                addr: Service::decode(r)?,
                time: take_le(r)?,
            },
            4 => Call::SetServices {
                addr: Service::decode(r)?,
                services: take_le(r)?,
            },
            5 => Call::TimeData {
                ip: NetAddr::decode(r)?,
                offset: take_le(r)?,
            },
            6 => Call::X11 {
                hash: take_hash(r)?,
            },
            _ => return None,
        })
    }
}

impl SessionOutcome {
    pub fn decode(mut r: &[u8]) -> Option<Self> {
        let r = &mut r;
        let outcome = Self {
            draws: Draw::decode_log(r)?,
            calls: take_list(r, Call::decode)?,
            globals: SessionGlobals {
                sync_started: take_le(r)?,
                preferred_download_peers: take_le(r)?,
                outbound_peers_protected: take_le(r)?,
                best_height: take_le(r)?,
                initial_sync_finished: take_bool(r)?,
                last_tip_update_s: take_le(r)?,
                stale_tip_check_time_s: take_le(r)?,
                last_block_inv_triggering_headers_sync: take_hash(r)?,
            },
            tip: BlockRef {
                height: take_le(r)?,
                hash: take_hash(r)?,
            },
        };
        r.is_empty().then_some(outcome)
    }
}
