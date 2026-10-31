//! The engine's edges over the regtest capture: boundaries, the scan's set rule (D10), the
//! `qrinfo` rows and extra cycles (D20, D21), deployments by height, and store failures.

use dash_consensus::ctx::{Deployment, DeploymentPos, DeploymentView};
use dash_evo::cbtx::CoinbaseFacts;
use dash_evo::locks::{LockReject, QuorumStatus};
use dash_evo::sml::SmlList;
use dash_params::BuriedDeployment;

use super::*;
use crate::view::Heights;
use crate::{LockOutcome, MnReject, MnRequest, MnViewError, RequestKey};

fn quorum_hash(t: u8, height: u32) -> QuorumHash {
    let q = core_quorums(t).into_iter().find(|q| q.height == height).expect("Core's quorum");
    QuorumHash::from(q.hash)
}

/// D10: the rotated scan is the set in index order when every index has a commitment, empty
/// when none has, and no scan when only some have.
#[test]
fn the_rotated_scan_is_the_set_only_when_it_covers_every_index() {
    let chain = FixtureChain::full();
    let engine = stepped(&chain);
    let scan = |at: u32, drop: Option<&QuorumHash>| {
        engine.scan_indexes(&chain, config(), llmq(ROTATED), chain.id(at), drop)
    };
    // Cycle 264's quorums are mined at 276.
    assert_eq!(scan(275, None), Some(Vec::new()));
    assert_eq!(scan(276, None), Some(vec![0, 1]));
    assert_eq!(scan(311, None), Some(vec![0, 1]));
    assert_eq!(scan(311, Some(&quorum_hash(ROTATED, 289))), None);
    assert_eq!(scan(311, Some(&quorum_hash(ROTATED, 288))), None);
}

/// The floor itself is held: a cycle whose `qrinfo` reaches exactly the floor (H−4C's work
/// block, D15) asks for it, and verifies when it arrives.
#[test]
fn a_cycle_reaching_exactly_the_floor_asks_for_its_qrinfo_then_verifies() {
    let chain = FixtureChain::anchored(288 - 4 * 24 - 8, 311);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(311), &chain, config()).expect("Core's diff");
    for height in [288, 289] {
        assert_eq!(
            state(&engine, ROTATED, quorum_hash(ROTATED, height)),
            QuorumState::Pending(RequestKey::QrInfo(chain.hash(288)))
        );
    }
    engine.apply_qrinfo(&qrinfo(288, &[]), &chain, config()).expect("Core's qrinfo");
    for height in [288, 289] {
        assert!(matches!(
            state(&engine, ROTATED, quorum_hash(ROTATED, height)),
            QuorumState::Verified(_)
        ));
    }
}

/// A held list off the requested block's chain is never offered as a request's base.
#[test]
fn a_list_off_the_chain_is_never_a_base() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    // A list at height 250 under an id the chain does not place there: another branch's.
    let fork = BlockHash::from([0xfa; 32]);
    engine.hold(BlockId(10_000), 250, fork, SmlList::new(), CoinbaseFacts::NoTransactions);
    engine.apply_diff(&genesis_diff(281), &chain, config()).expect("Core's diff");
    let requests = engine.requests();
    assert!(requests.contains(&MnRequest::MnListDiff {
        base: None,
        target: chain.hash(256),
        target_height: 256,
    }));
    for r in &requests {
        if let MnRequest::QrInfo {
            bases,
            ..
        } = r
        {
            assert!(!bases.contains(&fork), "{r:?}");
        }
    }
}

/// A ChainLock whose selection start block is the header tip is checked now. Of those above the
/// tip the engine holds the lowest, the first it saw at that height (D18): a forged lock far
/// above the tip cannot keep an honest one from waiting.
#[test]
fn the_held_chainlock_is_the_lowest_above_the_tip() {
    let full = FixtureChain::full();
    let mut engine = synced(&full);
    let cl = |h: i32| clsigs().into_iter().find(|(c, _)| c.height == h).expect("captured").0;
    let (cl311, cl308) = (cl(311), cl(308));
    let at_tip = FixtureChain::anchored(0, 311 - 8);
    assert!(engine.verify_chainlock(&cl311, &at_tip, config()).is_ok());

    let short = FixtureChain::anchored(0, 290);
    for order in [[&cl311, &cl308], [&cl308, &cl311]] {
        for c in order {
            assert_eq!(engine.verify_chainlock(c, &short, config()), Err(LockOutcome::Pending));
        }
        assert_eq!(engine.take_pending_chainlock(&full), Ok(Some(cl308.clone())));
    }
    let mut forged = cl311.clone();
    forged.height = i32::MAX;
    let mut other = cl308.clone();
    other.block_hash = full.hash(5);
    for c in [&forged, &cl308, &other] {
        assert_eq!(engine.verify_chainlock(c, &short, config()), Err(LockOutcome::Pending));
    }
    assert_eq!(engine.take_pending_chainlock(&full), Ok(Some(cl308)));
}

/// D6: buried deployments by height (`DeploymentActiveAt` / `After`), versionbits never; a
/// failed header read answers `false` and is latched for the caller.
#[test]
fn deployments_are_read_by_height_and_a_failed_read_is_latched() {
    let chain = FixtureChain::full();
    let heights = Heights::new(config().params(), &chain);
    let v20 = Deployment::Buried(BuriedDeployment::V20);
    assert!(!heights.active_at(BlockId(99), v20));
    assert!(heights.active_at(BlockId(100), v20));
    assert!(!heights.active_after(Some(BlockId(98)), v20));
    assert!(heights.active_after(Some(BlockId(99)), v20));
    assert!(!heights.active_after(None, Deployment::Buried(BuriedDeployment::Dip0003)));
    assert!(!heights.active_at(BlockId(300), Deployment::Versionbits(DeploymentPos::TestDummy)));
    assert_eq!(heights.take_error(), None);

    let failing = FixtureChain {
        fail_header: Some(150),
        ..FixtureChain::full()
    };
    let heights = Heights::new(config().params(), &failing);
    assert!(!heights.active_at(BlockId(150), v20));
    assert!(matches!(heights.take_error(), Some(MnViewError::Storage(_))));
    assert_eq!(heights.take_error(), None);
}

fn islock(name: &str) -> IsdLock {
    let txid = fixture()["provenance"]["locks"]
        .as_array()
        .expect("locks")
        .iter()
        .find(|l| l["name"] == name)
        .and_then(|l| l["txid"].as_str())
        .expect("the lock");
    isdlocks()
        .into_iter()
        .map(|(l, _)| l)
        .find(|l| *l.txid.as_bytes() == rpc(txid))
        .expect("captured")
}

/// A store failure stops a lock check wherever it is read: in the scan's deployment reads, and
/// in a request the check makes.
#[test]
fn a_store_failure_stops_a_lock_check() {
    let full = FixtureChain::full();
    let mut engine = synced(&full);
    // Lock D's scan reads DIP0024 after the block before its cycle (`IsQuorumRotationEnabled`).
    let failing = FixtureChain {
        fail_header: Some(287),
        ..FixtureChain::full()
    };
    assert!(matches!(
        engine.verify_islock(&islock("D"), &failing, config()),
        Err(LockOutcome::Stopped(_))
    ));

    // Without the set at the ChainLock's store block, the check asks for it, from the newest
    // held list below: a read that fails.
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(160), &full, config()).expect("Core's diff");
    let failing = FixtureChain {
        fail_ancestor: Some(160),
        ..FixtureChain::full()
    };
    let cl = clsigs().into_iter().find(|(c, _)| c.height == 311).expect("captured").0;
    assert!(matches!(
        engine.verify_chainlock(&cl, &failing, config()),
        Err(LockOutcome::Stopped(_))
    ));
}

/// A lock whose scan reads below the floor is outside the horizon, not unavailable.
#[test]
fn a_scan_reading_below_the_floor_is_outside_the_horizon() {
    let full = FixtureChain::full();
    let mut engine = synced(&full);
    let chain = FixtureChain::anchored(288, 311);
    assert_eq!(
        engine.verify_islock(&islock("D"), &chain, config()),
        Err(LockOutcome::Rejected(LockReject::OutsideHorizon))
    );
}

/// Step 1: a held base above the target is not its ancestor.
#[test]
fn a_base_above_the_target_is_refused() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(288), &chain, config()).expect("Core's diff");
    let mut d = genesis_diff(281);
    d.base_block_hash = chain.hash(288);
    assert_eq!(
        engine.apply_diff(&d, &chain, config()),
        Err(MnReject::UnknownBase(chain.hash(288)))
    );
}

/// A diff from a held block to itself (Core's empty diff) is accepted and changes nothing.
#[test]
fn a_diff_from_a_block_to_itself_changes_nothing() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(288), &chain, config()).expect("Core's diff");
    let before = engine.list(chain.id(288)).expect("held").clone();
    let mut d = genesis_diff(288);
    d.base_block_hash = chain.hash(288);
    d.mn_list.clear();
    d.deleted_mns.clear();
    d.new_quorums.clear();
    d.deleted_quorums.clear();
    let change = engine.apply_diff(&d, &chain, config()).expect("its own base");
    assert!(change.new_quorums.is_empty());
    assert_eq!(engine.list(chain.id(288)), Some(&before));
    assert!(engine.has_authenticated_set(chain.id(288)));
}

/// Each target cycle's diff sits at its place in Core's construction (H − kC's work block): a
/// diff to another cycle's work block there would key its row to the wrong cycle.
#[test]
fn a_qrinfo_part_out_of_its_place_is_refused() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    // With 256 held, H's diff keeps its base when H − C's diff is replaced.
    engine.apply_diff(&genesis_diff(256), &chain, config()).expect("Core's diff");
    let mut q = qrinfo(288, &[]);
    q.diff_h_c = genesis_diff(160);
    assert_eq!(
        engine.apply_qrinfo(&q, &chain, config()),
        Err(MnReject::QrInfo("a target cycle's diff is not at its place"))
    );
}

/// `qrinfo(288)` with extra cycles whose work blocks are `extras`, each with a copy of H − C's row.
fn with_extras(extras: &[u32]) -> QrInfo {
    let mut q = qrinfo(288, &[]);
    for (i, h) in extras.iter().enumerate() {
        q.mn_list_diff_list.insert(i, genesis_diff(*h));
        q.quorum_snapshot_list.insert(i, q.snapshot_h_c.clone());
    }
    q
}

/// D21: a requested reply's extra cycles are exactly Core's `snapshotHeightsNeeded` at the request
/// block, here none (the extra share makes H − 4C a target cycle). Out of order, repeated or a
/// target they are refused by place; ascending, distinct phantom ones are refused too, and
/// nothing of the reply is held.
#[test]
fn a_requested_reply_with_phantom_extra_cycles_is_refused() {
    let chain = FixtureChain::full();
    let mut engine = stepped(&chain);
    let key = RequestKey::QrInfo(chain.hash(288));
    assert!(engine.requests().iter().any(|r| r.key() == key));
    let not_ascending = Err(MnReject::QrInfo("the extra cycles are not Core's ascending set"));
    assert_eq!(engine.apply_qrinfo(&with_extras(&[160, 136]), &chain, config()), not_ascending);
    assert_eq!(engine.apply_qrinfo(&with_extras(&[160, 160]), &chain, config()), not_ascending);
    assert_eq!(engine.apply_qrinfo(&with_extras(&[256]), &chain, config()), not_ascending);
    assert_eq!(
        engine.apply_qrinfo(&with_extras(&[136, 160]), &chain, config()),
        Err(MnReject::QrInfo("the extra cycles are not Core's set"))
    );
    for cycle in [144, 168, 264] {
        assert_eq!(engine.held_row(llmq(ROTATED), chain.hash(cycle)), None, "{cycle}");
    }
    assert!(engine.requests().iter().any(|r| r.key() == key), "still asked");
    engine.apply_qrinfo(&qrinfo(288, &[]), &chain, config()).expect("Core's qrinfo");
    assert!(!engine.requests().iter().any(|r| r.key() == key));
}

/// D21, D14: without the extra share Core sends H − 4C among the extra cycles, its set at the
/// request block being cycle 264's commitments. A reply in that form (Core's own parts, moved
/// into it) is accepted and its extra row held.
#[test]
fn cores_extra_cycles_are_held() {
    let chain = FixtureChain::full();
    let mut engine = stepped(&chain);
    let mut q = qrinfo(288, &[]);
    let h_4c = q.cycle_h_4c.take().expect("asked with extraShare");
    q.mn_list_diff_list.insert(0, h_4c.diff);
    q.quorum_snapshot_list.insert(0, h_4c.snapshot);
    engine.apply_qrinfo(&q, &chain, config()).expect("Core's set");
    assert!(engine.held_row(llmq(ROTATED), chain.hash(192)).is_some());
}

/// D21: while the set at the request block is not held, the extra cycles cannot be checked: the
/// reply's lists authenticate and its target rows are held, its extra rows (the extra share's
/// too) not.
#[test]
fn extra_rows_wait_for_the_set_at_the_request_block() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(311), &chain, config()).expect("Core's diff");
    let key = RequestKey::QrInfo(chain.hash(288));
    assert_eq!(state(&engine, ROTATED, quorum_hash(ROTATED, 288)), QuorumState::Pending(key));
    engine.apply_qrinfo(&with_extras(&[136, 160]), &chain, config()).expect("authenticated lists");
    // H's work block, within the horizon: its set stays.
    assert!(engine.has_authenticated_set(chain.id(280)));
    // H − 4C (192) is the extra share's, held as an extra cycle (D14).
    for cycle in [144, 168, 192] {
        assert_eq!(engine.held_row(llmq(ROTATED), chain.hash(cycle)), None, "{cycle}");
    }
    assert!(engine.held_row(llmq(ROTATED), chain.hash(264)).is_some());
    for height in [288, 289] {
        assert!(matches!(
            state(&engine, ROTATED, quorum_hash(ROTATED, height)),
            QuorumState::Verified(_)
        ));
    }
}

/// D17: rows are held only from a reply to an outstanding request on H's chain. Another reply
/// adds its lists, and its rows wait for the request they make.
#[test]
fn rows_are_held_only_from_a_requested_qrinfo() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let q = qrinfo(288, &[]);
    engine.apply_qrinfo(&q, &chain, config()).expect("Core's qrinfo");
    assert!(engine.has_authenticated_set(chain.id(280)));
    let key = RequestKey::QrInfo(chain.hash(288));
    for height in [288, 289] {
        assert_eq!(
            state(&engine, ROTATED, quorum_hash(ROTATED, height)),
            QuorumState::Pending(key)
        );
    }
    engine.apply_qrinfo(&q, &chain, config()).expect("Core's qrinfo");
    for height in [288, 289] {
        assert!(matches!(
            state(&engine, ROTATED, quorum_hash(ROTATED, height)),
            QuorumState::Verified(_)
        ));
    }
}

/// A base on another branch than the target is not its ancestor, even at a lower height.
#[test]
fn a_base_on_another_branch_is_refused() {
    let chain = FixtureChain {
        fork: Some(270),
        ..FixtureChain::full()
    };
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(280), &chain, config()).expect("Core's diff");
    let mut d = step_diff(281);
    d.block_hash = chain.fork_hash(281);
    assert_eq!(
        engine.apply_diff(&d, &chain, config()),
        Err(MnReject::UnknownBase(chain.hash(280)))
    );
}

/// A row's diff must target a block on H's chain: the capture's diffs moved onto a branch
/// from 150, with an extra cycle's diff left on the active chain.
#[test]
fn a_row_off_hs_chain_is_refused() {
    let chain = FixtureChain {
        fork: Some(150),
        ..FixtureChain::full()
    };
    let mut q = qrinfo(288, &[]);
    for d in [&mut q.diff_h_3c, &mut q.diff_h_2c, &mut q.diff_h_c, &mut q.diff_h, &mut q.diff_tip] {
        *d = chain.on_fork(d.clone());
    }
    let h_4c = &mut q.cycle_h_4c.as_mut().expect("asked with extraShare").diff;
    *h_4c = chain.on_fork(h_4c.clone());
    q.mn_list_diff_list.insert(0, genesis_diff(160));
    q.quorum_snapshot_list.insert(0, q.snapshot_h_c.clone());
    assert_eq!(
        MasternodeListEngine::new(Mode::Client).apply_qrinfo(&q, &chain, config()),
        Err(MnReject::QrInfo("a row's cycle is not on H's chain"))
    );
}

/// A `qrinfo` for another branch's cycle at the requested height answers no request: its lists
/// are held, its rows are not, and the request stays.
#[test]
fn a_qrinfo_on_another_branch_answers_no_request() {
    let chain = FixtureChain {
        fork: Some(270),
        ..FixtureChain::full()
    };
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(311), &chain, config()).expect("Core's diff");
    let key = RequestKey::QrInfo(chain.hash(288));
    assert!(engine.requests().iter().any(|r| r.key() == key));
    let mut q = qrinfo(288, &[]);
    for d in [&mut q.diff_h, &mut q.diff_tip] {
        *d = chain.on_fork(d.clone());
    }
    engine.apply_qrinfo(&q, &chain, config()).expect("authenticated on its branch");
    assert!(engine.has_authenticated_set(BlockId(FORK + 280)));
    assert!(engine.requests().iter().any(|r| r.key() == key));
    for height in [288, 289] {
        assert_eq!(
            state(&engine, ROTATED, quorum_hash(ROTATED, height)),
            QuorumState::Pending(key)
        );
    }
}

/// A lock reads a quorum from the newest authenticated set holding it on the active chain
/// (Core's `GetQuorum`): each set made to name 288's commitment for 264 in turn.
#[test]
fn a_lock_reads_the_newest_set_on_the_active_chain() {
    let chain = FixtureChain {
        fork: Some(300),
        ..FixtureChain::full()
    };
    let mut engine = synced(&chain);
    let (q, other) = (quorum_hash(CHAIN_LOCKS, 264), quorum_hash(CHAIN_LOCKS, 288));
    let scan = |e: &MasternodeListEngine, q| e.scan_quorum(&chain, config(), llmq(CHAIN_LOCKS), q);
    let (own, others) = (scan(&engine, &q), scan(&engine, &other));
    assert!(matches!(own, Some(QuorumStatus::Verified(_))) && own != others);

    // The tip's set names the other commitment: it is the newest, not an older one.
    engine.point(chain.id(311), llmq(CHAIN_LOCKS), &q, Some(&other));
    assert_eq!(scan(&engine, &q), others);

    // A newer set on another branch is not read: the active chain's newest holding it is 310.
    engine.point(chain.id(311), llmq(CHAIN_LOCKS), &q, None);
    let fork = chain.on_fork(genesis_diff(311));
    engine.apply_diff(&fork, &chain, config()).expect("authenticated on its branch");
    assert!(engine.has_authenticated_set(BlockId(FORK + 311)));
    engine.point(BlockId(FORK + 311), llmq(CHAIN_LOCKS), &q, Some(&other));
    assert_eq!(scan(&engine, &q), own);
}

/// A lock is read and checked under the scheme after the tip (`!DeploymentActiveAfter(tip,
/// V19)`, `evo/specialtxman.cpp` rc.3:983-987): with V19 at the block after the tip, basic.
#[test]
fn a_lock_is_read_under_the_scheme_after_the_tip() {
    let chain = FixtureChain::full();
    let mut engine = synced(&chain);
    let v19_next = config_with(&["-testactivationheight=v19@312"]);
    let cl = clsigs().into_iter().find(|(c, _)| c.height == 311).expect("captured").0;
    assert!(engine.verify_chainlock(&cl, &chain, &v19_next).is_ok());
    assert!(engine.verify_islock(&islock("D"), &chain, &v19_next).is_ok());
    let v19_after = config_with(&["-testactivationheight=v19@313"]);
    assert!(engine.verify_chainlock(&cl, &chain, &v19_after).is_err());
}
