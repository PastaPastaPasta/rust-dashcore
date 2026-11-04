//! Ping, pong and address intake (P2a §3.5, §3.6.1–3.6.3; v24.0.0-rc.3
//! `net_processing.cpp:4388-4482`, `:5456-5527`, `:6148-6184`, `:6283-6298`). Core's own
//! coverage of these branches is functional (`p2p_ping.py`, `p2p_addr_relay.py`,
//! `p2p_addrv2_relay.py`, `p2p_addrfetch.py`); the cases below transcribe their unit-level
//! assertions: the timeout and nonce rules, the 1000-entry limit, the token bucket, the relay
//! choice and the addr-fetch disconnect.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use dash_clock::MockClock;
use dash_p2p::addr::{self, AddrRelay};
use dash_p2p::conn::{ConnectionType, Permissions};
use dash_p2p::ping::{self, PING_INTERVAL_US, PingState, TIMEOUT_INTERVAL_US};
use dash_p2p::store::NoStore;
use dash_p2p::{
    Address, Attach, CommandString, MemSocket, NetAddr, NetConfig, NetCore, NetEvents, Network,
    PeerHandle, Resolver, Service, ServiceFlags, Step,
};
use dash_p2p_core::{Addr, AddrV2Entry, AddrV2Msg, TimestampedAddr};
use dash_params::{ChainParamsBuilder, ChainSetup, Network as CoreNetwork, Release};
use dash_primitives::{AddrV1, AddrV2, ServiceV1};
use dash_types::codec::BaseCodec;

const NOW: i64 = 1_700_000_000;
const NOW_US: i64 = NOW * 1_000_000;

struct Events;

impl NetEvents for Events {
    fn wake(&self) {}

    fn initialize(&self, _core: &NetCore, _peer: &PeerHandle, _our_services: ServiceFlags) {}

    fn finalize(&self, _core: &NetCore, _peer: &PeerHandle) {}
}

struct NoResolver;

impl Resolver for NoResolver {
    fn resolve<'a>(
        &'a self,
        _host: &'a str,
        _max: usize,
        _allow_lookup: bool,
    ) -> Pin<Box<dyn Future<Output = Vec<NetAddr>> + Send + 'a>> {
        Box::pin(async { Vec::new() })
    }
}

fn params() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(CoreNetwork::Regtest, Release::V24_0).build().unwrap())
}

fn new_core(f: impl FnOnce(&mut NetConfig)) -> Arc<NetCore> {
    let mut cfg = NetConfig::new(params(), ServiceFlags::NODE_NETWORK);
    cfg.deterministic = true;
    cfg.peer_timeout = 60;
    f(&mut cfg);
    NetCore::new(
        cfg,
        tokio::runtime::Handle::current(),
        Arc::new(NoStore),
        Arc::new(NoResolver),
        Arc::new(MockClock::at(NOW)),
        Arc::new(Events),
    )
    .unwrap()
}

fn peer(
    core: &NetCore,
    conn_type: ConnectionType,
    permissions: Permissions,
) -> (PeerHandle, MemSocket) {
    let (ours, theirs) = MemSocket::pair();
    let p = core.attach(Attach {
        sock: Arc::new(ours),
        addr: Address {
            service: Service::new(NetAddr::ipv4([1, 2, 3, 4]), 19899),
            services: ServiceFlags::NODE_NETWORK,
            time: 0,
        },
        addr_bind: Service::default(),
        dest: None,
        conn_type,
        inbound_onion: false,
        permissions,
        use_v2: false,
        grant: None,
        masternode: false,
        masternode_probe: false,
    });
    (p, theirs)
}

/// Polls `cond` every 2 ms for up to five seconds.
async fn until(mut cond: impl FnMut() -> bool) {
    for _ in 0..2500 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("condition not reached in 5 s");
}

/// The (command, payload) of the whole V1 frames in `bytes`.
fn v1_frames(mut bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    while bytes.len() >= 24 {
        let len = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
        if bytes.len() < 24 + len {
            break;
        }
        let cmd = CommandString::from_bytes(bytes[4..16].try_into().unwrap());
        out.push((cmd.as_str().to_owned(), bytes[24..24 + len].to_vec()));
        bytes = &bytes[24 + len..];
    }
    out
}

/// Waits for `n` frames from the core's side of `theirs`.
async fn frames(theirs: &MemSocket, n: usize) -> Vec<(String, Vec<u8>)> {
    let mut bytes = Vec::new();
    until(|| {
        bytes.extend(theirs.drain());
        v1_frames(&bytes).len() >= n
    })
    .await;
    v1_frames(&bytes)
}

// --- Ping -----------------------------------------------------------------------------------

/// `MaybeSendPing`: the first pass pings (`m_ping_start` starts at 0), an outstanding ping
/// blocks the next until it is answered, an RPC-queued ping goes regardless, and every nonce is
/// non-zero.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maybe_send_ping_schedule() {
    let core = new_core(|_| {});
    let (p, theirs) = peer(&core, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let mut st = PingState::default();
    ping::maybe_send_ping(&core, &p, &mut st, NOW_US);
    let first = st.nonce_sent;
    assert_ne!(first, 0);
    assert_eq!(st.start_us, NOW_US);
    // Outstanding: nothing, even after the interval.
    ping::maybe_send_ping(&core, &p, &mut st, NOW_US + PING_INTERVAL_US + 1);
    assert_eq!(st.nonce_sent, first);
    // Answered, but within the interval: nothing.
    assert_eq!(ping::on_pong(&p, &mut st, &first.to_le_bytes(), NOW_US + 5), Step::Done);
    assert_eq!(st.nonce_sent, 0);
    ping::maybe_send_ping(&core, &p, &mut st, NOW_US + PING_INTERVAL_US);
    assert_eq!(st.nonce_sent, 0);
    // After the interval: a new ping.
    ping::maybe_send_ping(&core, &p, &mut st, NOW_US + PING_INTERVAL_US + 1);
    let second = st.nonce_sent;
    assert_ne!(second, 0);
    // Queued by the RPC: sent although one is outstanding.
    st.queued = true;
    ping::maybe_send_ping(&core, &p, &mut st, NOW_US + PING_INTERVAL_US + 2);
    assert!(!st.queued);
    let third = st.nonce_sent;
    let got = frames(&theirs, 3).await;
    let nonces: Vec<u64> = got
        .iter()
        .map(|(cmd, payload)| {
            assert_eq!(cmd, "ping");
            u64::from_le_bytes(payload[..].try_into().unwrap())
        })
        .collect();
    assert_eq!(nonces, vec![first, second, third]);
    assert_eq!(p.last_ping_us(), 5);
    assert_eq!(st.last_us, Some(5));
}

/// The ping timeout: only once inactivity checks apply (`m_connected + -peertimeout < now`), a
/// ping outstanding for more than 20 minutes disconnects.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ping_timeout() {
    let core = new_core(|_| {});
    let (p, _t) = peer(&core, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let start = NOW_US;
    // Exactly 20 minutes: not yet.
    let mut st = PingState {
        nonce_sent: 7,
        start_us: start,
        ..PingState::default()
    };
    ping::maybe_send_ping(&core, &p, &mut st, start + TIMEOUT_INTERVAL_US);
    assert!(!p.disconnect_requested());
    assert_eq!(st.nonce_sent, 7, "no new ping while one is outstanding");
    // Inactivity checks not yet running (`-peertimeout` 60 s from connect): no timeout.
    let core2 = new_core(|cfg| cfg.peer_timeout = 1 << 40);
    let (p2, _t2) = peer(&core2, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let mut st2 = PingState {
        nonce_sent: 7,
        start_us: start,
        ..PingState::default()
    };
    ping::maybe_send_ping(&core2, &p2, &mut st2, start + TIMEOUT_INTERVAL_US + 1);
    assert!(!p2.disconnect_requested());
    // Past 20 minutes: disconnect.
    ping::maybe_send_ping(&core, &p, &mut st, start + TIMEOUT_INTERVAL_US + 1);
    assert!(p.disconnect_requested());
    assert_eq!(st.nonce_sent, 7);
}

/// `ping`: a short payload throws (no `pong`); otherwise `pong` echoes the nonce at once,
/// ignoring trailing bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ping_replies_pong() {
    let core = new_core(|_| {});
    let (p, theirs) = peer(&core, ConnectionType::Inbound, Permissions::NONE);
    match ping::on_ping(&core, &p, &[1, 2, 3, 4, 5, 6, 7]) {
        Step::Throw(t) => {
            assert_eq!(t.what, "DataStream::read(): end of data: iostream error");
            assert_eq!(t.type_name, "NSt8ios_base7failureB5cxx11E");
        }
        other => panic!("{other:?}"),
    }
    let mut payload = 0x1122_3344_5566_7788u64.to_le_bytes().to_vec();
    payload.push(0xFF);
    assert_eq!(ping::on_ping(&core, &p, &payload), Step::Done);
    let got = frames(&theirs, 1).await;
    assert_eq!(got, vec![("pong".to_owned(), payload[..8].to_vec())]);
}

/// `pong`'s branches: a short payload finishes the ping, an unsolicited pong does not, a match
/// records the round trip (last and minimum), a mismatch keeps waiting, nonce zero finishes, and
/// a negative round trip is a "Timing mishap" that still finishes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pong_branches() {
    let core = new_core(|_| {});
    let (p, _t) = peer(&core, ConnectionType::Inbound, Permissions::NONE);
    let outstanding = |nonce| PingState {
        nonce_sent: nonce,
        start_us: 1_000,
        ..PingState::default()
    };
    // Short payload.
    let mut st = outstanding(9);
    assert_eq!(ping::on_pong(&p, &mut st, &[0; 7], 2_000), Step::Done);
    assert_eq!(st.nonce_sent, 0);
    // Unsolicited.
    let mut st = outstanding(0);
    ping::on_pong(&p, &mut st, &9u64.to_le_bytes(), 2_000);
    assert_eq!(st.nonce_sent, 0);
    assert_eq!(p.last_ping_us(), 0);
    // Mismatch: still outstanding.
    let mut st = outstanding(9);
    ping::on_pong(&p, &mut st, &8u64.to_le_bytes(), 2_000);
    assert_eq!(st.nonce_sent, 9);
    // Nonce zero: finished.
    ping::on_pong(&p, &mut st, &0u64.to_le_bytes(), 2_000);
    assert_eq!(st.nonce_sent, 0);
    assert_eq!(p.last_ping_us(), 0);
    // Matches: last and minimum.
    let mut st = outstanding(9);
    ping::on_pong(&p, &mut st, &9u64.to_le_bytes(), 1_500);
    assert_eq!((p.last_ping_us(), p.min_ping_us()), (500, 500));
    let mut st2 = outstanding(9);
    ping::on_pong(&p, &mut st2, &9u64.to_le_bytes(), 1_900);
    assert_eq!((p.last_ping_us(), p.min_ping_us()), (900, 500));
    assert_eq!(st.nonce_sent, 0);
    // Timing mishap: finished, not recorded.
    let mut st = outstanding(9);
    ping::on_pong(&p, &mut st, &9u64.to_le_bytes(), 999);
    assert_eq!(st.nonce_sent, 0);
    assert_eq!(p.last_ping_us(), 900);
    assert_eq!(st.last_us, None);
}

// --- Address intake ---------------------------------------------------------------------------

fn v1_entry(ip: [u8; 4], services: ServiceFlags, time: u32) -> TimestampedAddr {
    let s = Service::new(NetAddr::ipv4(ip), 9999);
    TimestampedAddr {
        time,
        services,
        addr: ServiceV1 {
            addr: AddrV1(s.addr.v1_bytes()),
            port: 9999,
        },
    }
}

fn addr_msg(entries: Vec<TimestampedAddr>) -> Vec<u8> {
    let mut buf = Vec::new();
    Addr {
        addrs: entries,
    }
    .encode(&mut buf);
    buf
}

/// `n` distinct routable IPv4 entries with `NODE_NETWORK`, dated now.
fn fresh(n: usize) -> Vec<TimestampedAddr> {
    (0..n)
        .map(|i| v1_entry([20, 1, (i >> 8) as u8, i as u8], ServiceFlags::NODE_NETWORK, NOW as u32))
        .collect()
}

/// `SetupAddressRelay`: never for block-relay-only peers; otherwise once, allocating
/// `m_addr_known`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_address_relay() {
    let core = new_core(|_| {});
    let (block, _t) = peer(&core, ConnectionType::BlockRelay, Permissions::NONE);
    let mut r = AddrRelay::new(NOW_US);
    assert!(!addr::setup_address_relay(&core, &block, &mut r));
    assert!(!r.enabled && r.known.is_none());
    let (inbound, _t) = peer(&core, ConnectionType::Inbound, Permissions::NONE);
    assert!(addr::setup_address_relay(&core, &inbound, &mut r));
    assert!(r.enabled);
    r.known.as_mut().unwrap().insert(b"key");
    assert!(addr::setup_address_relay(&core, &inbound, &mut r));
    assert!(r.known.as_ref().unwrap().contains(b"key"), "allocated once");
    // A new peer's state: one token, stamped at creation.
    let r = AddrRelay::new(NOW_US);
    assert_eq!((r.token_bucket, r.token_timestamp_us), (1.0, NOW_US));
}

/// More than 1000 entries: +20 `"%s message size = %u"`, after `SetupAddressRelay` (an inbound
/// peer's relay is enabled); a block-relay-only peer's is ignored unscored. A truncated vector
/// throws before either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn addr_size_limit() {
    let core = new_core(|_| {});
    let (inbound, _t) = peer(&core, ConnectionType::Inbound, Permissions::NONE);
    let mut r = AddrRelay::new(NOW_US);
    let big = addr_msg(fresh(1001));
    let out = addr::on_addr(&core, &inbound, &mut r, &big, false, NOW_US);
    assert_eq!(out.step, Some(Step::Done));
    assert_eq!(out.misbehaving, Some((20, "addr message size = 1001".to_owned())));
    assert!(r.enabled);
    assert_eq!(r.token_bucket, 1.0, "returns before the bucket");

    let mut buf = Vec::new();
    AddrV2Msg {
        addrs: (0..1001u32)
            .map(|i| AddrV2Entry {
                time: NOW as u32,
                services: ServiceFlags::NODE_NETWORK,
                addr: AddrV2::Ipv4(i.to_be_bytes()),
                port: 1,
            })
            .collect(),
    }
    .encode(&mut buf);
    let out = addr::on_addr(&core, &inbound, &mut r, &buf, true, NOW_US);
    assert_eq!(out.misbehaving, Some((20, "addrv2 message size = 1001".to_owned())));

    let (block, _t) = peer(&core, ConnectionType::BlockRelay, Permissions::NONE);
    let mut r = AddrRelay::new(NOW_US);
    let out = addr::on_addr(&core, &block, &mut r, &big, false, NOW_US);
    assert_eq!(out.step, Some(Step::Ignored));
    assert_eq!(out.misbehaving, None);
    assert!(!r.enabled);

    let mut r = AddrRelay::new(NOW_US);
    let out = addr::on_addr(&core, &inbound, &mut r, &big[..100], false, NOW_US);
    match out.step {
        Some(Step::Throw(t)) => {
            assert_eq!(t.what, "DataStream::read(): end of data: iostream error")
        }
        other => panic!("{other:?}"),
    }
    assert!(!r.enabled, "the read comes first");
}

/// The token bucket (`p2p_addr_relay.py`'s rate-limiting test): a new peer has one token; it
/// refills at 0.1 per second up to 1000; a peer without the `addr` permission has the rest of a
/// message rate-limited; with the permission nothing is, and the bucket is not decremented
/// below one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn addr_token_bucket() {
    let core = new_core(|_| {});
    let (p, _t) = peer(&core, ConnectionType::Inbound, Permissions::NONE);
    let mut r = AddrRelay::new(NOW_US);
    let out = addr::on_addr(&core, &p, &mut r, &addr_msg(fresh(5)), false, NOW_US);
    assert_eq!(out.step, Some(Step::Done));
    assert_eq!((r.processed, r.rate_limited), (1, 4));
    assert_eq!(r.token_bucket, 0.0);
    // 10 seconds later: one more token.
    let out = addr::on_addr(&core, &p, &mut r, &addr_msg(fresh(5)), false, NOW_US + 10_000_000);
    assert_eq!(out.step, Some(Step::Done));
    assert_eq!((r.processed, r.rate_limited), (2, 8));
    assert_eq!(r.token_timestamp_us, NOW_US + 10_000_000);
    // A clock going backwards adds nothing but restamps.
    addr::on_addr(&core, &p, &mut r, &addr_msg(fresh(1)), false, NOW_US);
    assert_eq!((r.processed, r.rate_limited), (2, 9));
    assert_eq!(r.token_timestamp_us, NOW_US);
    // The bucket is capped at 1000.
    addr::on_addr(&core, &p, &mut r, &addr_msg(fresh(1)), false, NOW_US + 1_000_000_000_000);
    assert_eq!(r.token_bucket, 999.0);

    let (whitelisted, _t) = peer(&core, ConnectionType::Inbound, Permissions::ADDR);
    let mut r = AddrRelay::new(NOW_US);
    addr::on_addr(&core, &whitelisted, &mut r, &addr_msg(fresh(5)), false, NOW_US);
    assert_eq!((r.processed, r.rate_limited), (5, 0));
    assert_eq!(r.token_bucket, 0.0);
    assert!(core.addrman().size(None, None) > 0);
}

/// Per entry: a token is taken before the services filter; entries without `NODE_NETWORK`
/// or `NODE_NETWORK_LIMITED` are dropped; banned and discouraged ones are remembered
/// (`m_addr_known`) but not processed; times at or below 100000000 or more than 10 minutes ahead
/// become now − 5 days, so they are not relayed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn addr_entry_rules() {
    let core = new_core(|_| {});
    let (p, _t) = peer(&core, ConnectionType::Inbound, Permissions::ADDR);
    let mut r = AddrRelay::new(NOW_US);
    r.token_bucket = 10.0;
    let discouraged = [30, 0, 0, 1];
    core.banlist().discourage(&NetAddr::ipv4(discouraged));
    let entries = vec![
        v1_entry([30, 0, 0, 2], ServiceFlags::NODE_BLOOM, NOW as u32),
        v1_entry(discouraged, ServiceFlags::NODE_NETWORK, NOW as u32),
        v1_entry([30, 0, 0, 3], ServiceFlags::NODE_NETWORK_LIMITED, 100_000_000),
        v1_entry([30, 0, 0, 4], ServiceFlags::NODE_NETWORK, (NOW + 601) as u32),
        v1_entry([30, 0, 0, 5], ServiceFlags::NODE_NETWORK, (NOW + 600) as u32),
        v1_entry([30, 0, 0, 6], ServiceFlags::NODE_NETWORK, (NOW - 599) as u32),
    ];
    let out = addr::on_addr(&core, &p, &mut r, &addr_msg(entries), false, NOW_US);
    assert_eq!(r.token_bucket, 4.0, "a token per entry, filtered or not");
    assert_eq!(r.processed, 4);
    let known = r.known.as_ref().unwrap();
    let key = |ip| Service::new(NetAddr::ipv4(ip), 9999).key();
    assert!(known.contains(&key(discouraged)));
    assert!(!known.contains(&key([30, 0, 0, 2])), "filtered before AddAddressKnown");
    let mut relayed: Vec<_> =
        out.relay.iter().map(|(a, reach)| (a.service.clone(), a.time, *reach)).collect();
    relayed.sort();
    assert_eq!(
        relayed,
        vec![
            (Service::new(NetAddr::ipv4([30, 0, 0, 5]), 9999), (NOW + 600) as u32, true),
            (Service::new(NetAddr::ipv4([30, 0, 0, 6]), 9999), (NOW - 599) as u32, true),
        ]
    );
}

/// The relay choice: only messages of at most 10 entries, never in reply to our `getaddr`
/// (which clears `m_getaddr_sent` for a reply under 1000), only routable addresses, with
/// `reachable` from `-onlynet`; unreachable addresses are relayed (to one peer) but not stored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn addr_relay_choice() {
    let core = new_core(|cfg| cfg.only_net = vec![Network::Ipv4]);
    let (p, _t) = peer(&core, ConnectionType::Inbound, Permissions::ADDR);
    let mut r = AddrRelay::new(NOW_US);
    let out = addr::on_addr(&core, &p, &mut r, &addr_msg(fresh(10)), false, NOW_US);
    assert_eq!(out.relay.len(), 10);
    assert!(out.relay.iter().all(|(_, reach)| *reach));
    let out = addr::on_addr(&core, &p, &mut r, &addr_msg(fresh(11)), false, NOW_US);
    assert!(out.relay.is_empty());

    r.getaddr_sent = true;
    let out = addr::on_addr(&core, &p, &mut r, &addr_msg(fresh(3)), false, NOW_US);
    assert!(out.relay.is_empty());
    assert!(!r.getaddr_sent, "a reply under 1000 entries ends the getaddr window");

    let mut buf = Vec::new();
    AddrV2Msg {
        addrs: vec![
            AddrV2Entry {
                time: NOW as u32,
                services: ServiceFlags::NODE_NETWORK,
                addr: AddrV2::Ipv6([0x20, 0x01, 0x0d, 0xb9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]),
                port: 1,
            },
            AddrV2Entry {
                time: NOW as u32,
                services: ServiceFlags::NODE_NETWORK,
                addr: AddrV2::Ipv4([10, 0, 0, 1]),
                port: 1,
            },
        ],
    }
    .encode(&mut buf);
    let before = core.addrman().size(None, None);
    let out = addr::on_addr(&core, &p, &mut r, &buf, true, NOW_US);
    assert_eq!(out.relay.len(), 1, "10.0.0.1 is not routable");
    assert!(!out.relay[0].1, "IPv6 is not reachable under -onlynet=ipv4");
    assert_eq!(core.addrman().size(None, None), before, "neither is stored");
}

/// `ADDR_FETCH`: an `addr` with more than one entry completes the connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn addr_fetch_disconnect() {
    let core = new_core(|_| {});
    let (p, _t) = peer(&core, ConnectionType::AddrFetch, Permissions::NONE);
    let mut r = AddrRelay::new(NOW_US);
    let out = addr::on_addr(&core, &p, &mut r, &addr_msg(fresh(1)), false, NOW_US);
    assert_eq!(out.step, Some(Step::Done));
    assert!(!p.disconnect_requested(), "a self-announcement");
    let out = addr::on_addr(&core, &p, &mut r, &addr_msg(fresh(2)), false, NOW_US);
    assert_eq!(out.step, Some(Step::Disconnect));
    assert!(p.disconnect_requested());
}
