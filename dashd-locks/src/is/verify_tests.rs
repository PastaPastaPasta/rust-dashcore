//! S1c against Core's captures: every `isdlock` of the LQ3a capture (non-rotated
//! `llmq_test_instantsend`, basic scheme) and of the BL-218 capture (rotated `llmq_test_dip0024`,
//! legacy scheme; `contrib/node/lq3a-captures/README.md:39-46`) verifies through a view built
//! from the capture, and the ingestion entry writes what Core's `WriteNewISLock` writes.
//!
//! A capture holds each quorum's key, base hash and height, not its commitment or the block that
//! mined it. Each quorum is rebuilt as a mined commitment carrying Core's key. A rotated scan
//! follows Core's store block (`llmq/quorumsman.cpp:179-207`): a cycle's quorums are mined by its
//! mining window's end and not before its start; a start that reads inside a window, where the
//! capture cannot tell, fails the test rather than guess.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use dash_consensus::BlsCtx;
use dash_consensus::primitives::P2pCommitment;
use dash_evo::llmq::{
    LlmqCommitment, ScanEntry, VerifiedQuorum, commitment_version, select_from_scan,
};
use dash_evo::locks::{
    LockError, LockReject, QuorumScan, QuorumScanView, QuorumStatus, ScanLookup, ScannedQuorum,
    isdlock_hash, islock_request_id,
};
use dash_kv::MemoryEngine;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, LlmqType, Network, Release};
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use serde_json::Value;

use super::{IngestError, IsIngest};
use crate::is::db::{IsDb, SCHEMA};
use crate::is::seams::IsStore;
use crate::is::{IsLockHash, IsTipCache, LockedTx};

fn hex(s: &str) -> Vec<u8> {
    assert_eq!(s.len() % 2, 0, "{s}");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

/// A hash as the RPCs print it (display order), in wire order.
fn rpc(s: &str) -> [u8; 32] {
    let mut bytes: [u8; 32] = hex(s).try_into().expect("32 bytes");
    bytes.reverse();
    bytes
}

fn lq3a() -> &'static Value {
    static F: OnceLock<Value> = OnceLock::new();
    F.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../../dash-evo/tests/data/lq3a-regtest-captures.json"
        ))
        .expect("the LQ3a capture")
    })
}

fn lq3r() -> &'static Value {
    static F: OnceLock<Value> = OnceLock::new();
    F.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../../dash-oracle-sys/tests/data/lq3r-regtest-v24.0.captures.json"
        ))
        .expect("the BL-218 capture")
    })
}

/// Regtest with the capture network's chain arguments (every node's are the same).
fn params(f: &Value) -> ChainSetup {
    let args = f["provenance"]["extra_args"][1].as_array().expect("extra_args");
    let mut builder = ChainParamsBuilder::new(Network::Regtest, Release::V24_0);
    for arg in args {
        let (flag, value) = arg.as_str().expect("an arg").split_once('=').unwrap_or_default();
        let option = match flag {
            "-llmqtestinstantsenddip0024" => ChainOption::LlmqTestInstantSendDip0024,
            "-llmqtestparams" => ChainOption::LlmqTestParams,
            "-llmqtestinstantsendparams" => ChainOption::LlmqTestInstantSendParams,
            "-llmqtestplatformparams" => ChainOption::LlmqTestPlatformParams,
            // Activation heights only move the BLS flag, which each check is given.
            _ => continue,
        };
        builder = builder.arg(option, value);
    }
    builder.build().expect("the capture's regtest")
}

fn locks(f: &Value) -> Vec<(LockedTx, [u8; 32])> {
    f["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter(|m| m["command"] == "isdlock")
        .map(|m| {
            let payload = hex(m["payload"].as_str().expect("a payload"));
            let lock = LockedTx::decode(&mut &payload[..]).expect("Core's isdlock");
            (lock, rpc(m["inv_hash"].as_str().expect("an inv hash")))
        })
        .collect()
}

fn payload(lock: &LockedTx) -> Vec<u8> {
    let mut bytes = Vec::new();
    lock.encode(&mut bytes);
    bytes
}

/// `lock` naming another cycle block.
fn with_cycle(lock: &LockedTx, cycle: [u8; 32]) -> LockedTx {
    LockedTx::new(lock.version(), lock.inputs().clone(), lock.txid(), cycle.into(), lock.sig())
}

/// `lock` with another lock's signature: a valid point over another message.
fn with_sig(lock: &LockedTx, other: &LockedTx) -> LockedTx {
    LockedTx::new(
        lock.version(),
        lock.inputs().clone(),
        lock.txid(),
        lock.cycle_hash(),
        other.sig(),
    )
}

/// A quorum as V's mined row would carry it: a commitment with Core's key and no signers.
fn mined_quorum(
    llmq_type: LlmqType,
    hash: [u8; 32],
    index: i16,
    key: &[u8],
    rotated: bool,
    basic: bool,
) -> VerifiedQuorum {
    let version = commitment_version(rotated, basic);
    let mut bytes = Vec::new();
    version.encode(&mut bytes);
    bytes.push(llmq_type.to_base());
    bytes.extend_from_slice(&hash);
    if rotated {
        index.encode(&mut bytes);
    }
    // Empty signers and valid-members bitsets.
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(key);
    bytes.extend_from_slice(&[0; 32 + 96 + 96]);
    let qc = LlmqCommitment::decode(&mut &bytes[..]).expect("a commitment");
    VerifiedQuorum::mined(&P2pCommitment::new(qc))
}

/// One quorum of a capture.
#[derive(Clone)]
struct Quorum {
    height: u32,
    /// Core's key bytes.
    key: Vec<u8>,
    quorum: VerifiedQuorum,
}

/// A [`QuorumScanView`] built from a capture at a chosen tip.
struct CaptureView {
    params: ChainSetup,
    tip: u32,
    /// Every header the view knows: the capture's quorum bases.
    heights: BTreeMap<[u8; 32], u32>,
    /// The quorums of each type by hash.
    quorums: BTreeMap<(u8, [u8; 32]), Quorum>,
    /// Each scan's start height, in order.
    scans: RefCell<Vec<u32>>,
}

impl CaptureView {
    fn new(f: &Value, tip: u32) -> CaptureView {
        let params = params(f);
        let basic = f["bls_scheme"] == "basic";
        let mut view = CaptureView {
            params,
            tip,
            heights: BTreeMap::new(),
            quorums: BTreeMap::new(),
            scans: RefCell::new(Vec::new()),
        };
        for (llmq_type, list) in f["quorums"].as_object().expect("quorums") {
            let llmq_type = LlmqType::from_base(llmq_type.parse().expect("a type"));
            let rotated = view.params.consensus.llmq(llmq_type).expect("configured").use_rotation;
            for q in list.as_array().expect("a list") {
                let hash = rpc(q["hash"].as_str().expect("a hash"));
                let height = u32::try_from(q["height"].as_u64().expect("a height")).expect("small");
                let index = i16::try_from(q["index"].as_i64().expect("an index")).expect("small");
                let key = hex(q["public_key"].as_str().expect("a key"));
                let quorum = mined_quorum(llmq_type, hash, index, &key, rotated, basic);
                view.heights.insert(hash, height);
                view.add(llmq_type, height, key, quorum);
            }
        }
        view
    }

    fn add(&mut self, llmq_type: LlmqType, height: u32, key: Vec<u8>, quorum: VerifiedQuorum) {
        let at = (llmq_type.to_base(), *quorum.quorum_hash().as_bytes());
        self.quorums.insert(
            at,
            Quorum {
                height,
                key,
                quorum,
            },
        );
    }

    fn rotated_type(&self) -> LlmqType {
        self.params.consensus.llmq_type_dip0024_instant_send
    }

    /// The newest cycle's quorums of `llmq_type` mined by the store block of a scan at `start`.
    fn core_scan(
        &self,
        llmq_type: LlmqType,
        start: u32,
        count: usize,
    ) -> Result<ScanLookup, String> {
        let llmq = self.params.consensus.llmq(llmq_type).expect("configured");
        let interval = u32::from(llmq.dkg_interval);
        let (ws, we) =
            (u32::from(llmq.dkg_mining_window_start), u32::from(llmq.dkg_mining_window_end));
        let cycle = start - start % interval;
        let store = if start < cycle + ws {
            if cycle + we < interval {
                return Ok(ScanLookup::Held(QuorumScan::default()));
            }
            cycle + we - interval
        } else if start > cycle + we {
            cycle + we
        } else {
            start
        };
        let mut cycles: BTreeMap<u32, Vec<&Quorum>> = BTreeMap::new();
        for ((t, _), q) in &self.quorums {
            if *t == llmq_type.to_base() {
                cycles.entry(q.height - q.height % interval).or_default().push(q);
            }
        }
        let mut entries = Vec::new();
        for (base, mut set) in cycles.into_iter().rev() {
            if store < base + ws {
                continue;
            }
            if store < base + we {
                return Err(format!("a scan at {start} reads inside cycle {base}'s mining window"));
            }
            set.sort_by_key(|q| q.quorum.quorum_index());
            entries.extend(set.into_iter().map(|q| {
                ScannedQuorum::verified(dash_consensus::BlockId(q.height), q.quorum.clone())
            }));
            if entries.len() >= count {
                break;
            }
        }
        entries.truncate(count);
        Ok(ScanLookup::Held(QuorumScan {
            rotation_enabled: llmq.use_rotation,
            entries,
        }))
    }
}

impl QuorumScanView for CaptureView {
    type Error = String;

    fn tip_height(&self) -> u32 {
        self.tip
    }

    fn scan(&self, llmq_type: LlmqType, start: u32) -> Result<ScanLookup, String> {
        self.scans.borrow_mut().push(start);
        let llmq = self.params.consensus.llmq(llmq_type).expect("configured");
        self.core_scan(llmq_type, start, usize::from(llmq.signing_active_quorum_count))
    }

    fn scan_at_tip(&self, llmq_type: LlmqType, count: usize) -> Result<ScanLookup, String> {
        self.core_scan(llmq_type, self.tip, count)
    }

    fn block_height(&self, hash: &BlockHash) -> Result<Option<u32>, String> {
        Ok(self.heights.get(hash.as_bytes()).copied())
    }

    fn quorum(
        &self,
        llmq_type: LlmqType,
        hash: &QuorumHash,
    ) -> Result<Option<QuorumStatus>, String> {
        let q = self.quorums.get(&(llmq_type.to_base(), *hash.as_bytes()));
        Ok(q.map(|q| QuorumStatus::Verified(q.quorum.clone())))
    }
}

fn memory_db() -> IsDb<MemoryEngine> {
    IsDb::open(Arc::new(MemoryEngine::new(&SCHEMA))).expect("a fresh store")
}

fn tip_cache(height: u32) -> IsTipCache {
    let cache = IsTipCache::new();
    cache.set(i32::try_from(height).expect("small"));
    cache
}

/// The flag fixed at one scheme.
fn fixed(bls: BlsCtx) -> impl Fn() -> BlsCtx {
    move || bls
}

/// The flag reading `seq` in order, recording how often it was read.
struct Sequence {
    seq: Vec<BlsCtx>,
    read: RefCell<usize>,
}

impl Sequence {
    fn new(seq: &[BlsCtx]) -> Sequence {
        Sequence {
            seq: seq.to_vec(),
            read: RefCell::new(0),
        }
    }
}

impl super::SchemeFlag for Sequence {
    fn sample(&self) -> BlsCtx {
        let mut read = self.read.borrow_mut();
        let bls = self.seq[*read];
        *read += 1;
        bls
    }
}

type Ingested = Result<IsLockHash, IngestError<String>>;

fn ingest(
    view: &CaptureView,
    store: &dyn IsStore,
    flag: &dyn super::SchemeFlag,
    lock: &LockedTx,
    mined: Option<u32>,
) -> Ingested {
    let cache = tip_cache(view.tip);
    let ingest = IsIngest {
        params: &view.params,
        store,
        tip_cache: &cache,
    };
    ingest.ingest_verified(view, flag, &payload(lock), mined)
}

fn rejected(r: Ingested) -> LockReject {
    match r {
        Err(IngestError::Verify(LockError::Rejected(reject))) => reject,
        other => panic!("not a rejection: {other:?}"),
    }
}

// ---- LQ3a: non-rotated, basic --------------------------------------------------------------

fn lq3a_view() -> CaptureView {
    let tip = u32::try_from(lq3a()["tip_height"].as_u64().expect("a tip")).expect("small");
    CaptureView::new(lq3a(), tip)
}

#[test]
fn every_lq3a_lock_verifies_and_is_written() {
    let view = lq3a_view();
    assert!(!view.params.consensus.llmq(view.rotated_type()).expect("configured").use_rotation);
    let captured = locks(lq3a());
    assert!(!captured.is_empty());
    let db = memory_db();
    for (lock, inv) in &captured {
        let hash = ingest(&view, &db, &fixed(BlsCtx::BASIC), lock, None).expect("Core's lock");
        assert_eq!(&hash, inv, "the hash Core announced");
        assert_eq!(db.by_hash(&hash, false).expect("a read").as_ref(), Some(lock));
        assert_eq!(db.by_txid(&lock.txid()).expect("a read").as_ref(), Some(lock));
    }
    // A non-rotated lock names its quorum by the cycle hash: no scan.
    assert!(view.scans.borrow().is_empty());
}

#[test]
fn lq3a_rejections() {
    let view = lq3a_view();
    let captured = locks(lq3a());
    let db = memory_db();
    let flag = fixed(BlsCtx::BASIC);
    let (lock, _) = &captured[0];
    // Another lock's signature: a readable signature the named quorum rejects on both passes.
    let other = captured.iter().map(|(l, _)| l).find(|l| l.sig() != lock.sig()).expect("two locks");
    assert_eq!(
        rejected(ingest(&view, &db, &flag, &with_sig(lock, other), None)),
        LockReject::InvalidSignature
    );
    // A cycle block the view does not know, and the null hash.
    assert_eq!(
        rejected(ingest(&view, &db, &flag, &with_cycle(lock, [0x77; 32]), None)),
        LockReject::UnknownCycle
    );
    assert_eq!(
        rejected(ingest(&view, &db, &flag, &with_cycle(lock, [0; 32]), None)),
        LockReject::UnknownCycle
    );
    // Under the legacy flag Core's basic signature is never accepted.
    assert!(ingest(&view, &db, &fixed(BlsCtx::LEGACY), lock, None).is_err());
    // Nothing was written.
    assert_eq!(db.by_txid(&lock.txid()).expect("a read"), None);
}

// ---- BL-218: rotated, legacy ---------------------------------------------------------------

/// A lock of `provenance.locks`: its capture, its cycle height and Core's signing tip.
struct Signed {
    lock: LockedTx,
    cycle: u32,
    tip: u32,
}

fn lq3r_locks() -> BTreeMap<String, Signed> {
    let captured = locks(lq3r());
    lq3r()["provenance"]["locks"]
        .as_array()
        .expect("locks")
        .iter()
        .map(|l| {
            let txid = rpc(l["txid"].as_str().expect("a txid"));
            let (lock, _) =
                captured.iter().find(|(c, _)| c.txid().as_bytes() == &txid).expect("captured");
            assert_eq!(
                lock.cycle_hash().as_bytes(),
                &rpc(l["cycle_hash"].as_str().expect("cycle"))
            );
            let signed = Signed {
                lock: lock.clone(),
                cycle: u32::try_from(l["cycle_height"].as_u64().expect("a cycle")).expect("small"),
                tip: u32::try_from(l["signed_at_tip"].as_u64().expect("a tip")).expect("small"),
            };
            (l["name"].as_str().expect("a name").to_owned(), signed)
        })
        .collect()
}

/// The hash of the capture's block at `height` (a quorum base).
fn lq3r_block(height: u32) -> [u8; 32] {
    let view = CaptureView::new(lq3r(), 0);
    *view.heights.iter().find(|(_, h)| **h == height).expect("a base at that height").0
}

/// The cycle of the quorum based at `hash`.
fn quorum_cycle(view: &CaptureView, hash: &[u8; 32]) -> u32 {
    let interval =
        u32::from(view.params.consensus.llmq(view.rotated_type()).expect("ok").dkg_interval);
    view.heights[hash] - view.heights[hash] % interval
}

#[test]
fn the_rotated_capture_is_legacy_with_cycles_264_and_288() {
    let f = lq3r();
    assert_eq!(f["bls_scheme"], "legacy");
    assert_eq!(f["provenance"]["cycles"]["F"], 264);
    assert_eq!(f["provenance"]["cycles"]["C"], 288);
    let view = CaptureView::new(f, 0);
    let llmq = view.params.consensus.llmq(view.rotated_type()).expect("configured");
    assert!(llmq.use_rotation);
    assert_eq!(llmq.dkg_interval, 24);
    let signed = lq3r_locks();
    let cycles: Vec<_> =
        ["A", "B", "D"].iter().map(|n| (signed[*n].cycle, signed[*n].tip)).collect();
    assert_eq!(cycles, [(264, 284), (264, 288), (288, 308)]);
}

/// Each lock at the tip Core signed it at verifies in the current set's pass, by the quorum of
/// its own cycle; the previous set is never scanned.
#[test]
fn rotated_locks_verify_at_their_signing_tips_in_the_first_pass() {
    for (name, signed) in lq3r_locks() {
        let view = CaptureView::new(lq3r(), signed.tip);
        let db = memory_db();
        let hash = ingest(&view, &db, &fixed(BlsCtx::LEGACY), &signed.lock, None)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(hash, isdlock_hash(&signed.lock));
        // The sign height is the tip (the cycle's interval has not passed), and one pass ran.
        assert_eq!(*view.scans.borrow(), [signed.tip], "{name}");
        let verified = super::verify_islock(
            &view.params,
            &view,
            dash_evo::locks::IslockSchemes::both(BlsCtx::LEGACY),
            &signed.lock,
        )
        .expect("verified");
        let quorum_hash = *verified.quorum.quorum_hash().as_bytes();
        assert_eq!(quorum_cycle(&view, &quorum_hash), signed.cycle, "{name}");
    }
}

/// A tip where only the previous set (offset `dkgInterval`) verifies. A cycle-264 lock always
/// signs at or before 287, inside cycle 264's set, so A and B are given cycle 288: at the tip
/// 311 they sign at the tip, cycle 288's quorum rejects them and the previous set's (start 287)
/// accepts them, as LQ3a D30's rewrite. D signed by cycle 288 has a later set only past the
/// capture: the view gains a cycle-312 set under other keys, and D given cycle 312 at the tip
/// 335 verifies only by the previous set, cycle 288's.
#[test]
fn rotated_locks_verify_only_in_the_previous_set_pass() {
    let signed = lq3r_locks();
    let interval = 24;
    for name in ["A", "B"] {
        let view = CaptureView::new(lq3r(), 311);
        let lock = with_cycle(&signed[name].lock, lq3r_block(288));
        assert_eq!(pass_zero_rejects(&view, &lock), 288, "{name}");
        let db = memory_db();
        ingest(&view, &db, &fixed(BlsCtx::LEGACY), &lock, None)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(*view.scans.borrow(), [311, 311 - interval], "{name}");
        assert!(db.by_hash(&isdlock_hash(&lock), false).expect("a read").is_some());
    }

    let mut view = CaptureView::new(lq3r(), 335);
    let block_312 = [0x31; 32];
    view.heights.insert(block_312, 312);
    let rotated = view.rotated_type();
    let cycle_264: Vec<_> = view
        .quorums
        .iter()
        .filter(|((t, _), q)| *t == rotated.to_base() && q.height < 288)
        .map(|(_, q)| q.clone())
        .collect();
    for q in cycle_264 {
        let index = q.quorum.quorum_index();
        let hash = if index == 0 {
            block_312
        } else {
            [0x32; 32]
        };
        let quorum = mined_quorum(rotated, hash, index, &q.key, true, false);
        view.add(rotated, 312 + u32::try_from(index).expect("an index"), q.key, quorum);
    }
    let lock = with_cycle(&signed["D"].lock, block_312);
    assert_eq!(pass_zero_rejects(&view, &lock), 312);
    let db = memory_db();
    let hash = ingest(&view, &db, &fixed(BlsCtx::LEGACY), &lock, None).expect("the previous set");
    assert_eq!(*view.scans.borrow(), [335, 335 - interval]);
    assert!(db.by_hash(&hash, false).expect("a read").is_some());
}

/// Pass 0 at `view`'s tip (sign height the tip, offset 0) for a lock signed at it: the cycle of
/// the quorum it selects, asserting that the quorum rejects the signature. The verdict comes
/// from [`PassZeroTwice`], so it is pass 0's own rather than the previous set's.
fn pass_zero_rejects(view: &CaptureView, lock: &LockedTx) -> u32 {
    let llmq_type = view.rotated_type();
    let llmq = view.params.consensus.llmq(llmq_type).expect("configured");
    let ScanLookup::Held(scan) = view.scan(llmq_type, view.tip).expect("a scan") else {
        panic!("pass 0's set is held");
    };
    let id = islock_request_id(lock.inputs());
    let selected = select_from_scan(llmq_type, llmq, scan.rotation_enabled, &scan.entries, &id)
        .expect("pass 0 selects a quorum");
    let verdict = super::verify_islock(
        &view.params,
        &PassZeroTwice(view),
        dash_evo::locks::IslockSchemes::both(BlsCtx::LEGACY),
        lock,
    );
    assert_eq!(verdict, Err(LockError::Rejected(LockReject::InvalidSignature)));
    assert_eq!(*view.scans.borrow(), [view.tip; 3]);
    view.scans.borrow_mut().clear();
    let interval = u32::from(llmq.dkg_interval);
    selected.base().0 - selected.base().0 % interval
}

/// A view whose every scan reads pass 0's set (the scan at the tip): both passes select the
/// same quorum under the same scheme, so the verdict, the last pass's, is pass 0's.
struct PassZeroTwice<'v>(&'v CaptureView);

impl QuorumScanView for PassZeroTwice<'_> {
    type Error = String;

    fn tip_height(&self) -> u32 {
        self.0.tip
    }

    fn scan(&self, llmq_type: LlmqType, _start: u32) -> Result<ScanLookup, String> {
        self.0.scan(llmq_type, self.0.tip)
    }

    fn scan_at_tip(&self, llmq_type: LlmqType, count: usize) -> Result<ScanLookup, String> {
        self.0.scan_at_tip(llmq_type, count)
    }

    fn block_height(&self, hash: &BlockHash) -> Result<Option<u32>, String> {
        self.0.block_height(hash)
    }

    fn quorum(
        &self,
        llmq_type: LlmqType,
        hash: &QuorumHash,
    ) -> Result<Option<QuorumStatus>, String> {
        self.0.quorum(llmq_type, hash)
    }
}

#[test]
fn rotated_rejections() {
    let signed = lq3r_locks();
    let a = &signed["A"];
    let view = CaptureView::new(lq3r(), a.tip);
    let db = memory_db();
    let flag = fixed(BlsCtx::LEGACY);
    // B's signature over A's message. At A's tip cycle 264's quorum rejects it and the previous
    // set (start 260, before any rotated quorum) has none: the answer is the last pass's,
    // `NoQuorum` (LQ3a D32; O2a-LQ3 D31 (a), Core's `NotVerified { invalid: false }`).
    let forged = with_sig(&a.lock, &signed["B"].lock);
    assert_eq!(rejected(ingest(&view, &db, &flag, &forged, None)), LockReject::NoQuorum);
    assert_eq!(*view.scans.borrow(), [a.tip, a.tip - 24]);
    // Named for cycle 288 at the tip 311, both sets have a quorum and both reject it.
    let late = CaptureView::new(lq3r(), 311);
    let forged = with_cycle(&forged, lq3r_block(288));
    assert_eq!(rejected(ingest(&late, &db, &flag, &forged, None)), LockReject::InvalidSignature);
    assert_eq!(
        rejected(ingest(&view, &db, &flag, &with_cycle(&a.lock, [0x77; 32]), None)),
        LockReject::UnknownCycle
    );
    // The index-1 quorum's base, at 265, is a known block off the interval boundary.
    assert_eq!(
        rejected(ingest(&view, &db, &flag, &with_cycle(&a.lock, lq3r_block(265)), None)),
        LockReject::InvalidCycle
    );
    assert_eq!(db.by_txid(&a.lock.txid()).expect("a read"), None);
}

/// The flag is read three times, in Core's order: at the read, then at each pass's start
/// (LQ3a D21/D22). A lock only the previous set verifies is accepted exactly when that pass's
/// sample is the legacy scheme it was signed in, whatever the current pass's sample.
#[test]
fn the_scheme_is_sampled_per_pass() {
    use BlsCtx as B;
    let signed = lq3r_locks();
    let lock = with_cycle(&signed["A"].lock, lq3r_block(288));
    for (seq, accepted) in [
        ([B::LEGACY, B::LEGACY, B::LEGACY], true),
        ([B::LEGACY, B::BASIC, B::LEGACY], true),
        ([B::LEGACY, B::LEGACY, B::BASIC], false),
        ([B::LEGACY, B::BASIC, B::BASIC], false),
    ] {
        let view = CaptureView::new(lq3r(), 311);
        let flag = Sequence::new(&seq);
        let got = ingest(&view, &memory_db(), &flag, &lock, None);
        assert_eq!(got.is_ok(), accepted, "{seq:?}: {got:?}");
        assert_eq!(*flag.read.borrow(), 3, "{seq:?}");
    }
    // A lock the current set verifies: the previous pass's sample is never used.
    let a = &signed["A"];
    let view = CaptureView::new(lq3r(), a.tip);
    let flag = Sequence::new(&[B::LEGACY, B::LEGACY, B::BASIC]);
    assert!(ingest(&view, &memory_db(), &flag, &a.lock, None).is_ok());
    // The read's sample decides how the signature reads: under the basic flag Core's legacy
    // signature is not accepted, though both passes check under the legacy flag.
    let flag = Sequence::new(&[B::BASIC, B::LEGACY, B::LEGACY]);
    assert!(ingest(&view, &memory_db(), &flag, &a.lock, None).is_err());
}

// ---- S3.0: the pass API on the rotated capture ---------------------------------------------

/// The cycle of the rotated quorum `quorum`, from its base height.
fn rotated_cycle(view: &CaptureView, quorum: &VerifiedQuorum) -> u32 {
    let at = (view.rotated_type().to_base(), *quorum.quorum_hash().as_bytes());
    let height = view.quorums[&at].height;
    height - height % 24
}

/// The previous-set cases of `rotated_locks_verify_only_in_the_previous_set_pass`, each pass
/// on its own through the public API: `islock_precheck` once, then per pass
/// `islock_sign_target` (what S3's batch pushes) and `islock_pass` (the individual check).
/// The current set's quorum, of cycle `current`, rejects the signature; the previous set's
/// (start `tip - dkgInterval`), of cycle `previous`, verifies it. Core:
/// `instantsend/net_instantsend.cpp` v24:136-163 (the sign height, the selection and the pushed
/// `SignHash`), :339-353 (offset 0, then `dkgInterval`); `llmq/quorumsman.cpp` v24:741-751.
fn previous_set_only_by_pass(view: &CaptureView, lock: &LockedTx, current: u32, previous: u32) {
    use dash_evo::locks::{Pass, islock_pass, islock_precheck, islock_sign_target};
    use dash_pkc::bls::core::{BlsVerdict, verify_insecure};
    let params = &view.params;
    let legacy = BlsCtx::LEGACY;
    let pre = islock_precheck(params, view, legacy, lock).expect("prechecked");
    assert_eq!(pre.llmq_type, view.rotated_type());
    assert_eq!((pre.interval, pre.sign_height), (24, -1));
    assert_eq!(pre.id, islock_request_id(lock.inputs()));
    for (pass, cycle, verdict) in [
        (Pass::Current, current, BlsVerdict::Invalid),
        (Pass::Previous, previous, BlsVerdict::Valid),
    ] {
        let (quorum, hash, key) =
            islock_sign_target(params, view, &pre, lock, pass).expect("a quorum");
        assert_eq!(rotated_cycle(view, &quorum), cycle, "{pass:?}");
        assert_eq!(
            hash,
            dash_evo::locks::sign_hash(
                pre.llmq_type,
                quorum.quorum_hash(),
                &pre.id,
                lock.txid().as_bytes()
            )
        );
        assert_eq!(&key, quorum.public_key());
        assert_eq!(verify_insecure(&pre.sig, &key, &hash, legacy.scheme()), verdict, "{pass:?}");
        let passed = islock_pass(params, view, &pre, lock, pass, legacy);
        match verdict {
            BlsVerdict::Valid => assert_eq!(passed, Ok(quorum)),
            _ => assert_eq!(passed, Err(LockError::Rejected(LockReject::InvalidSignature))),
        }
    }
    let tip = view.tip;
    assert_eq!(*view.scans.borrow(), [tip, tip, tip - 24, tip - 24]);
}

#[test]
fn rotated_previous_set_cases_through_the_pass_api() {
    let signed = lq3r_locks();
    for name in ["A", "B"] {
        let view = CaptureView::new(lq3r(), 311);
        let lock = with_cycle(&signed[name].lock, lq3r_block(288));
        previous_set_only_by_pass(&view, &lock, 288, 264);
    }

    // D under a cycle-312 set of other keys, as in the ingestion case.
    let mut view = CaptureView::new(lq3r(), 335);
    let block_312 = [0x31; 32];
    view.heights.insert(block_312, 312);
    let rotated = view.rotated_type();
    let cycle_264: Vec<_> = view
        .quorums
        .iter()
        .filter(|((t, _), q)| *t == rotated.to_base() && q.height < 288)
        .map(|(_, q)| q.clone())
        .collect();
    for q in cycle_264 {
        let index = q.quorum.quorum_index();
        let hash = if index == 0 {
            block_312
        } else {
            [0x32; 32]
        };
        let quorum = mined_quorum(rotated, hash, index, &q.key, true, false);
        view.add(rotated, 312 + u32::try_from(index).expect("an index"), q.key, quorum);
    }
    let lock = with_cycle(&signed["D"].lock, block_312);
    previous_set_only_by_pass(&view, &lock, 312, 288);
}

// ---- Ingestion ----------------------------------------------------------------------------

#[test]
fn ingestion_refuses_when_the_tip_cache_is_not_the_active_tip() {
    let a = &lq3r_locks()["A"];
    let view = CaptureView::new(lq3r(), a.tip);
    let db = memory_db();
    for cached in [0, a.tip - 1, a.tip + 1] {
        let cache = tip_cache(cached);
        let ingest = IsIngest {
            params: &view.params,
            store: &db,
            tip_cache: &cache,
        };
        let got = ingest.ingest_verified(&view, &fixed(BlsCtx::LEGACY), &payload(&a.lock), Some(1));
        match got {
            Err(IngestError::NotQuiescent {
                cached: c,
                active,
            }) => assert_eq!((c, active), (i32::try_from(cached).unwrap(), a.tip)),
            other => panic!("{cached}: {other:?}"),
        }
    }
    // Refused before any read: no scan, no row.
    assert!(view.scans.borrow().is_empty());
    assert_eq!(db.by_txid(&a.lock.txid()).expect("a read"), None);
}

#[test]
fn ingestion_writes_the_lock_and_its_mined_mark() {
    let signed = lq3r_locks();
    let (a, d) = (&signed["A"], &signed["D"]);
    let db = memory_db();
    let flag = fixed(BlsCtx::LEGACY);
    let hash_a = ingest(&CaptureView::new(lq3r(), a.tip), &db, &flag, &a.lock, Some(290))
        .expect("A verifies");
    let hash_d =
        ingest(&CaptureView::new(lq3r(), d.tip), &db, &flag, &d.lock, None).expect("D verifies");
    for (hash, lock) in [(hash_a, &a.lock), (hash_d, &d.lock)] {
        assert_eq!(db.by_hash(&hash, false).expect("a read").as_ref(), Some(lock));
        assert_eq!(db.by_input(&lock.inputs()[0]).expect("a read").as_ref(), Some(lock));
    }
    // A's mined mark is at 290: nothing confirms below it, A confirms at it; D has none.
    assert!(db.remove_confirmed(289).expect("a write").is_empty());
    let confirmed = db.remove_confirmed(290).expect("a write");
    assert_eq!(confirmed.keys().collect::<Vec<_>>(), [&hash_a]);
    assert!(db.remove_confirmed(i32::MAX).expect("a write").is_empty());
}

#[test]
fn ingestion_refuses_bad_bytes_and_heights_before_writing() {
    let a = &lq3r_locks()["A"];
    let view = CaptureView::new(lq3r(), a.tip);
    let db = memory_db();
    let flag = fixed(BlsCtx::LEGACY);
    let bytes = payload(&a.lock);
    let cache = tip_cache(a.tip);
    let ingest = IsIngest {
        params: &view.params,
        store: &db,
        tip_cache: &cache,
    };
    let got = ingest.ingest_verified(&view, &flag, &bytes[..bytes.len() - 1], None);
    assert!(matches!(got, Err(IngestError::Parse(_))), "{got:?}");
    let too_high = u32::try_from(i32::MAX).unwrap() + 1;
    let got = ingest.ingest_verified(&view, &flag, &bytes, Some(too_high));
    assert!(matches!(got, Err(IngestError::MinedHeight(h)) if h == too_high), "{got:?}");
    assert_eq!(db.by_hash(&isdlock_hash(&a.lock), false).expect("a read"), None);
    // Trailing bytes are ignored, as Core's stream read ignores them.
    let mut longer = bytes.clone();
    longer.push(0);
    assert_eq!(
        ingest.ingest_verified(&view, &flag, &longer, None).ok(),
        Some(isdlock_hash(&a.lock))
    );
}

#[cfg(feature = "test-support")]
mod with_store_double {
    use super::*;
    use crate::is::seams::{InMemoryIsStore, IsStoreCall};

    #[test]
    fn writes_new_then_mined_and_stops_at_a_failed_write() {
        let a = &lq3r_locks()["A"];
        let view = CaptureView::new(lq3r(), a.tip);
        let flag = fixed(BlsCtx::LEGACY);
        let hash = isdlock_hash(&a.lock);

        let store = InMemoryIsStore::default();
        assert_eq!(ingest(&view, &store, &flag, &a.lock, Some(300)).ok(), Some(hash));
        assert_eq!(
            store.calls(),
            [IsStoreCall::WriteNew(hash), IsStoreCall::WriteMined(hash, 300)]
        );

        let store = InMemoryIsStore::default();
        assert_eq!(ingest(&view, &store, &flag, &a.lock, None).ok(), Some(hash));
        assert_eq!(store.calls(), [IsStoreCall::WriteNew(hash)]);

        let store = InMemoryIsStore::default();
        let fault = dash_kv::StoreError::engine("test", dash_kv::ErrorKind::Io, "injected");
        store.scripted.lock().unwrap().write_new.push_back(Err(fault));
        let got = ingest(&view, &store, &flag, &a.lock, Some(300));
        assert!(matches!(got, Err(IngestError::Store(_))), "{got:?}");
        assert_eq!(store.calls(), [IsStoreCall::WriteNew(hash)]);

        // A rejected lock writes nothing.
        let store = InMemoryIsStore::default();
        let bad = with_cycle(&a.lock, [0x77; 32]);
        assert!(ingest(&view, &store, &flag, &bad, Some(300)).is_err());
        assert!(store.calls().is_empty());

        // A mined height outside Core's `int` is refused before any write, even for a lock
        // that verifies.
        let store = InMemoryIsStore::default();
        let too_high = u32::try_from(i32::MAX).unwrap() + 1;
        let got = ingest(&view, &store, &flag, &a.lock, Some(too_high));
        assert!(matches!(got, Err(IngestError::MinedHeight(h)) if h == too_high), "{got:?}");
        assert!(store.calls().is_empty());
    }
}

/// S3's batch path on the rotated capture: A and B given cycle 288 at the tip 311 fail the
/// current set's pass (cycle 288's quorum, scans at 311) and verify on the previous set's
/// (offset `dkgInterval`, scans at 287), so both are processed on pass two and nobody is scored.
// Core: instantsend/net_instantsend.cpp:136-163 (sign height, SelectQuorumForSigning with
// signOffset), :190-207, :340-354; llmq/quorumsman.cpp:741-751
#[test]
fn the_batch_path_verifies_rotated_locks_on_the_previous_set() {
    use std::sync::Mutex;

    use crate::is::batch::{IsPassView, PassBody};
    use crate::is::manager::PendingIsLockEntry;
    use crate::is::net::IsNetError;
    use crate::is::s3_fixtures::{Event, Rig, TestRng};

    struct CapturePasses {
        tip: u32,
        scans: Mutex<Vec<Vec<u32>>>,
    }

    impl IsPassView for CapturePasses {
        type Error = String;

        fn with_pass<T: Send + 'static>(
            &self,
            f: impl PassBody<String, T>,
        ) -> Result<T, IsNetError<String>> {
            let view = CaptureView::new(lq3r(), self.tip);
            let r = f(&view, BlsCtx::LEGACY);
            self.scans.lock().unwrap().push(view.scans.borrow().clone());
            r
        }
    }

    let signed = lq3r_locks();
    let pend: Vec<_> = ["A", "B"]
        .iter()
        .map(|name| {
            let lock = with_cycle(&signed[*name].lock, lq3r_block(288));
            PendingIsLockEntry {
                node_id: 1,
                islock_hash: isdlock_hash(&lock),
                islock: Arc::new(lock),
                legacy: true,
            }
        })
        .collect();
    let rig = Rig::with_params(params(lq3r()), false);
    let passes = CapturePasses {
        tip: 311,
        scans: Mutex::default(),
    };
    rig.net.process_pending_is_locks(&passes, pend.clone(), &mut TestRng(5)).expect("two passes");
    assert_eq!(*passes.scans.lock().unwrap(), [vec![311, 311], vec![287, 287]]);
    assert_eq!(
        rig.events(),
        [
            Event::Process(1, pend[0].islock_hash),
            Event::PushRecSig(*pend[0].islock.txid().as_bytes()),
            Event::Process(1, pend[1].islock_hash),
            Event::PushRecSig(*pend[1].islock.txid().as_bytes()),
        ]
    );
}
