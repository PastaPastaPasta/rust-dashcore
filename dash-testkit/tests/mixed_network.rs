//! `MixedNetwork`: dashd nodes from `DASHD_PATH` (`eval $(python3 contrib/setup-dashd.py)`) and
//! rdashd processes. Today rdashd joins as a process only; the edges into and out of it, and its
//! RPC, wait for P2b, P2a-net and RP0.
#![cfg(unix)]

use std::time::{Duration, Instant};

use dash_testkit::{MixedNetwork, MockTime, NodeId, NodeRpc, PATIENCE};
use dashcore::{Address, Network};

fn skip() -> bool {
    let skip = std::env::var_os("SKIP_DASHD_TESTS").is_some();
    if skip {
        eprintln!("Skipping dashd integration test (SKIP_DASHD_TESTS is set)");
    }
    skip
}

/// Two dashd, connected, and one rdashd, all on one mock clock; a block mined at the network's
/// time reaches the other dashd, and `advance_mocktime` moves the dashd clocks.
#[tokio::test]
async fn dashd_nodes_and_an_rdashd_share_one_network_and_clock() {
    if skip() {
        return;
    }
    // After genesis, so a block's time is the mock time (not its parent's median + 1).
    let start = MockTime::at(MockTime::regtest_genesis().time() + 1000);
    let mut network = MixedNetwork::builder()
        .dashd(2)
        .rdashd(1)
        .connect(NodeId::Dashd(0), NodeId::Dashd(1))
        .mocktime(start)
        .start()
        .await
        .unwrap();

    let d0 = network.rpc(NodeId::Dashd(0)).unwrap();
    let d1 = network.rpc(NodeId::Dashd(1)).unwrap();
    let d1_addr = network.p2p_addr(NodeId::Dashd(1)).unwrap().to_string();
    // `start` returned with the edge a peer connection at both ends.
    let peers = d0.peers().unwrap();
    assert!(peers.iter().any(|p| p.addr == d1_addr && p.handshaken()), "{peers:?}");
    let peers = d1.peers().unwrap();
    assert!(peers.iter().any(|p| p.inbound && p.handshaken()), "{peers:?}");
    assert!(network.rust(0).log().contains("init message: Done loading"));
    let mark = format!("(mocktime: {})", start.time());
    assert!(network.rust(0).log().contains(&mark), "rdashd runs on the network's clock");

    let miner = Address::dummy(Network::Regtest, 0);
    network.dashd(0).generate_blocks(1, &miner);
    let tip = d0.get_best_block_hash().unwrap();
    assert_eq!(d0.get_block_time(&tip).unwrap(), start.time());
    wait_for_tip(&d1, &tip).await;

    // rdashd has no setmocktime yet (RP0), so move the dashd clocks directly.
    let mut later = start;
    later.advance(600);
    later.apply([&d0, &d1]).unwrap();
    network.dashd(0).generate_blocks(1, &miner);
    let tip = d0.get_best_block_hash().unwrap();
    assert_eq!(d0.get_block_time(&tip).unwrap(), later.time());
    wait_for_tip(&d1, &tip).await;

    let stopped = network.stop_rdashd().await;
    assert_eq!(stopped.len(), 1);
    assert!(stopped[0].as_ref().unwrap().is_clean());
}

#[tokio::test]
#[ignore = "needs P2b (-listen/-port) and P2a-net (-connect): rdashd has no P2P yet"]
async fn dashd_and_rdashd_connect_both_ways() {
    if skip() {
        return;
    }
    let network = MixedNetwork::builder()
        .dashd(1)
        .rdashd(2)
        .connect(NodeId::Dashd(0), NodeId::Rust(0))
        .connect(NodeId::Rust(1), NodeId::Dashd(0))
        .start()
        .await
        .unwrap();
    let d0 = network.rpc(NodeId::Dashd(0)).unwrap();
    let peers = d0.peers().unwrap();
    let r0 = network.p2p_addr(NodeId::Rust(0)).unwrap().to_string();
    assert!(peers.iter().any(|p| p.addr == r0 && p.handshaken()), "{peers:?}");
    assert!(peers.iter().any(|p| p.inbound && p.handshaken()), "{peers:?}");
    assert_eq!(peers.len(), 2, "{peers:?}");
}

#[tokio::test]
#[ignore = "needs RP0 (rdashd RPC: setmocktime) and P2a-net/P2b (block relay to rdashd)"]
async fn advance_mocktime_moves_every_node() {
    if skip() {
        return;
    }
    let mut network = MixedNetwork::builder()
        .dashd(1)
        .rdashd(1)
        .rdashd_rpc(true)
        .connect(NodeId::Rust(0), NodeId::Dashd(0))
        .mocktime(MockTime::regtest_genesis())
        .start()
        .await
        .unwrap();
    network.advance_mocktime(600).unwrap();
    let rust = network.rpc(NodeId::Rust(0)).unwrap();
    network.dashd(0).generate_blocks(1, &Address::dummy(Network::Regtest, 0));
    let tip = network.rpc(NodeId::Dashd(0)).unwrap().get_best_block_hash().unwrap();
    wait_for_tip(&rust, &tip).await;
}

async fn wait_for_tip(node: &impl NodeRpc, tip: &dashcore::BlockHash) {
    let deadline = Instant::now() + PATIENCE;
    while node.get_best_block_hash().ok().as_ref() != Some(tip) {
        assert!(Instant::now() < deadline, "the block {tip} did not arrive");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
