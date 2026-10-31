# CH2b b4 (M1 NodeFlush + M2 NodeBlockFiles): Opus review, round 1

Target: `node/ch2b-b4` at `315bb6f9d`. Base: `node/integration` `7ad690b08`. Core reference: `~/workspace/dash` v24 (`v24.0.0-rc.2-109-g15db4787500`).

**Verdict: CHANGES REQUIRED.** There is one Major: a lost wakeup that b4's shared-wake park (D54) introduces into V's in-hold drain. There are also five Minors (four Core-parity or record gaps that are not D-rows, and one test gap that a surviving mutant shows) and three Nits.

Sol r1's N1–N5 (missing tests) still stand, and I don't repeat them. I disagree with Sol on two points. Q1's ordering is not sound for every production caller (Major below). And there are unrecorded divergences (m1–m4).

## Major

### M-1: V's in-hold `drain_fanout` can park forever, because b4's flush wait now takes the permit the drain depends on

- **Where:**
  - `dashd-chainstate/src/flush/driver.rs:435-460` (`wait_for_event`, which parks on `self.park`);
  - `dashd-chainstate/src/flush/site.rs:182-186` (`attach`: `park` is `FlushBell::park`, which is V's wake);
  - `dashd-chainstate/src/chain/abc.rs:347-352` (`drain_fanout`);
  - `dash-chain/src/feed.rs:479-484` and `feed/reader.rs:109-113`, where SB5 notifies the producer only once per `want_space`.
- **Mechanism:**
  - V's wake (`actor.rs:683-686`) serves four roles: the SB5 producer wake, the in-hold drain's parker, the bell's parker, and (since b4) the parker of every flush wait.
  - `drain_fanout` does `while retry_pending() == Blocked { answer_due(); wake.park(); }`. Unlike `backpressure_wait` (`control.rs:244-260`), it parks without re-checking `retry_pending` after `answer_due`.
  - Before b4, a flush wait parked on the driver's own `shared.v`, so it could not take the drain's permit. Since D54, a flush wait inside `answer_due` parks on the same `Parker`, takes its one permit, and returns without passing it on.
- **Interleaving** (fully reachable with `-dbcrashratio > 0`, i.e. `FlushPolicy::new(true)`, D14; Dash's `DEFAULT_TXINDEX` is `true`, so a Synced subscriber exists by default):
  1. Inside a hold, V steps, and `drain_fanout`'s `retry_pending` returns `Blocked`: txindex's queue is at its limit, so SB5 sets `want_space` and txindex posts `FlushDue`.
  2. `answer_due` → `flush(None)` → a DMAX `Freeze` with `wait_done = synchronous = true` → `wait_release` → `wait_for_event`. The pump retries and is still `Blocked`, so V parks on its wake.
  3. W's B1 advances `T_ack`. txindex pops, and `release_space` takes `want_space` and notifies the producer, which sets the permit. W then releases and rings V (the permit is already set, so the two coalesce).
  4. V wakes and consumes the permit. Then one of two things happens:
     - If `Released` is already queued, `wait_for_event` returns it at the top of the loop without pumping. The pop's notify is spent, and `want_space` is clear.
     - If V pumped first and the retry succeeded, the producer is no longer waiting at all.
  5. Back in `drain_fanout`, `wake.park()` has no permit and nothing left that will set one. W is idle, the subscriber has room (so no `want_space` notify), and no `FlushDue` is coming. V is inside a hold and reads no commands, so the node hangs and cannot be shut down.
- **Evidence:** I established this by reading the code, not with a reproducing test. Neither the node_stores rig (it has no subscriber) nor the watchdog test (no bell, per its module doc `subscriber/watchdog_tests.rs:10-15`) can reach it, and CRASH uses `set_pump` without a bell.
- **Fix:**
  1. Make `drain_fanout` re-check `retry_pending()` after `answer_due` and before `park()`, as `backpressure_wait` does. This is a one-line change in CH1-node's file, but b4's change of wait contract is what breaks it, so b4 should carry it or get CH1-node's sign-off.
  2. As defence in depth, make `wait_for_event` pass the baton: if it parked at least once, re-notify the park target before it returns. A spurious permit is harmless to every loop-shaped parker.
  3. Add a regression test: a hold whose fanout is blocked by a Synced toy subscriber at its limit, a synchronous policy, `ThreadDriver` with `set_bell`, and an assertion that V finishes the step.

## Minor

### m-1: The "System error" save path returns Core's null position, so V's reject state is not Core's

- **Where:** `dashd-chainstate/src/store/blocks.rs:68-78`, then `:139`, then `chain/accept.rs:280-289`.
- **Scenario:** the space query (or the blocks directory create in `Allocate`) fails, e.g. with EACCES on `blocks/`.
  - Core: the `filesystem_error` reaches `AcceptBlock`'s catch, `return AbortNode(state, "System error: " + what)` (`validation.cpp:4840-4842`). So the state is `System error: …`. `ProcessNewBlock` logs `AcceptBlock FAILED (System error: …)` (`:4890`), and `submitblock` returns that text.
  - Ours: the adapter reports `System error: {io::Error}` and returns `Ok(None)`. V's state becomes `AcceptBlock: Failed to find position to write new block to disk`, so the FAILED line and the RPC text differ.
  - The `what()` text also differs: libstdc++ gives `filesystem error: cannot get free space: … [path]`, and Rust's `io::Error` display gives `… (os error N)`.
- **Gap:** D62 claims this path is reported "as `AcceptBlock`'s System error", but it does not record either difference.
- **Fix:** report in the adapter and return `Err(source)`. V then builds `Fatal::new("System error: …")` (`accept.rs:290`), and `reported_by_v(latched=true)` stops a double report. Otherwise, record both differences in D62.

### m-2: With no tip, the adapter flushes nothing, but Core still writes the block index (D55's premise is wrong)

- **Where:** `dashd-chainstate/src/flush/site.rs:149-152` (`run` → `service_without_tip`); the D55 and D57 text ("Nothing without a tip").
- **Core:** with `Height() == -1`, `FlushStateToDisk(ALWAYS)` still runs `FlushBlockFile` and `WriteBlockIndexDB`, and so does a PERIODIC call due a write (`validation.cpp:3046-3074`). Only the coins write and `ChainStateFlushed` need a best block (`:3076`, `:3112`).
- **Scenario:** shutdown is requested between `LoadGenesisBlock` (or the devnet step's `AcceptBlock`) and the first `ConnectTip`.
  - Under DEC-241, Core's two shutdown `ForceFlushStateToDisk` calls fsync blk00000 and persist the genesis entry and the file info.
  - Ours persists neither, and the next start re-runs `LoadGenesisBlock`.
- **Second difference:** at a no-tip NONE call, Core's `FindFilesToPrune` returns early, but `m_check_for_pruning = false` still runs (`:3015`). We keep the check armed, so the first site with a tip does an extra dry run (a `BCLog::BENCHMARK` "find files to prune" line Core doesn't print).
- **Fix:** rewrite D55 to record both differences with their consequence (self-consistent, datadir state differs). Alternatively, give the no-tip path an index-only write with no locator.

### m-3: D57 overstates what `flush(mode, manual_prune_height, host)` does for PruneAndFlush

- **Where:** `dashd-chainstate/src/flush/site.rs:236-247`, `chain/flush.rs:81-98`, D57.
- **Core:** `PruneAndFlush` sets `m_check_for_pruning = true` and then runs a synchronous `FlushStateToDisk(NONE)` (`validation.cpp:3139-3145`). The selected files are unlinked before init's next `ShutdownRequested` check and step 11 (`init.cpp:2344-2362`).
- **Ours:**
  - Nothing in `flush(...)` arms the check (`PruneHost` has no arm), so M3 must call `rearm_check_for_pruning` on `GuardedFiles` itself.
  - An automatic prune's `Freeze` has `wait_done = false` (`policy.rs`: `synchronous || ALWAYS || manual`), so the call returns before W unlinks.
- **Fix:** state both facts in D57, as the contract M3 builds on. Alternatively, add an `arm_prune_check` argument and wait for NONE when the call armed it.

### m-4: BL-127's CRASH half is neither done nor recorded

- **Where:** `dashd-chainstate/src/crash/chain.rs:1714-1731`.
- **Gap:** R1 still treats a blk file lost to a failed directory sync (DEC-212) as a successful recovery. Since M2, production refuses that restart: `open_block_file` returns false, which gives `LoadingBlockDb` (`chain/load.rs:212-221`), as Core does. BL-127 names "CH2b (CRASH expectation)", but D69's deferral list doesn't mention it, and CRASH's restart doesn't go through `open_block_index`.
- **Scenario:** a crash with a failed `DirectoryCommit` loses blk0000N.dat. CRASH calls the recovery good, while the node and Core refuse to start.
- **Fix:** change CRASH's expectation for lost-blk cases to the init refusal, or add a D-row deferring it with BL-127 kept open for CH2b.

### m-5: D54's per-turn fanout pump is unpinned (the mutant survives)

- **Where:** `dashd-chainstate/src/flush/driver.rs:449` (`pump();` in `wait_for_event`).
- **Evidence:** I replaced the call with a no-op in my worktree and ran `cargo test -p dashd-chainstate --all-features --lib`: **586 passed, 0 failed.**
  - Nothing tests V's flush wait with a subscriber that needs V's pending fanout. The node_stores hub has no subscriber.
  - The watchdog schedule test runs without a bell, using `set_pump` and a 1 ms poll.
- **Also untested M2 arms:** the "System error" arm (`blocks.rs:68-78`), the rollover flush-failure arm (`:80-85`) and the `Corrupt` arm.
- **Fix:**
  - Add a node_stores test with a toy Synced subscriber that needs the step's events while V is in an ALWAYS wait. It also covers M-1.
  - Add a watchdog variant over `set_bell` and `wait_pumped`, so seeded schedules exercise the production park.
  - Add one test per untested M2 arm.

## Nit

### n-1: `set_pump` together with `set_bell` loses pops

- **Where:** `driver.rs:413-416`, `:457`, `:464`.
- **Problem:** `set_pump`'s doc says to register `v_wake()` (the driver's `shared.v`) as the producer wake. Once a bell is set, though, V parks on the bell's wake, so a pop that notifies `shared.v` doesn't end the park. No production caller combines the two today.
- **Fix:** `debug_assert!` that the two aren't both set, or make `v_wake()` return the bell's wake once one is set.

### n-2: D62's rationale misstates Core's outcome after a failed write

- **Where:** D62's rationale ("…no further write, which is Core's outcome once the abort's shutdown runs").
- **Problem:**
  - After `AbortNode("Failed to write block")`, Core's shutdown flushes still write the coins and the index (DEC-241).
  - Ours write nothing, because `GuardedFiles::mutate` latched (`store/guarded.rs:462-482`, ST3a R8).
- **Fix:** cite R8 (and BL-213/DEC-268 for NoSpace) rather than claim Core parity.

### n-3: Two wording inaccuracies in D62 and D63

- **D62 difference (1):** a failed write that isn't an open (`seq.rs:145-161`, not marked `OpenFailed`) logs `WriteBlockToDisk: OpenBlockFile failed` and `Failed to write block`, without `Unable to open file`. That matches neither Core's open failure (three lines) nor its write failure. Say so precisely.
- **D63:** it says "a fresh `BlockFileReader`", but `open_block_file` uses the reader built in `new` (`blocks.rs:36-37`, `:174-180`), which shares OsFs's handle cache. A cached handle still opens a deleted file (D67 notes this for the test rig). That matters only for an in-process reload.

## Answers to the brief's questions

1. **M1 liveness and ordering.**
   - Inside `wait_for_event`, the order is sound. Every notifier sets its state and then rings:
     - `report` pushes under the queue lock and rings after it;
     - `Ended` sets `ended` and then rings;
     - `trip` sets the `OnceLock` and then calls the registered wakes.
   - The loop checks events, `ended` and the latch, pumps, and re-checks events before parking. So no wakeup is lost *to the wait itself*.
   - The defect is the wake it shares with `drain_fanout` (M-1).
   - `register_fn`:
     - **After a trip:** it is not called. That is safe for `ThreadDriver`, which checks the latch before every park.
     - **Twice:** two entries, each called once; a drop removes one.
     - **During a drop:** serialized by the registry mutex. A ring that runs while its owner is mid-drop only touches `Arc`s (a channel `try_send` and a `Parker` notify), so it neither blocks nor touches the latch.
   - I found no lock-order inversion. Rings never take the queue, coins or files locks.
   - V outlives the flush on panic, abort and latch paths:
     - `abort(Panic)` trips the latch without the cell;
     - `ThreadDriver::drop` joins W, and every W wait is latch-registered or ends on the request's drop.
2. **M1 vs Core decision order.** `FlushPolicy::decide` matches `FlushStateToDisk`'s order, and b4 only feeds it inputs: `is_ibd` read per call (D58), `manual_prune_height` and `reindex`. The unrecorded divergences are m-2 (no tip) and m-3 (PruneAndFlush). The rest (THRESHOLD, DMAX, LEFT_IBD as a level, D56, D68) is recorded.
3. **M2 vs Core.**
   - These lines and orders match Core:
     - NoSpace → `*** Disk space is too low!` (with the user message), then `SaveBlockToDisk: FindBlockPos failed`;
     - the allocate-open failure → two `Unable to open file` lines, `WriteBlockToDisk: OpenBlockFile failed`, then `Failed to write block`;
     - the read lines;
     - the load check, in ascending file order, stopping at the first failure.
   - Gaps: m-1 (the System-error state) and n-3.
   - On restart:
     - a partial or preallocated last file loads, as in Core;
     - a truncated last record loads and fails later at `ReadBlockFromDisk`, as in Core;
     - a missing rev file passes the load, as in Core (the undo read fails later).
   - Removing the blanket refusal is safe in production. CRASH's model of the lost-file case is m-4.
4. **AbortCause (D59).** Tripping only on V's panic and leaving the store open on AbortNode matches DEC-241 and BL-202's ruling. Two cautions:
   - After a failed blk write, the store is latched anyway (n-2), so the shutdown flushes refuse, unlike Core's.
   - V's AbortNode exit waits for W's release with the latch clear. That relies on every W wait finishing (Sol N3's in-flight test is the right pin).
5. **Tests and mutants.** Missing: M-1's regression, a subscriber on the bell path (m-5, mutant survives), the untested M2 arms (m-5), CRASH's lost-blk expectation (m-4), plus Sol's N1–N5.
6. **Anything else.** n-1 (latent `set_pump`/`set_bell` hazard). I did not run `git diff --check` myself; Sol r1 reports it clean.

## Validation (my worktree, `CARGO_TARGET_DIR` under `/work/scratch/ch2b-b4-r1-opus`, `TMPDIR` 0700, `umask 077`)

- `cargo test -p dashd-chainstate --all-features node_stores`: 12 passed.
- `cargo test -p dashd-chainstate --all-features flush::control::tests`: 30 passed. `cargo test -p dash-kv --all-features latch::`: passed.
- Hand mutant (`driver.rs:449` `pump();` removed): `cargo test -p dashd-chainstate --all-features --lib` gave 586 passed, 0 failed (the mutant survives). I reverted it, and the worktree was clean at 315bb6f9d before removal.
- No commits, no pushes, the owner's worktree untouched, no processes killed.
