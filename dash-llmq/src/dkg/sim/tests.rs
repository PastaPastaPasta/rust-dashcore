//! `ShouldSimulateError` and `GetRandBool` (LQ4 §2.9; DEC-623 D-3).

use super::*;
use crate::dkg::bls::{BlsSkValue, IesSeed};
use crate::dkg::rng::{CoreRng, KeyDraw, RangeDraw, SeededRng};

/// Core's comparison as `quorumdkgsimerror` sets it up: the RPC stores
/// `static_cast<double>(rate) / 100` (`rpc/quorums.cpp:894`), and `GetRandBool` compares
/// `r <= v * rate` with `v = 100000000` (`random.cpp:653-661`).
#[allow(
    clippy::float_arithmetic,
    clippy::cast_precision_loss,
    reason = "the double expression the integer-percent decision must equal"
)]
fn core_get_rand_bool(r: u64, rpc_rate: i32) -> bool {
    let rate = f64::from(rpc_rate) / 100.0;
    if rate == 0.0 {
        return false;
    }
    let v: u64 = 100_000_000;
    (r as f64) <= v as f64 * rate
}

/// Core's `FastRandomContext::randrange` stand-in `CoreRng` uses, replayed over a clone of its
/// stream so the test sees every draw `r`.
fn draws(seed: u64, count: usize) -> Vec<u64> {
    use rand_core::Rng;
    let mut rng = SeededRng::new(seed);
    let n = RAND_BOOL_RANGE;
    let zone = u64::MAX - u64::MAX % n;
    (0..count)
        .map(|_| {
            loop {
                let v = rng.next_u64();
                if v < zone {
                    break v % n;
                }
            }
        })
        .collect()
}

/// D-3 rows: at 0, 1, 50, 99 and 100 percent the integer-percent decision equals Core's double
/// comparison for every draw the RNG makes, and for the draws at the threshold's edges.
#[test]
fn integer_percent_decides_as_cores_double_for_every_draw() {
    for percent in [0u8, 1, 50, 99, 100] {
        let rpc_rate = i32::from(percent);
        let mut rates = SimErrorRates::default();
        rates.set(SimError::ContributionLie, rpc_rate).expect("in range");
        let mut rng = CoreRng(SeededRng::new(u64::from(percent) + 1));
        let decisions: Vec<bool> = (0..5_000)
            .map(|_| {
                should_simulate(LlmqType::LlmqTest, SimError::ContributionLie, &rates, &mut rng)
            })
            .collect();
        if percent == 0 {
            // No draw at rate 0: the RNG's stream is untouched.
            use rand_core::Rng;
            assert_eq!(rng.0.next_u64(), SeededRng::new(1).next_u64());
            assert!(decisions.iter().all(|d| !d));
            continue;
        }
        let rs = draws(u64::from(percent) + 1, 5_000);
        for (r, decision) in rs.iter().zip(&decisions) {
            assert_eq!(*decision, core_get_rand_bool(*r, rpc_rate), "{percent}% at r = {r}");
        }
        let edge = u64::from(percent) * 1_000_000;
        for r in [0, 1, edge.saturating_sub(1), edge, edge + 1, 100_000_000] {
            if r < RAND_BOOL_RANGE {
                assert_eq!(
                    get_rand_bool(r, percent),
                    core_get_rand_bool(r, rpc_rate),
                    "{percent}% at r = {r}"
                );
            }
        }
        if percent == 100 {
            assert!(decisions.iter().all(|d| *d), "rate 1.0 draws but always fires");
        }
    }
}

/// Every rate and every edge draw; the double falls below `percent · 10^6` at 29, 57 and 58,
/// where the exact rational would say true.
#[test]
fn every_percent_rounds_as_cores_double() {
    for percent in 0..=100u8 {
        let edge = u64::from(percent) * 1_000_000;
        for r in [edge.saturating_sub(1), edge, edge + 1] {
            assert_eq!(get_rand_bool(r, percent), core_get_rand_bool(r, i32::from(percent)));
        }
    }
    for percent in [29u8, 57, 58] {
        let edge = u64::from(percent) * 1_000_000;
        assert!(!get_rand_bool(edge, percent));
        assert!(get_rand_bool(edge - 1, percent));
    }
    assert!(get_rand_bool(7_000_000, 7));
    assert!(!get_rand_bool(7_000_001, 7));
}

/// A [`DkgRng`] that counts its draws.
#[derive(Default)]
struct Counting(usize);

impl DkgRng for Counting {
    fn new_key(&mut self, _: KeyDraw) -> BlsSkValue {
        self.0 += 1;
        BlsSkValue::invalid()
    }
    fn ies_seed(&mut self, _: usize) -> IesSeed {
        self.0 += 1;
        IesSeed {
            ephemeral_sk: BlsSkValue::invalid(),
            iv_seed: [0; 32],
        }
    }
    fn rand_bool(&mut self, _: u8, _: SimError) -> bool {
        self.0 += 1;
        true
    }
    fn rand_range(&mut self, _: u64, _: RangeDraw) -> u64 {
        self.0 += 1;
        0
    }
}

#[test]
fn only_llmq_test_draws_and_rate_zero_never_does() {
    let mut rates = SimErrorRates::default();
    for kind in SimError::ALL {
        rates.set(kind, 100).expect("in range");
    }
    let mut rng = Counting::default();
    for t in [LlmqType::Llmq50_60, LlmqType::LlmqTestDip0024, LlmqType::LlmqDevnet] {
        assert!(!should_simulate(t, SimError::ContributionOmit, &rates, &mut rng));
    }
    assert_eq!(rng.0, 0);
    assert!(should_simulate(LlmqType::LlmqTest, SimError::ContributionOmit, &rates, &mut rng));
    assert_eq!(rng.0, 1);
    rates.set(SimError::ContributionOmit, 0).expect("in range");
    assert!(!should_simulate(LlmqType::LlmqTest, SimError::ContributionOmit, &rates, &mut rng));
    assert_eq!(rng.0, 1);
}

#[test]
fn the_rpc_refuses_rates_outside_zero_to_one_hundred() {
    let mut rates = SimErrorRates::default();
    assert_eq!(rates.set(SimError::CommitLie, -1), Err(InvalidRate(-1)));
    assert_eq!(rates.set(SimError::CommitLie, 101), Err(InvalidRate(101)));
    assert_eq!(rates.percent(SimError::CommitLie), 0);
    assert_eq!(InvalidRate(101).to_string(), "invalid rate. Must be between 0 and 100");
    for kind in SimError::ALL {
        assert_eq!(SimError::from_name(kind.name()), Some(kind));
    }
    assert_eq!(SimError::from_name("lie"), None);
}
