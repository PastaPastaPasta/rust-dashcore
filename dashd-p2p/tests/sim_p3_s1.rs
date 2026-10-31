//! P3 S1 in `sim` (P3 §6.1): the drain points and each message's drained snapshot (I0), the
//! post-reply redrain (D46), the header cell's `Ahead` retries (D56), the two cells (D47), C5′
//! (DEC-240), the thread-local no-V-wait assertion, DEC-230's reachability, the `RelayList`
//! overflow, `merkle_checked`, the S2-f invalid branch, `getpeerinfo` against a block wait, and
//! the block wait's two wake-ups (its in-wait drain and the reply's waker).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use dash_consensus::{Block, BlockValidationError};
use dash_p2p::policy::Throw;
use dash_p2p::rng::FastRandom;
use dash_p2p::{CommandString, ConnectionType, PeerId};
use dash_p2p_core::{Headers, Inv};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::BlockHash;
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::download::BlockDownload;
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::peer::HeaderCell;
use dashd_p2p::processing::seams::{
    BlockCheckedCx, BlockOutcome, BlockSubmit, BlockSync, BlockWait, ChainAccess, GetDataBatch,
    Interrupted, SyncCx, Validity,
};
use dashd_p2p::processing::signals::{ServiceWait, WaitEvent};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimChain, SimListener, SimPeer, until};
use dashd_p2p::{HandlerCx, NetHandler, Registry, Seams, Shared};

fn params() -> Arc<ChainParams> {
    Arc::new(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap())
}

fn sim_with(registry: Registry, block_sync: Box<dyn BlockSync>) -> Sim {
    let p = params();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let seams = Seams {
        block_sync,
        ..Seams::default()
    };
    Sim::with(p, id, ProcessorOptions::default(), registry, seams, |_| {})
}

fn outbound(sim: &mut Sim, n: u8) -> SimPeer {
    let p = sim.connect(n, ConnectionType::OutboundFullRelay, Default::default());
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    sim.pass();
    sim.sent(&p);
    p
}

fn block_msg(b: &Block) -> Vec<u8> {
    dash_consensus::core_serialize(b)
}

fn cmds(frames: &[(CommandString, Vec<u8>)]) -> Vec<&str> {
    frames.iter().map(|(c, _)| c.as_str()).collect()
}

/// What the recording seam saw.
#[derive(Default)]
struct Seen {
    /// `BlockChecked` deliveries.
    checked: AtomicUsize,
    /// At each `HeadersDirectFetchBlocks`: the deliveries so far and the snapshot's epoch.
    direct_fetch: Mutex<Vec<(usize, u64)>>,
    /// At each `mapBlockSource` insertion: whether the signal queue was `Live`.
    source_live: Mutex<Vec<bool>>,
}

/// P3's `BlockDownload` with a recorder around it.
struct Recording {
    inner: BlockDownload,
    seen: Arc<Seen>,
    shared: Arc<OnceLock<Arc<Shared>>>,
}

impl Recording {
    fn new(seen: &Arc<Seen>, shared: &Arc<OnceLock<Arc<Shared>>>) -> Box<Self> {
        Box::new(Self {
            inner: BlockDownload::new(),
            seen: Arc::clone(seen),
            shared: Arc::clone(shared),
        })
    }
}

impl BlockSync for Recording {
    fn is_block_requested(&self, hash: &BlockHash) -> bool {
        self.inner.is_block_requested(hash)
    }
    fn any_in_flight(&self) -> bool {
        BlockSync::any_in_flight(&self.inner)
    }
    fn blocks_in_flight(&self, peer: PeerId) -> usize {
        BlockSync::blocks_in_flight(&self.inner, peer)
    }
    fn stall_section(&mut self, cx: &mut SyncCx<'_>, now_us: i64) -> bool {
        self.inner.stall_section(cx, now_us)
    }
    fn getdata_section(
        &mut self,
        cx: &mut SyncCx<'_>,
        b: &mut GetDataBatch<'_>,
        sync: bool,
        now_us: i64,
    ) {
        self.inner.getdata_section(cx, b, sync, now_us);
    }
    fn headers_direct_fetch(&mut self, cx: &mut SyncCx<'_>, _last: dash_consensus::BlockId) {
        let n = self.seen.checked.load(Ordering::SeqCst);
        self.seen.direct_fetch.lock().unwrap().push((n, cx.snap.epoch));
    }
    fn block_checked(
        &mut self,
        cx: &mut BlockCheckedCx<'_>,
        hash: &BlockHash,
        invalid: Option<&BlockValidationError>,
        ibd: bool,
    ) {
        self.seen.checked.fetch_add(1, Ordering::SeqCst);
        self.inner.block_checked(cx, hash, invalid, ibd);
    }
    fn finalize(&mut self, peer: PeerId) {
        self.inner.finalize(peer);
    }
    fn remove_block_request(&mut self, hash: &BlockHash, from: Option<PeerId>, now_us: i64) {
        self.inner.remove_block_request(hash, from, now_us);
    }
    fn emplace_block_source(&mut self, hash: BlockHash, peer: PeerId, full: bool) {
        let live = self.shared.get().is_some_and(|s| s.signals().queue.is_live());
        self.seen.source_live.lock().unwrap().push(live);
        self.inner.emplace_block_source(hash, peer, full);
    }
    fn erase_block_source(&mut self, hash: &BlockHash) {
        self.inner.erase_block_source(hash);
    }
    fn bind_source_counter(&mut self, counter: Arc<AtomicUsize>) {
        BlockSync::bind_source_counter(&mut self.inner, counter);
    }
}

const QSIGREC: [CommandString; 1] = [CommandString::from_static("qsigrec")];

/// A handler that logs, per message, the seam's deliveries and the message snapshot's epoch.
struct Probe {
    seen: Arc<Seen>,
    log: Arc<Mutex<Vec<(usize, u64)>>>,
}

impl NetHandler for Probe {
    fn commands(&self) -> &'static [CommandString] {
        &QSIGREC
    }

    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        let n = self.seen.checked.load(Ordering::SeqCst);
        self.log.lock().unwrap().push((n, cx.snap.epoch));
        Ok(())
    }
}

struct Rig {
    sim: Sim,
    seen: Arc<Seen>,
    log: Arc<Mutex<Vec<(usize, u64)>>>,
}

fn rig() -> Rig {
    let seen = Arc::new(Seen::default());
    let shared = Arc::new(OnceLock::new());
    let log = Arc::default();
    let mut registry = Registry::new();
    registry.register(Box::new(Probe {
        seen: Arc::clone(&seen),
        log: Arc::clone(&log),
    }));
    let sim = sim_with(registry, Recording::new(&seen, &shared));
    shared.set(Arc::clone(&sim.shared)).ok().unwrap();
    Rig {
        sim,
        seen,
        log,
    }
}

/// V's push of a valid `BlockChecked` and the publication after it (C7 (a)).
fn v_signal(sim: &Sim) {
    sim.shared.block_checked(BlockHash::from_lendian([9; 32]), None, false);
    sim.chain.republish();
}

#[test]
fn every_message_reads_a_drained_snapshot_with_its_eligible_signals_applied() {
    // I0 (D46): a publication lands between the last drain and the message's start; the
    // message-start drain applies its entry before the handler runs, and the handler reads
    // that drain's snapshot.
    let mut r = rig();
    let p = outbound(&mut r.sim, 1);
    v_signal(&r.sim);
    assert_eq!(r.seen.checked.load(Ordering::SeqCst), 0);
    r.sim.deliver(&p, &[("qsigrec", vec![])]);
    r.sim.lp.logic_mut().process_messages(&p.handle);
    assert_eq!(*r.log.lock().unwrap(), vec![(1, r.sim.chain.view_seq())]);
}

#[test]
fn the_headers_continuation_reads_the_post_reply_drain() {
    // D46: another origin's `BlockChecked` lands with the `ProcessHeaders` reply's own
    // publication; the continuation runs after the redrain applied it, on that drain's snapshot.
    let r = rig();
    let mut sim = r.sim;
    let p = outbound(&mut sim, 1);
    let shared = Arc::clone(&sim.shared);
    let armed = Arc::new(AtomicBool::new(true));
    let a = Arc::clone(&armed);
    sim.chain.set_between_stores(Some(Arc::new(move || {
        if a.swap(false, Ordering::SeqCst) {
            shared.block_checked(BlockHash::from_lendian([7; 32]), None, false);
        }
    })));
    let hs = SimChain::build(&sim.chain.genesis(), 1, 150, 3);
    let mut payload = Vec::new();
    Headers {
        headers: hs,
    }
    .encode(&mut payload);
    sim.deliver(&p, &[("headers", payload)]);
    sim.lp.logic_mut().process_messages(&p.handle);
    assert!(!armed.load(Ordering::SeqCst));
    assert_eq!(*r.seen.direct_fetch.lock().unwrap(), vec![(1, sim.chain.view_seq())]);
}

/// Stores an announce stamp the current view does not show, and publishes it from another
/// thread, as V's accept would, once a caller is waiting for it: the `Ahead` is certain.
fn stamp_ahead(sim: &Sim) -> std::thread::JoinHandle<()> {
    let stamp = sim.chain.view_seq() + 1;
    sim.shared.signals().store_announce_stamp(stamp);
    let chain = Arc::clone(&sim.chain);
    let waits = chain.published_waits();
    std::thread::spawn(move || {
        until(|| chain.published_waits() > waits);
        chain.republish();
    })
}

#[test]
fn a_message_start_behind_the_announce_stamp_waits_and_retries_once() {
    let mut r = rig();
    let p = outbound(&mut r.sim, 1);
    let before = r.sim.lp.logic().region_retries;
    r.sim.deliver(&p, &[("qsigrec", vec![])]);
    let v = stamp_ahead(&r.sim);
    r.sim.lp.logic_mut().process_messages(&p.handle);
    v.join().unwrap();
    assert_eq!(r.sim.lp.logic().region_retries, before + 1);
    // The handler ran once, on the publication it waited for.
    assert_eq!(
        r.log.lock().unwrap().iter().map(|e| e.1).collect::<Vec<_>>(),
        vec![r.sim.chain.view_seq()]
    );
}

#[test]
fn the_head_of_send_messages_keeps_its_moved_list_across_an_ahead_retry() {
    let mut r = rig();
    let p = outbound(&mut r.sim, 1);
    r.sim.deliver(&p, &[("sendheaders", vec![])]);
    r.sim.pass();
    let g = r.sim.chain.genesis();
    r.sim.shared.with_state_mut(p.handle.id(), |s| s.headers_mut().best_known_block = Some(g.id));
    let hs = SimChain::build(&g, 1, 150, 7);
    let tip = r.sim.chain.add(&hs);
    r.sim.chain.set_tip(tip.id);
    r.sim.shared.updated_block_tip(&r.sim.core, &[tip.hash], false);
    let before = r.sim.lp.logic().region_retries;
    let v = stamp_ahead(&r.sim);
    r.sim.lp.logic_mut().send_messages(&p.handle);
    v.join().unwrap();
    assert_eq!(r.sim.lp.logic().region_retries, before + 1);
    let sent = r.sim.sent(&p);
    assert_eq!(cmds(&sent), vec!["headers"]);
    assert_eq!(r.sim.shared.peek_headers(p.handle.id()).unwrap().best_header_sent, Some(tip.id));
}

#[test]
fn getpeerinfo_waits_out_an_ahead_and_resolves_on_the_new_view() {
    let mut r = rig();
    let p = outbound(&mut r.sim, 1);
    let waits = r.sim.chain.published_waits();
    let v = stamp_ahead(&r.sim);
    let stamp = r.sim.chain.view_seq() + 1;
    assert!(r.sim.shared.node_state_stats(&p.handle).is_some());
    // It waited once, and returned only once the stamp's publication was there.
    assert_eq!(r.sim.chain.published_waits(), waits + 1);
    assert!(r.sim.chain.view_seq() >= stamp);
    v.join().unwrap();
}

/// Polls `w` once.
fn poll_now(w: &mut BlockWait) -> Option<Result<BlockOutcome, Interrupted>> {
    match w.0.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(r) => Some(r),
        Poll::Pending => None,
    }
}

/// Polls `w` until it is ready or `ms` pass.
fn ready_within(w: &mut BlockWait, ms: u64) -> Option<Result<BlockOutcome, Interrupted>> {
    let deadline = std::time::Instant::now() + Duration::from_millis(ms);
    loop {
        if let Some(r) = poll_now(w) {
            return Some(r);
        }
        if std::time::Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
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

#[test]
fn getpeerinfo_does_not_block_while_the_actor_waits_on_process_block() {
    let mut r = rig();
    let p = outbound(&mut r.sim, 1);
    r.sim.chain.spawn_v();
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    r.sim.chain.set_accept_hook(Some(Arc::new(move |_: &BlockSubmit| {
        entered_tx.send(()).unwrap();
        release_rx.lock().unwrap().recv().unwrap();
    })));
    let block = &SimChain::build_blocks(&r.sim.chain.genesis(), 1, 150, 3)[0];
    r.sim.deliver(&p, &[("block", block_msg(block))]);
    let shared = Arc::clone(&r.sim.shared);
    let handle = p.handle.clone();
    std::thread::scope(|s| {
        let sim = &mut r.sim;
        let actor = s.spawn(move || sim.pass());
        entered.recv_timeout(Duration::from_secs(30)).expect("V got the block");
        let (tx, rx) = mpsc::channel();
        s.spawn(move || tx.send(shared.node_state_stats(&handle)).unwrap());
        let stats = rx.recv_timeout(Duration::from_secs(10)).expect("getpeerinfo answered");
        assert!(stats.is_some());
        release.send(()).unwrap();
        actor.join().unwrap();
    });
    assert_eq!(r.sim.chain.drain_snapshot().0.tip.tip_hash, block.hash());
}

#[test]
fn c5_prime_defers_an_accept_until_every_signal_is_processed() {
    let mut r = rig();
    outbound(&mut r.sim, 1);
    r.sim.chain.spawn_v();
    r.sim.chain.set_ibd(false);
    v_signal(&r.sim);
    let blocks = SimChain::build_blocks(&r.sim.chain.genesis(), 2, 150, 3);
    let mut w = r.sim.chain.process_block(submit(&blocks[0])).unwrap();
    // The entry is eligible but the actor has not drained it: V waits.
    assert!(ready_within(&mut w, 200).is_none());
    r.sim.lp.drain_sync_signals();
    assert!(matches!(ready_within(&mut w, 10_000), Some(Ok(o)) if o.new_block));
    // In IBD, V does not wait.
    r.sim.chain.set_ibd(true);
    v_signal(&r.sim);
    let mut w = r.sim.chain.process_block(submit(&blocks[1])).unwrap();
    assert!(matches!(ready_within(&mut w, 10_000), Some(Ok(_))));
    // Held undrained: the first block's `BlockChecked`, the push and the second's.
    assert_eq!(r.sim.shared.signals().queue.held().0, 3);
}

#[test]
fn c5_prime_ends_when_the_queue_closes() {
    let mut r = rig();
    outbound(&mut r.sim, 1);
    r.sim.chain.spawn_v();
    r.sim.chain.set_ibd(false);
    v_signal(&r.sim);
    let block = &SimChain::build_blocks(&r.sim.chain.genesis(), 1, 150, 3)[0];
    let mut w = r.sim.chain.process_block(submit(block)).unwrap();
    assert!(ready_within(&mut w, 200).is_none());
    r.sim.shared.signals().queue.close();
    assert!(ready_within(&mut w, 10_000).is_some());
}

#[test]
#[should_panic(expected = "R12")]
fn a_call_that_waits_on_v_under_a_header_guard_panics() {
    let sim = rig().sim;
    let cell = HeaderCell::default();
    let _guard = sim.shared.lock_headers(&cell, u64::MAX, |_| ()).unwrap();
    let _ = sim.chain.wait_published(1);
}

#[test]
fn a_drain_between_the_two_cells_neither_shows_the_new_tip_nor_applies_its_signals() {
    // D47: the tip cell is stored, then the `ChainView`; a drain in between reads the view,
    // and `status` the status published with it (P3 §2.4; Sol S1 r1 F1): never B connected
    // with tip A, which no Core `cs_main` section shows (np:3202,3205 against v:3471,3761).
    let mut r = rig();
    outbound(&mut r.sim, 1);
    r.sim.chain.spawn_v();
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let armed = Arc::new(AtomicBool::new(false));
    let a = Arc::clone(&armed);
    r.sim.chain.set_between_stores(Some(Arc::new(move || {
        if a.swap(false, Ordering::SeqCst) {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        }
    })));
    let old = r.sim.chain.drain_snapshot().0.tip.tip_hash;
    let block = &SimChain::build_blocks(&r.sim.chain.genesis(), 1, 150, 3)[0];
    // B is indexed (header only) before its accept.
    let b = r.sim.chain.add(&[*block.header().raw()]).id;
    armed.store(true, Ordering::SeqCst);
    let mut w = r.sim.chain.process_block(submit(block)).unwrap();
    entered.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!(r.sim.chain.snapshot().tip.tip_hash, block.hash());
    let calls = r.sim.chain.snapshot_calls();
    let checked = r.seen.checked.load(Ordering::SeqCst);
    let obs = r.sim.lp.logic_mut().drain_sync_signals();
    assert_eq!(obs.snap.tip.tip_hash, old);
    assert_eq!(r.seen.checked.load(Ordering::SeqCst), checked);
    assert_ne!(
        r.sim.chain.status(&obs.snap, b).unwrap().validity,
        Validity::Scripts,
        "the old view exposed B's connected status"
    );
    assert_eq!(r.sim.chain.valid_scripts(&obs.snap, b), Ok(false));
    // No drain point reads the tip cell: a whole pass makes no `snapshot()` call (with no
    // getdata queued: `process_getdata` reads the tip cell for serving, which is not a drain).
    r.sim.pass_with(&mut FastRandom::deterministic());
    assert_eq!(r.sim.chain.snapshot_calls(), calls);
    release.send(()).unwrap();
    assert!(ready_within(&mut w, 10_000).is_some());
    // BL-236: the snapshot held across the publication still reads its own view's status; a
    // drain after it reads B connected.
    assert_ne!(r.sim.chain.status(&obs.snap, b).unwrap().validity, Validity::Scripts);
    assert_eq!(r.sim.chain.valid_scripts(&obs.snap, b), Ok(false));
    let new = r.sim.chain.drain_snapshot().0;
    assert_eq!(r.sim.chain.status(&new, b).unwrap().validity, Validity::Scripts);
    assert_eq!(r.sim.chain.valid_scripts(&new, b), Ok(true));
    let obs = r.sim.lp.logic_mut().drain_sync_signals();
    assert_eq!(obs.snap.tip.tip_hash, block.hash());
    assert_eq!(r.seen.checked.load(Ordering::SeqCst), checked + 1);
    assert_eq!(r.sim.chain.status(&obs.snap, b).unwrap().validity, Validity::Scripts);
}

#[test]
fn no_block_source_entry_exists_before_the_first_drain() {
    // DEC-230's reachability (D36): the queue is `PreStart` until the first drain, and every
    // `mapBlockSource` insertion happens with it `Live` (each runs inside a message, after that
    // message's own drain).
    let mut r = rig();
    assert!(!r.sim.shared.signals().queue.is_live());
    let p = outbound(&mut r.sim, 1);
    assert!(r.sim.shared.signals().queue.is_live());
    let blocks = SimChain::build_blocks(&r.sim.chain.genesis(), 2, 150, 3);
    for b in &blocks {
        r.sim.deliver(&p, &[("block", block_msg(b))]);
        r.sim.pass();
    }
    assert_eq!(*r.seen.source_live.lock().unwrap(), vec![true, true]);
}

#[test]
fn an_invalid_block_from_two_peers_punishes_each_before_its_tail() {
    // S2-f's invalid branch (P3 §3.7): the block is never new, each delivery's `BlockChecked`
    // finds that delivery's source before the tail's erase.
    let mut r = rig();
    let a = outbound(&mut r.sim, 1);
    let q = outbound(&mut r.sim, 2);
    let block = &SimChain::build_blocks(&r.sim.chain.genesis(), 1, 150, 3)[0];
    r.sim.chain.mark_bad_block(block.hash());
    for p in [&q, &a] {
        r.sim.deliver(p, &[("block", block_msg(block))]);
        r.sim.pass();
        assert!(p.handle.disconnect_requested());
    }
    assert_eq!(r.seen.checked.load(Ordering::SeqCst), 2);
}

/// The `message` field of every event on the given targets, in emission order.
struct Lines(&'static [&'static str], Mutex<Vec<String>>);

impl tracing::Subscriber for Lines {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        meta.is_event() && self.0.contains(&meta.target())
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
        self.1.lock().unwrap().push(line);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// The only test in this binary that reaches the "Block mutated" callsite: `tracing` caches a
/// callsite's interest globally, so one first reached on a thread with no subscriber could stay
/// disabled for this test's thread-local one.
#[test]
fn a_mutated_block_scores_100_logs_cores_line_and_reaches_no_chain() {
    let mut r = rig();
    let p = outbound(&mut r.sim, 1);
    let block = &SimChain::build_blocks(&r.sim.chain.genesis(), 1, 150, 3)[0];
    // A second transaction the header's root does not commit to (`bad-txnmrklroot`).
    let mut txs = block.txs().to_vec();
    txs.push(Arc::clone(&txs[0]));
    let mutated = Block::new(*block.header(), txs);
    let lines = Arc::new(Lines(&["validation", "net"], Mutex::default()));
    let _log = tracing::subscriber::set_default(Arc::clone(&lines));
    r.sim.deliver(&p, &[("block", block_msg(&mutated))]);
    r.sim.pass();
    assert!(p.handle.disconnect_requested());
    assert!(r.sim.chain.submits().is_empty());
    // Core: np:5348-5366, with validation.cpp:4434-4440 IsBlockMutated between: the received
    // line, then the VALIDATION line with the state as `ValidationState::ToString` of
    // CheckMerkleRoot's first failure (validation.cpp:4324), then the NET line. Every block
    // test reaches the received line's callsite, so `tracing`'s global interest cache may
    // hide it here; its order is checked only when it was captured.
    let lines = lines.1.lock().unwrap();
    let id = p.handle.id();
    let at = |w: &str| lines.iter().position(|l| l == w);
    let mutated_at = at("Block mutated: bad-txnmrklroot, hashMerkleRoot mismatch");
    let net_at = at(&format!("Received mutated block from peer={id}"));
    let received_at = at(&format!("received block {} peer={id}", mutated.hash()));
    assert!(mutated_at.is_some() && mutated_at < net_at, "VALIDATION then NET in {lines:?}");
    assert!(received_at.is_none_or(|r| Some(r) < mutated_at), "received first in {lines:?}");
    assert_eq!(lines.iter().filter(|l| l.starts_with("Block mutated")).count(), 1);
}

/// A listener over `Shared` that drives the block wait's two wake-ups with no clock. Each step
/// waits on a state of the actor's doorbell ([`Bells::is_parked`]): once the actor is asleep
/// again after a ring, it has run its loop since. A stuck actor is therefore a state, not a
/// timeout: the probe records it and then frees the actor, so a mutant fails instead of hanging.
///
/// [`Bells::is_parked`]: dashd_p2p::processing::signals::Bells::is_parked
struct WakeProbe {
    shared: Arc<Shared>,
    chain: Arc<SimChain>,
    /// At C5′, first play another origin's accept: its `BlockChecked`, then its publication.
    other_origin: AtomicBool,
    /// What the probe found stuck.
    stuck: Mutex<Option<&'static str>>,
}

impl WakeProbe {
    fn stuck(&self, what: &'static str) {
        self.stuck.lock().unwrap().get_or_insert(what);
    }
}

impl SimListener for WakeProbe {
    fn block_checked(&self, hash: BlockHash, invalid: Option<BlockValidationError>, ibd: bool) {
        self.shared.block_checked(hash, invalid, ibd);
    }

    /// After its ring, holds V until the actor has woken on it, drained, polled the pending
    /// reply and slept again, so the reply that follows is stored onto a sleeping actor and its
    /// waker is the only wake-up left.
    fn published(&self) {
        let bells = &self.shared.signals().bells;
        bells.ring_all();
        bells.wait_parked();
    }

    fn wait_quiescent(&self) -> Result<(), Interrupted> {
        if self.other_origin.swap(false, Ordering::SeqCst) {
            // V holds P's command, so the actor is in its block wait. Pushing X only once it is
            // asleep there puts any drain on entry behind it: only a drain after a ring takes X.
            self.shared.signals().bells.wait_parked();
            self.shared.block_checked(BlockHash::from_lendian([9; 32]), None, false);
            self.chain.republish();
        }
        self.shared.signals().queue.wait_quiescent(&mut DrainedOrStuck(self))
    }
}

/// C5′'s wait as V would serve it, judged once the actor is asleep. `wait_quiescent` rings before
/// it calls this, so by then the actor has woken on that ring and run its loop. With the in-wait
/// drain the outstanding entry is then processed; without it, neither side will ever move.
struct DrainedOrStuck<'a>(&'a WakeProbe);

impl ServiceWait for DrainedOrStuck<'_> {
    fn wait(&mut self, _: &WaitEvent, done: &mut dyn FnMut() -> bool) -> Result<(), Interrupted> {
        self.0.shared.signals().bells.wait_parked();
        if done() {
            return Ok(());
        }
        self.0
            .stuck("the actor slept in its block wait with a signal undrained, and V waits on it");
        Err(Interrupted)
    }
}

/// P's `block(B)` outside IBD with V on its own thread under [`WakeProbe`]'s schedule. The actor's
/// pass runs on a scoped thread; returns what the probe found stuck, the tip after the pass, and
/// B's hash.
fn wake_case(other_origin: bool) -> (Option<&'static str>, BlockHash, BlockHash) {
    let mut r = rig();
    let p = outbound(&mut r.sim, 1);
    r.sim.chain.spawn_v();
    r.sim.chain.set_ibd(false);
    let probe = Arc::new(WakeProbe {
        shared: Arc::clone(&r.sim.shared),
        chain: Arc::clone(&r.sim.chain),
        other_origin: AtomicBool::new(other_origin),
        stuck: Mutex::default(),
    });
    let listener: Arc<dyn SimListener> = probe.clone();
    r.sim.chain.set_listener(Arc::downgrade(&listener) as Weak<dyn SimListener>);
    let watch = Arc::clone(&probe);
    r.sim.chain.set_reply_hook(Some(Arc::new(move || {
        // V stored the reply onto a sleeping actor (`published`) and its waker has run, so an
        // actor still asleep with no ring pending missed it.
        let bells = &watch.shared.signals().bells;
        if bells.is_parked() {
            watch.stuck("the reply was stored without waking the block wait");
            bells.ring();
        }
    })));
    let block = SimChain::build_blocks(&r.sim.chain.genesis(), 1, 150, 3)[0].clone();
    r.sim.deliver(&p, &[("block", block_msg(&block))]);
    std::thread::scope(|s| {
        s.spawn(|| r.sim.pass());
    });
    r.sim.chain.set_reply_hook(None);
    let tip = r.sim.chain.drain_snapshot().0.tip.tip_hash;
    let stuck = *probe.stuck.lock().unwrap();
    (stuck, tip, block.hash())
}

/// P3 §2.6 (C1, the in-wait drain), Opus S1 r1 F1's schedule A. V takes P's `ProcessBlock(B)`
/// outside IBD, but another origin's `BlockChecked(X)` is published first, so C5′ waits. Only
/// the actor's drain inside its block wait can release it.
#[test]
fn another_origins_signal_at_c5_prime_is_drained_inside_the_block_wait() {
    let (stuck, tip, b) = wake_case(true);
    assert_eq!(stuck, None);
    assert_eq!(tip, b);
}

/// P3 §2.6 ("no wakeup is lost"), Opus S1 r1 F1's schedule B. The actor wakes on the
/// publication's ring and polls before V stores the reply, so the reply's waker is its only
/// wake-up, as when CH1-node's `ActorReply` closes before the signals thread's ring. (Schedule A
/// ends the same way, so it also depends on the waker.)
#[test]
fn the_reply_alone_wakes_the_block_wait() {
    let (stuck, tip, b) = wake_case(false);
    assert_eq!(stuck, None);
    assert_eq!(tip, b);
}

#[test]
fn sim_chain_skips_the_merkle_pass_only_when_the_submitter_ran_it() {
    let sim = rig().sim;
    let blocks = SimChain::build_blocks(&sim.chain.genesis(), 2, 150, 3);
    sim.chain.accept_block(&submit(&blocks[0]));
    assert_eq!(sim.chain.merkle_passes(), 1);
    sim.chain.accept_block(&BlockSubmit {
        merkle_checked: true,
        ..submit(&blocks[1])
    });
    assert_eq!(sim.chain.merkle_passes(), 1);
    assert_eq!(sim.chain.drain_snapshot().0.tip.tip_hash, blocks[1].hash());
}

#[test]
fn nine_appends_revert_section_7_to_an_inv_of_the_last() {
    // D44: past `MAX_BLOCKS_TO_ANNOUNCE` the relay list keeps the last hash only, and section 7
    // reverts to an inv of it, as Core does with the whole vector.
    let mut r = rig();
    let p = outbound(&mut r.sim, 1);
    r.sim.deliver(&p, &[("sendheaders", vec![])]);
    r.sim.pass();
    let g = r.sim.chain.genesis();
    r.sim.shared.with_state_mut(p.handle.id(), |s| s.headers_mut().best_known_block = Some(g.id));
    let hs = SimChain::build(&g, 9, 150, 7);
    let tip = r.sim.chain.add(&hs);
    r.sim.chain.set_tip(tip.id);
    let mut hashes = r.sim.chain.new_blocks(tip.id, Some(g.id));
    hashes.reverse();
    for h in &hashes {
        r.sim.shared.updated_block_tip(&r.sim.core, &[*h], false);
    }
    r.sim.pass();
    let sent = r.sim.sent(&p);
    assert_eq!(cmds(&sent), vec!["inv"]);
    let inv = Inv::decode(&mut &sent[0].1[..]).unwrap();
    assert_eq!(inv.inventory.len(), 1);
    assert_eq!(inv.inventory[0].hash.as_bytes(), tip.hash.as_bytes());
}
