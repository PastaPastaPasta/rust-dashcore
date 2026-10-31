//! The address relay's single-command `diff_p2p_*` targets (P2a §6.2's table; P2a-net): up to
//! four peers through their handshakes, then up to eight messages of the target's command, each
//! followed by a pass, run and compared by [`crate::p2p_node`] after every step. The node leg
//! runs P2a-net's `NodeAddressRelay` ([`node_address_relay`], P2a-net D24).
//!
//! Core's session cannot be given an AddrMan, relay state or permissions, so each state is
//! reached by the script (P2a-net D25):
//!
//! - Peers: two to four, each inbound, outbound full-relay, block-relay-only, ADDR_FETCH, manual
//!   or feeler, at protocol version 70242, 70223 (`ADDRV2_PROTO_VERSION`) or 70222, at an IPv4
//!   or IPv6 address (the reply cache's requestor network), sending `sendaddrv2` never, before
//!   `version`, between `version` and `verack` or after `verack`. Some peers handshake later in
//!   the script, so a fresh inbound peer can ask for addresses after a clock jump. Core must keep
//!   them: at most one peer is an outbound one Core downloads from, and an inbound peer offers
//!   blocks (one in eight) only without one, so Core's 15-min header-sync timeout ends no peer;
//!   outbound full-relay and block-relay-only peers announce the genesis block, Core's tip, so
//!   `ConsiderEviction` keeps them; each handshake message gets its own pass; and before the
//!   clock moves past a `ping`'s 20-min timeout, every peer answers its last `ping`. The peers
//!   Core ends on purpose (a feeler at `version`, a late `sendaddrv2` from 70223, an ADDR_FETCH
//!   peer at an address message of more than one entry or 300 s after the start) get nothing
//!   after; a 70222 peer's late `sendaddrv2` Core ignores, and the peer stays. [`script`] also
//!   returns which peers Core has ended after each pass, for the Core-only test to check.
//! - AddrMan: a seeding message of 10, 100 or 1000 (10 once [`MAX_LARGE`] is spent) fresh
//!   routable addresses from a live outbound full-relay or manual peer (whose token bucket the
//!   node's `getaddr` filled), else a live inbound one, chosen after its handshake; always for
//!   `diff_p2p_getaddr`, whose first choice is manual.
//! - Relay state: inbound peers turn relay on with their first `addr`, `addrv2` or `getaddr`;
//!   one address message in four is one Core relays (one to ten routable addresses stamped now,
//!   from an inbound peer, with another peer's relay on first); `getaddr` mostly comes from an
//!   inbound peer that has not asked yet (Core answers one per connection);
//!   the clock moves by none, seconds, tens of seconds, minutes or one of the jumps around the
//!   token bucket's refill, the relay destinations' 24 h rotation and the reply cache's
//!   21 h + [0, 6 h) expiry; a quarter of the messages are another address command, so `getaddr`
//!   flags, buckets and caches interleave with intake.
//!
//! Each address message holds 0–11, 999, 1000 or 1001 entries (at most [`MAX_LARGE`] above
//! [`SMALL`] per input), built from up to [`SMALL`] templates: every network the encoding has
//! (v1: routable and unroutable IPv4 and IPv6, IPv4-mapped, the TorV2 OnionCat and internal
//! prefixes, any 16 bytes; BIP155: IPv4, those IPv6 classes, TorV3, I2P, CJDNS, TorV2's id 3
//! (unknown at v24, so skipped) and other unknown ids (at 0–39 bytes, or one in eight at 511,
//! 512 or 513, either side of `MAX_ADDRV2_SIZE`); known ids at the right length, one byte off,
//! empty or above `MAX_ADDRV2_SIZE`), times now, stale, future, at or one past 100000000 or
//! any, services common or any (BIP155: also a non-canonical CompactSize), ports the network's,
//! 0 or any, and repeats. Now and then the payload is cut short.
//!
//! `diff_p2p_selfaddr` (P2a-net BL-203) runs `diff_p2p_addr`'s scripts out of IBD: both legs
//! take `-maxtipage` = [`SELFADDR_MAX_TIP_AGE`], so `IsInitialBlockDownload` latches to false at
//! its first call (validation.cpp:1989-1993) and `MaybeSendAddr` advertises our address
//! (net_processing.cpp:6203-6221). Each peer's `version` says what it sees us as (`addrMe`,
//! [`Builder::addr_me`]), which is all `GetLocalAddrForPeer` can advertise in a session with no
//! local addresses (net.cpp:245-281), and the clock jumps up to three days, past the 24 h
//! average of the self-announcement timer, so it fires again with the filter reset. The script
//! ends inside the tip-age window, so headers sync starts with every peer throughout
//! (net_processing.cpp:6381).

use std::sync::{Arc, OnceLock};

use dash_consensus::Header;
use dash_oracle_sys::{ConnType, NetAddr, Oracle, Pin, SendMode, Service, Substitution};
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dashd_p2p::processing::seams::{AddressRelay, ChainAccess};
use dashd_p2p::sim::PeerVersionMsg;

use crate::p2p::Script;
use crate::p2p_node::{PeerSpec, Session, Step, compare_session};
use crate::{Compared, Outcome};

/// Regtest's genesis block time, the session chain's tip.
pub const GENESIS_TIME: i64 = 1_417_713_337;
/// The script clock's start: regtest genesis time plus a day, as SB6's scripts.
pub const T0: i64 = GENESIS_TIME + 86_400;
/// `diff_p2p_selfaddr`'s `-maxtipage` on both legs: 30 days (P2a-net BL-203).
pub const SELFADDR_MAX_TIP_AGE: i64 = 30 * 86_400;
/// The longest clock jump in a `diff_p2p_selfaddr` script ([`Builder::advance`]).
const LIVE_MAX_JUMP: i64 = 3 * 86_400;
// Every `diff_p2p_selfaddr` script ends inside its tip-age window (D40): the clock moves only in
// `advance_by`, at most once per message and once for the final flush.
const _: () =
    assert!(T0 - GENESIS_TIME + MAX_MESSAGES as i64 * LIVE_MAX_JUMP + FLUSH < SELFADDR_MAX_TIP_AGE);
/// Target messages per input, at most.
const MAX_MESSAGES: usize = 8;
/// Peers per session, at most.
const MAX_PEERS: usize = 4;
/// Core's `MAX_ADDR_TO_SEND`: the most addresses an `addr`/`addrv2` may carry.
// Core: net_processing.cpp:213
const MAX_ADDR_TO_SEND: usize = 1000;
/// Core's `CNetAddr::MAX_ADDRV2_SIZE`.
// Core: netaddress.h:308
const MAX_ADDRV2_SIZE: usize = 512;
/// The most templates a message's entries are built from; a message of more entries repeats
/// them with distinct addresses.
const SMALL: usize = 16;
/// Messages of more than [`SMALL`] entries per input, seeding included, at most: a 1000-entry
/// message costs both legs a full intake.
const MAX_LARGE: u8 = 3;
/// The regtest port.
pub const PORT: u16 = 19899;
/// `NODE_NETWORK`.
const NODE_NETWORK: u64 = 1;

/// When a peer sends `sendaddrv2`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SendAddrV2 {
    Never,
    BeforeVersion,
    BeforeVerack,
    AfterVerack,
}

/// A script peer.
struct Peer {
    conn: ConnType,
    version: i32,
    sendaddrv2: SendAddrV2,
    /// Whether it offers blocks (`NODE_NETWORK`): every outbound peer but a feeler does
    /// ([`peers`]).
    serves_blocks: bool,
    /// Whether its handshake is in the script yet.
    connected: bool,
    /// Whether Core relays addresses to it (`m_addr_relay_enabled`): set for an outbound
    /// full-relay, manual or ADDR_FETCH peer by its `version`, for an inbound one by its first
    /// `addr`, `addrv2` or `getaddr`.
    relay: bool,
    /// Whether it sent `getaddr` (Core answers one per connection).
    asked: bool,
    /// Whether its `verack` is in the script (Core's `fSuccessfullyConnected`).
    verack: bool,
    /// Whether Core has ended it: a feeler at its `version`, a peer at a `sendaddrv2` after
    /// `verack` at protocol version [`ADDRV2_PROTO_VERSION`] or later, an ADDR_FETCH peer at an
    /// address message of more than one entry or at the first pass more than
    /// [`ADDR_FETCH_TIMEOUT`] after the session's start once connected.
    gone: bool,
}

impl Peer {
    /// Whether its handshake is in the script and Core keeps it.
    fn live(&self) -> bool {
        self.connected && !self.gone
    }
}

/// The script being built.
struct Builder<'a, 'b> {
    s: &'a mut Script<'b>,
    peers: Vec<Peer>,
    steps: Vec<Step>,
    now: i64,
    /// Messages of more than [`SMALL`] entries left ([`MAX_LARGE`]).
    large: u8,
    /// Addresses made so far: each new routable one is distinct.
    made: u32,
    /// The clock may move to here without a peer's `ping` timing out ([`Builder::advance`]).
    safe_until: i64,
    /// The last [`Builder::addr_payload`] fails Core's read (cut short or an entry Core cannot
    /// read), so it turns no relay on.
    unread: bool,
    /// After each pass, the peers Core has ended by then ([`Ends`]).
    ends: Ends,
    /// The last step is a pass whose ends are not recorded yet ([`Builder::settle`]).
    open: bool,
    /// `diff_p2p_selfaddr`: peers send an `addrMe`, and the clock jumps up to days.
    live: bool,
}

/// For each pass of a script (its step index), which peers Core has ended after it.
pub type Ends = Vec<(usize, Vec<bool>)>;

/// Core's `TIMEOUT_INTERVAL` less a minute: a peer that has not answered a `ping` sent this
/// long ago is not disconnected yet.
// Core: net.h TIMEOUT_INTERVAL (20 min); net_processing.cpp MaybeSendPing
const PING_SAFE: i64 = 19 * 60;

/// Core's ADDR_FETCH timeout, `10 * AVG_ADDRESS_BROADCAST_INTERVAL`, counted from the peer's
/// `m_connected`: the session adds every peer at [`T0`].
// Core: net_processing.cpp:6334 SendMessages
pub const ADDR_FETCH_TIMEOUT: i64 = 10 * 30;

/// Core's `ADDRV2_PROTO_VERSION`: below it, `sendaddrv2` is ignored, even after `verack`.
// Core: version.h:26; net_processing.cpp:4292-4304 checks it before fSuccessfullyConnected
pub const ADDRV2_PROTO_VERSION: i32 = 70_223;

/// The clock's last move, after the script's messages.
const FLUSH: i64 = 5 * 60;

/// An `inv` of the regtest genesis block (`MSG_BLOCK`).
#[allow(clippy::expect_used)]
fn genesis_inv() -> Vec<u8> {
    static INV: OnceLock<Vec<u8>> = OnceLock::new();
    INV.get_or_init(|| {
        let params =
            ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().expect("regtest");
        let mut inv = vec![1, 2, 0, 0, 0];
        inv.extend(Header::new(params.genesis.header).hash().as_bytes());
        inv
    })
    .clone()
}

fn deliver(peer: usize, command: &'static str, payload: Vec<u8>) -> Step {
    Step::Deliver {
        peer,
        command,
        payload,
        subs: Vec::new(),
    }
}

/// `n` as a CompactSize.
fn compact_size(out: &mut Vec<u8>, n: u64) {
    match n {
        0..0xfd => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend((n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend((n as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend(n.to_le_bytes());
        }
    }
}

/// `v4` as an IPv4-mapped IPv6 address.
fn ipv4_mapped(v4: &[u8]) -> [u8; 16] {
    let mut ip = [0u8; 16];
    ip[10..12].copy_from_slice(&[0xff, 0xff]);
    ip[12..].copy_from_slice(v4);
    ip
}

/// An address entry's network and bytes, in BIP155's terms: `(network id, address bytes)`.
type Raw = (u8, Vec<u8>);

/// One address entry before encoding.
#[derive(Clone)]
struct Entry {
    time: u32,
    services: u64,
    /// Services as a non-canonical CompactSize (BIP155 only: Core's read throws).
    services_noncanonical: bool,
    raw: Raw,
    port: u16,
}

impl Entry {
    /// A fresh entry stamped `time`, with `NODE_NETWORK` and the regtest port.
    /// Whether Core's BIP155 read of this entry throws.
    fn unreadable(&self) -> bool {
        let (id, bytes) = &self.raw;
        let right = match id {
            1 => Some(4),
            2 | 6 => Some(16),
            4 | 5 => Some(32),
            _ => None,
        };
        self.services_noncanonical
            || bytes.len() > MAX_ADDRV2_SIZE
            || right.is_some_and(|n| bytes.len() != n)
    }

    fn plain(time: u32, raw: Raw) -> Self {
        Entry {
            time,
            services: NODE_NETWORK,
            services_noncanonical: false,
            raw,
            port: PORT,
        }
    }

    /// The BIP155 encoding if `bip155`, else the v1 one.
    fn encode(&self, bip155: bool, out: &mut Vec<u8>) {
        if bip155 {
            self.encode_v2(out);
        } else {
            self.encode_v1(out);
        }
    }

    /// The v1 encoding (`CAddress` with time): 16 bytes of address, any network folded in.
    fn encode_v1(&self, out: &mut Vec<u8>) {
        out.extend(self.time.to_le_bytes());
        out.extend(self.services.to_le_bytes());
        let (id, bytes) = &self.raw;
        let mut ip = [0u8; 16];
        match (*id, bytes.len()) {
            (1, 4) => ip = ipv4_mapped(bytes),
            (_, 16) => ip.copy_from_slice(bytes),
            _ => {
                let n = bytes.len().min(16);
                ip[..n].copy_from_slice(&bytes[..n]);
            }
        }
        out.extend(ip);
        out.extend(self.port.to_be_bytes());
    }

    /// The BIP155 encoding.
    fn encode_v2(&self, out: &mut Vec<u8>) {
        out.extend(self.time.to_le_bytes());
        if self.services_noncanonical {
            // 0xfd and two bytes below 0xfd: Core's ReadCompactSize refuses it.
            out.push(0xfd);
            out.extend((self.services as u16 % 0xfd).to_le_bytes());
        } else {
            compact_size(out, self.services);
        }
        out.push(self.raw.0);
        compact_size(out, self.raw.1.len() as u64);
        let start = out.len();
        out.extend(&self.raw.1);
        // base-sdk decodes a BIP155 IPv6 address in `fc00::/8` as CJDNS, which Core keeps as
        // unroutable IPv6 (DDE-SWEEP §5, BS10's fix); until BS10 lands, such an address goes out
        // in `fd00::/8`, as unroutable to Core and IPv6 to both (P2a-net D30).
        if self.raw.0 == 2 && out.get(start) == Some(&0xfc) {
            out[start] = 0xfd;
        }
        out.extend(self.port.to_be_bytes());
    }
}

impl<'a, 'b> Builder<'a, 'b> {
    /// A script at [`T0`] with no step yet.
    fn new(s: &'a mut Script<'b>, peers: Vec<Peer>, live: bool) -> Self {
        Builder {
            s,
            peers,
            steps: Vec::new(),
            now: T0,
            large: MAX_LARGE,
            made: 0,
            // No peer has been sent a `ping` yet.
            safe_until: i64::MAX,
            unread: false,
            ends: Vec::new(),
            open: false,
            live,
        }
    }

    /// Records the ends of the last pass, if not yet: the builder marks the peers a pass ends
    /// after pushing it, so they are settled before the next step, or a clock move, or the end.
    fn settle(&mut self) {
        if std::mem::take(&mut self.open) {
            self.ends.push((self.steps.len() - 1, self.peers.iter().map(|p| p.gone).collect()));
        }
    }

    /// A fresh routable IPv4 address, each in its own /16 (so AddrMan keeps many).
    fn fresh_ipv4(&mut self) -> [u8; 4] {
        self.made += 1;
        let n = self.made;
        // 20.0.0.0 to 98.255.0.0: public, outside every RFC range Core treats as unroutable
        // (100.64/10 is above it); distinct for the 20,224 a script can make.
        [20 + ((n >> 8) % 79) as u8, n as u8, (n >> 16) as u8, 1]
    }

    /// What peer `i`'s `version` says our address is (`addr_recv`, Core's `addrMe`): `::` port 0
    /// as the other targets send, a fresh routable IPv4 address at our port or at a scripted one
    /// (ours too, for one byte pair), a fresh routable IPv6 one, unroutable IPv4 or IPv6, or the
    /// peer's own address. Core advertises it only if it and the peer's address are routable
    /// (net.cpp:245-250); an outbound peer gets its IP at our listening port, an inbound one gets
    /// it whole (net.cpp:262-272).
    fn addr_me(&mut self, i: usize) -> [u8; 18] {
        let b = self.s.u8();
        let mut out = [0; 18];
        let mut port = PORT;
        match b & 7 {
            0 => return out,
            1 | 2 => out[..16].copy_from_slice(&ipv4_mapped(&self.fresh_ipv4())),
            3 => {
                out[..16].copy_from_slice(&ipv4_mapped(&self.fresh_ipv4()));
                port = u16::from(self.s.u8()) << 8 | u16::from(b);
            }
            4 => {
                self.made += 1;
                let n = self.made.to_be_bytes();
                out[..16].copy_from_slice(&[
                    0x2a, 0x03, 0, 0, n[0], n[1], n[2], n[3], 0, 0, 0, 0, 0, 0, 0, 1,
                ]);
            }
            5 => {
                let ip = [[10, 0, 0, 1], [127, 0, 0, 1], [192, 168, 1, 1], [0, 0, 0, 0]];
                out[..16].copy_from_slice(&ipv4_mapped(&ip[usize::from(b >> 3) % 4]));
            }
            6 => {
                // Loopback, and `fd00::/8` (unique local, as D30's remap).
                out[15] = 1;
                if b & 8 != 0 {
                    out[0] = 0xfd;
                }
            }
            _ => {
                let own = peer_addr(i);
                match own.addr.bytes.as_slice() {
                    v4 @ [_, _, _, _] => out[..16].copy_from_slice(&ipv4_mapped(v4)),
                    v6 => out[..16].copy_from_slice(v6),
                }
                port = own.port;
            }
        }
        out[16..].copy_from_slice(&port.to_be_bytes());
        out
    }

    /// A v1-encodable address of class `class`.
    fn v1_addr(&mut self, class: u8) -> Raw {
        let b = self.s.u8();
        match class & 7 {
            0 => (1, self.fresh_ipv4().to_vec()),
            // Unroutable IPv4: private, loopback, unspecified, broadcast.
            1 => {
                let v4: [u8; 4] =
                    [[10, b, 0, 1], [127, 0, 0, 1], [192, 168, b, 1], [0; 4], [255; 4]]
                        [usize::from(b) % 5];
                (1, v4.to_vec())
            }
            // Routable IPv6.
            2 => {
                self.made += 1;
                let mut ip = [0x2a, 0x01, 0x04, 0xf8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
                ip[4..8].copy_from_slice(&self.made.to_be_bytes());
                (2, ip.to_vec())
            }
            // Unroutable IPv6: loopback, link-local, documentation, unique local.
            3 => {
                let prefix: [u8; 2] =
                    [[0, 0], [0xfe, 0x80], [0x20, 0x01], [0xfc, 0]][usize::from(b) % 4];
                let mut ip = [0u8; 16];
                ip[..2].copy_from_slice(&prefix);
                if prefix == [0x20, 0x01] {
                    ip[2..4].copy_from_slice(&[0x0d, 0xb8]);
                }
                ip[15] = 1;
                (2, ip.to_vec())
            }
            // TorV2's OnionCat prefix (no longer a network at v24) and Core's internal prefix.
            4 => {
                let prefix: [u8; 6] = if b & 1 == 0 {
                    [0xfd, 0x87, 0xd8, 0x7e, 0xeb, 0x43]
                } else {
                    [0xfd, 0x6b, 0x88, 0xc0, 0x87, 0x24]
                };
                let mut ip = [b; 16];
                ip[..6].copy_from_slice(&prefix);
                (2, ip.to_vec())
            }
            // IPv4-mapped IPv6 (decodes as IPv4).
            5 => (2, ipv4_mapped(&self.fresh_ipv4()).to_vec()),
            // Any 16 bytes.
            _ => (2, (0..16).map(|_| self.s.u8()).collect()),
        }
    }

    /// A BIP155 address of class `class`.
    fn v2_addr(&mut self, class: u8) -> Raw {
        let b = self.s.u8();
        let (id, len) = match class & 7 {
            0 => return (1, self.fresh_ipv4().to_vec()),
            // IPv6: routable, unroutable, the OnionCat and internal prefixes, IPv4-mapped.
            1 => return self.v1_addr(2 + b % 4),
            // TorV2's id, which v24 no longer knows: skipped at any length up to 512.
            2 => (3, 10),
            3 => (4, 32),
            4 => (5, 32),
            5 => (6, 16),
            // An unknown network id, which Core skips at a length up to `MAX_ADDRV2_SIZE` and
            // throws on past it: one in eight at 511, 512 or 513 bytes.
            6 => {
                let c = self.s.u8();
                let len = if c & 7 == 0 {
                    MAX_ADDRV2_SIZE - 1 + usize::from(c >> 3) % 3
                } else {
                    usize::from(c % 40)
                };
                (7 + b % 249, len)
            }
            // A known id at a wrong length, empty, or too long.
            _ => {
                let id = 1 + b % 6;
                let right = [4, 16, 10, 32, 32, 16][usize::from(id - 1)];
                let len = match self.s.u8() % 4 {
                    0 => right - 1,
                    1 => right + 1,
                    2 => 0,
                    _ => MAX_ADDRV2_SIZE + 1,
                };
                (id, len)
            }
        };
        let seed = self.s.u8();
        let mut bytes: Vec<u8> = (0..len).map(|i| seed.wrapping_add(i as u8)).collect();
        if id == 6 && len == 16 && b & 1 == 0 {
            // CJDNS addresses start with 0xfc; the rest are invalid.
            bytes[0] = 0xfc;
        }
        (id, bytes)
    }

    /// One entry's template.
    fn entry(&mut self, bip155: bool) -> Entry {
        let b = self.s.u8();
        let raw = if bip155 {
            self.v2_addr(b)
        } else {
            self.v1_addr(b)
        };
        let now = self.now as u32;
        let time = match b >> 3 & 7 {
            0..=2 => now.wrapping_sub(u32::from(self.s.u8())),
            // Stale: Core relays only addresses at most 10 min old.
            3 => now.wrapping_sub(601 + 60 * u32::from(self.s.u16())),
            // Future: more than 10 min ahead, which Core resets to 5 days ago.
            4 => now + 601 + u32::from(self.s.u8()),
            // At Core's `nTime <= 100000000` cut.
            5 => 100_000_000 + u32::from(self.s.u8() & 1),
            _ => u32::from(self.s.u16()) << 16 | u32::from(self.s.u16()),
        };
        let c = self.s.u8();
        let services = match c & 3 {
            0 | 1 => NODE_NETWORK,
            2 => NODE_NETWORK | 0x400,
            _ => self.s.u64() >> (c >> 2 & 63),
        };
        let port = match c >> 4 & 3 {
            0 | 1 => PORT,
            2 => 0,
            _ => self.s.u16(),
        };
        Entry {
            time,
            services,
            services_noncanonical: bip155 && c >> 6 == 3 && self.s.u8() & 7 == 0,
            raw,
            port,
        }
    }

    /// The entry count of the next message.
    fn count(&mut self) -> usize {
        let b = self.s.u8();
        let count = match b & 7 {
            0..=3 => usize::from(b >> 3) % 12,
            4 => MAX_ADDR_TO_SEND - 1,
            5 => MAX_ADDR_TO_SEND,
            6 => MAX_ADDR_TO_SEND + 1,
            _ => 1 + usize::from(b >> 3) % 4,
        };
        self.spend_large(count, count % SMALL)
    }

    /// `count`, or `fallback` once the messages of more than [`SMALL`] entries are spent.
    fn spend_large(&mut self, count: usize, fallback: usize) -> usize {
        if count <= SMALL {
            return count;
        }
        if self.large == 0 {
            return fallback;
        }
        self.large -= 1;
        count
    }

    /// An `addr` (or, `bip155`, `addrv2`) payload of `count` entries.
    fn addr_payload(&mut self, bip155: bool, count: usize) -> Vec<u8> {
        let templates = count.min(SMALL);
        let mut entries: Vec<Entry> = Vec::with_capacity(templates);
        for _ in 0..templates {
            // Now and then a repeat of an earlier entry.
            let repeat = !entries.is_empty() && self.s.u8() & 7 == 0;
            let e = if repeat {
                entries[usize::from(self.s.u8()) % entries.len()].clone()
            } else {
                self.entry(bip155)
            };
            entries.push(e);
        }
        let mut payload = Vec::with_capacity(9 + 30 * count);
        compact_size(&mut payload, count as u64);
        for i in 0..count {
            let mut e = entries[i % templates].clone();
            if i >= templates {
                // Repeats of routable IPv4 and IPv6 templates become fresh addresses of their
                // network; the rest repeat as they are.
                match &mut e.raw {
                    // [`Builder::fresh_ipv4`]'s range; the unroutable classes lie outside it.
                    (1, bytes) if bytes.len() == 4 && (20..99).contains(&bytes[0]) => {
                        let fresh = self.fresh_ipv4();
                        bytes.copy_from_slice(&fresh);
                    }
                    (2, bytes) if bytes.starts_with(&[0x2a, 0x01]) => {
                        let fresh = self.fresh_ipv4();
                        bytes[12..].copy_from_slice(&fresh);
                    }
                    _ => {}
                }
            }
            e.encode(bip155, &mut payload);
        }
        // Now and then cut short: Core's read throws and the message is dropped.
        let cut = self.s.u16();
        if cut & 0xf == 0 && payload.len() > 1 {
            payload.truncate(1 + usize::from(cut >> 4) % (payload.len() - 1));
            self.unread = true;
        }
        // Core reads the whole message before acting on it, and a BIP155 entry it cannot read
        // throws (netaddress.cpp:44-86): non-canonical services, an address above
        // `MAX_ADDRV2_SIZE`, or a known id at the wrong length (v24 skips TorV2's id 3).
        self.unread |= bip155 && entries.iter().any(Entry::unreadable);
        payload
    }

    /// A message Core relays: one to ten routable addresses stamped now, IPv4 or IPv6.
    fn relayable_payload(&mut self, bip155: bool) -> Vec<u8> {
        let count = 1 + usize::from(self.s.u8()) % 10;
        let mut payload = Vec::with_capacity(9 + 30 * count);
        compact_size(&mut payload, count as u64);
        for _ in 0..count {
            let class = self.s.u8() & 2;
            Entry::plain(self.now as u32, self.v1_addr(class)).encode(bip155, &mut payload);
        }
        payload
    }

    /// Seeding: fresh routable IPv4 addresses, stamped now, from peer `from`.
    fn seed_addrman(&mut self, from: usize) {
        let count = [10, 100, MAX_ADDR_TO_SEND][usize::from(self.s.u8() % 3)];
        let count = self.spend_large(count, 10);
        let mut payload = Vec::with_capacity(9 + 30 * count);
        compact_size(&mut payload, count as u64);
        for _ in 0..count {
            Entry::plain(self.now as u32, (1, self.fresh_ipv4().to_vec())).encode_v1(&mut payload);
        }
        self.send(from, "addr", payload);
    }

    /// Seeds AddrMan ([`Builder::seed_addrman`]) from a peer Core keeps: an outbound full-relay
    /// or manual one, whose token bucket the node's `getaddr` filled, or else an inbound one,
    /// whose bucket holds one token (the other kinds' `addr` Core ignores or ends the peer on).
    /// A waiting candidate handshakes first; one that handshake ends (a late `sendaddrv2`) gives
    /// way to the next. No candidate, no seeding.
    fn seed(&mut self) {
        let n = self.peers.len();
        let outbound = |p: &Peer| matches!(p.conn, ConnType::OutboundFullRelay | ConnType::Manual);
        let order: Vec<usize> = (0..n)
            .filter(|&i| outbound(&self.peers[i]))
            .chain((0..n).filter(|&i| self.peers[i].conn == ConnType::Inbound))
            .collect();
        for i in order {
            if self.peers[i].gone {
                continue;
            }
            if !self.peers[i].connected {
                self.handshake(i);
            }
            // Its handshake may end it.
            if !self.peers[i].gone {
                self.seed_addrman(i);
                return;
            }
        }
    }

    /// `peer` sends `command`, and the node processes it in a pass now.
    fn send(&mut self, peer: usize, command: &'static str, payload: Vec<u8>) {
        debug_assert!(!self.peers[peer].gone, "a delivery to a peer Core ended");
        self.settle();
        self.steps.push(deliver(peer, command, payload));
        // Before the pass: Core ends an ADDR_FETCH peer in the pass that processes its `verack`.
        if command == "verack" {
            self.peers[peer].verack = true;
        }
        self.pass(self.now);
        let unread = std::mem::take(&mut self.unread);
        let p = &mut self.peers[peer];
        if p.conn == ConnType::Inbound
            && (command == "getaddr" || matches!(command, "addr" | "addrv2") && !unread)
        {
            p.relay = true;
        }
        if command == "getaddr" {
            p.asked = true;
        }
    }

    /// A pass at `t`: its `SendMessages` ends every connected ADDR_FETCH peer past
    /// [`ADDR_FETCH_TIMEOUT`].
    fn pass(&mut self, t: i64) {
        self.settle();
        self.steps.push(Step::Pass(t));
        self.open = true;
        self.expire_addr_fetch(t);
    }

    /// Marks the ADDR_FETCH peers a pass at `t` ends.
    fn expire_addr_fetch(&mut self, t: i64) {
        if t - T0 > ADDR_FETCH_TIMEOUT {
            for p in self.peers.iter_mut().filter(|p| p.conn == ConnType::AddrFetch && p.verack) {
                p.gone = true;
            }
        }
    }

    /// Peer `i`'s handshake, now. Each message gets its own pass: a pass processes one message
    /// per peer, so a second one would wait for the next pass, after the next clock move.
    fn handshake(&mut self, i: usize) {
        let now = self.now;
        let p = &mut self.peers[i];
        p.connected = true;
        let (conn, sendaddrv2) = (p.conn, p.sendaddrv2);
        p.relay =
            matches!(conn, ConnType::OutboundFullRelay | ConnType::Manual | ConnType::AddrFetch);
        // NODE_BLOOM | NODE_HEADERS_COMPRESSED, and NODE_NETWORK for a peer that serves blocks.
        let services = 0x4 | 0x800 | u64::from(p.serves_blocks);
        let version = p.version;
        let addr_recv = if self.live {
            self.addr_me(i)
        } else {
            [0; 18]
        };
        let payload = PeerVersionMsg {
            version,
            services,
            nonce: 0x5eed_5eed_5eed_5eed + i as u64,
            addr_recv,
            ..PeerVersionMsg::v24(now)
        }
        .payload();
        if conn != ConnType::Inbound {
            // Every peer is added at the session's start, so the node's own `version` to an
            // outbound one went out at the first pass; this one is for a peer added first.
            self.pass(now);
        }
        if sendaddrv2 == SendAddrV2::BeforeVersion {
            self.send(i, "sendaddrv2", Vec::new());
        }
        self.send(i, "version", payload);
        if conn == ConnType::Feeler {
            self.peers[i].gone = true;
            return;
        }
        if sendaddrv2 == SendAddrV2::BeforeVerack {
            self.send(i, "sendaddrv2", Vec::new());
        }
        self.send(i, "verack", Vec::new());
        if self.peers[i].gone {
            // An ADDR_FETCH peer whose handshake comes past its timeout ends in this pass.
            return;
        }
        // Its first `ping` went out in that pass.
        self.safe_until = self.safe_until.min(now + PING_SAFE);
        if matches!(conn, ConnType::OutboundFullRelay | ConnType::BlockRelay) {
            // The genesis block, which is the node's tip: Core's `ConsiderEviction` drops an
            // outbound peer that has not shown the tip's work 22 min on.
            self.send(i, "inv", genesis_inv());
        }
        if sendaddrv2 == SendAddrV2::AfterVerack {
            self.send(i, "sendaddrv2", Vec::new());
            // Below `ADDRV2_PROTO_VERSION` Core ignores it and keeps the peer.
            self.peers[i].gone = self.peers[i].version >= ADDRV2_PROTO_VERSION;
        }
    }

    /// Moves the clock: by nothing, seconds, tens of seconds, minutes or a jump around the
    /// token bucket's refill (0.1/s), the relay rotation (24 h) and the reply cache (21 h,
    /// 27 h).
    ///
    /// A move past [`Builder::safe_until`] first has every connected peer answer the last `ping`
    /// it was sent (Core disconnects a peer 20 min after an unanswered one), so long jumps keep
    /// the peers; an answer to a `ping` already answered is only logged.
    fn advance(&mut self) {
        let b = self.s.u8();
        let delta = match b & 7 {
            0 => 0,
            1 | 2 => i64::from(b >> 3),
            3 => 10 * i64::from(self.s.u8()),
            4 => 60 * i64::from(self.s.u8()),
            _ if self.live => {
                // Around and past the self-announcement timer's 24 h average, at most
                // `LIVE_MAX_JUMP`, so the script ends inside the tip-age window.
                const H: i64 = 60 * 60;
                [24 * H, 27 * H, 2 * 24 * H, LIVE_MAX_JUMP, 21 * H, 36 * H, 10_000, 1]
                    [usize::from(b >> 3) % 8]
            }
            _ => {
                const H: i64 = 60 * 60;
                [21 * H - 1, 21 * H, 24 * H, 27 * H, 27 * H + 1, 10 * 60 + 1, 10_000, 1]
                    [usize::from(b >> 3) % 8]
            }
        };
        self.advance_by(delta);
    }

    /// Moves the clock by `delta`, answering pings first as [`Builder::advance`] describes.
    fn advance_by(&mut self, delta: i64) {
        self.settle();
        // Peers the pass at the new time ends are marked first, so they get no `pong`.
        self.expire_addr_fetch(self.now + delta);
        if self.now + delta > self.safe_until {
            for i in (0..self.peers.len()).filter(|&i| self.peers[i].live()) {
                self.steps.push(Step::Deliver {
                    peer: i,
                    command: "pong",
                    payload: vec![0; 8],
                    subs: vec![Substitution {
                        dst_offset: 0,
                        src_peer: i as i64,
                        src_command: "ping".into(),
                        src_offset: 0,
                        len: 8,
                    }],
                });
            }
            // At the moved clock: the pass processes the `pong` before `MaybeSendPing`, so the
            // next `ping` goes out at that time or later.
            self.steps.push(Step::Pass(self.now + delta));
            self.open = true;
            self.safe_until = self.now + delta + PING_SAFE;
        }
        self.now += delta;
    }
}

/// The peers, from a byte or two each.
///
/// Core keeps one sync peer and, past a 15-min header timeout, disconnects it whenever another
/// preferred-download peer (outbound and serving blocks) exists; every outbound peer must serve
/// blocks. So a session has at most one outbound full-relay, manual or block-relay-only peer
/// (later picks are inbound), one inbound peer in eight serves blocks, only in a session
/// without one, and a feeler serves none. Out of IBD (`live`, `diff_p2p_selfaddr`) the best
/// header is inside the tip-age window, so that timeout never applies (net_processing.cpp:6796,
/// 6822): one inbound peer in two serves blocks, with or without a preferred peer, and headers
/// sync starts with each of them (6381).
fn peers(s: &mut Script, getaddr: bool, live: bool) -> Vec<Peer> {
    // Two at least: a relay needs a peer to go to, and `getaddr` a requestor and a seeder.
    let n = (1 + usize::from(s.u8()) % MAX_PEERS).max(2);
    let mut preferred = false;
    let mut peers: Vec<Peer> = (0..n)
        .map(|i| {
            let b = s.u8();
            let mut conn = match b & 7 {
                // `getaddr` is answered only for inbound peers, and its first peer is one; its
                // second seeds AddrMan as a manual peer, which `ConsiderEviction` spares.
                _ if getaddr && i == 0 => ConnType::Inbound,
                _ if getaddr && i == 1 => ConnType::Manual,
                0 | 1 => ConnType::Inbound,
                2 | 3 => ConnType::OutboundFullRelay,
                4 => ConnType::BlockRelay,
                5 => ConnType::AddrFetch,
                6 => ConnType::Manual,
                _ => ConnType::Feeler,
            };
            if matches!(conn, ConnType::OutboundFullRelay | ConnType::Manual | ConnType::BlockRelay)
            {
                if preferred {
                    conn = ConnType::Inbound;
                }
                preferred = true;
            }
            // Late or early `sendaddrv2` ends (from 70223) or skews the peer, so each is one in
            // eight.
            let sendaddrv2 = match b >> 3 & 3 {
                0 => SendAddrV2::Never,
                1 | 2 => SendAddrV2::BeforeVerack,
                _ if s.u8() & 1 == 0 => SendAddrV2::AfterVerack,
                _ => SendAddrV2::BeforeVersion,
            };
            // A feeler that serves blocks would be a second preferred-download peer for the
            // header-sync timeout; Core ends it after `version` either way.
            let serves_blocks = match conn {
                ConnType::Inbound if live => s.u8() & 1 == 0,
                ConnType::Inbound => s.u8() & 7 == 0,
                ConnType::Feeler => false,
                _ => true,
            };
            Peer {
                conn,
                version: [70_242, 70_223, 70_222][usize::from(b >> 5) % 3],
                sendaddrv2,
                serves_blocks,
                connected: false,
                relay: false,
                asked: false,
                verack: false,
                gone: false,
            }
        })
        .collect();
    // With a preferred peer, an inbound one that serves blocks could become the sync peer
    // first and be dropped by the header-sync timeout.
    if preferred && !live {
        for p in peers.iter_mut().filter(|p| p.conn == ConnType::Inbound) {
            p.serves_blocks = false;
        }
    }
    peers
}

/// Peer `i`'s address: IPv4 or (every third peer) IPv6, so requestors differ in network.
fn peer_addr(i: usize) -> Service {
    if i % 3 == 2 {
        let mut ip = [0x2a, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        ip[15] = i as u8 + 1;
        Service::new(NetAddr::ipv6(ip), PORT)
    } else {
        Service::new(NetAddr::ipv4([1, 2, 3, i as u8 + 1]), PORT)
    }
}

/// The target's command.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Addr,
    AddrV2,
    GetAddr,
    /// `diff_p2p_addr` out of IBD, with `addrMe` and long clock jumps (BL-203).
    SelfAddr,
}

fn session(bytes: &[u8], target: Target) -> Option<(Session, Ends)> {
    let mut s = Script(bytes);
    if s.is_empty() {
        return None;
    }
    let live = target == Target::SelfAddr;
    // The self-announcement target speaks `diff_p2p_addr`'s vocabulary.
    let target = if live {
        Target::Addr
    } else {
        target
    };
    let getaddr = target == Target::GetAddr;
    let seed = s.u64();
    let peers = peers(&mut s, getaddr, live);
    let n = peers.len();
    let specs = (0..n)
        .map(|i| PeerSpec {
            conn: peers[i].conn,
            addr: peer_addr(i),
            send_mode: SendMode::Eager,
        })
        .collect();
    let mut b = Builder::new(&mut s, peers, live);
    // The first one to `n` peers handshake now (`diff_p2p_getaddr`: two); the rest wait in the
    // script.
    let now_peers = (1 + usize::from(b.s.u8()) % n).max(1 + usize::from(getaddr));
    for i in 0..now_peers {
        b.handshake(i);
    }
    if getaddr || b.s.u8() & 3 == 0 {
        b.seed();
    }
    for _ in 0..MAX_MESSAGES {
        if b.s.is_empty() {
            break;
        }
        b.advance();
        let a = b.s.u8();
        // A waiting peer handshakes first now and then.
        if a & 0xe0 == 0xe0
            && let Some(i) = (0..n).find(|&i| !b.peers[i].connected)
        {
            b.handshake(i);
            continue;
        }
        // Messages come from live peers only; with none, a waiting peer handshakes.
        let live: Vec<usize> = (0..n).filter(|&i| b.peers[i].live()).collect();
        if live.is_empty() {
            match (0..n).find(|&i| !b.peers[i].connected) {
                Some(i) => {
                    b.handshake(i);
                    continue;
                }
                None => break,
            }
        }
        let inbound: Vec<usize> =
            live.iter().copied().filter(|&i| b.peers[i].conn == ConnType::Inbound).collect();
        let peer = live[usize::from(a) % live.len()];
        // A quarter of the messages are another address command.
        let command = match (target, (a >> 3) & 3) {
            (Target::Addr, 3) | (Target::GetAddr, 1) => Target::AddrV2,
            (Target::AddrV2, 3) | (Target::GetAddr, 2) => Target::Addr,
            (Target::Addr | Target::AddrV2, 2) if a & 0x20 != 0 => Target::GetAddr,
            _ => target,
        };
        match command {
            Target::Addr | Target::AddrV2 => {
                let bip155 = command == Target::AddrV2;
                let cmd = if bip155 {
                    "addrv2"
                } else {
                    "addr"
                };
                // One in four is a message Core relays, from an inbound peer (which has a token
                // and no `getaddr` outstanding) to another peer that has relay on.
                if b.s.u8() & 3 == 0 && !inbound.is_empty() {
                    let from = inbound[usize::from(a >> 2) % inbound.len()];
                    let to = live.iter().copied().find(|&i| i != from && b.peers[i].relay);
                    // An inbound peer turns relay on with its `getaddr`.
                    if to.is_none()
                        && let Some(&other) = inbound.iter().find(|&&i| i != from)
                    {
                        b.send(other, "getaddr", Vec::new());
                    }
                    let payload = b.relayable_payload(bip155);
                    b.send(from, cmd, payload);
                } else {
                    let count = b.count();
                    let payload = b.addr_payload(bip155, count);
                    // Core ends an ADDR_FETCH peer at an address message of more than one entry
                    // it read, unless there are more than `MAX_ADDR_TO_SEND` (net_processing.cpp
                    // :4417 returns first; relay is already on).
                    let ends = b.peers[peer].conn == ConnType::AddrFetch
                        && (2..=MAX_ADDR_TO_SEND).contains(&count)
                        && !b.unread;
                    b.send(peer, cmd, payload);
                    b.peers[peer].gone |= ends;
                }
            }
            // `diff_p2p_getaddr` only: fresh addresses between replies, so a cached reply shows
            // what it hid.
            Target::GetAddr if getaddr && a >> 5 == 0 => b.seed(),
            Target::GetAddr | Target::SelfAddr => {
                // Core answers one `getaddr` per inbound connection: three in four requests
                // come from an inbound peer that has not asked yet, handshaking a waiting one
                // if none has, else from a live peer.
                let unasked = |b: &Builder, i: usize| {
                    b.peers[i].conn == ConnType::Inbound && !b.peers[i].asked
                };
                let mut from = peer;
                if a & 3 != 0 {
                    if let Some(&i) = inbound.iter().find(|&&i| unasked(&b, i)) {
                        from = i;
                    } else if let Some(i) =
                        (0..n).find(|&i| !b.peers[i].connected && unasked(&b, i))
                    {
                        // Its handshake may end it (a late `sendaddrv2`).
                        b.handshake(i);
                        if b.peers[i].live() {
                            from = i;
                        }
                    }
                }
                b.send(from, "getaddr", Vec::new());
            }
        }
    }
    // Long enough for relays still queued to go out (Core's average interval is 30 s).
    b.advance_by(FLUSH);
    b.pass(b.now);
    b.settle();
    let session = Session {
        seed,
        our_services: 1 | 4 | 1024,
        mocktime: T0,
        check_block_index: crate::p2p_node::check_block_index(),
        peers: specs,
        steps: b.steps,
        blocks: Vec::new(),
        max_tip_age: live.then_some(SELFADDR_MAX_TIP_AGE),
        addr_relay: Some(node_address_relay),
    };
    Some((session, b.ends))
}

/// The node's address relay, P2a-net's `NodeAddressRelay`, over the session leg's own chain, so
/// `MaybeSendAddr`'s `IsInitialBlockDownload()` reads the chain the sim validates on (P2a-net
/// D24). The session targets that relay addresses install it.
pub fn node_address_relay(chain: Arc<dyn ChainAccess>) -> Box<dyn AddressRelay> {
    Box::new(dashd_net::NodeAddressRelay::new(chain))
}

/// The session target `name` (`diff_p2p_addr`, `_addrv2`, `_getaddr` or `_selfaddr`) builds
/// from `bytes`, and the peers it expects Core to have ended after each pass, for tests and
/// triage; `None` for another name or out of domain.
pub fn script(name: &str, bytes: &[u8]) -> Option<(Session, Ends)> {
    let target = match name {
        "diff_p2p_addr" => Target::Addr,
        "diff_p2p_addrv2" => Target::AddrV2,
        "diff_p2p_getaddr" => Target::GetAddr,
        "diff_p2p_selfaddr" => Target::SelfAddr,
        _ => return None,
    };
    session(bytes, target)
}

fn compare(bytes: &[u8], pin: Pin, target: Target) -> Compared {
    match session(bytes, target) {
        None => Ok(Outcome::OutOfDomain),
        Some((s, _)) => compare_session(&Oracle::new(pin), &s),
    }
}

/// `diff_p2p_addr`: `addr` messages (and some `addrv2` and `getaddr`) from up to four peers.
pub fn compare_p2p_addr(bytes: &[u8], pin: Pin) -> Compared {
    compare(bytes, pin, Target::Addr)
}

/// `diff_p2p_addrv2`: `addrv2` messages (and some `addr` and `getaddr`) from up to four peers.
pub fn compare_p2p_addrv2(bytes: &[u8], pin: Pin) -> Compared {
    compare(bytes, pin, Target::AddrV2)
}

/// `diff_p2p_getaddr`: `getaddr` requests (and some `addr` and `addrv2`) over a seeded AddrMan,
/// across the reply cache's expiry.
pub fn compare_p2p_getaddr(bytes: &[u8], pin: Pin) -> Compared {
    compare(bytes, pin, Target::GetAddr)
}

/// `diff_p2p_selfaddr`: `diff_p2p_addr`'s scripts out of IBD, so the node advertises the address
/// each peer says it sees us at, and again after the timer fires (P2a-net BL-203).
pub fn compare_p2p_selfaddr(bytes: &[u8], pin: Pin) -> Compared {
    compare(bytes, pin, Target::SelfAddr)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    /// Every input decodes to a session whose steps name only scripted peers; the empty input
    /// is out of domain.
    #[test]
    fn scripts_decode() {
        assert!(session(&[], Target::Addr).is_none());
        let mut x = 7u64;
        for len in [1usize, 9, 40, 400, 4000] {
            for target in [Target::Addr, Target::AddrV2, Target::GetAddr, Target::SelfAddr] {
                let bytes: Vec<u8> = (0..len)
                    .map(|_| {
                        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                        (x >> 33) as u8
                    })
                    .collect();
                let (s, ends) = session(&bytes, target).expect("in domain");
                // One record per pass, in order, the last for the last step.
                let passes: Vec<usize> =
                    (0..s.steps.len()).filter(|&k| matches!(s.steps[k], Step::Pass(_))).collect();
                assert_eq!(ends.iter().map(|e| e.0).collect::<Vec<_>>(), passes);
                assert_eq!(passes.last(), Some(&(s.steps.len() - 1)));
                assert!((1..=MAX_PEERS).contains(&s.peers.len()));
                for step in &s.steps {
                    if let Step::Deliver {
                        peer,
                        ..
                    } = step
                    {
                        assert!(*peer < s.peers.len());
                    }
                }
                if target == Target::GetAddr {
                    assert_eq!(s.peers[0].conn, ConnType::Inbound);
                }
            }
        }
    }

    /// No script addresses a peer the generator expects Core to have ended: `Builder::send`'s
    /// debug assertion holds over random inputs (Opus r3-fix M1: a late ADDR_FETCH handshake
    /// past its timeout once went on after its ending `verack`).
    #[test]
    fn scripts_address_live_peers() {
        let mut x = 11u64;
        let mut next = || {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 33) as u8
        };
        for target in [Target::Addr, Target::AddrV2, Target::GetAddr, Target::SelfAddr] {
            for _ in 0..2000 {
                let len = usize::from(next()) * 4;
                let bytes: Vec<u8> = (0..len).map(|_| next()).collect();
                let _ = session(&bytes, target);
            }
        }
    }

    /// Unknown network ids come at 511, 512 and 513 bytes: both sides of `MAX_ADDRV2_SIZE`.
    #[test]
    fn unknown_ids_straddle_the_size_limit() {
        for (c, len) in [(0u8, 511), (8, 512), (16, 513), (1, 1)] {
            let bytes = [0, c, 0];
            let mut s = Script(&bytes);
            let mut b = Builder::new(&mut s, Vec::new(), false);
            let (id, raw) = b.v2_addr(6);
            assert!(id >= 7, "unknown id {id}");
            assert_eq!(raw.len(), len, "c {c}");
        }
    }

    /// No BIP155 IPv6 address goes out in `fc00::/8` (P2a-net D30); v1 keeps it, and CJDNS too.
    #[test]
    fn bip155_ipv6_avoids_the_cjdns_prefix() {
        let mut bytes = [0u8; 16];
        bytes[0] = 0xfc;
        let mut out = Vec::new();
        Entry::plain(0, (2, bytes.to_vec())).encode_v2(&mut out);
        assert_eq!(out[7], 0xfd);
        let mut out = Vec::new();
        Entry::plain(0, (6, bytes.to_vec())).encode_v2(&mut out);
        assert_eq!(out[7], 0xfc);
        let mut v1 = Vec::new();
        Entry::plain(0, (2, bytes.to_vec())).encode_v1(&mut v1);
        assert_eq!(v1[12], 0xfc);
    }

    /// The pinned base-sdk still reads BIP155 IPv6 in `fc00::/8` as CJDNS, which is why
    /// `encode_v2` remaps it (P2a-net D30, D32).
    #[test]
    fn base_sdk_still_reads_fc00_as_cjdns() {
        use dash_primitives::AddrV2;
        use dash_types::codec::BaseCodec;
        let mut raw = vec![2, 16, 0xfc];
        raw.extend([0; 15]);
        assert!(
            matches!(AddrV2::decode(&mut raw.as_slice()), Ok(AddrV2::Cjdns(_))),
            "BS10 landed: drop encode_v2's fc00 remap and this test, un-ignore \
             multi_relay_fc00_agrees and un-park diff_p2p_multi_relay (P2a-net D30, D32)"
        );
    }

    /// Core's BIP155 read throws on known ids at the wrong length, but not on TorV2's id 3 nor
    /// an unknown id up to `MAX_ADDRV2_SIZE` (netaddress.cpp:44-86).
    #[test]
    fn unreadable_entries() {
        let e = |id: u8, len: usize| Entry::plain(0, (id, vec![0; len])).unreadable();
        for (id, right) in [(1, 4), (2, 16), (4, 32), (5, 32), (6, 16)] {
            assert!(!e(id, right), "id {id}");
            for len in [right - 1, right + 1, 0] {
                assert!(e(id, len), "id {id} len {len}");
            }
        }
        assert!(!e(3, 11) && !e(3, 0) && !e(9, MAX_ADDRV2_SIZE));
        assert!(e(3, MAX_ADDRV2_SIZE + 1) && e(9, MAX_ADDRV2_SIZE + 1));
    }

    /// The v1 entry is Core's `CAddress` with time (30 bytes); BIP155's is time, CompactSize
    /// services, network id, CompactSize length, bytes and the big-endian port.
    #[test]
    fn entry_encodings() {
        let e = Entry {
            time: 0x0102_0304,
            services: 1,
            services_noncanonical: false,
            raw: (1, vec![20, 0, 1, 1]),
            port: PORT,
        };
        let mut v1 = Vec::new();
        e.encode_v1(&mut v1);
        assert_eq!(v1.len(), 30);
        assert_eq!(&v1[12..28], &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 20, 0, 1, 1]);
        assert_eq!(&v1[28..], &PORT.to_be_bytes());
        let mut v2 = Vec::new();
        e.encode_v2(&mut v2);
        assert_eq!(v2, [4, 3, 2, 1, 1, 1, 4, 20, 0, 1, 1, 0x4d, 0xbb]);
        let mut odd = Vec::new();
        Entry {
            services_noncanonical: true,
            ..e
        }
        .encode_v2(&mut odd);
        assert_eq!(&odd[4..7], &[0xfd, 1, 0]);
    }
}
