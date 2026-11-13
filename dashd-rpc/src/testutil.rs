//! A [`NodeControl`] for unit tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::server::{NodeControl, RpcServer};

pub const CATEGORIES: [&str; 3] = ["http", "net", "rpc"];

#[derive(Default)]
pub struct FakeControl {
    pub shutdown: AtomicBool,
    pub enabled: Mutex<Vec<&'static str>>,
    /// `Params().IsMockableChain()` is false.
    pub main_chain: AtomicBool,
    /// Each mocking call: `setmocktime` and `mockscheduler`'s.
    pub mocks: Mutex<Vec<String>>,
}

impl NodeControl for FakeControl {
    fn start_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
    fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
    fn now(&self) -> i64 {
        1_000_100
    }
    fn startup_time(&self) -> i64 {
        1_000_000
    }
    fn log_ips(&self) -> bool {
        false
    }
    fn log_path(&self) -> String {
        "/data/regtest/debug.log".into()
    }
    fn enable_log_category(&self, name: &str) -> bool {
        let mut enabled = self.enabled.lock().unwrap();
        if name == "all" || name == "1" {
            *enabled = CATEGORIES.to_vec();
            return true;
        }
        match CATEGORIES.iter().find(|c| **c == name) {
            Some(c) => {
                if !enabled.contains(c) {
                    enabled.push(c);
                }
                true
            }
            None => false,
        }
    }
    fn disable_log_category(&self, name: &str) -> bool {
        let mut enabled = self.enabled.lock().unwrap();
        if name == "all" || name == "1" {
            enabled.clear();
            return true;
        }
        let known = CATEGORIES.contains(&name);
        enabled.retain(|c| *c != name);
        known
    }
    fn log_categories(&self) -> Vec<(&'static str, bool)> {
        let enabled = self.enabled.lock().unwrap();
        CATEGORIES.iter().map(|c| (*c, enabled.contains(c))).collect()
    }
    fn package_name(&self) -> &'static str {
        "rdashd"
    }
    fn full_version(&self) -> &'static str {
        "v0.0.0"
    }
    fn is_mockable_chain(&self) -> bool {
        !self.main_chain.load(Ordering::SeqCst)
    }
    fn set_mocktime(&self, time: i64) {
        self.mocks.lock().unwrap().push(format!("mocktime {time}"));
    }
    fn mock_scheduler(&self, delta: Duration) {
        self.mocks.lock().unwrap().push(format!("scheduler {}", delta.as_secs()));
    }
}

/// A started server with the control commands, out of warmup.
pub fn server() -> (Arc<RpcServer>, Arc<FakeControl>) {
    let control = Arc::new(FakeControl::default());
    let server = Arc::new(RpcServer::new(Arc::clone(&control) as _));
    crate::commands::register_control_commands(&server);
    server.start();
    server.set_warmup_finished();
    (server, control)
}

/// Dash Core's options parsed from `argv` on regtest, with a data directory that exists.
pub struct TestArgs {
    pub args: dash_args::ArgsManager,
    pub dir: tempfile::TempDir,
}

pub fn test_args(argv: &[&str]) -> TestArgs {
    let dir = tempfile::tempdir().unwrap();
    let args = dash_args::ArgsManager::new();
    dash_args::Program::Dashd.setup_args(&args);
    let datadir = format!("-datadir={}", dir.path().display());
    let mut full = vec!["dashd", datadir.as_str(), "-regtest"];
    full.extend_from_slice(argv);
    args.parse_parameters(&full).unwrap();
    args.select_base_params(&args.get_chain_name().unwrap()).unwrap();
    std::fs::create_dir_all(args.get_data_dir_net().unwrap()).unwrap();
    TestArgs {
        args,
        dir,
    }
}
