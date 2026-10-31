# CH2b commit 6, round 3 — Sol independent review

Target `f42434a53d7855bd982da961c311093af49553b5` on `node/ch2b-crash` is one commit on `b86a69573aef143592a27b689ea1f3b2d4aaf59b` and reports signature `G`. Reviewed in detached, disposable worktrees; the diff is confined to the six stated Rust files and five new repros. This round addresses the round-2 refusal-only gap and the stated narrow audit. No private material was read.

## Round-2 Medium closure

**Closed.** On `INVALID_TIP`, `ChainSut::open` first requires `v.found.may_refuse()`, then constructs `ChainRef::checks_view(cfg)` and calls `sut.judge(world, &refm, &world.trace(), false)` in `world.checking` (`dashd-chainstate/src/crash/chain.rs:1003-1026`). `check()` calls the same judge with `opened = true` (`:1075-1081`). On the exact conditional-coin witness, the `c5;f;x3` pre-index window is exhaustively clean with `RefuseCoin` (31 images, 240 branches, zero truncated); the unplanted `c5;f;x3;i` window is clean (6 images, 13 branches); the planted refusal gives R0 explicitly naming R3's recovered-coin mismatch. `LostCoin` detects the same deletion on an ordinary restart as R3. The original round-2 false negative is gone.

## Audit

- `judge` invokes `check_tip`, `check_stores`, `check_flushed_were_connected`, `check_replayed`, `check_activation`, `effects`, and `check_replay_crash_came_up` (`chain.rs:1133-1147`), exactly the prior successful-open check list. The `opened` switch only withholds `check_outcome` (`:1380-1417`): R6(e)'s successful-start condition, best-valid height, and final-tip-failed condition. `may_refuse` still judges the refusal before that switch.
- R1/R5 run through `check_stores` (`:1213-1295`); R3/R4 and both recovered R2 clauses run through `check_tip` (`:1151-1201`). The harness still calls booted `store_check` before `open` (`dashd-store/src/faulty/harness.rs:941`), and the recovered marker clauses make a second post-recovery `store_check` redundant. R6(a) runs through the flush trace check. R6(b) runs but has no booted marker on reachable warranted refusals. R6(c)'s invalid-marked-valid half runs; its activation-connect half has no connect after this refusal. R13 runs through `effects`; C9 runs through its trace checker and has no recovery append to count. The no-marker/no-append premise and x3/x4/x5 trace evidence were established in round 2; the current `judge` includes these checks.
- All new planted damage is in the one post-recovery, pre-activation `plant_damage(&s, rec.found.must_refuse())` hook (`chain.rs:838-874, :958-962`), with Durable writes only when it changes a record. The refusal tests exhaustively check their unplanted windows. The checked-in replay test iterates all five `-refused` repros (`crash_planted.rs:502-515`); the prior repro files are unchanged. D-c6-5, D-c6-9 and D-c6-11 were updated.

## `checks_view` residual

The checks query `block`, `height`, `blocks.path`, `is_ancestor`, `coins_at`, `evo_at`, `valid_path`, and `cfg.presumed`; none iterate the global map. Synthetic block headers commit to the parent hash, a Merkle root containing the height/branch coinbase, and the invalid block's extra transaction (`synth.rs:137-221, :248-271`). Without a cryptographic collision, the same hash cannot have different height, branch, parent, or validity across configurations. `coins_at` folds those deterministic block records; `evo_at` folds the path with the *current* `cfg.presumed` (`reference.rs:168-194`). Thus blocks of this run cannot acquire wrong-run coin/evo answers.

The residual is real but outside a reachable image: a temporary cross-configuration test generated a valid height-3 block under one config, then showed that a reference with `invalid=3` does not know its hash while `checks_view` does. If an alien index record were injected, R1's unknown-block clause could therefore pass where a run-local reference would fail. Unlike the implementer's blanket “r1 was no stronger” claim, V's regeneration with the current config could reject this particular alien block. However, the modeled worlds never import another run's index/file records; every stored hash is made in this run, and the fault model cannot choose a foreign hash. This is a bounded membership caveat, not a reachable false negative or a change request in this round.

## New findings

None on reachable modeled behavior. The global-map membership caveat above should remain documented if the model later permits cross-run persisted records.

## Mutation and probe table

Each source mutation ran alone from saved copies. The sources were restored after each; `cmp` and `git diff --exit-code` confirmed exact restoration.

| Probe | Result |
|---|---|
| Skip `check_tip` only on the refusal path | **Killed:** `a_coin_lost_only_under_a_refusal` lost its expected R0/R3; the pre-index control stayed clean. |
| Skip `check_stores` only on the refusal path | **Killed:** `t31_under_a_warranted_refusal` lost its R0/R5; later R5/R12 did not substitute for the refused restart check. |
| Under `RefuseCoin`, delete `mn_diff/state` only when `must_refuse()` instead of deleting a coin | **Caught:** pre-index window clean (31 images, 240 branches); refused replay gave R0 naming R3, “evo state differs from the reference at the recovered tip.” This probes a different store from the round-2 coin witness and from the checked-in `RefuseEvo` metadata probe. |
| Generate a foreign valid block under another config, then inspect `checks_view` under `invalid=3` | Confirmed extra map membership; current-run `coins_at` and `evo_at` agreed with the run-local reference. |

## Validation

`cargo test -p dashd-chainstate --features alloc-tag,crash,planted-bugs` passed: 193 library tests, `crash` 29/29, `crash_planted` 39/39 (one ignored generator), the feature-scope and other integration tests, and the doc test. Separately, default-feature `cargo test -p dashd-chainstate --lib` passed 177/177. The checked-in repro replay test and conditional coin control are among the passing planted tests. A warmed rerun of the three CRASH gate binaries used 21.58 + 12.88 + 0.85 = **35.31 user CPU-s**, below the ≤60 user CPU-s D-c6-11 budget; the first run included recompilation and was excluded from that comparison. `cargo fmt --all -- --check` and clippy with `-D warnings` on default, `crash`, and `alloc-tag,crash,planted-bugs` all passed. No full workspace, physical power-cut, or Core-side test was run.

**Verdict:** APPROVE
Sol gate satisfied: yes
