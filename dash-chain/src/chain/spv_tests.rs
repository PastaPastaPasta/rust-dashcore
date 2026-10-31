//! SB4 C4's SPV half: the anchor pack (§11.1), partial ancestry and fallible reads (§8.4),
//! eviction and cold resolution (§13.3, §3.7, §5.4) and stage 5's cold-cache list (SB5-A5).

use std::sync::Arc;

use dash_consensus::pow::{block_proof, check_proof_of_work};
use dash_consensus::primitives::Header;
use dash_params::{ChainParams, NetworkKind};
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::codec::BaseCodec as _;

use super::cl_tests::enforced;
use super::*;
use crate::chainlock::{ChainLockRef, ClActiveState, ClPersisted, FloorRelation, ForkTarget};
use crate::config::StaticSporks;
use crate::error::FailStop as _;
use crate::host::{TipHost, VirtualTip};
use crate::mem_store::{Corrupt, MemHeaderStore, ReadKind};
use crate::reader::IndexAccess as _;
use crate::residency::{Evicting, Permanent};
use crate::step::{AnchorState, CHAIN_SCHEMA, ChainMeta};
use crate::store::RecordKind;
use crate::testing::{Latch, batch, params, services};

pub(super) type Spv = HeaderChain<MemHeaderStore, Evicting>;

// ---- fixtures -------------------------------------------------------------------------------

/// A committed mainnet slice (`tests/data/anchor-packs/README.md`): the pack for an anchor at
/// `anchor`, with the reference node's chain work, and the header after it.
fn mainnet_pack(anchor: u32) -> (AnchorPack, Header) {
    let chain_work = match anchor {
        34_164 => 0x39_a074_1ece_39d9,
        1_500_000 => 0x5cc3_c1ba_9c4e_8e7b_2a9c,
        _ => unreachable!("no slice for {anchor}"),
    };
    let path =
        format!("{}/tests/data/anchor-packs/mainnet-{anchor}.bin", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).expect("a committed slice");
    let headers: Vec<BlockHeader> = bytes
        .chunks(80)
        .map(|mut c| BlockHeader::decode(&mut c).expect("an 80-byte header"))
        .collect();
    let pack = AnchorPack {
        height: anchor,
        hash: Header::new(headers[24]).hash(),
        chain_work: Work::from_u128(chain_work),
        headers: headers[..25].try_into().expect("25 headers"),
    };
    (pack, Header::new(headers[25]))
}

/// Regtest's `nBits` (its proof-of-work limit): about two tries per header.
const REGTEST_BITS: u32 = 0x207f_ffff;
/// Regtest's DGW height is mainnet's, 34,140: a pack anchored here is post-DGW (§11.1).
pub(super) const ANCHOR: u32 = 40_000;
/// Header times start here, 150 s apart; the services' clock is at 1,700,000,000.
pub(super) const T0: u32 = 1_690_000_000;

/// A regtest header on `prev`, mined.
fn mine(p: &ChainParams, prev: BlockHash, time: u32) -> Header {
    let pow = *HeaderParams::from_chain_params(p).expect("regtest").pow();
    (0..)
        .map(|nonce| {
            Header::new(BlockHeader {
                version: 0x2000_0000,
                prev_hash: prev,
                time,
                bits: REGTEST_BITS,
                nonce,
                ..BlockHeader::default()
            })
        })
        .find(|h| check_proof_of_work(h.hash().as_bytes(), CompactTarget::new(REGTEST_BITS), &pow))
        .expect("a nonce")
}

/// `n` mined headers on `prev`, the first at `time`.
pub(super) fn mine_n(p: &ChainParams, mut prev: BlockHash, time: u32, n: u32) -> Vec<Header> {
    (0..n)
        .map(|i| {
            let h = mine(p, prev, time + 150 * i);
            prev = h.hash();
            h
        })
        .collect()
}

/// A regtest pack anchored at [`ANCHOR`], its work 10^6 proofs above the pack's.
pub(super) fn regtest_pack(p: &ChainParams) -> AnchorPack {
    pack_of(&mine_n(p, BlockHash::from([7; 32]), T0, 25), ANCHOR)
}

/// The pack of `headers[..25]` anchored at `height`, its work `2^20 − (ANCHOR − height)` proofs:
/// packs on one chain at [`ANCHOR`] and below it agree.
fn pack_of(headers: &[Header], height: u32) -> AnchorPack {
    let proofs = Work::from_u64((1 << 20) - u64::from(ANCHOR - height));
    AnchorPack {
        height,
        hash: headers[24].hash(),
        chain_work: block_proof(CompactTarget::new(REGTEST_BITS)).wrapping_mul(proofs),
        headers: headers[..25].iter().map(|h| *h.raw()).collect::<Vec<_>>().try_into().expect("25"),
    }
}

/// An anchored SPV chain over a fresh `MemHeaderStore` with window `w`, the pack committed.
pub(super) fn anchored(p: &Arc<ChainParams>, pack: &AnchorPack, w: u32) -> (Spv, MemHeaderStore) {
    anchored_with(p, pack, w, services(p))
}

/// [`anchored`] with the caller's services.
pub(super) fn anchored_with(
    p: &Arc<ChainParams>,
    pack: &AnchorPack,
    w: u32,
    svc: Services,
) -> (Spv, MemHeaderStore) {
    anchored_cfg(p, pack, ChainConfig::spv(p, w), svc)
}

/// [`anchored`] with the caller's config and services.
fn anchored_cfg(
    p: &Arc<ChainParams>,
    pack: &AnchorPack,
    cfg: ChainConfig,
    svc: Services,
) -> (Spv, MemHeaderStore) {
    let store = MemHeaderStore::new();
    let chain = Spv::new_anchored(p.clone(), cfg, svc, store.clone(), pack).expect("a valid pack");
    seeded(chain, store, (pack.hash, pack.height))
}

/// Window 10 and a one-record cold cache, sized at construction.
fn one_record_cache(p: &Arc<ChainParams>) -> ChainConfig {
    ChainConfig {
        cold_cache: 1,
        ..ChainConfig::spv(p, 10)
    }
}

/// A genesis-origin SPV chain over a fresh `MemHeaderStore` with window `w`, genesis committed.
pub(super) fn genesis_with(p: &Arc<ChainParams>, w: u32, svc: Services) -> (Spv, MemHeaderStore) {
    let store = MemHeaderStore::new();
    let chain =
        Spv::new_genesis(p.clone(), ChainConfig::spv(p, w), svc, store.clone()).expect("genesis");
    seeded(chain, store, (Header::new(p.genesis.header).hash(), 0))
}

/// A fresh chain's store reset to a Candidate anchored at `anchor`, the chain's root its tip and
/// best header, then the chain's first step committed.
fn seeded(
    mut chain: Spv,
    mut store: MemHeaderStore,
    anchor: (BlockHash, u32),
) -> (Spv, MemHeaderStore) {
    store
        .reset(ChainMeta {
            schema: CHAIN_SCHEMA,
            anchor,
            anchor_state: AnchorState::Candidate,
            next_id: BlockId(0),
            tip: chain.best_header(),
            best_header: chain.best_header(),
            sync_seq: 0,
            cl: ClPersisted {
                best: None,
                floor: None,
                enabled_observed: false,
            },
        })
        .expect("reset");
    commit(&chain, &mut store);
    chain.records_committed();
    (chain, store)
}

/// Services whose fail-stop latch the test reads.
pub(super) fn latched(p: &Arc<ChainParams>) -> (Services, Arc<Latch>) {
    let latch = Arc::new(Latch::default());
    let svc = Services {
        fail: latch.clone(),
        ..services(p)
    };
    (svc, latch)
}

fn commit(chain: &Spv, store: &mut MemHeaderStore) {
    chain.commit_mem_step(store).expect("commit");
}

/// One SPV header step (§9.1): accept, commit (stage 3), move the tip to the best header
/// (stage 4, which evicts), then stage 5. Stage 4 is skipped when the tip stays, so nothing is
/// evicted then; such a step must mark nothing, or its marks wait in the overlay for the next
/// stage 4 (mi-2).
pub(super) fn step(
    chain: &mut Spv,
    store: &mut MemHeaderStore,
    headers: &[Header],
) -> HeadersResult {
    let r = chain.accept(batch(headers), None).expect("accept");
    commit(chain, store);
    let to = chain.best_header();
    if chain.private_tip().expect("a tip").0.id != to {
        chain.tip_plan(to).expect("a plan");
        chain.set_active_tip(to).expect("stage 4");
    }
    chain.records_committed();
    r
}

pub(super) fn resident(chain: &Spv, id: BlockId) -> bool {
    chain.shared.arena.imm(id, |_| ()).is_some()
}

fn hash_of(chain: &Spv, id: BlockId) -> BlockHash {
    chain.reader().try_entry(id).expect("read").expect("held").hash
}

fn id_of(chain: &Spv, h: &Header) -> BlockId {
    chain.reader().try_lookup(&h.hash()).expect("read").expect("held")
}

// ---- the pack (§11.1) -----------------------------------------------------------------------

/// A-10: at anchor + 1 the rule context is exactly the pack, and DGW over it gives mainnet's
/// real `nBits` (the next header is accepted). A-32's second half: each pack entry's work is its
/// parent's plus its proof, down from the trusted absolute work.
#[test]
fn anchor_ctx_is_pack() {
    let p = params(NetworkKind::Mainnet);
    for anchor in [34_164, 1_500_000] {
        let (pack, next) = mainnet_pack(anchor);
        let (mut chain, store) = anchored(&p, &pack, 10_000);
        let floor = anchor - 24;
        assert_eq!(AnchoredAncestry::floor(&chain), floor);
        assert_eq!(chain.best_header(), BlockId(24));
        assert_eq!(chain.private_tip().expect("the anchor").0.id, BlockId(24));
        for i in 0..25u32 {
            let (height, context, work) = chain
                .shared
                .arena
                .imm(BlockId(i), |e| (e.height, e.context, e.chain_work))
                .expect("pack entries are resident");
            assert_eq!((height, context), (floor + i, i < 24), "pack entry {i}");
            if i > 0 {
                let parent = chain.work(BlockId(i - 1));
                let proof = block_proof(CompactTarget::new(pack.headers[i as usize].bits));
                assert_eq!(work - parent, proof, "pack entry {i}");
            }
        }
        assert_eq!(chain.work(BlockId(24)), pack.chain_work);
        for i in 0..25 {
            let rec = store.by_id(BlockId(i)).expect("read").expect("the first step wrote it");
            assert_eq!(rec.kind == RecordKind::Context, i < 24, "pack entry {i}");
        }

        let r = chain.accept(batch(&[next]), None).expect("accept");
        assert_eq!(r.stop, None, "anchor {anchor}: mainnet's next header");
        assert_eq!(chain.best_header(), BlockId(25));
        let (lo, path) = chain.rules.path();
        assert_eq!(lo, floor, "anchor {anchor}: the context starts at the floor");
        assert_eq!(path, (0..25).map(BlockId).collect::<Vec<_>>(), "anchor {anchor}");
    }
}

/// §11.1's refusals: the anchor hash, the links, the DGW floor and the checked work (A-32's
/// first half), the node mode and `Permanent` residency (SB5 D70).
#[test]
fn anchor_pack_refusals() {
    let p = params(NetworkKind::Mainnet);
    let (good, _) = mainnet_pack(1_500_000);
    let refuse = |pack: &AnchorPack, cfg: ChainConfig| {
        Spv::new_anchored(p.clone(), cfg, services(&p), MemHeaderStore::new(), pack)
            .err()
            .expect("refused")
    };
    let spv = ChainConfig::spv(&p, 10_000);

    let mut bad = good.clone();
    bad.hash = BlockHash::from([1; 32]);
    assert_eq!(refuse(&bad, spv.clone()), AnchorError::Hash.into());

    let mut bad = good.clone();
    bad.headers[5].nonce ^= 1;
    assert_eq!(
        refuse(&bad, spv.clone()),
        AnchorError::Link {
            index: 5
        }
        .into()
    );

    let hp = HeaderParams::from_chain_params(&p).expect("mainnet");
    let dgw = u32::try_from(hp.pow().config().dgw_height).expect("a height");
    let mut bad = good.clone();
    bad.height = dgw + 23;
    assert_eq!(
        refuse(&bad, spv.clone()),
        AnchorError::PreDgw {
            height: dgw + 23
        }
        .into()
    );
    let mut ok = good.clone();
    ok.height = dgw + 24;
    assert!(
        Spv::new_anchored(p.clone(), spv.clone(), services(&p), MemHeaderStore::new(), &ok).is_ok()
    );

    // The proofs of entries 1..=24 exactly: the lowest entry would get zero work, which the
    // check refuses before an unchecked subtraction could wrap.
    let proofs = good.headers[1..]
        .iter()
        .fold(Work::ZERO, |w, h| w + block_proof(CompactTarget::new(h.bits)));
    let mut low = good.clone();
    low.chain_work = proofs;
    assert_eq!(refuse(&low, spv.clone()), AnchorError::Work.into());
    low.chain_work = proofs + Work::from_u64(1);
    assert!(Spv::new_anchored(p.clone(), spv, services(&p), MemHeaderStore::new(), &low).is_ok());

    assert_eq!(refuse(&good, ChainConfig::node(&p)), ConfigError::AnchoredNode.into());
    let permanent = HeaderChain::<MemHeaderStore, Permanent>::new_anchored(
        p.clone(),
        ChainConfig::spv(&p, 10_000),
        services(&p),
        MemHeaderStore::new(),
        &good,
    );
    assert_eq!(permanent.err(), Some(ConfigError::AnchoredPermanent.into()));
}

/// A-11: a header on a context entry forks below the anchor; a known context header is skipped;
/// genesis is below the anchor (§5.3, D13).
#[test]
fn fork_below_anchor_ignored() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, _store) = anchored(&p, &pack, 10_000);

    let fork = mine(&p, Header::new(pack.headers[20]).hash(), T0 + 150 * 21 + 1);
    let r = chain.accept(batch(&[fork]), None).expect("accept");
    assert_eq!(
        r.stop,
        Some(HeaderStop::BelowAnchor {
            index: 0
        })
    );
    assert_eq!(chain.next_id, 25, "nothing inserted");

    let known = Header::new(pack.headers[10]);
    let one = chain.accept_header(&known).expect("accept_header").one;
    assert_eq!(one, AcceptOne::ContextSkipped);

    let genesis = Header::new(p.genesis.header);
    assert_eq!(chain.accept_header(&genesis).expect("accept_header").one, AcceptOne::BelowAnchor);
}

/// §8.4: below the floor, partial ancestry answers `BelowFloor` and the readers `None`; the
/// lowest pack entry has no parent and no skip (D-C4-1).
#[test]
fn partial_ancestry_ends_at_the_floor() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (chain, _store) = anchored(&p, &pack, 10_000);
    let (floor, anchor) = (ANCHOR - 24, BlockId(24));
    let a = chain.anchored();
    assert_eq!(a.ancestor(anchor, floor), Ok(Some(BlockId(0))));
    assert_eq!(
        a.ancestor(anchor, floor - 1),
        Err(AncestryError::BelowFloor {
            height: floor - 1
        })
    );
    assert_eq!(
        a.prev(BlockId(0)),
        Err(AncestryError::BelowFloor {
            height: floor - 1
        })
    );
    assert_eq!(a.prev(BlockId(1)), Ok(Some(BlockId(0))));
    assert_eq!(chain.shared.arena.imm(BlockId(0), |e| (e.prev, e.skip)), Some((None, None)));
    for i in 1..25 {
        let skip = chain.shared.arena.imm(BlockId(i), |e| e.skip).expect("resident");
        let want = (walk::skip_height(floor + i) >= floor)
            .then(|| BlockId(walk::skip_height(floor + i) - floor));
        assert_eq!(skip, want, "pack entry {i}");
    }
    let ctx = a.header(BlockId(5)).expect("resident");
    assert_eq!(ctx.mtp, MTP_UNKNOWN, "the ten lowest pack entries have no mtp");
    assert_ne!(a.header(BlockId(10)).expect("resident").mtp, MTP_UNKNOWN);

    let reader = chain.reader();
    assert_eq!(reader.try_ancestor(anchor, floor - 1), Ok(None));
    assert_eq!(reader.ancestor(anchor, floor - 1), None);
    assert_eq!(reader.try_ancestor(anchor, floor), Ok(Some(BlockId(0))));
    assert_eq!(reader.try_ancestor(BlockId(999), floor), Ok(None), "an id never issued");
    assert_eq!(reader.try_lca(anchor, BlockId(3)), Ok(Some(BlockId(3))));
}

// ---- eviction and cold reads (§13.3, §3.7, §5.4) -------------------------------------------

/// Eviction after each step's commit: entries below `tip − w` leave the arena and the index and
/// resolve cold, with their committed status; the context and the `max_cold_leaf_work` fold
/// follow; a chunk whose entries all left is released.
#[test]
fn eviction_resolves_cold() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    // A stale leaf on the anchor's child, to leave the window as a leaf.
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 1_100);
    step(&mut chain, &mut store, &trunk[..1]);
    let stale = mine(&p, trunk[0].hash(), T0 + 150 * 26 + 7);
    step(&mut chain, &mut store, &[stale]);
    let stale_id = chain.shared.lookup(&stale.hash()).expect("resident");
    let stale_work = chain.work(stale_id);
    step(&mut chain, &mut store, &trunk[1..6]);
    let resident_mtp = chain.shared.arena.imm(BlockId(30), |e| e.mtp);
    let epoch = chain.residency_epoch;
    for c in trunk[6..].chunks(100) {
        step(&mut chain, &mut store, c);
    }
    let tip = chain.private_tip().expect("a tip").0;
    assert_eq!(tip.height, ANCHOR + 1_100);
    assert!(chain.residency_epoch > epoch);

    // Below tip − w: gone from the arena and the index, still readable through the store.
    let below = tip.height - 10;
    for id in [BlockId(0), BlockId(24), BlockId(30), stale_id] {
        assert!(!resident(&chain, id), "{id:?}");
        let hash = hash_of(&chain, id);
        assert_eq!(chain.shared.lookup(&hash), None);
        assert_eq!(chain.reader().try_lookup(&hash), Ok(Some(id)));
        assert_eq!(chain.reader().entry(id), None);
    }
    let first_resident = chain.resident.first().expect("the window").0;
    assert_eq!(first_resident, below, "every entry below tip − w left");
    let tip_id = tip.id;
    assert!(resident(&chain, tip_id));
    // The context entries resolve cold too, with their kind and status.
    let view = chain.reader().try_entry(BlockId(3)).expect("read").expect("held");
    assert_eq!(view.height, ANCHOR - 21);
    assert!(view.status.is_valid(Status::VALID_TREE));
    assert_eq!(view.mtp, MTP_UNKNOWN, "cold entries carry no mtp");
    // Partial ancestry over records: GetAncestor through skip ids, and the computed mtp.
    let a = chain.anchored();
    assert_eq!(a.ancestor(tip_id, ANCHOR - 24), Ok(Some(BlockId(0))));
    assert_eq!(a.ancestor(tip_id, ANCHOR), Ok(Some(BlockId(24))));
    let at5 = chain.reader().try_lookup(&trunk[4].hash()).expect("read");
    assert_eq!(chain.reader().try_ancestor(tip_id, ANCHOR + 5), Ok(at5));
    assert_eq!(a.header(BlockId(30)).expect("cold").mtp, resident_mtp.expect("was resident"));
    assert_eq!(chain.reader().try_lca(tip_id, stale_id), Ok(Some(BlockId(25))));

    assert_eq!(*chain.max_cold_leaf_work.lock(), stale_work, "the stale leaf left the window");
    assert!(!chain.leaves.iter().any(|&(_, Reverse(id))| id == stale_id));
    assert!(!chain.chunk_live.contains_key(&0), "chunk 0's entries all left: released");
    assert!(chain.chunk_live.contains_key(&1));
}

/// A-38 and A-6a's eviction half: a stale branch forking 30 below the tip, its parent evicted.
/// `accept` reads the parent's record (`nTimeMax`, work), the rule context copies the cold
/// entries and computes the parent's `mtp`, and the verdicts and the new entry equal an
/// unwindowed run's; `time-too-old` at the MTP boundary included, by `accept_header` too.
#[test]
fn spv_accept_deep_fork_cold_prev() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 60);
    // The fork's parent is trunk[29]; its children: one at the MTP (too old), one valid.
    let parent = trunk[29].hash();
    let mut runs = Vec::new();
    for w in [100_000, 10] {
        let (mut chain, mut store) = anchored(&p, &pack, w);
        for c in trunk.chunks(20) {
            step(&mut chain, &mut store, c);
        }
        let parent_id = id_of(&chain, &trunk[29]);
        assert_eq!(resident(&chain, parent_id), w > 60, "w = {w}");
        let epoch = chain.residency_epoch;
        let mtp = chain.anchored().header(parent_id).expect("held").mtp;
        let old = mine(&p, parent, u32::try_from(mtp).expect("a time"));
        let r = chain.accept(batch(&[old]), None).expect("accept");
        let Some(HeaderStop::Invalid {
            reject,
            ..
        }) = r.stop
        else {
            panic!("w = {w}: {:?}", r.stop);
        };
        assert_eq!(reject.reject_reason(), "time-too-old", "w = {w}");
        let one = chain.accept_header(&old).expect("accept_header").one;
        assert!(matches!(one, AcceptOne::Invalid(ref e) if e.reject_reason() == "time-too-old"));

        let fork = mine(&p, parent, u32::try_from(mtp).expect("a time") + 1);
        let r = chain.accept(batch(&[fork]), None).expect("accept");
        assert_eq!(r.stop, None, "w = {w}");
        let id = r.last_accepted.expect("accepted");
        let imm = chain.shared.arena.imm(id, |e| (e.height, e.chain_work, e.mtp, e.time_max));
        let skip = chain.shared.arena.imm(id, |e| e.skip.map(|s| hash_of(&chain, s)));
        assert_eq!(chain.residency_epoch, epoch, "accepting evicts nothing");
        runs.push((imm, skip, mtp));
    }
    assert_eq!(runs[0], runs[1], "the windowed run equals the unwindowed one");
}

/// A store read failure on a cold path trips the latch and returns `Storage`.
#[test]
fn a_failed_cold_read_trips_the_latch() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (svc, latch) = latched(&p);
    let (mut chain, mut store) = anchored_with(&p, &pack, 10, svc);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 30);
    step(&mut chain, &mut store, &trunk);
    assert!(!resident(&chain, BlockId(30)));
    store.faults(|f| f.fail_read(ReadKind::ById, 1));
    assert!(matches!(chain.reader().try_entry(BlockId(30)), Err(ChainError::Storage(_))));
    assert!(latch.is_tripped());
}

/// Sol r1 M1 (§10.2): SPV's `getchaintips` joins the store's leaves to the resident ones. A
/// stale leaf off ANCHOR + 1 is a `headers-only` tip while resident, once evicted, while a child
/// the store has not seen yet replaces it, and after a reorg below the window re-extends the
/// active chain. Each read refreshes `max_cold_leaf_work` to the exact cold maximum.
#[test]
fn spv_chain_tips_join_the_stores_leaves() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 40);
    step(&mut chain, &mut store, &trunk[..1]);
    let stale = mine(&p, trunk[0].hash(), T0 + 150 * 26 + 7);
    step(&mut chain, &mut store, &[stale]);
    let stale_id = chain.shared.lookup(&stale.hash()).expect("resident");
    let at1 = chain.shared.lookup(&trunk[0].hash()).expect("resident");
    let tips = |c: &Spv| -> Vec<_> {
        let tips = c.chain_tips().expect("chain_tips");
        tips.iter().map(|t| (t.id, t.height, t.branchlen, t.fork_point, t.status)).collect()
    };
    let headers_only = ChainTipStatus::HeadersOnly;
    let stale_tip = (stale_id, ANCHOR + 2, 1, at1, headers_only);
    step(&mut chain, &mut store, &trunk[1..3]);
    assert!(resident(&chain, stale_id));
    assert_eq!(tips(&chain)[1], stale_tip, "resident");

    for c in trunk[3..].chunks(10) {
        step(&mut chain, &mut store, c);
    }
    let tip = chain.private_tip().expect("a tip").0;
    assert!(!resident(&chain, stale_id), "evicted");
    let active = (tip.id, ANCHOR + 40, 0, tip.id, ChainTipStatus::Active);
    *chain.max_cold_leaf_work.lock() = Work::ZERO;
    assert_eq!(tips(&chain), [active, stale_tip], "cold");
    let stale_work = chain.node(stale_id).expect("cold").chain_work;
    assert_eq!(*chain.max_cold_leaf_work.lock(), stale_work, "refreshed by the read");

    // A child of the cold leaf, accepted but not committed: the store still lists its parent.
    let child = mine(&p, stale.hash(), T0 + 150 * 27 + 7);
    chain.accept(batch(&[child]), None).expect("accept");
    let child_id = chain.shared.lookup(&child.hash()).expect("resident");
    let child_tip = (child_id, ANCHOR + 3, 2, at1, headers_only);
    assert_eq!(tips(&chain), [active, child_tip], "the uncommitted child replaces it");
    commit(&chain, &mut store);
    chain.records_committed();
    assert_eq!(tips(&chain), [active, child_tip]);
    assert_eq!(*chain.max_cold_leaf_work.lock(), Work::ZERO, "no leaf left the window");

    // From trunk[4] at ANCHOR + 5, past the trunk: the disconnects re-extend below the base.
    let branch = mine_n(&p, trunk[4].hash(), T0 + 150 * 30 + 11, 40);
    step(&mut chain, &mut store, &branch);
    let new = chain.private_tip().expect("a tip").0;
    assert_eq!((new.height, chain.active.base()), (ANCHOR + 45, ANCHOR + 35));
    let at5 = id_of(&chain, &trunk[4]);
    let old = (tip.id, ANCHOR + 40, 35, at5, headers_only);
    let active = (new.id, ANCHOR + 45, 0, new.id, ChainTipStatus::Active);
    assert!(!resident(&chain, child_id));
    assert_eq!(tips(&chain), [active, old, child_tip], "after the re-extension");
}

/// Sol r1 M1: a failed read of the store's leaves trips the latch and fails `getchaintips`.
#[test]
fn spv_chain_tips_leaves_read_failure_trips_the_latch() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (svc, latch) = latched(&p);
    let (mut chain, mut store) = anchored_with(&p, &pack, 10, svc);
    step(&mut chain, &mut store, &mine_n(&p, pack.hash, T0 + 150 * 25, 30));
    store.faults(|f| f.fail_read(ReadKind::Leaves, 1));
    // A failed read is not corruption: `open` stops rather than resyncs (D68).
    assert!(matches!(chain.chain_tips(), Err(ChainError::Storage(e)) if !e.is_corrupt()));
    assert!(latch.is_tripped());
}

/// Sol r1 m1 and D68: every read-layer integrity check is typed corruption
/// ([`StoreError::Corrupt`], which `open` resyncs from) and trips the latch. A cold record whose
/// skip names itself (`Corrupt::BadSkip`), whose parent is at another height, cold or resident,
/// or missing, that comes back under another id, or that a skip reaches and is gone: the walk
/// fails within a bounded number of reads (a `ById` watchdog on the 100th) instead of looping.
#[test]
fn a_cold_link_off_its_height_is_corruption() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 30);
    // ANCHOR + 8 is cold; GetSkipHeight(40,008) is the anchor, so its skip is the first jump.
    assert_eq!(walk::skip_height(ANCHOR + 8), ANCHOR);
    // Each case plants its fault on the record named by hash: ANCHOR + 8 itself, its parent,
    // or the anchor, which its skip reaches by id.
    let x = trunk[7].hash();
    let cases = [
        (x, Corrupt::BadSkip, ANCHOR, "off its height"),
        (x, Corrupt::PrevHash(trunk[11].hash()), ANCHOR + 6, "at height"),
        (x, Corrupt::PrevHash(trunk[29].hash()), ANCHOR + 6, "at height"),
        (x, Corrupt::MissingParent, ANCHOR + 6, "parent not found"),
        (x, Corrupt::Id(BlockId(0)), ANCHOR + 6, "read as record 0"),
        (trunk[6].hash(), Corrupt::Hash(trunk[5].hash()), ANCHOR + 6, "with hash"),
        (pack.hash, Corrupt::Gone, ANCHOR, "has no record"),
    ];
    for (victim, how, below, what) in cases {
        let (svc, latch) = latched(&p);
        let (mut chain, mut store) = anchored_with(&p, &pack, 10, svc);
        step(&mut chain, &mut store, &trunk);
        let lookup = |h: &BlockHash| chain.reader().try_lookup(h).expect("read").expect("held");
        let (x, victim) = (lookup(&x), lookup(&victim));
        assert!(!resident(&chain, x) && !resident(&chain, victim));
        store.faults(|f| {
            f.corrupt(victim, how);
            f.fail_read(ReadKind::ById, 100);
        });
        // SB5's cold cache may still hold the record as the step read it.
        chain.shared.invalidate_cold(ColdInvalidation::Clear);
        match chain.reader().try_ancestor(x, below) {
            Err(ChainError::Storage(e)) => {
                assert!(e.is_corrupt() && e.what().contains(what), "{how:?}: {e}");
            }
            other => panic!("{how:?}: {other:?}"),
        }
        assert!(latch.is_tripped(), "{how:?}");
    }
}

/// Opus D68 review: an id read from a record that the chain never issued (a parent whose record
/// comes back under a far id) is corruption in `AnchoredAncestry`'s walk and its cold MTP; only
/// a caller's id panics. The parent is read by hash, so SB5's cold cache refuses it first, against
/// the issued-id watermark ("(next …)"); `a_skip_never_issued_is_corruption` reaches the walk's
/// own check.
#[test]
fn an_unissued_id_read_from_a_record_is_corruption() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (svc, latch) = latched(&p);
    let (mut chain, mut store) = anchored_with(&p, &pack, 10, svc);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 30);
    step(&mut chain, &mut store, &trunk);
    let (x, parent) = (id_of(&chain, &trunk[7]), id_of(&chain, &trunk[6]));
    assert!(!resident(&chain, x) && !resident(&chain, parent));
    store.faults(|f| f.corrupt(parent, Corrupt::Id(BlockId(u32::MAX))));
    // SB5's cold cache may still hold the record as the step read it.
    chain.shared.invalidate_cold(ColdInvalidation::Clear);
    let header = AnchoredAncestry::header(&chain, x).map(|_| ());
    let ancestor = AnchoredAncestry::ancestor(&chain, x, ANCHOR + 6).map(|_| ());
    for r in [header, ancestor] {
        match r {
            Err(AncestryError::Storage(e)) => {
                assert!(e.is_corrupt() && e.what().contains("never issued (next "), "{e}");
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(latch.is_tripped());
}

/// The walk's own issued-id check: a cold record's skip naming an id never issued is corruption
/// in `AnchoredAncestry::ancestor`, read by id from the record, past SB5's cold cache.
#[test]
fn a_skip_never_issued_is_corruption() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (svc, latch) = latched(&p);
    let (mut chain, mut store) = anchored_with(&p, &pack, 10, svc);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 30);
    step(&mut chain, &mut store, &trunk);
    let x = id_of(&chain, &trunk[15]);
    assert!(!resident(&chain, x));
    let skip = store.by_id(x).expect("read").and_then(|r| r.skip).expect("a skip");
    let skip_height = store.by_id(skip).expect("read").expect("held").height;
    store.faults(|f| f.corrupt(x, Corrupt::Skip(BlockId(u32::MAX))));
    chain.shared.invalidate_cold(ColdInvalidation::Clear);
    match AnchoredAncestry::ancestor(&chain, x, skip_height) {
        Err(AncestryError::Storage(e)) => {
            let what = e.what();
            assert!(e.is_corrupt() && what.contains("never issued") && !what.contains("(next"));
        }
        other => panic!("{other:?}"),
    }
    assert!(latch.is_tripped());
}

/// Opus r1 Low-1: a resident entry's cold parent whose record claims another height (with a
/// parent consistent with it) fails the step down from the child: a tip plan's leg walk trips
/// the latch instead of returning a leg that climbs.
#[test]
fn a_resident_entrys_cold_parent_off_its_height_is_corruption() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (svc, latch) = latched(&p);
    let (mut chain, mut store) = anchored_with(&p, &pack, 10, svc);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 30);
    step(&mut chain, &mut store, &trunk);
    let (child, parent) = (id_of(&chain, &trunk[19]), id_of(&chain, &trunk[18]));
    assert!(resident(&chain, child) && !resident(&chain, parent));
    // ANCHOR + 19 claims ANCHOR + 30 over ANCHOR + 29: a walk down would climb back to the tip.
    store.faults(|f| f.corrupt(parent, Corrupt::Reparent(ANCHOR + 30, trunk[28].hash())));
    // SB5's cold cache may still hold the record as the step read it.
    chain.shared.invalidate_cold(ColdInvalidation::Clear);
    let tip = chain.private_tip().expect("a tip").0.id;
    match chain.walk_down(tip, 15) {
        Err(ChainError::Storage(e)) => {
            assert!(e.is_corrupt() && e.what().contains("off its height"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    assert!(latch.is_tripped());
}

/// Opus r1 n-3: a cold parent whose record claims height `u32::MAX` (over a parent record
/// one below it, so each link steps down) has no child height. Acceptance and
/// `AddToBlockIndex` on it are corruption, the latch tripped, with nothing inserted.
#[test]
fn a_parent_at_the_last_height_is_corruption() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 30);
    let child = mine(&p, trunk[7].hash(), T0 + 150 * 33 + 1);
    for via_accept in [true, false] {
        let (svc, latch) = latched(&p);
        let (mut chain, mut store) = anchored_with(&p, &pack, 10, svc);
        step(&mut chain, &mut store, &trunk);
        let (x, parent) = (id_of(&chain, &trunk[7]), id_of(&chain, &trunk[6]));
        assert!(!resident(&chain, x) && !resident(&chain, parent));
        store.faults(|f| {
            f.corrupt(x, Corrupt::Reparent(u32::MAX, trunk[6].hash()));
            f.corrupt(parent, Corrupt::Reparent(u32::MAX - 1, trunk[5].hash()));
        });
        // SB5's cold cache may still hold the record as the step read it.
        chain.shared.invalidate_cold(ColdInvalidation::Clear);
        let next = chain.next_id;
        let r = match via_accept {
            true => chain.accept(batch(&[child]), None).map(|_| ()),
            false => chain
                .add_to_block_index(child.raw(), &child.hash(), Status(Status::VALID_TREE))
                .map(|_| ()),
        };
        match r {
            Err(ChainError::Storage(e)) => {
                assert!(e.is_corrupt() && e.what().contains("no successor"), "{e}");
            }
            other => panic!("{other:?}"),
        }
        assert!(latch.is_tripped());
        assert_eq!(chain.next_id, next, "nothing inserted");
    }
}

/// Opus r1 Low-3: a cold leaf's status comes through the overlay before its commit
/// (`conflicting`), and a cold active tip that is also a store leaf is one `active` tip.
#[test]
fn spv_chain_tips_cold_overlay_and_cold_active_tip() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 40);
    let stale = mine(&p, trunk[0].hash(), T0 + 150 * 26 + 7);
    let setup = || {
        let (mut chain, mut store) = anchored(&p, &pack, 10);
        step(&mut chain, &mut store, &trunk[..1]);
        step(&mut chain, &mut store, &[stale]);
        for c in trunk[1..].chunks(10) {
            step(&mut chain, &mut store, c);
        }
        let id = id_of(&chain, &stale);
        assert!(!resident(&chain, id));
        (chain, store, id)
    };
    let statuses = |c: &Spv, id| -> Vec<_> {
        let tips = c.chain_tips().expect("chain_tips");
        tips.into_iter().filter(|t| t.id == id).map(|t| t.status).collect()
    };

    let (mut chain, _store, stale_id) = setup();
    chain.set_status(stale_id, |s| Status(s.bits() | Status::CONFLICT_CHAINLOCK)).expect("mark");
    assert_eq!(statuses(&chain, stale_id), [ChainTipStatus::Conflicting], "the overlay's");

    let (mut chain, _store, stale_id) = setup();
    chain.tip_plan(stale_id).expect("a plan down");
    chain.set_active_tip(stale_id).expect("stage 4");
    assert_eq!(chain.private_tip().expect("a tip").0.id, stale_id);
    assert!(!resident(&chain, stale_id), "a cold active tip");
    assert_eq!(statuses(&chain, stale_id), [ChainTipStatus::Active], "once");
}

/// §3.7's overlay over a cold entry: the mark loads nothing into the arena; the writer reads it
/// first; readers keep the committed status until the step commits; the commit writes the row,
/// and stage 4 touches no resident group for it.
#[test]
fn a_cold_mark_goes_through_the_overlay() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 30);
    step(&mut chain, &mut store, &trunk[..20]);
    let cold = BlockId(26);
    assert!(!resident(&chain, cold));
    let before = chain.reader().try_entry(cold).expect("read").expect("held").status;
    let marked = Status(before.bits() | Status::CONFLICT_CHAINLOCK);
    chain.set_status(cold, |s| Status(s.bits() | Status::CONFLICT_CHAINLOCK)).expect("mark");
    assert!(!resident(&chain, cold), "a cold mark loads nothing into the arena");
    assert_eq!(chain.writer_status(cold), Ok(marked));
    assert_eq!(chain.reader().try_entry(cold).expect("read").expect("held").status, before);
    step(&mut chain, &mut store, &trunk[20..]);
    assert_eq!(store.by_id(cold).expect("read").expect("stored").status, marked);
    assert_eq!(chain.reader().try_entry(cold).expect("read").expect("held").status, marked);
    assert!(chain.overlay.is_empty());
}

// ---- stage 5 and the SPV children (SB5-A5, §3.7) ------------------------------------------

/// SB5-A5: the step's written and marked ids, deduplicated, while they fit `cold_cache`; else
/// clear. Stage 5 empties the dirty set and the overlay.
#[test]
fn records_committed_invalidates_at_most_cold_cache() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10_000);
    chain.cfg.cold_cache = 4;
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 5);
    chain.accept(batch(&trunk[..2]), None).expect("accept");
    chain.set_status(BlockId(3), |s| Status(s.bits() | Status::CONFLICT_CHAINLOCK)).expect("mark");
    chain.mark_dirty(BlockId(3));
    let written = chain.dirty.ids().to_vec();
    assert_eq!(
        chain.cold_invalidation(written),
        ColdInvalidation::Ids(vec![BlockId(3), BlockId(25), BlockId(26)])
    );
    commit(&chain, &mut store);
    let to = chain.best_header();
    chain.tip_plan(to).expect("a plan");
    chain.set_active_tip(to).expect("stage 4 applies the mark");
    chain.records_committed();
    assert!(chain.dirty.ids().is_empty() && chain.overlay.is_empty());

    chain.accept(batch(&trunk[2..]), None).expect("accept");
    chain.set_status(BlockId(4), |s| Status(s.bits() | Status::CONFLICT_CHAINLOCK)).expect("mark");
    chain.set_status(BlockId(5), |s| Status(s.bits() | Status::CONFLICT_CHAINLOCK)).expect("mark");
    let written = chain.dirty.ids().to_vec();
    assert_eq!(chain.cold_invalidation(written), ColdInvalidation::Clear, "5 ids, cap 4");
}

/// mi-2: stage 5 drops only the marks a stage 4 applied. A step marks a resident entry and
/// commits it (stage 3); then its stage 4 errs (no plan, or a plan to another target) or its
/// hold is abandoned, and stage 5 runs. The mark stays: the writer reads the committed status,
/// readers keep the old one with the old snapshot (D36), and the dirty set is drained. The next
/// step's stage 4 applies it with its snapshot, and that step's stage 5 drops it.
#[test]
fn records_committed_keeps_the_marks_no_stage_4_applied() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 6);
    for case in ["no plan", "another target", "abandoned hold"] {
        let (mut chain, mut store) = anchored(&p, &pack, 10_000);
        step(&mut chain, &mut store, &trunk[..5]);
        let id = id_of(&chain, &trunk[2]);
        let before = status_of(&chain, id);
        let marked = Status(before.bits() | Status::CONFLICT_CHAINLOCK);
        chain.set_status(id, |s| Status(s.bits() | Status::CONFLICT_CHAINLOCK)).expect("mark");
        chain.accept(batch(&trunk[5..]), None).expect("accept");
        let to = chain.best_header();
        commit(&chain, &mut store);
        let epoch = chain.snapshot().epoch;
        match case {
            "no plan" => assert!(matches!(chain.set_active_tip(to), Err(ChainError::Internal(_)))),
            "another target" => {
                chain.tip_plan(id_of(&chain, &trunk[3])).expect("a plan");
                assert!(matches!(chain.set_active_tip(to), Err(ChainError::Internal(_))));
            }
            _ => {
                let mut hold = chain.hold_tip();
                hold.set_active_tip(to, 1).expect("one connect");
                hold.abandon();
            }
        }
        chain.records_committed();
        let stored = |store: &MemHeaderStore| store.by_id(id).expect("read").expect("stored");
        assert_eq!(stored(&store).status, marked, "{case}: committed at stage 3");
        assert_eq!(chain.writer_status(id), Ok(marked), "{case}: the writer keeps the mark");
        assert_eq!(chain.overlay_marks().collect::<Vec<_>>(), vec![(id, marked)], "{case}");
        assert_eq!(status_of(&chain, id), before, "{case}: readers wait for a snapshot");
        assert_eq!(chain.snapshot().epoch, epoch, "{case}: nothing published");
        assert!(chain.dirty.ids().is_empty(), "{case}: the dirty set is drained");

        // The next step: its stage 3 writes the kept mark again, its stage 4 applies it. After
        // the abandoned hold, that step re-marks the id first and its stage 4 is a release.
        let marked = match case {
            "abandoned hold" => {
                let failed = Status(marked.bits() | Status::FAILED_CHILD);
                chain.set_status(id, |s| Status(s.bits() | Status::FAILED_CHILD)).expect("mark");
                assert_eq!(chain.writer_status(id), Ok(failed), "the kept mark, then this one");
                failed
            }
            _ => marked,
        };
        commit(&chain, &mut store);
        let snap = match case {
            "abandoned hold" => chain.hold_tip().release(),
            _ => {
                chain.tip_plan(to).expect("a plan");
                chain.set_active_tip(to).expect("stage 4").snap
            }
        };
        assert_eq!((snap.tip.tip, snap.epoch), (to, epoch + 1), "{case}");
        assert_eq!(status_of(&chain, id), marked, "{case}: applied with the snapshot");
        chain.records_committed();
        assert!(chain.overlay.is_empty(), "{case}: applied, then dropped");
        assert_eq!(chain.writer_status(id), Ok(marked), "{case}");
        assert_eq!(stored(&store).status, marked, "{case}");
    }
}

/// §3.7's SPV children: the store's committed children of `prev` and the step's uncommitted
/// new entries on it.
#[test]
fn spv_children_join_the_store_and_the_step() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10_000);
    let a = mine(&p, pack.hash, T0 + 150 * 25);
    step(&mut chain, &mut store, &[a]);
    let b = mine(&p, pack.hash, T0 + 150 * 25 + 3);
    chain.accept(batch(&[b]), None).expect("accept");
    assert_eq!(chain.child_ids(BlockId(24)), Ok(vec![BlockId(25), BlockId(26)]));
    assert_eq!(chain.child_ids(BlockId(25)), Ok(vec![]));
}

/// SB5-A6: SPV refuses a zero-capacity cold cache.
#[test]
fn spv_refuses_an_empty_cold_cache() {
    let p = params(NetworkKind::Regtest);
    let cfg = ChainConfig {
        cold_cache: 0,
        ..ChainConfig::spv(&p, 10)
    };
    let r =
        Spv::new_anchored(p.clone(), cfg, services(&p), MemHeaderStore::new(), &regtest_pack(&p));
    assert_eq!(r.err(), Some(ConfigError::ColdCache.into()));
}

// ---- entries the chain keeps naming after they leave the window (C4 review) -----------------

/// The ChainLock floor and the best header can leave the window (no new lock for longer than
/// `w` blocks; C6's walk-backs). The snapshot shows their kept facts, and `HasChainLock` reads
/// the floor's ancestry cold.
#[test]
fn a_cold_floor_and_best_header_stay_in_the_snapshot() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let sporks = Arc::new(StaticSporks::for_network(&p, Some(true)));
    let svc = Services {
        sporks,
        ..services(&p)
    };
    let (mut chain, mut store) = anchored_with(&p, &pack, 10, svc);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 40);
    // The best lock names trunk[1]: AcceptedBlockHeader makes it the floor (§7.1).
    chain.cl_best = Some(ChainLockRef {
        height: ANCHOR + 2,
        hash: trunk[1].hash(),
    });
    let r = step(&mut chain, &mut store, &trunk[..2]);
    let floor = r.floor_moved.expect("the late header moved the floor");
    for c in trunk[2..].chunks(10) {
        step(&mut chain, &mut store, c);
    }
    assert!(!resident(&chain, floor));
    let snap = chain.snapshot();
    let want = BlockRef {
        id: floor,
        hash: trunk[1].hash(),
        height: ANCHOR + 2,
    };
    assert_eq!(snap.cl_floor, Some(want));
    assert_eq!(chain.has_chainlock(ANCHOR + 1, &trunk[0].hash()), Ok(true));
    assert_eq!(chain.has_conflicting_chainlock(ANCHOR + 1, &trunk[1].hash()), Ok(true));

    // A best header that left the window: its facts come from its record once, when it is set.
    let cold = id_of(&chain, &trunk[5]);
    assert!(!resident(&chain, cold));
    chain.best_header = Some(chain.facts_of(cold).expect("a cold read"));
    let (_, seq) = chain.private_tip().expect("a tip");
    let tip = chain.build_snapshot(seq).tip;
    let e = chain.reader().try_entry(cold).expect("read").expect("held");
    assert_eq!(
        (tip.best_header, tip.best_header_hash, tip.best_header_height),
        (cold, e.hash, e.height)
    );
    assert_eq!((tip.best_header_work, tip.best_header_time), (e.chain_work, e.header.time));
}

/// A heavier branch forking below the window: `chain_tips` finds the cold fork point, and the
/// reorg's plan reads its cold legs (disconnects, the fork, and connects whose entries left
/// the window before the rest of the branch arrived), steps them with their refs and
/// publishes once.
#[test]
fn spv_deep_reorg_activates_through_cold_legs() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 50);
    for c in trunk[..40].chunks(10) {
        step(&mut chain, &mut store, c);
    }
    let fork_point = id_of(&chain, &trunk[4]);
    assert!(!resident(&chain, fork_point));
    let branch = mine_n(&p, trunk[4].hash(), T0 + 150 * 30 + 11, 50);

    // Three headers: less work than the tip, so a leaf whose fork point is cold.
    step(&mut chain, &mut store, &branch[..3]);
    let leaf = chain.shared.lookup(&branch[2].hash()).expect("resident");
    let tips = chain.chain_tips().expect("chain_tips over a cold fork point");
    let t = tips.iter().find(|t| t.id == leaf).expect("the branch's leaf");
    assert_eq!((t.fork_point, t.branchlen, t.status), (fork_point, 3, ChainTipStatus::HeadersOnly));
    // The trunk moves on: the branch's first headers leave the window.
    step(&mut chain, &mut store, &trunk[40..]);
    assert!(!resident(&chain, leaf));

    // The rest: 50 headers from ANCHOR + 5 outweigh the trunk's 50 from the anchor.
    let old_tip = chain.private_tip().expect("a tip").0;
    let r = chain.accept(batch(&branch[3..]), None).expect("accept");
    assert_eq!(r.stop, None);
    commit(&chain, &mut store);
    let to = chain.best_header();
    let plan = chain.tip_plan(to).expect("a plan with cold legs");
    assert_eq!(plan.fork.id, fork_point);
    assert_eq!(plan.connect[2].id, leaf, "a connect leg read from the store");
    let change = chain.set_active_tip(to).expect("stage 4");
    chain.records_committed();
    assert_eq!(change.disconnected.len(), 45);
    assert_eq!(change.disconnected[0], old_tip);
    assert_eq!(change.disconnected[44].height, ANCHOR + 6);
    assert_eq!(change.connected.len(), 50);
    let c0 = change.connected[0];
    assert_eq!((c0.hash, c0.height), (branch[0].hash(), ANCHOR + 6));
    assert_eq!(change.snap.tip.tip, to);
    assert_eq!(change.last_seq, change.snap.tip_seq);
    // Below the window again (DEC-174): the connected leg resolves by ancestry.
    assert_eq!(chain.ancestor_private(ANCHOR + 6), None);
    assert_eq!(chain.reader().try_ancestor(to, ANCHOR + 6), Ok(Some(c0.id)));

    // The old tip is a resident leaf now, forking at the same cold entry.
    let tips = chain.chain_tips().expect("chain_tips");
    let t = tips.iter().find(|t| t.id == old_tip.id).expect("the old tip");
    assert_eq!((t.fork_point, t.branchlen), (fork_point, 45));
}

/// DEC-174: the writer windows the active chain at `tip − w` after each step, dropping its
/// leaves below; a reorg 2,143 deep at w = 10 re-extends it through those leaves by ancestry,
/// pops each old leg in order, and windows the new chain again. A tip moved down below the
/// window keeps its base until it climbs `w` past it. The locator fallback is still the anchor.
#[test]
fn a_disconnect_below_the_window_re_extends_then_pops() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    let n = 2_148;
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, n);
    for c in trunk.chunks(100) {
        step(&mut chain, &mut store, c);
    }
    let fallback = |c: &Spv| c.snapshot().active.fork_in_global_index(&[], &c.reader());
    let old_tip = chain.private_tip().expect("a tip").0;
    assert_eq!((old_tip.height, chain.active.base()), (ANCHOR + n, ANCHOR + n - 10));
    assert_eq!(chain.ancestor_private(ANCHOR + n - 11), None, "below the window");
    assert_eq!(fallback(&chain), BlockId(24), "the anchor (§10.1 n6)");

    // From trunk[4] at ANCHOR + 5: n headers outweigh the trunk's n from the anchor.
    let branch = mine_n(&p, trunk[4].hash(), T0 + 150 * 30 + 11, n);
    step(&mut chain, &mut store, &branch[..2_000]);
    assert_eq!(chain.private_tip().expect("a tip").0, old_tip, "less work: no reorg yet");
    chain.accept(batch(&branch[2_000..]), None).expect("accept");
    commit(&chain, &mut store);
    let to = chain.best_header();
    let plan = chain.tip_plan(to).expect("a plan below the window");
    assert_eq!(plan.fork.height, ANCHOR + 5);
    let change = chain.set_active_tip(to).expect("stage 4");
    chain.records_committed();
    let old: Vec<_> = change.disconnected.iter().map(|b| (b.hash, b.height)).collect();
    let want: Vec<_> = (6..=n).rev().map(|i| (trunk[i as usize - 1].hash(), ANCHOR + i)).collect();
    assert_eq!(old, want, "every old leg, tip first, read by ancestry below the window");
    assert_eq!(change.connected.len(), n as usize);

    let tip = chain.private_tip().expect("a tip").0;
    assert_eq!((tip.id, tip.height), (to, ANCHOR + 5 + n));
    assert_eq!(chain.active.base(), tip.height - 10, "windowed again");
    for h in [tip.height - 10, tip.height - 1, tip.height] {
        let want = branch[(h - ANCHOR - 6) as usize].hash();
        assert_eq!(chain.ancestor_private(h).map(|id| hash_of(&chain, id)), Some(want), "{h}");
    }
    assert_eq!(chain.ancestor_private(tip.height - 11), None);
    assert_eq!(chain.snapshot().active.base(), tip.height - 10);
    assert_eq!(fallback(&chain), BlockId(24), "the anchor (§10.1 n6)");
    let tips = chain.chain_tips().expect("chain_tips");
    let t = tips.iter().find(|t| t.id == old_tip.id).expect("the old tip");
    assert_eq!((t.fork_point, t.branchlen), (plan.fork.id, n - 5));

    // Down to a cold entry 2,053 below: the base stays at the new tip, above `tip − w`, until
    // the chain climbs back.
    let low = id_of(&chain, &branch[94]);
    chain.tip_plan(low).expect("a plan down");
    chain.set_active_tip(low).expect("stage 4");
    assert_eq!(chain.active.base(), ANCHOR + 100);
    assert_eq!(chain.ancestor_private(ANCHOR + 100), Some(low));
    assert_eq!(chain.ancestor_private(ANCHOR + 95), None, "evicted before the tip came down");
    chain.tip_plan(to).expect("a plan back up");
    chain.set_active_tip(to).expect("stage 4");
    assert_eq!(chain.active.base(), tip.height - 10);
}

/// SB5 §4.8's segmented reorg inside a hold: a disconnect-only sub-step leaves the writer's tip
/// on a cold fork; the next sub-steps, `is_ibd_private` and the release read its kept facts.
/// Then a tip moved to a cold target and back.
#[test]
fn a_cold_private_tip_steps_on() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 40);
    for c in trunk.chunks(10) {
        step(&mut chain, &mut store, c);
    }
    let fork_point = id_of(&chain, &trunk[4]);
    let branch = mine_n(&p, trunk[4].hash(), T0 + 150 * 30 + 11, 40);
    chain.accept(batch(&branch), None).expect("accept");
    commit(&chain, &mut store);
    chain.records_committed();
    let best = chain.best_header();

    let mut hold = chain.hold_tip();
    let down = hold.set_active_tip(fork_point, 1_000).expect("down to the cold fork");
    assert_eq!((down.disconnected.len(), down.connected.len()), (35, 0));
    let c = hold.chain();
    assert_eq!(c.private_tip().expect("a tip").0.id, fork_point);
    assert!(!resident(c, fork_point));
    let fork_time = c.reader().try_entry(fork_point).expect("read").expect("held").header.time;
    assert_eq!(c.tip.map(|t| (t.at.id, t.time)), Some((fork_point, fork_time)));
    // Past the fork's tip age and inside the old tip's: only the fork's facts say IBD.
    let age = i64::try_from(crate::DEFAULT_MAX_TIP_AGE.as_secs()).expect("seconds");
    let now = dash_clock::NodeSeconds(i64::from(fork_time) + age + 1);
    assert!(i64::from(trunk[39].time()) + age > now.secs(), "the old tip is younger");
    assert!(c.is_ibd_private(now, false), "the cold fork is the tip");
    let up = hold.set_active_tip(best, 1_000).expect("up the branch");
    assert_eq!(up.connected.len(), 40);
    // SB5's per-segment call (DEC-174), ahead of the release's eviction.
    let c = hold.chain();
    c.window_active();
    assert_eq!(c.active.base(), c.private_tip().expect("a tip").0.height - 10);
    let snap = hold.release();
    assert_eq!(snap.tip.tip, best);

    // A cold target: the tip and the snapshot name it from its record.
    let cold = id_of(&chain, &branch[9]);
    assert!(!resident(&chain, cold));
    chain.tip_plan(cold).expect("a plan to a cold target");
    let change = chain.set_active_tip(cold).expect("stage 4");
    let e = chain.reader().try_entry(cold).expect("read").expect("held");
    let t = &change.snap.tip;
    assert_eq!((t.tip, t.tip_hash, t.tip_height), (cold, e.hash, e.height));
    assert_eq!((t.tip_work, t.tip_time), (e.chain_work, e.header.time));
    chain.records_committed();
    let tips = chain.chain_tips().expect("chain_tips with a cold tip");
    let active = tips.iter().find(|t| t.id == cold).expect("the cold tip");
    assert_eq!((active.status, active.branchlen), (ChainTipStatus::Active, 0));
    chain.tip_plan(best).expect("a plan back");
    assert_eq!(chain.set_active_tip(best).expect("stage 4").snap.tip.tip, best);
}

/// A genesis-origin SPV chain whose genesis left the window: a resent genesis header goes
/// through Core's short-cut to `AddToBlockIndex` (D-C3-2) and `AcceptedBlockHeader` reads the
/// cold entry.
#[test]
fn a_resent_cold_genesis_is_accepted() {
    let p = params(NetworkKind::Regtest);
    let (mut chain, mut store) = genesis_with(&p, 10, services(&p));
    let genesis = Header::new(p.genesis.header);
    let trunk = mine_n(&p, genesis.hash(), T0, 30);
    step(&mut chain, &mut store, &trunk);
    assert!(!resident(&chain, BlockId(0)));
    chain.cl_best = Some(ChainLockRef {
        height: 0,
        hash: genesis.hash(),
    });
    let one = chain.accept_header(&genesis).expect("accept_header");
    assert_eq!(one.one, AcceptOne::Accepted(BlockId(0)));
    assert_eq!(one.floor_moved, Some(BlockId(0)));
    assert_eq!(chain.cl_floor(), Some(BlockId(0)));
}

// ---- ChainLocks over cold entries (§7.3, §9.1; A-21b, A-30, A-35) -----------------------------

/// Services with ChainLocks on from the start.
fn cl_services(p: &Arc<ChainParams>) -> Services {
    Services {
        sporks: Arc::new(StaticSporks::for_network(p, Some(true))),
        ..services(p)
    }
}

/// One SPV step with ChainLocks (§9.1's recipe): one `VirtualTip`, `accept` (when there are
/// headers), the step's calls, the target, then stages 3, 4 and 5.
fn cl_step(
    chain: &mut Spv,
    store: &mut MemHeaderStore,
    headers: &[Header],
    calls: impl FnOnce(&mut Spv, &mut VirtualTip),
) -> (VirtualTip, ForkTarget) {
    let mut vt = chain.virtual_tip();
    if !headers.is_empty() {
        assert_eq!(chain.accept(batch(headers), None).expect("accept").stop, None);
    }
    calls(chain, &mut vt);
    let target = chain.fork_target(&vt).expect("a target");
    chain.tip_plan(target.id).expect("a plan");
    commit(chain, store);
    chain.set_active_tip(target.id).expect("stage 4");
    chain.records_committed();
    (vt, target)
}

fn status_of(chain: &Spv, id: BlockId) -> Status {
    chain.reader().try_entry(id).expect("read").expect("held").status
}

fn lock_on(h: &Header, height: u32) -> ChainLockRef {
    ChainLockRef {
        height,
        hash: h.hash(),
    }
}

/// A-21b: w = 10, an active floor 30 below the tip, its entry cold. The lock's enforcement and a
/// later timer step mark nothing; a cursor moved below the active top answers `contains` from
/// the active chain, which still holds the floor's height (the skip-link fallback is for SB5's
/// windowed active chain), and enforcing from it marks nothing either.
#[test]
fn spv_reenforce_floor_below_window() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored_with(&p, &pack, 10, cl_services(&p));
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 40);
    for c in trunk.chunks(10) {
        step(&mut chain, &mut store, c);
    }
    let floor = id_of(&chain, &trunk[9]);
    assert!(!resident(&chain, floor));
    cl_step(&mut chain, &mut store, &[], |chain, vt| {
        let r =
            enforced(chain.apply_chainlock(lock_on(&trunk[9], ANCHOR + 10), vt).expect("applies"));
        assert!(r.marked.is_empty() && vt.disconnected().is_empty());
    });
    let tip = chain.private_tip().expect("a tip").0.id;
    let (vt, target) = cl_step(&mut chain, &mut store, &[], |chain, vt| {
        assert_eq!(chain.check_cl_active_state(), ClActiveState::Unchanged);
        let r = chain.enforce_best_chainlock(vt).expect("enforces").expect("a floor");
        assert!(r.marked.is_empty() && vt.disconnected().is_empty());
    });
    assert_eq!((vt.cursor(), target.id, target.floor), (tip, tip, FloorRelation::Descends));

    let mut vt = chain.virtual_tip();
    for _ in 0..5 {
        TipHost::<_, Evicting>::disconnect_tip(&mut vt, &mut chain).expect("down");
    }
    assert_eq!(vt.cursor(), id_of(&chain, &trunk[34]));
    assert_eq!(TipHost::<_, Evicting>::contains(&vt, &chain, floor), Ok(true));
    let above = id_of(&chain, &trunk[36]);
    assert_eq!(TipHost::<_, Evicting>::contains(&vt, &chain, above), Ok(false));
    let r = chain.enforce_best_chainlock(&mut vt).expect("enforces").expect("a floor");
    assert!(r.marked.is_empty() && vt.disconnected().len() == 5, "the walk disconnects nothing");
}

/// A-35: w = 10, B101…B110 active on A100, and A101…A120 arrive with a lock on A120 in one
/// step. B101 is resident, and enforcement marks B101…B110 in the overlay. After the commit and
/// stage 4 (whose window starts at 110), B101's cold record reads CONFLICT. With the commit
/// failing, stages 4 and 5 never run (SB5's driver trips the latch): B101 is still resident,
/// marked only in the overlay, readers and the store keep it unmarked, and nothing is
/// published.
#[test]
fn spv_b110_a120_w10_resident_to_cold() {
    for fail in [false, true] {
        let p = params(NetworkKind::Regtest);
        let pack = regtest_pack(&p);
        let (mut chain, mut store) = anchored_with(&p, &pack, 10, cl_services(&p));
        let a = mine_n(&p, pack.hash, T0 + 150 * 25, 120);
        for c in a[..100].chunks(20) {
            step(&mut chain, &mut store, c);
        }
        let b = mine_n(&p, a[99].hash(), T0 + 150 * 125 + 7, 10);
        step(&mut chain, &mut store, &b);
        let b101 = id_of(&chain, &b[0]);
        assert!(resident(&chain, b101));
        assert_eq!(chain.private_tip().expect("a tip").0.id, id_of(&chain, &b[9]));
        let (epoch, unmarked) = (chain.snapshot().epoch, status_of(&chain, b101));

        let mut vt = chain.virtual_tip();
        assert_eq!(chain.accept(batch(&a[100..]), None).expect("accept").stop, None);
        let r = enforced(
            chain.apply_chainlock(lock_on(&a[119], ANCHOR + 120), &mut vt).expect("applies"),
        );
        assert_eq!((r.marked, vt.disconnected().len()), (vec![b101], 10));
        let target = chain.fork_target(&vt).expect("a target");
        assert_eq!(target.id, id_of(&chain, &a[119]));
        chain.tip_plan(target.id).expect("a plan");
        if fail {
            store.faults(|f| f.fail_commit(1));
            assert!(chain.commit_mem_step(&mut store).is_err());
            assert!(resident(&chain, b101));
            assert!(chain.writer_status(b101).expect("known").has(Status::CONFLICT_CHAINLOCK));
            assert_eq!(chain.reader().status(b101), Some(unmarked));
            assert_eq!(store.by_id(b101).expect("read").expect("stored").status, unmarked);
            assert_eq!(chain.snapshot().epoch, epoch);
            continue;
        }
        commit(&chain, &mut store);
        chain.set_active_tip(target.id).expect("stage 4");
        chain.records_committed();
        assert!(!resident(&chain, b101));
        assert!(status_of(&chain, b101).has(Status::CONFLICT_CHAINLOCK), "the committed record");
        assert_eq!(chain.snapshot().epoch, epoch + 1);
    }
}

/// A-30: A-21's three branches at w = 10, so B81 and C81 are evicted before the lock. The
/// enforcement marks them through the overlay. Both branches are then extended in the same step:
/// B111 on the marked B110 is `bad-prevblk-chainlock` (`validation.cpp:4643-4647`), and C106…C110
/// are accepted on the unmarked C105, but the eligibility walk sees the cold C81 mark. After the
/// commit the cold records carry the marks and `fork_target` still never picks B or C. The
/// reopen half is SB5's `open`.
#[test]
fn spv_cold_mark_b81_c81() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored_with(&p, &pack, 10, cl_services(&p));
    let a = mine_n(&p, pack.hash, T0 + 150 * 25, 100);
    let c = mine_n(&p, a[79].hash(), T0 + 150 * 105 + 3, 30);
    let b = mine_n(&p, a[79].hash(), T0 + 150 * 105 + 5, 40);
    for chunk in a.chunks(20) {
        step(&mut chain, &mut store, chunk);
    }
    step(&mut chain, &mut store, &c[..25]);
    for chunk in b[..30].chunks(10) {
        step(&mut chain, &mut store, chunk);
    }
    let (b81, c81) = (id_of(&chain, &b[0]), id_of(&chain, &c[0]));
    assert!(!resident(&chain, b81) && !resident(&chain, c81));
    assert_eq!(chain.private_tip().expect("a tip").0.id, id_of(&chain, &b[29]));

    let (vt, target) = cl_step(&mut chain, &mut store, &[], |chain, vt| {
        let r =
            enforced(chain.apply_chainlock(lock_on(&a[99], ANCHOR + 100), vt).expect("applies"));
        assert_eq!(r.marked, vec![c81, b81], "id order (D9)");
        assert_eq!(vt.disconnected().len(), 30);
        assert!(!resident(chain, b81), "a cold mark loads nothing into the arena");
        let r = chain.accept(batch(&b[30..]), None).expect("accept");
        let Some(HeaderStop::Invalid {
            reject,
            ..
        }) = r.stop
        else {
            panic!("{:?}", r.stop);
        };
        assert_eq!(reject.reject_reason(), "bad-prevblk-chainlock");
        assert_eq!(chain.accept(batch(&c[25..]), None).expect("accept").stop, None);
    });
    let a100 = id_of(&chain, &a[99]);
    assert_eq!(
        (vt.cursor(), target.id, target.floor),
        (id_of(&chain, &a[79]), a100, FloorRelation::Descends)
    );
    for id in [b81, c81, id_of(&chain, &b[29])] {
        assert!(status_of(&chain, id).has(Status::CONFLICT_CHAINLOCK), "{id:?}");
        assert!(
            store.by_id(id).expect("read").expect("stored").status.has(Status::CONFLICT_CHAINLOCK)
        );
    }
    assert!(!status_of(&chain, id_of(&chain, &c[29])).has(Status::CONFLICT_CHAINLOCK));
    let vt = chain.virtual_tip();
    assert_eq!(chain.fork_target(&vt).expect("a target").id, a100);
}

/// `fork_target`'s cold leaves (§6.2, D19): w = 10, C6…C35 on A5 left the window as a leaf when
/// A41…A50 arrived. A lock on B33 (B11 on A10) marks A11; C35 outweighs B33, so the store's
/// leaves are read, and the floor bounds C35 to A5, below B33. With ChainLocks then observed off,
/// the cold C35 wins over B33 and the marked A branch, and stage 4 activates it.
#[test]
fn spv_fork_target_reads_cold_leaves() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let sporks = Arc::new(StaticSporks::for_network(&p, Some(true)));
    let svc = Services {
        sporks: sporks.clone(),
        ..services(&p)
    };
    let (mut chain, mut store) = anchored_with(&p, &pack, 10, svc);
    let a = mine_n(&p, pack.hash, T0 + 150 * 25, 50);
    let c = mine_n(&p, a[4].hash(), T0 + 150 * 30 + 3, 30);
    let b = mine_n(&p, a[9].hash(), T0 + 150 * 35 + 5, 23);
    for chunk in a[..40].chunks(20) {
        step(&mut chain, &mut store, chunk);
    }
    step(&mut chain, &mut store, &c);
    step(&mut chain, &mut store, &a[40..]);
    let (a11, c35) = (id_of(&chain, &a[10]), id_of(&chain, &c[29]));
    assert!(!resident(&chain, c35));
    assert_eq!(chain.private_tip().expect("a tip").0.id, id_of(&chain, &a[49]));

    let (vt, target) = cl_step(&mut chain, &mut store, &b, |chain, vt| {
        let r = enforced(chain.apply_chainlock(lock_on(&b[22], ANCHOR + 33), vt).expect("applies"));
        assert_eq!((r.marked, vt.disconnected().len()), (vec![a11], 40));
    });
    let b33 = id_of(&chain, &b[22]);
    assert_eq!(
        (vt.cursor(), target.id, target.floor),
        (id_of(&chain, &a[9]), b33, FloorRelation::Descends)
    );

    sporks.set(false);
    let (vt, target) = cl_step(&mut chain, &mut store, &[], |chain, _| {
        assert_eq!(chain.check_cl_active_state(), ClActiveState::TurnedOff);
    });
    assert_eq!((vt.cursor(), target.id, target.floor), (b33, c35, FloorRelation::Unenforced));
    assert_eq!(chain.private_tip().expect("a tip").0.id, c35);
}

// ---- the locator (§10.1 n6, D41; MJ-1) ------------------------------------------------------

/// The heights of a locator's hashes, read cold where they left the window.
fn locator_heights_of(chain: &Spv, locator: &[BlockHash]) -> Vec<u32> {
    let reader = chain.reader();
    let height = |h: &BlockHash| {
        let id = reader.try_lookup(h).expect("read").expect("held");
        reader.try_entry(id).expect("read").expect("held").height
    };
    locator.iter().map(height).collect()
}

/// §10.1 n6: at w = 10 with most of the chain cold, SPV's locator has Core's `LocatorEntries`
/// density down to the anchor, read from the store below the window, and ends at the anchor,
/// the lowest accepted entry. The anchor's own locator is itself; a context entry's is itself.
#[test]
fn spv_locator_core_density_to_the_anchor() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 200);
    for c in trunk.chunks(50) {
        step(&mut chain, &mut store, c);
    }
    let tip = chain.private_tip().expect("a tip").0;
    assert_eq!(tip.height, ANCHOR + 200);
    let reader = chain.reader();
    let locator = reader.try_locator(Some(tip.id)).expect("reads");
    assert_eq!(reader.locator(Some(tip.id)), locator);

    let mut want: Vec<u32> =
        walk::locator_heights(tip.height).into_iter().filter(|&h| h >= ANCHOR).collect();
    assert_eq!(want.last(), Some(&(ANCHOR + 63)), "Core's next height is below the anchor");
    want.push(ANCHOR);
    assert_eq!(locator_heights_of(&chain, &locator), want);
    // Every entry is on the trunk: the doubling entries from the store, then the anchor.
    let on_trunk = |h: u32| match h {
        ANCHOR => pack.hash,
        h => trunk[(h - ANCHOR - 1) as usize].hash(),
    };
    assert_eq!(locator, want.iter().map(|&h| on_trunk(h)).collect::<Vec<_>>());
    assert!(!resident(&chain, id_of(&chain, &trunk[62])), "read cold");

    assert_eq!(reader.locator(Some(BlockId(24))), [pack.hash], "the anchor ends its own walk");
    assert_eq!(reader.locator(Some(BlockId(3))), [hash_of(&chain, BlockId(3))], "context");
    assert_eq!(reader.locator(None), Vec::<BlockHash>::new());
}

/// §10.1, D7: a store read failure in the locator's cold walk trips the latch; `try_locator`
/// returns `Storage`, and the infallible `locator` answers the empty locator.
#[test]
fn spv_locator_read_failure_trips_the_latch() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (svc, latch) = latched(&p);
    let (mut chain, mut store) = anchored_with(&p, &pack, 10, svc);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 100);
    for c in trunk.chunks(50) {
        step(&mut chain, &mut store, c);
    }
    let tip = chain.private_tip().expect("a tip").0.id;
    store.faults(|f| f.fail_read(ReadKind::ById, 1));
    assert!(matches!(chain.reader().try_locator(Some(tip)), Err(ChainError::Storage(_))));
    assert!(latch.is_tripped());
    store.faults(|f| f.fail_read(ReadKind::ById, 1));
    assert_eq!(chain.reader().locator(Some(tip)), Vec::<BlockHash>::new());
}

/// MJ-1's getheaders case: a peer whose chain forked from ours more than w below our tip finds
/// a common entry in our locator, a doubling entry when the fork is above the anchor's
/// neighbourhood and the anchor terminus when it is lower, so it never falls back to its
/// genesis. The peer holds its whole index, as Core does. It is anchored 8 below us, so its own
/// fallback is not our terminus: without the terminus it answers its anchor (Opus-high A-L1).
#[test]
fn spv_locator_finds_a_fork_deeper_than_the_window() {
    const K: u32 = 8;
    let p = params(NetworkKind::Regtest);
    let below = mine_n(&p, BlockHash::from([7; 32]), T0, 25 + K);
    let (pack, peer_pack) = (pack_of(&below[K as usize..], ANCHOR), pack_of(&below, ANCHOR - K));
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * (25 + K), 200);
    for c in trunk.chunks(50) {
        step(&mut chain, &mut store, c);
    }
    let locator = chain.reader().locator(Some(chain.private_tip().expect("a tip").0.id));
    let without_terminus = &locator[..locator.len() - 1];

    // (fork height above the anchor, the height the peer finds, and without our terminus)
    for (fork, found, alone) in [(100, ANCHOR + 63, ANCHOR + 63), (10, ANCHOR, ANCHOR - K)] {
        let (mut peer, mut peer_store) = anchored(&p, &peer_pack, 10_000);
        let theirs = mine_n(&p, trunk[fork - 1].hash(), T0 + 150 * (25 + K + fork as u32) + 9, 250);
        step(&mut peer, &mut peer_store, &below[25..]);
        step(&mut peer, &mut peer_store, &trunk[..fork]);
        step(&mut peer, &mut peer_store, &theirs);
        let peer_tip = peer.private_tip().expect("a tip").0;
        assert_eq!(peer_tip.height, ANCHOR + fork as u32 + 250, "the peer is on its branch");

        let (snap, reader) = (peer.snapshot(), peer.reader());
        let height = |id| reader.entry(id).expect("resident").height;
        let id = snap.active.fork_in_global_index(&locator, &reader);
        let entry = reader.entry(id).expect("resident");
        assert_eq!(entry.height, found, "fork at {fork}");
        assert!(locator.contains(&entry.hash), "a locator entry, not the fallback");
        assert!(snap.active.contains(id, &reader));
        let id = snap.active.fork_in_global_index(without_terminus, &reader);
        assert_eq!(height(id), alone, "fork at {fork}, without the terminus");
    }
}

/// SB0 erratum E3 (§15.2, D41), SB4's half over `MemHeaderStore` at w = 10. The durable step
/// holds A100 with the ChainLock floor A70, observed enabled. A suffix committed Buffered but
/// not Durable observes the spork off, accepts B51…B110 forking at A50 and publishes B110. The
/// wallet's locator for B110 is B110…B100, B99, B97, B93, B85, B69, then the shared A37 and
/// genesis. After power loss the store is A100/A70 again, and `FindForkInGlobalIndex` of that
/// locator over the restored chain is A37: the first locator entry on it, at or below the true
/// fork 50. Controls: a locator whose first entry is on the restored chain returns it, and an
/// all-unknown one returns the anchor (genesis). The wallet half is SB14's.
#[test]
fn wallet_locator_lost_disabled_suffix() {
    let p = params(NetworkKind::Regtest);
    let a = mine_n(&p, Header::new(p.genesis.header).hash(), T0, 100);
    let b = mine_n(&p, a[49].hash(), T0 + 150 * 50 + 7, 60);
    // The durable step: A1…A100, then the lock on A70 with the spork observed on.
    let durable = |sporks: Arc<StaticSporks>| {
        let svc = Services {
            sporks,
            ..services(&p)
        };
        let (mut chain, mut store) = genesis_with(&p, 10, svc);
        for c in a.chunks(20) {
            step(&mut chain, &mut store, c);
        }
        cl_step(&mut chain, &mut store, &[], |chain, vt| {
            enforced(chain.apply_chainlock(lock_on(&a[69], 70), vt).expect("applies"));
        });
        store.sync_wal().expect("durable");
        let cl = chain.cl_state();
        assert!(cl.enabled_observed && cl.floor == Some(id_of(&chain, &a[69])));
        (chain, store)
    };

    let sporks = Arc::new(StaticSporks::for_network(&p, Some(true)));
    let (mut chain, mut store) = durable(sporks.clone());
    sporks.set(false);
    let (_, target) = cl_step(&mut chain, &mut store, &b, |chain, _| {
        assert_eq!(chain.check_cl_active_state(), ClActiveState::TurnedOff);
    });
    let b110 = id_of(&chain, &b[59]);
    assert_eq!((target.id, target.floor), (b110, FloorRelation::Unenforced));
    assert_eq!(chain.snapshot().tip.tip, b110, "published");
    assert_eq!(store.buffered_steps(), 1, "committed Buffered, not Durable");

    let locator = chain.reader().try_locator(Some(b110)).expect("reads");
    let mut want: Vec<u32> = (100..=110).rev().collect();
    want.extend([99, 97, 93, 85, 69, 37, 0]);
    assert_eq!(locator_heights_of(&chain, &locator), want);
    assert_eq!(locator[16], a[36].hash(), "the shared A37");
    assert_eq!(locator[15], b[18].hash(), "B69");

    // Power loss, and the reopened chain: SB5's `open` is not on this branch, so the restored
    // chain replays the durable step, and its committed tables equal what survived.
    let restored = store.power_loss();
    assert_eq!(restored.by_hash(&b[59].hash()).expect("read"), None, "the suffix is lost");
    let (chain, rebuilt) = durable(Arc::new(StaticSporks::for_network(&p, Some(true))));
    assert_eq!(rebuilt.image(), restored.image(), "the restored A100/A70");
    let (snap, reader) = (chain.snapshot(), chain.reader());
    assert_eq!(snap.tip.tip, id_of(&chain, &a[99]));

    let a37 = id_of(&chain, &a[36]);
    assert!(!resident(&chain, a37), "below the window");
    let fork = snap.active.try_fork_in_global_index(&locator, &reader).expect("reads");
    assert_eq!((fork, reader.try_entry(fork).expect("read").expect("held").height), (a37, 37));
    // The infallible form skips entries that left the window (D-C4-12): the anchor.
    assert_eq!(snap.active.fork_in_global_index(&locator, &reader), BlockId(0));

    // Controls: the restored chain's own locator, then an all-unknown one.
    let own = reader.try_locator(Some(snap.tip.tip)).expect("reads");
    let first = snap.active.try_fork_in_global_index(&own, &reader).expect("reads");
    assert_eq!(first, snap.tip.tip, "nothing to rewind");
    let unknown = &locator[..16];
    assert_eq!(snap.active.try_fork_in_global_index(unknown, &reader), Ok(BlockId(0)));
    assert_eq!(snap.active.try_fork_in_global_index(&[], &reader), Ok(BlockId(0)));
}

/// Opus-high nit, with SB5's cold cache (DEC-187 amendment 3): a locator hash the store
/// resolves to an id the writer never issued is corruption in `try_fork_in_global_index`. The
/// cache refuses the record before admitting it, against the issued-id watermark, and the latch
/// trips.
#[test]
fn try_fork_in_global_index_refuses_an_unissued_id() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (svc, latch) = latched(&p);
    let (mut chain, mut store) = anchored_with(&p, &pack, 10, svc);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 30);
    step(&mut chain, &mut store, &trunk);
    let x = id_of(&chain, &trunk[7]);
    assert!(!resident(&chain, x));
    let far = BlockId(u32::MAX);
    store.faults(|f| f.corrupt(x, Corrupt::Id(far)));
    let by_hash = store.by_hash(&trunk[7].hash()).expect("read").map(|r| r.id);
    assert_eq!((by_hash, store.by_id(far)), (Some(far), Ok(None)), "the store's answers");
    // SB5's cold cache may still hold the record as the step read it.
    chain.shared.invalidate_cold(ColdInvalidation::Clear);
    let (snap, reader) = (chain.snapshot(), chain.reader());
    assert!(!latch.is_tripped());
    match snap.active.try_fork_in_global_index(&[trunk[7].hash()], &reader) {
        Err(ChainError::Storage(e)) => {
            assert!(e.is_corrupt() && e.what().contains("never issued"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    assert!(latch.is_tripped());
    let cache = chain.shared.cold.as_ref().expect("SPV's cache");
    assert!(cache.peek(far).is_none(), "refused before the cache admits it");
}

/// The issued-id half (DEC-187 amendment 3): a locator hash the store resolves to an issued id
/// with no record. The cold cache admits the by-hash record under the id it names (§5.2), and
/// the walks resolve parents by hash, so `try_fork_in_global_index` answers that id from the
/// cache. The store's gap shows, and the latch trips, at the first by-id read that misses it.
#[test]
fn an_issued_id_with_no_record_shows_past_the_cache() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (svc, latch) = latched(&p);
    let (mut chain, mut store) = anchored_with(&p, &pack, 10, svc);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 30);
    let stale = mine_n(&p, trunk[2].hash(), T0 + 150 * 30 + 11, 1);
    step(&mut chain, &mut store, &trunk[..3]);
    step(&mut chain, &mut store, &stale);
    step(&mut chain, &mut store, &trunk[3..]);
    let (x, y) = (id_of(&chain, &trunk[7]), id_of(&chain, &stale[0]));
    assert!(!resident(&chain, x) && !resident(&chain, y));
    store.faults(|f| {
        f.corrupt(x, Corrupt::Id(y));
        f.corrupt(y, Corrupt::Gone);
    });
    let by_hash = store.by_hash(&trunk[7].hash()).expect("read").map(|r| r.id);
    assert_eq!((by_hash, store.by_id(y)), (Some(y), Ok(None)), "the store's answers");
    chain.shared.invalidate_cold(ColdInvalidation::Clear);
    let (snap, reader) = (chain.snapshot(), chain.reader());
    let fork = snap.active.try_fork_in_global_index(&[trunk[7].hash()], &reader);
    assert_eq!(fork, Ok(y), "the cache's record, read by hash");
    assert!(!latch.is_tripped());
    chain.shared.invalidate_cold(ColdInvalidation::Clear);
    let e = reader.try_link(y).expect_err("the store's gap, past the cache");
    assert!(e.is_corrupt() && e.what().contains("has no record"), "{e}");
    assert!(latch.is_tripped());
}

/// Sol r2 R2-M1 (DEC-187 amendment 3): an issued id with no record is corruption at every
/// public by-id read that misses the cache, not an id the chain never held. Window 10 and a
/// one-record cache (Sol's witness): the stale sibling `y` is gone and the store answers
/// `trunk[7]` by hash under its id, so a by-hash lookup admits `y`; a second lookup evicts it
/// naturally, and each probe's store read by id finds nothing. The latch trips. An id never
/// issued still answers `None` with the latch clear.
#[test]
fn an_issued_id_with_no_record_is_corruption_at_every_by_id_read() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 30);
    let stale = mine_n(&p, trunk[2].hash(), T0 + 150 * 30 + 11, 1);
    for probe in ["try_entry", "try_locator", "try_ancestor", "try_lca"] {
        let (svc, latch) = latched(&p);
        let (mut chain, mut store) = anchored_cfg(&p, &pack, one_record_cache(&p), svc);
        step(&mut chain, &mut store, &trunk[..3]);
        step(&mut chain, &mut store, &stale);
        step(&mut chain, &mut store, &trunk[3..]);
        let (x, y, tip) =
            (id_of(&chain, &trunk[7]), id_of(&chain, &stale[0]), id_of(&chain, &trunk[29]));
        assert!(!resident(&chain, x) && !resident(&chain, y) && resident(&chain, tip));
        let reader = chain.reader();
        assert_eq!(reader.try_entry(BlockId(chain.next_id)).map(|e| e.is_some()), Ok(false));
        assert!(!latch.is_tripped(), "an id never issued");
        store.faults(|f| {
            f.corrupt(x, Corrupt::Id(y));
            f.corrupt(y, Corrupt::Gone);
        });
        chain.shared.invalidate_cold(ColdInvalidation::Clear);
        assert_eq!(reader.try_lookup(&trunk[7].hash()), Ok(Some(y)), "admitted by hash");
        reader.try_lookup(&trunk[6].hash()).expect("evicts y from the cache");
        assert!(!latch.is_tripped());
        let by_id = || store.faults(|f| f.reads(ReadKind::ById));
        let before = by_id();
        let got = match probe {
            "try_entry" => reader.try_entry(y).map(drop),
            "try_locator" => reader.try_locator(Some(y)).map(drop),
            "try_ancestor" => reader.try_ancestor(y, pack.height).map(drop),
            _ => reader.try_lca(y, tip).map(drop),
        };
        assert!(by_id() > before, "{probe}: a store read by id");
        let Err(ChainError::Storage(e)) = got else {
            panic!("{probe}: {got:?}")
        };
        assert!(e.is_corrupt() && e.what() == format!("entry {} has no record", y.0), "{e}");
        assert!(latch.is_tripped(), "{probe}");
    }
}

/// R2-M1's real-code smoke beside loom L14: readers resolve every id up past the writer's next
/// while it steps with window 10 and a one-record cache, so new ids are resident before their
/// commit and cold soon after. No read is refused, every id issued before the race reads, and
/// the latch stays clear. The race window is
/// too short for this to catch L14's planted order; loom proves the order.
#[test]
fn by_id_readers_racing_issuance_are_never_refused() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (svc, latch) = latched(&p);
    let (mut chain, mut store) = anchored_cfg(&p, &pack, one_record_cache(&p), svc);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 300);
    let first = chain.next_id;
    let last = first + trunk.len() as u32;
    let done = AtomicBool::new(false);
    std::thread::scope(|sc| {
        for _ in 0..3 {
            let (reader, done) = (chain.reader(), &done);
            sc.spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    for id in (0..last + 2).rev() {
                        let got = reader.try_entry(BlockId(id)).expect("never refused");
                        assert!(id >= first || got.is_some(), "entry {id} held from the start");
                    }
                }
            });
        }
        for headers in trunk.chunks(3) {
            step(&mut chain, &mut store, headers);
        }
        done.store(true, Ordering::Relaxed);
    });
    assert_eq!(chain.next_id, last);
    assert!(!latch.is_tripped());
}

/// Opus-high A-M1: the anchor stays on SPV's active chain, so `FindForkInGlobalIndex` always
/// has its fallback. A tip change to a context entry (a fork below the anchor) is refused before
/// any plan, and so is `init_active_tip` on one, with nothing published; the anchor itself is a
/// valid tip. Were the anchor ever off a view, the fallback is the view's lowest entry, not a
/// reader panic.
#[test]
fn spv_refuses_a_tip_below_the_anchor() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 1_000);
    step(&mut chain, &mut store, &mine_n(&p, pack.hash, T0 + 150 * 25, 5));
    let (anchor, context) = (BlockId(24), BlockId(10));
    let tip = chain.private_tip().expect("a tip").0.id;
    let epoch = chain.snapshot().epoch;
    let below = Some(ChainError::BelowAnchor);
    assert_eq!(chain.tip_target(context).err(), below);
    assert_eq!(chain.tip_plan(context).err(), below);
    assert_eq!(chain.tip_plan_bounded(context, 100).err(), below);
    assert!(chain.set_active_tip(context).is_err(), "no pending plan to it");
    assert_eq!(chain.init_active_tip(context).err(), below);
    assert_eq!(chain.init_active_tip(BlockId(0)).err(), below, "the floor");
    assert_eq!((chain.private_tip().expect("a tip").0.id, chain.snapshot().epoch), (tip, epoch));
    let reader = chain.reader();
    for to in [anchor, tip] {
        chain.tip_plan(to).expect("a plan");
        chain.set_active_tip(to).expect("stage 4");
        let snap = chain.snapshot();
        assert_eq!(snap.active.fork_in_global_index(&[], &reader), anchor);
        assert_eq!(snap.active.try_fork_in_global_index(&[], &reader), Ok(anchor));
    }
    chain.init_active_tip(anchor).expect("the anchor");
    assert_eq!(chain.snapshot().active.fork_in_global_index(&[], &reader), anchor);

    let mut view = chain.snapshot().active.clone();
    view.chain.pop();
    assert_eq!(view.chain.anchor_id(), None, "the anchor popped");
    assert_eq!(view.fork_in_global_index(&[], &reader), BlockId(0), "the view's lowest");
    assert_eq!(view.try_fork_in_global_index(&[], &reader), Ok(BlockId(0)));
}

/// §6.2's cold gate at a tie (`max_cold_leaf_work >= best`, D-R1-2): w = 2, S6 and A6 on A5 at
/// equal work, S6 first (the lower id). A7…A11 evict S6 as a leaf. With A7 marked and the cursor
/// moved down to A6, the resident answer is A6, whose work ties the cold bound: the store's leaves
/// are read and S6, the lower id, wins the tie.
#[test]
fn spv_fork_target_cold_leaf_ties_the_bound() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 2);
    let a = mine_n(&p, pack.hash, T0 + 150 * 25, 11);
    let s = mine(&p, a[4].hash(), T0 + 150 * 30 + 7);
    step(&mut chain, &mut store, &a[..5]);
    step(&mut chain, &mut store, &[s]);
    step(&mut chain, &mut store, &a[5..]);
    let (a6, s6, a11) = (id_of(&chain, &a[5]), id_of(&chain, &s), id_of(&chain, &a[10]));
    assert!(s6 < a6 && !resident(&chain, s6));
    let work = |id| chain.reader().try_entry(id).expect("read").expect("held").chain_work;
    assert_eq!(work(s6), work(a6));
    assert_eq!(*chain.max_cold_leaf_work.lock(), work(s6), "S6 left the window as a leaf");
    assert_eq!(chain.fork_target(&chain.virtual_tip()).expect("a target").id, a11);

    let a7 = id_of(&chain, &a[6]);
    chain.set_status(a7, |s| Status(s.bits() | Status::CONFLICT_CHAINLOCK)).expect("marks");
    let mut vt = chain.virtual_tip();
    while vt.cursor() != a6 {
        TipHost::<_, Evicting>::disconnect_tip(&mut vt, &mut chain).expect("down");
    }
    assert_eq!(chain.fork_target(&vt).expect("a target").id, s6, "the lower id wins the tie");
}

/// A-37 (O-m8, D36): a lock step's marks reach readers with the step's snapshot, never before
/// and never after it. w = 1,000, so every entry stays resident; B81…B110 are active on A80 and
/// A81…A100 arrive with a lock on A100. Between stages 1–2, 2–3 and 3–4, `IndexAccess::status`
/// reads B81…B110 unmarked and the published tip is B110. `stage4` moves the tip to A100 and
/// returns its snapshot's tip; it is handed the reader's view to probe inside itself. At the
/// instant stage 4 stores its snapshot (the publish hook), and after it, the marks read
/// CONFLICT and the tip is A100.
fn status_coherent_through(
    stage4: impl FnOnce(&mut Spv, BlockId, &dyn Fn() -> (BlockId, usize)) -> BlockId,
) {
    use std::cell::RefCell;
    use std::rc::Rc;

    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored_with(&p, &pack, 1_000, cl_services(&p));
    let a = mine_n(&p, pack.hash, T0 + 150 * 25, 100);
    let b = mine_n(&p, a[79].hash(), T0 + 150 * 105 + 5, 30);
    step(&mut chain, &mut store, &a[..80]);
    step(&mut chain, &mut store, &b);
    let bs: Vec<BlockId> = b.iter().map(|h| id_of(&chain, h)).collect();
    let b110 = bs[29];
    let reader = chain.reader();
    // (published tip, how many of B81…B110 a lock-free reader sees marked)
    let seen = {
        let reader = reader.clone();
        let bs = bs.clone();
        move || {
            let marked = bs
                .iter()
                .filter(|&&id| reader.status(id).expect("resident").has(Status::CONFLICT_CHAINLOCK))
                .count();
            (reader.snapshot().tip.tip, marked)
        }
    };
    assert_eq!(seen(), (b110, 0));
    // SPV journals no status write (CH1-node D63): it makes no journal index (D68).
    assert!(chain.shared.readers.journals.is_empty());

    // Stage 1: compute.
    let mut vt = chain.virtual_tip();
    assert_eq!(chain.accept(batch(&a[80..]), None).expect("accept").stop, None);
    let r =
        enforced(chain.apply_chainlock(lock_on(&a[99], ANCHOR + 100), &mut vt).expect("applies"));
    assert_eq!((r.marked, vt.disconnected().len()), (vec![bs[0]], 30));
    let target = chain.fork_target(&vt).expect("a target");
    let a100 = id_of(&chain, &a[99]);
    assert_eq!(target.id, a100);
    chain.tip_plan(a100).expect("a plan");
    assert_eq!(seen(), (b110, 0), "between stages 1 and 2");
    // Stage 2: capture the step's status rows.
    let rows: Vec<(BlockId, Status)> = chain.overlay_marks().collect();
    assert!(
        bs.iter().all(|id| rows.iter().any(|(r, s)| r == id && s.has(Status::CONFLICT_CHAINLOCK)))
    );
    assert_eq!(seen(), (b110, 0), "between stages 2 and 3");
    // Stage 3: commit.
    commit(&chain, &mut store);
    assert_eq!(seen(), (b110, 0), "between stages 3 and 4");
    // Stage 4, probed when its snapshot becomes visible.
    let at_publish = Rc::new(RefCell::new(Vec::new()));
    let probe = (at_publish.clone(), seen.clone());
    super::AFTER_PUBLISH.set(Some(Box::new(move |_| probe.0.borrow_mut().push(probe.1()))));
    let published = stage4(&mut chain, a100, &seen);
    super::AFTER_PUBLISH.set(None);
    assert_eq!(published, a100);
    assert_eq!(*at_publish.borrow(), [(a100, 30)], "one publication, marks with the tip");
    chain.records_committed();
    assert_eq!(seen(), (a100, 30), "after stage 4");
    assert!(chain.shared.readers.journals.is_empty(), "SPV journals nothing");
}

/// A-37 through `set_active_tip`.
#[test]
fn spv_status_coherent_with_snapshot() {
    status_coherent_through(|chain, a100, _| {
        chain.set_active_tip(a100).expect("stage 4").snap.tip.tip
    });
}

/// Opus-high D-M1: A-37 through a hold, whose release runs stage 4's effects, then publishes.
/// The hold's sub-steps publish nothing and apply no mark.
#[test]
fn spv_status_coherent_through_a_hold() {
    status_coherent_through(|chain, a100, seen| {
        let before = seen();
        let mut hold = chain.hold_tip();
        let change = hold.set_active_tip(a100, u32::MAX).expect("sub-step");
        assert_eq!(change.connected.last().map(|b| b.id), Some(a100));
        assert_eq!(seen(), before, "inside the hold");
        hold.release().tip.tip
    });
}

// ---- A-27 with cold leaves ------------------------------------------------------------------

mod cold_brute_force {
    use std::cell::Cell;

    use proptest::prelude::*;
    use proptest::sample::Index;
    use proptest::test_runner::{Config, RngSeed, TestRunner};

    use super::*;

    /// `id`'s ancestor at height `h` by a `prev` walk over `try_entry`, resident or cold
    /// (independent of the skip-link helpers).
    fn anc(chain: &Spv, id: BlockId, h: u32) -> Option<BlockId> {
        let mut x = id;
        loop {
            let n = chain.reader().try_entry(x).expect("read").expect("held");
            if n.height <= h {
                return (n.height == h).then_some(x);
            }
            x = n.prev?;
        }
    }

    /// SB0's predicate (a) ∧ (b′) relative to `cursor` by brute force over every accepted
    /// record from the anchor up, resident or cold, with ancestry by [`anc`]. Lower id wins a
    /// tie.
    fn brute(chain: &Spv, cursor: BlockId, anchor: BlockId) -> BlockId {
        let e = |id: BlockId| chain.reader().try_entry(id).expect("read").expect("held");
        let anc = |id: BlockId, h: u32| anc(chain, id, h);
        let floor = chain.cl_floor.filter(|_| chain.cl_enabled_observed());
        let on_cursor = |x: BlockId| anc(cursor, e(x).height) == Some(x);
        let conflicts = |x: BlockId, f: BlockRef| {
            anc(f.id, e(x).height) != Some(x) && anc(x, f.height) != Some(f.id)
        };
        let bad = |x: BlockId| {
            chain
                .writer_status(x)
                .expect("known")
                .has(Status::FAILED_MASK | Status::CONFLICT_CHAINLOCK)
        };
        let eligible = |c: BlockId| {
            let mut x = c;
            while !on_cursor(x) {
                if bad(x) {
                    return false;
                }
                x = e(x).prev.expect("above the anchor");
            }
            floor.is_none_or(|f| !conflicts(c, f))
        };
        (anchor.0..chain.next_id)
            .map(BlockId)
            .filter(|&c| eligible(c))
            .max_by_key(|&c| (e(c).chain_work, std::cmp::Reverse(c)))
            .expect("the anchor qualifies")
    }

    /// 256 cases, or `SB4_A27_COLD_CASES`.
    fn cases() -> u32 {
        std::env::var("SB4_A27_COLD_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256)
    }

    /// A pinned seed, or `SB4_A27_COLD_SEED`: about 2 % of cases have an answer only the cold
    /// scan finds, so a random 256 would miss one in about 300 runs (Opus-high D-L2). With this
    /// seed 256 cases hold some, and a disabled cold scan fails.
    fn seed() -> u64 {
        std::env::var("SB4_A27_COLD_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x5b4_a27)
    }

    /// A-27 over an anchored windowed chain (w = 2…5, `MemHeaderStore`), so the random trees
    /// leave cold leaves and cold bad entries: `fork_target` reads `cold_leaves()` behind the
    /// `max_cold_leaf_work` gate, and enforcement reads SPV `children()` from the store. Each
    /// header goes on the newest one or on a random earlier one, one committed step each (so
    /// eviction runs); then random CONFLICT/FAILED marks (resident or cold, through the
    /// overlay), the spork off or on, and a random lock applied or only recorded on one shared
    /// cursor. The leaf answer equals the brute-force scan over all accepted records, and at
    /// least one case's answer is on neither the cursor's path nor a resident leaf's, so only
    /// the store's leaves could give it.
    #[test]
    fn prop_ft_cold_leaves_vs_brute_force() {
        let (with_cold, cold_target, cold_only) = (Cell::new(0u32), Cell::new(0u32), Cell::new(0));
        let strategy = (
            prop::collection::vec((any::<Index>(), any::<bool>()), 1..40),
            prop::collection::vec((any::<Index>(), any::<bool>()), 0..6),
            any::<Option<Index>>(),
            any::<bool>(),
            any::<bool>(),
            2u32..6,
        );
        let cases = cases();
        let mut runner = TestRunner::new(Config {
            cases,
            rng_seed: RngSeed::Fixed(seed()),
            failure_persistence: None,
            ..Config::default()
        });
        let run = runner.run(&strategy, |(parents, marks, locked, on, enforce, w)| {
            let p = params(NetworkKind::Regtest);
            let pack = regtest_pack(&p);
            let sporks = Arc::new(StaticSporks::for_network(&p, Some(false)));
            let svc = Services {
                sporks: sporks.clone(),
                ..services(&p)
            };
            let (mut chain, mut store) = anchored_with(&p, &pack, w, svc);
            let anchor = chain.reader().try_lookup(&pack.hash).expect("read").expect("held");
            let mut ids = vec![anchor];
            for (k, (pi, newest)) in parents.iter().enumerate() {
                let prev = if *newest {
                    ids[ids.len() - 1]
                } else {
                    ids[pi.index(ids.len())]
                };
                let h = mine(&p, hash_of(&chain, prev), T0 + 150 * (30 + k as u32));
                prop_assert_eq!(step(&mut chain, &mut store, &[h]).stop, None);
                ids.push(id_of(&chain, &h));
            }
            for (i, failed) in &marks {
                let bits = if *failed {
                    Status::FAILED_VALID
                } else {
                    Status::CONFLICT_CHAINLOCK
                };
                let id = ids[i.index(ids.len())];
                if id != anchor {
                    chain.set_status(id, |s| Status(s.bits() | bits)).expect("marks");
                }
            }
            sporks.set(on);
            chain.check_cl_active_state();
            let mut vt = chain.virtual_tip();
            if let Some(l) = locked {
                let id = ids[l.index(ids.len())];
                let e = chain.reader().try_entry(id).expect("read").expect("held");
                let cl = ChainLockRef {
                    height: e.height,
                    hash: e.hash,
                };
                // D18: SPV's enforcement never meets a FAILED locked block.
                if !chain.writer_status(id).expect("known").has(Status::FAILED_MASK) {
                    match enforce {
                        true => drop(chain.apply_chainlock(cl, &mut vt).expect("applies")),
                        false => drop(chain.update_best_chainlock(cl).expect("records")),
                    }
                }
            }
            let got = chain.fork_target(&vt).expect("a target").id;
            prop_assert_eq!(got, brute(&chain, vt.cursor(), anchor));
            if !chain.cold_leaf_ids().expect("read").is_empty() {
                with_cold.set(with_cold.get() + 1);
            }
            if !resident(&chain, got) {
                cold_target.set(cold_target.get() + 1);
            }
            // Every resident candidate is on the cursor's path (its floor-bounded self) or on
            // a resident leaf's: an answer on neither came from the store's leaves alone.
            let height = chain.reader().try_entry(got).expect("read").expect("held").height;
            let on = |x: BlockId| anc(&chain, x, height) == Some(got);
            if !on(vt.cursor()) && !chain.leaves.iter().any(|&(_, Reverse(l))| on(l)) {
                cold_only.set(cold_only.get() + 1);
            }
            Ok(())
        });
        if let Err(e) = run {
            panic!("{e}");
        }
        eprintln!(
            "A-27 cold: {cases} cases, {} with cold leaves, {} cold targets, {} from the store's \
             leaves only",
            with_cold.get(),
            cold_target.get(),
            cold_only.get()
        );
        assert!(with_cold.get() >= cases / 10, "the generator reaches cold leaves");
        assert!(cold_only.get() >= 1, "an answer only the cold scan finds");
    }
}
