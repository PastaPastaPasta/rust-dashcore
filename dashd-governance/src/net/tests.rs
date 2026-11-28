//! `NetGovernance` in the processor's sim, over the serial executor and the fake index: the
//! `govsync` replies and their tracking, the inert gate, admission outside the peer region, the
//! vote outcomes, and `AlreadyHave` and `getdata`.
// Core: governance/net_governance.cpp v24.0.2:80-303

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use dash_dml::MnList;
use dash_governance::manager::{GovClock, Manager};
use dash_governance::{object, vote};
use dash_p2p::{CommandString, ConnectionType, NetCore, Permissions};
use dash_p2p_core::{FilterLoad, GetData, Inv, NotFound};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{VoteOutcome, VoteSignal};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::bloom::BLOOM_UPDATE_ALL;
use dashd_p2p::processing::peerman::Shared as PeerShared;
use dashd_p2p::processing::seams::StaticOverlayState;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer, VirtualClock};
use dashd_p2p::{ProcessorOptions, Registry, Seams};

use super::*;
use crate::exec::GovCommand;
use crate::exec::fakes::{FakeChain, FakeEnv, RecordingSink, SerialExec};
use crate::tests::{
    Event, FakeIndex, block, chain_at, clock, io_error, logged, operator_vote, params, proposal,
    take_events, three_masternodes, trigger,
};

/// The manager's and mnsync's state, settable, with the bumps it got.
#[derive(Default)]
pub(crate) struct State {
    valid: AtomicBool,
    chain: AtomicBool,
    synced: AtomicBool,
    bumps: Mutex<Vec<String>>,
}

impl State {
    pub(crate) fn new(valid: bool, chain: bool, synced: bool) -> Arc<Self> {
        let s = Arc::new(Self::default());
        s.set(valid, chain, synced);
        s
    }

    pub(crate) fn set(&self, valid: bool, chain: bool, synced: bool) {
        self.valid.store(valid, Ordering::SeqCst);
        self.chain.store(chain, Ordering::SeqCst);
        self.synced.store(synced, Ordering::SeqCst);
    }

    pub(crate) fn take_bumps(&self) -> Vec<String> {
        std::mem::take(&mut *self.bumps.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl GovSyncState for State {
    fn is_valid(&self) -> bool {
        self.valid.load(Ordering::SeqCst)
    }

    fn is_blockchain_synced(&self) -> bool {
        self.chain.load(Ordering::SeqCst)
    }

    fn is_synced(&self) -> bool {
        self.synced.load(Ordering::SeqCst)
    }

    fn bump_asset_last_time(&self, func: &str) {
        self.bumps.lock().unwrap_or_else(PoisonError::into_inner).push(func.to_owned());
    }
}

/// A settable DMN-tip list.
pub(crate) struct Tip(Mutex<Arc<MnList>>);

impl Tip {
    pub(crate) fn new(list: MnList) -> Arc<Self> {
        Arc::new(Self(Mutex::new(Arc::new(list))))
    }

    fn set(&self, list: MnList) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(list);
    }
}

impl DmnTip for Tip {
    fn dmn_tip_list(&self) -> Arc<MnList> {
        Arc::clone(&self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Sends through the sim's processor once it exists, recording each, as dashd-node's sends go
/// through its processor's shared half.
#[derive(Default)]
pub(crate) struct Peers {
    net: OnceLock<(Arc<NetCore>, Arc<PeerShared>)>,
    pub(crate) relay_peers: Mutex<Vec<i64>>,
    pub(crate) pushed: Mutex<Vec<(i64, OutMsg)>>,
    pub(crate) relayed: Mutex<Vec<Inventory>>,
}

impl GovPeers for Peers {
    fn push_inventory(&self, peer: i64, inv: &Inventory) {
        if let Some((_, shared)) = self.net.get() {
            shared.push_inventory(peer, inv);
        }
    }

    fn relay_inv(&self, inv: &Inventory) {
        self.relayed.lock().unwrap_or_else(PoisonError::into_inner).push(*inv);
    }

    fn forget_object_request(&self, inv: &Inventory) {
        if let Some((_, shared)) = self.net.get() {
            shared.forget_object_request(inv);
        }
    }

    fn relay_peers(&self) -> Vec<i64> {
        self.relay_peers.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn push(&self, peer: i64, msg: OutMsg) {
        self.pushed.lock().unwrap_or_else(PoisonError::into_inner).push((peer, msg));
    }
}

/// The handler's parts, before it is built over them.
pub(crate) struct Parts<E> {
    pub(crate) state: Arc<State>,
    pub(crate) env: Arc<FakeEnv>,
    pub(crate) index: Arc<FakeIndex>,
    pub(crate) sink: Arc<RecordingSink>,
    pub(crate) tip: Arc<Tip>,
    pub(crate) relay: Arc<RelayInvs>,
    pub(crate) peers: Arc<Peers>,
    pub(crate) handle: Arc<GovernanceHandle<E>>,
}

impl<E: GovExec + 'static> Parts<E> {
    pub(crate) fn new(exec: E) -> Self {
        let env = Arc::new(FakeEnv::new(clock()));
        let sink = Arc::new(RecordingSink::default());
        let index = Arc::new(FakeIndex::default());
        let handle = GovernanceHandle::new(
            Manager::new(),
            exec,
            env.clone(),
            sink.clone(),
            index.clone(),
            params(),
        );
        Self {
            state: State::new(true, true, true),
            env,
            index,
            sink,
            tip: Tip::new(three_masternodes()),
            relay: Arc::default(),
            peers: Arc::default(),
            handle,
        }
    }

    pub(crate) fn net(&self) -> NetGovernance<E> {
        let fulfilled =
            Arc::new(NetFulfilledRequestManager::new(VirtualClock::at(SIM_START), 3600));
        NetGovernance::new(
            Arc::clone(&self.handle),
            self.state.clone(),
            self.tip.clone(),
            fulfilled,
            self.peers.clone(),
            Arc::clone(&self.relay),
            self.sink.clone(),
        )
    }

    /// Both the handler's state and the commands' environment.
    pub(crate) fn set_sync(&self, valid: bool, chain: bool, synced: bool) {
        self.state.set(valid, chain, synced);
        self.env.set(clock(), synced, chain);
    }
}

type Serial = Arc<SerialExec>;

struct Fx<E = Serial> {
    sim: Sim,
    parts: Parts<E>,
    /// An outbound announcer.
    a: SimPeer,
    /// An inbound peer.
    b: SimPeer,
}

/// A chain at 100 with `testkit::numbered(1..=3)` in the list, synced, and the handler in the
/// sim's registry.
fn fx() -> Fx {
    let exec = Arc::new(SerialExec::new(chain_at(100)));
    exec.chain().list = Arc::new(three_masternodes());
    sim_over(Parts::new(exec))
}

fn sim_over<E: GovExec + 'static>(parts: Parts<E>) -> Fx<E> {
    let mut registry = Registry::new();
    registry.register(Box::new(parts.net()));
    let p = Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let seams = Seams {
        overlay: Arc::new(StaticOverlayState {
            governance_ready: true,
            instantsend_enabled: false,
        }),
        ..Seams::default()
    };
    let mut sim = Sim::with(p, id, ProcessorOptions::default(), registry, seams, |_| {});
    sim.chain.set_ibd(false);
    let _ = parts.peers.net.set((Arc::clone(&sim.core), Arc::clone(&sim.shared)));
    let a = peer(&mut sim, 1, ConnectionType::OutboundFullRelay);
    let b = peer(&mut sim, 2, ConnectionType::Inbound);
    Fx {
        sim,
        parts,
        a,
        b,
    }
}

fn peer(sim: &mut Sim, n: u8, conn_type: ConnectionType) -> SimPeer {
    let p = sim.connect(n, conn_type, Permissions::NONE);
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    settle(sim);
    sim.sent(&p);
    p
}

fn settle(sim: &mut Sim) {
    while sim.pass() {}
    sim.pass();
}

/// Past every inventory trickle.
fn trickle(sim: &mut Sim) {
    sim.clock.advance(Duration::from_secs(60));
    settle(sim);
}

fn encode<T: BaseCodec>(m: &T) -> Vec<u8> {
    let mut out = Vec::new();
    m.encode(&mut out);
    out
}

impl<E> Fx<E> {
    fn deliver(&mut self, to_b: bool, cmd: &str, payload: Vec<u8>) {
        let p = if to_b {
            &self.b
        } else {
            &self.a
        };
        self.sim.deliver(p, &[(cmd, payload)]);
        settle(&mut self.sim);
    }

    fn score(&self, p: &SimPeer) -> i32 {
        self.sim.shared.misbehavior(p.handle.id()).unwrap().0
    }

    fn announced(&self, p: &SimPeer) -> usize {
        self.sim.lp.logic().requested_objects().count(p.handle.id())
    }
}

/// Every message `p` was sent since the last read, as (command, payload).
fn sent(sim: &Sim, p: &SimPeer) -> Vec<(CommandString, Vec<u8>)> {
    sim.sent(p)
}

fn invs(payload: &[u8]) -> Vec<Inventory> {
    Inv::decode(&mut &payload[..]).unwrap().inventory().clone()
}

/// The governance invs among `msgs`, in order.
fn gov_invs(msgs: &[(CommandString, Vec<u8>)]) -> Vec<Inventory> {
    msgs.iter()
        .filter(|(c, _)| *c == CommandString::INV)
        .flat_map(|(_, p)| invs(p))
        .filter(|i| {
            matches!(i.inv_type(), InvType::GovernanceObject | InvType::GovernanceObjectVote)
        })
        .collect()
}

fn govsync(hash: [u8; 32], filter: FilterLoad) -> Vec<u8> {
    encode(&GovSync::new(Hash256::from_lendian(hash), filter))
}

fn filter_with(hashes: &[[u8; 32]]) -> FilterLoad {
    let mut f = BloomFilter::new(10, 0.001, 0, BLOOM_UPDATE_ALL);
    for h in hashes {
        f.insert(h);
    }
    f.to_msg()
}

/// A held proposal (collateral six deep at 100), with two accepted operator votes.
fn held_with_votes<E: GovExec + 'static>(parts: &Parts<E>) -> ([u8; 32], [[u8; 32]; 2]) {
    let (obj, tx) = proposal(1);
    parts.index.put(&tx, block(95));
    assert!(parts.handle.admit(obj.clone()).wait().unwrap().outcome.unwrap().held);
    let parent = object::hash(&obj);
    let list = three_masternodes();
    let votes = [1, 2].map(|n| {
        let v = operator_vote(n, parent);
        assert_eq!(parts.handle.process_vote(&v, &list).0.result, Ok(true));
        vote::hash(&v)
    });
    parts.sink.take();
    (parent, votes)
}

fn ssc(msgs: &[(CommandString, Vec<u8>)]) -> Vec<(usize, Ssc)> {
    msgs.iter()
        .enumerate()
        .filter(|(_, (c, _))| *c == CommandString::SSC)
        .map(|(i, (_, p))| (i, Ssc::decode(&mut &p[..]).unwrap()))
        .collect()
}

fn first_inv(msgs: &[(CommandString, Vec<u8>)]) -> Option<usize> {
    msgs.iter().position(|(c, p)| {
        *c == CommandString::INV
            && invs(p).iter().any(|i| {
                i.inv_type() == InvType::GovernanceObject
                    || i.inv_type() == InvType::GovernanceObjectVote
            })
    })
}

#[test]
fn a_full_sync_sends_ssc_then_every_syncable_object_to_the_requester_only() {
    let mut f = fx();
    let objects: Vec<_> = (1..=2)
        .map(|n| {
            let (obj, tx) = proposal(n);
            f.parts.index.put(&tx, block(95));
            assert!(f.parts.handle.admit(obj.clone()).wait().unwrap().outcome.unwrap().held);
            object_inv(object::hash(&obj))
        })
        .collect();
    f.deliver(true, "govsync", govsync([0; 32], empty_bloom_filter()));
    trickle(&mut f.sim);
    let to_b = sent(&f.sim, &f.b);
    let counts = ssc(&to_b);
    assert_eq!(counts.len(), 1);
    assert_eq!((counts[0].1.item_id(), counts[0].1.count()), (MASTERNODE_SYNC_GOVOBJ, 2));
    assert!(counts[0].0 < first_inv(&to_b).expect("the invs"));
    let mut got = gov_invs(&to_b);
    got.sort_by_key(|i| i.inv_hash().to_lendian());
    let mut want = objects;
    want.sort_by_key(|i| i.inv_hash().to_lendian());
    assert_eq!(got, want);
    assert!(gov_invs(&sent(&f.sim, &f.a)).is_empty());
    assert_eq!(f.score(&f.b), 0);
}

#[test]
fn a_vote_sync_sends_ssc_then_the_votes_its_filter_lacks_to_the_requester_only() {
    let mut f = fx();
    let (parent, [first, second]) = held_with_votes(&f.parts);
    f.deliver(true, "govsync", govsync(parent, filter_with(&[first])));
    trickle(&mut f.sim);
    let to_b = sent(&f.sim, &f.b);
    let counts = ssc(&to_b);
    assert_eq!(counts.len(), 1);
    assert_eq!((counts[0].1.item_id(), counts[0].1.count()), (MASTERNODE_SYNC_GOVOBJ_VOTE, 1));
    assert!(counts[0].0 < first_inv(&to_b).expect("the inv"));
    assert_eq!(gov_invs(&to_b), [vote_inv(second)]);
    assert!(gov_invs(&sent(&f.sim, &f.a)).is_empty());
}

#[test]
fn a_vote_sync_checks_signatures_outside_the_lock_and_answers_a_repeat_from_the_memo() {
    let mut f = fx();
    let (parent, _) = held_with_votes(&f.parts);
    take_events();
    f.deliver(true, "govsync", govsync(parent, filter_with(&[])));
    let events = take_events();
    assert_eq!(events.iter().filter(|e| **e == Event::Verify).count(), 2);
    let mut held = false;
    for e in &events {
        match e {
            Event::Lock(_) => held = true,
            Event::Unlock => held = false,
            Event::Verify => assert!(!held, "a signature checked under the lock: {events:?}"),
            _ => {}
        }
    }
    f.deliver(false, "govsync", govsync(parent, filter_with(&[])));
    assert!(!take_events().contains(&Event::Verify), "the memo answered");
    trickle(&mut f.sim);
    let to_b = gov_invs(&sent(&f.sim, &f.b));
    assert_eq!(to_b.len(), 2);
    assert_eq!(gov_invs(&sent(&f.sim, &f.a)), to_b);
}

#[test]
fn a_vote_sync_skips_votes_invalid_against_the_tip_list() {
    let mut f = fx();
    let (parent, _) = held_with_votes(&f.parts);
    f.parts.tip.set(MnList::new(block(0), 0, 0));
    f.deliver(true, "govsync", govsync(parent, filter_with(&[])));
    trickle(&mut f.sim);
    let to_b = sent(&f.sim, &f.b);
    assert_eq!(ssc(&to_b)[0].1.count(), 0);
    assert!(gov_invs(&to_b).is_empty());
}

#[test]
fn an_object_fetch_is_one_direct_inv_only_when_fetchable_and_untracked() {
    let mut f = fx();
    let (parent, _) = held_with_votes(&f.parts);
    for _ in 0..2 {
        f.deliver(true, "govsync", govsync(parent, empty_bloom_filter()));
        let to_b = sent(&f.sim, &f.b);
        assert!(ssc(&to_b).is_empty());
        let direct: Vec<_> = to_b.iter().filter(|(c, _)| *c == CommandString::INV).collect();
        assert_eq!(direct.len(), 1);
        assert_eq!(invs(&direct[0].1), [object_inv(parent)]);
    }
    f.deliver(true, "govsync", govsync([7; 32], empty_bloom_filter()));
    trickle(&mut f.sim);
    let to_b = sent(&f.sim, &f.b);
    assert!(ssc(&to_b).is_empty() && gov_invs(&to_b).is_empty());
    assert_eq!(f.score(&f.b), 0);
}

#[test]
fn a_tracked_repeat_scores_20_and_an_unknown_parents_vote_sync_repeats_unscored() {
    let mut f = fx();
    let (parent, _) = held_with_votes(&f.parts);
    for _ in 0..3 {
        f.deliver(true, "govsync", govsync([9; 32], filter_with(&[])));
    }
    assert_eq!(f.score(&f.b), 0);
    f.deliver(true, "govsync", govsync([0; 32], empty_bloom_filter()));
    assert_eq!(f.score(&f.b), 0);
    f.deliver(true, "govsync", govsync([0; 32], empty_bloom_filter()));
    assert_eq!(f.score(&f.b), 20);
    f.deliver(true, "govsync", govsync(parent, filter_with(&[])));
    assert_eq!(f.score(&f.b), 20);
    f.deliver(true, "govsync", govsync(parent, filter_with(&[])));
    assert_eq!(f.score(&f.b), 40);
    sent(&f.sim, &f.b);
    // The refused repeat replies nothing.
    f.deliver(true, "govsync", govsync(parent, filter_with(&[])));
    assert!(ssc(&sent(&f.sim, &f.b)).is_empty());
}

#[test]
fn oversized_and_undecodable_filters_score_100_and_a_short_hash_is_only_logged() {
    // `nHashFuncs` over 50.
    let mut f = fx();
    f.deliver(true, "govsync", govsync([0; 32], FilterLoad::new(vec![0; 8], 51, 0, 0)));
    assert_eq!(f.score(&f.b), 100);
    // `vData` over 36000 bytes, refused before it is read.
    let mut f = fx();
    let mut p = vec![0u8; 32];
    p.extend([0xfe, 0x41, 0x8c, 0, 0]);
    f.deliver(true, "govsync", p);
    assert_eq!(f.score(&f.b), 100);
    // A filter cut short.
    let mut f = fx();
    let mut p = vec![0u8; 32];
    p.extend([5, 1, 2]);
    f.deliver(true, "govsync", p);
    assert_eq!(f.score(&f.b), 100);
    // The hash cut short: the outer catch.
    let mut f = fx();
    assert!(f.sim.admitted(&f.b, "govsync", vec![0; 31]).is_err());
    assert_eq!(f.score(&f.b), 0);
}

#[test]
fn govsync_is_ignored_unread_until_synced() {
    let mut f = fx();
    held_with_votes(&f.parts);
    f.parts.set_sync(true, true, false);
    for _ in 0..2 {
        f.deliver(true, "govsync", govsync([0; 32], empty_bloom_filter()));
        f.deliver(true, "govsync", vec![1, 2, 3]);
    }
    trickle(&mut f.sim);
    let to_b = sent(&f.sim, &f.b);
    assert!(ssc(&to_b).is_empty() && gov_invs(&to_b).is_empty());
    assert_eq!(f.score(&f.b), 0);
}

/// A vote whose signature is 10 bytes: refused by the read.
fn malformed_vote() -> Vec<u8> {
    let mut v = encode(&operator_vote(1, [3; 32]));
    v.truncate(84);
    v.push(10);
    v.extend([0; 10]);
    v
}

/// An object cut inside its data.
fn malformed_object() -> Vec<u8> {
    let mut o = encode(&proposal(1).0);
    o.truncate(90);
    o
}

#[test]
fn the_gate_leaves_malformed_messages_unread_while_disabled_or_unsynced() {
    for (valid, chain) in [(false, true), (true, false), (false, false)] {
        let mut f = fx();
        f.parts.set_sync(valid, chain, false);
        let before = f.parts.handle.published();
        for p in [&f.a, &f.b] {
            assert!(f.sim.admitted(p, "govobj", malformed_object()).is_ok());
            assert!(f.sim.admitted(p, "govobjvote", malformed_vote()).is_ok());
            assert!(f.sim.admitted(p, "govsync", vec![1]).is_ok());
        }
        settle(&mut f.sim);
        for p in [&f.a, &f.b] {
            assert_eq!(f.score(p), 0);
            assert!(sent(&f.sim, p).is_empty());
        }
        assert!(Arc::ptr_eq(&before, &f.parts.handle.published()));
        assert!(f.parts.handle.lock().objects().is_empty());
        assert!(f.parts.state.take_bumps().is_empty());
        assert!(f.parts.sink.take().is_empty());
    }
}

#[test]
fn a_malformed_vote_scores_100_and_a_malformed_object_is_unscored_and_logged() {
    for synced in [true, false] {
        let mut f = fx();
        f.parts.set_sync(true, true, synced);
        f.deliver(false, "govobjvote", malformed_vote());
        assert_eq!(f.score(&f.a), 100);
        assert!(f.sim.admitted(&f.b, "govobj", malformed_object()).is_err());
        let ((), lines) = logged(|| f.deliver(true, "govobj", malformed_object()));
        assert!(
            lines.iter().any(|(_, m)| m.contains("ProcessMessages(govobj") && m.contains("caught"))
        );
        assert_eq!(f.score(&f.b), 0);
        assert!(sent(&f.sim, &f.b).is_empty());
    }
}

#[test]
fn an_object_signature_over_96_bytes_throws_to_the_outer_catch() {
    let mut f = fx();
    let mut long = encode(&proposal(1).0);
    assert_eq!(long.pop(), Some(0), "an unsigned object ends in its empty vchSig");
    long.push(97);
    long.extend([0; 97]);
    assert!(f.sim.admitted(&f.b, "govobj", long).is_err());
    assert_eq!(f.score(&f.b), 0);
}

/// `a` then `b` announce `inv`; `a` is asked for it.
fn both_announce<E>(f: &mut Fx<E>, inv: Inventory) {
    let before = [f.announced(&f.a), f.announced(&f.b)];
    for to_b in [false, true] {
        f.deliver(to_b, "inv", encode(&Inv::new(vec![inv])));
    }
    let asked: Vec<_> = sent(&f.sim, &f.a)
        .into_iter()
        .filter(|(c, _)| *c == CommandString::GETDATA)
        .flat_map(|(_, p)| GetData::decode(&mut &p[..]).unwrap().inventory().clone())
        .collect();
    assert_eq!(asked, [inv]);
    assert_eq!([f.announced(&f.a), f.announced(&f.b)], before.map(|n| n + 1));
}

#[test]
fn an_unrequested_object_never_reaches_the_manager() {
    let mut f = fx();
    let (obj, tx) = proposal(1);
    f.parts.index.put(&tx, block(95));
    f.deliver(true, "govobj", encode(&obj));
    assert!(f.parts.handle.lock().objects().is_empty());
    assert!(f.parts.index.reads().is_empty());
    assert_eq!(f.score(&f.b), 0);
}

/// What one requested object's delivery did: (`b`'s announcement forgotten, the bumps, `a`'s
/// score, the relay queue).
fn admit_requested(f: &mut Fx, obj: &GovObject) -> (bool, Vec<String>, i32, Vec<Inventory>) {
    let inv = object_inv(object::hash(obj));
    both_announce(f, inv);
    f.parts.state.take_bumps();
    f.deliver(false, "govobj", encode(obj));
    (f.announced(&f.b) == 0, f.parts.state.take_bumps(), f.score(&f.a), f.parts.relay.take())
}

const ADDED: &str = "CGovernanceManager::AddGovernanceObject";

#[test]
fn a_kept_object_forgets_bumps_and_queues_its_relay() {
    let mut f = fx();
    let (obj, tx) = proposal(1);
    f.parts.index.put(&tx, block(95));
    let inv = object_inv(object::hash(&obj));
    assert_eq!(admit_requested(&mut f, &obj), (true, vec![ADDED.to_owned()], 0, vec![inv]));
    assert!(f.parts.handle.lock().object(&object::hash(&obj)).is_some());
}

#[test]
fn a_postponed_object_forgets_without_a_bump() {
    let mut f = fx();
    let (obj, tx) = proposal(1);
    f.parts.index.put(&tx, block(100));
    assert_eq!(admit_requested(&mut f, &obj), (true, vec![], 0, vec![]));
    assert!(f.parts.handle.lock().postponed().contains_key(&object::hash(&obj)));
}

#[test]
fn a_collateral_miss_or_an_invalid_object_scores_20_and_keeps_the_announcements() {
    let mut f = fx();
    let (obj, _) = proposal(1);
    assert_eq!(admit_requested(&mut f, &obj), (false, vec![], 20, vec![]));
    let mut f = fx();
    let bad = crate::tests::gov_object(object::PROPOSAL, b"not json", [4; 32]);
    assert_eq!(admit_requested(&mut f, &bad), (false, vec![], 20, vec![]));
}

#[test]
fn an_index_fault_is_unscored_and_logged_at_error_level() {
    let mut f = fx();
    let (obj, tx) = proposal(1);
    f.parts.index.set(object::txid_bytes(&tx.txid()), Err(io_error()));
    let (out, lines) = logged(|| admit_requested(&mut f, &obj));
    assert_eq!(out, (false, vec![], 0, vec![]));
    assert!(lines.iter().any(|(level, m)| {
        *level == tracing::Level::ERROR
            && m.contains("MNGOVERNANCEOBJECT")
            && m.contains("not scored")
    }));
}

#[test]
fn a_duplicate_object_forgets_without_a_bump() {
    let mut f = fx();
    let (obj, tx) = proposal(1);
    f.parts.index.put(&tx, block(95));
    both_announce(&mut f, object_inv(object::hash(&obj)));
    assert!(f.parts.handle.admit(obj.clone()).wait().unwrap().outcome.unwrap().held);
    f.parts.state.take_bumps();
    f.deliver(false, "govobj", encode(&obj));
    assert_eq!((f.announced(&f.b), f.parts.state.take_bumps(), f.score(&f.a)), (0, vec![], 0));
}

#[test]
fn a_rate_limited_trigger_forgets_without_a_bump() {
    let mut f = fx();
    let obj = trigger(3, [5; 32], 110);
    // Created more than two superblock cycles before the adjusted time.
    let cycle = i64::from(params().superblock_cycle()) * params().target_spacing();
    let late = clock().adjusted + 2 * cycle + 100;
    f.parts.env.set(GovClock::new(clock().now, late), true, true);
    assert_eq!(admit_requested(&mut f, &obj), (true, vec![], 0, vec![]));
    let m = f.parts.handle.lock();
    assert!(m.object(&object::hash(&obj)).is_none() && m.postponed().is_empty());
}

#[test]
fn an_admissions_meta_reaches_the_effects_worker() {
    let mut f = fx();
    let (obj, tx) = proposal(1);
    f.parts.index.put(&tx, block(95));
    let parent = object::hash(&obj);
    let orphan = operator_vote(1, parent);
    f.parts.handle.process_vote(&orphan, &three_masternodes());
    admit_requested(&mut f, &obj);
    let meta: Vec<_> = f.parts.sink.take().into_iter().flat_map(|e| e.meta).collect();
    assert_eq!(meta.len(), 1);
}

/// `a` announces the vote, is asked for it and sends it.
fn vote_requested<E>(f: &mut Fx<E>, v: &GovVote) {
    let inv = vote_inv(vote::hash(v));
    both_announce(f, inv);
    f.parts.state.take_bumps();
    f.deliver(false, "govobjvote", encode(v));
}

#[test]
fn an_accepted_vote_forgets_bumps_and_is_relayed_only_when_synced() {
    for synced in [true, false] {
        let mut f = fx();
        let (parent, _) = held_with_votes(&f.parts);
        f.parts.set_sync(true, true, synced);
        let v = operator_vote(3, parent);
        vote_requested(&mut f, &v);
        assert_eq!(f.parts.state.take_bumps(), ["MNGOVERNANCEOBJECTVOTE"]);
        assert_eq!(f.announced(&f.b), 0);
        let want = if synced {
            vec![vote_inv(vote::hash(&v))]
        } else {
            vec![]
        };
        assert_eq!(f.parts.relay.take(), want, "synced {synced}");
        assert_eq!(f.score(&f.a), 0);
        assert_eq!(f.parts.sink.take().len(), 1, "the vote's meta");
    }
}

#[test]
fn an_orphan_vote_requests_its_parent_once_and_is_kept_without_a_forget() {
    let mut f = fx();
    let (obj, _) = proposal(1);
    let parent = object::hash(&obj);
    for n in [1u8, 2] {
        vote_requested(&mut f, &operator_vote(u32::from(n), parent));
        let asks: Vec<_> = sent(&f.sim, &f.a)
            .into_iter()
            .filter(|(c, _)| *c == CommandString::GOVSYNC)
            .map(|(_, p)| GovSync::decode(&mut &p[..]).unwrap())
            .collect();
        let want = if n == 1 {
            vec![GovSync::new(Hash256::from_lendian(parent), empty_bloom_filter())]
        } else {
            vec![]
        };
        assert_eq!(asks, want);
        assert!(f.parts.state.take_bumps().is_empty());
        assert_eq!(f.announced(&f.b), usize::from(n), "no forget");
        assert_eq!(f.score(&f.a), 0);
    }
    assert!(f.parts.handle.lock().orphan(&parent).is_some());
}

#[test]
fn a_duplicate_vote_neither_forgets_nor_bumps() {
    let mut f = fx();
    let (parent, _) = held_with_votes(&f.parts);
    let v = operator_vote(3, parent);
    vote_requested(&mut f, &v);
    // `b` sends it too, unsolicited by then: its announcement was forgotten.
    f.deliver(true, "inv", encode(&Inv::new(vec![vote_inv(vote::hash(&v))])));
    assert_eq!(f.announced(&f.b), 0, "had: not announced");
    f.parts.state.take_bumps();
    f.deliver(true, "govobjvote", encode(&v));
    assert!(f.parts.state.take_bumps().is_empty());
    assert_eq!(f.score(&f.b), 0);
}

#[test]
fn a_bad_signature_scores_20_only_when_synced() {
    for (synced, score) in [(true, 20), (false, 0)] {
        let mut f = fx();
        let (parent, _) = held_with_votes(&f.parts);
        f.parts.set_sync(true, true, synced);
        let good = operator_vote(3, parent);
        let bad = GovVote::new(
            good.masternode_outpoint(),
            good.parent_hash(),
            VoteOutcome::new(vote::OUTCOME_NO as u32),
            VoteSignal::new(vote::SIGNAL_VALID as u32),
            good.time(),
            good.sig().clone(),
        );
        vote_requested(&mut f, &bad);
        assert_eq!(f.score(&f.a), score);
        assert!(f.parts.state.take_bumps().is_empty());
        assert_eq!(f.announced(&f.b), 1);
    }
}

#[test]
fn already_have_reads_validity_then_the_manager() {
    let f = fx();
    let (parent, [first, _]) = held_with_votes(&f.parts);
    let net = f.parts.net();
    let unknown = [object_inv([8; 32]), vote_inv([8; 32])];
    let held = [object_inv(parent), vote_inv(first)];
    for inv in unknown {
        assert!(!net.already_have(&inv));
    }
    for inv in held {
        assert!(net.already_have(&inv));
    }
    f.parts.set_sync(false, true, true);
    for inv in unknown.into_iter().chain(held) {
        assert!(net.already_have(&inv));
    }
    let tx = Inventory::new(InvType::Tx, Hash256::from_lendian([8; 32]));
    assert!(!net.already_have(&tx));
}

/// `b` asks for `inv`: the governance message it got, if any.
fn getdata<E>(f: &mut Fx<E>, inv: Inventory) -> Option<(CommandString, Vec<u8>)> {
    f.deliver(true, "getdata", encode(&GetData::new(vec![inv])));
    let to_b = sent(&f.sim, &f.b);
    let notfound = to_b.iter().any(|(c, p)| {
        *c == CommandString::NOTFOUND
            && NotFound::decode(&mut &p[..]).unwrap().inventory().contains(&inv)
    });
    let found: Vec<_> = to_b
        .into_iter()
        .filter(|(c, _)| *c == CommandString::GOVOBJ || *c == CommandString::GOVOBJVOTE)
        .collect();
    assert!(found.len() <= 1);
    assert_eq!(notfound, found.is_empty());
    found.into_iter().next()
}

#[test]
fn getdata_serves_held_and_postponed_objects_and_held_votes() {
    let mut f = fx();
    let (parent, [first, _]) = held_with_votes(&f.parts);
    let (later, later_tx) = proposal(2);
    f.parts.index.put(&later_tx, block(100));
    assert!(f.parts.handle.admit(later.clone()).wait().unwrap().outcome.unwrap().postponed);
    let held = f.parts.handle.lock().object_for_hash(&parent).cloned().unwrap();
    assert_eq!(getdata(&mut f, object_inv(parent)), Some((CommandString::GOVOBJ, encode(&held))));
    let postponed = (CommandString::GOVOBJ, encode(&later));
    assert_eq!(getdata(&mut f, object_inv(object::hash(&later))), Some(postponed));
    let vote = encode(&operator_vote(1, parent));
    assert_eq!(vote::hash(&GovVote::decode(&mut &vote[..]).unwrap()), first);
    assert_eq!(getdata(&mut f, vote_inv(first)), Some((CommandString::GOVOBJVOTE, vote)));
    assert_eq!(getdata(&mut f, object_inv([8; 32])), None);
    assert_eq!(getdata(&mut f, vote_inv([8; 32])), None);
}

#[test]
fn getdata_serves_nothing_from_a_disabled_manager() {
    let mut f = fx();
    f.parts.set_sync(false, false, false);
    let (obj, _) = proposal(1);
    assert_eq!(getdata(&mut f, object_inv(object::hash(&obj))), None);
    assert_eq!(getdata(&mut f, vote_inv(vote::hash(&operator_vote(1, [1; 32])))), None);
}

/// V on its own thread: each command waits there until the test lets it run, after V has
/// visited the sender's header cell, as `NewPoWValidBlock` does inside an accept.
struct OnV {
    tx: Mutex<mpsc::Sender<GovCommand>>,
}

impl GovExec for OnV {
    fn post(&self, command: GovCommand) {
        self.tx.lock().unwrap_or_else(PoisonError::into_inner).send(command).unwrap();
    }
}

#[test]
fn an_admission_waits_for_v_outside_the_region_while_v_visits_the_peers_header_cell() {
    let (tx, rx) = mpsc::channel::<GovCommand>();
    let parts = Parts::new(Arc::new(OnV {
        tx: Mutex::new(tx),
    }));
    let (obj, tx_collateral) = proposal(1);
    parts.index.put(&tx_collateral, block(95));
    let mut f = sim_over(parts);
    let shared = Arc::clone(&f.sim.shared);
    let a = f.a.handle.id();
    let (visited_tx, visited) = mpsc::channel();
    let v = thread::spawn(move || {
        let mut chain = FakeChain::new(100, three_masternodes());
        chain.active = (1..=100).map(|h| (block(h), u32::from(h))).collect();
        let command = rx.recv().unwrap();
        // The block's visit of `a`'s cell: it waits while a region holds it.
        let (peeked, done) = mpsc::channel();
        let shared = Arc::clone(&shared);
        thread::spawn(move || {
            let _ = peeked.send(shared.peek_headers(a).is_some());
        });
        let visit = done.recv_timeout(Duration::from_secs(30));
        visited_tx.send(visit).unwrap();
        command(&chain);
    });
    both_announce(&mut f, object_inv(object::hash(&obj)));
    f.deliver(false, "govobj", encode(&obj));
    v.join().unwrap();
    assert_eq!(visited.recv().unwrap(), Ok(true), "V visited the cell while the admission waited");
    assert!(f.parts.handle.lock().object(&object::hash(&obj)).is_some());
    assert_eq!(f.parts.state.take_bumps(), [ADDED]);
    assert_eq!(f.announced(&f.b), 0, "forgotten when V answered");
    assert_eq!(f.score(&f.a), 0);
}

/// How V cuts the admission's wait short before it runs the command.
#[derive(Clone, Copy, Debug)]
enum Cut {
    Disconnect,
    Interrupt,
}

#[test]
fn an_admission_whose_wait_is_cut_short_keeps_its_effects_and_forgets_the_request() {
    for cut in [Cut::Disconnect, Cut::Interrupt] {
        let (tx, rx) = mpsc::channel::<GovCommand>();
        let parts = Parts::new(Arc::new(OnV {
            tx: Mutex::new(tx),
        }));
        let (obj, tx_collateral) = proposal(1);
        parts.index.put(&tx_collateral, block(95));
        let parent = object::hash(&obj);
        parts.handle.process_vote(&operator_vote(1, parent), &three_masternodes());
        let mut f = sim_over(parts);
        let (core, shared, a) =
            (Arc::clone(&f.sim.core), Arc::clone(&f.sim.shared), f.a.handle.id());
        let v = thread::spawn(move || {
            let mut chain = FakeChain::new(100, three_masternodes());
            chain.active = (1..=100).map(|h| (block(h), u32::from(h))).collect();
            let command = rx.recv().unwrap();
            match cut {
                Cut::Disconnect => core.disconnect(a, dash_p2p::Why::Processor("test")),
                Cut::Interrupt => shared.interrupt(),
            }
            command(&chain);
        });
        both_announce(&mut f, object_inv(parent));
        f.parts.sink.take();
        f.deliver(false, "govobj", encode(&obj));
        v.join().unwrap();
        settle(&mut f.sim);
        assert!(f.parts.handle.lock().object(&parent).is_some(), "{cut:?}");
        let sunk = f.parts.sink.take();
        assert_eq!(sunk.len(), 1, "{cut:?}");
        assert_eq!(sunk[0].relay_objects, [parent], "{cut:?}");
        assert_eq!(sunk[0].sync_bumps, [ADDED], "{cut:?}");
        assert_eq!(sunk[0].meta.len(), 1, "{cut:?}: the replayed orphan vote's");
        assert!(f.parts.state.take_bumps().is_empty(), "{cut:?}: applied once, by the sink");
        assert!(f.parts.relay.take().is_empty(), "{cut:?}");
        assert_eq!(f.announced(&f.b), 0, "{cut:?}: every announcement forgotten");
    }
}

#[test]
fn an_admission_dropped_unrun_applies_nothing() {
    struct Drops;
    impl GovExec for Drops {
        fn post(&self, _command: GovCommand) {}
    }
    let mut f = sim_over(Parts::new(Arc::new(Drops)));
    let (obj, _) = proposal(1);
    both_announce(&mut f, object_inv(object::hash(&obj)));
    f.deliver(false, "govobj", encode(&obj));
    assert_eq!((f.announced(&f.b), f.score(&f.a)), (1, 0));
    assert!(f.parts.state.take_bumps().is_empty());
}

#[test]
fn relay_invs_queue_objects_then_votes() {
    let q = RelayInvs::default();
    q.queue(&Effects {
        relay_objects: vec![[1; 32]],
        relay_votes: vec![[2; 32], [3; 32]],
        ..Effects::default()
    });
    q.push(object_inv([4; 32]));
    assert_eq!(
        q.take(),
        [object_inv([1; 32]), vote_inv([2; 32]), vote_inv([3; 32]), object_inv([4; 32])]
    );
    assert!(q.take().is_empty());
}

#[test]
fn an_object_request_is_govsync_with_an_empty_filter() {
    let msg = object_request([6; 32]);
    assert_eq!(msg.command, CommandString::GOVSYNC);
    let read = GovSync::decode(&mut &msg.payload[..]).unwrap();
    assert_eq!(read, GovSync::new(Hash256::from_lendian([6; 32]), empty_bloom_filter()));
}
