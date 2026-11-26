//! A scripted chain and host for the intake, framed payloads and real messages.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use dash_evo::types::ProTxHash;
use dash_llmq::dkg::conn::ConnSnapshot;
use dash_llmq::dkg::reference::{Network, seeded_rng};
use dash_llmq::dkg::rng::SeededRng;
use dash_llmq::dkg::session::{DkgInv, DkgMsgKind};
use dash_llmq::dkg::wire::{Complaint, Contribution, Justification, PrematureCommitment};
use dash_llmq::qdata::PeerId;
use dash_params::{LlmqParams, LlmqType};
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric;
use tracing::debug;

use super::super::fakes::{FakeSporks, Trace, params, seams};
use crate::dkg::intake::DkgIntake;
use crate::dkg::seams::{
    BaseFacts, DkgChain, GetDataResponse, IntakeHost, RelayFacts, TipFacts, TipRef,
};
use crate::dkg::{DkgSessions, SessionKey, SessionSeams};

pub(super) const TEST: LlmqType = LlmqType::LlmqTest;

pub(super) const KINDS: [DkgMsgKind; 4] = [
    DkgMsgKind::Contribution,
    DkgMsgKind::Complaint,
    DkgMsgKind::Justification,
    DkgMsgKind::PrematureCommitment,
];

pub(super) fn key(llmq_type: LlmqType, quorum_index: i32) -> SessionKey {
    SessionKey {
        llmq_type,
        quorum_index,
    }
}

pub(super) fn pro_tx(n: u8) -> ProTxHash {
    ProTxHash::from([n; 32])
}

/// A base block's quorum hash, by its height.
pub(super) fn base_hash(height: i32) -> QuorumHash {
    let mut bytes = [0xcc; 32];
    bytes[..4].copy_from_slice(&height.to_le_bytes());
    QuorumHash::from(bytes)
}

/// A chain that knows the bases it was given, counting its lookups.
#[derive(Default)]
pub(super) struct IntakeChain {
    pub(super) bases: Mutex<BTreeMap<[u8; 32], i32>>,
    pub(super) rotation: AtomicBool,
    pub(super) lookups: AtomicUsize,
}

impl IntakeChain {
    pub(super) fn add(&self, height: i32) -> QuorumHash {
        let hash = base_hash(height);
        self.bases.lock().unwrap().insert(*hash.as_bytes(), height);
        hash
    }
}

impl DkgChain for IntakeChain {
    fn snapshot_unvalidated(&self) -> bool {
        false
    }
    fn dip3_enforced(&self, _: i32) -> bool {
        true
    }
    fn lookup(&self, hash: &QuorumHash) -> Option<BaseFacts> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        let height = *self.bases.lock().unwrap().get(hash.as_bytes())?;
        Some(TipRef {
            height,
            hash: BlockHash::from(*hash.as_bytes()),
        })
    }
    fn ancestor_hash(&self, _: &TipFacts, height: i32) -> BlockHash {
        BlockHash::from(*base_hash(height).as_bytes())
    }
    fn rotation_enabled(&self, _: &LlmqParams, _: &TipRef) -> bool {
        self.rotation.load(Ordering::SeqCst)
    }
    fn v19_after(&self, _: &TipRef) -> bool {
        true
    }
    fn current_tip(&self) -> Option<TipFacts> {
        None
    }
}

/// A host that answers as scripted and records each call, also as a `host` debug line.
pub(super) struct FakeHost {
    pub(super) verified: Mutex<HashMap<PeerId, ProTxHash>>,
    pub(super) response: Mutex<GetDataResponse>,
    pub(super) type_enabled: AtomicBool,
    pub(super) type_read_fails: AtomicBool,
    pub(super) facts: Mutex<RelayFacts>,
    pub(super) calls: Mutex<Vec<String>>,
}

impl Default for FakeHost {
    fn default() -> Self {
        Self {
            verified: Mutex::default(),
            response: Mutex::new(GetDataResponse::Requested),
            type_enabled: AtomicBool::new(true),
            type_read_fails: AtomicBool::new(false),
            facts: Mutex::default(),
            calls: Mutex::default(),
        }
    }
}

impl FakeHost {
    fn call(&self, what: String) {
        debug!(target: "host", "{what}");
        self.calls.lock().unwrap().push(what);
    }

    pub(super) fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }

    pub(super) fn verify(&self, peer: PeerId, pro_tx: ProTxHash) {
        self.verified.lock().unwrap().insert(peer, pro_tx);
    }
}

impl IntakeHost for FakeHost {
    fn verified_pro_tx(&self, peer: PeerId) -> Option<ProTxHash> {
        self.call(format!("verified {peer}"));
        self.verified.lock().unwrap().get(&peer).copied()
    }
    fn consume_getdata_response(&self, peer: PeerId, inv: DkgInv) -> GetDataResponse {
        self.call(format!("consume {peer} {:?}", inv.kind));
        *self.response.lock().unwrap()
    }
    fn quorum_type_enabled(&self, t: LlmqType, base: &BaseFacts) -> Option<bool> {
        self.call(format!("enabled {} {}", t.to_base(), base.height));
        (!self.type_read_fails.load(Ordering::SeqCst))
            .then(|| self.type_enabled.load(Ordering::SeqCst))
    }
    fn misbehaving(&self, peer: PeerId, score: i32) {
        self.call(format!("score {peer} {score}"));
    }
    fn relay_facts(&self, t: LlmqType, _: &QuorumHash) -> RelayFacts {
        self.call(format!("facts {}", t.to_base()));
        self.facts.lock().unwrap().clone()
    }
    fn push_inventory(&self, peer: PeerId, inv: DkgInv) {
        self.call(format!("push {peer} {:?}", inv.kind));
    }
}

/// An intake over scripted seams, spork 17 on.
pub(super) struct Rig {
    pub(super) intake: DkgIntake,
    pub(super) sessions: Arc<DkgSessions>,
    pub(super) seams: SessionSeams,
    pub(super) chain: Arc<IntakeChain>,
    pub(super) sporks: Arc<FakeSporks>,
    pub(super) host: Arc<FakeHost>,
}

impl Rig {
    /// An active masternode's intake of `types`.
    pub(super) fn new(types: &[LlmqType]) -> Self {
        Self::with(types, true)
    }

    /// An observer's intake of `llmq_test`.
    pub(super) fn observer() -> Self {
        Self::with(&[TEST], false)
    }

    fn with(types: &[LlmqType], active: bool) -> Self {
        let trace = Arc::new(Trace::default());
        let chain = Arc::new(IntakeChain::default());
        let (base, _, sporks) = seams(&trace);
        let seams = SessionSeams {
            chain: chain.clone(),
            sporks: sporks.clone(),
            cache: base.cache,
        };
        let llmqs: Vec<_> = types.iter().map(|t| (*t, params(*t))).collect();
        let sessions = Arc::new(if active {
            DkgSessions::new(llmqs.clone(), seams.clone())
        } else {
            DkgSessions::observer(llmqs.clone(), seams.clone())
        });
        let host = Arc::new(FakeHost::default());
        let intake = DkgIntake::new(sessions.clone(), llmqs, seams.clone(), host.clone());
        Self {
            intake,
            sessions,
            seams,
            chain,
            sporks,
            host,
        }
    }
}

/// A payload of `kind` for a `llmq_test`-sized quorum (size 3, minSize 2, threshold 2) of type
/// byte `t` at `quorum_hash`, well framed, with `salt` in its proTxHash; its BLS fields are
/// zeros.
pub(super) fn framed(kind: DkgMsgKind, t: u8, quorum_hash: &QuorumHash, salt: u8) -> Vec<u8> {
    let mut out = vec![t];
    out.extend(quorum_hash.as_bytes());
    out.extend([salt; 32]);
    match kind {
        DkgMsgKind::Contribution => {
            out.push(2);
            out.extend([0; 2 * 48 + 48 + 32]);
            out.extend([2, 0, 0]);
        }
        DkgMsgKind::Complaint => out.extend([3, 0, 3, 0]),
        DkgMsgKind::Justification => out.push(0),
        _ => {
            out.extend([3, 0]);
            out.extend([0; 48 + 32 + 96]);
        }
    }
    out.extend([0; 96]);
    out
}

/// One recorded message of each kind from a `llmq_test` round of three members in which member
/// 0 lies to member 1, so that member 1 complains and member 0 justifies.
pub(super) struct Round {
    pub(super) contribution: Contribution,
    pub(super) complaint: Complaint,
    pub(super) justification: Justification,
    pub(super) pcommit: PrematureCommitment,
}

/// A fresh three-member `llmq_test` network.
pub(super) fn network(seed: u64) -> Network {
    let weights = || Box::new(SeededRng::new(0x77)) as _;
    Network::new(TEST, &params(TEST), 3, seed, weights, seeded_rng)
}

pub(super) fn round() -> &'static Round {
    static ROUND: OnceLock<Round> = OnceLock::new();
    ROUND.get_or_init(|| {
        let mut net = network(0x1001);
        let contribution = net.contribute_modified(0, &BTreeSet::from([1]));
        for i in 1..3 {
            net.contribute(i).expect("a contribution");
        }
        net.drain(|_, _| 8);
        net.verify_pending();
        net.enter(DkgMsgKind::Complaint);
        let complaint = net.complain(1, &ConnSnapshot::default()).expect("a complaint");
        assert!(net.complain(0, &ConnSnapshot::default()).is_none());
        assert!(net.complain(2, &ConnSnapshot::default()).is_none());
        net.drain(|_, _| 8);
        net.enter(DkgMsgKind::Justification);
        let justification = net.justify(0, true).expect("a justification");
        assert!(net.justify(1, true).is_none() && net.justify(2, true).is_none());
        net.drain(|_, _| 8);
        net.enter(DkgMsgKind::PrematureCommitment);
        let bls = net.bls;
        let pcommit = net.nodes[2]
            .step(bls, |s, env| s.verify_and_commit(env))
            .expect("a premature commitment");
        Round {
            contribution,
            complaint,
            justification,
            pcommit,
        }
    })
}
