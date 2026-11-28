//! CJ1-W5 on an in-process node: step 7c's CoinJoin role, step 10a's 1 s tasks, and the mixing
//! counters across a clean restart through `mncache.dat`. Core citations are v24.0.0.

use std::io::Write as _;
use std::net::TcpStream;

use dash_coinjoin::queue::MixingMeta as _;
use dash_consensus::BlsCtx;
use dash_dml::testkit::{dmn_with, operator_key, operator_sig};
use dash_dml::{Dmn, MnList, MnType};
use dash_p2p_core::DsQueue;
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{BlockHash, TxHash};
use dash_types::codec::BaseCodec;
use dashd_chainstate::chain::Queued;
use dashd_coinjoin::adapters::MetaManMixing;
use dashd_masternode::meta::{self, MasternodeMetaMan};
use dashd_masternode::tip::{TipListRef, TipMnList};
use dashd_p2p::sim::{PeerVersionMsg, v1_frame};

use super::*;
use crate::coinjoin::role::{Role, Second, Ticks};

mod accept;

const MOCKTIME: i64 = 1_700_000_000;
const DENOM: i32 = 16;

/// The regtest operator key `a_regtest_key_attaches_the_active_masternode_and_runs_its_init` uses.
fn key() -> String {
    format!("-masternodeblsprivkey={}", "01".repeat(32))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Masternode `n` with operator key `n`.
fn masternode(n: u8) -> Arc<Dmn> {
    dmn_with(n, MnType::Regular, |_, cold| {
        cold.version = 2;
        cold.pub_key_operator = operator_key(n, BlsSchemeKind::Basic);
    })
}

fn pro(n: u8) -> TxHash {
    TxHash::from(*masternode(n).pro_tx_hash.as_bytes())
}

/// A tip list holding masternodes `ns`, for step 7c's override.
fn masternodes(ns: &[u8]) -> TipMnList {
    let mut list = MnList::new(BlockHash::from([1; 32]), 1, 0);
    for &n in ns {
        list.insert_for_tests(masternode(n));
    }
    TipMnList::fixed(TipListRef {
        list: Arc::new(list),
        bls: BlsCtx::BASIC,
    })
}

/// Masternode `n`'s signed, unready queue at the mock time.
fn queue(n: u8) -> DsQueue {
    let unsigned = DsQueue::new(DENOM, pro(n), MOCKTIME, false, Vec::new());
    let sig = operator_sig(n, unsigned.signature_hash().as_bytes());
    DsQueue::new(DENOM, pro(n), MOCKTIME, false, sig.to_bytes(BlsSchemeKind::Basic).to_vec())
}

/// An inbound V1 peer that sends `version`, `verack` and then `queues`.
fn send_queues(node: &Node, queues: &[DsQueue]) -> TcpStream {
    let net = node.net.as_ref().expect("step 6 built it");
    let addr = net.connman().expect("started").listen_addrs()[0];
    let chain = &node.params.chain;
    let mut bytes = v1_frame(chain, "version", &PeerVersionMsg::v24(MOCKTIME).payload());
    bytes.extend(v1_frame(chain, "verack", &[]));
    for q in queues {
        let mut payload = Vec::new();
        q.encode(&mut payload);
        bytes.extend(v1_frame(chain, "dsq", &payload));
    }
    let mut peer = TcpStream::connect(addr).unwrap();
    peer.write_all(&bytes).unwrap();
    peer
}

fn kinds(ticks: &Ticks) -> Vec<Second> {
    ticks.around(Arc::new(|_| {})).into_iter().map(|(kind, _)| kind).collect()
}

/// `IsMixingThresholdExceeded` for each of `pros` at every masternode count up to 30.
fn answers(metaman: &Arc<MasternodeMetaMan>, pros: &[TxHash]) -> Vec<bool> {
    let meta = MetaManMixing(Arc::clone(metaman));
    let meta = &meta;
    pros.iter()
        .flat_map(|p| (0..=30).map(move |n| meta.is_mixing_threshold_exceeded(p, n)))
        .collect()
}

/// An active masternode installs the server before SyncManager (the registry refuses the other
/// order, which fails init) and no `dsq` extension, the server being `dsq`'s one owner. Its 1 s
/// tasks are the server's, then SyncManager's, with no queue maintenance.
// Core: init.cpp:2174-2180, :2228-2233, :2366-2373
#[test]
fn a_masternode_installs_the_server_before_sync_manager_and_no_queue_manager() {
    let dir = tempfile::tempdir().unwrap();
    let argv = [key(), "-dnsseed=0".into(), "-fixedseeds=0".into()];
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let (mut node, _, stderr) = start(dir.path(), &argv);
    assert!(node.app_init_early() && node.app_init_main(), "{:?}", stderr.lock().unwrap());
    let net = node.net.as_ref().expect("step 6 built it");
    assert!(matches!(net.coinjoin_role(), Some(Role::Server(_))));
    let ticks = net.coinjoin_ticks();
    assert!(ticks.wallet.is_none());
    assert_eq!(kinds(&ticks), [Second::CoinJoinServer, Second::SyncManager]);
    node.interrupt();
    node.shutdown();
}

/// A node that relays transactions, as by default, installs the wallet queue manager as the `dsq`
/// extension and no server; its one queue-maintenance task runs every second after SyncManager's.
// Core: init.cpp:2174-2180; coinjoin/walletman.cpp:98-110, :121-137
#[test]
fn a_relaying_node_installs_the_queue_manager_and_no_server() {
    let dir = tempfile::tempdir().unwrap();
    let (mut node, _, stderr) = start(dir.path(), &[]);
    assert!(node.app_init_early() && node.app_init_main(), "{:?}", stderr.lock().unwrap());
    let net = node.net.as_ref().expect("step 6 built it");
    let Some(Role::Wallet(wallet)) = net.coinjoin_role() else {
        panic!("not the wallet queue manager");
    };
    assert!(wallet.schedule().is_some());
    let ticks = net.coinjoin_ticks();
    assert!(ticks.server.is_none());
    assert_eq!(kinds(&ticks), [Second::SyncManager, Second::CoinJoinWallet]);
    node.shutdown();
}

/// Step 10a puts the role's ticks on the node's scheduler: relaying adds exactly the one 1 s
/// queue-maintenance task to a blocks-only node's queue.
// Core: init.cpp:2372-2373; coinjoin/walletman.cpp:133-134
#[test]
fn a_relaying_node_s_scheduler_holds_one_more_task() {
    let tasks = |argv: &[&str]| {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) = start(dir.path(), argv);
        assert!(node.app_init_early() && node.app_init_main(), "{:?}", stderr.lock().unwrap());
        let n = node.scheduler.as_ref().expect("started at step 4a").queue_info().0;
        node.shutdown();
        n
    };
    assert_eq!(tasks(&[]), tasks(&["-blocksonly=1"]) + 1);
    assert_eq!(tasks(&[]), tasks(&["-blocksonly=0"]));
}

/// Under `-blocksonly` the queue manager keeps no queues and no 1 s maintenance task is
/// scheduled.
// Core: init.cpp:2178-2179 (`!ignores_incoming_txs`); coinjoin/walletman.cpp:107, :133-134
#[test]
fn blocks_only_keeps_no_queues_and_no_queue_task() {
    for argv in [&["-blocksonly"][..], &["-blocksonly=1"]] {
        let dir = tempfile::tempdir().unwrap();
        let (mut node, _, stderr) = start(dir.path(), argv);
        assert!(node.app_init_early() && node.app_init_main(), "{:?}", stderr.lock().unwrap());
        let net = node.net.as_ref().expect("step 6 built it");
        let Some(Role::Wallet(wallet)) = net.coinjoin_role() else {
            panic!("not the wallet queue manager");
        };
        assert!(wallet.schedule().is_none() && wallet.is_empty(), "{argv:?}");
        assert_eq!(kinds(&net.coinjoin_ticks()), [Second::SyncManager], "{argv:?}");
        node.shutdown();
    }
}

/// Two masternodes' queues raise the counters through the installed `dsq` owner (the server's
/// `AllowMixing` on a masternode, the wallet queue manager's otherwise), over the node's
/// `mn_metaman`. A clean restart loads them from `mncache.dat`, and `IsMixingThresholdExceeded`
/// answers as before.
// Core: coinjoin/server.cpp:128-190; coinjoin/walletman.cpp:261-326; masternode/meta.cpp:93-116;
// init.cpp:2249-2255, :418-419
fn counters_survive_a_clean_restart(as_masternode: bool) {
    let dir = tempfile::tempdir().unwrap();
    let pros = [pro(8), pro(9)];
    let run = |port: u16| {
        let (key, bind) = (key(), format!("-bind=127.0.0.1:{port}"));
        let mocktime = format!("-mocktime={MOCKTIME}");
        let mut argv = vec![bind.as_str(), "-listenonion=0", "-dnsseed=0"];
        argv.extend(["-fixedseeds=0", mocktime.as_str()]);
        if as_masternode {
            argv.push(&key);
            start(dir.path(), &argv)
        } else {
            start(dir.path(), &argv)
        }
    };
    let (mut node, _, stderr) = run(free_port());
    node.tip_override = Some(masternodes(&[8, 9]));
    assert!(node.app_init_early() && node.app_init_main(), "{:?}", stderr.lock().unwrap());
    // `loadblk`'s tip reaches `mn_sync` in IBD, which would undo an earlier switch.
    assert!(node.steps.join_import(), "loadblk ends");
    let net = node.net.as_ref().expect("step 6 built it");
    assert_eq!(matches!(net.coinjoin_role(), Some(Role::Server(_))), as_masternode);
    let (tx, rx) = crossbeam_channel::bounded(1);
    node.steps.chain().expect("step 7").signals().enqueue(Queued::Barrier(tx));
    rx.recv_timeout(Duration::from_secs(60)).expect("the signals thread reaches the barrier");
    net.mn_sync().expect("the masternode services").switch_to_next_asset();
    let _peer = send_queues(&node, &[queue(8), queue(9)]);
    let metaman = Arc::clone(node.steps.metaman().expect("made at step 6"));
    let deadline = Instant::now() + Duration::from_secs(60);
    while metaman.snapshot().dsq_count != 2 {
        assert!(Instant::now() < deadline, "the queues never reached mn_metaman");
        std::thread::sleep(Duration::from_millis(10));
    }
    let before = (metaman.snapshot(), answers(&metaman, &pros));
    assert!(before.1.contains(&true) && before.1.contains(&false), "{:?}", before.1);
    node.interrupt();
    node.shutdown();
    drop(metaman);

    let (mut node, _, stderr) = run(free_port());
    assert!(node.app_init_early() && node.app_init_main(), "{:?}", stderr.lock().unwrap());
    let metaman = node.steps.metaman().expect("made at step 6");
    assert_eq!((metaman.snapshot(), answers(metaman, &pros)), before);
    node.interrupt();
    node.shutdown();
}

#[test]
fn a_masternodes_counters_survive_a_clean_restart() {
    counters_survive_a_clean_restart(true);
}

/// W1-r1 N1: a node's `dsq` intake counts through `MetaManMixing`, never a private
/// `MixingCounters`, so its `AllowMixing` persists.
#[test]
fn a_nodes_dsq_intake_survives_a_clean_restart() {
    counters_survive_a_clean_restart(false);
}

/// A Core-written `mncache.dat` (Core's own `CMasternodeMetaMan` and `CFlatDB`, through the
/// oracle; no dashd capture holds a nonzero `nLastDsq`), read in Core's layout and stored as the
/// node's: after a restart the node's counters answer `IsMixingThresholdExceeded` as Core's
/// values give. A has `nLastDsq` 1, B 2, `nDsqCount` 2 (`oracle-v24/PROVENANCE.md`).
// Core: masternode/meta.cpp:93-108
#[test]
fn a_core_written_mncache_answers_through_the_node() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../dashd-masternode/tests/fixtures/oracle-v24/populated.mncache.dat");
    let dir = tempfile::tempdir().unwrap();
    // A first start leaves a tip, so the next one loads the caches.
    let (mut node, _, _) = start(dir.path(), &[]);
    assert!(node.app_init_early() && node.app_init_main());
    node.shutdown();
    let ms = config(dir.path(), &[]).1.unwrap().chain.message_start;
    let core = MasternodeMetaMan::new();
    assert!(core.load_cache_core_layout(&fixture, ms, true));
    assert!(core.store(&dir.path().join("regtest").join(meta::FILE_NAME), ms));
    let (mut node, _, stderr) = start(dir.path(), &[]);
    assert!(node.app_init_early() && node.app_init_main(), "{:?}", stderr.lock().unwrap());
    let metaman = node.steps.metaman().expect("made at step 6");
    assert_eq!(metaman.snapshot(), core.snapshot());
    let (a, b) = (TxHash::from([0xa1; 32]), TxHash::from([0xb2; 32]));
    let expected: Vec<bool> =
        [10, 5].into_iter().flat_map(|from| (0..=30).map(move |n| n >= from)).collect();
    assert_eq!(answers(metaman, &[a, b]), expected);
    assert!(MetaManMixing(Arc::clone(metaman)).is_valid_for_mixing_txes(&a));
    node.shutdown();
}
