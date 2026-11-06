//! Retention (SB0 §5.3, D28–D31) over the regtest capture.

use super::*;
use crate::{Coverage, MnRequest, RequestKey};

/// The capture one block at a time with the header tip at each block, as a client following the
/// chain from the list at `from_genesis`; the ChainLocks at `cls` verified while their block is
/// the tip, as they arrive.
fn following(chain: &mut FixtureChain, from_genesis: u32, cls: &[u32]) -> MasternodeListEngine {
    let tip = chain.tip;
    let mut engine = MasternodeListEngine::new(Mode::Client);
    chain.tip = from_genesis;
    engine.apply_diff(&genesis_diff(from_genesis), chain, config()).expect("Core's diff");
    for h in from_genesis + 1..=tip {
        chain.tip = h;
        engine.apply_diff(&step_diff(h), chain, config()).unwrap_or_else(|e| panic!("{h}: {e}"));
        if cls.contains(&h) {
            verify_chainlock_at(&mut engine, chain, h);
        }
    }
    engine
}

/// Verifies Core's ChainLock at `height`.
fn verify_chainlock_at(engine: &mut MasternodeListEngine, chain: &dyn MnChain, height: u32) {
    let height = i32::try_from(height).expect("small");
    let (lock, _) = clsigs().into_iter().find(|(c, _)| c.height == height).expect("one");
    engine.verify_chainlock(&lock, chain, config()).expect("Core verified it");
}

/// Fails every outstanding request, so only the retention rules keep a list.
fn fail_requests(engine: &mut MasternodeListEngine) {
    for key in engine.requests().iter().map(MnRequest::key) {
        engine.request_failed(key);
    }
}

/// After a switch to the fork's branch, the base coverage's request for the tip − 8 set on it
/// diffs from.
fn reorg_base(mut chain: FixtureChain, from_genesis: u32, cl: Option<u32>) -> Option<BlockHash> {
    let mut engine = following(&mut chain, from_genesis, cl.as_slice());
    chain.fork_active = true;
    engine.tip_changed(&chain, config()).expect("step 5");
    assert_eq!(engine.coverage(&chain, config()), Ok(Coverage::Pending));
    let target = chain.fork_hash(chain.tip - 8);
    let asked = engine.requests().into_iter().find_map(|r| match r {
        MnRequest::MnListDiff {
            base,
            target: t,
            ..
        } if t == target => Some(base),
        _ => None,
    });
    asked.expect("the set at tip − 8 on the fork is asked for")
}

/// D28: a diff that deletes and adds no quorum shares its base's set; one that changes a quorum
/// holds its own. Over the horizon's 56 diffs, 4 change a quorum: 5 distinct sets.
#[test]
fn a_diff_changing_no_quorum_shares_its_bases_set() {
    let chain = FixtureChain::full();
    let engine = stepped(&chain);
    let mut changed = 0;
    for h in chain.tip - config().h_keep() + 1..=chain.tip {
        let d = step_diff(h);
        let same = d.new_quorums.is_empty() && d.deleted_quorums.is_empty();
        assert_eq!(engine.shares_set(chain.id(h - 1), chain.id(h)), same, "{h}");
        changed += usize::from(!same);
    }
    assert_eq!(changed, 4);
}

/// D29, D30: after the step diffs, the sets within tip − H_keep (255) are held and none below; the
/// lists held whole are D29's, every other set without its list: the tip's (311); the rotated
/// type's work blocks of the next cycle 312, k = 0…3 (304, 280, 256, 232), 304 also the other
/// types'; those the commitments waiting on cycle 264's and 288's `qrinfo`s read (184, 208 with
/// those); and the bases of those requests (208, 232, 240, 256, 280; 240 also the test's pin).
#[test]
fn only_d29s_lists_are_held_whole() {
    let chain = FixtureChain::full();
    let engine = stepped(&chain);
    let horizon = chain.tip - config().h_keep();
    for h in FIRST..=chain.tip {
        let held = h >= horizon || engine.list(chain.id(h)).is_some();
        assert_eq!(engine.has_authenticated_set(chain.id(h)), held, "{h}");
    }
    assert_eq!(engine.whole_lists(), [184, 208, 232, 240, 256, 280, 304, 311]);
    for r in engine.requests() {
        let MnRequest::QrInfo {
            bases,
            ..
        } = r
        else {
            panic!("only the rotated cycles wait: {r:?}");
        };
        for base in bases {
            assert!(engine.list(chain.lookup(&base).unwrap().expect("held")).is_some());
        }
    }
}

/// DEC-387 (Q4): the list at the newest verified ChainLock (288, verified as it arrived) is kept
/// as the reorg base. After a switch to a branch forking at 300, the request for the set at its
/// tip − 8 diffs from it: on the new active chain and below the target, as Core's `getmnlistd`
/// requires (`evo/smldiff.cpp` rc.3:177-183). With no ChainLock held, 288 goes when it stops
/// being the tip, and the base is the newest list kept on that chain, the work block 280.
#[test]
fn a_request_after_a_tip_switch_diffs_from_the_chainlocks_list() {
    let chain = || FixtureChain {
        fork: Some(300),
        ..FixtureChain::full()
    };
    let base = reorg_base(chain(), FIRST, Some(288)).expect("a base");
    let c = chain();
    assert_eq!(base, c.hash(288));
    let new_tip = BlockId(FORK + c.tip - 8);
    assert_eq!(c.ancestry().ancestor(new_tip, 288), Ok(Some(c.id(288))));
    assert_eq!(reorg_base(chain(), FIRST, None), Some(c.hash(280)));
}

/// DEC-387 (Q4): with no ChainLock held and no list held on the new branch (every list is above
/// its fork at 285), the request after the switch has a null base.
#[test]
fn a_request_after_a_switch_below_every_list_has_a_null_base() {
    let chain = FixtureChain {
        fork: Some(285),
        ..FixtureChain::full()
    };
    assert_eq!(reorg_base(chain, 281, None), None);
}

/// DEC-387 (Q4): the reorg base follows the newest ChainLock verified. With the locks at 288 and
/// 308 verified as they arrive, the list at 308, no other rule's, is held whole; an older lock
/// verified after them leaves it so.
#[test]
fn the_reorg_base_follows_the_newest_chainlock() {
    let mut chain = FixtureChain::full();
    let mut engine = following(&mut chain, FIRST, &[288, 308]);
    let whole = |engine: &MasternodeListEngine| {
        (engine.best_chainlock(), engine.whole_lists().contains(&308))
    };
    assert_eq!(whole(&engine), (Some(308), true));
    verify_chainlock_at(&mut engine, &chain, 288);
    engine.tip_changed(&chain, config()).expect("step 5");
    assert_eq!(whole(&engine), (Some(308), true));
}

/// D29 (1), validator r2 finding 3: the engine's own fetch reply does not displace the host's
/// last diff as the base of its next tip diff. With the header tip at 297, the host applies the
/// list at 281; coverage fetches 289 (tip − 8), and that reply is applied; the host's step diff
/// 281 → 282 still applies.
#[test]
fn a_fetch_reply_leaves_the_hosts_tip_diff_base() {
    let mut chain = FixtureChain::full();
    chain.tip = 297;
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(281), &chain, config()).expect("Core's diff");
    assert_eq!(engine.coverage(&chain, config()), Ok(Coverage::Pending));
    let fetch = engine
        .requests()
        .into_iter()
        .any(|r| matches!(r, MnRequest::MnListDiff { target, .. } if target == chain.hash(289)));
    assert!(fetch, "{:?}", engine.requests());
    engine.apply_diff(&genesis_diff(289), &chain, config()).expect("Core's diff");
    engine.apply_diff(&step_diff(282), &chain, config()).expect("its base is held whole");
}

/// D34, validator r3: the host's tip diff to the header tip stays its next diff's base when a
/// lock's first pass asked for the same block (D27), and the second reply for that block changes
/// nothing new. With the header tip at 282 and a request for it, the host's step diff 281 → 282
/// is its next base, so 281 is no longer held whole; the fetch's reply 281 → 282 arriving after
/// the host's holds nothing new; the host's 282 → 283 applies.
#[test]
fn the_hosts_tip_diff_at_the_header_tip_is_its_base_despite_a_request() {
    let mut chain = FixtureChain::full();
    chain.tip = 281;
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&genesis_diff(281), &chain, config()).expect("Core's diff");
    chain.tip = 282;
    engine.ask(MnRequest::MnListDiff {
        base: Some(chain.hash(281)),
        target: chain.hash(282),
        target_height: 282,
    });
    engine.apply_diff(&step_diff(282), &chain, config()).expect("Core's diff");
    assert!(!engine.whole_lists().contains(&281), "{:?}", engine.whole_lists());
    let again = engine.apply_diff(&step_diff(282), &chain, config()).expect("held already");
    assert!(again.targets.is_empty());
    chain.tip = 283;
    engine.apply_diff(&step_diff(283), &chain, config()).expect("its base is held whole");
}

/// The fetch at 282 asked while the header tip was 282, its request.
fn fetch_282(chain: &FixtureChain) -> MnRequest {
    MnRequest::MnListDiff {
        base: Some(chain.hash(281)),
        target: chain.hash(282),
        target_height: 282,
    }
}

/// D34, validator r4: with the header tip at 283 and the host's base 281, the fetch's reply for
/// 282 lands first and is no base; the host's 281 → 282 after it holds nothing new and makes 282
/// its base (minor 1). A diff to 282 with another coinbase is no second reply: steps 1–4 refuse
/// it (minor 3). A reply to a request the host gave up on is a fetch's too (minor 2).
#[test]
fn the_second_reply_for_a_block_makes_the_hosts_base() {
    let start = || {
        let mut chain = FixtureChain::full();
        chain.tip = 281;
        let mut engine = MasternodeListEngine::new(Mode::Client);
        engine.apply_diff(&genesis_diff(281), &chain, config()).expect("Core's diff");
        chain.tip = 283;
        engine.ask(fetch_282(&chain));
        (chain, engine)
    };
    let (chain, mut engine) = start();
    engine.apply_diff(&step_diff(282), &chain, config()).expect("the fetch's reply");
    assert!(engine.whole_lists().contains(&281));
    let forged = MnListDiff {
        cb_tx: step_diff(283).cb_tx,
        ..step_diff(282)
    };
    assert!(engine.apply_diff(&forged, &chain, config()).is_err());
    engine.apply_diff(&step_diff(282), &chain, config()).expect("the host's");
    assert!(!engine.whole_lists().contains(&281), "{:?}", engine.whole_lists());

    let (chain, mut engine) = start();
    engine.request_failed(fetch_282(&chain).key());
    engine.apply_diff(&step_diff(282), &chain, config()).expect("a late reply");
    assert!(engine.whole_lists().contains(&281));
}

/// D34, validator r4 minor 4, Sol s3 r1 M1: the host's last diff (311) is no base once a reorg
/// leaves it on a branch the chain left: not kept whole, not the engine's base, and its stored
/// mark deleted in the same writes, not left for a restart to clear.
#[test]
fn the_hosts_base_on_a_branch_the_chain_left_goes() {
    let mut chain = FixtureChain {
        fork: Some(300),
        ..FixtureChain::full()
    };
    let mut engine = following(&mut chain, FIRST, &[]);
    assert!(engine.whole_lists().contains(&311));
    assert_eq!(engine.host_base(), Some(311));
    let mark = &b"Mlast_diff"[..];
    let store = super::persist::Store::new();
    store.commit(&engine.take_dirty());
    assert!(store.rows().contains_key(mark));
    chain.fork_active = true;
    engine.tip_changed(&chain, config()).expect("step 5");
    assert!(!engine.whole_lists().contains(&311), "{:?}", engine.whole_lists());
    assert_eq!(engine.host_base(), None);
    let w = engine.take_dirty();
    assert!(w.ops().any(|(k, v)| k == mark && v.is_none()), "the stale mark is deleted");
    store.commit(&w);
    assert!(!store.rows().contains_key(mark));
}

/// DEC-387 Q4, Opus s3 r1 m2, DEC-412: a verified lock whose block is not yet a header (D18: up
/// to 8 above the tip) leaves the newest lock whose block is known, 288, the reorg base, its list
/// kept, and the base survives a restart. The requests are failed so only the lock rule keeps it.
#[test]
fn the_reorg_base_stays_while_the_newest_lock_is_ahead_of_headers() {
    let mut chain = FixtureChain::full();
    let mut engine = following(&mut chain, FIRST, &[288]);
    fail_requests(&mut engine);
    engine.hold_chainlock(chain.tip + 1, BlockHash::from([7u8; 32]));
    engine.tip_changed(&chain, config()).expect("step 5");
    assert_eq!(engine.best_chainlock(), Some(chain.tip + 1));
    assert_eq!(engine.reorg_base(), Some(288));
    assert!(engine.whole_lists().contains(&288), "{:?}", engine.whole_lists());
    let loaded = super::persist::reloaded(&mut engine, &chain);
    assert_eq!(loaded.reorg_base(), Some(288));
}

/// D29, Opus s3 r1 nit 5 (mutant M12): the reorg base is the newest list held whole on the
/// lock's chain. A lock on the fork's block at the tip bases on 288, below the fork, not on the
/// active chain's tip list. The requests are failed so only the lock rule keeps 288.
#[test]
fn the_reorg_base_is_on_the_locks_chain() {
    let mut chain = FixtureChain {
        fork: Some(300),
        ..FixtureChain::full()
    };
    let mut engine = following(&mut chain, FIRST, &[288]);
    fail_requests(&mut engine);
    engine.hold_chainlock(chain.tip, chain.fork_hash(chain.tip));
    engine.tip_changed(&chain, config()).expect("step 5");
    assert_eq!(engine.reorg_base(), Some(chain.tip));
    assert!(engine.whole_lists().contains(&288), "{:?}", engine.whole_lists());
}

/// DEC-387 Q4, DEC-412: a lock whose header arrived since it was verified becomes the reorg base
/// when a newer lock ahead of headers replaces it, though no retention ran in between: the lock
/// at 301, verified at tip 300, then the header at 302, then Core's lock at 308.
#[test]
fn a_lock_replaced_after_its_header_came_is_the_reorg_base() {
    let mut chain = FixtureChain::full();
    chain.tip = 300;
    let mut engine = following(&mut chain, FIRST, &[288]);
    assert_eq!(engine.reorg_base(), Some(288));
    engine.hold_chainlock(301, chain.hash(301));
    chain.tip = 302;
    verify_chainlock_at(&mut engine, &chain, 308);
    assert_eq!(engine.best_chainlock(), Some(308));
    assert_eq!(engine.reorg_base(), Some(301));
}

/// Opus s3 r2 nit 2: a lock whose block is a header at another height is no reorg base, as Core's
/// `bestChainLockWithKnownBlock` (`chainlock.cpp` rc.3:117-127): the base stays 288.
#[test]
fn a_lock_on_a_block_at_another_height_is_no_reorg_base() {
    let mut chain = FixtureChain::full();
    let mut engine = following(&mut chain, FIRST, &[288]);
    engine.hold_chainlock(chain.tip, chain.hash(chain.tip - 1));
    engine.tip_changed(&chain, config()).expect("step 5");
    assert_eq!(engine.reorg_base(), Some(288));
}

/// BL-379, D45: a rotation row stays while a held rotated commitment's cycle c reads it (c and
/// c − C … c − 3C) or a kept candidate's confirmation does (its own c − C … c − 3C). The capture's
/// held rotated commitments are cycles 264's and 288's: their quarters 192–264 stay, all
/// candidates, and 168, which the candidates 192, 216 and 240 read.
/// While none goes, the answered `qrinfo` marks stay; rows at 96 (read only by 168's own
/// confirmation, 168 kept only as a predecessor) and 48 go, and with them those marks, so a later
/// need asks again rather than reading `NotServed`. Every rotated commitment stays verified.
#[test]
fn rows_no_held_cycle_reads_go_with_the_qrinfo_marks() {
    let chain = FixtureChain::full();
    let mut engine = synced(&chain);
    let rotated = llmq(ROTATED);
    let rows = |engine: &MasternodeListEngine| -> Vec<u32> {
        (FIRST..=chain.tip).filter(|h| engine.held_row(rotated, chain.hash(*h)).is_some()).collect()
    };
    let held = rows(&engine);
    assert_eq!(held, [168, 192, 216, 240, 264]);
    let key = RequestKey::QrInfo(chain.hash(288));
    engine.mark_answered(key);
    engine.tip_changed(&chain, config()).expect("step 5");
    assert_eq!(rows(&engine), held);
    assert!(engine.is_answered(key), "no row went");
    for h in [48, 96] {
        engine.hold_row(rotated, chain.hash(h), Default::default());
    }
    let unknown = BlockHash::from([9; 32]);
    engine.hold_row(rotated, unknown, Default::default());
    engine.tip_changed(&chain, config()).expect("step 5");
    assert_eq!(rows(&engine), held);
    assert_eq!(engine.held_row(rotated, unknown), None, "a row on a block the chain lacks");
    assert!(!engine.is_answered(key), "a row went");
    assert!(engine.requests().is_empty(), "{:?}", engine.requests());
    for q in core_quorums(ROTATED) {
        let state = state(&engine, ROTATED, q.hash);
        assert!(matches!(state, QuorumState::Verified(_)), "{}: {state:?}", q.height);
    }
}

/// BL-379, D45: exactly the held cycles' rows and a kept candidate's reads stay. With every row
/// Core's, rows 96–288 held: 288 (cycle 288's own, which cycle 312 reads) and its quarters
/// 192–264 stay, the rest go. With 192 a candidate instead: its reads 168, 144 and 120 stay too,
/// 96 (its c − 4C) does not.
#[test]
fn the_rows_kept_are_the_held_cycles_and_a_candidates_reads() {
    let chain = FixtureChain::full();
    let mut engine = synced(&chain);
    let rotated = llmq(ROTATED);
    let rows = |engine: &MasternodeListEngine| -> Vec<u32> {
        (FIRST..=chain.tip).filter(|h| engine.held_row(rotated, chain.hash(*h)).is_some()).collect()
    };
    let hold = |engine: &mut MasternodeListEngine, candidate: Option<u32>| {
        for h in (96..=288).step_by(24) {
            if Some(h) == candidate {
                engine.forget_row(rotated, chain.hash(h));
                engine.hold_candidate(rotated, chain.hash(h), Default::default());
            } else {
                engine.hold_row(rotated, chain.hash(h), Default::default());
            }
        }
    };
    hold(&mut engine, None);
    engine.tip_changed(&chain, config()).expect("step 5");
    assert_eq!(rows(&engine), [192, 216, 240, 264, 288]);
    hold(&mut engine, Some(192));
    engine.tip_changed(&chain, config()).expect("step 5");
    assert_eq!(rows(&engine), [120, 144, 168, 192, 216, 240, 264, 288]);
    assert!(matches!(
        engine.held_row(rotated, chain.hash(192)),
        Some(crate::view::HeldRow::Candidates(_))
    ));
}

/// BL-382, D47: retention forgets the bookkeeping (answered, failed, dropped, retried) for a block
/// the chain does not know, and keeps it for one within tip − (R + 2·L_est), here every block.
#[test]
fn bookkeeping_for_an_unknown_block_goes() {
    let chain = FixtureChain::full();
    let mut engine = synced(&chain);
    let known = [RequestKey::MnListDiff(chain.hash(FIRST)), RequestKey::QrInfo(chain.hash(48))];
    let unknown = [
        RequestKey::MnListDiff(BlockHash::from([7; 32])),
        RequestKey::QrInfo(BlockHash::from([7; 32])),
    ];
    for (key, height) in known.into_iter().zip([FIRST, 48]) {
        engine.mark_every_bookkeeping(key, height);
    }
    for key in unknown {
        engine.mark_every_bookkeeping(key, chain.tip);
    }
    engine.tip_changed(&chain, config()).expect("step 5");
    for key in known {
        assert_eq!(engine.bookkeeping_of(key), [true; 4], "{key:?}");
    }
    for key in unknown {
        assert_eq!(engine.bookkeeping_of(key), [false; 4], "{key:?}");
    }
}

/// BL-382, D47: below the floor a block's bookkeeping goes, at it stays, and an outstanding
/// request's stays below it (its retries bound its disputes, D20). The capture's rotated
/// commitments wait on cycles 264's and 288's `qrinfo`s.
#[test]
fn bookkeeping_below_the_floor_goes_unless_outstanding() {
    let chain = FixtureChain::full();
    let mut engine = stepped(&chain);
    let outstanding = RequestKey::QrInfo(chain.hash(264));
    assert!(engine.requests().iter().any(|r| r.key() == outstanding));
    let below = RequestKey::MnListDiff(chain.hash(299));
    let at = RequestKey::MnListDiff(chain.hash(300));
    for (key, height) in [(outstanding, 264), (below, 299), (at, 300)] {
        engine.mark_every_bookkeeping(key, height);
    }
    engine.forget_bookkeeping_below(&chain, 300);
    assert_eq!(engine.bookkeeping_of(below), [false; 4]);
    assert_eq!(engine.bookkeeping_of(at), [true; 4]);
    assert_eq!(engine.bookkeeping_of(outstanding), [true; 4]);
}

/// D47: the bookkeeping floor is tip − (R + 2·L_est), or L_est below the oldest held commitment's
/// base when that is lower.
#[test]
fn the_bookkeeping_floor_follows_the_oldest_held_base() {
    use crate::engine::bookkeeping_floor;
    let cfg = config();
    let tip = 100_000;
    let by_tip = tip - cfg.r() - 2 * cfg.l_est();
    assert_eq!(bookkeeping_floor(cfg, tip, None), by_tip);
    assert_eq!(bookkeeping_floor(cfg, tip, Some(tip)), by_tip);
    assert_eq!(bookkeeping_floor(cfg, tip, Some(5_000)), 5_000 - cfg.l_est());
    assert_eq!(bookkeeping_floor(cfg, 10, Some(5)), 0);
}

/// D47: a block whose header is pruned below the chain's floor, though the chain still knows it,
/// is below the bookkeeping floor whatever that is.
#[test]
fn bookkeeping_for_a_block_below_the_header_floor_goes() {
    let mut chain = FixtureChain::full();
    let mut engine = synced(&chain);
    chain.header_floor = 200;
    let (below, above) =
        (RequestKey::MnListDiff(chain.hash(150)), RequestKey::MnListDiff(chain.hash(250)));
    engine.mark_every_bookkeeping(below, 150);
    engine.mark_every_bookkeeping(above, 250);
    engine.forget_bookkeeping_below(&chain, 0);
    assert_eq!(engine.bookkeeping_of(below), [false; 4]);
    assert_eq!(engine.bookkeeping_of(above), [true; 4]);
}

/// D45 (Opus s4 r1 m1): a cycle held only by a nonzero quorum index keeps its cycle's rows. The
/// capture's index-1 commitment of cycle 288 (base 289), alone in a set at the tip: rows 288 and
/// its quarters 264, 240, 216 stay; 192 and 168 go.
#[test]
fn a_nonzero_index_keeps_its_cycles_rows() {
    let chain = FixtureChain::full();
    let synced = synced(&chain);
    let rotated = llmq(ROTATED);
    let q = core_quorums(ROTATED).into_iter().find(|q| q.height == 289).expect("index 1");
    let (qc, _) = synced.quorum(rotated, &QuorumHash::from(q.hash)).expect("held");
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(chain.id(chain.tip), chain.tip, chain.hash(chain.tip), &[(*qc).clone()]);
    for h in (168..=288).step_by(24) {
        engine.hold_row(rotated, chain.hash(h), Default::default());
    }
    let _ = engine.tip_changed(&chain, config());
    let rows: Vec<u32> = (FIRST..=chain.tip)
        .filter(|h| engine.held_row(rotated, chain.hash(*h)).is_some())
        .collect();
    assert_eq!(rows, [216, 240, 264, 288]);
}
