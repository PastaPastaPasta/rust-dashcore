//! Type 9's L0 (EV1 §6.1, EV1-5b-2): `evo_assetlocks_tests.cpp`'s unlock half and a vector per
//! step of `CheckAssetUnlockTx`.

use dash_consensus::BlockId;
use dash_consensus::hash::sha256d;
use dash_consensus::primitives::Tx;
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_evo::effect::EffectSink;
use dash_evo::locks::sign_hash;
use dash_pkc::bls::{BlsGlobalSig, BlsSchemeKind};
use dash_primitives::{AssetUnlock, LlmqType, QuorumHash, TxType};

use super::super::mnhf::tests::QuorumWorld;
use super::super::quorum::{
    MAX_QUORUM_SCAN, QuorumKey, QuorumLookup, QuorumScan, SeamError, TestQuorums,
};
use super::super::reasons::*;
use super::super::testctx::{special, txin, txout};
use super::super::trivial::tests::p2pkh;
use super::super::{SpecialTxFault, SpecialTxRules};
use super::{HEIGHT_DIFF_EXPIRING, MAXIMUM_WITHDRAWALS, check_asset_unlock_tx, unlock_too_late};
use crate::creditpool::RangesSet;
use crate::testkit::operator_sig;

const C: TxValidationResult = TxValidationResult::Consensus;
const S: TxValidationResult = TxValidationResult::BadSpecial;

fn platform(w: &QuorumWorld) -> LlmqType {
    w.fx.params().consensus.llmq_type_platform
}

/// The platform type's `signingActiveQuorumCount + 1`.
fn to_scan(w: &QuorumWorld) -> usize {
    let t = platform(w);
    let llmq = w.fx.params().consensus.llmqs.iter().find(|p| p.llmq_type == t).expect("in table");
    usize::try_from(llmq.signing_active_quorum_count).expect("positive") + 1
}

fn unlock_tx(p: &AssetUnlock, outputs: usize) -> Tx {
    let outs = (0..outputs).map(|i| txout(1_000 + i as i64, p2pkh([i as u8; 20]))).collect();
    special(TxType::AssetUnlock, vec![], outs, p)
}

fn global(bytes: &[u8; 96]) -> BlsGlobalSig {
    BlsGlobalSig::decode(bytes).expect("readable")
}

/// `p` signed by `key` over Core's `SignHash`, for a transaction of `outputs` outputs.
fn sign(w: &QuorumWorld, mut p: AssetUnlock, outputs: usize, key: u8) -> (AssetUnlock, Tx) {
    let msg = sha256d(&p.sig_hash_bytes(unlock_tx(&p, outputs).raw()));
    let hash = sign_hash(platform(w), &p.quorum_hash, p.request_id().as_bytes(), &msg);
    p.quorum_sig = global(&operator_sig(key, &hash).to_bytes(BlsSchemeKind::Basic));
    let tx = unlock_tx(&p, outputs);
    (p, tx)
}

/// The world with the platform quorum at `base` held (newest) under key 1, `pindexPrev` at 200,
/// and a version-2 unlock of index 7 requested at 190, signed.
fn ready() -> (QuorumWorld, AssetUnlock, Tx) {
    let mut w = QuorumWorld::new();
    let t = platform(&w);
    let q = w.hold(t, w.base, 1);
    let p = AssetUnlock {
        version: 2,
        index: 7,
        fee: 1_000,
        requested_height: 190,
        quorum_hash: q,
        quorum_sig: global(&[0; 96]),
    };
    let (p, tx) = sign(&w, p, 2, 1);
    (w, p, tx)
}

#[track_caller]
fn assert_reject(w: &mut QuorumWorld, tx: &Tx, class: TxValidationResult, reason: &str) {
    assert_eq!(w.verdict(tx), Err((class, reason.to_owned())));
}

/// A withdrawal signed by an active quorum, inside its height window: accepted, after one scan of
/// `signingActiveQuorumCount + 1` platform quorums at `pindexPrev` and one `GetQuorum`.
#[test]
fn unlock_accepts_a_signed_withdrawal() {
    let (mut w, _, tx) = ready();
    assert_eq!(w.verdict(&tx), Ok(()));
    let t = platform(&w);
    assert_eq!(w.quorums.calls, vec![("scan", t, to_scan(&w)), ("get", t, 0)]);
    assert!(to_scan(&w) <= MAX_QUORUM_SCAN);
}

/// Steps 2 to 6 in Core's order (`evo_assetlocks_tests.cpp` `evo_assetunlock`: an input, the
/// version range, version 2 without V24), each before any quorum read.
#[test]
fn unlock_rejects_the_shape_in_order() {
    let (mut w, p, tx) = ready();
    let mut with_input = tx.raw().clone();
    with_input.inputs.push(txin([3; 32], 0));
    assert_reject(&mut w, &Tx::new(with_input), S, BAD_ASSETUNLOCKTX_HAVE_INPUT);
    let (_, most) = sign(&w, p.clone(), MAXIMUM_WITHDRAWALS, 1);
    assert_eq!(w.verdict(&most), Ok(()));
    w.quorums.calls.clear();
    assert_reject(
        &mut w,
        &unlock_tx(&p, MAXIMUM_WITHDRAWALS + 1),
        S,
        BAD_ASSETUNLOCKTX_TOO_MANY_OUTS,
    );
    let mut bytes = tx.raw().extra_payload.clone();
    bytes.push(0);
    let mut trailing = tx.raw().clone();
    trailing.extra_payload = bytes;
    assert_reject(&mut w, &Tx::new(trailing), S, BAD_ASSETUNLOCKTX_PAYLOAD);
    for version in [0, 3, 255] {
        let q = AssetUnlock {
            version,
            ..p.clone()
        };
        assert_reject(&mut w, &unlock_tx(&q, 2), S, BAD_ASSETUNLOCKTX_VERSION);
    }
    w.fx.rules = SpecialTxRules {
        v24: false,
        evo_shares: false,
    };
    assert_reject(&mut w, &tx, S, BAD_ASSETUNLOCKTX_VERSION_2);
    let (_, v1) = sign(
        &w,
        AssetUnlock {
            version: 1,
            ..p
        },
        2,
        1,
    );
    assert_eq!(w.verdict(&v1), Ok(()));
    assert_eq!(w.quorums.calls.len(), 2, "only the version-1 unlock reached the quorums");
}

/// Step 7: an index the credit pool holds is refused on the block path; the mempool path
/// (no indexes) does not look.
#[test]
fn unlock_rejects_a_duplicated_index_on_the_block_path() {
    let (mut w, p, tx) = ready();
    let mut pool = RangesSet::default();
    assert!(pool.add(p.index));
    w.unlock = Some(pool.clone());
    assert_reject(&mut w, &tx, C, BAD_ASSETUNLOCK_DUPLICATED_INDEX);
    assert!(w.quorums.calls.is_empty());
    let mut other = RangesSet::default();
    assert!(other.add(p.index + 1));
    w.unlock = Some(other);
    assert_eq!(w.verdict(&tx), Ok(()));
    w.unlock = None;
    assert_eq!(w.verdict(&tx), Ok(()));
}

/// Step 8 (`evo_assetunlock`'s first check): an unknown quorum hash, before the scan. A hash on a
/// fork is known, so it passes on to the scan.
#[test]
fn unlock_rejects_an_unknown_quorum_hash() {
    let (mut w, p, _) = ready();
    let (_, unknown) = sign(
        &w,
        AssetUnlock {
            quorum_hash: QuorumHash::from([7; 32]),
            ..p.clone()
        },
        2,
        1,
    );
    assert_reject(&mut w, &unknown, C, BAD_ASSETUNLOCK_QUORUM_HASH);
    assert!(w.quorums.calls.is_empty());
    let fork = w.hash_of(w.fork);
    let (_, on_fork) = sign(
        &w,
        AssetUnlock {
            quorum_hash: fork,
            ..p
        },
        2,
        1,
    );
    assert_reject(&mut w, &on_fork, S, BAD_ASSETUNLOCK_TOO_OLD_QUORUM);
}

/// Step 9: the quorum must be among the newest `signingActiveQuorumCount + 1` of the platform
/// type: the last of them passes, one more newer quorum pushes it out.
#[test]
fn unlock_rejects_a_quorum_outside_the_scan() {
    let (mut w, p, tx) = ready();
    let t = platform(&w);
    let n = to_scan(&w);
    let ours = w.quorums.quorums.remove(0);
    let newer = |w: &mut QuorumWorld| {
        let block = w.chain.child(w.base);
        let key = QuorumKey {
            quorum_hash: w.hash_of(block),
            public_key: ours.1.public_key.clone(),
        };
        (t, key)
    };
    // Newest first: n - 1 newer quorums, then ours, the last the scan returns.
    let mut list: Vec<_> = (0..n - 1).map(|_| newer(&mut w)).collect();
    list.push(ours.clone());
    w.quorums.quorums = list;
    assert_eq!(w.verdict(&tx), Ok(()));
    let newest = newer(&mut w);
    w.quorums.quorums.insert(0, newest);
    assert_reject(&mut w, &tx, S, BAD_ASSETUNLOCK_TOO_OLD_QUORUM);
    // Quorums of another type do not count.
    let mut other = QuorumWorld::new();
    let mnhf = other.mnhf_type();
    other.hold(mnhf, other.base, 1);
    let _ = &p;
    assert_reject(&mut other, &tx, S, BAD_ASSETUNLOCK_TOO_OLD_QUORUM);
}

/// Step 10: `pindexPrev`'s height must be at least the requested height and below it plus
/// [`HEIGHT_DIFF_EXPIRING`]; the sum wraps as Core's `uint32_t`, then reads as `int`.
#[test]
fn unlock_rejects_outside_the_height_window() {
    let (mut w, p, _) = ready();
    let prev = 200u32;
    for (requested, ok) in [
        (prev, true),
        (prev + 1, false),
        (prev - HEIGHT_DIFF_EXPIRING + 1, true),
        (prev - HEIGHT_DIFF_EXPIRING, false),
        (0, false),
        (u32::MAX - 10, false),
    ] {
        let (_, tx) = sign(
            &w,
            AssetUnlock {
                requested_height: requested,
                ..p.clone()
            },
            2,
            1,
        );
        let want = if ok {
            Ok(())
        } else {
            Err((S, BAD_ASSETUNLOCK_TOO_LATE.to_owned()))
        };
        assert_eq!(w.verdict(&tx), want, "requested {requested}");
    }
}

/// A manager whose scans return what [`TestQuorums`] holds and whose `GetQuorum` finds nothing.
struct ScanOnly(TestQuorums);

impl QuorumLookup for ScanOnly {
    fn get_quorum(
        &mut self,
        _: LlmqType,
        _: &QuorumHash,
        _: &mut dyn EffectSink,
    ) -> Result<Option<QuorumKey>, SeamError> {
        Ok(None)
    }

    fn scan_quorums(
        &mut self,
        t: LlmqType,
        at: BlockId,
        n: usize,
        out: &mut QuorumScan,
        fx: &mut dyn EffectSink,
    ) -> Result<(), SeamError> {
        self.0.scan_quorums(t, at, n, out, fx)
    }
}

/// Step 11: a quorum the scan returns but `GetQuorum` does not have.
#[test]
fn unlock_rejects_a_quorum_the_manager_lost() {
    let got = lost_quorum().map_err(|e| (e.result(), e.reject_reason().to_owned()));
    assert_eq!(got, Err((C, BAD_ASSETUNLOCK_QUORUM_INTERNAL_ERROR.to_owned())));
}

/// Step 12: another key, another transaction (the outputs), another requested height (in
/// `msgHash`, though the version-2 txid leaves it out) or index (the request id) do not verify,
/// with `check_sigs` on or off (§6.1).
#[test]
fn unlock_verifies_the_signature_always() {
    for check_sigs in [true, false] {
        let (mut w, p, _) = ready();
        w.fx.check_sigs = check_sigs;
        let (_, other_key) = sign(&w, p.clone(), 2, 2);
        assert_reject(&mut w, &other_key, C, BAD_ASSETUNLOCK_NOT_VERIFIED);
        assert_reject(&mut w, &unlock_tx(&p, 3), C, BAD_ASSETUNLOCK_NOT_VERIFIED);
        let moved = AssetUnlock {
            requested_height: p.requested_height + 1,
            ..p.clone()
        };
        assert_reject(&mut w, &unlock_tx(&moved, 2), C, BAD_ASSETUNLOCK_NOT_VERIFIED);
        let reindexed = AssetUnlock {
            index: p.index + 1,
            ..p.clone()
        };
        assert_reject(&mut w, &unlock_tx(&reindexed, 2), C, BAD_ASSETUNLOCK_NOT_VERIFIED);
        assert_reject(
            &mut w,
            &unlock_tx(
                &AssetUnlock {
                    quorum_sig: global(&[0; 96]),
                    ..p
                },
                2,
            ),
            C,
            BAD_ASSETUNLOCK_NOT_VERIFIED,
        );
    }
}

/// The type step (`bad-assetunlocktx-type`) is unreachable through the dispatcher; a direct
/// call shows it. A missing `pindexPrev` faults before any step (Core's `gsl::not_null`).
#[test]
fn unlock_type_step_direct_and_no_prev() {
    let (mut w, p, tx) = ready();
    let wrong = special(TxType::AssetLock, vec![], vec![], &p);
    let mut quorums = TestQuorums::default();
    let got = w.fx.with_seams(w.seams(), &mut quorums, |cx| {
        check_asset_unlock_tx(&wrong, wrong.payload(), cx)
    });
    assert_eq!(
        got.map(|v| v.map_err(|e| e.reject_reason().to_owned())),
        Ok(Err(BAD_ASSETUNLOCKTX_TYPE.to_owned()))
    );
    w.prev = None;
    let got = w.fx.with_seams(w.seams(), &mut quorums, |cx| {
        check_asset_unlock_tx(&wrong, wrong.payload(), cx)
    });
    assert!(matches!(got, Err(SpecialTxFault::NoPrev(TxType::AssetUnlock))), "{got:?}");
    let got =
        w.fx.with_seams(w.seams(), &mut quorums, |cx| check_asset_unlock_tx(&tx, tx.payload(), cx));
    assert!(matches!(got, Err(SpecialTxFault::NoPrev(TxType::AssetUnlock))), "{got:?}");
}

/// Unbound seams fault where the quorum hash is read; the shape and index steps still answer.
#[test]
fn unlock_faults_without_seams() {
    let (mut w, p, tx) = ready();
    w.bound = false;
    assert!(matches!(w.check(&tx), Err(SpecialTxFault::Unbound(TxType::AssetUnlock))));
    assert_reject(
        &mut w,
        &unlock_tx(
            &AssetUnlock {
                version: 0,
                ..p
            },
            2,
        ),
        S,
        BAD_ASSETUNLOCKTX_VERSION,
    );
}

/// Step 10's arithmetic where a chain cannot reach: past `i32::MAX - 48` the expiry, an `int`,
/// is negative, so a withdrawal requested there is too late at once; and a requested height the
/// `uint32_t` sum wraps is too late at every height.
#[test]
fn unlock_window_wraps_as_core_ints() {
    let high = i32::MAX - 10;
    assert!(unlock_too_late(high, (high - 5) as u32), "the expiry wraps negative");
    let low = i32::MAX - 60;
    assert!(!unlock_too_late(low, (low - 5) as u32), "the expiry stays positive");
    for prev in [0, 200, i32::MAX] {
        assert!(unlock_too_late(prev, u32::MAX - 10));
    }
    assert!(!unlock_too_late(200, 200));
    assert!(unlock_too_late(200, 201));
    assert!(unlock_too_late(200, 200 - HEIGHT_DIFF_EXPIRING));
    assert!(!unlock_too_late(200, 200 - HEIGHT_DIFF_EXPIRING + 1));
}

// --- Census vectors (`specialtx::tests::CASES`): one per reject site of `CheckAssetUnlockTx` ---

type Verdict = Result<(), TxValidationError>;

/// `ready`'s world after `edit`, checking what `tx` makes of the ready unlock.
fn vector(edit: impl FnOnce(&mut QuorumWorld, AssetUnlock, Tx) -> Tx) -> Verdict {
    let (mut w, p, tx) = ready();
    let tx = edit(&mut w, p, tx);
    w.check(&tx).expect("no fault")
}

pub(crate) fn have_input() -> Verdict {
    vector(|_, _, tx| {
        let mut raw = tx.raw().clone();
        raw.inputs.push(txin([3; 32], 0));
        Tx::new(raw)
    })
}

pub(crate) fn too_many_outs() -> Verdict {
    vector(|_, p, _| unlock_tx(&p, MAXIMUM_WITHDRAWALS + 1))
}

pub(crate) fn payload() -> Verdict {
    vector(|_, _, tx| {
        let mut raw = tx.raw().clone();
        raw.extra_payload.push(0);
        Tx::new(raw)
    })
}

pub(crate) fn version() -> Verdict {
    vector(|_, p, _| {
        unlock_tx(
            &AssetUnlock {
                version: 3,
                ..p
            },
            2,
        )
    })
}

pub(crate) fn version_2() -> Verdict {
    vector(|w, _, tx| {
        w.fx.rules.v24 = false;
        tx
    })
}

pub(crate) fn duplicated_index() -> Verdict {
    vector(|w, p, tx| {
        let mut pool = RangesSet::default();
        assert!(pool.add(p.index));
        w.unlock = Some(pool);
        tx
    })
}

pub(crate) fn quorum_hash() -> Verdict {
    vector(|w, p, _| {
        let unknown = AssetUnlock {
            quorum_hash: QuorumHash::from([7; 32]),
            ..p
        };
        sign(w, unknown, 2, 1).1
    })
}

pub(crate) fn too_old_quorum() -> Verdict {
    vector(|w, p, _| {
        let on_fork = AssetUnlock {
            quorum_hash: w.hash_of(w.fork),
            ..p
        };
        sign(w, on_fork, 2, 1).1
    })
}

pub(crate) fn too_late() -> Verdict {
    vector(|w, p, _| {
        let early = AssetUnlock {
            requested_height: 200 - HEIGHT_DIFF_EXPIRING,
            ..p
        };
        sign(w, early, 2, 1).1
    })
}

pub(crate) fn lost_quorum() -> Verdict {
    let (w, _, tx) = ready();
    let mut lookup = ScanOnly(w.quorums.clone());
    w.fx.check_at(&tx, w.seams(), &mut lookup).expect("no fault")
}

pub(crate) fn not_verified() -> Verdict {
    vector(|w, p, _| sign(w, p, 2, 2).1)
}
