use std::collections::BTreeSet;
use std::ffi::{c_char, c_void, CStr, CString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use std::{env, fs, process, ptr, thread};

use dash_network::ffi::FFINetwork;
use dash_spv_ffi::client::{
    dash_spv_ffi_client_destroy, dash_spv_ffi_client_get_wallet_manager, dash_spv_ffi_client_new,
    dash_spv_ffi_client_run, dash_spv_ffi_client_stop, dash_spv_ffi_wallet_manager_free,
    FFIDashSpvClient, MOBILE_MAX_BLOCKING_THREADS, MOBILE_WORKER_THREADS,
};
use dash_spv_ffi::config::{
    dash_spv_ffi_config_add_peer, dash_spv_ffi_config_destroy, dash_spv_ffi_config_new,
    dash_spv_ffi_config_set_data_dir, dash_spv_ffi_config_set_masternode_sync_enabled,
    dash_spv_ffi_config_set_restrict_to_configured_peers, FFIClientConfig,
};
use dash_spv_ffi::{
    FFIEventCallbacks, FFIExtEventCallbacks, FFIProgressCallback, FFISyncEventCallbacks,
    FFISyncProgress,
};
use key_wallet_ffi::wallet_manager::wallet_manager_add_wallet_from_mnemonic;
use key_wallet_ffi::{FFIError, FFIWalletManager};

const SETTLE: Duration = Duration::from_secs(30);
const REORG_WAIT: Duration = Duration::from_secs(120);
const SYNC_TIMEOUT: Duration = Duration::from_secs(600);
const EXT_SETTER: &CStr = c"dash_spv_ffi_client_set_ext_event_callbacks";
const DEV_FEATURES: [&str; 2] =
    ["dash-spv/test-utils", "key-wallet-ffi/keep-finalized-transactions"];

#[cfg(all(target_os = "android", target_pointer_width = "32"))]
const RTLD_DEFAULT: *mut c_void = usize::MAX as *mut c_void;
#[cfg(not(all(target_os = "android", target_pointer_width = "32")))]
const RTLD_DEFAULT: *mut c_void = ptr::null_mut();

extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

type SetExt = unsafe extern "C" fn(*mut FFIDashSpvClient, *const FFIExtEventCallbacks) -> i32;

#[derive(Default)]
struct Events {
    synced_at: OnceLock<Instant>,
    marker: PathBuf,
    first_header: AtomicU32,
    last_header: AtomicU32,
    headers_seen: AtomicBool,
    filter_batches: AtomicU64,
    reorgs: AtomicU64,
}

extern "C" fn on_progress(progress: *const FFISyncProgress, user_data: *mut c_void) {
    let events = unsafe { &*(user_data as *const Events) };
    let Some(headers) = (unsafe { progress.as_ref() }).and_then(|p| unsafe { p.headers.as_ref() })
    else {
        return;
    };
    if !events.headers_seen.swap(true, Ordering::SeqCst) {
        events.first_header.store(headers.tip_height, Ordering::SeqCst);
    }
    events.last_header.fetch_max(headers.tip_height, Ordering::SeqCst);
}

extern "C" fn on_sync_complete(_header_tip: u32, _cycle: u32, user_data: *mut c_void) {
    let events = unsafe { &*(user_data as *const Events) };
    if events.synced_at.set(Instant::now()).is_ok() {
        if let Err(e) = fs::File::create(&events.marker) {
            eprintln!("mobile_probe: {}: {e}", events.marker.display());
        }
    }
}

extern "C" fn on_filters_stored(_start: u32, _end: u32, user_data: *mut c_void) {
    let events = unsafe { &*(user_data as *const Events) };
    events.filter_batches.fetch_add(1, Ordering::SeqCst);
}

extern "C" fn on_chain_reorganized(_fork: u32, _disc: u32, _conn: u32, user_data: *mut c_void) {
    let events = unsafe { &*(user_data as *const Events) };
    events.reorgs.fetch_add(1, Ordering::SeqCst);
}

fn thread_names() -> Vec<String> {
    let Ok(dir) = fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };
    dir.filter_map(|e| fs::read_to_string(e.ok()?.path().join("comm")).ok())
        .map(|name| name.trim().to_string())
        .collect()
}

#[derive(Default)]
struct Seen {
    peak: usize,
    names: BTreeSet<String>,
}

struct Sampler {
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Seen>>,
    handle: thread::JoinHandle<()>,
}

impl Sampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (s, p) = (stop.clone(), seen.clone());
        let handle = thread::Builder::new()
            .name("probe-sampler".into())
            .spawn(move || {
                while !s.load(Ordering::Relaxed) {
                    let now = thread_names();
                    let mut seen = p.lock().unwrap();
                    seen.peak = seen.peak.max(now.len());
                    seen.names.extend(now);
                    drop(seen);
                    thread::sleep(Duration::from_millis(1));
                }
            })
            .expect("the sampler");
        while seen.lock().unwrap().peak == 0 {
            thread::sleep(Duration::from_millis(1));
        }
        Self {
            stop,
            seen,
            handle,
        }
    }

    fn finish(self) -> Seen {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().expect("the sampler");
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

fn status_mib(key: &str) -> Option<f64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with(key))?;
    let kib: f64 = line[key.len()..].trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kib / 1024.0)
}

fn write_bytes() -> Option<u64> {
    let io = fs::read_to_string("/proc/self/io").ok()?;
    io.lines().find_map(|l| l.strip_prefix("write_bytes:")).and_then(|v| v.trim().parse().ok())
}

fn var(name: &str) -> String {
    env::var(name).unwrap_or_default()
}

fn fail(message: &str) -> ! {
    eprintln!("mobile_probe: {message}");
    process::exit(2);
}

fn network(name: &str) -> FFINetwork {
    match name {
        "regtest" => FFINetwork::Regtest,
        "testnet" => FFINetwork::Testnet,
        "mainnet" => FFINetwork::Mainnet,
        other => fail(&format!("PROBE_NETWORK={other:?} is not regtest, testnet or mainnet")),
    }
}

unsafe fn config(net: FFINetwork, dir: &Path, peer: Option<&str>) -> *mut FFIClientConfig {
    let config = dash_spv_ffi_config_new(net);
    let dir = CString::new(dir.to_str().expect("a UTF-8 data dir")).unwrap();
    assert_eq!(dash_spv_ffi_config_set_data_dir(config, dir.as_ptr()), 0, "the data dir");
    if matches!(net, FFINetwork::Regtest) {
        assert_eq!(dash_spv_ffi_config_set_masternode_sync_enabled(config, false), 0);
    }
    match peer {
        Some("") => {}
        Some(peer) => {
            let peer = CString::new(peer).unwrap();
            assert_eq!(dash_spv_ffi_config_add_peer(config, peer.as_ptr()), 0, "the peer");
            assert_eq!(dash_spv_ffi_config_set_restrict_to_configured_peers(config, true), 0);
        }
        None => {
            assert_eq!(dash_spv_ffi_config_set_restrict_to_configured_peers(config, true), 0);
        }
    }
    config
}

/// VmRSS after a client has run `SETTLE` with no peer to answer it.
unsafe fn idle_rss(net: FFINetwork, dir: &Path) -> Option<f64> {
    let config = config(net, dir, None);
    let client = dash_spv_ffi_client_new(config, FFIEventCallbacks::default());
    let rss = if client.is_null() || dash_spv_ffi_client_run(client) != 0 {
        None
    } else {
        thread::sleep(SETTLE);
        status_mib("VmRSS:")
    };
    if !client.is_null() {
        dash_spv_ffi_client_stop(client);
        dash_spv_ffi_client_destroy(client);
    }
    dash_spv_ffi_config_destroy(config);
    rss
}

struct Run {
    started: Instant,
    elapsed: Duration,
    written: Option<u64>,
    synced_rss: Option<f64>,
    ext_set: bool,
    error: Option<String>,
}

unsafe fn sync(
    events: &Events,
    net: FFINetwork,
    dir: &Path,
    peer: &str,
    mnemonic: &str,
    seconds: u64,
    min_reorgs: u64,
) -> Run {
    let ud = events as *const Events as *mut c_void;
    let config = config(net, dir, Some(peer));
    let callbacks = FFIEventCallbacks {
        sync: FFISyncEventCallbacks {
            on_filters_stored: Some(on_filters_stored),
            on_sync_complete: Some(on_sync_complete),
            user_data: ud,
            ..FFISyncEventCallbacks::default()
        },
        progress: FFIProgressCallback {
            on_progress: Some(on_progress),
            user_data: ud,
        },
        ..FFIEventCallbacks::default()
    };
    let client = dash_spv_ffi_client_new(config, callbacks);
    if client.is_null() {
        fail("dash_spv_ffi_client_new returned null");
    }
    let setter = dlsym(RTLD_DEFAULT, EXT_SETTER.as_ptr());
    let ext_set = !setter.is_null() && {
        let set_ext: SetExt = std::mem::transmute(setter);
        let ext = FFIExtEventCallbacks {
            user_data: ud,
            on_chain_reorganized: Some(on_chain_reorganized),
            ..FFIExtEventCallbacks::default()
        };
        set_ext(client, &ext) == 0
    };
    let wm = dash_spv_ffi_client_get_wallet_manager(client);
    let mut error = None;
    if !mnemonic.is_empty() {
        let mnemonic = CString::new(mnemonic).unwrap();
        let mut err = FFIError::default();
        if !wallet_manager_add_wallet_from_mnemonic(
            wm as *mut FFIWalletManager,
            mnemonic.as_ptr(),
            &mut err,
        ) {
            let message = (!err.message.is_null())
                .then(|| CStr::from_ptr(err.message).to_string_lossy().into_owned());
            error = Some(format!("the wallet: {message:?}"));
        }
    }

    let io_start = write_bytes();
    let started = Instant::now();
    if error.is_none() && dash_spv_ffi_client_run(client) != 0 {
        error = Some("dash_spv_ffi_client_run failed".into());
    }
    let fixed = (seconds > 0).then(|| started + Duration::from_secs(seconds));
    let mut synced_rss = None;
    while error.is_none() {
        thread::sleep(Duration::from_millis(50));
        let now = Instant::now();
        let synced = events.synced_at.get().copied();
        if synced_rss.is_none() && synced.is_some_and(|t| now >= t + SETTLE) {
            synced_rss = status_mib("VmRSS:");
        }
        if let Some(deadline) = fixed {
            if now >= deadline {
                break;
            }
            continue;
        }
        match synced {
            None if now >= started + SYNC_TIMEOUT => {
                error = Some(format!("no on_sync_complete within {SYNC_TIMEOUT:?}"));
            }
            None => {}
            Some(t) if now < t + SETTLE => {}
            Some(t)
                if min_reorgs == 0
                    || !ext_set
                    || events.reorgs.load(Ordering::SeqCst) >= min_reorgs
                    || now >= t + SETTLE + REORG_WAIT =>
            {
                break;
            }
            Some(_) => {}
        }
    }
    let elapsed = started.elapsed();
    let written = io_start.zip(write_bytes()).map(|(a, b)| b.saturating_sub(a));
    dash_spv_ffi_client_stop(client);
    dash_spv_ffi_wallet_manager_free(wm);
    dash_spv_ffi_client_destroy(client);
    dash_spv_ffi_config_destroy(config);
    Run {
        started,
        elapsed,
        written,
        synced_rss,
        ext_set,
        error,
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_opt<T: ToString>(v: Option<T>) -> String {
    v.map_or_else(|| "null".into(), |v| v.to_string())
}

fn json_names(names: &BTreeSet<String>) -> String {
    format!("[{}]", names.iter().map(|n| json_str(n)).collect::<Vec<_>>().join(", "))
}

fn build_features() -> BTreeSet<String> {
    let own = [
        ("log-file", cfg!(feature = "log-file")),
        ("env-filter", cfg!(feature = "env-filter")),
        ("blst-no-threads", cfg!(feature = "blst-no-threads")),
    ];
    let own = own.into_iter().filter(|(_, on)| *on).map(|(f, _)| format!("dash-spv-ffi/{f}"));
    DEV_FEATURES.iter().map(|f| f.to_string()).chain(own).collect()
}

fn round(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

pub fn main() {
    let json_path = env::var("PROBE_JSON").unwrap_or_else(|_| fail("PROBE_JSON is required"));
    let net_name = var("PROBE_NETWORK");
    let net = network(&net_name);
    let data_dir = PathBuf::from(var("PROBE_DATA_DIR"));
    if data_dir.as_os_str().is_empty() {
        fail("PROBE_DATA_DIR is required");
    }
    let peer = var("PROBE_PEER");
    let seconds: u64 = var("PROBE_SECONDS").parse().unwrap_or(0);
    let min_reorgs: u64 = var("PROBE_MIN_REORGS").parse().unwrap_or(0);
    let mode = env::var("PROBE_MODE").unwrap_or_else(|_| "emulator".into());
    let target = env::var("PROBE_TARGET")
        .unwrap_or_else(|_| format!("{}-{}", env::consts::ARCH, env::consts::OS));
    for sub in ["idle", "spv"] {
        fs::create_dir_all(data_dir.join(sub)).unwrap_or_else(|e| fail(&format!("{sub}: {e}")));
    }

    let sampler = Sampler::start();
    thread::sleep(Duration::from_millis(20));
    let baseline_names: BTreeSet<String> = thread_names().into_iter().collect();
    let baseline_threads = thread_names().len();
    let baseline_rss = status_mib("VmRSS:");

    let idle = unsafe { idle_rss(net, &data_dir.join("idle")) };

    let events = Events {
        marker: data_dir.join("synced"),
        ..Events::default()
    };
    let run = unsafe {
        sync(
            &events,
            net,
            &data_dir.join("spv"),
            &peer,
            &var("PROBE_MNEMONIC"),
            seconds,
            min_reorgs,
        )
    };
    let seen = sampler.finish();

    let elapsed = run.elapsed.as_secs_f64();
    let sync_s = events.synced_at.get().map_or(elapsed, |t| (*t - run.started).as_secs_f64());
    let headers = if events.headers_seen.load(Ordering::SeqCst) {
        events.last_header.load(Ordering::SeqCst) - events.first_header.load(Ordering::SeqCst)
    } else {
        0
    };
    let per_1000 =
        run.written.filter(|_| headers > 0).map(|w| round(w as f64 * 1000.0 / headers as f64));
    let reorgs = run.ext_set.then(|| events.reorgs.load(Ordering::SeqCst));
    let fields = [
        ("stamp", json_str(&var("PROBE_STAMP"))),
        ("mode", json_str(&mode)),
        ("target", json_str(&target)),
        ("network", json_str(&net_name)),
        ("build_features", json_names(&build_features())),
        ("baseline_threads", baseline_threads.to_string()),
        ("baseline_thread_names", json_names(&baseline_names)),
        (
            "thread_bound",
            (baseline_threads + MOBILE_WORKER_THREADS + MOBILE_MAX_BLOCKING_THREADS).to_string(),
        ),
        ("threads_peak", seen.peak.to_string()),
        ("thread_names", json_names(&seen.names)),
        ("baseline_rss_mib", json_opt(baseline_rss.map(round))),
        ("idle_rss_mib", json_opt(idle.map(round))),
        ("synced_rss_mib", json_opt(run.synced_rss.map(round))),
        ("peak_rss_mib", json_opt(status_mib("VmHWM:").map(round))),
        ("write_bytes", json_opt(run.written)),
        ("headers", headers.to_string()),
        (
            "headers_per_s",
            round(if sync_s > 0.0 {
                headers as f64 / sync_s
            } else {
                0.0
            })
            .to_string(),
        ),
        ("filter_batches", events.filter_batches.load(Ordering::SeqCst).to_string()),
        ("write_bytes_per_1000_headers", json_opt(per_1000)),
        ("reorgs", json_opt(reorgs)),
        ("elapsed_s", round(elapsed).to_string()),
        ("error", run.error.as_deref().map_or_else(|| "null".into(), json_str)),
    ];
    let body = fields.iter().map(|(k, v)| format!("  {}: {v}", json_str(k))).collect::<Vec<_>>();
    let text = format!("{{\n{}\n}}\n", body.join(",\n"));
    fs::write(&json_path, &text).unwrap_or_else(|e| fail(&format!("{json_path}: {e}")));
    print!("{text}");
    if run.error.is_some() {
        process::exit(1);
    }
}
