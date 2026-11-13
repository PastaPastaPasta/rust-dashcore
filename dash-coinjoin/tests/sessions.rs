//! The masternode's and the client's mixing sessions, driven through their handlers. The server
//! cases follow Dash Core v24.0.0's `src/coinjoin/server.cpp`; each state is reached through
//! the messages and scheduler steps that lead to it.

mod common;

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};

use common::*;
use dash_coinjoin::client::{
    ClientEntry, ClientSession, CompleteOutcome, FinalTxOutcome, FinalTxRefusal, StatusOutcome,
    TimeoutOutcome,
};
use dash_coinjoin::collateral::MempoolAccept;
use dash_coinjoin::denom::{self, PROMOTION_RATIO, SMALLEST_DENOMINATION};
use dash_coinjoin::dstx::{self, PoolLimits};
use dash_coinjoin::inouts::{CoinView, ViewCoin, sort_bip69};
use dash_coinjoin::pool::{
    FLAG_DEMOTION, FLAG_PROMOTION, PoolMessage, PoolState, PoolStatusUpdate, QUEUE_TIMEOUT,
    SIGNING_TIMEOUT,
};
use dash_coinjoin::queue::{
    self, DsqOutcome, MasternodeMixing, MixingCounters, MixingMasternode, MixingMeta,
    TipMasternodes,
};
use dash_coinjoin::server::{ServerEffect, ServerEnv, ServerSession, SignedInputs};
use dash_consensus::hash::hash160;
use dash_consensus::script::sighash::{TxSigData, signature_hash};
use dash_consensus::script::{
    DirectVerifier, ScriptFlags, TransactionSignatureChecker, push_data, verify_script,
};
use dash_num::Hash256;
use dash_p2p_core::{DsAccept, DsComplete, DsEntry, DsQueue, DsStatusUpdate, ProtocolVersion};
use dash_pkc::bls::core::BlsVerdict;
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut};
use secp256k1::{Message, PublicKey, SecretKey};

const V24: ProtocolVersion = ProtocolVersion::V24_0;
/// A peer before the rebalance protocol (Core's `COINJOIN_REBALANCE_VERSION`).
const LEGACY: ProtocolVersion = ProtocolVersion::new(ProtocolVersion::COINJOIN_REBALANCE.get() - 1);

/// The masternode's node: chain, mempool, clock, randomness, key and connections.
struct Env {
    chain: Chain,
    now: i64,
    now_reads: RefCell<VecDeque<i64>>,
    /// `GetRand` answers, in order; when empty, `n - 1` (no fee is ever charged).
    rand: VecDeque<u32>,
    v24: bool,
    registered: bool,
    pro_tx_hash: TxHash,
    collateral: OutPoint,
    enabled: i32,
    op: Operator,
    disconnected: BTreeSet<u8>,
    mempool_accepts: bool,
    committed: Vec<Transaction>,
    coin_answers: RefCell<VecDeque<(OutPoint, Option<ViewCoin>)>>,
}

impl Env {
    fn new() -> Self {
        Self {
            chain: Chain::default(),
            now: NOW,
            now_reads: RefCell::default(),
            rand: VecDeque::new(),
            v24: false,
            registered: true,
            pro_tx_hash: u256(0x77),
            collateral: outpoint(u256(0x78), 0),
            enabled: 100,
            op: Operator::new(1),
            disconnected: BTreeSet::new(),
            mempool_accepts: true,
            committed: Vec::new(),
            coin_answers: RefCell::default(),
        }
    }
}

impl CoinView for Env {
    fn coin(&self, outpoint: &OutPoint) -> Option<ViewCoin> {
        let mut answers = self.coin_answers.borrow_mut();
        let coin = if answers.front().is_some_and(|(expected, _)| expected == outpoint) {
            answers.pop_front().expect("queued answer").1
        } else {
            self.chain.coin(outpoint)
        };
        coin.filter(|coin| coin.out.value() != -1)
    }

    fn is_instantsend_locked(&self, txid: &TxHash) -> bool {
        self.chain.is_instantsend_locked(txid)
    }
}

impl MempoolAccept for Env {
    fn test_accept(&self, tx: &Transaction) -> Option<i64> {
        self.chain.test_accept(tx)
    }
}

impl ServerEnv for Env {
    type Peer = u8;

    fn now(&self) -> i64 {
        self.now_reads.borrow_mut().pop_front().unwrap_or(self.now)
    }

    fn adjusted_time(&self) -> i64 {
        self.now
    }

    fn rand_below(&mut self, n: u32) -> u32 {
        self.rand.pop_front().map_or(n - 1, |r| r % n)
    }

    fn v24_active_at_tip(&self) -> bool {
        self.v24
    }

    fn valid_self(&self) -> Option<TxHash> {
        self.registered.then_some(self.pro_tx_hash)
    }

    fn self_outpoint(&self) -> OutPoint {
        self.collateral
    }

    fn self_pro_tx_hash(&self) -> TxHash {
        self.pro_tx_hash
    }

    fn enabled_count(&self) -> i32 {
        self.enabled
    }

    fn sign_basic(&mut self, hash: &Hash256) -> Vec<u8> {
        self.op.sign(hash)
    }

    fn is_connected(&self, peer: &u8) -> bool {
        !self.disconnected.contains(peer)
    }

    fn commit_to_mempool(&mut self, tx: &Transaction, _txid: &TxHash) -> bool {
        self.committed.push(tx.clone());
        self.mempool_accepts
    }

    fn has_dstx(&self, _txid: &TxHash) -> bool {
        false
    }
}

/// The `dssu`s among the effects: (peer, status, message).
fn statuses(fx: &[ServerEffect<u8>]) -> Vec<(u8, i32, PoolMessage)> {
    fx.iter()
        .filter_map(|e| match e {
            ServerEffect::Status {
                peer,
                update,
            } => Some((
                *peer,
                update.status_update(),
                PoolMessage::from_i32(update.message_id()).expect("in range"),
            )),
            _ => None,
        })
        .collect()
}

const ACCEPTED: i32 = PoolStatusUpdate::Accepted as i32;
const REJECTED: i32 = PoolStatusUpdate::Rejected as i32;

fn dsa(denom: i32, collateral: &Transaction, flags: Option<u8>) -> DsAccept {
    DsAccept::new(denom, collateral.clone(), flags)
}

/// A participant: its collateral and the entry it will send.
struct Participant {
    peer: u8,
    collateral: Transaction,
    inputs: Vec<TxIn>,
    outputs: Vec<TxOut>,
}

impl Participant {
    /// A standard participant mixing one smallest-denomination coin.
    fn standard(env: &mut Env, peer: u8) -> Self {
        let collateral = env.chain.collateral();
        let inputs = env.chain.smallest_coins(1);
        for input in &inputs {
            env.chain.add_coin(
                input.prevout(),
                txout(SMALLEST_DENOMINATION, input_script(input)),
                false,
            );
        }
        let outputs = vec![txout(SMALLEST_DENOMINATION, key(u32::from(peer)))];
        Self {
            peer,
            collateral,
            inputs,
            outputs,
        }
    }

    /// A promotion: ten smallest coins to one of the next denomination.
    fn promotion(env: &mut Env, peer: u8) -> Self {
        let collateral = env.chain.collateral();
        let inputs = env.chain.smallest_coins(PROMOTION_RATIO as usize);
        let larger = denom::denomination_to_amount(denom::larger_adjacent_denom(SMALLEST));
        let outputs = vec![txout(larger, key(u32::from(peer) * 100))];
        Self {
            peer,
            collateral,
            inputs,
            outputs,
        }
    }

    /// A demotion: one coin of the next denomination to ten smallest.
    fn demotion(env: &mut Env, peer: u8) -> Self {
        let collateral = env.chain.collateral();
        let larger = denom::denomination_to_amount(denom::larger_adjacent_denom(SMALLEST));
        let inputs = vec![txin(env.chain.fund(larger))];
        let outputs = (0..PROMOTION_RATIO as u32)
            .map(|i| txout(SMALLEST_DENOMINATION, key(u32::from(peer) * 100 + i)))
            .collect();
        Self {
            peer,
            collateral,
            inputs,
            outputs,
        }
    }

    fn entry(&self) -> DsEntry {
        DsEntry::new(self.inputs.clone(), self.collateral.clone(), self.outputs.clone())
    }

    /// Its inputs signed against the final proposal with legacy CoinJoin's hash type.
    fn signed(&self, proposal: &Transaction) -> SignedInputs {
        SignedInputs::Inputs(
            self.inputs
                .iter()
                .map(|input| sign_input(proposal, input, &input_key(input), 0x81))
                .collect(),
        )
    }
}

fn input_key(input: &TxIn) -> SecretKey {
    SecretKey::from_secret_bytes(input.prevout().txid().to_lendian()).expect("fixed valid key")
}

fn key_script(key: &SecretKey) -> Vec<u8> {
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend(hash160(&PublicKey::from_secret_key(key).serialize()));
    script.extend([0x88, 0xac]);
    script
}

fn input_script(input: &TxIn) -> Vec<u8> {
    key_script(&input_key(input))
}

fn sign_input(proposal: &Transaction, input: &TxIn, key: &SecretKey, hash_type: u8) -> TxIn {
    let index =
        proposal.inputs().iter().position(|i| i.prevout() == input.prevout()).expect("input");
    let hash = signature_hash(&key_script(key), proposal, index, i32::from(hash_type));
    let mut signature =
        secp256k1::ecdsa::sign(Message::from_digest(hash), key).serialize_der().to_vec();
    signature.push(hash_type);
    let mut script = Vec::new();
    push_data(&mut script, &signature);
    push_data(&mut script, &PublicKey::from_secret_key(key).serialize());
    TxIn::new(input.prevout(), script.into(), input.sequence())
}

/// A session past admission: every participant's `dsa` accepted, then the scheduler step that
/// opens it for entries (at the most participants, or once timed out with enough).
fn accepting_session(
    env: &mut Env,
    limits: PoolLimits,
    ps: &[Participant],
    flags: &[Option<u8>],
    version: ProtocolVersion,
) -> ServerSession<u8> {
    let mut s = ServerSession::new(limits);
    let counters = MixingCounters::default();
    for (p, f) in ps.iter().zip(flags.iter().chain(std::iter::repeat(&None))) {
        let fx = s.process_dsa(env, &counters, &p.peer, version, &dsa(SMALLEST, &p.collateral, *f));
        assert_eq!(
            statuses(&fx),
            vec![(p.peer, ACCEPTED, PoolMessage::MsgNoErr)],
            "dsa of {}",
            p.peer
        );
    }
    assert_eq!(s.state(), PoolState::Queue);
    if !s.is_session_ready(env.now) {
        env.now += QUEUE_TIMEOUT;
    }
    let fx = s.tick(env);
    assert_eq!(s.state(), PoolState::AcceptingEntries);
    assert!(matches!(fx.as_slice(), [ServerEffect::RelayQueue(q)] if q.ready()));
    s
}

/// A full session of three standard participants, from `dsa` to the committed DSTX, with each
/// participant's client checking the final transaction.
#[test]
fn server_session_completes() {
    let mut env = Env::new();
    let ps: Vec<_> = (1..=3).map(|p| Participant::standard(&mut env, p)).collect();
    let mut s = ServerSession::new(mainnet_limits());
    let counters = MixingCounters::default();

    // The first dsa opens the session and announces it.
    let fx = s.process_dsa(&mut env, &counters, &1, V24, &dsa(SMALLEST, &ps[0].collateral, None));
    let [
        ServerEffect::RelayQueue(q),
        ServerEffect::Status {
            peer: 1,
            update,
        },
    ] = fx.as_slice()
    else {
        panic!("{fx:?}");
    };
    assert_eq!(
        (q.denom(), q.ready(), q.time(), q.pro_tx_hash()),
        (SMALLEST, false, NOW, env.pro_tx_hash)
    );
    assert_eq!(queue::check_queue_signature(q, &env.op.public()), BlsVerdict::Valid);
    assert_eq!(
        *update,
        DsStatusUpdate::new(
            999_999,
            PoolState::Queue as i32,
            ACCEPTED,
            PoolMessage::MsgNoErr as i32,
        )
    );
    assert_eq!((s.session_id(), s.session_denom(), s.collateral_count()), (999_999, SMALLEST, 1));
    assert!(s.queues().has_any_queue_from_masternode(&env.collateral));

    for p in &ps[1..] {
        let fx =
            s.process_dsa(&mut env, &counters, &p.peer, V24, &dsa(SMALLEST, &p.collateral, None));
        assert_eq!(statuses(&fx), vec![(p.peer, ACCEPTED, PoolMessage::MsgNoErr)]);
    }
    // Three of at most twenty: not ready until the queue times out.
    assert!(!s.is_session_ready(env.now));
    assert!(s.tick(&mut env).is_empty());
    env.now += QUEUE_TIMEOUT;
    assert!(s.is_session_ready(env.now));
    let fx = s.tick(&mut env);
    assert!(matches!(fx.as_slice(), [ServerEffect::RelayQueue(q)] if q.ready()));
    assert_eq!(s.state(), PoolState::AcceptingEntries);

    // Entries: the last one completes the pool and the final transaction goes out.
    for p in &ps[..2] {
        let fx = s.process_dsi(&mut env, &p.peer, p.entry());
        assert_eq!(statuses(&fx)[0], (p.peer, ACCEPTED, PoolMessage::MsgEntriesAdded));
    }
    let fx = s.process_dsi(&mut env, &3, ps[2].entry());
    assert_eq!(s.state(), PoolState::Signing);
    let finals: Vec<_> = fx
        .iter()
        .filter_map(|e| match e {
            ServerEffect::FinalTx {
                peer,
                session_id,
                tx,
            } => Some((*peer, *session_id, tx.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(finals.iter().map(|f| f.0).collect::<Vec<_>>(), vec![1, 2, 3]);
    let final_tx = finals[0].2.clone();
    let mut sorted = final_tx.clone();
    sort_bip69(&mut sorted);
    assert_eq!(sorted, final_tx);
    assert_eq!((final_tx.inputs().len(), final_tx.outputs().len()), (3, 3));

    // Each client finds its coins in it and signs its own input.
    for p in &ps {
        let mut client = ClientSession {
            state: PoolState::AcceptingEntries,
            session_id: 999_999,
            session_denom: SMALLEST,
            entries: vec![ClientEntry {
                inputs: p.inputs.clone(),
                outputs: p.outputs.clone(),
            }],
            ..ClientSession::default()
        };
        let FinalTxOutcome::Sign {
            inputs,
        } = client.process_final_tx(&env.chain, 999_999, final_tx.clone(), false, env.now)
        else {
            panic!("participant {} refuses", p.peer);
        };
        assert_eq!(inputs.len(), 1);
        assert_eq!(final_tx.inputs()[inputs[0]].prevout(), p.inputs[0].prevout());
    }

    // Signatures: the last completes it and the masternode commits and signs the DSTX.
    for p in &ps[..2] {
        let fx = s.process_dss(&mut env, &p.peer, p.signed(&final_tx));
        assert!(statuses(&fx).is_empty(), "{fx:?}");
    }
    let fx = s.process_dss(&mut env, &3, ps[2].signed(&final_tx));
    let committed = env.committed.pop().expect("committed");
    assert!(committed.inputs().iter().all(|i| !i.script_sig().as_bytes().is_empty()));
    let txid = dstx::txid(&committed);
    let [
        ServerEffect::AddDstx {
            txid: added,
            dstx,
        },
        ServerEffect::RelayDstxInv(relayed),
        rest @ ..,
    ] = fx.as_slice()
    else {
        panic!("{fx:?}");
    };
    assert_eq!((*added, *relayed), (txid, txid));
    assert_eq!(dstx.tx(), &committed);
    assert_eq!(dstx::check_dstx_signature(dstx, &env.op.public()), BlsVerdict::Valid);
    assert!(dstx::is_valid_structure(dstx, mainnet_limits(), false).valid);
    let completes: Vec<_> = rest
        .iter()
        .map(|e| match e {
            ServerEffect::Complete {
                peer,
                session_id: 999_999,
                message: PoolMessage::MsgSuccess,
            } => *peer,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(completes, vec![1, 2, 3]);
    assert_eq!(
        (s.state(), s.session_id(), s.entries().len(), s.collateral_count()),
        (PoolState::Idle, 0, 0, 0)
    );
    // The session's queues went with it.
    assert!(s.queues().is_empty());
}

/// `ProcessDSACCEPT`'s and `CreateNewSession`'s refusals.
#[test]
fn server_dsa_rejections() {
    let mut env = Env::new();
    let c = env.chain.collateral();
    let counters = MixingCounters::default();
    let reject = |s: &mut ServerSession<u8>,
                  env: &mut Env,
                  counters: &MixingCounters,
                  version,
                  dsa: DsAccept| {
        let fx = s.process_dsa(env, counters, &1, version, &dsa);
        let st = statuses(&fx);
        assert_eq!(st.len(), 1, "{fx:?}");
        assert_eq!(st[0].1, REJECTED);
        st[0].2
    };
    let mut s = ServerSession::new(mainnet_limits());

    env.registered = false;
    assert_eq!(
        reject(&mut s, &mut env, &counters, V24, dsa(SMALLEST, &c, None)),
        PoolMessage::ErrMnList
    );
    env.registered = true;

    // A fifth of the enabled masternodes' queues have not passed since this one's last.
    let recent = MixingCounters {
        dsq_count: 5,
        masternodes: [(
            env.pro_tx_hash,
            MasternodeMixing {
                last_dsq: 5,
                mixing_tx_count: 0,
            },
        )]
        .into(),
    };
    assert_eq!(
        reject(&mut s, &mut env, &recent, V24, dsa(SMALLEST, &c, None)),
        PoolMessage::ErrRecent
    );

    assert_eq!(reject(&mut s, &mut env, &counters, V24, dsa(3, &c, None)), PoolMessage::ErrDenom);
    let mut bad = c.clone();
    bad = dash_primitives::Transaction::new(
        bad.version(),
        bad.tx_type(),
        bad.inputs().clone(),
        bad.outputs().clone(),
        1,
        bad.extra_payload().clone(),
    );
    assert_eq!(
        reject(&mut s, &mut env, &counters, V24, dsa(SMALLEST, &bad, None)),
        PoolMessage::ErrInvalidCollateral
    );
    let both = Some(FLAG_PROMOTION | FLAG_DEMOTION);
    assert_eq!(
        reject(&mut s, &mut env, &counters, V24, dsa(SMALLEST, &c, both)),
        PoolMessage::ErrVersion
    );
    // A rebalance needs V24 at the tip and a peer that speaks it.
    assert_eq!(
        reject(&mut s, &mut env, &counters, V24, dsa(SMALLEST, &c, Some(FLAG_PROMOTION))),
        PoolMessage::ErrVersion
    );
    env.v24 = true;
    assert_eq!(
        reject(&mut s, &mut env, &counters, LEGACY, dsa(SMALLEST, &c, Some(FLAG_PROMOTION))),
        PoolMessage::ErrVersion
    );
    assert_eq!((s.state(), s.session_id()), (PoolState::Idle, 0));
    assert!(s.queues().is_empty());
}

/// The tip's masternode list as a masternode sees it.
struct Tip(Vec<MixingMasternode>);

impl TipMasternodes for Tip {
    fn valid_mn(&self, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        self.0.iter().find(|m| m.pro_tx_hash == *pro_tx_hash).cloned()
    }

    fn enabled_count(&self) -> i32 {
        100
    }
}

#[derive(Debug, Eq, PartialEq)]
enum MetaCall {
    Threshold(TxHash, i32),
    Allow(TxHash),
    Disallow(TxHash),
    Valid(TxHash),
}

#[derive(Default)]
struct RecordingMeta {
    calls: RefCell<Vec<MetaCall>>,
}

impl MixingMeta for RecordingMeta {
    fn is_mixing_threshold_exceeded(&self, pro_tx_hash: &TxHash, mn_count: i32) -> bool {
        self.calls.borrow_mut().push(MetaCall::Threshold(*pro_tx_hash, mn_count));
        false
    }

    fn allow_mixing(&mut self, pro_tx_hash: &TxHash) {
        self.calls.borrow_mut().push(MetaCall::Allow(*pro_tx_hash));
    }

    fn disallow_mixing(&mut self, pro_tx_hash: &TxHash) {
        self.calls.borrow_mut().push(MetaCall::Disallow(*pro_tx_hash));
    }

    fn is_valid_for_mixing_txes(&self, pro_tx_hash: &TxHash) -> bool {
        self.calls.borrow_mut().push(MetaCall::Valid(*pro_tx_hash));
        true
    }
}

/// A queue at `SMALLEST`, not ready, signed by `op`.
fn signed_dsq(op: &Operator, pro_tx_hash: TxHash, time: i64) -> DsQueue {
    let mut dsq = DsQueue::new(SMALLEST, pro_tx_hash, time, false, Vec::new());
    dsq = dash_p2p_core::DsQueue::new(
        dsq.denom(),
        dsq.pro_tx_hash(),
        dsq.time(),
        dsq.ready(),
        op.sign(&dsq.signature_hash()),
    );
    dsq
}

/// Core: coinjoin/server.cpp:186-190 checks the threshold before allowing a queue;
/// :89-111 only checks the threshold for the first `dsa`.
#[test]
fn mixing_meta_seam_preserves_core_call_order() {
    let mut env = Env::new();
    let other_op = Operator::new(2);
    let other_hash = u256(0x55);
    let tip = Tip(vec![MixingMasternode {
        pro_tx_hash: other_hash,
        collateral: outpoint(u256(0x56), 0),
        operator_key: other_op.public(),
    }]);
    let mut meta = RecordingMeta::default();
    let mut counters = MixingCounters::default();
    let mut session = ServerSession::<u8>::new(mainnet_limits());
    let mut concrete = session.clone();
    let dsq = signed_dsq(&other_op, other_hash, NOW);
    let outcome = session.process_dsq(&mut meta as &mut dyn MixingMeta, &tip, dsq.clone(), NOW);
    assert!(matches!(outcome, DsqOutcome::Relay(_)));
    assert_eq!(concrete.process_dsq(&mut counters, &tip, dsq, NOW), outcome);
    assert_eq!(
        *meta.calls.borrow(),
        vec![MetaCall::Threshold(other_hash, 100), MetaCall::Allow(other_hash)]
    );

    let collateral = env.chain.collateral();
    let accept = dsa(SMALLEST, &collateral, None);
    let fx = session.process_dsa(&mut env, &meta as &dyn MixingMeta, &1, V24, &accept);
    assert_eq!(statuses(&fx), vec![(1, ACCEPTED, PoolMessage::MsgNoErr)]);
    assert_eq!(
        statuses(&concrete.process_dsa(&mut env, &counters, &1, V24, &accept)),
        statuses(&fx)
    );
    assert_eq!(session.state(), concrete.state());
    assert_eq!(session.session_id(), concrete.session_id());
    assert_eq!(session.collateral_count(), concrete.collateral_count());
    assert_eq!(session.queues().len(), concrete.queues().len());
    assert_eq!(
        *meta.calls.borrow(),
        vec![
            MetaCall::Threshold(other_hash, 100),
            MetaCall::Allow(other_hash),
            MetaCall::Threshold(env.pro_tx_hash, 100),
        ]
    );

    let collateral = env.chain.collateral();
    let fx = session.process_dsa(&mut env, &meta, &2, V24, &dsa(SMALLEST, &collateral, None));
    assert_eq!(statuses(&fx), vec![(2, ACCEPTED, PoolMessage::MsgNoErr)]);
    assert_eq!(meta.calls.borrow().len(), 3);
}

/// Core: coinjoin/server.cpp:91-98, :160-165, :194-196 and :307 (`CCoinJoinServer::m_queueman`)
///
/// A masternode keeps the queues it receives in the same manager as its own. Its own queue,
/// relayed back to it, refuses a new session with ERR_RECENT, and a session reset forgets the
/// queues it received.
#[test]
fn server_dsq_shares_the_session_queue_manager() {
    let mut env = Env::new();
    let other_op = Operator::new(2);
    let other = MixingMasternode {
        pro_tx_hash: u256(0x55),
        collateral: outpoint(u256(0x56), 0),
        operator_key: other_op.public(),
    };
    let me = MixingMasternode {
        pro_tx_hash: env.pro_tx_hash,
        collateral: env.collateral,
        operator_key: env.op.public(),
    };
    let tip = Tip(vec![me, other.clone()]);
    let fresh = MixingCounters::default;
    let c = env.chain.collateral();

    let mut s = ServerSession::new(mainnet_limits());
    let own = signed_dsq(&env.op, env.pro_tx_hash, NOW);
    assert!(matches!(s.process_dsq(&mut fresh(), &tip, own, NOW), DsqOutcome::Relay(_)));
    let fx = s.process_dsa(&mut env, &fresh(), &1, V24, &dsa(SMALLEST, &c, None));
    assert_eq!(statuses(&fx), vec![(1, REJECTED, PoolMessage::ErrRecent)]);

    let mut s = ServerSession::new(limits(2, 3));
    let fx = s.process_dsa(&mut env, &fresh(), &1, V24, &dsa(SMALLEST, &c, None));
    assert_eq!(s.state(), PoolState::Queue, "{fx:?}");
    let theirs = signed_dsq(&other_op, other.pro_tx_hash, NOW);
    assert!(matches!(s.process_dsq(&mut fresh(), &tip, theirs, NOW), DsqOutcome::Relay(_)));
    let again = signed_dsq(&other_op, other.pro_tx_hash, NOW + 1);
    assert_eq!(s.process_dsq(&mut fresh(), &tip, again.clone(), NOW + 1), DsqOutcome::Ignored);
    // The lone participant times out; the fee draw (99) charges nothing, and the session resets.
    env.now = NOW + QUEUE_TIMEOUT;
    s.tick(&mut env);
    assert_eq!(s.state(), PoolState::Idle);
    assert!(s.queues().is_empty());
    assert!(matches!(s.process_dsq(&mut fresh(), &tip, again, env.now), DsqOutcome::Relay(_)));

    // `CheckTimeout` prunes received queues first (server.cpp:714), even with no session.
    let mut s = ServerSession::new(limits(2, 3));
    let theirs = signed_dsq(&other_op, other.pro_tx_hash, env.now);
    assert!(matches!(s.process_dsq(&mut fresh(), &tip, theirs, env.now), DsqOutcome::Relay(_)));
    env.now += QUEUE_TIMEOUT;
    s.tick(&mut env);
    assert_eq!(s.queues().len(), 1);
    env.now += 1;
    s.tick(&mut env);
    assert!(s.queues().is_empty());
}

/// `AddUserToExistingSession`'s refusals, and a full queue.
#[test]
fn server_join_rejections() {
    let mut env = Env::new();
    env.v24 = true;
    let counters = MixingCounters::default();
    let c1 = env.chain.collateral();
    let mut s = ServerSession::new(limits(2, 3));
    let fx = s.process_dsa(&mut env, &counters, &1, V24, &dsa(SMALLEST, &c1, Some(FLAG_PROMOTION)));
    assert_eq!(statuses(&fx), vec![(1, ACCEPTED, PoolMessage::MsgNoErr)]);

    let join = |s: &mut ServerSession<u8>, env: &mut Env, version, dsa: DsAccept| {
        statuses(&s.process_dsa(env, &counters, &2, version, &dsa))[0]
    };
    let c2 = env.chain.collateral();
    let other_denom = denom::larger_adjacent_denom(SMALLEST);
    assert_eq!(
        join(&mut s, &mut env, V24, dsa(other_denom, &c2, None)),
        (2, REJECTED, PoolMessage::ErrDenom)
    );
    // A re-signed collateral spending the same coin is the same participant.
    let mut resigned = c1.clone();
    {
        let mut outputs = resigned.outputs().clone();
        outputs[0] = dash_primitives::TxOut::from_duffs(
            outputs[0].value() - 1,
            outputs[0].script_pubkey().clone(),
        );
        resigned = dash_primitives::Transaction::new(
            resigned.version(),
            resigned.tx_type(),
            resigned.inputs().clone(),
            outputs,
            resigned.lock_time(),
            resigned.extra_payload().clone(),
        );
    }
    assert_eq!(
        join(&mut s, &mut env, V24, dsa(SMALLEST, &resigned, None)),
        (2, REJECTED, PoolMessage::ErrAlreadyHave)
    );
    // A legacy client cannot join a rebalance session.
    assert_eq!(
        join(&mut s, &mut env, LEGACY, dsa(SMALLEST, &c2, None)),
        (2, REJECTED, PoolMessage::ErrVersion)
    );
    assert_eq!(
        join(&mut s, &mut env, V24, dsa(SMALLEST, &c2, Some(FLAG_PROMOTION))),
        (2, ACCEPTED, PoolMessage::MsgNoErr)
    );
    // Full at the most participants (the sides are covered: two promotions, no outputs).
    let c3 = env.chain.collateral();
    assert_eq!(
        join(&mut s, &mut env, V24, dsa(SMALLEST, &c3, Some(FLAG_PROMOTION))),
        (2, ACCEPTED, PoolMessage::MsgNoErr)
    );
    assert!(s.is_session_ready(env.now));
    let c4 = env.chain.collateral();
    assert_eq!(
        join(&mut s, &mut env, V24, dsa(SMALLEST, &c4, None)),
        (2, REJECTED, PoolMessage::ErrQueueFull)
    );

    // A legacy participant first: a rebalance participant cannot join.
    let mut s = ServerSession::new(limits(2, 3));
    let fx = s.process_dsa(&mut env, &counters, &1, LEGACY, &dsa(SMALLEST, &c1, None));
    assert_eq!(statuses(&fx), vec![(1, ACCEPTED, PoolMessage::MsgNoErr)]);
    assert_eq!(
        join(&mut s, &mut env, V24, dsa(SMALLEST, &c2, Some(FLAG_DEMOTION))),
        (2, REJECTED, PoolMessage::ErrVersion)
    );
}

/// Core v24.0.0: coinjoin/server.cpp:847-880, :905-908 (MNO-18).
#[test]
fn server_addentry_binds_entries_to_accepted_collaterals() {
    let mut env = Env::new();
    let ps = vec![Participant::standard(&mut env, 1), Participant::standard(&mut env, 2)];
    let unadmitted = Participant::standard(&mut env, 3);
    let mut s = accepting_session(&mut env, limits(2, 2), &ps, &[], V24);

    // Core v24.0.0: coinjoin/server.cpp:870-874 rejects without consuming collateral.
    let fx = s.process_dsi(&mut env, &3, unadmitted.entry());
    assert_eq!(statuses(&fx), vec![(3, REJECTED, PoolMessage::ErrSession)]);
    assert!(s.entries().is_empty());
    assert_eq!(fx.len(), 1);
    assert_eq!(s.state(), PoolState::AcceptingEntries);

    // An accepted collateral covers exactly one entry.
    let fx = s.process_dsi(&mut env, &1, ps[0].entry());
    assert_eq!(statuses(&fx)[0], (1, ACCEPTED, PoolMessage::MsgEntriesAdded));
    let mut again = Participant::standard(&mut env, 1);
    again.collateral = ps[0].collateral.clone();
    let fx = s.process_dsi(&mut env, &1, again.entry());
    assert_eq!(statuses(&fx), vec![(1, REJECTED, PoolMessage::ErrAlreadyHave)]);
    assert_eq!(s.entries().len(), 1);
    assert_eq!(fx.len(), 1);

    // An accepted, unused collateral is checked again: its coin is gone now.
    let spent = ps[1].collateral.inputs()[0].prevout();
    env.chain.coins.remove(&spent);
    let fx = s.process_dsi(&mut env, &2, ps[1].entry());
    assert_eq!(statuses(&fx), vec![(2, REJECTED, PoolMessage::ErrInvalidCollateral)]);
    assert_eq!(s.entries().len(), 1);
}

/// Core: test/coinjoin_inouts_tests.cpp:401 server_addentry_rejects_entries_once_the_session_finalized
///
/// A straggler's entry after a timed-out session was finalized without it.
#[test]
fn server_addentry_rejects_entries_once_the_session_finalized() {
    let mut env = Env::new();
    let ps: Vec<_> = (1..=3).map(|p| Participant::standard(&mut env, p)).collect();
    let mut s = accepting_session(&mut env, limits(2, 3), &ps, &[], V24);
    for p in &ps[..2] {
        s.process_dsi(&mut env, &p.peer, p.entry());
    }
    env.now += QUEUE_TIMEOUT;
    let fx = s.tick(&mut env);
    assert_eq!(s.state(), PoolState::Signing);
    assert_eq!(fx.iter().filter(|e| matches!(e, ServerEffect::FinalTx { .. })).count(), 2);
    // No fee: the scheduler's coin flip spared the straggler.
    assert!(!fx.iter().any(|e| matches!(e, ServerEffect::ConsumeCollateral(_))));

    let fx = s.process_dsi(&mut env, &3, ps[2].entry());
    assert_eq!(statuses(&fx), vec![(3, REJECTED, PoolMessage::ErrSession)]);
    assert_eq!(s.entries().len(), 2);
}

/// `ChargeFees` on the timeout path: a third of the time one participant that sent no entry pays.
#[test]
fn server_timeout_charges_a_missing_participant() {
    let mut env = Env::new();
    let ps: Vec<_> = (1..=3).map(|p| Participant::standard(&mut env, p)).collect();
    let mut s = accepting_session(&mut env, limits(2, 3), &ps, &[], V24);
    for p in &ps[..2] {
        s.process_dsi(&mut env, &p.peer, p.entry());
    }
    env.now += QUEUE_TIMEOUT;
    // Charge (0 <= 33); one offender of three is not "nearly all"; pick it.
    env.rand.extend([0, 0]);
    let fx = s.tick(&mut env);
    let charged: Vec<_> = fx
        .iter()
        .filter_map(|e| match e {
            ServerEffect::ConsumeCollateral(c) => Some(c.clone()),
            _ => None,
        })
        .collect();
    // Core v24.0.0: coinjoin/server.cpp:420-422 selects the missing collateral;
    // :444-449 relays the final transaction before consuming it.
    assert_eq!(charged, vec![ps[2].collateral.clone()]);
    assert!(matches!(
        fx.as_slice(),
        [
            ServerEffect::FinalTx {
                peer: 1,
                ..
            },
            ServerEffect::FinalTx {
                peer: 2,
                ..
            },
            ServerEffect::ConsumeCollateral(_),
        ]
    ));
    assert_eq!(s.state(), PoolState::Signing);
}

/// Core: test/coinjoin_inouts_tests.cpp:431 server_finalization_rechecks_live_side_coverage
///
/// Three demotions and one promotion of a session admitted with two promotions: when it times
/// out, the entries' sides (one promoter) are not covered, so nothing is built; the next
/// timeout check resets it. This pins `check_pool`'s outcome, which tests the live side counts
/// before it would call `create_final_transaction`; the latter's own re-check of the same
/// counts is defensive and unreachable in the single-threaded port (D7).
#[test]
fn server_finalization_rechecks_live_side_coverage() {
    let mut env = Env::new();
    env.v24 = true;
    let mut ps: Vec<_> = (1..=3).map(|p| Participant::demotion(&mut env, p)).collect();
    ps.extend((4..=5).map(|p| Participant::promotion(&mut env, p)));
    let flags = [
        Some(FLAG_DEMOTION),
        Some(FLAG_DEMOTION),
        Some(FLAG_DEMOTION),
        Some(FLAG_PROMOTION),
        Some(FLAG_PROMOTION),
    ];
    let mut s = accepting_session(&mut env, limits(2, 5), &ps, &flags, V24);

    for p in &ps[..3] {
        let fx = s.process_dsi(&mut env, &p.peer, p.entry());
        assert_eq!(statuses(&fx)[0], (p.peer, ACCEPTED, PoolMessage::MsgEntriesAdded));
    }
    env.now += QUEUE_TIMEOUT;
    let fx = s.process_dsi(&mut env, &4, ps[3].entry());
    assert_eq!(statuses(&fx)[0], (4, ACCEPTED, PoolMessage::MsgEntriesAdded));
    assert_eq!(s.state(), PoolState::AcceptingEntries);
    assert_eq!(s.entries().len(), 4);
    assert!(!fx.iter().any(|e| matches!(e, ServerEffect::FinalTx { .. })));

    s.tick(&mut env);
    assert_eq!(s.state(), PoolState::Idle);
}

/// A rebalance participant's entry must have the shape it declared, at its cost.
#[test]
fn server_rebalance_entry_must_match_its_declared_shape() {
    let mut env = Env::new();
    env.v24 = true;
    let ps = vec![Participant::promotion(&mut env, 1), Participant::promotion(&mut env, 2)];
    let mut s = accepting_session(
        &mut env,
        limits(2, 2),
        &ps,
        &[Some(FLAG_PROMOTION), Some(FLAG_PROMOTION)],
        V24,
    );
    let mut standard = Participant::standard(&mut env, 1);
    standard.collateral = ps[0].collateral.clone();
    let fx = s.process_dsi(&mut env, &1, standard.entry());
    assert_eq!(statuses(&fx), vec![(1, REJECTED, PoolMessage::ErrSizeMismatch)]);
    assert!(
        matches!(fx.first(), Some(ServerEffect::ConsumeCollateral(c)) if *c == ps[0].collateral)
    );
}

/// Core: coinjoin/server.cpp:70-75 (`ProcessDSACCEPT`).
#[test]
fn pre_decode_dsa_reply_reports_full_session() {
    let mut env = Env::new();
    let ps: Vec<_> = (1..=2).map(|p| Participant::standard(&mut env, p)).collect();
    let mut session = ServerSession::<u8>::new(limits(2, 3));
    assert_eq!(session.pre_decode_dsa_reply(NOW), None);
    let meta = MixingCounters::default();
    for p in &ps {
        session.process_dsa(&mut env, &meta, &p.peer, V24, &dsa(SMALLEST, &p.collateral, None));
    }
    assert_eq!(session.pre_decode_dsa_reply(NOW), None);
    assert_eq!(session.pre_decode_dsa_reply(NOW + QUEUE_TIMEOUT), Some(PoolMessage::ErrQueueFull));
    env.now += QUEUE_TIMEOUT;
    session.tick(&mut env);
    assert_eq!(session.pre_decode_dsa_reply(env.now), Some(PoolMessage::ErrQueueFull));
    assert_eq!(session.state(), PoolState::AcceptingEntries);
}

/// Core: coinjoin/server.cpp:202-207 (`ProcessDSVIN`).
#[test]
fn pre_decode_dsi_reply_reports_unready_session() {
    let mut env = Env::new();
    let ps: Vec<_> = (1..=2).map(|p| Participant::standard(&mut env, p)).collect();
    let mut session = ServerSession::<u8>::new(limits(2, 3));
    assert_eq!(session.pre_decode_dsi_reply(NOW), Some(PoolMessage::ErrSession));
    let meta = MixingCounters::default();
    for p in &ps {
        session.process_dsa(&mut env, &meta, &p.peer, V24, &dsa(SMALLEST, &p.collateral, None));
    }
    assert_eq!(session.pre_decode_dsi_reply(NOW), Some(PoolMessage::ErrSession));
    assert_eq!(session.pre_decode_dsi_reply(NOW + QUEUE_TIMEOUT), None);
    env.now += QUEUE_TIMEOUT;
    session.tick(&mut env);
    assert_eq!(session.pre_decode_dsi_reply(env.now), None);
    assert_eq!(session.state(), PoolState::AcceptingEntries);
}

/// Core: coinjoin/server.cpp:238-241 (`ProcessDSSIGNFINALTX`).
#[test]
fn pre_decode_dss_state_reply_reports_wrong_state() {
    let mut env = Env::new();
    let ps: Vec<_> = (1..=2).map(|p| Participant::standard(&mut env, p)).collect();
    let mut session = ServerSession::<u8>::new(limits(2, 2));
    assert_eq!(session.pre_decode_dss_state_reply(), Some(PoolMessage::ErrSession));
    session = accepting_session(&mut env, limits(2, 2), &ps, &[], V24);
    assert_eq!(session.pre_decode_dss_state_reply(), Some(PoolMessage::ErrSession));
    for p in &ps {
        session.process_dsi(&mut env, &p.peer, p.entry());
    }
    assert_eq!(session.pre_decode_dss_state_reply(), None);
    assert_eq!(session.state(), PoolState::Signing);
}

/// Core: coinjoin/server.cpp:243-250 (`ProcessDSSIGNFINALTX`).
#[test]
fn pre_decode_dss_participant_reply_reports_unknown_peer() {
    let mut env = Env::new();
    let ps: Vec<_> = (1..=2).map(|p| Participant::standard(&mut env, p)).collect();
    let mut session = accepting_session(&mut env, limits(2, 2), &ps, &[], V24);
    assert!(!session.is_session_participant(&1));
    for p in &ps {
        session.process_dsi(&mut env, &p.peer, p.entry());
    }
    assert_eq!(session.state(), PoolState::Signing);
    assert!(session.is_session_participant(&1));
    assert!(!session.is_session_participant(&99));
    assert_eq!(session.pre_decode_dss_participant_reply(&1), None);
    assert_eq!(session.pre_decode_dss_participant_reply(&99), Some(PoolMessage::ErrInvalidInput));
    assert_eq!(session.state(), PoolState::Signing);
    assert_eq!(session.entries().len(), 2);
}

/// Core: coinjoin/server.cpp:254-263 (`ProcessDSSIGNFINALTX`).
#[test]
fn pre_decode_dss_count_reply_reports_over_cap() {
    let session = ServerSession::<u8>::new(mainnet_limits());
    assert_eq!(session.pre_decode_dss_count_reply(0), None);
    assert_eq!(session.pre_decode_dss_count_reply(180), None);
    assert_eq!(session.pre_decode_dss_count_reply(181), Some(PoolMessage::ErrMaximum));
    let session = ServerSession::<u8>::new(limits(2, 2));
    assert_eq!(session.pre_decode_dss_count_reply(18), None);
    assert_eq!(session.pre_decode_dss_count_reply(19), Some(PoolMessage::ErrMaximum));
    assert_eq!(session.state(), PoolState::Idle);
}

/// Core: test/coinjoin_inouts_tests.cpp:234 server_signfinaltx_nonparticipant_cannot_abort_session
/// and :272 server_signfinaltx_participant_oversized_count_is_rejected_locally
#[test]
fn server_dss_from_outside_or_oversized_leaves_the_session() {
    let mut env = Env::new();
    let ps: Vec<_> = (1..=3).map(|p| Participant::standard(&mut env, p)).collect();
    let mut s = accepting_session(&mut env, limits(3, 3), &ps, &[], V24);

    // Not signing yet.
    let fx = s.process_dss(&mut env, &1, SignedInputs::Inputs(Vec::new()));
    assert_eq!(statuses(&fx), vec![(1, REJECTED, PoolMessage::ErrSession)]);

    for p in &ps {
        s.process_dsi(&mut env, &p.peer, p.entry());
    }
    assert_eq!(s.state(), PoolState::Signing);

    // The participant is gone from our connections, so a session-wide rejection would reset it.
    env.disconnected.extend([1, 2, 3]);
    let signed = ps[0].signed(s.final_tx());
    let fx = s.process_dss(&mut env, &42, signed);
    assert_eq!(statuses(&fx), vec![(42, REJECTED, PoolMessage::ErrInvalidInput)]);
    assert_eq!((s.state(), s.entries().len()), (PoolState::Signing, 3));

    let fx = s.process_dss(&mut env, &1, SignedInputs::OverCap);
    assert_eq!(statuses(&fx), vec![(1, REJECTED, PoolMessage::ErrMaximum)]);
    assert_eq!((s.state(), s.entries().len()), (PoolState::Signing, 3));

    // A participant's bad signature is session-wide: with nobody connected, the pool resets.
    let mut stranger = ps[0].signed(s.final_tx());
    if let SignedInputs::Inputs(inputs) = &mut stranger {
        inputs[0] = dash_primitives::TxIn::new(
            outpoint(u256(0x55), 5),
            inputs[0].script_sig().clone(),
            inputs[0].sequence(),
        );
    }
    s.process_dss(&mut env, &1, stranger);
    assert_eq!(s.state(), PoolState::Idle);
}

/// Core #7815: develop server.cpp:965-994 binds signatures to the admitted input and sender,
/// accepts identical retransmissions, and leaves accepted state unchanged on rejection (DEC-1629).
#[test]
fn server_add_script_sig_matches_and_rejects() {
    let mut env = Env::new();
    let ps: Vec<_> = (1..=3).map(|p| Participant::standard(&mut env, p)).collect();
    let mut s = accepting_session(&mut env, limits(3, 3), &ps, &[], V24);
    for p in &ps {
        s.process_dsi(&mut env, &p.peer, p.entry());
    }
    let SignedInputs::Inputs(signed) = ps[0].signed(s.final_tx()) else {
        unreachable!()
    };
    let session_failed = |fx: &[ServerEffect<u8>]| {
        statuses(fx)
            == vec![
                (1, REJECTED, PoolMessage::MsgNoErr),
                (2, REJECTED, PoolMessage::MsgNoErr),
                (3, REJECTED, PoolMessage::MsgNoErr),
            ]
    };

    // The wrong sequence.
    let mut bad_seq = signed.clone();
    bad_seq[0] = dash_primitives::TxIn::new(
        bad_seq[0].prevout(),
        bad_seq[0].script_sig().clone(),
        bad_seq[0].sequence() ^ 0xff,
    );
    assert!(session_failed(&s.process_dss(&mut env, &1, SignedInputs::Inputs(bad_seq))));
    assert!(s.entries().iter().all(|e| e.inputs.iter().all(|i| !i.has_sig)));

    // Signed once.
    assert!(
        statuses(&s.process_dss(&mut env, &1, SignedInputs::Inputs(signed.clone()))).is_empty()
    );
    let input = &s.entries()[0].inputs[0];
    assert!(input.has_sig);
    assert_eq!(input.txin.script_sig(), signed[0].script_sig());
    assert!(s.entries()[1..].iter().all(|e| !e.inputs[0].has_sig));
    let in_final = s
        .final_tx()
        .inputs()
        .iter()
        .find(|i| i.prevout() == signed[0].prevout())
        .expect("in the final tx");
    assert_eq!(in_final.script_sig(), signed[0].script_sig());

    // An identical retransmission is accepted.
    let before = s.final_tx().clone();
    assert!(
        statuses(&s.process_dss(&mut env, &1, SignedInputs::Inputs(signed.clone()))).is_empty()
    );
    assert_eq!(s.final_tx(), &before);

    // An invalid retry, or the same script on another input, is rejected.
    let mut other = signed.clone();
    other[0] = dash_primitives::TxIn::new(
        other[0].prevout(),
        vec![0x01, 0x01].into(),
        other[0].sequence(),
    );
    assert!(session_failed(&s.process_dss(&mut env, &1, SignedInputs::Inputs(other))));
    let SignedInputs::Inputs(mut copied) = ps[1].signed(s.final_tx()) else {
        unreachable!()
    };
    copied[0] = dash_primitives::TxIn::new(
        copied[0].prevout(),
        signed[0].script_sig().clone(),
        copied[0].sequence(),
    );
    assert!(session_failed(&s.process_dss(&mut env, &2, SignedInputs::Inputs(copied))));
    assert_eq!(s.entries()[0].inputs[0].txin.script_sig(), signed[0].script_sig());
    assert_eq!(s.state(), PoolState::Signing);
}

/// Core #7815: develop server.cpp:928-940 re-reads the coin after IsValidInOuts (DEC-1629).
fn entry_with_unavailable_coin_is_rejected(spent: bool) {
    let mut env = Env::new();
    let ps = [Participant::standard(&mut env, 1), Participant::standard(&mut env, 2)];
    let mut s = accepting_session(&mut env, limits(2, 2), &ps, &[], V24);
    let prevout = ps[0].inputs[0].prevout();
    let coin = env.chain.coin(&prevout).expect("funded");
    let unavailable = spent.then(|| ViewCoin {
        out: txout(-1, Vec::new()),
        in_mempool: false,
    });
    // The first read passes IsValidInOuts; the admission snapshot sees the later chainstate.
    env.coin_answers.borrow_mut().extend([(prevout, Some(coin)), (prevout, unavailable)]);
    let fx = s.process_dsi(&mut env, &1, ps[0].entry());
    assert_eq!(statuses(&fx), vec![(1, REJECTED, PoolMessage::ErrMissingTx)]);
    assert_eq!(fx.len(), 1);
    assert!(s.entries().is_empty());
    assert_eq!(s.state(), PoolState::AcceptingEntries);
    assert!(env.coin_answers.borrow().is_empty());
}

#[test]
fn server_entry_missing_coin_at_admission_is_rejected() {
    entry_with_unavailable_coin_is_rejected(false);
}

#[test]
fn server_entry_spent_coin_at_admission_is_rejected() {
    entry_with_unavailable_coin_is_rejected(true);
}

fn verifies_input(proposal: &Transaction, input: &TxIn, script: &[u8], flags: ScriptFlags) -> bool {
    let index =
        proposal.inputs().iter().position(|i| i.prevout() == input.prevout()).expect("input");
    let data = TxSigData::new(proposal);
    let checker = TransactionSignatureChecker::new(&data, index, &DirectVerifier);
    verify_script(input.script_sig().as_bytes(), script, flags, &checker).is_ok()
}

/// Core #7815: develop coinjoin_inouts_tests.cpp:250-386 checks signature admission through
/// completion with legacy and full signatures over the BIP69 proposal (DEC-1629).
#[test]
fn server_signfinaltx_completes_with_legacy_and_full_signatures() {
    let mut env = Env::new();
    let mut ps: Vec<_> = (1..=3).map(|p| Participant::standard(&mut env, p)).collect();
    ps.reverse();
    ps[2].peer = ps[0].peer;
    let mut s = accepting_session(&mut env, limits(3, 3), &ps, &[], V24);
    for p in &ps {
        let fx = s.process_dsi(&mut env, &p.peer, p.entry());
        assert_eq!(statuses(&fx)[0], (p.peer, ACCEPTED, PoolMessage::MsgEntriesAdded));
    }
    assert_eq!(s.state(), PoolState::Signing);
    let proposal = s.final_tx().clone();
    assert_ne!(proposal.inputs(), &ps.iter().flat_map(|p| p.inputs.clone()).collect::<Vec<_>>());
    assert_ne!(proposal.outputs(), &ps.iter().flat_map(|p| p.outputs.clone()).collect::<Vec<_>>());
    let flags = ScriptFlags::STANDARD | ScriptFlags::SIGPUSHONLY;
    for (entry, p) in s.entries().iter().zip(&ps) {
        assert_eq!(entry.inputs[0].prev_pub_key, input_script(&p.inputs[0]));
        env.chain.coins.remove(&p.inputs[0].prevout());
    }
    let signed: Vec<_> = ps
        .iter()
        .enumerate()
        .map(|(i, p)| {
            sign_input(
                &proposal,
                &p.inputs[0],
                &input_key(&p.inputs[0]),
                if i == 0 {
                    0x81
                } else {
                    1
                },
            )
        })
        .collect();
    let reject = |s: &mut ServerSession<u8>, env: &mut Env, peer: u8, input: TxIn| {
        let before = s.final_tx().clone();
        let entries = s.entries().iter().map(|e| e.inputs.clone()).collect::<Vec<_>>();
        let fx = s.process_dss(env, &peer, SignedInputs::Inputs(vec![input]));
        assert_eq!(
            statuses(&fx),
            ps.iter().map(|p| (p.peer, REJECTED, PoolMessage::MsgNoErr)).collect::<Vec<_>>()
        );
        assert_eq!(s.final_tx(), &before);
        assert_eq!(s.entries().iter().map(|e| e.inputs.clone()).collect::<Vec<_>>(), entries);
        assert_eq!(s.state(), PoolState::Signing);
    };
    reject(&mut s, &mut env, ps[1].peer, signed[0].clone());

    let mut outputs = proposal.outputs().clone();
    outputs[0] = txout(SMALLEST_DENOMINATION, key(0xff));
    let modified = tx(proposal.inputs().clone(), outputs);
    let changed = sign_input(&modified, &ps[0].inputs[0], &input_key(&ps[0].inputs[0]), 0x81);
    assert!(verifies_input(&modified, &changed, &input_script(&ps[0].inputs[0]), flags));
    reject(&mut s, &mut env, ps[0].peer, changed);

    let other_key = SecretKey::from_secret_bytes([0x33; 32]).expect("fixed key");
    let other = sign_input(&proposal, &ps[0].inputs[0], &other_key, 0x81);
    assert!(verifies_input(&proposal, &other, &key_script(&other_key), flags));
    assert!(!verifies_input(&proposal, &other, &input_script(&ps[0].inputs[0]), flags));
    reject(&mut s, &mut env, ps[0].peer, other);

    let mut padded = vec![0x51];
    padded.extend(signed[0].script_sig().as_bytes());
    let padded = TxIn::new(signed[0].prevout(), padded.into(), signed[0].sequence());
    assert!(verifies_input(&proposal, &padded, &input_script(&ps[0].inputs[0]), ScriptFlags::P2SH));
    reject(&mut s, &mut env, ps[0].peer, padded);

    let mut nonpush = vec![0x61];
    nonpush.extend(signed[0].script_sig().as_bytes());
    let nonpush = TxIn::new(signed[0].prevout(), nonpush.into(), signed[0].sequence());
    assert!(verifies_input(
        &proposal,
        &nonpush,
        &input_script(&ps[0].inputs[0]),
        ScriptFlags::STANDARD,
    ));
    reject(&mut s, &mut env, ps[0].peer, nonpush);

    let submit = |s: &mut ServerSession<u8>, env: &mut Env, i: usize, input: TxIn| {
        let fx = s.process_dss(env, &ps[i].peer, SignedInputs::Inputs(vec![input]));
        assert!(statuses(&fx).is_empty(), "{fx:?}");
    };
    submit(&mut s, &mut env, 0, signed[0].clone());
    let accepted = s.final_tx().clone();
    reject(&mut s, &mut env, ps[1].peer, signed[0].clone());
    submit(&mut s, &mut env, 0, signed[0].clone());
    let alternate = sign_input(&proposal, &ps[0].inputs[0], &input_key(&ps[0].inputs[0]), 1);
    assert_ne!(alternate.script_sig(), signed[0].script_sig());
    submit(&mut s, &mut env, 0, alternate);
    assert_eq!(s.final_tx(), &accepted);
    reject(&mut s, &mut env, ps[0].peer, ps[0].inputs[0].clone());
    for (i, input) in signed.into_iter().enumerate().skip(1) {
        submit(&mut s, &mut env, i, input);
    }
    assert_eq!(s.state(), PoolState::Idle);
    let committed = env.committed.last().expect("committed");
    for p in &ps {
        let input = committed
            .inputs()
            .iter()
            .find(|i| i.prevout() == p.inputs[0].prevout())
            .expect("input");
        assert!(verifies_input(committed, input, &input_script(&p.inputs[0]), flags));
    }
}

/// Core v24.0.0: coinjoin/server.cpp:481-488 notifies participants before resetting (MN4-11).
#[test]
fn server_commit_refused_notifies_before_reset() {
    let mut env = Env::new();
    env.mempool_accepts = false;
    let ps: Vec<_> = (1..=3).map(|p| Participant::standard(&mut env, p)).collect();
    let mut s = accepting_session(&mut env, limits(3, 3), &ps, &[], V24);
    for p in &ps {
        s.process_dsi(&mut env, &p.peer, p.entry());
    }
    let session_id = s.session_id();
    let final_tx = s.final_tx().clone();
    let mut fx = Vec::new();
    for p in &ps {
        fx = s.process_dss(&mut env, &p.peer, p.signed(&final_tx));
    }
    assert_eq!(env.committed.len(), 1);
    assert!(matches!(fx.as_slice(), [
        ServerEffect::Complete { peer: 1, session_id: a, message: PoolMessage::ErrInvalidTx },
        ServerEffect::Complete { peer: 2, session_id: b, message: PoolMessage::ErrInvalidTx },
        ServerEffect::Complete { peer: 3, session_id: c, message: PoolMessage::ErrInvalidTx },
    ] if (*a, *b, *c) == (session_id, session_id, session_id)));
    assert_eq!((s.state(), s.session_id(), s.collateral_count()), (PoolState::Idle, 0, 0));
}

/// Core v24.0.0: coinjoin/server.cpp:1248-1255 refuses reused collateral inputs (MNO-17).
#[test]
fn server_dsa_reused_collateral_prevout_is_not_a_participant() {
    let mut env = Env::new();
    let p = Participant::standard(&mut env, 1);
    let mut s = ServerSession::new(limits(2, 3));
    let counters = MixingCounters::default();
    let fx = s.process_dsa(&mut env, &counters, &1, V24, &dsa(SMALLEST, &p.collateral, None));
    // Core v24.0.0: coinjoin/server.cpp:1128-1136 accepts the first collateral.
    assert_eq!(statuses(&fx), vec![(1, ACCEPTED, PoolMessage::MsgNoErr)]);
    let input = &p.collateral.inputs()[0];
    let resigned = tx(
        vec![TxIn::new(input.prevout(), vec![0x51].into(), input.sequence())],
        p.collateral.outputs().clone(),
    );
    assert_ne!(dstx::txid(&resigned), dstx::txid(&p.collateral));
    for collateral in [&p.collateral, &resigned] {
        let fx = s.process_dsa(&mut env, &counters, &2, V24, &dsa(SMALLEST, collateral, None));
        assert_eq!(statuses(&fx), vec![(2, REJECTED, PoolMessage::ErrAlreadyHave)]);
        assert_eq!(fx.len(), 1);
        assert_eq!((s.collateral_count(), s.state()), (1, PoolState::Queue));
    }
}

/// Core v24.0.0: coinjoin/server.cpp:915-920 consumes an empty entry's collateral (MNO-19).
#[test]
fn server_empty_entry_is_charged_without_taking_a_slot() {
    let mut env = Env::new();
    let ps = [Participant::standard(&mut env, 1), Participant::standard(&mut env, 2)];
    let mut s = accepting_session(&mut env, limits(2, 2), &ps, &[], V24);
    let empty = DsEntry::new(Vec::new(), ps[0].collateral.clone(), Vec::new());
    let fx = s.process_dsi(&mut env, &1, empty);
    assert_eq!(statuses(&fx), vec![(1, REJECTED, PoolMessage::ErrSizeMismatch)]);
    assert!(
        matches!(fx.as_slice(), [ServerEffect::ConsumeCollateral(c), ServerEffect::Status { .. }]
        if *c == ps[0].collateral)
    );
    assert!(s.entries().is_empty());
    assert_eq!(s.state(), PoolState::AcceptingEntries);
    // Core v24.0.0: coinjoin/server.cpp:982-986 records only a valid entry.
    let fx = s.process_dsi(&mut env, &2, ps[1].entry());
    assert_eq!(statuses(&fx)[0], (2, ACCEPTED, PoolMessage::MsgEntriesAdded));
    assert_eq!(s.entries().len(), 1);
}

/// Core v24.0.0: coinjoin/server.cpp:1165-1178 checks state before denomination (MNO-20).
#[test]
fn server_dsa_checks_readiness_then_state_before_denomination() {
    let mut env = Env::new();
    let ps = [Participant::standard(&mut env, 1), Participant::standard(&mut env, 2)];
    let mut s = accepting_session(&mut env, limits(2, 2), &ps, &[], V24);
    let counters = MixingCounters::default();
    let invalid = dsa(3, &ps[0].collateral, None);
    // Core v24.0.0: coinjoin/server.cpp:70-74 checks readiness before any admission rules.
    let fx = s.process_dsa(&mut env, &counters, &3, V24, &invalid);
    assert_eq!(statuses(&fx), vec![(3, REJECTED, PoolMessage::ErrQueueFull)]);
    for p in &ps {
        s.process_dsi(&mut env, &p.peer, p.entry());
    }
    assert_eq!(s.state(), PoolState::Signing);
    let fx = s.process_dsa(&mut env, &counters, &3, V24, &invalid);
    assert_eq!(statuses(&fx), vec![(3, REJECTED, PoolMessage::ErrMode)]);
    assert_eq!((s.state(), s.collateral_count(), s.entries().len()), (PoolState::Signing, 2, 2));
}

/// Core v24.0.0: coinjoin/server.cpp:1374-1382 skips disconnected peers (MN4-12).
#[test]
fn server_completion_reaches_all_connected_participants() {
    for accepted in [false, true] {
        for disconnected in [vec![1], vec![2], vec![1, 2, 3]] {
            let mut env = Env::new();
            env.mempool_accepts = accepted;
            let ps: Vec<_> = (1..=3).map(|p| Participant::standard(&mut env, p)).collect();
            let mut s = accepting_session(&mut env, limits(3, 3), &ps, &[], V24);
            for p in &ps {
                s.process_dsi(&mut env, &p.peer, p.entry());
            }
            let session_id = s.session_id();
            env.disconnected.extend(disconnected);
            // Core v24.0.0: coinjoin/server.cpp:507-510 completion precedes random fees.
            env.rand.extend([0, 99]);
            let final_tx = s.final_tx().clone();
            let mut fx = Vec::new();
            for p in &ps {
                fx = s.process_dss(&mut env, &p.peer, p.signed(&final_tx));
            }
            // Core v24.0.0: coinjoin/server.cpp:486 and :507 select the completion code.
            let message = if accepted {
                PoolMessage::MsgSuccess
            } else {
                PoolMessage::ErrInvalidTx
            };
            let completes: Vec<_> = fx
                .iter()
                .filter_map(|e| match e {
                    ServerEffect::Complete {
                        peer,
                        session_id,
                        message,
                    } => Some((*peer, *session_id, *message)),
                    _ => None,
                })
                .collect();
            let expected: Vec<_> = ps
                .iter()
                .filter(|p| !env.disconnected.contains(&p.peer))
                .map(|p| (p.peer, session_id, message))
                .collect();
            assert_eq!(completes, expected);
            assert!(statuses(&fx).is_empty());
            if accepted {
                assert!(
                    matches!(fx.last(), Some(ServerEffect::ConsumeCollateral(c)) if *c == ps[0].collateral)
                );
                assert_eq!(fx.len(), 3 + expected.len());
            } else {
                assert_eq!(fx.len(), expected.len());
            }
            assert_eq!((s.state(), s.session_id(), s.collateral_count()), (PoolState::Idle, 0, 0));
        }
    }
}

/// Core v24.0.0: coinjoin/server.cpp:700-707 preserves a queue that can advance (MN4-13).
#[test]
fn server_timeout_preserves_a_queue_that_became_ready_during_tick() {
    let mut env = Env::new();
    let ps = [Participant::standard(&mut env, 1), Participant::standard(&mut env, 2)];
    let mut s = ServerSession::new(limits(2, 3));
    let counters = MixingCounters::default();
    for p in &ps {
        s.process_dsa(&mut env, &counters, &p.peer, V24, &dsa(SMALLEST, &p.collateral, None));
    }
    let session_id = s.session_id();
    env.now = NOW + QUEUE_TIMEOUT;
    env.now_reads.borrow_mut().extend([env.now - 1, env.now]);
    // Core v24.0.0: coinjoin/server.cpp:734-741 rechecks readiness at the timeout decision.
    assert!(s.tick(&mut env).is_empty());
    assert_eq!(
        (s.state(), s.session_id(), s.collateral_count()),
        (PoolState::Queue, session_id, 2)
    );
    // Core v24.0.0: coinjoin/server.cpp:764-771 advances it on the next scheduler step.
    let fx = s.tick(&mut env);
    assert!(matches!(fx.as_slice(), [ServerEffect::RelayQueue(q)] if q.ready()));
    assert_eq!((s.state(), s.session_id()), (PoolState::AcceptingEntries, session_id));
}

/// Core v24.0.0: coinjoin/server.cpp:701-705 preserves entries that can finalize (MN4-13).
#[test]
fn server_timeout_preserves_entries_that_can_finalize() {
    let mut env = Env::new();
    let ps: Vec<_> = (1..=3).map(|p| Participant::standard(&mut env, p)).collect();
    let mut s = accepting_session(&mut env, limits(2, 3), &ps, &[], V24);
    for p in &ps[..2] {
        s.process_dsi(&mut env, &p.peer, p.entry());
    }
    let session_id = s.session_id();
    env.now += QUEUE_TIMEOUT;
    env.now_reads.borrow_mut().extend([env.now - 1, env.now]);
    assert!(s.tick(&mut env).is_empty());
    assert_eq!(
        (s.state(), s.session_id(), s.entries().len()),
        (PoolState::AcceptingEntries, session_id, 2)
    );
    // Core v24.0.0: coinjoin/server.cpp:367-371 finalizes the covered subset next tick.
    let fx = s.tick(&mut env);
    assert!(matches!(
        fx.as_slice(),
        [
            ServerEffect::FinalTx {
                peer: 1,
                ..
            },
            ServerEffect::FinalTx {
                peer: 2,
                ..
            }
        ]
    ));
    assert_eq!((s.state(), s.session_id()), (PoolState::Signing, session_id));
}

/// Core v24.0.0: coinjoin/server.cpp:444-449 consumes the selected collateral after relay.
#[test]
fn server_timeout_charge_survives_a_disconnected_final_relay() {
    let mut env = Env::new();
    let ps: Vec<_> = (1..=3).map(|p| Participant::standard(&mut env, p)).collect();
    let mut s = accepting_session(&mut env, limits(2, 3), &ps, &[], V24);
    for p in &ps[..2] {
        s.process_dsi(&mut env, &p.peer, p.entry());
    }
    env.disconnected.extend([1, 2]);
    env.now += QUEUE_TIMEOUT;
    env.rand.extend([0, 0]);
    let fx = s.tick(&mut env);
    // Core v24.0.0: coinjoin/server.cpp:1317-1320, :1361-1364 resets on a failed relay
    // with every participant disconnected; :447-449 still consumes the chosen collateral.
    assert!(
        matches!(fx.as_slice(), [ServerEffect::ConsumeCollateral(c)] if *c == ps[2].collateral)
    );
    assert_eq!((s.state(), s.session_id(), s.collateral_count()), (PoolState::Idle, 0, 0));
}

/// An entry is checked in Core's order: size, collateral, shape, duplicates, then the coins.
#[test]
fn server_entry_rejections() {
    let mut env = Env::new();
    let ps = vec![Participant::standard(&mut env, 1), Participant::standard(&mut env, 2)];
    let mut s = accepting_session(&mut env, limits(2, 2), &ps, &[], V24);
    let send = |s: &mut ServerSession<u8>, env: &mut Env, entry: DsEntry| {
        let fx = s.process_dsi(env, &1, entry);
        let consumed = fx.iter().any(|e| matches!(e, ServerEffect::ConsumeCollateral(_)));
        (statuses(&fx)[0].2, consumed)
    };

    // Over ENTRY_MAX_SIZE: charged.
    let mut big = ps[0].entry();
    big = dash_p2p_core::DsEntry::new(
        env.chain.smallest_coins(10),
        big.collateral().clone(),
        big.outputs().clone(),
    );
    big = dash_p2p_core::DsEntry::new(
        big.inputs().clone(),
        big.collateral().clone(),
        (0..10).map(|i| txout(SMALLEST_DENOMINATION, key(50 + i))).collect(),
    );
    assert_eq!(send(&mut s, &mut env, big), (PoolMessage::ErrMaximum, true));
    // An unknown shape: charged.
    let mut odd = ps[0].entry();
    {
        let mut outputs = odd.outputs().clone();
        outputs.push(txout(SMALLEST_DENOMINATION, key(60)));
        odd = dash_p2p_core::DsEntry::new(odd.inputs().clone(), odd.collateral().clone(), outputs);
    }
    assert_eq!(send(&mut s, &mut env, odd), (PoolMessage::ErrSizeMismatch, true));
    // A missing coin: free.
    let mut missing = ps[0].entry();
    {
        let mut inputs = missing.inputs().clone();
        inputs[0] = dash_primitives::TxIn::new(
            outpoint(u256(0x44), 0),
            inputs[0].script_sig().clone(),
            inputs[0].sequence(),
        );
        missing = dash_p2p_core::DsEntry::new(
            inputs,
            missing.collateral().clone(),
            missing.outputs().clone(),
        );
    }
    assert_eq!(send(&mut s, &mut env, missing), (PoolMessage::ErrMissingTx, false));
    // Another denomination: charged.
    let mut other = ps[0].entry();
    {
        let mut outputs = other.outputs().clone();
        outputs[0] = dash_primitives::TxOut::from_duffs(
            denom::denomination_to_amount(denom::larger_adjacent_denom(SMALLEST)),
            outputs[0].script_pubkey().clone(),
        );
        other = dash_p2p_core::DsEntry::new(
            other.inputs().clone(),
            other.collateral().clone(),
            outputs,
        );
    }
    assert_eq!(send(&mut s, &mut env, other), (PoolMessage::ErrDenom, true));
    assert!(s.entries().is_empty());

    // An input already in the pool: free (who is the culprit?).
    assert_eq!(send(&mut s, &mut env, ps[0].entry()), (PoolMessage::MsgEntriesAdded, false));
    let mut dup = ps[1].entry();
    dup = dash_p2p_core::DsEntry::new(
        ps[0].inputs.clone(),
        dup.collateral().clone(),
        dup.outputs().clone(),
    );
    let fx = s.process_dsi(&mut env, &2, dup);
    assert_eq!(statuses(&fx), vec![(2, REJECTED, PoolMessage::ErrAlreadyHave)]);
    assert!(!fx.iter().any(|e| matches!(e, ServerEffect::ConsumeCollateral(_))));
}

/// A rebalance session: one promotion among standard participants is finalized with the
/// promotion's output at the larger denomination.
#[test]
fn server_finalizes_a_rebalance_session() {
    let mut env = Env::new();
    env.v24 = true;
    let ps = vec![
        Participant::promotion(&mut env, 1),
        Participant::standard(&mut env, 2),
        Participant::standard(&mut env, 3),
    ];
    let mut s =
        accepting_session(&mut env, limits(3, 3), &ps, &[Some(FLAG_PROMOTION), None, None], V24);
    for p in &ps {
        s.process_dsi(&mut env, &p.peer, p.entry());
    }
    assert_eq!(s.state(), PoolState::Signing);
    let larger = denom::denomination_to_amount(denom::larger_adjacent_denom(SMALLEST));
    assert_eq!(s.final_tx().outputs().iter().filter(|o| o.value() == larger).count(), 1);
    assert_eq!((s.final_tx().inputs().len(), s.final_tx().outputs().len()), (12, 3));
}

/// `IsSessionReady` never opens a session whose declared sides leave a lone participant.
#[test]
fn server_session_with_a_lone_side_never_opens() {
    let mut env = Env::new();
    env.v24 = true;
    let ps = [Participant::promotion(&mut env, 1), Participant::standard(&mut env, 2)];
    let mut s = ServerSession::new(limits(2, 2));
    let counters = MixingCounters::default();
    s.process_dsa(
        &mut env,
        &counters,
        &1,
        V24,
        &dsa(SMALLEST, &ps[0].collateral, Some(FLAG_PROMOTION)),
    );
    s.process_dsa(&mut env, &counters, &2, V24, &dsa(SMALLEST, &ps[1].collateral, None));
    // Two of two, but one standard participant alone on the outputs side.
    assert!(!s.is_session_ready(env.now));
    env.now += QUEUE_TIMEOUT;
    assert!(!s.is_session_ready(env.now));
    // The timeout resets it.
    s.tick(&mut env);
    assert_eq!(s.state(), PoolState::Idle);
}

// ---------------------------------------------------------------------------------------------
// The client session
// ---------------------------------------------------------------------------------------------

fn update(
    session_id: i32,
    state: PoolState,
    status: PoolStatusUpdate,
    message: PoolMessage,
) -> DsStatusUpdate {
    DsStatusUpdate::new(session_id, state as i32, status as i32, message as i32)
}

/// `ProcessPoolStateUpdate` (client.cpp:414).
#[test]
fn client_status_updates() {
    let mut c = ClientSession::default();
    let accepted = update(7, PoolState::Queue, PoolStatusUpdate::Accepted, PoolMessage::MsgNoErr);
    assert_eq!(c.process_status_update(&accepted, NOW), StatusOutcome::Ignored);

    c.state = PoolState::Queue;
    assert_eq!(
        c.process_status_update(&accepted, NOW + 1),
        StatusOutcome::Accepted(PoolMessage::MsgNoErr)
    );
    assert_eq!((c.session_id, c.time_last_successful_step), (7, NOW + 1));
    // Set once.
    let again = update(8, PoolState::Queue, PoolStatusUpdate::Accepted, PoolMessage::MsgNoErr);
    c.process_status_update(&again, NOW + 2);
    assert_eq!(c.session_id, 7);

    // Out of range: ignored.
    let mut bad = accepted.clone();
    bad = dash_p2p_core::DsStatusUpdate::new(
        bad.session_id(),
        99,
        bad.status_update(),
        bad.message_id(),
    );
    assert_eq!(c.process_status_update(&bad, NOW), StatusOutcome::Ignored);
    let mut bad = accepted.clone();
    bad =
        dash_p2p_core::DsStatusUpdate::new(bad.session_id(), bad.state(), bad.status_update(), 99);
    assert_eq!(c.process_status_update(&bad, NOW), StatusOutcome::Ignored);
    let mut bad = accepted.clone();
    bad = dash_p2p_core::DsStatusUpdate::new(bad.session_id(), bad.state(), 2, bad.message_id());
    assert_eq!(c.process_status_update(&bad, NOW), StatusOutcome::Ignored);

    let rejected =
        update(7, PoolState::Queue, PoolStatusUpdate::Rejected, PoolMessage::ErrQueueFull);
    assert_eq!(
        c.process_status_update(&rejected, NOW + 3),
        StatusOutcome::Rejected(PoolMessage::ErrQueueFull)
    );
    assert_eq!(
        (c.state, c.last_message, c.time_last_successful_step),
        (PoolState::Error, Some(PoolMessage::ErrQueueFull), NOW + 3)
    );
    assert_eq!(c.process_status_update(&accepted, NOW), StatusOutcome::Ignored);
}

/// A client with one standard entry, and a valid final transaction of three.
fn client_and_final_tx(chain: &mut Chain) -> (ClientSession, Transaction) {
    let inputs = chain.smallest_coins(3);
    let outputs: Vec<_> = (1..=3).map(|i| txout(SMALLEST_DENOMINATION, key(i))).collect();
    let client = ClientSession {
        state: PoolState::AcceptingEntries,
        session_id: 5,
        session_denom: SMALLEST,
        entries: vec![ClientEntry {
            inputs: vec![inputs[0].clone()],
            outputs: vec![outputs[0].clone()],
        }],
        ..ClientSession::default()
    };
    let mut final_tx = tx(inputs, outputs);
    sort_bip69(&mut final_tx);
    (client, final_tx)
}

/// `SignFinalTransaction` (client.cpp:464): each refusal resets the session.
#[test]
fn client_final_tx_checks() {
    let mut chain = Chain::default();
    let (client, final_tx) = client_and_final_tx(&mut chain);
    let stranger_coin = chain.smallest_coins(1).remove(0);

    let mut c = client.clone();
    assert_eq!(
        c.process_final_tx(&chain, 6, final_tx.clone(), false, NOW),
        FinalTxOutcome::WrongSession
    );
    let FinalTxOutcome::Sign {
        inputs,
    } = c.process_final_tx(&chain, 5, final_tx.clone(), false, NOW)
    else {
        panic!()
    };
    assert_eq!(final_tx.inputs()[inputs[0]], client.entries[0].inputs[0]);
    assert_eq!(c.final_tx.as_ref(), Some(&final_tx));

    let refuse = |t: Transaction| {
        let mut c = client.clone();
        let FinalTxOutcome::Refused(why) = c.process_final_tx(&chain, 5, t, false, NOW) else {
            panic!("signed")
        };
        assert_eq!((c.state, c.session_id, c.entries.len()), (PoolState::Idle, 0, 0));
        why
    };
    let mut unsorted = final_tx.clone();
    {
        let mut inputs = unsorted.inputs().clone();
        inputs.reverse();
        unsorted = dash_primitives::Transaction::new(
            unsorted.version(),
            unsorted.tx_type(),
            inputs,
            unsorted.outputs().clone(),
            unsorted.lock_time(),
            unsorted.extra_payload().clone(),
        );
    }
    assert_eq!(refuse(unsorted), FinalTxRefusal::NotBip69);
    let mut extra = final_tx.clone();
    {
        let mut outputs = extra.outputs().clone();
        outputs[0] = dash_primitives::TxOut::from_duffs(
            denom::denomination_to_amount(1 << 3),
            outputs[0].script_pubkey().clone(),
        );
        extra = dash_primitives::Transaction::new(
            extra.version(),
            extra.tx_type(),
            extra.inputs().clone(),
            outputs,
            extra.lock_time(),
            extra.extra_payload().clone(),
        );
    }
    sort_bip69(&mut extra);
    assert_eq!(refuse(extra), FinalTxRefusal::InvalidInOuts(PoolMessage::ErrDenom));
    let ours = &client.entries[0];
    let mut no_output = final_tx.clone();
    let i = no_output.outputs().iter().position(|o| *o == ours.outputs[0]).unwrap();
    {
        let mut outputs = no_output.outputs().clone();
        outputs[i] = txout(SMALLEST_DENOMINATION, key(9));
        no_output = dash_primitives::Transaction::new(
            no_output.version(),
            no_output.tx_type(),
            no_output.inputs().clone(),
            outputs,
            no_output.lock_time(),
            no_output.extra_payload().clone(),
        );
    }
    sort_bip69(&mut no_output);
    assert_eq!(refuse(no_output), FinalTxRefusal::MissingOutput);
    let mut no_input = final_tx.clone();
    let i = no_input.inputs().iter().position(|x| *x == ours.inputs[0]).unwrap();
    {
        let mut inputs = no_input.inputs().clone();
        inputs[i] = stranger_coin;
        no_input = dash_primitives::Transaction::new(
            no_input.version(),
            no_input.tx_type(),
            inputs,
            no_input.outputs().clone(),
            no_input.lock_time(),
            no_input.extra_payload().clone(),
        );
    }
    sort_bip69(&mut no_input);
    assert_eq!(refuse(no_input), FinalTxRefusal::MissingInput);
}

/// Post-V24 the client refuses a final transaction where its rebalance coins have no cover.
#[test]
fn client_refuses_uncovered_rebalance() {
    let mut chain = Chain::default();
    let larger = denom::denomination_to_amount(denom::larger_adjacent_denom(SMALLEST));
    // Our promotion, and one other promotion: our ten inputs are covered by its ten.
    let ours = chain.smallest_coins(10);
    let theirs = chain.smallest_coins(10);
    let our_out = txout(larger, key(1));
    let mut client = ClientSession {
        state: PoolState::AcceptingEntries,
        session_id: 5,
        session_denom: SMALLEST,
        entries: vec![ClientEntry {
            inputs: ours.clone(),
            outputs: vec![our_out.clone()],
        }],
        ..ClientSession::default()
    };
    let mut covered = tx(
        ours.iter().chain(&theirs).cloned().collect(),
        vec![our_out.clone(), txout(larger, key(2))],
    );
    sort_bip69(&mut covered);
    let c = &mut client.clone();
    assert!(
        matches!(c.process_final_tx(&chain, 5, covered, true, NOW), FinalTxOutcome::Sign { inputs } if inputs.len() == 10)
    );

    // A masternode pads the other side with larger coins: one demotion. Value and composition
    // hold, but nobody else has session coins on our side.
    let big = vec![txin(chain.fund(larger))];
    let mut padded = tx(ours.iter().chain(&big).cloned().collect(), vec![our_out]);
    {
        let mut outputs = padded.outputs().clone();
        outputs.extend((10..20).map(|i| txout(SMALLEST_DENOMINATION, key(i))));
        padded = dash_primitives::Transaction::new(
            padded.version(),
            padded.tx_type(),
            padded.inputs().clone(),
            outputs,
            padded.lock_time(),
            padded.extra_payload().clone(),
        );
    }
    sort_bip69(&mut padded);
    assert_eq!(
        client.process_final_tx(&chain, 5, padded, true, NOW),
        FinalTxOutcome::Refused(FinalTxRefusal::NoCover)
    );
}

/// `DSCOMPLETE` and `CompletedTransaction` (client.cpp:124-141, :634).
#[test]
fn client_complete() {
    let mut chain = Chain::default();
    let (client, _) = client_and_final_tx(&mut chain);
    let mut c = client.clone();
    assert_eq!(
        c.process_complete(&DsComplete::new(4, PoolMessage::MsgSuccess as i32,), NOW),
        CompleteOutcome::Ignored
    );
    assert_eq!(c.process_complete(&DsComplete::new(5, 99,), NOW), CompleteOutcome::Ignored);
    let CompleteOutcome::Completed {
        message,
        mixed_inputs,
    } = c.process_complete(&DsComplete::new(5, PoolMessage::MsgSuccess as i32), NOW)
    else {
        panic!()
    };
    assert_eq!(message, PoolMessage::MsgSuccess);
    assert_eq!(mixed_inputs, BTreeSet::from([client.entries[0].inputs[0].prevout()]));
    assert_eq!(
        (c.state, c.session_id, c.last_message),
        (PoolState::Idle, 0, Some(PoolMessage::MsgSuccess))
    );

    let mut c = client.clone();
    let CompleteOutcome::Completed {
        mixed_inputs,
        ..
    } = c.process_complete(&DsComplete::new(5, PoolMessage::ErrInvalidTx as i32), NOW)
    else {
        panic!()
    };
    assert!(mixed_inputs.is_empty());
}

/// `JoinExistingQueue`/`StartNewQueue` (client.cpp:1384, :1473, :1557), `ProcessPendingDsaRequest`
/// (:1646) and `SendDenominate` (:354-408): each step restarts the session's clock.
#[test]
fn client_transitions_restart_the_clock() {
    let mut c = ClientSession {
        last_message: Some(PoolMessage::ErrSession),
        ..ClientSession::default()
    };
    c.enter_queue(SMALLEST, NOW);
    assert_eq!(
        (c.state, c.session_denom, c.time_last_successful_step),
        (PoolState::Queue, SMALLEST, NOW)
    );
    c.dsa_sent(NOW + 20);
    assert_eq!(c.check_timeout(NOW + 20 + QUEUE_TIMEOUT + 9), TimeoutOutcome::None);

    let entry = ClientEntry {
        inputs: vec![txin(outpoint(u256(9), 0))],
        outputs: vec![txout(SMALLEST_DENOMINATION, p2pkh(9))],
    };
    c.entry_sent(entry.clone(), NOW + 60);
    assert_eq!(c.state, PoolState::AcceptingEntries);
    assert_eq!(c.entries, vec![entry]);
    assert_eq!(c.last_message, None);
    assert_eq!(c.check_timeout(NOW + 60 + QUEUE_TIMEOUT + 9), TimeoutOutcome::None);
    assert_eq!(c.check_timeout(NOW + 60 + QUEUE_TIMEOUT + 10), TimeoutOutcome::TimedOut);
}

/// `CheckTimeout` (client.cpp:304): the masternode's timeouts plus ten seconds of lag; ten
/// seconds in `ERROR` before a reset.
#[test]
fn client_timeouts() {
    let mut c = ClientSession {
        state: PoolState::Queue,
        session_id: 5,
        time_last_successful_step: NOW,
        ..ClientSession::default()
    };
    assert_eq!(c.check_timeout(NOW + QUEUE_TIMEOUT + 9), TimeoutOutcome::None);
    assert_eq!(c.check_timeout(NOW + QUEUE_TIMEOUT + 10), TimeoutOutcome::TimedOut);
    assert_eq!((c.state, c.last_message), (PoolState::Error, Some(PoolMessage::ErrSession)));
    let t = c.time_last_successful_step;
    assert_eq!(c.check_timeout(t + 9), TimeoutOutcome::None);
    assert_eq!(c.check_timeout(t + 10), TimeoutOutcome::Reset);
    assert_eq!(c.state, PoolState::Idle);
    assert_eq!(c.check_timeout(t + 1_000), TimeoutOutcome::None);

    let mut c = ClientSession {
        state: PoolState::Signing,
        time_last_successful_step: NOW,
        ..ClientSession::default()
    };
    assert_eq!(c.check_timeout(NOW + SIGNING_TIMEOUT + 9), TimeoutOutcome::None);
    assert_eq!(c.check_timeout(NOW + SIGNING_TIMEOUT + 10), TimeoutOutcome::TimedOut);
}
