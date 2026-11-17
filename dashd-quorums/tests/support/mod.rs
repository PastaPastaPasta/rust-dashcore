//! Pieces the lane (c) differentials share (`lane_c_differential.rs` over regtest,
//! `testnet_quorums.rs` over testnet): the manager's in-memory seams, route B's reads over a
//! replayed V, and Core's `quorum info` object built from the manager's keys.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use dash_consensus::BlockId;
use dash_dml::Dmn;
use dash_dml::netinfo::{is_empty, primary};
use dash_evo::llmq::{OperatorKeyView, is_quorum_type_enabled};
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::{BlsPkValue, BlsSchemeKind};
use dash_llmq::quorum::{
    CleanupCounts, QuorumDbKey, QuorumKeys, QuorumView, StoredContributions, quorum_info,
};
use dash_netaddr::{NetAddr, Service};
use dash_params::{ChainSetup, LlmqType};
use dash_primitives::{BlockHash, NIEntry, NIPurpose, NetInfo, QuorumHash};
use dashd_chainstate::chain::{ChainHandle, ChainQuery, OnV};
use dashd_chainstate::llmq::quorum::{NodeQuorum, QuorumPtr};
use dashd_quorums::{LocalMasternode, QuorumKeyStore, WarmExecutor};
use serde_json::{Map, Value, json};

/// The scheme Core's global `bls_legacy_scheme` holds past V19, which every BLS value but a
/// stored operator key prints under.
pub const SCHEME: BlsSchemeKind = BlsSchemeKind::Basic;

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// --- The manager's seams over the fixture ---

#[derive(Default)]
pub struct MemStore(Mutex<BTreeMap<QuorumDbKey, StoredContributions>>);

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

pub struct Inline;

impl WarmExecutor for Inline {
    fn spawn_warm(&self, job: Box<dyn FnOnce() + Send>) {
        job();
    }
}

pub struct Local(pub Option<ProTxHash>);

impl LocalMasternode for Local {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        self.0
    }
}

// --- Route B over the replayed V ---

/// What route B gives for one recorded quorum, with `previousConsecutiveDKGFailures`' input.
pub struct RouteB {
    pub quorum: QuorumPtr,
    pub base_height: i32,
    /// `GetLastMinedCommitmentsByQuorumIndexUntilBlock(type, base, index, 0)`'s base height.
    pub prev_same_index_base: Option<i32>,
}

/// One phase's reads at its recorded tip: `quorum list` per type for each count, and each
/// recorded quorum through `GetQuorum(type, hash)`, or why route B has none.
pub struct PhaseReads {
    pub lists: BTreeMap<String, Value>,
    pub quorums: Vec<Result<RouteB, String>>,
}

impl PhaseReads {
    /// Each recorded quorum's read, in order.
    ///
    /// # Panics
    ///
    /// If route B has no quorum for one of them.
    #[allow(dead_code, reason = "testnet_quorums.rs reports a missing quorum as a mismatch")]
    pub fn found(&self) -> impl Iterator<Item = &RouteB> {
        self.quorums.iter().map(|q| q.as_ref().unwrap_or_else(|e| panic!("{e}")))
    }
}

pub fn type_name(t: LlmqType) -> String {
    t.to_string()
}

pub fn read_phase(
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
                let Some(quorum) = binding.quorum(t, &hash).expect("GetQuorum") else {
                    return Err(format!("route B has no quorum {t:?} {hash}"));
                };
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
                Ok(RouteB {
                    base_height: height(quorum.base),
                    quorum,
                    prev_same_index_base,
                })
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

/// `GetNetInfoWithLegacyFields(*pdmnState, nType)` (v24.0.0 `evo/core_write.cpp:37-57`): the
/// net info's `ToJson()`, plus an Evo node's platform endpoints at the primary address when the
/// net info is legacy and not empty. dashd-rpc's `ProTxNet::addresses`
/// (dashd-rpc/src/core_write/evo.rs) ports the same function over ProTx payloads; it is
/// crate-private, so this mirrors it over a list entry.
pub fn addresses(dmn: &Dmn) -> Value {
    let net_info = &*dmn.cold.net_info;
    let mut ret = net_info_json(net_info);
    // `CanStorePlatform()` is true for `ExtNetInfo` only (evo/netinfo.h:356, :444).
    if !dmn.mn_type.is_evo() || is_empty(net_info) || matches!(net_info, NetInfo::Extended(_)) {
        return Value::Object(ret);
    }
    let addr = primary(net_info);
    let at = |port| {
        let mut s = addr.clone();
        s.port = port;
        json!([s.to_string()])
    };
    ret.insert("platform_https".into(), at(dmn.cold.platform_http_port));
    ret.insert("platform_p2p".into(), at(dmn.cold.platform_p2p_port));
    Value::Object(ret)
}

/// `ExtNetInfo::CURRENT_VERSION` (evo/netinfo.h:378). Core's `Unserialize` leaves an unknown
/// version without entries (:412-424); the guard mirrors dashd-rpc's `NetInfoView::groups`.
const EXT_NET_INFO_VERSION: u8 = 1;

/// `netInfo->ToJson()`: `MnNetInfo`'s `core_p2p` unless empty (evo/netinfo.cpp:380-395), or
/// `ExtNetInfo`'s purposes in map order, each entry `ToStringAddrPort()` (:656-676, :285-296).
fn net_info_json(net_info: &NetInfo) -> Map<String, Value> {
    let mut ret = Map::new();
    match net_info {
        NetInfo::Legacy(_) => {
            if !is_empty(net_info) {
                ret.insert("core_p2p".into(), json!([primary(net_info).to_string()]));
            }
        }
        NetInfo::Extended(v2) if v2.version() == EXT_NET_INFO_VERSION => {
            for (purpose, entries) in v2.groups() {
                let entries: Vec<String> = entries.iter().map(entry_addr_port).collect();
                ret.insert(purpose_to_string(*purpose).into(), json!(entries));
            }
        }
        NetInfo::Extended(_) => {}
    }
    ret
}

/// `NetInfoEntry::ToStringAddrPort()` (evo/netinfo.cpp:285-296), as dashd-rpc's
/// `entry_addr_port`: base-sdk's decoder already reads an entry Core would hold as invalid as
/// [`NIEntry::Invalid`].
fn entry_addr_port(entry: &NIEntry) -> String {
    match entry {
        NIEntry::Service(s) => Service::new(NetAddr::from_addrv2(s.addr()), s.port()).to_string(),
        NIEntry::Domain {
            name,
            port,
        } => format!("{}:{port}", String::from_utf8_lossy(name)),
        NIEntry::Invalid => "[invalid entry]".into(),
    }
}

/// `PurposeToString` (evo/netinfo.h:92-103), as dashd-rpc's `purpose_to_string`.
fn purpose_to_string(purpose: NIPurpose) -> &'static str {
    match purpose {
        NIPurpose::CoreP2p => "core_p2p",
        NIPurpose::PlatformP2p => "platform_p2p",
        NIPurpose::PlatformHttps => "platform_https",
        NIPurpose::Unknown(_) => "",
    }
}

/// `quorum info type hash includeSkShare` (`rpc/quorums.cpp:192-252`, `:307`) over the
/// manager's keys on `quorum`.
pub fn info_json(
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
            // `pubKeyOperator.ToString()` prints in the captured key's scheme, `bufLegacyScheme`
            // (v24.0.0 src/bls/bls.h:574-577). Core keeps it legacy iff `nVersion == 1`
            // (evo/dmnstate.h:107; specialtxman.cpp:63-71), but reads the scheme, not the version.
            let stored = dmn.cold.pub_key_operator.scheme();
            mo.insert("pubKeyOperator".into(), json!(hex(&m.pub_key_operator.to_bytes(stored))));
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
pub fn fresh(quorum: &NodeQuorum) -> QuorumPtr {
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
