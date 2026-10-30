# DW E0-06 — narrow confirmation review, round 3

Reviewed `dw/e0-06-changeset-tap` at **7120df693d28c2d96fc12b7192a528b0f613bfd1**, with round-2 head **8b459a01f379bd0b1417041d71844f408772c6e3** as the comparison. Read r2 first, then r1 and the binding **DEC-114 / DEC-125** rulings. The fix commit changes 18 files, +640/−696. This review accepts positive provenance and the monotone boundary as decided; it does not request phase guards, an owed-phase mark, a denylist, notification reconciliation, or re-review settled areas.

**Verdict: REQUIRES CHANGES.**

**Sol gate satisfied: no.**

The supplied gate reproduces **632 passed / 0 failed / 3 ignored**, with fmt and Clippy clean. **R2-1 is not closed end to end:** the refused-store/live-cache case is covered while the latch survives, but a failed latch write makes a durable fallback identity verified on reopen without a record. **R2-2 is not closed end to end:** the previous continuous-supervisor queue order is fixed and the owed-phase machinery is deleted, but concurrent starts can lower the boundary, and a discovery readmission lost while SPV is stopped fails to advance it before the replacement bring-up.

## Findings

### R3-1 — major, required: a failed latch write is forgotten on reopen

**Locations:** desktop `rust/crates/dw-engine/src/platform/provenance.rs:57`, `:83`, `:114`; `rust/crates/dw-engine/src/platform/recovery.rs:323`.

`fallback_in_use()` sets `fallback_used` before writing `trust.fallback_used_at`, logs a write failure, and returns success (`()`). Every later call returns immediately because `swap(true)` sees the in-memory latch already set. The durable write is never retried. On reopen, a successfully read **absent** setting becomes `false`; `is_unverified()` then returns false without consulting whether any positive record exists.

A focused probe includes the actual production `provenance.rs` and uses the real AppDb and SQLite. A second connection holds `BEGIN IMMEDIATE` while `fallback_in_use()` runs. Its five-second busy timeout leaves no setting. After rollback, a second fallback call still leaves no setting. The current session says unverified; after dropping and reopening AppDb/Provenance, the same key says **verified**, with **no `dp_trust_verified` record**.

The read-error case reaches the same problem. `open()` correctly treats an unreadable latch as set, but that speculative in-memory `true` also makes `fallback_in_use()` skip persistence. A probe drops `settings_kv`, opens Provenance (correctly unverified), recreates the original table, calls `fallback_in_use()`, and reopens: the latch is absent and the key becomes verified without a record. This table-repair probe is fault injection, not a claim that ordinary startup drops schema tables.

The independent **real-session** probe additionally discovers and durably stores an identity after that failed latch write, confirms `identities()[0].unverified == true`, shuts the engine down, and reopens the same network directory without another fetch. The same identity is then returned with **`unverified == false`**, and AppDb still has **no positive verification record**. The identity-manager insertion, wallet store, facade and close/reopen are real; the Platform scan is the repository's offline mock.

This is the same provenance requirement as R2-1, not a request to restore the failed-store denylist. The latch is the precondition on which positive provenance relies. A memory-only latch cannot certify that an installation never used fallback after restart.

**Fix:** establish the latch durably **before a fallback read is allowed to supply data**. An idempotent setter that returns its persistence failure, and whose E0-10b caller refuses the fallback fetch on that failure, is sufficient. Separate an in-memory conservative latch from knowledge that it is durable; an early return based only on `fallback_used` is unsafe, including for concurrent first callers and a latch inferred from a read error. Merely retrying later leaves the close/crash-before-retry window open. Keep the positive-record design; no per-entity failure ledger is needed. Add a regression through persisted identity → session close/reopen, plus the read-error/fallback transition.

### R3-2 — medium, required; cosmetic effect: concurrent starts can move the boundary backward

**Locations:** desktop `rust/crates/dw-engine/src/platform/journal.rs:319`, `:351`, `:364`, `:367`, `:517`; `rust/crates/dw-appdb/src/lib.rs:633`, `:645`.

`move_boundary()` reads the boundary, releases the cache lock, and later executes an unconditional settings upsert and an unconditional cache insertion of its captured `now`. AppDb serializes individual statements, not that complete read/compare/write operation. `with_tap()` dispatches separate blocking tasks; discovery admission does not serialize discoveries with each other or with a supervisor phase.

The counterexample starts with boundary 900. A phase captures 1000 and reads 900, then is descheduled before the setter. A later phase captures 2000, also reads 900, and writes/caches 2000. The first resumes and writes/caches **1000**. Both disk and memory have moved backward after a successful later phase start. A dated event at 1500 can now be stored unread.

A deterministic harness executes the **verbatim production ChangesetTap boundary/cache methods**, with a controlled clock and a pause immediately before the lower-valued AppDb setter. The setter itself uses real AppDb/SQLite. It confirms disk and cache **2000 → 1000**. This is an algorithm/interleaving probe, not a live Platform or full-supervisor race reproduction. The production blocking-task entry points permit that interleaving; there is no surrounding per-wallet serialization to prohibit it.

The supplied `a_recovery_phase_moves_the_boundary_forward_only` checks sequential calls and a clock moving back. It does not cover a stale caller overwriting a newer caller. The original r2 queued-bring-up schedule now passes, but DEC-125's monotonicity is not true under every start order.

**Fix:** make the read/compare/persist/cache update one serialized operation, or implement an atomic numeric max in AppDb and update the cache monotonically from its committed result. A SQL max alone does not fix the unconditional stale cache insertions: both the store completion and the initial database-read cache fill must preserve the maximum. Add a deterministic overlapping-start test. This is a small boundary primitive fix, not phase ownership machinery.

### R3-3 — medium, required; cosmetic effect: a stopped supervisor loses the boundary advance for a deferred discovery bring-up

**Locations:** desktop `rust/crates/dw-engine/src/platform/runtime.rs:282`, `:289`; `rust/crates/dw-engine/src/platform/recovery.rs:688`; `rust/crates/dw-engine/src/platform/bringup.rs:218`, `:445`, `:470`.

`queue_discovered()` sets `names_due` and signals `Readmit`. The runtime explicitly drops signals while SPV is stopped because the next start brings every wallet up anyway. That replacement bring-up does not execute the supervisor's `Readmit` arm, which is where this fix advances the boundary for an identity already on file. It only calls `note_first_bring_up()`; its local `advance_catch_up()` is conditional on **no identity**. Discovery has just supplied that identity. The names pass runs after the replacement bring-up, so its advance is too late for events already journaled unread.

The new real-session scheduler probe reproduces this order:

1. Start an imported wallet, complete the initial no-identity bring-up, and stop SPV.
2. Explicit discovery stores an identity, advances the boundary, leaves its names pass due, and sends a `Readmit` that has no supervisor receiver.
3. Supply a durable marketplace name row dated **after discovery's boundary but before the next start**.
4. Restart SPV. The bring-up for the on-file identity stores the name event **unread** under the old boundary.
5. The due names pass advances the boundary beyond the event's authoritative time, but the already-journaled row stays unread.

The probe uses the real session, public start/stop/discovery calls, supervisor, persister and journal. It replaces Platform calls with the existing offline seam. It observes two bring-ups and one names pass, an authoritative source timestamp, and an unread `username_registered` row even after the later boundary has passed that timestamp. It is not DEC-125's undated-event residual: this event is dated and is replayed by the bring-up performing the deferred discovery recovery.

**Fix:** advance at the entry of the replacement/deferred recovery bring-up too, using the scheduler's existing recovery scheduling information (for example, the due names work), or retain the readmission's recovery intent through stop/restart. Advancing every admitted bring-up is another simple option if ordinary warm-start policy permits it. Do not restore an owed-phase ledger or guards. Add this stop/discovery/restart order to the normal suite, asserting read. No retroactive journal reconciliation is requested.

## Confirmed parts and bounded scope

- **Facade provenance:** `summaries()` combines the identity, a profile actually shown, and every shown name; labels are homograph-normalized before lookup. Both the live manager snapshot and the older cached snapshot go through that same check. A missing record cannot pass while the latch is set. A failed verification-record read loads an empty allowlist. A failed `record_verified()` write does not add the in-memory record.
- **Migration / storage:** V2026100901 creates the global `(kind, key, verified_at)` table. Records preserve their maximum verification time and survive wallet removal. No wallet-fetch producer mints them; E0-10b remains responsible for calling the stub after verified proof acceptance. The store-refusal trust path is removed as DEC-125 allows.
- **Dormant denylist:** the explicit flag writer, reader and classification payload are deleted. A repository-wide Rust/SQL search finds the old table only in its migration, migration fixtures, wallet deletion and generic backup handling. It does not participate in trust decisions. Generic backup import can still insert carried legacy rows; those rows cannot create positive verification records.
- **Export/import:** actual AppDb wallet export/import carries neither the global latch nor `dp_trust_verified`; a focused round-trip probe confirms it. The `.dwbackup` payload contains seed/secrets and wallet-scoped app metadata, and deliberately omits wallet.sqlite. It therefore does not import managed identity/profile/name snapshots used by `identities()`; those are fetched again. An imported main-identity preference alone creates no identity read-model row. There is no demonstrated backup-only trust bypass. A future fallback fetch must establish the destination latch durably, which R3-1 currently fails to guarantee. Positive records cannot be supplied through generic wallet-row import.
- **Phase entry points:** restore discovery bring-up, explicit discovery, a names pass and the supervisor's `Readmit` all call `advance_catch_up()`. Initial bring-up retains the first-boundary behavior. The owed-phase types, phase completion/drop machinery and catch-up guards are gone. The continuous-supervisor overlap regression is in the normal suite and passes. Authoritative ages and the DEC-125 treatment of undated events are accepted without reopening DEC-114.

## Validation

Review scratch writes and validation used **umask 022** and **TMPDIR=/dev/shm/e0-06-r3** (0755); Cargo builds used **CARGO_BUILD_JOBS=12**. The implementer worktree stayed at the exact head and was not edited.

- Exact-head `cargo fmt --all -- --check`: **PASS**.
- Exact-head `cargo clippy --workspace --all-targets -- -D warnings`: **PASS**, exit 0.
- Exact-head `cargo test --workspace --no-fail-fast`: **PASS**, exit 0, **632 / 0 / 3**, Rust **1.98.1**, using that worktree's own `rust/target`. The three supplied regression tests pass. No failure rerun was needed.
- Standalone `cargo test --offline --manifest-path /dev/shm/e0-06-r3/provenance-probe/Cargo.toml -- --nocapture --test-threads=1`: **PASS**, exit 0, **6 / 0 / 0**, Rust **1.99.0**, with its own target. It includes the actual Provenance source and real AppDb, the two supplied provenance unit tests, busy-write/reopen and read-error/repair probes, export/import, and the verbatim boundary-method interleaving harness. The boundary harness emitted a visibility warning about its wrapper type; this is not a production Clippy warning. A queued repeat of this same scratch command also passed.
- Real engine-session probes: **PASS**, exit 0, **2 / 0 / 0**, 176 filtered out, **11.24 s**. Built in detached `/work/scratch/e0-06-r3-sol` at 7120df6, using its own copied target and Rust **1.98.1**, with only the two temporary tests appended to `recovery_tests.rs`. Executed `rust/target/debug/deps/dw_engine-7df89b7e15ee6f9c review_probe_r3 --nocapture --test-threads=1`:
  - `review_probe_r3_failed_latch_write_reopens_a_persisted_fallback_identity_as_verified`: current facade true → reopened facade false, same durable identity, zero verification records. Confirms R3-1 through the real session.
  - `review_probe_r3_a_discovery_readmission_lost_while_stopped_replays_history_unread`: discovery while stopped → resumed bring-up → names pass, dated historical name stays unread. Confirms R3-3.
  - The initial filtered workspace build (`cargo test --workspace review_probe_r3 -- --nocapture --test-threads=1`) was intentionally interrupted with exit **130** after the engine test artifact had compiled and `--list` showed both probes; only unrelated FFI targets were still building. No compiler was left running. This is an interrupted extra scratch build, not a failed test or an interruption of the passing exact-head workspace gate.

## Open questions and limits

No user input is required. E0-10b's live layered-provider wiring and DP3-04's money-move gate are future work; this review evaluates the provenance primitive they will consume, not an attempted payment. Offline session probes substitute only the Platform calls at the repository's existing seam. No live DAPI timing, macOS UI, or FFI regeneration is claimed. Settled journal classification, deduplication, latency and removal-fence findings are not reopened.

## Cleanup

The temporary session test source was restored to exact head 7120df6 and verified clean before removing the detached worktree and its own target. The standalone harness, its target, test scratch directories and logs were removed with `/dev/shm/e0-06-r3`. No probe/build process remains. The implementer worktree is still clean at 7120df6. This report is the only requested output, mode **0644**, and is **uncommitted**. No commits, pushes or GitHub comments.
