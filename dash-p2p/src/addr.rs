//! Address intake: `addr`/`addrv2` and `SetupAddressRelay` (P2a §3.6.1–3.6.3; v24
//! `net_processing.cpp:4388-4482`, `:6283-6298`).
//!
//! Relaying (`RelayAddress`, `MaybeSendAddr`, the `getaddr` cache) is the node processor's
//! (P2a-net); intake returns the addresses Core would relay so the processor can.

use dash_p2p_core::{Addr, AddrV2Msg, P2pDecodeError, ServiceFlags};
use dash_types::codec::BaseCodec;

use crate::bloom::RollingBloom;
use crate::conn::{PeerHandle, Permissions, Why};
use crate::netaddr::{Address, NetAddr, Service};
use crate::netcore::NetCore;
use crate::policy::{Step, Throw};

/// Core's `MAX_ADDR_TO_SEND`.
pub const MAX_ADDR_TO_SEND: usize = 1000;
/// Core's `MAX_ADDR_PROCESSING_TOKEN_BUCKET`.
pub const MAX_ADDR_PROCESSING_TOKEN_BUCKET: f64 = 1000.0;
/// Core's `MAX_ADDR_RATE_PER_SECOND`.
pub const MAX_ADDR_RATE_PER_SECOND: f64 = 0.1;

/// A peer's address-relay state (Core's `Peer::m_addr_*`, P2a §3.6.1).
#[derive(Clone, Debug)]
pub struct AddrRelay {
    /// `m_addr_relay_enabled`.
    pub enabled: bool,
    /// `m_addr_known`: allocated by [`setup_address_relay`] (5000 entries, 0.001).
    pub known: Option<RollingBloom>,
    /// `m_wants_addrv2`.
    pub wants_addrv2: bool,
    /// `m_getaddr_sent`.
    pub getaddr_sent: bool,
    /// `m_getaddr_recvd`.
    pub getaddr_recvd: bool,
    /// `m_addrs_to_send` (at most 1000).
    pub to_send: Vec<Address>,
    /// `m_addr_token_bucket`.
    pub token_bucket: f64,
    /// `m_addr_token_timestamp` in microseconds.
    pub token_timestamp_us: i64,
    /// `m_addr_processed`.
    pub processed: u64,
    /// `m_addr_rate_limited`.
    pub rate_limited: u64,
    /// `m_next_addr_send` in microseconds.
    pub next_send_us: i64,
    /// `m_next_local_addr_send` in microseconds.
    pub next_local_send_us: i64,
}

impl AddrRelay {
    /// The state Core's `Peer` starts with when the peer is initialized at `now_us`
    /// (`GetTime<µs>()`): one token, stamped now (v24 `net_processing.cpp:414-416`).
    pub fn new(now_us: i64) -> Self {
        Self {
            enabled: false,
            known: None,
            wants_addrv2: false,
            getaddr_sent: false,
            getaddr_recvd: false,
            to_send: Vec::new(),
            token_bucket: 1.0,
            token_timestamp_us: now_us,
            processed: 0,
            rate_limited: 0,
            next_send_us: 0,
            next_local_send_us: 0,
        }
    }
}

/// What an `addr`/`addrv2` did besides updating AddrMan.
#[derive(Debug, Default)]
pub struct Intake {
    /// The step for the dispatcher (always set).
    pub step: Option<Step>,
    /// The misbehaviour Core scores (`+20 "%s message size = %u"`).
    pub misbehaving: Option<(i32, String)>,
    /// Addresses Core passes to `RelayAddress`, in Core's order, each with Core's `reachable`
    /// flag (one relay target or two).
    pub relay: Vec<(Address, bool)>,
}

impl Intake {
    fn step(step: Step) -> Self {
        Self {
            step: Some(step),
            ..Self::default()
        }
    }
}

/// Core's `SetupAddressRelay`: false for block-relay-only connections, otherwise enables relay
/// once, allocating `m_addr_known`, whose tweak is drawn from the core's RNG
/// (`CRollingBloomFilter::reset`'s `GetRand<unsigned int>()`). Takes `core` for that draw.
// Core: net_processing.cpp:6283-6298 SetupAddressRelay
pub fn setup_address_relay(core: &NetCore, peer: &PeerHandle, relay: &mut AddrRelay) -> bool {
    // We don't participate in addr relay with outbound block-relay-only connections to prevent
    // providing adversaries with the additional information of addr traffic to infer the link.
    if peer.conn_type() == crate::conn::ConnectionType::BlockRelay {
        return false;
    }
    if !relay.enabled {
        relay.enabled = true;
        // Core: common/bloom.cpp CRollingBloomFilter::reset (`GetRand<unsigned int>()`)
        let tweak = core.get_rand(u64::from(u32::MAX)) as u32;
        relay.known = Some(RollingBloom::new(5000, 0.001, tweak));
    }
    true
}

/// Core's `MayHaveUsefulAddressDB`.
// Core: protocol.h:392-395 MayHaveUsefulAddressDB
fn may_have_useful_address_db(services: ServiceFlags) -> bool {
    services.0 & (ServiceFlags::NODE_NETWORK.0 | ServiceFlags::NODE_NETWORK_LIMITED.0) != 0
}

/// Reads Core's `std::vector<CAddress>` in the v1 or BIP155 format.
fn read_addrs(payload: &[u8], v2: bool) -> Result<Vec<Address>, Throw> {
    let mut data = payload;
    let throw = |e| Throw::from_decode(&P2pDecodeError::Consensus(e));
    if v2 {
        let msg = AddrV2Msg::decode(&mut data).map_err(throw)?;
        Ok(msg
            .addrs
            .iter()
            .map(|e| Address {
                service: Service::new(NetAddr::from_addrv2(&e.addr), e.port),
                services: e.services,
                time: e.time,
            })
            .collect())
    } else {
        let msg = Addr::decode(&mut data).map_err(throw)?;
        Ok(msg
            .addrs
            .iter()
            .map(|e| Address {
                service: Service::from_v1(&e.addr),
                services: e.services,
                time: e.time,
            })
            .collect())
    }
}

/// Core's `addr`/`addrv2` branch (`v2` selects the BIP155 codec). `now_us` is
/// `GetTime<µs>()`; Core's `Now<NodeSeconds>()` is its whole seconds.
// Core: net_processing.cpp:4388-4482 ProcessMessage (ADDR, ADDRV2)
pub fn on_addr(
    core: &NetCore,
    peer: &PeerHandle,
    relay: &mut AddrRelay,
    payload: &[u8],
    v2: bool,
    now_us: i64,
) -> Intake {
    let msg_type = if v2 {
        "addrv2"
    } else {
        "addr"
    };
    let mut addrs = match read_addrs(payload, v2) {
        Ok(a) => a,
        Err(t) => return Intake::step(Step::Throw(t)),
    };

    if !setup_address_relay(core, peer, relay) {
        tracing::debug!(
            target: "net",
            "ignoring {msg_type} message from {} peer={}",
            peer.conn_type().as_str(),
            peer.id()
        );
        return Intake::step(Step::Ignored);
    }

    let n = addrs.len();
    if n > MAX_ADDR_TO_SEND {
        return Intake {
            step: Some(Step::Done),
            misbehaving: Some((20, format!("{msg_type} message size = {n}"))),
            relay: Vec::new(),
        };
    }

    // Store the new addresses.
    let mut ok = Vec::new();
    let mut to_relay = Vec::new();
    let current_a_time = now_us.div_euclid(1_000_000);

    // Update/increment addr rate limiting bucket.
    let current_time = now_us;
    if relay.token_bucket < MAX_ADDR_PROCESSING_TOKEN_BUCKET {
        // Don't increment bucket if it's already full.
        let time_diff = (current_time - relay.token_timestamp_us).max(0);
        let increment = time_diff as f64 / 1_000_000.0 * MAX_ADDR_RATE_PER_SECOND;
        relay.token_bucket = (relay.token_bucket + increment).min(MAX_ADDR_PROCESSING_TOKEN_BUCKET);
    }
    relay.token_timestamp_us = current_time;

    let rate_limited = !peer.has_permission(Permissions::ADDR);
    let mut num_proc = 0u64;
    let mut num_rate_limit = 0u64;
    // Core: net_processing.cpp:4429 `Shuffle(…, FastRandomContext())`
    core.new_rng().shuffle(&mut addrs);
    let cfg = core.config();
    for mut addr in addrs {
        if core.interrupted() {
            return Intake::step(Step::Done);
        }

        // Apply rate limiting.
        if relay.token_bucket < 1.0 {
            if rate_limited {
                num_rate_limit += 1;
                continue;
            }
        } else {
            relay.token_bucket -= 1.0;
        }
        // We only bother storing full nodes, though this may include things which we would not
        // make an outbound connection to, in part because we may make feeler connections to
        // them.
        if !may_have_useful_address_db(addr.services)
            && !core.has_all_desirable_services(addr.services)
        {
            continue;
        }

        let time = i64::from(addr.time);
        if time <= 100_000_000 || time > current_a_time + 10 * 60 {
            addr.time = (current_a_time - 5 * 24 * 60 * 60) as u32;
        }
        // Core: net_processing.cpp:1159-1163 AddAddressKnown
        relay.known.as_mut().expect("set up above").insert(&addr.service.key());
        let ip = &addr.service.addr;
        if core.banlist().is_discouraged(ip) || core.banlist().is_banned(ip, current_a_time) {
            // Do not process banned/discouraged addresses beyond remembering we received them.
            continue;
        }
        num_proc += 1;
        let reachable = core.is_reachable(ip.network(cfg.allow_private_net));
        if i64::from(addr.time) > current_a_time - 10 * 60
            && !relay.getaddr_sent
            && n <= 10
            && ip.is_routable(cfg.allow_private_net)
        {
            // Relay to a limited number of other nodes.
            to_relay.push((addr.clone(), reachable));
        }
        // Do not store addresses outside our network.
        if reachable {
            ok.push(addr);
        }
    }
    relay.processed += num_proc;
    relay.rate_limited += num_rate_limit;
    tracing::debug!(
        target: "net",
        "Received addr: {n} addresses ({num_proc} processed, {num_rate_limit} rate-limited) from peer={}",
        peer.id()
    );

    core.addrman().add(&ok, &peer.addr().service.addr, 2 * 60 * 60, current_a_time);
    if n < 1000 {
        relay.getaddr_sent = false;
    }

    let mut step = Step::Done;
    // AddrFetch: Require multiple addresses to avoid disconnecting on self-announcements.
    if peer.conn_type() == crate::conn::ConnectionType::AddrFetch && n > 1 {
        tracing::debug!(
            target: "net",
            "addrfetch connection completed peer={}; disconnecting",
            peer.id()
        );
        core.disconnect(peer.id(), Why::Processor("addrfetch connection completed"));
        step = Step::Disconnect;
    }
    Intake {
        step: Some(step),
        misbehaving: None,
        relay: to_relay,
    }
}
