//! RP3a-P2: multi-tx `testmempoolaccept`, `submitpackage` and `getmempoolentry` on a running node.

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

// Core: v24.0.2 src/rpc/mempool.cpp:96-238, :540-575, :749-883
#[test]
fn package_rpcs_test_submit_relay_and_read_mempool_entries() {
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

    let mut spendable = vec![0x61u8; 39];
    spendable.push(0x51);
    let parent = spend_to(&blocks[1], &spendable);
    let child = tx(
        vec![TxIn::new(OutPoint::new(parent.txid(), 0), Vec::new().into(), u32::MAX)],
        &[(PAID - 2 * FEE, &spendable)],
    );
    let (parent_id, child_id) = (parent.txid(), child.txid());
    let to_hex =
        |t: &Tx| -> String { t.core_serialize().iter().map(|b| format!("{b:02x}")).collect() };
    let pkg_param = format!(r#"[["{}", "{}"]]"#, to_hex(&parent), to_hex(&child));

    let test_reply = call("testmempoolaccept", &pkg_param);
    let arr = UniValue::read(test_reply.as_bytes()).expect("JSON");
    let items = arr.get_values().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(field(items[0], "txid").get_str().unwrap(), parent_id.to_string());
    assert!(field(items[0], "allowed").get_bool().unwrap());
    assert_eq!(field(items[1], "txid").get_str().unwrap(), child_id.to_string());
    assert!(field(items[1], "allowed").get_bool().unwrap());

    let pool = Arc::clone(mempool.pool());
    assert_eq!(pool.read().size(), 0, "package testmempoolaccept leaves pool empty");

    let submit_reply = call("submitpackage", &pkg_param);
    let submit_obj = UniValue::read(submit_reply.as_bytes()).expect("JSON");
    let results = field(&submit_obj, "tx-results");
    for (id, t) in [(parent_id, &parent), (child_id, &child)] {
        let entry = field(results, &id.to_string());
        assert_eq!(field(entry, "txid").get_str().unwrap(), id.to_string());
        assert_eq!(field(entry, "size").write(), t.size().to_string());
        assert_eq!(field(field(entry, "fees"), "base").write(), "0.00010000");
    }
    assert_eq!(pool.read().size(), 2);
    assert!(pool.read().exists(&parent_id));
    assert!(pool.read().exists(&child_id));

    let raw_all = UniValue::read(call("getrawmempool", "[true]").as_bytes()).expect("JSON");
    for id in [parent_id, child_id] {
        let single = call("getmempoolentry", &format!(r#"["{id}"]"#));
        assert_eq!(single, field(&raw_all, &id.to_string()).write());
    }

    let mut want: Vec<Inventory> = [parent_id, child_id]
        .into_iter()
        .map(|id| Inventory::new(InvType::Tx, Hash256::from_lendian(id.to_lendian())))
        .collect();
    node.clock.set_mocktime(node.clock.mocktime() + 30);
    let saw_both = pump_until(
        &node,
        &mut peer,
        "submitpackage did not relay inv for both parent and child",
        |cmd, payload| {
            if cmd == "inv" {
                let items = Inv::decode(&mut &payload[..]).expect("an inv");
                want.retain(|inv| !items.inventory().contains(inv));
                return want.is_empty();
            }
            false
        },
        || false,
    );
    assert!(saw_both);

    table.interrupt();
}
