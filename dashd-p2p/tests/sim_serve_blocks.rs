//! P4b in `sim`: Core's block serving (`ProcessGetBlockData`, net_processing.cpp:2736-2900;
//! `GETBLOCKS` :4636-4702; `GETBLOCKTXN` :4704-4757; `FILTERLOAD`/`FILTERADD`/`FILTERCLEAR`
//! :5529-5607) to inbound peers, one test per behaviour P4b's code decides (P4b D13): the
//! serving halves of `p2p_getdata.py`, `p2p_compactblocks.py`, `p2p_filter.py` and
//! `p2p_node_network_limited.py`, the prune race of O17 P2P-17, and the limits P4b's mutants
//! target. The live comparison with dashd is `dashd_serve_transcripts.rs`.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::thread::{self, ThreadId};
use std::time::Duration;

use dash_consensus::{Block, CoreSerialize};
use dash_num::Hash256;
use dash_p2p::conn::Permissions;
use dash_p2p::{CommandString, ConnectionType, NetAddr, Service, ServiceFlags};
use dash_p2p_core::{
    FilterAdd, FilterLoad, GetBlockTxn, GetBlocks, GetData, Inv, InvType, Inventory,
    ProtocolVersion,
};
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::BlockHash;
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity, local_services};
use dashd_p2p::processing::bloom::{BLOOM_UPDATE_ALL, BloomFilter};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::merkle::merkle_block;
use dashd_p2p::processing::seams::{BlockRead, ChainAccess, EntryFacts};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimChain, SimPeer, SimRead};
use dashd_p2p::{Registry, Seams};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, span};

const NETWORK: u64 = 1;

/// A node with `opts` whose active chain is genesis plus `n` stored blocks, out of IBD, its
/// clock just after the tip. Returns the blocks and their entries, genesis's entry first.
fn node_with(opts: &IdentityOptions, n: usize) -> (Sim, Vec<Arc<Block>>, Vec<EntryFacts>) {
    let p =
        Arc::new(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&p, opts).unwrap();
    let services = local_services(opts);
    let sim =
        Sim::with(p, id, ProcessorOptions::default(), Registry::new(), Seams::default(), |cfg| {
            cfg.local_services = services;
        });
    let blocks = SimChain::build_blocks(&sim.chain.genesis(), n, 1, 0);
    let tip = sim.chain.store_blocks(&blocks);
    sim.chain.set_tip(tip.id);
    sim.chain.set_ibd(false);
    sim.clock.set_mock_time(i64::from(tip.time) + 1);
    let mut entries = vec![sim.chain.genesis()];
    entries.extend(blocks.iter().map(|b| sim.chain.by_hash(&b.hash()).unwrap()));
    (sim, blocks, entries)
}

fn node(n: usize) -> (Sim, Vec<Arc<Block>>, Vec<EntryFacts>) {
    node_with(&IdentityOptions::new(), n)
}

/// Inbound peer `n` with `perms`, after the handshake and a pass, relaying transactions.
fn inbound(sim: &mut Sim, n: u8, perms: Permissions) -> SimPeer {
    let p = sim.accept(n, perms).expect("an inbound slot");
    sim.handshake(
        &p,
        PeerVersionMsg {
            services: NETWORK,
            ..PeerVersionMsg::v24(SIM_START)
        },
    );
    sim.pass();
    sim.sent(&p);
    p
}

fn hash256(h: &BlockHash) -> Hash256 {
    Hash256::from_lendian(*h.as_bytes())
}

fn inv(inv_type: InvType, h: &BlockHash) -> Inventory {
    Inventory {
        inv_type,
        hash: hash256(h),
    }
}

fn enc(m: &impl BaseCodec) -> Vec<u8> {
    let mut buf = Vec::new();
    m.encode(&mut buf);
    buf
}

fn getdata(items: &[Inventory]) -> Vec<u8> {
    enc(&GetData {
        inventory: items.to_vec(),
    })
}

/// Sends the messages and returns everything the node sent back after a pass.
/// Core serves one block item per `ProcessMessages` call, so this passes a few times.
fn exchange(sim: &mut Sim, p: &SimPeer, msgs: &[(&str, Vec<u8>)]) -> Vec<(CommandString, Vec<u8>)> {
    sim.deliver(p, msgs);
    for _ in 0..6 {
        sim.pass();
    }
    sim.sent(p)
}

fn score(sim: &Sim, p: &SimPeer) -> i32 {
    sim.shared.misbehavior(p.handle.id()).unwrap().0
}

fn block_msg(b: &Block) -> (CommandString, Vec<u8>) {
    (CommandString::BLOCK, b.core_serialize())
}

fn cmds(sent: &[(CommandString, Vec<u8>)]) -> Vec<&str> {
    sent.iter().map(|(c, _)| c.as_str()).collect()
}

/// `p2p_getdata.py` and the order of `ProcessGetData`: a stored block is sent as Core serializes
/// it; an unknown hash and a header without data are ignored without `notfound` (np:2777, :2806,
/// :3101-3110); one block item is served per call, all of them in order.
#[test]
fn getdata_block_serves_stored_blocks_only() {
    let (mut sim, blocks, c) = node(3);
    let p = inbound(&mut sim, 1, Permissions::NONE);
    let header_only = sim.chain.add(&SimChain::build(&c[3], 1, 1, 9));
    let unknown = BlockHash::from_lendian([7; 32]);
    let sent = exchange(
        &mut sim,
        &p,
        &[(
            "getdata",
            getdata(&[
                inv(InvType::Block, &c[2].hash),
                inv(InvType::Block, &unknown),
                inv(InvType::Block, &header_only.hash),
                inv(InvType::Block, &c[1].hash),
            ]),
        )],
    );
    assert_eq!(sent, vec![block_msg(&blocks[1]), block_msg(&blocks[0])]);
    assert!(!p.handle.disconnect_requested());
}

/// `BlockRequestAllowed` (np:2048-2055): a block off the active chain that never connected is
/// not served, whatever its age.
#[test]
fn getdata_ignores_a_side_block_that_never_connected() {
    let (mut sim, _, c) = node(3);
    let p = inbound(&mut sim, 1, Permissions::NONE);
    let side = SimChain::build_blocks(&c[1], 1, 7, 5);
    sim.chain.store_blocks(&side);
    let sent =
        exchange(&mut sim, &p, &[("getdata", getdata(&[inv(InvType::Block, &side[0].hash())]))]);
    assert!(sent.is_empty(), "{:?}", cmds(&sent));
}

/// `p2p_compactblocks.py`'s serving half (np:2862-2878): `cmpctblock` within
/// `MAX_CMPCTBLOCK_DEPTH` of a recent tip, else `block`; a stale tip (not `CanDirectFetch`)
/// gets `block` at any depth.
#[test]
fn getdata_cmpct_block_depth_and_direct_fetch() {
    let (mut sim, blocks, c) = node(8);
    let p = inbound(&mut sim, 1, Permissions::NONE);
    let ask = |sim: &mut Sim, h: &BlockHash| {
        exchange(sim, &p, &[("getdata", getdata(&[inv(InvType::CompactBlock, h)]))])
    };
    // Five deep (tip 8, height 3) is a compact block; six deep is the full block.
    let five = ask(&mut sim, &c[3].hash);
    assert_eq!(cmds(&five), ["cmpctblock"]);
    // The header and the coinbase, prefilled, as `CBlockHeaderAndShortTxIDs` lays them out.
    assert_eq!(&five[0].1[..80], &blocks[2].core_serialize()[..80]);
    assert_eq!(ask(&mut sim, &c[2].hash), vec![block_msg(&blocks[1])]);
    // Twenty block intervals later the tip is stale.
    let spacing = sim.chain.params().consensus.pow_target_spacing;
    sim.clock.set_mock_time(i64::from(c[8].time) + 20 * spacing);
    assert_eq!(ask(&mut sim, &c[8].hash), vec![block_msg(&blocks[7])]);
}

fn getblocks(locator: &[BlockHash], stop: BlockHash) -> Vec<u8> {
    enc(&GetBlocks {
        version: ProtocolVersion(70242),
        locator_hashes: locator.to_vec(),
        hash_stop: stop,
    })
}

/// The block hashes of the `inv`s in `sent`, in order.
fn inv_blocks(sent: &[(CommandString, Vec<u8>)]) -> Vec<Hash256> {
    sent.iter()
        .filter(|(c, _)| *c == CommandString::INV)
        .flat_map(|(_, b)| Inv::decode(&mut &b[..]).unwrap().inventory)
        .inspect(|i| assert_eq!(i.inv_type, InvType::Block))
        .map(|i| i.hash)
        .collect()
}

/// `getblocks` (np:4636-4702): 500 invs after the fork; fetching the 500th block sends the
/// continuation `inv` of the tip right after it, once; `hashStop` ends the batch before it.
#[test]
fn getblocks_batches_500_with_the_continuation() {
    let (mut sim, blocks, c) = node(600);
    let p = inbound(&mut sim, 1, Permissions::NONE);
    let sent = exchange(&mut sim, &p, &[("getblocks", getblocks(&[c[0].hash], BlockHash::ZERO))]);
    let want: Vec<_> = c[1..=500].iter().map(|e| hash256(&e.hash)).collect();
    assert_eq!(inv_blocks(&sent), want);

    let fetch = getdata(&[inv(InvType::Block, &c[500].hash)]);
    let sent = exchange(&mut sim, &p, &[("getdata", fetch.clone())]);
    assert_eq!(sent[0], block_msg(&blocks[499]));
    assert_eq!(inv_blocks(&sent[1..]), [hash256(&c[600].hash)]);
    // Cleared: a second fetch is only the block.
    assert_eq!(exchange(&mut sim, &p, &[("getdata", fetch)]), vec![block_msg(&blocks[499])]);

    // From 550, stopping at 560: 551..=559.
    let sent = exchange(&mut sim, &p, &[("getblocks", getblocks(&[c[550].hash], c[560].hash))]);
    let want: Vec<_> = c[551..560].iter().map(|e| hash256(&e.hash)).collect();
    assert_eq!(inv_blocks(&sent), want);
    // From the tip: nothing.
    let sent = exchange(&mut sim, &p, &[("getblocks", getblocks(&[c[600].hash], BlockHash::ZERO))]);
    assert!(inv_blocks(&sent).is_empty());
}

/// `MAX_LOCATOR_SZ` (np:4641-4645): 101 entries are served, 102 disconnect unscored.
#[test]
fn getblocks_locator_limit() {
    let (mut sim, _, c) = node(2);
    let ok = inbound(&mut sim, 1, Permissions::NONE);
    let bad = inbound(&mut sim, 2, Permissions::NONE);
    let sent =
        exchange(&mut sim, &ok, &[("getblocks", getblocks(&[c[0].hash; 101], BlockHash::ZERO))]);
    assert_eq!(inv_blocks(&sent).len(), 2);
    assert!(!ok.handle.disconnect_requested());
    exchange(&mut sim, &bad, &[("getblocks", getblocks(&[c[0].hash; 102], BlockHash::ZERO))]);
    assert!(bad.handle.disconnect_requested());
    assert_eq!(score(&sim, &bad), 0);
}

/// In prune mode `getblocks` stops at a block without data, or older than
/// `MIN_BLOCKS_TO_KEEP - 3600 / spacing` below the tip (np:4680-4686).
#[test]
fn getblocks_stops_at_pruned_or_old_blocks_in_prune_mode() {
    let (mut sim, _, c) = node(300);
    sim.chain.set_prune_mode(true);
    let p = inbound(&mut sim, 1, Permissions::NONE);
    // 288 - 3600/150 = 264: heights ≤ 300 - 264 = 36 are too old.
    let sent = exchange(&mut sim, &p, &[("getblocks", getblocks(&[c[0].hash], BlockHash::ZERO))]);
    assert!(inv_blocks(&sent).is_empty());
    let sent = exchange(&mut sim, &p, &[("getblocks", getblocks(&[c[36].hash], BlockHash::ZERO))]);
    let want: Vec<_> = c[37..=300].iter().map(|e| hash256(&e.hash)).collect();
    assert_eq!(inv_blocks(&sent), want);
    sim.chain.prune(c[40].id);
    let sent = exchange(&mut sim, &p, &[("getblocks", getblocks(&[c[36].hash], BlockHash::ZERO))]);
    let want: Vec<_> = c[37..40].iter().map(|e| hash256(&e.hash)).collect();
    assert_eq!(inv_blocks(&sent), want);
}

fn getblocktxn(h: &BlockHash, indexes: Vec<u16>) -> Vec<u8> {
    enc(&GetBlockTxn::new(*h, indexes).unwrap())
}

/// `getblocktxn` (np:4704-4757, SendBlockTransactions np:3113-3124): `blocktxn` within
/// `MAX_BLOCKTXN_DEPTH`, the full block (through the getdata queue) deeper, nothing for an
/// unknown block, +100 for an index past the end.
#[test]
fn getblocktxn_depth_bounds_and_unknown() {
    let (mut sim, blocks, c) = node(12);
    let p = inbound(&mut sim, 1, Permissions::NONE);
    let ten = exchange(&mut sim, &p, &[("getblocktxn", getblocktxn(&c[2].hash, vec![0]))]);
    let mut want = c[2].hash.as_bytes().to_vec();
    want.push(1);
    want.extend(blocks[1].txs()[0].core_serialize());
    assert_eq!(ten, vec![(CommandString::BLOCKTXN, want)]);
    let eleven = exchange(&mut sim, &p, &[("getblocktxn", getblocktxn(&c[1].hash, vec![0]))]);
    assert_eq!(eleven, vec![block_msg(&blocks[0])]);
    let unknown = BlockHash::from_lendian([7; 32]);
    assert!(exchange(&mut sim, &p, &[("getblocktxn", getblocktxn(&unknown, vec![0]))]).is_empty());
    assert_eq!(score(&sim, &p), 0);
    let oob = exchange(&mut sim, &p, &[("getblocktxn", getblocktxn(&c[12].hash, vec![0, 1]))]);
    assert!(oob.is_empty());
    assert_eq!(score(&sim, &p), 100);
}

fn filterload(f: &BloomFilter) -> Vec<u8> {
    enc(&f.to_msg())
}

/// `p2p_filter.py` (np:5529-5607): the size limits score +100 (36,000 bytes and 50 hash
/// functions pass, one more fails), `filteradd` without a filter scores +100, 520 bytes pass and
/// 521 fail the read. The two reads over their `LIMITED_VECTOR` bound log Core's `e.what()`
/// (np:5540, :5573; serialize.h:1030).
#[test]
fn filter_limits_score() {
    read_logs();
    let (mut sim, _, _) = node(1);
    let load = |data: usize, funcs: u32| {
        enc(&FilterLoad {
            data: vec![0; data],
            hash_funcs: funcs,
            tweak: 0,
            flags: 0,
        })
    };
    let add = |data: usize| {
        enc(&FilterAdd {
            data: vec![1; data],
        })
    };
    // (command, payload, a filter loaded first, the score, Core's reason with `%d` for the peer).
    let cases: [(&str, Vec<u8>, bool, i32, &str); 6] = [
        ("filterload", load(36_000, 50), false, 0, ""),
        (
            "filterload",
            load(36_001, 50),
            false,
            100,
            "misformatted bloom filter. peer=%d error=Vector length limit exceeded: iostream error",
        ),
        ("filterload", load(36_000, 51), false, 100, "too-large bloom filter"),
        ("filteradd", add(520), true, 0, ""),
        (
            "filteradd",
            add(521),
            true,
            100,
            "bad filteradd message. peer=%d error=Vector length limit exceeded: iostream error",
        ),
        ("filteradd", add(8), false, 100, "bad filteradd message"),
    ];
    for (i, (cmd, payload, preload, want, why)) in cases.into_iter().enumerate() {
        let p = inbound(&mut sim, i as u8 + 1, Permissions::NONE);
        if preload {
            exchange(&mut sim, &p, &[("filterload", load(10, 1))]);
        }
        let before = logs().len();
        exchange(&mut sim, &p, &[(cmd, payload)]);
        assert_eq!(score(&sim, &p), want, "case {i}: {cmd}");
        let lines = logs().split_off(before);
        if want == 0 {
            assert!(lines.is_empty(), "case {i}: {lines:?}");
            continue;
        }
        let id = p.handle.id();
        let reason = why.replace("%d", &id.to_string());
        let line =
            format!("Misbehaving: peer={id} (0 -> 100) DISCOURAGE THRESHOLD EXCEEDED: {reason}");
        assert_eq!(lines, [(Level::DEBUG, "net".to_owned(), line)], "case {i}: {cmd}");
    }
}

/// `MSG_FILTERED_BLOCK` (np:2834-2859): nothing without a filter; with one, `merkleblock` then
/// each matched transaction; after `filterclear`, nothing again.
#[test]
fn filtered_block_follows_the_filter() {
    let (mut sim, blocks, c) = node(2);
    let p = inbound(&mut sim, 1, Permissions::NONE);
    let ask = getdata(&[inv(InvType::FilteredBlock, &c[2].hash)]);
    assert!(exchange(&mut sim, &p, &[("getdata", ask.clone())]).is_empty());

    let mut filter = BloomFilter::new(10, 0.000_001, 0, BLOOM_UPDATE_ALL);
    filter.insert(blocks[1].txs()[0].txid().as_bytes());
    let mut expected_filter = filter.clone();
    let (mb, matched) = merkle_block(&blocks[1], &mut expected_filter);
    assert_eq!(matched.len(), 1);
    let sent =
        exchange(&mut sim, &p, &[("filterload", filterload(&filter)), ("getdata", ask.clone())]);
    assert_eq!(
        sent,
        vec![
            (CommandString::MERKLEBLOCK, enc(&mb)),
            (CommandString::TX, blocks[1].txs()[0].core_serialize()),
        ]
    );
    assert!(exchange(&mut sim, &p, &[("filterclear", Vec::new()), ("getdata", ask)]).is_empty());
    assert_eq!(score(&sim, &p), 0);
}

/// `-peerbloomfilters=0` (np:5530-5534): every filter command disconnects, unscored. The line is
/// Core's `NET_NETCONN` (np:5531), written under `-debug=net` or `-debug=netconn`.
#[test]
fn filters_without_node_bloom_disconnect() {
    read_logs();
    let opts = IdentityOptions {
        peer_bloom_filters: false,
        ..IdentityOptions::new()
    };
    let mut want = Vec::new();
    for (i, cmd) in ["filterload", "filteradd", "filterclear"].into_iter().enumerate() {
        let (mut sim, _, _) = node_with(&opts, 1);
        let p = inbound(&mut sim, i as u8 + 1, Permissions::NONE);
        exchange(&mut sim, &p, &[(cmd, Vec::new())]);
        assert!(p.handle.disconnect_requested(), "{cmd}");
        assert_eq!(score(&sim, &p), 0, "{cmd}");
        let line = format!("{cmd}{NO_BLOOM}{}; disconnecting", p.handle.id());
        want.push((Level::DEBUG, "net|netconn".to_owned(), line));
    }
    assert_eq!(logs(), want);
}

/// `p2p_node_network_limited.py`'s serving half (np:2795-2802): a pruned node serves blocks
/// up to 288 + 2 below its tip and disconnects for deeper ones, unless the peer has `noban`.
#[test]
fn node_network_limited_bound() {
    let opts = IdentityOptions {
        pruned: true,
        ..IdentityOptions::new()
    };
    let (mut sim, blocks, c) = node_with(&opts, 300);
    let p = inbound(&mut sim, 1, Permissions::NONE);
    // 300 - 10 = 290: served.
    let sent = exchange(&mut sim, &p, &[("getdata", getdata(&[inv(InvType::Block, &c[10].hash)]))]);
    assert_eq!(sent, vec![block_msg(&blocks[9])]);
    let noban = inbound(&mut sim, 2, Permissions::NO_BAN);
    let deep = getdata(&[inv(InvType::Block, &c[9].hash)]);
    assert_eq!(
        exchange(&mut sim, &noban, &[("getdata", deep.clone())]),
        vec![block_msg(&blocks[8])]
    );
    // 291 deep: disconnected, nothing sent.
    assert!(exchange(&mut sim, &p, &[("getdata", deep)]).is_empty());
    assert!(p.handle.disconnect_requested());
    assert_eq!(score(&sim, &p), 0);
}

/// An unpruned node has `NODE_NETWORK`, so the bound never applies (behaviour 2).
#[test]
fn unpruned_node_serves_any_depth() {
    let (mut sim, blocks, c) = node(300);
    let p = inbound(&mut sim, 1, Permissions::NONE);
    let sent = exchange(&mut sim, &p, &[("getdata", getdata(&[inv(InvType::Block, &c[1].hash)]))]);
    assert_eq!(sent, vec![block_msg(&blocks[0])]);
}

/// The log lines P4b's tests read, as (thread, level, target, message), captured process-wide:
/// the two P2P-17 lines, `Misbehaving:` and the `NODE_BLOOM` disconnect. A test's processor runs
/// on its own thread (`Sim::pass`), so [`logs`] gives each test its own lines.
static READ_LOGS: Mutex<Vec<(ThreadId, Level, String, String)>> = Mutex::new(Vec::new());

/// The lines that start one of these, or hold [`NO_BLOOM`].
const READ_LINES: [&str; 3] = ["Block was pruned", "Cannot load block", "Misbehaving: "];
const NO_BLOOM: &str = " received despite not offering bloom services from peer=";

/// A subscriber that keeps [`READ_LOGS`]'s lines and drops everything else.
struct ReadLogs;

impl tracing::Subscriber for ReadLogs {
    fn enabled(&self, m: &Metadata<'_>) -> bool {
        ["net", "net|netconn"].contains(&m.target()) || m.target().ends_with("serve_blocks")
    }

    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn event(&self, e: &Event<'_>) {
        struct Message(String);
        impl Visit for Message {
            fn record_debug(&mut self, f: &Field, v: &dyn fmt::Debug) {
                if f.name() == "message" {
                    self.0 = format!("{v:?}");
                }
            }
        }
        let mut m = Message(String::new());
        e.record(&mut m);
        if READ_LINES.iter().any(|l| m.0.starts_with(l)) || m.0.contains(NO_BLOOM) {
            let meta = e.metadata();
            let line = (thread::current().id(), *meta.level(), meta.target().to_owned(), m.0);
            READ_LOGS.lock().unwrap().push(line);
        }
    }

    fn enter(&self, _: &span::Id) {}

    fn exit(&self, _: &span::Id) {}
}

/// Installs [`ReadLogs`] once, before the test's first line.
fn read_logs() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| tracing::subscriber::set_global_default(ReadLogs).unwrap());
}

/// This test's lines in [`READ_LOGS`], as (level, target, message).
fn logs() -> Vec<(Level, String, String)> {
    let me = thread::current().id();
    let all = READ_LOGS.lock().unwrap();
    all.iter().filter(|l| l.0 == me).map(|(_, l, t, m)| (*l, t.clone(), m.clone())).collect()
}

/// O17 P2P-17: a prune that lands between `ProcessGetBlockData`'s data check and its read is
/// `IsBlockPruned`; a read that fails with the data still indexed is the I/O-fault path. Both
/// disconnect unscored and send nothing (np:2811-2829); the log line tells them apart: `net`
/// debug "Block was pruned before it could be read" here, unconditional "Cannot load block from
/// disk" for the fault.
#[test]
fn block_read_racing_a_prune_disconnects() {
    read_logs();
    let (mut sim, _, c) = node(3);
    let p = inbound(&mut sim, 1, Permissions::NONE);
    let target = c[2].id;
    let chain = Arc::clone(&sim.chain);
    let fired = Arc::new(AtomicBool::new(false));
    let f = Arc::clone(&fired);
    sim.chain.set_read_hook(Some(Arc::new(move |r| {
        if r == SimRead::ReadBlock(target) && !f.swap(true, Ordering::SeqCst) {
            chain.prune(target);
        }
    })));
    let sent = exchange(&mut sim, &p, &[("getdata", getdata(&[inv(InvType::Block, &c[2].hash)]))]);
    assert!(fired.load(Ordering::SeqCst));
    assert!(sent.is_empty());
    assert!(p.handle.disconnect_requested());
    assert_eq!(score(&sim, &p), 0);
    sim.chain.set_read_hook(None);
    assert!(matches!(sim.chain.read_block(target), BlockRead::Pruned));
    let logs = logs();
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert_eq!((logs[0].0, logs[0].1.as_str()), (Level::DEBUG, "net"), "{logs:?}");
    assert!(
        logs[0].2.starts_with("Block was pruned before it could be read, disconnect peer="),
        "{logs:?}"
    );
}

/// The I/O-fault half of P2P-17: an entry marked as having data whose read fails.
#[test]
fn block_read_failure_disconnects() {
    read_logs();
    let (mut sim, _, c) = node(2);
    // Connected without a stored block: the read finds nothing, the data is still indexed.
    let more = SimChain::build(&c[2], 1, 1, 3);
    let e = sim.chain.add(&more);
    sim.chain.set_tip(e.id);
    assert!(matches!(sim.chain.read_block(e.id), BlockRead::Failed));
    let p = inbound(&mut sim, 1, Permissions::NONE);
    let sent = exchange(&mut sim, &p, &[("getdata", getdata(&[inv(InvType::Block, &e.hash)]))]);
    assert!(sent.is_empty());
    assert!(p.handle.disconnect_requested());
    assert_eq!(score(&sim, &p), 0);
    let logs = logs();
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert_eq!(logs[0].0, Level::ERROR, "{logs:?}");
    assert!(logs[0].2.starts_with("Cannot load block from disk, disconnect peer="), "{logs:?}");
}

/// A block pruned before the request is on the active chain without data: Core returns before
/// the read, silently, without a disconnect (np:2804-2808).
#[test]
fn getdata_of_an_already_pruned_block_is_ignored() {
    let (mut sim, blocks, c) = node(3);
    sim.chain.set_prune_mode(true);
    sim.chain.prune(c[1].id);
    let p = inbound(&mut sim, 1, Permissions::NONE);
    let sent = exchange(
        &mut sim,
        &p,
        &[(
            "getdata",
            getdata(&[inv(InvType::Block, &c[1].hash), inv(InvType::Block, &c[2].hash)]),
        )],
    );
    assert_eq!(sent, vec![block_msg(&blocks[1])]);
    assert!(!p.handle.disconnect_requested());
}

/// P4b D16: Core reads the position it took with the `HAVE_DATA` test (np:2810, :2819). A prune
/// that marks the entry between the decision and the read, with its file still on disk
/// (`PruneOneBlockFile` before `UnlinkPrunedFiles`, blockstorage.cpp:131-144), still serves the
/// block; once the entry has no data, a later request is ignored silently (np:2806-2808).
#[test]
fn a_block_marked_pruned_after_the_decision_is_served_from_its_file() {
    read_logs();
    let (mut sim, blocks, c) = node(3);
    let p = inbound(&mut sim, 1, Permissions::NONE);
    let target = c[2].id;
    let chain = Arc::clone(&sim.chain);
    let fired = Arc::new(AtomicBool::new(false));
    let f = Arc::clone(&fired);
    sim.chain.set_read_hook(Some(Arc::new(move |r| {
        if r == SimRead::ReadBlock(target) && !f.swap(true, Ordering::SeqCst) {
            chain.prune_index_only(target);
        }
    })));
    let ask = getdata(&[inv(InvType::Block, &c[2].hash)]);
    let sent = exchange(&mut sim, &p, &[("getdata", ask.clone())]);
    assert!(fired.load(Ordering::SeqCst));
    assert_eq!(sent, vec![block_msg(&blocks[1])]);
    assert!(!p.handle.disconnect_requested());
    assert!(logs().is_empty(), "{:?}", logs());
    sim.chain.set_read_hook(None);
    assert!(matches!(sim.chain.read_block(target), BlockRead::Pruned), "read at no position");
    assert!(exchange(&mut sim, &p, &[("getdata", ask)]).is_empty());
    assert!(!p.handle.disconnect_requested());
}

/// P4b D16 for `getblocktxn`: within `MAX_BLOCKTXN_DEPTH` Core reads the position it took under
/// `cs_main` (np:4731, :4737), so an entry marked pruned before the read is still answered from
/// its file, not the read's `assert` (np:4740).
#[test]
fn getblocktxn_reads_the_position_it_took() {
    let (mut sim, blocks, c) = node(12);
    let p = inbound(&mut sim, 1, Permissions::NONE);
    let target = c[2].id;
    let chain = Arc::clone(&sim.chain);
    let fired = Arc::new(AtomicBool::new(false));
    let f = Arc::clone(&fired);
    sim.chain.set_read_hook(Some(Arc::new(move |r| {
        if r == SimRead::ReadBlock(target) && !f.swap(true, Ordering::SeqCst) {
            chain.prune_index_only(target);
        }
    })));
    let sent = exchange(&mut sim, &p, &[("getblocktxn", getblocktxn(&c[2].hash, vec![0]))]);
    assert!(fired.load(Ordering::SeqCst));
    let mut want = c[2].hash.as_bytes().to_vec();
    want.push(1);
    want.extend(blocks[1].txs()[0].core_serialize());
    assert_eq!(sent, vec![(CommandString::BLOCKTXN, want)]);
}

/// `p2p_blocksonly.py`'s serving side: a block-relay-only peer has no tx relay, so `filterload`
/// is read and size-checked (+100 for 51 hash functions) but stores nothing, a
/// `MSG_FILTERED_BLOCK` then gets nothing, `filteradd` neither stores nor scores and
/// `filterclear` returns (np:5544-5556, :2837-2843, :5577-5588, :5598-5599).
#[test]
fn a_block_relay_only_peer_keeps_no_filter() {
    let (mut sim, blocks, c) = node(2);
    let p = sim.connect(1, ConnectionType::BlockRelay, Permissions::NONE);
    sim.handshake(
        &p,
        PeerVersionMsg {
            services: NETWORK,
            ..PeerVersionMsg::v24(SIM_START)
        },
    );
    sim.pass();
    sim.sent(&p);
    let mut filter = BloomFilter::new(10, 0.000_001, 0, BLOOM_UPDATE_ALL);
    filter.insert(blocks[1].txs()[0].txid().as_bytes());
    let ask = getdata(&[inv(InvType::FilteredBlock, &c[2].hash)]);
    let sent = exchange(&mut sim, &p, &[("filterload", filterload(&filter)), ("getdata", ask)]);
    assert!(sent.is_empty(), "{:?}", cmds(&sent));
    assert!(!p.handle.bloom_filter_loaded() && !p.handle.relays_txs());
    let add = enc(&FilterAdd {
        data: vec![1; 8],
    });
    assert!(exchange(&mut sim, &p, &[("filteradd", add), ("filterclear", Vec::new())]).is_empty());
    assert!(!p.handle.bloom_filter_loaded() && !p.handle.relays_txs());
    assert_eq!(score(&sim, &p), 0);
    assert!(!p.handle.disconnect_requested());
    let mut wide = filter.to_msg();
    wide.hash_funcs = 51;
    exchange(&mut sim, &p, &[("filterload", enc(&wide))]);
    assert_eq!(score(&sim, &p), 100);
}

/// `getblocks` on a masternode connection that is not an intra-quorum relay (`CanRelay()` false,
/// np:4690): no `inv` is queued, but the 500th block is still the continuation, whose fetch sends
/// the tip's `inv` (np:4693-4698, :2885-2896). The connection is one the node dialled to a
/// masternode (an inbound one's flag comes from its `version`, which a plain node refuses).
#[test]
fn getblocks_without_can_relay_queues_no_inv_but_keeps_the_continuation() {
    let (mut sim, blocks, c) = node(600);
    let service = Service {
        addr: NetAddr::ipv4([10, 0, 0, 1]),
        port: 19899,
    };
    let p = sim.attach_at(
        service,
        ConnectionType::OutboundFullRelay,
        Permissions::NONE,
        ServiceFlags::NODE_NETWORK,
        true,
        false,
    );
    sim.handshake(
        &p,
        PeerVersionMsg {
            services: NETWORK,
            ..PeerVersionMsg::v24(SIM_START)
        },
    );
    assert!(!p.handle.can_relay());
    // Masternode connections are sent to once per 100 ms (P2a-core).
    let passes = |sim: &mut Sim| {
        for _ in 0..6 {
            sim.clock.advance(Duration::from_millis(100));
            sim.pass();
        }
        sim.sent(&p)
    };
    passes(&mut sim);
    let exchange = |sim: &mut Sim, msgs: &[(&str, Vec<u8>)]| {
        sim.deliver(&p, msgs);
        passes(sim)
    };
    let sent = exchange(&mut sim, &[("getblocks", getblocks(&[c[0].hash], BlockHash::ZERO))]);
    assert!(inv_blocks(&sent).is_empty(), "{:?}", cmds(&sent));
    let sent = exchange(&mut sim, &[("getdata", getdata(&[inv(InvType::Block, &c[500].hash)]))]);
    assert_eq!(sent[0], block_msg(&blocks[499]));
    assert_eq!(inv_blocks(&sent[1..]), [hash256(&c[600].hash)]);
}
