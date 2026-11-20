//! The dashd tests that arm a `dash_spv::fault_hooks` hook, on the dashd_sync harness. The
//! target requires `__fault-hooks`, so the gate runs it in its features shard.

#[path = "../dashd_sync/helpers.rs"]
#[allow(dead_code)]
mod helpers;
#[path = "../dashd_sync/setup.rs"]
#[allow(dead_code)]
mod setup;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc as std_mpsc, Arc};
use std::time::Duration;

use dash_spv::test_utils::TestChain;
use dash_spv::{EventHandler, SyncEvent};
use key_wallet_manager::WalletEvent;

use helpers::wait_for_sync;
use setup::TestContext;

/// Holds its first sync event until the progress is synced, then hands events on like
/// `TestEventHandler`.
struct SlowSyncHandler {
    inner: dash_spv::test_utils::TestEventHandler,
    errors: AtomicUsize,
    sync_events: AtomicUsize,
    synced: AtomicBool,
}

impl EventHandler for SlowSyncHandler {
    fn on_sync_event(&self, event: &SyncEvent) {
        if self.sync_events.fetch_add(1, Ordering::SeqCst) == 0 {
            let deadline = std::time::Instant::now() + dash_spv::test_utils::SYNC_TIMEOUT;
            while !self.synced.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        self.inner.on_sync_event(event);
    }
    fn on_network_event(&self, event: &dash_spv::network::NetworkEvent) {
        self.inner.on_network_event(event);
    }
    fn on_progress(&self, progress: &dash_spv::SyncProgress) {
        if progress.is_synced() {
            self.synced.store(true, Ordering::SeqCst);
        }
        self.inner.on_progress(progress);
    }
    fn on_wallet_event(&self, event: &WalletEvent) {
        self.inner.on_wallet_event(event);
    }
    fn on_error(&self, error: &str) {
        self.errors.fetch_add(1, Ordering::SeqCst);
        self.inner.on_error(error);
    }
}

struct WarningWriter(std_mpsc::Sender<Vec<u8>>);

impl std::io::Write for WarningWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.send(buf.to_vec());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A handler holding its first sync event until synced lags the `SyncEvent` monitor on an
/// 8-event monitor bus once. The monitor resyncs instead of stopping the client: the client
/// syncs and keeps running, and `on_error` is never called.
#[test]
fn slow_handler_never_stops_the_client() {
    let (sender, warnings) = std_mpsc::channel();
    let capture = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .with_writer(move || WarningWriter(sender.clone()))
            .finish(),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .on_thread_start(move || std::mem::forget(tracing::dispatcher::set_default(&capture)))
        .build()
        .unwrap();

    let ran = runtime.block_on(async {
        let Some(ctx) = TestContext::new(TestChain::Full).await else {
            return false;
        };
        let handler = Arc::new(SlowSyncHandler {
            inner: dash_spv::test_utils::TestEventHandler::new(),
            errors: AtomicUsize::new(0),
            sync_events: AtomicUsize::new(0),
            synced: AtomicBool::new(false),
        });
        let mut progress = handler.inner.subscribe_progress();
        let network = dash_spv::network::PeerNetworkManager::new(&ctx.client_config).await;
        let client = dash_spv::DashSpvClient::new(
            ctx.client_config.clone(),
            network,
            Arc::clone(&ctx.wallet),
            vec![handler.clone()],
        )
        .await
        .expect("Failed to create client");
        dash_spv::fault_hooks::with_monitor_event_capacity(8, client.run())
            .await
            .expect("client run failed");

        wait_for_sync(&mut progress, ctx.dashd.initial_height).await;
        tokio::time::timeout(dash_spv::test_utils::SYNC_TIMEOUT, async {
            while handler.sync_events.load(Ordering::SeqCst) < 2 && client.is_running() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the held monitor never resumed");
        assert!(client.is_running(), "the client stopped");
        assert_eq!(handler.errors.load(Ordering::SeqCst), 0, "on_error was called");

        client.stop().await;
        assert_eq!(handler.errors.load(Ordering::SeqCst), 0, "on_error was called");
        true
    });
    drop(runtime);
    if !ran {
        return;
    }

    let text: String =
        warnings.try_iter().map(|chunk| String::from_utf8_lossy(&chunk).into_owned()).collect();
    assert!(text.contains("SyncEvent monitor lagged, skipped"), "no lag warning in:\n{text}");
    assert!(!text.contains("monitor relay lagged"), "the relay lagged:\n{text}");
}
