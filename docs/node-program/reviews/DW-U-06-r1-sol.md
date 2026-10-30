# U-06 CoinJoin UI review

Reviewed `b7e83e32..3b69a56e` against `docs/design/U-06-coinjoin.md`, the CoinJoin sections of `docs/design/UX-SPEC.md`, the real engine, and the T1 report in `/work/scratch/dw-u06/t1-out/result.json`. Verdict: **CHANGES REQUIRED**.

## Findings

1. **P2 — “Starting…” can persist after mixing has begun.** `apps/desktop/src/coinjoin/model.ts:271`. The model treats every running, incomplete mixer with status `idle` as the initial start. On the real engine, `start_mixing` clears the status (`rust/crates/dw-engine/src/coinjoin.rs:1245`), and a successful denomination or collateral funding pass returns without setting another status (`:1861–1875`). The same-block wait then returns before setting one (`:1820–1822`). A wallet that creates denominations can therefore still show “Starting…” until a later block, indefinitely on a stalled chain. The r1 fix covers only 100% progress. Have the engine expose a distinct funding/waiting status, or have the UI track the initial phase separately from a later `idle` status; test an incomplete mixer after a successful funding pass.

2. **P2 — “Mixed X of Y” can show X greater than Y.** `apps/desktop/src/coinjoin/model.ts:253`. `X` is the wallet's entire `fullyMixed` balance, while the engine caps `amountAndRounds.amount` at the configured target (`rust/crates/dw-engine/src/coinjoin.rs:1040–1041,1102–1105`). For example, after mixing 10 DASH and lowering the target to 2 DASH, the page and Home card say “10 of 2” alongside 100%. Cap the displayed numerator at the target for an “of target” display, or label the actual mixed balance and target separately; add a test for a reduced target.

3. **P3 — Save can report success while discarding the edited Parallel sessions value.** `apps/desktop/src/screens/settings/CoinJoin.tsx:49,69–80`. Enable multi-session, change Parallel sessions from 4 to 6, then turn multi-session off before pressing Save. The disabled input keeps `edits.maxSessions`, so `changed` leaves Save enabled. Submit replaces the edit with `current.maxSessions`, writes 4, clears the edit, and says “Saved.” Clear the edit when turning multi-session off, or derive Save's enabled state from the effective settings that submit will write. Cover this transition in the UI test.

## Checks and limits

The focused Vitest suites (`model`, fixture CoinJoin, event bridge) passed: 36 tests. `pnpm typecheck` and `git diff --check` passed. The T1 result records a real Tauri UI start, a running engine that reaches Paused, a lock stop, a mixing-only restart, and a confirmed UI stop. It did not exercise masternode rounds; the design records a separate passing `dwcli` regtest suite for those. I did not rerun T1. Existing keys and page remounts separate wallet status and local action state on wallet switch. The Playwright suite covers a refused start and successful stop, but does not exercise a refused stop or the disabled-field transition above.

**Verdict — CHANGES REQUIRED.** The UI start and stop acceptance path has credible real-engine evidence, and the focused checks pass. The state model can nevertheless leave an active mixer labeled “Starting…” until a new block arrives, and the progress display can contradict its own target; fix those correctness issues before accepting U-06. The misleading Save state is a smaller settings issue to address in the same pass.
