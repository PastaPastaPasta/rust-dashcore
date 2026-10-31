# DW U-06 narrow Sol r3 review

Reviewed only `c409a559..1c32a631` (seven renderer, test, and design files) at `1c32a631`. **Verdict: ACCEPT.**

## Numbered findings

None. There is no severity, file:line, or fix to assign.

## Closure checks

1. **P2 closed.** `apps/desktop/src/coinjoin/model.ts:307-310` limits Starting… to `< 20,000 ms` after the first running read and then shows Mixing with the neutral waiting line. `queries.ts:74-80` schedules the rerender at that boundary. The model test at 19,999/20,000 ms and the Stop → Start Playwright test at 21 seconds would fail with the old unbounded branch.
2. **P3a closed.** `apps/desktop/src/coinjoin/model.ts:266-267` ignores a newly resolved wallet total when the stopped snapshot had no total; `queries.ts:67-73,129-143` records stopped marks and starts the window on the first running read. The model and Playwright late-balance tests would fail under the former `sameMarks` comparison.
3. **P3b closed.** `apps/desktop/src/screens/settings/CoinJoin.tsx:50-56` compares `checkSettings`' validated values with `current`, while keeping invalid edited text savable so validation can explain the refusal. The Playwright `04`/`4` and `1,000`/`1000` case would fail under the old raw-text comparison. Cancel still follows edited text.

The Playwright clock is installed before navigation in the two new elapsed-time tests; `runFor(21_000)` advances the page timer beyond its 20-second deadline. The fixture's funding progress changes the status balances, and Stop → Start leaves them unchanged, exercising the same-block idle presentation. The acceptance and lock/restart cases passed. D11 and D16 describe the implemented validation, snapshot, window, and waiting line.

None of the four declined suggestions must change: `pressStart` sets its fixed Date before the Start click (the elapsed-time tests install the clock before navigation); a running status first read late can show Starting… for at most the documented 20 seconds; the e2e literal 21 seconds is safely beyond the boundary, which the model test checks exactly; and the Playwright cases exercise the shared store through the real page and Home hooks without a separate store unit test.

Checks: `pnpm exec vitest run src/coinjoin/model.test.ts src/fixture/coinjoin.test.ts` passed (32 tests); `pnpm build:fixture` passed, including TypeScript and bundle checks; the five selected CoinJoin scenarios passed in both Chromium and WebKit (10 tests); `git diff --check c409a559..1c32a631` was clean. The worktree has no tracked changes. No source files were edited, committed, or pushed.
