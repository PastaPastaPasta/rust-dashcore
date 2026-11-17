//! SB18 `spv_fixed_threads`: the threads dashd-node itself runs in SPV mode, around a host that
//! starts none, are [`SPV_FIXED_THREADS`]. It has its own `main` (`harness = false`), so no
//! libtest thread is counted.

use std::fs::{self, DirEntry};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dashd_node::init::{init_config, setup_server_args};
use dashd_node::logging::Logger;
use dashd_node::spv_mode::{SPV_FIXED_THREADS, SpvHost, SpvStart};
use dashd_node::ui::Ui;
use dashd_node::{InitFailure, Node};
use dashd_rpc::server::RpcServer;

/// A host that starts nothing.
struct Idle;

impl SpvHost for Idle {
    fn start(&mut self, _: SpvStart<'_>) -> Result<(), InitFailure> {
        Ok(())
    }

    fn register_rpc(&self, _: &Arc<RpcServer>) {}

    fn interrupt(&self) {}

    fn shutdown(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// The OS name of the thread whose task directory is `entry`.
#[cfg(target_os = "linux")]
fn comm(entry: &DirEntry) -> String {
    fs::read_to_string(entry.path().join("comm")).unwrap().trim_end().to_owned()
}

/// The OS names of every thread of this process but the main one, sorted.
///
/// A spawned thread names itself once it first runs, and until then carries the main thread's
/// name, so this waits up to 2 s for no other thread to carry it.
#[cfg(target_os = "linux")]
fn thread_names() -> Vec<String> {
    let pid = std::process::id().to_string();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let mut main = String::new();
        let mut names = Vec::new();
        for entry in fs::read_dir("/proc/self/task").unwrap().map(|e| e.unwrap()) {
            if entry.file_name() == pid.as_str() {
                main = comm(&entry);
            } else {
                names.push(comm(&entry));
            }
        }
        if !names.contains(&main) || Instant::now() >= deadline {
            names.sort();
            return names;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn main() {
    let dir = tempfile::tempdir().unwrap();
    let args = ArgsManager::new();
    setup_server_args(&args);
    let datadir = format!("-datadir={}", dir.path().display());
    let port = format!("-rpcport={}", node_test_ports::free_port());
    let argv =
        ["rdashd", &datadir, "-regtest", "-printtoconsole=0", "-listen=0", "-server=1", &port];
    args.parse_parameters(&[&argv[..], &["-rpcthreads=2"]].concat()).unwrap();
    let params = init_config(&args).unwrap();
    let ui = Arc::new(Ui::new(Arc::new(Logger::new())));
    let mut node = Node::new(Arc::new(args), ui, params);
    assert_eq!(thread_names(), Vec::<String>::new(), "a thread before init");
    assert!(node.app_init_early());
    assert!(node.app_init_main_spv(Box::new(Idle)));
    // Kernel thread names are cut to TASK_COMM_LEN - 1 bytes.
    let mut want: Vec<String> =
        SPV_FIXED_THREADS.iter().map(|n| n.chars().take(15).collect()).collect();
    want.sort();
    assert_eq!(thread_names(), want);
    node.interrupt();
    node.shutdown();
    assert_eq!(node.exit_status(), 0);
    println!("spv_fixed_threads: ok ({} threads)", want.len());
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("spv_fixed_threads: skipped, /proc/self/task is Linux's");
}
