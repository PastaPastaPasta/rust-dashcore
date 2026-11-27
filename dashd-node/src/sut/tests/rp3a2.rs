//! RP3a-2: `testmempoolaccept` and `prioritisetransaction` through the node's command table on a
//! running node, with an outbound peer.

use std::cell::Cell;

use dashd_rpc::UniValue;
use dashd_rpc::request::find_value;

use super::mp1a9::{FEE, PAID, spend_to};
use super::mp1a11::pump_until;
use super::*;

fn field<'a>(obj: &'a UniValue, key: &str) -> &'a UniValue {
    match obj {
        UniValue::Obj(entries) => find_value(entries, key),
        other => panic!("not an object: {}", other.write()),
    }
}

// Core: v24.0.2 src/rpc/mempool.cpp:96-238; src/rpc/mining.cpp:499-528; src/txmempool.cpp:1878-1903
#[test]
fn testmempoolaccept_leaves_pool_and_relay_untouched_and_prioritise_updates_modified_fee() {
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
    let call = |method: &str, params: &str| {
        crate::chain::tests::rpc(&table, method, params)
            .unwrap_or_else(|e| panic!("{method} failed: {e:?}"))
    };

    let tx = spend_to(&blocks[1], &[0x51; 40]);
    let txid = tx.txid();
    let hex: String = tx.core_serialize().iter().map(|b| format!("{b:02x}")).collect();
    let test_reply = call("testmempoolaccept", &format!(r#"[["{hex}"]]"#));
    let arr = UniValue::read(test_reply.as_bytes()).expect("JSON");
    let entry = arr.get_values().unwrap()[0];
    assert_eq!(field(entry, "txid").get_str().unwrap(), txid.to_string());
    assert!(field(entry, "allowed").get_bool().unwrap());
    assert_eq!(field(entry, "vsize").write(), tx.size().to_string());
    let fees = field(entry, "fees");
    assert_eq!(field(fees, "base").write(), "0.00010000");

    let pool = Arc::clone(mempool.pool());
    assert_eq!(pool.read().size(), 0, "testmempoolaccept leaves pool unchanged");
    assert!(!pool.read().exists(&txid));

    // Advance past inv trickle timers and round-trip a ping to prove no inv was sent.
    node.clock.set_mocktime(node.clock.mocktime() + 30);
    peer.send("ping", &42u64.to_le_bytes());
    let inv = Inventory::new(InvType::Tx, Hash256::from_lendian(txid.to_lendian()));
    let got_pong = Cell::new(false);
    let saw_inv = pump_until(
        &node,
        &mut peer,
        "peer never answered pong",
        |cmd, payload| {
            if cmd == "inv"
                && Inv::decode(&mut &payload[..]).expect("an inv").inventory().contains(&inv)
            {
                return true;
            }
            if cmd == "pong" {
                got_pong.set(true);
            }
            false
        },
        || got_pong.get(),
    );
    assert!(!saw_inv, "testmempoolaccept must not send an inv");

    assert_eq!(call("sendrawtransaction", &format!(r#"["{hex}"]"#)), format!(r#""{txid}""#));
    assert!(pool.read().exists(&txid));
    let announced = |cmd: &str, payload: &[u8]| {
        cmd == "inv" && Inv::decode(&mut &payload[..]).expect("an inv").inventory().contains(&inv)
    };
    assert!(pump_until(&node, &mut peer, "the sent tx was never announced", announced, || false));

    let read_fees = || -> (String, String) {
        let raw = call("getrawmempool", "[true]");
        let obj = UniValue::read(raw.as_bytes()).expect("JSON");
        let entry = field(&obj, &txid.to_string());
        let fees = field(entry, "fees");
        (field(fees, "base").write(), field(fees, "modified").write())
    };
    assert_eq!(FEE, 10_000);
    assert_eq!(read_fees(), ("0.00010000".into(), "0.00010000".into()));

    assert_eq!(call("prioritisetransaction", &format!(r#"["{txid}", 25000]"#)), "true");
    assert_eq!(read_fees(), ("0.00010000".into(), "0.00035000".into()));

    assert_eq!(call("prioritisetransaction", &format!(r#"["{txid}", -10000]"#)), "true");
    assert_eq!(read_fees(), ("0.00010000".into(), "0.00025000".into()));

    table.interrupt();
}
