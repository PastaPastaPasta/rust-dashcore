use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_clock::MockClock;
use dash_core_vectors::hex::try_parse_hex;
use dash_evo::bls::BlsCtx;
use dash_evo::locks::{ChainLockSig, ClsigHashSchemes, clsig_hash};
use dash_p2p_core::{ClSig, InvType};
use dash_pkc::bls::BlsGlobalSig;
use dash_types::Numeric;
use dashd_locks::chainlock::{
    ChainLockHandler, ClChain, ClRelay, ClSigsSnapshot, ClVerdict, ClVerify, Read, Stopped,
};
use dashd_p2p::processing::registry::MessageProcessingResult;
use dashd_p2p::processing::seams::ChainLocks;
use parking_lot::Mutex;
use serde_json::Value;

#[derive(Default)]
struct State {
    events: Vec<&'static str>,
    snapshot: ClSigsSnapshot,
    height: i32,
    stop_intake: bool,
    record: Option<bool>,
    reset: bool,
    read: Option<BlsCtx>,
    from: Option<i64>,
    aborts: Vec<String>,
}

#[derive(Clone, Default)]
struct Chain(Arc<Mutex<State>>);

impl ClChain for Chain {
    fn intake(&self) -> Result<i32, Stopped> {
        let mut s = self.0.lock();
        s.events.push("intake");
        if s.stop_intake {
            return Err(Stopped);
        }
        if s.reset {
            s.height = -1;
            s.snapshot = ClSigsSnapshot::default();
        }
        Ok(s.height)
    }

    fn record(&self, sig: Arc<ChainLockSig>, hash: &[u8; 32], from: i64) -> Result<bool, Stopped> {
        let mut s = self.0.lock();
        s.events.push("record");
        s.from = Some(from);
        let relay = s.record.ok_or(Stopped)?;
        s.snapshot = ClSigsSnapshot {
            best: Some(sig.clone()),
            best_hash: *hash,
        };
        s.height = sig.height;
        Ok(relay)
    }

    fn abort_node(&self, msg: String) {
        let mut s = self.0.lock();
        s.events.push("abort");
        s.aborts.push(msg);
    }

    fn reads(&self) -> ClSigsSnapshot {
        self.0.lock().snapshot.clone()
    }
}

struct Verify(Chain, ClVerdict);

impl ClVerify for Verify {
    fn verify(&self, read: &Read<ClSig>) -> ClVerdict {
        let mut s = self.0.0.lock();
        s.events.push("verify");
        s.read = Some(read.read);
        self.1.clone()
    }
}

fn sig(height: i32) -> ClSig {
    ClSig::new(height, [4; 32].into(), [0; 96].into())
}

fn read(msg: ClSig) -> Read<ClSig> {
    Read {
        msg,
        read: BlsCtx::BASIC,
    }
}

fn handler(chain: &Chain, verdict: ClVerdict) -> ChainLockHandler<Chain, Verify> {
    ChainLockHandler::new(
        chain.clone(),
        Verify(chain.clone(), verdict),
        Arc::new(MockClock::new()),
        || true,
    )
}

#[test]
fn intake_precedes_duplicate_and_publishes_reset() {
    let chain = Chain::default();
    chain.0.lock().record = Some(true);
    let mut h = handler(&chain, ClVerdict::Valid);
    assert!(!h.process_new_chainlock(7, read(sig(5)), &[1; 32]).is_empty());
    chain.0.lock().reset = true;
    chain.0.lock().events.clear();
    assert!(h.process_new_chainlock(7, read(sig(5)), &[1; 32]).is_empty());
    assert_eq!(chain.0.lock().events, ["intake"]);
    assert!(h.best_chainlock(BlsCtx::BASIC).is_none());
    assert!(h.already_have(&[1; 32]));
}

#[test]
fn stale_is_seen_before_precheck_and_never_verified() {
    for height in [-1, 0, 5] {
        let chain = Chain::default();
        chain.0.lock().height = 5;
        let mut h = handler(&chain, ClVerdict::Invalid);
        assert!(h.process_new_chainlock(7, read(sig(height)), &[1; 32]).is_empty());
        assert_eq!(chain.0.lock().events, ["intake"]);
        assert!(h.already_have(&[1; 32]));
        assert_eq!(h.seen_len(), 1);
        chain.0.lock().height = -2;
        assert!(h.process_new_chainlock(7, read(sig(height)), &[1; 32]).is_empty());
        assert_eq!(chain.0.lock().events, ["intake", "intake"]);
    }
}

#[test]
fn reset_intake_height_is_used_for_precheck() {
    let chain = Chain::default();
    {
        let mut s = chain.0.lock();
        s.height = 100;
        s.reset = true;
        s.record = Some(true);
    }
    let mut h = handler(&chain, ClVerdict::Valid);
    assert_eq!(h.process_new_chainlock(7, read(sig(5)), &[1; 32]).inventory.len(), 1);
    assert_eq!(chain.0.lock().events, ["intake", "verify", "record"]);
}

#[test]
fn rejections_score_peers_but_not_external_and_remain_seen() {
    for verdict in [ClVerdict::NoQuorum, ClVerdict::Invalid] {
        for from in [-1, 0, 7] {
            let chain = Chain::default();
            let mut h = handler(&chain, verdict.clone());
            let result = h.process_new_chainlock(from, read(sig(5)), &[1; 32]);
            assert_eq!(result.error.map(|e| e.score), (from != -1).then_some(10));
            assert!(result.inventory.is_empty());
            assert_eq!(chain.0.lock().events, ["intake", "verify"]);
            assert!(h.already_have(&[1; 32]));
            assert!(h.process_new_chainlock(from, read(sig(5)), &[1; 32]).is_empty());
            assert_eq!(chain.0.lock().events, ["intake", "verify", "intake"]);
        }
    }
}

#[test]
fn unavailable_is_unscored_unrelayed_and_remains_seen() {
    for from in [-1, 7] {
        let chain = Chain::default();
        let mut h = handler(&chain, ClVerdict::Unavailable("view unavailable".into()));
        assert!(h.process_new_chainlock(from, read(sig(5)), &[1; 32]).is_empty());
        assert!(h.already_have(&[1; 32]));
        assert!(h.process_new_chainlock(from, read(sig(5)), &[1; 32]).is_empty());
        assert_eq!(chain.0.lock().events, ["intake", "verify", "intake"]);
        assert!(chain.0.lock().aborts.is_empty());
    }
}

#[test]
fn corrupt_aborts_without_scoring_or_relaying_and_remains_seen() {
    for from in [-1, 7] {
        let chain = Chain::default();
        let mut h = handler(&chain, ClVerdict::Corrupt("corrupt quorum state".into()));
        assert!(h.process_new_chainlock(from, read(sig(5)), &[1; 32]).is_empty());
        assert!(h.already_have(&[1; 32]));
        assert_eq!(chain.0.lock().aborts, ["corrupt quorum state"]);
        assert!(h.process_new_chainlock(from, read(sig(5)), &[1; 32]).is_empty());
        assert_eq!(chain.0.lock().events, ["intake", "verify", "abort", "intake"]);
    }
}

#[test]
fn verified_undecodable_signature_aborts_without_scoring_or_relaying() {
    let chain = Chain::default();
    chain.0.lock().record = Some(true);
    let mut h = handler(&chain, ClVerdict::Valid);
    let msg = ClSig::new(5, [4; 32].into(), [0xff; 96].into());
    assert!(h.process_new_chainlock(7, read(msg), &[1; 32]).is_empty());
    assert!(h.already_have(&[1; 32]));
    let state = chain.0.lock();
    assert_eq!(state.events, ["intake", "verify", "abort"]);
    assert_eq!(state.aborts.len(), 1);
    assert!(state.aborts[0].contains("verified ChainLock could not be decoded"));
}

#[test]
fn relay_follows_record_reply_and_preserves_sender() {
    for relay in [false, true] {
        for from in [-1, 7] {
            let chain = Chain::default();
            chain.0.lock().record = Some(relay);
            let mut h = handler(&chain, ClVerdict::Valid);
            let result = h.process_new_chainlock(from, read(sig(5)), &[1; 32]);
            assert!(result.error.is_none());
            assert_eq!(result.inventory.len(), usize::from(relay));
            if relay {
                assert_eq!(result.inventory[0].inv_type(), InvType::ClSig);
                assert_eq!(result.inventory[0].inv_hash().to_lendian(), [1; 32]);
            }
            assert_eq!(chain.0.lock().events, ["intake", "verify", "record"]);
            assert_eq!(chain.0.lock().from, Some(from));
        }
    }
}

#[test]
fn stopped_intake_does_not_insert_or_score() {
    let chain = Chain::default();
    chain.0.lock().stop_intake = true;
    let mut h = handler(&chain, ClVerdict::Invalid);
    assert!(h.process_new_chainlock(7, read(sig(5)), &[1; 32]).is_empty());
    assert!(!h.already_have(&[1; 32]));
    assert_eq!(chain.0.lock().events, ["intake"]);
}

#[test]
fn stopped_record_keeps_seen_and_does_not_score_or_relay() {
    let chain = Chain::default();
    let mut h = handler(&chain, ClVerdict::Valid);
    assert!(h.process_new_chainlock(7, read(sig(5)), &[1; 32]).is_empty());
    assert!(h.already_have(&[1; 32]));
    assert_eq!(chain.0.lock().events, ["intake", "verify", "record"]);
}

#[test]
fn reads_serve_best_or_null_and_already_have_uses_both_sources() {
    let chain = Chain::default();
    let mut h = handler(&chain, ClVerdict::Invalid);
    assert!(h.best_chainlock(BlsCtx::BASIC).is_none());
    assert!(h.already_have(&[0; 32]));
    let null = h.chainlock_by_hash(&[0; 32], BlsCtx::BASIC).unwrap();
    assert_eq!(null.height(), -1);
    assert_eq!(null.block_hash().as_bytes(), &[0; 32]);
    assert_eq!(null.sig().as_bytes(), &[0; 96]);
    assert!(h.chainlock_by_hash(&[1; 32], BlsCtx::BASIC).is_none());
    chain.0.lock().snapshot = ClSigsSnapshot {
        best: Some(Arc::new(ChainLockSig::from_wire(&sig(5), BlsCtx::BASIC).unwrap())),
        best_hash: [2; 32],
    };
    assert!(h.already_have(&[2; 32]));
    assert!(!h.already_have(&[0; 32]));
    assert_eq!(h.chainlock_by_hash(&[2; 32], BlsCtx::BASIC).unwrap(), sig(5));
    assert_eq!(h.best_chainlock(BlsCtx::BASIC).unwrap(), sig(5));
    h.process_new_chainlock(7, read(sig(6)), &[3; 32]);
    assert!(h.already_have(&[3; 32]));
    assert!(h.chainlock_by_hash(&[3; 32], BlsCtx::BASIC).is_none());
    assert!(!h.already_have(&[4; 32]));
}

#[test]
fn explicit_read_context_reaches_verifier() {
    let chain = Chain::default();
    let mut h = handler(&chain, ClVerdict::Invalid);
    h.process_new_chainlock(
        7,
        Read {
            msg: sig(5),
            read: BlsCtx::LEGACY,
        },
        &[1; 32],
    );
    assert_eq!(chain.0.lock().read, Some(BlsCtx::LEGACY));
}

#[derive(Default)]
struct Relay(Mutex<Vec<MessageProcessingResult>>);
impl ClRelay for Relay {
    fn post(&self, result: MessageProcessingResult) {
        self.0.lock().push(result);
    }
}

#[test]
fn external_entry_posts_as_no_peer() {
    let relay = Relay::default();
    for verdict in [ClVerdict::Valid, ClVerdict::Invalid] {
        let chain = Chain::default();
        chain.0.lock().record = Some(true);
        let mut h = handler(&chain, verdict.clone());
        h.process_external(
            Read {
                msg: sig(5),
                read: BlsCtx::BASIC,
            },
            &[1; 32],
            &relay,
        );
        assert_eq!(chain.0.lock().from, (verdict == ClVerdict::Valid).then_some(-1));
        let result = relay.0.lock().pop().unwrap();
        assert!(result.error.is_none());
        assert_eq!(result.inventory.len(), usize::from(verdict == ClVerdict::Valid));
    }
}

#[test]
fn enabled_reads_live_gate() {
    let enabled = Arc::new(AtomicBool::new(false));
    let flag = enabled.clone();
    let chain = Chain::default();
    let h = ChainLockHandler::new(
        chain.clone(),
        Verify(chain, ClVerdict::Valid),
        Arc::new(MockClock::new()),
        move || flag.load(Ordering::Relaxed),
    );
    assert!(!h.is_enabled());
    enabled.store(true, Ordering::Relaxed);
    assert!(h.is_enabled());
}

#[test]
fn serving_reencodes_signature_but_keeps_the_receipt_hash() {
    for relay in [false, true] {
        let chain = Chain::default();
        chain.0.lock().record = Some(relay);
        let mut h = handler(&chain, ClVerdict::Valid);
        let sig = signed_fixture();
        let hash = clsig_hash(&sig, ClsigHashSchemes::both(BlsCtx::BASIC)).unwrap();
        h.process_new_chainlock(7, read(sig.clone()), &hash);
        assert_eq!(h.chainlock_by_hash(&hash, BlsCtx::BASIC), Some(sig.clone()));
        let h = handler(&chain, ClVerdict::Valid);
        let served = h.chainlock_by_hash(&hash, BlsCtx::LEGACY).unwrap();
        assert_ne!(served.sig().as_bytes(), sig.sig().as_bytes());
        let decoded = BlsGlobalSig::decode(served.sig().as_bytes()).unwrap().get(BlsCtx::LEGACY);
        assert_eq!(&decoded.to_bytes(BlsCtx::BASIC.scheme()), sig.sig().as_bytes());
        assert_eq!(h.best_chainlock(BlsCtx::LEGACY), Some(served.clone()));
        let new_hash = clsig_hash(&served, ClsigHashSchemes::both(BlsCtx::LEGACY)).unwrap();
        assert_ne!(new_hash, hash);
        assert!(h.chainlock_by_hash(&new_hash, BlsCtx::BASIC).is_none());
        assert_eq!(chain.0.lock().snapshot.best_hash, hash);
        assert!(h.already_have(&hash));
        assert!(!h.already_have(&new_hash));
        chain.0.lock().snapshot = ClSigsSnapshot::default();
        assert!(h.best_chainlock(BlsCtx::BASIC).is_none());
        assert_eq!(h.chainlock_by_hash(&[0; 32], BlsCtx::BASIC).unwrap().height(), -1);
    }
}

#[test]
fn legacy_intake_serves_basic_bytes_under_the_original_hash() {
    let chain = Chain::default();
    chain.0.lock().record = Some(true);
    let mut h = handler(&chain, ClVerdict::Valid);
    let basic = signed_fixture();
    let decoded = ChainLockSig::from_wire(&basic, BlsCtx::BASIC).unwrap();
    let legacy = decoded.to_wire(BlsCtx::LEGACY);
    assert_ne!(legacy.sig().as_bytes(), basic.sig().as_bytes());
    let hash = clsig_hash(&legacy, ClsigHashSchemes::both(BlsCtx::LEGACY)).unwrap();
    let result = h.process_new_chainlock(
        7,
        Read {
            msg: legacy,
            read: BlsCtx::LEGACY,
        },
        &hash,
    );
    assert_eq!(result.inventory.len(), 1);
    assert_eq!(chain.0.lock().read, Some(BlsCtx::LEGACY));
    assert_eq!(h.best_chainlock(BlsCtx::BASIC), Some(basic.clone()));
    assert_eq!(h.chainlock_by_hash(&hash, BlsCtx::BASIC), Some(basic.clone()));
    let basic_hash = clsig_hash(&basic, ClsigHashSchemes::both(BlsCtx::BASIC)).unwrap();
    assert_ne!(hash, basic_hash);
    assert_eq!(chain.0.lock().snapshot.best_hash, hash);
    assert!(h.chainlock_by_hash(&basic_hash, BlsCtx::BASIC).is_none());
    assert!(h.already_have(&hash));
    assert!(!h.already_have(&basic_hash));
}

fn signed_fixture() -> ClSig {
    let fixtures: Value =
        serde_json::from_str(include_str!("../../dash-evo/tests/data/cbtx_testnet.json")).unwrap();
    let fixture =
        fixtures["blocks"].as_array().unwrap().iter().find(|b| b["height"] == 1_569_000).unwrap();
    let bytes: [u8; 96] = try_parse_hex(fixture["cbTx"]["bestCLSignature"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    ClSig::new(5, [4; 32].into(), bytes.into())
}
