//! P3 §5.1's (A − B) node leg with real V (S6a, D113): the same regtest blocks, with mainnet's
//! transaction counts, into two fresh nodes.
//!
//! - **A:** a scripted loopback peer serves them through the P2P path (`headers`, then `block`
//!   for each `getdata`) to a node whose P3 listener is registered.
//! - **B:** the same blocks go straight to V (`ChainCommand::ProcessBlock`) with the listener
//!   unregistered.
//!
//! Both nodes first take the same [`COINBASE_MATURITY`]-block prefix, uncounted: its first
//! coinbase funds every filler transaction, which spends its own matured output (BL-288), so the
//! blocks connect under V's `CoreConnector`. Each pass then counts `instructions:u` on every
//! thread of the process except the driver's (the scripted peer's, or the submitter's), from
//! the first workload block's offer until V's tip is the last block. P3's share per block is (A − B) / blocks. The Core leg needs E1/E6 (A-5), so the
//! comparison is a named PENDING row and this leg gates nothing yet. Testnet below 6,500 is
//! coinbase-only, so its numbers come from §6.7's runs, not from here.
//!
//! `amb_passes_run` runs both passes on a few blocks with no counters, in every test run.
//! `amb_node_leg` measures; it is ignored by default and must run alone:
//! `cargo test -p dashd-node --release --lib amb_node_leg -- --ignored --test-threads=1 --nocapture`.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dash_consensus::Block;
use dash_consensus::header::HeaderParams;
use dash_consensus::pow::check_proof_of_work;
use dash_consensus::primitives::{Header, Tx, TxHash};
use dash_consensus::tx::COINBASE_MATURITY;
use dash_num::CompactTarget;
use dash_p2p::CommandString;
use dash_p2p_core::{GetData, Headers, InvType};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{BlockHash, BlockHeader, Transaction};
use dash_types::codec::BaseCodec as _;
use dashd_chainstate::chain::genesis_block;
use dashd_p2p::processing::sync::block_hash;
use dashd_p2p::sim::{PeerVersionMsg, V1_HEADER, v1_frame};

use crate::init::{self, setup_server_args};
use crate::logging::Logger;
use crate::node::Node;
use crate::ui::Ui;

const REGTEST_BITS: u32 = 0x207f_ffff;
/// How long a pass may take before the test calls it stuck: the box runs at load 50–190.
const PATIENCE: Duration = Duration::from_secs(600);
/// `NODE_NETWORK | NODE_BLOOM | NODE_NETWORK_LIMITED`: no `NODE_HEADERS_COMPRESSED`, so the node
/// asks with `getheaders` and takes `headers`.
const PEER_SERVICES: u64 = 0x1 | 0x4 | 0x400;

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
    Headers {
        headers: blocks.into_iter().map(|b| *b.header().raw()).collect(),
    }
    .encode(&mut buf);
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
/// the prefix (BL-288). Returns the prefix and the workload.
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
    let mut prefix = mine(p, *genesis_block(p).header(), txs);
    let work = prefix.split_off(maturity as usize);
    (prefix, work)
}

/// Regtest blocks on `parent` (at `height`), the `i`th holding `ntx[i]` transactions, told apart
/// from another branch by `salt`. Its fillers are [`unfunded`].
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
    mine(p, parent, txs)
}

/// One regtest block on `parent` per entry of `txs`, each on the one before.
fn mine(
    p: &ChainSetup,
    parent: Header,
    txs: impl IntoIterator<Item = Vec<Arc<Tx>>>,
) -> Vec<Arc<Block>> {
    let pow = *HeaderParams::from_chain_params(p).expect("regtest").pow();
    let mut prev = parent;
    let mut out = Vec::new();
    for txs in txs {
        let mut raw = BlockHeader {
            version: 0x2000_0000,
            prev_hash: prev.hash(),
            time: prev.raw().time + 150,
            bits: REGTEST_BITS,
            ..BlockHeader::default()
        };
        raw.merkle_root = Block::new(Header::new(raw), txs.clone()).merkle().0;
        let header = (0..)
            .map(|nonce| {
                Header::new(BlockHeader {
                    nonce,
                    ..raw
                })
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

/// `instructions:u` per thread, for every thread but the caller's (Linux).
mod counters {
    use std::collections::BTreeSet;

    #[repr(C)]
    #[derive(Default)]
    struct Attr {
        type_: u32,
        size: u32,
        config: u64,
        sample_period: u64,
        sample_type: u64,
        read_format: u64,
        flags: u64,
        wakeup_events: u32,
        bp_type: u32,
        config1: u64,
    }

    const FLAG_EXCLUDE_KERNEL: u64 = 1 << 5;
    const FLAG_EXCLUDE_HV: u64 = 1 << 6;
    const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 1 << 3;

    /// The process's threads.
    pub fn threads() -> BTreeSet<i32> {
        std::fs::read_dir("/proc/self/task")
            .expect("/proc/self/task")
            .map(|e| e.expect("a task").file_name().to_string_lossy().parse().expect("a tid"))
            .collect()
    }

    /// Counters on every thread but the caller's, enabled from now.
    pub struct Counters(Vec<i32>, pub BTreeSet<i32>);

    impl Counters {
        pub fn open() -> Counters {
            // SAFETY: gettid has no preconditions.
            let me = unsafe { libc::gettid() };
            let all = threads();
            let fds = all
                .iter()
                .filter(|&&t| t != me)
                .filter_map(|&tid| {
                    let attr = Attr {
                        size: std::mem::size_of::<Attr>() as u32,
                        config: 1, // PERF_COUNT_HW_INSTRUCTIONS on PERF_TYPE_HARDWARE (0)
                        flags: FLAG_EXCLUDE_KERNEL | FLAG_EXCLUDE_HV,
                        ..Attr::default()
                    };
                    // SAFETY: `attr` is a valid perf_event_attr of the declared size.
                    let fd = unsafe {
                        libc::syscall(
                            libc::SYS_perf_event_open,
                            &attr as *const Attr,
                            tid,
                            -1 as libc::c_int,
                            -1 as libc::c_int,
                            PERF_FLAG_FD_CLOEXEC,
                        )
                    };
                    // A thread that exited since the listing has nothing to count.
                    (fd >= 0).then_some(fd as i32)
                })
                .collect::<Vec<_>>();
            assert!(!fds.is_empty(), "perf_event_open: {}", std::io::Error::last_os_error());
            Counters(fds, all)
        }

        /// The total so far.
        pub fn read(&self) -> u64 {
            self.0
                .iter()
                .map(|&fd| {
                    let mut v = 0u64;
                    // SAFETY: reads at most 8 bytes into an 8-byte value.
                    let n = unsafe { libc::read(fd, (&raw mut v).cast(), 8) };
                    assert_eq!(n, 8, "reading a counter");
                    v
                })
                .sum()
        }
    }

    impl Drop for Counters {
        fn drop(&mut self) {
            for &fd in &self.0 {
                // SAFETY: `fd` is ours.
                unsafe { libc::close(fd) };
            }
        }
    }
}

/// One pass's result.
#[derive(Debug)]
struct Pass {
    instructions: Option<u64>,
    /// Threads that started during the pass, which no counter saw.
    uncounted_threads: usize,
    wall: Duration,
}

/// A pass's measurement: the counters (when `count`) and the clock, started together.
struct Meter {
    counters: Option<counters::Counters>,
    start: Instant,
    before: Option<u64>,
}

impl Meter {
    fn start(count: bool) -> Meter {
        let counters = count.then(counters::Counters::open);
        let start = Instant::now();
        let before = counters.as_ref().map(counters::Counters::read);
        Meter {
            counters,
            start,
            before,
        }
    }

    fn finish(self) -> Pass {
        let after = self.counters.as_ref().map(counters::Counters::read);
        let wall = self.start.elapsed();
        let uncounted = self.counters.map_or(0, |c| counters::threads().difference(&c.1).count());
        Pass {
            instructions: self.before.zip(after).map(|(b, a)| a - b),
            uncounted_threads: uncounted,
            wall,
        }
    }
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

/// Pass A: the node dials the scripted peer (`-connect`), which completes the handshake, waits
/// for `getheaders` and serves `prefix` (`headers`, then each `getdata`); then, counted from
/// here, it sends `blocks`' headers and answers each `getdata` until V's tip is the last block.
fn pass_a(
    dir: &Path,
    params: &Arc<ChainSetup>,
    prefix: &[Arc<Block>],
    blocks: &[Arc<Block>],
    count: bool,
) -> Pass {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let connect = format!("-connect={}", listener.local_addr().expect("addr"));
    // The scripted peer speaks V1.
    let node =
        node(dir, &[&connect, "-dnsseed=0", "-fixedseeds=0", "-v2transport=0", "-debug=net"]);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("now");
    let start_height = (prefix.len() + blocks.len()) as i32;
    let mut peer = Peer::dialled(&listener, params, start_height, now.as_secs() as i64);
    let deadline = Instant::now() + PATIENCE;
    while !peer.poll().iter().any(|(c, _)| *c == CommandString::from_static("getheaders")) {
        assert!(Instant::now() < deadline, "no getheaders");
    }
    let by_hash: std::collections::HashMap<BlockHash, &Arc<Block>> =
        prefix.iter().chain(blocks).map(|b| (b.hash(), b)).collect();
    // Sends `part`'s headers, then serves each `getdata` until V's tip is `part`'s last block.
    let mut serve = |part: &[Arc<Block>], last: usize| {
        peer.send("headers", &headers_msg(part));
        while tip_height(&node) < last as u32 {
            assert!(
                Instant::now() < deadline + PATIENCE,
                "the tip stopped at {}",
                tip_height(&node)
            );
            for (cmd, payload) in peer.poll() {
                if cmd != CommandString::GETDATA {
                    continue;
                }
                let gd = GetData::decode(&mut &payload[..]).expect("a getdata");
                for inv in gd.inventory.iter().filter(|i| i.inv_type == InvType::Block) {
                    let block = by_hash[&block_hash(inv)];
                    peer.send("block", &dash_consensus::core_serialize(&**block));
                }
            }
        }
    };
    serve(prefix, prefix.len());

    let meter = Meter::start(count);
    serve(blocks, prefix.len() + blocks.len());
    let pass = meter.finish();
    drop(node);
    pass
}

/// Pass B: the same blocks to V with the network stopped (the listener unregistered), each
/// awaited before the next, as P3's actor submits; `prefix` first, uncounted.
fn pass_b(dir: &Path, prefix: &[Arc<Block>], blocks: &[Arc<Block>], count: bool) -> Pass {
    let mut node = node(dir, &["-connect=0", "-dnsseed=0", "-fixedseeds=0"]);
    node.stop_net();
    let handle = node.chain().expect("V runs").handle().clone();
    let submit = |part: &[Arc<Block>], last: usize| {
        for block in part {
            let r = handle.process_block(Arc::clone(block), true, None, true).recv().expect("V");
            assert!(r.new_block, "a new block");
        }
        wait_tip(&node, last as u32);
    };
    submit(prefix, prefix.len());
    let meter = Meter::start(count);
    submit(blocks, prefix.len() + blocks.len());
    let pass = meter.finish();
    drop(node);
    pass
}

pub(super) fn regtest() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest"))
}

/// Both passes connect the first fixture rows, fillers included, every test run (no counters:
/// CI may have no PMU).
#[test]
fn amb_passes_run() {
    let params = regtest();
    let (prefix, blocks) = funded(&params, &mainnet_ntx()[..8]);
    assert!(blocks.iter().any(|b| b.txs().len() > 1), "the rows have fillers");
    for pass in [
        pass_a(tempfile::tempdir().unwrap().path(), &params, &prefix, &blocks, false),
        pass_b(tempfile::tempdir().unwrap().path(), &prefix, &blocks, false),
    ] {
        assert!(pass.instructions.is_none());
    }
}

/// The measurement: every fixture row is one block. Prints one JSON line.
#[test]
#[ignore = "a measurement: run alone, release, with a PMU"]
fn amb_node_leg() {
    let params = regtest();
    let ntx = mainnet_ntx();
    let (prefix, blocks) = funded(&params, &ntx);
    let a = pass_a(tempfile::tempdir().unwrap().path(), &params, &prefix, &blocks, true);
    let b = pass_b(tempfile::tempdir().unwrap().path(), &prefix, &blocks, true);
    let (ai, bi) = (a.instructions.expect("counted"), b.instructions.expect("counted"));
    let n = blocks.len() as f64;
    println!(
        "{{\"blocks\":{},\"txs\":{},\"a_instructions\":{ai},\"b_instructions\":{bi},\
         \"a_minus_b_per_block\":{:.0},\"a_wall_s\":{:.3},\"b_wall_s\":{:.3},\
         \"uncounted_threads\":[{},{}],\"core_leg\":\"PENDING E1/E6\"}}",
        blocks.len(),
        ntx.iter().map(|&t| t.max(1) as u64).sum::<u64>(),
        (ai as f64 - bi as f64) / n,
        a.wall.as_secs_f64(),
        b.wall.as_secs_f64(),
        a.uncounted_threads,
        b.uncounted_threads,
    );
    let threads: BTreeSet<_> = [a.uncounted_threads, b.uncounted_threads].into();
    assert_eq!(threads, [0].into(), "a thread started inside a pass");
}
