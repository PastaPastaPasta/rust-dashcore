//! CH3b b2b-2's regtest network for the Core functional-test ports: n nodes of one binary,
//! rdashd or dashd v24, each in its own 0700 datadir with its own P2P and RPC ports, started with
//! the framework's flags, connected, split and synced as the framework's helpers do
//! (`test_framework.py:705-849`), and stopped cleanly with a binary-aware `stop`.
//!
//! Every wait is bounded by a base deadline times one [`timeout_factor`] (Core's
//! `--timeoutfactor`), so a loaded box delays a failure but never turns one into a pass: each
//! wait needs its condition seen positively before a monotonic deadline.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub use dash_args::univalue::SettingsValue as Json;

use super::{
    HTTP_START_FAILED, RDASHD, RetainedHome, Running, cookie, debug_log, free_port, private_dir,
    read_since, regtest_on, rpc, stderr,
};

/// Core's `TIME_GENESIS_BLOCK`: regtest genesis's time, a clean chain's mocktime.
pub const TIME_GENESIS_BLOCK: u64 = super::mine::GENESIS_TIME as u64;

/// The addresses of `TestNode.PRIV_KEYS` (`test_node.py:183-205`): node i mines to the i-th, as
/// `TestNode.generate` does.
pub const NODE_ADDRESSES: [&str; 4] = [
    "yYdShjQSptFKitYLksFEUSwHe4hnbar5rf",
    "yfTFJgvq65UZsb9RBbpdYAAzsJoCGXqH2w",
    "yU3w4VDjKhHiZpWszkUZVnFTS56AfgdfPV",
    "yYhzix2R5LiYnDixsUnF8XwBYGYpyeTgB4",
];

/// `address.py`'s `ADDRESS_BCRT1_UNSPENDABLE_DESCRIPTOR`.
pub const ADDRESS_BCRT1_UNSPENDABLE_DESCRIPTOR: &str =
    "addr(yVg3NBUHNEhgDceqwVUjsZHreC5PBHnUo9)#e5kt0jtk";

/// `address.py`'s `ADDRESS_BCRT1_P2SH_OP_TRUE`.
pub const ADDRESS_BCRT1_P2SH_OP_TRUE: &str = "8zJctvfrzGZ5s1zQ3kagwyW1DsPYSQ4V2P";

/// The factor every deadline is scaled by: `B2B2_TIMEOUT_FACTOR`, a whole number from 1, else
/// 10. Anything else fails the test.
pub fn timeout_factor() -> u32 {
    static FACTOR: OnceLock<u32> = OnceLock::new();
    *FACTOR.get_or_init(|| match std::env::var("B2B2_TIMEOUT_FACTOR") {
        Err(std::env::VarError::NotPresent) => 10,
        Ok(text) => match text.parse::<u32>() {
            Ok(factor @ 1..=1000) => factor,
            _ => panic!("B2B2_TIMEOUT_FACTOR={text:?} is not a whole number from 1 to 1000"),
        },
        Err(e) => panic!("B2B2_TIMEOUT_FACTOR: {e}"),
    })
}

/// `secs` times the [`timeout_factor`].
pub fn scaled(secs: u64) -> Duration {
    Duration::from_secs(secs * u64::from(timeout_factor()))
}

/// Waits until `done`, failing once [`scaled`] `base_secs` have passed without it.
pub fn wait_for(what: &str, base_secs: u64, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + scaled(base_secs);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "timed out after {:?} waiting for {what}",
            scaled(base_secs)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Which daemon a run starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dashd,
    Rdashd,
}

/// A daemon binary and its kind.
#[derive(Clone, Debug)]
pub struct Binary {
    pub path: PathBuf,
    pub kind: Kind,
}

impl Binary {
    /// The pinned dashd v24 these tests cannot skip (`require_dashd`, `CORE_VERSION`).
    pub fn dashd() -> Binary {
        Binary {
            path: super::require_dashd(),
            kind: Kind::Dashd,
        }
    }

    pub fn rdashd() -> Binary {
        Binary {
            path: PathBuf::from(RDASHD),
            kind: Kind::Rdashd,
        }
    }

    /// The `stop` RPC's reply: `PACKAGE_NAME " stopping"` (Core `rpc/server.cpp:184`).
    fn stopping(&self) -> &'static str {
        match self.kind {
            Kind::Dashd => "Dash Core stopping",
            Kind::Rdashd => "rdashd stopping",
        }
    }
}

/// `key`'s value in the JSON object `v`.
pub fn field<'a>(v: &'a Json, key: &str) -> &'a Json {
    let Json::Obj(fields) = v else {
        panic!("not an object: {}", v.write())
    };
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
        .unwrap_or_else(|| panic!("no {key} in {}", v.write()))
}

/// `key`'s value in `v`, if `v` is an object that has it.
pub fn try_field<'a>(v: &'a Json, key: &str) -> Option<&'a Json> {
    match v {
        Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

pub fn as_str(v: &Json) -> &str {
    v.get_str().unwrap_or_else(|_| panic!("not a string: {}", v.write()))
}

pub fn as_i64(v: &Json) -> i64 {
    v.get_int64().unwrap_or_else(|_| panic!("not an integer: {}", v.write()))
}

pub fn as_array(v: &Json) -> &[Json] {
    match v {
        Json::Arr(items) => items,
        _ => panic!("not an array: {}", v.write()),
    }
}

/// `s` as a JSON string.
pub fn json_str(s: &str) -> String {
    Json::Str(s.to_owned()).write()
}

/// An RPC error: its code and message.
#[derive(Debug, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// A regtest node of a [`Net`].
pub struct Node {
    pub index: usize,
    binary: Binary,
    home: PathBuf,
    pub datadir: PathBuf,
    pub p2p_port: u16,
    pub rpc_port: u16,
    extra: Vec<String>,
    running: Option<Running>,
}

/// How many port pairs a first start tries before a lost bind race fails the test.
const PORT_ATTEMPTS: usize = 3;

impl Node {
    /// The flags of every node (PLAN §3, A4, A6), with `mocktime` and the node's ports.
    fn args(&self, mocktime: u64) -> Vec<String> {
        let mut args: Vec<String> = [
            "-debug",
            "-debugexclude=libevent",
            "-debugexclude=leveldb",
            "-debugexclude=rand",
            "-logtimemicros",
            "-connect=0",
            "-discover=0",
            "-dnsseed=0",
            "-fixedseeds=0",
            "-listenonion=0",
            "-upnp=0",
            "-natpmp=0",
            "-peertimeout=999999999",
            "-disablewallet",
            "-blocksonly=1",
            "-printtoconsole=0",
        ]
        .map(str::to_owned)
        .to_vec();
        args.push(format!("-uacomment=testnode{}", self.index));
        args.push(format!("-mocktime={mocktime}"));
        args.push(format!("-bind=127.0.0.1:{}", self.p2p_port));
        args.push(format!("-rpcport={}", self.rpc_port));
        args.extend(self.extra.iter().cloned());
        args
    }

    /// Starts the node and waits until init is done. A first start (`retry`) that loses its
    /// P2P or RPC port to another process before binding it starts again on new ports; any other
    /// exit during init fails the test with the node's stderr.
    fn start(&mut self, mocktime: u64, retry: bool) {
        assert!(self.running.is_none(), "node {} is running", self.index);
        let log = debug_log(&self.datadir);
        for attempt in 1..=PORT_ATTEMPTS {
            let before = std::fs::metadata(&log).ok().map(|m| m.len());
            let args = self.args(mocktime);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let cmd = regtest_on(&self.binary.path, &self.home, &self.datadir, &args);
            let mut running = Running::spawn(cmd, log.clone());
            let deadline = Instant::now() + scaled(60);
            let exited = loop {
                if read_since(&log, before).contains("init message: Done loading") {
                    break false;
                }
                if running.exited() {
                    break true;
                }
                assert!(Instant::now() < deadline, "node {} did not finish init", self.index);
                std::thread::sleep(Duration::from_millis(50));
            };
            if !exited {
                self.running = Some(running);
                return;
            }
            let out = running.wait();
            let new = read_since(&log, before);
            let lost_p2p = stderr(&out).contains(&format!("127.0.0.1:{}", self.p2p_port));
            let lost_rpc = stderr(&out) == HTTP_START_FAILED
                && new.contains(&format!(
                    "Binding RPC on address 127.0.0.1 port {} failed.",
                    self.rpc_port
                ));
            assert!(
                retry && (lost_p2p || lost_rpc) && attempt < PORT_ATTEMPTS,
                "node {} exited ({}) during init: {}",
                self.index,
                out.status,
                stderr(&out)
            );
            self.p2p_port = free_port();
            self.rpc_port = free_port();
        }
        unreachable!("the last attempt returns or fails")
    }

    /// `stop`, its binary's reply, then a clean exit. A node that does not exit in time is
    /// killed and reaped by its pid ([`Running`]'s drop) as the test fails.
    pub fn stop(&mut self) {
        let reply = self.call("stop", "[]");
        assert_eq!(as_str(&reply), self.binary.stopping(), "node {}", self.index);
        let mut running = self.running.take().expect("a running node");
        wait_for(&format!("node {} to exit", self.index), 60, || running.exited());
        let out = running.wait();
        assert_eq!(out.status.code(), Some(0), "node {}: {}", self.index, stderr(&out));
    }

    fn log_path(&self) -> PathBuf {
        debug_log(&self.datadir)
    }

    /// The debug log's length now, where a log window opens.
    pub fn log_len(&self) -> u64 {
        std::fs::metadata(self.log_path()).map_or(0, |m| m.len())
    }

    /// What the debug log gained since it was `from` bytes long.
    pub fn log_since(&self, from: u64) -> String {
        read_since(&self.log_path(), Some(from))
    }

    /// `method` with `params` (JSON text): its result, or its error.
    pub fn try_call(&self, method: &str, params: &str) -> Result<Json, RpcError> {
        let reply = rpc(self.rpc_port, &cookie(&self.datadir), method, params);
        let body = Json::read(reply.body.as_bytes()).unwrap_or_else(|| {
            panic!("node {} {method}: not JSON ({}): {}", self.index, reply.status, reply.body)
        });
        let error = field(&body, "error");
        if error.is_null() {
            assert_eq!(reply.status, 200, "node {} {method}: {}", self.index, reply.body);
            return Ok(field(&body, "result").clone());
        }
        Err(RpcError {
            code: as_i64(field(error, "code")),
            message: as_str(field(error, "message")).to_owned(),
        })
    }

    /// `method`'s result; an error fails the test.
    pub fn call(&self, method: &str, params: &str) -> Json {
        self.try_call(method, params)
            .unwrap_or_else(|e| panic!("node {} {method} {params}: {e:?}", self.index))
    }

    pub fn block_count(&self) -> i64 {
        as_i64(&self.call("getblockcount", "[]"))
    }

    pub fn best_hash(&self) -> String {
        as_str(&self.call("getbestblockhash", "[]")).to_owned()
    }

    pub fn block_hash(&self, height: i64) -> String {
        as_str(&self.call("getblockhash", &format!("[{height}]"))).to_owned()
    }

    /// `method <hash>`, whose result must be null.
    fn hash_call(&self, method: &str, hash: &str) {
        assert!(self.call(method, &format!("[{}]", json_str(hash))).is_null());
    }

    pub fn invalidate(&self, hash: &str) {
        self.hash_call("invalidateblock", hash);
    }

    pub fn reconsider(&self, hash: &str) {
        self.hash_call("reconsiderblock", hash);
    }

    pub fn precious(&self, hash: &str) {
        self.hash_call("preciousblock", hash);
    }

    /// `getblock <hash> 0`: the block's hex.
    pub fn raw_block(&self, hash: &str) -> String {
        as_str(&self.call("getblock", &format!("[{},0]", json_str(hash)))).to_owned()
    }

    /// `submitblock <hex>`'s result: null, or a BIP22 string.
    pub fn submit(&self, hex: &str) -> Json {
        self.call("submitblock", &format!("[{}]", json_str(hex)))
    }

    /// `method <n> <target>` (`generatetoaddress` or `generatetodescriptor`): the blocks'
    /// hashes.
    fn generate_via(&self, method: &str, n: u32, target: &str) -> Vec<String> {
        let hashes = self.call(method, &format!("[{n},{}]", json_str(target)));
        as_array(&hashes).iter().map(|h| as_str(h).to_owned()).collect()
    }

    /// `generatetoaddress`: `n` blocks to `address`, their hashes.
    pub fn generate_to(&self, n: u32, address: &str) -> Vec<String> {
        self.generate_via("generatetoaddress", n, address)
    }

    pub fn generate_to_descriptor(&self, n: u32, descriptor: &str) -> Vec<String> {
        self.generate_via("generatetodescriptor", n, descriptor)
    }

    /// `TestNode.generate`: `n` blocks to the node's deterministic address.
    pub fn generate(&self, n: u32) -> Vec<String> {
        self.generate_to(n, NODE_ADDRESSES[self.index])
    }

    /// [`Node::generate`]'s last block: the node's new tip.
    pub fn generate_tip(&self, n: u32) -> String {
        self.generate(n).pop().expect("at least one block")
    }

    pub fn chain_tips(&self) -> Vec<Json> {
        as_array(&self.call("getchaintips", "[]")).to_vec()
    }

    pub fn peers(&self) -> Vec<Json> {
        as_array(&self.call("getpeerinfo", "[]")).to_vec()
    }

    pub fn subversion(&self) -> String {
        as_str(field(&self.call("getnetworkinfo", "[]"), "subversion")).to_owned()
    }

    /// `connect_nodes`' `find_conn`: the peer with `subver` and direction `inbound`.
    fn find_conn(&self, subver: &str, inbound: bool) -> Option<Json> {
        self.peers().into_iter().find(|p| {
            as_str(field(p, "subver")) == subver && field(p, "inbound").get_bool() == Ok(inbound)
        })
    }

    /// `disconnect_nodes`' `get_peer_ids`: the ids of peers whose subver names `testnode<other>`.
    fn peer_ids(&self, other: usize) -> Vec<i64> {
        let name = format!("testnode{other}");
        self.peers()
            .iter()
            .filter(|p| as_str(field(p, "subver")).contains(&name))
            .map(|p| as_i64(field(p, "id")))
            .collect()
    }
}

/// n nodes of one binary, in one retained home.
pub struct Net {
    pub nodes: Vec<Node>,
    /// The time every (re)start passes as `-mocktime`: the last [`Net::mocktime`] set.
    mocktime: u64,
    _home: RetainedHome,
}

impl Net {
    /// `n` nodes of `binary`, each with `extra` after the common flags, started at `mocktime`
    /// with no connections.
    pub fn start(binary: &Binary, n: usize, mocktime: u64, extra: &[&str]) -> Net {
        let prefix = match binary.kind {
            Kind::Dashd => "b2b2-dashd-",
            Kind::Rdashd => "b2b2-rdashd-",
        };
        let home = RetainedHome::new(prefix);
        let mut nodes: Vec<Node> = (0..n)
            .map(|index| {
                let datadir = home.path().join(format!("node{index}"));
                private_dir(&datadir);
                Node {
                    index,
                    binary: binary.clone(),
                    home: home.path().to_owned(),
                    datadir,
                    p2p_port: free_port(),
                    rpc_port: free_port(),
                    extra: extra.iter().map(|a| (*a).to_owned()).collect(),
                    running: None,
                }
            })
            .collect();
        for node in &mut nodes {
            node.start(mocktime, true);
        }
        Net {
            nodes,
            mocktime,
            _home: home,
        }
    }

    /// `restart_node`: stop node `i`, wait for its exit, start it again on its ports.
    pub fn restart(&mut self, i: usize) {
        self.nodes[i].stop();
        let mocktime = self.mocktime;
        self.nodes[i].start(mocktime, false);
    }

    /// Stops every node cleanly.
    pub fn stop_all(&mut self) {
        for node in &mut self.nodes {
            node.stop();
        }
    }

    /// `connect_nodes(a, b)`: `addnode <b> onetry` on `a`, then both sides list the other (by
    /// subversion and direction) and have had a pong (`bytesrecv_per_msg.pong` ≥ 29).
    pub fn connect(&self, a: usize, b: usize) {
        let (from, to) = (&self.nodes[a], &self.nodes[b]);
        let target = json_str(&format!("127.0.0.1:{}", to.p2p_port));
        assert!(from.call("addnode", &format!("[{target},\"onetry\"]")).is_null());
        let (from_subver, to_subver) = (from.subversion(), to.subversion());
        wait_for(&format!("{a} to list {b}"), 60, || from.find_conn(&to_subver, false).is_some());
        wait_for(&format!("{b} to list {a}"), 60, || to.find_conn(&from_subver, true).is_some());
        let ponged = |node: &Node, subver: &str, inbound: bool| {
            let peer = node.find_conn(subver, inbound).expect("Error: peer disconnected");
            let recv = field(&peer, "bytesrecv_per_msg");
            try_field(recv, "pong").map_or(0, as_i64) >= 29
        };
        wait_for(&format!("{a}'s pong from {b}"), 60, || ponged(from, &to_subver, false));
        wait_for(&format!("{b}'s pong from {a}"), 60, || ponged(to, &from_subver, true));
    }

    /// `disconnect_nodes(a, b)`: `disconnectnode` by `nodeid` (the named argument) for each
    /// peer of `a` naming `b`, then neither lists the other.
    pub fn disconnect(&self, a: usize, b: usize) {
        let (node_a, node_b) = (&self.nodes[a], &self.nodes[b]);
        let ids = node_a.peer_ids(b);
        assert!(!ids.is_empty(), "disconnect: {a} and {b} were not connected");
        for id in ids {
            match node_a.try_call("disconnectnode", &format!(r#"{{"nodeid":{id}}}"#)) {
                Ok(v) => assert!(v.is_null()),
                // RPC_CLIENT_NODE_NOT_CONNECTED: it went meanwhile.
                Err(e) => assert_eq!(e.code, -29, "{e:?}"),
            }
        }
        wait_for(&format!("{a} to drop {b}"), 5, || node_a.peer_ids(b).is_empty());
        wait_for(&format!("{b} to drop {a}"), 5, || node_b.peer_ids(a).is_empty());
    }

    /// `sync_blocks(nodes)`: every node in `subset` has one best hash, each keeping a peer
    /// meanwhile.
    pub fn sync_blocks(&self, subset: &[usize]) {
        let deadline = Instant::now() + scaled(60);
        loop {
            let best: Vec<String> = subset.iter().map(|&i| self.nodes[i].best_hash()).collect();
            if best.iter().all(|h| *h == best[0]) {
                return;
            }
            for &i in subset {
                assert!(!self.nodes[i].peers().is_empty(), "node {i} has no peer: {best:?}");
            }
            assert!(Instant::now() < deadline, "block sync timed out: {best:?}");
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// `setmocktime(absolute)` on `nodes`, then `mockscheduler(delta)` when a delta is given
    /// (Core's `bump_mocktime` with `update_schedulers`). Later restarts start at `absolute`.
    pub fn mocktime(&mut self, nodes: &[usize], absolute: u64, scheduler_delta: Option<u64>) {
        for &i in nodes {
            assert!(self.nodes[i].call("setmocktime", &format!("[{absolute}]")).is_null());
        }
        if let Some(delta) = scheduler_delta {
            for &i in nodes {
                assert!(self.nodes[i].call("mockscheduler", &format!("[{delta}]")).is_null());
            }
        }
        self.mocktime = absolute;
    }
}

impl std::ops::Index<usize> for Net {
    type Output = Node;

    fn index(&self, i: usize) -> &Node {
        &self.nodes[i]
    }
}

/// What a run records at its FT's checkpoints, compared across the two binaries in full.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Record(pub Vec<(String, String)>);

impl Record {
    pub fn push(&mut self, label: &str, value: impl ToString) {
        self.0.push((label.to_owned(), value.to_string()));
    }

    /// The run's `getchaintips` on `node`, as JSON text in a canonical order: height descending,
    /// then hash ascending within a height. Every field and full hash is kept; only the order
    /// of tips at one height is normalised. Core breaks such ties by `CBlockIndex*` address
    /// (`CompareBlocksByHeight`, `src/rpc/blockchain.cpp:1743-1756`), an order unspecified even
    /// between two dashd runs, and rdashd by entry id (SB4 D9). The raw list goes to the run's
    /// stderr and is returned, so the FTs' own assertions read the very response recorded, in
    /// its raw order.
    pub fn tips(&mut self, label: &str, node: &Node) -> Vec<Json> {
        let raw = node.chain_tips();
        eprintln!("{label} (raw): {}", Json::Arr(raw.clone()).write());
        let mut tips = raw.clone();
        tips.sort_by(|a, b| {
            let key = |t: &Json| {
                (std::cmp::Reverse(as_i64(field(t, "height"))), as_str(field(t, "hash")).to_owned())
            };
            key(a).cmp(&key(b))
        });
        self.push(label, Json::Arr(tips).write());
        raw
    }
}

/// Runs `scenario` with every node dashd v24, then with every node rdashd (so a missing dashd
/// fails first), prints each run's time, and asserts the two records equal in full.
pub fn run_on_both(name: &str, scenario: impl Fn(&Binary) -> Record) {
    let timed = |binary: Binary| {
        let start = Instant::now();
        let record = scenario(&binary);
        eprintln!("{name} on {:?}: {:.1?}", binary.kind, start.elapsed());
        record
    };
    let core = timed(Binary::dashd());
    let ours = timed(Binary::rdashd());
    assert_eq!(ours, core);
}
