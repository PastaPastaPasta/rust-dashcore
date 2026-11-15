//! Compact-filter serving, including the request cases in Core's p2p_blockfilters.py.

use std::sync::Arc;

use dash_num::Hash256;
use dash_p2p::{CommandString, Permissions};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::BlockHash;
use dash_types::Numeric;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity, local_services};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::{BlockFilterIndex, EntryFacts};
use dashd_p2p::sim::filters::SimFilterIndex;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimChain, SimPeer};
use dashd_p2p::{Registry, Seams};

fn node(n: usize, enabled: bool) -> (Sim, Arc<SimFilterIndex>, Vec<EntryFacts>) {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let opts = IdentityOptions {
        peer_block_filters: enabled,
        ..IdentityOptions::new()
    };
    let identity = local_identity(&params, &opts).unwrap();
    let chain = SimChain::new(Arc::clone(&params));
    chain.store_genesis_data();
    let index = Arc::new(SimFilterIndex::new(Arc::clone(&chain)));
    let sim = Sim::with_chain(
        params,
        identity,
        ProcessorOptions::default(),
        Registry::new(),
        Seams {
            filter_index: index.clone(),
            ..Seams::default()
        },
        SIM_START,
        |_| chain,
        |cfg| cfg.local_services = local_services(&opts),
    );
    let entries = extend(&sim, &sim.chain.genesis(), n, 0x11);
    sim.chain.set_tip(entries[n].id);
    sim.chain.set_ibd(false);
    index.index_coinbase_chain(entries[n].id);
    (sim, index, entries)
}

fn extend(sim: &Sim, base: &EntryFacts, n: usize, salt: u8) -> Vec<EntryFacts> {
    let blocks = SimChain::build_blocks(base, n, 1, salt);
    if blocks.is_empty() {
        return vec![*base];
    }
    sim.chain.store_blocks(&blocks);
    std::iter::once(*base)
        .chain(blocks.iter().map(|b| sim.chain.by_hash(&b.hash()).unwrap()))
        .collect()
}

fn peer(sim: &mut Sim, n: u8) -> SimPeer {
    let p = sim.accept(n, Permissions::NONE).unwrap();
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    sim.pass();
    sim.sent(&p);
    p
}

fn request(ty: u8, start: Option<u32>, stop: BlockHash) -> Vec<u8> {
    let mut data = vec![ty];
    if let Some(start) = start {
        data.extend_from_slice(&start.to_le_bytes());
    }
    data.extend_from_slice(stop.as_bytes());
    data
}

fn ask(sim: &mut Sim, p: &SimPeer, cmd: &str, start: Option<u32>, stop: BlockHash) -> Vec<Vec<u8>> {
    sim.deliver(p, &[(cmd, request(0, start, stop))]);
    sim.pass();
    sim.sent(p)
        .into_iter()
        .filter(|(c, _)| {
            matches!(
                *c,
                CommandString::CFILTER | CommandString::CFHEADERS | CommandString::CFCHECKPT
            )
        })
        .map(|(_, payload)| payload)
        .collect()
}

fn encoded_hashes(hashes: &[Hash256]) -> Vec<u8> {
    use dash_types::codec::BaseCodec;
    let mut out = Vec::new();
    dash_types::CompactSize::from(hashes.len() as u64).encode(&mut out);
    for hash in hashes {
        out.extend_from_slice(hash.as_bytes());
    }
    out
}

#[test]
fn ranges_and_headers_match_built_filters() {
    let (mut sim, index, c) = node(2000, true);
    let p = peer(&mut sim, 1);
    let filters = ask(&mut sim, &p, "getcfilters", Some(1), c[1000].hash);
    assert_eq!(filters.len(), 1000);
    for (payload, entry) in filters.iter().zip(&c[1..=1000]) {
        assert_eq!(payload[0], 0);
        assert_eq!(&payload[1..33], entry.hash.as_bytes());
        let filter = index.filter(entry.id).unwrap();
        assert_eq!(payload[33] as usize, filter.len());
        assert_eq!(&payload[34..], filter);
    }
    for start in [0, 1, 1001] {
        let stop = if start == 0 {
            1999
        } else {
            2000
        };
        let reply = ask(&mut sim, &p, "getcfheaders", Some(start), c[stop].hash);
        let prev = if start == 0 {
            Hash256::ZERO
        } else {
            index.filter_header(c[start as usize - 1].id).unwrap()
        };
        let mut expected = request(0, None, c[stop].hash);
        expected.extend_from_slice(prev.as_bytes());
        expected.extend(encoded_hashes(&index.filter_hashes(start, c[stop].id).unwrap()));
        assert_eq!(reply, [expected]);
    }
    for stop in [0, 999, 1000, 1999, 2000] {
        let reply = ask(&mut sim, &p, "getcfcheckpt", None, c[stop].hash);
        let headers: Vec<_> =
            (1000..=stop).step_by(1000).map(|h| index.filter_header(c[h].id).unwrap()).collect();
        let mut expected = request(0, None, c[stop].hash);
        expected.extend(encoded_hashes(&headers));
        assert_eq!(reply, [expected]);
    }
    assert_eq!(index.best_height(), Some(2000));
}

#[test]
fn invalid_requests_disconnect_without_scoring() {
    let (mut sim, _, c) = node(2000, true);
    let cases = [
        ("getcfilters", 0, Some(0), c[1000].hash),
        ("getcfheaders", 0, Some(0), c[2000].hash),
        ("getcfilters", 0, Some(1000), c[999].hash),
        ("getcfheaders", 0, Some(u32::MAX), c[999].hash),
        ("getcfcheckpt", 255, None, c[1000].hash),
        ("getcfilters", 255, Some(0), c[0].hash),
        ("getcfheaders", 255, Some(0), c[0].hash),
        ("getcfcheckpt", 0, None, BlockHash::from_lendian([7; 32])),
    ];
    for (i, (cmd, ty, start, stop)) in cases.into_iter().enumerate() {
        let p = peer(&mut sim, i as u8 + 1);
        sim.deliver(&p, &[(cmd, request(ty, start, stop))]);
        sim.pass();
        assert!(p.handle.disconnect_requested(), "{cmd}, {i}");
        assert_eq!(sim.shared.misbehavior(p.handle.id()).unwrap().0, 0);
    }
}

#[test]
fn service_disabled_disconnects_all_three_requests() {
    let (mut sim, _, c) = node(1, false);
    for (i, (cmd, start)) in
        [("getcfilters", Some(0)), ("getcfheaders", Some(0)), ("getcfcheckpt", None)]
            .into_iter()
            .enumerate()
    {
        let p = peer(&mut sim, i as u8 + 1);
        ask(&mut sim, &p, cmd, start, c[0].hash);
        assert!(p.handle.disconnect_requested());
        assert_eq!(sim.shared.misbehavior(p.handle.id()).unwrap().0, 0);
    }
}

#[test]
fn side_chain_ranges_and_checkpoint_cache_survive_reorg() {
    let (mut sim, index, c) = node(2000, true);
    let side = extend(&sim, &c[999], 2, 0x22);
    sim.chain.set_tip(side[2].id);
    index.index_coinbase_chain(side[2].id);
    let p = peer(&mut sim, 1);
    let stale = ask(&mut sim, &p, "getcfcheckpt", None, side[1].hash);
    sim.chain.set_tip(c[2000].id);
    let main = ask(&mut sim, &p, "getcfcheckpt", None, c[2000].hash);
    assert_eq!(&main[0][34..66], index.filter_header(c[1000].id).unwrap().as_bytes());
    assert_eq!(stale, ask(&mut sim, &p, "getcfcheckpt", None, side[1].hash));
    assert_ne!(&stale[0][34..66], &main[0][34..66]);
    let filters = ask(&mut sim, &p, "getcfilters", Some(999), side[2].hash);
    assert_eq!(filters.len(), 3);
    assert_eq!(&filters[1][1..33], side[1].hash.as_bytes());
    let headers = ask(&mut sim, &p, "getcfheaders", Some(1001), side[2].hash);
    assert_eq!(&headers[0][33..65], index.filter_header(side[1].id).unwrap().as_bytes());
    assert_eq!(&headers[0][66..], index.filter_hashes(1001, side[2].id).unwrap()[0].as_bytes());
}

#[test]
fn unvalidated_or_old_side_chain_stop_disconnects() {
    let (mut sim, _, c) = node(20, true);
    let side = extend(&sim, &c[10], 2, 0x22);
    let p = peer(&mut sim, 1);
    ask(&mut sim, &p, "getcfilters", Some(11), side[1].hash);
    assert!(p.handle.disconnect_requested());
    sim.chain.set_tip(side[2].id);
    sim.chain.set_tip(c[20].id);
    sim.chain.add(&SimChain::build(&c[20], 1, 31 * 24 * 3600, 0x33));
    let p = peer(&mut sim, 2);
    ask(&mut sim, &p, "getcfcheckpt", None, side[1].hash);
    assert!(p.handle.disconnect_requested());
}

#[test]
fn index_failures_send_no_partial_reply_and_keep_peer() {
    let (mut sim, index, c) = node(1001, true);
    let p = peer(&mut sim, 1);
    index.remove(c[500].id);
    assert!(ask(&mut sim, &p, "getcfilters", Some(1), c[1000].hash).is_empty());
    assert!(ask(&mut sim, &p, "getcfheaders", Some(1), c[1000].hash).is_empty());
    assert!(ask(&mut sim, &p, "getcfheaders", Some(501), c[501].hash).is_empty());
    index.remove(c[1000].id);
    assert!(ask(&mut sim, &p, "getcfcheckpt", None, c[1001].hash).is_empty());
    assert!(!p.handle.disconnect_requested());
    assert_eq!(ask(&mut sim, &p, "getcfilters", Some(1001), c[1001].hash).len(), 1);
}

#[test]
fn installed_but_empty_index_can_serve_empty_checkpoints() {
    let (mut sim, index, c) = node(0, true);
    index.remove(c[0].id);
    assert_eq!(index.best_height(), None);
    let p = peer(&mut sim, 1);
    let mut expected = request(0, None, c[0].hash);
    expected.push(0);
    assert_eq!(ask(&mut sim, &p, "getcfcheckpt", None, c[0].hash), [expected]);
    assert!(ask(&mut sim, &p, "getcfheaders", Some(0), c[0].hash).is_empty());
    assert!(ask(&mut sim, &p, "getcfilters", Some(0), c[0].hash).is_empty());
    assert!(!p.handle.disconnect_requested());
}

#[test]
fn absent_index_ignores_valid_requests_after_validation() {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let opts = IdentityOptions {
        peer_block_filters: true,
        ..IdentityOptions::new()
    };
    let identity = local_identity(&params, &opts).unwrap();
    let mut sim = Sim::with(
        params,
        identity,
        ProcessorOptions::default(),
        Registry::new(),
        Seams::default(),
        |cfg| cfg.local_services = local_services(&opts),
    );
    let stop = sim.chain.genesis().hash;
    let p = peer(&mut sim, 1);
    for (cmd, start) in
        [("getcfilters", Some(0)), ("getcfheaders", Some(0)), ("getcfcheckpt", None)]
    {
        assert!(ask(&mut sim, &p, cmd, start, stop).is_empty());
        assert!(!p.handle.disconnect_requested());
    }
    ask(&mut sim, &p, "getcfilters", Some(1), stop);
    assert!(p.handle.disconnect_requested());
    assert_eq!(sim.shared.misbehavior(p.handle.id()).unwrap().0, 0);
}
