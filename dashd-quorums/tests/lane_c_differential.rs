//! The lane (c) differential (plan §4, QM1a-3): Core v24.0.0's recorded regtest run
//! (`test-vectors/lane-c`) replayed into a real V (`dashd_node::sut::NodeSut`), its quorums read
//! through route B's binding, and each node's contributions built by [`QuorumManager`] with `C`
//! fed from the captured `qcontrib` messages and `M` that node. Every recorded `quorum list`
//! (default and `count`) and `quorum info` (both `includeSkShare` flags, every node) must equal
//! the answer Core's RPC builds from the same reads (`rpc/quorums.cpp:50-190`, `:192-252`), field
//! by field. A recorded quorum, node or call that is missing fails the test.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Mutex};

use dash_consensus::primitives::decode_block;
use dash_consensus::{Block, BlockId};
use dash_dml::Dmn;
use dash_dml::netinfo::{is_empty, primary};
use dash_evo::llmq::{OperatorKeyView, is_quorum_type_enabled};
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::{BlsCtx, BlsPkValue, BlsSchemeKind, BlsSkValue, decrypt_share};
use dash_llmq::dkg::wire::Contribution;
use dash_llmq::quorum::{
    CleanupCounts, QuorumDbKey, QuorumKeys, QuorumView, StoredContributions, quorum_info,
};
use dash_params::{ChainSetup, LlmqType};
use dash_primitives::{BlockHash, NetInfo, QuorumHash};
use dash_types::Numeric as _;
use dashd_chainstate::chain::{ChainHandle, ChainQuery, OnV};
use dashd_chainstate::llmq::quorum::{NodeQuorum, QuorumBuildHook, QuorumPtr, SharedQuorumHook};
use dashd_quorums::traits::Contributions;
use dashd_quorums::{
    ChainQuorums, LocalMasternode, MemKeyStore, NoDkgSessions, NotMasternode, QuorumKeyStore,
    QuorumLookup, QuorumManager, VerifiedContributions, WarmExecutor,
};
use serde_json::{Map, Value, json};

const SCHEME: BlsSchemeKind = BlsSchemeKind::Basic;

fn lane_c() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-vectors/lane-c")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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

// --- The manager's seams over the fixture ---

#[derive(Default)]
struct MemStore(Mutex<BTreeMap<QuorumDbKey, StoredContributions>>);

impl QuorumKeyStore for MemStore {
    type Error = String;

    fn read(&self, key: &QuorumDbKey) -> Result<Option<StoredContributions>, String> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }

    fn write(&self, key: &QuorumDbKey, row: &StoredContributions) -> Result<(), String> {
        self.0.lock().unwrap().insert(*key, row.clone());
        Ok(())
    }

    fn erase_except(
        &self,
        keep: &std::collections::BTreeSet<QuorumDbKey>,
    ) -> Result<CleanupCounts, String> {
        self.0.lock().unwrap().retain(|key, _| keep.contains(key));
        Ok(CleanupCounts::default())
    }
}

struct Inline;

impl WarmExecutor for Inline {
    fn spawn_warm(&self, job: Box<dyn FnOnce() + Send>) {
        job();
    }
}

struct Local(Option<ProTxHash>);

impl LocalMasternode for Local {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        self.0
    }
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
            sks.push(
                decrypt_share(qc.blobs(), my_index, sk, SCHEME).unwrap_or_else(BlsSkValue::invalid),
            );
        }
        Some((indexes, vvecs, sks))
    }
}

// --- Route B over the replayed V ---

/// What route B gives for one recorded quorum, with `previousConsecutiveDKGFailures`' input.
struct RouteB {
    quorum: QuorumPtr,
    base_height: i32,
    /// `GetLastMinedCommitmentsByQuorumIndexUntilBlock(type, base, index, 0)`'s base height.
    prev_same_index_base: Option<i32>,
}

/// One phase's reads at its recorded tip: `quorum list` per type for each count, and each
/// recorded quorum through `GetQuorum(type, hash)`.
struct PhaseReads {
    lists: BTreeMap<String, Value>,
    quorums: Vec<RouteB>,
}

fn type_name(t: LlmqType) -> String {
    t.to_string()
}

fn read_phase(
    v: &ChainHandle,
    params: &Arc<ChainSetup>,
    tip: BlockHash,
    recorded: Vec<(LlmqType, QuorumHash)>,
) -> PhaseReads {
    let params = params.clone();
    v.on_v(move |o: &OnV<'_>, q: &ChainQuery<'_>| {
        let reader = q.chain.reader();
        let tip_id = reader.lookup(&tip).expect("V has the recorded tip");
        let height =
            |id: BlockId| i32::try_from(reader.entry(id).expect("an entry").height).expect("small");
        let binding = q.bind(o);
        // `GetEnabledQuorumTypes(pindexTip)` (llmq/utils.cpp), each `IsQuorumTypeEnabled`.
        let enabled: Vec<(LlmqType, u16)> = q
            .evo_view()
            .with_qc(q.bls(), |qc| {
                params
                    .consensus
                    .llmq_params()
                    .filter_map(|(t, p)| {
                        is_quorum_type_enabled(&qc.llmq, t, Some(tip_id))
                            .expect("IsQuorumTypeEnabled")
                            .then_some((t, p.signing_active_quorum_count))
                    })
                    .collect()
            })
            .expect("the tip's LLMQ context");
        let mut lists = BTreeMap::new();
        for (label, count) in [("default", None), ("count_4", Some(4usize))] {
            let mut answer = Map::new();
            for &(t, active) in &enabled {
                let n = count.unwrap_or(usize::from(active));
                let scanned = binding.scan_quorums(t, tip_id, n).expect("ScanQuorums");
                let hashes = scanned.iter().map(|q| json!(q.commitment.quorum_hash().to_string()));
                answer.insert(type_name(t), Value::Array(hashes.collect()));
            }
            lists.insert(label.to_owned(), Value::Object(answer));
        }
        let quorums = recorded
            .into_iter()
            .map(|(t, hash)| {
                let quorum = binding
                    .quorum(t, &hash)
                    .expect("GetQuorum")
                    .unwrap_or_else(|| panic!("route B has no quorum {t:?} {hash}"));
                let rotated = params.consensus.llmq(t).expect("configured").use_rotation;
                let prev_same_index_base = rotated
                    .then(|| {
                        q.evo_view()
                            .with_qc(q.bls(), |qc| {
                                qc.mined
                                    .nth_mined_by_index(
                                        t,
                                        quorum.base,
                                        quorum.commitment.quorum_index().unwrap_or(0),
                                        0,
                                    )
                                    .expect("the mined index")
                            })
                            .expect("the tip's LLMQ context")
                    })
                    .flatten()
                    .map(height);
                RouteB {
                    base_height: height(quorum.base),
                    quorum,
                    prev_same_index_base,
                }
            })
            .collect();
        PhaseReads {
            lists,
            quorums,
        }
    })
    .recv()
    .expect("V serves")
}

// --- Core's RPC objects ---

/// `GetNetInfoWithLegacyFields(*pdmnState, nType)` (v24.0.0 `evo/core_write.cpp:38-57`) over a
/// legacy `MnNetInfo` (`evo/netinfo.cpp:380-395`).
fn addresses(dmn: &Dmn) -> Value {
    let net_info = &*dmn.cold.net_info;
    let NetInfo::Legacy(_) = net_info else {
        panic!("{}: extended net info is not in the lane (c) fixture", dmn.pro_tx_hash);
    };
    let mut ret = Map::new();
    if is_empty(net_info) {
        return Value::Object(ret);
    }
    let addr = primary(net_info);
    ret.insert("core_p2p".into(), json!([addr.to_string()]));
    if dmn.mn_type.is_evo() {
        let at = |port| {
            let mut s = addr.clone();
            s.port = port;
            json!([s.to_string()])
        };
        ret.insert("platform_https".into(), at(dmn.cold.platform_http_port));
        ret.insert("platform_p2p".into(), at(dmn.cold.platform_p2p_port));
    }
    Value::Object(ret)
}

/// `quorum info type hash includeSkShare` (`rpc/quorums.cpp:192-252`, `:307`) over the
/// manager's keys on `quorum`.
fn info_json(
    route: &RouteB,
    quorum: &NodeQuorum,
    llmq: &dash_params::LlmqParams,
    include_sk: bool,
) -> Value {
    let members: Vec<ProTxHash> = quorum.members.iter().map(|m| m.pro_tx_hash).collect();
    let operator_keys: Vec<BlsPkValue> = quorum.members.iter().map(|m| m.operator_key()).collect();
    let view = QuorumView {
        llmq,
        commitment: &quorum.commitment,
        members: &members,
        operator_keys: &operator_keys,
    };
    let mut keys = quorum.keys.lock().unwrap();
    let info = quorum_info(
        &view,
        &mut keys,
        route.base_height,
        &quorum.mined_block,
        route.prev_same_index_base,
        true,
        include_sk,
    );
    let mut ret = Map::new();
    ret.insert("height".into(), json!(info.height));
    ret.insert("type".into(), json!(info.type_name));
    ret.insert("quorumHash".into(), json!(info.quorum_hash.to_string()));
    ret.insert("quorumIndex".into(), json!(info.quorum_index));
    ret.insert("minedBlock".into(), json!(info.mined_block.to_string()));
    if let Some(failures) = info.previous_consecutive_dkg_failures {
        ret.insert("previousConsecutiveDKGFailures".into(), json!(failures));
    }
    let member_infos = info.members.expect("includeMembers");
    assert_eq!(member_infos.len(), quorum.members.len());
    let members = member_infos
        .iter()
        .zip(quorum.members.iter())
        .map(|(m, dmn)| {
            let mut mo = Map::new();
            mo.insert("proTxHash".into(), json!(m.pro_tx_hash.to_string()));
            mo.insert("addresses".into(), addresses(dmn));
            mo.insert("pubKeyOperator".into(), json!(hex(&m.pub_key_operator.to_bytes(SCHEME))));
            mo.insert("valid".into(), json!(m.valid));
            if let Some(share) = &m.pub_key_share {
                mo.insert("pubKeyShare".into(), json!(hex(&share.to_bytes(SCHEME))));
            }
            Value::Object(mo)
        })
        .collect();
    ret.insert("members".into(), Value::Array(members));
    ret.insert("quorumPublicKey".into(), json!(hex(&info.quorum_public_key.to_bytes(SCHEME))));
    if let Some(sk) = &info.secret_key_share {
        ret.insert("secretKeyShare".into(), json!(hex(&sk.to_bytes(SCHEME))));
    }
    Value::Object(ret)
}

/// A copy of route B's object with no keys, so each node's manager builds its own.
fn fresh(quorum: &NodeQuorum) -> QuorumPtr {
    QuorumPtr(Arc::new(NodeQuorum {
        llmq_type: quorum.llmq_type,
        commitment: quorum.commitment.clone(),
        base: quorum.base,
        base_hash: quorum.base_hash,
        mined_block: quorum.mined_block,
        members: quorum.members.clone(),
        keys: Mutex::new(QuorumKeys::default()),
    }))
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
        valid: &[bool],
    ) -> Option<Contributions> {
        if !self.0.quorums.contains_key(&(llmq_type.to_base(), *base)) {
            return None;
        }
        self.0.verified(llmq_type, base, valid)
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
        for (q, route) in quorums.iter().zip(&reads.quorums) {
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
            for (q, route) in quorums.iter().zip(&reads.quorums) {
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
        for (q, route) in quorums.iter().zip(&reads.quorums) {
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
        for route in &reads.quorums {
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
        for route in &reads.quorums {
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
/// off-V executor in place of the `q-cache` thread, the executor being the call's one argument.
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
    let node = dashd_node::init::quorums::init_quorums(&setup, jobs.clone(), &mut hook);
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
        for route in &reads.quorums {
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
