//! P6b in `sim`: Core's `getmnlistd` branch (net_processing.cpp:5629-5654) and
//! `BuildSimplifiedMNListDiff`'s request checks (evo/smldiff.cpp:160-193) through
//! `EvoServer::process_message`, over a fake `EvoServe`: each refusal's score and log line, no
//! reply on a refusal, and the source's bytes as the `mnlistdiff` reply at the peer's version.

use std::sync::{Arc, Mutex};

use dash_consensus::Block;
use dash_p2p::CommandString;
use dash_p2p::conn::Permissions;
use dash_p2p::policy::Throw;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::BlockHash;
use dash_types::Numeric;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::EntryFacts;
use dashd_p2p::processing::serve_mnlistdiff::{EvoServe, ServeRefusal};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimChain, SimPeer};
use dashd_p2p::{Registry, Seams};

mod log_capture;

/// The peer's version: below the node's, so it is the common version.
const PEER_VERSION: i32 = 70228;

type Calls = Arc<Mutex<Vec<(BlockHash, BlockHash, i32)>>>;

/// A source that records each call and answers with `reply`.
struct FakeEvo {
    calls: Calls,
    reply: Result<Option<Vec<u8>>, ServeRefusal>,
}

impl EvoServe for FakeEvo {
    fn mnlistdiff(
        &mut self,
        base: BlockHash,
        block: BlockHash,
        proto: i32,
    ) -> Result<Option<Vec<u8>>, ServeRefusal> {
        self.calls.lock().unwrap().push((base, block, proto));
        self.reply.clone()
    }
}

struct Node {
    sim: Sim,
    peer: SimPeer,
    calls: Calls,
    /// Genesis, then the three active blocks.
    main: Vec<EntryFacts>,
    /// A stored two-block branch off genesis, not active.
    side: Vec<EntryFacts>,
}

fn node(reply: Result<Option<Vec<u8>>, ServeRefusal>) -> Node {
    let calls = Calls::default();
    let source = FakeEvo {
        calls: Arc::clone(&calls),
        reply,
    };
    node_with(Registry::new_with_evo(Box::new(source)), calls)
}

fn node_with(registry: Registry, calls: Calls) -> Node {
    let p = Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let mut sim = Sim::with(p, id, ProcessorOptions::default(), registry, Seams::default(), |_| {});
    let genesis = sim.chain.genesis();
    let blocks = SimChain::build_blocks(&genesis, 3, 1, 0);
    let tip = sim.chain.store_blocks(&blocks);
    let side_blocks = SimChain::build_blocks(&genesis, 2, 1, 7);
    sim.chain.store_blocks(&side_blocks);
    sim.chain.set_tip(tip.id);
    sim.chain.set_ibd(false);
    sim.clock.set_mock_time(i64::from(tip.time) + 1);
    let entry = |b: &Arc<Block>| sim.chain.by_hash(&b.hash()).unwrap();
    let mut main = vec![genesis];
    main.extend(blocks.iter().map(entry));
    let side = side_blocks.iter().map(entry).collect();
    let peer = sim.accept(1, Permissions::NONE).expect("an inbound slot");
    sim.handshake(
        &peer,
        PeerVersionMsg {
            version: PEER_VERSION,
            services: 1,
            ..PeerVersionMsg::v24(SIM_START)
        },
    );
    sim.pass();
    sim.sent(&peer);
    Node {
        sim,
        peer,
        calls,
        main,
        side,
    }
}

fn request(base: &BlockHash, block: &BlockHash) -> Vec<u8> {
    [base.as_bytes().as_slice(), block.as_bytes()].concat()
}

/// A block hash in Core's `ToString()` order.
fn hex(hash: &BlockHash) -> String {
    hash.as_bytes().iter().rev().map(|b| format!("{b:02x}")).collect()
}

fn failed(base: &BlockHash, block: &BlockHash, error: &str) -> String {
    format!(
        "getmnlistdiff failed for baseBlockHash={}, blockHash={}. error={error}",
        hex(base),
        hex(block)
    )
}

/// What one request did: its result, the score it added, the replies and the `net` lines.
struct Outcome {
    result: Result<(), Throw>,
    score: i32,
    sent: Vec<(CommandString, Vec<u8>)>,
    lines: Vec<String>,
}

impl Node {
    fn score(&self) -> i32 {
        self.sim.shared.misbehavior(self.peer.handle.id()).unwrap().0
    }

    fn send(&mut self, payload: Vec<u8>) -> Outcome {
        let before = self.score();
        let log = log_capture::capture(&["net"]);
        let result = self.sim.admitted(&self.peer, "getmnlistd", payload);
        let lines = log.lines().clone();
        drop(log);
        self.sim.pass();
        Outcome {
            result,
            score: self.score() - before,
            sent: self.sim.sent(&self.peer),
            lines,
        }
    }

    fn ask(&mut self, base: &BlockHash, block: &BlockHash) -> Outcome {
        self.send(request(base, block))
    }

    fn calls(&self) -> Vec<(BlockHash, BlockHash, i32)> {
        self.calls.lock().unwrap().clone()
    }
}

/// A scored refusal: +1, logged only by `Misbehaving` with Core's `getmnlistdiff failed` text,
/// no reply, and the source never asked.
fn assert_scored(node: &Node, outcome: &Outcome, text: &str) {
    assert!(outcome.result.is_ok());
    assert_eq!(outcome.score, 1);
    assert!(outcome.sent.is_empty(), "{:?}", outcome.sent);
    assert!(
        outcome.lines.iter().any(|l| l.starts_with("Misbehaving: ") && l.contains(text)),
        "{:?}",
        outcome.lines
    );
    assert!(outcome.lines.iter().all(|l| l.starts_with("Misbehaving: ")), "{:?}", outcome.lines);
    assert!(node.calls().is_empty());
}

/// The success path: a null base goes to the source as the peer sent it, and the source's
/// bytes go out as `mnlistdiff` at the common version.
#[test]
fn a_null_base_is_served_from_the_source() {
    let reply = vec![0x5a, 1, 2, 3];
    let mut node = node(Ok(Some(reply.clone())));
    let tip = node.main[3].hash;
    let outcome = node.ask(&BlockHash::ZERO, &tip);
    assert!(outcome.result.is_ok());
    assert_eq!(outcome.score, 0);
    assert_eq!(outcome.sent, [(CommandString::MNLISTDIFF, reply)]);
    assert_eq!(node.calls(), [(BlockHash::ZERO, tip, PEER_VERSION)]);
    // Only the send itself is logged.
    assert_eq!(outcome.lines.len(), 1, "{:?}", outcome.lines);
    assert!(outcome.lines[0].starts_with("sending mnlistdiff (4 bytes) peer="));
}

#[test]
fn a_known_base_is_served_from_the_source() {
    let mut node = node(Ok(Some(vec![7])));
    let (base, tip) = (node.main[1].hash, node.main[3].hash);
    let outcome = node.ask(&base, &tip);
    assert_eq!(outcome.sent, [(CommandString::MNLISTDIFF, vec![7])]);
    assert_eq!(node.calls(), [(base, tip, PEER_VERSION)]);
}

/// Core: smldiff.cpp:162-175. The base is looked up before the target.
#[test]
fn an_unknown_hash_scores_one() {
    let mut node = node(Ok(Some(vec![7])));
    let (unknown, other) = (BlockHash::from([0x77; 32]), BlockHash::from([0x78; 32]));
    let tip = node.main[3].hash;
    for (base, block, missing) in
        [(unknown, tip, unknown), (node.main[1].hash, unknown, unknown), (unknown, other, unknown)]
    {
        let outcome = node.ask(&base, &block);
        let text = failed(&base, &block, &format!("block {} not found", hex(&missing)));
        assert_scored(&node, &outcome, &text);
    }
}

/// Core: smldiff.cpp:176-179, either block off the active chain.
#[test]
fn a_side_chain_block_scores_one() {
    let mut node = node(Ok(Some(vec![7])));
    let (side, tip) = (node.side[1].hash, node.main[3].hash);
    for (base, block) in [(side, tip), (node.main[0].hash, side)] {
        let outcome = node.ask(&base, &block);
        let text = failed(
            &base,
            &block,
            &format!("block {} and {} are not in the same chain", hex(&base), hex(&block)),
        );
        assert_scored(&node, &outcome, &text);
    }
}

/// Core: smldiff.cpp:180-183.
#[test]
fn a_base_above_the_target_scores_one() {
    let mut node = node(Ok(Some(vec![7])));
    let (base, block) = (node.main[3].hash, node.main[1].hash);
    let outcome = node.ask(&base, &block);
    let text = failed(
        &base,
        &block,
        &format!("base block {} is higher then block {}", hex(&base), hex(&block)),
    );
    assert_scored(&node, &outcome, &text);
}

/// Core: smldiff.cpp:189-193 and np:5641-5648: a target without data is the unavailable-data
/// sentinel, logged with Core's text and not scored.
#[test]
fn a_pruned_target_is_logged_unscored() {
    let mut node = node(Ok(Some(vec![7])));
    let (base, block) = (node.main[1].hash, node.main[2].hash);
    node.sim.chain.prune(node.main[2].id);
    let outcome = node.ask(&base, &block);
    assert!(outcome.result.is_ok());
    assert_eq!(outcome.score, 0);
    assert!(outcome.sent.is_empty());
    let text = format!(
        "block data for block {} is not available (pruned or below an unvalidated snapshot base)",
        hex(&block)
    );
    assert_eq!(outcome.lines, [failed(&base, &block, &text)]);
    assert!(node.calls().is_empty());
}

/// Core reads the request first (np:5630-5631): a short one throws, unscored, before any check.
#[test]
fn a_malformed_request_throws_unscored() {
    let mut node = node(Ok(Some(vec![7])));
    let outcome = node.send(vec![0xee; 63]);
    assert!(outcome.result.is_err());
    assert_eq!(outcome.score, 0);
    assert!(outcome.sent.is_empty());
    assert!(node.calls().is_empty());
}

/// A source refusal: scored ones are logged by `Misbehaving` only, unscored ones with Core's
/// text only; neither replies.
#[test]
fn source_refusals_follow_their_score() {
    let error = "failed to build quorums diff";
    let mut scored = node(Err(ServeRefusal {
        score: Some(1),
        text: error.into(),
    }));
    let (base, tip) = (scored.main[1].hash, scored.main[3].hash);
    let outcome = scored.ask(&base, &tip);
    assert_eq!(outcome.score, 1);
    assert!(outcome.sent.is_empty());
    assert!(outcome.lines.iter().all(|l| l.starts_with("Misbehaving: ")), "{:?}", outcome.lines);
    assert!(outcome.lines.iter().any(|l| l.contains(&failed(&base, &tip, error))));
    assert_eq!(scored.calls(), [(base, tip, PEER_VERSION)]);

    let error = "list unavailable";
    let mut unscored = node(Err(ServeRefusal {
        score: None,
        text: error.into(),
    }));
    let outcome = unscored.ask(&base, &tip);
    assert_eq!(outcome.score, 0);
    assert!(outcome.sent.is_empty());
    assert_eq!(outcome.lines, [failed(&base, &tip, error)]);
}

/// The node's source before RP2a-P2 (and `Registry::new`'s): a checked request ends with no
/// reply, no score and no line.
#[test]
fn no_source_ends_silently() {
    for mut node in [node(Ok(None)), node_with(Registry::new(), Calls::default())] {
        let (base, tip) = (node.main[1].hash, node.main[3].hash);
        let outcome = node.ask(&base, &tip);
        assert!(outcome.result.is_ok());
        assert_eq!(outcome.score, 0);
        assert!(outcome.sent.is_empty());
        assert!(outcome.lines.is_empty(), "{:?}", outcome.lines);
    }
}
