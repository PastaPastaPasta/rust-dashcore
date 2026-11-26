//! S1h: the collateral adapter, the command seam, the handle and the randomness.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Barrier, Mutex, PoisonError, mpsc};
use std::time::Duration;

use dash_consensus::{Tx, decode_tx};
use dash_dml::{MnList, testkit};
use dash_governance::GovParams;
use dash_governance::manager::{CollateralLookup, Effects, GovClock, Manager, MetaOp, RelayQueue};
use dash_governance::object::{self, FoundCollateral};
use dash_governance::vote;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_pkc::bls::{BlsScIetf, BlsSecretKey, BlsSigId};
use dash_primitives::{BlockHash, GovObject, GovVote, OutPoint, TxHash, VoteOutcome, VoteSignal};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_indexes::{IndexError, IndexStatus, IndexSummary, IndexedTx, TxIndexReader, Txid};
use proptest::prelude::*;

use crate::collateral::{AtUseCollateral, locate};
use crate::exec::fakes::{
    ExecEvent, FakeChain, FakeEnv, RecordingSink, SerialExec, current_command,
};
use crate::exec::{EffectsSink, GovEnv};
use crate::handle::{Admitted, GovernanceHandle};
use crate::rand::{CoreRng, Draw, GovRng, ReplayRng};

/// What the handle does on this thread, in order. Locks and env reads carry the command running
/// then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Lock(Option<u64>),
    Unlock,
    Env(Option<u64>),
    Submit,
    /// A vote signature checked by the vote sync.
    Verify,
}

thread_local! {
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn note(event: Event) {
    EVENTS.with(|l| l.borrow_mut().push(event));
}

pub(crate) fn take_events() -> Vec<Event> {
    EVENTS.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

pub(crate) fn params() -> GovParams {
    let chain = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest");
    GovParams::from_chain_params(&chain).expect("its parameters")
}

pub(crate) fn clock() -> GovClock {
    GovClock::new(1_417_715_856, 1_417_715_856)
}

pub(crate) fn block(n: u8) -> BlockHash {
    BlockHash::from_lendian([n; 32])
}

pub(crate) fn empty_list() -> MnList {
    MnList::new(block(0), 0, 0)
}

pub(crate) fn compact(out: &mut Vec<u8>, b: &[u8]) {
    assert!(b.len() < 0xfd);
    out.push(b.len() as u8);
    out.extend_from_slice(b);
}

pub(crate) fn gov_object(object_type: i32, data: &[u8], collateral: [u8; 32]) -> GovObject {
    signed_object(
        object_type,
        data,
        collateral,
        OutPoint::new(TxHash::from_lendian([0; 32]), u32::MAX),
        &[],
    )
}

pub(crate) fn signed_object(
    object_type: i32,
    data: &[u8],
    collateral: [u8; 32],
    masternode: OutPoint,
    sig: &[u8],
) -> GovObject {
    let mut b = vec![0u8; 32];
    b.extend(1i32.to_le_bytes());
    b.extend(1_417_715_841i64.to_le_bytes());
    b.extend(collateral);
    compact(&mut b, data);
    b.extend(object_type.to_le_bytes());
    b.extend(masternode.txid().to_lendian());
    b.extend(masternode.index().to_le_bytes());
    compact(&mut b, sig);
    GovObject::decode(&mut b.as_slice()).expect("an object")
}

pub(crate) fn operator_key(n: u32) -> BlsSecretKey<BlsScIetf> {
    BlsSecretKey::<BlsScIetf>::from_ikm(&testkit::numbered_operator_ikm(n)).expect("key")
}

/// A trigger paying `proposal` at `height`, signed by `testkit::numbered(n)`.
pub(crate) fn trigger(n: u32, proposal: [u8; 32], height: i32) -> GovObject {
    let mut hash = proposal;
    hash.reverse();
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    let data = format!(
        r#"{{"event_block_height": {}, "payment_addresses": "ySdjJ6TqxnBYa5V22N3kSPZtf6QRXZywzs", "payment_amounts": "1.10000000", "proposal_hashes": "{hex}", "type":2}}"#,
        height
    );
    let masternode = testkit::numbered(n).collateral;
    let unsigned = signed_object(object::TRIGGER, data.as_bytes(), [0; 32], masternode, &[]);
    let sig = operator_key(n).sign_with(&object::signature_hash(&unsigned), BlsSigId::Basic);
    signed_object(object::TRIGGER, data.as_bytes(), [0; 32], masternode, &sig.to_bytes())
}

/// A fee transaction burning one coin to `object_hash`, unique by `n`.
pub(crate) fn fee_tx(n: u16, object_hash: [u8; 32]) -> Tx {
    let mut b = 2i32.to_le_bytes().to_vec();
    b.push(1);
    let mut prevout = [0xaa; 32];
    prevout[..2].copy_from_slice(&n.to_le_bytes());
    b.extend(prevout);
    b.extend(0u32.to_le_bytes());
    compact(&mut b, &[]);
    b.extend(u32::MAX.to_le_bytes());
    b.push(1);
    b.extend(100_000_000i64.to_le_bytes());
    let mut script = vec![0x6a, 0x20];
    script.extend(object_hash);
    compact(&mut b, &script);
    b.extend(0u32.to_le_bytes());
    decode_tx(&mut b.as_slice()).expect("a transaction").into_value()
}

/// A valid regtest proposal and its fee transaction.
pub(crate) fn proposal(n: u16) -> (GovObject, Tx) {
    let data = format!(
        r#"{{"type": 1, "name": "P{n}", "start_epoch": 1417715841, "end_epoch": 1417802241, "payment_amount": 1.1, "payment_address": "ySdjJ6TqxnBYa5V22N3kSPZtf6QRXZywzs", "url": "https://dash.org"}}"#
    );
    let unfunded = gov_object(object::PROPOSAL, data.as_bytes(), [0; 32]);
    let tx = fee_tx(n, object::hash(&unfunded));
    (gov_object(object::PROPOSAL, data.as_bytes(), object::txid_bytes(&tx.txid())), tx)
}

pub(crate) fn txid(obj: &GovObject) -> [u8; 32] {
    object::txid_bytes(&obj.collateral_hash())
}

pub(crate) type Entry = Result<IndexedTx, IndexError>;
/// The txid whose lookup waits: it reports entry, then waits at the barrier for release.
type Hold = ([u8; 32], mpsc::Sender<()>, Arc<Barrier>);

/// A transaction index that records each `find_tx` with the command it ran in, and can hold
/// one lookup at a pair of barriers.
#[derive(Default)]
pub(crate) struct FakeIndex {
    entries: Mutex<BTreeMap<[u8; 32], Entry>>,
    reads: Mutex<Vec<([u8; 32], Option<u64>)>>,
    hold: Mutex<Option<Hold>>,
}

impl FakeIndex {
    pub(crate) fn put(&self, tx: &Tx, at: BlockHash) {
        self.set(object::txid_bytes(&tx.txid()), Ok((Arc::new(tx.raw().clone()), at)));
    }

    pub(crate) fn set(&self, key: [u8; 32], entry: Entry) {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).insert(key, entry);
    }

    pub(crate) fn reads(&self) -> Vec<([u8; 32], Option<u64>)> {
        self.reads.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn read_count(&self, key: &[u8; 32]) -> usize {
        self.reads().iter().filter(|(k, _)| k == key).count()
    }
}

impl IndexStatus for FakeIndex {
    fn summary(&self) -> IndexSummary {
        IndexSummary {
            name: "txindex",
            synced: true,
            best_block_height: 0,
        }
    }

    fn block_until_synced(&self) -> bool {
        true
    }
}

impl TxIndexReader for FakeIndex {
    fn find_tx(&self, txid: &Txid) -> Result<Option<IndexedTx>, IndexError> {
        let key = txid.to_lendian();
        self.reads.lock().unwrap_or_else(PoisonError::into_inner).push((key, current_command()));
        let hold = self.hold.lock().unwrap_or_else(PoisonError::into_inner).take();
        match hold {
            Some((k, entered, release)) if k == key => {
                let _ = entered.send(());
                release.wait();
            }
            other => *self.hold.lock().unwrap_or_else(PoisonError::into_inner) = other,
        }
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).get(&key).cloned().transpose()
    }

    fn has_tx(&self, txid: &Txid) -> Result<bool, IndexError> {
        Ok(self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&txid.to_lendian()))
    }
}

/// Records the collateral txids a manager call reads.
struct Recording<'a, L> {
    inner: &'a L,
    reads: RefCell<BTreeSet<[u8; 32]>>,
}

impl<'a, L> Recording<'a, L> {
    fn new(inner: &'a L) -> Self {
        Self {
            inner,
            reads: RefCell::default(),
        }
    }
}

impl<L: CollateralLookup> CollateralLookup for Recording<'_, L> {
    fn find_collateral(&self, txid: &[u8; 32]) -> Option<FoundCollateral<'_>> {
        self.reads.borrow_mut().insert(*txid);
        self.inner.find_collateral(txid)
    }
}

/// A vote for a parent no manager holds: an outpoint, parent, outcome, signal and time of
/// `fill` bytes, and no signature.
pub(crate) fn unknown_parent_vote(fill: u8) -> GovVote {
    let mut b = vec![fill; 32 + 4 + 32 + 4 + 4 + 8];
    b.push(0);
    GovVote::decode(&mut b.as_slice()).expect("a vote")
}

pub(crate) fn io_error() -> IndexError {
    IndexError::Io("disk".to_owned())
}

/// A chain at `tip` with blocks 1..=tip active, block `n` at height `n`.
pub(crate) fn chain_at(tip: u8) -> FakeChain {
    let mut chain = FakeChain::new(u32::from(tip), empty_list());
    chain.active = (1..=tip).map(|h| (block(h), u32::from(h))).collect();
    chain
}

// The seam accepts the non-`Sync` chain where V's view will go.
const _: fn(&FakeIndex, &FakeChain, &Txid) -> Option<dashd_indexes::LocatedTx> =
    |index, chain, txid| locate(index, chain, txid);

mod locate_tests {
    use super::*;

    #[test]
    fn found_on_the_active_chain_counts_confirmations() {
        let (_, tx) = proposal(1);
        let index = FakeIndex::default();
        index.put(&tx, block(95));
        let located = locate(&index, &chain_at(100), &tx.txid()).expect("found");
        assert_eq!((located.active_height, located.confirmations), (Some(95), 6));
        assert_eq!(located.block_hash, block(95));
        assert_eq!(located.tx.as_ref(), tx.raw());
    }

    #[test]
    fn a_block_off_the_active_chain_has_no_confirmations() {
        let (_, tx) = proposal(1);
        let index = FakeIndex::default();
        index.put(&tx, block(200));
        let located = locate(&index, &chain_at(100), &tx.txid()).expect("found");
        assert_eq!((located.active_height, located.confirmations), (None, 0));
    }

    #[test]
    fn an_index_error_is_absent() {
        let (_, tx) = proposal(1);
        let index = FakeIndex::default();
        index.set(object::txid_bytes(&tx.txid()), Err(io_error()));
        assert_eq!(locate(&index, &chain_at(100), &tx.txid()), None);
        assert_eq!(index.reads().len(), 1);
    }

    #[test]
    fn a_miss_is_absent() {
        let (_, tx) = proposal(1);
        let index = FakeIndex::default();
        assert_eq!(locate(&index, &chain_at(100), &tx.txid()), None);
        assert_eq!(index.reads().len(), 1);
    }

    #[test]
    fn at_use_answers_mined_with_the_located_confirmations() {
        let (obj, tx) = proposal(1);
        let index = FakeIndex::default();
        index.put(&tx, block(98));
        let at_use =
            AtUseCollateral::for_object(&obj, &Manager::new(), &params(), &index, &chain_at(100));
        let found = at_use.find_collateral(&txid(&obj)).expect("found");
        assert!(found.mined);
        assert_eq!(found.confirmations, 3);
        assert_eq!(found.tx.txid(), tx.txid());
    }

    #[test]
    fn a_trigger_reads_no_collateral_from_the_index() {
        let trigger = gov_object(object::TRIGGER, br#"{"type":2}"#, [7; 32]);
        let index = FakeIndex::default();
        let chain = chain_at(100);
        let m = Manager::new();
        let at_use = AtUseCollateral::for_object(&trigger, &m, &params(), &index, &chain);
        assert!(index.reads().is_empty());
        let rec = Recording::new(&at_use);
        let out = m
            .clone()
            .process_object_at(&trigger, &chain.list, &clock(), false, &params(), &rec)
            .expect("process");
        assert!(!out.ok);
        assert_eq!(*rec.reads.borrow(), *at_use.keys());
    }
}

/// A proposal's collateral state at the postponed pass.
#[derive(Clone, Debug)]
enum AtPass {
    Confirmations(u8),
    Miss,
    Fault,
}

fn at_pass() -> impl Strategy<Value = AtPass> {
    prop_oneof![(0u8..=8).prop_map(AtPass::Confirmations), Just(AtPass::Miss), Just(AtPass::Fault)]
}

/// An object offered for admission: a proposal postponed with 1 to 5 confirmations, a trigger,
/// or a proposal whose data fails, funded so that a lookup of it would succeed.
#[derive(Clone, Debug)]
enum Offered {
    Proposal(u8, AtPass),
    Trigger,
    Malformed,
}

fn offered() -> impl Strategy<Value = Offered> {
    prop_oneof![
        3 => (1u8..=5, at_pass()).prop_map(|(c, p)| Offered::Proposal(c, p)),
        1 => Just(Offered::Trigger),
        1 => Just(Offered::Malformed),
    ]
}

/// A proposal named `n` with no payment fields, and its fee transaction.
fn malformed(n: u16) -> (GovObject, Tx) {
    let data = format!(r#"{{"type": 1, "name": "M{n}"}}"#);
    let unfunded = gov_object(object::PROPOSAL, data.as_bytes(), [0; 32]);
    let tx = fee_tx(1000 + n, object::hash(&unfunded));
    (gov_object(object::PROPOSAL, data.as_bytes(), object::txid_bytes(&tx.txid())), tx)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn at_use_keys_equal_the_manager_reads(
        objects in prop::collection::vec(offered(), 0..7),
        synced in any::<bool>(),
        readmit in any::<prop::sample::Index>(),
    ) {
        let p = params();
        let index = FakeIndex::default();
        let mut chain = chain_at(100);
        let mut m = Manager::new();
        let made: Vec<_> = objects
            .iter()
            .enumerate()
            .map(|(i, offered)| match offered {
                Offered::Proposal(conf, _) => {
                    let (obj, tx) = proposal(i as u16);
                    index.put(&tx, block(101 - conf));
                    obj
                }
                Offered::Trigger => trigger(i as u32 + 1, [i as u8; 32], 100),
                Offered::Malformed => {
                    let (obj, tx) = malformed(i as u16);
                    index.put(&tx, block(90));
                    obj
                }
            })
            .collect();
        for (offered, obj) in objects.iter().zip(&made) {
            let at_use = AtUseCollateral::for_object(obj, &m, &p, &index, &chain);
            let rec = Recording::new(&at_use);
            let out = m.process_object_at(obj, &chain.list, &clock(), true, &p, &rec).expect("admit");
            prop_assert_eq!(&*rec.reads.borrow(), at_use.keys());
            prop_assert!(!at_use.index_fault());
            let looked_up = index.read_count(&txid(obj));
            match offered {
                Offered::Proposal(..) => prop_assert!(out.postponed && looked_up == 1),
                Offered::Trigger | Offered::Malformed => {
                    prop_assert!(!out.ok && !out.postponed && looked_up == 0);
                }
            }
        }
        if !made.is_empty() {
            let i = readmit.index(made.len());
            let at_use = AtUseCollateral::for_object(&made[i], &m, &p, &index, &chain);
            let rec = Recording::new(&at_use);
            m.process_object_at(&made[i], &chain.list, &clock(), true, &p, &rec).expect("again");
            prop_assert_eq!(&*rec.reads.borrow(), at_use.keys());
            if matches!(objects[i], Offered::Proposal(..)) {
                prop_assert!(at_use.keys().is_empty());
            }
        }
        chain.tip = 108;
        chain.active.extend((101..=108).map(|h| (block(h), u32::from(h))));
        for (offered, obj) in objects.iter().zip(&made) {
            let Offered::Proposal(_, pass) = offered else { continue };
            let key = txid(obj);
            match pass {
                AtPass::Confirmations(c) => {
                    let (tx, _) = index.entries.lock().expect("entries")[&key].clone().expect("put");
                    let at = if *c == 0 { block(250) } else { block(109 - c) };
                    index.set(key, Ok((tx, at)));
                }
                AtPass::Miss => { index.entries.lock().expect("entries").remove(&key); }
                AtPass::Fault => index.set(key, Err(io_error())),
            }
        }
        let at_use = AtUseCollateral::for_postponed(&m, synced, &index, &chain);
        let rec = Recording::new(&at_use);
        let postponed: BTreeSet<_> = m.postponed().values().map(txid).collect();
        m.updated_block_tip(108, &chain.list, &clock(), synced, &p, &rec).expect("tip");
        prop_assert_eq!(&*rec.reads.borrow(), at_use.keys());
        prop_assert_eq!(at_use.keys().clone(), if synced { postponed } else { BTreeSet::new() });
    }
}

/// The fake env, logging each read with the command it ran in.
struct Env(FakeEnv);

impl Env {
    fn set(&self, synced: bool, blockchain_synced: bool) {
        self.0.set(clock(), synced, blockchain_synced);
    }

    fn note(&self) {
        note(Event::Env(current_command()));
    }
}

impl GovEnv for Env {
    fn clock(&self) -> GovClock {
        self.note();
        self.0.clock()
    }

    fn synced(&self) -> bool {
        self.note();
        self.0.synced()
    }

    fn blockchain_synced(&self) -> bool {
        self.note();
        self.0.blockchain_synced()
    }
}

/// The recording sink, logging each submit.
#[derive(Default)]
struct Sink(RecordingSink);

impl Sink {
    fn take(&self) -> Vec<Effects> {
        self.0.take()
    }
}

impl EffectsSink for Sink {
    fn submit(&self, effects: Effects) {
        note(Event::Submit);
        self.0.submit(effects);
    }
}

struct Setup {
    exec_log: Arc<SerialExec>,
    env: Arc<Env>,
    sink: Arc<Sink>,
    index: Arc<FakeIndex>,
    handle: Arc<GovernanceHandle<Arc<SerialExec>>>,
}

fn setup(tip: u8) -> Setup {
    let exec = Arc::new(SerialExec::new(chain_at(tip)));
    let env = Arc::new(Env(FakeEnv::new(clock())));
    let sink = Arc::new(Sink::default());
    let index = Arc::new(FakeIndex::default());
    let handle = GovernanceHandle::new(
        Manager::new(),
        Arc::clone(&exec),
        env.clone(),
        sink.clone(),
        index.clone(),
        params(),
    );
    take_events();
    Setup {
        exec_log: exec,
        env,
        sink,
        index,
        handle,
    }
}

/// The handle left nothing queued in the manager.
fn assert_drained(s: &Setup) {
    let mut m = s.handle.state_for_test();
    assert_eq!(m.take_relay_queue(), RelayQueue::default());
    assert!(m.take_meta().is_empty());
}

fn advance(exec: &SerialExec, tip: u8) {
    let mut chain = exec.chain();
    chain.tip = u32::from(tip);
    chain.active = (1..=tip).map(|h| (block(h), u32::from(h))).collect();
}

#[test]
fn q_during_the_pass_runs_after_it_in_its_own_command() {
    let s = setup(96);
    let pending: Vec<_> = (0..3).map(proposal).collect();
    for (obj, tx) in &pending {
        s.index.put(tx, block(95));
        let Admitted {
            outcome: out,
            ..
        } = s.handle.admit(obj.clone()).wait().expect("ran");
        assert!(out.expect("admit").postponed);
    }
    advance(&s.exec_log, 100);
    let (q, q_tx) = proposal(9);
    let q_hash = object::hash(&q);
    s.index.put(&q_tx, block(90));
    let held_txid = txid(&pending[1].0);
    let (entered, entered_rx) = mpsc::channel();
    let release = Arc::new(Barrier::new(2));
    *s.index.hold.lock().expect("hold") = Some((held_txid, entered, release.clone()));
    let before = s.exec_log.log().len();

    let tip = std::thread::spawn({
        let h = Arc::clone(&s.handle);
        move || h.tip(100).wait().expect("ran")
    });
    entered_rx.recv_timeout(Duration::from_secs(60)).expect("the pass reached the held lookup");
    let admit = std::thread::spawn({
        let h = Arc::clone(&s.handle);
        move || h.admit(q).wait().expect("ran")
    });
    while s.exec_log.waiting() == 0 {
        std::thread::yield_now();
    }
    release.wait();
    let tip_done = tip.join().expect("tip");
    let q_done = admit.join().expect("admit");
    assert!(!tip_done.index_fault && !q_done.index_fault);

    let log = &s.exec_log.log()[before..];
    let [ExecEvent::Enter(t), ExecEvent::Exit(t2), ExecEvent::Enter(a), ExecEvent::Exit(a2)] = log
    else {
        panic!("two serial commands, got {log:?}");
    };
    assert_eq!((t, a), (t2, a2));
    tip_done.result.expect("tip");
    let reads_in = |id: u64| -> Vec<[u8; 32]> {
        s.index.reads().into_iter().filter(|(_, c)| *c == Some(id)).map(|(k, _)| k).collect()
    };
    let p: BTreeSet<_> = pending.iter().map(|(o, _)| txid(o)).collect();
    let tip_reads = reads_in(*t);
    assert_eq!(tip_reads.len(), p.len());
    assert_eq!(tip_reads.into_iter().collect::<BTreeSet<_>>(), p);
    assert_eq!(reads_in(*a), vec![object::txid_bytes(&q_tx.txid())]);
    assert_eq!(s.index.read_count(&object::txid_bytes(&q_tx.txid())), 1);
    let q_out = q_done.outcome.expect("admit");
    assert!(q_out.ok && q_out.held && !q_out.postponed);
    let held: BTreeSet<_> = pending.iter().map(|(o, _)| object::hash(o)).collect();
    let [tip_effects] = &s.sink.take()[..] else {
        panic!("one submission, from the tip");
    };
    assert_eq!(tip_effects.relay_objects.iter().copied().collect::<BTreeSet<_>>(), held);
    assert_eq!(q_done.effects.relay_objects, vec![q_hash]);
    let state = s.handle.state_for_test();
    assert!(held.iter().all(|h| state.objects().contains_key(h)) && state.postponed().is_empty());
}

#[test]
fn commands_touch_the_manager_only_inside_the_executor() {
    let s = setup(100);
    let (obj, tx) = proposal(1);
    s.index.put(&tx, block(95));
    s.handle.admit(obj).wait().expect("ran").outcome.expect("admit");
    s.handle.tip(100).wait().expect("ran").result.expect("tip");
    s.handle.clean().wait().expect("ran");
    let log = s.exec_log.log();
    let [admit, tip, clean] = [0, 1, 2].map(|i| match log[2 * i..2 * i + 2] {
        [ExecEvent::Enter(a), ExecEvent::Exit(b)] if a == b => Some(a),
        ref other => panic!("unbalanced {other:?}"),
    });
    assert_eq!(log.len(), 6);
    // Each command reads the clock and a sync flag under the lock; `tip` and `clean` submit
    // their effects once it is released.
    use Event::{Env, Lock, Submit, Unlock};
    assert_eq!(
        take_events(),
        [
            [Lock(admit), Env(admit), Env(admit), Unlock].as_slice(),
            &[Lock(tip), Env(tip), Env(tip), Unlock, Submit],
            &[Lock(clean), Env(clean), Env(clean), Unlock, Submit],
        ]
        .concat()
    );
    let vote = unknown_parent_vote(0);
    let _ = s.handle.process_vote(&vote, &empty_list());
    assert_eq!(take_events(), vec![Lock(None), Env(None), Unlock]);
}

#[test]
fn a_mutation_republishes_and_a_no_op_does_not() {
    let s = setup(100);
    let (obj, tx) = proposal(1);
    s.index.put(&tx, block(95));
    let first = s.handle.published();
    let Admitted {
        outcome: out,
        effects,
        ..
    } = s.handle.admit(obj.clone()).wait().expect("ran");
    assert!(out.expect("admit").held);
    assert_eq!(effects.relay_objects, vec![object::hash(&obj)]);
    let second = s.handle.published();
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(second.objects.is_empty() && second.superblocks.iter().next().is_none());
    assert!(s.handle.state_for_test().objects().contains_key(&object::hash(&obj)));

    let Admitted {
        outcome: out,
        effects,
        ..
    } = s.handle.admit(obj).wait().expect("ran");
    assert!(out.expect("seen").held);
    assert_eq!(effects, Default::default());
    assert!(Arc::ptr_eq(&second, &s.handle.published()));
    s.env.set(true, false);
    s.handle.clean().wait().expect("ran");
    assert!(Arc::ptr_eq(&second, &s.handle.published()));
    assert_eq!(s.sink.take(), vec![Effects::default()]);
    let vote = unknown_parent_vote(1);
    let (orphan, _) = s.handle.process_vote(&vote, &empty_list());
    assert!(orphan.result.is_err());
    assert!(Arc::ptr_eq(&second, &s.handle.published()));

    s.handle.tip(100).wait().expect("ran").result.expect("tip");
    assert_eq!(s.sink.take(), vec![Effects::default()]);
    let third = s.handle.published();
    assert!(!Arc::ptr_eq(&second, &third));
    assert_eq!(third.cached_height, 100);
}

/// A `valid` yes vote on `parent` by `testkit::numbered(n)`, signed with its operator key.
pub(crate) fn operator_vote(n: u32, parent: [u8; 32]) -> GovVote {
    let dmn = testkit::numbered(n);
    let unsigned = |sig| {
        GovVote::new(
            dmn.collateral,
            TxHash::from_lendian(parent),
            VoteOutcome::new(vote::OUTCOME_YES as u32),
            VoteSignal::new(vote::SIGNAL_VALID as u32),
            clock().now,
            sig,
        )
    };
    let hash = vote::signature_hash(&unsigned(Vec::new()));
    unsigned(operator_key(n).sign_with(&hash, BlsSigId::Basic).to_bytes().to_vec())
}

#[test]
fn votes_drain_meta_and_republish_only_for_triggers() {
    let list = three_masternodes();
    let s = setup(100);
    s.exec_log.chain().list = Arc::new(list.clone());
    let (obj, tx) = proposal(1);
    let parent = object::hash(&obj);
    s.index.put(&tx, block(95));
    let meta = |n, object| MetaOp::AddGovernanceVote {
        pro_tx_hash: *testkit::numbered(n).pro_tx_hash.as_bytes(),
        object,
    };

    let first = s.handle.published();
    let (orphan, effects) = s.handle.process_vote(&operator_vote(1, parent), &list);
    assert_eq!(orphan.to_request, Some(parent));
    assert!(effects.meta.is_empty());
    assert_drained(&s);
    assert!(Arc::ptr_eq(&first, &s.handle.published()));

    // The add republishes, with the replayed orphan's meta.
    let Admitted {
        outcome: out,
        effects,
        ..
    } = s.handle.admit(obj).wait().expect("ran");
    assert!(out.expect("admit").held);
    assert_eq!(effects.meta, vec![meta(1, parent)]);
    assert_eq!(effects.relay_votes, vec![vote::hash(&operator_vote(1, parent))]);
    assert_drained(&s);
    let admitted = s.handle.published();
    assert!(!Arc::ptr_eq(&first, &admitted));

    // A proposal vote affects no trigger: meta, but no republish.
    let (accepted, effects) = s.handle.process_vote(&operator_vote(2, parent), &list);
    assert_eq!(accepted.result, Ok(true));
    assert_eq!(effects.meta, vec![meta(2, parent)]);
    assert_drained(&s);
    assert!(Arc::ptr_eq(&admitted, &s.handle.published()));

    // Non-mainnet triggers more than two cycles ahead of the cached height are expired.
    s.handle.tip(100).wait().expect("ran").result.expect("tip");
    assert_drained(&s);
    let trigger = trigger(3, parent, 100);
    let hash = object::hash(&trigger);
    let Admitted {
        outcome: out,
        ..
    } = s.handle.admit(trigger).wait().expect("ran");
    assert!(out.expect("admit").held);
    assert_drained(&s);
    let with_trigger = s.handle.published();
    assert!(with_trigger.superblocks.get(&hash).is_some());

    // A trigger vote republishes, with its meta.
    let (accepted, effects) = s.handle.process_vote(&operator_vote(2, hash), &list);
    assert_eq!(accepted.result, Ok(true));
    assert_eq!(effects.meta, vec![meta(2, hash)]);
    assert_drained(&s);
    let voted = s.handle.published();
    assert!(!Arc::ptr_eq(&with_trigger, &voted));
    assert_eq!(voted.generation, with_trigger.generation.wrapping_add(1));
    assert_eq!(voted.objects[&hash].file.len(), 1);
}

/// A list holding `testkit::numbered(1..=3)`.
pub(crate) fn three_masternodes() -> MnList {
    let mut list = empty_list();
    for n in 1..=3 {
        list.insert_for_tests(testkit::numbered(n));
    }
    list
}

#[test]
fn a_synced_clean_publishes_a_view_without_an_expired_trigger() {
    let s = setup(100);
    s.exec_log.chain().list = Arc::new(three_masternodes());
    let (obj, tx) = proposal(1);
    s.index.put(&tx, block(95));
    s.handle.admit(obj.clone()).wait().expect("ran").outcome.expect("admit");
    s.handle.tip(100).wait().expect("ran").result.expect("tip");
    let trigger = trigger(3, object::hash(&obj), 100);
    let hash = object::hash(&trigger);
    assert!(s.handle.admit(trigger).wait().expect("ran").outcome.expect("admit").held);
    let past = 100 + params().superblock_cycle() + 1;
    s.handle.tip(past).wait().expect("ran").result.expect("tip");
    let before = s.handle.published();
    assert!(before.superblocks.get(&hash).is_some() && before.objects.contains_key(&hash));
    s.sink.take();

    // Unsynced, the clean is a no-op and republishes nothing.
    s.env.set(true, false);
    s.handle.clean().wait().expect("ran");
    assert!(Arc::ptr_eq(&before, &s.handle.published()));
    assert_drained(&s);

    s.env.set(true, true);
    s.handle.clean().wait().expect("ran");
    let after = s.handle.published();
    assert!(after.superblocks.get(&hash).is_none() && !after.objects.contains_key(&hash));
    assert_eq!(after.generation, s.handle.state_for_test().generation());
    assert!(s.handle.state_for_test().object(&hash).expect("held").expired);
    assert_eq!(s.sink.take().len(), 2);
    assert_drained(&s);
}

#[test]
fn admit_follows_synced_not_blockchain_synced() {
    let p = params();
    let late = clock().now + 2 * i64::from(p.superblock_cycle()) * p.target_spacing() + 1;
    for synced in [false, true] {
        let s = setup(100);
        s.exec_log.chain().list = Arc::new(three_masternodes());
        s.handle.tip(100).wait().expect("ran").result.expect("tip");
        // Past the trigger rate check's window, which only a synced admission applies.
        s.env.0.set(GovClock::new(late, late), synced, !synced);
        let (obj, tx) = proposal(1);
        s.index.put(&tx, block(95));
        let admitted = s.handle.admit(obj.clone()).wait().expect("ran");
        assert!(admitted.outcome.expect("admit").held);
        let relayed = if synced {
            vec![object::hash(&obj)]
        } else {
            Vec::new()
        };
        assert_eq!(admitted.effects.relay_objects, relayed);
        let trigger = trigger(3, object::hash(&obj), 100);
        let out = s.handle.admit(trigger).wait().expect("ran").outcome.expect("no overflow");
        assert_eq!(out.held, !synced);
    }
}

#[test]
fn the_tip_reads_the_sync_state_when_it_runs() {
    let s = setup(96);
    let (obj, tx) = proposal(1);
    let key = txid(&obj);
    s.index.put(&tx, block(95));
    assert!(s.handle.admit(obj.clone()).wait().expect("ran").outcome.expect("admit").postponed);
    advance(&s.exec_log, 100);
    s.env.set(false, true);
    s.handle.tip(100).wait().expect("ran").result.expect("tip");
    assert_eq!(s.index.read_count(&key), 1);
    assert_eq!(s.handle.state_for_test().postponed().len(), 1);

    s.env.set(true, true);
    s.handle.tip(100).wait().expect("ran").result.expect("tip");
    assert_eq!(s.index.read_count(&key), 2);
    assert!(s.handle.state_for_test().objects().contains_key(&object::hash(&obj)));
}

/// A miss (`Ok(None)`) and an index fault (`Err`): the same v24 outcome, but only the fault is
/// reported as ours.
fn misses_and_faults() -> [(Option<IndexError>, bool); 2] {
    [(None, false), (Some(io_error()), true)]
}

#[test]
fn a_collateral_fault_is_v24s_outcome_and_is_not_cached() {
    for (fault, index_fault) in misses_and_faults() {
        let s = setup(100);
        let (obj, tx) = proposal(1);
        let key = object::txid_bytes(&tx.txid());
        if let Some(e) = fault {
            s.index.set(key, Err(e));
        }
        let first = s.handle.published();
        let refused = s.handle.admit(obj.clone()).wait().expect("ran");
        let out = refused.outcome.expect("no overflow");
        assert!(!out.ok && !out.held && !out.postponed);
        assert_eq!(refused.effects, Default::default());
        assert_eq!(refused.index_fault, index_fault);
        assert!(Arc::ptr_eq(&first, &s.handle.published()));

        s.index.put(&tx, block(95));
        let admitted = s.handle.admit(obj.clone()).wait().expect("ran");
        assert!(admitted.outcome.expect("admit").held);
        assert!(!admitted.index_fault);
        assert_eq!(s.index.read_count(&key), 2);
    }
}

#[test]
fn a_collateral_fault_in_the_pass_drops_the_postponed_object_and_is_not_cached() {
    for (fault, index_fault) in misses_and_faults() {
        let s = setup(96);
        let (obj, tx) = proposal(1);
        let key = object::txid_bytes(&tx.txid());
        s.index.put(&tx, block(95));
        let postponed = s.handle.admit(obj.clone()).wait().expect("ran");
        assert!(postponed.outcome.expect("admit").postponed && !postponed.index_fault);
        match fault {
            Some(e) => s.index.set(key, Err(e)),
            None => {
                s.index.entries.lock().expect("entries").remove(&key);
            }
        }
        advance(&s.exec_log, 100);
        let pass = s.handle.tip(100).wait().expect("ran");
        pass.result.expect("tip");
        assert_eq!(pass.index_fault, index_fault);
        let state = s.handle.state_for_test();
        assert!(state.postponed().is_empty() && state.objects().is_empty());
        drop(state);

        s.index.put(&tx, block(95));
        let readmitted = s.handle.admit(obj).wait().expect("ran");
        assert!(readmitted.outcome.expect("admit").held && !readmitted.index_fault);
        assert_eq!(s.index.read_count(&key), 3);
    }
}

#[test]
fn a_proposal_refused_for_its_data_reads_no_collateral() {
    let s = setup(100);
    let (obj, tx) = malformed(1);
    let key = object::txid_bytes(&tx.txid());
    s.index.set(key, Err(io_error()));
    let refused = s.handle.admit(obj).wait().expect("ran");
    let out = refused.outcome.expect("no overflow");
    assert!(!out.ok && !out.held && !out.postponed);
    assert!(!refused.index_fault);
    assert!(s.index.reads().is_empty());
}

/// The events `f` logs on this thread, as (level, message).
pub(crate) fn logged<T>(f: impl FnOnce() -> T) -> (T, Vec<(tracing::Level, String)>) {
    struct Lines(Mutex<Vec<(tracing::Level, String)>>);
    impl tracing::Subscriber for Lines {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    if f.name() == "message" {
                        self.0.push_str(&format!("{v:?}"));
                    }
                }
            }
            let mut m = Message(String::new());
            event.record(&mut m);
            let level = *event.metadata().level();
            self.0.lock().unwrap_or_else(PoisonError::into_inner).push((level, m.0));
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    // Never the only registered dispatcher, or a thread without one that registers a callsite
    // first caches `Interest::never` for it.
    static KEEP: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    KEEP.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    let lines = Arc::new(Lines(Mutex::new(Vec::new())));
    let out = tracing::subscriber::with_default(Arc::clone(&lines), f);
    let lines = std::mem::take(&mut *lines.0.lock().unwrap_or_else(PoisonError::into_inner));
    (out, lines)
}

fn index_errors(lines: &[(tracing::Level, String)]) -> usize {
    lines
        .iter()
        .filter(|(level, m)| {
            *level == tracing::Level::ERROR && m.contains("transaction index failed")
        })
        .count()
}

#[test]
fn an_index_fault_is_logged_at_error_level_and_a_miss_is_not() {
    let (obj, tx) = proposal(1);
    let key = object::txid_bytes(&tx.txid());
    for (fault, errors) in [(None, 0), (Some(io_error()), 1)] {
        let index = FakeIndex::default();
        if let Some(e) = fault.clone() {
            index.set(key, Err(e));
        }
        let (found, lines) = logged(|| locate(&index, &chain_at(100), &tx.txid()));
        assert_eq!((found, index_errors(&lines)), (None, errors));

        let s = setup(100);
        if let Some(e) = fault {
            s.index.set(key, Err(e));
        }
        let (refused, lines) = logged(|| s.handle.admit(obj.clone()).wait().expect("ran"));
        assert_eq!((refused.index_fault, index_errors(&lines)), (errors == 1, errors));
    }
}

#[test]
fn a_poisoned_lock_stops_every_later_call() {
    let s = setup(100);
    let h = Arc::clone(&s.handle);
    let poisoner = std::thread::spawn(move || {
        let _m = h.state_for_test();
        panic!("a panic under the lock");
    });
    assert!(poisoner.join().is_err());
    let vote = unknown_parent_vote(2);
    let calls: [Box<dyn FnOnce()>; 2] = [
        Box::new(|| {
            let _ = s.handle.process_vote(&vote, &empty_list());
        }),
        Box::new(|| {
            let _ = s.handle.tip(100).wait();
        }),
    ];
    for call in calls {
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(call));
        assert!(panicked.is_err());
    }
    assert!(s.sink.take().is_empty());
}

#[test]
fn replay_returns_the_recorded_values() {
    let (a, b, c) = ([1u8; 32], [2u8; 32], [3u8; 32]);
    let mut rng = ReplayRng::new([
        Draw::BloomTweak(424_242),
        Draw::Shuffle(vec![c, a, b]),
        Draw::Sample(vec![b]),
    ]);
    assert_eq!(rng.bloom_tweak(), 424_242);
    let mut v = [a, b, c];
    rng.shuffle(&mut v);
    assert_eq!(v, [c, a, b]);
    let mut small = vec![a];
    rng.sample(&mut small, 1);
    assert_eq!(small, vec![a]);
    let mut s = vec![a, b, c];
    rng.sample(&mut s, 1);
    assert_eq!(s, vec![b]);
    assert_eq!(rng.remaining(), 0);
}

#[test]
#[should_panic(expected = "replay recording exhausted")]
fn replay_fails_when_exhausted() {
    let mut rng = ReplayRng::new([Draw::BloomTweak(1)]);
    rng.bloom_tweak();
    rng.bloom_tweak();
}

#[test]
#[should_panic(expected = "replay mismatch")]
fn replay_fails_on_a_mismatched_draw() {
    let mut rng = ReplayRng::new([Draw::Shuffle(vec![[9; 32]])]);
    rng.shuffle(&mut [[1; 32]]);
}

#[test]
fn core_rng_draws_from_fresh_contexts() {
    let source: Arc<dyn dash_p2p::rng::RandSource> =
        Arc::new(dash_p2p::rng::StreamRand::from_seed(7));
    let mut rng = CoreRng::new(source);
    assert!(rng.bloom_tweak() < 999_999);
    let mut v: Vec<[u8; 32]> = (0..120u8).map(|i| [i; 32]).collect();
    rng.sample(&mut v, 100);
    assert_eq!(v.len(), 100);
    assert_eq!(v.iter().collect::<BTreeSet<_>>().len(), 100);
    let mut short = vec![[1u8; 32], [2u8; 32]];
    rng.sample(&mut short, 100);
    assert_eq!(short, vec![[1u8; 32], [2u8; 32]]);
}

mod reply_tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Wake, Waker};

    use crate::exec::reply;

    #[derive(Default)]
    struct Flag(AtomicBool);

    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn lost_into(seen: &Arc<Mutex<Vec<u8>>>) -> impl FnOnce(u8) + Send + 'static {
        let seen = Arc::clone(seen);
        move |answer| seen.lock().unwrap().push(answer)
    }

    #[test]
    fn an_answer_is_read_or_lost_never_both() {
        let seen = Arc::default();
        let (tx, rx) = reply(lost_into(&seen));
        tx.send(1);
        assert_eq!(rx.wait(), Some(1));
        // Sent, then the reply dropped unread.
        let (tx, rx) = reply(lost_into(&seen));
        tx.send(2);
        drop(rx);
        // The reply dropped, then sent.
        let (tx, rx) = reply(lost_into(&seen));
        drop(rx);
        tx.send(3);
        // Dropped unrun: nothing to lose.
        let (tx, rx) = reply(lost_into(&seen));
        drop(tx);
        assert_eq!(rx.wait(), None);
        // Polled while pending, then sent: woken, and read.
        let (tx, mut rx) = reply(lost_into(&seen));
        let flag = Arc::new(Flag::default());
        let waker = Waker::from(Arc::clone(&flag));
        let mut cx = Context::from_waker(&waker);
        assert!(rx.poll_wait(&mut cx).is_pending());
        tx.send(5);
        assert!(flag.0.load(Ordering::SeqCst));
        assert_eq!(rx.poll_wait(&mut cx), Poll::Ready(Some(5)));
        drop(rx);
        assert_eq!(*seen.lock().unwrap(), [2, 3]);
    }
}
