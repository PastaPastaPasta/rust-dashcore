//! The lane (c) differential (plan §4, QM1a-3): Core v24.0.0's recorded regtest run
//! (`test-vectors/lane-c`) replayed into a real V (`dashd_node::sut::NodeSut`), its quorums read
//! through route B's binding, and each node's contributions built by [`QuorumManager`] with `C`
//! fed from the captured `qcontrib` messages and `M` that node. Every recorded `quorum list`
//! (default and `count`) and `quorum info` (both `includeSkShare` flags, every node) must equal
//! the answer Core's RPC builds from the same reads (`rpc/quorums.cpp:50-190`, `:192-252`), field
//! by field. A recorded quorum, node or call that is missing fails the test.
//!
//! The signing quorum lookup over the same route B ([`SigningQuorums`]) must answer every recorded
//! `quorum selectquorum` and the recorded `IsQuorumActive` boundary, and a signing runtime over it
//! signs in Core's selected quorum and verifies Core's recovered signature. Each node's recorded
//! log, replayed through its own runtime over a RocksDB recovered-signature database, gives
//! Core's writes, signs, reads, cleanups and subscriber relay.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::SeqCst;

use dash_clock::MockClock;
use dash_consensus::Block;
use dash_consensus::primitives::decode_block;
use dash_dml::StoreError;
use dash_evo::locks::wire::RecoveredSig;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::{BlsCtx, BlsSkValue, decrypt_share};
use dash_llmq::dkg::wire::Contribution;
use dash_llmq::quorum::{CleanupCounts, QuorumDbKey, StoredContributions};
use dash_llmq::sigenv::{
    QuorumLookup as _, SignRequest, SigningQuorum as _, select_for_signing_default,
};
use dash_llmq::signing::api::{QuorumSigning as _, RecoveredSigIntake as _};
use dash_llmq::signing::db::{RecSigBatch, RecSigKv, RecSigOp, RecSigScan};
use dash_llmq::signing::intake::IntakeStatus;
use dash_llmq::signing::manager::SigningManager;
use dash_params::ChainSetup;
use dash_params::LlmqType;
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric as _;
use dashd_chainstate::chain::ChainHandle;
use dashd_chainstate::llmq::quorum::{NodeQuorum, QuorumBuildHook, SharedQuorumHook};
use dashd_quorums::LocalMasternode;
use dashd_quorums::signing::{
    QuorumHandle, RecSigNotify, SignQueue, SigningHandles, SigningNet, SigningQuorums,
    SigningRuntime,
};
use dashd_quorums::traits::Contributions;
use dashd_quorums::{
    ChainQuorums, KvRecSigStore, MemKeyStore, NoDkgSessions, NotMasternode, QuorumKeyStore,
    QuorumLookup, QuorumManager, RECSIGDB_SCHEMA, VerifiedContributions, WarmExecutor,
};
use dashd_store::{RocksConfig, RocksEngine};
use serde_json::{Value, json};
use support::{Inline, Local, MemStore, PhaseReads, SCHEME, fresh, hex, info_json, read_phase};

fn lane_c() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-vectors/lane-c")
}

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

/// Each fixture data file checked in place against the sha256 that `PROVENANCE.md`'s table
/// records for it.
fn check_provenance() {
    let provenance = std::fs::read_to_string(lane_c().join("PROVENANCE.md")).expect("PROVENANCE");
    for name in ["chain-v24.0.0.hex", "regtest-v24.0.0.json"] {
        let row = provenance
            .lines()
            .find(|l| l.starts_with(&format!("| `test-vectors/lane-c/{name}` |")))
            .unwrap_or_else(|| panic!("PROVENANCE.md records no sha256 for {name}"));
        let recorded = row.split('`').nth(3).expect("the sha256 cell");
        let bytes = std::fs::read(lane_c().join(name)).expect("the fixture file");
        assert_eq!(hex(&dash_sha256::sha256(&bytes)), recorded, "{name}'s sha256");
    }
}

/// The recorded chain, genesis first.
fn chain() -> Vec<Arc<Block>> {
    let text = std::fs::read_to_string(lane_c().join("chain-v24.0.0.hex")).expect("the chain");
    text.lines()
        .map(|line| {
            let bytes = unhex(line);
            let mut rest = bytes.as_slice();
            let block = decode_block(&mut rest).expect("a block").into_value();
            assert!(rest.is_empty(), "trailing bytes after a block");
            Arc::new(block)
        })
        .collect()
}

/// One recorded quorum's captured contributions: its members (route B's) and each member's
/// `qcontrib` payload.
struct Captured {
    members: Vec<ProTxHash>,
    payloads: BTreeMap<String, Vec<u8>>,
    params: dash_params::LlmqParams,
}

/// `GetVerifiedContributions` (v24.0.0 `llmq/dkgsessionmgr.cpp:145-192`) as node `M`'s DKG
/// database holds it after the recorded run: each valid member's vvec from its captured
/// `qcontrib`, and `M`'s secret contribution from it decrypted with `M`'s operator key. A node
/// that is no member of the quorum stored no vvec, which Core answers `false`.
struct FixtureContributions {
    me: Option<(ProTxHash, BlsSkValue)>,
    quorums: Arc<BTreeMap<(u8, BlockHash), Captured>>,
}

impl VerifiedContributions for FixtureContributions {
    fn verified(
        &self,
        llmq_type: LlmqType,
        base: &BlockHash,
        _: &[ProTxHash],
        valid: &[bool],
    ) -> Option<Contributions> {
        let captured = self
            .quorums
            .get(&(llmq_type.to_base(), *base))
            .unwrap_or_else(|| panic!("a build of an unrecorded quorum {llmq_type:?} {base}"));
        let (me, sk) = self.me.as_ref()?;
        let my_index = captured.members.iter().position(|m| m == me)?;
        let (mut indexes, mut vvecs, mut sks) = (Vec::new(), Vec::new(), Vec::new());
        for (i, member) in captured.members.iter().enumerate() {
            if !valid[i] {
                continue;
            }
            let payload = captured
                .payloads
                .get(&member.to_string())
                .unwrap_or_else(|| panic!("no captured qcontrib from valid member {member}"));
            let qc =
                Contribution::read(payload, &captured.params, BlsCtx::BASIC).expect("qcontrib");
            assert_eq!(qc.pro_tx_hash(), member, "the qcontrib's sender");
            indexes.push(u16::try_from(i).expect("small"));
            vvecs.push(Arc::new(qc.vvec().to_vec()));
            // Core reads an unwritten skContribution as the default (invalid) key (:181).
            sks.push(decrypt_share(qc.blobs(), my_index, sk).unwrap_or_else(BlsSkValue::invalid));
        }
        Some((indexes, vvecs, sks))
    }
}

fn as_u8(v: &Value) -> u8 {
    u8::try_from(v.as_u64().expect("a number")).expect("a type")
}

// --- The replay ---

/// The fixture, its chain, and a SUT started from genesis with node0's arguments, fed the
/// recorded blocks through `fed`.
struct Lane {
    fixture: Value,
    chain: Vec<Arc<Block>>,
    params: Arc<ChainSetup>,
    v: ChainHandle,
    fed: usize,
    _sut: dashd_node::sut::NodeSut,
    _dir: tempfile::TempDir,
}

impl Lane {
    fn start() -> Lane {
        Lane::start_with(dashd_node::sut::Harness::default())
    }

    /// [`start`](Self::start) with `harness` for every open.
    fn start_with(harness: dashd_node::sut::Harness) -> Lane {
        check_provenance();
        let fixture: Value = serde_json::from_slice(
            &std::fs::read(lane_c().join("regtest-v24.0.0.json")).expect("the fixture"),
        )
        .expect("json");
        assert_eq!(fixture["schema_version"], json!(2));
        assert_eq!(fixture["bls_scheme"], json!("basic"));
        let chain = chain();
        let tip_height =
            usize::try_from(fixture["tip_height"].as_u64().expect("tip_height")).unwrap();
        assert_eq!(chain.len(), tip_height + 1, "the chain holds genesis through the tip");

        // The DIP3 arguments DashTestFramework writes to every node's dash.conf, outside
        // `extra_args` (v24.0.0 test/functional/test_framework/test_framework.py:1530-1535),
        // checked against the activation heights the fixture records.
        let buried = &fixture["provenance"]["deployments"]["deployments"];
        for (name, height) in [("dip0003", 2), ("v20", 100), ("mn_rr", 100)] {
            assert_eq!(buried[name]["height"], json!(height), "the recorded {name} height");
        }
        let dip3_config =
            ["-dip3params=2:2", "-testactivationheight=v20@100", "-testactivationheight=mn_rr@100"];
        // Then node0's (the controller's) `extra_args`, without the two that are no chain options.
        let args = fixture["provenance"]["extra_args"][0].as_array().expect("node0's arguments");
        let mut argv = vec!["-regtest", "-printtoconsole=0"];
        argv.extend(dip3_config);
        for arg in args {
            let arg = arg.as_str().expect("an argument");
            if arg != "-capturemessages" && !arg.starts_with("-watchquorums") {
                argv.push(arg);
            }
        }
        let now = chain.iter().map(|b| i64::from(b.header().time())).max().expect("blocks") + 60;
        let dir = tempfile::tempdir().expect("a temp dir");
        let ui = Arc::new(dashd_node::ui::Ui::with_stderr(
            Arc::new(dashd_node::logging::Logger::new()),
            Box::new(|_| {}),
        ));
        let sut = dashd_node::sut::NodeSut::with_harness(dir.path(), &argv, now, ui, harness)
            .expect("the SUT starts");
        let params = Arc::new(sut.params().clone());
        let v = sut.chain().expect("V runs").clone();
        let genesis = chain[0].hash();
        let has_genesis = v.on_v(move |_, q| q.chain.reader().lookup(&genesis).is_some());
        assert!(has_genesis.recv().expect("V serves"), "the fixture's genesis is V's");
        Lane {
            fixture,
            chain,
            params,
            v,
            fed: 0,
            _sut: sut,
            _dir: dir,
        }
    }

    fn tip_height(&self) -> usize {
        self.chain.len() - 1
    }

    /// V fed the recorded blocks through `height`.
    fn feed(&mut self, height: usize) {
        while self.fed < height {
            self.fed += 1;
            let block = self.chain[self.fed].clone();
            let r = self.v.process_block(block, true, None, true).recv().expect("V");
            assert!(r.accepted && r.state.is_valid(), "V at {}: {r:?}", self.fed);
        }
    }

    /// Every recorded node, with its proTxHash and operator secret if it is a masternode.
    fn nodes(&self) -> Vec<(String, Option<(ProTxHash, BlsSkValue)>)> {
        let signers = self.fixture["signers"].as_object().expect("signers");
        let operators = self.fixture["operators"].as_object().expect("operators");
        let mut nodes = Vec::new();
        for (node, signer) in signers {
            let me = signer["proTxHash"].as_str().map(|pro| {
                let sk = operators
                    .get(pro)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("node {node}: no operator secret"));
                let sk: [u8; 32] = unhex(sk).try_into().expect("32 bytes");
                let sk = BlsSkValue::from_canonical(&sk, SCHEME).expect("an operator secret");
                let pro_tx_hash = pro_tx_hash(pro);
                assert_eq!(pro_tx_hash.to_string(), pro, "display order");
                (pro_tx_hash, sk)
            });
            nodes.push((node.clone(), me));
        }
        nodes
    }

    /// The recorded phases by height.
    fn phases(&self) -> Vec<(String, usize)> {
        let lists = self.fixture["quorum_list"].as_array().expect("quorum_list");
        let mut phases: Vec<(String, usize)> = lists
            .iter()
            .map(|l| {
                let height = usize::try_from(l["height"].as_u64().expect("height")).unwrap();
                (l["phase"].as_str().expect("phase").to_owned(), height)
            })
            .collect();
        phases.sort_by_key(|(_, h)| *h);
        phases
    }

    /// The phase's recorded quorums.
    fn quorums(&self, phase: &str) -> &[Value] {
        self.fixture["phases"][phase]["quorums"].as_array().expect("the phase's quorums")
    }

    /// The phase's recorded quorums' types and hashes.
    fn recorded(&self, phase: &str) -> Vec<(LlmqType, QuorumHash)> {
        self.quorums(phase)
            .iter()
            .map(|q| {
                let t = LlmqType::from_base(as_u8(&q["type"]));
                (t, QuorumHash::from_hex(q["quorumHash"].as_str().expect("hash")).expect("hex"))
            })
            .collect()
    }

    /// Route B's reads at each phase's recorded tip in turn, with the phase's recorded quorums.
    fn replay_phases(&mut self) -> Vec<(String, Vec<Value>, PhaseReads)> {
        let mut out = Vec::new();
        for (phase, height) in self.phases() {
            self.feed(height);
            let recorded = self.recorded(&phase);
            let reads = read_phase(&self.v, &self.params, self.chain[height].hash(), recorded);
            out.push((phase.clone(), self.quorums(&phase).to_vec(), reads));
        }
        out
    }

    /// Installs `hook` on route B's facade, which has none yet.
    fn install(&self, hook: SharedQuorumHook) {
        let installed = self.v.on_v(move |o, _| o.install_quorum_hook(hook));
        assert!(installed.recv().expect("V serves"), "the facade had no hook");
    }
}

/// A proTxHash from its display hex.
fn pro_tx_hash(display: &str) -> ProTxHash {
    let mut bytes: [u8; 32] = unhex(display).try_into().expect("32 bytes");
    bytes.reverse();
    ProTxHash::from(bytes)
}

/// Every recorded quorum's captured `qcontrib` payloads, with its members in the order its
/// recorded `quorum info` lists them, keyed by type and base block.
fn captured_from_fixture(lane: &Lane) -> BTreeMap<(u8, BlockHash), Captured> {
    let mut captured = BTreeMap::new();
    for (phase, _) in lane.phases() {
        for q in lane.quorums(&phase) {
            let llmq_type = LlmqType::from_base(as_u8(&q["type"]));
            let hash = QuorumHash::from_hex(q["quorumHash"].as_str().expect("hash")).expect("hex");
            let variant = &q["quorum_info"]["variants"][0];
            let members = variant["members"].as_array().expect("members");
            let members = members
                .iter()
                .map(|m| pro_tx_hash(m["proTxHash"].as_str().expect("pro")))
                .collect();
            let payloads = q["qcontrib"]
                .as_object()
                .expect("qcontrib")
                .iter()
                .map(|(pro, raw)| (pro.clone(), unhex(raw.as_str().expect("hex"))))
                .collect();
            let params = *lane.params.consensus.llmq(llmq_type).expect("configured");
            let base = BlockHash::from(*hash.as_bytes());
            captured.insert(
                (llmq_type.to_base(), base),
                Captured {
                    members,
                    payloads,
                    params,
                },
            );
        }
    }
    captured
}

/// [`FixtureContributions`] for the recorded quorums; any other quorum the replay builds finds
/// none, as Core's `GetVerifiedContributions` without the session's data.
struct Recorded(FixtureContributions);

impl VerifiedContributions for Recorded {
    fn verified(
        &self,
        llmq_type: LlmqType,
        base: &BlockHash,
        members: &[ProTxHash],
        valid: &[bool],
    ) -> Option<Contributions> {
        if !self.0.quorums.contains_key(&(llmq_type.to_base(), *base)) {
            return None;
        }
        self.0.verified(llmq_type, base, members, valid)
    }
}

/// A store behind an `Arc`, counting its reads and writes, that a test keeps once the manager
/// owns it.
struct Counted<S> {
    store: S,
    reads: AtomicUsize,
    writes: AtomicUsize,
}

struct Shared<S>(Arc<Counted<S>>);

impl<S> Clone for Shared<S> {
    fn clone(&self) -> Self {
        Shared(self.0.clone())
    }
}

impl<S> Shared<S> {
    fn new(store: S) -> Self {
        Shared(Arc::new(Counted {
            store,
            reads: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
        }))
    }

    fn counts(&self) -> (usize, usize) {
        (self.0.reads.load(SeqCst), self.0.writes.load(SeqCst))
    }
}

impl<S: QuorumKeyStore> QuorumKeyStore for Shared<S> {
    type Error = S::Error;

    fn read(&self, key: &QuorumDbKey) -> Result<Option<StoredContributions>, S::Error> {
        self.0.reads.fetch_add(1, SeqCst);
        self.0.store.read(key)
    }

    fn write(&self, key: &QuorumDbKey, row: &StoredContributions) -> Result<(), S::Error> {
        self.0.writes.fetch_add(1, SeqCst);
        self.0.store.write(key, row)
    }

    fn erase_except(
        &self,
        keep: &std::collections::BTreeSet<QuorumDbKey>,
    ) -> Result<CleanupCounts, S::Error> {
        self.0.store.erase_except(keep)
    }
}

/// An inline executor counting its jobs.
#[derive(Default)]
struct Jobs(AtomicUsize);

impl WarmExecutor for Jobs {
    fn spawn_warm(&self, job: Box<dyn FnOnce() + Send>) {
        self.0.fetch_add(1, SeqCst);
        job();
    }
}

/// Whether route B's object holds contributions: a vvec, or a secret key share.
fn has_contributions(quorum: &NodeQuorum) -> bool {
    let keys = quorum.keys.lock().unwrap();
    keys.has_vvec() || keys.sk_share().is_some()
}

/// Every recorded `quorum list` and `quorum info` of both phases against the manager's answer at
/// the recorded tip.
#[test]
fn recorded_quorum_list_and_info_equal_the_managers_answers() {
    let mut lane = Lane::start();
    let params = lane.params.clone();
    let nodes = lane.nodes();
    assert_eq!(nodes.len(), 12, "twelve recorded nodes");
    assert_eq!(nodes.iter().filter(|(_, me)| me.is_some()).count(), 11, "eleven masternodes");

    let phases = lane.phases();
    assert_eq!(phases.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["ring", "mesh"]);

    let mut compared = (0usize, 0usize, 0usize);
    for (phase, height) in &phases {
        lane.feed(*height);
        let lists = lane.fixture["quorum_list"].as_array().expect("quorum_list");
        let recorded_list =
            lists.iter().find(|l| l["phase"] == json!(phase)).expect("the phase's quorum list");
        let quorums = lane.quorums(phase);
        assert!(!quorums.is_empty(), "{phase}: no recorded quorums");
        let recorded = lane.recorded(phase);
        let reads = read_phase(&lane.v, &params, lane.chain[*height].hash(), recorded);

        for label in ["default", "count_4"] {
            let theirs = recorded_list.get(label).unwrap_or_else(|| panic!("{phase}: no {label}"));
            assert_eq!(&reads.lists[label], theirs, "{phase} quorum list {label} at {height}");
            compared.0 += 1;
        }

        let mut captured = BTreeMap::new();
        for (q, route) in quorums.iter().zip(reads.found()) {
            let payloads = q["qcontrib"]
                .as_object()
                .expect("qcontrib")
                .iter()
                .map(|(pro, raw)| (pro.clone(), unhex(raw.as_str().expect("hex"))))
                .collect();
            let llmq_type = route.quorum.llmq_type;
            captured.insert(
                (llmq_type.to_base(), route.quorum.base_hash),
                Captured {
                    members: route.quorum.members.iter().map(|m| m.pro_tx_hash).collect(),
                    payloads,
                    params: *params.consensus.llmq(llmq_type).expect("configured"),
                },
            );
        }
        let captured = Arc::new(captured);

        for (node, me) in &nodes {
            let manager = QuorumManager::new(
                params.clone(),
                MemStore::default(),
                FixtureContributions {
                    me: me.clone(),
                    quorums: Arc::clone(&captured),
                },
                Local(me.as_ref().map(|(p, _)| *p)),
                Inline,
            );
            for (q, route) in quorums.iter().zip(reads.found()) {
                let ours = fresh(&route.quorum);
                manager.built(&ours, true, SCHEME).expect("the manager's build");
                let info = &q["quorum_info"];
                let entry = info["nodes"].get(node).unwrap_or_else(|| {
                    panic!("{phase} {}: node {node} has no quorum info", q["quorumHash"])
                });
                let variant = usize::try_from(entry["variant"].as_u64().expect("variant")).unwrap();
                let theirs = info["variants"].get(variant).expect("the node's variant").clone();
                let llmq = params.consensus.llmq(ours.llmq_type).expect("configured");
                let without = info_json(route, &ours, llmq, false);
                assert_eq!(
                    without, theirs,
                    "{phase} node {node} quorum info {} false",
                    q["quorumHash"]
                );
                let mut with = theirs;
                if let Some(sk) = entry.get("secretKeyShare") {
                    with.as_object_mut().unwrap().insert("secretKeyShare".into(), sk.clone());
                }
                assert_eq!(
                    info_json(route, &ours, llmq, true),
                    with,
                    "{phase} node {node} quorum info {} true",
                    q["quorumHash"]
                );
                compared.1 += 2;
            }
            compared.2 += 1;
        }
        assert_eq!(
            quorums
                .iter()
                .map(|q| q["quorum_info"]["nodes"].as_object().map_or(0, |n| n.len()))
                .min(),
            Some(nodes.len()),
            "{phase}: a quorum records a node the comparison has not"
        );
    }
    lane.feed(lane.tip_height());
    eprintln!(
        "compared {} quorum list calls and {} quorum info calls over {} node runs",
        compared.0, compared.1, compared.2
    );
    assert_eq!(compared, (4, 10 * 12 * 2, 2 * 12));
}

/// QM1a-4a: a member's manager installed on V's facade before the replay (Core constructs its
/// `CQuorumManager` before the chain loads, v24.0.0 src/node/chainstate.cpp:157-161) is the hook
/// route B builds every quorum through (`llmq/quorumsman.cpp:92-107`). Each recorded quorum's
/// route-B object then answers that member's recorded `quorum info`, with and without
/// `includeSkShare`, from the contributions the build made and wrote; `ChainQuorums`, bound to
/// the same V, looks up that very object. At the tip, a cleanup height (634 = 576 + 58), the
/// manager's cleanup over the bound `ChainQuorums` erases a planted stale row and keeps rows
/// the scans key (`llmq/net_quorum.cpp:711-764`).
#[test]
fn route_b_with_a_members_manager_builds_its_recorded_contributions() {
    let mut lane = Lane::start();
    let captured = Arc::new(captured_from_fixture(&lane));
    let shares = |node: &str| {
        let phases = lane.phases();
        let quorums = phases.iter().flat_map(|(p, _)| lane.quorums(p));
        quorums.filter(|q| q["quorum_info"]["nodes"][node].get("secretKeyShare").is_some()).count()
    };
    let (node, me) = lane
        .nodes()
        .into_iter()
        .filter_map(|(node, me)| Some((node, me?)))
        .max_by_key(|(node, _)| shares(node))
        .expect("a masternode");
    let recorded_shares = shares(&node);
    assert!(recorded_shares > 0, "node {node} records no share");
    let store = Shared::new(MemStore::default());
    let chain_quorums = Arc::new(ChainQuorums::new());
    chain_quorums.bind(lane.v.clone());
    let manager = QuorumManager::new(
        lane.params.clone(),
        store.clone(),
        Recorded(FixtureContributions {
            me: Some(me.clone()),
            quorums: captured.clone(),
        }),
        Local(Some(me.0)),
        Inline,
    )
    .with_cleanup_facade(chain_quorums.clone());
    let manager = Arc::new(manager);
    lane.install(manager.clone());

    let mut checked = (0, 0, 0);
    for (phase, quorums, reads) in lane.replay_phases() {
        for (q, route) in quorums.iter().zip(reads.found()) {
            let at = format!("{phase} node {node} quorum {}", q["quorumHash"]);
            let ours = &route.quorum;
            let (t, hash) = (ours.llmq_type, ours.commitment.quorum_hash());
            let member = captured[&(t.to_base(), ours.base_hash)].members.contains(&me.0);
            let has_vvec = ours.keys.lock().unwrap().has_vvec();
            assert_eq!(has_vvec, member, "{at}: route B built the vvec of a member's quorum");
            checked.2 += usize::from(has_vvec);
            let looked_up = chain_quorums.quorum(t, &hash).expect("bound").expect("mined");
            assert!(Arc::ptr_eq(&looked_up, &ours.0), "{at}: ChainQuorums answers route B's");
            let info = &q["quorum_info"];
            let entry = &info["nodes"][node.as_str()];
            let variant = usize::try_from(entry["variant"].as_u64().expect("variant")).unwrap();
            let theirs = info["variants"].get(variant).expect("the node's variant").clone();
            let llmq = lane.params.consensus.llmq(t).expect("configured");
            assert_eq!(info_json(route, ours, llmq, false), theirs, "{at} false");
            let mut with = theirs;
            if let Some(sk) = entry.get("secretKeyShare") {
                with.as_object_mut().unwrap().insert("secretKeyShare".into(), sk.clone());
                checked.1 += 1;
            }
            assert_eq!(info_json(route, ours, llmq, true), with, "{at} true");
            checked.0 += 1;
        }
    }
    let built = checked.2;
    assert!(built > 0, "node {node} is a member of a recorded quorum");
    assert_eq!(checked, (10, recorded_shares, built), "every recorded quorum, every share");
    let written = store.0.store.0.lock().unwrap().len();
    assert!(written >= built && store.counts().1 == written, "a row per vvec built: {written}");
    eprintln!(
        "node {node}: {built} vvecs and {} shares through route B, {written} rows",
        checked.1
    );

    lane.feed(lane.tip_height());
    let tip = lane.chain[lane.tip_height()].hash();
    let tip_id = lane.v.on_v(move |_, q| q.chain.reader().lookup(&tip)).recv().expect("V");
    let stale = QuorumDbKey([0xee; 32]);
    let row = store.0.store.0.lock().unwrap().values().next().expect("a row").clone();
    store.write(&stale, &row).expect("planted");
    let height = u32::try_from(lane.tip_height()).expect("small");
    manager.start_cleanup_old_quorum_data(tip_id.expect("V's tip"), height);
    let rows = store.0.store.0.lock().unwrap();
    assert!(!rows.contains_key(&stale), "the stale row is erased");
    assert!(!rows.is_empty(), "the scanned quorums' rows are kept");
    eprintln!("cleanup at {height} kept {} of {written} rows", rows.len());
}

/// QM1a-4a: the node's absent seams installed as the manager on V's facade (`NoDkgSessions`,
/// `NotMasternode`, `MemKeyStore`) give Core's node that is neither a masternode nor watching:
/// each build reads the store, finds no row, gets no contributions (`m_qdkgsman` null,
/// `llmq/quorumsman.cpp:118-122` at v24.0.0) and so writes nothing and warms nothing; no route-B
/// object holds a vvec or a share.
#[test]
fn route_b_with_the_nodes_absent_seams_reads_and_writes_nothing() {
    let mut lane = Lane::start();
    let store = Shared::new(MemKeyStore::new());
    let jobs = Arc::new(Jobs::default());
    let manager = QuorumManager::new(
        lane.params.clone(),
        store.clone(),
        NoDkgSessions,
        NotMasternode,
        jobs.clone(),
    );
    lane.install(Arc::new(manager));
    let mut checked = 0;
    for (phase, _, reads) in lane.replay_phases() {
        for route in reads.found() {
            assert!(!has_contributions(&route.quorum), "{phase}: no contributions");
            checked += 1;
        }
    }
    assert_eq!(checked, 10);
    let (reads, writes) = store.counts();
    assert!(reads >= 10, "every build reads the store first: {reads}");
    assert_eq!((writes, jobs.0.load(SeqCst)), (0, 0), "no write, no warm");
}

/// QM1a-4a: the same replay with no manager installed builds through `NoQuorumHook`: no
/// recorded quorum's route-B object gets a vvec or a share, where the member's manager above
/// built them.
#[test]
fn route_b_without_a_manager_builds_no_contributions() {
    let mut lane = Lane::start();
    let mut checked = 0;
    for (phase, _, reads) in lane.replay_phases() {
        for route in reads.found() {
            assert!(!has_contributions(&route.quorum), "{phase}: no contributions");
            checked += 1;
        }
    }
    assert_eq!(checked, 10);
}

/// A counting executor that runs each job on a thread of its own, never on V's: the manager is
/// installed on V's facade, and a job run inline on V that read V would deadlock it.
#[derive(Default)]
struct OffV(AtomicUsize);

impl WarmExecutor for OffV {
    fn spawn_warm(&self, job: Box<dyn FnOnce() + Send>) {
        self.0.fetch_add(1, SeqCst);
        std::thread::spawn(job);
    }
}

/// QM1a-4b: the node's own manager (`dashd_node::init::quorums::init_quorums`) with a test's
/// off-V executor in place of the `q-cache` thread and an in-memory quorum database.
/// Its hook goes through the harness to every open's coins setup, as the node's
/// `CoinsSetup::quorum_hook`, so V's facade has it from the start. Before `bind_chain` a Platform
/// lookup of a mined quorum is an error; after it, each recorded quorum is route B's own object
/// (`GetQuorum`, `llmq/quorumsman.cpp:491-512` at v24.0.0). As Core's node that is neither a
/// masternode nor watching (`:118-122`), no route-B object gets contributions and no warm job is
/// queued.
#[test]
fn the_nodes_quorum_manager_answers_platform_once_bound() {
    use dash_params::{ChainParamsBuilder, Network, Release};
    use dashd_chainstate::llmq::quorum::NoQuorumHook;

    let jobs = Arc::new(OffV::default());
    let setup =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let mut hook = None;
    let store = dashd_node::init::quorums::QuorumDb::in_memory();
    let node = dashd_node::init::quorums::init_quorums(&setup, store, jobs.clone(), &mut hook);
    let harness = dashd_node::sut::Harness {
        quorum_hook: hook,
        ..Default::default()
    };
    let mut lane = Lane::start_with(harness);
    let probe: SharedQuorumHook = Arc::new(NoQuorumHook);
    let installed = lane.v.on_v(move |o, _| o.install_quorum_hook(probe));
    assert!(!installed.recv().expect("V serves"), "V's facade has the manager's hook");

    let (first, height) = lane.phases()[0].clone();
    lane.feed(height);
    let (t, hash) = lane.recorded(&first)[0];
    let unbound = node.platform.quorum(t, &hash);
    assert!(matches!(unbound, Err(dash_dml::StoreError::NotBuilt(_))), "{first}: unbound");

    node.bind_chain(&lane.v);
    let mut checked = 0;
    for (phase, _, reads) in lane.replay_phases() {
        for route in reads.found() {
            let ours = &route.quorum;
            let (t, hash) = (ours.llmq_type, ours.commitment.quorum_hash());
            let looked_up = node.platform.quorum(t, &hash).expect("bound").expect("mined");
            assert!(Arc::ptr_eq(&looked_up, &ours.0), "{phase}: Platform answers route B's");
            assert!(!has_contributions(ours), "{phase}: no contributions");
            checked += 1;
        }
    }
    assert_eq!(checked, 10);
    assert_eq!(jobs.0.load(SeqCst), 0, "no warm job");
}

// --- W1-3: the signing quorum lookup over route B ---

/// A display-order (RPC) hash as stored bytes.
fn stored(display: &str) -> [u8; 32] {
    let mut bytes: [u8; 32] = unhex(display).try_into().expect("32 bytes");
    bytes.reverse();
    bytes
}

const SIGN_TYPE: LlmqType = LlmqType::LlmqTest;

impl Lane {
    /// The signing lookup over route B bound to this lane's V.
    fn signing(&self) -> SigningQuorums {
        let chain = Arc::new(ChainQuorums::new());
        chain.bind(self.v.clone());
        SigningQuorums::new(chain, self.params.clone())
    }

    /// The recorded `quorum selectquorum` calls: tip height, id and the selected quorum, in
    /// height order; `default_selection` marks the two whose quorum `signing_quorums` holds.
    fn selections(&self) -> Vec<(usize, [u8; 32], QuorumHash, bool)> {
        let mut out = Vec::new();
        for r in self.fixture["requests"].as_array().expect("requests") {
            let Some(selected) = r["selectquorum"].as_object() else {
                continue;
            };
            assert_eq!(as_u8(&r["type"]), SIGN_TYPE.to_base());
            let height = usize::try_from(r["tip_height"].as_u64().expect("height")).unwrap();
            let hash = selected["quorumHash"].as_str().expect("hash");
            let quorum = QuorumHash::from_hex(hash).expect("hex");
            let default = r["label"] == json!("default_selection");
            out.push((height, stored(r["id"].as_str().expect("id")), quorum, default));
        }
        out.sort_by_key(|s| s.0);
        out
    }
}

/// Core's `SelectQuorumForSigning` (`quorum selectquorum`, `SIGN_HEIGHT_OFFSET` at the tip) for
/// every recorded selection-window id at each of its nine blocks per phase, every selection
/// request, and both default-selection requests, whose quorums only `signing_quorums` records.
#[test]
fn route_b_selects_cores_recorded_signing_quorums() {
    let mut lane = Lane::start();
    let lookup = lane.signing();
    let mut windows: Vec<Value> =
        lane.fixture["selection_window"].as_array().expect("selection_window").clone();
    windows.sort_by_key(|w| w["height"].as_u64().expect("height"));
    let selections = lane.selections();
    assert_eq!(selections.len(), 20, "18 window requests and 2 default selections");
    let (mut compared, mut defaults) = (0, 0);
    let mut requests = selections.iter().peekable();
    for window in &windows {
        let height = usize::try_from(window["height"].as_u64().expect("height")).unwrap();
        while let Some((at, id, quorum, default)) = requests.next_if(|r| r.0 <= height) {
            lane.feed(*at);
            let chosen = select_for_signing_default(&lookup, SIGN_TYPE, id).expect("no fault");
            assert_eq!(chosen.map(|q| *q.quorum_hash()), Some(*quorum), "request at {at}");
            if *default {
                let key = format!("{}:{}", SIGN_TYPE.to_base(), quorum);
                assert!(lane.fixture["signing_quorums"].get(&key).is_some(), "{key}");
                defaults += 1;
            }
            compared += 1;
        }
        lane.feed(height);
        let ids = window["ids"].as_array().expect("ids");
        let results = window["results"].as_array().expect("results");
        assert_eq!((ids.len(), results.len()), (20, 20), "at {height}");
        for (id, result) in ids.iter().zip(results) {
            let id = stored(id.as_str().expect("id"));
            let want = QuorumHash::from_hex(result["quorumHash"].as_str().expect("hash")).unwrap();
            let chosen = select_for_signing_default(&lookup, SIGN_TYPE, &id).expect("no fault");
            let chosen = chosen.expect("a quorum");
            assert_eq!(*chosen.quorum_hash(), want, "at {height}");
            assert!(chosen.is_member(&pro_tx_hash(
                result["recoveryMembers"][0].as_str().expect("a recovery member")
            )));
            compared += 1;
        }
    }
    for (at, id, quorum, default) in requests {
        lane.feed(*at);
        let chosen = select_for_signing_default(&lookup, SIGN_TYPE, id).expect("no fault");
        assert_eq!(chosen.map(|q| *q.quorum_hash()), Some(*quorum), "request at {at}");
        defaults += usize::from(*default);
        compared += 1;
    }
    assert_eq!((compared, defaults), (18 * 20 + 20, 2));
}

/// Core's `IsQuorumActive` at both recorded `quorum list` tips: of the recorded `count 4` scan,
/// the newest `keepOldConnections` (3) quorums are active and the fourth is not; a type this
/// chain does not configure has none.
#[test]
fn route_b_is_quorum_active_ends_at_keep_old_connections() {
    let mut lane = Lane::start();
    let lookup = lane.signing();
    let keep = usize::from(lane.params.consensus.llmq(SIGN_TYPE).unwrap().keep_old_connections);
    assert_eq!(keep, 3);
    let lists = lane.fixture["quorum_list"].as_array().expect("quorum_list").clone();
    for list in &lists {
        lane.feed(usize::try_from(list["height"].as_u64().expect("height")).unwrap());
        let scanned = list["count_4"]["llmq_test"].as_array().expect("llmq_test");
        assert_eq!(scanned.len(), keep + 1);
        for (i, hash) in scanned.iter().enumerate() {
            let hash = QuorumHash::from_hex(hash.as_str().expect("hash")).expect("hex");
            let active = lookup.is_quorum_active(SIGN_TYPE, &hash).expect("no fault");
            assert_eq!(active, i < keep, "{} #{i}", list["height"]);
        }
        let first = QuorumHash::from_hex(scanned[0].as_str().unwrap()).unwrap();
        let unknown = LlmqType::from_base(250);
        assert_eq!(lookup.is_quorum_active(unknown, &first), Ok(false));
    }
}

/// With no V bound, or after it has stopped, every lookup is a fault, never "no quorum".
#[test]
fn an_unbound_lookup_faults_instead_of_answering_no_quorum() {
    let setup = Arc::new(
        dash_params::ChainParamsBuilder::new(
            dash_params::Network::Regtest,
            dash_params::Release::V24_0,
        )
        .build()
        .unwrap(),
    );
    let lookup = SigningQuorums::new(Arc::new(ChainQuorums::new()), setup);
    let hash = QuorumHash::from([1; 32]);
    assert!(matches!(lookup.get_quorum(SIGN_TYPE, &hash), Err(StoreError::NotBuilt(_))));
    assert!(matches!(lookup.is_quorum_active(SIGN_TYPE, &hash), Err(StoreError::NotBuilt(_))));
    let selected = select_for_signing_default(&lookup, SIGN_TYPE, &[2; 32]);
    assert!(matches!(selected, Err(StoreError::NotBuilt(_))));
}

#[derive(Default)]
struct RecSigMem(BTreeMap<Vec<u8>, Vec<u8>>);

impl RecSigKv for RecSigMem {
    type Error = std::convert::Infallible;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.0.get(key).cloned())
    }

    fn scan_from(&self, start: &[u8]) -> Result<RecSigScan<'_, Self::Error>, Self::Error> {
        Ok(Box::new(self.0.range(start.to_vec()..).map(|(k, v)| Ok((k.clone(), v.clone())))))
    }

    fn write(&mut self, batch: RecSigBatch) -> Result<(), Self::Error> {
        for op in batch.ops {
            match op {
                RecSigOp::Put(k, v) => {
                    self.0.insert(k, v);
                }
                RecSigOp::Delete(k) => {
                    self.0.remove(&k);
                }
            }
        }
        Ok(())
    }
}

/// The node's side of a signing runtime: the masternode, and every hand-off and notify.
#[derive(Default)]
struct SignerNode {
    me: Option<ProTxHash>,
    signed: std::sync::Mutex<Vec<(QuorumHash, [u8; 32])>>,
    notified: std::sync::Mutex<Vec<[u8; 32]>>,
    /// Each notified signature's sign hash, in notify order.
    sessions: std::sync::Mutex<Vec<[u8; 32]>>,
    /// Each notified signature's inventory hash and proactive-relay flag, in notify order.
    announced: std::sync::Mutex<Vec<([u8; 32], bool)>>,
}

impl SigningNet for SignerNode {
    fn misbehaving(&self, peer: i64, _: i32) {
        panic!("peer {peer} scored");
    }
    fn relay_inv(&self, _: &dash_p2p_core::Inventory) {}
    fn relay_transaction(&self, _: &dash_primitives::TxHash) {}
}

impl RecSigNotify for SignerNode {
    fn notify_recovered_sig(&self, sig: Arc<RecoveredSig>, proactive: bool) {
        self.notified.lock().unwrap().push(sig.id);
        self.sessions.lock().unwrap().push(sig.sign_hash());
        self.announced.lock().unwrap().push((sig.hash(), proactive));
    }
}

impl SignQueue<QuorumHandle> for SignerNode {
    fn force_reannouncement(&self, _: &Arc<QuorumHandle>, _: LlmqType, _: &[u8; 32], _: &[u8; 32]) {
        panic!("no re-sign requested");
    }
    fn async_sign(&self, quorum: Arc<QuorumHandle>, id: [u8; 32], _: [u8; 32]) {
        self.signed.lock().unwrap().push((*quorum.quorum_hash(), id));
    }
}

impl LocalMasternode for SignerNode {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        self.me
    }
}

struct Seq(u64);

impl rand_core::TryRng for Seq {
    type Error = std::convert::Infallible;
    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok(self.try_next_u64()? as u32)
    }
    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        Ok(self.0)
    }
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        dst.fill(0);
        Ok(())
    }
}

type Runtime = SigningRuntime<RecSigMem, SigningQuorums>;

fn runtime(lane: &Lane, me: Option<ProTxHash>, now: i64) -> (Runtime, Arc<SignerNode>) {
    runtime_on(lane, me, Arc::new(MockClock::at(now)), DEFAULT_MAX_AGE, RecSigMem::default())
}

/// Core's `DEFAULT_MAX_RECOVERED_SIGS_AGE`, a week.
const DEFAULT_MAX_AGE: i64 = 7 * 24 * 60 * 60;

fn runtime_on<K: RecSigKv<Error: std::fmt::Debug> + Send>(
    lane: &Lane,
    me: Option<ProTxHash>,
    clock: Arc<MockClock>,
    max_age: i64,
    store: K,
) -> (SigningRuntime<K, SigningQuorums>, Arc<SignerNode>) {
    let node = Arc::new(SignerNode {
        me,
        ..SignerNode::default()
    });
    let handles = SigningHandles {
        net: node.clone(),
        notify: node.clone(),
        masternode: node.clone(),
        clock,
    };
    let platform = lane.params.consensus.llmq_type_platform;
    let manager = SigningManager::new(store, platform, max_age);
    let rt = SigningRuntime::new(manager, Arc::new(lane.signing()), lane.params.clone(), handles);
    assert!(rt.set_sign_queue(node.clone()).is_ok());
    (rt, node)
}

/// The first selection-window request: its tip, mocktime, id, message, selected quorum and a
/// recovery member.
fn first_window_request(lane: &Lane) -> &Value {
    let requests = lane.fixture["requests"].as_array().expect("requests");
    requests.iter().find(|r| r["label"] == json!("selection_window")).expect("a window request")
}

/// `AsyncSignIfMember` with no quorum named, on a runtime over route B: the default selection
/// picks Core's recorded quorum, the recovery member signs in it and its vote is written.
#[test]
fn a_runtime_over_route_b_signs_in_cores_selected_quorum() {
    let mut lane = Lane::start();
    let r = first_window_request(&lane).clone();
    lane.feed(usize::try_from(r["tip_height"].as_u64().unwrap()).unwrap());
    let member = pro_tx_hash(r["selectquorum"]["recoveryMembers"][0].as_str().unwrap());
    let quorum = QuorumHash::from_hex(r["selectquorum"]["quorumHash"].as_str().unwrap()).unwrap();
    let (rt, node) = runtime(&lane, Some(member), r["mocktime"].as_i64().unwrap());
    let (id, msg) = (stored(r["id"].as_str().unwrap()), stored(r["msgHash"].as_str().unwrap()));
    let request = SignRequest {
        llmq_type: SIGN_TYPE,
        id,
        msg_hash: msg,
        quorum_hash: None,
        allow_resign: false,
        allow_diff_msg: false,
    };
    assert_eq!(rt.async_sign_if_member(&request), Ok(true));
    assert_eq!(*node.signed.lock().unwrap(), [(quorum, id)]);
    assert_eq!(rt.get_vote_for_id(SIGN_TYPE, &id), Ok(Some(msg)));
    let selected = rt.select_quorum_for_signing(SIGN_TYPE, &id).expect("no fault");
    assert_eq!(selected.map(|s| s.quorum_hash), Some(quorum));

    let (outsider, _) = runtime(&lane, Some(ProTxHash::from([0xee; 32])), 0);
    assert_eq!(outsider.async_sign_if_member(&request), Ok(false), "not a member");
}

/// A recorded `qsigrec` through the runtime over route B: the intake finds its quorum active,
/// the pending pass verifies Core's signature under route B's quorum key, and it is stored and
/// notified.
#[test]
fn a_runtime_over_route_b_verifies_and_stores_cores_recovered_sig() {
    let mut lane = Lane::start();
    let r = first_window_request(&lane).clone();
    lane.feed(usize::try_from(r["tip_height"].as_u64().unwrap()).unwrap());
    let recsig = &r["recsig"];
    let mut wire = vec![as_u8(&recsig["llmqType"])];
    for field in ["quorumHash", "id", "msgHash"] {
        wire.extend_from_slice(&stored(recsig[field].as_str().unwrap()));
    }
    wire.extend_from_slice(&unhex(recsig["sig"].as_str().unwrap()));
    let id = stored(recsig["id"].as_str().unwrap());
    let (rt, node) = runtime(&lane, None, r["mocktime"].as_i64().unwrap());
    assert_eq!(rt.on_qsigrec(7, &wire, false).1, Ok(IntakeStatus::Queued));
    assert_eq!(rt.process_pending_recovered_sigs(false, &mut Seq(1)), Ok(false));
    assert_eq!(rt.has_recovered_sig_for_id(SIGN_TYPE, &id), Ok(true));
    assert_eq!(*node.notified.lock().unwrap(), [id]);

    let mut forged = wire.clone();
    let last = forged.len() - 1;
    forged[last] ^= 1;
    let (rt, _) = runtime(&lane, None, r["mocktime"].as_i64().unwrap());
    rt.on_qsigrec(-1, &forged, false).1.expect("no fault");
    rt.process_pending_recovered_sigs(false, &mut Seq(1)).expect("no fault");
    assert_eq!(rt.has_recovered_sig_for_id(SIGN_TYPE, &id), Ok(false), "a forged sig");
}

/// A route-B handle's key reads: no vvec and no share before the quorum has one; with the vvec
/// Core builds from the valid members' captured `qcontrib` vvecs (public keys only), its first
/// element is the mined quorum key, every valid member has a share and an invalid or
/// out-of-range member none.
#[test]
fn a_route_b_handle_reads_its_quorums_key_shares() {
    use dash_llmq::dkg::bls::build_quorum_verification_vector;

    let mut lane = Lane::start();
    let (phase, height) = lane.phases()[0].clone();
    lane.feed(height);
    let (t, hash) = lane.recorded(&phase)[0];
    let handle = lane.signing().get_quorum(t, &hash).expect("no fault").expect("mined");
    let n = u16::try_from(handle.members().len()).unwrap();
    assert!(!handle.has_verification_vector());
    assert!((0..n).all(|i| handle.pub_key_share(i).is_none()));
    assert!(handle.sk_share().is_none());

    let captured = captured_from_fixture(&lane);
    let quorum = &captured[&(t.to_base(), BlockHash::from(*hash.as_bytes()))];
    assert_eq!(quorum.members, handle.members(), "route B's members in commitment order");
    let valid: Vec<bool> = handle.members().iter().map(|m| handle.is_valid_member(m)).collect();
    let vvecs: Vec<Vec<_>> = quorum
        .members
        .iter()
        .zip(&valid)
        .filter(|(_, v)| **v)
        .map(|(m, _)| {
            let payload = &quorum.payloads[&m.to_string()];
            Contribution::read(payload, &quorum.params, BlsCtx::BASIC)
                .expect("qcontrib")
                .vvec()
                .to_vec()
        })
        .collect();
    let refs: Vec<&[_]> = vvecs.iter().map(Vec::as_slice).collect();
    let vvec = build_quorum_verification_vector(&refs, SCHEME).expect("a quorum vvec");
    assert!(vvec[0].core_eq(handle.quorum_public_key()), "the mined quorum key");
    handle.quorum().keys.lock().unwrap().set_vvec_unchecked(Arc::new(vvec));

    assert!(handle.has_verification_vector());
    for (i, valid) in valid.iter().enumerate() {
        let share = handle.pub_key_share(u16::try_from(i).unwrap());
        assert_eq!(share.is_some(), *valid, "member {i}");
    }
    assert!(handle.pub_key_share(n).is_none(), "out of range");
    assert!(valid.iter().any(|v| *v));
    assert_eq!(handle.member_index(&handle.members()[1]), Some(1));
    assert_eq!(handle.member_index(&ProTxHash::from([0xee; 32])), None);
    assert!(handle.sk_share().is_none(), "no secret share without the node's contribution");
}

/// The lookup waits on V, so a call from a no-wait-marked task is a contract violation.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "signing quorum lookup from a no-wait task")]
fn a_lookup_from_a_no_wait_task_panics() {
    let setup = Arc::new(
        dash_params::ChainParamsBuilder::new(
            dash_params::Network::Regtest,
            dash_params::Release::V24_0,
        )
        .build()
        .unwrap(),
    );
    let lookup = SigningQuorums::new(Arc::new(ChainQuorums::new()), setup);
    let task = dashd_chainstate::chain::reply::no_wait_task(async move {
        lookup.is_quorum_active(SIGN_TYPE, &QuorumHash::from([1; 32]))
    });
    let mut task = std::pin::pin!(task);
    let waker = std::task::Waker::noop();
    let _ = task.as_mut().poll(&mut std::task::Context::from_waker(waker));
}

/// One line of a node's recorded log.
enum Step {
    Mocktime(i64),
    Tip(usize),
    QsigRec(i64, Vec<u8>),
    Recovered([u8; 32]),
    Written([u8; 32]),
    /// A recorded cleanup line: `CleanupOldRecoveredSigs` or `CleanupOldVotes`, and its count.
    Cleanup(String, usize),
    HasRecSig(LlmqType, [u8; 32], [u8; 32]),
    /// A submitting `quorum sign` or `platformsign`, with Core's answer where it is compared.
    Sign(SignRequest, Option<(bool, &'static str)>),
    /// The phase's `hasrecsig`, `isconflicting` and `getrecsig` answers, read after its barrier.
    Observe(Vec<Value>),
    /// The subscriber's GETDATA for each announced hash, and the `qsigrec` it received, if any.
    Arm(Vec<([u8; 32], Option<Vec<u8>>)>),
    /// The start of the final phase, which only mines blocks.
    FinalPhase,
}

/// The exact sign answers, keyed by node, RPC, params and phase.
type ExactSigns = BTreeMap<(String, String, String, u64), bool>;

fn exact_signs(lane: &Lane) -> ExactSigns {
    let mut exact = ExactSigns::new();
    for o in lane.fixture["observations"].as_array().expect("observations") {
        if o["sign_exact_reason"].is_null() {
            continue;
        }
        let method = o["method"].as_str().expect("method").to_owned();
        let key = (o["node"].to_string(), method, o["params"].to_string(), phase_of(o));
        assert!(exact.insert(key, o["result"].as_bool().expect("a bool")).is_none());
    }
    exact
}

/// The second requests of the Platform same-id pairs, whose submits are barrier inputs.
fn platform_seconds(lane: &Lane) -> BTreeSet<(String, String)> {
    let requests = lane.fixture["requests"].as_array().expect("requests");
    let seconds = requests.iter().filter(|r| r["label"] == json!("platform_same_id_second"));
    seconds.map(|r| (r["id"].to_string(), r["msgHash"].to_string())).collect()
}

fn phase_of(v: &Value) -> u64 {
    v["phase"].as_u64().expect("phase")
}

/// The RPC's `AsyncSignIfMember` call, or `None` for `submit = false`, which casts no vote.
// Core: v24.0.2 src/rpc/quorums.cpp:514-582, :601-650
fn sign_request(lane: &Lane, method: &str, params: &[Value]) -> Option<SignRequest> {
    let platform = lane.params.consensus.llmq_type_platform;
    let (llmq_type, rest) = match method {
        "sign" => (LlmqType::from_base(as_u8(&params[0])), &params[1..]),
        "platformsign" => (platform, params),
        other => panic!("sign rpc {other}"),
    };
    let [id, msg, quorum, submit] = rest else {
        panic!("{method} params {params:?}");
    };
    if !submit.as_bool().expect("submit") {
        return None;
    }
    let quorum = quorum.as_str().expect("quorumHash");
    Some(SignRequest {
        llmq_type,
        id: stored(id.as_str().expect("id")),
        msg_hash: stored(msg.as_str().expect("msgHash")),
        quorum_hash: (!quorum.is_empty()).then(|| QuorumHash::from(stored(quorum))),
        allow_resign: false,
        allow_diff_msg: llmq_type.to_base() == platform.to_base(),
    })
}

/// Core's `CRecoveredSig::ToJson`, whose `hash` hashes the signature bytes alone.
// Core: v24.0.2 src/llmq/core_write.cpp:103-113
fn recsig_json(sig: &RecoveredSig) -> Value {
    let display = |bytes: &[u8]| {
        let mut bytes = bytes.to_vec();
        bytes.reverse();
        hex(&bytes)
    };
    json!({
        "hash": display(&dash_sha256::sha256d(&sig.sig)),
        "id": display(&sig.id),
        "llmqType": sig.llmq_type.to_base(),
        "msgHash": display(&sig.msg_hash),
        "quorumHash": display(sig.quorum_hash.as_bytes()),
        "sig": hex(&sig.sig),
    })
}

fn wire_of(sig: &RecoveredSig) -> Vec<u8> {
    [&[sig.llmq_type.to_base()][..], sig.quorum_hash.as_bytes(), &sig.id, &sig.msg_hash, &sig.sig]
        .concat()
}

/// The subscriber's GETDATAs, in its queue order, with what each received. It is connected to
/// node 0 (`contrib/node/lq3a-captures/capture_lane_c.py:461`).
// Core: v24.0.2 src/protocol.h:521
fn subscriber_fetches(lane: &Lane) -> Vec<([u8; 32], Option<Vec<u8>>)> {
    let subscriber = &lane.fixture["relay"]["subscriber"];
    let mut captured = BTreeMap::new();
    for c in subscriber["captured"].as_array().expect("captured") {
        assert_eq!(c["command"], json!("qsigrec"));
        let payload = unhex(c["payload"].as_str().expect("payload"));
        assert!(captured.insert(stored(c["inv_hash"].as_str().expect("hash")), payload).is_none());
    }
    let fetches: Vec<_> = subscriber["invs"]
        .as_array()
        .expect("invs")
        .iter()
        .map(|inv| {
            assert_eq!(inv["type"], json!(28), "MSG_QUORUM_RECOVERED_SIG");
            let hash = stored(inv["hash"].as_str().expect("hash"));
            (hash, captured.remove(&hash))
        })
        .collect();
    assert!(captured.is_empty(), "every captured qsigrec answers an inv");
    fetches
}

fn final_phase(lane: &Lane) -> &Value {
    let phases = lane.fixture["control_phases"].as_array().expect("control_phases");
    let last = phases.last().expect("a phase");
    assert_eq!((&last["label"], &last["expected"]), (&json!("tip"), &json!([])));
    last
}

/// `node`'s recorded log in its order: mocktime, tips, received `qsigrec`s, local recoveries,
/// writes, cleanups, submitting signs, each phase's reads after its barrier, each request's
/// `quorum hasrecsig` at the request's log end, and the final phase's start.
fn node_log(lane: &Lane, node: &str, exact: &mut ExactSigns) -> Vec<Step> {
    let index: u64 = node.parse().expect("a node index");
    let seconds = platform_seconds(lane);
    let mut observed: BTreeMap<u64, Vec<Value>> = BTreeMap::new();
    for o in lane.fixture["observations"].as_array().expect("observations") {
        let read = ["quorum hasrecsig", "quorum isconflicting", "quorum getrecsig"];
        if o["node"].as_u64() == Some(index) && read.contains(&o["method"].as_str().unwrap()) {
            observed.entry(phase_of(o)).or_default().push(o.clone());
        }
    }
    let mut log = Vec::new();
    for e in lane.fixture["events"][node].as_array().expect("events") {
        let pos = e["pos"].as_u64().expect("pos");
        let sign_hash = || stored(e["sign_hash"].as_str().expect("sign_hash"));
        let step = match e["kind"].as_str().expect("kind") {
            "setmocktime" => Step::Mocktime(e["value"].as_i64().expect("value")),
            "tip" => Step::Tip(usize::try_from(e["height"].as_u64().expect("height")).unwrap()),
            "local_recovery" => Step::Recovered(sign_hash()),
            "write" => Step::Written(sign_hash()),
            "cleanup" => Step::Cleanup(
                e["function"].as_str().expect("function").to_owned(),
                usize::try_from(e["deleted"].as_u64().expect("deleted")).unwrap(),
            ),
            "sign_rpc" => {
                let method = e["method"].as_str().expect("method");
                let params = e["params"].as_array().expect("params");
                let Some(request) = sign_request(lane, method, params) else {
                    continue;
                };
                let key = (
                    node.to_owned(),
                    format!("quorum {method}"),
                    e["params"].to_string(),
                    phase_of(e),
                );
                let second = method == "platformsign"
                    && seconds.contains(&(params[0].to_string(), params[1].to_string()));
                let want = match exact.remove(&key) {
                    Some(want) => Some((want, "exact sign")),
                    // A member that voted for the first message answers true whether or not the
                    // second's recovered signature is there yet.
                    // Core: v24.0.2 src/llmq/signing_shares.cpp:834-878
                    None if second => {
                        Some((e["result"].as_bool().expect("result"), "platform second sign"))
                    }
                    None => None,
                };
                Step::Sign(request, want)
            }
            "barrier" => match observed.remove(&phase_of(e)) {
                Some(reads) => Step::Observe(reads),
                None => continue,
            },
            _ => continue,
        };
        log.push((pos, step));
    }
    assert!(observed.is_empty(), "node {node}: reads without a barrier");
    for m in lane.fixture["sig_messages"].as_array().expect("sig_messages") {
        if m["command"] == json!("qsigrec") && m["to"].as_u64() == Some(index) {
            let wire = unhex(m["payload"].as_str().expect("payload"));
            let step = Step::QsigRec(m["peer"].as_i64().expect("peer"), wire);
            log.push((m["receive_pos"].as_u64().expect("receive_pos"), step));
        }
    }
    for r in lane.fixture["requests"].as_array().expect("requests") {
        assert_eq!(r["hasrecsig"][node], json!(true), "every request is recovered on every node");
        let t = LlmqType::from_base(as_u8(&r["type"]));
        let (id, msg) = (stored(r["id"].as_str().unwrap()), stored(r["msgHash"].as_str().unwrap()));
        log.push((r["log_end"][node].as_u64().expect("log_end"), Step::HasRecSig(t, id, msg)));
    }
    let start = final_phase(lane)["starts"][node].as_u64().expect("final phase start");
    log.push((start, Step::FinalPhase));
    if index == 0 {
        log.push((start, Step::Arm(subscriber_fetches(lane))));
    }
    log.sort_by_key(|(pos, _)| *pos);
    log.into_iter().map(|(_, step)| step).collect()
}

type Durable = KvRecSigStore<RocksEngine>;

fn open_recsigs(dir: &Path) -> Durable {
    let (engine, _) = RocksEngine::open(dir, &RECSIGDB_SCHEMA, &RocksConfig::default()).unwrap();
    KvRecSigStore::new(engine)
}

/// The durable store, recording each deleted key.
struct Deleting {
    store: Durable,
    deleted: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
}

impl RecSigKv for Deleting {
    type Error = <Durable as RecSigKv>::Error;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        self.store.get(key)
    }

    fn scan_from(&self, start: &[u8]) -> Result<RecSigScan<'_, Self::Error>, Self::Error> {
        self.store.scan_from(start)
    }

    fn write(&mut self, batch: RecSigBatch) -> Result<(), Self::Error> {
        let mut deleted = self.deleted.lock().unwrap();
        deleted.extend(batch.ops.iter().filter_map(|op| match op {
            RecSigOp::Delete(key) => Some(key.clone()),
            RecSigOp::Put(..) => None,
        }));
        self.store.write(batch)
    }
}

/// Node1 runs `-maxrecsigsage=60` (test-vectors/lane-c/PROVENANCE.md:67).
fn max_age_of(lane: &Lane, node: &str) -> i64 {
    if lane.fixture["cleanup"]["node"].as_u64() == node.parse().ok() {
        60
    } else {
        DEFAULT_MAX_AGE
    }
}

/// A node's runtime in the replay, the source of each signature it wrote, and what it checked.
struct ReplayNode {
    rt: SigningRuntime<Deleting, SigningQuorums>,
    deleted: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    /// Signatures and votes the cleanups deleted since the last recorded line for each.
    expired: (usize, usize),
    signer: Arc<SignerNode>,
    clock: Arc<MockClock>,
    rng: Seq,
    /// Each written session's signature, which is the session's only one.
    local: BTreeMap<[u8; 32], Vec<u8>>,
    pushed: BTreeSet<[u8; 32]>,
    written: BTreeMap<[u8; 32], bool>,
    /// Notifies and sign hand-offs at the final phase's start.
    at_final: Option<(usize, usize)>,
    checked: BTreeMap<String, usize>,
}

impl ReplayNode {
    /// Pending passes until none leaves more work, then each new write's source.
    fn settle(&mut self) {
        while self.rt.process_pending_recovered_sigs(false, &mut self.rng).expect("no fault") {}
        for h in self.signer.sessions.lock().unwrap().drain(..) {
            let local = self.pushed.contains(&h);
            assert!(self.written.insert(h, local).is_none(), "one write per session");
        }
    }

    fn signatures(&self) -> (usize, usize) {
        (self.signer.notified.lock().unwrap().len(), self.signer.signed.lock().unwrap().len())
    }

    fn count(&mut self, what: &str) {
        *self.checked.entry(what.to_owned()).or_default() += 1;
    }

    fn read(&mut self, o: &Value) {
        let p = o["params"].as_array().expect("params");
        let t = LlmqType::from_base(as_u8(&p[0]));
        let (id, msg) = (stored(p[1].as_str().unwrap()), stored(p[2].as_str().unwrap()));
        let method = o["method"].as_str().unwrap();
        match method {
            "quorum hasrecsig" => {
                let got = self.rt.has_recovered_sig(t, &id, &msg);
                assert_eq!(got, Ok(o["result"].as_bool().unwrap()), "{o}");
            }
            "quorum isconflicting" => {
                let got = self.rt.is_conflicting(t, &id, &msg);
                assert_eq!(got, Ok(o["result"].as_bool().unwrap()), "{o}");
            }
            "quorum getrecsig" => {
                let sig = self.rt.get_recovered_sig(t, &id, &msg).expect("no fault");
                assert_eq!(sig.map(|s| recsig_json(&s)).as_ref(), Some(&o["result"]), "{o}");
            }
            other => panic!("{other}"),
        }
        self.count(method);
    }

    fn step(&mut self, step: Step) {
        match step {
            Step::Mocktime(t) => self.clock.set_mocktime(t),
            Step::Tip(_) => {}
            Step::QsigRec(peer, wire) => {
                self.rt.on_qsigrec(peer, &wire, false).1.expect("no fault");
            }
            Step::Recovered(h) => {
                let wire = self.local.get(&h).expect("the session's write");
                let sig = RecoveredSig::decode(&mut wire.as_slice()).expect("a recovered sig");
                assert_eq!(sig.sign_hash(), h);
                self.pushed.insert(h);
                self.rt.push_reconstructed_recovered_sig(Arc::new(sig));
            }
            Step::Written(h) => {
                self.settle();
                assert_eq!(self.rt.has_recovered_sig_for_session(&h), Ok(true), "written by now");
            }
            Step::Cleanup(function, deleted) => {
                self.rt.cleanup().expect("no fault");
                for key in self.deleted.lock().unwrap().drain(..) {
                    if key.starts_with(b"\x04rs_t") {
                        self.expired.0 += 1;
                    } else if key.starts_with(b"\x05rs_vt") {
                        self.expired.1 += 1;
                    }
                }
                let expired = match function.as_str() {
                    "CleanupOldRecoveredSigs" => &mut self.expired.0,
                    "CleanupOldVotes" => &mut self.expired.1,
                    other => panic!("cleanup {other}"),
                };
                assert_eq!(std::mem::take(expired), deleted, "{function}");
                self.count(&function);
            }
            Step::HasRecSig(t, id, msg) => {
                assert_eq!(self.rt.has_recovered_sig(t, &id, &msg), Ok(true));
            }
            Step::Sign(request, want) => {
                let got = self.rt.async_sign_if_member(&request).expect("no fault");
                if let Some((want, what)) = want {
                    assert_eq!(got, want, "{what} {:?} {:?}", request.id, request.msg_hash);
                    self.count(what);
                }
            }
            Step::Observe(reads) => reads.iter().for_each(|o| self.read(o)),
            Step::Arm(fetches) => {
                let announced = self.signer.announced.lock().unwrap().clone();
                assert!(announced.iter().all(|(_, proactive)| !proactive), "announced by inv");
                let invs: BTreeSet<_> = fetches.iter().map(|(h, _)| *h).collect();
                assert_eq!(announced.len(), invs.len(), "one announcement per signature");
                assert_eq!(announced.into_iter().map(|(h, _)| h).collect::<BTreeSet<_>>(), invs);
                for (h, want) in fetches {
                    let got = self.rt.for_getdata(&h).expect("no fault");
                    assert_eq!(got.map(|s| wire_of(&s)), want, "getdata {h:?}");
                    self.count(if want.is_some() {
                        "getdata qsigrec"
                    } else {
                        "getdata none"
                    });
                }
            }
            Step::FinalPhase => {
                self.settle();
                self.at_final = Some(self.signatures());
            }
        }
    }
}

/// The lane-c per-node replay: each node's recorded log, at its recorded tip and mocktime,
/// through its own runtime over route B and a RocksDB recovered-signature database, as that
/// node's masternode.
/// - Intake: each recorded `qsigrec` in receive order, and each local recovery handed over as
///   Core's sig-share manager does. Every request's `hasrecsig`, each node's `final_hasrecsig`,
///   and each write's session and source (Core's 493 remote and 167 local) are Core's. Node1
///   runs `-maxrecsigsage=60`, so its recorded cleanups delete: each recorded line's count of
///   expired signatures or votes is the rows the replay's cleanups deleted since.
/// - Signs: every submitting `quorum sign` and `platformsign` at its log position; each of the
///   186 exact answers (the threshold submits, Platform's included, and node1's two isolated
///   vote-only signs either side of its recorded vote expiry) is Core's, and so is each member's
///   submit of a Platform pair's second message under the same id.
/// - Reads: every recorded `hasrecsig`, `isconflicting` and `getrecsig` after its phase barrier.
/// - Relay: node 0 announces every signature once, by inv, as the subscriber saw them; at the
///   subscriber's GETDATAs it serves exactly the captured `qsigrec`s, byte for byte.
/// - The final phase, which only mines blocks, notifies and signs nothing.
/// - Reopened, each node's database still holds its final signatures and node1 its last vote.
// Core: v24.0.2 src/llmq/net_signing.cpp:198-281, src/llmq/signing.cpp:266-316, :342-381,
// :410-421, :581-634, src/llmq/signing_shares.cpp:792-878,
// src/net_processing.cpp:2668-2689, :3038-3044
#[test]
fn each_nodes_recorded_log_replays_to_cores_writes_signs_reads_and_relay() {
    let mut lane = Lane::start();
    let nodes: Vec<String> =
        lane.fixture["final_hasrecsig"].as_object().expect("per node").keys().cloned().collect();
    let mut exact = exact_signs(&lane);
    assert_eq!(exact.len(), 186);
    let mut dirs = Vec::new();
    let mut replay = Vec::new();
    let mut steps = Vec::new();
    for (n, node) in nodes.iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let me = lane.fixture["signers"][node]["proTxHash"].as_str().map(pro_tx_hash);
        let clock = Arc::new(MockClock::at(0));
        let deleted = Arc::default();
        let store = Deleting {
            store: open_recsigs(dir.path()),
            deleted: Arc::clone(&deleted),
        };
        let (rt, signer) = runtime_on(&lane, me, clock.clone(), max_age_of(&lane, node), store);
        dirs.push(dir);
        let mut local = BTreeMap::new();
        let mut recorded = BTreeMap::new();
        for e in lane.fixture["events"][node].as_array().unwrap() {
            if e["kind"] == json!("write") {
                let h = stored(e["sign_hash"].as_str().unwrap());
                let is_local = e["source"] == json!("local");
                assert!(recorded.insert(h, is_local).is_none(), "one recorded write per session");
                local.insert(h, unhex(e["wire_hex"].as_str().unwrap()));
            }
        }
        let log = node_log(&lane, node, &mut exact);
        let mut tip = log
            .iter()
            .find_map(|s| {
                if let Step::Tip(h) = s {
                    Some(*h)
                } else {
                    None
                }
            })
            .unwrap();
        for (seq, step) in log.into_iter().enumerate() {
            if let Step::Tip(h) = step {
                tip = h;
            }
            steps.push((tip, n, seq, step));
        }
        let node = ReplayNode {
            rt,
            deleted,
            expired: (0, 0),
            signer,
            clock,
            rng: Seq(n as u64 + 1),
            local,
            pushed: BTreeSet::new(),
            written: BTreeMap::new(),
            at_final: None,
            checked: BTreeMap::new(),
        };
        replay.push((node, recorded));
    }
    assert!(exact.is_empty(), "every exact sign is a recorded sign_rpc: {exact:?}");
    steps.sort_by_key(|(tip, n, seq, _)| (*tip, *n, *seq));
    for (tip, n, _, step) in steps {
        lane.feed(tip);
        replay[n].0.step(step);
    }
    assert_eq!(lane.tip_height(), 634);
    let (mut remote, mut local) = (0, 0);
    let mut checked = BTreeMap::<String, usize>::new();
    for ((node, recorded), name) in replay.iter_mut().zip(&nodes) {
        node.settle();
        assert_eq!(node.written, *recorded, "node {name}: each write's session and source");
        for (h, want) in lane.fixture["final_hasrecsig"][name].as_object().unwrap() {
            let got = node.rt.has_recovered_sig_for_session(&stored(h));
            assert_eq!(got, Ok(want.as_bool().unwrap()), "node {name}: final hasrecsig {h}");
        }
        assert_eq!(node.at_final, Some(node.signatures()), "node {name}: final phase signs");
        assert_eq!(node.expired, (0, 0), "node {name}: every deletion is a recorded line's");
        local += recorded.values().filter(|l| **l).count();
        remote += recorded.values().filter(|l| !**l).count();
        for (what, n) in &node.checked {
            *checked.entry(what.clone()).or_default() += n;
        }
    }
    assert_eq!((remote, local), (493, 167));
    let want: BTreeMap<String, usize> = [
        ("CleanupOldRecoveredSigs", 4),
        ("CleanupOldVotes", 4),
        ("exact sign", 186),
        ("getdata none", 38),
        ("getdata qsigrec", 17),
        ("platform second sign", 6),
        ("quorum getrecsig", 660),
        ("quorum hasrecsig", 1347),
        ("quorum isconflicting", 700),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    assert_eq!(checked, want);

    drop(replay);
    let end = final_phase(&lane)["held_mocktime"].as_i64().expect("held_mocktime");
    let cleanup = &lane.fixture["cleanup"];
    let vote_node = cleanup["node"].to_string();
    for (name, dir) in nodes.iter().zip(&dirs) {
        let clock = Arc::new(MockClock::at(end));
        let store = open_recsigs(dir.path());
        let (rt, _) = runtime_on(&lane, None, clock, max_age_of(&lane, name), store);
        for (h, want) in lane.fixture["final_hasrecsig"][name].as_object().unwrap() {
            let got = rt.has_recovered_sig_for_session(&stored(h));
            assert_eq!(got, Ok(want.as_bool().unwrap()), "node {name} reopened: {h}");
        }
        if *name == vote_node {
            let id = stored(cleanup["vote_only_id"].as_str().unwrap());
            let last = stored(cleanup["vote_messages"][1].as_str().unwrap());
            assert_eq!(rt.get_vote_for_id(SIGN_TYPE, &id), Ok(Some(last)), "node {name}'s vote");
        }
    }
}
