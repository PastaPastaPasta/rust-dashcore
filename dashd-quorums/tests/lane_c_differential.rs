//! The lane (c) differential (plan §4, QM1a-3): Core v24.0.0's recorded regtest run
//! (`test-vectors/lane-c`) replayed into a real V (`dashd_node::sut::NodeSut`), its quorums read
//! through route B's binding, and each node's contributions built by [`QuorumManager`] with `C`
//! fed from the captured `qcontrib` messages and `M` that node. Every recorded `quorum list`
//! (default and `count`) and `quorum info` (both `includeSkShare` flags, every node) must equal
//! the answer Core's RPC builds from the same reads (`rpc/quorums.cpp:50-190`, `:192-252`), field
//! by field. A recorded quorum, node or call that is missing fails the test.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
use dashd_chainstate::llmq::quorum::{NodeQuorum, QuorumBuildHook, QuorumPtr};
use dashd_quorums::traits::Contributions;
use dashd_quorums::{
    LocalMasternode, QuorumKeyStore, QuorumManager, VerifiedContributions, WarmExecutor,
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

/// Every recorded `quorum list` and `quorum info` of both phases against the manager's answer at
/// the recorded tip.
#[test]
fn recorded_quorum_list_and_info_equal_the_managers_answers() {
    check_provenance();
    let fixture: Value = serde_json::from_slice(
        &std::fs::read(lane_c().join("regtest-v24.0.0.json")).expect("the fixture"),
    )
    .expect("json");
    assert_eq!(fixture["schema_version"], json!(2));
    assert_eq!(fixture["bls_scheme"], json!("basic"));
    let chain = chain();
    let tip_height = usize::try_from(fixture["tip_height"].as_u64().expect("tip_height")).unwrap();
    assert_eq!(chain.len(), tip_height + 1, "the chain holds genesis through the tip");

    // The DIP3 arguments DashTestFramework writes to every node's dash.conf, outside
    // `extra_args` (v24.0.0 test/functional/test_framework/test_framework.py:1530-1535), checked
    // against the activation heights the fixture records.
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
    let sut = dashd_node::sut::NodeSut::new(dir.path(), &argv, now, ui).expect("the SUT starts");
    let params = Arc::new(sut.params().clone());
    let v = sut.chain().expect("V runs").clone();
    let genesis = chain[0].hash();
    let has_genesis = v.on_v(move |_, q| q.chain.reader().lookup(&genesis).is_some());
    assert!(has_genesis.recv().expect("V serves"), "the fixture's genesis is V's");

    // Every MN's proTxHash and operator secret.
    let signers = fixture["signers"].as_object().expect("signers");
    let operators = fixture["operators"].as_object().expect("operators");
    let mut nodes: Vec<(String, Option<(ProTxHash, BlsSkValue)>)> = Vec::new();
    for (node, signer) in signers {
        let me = signer["proTxHash"].as_str().map(|pro| {
            let sk = operators
                .get(pro)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("node {node}: no operator secret"));
            let sk: [u8; 32] = unhex(sk).try_into().expect("32 bytes");
            let sk = BlsSkValue::from_canonical(&sk, SCHEME).expect("an operator secret");
            let mut bytes: [u8; 32] = unhex(pro).try_into().expect("32 bytes");
            bytes.reverse();
            let pro_tx_hash = ProTxHash::from(bytes);
            assert_eq!(pro_tx_hash.to_string(), pro, "display order");
            (pro_tx_hash, sk)
        });
        nodes.push((node.clone(), me));
    }
    assert_eq!(nodes.len(), 12, "twelve recorded nodes");
    assert_eq!(nodes.iter().filter(|(_, me)| me.is_some()).count(), 11, "eleven masternodes");

    let lists = fixture["quorum_list"].as_array().expect("quorum_list");
    let mut phases: Vec<(String, usize)> = lists
        .iter()
        .map(|l| {
            let height = usize::try_from(l["height"].as_u64().expect("height")).unwrap();
            (l["phase"].as_str().expect("phase").to_owned(), height)
        })
        .collect();
    phases.sort_by_key(|(_, h)| *h);
    assert_eq!(phases.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["ring", "mesh"]);

    let mut fed = 0;
    let mut compared = (0usize, 0usize, 0usize);
    for (phase, height) in &phases {
        while fed < *height {
            fed += 1;
            let r = v.process_block(chain[fed].clone(), true, None, true).recv().expect("V");
            assert!(r.accepted && r.state.is_valid(), "V at {fed}: {r:?}");
        }
        let recorded_list =
            lists.iter().find(|l| l["phase"] == json!(phase)).expect("the phase's quorum list");
        let quorums = fixture["phases"][phase]["quorums"].as_array().expect("the phase's quorums");
        assert!(!quorums.is_empty(), "{phase}: no recorded quorums");
        let recorded: Vec<(LlmqType, QuorumHash)> = quorums
            .iter()
            .map(|q| {
                let t = LlmqType::from_base(as_u8(&q["type"]));
                (t, QuorumHash::from_hex(q["quorumHash"].as_str().expect("hash")).expect("hex"))
            })
            .collect();
        let reads = read_phase(&v, &params, chain[*height].hash(), recorded);

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
                SCHEME,
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
                manager.built(&ours, true).expect("the manager's build");
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
    while fed < tip_height {
        fed += 1;
        let r = v.process_block(chain[fed].clone(), true, None, true).recv().expect("V");
        assert!(r.accepted && r.state.is_valid(), "V at {fed}: {r:?}");
    }
    eprintln!(
        "compared {} quorum list calls and {} quorum info calls over {} node runs",
        compared.0, compared.1, compared.2
    );
    assert_eq!(compared, (4, 10 * 12 * 2, 2 * 12));
}
