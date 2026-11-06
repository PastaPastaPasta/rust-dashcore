//! BL-503 N1: `NetSlot::fetch_block` on a regtest node with real V, the processor's own thread
//! and a scripted loopback peer, mirroring `rpc_getblockfrompeer.py:44-86` through the adapter.
//! The assertions are Core's (each names its lines); BL-503's transcripts hold the
//! dashd-recorded comparison.

use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dash_consensus::{Block, BlockId};
use dash_p2p::CommandString;
use dash_p2p_core::{GetData, InvType};
use dash_primitives::BlockHash;
use dash_types::codec::BaseCodec as _;
use dashd_chainstate::chain::genesis_block;
use dashd_p2p::processing::fetch::FetchBlockError;
use dashd_p2p::processing::seams::{ChainAccess, TreeRead};
use dashd_p2p::processing::sync::block_hash;
use dashd_rpc::peers::NodeNet;

use super::amb::{PATIENCE, Peer, branch, headers_msg, node, regtest};
use super::chain::NodeChain;
use crate::node::Node;
use crate::rpc::NetSlot;

/// The wall time an answer may take once the processor is interrupted, on a loaded box: a
/// request made after the interrupt answers at once, and the actor's exit drain waits on no
/// socket, block or V (P2a-net D51).
const STOPPED_ANSWER: Duration = Duration::from_secs(5);

/// `f` on another thread: its answer, or `None` when it takes longer than `limit`.
fn within<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    rx.recv_timeout(limit).ok()
}

/// Polls `cond` until it holds, within [`PATIENCE`].
fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !cond() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The block entries of every `getdata` in `frames`.
fn getdata_blocks(frames: &[(CommandString, Vec<u8>)]) -> Vec<Vec<(InvType, BlockHash)>> {
    frames
        .iter()
        .filter(|(c, _)| *c == CommandString::GETDATA)
        .map(|(_, p)| {
            let gd = GetData::decode(&mut &p[..]).expect("a getdata");
            gd.inventory.iter().map(|i| (i.inv_type, block_hash(i))).collect()
        })
        .collect()
}

/// The scripted peer's frames until one is a `getdata`; every `getdata` among them.
fn next_getdata(peer: &mut Peer) -> Vec<Vec<(InvType, BlockHash)>> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let got = getdata_blocks(&peer.poll());
        if !got.is_empty() {
            return got;
        }
        assert!(Instant::now() < deadline, "no getdata");
    }
}

/// The scripted peer's frames until the node has answered two `ping`s in turn. The node
/// queues a `pong` while processing the peer's messages and runs its `SendMessages` for the peer
/// later in the same pass, so the second `pong` follows one whole pass after the first.
fn frames_through_two_pings(peer: &mut Peer) -> Vec<(CommandString, Vec<u8>)> {
    let pong = CommandString::from_static("pong");
    let mut frames = Vec::new();
    for nonce in [0xb503_0001_u64, 0xb503_0002] {
        peer.send("ping", &nonce.to_le_bytes());
        let deadline = Instant::now() + PATIENCE;
        loop {
            let got = peer.poll();
            let done = got.iter().any(|(c, p)| *c == pong && p[..] == nonce.to_le_bytes());
            frames.extend(got);
            if done {
                break;
            }
            assert!(Instant::now() < deadline, "no pong");
        }
    }
    frames
}

/// A regtest node on chain A (four blocks) whose one peer, scripted, announced a three-block
/// fork F off genesis with less work by `headers`: V holds F's headers only
/// (rpc_getblockfrompeer.py:57-60).
struct Forked {
    node: Node,
    peer: Peer,
    chain: NodeChain,
    chain_a: Vec<Arc<Block>>,
    fork: Vec<Arc<Block>>,
    f3: BlockHash,
    f3_id: BlockId,
    _dir: tempfile::TempDir,
}

impl Forked {
    fn start() -> Forked {
        let params = regtest();
        let genesis = *genesis_block(&params).header();
        let chain_a = branch(&params, genesis, 0, &[1, 1, 1, 1], 0x0a);
        let fork = branch(&params, genesis, 0, &[1, 1, 1], 0x0f);
        let f3 = fork[2].hash();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let connect = format!("-connect={}", listener.local_addr().expect("addr"));
        let dir = tempfile::tempdir().unwrap();
        let node = node(
            dir.path(),
            &[&connect, "-dnsseed=0", "-fixedseeds=0", "-v2transport=0", "-debug=net"],
        );
        let v = node.chain().expect("V runs").handle().clone();
        for b in &chain_a {
            v.process_block(Arc::clone(b), true, None, true).recv().expect("V");
        }
        let chain = NodeChain::new(v, None);
        until("V took chain A", || chain.snapshot().tip.tip_hash == chain_a[3].hash());

        let now = SystemTime::now().duration_since(UNIX_EPOCH).expect("now").as_secs() as i64;
        let mut peer = Peer::dialled(&listener, &params, fork.len() as i32, now);
        let deadline = Instant::now() + PATIENCE;
        while !peer.poll().iter().any(|(c, _)| *c == CommandString::from_static("getheaders")) {
            assert!(Instant::now() < deadline, "no getheaders");
        }
        peer.send("headers", &headers_msg(&fork));
        until("V has F3's header", || chain.lookup(&f3).is_some());
        let f3_id = chain.lookup(&f3).expect("indexed");
        assert!(!chain.status(&chain.snapshot(), f3_id).expect("a status").have_data);
        Forked {
            node,
            peer,
            chain,
            chain_a,
            fork,
            f3,
            f3_id,
            _dir: dir,
        }
    }
}

fn only_peer_id(slot: &NetSlot) -> i64 {
    let infos = slot.peer_info().expect("attached");
    let [info] = &infos[..] else {
        panic!("one peer: {} listed", infos.len())
    };
    info.id
}

#[test]
fn n1_fetch_block_through_the_slot() {
    let Forked {
        _dir,
        mut node,
        mut peer,
        chain,
        chain_a,
        fork,
        f3,
        f3_id,
    } = Forked::start();
    let tip = || chain.snapshot().tip.tip_hash;
    let have_data = || chain.status(&chain.snapshot(), f3_id).expect("a status").have_data;
    let slot = node.rpc_net();
    let id = only_peer_id(&slot);
    // rpc_getblockfrompeer.py:75-78: `{}`, and one `getdata(MSG_BLOCK F3)`, none for F1 or F2.
    assert_eq!(slot.fetch_block(&f3, id), Some(Ok(())));
    assert_eq!(next_getdata(&mut peer), [[(InvType::Block, f3)]]);
    let later = getdata_blocks(&frames_through_two_pings(&mut peer));
    assert!(later.is_empty(), "no other getdata after a whole pass: {later:?}");
    // The block is stored although its parents have no data (AcceptBlock's `fRequested`,
    // validation.cpp:4815-4818), and the tip stays.
    peer.send("block", &dash_consensus::core_serialize(&*fork[2]));
    until("V stored F3", have_data);
    assert_eq!(tip(), chain_a[3].hash());
    // "Block already downloaded" (rb:524-527) is the RPC's check, not FetchBlock's.
    assert_eq!(slot.fetch_block(&f3, id), Some(Ok(())));
    assert_eq!(next_getdata(&mut peer), [[(InvType::Block, f3)]]);
    // rpc_getblockfrompeer.py:68-73: an unknown peer id.
    for bad in [-1, id + 1] {
        assert_eq!(slot.fetch_block(&f3, bad), Some(Err("Peer does not exist".into())), "{bad}");
    }
    // `EnsurePeerman` (rb:507): no peer manager before the attach.
    assert_eq!(NetSlot::default().fetch_block(&f3, id), None);
    let call = |slot: &Arc<NetSlot>| {
        let slot = Arc::clone(slot);
        within(STOPPED_ANSWER, move || slot.fetch_block(&f3, id))
    };
    // P2a-net D51: once the processor is interrupted (its thread not yet joined), requests
    // queued before it and made after it get `FetchBlock`'s answer within `STOPPED_ANSWER`:
    // `Ok` and the `getdata` for the peer, which never answers it, and "Peer does not exist"
    // for an id with no peer. Once the network stops, a request answers `None` at once.
    let shared = node.net_shared().expect("attached");
    let (slow, dead) = (shared.request_fetch(f3, id), shared.request_fetch(f3, id + 1));
    shared.interrupt();
    assert_eq!(call(&slot), Some(Some(Ok(()))), "made after the interrupt");
    assert_eq!(within(STOPPED_ANSWER, move || slow.wait()), Some(Ok(())), "a slow peer");
    let dead = within(STOPPED_ANSWER, move || dead.wait());
    assert_eq!(dead, Some(Err(FetchBlockError::PeerDoesNotExist)), "no peer");
    let mut sent = next_getdata(&mut peer);
    if sent.len() < 2 {
        sent.extend(next_getdata(&mut peer));
    }
    assert_eq!(sent, [[(InvType::Block, f3)], [(InvType::Block, f3)]], "slow, then the call");
    node.stop_net();
    assert_eq!(call(&slot), Some(None), "stopped");
    slot.detach();
    assert_eq!(call(&slot), Some(None), "detached");
}

/// N-T: the RPC's path, `NetSlot` as `&dyn NodeNet`, passes the peer id through unchanged:
/// `-1` and `i64::MIN` are no peer while peer 0 is live, and 0 is that peer.
// Core: rpc/blockchain.cpp:510, :528; net_processing.cpp:2062-2063
#[test]
fn nt_the_trait_path_passes_the_peer_id_through() {
    let mut forked = Forked::start();
    let f3 = forked.f3;
    let slot = forked.node.rpc_net();
    assert_eq!(only_peer_id(&slot), 0);
    let net: &dyn NodeNet = &*slot;
    let no_peer = Some(Err("Peer does not exist".to_owned()));
    assert_eq!(net.fetch_block(&f3, -1), no_peer);
    let bounded = Arc::clone(&slot);
    let min = within(STOPPED_ANSWER, move || {
        let net: &dyn NodeNet = &*bounded;
        net.fetch_block(&f3, i64::MIN)
    });
    assert_eq!(min, Some(no_peer), "i64::MIN");
    let none = getdata_blocks(&frames_through_two_pings(&mut forked.peer));
    assert!(none.is_empty(), "no getdata for a missing peer: {none:?}");
    assert_eq!(net.fetch_block(&f3, 0), Some(Ok(())));
    assert_eq!(next_getdata(&mut forked.peer), [[(InvType::Block, f3)]]);
    assert_eq!(NodeNet::fetch_block(&NetSlot::default(), &f3, 0), None);
}

/// N2 (P2a-net D51, Opus r3 R3-1): out of IBD, V's block accepts made after `Node::interrupt`
/// and before `Net::stop` complete, as an RPC `submitblock` waits on the same reply with no
/// timeout and `rpc.stop()` runs before `Net::stop`.
#[test]
fn n2_v_accepts_after_the_net_interrupt() {
    let params = regtest();
    let genesis = *genesis_block(&params).header();
    let now = || SystemTime::now().duration_since(UNIX_EPOCH).expect("now").as_secs();
    let start = u32::try_from(now() - 7200).expect("a u32 time");
    let chain_a = super::amb::branch_from(&params, genesis, 6, 0x0a, start);
    let dir = tempfile::tempdir().unwrap();
    let mut node = node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0"]);
    let v = node.chain().expect("V runs").handle().clone();
    for b in &chain_a[..3] {
        v.process_block(Arc::clone(b), true, None, true).recv().expect("V");
    }
    let chain = NodeChain::new(v.clone(), None);
    until("V took 3", || chain.snapshot().tip.tip_hash == chain_a[2].hash());
    // Startup's import clears `fImporting` on its own thread, after V may have taken the blocks.
    until("out of IBD", || {
        let at = dash_clock::NodeSeconds(i64::try_from(now()).expect("an i64 time"));
        !chain.is_initial_block_download(&chain.snapshot(), at)
    });
    assert!(node.net_shared().is_some(), "attached");
    node.interrupt();
    // Let the actor leave its loop before the accepts.
    std::thread::sleep(Duration::from_millis(300));
    let rest = chain_a[3..].to_vec();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        for b in rest {
            let _ = tx.send(v.process_block(b, true, None, true).recv().map(|r| r.accepted));
        }
    });
    let deadline = Instant::now() + STOPPED_ANSWER;
    let mut got = Vec::new();
    while got.len() < 3 {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(r) => got.push(r),
            Err(_) => break,
        }
    }
    node.stop_net();
    worker.join().expect("the worker");
    assert_eq!(got, [Ok(true), Ok(true), Ok(true)], "accepts after the net interrupt");
}
