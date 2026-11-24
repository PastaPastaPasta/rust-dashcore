//! MP1a-11: `getrawmempool` and `getmempoolinfo` through the node's command table, over the
//! node's live pool.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future as _;
use std::sync::mpsc;
use std::time::Duration;

use dashd_rpc::UniValue;
use dashd_rpc::mempool::MempoolView as _;

use super::mp1a9::{COIN, FEE, PAID, assert_valid, live, mature, pool, spend_to};
use super::*;

fn call(sut: &NodeSut, method: &str, params: &str) -> String {
    sut.rpc(method, params).expect("V runs").unwrap_or_else(|e| panic!("{method}: {e:?}"))
}

fn parse(json: &str) -> Vec<(String, UniValue)> {
    match UniValue::read(json.as_bytes()).expect("JSON") {
        UniValue::Obj(kv) => kv,
        v => panic!("not an object: {}", v.write()),
    }
}

fn txids(json: &str) -> BTreeSet<String> {
    match UniValue::read(json.as_bytes()).expect("JSON") {
        UniValue::Arr(a) => a.iter().map(|v| v.get_str().unwrap().to_owned()).collect(),
        v => panic!("not an array: {}", v.write()),
    }
}

/// A parent and its child in the live pool: the verbose entries carry the pool's sizes, times,
/// heights and package statistics, `depends` and `spentby` link the two, `unbroadcast` follows
/// the pool's set; both shapes list the same txids; `mempool_sequence` moves by one per
/// admission; `getmempoolinfo` reports the pool's figures.
// Core: rpc/mempool.cpp:268-406, :657-704, txmempool.cpp:1984-1988 at v24.0.0
#[test]
fn the_mempool_commands_read_the_live_pool() {
    let dir = tempfile::tempdir().unwrap();
    let (mut sut, _) = live(dir.path(), &[]);
    imported(&mut sut);
    let blocks = mature(&sut, 101);
    let s0 = pool(&sut).read().sequence();
    let empty = format!("{{\"txids\":[],\"mempool_sequence\":{s0}}}");
    assert_eq!(call(&sut, "getrawmempool", "[false, true]"), empty);
    assert_eq!(call(&sut, "getrawmempool", "[true]"), "{}");

    // The parent's first output is a lone `OP_TRUE`, which the child spends with a clean stack.
    let coinbase = TxIn::new(OutPoint::new(blocks[0].txs()[0].txid(), 0), Vec::new().into(), !0);
    let parent = tx(vec![coinbase], &[(PAID - FEE - COIN, &[0x51]), (COIN, &[0x51; 40])]);
    let input = TxIn::new(OutPoint::new(parent.txid(), 0), Vec::new().into(), u32::MAX);
    let child = tx(vec![input], &[(PAID - 2 * FEE - COIN, &[0x51; 40])]);
    let handle = sut.steps.mempool().unwrap().handle();
    assert_valid(&handle.accept(parent.clone(), false).unwrap());
    let s1 = format!("\"mempool_sequence\":{}}}", s0 + 1);
    assert!(call(&sut, "getrawmempool", "[false, true]").ends_with(&s1));
    assert_valid(&handle.accept(child.clone(), false).unwrap());
    pool(&sut).add_unbroadcast_tx(parent.txid());

    let (p, c) = (parent.txid().to_string(), child.txid().to_string());
    let state = pool(&sut);
    let state = state.read();
    assert_eq!(state.sequence(), s0 + 2);
    let entry = |txid| state.entries().get(txid).expect("resident");
    let (pe, ce) = (entry(&parent.txid()), entry(&child.txid()));
    let (vp, vc) = (pe.vsize(), ce.vsize());
    let shared = |e: &dashd_mempool::pool::PoolEntry| {
        format!(
            "{{\"vsize\":{},\"time\":{},\"height\":{}",
            e.vsize(),
            e.metadata().time,
            e.metadata().height
        )
    };
    let want_parent = format!(
        "{},\"descendantcount\":2,\"descendantsize\":{},\"ancestorcount\":1,\"ancestorsize\":{vp},\
         \"fees\":{{\"base\":0.00010000,\"modified\":0.00010000,\"ancestor\":0.00010000,\
         \"descendant\":0.00020000}},\"depends\":[],\"spentby\":[\"{c}\"],\
         \"instantlock\":\"false\",\"unbroadcast\":true}}",
        shared(pe),
        vp + vc
    );
    let want_child = format!(
        "{},\"descendantcount\":1,\"descendantsize\":{vc},\"ancestorcount\":2,\"ancestorsize\":{},\
         \"fees\":{{\"base\":0.00010000,\"modified\":0.00010000,\"ancestor\":0.00020000,\
         \"descendant\":0.00010000}},\"depends\":[\"{p}\"],\"spentby\":[],\
         \"instantlock\":\"false\",\"unbroadcast\":false}}",
        shared(ce),
        vp + vc
    );
    let (usage, max) = (state.dynamic_memory_usage(), state.options().max_size_bytes);
    drop(state);

    let verbose = parse(&call(&sut, "getrawmempool", "[true]"));
    let by_txid: BTreeMap<_, _> = verbose.iter().map(|(k, v)| (k.clone(), v.write())).collect();
    assert_eq!(by_txid.get(&p), Some(&want_parent));
    assert_eq!(by_txid.get(&c), Some(&want_child));
    let listed = txids(&call(&sut, "getrawmempool", "[]"));
    assert_eq!(listed, BTreeSet::from([p.clone(), c.clone()]));
    assert_eq!(by_txid.keys().cloned().collect::<BTreeSet<_>>(), listed);
    let with_sequence = parse(&call(&sut, "getrawmempool", "[false, true]"));
    assert_eq!(txids(&with_sequence[0].1.write()), listed);
    assert_eq!(with_sequence[1].1.write(), (s0 + 2).to_string());

    let info = format!(
        "{{\"loaded\":true,\"size\":2,\"bytes\":{},\"usage\":{usage},\"total_fee\":0.00020000,\
         \"maxmempool\":{max},\"mempoolminfee\":0.00001000,\"minrelaytxfee\":0.00001000,\
         \"instantsendlocks\":0,\"unbroadcastcount\":1,\"pendingassetunlocks\":0.00000000}}",
        vp + vc
    );
    assert_eq!(call(&sut, "getmempoolinfo", "[]"), info);
}

/// `loaded` is the pool's load-tried flag: false on the pool step 7 attaches, before the import
/// reaches `LoadMempool`, and true once it has. Without a pool the slot reads a fresh one.
// Core: rpc/mempool.cpp:664, txmempool.cpp:2166-2170, validation.cpp:4995-5000 at v24.0.0
#[test]
fn loaded_turns_true_when_the_import_ends() {
    let dir = tempfile::tempdir().unwrap();
    let (mut sut, _) = live(dir.path(), &[]);
    let fresh = crate::rpc::ChainSlot::default();
    let info = fresh.info();
    assert!(!info.loaded);
    assert_eq!((info.size, fresh.txids_and_sequence()), (0, (Vec::new(), 1)));
    let (send, seen) = mpsc::channel();
    sut.next_before_pool_attach = Some(Box::new(move |pool, _| {
        let slot = crate::rpc::ChainSlot::default();
        slot.attach_mempool(Arc::clone(pool));
        send.send(slot.info().loaded).expect("the test");
    }));
    assert_eq!(sut.restart(Teardown::Clean), 2);
    assert_eq!(seen.try_recv(), Ok(false), "before LoadMempool");
    imported(&mut sut);
    assert!(call(&sut, "getmempoolinfo", "[]").starts_with("{\"loaded\":true,\"size\":0,"));
}

/// Polls `peer` (moving `node`'s mock clock a second a round, for the request and trickle
/// timers) until `done` holds or a frame matches `seen`, which it returns; panics after a minute.
pub(super) fn pump_until(
    node: &crate::node::Node,
    peer: &mut crate::p2p::amb::Peer,
    what: &str,
    mut seen: impl FnMut(&str, &[u8]) -> bool,
    done: impl Fn() -> bool,
) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if peer.poll().iter().any(|(cmd, payload)| seen(cmd.as_str(), payload)) {
            return true;
        }
        if done() {
            return false;
        }
        assert!(std::time::Instant::now() < deadline, "{what}");
        node.clock.set_mocktime(node.clock.mocktime() + 1);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// MP1a-11 1c: P2P runs over the node's pool. Right after startup an announced transaction is
/// requested (`AlreadyHave` is false for one the pool lacks), the `tx` is admitted, and a
/// `cmpctblock` holding it is rebuilt from the pool (`InitData`'s mempool pass) and connected,
/// with no `getblocktxn`. Over P2P's empty pool (`NoTxPool`) the inv is requested too, but the
/// `tx` is dropped unadmitted (`enabled()` is false).
// Core: net_processing.cpp:1990-2011, :4575-4594, :4879-4960, :5162-5270;
// blockencodings.cpp:107-139; init.cpp:2194-2199 at v24.0.0
#[test]
fn a_relayed_tx_is_requested_admitted_and_fills_a_compact_block() {
    use dash_consensus::CoreSerialize as _;
    use dash_num::Hash256;
    use dash_p2p_core::{GetData, Inv, InvType, Inventory};
    use dash_types::Numeric as _;
    use dash_types::codec::BaseCodec as _;
    use dashd_p2p::processing::compact::CompactBlock;

    let p = crate::p2p::amb::regtest();
    let genesis = dashd_chainstate::chain::genesis_block(&p);
    let t0 = genesis.header().time();
    let mut blocks: Vec<Arc<dash_consensus::Block>> = vec![genesis];
    for h in 1..=101 {
        let prev = blocks.last().unwrap().header();
        blocks.push(mine(&p, prev, h, t0 + h, &[(PAID, &[0x51])], vec![]));
    }
    let now = i64::from(t0) + 200;
    let dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let connect = format!("-connect={}", listener.local_addr().unwrap());
    let mocktime = format!("-mocktime={now}");
    let argv = ["-acceptnonstdtxn=1", "-dnsseed=0", "-fixedseeds=0", "-v2transport=0"];
    let node = crate::p2p::amb::node(dir.path(), &[&argv[..], &[&*connect, &*mocktime]].concat());
    let mut peer = crate::p2p::amb::Peer::dialled(&listener, &p, 0, now);
    let v = node.chain().expect("V runs").handle().clone();
    for block in &blocks[1..] {
        let r = v.process_block(Arc::clone(block), true, None, true).recv();
        assert!(r.expect("V replied").state.is_valid());
    }

    let tx = spend_to(&blocks[1], &[0x51; 40]);
    let inv = |txid: TxHash| Inventory::new(InvType::Tx, Hash256::from_lendian(txid.to_lendian()));
    let mut msg = Vec::new();
    Inv::new(vec![inv(tx.txid())]).encode(&mut msg);
    peer.send("inv", &msg);
    let asks = |cmd: &str, payload: &[u8]| {
        cmd == "getdata"
            && GetData::decode(&mut &payload[..])
                .expect("a getdata")
                .inventory()
                .contains(&inv(tx.txid()))
    };
    assert!(pump_until(&node, &mut peer, "the tx was never requested", asks, || false));
    peer.send("tx", &tx.core_serialize());
    let pool = Arc::clone(node.mempool().expect("step 7 built it").pool());
    let admitted = || pool.read().exists(&tx.txid());
    pump_until(&node, &mut peer, "the tx was not admitted", |_, _| false, admitted);

    let tip = blocks.last().unwrap();
    let block = mine(&p, tip.header(), 102, t0 + 102, &[(PAID, &[0x51])], vec![tx.clone()]);
    peer.send("cmpctblock", &CompactBlock::from_block(&block, 7).wire_bytes());
    let connected = || v.tip_watch().get() == Some(block.hash());
    let fetched = |cmd: &str, _: &[u8]| cmd == "getblocktxn";
    assert!(!pump_until(&node, &mut peer, "the block did not connect", fetched, connected));
    assert!(!pool.read().exists(&tx.txid()), "the block removed it");
}

/// MP1a-11 1c, P3's check 4: P2P's adapter over the node's real pool once its admission worker
/// has stopped. The reads answer from the pool as it was, an admission is `no-mempool` and
/// closes the adapter; nothing hangs.
// Core: net_processing.cpp:4959; validation.cpp:4920-4932 at v24.0.0
#[test]
fn p2p_reads_and_admissions_after_the_worker_stops() {
    use dash_consensus::TxValidationResult;
    use dashd_p2p::processing::seams::TxPool as _;

    use crate::p2p::txpool::NodeTxPool;

    let dir = tempfile::tempdir().unwrap();
    let (mut sut, _) = live(dir.path(), &[]);
    imported(&mut sut);
    let blocks = mature(&sut, 102);
    let mempool = Arc::clone(sut.steps.mempool().unwrap());
    let a = spend_to(&blocks[0], &[0x51; 40]);
    assert_valid(&mempool.handle().accept(a.clone(), false).unwrap());
    let p2p = NodeTxPool::new(mempool.handle());
    sut.steps.shutdown();

    assert!(p2p.enabled() && p2p.exists(&a.txid()));
    assert_eq!((p2p.size(), p2p.info_all().len()), (1, 1));
    assert_eq!(p2p.info(&a.txid()).map(|i| i.tx.txid()), Some(a.txid()));
    let b = spend_to(&blocks[1], &[0x51; 40]);
    let mut wait = p2p.accept(b, Some(1));
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let std::task::Poll::Ready(result) = std::pin::Pin::new(&mut wait).poll(&mut cx) else {
        panic!("a stopped pool's admission is answered at once");
    };
    assert_eq!(result.validation_result, TxValidationResult::NoMempool);
    assert!(!p2p.enabled(), "closed for good");
}
