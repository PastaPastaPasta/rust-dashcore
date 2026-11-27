#![cfg(feature = "testing")]
//! CJ1-W6 (D17): `dsa`/`dsi`/`dss` admission through [`SharedServer`] awaits the mempool outside
//! the peer region and the server's lock, reading in Core's order (W6C).

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use dash_coinjoin::collateral::MempoolAccept;
use dash_coinjoin::denom::SMALLEST_DENOMINATION;
use dash_coinjoin::dstx::{PoolLimits, txid};
use dash_coinjoin::inouts::{CoinView, ViewCoin};
use dash_coinjoin::pool::{ENTRY_MAX_SIZE, PoolMessage, PoolState};
use dash_coinjoin::queue::{MixingCounters, MixingMasternode};
use dash_consensus::Block;
use dash_consensus::hash::hash160;
use dash_consensus::script::push_data;
use dash_consensus::script::sighash::signature_hash;
use dash_p2p::conn::Permissions;
use dash_p2p::{NetCore, PeerHandle, Service};
use dash_p2p_core::{DsAccept, DsComplete, DsEntry, DsSignedInputs, DsStatusUpdate};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{BlockHash, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;
use dashd_coinjoin::dstx::DstxRuntime;
use dashd_coinjoin::env::{Answer, AsyncMempool, ChainView, Mempool, Pending};
use dashd_coinjoin::server::{CoinJoinServer, ServerClock, SharedServer};
use dashd_coinjoin::testing::{FakeActiveMn, FakeMnLists, FakePeerSink, FakeRandom, PeerCall};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::{
    BlockOutcome, BlockSubmit, BlockWait, ChainAccess, Interrupted,
};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimChain, SimPeer};
use dashd_p2p::{Registry, Seams};
use secp256k1::{Message, PublicKey, SecretKey};

const DSA: &str = "dsa";
const DSI: &str = "dsi";
const DSS: &str = "dss";
const COIN: i64 = 100_000_000;
const MAX_RAW_TX_FEE: i64 = COIN / 10;
/// The fake pool's relay floor on the modified fee.
const MIN_FEE: i64 = 1_000;
const COLLATERAL_FEE: i64 = 10_000;
const PATIENCE: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct HostChain;

impl ChainView for HostChain {
    fn tip(&self) -> Option<(i32, BlockHash)> {
        None
    }
    fn v24_active_at_tip(&self) -> bool {
        true
    }
    fn v24_active_after_tip(&self) -> bool {
        true
    }
    fn has_chain_lock(&self, _: i32, _: &BlockHash) -> bool {
        false
    }
    fn blockchain_synced(&self) -> bool {
        true
    }
}

#[derive(Clone, Default)]
struct Clock(Arc<AtomicI64>);

impl ServerClock for Clock {
    fn now(&self) -> i64 {
        SIM_START + self.0.load(Ordering::SeqCst)
    }
    fn adjusted_time(&self) -> i64 {
        self.now()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    Coins(Vec<OutPoint>),
    Locks(Vec<TxHash>),
    TestAccept(TxHash),
    Submit(TxHash),
    Prioritise(TxHash, i64),
    /// A blocking call, which the shared server never makes.
    Blocking(&'static str),
}

/// The fake pool: a transaction is accepted when it is not refused and its base fee plus its
/// priority delta reaches [`MIN_FEE`].
#[derive(Default)]
struct Book {
    coins: BTreeMap<OutPoint, ViewCoin>,
    fees: BTreeMap<TxHash, i64>,
    refused: BTreeSet<TxHash>,
    failing_submits: BTreeSet<TxHash>,
    deltas: BTreeMap<TxHash, i64>,
    /// The InstantSend-locked transactions.
    locked: BTreeSet<TxHash>,
    pooled: Vec<TxHash>,
    calls: Vec<Call>,
    /// The next test-accept tells the first and waits for its answer from the second.
    gate: Option<(mpsc::Sender<()>, mpsc::Receiver<Option<i64>>)>,
    /// The next submit is taken, tells the first and answers once the second releases it.
    submit_gate: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
}

impl Book {
    fn accept(&self, id: &TxHash) -> Option<i64> {
        if self.refused.contains(id) {
            return None;
        }
        let fee = self.fees.get(id).copied().unwrap_or(0);
        (fee + self.deltas.get(id).copied().unwrap_or(0) >= MIN_FEE).then_some(fee)
    }

    fn submit(&mut self, id: TxHash) -> bool {
        let ok = !self.failing_submits.contains(&id) && self.accept(&id).is_some();
        if ok {
            self.pooled.push(id);
        }
        ok
    }
}

#[derive(Clone, Default)]
struct Pool(Arc<Mutex<Book>>);

impl Pool {
    fn book(&self) -> MutexGuard<'_, Book> {
        self.0.lock().unwrap()
    }

    fn calls(&self) -> Vec<Call> {
        self.book().calls.clone()
    }
}

impl MempoolAccept for Pool {
    fn test_accept(&self, tx: &Transaction) -> Option<i64> {
        let mut book = self.book();
        book.calls.push(Call::Blocking("test_accept"));
        book.accept(&txid(tx))
    }
}

impl CoinView for Pool {
    fn coin(&self, outpoint: &OutPoint) -> Option<ViewCoin> {
        let mut book = self.book();
        book.calls.push(Call::Blocking("coin"));
        book.coins.get(outpoint).cloned()
    }
    fn is_instantsend_locked(&self, _: &TxHash) -> bool {
        false
    }
}

impl Mempool for Pool {
    fn submit(&mut self, tx: &Transaction) -> bool {
        let mut book = self.book();
        book.calls.push(Call::Blocking("submit"));
        book.submit(txid(tx))
    }
    fn prioritise(&mut self, txid: &TxHash, fee_delta: i64) {
        let mut book = self.book();
        book.calls.push(Call::Blocking("prioritise"));
        *book.deltas.entry(*txid).or_default() += fee_delta;
    }
    fn awaitable(&self) -> Option<Arc<dyn AsyncMempool>> {
        Some(Arc::new(self.clone()))
    }
}

fn now<T: Send + 'static>(value: T) -> Pending<T> {
    Box::pin(std::future::ready(Some(value)))
}

struct Slot<T> {
    value: Option<T>,
    waker: Option<Waker>,
}

/// An answer another thread sets.
struct Later<T>(Arc<Mutex<Slot<T>>>);

/// `f` of what `answer` receives, once it does.
fn later<A: Send + 'static, T: Send + 'static>(
    answer: mpsc::Receiver<A>,
    f: impl FnOnce(A) -> T + Send + 'static,
) -> Pending<T> {
    let slot = Arc::new(Mutex::new(Slot {
        value: None,
        waker: None,
    }));
    let setter = Arc::clone(&slot);
    std::thread::spawn(move || {
        let Ok(value) = answer.recv() else {
            return;
        };
        let mut slot = setter.lock().unwrap();
        slot.value = Some(f(value));
        if let Some(waker) = slot.waker.take() {
            waker.wake();
        }
    });
    Box::pin(Later(slot))
}

impl<T> Future for Later<T> {
    type Output = Option<T>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut slot = self.0.lock().unwrap();
        match slot.value.take() {
            Some(value) => Poll::Ready(Some(value)),
            None => {
                slot.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl AsyncMempool for Pool {
    fn coins(&self, outpoints: Vec<OutPoint>) -> Pending<Vec<Option<ViewCoin>>> {
        let mut book = self.book();
        book.calls.push(Call::Coins(outpoints.clone()));
        now(outpoints.iter().map(|o| book.coins.get(o).cloned()).collect())
    }

    fn instantsend_locked(&self, txids: Vec<TxHash>) -> Pending<Vec<bool>> {
        let mut book = self.book();
        book.calls.push(Call::Locks(txids.clone()));
        now(txids.iter().map(|t| book.locked.contains(t)).collect())
    }

    fn test_accept(&self, tx: Transaction) -> Pending<Answer<Option<i64>>> {
        let mut book = self.book();
        let id = txid(&tx);
        book.calls.push(Call::TestAccept(id));
        let Some((asked, answer)) = book.gate.take() else {
            return now(Ok(book.accept(&id)));
        };
        asked.send(()).unwrap();
        later(answer, Ok)
    }

    fn submit(&self, tx: Transaction) -> Pending<Answer<bool>> {
        let mut book = self.book();
        let id = txid(&tx);
        book.calls.push(Call::Submit(id));
        let taken = book.submit(id);
        let Some((asked, release)) = book.submit_gate.take() else {
            return now(Ok(taken));
        };
        asked.send(()).unwrap();
        later(release, move |()| Ok(taken))
    }

    fn prioritise(&self, txid: TxHash, fee_delta: i64) -> Pending<()> {
        let mut book = self.book();
        book.calls.push(Call::Prioritise(txid, fee_delta));
        *book.deltas.entry(txid).or_default() += fee_delta;
        now(())
    }
}

type Server =
    CoinJoinServer<HostChain, FakeMnLists, FakeActiveMn, Pool, FakePeerSink, Clock, FakeRandom>;

fn script(tag: u8) -> Vec<u8> {
    let mut s = vec![0x76, 0xa9, 0x14];
    s.extend([tag; 20]);
    s.extend([0x88, 0xac]);
    s
}

fn input_secret(tag: u8) -> SecretKey {
    SecretKey::from_secret_bytes([tag; 32]).unwrap()
}

fn input_script(tag: u8) -> Vec<u8> {
    let key = input_secret(tag);
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend(hash160(&PublicKey::from_secret_key(&key).serialize()));
    script.extend([0x88, 0xac]);
    script
}

fn entry_prevout(tag: u8) -> OutPoint {
    OutPoint::new(TxHash::from([50 + tag; 32]), 0)
}

fn signed_input(tx: &Transaction, tag: u8) -> TxIn {
    let prevout = entry_prevout(tag);
    let index = tx.inputs().iter().position(|i| i.prevout() == prevout).unwrap();
    let hash = signature_hash(&input_script(tag), tx, index, 0x81);
    let mut signature = secp256k1::ecdsa::sign(Message::from_digest(hash), &input_secret(tag))
        .serialize_der()
        .to_vec();
    signature.push(0x81);
    let mut script_sig = Vec::new();
    push_data(&mut script_sig, &signature);
    push_data(&mut script_sig, &PublicKey::from_secret_key(&input_secret(tag)).serialize());
    TxIn::new(prevout, script_sig.into(), tx.inputs()[index].sequence())
}

fn collateral(tag: u8) -> (Transaction, OutPoint) {
    let tx = collateral_of(tag, 1);
    let prevout = tx.inputs()[0].prevout();
    (tx, prevout)
}

/// A collateral spending `inputs` coins of `tag`'s funding transaction.
fn collateral_of(tag: u8, inputs: u32) -> Transaction {
    Transaction::new(
        2,
        TxType::Spend,
        (0..inputs)
            .map(|n| {
                TxIn::new(OutPoint::new(TxHash::from([tag; 32]), n), Vec::new().into(), u32::MAX)
            })
            .collect(),
        vec![TxOut::from_duffs(10_000, script(tag).into())],
        0,
        Vec::new(),
    )
}

fn entry(tag: u8, outputs: usize) -> DsEntry {
    DsEntry::new(
        vec![TxIn::new(entry_prevout(tag), Vec::new().into(), u32::MAX)],
        collateral(tag).0,
        (0..outputs)
            .map(|n| TxOut::from_duffs(SMALLEST_DENOMINATION, script(tag + 10 + n as u8).into()))
            .collect(),
    )
}

fn server(pool: Pool, clock: Clock) -> Server {
    let active = FakeActiveMn::default();
    let mut mns = FakeMnLists::default();
    let mn = MixingMasternode {
        pro_tx_hash: TxHash::from([9; 32]),
        collateral: active.outpoint,
        operator_key: dash_dml::testkit::numbered(7).cold.pub_key_operator.get(),
    };
    mns.by_collateral.insert(active.outpoint, mn.clone());
    mns.valid.insert(TxHash::from([9; 32]), mn);
    mns.enabled = 10;
    let limits = PoolLimits {
        min_participants: 2,
        max_participants: 2,
    };
    CoinJoinServer::new(
        limits,
        HostChain,
        mns,
        active,
        pool,
        FakePeerSink::default(),
        clock,
        FakeRandom::default(),
        MixingCounters::default(),
        Arc::new(DstxRuntime::new(limits)),
    )
}

fn encode(value: &impl BaseCodec) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

struct Rig {
    sim: Sim,
    shared: SharedServer<Server>,
    pool: Pool,
    clock: Clock,
    peers: Vec<SimPeer>,
}

/// A masternode's shared server on the sim with two peers; each tag's collateral and entry coin
/// are in the pool's view, and each collateral pays [`COLLATERAL_FEE`].
fn rig() -> Rig {
    let pool = Pool::default();
    for tag in 1..=2 {
        let (tx, prevout) = collateral(tag);
        let mut book = pool.book();
        book.coins.insert(
            prevout,
            ViewCoin {
                out: TxOut::from_duffs(20_000, script(tag).into()),
                in_mempool: false,
            },
        );
        book.coins.insert(
            entry_prevout(tag),
            ViewCoin {
                out: TxOut::from_duffs(SMALLEST_DENOMINATION, input_script(tag).into()),
                in_mempool: false,
            },
        );
        book.fees.insert(txid(&tx), COLLATERAL_FEE);
    }
    let clock = Clock::default();
    let shared = SharedServer::new(server(pool.clone(), clock.clone()));
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let mut identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    identity.masternode = true;
    let mut registry = Registry::new();
    registry.register(Box::new(shared.clone()));
    let mut sim = Sim::with(
        params,
        identity,
        ProcessorOptions::default(),
        registry,
        Seams::default(),
        |_| {},
    );
    sim.chain.set_ibd(false);
    let peers = (1..=2)
        .map(|n| {
            let p = sim
                .accept_from(n, dash_p2p::NetAddr::ipv4([10, 0, 0, n]), Permissions::NONE)
                .unwrap();
            sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
            sim.pass();
            sim.sent(&p);
            shared.lock().peers_mut().connected.insert(p.handle.addr().service.clone());
            p
        })
        .collect();
    Rig {
        sim,
        shared,
        pool,
        clock,
        peers,
    }
}

impl Rig {
    fn service(&self, n: usize) -> Service {
        self.peers[n].handle.addr().service.clone()
    }

    fn send(&mut self, n: usize, cmd: &str, payload: Vec<u8>) {
        self.sim.admitted(&self.peers[n], cmd, payload).unwrap();
    }

    fn dsa(&mut self, n: usize) {
        let tag = n as u8 + 1;
        self.dsa_with(n, collateral(tag).0);
    }

    fn dsa_with(&mut self, n: usize, collateral: Transaction) {
        self.send(n, DSA, encode(&DsAccept::new(16, collateral, Some(0))));
    }

    /// A collateral with `inputs` usable coins, paying [`COLLATERAL_FEE`].
    fn fund_collateral(&self, tag: u8, inputs: u32) -> Transaction {
        let tx = collateral_of(tag, inputs);
        let mut book = self.pool.book();
        for input in tx.inputs() {
            let out = TxOut::from_duffs(20_000, script(tag).into());
            book.coins.insert(
                input.prevout(),
                ViewCoin {
                    out,
                    in_mempool: false,
                },
            );
        }
        book.fees.insert(txid(&tx), COLLATERAL_FEE);
        tx
    }

    /// An entry input of `tag`, its coin in the pool's view.
    fn fund_input(&self, tag: u8) -> TxIn {
        let out = TxOut::from_duffs(SMALLEST_DENOMINATION, input_script(tag).into());
        self.pool.book().coins.insert(
            entry_prevout(tag),
            ViewCoin {
                out,
                in_mempool: false,
            },
        );
        TxIn::new(entry_prevout(tag), Vec::new().into(), u32::MAX)
    }

    /// Peer `n`'s `dsc` messages.
    fn completions(&self, n: usize) -> Vec<i32> {
        let decode = |bytes: Vec<u8>| DsComplete::decode(&mut &bytes[..]).unwrap().message();
        self.pushed(n, "dsc").into_iter().map(decode).collect()
    }

    /// How often `MSG_DSTX` for `id` was relayed.
    fn dstx_invs(&self, id: TxHash) -> usize {
        let server = self.shared.lock();
        let calls = server.peers().calls.iter();
        calls
            .filter(|c| matches!(c, PeerCall::RelayTxInv(inv) if inv.inv_hash() == id.into()))
            .count()
    }

    /// The messages pushed to peer `n` with `command`.
    fn pushed(&self, n: usize, command: &str) -> Vec<Vec<u8>> {
        let to = self.service(n);
        let server = self.shared.lock();
        let calls = server.peers().calls.iter();
        calls
            .filter_map(|call| match call {
                PeerCall::PushToAddr(peer, msg)
                    if *peer == to && msg.command.as_str() == command =>
                {
                    Some(msg.payload.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// Peer `n`'s statuses as (`status_update`, `message_id`).
    fn statuses(&self, n: usize) -> Vec<(i32, i32)> {
        let decode = |bytes: Vec<u8>| DsStatusUpdate::decode(&mut &bytes[..]).unwrap();
        let pushed = self.pushed(n, "dssu").into_iter().map(decode);
        pushed.map(|s| (s.status_update(), s.message_id())).collect()
    }

    fn relayed(&self) -> Vec<TxHash> {
        let server = self.shared.lock();
        let calls = server.peers().calls.iter();
        calls
            .filter_map(|call| match call {
                PeerCall::RelayTransaction(id) => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// Both peers' collaterals accepted, the queue completed and both entries added; returns
    /// each peer's signed input and the signed final transaction.
    fn signing(&mut self) -> (Vec<TxIn>, Transaction) {
        self.dsa(0);
        self.dsa(1);
        self.shared.tick(false);
        assert_eq!(self.shared.info().state, PoolState::AcceptingEntries);
        for n in 0..2 {
            self.send(n, DSI, encode(&entry(n as u8 + 1, 1)));
        }
        assert_eq!(self.shared.info().state, PoolState::Signing);
        let unsigned = self.shared.lock().session().final_tx().clone();
        let signed: Vec<TxIn> = (1..=2).map(|tag| signed_input(&unsigned, tag)).collect();
        let inputs = unsigned
            .inputs()
            .iter()
            .map(|i| signed.iter().find(|s| s.prevout() == i.prevout()).unwrap().clone())
            .collect();
        let final_tx = Transaction::new(
            unsigned.version(),
            unsigned.tx_type(),
            inputs,
            unsigned.outputs().clone(),
            unsigned.lock_time(),
            unsigned.extra_payload().clone(),
        );
        (signed, final_tx)
    }

    fn sign(&mut self, signed: Vec<TxIn>) {
        for (n, input) in signed.into_iter().enumerate() {
            self.send(n, DSS, encode(&DsSignedInputs::new(vec![input])));
        }
    }
}

const ACCEPTED: i32 = 1;
const REJECTED: i32 = 0;

fn assert_no_blocking_call(pool: &Pool) {
    let calls = pool.calls();
    assert!(!calls.iter().any(|c| matches!(c, Call::Blocking(_))), "{calls:?}");
}

#[test]
fn dsa_collateral_is_test_accepted_with_the_sane_fee_cap() {
    for (fee, expect) in [
        (Some(COLLATERAL_FEE), (ACCEPTED, PoolMessage::MsgNoErr)),
        (None, (REJECTED, PoolMessage::ErrInvalidCollateral)),
        (Some(MAX_RAW_TX_FEE + 1), (REJECTED, PoolMessage::ErrInvalidCollateral)),
    ] {
        let mut r = rig();
        let (tx, prevout) = collateral(1);
        let id = txid(&tx);
        match fee {
            Some(fee) => r.pool.book().fees.insert(id, fee).map(drop),
            None => r.pool.book().refused.insert(id).then_some(()),
        };
        r.dsa(0);
        assert_eq!(r.statuses(0), vec![(expect.0, expect.1 as i32)], "{fee:?}");
        assert_eq!(r.pool.calls(), vec![Call::Coins(vec![prevout]), Call::TestAccept(id)]);
        assert!(r.pool.book().pooled.is_empty());
        let state = if expect.0 == ACCEPTED {
            PoolState::Queue
        } else {
            PoolState::Idle
        };
        assert_eq!(r.shared.info().state, state);
    }
}

#[test]
fn final_mix_is_committed_only_after_its_priority_bump() {
    let mut r = rig();
    let (signed, final_tx) = r.signing();
    let id = txid(&final_tx);
    // The mix pays no fee: the pool refuses it below the relay floor.
    assert_eq!(r.pool.book().accept(&id), None);
    r.pool.book().calls.clear();
    r.sign(signed);
    let calls = r.pool.calls();
    assert_eq!(
        calls[..3],
        [Call::Prioritise(id, COIN / 10), Call::TestAccept(id), Call::Submit(id)]
    );
    assert_eq!(r.pool.book().pooled.first(), Some(&id));
    let of_mix = |c: &&Call| match c {
        Call::Prioritise(t, _) | Call::TestAccept(t) | Call::Submit(t) => *t == id,
        _ => false,
    };
    assert_eq!(calls.iter().filter(of_mix).count(), 3, "{calls:?}");
    assert!(r.shared.lock().dstx().contains(&id));
    assert_eq!(r.dstx_invs(id), 1);
    for n in 0..2 {
        assert_eq!(r.completions(n), vec![PoolMessage::MsgSuccess as i32]);
    }
    assert_eq!(r.shared.info().state, PoolState::Idle);
    r.shared.tick(false);
    assert_eq!(r.pool.calls().iter().filter(of_mix).count(), 3);
    assert_eq!(r.dstx_invs(id), 1);
    assert_no_blocking_call(&r.pool);
}

#[test]
fn final_mix_over_the_fee_cap_is_not_submitted() {
    let mut r = rig();
    let (signed, final_tx) = r.signing();
    let id = txid(&final_tx);
    r.pool.book().fees.insert(id, MAX_RAW_TX_FEE + 1);
    r.pool.book().calls.clear();
    r.sign(signed);
    let calls = r.pool.calls();
    assert_eq!(calls[..2], [Call::Prioritise(id, COIN / 10), Call::TestAccept(id)]);
    assert!(!calls.contains(&Call::Submit(id)), "{calls:?}");
    assert!(!r.pool.book().pooled.contains(&id));
    for n in 0..2 {
        let complete = r.pushed(n, "dsc").pop().expect("dsc");
        let complete = DsComplete::decode(&mut &complete[..]).unwrap();
        assert_eq!(complete.message(), PoolMessage::ErrInvalidTx as i32);
    }
}

#[test]
fn consumed_dsi_collateral_is_relayed_only_on_a_successful_submit() {
    for submit in [true, false] {
        let mut r = rig();
        r.dsa(0);
        r.dsa(1);
        r.shared.tick(false);
        let (tx, _) = collateral(1);
        let id = txid(&tx);
        if !submit {
            r.pool.book().failing_submits.insert(id);
        }
        r.pool.book().calls.clear();
        // One input and two outputs is no mix shape: the collateral is charged.
        r.send(0, DSI, encode(&entry(1, 2)));
        assert_eq!(r.statuses(0).pop(), Some((REJECTED, PoolMessage::ErrSizeMismatch as i32)));
        let calls = r.pool.calls();
        assert_eq!(&calls[calls.len() - 2..], [Call::TestAccept(id), Call::Submit(id)]);
        assert_eq!(
            r.relayed(),
            if submit {
                vec![id]
            } else {
                vec![]
            },
            "submit={submit}"
        );
        assert_no_blocking_call(&r.pool);
    }
}

#[test]
fn tick_consumes_a_timed_out_collateral_only_through_a_sane_fee_submit() {
    for (fee, submit, relay) in [
        (Some(COLLATERAL_FEE), true, true),
        (Some(COLLATERAL_FEE), false, false),
        (Some(MAX_RAW_TX_FEE + 1), true, false),
        (None, true, false),
    ] {
        let mut r = rig();
        r.dsa(0);
        r.dsa(1);
        r.shared.tick(false);
        r.send(1, DSI, encode(&entry(2, 1)));
        let (tx, _) = collateral(1);
        let id = txid(&tx);
        {
            let mut book = r.pool.book();
            match fee {
                Some(fee) => drop(book.fees.insert(id, fee)),
                None => drop(book.refused.insert(id)),
            }
            if !submit {
                book.failing_submits.insert(id);
            }
            book.calls.clear();
        }
        r.clock.0.store(31, Ordering::SeqCst);
        r.shared.tick(false);
        assert_eq!(r.shared.info().state, PoolState::Idle);
        let sane = fee.is_some_and(|fee| fee <= MAX_RAW_TX_FEE);
        let mut expect = vec![Call::TestAccept(id)];
        if sane {
            expect.push(Call::Submit(id));
        }
        assert_eq!(r.pool.calls(), expect, "{fee:?} {submit}");
        assert_eq!(
            r.relayed(),
            if relay {
                vec![id]
            } else {
                vec![]
            },
            "{fee:?} {submit}"
        );
        assert_no_blocking_call(&r.pool);
    }
}

fn submit(block: &Arc<Block>) -> BlockSubmit {
    BlockSubmit {
        block: Arc::clone(block),
        hash: block.hash(),
        force: false,
        from: -1,
        merkle_checked: false,
    }
}

/// Polls `w` until it is ready or `patience` passes.
fn ready_within(
    w: &mut BlockWait,
    patience: Duration,
) -> Option<Result<BlockOutcome, Interrupted>> {
    let deadline = Instant::now() + patience;
    loop {
        if let Poll::Ready(r) = w.0.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            return Some(r);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Runs `f` on its own thread; `None` when it has not answered within `patience`.
fn within<T: Send + 'static>(
    patience: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || drop(tx.send(f())));
    rx.recv_timeout(patience).ok()
}

/// While an admission waits on its test-accept, V's block still completes (msghand drains its
/// signals) and the server's lock and the other peer's state are free. Other peers' messages
/// wait for the admission, as Core's msghand waits on its inline ATMP.
#[test]
fn a_pending_admission_lets_v_progress_and_frees_the_server_lock() {
    let mut r = rig();
    let (asked, asked_rx) = mpsc::channel();
    let (answer, answer_rx) = mpsc::channel();
    r.pool.book().gate = Some((asked, answer_rx));
    r.sim.chain.spawn_v();
    let (shared, chain, server) =
        (Arc::clone(&r.sim.shared), Arc::clone(&r.sim.chain), r.shared.clone());
    let other = r.peers[1].handle.clone();
    let (done_tx, done) = mpsc::channel();
    let msghand = std::thread::spawn(move || {
        r.dsa(0);
        done_tx.send(()).unwrap();
        r
    });
    asked_rx.recv_timeout(PATIENCE).expect("the collateral's test-accept was asked for");
    // V delivers a block: it waits until msghand has drained its `BlockChecked`.
    shared.block_checked(BlockHash::from_lendian([9; 32]), None, false);
    chain.republish();
    let block = &SimChain::build_blocks(&chain.genesis(), 1, 150, 3)[0];
    let mut wait = chain.process_block(submit(block)).unwrap();
    let delivered = ready_within(&mut wait, Duration::from_secs(10));
    let stats = within(Duration::from_secs(10), move || shared.node_state_stats(&other).is_some());
    let info = within(Duration::from_secs(10), move || server.info().state);
    let still_pending = done.try_recv().is_err();
    answer.send(Some(COLLATERAL_FEE)).unwrap();
    assert!(matches!(delivered, Some(Ok(o)) if o.new_block), "V stalled on msghand");
    assert_eq!(stats, Some(true), "the other peer's state was held");
    assert_eq!(info, Some(PoolState::Idle), "the server's lock was held");
    assert!(still_pending, "the admission finished before its answer");
    done.recv_timeout(PATIENCE).expect("the admission completed");
    let mut r = msghand.join().unwrap();
    assert_eq!(r.statuses(0), vec![(ACCEPTED, PoolMessage::MsgNoErr as i32)]);
    r.dsa(1);
    assert_eq!(r.statuses(1), vec![(ACCEPTED, PoolMessage::MsgNoErr as i32)]);
    assert_eq!(r.shared.info().state, PoolState::Queue);
}

/// Peer `n`'s last statuses are its entry's acceptance, then the session's relayed status.
fn assert_entry_added(r: &Rig, n: usize) {
    let added =
        [(ACCEPTED, PoolMessage::MsgEntriesAdded as i32), (ACCEPTED, PoolMessage::MsgNoErr as i32)];
    let statuses = r.statuses(n);
    assert!(statuses.ends_with(&added), "{statuses:?}");
}

#[test]
fn a_two_input_dsi_is_admitted() {
    let mut r = rig();
    let second = r.fund_input(3);
    r.dsa(0);
    r.dsa(1);
    r.shared.tick(false);
    let two = DsEntry::new(
        vec![TxIn::new(entry_prevout(1), Vec::new().into(), u32::MAX), second],
        collateral(1).0,
        entry(1, 2).outputs().clone(),
    );
    r.send(0, DSI, encode(&two));
    assert_entry_added(&r, 0);
}

/// Each coin read, one outpoint at a time.
fn coins(outpoints: impl IntoIterator<Item = OutPoint>) -> Vec<Call> {
    outpoints.into_iter().map(|o| Call::Coins(vec![o])).collect()
}

/// The collateral's coins in input order, its test-accept once they and the fee pass, then each
/// entry input's coin: Core's order (coinjoin.cpp:480-500, then :377-393).
#[test]
fn a_full_entry_and_a_two_input_collateral_are_read_in_cores_order() {
    let mut r = rig();
    let collateral = r.fund_collateral(30, 2);
    let inputs: Vec<TxIn> = (10..10 + ENTRY_MAX_SIZE as u8).map(|tag| r.fund_input(tag)).collect();
    let outputs = (0..ENTRY_MAX_SIZE as u8)
        .map(|n| TxOut::from_duffs(SMALLEST_DENOMINATION, script(100 + n).into()))
        .collect();
    r.dsa_with(0, collateral.clone());
    r.dsa(1);
    r.shared.tick(false);
    assert_eq!(r.shared.info().state, PoolState::AcceptingEntries);
    r.pool.book().calls.clear();
    let full = DsEntry::new(inputs.clone(), collateral.clone(), outputs);
    r.send(0, DSI, encode(&full));
    assert_entry_added(&r, 0);
    let mut read = coins(collateral.inputs().iter().map(TxIn::prevout));
    read.push(Call::TestAccept(txid(&collateral)));
    read.extend(coins(inputs.iter().map(TxIn::prevout)));
    assert_eq!(r.pool.calls(), read);
    r.send(1, DSI, encode(&entry(2, 1)));
    assert_eq!(r.shared.info().state, PoolState::Signing);
    assert_eq!(r.shared.lock().session().final_tx().inputs().len(), ENTRY_MAX_SIZE + 1);
}

#[test]
fn a_dsa_with_a_three_input_collateral_is_accepted() {
    let mut r = rig();
    let collateral = r.fund_collateral(30, 3);
    r.dsa_with(0, collateral.clone());
    assert_eq!(r.statuses(0), vec![(ACCEPTED, PoolMessage::MsgNoErr as i32)]);
    let mut read = coins(collateral.inputs().iter().map(TxIn::prevout));
    read.push(Call::TestAccept(txid(&collateral)));
    assert_eq!(r.pool.calls(), read);
    assert_eq!(r.shared.info().state, PoolState::Queue);
}

/// A `dsi` whose collateral is not one of the session's is refused before anything is read, as
/// Core's AddEntry refuses it under `cs_coinjoin` (server.cpp:867-875).
#[test]
fn a_dsi_from_a_non_participant_reads_no_coin() {
    let mut r = rig();
    r.dsa(0);
    r.dsa(1);
    r.shared.tick(false);
    assert_eq!(r.shared.info().state, PoolState::AcceptingEntries);
    let stranger = r.fund_collateral(40, 1);
    let input = r.fund_input(40);
    r.pool.book().calls.clear();
    let entry = DsEntry::new(vec![input], stranger, entry(1, 1).outputs().clone());
    r.send(0, DSI, encode(&entry));
    assert_eq!(r.statuses(0).last(), Some(&(REJECTED, PoolMessage::ErrSession as i32)));
    assert_eq!(r.pool.calls(), vec![]);
    assert_eq!(r.shared.lock().session().entries().len(), 0);
}

/// Core's collateral loop returns at the first missing coin (coinjoin.cpp:480-486): one read, and
/// no test-accept.
#[test]
fn a_collateral_whose_first_input_is_missing_reads_one_coin() {
    let mut r = rig();
    let collateral = r.fund_collateral(30, 2);
    let first = collateral.inputs()[0].prevout();
    r.pool.book().coins.remove(&first);
    r.dsa_with(0, collateral);
    assert_eq!(r.statuses(0), vec![(REJECTED, PoolMessage::ErrInvalidCollateral as i32)]);
    assert_eq!(r.pool.calls(), coins([first]));
    assert_eq!(r.shared.info().state, PoolState::Idle);
}

/// A collateral spending `tag`'s single mempool coin, `locked` or not.
fn pooled_collateral(r: &Rig, tag: u8, locked: bool) -> Transaction {
    let collateral = r.fund_collateral(tag, 1);
    let prevout = collateral.inputs()[0].prevout();
    let mut book = r.pool.book();
    book.coins.get_mut(&prevout).unwrap().in_mempool = true;
    if locked {
        book.locked.insert(prevout.txid());
    }
    collateral
}

/// A collateral spending an InstantSend-locked mempool coin is valid; the lock is read with the
/// coin, before the test-accept (coinjoin.cpp:482-483).
#[test]
fn a_dsa_whose_collateral_spends_a_locked_pool_coin_is_accepted() {
    let mut r = rig();
    let collateral = pooled_collateral(&r, 30, true);
    let prevout = collateral.inputs()[0].prevout();
    r.dsa_with(0, collateral.clone());
    assert_eq!(r.statuses(0), vec![(ACCEPTED, PoolMessage::MsgNoErr as i32)]);
    let read = vec![
        Call::Coins(vec![prevout]),
        Call::Locks(vec![prevout.txid()]),
        Call::TestAccept(txid(&collateral)),
    ];
    assert_eq!(r.pool.calls(), read);
    assert_eq!(r.shared.info().state, PoolState::Queue);
}

/// A collateral spending an unlocked mempool coin is refused, with no test-accept.
// Core: coinjoin/coinjoin.cpp:480-486
#[test]
fn a_dsa_whose_collateral_spends_an_unlocked_pool_coin_is_refused() {
    let mut r = rig();
    let collateral = pooled_collateral(&r, 30, false);
    let prevout = collateral.inputs()[0].prevout();
    r.dsa_with(0, collateral);
    assert_eq!(r.statuses(0), vec![(REJECTED, PoolMessage::ErrInvalidCollateral as i32)]);
    assert_eq!(r.pool.calls(), vec![Call::Coins(vec![prevout]), Call::Locks(vec![prevout.txid()])]);
    assert_eq!(r.shared.info().state, PoolState::Idle);
}

/// After an honest commit the tick runs again: the next session opens, and the tick completes its
/// queue.
// Core: coinjoin/server.cpp:712-719 (the tick after `cs_check_pool` is released)
#[test]
fn the_tick_runs_again_after_an_honest_commit() {
    let mut r = rig();
    let (signed, _) = r.signing();
    r.sign(signed);
    assert_eq!(r.shared.info().state, PoolState::Idle);
    r.dsa(0);
    r.dsa(1);
    assert_eq!(r.shared.info().state, PoolState::Queue);
    r.shared.tick(false);
    assert_eq!(r.shared.info().state, PoolState::AcceptingEntries, "the tick still skips");
}

/// Gates the next submit; returns when it was issued and the release.
fn gate_submit(pool: &Pool) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (asked, asked_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    pool.book().submit_gate = Some((asked, release_rx));
    (asked_rx, release)
}

/// Peer `n` disconnects: the driver drops its pending admission once woken, and the server no
/// longer finds it.
fn disconnect(core: &NetCore, shared: &SharedServer<Server>, peer: &PeerHandle) {
    assert!(core.disconnect_by_id(peer.id()));
    shared.lock().peers_mut().connected.remove(&peer.addr().service);
}

#[test]
fn a_commit_whose_sender_disconnects_is_finished_by_the_next_tick() {
    let mut r = rig();
    let (signed, final_tx) = r.signing();
    let id = txid(&final_tx);
    r.send(1, DSS, encode(&DsSignedInputs::new(vec![signed[1].clone()])));
    let (issued, release) = gate_submit(&r.pool);
    let (core, shared, last) = (Arc::clone(&r.sim.core), r.shared.clone(), signed[0].clone());
    let sender = r.peers[0].handle.clone();
    let msghand = std::thread::spawn(move || {
        r.send(0, DSS, encode(&DsSignedInputs::new(vec![last])));
        r
    });
    issued.recv_timeout(PATIENCE).expect("the final transaction's submit was issued");
    disconnect(&core, &shared, &sender);
    release.send(()).unwrap();
    let r = msghand.join().unwrap();
    assert_eq!(r.pool.book().pooled.first(), Some(&id));
    assert_eq!(r.shared.info().state, PoolState::Signing, "the admission completed");
    assert!(r.completions(1).is_empty());
    r.shared.tick(false);
    assert!(r.shared.lock().dstx().contains(&id));
    assert_eq!(r.dstx_invs(id), 1);
    assert_eq!(r.completions(1), vec![PoolMessage::MsgSuccess as i32]);
    assert!(r.completions(0).is_empty());
    assert_eq!(r.shared.info().state, PoolState::Idle);
    assert_eq!(r.pool.calls().iter().filter(|c| **c == Call::Submit(id)).count(), 1);
}

#[test]
fn the_tick_neither_charges_nor_resets_a_session_whose_commit_is_in_flight() {
    let mut r = rig();
    let (signed, final_tx) = r.signing();
    let id = txid(&final_tx);
    r.send(1, DSS, encode(&DsSignedInputs::new(vec![signed[1].clone()])));
    let (issued, release) = gate_submit(&r.pool);
    let (shared, pool, clock, last) =
        (r.shared.clone(), r.pool.clone(), r.clock.clone(), signed[0].clone());
    let msghand = std::thread::spawn(move || {
        r.send(0, DSS, encode(&DsSignedInputs::new(vec![last])));
        r
    });
    issued.recv_timeout(PATIENCE).expect("the final transaction's submit was issued");
    clock.0.store(1_000, Ordering::SeqCst);
    pool.book().calls.clear();
    let ticked = shared.clone();
    let state = within(PATIENCE, move || {
        ticked.tick(false);
        ticked.info().state
    });
    let calls = pool.calls();
    release.send(()).unwrap();
    assert_eq!(state, Some(PoolState::Signing), "the tick reset the committing session");
    assert!(calls.is_empty(), "the tick charged a collateral: {calls:?}");
    let r = msghand.join().unwrap();
    for n in 0..2 {
        assert_eq!(r.completions(n), vec![PoolMessage::MsgSuccess as i32]);
    }
    assert_eq!(r.dstx_invs(id), 1);
    assert_eq!(r.shared.info().state, PoolState::Idle);
}

#[test]
fn a_consumed_collateral_whose_sender_disconnects_is_still_relayed() {
    let mut r = rig();
    r.dsa(0);
    r.dsa(1);
    r.shared.tick(false);
    let id = txid(&collateral(1).0);
    let (issued, release) = gate_submit(&r.pool);
    let (core, shared) = (Arc::clone(&r.sim.core), r.shared.clone());
    let sender = r.peers[0].handle.clone();
    let msghand = std::thread::spawn(move || {
        // One input and two outputs is no mix shape: the collateral is charged.
        r.send(0, DSI, encode(&entry(1, 2)));
        r
    });
    issued.recv_timeout(PATIENCE).expect("the collateral's submit was issued");
    disconnect(&core, &shared, &sender);
    release.send(()).unwrap();
    let r = msghand.join().unwrap();
    assert!(r.relayed().is_empty(), "the admission completed");
    r.shared.tick(false);
    assert_eq!(r.relayed(), vec![id]);
    assert_eq!(r.pool.book().pooled, vec![id]);
}
