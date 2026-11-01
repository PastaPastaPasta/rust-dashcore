//! N dashd and M rdashd regtest nodes as one network.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dash_spv::test_utils::{DashCoreConfig, DashCoreNode, NodeImpl, find_available_port};

use crate::PATIENCE;
use crate::dir::TestDir;
use crate::mocktime::MockTime;
use crate::rpc::{NodeRpc, PeerInfo, RpcEndpoint, RpcError};
use crate::rust_node::{
    RustNode, RustNodeConfig, StartError, StopError, Stopped, assert_args_allowed,
};

/// A node of a [`MixedNetwork`]: the i-th dashd or the i-th rdashd.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeId {
    Dashd(usize),
    Rust(usize),
}

/// Builds a [`MixedNetwork`].
///
/// An edge `a → b` makes `a` connect to `b`:
/// - from a dashd, with `addnode <b> onetry` once every node is up, waiting until `b` is a peer;
/// - from an rdashd, with `-connect=<b>` at start (rdashd: P2a-net).
///
/// An edge into an rdashd makes it listen (`-listen=1 -port`; rdashd: P2b). Without edges an
/// rdashd gets no P2P or RPC arguments, so it starts today.
#[derive(Clone, Debug, Default)]
pub struct MixedNetworkBuilder {
    dashd: usize,
    rust: usize,
    edges: Vec<(NodeId, NodeId)>,
    mocktime: Option<MockTime>,
    rust_rpc: bool,
    dashd_args: Vec<String>,
    rust_args: Vec<String>,
}

impl MixedNetworkBuilder {
    /// `count` dashd nodes, from `DASHD_PATH` whatever `DASHD_IMPL` says.
    pub fn dashd(mut self, count: usize) -> Self {
        self.dashd = count;
        self
    }

    /// `count` rdashd nodes.
    pub fn rdashd(mut self, count: usize) -> Self {
        self.rust = count;
        self
    }

    pub fn connect(mut self, from: NodeId, to: NodeId) -> Self {
        self.edges.push((from, to));
        self
    }

    /// Every node starts at `time`.
    pub fn mocktime(mut self, time: MockTime) -> Self {
        self.mocktime = Some(time);
        self
    }

    /// The rdashd nodes serve RPC (rdashd: RP0).
    pub fn rdashd_rpc(mut self, on: bool) -> Self {
        self.rust_rpc = on;
        self
    }

    /// Panics on an argument [`RustNodeConfig::with_args`] does not allow: the same rules hold
    /// for dashd.
    pub fn dashd_args<S: Into<String>>(mut self, args: impl IntoIterator<Item = S>) -> Self {
        let args: Vec<String> = args.into_iter().map(Into::into).collect();
        assert_args_allowed(&args);
        self.dashd_args.extend(args);
        self
    }

    /// Panics on an argument [`RustNodeConfig::with_args`] does not allow.
    pub fn rdashd_args<S: Into<String>>(mut self, args: impl IntoIterator<Item = S>) -> Self {
        let args: Vec<String> = args.into_iter().map(Into::into).collect();
        assert_args_allowed(&args);
        self.rust_args.extend(args);
        self
    }

    /// Starts every node, makes the dashd edges, and returns once every edge is a peer
    /// connection: each dashd edge's peer has sent its `version`, and each dashd has as many
    /// handshaken inbound peers as edges lead into it (rdashd edges included).
    ///
    /// A dashd that fails to start panics, as in dash-spv's harness, and the network directory
    /// is retained as for any panicking test. An rdashd that fails returns a
    /// [`NetworkStartError`], after every node is stopped and the directory is retained under
    /// `DASHD_TEST_RETAIN_DIR` whatever the test does with the error. Panics on an edge naming a
    /// node the builder does not have, a node itself, or an edge given twice.
    pub async fn start(self) -> Result<MixedNetwork, NetworkStartError> {
        let known = |id: NodeId| match id {
            NodeId::Dashd(i) => i < self.dashd,
            NodeId::Rust(i) => i < self.rust,
        };
        for &(from, to) in &self.edges {
            assert!(from != to, "edge from {from:?} to itself");
            let copies = self.edges.iter().filter(|&&e| e == (from, to)).count();
            assert!(copies == 1, "edge from {from:?} to {to:?} given {copies} times");
            for id in [from, to] {
                assert!(known(id), "edge names {id:?}, which is not in the network");
            }
        }
        let io = |error| NetworkStartError {
            error: StartError::Io(error),
            retained: None,
        };
        let dir = TestDir::new("mixed-network").map_err(io)?;
        let dashd_extra: Vec<String> = self
            .mocktime
            .map(MockTime::arg)
            .into_iter()
            .chain(self.dashd_args.iter().cloned())
            .collect();

        let dashd_path = (self.dashd > 0)
            .then(|| NodeImpl::Dashd.binary().unwrap_or_else(|missing| panic!("{missing}")));
        let dashd_configs = (0..self.dashd)
            .map(|i| {
                // A fresh regtest chain without a wallet.
                let config = DashCoreConfig::new(
                    dashd_path.clone().expect("dashd nodes"),
                    dir.subdir(&format!("dashd-{i}"))?,
                    find_available_port(),
                    find_available_port(),
                );
                Ok(config.with_extra_args(dashd_extra.clone()).with_start_timeout(PATIENCE))
            })
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(io)?;
        let mut network = MixedNetwork {
            dashd: Vec::with_capacity(self.dashd),
            rust: Vec::with_capacity(self.rust),
            mocktime: self.mocktime,
            dir,
        };
        for config in dashd_configs {
            let mut node = DashCoreNode::with_config(config);
            node.start().await;
            network.dashd.push(node);
        }

        // Ports for rdashd only now, just before it binds them, not across the dashd starts.
        let rust_ports: Vec<u16> = (0..self.rust).map(|_| find_available_port()).collect();
        let addr = |id: NodeId| match id {
            NodeId::Dashd(i) => crate::loopback(network.dashd[i].p2p_port()),
            NodeId::Rust(i) => crate::loopback(rust_ports[i]),
        };
        let mut rust_configs = Vec::with_capacity(self.rust);
        for (i, &port) in rust_ports.iter().enumerate() {
            let datadir = network.dir.subdir(&format!("rdashd-{i}")).map_err(io)?;
            let mut config = RustNodeConfig::new(datadir)
                .with_args(self.rust_args.iter().cloned())
                .with_mocktime(self.mocktime);
            if self.edges.iter().any(|&(_, to)| to == NodeId::Rust(i)) {
                config = config.with_listen(port);
            }
            for &(_, to) in self.edges.iter().filter(|&&(from, _)| from == NodeId::Rust(i)) {
                config = config.with_connect(addr(to));
            }
            if self.rust_rpc {
                config = config.with_rpc(find_available_port());
            }
            rust_configs.push(config);
        }
        let dashd_edges: Vec<(usize, SocketAddr)> = self
            .edges
            .iter()
            .filter_map(|&(from, to)| match from {
                NodeId::Dashd(i) => Some((i, addr(to))),
                NodeId::Rust(_) => None,
            })
            .collect();

        for config in rust_configs {
            match RustNode::start(config).await {
                Ok(node) => network.rust.push(node),
                Err(error) => {
                    let retained = network.into_dir().retain();
                    return Err(NetworkStartError {
                        error,
                        retained,
                    });
                }
            }
        }
        let deadline = Instant::now() + PATIENCE;
        for (i, peer) in dashd_edges {
            if let Err(e) = network.dashd_connect(i, peer, deadline).await {
                panic!("{e}");
            }
        }
        // Edges into a dashd, from rdashd's `-connect` too: wait until it has that many peers.
        for j in 0..self.dashd {
            let expected = self.edges.iter().filter(|&&(_, to)| to == NodeId::Dashd(j)).count();
            if let Err(e) = network.dashd_inbound(j, expected, deadline).await {
                panic!("{e}");
            }
        }
        Ok(network)
    }
}

/// An rdashd of a [`MixedNetwork`] did not start.
#[derive(Debug)]
pub struct NetworkStartError {
    pub error: StartError,
    /// Where the network directory was retained, with every node stopped: set when
    /// `DASHD_TEST_RETAIN_DIR` is.
    pub retained: Option<PathBuf>,
}

impl std::fmt::Display for NetworkStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)?;
        if let Some(dir) = &self.retained {
            write!(f, " (network directory retained at {})", dir.display())?;
        }
        Ok(())
    }
}

impl std::error::Error for NetworkStartError {}

/// Running dashd and rdashd regtest nodes in one test directory.
///
/// Dropping it kills every node, then removes the directory, retaining it first under
/// `DASHD_TEST_RETAIN_DIR` when the test panics (`<test>/mixed-network/{dashd,rdashd}-<i>`).
pub struct MixedNetwork {
    dashd: Vec<DashCoreNode>,
    rust: Vec<RustNode>,
    mocktime: Option<MockTime>,
    // Last: dropped after the nodes. rdashd has exited by then; dashd is only signalled
    // (dash-spv's Drop), so its retained log may lack the last lines.
    dir: TestDir,
}

impl MixedNetwork {
    pub fn builder() -> MixedNetworkBuilder {
        MixedNetworkBuilder::default()
    }

    pub fn dashd(&self, i: usize) -> &DashCoreNode {
        &self.dashd[i]
    }

    pub fn rust(&self, i: usize) -> &RustNode {
        &self.rust[i]
    }

    pub fn dir(&self) -> &std::path::Path {
        self.dir.path()
    }

    /// The node's P2P address, if it listens. Every dashd listens.
    pub fn p2p_addr(&self, id: NodeId) -> Option<SocketAddr> {
        match id {
            NodeId::Dashd(i) => Some(crate::loopback(self.dashd[i].p2p_port())),
            NodeId::Rust(i) => self.rust[i].p2p_addr(),
        }
    }

    /// An RPC client for the node.
    pub fn rpc(&self, id: NodeId) -> Result<dashcore_rpc::Client, RpcError> {
        match id {
            NodeId::Dashd(i) => {
                let node = &self.dashd[i];
                RpcEndpoint::regtest(node.datadir(), node.rpc_port()).client()
            }
            NodeId::Rust(i) => self.rust[i].rpc(),
        }
    }

    pub fn ids(&self) -> impl Iterator<Item = NodeId> + use<> {
        (0..self.dashd.len()).map(NodeId::Dashd).chain((0..self.rust.len()).map(NodeId::Rust))
    }

    /// The network's mock time, if it has one.
    pub fn mocktime(&self) -> Option<MockTime> {
        self.mocktime
    }

    /// Moves the mock clock by `secs` on every node (`setmocktime`, so rdashd needs RP0).
    ///
    /// Every node is tried. The network's [`mocktime`](Self::mocktime) moves even if some fail,
    /// and the error names the nodes still on the old time, so a test can retry them (with
    /// [`MockTime::apply`]) or fail knowing the clocks are split.
    pub fn advance_mocktime(&mut self, secs: i64) -> Result<i64, RpcError> {
        let mut time = self.mocktime.ok_or("the network has no mock time")?;
        time.advance(secs);
        self.mocktime = Some(time);
        let failed: Vec<String> = self
            .ids()
            .filter_map(|id| {
                let moved = self.rpc(id).and_then(|rpc| rpc.set_mocktime(time.time()));
                moved.err().map(|e| format!("{id:?}: {e}"))
            })
            .collect();
        if failed.is_empty() {
            Ok(time.time())
        } else {
            Err(format!("setmocktime {} failed on {}", time.time(), failed.join("; ")).into())
        }
    }

    /// Stops every rdashd as Core stops, returning how each stopped. The network (its dashd
    /// nodes and directory) lives on until dropped, so a failed assertion on the result still
    /// retains the logs.
    pub async fn stop_rdashd(&mut self) -> Vec<Result<Stopped, StopError>> {
        let mut stopped = Vec::with_capacity(self.rust.len());
        for node in std::mem::take(&mut self.rust) {
            stopped.push(node.stop().await);
        }
        stopped
    }

    /// The directory, once every node is gone.
    fn into_dir(mut self) -> TestDir {
        self.rust.clear();
        self.dashd.clear();
        let Self {
            dir,
            ..
        } = self;
        dir
    }

    /// Makes dashd `i` connect to `peer` (`addnode onetry`, again every few seconds), until
    /// `peer` is in its `getpeerinfo` with a version: handshaken, not just accepted. RPC errors
    /// (a slow node) are retried until `deadline`.
    async fn dashd_connect(
        &self,
        i: usize,
        peer: SocketAddr,
        deadline: Instant,
    ) -> Result<(), String> {
        let wanted = peer.to_string();
        loop {
            let addnode = self.rpc(NodeId::Dashd(i)).and_then(|rpc| rpc.add_node(peer, "onetry"));
            let retry = deadline.min(Instant::now() + Duration::from_secs(5));
            let connected =
                |peers: &[PeerInfo]| peers.iter().any(|p| p.addr == wanted && p.handshaken());
            match self.poll_peers(i, retry, connected).await {
                Ok(()) => return Ok(()),
                Err(seen) if Instant::now() > deadline => {
                    let addnode = addnode.err().map(|e| format!(" (addnode failed: {e})"));
                    return Err(format!(
                        "dashd-{i} did not connect to {peer}{}; last saw {seen}",
                        addnode.unwrap_or_default()
                    ));
                }
                Err(_) => {}
            }
        }
    }

    /// Waits until dashd `j` has at least `expected` handshaken inbound peers.
    async fn dashd_inbound(
        &self,
        j: usize,
        expected: usize,
        deadline: Instant,
    ) -> Result<(), String> {
        let inbound = |peers: &[PeerInfo]| {
            peers.iter().filter(|p| p.inbound && p.handshaken()).count() >= expected
        };
        self.poll_peers(j, deadline, inbound).await.map_err(|seen| {
            format!("dashd-{j} did not get {expected} inbound peers; last saw {seen}")
        })
    }

    /// Polls dashd `i`'s `getpeerinfo` every 200 ms until `ok` holds or `until` passes. RPC
    /// errors are retried; the error is what the last poll saw.
    async fn poll_peers(
        &self,
        i: usize,
        until: Instant,
        ok: impl Fn(&[PeerInfo]) -> bool,
    ) -> Result<(), String> {
        loop {
            let seen = match self.rpc(NodeId::Dashd(i)).and_then(|rpc| rpc.peers()) {
                Ok(peers) if ok(&peers) => return Ok(()),
                Ok(peers) => format!("peers {peers:?}"),
                Err(e) => format!("getpeerinfo failed: {e}"),
            };
            if Instant::now() > until {
                return Err(seen);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The second copy of an edge would make one connection, and the wait for two inbound
    /// peers would stall until its deadline.
    #[tokio::test]
    #[should_panic(expected = "given 2 times")]
    async fn a_duplicate_edge_is_refused() {
        let edge = (NodeId::Dashd(0), NodeId::Dashd(1));
        let _ = MixedNetwork::builder()
            .dashd(2)
            .connect(edge.0, edge.1)
            .connect(edge.0, edge.1)
            .start()
            .await;
    }

    /// A peer that accepts the TCP connection, or connects in, and never sends `version` is not
    /// an edge: dashd lists it with version 0, and the waits fail at their deadlines saying so.
    #[tokio::test]
    async fn an_accepted_connection_without_a_handshake_is_not_an_edge() {
        if std::env::var_os("SKIP_DASHD_TESTS").is_some() {
            return;
        }
        let network = MixedNetwork::builder().dashd(1).start().await.unwrap();
        // The kernel completes connections into the backlog; nothing ever answers on them.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = silent.local_addr().unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let err = network.dashd_connect(0, addr, deadline).await.unwrap_err();
        assert!(err.contains(&format!("addr: \"{addr}\", version: 0")), "{err}");

        // An inbound connection that never sends `version` is listed, unhandshaken, and not
        // counted.
        let _quiet =
            std::net::TcpStream::connect(crate::loopback(network.dashd(0).p2p_port())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let err = network.dashd_inbound(0, 1, deadline).await.unwrap_err();
        assert!(err.contains("did not get 1 inbound peers"), "{err}");
        assert!(err.contains("version: 0, inbound: true"), "{err}");
    }
}
