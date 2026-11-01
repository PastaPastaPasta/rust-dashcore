//! P3's real-V peer cases (D122): S4-v, the subset of S4-x through the node's own V, adapter and
//! listener (P3 §6.3, D48, D102 (4)); the window inside V's hold; and the cell against S (DEC-236,
//! D44). Scripted V1 peers listen on loopback and the gated network of [`Rig`] dials them
//! (`-connect`). A [`Pauser`], registered on V's signals after P3's listener, stops V where Core
//! has no lock to show: right after `NewPoWValidBlock` (before the write), at ABC's first release
//! point (before the first `ConnectTip`, serving V's commands as C5's wait does), and inside the
//! hold at `SynchronousUpdatedBlockTip`. The two-cell window itself, between SB4's snapshot
//! store (`Hold::release`) and the `ChainView` store (`publish_view`), is held open through CH1's
//! `test-support` hook (`ChainHandle::set_between_stores`, D126).

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use dash_chain::Status;
use dash_clock::Clock as _;
use dash_consensus::primitives::Header;
use dash_consensus::{Block, BlockId};
use dash_p2p::{CommandString, PeerId};
use dash_p2p_core::{GetHeaders, Headers, Inv, InvType, ProtocolVersion, SendCmpct};
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec as _;
use dashd_chainstate::chain::{
    NewBlock, Queued, ServiceWait, ValidationInterface, ValidationSignals, WaitError, genesis_block,
};
use dashd_p2p::processing::peerman::LockPoint;
use dashd_p2p::processing::sync::block_hash;

use super::amb::{Peer, branch, headers_msg, regtest};
use super::chain::lock;
use super::realv::{PATIENCE, Rig, new_block, on_actor, until};

/// How long a check waits for something that must not happen.
const QUIET: Duration = Duration::from_millis(300);

type Frames = Vec<(CommandString, Vec<u8>)>;

/// One scripted peer: its end of the connection, every frame the node sent it, and its id in
/// the node's table.
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

    fn send_block(&mut self, block: &Block) {
        self.send("block", &dash_consensus::core_serialize(block));
    }

    fn pump(&mut self) {
        let frames = self.peer.poll();
        self.log.extend(frames);
    }

    /// Reads until `f` holds on the log.
    fn until(&mut self, what: &str, f: impl Fn(&Frames) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            self.pump();
            if f(&self.log) {
                return;
            }
            assert!(Instant::now() < deadline, "peer={}: no {what}", self.id);
        }
    }

    /// Two `ping` round trips: the actor has handled every frame sent before, and the pass that
    /// answered the first has run its `SendMessages`, whose frames precede the second `pong`.
    fn sync(&mut self) {
        for _ in 0..2 {
            self.nonce += 1;
            let nonce = self.nonce.to_le_bytes();
            self.send("ping", &nonce);
            self.until("pong", |log| log.iter().any(|(c, p)| c.as_str() == "pong" && *p == nonce));
        }
    }

    /// The `headers` and `inv` frames naming `hash`.
    fn announced(&self, hash: BlockHash) -> (usize, usize) {
        let frames = |cmd: &'static str| self.log.iter().filter(move |(c, _)| c.as_str() == cmd);
        let headers = frames("headers").filter(|(_, p)| header_hashes(p).contains(&hash)).count();
        let inv = frames("inv")
            .filter(|(_, p)| {
                Inv::decode(&mut &p[..])
                    .expect("an inv")
                    .inventory
                    .iter()
                    .any(|i| i.inv_type == InvType::Block && block_hash(i) == hash)
            })
            .count();
        (headers, inv)
    }

    /// `getheaders` from `tip`, answered with no headers: the node's `best_header_sent` is the
    /// tip, so a block on it passes `PeerHasHeader(pprev)` (np:4842).
    fn knows(&mut self, tip: BlockHash) {
        let mut buf = Vec::new();
        GetHeaders {
            version: ProtocolVersion(70242),
            locator_hashes: vec![tip],
            hash_stop: BlockHash::from_lendian([0; 32]),
        }
        .encode(&mut buf);
        let before = count(&self.log, "headers");
        self.send("getheaders", &buf);
        self.until("headers", |log| count(log, "headers") > before);
    }

    /// The last `headers` frame's hashes.
    fn last_headers(&self) -> Vec<BlockHash> {
        let (_, p) = self.log.iter().rev().find(|(c, _)| c.as_str() == "headers").expect("headers");
        header_hashes(p)
    }

    /// `sendcmpct(high_bandwidth, 1)`.
    fn sendcmpct(&mut self, high_bandwidth: bool) {
        let mut buf = Vec::new();
        SendCmpct {
            high_bandwidth,
            version: 1,
        }
        .encode(&mut buf);
        self.send("sendcmpct", &buf);
    }

    fn clear(&mut self) {
        self.pump();
        self.log.clear();
    }
}

/// The `cmpctblock` frames for `hash` (its header is the first 80 bytes).
fn cmpcts(log: &Frames, hash: BlockHash) -> usize {
    let header = |p: &[u8]| BlockHeader::decode(&mut &p[..80]).expect("a header");
    log.iter()
        .filter(|(c, p)| c.as_str() == "cmpctblock" && Header::new(header(p)).hash() == hash)
        .count()
}

fn count(log: &Frames, cmd: &str) -> usize {
    log.iter().filter(|(c, _)| c.as_str() == cmd).count()
}

/// A `headers` payload's hashes.
fn header_hashes(payload: &[u8]) -> Vec<BlockHash> {
    let headers = Headers::decode(&mut &payload[..]).expect("a headers").headers;
    headers.into_iter().map(|h| Header::new(h).hash()).collect()
}

/// Parks the actor once, at a lock hook, or V, between its two stores, until the test lets it go.
struct Latch {
    armed: AtomicBool,
    entered: (Sender<()>, Receiver<()>),
    go: (Sender<()>, Receiver<()>),
}

impl Latch {
    fn new(armed: bool) -> Arc<Latch> {
        Arc::new(Latch {
            armed: AtomicBool::new(armed),
            entered: crossbeam_channel::bounded(1),
            go: crossbeam_channel::bounded(1),
        })
    }

    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    /// From a lock hook: parks the actor the first time it gets here. Returns whether it parked.
    fn park(&self) -> bool {
        on_actor() && self.hold()
    }

    /// Parks the calling thread the first time any thread gets here.
    fn hold(&self) -> bool {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return false;
        }
        self.entered.0.send(()).unwrap();
        self.go.1.recv_timeout(PATIENCE).expect("released");
        true
    }

    /// The test waits until the actor is parked.
    fn entered(&self) {
        self.entered.1.recv_timeout(PATIENCE).expect("the actor parks");
    }

    fn release(&self) {
        self.go.0.send(()).unwrap();
    }
}

/// Where V stops for a [`Pauser`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum At {
    /// Right after `NewPoWValidBlock`, before the write (v:4831-4832).
    Announce,
    /// ABC's first release point after that accept, before its first `ConnectTip` (v:3760),
    /// serving V's class R and M commands meanwhile.
    Connect,
    /// Inside the hold that connected the block, at `SynchronousUpdatedBlockTip` (v:3819-3824):
    /// before SB4's snapshot store and the `ChainView` store.
    InHold,
}

#[derive(Default)]
struct PauseState {
    /// The block whose accept, connect and hold pause, and its id once accepted.
    target: Option<BlockHash>,
    target_id: Option<BlockId>,
    /// The points V stops at; the others pass.
    armed: Vec<At>,
    /// Where V waits now.
    at: Option<At>,
    /// Points the test let V pass.
    open: Vec<At>,
    /// Set by the accept's `NewPoWValidBlock`: the next release point is ABC's first.
    accepted: bool,
}

/// A listener after P3's that stops V at the [`At`] points for one block.
#[derive(Default)]
struct Pauser {
    state: Mutex<PauseState>,
    cond: Condvar,
    /// The connect pause's wake (D67 N3: a slot of one, read only by V).
    ring: Mutex<Option<Sender<()>>>,
}

impl Pauser {
    fn arm(&self, target: BlockHash, at: &[At]) {
        let mut st = lock(&self.state);
        *st = PauseState {
            target: Some(target),
            armed: at.to_vec(),
            ..PauseState::default()
        };
    }

    /// V blocks here until the test opens `at`.
    fn pause(&self, at: At) {
        let mut st = lock(&self.state);
        st.at = Some(at);
        self.cond.notify_all();
        while !st.open.contains(&at) {
            st = self.cond.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
        st.at = None;
    }

    /// The test waits until V stops at `at`.
    fn reached(&self, at: At) {
        let deadline = Instant::now() + PATIENCE;
        let mut st = lock(&self.state);
        while st.at != Some(at) {
            let (s, _) = self
                .cond
                .wait_timeout(st, Duration::from_millis(50))
                .unwrap_or_else(PoisonError::into_inner);
            st = s;
            assert!(Instant::now() < deadline, "V never reached {at:?}");
        }
    }

    fn at(&self) -> Option<At> {
        lock(&self.state).at
    }

    /// Lets V pass `at`, now or when it gets there.
    fn open(&self, at: At) {
        lock(&self.state).open.push(at);
        self.cond.notify_all();
        if let Some(ring) = &*lock(&self.ring) {
            let _ = ring.try_send(());
        }
    }

    fn open_all(&self) {
        for at in [At::Announce, At::Connect, At::InHold] {
            self.open(at);
        }
    }
}

impl ValidationInterface for Pauser {
    fn new_pow_valid_block(&self, nb: &NewBlock<'_>) {
        let announce = {
            let mut st = lock(&self.state);
            if st.target != Some(nb.entry.hash) {
                return;
            }
            st.target_id = Some(nb.id);
            st.accepted = true;
            st.armed.contains(&At::Announce)
        };
        if announce {
            self.pause(At::Announce);
        }
    }

    /// The first release point after the target's accept is ABC's, before its first
    /// `ConnectTip`: the cases submit B with V idle, so no other block's ABC runs between them.
    fn release_point(&self, wait: &mut dyn ServiceWait) -> Result<(), WaitError> {
        {
            let mut st = lock(&self.state);
            if !std::mem::take(&mut st.accepted) || !st.armed.contains(&At::Connect) {
                return Ok(());
            }
            st.at = Some(At::Connect);
        }
        self.cond.notify_all();
        let (ring, wake): (Sender<()>, Receiver<()>) = crossbeam_channel::bounded(1);
        *lock(&self.ring) = Some(ring);
        let r = wait.wait_until(&wake, &mut || lock(&self.state).open.contains(&At::Connect));
        *lock(&self.ring) = None;
        lock(&self.state).at = None;
        r
    }

    fn synchronous_updated_block_tip(&self, new: BlockId, _: Option<BlockId>, _: bool) {
        let hit = {
            let st = lock(&self.state);
            st.armed.contains(&At::InHold) && st.target_id == Some(new)
        };
        if hit {
            self.pause(At::InHold);
        }
    }
}

/// The node, its scripted peers (by index), and the chain they share.
struct Net4 {
    rig: Rig,
    wires: Vec<Wire>,
    pauser: Arc<Pauser>,
    signals: ValidationSignals,
    /// The blocks up to A, the tip every case starts from.
    chain: Vec<Arc<Block>>,
    /// The case's block on A.
    b: Arc<Block>,
    /// A sibling of B on A, for the actor's block wait.
    y: Arc<Block>,
    /// The node's `GetTime()`.
    now: i64,
}

/// A header preference (S4-x's: `sendheaders2` needs `NODE_HEADERS_COMPRESSED`, which the
/// scripted peers do not offer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pref {
    Neither,
    Headers,
}

impl Net4 {
    /// `n` peers; those at the indices in `hb` ask for high-bandwidth compact blocks. Every
    /// peer knows A (`getheaders` from it) and has `pref`.
    fn new(dir: &Path, n: usize, hb: &[usize], pref: Pref) -> Net4 {
        let listeners: Vec<_> =
            (0..n).map(|_| TcpListener::bind("127.0.0.1:0").expect("bind")).collect();
        let addrs: Vec<SocketAddr> =
            listeners.iter().map(|l| l.local_addr().expect("addr")).collect();
        let rig = Rig::with(dir, &addrs);
        let p = regtest();
        let mut blocks = branch(&p, *genesis_block(&p).header(), 0, &[1; 4], 0x41);
        let y = branch(&p, *blocks[2].header(), 3, &[1], 0x42).remove(0);
        let b = blocks.pop().expect("B");
        // Out of IBD from the first block: the tip is minutes old on the node's clock.
        let now = i64::from(b.header().raw().time) + 60;
        rig.node.clock.set_mocktime(now);
        let peers: Vec<_> = listeners.iter().map(|l| Peer::dialled(l, &p, 0, now)).collect();
        let core = Arc::clone(rig.net.as_ref().expect("started").core());
        until(|| core.peers().iter().filter(|p| p.successfully_connected()).count() == n);
        let table = core.peers();
        let mut wires: Vec<Wire> = peers
            .into_iter()
            .zip(&addrs)
            .map(|(peer, a)| Wire {
                peer,
                id: table.iter().find(|p| p.addr().service.port == a.port()).expect("dialled").id(),
                log: Vec::new(),
                nonce: 0,
            })
            .collect();
        for block in &blocks {
            assert!(new_block(&rig.submit(block)));
        }
        let view = rig.v.view_handle();
        until(|| {
            rig.served();
            !view.is_initial_block_download(&view.view().snap, rig.node.clock.node_time())
        });
        let a = blocks.last().expect("A").hash();
        for (i, w) in wires.iter_mut().enumerate() {
            if pref == Pref::Headers {
                w.send("sendheaders", &[]);
            }
            w.knows(a);
            if hb.contains(&i) {
                w.sendcmpct(true);
            }
            w.sync();
            w.clear();
        }
        until(|| rig.queue().held().0 == 0);
        let signals = rig.node.chain().expect("V runs").signals().clone();
        // Every setup block's queued `UpdatedBlockTip` has appended and called the relay hook
        // before a case installs its own (BL-321).
        delivered(&signals);
        let pauser = Arc::new(Pauser::default());
        signals.register(Arc::clone(&pauser) as Arc<dyn ValidationInterface>);
        Net4 {
            rig,
            wires,
            pauser,
            signals,
            chain: blocks,
            b,
            y,
            now,
        }
    }

    fn a(&self) -> BlockHash {
        self.chain.last().expect("A").hash()
    }

    /// The published view's tip.
    fn tip(&self) -> BlockHash {
        self.rig.v.view_handle().view().snap.tip.tip_hash
    }

    /// `block`'s status at the published view: `None` before the view has its entry.
    fn status(&self, block: &Block) -> Option<Status> {
        let view = self.rig.v.view_handle();
        let id = view.lookup(&block.hash())?;
        view.status(&view.view(), id).expect("the newest view")
    }

    fn written(&self, block: &Block) -> bool {
        self.status(block).is_some_and(|s| s.has(Status::HAVE_DATA))
    }
}

/// Core's `SyncWithValidationInterfaceQueue` from the test thread: every signal queued so far has
/// been delivered.
fn delivered(signals: &ValidationSignals) {
    let (tx, rx) = crossbeam_channel::bounded(1);
    signals.enqueue(Queued::Barrier(tx));
    rx.recv_timeout(PATIENCE).expect("the signals thread reaches the barrier");
}

impl Drop for Net4 {
    fn drop(&mut self) {
        self.rig.shared.signals().set_lock_hook(None);
        self.rig.shared.signals().set_relay_hook(None);
        self.pauser.open_all();
        self.signals.unregister(&(Arc::clone(&self.pauser) as Arc<dyn ValidationInterface>));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    Rpc,
    P2p,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Actor {
    /// No message in progress.
    Idle,
    /// The actor waits on V for another block, which V defers behind B's connect.
    BlockWait,
    /// The actor holds a peer's header cell in a region when B's visit starts.
    Region,
}

/// S4-v (6), accept then connect (F1), through real V; with the P2P origin, also the timing
/// probe's ordering gate (§6.3 item 6, D52): at the pause after `NewPoWValidBlock` (before the
/// write) and again at ABC's first release point (B accepted, not connected), every HB peer has
/// already received `cmpctblock(B)`, and the call waited for any region in progress. Real V
/// runs no P2P block inline, so a P2P block always has the actor in its block wait for B; the
/// RPC block's block wait is for a sibling the actor submits at B's connect pause, which V
/// defers behind B (C5′), so at the announce pause it is the idle case.
// Core: validation.cpp:3760, :4831-4832, net_processing.cpp:2228-2268
#[test]
fn s4v6_accept_then_connect() {
    for pref in [Pref::Neither, Pref::Headers] {
        for (origin, actor) in [
            (Origin::Rpc, Actor::Idle),
            (Origin::Rpc, Actor::BlockWait),
            (Origin::Rpc, Actor::Region),
            (Origin::P2p, Actor::BlockWait),
        ] {
            s4v6_case(pref, origin, actor);
        }
    }
}

fn s4v6_case(pref: Pref, origin: Origin, actor: Actor) {
    let case = format!("{pref:?} {origin:?} {actor:?}");
    let dir = tempfile::tempdir().unwrap();
    // Peers 0 and 2 ask for HB compact blocks, 1 does not, and 3 is the P2P source.
    let (hb, x) = ([0, 2], 3);
    let mut n = Net4::new(dir.path(), 4, &hb, pref);
    let (block, tip) = (Arc::clone(&n.b), n.a());
    n.pauser.arm(block.hash(), &[At::Announce, At::Connect]);

    let visits = Arc::new(Mutex::new(Vec::new()));
    let region = Arc::new(AtomicBool::new(false));
    let latch = Latch::new(actor == Actor::Region);
    {
        let (visits, region, latch) =
            (Arc::clone(&visits), Arc::clone(&region), Arc::clone(&latch));
        n.rig.shared.signals().set_lock_hook(Some(Arc::new(move |at| match at {
            LockPoint::Visit(p) => lock(&visits).push(p),
            LockPoint::Locked if latch.park() => region.store(true, Ordering::SeqCst),
            _ => {}
        })));
    }
    let rpc = match (origin, actor) {
        (Origin::Rpc, Actor::Region) => {
            latch.entered();
            let rpc = n.rig.submit(&block);
            // The visit pushes to each HB peer before the region's peer, then waits on that
            // peer's cell until the region ends; V cannot reach the announce pause meanwhile.
            let deadline = Instant::now() + PATIENCE;
            let mut seen = 0;
            loop {
                std::thread::sleep(QUIET);
                let now = lock(&visits).len();
                if now > 0 && now == seen {
                    break;
                }
                assert!(Instant::now() < deadline, "{case}: the visit never settled");
                seen = now;
            }
            assert_eq!(n.pauser.at(), None, "{case}: the visit waited for the region");
            let visited = lock(&visits).clone();
            let held = *visited.last().expect("a visit");
            for &i in &hb {
                if visited[..visited.len() - 1].contains(&n.wires[i].id) {
                    n.wires[i].until(&format!("{case}: cmpctblock(B) before peer={held}"), |log| {
                        cmpcts(log, block.hash()) > 0
                    });
                }
            }
            latch.release();
            Some(rpc)
        }
        (Origin::Rpc, _) => Some(n.rig.submit(&block)),
        (Origin::P2p, _) => {
            n.wires[x].send_block(&block);
            None
        }
    };

    for at in [At::Announce, At::Connect] {
        n.pauser.reached(at);
        if actor == Actor::Region {
            assert!(region.load(Ordering::SeqCst), "{case} {at:?}: the call waited for the region");
        }
        if at == At::Connect && actor == Actor::BlockWait && origin == Origin::Rpc {
            // x's sibling y: the actor submits it and waits; V defers it behind B's connect.
            let y = Arc::clone(&n.y);
            n.wires[x].send_block(&y);
            until(|| n.rig.gate.blocks() >= 1);
        }
        for &i in &hb {
            n.wires[i].until(&format!("{case} {at:?}: cmpctblock(B)"), |log| {
                cmpcts(log, block.hash()) > 0
            });
        }
        assert_eq!(n.tip(), tip, "{case} {at:?}: B is not connected");
        assert_eq!(n.written(&block), at == At::Connect, "{case} {at:?}: B's write");
        n.pauser.open(at);
    }
    if let Some(rpc) = rpc {
        assert!(new_block(&rpc), "{case}");
    }
    until(|| n.tip() == block.hash());
    for w in &mut n.wires {
        w.sync();
    }
    for (i, w) in n.wires.iter().enumerate() {
        let want = usize::from(hb.contains(&i));
        assert_eq!(cmpcts(&w.log, block.hash()), want, "{case}: peer {i}'s cmpctblock(B)");
    }
}

/// S4-v (7)'s `headers` branch through real V (D102 (4)): it needs a release point between B's
/// write and its connect, which `SimChain` lacks. HB peer Q, at best-known A, sends
/// `headers(B)`. The actor's message start finds B unknown, and the actor waits at the gate
/// before its `ProcessHeaders` reaches V, while RPC `submitblock B` is accepted; B's visit pushes
/// `cmpctblock(B)` to Q. V then serves the batch at B's connect pause (accepted, not connected),
/// or after the connect. Accepted, not connected: Q has `cmpctblock(B)` and the announcement
/// time is set; connected: `cmpctblock(B)` and no announcement time (np:3196-3199).
#[test]
fn s4v7_headers_racing_an_rpc_block() {
    for pref in [Pref::Neither, Pref::Headers] {
        for connected in [false, true] {
            let case = format!("{pref:?} connected={connected}");
            let dir = tempfile::tempdir().unwrap();
            let q = 0;
            let mut n = Net4::new(dir.path(), 2, &[q], pref);
            let block = Arc::clone(&n.b);
            n.pauser.arm(block.hash(), &[At::Connect]);
            n.rig.gate.set(|g| g.headers_shut = true);
            n.wires[q].send("headers", &headers_msg([&block]));
            until(|| n.rig.gate.holds_at() == Some("process_headers"));
            let rpc = n.rig.submit(&block);
            n.pauser.reached(At::Connect);
            n.wires[q]
                .until(&format!("{case}: cmpctblock(B)"), |log| cmpcts(log, block.hash()) > 0);
            if connected {
                n.pauser.open(At::Connect);
                until(|| n.tip() == block.hash());
            } else {
                assert!(n.written(&block) && n.tip() == n.a(), "{case}: accepted, not connected");
            }
            n.rig.gate.set(|g| g.headers_shut = false);
            // The continuation has run once the actor answers Q again.
            n.wires[q].sync();
            let id = n.wires[q].id;
            let announced =
                n.rig.shared.with_state(id, |st| st.last_block_announcement).expect("Q");
            if !connected {
                n.pauser.open(At::Connect);
            }
            assert!(new_block(&rpc), "{case}");
            until(|| n.tip() == block.hash());
            n.wires[q].sync();
            assert_eq!(cmpcts(&n.wires[q].log, block.hash()), 1, "{case}");
            let want = if connected {
                0
            } else {
                n.now
            };
            assert_eq!(announced, want, "{case}: announcement time");
        }
    }
}

/// The window inside V's hold (D122 (3)): V has connected B and pushed `BlockChecked(B)` with the
/// stamp of the view its release will publish, and has stored neither SB4's snapshot nor the
/// `ChainView`. A drain there shows tip A and leaves `BlockChecked(B)` queued, and a source P's
/// `getheaders` is answered from A; after the release, B is shown, applied and served (D47).
// Core: validation.cpp:3471, :3761, :3819-3824, net_processing.cpp:3202-3205
#[test]
fn inside_the_hold_a_drain_shows_neither_the_new_tip_nor_applies_its_block_checked() {
    let dir = tempfile::tempdir().unwrap();
    let mut n = Net4::new(dir.path(), 1, &[], Pref::Neither);
    let (block, a) = (Arc::clone(&n.b), n.a());
    n.pauser.arm(block.hash(), &[At::InHold]);
    let rpc = n.rig.submit(&block);
    n.pauser.reached(At::InHold);
    assert_eq!(n.rig.queue().held().0, 1, "BlockChecked(B) is queued");
    // Two passes of the actor, each with its drains, then a `getheaders` from A.
    n.wires[0].sync();
    n.wires[0].knows(a);
    assert_eq!(n.rig.queue().held().0, 1, "BlockChecked(B) applied before its view");
    assert_eq!(n.tip(), a);
    assert_eq!(n.wires[0].last_headers(), Vec::<BlockHash>::new(), "answered from A");
    n.pauser.open(At::InHold);
    assert!(new_block(&rpc));
    until(|| n.rig.queue().held().0 == 0);
    n.wires[0].knows(a);
    assert_eq!(n.wires[0].last_headers(), vec![block.hash()]);
}

/// The two-cell window proper (D122 (3), D126): V has stored SB4's snapshot, which names B, and
/// not yet the `ChainView` (CH1's `between_stores` hook holds V there). The adapter reads only the
/// view, so a drain still shows tip A and leaves `BlockChecked(B)` queued, and a `getheaders`
/// from A is answered from A; after the store, B is shown, applied and served (D47).
// Core: validation.cpp:3471, :3761, net_processing.cpp:3202-3205
#[cfg(feature = "test-support")]
#[test]
fn between_the_two_stores_a_drain_shows_neither_the_new_tip_nor_applies_its_block_checked() {
    let dir = tempfile::tempdir().unwrap();
    let mut n = Net4::new(dir.path(), 1, &[], Pref::Neither);
    let (block, a) = (Arc::clone(&n.b), n.a());
    let latch = Latch::new(true);
    let (view, l, b) = (n.rig.v.view_handle(), Arc::clone(&latch), block.hash());
    n.rig.v.set_between_stores(Some(Arc::new(move || {
        if view.snapshot().tip.tip_hash == b {
            l.hold();
        }
    })));
    let rpc = n.rig.submit(&block);
    latch.entered();
    let view = n.rig.v.view_handle();
    assert_eq!(view.snapshot().tip.tip_hash, block.hash(), "SB4's snapshot is stored");
    assert_eq!(n.tip(), a, "the view is not");
    assert_eq!(n.rig.queue().held().0, 1, "BlockChecked(B) is queued");
    // Two passes of the actor, each with its drains, then a `getheaders` from A.
    n.wires[0].sync();
    n.wires[0].knows(a);
    assert_eq!(n.rig.queue().held().0, 1, "BlockChecked(B) applied before its view");
    assert_eq!(n.tip(), a);
    assert_eq!(n.wires[0].last_headers(), Vec::<BlockHash>::new(), "answered from A");
    latch.release();
    assert!(new_block(&rpc));
    n.rig.v.set_between_stores(None);
    until(|| n.rig.queue().held().0 == 0);
    n.wires[0].knows(a);
    assert_eq!(n.wires[0].last_headers(), vec![block.hash()]);
}

/// Where `UpdatedBlockTip(T)`'s append lands against the head of the peer's `SendMessages`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Append {
    /// (i) Before the head locks the cell, after the actor's previous S: this S shows T and
    /// section 7 uses the list.
    BeforeHead,
    /// (ii) While the head holds the cell: the append waits on it and lands in the next round.
    WhileHeld,
    /// (iii) After the head unlocked it and before section 7 (the region's `Ahead` wait): the
    /// next round.
    AfterUnlock,
}

/// The cell against S through real V (DEC-236, D44): V publishes T before `UpdatedBlockTip(T)`
/// appends to the peer's `block_inv` cell, and the head takes S under that cell, so whenever the
/// moved list holds T, S shows T. For each barrier, a `sendheaders` peer gets T's `headers`
/// exactly once and never Core's `inv` fallback ("bail out if we reorged away", np:6623). In
/// (i) the actor reaches the head's entry at T's connect pause, when the published view is T's
/// accept (its stamp is T's `NewPoWValidBlock` stamp, so no region sees it `Ahead`), and waits
/// there while V connects and publishes T and U appends it: a head that took S before its cell
/// would move T under a view without it. The per-visit recipients (a peer's `VERACK` between
/// two visits) stay on the sim, which places `VERACK` between callbacks (D122 (4)).
// Core: net_processing.cpp:2297-2307, :6574-6660
#[test]
fn the_cell_against_s() {
    for when in [Append::BeforeHead, Append::WhileHeld, Append::AfterUnlock] {
        let dir = tempfile::tempdir().unwrap();
        let mut n = Net4::new(dir.path(), 1, &[], Pref::Headers);
        let (t, pid) = (Arc::clone(&n.b), n.wires[0].id);
        let (relaying, relayed) =
            (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        {
            let relayed = Arc::clone(&relayed);
            n.rig.shared.signals().set_relay_hook(Some(Arc::new(move |id| {
                if id == pid {
                    relayed.store(true, Ordering::SeqCst);
                }
            })));
        }
        let latch = Latch::new(when != Append::BeforeHead);
        {
            let (latch, relaying) = (Arc::clone(&latch), Arc::clone(&relaying));
            let park = if when == Append::BeforeHead {
                LockPoint::HeadEntry(pid)
            } else {
                LockPoint::Head(pid)
            };
            n.rig.shared.signals().set_lock_hook(Some(Arc::new(move |at| match at {
                LockPoint::Relay(id) if id == pid => relaying.store(true, Ordering::SeqCst),
                at if at == park => {
                    latch.park();
                }
                _ => {}
            })));
        }
        match when {
            Append::BeforeHead => {
                n.pauser.arm(t.hash(), &[At::Connect]);
                let rpc = n.rig.submit(&t);
                n.pauser.reached(At::Connect);
                latch.arm();
                latch.entered();
                n.pauser.open(At::Connect);
                until(|| relayed.load(Ordering::SeqCst));
                latch.release();
                assert!(new_block(&rpc));
            }
            Append::WhileHeld => {
                latch.entered();
                assert!(new_block(&n.rig.submit(&t)));
                until(|| relaying.load(Ordering::SeqCst));
                std::thread::sleep(QUIET);
                assert!(!relayed.load(Ordering::SeqCst), "the append waits on the held cell");
                latch.release();
                // U appends once the head unlocks; `sync` below sees T only after that (BL-321).
                until(|| relayed.load(Ordering::SeqCst));
            }
            Append::AfterUnlock => {
                latch.entered();
                n.pauser.arm(t.hash(), &[At::Announce]);
                let rpc = n.rig.submit(&t);
                // T's stamp is stored, its view not published: the head's S is older.
                n.pauser.reached(At::Announce);
                n.rig.gate.set(|g| g.published_shut = true);
                latch.release();
                until(|| n.rig.gate.holds_at() == Some("wait_published"));
                n.pauser.open(At::Announce);
                until(|| relayed.load(Ordering::SeqCst));
                n.rig.gate.set(|g| g.published_shut = false);
                assert!(new_block(&rpc));
            }
        }
        n.wires[0].sync();
        assert_eq!(n.wires[0].announced(t.hash()), (1, 0), "{when:?}: headers(T) once, no inv");
    }
}

/// The timing probe's P2P origin with real V (§6.3 item 6, D52): source x sends each block as a
/// `block`; H is an HB compact peer. **Gate:** for every block, H received `cmpctblock(B)` while
/// V was stopped right after `NewPoWValidBlock`, before the write and the first `ConnectTip`,
/// and received it exactly once. **Reported:** B's last byte at x to `cmpctblock(B)`'s arrival
/// at H, and from the first block to HB selection's `sendcmpct(1)` at x; one JSON line. Idle
/// only: the script-heavy load needs spendable coins (BL-288), and the Core leg and the numbers
/// need a quiet host (D122 (6)).
fn timing_probe(n: usize) -> String {
    let dir = tempfile::tempdir().unwrap();
    let (h, x) = (0, 1);
    let mut net = Net4::new(dir.path(), 2, &[h], Pref::Neither);
    net.wires[x].sendcmpct(false);
    net.wires[x].sync();
    let p = regtest();
    let blocks = branch(&p, *net.chain.last().expect("A").header(), 3, &vec![1; n], 0x43);
    let start = Instant::now();
    let mut lat = Vec::with_capacity(n);
    let mut hb_selected = None;
    for block in &blocks {
        let hash = block.hash();
        net.pauser.arm(hash, &[At::Announce]);
        net.rig.node.clock.set_mocktime(i64::from(block.header().raw().time) + 60);
        net.wires[x].send_block(block);
        let sent = Instant::now();
        net.wires[h].until("cmpctblock(B)", |log| cmpcts(log, hash) > 0);
        lat.push(sent.elapsed());
        net.pauser.reached(At::Announce);
        assert!(!net.written(block), "{hash}: cmpctblock(B) before the write");
        net.pauser.open(At::Announce);
        until(|| net.tip() == hash);
        // x answers the node's pings: the probe's mocktime moves 150 s a block.
        net.wires[x].pump();
        if hb_selected.is_none()
            && net.wires[x]
                .log
                .iter()
                .any(|(c, p)| c.as_str() == "sendcmpct" && p.first() == Some(&1))
        {
            hb_selected = Some(start.elapsed());
        }
    }
    net.wires[h].sync();
    for block in &blocks {
        assert_eq!(cmpcts(&net.wires[h].log, block.hash()), 1, "{}", block.hash());
    }
    lat.sort();
    let pct = |q: f64| lat[((lat.len() - 1) as f64 * q).round() as usize].as_micros();
    format!(
        "{{\"origin\":\"p2p\",\"load\":\"idle\",\"blocks\":{n},\"p50_us\":{},\"p90_us\":{},\
         \"p99_us\":{},\"hb_sendcmpct_us\":{},\"core_leg\":\"PENDING (quiet host)\"}}",
        pct(0.5),
        pct(0.9),
        pct(0.99),
        hb_selected.map_or("null".into(), |d| d.as_micros().to_string()),
    )
}

/// The probe's gate, every test run.
#[test]
fn timing_probe_p2p_origin_orders_the_hb_push_before_the_write() {
    timing_probe(20);
}

/// The measurement: one cell of 200 blocks. Prints one JSON line.
#[test]
#[ignore = "a measurement: run alone, release, on a quiet host"]
fn timing_probe_p2p_origin_idle() {
    println!("{}", timing_probe(200));
}
