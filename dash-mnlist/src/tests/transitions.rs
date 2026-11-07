//! The engine across DIP0008 and V19 (D12, D19), over Core's capture
//! `tests/data/sb11-regtest-transitions-v24.0.captures.json.gz`: CbTx version 2 and its quorum root
//! from DIP0008 at 300, the basic BLS scheme from V19 at 340; two `llmq_test` quorums mined under
//! version 1, a third under version 2 and the legacy scheme, and ChainLocks on either side.

use dash_evo::locks::{LockReject, QuorumStatus};

use super::*;
use crate::{LockOutcome, MnRequest, RequestKey, Unavailable};

fn height(name: &str) -> u32 {
    u32_of(&transitions()["provenance"][name])
}

/// A quorum the capture names in its provenance.
fn quorum(hash: &Value) -> QuorumHash {
    QuorumHash::from(rpc(hash.as_str().expect("a hash")))
}

fn early() -> Vec<QuorumHash> {
    transitions()["provenance"]["early_quorums"]
        .as_array()
        .expect("early")
        .iter()
        .map(quorum)
        .collect()
}

fn late() -> QuorumHash {
    quorum(&transitions()["provenance"]["late_quorum"])
}

fn chainlock(name: &str) -> ClSig {
    let cl = transitions()["chainlocks"]
        .as_array()
        .expect("chainlocks")
        .iter()
        .find(|c| c["name"] == name)
        .expect("the ChainLock");
    assert_eq!(cl["core_verify"], true, "Core verified it at capture");
    decode(&cl["hex"])
}

/// D19: consecutive diffs across DIP0008. The sets below it are unauthenticated; the first
/// version-2 CbTx authenticates the whole set it commits to, all of it inherited from version-1
/// bases, and every later set is authenticated too. A ChainLock whose scan reads a version-1 set
/// has no authenticated quorum; later ones verify, one selecting a quorum mined under version 1.
#[test]
fn the_first_rooted_set_is_authenticated_from_a_version_1_base() {
    let f = transitions();
    let chain = FixtureChain::of(f);
    let cfg = config_of(f, &[]);
    let dip8 = height("dip8_height");
    let first = 136;
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff_of(f, first), &chain, &cfg).expect("Core's diff");
    for h in first + 1..=chain.tip {
        engine.apply_diff(&step_diff_of(f, h), &chain, &cfg).unwrap_or_else(|e| panic!("{h}: {e}"));
    }
    for h in first..=chain.tip {
        assert_eq!(engine.has_authenticated_set(chain.id(h)), h >= dip8, "{h}");
    }
    for q in early().into_iter().chain([late()]) {
        let (_, state) = engine.quorum(llmq(CHAIN_LOCKS), &q).expect("held");
        assert!(matches!(state, QuorumState::Verified(_)), "{q:?}: {state:?}");
    }
    // 299, 300 and 301 select at the store block 288, below DIP0008.
    for name in ["before", "first rooted", "later rooted"] {
        assert_eq!(
            engine.verify_chainlock(&chainlock(name), &chain, &cfg),
            Err(LockOutcome::Rejected(LockReject::QuorumUnavailable)),
            "{name}"
        );
    }
    // 332 and 340 read rooted sets. Each is checked at the tip Core checked it at: 332's under
    // the legacy scheme, 340's under the basic.
    let mut selected = Vec::new();
    for (name, tip) in [("after", 332), ("after v19", chain.tip)] {
        let at = FixtureChain {
            tip,
            ..FixtureChain::of(f)
        };
        let v = engine.verify_chainlock(&chainlock(name), &at, &cfg).expect("Core's verdict");
        selected.push(*v.quorum.quorum_hash());
    }
    assert_eq!(selected, [late(), early()[1]], "late, then one mined under version 1");
}

/// D12: quorums mined below V19 (the legacy scheme) and first seen in a diff from genesis to V19's
/// block. Step 5 asks for the set at V19 − 1 first, which holds them, then checks them under the
/// legacy scheme of the block that mined them, not the basic one of the block they were first
/// seen in. (Below V20 a quorum's members are its base block's list.)
#[test]
fn a_quorum_mined_below_v19_and_first_seen_above_it_is_checked_under_its_mining_scheme() {
    let f = transitions();
    let chain = FixtureChain::of(f);
    let cfg = config_of(f, &[]);
    let v19 = height("v19_height");
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff_of(f, v19), &chain, &cfg).expect("Core's diff");
    let at = RequestKey::MnListDiff(chain.hash(v19 - 1));
    let quorums = [late(), early()[1]];
    for q in &quorums {
        assert_eq!(engine.quorum(llmq(CHAIN_LOCKS), q).expect("held").1, QuorumState::Pending(at));
    }
    assert!(engine.requests().contains(&MnRequest::MnListDiff {
        base: None,
        target: chain.hash(v19 - 1),
        target_height: v19 - 1,
    }));
    engine.apply_diff(&genesis_diff_of(f, v19 - 1), &chain, &cfg).expect("Core's diff");
    // The quorums' base blocks, 312 and 168.
    for (from, to) in [(304, 312), (160, 168)] {
        engine.apply_diff(&genesis_diff_of(f, from), &chain, &cfg).expect("Core's diff");
        for h in from + 1..=to {
            engine.apply_diff(&step_diff_of(f, h), &chain, &cfg).expect("Core's diff");
        }
    }
    for q in &quorums {
        let state = engine.quorum(llmq(CHAIN_LOCKS), q).expect("held").1;
        assert!(matches!(state, QuorumState::Verified(_)), "{q:?}: {state:?}");
    }
}

/// D12: a quorum whose base block is below V19 and which is mined above it (V19 moved between
/// the late quorum's base, 312, and its mining block, 323). The authenticated set at V19 − 1
/// does not hold it, so it is checked under the basic scheme, which Core's legacy-scheme
/// commitment fails: Core's verdict on that configuration.
#[test]
fn a_quorum_mined_above_v19_from_a_base_below_it_is_checked_under_the_basic_scheme() {
    let f = transitions();
    let chain = FixtureChain::of(f);
    let v19 = 320;
    let cfg = config_of(f, &[&format!("-testactivationheight=v19@{v19}")]);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff_of(f, chain.tip), &chain, &cfg).expect("Core's diff");
    let at = RequestKey::MnListDiff(chain.hash(v19 - 1));
    assert_eq!(
        engine.quorum(llmq(CHAIN_LOCKS), &late()).expect("held").1,
        QuorumState::Pending(at)
    );
    // The set at V19 − 1, and the late quorum's base block's list on the way.
    engine.apply_diff(&genesis_diff_of(f, 304), &chain, &cfg).expect("Core's diff");
    for h in 305..v19 {
        engine.apply_diff(&step_diff_of(f, h), &chain, &cfg).expect("Core's diff");
    }
    assert!(engine.has_authenticated_set(chain.id(v19 - 1)));
    assert_eq!(engine.quorum(llmq(CHAIN_LOCKS), &late()).expect("held").1, QuorumState::Unverified);
}

/// D12: where V19 activates below A_q, no authenticated set says which side of it a quorum was
/// mined on: such a quorum is unavailable, never `Unverified`. (The capture's chain with V19
/// moved to DIP0008's height by a later argument, as Core reads `-testactivationheight`.)
#[test]
fn a_mining_scheme_no_authenticated_set_decides_is_unavailable() {
    let f = transitions();
    let chain = FixtureChain::of(f);
    let dip8 = height("dip8_height");
    let cfg = config_of(f, &[&format!("-testactivationheight=v19@{dip8}")]);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff_of(f, dip8), &chain, &cfg).expect("Core's diff");
    let at = RequestKey::MnListDiff(chain.hash(dip8 - 1));
    for q in early() {
        assert_eq!(engine.quorum(llmq(CHAIN_LOCKS), &q).expect("held").1, QuorumState::Pending(at));
    }
    engine.apply_diff(&genesis_diff_of(f, dip8 - 1), &chain, &cfg).expect("Core's diff");
    assert!(!engine.has_authenticated_set(chain.id(dip8 - 1)));
    for q in early() {
        assert_eq!(
            engine.quorum(llmq(CHAIN_LOCKS), &q).expect("held").1,
            QuorumState::Unavailable(Unavailable::MiningScheme)
        );
    }
}

/// D19: a request diffs from an authenticated set only. A set below A_q may be a peer's (its
/// quorums no root commits to), and Core's delta from it to a rooted target would fail the root.
#[test]
fn a_request_never_diffs_from_an_unauthenticated_set() {
    let f = transitions();
    let chain = FixtureChain::of(f);
    let cfg = config_of(f, &[]);
    let v19 = height("v19_height");
    let asked = |held: &[u32]| {
        let mut engine = MasternodeListEngine::new(Mode::Client);
        for h in held {
            engine.apply_diff(&genesis_diff_of(f, *h), &chain, &cfg).expect("Core's diff");
        }
        engine.requests().into_iter().find_map(|r| match r {
            MnRequest::MnListDiff {
                base,
                target_height,
                ..
            } if target_height == v19 - 1 => Some(base),
            _ => None,
        })
    };
    assert_eq!(asked(&[299, v19]), Some(None), "not from 299");
    assert_eq!(asked(&[299, 300, v19]), Some(Some(chain.hash(300))));
}

/// Below A_q a held set is unauthenticated: its commitments run no step 5, ask for nothing, and
/// no lock may use them, until a rooted set holds them (D19). A verified commitment no
/// authenticated set holds is not one a lock may use either.
#[test]
fn a_set_below_a_q_authenticates_nothing() {
    let f = transitions();
    let chain = FixtureChain::of(f);
    let cfg = config_of(f, &[]);
    let dip8 = height("dip8_height");
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff_of(f, dip8 - 1), &chain, &cfg).expect("Core's diff");
    assert!(!engine.has_authenticated_set(chain.id(dip8 - 1)));
    for q in early() {
        assert_eq!(
            engine.quorum(llmq(CHAIN_LOCKS), &q).expect("held").1,
            QuorumState::Unauthenticated
        );
        assert_eq!(
            engine.scan_quorum(&chain, &cfg, llmq(CHAIN_LOCKS), &q),
            Some(QuorumStatus::Unavailable)
        );
    }
    assert_eq!(engine.requests(), []);

    engine.apply_diff(&step_diff_of(f, dip8), &chain, &cfg).expect("Core's diff");
    for (q, base) in early().iter().zip([144, 168]) {
        let key = RequestKey::MnListDiff(chain.hash(base));
        assert_eq!(engine.quorum(llmq(CHAIN_LOCKS), q).expect("held").1, QuorumState::Pending(key));
    }
    // The quorums' base blocks.
    for (from, to) in [(136, 144), (160, 168)] {
        engine.apply_diff(&genesis_diff_of(f, from), &chain, &cfg).expect("Core's diff");
        for h in from + 1..=to {
            engine.apply_diff(&step_diff_of(f, h), &chain, &cfg).expect("Core's diff");
        }
    }
    for q in early() {
        assert!(matches!(
            engine.scan_quorum(&chain, &cfg, llmq(CHAIN_LOCKS), &q),
            Some(QuorumStatus::Verified(_))
        ));
    }
    engine.forget_authentication(chain.id(dip8));
    for q in early() {
        assert_eq!(
            engine.scan_quorum(&chain, &cfg, llmq(CHAIN_LOCKS), &q),
            Some(QuorumStatus::Unavailable)
        );
    }
    assert_eq!(
        engine.scan_quorum(&chain, &cfg, llmq(CHAIN_LOCKS), &QuorumHash::from([7; 32])),
        None
    );
}

/// The capture's chain with a sibling branch above 330.
fn forked(f: Capture) -> FixtureChain {
    FixtureChain {
        fork: Some(330),
        ..FixtureChain::of(f)
    }
}

/// The request for the set at V19 − 1 on the fork's branch.
fn v19_request_on_fork(chain: &FixtureChain, v19: u32) -> MnRequest {
    MnRequest::MnListDiff {
        base: None,
        target: chain.fork_hash(v19 - 1),
        target_height: v19 - 1,
    }
}

/// D12 across a reorg (Sol r2 major 1). The late quorum, mined at 323 below a fork at 330, is
/// held on both branches from a diff from genesis to V19's block. Its mining scheme is read on
/// the active chain, the one Core serves `getmnlistd` for (`evo/smldiff.cpp` rc.3:177-180): on
/// the first branch the set at V19 − 1 there is asked for and fails; after a reorg onto the
/// other branch the same commitment asks for that branch's set, which with the quorums' base
/// blocks verifies it.
#[test]
fn a_reorg_moves_the_mining_scheme_request_to_the_active_branch() {
    let f = transitions();
    let mut chain = forked(f);
    let cfg = config_of(f, &[]);
    let v19 = height("v19_height");
    let quorums = [late(), early()[1]];
    let state =
        |engine: &MasternodeListEngine, q| engine.quorum(llmq(CHAIN_LOCKS), q).expect("held").1;
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let to_v19 = genesis_diff_of(f, v19);
    engine.apply_diff(&to_v19, &chain, &cfg).expect("Core's diff");
    let on_a = RequestKey::MnListDiff(chain.hash(v19 - 1));
    for q in &quorums {
        assert_eq!(state(&engine, q), QuorumState::Pending(on_a));
    }
    engine.request_failed(on_a);
    // A failure on the active chain stands: the next payload asks for nothing.
    engine.apply_diff(&to_v19, &chain, &cfg).expect("Core's diff");
    assert_eq!(engine.requests(), []);
    for q in &quorums {
        assert_eq!(state(&engine, q), QuorumState::Unavailable(Unavailable::FetchFailed));
    }

    chain.fork_active = true;
    engine.apply_diff(&chain.on_fork(to_v19), &chain, &cfg).expect("the same set on the fork");
    let on_b = v19_request_on_fork(&chain, v19);
    assert_eq!(engine.requests(), std::slice::from_ref(&on_b));
    for q in &quorums {
        assert_eq!(state(&engine, q), QuorumState::Pending(on_b.key()));
    }
    engine
        .apply_diff(&chain.on_fork(genesis_diff_of(f, v19 - 1)), &chain, &cfg)
        .expect("Core's diff");
    // The quorums' base blocks, 312 and 168, below the fork.
    for (from, to) in [(304, 312), (160, 168)] {
        engine.apply_diff(&genesis_diff_of(f, from), &chain, &cfg).expect("Core's diff");
        for h in from + 1..=to {
            engine.apply_diff(&step_diff_of(f, h), &chain, &cfg).expect("Core's diff");
        }
    }
    for q in &quorums {
        assert!(matches!(state(&engine, q), QuorumState::Verified(_)), "{q:?}");
    }
}

/// D12 across a reorg: a request for the stale branch's set no commitment waits on any more is
/// dropped, and the active branch's asked for.
#[test]
fn a_reorg_drops_the_stale_branchs_mining_scheme_request() {
    let f = transitions();
    let mut chain = forked(f);
    let cfg = config_of(f, &[]);
    let v19 = height("v19_height");
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let to_v19 = genesis_diff_of(f, v19);
    engine.apply_diff(&to_v19, &chain, &cfg).expect("Core's diff");
    assert_eq!(engine.requests().len(), 1);
    chain.fork_active = true;
    engine.apply_diff(&chain.on_fork(to_v19), &chain, &cfg).expect("the same set on the fork");
    assert_eq!(engine.requests(), [v19_request_on_fork(&chain, v19)]);
}

/// D12 across a reorg and back (validator r2): the first branch's request fails while the other
/// is active, as its peers cannot serve it. That failure says nothing once the first branch is
/// active again: its set at V19 − 1 is asked for again.
#[test]
fn a_reorg_back_asks_again_for_a_request_failed_off_the_active_chain() {
    let f = transitions();
    let mut chain = forked(f);
    let cfg = config_of(f, &[]);
    let v19 = height("v19_height");
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let to_v19 = genesis_diff_of(f, v19);
    engine.apply_diff(&to_v19, &chain, &cfg).expect("Core's diff");
    let on_a = RequestKey::MnListDiff(chain.hash(v19 - 1));
    chain.fork_active = true;
    engine.request_failed(on_a);
    engine.apply_diff(&chain.on_fork(to_v19.clone()), &chain, &cfg).expect("the fork's");
    let on_b = v19_request_on_fork(&chain, v19);
    assert_eq!(engine.requests(), std::slice::from_ref(&on_b));
    chain.fork_active = false;
    engine.apply_diff(&to_v19, &chain, &cfg).expect("Core's diff");
    assert_eq!(
        engine.quorum(llmq(CHAIN_LOCKS), &late()).expect("held").1,
        QuorumState::Pending(on_a)
    );
    assert!(engine.requests().iter().any(|r| r.key() == on_a));
}

/// A host's failure report for a request a branch change already dropped is ignored: the
/// request is asked for again when its branch is active again (validator r2).
#[test]
fn a_failure_reported_for_a_dropped_request_is_ignored() {
    let f = transitions();
    let mut chain = forked(f);
    let cfg = config_of(f, &[]);
    let v19 = height("v19_height");
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let to_v19 = genesis_diff_of(f, v19);
    engine.apply_diff(&to_v19, &chain, &cfg).expect("Core's diff");
    let on_a = RequestKey::MnListDiff(chain.hash(v19 - 1));
    chain.fork_active = true;
    engine.apply_diff(&chain.on_fork(to_v19.clone()), &chain, &cfg).expect("the fork's");
    assert!(!engine.requests().iter().any(|r| r.key() == on_a), "dropped");
    engine.request_failed(on_a);
    chain.fork_active = false;
    engine.apply_diff(&to_v19, &chain, &cfg).expect("Core's diff");
    assert_eq!(
        engine.quorum(llmq(CHAIN_LOCKS), &late()).expect("held").1,
        QuorumState::Pending(on_a)
    );
}

/// D12 (validator r2): a commitment held only on a branch the chain left needs that branch's set
/// at V19 − 1, a block Core serves no payload for: it asks for nothing, payload after payload,
/// and asks again once a reorg makes the branch active.
#[test]
fn a_commitment_held_only_off_the_active_chain_asks_for_nothing() {
    let f = transitions();
    let mut chain = forked(f);
    let cfg = config_of(f, &[]);
    let v19 = height("v19_height");
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let on_fork = chain.on_fork(genesis_diff_of(f, v19));
    engine.apply_diff(&on_fork, &chain, &cfg).expect("the fork's");
    let on_b = v19_request_on_fork(&chain, v19);
    assert!(engine.requests().is_empty(), "the fork is not active");
    engine.apply_diff(&genesis_diff_of(f, 304), &chain, &cfg).expect("Core's diff");
    for h in 305..=308 {
        engine.apply_diff(&step_diff_of(f, h), &chain, &cfg).expect("Core's diff");
        assert_eq!(
            engine.quorum(llmq(CHAIN_LOCKS), &late()).expect("held").1,
            QuorumState::Unavailable(Unavailable::OffChain),
            "{h}"
        );
        assert!(!engine.requests().iter().any(|r| r.key() == on_b.key()), "{h}");
    }
    chain.fork_active = true;
    engine.apply_diff(&step_diff_of(f, 309), &chain, &cfg).expect("Core's diff");
    assert_eq!(
        engine.quorum(llmq(CHAIN_LOCKS), &late()).expect("held").1,
        QuorumState::Pending(on_b.key())
    );
    assert!(engine.requests().iter().any(|r| r.key() == on_b.key()));
}

/// Two branches mining the late quorum (base 312) on either side of V19, moved to 325: the
/// capture's, which mines it at 323 (the legacy scheme, its commitment verifies), and a sibling
/// above 322 whose sets at 323 and 324 lack it, so it is mined at 325 (the basic scheme, Core's
/// legacy-scheme commitment fails). Both branches' sets are held with the capture's branch
/// active. The sibling's two sets are the capture's with the quorum's entry removed: no root
/// commits to them as altered, which no check below reads.
fn on_either_side_of_v19(f: Capture) -> (FixtureChain, LlmqConfig, MasternodeListEngine) {
    let chain = FixtureChain {
        fork: Some(322),
        ..FixtureChain::of(f)
    };
    let cfg = config_of(f, &["-testactivationheight=v19@325"]);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    // The sibling's diffs base on the capture's lists below the fork.
    engine.keep_every_list();
    engine.apply_diff(&genesis_diff_of(f, 304), &chain, &cfg).expect("Core's diff");
    for h in 305..=325 {
        engine.apply_diff(&step_diff_of(f, h), &chain, &cfg).expect("Core's diff");
    }
    for h in 323..=325 {
        engine.apply_diff(&chain.on_fork(step_diff_of(f, h)), &chain, &cfg).expect("the sibling");
    }
    for h in [323, 324] {
        engine.point(BlockId(FORK + h), llmq(CHAIN_LOCKS), &late(), None);
    }
    (chain, cfg, engine)
}

/// The late quorum's step-5 state.
fn late_state(engine: &MasternodeListEngine) -> QuorumState {
    engine.quorum(llmq(CHAIN_LOCKS), &late()).expect("held").1
}

/// What a lock check reads for the late quorum on the active chain: by its hash, and in the scan
/// from 325, whose store block holds it on either branch.
fn late_reads(
    engine: &MasternodeListEngine,
    chain: &FixtureChain,
    cfg: &LlmqConfig,
) -> [QuorumStatus; 2] {
    let by_hash = engine.scan_quorum(chain, cfg, llmq(CHAIN_LOCKS), &late()).expect("a known base");
    let scan = engine.scan_statuses(chain, cfg, llmq(CHAIN_LOCKS), 325).expect("a scan");
    let (_, in_scan) = scan.into_iter().find(|(q, _)| *q == late()).expect("in the scan");
    [by_hash, in_scan]
}

/// D12 across a reorg (Sol r3 major 1): a verdict holds on the branch whose sets decided its
/// mining scheme. After a reorg onto the sibling, a lock check reads no status for the late
/// quorum until step 5 runs again, which finds it `Unverified` there; back on the capture's
/// branch the stale `Unverified` is read as none, and step 5 verifies it again. The tip moves with
/// no payload: `tip_changed` runs step 5.
#[test]
fn a_reorg_across_the_mining_scheme_reruns_step_5_before_a_lock_reads_the_verdict() {
    let f = transitions();
    let (mut chain, cfg, mut engine) = on_either_side_of_v19(f);
    let verified = |s: &QuorumStatus| matches!(s, QuorumStatus::Verified(_));
    assert!(matches!(late_state(&engine), QuorumState::Verified(_)));
    assert!(late_reads(&engine, &chain, &cfg).iter().all(verified));

    chain.fork_active = true;
    // The capture's branch's verdict: no key for a lock until step 5 runs on the sibling.
    assert_eq!(late_reads(&engine, &chain, &cfg), [const { QuorumStatus::Unavailable }; 2]);
    engine.tip_changed(&chain, &cfg).expect("no stop");
    assert_eq!(late_state(&engine), QuorumState::Unverified);
    assert_eq!(late_reads(&engine, &chain, &cfg), [const { QuorumStatus::Unverified }; 2]);

    chain.fork_active = false;
    assert_eq!(late_reads(&engine, &chain, &cfg), [const { QuorumStatus::Unavailable }; 2]);
    engine.tip_changed(&chain, &cfg).expect("no stop");
    assert!(matches!(late_state(&engine), QuorumState::Verified(_)));
    assert!(late_reads(&engine, &chain, &cfg).iter().all(verified));
}

/// D12 across a reorg: an applied payload runs step 5 for a verdict decided off the active chain,
/// in either order.
#[test]
fn a_payload_after_a_reorg_across_the_mining_scheme_rechecks_the_verdict() {
    let f = transitions();
    let (mut chain, cfg, mut engine) = on_either_side_of_v19(f);
    chain.fork_active = true;
    engine.apply_diff(&chain.on_fork(step_diff_of(f, 326)), &chain, &cfg).expect("the sibling");
    assert_eq!(late_state(&engine), QuorumState::Unverified);
    assert_eq!(late_reads(&engine, &chain, &cfg), [const { QuorumStatus::Unverified }; 2]);
    chain.fork_active = false;
    engine.apply_diff(&step_diff_of(f, 326), &chain, &cfg).expect("Core's diff");
    assert!(matches!(late_state(&engine), QuorumState::Verified(_)));
    assert!(
        late_reads(&engine, &chain, &cfg).iter().all(|s| matches!(s, QuorumStatus::Verified(_)))
    );
}

/// D12: a verdict held only on a branch the chain left stands, and step 5 ends: no set on the
/// active chain holds the commitment, so its decider stays the left branch's. A lock check reads
/// none there, and the verdict again as soon as that branch is active, with no step 5 between.
#[test]
fn a_verdict_held_only_on_a_branch_the_chain_left_stands() {
    let f = transitions();
    let mut chain = FixtureChain {
        fork: Some(322),
        ..FixtureChain::of(f)
    };
    let cfg = config_of(f, &["-testactivationheight=v19@325"]);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    // The payload below the fork diffs from the capture's list at 321.
    engine.keep_every_list();
    engine.apply_diff(&genesis_diff_of(f, 304), &chain, &cfg).expect("Core's diff");
    for h in 305..=325 {
        engine.apply_diff(&step_diff_of(f, h), &chain, &cfg).expect("Core's diff");
    }
    let by_hash = |engine: &MasternodeListEngine, chain: &FixtureChain| {
        engine.scan_quorum(chain, &cfg, llmq(CHAIN_LOCKS), &late()).expect("a known base")
    };
    assert!(matches!(late_state(&engine), QuorumState::Verified(_)));
    chain.fork_active = true;
    // A payload below the fork: the late quorum is in no set on the sibling.
    engine.apply_diff(&step_diff_of(f, 322), &chain, &cfg).expect("Core's diff");
    assert!(matches!(late_state(&engine), QuorumState::Verified(_)));
    assert_eq!(by_hash(&engine, &chain), QuorumStatus::Unavailable);
    chain.fork_active = false;
    assert!(matches!(by_hash(&engine, &chain), QuorumStatus::Verified(_)));
}

/// The held lists the newest-first reads visit while `f` runs.
fn list_reads(f: impl FnOnce()) -> u64 {
    crate::engine::LIST_READS.with(|r| r.set(0));
    f();
    crate::engine::LIST_READS.with(std::cell::Cell::get)
}

/// A request diffs from the newest authenticated held list on its chain at or below the block
/// before its target: the list right below it when held. (V19 at 320: the late quorum, mined at
/// 323, asks for the set at 319.)
#[test]
fn a_request_diffs_from_the_held_list_right_below_its_target() {
    let f = transitions();
    let chain = FixtureChain::of(f);
    let cfg = config_of(f, &["-testactivationheight=v19@320"]);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff_of(f, 304), &chain, &cfg).expect("Core's diff");
    for h in 305..=318 {
        engine.apply_diff(&step_diff_of(f, h), &chain, &cfg).expect("Core's diff");
    }
    engine.apply_diff(&genesis_diff_of(f, chain.tip), &chain, &cfg).expect("Core's diff");
    let wanted = MnRequest::MnListDiff {
        base: Some(chain.hash(318)),
        target: chain.hash(319),
        target_height: 319,
    };
    assert!(engine.requests().contains(&wanted), "{:?}", engine.requests());
}

/// The reads that want the newest held list matching something visit the held lists newest
/// first and stop at the first match, so their cost does not grow with the lists held: what the
/// testnet replay's 1300 lists made quadratic. With 37 lists held: a lookup by hash and a lock
/// check's read of the late quorum (held at 323 to 340) visit one list each, and a diff whose
/// step 5 asks again for two lists below every held one visits none for them.
#[test]
fn the_newest_held_list_reads_visit_no_more_lists_than_they_return() {
    let f = transitions();
    let chain = FixtureChain::of(f);
    let cfg = config_of(f, &[]);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff_of(f, 304), &chain, &cfg).expect("Core's diff");
    for h in 305..=339 {
        engine.apply_diff(&step_diff_of(f, h), &chain, &cfg).expect("Core's diff");
    }
    let step = list_reads(|| {
        engine.apply_diff(&step_diff_of(f, 340), &chain, &cfg).expect("Core's diff");
    });
    // The early quorums' base blocks, 168 and 144.
    assert_eq!(engine.requests().len(), 2);
    assert_eq!(step, 0);
    assert_eq!(list_reads(|| assert!(engine.quorum(llmq(CHAIN_LOCKS), &late()).is_some())), 1);
    let scan = list_reads(|| {
        assert!(engine.scan_quorum(&chain, &cfg, llmq(CHAIN_LOCKS), &late()).is_some());
    });
    assert_eq!(scan, 1);
}
