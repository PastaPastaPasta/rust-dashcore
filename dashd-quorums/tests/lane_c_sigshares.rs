//! Lane (c) through the sig-share runtime: Core v24.0.0's recorded offline shares recover each
//! request's signature on a member's runtime (R2), and each masternode's recorded inbound share
//! traffic replays through the runtime's entry points, whose sends keep Core's caps, announce a
//! session before referencing it and carry only shares that verify (R8). Sporks are the recorded
//! raw values (R6).

#[path = "support/shares.rs"]
mod shares;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use dash_clock::MockClock;
use dash_evo::locks::sign_hash;
use dash_evo::locks::wire::RecoveredSig;
use dash_evo::spork::is_active;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::{
    BlsCtx, BlsPkValue, BlsSchemeKind, BlsSigValue, BlsSkValue, verify_insecure,
};
use dash_llmq::dkg::wire::{
    BatchedSigShare, BatchedSigShares, SigSesAnn, SigShare, SigShareBytes, SigSharesInv,
};
use dash_llmq::sigenv::fakes::{FakeLookup, FakeNet, FakeOperatorSigner, FakeQuorum};
use dash_llmq::sigenv::{
    Now, PeerFacts, PeerId, QuorumKey, RecSigReads, SIGN_HEIGHT_OFFSET, SigMsg, SignRequest,
    SignerFacts, SporkFacts,
};
use dash_llmq::signing::api::{QuorumSigning, RecoveredSigIntake, SigningFault};
use dash_llmq::signing::intake::IntakeStatus;
use dash_llmq::signing::manager::SigningManager;
use dash_llmq::sigshares::recovery::LlmqRecovery;
use dash_p2p_core::Inventory;
use dash_params::{
    ChainOption, ChainParamsBuilder, ChainSetup, LlmqType, MAX_LLMQ_SIZE, Network, Release,
};
use dash_primitives::TxHash;
use dash_types::codec::{BaseCodec, DecodeError};
use dash_types::{CompactSize, Numeric as _};
use dashd_quorums::LocalMasternode;
use dashd_quorums::signing::{RecSigNotify, SigningHandles, SigningNet, SigningRuntime};
use dashd_quorums::sigshares::{
    SharesFacts, SharesHandles, SharesInvKind, SharesOut, SigSharesRuntime,
};
use serde_json::Value;
use shares::{Lookup, Mem, Xs, verifier};

const SCHEME: BlsSchemeKind = BlsSchemeKind::Basic;
const MAX_AGE: i64 = 604_800;

// Core: v24.0.0 src/llmq/signing_shares.h:462-464 CSigSharesManager
const MAX_MSGS_CNT_QSIGSESANN: usize = 100;
const MAX_MSGS_CNT_QSIGSHARES: usize = 200;
const MAX_MSGS_SIG_SHARES: usize = 32;
// Core: v24.0.0 src/llmq/signing_shares.h:48 MAX_MSGS_TOTAL_BATCHED_SIGS
const MAX_MSGS_TOTAL_BATCHED_SIGS: usize = MAX_LLMQ_SIZE as usize;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

/// An RPC display-order (`GetHex`) hash as stored bytes.
fn rpc(v: &Value) -> [u8; 32] {
    let mut out: [u8; 32] = unhex(v.as_str().expect("hash")).try_into().expect("32 bytes");
    out.reverse();
    out
}

fn sig_bytes(v: &Value) -> SigShareBytes {
    <[u8; 96]>::try_from(unhex(v.as_str().expect("signature"))).expect("96 bytes").into()
}

fn sig_value(b: &SigShareBytes) -> Option<BlsSigValue> {
    BlsSigValue::from_canonical(b.as_bytes(), SCHEME)
}

fn llmq_type(v: &Value) -> LlmqType {
    LlmqType::from_base(u8::try_from(v.as_u64().expect("type")).expect("a type byte"))
}

/// A recorded quorum: members in commitment order, their public key shares and each member
/// node's secret key share.
struct Recorded {
    llmq_type: LlmqType,
    quorum_hash: [u8; 32],
    members: Vec<ProTxHash>,
    quorum_public_key: BlsPkValue,
    pk_shares: BTreeMap<u16, BlsPkValue>,
    sk_shares: BTreeMap<ProTxHash, BlsSkValue>,
}

struct Lane {
    f: Value,
    chain: Arc<ChainSetup>,
    quorums: BTreeMap<QuorumKey, Recorded>,
    pro_tx: BTreeMap<u64, ProTxHash>,
}

fn lane() -> &'static Lane {
    static LANE: OnceLock<Lane> = OnceLock::new();
    LANE.get_or_init(|| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../test-vectors/lane-c/regtest-v24.0.0.json");
        let f: Value =
            serde_json::from_slice(&std::fs::read(path).expect("lane (c) fixture")).expect("json");
        let pro_tx: BTreeMap<u64, ProTxHash> = f["signers"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, s)| !s["proTxHash"].is_null())
            .map(|(n, s)| (n.parse().unwrap(), ProTxHash::from(rpc(&s["proTxHash"]))))
            .collect();
        let mut quorums = BTreeMap::new();
        let recorded = f["phases"]
            .as_object()
            .unwrap()
            .values()
            .flat_map(|p| p["quorums"].as_array().unwrap())
            .chain(f["signing_quorums"].as_object().unwrap().values());
        for q in recorded {
            let info = &q["quorum_info"];
            let variant = info["variants"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| {
                    v["members"].as_array().unwrap().iter().all(|m| m["pubKeyShare"].is_string())
                })
                .expect("a variant with public key shares");
            let members: Vec<&Value> = variant["members"].as_array().unwrap().iter().collect();
            let pk = |v: &Value| {
                let bytes: [u8; 48] = unhex(v.as_str().unwrap()).try_into().expect("48 bytes");
                BlsPkValue::from_canonical(&bytes, SCHEME).expect("a basic public key")
            };
            let sk_shares = info["nodes"]
                .as_object()
                .unwrap()
                .iter()
                .filter_map(|(n, node)| {
                    let sk = node["secretKeyShare"].as_str()?;
                    let bytes: [u8; 32] = unhex(sk).try_into().expect("32 bytes");
                    let sk = BlsSkValue::from_canonical(&bytes, SCHEME).expect("a secret share");
                    Some((pro_tx[&n.parse::<u64>().unwrap()], sk))
                })
                .collect();
            let llmq_type = llmq_type(&q["type"]);
            let quorum_hash = rpc(&q["quorumHash"]);
            let r = Recorded {
                llmq_type,
                quorum_hash,
                members: members.iter().map(|m| ProTxHash::from(rpc(&m["proTxHash"]))).collect(),
                quorum_public_key: pk(&variant["quorumPublicKey"]),
                pk_shares: (0..).zip(&members).map(|(i, m)| (i, pk(&m["pubKeyShare"]))).collect(),
                sk_shares,
            };
            quorums.insert((llmq_type.to_base(), quorum_hash.into()), r);
        }
        Lane {
            chain: Arc::new(chain(&f)),
            f,
            quorums,
            pro_tx,
        }
    })
}

/// Regtest with the capture's chain arguments (every node's quorum parameters are the same).
fn chain(f: &Value) -> ChainSetup {
    let mut builder = ChainParamsBuilder::new(Network::Regtest, Release::V24_0);
    for arg in f["provenance"]["extra_args"][0].as_array().expect("extra_args") {
        let (flag, value) = arg.as_str().unwrap().split_once('=').unwrap_or_default();
        let option = match flag {
            "-llmqtestinstantsenddip0024" => ChainOption::LlmqTestInstantSendDip0024,
            "-llmqtestparams" => ChainOption::LlmqTestParams,
            "-llmqtestinstantsendparams" => ChainOption::LlmqTestInstantSendParams,
            "-llmqtestplatformparams" => ChainOption::LlmqTestPlatformParams,
            _ => continue,
        };
        builder = builder.arg(option, value);
    }
    builder.build().expect("the capture's regtest")
}

impl Lane {
    fn quorum(&self, key: &QuorumKey, me: Option<&ProTxHash>) -> Arc<FakeQuorum> {
        let r = &self.quorums[key];
        Arc::new(FakeQuorum {
            llmq_type: r.llmq_type,
            quorum_hash: r.quorum_hash.into(),
            params: *self.chain.consensus.llmq(r.llmq_type).expect("a regtest type"),
            members: r.members.clone(),
            valid_members: (0..).take(r.members.len()).collect(),
            has_verification_vector: true,
            quorum_public_key: r.quorum_public_key.clone(),
            pk_shares: r.pk_shares.clone(),
            sk_share: me.and_then(|me| r.sk_shares.get(me).cloned()),
        })
    }

    /// Every recorded quorum, active, as `me` holds it, and each request's id selecting the
    /// quorum Core signed it with.
    fn lookup(&self, me: Option<&ProTxHash>) -> FakeLookup {
        let mut lookup = FakeLookup::default();
        for key in self.quorums.keys() {
            lookup.active.insert(*key);
            lookup.quorums.insert(*key, self.quorum(key, me));
        }
        for r in self.requests() {
            let key = Lane::request_key(r);
            let at = (key.0, rpc(&r["id"]), -1, SIGN_HEIGHT_OFFSET);
            let q = lookup.quorums[&key].clone();
            if let Some(old) = lookup.selections.insert(at, q) {
                assert_eq!(old.quorum_hash, key.1, "one recorded selection per id");
            }
        }
        lookup
    }

    /// SPORK_21's recorded raw value for a phase.
    // Core: v24.0.0 src/llmq/net_signing.cpp:66 ProcessMessage
    fn spork21(&self, phase: &str) -> i64 {
        let raw = self.f["phases"][phase]["sporks"]["SPORK_21_QUORUM_ALL_CONNECTED"].as_i64();
        raw.expect("a recorded SPORK_21 value")
    }

    fn requests(&self) -> &[Value] {
        self.f["requests"].as_array().unwrap()
    }

    fn request_key(r: &Value) -> QuorumKey {
        (llmq_type(&r["type"]).to_base(), rpc(&r["quorumHash"]).into())
    }
}

/// Node facts the replay moves: mocktime and SPORK_21's raw value. Each RNG is freshly seeded,
/// as Core's `GetRand` draws are fresh.
struct Facts {
    me: Option<ProTxHash>,
    now_ms: Mutex<i64>,
    spork21: Mutex<i64>,
    draws: AtomicU64,
}

impl SharesFacts for Facts {
    type Rng = Xs;
    fn signer(&self) -> SignerFacts {
        SignerFacts {
            my_pro_tx: self.me,
            signing_allowed: true,
        }
    }
    fn sporks(&self) -> SporkFacts {
        let value = *self.spork21.lock().unwrap();
        SporkFacts {
            spork21_active: is_active(value, self.now().secs()),
            spork21_value: value,
        }
    }
    fn now(&self) -> Now {
        Now {
            ms: *self.now_ms.lock().unwrap(),
        }
    }
    fn bls(&self) -> BlsCtx {
        BlsCtx::BASIC
    }
    fn rng(&self) -> Xs {
        Xs(self.draws.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Default)]
struct Node {
    me: Option<ProTxHash>,
    scored: Mutex<Vec<PeerId>>,
}

impl SigningNet for Node {
    fn misbehaving(&self, peer: PeerId, _: i32) {
        self.scored.lock().unwrap().push(peer);
    }
    fn relay_inv(&self, _: &Inventory) {}
    fn relay_transaction(&self, _: &TxHash) {}
}

impl RecSigNotify for Node {
    fn notify_recovered_sig(&self, _: Arc<RecoveredSig>, _: bool) {}
}

impl LocalMasternode for Node {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        self.me
    }
}

/// Every send and score, in order.
#[derive(Default)]
struct Out {
    sent: Mutex<Vec<(PeerId, SigMsg)>>,
    scored: Mutex<Vec<(PeerId, i32)>>,
}

impl SharesOut for Out {
    fn send(&self, peer: PeerId, msgs: Vec<SigMsg>) {
        self.sent.lock().unwrap().extend(msgs.into_iter().map(|m| (peer, m)));
    }
    fn misbehaving(&self, peer: PeerId, score: i32) {
        self.scored.lock().unwrap().push((peer, score));
    }
}

type Signing = SigningRuntime<Mem, Lookup>;
type Shares = SigSharesRuntime<Lookup, Recorder, FakeNet, Out, Facts, LlmqRecovery>;

/// The signing runtime as the share runtime's intake, recording every recovered sig the share
/// runtime applies.
struct Recorder {
    signing: Arc<Signing>,
    applied: Mutex<Vec<Arc<RecoveredSig>>>,
}

impl RecoveredSigIntake for Recorder {
    fn process_recovered_sig(
        &self,
        sig: Arc<RecoveredSig>,
        consider_proactive_relay: bool,
    ) -> Result<bool, SigningFault> {
        self.applied.lock().unwrap().push(sig.clone());
        self.signing.process_recovered_sig(sig, consider_proactive_relay)
    }

    fn push_reconstructed_recovered_sig(&self, sig: Arc<RecoveredSig>) {
        self.applied.lock().unwrap().push(sig.clone());
        self.signing.push_reconstructed_recovered_sig(sig);
    }

    fn remove_node(&self, peer: PeerId) {
        RecoveredSigIntake::remove_node(&*self.signing, peer);
    }
}

impl RecSigReads for Recorder {
    type Error = SigningFault;

    fn has_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        self.signing.has_for_signing(t, id, msg)
    }

    fn has_for_session(&self, sign_hash: &[u8; 32]) -> Result<bool, SigningFault> {
        self.signing.has_for_session(sign_hash)
    }
}

/// One node's share runtime over its real signing runtime.
struct Member {
    signing: Arc<Signing>,
    intake: Arc<Recorder>,
    shares: Arc<Shares>,
    facts: Arc<Facts>,
    clock: Arc<MockClock>,
    node: Arc<Node>,
    out: Arc<Out>,
    aborts: Arc<Mutex<Vec<String>>>,
}

impl Member {
    fn new(lane: &Lane, me: Option<ProTxHash>, net: FakeNet, mocktime: i64, spork21: i64) -> Self {
        Self::with_lookup(lane, me, lane.lookup(me.as_ref()), net, mocktime, spork21)
    }

    fn with_lookup(
        lane: &Lane,
        me: Option<ProTxHash>,
        lookup: FakeLookup,
        net: FakeNet,
        mocktime: i64,
        spork21: i64,
    ) -> Self {
        let lookup = Arc::new(Lookup(lookup));
        let node = Arc::new(Node {
            me,
            ..Node::default()
        });
        let clock = Arc::new(MockClock::at(mocktime));
        let signing = Arc::new(SigningRuntime::new(
            SigningManager::new(Mem::default(), lane.chain.consensus.llmq_type_platform, MAX_AGE),
            lookup.clone(),
            lane.chain.clone(),
            SigningHandles {
                net: node.clone(),
                notify: node.clone(),
                masternode: node.clone(),
                clock: clock.clone(),
            },
        ));
        let facts = Arc::new(Facts {
            me,
            now_ms: Mutex::new(mocktime * 1000),
            spork21: Mutex::new(spork21),
            draws: AtomicU64::new(3),
        });
        let out = Arc::new(Out::default());
        let aborts = Arc::new(Mutex::new(Vec::new()));
        let sink = aborts.clone();
        let intake = Arc::new(Recorder {
            signing: signing.clone(),
            applied: Mutex::new(Vec::new()),
        });
        let shares = Arc::new(SigSharesRuntime::new(SharesHandles {
            lookup,
            intake: intake.clone(),
            net: Arc::new(net),
            out: out.clone(),
            facts: facts.clone(),
            recovery: Arc::new(LlmqRecovery),
            operator: Arc::new(FakeOperatorSigner::default()),
            chain: lane.chain.clone(),
            abort: Arc::new(move |e: &str| sink.lock().unwrap().push(e.to_owned())),
        }));
        assert!(signing.set_sign_queue(shares.clone()).is_ok());
        signing.register_listener(shares.clone());
        Self {
            signing,
            intake,
            shares,
            facts,
            clock,
            node,
            out,
            aborts,
        }
    }

    fn set_mocktime(&self, t: i64) {
        self.clock.set_mocktime(t);
        *self.facts.now_ms.lock().unwrap() = t * 1000;
    }

    /// Dispatcher passes, each followed by the pending recovered sigs.
    fn settle(&self) {
        let mut rng = Xs(5);
        for _ in 0..3 {
            assert_eq!(self.shares.dispatch_pass(&verifier()), None, "no quorum goes missing");
            while self.signing.process_pending_recovered_sigs(false, &mut rng).expect("no fault") {}
        }
    }

    /// One inbound message through the runtime's share entry points, as NetSigning reads it.
    // Core: v24.0.0 src/llmq/net_signing.cpp:47-150 NetSigning::ProcessMessage
    fn receive(&self, peer: PeerId, msg: SigMsg) {
        match msg {
            SigMsg::SesAnn(m) => self.shares.on_ses_ann(peer, m),
            SigMsg::SigSharesInv(m) => {
                self.shares.on_sig_shares_inv(peer, m, SharesInvKind::Announced)
            }
            SigMsg::GetSigShares(m) => {
                self.shares.on_sig_shares_inv(peer, m, SharesInvKind::Requested)
            }
            SigMsg::BSigShares(m) => self.shares.on_batched(peer, m),
            SigMsg::SigShare(m) => {
                if self.shares.spork21_active() {
                    self.shares.on_sig_share(peer, m);
                }
            }
        }
    }

    fn assert_clean(&self) {
        assert!(self.aborts.lock().unwrap().is_empty(), "{:?}", self.aborts.lock().unwrap());
        assert!(
            self.out.scored.lock().unwrap().is_empty(),
            "{:?}",
            self.out.scored.lock().unwrap()
        );
        assert!(
            self.node.scored.lock().unwrap().is_empty(),
            "{:?}",
            self.node.scored.lock().unwrap()
        );
    }

    fn recovered(&self, t: LlmqType, id: &[u8; 32], msg: &[u8; 32]) -> Option<RecoveredSig> {
        self.signing.get_recovered_sig(t, id, msg).expect("no fault")
    }
}

fn shares_peer(member: u16) -> PeerId {
    1000 + PeerId::from(member)
}

/// The quorum's other members as verified, fully connected quorum peers.
fn quorum_net(lane: &Lane, key: &QuorumKey, me: &ProTxHash) -> FakeNet {
    let mut net = FakeNet::default();
    let members = &lane.quorums[key].members;
    for (i, m) in (0..).zip(members).filter(|(_, m)| *m != me) {
        net.connected.push(PeerFacts {
            id: shares_peer(i),
            verified_pro_tx: Some(*m),
            common_version: 0,
        });
        net.quorum_peers.entry(*key).or_default().push(shares_peer(i));
    }
    net
}

/// A recorded request: its quorum, id, message, threshold, offline shares by member and
/// recovered signature.
struct Request<'a> {
    r: &'a Value,
    key: QuorumKey,
    llmq_type: LlmqType,
    id: [u8; 32],
    msg: [u8; 32],
    shares: BTreeMap<u16, SigShareBytes>,
    spork21: i64,
    mocktime: i64,
}

impl<'a> Request<'a> {
    fn new(lane: &Lane, r: &'a Value) -> Self {
        let key = Lane::request_key(r);
        let members = &lane.quorums[&key].members;
        let shares = r["offline_shares"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(pro_tx, s)| {
                let member = u16::try_from(s["quorumMember"].as_u64().unwrap()).unwrap();
                assert_eq!(
                    members[usize::from(member)],
                    ProTxHash::from(rpc(&Value::from(pro_tx.as_str())))
                );
                (member, sig_bytes(&s["signature"]))
            })
            .collect();
        let llmq_type = llmq_type(&r["type"]);
        let threshold = lane.chain.consensus.llmq(llmq_type).unwrap().threshold;
        assert_eq!(u64::from(threshold), r["threshold"].as_u64().unwrap());
        Self {
            r,
            key,
            llmq_type,
            id: rpc(&r["id"]),
            msg: rpc(&r["msgHash"]),
            shares,
            spork21: lane.spork21(r["phase"].as_str().unwrap()),
            mocktime: r["mocktime"].as_i64().unwrap(),
        }
    }

    fn threshold(&self) -> usize {
        usize::from(lane().chain.consensus.llmq(self.llmq_type).unwrap().threshold)
    }

    fn sign_hash(&self) -> [u8; 32] {
        sign_hash(self.llmq_type, &self.key.1, &self.id, &self.msg)
    }

    fn mesh(&self) -> bool {
        SporkFacts {
            spork21_active: is_active(self.spork21, self.mocktime),
            spork21_value: self.spork21,
        }
        .all_members_connected(self.llmq_type)
    }

    fn sign_request(&self) -> SignRequest {
        SignRequest {
            llmq_type: self.llmq_type,
            id: self.id,
            msg_hash: self.msg,
            quorum_hash: Some(self.key.1),
            allow_resign: false,
            allow_diff_msg: self.llmq_type.to_base()
                == lane().chain.consensus.llmq_type_platform.to_base(),
        }
    }

    fn recsig(&self) -> SigShareBytes {
        sig_bytes(&self.r["recsig"]["sig"])
    }

    /// Feeds member shares from their own peers: QSIGSHARE under all-members-connected, else the
    /// ring's announcement and batch.
    // Core: v24.0.0 src/llmq/signing_shares.cpp:636-669 ProcessSigShare; :1107-1197 SendMessages
    fn feed(&self, m: &Member, members: &[u16]) {
        for &member in members {
            let peer = shares_peer(member);
            let sig = self.shares[&member];
            if self.mesh() {
                let share = SigShare::new(
                    self.llmq_type,
                    self.key.1,
                    member,
                    self.id.into(),
                    self.msg.into(),
                    sig,
                );
                m.receive(peer, SigMsg::SigShare(vec![share]));
            } else {
                let session = u32::from(member) + 1;
                let ann = SigSesAnn::new(
                    session,
                    self.llmq_type,
                    self.key.1,
                    self.id.into(),
                    self.msg.into(),
                );
                m.receive(peer, SigMsg::SesAnn(vec![ann]));
                let batch = BatchedSigShares::new(session, vec![BatchedSigShare::new(member, sig)]);
                m.receive(peer, SigMsg::BSigShares(vec![batch]));
            }
            m.settle();
        }
    }
}

/// Our share as the runtime first sends it: maint passes, each 10 s later, until a QBSIGSHARES
/// or QSIGSHARE carries it.
// Core: v24.0.0 src/llmq/signing_shares.cpp:1000-1083 CollectSigSharesToSendConcentrated
fn own_share(m: &Member, req: &Request<'_>, member: u16) -> SigShareBytes {
    let recovery_members = lane().chain.consensus.llmq(req.llmq_type).unwrap().recovery_members;
    let mut sessions: BTreeMap<(PeerId, u32), [u8; 32]> = BTreeMap::new();
    for _ in 0..=recovery_members {
        m.shares.maint_pass();
        for (peer, msg) in m.out.sent.lock().unwrap().drain(..) {
            match msg {
                SigMsg::SesAnn(anns) => {
                    for a in anns {
                        sessions.insert((peer, a.session_id()), sign_hash_of_ann(&a));
                    }
                }
                SigMsg::BSigShares(batches) => {
                    for b in batches {
                        if sessions.get(&(peer, b.session_id())) != Some(&req.sign_hash()) {
                            continue;
                        }
                        if let Some(s) = b.sig_shares().iter().find(|s| s.quorum_member() == member)
                        {
                            return s.sig_share();
                        }
                    }
                }
                SigMsg::SigShare(v) => {
                    if let Some(s) = v.iter().find(|s| {
                        s.quorum_member() == member && share_sign_hash(s) == req.sign_hash()
                    }) {
                        return s.sig_share();
                    }
                }
                SigMsg::GetSigShares(_) | SigMsg::SigSharesInv(_) => {}
            }
        }
        let now = m.facts.now().secs();
        m.set_mocktime(now + 10);
    }
    panic!("member {member} never sent its share");
}

fn sign_hash_of_ann(a: &SigSesAnn) -> [u8; 32] {
    sign_hash(a.llmq_type(), &a.quorum_hash(), a.id().as_bytes(), a.msg_hash().as_bytes())
}

fn share_sign_hash(s: &SigShare) -> [u8; 32] {
    sign_hash(s.llmq_type(), &s.quorum_hash(), s.id().as_bytes(), s.msg_hash().as_bytes())
}

#[test]
fn r2_offline_shares_recover_through_the_share_runtime() {
    let lane = lane();
    assert_eq!(lane.requests().len(), 55);
    let (mut ring, mut mesh) = (0, 0);
    for (i, r) in lane.requests().iter().enumerate() {
        let req = Request::new(lane, r);
        let members = &lane.quorums[&req.key].members;
        let me_index = u16::try_from(i % members.len()).unwrap();
        let me = members[usize::from(me_index)];
        let others: Vec<u16> = req.shares.keys().copied().filter(|&m| m != me_index).collect();
        let (t, threshold) = (req.llmq_type, req.threshold());
        assert!(others.len() >= threshold);

        let m =
            Member::new(lane, Some(me), quorum_net(lane, &req.key, &me), req.mocktime, req.spork21);
        req.feed(&m, &others[..threshold - 1]);
        assert_eq!(m.recovered(t, &req.id, &req.msg), None, "request {i}: below threshold");
        req.feed(&m, &others[threshold - 1..]);
        let sig = m.recovered(t, &req.id, &req.msg).unwrap_or_else(|| panic!("request {i}"));
        assert_eq!(SigShareBytes::from(sig.sig), req.recsig(), "request {i}");
        assert_eq!(sig.sign_hash(), req.sign_hash());
        m.assert_clean();

        let m =
            Member::new(lane, Some(me), quorum_net(lane, &req.key, &me), req.mocktime, req.spork21);
        assert!(m.signing.async_sign_if_member(&req.sign_request()).unwrap(), "request {i}");
        m.settle();
        assert_eq!(own_share(&m, &req, me_index), req.shares[&me_index], "request {i}: our share");
        assert_eq!(m.recovered(t, &req.id, &req.msg), None);
        req.feed(&m, &others[..threshold - 1]);
        let sig = m.recovered(t, &req.id, &req.msg).unwrap_or_else(|| panic!("request {i}"));
        assert_eq!(SigShareBytes::from(sig.sig), req.recsig(), "request {i}: our share counts");
        m.assert_clean();
        if req.mesh() {
            mesh += 1;
        } else {
            ring += 1;
        }
    }
    assert!(ring > 0 && mesh > 0, "{ring} ring, {mesh} mesh");
}

#[test]
fn a_wrong_own_share_fails_the_recovery_self_verify() {
    let lane = lane();
    let req = Request::new(lane, &lane.requests()[0]);
    let members = &lane.quorums[&req.key].members;
    let (me, other) = (members[0], members[1]);
    let mut lookup = lane.lookup(Some(&me));
    lookup.quorums.insert(req.key, lane.quorum(&req.key, Some(&other)));
    let net = quorum_net(lane, &req.key, &me);
    let m = Member::with_lookup(lane, Some(me), lookup, net, req.mocktime, req.spork21);
    assert!(m.signing.async_sign_if_member(&req.sign_request()).unwrap());
    m.settle();
    let others: Vec<u16> = (1..).take(req.threshold() - 1).collect();
    req.feed(&m, &others);
    assert_eq!(m.recovered(req.llmq_type, &req.id, &req.msg), None, "the self-verify refuses it");
    m.assert_clean();
}

// --- R8 ---

/// Core's vector serialization: a CompactSize count, then the items.
fn read_vec<T: BaseCodec>(mut data: &[u8]) -> Result<Vec<T>, DecodeError> {
    let n = CompactSize::decode(&mut data)?.into_len(usize::MAX)?;
    let v = (0..n).map(|_| T::decode(&mut data)).collect::<Result<_, _>>()?;
    assert!(data.is_empty(), "trailing bytes");
    Ok(v)
}

/// A recorded share message's payload, read as the runtime's entry points take it.
fn decode(command: &str, payload: &[u8]) -> Option<SigMsg> {
    Some(match command {
        "qsigsesann" => SigMsg::SesAnn(read_vec(payload).unwrap()),
        "qsigsinv" => SigMsg::SigSharesInv(read_vec::<SigSharesInv>(payload).unwrap()),
        "qgetsigs" => SigMsg::GetSigShares(read_vec::<SigSharesInv>(payload).unwrap()),
        "qbsigs" => SigMsg::BSigShares(read_vec(payload).unwrap()),
        "qsigshare" => SigMsg::SigShare(read_vec(payload).unwrap()),
        _ => return None,
    })
}

/// Core's cap for a share message.
// Core: v24.0.0 src/llmq/signing_shares.cpp:1201-1297 SendMessages
fn cap_violation(msg: &SigMsg) -> Option<String> {
    let (what, n, cap) = match msg {
        SigMsg::SesAnn(v) => ("qsigsesann", v.len(), MAX_MSGS_CNT_QSIGSESANN),
        SigMsg::GetSigShares(v) => ("qgetsigs", v.len(), MAX_MSGS_CNT_QSIGSHARES),
        SigMsg::SigSharesInv(v) => ("qsigsinv", v.len(), MAX_MSGS_CNT_QSIGSHARES),
        SigMsg::BSigShares(v) => (
            "qbsigs inner",
            v.iter().map(|b| b.sig_shares().len()).sum(),
            MAX_MSGS_TOTAL_BATCHED_SIGS,
        ),
        SigMsg::SigShare(v) => ("qsigshare", v.len(), MAX_MSGS_SIG_SHARES),
    };
    (n > cap).then(|| format!("{what}: {n} over {cap}"))
}

/// One directed pair's sessions, renamed to their first-seen ordinals.
#[derive(Default)]
struct Pair {
    ordinals: BTreeMap<u32, usize>,
    announced: Vec<SigSesAnn>,
}

impl Pair {
    fn ordinal(&mut self, session: u32) -> usize {
        let next = self.ordinals.len();
        *self.ordinals.entry(session).or_insert(next)
    }
}

/// PLAN-SG1 R8's invariants over one sender's messages, per directed pair.
struct R8<'a> {
    lane: &'a Lane,
    pairs: BTreeMap<(u64, PeerId), Pair>,
    errors: Vec<String>,
    shares_verified: usize,
    commands: BTreeMap<&'static str, usize>,
}

impl<'a> R8<'a> {
    fn new(lane: &'a Lane) -> Self {
        Self {
            lane,
            pairs: BTreeMap::new(),
            errors: Vec::new(),
            shares_verified: 0,
            commands: BTreeMap::new(),
        }
    }

    fn verify(
        &mut self,
        t: LlmqType,
        qh: [u8; 32],
        id: [u8; 32],
        msg: [u8; 32],
        member: u16,
        sig: &SigShareBytes,
    ) {
        let hash = sign_hash(t, &qh.into(), &id, &msg);
        let pk =
            self.lane.quorums.get(&(t.to_base(), qh.into())).and_then(|q| q.pk_shares.get(&member));
        match (pk, sig_value(sig)) {
            (Some(pk), Some(sig)) if verify_insecure(&sig, pk, &hash, SCHEME) => {
                self.shares_verified += 1
            }
            _ => self
                .errors
                .push(format!("share of member {member} for {hash:02x?} does not verify")),
        }
    }

    fn reference(&mut self, pair: (u64, PeerId), session: u32) -> Option<SigSesAnn> {
        let p = self.pairs.entry(pair).or_default();
        let o = p.ordinal(session);
        let ann = p.announced.get(o).cloned();
        if ann.is_none() {
            self.errors.push(format!("{pair:?}: session #{o} referenced before its qsigsesann"));
        }
        ann
    }

    fn check(&mut self, pair: (u64, PeerId), msg: &SigMsg) {
        if let Some(e) = cap_violation(msg) {
            self.errors.push(format!("{pair:?}: {e}"));
        }
        let command = match msg {
            SigMsg::SesAnn(anns) => {
                let p = self.pairs.entry(pair).or_default();
                for a in anns {
                    let o = p.ordinal(a.session_id());
                    if o == p.announced.len() {
                        p.announced.push(a.clone());
                    } else if p.announced.get(o) != Some(a) {
                        self.errors.push(format!("{pair:?}: session #{o} announced out of order"));
                    }
                }
                "qsigsesann"
            }
            SigMsg::GetSigShares(invs) | SigMsg::SigSharesInv(invs) => {
                for inv in invs {
                    self.reference(pair, inv.session_id());
                }
                if matches!(msg, SigMsg::GetSigShares(_)) {
                    "qgetsigs"
                } else {
                    "qsigsinv"
                }
            }
            SigMsg::BSigShares(batches) => {
                for b in batches {
                    let Some(a) = self.reference(pair, b.session_id()) else {
                        continue;
                    };
                    for s in b.sig_shares() {
                        let (id, m) = (*a.id().as_bytes(), *a.msg_hash().as_bytes());
                        self.verify(
                            a.llmq_type(),
                            *a.quorum_hash().as_bytes(),
                            id,
                            m,
                            s.quorum_member(),
                            &s.sig_share(),
                        );
                    }
                }
                "qbsigs"
            }
            SigMsg::SigShare(v) => {
                for s in v {
                    let (qh, id, m) =
                        (*s.quorum_hash().as_bytes(), *s.id().as_bytes(), *s.msg_hash().as_bytes());
                    self.verify(s.llmq_type(), qh, id, m, s.quorum_member(), &s.sig_share());
                }
                "qsigshare"
            }
        };
        *self.commands.entry(command).or_default() += 1;
    }
}

fn messages(lane: &Lane) -> impl Iterator<Item = &Value> {
    lane.f["sig_messages"].as_array().unwrap().iter()
}

fn u(v: &Value) -> u64 {
    v.as_u64().unwrap()
}

/// Every node's recorded share messages, sender by sender in each receiver's receive order.
fn core_checker(lane: &Lane) -> R8<'_> {
    let mut sent: Vec<&Value> = messages(lane).collect();
    sent.sort_by_key(|m| (u(&m["from"]), u(&m["to"]), u(&m["receive_pos"])));
    let mut r8 = R8::new(lane);
    for m in sent {
        let payload = unhex(m["payload"].as_str().unwrap());
        if let Some(msg) = decode(m["command"].as_str().unwrap(), &payload) {
            r8.check((u(&m["from"]), m["to"].as_i64().unwrap()), &msg);
        }
    }
    r8
}

#[test]
fn the_r8_checker_accepts_core_s_recorded_share_traffic() {
    let lane = lane();
    let r8 = core_checker(lane);
    assert!(r8.errors.is_empty(), "{:#?}", r8.errors);
    let mut recorded = BTreeMap::new();
    for m in messages(lane).filter(|m| m["command"] != "qsigrec") {
        let c = m["command"].as_str().unwrap();
        let c = r8.commands.keys().find(|k| **k == c).copied().unwrap_or("unchecked");
        *recorded.entry(c).or_insert(0) += 1;
    }
    assert_eq!(r8.commands, recorded);
    assert!(r8.shares_verified > r8.commands["qbsigs"]);
}

#[test]
fn the_r8_checker_rejects_what_core_never_sends() {
    let lane = lane();
    let ann =
        |s| SigSesAnn::new(s, LlmqType::LlmqTest, [1; 32].into(), [2; 32].into(), [3; 32].into());
    let inv =
        || SigSharesInv::new(1, dash_llmq::dkg::wire::SigSharesBits::new(8, vec![1]).unwrap());
    let share = || {
        SigShare::new(
            LlmqType::LlmqTest,
            [1; 32].into(),
            0,
            [2; 32].into(),
            [3; 32].into(),
            [0; 96].into(),
        )
    };
    let batch =
        |n: usize| BatchedSigShares::new(1, vec![BatchedSigShare::new(0, [0; 96].into()); n]);
    // Core: v24.0.0 src/llmq/signing_shares.h:48, :462-464 (independent of the checker's consts)
    let sized = |ses: u32, invs: usize, inner: usize, shares: usize| {
        [
            SigMsg::SesAnn((0..ses).map(ann).collect()),
            SigMsg::GetSigShares(vec![inv(); invs]),
            SigMsg::SigSharesInv(vec![inv(); invs]),
            SigMsg::BSigShares(vec![batch(inner - 1), batch(1)]),
            SigMsg::SigShare(vec![share(); shares]),
        ]
    };
    for msg in sized(100, 200, 400, 32) {
        assert_eq!(cap_violation(&msg), None);
    }
    for msg in sized(101, 201, 401, 33) {
        assert!(cap_violation(&msg).is_some());
    }

    let mut sent: Vec<&Value> = messages(lane).filter(|m| m["command"] == "qbsigs").collect();
    sent.sort_by_key(|m| u(&m["receive_pos"]));
    let first = sent[0];
    let pair = (u(&first["from"]), first["to"].as_i64().unwrap());
    let Some(SigMsg::BSigShares(mut batches)) =
        decode("qbsigs", &unhex(first["payload"].as_str().unwrap()))
    else {
        unreachable!()
    };
    let mut r8 = R8::new(lane);
    r8.check(pair, &SigMsg::BSigShares(batches.clone()));
    assert_eq!(r8.errors.len(), batches.len(), "a batch before its announcement");

    let anns = messages(lane)
        .filter(|m| {
            m["command"] == "qsigsesann"
                && u(&m["from"]) == pair.0
                && m["to"].as_i64() == Some(pair.1)
        })
        .filter(|m| u(&m["receive_pos"]) < u(&first["receive_pos"]))
        .map(|m| decode("qsigsesann", &unhex(m["payload"].as_str().unwrap())).unwrap())
        .collect::<Vec<_>>();
    let mut r8 = R8::new(lane);
    for a in &anns {
        r8.check(pair, a);
    }
    r8.check(pair, &SigMsg::BSigShares(batches.clone()));
    assert!(r8.errors.is_empty(), "{:?}", r8.errors);
    let s = &batches[0].sig_shares()[0];
    let mut bytes = *s.sig_share().as_bytes();
    bytes[95] ^= 1;
    let flipped = BatchedSigShare::new(s.quorum_member(), bytes.into());
    let mut shares = batches[0].sig_shares().clone();
    shares[0] = flipped;
    batches[0] = BatchedSigShares::new(batches[0].session_id(), shares);
    let mut r8 = R8::new(lane);
    for a in &anns {
        r8.check(pair, a);
    }
    r8.check(pair, &SigMsg::BSigShares(batches));
    assert_eq!(r8.errors.len(), 1, "{:?}", r8.errors);
}

/// One recorded node's inputs in its own log order: mocktime, sporks, its submits and each
/// inbound share or recovered-sig message.
enum Step<'a> {
    Mocktime(i64),
    Spork(i64),
    Submit(&'a Value),
    Message(&'a Value),
}

fn timeline(lane: &Lane, node: u64) -> Vec<Step<'_>> {
    let mut steps: Vec<(u64, u8, Step<'_>)> = Vec::new();
    for e in lane.f["events"][node.to_string()].as_array().unwrap() {
        let pos = u(&e["pos"]);
        match e["kind"].as_str().unwrap() {
            "setmocktime" => steps.push((pos, 0, Step::Mocktime(e["mocktime"].as_i64().unwrap()))),
            "spork" => {
                let v = e["values"]["SPORK_21_QUORUM_ALL_CONNECTED"].as_i64().unwrap();
                steps.push((pos, 0, Step::Spork(v)));
            }
            "sign_rpc" if e["params"].as_array().unwrap().last() == Some(&Value::Bool(true)) => {
                steps.push((pos, 0, Step::Submit(e)));
            }
            _ => {}
        }
    }
    for m in messages(lane).filter(|m| u(&m["to"]) == node) {
        steps.push((u(&m["receive_pos"]), 1, Step::Message(m)));
    }
    steps.sort_by_key(|(pos, order, _)| (*pos, *order));
    steps.into_iter().map(|(_, _, s)| s).collect()
}

/// The node's peers as it recorded them, each with its sender's MNAUTH identity, and a quorum's
/// peers its connected members.
fn replay_net(lane: &Lane, node: u64) -> FakeNet {
    let mut peers = BTreeMap::new();
    for m in messages(lane).filter(|m| u(&m["to"]) == node) {
        peers.insert(m["peer"].as_i64().unwrap(), u(&m["from"]));
    }
    let mut net = FakeNet::default();
    for (&peer, from) in &peers {
        let pro_tx = lane.pro_tx.get(from).copied();
        net.connected.push(PeerFacts {
            id: peer,
            verified_pro_tx: pro_tx,
            common_version: 0,
        });
        for (key, q) in &lane.quorums {
            if pro_tx.is_some_and(|p| q.members.contains(&p)) {
                net.quorum_peers.entry(*key).or_default().push(peer);
            }
        }
    }
    net
}

fn submit(lane: &Lane, e: &Value) -> SignRequest {
    let p = e["params"].as_array().unwrap();
    let platform = lane.chain.consensus.llmq_type_platform;
    let (t, rest) = match e["method"].as_str().unwrap() {
        "platformsign" => (platform, &p[..]),
        _ => (llmq_type(&p[0]), &p[1..]),
    };
    SignRequest {
        llmq_type: t,
        id: rpc(&rest[0]),
        msg_hash: rpc(&rest[1]),
        quorum_hash: (rest[2] != "").then(|| rpc(&rest[2]).into()),
        allow_resign: false,
        allow_diff_msg: t.to_base() == platform.to_base(),
    }
}

/// Replays `node`'s recorded inputs, inbound `qsigrec` only if `qsigrec`; returns its runtime and
/// its sends, per peer, in order.
fn replay(lane: &Lane, node: u64, qsigrec: bool) -> (Member, Vec<(PeerId, SigMsg)>) {
    let me = lane.pro_tx[&node];
    let steps = timeline(lane, node);
    let first_spork = steps.iter().find_map(|s| match s {
        Step::Spork(v) => Some(*v),
        _ => None,
    });
    let m = Member::new(lane, Some(me), replay_net(lane, node), 0, first_spork.expect("a spork"));
    let mut sent = Vec::new();
    let mut rng = Xs(7);
    for step in steps {
        match step {
            Step::Mocktime(t) => m.set_mocktime(t),
            Step::Spork(v) => *m.facts.spork21.lock().unwrap() = v,
            Step::Submit(e) => {
                m.set_mocktime(e["mocktime"].as_i64().unwrap());
                m.signing.async_sign_if_member(&submit(lane, e)).expect("no fault");
            }
            Step::Message(msg) => {
                m.set_mocktime(msg["mocktime"].as_i64().unwrap());
                let peer = msg["peer"].as_i64().unwrap();
                let payload = unhex(msg["payload"].as_str().unwrap());
                let command = msg["command"].as_str().unwrap();
                match decode(command, &payload) {
                    Some(msg) => m.receive(peer, msg),
                    None if !qsigrec => {
                        assert_eq!(command, "qsigrec");
                        continue;
                    }
                    None => {
                        assert_eq!(command, "qsigrec");
                        let status = m.signing.on_qsigrec(peer, &payload, false).1;
                        assert!(
                            matches!(status, Ok(IntakeStatus::Queued | IntakeStatus::AlreadyKnown)),
                            "node {node}: qsigrec at {}: {status:?}",
                            msg["receive_pos"]
                        );
                        while m
                            .signing
                            .process_pending_recovered_sigs(false, &mut rng)
                            .expect("no fault")
                        {}
                    }
                }
            }
        }
        m.settle();
        m.shares.maint_pass();
        sent.append(&mut m.out.sent.lock().unwrap());
    }
    (m, sent)
}

/// Each recovered sig the share runtime applied, which must be its request's recorded `recsig`;
/// returns their sign hashes.
fn share_recoveries(
    node: u64,
    m: &Member,
    requests: &BTreeMap<[u8; 32], Request<'_>>,
) -> BTreeSet<[u8; 32]> {
    let mut recovered = BTreeSet::new();
    for sig in m.intake.applied.lock().unwrap().iter() {
        let req = requests.get(&sig.sign_hash()).unwrap_or_else(|| {
            panic!("node {node}: a share recovery for no recorded request: {sig:?}")
        });
        assert_eq!(SigShareBytes::from(sig.sig), req.recsig(), "node {node}: {}", req.r["id"]);
        assert_eq!(sig.quorum_hash, req.key.1, "node {node}: {}", req.r["id"]);
        recovered.insert(sig.sign_hash());
    }
    recovered
}

/// The sign hashes `node` recorded recovering from shares itself.
// Core: v24.0.0 src/llmq/signing_shares.cpp:681-762 TryRecoverSig
fn local_recoveries(lane: &Lane, node: u64) -> BTreeSet<[u8; 32]> {
    lane.f["events"][node.to_string()]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "local_recovery")
        .map(|e| rpc(&e["sign_hash"]))
        .collect()
}

#[test]
fn every_masternode_s_replay_keeps_core_s_share_invariants() {
    let lane = lane();
    let requests: BTreeMap<[u8; 32], Request<'_>> = lane
        .requests()
        .iter()
        .map(|r| {
            let req = Request::new(lane, r);
            assert_eq!(req.sign_hash(), rpc(&r["signHash"]), "{}", r["id"]);
            (req.sign_hash(), req)
        })
        .collect();
    assert_eq!(requests.len(), 55);
    let mut totals = BTreeMap::new();
    for &node in lane.pro_tx.keys() {
        let (m, sent) = replay(lane, node, true);
        m.assert_clean();
        for req in requests.values() {
            let sig = m.recovered(req.llmq_type, &req.id, &req.msg);
            assert_eq!(
                Value::Bool(sig.is_some()),
                req.r["hasrecsig"][node.to_string()],
                "node {node}: {}",
                req.r["id"]
            );
            if let Some(sig) = sig {
                assert_eq!(
                    SigShareBytes::from(sig.sig),
                    req.recsig(),
                    "node {node}: {}",
                    req.r["id"]
                );
                assert_eq!(sig.sign_hash(), req.sign_hash(), "node {node}: {}", req.r["id"]);
            }
        }
        *totals.entry("share recoveries with qsigrec").or_insert(0) +=
            share_recoveries(node, &m, &requests).len();
        let mut r8 = R8::new(lane);
        for (peer, msg) in &sent {
            r8.check((node, *peer), msg);
        }
        assert!(r8.errors.is_empty(), "node {node}: {:#?}", r8.errors);
        for (c, n) in r8.commands {
            *totals.entry(c).or_insert(0) += n;
        }
        *totals.entry("shares verified").or_insert(0) += r8.shares_verified;

        let (m, _) = replay(lane, node, false);
        m.assert_clean();
        let recovered = share_recoveries(node, &m, &requests);
        let local = local_recoveries(lane, node);
        let missing: Vec<&Value> =
            local.difference(&recovered).map(|h| &requests[h].r["id"]).collect();
        assert!(missing.is_empty(), "node {node}: not recovered from shares: {missing:?}");
        for h in &local {
            let req = &requests[h];
            let sig = m.recovered(req.llmq_type, &req.id, &req.msg).expect("stored");
            assert_eq!(SigShareBytes::from(sig.sig), req.recsig(), "node {node}: {}", req.r["id"]);
            let phase = if req.mesh() {
                "local mesh recoveries"
            } else {
                "local ring recoveries"
            };
            *totals.entry(phase).or_insert(0) += 1;
        }
    }
    for c in [
        "qsigsesann",
        "qsigsinv",
        "qbsigs",
        "qsigshare",
        "shares verified",
        "local ring recoveries",
        "local mesh recoveries",
    ] {
        assert!(totals.get(c).copied().unwrap_or(0) > 0, "no {c}: {totals:?}");
    }
}

#[test]
fn spork_facts_take_each_phase_s_recorded_raw_value() {
    let lane = lane();
    let timeline = lane.f["provenance"]["spork_timeline"].as_array().unwrap();
    assert_eq!(timeline.len(), 2);
    for entry in timeline {
        let phase = entry["phase"].as_str().unwrap();
        let raw = entry["values"]["SPORK_21_QUORUM_ALL_CONNECTED"].as_i64().unwrap();
        assert_eq!(lane.spork21(phase), raw);
        let facts = Facts {
            me: None,
            now_ms: Mutex::new(entry["mocktime"].as_i64().unwrap() * 1000),
            spork21: Mutex::new(raw),
            draws: AtomicU64::new(3),
        };
        let sporks = facts.sporks();
        assert_eq!(sporks.spork21_value, raw);
        let all_connected = lane.f["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["phase"] == phase)
            .map(|r| sporks.all_members_connected(llmq_type(&r["type"])))
            .collect::<BTreeSet<_>>();
        let expected = phase == "mesh";
        assert_eq!(sporks.spork21_active, expected, "{phase}");
        assert_eq!(all_connected, [expected].into(), "{phase}");
        for node in lane.f["events"].as_object().unwrap().values() {
            let recorded = node
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "spork" && e["values"] == entry["values"]);
            assert_eq!(recorded.count(), 1);
        }
    }
}
