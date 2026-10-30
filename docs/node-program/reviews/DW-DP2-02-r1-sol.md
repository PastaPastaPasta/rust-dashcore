# DW DP2-02 — Sol review, round 1

Reviewed `682419a..5cbfe8c` (`d89d88d`, `7b354c3`, `5cbfe8c`) in detached scratch worktrees. The implementer's worktree and branch were untouched.

## Findings

1. possible security note: see manager
2. **Low — lease error contracts disagree.** `docs/design/E0-04-grants-leases.md:2545-2547` says a lease missing a call's purpose returns `platform.needs_grant`, while m4 version 12 (`docs/contracts/m4-dashpay-engine.md:1794-1798`) and the new code return `platform.grant_invalid` for a lease of another flow. Both refuse the write. Reconcile the design's flow rule with m4 so hosts know which code to handle.

## Manager boundary

possible security note: see manager

## Deviation assessment

| Deviation | Judgment |
|---|---|
| IdentityUpdate signs with MASTER and added keys | Correct for dpp's proof of possession; the targeted signer test covers the intended set. |
| `VaultIdentitySigner::only_keys` in the engine | Consistent with the identity-index vault grant policy; the narrowed signer's raw-vault accessor returns `None`. |
| Q8 inactive while sessions open Degraded | Matches the stated H-09 dependency; the Enforcing-mode guard has a focused test. |
| `RequestOutcome.request` is `$createdAt` in ms | Matches the journal's request reference and m4 version 12. |
| Another flow's lease returns `platform.grant_invalid` | Safe refusal; the E0-04 wording needs the alignment noted above. |
| `Lease::charge_credits` keeps a quote on failure | Conservative one-quote budget accounting; the failed-write lease test covers it. |
| Trusted-context-provider `dashpay-contract` feature | Needed for the proof-checked DashPay document path; no scope widening found. |

## Mutation table

Each production mutant was applied alone to the detached worktree, run against the named focused test, and restored. All eight were killed by test assertions, with no compile failures.

| Production mutant | Focused test | Result |
|---|---|---|
| Ignore the PendingIncoming eligibility guard | `eligibility_asks_the_wallet_then_platform` | Killed |
| Refuse balance equal to the quote | `a_send_refuses_before_it_redeems_the_grant` | Killed |
| Refuse a grant cap equal to the quote | `a_send_refuses_before_it_redeems_the_grant` | Killed |
| Admit MASTER as a contact-request document key | `each_flow_signs_with_its_own_keys_only` | Killed |
| Admit a wrong-flow lease to accept | `accept_establishes_from_their_request` | Killed |
| Skip the lease's quote charge | `one_lease_pays_for_one_write` | Killed |
| Ignore a landed request in failed-write read-back | `a_failed_send_that_landed_is_pending` | Killed |
| Remove the scan's 60-second expiry margin | `a_send_spends_its_scan_or_releases_it` | Killed |

## Validation and limits

- `dw-engine` request tests: 14 passed, public-testnet T2 test ignored. Names tests: 38 passed. User/scan tests: 19 passed, one network fixture ignored. m4 contract tests: 9 passed. Focused dwcli command-table test: passed.
- `cargo fmt --all -- --check` and `git diff --check 682419a..5cbfe8c` passed. The reviewed worktree was clean after every mutation.
- T2 was reviewed by reading only, as requested. The test reads mnemonics into zeroizing buffers and prints identities and fee measurements, not the mnemonic. The existing `A.mnemonic` and `C.mnemonic` files are `0600` under a `0700` directory. The test's Platform writes are C's key upgrade, C's request to A, and A's acceptance; ignore/unignore are local. The T2 measurements in m4 were not independently rerun.
- Commit `d89d88d` built alone with `cargo build -q -p dw-engine --lib` in its own detached worktree and Cargo target.

**Verdict:** REQUIRES CHANGES

Sol gate satisfied: yes
