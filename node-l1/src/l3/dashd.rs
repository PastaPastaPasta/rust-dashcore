//! One regtest dashd per leg (plan §3.1): its process on a private datadir and free ports, and a
//! JSON-RPC client over plain HTTP on loopback (std only: dash-testkit would pull dash-spv in).

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The RPC credentials: the node listens on loopback only.
const RPC_USER: &str = "l3";
const RPC_PASSWORD: &str = "l3";
/// `RPC_IN_WARMUP`: the node answers before its load is done.
const RPC_IN_WARMUP: i64 = -28;
/// How long a start may take before the leg gives up.
const START_TIMEOUT: Duration = Duration::from_secs(120);
/// How long `stop` may take before the process is killed.
const STOP_TIMEOUT: Duration = Duration::from_secs(60);
/// One RPC's read and write timeout.
const RPC_TIMEOUT: Duration = Duration::from_secs(300);
/// Starts on new ports after a node that exited during init (another process took its port:
/// node-test-ports' contract).
const START_ATTEMPTS: u32 = 3;

/// A JSON-RPC error: Core's code and message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// A JSON-RPC client for one node.
#[derive(Clone, Debug)]
pub struct Rpc {
    addr: SocketAddr,
    auth: String,
}

impl Rpc {
    pub fn new(port: u16, user: &str, password: &str) -> Rpc {
        Rpc {
            addr: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            auth: base64(format!("{user}:{password}").as_bytes()),
        }
    }

    /// `method(params)`: `Ok(Ok(result))`, `Ok(Err(error))` for Core's JSON-RPC error, and `Err`
    /// for anything else (no connection, a reply that is not JSON-RPC).
    pub fn call(&self, method: &str, params: Value) -> Result<Result<Value, RpcError>, String> {
        let body = json!({"jsonrpc": "1.0", "id": "l3", "method": method, "params": params});
        let body = body.to_string();
        let mut stream = TcpStream::connect_timeout(&self.addr, RPC_TIMEOUT)
            .map_err(|e| format!("{method}: connect {}: {e}", self.addr))?;
        stream.set_read_timeout(Some(RPC_TIMEOUT)).map_err(|e| format!("{method}: {e}"))?;
        stream.set_write_timeout(Some(RPC_TIMEOUT)).map_err(|e| format!("{method}: {e}"))?;
        let request = format!(
            "POST / HTTP/1.1\r\nHost: {}\r\nAuthorization: Basic {}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.addr,
            self.auth,
            body.len()
        );
        stream.write_all(request.as_bytes()).map_err(|e| format!("{method}: write: {e}"))?;
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).map_err(|e| format!("{method}: read: {e}"))?;
        parse_reply(method, &reply)
    }

    /// [`call`](Self::call), with Core's error as an `Err` too.
    pub fn ok(&self, method: &str, params: Value) -> Result<Value, String> {
        self.call(method, params)?
            .map_err(|e| format!("{method}: Core's error {}: {}", e.code, e.message))
    }
}

/// An HTTP reply's JSON-RPC result. Core answers an RPC error with a non-200 status and the
/// error in the body; a body without one is the transport's failure.
fn parse_reply(method: &str, reply: &[u8]) -> Result<Result<Value, RpcError>, String> {
    let text = String::from_utf8_lossy(reply);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| format!("{method}: a reply without headers: {text:?}"))?;
    let status = head.lines().next().unwrap_or_default();
    let value: Value = serde_json::from_str(body)
        .map_err(|e| format!("{method}: {status}: a body that is not JSON ({e}): {body:?}"))?;
    match value.get("error") {
        Some(Value::Null) | None => {
            value.get("result").cloned().map(Ok).ok_or_else(|| format!("{method}: no result"))
        }
        Some(error) => {
            let code = error.get("code").and_then(Value::as_i64);
            let message = error.get("message").and_then(Value::as_str);
            match (code, message) {
                (Some(code), Some(message)) => Ok(Err(RpcError {
                    code,
                    message: message.to_owned(),
                })),
                _ => Err(format!("{method}: {status}: a malformed error {error}")),
            }
        }
    }
}

/// RFC 4648 base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n =
            chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `mnsync status`'s `IsSynced`; a reply without it, or not a boolean, is an error.
pub fn is_synced(status: &Value) -> Result<bool, String> {
    match status.get("IsSynced") {
        Some(Value::Bool(synced)) => Ok(*synced),
        Some(other) => Err(format!("mnsync status: IsSynced is {other}")),
        None => Err(format!("mnsync status without IsSynced: {status}")),
    }
}

/// A running regtest dashd: killed on drop if [`stop`](Process::stop) did not end it.
pub struct Process {
    child: Child,
    datadir: PathBuf,
    argv: Vec<String>,
    rpc: Rpc,
}

impl Process {
    /// `binary` on `datadir` (an existing 0700 directory) with
    /// `-regtest -datadir -port -rpcport -listen=0 -connect=0 -dnsseed=0 -server=1 -rpcuser
    /// -rpcpassword -printtoconsole=0 -mocktime=<mocktime>`, then `args` (the leg's chain args and
    /// `-par`), once its RPC answers `getblockchaininfo`. A node that exits during init is started
    /// again on new ports, up to three times.
    pub fn start(
        binary: &Path,
        datadir: &Path,
        args: &[String],
        mocktime: i64,
    ) -> Result<Process, String> {
        let mut last = String::new();
        for _ in 0..START_ATTEMPTS {
            let port = node_test_ports::free_port();
            let rpc_port = node_test_ports::free_port();
            let mut argv = vec![
                "-regtest".to_owned(),
                format!("-datadir={}", datadir.display()),
                format!("-port={port}"),
                format!("-rpcport={rpc_port}"),
                "-listen=0".to_owned(),
                "-connect=0".to_owned(),
                "-dnsseed=0".to_owned(),
                "-server=1".to_owned(),
                format!("-rpcuser={RPC_USER}"),
                format!("-rpcpassword={RPC_PASSWORD}"),
                "-printtoconsole=0".to_owned(),
                format!("-mocktime={mocktime}"),
            ];
            argv.extend_from_slice(args);
            let stderr = std::fs::File::create(datadir.join("stderr.log"))
                .map_err(|e| format!("{}: {e}", datadir.display()))?;
            let child = Command::new(binary)
                .args(&argv)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(stderr)
                .spawn()
                .map_err(|e| format!("{}: {e}", binary.display()))?;
            let mut process = Process {
                child,
                datadir: datadir.to_owned(),
                argv,
                rpc: Rpc::new(rpc_port, RPC_USER, RPC_PASSWORD),
            };
            match process.ready() {
                Ok(()) => return Ok(process),
                Err(Ready::Exited(e)) => last = e,
                Err(Ready::Failed(e)) => return Err(e),
            }
        }
        Err(format!("dashd exited during init {START_ATTEMPTS} times; last: {last}"))
    }

    /// Wait for the RPC: connection refusals and warmup errors are retried until
    /// [`START_TIMEOUT`].
    fn ready(&mut self) -> Result<(), Ready> {
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().map_err(|e| Ready::Failed(e.to_string()))? {
                return Err(Ready::Exited(format!("{status}; {}", self.stderr_tail())));
            }
            match self.rpc.call("getblockchaininfo", json!([])) {
                Ok(Ok(_)) => return Ok(()),
                Ok(Err(e)) if e.code == RPC_IN_WARMUP => {}
                Ok(Err(e)) => {
                    return Err(Ready::Failed(format!(
                        "getblockchaininfo: {} {}",
                        e.code, e.message
                    )));
                }
                Err(_) => {}
            }
            if start.elapsed() > START_TIMEOUT {
                return Err(Ready::Failed(format!(
                    "dashd's RPC did not answer in {START_TIMEOUT:?}; {}",
                    self.stderr_tail()
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn stderr_tail(&self) -> String {
        let text = std::fs::read_to_string(self.datadir.join("stderr.log")).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        format!("stderr: {:?}", &lines[lines.len().saturating_sub(5)..])
    }

    pub fn rpc(&self) -> &Rpc {
        &self.rpc
    }

    /// The node's arguments, as the report records them.
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// `stop`, then the exit, within [`STOP_TIMEOUT`]; past it the process is killed and the
    /// stop is an error.
    pub fn stop(mut self) -> Result<(), String> {
        self.rpc.ok("stop", json!([]))?;
        let start = Instant::now();
        while start.elapsed() < STOP_TIMEOUT {
            match self.child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => return Err(format!("dashd exited with {status}")),
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => return Err(e.to_string()),
            }
        }
        Err(format!("dashd did not stop in {STOP_TIMEOUT:?}"))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Why a start's wait ended without an answering node.
enum Ready {
    /// The node exited: it is started again on new ports.
    Exited(String),
    /// Anything else: the leg's harness error.
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_is_rfc4648s() {
        for (plain, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("l3:l3", "bDM6bDM="),
        ] {
            assert_eq!(base64(plain.as_bytes()), coded, "{plain}");
        }
    }

    /// Core's replies: a result, an error in a 500, and the transport's failures.
    #[test]
    fn replies_are_results_errors_or_failures() {
        let ok = b"HTTP/1.1 200 OK\r\nContent-Length: 40\r\n\r\n{\"result\":7,\"error\":null,\"id\":\"l3\"}";
        assert_eq!(parse_reply("m", ok), Ok(Ok(json!(7))));
        let err = b"HTTP/1.1 500 Internal Server Error\r\n\r\n\
            {\"result\":null,\"error\":{\"code\":-22,\"message\":\"Block decode failed\"},\"id\":\"l3\"}";
        assert_eq!(
            parse_reply("m", err),
            Ok(Err(RpcError {
                code: -22,
                message: "Block decode failed".to_owned()
            }))
        );
        assert!(parse_reply("m", b"HTTP/1.1 401 Unauthorized\r\n\r\n").is_err());
        assert!(parse_reply("m", b"garbage").is_err());
        let malformed = b"HTTP/1.1 500 x\r\n\r\n{\"result\":null,\"error\":{\"code\":\"x\"}}";
        assert!(parse_reply("m", malformed).is_err());
    }

    #[test]
    fn mnsync_status_says_synced_or_is_an_error() {
        assert_eq!(is_synced(&json!({"IsSynced": false, "AssetID": 0})), Ok(false));
        assert_eq!(is_synced(&json!({"IsSynced": true})), Ok(true));
        assert!(is_synced(&json!({"AssetID": 0})).is_err());
        assert!(is_synced(&json!({"IsSynced": "no"})).is_err());
    }
}
