# DW DP2-04 — Sol review, round 3

Reviewed only `ed16feb9..3c8cc5d3` (nine signed commits) in a detached worktree. Read the full delta. The implementer's worktree, branch, and GitHub were not changed.

## Findings

1. possible security note: see manager

## Closure table

| Open item | Judgment | Evidence |
|---|---|---|
| Item 1 | Partially closed; changes required | possible security note: see manager |
| F4, read deadline | Closed | `private_details.rs:565-568` checks after the last open; `:790-797` checks before apply. `private_details_tests.rs:934-985` covers both boundaries, and `:951-966` covers read-back. The m4 contract at `docs/contracts/m4-dashpay-engine.md:2126-2130` says local apply may complete after starting in time. |
| F3, publish fetch | Closed as accepted residual | `docs/contracts/m4-dashpay-engine.md:2184-2190` cites DEC-206 for the library's own publish fetch. |
| R6, digest load overtaken by write | Closed | `private_details.rs:292-321,340-348,654-666` checks the cache epoch and rereads after a write; `private_details_tests.rs:1123-1136` arranges the race. |
| R9b, recreated cache entry | Equivalent for practical runs | `private_details.rs:286-297,308-321,360-362` allocates epochs from one counter, including for a new entry, so the old load cannot match the new entry. A 64-bit counter wrap is theoretical. |

The test guard at `private_details_tests.rs:162-179` uses one static mutex; its thread-local value retains that mutex guard, so tests on distinct threads serialize after entering `fixture`. `Fixture::new` is evaluated before that call (`:379`), so fixture construction itself is outside the guard.

## Mutation table

Each mutant was applied alone from a saved exact-head file. After each focused run, the file was restored with `cp -p`, touched, and verified with `cmp`. These source edits differ from the implementer's reported mutants.

| Reviewer mutant | Focused result | Judgment |
|---|---|---|
| S-A: possible security note: see manager | 1 focused test failed at its intended assertion | Killed |
| S-B: possible security note: see manager | 1 focused test failed at its intended assertion | Killed |
| D-A: allow three extra seconds after the last open | The settle test survived because the pre-apply check also rejected it; the read-back test failed, returning `Published` | Killed by the read-back test |
| D-B: allow one extra second before apply | 1 focused test failed at its intended assertion | Killed |

## Validation and limits

- `cargo test --locked -p dw-engine -p dw-appdb -p dw-app`: passed; 1,041 passed, 0 failed, 8 ignored across 22 result blocks. The engine unit suite was 769 passed and 6 ignored; appdb 73 passed; app 115 passed. All 13 `e0_05_bringup` tests passed.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --locked -p dw-engine -p dw-appdb -p dw-app -p dwcli --all-targets -- -D warnings`: passed.
- The testnet private-details test remained ignored; no live Platform fetch was run. The detached scratch worktree used mode 0700, its own `TMPDIR` and `CARGO_TARGET_DIR`, `CARGO_BUILD_JOBS=12`, and `nice -n 10`.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
