# DW DP1-03 — narrow confirmation review, round 4

Reviewed `dashwallet-desktop` branch `dw/dp1-03-names` at `0ae1be56fd184f081f4b0f1ea8dba4b3a749e08e`, against r3 head `8ff269c0ffa47a7d6c154b48b960c1011d3b702d`. Read r3 first, then r1/r2 and `REVIEW-COMMON.md`; applied the code-review-validator workflow within the requested confirmation scope. DEC-124 is binding. The implementation worktree remained read-only; experiments used a detached checkout under `/work/scratch/dp1-03-r4-sol/`. No commits, pushes, or GitHub comments.

## Findings

### R4-1 — major: the claimed one read is five independent SELECTs, and can miss both pending and contention evidence

**File:line:** `rust/crates/dw-engine/src/platform/recovery.rs:752`, `:424`, `:319`; `rust/crates/dw-engine/src/platform/names.rs:402`, `:1248`, `:1250`; storage at `rust/crates/dw-appdb/src/lib.rs:697`.

`read_name_prefs` loops over five keys and calls `AppDb::dp_prefs` once per key. Each call acquires/releases the connection mutex, prepares its own SELECT, fully collects its results, and finalizes that statement. Neither one helper invocation nor one `spawn_blocking` closure makes these statements one SQLite snapshot. `identities()` also does not take the asynchronous `choice_writer` mutex. The async preference reader takes it, but that does not protect the synchronous reader.

**Concrete failure:** Prime the raw recovery snapshot with provisional `alice`, pending in SQLite and absent from ownership rows/open contests. Start `identities()` and let it read the old, empty `contested_name`. Between that SELECT and the `pending_name` SELECT, a registration retry receives positive self-contention evidence, removes the provisional live copy, records the open contest, writes `contested_name = alice`, and clears pending. Hold an ordinary wallet-manager write lock before the reader resumes. Its remaining SELECTs see empty pending/refused rows; its earlier contested result was empty. It combines those mixed-generation preferences with the old snapshot and returns `names = ["alice"]`, `main_name = Some("alice")`, while the live identity owns no name and `main_name()` returns `None`.

**Suggested fix:** Add an app-DB API that returns all required `(identity, key, value)` rows in one SELECT, and build `MainNamePrefs` from that result. One fully collected SELECT supplies a statement snapshot under the actual connection modes. Keep the DB read before selecting the live list/snapshot. Add an actual `identities()` regression that schedules settlement across the old third/fourth SELECT boundary; the current pure one-contender summary test does not exercise the read boundary.

### R4-2 — major: the older-snapshot filter remembers only the most recently settled contender

**File:line:** `rust/crates/dw-engine/src/platform/recovery.rs:319`; `rust/crates/dw-engine/src/platform/names.rs:388`, `:1118`, `:1248`, `:1250`.

`prefs.contested` is one name-selection preference, not a complete record of labels the identity contends for. Every settlement overwrites it, and removes that label from pending. The older snapshot still contains earlier provisional labels with no open-contest evidence.

**Concrete failure without concurrent SQL:** An interrupted sequence leaves `alice` and `bob` pending and provisionally listed. Take a summary, which correctly hides both but stores both raw copies. Retry `alice` with Platform evidence that this identity contends for both labels. `conclude_others` first settles `bob`, then the requested call settles `alice`: both leave pending, both enter the live open-contest list, but `contested_name` ends as only `alice`. With the wallet-manager write lock held, the next summary filters only `alice` from the older snapshot and returns `names = ["bob"]`, `main_name = Some("bob")`. The live identity owns neither label, and `main_name()` returns `None`. A single coherent SQL read alone does not close this case.

**Suggested fix:** Persist complete per-label suppression for contenders whose provisional copies may survive in an older snapshot, separately from the single contested-name selection preference. Publish that evidence before clearing each pending intent and filter both summary fields against it. Remove suppression only on positive ownership evidence as appropriate to DEC-124. Add a two-contender fallback regression through the real settlement path.

## R3 disposition and cache visibility

- **R3-1 closed:** Every `identities()` call obtains main-name preferences from SQLite and propagates a read failure. The choices cache load stores only `main_identity`, and name-preference writes no longer put names in that cache. The summary replaces `IdentityChoices.names` with its fresh DB result before calling `summaries`; a missing or failed choices load cannot supply name visibility. The inherited cache-removal race remains minor and concerns the main identity only.
- **R3-2's original definitive-refusal reproduction closed:** `forget` writes refused before removing the live provisional copy and before deleting pending, all awaited before returning the definitive refusal. The saved provisional snapshot remains filtered by pending or refused. The broader stale-snapshot settlement exposure is still open as R4-1/R4-2; it would be misleading to declare all of r3's snapshot concern closed.
- `update_refused(false)` is called only through settlement with ownership or contention evidence. A refused label without an ownership row is filtered from both the async view and summaries. The declared delay for later acquisition by transfer until a marketplace row appears is recorded and not raised again.
- The new `an_older_snapshot_shows_no_contended_label` test checks one already-populated contested preference through the pure summary function. It covers neither the split read nor an earlier second contender.

## SQLite isolation and the no-transaction argument

The production live session opens one `AppDb` and shares its `Arc` with sync callers and `appdb_op` (`session.rs:227`, `:234`; `coins.rs:217`). `AppDb` holds one `Mutex<Connection>` and requests `journal_mode = WAL` (`lib.rs:156`, `:173`). Default private connection caches/read isolation apply; the code enables neither shared cache nor `read_uncommitted`. The mutex is held for each `dp_prefs`/`set_dp_pref` operation, not for the five-query helper.

For a **single pending-to-refused move**, the proposed order is valid even without a write transaction. Let W be the committed refused insert, D the committed pending deletion, P the pending SELECT, and R the refused SELECT. W precedes D and P precedes R. If P sees pending, it hides the label. If P sees the deletion, W has already committed before P and therefore before R; R sees refused, assuming no subsequent legitimate ownership/contention settlement removes it. There is no schedule that misses both markers during that move. A statement error fails the call.

That proof holds for the actual shared connection and for separate private-cache connections. WAL gives each autocommit SELECT its own snapshot; a later SELECT can observe an intervening commit. Rollback-journal readers release their read locks when each fully collected statement ends, so a writer can commit before the next SELECT. Rollback journaling therefore supplies no snapshot spanning the helper either. A connection mutex prevents simultaneous operations on one connection but permits writes between its successive SELECTs. Neither mode salvages the contested-before-pending read order in R4-1, and neither fixes the missing per-label record in R4-2.

## Validation

On the unchanged reviewed head, under `rust/`:

- `cargo fmt --check`: exit 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test --workspace --no-fail-fast`: exit 0; **647 passed, 0 failed, 4 ignored**, including doc tests. This independently confirms the claimed gate. All three new committed regression tests passed.
- The detached checkout was clean before adding any probe; the implementation worktree remained clean at the exact reviewed head throughout.

After that gate, added five review-only tests in the scratch checkout and ran `cargo test --workspace --no-fail-fast review_r4_ -- --nocapture`: exit **101**, **3 passed, 2 failed**. Other tests were filtered out. The two failures assert the required safe display behavior and fail on the implementation's actual false ownership answers:

1. `review_r4_split_read_during_contention` — failed: actual `identities()` output was `names=["alice"], main=Some("alice")`. A thread-local, review-only pause after the real contested SELECT lets the real registration settle before the pending SELECT; an ordinary manager write lock forces the fallback. Before releasing the reader, the live owned-name list was empty, pending was empty, contested was `alice`, and `main_name()` was `None`.
2. `review_r4_two_contenders_in_older_snapshot` — failed: actual `identities()` output was `names=["bob"], main=Some("bob")`, with live open contests `["b0b", "a11ce"]`, no live owned names, no pending labels, and `main_name()` equal to `None`. This uses real `conclude_others`/registration settlement and no scheduled SQL interleaving.
3. `review_r4_database_error_never_uses_choices_cache` — passed: rename the real SQLite `dp_prefs` table while retaining its contents; summaries return errors both with and without the choices cache. Restore the table; the next call immediately hides the pending provisional label without a choices reload.
4. `review_r4_five_selects_have_independent_snapshots` — passed: the actual `AppDb::dp_prefs`/`set_dp_pref` operations reproduce old-contested/new-pending results in all four combinations of WAL versus DELETE rollback journal, and shared versus separate connections. Each configuration verifies the actual journal mode, `read_uncommitted = 0`, and autocommit state. The linked bundled SQLite version is **3.51.1**.
5. `review_r4_refusal_order_covers_all_statement_interleavings` — passed: all six legal interleavings of pending read/refused read and refused write/pending deletion, for all four configurations (**24 schedules**), retain at least one suppression marker. This corroborates the narrow refusal-order proof rather than rejecting it merely because there is no transaction.

All validation used `umask 022`, `TMPDIR=/dev/shm/dp1-03-r4` (mode **0755**, following the requested umask), `CARGO_BUILD_JOBS=12`, and `RUSTC_WRAPPER=`. Existing build artifacts were reflink-copied into the detached checkout's own `rust/target`, then rebuilt there; no target directory was shared. The review-only pause-hook placement emitted an unused-doc-comment warning in the probe build; the unchanged implementation's fmt/clippy gate was clean. No shared cache service or other agent process was stopped.

The scratch worktree, probe sources, build output, logs, and this review's `/dev/shm` directory were removed after validation. This uncommitted report is the only retained review artifact.

## Open questions / limits

This was the requested DB/cache/snapshot confirmation only. Settled fee accounting, grants, authorization, transport prerequisites, acquisition ordering, UI, and live-network registration were not re-reviewed. No public-network registration was attempted. The declared transfer-display delay and inherited main-identity cache-removal race remain documented limitations.

## Verdict

**CHANGES-REQUIRED.** R3-1 and the original refused-name reproduction are fixed, but the five-statement DB read and the incomplete contention suppression still allow an older snapshot to assert ownership of a contender.

**Sol gate satisfied: no.**
