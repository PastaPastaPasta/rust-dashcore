//! Peer scoring of header batches (PLAN-r1 §2.2.3, rule 1, §9 P3 and P4): Core's checks before
//! `ProcessNewBlockHeaders` and Core's weights.

use dash_chain::{HeaderStop, PeerId};
use dash_consensus::BlockValidationResult;

use super::step::{bridged, mine, mine_bits, PEER, POW_LIMIT_BITS, SPACING};
use super::*;
use crate::bridge::headers::BridgedHeaders;
use crate::client::chain::score;

const T: u32 = 1_000_000;

fn anchor() -> AnchorPack {
    pack(20000, T)
}

/// `n` headers whose first parent no chain holds.
fn orphans(n: u32, salt: u8) -> BridgedHeaders {
    bridged(&mine(BlockHash::from([5; 32]), T, n, salt))
}

fn unconnecting(weight: Option<u32>, n: u32) -> Option<HeaderMisbehaviour> {
    weight.map(|weight| HeaderMisbehaviour {
        result: None,
        reason: format!("{n} non-connecting headers"),
        weight,
    })
}

/// A host at the anchor with A1…A3 accepted.
async fn host(h: &Mem) -> (SpvChainHost, Vec<Header>) {
    let host = SpvChainHost::open_on(h.clone(), setup(false, None)).await.unwrap();
    let a = mine(anchor().hash, T, 3, 1);
    let reply = host.handle().headers(PEER, bridged(&a)).await.unwrap();
    assert_eq!((reply.stop, reply.refused, reply.misbehaviour), (None, None, None));
    (host, a)
}

/// `HandleFewUnconnectingHeaders` (`net_processing.cpp:3143-3170`): each short batch with an
/// unknown parent is refused with `getheaders` from the best header's locator, and every 10th in
/// a row scores 20.
#[tokio::test]
async fn score_unconnecting_every_tenth_is_20() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let (host, a) = host(&h).await;
    let orphans = orphans(1, 3);
    for n in 1..=20u32 {
        let r = host.handle().headers(PEER, orphans.clone()).await.unwrap();
        let Some(Refused::Unconnecting {
            locator,
        }) = &r.refused
        else {
            panic!("{n}: {:?}", r.refused);
        };
        assert_eq!(locator.first(), Some(&a[2].hash()), "{n}: from the best header");
        assert_eq!(locator.last(), Some(&anchor().hash), "{n}");
        assert_eq!((&r.stop, r.last_accepted), (&None, None), "{n}");
        assert_eq!(r.misbehaviour, unconnecting(n.is_multiple_of(10).then_some(20), n), "{n}");
    }
    assert!(h.by_hash(&orphans.batches(1)[0].hashes[0]).unwrap().is_none(), "nothing stored");
}

/// §9 P4 (1): `nCount <= MAX_BLOCKS_TO_ANNOUNCE` takes the few path (`:3341-3348`), so 7 and 8
/// headers count and ask; 9 score 10 with no `accept` and leave the count as it is.
#[tokio::test]
async fn unconnecting_split_is_at_eight() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let (host, _) = host(&h).await;
    for n in [7, 8] {
        let r = host.handle().headers(PEER, orphans(n, n as u8)).await.unwrap();
        assert!(matches!(r.refused, Some(Refused::Unconnecting { .. })), "{n}: {:?}", r.refused);
        assert_eq!(r.misbehaviour, None, "{n}");
    }
    let nine = orphans(9, 9);
    let r = host.handle().headers(PEER, nine.clone()).await.unwrap();
    assert_eq!((r.refused, r.stop), (Some(Refused::UnconnectingBatch), None));
    let invalid = HeaderMisbehaviour {
        result: None,
        reason: "invalid header received".into(),
        weight: 10,
    };
    assert_eq!(r.misbehaviour, Some(invalid));
    for hash in &nine.batches(9)[0].hashes {
        assert!(h.by_hash(hash).unwrap().is_none(), "not accepted");
    }
    for n in 3..=10 {
        let r = host.handle().headers(PEER, orphans(1, 3)).await.unwrap();
        assert_eq!(r.misbehaviour, unconnecting((n == 10).then_some(20), n), "{n}");
    }
}

/// §9 P4 (3): the count resets after a batch is accepted (`:3385`), and not after one that
/// connects but is rejected (`:3367-3371` returns first) nor after an empty one (`:3333-3336`).
#[tokio::test]
async fn unconnecting_count_resets_on_connect() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let (host, a) = host(&h).await;
    let handle = host.handle();
    let few = || handle.headers(PEER, orphans(1, 3));
    for n in 1..=9 {
        assert_eq!(few().await.unwrap().misbehaviour, None, "{n}");
    }
    let r = handle.headers(PEER, BridgedHeaders::default()).await.unwrap();
    assert_eq!((r.refused, r.misbehaviour), (None, None));
    let time = T + 4 * SPACING;
    let bad = mine_bits(a[2].hash(), time, 1, 4, POW_LIMIT_BITS - 1);
    let r = handle.headers(PEER, bridged(&bad)).await.unwrap();
    assert!(
        matches!(
            r.stop,
            Some(HeaderStop::Invalid {
                index: 0,
                ..
            })
        ),
        "{:?}",
        r.stop
    );
    assert_eq!(few().await.unwrap().misbehaviour, unconnecting(Some(20), 10), "no reset");

    let good = mine(a[2].hash(), T + 3 * SPACING, 1, 5);
    let r = handle.headers(PEER, bridged(&good)).await.unwrap();
    assert_eq!((r.stop, r.refused), (None, None));
    for n in 1..=10 {
        let r = few().await.unwrap();
        assert_eq!(r.misbehaviour, unconnecting((n == 10).then_some(20), n), "{n}: reset");
    }
}

/// The count is each peer's own.
#[tokio::test]
async fn unconnecting_counts_per_peer() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let (host, _) = host(&h).await;
    for n in 1..=9 {
        host.handle().headers(PEER, orphans(1, 3)).await.unwrap();
        let other = host.handle().headers(PeerId(8), orphans(1, 3)).await.unwrap();
        assert_eq!(other.misbehaviour, None, "{n}");
    }
    let r = host.handle().headers(PEER, orphans(1, 3)).await.unwrap();
    assert_eq!(r.misbehaviour, unconnecting(Some(20), 10));
}

/// Core's `MaybePunishNodeForBlock` (`net_processing.cpp:1974-2018`) for each result, from an
/// outbound peer and not via a compact block.
#[test]
fn reject_weights_follow_maybe_punish_node_for_block() {
    use BlockValidationResult as R;
    let weights = [
        (R::Unset, 0),
        (R::Consensus, 100),
        (R::RecentConsensusChange, 0),
        (R::CachedInvalid, 100),
        (R::InvalidHeader, 100),
        (R::Mutated, 100),
        (R::MissingPrev, 10),
        (R::InvalidPrev, 100),
        (R::TimeFuture, 0),
        (R::Checkpoint, 100),
        (R::ChainLock, 10),
    ];
    for (result, weight) in weights {
        assert_eq!(score::reject_weight(result), weight, "{result:?}");
    }
}

/// Rule 1, weight 100: an invalid header (`bad-diffbits`); the peer is kept.
#[tokio::test]
async fn score_weight_100_invalid_header() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let (host, a) = host(&h).await;
    let bad = mine_bits(a[2].hash(), T + 4 * SPACING, 1, 4, POW_LIMIT_BITS - 1);
    let r = host.handle().headers(PEER, bridged(&bad)).await.unwrap();
    let want = HeaderMisbehaviour {
        result: Some(BlockValidationResult::InvalidHeader),
        reason: "bad-diffbits".into(),
        weight: 100,
    };
    assert_eq!(r.misbehaviour, Some(want));
    let more = mine(a[2].hash(), T + 3 * SPACING, 1, 5);
    let r = host.handle().headers(PEER, bridged(&more)).await.unwrap();
    assert_eq!((r.stop, r.misbehaviour), (None, None), "the peer is kept");
}

/// Rule 1, weight 10: a header conflicting with the ChainLock (`MaybePunishNodeForBlock`'s
/// `BLOCK_CHAINLOCK`), and a batch of more than 8 headers with an unknown parent
/// (`unconnecting_split_is_at_eight`).
#[tokio::test]
async fn score_weight_10_chainlock_conflict() {
    let f = step::Forked::new(None).await;
    f.host.handle().apply_chainlock(f.lock_on_b()).await.unwrap();
    let r = f.host.handle().headers(PEER, bridged(&f.a[2..3])).await.unwrap();
    assert!(
        matches!(
            r.stop,
            Some(HeaderStop::Invalid {
                index: 0,
                ..
            })
        ),
        "{:?}",
        r.stop
    );
    let want = HeaderMisbehaviour {
        result: Some(BlockValidationResult::ChainLock),
        reason: "duplicate".into(),
        weight: 10,
    };
    assert_eq!(r.misbehaviour, Some(want));
}

/// §9 P4 (2): headers that connect but do not chain score 20 (`net_processing.cpp:3356-3360`),
/// none of them is accepted, and the unconnecting count stays. Core's connect check comes first
/// (`:3341`), and the chain is checked across the whole message, also where its steps split.
#[tokio::test]
async fn score_non_continuous_is_20() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let (host, a) = host(&h).await;
    let handle = host.handle();
    for _ in 1..=9 {
        handle.headers(PEER, orphans(1, 3)).await.unwrap();
    }
    let non_continuous = HeaderMisbehaviour {
        result: None,
        reason: "non-continuous headers sequence".into(),
        weight: 20,
    };
    let mut long = mine(a[2].hash(), T + 3 * SPACING, 2001, 6);
    long[2000] = mine(a[2].hash(), T, 1, 7)[0];
    let stray = mine(BlockHash::from([6; 32]), T, 1, 6)[0];
    for headers in [vec![long[0], stray, long[1]], long.clone()] {
        let r = handle.headers(PEER, bridged(&headers)).await.unwrap();
        assert_eq!(
            (r.refused, r.stop, r.last_accepted),
            (Some(Refused::NonContinuous), None, None)
        );
        assert_eq!(r.misbehaviour, Some(non_continuous.clone()));
        assert!(h.by_hash(&long[0].hash()).unwrap().is_none(), "nothing accepted");
    }
    let r = handle.headers(PEER, bridged(&[stray, long[1]])).await.unwrap();
    assert!(matches!(r.refused, Some(Refused::Unconnecting { .. })), "{:?}", r.refused);
    assert_eq!(r.misbehaviour, unconnecting(Some(20), 10), "the count stayed at 9");
}
