# DW DP1-03 — confirmation review, round 2

Reviewed `dashwallet-desktop` `dw/dp1-03-names` at `5107a2055ef155cb996bc5761b3557e5aeebeebb`, against round-1 head `fd4a69a` and base `c19e441`. Followed `REVIEW-COMMON.md`. The implementation worktree was read-only; validation used a detached checkout at `/work/scratch/dp1-03-r2-sol/`. No commits, pushes, or GitHub comments.

Read the previous review, DASHPAY/ROADMAP, m4 Contract-Version 6, E0-04 §4.2 and accepted Q7, and the relevant program decisions. Checked the actual SDK/wallet code at the pinned Platform revision `bc41f1bc233dec4607d387101c1d9c2f111019b2`, including registration's provisional label append and marketplace ownership rows. DP1-05 integration remains deliberately deferred; this review does not reopen R5 as a present blocker.

## Findings

### R2-1 — major: an unresolved contested registration is still displayed and selectable as an owned name

**File:line:** `rust/crates/dw-engine/src/platform/names.rs:742`, `:1169`, `:1255`, `:1305`.

**Failure scenario:** An identity with no other name registers `alice`. The pinned wallet appends `alice` to `dpns_names` after its contested write returns; the process stops before the engine moves it into `contested_dpns_names`. The persisted intent is `contested:alice`, there is no owned marketplace row, and the contest is still running. `settle_pending` correctly refuses to declare that contest won, but `main_name` then passes the unchanged provisional `owned` list into `resolve_main_name`, which returns `Some("alice")`. `set_main_name(identity, Some("alice"))` also succeeds and persists an explicit pick without ownership evidence. The row rule therefore protects preference settlement only, not the actual ownership/display decisions.

The existing `a_listed_contested_label_is_settled_only_on_evidence` test calls `main_name` and discards its result in the no-row case; its older `carol` fixture also masks the fallback problem.

**Suggested fix:** Exclude unresolved pending contested labels from the effective owned-name set for display and explicit selection until an owned marketplace row or positive Platform ownership evidence establishes ownership. Apply the same rule during the SDK-append/engine-settlement window, including when `main_name` cannot acquire the registration lock. The failed-try-lock path also reads wallet names and database prefs separately while registration can settle between them; obtain a consistent settled view or a stable cached result rather than combining a provisional names snapshot with later cleared prefs. Filtering only a later pending flag would not close that race. Preserve legitimately owned contested labels acquired by transfer/recovery. Add a no-other-name test asserting `main_name == None` and explicit selection is refused before ownership confirmation, then accepted after confirmation.

### R2-2 — major: registration retries ignore the persisted kind, and new registrations ignore interrupted contests

**File:line:** `rust/crates/dw-engine/src/platform/names.rs:983`, `:985`, `:1085`, `:1134`.

**Failure scenario A:** Persist `extra:extra2` when there are no open contests and let its write land without settlement. A contest for `alice` starts before retrying `extra2`. The retry recomputes `NameKind::Temporary` from today's open contests rather than using the stored `Extra`; `settle` checks only whether *some* intent exists. It writes `temporary_name = extra2`, removes the original intent, and changes the displayed main name. A `main_name` recovery of the exact same original intent uses the stored `Extra` and would not do that. Thus outcome bookkeeping still depends on which recovery API runs first.

**Failure scenario B:** A cut-short contested `alice` registration is pending and appears in the library's names but not yet its open contests. Register `alice2` next. The per-identity lock serializes the calls, but `register` does not reconcile the earlier intent before classifying the new label, so it records `alice2` as `Extra`. Once the contest sweep discovers the running `alice` contest, no pending `alice2` remains to repair its temporary-name preference; the older `carol2` stays displayed.

**Suggested fix:** For a retry, use the stored kind of the matching intent rather than recomputing it. Before classifying a new registration, reconcile relevant earlier pending contests under the same identity lock; obtain Platform evidence for ambiguous provisional labels, or refuse until their state is known. Keep the user's explicit pick independent. Test both recovery entry points after contest state changes and a new plain registration following a cut-short contested write.

### R2-3 — major: refusal cleanup destroys recovery evidence but retains the provisional owned label

**File:line:** `rust/crates/dw-engine/src/platform/names.rs:621`, `:1011`, `:1014`, `:988`.

**Failure scenario:** A pending contested registration landed and the library lists its label. The vote-state page is full with 100 other identities; this identity is outside the returned page. `registration_step` returns `name.contest_open` because the page is truncated, and `register` treats that as proof the pending request cannot land, deleting the intent. On the next retry, `unsure` is false, so the locally listed contested label returns `Registered` without querying Platform, although it is only a contender. This does not require supporting crowded-contest writes: retaining uncertainty is sufficient. Even a definitive `Locked` or `Taken` answer has a second defect: the intent is deleted but the provisional `dpns_names` entry remains, so the next offline retry reports `Registered`, and `main_name` displays the label despite the known refusal.

**Suggested fix:** Distinguish incomplete evidence from a permanent negative result. Never clear pending intent on the page-full refusal; similarly, a deadline refusal based on an incomplete contender set cannot prove absence of an earlier successful join. Keep the conservative no-new-write refusal, but retain the marker until definitive ownership/contest membership or permanent negative evidence settles it. On a definitive negative result, remove/invalidate the provisional owned-name entry together with clearing the intent; do not leave it eligible for the local idempotent shortcut. Test a self-contender outside the first page and locked/taken outcomes with a locally listed provisional label; repeated retries must never turn those labels into an offline `Registered` answer.

### R2-4 — minor: the contract table still promises an actual fee ceiling

**File:line:** `docs/contracts/m4-dashpay-engine.md:145`, `:236`; compare `rust/crates/dw-engine/src/platform/names.rs:932`.

The code explicitly says the fee cost is budgeted, not enforced, and accepted E0-04 Q7 allows an estimate. The contract table nevertheless says registration "Costs at most" the bound and the engine "never charges more." If execution fees exceed the estimate, the SDK can spend above the grant's numeric cap; if the balance covers only the estimate, the preorder can spend before the domain fails.

**Suggested fix:** Describe the checked estimated budget consistently in these table rows and retain the explicit DP1-06/E0-04 integration requirement. Do not present the empirical 3× headroom as a proven maximum.

## Round-1 disposition and requested checks

- **R1's concrete bypass is fixed:** plain grants capped at 0 or 1 credits, and contested grants/balances covering only the fund, are refused before redemption/submission. The same `name_cost` feeds the quote and registration check. Known over-cap refusals preserve the grant. A retry after an ambiguous failed submission requires a fresh single-use grant. At the pin, this registration path builds/signs one preorder and one domain; the document put wrapper does not contain a re-signing retry loop.
- **Fee bound verdict: backlog/integration item, not a new blocker by itself.** E0-04 §4.2 explicitly permits conservative per-transition constants until DP1-06, and Q7 accepts `max_credits` bounding an estimate rather than the actual execution fee. The 100M+300M constants are empirically motivated headroom, not a proof against future fees. Over-estimate execution, concurrent external credit spending, or a contest changing between query and domain execution can still spend a preorder before a failed domain. Keep E0-04's per-call/transition accounting, re-signed retry charging, and dispatch/failure semantics as prerequisites before enabling a host flow that promises their guarantees. This branch has neither an enforced execution-fee ceiling nor the dispatch fence; do not claim otherwise. No new deterministic tiny-grant or fund-only precheck bypass found.
- **R2 fixed:** an unknown joining deadline yields `platform.unavailable`, no submission, no intent, and an unused grant. Already-self-contending results remain idempotent.
- **R3 improved but not closed:** intent is stored before submission, success/confirmed-error/idempotent paths share settlement, and ordinary settlement preserves the explicit pick. The three major findings above leave recovery-dependent state and false ownership answers.
- **Serialization:** the static mutex protecting the weak lock map makes lock lookup/creation atomic; live callers retain strong references, so they cannot receive different mutexes for the same wallet/identity. `register` holds its guard through submission and settlement. `main_name` holds a successful try-lock through pending settlement, and skips writes when a registration holds it. No lost pending-list update between these engine entry points found. This lock does not resolve an earlier interrupted operation's semantic state, nor exclude provisional contested names from read/selection paths.
- **`Vault::grant_purpose`: no new authority or secret leak found.** It returns a copy of purpose/caps already present in the issued `AuthGrant`, not a token, signer, key, or hold. It uses the same locked `valid_grant` validation as the prior `check_grant`, including expiry, wallet and kind; own-key/locked-vault behavior is preserved. Redemption still revalidates and consumes the single-use id. Caps cannot be changed between inspection and redemption; a concurrent revoke/redeem/epoch change fails the later authorization rather than authorizing a second write. This metadata read is not a spending reservation.
- **Contested ownership rows:** at the pin, `Owned` rows are generated from domain documents with `$ownerId` checked against the wallet identity, so they are suitable positive ownership evidence. The defect is their incomplete application to other code paths (R2-1), not the meaning of the row itself.
- **R4 fixed in this branch:** fallback compares acquisition timestamps, untimed names last, stable ties; marketplace `$transferredAt` then `$createdAt` override the library fetch stamp. The helper follows DP1-05's rule.
- **R5 remains ready for the planned integration:** `identity_dp_prefs` avoids the previous API name collision, the copied ownership helper and explicit-pick writer have rebase notes, and derived temporary/contested choices remain separate from the user's pick. On the DP1-05 rebase, share the resolver/ownership extraction and update all relevant persistence/cache paths, then assert agreement with `IdentitySummary.main_name` through sync/reopen. No demand to merge that deferred cache now.

## Validation

On the unchanged reviewed head, under `rust/`:

- `cargo fmt --check`: exit 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test --workspace --no-fail-fast`: exit 0; **627 passed, 0 failed, 3 ignored**, including doc tests. This independently reproduces the reported count.

All commands use `umask 022`, `TMPDIR=/dev/shm/dp1-03-r2-sol-tmp` (mode 0700), `CARGO_BUILD_JOBS=12`, and the scratch worktree's own target directory. `RUSTC_WRAPPER=` avoids the shared-sccache stall encountered in round 1. The optional read-only testnet check also passed: **10/10** sample labels matched the recorded explorer expectations; the homograph and fixture-owned-name assertions passed too. Ran the gate-built `dp1_03_names` executable with `--ignored --nocapture` and `DW_E005_TESTNET_FIXTURE=/work/scratch/dw-e005/testnet-fixture`. This checks today's DAPI answers against the committed explorer snapshot; it makes no registration write and covers no active ContestOpen/Unknown response. After the unchanged-head gate, added five review-only probes in the detached scratch checkout and ran `cargo test --workspace --lib review_ -- --nocapture`: **5 passed, 0 failed**. They assert observed faulty behavior:

1. A pending contested label with no owned row is displayed and accepted as the explicit main-name pick; the display answer is also wrong while the registration mutex is held.
2. Retrying an `Extra` intent after another contest starts changes it into a temporary-name preference.
3. A new plain registration following an interrupted contested registration is classified as `Extra`, so an older timed name remains displayed once the contest sweep catches up.
4. A truncated 100-contender page clears the pending marker, and the next offline retry falsely returns `Registered`.
5. Definitive `Locked` and other-owner `Taken` results clear the intent but leave the provisional local label eligible for display and an offline `Registered` retry.

The first probe run had four passes and one fixture failure: the old name in probe 3 had no timestamp, so the newly timed name won the acquisition fallback despite its missing temporary preference. Correcting the old name to its intended earlier acquisition time made that probe pass. The initial mistaken append used a nonexistent relative path and changed no source; its filtered Cargo invocation was stopped and rerun with the correct path and workspace feature selection. Neither experiment affected the unchanged-head gate.

No probes were committed or applied to the implementation branch. No live registration was attempted. The scratch worktree, build output, probe files/logs, and this review's `/dev/shm` directory were removed after validation.

## Verdict

**CHANGES-REQUIRED.** Close R2-1–R2-3. The estimated fee budget and deferred DP1-05 cache integration are not independent blockers for this round.
