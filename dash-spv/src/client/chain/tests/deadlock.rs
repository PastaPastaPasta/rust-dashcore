//! Subscriber pumps never deadlock the header task (PLAN-r1 §2.1, §4.4). An owner may await the
//! task while its pump drains; a pump stuck on a wait the no-wait marker does not cover holds a
//! step only until the lag budget's `time`; a stop's `stop_subscribers` frees a writer parked
//! on a pump before the store drains.

use std::future::Future;
use std::time::Duration;

use tokio::sync::oneshot;

use super::step::{bridged, mine, PEER};
use super::sub::{anchor, next, reorg_after_a, rig_in, rig_lag, PumpHook, Rig};
use super::*;
use crate::bridge::headers::BridgedHeaders;

/// The budget's `time` where it is what the test spends.
const TIME: Duration = Duration::from_millis(200);
/// A `time` no test reaches, so no pump is closed for time.
const NEVER: Duration = Duration::from_secs(60);

/// 100 headers on the anchor: one step of 100 events, past Q = E = 4.
fn hundred() -> Vec<Header> {
    mine(anchor().hash, 1_000_000, 100, 1)
}

/// A host on the client's store, with Q = E = 4 and `time`.
async fn rig_spv(time: Duration) -> (Rig, SpvStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SpvStore::open(dir.path()).unwrap();
    (rig_in(store.clone(), dir, false, 4, HEADER_WINDOW, |l| l.time = time).await, store)
}

/// A sub of `kind` whose pump awaits `stall` before its first pop.
async fn stalled(
    r: &Rig,
    kind: SubKind,
    stall: impl Future<Output = ()> + Send + 'static,
) -> ChainSub {
    let mut sub = r.host.subscribe(kind);
    *sub.hooks.pump.lock() = Some(PumpHook::Stall(Box::pin(stall)));
    next(&mut sub).await;
    sub
}

/// `f`, within a bound far past any wait these tests spend on correct code.
async fn bound<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), f).await.expect("within the test's bound")
}

/// `sub`'s pump was closed by the time limit alone, once the step had parked `time`, and its
/// next notice is one `Resync { Lag }`.
async fn closed_for_time(r: &Rig, sub: &mut ChainSub) {
    assert_eq!(r.host.subs.lag.closes(), [(sub.kind(), "time")]);
    assert!(r.host.subs.lag.most() >= r.setup.lag.time);
    let n = next(sub).await;
    assert!(
        matches!(
            n,
            ChainNotice::Resync {
                why: ResyncWhy::Lag,
                ..
            }
        ),
        "{n:?}"
    );
    assert!(!r.setup.fail.is_tripped());
}

/// SPV-LO9 positive: through a 2,900-block reorg with Q = E = 4, owners await a `HeaderHandle`
/// reply after each notice. Their pumps drain while they wait, so every step completes, with
/// `time` out of reach, and no pump is closed. It tests freedom from deadlock, not the event
/// limit, so `events` is out of reach too: on a starved runtime that limit may close a healthy
/// pump that went unscheduled for a slice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owners_await_the_task_while_their_pumps_drain() {
    let r = rig_lag(false, 4, 2048, |l| {
        l.time = NEVER;
        l.events = u64::MAX;
    })
    .await;
    let b = reorg_after_a(&r).await;
    let until = b[2949].hash();
    let mut owners = Vec::new();
    for kind in [SubKind::Filters, SubKind::Blocks] {
        let mut sub = r.host.subscribe(kind);
        next(&mut sub).await;
        let handle = r.host.handle();
        owners.push(tokio::spawn(async move {
            loop {
                let n = next(&mut sub).await;
                handle.headers(PEER, BridgedHeaders::default()).await.unwrap();
                match n {
                    ChainNotice::Advanced {
                        to,
                        ..
                    }
                    | ChainNotice::Reorg {
                        new_tip: to,
                        ..
                    } if to.hash == until => return,
                    ChainNotice::Resync {
                        ..
                    } => panic!("{n:?}"),
                    _ => {}
                }
            }
        }));
    }
    for batch in b.chunks(r.setup.feeds.step_headers as usize) {
        r.send(batch).await;
    }
    for owner in owners {
        bound(owner).await.unwrap();
    }
    assert_eq!(r.host.subs.lag.closes(), []);
}

/// SPV-LO9 negative 2: a pump stuck on a wait the no-wait marker does not see holds the step
/// until the budget's `time` is spent; then it is closed, the step completes, and its sub
/// resyncs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unmarked_stalled_pump_is_resynced_after_time() {
    let r = rig_lag(false, 4, HEADER_WINDOW, |l| l.time = TIME).await;
    let mut sub = stalled(&r, SubKind::Filters, std::future::pending()).await;
    r.send(&hundred()).await;
    closed_for_time(&r, &mut sub).await;
}

/// The stop order (S4a's seam): with `time` out of reach, a writer parked on a stuck pump holds
/// its store job. `stop_subscribers` closes every feed, so the job returns and the store drains
/// with no `StopTimedOut`. Correct code drains within one park slice; the drain's 30 s limit
/// is far past it, and short of `time`, so it cannot flake on correct code.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_subscribers_frees_a_parked_writer_before_the_drain() {
    let (r, store) = rig_spv(NEVER).await;
    let _stuck = stalled(&r, SubKind::Blocks, std::future::pending()).await;
    let sending = {
        let (handle, headers) = (r.host.handle(), bridged(&hundred()));
        tokio::spawn(async move { handle.headers(PEER, headers).await })
    };
    bound(r.host.subs.lag.parks.notified()).await;
    let closed = store.close();
    r.host.stop_subscribers();
    closed.drain(Duration::from_secs(30)).await.unwrap();
    bound(sending).await.unwrap().unwrap();
    assert_eq!(r.host.subs.lag.closes(), []);
    assert!(!r.setup.fail.is_tripped());
}

/// The marker does not cover [`IngressReplies`]. A pump awaiting a batch's reply waits on the
/// batch's step, which waits on the pump; the time limit breaks the cycle, and the reply comes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pump_awaiting_ingress_replies_is_closed_for_time() {
    let mut r = rig_lag(false, 4, HEADER_WINDOW, |l| l.time = TIME).await;
    let (mut ingress, mut replies) = r.host.take_ingress().unwrap();
    let (tx, got) = oneshot::channel();
    let mut sub = stalled(&r, SubKind::Filters, async move {
        let _ = tx.send(replies.recv().await);
    })
    .await;
    assert_eq!(ingress.offer(PEER, bridged(&hundred())).unwrap(), Offered::Queued);
    let reply = bound(got).await.unwrap().expect("a reply");
    assert_eq!(reply.reply.unwrap().snap.tip.tip_height, 20100);
    closed_for_time(&r, &mut sub).await;
}

/// The marker does not cover `HeaderIngress::space().notified()`. The task notifies it as it
/// takes a batch, before the batch's step, so a pump waiting for the notice after that one waits
/// on the step that waits on it; the time limit breaks the cycle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pump_awaiting_ingress_space_is_closed_for_time() {
    let mut r = rig_lag(false, 4, HEADER_WINDOW, |l| l.time = TIME).await;
    let (mut ingress, mut replies) = r.host.take_ingress().unwrap();
    let (space, subs, (tx, first)) =
        (ingress.space(), Arc::clone(&r.host.subs), oneshot::channel());
    let mut sub = stalled(&r, SubKind::Filters, async move {
        space.notified().await;
        let _ = tx.send(subs.lag.closes());
        space.notified().await;
    })
    .await;
    assert_eq!(ingress.offer(PEER, bridged(&hundred())).unwrap(), Offered::Queued);
    assert!(bound(replies.recv()).await.expect("a reply").reply.is_ok());
    assert_eq!(bound(first).await.unwrap(), [], "the batch's notice came before the close");
    closed_for_time(&r, &mut sub).await;
}

/// The marker does not cover `SpvChainHost::source().changed()`. The source changes only at a
/// replacement or a stop, which the task runs between steps, so a pump waiting for it inside a
/// step waits on the task: a `Replace` sent while the step is parked publishes `Replacing` only
/// after the time limit has closed the pump and the step has returned. The pump reads the
/// source's latest value, `Replacing`, or `Live` at gen 2 if it runs after the replacement, never
/// the step's own `Live` at gen 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pump_awaiting_the_source_is_closed_for_time() {
    let r = rig_lag(true, 4, HEADER_WINDOW, |l| l.time = TIME).await;
    let (mut source, subs, (tx, seen)) =
        (r.host.source(), Arc::clone(&r.host.subs), oneshot::channel());
    let _sub = stalled(&r, SubKind::Filters, async move {
        let _ = source.changed().await;
        let first = {
            let s = source.borrow();
            let state = match s.state {
                SourceState::Live {
                    ..
                } => "Live",
                SourceState::Replacing => "Replacing",
                SourceState::Stopped => "Stopped",
            };
            (s.gen, state)
        };
        let _ = tx.send((first, subs.lag.closes()));
    })
    .await;
    let sending = {
        let (handle, headers) = (r.host.handle(), bridged(&hundred()));
        tokio::spawn(async move { handle.headers(PEER, headers).await })
    };
    bound(r.host.subs.lag.parks.notified()).await;
    let replace = {
        let (handle, anchor) = (r.host.handle(), pack_anchor(&pack(10000, 1_000_000)));
        tokio::spawn(async move { handle.replace(anchor).await })
    };
    let (first, closes) = bound(seen).await.unwrap();
    assert!(matches!(first, (ChainGen(1), "Replacing") | (ChainGen(2), "Live")), "{first:?}");
    assert_eq!(closes, [(SubKind::Filters, "time")], "the pump was closed first");
    assert!(r.host.subs.lag.most() >= r.setup.lag.time);
    assert!(bound(sending).await.unwrap().is_ok());
    let out = bound(replace).await.unwrap();
    assert!(matches!(out, ReplaceOutcome::Replaced(ChainGen(2))), "{out:?}");
    assert!(!r.setup.fail.is_tripped());
}

/// The marker does not cover `SpvStore::run`. A store job runs beside the step's own job on the
/// blocking pool, so a pump that awaits one while the writer is parked on it gets it and drains:
/// the step completes with no pump closed, `time` out of reach.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pump_awaiting_a_store_job_drains_beside_the_step() {
    let (r, store) = rig_spv(NEVER).await;
    let subs = Arc::clone(&r.host.subs);
    let (tx, ran) = oneshot::channel();
    let _sub = stalled(&r, SubKind::Filters, async move {
        subs.lag.parks.notified().await;
        let _ = tx.send(store.run(|_| Ok(())).await.is_ok());
    })
    .await;
    bound(r.send(&hundred())).await;
    assert!(ran.await.unwrap(), "the job ran");
    assert_eq!(r.host.subs.lag.closes(), []);
    assert!(!r.setup.fail.is_tripped());
}
