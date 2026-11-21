//! LK1a F on the whole node (rule 4, manager PLAN-CHECK 2026-11-12): the recorded
//! `feature_llmq_chainlocks` chain submitted to a node whose network dials two scripted peers.
//! A `clsig` inv is requested only out of IBD and never resurrected at the exit; a requested lock
//! is recorded in V and relayed; a lock that fails verification costs the sender 10; the best
//! lock is served to `getdata` and in the `mempool` reply; the coinbase route records a CbTx lock
//! once the blockchain is synced; and after a restart nothing is seeded, the lock is relearned
//! by gossip and served again.
// Core: net_processing.cpp:3046-3051, :4575-4594, :5426-5454, :5794-5833, :6668-6675;
// chainlock/handler.cpp:104-183 at v24.0.0

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dash_chain::SporkView as _;
use dash_clock::{Clock as _, MockClock};
use dash_consensus::Block;
use dash_evo::locks::{ClsigHashSchemes, clsig_hash};
use dash_evo::spork::SporkId;
use dash_num::Hash256;
use dash_p2p::{CommandString, PeerId};
use dash_p2p_core::{ClSig, GetData, Inv, InvType, Inventory};
use dash_pkc::bls::BlsSigBytes;
use dash_primitives::BlockHash;
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec as _;
use dashd_chainstate::chain::{ChainHandle, Outcome, WaitError};
use dashd_locks::chainlock::{ChainLockHandler, Read};
use dashd_p2p::processing::seams::ChainLocks as _;

#[cfg(feature = "test-support")]
use super::amb::node_with;
use super::amb::{Peer, node};
use super::realv::{HANG_GUARD as PATIENCE, until};
use crate::locks::{ClPoint, NodeClChain, SharedHandler};
use crate::node::Node;

const SPORK_ARG: &str = "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK";

type Frames = Vec<(CommandString, Vec<u8>)>;

/// The recorded run: its chain arguments and blocks, genesis first.
struct Run {
    args: Vec<String>,
    blocks: Vec<Arc<Block>>,
}

impl Run {
    fn open() -> Run {
        let dir: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../test-vectors/statedigest/regtest-v24/feature_llmq_chainlocks/node0");
        let extract: serde_json::Value =
            serde_json::from_reader(std::fs::File::open(dir.join("extract.json")).unwrap())
                .unwrap();
        let args = extract["chain_args"].as_array().expect("chain_args");
        let args = args.iter().map(|a| a.as_str().expect("an argument").to_owned()).collect();
        let raw =
            zstd::decode_all(std::fs::File::open(dir.join("blocks.dat.zst")).unwrap()).unwrap();
        let (mut rest, mut blocks) = (raw.as_slice(), Vec::new());
        while let Some((record, tail)) = rest.split_at_checked(8) {
            let size = u32::from_le_bytes(record[4..].try_into().unwrap());
            let (mut bytes, tail) = tail.split_at(usize::try_from(size).unwrap());
            let block = dash_consensus::primitives::decode_block(&mut bytes).expect("a block");
            blocks.push(Arc::new(block.into_value()));
            rest = tail;
        }
        Run {
            args,
            blocks,
        }
    }

    /// The run's distinct non-null CbTx ChainLocks, each at the first block carrying it.
    fn cbtx_locks(&self) -> Vec<Lock> {
        let mut out: Vec<Lock> = Vec::new();
        for (at, b) in (1_u32..).zip(&self.blocks[1..]) {
            let payload = b.txs()[0].to_mutable().extra_payload().clone();
            if payload.len() < 167 || u16::from_le_bytes([payload[0], payload[1]]) < 3 {
                continue;
            }
            let sig: [u8; 96] = payload[71..167].try_into().expect("96 bytes");
            if sig == [0; 96] {
                continue;
            }
            let height = at - 1 - u32::from(payload[70]);
            if out.last().is_some_and(|l| l.height == height) {
                continue;
            }
            out.push(Lock {
                at,
                height,
                hash: self.blocks[usize::try_from(height).unwrap()].hash(),
                sig,
            });
        }
        out
    }

    fn latest_time(&self) -> i64 {
        i64::from(self.blocks.last().expect("blocks").header().raw().time())
    }
}

/// A CbTx ChainLock: the first block carrying it, the locked height, hash and signature.
#[derive(Clone, Copy, Debug)]
struct Lock {
    at: u32,
    height: u32,
    hash: BlockHash,
    sig: [u8; 96],
}

impl Lock {
    fn clsig(&self) -> ClSig {
        ClSig::new(i32::try_from(self.height).unwrap(), self.hash, BlsSigBytes::from(self.sig))
    }
}

fn inv_hash(clsig: &ClSig, node: &Node) -> [u8; 32] {
    let bls = node.chain().expect("V runs").handle().view().bls;
    clsig_hash(clsig, ClsigHashSchemes::both(bls)).expect("a decodable lock")
}

fn clsig_inv(hash: [u8; 32]) -> Inventory {
    Inventory::new(InvType::ClSig, Hash256::from_lendian(hash))
}

/// One scripted peer and every frame the node sent it.
struct Wire {
    peer: Peer,
    id: PeerId,
    log: Frames,
    nonce: u64,
}

impl Wire {
    fn send(&mut self, cmd: &str, payload: &[u8]) {
        self.peer.send(cmd, payload);
    }

    fn pump(&mut self) {
        let frames = self.peer.poll();
        self.log.extend(frames);
    }

    fn until(&mut self, what: &str, f: impl Fn(&Wire) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            self.pump();
            if f(self) {
                return;
            }
            assert!(Instant::now() < deadline, "peer={}: no {what}", self.id);
        }
    }

    /// Two `ping` round trips: the node handled every frame sent before and ran the
    /// `SendMessages` pass after it.
    fn sync(&mut self) {
        for _ in 0..2 {
            self.nonce += 1;
            let nonce = self.nonce.to_le_bytes();
            self.send("ping", &nonce);
            self.until("pong", |w| w.log.iter().any(|(c, p)| c.as_str() == "pong" && *p == nonce));
        }
    }

    fn inv(&mut self, hash: [u8; 32]) {
        let mut buf = Vec::new();
        Inv::new(vec![clsig_inv(hash)]).encode(&mut buf);
        self.send("inv", &buf);
    }

    fn getdata(&mut self, hash: [u8; 32]) {
        let mut buf = Vec::new();
        GetData::new(vec![clsig_inv(hash)]).encode(&mut buf);
        self.send("getdata", &buf);
    }

    fn clsig(&mut self, clsig: &ClSig) {
        let mut buf = Vec::new();
        clsig.encode(&mut buf);
        self.send("clsig", &buf);
    }

    /// The `getdata`s asking for `hash`.
    fn asked(&self, hash: [u8; 32]) -> usize {
        self.frames("getdata", |p| {
            GetData::decode(&mut &p[..]).expect("a getdata").inventory().contains(&clsig_inv(hash))
        })
    }

    /// The `inv`s naming `hash`.
    fn announced(&self, hash: [u8; 32]) -> usize {
        self.frames("inv", |p| {
            Inv::decode(&mut &p[..]).expect("an inv").inventory().contains(&clsig_inv(hash))
        })
    }

    /// The `clsig`s carrying `clsig`.
    fn served(&self, clsig: &ClSig) -> usize {
        self.frames("clsig", |p| ClSig::decode(&mut &p[..]).is_ok_and(|c| c == *clsig))
    }

    fn frames(&self, cmd: &str, f: impl Fn(&[u8]) -> bool) -> usize {
        self.log.iter().filter(|(c, p)| c.as_str() == cmd && f(p)).count()
    }
}

/// The node on the run's chain, dialling the two scripted peers.
struct Rig {
    node: Node,
    a: Wire,
    b: Option<Wire>,
}

impl Rig {
    /// The node at `now` with A connected; B's connection waits in its listener's backlog
    /// until [`Rig::accept_b`].
    fn start(dir: &Path, run: &Run, listeners: &[TcpListener; 2], now: i64) -> Rig {
        let connect: Vec<String> =
            listeners.iter().map(|l| format!("-connect={}", l.local_addr().unwrap())).collect();
        let mocktime = format!("-mocktime={now}");
        let mut argv: Vec<&str> = run.args.iter().map(String::as_str).collect();
        argv.extend([
            SPORK_ARG,
            mocktime.as_str(),
            "-assumevalid=0",
            "-dnsseed=0",
            "-fixedseeds=0",
            "-listen=0",
            // The `mempool` reply needs tx relay, which Core's blocks-only mode turns off.
            "-blocksonly=0",
            "-peerbloomfilters=1",
            "-v2transport=0",
        ]);
        argv.extend(connect.iter().map(String::as_str));
        let node = node(dir, &argv);
        let a = accept(&node, &listeners[0], now);
        Rig {
            node,
            a,
            b: None,
        }
    }

    /// B's handshake at `now`. Core draws a peer's first trickle time at its first
    /// `SendMessages` after the handshake (net_processing.cpp:6592-6609 at v24.0.0), so B's follows the
    /// clock from here on.
    fn accept_b(&mut self, listener: &TcpListener, now: i64) {
        self.b = Some(accept(&self.node, listener, now));
    }

    fn b(&mut self) -> &mut Wire {
        self.b.as_mut().expect("B accepted")
    }

    fn submit(&mut self, blocks: &[Arc<Block>]) {
        let v = self.node.chain().expect("V runs").handle().clone();
        for block in blocks {
            let r = v.process_block(Arc::clone(block), true, None, true).recv_timeout(PATIENCE);
            let r = r.expect("V replied").expect("V serves");
            assert!(r.state.is_valid(), "{}: {:?}", block.hash(), r.state);
            self.a.pump();
            if let Some(b) = &mut self.b {
                b.pump();
            }
        }
    }

    fn ibd(&self) -> bool {
        let view = self.node.chain().expect("V runs").handle().view_handle();
        view.is_initial_block_download(&view.view().snap, self.node.clock.node_time())
    }

    fn best_height(&self) -> i32 {
        self.node.chain().expect("V runs").handle().view().cl.best_height()
    }

    fn score(&self, id: PeerId) -> i32 {
        self.node.net_shared().expect("attached").misbehavior(id).expect("connected").0
    }

    /// `mempool` from B, then the lock's inv in the reply. The reply waits for B's trickle
    /// (Core: net_processing.cpp:6592-6609 at v24.0.0), which runs on the mocked clock, so the clock
    /// moves a second per round.
    fn mempool_reply(&mut self, hash: [u8; 32]) {
        let clock = Arc::clone(&self.node.clock);
        let b = self.b();
        let before = b.announced(hash);
        b.send("mempool", &[]);
        b.sync();
        for _ in 0..120 {
            if b.announced(hash) > before {
                return;
            }
            clock.set_mocktime(clock.mocktime() + 1);
            b.sync();
        }
        panic!("no inv in the mempool reply");
    }

    fn spork_19_on(&self) {
        let sporks = self.node.sporks().expect("loaded");
        sporks.update_spork(SporkId::SPORK_19_CHAINLOCKS_ENABLED, 0).expect("signed");
        assert!(sporks.chainlocks_enabled());
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
    until(|| connected().is_some());
    let mut wire = Wire {
        peer,
        id: connected().unwrap(),
        log: Vec::new(),
        nonce: 0,
    };
    wire.sync();
    wire
}

/// Requests a lock `from` announces and delivers it: `getdata` asked once, `clsig` sent.
fn gossip(from: &mut Wire, clsig: &ClSig, hash: [u8; 32]) {
    let asked = from.asked(hash);
    from.inv(hash);
    let deadline = Instant::now() + PATIENCE;
    while from.asked(hash) == asked {
        from.pump();
        assert!(Instant::now() < deadline, "peer={}: the lock was not requested", from.id);
    }
    from.clsig(clsig);
    from.sync();
}

/// Rule 4 on the whole node: an inv in IBD is not requested and not resurrected at the exit;
/// out of IBD the inv is requested, the lock recorded in V and relayed to the other peer, and
/// served to its `getdata` and in its `mempool` reply; locks that fail verification (no quorum,
/// a bad signature) cost the sender 10 each; once the blockchain is synced the coinbase route
/// records a newer CbTx lock and relays it. After a restart nothing is seeded: the best lock is
/// relearned by gossip, then served again.
#[test]
fn chainlocks_gossip_coinbase_and_restart_on_the_node() {
    let run = Run::open();
    let locks = run.cbtx_locks();
    assert!(locks.len() >= 2, "{locks:?}");
    let (gossiped, carried) = (locks[locks.len() - 2], locks[locks.len() - 1]);
    let now = run.latest_time() + 60;
    let dir = tempfile::tempdir().unwrap();
    let listeners = [(); 2].map(|()| TcpListener::bind("127.0.0.1:0").unwrap());
    // The run spans less than an hour: two days on, even its genesis tip is in IBD. B
    // connects once the clock is back, so its trickle follows that clock.
    let mut rig = Rig::start(dir.path(), &run, &listeners, now + 2 * 86_400);
    rig.spork_19_on();
    let clsig = gossiped.clsig();
    // Hashed under genesis's scheme: the inv announced in IBD.
    let early = inv_hash(&clsig, &rig.node);

    // Gate 1: in IBD the inv is not requested.
    assert!(rig.ibd());
    rig.a.inv(early);
    rig.a.sync();
    assert_eq!(rig.a.asked(early), 0, "requested in IBD");

    // The chain up to the block before the first one carrying the gossiped lock: out of IBD,
    // and the inv seen in IBD is not resurrected.
    let tip = usize::try_from(gossiped.at).unwrap();
    rig.node.clock.set_mocktime(now);
    rig.submit(&run.blocks[1..tip]);
    until(|| !rig.ibd());
    rig.a.sync();
    assert_eq!(rig.a.asked(early), 0, "resurrected at the IBD exit");
    assert_eq!(rig.best_height(), -1, "nothing recorded yet");
    rig.accept_b(&listeners[1], now);
    // Past V19 the node hashes the lock under the basic scheme.
    let hash = inv_hash(&clsig, &rig.node);

    // Out of IBD: inv → getdata → clsig → record → relay.
    gossip(&mut rig.a, &clsig, hash);
    until(|| rig.best_height() == i32::try_from(gossiped.height).unwrap());
    let handler = rig.node.net().and_then(super::Net::chainlocks).expect("masternode services");
    assert!(handler.already_have(&hash), "the lock's hash is in the seen map");
    rig.b().until("the relayed inv", |w| w.announced(hash) > 0);
    assert_eq!(rig.score(rig.a.id), 0, "a valid solicited lock");

    // Gate 4: served to getdata and in the mempool reply.
    rig.b().getdata(hash);
    rig.b().until("the served lock", |w| w.served(&clsig) > 0);
    rig.mempool_reply(hash);

    // Locks that fail verification, each solicited: 10 apiece.
    let height = i32::try_from(gossiped.height).unwrap();
    let tip_height = i32::try_from(tip - 1).unwrap();
    let next = run.blocks[usize::try_from(height + 1).unwrap()].hash();
    let invalid = ClSig::new(height + 1, next, BlsSigBytes::from(gossiped.sig));
    let no_quorum = ClSig::new(tip_height + 20, gossiped.hash, BlsSigBytes::from(gossiped.sig));
    for (n, bad) in [invalid, no_quorum].iter().enumerate() {
        let bad_hash = inv_hash(bad, &rig.node);
        gossip(&mut rig.a, bad, bad_hash);
        let want = 10 * (i32::try_from(n).unwrap() + 1);
        until(|| rig.score(rig.a.id) == want);
    }
    assert_eq!(rig.best_height(), height, "neither recorded");

    // The coinbase route, once the blockchain is synced: the newer CbTx lock is recorded and
    // relayed.
    let signals = rig.node.chain().expect("V runs").signals();
    signals.sync(&mut dashd_chainstate::chain::Blocking).expect("the earlier events ran");
    rig.node.mn_sync().expect("constructed").switch_to_next_asset();
    let carried_clsig = carried.clsig();
    let carried_hash = inv_hash(&carried_clsig, &rig.node);
    // One block at a time, each block's events run before the next: the sync reads the best
    // header when its tip event runs, so a lagging event resets it and the carried lock is
    // skipped, as in Core.
    // Core: dsnotificationinterface.cpp:60, masternode/sync.cpp:148-152,
    // chainlock/handler.cpp:106-110 at v24.0.0
    for block in &run.blocks[tip..=usize::try_from(carried.at).unwrap()] {
        rig.submit(std::slice::from_ref(block));
        let signals = rig.node.chain().expect("V runs").signals();
        signals.sync(&mut dashd_chainstate::chain::Blocking).expect("the block's signals ran");
    }
    until(|| rig.best_height() == i32::try_from(carried.height).unwrap());
    rig.b().until("the coinbase lock's inv", |w| w.announced(carried_hash) > 0);

    // Restart: nothing seeded; relearned by gossip, then served.
    let Rig {
        mut node,
        a,
        b,
    } = rig;
    node.shutdown();
    drop((node, a, b));
    let mut rig = Rig::start(dir.path(), &run, &listeners, now);
    rig.accept_b(&listeners[1], now);
    rig.spork_19_on();
    assert_eq!(rig.best_height(), -1, "no lock seeded at startup");
    until(|| !rig.ibd());
    gossip(&mut rig.a, &carried_clsig, carried_hash);
    until(|| rig.best_height() == i32::try_from(carried.height).unwrap());
    rig.b().getdata(carried_hash);
    rig.b().until("the relearned lock served", |w| w.served(&carried_clsig) > 0);
    rig.mempool_reply(carried_hash);
    rig.node.shutdown();
}

type Events = Arc<std::sync::Mutex<Vec<String>>>;

fn push(events: &Events, event: String) {
    events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(event);
}

fn position(events: &[String], event: &str) -> usize {
    events.iter().position(|e| e == event).unwrap_or_else(|| panic!("no {event:?} in {events:?}"))
}

/// A listener registered after the P2P listener: per callback, V's best ChainLock height as it
/// ran.
struct After {
    v: dashd_chainstate::chain::ChainHandle,
    events: Events,
}

impl dashd_chainstate::chain::ValidationInterface for After {
    fn block_connected(
        &self,
        _: &Arc<Block>,
        _: dash_consensus::BlockId,
    ) -> dashd_chainstate::chain::Cont {
        push(&self.events, format!("after {}", self.v.view().cl.best_height()));
        dashd_chainstate::chain::Cont::Done
    }

    fn updated_block_tip(
        &self,
        _: dash_consensus::BlockId,
        _: Option<dash_consensus::BlockId>,
        _: bool,
    ) {
        push(&self.events, format!("tip {}", self.v.view().cl.best_height()));
    }
}

/// The node alone at `now` on the run's chain before `upto`, SPORK_19 on if `spork`, the
/// blockchain not synced, and every signal the blocks queued run: no coinbase job learned a
/// lock.
fn alone_before(dir: &Path, run: &Run, upto: usize, spork: bool, now: i64) -> Node {
    let mut argv = alone_argv(run, now);
    argv.push("-connect=0".into());
    let node = node(dir, &argv.iter().map(String::as_str).collect::<Vec<_>>());
    if spork {
        let sporks = node.sporks().expect("loaded");
        sporks.update_spork(SporkId::SPORK_19_CHAINLOCKS_ENABLED, 0).expect("signed");
    }
    let v = node.chain().expect("V runs").handle().clone();
    for block in &run.blocks[1..upto] {
        connect(&v, block);
    }
    let signals = node.chain().expect("V runs").signals();
    signals.sync(&mut dashd_chainstate::chain::Blocking).expect("the setup's signals ran");
    assert!(!node.mn_sync().expect("constructed").is_blockchain_synced());
    assert_eq!(v.view().cl.best_height(), -1, "nothing learned");
    node
}

fn until_out_of_ibd(node: &Node) {
    let view = node.chain().expect("V runs").handle().view_handle();
    until(|| !view.is_initial_block_download(&view.view().snap, node.clock.node_time()));
}

/// `block` submitted to V, and valid.
fn connect(v: &dashd_chainstate::chain::ChainHandle, block: &Arc<Block>) {
    let r = v.process_block(Arc::clone(block), true, None, true).recv_timeout(PATIENCE);
    assert!(r.expect("V replied").expect("V serves").state.is_valid());
}

/// [`alone_before`] at the run's time, then out of IBD and the blockchain synced.
fn synced_alone(dir: &Path, run: &Run, upto: usize, spork: bool) -> Node {
    let node = alone_before(dir, run, upto, spork, run.latest_time() + 60);
    until_out_of_ibd(&node);
    node.mn_sync().expect("constructed").switch_to_next_asset();
    node
}

/// The node alone on the run's chain up to the block before `lock.at`, out of IBD, SPORK_19 on
/// and the blockchain synced, with [`After`] registered and the shutdown probe on `events`.
fn synced_before(dir: &Path, run: &Run, lock: &Lock, events: &Events) -> Node {
    let mut node = synced_alone(dir, run, usize::try_from(lock.at).unwrap(), true);
    let v = node.chain().expect("V runs").handle().clone();
    let signals = node.chain().expect("V runs").signals();
    signals.register(Arc::new(After {
        v,
        events: Arc::clone(events),
    }));
    let probe = Arc::clone(events);
    node.set_probe(Box::new(move |point| push(&probe, format!("{point:?}"))));
    let drain = Arc::clone(events);
    node.chain().expect("V runs").set_drain_hook(Box::new(move || push(&drain, "Drain".into())));
    node
}

/// The handler's gate, closed at `at`: the first pass there reports on `reached` and waits for
/// `release`, then logs it.
fn close_gate(
    node: &Node,
    at: ClPoint,
    events: &Events,
) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (reached_tx, reached) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let released = std::sync::Mutex::new(released);
    let events = Arc::clone(events);
    let gate = node.chain().expect("V runs").cl_gate();
    *gate.0.lock().unwrap() = Some(Box::new(move |point| {
        if point == at && reached_tx.send(()).is_ok() {
            let _ = released.lock().unwrap().recv();
            push(&events, format!("released {point:?}"));
        }
    }));
    (reached, release)
}

/// Shutdown with the coinbase job of the block carrying the run's last CbTx lock held at `at`
/// (before `ClIntake`, or between the verification and `RecordChainLock`): shutdown waits with
/// V running; once released, the job ingests the lock (seen, recorded in V) and the event's
/// later listener runs, and only then V stops and the first flush runs. The block's
/// `UpdatedBlockTip`, queued behind the event, is not waited for: it runs after the event.
// Core: init.cpp:347-361; net_processing.cpp:2167-2169 at v24.0.0
fn shutdown_waits_for_a_held_coinbase_job(at: ClPoint) {
    const QUIET: Duration = Duration::from_millis(500);
    let run = Run::open();
    let lock = *run.cbtx_locks().last().expect("a CbTx lock");
    let dir = tempfile::tempdir().unwrap();
    let events = Events::default();
    let mut node = synced_before(dir.path(), &run, &lock, &events);
    let handler = node.net().and_then(super::Net::chainlocks).expect("masternode services");
    let hash = inv_hash(&lock.clsig(), &node);
    let v = node.chain().expect("V runs").handle().clone();
    let (reached, release) = close_gate(&node, at, &events);

    let carrier = Arc::clone(&run.blocks[usize::try_from(lock.at).unwrap()]);
    let r = v.process_block(carrier, true, None, true).recv_timeout(PATIENCE);
    assert!(r.expect("V replied").expect("V serves").state.is_valid());
    reached.recv_timeout(PATIENCE).expect("the coinbase job reached the gate");
    assert_eq!(v.view().cl.best_height(), -1, "nothing recorded yet");

    // Released only once shutdown has reached the drain: its probe point.
    let control = {
        let (events, v) = (Arc::clone(&events), v.clone());
        std::thread::spawn(move || {
            until(|| events.lock().unwrap().iter().any(|e| e == "Drain"));
            std::thread::sleep(QUIET);
            let now = events.lock().unwrap().clone();
            assert!(!now.iter().any(|e| e == "VStopped"), "V stopped before the job: {now:?}");
            assert_eq!(v.query(|_| ()).recv_timeout(PATIENCE), Some(Ok(())), "V serves");
            assert_eq!(v.view().cl.best_height(), -1, "the job is still held");
            push(&events, "checked".into());
            release.send(()).expect("the job waits");
        })
    };
    node.shutdown();
    control.join().expect("the control thread's checks");

    let events = events.lock().unwrap().clone();
    let height = i32::try_from(lock.height).unwrap();
    let order = [
        "checked".to_owned(),
        format!("released {at:?}"),
        format!("after {height}"),
        "VStopped".to_owned(),
        "Flush { aborted: false }".to_owned(),
    ];
    let at: Vec<usize> = order.iter().map(|e| position(&events, e)).collect();
    assert!(at.is_sorted(), "{events:?}");
    assert!(position(&events, &format!("tip {height}")) > at[2], "{events:?}");
    assert!(handler.already_have(&hash), "the lock's hash is in the seen map");
    assert_eq!(node.abort.reports(), 0);
    assert_eq!(node.exit_status(), 0);
}

#[test]
fn shutdown_waits_for_a_coinbase_job_held_before_intake() {
    shutdown_waits_for_a_held_coinbase_job(ClPoint::Intake);
}

#[test]
fn shutdown_waits_for_a_coinbase_job_held_between_verify_and_record() {
    shutdown_waits_for_a_held_coinbase_job(ClPoint::Record);
}

/// V stops on a `Fatal` while the coinbase job waits before `ClIntake`: the job completes
/// `Stopped`, the signals thread raises no second `AbortNode` and resumes the event, and
/// shutdown completes.
#[cfg(feature = "test-support")]
#[test]
fn a_fatal_v_stop_under_a_held_coinbase_job_resumes_and_shuts_down() {
    use dashd_chainstate::chain::Fatal;

    let run = Run::open();
    let lock = *run.cbtx_locks().last().expect("a CbTx lock");
    let dir = tempfile::tempdir().unwrap();
    let events = Events::default();
    let mut node = synced_before(dir.path(), &run, &lock, &events);
    let v = node.chain().expect("V runs").handle().clone();
    let signals = node.chain().expect("V runs").signals().clone();
    let (reached, release) = close_gate(&node, ClPoint::Intake, &events);
    let carrier = Arc::clone(&run.blocks[usize::try_from(lock.at).unwrap()]);
    let _ = v.process_block(carrier, true, None, true).recv_timeout(PATIENCE);
    reached.recv_timeout(PATIENCE).expect("the coinbase job reached the gate");

    let _ = v.query_or_stop(|_| Err::<(), _>(Fatal::new("injected"))).recv();
    until(|| signals.v_fatal());
    release.send(()).expect("the job waits");
    signals.sync(&mut dashd_chainstate::chain::Blocking).expect("the event resumed");
    node.shutdown();
    let events = events.lock().unwrap().clone();
    let at: Vec<usize> =
        ["after -1", "tip -1", "VStopped"].iter().map(|e| position(&events, e)).collect();
    assert!(at.is_sorted(), "{events:?}");
    assert_eq!(node.abort.reports(), 1, "the injected stop's alone");
}

/// m6 on the whole node: a `ListUnavailable` met by the wired verifier's LLMQ reads is a local
/// fault, never a peer's. The handler records nothing, scores nobody and relays nothing, and
/// raises the node's `AbortNode` once, so the node exits failing.
// Core: chainlock/chainlock.cpp:96-104, llmq/quorumsman.cpp:179-295 at v24.0.0
#[cfg(feature = "test-support")]
#[test]
fn a_list_unavailable_verify_aborts_the_node_once_and_scores_nobody() {
    let run = Run::open();
    let lock = *run.cbtx_locks().last().expect("a CbTx lock");
    let dir = tempfile::tempdir().unwrap();
    let events = Events::default();
    let mut node = synced_before(dir.path(), &run, &lock, &events);
    let mut handler = node.net().and_then(super::Net::chainlocks).expect("masternode services");
    let v = node.chain().expect("V runs").handle().clone();
    let clsig = lock.clsig();
    let hash = inv_hash(&clsig, &node);
    node.fail_route_b_lists();

    let read = dashd_p2p::processing::seams::Read {
        msg: clsig,
        read: v.view().bls,
    };
    let result = handler.process_new_chainlock(7, read, &hash);
    assert!(result.error.is_none(), "a local fault scores nobody");
    assert!(result.inventory.is_empty(), "nothing relayed");
    assert_eq!(v.view().cl.best_height(), -1, "nothing recorded");
    assert_eq!(node.abort.reports(), 1, "one AbortNode");
    let report = node.abort.report(0).expect("reported");
    assert!(report.log.contains("masternode list unavailable"), "{report:?}");
    assert!(v.shutdown_requested(), "the AbortNode requested the shutdown");
    node.shutdown();
    assert_eq!(node.exit_status(), crate::shutdown::EXIT_FAILURE);
}

/// The node alone on the run's chain through the block carrying `lock`, out of IBD and SPORK_19
/// on: that block connected while the blockchain was not synced, so nothing was learned, then
/// the blockchain synced.
fn carried_unsynced(dir: &Path, run: &Run, lock: &Lock) -> Node {
    synced_alone(dir, run, usize::try_from(lock.at).unwrap() + 1, true)
}

/// The coinbase route's job for the block carrying `lock`, on its own thread: the route over the
/// node's V, ChainLock handler and relay, with the node's gates over `params` and the blockchain
/// synced.
fn coinbase_job(
    node: &Node,
    params: &dash_params::ChainSetup,
    run: &Run,
    lock: &Lock,
) -> std::thread::JoinHandle<Outcome> {
    let handler = node.net().and_then(super::Net::chainlocks).expect("masternode services");
    coinbase_job_with(node, params, run, lock, handler)
}

/// [`coinbase_job`] with `handler` for the node's.
fn coinbase_job_with(
    node: &Node,
    params: &dash_params::ChainSetup,
    run: &Run,
    lock: &Lock,
    handler: SharedHandler,
) -> std::thread::JoinHandle<Outcome> {
    let v = node.chain().expect("V runs").handle().clone();
    let carrier = Arc::clone(&run.blocks[usize::try_from(lock.at).unwrap()]);
    let id = v.view_handle().lookup(&carrier.hash()).expect("connected");
    let route = crate::locks::CoinbaseRoute {
        v,
        gates: super::coinbase_gates(Box::new(|| true), node.sporks().expect("loaded"), params),
        handler,
        relay: crate::locks::SharedRelay(node.net_shared().expect("attached")),
    };
    let height = lock.at;
    std::thread::spawn(move || route.run(&carrier, id, height))
}

/// The coinbase route over a V that stops without a fatal error while the job waits at `at`
/// (before `ClIntake`, before the verifier's `OnV`, or before `RecordChainLock`): the lock is
/// skipped, and the job reports `Stopped`, never `Done`.
#[cfg(feature = "test-support")]
fn the_coinbase_route_reports_a_v_stopped_at(at: ClPoint) {
    let run = Run::open();
    let lock = *run.cbtx_locks().last().expect("a CbTx lock");
    let dir = tempfile::tempdir().unwrap();
    let mut node = carried_unsynced(dir.path(), &run, &lock);
    let v = node.chain().expect("V runs").handle().clone();
    let signals = node.chain().expect("V runs").signals().clone();
    let events = Events::default();
    let (reached, release) = close_gate(&node, at, &events);
    let job = coinbase_job(&node, &node.params().chain, &run, &lock);
    reached.recv_timeout(PATIENCE).expect("the job reached the gate");
    v.shutdown();
    until(|| v.query(|_| ()).recv().is_err());
    release.send(()).expect("the job waits");
    assert_eq!(job.join().expect("the job"), Outcome::Stopped, "{at:?}");
    assert!(!signals.v_fatal(), "V stopped without a fatal error");
    assert_eq!(v.view().cl.best_height(), -1, "nothing recorded");
    node.shutdown();
    assert_eq!(node.abort.reports(), 0);
}

#[cfg(feature = "test-support")]
#[test]
fn the_coinbase_route_reports_stopped_for_a_v_stopped_before_intake() {
    the_coinbase_route_reports_a_v_stopped_at(ClPoint::Intake);
}

#[cfg(feature = "test-support")]
#[test]
fn the_coinbase_route_reports_stopped_for_a_v_stopped_in_verification() {
    the_coinbase_route_reports_a_v_stopped_at(ClPoint::Verify);
}

#[cfg(feature = "test-support")]
#[test]
fn the_coinbase_route_reports_stopped_for_a_v_stopped_before_record() {
    the_coinbase_route_reports_a_v_stopped_at(ClPoint::Record);
}

/// A ChainLock handler over the node's V and abort, with the node's verifier verifying on `on`,
/// a second V that stands in for the node's at the verification only.
fn handler_verifying_on(node: &Node, on: ChainHandle) -> SharedHandler {
    let chain = node.chain().expect("V runs");
    let handler = node.net().and_then(super::Net::chainlocks).expect("masternode services");
    let verify = handler.lock().verifier().over(on);
    let cl_chain = NodeClChain::new(chain.handle().clone(), Arc::clone(chain.abort()));
    SharedHandler::new(ChainLockHandler::new(cl_chain, verify, Arc::new(MockClock::new()), || true))
}

/// The coinbase route whose verification V drops unrun while it serves on: the lock is skipped,
/// and the job ends `Done` with no abort. The verification runs on a second V whose facade has a
/// latch of its own, tripped while that V's store latch stays open, so the facade refuses route
/// B (D74) and that V answers the probe after it.
#[test]
fn a_coinbase_verify_dropped_while_v_serves_ends_done() {
    let run = Run::open();
    let lock = *run.cbtx_locks().last().expect("a CbTx lock");
    let dir = tempfile::tempdir().unwrap();
    let mut node = carried_unsynced(dir.path(), &run, &lock);
    let latch = dashd_chainstate::store::GlobalFailStop::new();
    let (mut other, other_abort) = crate::locks::v_with_facade_over(latch.clone());
    assert!(latch.trip(dash_kv::Failure {
        kind: dash_kv::ErrorKind::Other,
        site: "test",
        message: "tripped".to_owned(),
    }));
    assert_eq!(other.handle().on_v(|_, _| ()).recv(), Err(WaitError::Disconnected), "dropped");
    other.handle().query(|_| ()).recv().expect("the second V serves on");
    let handler = handler_verifying_on(&node, other.handle().clone());
    let job = coinbase_job_with(&node, &node.params().chain, &run, &lock, handler.clone());
    assert_eq!(job.join().expect("the job"), Outcome::Done);
    other.handle().query(|_| ()).recv().expect("the second V serves on");
    let v = node.chain().expect("V runs").handle().clone();
    assert_eq!(v.view().cl.best_height(), -1, "nothing recorded");
    assert!(handler.already_have(&inv_hash(&lock.clsig(), &node)), "seen");
    other.stop().expect("V stops");
    node.shutdown();
    assert_eq!(node.abort.reports(), 0);
    assert!(other_abort.messages().is_empty());
}

/// A P2P verification that found V stopped does not decide a later coinbase job's outcome: a
/// peer's clsig for the carrier's lock is verified first, on a V that has stopped, and the job
/// for the same lock then returns at the seen check and ends `Done`.
#[test]
fn a_p2p_verify_that_found_v_stopped_leaves_a_later_coinbase_job_done() {
    let run = Run::open();
    let lock = *run.cbtx_locks().last().expect("a CbTx lock");
    let dir = tempfile::tempdir().unwrap();
    let mut node = carried_unsynced(dir.path(), &run, &lock);
    let (mut other, _) = crate::locks::v_with_facade_over(Default::default());
    let stopped = other.handle().clone();
    other.stop().expect("V stops");
    let mut handler = handler_verifying_on(&node, stopped);
    let hash = inv_hash(&lock.clsig(), &node);
    let read = Read {
        msg: lock.clsig(),
        read: node.chain().expect("V runs").handle().view().bls,
    };
    let result = handler.process_new_chainlock(7, read, &hash);
    assert!(result.error.is_none() && result.inventory.is_empty(), "skipped, unscored");
    assert!(handler.lock().verifier().found_v_stopped(), "the P2P verify found V stopped");
    let job = coinbase_job_with(&node, &node.params().chain, &run, &lock, handler.clone());
    assert_eq!(job.join().expect("the job"), Outcome::Done);
    let v = node.chain().expect("V runs").handle().clone();
    assert_eq!(v.view().cl.best_height(), -1, "nothing recorded");
    node.shutdown();
    assert_eq!(node.abort.reports(), 0);
}

/// The coinbase route's V20 gate, through the node's wiring over the run's parameters with V20
/// (and MN_RR, which may not precede it) moved: with V20 active from the height after the
/// carrying block's, the lock is skipped; with V20 active from that block's own height, it is
/// recorded and seen.
// Core: chainlock/handler.cpp:106-110; deploymentstatus.h:34-38 at v24.0.0
#[test]
fn the_coinbase_route_runs_from_v20_s_activation_height() {
    let run = Run::open();
    let lock = *run.cbtx_locks().last().expect("a CbTx lock");
    let dir = tempfile::tempdir().unwrap();
    let mut node = carried_unsynced(dir.path(), &run, &lock);
    let v = node.chain().expect("V runs").handle().clone();
    let handler = node.net().and_then(super::Net::chainlocks).expect("masternode services");
    let hash = inv_hash(&lock.clsig(), &node);
    let moved = |v20: u32| {
        let mut argv = vec!["-regtest".to_owned()];
        argv.extend(run.args.iter().map(|a| match a.split_once('@') {
            Some((name, _)) if name.ends_with("=v20") || name.ends_with("=mn_rr") => {
                format!("{name}@{v20}")
            }
            _ => a.clone(),
        }));
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let args = crate::init::test_args(&argv);
        crate::init::params::select_params(&args, "regtest").expect("the run's parameters").chain
    };
    for (v20, learned) in [(lock.at + 1, false), (lock.at, true)] {
        let job = coinbase_job(&node, &moved(v20), &run, &lock);
        assert_eq!(job.join().expect("the job"), Outcome::Done, "v20@{v20}");
        let best = if learned {
            i32::try_from(lock.height).unwrap()
        } else {
            -1
        };
        assert_eq!(v.view().cl.best_height(), best, "v20@{v20}");
        assert_eq!(handler.already_have(&hash), learned, "v20@{v20}");
    }
    node.shutdown();
    assert_eq!(node.abort.reports(), 0);
}

/// With SPORK_19 off the coinbase route learns nothing: the block carrying a CbTx lock connects
/// with the blockchain synced, and the lock is neither recorded nor seen.
// Core: chainlock/handler.cpp:106-110 at v24.0.0
#[test]
fn with_spork_19_off_the_coinbase_route_learns_nothing() {
    let run = Run::open();
    let lock = *run.cbtx_locks().last().expect("a CbTx lock");
    let dir = tempfile::tempdir().unwrap();
    let upto = usize::try_from(lock.at).unwrap();
    let mut node = synced_alone(dir.path(), &run, upto, false);
    assert!(!node.sporks().expect("loaded").chainlocks_enabled());
    let v = node.chain().expect("V runs").handle().clone();
    let handler = node.net().and_then(super::Net::chainlocks).expect("masternode services");
    let hash = inv_hash(&lock.clsig(), &node);
    connect(&v, &run.blocks[upto]);
    let signals = node.chain().expect("V runs").signals();
    signals.sync(&mut dashd_chainstate::chain::Blocking).expect("the block's signals ran");
    assert_eq!(v.view().cl.best_height(), -1, "nothing recorded");
    assert!(!handler.already_have(&hash), "nothing seen");
    node.shutdown();
    assert_eq!(node.abort.reports(), 0);
    assert_eq!(node.exit_status(), 0);
}

/// A listener registered after the masternode sync's: per callback, V's best ChainLock height
/// and whether the blockchain was synced as it ran.
struct SyncSeen {
    v: dashd_chainstate::chain::ChainHandle,
    sync: Arc<crate::rpc::SyncSlot>,
    events: Events,
}

impl SyncSeen {
    fn note(&self, what: &str) {
        use dashd_rpc::dash::NodeSync as _;
        let synced = self.sync.mn_sync().expect("constructed").is_blockchain_synced();
        push(&self.events, format!("{what} {} {synced}", self.v.view().cl.best_height()));
    }
}

impl dashd_chainstate::chain::ValidationInterface for SyncSeen {
    fn block_connected(
        &self,
        _: &Arc<Block>,
        _: dash_consensus::BlockId,
    ) -> dashd_chainstate::chain::Cont {
        self.note("connected");
        dashd_chainstate::chain::Cont::Done
    }

    fn updated_block_tip(
        &self,
        _: dash_consensus::BlockId,
        _: Option<dash_consensus::BlockId>,
        _: bool,
    ) {
        self.note("tip");
    }
}

/// In IBD, with the blockchain synced and the masternode sync not finished, the block carrying a
/// CbTx lock: its coinbase job holds the block's `BlockConnected`, so the job samples
/// `IsBlockchainSynced()` and learns the lock before the block's `UpdatedBlockTip` resets the
/// masternode sync.
// Core: net_processing.cpp:2167-2169; chainlock/handler.cpp:106-110;
// dsnotificationinterface.cpp:60; masternode/sync.cpp:121-153 at v24.0.0
#[test]
fn the_coinbase_job_samples_the_sync_before_the_block_s_tip_resets_it() {
    let run = Run::open();
    let lock = *run.cbtx_locks().last().expect("a CbTx lock");
    let dir = tempfile::tempdir().unwrap();
    let upto = usize::try_from(lock.at).unwrap();
    let now = run.latest_time() + 60 + 2 * 86_400;
    let mut node = alone_before(dir.path(), &run, upto, true, now);
    let v = node.chain().expect("V runs").handle().clone();
    let view = v.view_handle();
    assert!(view.is_initial_block_download(&view.view().snap, node.clock.node_time()));
    let sync = node.mn_sync().expect("constructed");
    sync.switch_to_next_asset();
    assert!(sync.is_blockchain_synced() && !sync.is_synced());
    let events = Events::default();
    let signals = node.chain().expect("V runs").signals();
    signals.register(Arc::new(SyncSeen {
        v: v.clone(),
        sync: node.sync_slot(),
        events: Arc::clone(&events),
    }));

    connect(&v, &run.blocks[upto]);
    signals.sync(&mut dashd_chainstate::chain::Blocking).expect("the block's signals ran");
    let height = lock.height;
    assert_eq!(
        *events.lock().unwrap(),
        [format!("connected {height} true"), format!("tip {height} false")]
    );
    assert!(!sync.is_blockchain_synced(), "reset by the block's tip");
    node.shutdown();
    assert_eq!(node.abort.reports(), 0);
}

/// How long a held startup ABC's replay, and the test around it, may take before it counts as
/// hung.
#[cfg(feature = "test-support")]
const HOLD_BOUND: Duration = Duration::from_secs(600);

/// `f` on its own thread; the test fails if it panics or is still running after `HOLD_BOUND`.
#[cfg(feature = "test-support")]
fn bounded(what: &str, f: impl FnOnce() + Send + 'static) {
    let (done, finished) = std::sync::mpsc::channel();
    let run = std::thread::spawn(move || {
        f();
        let _ = done.send(());
    });
    match finished.recv_timeout(HOLD_BOUND) {
        Ok(()) => run.join().expect("the bounded run"),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("hung: {what}"),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            std::panic::resume_unwind(run.join().expect_err("the run panicked"));
        }
    }
}

/// The run's arguments for a node alone at `now`, with the spork key.
fn alone_argv(run: &Run, now: i64) -> Vec<String> {
    let mut argv = run.args.clone();
    argv.extend(
        [SPORK_ARG, "-assumevalid=0", "-dnsseed=0", "-fixedseeds=0", "-listen=0"]
            .map(str::to_owned),
    );
    argv.push(format!("-mocktime={now}"));
    argv
}

/// A first start stores the run's blocks before `upto`, with SPORK_19 on, and shuts down.
#[cfg(feature = "test-support")]
fn stored(dir: &Path, run: &Run, upto: usize, now: i64) {
    let mut node = alone_before(dir, run, upto, true, now);
    node.shutdown();
    assert_eq!(node.exit_status(), 0);
}

/// The run's first two CbTx locks, and where the held replay ends: before the block carrying the
/// second. Each block connected queues two entries (`BlockConnected`, `UpdatedBlockTip`), more
/// than 11 blocks follow the first lock's in the replay, and Core's ABC waits for the queue to
/// come within 10 entries before each step, so the first lock's `BlockConnected` is dispatched
/// before the last block connects, while V still holds. Every later block's CbTx carries that
/// lock too.
// Core: validation.cpp:3727-3733, :3771 at v24.0.0
#[cfg(feature = "test-support")]
fn held_replay_end(run: &Run) -> (Lock, Lock, usize) {
    let locks = run.cbtx_locks();
    let (first, next) = (locks[0], locks[1]);
    assert!(next.at - first.at > 11, "{first:?}, {next:?}: too near the replay's end");
    (first, next, usize::try_from(next.at).unwrap())
}

/// Per `BlockConnected`, once the listeners before it (the P2P listener's coinbase route among
/// them) are done with it: the height, whether V still held commands for startup's ABC, and V's
/// best ChainLock height.
#[cfg(feature = "test-support")]
type Watched = Arc<std::sync::Mutex<Vec<(u32, bool, i32)>>>;

/// The listener behind [`Watched`].
#[cfg(feature = "test-support")]
struct Watch {
    v: dashd_chainstate::chain::ChainHandle,
    watched: Watched,
}

#[cfg(feature = "test-support")]
impl dashd_chainstate::chain::ValidationInterface for Watch {
    fn block_connected(
        &self,
        _: &Arc<Block>,
        id: dash_consensus::BlockId,
    ) -> dashd_chainstate::chain::Cont {
        let height = self.v.view_handle().entry(id).map_or(u32::MAX, |e| e.height);
        let held = self.v.holds_for_startup_abc();
        let best = self.v.view().cl.best_height();
        self.watched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((height, held, best));
        dashd_chainstate::chain::Cont::Done
    }
}

/// A restart over [`stored`]'s blocks with `-reindex-chainstate` and `extra`, V holding
/// commands for startup's ABC (D38), which replays them with SPORK_19 on, the masternode sync
/// finished and [`Watch`] registered before that ABC is sent, after the P2P listener: back once
/// that ABC has replayed them all, the hold has ended and the signals queued meanwhile have run.
#[cfg(feature = "test-support")]
fn replayed_under_the_hold(
    dir: &Path,
    run: &Run,
    upto: usize,
    now: i64,
    extra: &[&str],
) -> (Node, Watched) {
    let mut argv = alone_argv(run, now);
    argv.push("-reindex-chainstate".into());
    argv.extend(extra.iter().map(|&a| a.to_owned()));
    let watched = Watched::default();
    let watch = Arc::clone(&watched);
    let node = node_with(dir, &argv.iter().map(String::as_str).collect::<Vec<_>>(), |node| {
        node.hold_startup_abc(Box::new(move |chain, sync| {
            assert!(chain.handle().holds_for_startup_abc(), "V holds for startup's ABC");
            while !sync.is_synced() {
                sync.switch_to_next_asset();
            }
            chain.signals().register(Arc::new(Watch {
                v: chain.handle().clone(),
                watched: watch,
            }));
        }));
    });
    assert!(node.sporks().expect("loaded").chainlocks_enabled(), "SPORK_19 kept");
    let chain = node.chain().expect("V runs");
    let v = chain.handle().clone();
    let last = u32::try_from(upto - 1).unwrap();
    while v.holds_for_startup_abc() || v.view_handle().view().snap.tip.tip_height != last {
        assert_eq!(node.abort.reports(), 0, "an AbortNode in the replay");
        std::thread::sleep(Duration::from_millis(20));
    }
    chain.signals().sync(&mut dashd_chainstate::chain::Blocking).expect("the replay's signals ran");
    (node, watched)
}

/// DEC-1811: the block carrying the run's first CbTx ChainLock connected by startup's ABC while
/// V holds commands for it (D38), with the blockchain synced and SPORK_19 on. Its
/// `BlockConnected` is dispatched under the hold, so the P2P listener posts no coinbase job:
/// nothing is learned under the hold, the replay completes, and shutdown completes cleanly. Core
/// runs `ProcessCoinbaseChainLock` there.
// Core: net_processing.cpp:2167-2169; chainlock/handler.cpp:104-110 at v24.0.0
#[cfg(feature = "test-support")]
#[test]
fn a_coinbase_lock_connected_under_the_startup_hold_blocks_neither_limit_queue_nor_shutdown() {
    bounded("the held startup ABC's replay or the shutdown after it", || {
        let run = Run::open();
        let (first, _, upto) = held_replay_end(&run);
        let now = run.latest_time() + 60;
        let dir = tempfile::tempdir().unwrap();
        stored(dir.path(), &run, upto, now);
        let (mut node, watched) = replayed_under_the_hold(dir.path(), &run, upto, now, &[]);
        let watched = watched.lock().unwrap().clone();
        let carrier = watched.iter().find(|w| w.0 == first.at).expect("the lock's block");
        assert!(carrier.1, "dispatched under the hold: {watched:?}");
        assert!(watched.iter().filter(|w| w.1).all(|w| w.2 == -1), "learned: {watched:?}");
        node.shutdown();
        assert_eq!(node.abort.reports(), 0);
        assert_eq!(node.exit_status(), 0);
    });
}

/// DEC-1811: after the held startup ABC, the first block carrying a new CbTx ChainLock goes
/// through the coinbase route as before: recorded as the best lock, seen, and relayed to the
/// peer.
// Core: net_processing.cpp:2167-2169; chainlock/handler.cpp:104-183 at v24.0.0
#[cfg(feature = "test-support")]
#[test]
fn the_first_coinbase_lock_after_the_startup_hold_is_processed() {
    bounded("the held startup ABC's replay or the lock after it", || {
        let run = Run::open();
        let (_, next, upto) = held_replay_end(&run);
        let now = run.latest_time() + 60;
        let dir = tempfile::tempdir().unwrap();
        stored(dir.path(), &run, upto, now);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let peer = format!("-connect={}", listener.local_addr().unwrap());
        let extra = [peer.as_str(), "-v2transport=0"];
        let (mut node, _) = replayed_under_the_hold(dir.path(), &run, upto, now, &extra);
        let mut a = accept(&node, &listener, now);
        let v = node.chain().expect("V runs").handle().clone();
        let height = i32::try_from(next.height).unwrap();
        assert!(v.view().cl.best_height() < height, "the new lock not known yet");
        until_out_of_ibd(&node);

        let hash = inv_hash(&next.clsig(), &node);
        connect(&v, &run.blocks[upto]);
        until(|| v.view().cl.best_height() == height);
        let handler = node.net().and_then(super::Net::chainlocks).expect("masternode services");
        assert!(handler.already_have(&hash), "the lock's hash is in the seen map");
        a.until("the coinbase lock's inv", |w| w.announced(hash) > 0);
        node.shutdown();
        assert_eq!(node.abort.reports(), 0);
        assert_eq!(node.exit_status(), 0);
    });
}
