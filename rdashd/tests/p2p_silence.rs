//! P3 S6a's runtime proofs of silence (DEC-204, DEC-280) for the negative asserted-log rows its
//! network composition brought into rdashd's build. rdashd dials a scripted peer (`-connect`),
//! which plays the closest scenario rdashd reaches to the Core functional test asserting a line
//! stays out of the log, in which Core is silent too; the test reads rdashd's real debug.log. Each
//! scenario ends with a control that must log: a `Misbehaving` line whose score starts at 0 (so the
//! silent step scored nothing), or the very line, from Core's own next step. The node then stops
//! cleanly, so the log is complete when it is read. P3-notes D115/D116 say what each scenario
//! keeps of Core's, and why rows 161 and 167 stay linked. P4b adds, in the same harness, the
//! runtime proof of one expected line its serving brought in (row 220), and BL-327 the other
//! direction: the scripted peer dials the listening node. BL-327's `-maxconnections` tests run
//! the same peers against dashd v24.0.3 too (skipped with `SKIP_DASHD_TESTS`). The last
//! test is the runtime proof of two expected rows: `-blocksonly` (P5a D23).

mod common;

use std::io::{ErrorKind, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant, SystemTime};

use common::wire::Wire;
use common::{
    RDASHD, RpcNode, Running, debug_log, find_dashd, free_port, home_and_datadir, read, start,
};
use dash_consensus::header::HeaderParams;
use dash_consensus::pow::check_proof_of_work;
use dash_consensus::primitives::{Header, Tx};
use dash_consensus::{Block, core_serialize};
use dash_num::CompactTarget;
use dash_p2p_core::{GetData, Headers, InvType, PlatformBan};
use dash_p2p_transport::{
    CommandString, Magic, NetMessage, OutMsg, ProtocolVersion, TransportKind,
};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{BlockHeader, QuorumHash, Transaction, TxHash};
use dash_types::Hashable as _;
use dash_types::codec::BaseCodec as _;
use socket2::{Domain, Socket, Type};

/// How long one step may take on a box at load 50–190.
const PATIENCE: Duration = Duration::from_secs(300);
const REGTEST_BITS: u32 = 0x207f_ffff;
/// `NODE_NETWORK | NODE_BLOOM | NODE_NETWORK_LIMITED`: no `NODE_HEADERS_COMPRESSED`, so the node
/// asks with `getheaders` and takes `headers`.
const PEER_SERVICES: u64 = 0x1 | 0x4 | 0x400;
/// Core's `MAX_BLOOM_FILTER_SIZE` (test_framework/messages.py).
const MAX_BLOOM_FILTER_SIZE: usize = 36_000;
/// feature_llmq_dkg_intake.py's `FAKE_PUBKEY` and `INVALID_NONZERO_BLS_PUBKEY`.
const VALID_BLS_PUBKEY: &str = "8e7afdb849e5e2a085b035b62e21c0940c753f2d4501325743894c37162f287bccaffbedd60c36581dabbf127a22e43f";
const INVALID_NONZERO_BLS_PUBKEY: [u8; 48] = [0xff; 48];
/// Core's `Consensus::LLMQType::LLMQ_TEST`.
const LLMQ_TEST: u8 = 100;
/// The protocol version the scripted peer speaks (v24's `PROTOCOL_VERSION`).
const PROTOCOL_VERSION: i32 = 70242;

fn regtest() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest")
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).expect("now").as_secs()
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

/// The coinbase at `height`: a null prevout with the BIP34 push, one `OP_TRUE` output.
fn coinbase(height: u32) -> Arc<Tx> {
    let mut raw = 1i32.to_le_bytes().to_vec();
    raw.push(1);
    raw.extend([0u8; 32]);
    raw.extend(u32::MAX.to_le_bytes());
    let script_sig = [height_push(height), vec![1, 0x5a]].concat();
    raw.push(script_sig.len() as u8);
    raw.extend(script_sig);
    raw.extend(u32::MAX.to_le_bytes());
    raw.push(1);
    raw.extend(0i64.to_le_bytes());
    raw.extend([1, 0x51]);
    raw.extend(0u32.to_le_bytes());
    Arc::new(Tx::new(Transaction::decode(&mut raw.as_slice()).expect("a transaction")))
}

/// `n` regtest blocks on genesis, 150 s apart and ending now, so the last one takes the node out
/// of initial block download.
fn chain(p: &ChainSetup, n: u32) -> Vec<Arc<Block>> {
    let pow = *HeaderParams::from_chain_params(p).expect("regtest").pow();
    let first = unix_now() as u32 - 150 * n;
    let mut prev = Header::new(dash_consensus::params::genesis_block(p).header());
    (1..=n)
        .map(|h| {
            let txs = vec![coinbase(h)];
            let mut raw = {
                let base = BlockHeader::default();
                BlockHeader::new(
                    0x2000_0000,
                    prev.hash(),
                    base.merkle_root(),
                    first + 150 * h,
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
            Arc::new(Block::new(header, txs))
        })
        .collect()
}

fn headers(blocks: &[Arc<Block>]) -> Vec<u8> {
    let mut out = Vec::new();
    Headers::new(blocks.iter().map(|b| *b.header().raw()).collect()).encode(&mut out);
    out
}

/// Our `version` in the layout of Core's `PushNodeVersion`, with Dash's MNAUTH challenge
/// (zero) and masternode flag (false).
fn version_payload(start_height: i32) -> Vec<u8> {
    let mut out = PROTOCOL_VERSION.to_le_bytes().to_vec();
    out.extend(PEER_SERVICES.to_le_bytes());
    out.extend((unix_now() as i64).to_le_bytes());
    for _ in 0..2 {
        out.extend(PEER_SERVICES.to_le_bytes());
        out.extend([0; 18]);
    }
    out.extend(0x5eed_5eed_5eed_5eed_u64.to_le_bytes());
    let ua = b"/p3-silence:0.1/";
    out.push(ua.len() as u8);
    out.extend(ua);
    out.extend(start_height.to_le_bytes());
    out.push(1);
    out.extend([0; 32]);
    out.push(0);
    out
}

/// The node's next connection to `listener`, by `deadline`.
fn next_connection(listener: &TcpListener, deadline: Instant) -> TcpStream {
    listener.set_nonblocking(true).expect("nonblocking");
    let sock = loop {
        match listener.accept() {
            Ok((sock, _)) => break sock,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "the node never dialled");
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("accept: {e}"),
        }
    };
    sock.set_nonblocking(false).expect("blocking");
    sock.set_read_timeout(Some(Duration::from_millis(20))).expect("timeout");
    sock.set_nodelay(true).expect("nodelay");
    sock
}

/// The scripted peer's end of the connection the node dialled.
struct Peer {
    wire: Wire,
    /// The blocks it serves on `getdata`; its `version` starts at the last.
    blocks: Vec<Arc<Block>>,
}

impl Peer {
    /// The peer on `sock`, a V2 responder (which also takes V1) if `v2`, else V1.
    fn new(sock: TcpStream, v2: bool, blocks: &[Arc<Block>]) -> Peer {
        let magic = Magic::from_bytes(regtest().message_start);
        let wire = if v2 {
            Wire::v2(sock, magic)
        } else {
            Wire::v1(sock, magic)
        };
        Peer {
            wire,
            blocks: blocks.to_vec(),
        }
    }

    fn send(&mut self, command: &str, payload: Vec<u8>) {
        self.wire.send(command, payload, PATIENCE).expect("the connection");
    }

    /// The next message of type `command`, answering `ping`s and serving `getdata` meanwhile;
    /// any other message is dropped.
    fn expect(&mut self, command: &str) -> NetMessage {
        let deadline = Instant::now() + PATIENCE;
        loop {
            while let Some(m) = self.wire.inbox.pop_front() {
                if m.command.as_str() == command {
                    return m;
                }
                self.reply(m);
            }
            self.wire.pump().expect("the connection");
            assert!(Instant::now() < deadline, "no {command} from the node");
        }
    }

    /// Answers what has arrived.
    fn answer(&mut self) {
        self.wire.pump().expect("the connection");
        while let Some(m) = self.wire.inbox.pop_front() {
            self.reply(m);
        }
    }

    /// A `pong` per `ping`, the blocks a `getdata` asks for.
    fn reply(&mut self, m: NetMessage) {
        match m.command.as_str() {
            "ping" => self.send("pong", m.payload),
            "getdata" => {
                let gd = GetData::decode(&mut &m.payload[..]).expect("a getdata");
                for inv in gd.inventory().iter().filter(|i| i.inv_type() == InvType::Block) {
                    let found = self
                        .blocks
                        .iter()
                        .find(|b| b.hash().as_bytes() == inv.inv_hash().as_bytes());
                    if let Some(block) = found.map(|b| core_serialize(&**b)) {
                        self.send("block", block);
                    }
                }
            }
            _ => {}
        }
    }

    /// The `version`/`verack` exchange: the node dialled, so it speaks first. The common version
    /// then gates V2's short IDs, as in Core. Returns the node's `version` payload.
    fn handshake(&mut self) -> Vec<u8> {
        let theirs = self.expect("version");
        let version = i32::from_le_bytes(theirs.payload[..4].try_into().expect("4 bytes"));
        self.wire.t.set_peer_version(ProtocolVersion::new(version.min(PROTOCOL_VERSION)));
        self.send("version", version_payload(self.blocks.len() as i32));
        self.send("verack", Vec::new());
        self.expect("verack");
        theirs.payload
    }

    /// The exchange when the peer dialled: it speaks first, then takes the node's `version` and
    /// `verack`.
    fn handshake_inbound(&mut self) {
        self.send("version", version_payload(0));
        let theirs = self.expect("version");
        let version = i32::from_le_bytes(theirs.payload[..4].try_into().expect("4 bytes"));
        self.wire.t.set_peer_version(ProtocolVersion::new(version.min(PROTOCOL_VERSION)));
        self.expect("verack");
        self.send("verack", Vec::new());
    }

    /// Syncs the peer's blocks (serving the node's `getdata`s) until the node's tip is the last.
    fn sync(&mut self, datadir: &Path) {
        self.expect("getheaders");
        self.send("headers", headers(&self.blocks));
        let tip = format!(" height={} ", self.blocks.len());
        let deadline = Instant::now() + PATIENCE;
        while !whole_log(datadir)
            .lines()
            .any(|l| l.contains("UpdateTip: new best=") && l.contains(&tip))
        {
            assert!(
                Instant::now() < deadline,
                "the tip never reached {tip}: {}",
                whole_log(datadir)
            );
            self.answer();
        }
    }

    /// Core's `send_and_ping`: the message, then a `ping` whose `pong` shows the node has
    /// processed it (a peer's messages are processed in order).
    fn send_and_ping(&mut self, command: &str, payload: Vec<u8>) {
        self.send(command, payload);
        let nonce = 0x0123_4567_89ab_cdef_u64.to_le_bytes().to_vec();
        self.send("ping", nonce.clone());
        assert_eq!(self.expect("pong").payload, nonce);
    }
}

fn listen() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").expect("bind")
}

/// rdashd on regtest, dialling only `listener`, with `[net]` lines in its log; V1 unless `v2`.
fn dialling(home: &Path, datadir: &Path, listener: &TcpListener, v2: bool) -> Running {
    dialling_with(home, datadir, listener, v2, &[])
}

/// [`dialling`] with `extra` arguments.
fn dialling_with(
    home: &Path,
    datadir: &Path,
    listener: &TcpListener,
    v2: bool,
    extra: &[&str],
) -> Running {
    let connect = format!("-connect={}", listener.local_addr().expect("addr"));
    let mut args = vec![connect.as_str(), "-listen=0", "-dnsseed=0", "-fixedseeds=0", "-debug=net"];
    if !v2 {
        args.push("-v2transport=0");
    }
    args.extend(extra);
    start(home, datadir, &args)
}

/// rdashd dialling a peer that serves `blocks`, after the handshake and, if there are blocks, the
/// sync to the last of them.
fn connected(home: &Path, datadir: &Path, blocks: &[Arc<Block>], v2: bool) -> (Running, Peer) {
    let listener = listen();
    let node = dialling(home, datadir, &listener, v2);
    let mut peer = Peer::new(next_connection(&listener, Instant::now() + PATIENCE), v2, blocks);
    peer.handshake();
    if !blocks.is_empty() {
        peer.sync(datadir);
    }
    (node, peer)
}

fn whole_log(datadir: &Path) -> String {
    read(&debug_log(datadir))
}

/// Where the next step's lines start in the log.
fn mark(datadir: &Path) -> usize {
    whole_log(datadir).len()
}

/// Waits until the log, from `from`, holds `needle`.
fn wait_log(datadir: &Path, from: usize, needle: &str) {
    let deadline = Instant::now() + PATIENCE;
    while !whole_log(datadir).get(from..).is_some_and(|l| l.contains(needle)) {
        assert!(Instant::now() < deadline, "no {needle:?} in the log: {}", whole_log(datadir));
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Stops the node cleanly and returns everything it logged from `from`.
fn stop(node: Running, datadir: &Path, from: usize) -> String {
    node.stop_ok(libc::SIGTERM);
    whole_log(datadir).get(from..).expect("the log from the mark").to_owned()
}

fn lines<'a>(log: &'a str, fragment: &str) -> Vec<&'a str> {
    log.lines().filter(|l| l.contains(fragment)).collect()
}

/// The log's only `Misbehaving` line.
fn only_misbehaving(log: &str) -> &str {
    let misbehaving = lines(log, "Misbehaving");
    assert_eq!(misbehaving.len(), 1, "{log}");
    misbehaving[0]
}

/// The control after a silent step: an unrequested `mnlistdiff` scores 100 at once (Core:
/// net_processing.cpp:5654-5658). Its line must be the only `Misbehaving` since `from`, starting
/// at 0. The node then stops.
fn end_with_mnlistdiff_control(node: Running, datadir: &Path, peer: &mut Peer, from: usize) {
    peer.send("mnlistdiff", Vec::new());
    wait_log(datadir, from, "received not-requested mnlistdiff");
    let log = stop(node, datadir, from);
    assert!(
        only_misbehaving(&log).contains(
            "(0 -> 100) DISCOURAGE THRESHOLD EXCEEDED: received not-requested mnlistdiff"
        ),
        "{log}"
    );
}

/// Rows 262 (`Misbehaving`) and 263 (`non-continuous headers sequence`), p2p_invalid_messages.py
/// `test_noncontinuous_headers_msg` (:202): the node has ten blocks; a peer sends their ten
/// headers, continuous, and neither line appears. Core's next step is the control: one header
/// deleted from the middle logs both, scoring 20 from 0.
#[test]
fn continuous_headers_are_not_misbehaviour() {
    let (home, datadir) = home_and_datadir();
    let blocks = chain(&regtest(), 10);
    let (node, mut peer) = connected(home.path(), &datadir, &blocks, false);

    let from = mark(&datadir);
    peer.send_and_ping("headers", headers(&blocks));
    let mut gapped = blocks.clone();
    gapped.remove(5);
    peer.send_and_ping("headers", headers(&gapped));
    let log = stop(node, &datadir, from);
    assert!(only_misbehaving(&log).contains("(0 -> 20): non-continuous headers sequence"), "{log}");
    assert_eq!(lines(&log, "non-continuous headers sequence").len(), 1, "{log}");
}

/// Row 237 (`Misbehaving`), p2p_filter.py `test_size_limits` (:117): a `filterload` of exactly
/// `MAX_BLOOM_FILTER_SIZE` bytes is not misbehaviour. rdashd offers `NODE_BLOOM` as Core does
/// (`DEFAULT_PEERBLOOMFILTERS`), so Core's branch is the one taken (net_processing.cpp:5529-5556).
/// The filter itself is P4's (P2a §3.11): rdashd keeps none yet, so Core's previous step (one byte
/// over scores 100) cannot be the control; an unrequested `mnlistdiff` is.
#[test]
fn a_max_size_filterload_is_not_misbehaviour() {
    let (home, datadir) = home_and_datadir();
    let (node, mut peer) = connected(home.path(), &datadir, &[], false);
    let from = mark(&datadir);
    // Core's `msg_filterload(data=b'\xbb' * MAX_BLOOM_FILTER_SIZE)`: vData, nHashFuncs 0,
    // nTweak 0, nFlags 0.
    let mut filter = vec![0xfd];
    filter.extend((MAX_BLOOM_FILTER_SIZE as u16).to_le_bytes());
    filter.extend([0xbb; MAX_BLOOM_FILTER_SIZE]);
    filter.extend([0; 9]);
    peer.send_and_ping("filterload", filter);
    end_with_mnlistdiff_control(node, &datadir, &mut peer, from);
}

/// The node's next connection that opens with a V1 `version` header, read as a V1-only node
/// reads it, and how many came before it: those (the node's V2 key) are dropped unread, as such a
/// node drops them.
fn next_v1_connection(listener: &TcpListener) -> (Peer, usize) {
    let prefix = [&regtest().message_start[..], b"version\0\0\0\0\0"].concat();
    let deadline = Instant::now() + PATIENCE;
    for dropped in 0.. {
        let sock = next_connection(listener, deadline);
        let mut head = [0u8; 16];
        let v1 = loop {
            match sock.peek(&mut head) {
                Ok(n) if n >= head.len() => break head[..] == prefix[..],
                Ok(0) => break false,
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                    ) => {}
                Err(_) => break false,
            }
            assert!(Instant::now() < deadline, "the node never dialled over V1");
            std::thread::sleep(Duration::from_millis(10));
        };
        if v1 {
            return (Peer::new(sock, false, &[]), dropped);
        }
    }
    unreachable!()
}

/// Row 313 (`retrying with v1 transport protocol for peer`), p2p_v2_transport.py (:45): a V2 node
/// dials a V2 peer, syncs five blocks, and never falls back. The control is Core's own (:132): a
/// V1-only peer the node dials over V2 makes it log the line once and retry over V1, once.
#[test]
fn a_v2_dial_to_a_v2_peer_does_not_fall_back_to_v1() {
    let (home, datadir) = home_and_datadir();
    let (node, peer) = connected(home.path(), &datadir, &chain(&regtest(), 5), true);
    assert_eq!(peer.wire.t.info().kind, TransportKind::V2);
    let log = stop(node, &datadir, 0);
    assert!(lines(&log, "retrying with v1 transport protocol for peer").is_empty(), "{log}");

    let (home, datadir) = home_and_datadir();
    let listener = listen();
    let node = dialling(home.path(), &datadir, &listener, true);
    let (mut peer, dropped) = next_v1_connection(&listener);
    peer.handshake();
    let log = stop(node, &datadir, 0);
    assert_eq!(dropped, 1, "{log}");
    assert_eq!(lines(&log, "retrying with v1 transport protocol for peer").len(), 1, "{log}");
}

/// Row 161 (`malformed DKG message`), feature_llmq_dkg_intake.py
/// `test_malformed_bls_pubkey_rejected_by_worker` (:273): a QCONTRIB whose verification vector
/// holds a nonzero invalid BLS key is not a malformed DKG message. Core's scenario is a verified
/// masternode peer whose requested contribution reaches the DKG worker. rdashd has no masternode
/// role, so the same payload meets Core's `NetDKGStub` (llmq/net_dkg.cpp:938-944), where Core is
/// silent too and scores 10 with no text: that line is the control. The worker branch is not
/// reachable, so the row stays linked until DK1 (BL-264, DEC-282); this stand-in stays as a
/// regression test.
#[test]
fn a_qcontrib_with_an_invalid_bls_key_is_not_a_malformed_dkg_message() {
    let (home, datadir) = home_and_datadir();
    let (node, mut peer) = connected(home.path(), &datadir, &[], false);
    let from = mark(&datadir);
    // feature_llmq_dkg_intake.py `qcontrib_payload(blob_count=2, vvec_pubkey=INVALID_...)`.
    let valid: Vec<u8> = (0..VALID_BLS_PUBKEY.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&VALID_BLS_PUBKEY[i..i + 2], 16).expect("hex"))
        .collect();
    let mut qcontrib = vec![LLMQ_TEST];
    qcontrib.extend([0x5a; 32]);
    qcontrib.extend([0; 32]);
    qcontrib.push(2);
    qcontrib.extend(INVALID_NONZERO_BLS_PUBKEY);
    qcontrib.extend(&valid);
    qcontrib.extend(&valid);
    qcontrib.extend([0; 32]);
    qcontrib.push(2);
    for _ in 0..2 {
        qcontrib.push(32);
        qcontrib.extend([0; 32]);
    }
    qcontrib.extend([0; 96]);
    peer.send_and_ping("qcontrib", qcontrib);
    let log = stop(node, &datadir, from);
    assert!(only_misbehaving(&log).ends_with("(0 -> 10)"), "{log}");
    assert!(lines(&log, "malformed DKG message").is_empty(), "{log}");
    assert!(lines(&log, "unrequested DKG message").is_empty(), "{log}");
}

/// In today's rdashd composition, the inert `NoTxPool` logs no rejection for a transaction sent
/// during initial block download. The handler-level pool regression lives in dashd-p2p.
#[test]
fn inert_pool_transaction_during_ibd_is_silent() {
    let (home, datadir) = home_and_datadir();
    let (node, mut peer) = connected(home.path(), &datadir, &[], false);

    let from = mark(&datadir);
    // Core's p2p_ibd_txrelay.py transaction from tx_valid.json: well-formed, but no UTXO exists.
    let rawhex = "0100000001b14bdcbc3e01bdaad36cc08e81e69c82e1060bc14e518db2b49aa43ad90ba260000000004a01ff47304402203f16c6f40162ab686621ef3000b04e75418a0c0cb2d8aebeac894ae360ac1e780220ddc15ecdfc3507ac48e1681a33eb60996631bf6bf5bc0a0682c4db743ce7ca2b01ffffffff0140420f00000000001976a914660d4ef3a743e3e696ad990364e555c271ad504b88ac00000000";
    let tx = (0..rawhex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&rawhex[i..i + 2], 16).expect("hex"))
        .collect();
    peer.send_and_ping("tx", tx);
    let log = stop(node, &datadir, from);
    assert_eq!(lines(&log, "received: tx").len(), 1, "{log}");
    assert!(lines(&log, "was not accepted").is_empty(), "{log}");
}

/// Row 46 (`Misbehaving`), feature_asset_locks.py (:535): out of initial block download, a peer
/// relays an asset unlock the mempool refuses, and it is not misbehaviour. Core's test refuses it
/// as `bad-assetunlock-too-late`; at this ten-block regtest tip DIP3 is inactive, so Core refuses
/// this one as `bad-tx-type-dip3-inactive` (`TX_BAD_SPECIAL`, which `MaybePunishNodeForTx` does
/// not punish, net_processing.cpp:2041): dashd v24.0.0-rc.3's `testmempoolaccept` gives that
/// reason for these bytes at this tip (D116). rdashd has no mempool until P5/EV and takes `tx`
/// unread, so the reject line is not reachable; an unrequested `mnlistdiff` is the control.
#[test]
fn an_asset_unlock_relayed_out_of_ibd_is_not_misbehaviour() {
    let (home, datadir) = home_and_datadir();
    let (node, mut peer) = connected(home.path(), &datadir, &chain(&regtest(), 10), false);
    wait_log(&datadir, 0, "Leaving InitialBlockDownload (latching to false)");

    let from = mark(&datadir);
    // feature_asset_locks.py `create_assetunlock`: nVersion 3, nType 9, no inputs, one P2PK
    // output, a CAssetUnlockTx payload (version 1, index, fee, requestedHeight, quorumHash, a
    // zero quorumSig).
    let mut unlock = (3u32 | (9 << 16)).to_le_bytes().to_vec();
    unlock.extend([0, 1]);
    unlock.extend((100_000_000i64 - 1_000).to_le_bytes());
    unlock.extend([35, 33, 0x02]);
    unlock.extend([0x11; 32]);
    unlock.push(0xac);
    unlock.extend(0u32.to_le_bytes());
    let mut payload = vec![1];
    payload.extend(101u64.to_le_bytes());
    payload.extend(1_000u32.to_le_bytes());
    payload.extend(1u32.to_le_bytes());
    payload.extend([0x5a; 32]);
    payload.extend([0; 96]);
    unlock.push(payload.len() as u8);
    unlock.extend(payload);
    peer.send_and_ping("tx", unlock);
    end_with_mnlistdiff_control(node, &datadir, &mut peer, from);
}

/// The node as composed today installs inert platform-ban and quorum seams, so it ignores a
/// `platformban`: neither the first send nor an identical resend logs `PLATFORMBAN -- hash:` or
/// scores the peer. This checks that composition only; the handler's dedupe is covered by
/// dashd-p2p's sim tests.
#[test]
fn an_inert_node_ignores_a_platform_ban_and_its_resend() {
    let (home, datadir) = home_and_datadir();
    let (node, mut peer) = connected(home.path(), &datadir, &chain(&regtest(), 10), false);
    wait_log(&datadir, 0, "Leaving InitialBlockDownload (latching to false)");

    let ban =
        PlatformBan::new(TxHash::from([0x11; 32]), 0, QuorumHash::from([0x22; 32]), [0; 96].into());
    let mut payload = Vec::new();
    ban.encode(&mut payload);
    let needle = format!("PLATFORMBAN -- hash: {}", ban.hash());
    let from = mark(&datadir);
    peer.send_and_ping("platformban", payload.clone());
    let after_first = mark(&datadir);
    peer.send_and_ping("platformban", payload);
    let log = stop(node, &datadir, from);
    assert!(lines(&log, &needle).is_empty(), "{log}");
    assert!(lines(&log, "Misbehaving").is_empty(), "{log}");
    assert!(whole_log(&datadir)[after_first..].find(&needle).is_none(), "resend was logged: {log}");
}

/// Row 220 (`getblocktxn with out-of-bounds tx indices`), p2p_compactblocks.py
/// `test_getblocktxn_handler` (:645), an expected line (P4b): the node has ten blocks; a peer
/// asks `getblocktxn` for indexes 0 and 1 of the tip, which has one transaction, and Core's
/// `SendBlockTransactions` scores 100 with the line (net_processing.cpp:3113-3124).
#[test]
fn an_out_of_bounds_getblocktxn_is_misbehaviour() {
    let (home, datadir) = home_and_datadir();
    let blocks = chain(&regtest(), 10);
    let (node, mut peer) = connected(home.path(), &datadir, &blocks, false);

    let from = mark(&datadir);
    // BIP152's differential indexes: a count of 2, then 0 and 0 (indexes 0 and 1).
    let mut request = blocks[9].hash().as_bytes().to_vec();
    request.extend([2, 0, 0]);
    peer.send("getblocktxn", request);
    wait_log(&datadir, from, "getblocktxn with out-of-bounds tx indices");
    let log = stop(node, &datadir, from);
    assert!(
        only_misbehaving(&log).contains(
            "(0 -> 100) DISCOURAGE THRESHOLD EXCEEDED: getblocktxn with out-of-bounds tx indices"
        ),
        "{log}"
    );
}

/// p2p_dstx.py's DSTX (`make_dstx`): three inputs whose prevouts carry `nonce`, three P2PKH
/// outputs of the smallest denomination, masternode proTxHash 1 and a dummy signature.
fn dstx(nonce: u64, first_output_extra: i64) -> (Vec<u8>, TxHash) {
    use dash_primitives::{OutPoint, TxIn, TxOut, TxType};
    let mut p2pkh = vec![0x76, 0xa9, 0x14];
    p2pkh.extend([1; 20]);
    p2pkh.extend([0x88, 0xac]);
    let prevout = |i: u64| {
        let mut hash = [0; 32];
        hash[..8].copy_from_slice(&((nonce << 8) | (i + 1)).to_le_bytes());
        OutPoint::new(TxHash::from(hash), 0)
    };
    let inputs = (0..3).map(|i| TxIn::new(prevout(i), Vec::new().into(), u32::MAX)).collect();
    let outputs = (0..3)
        .map(|n| {
            let extra = if n == 0 {
                first_output_extra
            } else {
                0
            };
            TxOut::from_duffs(100_000 + 1 + extra, p2pkh.clone().into())
        })
        .collect();
    let tx = Transaction::new(1, TxType::Spend, inputs, outputs, 0, Vec::new());
    let txid = Tx::new(tx.clone()).txid();
    let mut pro = [0; 32];
    pro[0] = 1;
    let sig_time = i64::try_from(unix_now()).expect("a time");
    let mut payload = Vec::new();
    dash_p2p_core::DsTx::new(tx, TxHash::from(pro), vec![1; 96], sig_time).encode(&mut payload);
    (payload, txid)
}

/// Rows 229 (`Can't find masternode`) and 233 (`Invalid DSTX structure`), p2p_dstx.py (:101,
/// :113), and row 52 (feature_asset_locks.py :1208, the same structure line): out of initial
/// block download, a peer sends the test's DSTX naming a masternode the list does not have, then
/// one whose first output is no denomination. rdashd logs Core's `ValidateDSTX` lines
/// (net_processing.cpp:3661, :3696 at v24.0.2), with Core's null `masternodeOutpoint`, and
/// scores 1, then 10.
#[test]
fn a_dstx_from_an_unknown_masternode_or_of_a_bad_structure_logs_cores_line() {
    let (home, datadir) = home_and_datadir();
    let listener = listen();
    let node = dialling_with(home.path(), &datadir, &listener, false, &["-debug=coinjoin"]);
    let blocks = chain(&regtest(), 10);
    let mut peer = Peer::new(next_connection(&listener, Instant::now() + PATIENCE), false, &blocks);
    peer.handshake();
    peer.sync(&datadir);
    wait_log(&datadir, 0, "Leaving InitialBlockDownload (latching to false)");

    let from = mark(&datadir);
    let (unknown, unknown_txid) = dstx(1, 0);
    peer.send_and_ping("dstx", unknown);
    let null = "0000000000000000000000000000000000000000000000000000000000000000-4294967295";
    let unknown_line = format!("DSTX -- Can't find masternode {null} to verify {unknown_txid}");
    wait_log(&datadir, from, &unknown_line);
    let (bad, bad_txid) = dstx(2, 1);
    peer.send_and_ping("dstx", bad);
    let bad_line = format!("DSTX -- Invalid DSTX structure: {bad_txid}");
    wait_log(&datadir, from, &bad_line);
    let log = stop(node, &datadir, from);
    let scores: Vec<&str> = lines(&log, "Misbehaving");
    assert_eq!(scores.len(), 2, "{log}");
    assert!(scores[0].contains("(0 -> 1)") && scores[0].contains("invalid dstx"), "{log}");
    assert!(scores[1].contains("(1 -> 11)") && scores[1].contains("invalid dstx"), "{log}");
    assert_eq!(lines(&log, "Can't find masternode").len(), 1, "{log}");
    assert_eq!(lines(&log, "Invalid DSTX structure").len(), 1, "{log}");
}

/// The scripted V1 peer's connection to the node listening on 127.0.0.1:`port`, after the
/// handshake.
fn dial(port: u16) -> Peer {
    let sock = TcpStream::connect(("127.0.0.1", port)).expect("the node listens");
    sock.set_read_timeout(Some(Duration::from_millis(20))).expect("timeout");
    sock.set_nodelay(true).expect("nodelay");
    let mut peer = Peer::new(sock, false, &[]);
    peer.handshake_inbound();
    peer
}

/// BL-327: `-bind` turns listening on despite `-connect=0` (`-bind` is read first), the node
/// binds the address (a `LogPrintf` line, without a category), skips `Discover` (it binds on no
/// wildcard), and a peer that dials it gets the responder's handshake and its pings answered.
// Core: init.cpp:1004-1007, :1013-1019, :2641-2651, :2708-2709, net.cpp:3823 BindListenPort
#[test]
fn a_peer_dials_the_listening_node() {
    let (home, datadir) = home_and_datadir();
    let port = free_port();
    let bind = format!("-bind=127.0.0.1:{port}");
    let args = [bind.as_str(), "-listenonion=0", "-connect=0", "-dnsseed=0", "-debug=net"];
    let node = start(home.path(), &datadir, &args);
    let mut peer = dial(port);
    let from = mark(&datadir);
    peer.send_and_ping("sendheaders", Vec::new());
    let log = stop(node, &datadir, 0);
    for line in [
        "InitParameterInteraction: parameter interaction: -bind set -> setting -listen=1\n".into(),
        format!("Bound to 127.0.0.1:{port}\n"),
    ] {
        assert!(log.contains(&line), "no {line:?}: {log}");
    }
    assert!(!log.contains("-connect or -maxconnections=0 set -> setting -listen=0"), "{log}");
    assert!(!log.contains("[net:info] Bound to") && !log.contains("Discover: "), "{log}");
    assert!(log[from..].contains("received: sendheaders"), "{log}");
}

/// The `permissions` `getpeerinfo` lists for a peer dialling `binary` (rdashd or dashd) on
/// 127.0.0.1, which listens on `-whitebind` if `whitebind` (with `listen` as its permissions)
/// or else on `-bind`, with `-blocksonly` set to `blocks_only` and `extra`. A `-whitebind` alone
/// gets Core's default onion target (127.0.0.1 on the chain's onion port, fixed), so it is given
/// an onion bind of its own.
fn permissions_of(
    binary: &Path,
    blocks_only: bool,
    whitebind: Option<&str>,
    extra: &[&str],
) -> String {
    let (home, datadir) = home_and_datadir();
    let port = free_port();
    let (listen, onion) = match whitebind {
        Some(perms) => (
            format!("-whitebind={perms}127.0.0.1:{port}"),
            format!("-bind=127.0.0.1:{}=onion", free_port()),
        ),
        None => (format!("-bind=127.0.0.1:{port}"), "-listenonion=0".to_owned()),
    };
    let blocks_only = format!("-blocksonly={}", u8::from(blocks_only));
    let args =
        [&[&*listen, &*onion, "-listenonion=0", "-connect=0", &*blocks_only], extra].concat();
    let node = RpcNode::start_on(binary, home.path(), &datadir, &args);
    let mut peer = try_dial(port).expect("a slot");
    peer.send_and_ping("sendheaders", Vec::new());
    let info = node.call("getpeerinfo", "[]").result().to_owned();
    node.node.stop_ok(libc::SIGTERM);
    let from = info.find(r#""permissions":"#).unwrap_or_else(|| panic!("{info}"));
    info[from..=from + info[from..].find(']').unwrap()].to_owned()
}

/// BL-327 D24: `-whitelist` and `-whitebind` reach the inbound peer's permissions as dashd
/// gives them. An entry without permissions is implicit: `mempool` and `noban` (which implies
/// `download`), `relay` under `-whitelistrelay` (on by default) and `forcerelay` under
/// `-whitelistforcerelay`, which turns `-whitelistrelay` on. `getpeerinfo` lists them in
/// `ToStrings` order. An entry with permissions takes neither flag. The cases run with Core's
/// default `-blocksonly=0` but the last two, which run under `-blocksonly=1` (P5a D23): it turns
/// `-whitelistrelay` off first, so `-whitelistforcerelay` cannot turn it on, and `forcerelay`
/// alone still lists `relay` (its flag includes `Relay`'s).
// Core: init.cpp:1058-1061, :1068-1071, :2708-2712, net.cpp:1880-1891,
// net_permissions.cpp:24-124, net_permissions.h:26
#[test]
fn whitelisted_peers_have_dashd_s_permissions() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    let cases: [(bool, Option<&str>, &[&str], &str); 8] = [
        (false, None, &["-whitelist=noban,relay@127.0.0.1"], r#"["noban","relay","download"]"#),
        (false, None, &["-whitelist=127.0.0.1"], r#"["noban","relay","mempool","download"]"#),
        (
            false,
            None,
            &["-whitelist=127.0.0.1", "-whitelistrelay=0"],
            r#"["noban","mempool","download"]"#,
        ),
        (
            false,
            None,
            &["-whitelist=127.0.0.1", "-whitelistforcerelay=1"],
            r#"["noban","forcerelay","relay","mempool","download"]"#,
        ),
        (false, Some(""), &[], r#"["noban","relay","mempool","download"]"#),
        // An explicit entry takes neither flag.
        (false, Some("bloomfilter@"), &["-whitelistforcerelay=1"], r#"["bloomfilter"]"#),
        (true, None, &["-whitelist=127.0.0.1"], r#"["noban","mempool","download"]"#),
        (
            true,
            None,
            &["-whitelist=127.0.0.1", "-whitelistforcerelay=1"],
            r#"["noban","forcerelay","relay","mempool","download"]"#,
        ),
    ];
    for (blocks_only, whitebind, extra, want) in cases {
        let want = format!(r#""permissions":{want}"#);
        let case = format!("-blocksonly={} {whitebind:?} {extra:?}", u8::from(blocks_only));
        assert_eq!(permissions_of(&dashd, blocks_only, whitebind, extra), want, "dashd {case}");
        let rdashd = permissions_of(Path::new(RDASHD), blocks_only, whitebind, extra);
        assert_eq!(rdashd, want, "rdashd {case}");
    }
}

/// The mock time of the `-maxconnections` runs. Core's eviction sorts on connection time in whole
/// seconds; frozen, every peer ties, so both daemons break the ties by the sort alone.
const LIMIT_MOCKTIME: &str = "-mocktime=1700000000";

/// What a node did with peers dialling it one at a time: whether each still has its connection,
/// the ids `getpeerinfo` lists, and the node's limit lines (eviction, drop, disconnect, removal)
/// without their timestamps.
#[derive(Debug, PartialEq)]
struct LimitRun {
    connected: Vec<bool>,
    ids: Vec<u64>,
    lines: Vec<String>,
}

/// The V1 handshake with the node on 127.0.0.1:`port`, or `None` when the node closes the
/// connection first (it dropped the peer).
fn try_dial(port: u16) -> Option<Peer> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let socket = Socket::new(Domain::IPV4, Type::STREAM, None).expect("a socket");
    socket.connect(&addr.into()).expect("the node listens");
    let sock = TcpStream::from(socket);
    sock.set_read_timeout(Some(Duration::from_millis(20))).expect("timeout");
    let mut peer = Peer::new(sock, false, &[]);
    let version = OutMsg::new(CommandString::from_static("version"), version_payload(0));
    assert!(peer.wire.t.set_message_to_send(version).is_ok());
    let deadline = Instant::now() + PATIENCE;
    let (mut version, mut verack) = (false, false);
    while !(version && verack) {
        peer.wire.pump().ok()?;
        while let Some(m) = peer.wire.inbox.pop_front() {
            match m.command.as_str() {
                "version" => version = true,
                "verack" => verack = true,
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "no handshake and no close");
    }
    peer.send("verack", Vec::new());
    Some(peer)
}

/// Whether the node has closed `peer`'s connection: reads (and drops) what is pending until the
/// end of the stream, or until nothing more arrives.
fn closed(peer: &mut Peer) -> bool {
    for _ in 0..5 {
        if peer.wire.pump().is_err() {
            return true;
        }
        peer.wire.inbox.clear();
    }
    false
}

/// The `id`s of a `getpeerinfo` result (Core puts `id` first in each peer's object).
fn peer_ids(info: &str) -> Vec<u64> {
    let mut ids: Vec<u64> = info
        .split(r#"{"id":"#)
        .skip(1)
        .map(|p| p[..p.find(|c: char| !c.is_ascii_digit()).unwrap()].parse().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

/// `binary` (rdashd or dashd) with `-maxconnections=max`, dialled by `peers` peers in turn; each
/// dial ends with the handshake or the node closing the connection. The node's limit lines are
/// read once `getpeerinfo` is within the limit, before it stops. dashd's "removing node" line
/// has an `nRefCount`, which rdashd has no counterpart for (BL-327 D14); it is cut.
// Core: net.cpp:1830-1846, :1881-1883, :1948-1961, :2150-2154
fn limit_run(binary: &Path, max: u32, peers: usize) -> LimitRun {
    let (home, datadir) = home_and_datadir();
    let port = free_port();
    let bind = format!("-bind=127.0.0.1:{port}");
    let maxconn = format!("-maxconnections={max}");
    let args = [
        &*bind,
        "-listenonion=0",
        "-connect=0",
        "-dnsseed=0",
        "-debug=net",
        &*maxconn,
        LIMIT_MOCKTIME,
    ];
    let node = RpcNode::start_on(binary, home.path(), &datadir, &args);
    let from = mark(&datadir);
    let mut dialled: Vec<Option<Peer>> = (0..peers).map(|_| try_dial(port)).collect();
    // An evicted peer leaves the table after the new one is in it.
    let deadline = Instant::now() + PATIENCE;
    let ids = loop {
        let ids = peer_ids(node.call("getpeerinfo", "[]").result());
        let log = whole_log(&datadir);
        if !log[from..].contains("for eviction") || log[from..].contains("removing node: peer=") {
            break ids;
        }
        assert!(Instant::now() < deadline, "the evicted peer was never removed: {log}");
        std::thread::sleep(Duration::from_millis(50));
    };
    let connected = dialled.iter_mut().map(|p| p.as_mut().is_some_and(|p| !closed(p))).collect();
    let log = whole_log(&datadir)[from..].to_owned();
    node.node.stop_ok(libc::SIGTERM);
    LimitRun {
        connected,
        ids,
        lines: limit_lines(&log),
    }
}

/// The limit lines (eviction, drop, disconnect, removal) of `log`, without their timestamps
/// and without dashd's `nRefCount` (BL-327 D14).
fn limit_lines(log: &str) -> Vec<String> {
    let limit = ["connection dropped", "for eviction", "disconnecting peer=", "removing node:"];
    log.lines()
        .filter(|l| limit.iter().any(|k| l.contains(k)))
        .map(|l| {
            let line = l.split_once(") ").map_or(l, |(_, rest)| rest);
            line.split(' ').filter(|w| !w.starts_with("nRefCount=")).collect::<Vec<_>>().join(" ")
        })
        .collect()
}

/// What a node with two listening sockets did with peers dialling both at once: how many
/// still have their connection, the ids `getpeerinfo` lists at the end, the most it ever
/// listed while the peers dialled, how many "connection accepted" lines it logged, and its
/// limit lines.
#[derive(Debug, PartialEq)]
struct BurstRun {
    connected: usize,
    ids: Vec<u64>,
    most_listed: usize,
    accepted: usize,
    lines: Vec<String>,
}

/// `binary` (rdashd or dashd) with `-maxconnections=max`, bound on two 127.0.0.1 ports, and
/// `per_port` peers dialling each port, all released together. rdashd accepts on each
/// listening socket in its own task; dashd's one socket thread takes both sockets in turn.
// Core: net.cpp:1881-1883, :1948-1961, :2009-2012, :2579-2589
fn burst_run(binary: &Path, max: u32, per_port: usize) -> BurstRun {
    let (home, datadir) = home_and_datadir();
    let ports = [free_port(), free_port()];
    let binds = ports.map(|p| format!("-bind=127.0.0.1:{p}"));
    let maxconn = format!("-maxconnections={max}");
    let args = [
        &*binds[0],
        &*binds[1],
        "-listenonion=0",
        "-connect=0",
        "-dnsseed=0",
        "-debug=net",
        &*maxconn,
        LIMIT_MOCKTIME,
    ];
    let node = RpcNode::start_on(binary, home.path(), &datadir, &args);
    let from = mark(&datadir);
    let dials = 2 * per_port;
    let go = Barrier::new(dials);
    let done = AtomicBool::new(false);
    let (mut dialled, most_listed) = std::thread::scope(|s| {
        // Polls without pause, to catch a second peer however briefly it is listed. Its last
        // call starts after every dial is decided: a node logs "connection accepted" before it
        // lists the peer, so a call in flight then can miss the kept one (BL-366).
        let sampler = s.spawn(|| {
            let mut most = 0;
            loop {
                let last = done.load(Ordering::SeqCst);
                most = most.max(peer_ids(node.call("getpeerinfo", "[]").result()).len());
                if last {
                    break;
                }
            }
            most
        });
        let dialers: Vec<_> = (0..dials)
            .map(|i| {
                let (go, port) = (&go, ports[i % 2]);
                s.spawn(move || {
                    go.wait();
                    try_dial(port)
                })
            })
            .collect();
        let dialled: Vec<Option<Peer>> = dialers.into_iter().map(|d| d.join().unwrap()).collect();
        // Every dial ends in one accept or one drop line.
        let deadline = Instant::now() + PATIENCE;
        loop {
            let log = whole_log(&datadir);
            let decided = log[from..]
                .lines()
                .filter(|l| l.contains("connection accepted") || l.contains("connection dropped"))
                .count();
            if decided >= dials {
                break;
            }
            assert!(Instant::now() < deadline, "{decided} of {dials} dials decided: {log}");
            std::thread::sleep(Duration::from_millis(50));
        }
        done.store(true, Ordering::SeqCst);
        (dialled, sampler.join().unwrap())
    });
    let ids = peer_ids(node.call("getpeerinfo", "[]").result());
    let connected = dialled.iter_mut().flatten().map(|p| !closed(p)).filter(|&c| c).count();
    let log = whole_log(&datadir)[from..].to_owned();
    node.node.stop_ok(libc::SIGTERM);
    BurstRun {
        connected,
        ids,
        most_listed,
        accepted: log.lines().filter(|l| l.contains("connection accepted")).count(),
        lines: limit_lines(&log),
    }
}

/// BL-327 D16: one inbound slot (`-maxconnections=12` less Core's 8 full-relay, 2 block-relay
/// and 1 feeler outbound slots). The first peer takes it. With one candidate, Core's eviction
/// protects it (four by netgroup), so each later peer is dropped, as dashd drops it.
// Core: net.cpp:1881-1883, :1948-1961, node/eviction.cpp:178-208
#[test]
fn the_inbound_limit_drops_a_peer_as_dashd_does() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    let want = LimitRun {
        connected: vec![true, false, false],
        ids: vec![0],
        lines: vec![
            "[net|netconn] failed to find an eviction candidate - connection dropped (full)"
                .into();
            2
        ],
    };
    assert_eq!(limit_run(&dashd, 12, 3), want, "dashd");
    assert_eq!(limit_run(Path::new(RDASHD), 12, 3), want, "rdashd");
}

/// BL-327 D16: 21 inbound slots (`-maxconnections=32`), filled; the 22nd peer is accepted after
/// one inbound peer is evicted. Of 21 candidates, 20 are protected outright (4 by netgroup, 8 by
/// ping, 4 by transaction and 4 by block time), and the ratio step protects half of the one
/// left, none. At frozen mock time every key ties, so which peer is left is the sort's (Core's
/// `std::sort`, which dash-p2p ports). dashd evicts the 14th peer (id 13), and so does rdashd.
// Core: net.cpp:1830-1846, :1948-1961, :2150-2154, node/eviction.cpp:178-239
#[test]
fn the_inbound_limit_evicts_a_peer_as_dashd_does() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    let want = LimitRun {
        connected: (0..22).map(|i| i != 13).collect(),
        ids: (0..22).filter(|&i| i != 13).collect(),
        lines: vec![
            "[net|netconn] selected inbound connection for eviction peer=13; disconnecting".into(),
            "ThreadSocketHandler -- removing node: peer=13 fInbound=1 m_masternode_connection=0 \
             m_masternode_iqr_connection=0"
                .into(),
            "[net] disconnecting peer=13".into(),
        ],
    };
    assert_eq!(limit_run(&dashd, 32, 22), want, "dashd");
    assert_eq!(limit_run(Path::new(RDASHD), 32, 22), want, "rdashd");
}

/// BL-327 D20: one inbound slot, two listening sockets, and four peers dialling each socket at
/// once. dashd decides the eight on its one socket thread; rdashd in an accept task per
/// socket, under the admission lock. Each keeps one peer and drops seven, never listing more
/// than one. Which dial wins is timing (the kernel's and the scheduler's), so the test compares
/// counts, the final ids (the kept peer is the first created, id 0) and the lines, not which
/// socket stayed open.
// Core: net.cpp:1881-1883, :1948-1961, :2579-2589
#[test]
fn simultaneous_dials_on_two_sockets_take_one_slot_as_dashd_does() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    let want = BurstRun {
        connected: 1,
        ids: vec![0],
        most_listed: 1,
        accepted: 1,
        lines: vec![
            "[net|netconn] failed to find an eviction candidate - connection dropped (full)"
                .into();
            7
        ],
    };
    assert_eq!(burst_run(&dashd, 12, 4), want, "dashd");
    assert_eq!(burst_run(Path::new(RDASHD), 12, 4), want, "rdashd");
}

/// What the node of `binary` (rdashd or dashd) has read from a peer that writes `ping`s and
/// never reads, once it has stopped reading (`getpeerinfo`'s `bytesrecv`, so the kernel's
/// buffers do not count), and the `pong` bytes it has sent by then: two readings taken 5 s of
/// stalled writes apart agree. A reading is an RPC the node answers, so a node that merely did
/// not run for 5 s under load (BL-366) is read again after it resumes. The node's `pong`s fill
/// the peer's receive window and then the node's send buffer, past `-maxsendbuffer`
/// (`fPauseSend`), so it stops processing the peer's messages; their queue passes
/// `-maxreceivebuffer` (`fPauseRecv`), so it stops reading.
///
/// The peer keeps the kernel's receive buffer (BL-545). With a 4 KiB `SO_RCVBUF` its receive
/// memory could pass the locked buffer and stay there, as it never reads; it then dropped every
/// segment from the node, the ACKs answering its zero-window probes among them. So when a slow
/// node's queue reached the limit before its send buffer filled, the peer never saw the node's
/// window reopen, and its writes stalled while the node read on an empty socket below the limit.
// Core: net.cpp:507, :1765-1769, :4900-4903, net_processing.cpp ProcessMessages (fPauseSend)
fn read_before_paused(binary: &Path, extra: &[&str]) -> Paused {
    let (home, datadir) = home_and_datadir();
    let port = free_port();
    let bind = format!("-bind=127.0.0.1:{port}");
    let args = [&[&*bind, "-listenonion=0", "-connect=0", "-dnsseed=0"], extra].concat();
    let node = RpcNode::start_on(binary, home.path(), &datadir, &args);
    let mut peer = try_dial(port).expect("a slot");
    let mut pings = Vec::new();
    for n in 0..1000u64 {
        let msg = OutMsg::new(CommandString::from_static("ping"), n.to_le_bytes().to_vec());
        assert!(peer.wire.t.set_message_to_send(msg).is_ok());
        loop {
            let data = peer.wire.t.bytes_to_send(false).data.to_vec();
            if data.is_empty() {
                break;
            }
            peer.wire.t.mark_bytes_sent(data.len());
            pings.extend(data);
        }
    }
    peer.wire.sock.set_nonblocking(true).expect("nonblocking");
    let mut at = 0;
    let mut read = None;
    let info = loop {
        let mut progress = Instant::now();
        while progress.elapsed() < Duration::from_secs(5) {
            match peer.wire.sock.write(&pings[at..]) {
                Ok(n) => {
                    at = (at + n) % pings.len();
                    progress = Instant::now();
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("the node closed the connection: {e}"),
            }
        }
        let info = node.call("getpeerinfo", "[]").result().to_owned();
        let now = number(&info, r#""bytesrecv":"#);
        if read == Some(now) {
            break info;
        }
        read = Some(now);
    };
    let sent = &info[info.find(r#""bytessent_per_msg":"#).expect("bytessent_per_msg")..];
    let paused = Paused {
        read: read.unwrap(),
        pongs: number(sent, r#""pong":"#),
    };
    node.node.stop_ok(libc::SIGTERM);
    paused
}

/// [`read_before_paused`]'s reading.
#[derive(Clone, Copy, Debug)]
struct Paused {
    /// `bytesrecv`.
    read: u64,
    /// `bytessent_per_msg`'s `pong` in the same reply: what the kernel took of the node's
    /// answers.
    pongs: u64,
}

impl Paused {
    /// What the node read and has not answered into the kernel: the pings it holds unprocessed
    /// (its receive queue) and those whose `pong`s wait in its send queue, which
    /// `-maxsendbuffer` bounds the same at either receive limit. A `ping` and its `pong` take
    /// the same bytes on the wire. How much the kernel takes varies by node and load, so this
    /// leaves it out.
    fn held(self) -> u64 {
        self.read.saturating_sub(self.pongs)
    }
}

/// The digits after the first `key` in `json`.
fn number(json: &str, key: &str) -> u64 {
    let field = json.split(key).nth(1).unwrap_or_else(|| panic!("no {key} in {json}"));
    field[..field.find(|c: char| !c.is_ascii_digit()).unwrap()].parse().unwrap()
}

/// BL-327 D15: `-maxreceivebuffer=100` pauses the reader of a peer whose messages wait, as in
/// dashd: it holds 4.9 MB less than at the default 5,000, Core's `(5000 - 100) * 1000`, give or
/// take its 64 KiB reads. (`-maxsendbuffer`'s pause is the unit test's and D15's: Core's handler
/// polls it every 100 ms, so how fast a paused peer drains differs, not the limit.)
// Core: init.cpp:2617-2618, net.cpp:4806-4850 PollMessage
#[test]
fn the_receive_limit_pauses_a_reader_as_dashd_does() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    for binary in [dashd.as_path(), Path::new(RDASHD)] {
        let default = read_before_paused(binary, &[]);
        let limited = read_before_paused(binary, &["-maxreceivebuffer=100"]);
        let (held, at_100) = (default.held(), limited.held());
        let less = held.saturating_sub(at_100);
        let found = format!(
            "{}: holds {held} bytes ({default:?}) at the default, {at_100} ({limited:?}) at \
             -maxreceivebuffer=100, {less} less",
            binary.display()
        );
        eprintln!("{found}");
        assert!((4_400_000..=5_400_000).contains(&less), "{found}");
    }
}

/// Rows 218 and 214 (p2p_blocksonly.py :119, :36): under `-blocksonly` (P5a D23) rdashd's
/// `version` carries relay 0; a peer that sends a `tx` is disconnected with Core's line
/// (np:4381-4386), and so is one that sends an `inv` of a transaction (np:4547-4551). Core's test uses inbound peers; rdashd dials, and
/// `-connect` dials the same address again after each disconnect.
#[test]
fn blocks_only_a_tx_or_tx_inv_disconnects_the_peer_with_cores_lines() {
    let (home, datadir) = home_and_datadir();
    let listener = listen();
    let node = dialling_with(home.path(), &datadir, &listener, false, &["-blocksonly"]);
    let mut peer = Peer::new(next_connection(&listener, Instant::now() + PATIENCE), false, &[]);
    let version = peer.handshake();
    // `fRelay`, then `mnauth_challenge` and `fMasternode` (Core: net_processing.cpp:1582-1590).
    assert_eq!(version[version.len() - 34], 0, "relay");
    peer.send("tx", Vec::new());
    let tx_line = "tx sent in violation of protocol peer=0";
    wait_log(&datadir, 0, tx_line);

    let mut peer = Peer::new(next_connection(&listener, Instant::now() + PATIENCE), false, &[]);
    peer.handshake();
    // `p2p_blocksonly.py`'s announcement: one `MSG_TX` entry for hash 0x1234.
    let mut inv = vec![1];
    inv.extend(1u32.to_le_bytes());
    inv.extend(0x1234u16.to_le_bytes());
    inv.extend([0; 30]);
    peer.send("inv", inv);
    let line = "tx (0000000000000000000000000000000000000000000000000000000000001234) inv sent in violation of protocol, disconnecting peer=1";
    wait_log(&datadir, 0, line);
    let log = stop(node, &datadir, 0);
    for line in [tx_line, line] {
        assert_eq!(
            log.lines().filter(|l| l.ends_with(&format!("[net] {line}"))).count(),
            1,
            "{log}"
        );
    }
}

/// Rows 259 and 263 (`p2p_invalid_messages.py:118`, `:158`): a V1 frame with a wrong checksum
/// or an invalid message type is dropped without disconnecting the peer and logs Core's
/// `Header error:` line (`src/net.cpp:811-822`).
#[test]
fn v1_bad_checksum_and_invalid_type_log_cores_lines() {
    let (home, datadir) = home_and_datadir();
    let (node, mut peer) = connected(home.path(), &datadir, &[], false);
    let from = mark(&datadir);
    let magic = regtest().message_start;

    // `msg_unrecognized(str_data="d")`: command `b"badmsg\x01"`, payload `ser_string(b"d")`.
    let payload = [0x01u8, b'd'];
    let checksum = dash_p2p_transport::checksum(&payload);
    assert_eq!(checksum, [0x78, 0xdf, 0x0a, 0x04]);

    let build_unrecognized = |cmd: [u8; 12], csum: [u8; 4]| {
        let mut frame = Vec::with_capacity(24 + payload.len());
        frame.extend_from_slice(&magic);
        frame.extend_from_slice(&cmd);
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&csum);
        frame.extend_from_slice(&payload);
        frame
    };

    // Core `test_checksum` (:118-125): `badmsg\x01` with checksum overwritten to `ffffffff`.
    let mut bad_cmd = [0u8; 12];
    bad_cmd[..7].copy_from_slice(b"badmsg\x01");
    let bad_checksum_frame = build_unrecognized(bad_cmd, [0xff; 4]);
    peer.wire.flush().expect("flush");
    peer.wire.sock.write_all(&bad_checksum_frame).expect("write bad checksum frame");
    let nonce1 = 0x1111_2222_3333_4444_u64.to_le_bytes().to_vec();
    peer.send("ping", nonce1.clone());
    assert_eq!(peer.expect("pong").payload, nonce1);
    let checksum_line =
        "Header error: Wrong checksum (badmsg, 2 bytes), expected 78df0a04 was ffffffff";
    wait_log(&datadir, from, &format!("{checksum_line}, peer=0"));

    // Core `test_msgtype` (:158-164): byte 7 (index 3 of command) overwritten to NUL, valid
    // checksum.
    let mut invalid_type_frame = build_unrecognized(bad_cmd, checksum);
    invalid_type_frame[7] = 0;
    peer.wire.flush().expect("flush");
    peer.wire.sock.write_all(&invalid_type_frame).expect("write invalid type frame");
    let nonce2 = 0x5555_6666_7777_8888_u64.to_le_bytes().to_vec();
    peer.send("ping", nonce2.clone());
    assert_eq!(peer.expect("pong").payload, nonce2);
    let invalid_type_line = "Header error: Invalid message type (bad, 2 bytes)";
    wait_log(&datadir, from, &format!("{invalid_type_line}, peer=0"));

    let log = stop(node, &datadir, from);
    assert!(log.contains(checksum_line), "{log}");
    assert!(log.contains("Header error: Invalid message type"), "{log}");
    for expected in
        [format!("[net] {checksum_line}, peer=0"), format!("[net] {invalid_type_line}, peer=0")]
    {
        assert_eq!(
            log.lines().filter(|l| l.ends_with(&expected)).count(),
            1,
            "missing or duplicate {expected:?}:\n{log}"
        );
    }
}
