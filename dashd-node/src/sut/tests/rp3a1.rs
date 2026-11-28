//! RP3a-1: `sendrawtransaction` through the node's command table on a running node, with an
//! outbound peer.

use std::sync::{Mutex, mpsc};
use std::time::Duration;

use dash_consensus::TxRef;
use dashd_chainstate::chain::ValidationInterface;

use super::mp1a9::{PAID, spend_to};
use super::mp1a11::pump_until;
use super::*;

/// Reports each `TransactionAddedToMempool` as it starts, then holds the signals thread until
/// the test drops the gate's sender.
struct GatedAdded {
    entered: Mutex<mpsc::Sender<TxHash>>,
    gate: Mutex<mpsc::Receiver<()>>,
}

impl ValidationInterface for GatedAdded {
    fn transaction_added_to_mempool(&self, tx: &TxRef, _: i64, _: u64) {
        let _ = self.entered.lock().unwrap().send(tx.txid());
        let _ = self.gate.lock().unwrap().recv();
    }
}

/// The reply comes after the admission's `TransactionAddedToMempool` has run, the pool holds the
/// transaction as unbroadcast, and the peer is sent its `inv`. A resend answers the txid with no
/// second admission; once a block holds it, the answer is -27.
// Core: v24.0.2 src/node/transaction.cpp:33-125; src/rpc/mempool.cpp:38-90
#[test]
fn sendrawtransaction_admits_waits_for_the_queue_and_relays() {
    use dash_consensus::CoreSerialize as _;
    use dash_num::Hash256;
    use dash_p2p_core::{Inv, InvType, Inventory};
    use dash_types::Numeric as _;
    use dash_types::codec::BaseCodec as _;

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
    let argv =
        ["-blocksonly=0", "-acceptnonstdtxn=1", "-dnsseed=0", "-fixedseeds=0", "-v2transport=0"];
    let node = crate::p2p::amb::node(dir.path(), &[&argv[..], &[&*connect, &*mocktime]].concat());
    let mut peer = crate::p2p::amb::Peer::dialled(&listener, &p, 0, now);
    let chain = node.chain().expect("V runs");
    let v = chain.handle().clone();
    for block in &blocks[1..] {
        let r = v.process_block(Arc::clone(block), true, None, true).recv();
        assert!(r.expect("V replied").state.is_valid());
    }
    let (entered_tx, entered) = mpsc::channel();
    let (open, gate) = mpsc::channel::<()>();
    let added = Arc::new(GatedAdded {
        entered: Mutex::new(entered_tx),
        gate: Mutex::new(gate),
    });
    let watcher: Arc<dyn ValidationInterface> = Arc::clone(&added) as _;
    chain.signals().register(Arc::clone(&watcher));

    let mempool = Arc::clone(node.mempool().expect("step 7 built it"));
    let table = crate::rpc::command_table(
        &p,
        &Arc::new(Logger::new()),
        &Arc::default(),
        &node.clock,
        (true, &crate::scheduler::Scheduler::new()),
        &node.rpc_chain(),
        &node.rpc_net(),
        &Arc::default(),
        &Arc::default(),
        Default::default(),
        std::env::temp_dir(),
    );
    table.start();
    table.set_warmup_finished();
    let tx = spend_to(&blocks[1], &[0x51; 40]);
    let txid = tx.txid();
    let hex: String = tx.core_serialize().iter().map(|b| format!("{b:02x}")).collect();
    let params = format!(r#"["{hex}"]"#);
    let send = || crate::chain::tests::rpc(&table, "sendrawtransaction", &params);

    let minute = Duration::from_secs(60);
    std::thread::scope(|s| {
        let (replied_tx, replied) = mpsc::channel();
        s.spawn(move || replied_tx.send(send()));
        assert_eq!(entered.recv_timeout(minute), Ok(txid), "the admission's event is delivered");
        let early = replied.recv_timeout(Duration::from_secs(2));
        assert!(early.is_err(), "the reply waited for the queue: {early:?}");
        drop(open);
        assert_eq!(replied.recv_timeout(minute), Ok(Ok(format!(r#""{txid}""#))));
    });
    let pool = Arc::clone(mempool.pool());
    assert!(pool.read().exists(&txid) && pool.read().is_unbroadcast_tx(&txid));
    let inv = Inventory::new(InvType::Tx, Hash256::from_lendian(txid.to_lendian()));
    let announced = |cmd: &str, payload: &[u8]| {
        cmd == "inv" && Inv::decode(&mut &payload[..]).expect("an inv").inventory().contains(&inv)
    };
    assert!(pump_until(&node, &mut peer, "the tx was never announced", announced, || false));

    assert_eq!(send(), Ok(format!(r#""{txid}""#)));
    assert!(entered.try_recv().is_err(), "no second admission");

    let tip = blocks.last().unwrap();
    let block = mine(&p, tip.header(), 102, t0 + 102, &[(PAID, &[0x51])], vec![tx.clone()]);
    let r = v.process_block(Arc::clone(&block), true, None, true).recv();
    assert!(r.expect("V replied").state.is_valid());
    let in_chain = r#"{"code":-27,"message":"Transaction already in block chain"}"#;
    assert_eq!(send(), Err((-27, in_chain.to_owned())));
    assert!(!pool.read().exists(&txid), "the block removed it");
    chain.signals().unregister(&watcher);
    table.interrupt();
}
