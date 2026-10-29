//! The coins-cache differential's smoke run (O2a-CO, CH2c): CH2's cache and overlay against Core's
//! coins views (`oracle_coins_ops`, v24.0) on the hand-written seeds and on pseudo-random inputs in
//! both modes, with no divergence. The by-design differences (`node_l1::coins_ops::Allowed`) end a
//! run or skip an answer and are counted; each must be reached by its seed.

use dash_oracle_sys::Pin;
use node_l1::coins_ops::{
    CoinsOpsRun, Mode, compare_coins_ops, compare_coins_script, script_from_bytes,
};

#[path = "support/coins_ops_seeds.rs"]
mod coins_ops_seeds;

use coins_ops_seeds::{Expect, SEEDS};

fn check(name: &str, run: Result<CoinsOpsRun, node_l1::Divergence>) -> CoinsOpsRun {
    match run {
        Ok(run) => run,
        Err(d) => panic!("{name}: diverged in {d}"),
    }
}

#[test]
fn seeds_agree() {
    for seed in SEEDS {
        let ops = script_from_bytes(seed.mode, seed.input);
        let r = check(seed.name, compare_coins_script(seed.mode, &ops, Pin::V24_0));
        let name = seed.name;
        // Core's usage after every op, but one that threw.
        assert!(r.usage_checks + 1 >= r.ops, "{name}: {r:?}");
        match seed.expect {
            Expect::BothLogicError => assert!(r.both_logic_error, "{name}"),
            Expect::Ended(by) => assert_eq!(r.ended_by, Some(by), "{name}"),
            Expect::Skips => assert!(r.skipped > 0, "{name}"),
            Expect::Full => {
                assert_eq!(r.ops, ops.len(), "{name} ended early");
                assert!(!r.both_logic_error && r.ended_by.is_none(), "{name}");
                assert!(r.flushes > 0, "{name} never flushed");
            }
        }
    }
}

/// xorshift64: inputs without a dependency, the same on every run.
fn inputs(n: usize) -> impl Iterator<Item = Vec<u8>> {
    let mut s = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    (0..n).map(move |_| {
        let len = (next() % 160) as usize;
        (0..len).map(|_| next() as u8).collect()
    })
}

#[test]
fn random_inputs_agree() {
    for mode in [Mode::Aligned, Mode::Hidden] {
        let mut total = CoinsOpsRun::default();
        let (mut ended, mut allowed) = (0, 0);
        for (i, mut input) in inputs(600).enumerate() {
            input.insert(0, mode.byte());
            let r = check(&format!("{mode:?} input {i}"), compare_coins_ops(&input, Pin::V24_0));
            total.ops += r.ops;
            total.flushes += r.flushes;
            total.commits += r.commits;
            total.skipped += r.skipped;
            total.uncompared += r.uncompared;
            total.usage_checks += r.usage_checks;
            total.usage_ambiguous += r.usage_ambiguous;
            total.freezes += r.freezes;
            total.w_steps += r.w_steps;
            ended += usize::from(r.both_logic_error);
            allowed += usize::from(r.ended_by.is_some());
        }
        // The inputs reach every kind of op, and Core's usage is checked after nearly every one.
        assert!(total.flushes > 100 && total.commits > 100, "{mode:?} {total:?}");
        assert!(ended > 0 && allowed > 0, "{mode:?} {ended} {allowed}");
        assert!(total.usage_checks + 1200 > total.ops, "{mode:?} {total:?}");
        match mode {
            Mode::Aligned => assert_eq!(total.freezes + total.uncompared, 0, "{total:?}"),
            Mode::Hidden => assert!(total.freezes > 100 && total.w_steps > 20, "{total:?}"),
        }
    }
}
