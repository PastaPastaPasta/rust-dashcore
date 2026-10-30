# DW DP2-03 — Sol review, round 1

Reviewed `dw/dp2-03-user-links` at `f4d0877` against `main` at `08d3019` in a detached scratch worktree. No implementation branch files were changed.

## Findings

1. possible security note: see manager

2. **Low — Empty searches still fetch profiles.** Location: `rust/crates/dw-engine/src/platform/users.rs:180,236-240`. Scenario: a valid prefix with no DPNS matches calls `hits` with no identities, which still issues an `$ownerId In []` profiles query. The query can fail and is swallowed, adding a DAPI round trip and a misleading debug message to a no-results search. Fix: return an empty hit list before calling `profiles` when `found` is empty.

3. **Low — The m4 status summary is stale.** Location: `docs/contracts/m4-dashpay-engine.md:184-186`. Scenario: the summary still says every call is a stub except earlier milestones, while the DP2-03 rows correctly mark `search_users`, `resolve_user`, `my_user_link`, and `verify_scanned` implemented. A DP2-02 implementer reading the summary gets the wrong status. Fix: add DP2-03 to the summary or replace it with a pointer to the per-call status rows.

## Mutation table

Each mutant was applied only to `users.rs` in the detached worktree, tested, and restored before the next. “Killed” means the named T0 test failed as expected.

| Mutant | Focused test | Result |
|---|---|---|
| M1: identity match reversed | `ios_made_links_verify` | Killed |
| M2: expiry refusal bypassed | `dapk_codes_are_checked` | Killed |
| M3: unverified classification suppressed | `hits_follow_their_fetches` | Killed |
| M4: scan spend made a no-op | `a_scan_gives_a_send_its_proof` | Killed |
| M5: proof source given a different key | `a_scan_gives_a_send_its_proof` | Killed |

## Validation and limits

- Focused clean-source `dw-engine` user tests: 13 passed, 1 ignored (the read-only testnet case run separately). Additional scratch probes checked real DIP-15 derivation and proof binding, negative network/identity/account-reference cases, scan lifecycle, and normalization. The probes were removed from the worktree after testing.
- `dw-uri --lib`: 30 passed. `m4_dashpay_contract`: 9 passed. `dwcli` error and command-table tests: 2 passed. `cargo fmt --all -- --check` passed.
- The ignored DP2-03 test ran against the public testnet fixture in read-only mode: 1 passed. It exercised iOS-generated links and the typed proof seam. The T0 iOS fixture comes from Swift compiled with a Base58 shim, not a device-made QR. A device-made sample remains pending.
- The scan test signs with a fixed test key. It does not send a contact request or demonstrate that the named recipient accepts that proof; DP2-02 owns the send. `dw-ffi` currently has no DashPay facade binding (deferred to E0-13); the two new errors are mapped in `dw-engine` and `dwcli`. `classify_link` still routes `dapk` through Payment (accepted D8, DP2-08 follow-up).
- Search uses one profiles `$ownerId In` query after deduplicating up to 100 identities; the DashPay profile index is unique by owner. The API caps results at 100 and has no continuation cursor. The implementer's full gate reported 953 passed and one unrelated `e0_05` timing failure under load; I did not repeat the full workspace gate.

**Verdict:** REQUIRES CHANGES

Sol gate satisfied: no
