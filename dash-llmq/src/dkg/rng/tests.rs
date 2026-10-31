//! The production RNG's draws and the replay's tag checks.

use super::*;

#[test]
fn core_rng_draws_valid_keys_and_values_in_range() {
    let mut rng = CoreRng(SeededRng::new(9));
    for j in 0..50 {
        assert!(rng.new_key(KeyDraw::Coefficient(j)).is_valid());
    }
    assert!(rng.ies_seed(3).ephemeral_sk.is_valid());
    let mut seen = [false; 5];
    for _ in 0..200 {
        let v = rng.rand_range(5, RangeDraw::CommitLieType);
        assert!(v < 5);
        seen[v as usize] = true;
    }
    assert!(seen.iter().all(|s| *s));
}

fn key(n: u8) -> BlsSkValue {
    make_new_key(&mut SeededRng::new(u64::from(n)))
}

#[test]
fn a_replay_returns_its_draws_in_order() {
    let mut replay = ReplayRng::new([
        Draw::Key(KeyDraw::Coefficient(0), key(1)),
        Draw::Bool(SimError::ContributionOmit, 100, false),
        Draw::Ies(
            3,
            IesSeed {
                ephemeral_sk: key(2),
                iv_seed: [4; IV_SEED_LEN],
            },
        ),
        Draw::Range(RangeDraw::CommitLieType, 3),
    ]);
    assert_eq!(replay.new_key(KeyDraw::Coefficient(0)), key(1));
    assert!(!replay.rand_bool(100, SimError::ContributionOmit));
    assert_eq!(replay.ies_seed(3).iv_seed, [4; IV_SEED_LEN]);
    assert_eq!(replay.rand_range(5, RangeDraw::CommitLieType), 3);
    assert_eq!(replay.finish(), Ok(()));
}

#[test]
fn a_replay_fails_at_the_first_wrong_tag() {
    let mut replay = ReplayRng::new([
        Draw::Key(KeyDraw::Coefficient(0), key(1)),
        Draw::Bool(SimError::ContributionOmit, 100, true),
    ]);
    let _ = replay.new_key(KeyDraw::Coefficient(0));
    // The session asks for a lie before the omit: Core's order is the other one.
    assert!(!replay.rand_bool(100, SimError::ContributionLie));
    let _ = replay.rand_bool(100, SimError::ContributionOmit);
    let err = replay.finish().expect_err("a mismatch");
    assert_eq!(err.at, 1);
    assert!(err.asked.contains("ContributionLie"));
    assert!(err.recorded.expect("a recorded draw").contains("ContributionOmit"));
}

#[test]
fn a_replay_fails_past_its_end_and_with_draws_left() {
    let mut short = ReplayRng::new([]);
    assert!(!short.new_key(KeyDraw::SingleMemberSk1).is_valid());
    assert_eq!(short.finish().expect_err("past the end").recorded, None);

    let long = ReplayRng::new([Draw::Range(RangeDraw::CommitLieType, 1)]);
    assert!(long.finish().is_err());
}

#[test]
fn a_replay_fails_on_another_recipient_count_or_rate() {
    let seed = || IesSeed {
        ephemeral_sk: key(2),
        iv_seed: [4; IV_SEED_LEN],
    };
    let mut ies = ReplayRng::new([Draw::Ies(33, seed())]);
    let _ = ies.ies_seed(32);
    let err = ies.finish().expect_err("another recipient count");
    assert_eq!((err.at, err.asked.as_str()), (0, "Ies(32)"));

    let mut rate = ReplayRng::new([Draw::Bool(SimError::ContributionLie, 100, true)]);
    assert!(!rate.rand_bool(50, SimError::ContributionLie));
    let err = rate.finish().expect_err("another rate");
    assert_eq!((err.at, err.asked.as_str()), (0, "Bool(ContributionLie, 50%)"));
}
