# DW E0-06 — independent code review, round 1

Reviewed `dw/e0-06-changeset-tap` at **02879e3cea3bb63c099c5e7efdd2895b591ac6ed**, against `main` **c19e441262ee2149dc788ba2ed664263ff6d97cb** (68aded9, d57150c, 02879e3). Also inspected DP1-05 **c9a7d130cbe23131a290965e593eea741f966570**, `platform-wallet` pinned at `bc41f1bc233dec4607d387101c1d9c2f111019b2`, DASHPAY §3.4–§3.6 and the m4 contract §5–§6. The actual base-to-head diff is **15 files, +2222/−21**.

## Findings

### M1 — major: successful wallet stores can lose mandatory trust provenance

**Location:** `rust/crates/dw-engine/src/store.rs:102`; `rust/crates/dw-engine/src/platform/journal.rs:515`–525; `rust/crates/dw-appdb/src/dashpay_journal.rs:57`–99.

`inner.store` accepts the entity first. The tap then attempts one app.sqlite transaction containing both display events and trust flags, logs any failure, and returns no error. `WalletStore::store` consequently reports success even when none of the required `dp_trust_unverified` rows exists. An external SQLite writer holding `BEGIN IMMEDIATE` through the app database's five-second busy timeout is enough. Disk/write failures have the same structural outcome. There is no retained retry, failed-provenance latch, or startup reconciliation for this failure. A money-move gate that checks the absence of a row cannot distinguish this data from verified data after fallback ends. The eventual E0-10b/DP3-04 consumers therefore cannot meet DASHPAY §2.2's fail-closed rule using this tap.

The same path can permanently lose notification rows: upstream's payment insertion deduplicates in memory after the reported successful store, so another sync need not re-emit the missing event. The statement that the journal is display data does not apply to the trust flags sharing its transaction.

**Fix:** make provenance fail closed before the corresponding data can be treated as verified. Conservatively flag before accepting the wallet write, or introduce a durable pending/reconciliation mechanism and a money-move refusal while provenance is unresolved. Preserve failures for retry and restart recovery. Merely returning a failure after `inner.store` committed is insufficient and must not violate the persister's no-commit-on-retryable-error guarantee. Add busy/error-path tests through the real session store.

**Synchronous-write deviation:** not acceptable as currently implemented. I found no reverse lock cycle in the inspected paths: the inner persister's connection guard is gone before appdb access; appdb methods take leaf connection locks; history reads release their lock before database writes. But a leaf lock does not imply bounded latency. The appdb connection has a five-second busy timeout (`dw-appdb/src/lib.rs:177`), and upstream calls the store while holding the shared wallet-manager write lock, for example `rs-platform-wallet/src/wallet/identity/network/contacts.rs:169`–216. A blocked appdb operation therefore stalls a runtime worker and wallet access, not just a small journal append. With one worker, executor timers/cancellation cannot progress during the call. Use an appropriately bounded/offloaded writer with explicit durability, ordering, close/drain and error semantics; do not solve the latency problem by silently dropping flags.

### M2 — major: the timestamp fallback cannot guarantee catch-up silence

**Location:** `rust/crates/dw-engine/src/platform/journal.rs:449`–506 and 561–574; `rust/crates/dw-engine/src/platform/bringup.rs:426`–431.

The active incoming-payment recovery path defeats the timestamp boundary once a restore guard has dropped; name timestamps also need an explicit source constraint:

1. **An old payment can arrive without its history entry.** `payment_age` consults only same-changeset Core records and SessionHub's history. `Age::Unknown` is treated as news. Upstream's incoming-payment reconciliation reconstructs payments from receival-account UTXOs (`network/payments.rs:35`–70), and its wallet-event broadcast can lag/drop events (`network/payment_handler.rs:13`–23). An old transaction can be persisted/reconciled while missing from the host's broadcast-fed history. The overlay then has no Core record and is journaled unread. Receiving the old timestamp later does not repair it: INSERT OR IGNORE deliberately preserves the unread row. This can produce catch-up notifications during a large rescan.
2. **Name-time compatibility caveat.** At the pin, `rs-platform-wallet/src/wallet/identity/network/dpns.rs:367`–389 explicitly assigns wall-clock milliseconds to a newly fetched name because `DpnsUsername` does not expose the document's creation time. Calling that API to refresh a previously missed, years-old name supplies `acquired_at = now`, which this generic tap marks unread. **Scope qualification:** I found no current dw-engine caller of `sync_dpns_names`; DP1-05's explicit names pass uses marketplace document timestamps instead (with a local `now` fallback when source timestamps are absent, dpns_marketplace.rs:1676–1680). This is a demonstrated classifier/source incompatibility, not a demonstrated current automatic name-refresh call. Do not rely on the blanket journal.rs:16–20 promise for a future name-refresh caller, or treat observation time as proof that the name is news.

**Fix:** distinguish authoritative event/acquisition time from local observation time, persist enough recovery state to resume an interrupted catch-up, and keep restore guards across all recovery phases. Resolve payment age from durable Core records or defer the notification decision until its age/source is established; missing history must not automatically imply a live payment. Add tests for a cut-off restore followed by late DPNS refresh and for an overlay arriving before its old transaction history.

**DP1-05 composition:** the automatic merge is clean, but semantically incomplete. E0-06 guards only a bring-up entered with no identity on file and `!created_here`. DP1-05's `names_pass` (`recovery.rs:527`) and `discover_identities_of` (`recovery.rs:572`) have no tap guard. The queued bring-up after explicit discovery already has an identity, so E0-06's predicate will not guard it either. Wrapping only the foreground discovery call does not cover the later background DashPay/name passes, or their deferred work after stop/unlock. DP1-05 also starts the loops before running `names_passes` (`bringup.rs:238`–239), so recovery ownership must cover the handoff and concurrently touching passes, not start too late inside the final names job.

### m2 — minor: the tap has no independent removal fence

**Location:** `rust/crates/dw-engine/src/wallets.rs:223`–236; `rust/crates/dw-engine/src/platform/journal.rs:443`–447, 467–529; `rust/crates/dw-engine/src/events.rs:675`–684.

If a store reaches the tap after appdb deletion, `record` can recreate journal rows, trust flags and pending signals: `forget` only removes the cached boundary, and the tables have no wallet foreign-key/admission check. A deterministic tap-stage interleaving demonstrates that possibility. **However, that is not a reproduced production removal race.** The inspected live journal producers hold the shared wallet-manager write lock through persistence and tap recording (incoming-payment reconciliation, contact mutations, identity/name snapshots). Manager removal takes that lock too (`rs-platform-wallet/src/manager/wallet_lifecycle.rs:865`), so those calls finish before removal can proceed. The independent Core adapter releases the lock before storing, but its current payment overlay carries sent-payment verdicts, which do not produce received-payment journal entries or trust flags. Its asset-lock updates likewise produce signals, not journal entries. Already-pending Platform signals can still outlive WalletRemoved, because the pump has no wallet-generation filter.

I therefore do **not** promote the implementer's journal-removal caveat to a demonstrated major issue at this head. The hypothetical row recreation would have wipe/privacy consequences and must not be introduced when new producers are added or the tap is offloaded, but present producer serialization substantially narrows it. A helper-only reproduction cannot establish that missing step.

**Fix:** add generation-aware tap admission/draining and discard pending Platform signals for a removed generation, or formally enforce the producer-lock invariant. Preserve DP1-05's discovery-removal barrier. A real paused-store/removal test should cover the new architecture; do not count the tap-stage model as that integration test.

### m1 — minor: the fallback test omits touched identity tombstones

**Location:** `rust/crates/dw-engine/src/platform/journal.rs:181`–198; `rust/crates/dw-engine/src/platform/journal_tests.rs:894`.

The identity and identity-key branches signal nonempty removal changesets but build trust entities only from inserted identities and key `upserts`. An `IdentityKeysChangeSet` containing only `(identity, key_id)` in `removed` touches the identity and emits Identities while adding no flag. The analogous identity tombstone also adds none. Thus the literal acceptance criterion “every touched entity” is not covered by the current test or classifier. These deletions do not introduce new fallback-fetched key material, so I classify the isolated omission below M1 rather than claiming a demonstrated money-move bypass.

**Fix:** include known identity ids from tombstones, or explicitly document and agree a deletion-only exemption. Test both forms. For request/DPNS removals, specify how missing request timestamps/labels are handled instead of calling the representative upsert fixture “every entity.”

## Acceptance and claims

| DASHPAY §3.5 row | Test(s) inspected |
|---|---|
| identities / username / contest | `row_identities_signal_and_journal_usernames`, `row_identities_journal_a_won_contest_once` |
| contacts received | `row_contacts_received_journals_the_request` |
| contacts established | `row_contacts_established_journals_who_accepted` |
| alias / note / hidden / ignored senders | `row_contact_edits_signal_without_journal` |
| asset locks | `row_asset_locks_signal_their_registration` |
| payments overlay | `row_payments_journal_received_payments` |
| contact account registrations | `row_contact_accounts_signal_contacts` |
| DPNS name states | `row_dpns_name_states_signal_names` |

The per-row test claim is correct. Won contests are tested; lost/locked outcomes are explicitly delegated to DP1-04, not implemented or validated here. The contract's watch-row deletion obligation must remain with that task.

The pump test checks ≥250 ms flush spacing, leading delivery and the final burst change; BTreeSet coalescing retains distinct dirty domains/identities. Appdb tests and the replay/rotation tests demonstrate event-key idempotency, preservation of read state, and once-per-relationship suppression. Guard reference counting, wallet isolation, timestamp persistence and non-export to .dwbackup are tested. A genuine session-store wiring test exists and exercises rejection before tap recording. Those successes do not cover M1–M2 or the tap lifecycle gap.

The second known open item, a contact first seen only after a recent key rotation appearing as news, is **minor/low**: there is no original relationship time in the current snapshot, and the damage is a false notification, bounded by relationship deduplication. It should be documented and tested. The first known item (rows after removal) remains **minor/low at this head**, with the production-reachability qualification in m2; I did not independently reproduce a journal-producing store escaping the current removal serialization.

## Required changes when rebasing after DP1-05

`git merge-tree --write-tree --name-only c9a7d13 02879e3` reports no textual conflicts (tree `c8d64c71ac8027bace5732241d7794823ccf3b4e`). This is a semantic rebase checklist:

1. Keep both Live tap/store wiring and DP1-05's session identity-choice initialization. Keep both new appdb modules and both sets of platform modules/test classifications.
2. Hold `catch_up(id)` for explicit discovery, pass 2, and the queued/deferred recovery bring-ups. Carry the recovery intent across discovery → background bring-up → names, cancellation/restart and unlock, including partial discovery success. Account for loop startup before names completion. Add combined session-store tests proving all those writes are read; helper-only guard tests are insufficient.
3. Preserve DP1-05's names scheduling, identity-cache refreshes, error/cancellation handling and `end_discoveries` removal/rollback barriers. If offloading tap writes or addressing m2, add an independent tap generation/drain fence rather than relying on the old synchronous producer serialization. Fix interrupted recovery and age handling in M2; adding two guards alone does not fix late history outside them.
4. Merge both contract edits and both `NOT_FACADE` additions. Keep DP1-05's IMPLEMENTED entries and generated listing. Preserve the new reference encoding, relationship rule, local catch-up setting and contest-watch ownership, correcting the unsupported timestamp guarantee. Verify .dwbackup still excludes the local boundary.
5. Run fmt, clippy and workspace tests again on the combined tree, plus the new busy, removal and recovery-order regressions.

## Contract version

**No mandatory version bump is established by the contract's current facade-surface rules.** The header scopes the contract code to the §0 facade files, and its explicit bump rule names changes to signatures, records, trait impls or codes. E0-06 changes none of those in the facade: the generated §3 listing remains unchanged; journal files are NOT_FACADE; EngineEvent/PlatformChange are the planned §6 additions outside that surface; the FFI variant is deliberately withheld. §5's persistence/reference and catch-up clarifications change semantics but not a declared facade record shape. The current test does not enforce semantic versioning of all prose. If semantic changes are intended to require a bump too, state that rule and extend the gate; a passing surface test would not prove compliance with that broader policy. On rebase, retain the version/listing approved for DP1-05 rather than re-blessing blindly.

## Validation

All builds used the detached scratch worktree's own target directory, `CARGO_BUILD_JOBS=12`, `TMPDIR=/dev/shm/e0-06-r1-sol-tmp` (0700), and `umask 022`.

- `cargo fmt --all -- --check`: **PASS**, exit 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: **PASS**, exit 0.
- `cargo test --workspace`: the initial run stopped on unchanged `dw-desktop` `notify::tests::test_QT_031_notify_send_shows_and_reports_clicks` with `Text file busy (os error 26)`, at notify.rs:361. Its isolated rerun passed, and subsequent full runs passed that target. The fake executable is created by the test; no real desktop notification was used.
- First complete `cargo test --workspace --no-fail-fast`: **613 passed / 1 failed / 2 ignored**, exit 101. `e0_05_bringup::a_held_key_read_does_not_hold_spv_past_the_budget` started SPV after 3.036661273 s but saw an unexpected UncleanShutdown notice at e0_05_bringup.rs:739. The already-built test binary passed in isolation (SPV after 3.017210626 s, clean shutdown). A redundant package-only rebuild was interrupted rather than rebuilding a different feature-unification graph; its interruption is not a test failure.
- Final complete `cargo test --workspace --no-fail-fast`: **PASS — 614 passed / 0 failed / 2 ignored**, exit 0, including contract and bring-up suites. Thus the claimed 614/0 gate was reproduced, with the two transient failures above retained in the record. I did not establish that either transient is an E0-06 regression.

After the baseline gate, six temporary reviewer tests were appended only in the detached worktree and run with `cargo test --workspace review_probe -- --nocapture --test-threads=1` (exit 0, six assertions confirming current behavior):

| Probe | Observed result |
|---|---|
| `review_probe_busy_appdb_loses_flags_after_successful_store` | A second connection held `BEGIN IMMEDIATE`; the real session store returned success after **5.083575415 s**, with **zero trust flags** after rollback. A 100 ms timer on the calling Tokio executor took **5.083511009 s**. Confirms M1 and the latency concern. |
| `review_probe_persisted_old_payment_without_host_history_is_unread` | The session persister already held the confirmed Core record with block time 500 s; host history had no entry. A later received-payment overlay was accepted and journaled **unread** despite the first-bring-up boundary being current time. Confirms M2 without requiring a missing durable record. |
| `review_probe_old_payment_before_history_is_permanently_unread` | Supplying the old history timestamp after the initial overlay, then replaying it, left the row unread. Confirms the initial mistake is not repaired by replay. |
| `review_probe_key_removal_touches_identity_without_flag` | An identity-key tombstone emitted Identities but wrote no trust flag. Confirms m1. |
| `review_probe_old_name_with_refresh_timestamp_is_unread` | A synthetic name snapshot with the local-refresh timestamp was unread. Confirms the classifier's source-time caveat, not a live name-network integration path. |
| `review_probe_removal_can_be_followed_by_preaccepted_tap` | Classify → delete wallet rows → forget → record recreated a journal row, a trust flag and a pending signal. This models the tap stage only; it does **not** prove that current production journal writers escape the wallet-manager lock (m2). |

The temporary tests were removed after execution. The source tree was restored to exact head 02879e3, and the detached worktree, build artifacts, scratch logs and test TMPDIR were cleaned up. No branch changes, commits, pushes or GitHub comments.

**Verdict: CHANGES-REQUIRED.**
