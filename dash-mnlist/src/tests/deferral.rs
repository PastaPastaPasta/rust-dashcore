//! A deferred engine (D57): steps 1–4 and coverage only until the host ends deferral.

use dash_evo::locks::LockReject;

use super::*;
use crate::{Coverage, FetchLookupError, LockOutcome, MnReject, QuorumState};

/// Core's CL quorum based at 288.
fn cores_288() -> QuorumHash {
    let q = core_quorums(CHAIN_LOCKS).into_iter().find(|q| q.height == 288).expect("Core's");
    QuorumHash::from(q.hash)
}

/// Core's ChainLock at `height`.
fn clsig_at(height: i32) -> ClSig {
    clsigs().into_iter().find(|(c, _)| c.height() == height).expect("captured").0
}

/// Every step diff applied deferred: the payloads that need step 5 are refused, the lock checks
/// read `QuorumUnavailable` and ask nothing, `tip_changed` runs retention only, and only coverage
/// asks (over an engine holding only the tip's list). Ending deferral runs step 5: the records leave `Unauthenticated` and the payloads and
/// lock checks work.
#[test]
fn a_deferred_engine_asks_only_for_coverage() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client).with_deferred_verification();
    apply_to(&mut engine, &chain, 24, 25..=chain.tip);
    assert!(engine.is_deferred());
    let hash = cores_288();
    let unauthenticated = |engine: &MasternodeListEngine| {
        engine.quorum(llmq(CHAIN_LOCKS), &hash).map(|(_, s)| s)
            == Some(QuorumState::Unauthenticated)
    };
    assert!(unauthenticated(&engine), "no verdict before step 5");
    assert!(engine.requests().is_empty(), "step 5 asks nothing while deferred");

    let cycle = qrinfo(288, &[QRINFO_BASE]);
    assert_eq!(engine.apply_qrinfo(&cycle, &chain, config()), Err(MnReject::Deferred));
    assert_eq!(
        engine.fetch_lookup(llmq(CHAIN_LOCKS), &hash, chain.tip, &chain, config()),
        Err(FetchLookupError::Reject(MnReject::Deferred))
    );
    assert!(!engine.is_waiting(llmq(CHAIN_LOCKS), &hash, chain.tip));
    let unavailable = Err(LockOutcome::Rejected(LockReject::QuorumUnavailable));
    assert_eq!(engine.verify_chainlock(&clsig_at(311), &chain, config()).map(|_| ()), unavailable);
    let (islock, _) = isdlocks().into_iter().next().expect("captured");
    assert_eq!(engine.verify_islock(&islock, &chain, config()).map(|_| ()), unavailable);
    assert!(engine.requests().is_empty(), "the lock checks ask nothing");
    engine.tip_changed(&chain, config()).expect("retention");
    assert!(unauthenticated(&engine), "retention only");
    assert!(engine.requests().is_empty());

    assert_eq!(engine.coverage(&chain, config()), Ok(Coverage::Covered));
    assert!(engine.requests().is_empty());
    // With only the tip's list held, coverage asks for its ends while deferred.
    let mut tip_only = MasternodeListEngine::new(Mode::Client).with_deferred_verification();
    tip_only.apply_diff(&genesis_diff(chain.tip), &chain, config()).expect("steps 1-4");
    assert!(tip_only.requests().is_empty());
    assert_eq!(tip_only.coverage(&chain, config()), Ok(Coverage::Pending));
    assert!(!tip_only.requests().is_empty(), "coverage asks for its ends");

    engine.end_deferral(&chain, config()).expect("step 5");
    assert!(!engine.is_deferred());
    assert!(
        matches!(
            engine.quorum(llmq(CHAIN_LOCKS), &hash).map(|(_, s)| s),
            Some(QuorumState::Verified(_))
        ),
        "step 5 decided the record"
    );
    engine.apply_qrinfo(&qrinfo(284, &[]), &chain, config()).expect("Core's qrinfo");
    engine.apply_qrinfo(&cycle, &chain, config()).expect("Core's qrinfo");
    assert!(engine.verify_chainlock(&clsig_at(311), &chain, config()).is_ok());
}

/// A ChainLock held across a restart into a deferred engine is not ready and is not cleared until
/// deferral ends; then it is.
#[test]
fn a_held_chainlock_waits_out_deferral() {
    let full = FixtureChain::full();
    let mut engine = synced(&full);
    let short = FixtureChain::anchored(0, 300);
    let cl = clsig_at(311);
    assert_eq!(engine.verify_chainlock(&cl, &short, config()), Err(LockOutcome::Pending));
    let mut engine = super::persist::reloaded(&mut engine, &full).with_deferred_verification();
    assert_eq!(engine.pending_chainlock_ready(&full), Ok(None));
    assert!(!engine.clear_pending_chainlock(&cl));
    let progress = engine.progress();
    assert_eq!(
        engine.verify_chainlock(&cl, &full, config()).map(|_| ()),
        Err(LockOutcome::Rejected(LockReject::QuorumUnavailable))
    );
    assert!(engine.requests().is_empty());
    assert_eq!(engine.progress(), progress);

    engine.end_deferral(&full, config()).expect("step 5");
    assert_eq!(engine.pending_chainlock_ready(&full), Ok(Some(cl.clone())));
    assert!(engine.verify_chainlock(&cl, &full, config()).is_ok());
    assert!(engine.clear_pending_chainlock(&cl));
    assert_eq!(engine.pending_chainlock_ready(&full), Ok(None));
}
