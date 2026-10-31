// EMBARGO-1
//! The gate's replay of every L1 target's private seeds against Dash Core's oracle (CS0 §9.4).
//!
//! The seeds are read through `$DASH_EMBARGO_DIR/fuzz/MANIFEST` (`node_sink::manifest`): a
//! `required` target must be registered in `node_l1::TARGETS`; a `pending` one is replayed when
//! it is and skipped otherwise. A seed is replayed on its target's pins and counts only when one
//! of them compares it (both sides agree on it, [`node_l1::Outcome::BothAccept`] or `BothReject`,
//! or it is a declared `StrictFlag`); a seed each of its target's pins leaves out of its domain
//! (`OutOfDomain`) fails the replay. The output is one aggregate line and a receipt; it names no
//! target and no input.
//!
//! The test always compiles. Without the `oracle` feature there is nothing to replay against, so
//! it skips when the variable is unset and **fails** when it is set: a gate that sets the variable
//! but forgets `--features oracle` cannot pass without the replay.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::use_debug,
    clippy::dbg_macro,
    clippy::print_stdout
)]

use node_sink::{private_tree, report};

const TEST: &str = "private_seed_replay";

#[cfg(not(feature = "oracle"))]
#[test]
fn private_seed_replay() {
    let Some(_root) = private_tree(TEST) else {
        return;
    };
    report(&format!("{TEST}: FAIL"));
    panic!("{TEST}: the replay needs --features oracle");
}

#[cfg(feature = "oracle")]
#[test]
fn private_seed_replay() {
    let Some(root) = private_tree(TEST) else {
        return;
    };
    match replay::replay(&root.join("fuzz")) {
        Ok(done) => {
            let receipt = node_sink::write_receipt(
                &root,
                TEST,
                &[
                    ("executed", done.executed),
                    ("applicable", done.applicable),
                    ("required", done.required),
                ],
            );
            report(&format!("{TEST}: PASS {} seeds", done.executed));
            assert!(receipt.is_ok(), "{TEST}: the receipt could not be written");
        }
        Err(message) => {
            report(&format!("{TEST}: FAIL"));
            panic!("{TEST}: {message}");
        }
    }
}

#[cfg(feature = "oracle")]
mod replay {
    use std::path::{Path, PathBuf};

    use node_l1::Outcome;
    use node_l1::check_inputs::{HEADER_INPUT_LEN, Network, Position};
    use node_sink::manifest::testing::write_seed_corpus;
    use node_sink::manifest::{SeedState, SeedTarget, load_seeds};

    /// How a replay went: the seeds compared (on at least one of their target's pins, with no pin
    /// diverging), the seeds applicable to the tree under test (those of registered targets,
    /// counted before anything runs), and the `required` seeds the manifest lists. Every required
    /// target is registered and every applicable seed is compared, or the replay fails, so
    /// `executed == applicable >= required`; the gate checks `required` and `applicable` against
    /// the manifest.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct Replayed {
        pub executed: u64,
        pub applicable: u64,
        pub required: u64,
    }

    /// Replays the seeds under `fuzz_dir` on each target's pins. The error is a fixed, aggregate
    /// message.
    pub fn replay(fuzz_dir: &Path) -> Result<Replayed, String> {
        let seeds = load_seeds(fuzz_dir).map_err(|e| e.to_string())?;
        let count = |keep: &dyn Fn(&SeedTarget) -> bool| -> u64 {
            seeds.targets.iter().filter(|t| keep(t)).map(|t| t.seeds.len() as u64).sum()
        };
        let required = count(&|t| t.state == SeedState::Required);
        let applicable = count(&|t| node_l1::target(&t.name).is_some());
        let (mut executed, mut divergent, mut uncompared) = (0u64, 0u64, 0u64);
        for target in &seeds.targets {
            let Some(registered) = node_l1::target(&target.name) else {
                if target.state == SeedState::Required {
                    return Err("a required target is not in the tree under test".into());
                }
                continue;
            };
            for seed in &target.seeds {
                let (mut diverged, mut compared) = (0u64, false);
                for &pin in registered.pins {
                    match (registered.compare)(seed, pin) {
                        Ok(Outcome::BothAccept | Outcome::BothReject | Outcome::StrictFlag) => {
                            compared = true
                        }
                        // A discarded seed (`diff_chain`'s tie, D30) compared nothing.
                        Ok(Outcome::OutOfDomain | Outcome::Discarded) => {}
                        Err(_) => diverged += 1,
                    }
                }
                divergent += diverged;
                // A seed no pin compares exercises nothing: it gets no credit, and fails the
                // replay below whatever its target's state.
                if compared {
                    executed += 1;
                } else {
                    uncompared += 1;
                }
            }
        }
        if divergent > 0 {
            return Err(format!("{divergent} seed comparisons diverge"));
        }
        if uncompared > 0 {
            return Err(format!("seeds outside every pin's domain: {uncompared}"));
        }
        if executed != applicable {
            return Err("not every applicable seed was replayed".into());
        }
        Ok(Replayed {
            executed,
            applicable,
            required,
        })
    }

    // Self-tests of the replay's checks on synthetic seed trees (no private data).

    fn synthetic(name: &str, manifest: &str, seeds: &[(&str, &[u8])]) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("private-seed-replay").join(name);
        write_seed_corpus(&dir, manifest, seeds);
        dir
    }

    /// 80 zero bytes: a header both sides decode alike.
    const HEADER: &[u8] = &[0; 80];

    #[test]
    fn a_required_registered_target_replays() {
        let dir = synthetic(
            "required",
            "min 1\ntarget diff_header_decode required\nseed diff_header_decode/seeds/a\n",
            &[("diff_header_decode/seeds/a", HEADER)],
        );
        assert!(
            replay(&dir)
                == Ok(Replayed {
                    executed: 1,
                    applicable: 1,
                    required: 1
                })
        );
    }

    #[test]
    fn pending_targets_replay_only_when_registered() {
        let dir = synthetic(
            "pending",
            "min 1\ntarget diff_header_decode required\nseed diff_header_decode/seeds/a\ntarget not_in_this_tree pending\nseed not_in_this_tree/seeds/b\ntarget diff_tx_decode pending\nseed diff_tx_decode/seeds/c\n",
            &[
                ("diff_header_decode/seeds/a", HEADER),
                ("not_in_this_tree/seeds/b", b"x"),
                ("diff_tx_decode/seeds/c", b"\x00"),
            ],
        );
        assert!(
            replay(&dir)
                == Ok(Replayed {
                    executed: 2,
                    applicable: 2,
                    required: 1
                })
        );
    }

    #[test]
    fn a_missing_required_target_or_an_empty_manifest_fails() {
        let missing = synthetic(
            "missing",
            "min 1\ntarget not_in_this_tree required\nseed not_in_this_tree/seeds/b\n",
            &[("not_in_this_tree/seeds/b", b"x")],
        );
        assert!(replay(&missing).is_err());
        let empty = synthetic("empty", "min 0\n", &[]);
        assert!(replay(&empty).is_err());
        let none = synthetic("none", "min 1\n", &[]);
        assert!(replay(&none).is_err());
    }

    /// A version-1 transaction with one input and one output, which `CheckTransaction` accepts.
    const PLAIN_TX: &[u8] = &[
        1, 0, 0, 0, // version 1, type 0
        1, // one input
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 1, // prevout hash
        0, 0, 0, 0, // prevout index
        1, 0x51, // scriptSig
        0xff, 0xff, 0xff, 0xff, // sequence
        1,    // one output
        1, 0, 0, 0, 0, 0, 0, 0, // value
        1, 0x51, // scriptPubKey
        0, 0, 0, 0, // lock time
    ];

    fn uncompared(n: u64) -> Result<Replayed, String> {
        Err(format!("seeds outside every pin's domain: {n}"))
    }

    /// The CS3 header-check seeds that compare nothing: one too short for the input format, and
    /// one whose eleven-header window is too short for DGW, which the oracle refuses
    /// (`ORACLE_E_CONTEXT`).
    fn uncompared_header_seeds() -> [Vec<u8>; 2] {
        let mut short_window = Position::encode_height(Network::Main, 68_590, 17);
        short_window.resize(HEADER_INPUT_LEN, 0);
        short_window[13] = 3; // version 4
        short_window[14..18].copy_from_slice(&150i32.to_le_bytes());
        [b"x".to_vec(), short_window]
    }

    const HEADER_TARGET: &str = "diff_contextual_check_block_header";

    /// A manifest with one target and `seeds` seeds named `0`, `1`, ...
    fn one_target(name: &str, target: &str, state: &str, seeds: &[&[u8]]) -> PathBuf {
        let paths: Vec<String> = (0..seeds.len()).map(|i| format!("{target}/seeds/{i}")).collect();
        let required = if state == "required" {
            seeds.len()
        } else {
            0
        };
        let mut manifest = format!("min {required}\ntarget {target} {state}\n");
        for path in &paths {
            manifest += &format!("seed {path}\n");
        }
        let files: Vec<(&str, &[u8])> =
            paths.iter().map(String::as_str).zip(seeds.iter().copied()).collect();
        synthetic(name, &manifest, &files)
    }

    /// A CS3 target compares on v24 alone (its v23.1 comparison is out of domain); a decode
    /// rejection both sides share counts.
    #[test]
    fn a_v24_only_seed_counts() {
        let dir = one_target("v24-only", "diff_check_transaction", "required", &[PLAIN_TX, b"x"]);
        assert!(
            replay(&dir)
                == Ok(Replayed {
                    executed: 2,
                    applicable: 2,
                    required: 2
                })
        );
    }

    /// A required seed outside every pin's domain fails the replay, alone or beside a seed that
    /// compares: the credit is per seed.
    #[test]
    fn a_required_seed_no_pin_compares_fails() {
        let [short_input, short_window] = uncompared_header_seeds();
        for (name, seed) in [("format", &short_input), ("context", &short_window)] {
            let alone =
                one_target(&format!("uncompared-{name}"), HEADER_TARGET, "required", &[seed]);
            assert!(replay(&alone) == uncompared(1), "{name}");
        }
        let mixed = synthetic(
            "uncompared-mixed",
            &format!(
                "min 2\ntarget diff_check_transaction required\nseed diff_check_transaction/seeds/a\ntarget {HEADER_TARGET} required\nseed {HEADER_TARGET}/seeds/b\n"
            ),
            &[
                ("diff_check_transaction/seeds/a", PLAIN_TX),
                (&format!("{HEADER_TARGET}/seeds/b"), &short_window),
            ],
        );
        assert!(replay(&mixed) == uncompared(1));
    }

    /// So does a pending one: its target is registered, so it is replayed, and it becomes
    /// required at the merge.
    #[test]
    fn a_pending_seed_no_pin_compares_fails() {
        let [_, short_window] = uncompared_header_seeds();
        let dir = synthetic(
            "uncompared-pending",
            &format!(
                "min 1\ntarget diff_check_transaction required\nseed diff_check_transaction/seeds/a\ntarget {HEADER_TARGET} pending\nseed {HEADER_TARGET}/seeds/b\n"
            ),
            &[
                ("diff_check_transaction/seeds/a", PLAIN_TX),
                (&format!("{HEADER_TARGET}/seeds/b"), &short_window),
            ],
        );
        assert!(replay(&dir) == uncompared(1));
    }
}
