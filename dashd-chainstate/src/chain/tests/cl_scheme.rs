//! R (LK1a-2a, P24): V keeps a ChainLock's signature as the object the handler decoded once under
//! the scheme in force at its receipt, never as bytes. The hash it was seen under is fixed at
//! intake (`chainlock.cpp:115-116` at v24.0.0), and a reader gets the bytes under the scheme in
//! force at its own call (`bls.h:178-208`).
//!
//! These cross V19 on BL-839's recorded chain (`tests/data/v19-window/r3`, V19 at 150), a lock
//! received under legacy at 140 and read under basic from 150 on. The signature is one point
//! whose two encodings differ.

use std::sync::Arc;

use dash_consensus::BlsCtx;
use dash_num::Hash256;
use dash_pkc::bls::{BlsEagerSig, BlsGlobalSig, BlsScIetf, BlsSecretKey};
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

use super::evo_chain::{Kept, decode, linked, recorded_config_on};
use super::locks::{cl_rig, intake, settle};
use super::support::{Mined, Rig, RigConfig};
use super::v19_window::{R3_CHAIN, R3_SUMMARY, last_time, params, scheme, summary};
use crate::chain::{ChainLockSig, ClSigs};

/// R3's V19 height.
const V19: usize = 150;
/// The tip a lock is received at, under legacy.
const BEFORE: usize = 140;
/// A tip past V19.
const AFTER: usize = 152;

/// A rig on R3's chain, and the point every lock here signs with.
struct Crossing {
    rig: Rig,
    kept: Kept,
    m: Vec<Mined>,
    point: BlsEagerSig,
}

/// A lock as received: what V keeps, and the hash it was seen under.
struct Received {
    lock: Arc<ChainLockSig>,
    seen: Hash256,
}

impl Crossing {
    fn new() -> Crossing {
        let s = summary(R3_SUMMARY);
        assert_eq!(s["v19_height"], V19);
        let raw = decode(R3_CHAIN);
        let (cfg, kept) =
            recorded_config_on(RigConfig::default(), params(&s), last_time(&raw), None);
        let (rig, _) = cl_rig(cfg);
        let m = linked(&rig.genesis, raw);
        assert_eq!(m.len(), 160);
        let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[5; 32]).expect("a key");
        let point = BlsGlobalSig::decode(&key.sign_message(b"a chainlock").to_bytes())
            .expect("a signature")
            .get(BlsCtx::BASIC);
        assert!(point.is_valid());
        Crossing {
            rig,
            kept,
            m,
            point,
        }
    }

    /// The chain's blocks above `from` up to `to`, connected.
    fn connect(&self, from: usize, to: usize) {
        for b in &self.m[from..to] {
            self.rig.accept(b);
        }
        settle(&self.rig);
    }

    fn legacy_bytes(&self) -> [u8; 96] {
        self.point.to_bytes(BlsCtx::LEGACY.scheme())
    }

    fn basic_bytes(&self) -> [u8; 96] {
        self.point.to_bytes(BlsCtx::BASIC.scheme())
    }

    /// Records the lock `(height, hash)` as the handler would receive it under legacy: decoded
    /// once from its legacy bytes, its hash taken then. Whether `RecordChainLock` relays it.
    fn record(&self, height: i32, hash: BlockHash) -> (Received, bool) {
        assert_eq!(scheme(&self.rig), BlsCtx::LEGACY, "received before V19");
        let wire = dash_p2p_core::ClSig::new(height, hash, self.legacy_bytes().into());
        let lock = ChainLockSig::from_wire(&wire, BlsCtx::LEGACY).expect("a signature");
        assert_eq!(lock.sig, self.point, "the legacy bytes decode to the point");
        let seen = Hash256::from_lendian(lock.serialize_hash(BlsCtx::LEGACY));
        assert_ne!(seen, Hash256::from_lendian(lock.serialize_hash(BlsCtx::BASIC)));
        intake(&self.rig);
        let lock = Arc::new(lock);
        let relay =
            self.rig.v.record_chainlock(lock.clone(), seen, None).recv().expect("V replies");
        settle(&self.rig);
        (
            Received {
                lock,
                seen,
            },
            relay.relay,
        )
    }

    fn sigs(&self) -> Arc<ClSigs> {
        self.rig.v.view_handle().cl()
    }

    /// `got` is `received`'s object, whose bytes follow the scheme now in force: the basic
    /// encoding of the point, not the legacy one it arrived as.
    fn assert_current_scheme(&self, got: &Arc<ChainLockSig>, received: &Received) {
        assert_eq!(scheme(&self.rig), BlsCtx::BASIC, "read after V19");
        assert!(Arc::ptr_eq(got, &received.lock), "the object V was given, not a decode of bytes");
        assert_eq!(got.sig_bytes(BlsCtx::BASIC), self.basic_bytes());
        assert_ne!(got.sig_bytes(BlsCtx::BASIC), self.legacy_bytes());
        assert_eq!(got.sig_bytes(BlsCtx::LEGACY), self.legacy_bytes());
        assert_eq!(got.to_wire(BlsCtx::BASIC).sig().as_bytes(), &self.basic_bytes());
    }

    /// The hash it was seen under still names it, and the hash of its basic serialization,
    /// which a re-hash after V19 would give, names nothing.
    fn assert_hash_fixed(&self, sigs: &ClSigs, received: &Received) {
        assert_eq!(sigs.best_hash, received.seen);
        let reserialized = Hash256::from_lendian(received.lock.serialize_hash(BlsCtx::BASIC));
        assert_ne!(reserialized, received.seen);
        assert!(sigs.by_hash(&reserialized).is_none(), "the hash is fixed at intake");
        assert!(sigs.by_hash(&Hash256::from_lendian([9; 32])).is_none());
        let by_hash = sigs.by_hash(&received.seen).expect("answered by the hash it was seen under");
        self.assert_current_scheme(&by_hash, received);
    }
}

/// A lock on a known block, taken at a V19-crossing: `by_hash`, `best` and `best_known` are the
/// one object, read under basic.
#[test]
fn a_known_lock_received_under_legacy_is_read_under_basic_after_v19() {
    let c = Crossing::new();
    c.connect(0, BEFORE);
    let (received, relay) = c.record(120, c.m[119].hash());
    assert!(relay, "a lock on a known block at its height");
    c.connect(BEFORE, AFTER);

    let sigs = c.sigs();
    c.assert_current_scheme(sigs.best.as_ref().expect("best"), &received);
    c.assert_current_scheme(sigs.best_known.as_ref().expect("best_known"), &received);
    c.assert_hash_fixed(&sigs, &received);
    let view = c.rig.v.view();
    c.assert_current_scheme(view.best_chainlock().expect("the view's best"), &received);
    c.kept.assert_no_abort(&c.rig);
}

/// A reset after the crossing leaves Core's null signature, which has no bytes in either scheme,
/// and `getdata(MSG_CLSIG, 0)`'s answer, the null sig, is the same under both.
#[test]
fn a_reset_after_v19_clears_the_lock_received_before_it() {
    let c = Crossing::new();
    c.connect(0, BEFORE);
    let (received, _) = c.record(120, c.m[119].hash());
    c.connect(BEFORE, AFTER);

    let sporks = c.rig.sporks.clone().expect("a fresh rig");
    let flip = |enabled: bool| {
        sporks.set(enabled);
        intake(&c.rig);
    };
    flip(false);
    flip(true);
    settle(&c.rig);

    let sigs = c.sigs();
    assert!(sigs.best.is_none() && sigs.best_known.is_none());
    assert_eq!(sigs.best_hash, Hash256::ZERO);
    assert!(sigs.by_hash(&received.seen).is_none(), "the lock is gone");
    let null = sigs.by_hash(&Hash256::ZERO).expect("the null sig answers hash 0");
    assert_eq!(*null, ChainLockSig::null());
    for write in [BlsCtx::LEGACY, BlsCtx::BASIC] {
        assert_eq!(null.sig_bytes(write), [0; 96], "{write:?}");
    }
    c.kept.assert_no_abort(&c.rig);
}

/// A lock on a block V does not have yet is pending: `best` only, the same object across V19, and
/// `best_known` once the header comes (`AcceptedBlockHeader`).
#[test]
fn a_pending_lock_crosses_v19_and_becomes_known_as_the_same_object() {
    let c = Crossing::new();
    c.connect(0, BEFORE);
    let (received, relay) = c.record(155, c.m[154].hash());
    assert!(relay, "a pending lock is relayed");
    c.connect(BEFORE, AFTER);

    let sigs = c.sigs();
    c.assert_current_scheme(sigs.best.as_ref().expect("best"), &received);
    assert!(sigs.best_known.is_none(), "its block is not known yet");
    c.assert_hash_fixed(&sigs, &received);

    c.connect(AFTER, 156);
    let sigs = c.sigs();
    c.assert_current_scheme(sigs.best.as_ref().expect("best"), &received);
    c.assert_current_scheme(sigs.best_known.as_ref().expect("best_known"), &received);
    c.assert_hash_fixed(&sigs, &received);
    c.kept.assert_no_abort(&c.rig);
}

/// A lock naming a known block at another height moves `best` and nothing else
/// (`chainlock.cpp:115-125`): not known, not relayed, the same object across V19.
#[test]
fn a_height_mismatch_lock_crosses_v19_as_best_only() {
    let c = Crossing::new();
    c.connect(0, BEFORE);
    let (received, relay) = c.record(130, c.m[119].hash());
    assert!(!relay, "a height mismatch is not relayed");
    c.connect(BEFORE, AFTER);

    let sigs = c.sigs();
    c.assert_current_scheme(sigs.best.as_ref().expect("best"), &received);
    assert!(sigs.best_known.is_none());
    c.assert_hash_fixed(&sigs, &received);
    c.kept.assert_no_abort(&c.rig);
}
