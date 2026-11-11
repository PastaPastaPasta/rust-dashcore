//! P3 §5.1's (A − B) node leg with real V (S6a, D113; the Core-leg plan rev8, OD-P3-8): the
//! shared fixture's blocks ([`node_amb::fixture`], built here by [`funded`]) into two fresh
//! nodes at `-mocktime=T0`, with `-checkblockindex=0` and no debug categories.
//!
//! - **A:** the node dials two scripted loopback peers (`-connect`): S, a plain source that
//!   serves `headers` and then a `block` for each `getdata`, and H, which asked for
//!   high-bandwidth compact blocks and knows the prefix tip. The prefix goes through S first,
//!   uncounted, after H's handshake and HB negotiation.
//! - **B:** the same blocks straight to V (`ChainCommand::ProcessBlock`) with the network
//!   stopped (the listener unregistered), the prefix first, uncounted.
//!
//! Each measured pass runs in its own child process under [`node_amb::sys::supervisor`] and
//! counts `instructions:u` on every thread but the driver's (the scripted peers', or the
//! submitter's) inside [`node_amb::sys::window`]'s guarded window. A pass ends when the
//! completion rule (F5) holds: the tip is the last workload block, V's signal queue passed a
//! barrier, S's pings are answered and then the actor acknowledges a full pass over every peer
//! that started after the barrier (dashd-p2p's `sim` `request_pass_ack`), with no announcement,
//! block in flight or unserved request left and no P3 signal held, and in A, H has the last
//! block's `cmpctblock`. After the window, H must hold exactly one `cmpctblock` per workload
//! block, or the pass is refused. P3's share per block is (A − B) / blocks; node-l1's `amb_core` measures
//! Core's, and `node_amb::compare::gate` compares them. The measured binary is this crate's
//! lib-test build (`cfg(test)`, dashd-p2p's `sim` hooks), which can only add node-side work.
//!
//! `amb_passes_run` runs both passes on a few blocks with no counters, in every test run, and
//! `amb_child_per_pass_lifecycle` runs them in children. `amb_node_leg` measures; it is ignored
//! by default and must run alone:
//! `AMB_OUT=<dir> cargo test -p dashd-node --release --lib amb_node_leg -- --ignored
//! --test-threads=1 --nocapture`.
//!
//! Linux x86_64 only, as `node_amb::sys` is.

use node_amb::sys::leg;
use node_amb::sys::meter::{Meter, Plain};

use super::*;

/// The early-close test's barrier patience, once its actor is parked.
const EARLY: Duration = Duration::from_secs(2);
/// The child test every measured pass runs as.
const CHILD: &str = "p2p::amb::measure::amb_pass_child";
/// W, the chainstate's flush thread: held idle in every window (OD-P3-13).
const FLUSH_THREAD: &str = "dashd-flush";
/// How long W must neither run nor wait off its queue for the flush quiesce.
const FLUSH_QUIET: Duration = Duration::from_millis(200);
/// A test knob: a child pass B asks V for a full flush inside its window.
const FLUSH_IN_WINDOW: &str = "P3_AMB_FLUSH_IN_WINDOW";

/// The fixture's blocks.
fn blocks_of(f: &Fixture) -> (Vec<Arc<Block>>, Vec<Arc<Block>>) {
    let decode = |bytes: &[Vec<u8>]| -> Vec<Arc<Block>> {
        bytes
            .iter()
            .map(|b| {
                let block = dash_consensus::primitives::decode_block(&mut b.as_slice());
                Arc::new(block.expect("a fixture block").into_value())
            })
            .collect()
    };
    (decode(&f.prefix), decode(&f.workload))
}

/// A pass's record, and what the smoke compares across passes.
struct PassOut {
    record: Record,
    /// V's active chain above the prefix.
    accepted: Vec<BlockHash>,
    /// H's `cmpctblock`s per block hash (pass A).
    cmpcts: BTreeMap<BlockHash, usize>,
}

/// The node's `-checkblockindex` as V and its header chain resolved it.
fn check_flags(node: &Node) -> Vec<(String, bool)> {
    let q = node
        .chain()
        .expect("V runs")
        .handle()
        .view_handle()
        .query(|q| (q.chain.config().consistency_checks, q.check_block_index));
    let (header, v) = q.recv_timeout(PATIENCE).expect("V answers").expect("V replied");
    vec![("header-chain".into(), header), ("v".into(), v)]
}

/// V's active chain from `from` (a height) to its tip.
fn active_from(node: &Node, from: u32) -> Vec<BlockHash> {
    let view = node.chain().expect("V runs").handle().view_handle();
    let v = view.view();
    (from..=v.snap.tip.tip_height)
        .map(|h| {
            let id = view.at(&v, h).expect("on the active chain");
            view.entry(id).expect("an entry").hash
        })
        .collect()
}

/// Core's `SyncWithValidationInterfaceQueue` from the driver: every V signal queued so far was
/// delivered.
fn delivered(signals: &ValidationSignals) -> bool {
    let (tx, rx) = crossbeam_channel::bounded(1);
    signals.enqueue(Queued::Barrier(tx));
    rx.recv_timeout(PATIENCE).is_ok()
}

fn not_ibd(node: &Node) -> bool {
    let view = node.chain().expect("V runs").handle().view_handle();
    !view.is_initial_block_download(&view.view().snap, node.clock.node_time())
}

/// The `cmpctblock` frames' block hashes (the header is the payload's first 80 bytes).
fn cmpct_hash(payload: &[u8]) -> BlockHash {
    Header::new(BlockHeader::decode(&mut &payload[..80]).expect("a header")).hash()
}

/// A scripted peer with the frames it has seen and its pings.
struct Wire {
    peer: Peer,
    id: PeerId,
    nonce: u64,
    cmpcts: BTreeMap<BlockHash, usize>,
    /// A block whose `cmpctblock` this peer drops unread (the relay-map test).
    drop: Option<BlockHash>,
    /// `getdata` block requests not yet served.
    wanted: Vec<BlockHash>,
    pongs: Vec<Vec<u8>>,
    getheaders: usize,
}

impl Wire {
    fn pump(&mut self) {
        for (cmd, payload) in self.peer.poll() {
            match cmd.as_str() {
                "cmpctblock" => {
                    let hash = cmpct_hash(&payload);
                    if self.drop != Some(hash) {
                        *self.cmpcts.entry(hash).or_default() += 1;
                    }
                }
                "getdata" => {
                    let gd = GetData::decode(&mut &payload[..]).expect("a getdata");
                    let blocks = gd.inventory().iter().filter(|i| i.inv_type() == InvType::Block);
                    self.wanted.extend(blocks.map(block_hash));
                }
                "pong" => self.pongs.push(payload),
                "getheaders" => self.getheaders += 1,
                _ => {}
            }
        }
    }

    /// Two `ping` round trips: the actor has handled every frame sent before, and the pass that
    /// answered the first has run its `SendMessages`, whose frames precede the second `pong`.
    fn sync(&mut self) -> bool {
        self.sync_within(PATIENCE)
    }

    fn sync_within(&mut self, patience: Duration) -> bool {
        let deadline = Instant::now() + patience;
        for _ in 0..2 {
            self.nonce += 1;
            let nonce = self.nonce.to_le_bytes().to_vec();
            self.peer.send("ping", &nonce);
            while !self.pongs.contains(&nonce) {
                if Instant::now() > deadline {
                    return false;
                }
                self.pump();
            }
        }
        true
    }
}

fn sendcmpct_hb() -> Vec<u8> {
    let mut buf = Vec::new();
    SendCmpct::new(true, 1).encode(&mut buf);
    buf
}

/// Both passes' options after `connect`. `-parbls=1` keeps the BLS check workers, which share
/// the script workers' `d-scriptch.<n>` names, out of the serial census, as Core's pinned test
/// setup starts none (`ChainstateLoadOptions::bls_threads` 0).
fn args(mut connect: Vec<String>) -> Vec<String> {
    connect.extend(
        ["-dnsseed=0", "-fixedseeds=0", "-checkblockindex=0", "-par=1", "-parbls=1"]
            .map(String::from),
    );
    connect.push(format!("-mocktime={T0}"));
    connect
}

/// Pass A's options: the scripted peers, which speak V1.
fn args_a(s: &TcpListener, h: &TcpListener) -> Vec<String> {
    let mut connect: Vec<String> =
        [s, h].iter().map(|l| format!("-connect={}", l.local_addr().expect("addr"))).collect();
    connect.push("-v2transport=0".into());
    args(connect)
}

fn args_b() -> Vec<String> {
    args(vec!["-connect=0".into()])
}

/// The run conditions at a window's start.
fn conditions(node: &Node, options: Vec<String>, hb_at_start: Option<bool>) -> Conditions {
    Conditions {
        ibd_at_start: !not_ibd(node),
        hb_at_start,
        options,
        check_block_index: check_flags(node),
        log_categories: node.ui.logger().category_mask().0,
        bench: None,
        script_check_threads: node_amb::sys::script_check_threads().expect("the census"),
    }
}

/// The flush quiesce before a window (OD-P3-13), run before the prefix's "nothing pending"
/// checks so that V's handling of a flush it starts (its release, `ChainStateFlushed`) is done
/// by then. A `FlushStateToDisk(NONE)` starts any job V's kept causes (or its once-only
/// `LEFT_IBD`) call for now, adding no cause of its own; then W must sleep without running for
/// [`FLUSH_QUIET`], twice in a row, each after another such call. A sleeping W is not proven to
/// be on its queue (a job stalled in a wait looks the same), so W's work inside the window
/// still refuses the pass ([`FLUSH_THREAD`]).
///
/// W is found by name, so only a measured child, which runs one node, watches it; in-process
/// (the smoke tests, beside other tests' nodes) V is only asked.
fn flush_quiesce(node: &Node) {
    use dashd_chainstate::flush::FlushStateMode;
    use node_amb::sys::{run_ns, state, supervisor, threads_named};
    let handle = node.chain().expect("V runs").handle();
    let poke = || {
        let r = handle.flush(FlushStateMode::None).recv_timeout(PATIENCE);
        assert!(matches!(r, Some(Ok(()))), "V's flush(NONE): {r:?}");
    };
    if supervisor::pass().is_none() {
        return poke();
    }
    let w = threads_named(|n| n == FLUSH_THREAD).expect("the census");
    assert!(!w.is_empty(), "no {FLUSH_THREAD} thread");
    let ran = || w.iter().map(|&t| run_ns(t).expect("W's schedstat")).sum::<u64>();
    let asleep = || w.iter().all(|&t| state(t).expect("W's state") == 'S');
    let deadline = Instant::now() + PATIENCE;
    let mut quiet = 0;
    while quiet < 2 {
        assert!(Instant::now() < deadline, "W never quiesced");
        poke();
        let before = ran();
        std::thread::sleep(FLUSH_QUIET);
        quiet = if asleep() && ran() == before {
            quiet + 1
        } else {
            0
        };
    }
}

/// Parks the actor in its final drain: at the head of a `send_messages` once the published
/// view's tip is `last`, until released. [`Park::new`] parks the first such head; when `early`
/// (the early-close test), the pass waits for the park, gives up on the barrier after [`EARLY`],
/// reads its completion with the actor still parked, and releases it. [`Park::h_after_s`]
/// parks H's head only, in a pass after S answered the pass's pings (SOL-1's schedule).
struct Park {
    last: BlockHash,
    early: bool,
    armed: AtomicBool,
    /// Only this peer's head parks (H's, once [`pass_a`] knows its id).
    peer: Option<std::sync::OnceLock<PeerId>>,
    entered: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),
    go: (crossbeam_channel::Sender<()>, crossbeam_channel::Receiver<()>),
}

impl Park {
    fn new(last: BlockHash, early: bool) -> Arc<Park> {
        Arc::new(Park {
            last,
            early,
            armed: AtomicBool::new(true),
            peer: None,
            entered: crossbeam_channel::bounded(1),
            go: crossbeam_channel::bounded(1),
        })
    }

    /// Armed by [`pass_a`] once S answered its pings after the tip's validation barrier.
    fn h_after_s(last: BlockHash) -> Arc<Park> {
        Arc::new(Park {
            last,
            early: false,
            armed: AtomicBool::new(false),
            peer: Some(std::sync::OnceLock::new()),
            entered: crossbeam_channel::bounded(1),
            go: crossbeam_channel::bounded(1),
        })
    }
}

fn install_park(node: &Node, park: Arc<Park>) {
    use dashd_p2p::processing::peerman::LockPoint;
    let shared = Arc::clone(node.net().expect("the network").shared().expect("started"));
    let view = node.chain().expect("V runs").handle().view_handle();
    shared.signals().set_lock_hook(Some(Arc::new(move |at| {
        if let LockPoint::HeadEntry(id) = at
            && park.peer.as_ref().is_none_or(|p| p.get() == Some(&id))
            && super::super::realv::on_actor()
            && view.view().snap.tip.tip_hash == park.last
            && park.armed.swap(false, Ordering::SeqCst)
        {
            park.entered.0.send(()).unwrap();
            park.go.1.recv_timeout(PATIENCE).expect("released");
        }
    })));
}

/// The actor's acknowledgment at the end of a full pass over every peer that started after this
/// call (completion (3), SOL-1), with both peers read meanwhile; `None` after `patience`.
fn pass_ack(shared: &Shared, s: &mut Wire, h: &mut Wire, patience: Duration) -> Option<PassAck> {
    use std::sync::mpsc::RecvTimeoutError;
    let rx = shared.request_pass_ack();
    let deadline = Instant::now() + patience;
    loop {
        // A short block between pumps, not a spin: the driver is uncounted, and a spinning
        // driver takes a CPU from the counted threads it waits on (R2-N4).
        match rx.recv_timeout(Duration::from_millis(1)) {
            Ok(ack) => return Some(ack),
            Err(RecvTimeoutError::Disconnected) => return None,
            Err(RecvTimeoutError::Timeout) if Instant::now() > deadline => return None,
            Err(RecvTimeoutError::Timeout) => {
                s.pump();
                h.pump();
            }
        }
    }
}

/// [`pass_a`]'s test knobs.
#[derive(Default)]
struct Script {
    park: Option<Arc<Park>>,
    /// A block S announces after the workload and never serves (the pending-work test).
    withhold: Option<Arc<Block>>,
    /// A workload block whose `cmpctblock` H drops (the relay-map test).
    drop_cmpct: Option<BlockHash>,
}

/// Pass A on `f`, its window `meter`'s, with `script`'s test knobs. After the window, H must
/// hold exactly one `cmpctblock` per workload block, or the pass is refused (SOL-2).
fn pass_a(
    dir: &Path,
    f: &Fixture,
    meter: &mut dyn Meter,
    script: Script,
    revision: String,
) -> Result<PassOut, Refusal> {
    let Script {
        park,
        withhold,
        drop_cmpct,
    } = script;
    let params = regtest();
    let (prefix, blocks) = blocks_of(f);
    let end = prefix.len() + blocks.len();
    let (sl, hl) = (
        TcpListener::bind("127.0.0.1:0").expect("bind"),
        TcpListener::bind("127.0.0.1:0").expect("bind"),
    );
    let options = args_a(&sl, &hl);
    let node = node(dir, &options.iter().map(String::as_str).collect::<Vec<_>>());
    let early = park.clone().filter(|p| p.early);
    let h_park = park.clone().filter(|p| p.peer.is_some());
    if let Some(park) = park {
        install_park(&node, park);
    }
    let net = node.net().expect("the network");
    let shared = Arc::clone(net.shared().expect("started"));
    let s_peer = Peer::dialled(&sl, &params, end as i32, T0);
    let h_peer = Peer::dialled(&hl, &params, prefix.len() as i32, T0);
    let deadline = Instant::now() + PATIENCE;
    let id_of = |l: &TcpListener| loop {
        let port = l.local_addr().expect("addr").port();
        let table = net.core().peers();
        if let Some(p) =
            table.iter().find(|p| p.addr().service.port == port && p.successfully_connected())
        {
            break p.id();
        }
        assert!(Instant::now() < deadline, "the handshakes");
        std::thread::sleep(Duration::from_millis(5));
    };
    let wire = |peer, id| Wire {
        peer,
        id,
        nonce: 0,
        cmpcts: BTreeMap::new(),
        drop: None,
        wanted: Vec::new(),
        pongs: Vec::new(),
        getheaders: 0,
    };
    let mut s = wire(s_peer, id_of(&sl));
    let mut h = wire(h_peer, id_of(&hl));
    h.drop = drop_cmpct;
    if let Some(p) = h_park.as_ref().and_then(|p| p.peer.as_ref()) {
        p.set(h.id).expect("one H");
    }
    // H's handshake and HB negotiation come first.
    h.peer.send("sendcmpct", &sendcmpct_hb());
    assert!(h.sync(), "H's handshake");
    // The node picks its one IBD headers-sync peer; S announces the prefix whichever it is.
    while s.getheaders + h.getheaders == 0 {
        assert!(Instant::now() < deadline, "no getheaders");
        s.pump();
        h.pump();
    }
    let by_hash: HashMap<BlockHash, &Arc<Block>> =
        prefix.iter().chain(&blocks).map(|b| (b.hash(), b)).collect();
    // Sends `part`'s headers, then serves each `getdata` until V's tip is at `last`.
    let serve = |s: &mut Wire, h: &mut Wire, part: &[Arc<Block>], last: usize| {
        s.peer.send("headers", &headers_msg(part));
        while tip_height(&node) < last as u32 {
            assert!(
                Instant::now() < deadline + PATIENCE,
                "the tip stopped at {}",
                tip_height(&node)
            );
            s.pump();
            h.pump();
            for hash in std::mem::take(&mut s.wanted) {
                s.peer.send("block", &dash_consensus::core_serialize(&**by_hash[&hash]));
            }
        }
    };
    serve(&mut s, &mut h, &prefix, prefix.len());
    flush_quiesce(&node);

    // H knows every parent header: the prefix tip.
    h.peer.send("headers", &headers_msg(prefix.last()));
    let signals = node.chain().expect("V runs").signals().clone();
    assert!(delivered(&signals) && s.sync() && h.sync(), "the prefix quiesces");
    let tip = node.chain().expect("V runs").handle().view_handle().view().snap.tip.tip_hash;
    assert_eq!(tip, prefix.last().expect("a prefix").hash(), "the prefix tip");
    let handle = |id| net.core().peers().iter().find(|p| p.id() == id).cloned();
    let (s_handle, h_handle) = (handle(s.id).expect("S"), handle(h.id).expect("H"));
    // No block in flight or unserved, no P3 signal held, and nothing queued either way on S or
    // H: the start's quiescence, and (3)'s "nothing pending" at the window's end.
    let quiet = |s: &Wire, h: &Wire| {
        shared.signals().queue.held() == (0, 0)
            && shared.node_state_stats(&s_handle).is_some_and(|st| st.heights_in_flight.is_empty())
            && [&s_handle, &h_handle].iter().all(|p| p.send_queue_empty() && p.recv_queue_empty())
            && s.wanted.is_empty()
            && h.wanted.is_empty()
    };
    assert!(quiet(&s, &h), "the prefix left work pending");
    let hb = shared.peek_headers(h.id).expect("H").requested_hb_cmpctblocks;
    let conditions = conditions(&node, options, Some(hb));
    // Every worker a pass uses exists before the guard: the blocking pool too.
    net.rt.block_on(net.rt.spawn_blocking(|| ())).expect("a blocking thread");
    h.cmpcts.clear();

    meter.open(false)?;
    serve(&mut s, &mut h, &blocks, end);
    if let Some(extra) = &withhold {
        s.peer.send("headers", &headers_msg([extra]));
        while !s.wanted.contains(&extra.hash()) {
            assert!(Instant::now() < deadline + PATIENCE, "the withheld block was never asked for");
            s.pump();
        }
    }
    let last = blocks.last().expect("a workload").hash();
    let barrier = match &early {
        Some(p) => {
            p.entered.1.recv_timeout(PATIENCE).expect("the actor parks in its final drain");
            EARLY
        }
        None => PATIENCE,
    };
    let tip_is_last = tip_height(&node) as usize == end;
    let signals_barrier = delivered(&signals);
    // (3): S's frames are handled (its pings answered), then the actor acknowledges a full pass
    // over every peer that started after the barrier, with H's announcements sent.
    let synced = s.sync_within(barrier);
    if let Some(p) = h_park.as_ref().filter(|_| synced) {
        p.armed.store(true, Ordering::SeqCst);
    }
    let ack = synced.then(|| pass_ack(&shared, &mut s, &mut h, barrier)).flatten();
    let mut completion = Completion {
        tip_is_last,
        signals_barrier,
        actor_barrier: Some(ack.is_some()),
        hb_last_cmpct: None,
        quiesced: None,
    };
    let hb_deadline = Instant::now() + barrier;
    while !h.cmpcts.contains_key(&last) && Instant::now() < hb_deadline {
        h.pump();
    }
    completion.hb_last_cmpct = Some(h.cmpcts.contains_key(&last));
    let relay_done = ack.is_some_and(|a| a.relay_pending.is_empty());
    completion.quiesced = Some(relay_done && quiet(&s, &h));
    if let Some(p) = early {
        p.go.0.send(()).expect("the parked actor");
    }
    let counts = meter.close(&completion, true)?;

    // Uncounted: H's pings are answered after every frame queued to it before them, so it has
    // read every cmpctblock the workload sent.
    assert!(h.sync(), "H after the window");
    node_amb::compare::one_each(&h.cmpcts, blocks.iter().map(|b| b.hash()))?;
    Ok(PassOut {
        record: Record::new(
            Leg::Node,
            Pass::A,
            Identity::of(f, revision, None),
            conditions,
            completion,
            counts,
        ),
        accepted: active_from(&node, prefix.len() as u32 + 1),
        cmpcts: h.cmpcts,
    })
}

/// Pass B on `f`: the blocks to V with the network stopped, each awaited before the next, as
/// P3's actor submits; the prefix first, uncounted.
fn pass_b(
    dir: &Path,
    f: &Fixture,
    meter: &mut dyn Meter,
    revision: String,
) -> Result<PassOut, Refusal> {
    let (prefix, blocks) = blocks_of(f);
    let end = prefix.len() + blocks.len();
    let options = args_b();
    let mut node = node(dir, &options.iter().map(String::as_str).collect::<Vec<_>>());
    node.stop_net();
    let handle = node.chain().expect("V runs").handle().clone();
    let signals = node.chain().expect("V runs").signals().clone();
    let submit = |part: &[Arc<Block>], last: usize| {
        for block in part {
            let r = handle.process_block(Arc::clone(block), true, None, true).recv().expect("V");
            assert!(r.new_block, "a new block");
        }
        wait_tip(&node, last as u32);
    };
    submit(&prefix, prefix.len());
    flush_quiesce(&node);
    assert!(delivered(&signals), "the prefix quiesces");
    let conditions = conditions(&node, options, None);
    meter.open(false)?;
    if std::env::var_os(FLUSH_IN_WINDOW).is_some() {
        use dashd_chainstate::flush::FlushStateMode;
        let r = handle.flush(FlushStateMode::Always).recv_timeout(PATIENCE);
        assert!(matches!(r, Some(Ok(()))), "V's flush(ALWAYS): {r:?}");
    }
    submit(&blocks, end);
    let completion = Completion {
        tip_is_last: tip_height(&node) as usize == end,
        signals_barrier: delivered(&signals),
        ..Completion::default()
    };
    let counts = meter.close(&completion, false)?;
    Ok(PassOut {
        record: Record::new(
            Leg::Node,
            Pass::B,
            Identity::of(f, revision, None),
            conditions,
            completion,
            counts,
        ),
        accepted: active_from(&node, prefix.len() as u32 + 1),
        cmpcts: BTreeMap::new(),
    })
}

/// The workload's hashes.
fn hashes(f: &Fixture) -> Vec<BlockHash> {
    blocks_of(f).1.iter().map(|b| b.hash()).collect()
}

/// The funded blocks are the checked-in fixture's, byte for byte, under its pinned sha256.
/// `AMB_BLESS=1` writes the file instead.
#[test]
fn amb_fixture_is_the_node_s_funded_blocks() {
    let (prefix, work) = funded(&regtest(), &mainnet_ntx());
    let ser = |b: &[Arc<Block>]| b.iter().map(|b| dash_consensus::core_serialize(&**b)).collect();
    let built = Fixture {
        prefix: ser(&prefix),
        workload: ser(&work),
    }
    .encode();
    if std::env::var_os("AMB_BLESS").is_some() {
        std::fs::write(fixture::path(), &built).expect("writing the fixture");
        panic!("blessed {} with sha256 {}", fixture::path().display(), fixture::sha256_hex(&built));
    }
    let file = std::fs::read(fixture::path()).expect("the fixture");
    assert!(file == built, "the fixture is not the node's funded blocks (AMB_BLESS=1 rewrites it)");
    assert_eq!(fixture::sha256_hex(&file), fixture::SHA256);
    // 1 s apart, the last at T0: the prefix tip is T0 − 201 s old.
    let times: Vec<i64> =
        prefix.iter().chain(&work).map(|b| i64::from(b.header().raw().time())).collect();
    assert!(times.windows(2).all(|w| w[1] == w[0] + 1));
    assert_eq!((times[prefix.len() - 1], *times.last().unwrap()), (T0 - work.len() as i64, T0));
}

/// The semantic smoke (no counters, every test run): both passes accept the same blocks, the
/// fixture's; neither is in IBD at its window's start; H is high-bandwidth and gets each block's
/// `cmpctblock` once; every completion condition held, and no block-index check or debug
/// category is on.
#[test]
fn amb_passes_run() {
    let f = Fixture::smoke().expect("the fixture");
    assert!(blocks_of(&f).1.iter().any(|b| b.txs().len() > 1), "the rows have fillers");
    let a = pass_a(
        tempfile::tempdir().unwrap().path(),
        &f,
        &mut Plain::default(),
        Script::default(),
        "smoke".into(),
    )
    .unwrap();
    let b = pass_b(tempfile::tempdir().unwrap().path(), &f, &mut Plain::default(), "smoke".into())
        .unwrap();
    assert_eq!(a.accepted, hashes(&f));
    assert_eq!(b.accepted, a.accepted);
    for (out, pass_a) in [(&a, true), (&b, false)] {
        let r = &out.record;
        assert!(!r.conditions.ibd_at_start, "{:?}", r.pass);
        assert!(r.completion.check(pass_a).is_ok(), "{:?}", r.completion);
        assert!(r.conditions.check_block_index.iter().all(|(_, on)| !on), "{:?}", r.conditions);
        assert_eq!(r.conditions.log_categories, 0);
        assert!(r.counts.is_none());
    }
    assert_eq!(a.record.conditions.hb_at_start, Some(true));
    let once: BTreeMap<BlockHash, usize> = hashes(&f).into_iter().map(|h| (h, 1)).collect();
    assert_eq!(a.cmpcts, once, "H's cmpctblocks");
}

/// F5's held-drain test: with the actor parked in its final drain after V published the last
/// tip, the window stays open, and released, the pass completes; a pass that gives up on the
/// actor barrier while the actor is parked (closing at tip publication) is refused for that
/// barrier and for the work the parked actor leaves pending.
#[test]
fn amb_completion_waits_for_the_actor_s_final_drain() {
    let f = Fixture::smoke().expect("the fixture");
    let last = *hashes(&f).last().unwrap();
    let park = Park::new(last, false);
    let plain = Plain::default();
    let closed = Arc::clone(&plain.closed);
    let dir = tempfile::tempdir().unwrap();
    let run = {
        let (park, path, f) = (Arc::clone(&park), dir.path().to_owned(), f.clone());
        std::thread::spawn(move || {
            let mut plain = plain;
            let script = Script {
                park: Some(park),
                ..Script::default()
            };
            pass_a(&path, &f, &mut plain, script, "held".into())
        })
    };
    park.entered.1.recv_timeout(PATIENCE).expect("the actor parks in its final drain");
    std::thread::sleep(Duration::from_millis(500));
    assert!(!closed.load(Ordering::SeqCst), "the window closed with the actor parked");
    park.go.0.send(()).unwrap();
    let out = run.join().expect("the pass").expect("completes");
    assert!(closed.load(Ordering::SeqCst) && out.record.completion.check(true).is_ok());

    let dir = tempfile::tempdir().unwrap();
    let script = Script {
        park: Some(Park::new(last, true)),
        ..Script::default()
    };
    let e = pass_a(dir.path(), &f, &mut Plain::default(), script, "early".into())
        .err()
        .expect("an early close is refused");
    assert!(e.0.contains("actor_barrier: Some(false)"), "{e}");
}

/// SOL-1: S answers its pings after the tip's validation barrier while H's `SendMessages` is
/// held (the actor parked at H's head in a later pass): the window stays open, because (3) waits
/// for the actor's acknowledgment of a full pass over every peer, and released, the pass
/// completes.
#[test]
fn amb_completion_waits_for_h_s_send_messages_after_s_answered() {
    let f = Fixture::smoke().expect("the fixture");
    let park = Park::h_after_s(*hashes(&f).last().unwrap());
    let plain = Plain::default();
    let closed = Arc::clone(&plain.closed);
    let dir = tempfile::tempdir().unwrap();
    let run = {
        let (park, path, f) = (Arc::clone(&park), dir.path().to_owned(), f.clone());
        std::thread::spawn(move || {
            let mut plain = plain;
            let script = Script {
                park: Some(park),
                ..Script::default()
            };
            pass_a(&path, &f, &mut plain, script, "held-h".into())
        })
    };
    park.entered.1.recv_timeout(PATIENCE).expect("the actor parks at H's head after S answered");
    std::thread::sleep(Duration::from_millis(500));
    assert!(!closed.load(Ordering::SeqCst), "the window closed with H's SendMessages held");
    park.go.0.send(()).unwrap();
    let out = run.join().expect("the pass").expect("completes");
    assert!(closed.load(Ordering::SeqCst) && out.record.completion.check(true).is_ok());
}

/// SOL-2: an intermediate workload block's `cmpctblock` never reaches H while the last one
/// does: every in-window condition holds, and the full map after the window refuses the pass.
#[test]
fn amb_a_missing_intermediate_relay_is_refused() {
    let f = Fixture::smoke().expect("the fixture");
    let hashes = hashes(&f);
    let script = Script {
        drop_cmpct: Some(hashes[hashes.len() / 2]),
        ..Script::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let e = pass_a(dir.path(), &f, &mut Plain::default(), script, "relay".into())
        .err()
        .expect("a missing relay is refused");
    assert!(e.0.contains("H's cmpctblocks after the window") && e.0.contains("1 of"), "{e}");
}

/// F5 (3)'s "nothing pending": a block S announced and never served is in flight and unserved
/// at the window's end although the actor answered its pings, and the pass is refused.
#[test]
fn amb_completion_refuses_pending_work() {
    let f = Fixture::smoke().expect("the fixture");
    let (prefix, blocks) = blocks_of(&f);
    let height = (prefix.len() + blocks.len() + 1) as u32;
    let parent = *blocks.last().expect("a workload").header();
    let time = u32::try_from(T0).expect("T0 fits") + 1;
    let extra = mine(&regtest(), parent, time, 1, [vec![coinbase(height, 0x77)]]).remove(0);
    let dir = tempfile::tempdir().unwrap();
    let script = Script {
        withhold: Some(extra),
        ..Script::default()
    };
    let e = pass_a(dir.path(), &f, &mut Plain::default(), script, "pending".into())
        .err()
        .expect("pending work is refused");
    assert!(e.0.contains("actor_barrier: Some(true)"), "{e}");
    assert!(e.0.contains("quiesced: Some(false)"), "{e}");
}

/// The measured pass of a child: `AMB_PASS=node:<a|b>:<rows>:<count 0|1>`. A no-op otherwise.
/// The node's datadir is removed before the child's record exits it.
#[test]
fn amb_pass_child() {
    leg::child_main("node", true, |mut p| {
        p.meter.hold_idle(&[FLUSH_THREAD]);
        let dir = tempfile::tempdir().expect("a datadir");
        let out = if p.pass_a {
            pass_a(dir.path(), &p.fixture, &mut p.meter, Script::default(), p.revision)
        } else {
            pass_b(dir.path(), &p.fixture, &mut p.meter, p.revision)
        };
        drop(dir);
        out.map(|o| o.record)
    });
}

/// The child-per-pass lifecycle (rev5), end to end on a few rows: node A then node B, each in
/// a fresh child; both records arrive with matching identities from distinct processes, and the
/// leg's report is produced (with a PMU; without one, the comparison refuses the missing count).
#[test]
fn amb_child_per_pass_lifecycle() {
    let count = node_amb::sys::counters::available();
    if !count {
        node_amb::sys::supervisor::no_pmu("amb_child_per_pass_lifecycle (counting off)");
    }
    let (a, b) = leg::run(CHILD, "node", 4, count).expect("both passes");
    assert_ne!(a.pid, b.pid);
    assert_ne!(a.pid, std::process::id());
    assert_eq!(a.identity, b.identity);
    assert_eq!(a.identity.workload_blocks, 4);
    let report = node_amb::compare::leg(&a, &b);
    if count {
        let r = report.expect("a report");
        assert!(r.a_instructions > 0 && r.b_instructions > 0, "{r:?}");
    } else {
        assert!(report.unwrap_err().0.contains("no count"));
    }
}

/// OD-P3-13: W's work inside a window refuses the pass, here a full flush V is asked for inside
/// pass B's (with a PMU; without one there is no count to see it).
#[test]
fn amb_flush_work_in_the_window_is_refused() {
    if !node_amb::sys::counters::available() {
        node_amb::sys::supervisor::no_pmu("amb_flush_work_in_the_window_is_refused");
        return;
    }
    let env = [(FLUSH_IN_WINDOW, "1".to_string())];
    let e = node_amb::sys::supervisor::run(CHILD, "node:b:4:1", &env).expect_err("refused");
    assert!(e.0.contains("idle thread") && e.0.contains("instructions inside the window"), "{e}");
    let (_, bytes) = node_amb::sys::supervisor::run(CHILD, "node:b:4:1", &[]).expect("quiet");
    let r = Record::decode(&bytes).expect("a record");
    assert!(r.counts.is_some_and(|c| c.threads_idle >= 1), "{:?}", r.counts);
}

/// The measurement: every fixture row is one block (or `AMB_ROWS` rows), each pass in its own
/// child. Prints the leg's report and saves it with the records under `AMB_OUT`.
#[test]
#[ignore = "a measurement: run alone, release, with a PMU"]
fn amb_node_leg() {
    assert!(std::env::var_os(FLUSH_IN_WINDOW).is_none(), "{FLUSH_IN_WINDOW} is a test knob");
    let (a, b) = leg::run(CHILD, "node", leg::measured_rows(), true).expect("both passes");
    let report = node_amb::compare::leg(&a, &b).expect("the node leg");
    println!("{}", leg::save("node", &a, &b, &report).expect("the report"));
}
