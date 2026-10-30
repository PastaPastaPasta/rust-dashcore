# DW DP2-04 — Sol review, round 1

Reviewed `dw/dp2-04-private-details` at `1a2b35e3` against desktop `main` at `5da167fc` (7 first-parent commits, 29 files) in a detached, private scratch worktree. The diff was read in full. No implementer worktree, GitHub item, or branch was changed.

## Findings

1. possible security note: see manager

2. **Medium — `contact()` reads SQLite on the synchronous facade path.** Location: `rust/crates/dw-engine/src/platform/contacts_model.rs:609-615` (`DashPay::contact`, `contacts.rs:195-202`). Scenario: every established contact detail calls `AppDb::contact_info_digest` directly on the caller's thread; disk stalls or a contended SQLite connection delay a sync UI read. M4 §1 explicitly requires sync calls to read memory only. Fix: keep the digest in the session's contacts snapshot/cache, load it asynchronously and update it when a digest changes.

3. **Medium — the declared 1,000-document limit does not bound a publish.** Location: `rust/crates/dw-engine/src/platform/private_details.rs:624-627`, `requests_net.rs:173-185`; the new read also counts only parsed documents at `private_details.rs:128-139`. Scenario: `set_contact_info_with_external_signer` in the pinned platform-wallet dependency paginates *all* owned documents and opens them before writing, without the engine's limit. More than 1,000 documents make a publish perform unbounded network and crypto work while the identity write is held. Malformed documents do not count toward the engine read's 1,000 limit either. Fix: enforce a raw-document cap and deadline in the library publish fetch (or expose a bounded publish API), and count raw page entries in the engine read.

4. **Medium — the read-first lock has no 30-second end-to-end bound.** Location: `rust/crates/dw-engine/src/platform/private_details.rs:254-279,413-416,571-577`. Scenario: the 30-second timeout wraps only the network `documents` future; after it succeeds, up to 1,000 synchronous vault opens run without a timeout or cancellation point while the identity write lock is held. An edit can therefore block other writes beyond the accepted 30 seconds, and close cannot interrupt that loop promptly. Fix: budget the complete read, including opens, and add cooperative cancellation between opens (or move the opens to a bounded worker).

5. **Low — stale digest rows do not clear on a later read.** Location: `rust/crates/dw-engine/src/platform/private_details.rs:428-453`. Scenario: a contact that is no longer established is absent from the loop, so its old `dp_contact_info` row remains across every settle. Item 42 says the next read removes it. Fix: delete rows for contacts absent from the current established set, or document that they persist until wallet removal.

## Claims

| Claim | Review |
|---|---|
| 1. State derived on read | The `<2`, digest-match, and mismatch branches are correct; finding 2 concerns the sync database read. |
| 2. No-grant edits | The recorded-row refusal and no-row read-first branches match the contract; the offline/locked residual remains. |
| 3. Failed writes | Signed failures retain the local edit and schedule a later read; `Cancelled` restores prior shown details only when a row exists. |
| 4. Grants | The skip requires both shown details and their recorded digest to match. |
| 5. Unopenable documents | The code skips individual failures and refuses a nonempty, wholly unopenable *parsed* set. possible security note: see manager |
| 6. Signals | A changed digest calls `Platform{Contacts}`, even when the shown details did not change. |
| 7. Restore | The offline recovery test exercises unlocked bring-up and locked-then-unlocked restore; results below. |
| 8. Bring-up | Settle is spawned after bring-up and contacts tasks abort on close; finding 4 qualifies prompt cancellation. |
| 9. Keys and secrets | possible security note: see manager |
| 10. Implementer mutants | The reported 23/26 kill count was not adopted as proof; survivors are re-judged below, and independent mutants are run below. |

## Residuals from item 42

| Residual | Judgment |
|---|---|
| Offline or locked no-grant edit can be overwritten | Accepted, but it is user-visible data loss that should be surfaced when the product adds an offline edit flow. |
| Read-first holds the write lock up to 30 s | Defect: finding 4; the bound currently covers the fetch only. |
| Resettle tasks not deduplicated | Accepted for this scope; each has five bounded tries, although many failed writes multiply work. |
| Unsalted digest next to plaintext in `wallet.sqlite` | Accepted under the existing storage model; possible security note: see manager. |
| Row for a dropped contact remains | Low documentation defect: finding 5. A row can persist indefinitely, but cannot make a dropped contact appear published. |
| Up to 1,000 documents per write | Defect: finding 3; the pinned library publish fetch is unbounded. |
| Newest document wins between devices | Accepted as a policy. The implementation depends on the SDK returning query order; it does not independently compare `$updatedAt`. |
| `acceptedAccounts` rewritten empty | Accepted for the current account-0 clients, with an interoperability follow-up before any client writes nonempty accepted accounts. |

## Mutation table

Each reviewer mutant was applied alone from a saved copy of the exact-head source. After its focused test, the file was restored with `cp -p`, touched, and `cmp`-verified. These edits differ from the implementer's 26 mutations.

| Reviewer mutant | Focused test | Result |
|---|---|---|
| R1: compare a no-grant edit to the requested digest instead of checking row presence | `a_published_contact_needs_a_grant_to_change` | Killed: expected error became `Ok(Local)` |
| R2: roll back a signed failure before its read-back | `a_failed_write_keeps_the_edit_until_a_read` | Killed: shown details reverted to old `Published` |
| R3: skip a granted publish when shown details match but no row exists | `a_grant_publishes_and_records_the_details` | Killed: write count stayed 0 instead of 1 |
| R4: accept one wholly unopenable document as an empty read | `the_settle_applies_what_platform_holds` | Killed: settle incorrectly succeeded |

**Re-judgment of survivors:** M17 and M18 are not equivalent; possible security note: see manager. M26 is not equivalent: returning the document-source error early changes the reported error and omits the later retry when the source disappears after a signed write. The existing tests do not exercise that narrow path; it is a coverage gap, not evidence that the current branch mishandles it.

## Validation and limits

- Required `cargo test --locked -p dw-engine -p dw-appdb -p dw-app`: 115 app tests, 73 appdb tests, and 749 engine unit tests passed (6 engine tests ignored). The command exited 101 in `e0_05_bringup`: two of 13 wall-clock budget tests failed under concurrent load (4.320 s and 22.052 s observed). Each passed when rerun alone from the exact-head binary (3.589 s and 20.662 s observed).
- M4 contract test: 9 passed.
- `cargo fmt --all -- --check`: passed.
- Clippy `-D warnings` on `dw-engine`, `dw-appdb`, `dw-app`, and `dwcli`: passed.
- Restored-source focused control: 14 private-details tests passed, 1 ignored (T2). The detached checkout was clean and `cmp` matched the saved exact-head source after all mutants.
- No live testnet run: the ignored T2 case needs funds not available to this review.
- Scratch setup: detached worktree under `/work/scratch/dp2-04-sol-r1/`, mode 0700, separate `TMPDIR` and `CARGO_TARGET_DIR`, `CARGO_BUILD_JOBS=12`, `nice -n 10`. The scratch worktree and target were removed after validation.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
