//! `block_download_model` (P3 §6.5): §6.1's in-flight accounting as a model target, with no
//! oracle. [`run`] applies a sequence of `BlockRequested`, `RemoveBlockRequest`, `FinalizeNode`
//! and section 12's stall start to [`BlockDownload`] and to [`Model`], a direct transliteration
//! of Core's structures, and compares the two after every step. `node-fuzz`'s model target of
//! that name runs [`check`]; this module's proptest runs [`run`] (P3 D106).
//!
//! Input: the first byte is the peers' inbound mask (bit `p` for peer `p`; Core records
//! `m_is_inbound` at connect, in `InitializeNode`). Then each step is three bytes, `op`, `a` and
//! `b`, and a fourth, read as an `i8`, that moves the clock by that many µs. The clock may step
//! back, as Core's `GetTime<µs>()` does under mocktime or a wall-clock change, which is what
//! `RemoveBlockRequest`'s `std::max` is for (np:1310). It starts at 10⁹ µs, so it stays positive
//! for any input under 31 MB. `op % 8` picks the step: 0–2 `Request(a, b)`, 3–5 `Remove(a, b)`,
//! 6 `Finalize(a)`, 7 `Stall(a, b)`.
//!
//! Compared after every step: `m_peers_downloading_from`, `mapBlocksInFlight.size()`, each peer's
//! `vBlocksInFlight` and its stats mirror, `m_downloading_since` and `m_stalling_since`,
//! `m_is_inbound`, and for every block `IsBlockRequested`, `IsBlockRequestedFromOutbound` and
//! each peer's `cmpctblock` in-flight facts (np:5124-5138). The last ones read the order of a
//! block's `mapBlocksInFlight` entries: its first peer is both `first_in_flight` and
//! `FindNextBlocksToDownload`'s staller (np:1554).
//!
//! One shape follows the Rust side rather than Core: a peer's state exists from its first request
//! to its `FinalizeNode`, where Core's `CNodeState` exists from `InitializeNode`. Core reads none
//! of these fields for a peer that never requested a block, so the two are indistinguishable here
//! (P3 D108).

use std::collections::HashMap;

use dash_consensus::BlockId;
use dash_p2p::PeerId;
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

use crate::processing::download::{BlockDownload, HeightsCell, InFlightFacts, PeerFacts};
use crate::processing::seams::{BlockSync as _, EntryFacts, Work};

/// Peers `0..PEERS`.
const PEERS: u8 = 8;
/// Blocks `0..BLOCKS`.
const BLOCKS: u8 = 16;

/// One step of the sequence.
#[derive(Clone, Copy, Debug)]
enum Op {
    /// `BlockRequested(peer, block)`.
    Request(u8, u8),
    /// `RemoveBlockRequest(block, from)`; `None` removes every peer's entry.
    Remove(u8, Option<u8>),
    /// `FinalizeNode(peer)`'s block-download part.
    Finalize(u8),
    /// Section 12's tail for `peer` with `FindNextBlocksToDownload`'s staller (np:6832-6837).
    /// Core names a staller only when one of its blocks is in flight, so a step whose staller
    /// has none is skipped on both sides.
    Stall(u8, Option<u8>),
}

impl Op {
    fn decode(op: u8, a: u8, b: u8) -> Self {
        match op % 8 {
            0..=2 => Op::Request(a % PEERS, b % BLOCKS),
            3..=5 => Op::Remove(a % BLOCKS, (b < 0x80).then_some(b % PEERS)),
            6 => Op::Finalize(a % PEERS),
            _ => Op::Stall(a % PEERS, (b < 0x80).then_some(b % PEERS)),
        }
    }
}

/// Block `b`'s entry: its height is `b + 100`.
fn entry(b: u8) -> EntryFacts {
    EntryFacts {
        id: BlockId(u32::from(b)),
        hash: BlockHash::from_lendian([b; 32]),
        prev: None,
        height: u32::from(b) + 100,
        time: 0,
        chain_work: Work::ONE,
        header: Default::default(),
    }
}

/// Core's structures transliterated: `mapBlocksInFlight` as an insertion-ordered list of
/// `(hash, peer, list entry)`, each peer's `std::list` as a vector of entries, and each peer's
/// `CNodeState` fields.
#[derive(Default)]
struct Model {
    map: Vec<(u8, PeerId, u64)>,
    lists: HashMap<PeerId, Vec<(u64, u8)>>,
    /// `(m_downloading_since, m_stalling_since)`.
    since: HashMap<PeerId, (i64, i64)>,
    inbound: HashMap<PeerId, bool>,
    downloading_from: i32,
    next: u64,
}

impl Model {
    // Core: net_processing.cpp:1287-1322 RemoveBlockRequest
    fn remove(&mut self, b: u8, from: Option<PeerId>, now: i64) {
        let mut i = 0;
        while i < self.map.len() {
            let (h, peer, it) = self.map[i];
            if h != b || from.is_some_and(|f| f != peer) {
                i += 1;
                continue;
            }
            let list = self.lists.get_mut(&peer).expect("an entry's peer has a list");
            let times = self.since.get_mut(&peer).expect("an entry's peer has a state");
            if list.first().is_some_and(|e| e.0 == it) {
                times.0 = times.0.max(now);
            }
            list.retain(|e| e.0 != it);
            if list.is_empty() {
                self.downloading_from -= 1;
            }
            times.1 = 0;
            self.map.remove(i);
        }
    }

    // Core: net_processing.cpp:1324-1358 BlockRequested
    fn request(&mut self, peer: PeerId, inbound: bool, b: u8, now: i64) -> bool {
        if self.map.iter().any(|&(h, p, _)| h == b && p == peer) {
            return false;
        }
        self.remove(b, Some(peer), now);
        let it = self.next;
        self.next += 1;
        let times = self.since.entry(peer).or_default();
        self.inbound.entry(peer).or_insert(inbound);
        let list = self.lists.entry(peer).or_default();
        list.push((it, b));
        if list.len() == 1 {
            times.0 = now;
            self.downloading_from += 1;
        }
        self.map.push((b, peer, it));
        true
    }

    // Core: net_processing.cpp:1804-1830 FinalizeNode
    fn finalize(&mut self, peer: PeerId) {
        let Some(list) = self.lists.remove(&peer) else {
            return;
        };
        self.since.remove(&peer);
        self.inbound.remove(&peer);
        for (_, b) in &list {
            self.map.retain(|&(h, p, _)| !(h == *b && p == peer));
        }
        self.downloading_from -= i32::from(!list.is_empty());
    }

    /// Whether `peer` has a block in flight.
    fn in_flight(&self, peer: PeerId) -> bool {
        self.lists.get(&peer).is_some_and(|l| !l.is_empty())
    }

    // Core: net_processing.cpp:6832-6837
    fn start_stall(&mut self, peer: PeerId, staller: PeerId, now: i64) {
        if self.in_flight(peer) {
            return;
        }
        let s = self.since.get_mut(&staller).expect("the staller has a block in flight");
        if s.1 == 0 {
            s.1 = now;
        }
    }
}

/// Applies `ops` from clock 10⁹ µs, each with its clock step, with `inbound_mask`'s bit `p` as
/// peer `p`'s `m_is_inbound`; the error names the first field that differs.
fn run(inbound_mask: u8, ops: &[(Op, i8)]) -> Result<(), &'static str> {
    let mut bd = BlockDownload::new();
    let mut model = Model::default();
    let heights: Vec<HeightsCell> = (0..PEERS).map(|_| HeightsCell::default()).collect();
    let mut now = 1_000_000_000_i64;
    for &(op, step) in ops {
        now += i64::from(step);
        match op {
            Op::Request(p, b) => {
                let inbound = (inbound_mask >> p) & 1 == 1;
                let facts = PeerFacts {
                    id: i64::from(p),
                    inbound,
                    heights: &heights[usize::from(p)],
                };
                let got = bd.block_requested(facts, &entry(b), now);
                if got != model.request(i64::from(p), inbound, b, now) {
                    return Err("block_requested");
                }
            }
            Op::Remove(b, from) => {
                let from = from.map(i64::from);
                bd.remove_block_request(&entry(b).hash, from, now);
                model.remove(b, from, now);
            }
            Op::Finalize(p) => {
                bd.finalize(i64::from(p));
                model.finalize(i64::from(p));
            }
            Op::Stall(p, staller) => {
                let staller = staller.map(i64::from);
                if let Some(s) = staller.filter(|&s| model.in_flight(s)) {
                    model.start_stall(i64::from(p), s, now);
                    bd.start_stall(i64::from(p), Some(s), now);
                } else if staller.is_none() {
                    bd.start_stall(i64::from(p), None, now);
                }
            }
        }
        compare(&bd, &model, &heights)?;
    }
    Ok(())
}

fn compare(bd: &BlockDownload, model: &Model, heights: &[HeightsCell]) -> Result<(), &'static str> {
    if bd.peers_downloading_from() != model.downloading_from {
        return Err("peers_downloading_from");
    }
    if bd.in_flight_entries() != model.map.len() {
        return Err("in_flight_entries");
    }
    for p in 0..i64::from(PEERS) {
        let got: Vec<u8> = bd.queued(p).iter().map(|q| q.hash.as_bytes()[0]).collect();
        let want: Vec<u8> = model.lists.get(&p).into_iter().flatten().map(|e| e.1).collect();
        if got != want {
            return Err("queued");
        }
        if bd.times(p) != model.since.get(&p).copied() {
            return Err("times");
        }
        if bd.is_inbound(p) != model.inbound.get(&p).copied() {
            return Err("is_inbound");
        }
        if bd.times(p).is_some() {
            let hs: Vec<i32> = want.iter().map(|&b| i32::from(b) + 100).collect();
            if *heights[p as usize].lock() != hs {
                return Err("heights");
            }
        }
    }
    for b in 0..BLOCKS {
        let hash = entry(b).hash;
        let peers: Vec<PeerId> = model.map.iter().filter(|e| e.0 == b).map(|e| e.1).collect();
        if bd.is_block_requested(&hash) != !peers.is_empty() {
            return Err("is_block_requested");
        }
        let outbound = peers.iter().any(|p| !model.inbound[p]);
        if bd.is_block_requested_from_outbound(&hash) != outbound {
            return Err("is_block_requested_from_outbound");
        }
        for p in 0..i64::from(PEERS) {
            let want = InFlightFacts {
                already: peers.len(),
                first: peers.first().is_none_or(|&f| f == p),
                from_this_peer: peers.contains(&p),
            };
            if bd.in_flight_facts(&hash, p) != want {
                return Err("in_flight_facts");
            }
        }
    }
    Ok(())
}

/// The target's check (the layout is in the module docs); the error names the first field that
/// differs.
pub fn check(data: &[u8]) -> Result<(), &'static str> {
    let Some((&mask, rest)) = data.split_first() else {
        return Ok(());
    };
    let ops: Vec<(Op, i8)> = rest
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&[op, a, b, step]| (Op::decode(op, a, b), step.cast_signed()))
        .collect();
    run(mask, &ops)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn op() -> impl Strategy<Value = Op> {
        let peer = 0..PEERS;
        let block = 0..BLOCKS;
        prop_oneof![
            4 => (peer.clone(), block.clone()).prop_map(|(p, b)| Op::Request(p, b)),
            3 => (block, proptest::option::of(peer.clone())).prop_map(|(b, p)| Op::Remove(b, p)),
            1 => peer.clone().prop_map(Op::Finalize),
            1 => (peer.clone(), proptest::option::of(peer)).prop_map(|(p, s)| Op::Stall(p, s)),
        ]
    }

    proptest! {
        /// `block_requested`, `remove_block_request`, `finalize` and the stall start keep Core's
        /// accounting (P3 §6.1 in-flight accounting).
        #[test]
        fn in_flight_accounting_matches_core(
            mask in any::<u8>(),
            ops in proptest::collection::vec((op(), -16..16i8), 1..120),
        ) {
            prop_assert_eq!(run(mask, &ops), Ok(()));
        }
    }

    /// A deterministic sweep of [`check`]'s byte layout; `node-fuzz` runs the real search.
    #[test]
    fn check_holds_on_a_sweep() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for len in 0..600 {
            let data: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if let Err(e) = check(&data) {
                panic!("{e}: {}", crate::sim::transcript::hex(&data));
            }
        }
    }
}
