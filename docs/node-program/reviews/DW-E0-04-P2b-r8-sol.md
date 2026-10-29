# DW E0-04 P2b — Sol review, round 8 (narrow)

Reviewed only `f062d09..c66d661` (`515be9d`, `c66d661`) at detached `c66d661fae6a372e2e4740343017609760739777`. The implementer's worktree and branch were untouched. Item 5, earlier changes, and DP1-02 integration were excluded.

## Findings

None. Severity, location, and failure scenario: not applicable for this range.

## Closure of requested items

| Item | Status | Evidence |
|---|---|---|
| 1. R6-02 / empty-tap boundary | **Closed under the manager's accepted narrowing** | `platform/journal.rs:368–379` calls the stop path for a lock removal with no lock transaction; `platform/lease/fence.rs:3577–3604` stops a running call with released, uncovered signatures. The named real-store test passes and its trigger mutation fails at the expected store assertion. `platform/lease/mode_b.rs:677–703` keeps a released, recordless failed step `MaybeSent`; the real-library empty-store test and `r6_01_a_released_call_with_no_recorded_lock_reads_maybe_sent` verify that state, including reload. The release-absence mutation fails with `NotSent` instead of `MaybeSent`. Core-only and other non-removal lockless stores do not change that independent release row. `docs/design/E0-04-grants-leases.md:982–995` states the stop's scope and the unrelated-removal abort cost. A successful call reports `Sent` from its positive result, not from absence. |
| 2. O-2 / PSBT | **Closed for the requested same-session path** | `send/psbt.rs:500–550` waits for peer readiness, discovers owning wallets, acquires their fences in wallet-id order, rereads raw unresolved inputs, and retains the guards through `broadcast`. The post-wait race test and existing held-input test pass; independently removing the hold check and weakening the fence wait each fails its intended assertion. All other production fence takers found by searching `coin_fence`, `try_coin_fence`, and `coin_fences.acquire` in `dw-engine/src` take one wallet fence; the PSBT path alone takes several. The per-wallet operations do not await another wallet fence. A funding permit holds its one fence until tracking or call end, so the PSBT path waits for it without a cycle. |
| 3. O-2 / CoinJoin session | **Closed** | `coinjoin.rs:2580–2635` takes the wallet fence, reads unresolved holds, and retains it through signing. The session test passes; removing the hold check or changing the fence wait to a try-acquire fails the expected assertion. The two manager-accepted Known limits are explicitly stated in E0-04 §2a.6. |
| 4. Initialization contract | **Closed** | `docs/design/E0-04-grants-leases.md:950` now says schema ≥ 7. |

## Mutations

Each production mutation was made alone in the detached worktree, tested, and restored before the next. All six test processes exited 101 because the named test assertion failed; none failed to compile.

| Mutation | Focused test assertion |
|---|---|
| Restore the old nonempty-lock-transaction tap trigger, excluding removal-only stores | `mode_b_a_store_with_no_lock_stops_a_call_whose_signatures_no_record_covers`: expected stop, got `Ok(())`. |
| Remove the PSBT held-input check | `mode_b_a_psbt_spending_an_unresolved_locks_input_is_refused`: got `NoPeers` instead of held-input refusal. |
| Make PSBT fence acquisition nonblocking | `mode_b_a_hold_recorded_during_a_psbt_broadcasts_peer_wait_refuses_it`: broadcast did not wait for the fence. |
| Disable the CoinJoin session held-input check | `mode_b_a_mixing_session_does_not_sign_an_unresolved_locks_input`: held-input assertion failed. |
| Make CoinJoin session fence acquisition nonblocking | The same test: signing did not wait for the fence. |
| Remove the released-row absence guard | `r6_01_a_released_call_with_no_recorded_lock_reads_maybe_sent`: got `NotSent` instead of `MaybeSent`. |

## Validation and limits

- `cargo test --locked -p dw-engine --lib platform::lease:: -- --skip platform::lease::stress_tests::`: **224 passed**, both before mutation and after restoration. This includes the focused removal, PSBT, CoinJoin, and real-library empty-store cases.
- `cargo test --locked -p dw-engine --lib platform::lease::mode_b_tests:: --quiet`: **76 passed**.
- `cargo fmt --all -- --check` and `git diff --check f062d09 c66d661`: **passed**. `git diff --quiet c66d661` and `git status --short`: clean after restoration.
- No extra probe was needed: the existing real-library empty-store test, reload test, and post-wait race tests exercise the requested claims directly. The complete workspace suite, live peer acceptance, cross-instance behavior, and DP1-02 integration were outside this narrow review.

**Verdict:** APPROVE

Sol gate satisfied: yes
