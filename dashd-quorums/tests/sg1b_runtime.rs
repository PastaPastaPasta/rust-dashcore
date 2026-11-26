//! The sig-share runtime wired to the real signing runtime on an in-memory store: a sign
//! request reaches the share queue, the dispatcher signs our share, two peers' shares recover
//! the signature into the signing runtime, and the shareman listener purges the session. The
//! passes run inline, then on the share threads.

#[path = "support/shares.rs"]
mod shares;

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dash_clock::MockClock;
use dash_evo::locks::sign_hash;
use dash_evo::locks::wire::RecoveredSig;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::{
    BlsCtx, BlsSchemeKind, BlsSkValue, generate_contributions, secret_to_public, sign,
};
use dash_llmq::dkg::wire::{SigSesAnn, SigShare, SigShareBytes};
use dash_llmq::sigenv::fakes::{FakeLookup, FakeNet, FakeOperatorSigner, FakeQuorum};
use dash_llmq::sigenv::{Now, PeerId, SigMsg, SignRequest, SignerFacts, SporkFacts};
use dash_llmq::signing::api::QuorumSigning;
use dash_llmq::signing::manager::SigningManager;
use dash_llmq::sigshares::recovery::LlmqRecovery;
use dash_p2p_core::Inventory;
use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
use dash_primitives::TxHash;
use dash_types::Numeric as _;
use dashd_quorums::LocalMasternode;
use dashd_quorums::signing::{
    RecSigNotify, SignQueue, SigningHandles, SigningInterrupt, SigningNet, SigningRuntime,
};
use dashd_quorums::sigshares::{SharesFacts, SharesHandles, SharesOut, SigSharesRuntime};
use shares::{Lookup, Mem, Xs, verifier};

const T: LlmqType = LlmqType::LlmqTest;
const QH: [u8; 32] = [4; 32];
const ID: [u8; 32] = [1; 32];
const MSG: [u8; 32] = [2; 32];
const ANNOUNCER: PeerId = 11;

#[derive(Default)]
struct Node {
    me: Option<ProTxHash>,
    notified: Mutex<Vec<(Arc<RecoveredSig>, bool)>>,
}

impl SigningNet for Node {
    fn misbehaving(&self, peer: PeerId, _: i32) {
        panic!("peer {peer} scored");
    }
    fn relay_inv(&self, _: &Inventory) {}
    fn relay_transaction(&self, _: &TxHash) {}
}

impl RecSigNotify for Node {
    fn notify_recovered_sig(&self, sig: Arc<RecoveredSig>, proactive_relay: bool) {
        self.notified.lock().unwrap().push((sig, proactive_relay));
    }
}

impl LocalMasternode for Node {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        self.me
    }
}

struct Out;

impl SharesOut for Out {
    fn send(&self, peer: PeerId, _: Vec<SigMsg>) {
        panic!("no peer is connected, yet {peer} was sent to");
    }
    fn misbehaving(&self, peer: PeerId, _: i32) {
        panic!("peer {peer} scored");
    }
}

struct Facts(ProTxHash);

impl SharesFacts for Facts {
    type Rng = Xs;
    fn signer(&self) -> SignerFacts {
        SignerFacts {
            my_pro_tx: Some(self.0),
            signing_allowed: true,
        }
    }
    fn sporks(&self) -> SporkFacts {
        SporkFacts {
            spork21_active: true,
            spork21_value: 0,
        }
    }
    fn now(&self) -> Now {
        Now {
            ms: 1_700_000_000_000,
        }
    }
    fn bls(&self) -> BlsCtx {
        BlsCtx::BASIC
    }
    fn rng(&self) -> Xs {
        Xs(3)
    }
}

type Signing = SigningRuntime<Mem, Lookup>;
type Shares = SigSharesRuntime<Lookup, Signing, FakeNet, Out, Facts, LlmqRecovery>;

fn scalar(n: u8) -> [u8; 32] {
    let mut b = [0; 32];
    b[31] = n;
    b
}

struct Wired {
    aborts: Arc<Mutex<Vec<String>>>,
    signing: Arc<Signing>,
    shares: Arc<Shares>,
    node: Arc<Node>,
    sks: Vec<BlsSkValue>,
    master: BlsSkValue,
}

/// A 3-member quorum with threshold 3, so our share and both peers' shares are all needed.
fn wired() -> Wired {
    let chain =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let members: Vec<_> = (1..=3).map(|i| ProTxHash::from(scalar(i))).collect();
    let coefficients: Vec<_> = [11, 22, 33]
        .map(|c| BlsSkValue::from_canonical(&scalar(c), BlsSchemeKind::Basic).unwrap())
        .into();
    let ids: Vec<_> = members.iter().map(|m| *m.as_bytes()).collect();
    let (vvec, sks) = generate_contributions(&coefficients, &ids).unwrap();
    let mut params = *chain.consensus.llmq(T).unwrap();
    params.threshold = 3;
    let quorum = Arc::new(FakeQuorum {
        llmq_type: T,
        quorum_hash: QH.into(),
        params,
        members: members.clone(),
        valid_members: [0, 1, 2].into(),
        has_verification_vector: true,
        quorum_public_key: vvec[0].clone(),
        pk_shares: (0..3).map(|i| (i, secret_to_public(&sks[usize::from(i)]))).collect(),
        sk_share: Some(sks[0].clone()),
    });
    let mut fake = FakeLookup::default();
    let key = (T.to_base(), quorum.quorum_hash);
    fake.quorums.insert(key, quorum.clone());
    fake.active.insert(key);
    fake.selections.insert((T.to_base(), ID, -1, 8), quorum);
    let lookup = Arc::new(Lookup(fake));

    let node = Arc::new(Node {
        me: Some(members[0]),
        ..Node::default()
    });
    let signing = Arc::new(SigningRuntime::new(
        SigningManager::new(Mem::default(), chain.consensus.llmq_type_platform, 60),
        lookup.clone(),
        chain.clone(),
        SigningHandles {
            net: node.clone(),
            notify: node.clone(),
            masternode: node.clone(),
            clock: Arc::new(MockClock::at(1_700_000_000)),
        },
    ));
    let aborts = Arc::new(Mutex::new(Vec::new()));
    let sink = aborts.clone();
    let shares = Arc::new(SigSharesRuntime::new(SharesHandles {
        lookup,
        intake: signing.clone(),
        net: Arc::new(FakeNet::default()),
        out: Arc::new(Out),
        facts: Arc::new(Facts(members[0])),
        recovery: Arc::new(LlmqRecovery),
        operator: Arc::new(FakeOperatorSigner::default()),
        chain,
        abort: Arc::new(move |e: &str| sink.lock().unwrap().push(e.to_owned())),
    }));
    assert!(Arc::ptr_eq(signing.chain(), shares.chain()));
    assert!(signing.set_sign_queue(shares.clone()).is_ok());
    let rejected = signing.set_sign_queue(shares.clone()).unwrap_err();
    assert!(Arc::ptr_eq(&rejected, &(shares.clone() as Arc<dyn SignQueue<FakeQuorum>>)));
    signing.register_listener(shares.clone());
    Wired {
        aborts,
        signing,
        shares,
        node,
        sks,
        master: coefficients[0].clone(),
    }
}

fn peer_share(w: &Wired, member: u16) -> SigShare {
    let hash = sign_hash(T, &QH.into(), &ID, &MSG);
    let sig = sign(&w.sks[usize::from(member)], &hash, false);
    SigShare::new(
        T,
        QH.into(),
        member,
        ID.into(),
        MSG.into(),
        SigShareBytes::from_bytes(sig.to_bytes(BlsSchemeKind::Basic)),
    )
}

fn request() -> SignRequest {
    SignRequest {
        llmq_type: T,
        id: ID,
        msg_hash: MSG,
        quorum_hash: None,
        allow_resign: false,
        allow_diff_msg: false,
    }
}

#[test]
fn a_member_sign_request_recovers_through_the_share_runtime() {
    let w = wired();
    assert!(w.signing.async_sign_if_member(&request()).unwrap());

    w.shares.on_ses_ann(ANNOUNCER, vec![SigSesAnn::new(5, T, QH.into(), ID.into(), MSG.into())]);
    assert!(w.shares.session_info(ANNOUNCER, 5).is_some());

    assert_eq!(w.shares.dispatch_pass(&verifier()), None);
    w.shares.on_sig_share(ANNOUNCER, vec![peer_share(&w, 1)]);
    w.shares.on_sig_share(12, vec![peer_share(&w, 2)]);
    assert!(!w.signing.has_recovered_sig(T, &ID, &MSG).unwrap());
    assert_eq!(w.shares.dispatch_pass(&verifier()), None);

    assert_recovered(&w);
}

fn assert_recovered(w: &Wired) {
    assert!(w.aborts.lock().unwrap().is_empty(), "{:?}", w.aborts.lock().unwrap());
    assert!(w.signing.has_recovered_sig(T, &ID, &MSG).unwrap());
    let notified = w.node.notified.lock().unwrap();
    let [(sig, proactive)] = notified.as_slice() else {
        panic!("one recovered sig, got {}", notified.len());
    };
    assert!(*proactive);
    let hash = sign_hash(T, &QH.into(), &ID, &MSG);
    assert_eq!(sig.sig, sign(&w.master, &hash, false).to_bytes(BlsSchemeKind::Basic));
    assert!(w.shares.session_info(ANNOUNCER, 5).is_none(), "the shareman listener purges");
}

#[test]
fn the_share_threads_recover_a_member_sign_request() {
    let w = wired();
    let interrupt = SigningInterrupt::new();
    let threads = w.shares.start(interrupt.clone(), verifier).unwrap();
    assert!(w.signing.async_sign_if_member(&request()).unwrap());
    w.shares.on_ses_ann(ANNOUNCER, vec![SigSesAnn::new(5, T, QH.into(), ID.into(), MSG.into())]);
    w.shares.on_sig_share(ANNOUNCER, vec![peer_share(&w, 1)]);
    w.shares.on_sig_share(12, vec![peer_share(&w, 2)]);
    let deadline = Instant::now() + Duration::from_secs(60);
    while w.node.notified.lock().unwrap().is_empty()
        || w.shares.session_info(ANNOUNCER, 5).is_some()
    {
        assert!(w.aborts.lock().unwrap().is_empty(), "{:?}", w.aborts.lock().unwrap());
        assert!(Instant::now() < deadline, "no recovery on the share threads");
        thread::sleep(Duration::from_millis(5));
    }
    interrupt.trigger();
    threads.join();
    assert_recovered(&w);
}

#[test]
fn two_peer_shares_alone_do_not_reach_the_threshold() {
    let w = wired();
    w.shares.on_sig_share(ANNOUNCER, vec![peer_share(&w, 1)]);
    w.shares.on_sig_share(12, vec![peer_share(&w, 2)]);
    w.shares.dispatch_pass(&verifier());
    assert!(!w.signing.has_recovered_sig(T, &ID, &MSG).unwrap());
    assert!(w.signing.async_sign_if_member(&request()).unwrap());
    w.shares.dispatch_pass(&verifier());
    assert!(w.signing.has_recovered_sig(T, &ID, &MSG).unwrap());
}
