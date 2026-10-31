# DW DP2-04 — Sol review, round 4

Reviewed only `3c8cc5d3..4b209278` (six signed commits) in a detached scratch worktree. The implementer's worktree, branch, and GitHub were untouched.

## Findings

1. possible security note: see manager

## Closure table

| Item | Judgment | Evidence |
|---|---|---|
| Item 1 | Partially closed; changes required | possible security note: see manager |
| F4, read deadline | Closed | The earlier ruling remains intact in this delta; its focused cases passed in the engine suite. |
| F3, publish fetch | Closed as accepted residual | The earlier contract ruling is unchanged. |
| R6, digest load overtaken by write | Closed | The cache epoch behavior remains intact after the write moves into the job; its regression test passed. |
| R9b, recreated cache entry | Closed | Epoch allocation for new entries is unchanged. |
| Session close ordering | Closed for this delta | The close tests passed; no new lock-order cycle was found. |
| Fixture ordering | Closed | `fixture(make)` takes `one_test_at_a_time()` before calling `make`; all direct fixture constructors use it. |

## Mutation table

Each reviewer mutant was applied alone to the exact-head source. After each focused run, the file was restored with `cp -p`, touched, and verified with `cmp`; the final worktree was clean. These two mutations differ from the implementer's reported mutants.

| Reviewer mutant | Focused result | Judgment |
|---|---|---|
| M1: possible security note: see manager | Record-path cancellation test failed at its intended fence assertion | Killed |
| M2: possible security note: see manager | Settle-path cancellation test failed at its intended fence assertion | Killed |

## Validation and limits

- `cargo test --locked -p dw-engine -p dw-appdb -p dw-app`: one `e0_05_bringup` wall-clock failure under load; all other reached binaries passed. The engine unit suite passed 772 tests with 6 ignored; appdb passed 73; app passed 115. The failing test passed alone (1 passed, 0 failed). The ten integration binaries not reached after Cargo stopped all passed in a follow-on run (53 tests); doctests passed (1 passed, 1 ignored).
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --locked -p dw-engine -p dw-appdb -p dw-app -p dwcli --all-targets -- -D warnings`: passed.
- No live testnet fetch was run. The scratch worktree used mode 0700, its own `TMPDIR` and `CARGO_TARGET_DIR`, `CARGO_BUILD_JOBS=12`, `nice -n 10`, and the configured Rust wrapper.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
