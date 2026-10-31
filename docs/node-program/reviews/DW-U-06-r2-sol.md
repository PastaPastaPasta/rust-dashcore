# DW U-06 narrow Sol r2 review

Reviewed `3b69a56e..c409a559` in `/work/home/agent/workspace/dw-wt/u-06-coinjoin` against D3, D11, D16, the renderer tests, and the engine transitions. **Verdict: CHANGES REQUIRED.**

## Finding

1. **P2 — “Starting…” can still persist indefinitely after Stop → Start.** `apps/desktop/src/coinjoin/model.ts:290–293`, `apps/desktop/src/coinjoin/queries.ts:66–69`. After a denomination or collateral funding pass, stop mixing and start again before another block. The stopped read records the post-funding balances and wallet total. `start_mixing` clears the status to `idle` but keeps `last_success_height`; on every subsequent pass in that block, `auto_denominate` returns at its `WaitForAnotherBlock` check (`rust/crates/dw-engine/src/coinjoin.rs:1819–1822`) before setting a status or moving any balance. `sameMarks` stays true, so the page and Home card show a spinner and “Starting…” until a new block arrives, indefinitely if the chain stalls. D16 explicitly describes this restart case, but it recreates the misleading unbounded start state from r1 on a normal user action. Give this waiting condition a distinct or bounded presentation, and test Stop → Start in the same block after a successful funding pass.

## Smaller observations

- **P3 — An independent balance read can label an untouched first run “Mixing.”** `apps/desktop/src/coinjoin/queries.ts:56–69`. The status can render `off` and enable Start while `useBalance` is unresolved. The effect then stores `{ total: undefined }`. If the status changes to `mixing` before the balance read completes, the effect no longer updates the resting marks; when the balance finally arrives, `sameMarks` fails although the engine has not reached its first 5–15-second step. The new fixture test starts after waiting for `off`, but never delays the balance response. This is usually brief, yet it can persist during the same-block wait above. Keep the snapshot coherent or avoid treating a late initial balance as mixer activity.
- **P3 — D11's “what Save would write” comparison is textual.** `apps/desktop/src/screens/settings/CoinJoin.tsx:51–56`. Entering `04` for a saved value of `4` enables Save and reports “Saved,” although `checkSettings` parses both to the same number and writes no effective change. The new disabled Parallel sessions test covers the r1 transition; it does not cover equivalent numeric input. Compare validated values if D11's stated Save rule is meant literally.

## Checks

The D3 cap is correct for a lowered target and has unit and UI coverage. The D11 multi-session turn-off now drops the unsaved Parallel sessions edit after a successful write, retains it on refusal, and has a UI test. The D16 tests cover a changed status balance and a changed wallet total after funding; they do not cover same-block restart or asynchronous initial balance reads. `pnpm exec vitest run src/coinjoin/model.test.ts src/fixture/coinjoin.test.ts` passed (30 tests), `pnpm typecheck` passed, and `git diff --check 3b69a56e c409a559` was clean. I did not rerun the Playwright or Tauri suites. No source files were changed.
