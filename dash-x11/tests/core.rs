//! Differential tests against Dash Core's own X11 code.
//!
//! `core_fixture` always runs: `tests/data/core-fixture.json` holds the SHA-256 of Core's digest
//! streams (`contrib/core-fixture.sh`, identical for v23.1.8 and v24.0.0-rc.3) for 2000 inputs
//! per stream (lengths `i % 201`, `201 + i % 900` and 80 for X11; 64 bytes per chain stage after
//! BLAKE), which this crate regenerates and hashes under every implementation.
//!
//! `core_live_*` run when `DASH_X11_CORE_HARNESS` names harness commands built by
//! `contrib/core-harness/build.sh` (`;`-separated, for example one per Core pin). Inputs come
//! from the generator both sides implement; every implementation this CPU runs must give Core's
//! digests. `DASH_X11_CORE_CASES` sets the number of cases per stream (default 20000),
//! `DASH_X11_CORE_SEED` the generator's seed (default `SEED`), `DASH_X11_STAGE` limits
//! `core_live_stages` to one stage and `DASH_X11_KIND` limits `core_live_x11` to one input kind
//! (`random`, every length 0..=200; `long`, 201..=1100; `header`, 80 bytes). A harness command
//! `env DASH_X11_CORE_SOFT=1 <harness>` makes Core use its software ECHO and SHAvite-3.

mod common;

use bitcoin_hashes::{Hash as _, HashEngine as _, sha256};
use common::{Rng, env_u64, harnesses, hex, implementations, kind_len, run};
use dash_x11::{Implementation, Stage};

const SEED: u64 = 0x5831_3131;

/// The harness input kind of a stage: BLAKE hashes X11 inputs, the other stages 64 bytes.
fn stage_kind(stage: Stage) -> &'static str {
    if stage == Stage::Blake {
        "random"
    } else {
        "64"
    }
}

/// The SHA-256, in hex, of `f`'s outputs over the first `n` generated inputs of `kind`.
fn stream_digest<const N: usize>(
    kind: &str,
    seed: u64,
    n: u64,
    f: impl Fn(&[u8]) -> [u8; N],
) -> String {
    let mut rng = Rng(seed);
    let mut input = Vec::new();
    let mut stream = sha256::HashEngine::default();
    for i in 0..n {
        rng.fill(&mut input, kind_len(kind, i));
        stream.input(&f(&input));
    }
    hex(sha256::Hash::from_engine(stream).as_byte_array())
}

#[test]
fn core_fixture() {
    let json: serde_json::Value =
        serde_json::from_str(include_str!("data/core-fixture.json")).unwrap();
    let seed = json["seed"].as_u64().unwrap();
    let n = json["cases"].as_u64().unwrap();
    assert_eq!((seed, n), (SEED, 2000));
    for imp in implementations() {
        for kind in ["random", "long", "header"] {
            let got = stream_digest(kind, seed, n, |input| imp.x11(input));
            assert_eq!(got, json["x11"][kind], "x11 {kind}, {}", imp.name());
        }
        for stage in Stage::ALL {
            let got = stream_digest(stage_kind(stage), seed, n, |input| imp.stage(stage, input));
            assert_eq!(got, json["stages"][stage.name()], "{}, {}", stage.name(), imp.name());
        }
    }
}

/// Streams `n` generated inputs of `kind` (seed `seed`) through `harness <args> <seed> <n>`
/// and checks that `f` gives Core's `N`-byte output for each, under every implementation.
/// `label` names the stream in messages; `contrib/differential.sh` parses the summary line.
fn check_live<const N: usize>(
    harness: &[String],
    args: [&str; 2],
    label: &str,
    kind: &str,
    f: impl Fn(&Implementation, &[u8]) -> [u8; N],
) {
    let n = env_u64("DASH_X11_CORE_CASES", 20_000);
    let seed = env_u64("DASH_X11_CORE_SEED", SEED);
    let core = run(harness, &[args[0], args[1], &seed.to_string(), &n.to_string()]);
    assert_eq!(core.len() as u64, N as u64 * n);
    for imp in implementations() {
        let mut rng = Rng(seed);
        let mut input = Vec::new();
        for (i, want) in core.as_chunks::<N>().0.iter().enumerate() {
            rng.fill(&mut input, kind_len(kind, i as u64));
            let got = f(imp, &input);
            if got[..] != want[..] {
                panic!(
                    "{label} case {i} ({}): got {}, Core {}, input {}, {harness:?}",
                    imp.name(),
                    hex(&got),
                    hex(want),
                    hex(&input)
                );
            }
        }
    }
    eprintln!("{label}: {n} cases (seed {seed}) match {harness:?}");
}

#[test]
fn core_live_stages() {
    let only = std::env::var("DASH_X11_STAGE").ok();
    for harness in harnesses() {
        for stage in Stage::ALL {
            if only.as_deref().is_some_and(|s| s != stage.name()) {
                continue;
            }
            check_live(
                &harness,
                ["stage", stage.name()],
                stage.name(),
                stage_kind(stage),
                |imp, input| imp.stage(stage, input),
            );
        }
    }
}

#[test]
fn core_live_x11() {
    let only = std::env::var("DASH_X11_KIND").ok();
    for harness in harnesses() {
        for kind in ["random", "long", "header"] {
            if only.as_deref().is_some_and(|k| k != kind) {
                continue;
            }
            check_live(&harness, ["stream", kind], &format!("x11 {kind}"), kind, |imp, input| {
                imp.x11(input)
            });
        }
    }
}
