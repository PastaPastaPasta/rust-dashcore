//! GV1b-S5a: governance in the node, still off by default. The three load cases, the disabled
//! node, the shutdown store, the injected index and V, and the order of the binding, the
//! registrations and the init callback.
// Core: init.cpp v24.0.2:2149, :2242, :2260-2273; governance/governance.cpp v24.0.2:91-112

use dash_coreimport::netstate::governance as core_gov;
use dash_governance::manager::StoreState;
use dashd_chainstate::connect::ChainHelper as _;
use dashd_governance::store;
use dashd_governance::{GovExec as _, net};
use dashd_indexes::testing::MemTxIndex;
use dashd_masternode::flatdb::{self, FlatObject, ReadError};
use dashd_p2p::NetHandler as _;

use super::txindex::started;
use super::*;
use crate::governance::NodeGovernance;
use crate::governance::tests::Event;

const ON: &str = "-disablegovernance=0";

fn gov(node: &Node) -> &Arc<NodeGovernance> {
    node.steps.governance.as_ref().expect("made before the load")
}

fn dat(dir: &Path) -> PathBuf {
    dir.join("regtest").join(store::FILE)
}

fn message_start(node: &Node) -> [u8; 4] {
    node.params.chain.message_start
}

/// R9's `governance.dat` as Core stored it, imported.
fn r9_state() -> StoreState {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../dashd-governance/tests/fixtures/dashd-v24.0.0/R9.governance.dat");
    let bytes = std::fs::read(&path).unwrap();
    let loaded = core_gov::parse(&bytes, dash_network::Network::Regtest).expect("Core's file");
    dashd_governance::from_core(loaded.value).expect("R9 imports")
}

/// A node through init whose start failed, with its error lines.
fn refused(dir: &Path, argv: &[&str]) -> Vec<String> {
    let (mut node, _, stderr) = start(dir, argv);
    assert!(node.app_init_early());
    assert!(!node.app_init_main(), "the start is refused");
    node.shutdown();
    stderr.lock().unwrap().clone()
}

/// `-disablegovernance` (the default): the manager is unloaded and inert everywhere it is wired.
/// `GovernanceSync` is invalid, `NetGovernance` has every governance inventory, the helper's
/// `gov_valid` is false and G2 keeps refusing, and an existing `governance.dat` is never read or
/// written. The handler's messages return unread at `!IsValid()` (S3b's
/// `the_gate_leaves_malformed_messages_unread_while_disabled_or_unsynced`, over the same seam).
// Core: init.cpp v24.0.2:2260-2273; governance/net_governance.cpp v24.0.2:80-84, :273-282
#[test]
fn disabled_governance_is_inert_and_leaves_its_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("regtest")).unwrap();
    let junk = b"neither read nor written".to_vec();
    std::fs::write(dat(dir.path()), &junk).unwrap();
    for _ in 0..2 {
        let mut node = started(dir.path(), &[]);
        let g = Arc::clone(gov(&node));
        assert!(!g.enabled());
        assert!(!g.sync_seam().is_valid(), "GovernanceSync reports invalid");
        assert!(!g.ready());
        assert!(!g.handle().published().is_loaded);
        assert!(!g.helper().gov_valid().unwrap(), "the helper's gov_valid");
        let handler = node.net.as_ref().unwrap().governance_handler().expect("registered");
        assert!(handler.already_have(&net::object_inv([1; 32])));
        assert!(handler.already_have(&net::vote_inv([2; 32])));
        node.shutdown();
        assert_eq!(std::fs::read(dat(dir.path())).unwrap(), junk, "byte for byte");
    }
}

/// The scheduler holds the two governance jobs only while governance is valid.
// Core: governance/net_governance.cpp v24.0.2:39-78
#[test]
fn the_jobs_are_scheduled_only_while_governance_is_valid() {
    let count = |argv: &[&str]| {
        let dir = tempfile::tempdir().unwrap();
        let mut node = started(dir.path(), argv);
        let tasks = node.scheduler.as_ref().unwrap().queue_info().0;
        node.shutdown();
        tasks
    };
    assert_eq!(count(&[ON]), count(&[]) + 2, "maintenance and the relay flush");
}

/// Case 2, clear: a datadir with no tip, or `-reindex`, writes the empty store and is loaded
/// when the write succeeds; a write that fails leaves it unloaded and refuses the start.
// Core: init.cpp v24.0.2:2242, :2260-2266; governance/governance.cpp v24.0.2:102-110
#[test]
fn clearing_writes_the_empty_store_and_loads_on_its_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[ON]);
    assert!(gov(&node).handle().published().is_loaded, "a new datadir clears");
    let ms = message_start(&node);
    let state = store::decode(&std::fs::read(dat(dir.path())).unwrap(), ms).unwrap().state;
    assert!(state.objects.is_empty());
    node.shutdown();
    std::fs::write(dat(dir.path()), store::encode(r9_state(), ms)).unwrap();
    let mut node = started(dir.path(), &[ON, "-reindex"]);
    assert!(gov(&node).handle().published().is_loaded);
    let state = store::decode(&std::fs::read(dat(dir.path())).unwrap(), ms).unwrap().state;
    assert!(state.objects.is_empty(), "-reindex wrote the empty store");
    assert!(gov(&node).handle().stored_state().unwrap().objects.is_empty());
    node.shutdown();

    let fresh = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dat(fresh.path())).unwrap();
    let path = dat(fresh.path());
    assert_eq!(
        refused(fresh.path(), &[ON]),
        [format!("Error: Failed to clear governance cache at {}\n", path.display())]
    );
}

/// Case 1, load: a valid store is imported, cleaned and indexed at the active height, and a
/// clean shutdown stores it back; a recreating error gives the empty store; any other refuses
/// the start and leaves the file as it was.
// Core: init.cpp v24.0.2:2261-2264; governance/governance.cpp v24.0.2:99-112;
// flat-database.h v24.0.2:158-190
#[test]
fn loading_imports_a_valid_store_recreates_a_bad_body_and_refuses_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[ON]);
    let ms = message_start(&node);
    node.shutdown();
    let r9 = r9_state();
    let keys: Vec<_> = r9.objects.keys().copied().collect();
    assert!(!keys.is_empty());
    std::fs::write(dat(dir.path()), store::encode(r9, ms)).unwrap();
    let mut node = started(dir.path(), &[ON]);
    let g = Arc::clone(gov(&node));
    assert!(g.handle().published().is_loaded);
    assert!(g.events().contains(&Event::Loaded));
    let loaded = g.handle().stored_state().unwrap();
    assert_eq!(loaded.objects.keys().copied().collect::<Vec<_>>(), keys);
    node.shutdown();
    let stored = store::decode(&std::fs::read(dat(dir.path())).unwrap(), ms).unwrap().state;
    assert_eq!(stored.objects.keys().copied().collect::<Vec<_>>(), keys, "stored back");

    /// A well-formed envelope around a body that is not a store.
    #[derive(Default)]
    struct NotAStore;
    impl FlatObject for NotAStore {
        const FILE: &'static str = store::FILE;
        const MAGIC: &'static str = store::MAGIC;
        fn write(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(b"\x05other");
        }
        fn read(&mut self, _: &mut &[u8]) -> Result<(), ReadError> {
            unreachable!()
        }
        fn clear(&mut self) {}
        fn summary(&self) -> String {
            String::new()
        }
    }
    std::fs::write(dat(dir.path()), flatdb::encode(&NotAStore, ms)).unwrap();
    let mut node = started(dir.path(), &[ON]);
    assert!(gov(&node).handle().published().is_loaded, "recreated");
    assert!(gov(&node).handle().stored_state().unwrap().objects.is_empty());
    node.shutdown();

    std::fs::write(dat(dir.path()), b"cut").unwrap();
    let path = dat(dir.path());
    assert_eq!(
        refused(dir.path(), &[ON]),
        [format!("Error: Failed to load governance cache from {}\n", path.display())]
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"cut");
}

/// A clean shutdown stores a loaded store once the network and V have stopped; a killed node,
/// or one an assert stopped (DEC-1156), writes nothing.
// Core: init.cpp v24.0.2:414; governance/governance.cpp v24.0.2:91-97
#[test]
fn only_a_clean_shutdown_stores_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[ON]);
    std::fs::remove_file(dat(dir.path())).unwrap();
    node.shutdown();
    assert!(dat(dir.path()).exists(), "stored at shutdown");

    let mut node = started(dir.path(), &[ON]);
    std::fs::remove_file(dat(dir.path())).unwrap();
    node.steps.kill();
    node.shutdown();
    assert!(!dat(dir.path()).exists(), "a killed node stores nothing");

    let (mut node, _, _, _) = settled_with(dir.path(), &[ON]);
    assert!(gov(&node).handle().published().is_loaded, "a missing file loads empty");
    assert!(!dat(dir.path()).exists(), "and the load writes nothing");
    node.steps.chainstate().trip_latch(V_PANIC_SITE, "the chain actor panicked");
    node.abort.abort_node("V panicked: x", None);
    node.shutdown();
    assert!(!dat(dir.path()).exists(), "an assert stores nothing");
}

/// The processor's overlay reads governance's own readiness: false while blockchain sync is
/// pending, true once an enabled, loaded node is blockchain-synced, and false on a disabled one.
// Core: governance/net_governance.cpp v24.0.2:273-282; governance/governance.cpp v24.0.2:629
#[test]
fn the_overlay_reads_governance_ready_once_loaded_and_blockchain_synced() {
    let ready = |argv: &[&str]| {
        let dir = tempfile::tempdir().unwrap();
        let mut node = started(dir.path(), argv);
        let overlay = Arc::clone(node.net.as_ref().unwrap().overlay().expect("attached"));
        let before = overlay.governance_ready();
        gov(&node).sync().switch_to_next_asset();
        let after = overlay.governance_ready();
        node.shutdown();
        (before, after)
    };
    assert_eq!(ready(&[ON]), (false, true), "enabled: loaded, then blockchain-synced");
    assert_eq!(ready(&[]), (false, false), "disabled");
}

/// A shutdown store that fails to write logs `CoreWrite`'s error line and leaves no file.
// Core: flat-database.h v24.0.2:56, :65; governance/governance.cpp v24.0.2:91-97
#[test]
fn a_failed_shutdown_store_logs_core_write_s_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[ON]);
    std::fs::remove_file(dat(dir.path())).unwrap();
    std::fs::create_dir(dat(dir.path()).with_extension("dat.new")).unwrap();
    let ((), traced) = crate::test_log::logged(|| node.shutdown());
    let errors: Vec<_> = traced.iter().filter(|(l, _)| *l == tracing::Level::ERROR).collect();
    assert!(
        errors.iter().any(|(_, m)| m.starts_with("CoreWrite: Serialize or I/O error - ")),
        "the write's error is logged: {errors:?}"
    );
    assert!(!dat(dir.path()).exists());
}

/// V is bound before the listener registers, the listener registers after SP1's mnsync
/// listener and the DSTX listener, and the init callback's tip command runs after all three,
/// after the load: on a restart whose tip is recent, its effects reach the worker last.
// Core: init.cpp v24.0.2:2225-2228, :2260-2264, :2456-2470; dsnotificationinterface.cpp
// v24.0.2:31-35, :55-72
#[test]
fn v_binds_before_the_listener_which_follows_mnsync_and_precedes_the_init_tip() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[ON]);
    generate(&node, dir.path(), 1);
    node.shutdown();
    let mut node = started(dir.path(), &[ON]);
    let events = gov(&node).events();
    node.shutdown();
    assert_eq!(
        events,
        [
            Event::Bound,
            Event::Registered {
                after_sync: true,
                after_dstx: true,
            },
            Event::Effects,
            Event::Loaded,
            Event::Effects,
        ],
        "the load's clean, then the init callback's tip"
    );
}

/// The injected index and V: a collateral located inside a command on V through the node's
/// executor, over an index that still finds it after its block was invalidated, has one
/// confirmation at the tip and none once the block left the active chain.
// Core: governance/object.cpp v24.0.2:798-801, :855-860
#[test]
fn a_collateral_in_an_invalidated_block_has_no_confirmations() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[ON]);
    let mined = generate(&node, dir.path(), 2);
    let block: BlockHash = mined[1].parse().unwrap();
    let txid = dashd_indexes::Txid::from_hex(&"07".repeat(32)).unwrap();
    let tx = dash_primitives::Transaction::new(
        1,
        dash_primitives::TxType::new(0),
        vec![],
        vec![],
        0,
        vec![],
    );
    let mut index = MemTxIndex::default();
    index.entries.insert(txid, (Arc::new(tx), block));
    let index = Arc::new(index);
    let exec = gov(&node).exec().clone();
    let locate = move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let index = Arc::clone(&index);
        exec.post(Box::new(move |chain| {
            let found = dashd_governance::locate(&*index, chain, &txid);
            tx.send(found.map(|l| l.confirmations)).unwrap();
        }));
        rx.recv().expect("the command ran on V")
    };
    assert_eq!(locate(), Some(1));
    let v = node.steps.chain().unwrap().handle().clone();
    v.invalidate_block(block).recv().unwrap().unwrap();
    assert_eq!(locate(), Some(0), "found, off the active chain");
    node.shutdown();
}
