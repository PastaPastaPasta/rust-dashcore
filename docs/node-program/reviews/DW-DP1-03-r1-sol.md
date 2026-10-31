# DW DP1-03 — independent code review, round 1

Reviewed `dashwallet-desktop` commit `fd4a69a` (`dw/dp1-03-names`), against `c19e441`, and the overlap with DP1-05 `4ee3f74e` (committed blobs; concurrent uncommitted DP1-05 fixes are outside this review). The implementer's worktrees were read-only. Review and experiments used a detached checkout under `/work/scratch/dp1-03-r1-sol/`. No commits, pushes, or GitHub comments.

Read REVIEW-COMMON.md first, DASHPAY.md, ROADMAP DP1-03/DP1-05, m4 Contract-Version 6 including decision 35, and the relevant E0-04 safety clauses. Checked Platform at the exact pinned revision `bc41f1bc233dec4607d387101c1d9c2f111019b2`, rather than the local Platform checkout's newer HEAD. Read iOS `37c0e78a2f557fa35c98c051a4ce40b77be350ee` via GitHub: `CreateUsernameViewModel.swift`, `UsernameMarketplaceService.swift`, and `DWDashPayConstants.m`, plus the pinned Swift SDK persistence behavior.

## Findings

### R1 — major: registration fees escape the grant cap and the balance precheck

**File:line:** `rust/crates/dw-engine/src/platform/names.rs:756`, `:761`, `:605`, `:502`.

**Reproduction:** Register a valid free plain label, e.g. `alice2`, using a `PlatformOp` grant with `max_credits = 1`, from an identity with enough credits. The plain-name path passes `needed = 0` into `identity_signer`; that helper accepts the cap and creates a PlatformIdentity signer. The vault checks only that this scope has a nonzero credit cap; the signer does not debit a credit budget. The pinned SDK's `register_dpns_name` submits a fee-bearing preorder and then a fee-bearing domain. Thus the operation can charge more than the authorized one credit. A zero-credit grant does fail at signer creation; that does not fix the one-credit bypass.

For a contested label, give the identity exactly the required contest fund and authorize exactly that fund. Both prechecks pass, although the preorder and domain fees are additional. The preorder can spend credits before the domain fails for insufficient credits. This contradicts m4 §1's budget ceiling and the requested refusal ordering. `contest_fund` caps only the voting fund, not these fees.

**Fix:** Price/bound both document fees plus the contest fund before redeeming authorization or submitting the preorder. Check the identity balance against that total and enforce the same total against the grant/lease; provide the matching `GrantAction::RegisterName` quote. In the E0-04 integration, charge the call/transition budget rather than relying on a signer scope as a spending limit. Add tests for plain-name cap 1, fund-only balance/cap, fee-inclusive success, and a fee increase/retry that would exceed the cap.

Also, a known budget refusal currently **consumes** its grant: `redeem_grant` removes it at line 604 before line 606 checks the cap. A helper call with cap 1 and needed 2 returns `platform.grant_exceeded`, and redeeming the same id afterward returns `GrantInvalid`. Validate/reserve atomically before consuming it, using E0-04's authorization path. The claim that all refusals precede grant use is too broad.

### R2 — major: a missing contest deadline permits a doomed, fee-bearing preorder

**File:line:** `rust/crates/dw-engine/src/platform/names.rs:475`, `:414`, `:747`.

**Reproduction:** A contested label has another contender and its joining window has closed. Let the vote-state query succeed, but let the current-contests listing fail or omit that label. `contest_end` returns `None`, hence `join_until = None`. `registration_step` returns `Ok(None)` (proceed) regardless of the current time. The registration consumes authorization and the SDK submits its preorder before the domain transition encounters the network's closed joining window. The preorder fee is avoidable. The existing unit test explicitly asserts that an unknown deadline proceeds.

This is also reachable through the pinned SDK's incomplete current-contests pagination: its continuation decision uses the number of DPNS polls in the **last timestamp group**, not the number of polls returned across the page (`contested_queries.rs:487`). A successful listing can therefore omit later contests. This is not a request to change upstream consensus behavior.

**Fix:** For a new contender, establish a joinable deadline before spending credits. If the deadline cannot be established, return `platform.unavailable` without redeeming the grant or sending the preorder, or obtain reliable per-poll start/end evidence. Retain the already-self-contending idempotent answer. Add a test with a closed poll and a failing/incomplete deadline query that asserts zero submissions and an unused grant.

The arithmetic for a *known* deadline matches the current versioned windows. iOS also allows an unknown deadline, so this is not a claimed iOS mismatch; reproducing that allowance does not satisfy this task's no-doomed-write requirement.

### R3 — major: successful/idempotent recovery bypasses contest and temporary-name bookkeeping

**File:line:** `rust/crates/dw-engine/src/platform/names.rs:733`, `:751`, `:778`, `:784`.

**Reproduction:** An identity owns an older `carol2`, is contending for `alice`, and the `alice2` domain lands. Terminate between the upstream wallet recording the new name and DP1-03 recording `temporary_name`, or make the domain's confirmation fail and the post-error domain lookup return this identity. Retry `register_name(identity, "alice2", ...)`. Both the locally-owned early return and the remotely-owned return report `Registered` without writing the temporary preference. `main_name` keeps showing `carol2` during the contest. The missing preference remains missing on every retry.

Similarly, terminating after a contested domain request lands but before `record_contest`/`contested_name` means the network-self-contending answer returns `ContestStarted` without repairing either local record. The confirmed-domain error branch at line 778 also bypasses all successful-path bookkeeping. A generic names sync can eventually recover owned labels, but does not reconstruct these app preferences.

**Fix:** Factor confirmed-success reconciliation and run it for the normal success path, an already-owned network/local name, an already-self-contending result, and a post-error confirmed result. Persist enough registration intent to reconcile the crash window without guessing which arbitrary historical owned name was the intended temporary name. Repair contest membership and preferences before reporting completion. Add deterministic tests for each interrupted boundary and the successful-write/failed-confirmation case. Do not simply overwrite the user's explicit main-name pick while reconciling.

### R4 — major: “oldest owned name” means vector order, not earliest acquisition

**File:line:** `rust/crates/dw-engine/src/platform/names.rs:288`, `:261`, `:586`.

**Reproduction:** Supply `DpnsNameInfo("new2", acquired_at=200)` followed by `DpnsNameInfo("old2", acquired_at=100)`, with no preferences or open contests. `resolve_main_name` returns `new2`. The `DpnsNameInfo` vector is not guaranteed to be sorted by acquisition. On restore the library stamps all fetched names with the same fetch time and preserves query order, so sorting those stamps alone would also be insufficient. DP1-05 deliberately uses marketplace `$transferredAt`/`$createdAt` and selects the earliest name; the two branches will disagree after restoring the same identity.

**Fix:** Reuse DP1-05's ownership/acquisition extraction and select earliest acquisition for the final fallback (unknown timestamps last, stable order for ties). Keep DP1-03's explicit-pick, temporary, and won-name precedence above that fallback. Make `DashPay::main_name` and `IdentitySummary.main_name` call the same resolver. Test unsorted timestamps and restore records whose fetch timestamps tie but Platform acquisition timestamps differ. Current DP1-03 tests use `acquired_at=None` throughout and assume the desired order in their fixture.

### R5 — major integration requirement: main-name writes bypass DP1-05's live preference cache

**File:line:** `rust/crates/dw-engine/src/platform/names.rs:675`, `:844`; DP1-05 `rust/crates/dw-engine/src/platform/recovery.rs:381`, `:207`.

**Reproduction:** Merge DP1-05 first and resolve only the textual conflicts. Call `DashPay::set_main_name` to choose a different owned label. DP1-03 writes SQLite directly; `main_name()` reads the new value, while DP1-05's sync `identities()` reads the old `IdentityChoices.main_names` cache. DP1-05's `NetworkSession::set_main_name` already updates both persistence and that cache, but DP1-03 never calls it. An ordinary sync does not reload the choice cache; the new pick can remain invisible in identity summaries until reopening/reloading.

Even routing the explicit pick through that helper is only part of the fix: DP1-05's summary resolver does not know DP1-03's temporary/contested preferences, so contest-driven selection would still diverge.

**Fix:** On the DP1-03 rebase, route choice mutations through the session's shared persistence/cache path; extend the cached read model to include the temporary/contested inputs and share one resolver. Automatic contest selection must not be stored as the user's explicit pick. Add an integration test that chooses/clears a name, opens/resolves a contest, syncs, and reopens, asserting `main_name()` equals `identities()[...].main_name` at every step. This is a required second-branch integration change, not a claim that DP1-03 alone contains DP1-05's cache.

## DP1-05 overlap and merge order

**Merge DP1-05 first**, following its own review closure, then rebase/fix DP1-03. No reason found to reverse the requested order. DP1-05 supplies the shared choices cache and the correct acquisition metadata; DP1-03 should build on those.

- There are no duplicate facade methods in the raw branches: DP1-05 leaves `names.rs` unchanged. Its `NetworkSession::set_main_name` is a different owner from DP1-03's `DashPay::set_main_name`; its `recovery::main_name` is a module helper. Their names are legal together, but their semantics currently disagree as described in R4/R5.
- `AppDb::dp_prefs` **does** collide: DP1-05 adds `(wallet_id, key) -> Vec<(identity, value)>`, while DP1-03 adds `(wallet_id, identity, keys) -> Vec<Option<String>>`. Rust cannot overload these. Preserve DP1-05's wallet-wide loader and give the identity-scoped reader a separate descriptive name (or adopt one unified API and update every caller).
- `set_dp_pref` is identical in both branches; retain one copy. Both use the same `main_name` key and the same `(wallet_id, identity, key)` storage scope. Neither introduces a new migration. The existing `V2026100801__dashpay.sql` creates `dp_prefs`, and existing wallet deletion/export/import and migration tests already include it. No conflicting migration version/checksum or need to rewrite a historical migration was found.
- `git merge-tree --write-tree fd4a69a 4ee3f74e` reports four textual conflicts: m4 contract, `dw-appdb/src/lib.rs`, `platform/dashpay.rs`, and `m4_dashpay_contract.rs`. Preserve both branches' visibility changes, implemented-call sets and non-facade file classifications. Regenerate/bless the combined m4 surface with a fresh Contract-Version; do not keep one branch's surface hash and drop the other.
- The second branch must resolve R4/R5, retain DP1-05's main-identity behavior and same-seed recovery tests, and rerun the combined workspace gate. Do not turn a recovered/default/temporary/won label into an explicit user pick merely to make the caches match.

## Claims, tests, and disposition of reported gaps

- **Rules/homographs:** confirmed by source. The 129 fixture entries are 56 validity, 24 normalization, 35 contested, 9 broken-rule, and 5 collision entries, each with provenance. The exhaustive comparison generates 137,279 labels. Normalization calls DPNS's pinned function directly (`o/O -> 0`, `i/I/l/L -> 1`, ASCII lowercase); invalid Unicode is rejected. `NoDoubleHyphen` matches the pinned validation and availability FFI used by iOS, even though the bare contract label pattern itself is less restrictive. The 23-character cap matches the iOS constant.
- **Availability:** domain ownership is checked before the contested vote-state query; Taken/Locked/Available/ContestOpen/Unknown mapping and normalized poll keys are implemented. **Deferring a join-deadline distinction in this read API is reasonable:** `ContestOpen` describes a running poll and m4 does not promise it is joinable. The write still needs R2's safe precheck.
- **Contest fund:** calls upstream `required_vote_resolution_fund_to_join` with the *existing* contender count and the SDK version. At PV14 the first 250 entrants pay 10,000,000,000 credits; with 250 existing contenders the next pays twice that, and every further 50 existing contenders adds a doubling. Before PV14 the fund stays flat at 20,000,000,000. Known deadline arithmetic uses the current mainnet/test-network duration and join window correctly. **The doubling claim is arithmetic-only here:** this branch refuses a full 100-contender page before reaching the pricing function, so no actual 250-contender registration is supported.
- **100-contender refusal:** may be deferred as a conservative limitation; it avoids underpricing/duplicating a hidden self-contender. Availability's count is a page/lower bound, not an exact count when full. Document that limitation and distinguish it from a closed join window in user-facing behavior. A follow-up should query/paginate sufficient vote-state evidence to support up to the protocol maximum and test 249/250/299/300 boundaries. Do not advertise this branch as supporting crowded contests or an exercised doubled-fund write.
- **Fee budgeting:** must be fixed now (R1); it violates the authorization ceiling and can spend credits on a registration that cannot finish.
- **E0-04 fence / leases / `broadcast_unknown`:** defer the transport/lease integration to E0-04 as the owning prerequisite, not by changing the already-approved contract to bless ordinary errors as proof of no submission. This branch takes raw grants only and exposes no dispatch artifact on ambiguous completion. It must receive the E0-04 Mode A/B handling before enabling a host flow that promises cancellation or offers retries after an uncertain write. A missing post-error domain is not proof that the preorder/domain was not sent. R3's positive-evidence bookkeeping can and should be fixed in DP1-03 independently.
- **No cache:** fine for these async reads. The mandatory cache issue is consistency with DP1-05's sync summaries (R5), not caching availability results.
- **No live registration:** do not spend public-network credits merely to review this branch. However, the present tests never exercise the successful `register_name` SDK write path, fee budgeting, grant survival on refusal, post-write error reconciliation, or crash boundaries. Add deterministic network/writer seams for R1–R3; run the planned T2 registration/contest suite when its environment is ready before declaring the end-to-end write flow validated.
- **Main-name persistence:** the stored explicit choice survives a partial/complete username sync and a session reopen in the existing test. That is useful evidence for #4978, but not evidence for acquisition ordering, interrupted registration reconciliation, or agreement with DP1-05 summaries.
- **Prefs safety:** parameterized SQL, atomic upsert/delete, canonical base58 identity keys, wallet scope, and an existing STRICT table with a composite primary key. No new migration defect found. Registration's split remote/wallet/app preference writes still need R3's recovery handling.
- **Provider feature:** `rust/Cargo.toml` enables `dpns-contract` on the trusted context provider, as claimed.

## Validation

The requested gate passed on the unchanged reviewed commit, under `rust/`:

- `cargo fmt --check`: exit 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test --workspace --no-fail-fast`: exit 0; **611 passed, 0 failed, 3 ignored**, including doc tests. This independently reproduces the implementer's count.

All runs used `umask 022` and `TMPDIR=/dev/shm/dp1-03-r1-sol-tmp` (directory mode 0700), with the scratch checkout's own Cargo target. The initial shared-sccache attempt stalled; only this review's processes were stopped, and the gate was restarted with `RUSTC_WRAPPER=`. No shared cache service or other agent process was stopped.

The optional read-only testnet check also passed: **10/10** sample labels matched the test's recorded explorer expectations, including `dasher -> Locked`; the additional `A11CE` homograph and fixture-owned-name checks passed. Ran the gate-built `dp1_03_names` integration-test executable with `--ignored --nocapture` and `DW_E005_TESTNET_FIXTURE=/work/scratch/dw-e005/testnet-fixture`. This checks today's DAPI answers against the committed explorer snapshot, not a fresh fetch of the explorer API. The sample set contains no active ContestOpen or Unknown response and makes no registration write.

After the unmodified gate, appended four review-only probes in the detached scratch checkout and ran `cargo test --workspace --lib review_ -- --nocapture`: **4 passed, 0 failed**. These assert the observed faulty behavior, rather than the desired fixes:

1. `review_grant_fee_budget_and_consumption`: a one-credit grant creates the plain-name signer; a known over-cap refusal consumes its grant.
2. `review_unknown_join_deadline_allows_a_write`: another identity's contest with no deadline authorizes the write step at an arbitrary later time.
3. `review_idempotent_temporary_name_does_not_repair_prefs`: with owned `carol2`/`alice2` and an open `alice` contest, retrying `alice2` returns Registered but leaves the temporary preference unset and displays `carol2`.
4. `review_oldest_name_is_not_acquisition_order`: names ordered `(new2, 200), (old2, 100)` resolve to `new2`.

The probes were never committed or applied to the implementer's branch. No live registration was attempted. The scratch worktree, probe artifacts, build output, and this review's `/dev/shm` directory were removed after validation.

## Verdict

**CHANGES-REQUIRED.** Fix R1–R4 and incorporate R5 when DP1-03 rebases onto DP1-05. Preserve the explicit user pick; unify the derived display-name behavior without letting sync choose on the user's behalf.
