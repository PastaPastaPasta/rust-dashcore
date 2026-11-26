//! The headers manager's ingress (PLAN-r1 §2.2.3, SPV-LO9): it never waits on the task, a full
//! queue pauses the peer, and each peer's batches are stepped in the order it sent them.

use std::time::Duration;

use dash_chain::PeerId;

use super::step::{bridged, mine, PEER};
use super::*;

const OTHER: PeerId = PeerId(8);
const THIRD: PeerId = PeerId(9);

/// Mutant #15 (M5): with nothing driving the runtime the task takes no batch, so the queue fills
/// at `INGRESS_CAP`. An offer then returns at once and pauses its peer, whose next batch is
/// dropped. While a peer is paused a new peer's batch is kept too, even with room. Resumed on the
/// task's space signal, the kept batches follow the queued ones in order, and the dropped batch
/// is never stepped.
#[test]
fn a_full_ingress_pauses_the_peer_and_keeps_its_order() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let s = setup(false, None);
    let mut host = rt.block_on(SpvChainHost::open_on(h.clone(), s.clone())).unwrap();
    let (mut ingress, mut replies) = host.take_ingress().expect("the ingress");
    let a = mine(pack(20000, 1_000_000).hash, 1_000_000, 12, 1);
    let batches: Vec<_> = a.chunks(2).map(bridged).collect();
    assert_eq!(batches.len(), INGRESS_CAP + 2);

    let (sent, offered) = std::sync::mpsc::channel();
    let offers = std::thread::spawn(move || {
        let mut out: Vec<_> = batches.iter().map(|b| ingress.offer(PEER, b.clone())).collect();
        out.push(ingress.offer(OTHER, batches[0].clone()));
        let _ = sent.send(out.into_iter().map(Result::unwrap).collect::<Vec<_>>());
        ingress
    });
    let out = offered.recv_timeout(Duration::from_secs(2)).expect("an offer waited on the queue");
    let mut ingress = offers.join().unwrap();
    use Offered::{Dropped, Paused, Queued};
    assert_eq!(out, [Queued, Queued, Queued, Queued, Paused, Dropped, Paused]);
    assert!(ingress.paused(PEER) && ingress.paused(OTHER));

    let (third, got, resumed) = rt.block_on(async {
        let space = ingress.space();
        let first = tokio::time::timeout(Duration::from_secs(60), space.notified()).await;
        first.expect("the task signals space");
        let third = ingress.offer(THIRD, bridged(&a[..2])).unwrap();
        let (mut got, mut resumed) = (Vec::new(), Vec::new());
        let all = async {
            while got.len() < INGRESS_CAP + 3 {
                tokio::select! {
                    r = replies.recv() => got.push(r.expect("the task runs")),
                    _ = space.notified() => resumed.extend(ingress.resume().unwrap()),
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(60), all).await.expect("every batch answered");
        (third, got, resumed)
    });
    assert_eq!(third, Paused, "kept behind the paused peers, with room in the queue");
    assert_eq!(resumed, [PEER, OTHER, THIRD]);
    assert!(!ingress.paused(PEER) && !ingress.paused(OTHER) && !ingress.paused(THIRD));
    let peers: Vec<_> = got.iter().map(|r| r.peer).collect();
    assert_eq!(peers, [PEER, PEER, PEER, PEER, PEER, OTHER, THIRD]);
    for (k, r) in got.iter().enumerate() {
        let reply = r.reply.as_ref().unwrap();
        assert_eq!(reply.stop, None, "reply {k}");
        let last = if k < 5 {
            2 * k + 1
        } else {
            1
        };
        let rec = h.by_id(reply.last_accepted.expect("accepted")).unwrap().unwrap();
        assert_eq!(rec.hash, a[last].hash(), "reply {k}");
    }
    assert_eq!(got[4].reply.as_ref().unwrap().snap.tip.tip_hash, a[9].hash());
    for dropped in &a[10..] {
        assert!(h.by_hash(&dropped.hash()).unwrap().is_none(), "the dropped batch was stepped");
    }
    assert!(!s.fail.is_tripped());
}
