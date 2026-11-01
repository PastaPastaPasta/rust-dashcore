//! SB8's lock cases on synthetic scans: Core's request ids and sign hash (Core's own InstantSend
//! vector), the §5.2 step 6 selection gate (DEC-105 F4), Sol r3's case (DEC-107 M4), and the
//! ChainLock and InstantSend checks' every outcome.

use std::cell::RefCell;
use std::collections::BTreeMap;

use dash_consensus::BlockId;
use dash_consensus::hash::sha256d;
use dash_p2p_core::{ClSig, IsdLock};
use dash_params::LlmqType;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_pkc::bls::core::{BlsPkValue, BlsSigValue};
use dash_pkc::bls::{BlsScChia, BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSigBytes, BlsSigId};
use dash_primitives::{BlockHash, OutPoint, QuorumHash, TxHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;

use super::*;

pub(super) fn regtest() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest")
}

fn mainnet() -> ChainSetup {
    ChainParamsBuilder::new(Network::Main, Release::V24_0).build().expect("mainnet")
}

pub(super) fn secret(seed: u8) -> BlsSecretKey<BlsScIetf> {
    BlsSecretKey::from_ikm(&[seed; 32]).expect("a key")
}

pub(super) fn public(seed: u8) -> BlsPkValue {
    BlsPkValue::from_canonical(&secret(seed).public_key().to_bytes(), BlsSchemeKind::Basic)
        .expect("canonical")
}

/// The signature of the quorum key `seed` over `hash`, as wire bytes.
pub(super) fn sign_bytes(seed: u8, hash: &[u8; 32]) -> [u8; 96] {
    secret(seed).sign_with(hash, BlsSigId::Basic).to_bytes()
}

/// The legacy-scheme signature of the quorum key `seed` over `hash` (the scheme before V19), as
/// its legacy wire bytes.
pub(super) fn legacy_sign_bytes(seed: u8, hash: &[u8; 32]) -> [u8; 96] {
    secret(seed).to_scheme::<BlsScChia>().expect("the same scalar").sign_digest(hash).to_bytes()
}

pub(super) fn sig(seed: u8, hash: &[u8; 32]) -> BlsSigValue {
    BlsSigValue::from_canonical(&sign_bytes(seed, hash), BlsSchemeKind::Basic).expect("canonical")
}

pub(super) fn qhash(n: u8) -> QuorumHash {
    QuorumHash::from([n; 32])
}

/// A quorum of hash `[n; 32]` and key `n`, verified.
pub(super) fn verified(llmq_type: LlmqType, n: u8, index: i16) -> ScannedQuorum {
    ScannedQuorum::verified(
        BlockId(u32::from(n)),
        VerifiedQuorum::for_tests(llmq_type, qhash(n), index, public(n)),
    )
}

pub(super) fn with_status(n: u8, index: i16, status: QuorumStatus) -> ScannedQuorum {
    let base = BlockId(u32::from(n));
    match status {
        QuorumStatus::Unverified => ScannedQuorum::unverified(base, qhash(n), index),
        QuorumStatus::Unavailable => ScannedQuorum::unavailable(base, qhash(n), index),
        QuorumStatus::Verified(_) => unreachable!("a verified quorum carries its own identity"),
    }
}

/// A view: the tip, a scan per start height, the tip's recent quorums per type, block heights,
/// quorums by hash.
#[derive(Default)]
pub(super) struct View {
    pub(super) tip: u32,
    pub(super) scans: BTreeMap<u32, ScanLookup>,
    /// The tip's quorums per type, newest first: `ScanQuorums(type, count)` is the first `count`
    /// of a held list. A type absent is not held.
    pub(super) recent: BTreeMap<u8, ScanLookup>,
    /// Each `scan_at_tip` call's type and count.
    pub(super) asked: RefCell<Vec<(u8, usize)>>,
    pub(super) heights: BTreeMap<[u8; 32], u32>,
    pub(super) quorums: BTreeMap<[u8; 32], QuorumStatus>,
}

/// A scan Core would compute, held.
pub(super) fn held(rotation_enabled: bool, entries: Vec<ScannedQuorum>) -> ScanLookup {
    ScanLookup::Held(QuorumScan {
        rotation_enabled,
        entries,
    })
}

impl View {
    pub(super) fn at(tip: u32) -> View {
        View {
            tip,
            ..View::default()
        }
    }

    /// A view at `tip` whose `llmq_type` quorums at the tip are `recent` (newest first), each
    /// also held by hash.
    pub(super) fn with_recent(tip: u32, llmq_type: LlmqType, recent: Vec<ScannedQuorum>) -> View {
        let mut view = View::at(tip);
        for q in &recent {
            view.quorums.insert(*q.quorum_hash().as_bytes(), q.status().clone());
        }
        view.recent.insert(llmq_type.to_base(), held(false, recent));
        view
    }

    fn with_scan(
        tip: u32,
        start: u32,
        rotation_enabled: bool,
        entries: Vec<ScannedQuorum>,
    ) -> View {
        let mut view = View::at(tip);
        view.scans.insert(start, held(rotation_enabled, entries));
        view
    }
}

impl QuorumScanView for View {
    type Error = String;
    fn tip_height(&self) -> u32 {
        self.tip
    }
    fn scan(&self, _: LlmqType, start: u32) -> Result<ScanLookup, String> {
        self.scans.get(&start).cloned().ok_or_else(|| format!("no scan at {start}"))
    }
    fn scan_at_tip(&self, t: LlmqType, count: usize) -> Result<ScanLookup, String> {
        self.asked.borrow_mut().push((t.to_base(), count));
        Ok(match self.recent.get(&t.to_base()) {
            Some(ScanLookup::Held(scan)) => {
                held(scan.rotation_enabled, scan.entries.iter().take(count).cloned().collect())
            }
            Some(other) => other.clone(),
            None => ScanLookup::Unavailable,
        })
    }
    fn block_height(&self, hash: &BlockHash) -> Result<Option<u32>, String> {
        Ok(self.heights.get(hash.as_bytes()).copied())
    }
    fn quorum(&self, _: LlmqType, h: &QuorumHash) -> Result<Option<QuorumStatus>, String> {
        Ok(self.quorums.get(h.as_bytes()).cloned())
    }
}

/// Core's non-rotated choice, written out: the least `SHA256d(type ‖ quorumHash ‖ id)`.
fn core_winner(llmq_type: LlmqType, hashes: &[QuorumHash], id: &[u8; 32]) -> QuorumHash {
    *hashes
        .iter()
        .min_by_key(|h| sha256d(&[&[llmq_type.to_base()][..], h.as_bytes(), id].concat()))
        .expect("a quorum")
}

fn rejected<T: fmt::Debug>(r: Result<T, LockError<String>>) -> LockReject {
    match r {
        Err(LockError::Rejected(reject)) => reject,
        other => panic!("not a rejection: {other:?}"),
    }
}

// ---- Core's vectors ------------------------------------------------------------------------

/// Core's `deserialize_instantlock_from_realdata2` (`src/test/evo_islock_tests.cpp` v24:177-229):
/// a mainnet `isdlock`, its request id (the formula Core's test computes) and its sign hash
/// under the LLMQ_60_75 quorum Core's test names.
#[test]
fn cores_islock_vector() {
    let bytes = hex(concat!(
        "0101497915895c30eebfad0c5fcfb9e0e72308c7e92cd3749be2fd49c8320c4c58b6010000005b9d05c613c2a5",
        "f8ca60800f65f47f46bebbc934571b9ceae813a6ff8e96337bd674ea572a713d6b07deef085b9ce97e1e354055",
        "b91b0bbd0b00000000000000997d0b36738a9eef46ceeb4405998ff7235317708f277402799ffe05258015cae9",
        "b6bae43683f992b2f50f70f8f0cb9c0f26af340b00903e93995c1345d1b2c5b697ebecdbe5811dd112e1188910",
        "1dcb4553b2bc206ab304026b96c07dec4f24"
    ));
    let islock = IsdLock::decode(&mut &bytes[..]).expect("Core's lock");
    assert_eq!(
        islock.txid.as_bytes(),
        &rpc("7b33968effa613e8ea9c1b5734c9bbbe467ff4650f8060caf8a5c213c6059d5b")
    );
    assert_eq!(
        islock.cycle_hash.as_bytes(),
        &rpc("000000000000000bbd0b1bb95540351e7ee99c5b08efde076b3d712a57ea74d6")
    );
    assert_eq!(islock.inputs.len(), 1);
    assert_eq!(
        islock.inputs[0].hash.as_bytes(),
        &rpc("b6584c0c32c849fde29b74d32ce9c70823e7e0b9cf5f0cadbfee305c89157949")
    );
    assert_eq!(islock.inputs[0].index, 1);
    assert!(islock_trivially_valid(&islock));

    // CalculateRequestId: "islock" as a string_view, then the inputs vector.
    let mut pre = vec![6];
    pre.extend_from_slice(b"islock");
    pre.push(1);
    pre.extend_from_slice(islock.inputs[0].hash.as_bytes());
    pre.extend_from_slice(&1u32.to_le_bytes());
    let id = islock_request_id(&islock.inputs);
    assert_eq!(id, sha256d(&pre));

    let quorum =
        QuorumHash::from(rpc("0000000000000019756ecc9c9c5f476d3f66876b1dcfa5dde1ea82f0d99334a2"));
    assert_eq!(
        sign_hash(LlmqType::Llmq60_75, &quorum, &id, islock.txid.as_bytes()),
        rpc("6a3c37bc610c4efd5babd8941068a8eca9e7bec942fe175b8ca9cae31b67e838")
    );
    // The signature reads in the basic scheme ("created with non-legacy", Core's test).
    assert!(matches!(read_lazy_sig(islock.sig.as_bytes(), BlsCtx::BASIC), SigRead::Valid(_)));
}

#[test]
fn the_chainlock_request_id_is_cores_pair() {
    // SerializeHash(pair(string_view "clsig", int32 height)).
    for height in [0, 1, 2_500_155, -1, i32::MAX] {
        let mut pre = vec![5];
        pre.extend_from_slice(b"clsig");
        pre.extend_from_slice(&height.to_le_bytes());
        assert_eq!(chainlock_request_id(height), sha256d(&pre), "{height}");
    }
}

pub(super) fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

/// A hash in RPC (display) order.
pub(super) fn rpc(s: &str) -> [u8; 32] {
    let mut h: [u8; 32] = hex(s).try_into().expect("32 bytes");
    h.reverse();
    h
}

// ---- the selection gate (SB0 §5.2 step 6, DEC-105 F4) ----------------------------------------

/// A recovered signature of LLMQ_TEST (non-rotated) at sign height 100, offset 8: the start
/// block 92.
fn non_rotated(
    view: &View,
    id: &[u8; 32],
    signer: u8,
) -> Result<VerifiedQuorum, LockError<String>> {
    let msg = [0x33; 32];
    let hash = sign_hash(LlmqType::LlmqTest, &qhash(signer), id, &msg);
    verify_recovered_sig(
        &regtest(),
        view,
        BlsCtx::BASIC,
        LlmqType::LlmqTest,
        100,
        8,
        id,
        &msg,
        &sig(signer, &hash),
    )
}

#[test]
fn non_rotated_an_unverified_winner_is_unavailable_never_the_best_verified() {
    let id = [0x5a; 32];
    let hashes: Vec<QuorumHash> = (1..=4).map(qhash).collect();
    let winner = core_winner(LlmqType::LlmqTest, &hashes, &id).as_bytes()[0];
    let runner_up = {
        let rest: Vec<QuorumHash> =
            hashes.iter().copied().filter(|h| h.as_bytes()[0] != winner).collect();
        core_winner(LlmqType::LlmqTest, &rest, &id).as_bytes()[0]
    };
    for status in [QuorumStatus::Unverified, QuorumStatus::Unavailable] {
        let entries = (1..=4)
            .map(|n| {
                if n == winner {
                    with_status(n, 0, status.clone())
                } else {
                    verified(LlmqType::LlmqTest, n, 0)
                }
            })
            .collect();
        let view = View::with_scan(120, 92, false, entries);
        // Signed by the best-scoring verified quorum: still unavailable.
        assert_eq!(rejected(non_rotated(&view, &id, runner_up)), LockReject::QuorumUnavailable);
        // Signed by the winner itself: unavailable too, its status unread past selection.
        assert_eq!(rejected(non_rotated(&view, &id, winner)), LockReject::QuorumUnavailable);
    }
}

#[test]
fn non_rotated_an_unverified_non_winner_leaves_the_winner_verifying() {
    let id = [0x5a; 32];
    let hashes: Vec<QuorumHash> = (1..=4).map(qhash).collect();
    let winner = core_winner(LlmqType::LlmqTest, &hashes, &id).as_bytes()[0];
    let loser = (1..=4).find(|&n| n != winner).expect("another");
    let entries = (1..=4)
        .map(|n| {
            if n == loser {
                with_status(n, 0, QuorumStatus::Unverified)
            } else {
                verified(LlmqType::LlmqTest, n, 0)
            }
        })
        .collect();
    let view = View::with_scan(120, 92, false, entries);
    let quorum = non_rotated(&view, &id, winner).expect("the winner verifies");
    assert_eq!(quorum.quorum_hash(), &qhash(winner));
    // The lock verifies only against the selected key: another verified quorum's signature (over
    // its own sign hash) is Core's invalid signature.
    let other = (1..=4).find(|&n| n != winner && n != loser).expect("a third");
    assert_eq!(rejected(non_rotated(&view, &id, other)), LockReject::InvalidSignature);
}

/// A selection hash whose rotated signer (bit 62 of the last word, two signing quorums) is
/// `signer`.
fn rotated_id(signer: u64) -> [u8; 32] {
    let mut id = [0x77; 32];
    let word = (u64::from_le_bytes(id[24..].try_into().expect("8")) & !(1 << 62)) | signer << 62;
    id[24..].copy_from_slice(&word.to_le_bytes());
    id
}

fn rotated(view: &View, id: &[u8; 32], signer: u8) -> Result<VerifiedQuorum, LockError<String>> {
    let t = LlmqType::LlmqTestDip0024;
    let msg = [0x44; 32];
    let hash = sign_hash(t, &qhash(signer), id, &msg);
    verify_recovered_sig(&regtest(), view, BlsCtx::BASIC, t, -1, 8, id, &msg, &sig(signer, &hash))
}

#[test]
fn rotated_the_newest_at_the_index_unverified_is_unavailable_never_an_older_one() {
    let t = LlmqType::LlmqTestDip0024;
    // Newest first: index 0 (unverified), index 1, then the previous cycle's index 0 and 1.
    let entries = vec![
        with_status(10, 0, QuorumStatus::Unverified),
        verified(t, 11, 1),
        verified(t, 8, 0),
        verified(t, 9, 1),
    ];
    let view = View::with_scan(100, 92, true, entries);
    // Signer 0: the newest entry with index 0 is selected; the older verified one never is.
    assert_eq!(rejected(rotated(&view, &rotated_id(0), 8)), LockReject::QuorumUnavailable);
    // Signer 1: the newest with index 1 verifies.
    assert_eq!(rotated(&view, &rotated_id(1), 11).expect("verified").quorum_hash(), &qhash(11));
    // And only with its own key.
    assert_eq!(rejected(rotated(&view, &rotated_id(1), 9)), LockReject::InvalidSignature);
}

#[test]
fn no_quorum_is_cores_no_quorum() {
    let id = [1; 32];
    // An empty scan.
    let view = View::with_scan(120, 92, false, vec![]);
    assert_eq!(rejected(non_rotated(&view, &id, 1)), LockReject::NoQuorum);
    // A start above the tip, and below zero: no start block, the view unread.
    let view = View::at(91);
    assert_eq!(rejected(non_rotated(&view, &id, 1)), LockReject::NoQuorum);
    let msg = [0; 32];
    let s = sig(1, &[0; 32]);
    let below = verify_recovered_sig(
        &regtest(),
        &view,
        BlsCtx::BASIC,
        LlmqType::LlmqTest,
        7,
        8,
        &id,
        &msg,
        &s,
    );
    assert_eq!(rejected(below), LockReject::NoQuorum);
    // Rotated: no entry with the signer's index.
    let view = View::with_scan(100, 92, true, vec![verified(LlmqType::LlmqTestDip0024, 11, 1)]);
    assert_eq!(rejected(rotated(&view, &rotated_id(0), 11)), LockReject::NoQuorum);
}

#[test]
fn a_set_the_view_lacks_is_unavailable_or_outside_the_horizon() {
    let id = [1; 32];
    for (lookup, want) in [
        (ScanLookup::Unavailable, LockReject::QuorumUnavailable),
        (ScanLookup::OutsideHorizon, LockReject::OutsideHorizon),
    ] {
        let mut view = View::at(120);
        view.scans.insert(92, lookup);
        assert_eq!(rejected(non_rotated(&view, &id, 1)), want);
    }
    // A view error is passed through.
    let view = View::at(120);
    assert!(matches!(non_rotated(&view, &id, 1), Err(LockError::View(_))));
    // An unconfigured type.
    let s = sig(1, &[0; 32]);
    let unknown = verify_recovered_sig(
        &regtest(),
        &view,
        BlsCtx::BASIC,
        LlmqType::Llmq400_60,
        -1,
        8,
        &id,
        &[0; 32],
        &s,
    );
    assert_eq!(unknown, Err(LockError::UnknownLlmqType(2)));
}

// ---- Sol r3's case (SB0 §5.2.1, DEC-107 M4) ----------------------------------------------------

/// Mainnet: a CL-type (`llmq_400_60`) commitment for cycle base 2,500,128 mined at 2,500,148
/// replaces the oldest of the four active quorums. A ChainLock for 2,500,155 selects from the set
/// at 2,500,147, before the mine, where the replaced quorum is the constructed scoring winner; a
/// ChainLock for 2,500,156 selects from the set at 2,500,148. Each selects as Core does on its
/// set, and each verifies with the quorum it selects.
#[test]
fn sol_r3_each_lock_selects_on_its_own_set() {
    let params = mainnet();
    let t = params.consensus.llmq_type_chain_locks;
    assert_eq!(t, LlmqType::Llmq400_60);
    let id155 = chainlock_request_id(2_500_155);
    let id156 = chainlock_request_id(2_500_156);
    let active = [qhash(21), qhash(22), qhash(23)];
    // The replaced quorum: a hash that wins 155's selection over the set before the mine.
    let old = (100u8..=255)
        .map(qhash)
        .find(|h| {
            let set = [active[0], active[1], active[2], *h];
            core_winner(t, &set, &id155) == *h
        })
        .expect("a constructed winner");
    // The mined quorum: one that 156's selection after the mine does not pick as `old` (gone).
    let new = qhash(30);
    let before = [active[0], active[1], active[2], old];
    let after = [new, active[0], active[1], active[2]];
    let scan = |set: &[QuorumHash]| {
        let entries = set
            .iter()
            .map(|h| {
                let n = h.as_bytes()[0];
                ScannedQuorum::verified(
                    BlockId(u32::from(n)),
                    VerifiedQuorum::for_tests(t, *h, 0, public(n)),
                )
            })
            .collect();
        held(false, entries)
    };
    let mut view = View::at(2_500_156);
    view.scans.insert(2_500_147, scan(&before));
    view.scans.insert(2_500_148, scan(&after));

    let chainlock = |view: &View, height: i32, signer: QuorumHash| {
        let block = BlockHash::from([height as u8; 32]);
        let id = chainlock_request_id(height);
        let hash = sign_hash(t, &signer, &id, block.as_bytes());
        let clsig = ClSig {
            height,
            block_hash: block,
            sig: BlsSigBytes::from(sign_bytes(signer.as_bytes()[0], &hash)),
        };
        verify_chainlock(&params, view, LockSchemes::both(BlsCtx::BASIC), &clsig)
    };
    // 155: Core's winner over the set at 2,500,147 is the replaced quorum, and it verifies.
    assert_eq!(core_winner(t, &before, &id155), old);
    let lock = chainlock(&view, 2_500_155, old).expect("verifies from the earlier set");
    assert_eq!(lock.quorum.quorum_hash(), &old);
    // 156: Core's winner over the set at 2,500,148, which no longer holds `old`.
    let winner156 = core_winner(t, &after, &id156);
    let lock = chainlock(&view, 2_500_156, winner156).expect("verifies from the later set");
    assert_eq!(lock.quorum.quorum_hash(), &winner156);
    // Selected on the tip's set instead, 155 would need another quorum: its lock never verifies
    // there, which is why the earlier set is kept (SB11 retains and fetches it).
    let winner_after = core_winner(t, &after, &id155);
    assert_ne!(winner_after, old);
    view.scans.insert(2_500_147, scan(&after));
    assert_eq!(rejected(chainlock(&view, 2_500_155, old)), LockReject::InvalidSignature);
}

// ---- ChainLocks -------------------------------------------------------------------------------

#[test]
fn a_chainlock_selects_at_height_minus_eight_and_reads_its_signature_under_the_scheme() {
    let params = regtest();
    let t = params.consensus.llmq_type_chain_locks;
    let height = 100;
    let block = BlockHash::from([0xab; 32]);
    let view = View::with_scan(105, 92, false, vec![verified(t, 1, 0)]);
    let id = chainlock_request_id(height);
    let hash = sign_hash(t, &qhash(1), &id, block.as_bytes());
    let mut clsig = ClSig {
        height,
        block_hash: block,
        sig: BlsSigBytes::from(sign_bytes(1, &hash)),
    };
    let lock = verify_chainlock(&params, &view, LockSchemes::both(BlsCtx::BASIC), &clsig)
        .expect("verifies");
    assert_eq!((lock.height, lock.block_hash), (height, block));
    // Another block hash: Core's invalid.
    clsig.block_hash = BlockHash::from([0xac; 32]);
    assert_eq!(
        rejected(verify_chainlock(&params, &view, LockSchemes::both(BlsCtx::BASIC), &clsig)),
        LockReject::InvalidSignature
    );
    clsig.block_hash = block;
    // Under the legacy flag the basic encoding does not decode: the invalid object, which no key
    // verifies (Core reads it under its flag).
    assert_eq!(
        rejected(verify_chainlock(&params, &view, LockSchemes::both(BlsCtx::LEGACY), &clsig)),
        LockReject::InvalidSignature
    );
    // Bytes canonical in neither scheme: Core's `Unserialize` throws.
    clsig.sig = BlsSigBytes::from([0xff; 96]);
    assert_eq!(
        rejected(verify_chainlock(&params, &view, LockSchemes::both(BlsCtx::BASIC), &clsig)),
        LockReject::MalformedSignature
    );
    // Zeros read as the invalid object.
    clsig.sig = BlsSigBytes::from([0; 96]);
    assert_eq!(
        rejected(verify_chainlock(&params, &view, LockSchemes::both(BlsCtx::BASIC), &clsig)),
        LockReject::InvalidSignature
    );
}

/// LQ3a D21, D23: the V19 block connects between Core's read of a `clsig` (`net_processing.cpp`
/// v24:5797) and the handler's check (`chainlock/handler.cpp` v24:156). The eager signature holds
/// the point it read under the legacy flag, and the basic check decides: it rejects a legacy
/// signature, and Core scores the peer (:161), but accepts a basic one sent in the legacy
/// encoding. That case tells the read stage's scheme apart: read under the basic flag, those
/// bytes are the invalid object. Disconnecting the block is the reverse.
#[test]
fn a_chainlock_checked_across_the_v19_switch_is_invalid() {
    let params = regtest();
    let t = params.consensus.llmq_type_chain_locks;
    let height = 100;
    let block = BlockHash::from([0xab; 32]);
    let view = View::with_scan(105, 92, false, vec![verified(t, 1, 0)]);
    let hash = sign_hash(t, &qhash(1), &chainlock_request_id(height), block.as_bytes());
    let at = |sig: [u8; 96], read, check| {
        let clsig = ClSig {
            height,
            block_hash: block,
            sig: BlsSigBytes::from(sig),
        };
        verify_chainlock(
            &params,
            &view,
            LockSchemes {
                read,
                check,
            },
            &clsig,
        )
        .map(|lock| lock.quorum)
    };
    let legacy = legacy_sign_bytes(1, &hash);
    assert!(at(legacy, BlsCtx::LEGACY, BlsCtx::LEGACY).is_ok());
    assert_eq!(rejected(at(legacy, BlsCtx::LEGACY, BlsCtx::BASIC)), LockReject::InvalidSignature);
    let basic = sign_bytes(1, &hash);
    assert!(at(basic, BlsCtx::BASIC, BlsCtx::BASIC).is_ok());
    assert_eq!(rejected(at(basic, BlsCtx::BASIC, BlsCtx::LEGACY)), LockReject::InvalidSignature);
    // Each signature's point in the other scheme's encoding: read under that scheme, checked
    // under its own, accepted.
    let reencoded = |bytes: &[u8; 96], from, to| {
        BlsSigValue::from_canonical(bytes, from).expect("canonical").to_bytes(to)
    };
    let basic_as_legacy = reencoded(&basic, BlsSchemeKind::Basic, BlsSchemeKind::Legacy);
    assert!(at(basic_as_legacy, BlsCtx::LEGACY, BlsCtx::BASIC).is_ok());
    assert_eq!(
        rejected(at(basic_as_legacy, BlsCtx::BASIC, BlsCtx::BASIC)),
        LockReject::InvalidSignature
    );
    let legacy_as_basic = reencoded(&legacy, BlsSchemeKind::Legacy, BlsSchemeKind::Basic);
    assert!(at(legacy_as_basic, BlsCtx::BASIC, BlsCtx::LEGACY).is_ok());
    assert_eq!(
        rejected(at(legacy_as_basic, BlsCtx::LEGACY, BlsCtx::LEGACY)),
        LockReject::InvalidSignature
    );
}

// ---- InstantSend locks -------------------------------------------------------------------------

const CYCLE: [u8; 32] = [0xc0; 32];

/// An `isdlock` of one input `(n, 0)` for txid `[0x11; 32]`, cycle block `CYCLE`, signed by the
/// quorum key `signer` for the quorum `[signer; 32]`.
fn islock(n: u8, signer: u8, llmq_type: LlmqType) -> IsdLock {
    let inputs = vec![OutPoint {
        hash: TxHash::from([n; 32]),
        index: 0,
    }];
    let txid = TxHash::from([0x11; 32]);
    let hash = sign_hash(llmq_type, &qhash(signer), &islock_request_id(&inputs), txid.as_bytes());
    IsdLock {
        version: 1,
        inputs,
        txid,
        cycle_hash: BlockHash::from(CYCLE),
        sig: BlsSigBytes::from(sign_bytes(signer, &hash)),
    }
}

/// An input whose request id selects the rotated signer `signer`.
fn input_for_signer(signer: u64) -> u8 {
    (0u8..=255)
        .find(|&n| {
            let id = islock_request_id(&[OutPoint {
                hash: TxHash::from([n; 32]),
                index: 0,
            }]);
            (u64::from_le_bytes(id[24..].try_into().expect("8")) >> 62) & 1 == signer
        })
        .expect("an input")
}

#[test]
fn islock_trivial_checks_come_first() {
    let params = regtest();
    let t = params.consensus.llmq_type_dip0024_instant_send;
    let view = View::default();
    let good = islock(1, 1, t);
    let check = |edit: &dyn Fn(&mut IsdLock)| {
        let mut l = good.clone();
        edit(&mut l);
        rejected(verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &l))
    };
    assert_eq!(check(&|l| l.txid = TxHash::from([0; 32])), LockReject::Malformed);
    assert_eq!(check(&|l| l.inputs.clear()), LockReject::Malformed);
    assert_eq!(check(&|l| l.inputs.push(l.inputs[0])), LockReject::Malformed);
    assert_eq!(
        check(&|l| {
            l.inputs = (0..=ISLOCK_MAX_INPUTS as u32)
                .map(|index| OutPoint {
                    hash: TxHash::from([1; 32]),
                    index,
                })
                .collect();
        }),
        LockReject::Malformed
    );
    // Unknown and null cycle hashes.
    assert_eq!(check(&|_| {}), LockReject::UnknownCycle);
    assert_eq!(check(&|l| l.cycle_hash = BlockHash::from([0; 32])), LockReject::UnknownCycle);
    // A cycle block off the boundary (LLMQ_TEST_DIP0024's dkgInterval is 24).
    let mut view = View::default();
    view.heights.insert(CYCLE, 25);
    assert_eq!(
        rejected(verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &good)),
        LockReject::InvalidCycle
    );
    // An unreadable signature, at a boundary.
    view.heights.insert(CYCLE, 48);
    let mut l = good.clone();
    l.sig = BlsSigBytes::from([0xff; 96]);
    assert_eq!(
        rejected(verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &l)),
        LockReject::MalformedSignature
    );
    l.sig = BlsSigBytes::from([0; 96]);
    assert_eq!(
        rejected(verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &l)),
        LockReject::MalformedSignature
    );
}

/// Rotated InstantSend: the current active set first (offset 0), then the previous one (offset
/// `dkgInterval`); either verifying accepts, and a pass whose quorum is not verified decides
/// nothing.
#[test]
fn islock_two_passes() {
    let params = regtest();
    let t = params.consensus.llmq_type_dip0024_instant_send;
    let n = input_for_signer(0);
    // Tip 60, cycle 48: the tip is not past the cycle (48 + 24 >= 60), so the sign height is
    // the tip: start 60 for the first pass, 36 for the second.
    let mut view = View::at(60);
    view.heights.insert(CYCLE, 48);
    let set = |q: ScannedQuorum| held(true, vec![q]);
    // Signed by quorum 5, selected in the current set.
    view.scans.insert(60, set(verified(t, 5, 0)));
    view.scans.insert(36, set(verified(t, 6, 0)));
    let lock = verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &islock(n, 5, t))
        .expect("pass 1");
    assert_eq!(lock.quorum.quorum_hash(), &qhash(5));
    assert_eq!(lock.request_id, islock_request_id(&islock(n, 5, t).inputs));
    // Signed by quorum 6 of the previous set: pass 1 fails, pass 2 verifies.
    let lock = verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &islock(n, 6, t))
        .expect("pass 2");
    assert_eq!(lock.quorum.quorum_hash(), &qhash(6));
    // Neither set's key: Core's rejection.
    assert_eq!(
        rejected(verify_islock(
            &params,
            &view,
            IslockSchemes::both(BlsCtx::BASIC),
            &islock(n, 7, t)
        )),
        LockReject::InvalidSignature
    );
    // The current set's selected quorum unverified: the previous set still verifies its own
    // lock, and a lock of the current set is unavailable, not invalid.
    view.scans.insert(60, set(with_status(5, 0, QuorumStatus::Unverified)));
    assert!(
        verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &islock(n, 6, t)).is_ok()
    );
    assert_eq!(
        rejected(verify_islock(
            &params,
            &view,
            IslockSchemes::both(BlsCtx::BASIC),
            &islock(n, 5, t)
        )),
        LockReject::QuorumUnavailable
    );
    // No quorum in either pass.
    view.scans.insert(60, set(verified(t, 5, 1)));
    view.scans.insert(36, set(verified(t, 6, 1)));
    assert_eq!(
        rejected(verify_islock(
            &params,
            &view,
            IslockSchemes::both(BlsCtx::BASIC),
            &islock(n, 5, t)
        )),
        LockReject::NoQuorum
    );

    // Once the tip is past the cycle (48 + 24 < 100) the lock signs at 48 + 24 - 1 = 71: the
    // passes start at 71 and 47.
    let mut view = View::at(100);
    view.heights.insert(CYCLE, 48);
    view.scans.insert(71, set(verified(t, 5, 0)));
    view.scans.insert(47, set(verified(t, 6, 0)));
    assert!(
        verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &islock(n, 5, t)).is_ok()
    );
    assert!(
        verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &islock(n, 6, t)).is_ok()
    );
}

/// The sign height's edge (`net_instantsend.cpp` v24:146): the lock signs at
/// `cycleHeight + dkgInterval - 1` only once the tip is strictly past `cycleHeight +
/// dkgInterval`. At tip 72 = 48 + 24 the sign height is the tip, so the passes start at 72 and
/// 48, not at 71 and 47 (O2a-LQ3 D24).
#[test]
fn islock_signs_at_the_tip_when_the_tip_is_the_cycle_end() {
    let params = regtest();
    let t = params.consensus.llmq_type_dip0024_instant_send;
    let n = input_for_signer(0);
    let set = |q: ScannedQuorum| held(true, vec![q]);
    let mut view = View::at(72);
    view.heights.insert(CYCLE, 48);
    view.scans.insert(72, set(verified(t, 5, 0)));
    view.scans.insert(48, set(verified(t, 6, 0)));
    view.scans.insert(71, set(verified(t, 7, 0)));
    view.scans.insert(47, set(verified(t, 8, 0)));
    let at = |signer| {
        verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &islock(n, signer, t))
    };
    assert_eq!(at(5).expect("pass 1 at the tip").quorum.quorum_hash(), &qhash(5));
    assert_eq!(at(6).expect("pass 2 at the tip").quorum.quorum_hash(), &qhash(6));
    assert_eq!(rejected(at(7)), LockReject::InvalidSignature);
    // One block later the tip is past the cycle's end: the passes start at 71 and 47.
    view.tip = 73;
    let at = |signer| {
        verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &islock(n, signer, t))
    };
    assert_eq!(at(7).expect("pass 1 at 71").quorum.quorum_hash(), &qhash(7));
    assert_eq!(at(8).expect("pass 2 at 47").quorum.quorum_hash(), &qhash(8));
    assert_eq!(rejected(at(5)), LockReject::InvalidSignature);
}

/// O2a-LQ3 D23 (BL-209): an `isdlock` that neither pass verifies is rejected as its last pass
/// rejects it, because Core's response follows that pass alone (the first pass does not score the
/// peer, `net_instantsend.cpp` v24:340-353). The current set's quorum rejecting the signature
/// and the previous set having no quorum is Core's no-quorum; the reverse is its invalid
/// signature.
#[test]
fn an_islock_rejected_by_both_passes_takes_the_last_pass_status() {
    let params = regtest();
    let t = params.consensus.llmq_type_dip0024_instant_send;
    let n = input_for_signer(0);
    let lock = islock(n, 6, t);
    let at = |current: Vec<ScannedQuorum>, previous: Vec<ScannedQuorum>| {
        let mut view = View::at(60);
        view.heights.insert(CYCLE, 48);
        view.scans.insert(60, held(true, current));
        view.scans.insert(36, held(true, previous));
        rejected(verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &lock))
    };
    assert_eq!(at(vec![verified(t, 5, 0)], vec![]), LockReject::NoQuorum);
    assert_eq!(at(vec![], vec![verified(t, 5, 0)]), LockReject::InvalidSignature);
    assert_eq!(at(vec![verified(t, 5, 0)], vec![verified(t, 7, 0)]), LockReject::InvalidSignature);
    assert_eq!(at(vec![], vec![]), LockReject::NoQuorum);
}

/// Non-rotated InstantSend (a regtest `-llmqtypeinstantsend` configuration): the cycle hash names
/// the quorum, in both passes.
#[test]
fn islock_non_rotated_names_its_quorum() {
    let mut params = regtest();
    params.consensus.llmq_type_dip0024_instant_send = LlmqType::LlmqTestInstantsend;
    let t = LlmqType::LlmqTestInstantsend;
    let interval = u32::from(params.consensus.llmq(t).expect("configured").dkg_interval);
    let mut view = View::at(1000);
    view.heights.insert(CYCLE, 2 * interval);
    let signer = CYCLE[0];
    let lock = islock(1, signer, t);
    // Not mined: Core's null quorum.
    assert_eq!(
        rejected(verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &lock)),
        LockReject::NoQuorum
    );
    view.quorums.insert(CYCLE, QuorumStatus::Unavailable);
    assert_eq!(
        rejected(verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &lock)),
        LockReject::QuorumUnavailable
    );
    view.quorums.insert(
        CYCLE,
        QuorumStatus::Verified(VerifiedQuorum::for_tests(
            t,
            QuorumHash::from(CYCLE),
            0,
            public(signer),
        )),
    );
    let verified =
        verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &lock).expect("verifies");
    assert_eq!(verified.quorum.quorum_hash(), &QuorumHash::from(CYCLE));
    // A view answering with a quorum of another hash, or of another type, gives no verdict:
    // never a check under a `SignHash` the lock was not signed over.
    for (llmq_type, quorum_hash) in
        [(t, QuorumHash::from([0xc1; 32])), (LlmqType::LlmqTest, QuorumHash::from(CYCLE))]
    {
        view.quorums.insert(
            CYCLE,
            QuorumStatus::Verified(VerifiedQuorum::for_tests(
                llmq_type,
                quorum_hash,
                0,
                public(signer),
            )),
        );
        assert_eq!(
            rejected(verify_islock(&params, &view, IslockSchemes::both(BlsCtx::BASIC), &lock)),
            LockReject::QuorumUnavailable,
            "{llmq_type:?} {quorum_hash:?}"
        );
    }
}

/// A scan whose selected entry holds another type's quorum: no verdict.
#[test]
fn a_selected_quorum_of_another_type_is_unavailable() {
    let id = [0x5a; 32];
    let hashes: Vec<QuorumHash> = (1..=4).map(qhash).collect();
    let winner = core_winner(LlmqType::LlmqTest, &hashes, &id).as_bytes()[0];
    let entries = (1..=4)
        .map(|n| {
            verified(
                if n == winner {
                    LlmqType::LlmqTestPlatform
                } else {
                    LlmqType::LlmqTest
                },
                n,
                0,
            )
        })
        .collect();
    let view = View::with_scan(120, 92, false, entries);
    assert_eq!(rejected(non_rotated(&view, &id, winner)), LockReject::QuorumUnavailable);
}

#[test]
fn passes_combine_safely() {
    use LockReject::*;
    let r = |x| LockError::<String>::Rejected(x);
    // Core's rejection is the last pass's (O2a-LQ3 D23).
    assert_eq!(combine(vec![r(InvalidSignature), r(NoQuorum)]), r(NoQuorum));
    assert_eq!(combine(vec![r(NoQuorum), r(InvalidSignature)]), r(InvalidSignature));
    assert_eq!(combine(vec![r(InvalidSignature), r(InvalidSignature)]), r(InvalidSignature));
    assert_eq!(combine(vec![r(NoQuorum), r(NoQuorum)]), r(NoQuorum));
    assert_eq!(combine(vec![r(QuorumUnavailable), r(InvalidSignature)]), r(QuorumUnavailable));
    assert_eq!(combine(vec![r(InvalidSignature), r(OutsideHorizon)]), r(OutsideHorizon));
    assert_eq!(
        combine(vec![LockError::BlsUnsupported, r(QuorumUnavailable)]),
        LockError::BlsUnsupported
    );
    // An outcome no pass gives is passed through, not folded.
    assert_eq!(combine(vec![r(NoQuorum), r(Malformed)]), r(Malformed));
}

/// An `isdlock` of [`islock`]'s inputs and txid, signed by the quorum key `signer` in the legacy
/// scheme.
fn legacy_islock(n: u8, signer: u8, llmq_type: LlmqType) -> IsdLock {
    let mut lock = islock(n, signer, llmq_type);
    let id = islock_request_id(&lock.inputs);
    let hash = sign_hash(llmq_type, &qhash(signer), &id, lock.txid.as_bytes());
    lock.sig = BlsSigBytes::from(legacy_sign_bytes(signer, &hash));
    lock
}

/// `islock_two_passes`'s view: quorum 5 selected in the current set, quorum 6 in the previous.
fn two_set_view(llmq_type: LlmqType) -> View {
    let mut view = View::at(60);
    view.heights.insert(CYCLE, 48);
    view.scans.insert(60, held(true, vec![verified(llmq_type, 5, 0)]));
    view.scans.insert(36, held(true, vec![verified(llmq_type, 6, 0)]));
    view
}

/// LQ3a D21: the V19 block connects between Core's read of an `isdlock` and the InstantSend
/// worker's check. The lazy signature keeps the legacy flag it was read under; both passes check
/// it under the basic scheme and reject it. Disconnecting the block is the reverse.
#[test]
fn an_islock_checked_across_the_v19_switch_is_invalid() {
    let params = regtest();
    let t = params.consensus.llmq_type_dip0024_instant_send;
    let n = input_for_signer(0);
    let view = two_set_view(t);
    let (legacy, basic) = (legacy_islock(n, 5, t), islock(n, 5, t));
    let at = |lock: &IsdLock, read, check| {
        verify_islock(
            &params,
            &view,
            IslockSchemes {
                read,
                passes: IslockPassSchemes::both(check),
            },
            lock,
        )
    };
    assert!(at(&legacy, BlsCtx::LEGACY, BlsCtx::LEGACY).is_ok());
    assert_eq!(rejected(at(&legacy, BlsCtx::LEGACY, BlsCtx::BASIC)), LockReject::InvalidSignature);
    assert!(at(&basic, BlsCtx::BASIC, BlsCtx::BASIC).is_ok());
    assert_eq!(rejected(at(&basic, BlsCtx::BASIC, BlsCtx::LEGACY)), LockReject::InvalidSignature);
}

/// Sol r2, LQ3a D22: the V19 block connects between the worker's two passes, each of which reads
/// the flag afresh (`instantsend/net_instantsend.cpp` v24:312-327, :339-353). Locks read and
/// queued at `V19Height - 2` in one batch: one signed by the current set's quorum verifies in the
/// first pass, under the legacy flag; one signed by the previous set's quorum fails the first
/// pass, and the second, after `V19Height - 1` connects, rejects its legacy signature under the
/// basic scheme. No single scheme gives both. Disconnecting the block is the reverse.
#[test]
fn an_islocks_passes_check_under_the_flag_each_reads() {
    let params = regtest();
    let t = params.consensus.llmq_type_dip0024_instant_send;
    let n = input_for_signer(0);
    let view = two_set_view(t);
    let at = |lock: &IsdLock, read, current, previous| {
        let passes = IslockPassSchemes {
            current,
            previous,
        };
        verify_islock(
            &params,
            &view,
            IslockSchemes {
                read,
                passes,
            },
            lock,
        )
        .map(|l| *l.quorum.quorum_hash())
    };
    let (legacy_current, legacy_previous) = (legacy_islock(n, 5, t), legacy_islock(n, 6, t));
    // No switch: each verifies, the previous set's in the second pass.
    assert_eq!(at(&legacy_previous, BlsCtx::LEGACY, BlsCtx::LEGACY, BlsCtx::LEGACY), Ok(qhash(6)));
    // The switch between the passes.
    let (l, b) = (BlsCtx::LEGACY, BlsCtx::BASIC);
    assert_eq!(at(&legacy_current, l, l, b), Ok(qhash(5)));
    assert_eq!(rejected(at(&legacy_previous, l, l, b)), LockReject::InvalidSignature);
    // The reverse, for basic locks read before the block disconnects.
    let (basic_current, basic_previous) = (islock(n, 5, t), islock(n, 6, t));
    assert_eq!(at(&basic_previous, b, b, b), Ok(qhash(6)));
    assert_eq!(at(&basic_current, b, b, l), Ok(qhash(5)));
    assert_eq!(rejected(at(&basic_previous, b, b, l)), LockReject::InvalidSignature);
}
