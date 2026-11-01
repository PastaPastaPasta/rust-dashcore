//! Every `sb4-core-scenarios` vector is replayed or listed with its reason (SB4 §15; review mi-1).
//!
//! A replay is declared with `scenario_test!`, which makes a plain `#[test]` (the macro takes no
//! other attribute, so it cannot be ignored or compiled out on its own) and its registration,
//! [`Replays`]: the vectors it replays. [`REPLAYED`] lists every registration by path; only the
//! macro builds one (the coverage test checks the source), so a registered test exists and is a
//! `scenario_test!`. While the test runs, [`load`](super::load) refuses a vector its
//! registration does not name, and a load outside a registered test; when it ends, every vector
//! it names must have been loaded. The coverage test checks that the vectors on disk are exactly
//! the registered ones and [`NOT_REPLAYED`], which are disjoint. [`OWED`] names each op a
//! replay injects from Core's view instead of replaying, with its owner; a replay refuses any
//! other op it does not handle, and an `OWED` row its vector never uses.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// A replay test's registration (`scenario_test!`).
pub(super) struct Replays {
    /// The registration's module path, `dash_chain::chain::scenario_tests::…::test`.
    path: &'static str,
    /// The vectors the test replays.
    vectors: fn() -> Vec<&'static str>,
}

/// Every replay test.
const REPLAYED: &[&Replays] = &[
    &super::replay_a1a_pnbh_rules_genesis::REPLAYS,
    &super::replay_a1a_devnet::REPLAYS,
    &super::replay_a1a_testnet_min_difficulty::REPLAYS,
    &super::replay_a1a_mainnet_stale_branch_chainlock::REPLAYS,
    &super::replay_a18_invalid_chain_found::REPLAYS,
    &super::replay_a18_invalidate_walkback::REPLAYS,
    &super::replay_a19_first_seen_tie::REPLAYS,
    &super::replay_hx1_bad_chainlock_then_connect::REPLAYS,
    &super::replay_hx1_dump_orders::REPLAYS,
    &super::replay_hx1_failed_walk_early_reject::REPLAYS,
    &super::replay_hx1_precious::REPLAYS,
    &super::replay_hx1_propose_check::REPLAYS,
    &super::replay_a38_deep_fork_time_too_old::REPLAYS,
    &super::replay_a38_deep_fork_time_too_old_spv::REPLAYS,
    &super::replay_smoke_reload::REPLAYS,
    &super::replay_a24_checkpoint::REPLAYS,
    &super::replay_a28_pnbh_index_state::REPLAYS,
    &super::replay_a2_pnbh_rules_anchored::REPLAYS,
    &super::replay_smoke_pnbh::REPLAYS,
    &super::replay_pnbh_mainnet_dgw::REPLAYS,
    &super::replay_pnbh_testnet::REPLAYS,
    &super::replay_pnbh_mainnet_kgw::REPLAYS,
    &super::locator_heights_match_core_on_mainnet::REPLAYS,
    &super::record_codec_matches_core_disk_index::REPLAYS,
    &super::cl::replay_cl_vectors::REPLAYS,
];

/// The vectors no test replays, with why and who owns them.
const NOT_REPLAYED: &[(&str, &str)] = &[
    (
        "hx1-enforce-fixed-point",
        "CH1-node (HX1 X5, DEC-167): no header step; the fixed point turns on invalidateblock and \
         ActivateBestChain's FAILED_CHILD, block validation through V. SB4's half of DEC-167 is the cl \
         vectors' fixed point (a21b, a29)",
    ),
    (
        "hx1-pnb-force",
        "CH1-node (HX1 X2, X5): no header step; every op is ProcessNewBlock's AcceptBlock \
         (force_processing, BIP34 at ContextualCheckBlock), block acceptance through V",
    ),
    (
        "hx1-reconsider-ignore-cl",
        "CH1-node (HX1 X2, X5): no header step; reconsiderblock(ignore_chainlocks) and a CLSIG \
         whose signature fails verification are the node's RPC and LK1a intake through V",
    ),
    (
        "bench-pnbh-mainnet-dgw",
        "A-25's Core side: instructions:u of one ProcessNewBlockHeaders, a PF0 perfgate input \
         (contrib/node/perfgate/workloads.toml), not a verdict; its batch's verdict is \
         pnbh-mainnet-dgw",
    ),
    (
        "bench-pnbh-mainnet-kgw",
        "A-25's Core side: instructions:u of one ProcessNewBlockHeaders, a PF0 perfgate input, \
         not a verdict; its batch's verdict is pnbh-mainnet-kgw",
    ),
];

/// The ops a replay injects from Core's views instead of replaying, as (vector, op, owner).
/// [`super::replay`] refuses an unhandled op not listed here, and a row its vector never uses.
const OWED: &[(&str, &str, &str)] = &[
    (
        "a1a-pnbh-rules-genesis",
        "timer",
        "DEC-167's enforcement pass, replayed strictly on the cl vectors",
    ),
    ("a1a-pnbh-rules-genesis", "cl", "a received lock, replayed strictly on the cl vectors"),
    (
        "a1a-mainnet-stale-branch-chainlock",
        "seed-cl",
        "a seeded lock, replayed strictly on the cl vectors",
    ),
    (
        "a18-invalid-chain-found",
        "connect",
        "CH1-node (D-C6-7): the connect that finds C7 invalid (InvalidChainFound) is the node's \
         block validation",
    ),
    ("a18-invalidate-walkback", "invalidate", INVALIDATE),
    ("a18-invalidate-walkback", "reconsider", INVALIDATE),
    ("a28-pnbh-index-state", "invalidate", INVALIDATE),
    ("a28-pnbh-index-state", "reconsider", INVALIDATE),
    ("a28-pnbh-index-state", "timer", "DEC-167's enforcement pass, replayed on the cl vectors"),
    ("a28-pnbh-index-state", "cl", "a received lock, replayed strictly on the cl vectors"),
    ("hx1-bad-chainlock-then-connect", "timer", HX1_CL),
    ("hx1-bad-chainlock-then-connect", "cl", HX1_CL),
    ("hx1-bad-chainlock-then-connect", "connect", HX1_CL),
    (
        "hx1-dump-orders",
        "connect",
        "CH1-node (HX1 X3, X5): ProcessNewBlock's unlinked-block orders are block data",
    ),
    ("hx1-failed-walk-early-reject", "invalidate", INVALIDATE),
    ("hx1-precious", "precious", "CH1-node (HX1 X2, X5): preciousblock is the node's RPC"),
    ("hx1-propose-check", "propose", HX1_PROPOSE),
    ("hx1-propose-check", "timer", HX1_PROPOSE),
    ("hx1-propose-check", "cl", HX1_PROPOSE),
];

/// HX1-X1's lock and the connects it judges (bad-chainlock, then the reset).
const HX1_CL: &str = "the lock's enforcement over header-only A100 is replayed strictly on the cl \
                      vectors (a26-i); the ConnectTip it fails (bad-chainlock) and the reset run \
                      through V (CH1-node, HX1 X1, X5)";

/// HX1-X1's proposals and the lock on header-only F11 they are judged against.
const HX1_PROPOSE: &str = "the lock's enforcement over header-only F11 is replayed strictly on the \
                           cl vectors (a26-i); the getblocktemplate proposals judged against it \
                           (TestBlockValidity) run through V (CH1-node, HX1 X1, X5)";

/// The owner of the node's block-data RPCs.
const INVALIDATE: &str =
    "CH1-node (D-C6-7): invalidateblock/reconsiderblock are the node's RPCs over block data";

/// The ops [`OWED`] lists for `vector`.
pub(super) fn owed(vector: &str) -> BTreeSet<&'static str> {
    OWED.iter().filter(|&&(v, ..)| v == vector).map(|&(_, op, _)| op).collect()
}

thread_local! {
    /// The running replay test's registration and the vectors it has loaded.
    static RUNNING: RefCell<Option<(&'static Replays, BTreeSet<&'static str>)>> =
        const { RefCell::new(None) };
}

impl Replays {
    /// A registration; `scenario_test!` builds the only ones.
    pub(super) const fn new(path: &'static str, vectors: fn() -> Vec<&'static str>) -> Replays {
        Replays {
            path,
            vectors,
        }
    }

    /// The test's libtest name: the path without the crate.
    fn test(&self) -> &'static str {
        self.path.split_once("::").expect("a crate path").1
    }

    /// Starts the registered test running on this thread.
    pub(super) fn begin(&'static self) {
        assert!(
            REPLAYED.iter().any(|r| r.path == self.path),
            "{}: list it in REPLAYED (coverage.rs)",
            self.test()
        );
        // libtest names each test's thread after the test (and runs it on its own thread).
        let thread = std::thread::current();
        assert_eq!(thread.name(), Some(self.test()), "a replay runs on its libtest thread");
        RUNNING.set(Some((self, BTreeSet::new())));
    }

    /// Ends the test: it loaded every vector it replays.
    pub(super) fn end(&'static self) {
        let (running, loaded) = RUNNING.take().expect("begun");
        assert_eq!(running.path, self.path, "the running test");
        let named: BTreeSet<&str> = (self.vectors)().into_iter().collect();
        let unloaded: Vec<&&str> = named.difference(&loaded).collect();
        assert!(unloaded.is_empty(), "{}: never loaded {unloaded:?}", self.test());
    }
}

/// Asserts that the running test is a registered replay of `name`, and records the load.
pub(super) fn check_load(name: &str) {
    RUNNING.with_borrow_mut(|running| {
        let Some((test, loaded)) = running else {
            panic!("{name}: only a scenario_test! loads a vector (coverage.rs)");
        };
        let Some(&vector) = (test.vectors)().iter().find(|&&v| v == name) else {
            panic!("{} loads {name}: add it to its #[replays(..)]", test.test());
        };
        loaded.insert(vector);
    });
}

/// A vector file's extension: gzipped JSON, as `regen.py` writes it.
pub(super) const EXT: &str = ".json.gz";

/// The vectors' directory: `dash-chain/tests/data/core-scenarios`, or `SB4_CORE_SCENARIOS_DIR`.
pub(super) fn dir() -> PathBuf {
    std::env::var_os("SB4_CORE_SCENARIOS_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/core-scenarios"),
        PathBuf::from,
    )
}

#[test]
fn every_core_scenario_is_replayed_or_listed() {
    let files: Vec<String> = std::fs::read_dir(dir())
        .expect("the vectors' directory")
        .map(|e| e.expect("an entry").file_name().to_string_lossy().into_owned())
        .collect();
    // A stray uncompressed vector (an old regen or directory) would otherwise go unseen.
    let plain: Vec<&String> = files.iter().filter(|f| f.ends_with(".json")).collect();
    assert!(plain.is_empty(), "vectors are {EXT}: {plain:?}");
    let on_disk: BTreeSet<String> =
        files.iter().filter_map(|f| f.strip_suffix(EXT).map(str::to_owned)).collect();
    let tests: BTreeSet<&str> = REPLAYED.iter().map(|r| r.path).collect();
    assert_eq!(tests.len(), REPLAYED.len(), "REPLAYED lists a test twice");
    let mut replayed = BTreeSet::new();
    for r in REPLAYED {
        let vectors = (r.vectors)();
        assert!(!vectors.is_empty(), "{}: replays nothing", r.test());
        let distinct: BTreeSet<&str> = vectors.iter().copied().collect();
        assert_eq!(distinct.len(), vectors.len(), "{}: names a vector twice", r.test());
        replayed.extend(vectors.into_iter().map(str::to_owned));
    }
    let listed: BTreeSet<String> = NOT_REPLAYED.iter().map(|(v, _)| (*v).to_owned()).collect();
    assert_eq!(listed.len(), NOT_REPLAYED.len(), "NOT_REPLAYED lists a vector twice");
    for (vector, reason) in NOT_REPLAYED {
        assert!(!reason.trim().is_empty(), "{vector}: NOT_REPLAYED needs a reason");
    }
    let both: Vec<&String> = replayed.intersection(&listed).collect();
    assert!(both.is_empty(), "both replayed and not replayed: {both:?}");
    let registered: BTreeSet<String> = replayed.union(&listed).cloned().collect();
    let unregistered: Vec<&String> = on_disk.difference(&registered).collect();
    assert!(unregistered.is_empty(), "replay these or list them in NOT_REPLAYED: {unregistered:?}");
    let missing: Vec<&String> = registered.difference(&on_disk).collect();
    assert!(missing.is_empty(), "registered but not on disk: {missing:?}");
    let rows: BTreeSet<(&str, &str)> = OWED.iter().map(|&(v, op, _)| (v, op)).collect();
    assert_eq!(rows.len(), OWED.len(), "OWED lists an op twice");
    for (vector, op, owner) in OWED {
        assert!(replayed.contains(*vector), "{vector}: OWED lists replayed vectors only");
        assert!(!owner.trim().is_empty(), "{vector} {op}: OWED needs an owner");
    }
    // Only `scenario_test!` builds a registration, so each is a plain `#[test]` with its checks.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/chain");
    let files = std::fs::read_dir(src.join("scenario_tests"))
        .expect("scenario_tests/")
        .map(|e| e.expect("an entry").path())
        .chain([src.join("scenario_tests.rs")]);
    let call = concat!("Replays::", "new("); // split, so this line is not a call
    let built: usize =
        files.map(|f| std::fs::read_to_string(f).expect("a source").matches(call).count()).sum();
    assert_eq!(built, 1, "a registration built outside scenario_test!");
}
