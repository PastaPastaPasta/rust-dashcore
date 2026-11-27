//! The RPC a test drives a node with, as a small trait both node implementations satisfy.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use dashcore::BlockHash;
use dashcore_rpc::jsonrpc::{self, simple_http::SimpleHttpTransport};
use dashcore_rpc::{Auth, Client, RpcApi};
use serde_json::Value;

pub type RpcError = Box<dyn std::error::Error + Send + Sync>;

/// A JSON-RPC connection to a node: `call`, plus the calls the harness itself makes.
///
/// Implemented for dashcore-rpc's [`Client`], which speaks Core's JSON-RPC over HTTP with cookie
/// authentication. That serves dashd, and rdashd once its RPC server lands (RP0).
pub trait NodeRpc {
    fn call(&self, method: &str, params: &[Value]) -> Result<Value, RpcError>;

    fn get_block_count(&self) -> Result<u64, RpcError> {
        Ok(serde_json::from_value(self.call("getblockcount", &[])?)?)
    }

    fn get_best_block_hash(&self) -> Result<BlockHash, RpcError> {
        Ok(serde_json::from_value(self.call("getbestblockhash", &[])?)?)
    }

    /// `getblock <hash>`'s `time`.
    fn get_block_time(&self, hash: &BlockHash) -> Result<i64, RpcError> {
        let block = self.call("getblock", &[hash.to_string().into()])?;
        block["time"].as_i64().ok_or_else(|| format!("getblock without a time: {block}").into())
    }

    fn set_mocktime(&self, time: i64) -> Result<(), RpcError> {
        self.call("setmocktime", &[time.into()]).map(drop)
    }

    /// `addnode <addr> <command>` (`add`, `remove` or `onetry`).
    fn add_node(&self, addr: SocketAddr, command: &str) -> Result<(), RpcError> {
        self.call("addnode", &[addr.to_string().into(), command.into()]).map(drop)
    }

    /// Every peer in `getpeerinfo`.
    fn peers(&self) -> Result<Vec<PeerInfo>, RpcError> {
        let peers = self.call("getpeerinfo", &[])?;
        let peers = peers.as_array().ok_or("getpeerinfo did not return an array")?;
        peers
            .iter()
            .map(|peer| {
                Ok(PeerInfo {
                    addr: peer["addr"].as_str().ok_or("a peer without an addr")?.to_string(),
                    version: peer["version"].as_u64().unwrap_or(0),
                    inbound: peer["inbound"].as_bool().unwrap_or(false),
                })
            })
            .collect()
    }
}

/// A `getpeerinfo` entry, as far as the harness reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerInfo {
    pub addr: String,
    /// The protocol version the peer sent; 0 until its `version` message arrives.
    pub version: u64,
    pub inbound: bool,
}

impl PeerInfo {
    /// Its `version` message has arrived: a peer, not just a TCP connection.
    pub fn handshaken(&self) -> bool {
        self.version != 0
    }
}

impl NodeRpc for Client {
    fn call(&self, method: &str, params: &[Value]) -> Result<Value, RpcError> {
        Ok(RpcApi::call(self, method, params)?)
    }
}

/// Where a node serves RPC: a loopback port, with the cookie it writes in its network directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcEndpoint {
    pub port: u16,
    pub cookie: PathBuf,
}

impl RpcEndpoint {
    /// The endpoint of a regtest node in `datadir` serving on `port`.
    pub fn regtest(datadir: &std::path::Path, port: u16) -> Self {
        Self {
            port,
            cookie: datadir.join("regtest").join(".cookie"),
        }
    }

    /// The node arguments that serve this endpoint, as dash-spv's harness gives dashd.
    pub fn args(&self) -> Vec<String> {
        vec![
            "-server=1".to_string(),
            format!("-rpcport={}", self.port),
            "-rpcbind=127.0.0.1".to_string(),
            "-rpcallowip=127.0.0.1".to_string(),
        ]
    }

    /// A client; fails until the node has written its cookie.
    pub fn client(&self) -> Result<Client, RpcError> {
        if !self.cookie.exists() {
            return Err(format!("no RPC cookie at {}", self.cookie.display()).into());
        }
        Ok(Client::new(
            &format!("http://127.0.0.1:{}", self.port),
            Auth::CookieFile(self.cookie.clone()),
        )?)
    }

    /// [`Self::client`] waiting up to `timeout` for each reply, where jsonrpc waits 15 s.
    pub fn client_with_timeout(&self, timeout: Duration) -> Result<Client, RpcError> {
        if !self.cookie.exists() {
            return Err(format!("no RPC cookie at {}", self.cookie.display()).into());
        }
        let (user, pass) = Auth::CookieFile(self.cookie.clone()).get_user_pass()?;
        let transport = SimpleHttpTransport::builder()
            .url(&format!("http://127.0.0.1:{}", self.port))?
            .timeout(timeout)
            .auth(user.unwrap_or_default(), pass)
            .build();
        Ok(Client::from_jsonrpc(jsonrpc::Client::with_transport(transport)))
    }
}
