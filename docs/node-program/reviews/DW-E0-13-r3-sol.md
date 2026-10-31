# DW E0-13 round 3 — Sol review

Target: detached `555d220`, reviewing `9f27c09..555d220` only. Both commits have valid signatures. The implementer's worktree was untouched.

## Findings

1. **F1b — HIGH:** possible security note: see manager. The reported write/generation interval is a finding, not an accepted residual.
2. **F3a — MEDIUM:** possible security note: see manager. The per-report byte claim and process-lifetime report retention both need correction.

## Disposition

| Round-2 item | Decision | Evidence |
|---|---|---|
| F1, policy scope | Closed | The setting uses the vault file's 16-byte random ID. Create-after-destroy and vault replacement IPC tests pass; recovery makes a new ID, while wallet imports use the current vault. The old global key is not read. A random-ID collision is negligible, and an in-flight destroy either changes the ID seen at issue or invalidates an issued grant with the vault epoch. |
| F1b, concurrent policy change | Reopen, HIGH | A temporary two-stage IPC probe reproduced the reported interval. The existing barrier test protects changes completed before issuance but does not cover this interval. Possible security note: see manager. |
| F3a, bounded event bytes | Reopen, MEDIUM | The text and flow/artifact caps work and their mutant was killed. A separate temporary event-size probe disproved the universal 32 KiB claim; lifecycle inspection found another retained-state bound. Possible security note: see manager. |
| F3b, dropped `done` recovery | Closed | `EventMemory` records mapped `done` before `try_send`; `lock` returns the report; `lastLockReport` reads the last report for the active network and resets on session open. The focused IPC and overflow tests pass. |

`lastLockReport` adds no key material to the allowlist: its DTO contains lock state, wallet and flow identifiers, outcomes, and omission counts. Its byte bound is **not** established because of F3a. The >16-flow selection is an acceptable lossy cap only when callers treat `flowsOmitted > 0` as unresolved; prioritizing ambiguous outcomes over known sent outcomes would improve the report but cannot remove the need to handle omissions.

## Mutation table

Each production edit was applied singly, run against its focused test, restored, and touched afterward. “Killed” means the test failed at the intended behavioral assertion, not during compilation.

| Production mutant | Focused test | Result |
|---|---|---|
| Use the former global payment-policy key | `payment_authentication_does_not_outlive_its_vault` | Killed: new-vault default assertion failed. |
| Remove the policy-generation recheck | `a_policy_change_during_authorize_applies_to_it` | Killed at the intended policy assertion. |
| Disable the text byte cap | `oversized_payloads_stay_bounded_and_a_dropped_lock_report_is_kept` | Killed: oversized event assertion failed. |
| Skip retaining a `done` report | `the_last_lock_report_is_kept_for_its_network` | Killed: recovery query returned `null`. |
| **Own:** remove the vault-ID recheck | `a_vault_replaced_during_authorize_is_judged_by_its_own_setting` | Killed at the intended replacement assertion. |

## Validation and limits

- Clean head before probes: `cargo test -p dw-app --lib` passed **109/109**. After restoring and touching every temporary edit, it passed **109/109** again; `git diff --check` and `cargo fmt --all -- --check` passed; the detached worktree was clean.
- Temporary IPC ordering and event-size probes both reproduced the issues in the findings, then were removed. They used a private offline engine fixture and synthetic event data.
- Renderer: `pnpm typecheck` passed; `pnpm test --run` passed **30/30**.
- No live GUI, funded broadcast, or external endpoint test was run.

**Verdict:** REQUIRES CHANGES

Sol gate satisfied: yes
