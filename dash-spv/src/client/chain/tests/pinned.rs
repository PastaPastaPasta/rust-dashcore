//! The pinned chain view (PLAN-r1 §6): one publication's chain, read as the writer reads it.

use dash_chain::test_util::SpvRig;
use dash_chain::{AncestryError, AnchoredAncestry, BlockId, FeedConfig, Needs, ReadKind, Refused};

use super::step::{bridged, mine, PEER};
use super::*;

fn rig() -> SpvRig {
    SpvRig::new(64, FeedConfig::spv(), 1, false)
}

fn pin(rig: &SpvRig) -> PinnedChain {
    PinnedChain::new(&rig.chain.reader(), ChainGen(1), Arc::default())
}

/// Every height of the snapshot's chain, resident and cold, answers as the writer's
/// ancestry; after a reorg, the old pin still answers its own chain.
#[test]
fn pinned_chain_answers_as_the_writer_for_its_snapshot() {
    let mut rig = rig();
    let a = rig.mine_on_anchor(0, 200);
    rig.extend(&a);
    let old = pin(&rig);
    let w = rig.chain.anchored();
    let (floor, tip) = (w.floor(), old.snap.tip.tip);
    assert_eq!(old.floor(), floor);
    let mut ids = Vec::new();
    for h in floor..=old.snap.tip.tip_height {
        let id = w.ancestor(tip, h).unwrap().unwrap();
        assert_eq!(old.at_height(h).unwrap(), Some(id), "height {h}");
        assert_eq!(old.header(id).unwrap(), w.header(id).unwrap(), "height {h}");
        assert_eq!(old.prev(id), w.prev(id), "height {h}");
        assert_eq!(old.ancestor(tip, h).unwrap(), Some(id));
        ids.push((id, w.header(id).unwrap(), w.prev(id)));
    }
    assert_eq!(old.at_height(floor - 1).unwrap(), None);
    assert_eq!(old.at_height(old.snap.tip.tip_height + 1).unwrap(), None);
    assert_eq!(
        old.ancestor(tip, floor - 1),
        Err(AncestryError::BelowFloor {
            height: floor - 1
        })
    );

    let b = rig.mine(a[99].hash(), a[99].time() + 157, 150);
    rig.extend(&b);
    assert_eq!(rig.chain.snapshot().tip.tip_hash, b[149].hash());
    let fork = old.snap.tip.tip_height - 100;
    for (h, (id, header, prev)) in (floor..).zip(&ids) {
        assert_eq!(old.at_height(h).unwrap(), Some(*id), "the old pin's own chain at {h}");
        assert_eq!(old.header(*id).as_ref(), Ok(header), "height {h}");
        assert_eq!(old.prev(*id), *prev, "height {h}");
        assert_eq!(old.ancestor(tip, h), Ok(Some(*id)), "height {h}");
    }
    let side = ids.iter().filter(|(_, header, _)| header.height > fork);
    assert_eq!(side.count(), 100, "the old tip's branch is a side branch now");
    let new = pin(&rig);
    assert_eq!(new.at_height(fork).unwrap(), old.at_height(fork).unwrap());
    assert_ne!(new.at_height(fork + 1).unwrap(), old.at_height(fork + 1).unwrap());
}

/// A cold read that fails is a storage error and trips the latch.
#[test]
fn pinned_chain_reads_are_fallible() {
    let mut rig = rig();
    rig.extend(&rig.mine_on_anchor(0, 200));
    let p = pin(&rig);
    let cold = p.at_height(p.floor() + 30).unwrap().unwrap();
    let other = p.at_height(p.floor() + 40).unwrap().unwrap();
    p.header(other).unwrap();
    assert!(rig.chain.reader().entry(cold).is_none(), "a cold entry");
    rig.store.faults(|f| f.fail_read(ReadKind::ById, 1));
    let read = p.header(cold);
    assert!(matches!(read, Err(AncestryError::Storage(_))), "{read:?}");
    assert_eq!(rig.chain.feeds().register(Needs::default()).err(), Some(Refused::Latched));
}

/// The host pins its latest publication, of its generation.
#[tokio::test]
async fn the_host_pins_its_latest_publication() {
    let dir = tempfile::tempdir().unwrap();
    let host = SpvChainHost::open_on(mem(&dir), setup(false, None)).await.unwrap();
    let at = pack(20000, 1_000_000);
    let reply =
        host.handle().headers(PEER, bridged(&mine(at.hash, 1_000_000, 5, 1))).await.unwrap();
    let p = host.pinned().expect("live");
    assert_eq!((p.gen, p.snap.tip_seq), (ChainGen(1), reply.snap.tip_seq));
    assert_eq!(p.at_height(20005).unwrap(), Some(reply.snap.tip.tip));
    assert_eq!(p.header(reply.snap.tip.tip).unwrap().hash, reply.snap.tip.tip_hash);
}

/// A Candidate host whose window is 64, with 300 headers on its anchor: a pin, the id of the
/// 30th header (cold, and never read) and the tip's.
async fn pinned_host(h: &Mem, s: &ChainHostSetup) -> (SpvChainHost, PinnedChain, BlockId, BlockId) {
    let host = SpvChainHost::open_on(h.clone(), s.clone()).await.unwrap();
    let headers = mine(pack(20000, 1_000_000).hash, 1_000_000, 300, 1);
    let mut last = Vec::new();
    for part in [&headers[..30], &headers[30..]] {
        let reply = host.handle().headers(PEER, bridged(part)).await.unwrap();
        last.push(reply.last_accepted.unwrap());
    }
    let SourceState::Live {
        reader,
        ..
    } = host.source().borrow().state.clone()
    else {
        panic!("not live");
    };
    assert!(reader.entry(last[0]).is_none(), "a cold entry");
    let p = host.pinned().unwrap();
    (host, p, last[0], last[1])
}

/// The hash of the header `pinned_host` returns as cold.
fn cold_hash() -> BlockHash {
    mine(pack(20000, 1_000_000).hash, 1_000_000, 30, 1)[29].hash()
}

/// Holds the next reset where it waits for the writer, before its first write: `reached` then,
/// until `release`.
fn hold_reset(h: &Mem) -> (tokio::sync::oneshot::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (reached, at_reset) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    h.interleave(Point::WriterWaiting, move |_| {
        reached.send(()).unwrap();
        wait.recv().unwrap();
    });
    (at_reset, release)
}

fn replace_to_10000(host: &SpvChainHost) -> tokio::task::JoinHandle<ReplaceOutcome> {
    let (handle, anchor) = (host.handle(), pack_anchor(&pack(10000, 1_000_000)));
    tokio::spawn(async move { handle.replace(anchor).await })
}

fn small_window() -> ChainHostSetup {
    ChainHostSetup {
        window: 64,
        ..setup(true, None)
    }
}

fn assert_retired(p: &PinnedChain, id: BlockId, s: &ChainHostSetup) {
    assert_eq!(p.header(id), Err(AncestryError::Retired));
    assert_eq!(p.prev(id), Err(AncestryError::Retired));
    assert_eq!(p.ancestor(id, p.floor()), Err(AncestryError::Retired));
    assert!(!s.fail.is_tripped());
}

/// A pin held across a replacement reads `Retired`, cold or not, with nothing tripped: its
/// store is reset under it.
#[tokio::test]
async fn a_pin_held_across_a_replacement_reads_retired() {
    let dir = tempfile::tempdir().unwrap();
    let (h, s) = (mem(&dir), small_window());
    let (host, old, cold, tip) = pinned_host(&h, &s).await;
    let out = host.handle().replace(pack_anchor(&pack(10000, 1_000_000))).await;
    assert!(matches!(out, ReplaceOutcome::Replaced(ChainGen(2))), "{out:?}");
    assert_retired(&old, cold, &s);
    assert_retired(&old, tip, &s);
    assert_eq!(old.at_height(old.snap.tip.tip_height), Err(AncestryError::Retired));
    assert_eq!(old.ancestor(tip, old.floor() - 1), Err(AncestryError::Retired));
    assert_eq!(old.lookup(&cold_hash()), Err(AncestryError::Retired));
    assert_eq!(old.entry(cold), Err(AncestryError::Retired));
    let now = host.pinned().unwrap();
    let anchor = now.snap.tip.tip;
    assert_eq!(now.lookup(&pack(10000, 1_000_000).hash), Ok(Some(anchor)));
    assert_eq!(now.entry(anchor).map(|e| e.hash), Ok(now.snap.tip.tip_hash));
    assert!(!s.fail.is_tripped());
}

/// The mark is set before the reset's first write: a read while the reset waits is `Retired`.
#[tokio::test]
async fn the_mark_precedes_the_reset() {
    let dir = tempfile::tempdir().unwrap();
    let (h, s) = (mem(&dir), small_window());
    let (host, old, cold, _) = pinned_host(&h, &s).await;
    let (at_reset, release) = hold_reset(&h);
    let replace = replace_to_10000(&host);
    at_reset.await.unwrap();
    assert_eq!(old.header(cold), Err(AncestryError::Retired));
    assert!(!s.fail.is_tripped());
    release.send(()).unwrap();
    let out = replace.await.unwrap();
    assert!(matches!(out, ReplaceOutcome::Replaced(ChainGen(2))), "{out:?}");
}

/// A read holds the guard to its end: the replacement waits in `retire()` for it, and the read
/// sees the store as it was.
#[tokio::test]
async fn a_read_in_flight_holds_back_the_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let (h, s) = (mem(&dir), small_window());
    let (host, old, cold, _) = pinned_host(&h, &s).await;
    let (parked, at_read) = tokio::sync::oneshot::channel();
    let (go, wait) = std::sync::mpsc::channel::<()>();
    old.on_read(move || {
        parked.send(()).unwrap();
        wait.recv().unwrap();
    });
    let reading = {
        let old = old.clone();
        tokio::task::spawn_blocking(move || old.header(cold))
    };
    at_read.await.unwrap();
    let (mut at_reset, release) = hold_reset(&h);
    let replace = replace_to_10000(&host);
    host.source().wait_for(|now| matches!(now.state, SourceState::Replacing)).await.unwrap();
    let early = tokio::time::timeout(std::time::Duration::from_secs(1), &mut at_reset).await;
    assert!(early.is_err(), "the reset waits for the read in flight");
    go.send(()).unwrap();
    assert_eq!(reading.await.unwrap().map(|e| e.hash), Ok(cold_hash()));
    at_reset.await.unwrap();
    release.send(()).unwrap();
    let out = replace.await.unwrap();
    assert!(matches!(out, ReplaceOutcome::Replaced(ChainGen(2))), "{out:?}");
    assert_eq!(old.header(cold), Err(AncestryError::Retired));
    assert!(!s.fail.is_tripped());
}

/// The reviewer's repro: a pin held while the next generation issues its ids again never reads
/// the next generation's block.
#[tokio::test]
async fn a_pin_held_while_ids_are_reissued_reads_retired() {
    let dir = tempfile::tempdir().unwrap();
    let (h, s) = (mem(&dir), small_window());
    let (host, old, cold, _) = pinned_host(&h, &s).await;
    let new = pack(10000, 1_000_000);
    host.handle().replace(pack_anchor(&new)).await;
    let reply = host.handle().headers(PEER, bridged(&mine(new.hash, 1_000_000, 300, 2))).await;
    let now = host.pinned().unwrap();
    assert_eq!((now.gen, now.snap.tip_seq), (ChainGen(2), reply.unwrap().snap.tip_seq));
    assert!(now.header(cold).is_ok(), "the next generation issued {cold:?} again");
    assert_retired(&old, cold, &s);
}

#[tokio::test]
async fn a_reissued_older_id_reads_the_new_block() {
    let dir = tempfile::tempdir().unwrap();
    let (h, s) = (mem(&dir), small_window());
    let (host, _, cold, _) = pinned_host(&h, &s).await;
    let new = pack(10000, 1_000_000);
    host.handle().replace(pack_anchor(&new)).await;
    let headers = mine(new.hash, 1_000_000, 300, 2);
    host.handle().headers(PEER, bridged(&headers)).await.unwrap();
    let now = host.pinned().unwrap();
    assert_eq!(now.gen, ChainGen(2));
    let e = now.entry(cold).unwrap();
    assert_ne!(e.hash, cold_hash());
    assert!(headers.iter().any(|h| h.hash() == e.hash), "gen 2's block at {cold:?}");
    assert_eq!(now.header(cold).map(|c| c.hash), Ok(e.hash));
    assert_eq!(now.lookup(&e.hash), Ok(Some(cold)));
    assert!(!s.fail.is_tripped());
}

/// An id this generation has not issued (an older one's tip) is `Retired`, not a panic.
#[tokio::test]
async fn an_unissued_id_reads_retired() {
    let dir = tempfile::tempdir().unwrap();
    let (h, s) = (mem(&dir), small_window());
    let (host, _, _, tip) = pinned_host(&h, &s).await;
    host.handle().replace(pack_anchor(&pack(10000, 1_000_000))).await;
    let now = host.pinned().unwrap();
    assert_eq!(now.gen, ChainGen(2));
    assert_retired(&now, tip, &s);
    assert!(now.header(now.snap.tip.tip).is_ok(), "the new pin is live");
}

/// The host pins nothing while it replaces the chain, and pins the next generation after.
#[tokio::test]
async fn the_host_pins_the_current_generation() {
    let dir = tempfile::tempdir().unwrap();
    let (h, s) = (mem(&dir), setup(true, None));
    let host = SpvChainHost::open_on(h.clone(), s.clone()).await.unwrap();
    let (at_reset, release) = hold_reset(&h);
    let replace = replace_to_10000(&host);
    at_reset.await.unwrap();
    assert!(host.pinned().is_none(), "nothing is pinned while Replacing");
    release.send(()).unwrap();
    let out = replace.await.unwrap();
    assert!(matches!(out, ReplaceOutcome::Replaced(ChainGen(2))), "{out:?}");
    let p = host.pinned().unwrap();
    assert_eq!((p.gen, p.snap.tip.tip_hash), (ChainGen(2), pack(10000, 1_000_000).hash));
}
