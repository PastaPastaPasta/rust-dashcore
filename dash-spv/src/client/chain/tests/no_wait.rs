//! The no-wait marker (PLAN-r1 §2.1, §4.4 negative 1): a pump that awaits a reply or a join
//! fails at once instead of waiting.

use std::time::Duration;

use tokio::sync::oneshot;

use super::step::{bridged, mine, PEER};
use super::*;
use crate::bridge::headers::BridgedHeaders;
use crate::client::chain::no_wait::spawn_pump;

/// Runs `f` as a pump and returns what it returned, within a bound no wait would meet.
async fn in_pump<T: Send + 'static>(f: impl std::future::Future<Output = T> + Send + 'static) -> T {
    let (tx, rx) = oneshot::channel();
    spawn_pump(async move {
        let _ = tx.send(f.await);
    });
    tokio::time::timeout(Duration::from_secs(5), rx).await.expect("the pump waited").unwrap()
}

#[tokio::test]
async fn pump_await_fails_with_marker() {
    let dir = tempfile::tempdir().unwrap();
    let h = mem(&dir);
    let host = SpvChainHost::open_on(h.clone(), setup(false, None)).await.unwrap();
    let headers = bridged(&mine(pack(20000, 1_000_000).hash, 1_000_000, 2, 1));

    let (_never, rx) = oneshot::channel::<()>();
    let reply = in_pump(SpvReply::new(rx)).await;
    assert!(matches!(reply, Err(SubError::WaitOnNoWaitTask)), "{reply:?}");
    let join = in_pump(SpvJoin::new(tokio::spawn(std::future::pending::<()>()))).await;
    assert!(matches!(join, Err(SubError::WaitOnNoWaitTask)), "{join:?}");

    let handle = host.handle();
    let out = in_pump({
        let headers = headers.clone();
        async move { handle.headers(PEER, headers).await }
    })
    .await;
    let Err(SpvError::ChannelFailure(_, why)) = &out else {
        panic!("{out:?}");
    };
    assert_eq!(*why, SubError::WaitOnNoWaitTask.to_string());
    host.handle().headers(PEER, BridgedHeaders::default()).await.unwrap();
    let tip = host.pinned().unwrap().snap.tip.tip_hash;
    assert_eq!(tip, pack(20000, 1_000_000).hash, "the refused request was never queued");
    let job = in_pump({
        let h = h.clone();
        async move { h.job(|| ()).await }
    })
    .await;
    assert!(matches!(job, Err(SpvError::ChannelFailure(..))), "{job:?}");
    let store = SpvStore::open(&dir.path().join("spv")).unwrap();
    let job = in_pump(async move { store.job(|| ()).await }).await;
    assert!(matches!(job, Err(SpvError::ChannelFailure(..))), "the client store: {job:?}");

    let reply = host.handle().headers(PEER, headers).await.unwrap();
    assert_eq!(reply.snap.tip.tip_height, 20002, "unmarked, the same request is answered");
}
