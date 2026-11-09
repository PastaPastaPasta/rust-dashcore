//! The address manager and multi-peer message handling against Core's stateful oracle entries
//! (O1c; P2a §6.2–6.3): `diff_addrman` and the `diff_p2p_multi_*` targets.
//!
//! An input is a script the oracle runs: AddrMan operations, or peers, messages and send passes
//! in a peer session, decoded leniently from fuzzer bytes (`Script`). Until SB6's AddrMan lands
//! here, `diff_addrman`'s second leg is Core again on the same seed: every output, draw log and
//! the final state must replay byte for byte, and Core's own consistency check
//! (`-checkaddrman`) runs on every AddrMan operation. P2a-core's [`crate::p2p_node`] runs a
//! session script in Core's peer session and on the node's processor, and compares the two after
//! every step.

use dash_oracle_sys::{
    AddrMan, AddrManConfig, AddrOp, Address, ConnType, ErrorKind, NetAddr, Oracle, Pin, SendMode,
    Service, Substitution,
};

use crate::p2p_node::{PeerSpec, Session, Step};
use crate::{Compared, Outcome, check, divergence};

/// Core's `asmap.raw` test map (`src/test/data/asmap.raw`), for scripts that bucket by AS.
const ASMAP: &[u8] = include_bytes!("../../dash-oracle-sys/tests/data/core-tests/asmap.raw");
/// The script clock's start: regtest genesis time plus a day.
const T0: i64 = 1_417_713_337 + 86_400;
/// Operations or session steps per input, at most.
const MAX_STEPS: usize = 64;

/// Fuzzer bytes read front to back; exhausted reads give zeros.
pub(crate) struct Script<'a>(pub(crate) &'a [u8]);

impl Script<'_> {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn u8(&mut self) -> u8 {
        let Some((&b, rest)) = self.0.split_first() else {
            return 0;
        };
        self.0 = rest;
        b
    }

    pub(crate) fn u16(&mut self) -> u16 {
        u16::from_le_bytes([self.u8(), self.u8()])
    }

    pub(crate) fn u64(&mut self) -> u64 {
        (0..8).fold(0, |n, i| n | u64::from(self.u8()) << (8 * i))
    }

    pub(crate) fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = usize::from(self.u8()).min(max).min(self.0.len());
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        head.to_vec()
    }

    /// One of 64 IPv4 addresses (a few /16 groups) or 8 IPv6 ones, so operations collide.
    fn net_addr(&mut self) -> NetAddr {
        let b = self.u8();
        if b & 0xc0 == 0xc0 {
            let mut ip = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            ip[4] = b & 7;
            ip[15] = 1;
            NetAddr::ipv6(ip)
        } else {
            NetAddr::ipv4([250, b & 3, (b >> 2) & 15, 1])
        }
    }

    fn service(&mut self) -> Service {
        let addr = self.net_addr();
        let port = if self.u8() & 1 == 0 {
            9999
        } else {
            19999
        };
        Service::new(addr, port)
    }

    fn network(&mut self) -> Option<u8> {
        match self.u8() % 4 {
            0 => None,
            1 => Some(dash_oracle_sys::addrman::NET_IPV4),
            2 => Some(dash_oracle_sys::addrman::NET_IPV6),
            _ => Some(dash_oracle_sys::addrman::NET_ONION),
        }
    }
}

/// `diff_addrman`'s configuration and operations, with each operation's mock time.
fn addrman_script(bytes: &[u8]) -> Option<(AddrManConfig, Vec<(AddrOp, i64)>)> {
    let mut s = Script(bytes);
    let flags = s.u8();
    let config = AddrManConfig {
        deterministic: flags & 1 == 0,
        rng_seed: s.u64(),
        asmap: if flags & 2 == 0 {
            Vec::new()
        } else {
            ASMAP.to_vec()
        },
        // -checkaddrman: Core checks its invariants every 1st to 3rd operation.
        check_ratio: i32::from(flags >> 2 & 3).max(1),
        ..AddrManConfig::default()
    };
    let mut ops = Vec::new();
    let mut now = T0;
    while !s.is_empty() && ops.len() < MAX_STEPS {
        now += i64::from(s.u16()) * 8;
        let op = match s.u8() % 12 {
            0 => AddrOp::Add {
                addrs: (0..1 + s.u8() % 8)
                    .map(|_| Address {
                        time: (now - i64::from(s.u16()) * 64).max(0) as u32,
                        services: u64::from(s.u8()),
                        service: s.service(),
                    })
                    .collect(),
                source: s.net_addr(),
                time_penalty: i64::from(s.u16()),
            },
            1 => AddrOp::Good(s.service()),
            2 => AddrOp::Attempt {
                addr: s.service(),
                count_failure: s.u8() & 1 == 1,
            },
            3 => AddrOp::Connected(s.service()),
            4 => AddrOp::SetServices {
                addr: s.service(),
                services: u64::from(s.u8()),
            },
            5 => AddrOp::ResolveCollisions,
            6 => AddrOp::Select {
                new_only: s.u8() & 1 == 1,
                network: s.network(),
            },
            7 => AddrOp::SelectTriedCollision,
            8 => AddrOp::GetAddr {
                max_addresses: u64::from(s.u8()),
                max_pct: u64::from(s.u8() % 101),
                network: s.network(),
                filtered: s.u8() & 1 == 1,
            },
            9 => AddrOp::Size {
                network: s.network(),
                in_new: match s.u8() % 3 {
                    0 => None,
                    b => Some(b == 1),
                },
            },
            10 => AddrOp::Dump,
            // A load of the last dump with one byte changed (or of fuzzer bytes).
            _ => AddrOp::Load(s.bytes(255)),
        };
        ops.push((op, now));
    }
    (!ops.is_empty()).then_some((config, ops))
}

/// Every raw output, then the final canonical state; `None` if the oracle refuses the input.
type Transcript = Option<Vec<Vec<u8>>>;

fn oracle_refusal(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::Argument | ErrorKind::Context)
}

/// The addrman script on Core.
fn core_addrman(
    oracle: &Oracle,
    config: &AddrManConfig,
    ops: &[(AddrOp, i64)],
) -> Result<Transcript, ()> {
    let mut am: AddrMan = match oracle.addrman_new(config) {
        Ok(Some(am)) => am,
        Ok(None) => return Ok(None),
        Err(e) if oracle_refusal(e.kind) => return Ok(None),
        Err(_) => return Err(()),
    };
    let mut out = Vec::with_capacity(ops.len() + 1);
    let mut last_dump: Option<Vec<u8>> = None;
    for (op, t) in ops {
        let op = match op {
            AddrOp::Load(edit) => AddrOp::Load(match (&last_dump, edit.as_slice()) {
                (Some(dump), [at, flip, ..]) => {
                    let mut file = dump.clone();
                    let at = usize::from(*at) * file.len() / 256;
                    if let Some(b) = file.get_mut(at) {
                        *b ^= flip;
                    }
                    file
                }
                (Some(dump), _) => dump.clone(),
                (None, edit) => edit.to_vec(),
            }),
            op => op.clone(),
        };
        let raw = am.op_raw(&op.encode(), *t).map_err(drop)?;
        if matches!(op, AddrOp::Dump) {
            // The result is list<u8> peers.dat ahead of the draw log.
            last_dump = dash_oracle_sys::addrman::dump_file(&raw);
        }
        out.push(raw);
    }
    out.push(am.canonical_raw().map_err(drop)?);
    Ok(Some(out))
}

/// The node's side of `diff_addrman`: Core again until SB6's AddrMan lands.
fn node_addrman(
    oracle: &Oracle,
    config: &AddrManConfig,
    ops: &[(AddrOp, i64)],
) -> Result<Transcript, ()> {
    core_addrman(oracle, config, ops)
}

/// `diff_addrman`: an AddrMan operation script. Compared: every operation's result and draw
/// log, then the canonical state.
pub fn compare_addrman(bytes: &[u8], pin: Pin) -> Compared {
    let Some((config, ops)) = addrman_script(bytes) else {
        return Ok(Outcome::OutOfDomain);
    };
    let oracle = Oracle::new(pin);
    let core = core_addrman(&oracle, &config, &ops).map_err(|()| divergence("oracle error"))?;
    let node = node_addrman(&oracle, &config, &ops).map_err(|()| divergence("node error"))?;
    compare_transcripts(core, node, ops.len())
}

fn compare_transcripts(core: Transcript, node: Transcript, steps: usize) -> Compared {
    match (core, node) {
        (None, None) => Ok(Outcome::OutOfDomain),
        (Some(core), Some(node)) => {
            check(core.len() == node.len(), "steps")?;
            for (i, (c, n)) in core.iter().zip(&node).enumerate() {
                check(
                    c == n,
                    if i < steps {
                        "step output"
                    } else {
                        "final state"
                    },
                )?;
            }
            Ok(Outcome::BothAccept)
        }
        _ => Err(divergence("refusal")),
    }
}

/// Which messages a `diff_p2p_multi_*` script draws from (P2a §6.2's split).
#[derive(Clone, Copy)]
enum Vocabulary {
    /// Phase C: version/verack, the feature negotiation and ping, across peers.
    Handshake,
    /// The joint E/F gate: header announcements, requests and the sync timeouts.
    Headers,
    /// The joint E/F gate: address relay.
    Relay,
}

impl Vocabulary {
    fn commands(self) -> &'static [&'static str] {
        match self {
            Vocabulary::Handshake => &[
                "version",
                "verack",
                "sendaddrv2",
                "sendheaders",
                "sendcmpct",
                "senddsq",
                "ping",
                "pong",
                "wtxidrelay",
                "feefilter",
                "qsendrecsigs",
                "mnauth",
            ],
            Vocabulary::Headers => &[
                "version",
                "verack",
                "headers",
                "headers2",
                "getheaders",
                "inv",
                "sendheaders",
                "ping",
                "pong",
            ],
            Vocabulary::Relay => {
                &["version", "verack", "addr", "addrv2", "getaddr", "sendaddrv2", "ping", "pong"]
            }
        }
    }
}

/// A `version` from a peer at Core's protocol version (or below it, from the script).
fn version_payload(s: &mut Script, now: i64) -> Vec<u8> {
    let version: i32 = if s.u8() & 7 == 0 {
        70_000
    } else {
        70_242
    };
    let mut p = Vec::new();
    p.extend(version.to_le_bytes());
    p.extend(u64::from(s.u8() & 0x0f | 1).to_le_bytes());
    p.extend(now.to_le_bytes());
    for _ in 0..2 {
        p.extend([0u8; 8]);
        p.extend([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 127, 0, 0, 1]);
        p.extend(19899u16.to_be_bytes());
    }
    p.extend(s.u64().to_le_bytes());
    p.push(0);
    p.extend(0i32.to_le_bytes());
    p.push(s.u8() & 1);
    p
}

fn session_script(bytes: &[u8], vocabulary: Vocabulary) -> Option<Session> {
    let mut s = Script(bytes);
    let seed = s.u64();
    let peers: Vec<PeerSpec> = (0..2 + s.u8() % 9)
        .map(|i| {
            let conn = match s.u8() % 4 {
                0 | 1 => ConnType::Inbound,
                2 => ConnType::OutboundFullRelay,
                _ => ConnType::BlockRelay,
            };
            PeerSpec {
                conn,
                addr: Service::new(NetAddr::ipv4([1, 2, 3, i]), 19899),
                send_mode: SendMode::Eager,
            }
        })
        .collect();
    let commands = vocabulary.commands();
    let mut steps = Vec::new();
    let mut now = T0;
    while !s.is_empty() && steps.len() < MAX_STEPS {
        let b = s.u8();
        if b & 3 == 0 {
            // Seconds up to about 18 h, so ping and chain-sync timeouts fire (a session runs no
            // stale-tip eviction or inactivity check: oracle.h).
            now += i64::from(s.u16()) * i64::from(b >> 2 & 15).max(1);
            steps.push(Step::Pass(now));
            continue;
        }
        let peer = usize::from(s.u8()) % peers.len();
        let command = commands[usize::from(b >> 2) % commands.len()];
        let payload = if command == "version" {
            version_payload(&mut s, now)
        } else {
            s.bytes(255)
        };
        // A pong echoes the nonce of our last ping to the peer when the script asks.
        let subs = if command == "pong" && payload.len() >= 8 && b & 0x80 != 0 {
            vec![Substitution {
                dst_offset: 0,
                src_peer: peer as i64,
                src_command: "ping".into(),
                src_offset: 0,
                len: 8,
            }]
        } else {
            Vec::new()
        };
        steps.push(Step::Deliver {
            peer,
            command,
            payload,
            subs,
        });
    }
    (!steps.is_empty()).then_some(Session {
        seed,
        our_services: 1 | 4 | 1024,
        mocktime: T0,
        check_block_index: crate::p2p_node::check_block_index(),
        peers,
        steps,
        blocks: Vec::new(),
        max_tip_age: None,
        addr_relay: Some(crate::p2p_addr::node_address_relay),
    })
}

fn compare_session(bytes: &[u8], pin: Pin, vocabulary: Vocabulary) -> Compared {
    let Some(script) = session_script(bytes, vocabulary) else {
        return Ok(Outcome::OutOfDomain);
    };
    crate::p2p_node::compare_session(&Oracle::new(pin), &script)
}

/// `diff_p2p_multi_handshake` (P2a phase C): 2–10 peers through version/verack, the feature
/// negotiation and ping. Compared: after every step, each peer's outcome (messages pushed,
/// Misbehaving lines, Peer and CNodeState) and the session's (draws, AddrMan and time-data
/// calls, PeerManager globals).
pub fn compare_p2p_multi_handshake(bytes: &[u8], pin: Pin) -> Compared {
    compare_session(bytes, pin, Vocabulary::Handshake)
}

/// `diff_p2p_multi_headers` (P2a joint E/F gate): header announcements and requests, sync-peer
/// selection and the sync timeouts, compared as `diff_p2p_multi_handshake`.
pub fn compare_p2p_multi_headers(bytes: &[u8], pin: Pin) -> Compared {
    compare_session(bytes, pin, Vocabulary::Headers)
}

/// `diff_p2p_multi_relay` (P2a joint E/F gate): address relay across peers, compared as
/// `diff_p2p_multi_handshake`. Its sessions, like every multi session, run P2a-net's relay
/// ([`crate::p2p_addr::node_address_relay`], P2a-net D29).
pub fn compare_p2p_multi_relay(bytes: &[u8], pin: Pin) -> Compared {
    compare_session(bytes, pin, Vocabulary::Relay)
}
