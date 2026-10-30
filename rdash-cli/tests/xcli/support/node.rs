//! A regtest dashd on a private datadir and free ports, driven through the stock `dash-cli`.
//!
//! Only the child this module spawned is ever stopped or killed, by its handle.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::{Tools, free_port};

/// How a node authenticates RPC clients.
#[derive(Clone, Debug)]
pub enum Auth {
    /// The `.cookie` dashd writes into the network datadir.
    Cookie,
    /// `-rpcuser` / `-rpcpassword` (dashd then writes no cookie).
    UserPass(&'static str, &'static str),
}

pub struct Node {
    pub name: String,
    pub datadir: PathBuf,
    pub rpcport: u16,
    pub p2pport: u16,
    pub auth: Auth,
    extra: Vec<String>,
    tools: Tools,
    home: PathBuf,
    child: Option<Child>,
}

impl Node {
    /// A node that is not running yet.
    pub fn new(tools: &Tools, root: &Path, name: &str, auth: Auth, extra: &[&str]) -> Node {
        let datadir = root.join(name);
        std::fs::create_dir_all(&datadir).unwrap();
        // Code that refuses group-writable directories needs 0700 test dirs.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&datadir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let home = root.join(format!("{name}-home"));
        std::fs::create_dir_all(&home).unwrap();
        Node {
            name: name.to_string(),
            datadir,
            rpcport: free_port(),
            p2pport: free_port(),
            auth,
            extra: extra.iter().map(|s| s.to_string()).collect(),
            tools: tools.clone(),
            home,
            child: None,
        }
    }

    fn daemon_args(&self) -> Vec<String> {
        let mut args = vec![
            "-regtest".to_string(),
            format!("-datadir={}", self.datadir.display()),
            format!("-port={}", self.p2pport),
            "-bind=127.0.0.1".to_string(),
            "-listenonion=0".to_string(),
            "-listen=1".to_string(),
            format!("-rpcport={}", self.rpcport),
            "-rpcbind=127.0.0.1".to_string(),
            "-rpcallowip=127.0.0.1".to_string(),
            "-server=1".to_string(),
            "-daemon=0".to_string(),
            "-printtoconsole=0".to_string(),
            "-dnsseed=0".to_string(),
            "-fixedseeds=0".to_string(),
            "-discover=0".to_string(),
            "-fallbackfee=0.00001".to_string(),
        ];
        if let Auth::UserPass(user, pass) = &self.auth {
            args.push(format!("-rpcuser={user}"));
            args.push(format!("-rpcpassword={pass}"));
        }
        args.extend(self.extra.iter().cloned());
        args
    }

    /// The arguments that point a `dash-cli` at this node.
    pub fn cli_args(&self) -> Vec<String> {
        let mut args = vec![
            "-regtest".to_string(),
            format!("-datadir={}", self.datadir.display()),
            format!("-rpcport={}", self.rpcport),
        ];
        if let Auth::UserPass(user, pass) = &self.auth {
            args.push(format!("-rpcuser={user}"));
            args.push(format!("-rpcpassword={pass}"));
        }
        args
    }

    /// Start the process; call [`Node::wait_ready`] before using it.
    pub fn spawn(&mut self) {
        assert!(self.child.is_none(), "{} already running", self.name);
        let child = Command::new(&self.tools.dashd)
            .args(self.daemon_args())
            .env("HOME", &self.home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start dashd: {e}"));
        self.child = Some(child);
    }

    /// Start and wait; retries on fresh ports if dashd cannot bind (another process took them).
    pub fn start(&mut self) {
        for attempt in 1..=5 {
            if self.start_once(attempt < 5) {
                return;
            }
        }
    }

    /// One attempt of [`Node::start`]: true once the node answers RPC. A dashd that exits within
    /// 60 s could not bind: with `retry`, fresh ports are picked for the next attempt and the
    /// answer is false; without, the attempt panics.
    pub fn start_once(&mut self, retry: bool) -> bool {
        self.spawn();
        let started = Instant::now();
        loop {
            let child = self.child.as_mut().unwrap();
            if child.try_wait().unwrap().is_some() {
                if retry && started.elapsed() < Duration::from_secs(60) {
                    eprintln!("dashd {} exited early; retrying on other ports", self.name);
                    self.child = None;
                    self.rpcport = free_port();
                    self.p2pport = free_port();
                    return false;
                }
                panic!(
                    "dashd {} keeps exiting; debug.log ends:\n{}",
                    self.name,
                    self.debug_log_tail()
                );
            }
            if self.try_rpc(&["getblockcount"]).is_some_and(|o| o.status.success()) {
                return true;
            }
            if started.elapsed() > Duration::from_secs(240) {
                panic!(
                    "dashd {} did not answer RPC within 240 s; debug.log ends:\n{}",
                    self.name,
                    self.debug_log_tail()
                );
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn debug_log_tail(&self) -> String {
        let log = std::fs::read_to_string(self.datadir.join("regtest").join("debug.log"))
            .unwrap_or_else(|e| format!("(no debug.log: {e})"));
        let lines: Vec<&str> = log.lines().collect();
        lines[lines.len().saturating_sub(25)..].join("\n")
    }

    fn try_rpc(&self, args: &[&str]) -> Option<std::process::Output> {
        Command::new(&self.tools.stock)
            .args(self.cli_args())
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .stdin(Stdio::null())
            .output()
            .ok()
    }

    /// Run an RPC through the stock `dash-cli`; panics unless it succeeds. Returns stdout
    /// without the trailing newline.
    pub fn rpc(&self, args: &[&str]) -> String {
        self.rpc_in(None, args)
    }

    /// Like [`Node::rpc`] on a named wallet.
    pub fn rpc_wallet(&self, wallet: &str, args: &[&str]) -> String {
        self.rpc_in(Some(wallet), args)
    }

    fn rpc_in(&self, wallet: Option<&str>, args: &[&str]) -> String {
        let mut cmd = Command::new(&self.tools.stock);
        cmd.args(self.cli_args());
        if let Some(w) = wallet {
            cmd.arg(format!("-rpcwallet={w}"));
        }
        let out = cmd
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}: dash-cli {:?} failed ({:?}): {}{}",
            self.name,
            args,
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string()
    }

    /// `stop`, then wait for the process; kill it if it does not exit.
    pub fn stop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = self.try_rpc(&["stop"]);
        let deadline = Instant::now() + Duration::from_secs(120);
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = self.try_rpc(&["stop"]);
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                if child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Copy a directory tree (the template datadir the mirror nodes start from).
pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}
