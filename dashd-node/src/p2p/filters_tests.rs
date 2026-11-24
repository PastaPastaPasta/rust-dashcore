//! [`FilterSlot`] under the processor's compact-filter handlers, over `MemFilterIndex` filled
//! from the filters `SimFilterIndex` builds.

use std::collections::HashMap;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_p2p::{CommandString, Permissions};
use dash_params::{ChainParamsBuilder, Network as CoreNetwork, Release};
use dash_primitives::BlockHash;
use dashd_indexes::api::{
    Ancestry, BlockFilter, BlockRef, FilterHash, FilterHeader, FilterIndexReader, IndexError,
    IndexStatus, IndexSummary,
};
use dashd_indexes::testing::MemFilterIndex;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity, local_services};
use dashd_p2p::processing::seams::{BlockFilterIndex, ChainAccess, EntryFacts, TreeRead};
use dashd_p2p::sim::filters::SimFilterIndex;
use dashd_p2p::sim::transcript::Hooks;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimChain, SimPeer};
use dashd_p2p::{ProcessorOptions, Registry, Seams};

use super::FilterSlot;

fn chain() -> Arc<SimChain> {
    let params =
        Arc::new(ChainParamsBuilder::new(CoreNetwork::Regtest, Release::V24_0).build().unwrap());
    let chain = SimChain::new(params);
    chain.store_genesis_data();
    chain
}

fn extend(chain: &SimChain, base: &EntryFacts, n: usize, salt: u8) -> Vec<EntryFacts> {
    let blocks = SimChain::build_blocks(base, n, 1, salt);
    chain.store_blocks(&blocks);
    std::iter::once(*base).chain(blocks.iter().map(|b| chain.by_hash(&b.hash()).unwrap())).collect()
}

/// Copies `built`'s entries on `tip`'s ancestry into `entries`, by block hash.
fn mirror(
    chain: &SimChain,
    built: &SimFilterIndex,
    entries: &mut HashMap<BlockHash, (BlockFilter, FilterHeader)>,
    tip: BlockId,
) {
    built.index_coinbase_chain(tip);
    let mut next = Some(tip);
    while let Some(entry) = next.and_then(|id| chain.entry(id)) {
        if entries.contains_key(&entry.hash) {
            break;
        }
        let encoded = built.filter(entry.id).unwrap();
        let filter = BlockFilter::from_encoded(&entry.hash, encoded, false).unwrap();
        entries.insert(entry.hash, (filter, built.filter_header(entry.id).unwrap()));
        next = entry.prev;
    }
}

fn mem(entries: &HashMap<BlockHash, (BlockFilter, FilterHeader)>, best: u32) -> MemFilterIndex {
    let mut index = MemFilterIndex {
        entries: entries.clone(),
        ..MemFilterIndex::default()
    };
    index.status.best_block_height = best;
    index
}

fn node(chain: &Arc<SimChain>, slot: &Arc<FilterSlot>) -> Sim {
    let params = Arc::clone(chain.params());
    let opts = IdentityOptions {
        peer_block_filters: true,
        ..IdentityOptions::new()
    };
    let identity = local_identity(&params, &opts).unwrap();
    slot.bind_chain(Arc::clone(chain) as _);
    let chain = Arc::clone(chain);
    Sim::with_chain(
        params,
        identity,
        ProcessorOptions::default(),
        Registry::new(),
        Seams {
            filter_index: Arc::clone(slot) as _,
            ..Seams::default()
        },
        SIM_START,
        |_| chain,
        |cfg| {
            cfg.peer_timeout = 60;
            cfg.local_services = local_services(&opts);
        },
    )
}

struct Mirror {
    built: SimFilterIndex,
    entries: HashMap<BlockHash, (BlockFilter, FilterHeader)>,
    slot: Arc<FilterSlot>,
}

impl Hooks for Mirror {
    fn settled(&mut self, sim: &mut Sim) {
        let tip = sim.chain.snapshot().tip;
        mirror(&sim.chain, &self.built, &mut self.entries, tip.tip);
        self.slot.attach(Arc::new(mem(&self.entries, tip.tip_height)));
    }
}

/// Core v24.0.0's recorded cfilter, cfheaders and cfcheckpt bytes, served through the slot.
#[test]
fn slot_replays_core_serve_cfilters_transcript() {
    let chain = chain();
    let slot = Arc::new(FilterSlot::default());
    let sim = node(&chain, &slot);
    let mut hooks = Mirror {
        built: SimFilterIndex::new(Arc::clone(&chain)),
        entries: HashMap::new(),
        slot,
    };
    let name = "serve-cfilters";
    let path =
        format!("{}/../dashd-p2p/tests/fixtures/dashd-v24/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    dashd_p2p::sim::transcript::replay(name, &text, sim, &mut hooks);
}

/// Every seam read through the slot equals `SimFilterIndex`'s, on the active and a stale branch.
#[test]
fn slot_reads_equal_built_index_on_both_branches() {
    let chain = chain();
    let main = extend(&chain, &chain.genesis(), 1001, 0x11);
    let side = extend(&chain, &main[999], 3, 0x22);
    let built = SimFilterIndex::new(Arc::clone(&chain));
    let mut entries = HashMap::new();
    mirror(&chain, &built, &mut entries, side[3].id);
    mirror(&chain, &built, &mut entries, main[1001].id);
    chain.set_tip(main[1001].id);
    let slot = FilterSlot::default();
    slot.bind_chain(Arc::clone(&chain) as _);
    assert!(!slot.available());
    assert_eq!(slot.filters(0, main[1].id), None);
    slot.attach(Arc::new(mem(&entries, 1001)));
    assert!(slot.available());
    for stop in [main[0], main[1000], main[1001], side[1], side[3]] {
        for start in [0, 1, 999, 1000, 1001, 1002] {
            assert_eq!(slot.filters(start, stop.id), built.filters(start, stop.id));
            assert_eq!(slot.filter_hashes(start, stop.id), built.filter_hashes(start, stop.id));
        }
        assert_eq!(slot.filter(stop.id), built.filter(stop.id));
        assert_eq!(slot.filter_header(stop.id), built.filter_header(stop.id));
    }
    assert_eq!(slot.filters(1000, side[3].id).unwrap()[1].0, side[2].hash);
    assert_eq!(slot.best_height(), Some(1001));
    slot.attach(Arc::new(mem(&entries, 1002)));
    assert_eq!(slot.best_height(), Some(1001));
    slot.attach(Arc::new(mem(&HashMap::new(), 0)));
    assert_eq!(slot.best_height(), None);
    slot.attach(Arc::new(mem(&entries, 0)));
    assert_eq!(slot.best_height(), Some(0));
    slot.detach();
    assert!(!slot.available());
    assert_eq!(slot.filter(main[0].id), None);
}

fn peer(sim: &mut Sim, n: u8) -> SimPeer {
    let p = sim.accept(n, Permissions::NONE).unwrap();
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    sim.pass();
    sim.sent(&p);
    p
}

/// Each request for a valid stop: the replies sent, and whether the peer was disconnected.
fn ask_all(sim: &mut Sim, stop: BlockHash) -> Vec<(usize, bool)> {
    [("getcfilters", Some(1)), ("getcfheaders", Some(1)), ("getcfcheckpt", None)]
        .into_iter()
        .enumerate()
        .map(|(i, (cmd, start))| {
            let p = peer(sim, i as u8 + 1);
            let mut data = vec![0];
            if let Some(start) = start {
                data.extend_from_slice(&u32::to_le_bytes(start));
            }
            data.extend_from_slice(stop.as_bytes());
            sim.deliver(&p, &[(cmd, data)]);
            sim.pass();
            let replies = sim
                .sent(&p)
                .into_iter()
                .filter(|(c, _)| {
                    [CommandString::CFILTER, CommandString::CFHEADERS, CommandString::CFCHECKPT]
                        .contains(c)
                })
                .count();
            assert_eq!(sim.shared.misbehavior(p.handle.id()).unwrap().0, 0);
            (replies, p.handle.disconnect_requested())
        })
        .collect()
}

/// Core's `Filter index for supported type basic not found`: ignored, the peer kept.
// Core: net_processing.cpp:3480-3483 at v24.0.0
#[test]
fn no_attached_reader_ignores_requests() {
    let chain = chain();
    let c = extend(&chain, &chain.genesis(), 1000, 0x11);
    chain.set_tip(c[1000].id);
    chain.set_ibd(false);
    let slot = Arc::new(FilterSlot::default());
    let mut sim = node(&chain, &slot);
    assert_eq!(ask_all(&mut sim, c[1000].hash), [(0, false); 3]);
}

struct Failing;

impl IndexStatus for Failing {
    fn summary(&self) -> IndexSummary {
        MemFilterIndex::default().summary()
    }

    fn block_until_synced(&self) -> bool {
        false
    }
}

fn io<T>() -> Result<T, IndexError> {
    Err(IndexError::Io("unreadable".to_owned()))
}

impl FilterIndexReader for Failing {
    fn filter(&self, _: &BlockRef) -> Result<Option<BlockFilter>, IndexError> {
        io()
    }

    fn filter_header(&self, _: &BlockRef) -> Result<Option<FilterHeader>, IndexError> {
        io()
    }

    fn filter_range(
        &self,
        _: u32,
        _: &BlockRef,
        _: &dyn Ancestry,
    ) -> Result<Option<Vec<BlockFilter>>, IndexError> {
        io()
    }

    fn filter_hash_range(
        &self,
        _: u32,
        _: &BlockRef,
        _: &dyn Ancestry,
    ) -> Result<Option<Vec<FilterHash>>, IndexError> {
        io()
    }
}

/// A failed lookup: Core's `Failed to find block filter ...` and an ignored request, the peer
/// kept; a working reader attached later serves.
// Core: net_processing.cpp:3507, :3541, :3549, :3589 at v24.0.0
#[test]
fn reader_error_ignores_requests() {
    let chain = chain();
    let c = extend(&chain, &chain.genesis(), 1000, 0x11);
    chain.set_tip(c[1000].id);
    chain.set_ibd(false);
    let slot = Arc::new(FilterSlot::default());
    let mut sim = node(&chain, &slot);
    slot.attach(Arc::new(Failing));
    assert_eq!(ask_all(&mut sim, c[1000].hash), [(0, false); 3]);
    assert_eq!(slot.best_height(), None);
    let built = SimFilterIndex::new(Arc::clone(&chain));
    let mut entries = HashMap::new();
    mirror(&chain, &built, &mut entries, c[1000].id);
    slot.attach(Arc::new(mem(&entries, 1000)));
    assert_eq!(ask_all(&mut sim, c[1000].hash), [(1000, false), (1, false), (1, false)]);
}
