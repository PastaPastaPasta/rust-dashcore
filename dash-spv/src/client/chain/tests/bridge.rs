//! The chain's `SyncEvent` feed (PLAN-r1 §2.4.4).

use std::time::Duration;

use tokio::sync::broadcast;

use super::step::{bridged, mine, PEER, SPACING};
use super::sub::{anchor, bound, hand, relive, rig, Rig};
use super::*;
use crate::network::NetworkManager;
use crate::sync::SyncEvent;
use crate::test_utils::MockNetworkManager;

struct Bridged {
    r: Rig,
    ingress: HeaderIngress,
    bridge: ChainEventBridge,
    network: Arc<MockNetworkManager>,
}

/// A started host, its ingress, and a bridge on it whose start has been taken.
async fn bridged_rig() -> Bridged {
    let mut r = rig(1024, HEADER_WINDOW).await;
    let (ingress, _) = r.host.take_ingress().unwrap();
    let network = Arc::new(MockNetworkManager::new());
    let mut bridge = r.host.event_bridge(broadcast::channel(16).0, network.clone());
    assert_eq!(events(&mut bridge).await, [stored(anchor().height)]);
    Bridged {
        r,
        ingress,
        bridge,
        network,
    }
}

async fn events(bridge: &mut ChainEventBridge) -> Vec<String> {
    let events = bound(bridge.next_events()).await.expect("the chain runs");
    events.iter().map(ToString::to_string).collect()
}

fn stored(tip_height: u32) -> String {
    SyncEvent::BlockHeadersStored {
        tip_height,
    }
    .to_string()
}

fn complete(tip_height: u32, connection_epoch: u64) -> String {
    SyncEvent::BlockHeaderSyncComplete {
        tip_height,
        connection_epoch,
    }
    .to_string()
}

fn reorganized(fork_height: u32, disconnected: u32, connected: u32) -> String {
    SyncEvent::ChainReorganized {
        fork_height,
        disconnected,
        connected,
    }
    .to_string()
}

/// `headers` from the sync peer, a short reply taken whole: the chain is caught up at their
/// last.
async fn caught_up(b: &mut Bridged, headers: &[Header]) {
    let round = Round {
        asked: b.r.host.status().borrow().asked(),
        complete: true,
        unknown_announced: false,
    };
    assert_eq!(b.ingress.reply(PEER, bridged(headers), round).unwrap(), Offered::Queued);
    let last = headers.last().expect("headers").hash();
    let mut status = b.r.host.status();
    bound(status.wait_for(|s| s.caught_up && s.tip.hash == last)).await.unwrap();
}

/// `a`, ten headers on the anchor, and `b`, eight on `a`'s fifth: a reorg with its fork at the
/// anchor + 5 that disconnects 5 and connects 8.
fn branches() -> (Vec<Header>, Vec<Header>) {
    let a = mine(anchor().hash, 1_000_000, 10, 1);
    let b = mine(a[4].hash(), 1_000_000 + 5 * SPACING, 8, 2);
    (a, b)
}

fn close_for_lag(r: &Rig) {
    for e in r.host.subs.live(ChainGen(1)) {
        e.close();
    }
}

/// Each step's tip is stored, a reorg is announced at its last common ancestor before its tip,
/// and the caught-up transition completes once, stamped with the network's disconnect count.
#[tokio::test]
async fn the_bridge_follows_each_notice() {
    let mut b = bridged_rig().await;
    let h = anchor().height;
    let (a, fork) = branches();
    b.r.send(&a).await;
    assert_eq!(events(&mut b.bridge).await, [stored(h + 10)]);
    b.r.send(&fork).await;
    assert_eq!(events(&mut b.bridge).await, [reorganized(h + 5, 5, 8), stored(h + 13)]);

    b.network.set_connected(0);
    b.network.set_connected(1);
    let more = mine(fork[7].hash(), 1_000_000 + 13 * SPACING, 1, 2);
    caught_up(&mut b, &more).await;
    assert_eq!(events(&mut b.bridge).await, [stored(h + 14)]);
    assert_eq!(events(&mut b.bridge).await, [complete(h + 14, 1)]);
}

/// A subscriber lagged across the caught-up transition gets it in its `Resync`, and so does
/// the bridge's listener, with the tip it missed.
#[tokio::test]
async fn a_resync_across_the_caught_up_transition_completes() {
    let mut b = bridged_rig().await;
    close_for_lag(&b.r);
    let a = mine(anchor().hash, 1_000_000, 5, 1);
    caught_up(&mut b, &a).await;
    let h = anchor().height;
    assert_eq!(events(&mut b.bridge).await, [stored(h + 5), complete(h + 5, 0)]);
}

/// A `Resync` on the same generation emits the reorg it skipped, read through its pin.
#[tokio::test]
async fn a_resync_across_a_reorg_reports_it() {
    let mut b = bridged_rig().await;
    let h = anchor().height;
    let (a, fork) = branches();
    b.r.send(&a).await;
    assert_eq!(events(&mut b.bridge).await, [stored(h + 10)]);
    close_for_lag(&b.r);
    b.r.send(&fork).await;
    assert_eq!(events(&mut b.bridge).await, [reorganized(h + 5, 5, 8), stored(h + 13)]);
}

/// Across generations no id is compared: the new generation's tip is stored, the same block
/// or not.
#[tokio::test]
async fn a_resync_on_a_new_generation_stores_its_tip() {
    let r = rig(1024, HEADER_WINDOW).await;
    let (tx, sub) = hand(&r);
    let network: Arc<dyn NetworkManager> = Arc::new(MockNetworkManager::new());
    let mut bridge =
        ChainEventBridge::new(sub, tx.source.subscribe(), broadcast::channel(16).0, network);
    assert_eq!(events(&mut bridge).await, [stored(anchor().height)]);
    relive(&r, &tx, 2);
    assert_eq!(events(&mut bridge).await, [stored(anchor().height)]);
}

/// `run` sends each event on the bus, and ends once the chain stops.
#[tokio::test]
async fn the_bridge_runs_until_the_chain_stops() {
    let r = rig(1024, HEADER_WINDOW).await;
    let (tx, mut rx) = broadcast::channel(16);
    let bridge = r.host.event_bridge(tx, Arc::new(MockNetworkManager::new()));
    let task = tokio::spawn(bridge.run());
    let first = bound(rx.recv()).await.unwrap();
    assert_eq!(first.to_string(), stored(anchor().height));
    let a = mine(anchor().hash, 1_000_000, 3, 1);
    r.send(&a).await;
    let next = bound(rx.recv()).await.unwrap();
    assert_eq!(next.to_string(), stored(anchor().height + 3));
    drop(r);
    tokio::time::timeout(Duration::from_secs(20), task).await.expect("it ends").unwrap();
}
