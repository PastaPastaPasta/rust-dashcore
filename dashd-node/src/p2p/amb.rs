//! Scripted peers, regtest blocks and a test node for the p2p tests, and P3's (A − B) node leg
//! in `measure` (Linux x86_64 only: it needs `node_amb::sys`).
// Elsewhere the helpers only `measure` uses are dead.
#![cfg_attr(
    not(all(target_os = "linux", target_arch = "x86_64")),
    allow(dead_code, unused_imports)
)]

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dash_clock::Clock as _;
use dash_consensus::Block;
use dash_consensus::header::HeaderParams;
use dash_consensus::pow::check_proof_of_work;
use dash_consensus::primitives::{Header, Tx, TxHash};
use dash_consensus::tx::COINBASE_MATURITY;
use dash_num::CompactTarget;
use dash_p2p::{CommandString, PeerId};
use dash_p2p_core::{GetData, Headers, InvType, SendCmpct};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{BlockHash, BlockHeader, Transaction};
use dash_types::codec::BaseCodec as _;
use dashd_chainstate::chain::{Queued, ValidationSignals, genesis_block};
use dashd_p2p::processing::peerman::{PassAck, Shared};
use dashd_p2p::processing::sync::block_hash;
use dashd_p2p::sim::{PeerVersionMsg, V1_HEADER, v1_frame};
use node_amb::Refusal;
use node_amb::account::Completion;
use node_amb::fixture::{self, Fixture, PEER_SERVICES, T0};
use node_amb::record::{Conditions, Identity, Leg, Pass, Record};

use crate::init::{self, setup_server_args};
use crate::logging::Logger;
use crate::node::Node;
use crate::ui::Ui;

const REGTEST_BITS: u32 = 0x207f_ffff;
/// How long a pass may take before the test calls it stuck: the box runs at load 50–190.
pub(super) const PATIENCE: Duration = Duration::from_secs(600);

/// The checked-in mainnet transaction counts (height, nTx).
fn mainnet_ntx() -> Vec<u32> {
    include_str!("../../tests/fixtures/p3_amb_mainnet_ntx.txt")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.split_whitespace().nth(1).expect("height nTx").parse().expect("nTx"))
        .collect()
}

/// Core's `CScript() << height`.
fn height_push(n: u32) -> Vec<u8> {
    match n {
        1..=16 => vec![0x50 + n as u8],
        _ => {
            let mut num = n.to_le_bytes().to_vec();
            while num.last() == Some(&0) {
                num.pop();
            }
            if num.last().is_some_and(|b| b & 0x80 != 0) {
                num.push(0);
            }
            [vec![num.len() as u8], num].concat()
        }
    }
}

/// A transaction's one input.
enum Input {
    /// The coinbase at a height (null prevout, BIP34 push), told apart by a salt.
    Coinbase(u32, u8),
    /// An output of a transaction: `txid`, `vout`.
    Spend([u8; 32], u32),
}

/// A one-input transaction paying `outputs` zero-value `OP_TRUE` outputs. An `OP_1` script sig
/// satisfies such an output, and a zero value passes `CheckTxInputs` (no fee, no dust rule in
/// consensus).
fn tx(input: Input, outputs: u32) -> Arc<Tx> {
    let mut raw = 1i32.to_le_bytes().to_vec();
    raw.push(1);
    let script_sig = match input {
        Input::Coinbase(height, salt) => {
            raw.extend([0u8; 32]);
            raw.extend(u32::MAX.to_le_bytes());
            [height_push(height), vec![1, salt]].concat()
        }
        Input::Spend(txid, vout) => {
            raw.extend(txid);
            raw.extend(vout.to_le_bytes());
            vec![0x51]
        }
    };
    raw.push(script_sig.len() as u8);
    raw.extend(script_sig);
    raw.extend(u32::MAX.to_le_bytes());
    match u16::try_from(outputs).expect("at most 65,535 outputs") {
        n @ 0..0xfd => raw.push(n as u8),
        n => {
            raw.push(0xfd);
            raw.extend(n.to_le_bytes());
        }
    }
    for _ in 0..outputs {
        raw.extend(0i64.to_le_bytes());
        raw.extend([1, 0x51]);
    }
    raw.extend(0u32.to_le_bytes());
    Arc::new(Tx::new(Transaction::decode(&mut raw.as_slice()).expect("a transaction")))
}

/// A `headers` message with `blocks`' headers.
pub(super) fn headers_msg<'a>(blocks: impl IntoIterator<Item = &'a Arc<Block>>) -> Vec<u8> {
    let mut buf = Vec::new();
    Headers::new(blocks.into_iter().map(|b| *b.header().raw()).collect()).encode(&mut buf);
    buf
}

fn coinbase(height: u32, salt: u8) -> Arc<Tx> {
    tx(Input::Coinbase(height, salt), 1)
}

/// A filler spending an outpoint no block creates: `CoreConnector`'s `CheckTxInputs` rejects it
/// (`bad-txns-inputs-missingorspent`), so a block holding one is stored but never connects.
fn unfunded(height: u32, index: u32, salt: u8) -> Arc<Tx> {
    let mut prevout = [salt; 32];
    prevout[..4].copy_from_slice(&height.to_le_bytes());
    prevout[4..8].copy_from_slice(&index.to_le_bytes());
    tx(Input::Spend(prevout, 0), 1)
}

/// The (A − B) workload on genesis: a prefix of [`COINBASE_MATURITY`] coinbase-only blocks, the
/// first paying one output per filler, then a block per `ntx` row holding that many
/// transactions, each filler spending its own output of the first block's coinbase, matured by
/// the prefix (BL-288). Blocks are a second apart and the last is at [`T0`]. Returns the prefix
/// and the workload.
fn funded(p: &ChainSetup, ntx: &[u32]) -> (Vec<Arc<Block>>, Vec<Arc<Block>>) {
    const SALT: u8 = 0x5a;
    let maturity = COINBASE_MATURITY as u32;
    let fillers: u32 = ntx.iter().map(|&n| n.max(1) - 1).sum();
    let funding = tx(Input::Coinbase(1, SALT), fillers);
    let txid: TxHash = funding.txid();
    let mut txs = vec![vec![funding]];
    txs.extend((2..=maturity).map(|h| vec![coinbase(h, SALT)]));
    let mut vout = 0..fillers;
    for (h, &n) in (maturity + 1..).zip(ntx) {
        let spends = vout.by_ref().take(n.max(1) as usize - 1);
        let block = std::iter::once(coinbase(h, SALT))
            .chain(spends.map(|v| tx(Input::Spend(*txid.as_bytes(), v), 1)));
        txs.push(block.collect());
    }
    let first = u32::try_from(T0).expect("T0 fits") - (txs.len() as u32 - 1);
    let mut prefix = mine(p, *genesis_block(p).header(), first, 1, txs);
    let work = prefix.split_off(maturity as usize);
    (prefix, work)
}

/// Regtest blocks on `parent` (at `height`), the `i`th holding `ntx[i]` transactions, told apart
/// from another branch by `salt`, 150 s apart. Its fillers are [`unfunded`].
pub(super) fn branch(
    p: &ChainSetup,
    parent: Header,
    height: u32,
    ntx: &[u32],
    salt: u8,
) -> Vec<Arc<Block>> {
    let txs = (height + 1..).zip(ntx).map(|(h, &n)| {
        std::iter::once(coinbase(h, salt))
            .chain((1..n.max(1)).map(|i| unfunded(h, i, salt)))
            .collect()
    });
    mine(p, parent, parent.raw().time() + 150, 150, txs)
}

/// `branch`, with its first block's time `start` (at least one second after `parent`'s).
pub(super) fn branch_from(
    p: &ChainSetup,
    parent: Header,
    n: u32,
    salt: u8,
    start: u32,
) -> Vec<Arc<Block>> {
    let txs = (1..=n).map(|h| vec![coinbase(h, salt)]);
    mine(p, parent, start.max(parent.raw().time() + 1), 150, txs)
}

/// One regtest block on `parent` per entry of `txs`, each on the one before, the first at
/// `time` and each next `spacing` seconds later.
fn mine(
    p: &ChainSetup,
    parent: Header,
    time: u32,
    spacing: u32,
    txs: impl IntoIterator<Item = Vec<Arc<Tx>>>,
) -> Vec<Arc<Block>> {
    let pow = *HeaderParams::from_chain_params(p).expect("regtest").pow();
    let mut prev = parent;
    let mut out = Vec::new();
    for (i, txs) in txs.into_iter().enumerate() {
        let mut raw = {
            let base = BlockHeader::default();
            BlockHeader::new(
                0x2000_0000,
                prev.hash(),
                base.merkle_root(),
                time + i as u32 * spacing,
                REGTEST_BITS,
                base.nonce(),
            )
        };
        raw.set_merkle_root(Block::new(Header::new(raw), txs.clone()).merkle().0);
        let header = (0..)
            .map(|nonce| {
                Header::new(BlockHeader::new(
                    raw.version(),
                    raw.prev_hash(),
                    raw.merkle_root(),
                    raw.time(),
                    raw.bits(),
                    nonce,
                ))
            })
            .find(|h| {
                check_proof_of_work(h.hash().as_bytes(), CompactTarget::new(REGTEST_BITS), &pow)
            })
            .expect("a nonce");
        prev = header;
        out.push(Arc::new(Block::new(header, txs)));
    }
    out
}

/// A regtest node on `dir` through step 13, with `argv`.
pub(super) fn node(dir: &Path, argv: &[&str]) -> Node {
    let args = ArgsManager::new();
    setup_server_args(&args);
    let datadir = format!("-datadir={}", dir.display());
    let mut full = vec!["rdashd", datadir.as_str(), "-regtest", "-printtoconsole=0"];
    full.extend(init::quiet_node_args(argv));
    full.extend_from_slice(argv);
    args.parse_parameters(&full).expect("argv");
    let params = init::init_config(&args).expect("regtest");
    let logger = Arc::new(Logger::new());
    if std::env::var_os("P3_AMB_LOG").is_some() {
        logger.push_back_callback(|s| eprint!("{s}"));
        let _ = tracing::subscriber::set_global_default(crate::logging::CoreSubscriber::new(
            Arc::clone(&logger),
        ));
    }
    let ui = Ui::with_stderr(logger, Box::new(|s| eprint!("{s}")));
    let mut node = Node::new(Arc::new(args), Arc::new(ui), params);
    assert!(node.app_init_early() && node.app_init_main(), "init");
    node
}

fn tip_height(node: &Node) -> u32 {
    let chain = node.chain().expect("V runs");
    chain.handle().view_handle().view().snap.tip.tip_height
}

fn wait_tip(node: &Node, height: u32) {
    let deadline = Instant::now() + PATIENCE;
    while tip_height(node) < height {
        assert!(Instant::now() < deadline, "the tip stopped at {}", tip_height(node));
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The scripted peer's end of one connection, V1 framing.
pub(super) struct Peer {
    sock: TcpStream,
    params: Arc<ChainSetup>,
    buf: Vec<u8>,
}

impl Peer {
    /// The node's connection to `listener`, within [`PATIENCE`], greeted with our `version` (at
    /// `time`, `start_height`) and `verack`.
    pub(super) fn dialled(
        listener: &TcpListener,
        params: &Arc<ChainSetup>,
        start_height: i32,
        time: i64,
    ) -> Peer {
        listener.set_nonblocking(true).expect("nonblocking");
        let deadline = Instant::now() + PATIENCE;
        let sock = loop {
            match listener.accept() {
                Ok((sock, _)) => break sock,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "the node never dialled");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("accept: {e}"),
            }
        };
        sock.set_nonblocking(false).expect("blocking");
        sock.set_read_timeout(Some(Duration::from_millis(1))).expect("timeout");
        sock.set_nodelay(true).expect("nodelay");
        let mut peer = Peer {
            sock,
            params: Arc::clone(params),
            buf: Vec::new(),
        };
        let version = PeerVersionMsg {
            services: PEER_SERVICES,
            start_height,
            ..PeerVersionMsg::v24(time)
        };
        peer.send("version", &version.payload());
        peer.send("verack", &[]);
        peer
    }

    pub(super) fn send(&mut self, cmd: &str, payload: &[u8]) {
        self.sock.write_all(&v1_frame(&self.params, cmd, payload)).expect("send");
    }

    /// The next frames, waiting up to 1 ms for bytes.
    fn frames(&mut self) -> Vec<(CommandString, Vec<u8>)> {
        let mut chunk = [0u8; 65536];
        match self.sock.read(&mut chunk) {
            Ok(0) => panic!("the node closed the connection"),
            Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => panic!("read: {e}"),
        }
        let mut out = Vec::new();
        while self.buf.len() >= V1_HEADER {
            let len = u32::from_le_bytes(self.buf[16..20].try_into().expect("4 bytes")) as usize;
            if self.buf.len() < V1_HEADER + len {
                break;
            }
            let cmd = CommandString::from_bytes(self.buf[4..16].try_into().expect("12 bytes"));
            out.push((cmd, self.buf[V1_HEADER..V1_HEADER + len].to_vec()));
            self.buf.drain(..V1_HEADER + len);
        }
        out
    }

    /// Answers `ping`s; returns the frames that are not one.
    pub(super) fn poll(&mut self) -> Vec<(CommandString, Vec<u8>)> {
        let mut rest = Vec::new();
        for (cmd, payload) in self.frames() {
            if cmd == CommandString::from_static("ping") {
                self.send("pong", &payload);
            } else {
                rest.push((cmd, payload));
            }
        }
        rest
    }
}

pub(super) fn regtest() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest"))
}

/// Step 6's network setup with `max_connections`, V1 only and 1024 file descriptors.
pub(super) fn net_setup(max_connections: i32) -> crate::init::checks::NetSetup {
    crate::init::checks::NetSetup {
        basic_filter_index: false,
        v2_transport: false,
        compact_filters: false,
        allow_private_net: false,
        user_max_connections: max_connections,
        max_connections,
        fd: 1024,
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod measure;
