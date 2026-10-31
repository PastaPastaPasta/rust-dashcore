//! SB0 §7's thread-count test (m2, DEC-112 O5), over the old masternode path: an SPV sync
//! through dash-spv-ffi with its mobile runtime (2 workers, 2 blocking threads), dash-spv's
//! injected CPU pool and blst built `no-threads`, against the regtest masternode network.
//! The sync covers headers, filters with wallet matching, the masternode lists (QRInfo and
//! MnListDiff verified by dashcore's verifier, which reaches blst's pooled calls) and a reorg.
//! A sampler counts `/proc/self/task` throughout; no more than the platform baseline (the
//! threads before the client exists), plus the runtime's workers and blocking threads, plus
//! the CPU pool, may ever exist, and the CPU pool's threads are the same throughout (one pool
//! for the client's lifetime, across its restarts).
//!
//! It needs blst's `no-threads` off mobile: `cargo test -p dash-spv-ffi --features
//! blst-no-threads --test thread_count` (the target's `required-features`).

#[allow(dead_code)]
#[path = "dashd_sync/callbacks.rs"]
mod callbacks;

use std::collections::BTreeSet;
use std::ffi::{CStr, CString};
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use callbacks::{
    create_network_callbacks, create_sync_callbacks, create_wallet_callbacks, CallbackTracker,
};
use dash_network::ffi::FFINetwork;
use dash_spv::test_utils::MasternodeTestContext;
use dash_spv::CpuPool;
use dash_spv_ffi::client::{
    dash_spv_ffi_client_destroy, dash_spv_ffi_client_get_wallet_manager, dash_spv_ffi_client_new,
    dash_spv_ffi_client_run, dash_spv_ffi_client_stop, dash_spv_ffi_wallet_manager_free,
    MOBILE_MAX_BLOCKING_THREADS, MOBILE_WORKER_THREADS,
};
use dash_spv_ffi::config::{
    dash_spv_ffi_config_add_peer, dash_spv_ffi_config_destroy, dash_spv_ffi_config_new,
    dash_spv_ffi_config_set_data_dir, dash_spv_ffi_config_set_masternode_sync_enabled,
    dash_spv_ffi_config_set_restrict_to_configured_peers,
};
use dash_spv_ffi::FFIEventCallbacks;
use key_wallet_ffi::wallet_manager::wallet_manager_add_wallet_from_mnemonic;
use key_wallet_ffi::{FFIError, FFIWalletManager};

const TIMEOUT: Duration = Duration::from_secs(180);

/// The threads of this process: their ids and names, by name.
fn threads() -> Vec<(String, u32)> {
    let mut threads: Vec<(String, u32)> = fs::read_dir("/proc/self/task")
        .expect("/proc/self/task")
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let tid = e.file_name().to_str()?.parse().ok()?;
            let name = fs::read_to_string(e.path().join("comm")).ok()?;
            Some((name.trim().to_string(), tid))
        })
        .collect();
    threads.sort();
    threads
}

/// What the sampler saw: the threads at the peak, and every CPU-pool thread.
#[derive(Default)]
struct Seen {
    peak: Vec<(String, u32)>,
    cpu_pool: BTreeSet<u32>,
}

/// Samples the threads every millisecond.
struct Sampler {
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Seen>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Sampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (s, p) = (stop.clone(), seen.clone());
        let handle = thread::Builder::new()
            .name("thread-sampler".into())
            .spawn(move || {
                while !s.load(Ordering::Relaxed) {
                    let now = threads();
                    let mut seen = p.lock().unwrap();
                    seen.cpu_pool.extend(
                        now.iter()
                            .filter(|(name, _)| name.starts_with(dash_spv::CPU_POOL_THREAD_NAME))
                            .map(|&(_, tid)| tid),
                    );
                    if now.len() > seen.peak.len() {
                        seen.peak = now;
                    }
                    drop(seen);
                    thread::sleep(Duration::from_millis(1));
                }
            })
            .expect("the sampler");
        // Its first sample includes itself.
        while seen.lock().unwrap().peak.is_empty() {
            thread::sleep(Duration::from_millis(1));
        }
        Self {
            stop,
            seen,
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> Seen {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().expect("running").join().expect("the sampler");
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

fn wait(what: &str, tracker: &CallbackTracker, done: impl Fn(&CallbackTracker) -> bool) {
    let start = Instant::now();
    while !done(tracker) {
        assert!(start.elapsed() < TIMEOUT, "{what}: not within {TIMEOUT:?}");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn spv_threads_stay_within_the_mobile_bound() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let Some(mut mn) = rt.block_on(MasternodeTestContext::new(true)) else {
        return;
    };
    let target = mn.expected_height;
    let tmp = tempfile::TempDir::new().unwrap();
    let tracker = Arc::new(CallbackTracker::default());

    // The baseline: this process before the client exists (the test harness, the sampler).
    let sampler = Sampler::start();
    thread::sleep(Duration::from_millis(20));
    let baseline = threads().len();

    unsafe {
        let config = dash_spv_ffi_config_new(FFINetwork::Regtest);
        let dir = CString::new(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(dash_spv_ffi_config_set_data_dir(config, dir.as_ptr()), 0);
        assert_eq!(dash_spv_ffi_config_set_masternode_sync_enabled(config, true), 0);
        let peer = CString::new(mn.controller_addr.to_string()).unwrap();
        assert_eq!(dash_spv_ffi_config_add_peer(config, peer.as_ptr()), 0);
        assert_eq!(dash_spv_ffi_config_set_restrict_to_configured_peers(config, true), 0);
        let callbacks = FFIEventCallbacks {
            sync: create_sync_callbacks(&tracker),
            network: create_network_callbacks(&tracker),
            wallet: create_wallet_callbacks(&tracker),
            ..FFIEventCallbacks::default()
        };
        let client = dash_spv_ffi_client_new(config, callbacks);
        assert!(!client.is_null(), "the client");

        // A wallet, so each filter batch runs the wallet matching.
        let wm = dash_spv_ffi_client_get_wallet_manager(client);
        let mnemonic = CString::new(mn.wallet.mnemonic.clone()).unwrap();
        let mut error = FFIError::default();
        let added = wallet_manager_add_wallet_from_mnemonic(
            wm as *mut FFIWalletManager,
            mnemonic.as_ptr(),
            &mut error,
        );
        let message = (!error.message.is_null())
            .then(|| CStr::from_ptr(error.message).to_string_lossy().into_owned());
        assert!(added, "the wallet: {message:?}");

        assert_eq!(dash_spv_ffi_client_run(client), 0);
        wait("the masternode lists", &tracker, |t| {
            t.masternode_state_updated_count.load(Ordering::SeqCst) > 0
        });
        wait("the initial sync", &tracker, |t| {
            t.sync_complete_count.load(Ordering::SeqCst) > 0
                && t.last_header_tip.load(Ordering::SeqCst) >= target
                && t.last_filter_tip.load(Ordering::SeqCst) >= target
        });

        // A reorg: the controller drops its tip and mines a longer branch.
        let tip = mn.controller.get_best_block_hash();
        mn.controller.invalidate_block(&tip);
        mn.move_blocks(3);
        let height = mn.controller.get_block_count();
        wait("the reorg's headers", &tracker, |t| {
            t.last_header_tip.load(Ordering::SeqCst) >= height
        });
        wait("the reorg's filters", &tracker, |t| {
            t.last_filter_tip.load(Ordering::SeqCst) >= height
        });

        dash_spv_ffi_client_stop(client);
        dash_spv_ffi_wallet_manager_free(wm);
        dash_spv_ffi_client_destroy(client);
        dash_spv_ffi_config_destroy(config);
    }

    let seen = sampler.finish();
    let peak: Vec<&str> = seen.peak.iter().map(|(name, _)| name.as_str()).collect();
    let pool = CpuPool::default_threads();
    let bound = baseline + MOBILE_WORKER_THREADS + MOBILE_MAX_BLOCKING_THREADS + pool;
    eprintln!(
        "threads: baseline {baseline}, peak {} (bound {bound}: {MOBILE_WORKER_THREADS} workers, \
         {MOBILE_MAX_BLOCKING_THREADS} blocking, {pool} CPU pool): {peak:?}",
        peak.len()
    );
    assert!(peak.len() <= bound, "{} threads at the peak, over {bound}: {peak:?}", peak.len());
    // The runtime's threads and the pool's, by the names they were given.
    let named = |prefix: &str| peak.iter().filter(|n| n.starts_with(prefix)).count();
    assert!(named("dash-spv-worker") <= MOBILE_WORKER_THREADS + MOBILE_MAX_BLOCKING_THREADS);
    assert!(named(dash_spv::CPU_POOL_THREAD_NAME) <= pool, "{peak:?}");
    // One pool throughout: a pool built again (a restart, the reorg's) would show new threads.
    assert_eq!(seen.cpu_pool.len(), pool, "CPU-pool threads seen: {:?}", seen.cpu_pool);
}
