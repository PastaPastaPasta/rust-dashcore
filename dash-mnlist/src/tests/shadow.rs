//! The shadow-mode surface (SB0 §5.4, D40) against Dash Core's `quorum info`.

use super::*;
use crate::MnViewError;
use crate::view::HeldRow;
use dash_evo::llmq::MemberError;

/// The capture synced with every list held, each rotated cycle's row held as it arrived: a
/// candidate, which no confirmation reaches here (no row is held for the cycles at and below
/// 144, so LQ1 computes none, D20).
fn synced_with_every_list(chain: &FixtureChain) -> MasternodeListEngine {
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.keep_every_list();
    apply_to(&mut engine, chain, FIRST, FIRST + 1..=chain.tip);
    apply_cycles(&mut engine, chain);
    engine
}

/// [`synced_with_every_list`], each rotated cycle's row then held as Core's: the rows are Core's
/// `qrinfo` replies.
fn synced_with_cores_rows(chain: &FixtureChain) -> MasternodeListEngine {
    let mut engine = synced_with_every_list(chain);
    let rotated = llmq(ROTATED);
    let mut held = 0;
    for cycle in (0..=chain.tip).step_by(24) {
        if let Some(HeldRow::Candidates(rows)) = engine.held_row(rotated, chain.hash(cycle)) {
            let [row] = rows.as_slice() else {
                panic!("one reply per cycle");
            };
            engine.hold_row(rotated, chain.hash(cycle), row.clone());
            held += 1;
        }
    }
    assert_eq!(held, 5, "cycles 168 to 264");
    engine
}

fn operator_keys(members: &[dash_evo::sml::SmlEntry]) -> Vec<Vec<u8>> {
    members.iter().map(|m| m.operator_key.to_vec()).collect()
}

/// D40: `shadow_members` are Core's members, in Core's order, for each CL and rotated quorum.
/// Over candidate rows a rotated quorum has none (Sol s3 r1 m1, D43).
#[test]
fn shadow_members_are_cores() {
    let chain = FixtureChain::full();
    let engine = synced_with_every_list(&chain);
    for q in core_quorums(ROTATED) {
        assert!(matches!(
            engine.shadow_members(llmq(ROTATED), chain.id(q.height), &chain, config()),
            Err(MemberError::View(MnViewError::RowNotHeld { .. }))
        ));
    }
    let engine = synced_with_cores_rows(&chain);
    for t in [CHAIN_LOCKS, ROTATED] {
        for q in core_quorums(t) {
            let ours = engine
                .shadow_members(llmq(t), chain.id(q.height), &chain, config())
                .expect("the members");
            assert_eq!(operator_keys(&ours), q.operator_keys, "type {t} at {}", q.height);
        }
    }
}

/// D40, Sol s3 r1 m1: a cycle whose row only an unconfirmed candidate holds gives no shadow
/// members, though step 5 reads the candidate: Core's `GetAllQuorumMembers` reads none. Cycle
/// 288's quorum reads cycle 216's row (k = 3); a candidate with its first bit flipped changes
/// step 5's members.
#[test]
fn shadow_members_read_no_candidate() {
    let chain = FixtureChain::full();
    let mut engine = synced_with_cores_rows(&chain);
    let rotated = llmq(ROTATED);
    let q = core_quorums(ROTATED).into_iter().find(|q| q.height / 24 == 12).expect("cycle 288's");
    let base = chain.id(q.height);
    let Some(HeldRow::Core(row)) = engine.held_row(rotated, chain.hash(216)).cloned() else {
        panic!("Core's row for cycle 216");
    };
    let mut flipped = row;
    flipped.active_quorum_members[0] = !flipped.active_quorum_members[0];
    engine.hold_candidates(rotated, chain.hash(216), vec![flipped]);

    let stepped = engine.members(&chain, config(), rotated, base, true).expect("step 5's members");
    assert_ne!(operator_keys(&stepped), q.operator_keys, "the candidate changes step 5's members");
    assert!(matches!(
        engine.shadow_members(rotated, base, &chain, config()),
        Err(MemberError::View(MnViewError::RowNotHeld { .. }))
    ));
}

/// D40: `shadow_list` is the list held whole at a block, with its set's states; a block whose
/// list retention dropped has none.
#[test]
fn shadow_list_is_the_held_list() {
    let chain = FixtureChain::full();
    let engine = synced(&chain);
    let tip = engine.shadow_list(chain.id(chain.tip)).expect("the tip's list");
    assert_eq!((tip.height, tip.block_hash), (chain.tip, chain.hash(chain.tip)));
    assert!(!tip.sml.is_empty());
    assert!(!tip.quorums.is_empty());
    assert!(engine.shadow_list(chain.id(FIRST)).is_none(), "pruned");
}

/// D40, Opus s3 r1 m1, DEC-412: a header read failing inside a deployment check gives the read's
/// error, never members other than Core's: `Heights` latches it as `false`.
#[test]
fn shadow_members_stop_on_a_failed_deployment_read() {
    let mut chain = FixtureChain::full();
    let engine = synced_with_cores_rows(&chain);
    let (mut wrong, mut failed) = (Vec::new(), 0);
    for q in core_quorums(CHAIN_LOCKS) {
        for h in q.height.saturating_sub(40)..q.height {
            chain.fail_header = Some(h);
            match engine.shadow_members(llmq(CHAIN_LOCKS), chain.id(q.height), &chain, config()) {
                Ok(ours) if operator_keys(&ours) != q.operator_keys => wrong.push((q.height, h)),
                Ok(_) => {}
                Err(MemberError::View(MnViewError::Storage(_))) => failed += 1,
                Err(e) => panic!("{e:?}"),
            }
        }
        chain.fail_header = None;
    }
    assert!(wrong.is_empty(), "not Core's members (quorum height, failed header): {wrong:?}");
    assert!(failed > 0, "a deployment read failed");
}
