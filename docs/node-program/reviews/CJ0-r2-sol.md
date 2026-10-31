# CJ0 dash-coinjoin — Sol r2 narrow review

**Verdict: ACCEPT WITH NITS**

## Numbered findings

1. **Nit — `dash-coinjoin/tests/core_ports.rs:944-946`: the test comment overstates what the oversized-output case proves.** With a `MAX_MONEY` input and a `MAX_MONEY + 1` output, the fee would be `-1`, below `COLLATERAL_AMOUNT`, even if the new range checks were removed. The output-total case also fails the fee check. Both assertions have the correct Core verdict, but the comment says only the output-total case would fail without the range checks. **Fix:** name both exceptions in the comment; retain the oversized-output assertion as a boundary verdict.

## Review basis

I reviewed only `adc4cac2d..452c3f580`. The new output and input-coin checks, plus both running-sum checks, match `MoneyRange` in pinned Core `0364ed626e` (`tx_check.cpp:38-45`, `tx_verify.cpp:188-191`). Accepted amounts and sums stay within `0..=MAX_MONEY`, so the Rust additions and fee subtraction cannot overflow. Core's `IsCollateralValid` reaches `ProcessTransaction` for fee-valid candidates; its `CheckTransaction` and `CheckTxInputs` reject the out-of-range values. I found no case where the new checks reject a collateral Core accepts. Core's signed-overflow cases are undefined before test-accept, as D12 states.

Under the documented `rand_below(999_999)` contract, the modulo leaves Core's `GetRand<int>(999999) + 1` result unchanged. Outside that contract, the ID stays in `1..=999_999`. The F2/D17 full-view warning, F4/D11 separate RNG note and F2 replay hand-off, F6 test comment, and rewritten D12 accurately describe the reviewed behavior.

**Validation:** `cargo test -p dash-coinjoin --all-features --locked` passed all 89 tests; `git diff --check adc4cac2d 452c3f580` passed. Worktree remained clean. No commit or push.
