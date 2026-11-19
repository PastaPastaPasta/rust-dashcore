//! LQ3a's `qsigrec` verdict on synthetic views: every step of Core's handling in its order
//! (`llmq/net_signing.cpp` v24:47-62, :220-281; `llmq/signing.cpp` v24:422-475, :488-571), the
//! active-quorum set at the tip, and the no-verdict cases.

use super::tests::{
    View, held, legacy_sign_bytes, public, qhash, regtest, sign_bytes, verified, with_status,
};
use super::*;

const TEST: LlmqType = LlmqType::LlmqTest;

/// LLMQ_TEST's `keepOldConnections` in regtest.
fn keep_old() -> u8 {
    u8::try_from(regtest().consensus.llmq(TEST).expect("LLMQ_TEST").keep_old_connections)
        .expect("small")
}

/// A recovered signature of type `llmq_type` naming quorum `[n; 32]`, over (id, msgHash), signed
/// by key `signer`.
fn recsig_of(llmq_type: LlmqType, n: u8, signer: u8, id: u8, msg: u8) -> RecoveredSig {
    let mut rec = RecoveredSig {
        llmq_type,
        quorum_hash: qhash(n),
        id: [id; 32],
        msg_hash: [msg; 32],
        sig: [0; 96],
    };
    rec.sig = sign_bytes(signer, &rec.sign_hash());
    rec
}

fn recsig(n: u8, signer: u8, id: u8, msg: u8) -> RecoveredSig {
    recsig_of(TEST, n, signer, id, msg)
}

/// The tip's LLMQ_TEST quorums `[1; 32]`, `[2; 32]`, …, `[count; 32]`, newest first, each
/// verified with its own key and held by hash.
fn view_with(count: u8) -> View {
    View::with_recent(1000, TEST, (1..=count).map(|n| verified(TEST, n, 0)).collect())
}

fn verdict(view: &View, rec: &RecoveredSig) -> Result<RecSigVerdict, RecSigError<String>> {
    verify_qsigrec(&regtest(), view, LockSchemes::both(BlsCtx::BASIC), rec)
}

const INACTIVE: Result<RecSigVerdict, RecSigError<String>> =
    Ok(RecSigVerdict::Ignore(RecSigIgnore::InactiveQuorum));
const UNAVAILABLE: Result<RecSigVerdict, RecSigError<String>> = Err(RecSigError::QuorumUnavailable);

fn ban(reason: RecSigBan) -> Result<RecSigVerdict, RecSigError<String>> {
    Ok(RecSigVerdict::Ban(reason))
}

#[test]
fn a_recent_verified_quorums_signature_is_accepted() {
    let view = view_with(2);
    let rec = recsig(2, 2, 7, 8);
    let Ok(RecSigVerdict::Accept(quorum)) = verdict(&view, &rec) else {
        panic!("not accepted: {:?}", verdict(&view, &rec))
    };
    assert_eq!(quorum.quorum_hash(), &qhash(2));
    assert_eq!(quorum.llmq_type(), TEST);
}

#[test]
fn an_unconfigured_type_bans_before_any_read() {
    let view = view_with(1);
    let mut rec = recsig(1, 1, 7, 8);
    rec.llmq_type = LlmqType::from_base(0xfe);
    assert_eq!(verdict(&view, &rec), ban(RecSigBan::UnknownLlmqType));
    // `IsQuorumActive` answers false for it, also without a read.
    assert_eq!(is_quorum_active(&regtest(), &view, rec.llmq_type, &qhash(1)), Ok(false));
    assert!(view.asked.borrow().is_empty());
}

/// Core's `llmq_invalid_type_tests` types (`src/test/llmq_invalid_type_tests.cpp` v24:29-32): 0,
/// no enumerator, and LLMQ_25_67, an enumerator regtest does not use. A `qsigrec` naming either
/// bans its sender (`llmq/net_signing.cpp` v24:56-58), and neither has an active quorum.
#[test]
fn cores_unregistered_types_ban() {
    let view = view_with(1);
    for base in [0, 6] {
        let mut rec = recsig(1, 1, 7, 8);
        rec.llmq_type = LlmqType::from_base(base);
        assert!(regtest().consensus.llmq(rec.llmq_type).is_none(), "{base}");
        assert_eq!(verdict(&view, &rec), ban(RecSigBan::UnknownLlmqType));
        assert_eq!(serves_recovered_sig(&regtest(), &view, &rec), Ok(false));
    }
}

#[test]
fn only_the_newest_keep_old_connections_quorums_are_active() {
    let keep = keep_old();
    let view = view_with(keep + 1);
    let params = regtest();
    assert_eq!(is_quorum_active(&params, &view, TEST, &qhash(keep)), Ok(true));
    assert_eq!(is_quorum_active(&params, &view, TEST, &qhash(keep + 1)), Ok(false));
    // The set read is `ScanQuorums(type, keepOldConnections)`.
    assert_eq!(view.asked.borrow().as_slice(), [(TEST.to_base(), usize::from(keep)); 2]);
    // A correctly signed message from the quorum one past the window is ignored, without
    // penalty, before its key is read.
    assert_eq!(verdict(&view, &recsig(keep + 1, keep + 1, 7, 8)), INACTIVE);
    assert!(matches!(verdict(&view, &recsig(keep, keep, 7, 8)), Ok(RecSigVerdict::Accept(_))));
}

#[test]
fn an_active_quorum_the_view_cannot_give_is_no_verdict() {
    // At one tip every quorum Core's scan lists is one `GetQuorum` builds: a view listing it
    // with no quorum is not Core's state, whatever the signature.
    let mut view = view_with(2);
    view.quorums.remove(qhash(2).as_bytes());
    assert_eq!(verdict(&view, &recsig(2, 2, 7, 8)), UNAVAILABLE);
    let mut rec = recsig(2, 2, 7, 8);
    rec.sig = [0; 96];
    assert_eq!(verdict(&view, &rec), UNAVAILABLE);
}

#[test]
fn an_unreadable_signature_bans_after_the_quorum_checks() {
    let view = view_with(2);
    let mut rec = recsig(2, 2, 7, 8);
    rec.sig = [0; 96];
    assert_eq!(verdict(&view, &rec), ban(RecSigBan::MalformedSignature));
    // Canonical only in the other scheme: invalid under the scheme it is read with.
    let basic = recsig(2, 2, 7, 8);
    assert_eq!(
        verify_qsigrec(&regtest(), &view, LockSchemes::both(BlsCtx::LEGACY), &basic),
        ban(RecSigBan::MalformedSignature)
    );
    // Core gates on the quorum first: the same bytes for an inactive quorum are only ignored.
    rec.quorum_hash = qhash(9);
    assert_eq!(verdict(&view, &rec), INACTIVE);
}

#[test]
fn an_unreadable_signature_bans_whatever_the_quorums_status() {
    // The read does not depend on the key, and Core reads it once the quorum is present.
    for status in [QuorumStatus::Unverified, QuorumStatus::Unavailable] {
        let mut view = view_with(1);
        view.quorums.insert(*qhash(1).as_bytes(), status);
        let mut rec = recsig(1, 1, 7, 8);
        rec.sig = [0; 96];
        assert_eq!(verdict(&view, &rec), ban(RecSigBan::MalformedSignature));
    }
}

#[test]
fn a_signature_the_named_key_rejects_bans() {
    let view = view_with(2);
    let invalid = ban(RecSigBan::InvalidSignature);
    // Another key's signature.
    assert_eq!(verdict(&view, &recsig(2, 1, 7, 8)), invalid);
    // The right key over another request id or message.
    let rec = recsig(2, 2, 7, 8);
    for edit in [|r: &mut RecoveredSig| r.id[0] ^= 1, |r: &mut RecoveredSig| r.msg_hash[0] ^= 1] {
        let mut changed = rec.clone();
        edit(&mut changed);
        assert_eq!(verdict(&view, &changed), invalid, "{changed:?}");
    }
}

#[test]
fn the_quorum_hash_and_type_are_in_the_sign_hash() {
    // The quorum `[1; 32]` holds key 2: a signature by key 2 naming `[2; 32]`, renamed to
    // `[1; 32]`, is checked under the right key over another sign hash.
    let mut view = view_with(2);
    view.quorums.insert(
        *qhash(1).as_bytes(),
        QuorumStatus::Verified(VerifiedQuorum::for_tests(TEST, qhash(1), 0, public(2))),
    );
    let mut renamed = recsig(2, 2, 7, 8);
    renamed.quorum_hash = qhash(1);
    assert_eq!(verdict(&view, &renamed), ban(RecSigBan::InvalidSignature));
    let mut control = renamed.clone();
    control.sig = sign_bytes(2, &control.sign_hash());
    assert!(matches!(verdict(&view, &control), Ok(RecSigVerdict::Accept(_))));

    // LLMQ_TEST_INSTANTSEND's quorum `[2; 32]` with key 2: a signature by key 2 over LLMQ_TEST's
    // sign hash, renamed to the InstantSend type.
    let is = LlmqType::LlmqTestInstantsend;
    let view = View::with_recent(1000, is, vec![verified(is, 2, 0)]);
    let mut retyped = recsig(2, 2, 7, 8);
    retyped.llmq_type = is;
    assert_eq!(verdict(&view, &retyped), ban(RecSigBan::InvalidSignature));
    assert!(matches!(verdict(&view, &recsig_of(is, 2, 2, 7, 8)), Ok(RecSigVerdict::Accept(_))));
}

#[test]
fn a_quorum_not_verified_locally_gives_no_verdict() {
    for status in [QuorumStatus::Unverified, QuorumStatus::Unavailable] {
        let mut view = view_with(2);
        view.quorums.insert(*qhash(2).as_bytes(), status.clone());
        // Valid and invalid signatures alike: the key is not read.
        assert_eq!(verdict(&view, &recsig(2, 2, 7, 8)), UNAVAILABLE);
        assert_eq!(verdict(&view, &recsig(2, 1, 7, 8)), UNAVAILABLE);
        // The active set reads hashes only, whatever the entries' statuses.
        let view = View::with_recent(1000, TEST, vec![with_status(1, 0, status)]);
        assert_eq!(is_quorum_active(&regtest(), &view, TEST, &qhash(1)), Ok(true));
    }
}

#[test]
fn a_view_answering_another_quorum_gives_no_verdict() {
    // The view answers for `[2; 32]` with the quorum `[1; 32]`, or with another type's quorum.
    for answer in [
        VerifiedQuorum::for_tests(TEST, qhash(1), 0, public(2)),
        VerifiedQuorum::for_tests(LlmqType::LlmqTestInstantsend, qhash(2), 0, public(2)),
    ] {
        let mut view = view_with(2);
        view.quorums.insert(*qhash(2).as_bytes(), QuorumStatus::Verified(answer));
        assert_eq!(verdict(&view, &recsig(2, 2, 7, 8)), UNAVAILABLE);
    }
}

#[test]
fn an_active_set_the_view_lacks_gives_no_verdict() {
    let rec = recsig(2, 2, 7, 8);
    for lookup in [None, Some(ScanLookup::Unavailable), Some(ScanLookup::OutsideHorizon)] {
        let mut view = view_with(2);
        view.recent.remove(&TEST.to_base());
        if let Some(lookup) = lookup {
            view.recent.insert(TEST.to_base(), lookup);
        }
        assert_eq!(verdict(&view, &rec), UNAVAILABLE);
        assert_eq!(
            serves_recovered_sig(&regtest(), &view, &rec),
            Err(RecSigError::QuorumUnavailable)
        );
    }
    // A set held as empty is Core's (the type not enabled at the tip): no quorum is active.
    let mut view = view_with(2);
    view.recent.insert(TEST.to_base(), held(false, Vec::new()));
    assert_eq!(verdict(&view, &rec), INACTIVE);
}

#[test]
fn a_held_recovered_sig_is_served_only_while_its_quorum_is_active() {
    let keep = keep_old();
    let view = view_with(keep + 1);
    let params = regtest();
    assert_eq!(serves_recovered_sig(&params, &view, &recsig(1, 1, 7, 8)), Ok(true));
    assert_eq!(serves_recovered_sig(&params, &view, &recsig(keep + 1, 1, 7, 8)), Ok(false));
    let mut unknown = recsig(1, 1, 7, 8);
    unknown.llmq_type = LlmqType::from_base(0xfe);
    assert_eq!(serves_recovered_sig(&params, &view, &unknown), Ok(false));
}

/// Sol r1 M1: the V19 block connects between Core's read of a `qsigrec` and its signing worker's
/// check. The lazy signature keeps the legacy flag it was read under, so a real legacy signature
/// by the named, verified quorum reads as valid; the worker checks it under the basic scheme,
/// whose hash to the curve differs, and bans the sender. Disconnecting the block is the reverse.
#[test]
fn a_signature_checked_across_the_v19_switch_bans() {
    let view = view_with(2);
    let params = regtest();
    let mut legacy = recsig(2, 2, 7, 8);
    legacy.sig = legacy_sign_bytes(2, &legacy.sign_hash());
    let at = |rec: &RecoveredSig, read, check| {
        verify_qsigrec(
            &params,
            &view,
            LockSchemes {
                read,
                check,
            },
            rec,
        )
    };
    // Read and checked before the switch: accepted.
    assert!(matches!(at(&legacy, BlsCtx::LEGACY, BlsCtx::LEGACY), Ok(RecSigVerdict::Accept(_))));
    // Read before it, checked after it: the key rejects the signature.
    assert_eq!(at(&legacy, BlsCtx::LEGACY, BlsCtx::BASIC), ban(RecSigBan::InvalidSignature));
    // Read after it: the legacy bytes do not read under the basic scheme.
    assert_eq!(at(&legacy, BlsCtx::BASIC, BlsCtx::BASIC), ban(RecSigBan::MalformedSignature));
    // The reverse: a basic signature read before the block disconnects, checked after it.
    let basic = recsig(2, 2, 7, 8);
    assert!(matches!(at(&basic, BlsCtx::BASIC, BlsCtx::BASIC), Ok(RecSigVerdict::Accept(_))));
    assert_eq!(at(&basic, BlsCtx::BASIC, BlsCtx::LEGACY), ban(RecSigBan::InvalidSignature));
}
