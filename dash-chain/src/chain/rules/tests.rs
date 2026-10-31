use dash_consensus::pow::get_next_work_required;
use dash_params::Network;
use dash_primitives::BlockHeader;
use dash_types::Numeric as _;

use super::*;
use crate::chain::HeaderChain;
use crate::config::ChainConfig;
use crate::status::Status;
use crate::testing::{fake_hash, params, services};

/// A mainnet-parameter node chain: genesis, then `n` synthetic headers 150 s apart at A-8's
/// `nBits` (no proof of work: `add_to_block_index` takes the hash as given). At exact spacing
/// KGW never crosses its event horizon, so each KGW-era target walks the whole window.
fn mainnet_chain(n: u32) -> (HeaderChain, Vec<BlockId>) {
    let p = params(Network::Main);
    let svc = services(&p);
    let mut chain = HeaderChain::new_genesis(p.clone(), ChainConfig::node(&p), svc, crate::NoStore)
        .expect("genesis");
    let mut ids = vec![BlockId(0)];
    for h in 1..=n {
        let prev = ids[h as usize - 1];
        ids.push(add(&mut chain, prev, u64::from(h), 1_000_000 + 150 * h));
    }
    (chain, ids)
}

fn add(chain: &mut HeaderChain, prev: BlockId, k: u64, time: u32) -> BlockId {
    let prev_hash = chain.shared.arena.imm(prev, |e| e.hash).expect("resident prev");
    let header = BlockHeader {
        version: 0x2000_0000,
        prev_hash,
        time,
        bits: 0x1e01_0000,
        nonce: k as u32,
        ..BlockHeader::default()
    };
    chain.add_to_block_index(&header, &fake_hash(k), Status(Status::VALID_TREE)).expect("added")
}

fn link(chain: &HeaderChain) -> impl FnMut(BlockId) -> Result<PathEntry, ChainError> + '_ {
    |id| {
        let prev = chain.shared.arena.imm(id, |e| e.prev).ok_or(ChainError::Internal("gone"))?;
        Ok(PathEntry {
            prev,
            cold: None,
        })
    }
}

fn time(chain: &HeaderChain) -> impl Fn(BlockId) -> Option<u32> + '_ {
    |id| chain.shared.arena.imm(id, |e| e.header.time)
}

/// Refreshes `cache` for the header on `prev` and checks the next target over the context
/// against the complete ancestry's, with no read outside the context.
fn check_next(chain: &HeaderChain, cache: &mut RuleCache, prev: BlockId) {
    let height = chain.shared.arena.imm(prev, |e| e.height).expect("resident");
    check_next_at(chain, cache, prev, height, 1_000_000 + 150 * (height + 1));
}

/// [`check_next`] for a next header at `at` on `prev`, at `height`.
fn check_next_at(chain: &HeaderChain, cache: &mut RuleCache, prev: BlockId, height: u32, at: u32) {
    let pow = chain.hp.pow();
    let envelope = cache.envelope(pow, height);
    cache
        .refresh(prev, height, envelope, (chain.floor, 0), link(chain), time(chain))
        .expect("refreshes");
    assert_eq!(cache.top_height(), Some(height));
    assert_eq!(cache.path.back(), Some(&prev));
    assert_eq!(cache.lo, (height + 1).saturating_sub(envelope));
    let ctx = RuleContext::new(&chain.shared.arena, cache);
    let complete = chain.complete().expect("a node chain");
    assert_eq!(
        get_next_work_required(&ctx, prev, at, pow),
        get_next_work_required(complete, prev, at, pow),
        "next height {}",
        height + 1
    );
    assert!(!ctx.read_outside(), "next height {}", height + 1);
}

#[test]
fn read_envelope_follows_the_dispatch() {
    // Mainnet: BTC retargets every 576, KGW from 15,200 (PastBlocksMax 4,032), DGW from 34,140.
    let p = params(Network::Main);
    let pow = HeaderChain::<crate::NoStore>::new_genesis(
        p.clone(),
        ChainConfig::node(&p),
        services(&p),
        crate::NoStore,
    )
    .expect("genesis")
    .hp;
    let r = |h| read_envelope(pow.pow(), h);
    assert_eq!([r(1), r(575), r(576), r(577), r(1152)], [11, 11, 576, 11, 576]);
    assert_eq!([r(15_199), r(15_200), r(34_139)], [11, 4_033, 4_033]);
    assert_eq!([r(34_140), r(2_000_000)], [25, 25]);

    // The other networks, from their own parameters.
    for network in [Network::Testnet3, Network::Regtest, Network::Devnet] {
        let p = params(network);
        let pow = dash_consensus::pow::PowParams::from_chain_params(&p).expect("valid");
        let c = *pow.config();
        let interval = (c.target_timespan / c.target_spacing) as u32;
        for h in [1u32, 2, interval, c.kgw_height as u32, c.dgw_height as u32, 1_000_000] {
            let next = i64::from(h);
            let era = if next - 1 < i64::from(c.minimum_difficulty_blocks) {
                0
            } else if next < i64::from(c.kgw_height) {
                match h % interval == 0 || c.allow_min_difficulty_blocks {
                    true => interval,
                    false => 1,
                }
            } else if c.no_retargeting {
                1
            } else if next < i64::from(c.dgw_height) {
                (7 * c.target_timespan / c.target_spacing + 1) as u32
            } else {
                25
            };
            assert_eq!(read_envelope(&pow, h), era.max(11), "{network:?} next height {h}");
        }
    }
}

/// A-5, A-6 and A-8 over synthetic headers: consecutive accepts with one warm cache through the
/// first BTC retarget, the KGW boundary, KGW's longest walk and the KGW → DGW switch.
#[test]
fn a_warm_context_matches_complete_ancestry() {
    let (chain, ids) = mainnet_chain(34_160);
    let mut cache = RuleCache::default();
    let ranges = [1..30, 560..600, 1_140..1_160, 15_190..15_210, 19_995..20_005, 34_130..34_150];
    for range in ranges {
        for next in range {
            check_next(&chain, &mut cache, ids[next as usize - 1]);
        }
    }
}

/// A-8's synthetic chain and Core's target for it (`tests/data/kgw-max-walk.json`, recorded
/// through `dash_oracle_sys::Oracle::next_work_required`, which
/// `dash-oracle-sys/tests/sb4_kgw_max_walk.rs` re-checks): `(parent height, next time, bits)`.
fn kgw_max_walk_core() -> (u32, u32, u32) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/kgw-max-walk.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("kgw-max-walk.json"))
            .expect("JSON");
    let n = |k: &str| u32::try_from(v[k].as_u64().expect(k)).expect("a u32");
    // `mainnet_chain`'s headers: time 1,000,000 + 150·h, A-8's bits.
    assert_eq!((n("time_base"), n("spacing"), n("bits")), (1_000_000, 150, 0x1e01_0000));
    let parent = n("parent_height");
    assert_eq!(parent - n("first_height") + 1, 4_033, "the walk and its guard read");
    (parent, 1_000_000 + 150 * (parent + 1), n("next_bits"))
}

/// A-8 resident: KGW's longest walk (4,032 entries down to 15,968 and the guard read of 15,967)
/// over a `RuleContext` equals `complete()`'s target, and that is Core's.
#[test]
fn kgw_max_walk_resident() {
    let (parent, time, core) = kgw_max_walk_core();
    let (chain, ids) = mainnet_chain(parent);
    let mut cache = RuleCache::default();
    check_next(&chain, &mut cache, ids[parent as usize]);
    assert_eq!(cache.lo, parent - 4_032, "the context reaches the guard read");
    let complete = chain.complete().expect("a node chain");
    let target = get_next_work_required(complete, ids[parent as usize], time, chain.hp.pow());
    assert_eq!(target.to_base(), core, "Core's GetNextWorkRequired");
}

/// A-8 cold: the same chain in a genesis-origin SPV chain at w = 10,000 (the walk resident) and
/// w = 1,000 (most of it cold, read from the records), through the context the writer refreshes
/// (`contextual_check`'s reads): Core's target, with no read outside the context.
#[test]
fn kgw_max_walk_cold() {
    use crate::chain::spv_tests::genesis_with;
    let (parent, time, core) = kgw_max_walk_core();
    let p = params(Network::Main);
    for w in [10_000, 1_000] {
        let (mut chain, mut store) = genesis_with(&p, w, services(&p));
        let mut prev = BlockId(0);
        for chunk in (1..=parent).collect::<Vec<_>>().chunks(2_000) {
            for &h in chunk {
                let prev_hash = chain.reader().try_entry(prev).expect("read").expect("held").hash;
                let header = BlockHeader {
                    version: 0x2000_0000,
                    prev_hash,
                    time: 1_000_000 + 150 * h,
                    bits: 0x1e01_0000,
                    nonce: h,
                    ..BlockHeader::default()
                };
                let status = Status(Status::VALID_TREE);
                prev = chain
                    .add_to_block_index(&header, &fake_hash(u64::from(h)), status)
                    .expect("added");
            }
            chain.commit_mem_step(&mut store).expect("commit");
            chain.tip_plan(prev).expect("a plan");
            chain.set_active_tip(prev).expect("stage 4");
            chain.records_committed();
        }
        let mut cache = RuleCache::default();
        let pow = *chain.hp.pow();
        let envelope = cache.envelope(&pow, parent);
        let link = |id| {
            let n = chain.node(id)?;
            Ok(PathEntry {
                prev: n.prev,
                cold: (!n.resident).then(|| crate::chain::node_ctx(&n)),
            })
        };
        let resident_time = |id| chain.shared.arena.imm(id, |e| e.header.time);
        let epoch = (chain.floor, chain.residency_epoch);
        cache.refresh(prev, parent, envelope, epoch, link, resident_time).expect("refreshes");
        assert_eq!(cache.lo, parent - 4_032, "w = {w}");
        assert_eq!(cache.cold.is_empty(), w > 4_100, "w = {w}: cold entries in the walk");
        let ctx = RuleContext::new(&chain.shared.arena, &cache);
        let target = get_next_work_required(&ctx, prev, time, &pow);
        assert!(!ctx.read_outside(), "w = {w}");
        assert_eq!(target.to_base(), core, "w = {w}: Core's GetNextWorkRequired");
    }
}

/// A-5 and A-6 on H-M's real headers (`tests/data/headers`): mainnet accepts a header whose
/// `nBits` is within ±50 % of the target up to height 68,589 (quirk 9), so A-4's accept does not
/// pin the context's target there. One warm cache over every height to 68,600 matches the
/// complete ancestry's target, which CS2's history sweep checks against Core's
/// `GetNextWorkRequired` at every one of those heights.
#[test]
#[ignore = "nightly: needs H-M (tests/data/headers/fetch.sh)"]
fn real_tolerance_era_contexts_match_complete_ancestry() {
    const TOP: u32 = 68_600;
    let dir = std::env::var_os("SB4_HEADERS_DIR").map_or_else(
        || std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/headers"),
        std::path::PathBuf::from,
    );
    let mut raw = vec![0u8; 80 * (TOP as usize + 1)];
    std::io::Read::read_exact(
        &mut std::fs::File::open(dir.join("mainnet.bin")).expect("H-M (fetch.sh)"),
        &mut raw,
    )
    .expect("H-M's first headers");
    let headers: Vec<BlockHeader> = raw
        .as_chunks::<80>()
        .0
        .iter()
        .map(|h| dash_types::codec::BaseCodec::decode(&mut &h[..]).expect("a header"))
        .collect();
    let p = params(Network::Main);
    let mut chain =
        HeaderChain::new_genesis(p.clone(), ChainConfig::node(&p), services(&p), crate::NoStore)
            .expect("genesis");
    let mut ids = vec![BlockId(0)];
    for h in &headers[1..] {
        let hash = dash_consensus::primitives::Header::new(*h).hash();
        let id = chain.add_to_block_index(h, &hash, Status(Status::VALID_TREE)).expect("added");
        assert_eq!(chain.shared.arena.imm(id, |e| e.prev), Some(Some(*ids.last().expect("one"))));
        ids.push(id);
    }
    let mut cache = RuleCache::default();
    for next in 1..=TOP {
        let prev = ids[next as usize - 1];
        check_next_at(&chain, &mut cache, prev, next - 1, headers[next as usize].time);
    }
}

/// A-6a's branch half: the context follows a reorg (rebuild), a parent below the cached top
/// (cut back), a jump above the envelope (rebuild), and near-genesis envelopes
/// (`saturating_sub`).
#[test]
fn the_context_follows_branches() {
    let (mut chain, trunk) = mainnet_chain(15_300);
    let fork = trunk[15_250];
    let mut branch = vec![fork];
    for k in 0..40 {
        let prev = branch[branch.len() - 1];
        let h = 15_251 + k;
        branch.push(add(&mut chain, prev, 1_000_000 + u64::from(k), 1_000_000 + 150 * h + 7));
    }
    let mut cache = RuleCache::default();
    for &prev in &trunk[15_255..15_270] {
        check_next(&chain, &mut cache, prev);
    }
    // A branch change mid-KGW, back and forth.
    for i in 5..15 {
        check_next(&chain, &mut cache, branch[i]);
        check_next(&chain, &mut cache, trunk[15_250 + i]);
    }
    // A parent below the cached top: siblings at one height.
    check_next(&chain, &mut cache, trunk[15_280]);
    check_next(&chain, &mut cache, trunk[15_270]);
    check_next(&chain, &mut cache, trunk[15_271]);
    // More than the envelope above the cached top: a rebuild.
    let mut small = RuleCache::default();
    check_next(&chain, &mut small, trunk[40]);
    check_next(&chain, &mut small, trunk[300]);
    // Envelopes reaching below genesis.
    let mut near = RuleCache::default();
    for &prev in &trunk[..11] {
        check_next(&chain, &mut near, prev);
        assert_eq!(near.lo, 0);
    }
}

/// A-9: a KGW context one entry short of the guard read is a read outside the envelope: debug
/// builds panic, release builds flag it (the caller's `ChainError::Internal`).
#[test]
#[cfg_attr(debug_assertions, should_panic(expected = "rule read outside its envelope"))]
fn a_short_kgw_context_reads_outside() {
    let (chain, ids) = mainnet_chain(20_000);
    let mut cache = RuleCache {
        envelope: Some(4_032),
        ..RuleCache::default()
    };
    let prev = ids[19_999];
    cache.refresh(prev, 19_999, 4_032, (0, 0), link(&chain), time(&chain)).expect("refreshes");
    let ctx = RuleContext::new(&chain.shared.arena, &cache);
    let pow = chain.hp.pow();
    let _ = get_next_work_required(&ctx, prev, 1_000_000 + 150 * 20_000, pow);
    assert!(ctx.read_outside());
}
