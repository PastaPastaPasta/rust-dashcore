//! CH3b b2b-2's scripted peer: Core's `P2PDataStore` (`test_framework/p2p.py`) for the
//! functional-test ports. It dials a node (inbound from the node's side) over V1 with the
//! framework's services and user agent, keeps a block store with its current last block, answers
//! `getheaders` and `getheaders2` with the store's walk back toward the locator, serves stored
//! blocks on `getdata`, and sends blocks as `send_blocks_and_test` does. `sync_with_ping` is the
//! framework's two-ping barrier. Negotiation messages (`sendheaders`, `sendheaders2`, `sendcmpct`)
//! are consumed and never echoed, as `P2PInterface` does.
//!
//! The transport and framing start as a copy of `p2p_silence.rs`'s scripted peer (P3's file,
//! copied, not moved).

use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant, SystemTime};

use dash_consensus::primitives::decode_header;
use dash_p2p_core::{GetData, GetHeaders, GetHeaders2, Headers, Headers2, Inv, InvType};
use dash_p2p_transport::{CommandString, Magic, NetMessage, OutMsg, ProtocolVersion, Transport};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::BlockHeader;
use dash_types::codec::BaseCodec;

use super::net::{Node, as_i64, as_str, field, scaled};

/// The protocol version the framework speaks (`p2p.py` `P2P_VERSION`).
const P2P_VERSION: i32 = 70242;
/// `P2P_SERVICES = NODE_NETWORK | NODE_HEADERS_COMPRESSED`.
const P2P_SERVICES: u64 = 1 | 1 << 11;
/// `P2P_SUBVERSION % ""`.
const P2P_SUBVERSION: &str = "/python-p2p-tester:0.0.3/";
/// Core's `MAX_HEADERS_UNCOMPRESSED_RESULT` and `MAX_HEADERS_COMPRESSED_RESULT`.
const MAX_HEADERS_UNCOMPRESSED_RESULT: usize = 2000;
const MAX_HEADERS_COMPRESSED_RESULT: usize = 8000;

/// A block: its bytes, its header and hash (internal byte order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub bytes: Vec<u8>,
    pub header: BlockHeader,
    pub hash: [u8; 32],
}

impl Block {
    pub fn from_bytes(bytes: Vec<u8>) -> Block {
        let header = BlockHeader::decode(&mut &bytes[..80]).expect("a header");
        let hash = header_hash(&bytes[..80]);
        Block {
            bytes,
            header,
            hash,
        }
    }

    /// The hash as RPC shows it.
    pub fn hash_hex(&self) -> String {
        hash_hex(&self.hash)
    }

    pub fn prev(&self) -> [u8; 32] {
        bytes32(&self.header.prev_hash)
    }

    /// The block's bytes as `submitblock` takes them.
    pub fn hex(&self) -> String {
        super::mine::hex(&self.bytes)
    }
}

/// The X11 hash of an 80-byte header (internal byte order).
pub fn header_hash(header: &[u8]) -> [u8; 32] {
    *decode_header(&mut &header[..80]).expect("a header").into_value().hash().as_bytes()
}

/// `hash` (internal byte order) as RPC shows it.
pub fn hash_hex(hash: &[u8; 32]) -> String {
    hash.iter().rev().map(|b| format!("{b:02x}")).collect()
}

/// A hash as RPC shows it, in internal byte order: [`hash_hex`]'s inverse.
pub fn unhex_hash(hex: &str) -> [u8; 32] {
    let mut hash: [u8; 32] = super::mine::parse_hex(hex).try_into().expect("32 bytes");
    hash.reverse();
    hash
}

/// The 32 bytes `v` encodes to.
fn bytes32(v: &impl BaseCodec) -> [u8; 32] {
    encode(v).try_into().expect("32 bytes")
}

fn encode(v: &impl BaseCodec) -> Vec<u8> {
    let mut out = Vec::new();
    v.encode(&mut out);
    out
}

/// One `getdata` entry the node sent, in arrival order.
#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub inv_type: InvType,
    pub hash: [u8; 32],
}

/// The options of `send_blocks_and_test`, plus the further lines a step expects in its window.
#[derive(Clone, Copy, Debug)]
pub struct Expect<'a> {
    pub success: bool,
    pub force_send: bool,
    /// The FT's `reject_reason`, then any further fragments the window must hold.
    pub expected: &'a [&'a str],
    /// The FT's `timeout`, before scaling.
    pub timeout: u64,
}

impl<'a> Expect<'a> {
    /// A block the node must reject, with the window holding `expected`.
    pub fn rejected(expected: &'a [&'a str]) -> Self {
        Expect {
            success: false,
            expected,
            ..Expect::default()
        }
    }
}

impl Default for Expect<'_> {
    fn default() -> Self {
        Expect {
            success: true,
            force_send: false,
            expected: &[],
            timeout: 60,
        }
    }
}

/// The peer's connection to a node, after the handshake.
pub struct Peer {
    sock: TcpStream,
    t: Transport,
    inbox: VecDeque<NetMessage>,
    /// `block_store`, by hash.
    store: HashMap<[u8; 32], Block>,
    /// `last_block_hash`: the store's tip for the header walk.
    last_block_hash: Option<[u8; 32]>,
    /// Every `getdata` entry, its index the sequence number.
    pub requests: Vec<Request>,
    /// Every `pong`'s nonce, in arrival order.
    pongs: Vec<u64>,
    /// Every message's command, in arrival order.
    pub received: Vec<String>,
    ping_counter: u64,
    /// The node's id for this peer (`getpeerinfo`'s `id`).
    pub id: i64,
}

impl Peer {
    /// `add_p2p_connection(P2PDataStore())`: dials `node`, sends `version`, takes the node's
    /// `version` and `verack` (answering with `verack` and `getaddr`), runs `sync_with_ping`, and
    /// finds itself in the node's `getpeerinfo` by its address and user agent.
    // Core: test_node.py:701-770, p2p.py:520-531, :628-646
    pub fn connect(node: &Node) -> Peer {
        let sock = TcpStream::connect(("127.0.0.1", node.p2p_port)).expect("the node listens");
        sock.set_read_timeout(Some(Duration::from_millis(20))).expect("timeout");
        sock.set_write_timeout(Some(scaled(60))).expect("timeout");
        sock.set_nodelay(true).expect("nodelay");
        let local = sock.local_addr().expect("an address");
        let regtest = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
        let mut peer = Peer {
            sock,
            t: Transport::v1(Magic::from_bytes(regtest.message_start)),
            inbox: VecDeque::new(),
            store: HashMap::new(),
            last_block_hash: None,
            requests: Vec::new(),
            pongs: Vec::new(),
            received: Vec::new(),
            ping_counter: 1,
            id: -1,
        };
        peer.send("version", version_payload(node.p2p_port));
        let theirs = peer.expect("version");
        let version = i32::from_le_bytes(theirs.payload[..4].try_into().expect("4 bytes"));
        peer.t.set_peer_version(ProtocolVersion(version.min(P2P_VERSION)));
        peer.send("verack", Vec::new());
        peer.send("getaddr", Vec::new());
        peer.expect("verack");
        peer.sync_with_ping();
        let ours = local.to_string();
        let info: Vec<_> =
            node.peers().into_iter().filter(|p| as_str(field(p, "addr")) == ours).collect();
        assert_eq!(info.len(), 1, "getpeerinfo lists {ours} once");
        assert_eq!(as_str(field(&info[0], "subver")), P2P_SUBVERSION);
        peer.id = as_i64(field(&info[0], "id"));
        peer
    }

    fn flush(&mut self) -> std::io::Result<()> {
        loop {
            let data = self.t.bytes_to_send(false).data;
            if data.is_empty() {
                return Ok(());
            }
            let n = self.sock.write(data)?;
            self.t.mark_bytes_sent(n);
        }
    }

    /// Sends what is pending and feeds what arrives; `Err` on end-of-stream or bytes the
    /// transport refuses.
    fn pump(&mut self) -> std::io::Result<()> {
        self.flush()?;
        let mut buf = [0u8; 65536];
        match self.sock.read(&mut buf) {
            Ok(0) => Err(ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                let inbox = &mut self.inbox;
                self.t
                    .receive(&buf[..n], 0, |m| inbox.extend(m.ok()))
                    .map_err(|e| std::io::Error::new(ErrorKind::InvalidData, e))
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn send(&mut self, command: &str, payload: Vec<u8>) {
        let mut msg = OutMsg::new(CommandString::from_static(command), payload);
        let deadline = Instant::now() + scaled(60);
        while let Err(m) = self.t.set_message_to_send(msg) {
            msg = m;
            self.pump().expect("the node disconnected the peer");
            assert!(Instant::now() < deadline, "the transport never took {command}");
        }
        self.flush().expect("send");
    }

    /// The next message of type `command`, handling every other message meanwhile.
    fn expect(&mut self, command: &str) -> NetMessage {
        let deadline = Instant::now() + scaled(60);
        loop {
            while let Some(m) = self.inbox.pop_front() {
                if m.command.as_str() == command {
                    return m;
                }
                self.handle(m);
            }
            self.pump().expect("the node disconnected the peer");
            assert!(Instant::now() < deadline, "no {command} from the node");
        }
    }

    /// Handles what has arrived: one pump, then every queued message.
    pub fn answer(&mut self) {
        self.pump().expect("the node disconnected the peer");
        while let Some(m) = self.inbox.pop_front() {
            self.handle(m);
        }
    }

    /// Pumps and handles messages until `done`, failing after [`scaled`] `base_secs`.
    pub fn answer_until(
        &mut self,
        what: &str,
        base_secs: u64,
        mut done: impl FnMut(&Peer) -> bool,
    ) {
        let deadline = Instant::now() + scaled(base_secs);
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "timed out after {:?} waiting for {what}",
                scaled(base_secs)
            );
            self.answer();
        }
    }

    /// `P2PDataStore`'s handlers: `pong` per `ping`; `getdata` recorded and served from the
    /// store; both header requests answered by the walk; `inv` requested back (`on_inv`); `pong`
    /// recorded; everything else consumed.
    // Core: p2p.py:595-646, :870-925
    fn handle(&mut self, m: NetMessage) {
        self.received.push(m.command.as_str().to_owned());
        match m.command.as_str() {
            "ping" => self.send("pong", m.payload),
            "pong" => {
                let nonce = u64::from_le_bytes(m.payload[..8].try_into().expect("8 bytes"));
                self.pongs.push(nonce);
            }
            "getdata" => {
                let gd = GetData::decode(&mut &m.payload[..]).expect("a getdata");
                for inv in &gd.inventory {
                    let hash = *inv.hash.as_bytes();
                    self.requests.push(Request {
                        inv_type: inv.inv_type,
                        hash,
                    });
                    if inv.inv_type == InvType::Block
                        && let Some(block) = self.store.get(&hash)
                    {
                        let bytes = block.bytes.clone();
                        self.send("block", bytes);
                    }
                }
            }
            "getheaders" => {
                let gh = GetHeaders::decode(&mut &m.payload[..]).expect("a getheaders");
                let locator: Vec<[u8; 32]> = gh.locator_hashes.iter().map(bytes32).collect();
                let headers =
                    self.walk(&locator, bytes32(&gh.hash_stop), MAX_HEADERS_UNCOMPRESSED_RESULT);
                self.send(
                    "headers",
                    encode(&Headers {
                        headers,
                    }),
                );
            }
            "getheaders2" => {
                let gh = GetHeaders2::decode(&mut &m.payload[..]).expect("a getheaders2");
                let locator: Vec<[u8; 32]> = gh.locator_hashes.iter().map(bytes32).collect();
                let headers =
                    self.walk(&locator, bytes32(&gh.hash_stop), MAX_HEADERS_COMPRESSED_RESULT);
                self.send("headers2", encode(&Headers2::reply(&headers)));
            }
            "inv" => {
                let inv = Inv::decode(&mut &m.payload[..]).expect("an inv");
                let want: Vec<_> =
                    inv.inventory.into_iter().filter(|i| i.inv_type != InvType::Error).collect();
                if !want.is_empty() {
                    self.send(
                        "getdata",
                        encode(&GetData {
                            inventory: want,
                        }),
                    );
                }
            }
            _ => {}
        }
    }

    /// `_compute_requested_block_headers`: from the store's last block back toward the locator
    /// (or the stop hash, or a block the store lacks), oldest first, at most `max` of the
    /// newest. An empty store gives none.
    // Core: p2p.py:883-905
    fn walk(&self, locator: &[[u8; 32]], stop: [u8; 32], max: usize) -> Vec<BlockHeader> {
        let Some(last) = self.last_block_hash else {
            return Vec::new();
        };
        let mut list = vec![&self.store[&last]];
        while !locator.contains(&list.last().unwrap().hash) {
            let Some(prev) = self.store.get(&list.last().unwrap().prev()) else {
                break;
            };
            list.push(prev);
            if prev.hash == stop {
                break;
            }
        }
        list.iter().take(max).rev().map(|b| b.header).collect()
    }

    /// `sync_with_ping`: a ping with nonce 0, then one with a fresh nonce, each waiting for its
    /// own new pong, so the node has run `SendMessages` for this peer since it read what came
    /// before (`p2p.py:759-771`).
    pub fn sync_with_ping(&mut self) {
        for nonce in [0, self.ping_counter] {
            let from = self.pongs.len();
            self.send("ping", nonce.to_le_bytes().to_vec());
            self.answer_until(&format!("pong {nonce}"), 60, |p| p.pongs[from..].contains(&nonce));
        }
        self.ping_counter += 1;
    }

    /// `send_blocks_and_test` (`p2p.py:930-967`): stores `blocks`, then sends one `headers` and
    /// waits for a new `MSG_BLOCK` request of the last one, served from the store (or, with
    /// `force_send`, sends the blocks), then the ping barrier and the tip check: the last block
    /// on success, else the tip unchanged. The log window opens before anything is sent and
    /// must hold every expected fragment within 2 s × the factor of opening, checked at least
    /// once after the barrier and the tip condition. Returns the window's text.
    pub fn send_blocks_and_test(
        &mut self,
        node: &Node,
        blocks: &[&Block],
        e: Expect<'_>,
    ) -> String {
        let last = blocks.last().expect("a block").hash;
        for b in blocks {
            self.store.insert(b.hash, (*b).clone());
            self.last_block_hash = Some(b.hash);
        }
        let tip_before = node.best_hash();
        let window = node.log_len();
        let window_end = Instant::now() + scaled(2);
        let requests_before = self.requests.len();
        if e.force_send {
            for b in blocks {
                self.send("block", b.bytes.clone());
            }
        } else {
            let headers = Headers {
                headers: blocks.iter().map(|b| b.header).collect(),
            };
            self.send("headers", encode(&headers));
            self.answer_until(&format!("a new request of {}", hash_hex(&last)), e.timeout, |p| {
                p.requests[requests_before..]
                    .iter()
                    .any(|r| r.inv_type == InvType::Block && r.hash == last)
            });
        }
        self.sync_with_ping();
        if e.success {
            let want = hash_hex(&last);
            self.answer_until(&format!("tip {want}"), e.timeout, |_| node.best_hash() == want);
        } else {
            assert_eq!(node.best_hash(), tip_before, "the tip moved");
        }
        loop {
            let log = node.log_since(window);
            let missing: Vec<_> = e.expected.iter().filter(|m| !log.contains(**m)).collect();
            if missing.is_empty() {
                return log;
            }
            assert!(Instant::now() < window_end, "the log window lacks {missing:?}:\n{log}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// `msg_version` as `peer_connect_send_version` fills it: `P2P_VERSION`, `P2P_SERVICES`, the
/// node's address as `addrTo`, `0.0.0.0:0` as `addrFrom`, a random nonce, `P2P_SUBVERSION`,
/// starting height -1, relay 1, and Dash's zero MNAUTH challenge and masternode flag.
// Core: messages.py:1635-1693, :326-336
fn version_payload(port: u16) -> Vec<u8> {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    let mut out = P2P_VERSION.to_le_bytes().to_vec();
    out.extend(P2P_SERVICES.to_le_bytes());
    out.extend((now as i64).to_le_bytes());
    for (ip, port) in [([127, 0, 0, 1], port), ([0, 0, 0, 0], 0)] {
        out.extend(1u64.to_le_bytes());
        out.extend([0; 10]);
        out.extend([0xff; 2]);
        out.extend(ip);
        out.extend(port.to_be_bytes());
    }
    let nonce = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos() as u64;
    out.extend(nonce.to_le_bytes());
    out.push(P2P_SUBVERSION.len() as u8);
    out.extend(P2P_SUBVERSION.as_bytes());
    out.extend((-1i32).to_le_bytes());
    out.push(1);
    out.extend([0; 32]);
    out.push(0);
    out
}
