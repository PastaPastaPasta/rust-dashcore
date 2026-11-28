//! LK1b-IN on the whole node: S1e's `main` capture (`tests/data/lk1b-is/main.json`) submitted to a
//! node whose network dials two scripted peers, and its scenario (b) lock sent by one of them as
//! an `isdlock`. Core citations are v24.0.0.

use std::net::TcpListener;

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::{TxRef, core_serialize, decode_block, decode_tx_exact};
use dash_evo::spork::SporkId;
use dash_num::Hash256;
use dash_p2p::{CommandString, PeerId};
use dash_p2p_core::{GetData, Inv, InvType, Inventory};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec as _;
use dashd_mempool::types::MempoolAcceptResultType;
use dashd_mining::BlockTemplateSource as _;
use serde_json::Value as Json;

use super::*;
use crate::p2p::amb::{Peer, node};

const SPORK_ARG: &str = "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK";
const PATIENCE: Duration = Duration::from_secs(600);
/// A lock's BLS signature ends its encoding (`instantsend/lock.h:39-43`).
const SIG: usize = 96;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn tx(bytes: &[u8]) -> TxRef {
    Arc::new(decode_tx_exact(bytes).expect("a transaction").into_value())
}

/// Whether `f` holds within [`PATIENCE`].
fn waited(mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + PATIENCE;
    while !f() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

fn wait(what: &str, f: impl FnMut() -> bool) {
    assert!(waited(f), "no {what}");
}

/// The capture up to scenario (b)'s lock: the recorder's chain arguments, its blocks to height
/// 189, scenario (a)'s and (b)'s locks, and the transaction (b)'s conflicting block mines in
/// place of (b)'s.
struct Capture {
    args: Vec<String>,
    blocks: Vec<Arc<dash_consensus::Block>>,
    mocktime: i64,
    lock_a: Vec<u8>,
    lock: Vec<u8>,
    lock_time: i64,
    locked: TxRef,
    conflicting: TxRef,
}

impl Capture {
    fn open() -> Capture {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/lk1b-is/main.json");
        let raw: Json = serde_json::from_reader(std::fs::File::open(path).unwrap()).unwrap();
        let recorder = raw["provenance"]["recorder_args"].as_array().expect("recorder_args");
        let mut args: Vec<String> = recorder
            .iter()
            .map(|a| a.as_str().expect("an argument").to_owned())
            .filter(|a| a.starts_with("-llmqtest"))
            .collect();
        // The framework's dash.conf (lk1b_is::framework_conf).
        args.extend(
            ["-dip3params=2:2", "-testactivationheight=v20@100", "-testactivationheight=mn_rr@100"]
                .map(str::to_owned),
        );
        let steps = raw["steps"].as_array().expect("steps");
        let bytes = |s: &Json| unhex(s["op"]["bytes"].as_str().expect("bytes"));
        let is =
            |s: &Json, scenario: &str, op: &str| s["scenario"] == scenario && s["op"]["op"] == op;
        let at = |scenario: &str, op: &str| {
            steps.iter().position(|s| is(s, scenario, op)).expect("the step")
        };
        let b = at("b", "ISLOCK");
        let blocks = steps[..b]
            .iter()
            .filter(|s| s["op"]["op"] == "BLOCK")
            .map(|s| Arc::new(decode_block(&mut &bytes(s)[..]).expect("a block").into_value()))
            .collect();
        let conflict = steps
            .iter()
            .find(|s| is(s, "b", "BLOCK") && s["op"]["core_reply"] == "conflict-tx-lock")
            .expect("(b)'s conflicting block");
        let conflict = decode_block(&mut &bytes(conflict)[..]).expect("a block").into_value();
        let [_, conflicting] = conflict.txs() else {
            panic!("a coinbase and the conflicting transaction")
        };
        Capture {
            args,
            blocks,
            mocktime: steps[0]["mocktime"].as_i64().unwrap(),
            lock_a: bytes(&steps[at("a", "ISLOCK")]),
            lock: bytes(&steps[b]),
            lock_time: steps[b]["mocktime"].as_i64().unwrap(),
            locked: tx(&unhex(steps[b]["op"]["core_tx"].as_str().unwrap())),
            conflicting: Arc::clone(conflicting),
        }
    }

    /// (b)'s lock signed with (a)'s signature: well formed, but not a recovered signature of
    /// (b)'s request.
    fn bad_lock(&self) -> Vec<u8> {
        let mut bad = self.lock.clone();
        let at = bad.len() - SIG;
        bad[at..].copy_from_slice(&self.lock_a[self.lock_a.len() - SIG..]);
        bad
    }
}

/// `::SerializeHash(islock)`, the lock's inventory hash.
fn isdlock_inv(lock: &[u8]) -> Inventory {
    Inventory::new(InvType::IsdLock, Hash256::from_lendian(sha256d(lock)))
}

/// One scripted peer and every frame the node sent it.
struct Wire {
    peer: Peer,
    id: PeerId,
    log: Vec<(CommandString, Vec<u8>)>,
    nonce: u64,
}

impl Wire {
    fn pump(&mut self) {
        let frames = self.peer.poll();
        self.log.extend(frames);
    }

    /// Two `ping` round trips: the node handled every frame sent before.
    fn sync(&mut self) {
        for _ in 0..2 {
            self.nonce += 1;
            let nonce = self.nonce.to_le_bytes();
            self.peer.send("ping", &nonce);
            let deadline = Instant::now() + PATIENCE;
            while !self.log.iter().any(|(c, p)| c.as_str() == "pong" && *p == nonce) {
                assert!(Instant::now() < deadline, "peer={}: no pong", self.id);
                self.pump();
            }
        }
    }

    fn frames(&self, cmd: &str, f: impl Fn(&[u8]) -> bool) -> usize {
        self.log.iter().filter(|(c, p)| c.as_str() == cmd && f(p)).count()
    }

    fn announced(&self, inv: &Inventory) -> usize {
        self.frames("inv", |p| Inv::decode(&mut &p[..]).expect("an inv").inventory().contains(inv))
    }

    fn asked(&self, inv: &Inventory) -> usize {
        self.frames("getdata", |p| {
            GetData::decode(&mut &p[..]).expect("a getdata").inventory().contains(inv)
        })
    }
}

/// The node on the capture's chain at its tip, dialling the two scripted peers.
struct Rig {
    node: Node,
    a: Wire,
    b: Wire,
}

impl Rig {
    fn start(dir: &Path, capture: &Capture) -> Rig {
        let rig = Rig::boot(dir, capture);
        rig.connect(&capture.blocks);
        rig.node.clock.set_mocktime(capture.lock_time);
        rig
    }

    /// The node dialling the two scripted peers, InstantSend on, before the capture's blocks.
    fn boot(dir: &Path, capture: &Capture) -> Rig {
        let listeners = [(); 2].map(|()| TcpListener::bind("127.0.0.1:0").unwrap());
        let connect: Vec<String> =
            listeners.iter().map(|l| format!("-connect={}", l.local_addr().unwrap())).collect();
        let mocktime = format!("-mocktime={}", capture.mocktime);
        let mut argv: Vec<&str> = capture.args.iter().map(String::as_str).collect();
        argv.extend([
            SPORK_ARG,
            mocktime.as_str(),
            "-acceptnonstdtxn=1",
            "-assumevalid=0",
            "-dnsseed=0",
            "-fixedseeds=0",
            "-listen=0",
            "-blocksonly=0",
            // The `mempool` reply is served to a peer only with NODE_BLOOM.
            "-peerbloomfilters=1",
            "-v2transport=0",
        ]);
        argv.extend(connect.iter().map(String::as_str));
        let node = node(dir, &argv);
        let [a, b] = [0, 1].map(|i| accept(&node, &listeners[i], capture.mocktime));
        let rig = Rig {
            node,
            a,
            b,
        };
        let sporks = rig.node.sporks().expect("loaded");
        sporks.update_spork(SporkId::SPORK_2_INSTANTSEND_ENABLED, 0).expect("signed");
        rig
    }

    /// `blocks` submitted to V in turn, each valid.
    fn connect(&self, blocks: &[Arc<dash_consensus::Block>]) {
        let v = self.node.chain().expect("V runs").handle();
        for block in blocks {
            let r = v.process_block(Arc::clone(block), true, None, true).recv_timeout(PATIENCE);
            let r = r.expect("V replied").expect("V serves");
            assert!(r.state.is_valid(), "{}: {:?}", block.hash(), r.state);
        }
    }

    fn signals(&self) -> &ValidationSignals {
        self.node.chain().expect("V runs").signals()
    }

    /// `mnsync next` once V's queued signals have run. The sync reads the best header when a tip
    /// event runs, so a block's event still queued after an event at the best header resets it,
    /// and `TransactionAddedToMempool` then skips every parked lock's transaction.
    // Core: dsnotificationinterface.cpp:60, masternode/sync.cpp:148-152,
    // instantsend/net_instantsend.cpp:449 at v24.0.2
    fn sync_blockchain(&self) {
        self.signals()
            .sync(&mut dashd_chainstate::chain::Blocking)
            .expect("the queued signals ran");
        self.node.mn_sync().expect("constructed").switch_to_next_asset();
    }

    fn table(&self, dir: &Path) -> Arc<dashd_rpc::server::RpcServer> {
        command_table(&self.node, dir)
    }

    fn accept(&self, tx: &TxRef) -> dashd_mempool::types::MempoolAcceptResult {
        let pool = self.node.steps.mempool().expect("step 7 built it").handle();
        pool.accept(Arc::clone(tx), false).expect("V replies")
    }

    fn pooled(&self, tx: &TxRef) -> bool {
        let pool = self.node.steps.mempool().expect("step 7 built it");
        pool.pool().read().exists(&tx.txid())
    }

    /// (b)'s lock from peer A before its transaction, while `conflicting` holds the pool: parked
    /// and relayed to B, the conflict left pooled.
    // Core: net_instantsend.cpp:396-398, :416-422
    fn park_lock(&mut self, capture: &Capture, conflicting: &TxRef) {
        self.sync_blockchain();
        assert_eq!(self.accept(conflicting).result_type, MempoolAcceptResultType::Valid);
        let inv = isdlock_inv(&capture.lock);
        self.a.peer.send("isdlock", &capture.lock);
        self.a.sync();
        let b = &mut self.b;
        wait("the relayed inv", || {
            b.pump();
            b.announced(&inv) > 0
        });
        assert!(self.pooled(conflicting), "the lock's transaction is unknown");
    }

    /// Whether (b)'s lock is in the IS store.
    fn written(&self, capture: &Capture) -> bool {
        let manager = &self.node.steps.instantsend().expect("attached").manager;
        manager.is_locked(&capture.locked.txid()).expect("the store reads")
    }

    /// What tells a stalled lock's cases apart: the pool, the sync, where the IS manager holds
    /// (b)'s lock, what `AlreadyHave` reads for its transaction besides the recent rejects (which
    /// hold it from its refusal on: the tip never moves), and the requests.
    fn state(&mut self, capture: &Capture) -> String {
        self.a.pump();
        self.b.pump();
        let txid = capture.locked.txid();
        let hash = sha256d(&capture.lock);
        let manager = &self.node.steps.instantsend().expect("attached").manager;
        let sync = self.node.mn_sync().expect("constructed");
        let tx_inv = Inventory::new(InvType::Tx, Hash256::from_lendian(txid.to_lendian()));
        let lock_inv = isdlock_inv(&capture.lock);
        format!(
            "pooled: locked={} conflicting={}; mnsync: {} (blockchain synced {}); lock: parked={} \
             queued or known={:?} written={:?} counts={:?}; AlreadyHave(tx) skips the recent \
             rejects: waiting={} locked={:?}; getdata to A: tx {}, lock {}; inv to B: lock {}",
            self.pooled(&capture.locked),
            self.pooled(&capture.conflicting),
            sync.asset_name(),
            sync.is_blockchain_synced(),
            !manager.has_tx_for_lock(&hash),
            manager.already_have(&hash),
            manager.is_locked(&txid),
            manager.counts(),
            manager.is_waiting_for_tx(&txid),
            manager.is_locked(&txid),
            self.a.asked(&tx_inv),
            self.a.asked(&lock_inv),
            self.b.announced(&lock_inv),
        )
    }

    fn score(&self, id: PeerId) -> i32 {
        self.node.net_shared().expect("attached").misbehavior(id).expect("connected").0
    }
}

/// The node's connection to `listener`, handshaken at `time` and synced.
fn accept(node: &Node, listener: &TcpListener, time: i64) -> Wire {
    let params = Arc::new(node.params().chain.clone());
    let peer = Peer::dialled(listener, &params, 0, time);
    let port = listener.local_addr().unwrap().port();
    let core = Arc::clone(node.net().expect("started").core());
    let connected = || {
        core.peers()
            .iter()
            .find(|p| p.addr().service.port == port && p.successfully_connected())
            .map(|p| p.id())
    };
    wait("connection", || connected().is_some());
    let mut wire = Wire {
        peer,
        id: connected().unwrap(),
        log: Vec::new(),
        nonce: 0,
    };
    wire.sync();
    wire
}

fn rpc(table: &dashd_rpc::server::RpcServer, method: &str, params: &str) -> Json {
    let out = crate::chain::tests::rpc(table, method, params).expect(method);
    serde_json::from_str(&out).unwrap()
}

/// Whether the commands report `txid` locked: `getrawtransaction`'s `instantlock` and
/// `getrawmempool`'s, which Core writes as a string (`rpc/mempool.cpp:311`,
/// `rpc/rawtransaction.cpp:163-165`).
fn reported_locked(table: &dashd_rpc::server::RpcServer, txid: &str) -> (bool, String) {
    let raw = rpc(table, "getrawtransaction", &format!(r#"["{txid}", true]"#));
    let pool = rpc(table, "getrawmempool", "[true]");
    let entry = pool[txid]["instantlock"].as_str().expect("a pooled entry").to_owned();
    (raw["instantlock"].as_bool().expect("instantlock"), entry)
}

/// A pooled transaction's lock from a peer: an `isdlock` signed by the wrong recovered signature
/// costs the sender 20 (`instantsend/net_instantsend.cpp:200-203`) and locks nothing; the real
/// one is verified on the `isman` thread, stored, relayed to the other peer
/// (`net_instantsend.cpp:416-422`), and reported by `getrawtransaction`, `getrawmempool` and
/// `getmempoolinfo`. A transaction spending the lock's input is then refused as
/// `tx-txlock-conflict` (`validation.cpp:902-911`), where the pool alone answers
/// `txn-mempool-conflict`.
// Core: net_instantsend.cpp:238-289, :359-423, :425-445; instantsend.cpp:400-407, :447-450;
// rpc/mempool.cpp:672
#[test]
fn a_peers_lock_on_a_pooled_transaction_is_verified_stored_and_reported() {
    let capture = Capture::open();
    let dir = tempfile::tempdir().unwrap();
    let mut rig = Rig::start(dir.path(), &capture);
    let table = rig.table(dir.path());
    let txid = capture.locked.txid().to_string();
    assert_eq!(rig.accept(&capture.locked).result_type, MempoolAcceptResultType::Valid);
    assert_eq!(reported_locked(&table, &txid), (false, "false".to_owned()));

    rig.a.peer.send("isdlock", &capture.bad_lock());
    rig.a.sync();
    wait("the bad lock's score", || rig.score(rig.a.id) == 20);
    assert_eq!(reported_locked(&table, &txid), (false, "false".to_owned()));

    let inv = isdlock_inv(&capture.lock);
    rig.a.peer.send("isdlock", &capture.lock);
    rig.a.sync();
    wait("the lock reported", || reported_locked(&table, &txid).0);
    assert_eq!(reported_locked(&table, &txid), (true, "true".to_owned()));
    assert_eq!(rpc(&table, "getmempoolinfo", "[]")["instantsendlocks"], 1);
    let b = &mut rig.b;
    wait("the relayed inv", || {
        b.pump();
        b.announced(&inv) > 0
    });
    assert_eq!(rig.score(rig.a.id), 20, "a valid lock costs nothing");

    let refused = rig.accept(&capture.conflicting);
    assert_eq!(refused.result_type, MempoolAcceptResultType::Invalid);
    assert_eq!(refused.reason, "tx-txlock-conflict", "{refused:?}");
    assert!(rig.pooled(&capture.locked));
    rig.node.shutdown();
}

/// DEC-1935: with InstantSend on, the node's templates hold an unlocked pooled transaction back
/// (`IsTxSafeForMining` never true until BL-996) and include it once its lock is stored.
// Core: v24.0.2 src/node/miner.cpp:429-436
#[test]
fn a_template_mines_a_pooled_transaction_only_once_it_is_locked() {
    let capture = Capture::open();
    let dir = tempfile::tempdir().unwrap();
    let mut rig = Rig::start(dir.path(), &capture);
    let table = rig.table(dir.path());
    let txid = capture.locked.txid();
    assert_eq!(rig.accept(&capture.locked).result_type, MempoolAcceptResultType::Valid);
    let miner = rig.node.rpc_chain.miner().expect("the chain is attached");
    let mined = || {
        let template = miner.create_new_block(&[0x51], dashd_mining::TemplateTxs::Mempool);
        template.expect("a template").block.txs().iter().any(|tx| tx.txid() == txid)
    };
    assert!(!mined(), "an unlocked transaction is held back");

    rig.a.peer.send("isdlock", &capture.lock);
    rig.a.sync();
    wait("the lock reported", || reported_locked(&table, &txid.to_string()).0);
    assert!(mined(), "a locked transaction is mined");
    rig.node.shutdown();
}

/// A lock that arrives before its transaction waits for it, while a conflicting transaction
/// holds the pool; the locked transaction then evicts the conflict on admission
/// (`validation.cpp:913-916`, `IsWaitingForTx`) and takes the lock (`instantsend.cpp:166-198`,
/// `net_instantsend.cpp:447-463`): reported locked, and the conflict gone.
// Core: net_instantsend.cpp:396-398, :475-505; instantsend.cpp:409-426
#[test]
fn a_lock_waiting_for_its_transaction_evicts_a_pooled_conflict() {
    let capture = Capture::open();
    let dir = tempfile::tempdir().unwrap();
    let mut rig = Rig::start(dir.path(), &capture);
    let table = rig.table(dir.path());
    rig.park_lock(&capture, &capture.conflicting);
    assert_eq!(rpc(&table, "getmempoolinfo", "[]")["instantsendlocks"], 0);

    assert_eq!(rig.accept(&capture.locked).result_type, MempoolAcceptResultType::Valid);
    assert!(!rig.pooled(&capture.conflicting), "the conflict is evicted");
    let txid = capture.locked.txid().to_string();
    wait("the lock reported", || reported_locked(&table, &txid).0);
    assert_eq!(reported_locked(&table, &txid), (true, "true".to_owned()));
    rig.node.shutdown();
}

/// The listener's half of a parked lock: `TransactionAddedToMempool` for its transaction removes
/// the pool's conflicts through the mempool the node hands NetInstantSend; the worker's retry,
/// still without the transaction, removes none (`net_instantsend.cpp:396-410`).
// Core: net_instantsend.cpp:447-463, :475-505; init.cpp:2198
#[test]
fn the_is_listener_removes_a_parked_locks_pool_conflicts() {
    let capture = Capture::open();
    let dir = tempfile::tempdir().unwrap();
    let mut rig = Rig::start(dir.path(), &capture);
    rig.park_lock(&capture, &capture.conflicting);

    let (_, listener) = rig.node.steps.is_listener().expect("NetInstantSend listens");
    listener.transaction_added_to_mempool(&capture.locked, capture.lock_time, 0);
    assert!(
        !rig.pooled(&capture.conflicting),
        "the listener left the conflict pooled: {}",
        rig.state(&capture)
    );
    rig.node.shutdown();
}

/// A transaction a peer sent while a conflict held the pool is refused `txn-mempool-conflict` and
/// remembered as rejected; its lock then waits for it, so `AlreadyHave` skips the recent rejects
/// (`net_processing.cpp:2394-2410`) and `AskPeersForTransaction` requests it from that peer
/// (`:2478-2516`, `net_instantsend.cpp:419-421`). Sent again, it evicts the conflict
/// (`validation.cpp:913-916`) and is reported locked.
#[test]
fn a_rejected_transaction_is_requested_again_for_its_lock() {
    let capture = Capture::open();
    let dir = tempfile::tempdir().unwrap();
    let mut rig = Rig::start(dir.path(), &capture);
    let table = rig.table(dir.path());
    rig.sync_blockchain();
    assert_eq!(rig.accept(&capture.conflicting).result_type, MempoolAcceptResultType::Valid);
    let refused = rig.accept(&capture.locked);
    assert_eq!(refused.reason, "txn-mempool-conflict", "{refused:?}");
    let raw = core_serialize(&*capture.locked);
    let inv =
        Inventory::new(InvType::Tx, Hash256::from_lendian(capture.locked.txid().to_lendian()));
    rig.a.peer.send("tx", &raw);
    rig.a.sync();
    assert!(!rig.pooled(&capture.locked));
    assert_eq!(rig.a.asked(&inv), 0);

    rig.a.peer.send("isdlock", &capture.lock);
    let a = &mut rig.a;
    wait("the transaction requested again", || {
        a.pump();
        a.asked(&inv) > 0
    });
    a.peer.send("tx", &raw);
    a.sync();
    if !waited(|| rig.pooled(&capture.locked)) {
        panic!("no transaction pooled again: {}", rig.state(&capture));
    }
    if !waited(|| rig.written(&capture)) {
        panic!("no lock written: {}", rig.state(&capture));
    }
    let txid = capture.locked.txid().to_string();
    assert_eq!(reported_locked(&table, &txid), (true, "true".to_owned()));
    assert!(!rig.pooled(&capture.conflicting), "the conflict is evicted");
    rig.node.shutdown();
}

/// Holds V's signals thread in `block`'s `BlockConnected` until opened, and every event queued
/// after it.
struct Gate {
    block: BlockHash,
    open: Mutex<bool>,
    cond: std::sync::Condvar,
}

impl Gate {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.cond.notify_all();
    }
}

impl ValidationInterface for Gate {
    fn block_connected(
        &self,
        block: &Arc<dash_consensus::Block>,
        _: BlockId,
    ) -> dashd_chainstate::chain::Cont {
        if block.hash() == self.block {
            let open = self.open.lock().unwrap();
            drop(self.cond.wait_while(open, |open| !*open).unwrap());
        }
        dashd_chainstate::chain::Cont::Done
    }
}

/// The rig's switch to a synced blockchain outlasts a tip event of its blocks still queued: the
/// sync, at the best header after the earlier blocks, takes the next block's event once the last
/// block is the best header, and resets (`masternode/sync.cpp:148-152`) unless that event ran
/// before the switch.
#[test]
fn a_queued_tip_event_does_not_undo_the_rigs_sync() {
    let capture = Capture::open();
    let dir = tempfile::tempdir().unwrap();
    let mut rig = Rig::boot(dir.path(), &capture);
    let (earlier, last_two) = capture.blocks.split_at(capture.blocks.len() - 2);
    rig.connect(earlier);
    rig.signals().sync(&mut dashd_chainstate::chain::Blocking).expect("the queued signals ran");
    let gate = Arc::new(Gate {
        block: last_two[0].hash(),
        open: Mutex::new(false),
        cond: std::sync::Condvar::new(),
    });
    rig.signals().register(Arc::clone(&gate) as Arc<dyn ValidationInterface>);
    rig.connect(last_two);
    rig.node.clock.set_mocktime(capture.lock_time);
    let sync = Arc::clone(rig.node.steps.is_sync.as_ref().expect("constructed"));
    let opener = std::thread::spawn({
        let (gate, sync) = (Arc::clone(&gate), Arc::clone(&sync));
        move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !sync.is_blockchain_synced() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            gate.open();
        }
    });
    rig.sync_blockchain();
    opener.join().unwrap();
    rig.signals().sync(&mut dashd_chainstate::chain::Blocking).expect("the queued signals ran");
    assert!(sync.is_blockchain_synced(), "the sync reset to {}", sync.asset_name());
    rig.node.shutdown();
}

/// A stored lock is served: a `getdata` for its hash gets the `isdlock`
/// (`GetInstantSendLockByHash`), its `inv` is not requested where an unknown lock's in the same
/// message is (`AlreadyHave`), and the `mempool` reply announces it after its transaction.
// Core: net_processing.cpp:2446-2447, :3054-3058, :6655-6664; instantsend.cpp:355-398
#[test]
fn a_stored_lock_is_served_and_not_requested_again() {
    let capture = Capture::open();
    let dir = tempfile::tempdir().unwrap();
    let mut rig = Rig::start(dir.path(), &capture);
    let table = rig.table(dir.path());
    assert_eq!(rig.accept(&capture.locked).result_type, MempoolAcceptResultType::Valid);
    rig.a.peer.send("isdlock", &capture.lock);
    rig.a.sync();
    let txid = capture.locked.txid().to_string();
    wait("the lock reported", || reported_locked(&table, &txid).0);

    let inv = isdlock_inv(&capture.lock);
    let b = &mut rig.b;
    let mut buf = Vec::new();
    GetData::new(vec![inv]).encode(&mut buf);
    b.peer.send("getdata", &buf);
    b.sync();
    assert_eq!(b.frames("isdlock", |p| p == capture.lock), 1, "the lock served");

    let unknown = Inventory::new(InvType::IsdLock, Hash256::from_lendian([9; 32]));
    let mut buf = Vec::new();
    Inv::new(vec![inv, unknown]).encode(&mut buf);
    b.peer.send("inv", &buf);
    wait("the unknown lock requested", || {
        b.pump();
        b.asked(&unknown) > 0
    });
    assert_eq!(b.asked(&inv), 0, "the stored lock requested");

    let announced = b.announced(&inv);
    b.peer.send("mempool", &[]);
    b.sync();
    let clock = Arc::clone(&rig.node.clock);
    for _ in 0..120 {
        if b.announced(&inv) > announced {
            break;
        }
        clock.set_mocktime(clock.mocktime() + 1);
        b.sync();
    }
    assert!(b.announced(&inv) > announced, "no lock inv in the mempool reply");
    rig.node.shutdown();
}
