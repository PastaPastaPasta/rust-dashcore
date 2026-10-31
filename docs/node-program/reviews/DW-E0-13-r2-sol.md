# DW E0-13 round 2 — Sol review

Target: detached `9f27c09`, reviewing only `0444bc5..9f27c09`. The 21 rebased commits were outside this round.

## Findings

1. **F1 — HIGH:** possible security note: see manager.
2. **F3 — MEDIUM:** `lockProgress` `done` contains `LockOutcome.flows` and per-artifact outcomes, but D3's proposed recovery query, `lockStatus`, returns only vault state. The `lock` IPC command also discards the report, and `leases` has no artifact outcomes. A dropped `done` event has no query path back. Locations: `rust/crates/dw-app/src/api/vault.rs:542,599`, `rust/crates/dw-app/src/bridge.rs:164`, `rust/crates/dw-app/src/dto/platform.rs:294`, `docs/design/E0-13-bindings.md:12`.
3. **F3 — MEDIUM:** possible security note: see manager.

## Closure and follow-up checks

- **F1:** The new commands and default-on grant checks pass the ordinary IPC tests. The setting is scoped by the engine's per-network data directory; the temporary IPC probe observed an off value on regtest and the default on value on testnet. The lifecycle probe exposed finding 1. Existing grants have the vault's configured 120-second TTL and are not revoked by a policy change, as D11 states.
- **F2:** Closed. `reserve_prepared` counts open and in-flight handles before the signing await. An unfilled slot's `Drop`, `close_draft`, and `clear` release reservations; `fill` transfers its slot into a prepared handle under the same mutex. The barrier test proves the cap loser fails before its grant is redeemed. All four focused mutants below were killed.
- **F3:** The bounded channel, `try_send`, coalesced overflow flag, and idle timeout work in their focused tests, including a late drop after the queue drains. Findings 2 and 3 leave the full F3 ruling unmet.
- **F4:** Closed under the documentation ruling. D8 and the command docs say an in-flight broadcast can finish after `closeDraft`; `closeSendDraft` returns DEC-67's exact signed-before-lock line when its caller reports a running broadcast, and its renderer test passes.
- **7ce7ccc:** The quorum source and trusted-only commands, debug-only setter guard, `ContactList.updating`, and `platformTrustMismatch` mirror the engine changes. Generated bindings, capability grants, and the coverage test pass: **253 facade methods, 200 bound, 53 excluded**. I found no further issue in this follow-up.

## Mutation table

Each production edit was applied singly in the detached worktree and restored immediately afterward. “Killed” means the focused test failed for the intended assertion, not a compile error.

| Production mutant | Focused test | Result |
|---|---|---|
| Omit `PlatformOp` from the payment-purpose predicate | `payment_authentication_is_enforced_over_ipc` | Killed: no-credential Platform grant was accepted. |
| Remove the encrypted-vault condition from the payment guard | `payment_authentication_needs_an_encrypted_vault` | Killed: an unencrypted vault's grant was refused. |
| Exclude in-flight reservations from the prepared-handle cap | `the_last_prepared_handle_goes_to_one_prepare` | Killed: both racing prepares passed the cap. |
| Remove the unfilled slot's `Drop` release | `the_last_prepared_handle_goes_to_one_prepare` | Killed: the failed prepare retained its slot. |
| Remove `close_draft`'s reservation release | `the_last_prepared_handle_goes_to_one_prepare` | Killed: the closed draft retained slots. |
| Remove `clear`'s reservation release | `the_last_prepared_handle_goes_to_one_prepare` | Killed: the network switch retained slots. |
| Suppress the overflow flag on a full queue | `a_stalled_forwarder_overflows_once_and_stays_bounded` | Killed: no overflow signal arrived. |

## Validation and limits

- Original head, after all restorations: `cargo test -p dw-app --lib` **103/103 passed**, including binding and facade coverage checks. `cargo fmt --all -- --check` passed.
- Renderer: `pnpm --dir apps/desktop typecheck` passed; `pnpm --dir apps/desktop test --run` **30/30 passed**.
- A temporary IPC lifecycle probe ran in this worktree and was removed. It used private offline test data and no real network send. The worktree was clean after the final suite.
- No live GUI, funded broadcast, or external endpoint test was run.

**Verdict:** REQUIRES CHANGES

Sol gate satisfied: yes
