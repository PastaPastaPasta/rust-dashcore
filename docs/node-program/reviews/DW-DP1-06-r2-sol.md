# DW-DP1-06 round 2 narrow review (Sol)

Reviewed `5aee23d2..7ff4b46d` and merge `13e2c01d` (`7ff4b46d` + `72d61af9`) in an isolated clone of `dw/dp1-06-credits`. The supplied worktree remained read-only. T3 live testnet top-up was excluded as requested.

## Round-1 findings

| Round-1 item | Status | Evidence and assessment |
| --- | --- | --- |
| M1 — consumption-unknown lock drops from pending/double-fund gate | **CLOSED** for a lock funded on this installation | `rust/crates/dw-engine/src/platform/topup.rs:109-149` distinguishes the journal's own signed locks from historical recovered locks and keeps own `RecoveredFromChain` locks `ConsumptionUnknown`. `:263-269` gates a second funding; `:297-313` and `:556-573` keep it pending; `:516-528` settles only a proved apply, not an unauthenticated “taken” response. `rust/crates/dw-appdb/src/dispatch.rs:274-290,1241-1279` persists settled locks through removal. False code-24/restart and stored-lock tests at `topup_tests.rs:1121-1184,1425-1516` passed. The old-behavior scratch mutant (`RecoveredFromChain => Settled`) made `a_funded_lock_recovered_from_the_chain_stays_pending_and_blocks` fail at `topup_tests.rs:1173`: pending length **0** instead of **1**. Fresh-machine backup restore remains an out-of-range note below. |
| M2 — document cost table is an estimate below real charge | **CLOSED** for the reported estimator/cost gap | `credit_costs.rs:63-70,183-197` prices at `2^48` assumed tree entries and doubles only Drive's non-exact processing. `:346-418` checks every known protocol version, four tree fills, measured PV13 maxima, and the exact/estimated split. The baseline suite passed 10/10. A scratch mutant restoring the old 1,000-entry, factor-one price made `the_document_prices_bound_the_estimate_in_full_trees` fail: at protocol version 1, contact-request estimate **103,607,060** exceeded price **99,283,060** at one million entries (`:369`). This validates that the new test catches the reported underprice; the broad upper-bound claim still rests on Drive's stated estimator bound and the documented size assumptions. |
| M3 — level remains stale after credit-spending writes | **CLOSED** | `balances.rs:140-181,221-275,309-365` marks a spend, refreshes after the write lock drops, retains the mark on failure or an older balance, and refreshes stale identities on a pass. Call sites cover requests, key upgrade, names, private details, and registration profile writes (`requests.rs:534,600,708`; `names.rs:1018`; `private_details.rs:985`; `registration_flow.rs:2920,2942`). `recovery.rs:500-516` exposes `credit_level` plus `balance_stale`; `bringup.rs:247-270` adds the periodic pass. Baseline balance tests passed 6/6. A scratch mutant omitting the spend mark made `a_spend_refreshes_the_balance_once_the_write_lock_is_released` fail at `balances.rs:462`: a failed refresh incorrectly left `spent=false`. |
| m4 — cap refusal consumes top-up grant | **CLOSED** | `topup.rs:423-445` checks the quote, step and cap through `funding_cap` before `authority` redeems the grant. `topup_tests.rs:992-1009` refuses a 5,000,000-duff grant for that amount plus fee, then successfully uses that same grant for a smaller top-up; it passed. |
| m5 — restart tests only retain in-memory World | **PARTIAL (coverage nit)** | `topup_tests.rs:153-173,510-527,1425-1516` now uses the real wallet store and reopens `wallet.sqlite` to prove persisted proved and `RecoveredFromChain` locks, their identity index, and a false “already consumed” response. Those tests passed. The scripted crash helper at `:268-294` still reuses the `World` Arc, and its `Fund::Tracked`/`Built` kill at `:1287-1316` is not a storage-backed kill. The principal persistence gap is covered, while that exact early kill window remains a test-evidence limit. |

## Merge and new-defect check

No new implementation defect was found in the fix range or merge. `git show --remerge-diff 13e2c01d` showed conflicts only in `docs/contracts/m4-dashpay-engine.md` and `rust/crates/dw-engine/tests/m4_dashpay_contract.rs`. The merge versions of `pump.rs`, `store.rs`, `wallets.rs`, and `dw-ffi/src/api/engine.rs` match the DP2-05 parent exactly; DP1-06 had no changes in those files to drop. In overlapping `mod.rs`, `runtime.rs`, `private_details.rs`, `recovery_tests.rs` and `dwcli/src/dashpay.rs`, the final tree retains both sides' additions. Generated desktop commands contain both credits and notifications bindings (`apps/desktop/src/api/generated/commands.ts:1515-1545,1818-1832`).

The m4 header is **Contract-Version 18** (`docs/contracts/m4-dashpay-engine.md:3`), its generated surface fingerprint says version 18 (`:544`), DP2-05 item 43 and DP1-06 item 44 are both present (`:2305,2347`), and DP1-06 cross-references point to item 44. The contract test's `IMPLEMENTED` list includes both notifications and credits (`rust/crates/dw-engine/tests/m4_dashpay_contract.rs:79-98`); its test binary passed **9/9**, including `surface_listing_matches_the_contract`.

## Scope notes

- A fresh-machine `.dwbackup` restore has no dispatch journal or `wallet.sqlite` (`rust/crates/dw-engine/src/backup.rs:10-20,173-181,345-386`). A chain-recovered, unconsumed top-up lock without local signed-lock provenance is classified historical by `topup.rs:140-145`. A scratch counterexample injected such a lock with no consumed state: `pending_top_ups()` was empty, a new top-up funded, and Finish later credited the first lock too. This limitation existed at `5aee23d2` (all `RecoveredFromChain` locks were excluded); it is **not** a regression in the reviewed fix range. Local crash/restart with the same dispatch journal is covered by M1.
- `resolve_top_up` deliberately accepts any proved pending lock (`topup.rs:649-676`), treating the user's assertion that credits arrived as final for the funding gate. A scratch counterexample resolved a proved lock before its first apply, funded another, then Finish applied the first; the two locks both credited. This is the documented explicit user-resolution escape hatch (`m4` item 44), so the host must present it only after an informed “credits arrived” action. I did not treat that authorized action as an implementation defect.
- `git diff --check` reports trailing whitespace in generated TypeScript (`CostTable.ts`, `IdentitySummary.ts`, `PendingTopUp.ts`, `TopUpOutcome.ts`). This is formatting output from the binding generator, not a functional failure.

## Commands and mutant results

All local builds used `CARGO_TARGET_DIR=/work/scratch/dw-dp106-r2-sol/target`, `TMPDIR=/work/scratch/dw-dp106-r2-sol/tmp`, `CARGO_BUILD_JOBS=12`, in the scratch clone. Mutations and counterexample tests were restored; the scratch clone ended clean at `13e2c01d`. No GitHub writes were made.

| Check | Result |
| --- | --- |
| `cargo test -p dw-engine --lib topup_tests` on clean source | 25 passed, 0 failed, 2 ignored (T2/T3). |
| `cargo test -p dw-engine --lib balances::tests` | 6 passed. |
| `cargo test -p dw-engine --lib credit_costs::tests` | 10 passed. |
| `cargo test -p dw-engine --test m4_dashpay_contract` | 9 passed. |
| `cargo fmt --all -- --check` | Passed. |
| M1 mutant: `RecoveredFromChain => Settled`; run `a_funded_lock_recovered_from_the_chain_stays_pending_and_blocks` | Failed as expected: pending 0 vs 1. |
| M2 mutant: 1,000 existing documents and processing factor 1; run `the_document_prices_bound_the_estimate_in_full_trees` | Failed as expected: 103,607,060 estimate > 99,283,060 price. |
| M3 mutant: omit `AfterSpend::drop`'s spend mark; run `a_spend_refreshes_the_balance_once_the_write_lock_is_released` | Failed as expected: `balances.spent(f.wallet, id)` false after a failed refresh. |
| Scratch `resolve` and fresh-restore counterexamples | Both passed, with the behavior stated above; tests removed afterward. |
| Implementer's supplied `dw-gate.log` for `13e2c01d` | Reports `GATE EXIT 0`, 1,594 passed, 0 failed; I did not rerun the full gate. |

The scratch `target/` directory was deleted after testing.

APPROVE-WITH-NITS
