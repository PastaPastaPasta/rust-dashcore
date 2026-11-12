//! Fixed Core witnesses for the multi-peer block download comparison.

use std::sync::Arc;

use bitcoin_hashes::sha256;
use dash_consensus::primitives::core_serialize;
use dash_consensus::{Block, decode_block};
use dash_oracle_sys::{ConnType, Oracle, Pin};
use dash_p2p::rng::FastRandom;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START};
use node_l1::Outcome;
use node_l1::p2p_blocks::{
    BadVariant, Choice, FIXTURE_SHA256, Op, Script, ScriptHeader, bad_block, blocktxn,
    compare_p2p_multi_blocks, decode, deliver, encode, fixture, getblocktxn, headers,
    honest_cmpctblock, request_indexes, sendcmpct,
};
use node_l1::p2p_node::{
    DownloadView, Report, Session, Step, StepResolver, StepSource, report_session,
};

fn chain(profile: u8) -> Vec<Arc<Block>> {
    fixture().unwrap().chain(profile)
}
fn b(height: usize) -> Arc<Block> {
    Arc::clone(&chain(3)[height - 1])
}
fn pass(s: &mut Session) {
    s.steps.push(Step::Pass(s.mocktime));
}
fn send(s: &mut Session, peer: usize, command: &'static str, payload: Vec<u8>) {
    s.steps.push(deliver(peer, command, payload));
    pass(s);
}
fn announce(s: &mut Session, peer: usize, blocks: &[Arc<Block>]) {
    send(s, peer, "headers", headers(blocks));
}
fn serve_block(s: &mut Session, peer: usize, block: &Block) {
    send(s, peer, "block", core_serialize(block));
}
fn compact(s: &mut Session, peer: usize, block: &Block, prefilled: &[usize]) {
    send(s, peer, "cmpctblock", honest_cmpctblock(block, 17, prefilled).unwrap());
}
fn base(profile: u8, n: usize) -> Session {
    let mut s = Script {
        header: ScriptHeader {
            seed: 7,
            profile,
            peers: vec![[0, 2]; n],
        },
        ops: vec![Op::Tick],
    }
    .session()
    .unwrap();
    s.ops = None;
    for command in ["version", "verack"] {
        for peer in 0..n {
            s.steps.push(deliver(
                peer,
                command,
                if command == "version" {
                    PeerVersionMsg::v24(SIM_START).payload()
                } else {
                    Vec::new()
                },
            ));
        }
        pass(&mut s);
    }
    s
}
fn agree(s: &Session) -> Report {
    let r = report_session(&Oracle::new(Pin::V24_0), s).expect("both legs run").expect("in domain");
    assert!(r.difference.is_none(), "{}", r.difference.as_deref().unwrap_or(""));
    assert_eq!(r.compared, r.steps.len());
    r
}
fn views(r: &Report) -> impl DoubleEndedIterator<Item = &DownloadView> {
    r.downloads.iter().flatten()
}
fn last(r: &Report) -> &DownloadView {
    views(r).last().unwrap()
}
fn messages<'a>(r: &'a Report, command: &'a str) -> impl Iterator<Item = &'a Vec<u8>> {
    views(r)
        .flat_map(|v| &v.received)
        .flatten()
        .filter_map(move |(c, p)| (c == command).then_some(p))
}

#[test]
fn the_fixture_is_the_recorded_one() {
    assert_eq!(
        sha256::Hash::hash(include_bytes!("../fixtures/p3_multi_blocks.bin")).to_byte_array(),
        FIXTURE_SHA256
    );
    let f = fixture().unwrap();
    assert_eq!([f.deep.len(), f.prefix.len(), f.workload.len(), f.side.len()], [1100, 100, 24, 3]);
    assert_eq!(
        f.workload.iter().map(|b| b.txs().len()).collect::<Vec<_>>(),
        [2, 1, 1, 2, 3, 1, 5, 9, 1, 2, 17, 1, 33, 1, 1, 2, 4, 1, 3, 1, 2, 1, 1, 6]
    );
    let old: Vec<_> = include_str!("../../dashd-p2p/tests/fixtures/dashd-v24/blocks-1100.txt")
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            (parts.next()? == ">" && parts.nth(1)? == "block")
                .then(|| parts.next().unwrap().to_owned())
        })
        .collect();
    assert_eq!(old.len(), 1100);
    for (raw, b) in old.iter().zip(&f.deep) {
        assert_eq!(
            raw,
            &core_serialize(&**b).iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
    }
}

#[test]
fn sixteen_in_flight_then_one_per_block() {
    let blocks = chain(0);
    let mut s = base(0, 2);
    announce(&mut s, 0, &blocks);
    for index in [1, 4, 2] {
        serve_block(&mut s, 0, &blocks[index]);
    }
    let r = agree(&s);
    let states: Vec<_> = views(&r)
        .filter_map(|v| v.peers[0].as_ref())
        .filter(|p| !p.blocks_in_flight.is_empty())
        .collect();
    assert_eq!(states[0].blocks_in_flight.len(), 16);
    assert_eq!(
        states[0].blocks_in_flight.iter().map(|q| q.0.1).collect::<Vec<_>>(),
        (1..=16).collect::<Vec<_>>()
    );
    assert!(states.iter().all(|p| p.blocks_in_flight.len() <= 16));
    assert!(states[0].downloading_since_us > 0);
    assert_eq!(last(&r).globals.peers_downloading_from, 1);
    assert_eq!(last(&r).peers[0].as_ref().unwrap().blocks_in_flight.len(), 16);
    assert!(messages(&r, "getdata").any(|m| m[0] == 16));
    assert!(messages(&r, "getdata").filter(|m| m[0] == 1).count() >= 3);
}

fn stall_script() -> Script {
    Script {
        header: ScriptHeader {
            seed: 7,
            profile: 0,
            peers: vec![[0, 3]; 4],
        },
        ops: vec![Op::Serve {
            rounds: 360,
            peers: 15,
            withhold: Some((0, true)),
        }],
    }
}
fn stalled() -> Session {
    stall_script().session().unwrap()
}

#[test]
fn the_window_stalls_on_a_withheld_block() {
    let r = agree(&stalled());
    assert!(
        views(&r).any(|v| v.peers.iter().flatten().any(|p| p.stalling_since_us != 0)),
        "Core: stalling_since != 0"
    );
    assert!(views(&r).any(|v| v.peers.iter().flatten().any(|p| p.last_common_block.1 == 0)));
}

fn timeout_script() -> Script {
    let mut script = stall_script();
    script.ops.extend([
        Op::Pass {
            class: 2,
            peer: 0,
            delta: 0,
        },
        Op::Pass {
            class: 2,
            peer: 0,
            delta: 1,
        },
    ]);
    script
}

#[test]
fn a_stall_times_out_at_the_strict_edge() {
    let script = timeout_script();
    let r = agree(&script.session().unwrap());
    assert!(views(&r).any(|v| v.globals.block_stalling_timeout_s == 4));
    let tail: Vec<_> = views(&r).rev().take(2).collect();
    assert!(tail[0].disconnect.iter().any(|d| *d));
    assert!(!tail[1].disconnect.iter().any(|d| *d));
}

fn decay_script() -> Script {
    let mut script = stall_script();
    script.ops.extend([
        Op::Pass {
            class: 2,
            peer: 0,
            delta: 1,
        },
        Op::Block {
            peer: 1,
            which: Choice {
                mode: 3,
                index: 0,
            },
        },
        Op::Pass {
            class: 0,
            peer: 0,
            delta: 0,
        },
        Op::Block {
            peer: 1,
            which: Choice {
                mode: 3,
                index: 0,
            },
        },
        Op::Pass {
            class: 0,
            peer: 0,
            delta: 0,
        },
    ]);
    script
}

#[test]
fn the_stalling_timeout_decays_per_connected_block() {
    let script = decay_script();
    let r = agree(&script.session().unwrap());
    let timeouts: Vec<_> = views(&r).map(|v| v.globals.block_stalling_timeout_s).collect();
    assert!(timeouts.contains(&4));
    assert!(timeouts.contains(&3));
    assert_eq!(*timeouts.last().unwrap(), 2);
}

#[test]
fn a_download_times_out_by_downloading_peers() {
    for peers in [1, 2] {
        let mut s = base(0, 2);
        let blocks = chain(0);
        for peer in 0..peers {
            announce(&mut s, peer, &blocks);
        }
        let edge = SIM_START + 150 + 75 * (peers as i64 - 1);
        s.steps.push(Step::Pass(edge));
        s.steps.push(Step::Pass(edge + 1));
        let r = agree(&s);
        let tail: Vec<_> = views(&r).rev().take(2).collect();
        assert!(tail[0].disconnect.iter().any(|d| *d));
        assert!(!tail[1].disconnect.iter().any(|d| *d));
    }
}

#[test]
fn direct_fetch_takes_seventeen_and_refuses_eighteen() {
    for count in [16, 17, 18] {
        let mut s = base(2, 2);
        announce(&mut s, 0, &chain(2)[100..100 + count]);
        pass(&mut s);
        pass(&mut s);
        let r = agree(&s);
        assert!(last(&r).globals.initial_block_download);
        let requests: Vec<_> = messages(&r, "getdata").collect();
        if count == 18 {
            assert!(requests.is_empty());
            assert!(last(&r).globals.blocks_in_flight.is_empty());
        } else {
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0][0], 16);
            assert_eq!(requests[0][1..5], 2_u32.to_le_bytes());
            assert_eq!(last(&r).peers[0].as_ref().unwrap().blocks_in_flight.len(), 16);
        }
    }
}

#[test]
fn a_compact_request_while_in_ibd() {
    let mut s = base(2, 2);
    send(&mut s, 0, "sendcmpct", sendcmpct(false, 1));
    announce(&mut s, 0, &[b(101)]);
    let r = agree(&s);
    assert!(last(&r).globals.initial_block_download);
    assert!(messages(&r, "getdata").any(|p| p[1..5] == 20_u32.to_le_bytes()));
    let mut s = base(2, 2);
    s.peers[0].conn = ConnType::OutboundFullRelay;
    announce(&mut s, 0, &[b(101)]);
    let r = agree(&s);
    assert!(r.removed[0]);
    let mut s = base(0, 2);
    send(&mut s, 0, "sendcmpct", sendcmpct(false, 1));
    announce(&mut s, 0, &chain(0));
    let r = agree(&s);
    assert!(messages(&r, "getdata").all(|p| p[1..5] == 2_u32.to_le_bytes()));
}

#[test]
fn an_honest_compact_block_fills_from_blocktxn() {
    let mut s = base(3, 2);
    send(&mut s, 0, "sendcmpct", sendcmpct(false, 1));
    compact(&mut s, 0, &b(101), &[0]);
    send(&mut s, 0, "blocktxn", blocktxn(&b(101), &[1]));
    let r = agree(&s);
    assert!(messages(&r, "getblocktxn").any(|p| request_indexes(p).unwrap().1 == [1]));
    assert!(
        views(&r).any(|v| v.peers.iter().flatten().any(|p| p.blocks_in_flight.iter().any(|q| q.1)))
    );
    assert_eq!(last(&r).tip.1, 101);
}

#[test]
fn a_compact_block_two_above_the_tip_keeps_its_source() {
    let mut s = base(3, 2);
    announce(&mut s, 0, &[b(101), b(102)]);
    compact(&mut s, 0, &b(102), &[0]);
    let r = agree(&s);
    assert_eq!(last(&r).tip.1, 100);
    assert!(last(&r).globals.block_sources.contains(&(*b(102).hash().as_bytes(), 0, false)));
}

#[test]
fn a_coinbase_only_compact_block_connects_at_once() {
    let mut s = base(3, 2);
    serve_block(&mut s, 0, &b(101));
    compact(&mut s, 0, &b(102), &[0]);
    let r = agree(&s);
    assert_eq!(last(&r).tip.1, 102);
    assert_eq!(messages(&r, "getblocktxn").count(), 0);
}

#[test]
fn three_peers_may_fill_one_block() {
    let mut s = base(3, 4);
    s.peers[0].conn = ConnType::OutboundFullRelay;
    for peer in 0..4 {
        send(&mut s, peer, "sendcmpct", sendcmpct(false, 1));
    }
    for (peer, height) in [(0, 101), (1, 102), (2, 103)] {
        serve_block(&mut s, peer, &b(height));
    }
    for peer in 0..4 {
        compact(&mut s, peer, &b(104), &[0]);
    }
    let r = agree(&s);
    let hashes: Vec<_> = last(&r)
        .globals
        .blocks_in_flight
        .iter()
        .filter(|(h, _)| h == b(104).hash().as_bytes())
        .collect();
    assert_eq!(hashes.len(), 3);
    assert_eq!(hashes.iter().map(|(_, p)| *p).collect::<Vec<_>>(), [0, 1, 2]);
}

#[test]
fn blocktxn_from_the_wrong_peer_or_twice_changes_nothing() {
    let mut s = base(3, 2);
    compact(&mut s, 0, &b(101), &[0]);
    send(&mut s, 1, "blocktxn", blocktxn(&b(101), &[1]));
    send(&mut s, 0, "blocktxn", blocktxn(&b(101), &[1]));
    send(&mut s, 0, "blocktxn", blocktxn(&b(101), &[1]));
    let r = agree(&s);
    assert_eq!(last(&r).tip.1, 101);
    assert!(views(&r).all(|v| v.misbehaving.iter().all(Vec::is_empty)));
}

#[test]
fn hb_selection_rotates_at_three() {
    let mut s = base(3, 5);
    s.peers[0].conn = ConnType::OutboundFullRelay;
    for peer in 0..5 {
        send(&mut s, peer, "sendcmpct", sendcmpct(false, 1));
    }
    for (peer, height) in (0..5).zip(101..106) {
        announce(&mut s, peer, &[b(height)]);
        serve_block(&mut s, peer, &b(height));
    }
    let r = agree(&s);
    let lists: Vec<_> = views(&r).map(|v| v.globals.hb_cmpct_announcers.clone()).collect();
    assert!(lists.iter().any(|l| l.len() == 3));
    assert!(lists.iter().all(|l| l.len() <= 3));
    assert!(lists.windows(2).any(|w| w[0].len() == 3 && w[1].len() == 3 && w[0] != w[1]));
    assert!(last(&r).globals.hb_cmpct_announcers.contains(&0));
    assert!(messages(&r, "sendcmpct").any(|p| p[0] == 0));
}

#[test]
fn a_new_block_is_announced_by_cmpctblock() {
    let mut s = base(3, 2);
    send(&mut s, 1, "sendcmpct", sendcmpct(true, 1));
    announce(&mut s, 1, &[b(100)]);
    serve_block(&mut s, 0, &b(101));
    let r = agree(&s);
    let payload = messages(&r, "cmpctblock").last().expect("Core: compact announcement");
    let nonce = u64::from_le_bytes(payload[80..88].try_into().unwrap());
    assert_eq!(*payload, honest_cmpctblock(&b(101), nonce, &[0]).unwrap());
    let connect = s.steps.len() - 1;
    let matching =
        r.step_draws[connect]
            .iter()
            .filter(|(strong, bytes)| {
                !strong
                    && bytes.as_slice().try_into().ok().is_some_and(|seed| {
                        FastRandom::with_seed(seed).randrange(u64::MAX) == nonce
                    })
            })
            .count();
    assert_eq!(matching, 1, "Core: one announcement nonce draw");
}

#[test]
fn a_peer_asks_for_block_transactions() {
    let mut s = base(3, 2);
    send(&mut s, 0, "getblocktxn", getblocktxn(*b(100).hash().as_bytes(), &[0]));
    send(&mut s, 0, "getblocktxn", getblocktxn(*b(89).hash().as_bytes(), &[0]));
    pass(&mut s);
    let r = agree(&s);
    assert_eq!(messages(&r, "blocktxn").count(), 1);
    assert_eq!(messages(&r, "block").count(), 1);
    let mut ops = Vec::new();
    for index in [100, 101] {
        ops.push(Op::Block {
            peer: 0,
            which: Choice::absolute(index),
        });
        ops.push(Op::Pass {
            class: 0,
            peer: 0,
            delta: 0,
        });
    }
    ops.push(Op::Headers {
        peer: 0,
        from: Choice {
            mode: 1,
            index: 124,
        },
        count: 3,
    });
    ops.push(Op::Pass {
        class: 0,
        peer: 0,
        delta: 0,
    });
    for index in 124..127 {
        ops.push(Op::Block {
            peer: 0,
            which: Choice::absolute(index),
        });
        ops.push(Op::Pass {
            class: 0,
            peer: 0,
            delta: 0,
        });
    }
    ops.push(Op::GetBlockTxn {
        peer: 1,
        depth: 3,
        indexes: 1,
    });
    ops.push(Op::Pass {
        class: 0,
        peer: 0,
        delta: 0,
    });
    let script = Script {
        header: ScriptHeader {
            seed: 7,
            profile: 3,
            peers: vec![[0, 2]; 2],
        },
        ops,
    };
    let r = agree(&script.session().unwrap());
    assert_eq!(last(&r).tip.0, *fixture().unwrap().side[2].hash().as_bytes());
    assert!(
        messages(&r, "blocktxn").any(|p| p[..32] == b(100).hash().as_bytes()[..]),
        "Core: depth walks the active branch"
    );
}

#[test]
fn a_peer_asks_for_genesis_transactions() {
    let mut s = base(0, 2);
    let r = agree(&s);
    let genesis = last(&r).tip.0;
    send(&mut s, 0, "getblocktxn", getblocktxn(genesis, &[0]));
    let r = agree(&s);
    assert_eq!(messages(&r, "blocktxn").count(), 1);
}

fn mutated(variant: BadVariant) {
    let mut s = base(3, 2);
    for height in 101..105 {
        serve_block(&mut s, 1, &b(height));
    }
    announce(&mut s, 0, &[b(105)]);
    let bad = bad_block(&b(105), variant).unwrap();
    serve_block(&mut s, 0, &bad);
    serve_block(&mut s, 1, &b(105));
    let r = agree(&s);
    assert!(views(&r).flat_map(|v| &v.misbehaving).flatten().any(|l| l.contains("mutated block")));
    assert_eq!(last(&r).tip.1, 105);
    assert!(!last(&r).globals.blocks_in_flight.iter().any(|(h, _)| h == b(105).hash().as_bytes()));
}
#[test]
fn a_duplicated_trailing_pair_is_a_mutated_block() {
    mutated(BadVariant::TrailingPair);
}
#[test]
fn a_bad_merkle_root_is_a_mutated_block() {
    mutated(BadVariant::MerkleRoot);
}

#[derive(Clone, Debug)]
struct Fixed {
    steps: Vec<Step>,
    bad: Vec<dash_primitives::BlockHash>,
}
struct FixedResolver {
    steps: std::vec::IntoIter<Step>,
}
impl StepSource for Fixed {
    fn start(&self) -> Box<dyn StepResolver> {
        Box::new(FixedResolver {
            steps: self.steps.clone().into_iter(),
        })
    }
    fn bad_connects(&self) -> Vec<dash_primitives::BlockHash> {
        self.bad.clone()
    }
}
impl StepResolver for FixedResolver {
    fn next(&mut self, _: Option<&DownloadView>) -> Option<Step> {
        self.steps.next()
    }
}

fn duplicate_session() -> Session {
    let mut s = base(3, 3);
    for height in 101..105 {
        serve_block(&mut s, 0, &b(height));
    }
    send(&mut s, 2, "sendcmpct", sendcmpct(true, 1));
    announce(&mut s, 2, &[b(104)]);
    let bad = bad_block(&b(105), BadVariant::DuplicateTransaction).unwrap();
    announce(&mut s, 0, &[Arc::clone(&bad)]);
    serve_block(&mut s, 0, &bad);
    serve_block(&mut s, 1, &b(105));
    s.ops = Some(Arc::new(Fixed {
        steps: s.steps.clone(),
        bad: vec![bad.hash()],
    }));
    s.steps.clear();
    s
}
#[test]
fn a_duplicate_transaction_fails_at_connect() {
    let r = agree(&duplicate_session());
    assert!(views(&r).flat_map(|v| &v.misbehaving).flatten().any(|l| l.contains("100")));
    let bad = bad_block(&b(105), BadVariant::DuplicateTransaction).unwrap();
    assert!(messages(&r, "cmpctblock").any(|p| p[..80] == core_serialize(bad.header())));
    let failed = r.steps.iter().position(|step| matches!(step,
        Step::Deliver {command: "block", payload, ..} if payload[..80] == core_serialize(bad.header()))).unwrap() + 1;
    let order = &r.pass_orders[failed];
    assert!(
        order.iter().position(|p| *p == 1) < order.iter().position(|p| *p == 0),
        "observer before source"
    );
    let view = r.downloads[failed].as_ref().unwrap();
    assert_eq!(view.tip.1, 104, "Core: failed connect leaves tip");
    assert!(!view.misbehaving[0].is_empty(), "Core: source punished");
    assert!(view.relay[1].is_empty(), "Core: failed block queues no tip relay");
    assert!(
        view.received[1]
            .iter()
            .all(|(c, _)| !["inv", "headers", "headers2", "cmpctblock"].contains(&c.as_str()))
    );
    assert!(views(&r).all(|v| !v.relay[1].contains(bad.hash().as_bytes())));
    assert_eq!(last(&r).tip.1, 105);
}

#[test]
fn prefilled_subsets_are_honest() {
    assert!(!has_noncoinbase_prefilled(&honest_cmpctblock(&b(108), 17, &[0]).unwrap()));
    assert!(has_noncoinbase_prefilled(&honest_cmpctblock(&b(108), 17, &[1]).unwrap()));
    for prefilled in [vec![0, 2, 5, 8], vec![1, 4, 7]] {
        let mut s = base(3, 2);
        for height in 101..108 {
            serve_block(&mut s, 1, &b(height));
        }
        let block = b(108);
        compact(&mut s, 0, &block, &prefilled);
        let complement: Vec<_> =
            (0..block.txs().len()).filter(|i| !prefilled.contains(i)).collect();
        let r = agree(&s);
        let asked: Vec<_> =
            messages(&r, "getblocktxn").map(|p| request_indexes(p).unwrap().1).collect();
        assert!(views(&r).all(|v| v.misbehaving.iter().all(Vec::is_empty)), "Core: no Misbehaving");
        assert_eq!(asked, vec![complement.clone()], "Core: getblocktxn complement");
        send(&mut s, 0, "blocktxn", blocktxn(&block, &complement));
        let r = agree(&s);
        assert!(views(&r).all(|v| v.misbehaving.iter().all(Vec::is_empty)), "Core: no Misbehaving");
        assert_eq!(last(&r).tip.1, 108);
    }
}

fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
#[test]
fn honest_encoder_rebuilds_dashds_bytes() {
    let transcript = include_str!("../../dashd-p2p/tests/fixtures/dashd-v24/cmpct-announce.txt");
    let encoded = transcript
        .lines()
        .find_map(|l| {
            let p: Vec<_> = l.split_whitespace().collect();
            (p.len() == 4 && p[0] == "<" && p[2] == "cmpctblock").then(|| unhex(p[3]))
        })
        .unwrap();
    let raw = transcript
        .lines()
        .filter_map(|l| {
            let p: Vec<_> = l.split_whitespace().collect();
            (p.len() == 2 && p[0] == "submitblock").then(|| unhex(p[1]))
        })
        .next_back()
        .unwrap();
    let block = decode_block(&mut raw.as_slice()).unwrap().into_value();
    let nonce = u64::from_le_bytes(encoded[80..88].try_into().unwrap());
    assert_eq!(honest_cmpctblock(&block, nonce, &[0]).unwrap(), encoded, "Core: compact bytes");
}

#[test]
fn unrequested_blocks_follow_accept_block() {
    let deep = chain(0);
    let mut far = base(0, 2);
    announce(&mut far, 0, &deep);
    serve_block(&mut far, 1, &deep[288]);
    let r = agree(&far);
    assert!(
        !last(&r).globals.block_sources.iter().any(|(h, _, _)| h == deep[288].hash().as_bytes())
    );
    let mut requested_far = base(0, 2);
    announce(&mut requested_far, 0, &deep);
    for block in &deep[1..=288] {
        serve_block(&mut requested_far, 0, block);
    }
    let r = agree(&requested_far);
    assert_eq!(last(&r).tip.1, 0);
    assert!(
        last(&r).globals.block_sources.contains(&(*deep[288].hash().as_bytes(), 0, true)),
        "Core: requested block beyond 288 stored"
    );
    let mut equal = base(3, 2);
    let side = fixture().unwrap().side.clone();
    serve_block(&mut equal, 0, &b(101));
    serve_block(&mut equal, 1, &side[0]);
    let r = agree(&equal);
    assert!(last(&r).globals.block_sources.iter().any(|(h, _, _)| h == side[0].hash().as_bytes()));
    let mut less = base(3, 2);
    serve_block(&mut less, 0, &b(101));
    serve_block(&mut less, 0, &b(102));
    announce(&mut less, 1, &side[..1]);
    serve_block(&mut less, 1, &side[0]);
    let r = agree(&less);
    assert!(!last(&r).globals.block_sources.iter().any(|(h, _, _)| h == side[0].hash().as_bytes()));
    let mut requested_less = base(3, 2);
    announce(&mut requested_less, 1, &side[..1]);
    serve_block(&mut requested_less, 0, &b(101));
    serve_block(&mut requested_less, 0, &b(102));
    serve_block(&mut requested_less, 1, &side[0]);
    let r = agree(&requested_less);
    assert!(
        last(&r).globals.block_sources.contains(&(*side[0].hash().as_bytes(), 1, true)),
        "Core: requested less-work block stored"
    );
    let mut minimum = base(2, 2);
    serve_block(&mut minimum, 0, &b(101));
    let r = agree(&minimum);
    assert_eq!(last(&r).tip.1, 100);
    assert!(last(&r).globals.block_sources.is_empty());
    announce(&mut minimum, 0, &[b(101)]);
    serve_block(&mut minimum, 0, &b(101));
    let r = agree(&minimum);
    assert_eq!(last(&r).tip.1, 101);
    assert!(last(&r).globals.initial_block_download);
    serve_block(&mut minimum, 0, &b(101));
    agree(&minimum);
}

#[test]
fn the_side_branch_reorgs() {
    let mut s = base(3, 3);
    s.seed = 0;
    serve_block(&mut s, 0, &b(101));
    serve_block(&mut s, 0, &b(102));
    send(&mut s, 1, "sendheaders", Vec::new());
    announce(&mut s, 1, &[b(100)]);
    let side = fixture().unwrap().side.clone();
    announce(&mut s, 0, &side);
    for block in &side {
        serve_block(&mut s, 0, block);
    }
    let r = agree(&s);
    assert_eq!(r.pass_orders.last().unwrap(), &[1, 2, 0]);
    assert_eq!(last(&r).tip.0, *side[2].hash().as_bytes());
    assert!(last(&r).globals.block_sources.is_empty());
    assert_eq!(last(&r).relay[1], side.iter().map(|b| *b.hash().as_bytes()).collect::<Vec<_>>());
}

#[test]
fn ibd_ends_at_the_first_recent_block() {
    let mut s = base(1, 2);
    send(&mut s, 0, "sendcmpct", sendcmpct(false, 1));
    serve_block(&mut s, 0, &b(1));
    let r = agree(&s);
    assert!(views(&r).next().unwrap().globals.initial_block_download);
    assert!(!last(&r).globals.initial_block_download);
    assert!(last(&r).globals.hb_cmpct_announcers.is_empty());
}

fn descendants(count: usize) -> Session {
    let mut s = base(1, 2);
    s.seed = 7;
    send(&mut s, 0, "sendcmpct", sendcmpct(false, 1));
    send(&mut s, 1, "sendheaders", Vec::new());
    announce(&mut s, 0, &chain(1)[..count]);
    announce(&mut s, 1, &[b(1)]);
    for height in 2..=count {
        serve_block(&mut s, 0, &b(height));
    }
    serve_block(&mut s, 0, &b(1));
    pass(&mut s);
    s
}
#[test]
fn ibd_ends_inside_one_accept() {
    let r = agree(&descendants(40));
    assert_eq!(last(&r).tip.1, 40);
    assert!(!last(&r).globals.initial_block_download);
    assert!(last(&r).globals.hb_cmpct_announcers.contains(&0));
    assert!(messages(&r, "inv").any(|p| p.ends_with(b(40).hash().as_bytes())));
}

#[test]
fn stored_descendants_notify_one_by_one() {
    for count in [8, 9] {
        let r = agree(&descendants(count));
        assert_eq!(last(&r).tip.1, count as i32);
        if count == 9 {
            let connect = r.steps.len() - 2;
            assert_eq!(r.pass_orders[connect], vec![0, 1], "deliverer before recipient");
            let recipient = &r.downloads[connect].as_ref().unwrap().received[1];
            assert!(
                recipient.iter().any(|(c, p)| c == "inv" && p.ends_with(b(9).hash().as_bytes()))
            );
            assert!(recipient.iter().all(|(c, _)| c != "headers"));
        } else {
            assert!(messages(&r, "headers").any(|p| p[0] == 7));
        }
    }
}

#[test]
fn two_blocks_connect_in_one_pass() {
    let mut s = base(3, 3);
    send(&mut s, 2, "sendheaders", Vec::new());
    announce(&mut s, 2, &[b(100)]);
    announce(&mut s, 0, &[b(101), b(102)]);
    s.steps.push(Step::Batch(vec![
        deliver(0, "block", core_serialize(&*b(101))),
        deliver(1, "block", core_serialize(&*b(102))),
    ]));
    pass(&mut s);
    pass(&mut s);
    let r = agree(&s);
    assert_eq!(last(&r).tip.1, 102);
    assert!(messages(&r, "headers").count() > 0);
}

#[test]
fn each_header_preference_gets_its_announcement() {
    let mut s = base(3, 5);
    send(&mut s, 1, "sendheaders", Vec::new());
    send(&mut s, 2, "sendheaders2", Vec::new());
    send(&mut s, 4, "sendcmpct", sendcmpct(true, 1));
    for peer in 1..5 {
        announce(&mut s, peer, &[b(100)]);
    }
    serve_block(&mut s, 0, &b(101));
    pass(&mut s);
    let r = agree(&s);
    for command in ["headers", "headers2", "inv", "cmpctblock"] {
        assert!(messages(&r, command).count() > 0, "Core: {command}");
    }
}

#[test]
fn a_stall_edge_in_the_pass_of_a_connect() {
    let mut script = stall_script();
    script.ops.extend([
        Op::Pass {
            class: 2,
            peer: 0,
            delta: 1,
        },
        Op::Pass {
            class: 0,
            peer: 0,
            delta: 0,
        },
    ]);
    let first = agree(&script.session().unwrap());
    assert_eq!(last(&first).globals.block_stalling_timeout_s, 4);
    let mut prepared = first;
    for _ in 0..3 {
        if last(&prepared).peers.iter().flatten().any(|p| p.stalling_since_us != 0) {
            break;
        }
        script.ops.push(Op::Pass {
            class: 0,
            peer: 0,
            delta: 0,
        });
        prepared = agree(&script.session().unwrap());
    }
    let view = last(&prepared);
    let (staller, state) = view
        .peers
        .iter()
        .enumerate()
        .filter_map(|(p, s)| s.as_ref().map(|s| (p, s)))
        .find(|(_, s)| s.stalling_since_us != 0)
        .expect("Core: second staller");
    let source = (0..4).find(|p| *p != staller && !view.disconnect[*p]).unwrap();
    let block = chain(0)
        .into_iter()
        .find(|b| b.hash().as_bytes() == &state.blocks_in_flight[0].0.0)
        .unwrap();
    let at = state.stalling_since_us / 1_000_000 + 4;
    let mut s = script.session().unwrap();
    s.ops = None;
    s.steps = prepared.steps;
    s.steps.push(deliver(source, "block", core_serialize(&*block)));
    s.steps.push(Step::Pass(at));
    let r = agree(&s);
    assert!(last(&r).globals.block_stalling_timeout_s < 4);
}

#[test]
fn a_block_source_stays_until_its_parent_connects() {
    let mut s = base(3, 2);
    announce(&mut s, 0, &[b(101), b(102)]);
    serve_block(&mut s, 0, &b(102));
    let r = agree(&s);
    assert!(last(&r).globals.block_sources.contains(&(*b(102).hash().as_bytes(), 0, true)));
    serve_block(&mut s, 0, &b(101));
    let r = agree(&s);
    assert_eq!(last(&r).tip.1, 102);
    assert!(last(&r).globals.block_sources.is_empty());
}
#[test]
fn misbehaviour_from_a_gap_in_headers() {
    let mut s = base(3, 2);
    for _ in 0..10 {
        announce(&mut s, 0, &[b(102)]);
    }
    let r = agree(&s);
    assert!(views(&r).any(|v| v.misbehaving.iter().any(|l| !l.is_empty())));
}
#[test]
fn a_removed_peer_leaves_the_hb_list_as_core_does() {
    let mut s = base(3, 2);
    send(&mut s, 0, "sendcmpct", sendcmpct(false, 1));
    serve_block(&mut s, 0, &b(101));
    s.steps.push(Step::Pass(SIM_START + 151));
    let r = agree(&s);
    assert!(last(&r).globals.hb_cmpct_announcers.len() <= 3);
}
#[test]
fn an_unanswered_ping_removes_the_peer() {
    let mut s = base(3, 2);
    pass(&mut s);
    s.steps.extend([Step::Pass(SIM_START + 1201), Step::Pass(SIM_START + 1201)]);
    let r = agree(&s);
    assert!(
        r.removed.iter().all(|p| *p),
        "{:?}",
        r.downloads.iter().flatten().map(|v| &v.disconnect).collect::<Vec<_>>()
    );
}

#[test]
fn the_resolver_reads_no_outcome() {
    let script = Script {
        header: ScriptHeader {
            seed: 1,
            profile: 3,
            peers: vec![[0, 2]; 2],
        },
        ops: vec![
            Op::NextHeader {
                peer: 0,
            },
            Op::Pass {
                class: 0,
                peer: 0,
                delta: 0,
            },
        ],
    };
    let r = agree(&script.session().unwrap());
    assert_eq!(r.outcome_reads, 3 * r.compared);
}
#[test]
fn peer_ids_are_script_indexes() {
    let mut s = base(0, 10);
    announce(&mut s, 9, &chain(0));
    let r = agree(&s);
    assert!(last(&r).globals.blocks_in_flight.iter().all(|(_, p)| *p == 9));
}
#[test]
fn each_chain_syncs_over_p2p() {
    for profile in [0, 1] {
        let mut s = base(profile, 2);
        let blocks = chain(profile);
        let end = if profile == 0 {
            1100
        } else {
            124
        };
        announce(&mut s, 0, &blocks[..end]);
        for block in &blocks[..end] {
            serve_block(&mut s, 0, block);
        }
        let r = agree(&s);
        assert_eq!(last(&r).tip.1, end as i32);
    }
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}
#[test]
fn structured_bytes_reach_the_download_states() {
    assert_eq!(compare_p2p_multi_blocks(&[], Pin::V24_0), Ok(Outcome::OutOfDomain));
    let mut counts = [0; 10];
    let mut state = 238;
    for input in 0..64 {
        let seed = splitmix(&mut state);
        let mut header = ScriptHeader {
            seed,
            profile: 3,
            peers: vec![[16, 2]; 2],
        };
        let mut ops = vec![
            Op::NextHeader {
                peer: 0,
            },
            Op::Pass {
                class: 0,
                peer: 0,
                delta: 0,
            },
        ];
        match input % 8 {
            0 => {
                header = stall_script().header;
                header.seed = seed;
                ops = stall_script().ops;
            }
            1 | 2 => ops.extend([
                Op::CmpctBlock {
                    peer: 0,
                    which: Choice::absolute(100),
                    nonce: seed,
                    prefilled: 1,
                },
                Op::Pass {
                    class: 0,
                    peer: 0,
                    delta: 0,
                },
                Op::BlockTxn {
                    peer: 0,
                    source: 0,
                    index: 0,
                },
                Op::Pass {
                    class: 0,
                    peer: 0,
                    delta: 0,
                },
            ]),
            3..=5 => {
                ops.extend([
                    Op::Headers {
                        peer: 1,
                        from: Choice {
                            mode: 1,
                            index: 100,
                        },
                        count: 4,
                    },
                    Op::Pass {
                        class: 0,
                        peer: 0,
                        delta: 0,
                    },
                    Op::Serve {
                        rounds: 5,
                        peers: 3,
                        withhold: None,
                    },
                ]);
                ops.extend([
                    Op::BadBlock {
                        peer: 0,
                        which: Choice::absolute(104),
                        variant: match input % 8 {
                            3 => BadVariant::TrailingPair,
                            4 => BadVariant::MerkleRoot,
                            _ => BadVariant::DuplicateTransaction,
                        },
                    },
                    Op::Pass {
                        class: 0,
                        peer: 0,
                        delta: 0,
                    },
                ]);
            }
            6 => {
                header.profile = 2;
            }
            _ => {
                ops.extend([
                    Op::Headers {
                        peer: 1,
                        from: Choice {
                            mode: 1,
                            index: 100,
                        },
                        count: 7,
                    },
                    Op::Pass {
                        class: 0,
                        peer: 0,
                        delta: 0,
                    },
                    Op::Serve {
                        rounds: 8,
                        peers: 3,
                        withhold: None,
                    },
                    Op::CmpctBlock {
                        peer: 0,
                        which: Choice::absolute(107),
                        nonce: seed,
                        prefilled: 0b100100101,
                    },
                    Op::Pass {
                        class: 0,
                        peer: 0,
                        delta: 0,
                    },
                    Op::BlockTxn {
                        peer: 0,
                        source: 0,
                        index: 0,
                    },
                    Op::Pass {
                        class: 0,
                        peer: 0,
                        delta: 0,
                    },
                ]);
            }
        }
        let bytes = encode(&header, &ops).expect("decoder inverse");
        let (decoded, ops) = decode(&bytes).unwrap();
        for variant in
            [BadVariant::TrailingPair, BadVariant::MerkleRoot, BadVariant::DuplicateTransaction]
        {
            counts[4 + variant as usize] += usize::from(
                ops.iter().any(|op| matches!(op,Op::BadBlock {variant:v,..} if *v == variant)),
            );
        }
        counts[7] += usize::from(
            ops.iter().any(|op| matches!(op,Op::CmpctBlock {prefilled,..} if prefilled & !1 != 0)),
        );
        let direct = decoded.profile == 2;
        let r = agree(
            &Script {
                header: decoded,
                ops,
            }
            .session()
            .unwrap(),
        );
        counts[0] += usize::from(views(&r).any(|v| !v.globals.blocks_in_flight.is_empty()));
        if matches!(input % 8, 3..=5) {
            assert_eq!(last(&r).tip.1, 104, "Core: invalid block leaves its parent active");
            assert!(
                views(&r).flat_map(|v| &v.misbehaving).flatten().any(|line| line.contains("100")),
                "Core: invalid block was processed"
            );
        }
        if input % 8 == 7 {
            assert_eq!(last(&r).tip.1, 108, "Core: noncoinbase-prefilled compact connects");
            assert!(
                messages(&r, "getblocktxn").any(|p| request_indexes(p)
                    .is_some_and(|(hash, indexes)| hash == *b(108).hash().as_bytes()
                        && indexes == [1, 3, 4, 6, 7])),
                "Core: noncoinbase-prefilled complement"
            );
        }
        counts[1] += usize::from(matches!(input % 8, 1 | 2 | 7) && last(&r).tip.1 > 100);
        counts[8] += usize::from(
            direct
                && messages(&r, "getdata").any(|p| p.get(1..5) == Some(&20_u32.to_le_bytes()[..])),
        );
        counts[2] += usize::from(messages(&r, "getblocktxn").count() > 0);
        counts[3] += usize::from(views(&r).any(|v| !v.globals.hb_cmpct_announcers.is_empty()));
        counts[9] += usize::from(
            views(&r).any(|v| v.peers.iter().flatten().any(|p| p.stalling_since_us != 0)),
        );
    }
    eprintln!("reach counts: {counts:?}");
    for (actual, floor) in counts.into_iter().zip([40, 8, 8, 6, 4, 4, 4, 4, 2, 1]) {
        assert!(actual >= floor, "reach floor {floor}, got {actual}");
    }
    for script in [stall_script(), timeout_script(), decay_script()] {
        let bytes = encode(&script.header, &script.ops).unwrap();
        let (header, ops) = decode(&bytes).unwrap();
        assert_eq!(header, script.header);
        assert_eq!(ops, script.ops);
        let r = agree(
            &Script {
                header,
                ops,
            }
            .session()
            .unwrap(),
        );
        assert!(views(&r).any(|v| v.peers.iter().flatten().any(|p| p.stalling_since_us != 0)));
    }
}

#[test]
fn an_early_send_return_keeps_pending_header_relay() {
    let mut s = base(3, 3);
    let bad = bad_block(&b(105), BadVariant::TrailingPair).unwrap();
    s.steps.push(Step::Batch(vec![
        deliver(0, "block", core_serialize(&*b(101))),
        deliver(1, "block", core_serialize(&*bad)),
    ]));
    pass(&mut s);
    let r = agree(&s);
    let order = r.pass_orders.last().unwrap();
    assert!(order.iter().position(|p| *p == 0) < order.iter().position(|p| *p == 1));
    let v = last(&r);
    assert_eq!(v.tip.1, 101);
    assert!(v.disconnect[1], "Core: the mutated block disconnects its source");
    assert!(v.misbehaving[1].iter().any(|line| line.contains("100")));
    assert_eq!(v.relay[1], vec![*b(101).hash().as_bytes()]);
}

fn has_noncoinbase_prefilled(payload: &[u8]) -> bool {
    let short_ids = payload[88];
    assert!(short_ids < 253, "fixture short-ID count fits one byte");
    let offset = 89 + 6 * usize::from(short_ids);
    let count = payload[offset];
    assert!(count < 253, "fixture prefilled count fits one byte");
    count > 1 || (count == 1 && payload[offset + 1] != 0)
}

#[test]
fn bytes_reach_the_download_states() {
    assert_eq!(compare_p2p_multi_blocks(&[], Pin::V24_0), Ok(Outcome::OutOfDomain));
    let invalid: Vec<Vec<_>> =
        [BadVariant::TrailingPair, BadVariant::MerkleRoot, BadVariant::DuplicateTransaction]
            .into_iter()
            .map(|variant| {
                fixture()
                    .unwrap()
                    .workload
                    .iter()
                    .filter_map(|block| bad_block(block, variant).ok())
                    .map(|block| core_serialize(&*block))
                    .collect()
            })
            .collect();
    let mut counts = [0; 10];
    let mut state = 238;
    for input in 0..64 {
        let bytes: Vec<_> = (0..128).flat_map(|_| splitmix(&mut state).to_le_bytes()).collect();
        let (header, ops) = decode(&bytes).unwrap();
        let blocks = chain(header.profile);
        let direct = header.profile == 2;
        let initial_height = if header.profile >= 2 {
            100
        } else {
            0
        };
        let r = report_session(
            &Oracle::new(Pin::V24_0),
            &Script {
                header,
                ops,
            }
            .session()
            .unwrap(),
        )
        .unwrap()
        .unwrap();
        assert!(r.difference.is_none(), "input {input}: {}", r.difference.as_deref().unwrap_or(""));
        let delivered: Vec<_> = r
            .steps
            .iter()
            .zip(&r.downloads)
            .filter_map(|(step, view)| view.as_ref().map(|_| step))
            .collect();
        let delivered_hashes = |command| {
            delivered
                .iter()
                .copied()
                .filter_map(|step| match step {
                    Step::Deliver {
                        command: c,
                        payload,
                        ..
                    } if *c == command => blocks
                        .iter()
                        .find(|b| payload[..80] == core_serialize(b.header()))
                        .map(|b| *b.hash().as_bytes()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for (i, variants) in invalid.iter().enumerate() {
            counts[4 + i] += usize::from(delivered.iter().copied().any(|step| {
                matches!(step,
                Step::Deliver { command: "block", payload, .. } if variants.contains(payload))
            }));
        }
        counts[7] += usize::from(delivered.iter().copied().any(|step| matches!(step,
            Step::Deliver { command: "cmpctblock", payload, .. } if has_noncoinbase_prefilled(payload))));
        let compact = delivered_hashes("cmpctblock");
        let whole = delivered_hashes("block");
        counts[0] += usize::from(views(&r).any(|v| !v.globals.blocks_in_flight.is_empty()));
        counts[1] += usize::from(views(&r).any(|v| {
            v.tip.1 > initial_height && compact.contains(&v.tip.0) && !whole.contains(&v.tip.0)
        }));
        counts[2] += usize::from(messages(&r, "getblocktxn").count() > 0);
        counts[3] += usize::from(views(&r).any(|v| !v.globals.hb_cmpct_announcers.is_empty()));
        counts[8] += usize::from(
            direct
                && messages(&r, "getdata").any(|p| matches!(p.get(1..5), Some([2 | 20, 0, 0, 0]))),
        );
        counts[9] += usize::from(
            views(&r).any(|v| v.peers.iter().flatten().any(|p| p.stalling_since_us != 0)),
        );
    }
    eprintln!("reach counts: {counts:?}");
    for (actual, floor) in counts.into_iter().zip([40, 8, 8, 6, 4, 4, 4, 4, 2, 1]) {
        assert!(actual >= floor, "reach floor {floor}, got {actual}");
    }
}
