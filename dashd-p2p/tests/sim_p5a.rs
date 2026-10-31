//! P5a in `sim`: Core's object-request tracker at the `inv` branch, section 13 and `notfound`;
//! the `clsig` branch with its solicitation check; `PostProcessMessage` of a `ChainLocks` result;
//! `MSG_CLSIG` in `ProcessGetData`; the non-transaction relay of section 8 (P5a D8-D16); and
//! `-blocksonly`, rdashd's default until P5 (P5a D23).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dash_num::Hash256;
use dash_p2p::conn::Permissions;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, ConnectionType, PeerId, Why};
use dash_p2p_core::{ClSig, GetData, Inv, InvType, Inventory, NotFound};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::MessageProcessingResult;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::registry::Extension;
use dashd_p2p::processing::seams::{ChainLocks, SeenChainLocks, StaticOverlayState};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{HandlerCx, NetHandler, Registry, Seams};

const MSG_TX: u32 = 1;
const MSG_FILTERED_BLOCK: u32 = 3;
const MSG_SPORK: u32 = 6;
const MSG_GOVERNANCE_OBJECT: u32 = 17;
const MSG_GOVERNANCE_OBJECT_VOTE: u32 = 18;
const MSG_CMPCT_BLOCK: u32 = 20;
const MSG_QUORUM_FINAL_COMMITMENT: u32 = 21;
const MSG_QUORUM_CONTRIB: u32 = 23;
const MSG_QUORUM_COMPLAINT: u32 = 24;
const MSG_QUORUM_JUSTIFICATION: u32 = 25;
const MSG_QUORUM_PREMATURE_COMMITMENT: u32 = 26;
const MSG_QUORUM_RECOVERED_SIG: u32 = 28;
const MSG_CLSIG: u32 = 29;
const MSG_ISDLOCK: u32 = 31;
const MSG_DSQ: u32 = 32;
const MSG_PLATFORM_BAN: u32 = 33;
const MSG_ASSET_UNLOCK: u32 = 34;

/// A sim whose `spork` has an owner that consumes responses ([`SporkOwner`]).
fn sim(seams: Seams) -> Sim {
    sim_reg(ProcessorOptions::default(), owners(&[]), seams)
}

/// A registry with [`SporkOwner`] and `more`.
fn owners(more: &[fn() -> Box<dyn NetHandler>]) -> Registry {
    let mut reg = Registry::new();
    reg.register(Box::new(SporkOwner));
    for h in more {
        reg.register(h());
    }
    reg
}

fn sim_reg(opts: ProcessorOptions, registry: Registry, seams: Seams) -> Sim {
    let p = Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    Sim::with(p, id, opts, registry, seams, |_| {})
}

/// A peer of `conn_type` through the handshake, its handshake output drained.
fn peer(sim: &mut Sim, n: u8, conn_type: ConnectionType) -> SimPeer {
    let p = sim.connect(n, conn_type, Permissions::NONE);
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    settle(sim);
    sim.sent(&p);
    p
}

/// Passes until no peer has work, then one more: a peer that came before the sender in the
/// pass's shuffle sends on the next pass.
fn settle(sim: &mut Sim) {
    while sim.pass() {}
    sim.pass();
}

fn inv(ty: u32, hash: [u8; 32]) -> Inventory {
    Inventory {
        inv_type: InvType::new(ty),
        hash: Hash256::from_lendian(hash),
    }
}

fn encode<T: BaseCodec>(m: &T) -> Vec<u8> {
    let mut out = Vec::new();
    m.encode(&mut out);
    out
}

fn announce(sim: &mut Sim, p: &SimPeer, invs: &[Inventory]) {
    let m = Inv {
        inventory: invs.to_vec(),
    };
    sim.deliver(p, &[("inv", encode(&m))]);
    settle(sim);
}

/// The inventories of `cmd` messages `p` was sent since the last read, waiting for `n` frames.
fn got(sim: &Sim, p: &SimPeer, cmd: CommandString, n: usize) -> Vec<Inventory> {
    sim.sent_at_least(p, n)
        .into_iter()
        .filter(|(c, _)| *c == cmd)
        .flat_map(|(c, payload)| match c {
            CommandString::GETDATA => GetData::decode(&mut &payload[..]).unwrap().inventory,
            CommandString::NOTFOUND => NotFound::decode(&mut &payload[..]).unwrap().inventory,
            _ => Inv::decode(&mut &payload[..]).unwrap().inventory,
        })
        .collect()
}

fn getdata(sim: &Sim, p: &SimPeer, n: usize) -> Vec<Inventory> {
    got(sim, p, CommandString::GETDATA, n)
}

fn score(sim: &Sim, p: &SimPeer) -> i32 {
    sim.shared.misbehavior(p.handle.id()).unwrap().0
}

/// A ChainLock with a zero signature at `height`, and its `MSG_CLSIG` inventory (Core's
/// `SerializeHash`: the zero signature re-serializes as read).
fn clsig(height: i32) -> (ClSig, Inventory) {
    let cl = ClSig {
        height,
        block_hash: [0xab; 32].into(),
        sig: [0; 96].into(),
    };
    let hash = dash_sha256::sha256d(&encode(&cl));
    (cl, inv(MSG_CLSIG, hash))
}

/// What LK1a's seam was asked, and the results it returns.
#[derive(Default)]
struct Calls {
    processed: Vec<(PeerId, Inventory)>,
    results: Vec<MessageProcessingResult>,
    stored: Option<ClSig>,
}

#[derive(Clone, Default)]
struct Recording(Arc<Mutex<Calls>>);

impl ChainLocks for Recording {
    fn is_enabled(&self) -> bool {
        true
    }

    fn already_have(&self, _hash: &[u8; 32]) -> bool {
        false
    }

    fn process_new_chainlock(
        &mut self,
        from: PeerId,
        _clsig: &ClSig,
        hash: &[u8; 32],
    ) -> MessageProcessingResult {
        let mut c = self.0.lock().unwrap();
        c.processed.push((from, inv(MSG_CLSIG, *hash)));
        if c.results.is_empty() {
            MessageProcessingResult::default()
        } else {
            c.results.remove(0)
        }
    }

    fn chainlock_by_hash(&self, _hash: &[u8; 32]) -> Option<ClSig> {
        self.0.lock().unwrap().stored.clone()
    }

    fn best_chainlock(&self) -> Option<ClSig> {
        None
    }
}

fn with_chainlocks(cl: &Recording) -> Sim {
    sim(Seams {
        chainlocks: Box::new(cl.clone()),
        ..Seams::default()
    })
}

/// In IBD only sporks are requested (np:4567-4577); the request waits on the first announcer
/// for 60 s, then goes to the next one with Core's timeout line (np:6851-6855, 6886-6891).
#[test]
fn a_spork_inv_is_requested_in_ibd_and_falls_back_after_60_s() {
    let mut sim = sim(Seams::default());
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let b = peer(&mut sim, 2, ConnectionType::Inbound);
    let spork = inv(MSG_SPORK, [6; 32]);
    let gov = inv(MSG_GOVERNANCE_OBJECT, [17; 32]);
    announce(&mut sim, &a, &[spork, gov]);
    assert_eq!(getdata(&sim, &a, 1), [spork]);
    // The second announcement waits behind the request in flight.
    announce(&mut sim, &b, &[spork]);
    assert_eq!(getdata(&sim, &b, 0), []);
    sim.clock.advance(Duration::from_secs(59));
    settle(&mut sim);
    assert_eq!(getdata(&sim, &b, 0), []);
    sim.clock.advance(Duration::from_secs(1));
    settle(&mut sim);
    assert_eq!(getdata(&sim, &b, 1), [spork]);
    assert_eq!(getdata(&sim, &a, 0), []);
}

/// `notfound` completes the request, so the next announcer is asked without waiting
/// (np:5756-5764).
#[test]
fn notfound_moves_the_request_to_the_next_announcer() {
    let mut sim = sim(Seams::default());
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let b = peer(&mut sim, 2, ConnectionType::Inbound);
    let spork = inv(MSG_SPORK, [6; 32]);
    announce(&mut sim, &a, &[spork]);
    assert_eq!(getdata(&sim, &a, 1), [spork]);
    announce(&mut sim, &b, &[spork]);
    sim.deliver(
        &a,
        &[(
            "notfound",
            encode(&NotFound {
                inventory: vec![spork],
            }),
        )],
    );
    settle(&mut sim);
    assert_eq!(getdata(&sim, &b, 1), [spork]);
}

/// A peer that disconnects while its answer is awaited hands the request to the next announcer
/// at once (np:1816, `DisconnectedPeer`).
#[test]
fn a_disconnected_announcer_hands_its_request_to_the_next() {
    let mut sim = sim(Seams::default());
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let b = peer(&mut sim, 2, ConnectionType::Inbound);
    let spork = inv(MSG_SPORK, [6; 32]);
    announce(&mut sim, &a, &[spork]);
    assert_eq!(getdata(&sim, &a, 1), [spork]);
    announce(&mut sim, &b, &[spork]);
    assert_eq!(getdata(&sim, &b, 0), []);
    sim.core.disconnect(a.handle.id(), Why::Processor("test"));
    sim.remove_disconnected();
    settle(&mut sim);
    assert_eq!(getdata(&sim, &b, 1), [spork]);
}

/// A `spork` handler that consumes the response and relays nothing, as Core's does for a spork
/// it rejects (`ret.m_to_erase` only); the payload is the spork's hash. Any other payload queues
/// a relay and throws.
struct SporkOwner;

const SPORK_CMD: [CommandString; 1] = [CommandString::from_static("spork")];

impl NetHandler for SporkOwner {
    fn commands(&self) -> &'static [CommandString] {
        &SPORK_CMD
    }

    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        let Ok(hash) = payload.try_into() else {
            // Queues, then throws: Core post-processes nothing.
            cx.relay_inv(inv(MSG_SPORK, [9; 32]));
            return Err(Throw::ios_failure("short spork"));
        };
        cx.received_response(inv(MSG_SPORK, hash));
        Ok(())
    }

    /// It has spork `[0xee; 32]`.
    fn already_have(&self, inv: &Inventory) -> bool {
        inv.hash.to_lendian() == [0xee; 32]
    }
}

/// A handler's `AlreadyHave` keeps its inventory from being requested (np:2366-2459).
#[test]
fn a_handlers_had_inventory_is_not_requested() {
    let mut sim = sim(Seams::default());
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let (had, new) = (inv(MSG_SPORK, [0xee; 32]), inv(MSG_SPORK, [6; 32]));
    announce(&mut sim, &a, &[had, new]);
    assert_eq!(getdata(&sim, &a, 1), [new]);
}

/// `m_to_erase` completes the request, so the next announcer is asked without waiting, even
/// though nothing was relayed (np:3819-3825).
#[test]
fn a_consumed_response_moves_the_request_to_the_next_announcer() {
    let mut sim = sim(Seams::default());
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let b = peer(&mut sim, 2, ConnectionType::Inbound);
    let spork = inv(MSG_SPORK, [6; 32]);
    announce(&mut sim, &a, &[spork]);
    assert_eq!(getdata(&sim, &a, 1), [spork]);
    announce(&mut sim, &b, &[spork]);
    assert_eq!(getdata(&sim, &b, 0), []);
    sim.deliver(&a, &[("spork", vec![6; 32])]);
    settle(&mut sim);
    assert_eq!(getdata(&sim, &b, 1), [spork]);
}

/// A handler that throws returns no `MessageProcessingResult`: what it queued is dropped
/// (P5a D13, np:3819-3836).
#[test]
fn a_throwing_handlers_results_are_dropped() {
    let mut sim = sim(Seams::default());
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let b = peer(&mut sim, 2, ConnectionType::Inbound);
    assert!(sim.admitted(&a, "spork", vec![1]).is_err());
    settle(&mut sim);
    assert_eq!(got(&sim, &b, CommandString::INV, 0), []);
}

/// A preferred (outbound) announcer is asked before a non-preferred one that announced first
/// (txrequest.cpp `PriorityComputer`, np:1648-1663).
#[test]
fn a_preferred_announcer_wins_over_an_earlier_inbound_one() {
    let mut sim = sim(Seams::default());
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let o = peer(&mut sim, 2, ConnectionType::OutboundFullRelay);
    let spork = inv(MSG_SPORK, [6; 32]);
    // Both announce before the next `SendMessages` of either.
    let m = encode(&Inv {
        inventory: vec![spork],
    });
    sim.admitted(&a, "inv", m.clone()).unwrap();
    sim.admitted(&o, "inv", m).unwrap();
    settle(&mut sim);
    assert_eq!(getdata(&sim, &o, 1), [spork]);
    assert_eq!(getdata(&sim, &a, 0), []);
}

/// A `spork` handler whose `AlreadyHave` is a set the test fills.
struct HadSporks(Arc<Mutex<HashSet<[u8; 32]>>>);

impl NetHandler for HadSporks {
    fn commands(&self) -> &'static [CommandString] {
        &SPORK_CMD
    }

    fn process_message(
        &mut self,
        _cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        Ok(())
    }

    fn already_have(&self, inv: &Inventory) -> bool {
        self.0.lock().unwrap().contains(&inv.hash.to_lendian())
    }
}

/// The sizes of the `getdata` messages `p` was sent since the last read, waiting for `n`.
fn getdata_sizes(sim: &Sim, p: &SimPeer, n: usize) -> Vec<usize> {
    sim.sent_at_least(p, n)
        .into_iter()
        .filter(|(c, _)| *c == CommandString::GETDATA)
        .map(|(_, payload)| GetData::decode(&mut &payload[..]).unwrap().inventory.len())
        .collect()
}

/// A peer holds at most 100,000 announcements (np:89, 1643-1646), objects are asked for in
/// `getdata` messages of 1,000 (np:115, 6872-6875), and an announcement whose object became had
/// before its request is forgotten (np:6862-6867, 6888-6891), which frees its place under the cap.
#[test]
fn announcements_are_capped_at_100000_asked_in_1000s_and_forgotten_once_had() {
    let had = Arc::new(Mutex::new(HashSet::new()));
    let mut reg = Registry::new();
    reg.register(Box::new(HadSporks(Arc::clone(&had))));
    let mut sim = sim_reg(ProcessorOptions::default(), reg, Seams::default());
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let spork = |i: u32| {
        let mut h = [0x5a; 32];
        h[..4].copy_from_slice(&i.to_le_bytes());
        inv(MSG_SPORK, h)
    };
    // `MAX_INV_SZ` per message: two fill the cap.
    let first: Vec<_> = (0..50_000).map(spork).collect();
    announce(&mut sim, &a, &first);
    assert_eq!(getdata_sizes(&sim, &a, 50), [1000; 50]);
    // With 50,000 in flight, these wait the overload's 2 s; one becomes had meanwhile.
    let second: Vec<_> = (50_000..100_000).map(spork).collect();
    announce(&mut sim, &a, &second);
    assert!(getdata_sizes(&sim, &a, 0).is_empty());
    had.lock().unwrap().insert(spork(70_000).hash.to_lendian());
    sim.clock.advance(Duration::from_secs(2));
    settle(&mut sim);
    let mut sizes = vec![1000; 49];
    sizes.push(999);
    assert_eq!(getdata_sizes(&sim, &a, 50), sizes);
    // The had one's place is free; one more is over the cap.
    let (fits, over) = (spork(100_000), spork(100_001));
    announce(&mut sim, &a, &[fits, over]);
    sim.clock.advance(Duration::from_secs(2));
    settle(&mut sim);
    assert_eq!(getdata(&sim, &a, 1), [fits]);
    sim.clock.advance(Duration::from_secs(60));
    settle(&mut sim);
    assert!(!getdata(&sim, &a, 0).contains(&over));
}

/// A peer with 100 requests in flight waits 2 s more for the next (np:1657-1659).
#[test]
fn an_overloaded_announcer_is_asked_2_s_later() {
    let mut sim = sim(Seams::default());
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let first: Vec<_> = (0..100u8).map(|i| inv(MSG_SPORK, [i; 32])).collect();
    announce(&mut sim, &a, &first);
    assert_eq!(getdata(&sim, &a, 1), first);
    let next = inv(MSG_SPORK, [100; 32]);
    announce(&mut sim, &a, &[next]);
    sim.clock.advance(Duration::from_micros(1_999_999));
    settle(&mut sim);
    assert_eq!(getdata(&sim, &a, 0), []);
    sim.clock.advance(Duration::from_micros(1));
    settle(&mut sim);
    assert_eq!(getdata(&sim, &a, 1), [next]);
}

/// Out of IBD, a non-preferred peer's `MSG_TX` waits 2 s; a preferred peer's and any other type
/// do not (np:1650-1656).
#[test]
fn only_a_non_preferred_tx_announcement_is_delayed() {
    let mut sim = sim(Seams::default());
    sim.chain.set_ibd(false);
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let o = peer(&mut sim, 2, ConnectionType::OutboundFullRelay);
    let spork = inv(MSG_SPORK, [6; 32]);
    announce(&mut sim, &a, &[inv(MSG_TX, [1; 32]), spork]);
    assert_eq!(getdata(&sim, &a, 1), [spork]);
    let tx_o = inv(MSG_TX, [2; 32]);
    announce(&mut sim, &o, &[tx_o]);
    assert_eq!(getdata(&sim, &o, 1), [tx_o]);
    sim.clock.advance(Duration::from_micros(1_999_999));
    settle(&mut sim);
    assert_eq!(getdata(&sim, &a, 0), []);
    sim.clock.advance(Duration::from_micros(1));
    settle(&mut sim);
    assert_eq!(getdata(&sim, &a, 1), [inv(MSG_TX, [1; 32])]);
}

/// A sim with governance's readiness and InstantSend at `governance_ready` and
/// `instantsend_enabled`.
fn with_overlay(governance_ready: bool, instantsend_enabled: bool) -> Sim {
    sim(Seams {
        overlay: Box::new(StaticOverlayState {
            governance_ready,
            instantsend_enabled,
        }),
        ..Seams::default()
    })
}

/// `sim` out of IBD, with an outbound full-relay peer `a` and an inbound peer `b`.
fn out_of_ibd_pair(mut sim: Sim) -> (Sim, SimPeer, SimPeer) {
    sim.chain.set_ibd(false);
    let a = peer(&mut sim, 1, ConnectionType::OutboundFullRelay);
    let b = peer(&mut sim, 2, ConnectionType::Inbound);
    (sim, a, b)
}

/// Out of IBD, what Core does not know (`MSG_FILTERED_BLOCK`, `MSG_CMPCT_BLOCK`), governance
/// before mnsync's blockchain sync and `MSG_ISDLOCK` while InstantSend is off are had, so never
/// requested (P5a D20, np:2426-2459).
#[test]
fn what_core_has_is_never_requested() {
    let mut sim = sim(Seams::default());
    sim.chain.set_ibd(false);
    let a = peer(&mut sim, 1, ConnectionType::OutboundFullRelay);
    let spork = inv(MSG_SPORK, [6; 32]);
    let had = [
        inv(MSG_FILTERED_BLOCK, [3; 32]),
        inv(MSG_GOVERNANCE_OBJECT, [17; 32]),
        inv(MSG_GOVERNANCE_OBJECT_VOTE, [18; 32]),
        inv(MSG_CMPCT_BLOCK, [20; 32]),
        inv(MSG_ISDLOCK, [31; 32]),
    ];
    announce(&mut sim, &a, &had);
    announce(&mut sim, &a, &[spork]);
    assert_eq!(getdata(&sim, &a, 1), [spork]);
    sim.clock.advance(Duration::from_secs(120));
    settle(&mut sim);
    assert_eq!(getdata(&sim, &a, 0), []);
}

/// Out of IBD, every known type Core does not have is requested whether or not its manager is
/// ported (np:4567-4577): governance once mnsync's blockchain sync holds, `MSG_ISDLOCK` once
/// InstantSend is on. Each request waits its type's interval before the next announcer is asked:
/// 5 s for a ChainLock, 10 s for an InstantSend lock, 15 s for a recovered signature, 60 s for
/// the rest (np:1595-1610, 6851-6855).
#[test]
fn every_known_type_core_lacks_is_requested_with_its_timeout() {
    let (mut sim, a, b) = out_of_ibd_pair(with_overlay(true, true));
    let at = |ty: u32| inv(ty, [ty as u8; 32]);
    let rest = [
        MSG_TX,
        MSG_SPORK,
        MSG_GOVERNANCE_OBJECT,
        MSG_GOVERNANCE_OBJECT_VOTE,
        MSG_QUORUM_FINAL_COMMITMENT,
        MSG_QUORUM_CONTRIB,
        MSG_QUORUM_COMPLAINT,
        MSG_QUORUM_JUSTIFICATION,
        MSG_QUORUM_PREMATURE_COMMITMENT,
        MSG_DSQ,
        MSG_PLATFORM_BAN,
        MSG_ASSET_UNLOCK,
    ]
    .map(at);
    let all: Vec<_> = [at(MSG_CLSIG), at(MSG_ISDLOCK), at(MSG_QUORUM_RECOVERED_SIG)]
        .into_iter()
        .chain(rest)
        .collect();
    announce(&mut sim, &a, &all);
    assert_eq!(getdata(&sim, &a, 1), all);
    announce(&mut sim, &b, &all);
    let mut asked = Vec::new();
    for (secs, want) in [
        (5, vec![at(MSG_CLSIG)]),
        (5, vec![at(MSG_ISDLOCK)]),
        (5, vec![at(MSG_QUORUM_RECOVERED_SIG)]),
        (45, rest.to_vec()),
    ] {
        sim.clock.advance(Duration::from_micros(secs * 1_000_000 - 1));
        settle(&mut sim);
        asked.push(getdata(&sim, &b, 0));
        sim.clock.advance(Duration::from_micros(1));
        settle(&mut sim);
        asked.push(getdata(&sim, &b, 1));
        assert_eq!(asked, [vec![], want], "after {secs} s more");
        asked.clear();
    }
}

/// A peer with 100 requests in flight, whatever their types, makes the next spork and ChainLock
/// requests wait 2 s more (np:1657-1659): the overlay types without a ported manager count. With
/// transaction relay on (P5's default) the burst mixes every overlay type; under `-blocksonly`
/// (rdashd's until P5, DEC-322) only the types it does not disconnect for, with governance after
/// mnsync's blockchain sync (P5a D23).
#[test]
fn a_mixed_burst_delays_the_next_spork_and_clsig_by_2_s() {
    let relaying = [
        MSG_QUORUM_FINAL_COMMITMENT,
        MSG_QUORUM_CONTRIB,
        MSG_QUORUM_COMPLAINT,
        MSG_QUORUM_JUSTIFICATION,
        MSG_QUORUM_PREMATURE_COMMITMENT,
        MSG_QUORUM_RECOVERED_SIG,
        MSG_DSQ,
        MSG_PLATFORM_BAN,
        MSG_SPORK,
        MSG_CLSIG,
    ];
    let blocks_only_types = [
        MSG_QUORUM_FINAL_COMMITMENT,
        MSG_GOVERNANCE_OBJECT,
        MSG_GOVERNANCE_OBJECT_VOTE,
        MSG_SPORK,
        MSG_CLSIG,
    ];
    let relay = sim(Seams::default());
    relay.chain.set_ibd(false);
    for (mut sim, types) in [(relay, &relaying[..]), (blocks_only(true), &blocks_only_types[..])] {
        let a = peer(&mut sim, 1, ConnectionType::OutboundFullRelay);
        let burst: Vec<_> =
            (0..100u8).map(|i| inv(types[usize::from(i) % types.len()], [i; 32])).collect();
        announce(&mut sim, &a, &burst);
        assert_eq!(getdata(&sim, &a, 1), burst);
        let next = [inv(MSG_SPORK, [100; 32]), inv(MSG_CLSIG, [101; 32])];
        announce(&mut sim, &a, &next);
        sim.clock.advance(Duration::from_micros(1_999_999));
        settle(&mut sim);
        assert_eq!(getdata(&sim, &a, 0), []);
        sim.clock.advance(Duration::from_micros(1));
        settle(&mut sim);
        assert_eq!(getdata(&sim, &a, 1), next);
        assert!(!a.handle.disconnect_requested());
    }
}

/// The `inbound-qfcommit` fixture's commitment as dashd v24 re-serializes it, and its inventory.
fn commitment() -> (Vec<u8>, Inventory) {
    let fixture = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dashd-v24/inbound-qfcommit.txt"
    ))
    .unwrap();
    let wire = fixture
        .lines()
        .find_map(|l| l.strip_prefix("> 1 qfcommit "))
        .map(dashd_p2p::sim::transcript::unhex)
        .unwrap();
    let mut canonical = Vec::new();
    dash_primitives::Commitment::decode(&mut &wire[..]).unwrap().encode(&mut canonical);
    let hash = dash_sha256::sha256d(&canonical);
    (canonical, inv(MSG_QUORUM_FINAL_COMMITMENT, hash))
}

/// A requested commitment passes the solicitation gate on the tracker
/// (`PeerConsumeGetDataResponse`, blockprocessor.cpp:113-119): unscored, and its request
/// complete, so the next announcer is asked without waiting; a second copy is unrequested.
#[test]
fn a_requested_qfcommit_completes_its_request_unscored() {
    let (mut sim, a, b) = out_of_ibd_pair(sim(Seams::default()));
    let (payload, qc) = commitment();
    announce(&mut sim, &a, &[qc]);
    assert_eq!(getdata(&sim, &a, 1), [qc]);
    announce(&mut sim, &b, &[qc]);
    sim.deliver(&a, &[("qfcommit", payload.clone())]);
    settle(&mut sim);
    assert_eq!(score(&sim, &a), 0);
    assert_eq!(getdata(&sim, &b, 1), [qc]);
    sim.deliver(&a, &[("qfcommit", payload)]);
    settle(&mut sim);
    assert_eq!(score(&sim, &a), 10);
}

/// A recovered signature erases its request when read (`PeerEraseObjectRequest`,
/// llmq/net_signing.cpp:49-54), so the next announcer is asked without waiting 15 s.
#[test]
fn a_qsigrec_completes_its_request() {
    let (mut sim, a, b) = out_of_ibd_pair(sim(Seams::default()));
    // `LLMQ_TEST` (100), a type regtest has, so Core's type check passes (net_signing.cpp:56).
    let mut payload = vec![0x5a; 1 + 32 * 3 + 96];
    payload[0] = 100;
    let rec = inv(MSG_QUORUM_RECOVERED_SIG, dash_sha256::sha256d(&payload));
    announce(&mut sim, &a, &[rec]);
    assert_eq!(getdata(&sim, &a, 1), [rec]);
    announce(&mut sim, &b, &[rec]);
    sim.deliver(&a, &[("qsigrec", payload)]);
    settle(&mut sim);
    assert_eq!(getdata(&sim, &b, 1), [rec]);
    assert_eq!(score(&sim, &a), 0);
}

/// ChainLocks off (`SPORK_19`): a `clsig` is neither read nor scored (np:5795).
#[test]
fn a_clsig_while_chainlocks_are_off_is_dropped_unread() {
    let mut sim = sim(Seams::default());
    let a = sim.connect(1, ConnectionType::Inbound, Permissions::NONE);
    sim.admitted(&a, "clsig", vec![]).unwrap();
    let (cl, _) = clsig(10);
    sim.admitted(&a, "clsig", encode(&cl)).unwrap();
    assert_eq!(score(&sim, &a), 0);
}

/// Without a ChainLocks handler nothing is had, so out of IBD a `clsig` is still requested, as
/// Core with `SPORK_19` off requests it (np:2440-2441; chainlock/handler.cpp:76-87).
#[test]
fn a_clsig_inv_is_requested_while_chainlocks_are_off() {
    let mut sim = sim(Seams::default());
    sim.chain.set_ibd(false);
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let (_, i) = clsig(10);
    announce(&mut sim, &a, &[i]);
    assert_eq!(getdata(&sim, &a, 1), [i]);
}

/// P2P-12, PRD 23: an unrequested `clsig` scores 10 and never reaches `ProcessNewChainLock`; a
/// short one throws at the read, unscored; one canonical in neither BLS scheme throws at Core's
/// eager read (np:5794-5812).
#[test]
fn an_unrequested_clsig_scores_10_before_processing() {
    let cl = Recording::default();
    let mut sim = with_chainlocks(&cl);
    let a = sim.connect(1, ConnectionType::Inbound, Permissions::NONE);
    let (msg, _) = clsig(10);
    sim.admitted(&a, "clsig", encode(&msg)).unwrap();
    assert_eq!(score(&sim, &a), 10);
    assert!(cl.0.lock().unwrap().processed.is_empty());
    assert!(sim.admitted(&a, "clsig", vec![1, 2, 3]).is_err());
    let mut neither = msg.clone();
    neither.sig = [0xff; 96].into();
    let t = sim.admitted(&a, "clsig", encode(&neither)).unwrap_err();
    // Core: bls/bls.h:196
    assert_eq!(t.what, "malleable BLS object: iostream error");
    assert_eq!(score(&sim, &a), 10);
}

/// A requested `clsig` reaches `ProcessNewChainLock`; so does one answered within two request
/// intervals after its request expired (Core's `LATE`); the answer spends the grace, so a replay
/// scores (np:6925-6957). The request itself waits 5 s (`GetObjectInterval`).
#[test]
fn a_requested_clsig_is_processed_and_its_grace_is_spent() {
    let cl = Recording::default();
    let mut sim = with_chainlocks(&cl);
    sim.chain.set_ibd(false);
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let (msg, i) = clsig(10);
    announce(&mut sim, &a, &[i]);
    assert_eq!(getdata(&sim, &a, 1), [i]);
    sim.deliver(&a, &[("clsig", encode(&msg))]);
    settle(&mut sim);
    assert_eq!(score(&sim, &a), 0);
    assert_eq!(cl.0.lock().unwrap().processed, [(a.handle.id(), i)]);
    sim.deliver(&a, &[("clsig", encode(&msg))]);
    settle(&mut sim);
    assert_eq!(score(&sim, &a), 10, "a replay is unrequested");

    // Late: the request expired after 5 s, the answer comes within 10 s of it.
    let (msg, i) = clsig(11);
    announce(&mut sim, &a, &[i]);
    assert_eq!(getdata(&sim, &a, 1), [i]);
    sim.clock.advance(Duration::from_secs(5));
    settle(&mut sim);
    sim.clock.advance(Duration::from_secs(4));
    sim.deliver(&a, &[("clsig", encode(&msg))]);
    settle(&mut sim);
    assert_eq!(score(&sim, &a), 10);
    assert_eq!(cl.0.lock().unwrap().processed.len(), 2);
}

/// `PostProcessMessage` of LK1a's result: the score, then for each inventory `ForgetTxHash` and
/// `RelayInv` to every peer, block-relay-only ones included (P5a D11), but not to a peer that
/// announced it (its known filter) (np:3819-3836, 6723-6745).
#[test]
fn a_chainlock_result_is_scored_and_relayed() {
    let cl = Recording::default();
    let mut sim = with_chainlocks(&cl);
    sim.chain.set_ibd(false);
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let b = peer(&mut sim, 2, ConnectionType::Inbound);
    let blocks = peer(&mut sim, 3, ConnectionType::BlockRelay);
    let (msg, i) = clsig(10);
    announce(&mut sim, &a, &[i]);
    assert_eq!(getdata(&sim, &a, 1), [i]);
    // B announces too; the relay makes the tracker forget it, so B is never asked.
    announce(&mut sim, &b, &[i]);
    cl.0.lock().unwrap().results.push(MessageProcessingResult::relay(i));
    sim.deliver(&a, &[("clsig", encode(&msg))]);
    settle(&mut sim);
    assert_eq!(got(&sim, &blocks, CommandString::INV, 1), [i]);
    assert_eq!(got(&sim, &a, CommandString::INV, 0), []);
    assert_eq!(got(&sim, &b, CommandString::INV, 0), []);
    sim.clock.advance(Duration::from_secs(6));
    settle(&mut sim);
    assert_eq!(getdata(&sim, &b, 0), []);

    // An invalid one: LK1a's `MisbehavingError{10}`.
    let (msg, i) = clsig(11);
    announce(&mut sim, &b, &[i]);
    assert_eq!(getdata(&sim, &b, 1), [i]);
    cl.0.lock().unwrap().results.push(MessageProcessingResult::misbehaving(10, ""));
    sim.deliver(&b, &[("clsig", encode(&msg))]);
    settle(&mut sim);
    assert_eq!(score(&sim, &b), 10);
}

/// An extension Core runs on every known command: on `clsig` it records the call and relays
/// spork `[tag; 32]`, then throws if `throws`.
struct Ext {
    ext: Extension,
    cmds: &'static [CommandString],
    tag: u8,
    throws: bool,
    calls: Arc<Mutex<Vec<(u8, CommandString)>>>,
}

const MNAUTH_CMD: [CommandString; 1] = [CommandString::from_static("mnauth")];
const PLATFORMBAN_CMD: [CommandString; 1] = [CommandString::from_static("platformban")];

impl NetHandler for Ext {
    fn commands(&self) -> &'static [CommandString] {
        self.cmds
    }

    fn extension(&self) -> Option<Extension> {
        Some(self.ext)
    }

    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        self.calls.lock().unwrap().push((self.tag, *cmd));
        if *cmd == CommandString::CLSIG {
            cx.relay_inv(inv(MSG_SPORK, [self.tag; 32]));
            if self.throws {
                return Err(Throw::ios_failure("extension"));
            }
        }
        Ok(())
    }
}

type ExtCalls = Arc<Mutex<Vec<(u8, CommandString)>>>;

/// A sim out of IBD with LK1a's recording seam and two extensions registered out of Core's
/// order: platform ban (tag 2, throwing if `platformban_throws`), then CMNAuth (tag 1). Peer A is
/// asked for ChainLock `i`, which LK1a's seam will relay; C is another peer.
fn extensions_sim(
    platformban_throws: bool,
) -> (Sim, Recording, ExtCalls, SimPeer, SimPeer, ClSig, Inventory) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut reg = owners(&[]);
    for (ext, cmds, tag, throws) in [
        (Extension::PlatformBan, &PLATFORMBAN_CMD, 2, platformban_throws),
        (Extension::MnAuth, &MNAUTH_CMD, 1, false),
    ] {
        reg.register(Box::new(Ext {
            ext,
            cmds,
            tag,
            throws,
            calls: Arc::clone(&calls),
        }));
    }
    let cl = Recording::default();
    let mut sim = sim_reg(
        ProcessorOptions::default(),
        reg,
        Seams {
            chainlocks: Box::new(cl.clone()),
            ..Seams::default()
        },
    );
    sim.chain.set_ibd(false);
    let a = peer(&mut sim, 1, ConnectionType::OutboundFullRelay);
    let c = peer(&mut sim, 2, ConnectionType::Inbound);
    let (msg, i) = clsig(10);
    announce(&mut sim, &a, &[i]);
    assert_eq!(getdata(&sim, &a, 1), [i]);
    cl.0.lock().unwrap().results.push(MessageProcessingResult::relay(i));
    (sim, cl, calls, a, c, msg, i)
}

/// Core runs its extensions on `clsig` before the `CLSIG` branch, in its order whatever the
/// registration order, applying each result as it returns, and not again after it
/// (np:5779-5819, P5a D21): C is sent the CMNAuth result, the platform-ban result, then LK1a's.
#[test]
fn extensions_run_before_the_clsig_branch_in_cores_order() {
    let (mut sim, _cl, calls, a, c, msg, i) = extensions_sim(false);
    sim.deliver(&a, &[("clsig", encode(&msg))]);
    settle(&mut sim);
    assert_eq!(
        got(&sim, &c, CommandString::INV, 1),
        [inv(MSG_SPORK, [1; 32]), inv(MSG_SPORK, [2; 32]), i]
    );
    assert_eq!(*calls.lock().unwrap(), [(1, CommandString::CLSIG), (2, CommandString::CLSIG)]);
    // Any other command reaches only its owner.
    sim.deliver(&a, &[("mnauth", vec![])]);
    settle(&mut sim);
    assert_eq!(calls.lock().unwrap().last(), Some(&(1, CommandString::from_static("mnauth"))));
    assert_eq!(calls.lock().unwrap().len(), 3);
}

/// An extension that throws ends the message: what the extensions before it returned stays
/// applied, its own results are dropped, and the `CLSIG` branch never runs (np:5779-5792; the
/// exception leaves `ProcessMessage`, P5a D13).
#[test]
fn a_throwing_extension_keeps_earlier_results_and_skips_the_clsig_branch() {
    let (mut sim, cl, calls, a, c, msg, _) = extensions_sim(true);
    assert!(sim.admitted(&a, "clsig", encode(&msg)).is_err());
    settle(&mut sim);
    assert_eq!(got(&sim, &c, CommandString::INV, 1), [inv(MSG_SPORK, [1; 32])]);
    assert_eq!(*calls.lock().unwrap(), [(1, CommandString::CLSIG), (2, CommandString::CLSIG)]);
    assert_eq!(cl.0.lock().unwrap().processed, []);
    assert_eq!(score(&sim, &a), 0);
}

/// `getdata` for a ChainLock: served from LK1a's store, else `notfound` (np:3041-3047,
/// 3094-3110).
#[test]
fn a_clsig_getdata_is_served_from_the_store_or_notfound() {
    let cl = Recording::default();
    let mut sim = with_chainlocks(&cl);
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let (msg, i) = clsig(10);
    let req = encode(&GetData {
        inventory: vec![i],
    });
    sim.deliver(&a, &[("getdata", req.clone())]);
    settle(&mut sim);
    assert_eq!(got(&sim, &a, CommandString::NOTFOUND, 1), [i]);
    cl.0.lock().unwrap().stored = Some(msg.clone());
    sim.deliver(&a, &[("getdata", req)]);
    settle(&mut sim);
    let sent = sim.sent_at_least(&a, 1);
    assert_eq!(sent, [(CommandString::CLSIG, encode(&msg))]);
}

/// The stand-in (P5a D5): `SPORK_19` read live; a processed hash is had, so its announcement is
/// not requested again; nothing is relayed or scored; nothing is served.
#[test]
fn the_seen_stand_in_remembers_and_does_nothing_else() {
    let enabled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&enabled);
    let mut sim = sim(Seams {
        chainlocks: Box::new(SeenChainLocks::new(move || {
            flag.load(std::sync::atomic::Ordering::SeqCst)
        })),
        ..Seams::default()
    });
    sim.chain.set_ibd(false);
    let a = peer(&mut sim, 1, ConnectionType::Inbound);
    let b = peer(&mut sim, 2, ConnectionType::Inbound);
    let (msg, i) = clsig(10);
    announce(&mut sim, &a, &[i]);
    assert_eq!(getdata(&sim, &a, 1), [i]);
    // Off: dropped unread, so neither had nor answered; B is asked once A's request expires.
    sim.deliver(&a, &[("clsig", encode(&msg))]);
    settle(&mut sim);
    announce(&mut sim, &b, &[i]);
    sim.clock.advance(Duration::from_secs(5));
    settle(&mut sim);
    assert_eq!(getdata(&sim, &b, 1), [i]);
    enabled.store(true, std::sync::atomic::Ordering::SeqCst);
    let (msg, i) = clsig(11);
    announce(&mut sim, &a, &[i]);
    assert_eq!(getdata(&sim, &a, 1), [i]);
    sim.deliver(&a, &[("clsig", encode(&msg))]);
    settle(&mut sim);
    assert_eq!(score(&sim, &a), 0);
    assert_eq!(got(&sim, &b, CommandString::INV, 0), []);
    // Had now: B's announcement is not requested, not even past A's request's expiry.
    announce(&mut sim, &b, &[i]);
    assert_eq!(getdata(&sim, &b, 0), []);
    sim.clock.advance(Duration::from_secs(5));
    settle(&mut sim);
    assert_eq!(getdata(&sim, &b, 0), []);
    sim.deliver(
        &b,
        &[(
            "getdata",
            encode(&GetData {
                inventory: vec![i],
            }),
        )],
    );
    settle(&mut sim);
    assert_eq!(got(&sim, &b, CommandString::NOTFOUND, 1), [i]);
}

/// The stand-in's seen map trims to 1024 oldest-first once above 2048 (D5).
#[test]
fn the_seen_map_is_bounded() {
    let mut s = SeenChainLocks::new(|| true);
    let (msg, _) = clsig(1);
    for n in 0..2049u32 {
        let mut h = [0; 32];
        h[..4].copy_from_slice(&n.to_le_bytes());
        assert!(s.process_new_chainlock(1, &msg, &h).is_empty());
        if n == 2047 {
            assert_eq!(s.seen_len(), 2048, "no trim at the maximum");
        }
    }
    assert_eq!(s.seen_len(), 1024);
    let mut newest = [0; 32];
    newest[..4].copy_from_slice(&2048u32.to_le_bytes());
    assert!(s.already_have(&newest));
    assert!(!s.already_have(&[0; 32]));
}

/// Section 8's non-tx loop: a type that violates blocks-only goes only to peers that relay
/// transactions; an InstantSend lock not to a block-relay-only peer (`fSendIS`), which under
/// `-blocksonly` is every peer without the `relay` permission; sporks and ChainLocks to every
/// peer (np:6719-6745; net.cpp:554-560 `IsBlockRelayOnly`).
#[test]
fn section_8_withholds_by_type_and_connection() {
    let sporkinv = inv(MSG_SPORK, [6; 32]);
    let (_, clsiginv) = clsig(10);
    let islock = Inventory {
        inv_type: InvType::IsdLock,
        hash: Hash256::from_lendian([31; 32]),
    };
    let recsig = Inventory {
        inv_type: InvType::QuorumRecoveredSig,
        hash: Hash256::from_lendian([22; 32]),
    };
    let all = [sporkinv, clsiginv, islock, recsig];
    let relay = |sim: &mut Sim| {
        for i in &all {
            sim.shared.relay_inv(i);
        }
        settle(sim);
    };

    let mut sim = sim(Seams::default());
    let full = peer(&mut sim, 1, ConnectionType::Inbound);
    let blocks = peer(&mut sim, 2, ConnectionType::BlockRelay);
    relay(&mut sim);
    assert_eq!(got(&sim, &full, CommandString::INV, 1), all);
    assert_eq!(got(&sim, &blocks, CommandString::INV, 1), [sporkinv, clsiginv]);

    let opts = ProcessorOptions {
        blocks_only: true,
        ..ProcessorOptions::default()
    };
    let mut sim = sim_reg(opts, owners(&[]), Seams::default());
    let plain = peer(&mut sim, 1, ConnectionType::Inbound);
    let relayer = sim.connect(2, ConnectionType::Inbound, Permissions::RELAY);
    sim.handshake(&relayer, PeerVersionMsg::v24(SIM_START));
    settle(&mut sim);
    sim.sent(&relayer);
    relay(&mut sim);
    assert_eq!(got(&sim, &plain, CommandString::INV, 1), [sporkinv, clsiginv, recsig]);
    assert_eq!(got(&sim, &relayer, CommandString::INV, 1), all);
}

/// The `message` field of every `net` event on this thread, in emission order (as `sim_p3_s1`).
/// `tracing` caches a callsite's interest globally, so a test asserts only lines whose callsite
/// no test without a subscriber reaches (the blocks-only disconnects), not shared ones such as
/// `got inv`.
struct NetLines(Mutex<Vec<String>>);

impl tracing::Subscriber for NetLines {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        meta.is_event() && meta.target() == "net"
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Message<'a>(&'a mut String);
        impl tracing::field::Visit for Message<'_> {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    *self.0 = format!("{v:?}");
                }
            }
        }
        let mut line = String::new();
        event.record(&mut Message(&mut line));
        self.0.lock().unwrap().push(line);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

impl NetLines {
    /// How many times `line` was logged.
    fn count(&self, line: &str) -> usize {
        self.0.lock().unwrap().iter().filter(|l| *l == line).count()
    }
}

/// rdashd's default until P5 (DEC-322): `-blocksonly=1`, out of IBD, with mnsync's blockchain
/// sync done and InstantSend at `instantsend_enabled`.
fn blocks_only(instantsend_enabled: bool) -> Sim {
    let opts = ProcessorOptions {
        blocks_only: true,
        ..ProcessorOptions::default()
    };
    let seams = Seams {
        overlay: Box::new(StaticOverlayState {
            governance_ready: true,
            instantsend_enabled,
        }),
        ..Seams::default()
    };
    let sim = sim_reg(opts, owners(&[]), seams);
    sim.chain.set_ibd(false);
    sim
}

/// A peer through the handshake with `version`, its handshake output drained.
fn peer_at(sim: &mut Sim, n: u8, version: i32, permissions: Permissions) -> SimPeer {
    let p = sim.connect(n, ConnectionType::OutboundFullRelay, permissions);
    sim.handshake(
        &p,
        PeerVersionMsg {
            version,
            ..PeerVersionMsg::v24(SIM_START)
        },
    );
    settle(sim);
    sim.sent(&p);
    p
}

const ADDRV2_PROTO_VERSION: i32 = 70223;

/// Core's `netMessageTypesViolateBlocksOnly`, as protocol.cpp:185-212 lists it.
const VIOLATES_BLOCKS_ONLY: [&str; 26] = [
    "dsa",
    "dsc",
    "dsf",
    "dsq",
    "dss",
    "dssu",
    "dstx",
    "dsi",
    "getqrinfo",
    "platformban",
    "qbsigs",
    "qcomplaint",
    "qcontrib",
    "qdata",
    "qgetdata",
    "qgetsigs",
    "qjustify",
    "qpcommit",
    "qsendrecsigs",
    "qsigrec",
    "qsigsesann",
    "qsigshare",
    "qsigsinv",
    "qrinfo",
    "qwatch",
    "tx",
];

/// Under `-blocksonly`, each message in Core's set disconnects its sender before it is read,
/// with Core's line once; a peer with the `relay` permission is exempt (P5a D23;
/// np:4381-4386, :6273-6281; protocol.cpp:185-213, :335-338).
#[test]
fn blocks_only_a_message_in_cores_set_disconnects_with_cores_line() {
    let lines = Arc::new(NetLines(Mutex::default()));
    let _log = tracing::subscriber::set_default(Arc::clone(&lines));
    let mut sim = blocks_only(true);
    for (n, cmd) in (1..).zip(VIOLATES_BLOCKS_ONLY) {
        let p = peer_at(&mut sim, n, 70242, Permissions::NONE);
        sim.deliver(&p, &[(cmd, Vec::new())]);
        settle(&mut sim);
        assert!(p.handle.disconnect_requested(), "{cmd}");
        let line = format!("{cmd} sent in violation of protocol peer={}", p.handle.id());
        assert_eq!(lines.count(&line), 1, "{cmd}");
        assert_eq!(score(&sim, &p), 0, "{cmd}");
    }
    let relayer = peer_at(&mut sim, 100, 70242, Permissions::RELAY);
    sim.deliver(&relayer, &[("tx", Vec::new())]);
    settle(&mut sim);
    assert!(!relayer.handle.disconnect_requested());
    let line = format!("tx sent in violation of protocol peer={}", relayer.handle.id());
    assert_eq!(lines.count(&line), 0);
}

/// Under `-blocksonly`, an `inv` entry whose command is in Core's set disconnects its sender
/// with Core's line, in IBD or not. A peer with the `relay` permission is exempt (P5a D23;
/// np:4547-4551).
#[test]
fn blocks_only_a_tx_or_dash_overlay_inv_disconnects_with_cores_line() {
    const MSG_DSTX: u32 = 16;
    let lines = Arc::new(NetLines(Mutex::default()));
    let _log = tracing::subscriber::set_default(Arc::clone(&lines));
    let mut sim = blocks_only(true);
    let mut hash = [0; 32];
    hash[..2].copy_from_slice(&[0x34, 0x12]);
    let types = [
        (MSG_TX, "tx"),
        (MSG_DSTX, "dstx"),
        // Core names its inventory `tx` (protocol.cpp:303-305).
        (MSG_ASSET_UNLOCK, "tx"),
        (MSG_QUORUM_CONTRIB, "qcontrib"),
        (MSG_QUORUM_COMPLAINT, "qcomplaint"),
        (MSG_QUORUM_JUSTIFICATION, "qjustify"),
        (MSG_QUORUM_PREMATURE_COMMITMENT, "qpcommit"),
        (MSG_QUORUM_RECOVERED_SIG, "qsigrec"),
        (MSG_DSQ, "dsq"),
        (MSG_PLATFORM_BAN, "platformban"),
    ];
    for ((ty, cmd), n) in types.into_iter().zip(1..) {
        sim.chain.set_ibd(n % 2 == 0);
        let p = peer_at(&mut sim, n, 70242, Permissions::NONE);
        announce(&mut sim, &p, &[inv(ty, hash)]);
        assert!(p.handle.disconnect_requested(), "{cmd}");
        let line = format!(
            "{cmd} (0000000000000000000000000000000000000000000000000000000000001234) inv sent in violation of protocol, disconnecting peer={}",
            p.handle.id()
        );
        assert_eq!(lines.count(&line), 1, "{cmd}");
    }
    sim.chain.set_ibd(false);
    let relayer = peer_at(&mut sim, 100, 70242, Permissions::RELAY);
    let tx = inv(MSG_TX, hash);
    announce(&mut sim, &relayer, &[tx]);
    assert!(!relayer.handle.disconnect_requested());
    assert_eq!(getdata(&sim, &relayer, 1), [tx], "requested as Core does");
}

/// Under `-blocksonly`, a new `MSG_ISDLOCK` is ignored from a peer whose common version is at
/// most `ADDRV2_PROTO_VERSION`, and disconnects a newer one with Core's line; a had one
/// (InstantSend off) is neither (P5a D23; np:4553-4573).
#[test]
fn blocks_only_an_isdlock_inv_is_ignored_from_an_old_peer_and_disconnects_a_new_one() {
    let lines = Arc::new(NetLines(Mutex::default()));
    let _log = tracing::subscriber::set_default(Arc::clone(&lines));
    let lock = inv(MSG_ISDLOCK, [31; 32]);
    let violation = |id: PeerId| {
        format!(
            "isdlock ({}) inv sent in violation of protocol, disconnecting peer={id}",
            lock.hash
        )
    };

    let mut sim = blocks_only(true);
    let old = peer_at(&mut sim, 1, ADDRV2_PROTO_VERSION, Permissions::NONE);
    let new = peer_at(&mut sim, 2, ADDRV2_PROTO_VERSION + 1, Permissions::NONE);
    announce(&mut sim, &old, &[lock]);
    announce(&mut sim, &new, &[lock]);
    assert!(!old.handle.disconnect_requested());
    assert!(new.handle.disconnect_requested());
    assert_eq!(lines.count(&violation(old.handle.id())), 0);
    assert_eq!(lines.count(&violation(new.handle.id())), 1);
    sim.clock.advance(Duration::from_secs(120));
    settle(&mut sim);
    assert_eq!(getdata(&sim, &old, 0), [], "never requested");

    let mut sim = blocks_only(false);
    let new = peer_at(&mut sim, 1, 70242, Permissions::NONE);
    announce(&mut sim, &new, &[lock]);
    assert!(!new.handle.disconnect_requested(), "had: the branch is not reached");
}

/// Under `-blocksonly`, the peers that keep to relay 0 announce no transactions, and their
/// InstantSend locks (from a peer at most at `ADDRV2_PROTO_VERSION`) are never requested: the
/// tracker holds only what has an answer before P5 (P5a D20, D23).
#[test]
fn blocks_only_tx_and_isdlock_invs_from_a_respecting_peer_produce_no_request() {
    let mut sim = blocks_only(true);
    let old = peer_at(&mut sim, 1, ADDRV2_PROTO_VERSION, Permissions::NONE);
    let new = peer_at(&mut sim, 2, 70242, Permissions::NONE);
    let locks: Vec<_> = (0..10u8).map(|i| inv(MSG_ISDLOCK, [i; 32])).collect();
    announce(&mut sim, &old, &locks);
    let (_, cl) = clsig(10);
    let rest = [inv(MSG_SPORK, [6; 32]), cl];
    announce(&mut sim, &new, &rest);
    assert_eq!(getdata(&sim, &new, 1), rest);
    for _ in 0..4 {
        sim.clock.advance(Duration::from_secs(30));
        settle(&mut sim);
        assert_eq!(getdata(&sim, &old, 0), []);
    }
    assert!(!old.handle.disconnect_requested() && !new.handle.disconnect_requested());
}
