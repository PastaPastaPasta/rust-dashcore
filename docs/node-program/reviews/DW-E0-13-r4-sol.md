# DW E0-13 round 4 — Sol review

Target: `555d220..67bb4dd` (`dw/e0-13-tauri-bindings-r`). All five commits verify as signed. Review used only a detached, private scratch worktree; the implementer's worktree and branch were untouched.

## Closure table

| Item | Decision | Evidence |
|---|---|---|
| F1b (HIGH) | **Closed** | `rust/crates/dw-app/src/api/vault.rs:348–372,825–849`: the reader pairs generation and setting under the async policy gate; the setter advances generation before its write under the same gate; issuance checks generation and vault ID after making grants and revokes on a stricter mismatch. The setting has only these app-facing read/write sites; `dw-engine/src/keys.rs:199–253` reads the vault-scoped SQLite key on demand and has no separate startup or settings-file reload. `AppState::session()` releases its slot before the policy gate; no vault, session, or engine path acquires the policy gate in reverse order. No standard mutex guard spans an `await`. The two existing off-to-on stages and two independent on-to-off stages passed. Returned grants made before a change may remain usable for their 120-second lifetime; an in-flight grant that has not returned may be revoked by the final recheck. |
| F3a (MEDIUM) | **Closed for encoded event and retained-state bounds** | `rust/crates/dw-app/src/bridge.rs:825–853` constructs all 18 variants and uses an exhaustive match. A temporary added variant produced Rust E0004 (non-exhaustive match). The fixture fills nested lists, options, enum strings, numbers, and JSON-escaped text at their caps and beyond. An independent hand-built report with 16 flows × 8 artifacts, 16 wallet IDs, maximum omission counters, and escape-heavy 128-byte IDs encoded to **28,873 bytes**, below 32,768. `EventMemory::record` removes a network's report on both `SessionClosed` and `SessionOpened` (`bridge.rs:433–440`). Explicit close, network switch, shutdown, and engine drop call `NetworkSession::close`, whose normal error handling and flow aborts still reach its final `SessionClosed` emit (`dw-engine/src/session.rs:660–735`). The app's close caller can be dropped while the engine-spawned close continues. A process-ending runtime teardown or panic before that final emit is outside this in-process retention claim. |

The generated `LockOutcome` binding exposes `flowsOmitted` and `walletsWithSecretsOmitted`; listed flows also expose `artifactsOmitted`. Positive counts let a consumer avoid treating the truncated report as complete. `lockStatus` can return the full wallet list; `leases` and `dispatchStatus` can reconcile omitted flows and artifacts. Outcome ordering is `maybeSent`, `willBeSent`, `sent`, `cancelled` and stable within a class.

## New findings

1. **LOW — current renderer behavior is overstated.** `docs/design/E0-13-bindings.md:12` says the renderer treats omitted flows/artifacts as unresolved and reads `leases`/`dispatchStatus`. In the current renderer, `apps/desktop/src/api/events.ts:33–35` invalidates only `lockStatus` on a completed lock report, and no screen calls `lastLockReport` yet. **Probe:** repository-wide search for `lastLockReport`, `flowsOmitted`, `artifactsOmitted`, and `walletsWithSecretsOmitted` found only generated bindings, fixtures, and the design text. **Fix:** word this as a required rule for the U-07 consumer, or implement it when the lease/dispatch views land. The DTO fields preserve the needed information, so no present screen misclassifies a truncated report.
2. **LOW — the D3 “bounded work” phrase is broader than the implementation.** `docs/design/E0-13-bindings.md:12` says sink mapping does bounded work. `rust/crates/dw-app/src/dto/platform.rs:276–289,355–361` sorts every flow and artifact before truncation and converts every wallet ID to hex before keeping 16. Work and temporary allocation still grow with an engine report's input length, although the queued DTO and retained report are bounded. **Probe:** source path from `TauriSink::emit` through `LockOutcome::from`; the wallet and list caps apply only after those full-input operations. **Fix:** narrow the documentation claim, or count and select the retained entries with bounded extra storage and convert only retained wallet IDs.

## Mutation table

Every mutation was applied singly to the scratch copy, run with its focused test, and restored. All 17 were killed by a behavioral assertion; none relied on a compile error or timeout. The final clean-source suite passed after restoration.

| Mutation | Focused test | Result |
|---|---|---|
| Old setter order plus no generation recheck | `a_grant_issued_once_the_strict_value_is_readable_is_refused` | Killed: grant wrongly returned after “on” was readable. |
| Authorize omits policy gate | `authorize_waits_for_a_policy_write_in_progress` | Killed: authorize passed during write. |
| Setter omits policy gate | `authorize_waits_for_a_policy_write_in_progress` | Killed: authorize passed during write. |
| Lock-report wallets uncapped | `every_event_fits_its_byte_bound` | Killed: retained wallet count assertion. |
| JSON escapes not counted | `cap_text_cuts_at_a_char_boundary_and_says_how_much` | Killed: encoded text cap assertion. |
| Flow lease ID uncut | `every_event_fits_its_byte_bound` | Killed: encoded event bound. |
| Lease budgets uncapped | `every_event_fits_its_byte_bound` | Killed: four-budget assertion. |
| Sync phases uncapped | `every_event_fits_its_byte_bound` | Killed: four-phase assertion. |
| Closed network report kept | `oversized_payloads_stay_bounded_and_a_dropped_lock_report_is_kept` | Killed: close purge assertion. |
| `maybeSent` ranked last | `a_cut_report_keeps_unresolved_outcomes_first` | Killed: report priority assertion. |
| `willBeSent` ranked last | `a_cut_report_keeps_unresolved_outcomes_first` | Killed: report priority assertion. |
| `sent` ranked first | `a_cut_report_keeps_unresolved_outcomes_first` | Killed: report priority assertion. |
| `cancelled` ranked first | `a_cut_report_keeps_unresolved_outcomes_first` | Killed: report priority assertion. |
| **Own:** skip the initial payment check | `payment_authentication_is_enforced_over_ipc` | Killed: no-credential spend grant wrongly returned. |
| **Own:** ignore vault ID on final recheck | `a_vault_replaced_during_authorize_is_judged_by_its_own_setting` | Killed: replacement-vault grant wrongly returned. |
| **Own:** leave Platform contacts identity uncut | `every_event_fits_its_byte_bound` | Killed: encoded event bound. |
| **Own:** leave dispatch artifact uncut | `every_event_fits_its_byte_bound` | Killed: encoded event bound. |

## Validation and limits

- Clean `67bb4dd`: `cargo test -p dw-app --lib` **114/114 passed**. With three independent scratch probes, **117/117 passed** after all mutations were restored. The reversed probes checked both the after-write and after-generation-bump stages; the hand-built event encoded to 28,873 bytes. A temporary `AppEvent::ProbeOnly` variant failed compilation at the exhaustive match with E0004, as intended.
- Renderer: `pnpm tokens:check`, `pnpm typecheck`, and `pnpm test` passed (**30/30**). `cargo fmt --all -- --check` passed on clean source.
- The policy gate deliberately serializes authorize behind a setting write. Its engine path waits for vault admission and the SQLite write (the database has a 5-second busy timeout); no lock cycle was found. The staged tests show the wait and the resulting policy value.
- No live GUI, funded payment, external endpoint, process-panic close, or platform-specific build was run. The source-level bound assumes engine events use validated network names, as all open-session emission paths do.

**Verdict:** APPROVE-WITH-NITS
Sol gate satisfied: yes
