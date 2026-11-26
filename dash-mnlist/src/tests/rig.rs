//! The capture rig (SB15 §2.1 (e)) drives the engine's real payload and request path with Core's
//! testnet replies.

use dash_pkc::bls::BlsSchemeKind;

use crate::{CaptureRig, LockOutcome, Lookup, MnRequest};

/// A ChainLock held `Pending` reaches `Ok` once the host's tip diffs reach its selection start,
/// and a Platform lookup above the synced tip reaches `Found` through its fetch, answered with
/// Core's step diff. Both go through `verify_chainlock`, `apply_diff`, `fetch_lookup`, `requests`
/// and `request_failed` only.
#[test]
fn the_rig_drives_a_lock_and_a_lookup_through_cores_replies() {
    const MINED: u32 = 1_574_531;
    let mut rig = CaptureRig::testnet(1_574_496);

    let cl = rig.chainlock(1_574_516).expect("captured");
    assert_eq!(
        rig.engine.verify_chainlock(&cl, &rig.chain, &rig.config),
        Err(LockOutcome::Pending)
    );
    assert_eq!(rig.engine.pending_chainlock_ready(&rig.chain), Ok(None));
    rig.advance(MINED - 1);
    assert_eq!(rig.engine.pending_chainlock_ready(&rig.chain), Ok(Some(cl.clone())));
    let v = rig.engine.verify_chainlock(&cl, &rig.chain, &rig.config).expect("Core's verdict");
    assert_eq!((v.height, v.block_hash), (cl.height(), cl.block_hash()));
    assert!(rig.engine.clear_pending_chainlock(&cl));

    let platform = rig.config.params().consensus.llmq_type_platform;
    let (hash, core_key) = rig.quorum(platform, 1_574_520).expect("Core lists it");
    rig.chain.set_tip(rig.chain.captured_tip());
    let lookup = |rig: &CaptureRig| {
        rig.engine.lookup_quorum(platform, &hash, MINED, &rig.chain, &rig.config).expect("held")
    };
    assert_eq!(lookup(&rig), Lookup::Fetch);
    rig.engine.fetch_lookup(platform, &hash, MINED, &rig.chain, &rig.config).expect("not busy");
    assert!(rig.engine.is_waiting(platform, &hash, MINED));
    let step = MnRequest::MnListDiff {
        base: Some(rig.chain.hash(MINED - 1)),
        target: rig.chain.hash(MINED),
        target_height: MINED,
    };
    assert!(rig.engine.requests().contains(&step), "its step diff, which Core's reply answers");
    let unanswered = rig.pump();
    assert!(!rig.engine.requests().contains(&step), "answered");
    let Lookup::Found(found) = lookup(&rig) else {
        panic!("found from Core's step diff")
    };
    assert!(!rig.engine.is_waiting(platform, &hash, MINED));
    let key = found.quorum.public_key().to_bytes(BlsSchemeKind::Basic);
    assert_eq!(key.as_slice(), core_key.as_slice(), "Core's key");
    assert_eq!(found.commitment.quorum_hash(), hash);

    // What no capture answers stays out until the host gives up on it.
    let left = unanswered.first().expect("a request no capture answers").key();
    assert!(rig.engine.requests().iter().any(|r| r.key() == left));
    rig.fail(left);
    assert!(!rig.engine.requests().iter().any(|r| r.key() == left));
    assert!(matches!(lookup(&rig), Lookup::Found(_)));
}
