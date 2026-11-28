use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use dash_clock::{Clock, MockClock};
use dash_evo::locks::RecoveredSig;
use dash_kv::{AbortSink, ErrorKind, StoreError};
use dash_num::Hash256;
use dash_p2p::{CommandString, ConnectionType, PeerId, Permissions};
use dash_p2p_core::{GetData, Inv, InvType, Inventory, NotFound};
use dash_params::{BuriedDeployment, ChainParamsBuilder, ChainSetup, LlmqType, Network};
use dash_primitives::QuorumHash;
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;
use dashd_chainstate::chain::{RecordingAbort, genesis_block};
use dashd_locks::is::seams::RecoveredSigs;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::seams::SigningSeam;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{ProcessorOptions, Registry, Seams};
use dashd_quorums::signing::api::{ListenerId, RecoveredSigListener, SelectedQuorum};
use dashd_quorums::signing::{
    IntakeStatus, QuorumSigning, RecSigNotify, RecoveredSigIntake, RecoveredSigNet, SignRequest,
    SigningFault,
};

use super::*;
use crate::chain::Chain;
use crate::chain::tests::{SharedFiles, fresh, imported, regtest, startup};
use crate::init::params::RELEASE;

/// V's genesis tip as `UpdatedBlockTip` reads it, no block for an id not in the index, the
/// active tip's empty signals, the abort's report, and `Stopped` once V is gone.
#[test]
fn ehf_reads_are_one_v_turn_each_and_stopped_after_v() {
    let params = regtest();
    let genesis = genesis_block(&params);
    let clock: Arc<dyn Clock> =
        Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 86_400));
    let abort = Arc::new(RecordingAbort::default());
    let mut chain = Chain::start(fresh(&params, &clock, &abort, Box::new(SharedFiles::default())))
        .expect("V starts");
    startup(&mut chain);
    imported(&mut chain);
    let ehf = NodeEhfChain::new(chain.handle().clone(), abort.clone());

    let handle = chain.handle().view_handle();
    let id = handle.view().snap.tip.tip;
    let entry = handle.entry(id).expect("genesis");
    let v20 = params.consensus.deployment_height(BuriedDeployment::V20).to_u32();
    assert_eq!(
        ehf.tip(id),
        Ok(Some(EhfTip {
            height: 0,
            v20_active: 1 >= v20,
            mtp: entry.mtp,
            signals: EhfSignals::new(),
        }))
    );
    assert_eq!(ehf.tip(BlockId(u32::MAX)), Ok(None));
    assert_eq!(ehf.active_tip_signals(), Ok(EhfSignals::new()));

    ehf.abort(&SigningFault::Lookup("v".into()));
    assert_eq!(abort.messages(), ["CEHFSignalsHandler: quorum lookup fault: v"]);

    chain.stop().expect("V's state");
    assert_eq!(ehf.tip(id), Err(SigningFault::Stopped));
    assert_eq!(ehf.active_tip_signals(), Err(SigningFault::Stopped));
}

#[derive(Debug, PartialEq)]
enum Call {
    Intake(PeerId, RecoveredSig, bool),
    Remove(PeerId),
    Truncate(LlmqType, [u8; 32]),
    Push(RecoveredSig),
}

/// A signing runtime that records each call and answers from a script; an empty script answers
/// `Ok` with nothing known.
struct FakeRuntime {
    chain: Arc<ChainSetup>,
    calls: Mutex<Vec<Call>>,
    faults: Mutex<VecDeque<SigningFault>>,
    stored: Mutex<Vec<RecoveredSig>>,
}

impl FakeRuntime {
    fn new() -> Arc<FakeRuntime> {
        Arc::new(FakeRuntime {
            chain: params(),
            calls: Mutex::default(),
            faults: Mutex::default(),
            stored: Mutex::default(),
        })
    }

    fn fail_next(&self, faults: impl IntoIterator<Item = SigningFault>) {
        self.faults.lock().unwrap().extend(faults);
    }

    fn answer<T>(&self, ok: T) -> Result<T, SigningFault> {
        self.faults.lock().unwrap().pop_front().map_or(Ok(ok), Err)
    }

    fn calls(&self) -> Vec<Call> {
        std::mem::take(&mut self.calls.lock().unwrap())
    }
}

impl RecoveredSigIntake for FakeRuntime {
    fn process_recovered_sig(&self, _: Arc<RecoveredSig>, _: bool) -> Result<bool, SigningFault> {
        unreachable!("the adapters never store a verified sig")
    }

    fn push_reconstructed_recovered_sig(&self, sig: Arc<RecoveredSig>) {
        self.calls.lock().unwrap().push(Call::Push((*sig).clone()));
    }

    fn remove_node(&self, peer: PeerId) {
        self.calls.lock().unwrap().push(Call::Remove(peer));
    }
}

impl RecoveredSigNet for FakeRuntime {
    fn chain(&self) -> &ChainSetup {
        &self.chain
    }

    fn on_recovered_sig(
        &self,
        peer: PeerId,
        sig: Arc<RecoveredSig>,
        legacy: bool,
    ) -> Result<IntakeStatus, SigningFault> {
        self.calls.lock().unwrap().push(Call::Intake(peer, (*sig).clone(), legacy));
        self.answer(IntakeStatus::Queued)
    }

    fn already_have(&self, hash: &[u8; 32]) -> Result<bool, SigningFault> {
        let known = self.stored.lock().unwrap().iter().any(|s| s.hash() == *hash);
        self.answer(known)
    }

    fn for_getdata(&self, hash: &[u8; 32]) -> Result<Option<RecoveredSig>, SigningFault> {
        let stored = self.stored.lock().unwrap().iter().find(|s| s.hash() == *hash).cloned();
        self.answer(stored)
    }
}

impl QuorumSigning for FakeRuntime {
    fn async_sign_if_member(&self, _: &SignRequest) -> Result<bool, SigningFault> {
        unreachable!()
    }

    fn select_quorum_for_signing(
        &self,
        _: LlmqType,
        _: &[u8; 32],
    ) -> Result<Option<SelectedQuorum>, SigningFault> {
        unreachable!()
    }

    fn has_recovered_sig(
        &self,
        _: LlmqType,
        _: &[u8; 32],
        _: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        unreachable!()
    }

    fn has_recovered_sig_for_id(&self, t: LlmqType, id: &[u8; 32]) -> Result<bool, SigningFault> {
        let known = self.stored.lock().unwrap().iter().any(|s| s.llmq_type == t && s.id == *id);
        self.answer(known)
    }

    fn has_recovered_sig_for_session(&self, _: &[u8; 32]) -> Result<bool, SigningFault> {
        unreachable!()
    }

    fn has_recovered_sig_for_signing(
        &self,
        _: LlmqType,
        _: &[u8; 32],
        _: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        unreachable!()
    }

    fn get_recovered_sig(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<Option<RecoveredSig>, SigningFault> {
        let stored = self.stored.lock().unwrap();
        let found = stored.iter().find(|s| s.llmq_type == t && s.id == *id && s.msg_hash == *msg);
        self.answer(found.cloned())
    }

    fn is_conflicting(
        &self,
        _: LlmqType,
        _: &[u8; 32],
        _: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        unreachable!()
    }

    fn get_vote_for_id(&self, _: LlmqType, _: &[u8; 32]) -> Result<Option<[u8; 32]>, SigningFault> {
        unreachable!()
    }

    fn truncate_recovered_sig(&self, t: LlmqType, id: &[u8; 32]) -> Result<(), SigningFault> {
        self.calls.lock().unwrap().push(Call::Truncate(t, *id));
        self.answer(())
    }

    fn register_listener(&self, _: Arc<dyn RecoveredSigListener>) -> ListenerId {
        unreachable!()
    }

    fn unregister_listener(&self, _: ListenerId) {
        unreachable!()
    }
}

#[derive(Default)]
struct Aborts(Mutex<Vec<String>>);

impl AbortSink for Aborts {
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>) {
        assert_eq!(user_msg, None);
        self.0.lock().unwrap().push(log_msg.to_owned());
    }

    fn read_failed(&self, what: &str) {
        panic!("no store read here: {what}");
    }
}

fn params() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, RELEASE).build().unwrap())
}

fn seam() -> (P2pSigning<FakeRuntime>, Arc<FakeRuntime>, Arc<Aborts>) {
    let (seam, _, runtime, aborts) = adapters();
    (seam, runtime, aborts)
}

/// The P2P seam and NetInstantSend's adapter over one runtime, sharing the node's fault policy.
fn adapters() -> (P2pSigning<FakeRuntime>, NodeRecSigs<FakeRuntime>, Arc<FakeRuntime>, Arc<Aborts>)
{
    let runtime = FakeRuntime::new();
    let aborts = Arc::new(Aborts::default());
    let faults = Arc::new(SigningFaults::new(Arc::clone(&aborts) as _));
    let recsigs = NodeRecSigs::new(Arc::clone(&runtime), Arc::clone(&faults));
    (P2pSigning::new(Arc::clone(&runtime), faults), recsigs, runtime, aborts)
}

fn recsig(llmq_type: LlmqType, n: u8) -> RecoveredSig {
    RecoveredSig {
        llmq_type,
        quorum_hash: QuorumHash::from([n; 32]),
        id: [n.wrapping_add(1); 32],
        msg_hash: [n.wrapping_add(2); 32],
        sig: [n.wrapping_add(3); 96],
    }
}

fn wire(sig: &RecoveredSig) -> Vec<u8> {
    let mut out = vec![sig.llmq_type.to_base()];
    out.extend_from_slice(sig.quorum_hash.as_bytes());
    out.extend_from_slice(&sig.id);
    out.extend_from_slice(&sig.msg_hash);
    out.extend_from_slice(&sig.sig);
    out
}

fn sim(seams: Seams) -> Sim {
    let params = params();
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    Sim::with(params, identity, ProcessorOptions::default(), Registry::new(), seams, |_| {})
}

fn settle(sim: &mut Sim) {
    while sim.pass() {}
    sim.pass();
}

fn connect(sim: &mut Sim, n: u8, subscribe: bool) -> SimPeer {
    let peer = handshaken(sim, n);
    sim.deliver(&peer, &[("qsendrecsigs", vec![u8::from(subscribe)])]);
    settle(sim);
    sim.sent(&peer);
    peer
}

fn handshaken(sim: &mut Sim, n: u8) -> SimPeer {
    let peer = sim.connect(n, ConnectionType::Inbound, Permissions::NONE);
    sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
    settle(sim);
    sim.sent(&peer);
    peer
}

#[test]
fn the_seam_answers_the_llmq_check_from_the_runtimes_chain() {
    let (seam, _, _) = seam();
    assert!(seam.enabled());
    assert!(seam.llmq_configured(LlmqType::LlmqTest));
    assert!(!seam.llmq_configured(LlmqType::from_base(250)));
}

#[test]
fn a_qsigrec_runs_the_runtimes_intake_inline_with_no_worker() {
    let (seam, runtime, aborts) = seam();
    let seam = Arc::new(seam);
    let mut sim = sim(Seams {
        signing: Arc::clone(&seam) as _,
        ..Seams::default()
    });
    let peer = connect(&mut sim, 1, false);
    let sig = recsig(LlmqType::LlmqTest, 1);
    assert!(sim.admitted(&peer, "qsigrec", wire(&sig)).is_ok());
    let id = peer.handle.id();
    assert_eq!(runtime.calls(), [Call::Intake(id, sig, false)]);

    seam.remove_node(id);
    assert_eq!(runtime.calls(), [Call::Remove(id)]);
    assert!(aborts.0.lock().unwrap().is_empty());
}

#[test]
fn already_have_and_getdata_pass_the_runtimes_answers() {
    let (seam, runtime, _) = seam();
    let sig = recsig(LlmqType::LlmqTest, 1);
    let other = recsig(LlmqType::LlmqTest, 2);
    *runtime.stored.lock().unwrap() = vec![sig.clone()];
    assert!(seam.already_have(&sig.hash()));
    assert!(!seam.already_have(&other.hash()));
    assert_eq!(seam.for_getdata(&sig.hash()), Some(sig));
    assert_eq!(seam.for_getdata(&other.hash()), None);
}

#[test]
fn a_storage_or_lookup_fault_aborts_the_node_once_and_stopped_is_dropped() {
    let (seam, runtime, aborts) = seam();
    let sig = recsig(LlmqType::LlmqTest, 1);
    *runtime.stored.lock().unwrap() = vec![sig.clone()];

    runtime.fail_next(std::iter::repeat_n(SigningFault::Stopped, 3));
    seam.verify_and_process(1, sig.clone(), false);
    assert!(!seam.already_have(&sig.hash()));
    assert_eq!(seam.for_getdata(&sig.hash()), None);
    assert!(aborts.0.lock().unwrap().is_empty(), "Stopped is dropped");

    runtime.fail_next([SigningFault::Storage("disk".into())]);
    seam.verify_and_process(1, sig.clone(), false);
    assert_eq!(aborts.0.lock().unwrap().len(), 1);
    assert!(aborts.0.lock().unwrap()[0].contains("disk"));

    runtime.fail_next([SigningFault::Lookup("tree".into()), SigningFault::Storage("disk".into())]);
    assert!(!seam.already_have(&sig.hash()));
    assert_eq!(seam.for_getdata(&sig.hash()), None);
    assert_eq!(aborts.0.lock().unwrap().len(), 1, "only the first fault aborts");
    assert_eq!(seam.for_getdata(&sig.hash()), Some(sig), "a later call is answered");
}

#[test]
fn a_lookup_fault_alone_aborts_the_node() {
    let (seam, runtime, aborts) = seam();
    runtime.fail_next([SigningFault::Lookup("tree".into())]);
    assert!(!seam.already_have(&[0; 32]));
    assert_eq!(aborts.0.lock().unwrap().len(), 1);
    assert!(aborts.0.lock().unwrap()[0].contains("tree"));
}

#[test]
fn notify_relays_through_the_bound_processor_only() {
    let relay = RelayRecoveredSigs::default();
    let mut sim = sim(Seams::default());
    let subscriber = connect(&mut sim, 1, true);
    let other = connect(&mut sim, 2, false);

    relay.notify_recovered_sig(Arc::new(recsig(LlmqType::LlmqTest, 1)), true);
    settle(&mut sim);
    assert!(sim.sent(&subscriber).is_empty(), "not bound yet");

    relay.bind(&sim.shared);
    let pushed = recsig(LlmqType::LlmqTest, 2);
    relay.notify_recovered_sig(Arc::new(pushed.clone()), true);
    settle(&mut sim);
    assert_eq!(sim.sent(&subscriber), [(CommandString::QSIGREC, wire(&pushed))]);
    assert!(sim.sent(&other).is_empty());

    relay.notify_recovered_sig(Arc::new(recsig(LlmqType::LlmqTest, 3)), false);
    settle(&mut sim);
    let sent = sim.sent(&subscriber);
    assert_eq!(sent.iter().map(|(c, _)| *c).collect::<Vec<_>>(), [CommandString::INV]);
    assert!(sim.sent(&other).is_empty());

    drop(sim);
    relay.notify_recovered_sig(Arc::new(recsig(LlmqType::LlmqTest, 4)), true);
}

#[test]
fn truncate_reports_a_storage_or_lookup_fault_as_a_store_error_and_drops_stopped() {
    let (_, recsigs, runtime, _) = adapters();
    let t = LlmqType::LlmqTest;
    assert!(recsigs.truncate(t, &[1; 32]).is_ok());

    runtime.fail_next([SigningFault::Stopped]);
    assert!(recsigs.truncate(t, &[2; 32]).is_ok());

    for fault in [SigningFault::Storage("disk".into()), SigningFault::Lookup("tree".into())] {
        let text = fault.to_string();
        runtime.fail_next([fault]);
        let err = recsigs.truncate(t, &[3; 32]).unwrap_err();
        assert!(matches!(
            &err,
            StoreError::Engine { kind: ErrorKind::Other, message, .. } if *message == text
        ));
    }
    assert_eq!(
        runtime.calls(),
        [[1; 32], [2; 32], [3; 32], [3; 32]].map(|id| Call::Truncate(t, id))
    );
}

fn fixture() -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../test-vectors/lane-c/regtest-v24.0.0.json");
    serde_json::from_slice(&std::fs::read(path).expect("lane (c) fixture")).expect("json")
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

/// An RPC display-order (`GetHex`) hash as stored bytes.
fn stored_hash(v: &serde_json::Value) -> [u8; 32] {
    let mut out: [u8; 32] = unhex(v.as_str().expect("hash")).try_into().expect("32 bytes");
    out.reverse();
    out
}

fn fixture_recsig(request: &serde_json::Value) -> RecoveredSig {
    let r = &request["recsig"];
    let sig = RecoveredSig {
        llmq_type: LlmqType::from_base(r["llmqType"].as_u64().expect("type") as u8),
        quorum_hash: stored_hash(&r["quorumHash"]).into(),
        id: stored_hash(&r["id"]),
        msg_hash: stored_hash(&r["msgHash"]),
        sig: unhex(r["sig"].as_str().expect("sig")).try_into().expect("96 bytes"),
    };
    assert_eq!(sig.hash(), stored_hash(&request["recsig_hash"]));
    sig
}

fn recsig_invs(sent: &[(CommandString, Vec<u8>)]) -> Vec<[u8; 32]> {
    sent.iter()
        .filter(|(c, _)| *c == CommandString::INV)
        .flat_map(|(_, bytes)| Inv::decode(&mut &bytes[..]).unwrap().inventory().clone())
        .filter(|inv| inv.inv_type() == InvType::QuorumRecoveredSig)
        .map(|inv| inv.inv_hash().to_lendian())
        .collect()
}

/// Lane (c)'s relay capture at node0, a node with no signing share in the sigs it relays. The
/// subscriber sent `qsendrecsigs 1` and fetched each announced recsig with its own GETDATA; the
/// silent observer never subscribed. Node0 processed all 55 requests' recsigs from its peers, so
/// none was a proactive push: each reaches the subscriber as an INV, and the silent observer gets
/// no recsig traffic. GETDATA answers from the runtime, which serves an active quorum's recsig
/// only. Which 17 node0 served is that runtime's input here, from the capture; the test checks
/// the announcements, the wire bytes of each reply, NOTFOUND for the rest, and the silence.
// Core: v24.0.2 src/net_processing.cpp:2668-2689, :3038-3044; src/llmq/signing.cpp:410-420
#[test]
fn lane_c_subscriber_transcript_and_silent_observer() {
    let fixture = fixture();
    let requests = fixture["requests"].as_array().expect("requests");
    let sigs: Vec<RecoveredSig> = requests.iter().map(fixture_recsig).collect();
    let relay_capture = &fixture["relay"];
    let captured: BTreeMap<[u8; 32], Vec<u8>> = relay_capture["subscriber"]["captured"]
        .as_array()
        .expect("captured")
        .iter()
        .map(|c| {
            assert_eq!(c["command"], "qsigrec");
            (stored_hash(&c["inv_hash"]), unhex(c["payload"].as_str().expect("payload")))
        })
        .collect();
    let recorded_invs: BTreeSet<[u8; 32]> = relay_capture["subscriber"]["invs"]
        .as_array()
        .expect("invs")
        .iter()
        .map(|inv| {
            assert_eq!(inv["type"], 28);
            stored_hash(&inv["hash"])
        })
        .collect();
    assert_eq!((sigs.len(), recorded_invs.len(), captured.len()), (55, 55, 17));
    let silent_capture = &relay_capture["silent"];
    assert_eq!(silent_capture["inv_count"], 0);
    assert!(silent_capture["captured"].as_array().unwrap().is_empty());
    assert!(silent_capture["unsolicited"].as_array().unwrap().is_empty());

    let (seam, runtime, aborts) = seam();
    *runtime.stored.lock().unwrap() =
        sigs.iter().filter(|s| captured.contains_key(&s.hash())).cloned().collect();
    let mut sim = sim(Seams {
        signing: Arc::new(seam) as _,
        ..Seams::default()
    });
    let relay = RelayRecoveredSigs::default();
    relay.bind(&sim.shared);
    let subscriber = connect(&mut sim, 1, true);
    let silent = handshaken(&mut sim, 2);

    for sig in &sigs {
        relay.notify_recovered_sig(Arc::new(sig.clone()), false);
    }
    settle(&mut sim);
    let announced = recsig_invs(&sim.sent(&subscriber));
    assert_eq!(announced.len(), 55, "one INV per recsig");
    assert_eq!(announced.iter().copied().collect::<BTreeSet<_>>(), recorded_invs);

    let mut replies = BTreeMap::new();
    let mut not_found = 0;
    for hash in &announced {
        let inv = Inventory::new(InvType::QuorumRecoveredSig, Hash256::from_lendian(*hash));
        let mut getdata = Vec::new();
        GetData::new(vec![inv]).encode(&mut getdata);
        sim.deliver(&subscriber, &[("getdata", getdata)]);
        settle(&mut sim);
        match &sim.sent(&subscriber)[..] {
            [(CommandString::QSIGREC, payload)] => {
                assert!(replies.insert(*hash, payload.clone()).is_none());
            }
            [(CommandString::NOTFOUND, payload)] => {
                assert_eq!(NotFound::decode(&mut &payload[..]).unwrap().inventory(), &[inv]);
                not_found += 1;
            }
            other => panic!("one reply per GETDATA, got {other:?}"),
        }
    }
    assert_eq!(replies, captured, "the 17 GETDATA replies, byte for byte");
    assert_eq!(not_found, 55 - 17);

    let heard = sim.sent(&silent);
    assert!(recsig_invs(&heard).is_empty());
    assert!(heard.iter().all(|(c, _)| *c != CommandString::QSIGREC), "{heard:?}");
    assert!(aborts.0.lock().unwrap().is_empty());
}

/// NetInstantSend's reads and push over the node's real signing runtime, on an in-memory
/// recovered-signature database, and the batch path through them.
mod over_the_runtime {
    use dash_evo::locks::{Pass, islock_request_id};
    use dash_pkc::bls::BlsSigBytes;
    use dash_primitives::TxHash;
    use dashd_locks::is::LockedTx;
    use dashd_locks::is::s3_fixtures::{
        BASIC, FakeView, Passes, Rig, TestRng, entry, is_type, lock, quorum_hash,
    };
    use dashd_quorums::signing::SigningNet;
    use dashd_quorums::signing::api::RecoveredSigResult;

    use super::*;
    use crate::init::quorums::{NodeQuorums, QCacheThread, QuorumDb, init_quorums};
    use crate::init::recsigs::RecoveredSigsDb;
    use crate::init::signing::{NodeSigning, NodeSigningRuntime};

    struct NoNet;

    impl SigningNet for NoNet {
        fn misbehaving(&self, _: PeerId, _: i32) {}
        fn relay_inv(&self, _: &Inventory) {}
        fn relay_transaction(&self, _: &TxHash) {}
    }

    #[derive(Default)]
    struct Heard(Mutex<Vec<RecoveredSig>>);

    impl RecoveredSigListener for Heard {
        fn handle_new_recovered_sig(&self, sig: &RecoveredSig) -> RecoveredSigResult {
            self.0.lock().unwrap().push(sig.clone());
            RecoveredSigResult::Nothing
        }
    }

    /// The runtime, never started: each test runs the worker's pass itself.
    fn signing() -> (NodeQuorums, NodeSigning) {
        let setup = crate::chain::tests::regtest();
        let mut hook = None;
        let mut quorums =
            init_quorums(&setup, QuorumDb::in_memory(), QCacheThread::new(), &mut hook);
        quorums.recsigs = RecoveredSigsDb::in_memory();
        let relay = Arc::new(RelayRecoveredSigs::default());
        let clock = Arc::new(MockClock::at(1));
        let signing = NodeSigning::new(&quorums, setup, Arc::new(NoNet), relay, clock, None);
        (quorums, signing)
    }

    fn adapter(runtime: &Arc<NodeSigningRuntime>) -> (Arc<dyn RecoveredSigs>, Arc<Aborts>) {
        let aborts = Arc::new(Aborts::default());
        let faults = Arc::new(SigningFaults::new(Arc::clone(&aborts) as _));
        (Arc::new(NodeRecSigs::new(Arc::clone(runtime), faults)), aborts)
    }

    fn pass(runtime: &NodeSigningRuntime) {
        assert!(!runtime.process_pending_recovered_sigs(false, &mut TestRng(3)).unwrap());
    }

    /// Before step 7c's bind the late slot answers as no signature stored and drops a push;
    /// bound, it reads the runtime's database: the stored signature for its id and msgHash, none
    /// for another msgHash, and its id known.
    // Core: v24.0.2 src/llmq/signing.cpp:133-144, :654-657, :670-674
    #[test]
    fn the_late_slot_answers_nothing_stored_until_bound_then_reads_the_runtime() {
        let (_quorums, signing) = signing();
        let runtime = signing.runtime();
        let sig = recsig(LlmqType::LlmqTest, 1);
        let (t, id, msg) = (sig.llmq_type, sig.id, sig.msg_hash);
        assert!(runtime.process_recovered_sig(Arc::new(sig.clone()), false).unwrap());

        let late = LateRecSigs::default();
        assert_eq!(late.get_recovered_sig(t, &id, &msg), None);
        assert!(!late.has_recovered_sig_for_id(t, &id));
        let unbound = recsig(LlmqType::LlmqTest, 2);
        late.push_reconstructed_recovered_sig(Arc::new(unbound.clone()));
        pass(runtime);
        assert!(!runtime.has_recovered_sig_for_id(t, &unbound.id).unwrap(), "dropped");

        let (recsigs, aborts) = adapter(runtime);
        late.bind(recsigs);
        let stored = Some((sig.quorum_hash, BlsSigBytes::from(sig.sig)));
        assert_eq!(late.get_recovered_sig(t, &id, &msg), stored);
        assert_eq!(late.get_recovered_sig(t, &id, &[9; 32]), None, "another msgHash");
        assert!(late.has_recovered_sig_for_id(t, &id));
        assert!(!late.has_recovered_sig_for_id(t, &unbound.id));
        assert!(aborts.0.lock().unwrap().is_empty());
    }

    /// A pushed reconstructed signature waits in the queue, unstored, until the worker's pass
    /// stores it and calls the listeners.
    // Core: v24.0.2 src/llmq/signing.cpp:619-623; src/llmq/net_signing.cpp:226-229
    #[test]
    fn a_pushed_reconstructed_sig_is_stored_by_the_next_pass_and_heard() {
        let (_quorums, signing) = signing();
        let runtime = signing.runtime();
        let heard = Arc::new(Heard::default());
        runtime.register_listener(Arc::clone(&heard) as _);
        let late = LateRecSigs::default();
        let (recsigs, aborts) = adapter(runtime);
        late.bind(recsigs);

        let sig = recsig(LlmqType::LlmqTest, 1);
        late.push_reconstructed_recovered_sig(Arc::new(sig.clone()));
        assert!(runtime.already_have(&sig.hash()).unwrap(), "queued");
        assert!(!late.has_recovered_sig_for_id(sig.llmq_type, &sig.id), "not stored yet");
        pass(runtime);
        assert!(late.has_recovered_sig_for_id(sig.llmq_type, &sig.id));
        assert_eq!(*heard.0.lock().unwrap(), [sig]);
        assert!(aborts.0.lock().unwrap().is_empty());
    }

    /// The recovered signature a lock carries: its quorum, request id, txid and signature.
    fn lock_recsig(lock: &LockedTx, q: u8, sig: BlsSigBytes) -> RecoveredSig {
        RecoveredSig {
            llmq_type: is_type(),
            quorum_hash: quorum_hash(q).into(),
            id: islock_request_id(lock.inputs()),
            msg_hash: *lock.txid().as_bytes(),
            sig: *sig.as_bytes(),
        }
    }

    /// The batch reads the runtime: a lock whose recovered signature is stored from its quorum
    /// with its signature is already verified and never pushed to the verifier.
    // Core: v24.0.2 src/instantsend/net_instantsend.cpp:153-158
    #[test]
    fn a_lock_whose_recsig_is_stored_is_already_verified() {
        let (_quorums, signing) = signing();
        let runtime = signing.runtime();
        let (recsigs, aborts) = adapter(runtime);
        let rig = Rig::with_sigman(recsigs);
        let (stored, fresh) = (lock(1, 1), lock(2, 1));
        let sig = lock_recsig(&stored, 1, stored.sig());
        assert!(runtime.process_recovered_sig(Arc::new(sig), false).unwrap());

        let pend = [entry(1, stored), entry(1, fresh)];
        let view = FakeView::with_quorums(1);
        let targets = rig.net.batch_targets(&view, Pass::Current, &pend).unwrap();
        let data = rig.net.build_verification_batch(BASIC, &pend, targets, &mut TestRng(1));
        assert_eq!((data.already_verified, data.verify_count), (1, 1));
        assert!(data.rec_sig(&pend[0].islock_hash).is_none());
        assert!(data.rec_sig(&pend[1].islock_hash).is_some());
        assert!(aborts.0.lock().unwrap().is_empty());
    }

    /// A newly verified lock hands its reconstructed signature to the runtime's queue, and the
    /// next pass stores it; a verified lock whose id already has a signature (here another,
    /// mismatching one) hands nothing.
    // Core: v24.0.2 src/instantsend/net_instantsend.cpp:167-170, :207-218
    #[test]
    fn a_newly_verified_lock_pushes_its_recsig_unless_its_id_has_one() {
        let (_quorums, signing) = signing();
        let runtime = signing.runtime();
        let (recsigs, aborts) = adapter(runtime);
        let rig = Rig::with_sigman(recsigs);
        let (fresh, known) = (lock(1, 1), lock(2, 1));
        let other = lock_recsig(&known, 1, lock(3, 1).sig());
        assert!(runtime.process_recovered_sig(Arc::new(other.clone()), false).unwrap());
        let reconstructed = lock_recsig(&fresh, 1, fresh.sig());
        let not_pushed = lock_recsig(&known, 1, known.sig());

        let pend = vec![entry(1, fresh), entry(2, known)];
        let hashes = [pend[0].islock_hash, pend[1].islock_hash];
        let view = FakeView::with_quorums(1);
        rig.net.process_pending_is_locks(&Passes::one(view), pend, &mut TestRng(2)).unwrap();
        assert_eq!(rig.processed(), [(1, hashes[0]), (2, hashes[1])]);
        assert!(runtime.already_have(&reconstructed.hash()).unwrap(), "queued");
        assert!(!runtime.already_have(&not_pushed.hash()).unwrap(), "its id has one");

        pass(runtime);
        let (t, id, msg) = (reconstructed.llmq_type, reconstructed.id, reconstructed.msg_hash);
        assert_eq!(runtime.get_recovered_sig(t, &id, &msg).unwrap(), Some(reconstructed));
        let (id, msg) = (other.id, other.msg_hash);
        assert_eq!(runtime.get_recovered_sig(t, &id, &msg).unwrap(), Some(other));
        assert!(aborts.0.lock().unwrap().is_empty());
    }
}

/// Each read's storage or lookup fault answers as nothing stored and aborts the node through the
/// P2P seam's policy: once, keeping an earlier fault's cause. `Stopped` answers the same with no
/// abort. A push has no fault.
#[test]
fn a_read_fault_answers_nothing_stored_and_aborts_once_through_the_seams_policy() {
    let t = LlmqType::LlmqTest;
    let sig = recsig(t, 1);
    type Read = fn(&NodeRecSigs<FakeRuntime>, &RecoveredSig) -> bool;
    let reads: [(&str, Read); 2] = [
        ("get", |r, s| r.get_recovered_sig(s.llmq_type, &s.id, &s.msg_hash).is_some()),
        ("has", |r, s| r.has_recovered_sig_for_id(s.llmq_type, &s.id)),
    ];
    for (name, read) in reads {
        let (seam, recsigs, runtime, aborts) = adapters();
        *runtime.stored.lock().unwrap() = vec![sig.clone()];
        assert!(read(&recsigs, &sig), "{name}: stored");

        runtime.fail_next([SigningFault::Stopped]);
        assert!(!read(&recsigs, &sig), "{name}");
        assert!(aborts.0.lock().unwrap().is_empty(), "{name}: Stopped does not abort");

        runtime.fail_next([SigningFault::Storage("disk".into())]);
        assert!(!read(&recsigs, &sig), "{name}");
        runtime.fail_next([SigningFault::Lookup("tree".into())]);
        assert!(!read(&recsigs, &sig), "{name}");
        assert_eq!(
            *aborts.0.lock().unwrap(),
            ["recovered sig read: recovered-signature storage fault: disk"]
        );
        assert!(!seam.already_have(&[0; 32]));
        assert!(read(&recsigs, &sig), "{name}: a later read is answered");

        let (seam, recsigs, runtime, aborts) = adapters();
        runtime.fail_next([SigningFault::Storage("first".into())]);
        assert!(!seam.already_have(&sig.hash()));
        runtime.fail_next([SigningFault::Lookup("second".into())]);
        assert!(!read(&recsigs, &sig), "{name}");
        let aborted = aborts.0.lock().unwrap().clone();
        assert_eq!(aborted.len(), 1, "{name}");
        assert!(aborted[0].contains("first"), "{name}: {aborted:?}");

        let (_, recsigs, runtime, aborts) = adapters();
        runtime.fail_next([SigningFault::Lookup("tree".into())]);
        assert!(!read(&recsigs, &sig), "{name}");
        assert_eq!(aborts.0.lock().unwrap().len(), 1, "{name}: a lookup fault alone aborts");
    }

    let (_, recsigs, runtime, aborts) = adapters();
    recsigs.push_reconstructed_recovered_sig(Arc::new(sig.clone()));
    assert_eq!(runtime.calls(), [Call::Push(sig)]);
    assert!(aborts.0.lock().unwrap().is_empty());
}

mod ehf {
    use std::sync::mpsc;
    use std::time::Duration;

    use dash_consensus::primitives::{Transaction, TxType};
    use dash_consensus::{Tx, TxValidationResult};
    use dash_primitives::TxHash;
    use dashd_chainstate::chain::{ActorReply, WaitError};
    use dashd_indexes::IndexSummary;
    use dashd_mempool::api::RelayRead;
    use dashd_mempool::types::{
        MempoolAcceptResult, MempoolAcceptResultType, Package, PackageMempoolAcceptResult,
    };

    use super::*;

    /// A pool whose admission answers `reply`, recording each call's `bypass_limits`.
    struct Pool {
        reply: Result<MempoolAcceptResult, WaitError>,
        bypass: Mutex<Vec<bool>>,
    }

    impl MempoolHandle for Pool {
        fn accept_async(&self, _: TxRef, _: Option<u64>) -> ActorReply<MempoolAcceptResult> {
            unreachable!()
        }

        fn accept(&self, _: TxRef, bypass: bool) -> Result<MempoolAcceptResult, WaitError> {
            self.bypass.lock().unwrap().push(bypass);
            self.reply.clone()
        }

        fn test_accept(&self, _: TxRef, _: bool) -> Result<MempoolAcceptResult, WaitError> {
            unreachable!("ProcessTransaction runs with test_accept false")
        }

        fn process_new_package(
            &self,
            _: Package,
            _: bool,
        ) -> Result<PackageMempoolAcceptResult, WaitError> {
            unreachable!()
        }

        fn prioritise(&self, _: &TxHash, _: dash_consensus::Amount) {
            unreachable!()
        }

        fn post_prioritise(&self, _: &TxHash, _: dash_consensus::Amount) {
            unreachable!()
        }

        fn add_unbroadcast(&self, _: &TxHash) {
            unreachable!()
        }

        fn remove_unbroadcast(&self, _: &TxHash, _: bool) {
            unreachable!()
        }

        fn with_relay_read(&self, _: &mut dyn FnMut(&dyn RelayRead)) {
            unreachable!()
        }
    }

    fn accept_result(
        kind: MempoolAcceptResultType,
        reason: &str,
        debug: &str,
    ) -> MempoolAcceptResult {
        MempoolAcceptResult {
            result_type: kind,
            validation_result: match kind {
                MempoolAcceptResultType::Invalid => TxValidationResult::Consensus,
                _ => TxValidationResult::Unset,
            },
            reason: reason.to_owned(),
            debug_message: debug.to_owned(),
            vsize: None,
            base_fee: None,
            effective_feerate: None,
            txids_fee_calculations: None,
        }
    }

    fn mnhf_tx() -> TxRef {
        Arc::new(Tx::new(Transaction::new(3, TxType::MnhfSignal, vec![], vec![], 0, vec![1])))
    }

    fn process(
        reply: Result<MempoolAcceptResult, WaitError>,
    ) -> Result<Result<(), String>, SigningFault> {
        let pool = Arc::new(Pool {
            reply,
            bypass: Mutex::default(),
        });
        let mempool = NodeEhfMempool {
            mempool: Some(Arc::clone(&pool) as _),
            txindex: None,
        };
        let answer = mempool.process_transaction(mnhf_tx());
        assert_eq!(*pool.bypass.lock().unwrap(), [false], "bypass_limits false");
        answer
    }

    /// `ProcessTransaction`'s result as `HandleNewRecoveredSig` reads it: `VALID`, else the
    /// state's `ToString` (`Valid` for `MEMPOOL_ENTRY`'s valid state). An evo-db exception is a
    /// fault, and a dropped reply `Stopped`; without a pool, Core's `no-mempool` rejection.
    // Core: v24.0.3 src/llmq/ehf_signals.cpp:134-143, src/validation.cpp:4923-4935,
    // src/consensus/validation.h:105-117
    #[test]
    fn ehf_admission_maps_core_results() {
        use MempoolAcceptResultType::*;
        assert_eq!(process(Ok(accept_result(Valid, "", ""))), Ok(Ok(())));
        assert_eq!(
            process(Ok(accept_result(Invalid, "bad-mnhf-sig", "quorum"))),
            Ok(Err("bad-mnhf-sig, quorum".to_owned()))
        );
        assert_eq!(
            process(Ok(accept_result(Invalid, "tx-txlock-conflict", ""))),
            Ok(Err("tx-txlock-conflict".to_owned()))
        );
        assert_eq!(process(Ok(accept_result(MempoolEntry, "", ""))), Ok(Err("Valid".to_owned())));
        assert_eq!(
            process(Ok(accept_result(Unavailable, "missing evo state", ""))),
            Err(SigningFault::Storage("ProcessTransaction: missing evo state".to_owned()))
        );
        assert_eq!(process(Err(WaitError::Disconnected)), Err(SigningFault::Stopped));
        let none = NodeEhfMempool {
            mempool: None,
            txindex: None,
        };
        assert_eq!(none.process_transaction(mnhf_tx()), Ok(Err("no-mempool".to_owned())));
    }

    /// A txindex that is caught up only once `release` is sent.
    struct Catching {
        release: Mutex<mpsc::Receiver<()>>,
        waits: Mutex<u32>,
    }

    impl IndexStatus for Catching {
        fn summary(&self) -> IndexSummary {
            IndexSummary {
                name: "txindex",
                synced: true,
                best_block_height: 0,
            }
        }

        fn block_until_synced(&self) -> bool {
            self.release.lock().unwrap().recv().unwrap();
            *self.waits.lock().unwrap() += 1;
            true
        }
    }

    /// `if (g_txindex) g_txindex->BlockUntilSyncedToCurrentChain()`: the call waits for the
    /// txindex, and without one returns at once.
    // Core: v24.0.3 src/llmq/ehf_signals.cpp:102-104
    #[test]
    fn ehf_waits_for_the_txindex_only_when_one_runs() {
        NodeEhfMempool {
            mempool: None,
            txindex: None,
        }
        .block_until_txindex_synced();
        let (release, gate) = mpsc::channel();
        let index = Arc::new(Catching {
            release: Mutex::new(gate),
            waits: Mutex::default(),
        });
        let mempool = NodeEhfMempool {
            mempool: None,
            txindex: Some(Arc::clone(&index) as _),
        };
        let (done, returned) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            mempool.block_until_txindex_synced();
            done.send(()).unwrap();
        });
        assert_eq!(
            returned.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "returned before the txindex caught up"
        );
        release.send(()).unwrap();
        returned.recv_timeout(Duration::from_secs(60)).unwrap();
        waiter.join().unwrap();
        assert_eq!(*index.waits.lock().unwrap(), 1);
    }

    /// An [`EhfChain`] whose `tip` answers the scripted faults, then no block.
    #[derive(Clone, Default)]
    struct Scripted {
        faults: Arc<Mutex<VecDeque<SigningFault>>>,
        reads: Arc<Mutex<u32>>,
        aborts: Arc<Mutex<Vec<String>>>,
    }

    impl EhfChain for Scripted {
        fn tip(&self, _: BlockId) -> Result<Option<EhfTip>, SigningFault> {
            *self.reads.lock().unwrap() += 1;
            self.faults.lock().unwrap().pop_front().map_or(Ok(None), Err)
        }

        fn active_tip_signals(&self) -> Result<EhfSignals, SigningFault> {
            unreachable!("no recovered sig here")
        }

        fn abort(&self, fault: &SigningFault) {
            self.aborts.lock().unwrap().push(fault.to_string());
        }
    }

    /// `ehf_sighandler->UpdatedBlockTip`'s fault on the `am1-tip` worker is `AbortNode`, except
    /// `Stopped`; [`LateEhf`] reaches the handler only once bound.
    // Core: v24.0.3 src/active/context.cpp:110, src/llmq/ehf_signals.cpp:42-64
    #[test]
    fn an_ehf_tip_fault_aborts_except_stopped() {
        let chain = Scripted::default();
        let mempool = Arc::new(NodeEhfMempool {
            mempool: None,
            txindex: None,
        });
        let handler =
            Arc::new(EhfSignalsHandler::new(params(), FakeRuntime::new(), chain.clone(), mempool));
        let late = LateEhf::default();
        late.updated_block_tip(BlockId(1), false);
        assert_eq!(*chain.reads.lock().unwrap(), 0, "unbound");
        late.bind(Arc::new(EhfTipDuty::new(handler, chain.clone())));
        chain.faults.lock().unwrap().extend([
            SigningFault::Stopped,
            SigningFault::Storage("disk".into()),
            SigningFault::Lookup("tree".into()),
        ]);
        for _ in 0..4 {
            late.updated_block_tip(BlockId(1), false);
        }
        assert_eq!(*chain.reads.lock().unwrap(), 4);
        assert_eq!(
            *chain.aborts.lock().unwrap(),
            ["recovered-signature storage fault: disk", "quorum lookup fault: tree"]
        );
    }
}
