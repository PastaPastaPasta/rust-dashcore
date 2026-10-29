# CH2b b4 (M1 NodeFlush + M2 NodeBlockFiles) — round 1 review

Target: `node/ch2b-b4` at `315bb6f9d`  
Base: `node/integration` at `7ad690b08`  
Scope: M1 `NodeFlush`, M2 `NodeBlockFiles`  
Verdict: **ACCEPT WITH NITS**

I found no Blocker, Major, or unrecorded functional divergence from the requested Core behavior. The two material M2 differences and the M1 space-check timing difference are already recorded in D62 and D68. The review worktree remained unmodified.

## Findings

### Nit N1 — latch callback edge semantics need direct tests and clearer API wording

- **Location:** `dash-kv/src/latch.rs:166-179`, `:208-215`
- **Scenario:** `register_fn` does not invoke a callback when registration happens after the latch has already tripped. That is safe for the current `ThreadDriver` caller because `wait_for_event` checks the latch before every park (`dashd-chainstate/src/flush/driver.rs:435-460`). Duplicate registrations are independent entries and call the same function once per registration; dropping one removes one entry. A registration racing with `trip` or another registration is serialized by the waker mutex. The callback runs under that mutex, so a callback that touches the latch or drops its own registration would deadlock; the documented callback contract forbids that, and the production callback only rings `FlushBell`.
- **Suggested fix:** Add tests for register-after-trip, duplicate function registrations, and concurrent registration/drop/trip, and document explicitly that late registration relies on the caller's post-registration latch check (or make late registration invoke the callback immediately).

### Nit N2 — W end/panic while V is parked is not pinned by a production-driver test

- **Location:** `dashd-chainstate/src/flush/driver.rs:338-364`, `:435-460`
- **Scenario:** If W ends or panics while V is parked on `FlushBell::park`, `Ended::drop` marks `ended`, trips the latch on panic, and rings both the parker and bell. The wait loop rechecks queued events, `ended`, and the latch before parking again, so the wake ordering is sound. The existing node test covers a latch trip while W is held in a store call, but not W's own end/panic path with V already in the bell wait.
- **Suggested fix:** Add deterministic tests for a normal close and a W panic while V is in `wait_release`; assert V wakes, does not hang, and reports the expected fatal path.

### Nit N3 — the actual in-flight `AbortNode` shutdown-flush path is not tested

- **Location:** `dashd-chainstate/src/chain/actor.rs:1024-1057`, `dashd-chainstate/src/flush/site.rs:257-265`
- **Scenario:** `AbortCause::AbortNode` intentionally leaves the latch clear so the shutdown flushes can run. The current node test (`chain/tests/node_stores.rs:644-655`) raises an immediate `Fatal::new` with no job in flight; it does not exercise an abort while W is waiting on a prune clear, subscriber gate/ack, or a store step. The implementation's shutdown path is consistent with D59/DEC-241, but this is the path most likely to regress into either an early W drop or an incorrectly latched store.
- **Suggested fix:** Hold W at a deterministic in-flight wait, make V take an `AbortNode`, then assert the shutdown flush completes (or returns the already-reported failure), the latch remains clear when appropriate, and W is joined without a hang.

### Nit N4 — restart edge cases for truncated blk records and missing rev files lack integration coverage

- **Location:** `dashd-chainstate/src/store/blocks.rs:174-180`; `dashd-store/src/flatfile/blocks.rs:925-945`; restart test at `dashd-chainstate/src/chain/tests/node_stores.rs:512-554`
- **Scenario:** Core's `LoadBlockIndexDB` checks only the presence/openability of blk files. Therefore a restart with a truncated last blk record should load the index and fail later when `ReadBlockFromDisk` reads that record; a missing rev file should likewise pass startup and fail only on a later undo read. The implementation follows that behavior: `open_block_file` only opens blk files, and `read_sized` rejects a record past EOF. The current restart test covers a complete restart and a missing blk file, but not these two cases.
- **Suggested fix:** Add regressions that restart with a truncated final blk record and separately remove a referenced rev file, asserting startup behavior and the later Core read/open diagnostic.

### Nit N5 — `FlushSite::flush` manual-prune routing is covered at the control seam, not through NodeFlush

- **Location:** `dashd-chainstate/src/flush/site.rs:236-247`; policy handling at `dashd-chainstate/src/flush/policy.rs:254-285`
- **Scenario:** `NodeFlush::flush` installs `manual_prune_height` into the call context and invokes `FlushControl::flush`; the control tests cover manual pruning and waiting/unlink ordering, while the node-store integration tests cover `flush_now(Always)`. There is no direct NodeFlush/chain command test proving the manual height survives the host/context boundary, waits for W's release, and hands the resulting `ChainStateFlushed`/prune effects back to V.
- **Suggested fix:** Add one production-driver integration test invoking `FlushSite::flush(FlushStateMode::None, Some(height), ...)` through the node command path, with a small block-file set and assertions for selected/unlinked files and completion ordering.

## Answers to the brief's questions

### 1. M1 liveness and ordering

The ordering is sound for the current production callers. `ThreadDriver::wait_for_event` first drains an event and checks `ended`, then checks the latch, runs both fanout pumps, rechecks the event queue, and only then parks (`driver.rs:435-460`). W rings for every event, including `Released`, and on thread end (`driver.rs:315-364`). `FlushBell` uses both a bounded one-slot ring and a permit-bearing `Parker` (`chain/flush.rs:184-216`), so an event or latch trip between the final queue check and the park is retained. `Latch::register_fn` rings the same bell on a trip. `PruneClears` is handled in the wait loop before V can park again (`control.rs:619-629`), so W is not left waiting for V's answer.

`register_fn` is safe in this usage even when the latch has already tripped: registration and trip/drop are mutex-serialized, and the wait loop checks the latch after registration and before parking. It is not an immediate-callback API for late registrations, and duplicate registrations are separate callbacks; those semantics should be tested/documented as N1. A callback must not block or touch the latch because it runs under the registry lock.

The host lifetime is also correct. `NodeFlush::with_cx` hands released tips back to the current `FlushHost` on both success and error returns, and the W job never retains that borrowed host. Calls that Core makes synchronous wait for their release; asynchronous causes are serviced by later V calls while V remains alive. Panic/stop paths construct a fresh host after the original call unwinds. `AbortCause::Panic` trips the shared latch without taking the possibly poisoned coins cell; `AbortCause::AbortNode` leaves it clear for shutdown flushes, as D59 requires.

### 2. M1 versus Core policy

`FlushPolicy::decide` reproduces Core's mode and trigger order: first-call timer initialization, IF_NEEDED/PERIODIC/ALWAYS causes, cache critical/large thresholds, the 64 MiB Evo threshold, periodic write/flush intervals, pending causes while a job is in flight, no-best-block index-only writes, the ALWAYS clean-at-durable shortcut, and automatic/manual prune selection. The strict `now > deadline` tests and the policy matrix are in `flush/policy/tests.rs:109-288`.

The known differences are recorded: D56 (`NoInputs` supplies no mempool/reindex yet), D68 (space checking remains on W and latches after freeze), and the earlier CH2 design deviations for `THRESHOLD`, `DMAX`, `LEFT_IBD`, and `SCRIPTED`. `last_flush` advances at decision/freeze time as explicitly accepted by the earlier CH2 design notes. I found no additional divergence in this diff.

### 3. M2 versus Core block files

The adapter reports Core's no-space and write-failure lines, invokes the abort sink once, and returns `Ok(None)` for a failed save, which is Core's null `FlatFilePos` (`store/blocks.rs:46-97`, `:117-142`). It opens blk files through a zero-length read, reports the Core open line, checks framing/record bounds, decodes, checks PoW, then compares the resulting hash (`store/blocks.rs:144-180`; `dashd-store/src/flatfile/blocks.rs:925-945`). The node-store tests pin the report strings and levels (`chain/tests/node_stores.rs:387-486`).

Restart checks only the blk files, matching `LoadBlockIndexDB`; it does not validate record contents or require rev files at startup. Truncated records and missing rev files therefore fail later on the corresponding read, as Core does. Removing BL-127's blanket refusal is safe on the normal restart paths; N4 names the untested edge regressions.

D62 records the two material adapter differences: ST2 cannot distinguish a failed write from a failed open, so failed writes use Core's open-failure report; and a rollover flush error latches/stops ST2 before writing the block, whereas Core reports the error and continues that write. D66 records the expected hash-based mismatch wording.

### 4. AbortCause merge

The merge is consistent with shutdown and the CRASH model. A V-owned `AbortNode` is reported, leaves the latch clear, and then `V::stop_flush_at_exit(true)` performs the orderly stop/shutdown flush. A store or W failure that already latched and reported returns `true` from the abort hook, so no second report or futile shutdown write is attempted. A V panic trips the latch without taking the coins cell; W is woken and refuses subsequent writes. The W crash path exits before ordinary shutdown work, matching the simulated `_Exit` model.

### 5. Tests and mutants

The new behavior is substantially pinned: 12 `node_stores` tests pass, including ALWAYS durability, `Released` waking an idle V, a trip waking V while W is inside a store call, both abort causes, restart/lost-blk behavior, and all principal M2 report lines. The latch tests pass 4/4, and the flat-file tests pass 38/38 (one ignored). The owner evidence also reports the fast gate, CRASH replay/smoke, and latch/store mutation runs clean.

The missing cases are the five Nits above, especially late `register_fn` registration/duplicate/drop races, W normal end/panic while V is bell-parked, an actual in-flight AbortNode followed by shutdown flush, truncated blk/missing rev restart behavior, and direct NodeFlush manual-prune routing. No mutant or test result exposed a functional failure in the reviewed range.

### 6. Other review

`git diff --check 7ad690b08 315bb6f9d` is clean. No further correctness, liveness, report-line, or consensus/security issue was found.

## Validation

- `CARGO_TARGET_DIR=/work/scratch/ch2b-b4-r1-sol/target cargo test -p dashd-chainstate --all-features chain::tests::node_stores -- --test-threads=1`: **12 passed**.
- `CARGO_TARGET_DIR=/work/scratch/ch2b-b4-r1-sol/target cargo test -p dash-kv --all-features latch:: -- --test-threads=1`: **4 passed**.
- `CARGO_TARGET_DIR=/work/scratch/ch2b-b4-r1-sol/target cargo test -p dashd-store --all-features flatfile::`: **38 passed, 1 ignored**.
- `git diff --check`: passed.
