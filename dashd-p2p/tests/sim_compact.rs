//! P3 S3 and S4 in `sim` (P3 §6.2): the compact-block client, high-bandwidth selection and
//! compact announcements through the real `BlockDownload` and `SimChain`'s block path. S3-a…
//! and S4-a… port subtests of `p2p_compactblocks.py` step for step; the rest are not FTs and
//! name the Core lines they pin. S4-x, the ordering matrix, places V's accept, `UpdatedBlockTip`
//! and the actor's regions against each other with `sim`'s pause seams, on threads standing in
//! for V and the scheduler (P3 D102).
//!
//! Transactions the Python relays with `msg_tx` reach the node's mempool here through a
//! [`Pool`] installed as the `CompactTxSource`, a stand-in for MP1a's mempool and P5's `tx`
//! handler. S3-d sets its peers' high-bandwidth state directly, where its Python earns it by
//! having each peer relay a block first.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Once};
use std::time::Duration;

use dash_consensus::{Block, BlockId, Header, TxRef, decode_tx};
use dash_p2p::capture::MessageCapture;
use dash_p2p::policy::Throw;
use dash_p2p::rng::RandSource;
use dash_p2p::{Address, CommandString, ConnectionType, NetConfig, PeerId, Why};
use dash_p2p_core::{
    BlockTxn, GetBlockTxn, GetData, GetHeaders, Headers, Headers2, Inv, InvType, Inventory,
    ProtocolVersion, SendCmpct,
};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, BlockHeader, MerkleRoot, TxHash};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::compact::{CompactBlock, CompactTxSource, Prefilled, bucket_count};
use dashd_p2p::processing::download::BlockDownload;
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::peerman::LockPoint;
use dashd_p2p::processing::seams::{BlockSubmit, ChainAccess, EntryFacts, TreeRead};
use dashd_p2p::processing::sync::block_hash;
use dashd_p2p::sim::{
    AnnounceHook, PATIENCE, PeerVersionMsg, Sim, SimChain, SimPeer, SimRead, until,
};
use dashd_p2p::{HandlerCx, NetHandler, Registry, Seams};

thread_local! {
    /// The `net` and `validation` lines this thread logged while [`capture`] ran.
    static LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// A global subscriber, installed before any callsite registers (P3 D75's trap); events go to
/// the emitting thread's [`LINES`]. The sim runs the message loop on the test's thread.
struct Global;

impl tracing::Subscriber for Global {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        meta.is_event()
            && ["net", "validation"].contains(&meta.target())
            && LINES.with(|l| l.borrow().is_some())
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Message<'a>(&'a mut String);
        impl tracing::field::Visit for Message<'_> {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    *self.0 = format!("{v:?}");
                }
            }
        }
        let mut line = String::new();
        event.record(&mut Message(&mut line));
        LINES.with(|l| l.borrow_mut().as_mut().map(|v| v.push(line)));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| tracing::subscriber::set_global_default(Global).expect("one subscriber"));
}

/// Runs `f`, returning what it logged on this thread.
fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    LINES.with(|l| *l.borrow_mut() = Some(Vec::new()));
    let r = f();
    (r, LINES.with(|l| l.borrow_mut().take()).unwrap_or_default())
}

fn has(lines: &[String], want: &str) -> bool {
    lines.iter().any(|l| l.contains(want))
}

fn params() -> Arc<ChainParams> {
    Arc::new(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap())
}

/// The node's mempool as `InitData` reads it, in insertion order (`vTxHashes` with no removal).
#[derive(Default)]
struct Pool(parking_lot::Mutex<Vec<(TxHash, TxRef)>>);

impl CompactTxSource for Pool {
    fn for_each_in_vtxhashes_order(&self, f: &mut dyn FnMut(&TxHash, &TxRef) -> bool) {
        for (hash, tx) in self.0.lock().iter() {
            if !f(hash, tx) {
                return;
            }
        }
    }
}

/// A Core-serialized version 1 transaction: one input spending `prevout` with `script_sig`, one
/// `OP_TRUE` output of `value`, lock time 0.
fn raw_tx(prevout: ([u8; 32], u32), script_sig: &[u8], value: u64) -> Vec<u8> {
    let mut raw = vec![1, 0, 0, 0, 1];
    raw.extend_from_slice(&prevout.0);
    raw.extend_from_slice(&prevout.1.to_le_bytes());
    raw.push(script_sig.len() as u8);
    raw.extend_from_slice(script_sig);
    raw.extend_from_slice(&[0xff; 4]);
    raw.push(1);
    raw.extend_from_slice(&value.to_le_bytes());
    raw.extend_from_slice(&[1, 0x51]);
    raw.extend_from_slice(&[0; 4]);
    raw
}

fn decode(raw: &[u8]) -> TxRef {
    Arc::new(decode_tx(&mut &raw[..]).expect("a valid transaction").into_value())
}

/// A coinbase whose script carries `seed`, paying `value`.
fn coinbase(seed: u32, value: u64) -> TxRef {
    let mut script = seed.to_le_bytes().to_vec();
    script.push(0x51);
    decode(&raw_tx(([0; 32], u32::MAX), &script, value))
}

/// The `i`th spend of block `seed`: synthetic `sim` data; `SimChain` checks no inputs.
fn spend(seed: u32, i: u32) -> TxRef {
    let mut prevout = [0; 32];
    prevout[..4].copy_from_slice(&seed.to_le_bytes());
    prevout[4..8].copy_from_slice(&i.to_le_bytes());
    decode(&raw_tx((prevout, 0), &[], 42))
}

/// A block on `prev_hash` (whose header is `prev`) holding `txs`, its header committing to their
/// merkle root.
fn block_on(prev_hash: BlockHash, prev: &BlockHeader, txs: Vec<TxRef>, nonce: u32) -> Arc<Block> {
    let header = |merkle_root| BlockHeader {
        version: 0x2000_0000,
        prev_hash,
        merkle_root,
        time: prev.time + 1,
        bits: prev.bits,
        nonce,
    };
    let (root, mutated) = Block::new(Header::new(header(MerkleRoot::ZERO)), txs.clone()).merkle();
    assert!(!mutated);
    Arc::new(Block::new(Header::new(header(root)), txs))
}

/// A child of `prev` with a coinbase and one spend, both seeded by `seed`.
fn child(prev: &Block, seed: u32) -> Arc<Block> {
    block_on(prev.hash(), prev.header().raw(), vec![coinbase(seed, 50), spend(seed, 0)], seed)
}

/// `HeaderAndShortIDs.initialize_from_block(block, prefill_list)` with nonce 0: the listed
/// transactions prefilled (differentially indexed), short IDs for the rest.
fn compact(block: &Block, prefill: &[usize]) -> CompactBlock {
    let mut c = CompactBlock::from_block(block, 0);
    let mut last: Option<usize> = None;
    c.prefilled = prefill
        .iter()
        .map(|&i| {
            let index = i - last.map_or(0, |l| l + 1);
            last = Some(i);
            Prefilled {
                index: index as u16,
                tx: Arc::clone(&block.txs()[i]),
            }
        })
        .collect();
    c.short_ids = (0..block.txs().len())
        .filter(|i| !prefill.contains(i))
        .map(|i| c.short_id(&block.txs()[i].instance_hash()))
        .collect();
    c
}

/// `n` distinct short IDs, the first `crowd` in one libstdc++ bucket of `bucket_count(n)`, the rest
/// one per other bucket: `READ_STATUS_FAILED` from `InitData` when `crowd` is 13 (P3 §2.3.2).
fn crowded(n: usize, crowd: usize) -> Vec<u64> {
    let b = bucket_count(n);
    let mut ids: Vec<u64> = (0..crowd as u64).map(|k| 1 + k * b).collect();
    ids.extend((0..b).filter(|&x| x != 1).take(n - crowd));
    ids
}

/// `submitblock`: `block` through V from no peer.
fn rpc_submit(chain: &SimChain, block: &Arc<Block>) {
    chain.accept_block(&BlockSubmit {
        hash: block.hash(),
        block: Arc::clone(block),
        force: true,
        from: -1,
        merkle_checked: false,
    });
}

fn cmpct_msg(c: &CompactBlock) -> Vec<u8> {
    let mut buf = Vec::new();
    c.to_msg().encode(&mut buf);
    buf
}

fn blocktxn_msg(hash: BlockHash, txs: &[TxRef]) -> Vec<u8> {
    let mut buf = Vec::new();
    BlockTxn {
        block_hash: hash,
        txs: txs.iter().map(|t| t.to_mutable()).collect(),
    }
    .encode(&mut buf);
    buf
}

fn block_msg(block: &Block) -> Vec<u8> {
    dash_consensus::core_serialize(block)
}

fn headers_msg(blocks: &[&Arc<Block>]) -> Vec<u8> {
    let mut buf = Vec::new();
    Headers {
        headers: blocks.iter().map(|b| *b.header().raw()).collect::<Vec<BlockHeader>>(),
    }
    .encode(&mut buf);
    buf
}

/// An `inv` of `MSG_BLOCK` entries for `hashes`.
fn inv_msg(hashes: &[BlockHash]) -> Vec<u8> {
    let mut buf = Vec::new();
    Inv {
        inventory: hashes
            .iter()
            .map(|h| Inventory {
                inv_type: InvType::Block,
                hash: dash_num::Hash256::from_lendian(*h.as_bytes()),
            })
            .collect(),
    }
    .encode(&mut buf);
    buf
}

fn sendcmpct_msg(high_bandwidth: bool) -> Vec<u8> {
    sendcmpct_version(high_bandwidth, 1)
}

fn sendcmpct_version(high_bandwidth: bool, version: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    SendCmpct {
        high_bandwidth,
        version,
    }
    .encode(&mut buf);
    buf
}

/// `getheaders` with locator `[from]` and no stop.
fn getheaders_msg(from: BlockHash) -> Vec<u8> {
    let mut buf = Vec::new();
    GetHeaders {
        version: ProtocolVersion(70242),
        locator_hashes: vec![from],
        hash_stop: BlockHash::from_lendian([0; 32]),
    }
    .encode(&mut buf);
    buf
}

/// The block hashes a block announcement names, as the Python's `announced_blockhashes`:
/// `inv`'s `MSG_BLOCK` entries, `headers`' headers and `cmpctblock`'s header.
fn announced_hashes(cmd: &str, payload: &[u8]) -> Vec<BlockHash> {
    match cmd {
        "inv" => {
            let inv = Inv::decode(&mut &payload[..]).expect("an inv");
            inv.inventory.iter().filter(|i| i.inv_type == InvType::Block).map(block_hash).collect()
        }
        "headers" => {
            let h = Headers::decode(&mut &payload[..]).expect("a headers");
            h.headers.iter().map(|h| Header::new(*h).hash()).collect()
        }
        "cmpctblock" => vec![cmpct_header(payload).hash()],
        _ => Vec::new(),
    }
}

/// `cmpctblock`'s header.
fn cmpct_header(payload: &[u8]) -> Header {
    Header::new(BlockHeader::decode(&mut &payload[..80]).expect("a header"))
}

/// The encoder's `cmpctblock` for `block` with the nonce `payload` carries.
fn reencode(block: &Block, payload: &[u8]) -> Vec<u8> {
    let nonce = u64::from_le_bytes(payload[80..88].try_into().unwrap());
    CompactBlock::from_block(block, nonce).wire_bytes()
}

/// A scripted peer and what the node sent it, as the Python's `last_message`.
struct Conn {
    peer: SimPeer,
    sent: Vec<(CommandString, Vec<u8>)>,
    /// What the node sent by the end of the handshake's `verack` pass.
    greeting: Vec<(CommandString, Vec<u8>)>,
    gone: bool,
}

impl Conn {
    fn last(&self, cmd: &str) -> Option<&[u8]> {
        self.sent.iter().rev().find(|(c, _)| c.as_str() == cmd).map(|(_, p)| &p[..])
    }

    /// `getblocktxn`'s absolute indexes, as `block_txn_request.to_absolute()`.
    fn getblocktxn(&self) -> Option<(BlockHash, Vec<u16>)> {
        self.last("getblocktxn").map(|p| {
            let req = GetBlockTxn::decode(&mut &p[..]).expect("a getblocktxn");
            (*req.block_hash(), req.indexes().to_vec())
        })
    }

    /// The last `getdata`'s entries.
    fn getdata(&self) -> Option<Vec<Inventory>> {
        self.last("getdata").map(|p| GetData::decode(&mut &p[..]).expect("a getdata").inventory)
    }

    /// The last `getdata` is `MSG_BLOCK` for `hash` alone.
    fn assert_getdata_block(&self, hash: BlockHash) {
        let gd = self.getdata().expect("a getdata");
        let got: Vec<_> = gd.iter().map(|i| (i.inv_type, block_hash(i))).collect();
        assert_eq!(got, [(InvType::Block, hash)]);
    }

    fn clear(&mut self) {
        self.sent.clear();
    }

    /// `wait_for_block_announcement(hash)`, then the announcing commands since the last
    /// [`Conn::clear`] (`clear_block_announcement`): what the Python's predicates read.
    fn announced(&self, hash: BlockHash) -> Vec<&str> {
        let cmds: Vec<&str> = self
            .sent
            .iter()
            .filter(|(c, _)| ["inv", "headers", "headers2", "cmpctblock"].contains(&c.as_str()))
            .map(|(c, _)| c.as_str())
            .collect();
        assert!(
            self.sent.iter().any(|(c, p)| announced_hashes(c.as_str(), p).contains(&hash)),
            "{hash} not announced: {cmds:?}"
        );
        cmds
    }
}

/// Counts `GetRand`'s draws and answers each with distinct bytes.
#[derive(Default)]
struct CountingRand(AtomicU64);

impl RandSource for CountingRand {
    fn rand_bytes(&self, out: &mut [u8], _strong: bool) {
        let n = self.0.fetch_add(1, Ordering::Relaxed);
        out.iter_mut().enumerate().for_each(|(i, b)| *b = (n as u8).wrapping_add(i as u8));
    }
}

/// What a `qsigrec` message runs inside its region, with the peer's state and header cell held
/// (any Core command no other handler takes).
type HoldFn = Box<dyn FnOnce() + Send>;

const QHOLD: [CommandString; 1] = [CommandString::from_static("qsigrec")];

/// The handler for `qsigrec`: an actor region for a test to run code in.
struct Hold(Arc<parking_lot::Mutex<Option<HoldFn>>>);

impl NetHandler for Hold {
    fn commands(&self) -> &'static [CommandString] {
        &QHOLD
    }

    fn process_message(
        &mut self,
        _cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        if let Some(f) = self.0.lock().take() {
            f();
        }
        Ok(())
    }
}

/// Every message the core pushed, in push order across peers (`-capturemessages`' send side,
/// recorded on the pushing thread).
#[derive(Debug, Default)]
struct Pushes(parking_lot::Mutex<Vec<(Address, String, Vec<u8>)>>);

impl MessageCapture for Pushes {
    fn capture(&self, _: i64, addr: &Address, cmd: &str, payload: &[u8], incoming: bool) {
        if !incoming {
            self.0.lock().push((addr.clone(), cmd.to_owned(), payload.to_vec()));
        }
    }
}

/// The node, its pool and its peers.
struct Rig {
    /// What the core pushed.
    pushes: Arc<Pushes>,
    sim: Sim,
    pool: Arc<Pool>,
    /// What the next `qsigrec` runs.
    hold: Arc<parking_lot::Mutex<Option<HoldFn>>>,
    /// The network core's random source.
    rand: Arc<CountingRand>,
    conns: Vec<Conn>,
    nonce: u32,
    /// The handshake's version time.
    mocktime: i64,
}

impl Rig {
    /// A node with P3's `BlockDownload` and a [`Pool`], out of IBD on a genesis tip 10 s old.
    fn new() -> Self {
        Self::with(ProcessorOptions::default(), false)
    }

    fn with(opts: ProcessorOptions, ibd: bool) -> Self {
        install();
        let p = params();
        let id = local_identity(&p, &IdentityOptions::new()).unwrap();
        let pool = Arc::new(Pool::default());
        let mut dl = BlockDownload::new();
        dl.set_tx_source(Arc::clone(&pool) as Arc<dyn CompactTxSource>);
        let seams = Seams {
            block_sync: Box::new(dl),
            ..Seams::default()
        };
        let chain = SimChain::new(Arc::clone(&p));
        let start = i64::from(chain.genesis().time) + 10;
        let rand = Arc::new(CountingRand::default());
        let src: Arc<dyn RandSource> = rand.clone();
        let pushes = Arc::new(Pushes::default());
        let capture: Arc<dyn MessageCapture> = pushes.clone();
        let cfg = |cfg: &mut NetConfig| {
            *cfg = cfg.clone().with_rand_source(src);
            cfg.capture = Some(capture);
        };
        let hold = Arc::default();
        let mut registry = Registry::new();
        registry.register(Box::new(Hold(Arc::clone(&hold))));
        let sim = Sim::with_chain(p, id, opts, registry, seams, start, |_| chain, cfg);
        sim.chain.set_ibd(ibd);
        Self {
            pushes,
            sim,
            pool,
            hold,
            rand,
            conns: Vec::new(),
            nonce: 0,
            mocktime: start,
        }
    }

    /// Connects scripted peer `n`, outbound full-relay or inbound, and runs the handshake.
    fn connect(&mut self, n: u8, outbound: bool) -> usize {
        let peer = if outbound {
            self.sim.connect(n, ConnectionType::OutboundFullRelay, Default::default())
        } else {
            self.sim.accept(n, Default::default()).expect("inbound admitted")
        };
        let greeting = self.sim.handshake(&peer, PeerVersionMsg::v24(self.mocktime));
        self.conns.push(Conn {
            peer,
            sent: Vec::new(),
            greeting,
            gone: false,
        });
        let i = self.conns.len() - 1;
        self.sync();
        self.conns[i].clear();
        i
    }

    /// Message-loop passes until one has no more work, recording what each peer was sent, then
    /// `FinalizeNode` for every peer marked for disconnection: `sync_with_ping` on every peer.
    fn sync(&mut self) {
        loop {
            let more = self.sim.pass();
            for c in self.conns.iter_mut().filter(|c| !c.gone) {
                c.sent.extend(self.sim.sent(&c.peer));
            }
            if !more {
                break;
            }
        }
        self.sim.remove_disconnected();
        for c in self.conns.iter_mut().filter(|c| !c.gone) {
            c.gone = c.peer.handle.disconnect_requested();
        }
    }

    /// `send_and_ping`.
    fn send(&mut self, i: usize, cmd: &str, payload: Vec<u8>) {
        self.sim.deliver(&self.conns[i].peer, &[(cmd, payload)]);
        self.sync();
    }

    /// [`Rig::send`], returning what the node logged meanwhile.
    fn send_logged(&mut self, i: usize, cmd: &str, payload: Vec<u8>) -> Vec<String> {
        capture(|| self.send(i, cmd, payload)).1
    }

    /// Peer `i`'s node-side id, as the log lines name it.
    fn id(&self, i: usize) -> PeerId {
        self.conns[i].peer.handle.id()
    }

    /// [`rpc_submit`], then the node's reaction.
    fn rpc_submit(&mut self, block: &Arc<Block>) {
        rpc_submit(&self.sim.chain, block);
        self.sync();
    }

    fn tip(&self) -> EntryFacts {
        let snap = self.sim.chain.snapshot();
        self.sim.chain.entry(snap.tip.tip).expect("the tip is indexed")
    }

    /// The id of `block`'s index entry.
    fn block_id(&self, block: &Block) -> BlockId {
        self.sim.chain.by_hash(&block.hash()).expect("the block is indexed").id
    }

    /// `build_block_with_transactions(node, utxo, n)`: a coinbase and `n` spends on the tip.
    fn build_block(&mut self, n: u32) -> Arc<Block> {
        self.nonce += 1;
        let seed = self.nonce;
        let mut txs = vec![coinbase(seed, 50)];
        txs.extend((0..n).map(|i| spend(seed, i)));
        let tip = self.tip();
        block_on(tip.hash, &tip.header, txs, seed)
    }

    /// `generate(node, 1)` with `n` spends: a block on the tip through `submitblock` (so
    /// `NewPoWValidBlock`), then `UpdatedBlockTip` and the node's reaction. Clears every peer's
    /// record first (`clear_block_announcement`).
    fn generate(&mut self, n: u32) -> Arc<Block> {
        let block = self.build_block(n);
        self.submit_and_notify(&block);
        block
    }

    /// [`Rig::generate`] for a given block on the tip.
    fn submit_and_notify(&mut self, block: &Arc<Block>) {
        self.conns.iter_mut().for_each(Conn::clear);
        let prev = self.tip();
        rpc_submit(&self.sim.chain, block);
        assert_eq!(self.tip().hash, block.hash(), "the block connected");
        self.updated_block_tip(&prev);
        self.sync();
    }

    /// `request_headers_and_sync(locator=[from])`: a `getheaders`, its answer, then cleared.
    fn headers_sync(&mut self, i: usize, from: BlockHash) {
        self.send(i, "getheaders", getheaders_msg(from));
        assert!(self.conns[i].last("headers").is_some(), "a headers answer");
        self.conns[i].clear();
    }

    /// The draws from the network core's random source so far.
    fn draws(&self) -> u64 {
        self.rand.0.load(Ordering::Relaxed)
    }

    /// `request_cb_announcements`: a `getheaders` from the tip, then `sendcmpct(1)`.
    fn request_cb_announcements(&mut self, i: usize) {
        let tip = self.tip().hash;
        self.send(i, "getheaders", getheaders_msg(tip));
        self.send(i, "sendcmpct", sendcmpct_msg(true));
        self.conns[i].clear();
    }

    /// `msg_tx` accepted to the mempool (the [`Pool`] stand-in).
    fn to_mempool(&self, txs: &[TxRef]) {
        let mut pool = self.pool.0.lock();
        pool.extend(txs.iter().map(|t| (t.instance_hash(), Arc::clone(t))));
    }

    /// `test_getblocktxn_response`: send the compact block, return the request's indexes.
    fn announce(&mut self, i: usize, c: &CompactBlock) -> Option<Vec<u16>> {
        self.conns[i].clear();
        self.send(i, "cmpctblock", cmpct_msg(c));
        self.conns[i].getblocktxn().map(|(_, idx)| idx)
    }
}

/// S3-a: `test_getblocktxn_requests` (p2p_compactblocks.py:430-509). The mempool cases use the
/// [`Pool`] stand-in for `msg_tx`.
// Core: net_processing.cpp:5191-5218, 3734-3816
#[test]
fn s3a_getblocktxn_requests() {
    let mut t = Rig::new();
    let p = t.connect(1, false);

    // Nothing in the mempool: every non-prefilled transaction is requested.
    let block = t.build_block(5);
    let c = compact(&block, &[0]);
    assert_eq!(t.announce(p, &c), Some(vec![1, 2, 3, 4, 5]));
    assert_eq!(t.conns[p].getblocktxn().unwrap().0, block.hash());
    t.send(p, "blocktxn", blocktxn_msg(block.hash(), &block.txs()[1..]));
    assert_eq!(t.tip().hash, block.hash());

    // Prefilled transactions interspersed.
    let block = t.build_block(5);
    let c = compact(&block, &[0, 1, 5]);
    assert_eq!(t.announce(p, &c), Some(vec![2, 3, 4]));
    t.send(p, "blocktxn", blocktxn_msg(block.hash(), &block.txs()[2..5]));
    assert_eq!(t.tip().hash, block.hash());

    // One transaction already in the mempool.
    let block = t.build_block(5);
    t.to_mempool(&block.txs()[1..2]);
    let c = compact(&block, &[0, 2, 3, 4]);
    assert_eq!(t.announce(p, &c), Some(vec![5]));
    t.send(p, "blocktxn", blocktxn_msg(block.hash(), &block.txs()[5..]));
    assert_eq!(t.tip().hash, block.hash());

    // Every transaction in the mempool: reconstructed at once, nothing requested.
    let block = t.build_block(10);
    t.to_mempool(&block.txs()[1..]);
    let c = compact(&block, &[0]);
    assert_eq!(t.announce(p, &c), None);
    assert_eq!(t.tip().hash, block.hash());
    assert!(!t.conns[p].gone);

    // The tip announced again: `BLOCK_HAVE_DATA`, so nothing (np:5131-5133).
    assert_eq!(t.announce(p, &c), None);
    assert!(t.conns[p].getdata().is_none());
}

/// S3-b: `test_incorrect_blocktxn_response` (p2p_compactblocks.py:511-560): a wrong `blocktxn`
/// fails reconstruction, falls back to `getdata(MSG_BLOCK)` and leaves the block fetchable.
// Core: net_processing.cpp:3780-3792
#[test]
fn s3b_incorrect_blocktxn_response() {
    let mut t = Rig::new();
    let p = t.connect(1, false);
    let block = t.build_block(10);
    t.to_mempool(&block.txs()[1..6]);
    let c = compact(&block, &[0]);
    assert_eq!(t.announce(p, &c), Some(vec![6, 7, 8, 9, 10]));
    let prev = t.tip().hash;

    let mut wrong = vec![Arc::clone(&block.txs()[5])];
    wrong.extend(block.txs()[7..].iter().cloned());
    t.send(p, "blocktxn", blocktxn_msg(block.hash(), &wrong));
    assert_eq!(t.tip().hash, prev);
    t.conns[p].assert_getdata_block(block.hash());

    t.send(p, "block", block_msg(&block));
    assert_eq!(t.tip().hash, block.hash());
    assert!(!t.conns[p].gone);
}

/// S3-c: `test_compactblock_reconstruction_stalling_peer` (p2p_compactblocks.py:754-808): a
/// second announcer reconstructs from the mempool while the first stalls; a second announcer
/// whose reconstruction fails gives up quietly and the first one's `blocktxn` still lands.
// Core: net_processing.cpp:5160-5218, 3780-3792
#[test]
fn s3c_reconstruction_stalling_peer() {
    let mut t = Rig::new();
    let stalling = t.connect(1, false);
    let delivery = t.connect(2, false);

    let block = t.build_block(5);
    let c = compact(&block, &[0]);
    assert!(t.announce(stalling, &c).is_some());
    t.to_mempool(&block.txs()[1..]);
    t.send(delivery, "cmpctblock", cmpct_msg(&c));
    assert_eq!(t.tip().hash, block.hash());

    // A compact block that fails to reconstruct does not break relay.
    let block = t.build_block(5);
    let mut c = compact(&block, &[0]);
    assert!(t.announce(stalling, &c).is_some());
    t.to_mempool(&block.txs()[1..]);
    // The prefilled coinbase pays one more: still a coinbase, but the merkle root no longer
    // matches, which `FillBlock`'s `IsBlockMutated` catches.
    c.prefilled[0].tx = coinbase(t.nonce, 51);
    let delivery_id = t.id(delivery);
    let lines = t.send_logged(delivery, "cmpctblock", cmpct_msg(&c));
    assert_ne!(t.tip().hash, block.hash());
    assert!(
        has(
            &lines,
            &format!(
                "Peer {delivery_id} sent us a compact block but it failed to reconstruct, waiting on first download to complete"
            )
        ),
        "{lines:?}"
    );
    assert!(!t.conns[delivery].gone);

    t.send(stalling, "blocktxn", blocktxn_msg(block.hash(), &block.txs()[1..]));
    assert_eq!(t.tip().hash, block.hash());
}

/// S3-d: `test_compactblock_reconstruction_parallel_reconstruction`
/// (p2p_compactblocks.py:838-907): of three high-bandwidth announcers after a low-bandwidth
/// first one, the second gets `getblocktxn`, an inbound third does not, an outbound third does;
/// a late fill from the first is harmless.
// Core: net_processing.cpp:5160, 5199-5218
#[test]
fn s3d_reconstruction_parallel_reconstruction() {
    let mut t = Rig::new();
    let stalling = t.connect(1, false);
    let delivery = t.connect(2, false);
    let inbound = t.connect(3, false);
    let outbound = t.connect(4, true);
    // Set directly: the Python earns these by having each peer relay a block first.
    for i in [delivery, inbound, outbound] {
        t.conns[i].peer.handle.set_bip152_highbandwidth_to(true);
    }
    for num_missing in [1, 5, 20] {
        let block = t.build_block(num_missing);
        let c = compact(&block, &[0]);
        assert!(t.announce(stalling, &c).is_some(), "the first announcer, n = {num_missing}");
        assert!(t.announce(delivery, &c).is_some(), "the second gets getblocktxn");
        assert_ne!(t.tip().hash, block.hash());
        assert!(t.announce(inbound, &c).is_none(), "an inbound third does not");
        assert_ne!(t.tip().hash, block.hash());
        assert!(t.announce(outbound, &c).is_some(), "an outbound third does");
        assert_ne!(t.tip().hash, block.hash());

        let fill = blocktxn_msg(block.hash(), &block.txs()[1..]);
        t.send(delivery, "blocktxn", fill.clone());
        assert_eq!(t.tip().hash, block.hash());
        let stalling_id = t.id(stalling);
        let lines = t.send_logged(stalling, "blocktxn", fill);
        assert!(has(
            &lines,
            &format!(
                "Peer {stalling_id} sent us block transactions for block we weren't expecting"
            )
        ));
        assert!(t.conns.iter().all(|c| !c.gone));
    }
}

/// S3-e: `test_invalid_tx_in_compactblock` (p2p_compactblocks.py:725-745): a reconstructed
/// block that fails `ConnectBlock` does not get its compact source punished (`via_compact`).
// Core: net_processing.cpp:3801-3808, 1979-1995
#[test]
fn s3e_invalid_tx_in_compactblock() {
    let mut t = Rig::new();
    let p = t.connect(1, false);
    let block = t.build_block(4);
    t.sim.chain.mark_bad_connect(block.hash());
    let prev = t.tip().hash;
    let c = compact(&block, &[0, 1, 2, 3, 4]);
    assert_eq!(t.announce(p, &c), None);
    assert_eq!(t.tip().hash, prev);
    assert!(!t.conns[p].gone);
    assert_eq!(
        t.sim.chain.submits().last().map(|s| (s.hash, s.force, s.merkle_checked)),
        Some((block.hash(), true, true))
    );
}

/// S3-f: `test_invalid_cmpctblock_message` (p2p_compactblocks.py:266-277): a prefilled index past
/// the block is `READ_STATUS_INVALID`, scored 100.
// Core: net_processing.cpp:5174-5178
#[test]
fn s3f_invalid_cmpctblock_message() {
    let mut t = Rig::new();
    let p = t.connect(1, false);
    let block = t.build_block(0);
    let mut c = compact(&block, &[0]);
    c.prefilled[0].index = 1;
    let lines = t.send_logged(p, "cmpctblock", cmpct_msg(&c));
    assert!(t.conns[p].gone, "{lines:?}");
    assert!(has(&lines, "invalid compact block"), "{lines:?}");
    assert_eq!(t.tip().hash, block.header().raw().prev_hash);
}

/// S3-g: `test_multiple_blocktxn_response` (p2p_compactblocks.py:562-596): after a failed fill
/// falls back to `getdata`, a second `blocktxn` finds the header gone and is scored 100.
// Core: net_processing.cpp:3765-3773
#[test]
fn s3g_multiple_blocktxn_response() {
    let mut t = Rig::new();
    let p = t.connect(1, false);
    let block = t.build_block(2);
    let c = compact(&block, &[0]);
    assert_eq!(t.announce(p, &c), Some(vec![1, 2]));
    let prev = t.tip().hash;
    let swapped = [Arc::clone(&block.txs()[2]), Arc::clone(&block.txs()[1])];
    t.send(p, "blocktxn", blocktxn_msg(block.hash(), &swapped));
    assert_eq!(t.tip().hash, prev);
    t.conns[p].assert_getdata_block(block.hash());
    let id = t.id(p);
    let lines = t.send_logged(p, "blocktxn", blocktxn_msg(block.hash(), &swapped));
    assert!(has(&lines, "previous compact block reconstruction attempt failed"), "{lines:?}");
    assert!(has(&lines, &format!("Peer {id} sent compact block transactions multiple times")));
    assert!(t.conns[p].gone);
}

/// S2-e's compact half (np:3247-3255, np:1428-1431): a lone direct fetch from a peer that sent
/// `sendcmpct(_, 1)` asks `MSG_CMPCT_BLOCK`, in IBD too; with an empty mempool the `cmpctblock`
/// then gets `getblocktxn` for every non-prefilled index. Without `sendcmpct`, with
/// `-blocksonly`, or with two blocks to fetch, it stays `MSG_BLOCK`.
#[test]
fn s2e_direct_fetch_swaps_a_lone_request_to_msg_cmpct_block() {
    // The direct fetch for `n` new headers: the rig, the peer, the first block, the `getdata`.
    let run = |opts: ProcessorOptions, ibd: bool, sendcmpct: bool, n: usize| {
        let mut t = Rig::with(opts, ibd);
        let p = t.connect(1, true);
        if sendcmpct {
            t.send(p, "sendcmpct", sendcmpct_msg(false));
        }
        let mut blocks = vec![t.build_block(2)];
        while blocks.len() < n {
            let next = child(blocks.last().unwrap(), 100 + blocks.len() as u32);
            blocks.push(next);
        }
        t.conns[p].clear();
        t.send(p, "headers", headers_msg(&blocks.iter().collect::<Vec<_>>()));
        let gd = t.conns[p].getdata().expect("a direct fetch");
        assert_eq!(gd.len(), n);
        let types: Vec<InvType> = gd.iter().map(|i| i.inv_type).collect();
        (t, p, Arc::clone(&blocks[0]), types)
    };
    let blocks_only = ProcessorOptions {
        blocks_only: true,
        ..ProcessorOptions::default()
    };
    for ibd in [false, true] {
        let (mut t, p, block, types) = run(ProcessorOptions::default(), ibd, true, 1);
        assert_eq!(types, [InvType::CompactBlock], "ibd = {ibd}");
        assert_eq!(t.announce(p, &compact(&block, &[0])), Some(vec![1, 2]), "ibd = {ibd}");
        t.send(p, "blocktxn", blocktxn_msg(block.hash(), &block.txs()[1..]));
        assert_eq!(t.tip().hash, block.hash());
    }
    assert_eq!(
        run(ProcessorOptions::default(), false, false, 1).3,
        [InvType::Block],
        "no sendcmpct"
    );
    assert_eq!(run(blocks_only, false, true, 1).3, [InvType::Block], "-blocksonly");
    assert_eq!(
        run(ProcessorOptions::default(), false, true, 2).3,
        [InvType::Block; 2],
        "two blocks"
    );

    // One block to fetch on a connected parent, but a competing child already in flight from
    // another peer: `MSG_BLOCK`.
    let mut t = Rig::new();
    let other = t.connect(1, true);
    let p = t.connect(2, true);
    t.send(p, "sendcmpct", sendcmpct_msg(false));
    let parent = t.build_block(1);
    t.rpc_submit(&parent);
    assert_eq!(t.tip().hash, parent.hash());
    let (competing, ours) = (child(&parent, 100), child(&parent, 101));
    t.send(other, "headers", headers_msg(&[&competing]));
    assert_eq!(t.conns[other].getdata().map(|gd| gd.len()), Some(1));
    t.conns[p].clear();
    t.send(p, "headers", headers_msg(&[&ours]));
    t.conns[p].assert_getdata_block(ours.hash());

    // One block to fetch, nothing else in flight, but its parent's data is stored and not
    // connected (`pprev` not `BLOCK_VALID_CHAIN`): `MSG_BLOCK`.
    let mut t = Rig::new();
    let p = t.connect(1, true);
    t.send(p, "sendcmpct", sendcmpct_msg(false));
    let parent = t.build_block(1);
    let ours = child(&parent, 100);
    t.sim.chain.store_blocks(&[Arc::clone(&parent)]);
    t.conns[p].clear();
    t.send(p, "headers", headers_msg(&[&parent, &ours]));
    t.conns[p].assert_getdata_block(ours.hash());
}

/// Steps 12 and 13 of `cmpctblock` (np:5162-5164, 5238-5262), which no FT reaches: an
/// unrequested announcement more than two above the tip is processed as `headers` (here a direct
/// fetch); once it is in flight from this peer the same announcement gets `getdata(MSG_BLOCK)`.
/// Within two of the tip, a block already in flight as `MSG_BLOCK` from this peer is tracked
/// (`getblocktxn`), and a second announcement is "already syncing".
#[test]
fn cmpctblock_far_and_already_in_flight() {
    let mut t = Rig::new();
    let p = t.connect(1, true);
    let b1 = t.build_block(1);
    let b2 = child(&b1, 100);
    let b3 = child(&b2, 101);
    // The node knows b1 and b2's headers, from no peer.
    t.sim.chain.add(&[*b1.header().raw(), *b2.header().raw()]);
    t.conns[p].clear();
    let lines = t.send_logged(p, "cmpctblock", cmpct_msg(&compact(&b3, &[0])));
    let gd = t.conns[p].getdata().expect("the headers path's direct fetch");
    assert_eq!(gd.iter().map(block_hash).collect::<Vec<_>>(), [b1.hash(), b2.hash(), b3.hash()]);
    assert!(gd.iter().all(|i| i.inv_type == InvType::Block));
    assert!(has(&lines, &format!("Saw new cmpctblock header hash={}", b3.hash())), "{lines:?}");
    assert!(t.conns[p].getblocktxn().is_none());

    t.conns[p].clear();
    let lines = t.send_logged(p, "cmpctblock", cmpct_msg(&compact(&b3, &[0])));
    t.conns[p].assert_getdata_block(b3.hash());
    assert!(!has(&lines, "Saw new cmpctblock header"), "a known header: {lines:?}");
    assert!(t.conns[p].getblocktxn().is_none());

    assert_eq!(t.announce(p, &compact(&b1, &[0])), Some(vec![1]));
    let (again, lines) = capture(|| t.announce(p, &compact(&b1, &[0])));
    assert!(has(&lines, "Peer sent us compact block we were already syncing!"), "{lines:?}");
    assert_eq!(again, None);
    t.send(p, "blocktxn", blocktxn_msg(b1.hash(), &b1.txs()[1..]));
    assert_eq!(t.tip().hash, b1.hash());
    assert!(!t.conns[p].gone);
}

/// Step 10 (np:5145-5152): a block requested from this peer whose work is no longer above the
/// tip's (an equal-work sibling connected first) comes by `getdata(MSG_BLOCK)`.
#[test]
fn cmpctblock_requested_without_more_work_is_fetched_whole() {
    let mut t = Rig::new();
    let p = t.connect(1, true);
    let sibling = t.build_block(1);
    let tip = t.build_block(1);
    t.send(p, "headers", headers_msg(&[&sibling]));
    assert_eq!(t.conns[p].getdata().map(|gd| gd.len()), Some(1));
    // `submitblock` of an equal-work sibling.
    t.rpc_submit(&tip);
    assert_eq!(t.tip().hash, tip.hash());
    t.conns[p].clear();
    t.send(p, "cmpctblock", cmpct_msg(&compact(&sibling, &[0])));
    t.conns[p].assert_getdata_block(sibling.hash());
    assert!(t.conns[p].getblocktxn().is_none());
}

/// The untracked branch (np:5219-5236, 5266-5290): with three requests for the block in flight
/// a fourth announcer is untracked, and reconstructs it from the mempool; once it is stored every
/// request for it is cleared, so the first announcer's late `blocktxn` is unexpected. (Both
/// `ProcessBlock`'s clear for a new block and the branch's own clear after it do this; the
/// second matters only when another origin stored the block in between, P3 D92.)
#[test]
fn cmpctblock_untracked_reconstruction_clears_the_other_requests() {
    let mut t = Rig::new();
    let first = t.connect(1, false);
    let x = t.connect(2, true);
    let y = t.connect(3, true);
    let late = t.connect(4, false);
    for i in [x, y] {
        t.conns[i].peer.handle.set_bip152_highbandwidth_to(true);
    }
    let block = t.build_block(3);
    let c = compact(&block, &[0]);
    for i in [first, x, y] {
        assert!(t.announce(i, &c).is_some());
    }
    t.to_mempool(&block.txs()[1..]);
    assert_eq!(t.announce(late, &c), None);
    assert_eq!(t.tip().hash, block.hash());
    let first_id = t.id(first);
    let lines = t.send_logged(first, "blocktxn", blocktxn_msg(block.hash(), &block.txs()[1..]));
    assert!(
        has(
            &lines,
            &format!("Peer {first_id} sent us block transactions for block we weren't expecting")
        ),
        "{lines:?}"
    );
    assert!(t.conns.iter().all(|c| !c.gone));
}

/// S3-r, `cmpctblock` racing a connect (M2, D46): an RPC `submitblock A` connects A and B while
/// V serves Q's `ProcessHeaders` for C's header. Steps 6–13 read the post-reply drain's snapshot,
/// whose tip is B, so C (height 3) is within two of it and is reconstructed from its prefilled
/// coinbase; on the message-start snapshot (tip genesis) it would be three above, processed as
/// `headers`, and Q would be asked for it by `getdata`.
#[test]
fn s3r_cmpctblock_after_another_origins_connect_reads_the_redrain() {
    let mut t = Rig::new();
    let q = t.connect(1, true);
    let genesis = t.tip();
    let blocks = SimChain::build_blocks(&genesis, 3, 1, 0);
    let (a, b, c) = (Arc::clone(&blocks[0]), &blocks[1], &blocks[2]);
    // A's header is known and B's data stored, so connecting A connects B too.
    t.sim.chain.add(&[*a.header().raw()]);
    t.sim.chain.store_blocks(&[Arc::clone(b)]);
    let q_id = t.id(q);
    let chain = Arc::clone(&t.sim.chain);
    let armed = Arc::new(AtomicBool::new(true));
    let hook_armed = Arc::clone(&armed);
    t.sim.chain.set_headers_hook(Some(Arc::new(move |from| {
        if from == q_id && hook_armed.swap(false, Ordering::SeqCst) {
            rpc_submit(&chain, &a);
        }
    })));
    t.send(q, "cmpctblock", cmpct_msg(&CompactBlock::from_block(c, 0)));
    assert!(!armed.load(Ordering::SeqCst));
    assert_eq!(t.tip().hash, c.hash());
    assert!(t.conns[q].getdata().is_none());
}

/// np:5199-5216: a second announcer that is not a high-bandwidth peer of ours gets no
/// `getblocktxn` and its request is dropped, so its `blocktxn` is unexpected.
#[test]
fn cmpctblock_second_low_bandwidth_announcer_is_dropped() {
    let mut t = Rig::new();
    let first = t.connect(1, false);
    let second = t.connect(2, false);
    let block = t.build_block(3);
    let c = compact(&block, &[0]);
    assert_eq!(t.announce(first, &c), Some(vec![1, 2, 3]));
    assert_eq!(t.announce(second, &c), None);
    assert!(t.conns[second].getdata().is_none());
    let id = t.id(second);
    let lines = t.send_logged(second, "blocktxn", blocktxn_msg(block.hash(), &block.txs()[1..]));
    assert!(
        has(
            &lines,
            &format!("Peer {id} sent us block transactions for block we weren't expecting")
        ),
        "{lines:?}"
    );
    t.send(first, "blocktxn", blocktxn_msg(block.hash(), &block.txs()[1..]));
    assert_eq!(t.tip().hash, block.hash());
}

/// `InitData`'s `READ_STATUS_FAILED` (np:5179-5188), here 13 short IDs in one bucket: the first
/// announcer is asked `getdata(MSG_BLOCK)`; a later one (tracked as a high-bandwidth outbound
/// peer) is dropped without a request.
#[test]
fn cmpctblock_init_data_failed() {
    let mut t = Rig::new();
    let first = t.connect(1, true);
    let later = t.connect(2, true);
    t.conns[later].peer.handle.set_bip152_highbandwidth_to(true);
    let block = t.build_block(19);
    let mut c = compact(&block, &[]);
    c.short_ids = crowded(20, 13);
    t.conns[first].clear();
    t.send(first, "cmpctblock", cmpct_msg(&c));
    t.conns[first].assert_getdata_block(block.hash());
    assert!(t.conns[first].getblocktxn().is_none());

    assert_eq!(t.announce(later, &c), None);
    assert!(t.conns[later].getdata().is_none());
    let id = t.id(later);
    let lines = t.send_logged(later, "blocktxn", blocktxn_msg(block.hash(), &block.txs()[..1]));
    assert!(
        has(
            &lines,
            &format!("Peer {id} sent us block transactions for block we weren't expecting")
        ),
        "{lines:?}"
    );
    // Twelve in the bucket: no failure (the release table, P3 D90).
    c.short_ids = crowded(20, 12);
    let third = t.connect(3, true);
    t.conns[third].peer.handle.set_bip152_highbandwidth_to(true);
    assert_eq!(t.announce(third, &c).map(|idx| idx.len()), Some(20));
    assert!(t.conns.iter().all(|c| !c.gone));
}

/// np:5069-5075: a `cmpctblock` whose parent is unknown, out of IBD, asks `getheaders` from the
/// tip and is not scored.
#[test]
fn cmpctblock_with_an_unknown_parent_asks_for_headers() {
    let mut t = Rig::new();
    let p = t.connect(1, false);
    // An answer to the handshake's `getheaders` clears its timestamp (np:4823), as the Python's
    // `send_and_ping(msg_headers())` does not need to.
    t.send(p, "headers", headers_msg(&[]));
    let b1 = t.build_block(1);
    let b2 = child(&b1, 100);
    t.conns[p].clear();
    t.send(p, "cmpctblock", cmpct_msg(&compact(&b2, &[0])));
    let sent: Vec<&str> = t.conns[p].sent.iter().map(|(c, _)| c.as_str()).collect();
    assert!(sent.iter().any(|c| c.starts_with("getheaders")), "{sent:?}");
    assert!(t.conns[p].getdata().is_none() && t.conns[p].getblocktxn().is_none());
    assert!(t.sim.chain.by_hash(&b2.hash()).is_none());
    assert!(!t.conns[p].gone);
}

/// Step 5's `via_compact` (np:5085-5090): a known-invalid header (`BLOCK_CACHED_INVALID`) in an
/// outbound peer's `cmpctblock` is not scored; the same header in `headers` is.
#[test]
fn cmpctblock_with_a_cached_invalid_header_is_not_scored() {
    let mut t = Rig::new();
    let cmpct_peer = t.connect(1, true);
    let headers_peer = t.connect(2, true);
    let bad = t.build_block(1);
    t.sim.chain.mark_bad_connect(bad.hash());
    t.rpc_submit(&bad);
    assert_ne!(t.tip().hash, bad.hash());
    t.send(cmpct_peer, "cmpctblock", cmpct_msg(&compact(&bad, &[0])));
    assert!(!t.conns[cmpct_peer].gone);
    t.send(headers_peer, "headers", headers_msg(&[&bad]));
    assert!(t.conns[headers_peer].gone);
}

/// Step 11 (np:5153-5155): with the tip too old to direct-fetch and nothing in flight, a
/// `cmpctblock` within two of the tip is not tracked; the block is left to the parallel download
/// (`MSG_BLOCK`).
#[test]
fn cmpctblock_without_direct_fetch_is_left_to_the_download() {
    let mut t = Rig::new();
    let p = t.connect(1, true);
    t.sim.clock.set_mock_time(t.mocktime + 3600);
    let block = t.build_block(2);
    t.conns[p].clear();
    t.send(p, "cmpctblock", cmpct_msg(&compact(&block, &[0])));
    assert!(t.conns[p].getblocktxn().is_none());
    t.conns[p].assert_getdata_block(block.hash());
}

/// Step 11's `!already_in_flight` (np:5153): with the tip as old, a block the parallel download
/// already asked one peer for is tracked for a high-bandwidth announcer, which is asked for the
/// missing transactions.
#[test]
fn cmpctblock_without_direct_fetch_tracks_a_block_in_flight() {
    let mut t = Rig::new();
    let p = t.connect(1, true);
    let q = t.connect(2, true);
    t.conns[q].peer.handle.set_bip152_highbandwidth_to(true);
    t.sim.clock.set_mock_time(t.mocktime + 3600);
    let block = t.build_block(2);
    t.conns[p].clear();
    t.send(p, "cmpctblock", cmpct_msg(&compact(&block, &[0])));
    t.conns[p].assert_getdata_block(block.hash());
    assert_eq!(t.announce(q, &compact(&block, &[0])), Some(vec![1, 2]));
}

/// Step 9 (np:5117-5120): a `cmpctblock` with a new header of more work than the tip stamps
/// `m_last_block_announcement`, which `EvictExtraOutboundPeers` reads; one whose header the node
/// already has does not.
#[test]
fn cmpctblock_with_a_new_tip_stamps_the_announcement() {
    let mut t = Rig::new();
    let p = t.connect(1, true);
    let announced =
        |t: &Rig| t.sim.shared.with_state(t.id(p), |s| s.last_block_announcement).unwrap();
    assert_eq!(announced(&t), 0);
    let block = t.build_block(1);
    t.sim.clock.set_mock_time(t.mocktime + 7);
    t.announce(p, &compact(&block, &[0]));
    assert_eq!(announced(&t), t.mocktime + 7);
    t.sim.clock.set_mock_time(t.mocktime + 9);
    t.announce(p, &compact(&block, &[0]));
    assert_eq!(announced(&t), t.mocktime + 7);
}

/// The untracked branch's source is `(peer, false)` (np:5268-5271): a reconstructed block that
/// fails `ConnectBlock` does not get its announcer scored.
#[test]
fn cmpctblock_untracked_invalid_block_is_not_scored() {
    let mut t = Rig::new();
    let first = t.connect(1, false);
    let x = t.connect(2, true);
    let y = t.connect(3, true);
    let late = t.connect(4, true);
    for i in [x, y] {
        t.conns[i].peer.handle.set_bip152_highbandwidth_to(true);
    }
    let block = t.build_block(3);
    t.sim.chain.mark_bad_connect(block.hash());
    let c = compact(&block, &[0]);
    for i in [first, x, y] {
        assert!(t.announce(i, &c).is_some());
    }
    t.to_mempool(&block.txs()[1..]);
    let prev = t.tip().hash;
    assert_eq!(t.announce(late, &c), None);
    assert_eq!(
        t.sim.chain.submits().last().map(|s| (s.hash, s.from)),
        Some((block.hash(), t.id(late)))
    );
    assert_eq!(t.tip().hash, prev);
    assert!(t.conns.iter().all(|c| !c.gone));
}

// ---- S4: high-bandwidth selection and compact announcements

/// S4-a: `test_sendcmpct` (p2p_compactblocks.py:188-264) for the Python's four peers, three
/// inbound and one outbound full-relay: only a version-1 `sendcmpct(1)` gets new blocks as
/// `cmpctblock`, and `sendcmpct(0)` turns it off. Every peer here is a v24 full node with
/// `NODE_HEADERS_COMPRESSED`, where the Python gives that bit to its second peer only; neither
/// `SENDCMPCT` nor the announcement choice reads it.
// Core: net_processing.cpp:2228-2268, 4273-4288, 6478-6494
#[test]
fn s4a_sendcmpct() {
    for outbound in [false, false, false, true] {
        let mut r = Rig::new();
        let i = r.connect(1, outbound);
        let greeting = &r.conns[i].greeting;
        let sendcmpct: Vec<_> =
            greeting.iter().filter(|(c, _)| c.as_str() == "sendcmpct").collect();
        assert_eq!(sendcmpct.len(), 1);
        assert_eq!(sendcmpct[0].1, sendcmpct_version(false, 1), "version 1, low bandwidth");
        let tip = r.tip().hash;
        let check = |r: &mut Rig, pred: &dyn Fn(&[&str]) -> bool| {
            let b = r.generate(0);
            let got = r.conns[i].announced(b.hash());
            assert!(pred(&got), "outbound={outbound}: {got:?}");
        };
        check(&mut r, &|a| !a.contains(&"cmpctblock"));
        r.headers_sync(i, tip);
        check(&mut r, &|a| !a.contains(&"cmpctblock") && a.contains(&"inv"));
        for (hb, version) in [(true, 0), (true, 2), (false, 1)] {
            r.headers_sync(i, tip);
            r.send(i, "sendcmpct", sendcmpct_version(hb, version));
            check(&mut r, &|a| !a.contains(&"cmpctblock"));
        }
        r.headers_sync(i, tip);
        r.send(i, "sendcmpct", sendcmpct_msg(true));
        check(&mut r, &|a| a.contains(&"cmpctblock"));
        // No headers sync needed: the push set `pindexBestHeaderSent`.
        check(&mut r, &|a| a.contains(&"cmpctblock"));
        r.send(i, "sendheaders", Vec::new());
        check(&mut r, &|a| a.contains(&"cmpctblock"));
        r.send(i, "sendcmpct", sendcmpct_version(false, 0));
        check(&mut r, &|a| a.contains(&"cmpctblock"));
        r.send(i, "sendcmpct", sendcmpct_msg(false));
        check(&mut r, &|a| !a.contains(&"cmpctblock") && a.contains(&"headers"));
    }
}

/// S4-a: `test_highbandwidth_mode_states_via_getpeerinfo` (p2p_compactblocks.py:810-836):
/// `bip152_hb_from` follows the peer's `sendcmpct`, `bip152_hb_to` the node's choice after the
/// peer relays a new block.
// Core: net_processing.cpp:1360-1416, 2314-2343, 4273-4288
#[test]
fn s4a_highbandwidth_mode_states_via_getpeerinfo() {
    let mut r = Rig::new();
    let i = r.connect(1, false);
    let states = |r: &Rig| {
        let h = &r.conns[i].peer.handle;
        (h.bip152_highbandwidth_to(), h.bip152_highbandwidth_from())
    };
    assert_eq!(states(&r), (false, false));
    r.send(i, "sendcmpct", sendcmpct_msg(true));
    assert_eq!(states(&r), (false, true));
    let block = r.build_block(0);
    r.send(i, "block", block_msg(&block));
    assert_eq!(r.tip().hash, block.hash());
    assert_eq!(states(&r), (true, true));
    assert_eq!(r.conns[i].last("sendcmpct"), Some(&sendcmpct_msg(true)[..]));
    r.send(i, "sendcmpct", sendcmpct_msg(false));
    assert_eq!(states(&r), (true, false));
}

/// S4-a: `test_end_to_end_block_relay` (p2p_compactblocks.py:705-722): both listeners get the
/// `submitblock`'s block as a `cmpctblock` from `NewPoWValidBlock`, one encoding with one nonce,
/// which is the encoder's for that nonce.
// Core: net_processing.cpp:2228-2268
#[test]
fn s4a_end_to_end_block_relay() {
    let mut r = Rig::new();
    let listeners = [r.connect(1, false), r.connect(2, false)];
    for &l in &listeners {
        r.request_cb_announcements(l);
    }
    let block = r.build_block(10);
    let before = r.draws();
    r.submit_and_notify(&block);
    // `NewPoWValidBlock`'s one draw; section 7 then finds both peers have the header.
    assert_eq!(r.draws(), before + 1);
    let recent = r.sim.shared.signals().recent();
    assert_eq!(recent.hash, Some(block.hash()));
    for &l in &listeners {
        assert_eq!(r.conns[l].announced(block.hash()), ["cmpctblock"]);
        let got = r.conns[l].last("cmpctblock").unwrap();
        assert_eq!(cmpct_header(got).hash(), block.hash());
        assert_eq!(got, recent.cmpct.as_deref().unwrap());
        assert_eq!(reencode(&block, got), got);
    }
}

/// m-2 (P3 D28): `submitblock` of a block whose header is new, with an HB peer that announced
/// it by `inv` first. `NewPoWValidBlock`'s `ProcessBlockAvailability` resolves the peer's
/// `hashLastUnknownBlock` to the new entry, so the peer has the header and gets no
/// `cmpctblock`, from the visit or from section 7.
// Core: net_processing.cpp:1442-1456, 2252-2259
#[test]
fn m2_a_new_header_submitblock_skips_the_inv_announcing_peer() {
    let mut r = Rig::new();
    let (p, q) = (r.connect(1, false), r.connect(2, false));
    r.request_cb_announcements(p);
    r.request_cb_announcements(q);
    let block = r.build_block(1);
    r.send(p, "inv", inv_msg(&[block.hash()]));
    let unknown = r.sim.shared.with_state(r.id(p), |s| s.headers().last_unknown_block).unwrap();
    assert_eq!(unknown, Some(block.hash()));
    assert!(r.sim.chain.by_hash(&block.hash()).is_none(), "the header is new to the node");
    r.submit_and_notify(&block);
    assert!(r.conns[p].last("cmpctblock").is_none(), "{:?}", r.conns[p].sent);
    assert_eq!(r.conns[q].announced(block.hash()), ["cmpctblock"]);
    let known = r.sim.shared.with_state(r.id(p), |s| s.headers().best_known_block).unwrap();
    assert_eq!(known, r.sim.chain.by_hash(&block.hash()).map(|e| e.id));
}

/// Section 7's `cmpctblock` branch reuses `NewPoWValidBlock`'s encoding when the block to
/// announce is `m_most_recent_block_hash`, with no draw: here the peer asked for high
/// bandwidth after V's visit passed it.
// Core: net_processing.cpp:6478-6487
#[test]
fn section7_sends_the_cached_compact_block_without_a_draw() {
    let mut r = Rig::new();
    let p = r.connect(1, false);
    let prev = r.tip();
    r.headers_sync(p, prev.hash);
    let block = r.build_block(2);
    rpc_submit(&r.sim.chain, &block);
    r.send(p, "sendcmpct", sendcmpct_msg(true));
    assert!(r.conns[p].last("cmpctblock").is_none(), "the visit passed a low-bandwidth peer");
    let before = r.draws();
    r.updated_block_tip(&prev);
    r.sync();
    assert_eq!(r.draws(), before);
    let recent = r.sim.shared.signals().recent();
    assert_eq!(r.conns[p].last("cmpctblock"), recent.cmpct.as_deref());
}

/// The branch's other path: a block that is not `m_most_recent_block_hash` (connected by
/// another origin, which calls no `NewPoWValidBlock`) is read from disk and encoded with a
/// fresh nonce, one draw.
// Core: net_processing.cpp:6488-6494
#[test]
fn section7_encodes_another_block_from_disk_with_one_draw() {
    let mut r = Rig::new();
    let p = r.connect(1, false);
    r.request_cb_announcements(p);
    let first = r.generate(0);
    assert_eq!(r.conns[p].announced(first.hash()), ["cmpctblock"]);
    let block = r.build_block(3);
    let prev = r.tip();
    r.sim.chain.store_blocks(&[Arc::clone(&block)]);
    r.sim.chain.set_tip(r.block_id(&block));
    r.conns[p].clear();
    let before = r.draws();
    r.updated_block_tip(&prev);
    r.sync();
    assert_eq!(r.draws(), before + 1);
    assert_eq!(r.sim.shared.signals().recent().hash, Some(first.hash()));
    let got = r.conns[p].last("cmpctblock").expect("a cmpctblock");
    assert_eq!(cmpct_header(got).hash(), block.hash());
    assert_eq!(reencode(&block, got), got);
}

/// `BlockChecked`'s HB branch runs only outside IBD, for a valid block that is every entry in
/// flight, and not under `-blocksonly`: a peer relaying a new block unrequested is chosen
/// (`m_bip152_highbandwidth_to`, a `sendcmpct(1)`) only then.
// Core: net_processing.cpp:2314-2343
#[test]
fn block_checked_chooses_the_source_only_when_cores_conditions_hold() {
    let relays = |r: &mut Rig, i: usize| {
        let block = r.build_block(0);
        r.send(i, "block", block_msg(&block));
        let h = &r.conns[i].peer.handle;
        let sendcmpct = r.conns[i].last("sendcmpct").map(<[u8]>::to_vec);
        (r.tip().hash == block.hash(), h.bip152_highbandwidth_to(), sendcmpct)
    };
    // The control: chosen.
    let mut r = Rig::new();
    let p = r.connect(1, false);
    r.send(p, "sendcmpct", sendcmpct_msg(false));
    assert_eq!(relays(&mut r, p), (true, true, Some(sendcmpct_msg(true))));
    // In IBD.
    let mut r = Rig::with(ProcessorOptions::default(), true);
    let p = r.connect(1, false);
    r.send(p, "sendcmpct", sendcmpct_msg(false));
    assert_eq!(relays(&mut r, p), (true, false, None));
    // `-blocksonly`.
    let opts = ProcessorOptions {
        blocks_only: true,
        ..ProcessorOptions::default()
    };
    let mut r = Rig::with(opts, false);
    let p = r.connect(1, false);
    r.send(p, "sendcmpct", sendcmpct_msg(false));
    assert_eq!(relays(&mut r, p), (true, false, None));
    // A peer that never sent `sendcmpct` is not a candidate (`m_provides_cmpctblocks`).
    let mut r = Rig::new();
    let p = r.connect(1, false);
    assert_eq!(relays(&mut r, p), (true, false, None));
    // Another block in flight: `mapBlocksInFlight.count(hash) != mapBlocksInFlight.size()`.
    let mut r = Rig::new();
    let (p, q) = (r.connect(1, false), r.connect(2, true));
    r.send(p, "sendcmpct", sendcmpct_msg(false));
    r.nonce += 100;
    let other = r.build_block(0);
    r.send(q, "headers", headers_msg(&[&other]));
    assert!(r.conns[q].last("getdata").is_some(), "the other block in flight from q");
    assert_eq!(relays(&mut r, p), (true, false, None));
}

/// An invalid block's source is punished and never chosen.
// Core: net_processing.cpp:2322-2327
#[test]
fn block_checked_never_chooses_the_source_of_an_invalid_block() {
    let mut r = Rig::new();
    let p = r.connect(1, false);
    r.send(p, "sendcmpct", sendcmpct_msg(false));
    let block = r.build_block(0);
    r.sim.chain.mark_bad_connect(block.hash());
    r.send(p, "block", block_msg(&block));
    assert_ne!(r.tip().hash, block.hash());
    assert!(!r.conns[p].peer.handle.bip152_highbandwidth_to());
    assert!(r.conns[p].last("sendcmpct").is_none());
    assert!(r.conns[p].gone, "punished: {:?}", r.conns[p].sent);
}

// ---- S4-x: the ordering matrix (P3 §6.2)

/// A peer's header preference, the matrix's second axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pref {
    Headers,
    Headers2,
    Neither,
}

const PREFS: [Pref; 3] = [Pref::Headers, Pref::Headers2, Pref::Neither];

/// Block names for [`Rig::script`].
type Names = Vec<(&'static str, BlockHash)>;

fn name(names: &Names, h: &BlockHash) -> &'static str {
    names.iter().find(|(_, x)| x == h).map_or("?", |(n, _)| n)
}

/// One frame as the S4-x expectations write it: an announcement with its blocks, a `getheaders`
/// (or `getheaders2`) with its locator's first entry, a `sendcmpct` with its flag; `None` for
/// any other frame.
fn frame(names: &Names, cmd: &str, payload: &[u8]) -> Option<String> {
    let list = |hs: Vec<BlockHash>| hs.iter().map(|h| name(names, h)).collect::<Vec<_>>().join(",");
    let body = match cmd {
        "inv" | "headers" | "cmpctblock" => list(announced_hashes(cmd, payload)),
        "headers2" => {
            let h = Headers2::decode(&mut &payload[..]).expect("a headers2");
            list(h.block_headers().map(|h| Header::new(*h).hash()).collect())
        }
        // `getheaders2` has `getheaders`' layout.
        "getheaders" | "getheaders2" => {
            let g = GetHeaders::decode(&mut &payload[..]).expect("a getheaders");
            name(names, &g.locator_hashes[0]).to_owned()
        }
        "sendcmpct" => {
            let m = SendCmpct::decode(&mut &payload[..]).expect("a sendcmpct");
            u8::from(m.high_bandwidth).to_string()
        }
        _ => return None,
    };
    Some(format!("{cmd} {body}"))
}

impl Pref {
    /// The message that sets this preference, if any.
    fn command(self) -> Option<&'static str> {
        match self {
            Pref::Headers => Some("sendheaders"),
            Pref::Headers2 => Some("sendheaders2"),
            Pref::Neither => None,
        }
    }

    /// The announcement a peer with this preference gets for `blocks` from section 7 when it
    /// reverts to `inv` only for the `Neither` preference.
    fn headers(self, blocks: &str) -> String {
        match self {
            Pref::Headers => format!("headers {blocks}"),
            Pref::Headers2 => format!("headers2 {blocks}"),
            Pref::Neither => format!("inv {}", blocks.rsplit(',').next().unwrap()),
        }
    }
}

impl Rig {
    /// Peer `i` sends its header preference.
    fn prefer(&mut self, i: usize, pref: Pref) {
        if let Some(cmd) = pref.command() {
            self.send(i, cmd, Vec::new());
        }
    }

    /// Scripted inbound peer `n`, which has sent its `version` and nothing after it, so it is
    /// not yet fully connected.
    fn attach(&mut self, n: u8) -> usize {
        let peer = self.sim.accept(n, Default::default()).expect("inbound admitted");
        self.sim.deliver(&peer, &[("version", PeerVersionMsg::v24(self.mocktime).payload())]);
        self.sim.pass();
        self.conns.push(Conn {
            greeting: self.sim.sent(&peer),
            peer,
            sent: Vec::new(),
            gone: false,
        });
        self.conns.len() - 1
    }

    /// A high-bandwidth compact peer with header preference `pref` that has the tip
    /// (`request_cb_announcements`).
    fn hb_peer(&mut self, n: u8, outbound: bool, pref: Pref) -> usize {
        let i = self.connect(n, outbound);
        self.prefer(i, pref);
        self.request_cb_announcements(i);
        self.clear_pushes();
        i
    }

    /// The pushes to peer `i` since the last [`Rig::clear_pushes`], as [`frame`]s.
    fn script(&self, i: usize, names: &Names) -> Vec<String> {
        let addr = self.conns[i].peer.handle.addr();
        let pushes = self.pushes.0.lock();
        pushes
            .iter()
            .filter(|(a, ..)| a == addr)
            .filter_map(|(_, c, p)| frame(names, c, p))
            .collect()
    }

    /// Every push since the last clear, `(peer index, frame)` in push order.
    fn pushes(&self, names: &Names) -> Vec<(usize, String)> {
        let pushes = self.pushes.0.lock();
        pushes
            .iter()
            .filter_map(|(a, c, p)| {
                let i = self.conns.iter().position(|x| x.peer.handle.addr() == a)?;
                Some((i, frame(names, c, p)?))
            })
            .collect()
    }

    /// The `getheaders` command the node uses with peer `i` (`getheaders2` to a
    /// `NODE_HEADERS_COMPRESSED` peer).
    fn getheaders_cmd(&self, i: usize) -> String {
        dashd_p2p::processing::sync::getheaders_type(&self.conns[i].peer.handle).as_str().to_owned()
    }

    fn clear_pushes(&self) {
        self.pushes.0.lock().clear();
    }

    /// One head of `send_messages` and its sections for peer `i`, outside a message-loop pass.
    fn head(&mut self, i: usize) {
        let handle = self.conns[i].peer.handle.clone();
        self.sim.lp.logic_mut().send_messages(&handle);
    }

    /// `UpdatedBlockTip` for the tip after `prev`, outside IBD.
    fn updated_block_tip(&self, prev: &EntryFacts) {
        let tip = self.tip();
        let new = self.sim.chain.new_blocks(tip.id, Some(prev.id));
        self.sim.shared.updated_block_tip(&self.sim.core, &new, false);
    }
}

/// What S4-x (6)'s two pauses saw: each HB peer's `cmpctblock` pushes so far, and whether the
/// block was connected.
type Pauses = Arc<parking_lot::Mutex<Vec<(&'static str, Vec<usize>, bool)>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    Rpc,
    P2p,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Actor {
    /// No message in progress: an RPC block from the test thread, or a P2P block the actor
    /// accepts inline (no V thread).
    Idle,
    /// The actor waits on V for a block (V on its own thread).
    BlockWait,
    /// The actor is in another peer's message region.
    Region,
}

/// S4-x (6), accept then connect (F1), and the timing probe's ordering gate on `SimChain` (§6.3
/// item 6, D52): `SimChain` pauses right after the `NewPoWValidBlock` call, before the write,
/// and again before the accept's first `ConnectTip`. At both pauses every HB peer whose header
/// tests pass already holds `cmpctblock(B)`, for an RPC block and a P2P one, with the actor
/// idle, in its block wait, or inside another peer's message, which the call waited for.
// Core: validation.cpp:4831-4832, net_processing.cpp:2228-2268
#[test]
fn s4x6_accept_then_connect() {
    for pref in PREFS {
        for (origin, actor) in [
            (Origin::Rpc, Actor::Idle),
            (Origin::Rpc, Actor::BlockWait),
            (Origin::Rpc, Actor::Region),
            (Origin::P2p, Actor::Idle),
            (Origin::P2p, Actor::BlockWait),
        ] {
            s4x6_case(pref, origin, actor);
        }
    }
}

fn s4x6_case(pref: Pref, origin: Origin, actor: Actor) {
    let case = format!("{pref:?} {origin:?} {actor:?}");
    let mut r = Rig::new();
    // In table order: HB peer a, the region's peer z, HB peer b, the P2P source x.
    let a = r.hb_peer(1, false, pref);
    let z = r.connect(2, false);
    let b = r.hb_peer(3, true, pref);
    let x = r.connect(4, true);
    if actor == Actor::BlockWait {
        r.sim.chain.spawn_v();
    }
    let block = r.build_block(2);
    let names: Names = vec![("B", block.hash())];
    let prev = r.tip();
    let pauses: Pauses = Arc::default();
    let region_done = Arc::new(AtomicBool::new(actor != Actor::Region));
    let pause = |at: &'static str| {
        let (pauses, chain, done, pushes) = (
            Arc::clone(&pauses),
            Arc::clone(&r.sim.chain),
            Arc::clone(&region_done),
            Arc::clone(&r.pushes),
        );
        let addrs = [a, b].map(|i| r.conns[i].peer.handle.addr().clone());
        let hash = block.hash();
        let hook: AnnounceHook = Arc::new(move |_, e: &EntryFacts| {
            if e.hash != hash {
                return;
            }
            assert!(done.load(Ordering::SeqCst), "{at}: the call waited for the region");
            let held = addrs
                .iter()
                .map(|addr| {
                    let p = pushes.0.lock();
                    p.iter().filter(|(x, c, _)| x == addr && c == "cmpctblock").count()
                })
                .collect();
            let connected = chain.snapshot().tip.tip == e.id;
            pauses.lock().push((at, held, connected));
        });
        hook
    };
    r.sim.chain.set_announce_hook(Some(pause("announce")));
    r.sim.chain.set_connect_hook(Some(pause("connect")));
    match (origin, actor) {
        (Origin::Rpc, Actor::Idle) => rpc_submit(&r.sim.chain, &block),
        (Origin::Rpc, Actor::BlockWait) => {
            // B's accept runs on V while the actor waits on V for x's sibling y.
            r.nonce += 100;
            let y = r.build_block(0);
            let (chain, b2, y_hash) = (Arc::clone(&r.sim.chain), Arc::clone(&block), y.hash());
            let armed = AtomicBool::new(true);
            r.sim.chain.set_accept_hook(Some(Arc::new(move |s: &BlockSubmit| {
                if s.hash == y_hash && armed.swap(false, Ordering::SeqCst) {
                    rpc_submit(&chain, &b2);
                }
            })));
            r.send(x, "block", block_msg(&y));
        }
        (Origin::Rpc, Actor::Region) => {
            let (chain, b2) = (Arc::clone(&r.sim.chain), Arc::clone(&block));
            let (ha, hb) =
                (r.conns[a].peer.handle.addr().clone(), r.conns[b].peer.handle.addr().clone());
            let (pushes, done) = (Arc::clone(&r.pushes), Arc::clone(&region_done));
            let visiting = reached(&r, LockPoint::Visit(r.id(z)));
            let v = Arc::new(parking_lot::Mutex::new(None));
            let v2 = Arc::clone(&v);
            *r.hold.lock() = Some(Box::new(move || {
                *v2.lock() = Some(std::thread::spawn(move || rpc_submit(&chain, &b2)));
                let count =
                    |addr: &Address| pushes.0.lock().iter().filter(|(x, ..)| x == addr).count();
                // The visit pushes to a, before z in the table, then waits on z's cell only.
                until(|| visiting.load(Ordering::SeqCst));
                assert_eq!(count(&ha), 1, "the visit pushed to a");
                assert_eq!(count(&hb), 0, "the visit waits on z's cell");
                done.store(true, Ordering::SeqCst);
            }));
            r.send(z, "qsigrec", Vec::new());
            let v = v.lock().take().expect("V's thread");
            v.join().unwrap();
            r.sim.shared.signals().set_lock_hook(None);
        }
        (Origin::P2p, _) => r.send(x, "block", block_msg(&block)),
    }
    assert_eq!(r.tip().hash, block.hash(), "{case}");
    assert_eq!(
        *pauses.lock(),
        [("announce", vec![1, 1], false), ("connect", vec![1, 1], false)],
        "{case}"
    );
    r.updated_block_tip(&prev);
    r.sync();
    for i in [a, b] {
        assert_eq!(r.script(i, &names), ["cmpctblock B"], "{case}: peer {i}");
    }
    assert!(r.script(x, &names).iter().all(|f| !f.starts_with("cmpctblock")), "{case}");
}

/// S4-x (10), a call between the cell and the cache copy (Sol r10 F1, D58, D59). Tip T is
/// `recent`; HB peer P knows A, T's parent, has not started sync, and has U(T) pending. P's head
/// pauses after it locks P's cell, before and after its stamp read, while RPC accepts C on T,
/// which stores its stamp, sets `recent = C` and waits on P's cell. After the read: a locator
/// from A, T's cached compact block with no draw, then `cmpctblock(C)`. Before it: `Ahead`, a
/// locator from T, `cmpctblock(T)` encoded from disk with one draw, and no `cmpctblock(C)`. The
/// after-read case is the one rev10's order failed.
// Core: net_processing.cpp:2228-2268, 6380-6394, 6478-6494
#[test]
fn s4x10_a_call_between_the_cell_and_the_cache_copy() {
    for pref in PREFS {
        for after_read in [false, true] {
            s4x10_case(pref, after_read);
        }
    }
}

fn s4x10_case(pref: Pref, after_read: bool) {
    let case = format!("{pref:?} after_read={after_read}");
    let mut r = Rig::new();
    let p = r.connect(1, false);
    r.prefer(p, pref);
    let a = r.generate(0);
    // T by RPC while P is low-bandwidth: `recent` is T and the visit skipped P.
    let t = r.build_block(1);
    let at = r.tip();
    rpc_submit(&r.sim.chain, &t);
    r.send(p, "sendcmpct", sendcmpct_msg(true));
    let a_id = r.block_id(&a);
    // "Has not started sync": as if the connect's initial `getheaders` was never sent.
    r.sim.shared.with_state_mut(r.id(p), |s| {
        s.sync_started = false;
        s.last_getheaders_us = 0;
        let h = s.headers_mut();
        h.best_known_block = Some(a_id);
        h.best_header_sent = None;
    });
    r.updated_block_tip(&at);
    let cached_t = r.sim.shared.signals().recent().cmpct.expect("T's encoding");
    let c = r.build_block(2);
    let names: Names = vec![("A", a.hash()), ("T", t.hash()), ("C", c.hash())];
    r.clear_pushes();
    let point = if after_read {
        LockPoint::StampRead
    } else {
        LockPoint::Locked
    };
    let armed = Arc::new(AtomicBool::new(true));
    let v: VThread = Arc::default();
    {
        let (armed, v, chain, shared, c) = (
            Arc::clone(&armed),
            Arc::clone(&v),
            Arc::clone(&r.sim.chain),
            Arc::clone(&r.sim.shared),
            Arc::clone(&c),
        );
        r.sim.shared.signals().set_lock_hook(Some(Arc::new(move |at| {
            if at == point && armed.swap(false, Ordering::SeqCst) {
                let (chain, c2) = (Arc::clone(&chain), Arc::clone(&c));
                spawn_into(&v, move || rpc_submit(&chain, &c2));
                // V has stored C's stamp and `recent`, and goes on to P's cell.
                until(|| shared.signals().recent().hash == Some(c.hash()));
            }
        })));
    }
    let (draws, retries) = (r.draws(), r.sim.lp.logic().region_retries);
    r.head(p);
    r.sim.shared.signals().set_lock_hook(None);
    assert!(!armed.load(Ordering::SeqCst), "{case}: the pause ran");
    join(&v);
    assert_eq!(r.tip().hash, c.hash());
    let got = r.script(p, &names);
    let cmpct_t = r
        .pushes
        .0
        .lock()
        .iter()
        .find(|(_, cmd, pl)| cmd == "cmpctblock" && cmpct_header(pl).hash() == t.hash())
        .map(|(.., pl)| pl.clone())
        .expect("cmpctblock(T)");
    if after_read {
        assert_eq!(
            got,
            [&*format!("{} A", r.getheaders_cmd(p)), "cmpctblock T", "cmpctblock C"],
            "{case}"
        );
        assert_eq!(&cmpct_t[..], &cached_t[..], "{case}: T's cached encoding");
        assert_eq!(r.draws(), draws + 1, "{case}: C's draw only");
        assert_eq!(r.sim.lp.logic().region_retries, retries, "{case}");
    } else {
        assert_eq!(got, [&*format!("{} T", r.getheaders_cmd(p)), "cmpctblock T"], "{case}");
        assert_ne!(&cmpct_t[..], &cached_t[..], "{case}: a fresh encoding of T");
        assert_eq!(reencode(&t, &cmpct_t), cmpct_t, "{case}");
        assert_eq!(r.draws(), draws + 2, "{case}: C's draw and T's");
        assert_eq!(r.sim.lp.logic().region_retries, retries + 1, "{case}: Ahead");
    }
}

/// The frames of `script` that announce blocks.
fn announcements(script: Vec<String>) -> Vec<String> {
    let kinds = ["inv ", "headers ", "headers2 ", "cmpctblock "];
    script.into_iter().filter(|f| kinds.iter().any(|k| f.starts_with(k))).collect()
}

/// Arms `hook` for the next [`SimRead::DrainSnapshot`] only.
fn at_next_drain(r: &Rig, hook: impl FnOnce() + Send + 'static) {
    let hook = parking_lot::Mutex::new(Some(hook));
    r.sim.chain.set_read_hook(Some(Arc::new(move |read| {
        if read == SimRead::DrainSnapshot
            && let Some(h) = hook.lock().take()
        {
            h();
        }
    })));
}

/// Arms `hook` for the next [`LockPoint`] `at` only.
fn at_next_lock(r: &Rig, at: LockPoint, hook: impl FnOnce() + Send + 'static) {
    let hook = parking_lot::Mutex::new(Some(hook));
    r.sim.shared.signals().set_lock_hook(Some(Arc::new(move |point| {
        if point == at
            && let Some(h) = hook.lock().take()
        {
            h();
        }
    })));
}

/// Set once a thread reaches the [`LockPoint`] `at`: a positive sign that V's visit or a relay
/// is about to lock (and, while a region holds it, wait on) a peer's cell.
fn reached(r: &Rig, at: LockPoint) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let f = Arc::clone(&flag);
    r.sim.shared.signals().set_lock_hook(Some(Arc::new(move |point| {
        if point == at {
            f.store(true, Ordering::SeqCst);
        }
    })));
    flag
}

/// A thread standing in for V (or for the scheduler's `UpdatedBlockTip`), kept for a join.
type VThread = Arc<parking_lot::Mutex<Option<std::thread::JoinHandle<()>>>>;

fn spawn_into(slot: &VThread, f: impl FnOnce() + Send + 'static) {
    *slot.lock() = Some(std::thread::spawn(f));
}

fn join(slot: &VThread) {
    if let Some(h) = slot.lock().take() {
        h.join().unwrap();
    }
}

/// Pauses V right after `hash`'s `NewPoWValidBlock` call, before the write, until a caller has
/// started waiting for a publication (`wait_published`): an `Ahead` is then certain.
fn hold_after_visit(r: &Rig, hash: BlockHash) {
    let chain = Arc::clone(&r.sim.chain);
    let waits = chain.published_waits();
    r.sim.chain.set_announce_hook(Some(Arc::new(move |_, e: &EntryFacts| {
        if e.hash == hash {
            until(|| chain.published_waits() > waits);
        }
    })));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VAt {
    /// B's accept runs before the head.
    BeforeHead,
    /// At the head's cell lock, before its stamp read: `Ahead`.
    Locked,
    /// After the head's stamp read: the visit waits for the head's region.
    StampRead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UAt {
    BeforeHead,
    /// While the head holds the `block_inv` cell.
    UnderBlockInv,
    /// After the head released the `block_inv` cell, with the header cell held.
    AfterUnlock,
    /// After the head.
    NextRound,
}

/// S4-x (1), a stored descendant (Sol r2). A valid descendant D of B is stored, and B arrives
/// by RPC. B's visit of the peer runs before the head takes the header cell, or waits for the
/// head's region; B's publication lands before S or after it (`Ahead`); and U appends before
/// the head locks the `block_inv` cell, while it holds it, or after it unlocks. Headers
/// preferred: `cmpctblock(B)` then `cmpctblock(D)`; neither: `cmpctblock(B)` then `inv(D)`; never
/// `headers(B,D)`, `headers2(B,D)`, or `inv(D)` before `cmpctblock(B)`.
// Core: net_processing.cpp:2228-2268, 6413-6494
#[test]
fn s4x1_stored_descendant() {
    for pref in PREFS {
        for u_at in [UAt::BeforeHead, UAt::UnderBlockInv, UAt::AfterUnlock, UAt::NextRound] {
            s4x1_case(pref, VAt::BeforeHead, u_at);
        }
        s4x1_case(pref, VAt::Locked, UAt::NextRound);
        s4x1_case(pref, VAt::StampRead, UAt::NextRound);
    }
}

fn s4x1_case(pref: Pref, v_at: VAt, u_at: UAt) {
    let case = format!("{pref:?} {v_at:?} {u_at:?}");
    let mut r = Rig::new();
    let p = r.hb_peer(1, false, pref);
    let t = r.tip();
    let b = r.build_block(1);
    let d = block_on(b.hash(), b.header().raw(), vec![coinbase(900, 50), spend(900, 0)], 900);
    r.sim.chain.add(&[*b.header().raw()]);
    r.sim.chain.store_blocks(&[Arc::clone(&d)]);
    let names: Names = vec![("T", t.hash), ("B", b.hash()), ("D", d.hash())];
    r.clear_pushes();
    let v: VThread = Arc::default();
    let u: VThread = Arc::default();
    let (chain, b2) = (Arc::clone(&r.sim.chain), Arc::clone(&b));
    let accept = move || rpc_submit(&chain, &b2);
    let (shared, core, chain) =
        (Arc::clone(&r.sim.shared), Arc::clone(&r.sim.core), Arc::clone(&r.sim.chain));
    let tip_d = move || {
        let tip = chain.snapshot().tip.tip;
        shared.updated_block_tip(&core, &chain.new_blocks(tip, Some(t.id)), false);
    };
    match v_at {
        VAt::BeforeHead => accept(),
        VAt::Locked | VAt::StampRead => {
            let (v, s, hash) = (Arc::clone(&v), Arc::clone(&r.sim.shared), b.hash());
            let point = if v_at == VAt::Locked {
                LockPoint::Locked
            } else {
                LockPoint::StampRead
            };
            at_next_lock(&r, point, move || {
                spawn_into(&v, accept);
                until(|| s.signals().recent().hash == Some(hash));
            });
        }
    }
    match u_at {
        UAt::BeforeHead => tip_d(),
        UAt::UnderBlockInv => {
            let u = Arc::clone(&u);
            let relaying = reached(&r, LockPoint::Relay(r.id(p)));
            at_next_drain(&r, move || {
                spawn_into(&u, tip_d);
                until(|| relaying.load(Ordering::SeqCst));
            });
        }
        UAt::AfterUnlock => {
            let u = Arc::clone(&u);
            at_next_lock(&r, LockPoint::Locked, move || spawn_into(&u, tip_d));
        }
        UAt::NextRound => {}
    }
    r.head(p);
    join(&v);
    join(&u);
    r.sim.chain.set_read_hook(None);
    r.sim.shared.signals().set_lock_hook(None);
    assert_eq!(r.tip().hash, d.hash(), "{case}");
    if u_at == UAt::NextRound {
        r.updated_block_tip(&t);
    }
    r.head(p);
    let second = if pref == Pref::Neither {
        "inv D"
    } else {
        "cmpctblock D"
    };
    assert_eq!(announcements(r.script(p, &names)), ["cmpctblock B", second], "{case}");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reorg {
    /// The head takes S and the cell, then V reorganizes to B and accepts C, whose visit waits
    /// for the head.
    Waits,
    /// C's visit takes the cell before the head, published before S.
    First,
    /// C's visit takes the cell before the head, published after S: `Ahead`.
    FirstAhead,
}

/// S4-x (2), a reorg crossing (Sol r3). Tip A, with A's append in the cell. The peer knows the
/// fork parent and branch B, not A. `cmpctblock(A)` then `cmpctblock(C)`, with
/// `best_header_sent` A then C; when C's visit takes the cell first, `cmpctblock(C)`, then
/// section 7's inv fallback for A per Core; never `cmpctblock(C)` then `cmpctblock(A)`, and
/// `best_header_sent` never moves back.
// Core: net_processing.cpp:2228-2268, 6413-6494, 6527-6545
#[test]
fn s4x2_reorg_crossing() {
    for pref in PREFS {
        for reorg in [Reorg::Waits, Reorg::First, Reorg::FirstAhead] {
            s4x2_case(pref, reorg);
        }
    }
}

fn s4x2_case(pref: Pref, reorg: Reorg) {
    let case = format!("{pref:?} {reorg:?}");
    let mut r = Rig::new();
    let f = r.tip();
    // A by RPC before P connects: the visit never saw P.
    let a = r.generate(0);
    let p = r.connect(1, false);
    r.prefer(p, pref);
    r.send(p, "sendcmpct", sendcmpct_msg(true));
    // Branch B on F from P: its header and data, stored behind A.
    let b = block_on(f.hash, &f.header, vec![coinbase(700, 50)], 700);
    r.send(p, "headers", headers_msg(&[&b]));
    r.send(p, "block", block_msg(&b));
    assert_eq!(r.tip().hash, a.hash());
    let b_id = r.block_id(&b);
    let peek = |r: &Rig| r.sim.shared.peek_headers(r.id(p)).unwrap();
    assert_eq!(peek(&r).best_known_block, Some(b_id), "{case}: P knows B");
    assert_eq!(peek(&r).best_header_sent, None, "{case}: and not A");
    // A's append in P's cell.
    r.sim.shared.updated_block_tip(&r.sim.core, &[a.hash()], false);
    let c = block_on(b.hash(), b.header().raw(), vec![coinbase(701, 50)], 701);
    let names: Names = vec![("A", a.hash()), ("B", b.hash()), ("C", c.hash())];
    r.clear_pushes();
    let (chain, c2) = (Arc::clone(&r.sim.chain), Arc::clone(&c));
    let reorg_and_accept = move || {
        chain.set_tip(b_id);
        rpc_submit(&chain, &c2);
    };
    let v: VThread = Arc::default();
    match reorg {
        Reorg::Waits => {
            let (v, s, hash) = (Arc::clone(&v), Arc::clone(&r.sim.shared), c.hash());
            at_next_lock(&r, LockPoint::StampRead, move || {
                spawn_into(&v, reorg_and_accept);
                until(|| s.signals().recent().hash == Some(hash));
            });
        }
        Reorg::First => reorg_and_accept(),
        Reorg::FirstAhead => {
            r.sim.chain.set_tip(b_id);
            hold_after_visit(&r, c.hash());
            let (v, pushes, chain, c2) =
                (Arc::clone(&v), Arc::clone(&r.pushes), Arc::clone(&r.sim.chain), Arc::clone(&c));
            at_next_drain(&r, move || {
                spawn_into(&v, move || rpc_submit(&chain, &c2));
                until(|| pushes.0.lock().iter().any(|(_, cmd, _)| cmd == "cmpctblock"));
            });
        }
    }
    let retries = r.sim.lp.logic().region_retries;
    r.head(p);
    join(&v);
    r.sim.chain.set_read_hook(None);
    r.sim.chain.set_announce_hook(None);
    r.sim.shared.signals().set_lock_hook(None);
    assert_eq!(r.tip().hash, c.hash(), "{case}");
    let want = match reorg {
        Reorg::Waits => ["cmpctblock A", "cmpctblock C"],
        Reorg::First | Reorg::FirstAhead => ["cmpctblock C", "inv A"],
    };
    assert_eq!(announcements(r.script(p, &names)), want, "{case}");
    assert_eq!(peek(&r).best_header_sent, Some(r.block_id(&c)), "{case}");
    let ahead = u64::from(reorg == Reorg::FirstAhead);
    assert_eq!(r.sim.lp.logic().region_retries, retries + ahead, "{case}");
}

/// S4-x (3), a P2P block: a peer's `block` is accepted and connected while the actor waits for
/// the reply, and `UpdatedBlockTip` runs on V's thread after it. Each HB peer holds
/// `cmpctblock(B)` before V writes B (S4-x (6)'s pause) and before that pass's section 7
/// announces it to anyone.
// Core: net_processing.cpp:2228-2268, 5348-5387, 6413-6494
#[test]
fn s4x3_p2p_block() {
    for pref in PREFS {
        let case = format!("{pref:?}");
        let mut r = Rig::new();
        let hb = [r.hb_peer(1, false, pref), r.hb_peer(2, true, pref)];
        let plain = r.connect(3, false);
        r.prefer(plain, pref);
        r.request_cb_announcements(plain);
        r.send(plain, "sendcmpct", sendcmpct_msg(false));
        let x = r.connect(4, true);
        r.sim.chain.spawn_v();
        let prev = r.tip();
        let block = r.build_block(2);
        let names: Names = vec![("B", block.hash())];
        let (chain, shared, core) =
            (Arc::clone(&r.sim.chain), Arc::clone(&r.sim.shared), Arc::clone(&r.sim.core));
        let (armed, notified) = (AtomicBool::new(true), Arc::new(AtomicBool::new(false)));
        let hook_notified = Arc::clone(&notified);
        r.sim.chain.set_reply_hook(Some(Arc::new(move || {
            if armed.swap(false, Ordering::SeqCst) {
                let tip = chain.snapshot().tip.tip;
                shared.updated_block_tip(&core, &chain.new_blocks(tip, Some(prev.id)), false);
                hook_notified.store(true, Ordering::SeqCst);
            }
        })));
        r.clear_pushes();
        r.send(x, "block", block_msg(&block));
        // The scheduler's `UpdatedBlockTip` may land after that pass: wait until it has
        // appended to every relay list (not merely started, D105), then sync.
        until(|| notified.load(Ordering::SeqCst));
        r.sync();
        assert_eq!(r.tip().hash, block.hash(), "{case}");
        let pushes = r.pushes(&names);
        let of_b = |to: &[usize]| {
            pushes.iter().filter(|(i, f)| f.ends_with(" B") && to.contains(i)).count()
        };
        assert_eq!(of_b(&hb), 2, "{case}: {pushes:?}");
        // Section 7 announces B to the plain peer, and back to x: a `block` message does not
        // update its source's best-known block (np:5348-5387).
        assert_eq!(of_b(&[plain, x]), 2, "{case}: {pushes:?}");
        let last_cmpct = pushes.iter().rposition(|(i, f)| hb.contains(i) && f == "cmpctblock B");
        let first_other = pushes.iter().position(|(i, f)| f.ends_with(" B") && !hb.contains(i));
        assert!(last_cmpct < first_other, "{case}: {pushes:?}");
        for i in hb {
            assert_eq!(announcements(r.script(i, &names)), ["cmpctblock B"], "{case}");
        }
        assert_eq!(announcements(r.script(plain, &names)), [pref.headers("B")], "{case}");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Late {
    /// `VERACK` and `sendcmpct(1)` handled before V's visit.
    Before,
    /// Both handled, and the visit waits on the peer's cell during a later region.
    WaitsAfterSendcmpct,
    /// `VERACK` handled, the visit waits on the cell during a region, `sendcmpct(1)` after.
    WaitsBeforeSendcmpct,
    /// `VERACK` handled after the visit passed the peer.
    After,
}

/// S4-x (4), a late full connection. A peer completes `VERACK` and a high-bandwidth `sendcmpct`
/// before V's visit reaches it, while the visit waits on its cell, or after the visit has
/// passed it, for both table orders: it gets `cmpctblock(B)` exactly when both were handled
/// before the visit read it, as with Core's `ForEachNode` (D50).
// Core: net.cpp:4664-4672 ForEachNode, net_processing.cpp:2249-2265
#[test]
fn s4x4_late_full_connection() {
    for pref in PREFS {
        for late_first in [true, false] {
            for late in
                [Late::Before, Late::WaitsAfterSendcmpct, Late::WaitsBeforeSendcmpct, Late::After]
            {
                s4x4_case(pref, late_first, late);
            }
        }
    }
}

fn s4x4_case(pref: Pref, late_first: bool, late: Late) {
    let case = format!("{pref:?} late_first={late_first} {late:?}");
    let mut r = Rig::new();
    let (l, h) = if late_first {
        let l = r.attach(1);
        (l, r.hb_peer(2, true, pref))
    } else {
        let h = r.hb_peer(2, true, pref);
        (r.attach(1), h)
    };
    let complete = |r: &mut Rig, sendcmpct: bool| {
        r.send(l, "verack", Vec::new());
        r.prefer(l, pref);
        let tip = r.tip().hash;
        r.send(l, "getheaders", getheaders_msg(tip));
        if sendcmpct {
            r.send(l, "sendcmpct", sendcmpct_msg(true));
        }
    };
    let block = r.build_block(1);
    let names: Names = vec![("B", block.hash())];
    let v: VThread = Arc::default();
    let in_region = |r: &mut Rig| {
        let (v2, chain, b2) = (Arc::clone(&v), Arc::clone(&r.sim.chain), Arc::clone(&block));
        let visiting = reached(r, LockPoint::Visit(r.id(l)));
        *r.hold.lock() = Some(Box::new(move || {
            spawn_into(&v2, move || rpc_submit(&chain, &b2));
            // The visit (past h when h is first in the table) waits on l's cell.
            until(|| visiting.load(Ordering::SeqCst));
        }));
        r.clear_pushes();
        r.send(l, "qsigrec", Vec::new());
        join(&v);
        r.sim.shared.signals().set_lock_hook(None);
    };
    match late {
        Late::Before => {
            complete(&mut r, true);
            r.clear_pushes();
            rpc_submit(&r.sim.chain, &block);
        }
        Late::WaitsAfterSendcmpct => {
            complete(&mut r, true);
            in_region(&mut r);
        }
        Late::WaitsBeforeSendcmpct => {
            complete(&mut r, false);
            in_region(&mut r);
            r.send(l, "sendcmpct", sendcmpct_msg(true));
        }
        Late::After => {
            r.clear_pushes();
            rpc_submit(&r.sim.chain, &block);
            complete(&mut r, true);
        }
    }
    assert_eq!(r.tip().hash, block.hash(), "{case}");
    assert_eq!(announcements(r.script(h, &names)), ["cmpctblock B"], "{case}");
    let gets = matches!(late, Late::Before | Late::WaitsAfterSendcmpct);
    let got = announcements(r.script(l, &names)).contains(&"cmpctblock B".to_owned());
    assert_eq!(got, gets, "{case}: {:?}", r.script(l, &names));
}

/// D57: a peer an actor region disconnects while V's visit waits on its cell. HB peers a and P
/// both qualify for `cmpctblock(B)`; the visit pushes to a, then waits on P's cell while P's
/// region marks P for disconnection. Re-tested under the cell, P is no longer fully connected
/// and gets nothing, as Core's `ForEachNode` skips a node with `fDisconnect` set.
// Core: net_processing.cpp:2252, net.cpp:4852-4855
#[test]
fn d57_a_peer_disconnected_while_the_visit_waits() {
    for pref in PREFS {
        let case = format!("{pref:?}");
        let mut r = Rig::new();
        let a = r.hb_peer(1, false, pref);
        let p = r.hb_peer(2, false, pref);
        let block = r.build_block(1);
        let names: Names = vec![("B", block.hash())];
        let v: VThread = Arc::default();
        let (v2, chain, b2, core) =
            (Arc::clone(&v), Arc::clone(&r.sim.chain), Arc::clone(&block), Arc::clone(&r.sim.core));
        let (pid, visiting) = (r.id(p), reached(&r, LockPoint::Visit(r.id(p))));
        *r.hold.lock() = Some(Box::new(move || {
            spawn_into(&v2, move || rpc_submit(&chain, &b2));
            until(|| visiting.load(Ordering::SeqCst));
            core.disconnect(pid, Why::Processor("test"));
        }));
        r.clear_pushes();
        r.send(p, "qsigrec", Vec::new());
        join(&v);
        r.sim.shared.signals().set_lock_hook(None);
        assert_eq!(r.tip().hash, block.hash(), "{case}");
        assert_eq!(announcements(r.script(a, &names)), ["cmpctblock B"], "{case}");
        assert!(
            r.script(p, &names).iter().all(|f| !f.starts_with("cmpctblock")),
            "{case}: {:?}",
            r.script(p, &names)
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drain {
    /// The actor is idle; its next pass drains.
    Idle,
    /// The actor is in another peer's region when D reaches V.
    Region,
    /// D arrives by P2P instead, and its C5′ wait is met by the actor's in-wait drain.
    BlockWait,
}

/// S4-x (8), `BlockChecked` before a later accept (Opus r10 M1). A stored B with source P, an
/// HB-requesting peer, is connected by another origin (here an RPC accept of its parent), and
/// `BlockChecked(B)` is queued; a later accept of D on B reaches V before the actor drains.
/// C5′ holds D until the actor has applied `BlockChecked(B)`: P gets `sendcmpct(1)`, and the
/// evicted HB peer `sendcmpct(0)`, before `cmpctblock(D)`; never after.
// Core: net_processing.cpp:1360-1416, 2228-2268, 2314-2343
#[test]
fn s4x8_block_checked_before_a_later_accept() {
    for pref in PREFS {
        for drain in [Drain::Idle, Drain::Region, Drain::BlockWait] {
            s4x8_case(pref, drain);
        }
    }
}

fn s4x8_case(pref: Pref, drain: Drain) {
    let case = format!("{pref:?} {drain:?}");
    let mut r = Rig::new();
    r.sim.chain.spawn_v();
    // Three chosen HB peers, h1 first in the list, each by relaying a block.
    let hs: Vec<usize> = (1..=3)
        .map(|n| {
            let h = r.hb_peer(n, false, pref);
            let k = r.build_block(0);
            r.send(h, "block", block_msg(&k));
            assert!(r.conns[h].peer.handle.bip152_highbandwidth_to(), "{case}");
            h
        })
        .collect();
    let p = r.hb_peer(4, false, pref);
    let z = r.connect(5, false);
    let x = r.connect(6, true);
    let a = r.build_block(0);
    let b = block_on(a.hash(), a.header().raw(), vec![coinbase(800, 50), spend(800, 0)], 800);
    let d = block_on(b.hash(), b.header().raw(), vec![coinbase(801, 50)], 801);
    // B from P, stored behind its parent's missing data.
    r.sim.chain.add(&[*a.header().raw()]);
    r.send(p, "block", block_msg(&b));
    let b_id = r.block_id(&b);
    assert!(r.sim.chain.entry(b_id).is_some() && r.tip().hash != b.hash(), "{case}");
    // Another origin connects B: `BlockChecked(B)` is queued with source P.
    rpc_submit(&r.sim.chain, &a);
    assert_eq!(r.tip().hash, b.hash(), "{case}");
    for &i in hs.iter().chain([&p]) {
        r.sim.shared.with_state_mut(r.id(i), |s| s.headers_mut().best_known_block = Some(b_id));
    }
    let names: Names = vec![("D", d.hash())];
    r.clear_pushes();
    let submit = BlockSubmit {
        hash: d.hash(),
        block: Arc::clone(&d),
        force: true,
        from: -1,
        merkle_checked: false,
    };
    match drain {
        Drain::Idle => {
            let mut w = r.sim.chain.process_block(submit).unwrap();
            assert!(ready_within(&mut w, 100).is_none(), "{case}: C5′ holds D");
            while ready_within(&mut w, 10).is_none() {
                r.sync();
            }
        }
        Drain::Region => {
            let (chain, d2) = (Arc::clone(&r.sim.chain), Arc::clone(&d));
            let v: VThread = Arc::default();
            let v2 = Arc::clone(&v);
            *r.hold.lock() = Some(Box::new(move || {
                let v_chain = Arc::clone(&chain);
                spawn_into(&v2, move || {
                    let mut w = v_chain.process_block(submit).unwrap();
                    assert!(ready_within(&mut w, PATIENCE.as_millis() as u64).is_some());
                });
                std::thread::sleep(Duration::from_millis(50));
                assert_ne!(chain.snapshot().tip.tip_hash, d2.hash(), "C5′ holds D");
            }));
            r.send(z, "qsigrec", Vec::new());
            while r.tip().hash != d.hash() {
                r.sync();
            }
            join(&v);
        }
        Drain::BlockWait => r.send(x, "block", block_msg(&d)),
    }
    r.sync();
    assert_eq!(r.tip().hash, d.hash(), "{case}");
    assert!(r.conns[p].peer.handle.bip152_highbandwidth_to(), "{case}");
    let only = |i: usize| -> Vec<String> {
        r.script(i, &names)
            .into_iter()
            .filter(|f| f.starts_with("sendcmpct") || f == "cmpctblock D")
            .collect()
    };
    assert_eq!(only(p), ["sendcmpct 1", "cmpctblock D"], "{case}");
    assert_eq!(only(hs[0]), ["sendcmpct 0", "cmpctblock D"], "{case}");
    for &h in &hs[1..] {
        assert_eq!(only(h), ["cmpctblock D"], "{case}");
    }
}

fn ready_within(w: &mut dashd_p2p::processing::seams::BlockWait, ms: u64) -> Option<()> {
    use std::task::{Context, Waker};
    let deadline = std::time::Instant::now() + Duration::from_millis(ms);
    loop {
        if w.0.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_ready() {
            return Some(());
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Visit {
    /// C's whole accept before P's `inv` region.
    BeforeRegion,
    /// P's region before C's accept.
    AfterRegion,
    /// C's stamp stored after the region's drain and before its lock: `Ahead`.
    Ahead,
}

/// S4-x (9), a read-only visit (Opus r10 M2). Tip B; HB peer P at best-known A. RPC accepts C,
/// whose visit only reads P's cell, and P's `inv(B, X)` region runs before or after the visit.
/// After it: the region runs on a snapshot showing C (a `getheaders` locator from C, no
/// `cmpctblock(C)`); before it: the region's locator from B, then `cmpctblock(C)`; never the
/// locator from B without `cmpctblock(C)`. Section 7 with the region holding P's cell when the
/// call begins is S4-x (10)'s after-read case.
// Core: net_processing.cpp:2249-2265, 4316-4370
#[test]
fn s4x9_a_read_only_visit() {
    for pref in PREFS {
        for visit in [Visit::BeforeRegion, Visit::AfterRegion, Visit::Ahead] {
            s4x9_case(pref, visit);
        }
    }
}

fn s4x9_case(pref: Pref, visit: Visit) {
    let case = format!("{pref:?} {visit:?}");
    let mut r = Rig::new();
    let a = r.generate(0);
    let b = r.generate(0);
    let p = r.hb_peer(1, false, pref);
    let a_id = r.block_id(&a);
    r.sim.shared.with_state_mut(r.id(p), |s| {
        s.last_getheaders_us = 0;
        let h = s.headers_mut();
        h.best_known_block = Some(a_id);
        h.best_header_sent = None;
    });
    let c = r.build_block(1);
    let x = BlockHash::from_lendian([7; 32]);
    let names: Names = vec![("A", a.hash()), ("B", b.hash()), ("C", c.hash()), ("X", x)];
    let inv = inv_msg(&[b.hash(), x]);
    r.clear_pushes();
    let retries = r.sim.lp.logic().region_retries;
    let v: VThread = Arc::default();
    match visit {
        Visit::BeforeRegion => {
            rpc_submit(&r.sim.chain, &c);
            r.send(p, "inv", inv);
        }
        Visit::AfterRegion => {
            r.send(p, "inv", inv);
            rpc_submit(&r.sim.chain, &c);
        }
        Visit::Ahead => {
            hold_after_visit(&r, c.hash());
            let (v2, chain, shared, c2) = (
                Arc::clone(&v),
                Arc::clone(&r.sim.chain),
                Arc::clone(&r.sim.shared),
                Arc::clone(&c),
            );
            at_next_drain(&r, move || {
                let hash = c2.hash();
                spawn_into(&v2, move || rpc_submit(&chain, &c2));
                until(|| shared.signals().recent().hash == Some(hash));
            });
            r.send(p, "inv", inv);
            join(&v);
            r.sim.chain.set_announce_hook(None);
        }
    }
    assert_eq!(r.tip().hash, c.hash(), "{case}");
    let gh = r.getheaders_cmd(p);
    let got: Vec<String> = r
        .script(p, &names)
        .into_iter()
        .filter(|f| f.starts_with(&gh) || f.starts_with("cmpctblock"))
        .collect();
    let want = match visit {
        Visit::BeforeRegion | Visit::Ahead => vec![format!("{gh} C")],
        Visit::AfterRegion => vec![format!("{gh} B"), "cmpctblock C".to_owned()],
    };
    assert_eq!(got, want, "{case}");
    let ahead = u64::from(visit == Visit::Ahead);
    assert_eq!(r.sim.lp.logic().region_retries, retries + ahead, "{case}");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Race {
    /// B's call begins between Q's message-start drain and V serving Q's `ProcessHeaders`.
    BeforeServe,
    /// After the reply, published before the redrain's snapshot.
    BeforeRedrain,
    /// After the reply, published after the redrain's snapshot: `Ahead`.
    AfterRedrain,
    /// After the continuation's lock (and its stamp read).
    AfterLock,
}

/// S4-x (7), headers racing an RPC block (M2, F2). Outbound HB peer Q at best-known A sends
/// `headers(B)` while RPC `submitblock B` is accepted and connected, B's `NewPoWValidBlock`
/// beginning before V serves Q's `ProcessHeaders`, after the reply with B published before or
/// after the redrain's snapshot, or after the continuation's lock. Before the lock: Q gets
/// `cmpctblock(B)` and no announcement time (`Ahead` first when published after the redrain);
/// after it: the announcement time is set and B's visit skips Q; never neither. The variants
/// with B accepted but not yet connected when the continuation runs, and the `cmpctblock(B)`
/// variant, need V's release points: S4-v (D48, D102).
// Core: net_processing.cpp:2249-2265, 3080-3110, 3197-3214
#[test]
fn s4x7_headers_racing_an_rpc_block() {
    for pref in PREFS {
        for race in [Race::BeforeServe, Race::BeforeRedrain, Race::AfterRedrain, Race::AfterLock] {
            s4x7_case(pref, race);
        }
    }
}

fn s4x7_case(pref: Pref, race: Race) {
    let case = format!("{pref:?} {race:?}");
    let mut r = Rig::new();
    // V serves `ProcessHeaders` on its own thread; the drain hooks fire on the actor's only.
    r.sim.chain.spawn_v();
    let actor = std::thread::current().id();
    let a = r.generate(0);
    let q = r.hb_peer(1, true, pref);
    let a_id = r.block_id(&a);
    r.sim.shared.with_state_mut(r.id(q), |s| s.headers_mut().best_known_block = Some(a_id));
    let announced_at =
        |r: &Rig| r.sim.shared.with_state(r.id(q), |s| s.last_block_announcement).unwrap();
    let before = announced_at(&r);
    let b = r.build_block(1);
    let names: Names = vec![("B", b.hash())];
    r.clear_pushes();
    let retries = r.sim.lp.logic().region_retries;
    let v: VThread = Arc::default();
    let q_id = r.id(q);
    let (chain, shared, b2, v2, rig_chain) = (
        Arc::clone(&r.sim.chain),
        Arc::clone(&r.sim.shared),
        Arc::clone(&b),
        Arc::clone(&v),
        Arc::clone(&r.sim.chain),
    );
    if race == Race::AfterRedrain {
        hold_after_visit(&r, b.hash());
    }
    let armed = AtomicBool::new(true);
    r.sim.chain.set_headers_hook(Some(Arc::new(move |from| {
        if from != q_id || !armed.swap(false, Ordering::SeqCst) {
            return;
        }
        let (chain, b2, v2, hash, s2) =
            (Arc::clone(&chain), Arc::clone(&b2), Arc::clone(&v2), b2.hash(), Arc::clone(&shared));
        let accept = move || rpc_submit(&chain, &b2);
        let stored = move || until(|| s2.signals().recent().hash == Some(hash));
        match race {
            Race::BeforeServe => accept(),
            Race::BeforeRedrain => {
                let hook = parking_lot::Mutex::new(Some(accept));
                rig_chain.set_read_hook(Some(Arc::new(move |read| {
                    if read == SimRead::DrainSnapshot
                        && std::thread::current().id() == actor
                        && let Some(f) = hook.lock().take()
                    {
                        std::thread::spawn(f).join().unwrap();
                    }
                })));
            }
            Race::AfterRedrain => {
                let hook = parking_lot::Mutex::new(Some((accept, stored)));
                rig_chain.set_read_hook(Some(Arc::new(move |read| {
                    if read == SimRead::DrainSnapshot
                        && std::thread::current().id() == actor
                        && let Some((f, wait)) = hook.lock().take()
                    {
                        spawn_into(&v2, f);
                        wait();
                    }
                })));
            }
            Race::AfterLock => {
                let hook = parking_lot::Mutex::new(Some((accept, stored)));
                shared.signals().set_lock_hook(Some(Arc::new(move |at| {
                    if at == LockPoint::StampRead
                        && let Some((f, wait)) = hook.lock().take()
                    {
                        spawn_into(&v2, f);
                        wait();
                    }
                })));
            }
        }
    })));
    r.send(q, "headers", headers_msg(&[&b]));
    join(&v);
    r.sim.chain.set_headers_hook(None);
    r.sim.chain.set_read_hook(None);
    r.sim.chain.set_announce_hook(None);
    r.sim.shared.signals().set_lock_hook(None);
    assert_eq!(r.tip().hash, b.hash(), "{case}");
    let cmpct = r.script(q, &names).contains(&"cmpctblock B".to_owned());
    let announced = announced_at(&r) != before;
    if race == Race::AfterLock {
        assert!(announced && !cmpct, "{case}: {:?}", r.script(q, &names));
    } else {
        assert!(cmpct && !announced, "{case}: {:?}", r.script(q, &names));
    }
    // After the lock, a later region of Q's may also meet B's stamp before V publishes.
    let retried = r.sim.lp.logic().region_retries - retries;
    match race {
        Race::BeforeServe | Race::BeforeRedrain => assert_eq!(retried, 0, "{case}"),
        Race::AfterRedrain => assert!(retried >= 1, "{case}: Ahead"),
        Race::AfterLock => {}
    }
}

/// S4-x (5), a relay racing a full connection (Sol r4). V has published A. `UpdatedBlockTip(A)`
/// runs on its own thread, one peer visit at a time, while peer P, which knows A's parent but
/// not A, completes `VERACK`, and HB peer Q's head takes S showing A; V then publishes B, and
/// U(B) appends before or after Q's next head. Each visit reads its peer as it reaches it (D44):
/// Q gets A at S exactly when U(A) appended to Q's cell before Q's head, else with B in its next
/// round; P gets A exactly when U(A) visited P after its `VERACK`, and otherwise `inv(B)` alone.
/// Both table orders.
// Core: net_processing.cpp:2185-2212, 6413-6545
#[test]
fn s4x5_a_relay_racing_a_full_connection() {
    for pref in PREFS {
        for p_first in [true, false] {
            for verack_before_visit in [true, false] {
                for head_before_visit in [true, false] {
                    for ub_before_next_head in [true, false] {
                        s4x5_case(
                            pref,
                            p_first,
                            verack_before_visit,
                            head_before_visit,
                            ub_before_next_head,
                        );
                    }
                }
            }
        }
    }
}

fn s4x5_case(pref: Pref, p_first: bool, verack_before: bool, head_before: bool, ub_first: bool) {
    let case = format!(
        "{pref:?} p_first={p_first} verack_before_visit={verack_before} \
         head_before_visit={head_before} ub_before_next_head={ub_first}"
    );
    let mut r = Rig::new();
    let f = r.tip();
    // Q is low-bandwidth while A is accepted, so A's visit skips it; then it asks for HB.
    let q_conn = |r: &mut Rig| {
        let q = r.connect(2, true);
        r.prefer(q, pref);
        r.send(q, "getheaders", getheaders_msg(f.hash));
        q
    };
    let (p, q) = if p_first {
        let p = r.attach(1);
        (p, q_conn(&mut r))
    } else {
        let q = q_conn(&mut r);
        (r.attach(1), q)
    };
    let a = r.build_block(0);
    rpc_submit(&r.sim.chain, &a);
    r.send(q, "sendcmpct", sendcmpct_msg(true));
    let b = r.build_block(0);
    let names: Names = vec![("F", f.hash), ("A", a.hash()), ("B", b.hash())];
    r.clear_pushes();
    let p_handle = r.conns[p].peer.handle.clone();
    // The table order is the connection order; slot k runs just before visit k (slot 0 before
    // U starts, the last slot after it ends).
    let (vp, vq) = if p_first {
        (0, 1)
    } else {
        (1, 0)
    };
    let verack_slot = if verack_before {
        vp
    } else {
        vp + 1
    };
    let head_slot = if head_before {
        vq
    } else {
        vq + 1
    };
    let run_slot = |r: &mut Rig, k: usize| {
        if k == verack_slot {
            r.sim.deliver(&r.conns[p].peer, &[("verack", Vec::new())]);
            r.sim.lp.logic_mut().process_messages(&p_handle);
            assert!(p_handle.successfully_connected(), "{case}");
            // Its preference, which Core ignores before `VERACK`.
            if let Some(cmd) = pref.command() {
                r.sim.deliver(&r.conns[p].peer, &[(cmd, Vec::new())]);
                r.sim.lp.logic_mut().process_messages(&p_handle);
            }
            r.sim
                .shared
                .with_state_mut(p_handle.id(), |s| s.headers_mut().best_known_block = Some(f.id));
        }
        if k == head_slot {
            r.head(q);
        }
    };
    let (visited_tx, visited) = std::sync::mpsc::channel();
    let (go, go_rx) = std::sync::mpsc::channel::<()>();
    let go_rx = parking_lot::Mutex::new(go_rx);
    r.sim.shared.signals().set_relay_hook(Some(Arc::new(move |id| {
        visited_tx.send(id).unwrap();
        go_rx.lock().recv().unwrap();
    })));
    run_slot(&mut r, 0);
    let (shared, core, hash) = (Arc::clone(&r.sim.shared), Arc::clone(&r.sim.core), a.hash());
    let u = std::thread::spawn(move || shared.updated_block_tip(&core, &[hash], false));
    for k in 1..=2 {
        visited.recv().unwrap();
        run_slot(&mut r, k);
        go.send(()).unwrap();
    }
    u.join().unwrap();
    r.sim.shared.signals().set_relay_hook(None);
    // P's first head after U(A).
    r.head(p);
    rpc_submit(&r.sim.chain, &b);
    let tip_b = |r: &Rig| r.sim.shared.updated_block_tip(&r.sim.core, &[b.hash()], false);
    if ub_first {
        tip_b(&r);
        r.head(q);
    } else {
        r.head(q);
        tip_b(&r);
        r.head(q);
    }
    r.head(p);
    let q_got_a_at_s = !head_before;
    let want_q: Vec<String> = if q_got_a_at_s || !ub_first {
        vec!["cmpctblock A".into(), "cmpctblock B".into()]
    } else {
        vec![pref.headers("A,B")]
    };
    assert_eq!(announcements(r.script(q, &names)), want_q, "{case}");
    let want_p: Vec<String> = if verack_before {
        vec![pref.headers("A"), pref.headers("B")]
    } else {
        vec!["inv B".into()]
    };
    assert_eq!(announcements(r.script(p, &names)), want_p, "{case}");
}
