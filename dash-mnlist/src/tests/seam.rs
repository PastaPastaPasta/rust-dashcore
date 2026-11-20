//! The `test-util` BLS seam reads the tip, after the scan.

use dash_evo::locks::LockReject;

use super::edges::islock;
use super::persist::reloaded;
use super::*;
use crate::{LockOutcome, MnRequest};

fn seam(height: u32) -> LlmqConfig {
    config().clone().with_bls_unsupported_below(height)
}

fn clsig_at(height: i32) -> ClSig {
    clsigs().into_iter().find(|(cl, _)| cl.height() == height).expect("captured").0
}

fn valid_islock() -> IsdLock {
    isdlocks().into_iter().find(|(_, valid)| *valid).expect("captured").0
}

/// Below its height (tip + 1 < height) a lock that selected its quorum reads `BlsUnsupported`;
/// at tip + 1 == height it gets its real verdict. The same after a reload with the same
/// configuration. With no seam, the same chain gives the real verdict.
#[test]
fn the_seam_reads_the_tip_for_chainlocks_and_islocks() {
    let full = FixtureChain::full();
    let below = FixtureChain::anchored(0, 300);
    let mut engine = synced(&full);
    let (cl, is) = (clsig_at(308), valid_islock());
    let unsupported = Err(LockOutcome::BlsUnsupported);
    let checks = |engine: &mut MasternodeListEngine, cfg: &LlmqConfig| {
        (engine.verify_chainlock(&cl, &below, cfg), engine.verify_islock(&is, &below, cfg))
    };

    let (c, i) = checks(&mut engine, &seam(below.tip + 2));
    assert_eq!((c.map(|_| ()), i.map(|_| ())), (unsupported.clone(), unsupported.clone()));
    let (c, i) = checks(&mut engine, &seam(below.tip + 1));
    assert!(c.is_ok() && i.is_ok(), "the boundary: {c:?} {i:?}");
    let (c, i) = checks(&mut engine, config());
    assert!(c.is_ok() && i.is_ok(), "no seam by default: {c:?} {i:?}");

    let mut loaded = reloaded(&mut engine, &full);
    let (c, i) = checks(&mut loaded, &seam(below.tip + 2));
    assert_eq!((c.map(|_| ()), i.map(|_| ())), (unsupported.clone(), unsupported));
    let (c, i) = checks(&mut loaded, &seam(below.tip + 1));
    assert!(c.is_ok() && i.is_ok(), "{c:?} {i:?}");
}

/// The seam applies after the scan: a lock whose set is not held reads `QuorumUnavailable` and
/// asks for the set, as without the seam. Once the set is held the retry reads
/// `BlsUnsupported`, and at the seam's boundary it verifies.
#[test]
fn quorum_unavailable_passes_the_seam_with_its_request() {
    let chain = FixtureChain::full();
    let mut engine = synced(&chain);
    engine.forget_list(chain.id(303));
    let cl = clsig_at(311);
    let cfg = seam(chain.tip + 2);
    assert_eq!(
        engine.verify_chainlock(&cl, &chain, &cfg),
        Err(LockOutcome::Rejected(LockReject::QuorumUnavailable))
    );
    assert_eq!(asked_heights(&engine), [303], "the set is asked for");
    let base = engine
        .requests()
        .into_iter()
        .find_map(|r| match r {
            MnRequest::MnListDiff {
                base: Some(base),
                target_height: 303,
                ..
            } => chain.lookup(&base).expect("ok"),
            _ => None,
        })
        .expect("a based request")
        .0;
    for h in base + 1..=303 {
        engine.apply_diff(&step_diff(h), &chain, config()).unwrap_or_else(|e| panic!("{h}: {e}"));
    }
    assert_eq!(engine.verify_chainlock(&cl, &chain, &cfg), Err(LockOutcome::BlsUnsupported));
    assert!(engine.verify_chainlock(&cl, &chain, &seam(chain.tip + 1)).is_ok());
}

/// Fail-stop wins over the seam: below its height, a store fault in the scan's reads, or in a
/// request the check makes, still reads `Stopped`.
#[test]
fn a_store_failure_stops_a_lock_check_under_the_seam() {
    let full = FixtureChain::full();
    let mut engine = synced(&full);
    let failing = FixtureChain {
        fail_header: Some(287),
        ..FixtureChain::full()
    };
    let cfg = seam(failing.tip + 2);
    assert!(matches!(
        engine.verify_islock(&islock("D"), &failing, &cfg),
        Err(LockOutcome::Stopped(_))
    ));

    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(160), &full, config()).expect("Core's diff");
    let failing = FixtureChain {
        fail_ancestor: Some(160),
        ..FixtureChain::full()
    };
    assert!(matches!(
        engine.verify_chainlock(&clsig_at(311), &failing, &cfg),
        Err(LockOutcome::Stopped(_))
    ));
}
