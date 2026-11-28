use std::sync::Arc;

use dash_chain::{AnchorState, FailStop, HeaderRecord, HeaderStore};
use dash_clock::MockClock;
use dash_consensus::primitives::header_bytes;
use dash_consensus::Header;
use dash_kv::{KvEngine, MemoryEngine};
use dash_primitives::BlockHash;
use dashcore::Network;
use parking_lot::Mutex;

use super::*;
use crate::client::anchor::tests::pack;
use crate::client::anchor::{pack_anchor, Phase};
use crate::client::llmq::llmq_config;
use crate::client::DevnetConfig;
use crate::storage::store::hdr::io::HdrIo;
use crate::storage::store::{Point, Store, Subscriber};

mod crash;
mod deadlock;
pub(crate) mod driver;
mod establish;
mod ingress;
mod no_wait;
mod pinned;
mod rules;
mod score;
pub(crate) mod step;
pub(super) mod sub;

type Mem = Arc<Store<MemoryEngine>>;

/// Packs at 10000, 20000 and 30000; the clock lets the candidate be at most 20000.
fn setup(mn: bool, birth: Option<u32>) -> ChainHostSetup {
    let mut config =
        ClientConfig::new(Network::Devnet).with_devnet(DevnetConfig::new("sb14b-chain-host"));
    config.enable_masternodes = mn;
    config.clock = Arc::new(MockClock::at(2_000_000));
    let llmq = llmq_config(&config).unwrap();
    let sporks = crate::client::chain_sporks(&config, llmq.params());
    ChainHostSetup {
        packs: vec![pack(10000, 1_000_000), pack(20000, 1_000_000), pack(30000, 2_000_000)],
        ..ChainHostSetup::new(config, llmq, birth, SpvFailStop::shared(), sporks)
    }
}

fn mem(dir: &tempfile::TempDir) -> Mem {
    Arc::new(Store::in_memory(dir))
}

fn meta<E: KvEngine + 'static, F: HdrIo>(h: &Store<E, F>) -> dash_chain::ChainMeta {
    h.chain_meta().unwrap().expect("a meta row")
}

fn tip<H: StoreHandle>(chain: &Chain<H>) -> BlockHash {
    chain.private_tip().unwrap().0.hash
}

/// The store holds the anchor's chain: the anchor active and alone, its 24 context records
/// below it, `meta` at it in `state`.
fn assert_anchored<E: KvEngine + 'static, F: HdrIo>(
    h: &Store<E, F>,
    anchor: &AnchorPack,
    state: AnchorState,
) {
    let m = meta(h);
    assert_eq!((m.anchor, m.anchor_state), ((anchor.hash, anchor.height), state));
    let v = h.view();
    assert_eq!((v.active.lo, v.active.hi), (Some(anchor.height), Some(anchor.height)));
    assert_eq!(h.min_height().unwrap(), Some(anchor.height - 24));
    for header in &anchor.headers {
        assert!(h.by_hash(&Header::new(*header).hash()).unwrap().is_some());
    }
    assert_eq!(h.legacy_start(), None);
}

#[test]
fn a_fresh_store_gets_a_candidate_chain_that_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let s = setup(true, None);
    let a = pack(20000, 1_000_000);
    let (chain, sync) = open_chain(&h, &s).unwrap();
    assert!(matches!(sync.phase(), Phase::Candidate { .. }), "{:?}", sync.phase());
    assert_eq!(tip(&chain), a.hash);
    assert_anchored(&h, &a, AnchorState::Candidate);
    let ids = h.max_id().unwrap();
    drop(chain);

    let (chain, sync) = open_chain(&h, &s).unwrap();
    assert!(matches!(sync.phase(), Phase::Candidate { .. }));
    assert_eq!(tip(&chain), a.hash);
    assert_eq!(h.max_id().unwrap(), ids);
    assert!(!s.fail.is_tripped());
}

#[test]
fn with_masternode_sync_off_the_new_chain_is_established_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let s = setup(false, Some(15000));
    let a = pack(10000, 1_000_000);
    let (chain, sync) = open_chain(&h, &s).unwrap();
    assert_eq!(sync.phase(), &Phase::Established);
    assert_eq!(tip(&chain), a.hash);
    assert_anchored(&h, &a, AnchorState::Established);
    drop(chain);

    let (chain, sync) = open_chain(&h, &setup(false, None)).unwrap();
    assert_eq!(sync.phase(), &Phase::Established);
    assert_eq!(tip(&chain), a.hash, "an Established store keeps its anchor");
}

#[test]
fn without_packs_the_chain_starts_at_genesis() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let mut s = setup(false, None);
    s.packs.clear();
    let (chain, _) = open_chain(&h, &s).unwrap();
    let genesis = s.llmq.params().consensus.hash_genesis_block;
    assert_eq!(*tip(&chain).as_bytes(), *genesis.as_bytes());
    let m = meta(&h);
    assert_eq!((m.anchor.1, m.anchor_state), (0, AnchorState::Established));
    drop(chain);
    let (chain, _) = open_chain(&h, &s).unwrap();
    assert_eq!(chain.private_tip().unwrap().0.height, 0);
}

#[test]
fn a_reset_left_without_its_anchor_step_is_built_again() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let s = setup(true, None);
    let a = pack(20000, 1_000_000);
    h.reset(candidate(&s, &pack_anchor(&a))).unwrap();
    assert_eq!(h.view().active.hi, None);
    let (chain, _) = open_chain(&h, &s).unwrap();
    assert_eq!(tip(&chain), a.hash);
    assert_anchored(&h, &a, AnchorState::Candidate);
}

#[test]
fn a_candidate_whose_pack_is_gone_resets_and_its_subscribers_go_back() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    drop(open_chain(&h, &setup(true, None)).unwrap());
    put_subs(&h, 20000);
    let mut s = setup(true, None);
    s.packs.retain(|p| p.height != 20000);
    let (chain, _) = open_chain(&h, &s).unwrap();
    let a = pack(10000, 1_000_000);
    assert_eq!(tip(&chain), a.hash);
    assert_anchored(&h, &a, AnchorState::Candidate);
    assert_no_subs(&h);
}

#[test]
fn the_chain_store_serves_no_record_its_header_does_not_hash_to() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let header = pack(20000, 1_000_000).headers[24];
    let wrong = BlockHash::from([9; 32]);
    h.put_headers(100, &[(*wrong.as_bytes(), header_bytes(&header))]).unwrap();
    let rec = h.by_hash(&wrong).unwrap().expect("the store keeps the pair as written");
    let chain = SpvChainStore(Arc::clone(&h));
    let refused = [
        chain.by_hash(&wrong).map(drop),
        chain.by_id(rec.id).map(drop),
        chain.from_height(100).map(drop),
        chain.walk_down(rec.id, 1).map(drop),
    ];
    for r in refused {
        assert!(matches!(r, Err(StoreError::Corrupt(_))), "{r:?}");
    }
    let good = HeaderRecord {
        hash: Header::new(header).hash(),
        ..rec
    };
    assert!(store::rehashed(good).is_ok());
}

/// The Candidate `meta` the initial sync resets to for `anchor`.
fn candidate(s: &ChainHostSetup, anchor: &Anchor) -> dash_chain::ChainMeta {
    candidate_meta(s.llmq.params(), anchor)
}

fn legacy(dir: &tempfile::TempDir, pack: &AnchorPack) -> Mem {
    let h = mem(dir);
    fill_legacy(&h, pack);
    h
}

/// A legacy store: `pack`'s 25 headers through the legacy path from `pack.height − 24`,
/// filter-header rows over them with their locator, a filter row below them with none, then a
/// truncation, whose lowering stores a `meta` derived from the active chain.
fn fill_legacy<E: KvEngine + 'static, F: HdrIo>(h: &Store<E, F>, pack: &AnchorPack) {
    let start = pack.height - 24;
    let raw: Vec<_> = pack
        .headers
        .iter()
        .map(|b| (*Header::new(*b).hash().as_bytes(), header_bytes(b)))
        .collect();
    h.put_headers(start, &raw).unwrap();
    let rows = [[1u8; 32]; 25];
    let rows: Vec<&[u8]> = rows.iter().map(|r| &r[..]).collect();
    h.put_sub(Subscriber::FilterHeaders, start, &rows).unwrap();
    h.put_sub(Subscriber::Filters, start - 1, &rows[..1]).unwrap();
    h.truncate_headers(pack.height - 1).unwrap();
    h.sync_wal().unwrap();
    assert_eq!(h.legacy_start(), Some(start));
    let derived = h.chain_meta().unwrap().expect("a derived meta");
    assert_eq!(derived.anchor, (Header::new(pack.headers[0]).hash(), start));
    assert!(h.locator(Subscriber::FilterHeaders).unwrap().is_some());
    assert_eq!(h.locator(Subscriber::Filters).unwrap(), None);
    assert!(!h.view().sub(Subscriber::Filters).is_empty());
}

#[test]
fn a_legacy_store_resets_to_a_candidate_born_at_most_at_its_start() {
    let old = pack(15000, 1_000_000);
    for (mn, birth, expect) in [
        (false, None, Some(pack(10000, 1_000_000))),
        (true, None, Some(pack(10000, 1_000_000))),
        (false, Some(25000), Some(pack(10000, 1_000_000))),
        (false, Some(5000), None),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let h = legacy(&dir, &old);
        let s = setup(mn, birth);
        let (chain, sync) = open_chain(&h, &s).unwrap();
        let state = match mn {
            true => AnchorState::Candidate,
            false => AnchorState::Established,
        };
        match &expect {
            Some(a) => {
                assert_eq!(tip(&chain), a.hash, "birth {birth:?}");
                assert_anchored(&h, a, state);
            }
            None => {
                let m = meta(&h);
                assert_eq!((m.anchor.1, m.anchor_state), (0, state));
            }
        }
        assert_eq!(matches!(sync.phase(), Phase::Established), !mn);
        for header in &old.headers {
            assert!(h.by_hash(&Header::new(*header).hash()).unwrap().is_none());
        }
        for sub in Subscriber::ALL {
            assert!(h.view().sub(sub).is_empty(), "{sub:?}");
            assert_eq!(h.locator(sub).unwrap(), None, "{sub:?}");
        }
        assert!(!s.fail.is_tripped());
    }
}

#[test]
fn a_store_reopened_from_disk_keeps_whether_a_reset_wrote_it() {
    let dir = tempfile::tempdir().unwrap();
    let disk = || Arc::new(Store::open_dir(dir.path()).unwrap());
    let old = pack(15000, 1_000_000);
    fill_legacy(&disk(), &old);
    let h = disk();
    assert_eq!(h.legacy_start(), Some(old.height - 24));
    let s = setup(false, None);
    let (chain, _) = open_chain(&h, &s).unwrap();
    let opened = (tip(&chain), h.max_id().unwrap());
    drop((chain, h));

    let h = disk();
    assert_eq!(h.legacy_start(), None);
    for sub in Subscriber::ALL {
        assert!(h.view().sub(sub).is_empty(), "{sub:?}");
    }
    let (chain, _) = open_chain(&h, &s).unwrap();
    assert_eq!((tip(&chain), h.max_id().unwrap()), opened, "the chain is opened, not reset");
}

#[test]
fn a_fresh_store_reopened_from_disk_opens_its_chain() {
    let dir = tempfile::tempdir().unwrap();
    let disk = || Arc::new(Store::open_dir(dir.path()).unwrap());
    let s = setup(true, None);
    let h = disk();
    let (chain, _) = open_chain(&h, &s).unwrap();
    let opened = (tip(&chain), h.max_id().unwrap());
    drop((chain, h));

    let h = disk();
    assert_eq!(h.legacy_start(), None, "the reset's marker reached disk");
    let (chain, sync) = open_chain(&h, &s).unwrap();
    assert!(matches!(sync.phase(), Phase::Candidate { .. }), "{:?}", sync.phase());
    assert_eq!((tip(&chain), h.max_id().unwrap()), opened, "the chain is opened, not reset");
}

#[test]
fn only_a_store_no_reset_wrote_takes_the_legacy_reset() {
    let s = setup(true, None);
    let candidate = candidate(&s, &Anchor::Genesis);
    let dir = tempfile::tempdir().unwrap();
    let h = legacy(&dir, &pack(15000, 1_000_000));
    let refused = h.reset(candidate).unwrap_err();
    assert!(refused.to_string().contains("Established"), "{refused}");
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    drop(open_chain(&h, &s).unwrap());
    let refused = h.reset_legacy(candidate).unwrap_err();
    assert!(refused.to_string().contains("legacy reset"), "{refused}");
    assert_eq!(meta(&h).anchor.1, 20000, "nothing was written");
}

fn live_tip(source: &ChainSource) -> (ChainGen, BlockHash) {
    match &source.state {
        SourceState::Live {
            reader,
            ..
        } => (source.gen, reader.snapshot().tip.tip_hash),
        _ => panic!("not live at {:?}", source.gen),
    }
}

#[tokio::test]
async fn the_opened_chain_is_live_at_generation_one_until_the_host_drops() {
    let dir = tempfile::tempdir().unwrap();
    let store = SpvStore::open(dir.path()).unwrap();
    let host = SpvChainHost::open(store.clone(), setup(false, None)).await.unwrap();
    assert_eq!(host.initial_sync().phase(), &Phase::Established);
    let mut source = host.source();
    let a = pack(20000, 1_000_000);
    assert_eq!(live_tip(&source.borrow_and_update()), (ChainGen(1), a.hash));
    drop(host);
    source.wait_for(|now| matches!(now.state, SourceState::Stopped)).await.unwrap();
    assert_eq!(source.borrow().gen, ChainGen(1));
    assert!(source.changed().await.is_err(), "the source closes with its task");
}

/// Filter-header rows at the anchor with their locator, and a filter row with none.
fn put_subs<E: KvEngine + 'static, F: HdrIo>(h: &Store<E, F>, at: u32) {
    let row = [1u8; 32];
    h.put_sub(Subscriber::FilterHeaders, at, &[&row[..]]).unwrap();
    h.put_sub(Subscriber::Filters, at - 1, &[&row[..]]).unwrap();
    assert!(h.locator(Subscriber::FilterHeaders).unwrap().is_some());
    assert_eq!(h.locator(Subscriber::Filters).unwrap(), None);
}

fn assert_no_subs<E: KvEngine + 'static, F: HdrIo>(h: &Store<E, F>) {
    for sub in Subscriber::ALL {
        assert!(h.view().sub(sub).is_empty(), "{sub:?}");
        assert_eq!(h.locator(sub).unwrap(), None, "{sub:?}");
    }
}

#[tokio::test]
async fn replacement_resets_while_replacing_then_publishes_the_next_generation() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let s = setup(true, None);
    let host = SpvChainHost::open_on(h.clone(), s.clone()).await.unwrap();
    let old = pack(20000, 1_000_000);
    let new = pack(10000, 1_000_000);
    put_subs(&h, old.height);
    let seen = Arc::new(Mutex::new(None));
    {
        let (seen, source) = (Arc::clone(&seen), host.source());
        h.interleave(Point::WriterWaiting, move |store| {
            let now = source.borrow().clone();
            let replacing = matches!(now.state, SourceState::Replacing);
            *seen.lock() = Some((now.gen, replacing, store.chain_meta().unwrap()));
        });
    }
    let out = host.handle().replace(pack_anchor(&new)).await;
    assert!(matches!(out, ReplaceOutcome::Replaced(ChainGen(2))), "{out:?}");
    let (gen, replacing, before) = seen.lock().take().expect("the reset ran");
    assert_eq!((gen, replacing), (ChainGen(1), true), "the reset runs while gen 1 is Replacing");
    assert_eq!(before.unwrap().anchor, (old.hash, old.height), "before the reset");
    assert_eq!(live_tip(&host.source().borrow()), (ChainGen(2), new.hash));
    assert_anchored(&h, &new, AnchorState::Candidate);
    assert!(h.by_hash(&old.hash).unwrap().is_none());
    assert_no_subs(&h);
    assert!(!s.fail.is_tripped());
}

#[tokio::test]
async fn replacing_an_established_chain_is_refused_and_the_task_runs_on() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let s = setup(false, None);
    let host = SpvChainHost::open_on(h.clone(), s.clone()).await.unwrap();
    let a = pack(20000, 1_000_000);
    let before = h.max_id().unwrap();
    for _ in 0..2 {
        let out = host.handle().replace(Anchor::Genesis).await;
        let ReplaceOutcome::Refused(why) = out else {
            panic!("{out:?}");
        };
        assert!(why.contains("Established"), "{why}");
    }
    assert!(!s.fail.is_tripped());
    assert_eq!(live_tip(&host.source().borrow()), (ChainGen(1), a.hash));
    assert_anchored(&h, &a, AnchorState::Established);
    assert_eq!(h.max_id().unwrap(), before);
}

#[test]
fn every_reset_clears_the_subscribers_in_its_own_commit() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let s = setup(true, None);
    drop(open_chain(&h, &s).unwrap());
    put_subs(&h, 20000);
    h.reset(candidate(&s, &Anchor::Genesis)).unwrap();
    assert_no_subs(&h);
}
