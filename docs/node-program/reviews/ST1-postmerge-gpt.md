# ST1 post-merge GPT review: engine layer

Reviewed merge `583d0fbc` in `/work/home/agent/workspace/rdc-wt/integration`. The checkout is now `a0f1596275c58e7c58546f6bca8ecbc0eb3b1f55`; `git diff 583d0fbc HEAD -- dashd-store` is empty. All references below are relative to that worktree and apply to the merge. Context: `reviews/ST1-r4-opus.md`.

Scope: `dashd-store/src/engine.rs`, `rocks.rs`, `memory.rs`, their batch implementations, and `fail_stop.rs` (necessary to inspect the latch). No importer, consensus, dependency-source review, builds, long jobs, or branch modifications.

## 1. Crash atomicity and Durable commits

**Yes, the configuration preserves multi-CF batch atomicity under RocksDB's normal crash-recovery guarantees. No missing sync propagation found.**

- The contract requires all-or-none recovery and commit-order durability (`dashd-store/src/engine.rs:104-106`), with Durable covering preceding buffered commits too (`engine.rs:29-34`).
- `RocksBatch` records each CF operation into one native `WriteBatch`, rather than issuing separate writes (`rocks.rs:463-477`). The entire batch goes through one `db.write_opt` (`rocks.rs:612-616`).
- The database enables `atomic_flush`, PointInTime WAL recovery, and paranoid checks (`rocks.rs:161-165`). Atomic flush prevents independently persisted CF memtables from defeating the recovered WAL prefix. WAL remains enabled and manual WAL buffering remains disabled: the options start with RocksDB defaults and neither setting is overridden (`rocks.rs:153-168`, `614-616`).
- Durable explicitly sets `WriteOptions.sync = true`, and those same options reach `write_opt` (`rocks.rs:614-616`). Buffered sets it false. `sync_wal` calls `flush_wal(true)` through the same guard (`rocks.rs:619-620`). `wal_bytes_per_sync` (`rocks.rs:158`) is incremental writeback, not the Durable barrier; the explicit sync flag supplies that barrier.
- Explicit memtable flushing submits every schema CF in a single flush request (`rocks.rs:623-628`), with atomic flush enabled. The extra RocksDB `default` CF is not a batch target: handles come from the schema (`rocks.rs:173-180`).
- MemoryEngine holds one write lock across applying all operations, buffering the whole batch, and the Durable barrier (`memory.rs:114-125`, `231-238`). Its simulated recovery also holds that lock and restores whole buffered batches in prefix order (`memory.rs:79-89`), never individual CF operations.

The short reopen tests below corroborate the paths; they do not independently prove power-loss behavior. The conclusion relies on documented RocksDB defaults and persistence semantics, not a new crash campaign.

## 2. Fail-stop coverage and the counter/write race

**Calls beginning after a foreground failure has returned are refused. Background errors observed by the counter are latched before a real batch or barrier executes. There is a concurrent-operation window; the implementation does not promise unconditional exclusion of already-running calls.**

- The actual contract explicitly leaves writes concurrent with a failing call unordered (`engine.rs:77-85`).
- `FailStop::guard` checks its `OnceLock` before calling the operation; any error is latched before returning (`fail_stop.rs:37-51`). Therefore a later call cannot pass an already-set latch. All engine batch writes, including empty and Durable batches, WAL syncs, and memtable flushes use `guarded` (`rocks.rs:612-628`). `guarded` checks the background counter before the operation (`rocks.rs:272-276`). `wait_for_compactions` also latches observed background failures (`rocks.rs:217`).
- A nonzero background-error count always makes `check_background` fail: either its empty probe gets RocksDB's stopping error, or the successful probe is followed by an explicit engine error (`rocks.rs:243-269`). Thus automatic recovery does not defeat a subsequent counter check. The probe writes no user operations (`rocks.rs:254`).
- **The precise race:** W reads `background-errors == 0` (`rocks.rs:244-250`); a background job then fails and increments the counter; W executes its batch (`rocks.rs:275`, `616`). A soft background failure can permit W to succeed. A sufficiently delayed W could also resume after RocksDB recovers from a hard failure. There is no lock or second counter check covering that interval. Similarly, a foreground operation that has already passed `fail_stop.rs:42` can execute after another thread sets the latch. Buffered, Durable, and barrier calls all share this window.
- These calls began before the failure and overlap it, so they fall within `engine.rs:84`'s exception. This is **not** evidence that a call beginning after the failure returns can evade the latch. Round 4's opening statement that no write can slip past the latch should be read with this concurrency qualification; its detailed race analysis already supplies that qualification.
- MemoryEngine has stronger exclusion: the same write lock protects the latch check, injected failure, data changes, and barrier (`memory.rs:114-125`, `231-246`).
- `raw()` exposes a writable native DB and explicitly says writes through it bypass the latch (`rocks.rs:198-201`). The guarantees concern the engine API and compliance with this restriction. Enforcing them against arbitrary `raw().write/put` calls would require a restricted inspection interface.

No blocker or major under the stated contract. A stronger requirement forbidding even overlapping writes after the error event would require coordination with background error notification and write admission; a post-write check alone cannot undo an applied batch.

## 3. Reader visibility

**No torn batch is exposed within one iterator or one snapshot, with the configured default ordered RocksDB write behavior. Independent live reads can straddle an entire commit.**

- RocksEngine submits a native atomic `WriteBatch` (`rocks.rs:612-616`) and does not enable unordered writes in its DB options (`rocks.rs:153-168`). Its live iterator is a native RocksDB iterator with implicit point-in-time visibility (`rocks.rs:535-544`), rather than a loop issuing fresh point reads.
- `snapshot()` captures one native DB snapshot (`rocks.rs:631-635`). Point reads use that snapshot (`rocks.rs:563-568`); MultiGet explicitly sets it in ReadOptions (`rocks.rs:555-558`, `571-576`); iteration uses the snapshot object's `iterator_cf_opt`, which applies its snapshot to the supplied prefix options (`rocks.rs:580-589`). Thus prefix bounds do not discard snapshot visibility.
- MemoryEngine excludes readers while applying the whole batch (`memory.rs:114-125`, `231-238`). An iterator captures the maps under a read lock and materializes its results (`memory.rs:72-74`, `138-160`). A snapshot clones all CF map Arcs under that same read lock (`memory.rs:249-252`); later writes use copy-on-write (`memory.rs:42-48`), so they cannot mutate the captured view (`memory.rs:196-215`).
- Two separately created live iterators, or independent live `get` calls across CFs, may observe opposite sides of a commit (`rocks.rs:526-544`, `memory.rs:145-160`). That is two read views, not partial application of a batch. Use one engine snapshot for a consistent read across CFs, as the trait specifies (`engine.rs:114-115`).

## Findings

### Minor — background diagnostic may name a later error

- **Evidence:** `dashd-store/src/rocks.rs:93-108`, `258-266`; first-failure diagnostic contract at `dashd-store/src/engine.rs:87-89`.
- **Scenario:** two background errors occur before the next guarded call. The counter triggers fail-stop, but the parser scans the LOG tail backwards and returns the latest matching error. If the first error was NoSpace and the later one was a different I/O error, the first reported/latching cause can be wrong. This is the existing round-4 m1 diagnostic issue, not a failure of write refusal or batch atomicity.
- **Suggested fix:** select the earliest counted background error for the current open, using the accumulated-count suffix; preserve fail-stop even when the LOG is missing or incomplete. Add a small parser regression test with two different error kinds.

### Blocker / major

None.

## Short validation

Used existing local test executables directly, without a rebuild; their provenance was not independently re-established. Each run had `TMPDIR=/dev/shm/st1-gpt2-tmp` (0700) and a 10–15 second timeout. All five passed:

- `memory::tests::an_injected_failure_stops_all_later_writes`
- `rocks::tests::batches_record_into_the_right_column_family`
- `rocks::tests::reopen_keeps_data_and_schema`
- `rocks::tests::buffered_batches_survive_reopen`
- `refuses_writes_after_a_flush_error` (existing integration-test executable)

No disk-full/background-recovery stress jobs or new crash tests were run. The source tree under review was unchanged, and the temporary directory was removed after validation.

## Verdict

**APPROVE-WITH-NITS.** No blocker or major in the three requested engine properties. The concurrent failure window is real and permitted by the trait; the remaining minor concerns first-cause diagnostics only.
