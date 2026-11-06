//! The single-command `diff_p2p_*` targets (P2a §6.2's table; P2a-core): one peer through the
//! handshake, then up to eight messages of the target's command, each followed by a pass, run
//! and compared by [`crate::p2p_node`] after every step.
//!
//! The input picks the peer first: its connection type, protocol version (70234, 70235 or
//! 70242), `NODE_NETWORK`, bit 11 (`NODE_HEADERS_COMPRESSED`) and `fRelay`. A peer without
//! `NODE_NETWORK` is never asked for blocks in IBD, so its header runs are compared past their
//! first connecting batch (P2a-core D44, D46). Then each message:
//!
//! - `headers`/`headers2`: a run of one chain of 8001 regtest headers on genesis that pass Core's
//!   checks, 10 s apart, usually from where the last run ended, of 1–16, 2000, 2001, 8000 or
//!   8001 headers (at most two runs of more than 16 within the peer's limit per input), with
//!   one header possibly broken (its prev hash, proof of work or time) and the payload possibly
//!   truncated. A quarter of the messages use the other command, so either command reaches
//!   either kind of peer (PQ-24's mixed cases).
//! - `inv`/`getdata`/`notfound`: up to eight entries of any type below 40 (34 included) naming a
//!   random hash, genesis or a chain header, repeated to 0–8, 115–117, 49,999–50,001 or any
//!   u16 count (at most two of 118–50,000 entries per input, D57). `getdata` never names
//!   genesis: serving it is P4's (D6).
//!
//! The empty input builds the chain and answers out of domain: libFuzzer runs it once, untimed,
//! before any unit when fuzzing or replaying a directory, so no unit's time there includes the
//! chain's 8001 X11 hashes (D56).

use std::sync::OnceLock;

use dash_consensus::Header;
use dash_consensus::header::{HeaderParams, check_block_header};
use dash_oracle_sys::{ConnType, NetAddr, Oracle, Pin, SendMode, Service};
use dash_p2p_core::{Headers, Headers2, ProtocolVersion};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{BlockHash, BlockHeader, MerkleRoot};
use dash_types::codec::BaseCodec;
use dashd_p2p::processing::headers::headers_limit;
use dashd_p2p::processing::logic::MAX_INV_SZ;
use dashd_p2p::sim::PeerVersionMsg;

use crate::p2p::Script;
use crate::p2p_node::{PeerSpec, Session, Step, addr_send_timer, compare_session};
use crate::{Compared, Outcome};

/// The script clock's start: regtest genesis time plus a day, as SB6's scripts.
const T0: i64 = 1_417_713_337 + 86_400;
/// Messages per input, at most.
const MAX_MESSAGES: usize = 8;
/// The chain's length: one more than a full `headers2` batch.
const CHAIN: usize = 8001;
/// Seconds between the chain's headers (8001 of them end before `T0`).
const SPACING: u32 = 10;
/// Runs of more than 16 headers within the peer's limit per input, at most: two reach the
/// continued sync after a full batch, and each one more costs a full batch for the same path
/// (D52). A run over the limit or cut short reaches no header processing, so it is not counted.
const MAX_LONG_RUNS: u8 = 2;
/// Inventories of more than [`SMALL_INVENTORY`] entries, up to `MAX_INV_SZ`, per input at most:
/// two reach a second large message while the first is still served or tracked, and each costs
/// 0.04–0.45 CPU-s per 50,000 entries (D57). The script's later ones become small. Larger counts
/// are not counted: Core scores them before any per-entry work, and five reach discouragement.
const MAX_LARGE_INVENTORIES: u8 = 2;
/// The largest small inventory, the top of the 115–117 class; the 49,999–50,001 class and most
/// u16 counts are larger (D57).
const SMALL_INVENTORY: usize = 117;

/// The chain's headers and hashes.
// Fixed regtest constants: a failure is a bug, not an input.
#[allow(clippy::expect_used)]
fn chain() -> &'static [(BlockHeader, BlockHash)] {
    static CHAIN_HEADERS: OnceLock<Vec<(BlockHeader, BlockHash)>> = OnceLock::new();
    CHAIN_HEADERS.get_or_init(|| {
        let p = params();
        let genesis = Header::new(dash_consensus::params::genesis_block(p).header);
        let (mut prev, mut time) = (genesis.hash(), genesis.time());
        (0..CHAIN as u32)
            .map(|i| {
                time += SPACING;
                let mut root = [0xa5; 32];
                root[..4].copy_from_slice(&i.to_le_bytes());
                let h = grind(
                    BlockHeader {
                        version: 0x2000_0000,
                        prev_hash: prev,
                        merkle_root: MerkleRoot::decode(&mut &root[..]).expect("32 bytes"),
                        time,
                        bits: genesis.bits(),
                        nonce: 0,
                    },
                    true,
                );
                prev = Header::new(h).hash();
                (h, prev)
            })
            .collect()
    })
}

#[allow(clippy::expect_used)]
fn params() -> &'static dash_params::ChainSetup {
    static PARAMS: OnceLock<dash_params::ChainSetup> = OnceLock::new();
    PARAMS.get_or_init(|| {
        ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest")
    })
}

/// `h` with the first nonce from its own on whose proof of work Core's check agrees with `pass`.
#[allow(clippy::expect_used)]
fn grind(mut h: BlockHeader, pass: bool) -> BlockHeader {
    static HP: OnceLock<HeaderParams> = OnceLock::new();
    let hp = HP.get_or_init(|| HeaderParams::from_chain_params(params()).expect("regtest pow"));
    while check_block_header(&Header::new(h), hp, true).is_ok() != pass {
        h.nonce = h.nonce.wrapping_add(1);
    }
    h
}

/// A 32-byte hash from eight script bytes, spread by splitmix64.
fn random_hash(s: &mut Script) -> [u8; 32] {
    let mut x = s.u64();
    let mut out = [0; 32];
    for word in out.chunks_mut(8) {
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        word.copy_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    out
}

/// The peer, its `version` and the protocol version in it.
fn peer(s: &mut Script) -> (PeerSpec, Vec<u8>, ProtocolVersion) {
    let b = s.u8();
    let conn = match b & 3 {
        0 => ConnType::Inbound,
        1 | 2 => ConnType::OutboundFullRelay,
        _ => ConnType::BlockRelay,
    };
    // NODE_BLOOM | NODE_NETWORK_LIMITED, then NODE_NETWORK and bit 11 by the script.
    let mut services = 0x4 | 0x400;
    if b & 0x10 != 0 {
        services |= 0x1;
    }
    if b & 0x20 != 0 {
        services |= 0x800;
    }
    let version = PeerVersionMsg {
        version: [70_242, 70_235, 70_234][usize::from(b >> 2 & 3) % 3],
        services,
        relay: b & 0x40 != 0,
        ..PeerVersionMsg::v24(T0)
    };
    let spec = PeerSpec {
        conn,
        addr: Service::new(NetAddr::ipv4([1, 2, 3, 4]), 19899),
        send_mode: SendMode::Eager,
    };
    (spec, version.payload(), ProtocolVersion(version.version))
}

/// The header runs of one input so far.
struct Runs {
    /// Where the last run ended.
    next: usize,
    /// Long runs left ([`MAX_LONG_RUNS`]).
    long: u8,
    /// The peer's version, the common one (the node's own is the newest).
    version: ProtocolVersion,
}

/// A `headers` (or, `compressed`, `headers2`) payload delivered at `now`.
fn headers(s: &mut Script, runs: &mut Runs, compressed: bool, now: i64) -> Vec<u8> {
    let chain = chain();
    let b = s.u8();
    let start = match b & 3 {
        0 | 1 => runs.next,
        2 => usize::from(s.u16()) % CHAIN,
        _ => 0,
    };
    let count = match usize::from(b >> 2 & 7) {
        k @ 4.. if runs.long > 0 => [2000, 2001, 8000, 8001][k - 4],
        _ => 1 + usize::from(s.u8() % 16),
    };
    let end = (start + count).min(CHAIN);
    let mut hs: Vec<BlockHeader> = chain[start.min(end)..end].iter().map(|(h, _)| *h).collect();
    runs.next = end;
    let at = usize::from(s.u16());
    if let Some(k) = (!hs.is_empty()).then(|| at % hs.len()) {
        match b >> 5 {
            // Discontinuous, or unconnecting at the first header.
            1 => hs[k].prev_hash = BlockHash::from(random_hash(s)),
            // Too little work, or a time more than 2 h past the pass that reads it; the rest
            // would not connect.
            2 | 3 => {
                if b >> 5 == 3 {
                    hs[k].time = (now + 2 * 60 * 60 + 1 + i64::from(s.u8())) as u32;
                }
                hs[k] = grind(hs[k], b >> 5 == 3);
                hs.truncate(k + 1);
            }
            _ => {}
        }
    }
    let n = hs.len();
    let mut payload = Vec::new();
    if compressed {
        Headers2::reply(&hs).encode(&mut payload);
    } else {
        Headers {
            headers: hs,
        }
        .encode(&mut payload);
    }
    let cut = s.u16();
    if cut & 0xf == 0 && !payload.is_empty() {
        payload.truncate(usize::from(cut >> 4) % payload.len());
    } else if (17..=headers_limit(runs.version, compressed)).contains(&n) {
        // Only a whole payload reaches header processing: a cut one throws in the read.
        runs.long -= 1;
    }
    payload
}

/// An inventory payload; `getdata` never names genesis. `large` counts the large ones left.
fn inventory(s: &mut Script, getdata: bool, large: &mut u8) -> Vec<u8> {
    let chain = chain();
    let b = s.u8();
    let template: Vec<(u32, [u8; 32])> = (0..1 + usize::from(b >> 5))
        .map(|_| {
            let kind = u32::from(s.u8() % 40);
            let hash = match s.u8() % 4 {
                0 if !getdata => {
                    *Header::new(dash_consensus::params::genesis_block(params()).header)
                        .hash()
                        .as_bytes()
                }
                1 => *chain[usize::from(s.u16()) % CHAIN].1.as_bytes(),
                _ => random_hash(s),
            };
            (kind, hash)
        })
        .collect();
    let mut count = match b & 7 {
        0..=3 => usize::from(s.u8() % 9),
        4 => 115 + usize::from(s.u8() % 3),
        5 => 49_999 + usize::from(s.u8() % 3),
        _ => usize::from(s.u16()),
    };
    if (SMALL_INVENTORY + 1..=MAX_INV_SZ).contains(&count) {
        if *large == 0 {
            count %= 9;
        } else {
            *large -= 1;
        }
    }
    let mut payload = Vec::with_capacity(5 + 36 * count);
    // CompactSize.
    match count {
        0..0xfd => payload.push(count as u8),
        _ => {
            payload.push(0xfd);
            payload.extend((count as u16).to_le_bytes());
        }
    }
    for i in 0..count {
        let (kind, mut hash) = template[i % template.len()];
        // Repeats differ in their last four bytes.
        let round = ((i / template.len()) as u32).to_le_bytes();
        hash[28..].iter_mut().zip(round).for_each(|(h, r)| *h ^= r);
        payload.extend(kind.to_le_bytes());
        payload.extend(hash);
    }
    payload
}

fn session(bytes: &[u8], command: &'static str) -> Option<Session> {
    let mut s = Script(bytes);
    if s.is_empty() {
        chain();
        return None;
    }
    let seed = s.u64();
    let (spec, version, protocol) = peer(&mut s);
    let mut steps = Vec::new();
    if spec.conn != ConnType::Inbound {
        steps.push(Step::Pass(T0));
    }
    for (cmd, payload) in [("version", version), ("verack", Vec::new())] {
        steps.push(Step::Deliver {
            peer: 0,
            command: cmd,
            payload,
            subs: Vec::new(),
        });
        steps.push(Step::Pass(T0));
    }
    let mut now = T0;
    let mut runs = Runs {
        next: 0,
        long: MAX_LONG_RUNS,
        version: protocol,
    };
    let mut large = MAX_LARGE_INVENTORIES;
    for _ in 0..MAX_MESSAGES {
        if s.is_empty() {
            break;
        }
        now += i64::from(s.u8());
        let (command, payload) = match command {
            "headers" | "headers2" => {
                // Now and then the other command, so both of PQ-24's mixed cases occur.
                let compressed = (command == "headers2") != (s.u8() & 0xc0 == 0xc0);
                let cmd = if compressed {
                    "headers2"
                } else {
                    "headers"
                };
                (cmd, headers(&mut s, &mut runs, compressed, now))
            }
            _ => (command, inventory(&mut s, command == "getdata", &mut large)),
        };
        steps.push(Step::Deliver {
            peer: 0,
            command,
            payload,
            subs: Vec::new(),
        });
        steps.push(Step::Pass(now));
    }
    Some(Session {
        seed,
        our_services: 1 | 4 | 1024,
        mocktime: T0,
        check_block_index: crate::p2p_node::check_block_index(),
        peers: vec![spec],
        steps,
        blocks: Vec::new(),
        max_tip_age: None,
        addr_relay: Some(addr_send_timer),
    })
}

fn compare(bytes: &[u8], pin: Pin, command: &'static str) -> Compared {
    match session(bytes, command) {
        None => Ok(Outcome::OutOfDomain),
        Some(s) => compare_session(&Oracle::new(pin), &s),
    }
}

/// `diff_p2p_headers`: runs of `headers` (and `headers2`) from one peer.
pub fn compare_p2p_headers(bytes: &[u8], pin: Pin) -> Compared {
    compare(bytes, pin, "headers")
}

/// `diff_p2p_headers2`: runs of `headers2` (and `headers`) from one peer.
pub fn compare_p2p_headers2(bytes: &[u8], pin: Pin) -> Compared {
    compare(bytes, pin, "headers2")
}

/// `diff_p2p_inv`: inventories announced by one peer.
pub fn compare_p2p_inv(bytes: &[u8], pin: Pin) -> Compared {
    compare(bytes, pin, "inv")
}

/// `diff_p2p_getdata`: inventories one peer asks for.
pub fn compare_p2p_getdata(bytes: &[u8], pin: Pin) -> Compared {
    compare(bytes, pin, "getdata")
}

/// `diff_p2p_notfound`: inventories one peer says it does not have.
pub fn compare_p2p_notfound(bytes: &[u8], pin: Pin) -> Compared {
    compare(bytes, pin, "notfound")
}
