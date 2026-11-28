//! The batch outcome tables (PLAN-LK1b §5 item 4, §6 S3) against Core v24.0.0
//! `instantsend/net_instantsend.cpp:104-224, :313-357` and `bls/bls_batchverifier.h:68-146`.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;

use dash_evo::locks::{Pass, islock_request_id};
use dash_pkc::bls::BlsSigBytes;

use super::*;
use crate::is::s3_fixtures::{
    BASIC, Event, FakeView, Passes, Rig, TestRng, entry, lock, lock_signed_by, quorum_hash,
    unreadable,
};

const SCORE: i32 = OLD_ACTIVE_SET_FAILURE_MISBEHAVIOR_SCORE;

fn hashes(entries: &[PendingIsLockEntry]) -> Vec<IsLockHash> {
    entries.iter().map(|e| e.islock_hash).collect()
}

fn scores(rig: &Rig) -> Vec<(NodeId, i32)> {
    rig.events()
        .into_iter()
        .filter_map(|e| match e {
            Event::Misbehaving(n, s) => Some((n, s)),
            _ => None,
        })
        .collect()
}

/// Pass one's batch, built under the basic flag.
fn build(rig: &Rig, view: &FakeView, pend: &[PendingIsLockEntry]) -> BatchVerificationData {
    let targets = rig.net.batch_targets(view, Pass::Current, pend).expect("the view reads");
    rig.net.build_verification_batch(BASIC, pend, targets, &mut TestRng(1))
}

/// Pass one alone (offset 0, no ban): the retry set.
fn pass_one(rig: &Rig, view: &FakeView, pend: &[PendingIsLockEntry]) -> BTreeSet<IsLockHash> {
    let passes = Passes::one(view.clone());
    rig.net
        .process_pending_instant_send_locks(&passes, Pass::Current, false, pend, &mut TestRng(1))
        .expect("pass one")
}

fn both_passes(rig: &Rig, passes: &Passes, pend: Vec<PendingIsLockEntry>) {
    rig.net.process_pending_is_locks(passes, pend, &mut TestRng(2)).expect("two passes");
}

/// Eight valid locks of source 1 fill a sub-batch that verifies and is cleared; source 1's ninth,
/// badly signed, fails the last sub-batch alone. Every entry of source 1 is dropped on pass one,
/// the earlier sub-batch's included, and only the ninth is retried; pass two scores source 1 once.
// Core: bls_batchverifier.h:80-83 (Verify + ClearMessages at 8 ids, badSources kept), :114-130
// (one source: no per-source batch; one message: badMessages directly); net_instantsend.cpp:190-205
// (a bad source drops every entry), :340-354 (only badMessages retried, ban on pass two)
#[test]
fn a_bad_signature_after_a_valid_sub_batch_drops_the_whole_source_and_retries_it_alone() {
    let rig = Rig::new(false);
    let view = FakeView::with_quorums(1);
    let mut pend: Vec<_> = (0..8).map(|t| entry(1, lock(t, 1))).collect();
    pend.push(entry(1, lock_signed_by(8, 1, 9)));

    let data = build(&rig, &view, &pend);
    // The first eight were verified and cleared at the eighth push; the ninth waits.
    assert_eq!(data.verify_count, 9);
    assert_eq!(data.unique_source_count(), 1);
    assert!(!data.source_bad(1));

    let retry = pass_one(&rig, &view, &pend);
    assert_eq!(retry, BTreeSet::from([pend[8].islock_hash]));
    assert!(rig.processed().is_empty());
    assert!(scores(&rig).is_empty());

    rig.clear();
    let passes = Passes::one(view);
    both_passes(&rig, &passes, pend);
    assert_eq!(passes.taken(), 2);
    assert!(rig.processed().is_empty());
    assert_eq!(scores(&rig), [(1, SCORE)]);
}

/// Source 1's seven valid locks and one bad one fill a sub-batch that fails: source 1 turns bad
/// there, the bad lock is its bad message, and source 1's ninth lock, valid, is skipped (never
/// pushed), dropped and not retried.
// Core: bls_batchverifier.h:80-83, :110-145; net_instantsend.cpp:118-120 (the skip reads the
// sources the sub-batch found bad), :190-205
#[test]
fn a_source_found_bad_by_a_sub_batch_is_skipped_for_the_rest_of_the_build() {
    let rig = Rig::new(false);
    let view = FakeView::with_quorums(1);
    let mut pend: Vec<_> = (0..7).map(|t| entry(1, lock(t, 1))).collect();
    pend.push(entry(1, lock_signed_by(7, 1, 9)));
    pend.push(entry(1, lock(8, 1)));
    pend.push(entry(2, lock(9, 1)));
    let data = build(&rig, &view, &pend);
    assert_eq!(data.verify_count, 9, "eight, then source 2's; source 1's ninth is skipped");
    assert!(data.source_bad(1) && !data.source_bad(2));
    assert!(data.message_bad(&pend[7].islock_hash));
    assert!(!data.message_bad(&pend[8].islock_hash));
    assert_eq!(pass_one(&rig, &view, &pend), BTreeSet::from([pend[7].islock_hash]));
    assert_eq!(rig.processed(), [(2, pend[9].islock_hash)]);
}

/// Source 1 sends a, b (bad), c; source 2 sends d. The sub-batch fails, source 1's batch fails
/// and source 2's passes; one by one, only b is bad. Pass one processes d and retries b alone;
/// a and c are dropped for their source and not retried; pass two scores source 1.
// Core: bls_batchverifier.h:110-145 (per source, then per message), net_instantsend.cpp:190-205,
// :340-354
#[test]
fn one_invalid_message_among_valid_ones_of_its_source_is_the_only_one_retried() {
    let rig = Rig::new(false);
    let view = FakeView::with_quorums(1);
    let pend = vec![
        entry(1, lock(1, 1)),
        entry(1, lock_signed_by(2, 1, 7)),
        entry(1, lock(3, 1)),
        entry(2, lock(4, 1)),
    ];
    let retry = pass_one(&rig, &view, &pend);
    assert_eq!(retry, BTreeSet::from([pend[1].islock_hash]));
    assert_eq!(rig.processed(), [(2, pend[3].islock_hash)]);

    let rig = Rig::new(false);
    both_passes(&rig, &Passes::one(view), pend.clone());
    assert_eq!(rig.processed(), [(2, pend[3].islock_hash)]);
    assert_eq!(scores(&rig), [(1, SCORE)]);
}

/// A lock whose message is bad (no quorum) from a source that is bad (a later unreadable
/// signature) is retried; the unreadable one is not, as its message is not bad. In pass two the
/// fresh verifier holds no bad source, so the retried lock is a bad message only: no score.
// Core: net_instantsend.cpp:122-126, :148-151, :193-204 (retried "even if its source is also
// bad"), :349-354
#[test]
fn a_message_both_bad_and_from_a_bad_source_is_retried() {
    let rig = Rig::new(false);
    let mut view = FakeView::with_quorums(2);
    view.drop_quorum(2);
    let pend = vec![entry(1, lock(1, 2)), entry(1, unreadable(&lock(2, 1))), entry(2, lock(3, 1))];
    let data = build(&rig, &view, &pend);
    assert!(data.source_bad(1) && data.message_bad(&pend[0].islock_hash));
    assert!(!data.message_bad(&pend[1].islock_hash));

    let retry = pass_one(&rig, &view, &pend);
    assert_eq!(retry, BTreeSet::from([pend[0].islock_hash]));
    assert_eq!(rig.processed(), [(2, pend[2].islock_hash)]);

    let rig = Rig::new(false);
    both_passes(&rig, &Passes::one(view), pend.clone());
    assert_eq!(rig.processed(), [(2, pend[2].islock_hash)]);
    assert!(scores(&rig).is_empty());
}

/// An unreadable signature (source 1) and an unknown cycle hash (source 2) make their sources
/// bad at build time: their later locks, valid ones included, are skipped (not pushed), dropped
/// and not retried. Source 3 is unaffected.
// Core: net_instantsend.cpp:118-134 (badSources at build; later entries skipped), :190-205
#[test]
fn a_build_time_bad_source_skips_its_later_entries() {
    let rig = Rig::new(false);
    let view = FakeView::with_quorums(1);
    let pend = vec![
        entry(1, unreadable(&lock(1, 1))),
        entry(1, lock(2, 1)),
        entry(2, lock(3, 9)),
        entry(2, lock(4, 1)),
        entry(3, lock(5, 1)),
    ];
    let data = build(&rig, &view, &pend);
    assert_eq!(data.verify_count, 1, "only source 3's lock is pushed");
    assert!(data.source_bad(1) && data.source_bad(2) && !data.source_bad(3));
    assert!(hashes(&pend).iter().all(|h| !data.message_bad(h)));

    let passes = Passes::one(view);
    both_passes(&rig, &passes, pend.clone());
    assert_eq!(passes.taken(), 1, "nothing to retry: no second pass");
    assert_eq!(rig.processed(), [(3, pend[4].islock_hash)]);
    assert!(scores(&rig).is_empty());
}

/// SIG-14 (O17): a lock whose quorum is missing is a bad message on its own; the batch's other
/// locks, of its source too, verify and are processed (v24). v23.1.8 dropped the whole batch
/// (`return nullptr`, `instantsend/net_instantsend.cpp:124-127` there). The lock is retried and,
/// if pass two finds its quorum, processed there; if not, it is dropped with no score.
// Core: net_instantsend.cpp:148-151 (v24 badMessages per message), :193-204, :340-354
#[test]
fn sig14_a_missing_quorum_is_a_bad_message_per_lock() {
    let mut no_q2 = FakeView::with_quorums(2);
    no_q2.drop_quorum(2);
    let pend = vec![entry(1, lock(1, 1)), entry(1, lock(2, 2)), entry(2, lock(3, 1))];

    let rig = Rig::new(false);
    let data = build(&rig, &no_q2, &pend);
    assert_eq!(data.verify_count, 2);
    assert!(data.message_bad(&pend[1].islock_hash) && !data.source_bad(1));
    assert_eq!(pass_one(&rig, &no_q2, &pend), BTreeSet::from([pend[1].islock_hash]));
    assert_eq!(rig.processed(), [(1, pend[0].islock_hash), (2, pend[2].islock_hash)]);

    // Still missing on pass two: dropped, nobody scored.
    let rig = Rig::new(false);
    both_passes(&rig, &Passes::one(no_q2.clone()), pend.clone());
    assert_eq!(rig.processed(), [(1, pend[0].islock_hash), (2, pend[2].islock_hash)]);
    assert!(scores(&rig).is_empty());

    // Found on pass two: processed there.
    let rig = Rig::new(false);
    let passes = Passes::new(vec![no_q2, FakeView::with_quorums(2)], vec![BASIC]);
    both_passes(&rig, &passes, pend.clone());
    assert_eq!(
        rig.processed(),
        [(1, pend[0].islock_hash), (2, pend[2].islock_hash), (1, pend[1].islock_hash)]
    );
    assert!(scores(&rig).is_empty());
}

/// Two bad locks of source 1 and one of source 2: pass one scores nobody; pass two scores each
/// bad source +20 once.
// Core: net_instantsend.cpp:36, :196-200 (`ban && source_bad && penalized.emplace`), :341, :354
#[test]
fn pass_two_scores_each_bad_source_once_and_pass_one_none() {
    let view = FakeView::with_quorums(1);
    let pend = vec![
        entry(1, lock_signed_by(1, 1, 5)),
        entry(1, lock_signed_by(2, 1, 6)),
        entry(2, lock_signed_by(3, 1, 7)),
        entry(3, lock(4, 1)),
    ];
    let rig = Rig::new(false);
    let retry = pass_one(&rig, &view, &pend);
    assert_eq!(retry, hashes(&pend[..3]).into_iter().collect());
    assert!(scores(&rig).is_empty());

    let rig = Rig::new(false);
    both_passes(&rig, &Passes::one(view), pend.clone());
    assert_eq!(scores(&rig), [(1, SCORE), (2, SCORE)]);
    assert_eq!(rig.processed(), [(3, pend[3].islock_hash)]);
}

/// The verifier's case (the queue holds each hash once, so it never yields one): one lock id from
/// sources 1 and 2, badly signed, beside source 3's seven valid locks. The id
/// counts once toward the sub-batch: the eighth distinct id is source 3's last, whose push runs
/// the sub-batch and leaves nothing (a double count would have run it one push earlier and left
/// source 3's last lock waiting). Both sources keep the id, so both fail their re-verification
/// and are bad, with the id a bad message; source 3 is good.
// Core: bls_batchverifier.h:72-83 (inserted once, appended to each source), :110-145
#[test]
fn a_duplicate_id_from_two_sources_counts_once_and_fails_both_sources() {
    let rig = Rig::new(false);
    let view = FakeView::with_quorums(1);
    let dup = lock_signed_by(100, 1, 4);
    let mut pend: Vec<_> = (0..6).map(|t| entry(3, lock(t, 1))).collect();
    pend.push(entry(1, dup.clone()));
    pend.push(entry(2, dup));
    pend.push(entry(3, lock(6, 1)));
    let data = build(&rig, &view, &pend);
    assert_eq!(data.verify_count, 9);
    assert_eq!(data.unique_source_count(), 0, "the sub-batch ran at the last push");
    assert!(data.source_bad(1) && data.source_bad(2) && !data.source_bad(3));
    assert!(data.message_bad(&pend[6].islock_hash));
    assert!(pend[..6].iter().chain(&pend[8..]).all(|e| !data.message_bad(&e.islock_hash)));
}

/// A lock whose recovered signature is stored from the same quorum with the same signature is
/// already verified: not pushed, no recovered signature kept, still processed. A stored one of
/// another quorum or signature does not count. An already verified lock of a source that turns
/// out bad is dropped with the rest of its source.
// Core: net_instantsend.cpp:153-158, :167-170, :190-207
#[test]
fn an_already_verified_lock_skips_the_batch_and_is_still_processed() {
    let view = FakeView::with_quorums(2);
    let done = lock(1, 1);
    let other_quorum = lock(2, 1);
    let other_sig = lock(3, 1);
    let rig = Rig::new(false);
    rig.recsigs.store(&done, 1, done.sig());
    rig.recsigs.store(&other_quorum, 2, other_quorum.sig());
    rig.recsigs.store(&other_sig, 1, lock(4, 1).sig());
    let pend = vec![entry(1, done), entry(1, other_quorum), entry(1, other_sig)];
    let data = build(&rig, &view, &pend);
    assert_eq!((data.already_verified, data.verify_count), (1, 2));
    // Their ids have recovered signatures: none is kept.
    assert!(pend.iter().all(|e| data.rec_sig(&e.islock_hash).is_none()));
    assert!(pass_one(&rig, &view, &pend).is_empty());
    assert_eq!(rig.processed().len(), 3);

    // Source 1 also sends a bad lock: the already verified one is dropped with it, not retried.
    let rig = Rig::new(false);
    let done = lock(1, 1);
    rig.recsigs.store(&done, 1, done.sig());
    let pend = vec![entry(1, done), entry(1, lock_signed_by(5, 1, 8))];
    assert_eq!(pass_one(&rig, &view, &pend), BTreeSet::from([pend[1].islock_hash]));
    assert!(rig.processed().is_empty());
}

/// A pushed lock whose id has no recovered signature keeps one rebuilt from the lock, handed to
/// the signing manager right after the lock is processed; none when the id has one at build
/// time, and none when it has one by the time the lock is processed.
// Core: net_instantsend.cpp:164-170, :207-220
#[test]
fn the_rebuilt_recovered_signature_is_pushed_only_when_still_absent() {
    let view = FakeView::with_quorums(1);
    let fresh = lock(1, 1);
    let rig = Rig::new(false);
    let pend = vec![entry(1, fresh.clone())];
    let data = build(&rig, &view, &pend);
    let kept = data.rec_sig(&pend[0].islock_hash).expect("kept");
    assert_eq!(kept.llmq_type, crate::is::s3_fixtures::is_type());
    assert_eq!(kept.quorum_hash.as_bytes(), &quorum_hash(1));
    assert_eq!(kept.id, islock_request_id(fresh.inputs()));
    assert_eq!(&kept.msg_hash, fresh.txid().as_bytes());
    assert_eq!(&kept.sig, fresh.sig().as_bytes());
    pass_one(&rig, &view, &pend);
    assert_eq!(
        rig.events(),
        [Event::Process(1, pend[0].islock_hash), Event::PushRecSig(*fresh.txid().as_bytes())]
    );

    // The id already has a recovered signature (over another message): nothing kept or pushed.
    let rig = Rig::new(false);
    rig.recsigs.has_ids.lock().unwrap().insert(islock_request_id(fresh.inputs()));
    pass_one(&rig, &view, &pend);
    assert_eq!(rig.events(), [Event::Process(1, pend[0].islock_hash)]);

    // One arrives while the lock is processed: kept, but not pushed.
    let rig = Rig::new(false);
    rig.processor.store_id_on_process.store(true, Ordering::SeqCst);
    pass_one(&rig, &view, &pend);
    assert_eq!(rig.events(), [Event::Process(1, pend[0].islock_hash)]);
}

/// The processor's store failure stops the batch: later locks are not processed.
#[test]
fn a_processor_failure_stops_the_batch() {
    let view = FakeView::with_quorums(1);
    let rig = Rig::new(false);
    rig.processor.fail.store(true, Ordering::SeqCst);
    let pend = vec![entry(1, lock(1, 1)), entry(1, lock(2, 1))];
    let passes = Passes::one(view);
    let res = rig.net.process_pending_instant_send_locks(
        &passes,
        Pass::Current,
        false,
        &pend,
        &mut TestRng(1),
    );
    assert!(matches!(res, Err(IsNetError::Store(_))));
    assert_eq!(rig.processed(), [(1, pend[0].islock_hash)]);
}

/// Each pass samples the flag afresh: a basic-signed lock checked under the legacy flag on pass
/// one fails there unscored, and verifies under the basic flag on pass two.
// Core: net_instantsend.cpp:317, :341, :354 (one batch per pass); bls/bls.cpp:335 (the flag read
// at verification); LQ3a D21/D22
#[test]
fn each_pass_samples_its_own_flag() {
    let pend = vec![entry(1, lock(1, 1))];
    let rig = Rig::new(false);
    let passes = Passes::new(vec![FakeView::with_quorums(1)], vec![BlsCtx::LEGACY, BASIC]);
    both_passes(&rig, &passes, pend.clone());
    assert_eq!(passes.taken(), 2);
    assert_eq!(rig.processed(), [(1, pend[0].islock_hash)]);
    assert!(scores(&rig).is_empty());
}

/// An unconfigured InstantSend type, where Core asserts, is an error before any pass.
#[test]
fn an_unconfigured_instantsend_type_is_an_error() {
    let mut params = crate::is::s3_fixtures::params();
    let t = params.consensus.llmq_type_dip0024_instant_send;
    params.consensus.llmq_type_dip0024_instant_send = dash_params::LlmqType::from_base(0xfe);
    assert_ne!(params.consensus.llmq_type_dip0024_instant_send, t);
    let rig = Rig::with_params(params, false);
    let passes = Passes::one(FakeView::with_quorums(1));
    let res =
        rig.net.process_pending_is_locks(&passes, vec![entry(1, lock(1, 1))], &mut TestRng(1));
    assert!(matches!(res, Err(IsNetError::UnknownLlmqType(0xfe))));
    assert_eq!(passes.taken(), 0);
}

/// The stored recovered signature is read into a fresh lazy field, so under the pass's flag, not
/// the lock's read scheme: a lock read under the basic flag whose own signature is stored counts
/// as already verified on a basic pass and not on a legacy one. Zero bytes never match.
// Core: net_instantsend.cpp:153-155; bls/bls.h:411-414, :481-484 (`CRecoveredSig recovered_sig`
// is built, then read from the db, at the pass)
#[test]
fn the_stored_signature_is_read_under_the_pass_flag() {
    let view = FakeView::with_quorums(1);
    let l = lock(1, 1);
    let pend = [entry(1, l.clone())];
    let built = |rig: &Rig, scheme| {
        let targets = rig.net.batch_targets(&view, Pass::Current, &pend).unwrap();
        let data = rig.net.build_verification_batch(scheme, &pend, targets, &mut TestRng(1));
        (data.already_verified, data.verify_count)
    };
    let rig = Rig::new(false);
    rig.recsigs.store(&l, 1, l.sig());
    assert_eq!(built(&rig, BASIC), (1, 0));
    assert_eq!(built(&rig, BlsCtx::LEGACY), (0, 1));

    let rig = Rig::new(false);
    rig.recsigs.store(&l, 1, BlsSigBytes::from([0; 96]));
    assert_eq!(built(&rig, BASIC), (0, 1));
}
