# CH2b b1 — Opus high review

**Head reviewed:** `ba03deb86` on `node/ch2b-flush` (base `96f9b2cc7`). **Core reference:** v24.0.0-rc.3 (tag `da567d7d`, commit `0364ed626`). **Scope:** the b1 series 0ccac2be7..ba03deb86 (the stopgap, errata, FlushPolicy, FlushMachine, drivers, dashd-store `OpenFailed`, recovery, PF0 rows, the DEC-180 docs row). Commit 6 (the ST3a CRASH suite) is out of scope, as in Sol r1/r2. Closed items from Sol r1/r2 (R1-1, R2-1) were rechecked, not reopened.

The review was done in a detached scratch worktree. The branch was not edited.

No blocker or major. The flush semantics, marker protocol, recovery order and failure paths match Core, or are covered by registered rows. The findings are test-strength gaps that mutation testing exposed and two always-on Core log lines that are missing, plus nits.

## Findings

### Minor

**m1. The commit kind and barriers that make flush and recovery crash-safe have no test until commit 6.**
- *Location:*
  - `flush/machine.rs:645` (Bk / one-batch final `Commit::Durable`);
  - `recovery/replay.rs:276` (pre-final `sync_wal`);
  - `recovery/replay.rs:287` (recovery's final `Commit::Durable`).
- *Evidence:* these three mutants all survive the full lib suite (`--all-features`):
  - M1: the final write is `Buffered`;
  - M12: recovery's final write is `Buffered`;
  - M13: recovery's pre-final `sync_wal` is skipped.

  By contrast, the split-B1 kind (M3) and the pre-Bk barrier (M2) are killed.
- *Failure scenario:* say a refactor makes Bk `Buffered`. With an all-Buffered tail, a power loss after `Released` (the `ChainStateFlushed` hand-over) can keep B1's marker but lose Bk.
  - On a split flush, recovery replays to `new`, so the result is correct but needs the replay. If the WAL keeps only a non-prefix subset, that's covered by the pre-Bk barrier.
  - On a one-batch flush, B1 *is* the final write. With M1 the whole flush may vanish after V has released the window and the signal was sent, which breaks DEC-166's "lags, never leads" promise.

  Recovery without M12/M13 can leave a converged image that isn't durable before the node continues. The suite would stay green either way.
- *Fix:* b1 merges before commit 6, which waits for ST3a's CP3.4. So add two cheap `MemoryEngine` tests now, rather than leaving these to the CRASH suite:
  1. Run one-batch and split flushes to `Released`, then call `simulate_power_loss(0)`. Assert `tip == new` with no `head_blocks`.
  2. Run recovery to completion, then call `simulate_power_loss(0)`. Assert the converged tip and an empty marker. Also assert recovery's `sync_wal` count, like `flush-split`'s barrier count.

  These kill M1, M12 and M13.

**m2. Four policy and control behaviours survive mutation.**
- *Location:*
  - `flush/control.rs:272` (`clean_at_durable`);
  - `flush/control.rs:372` (and the height-distance clause after it);
  - `flush/control.rs:430` (`policy.written`).
- *Evidence:* these four mutants survive:
  - **M6:** the ALWAYS shortcut ignores whether best == durable == cx.tip.
  - **M7:** `check_pair` ignores `tip_seq`.
  - **M8:** `check_pair` ignores the ≤ 1 height distance.
  - **M9:** `last_write` is not advanced at release.
- *Failure scenarios:*
  - **M9:** after the first flush, `last_write` stays at the first call's time. From then on, every PERIODIC call after an hour satisfies `fPeriodicWrite` (`validation.cpp:3042`; policy.rs:178), so the index is rewritten at every block. That is a flush storm Core doesn't have (Core sets `m_last_write = nNow` after the write, `:3075`).
  - **M6:** a block that leaves the active layer clean is one whose outputs are all unspendable and that spends nothing. An ALWAYS call at it would take the index-only shortcut, and the stored tip would trail the chain. Core writes BEST on every ALWAYS flush.
  - **M7/M8:** a mislabelled packet would pass the pairing check.
- *Fix:* add one test per mutant:
  - **M9:** flush, advance the clock 1 h + ε, and assert PERIODIC does *not* write until a further hour passes.
  - **M6:** an ALWAYS at a tip that moved with a clean active layer must do a full flush.
  - **M7/M8:** a packet with a stale seq or a two-block gap must fail `check_pair`.

  If the manager rules M7/M8's checks redundant (focus 6: no stricter invariants than Core), drop them instead of testing them.

**m3. Two unconditional Core log lines are missing.**
- *Location:* the policy at CRITICAL, and W's coins/evo write in `flush/machine.rs`.
- *Evidence:*
  - `validation.cpp:2961`: `LogPrintf("Cache size (%s) exceeds total space (%s)\n", …)` is unconditional (not a category line). CH2 §3.3 (design line 947) and the log table (line 1789) promise it. Nothing on the branch emits it (`git grep "exceeds total space"` hits only the design).
  - `validation.cpp:3096`: `LOG_TIME_SECONDS("write evodb cache to disk")` prints `FlushStateToDisk: write evodb cache to disk started` / `… completed (%.2fs)` on every full flush. W commits the evo tip with the coins (recovery checks `evo_tip == new`), but no such line is emitted, and the design doesn't register one.
- *Failure scenario:* a log-parity check against Core's debug.log on any full flush, or on a CRITICAL cache, is missing lines that Core always prints.
- *Fix:* emit both through `FlushObserver`, using `BenchTimer` for the evodb pair at the step that commits the evo window. Or register a D-row if the evo commit's timing makes the pair meaningless here.

**m4. Not all of §7.2 b1's acceptance items are in the series, and no deferral is recorded for those that aren't.**
- *Location:* CH2-coins-flush.md:2499 (b1 row).
- *Evidence:*
  - There is no `coins_replay_blind` smoke target or test.
  - FA7 (§6.4, line 2405) is not asserted anywhere.
  - D14's smoke is specified as "O-R6-M1's scenario fails with the hook off and passes with it on". But `flush/control/tests.rs:470` `dbcrashratio_waits_inside_freeze_after_step` checks only that the wait happens.

  The commit messages and Sol r1/r2 scope commit 6 out but don't list these items, and some of them (the smoke, D14) aren't CRASH-suite items.
- *Failure scenario:* b1 merges, and the b1 row is read as satisfied, with these items untracked.
- *Fix:* add them to commit 6's recorded scope (or a ledger row) before the merge, or add the D14 hook-off/hook-on pair and the replay smoke now.

### Nits

- **n1.** `flush/machine.rs:662`: `{kb:.2}kB` prints `4567.00kB`. Core passes an integer (`coins_mem_usage / 1000`) to `%.2f`, and tinyformat prints it as an integer, `4567kB` (checked with a tinyformat probe). Format an integer.
- **n2.** `flush/machine.rs:426/456` and `:527`: B1's coins and the last sub-batch's coins are encoded twice: once to decide whether the chunk is full or the flush splits, then again to write it. That is at most S extra bytes encoded per flush. The PF0 rows include it, so it is only an efficiency nit.
- **n3.** `flush/driver.rs:410-418`: `ThreadDriver::drop` panics with "W's flush thread panicked" after W's panic has already tripped the latch and reported to the abort sink. A second panic during the orderly shutdown that follows adds noise. Consider logging instead.
- **n4.** `benches/coins_flush.rs:859/882`: `site_and_step` calls `poll` after the overlay commit but before the tip step. That isn't a block boundary production polls at. It is harmless for the counters but misleading as a model.
- **n5.** `benches/coins_flush.rs:1157`: the comment "The flushes the replay started end inside the region" reads badly, and `flush_memtables()` is a call production never makes. Say that the row forces the memtables out so the region includes the write cost. The manager has separately asked for the recovery row's allocation comment in workloads.toml to be fixed.
- **n6.** `recovery/replay.rs:380-383`: with no marker, the evo check runs before the tip lookup, so a tip that is both unknown and evo-mismatched reports EvoInconsistent. Core reaches the unknown-tip failure first. This only happens on a corrupt store and both fail the load, so it is just text order.

## Confirmations

- **Flush semantics (focus 1).** `FlushPolicy::decide` matches FlushStateToDisk (`validation.cpp:2969-3125`):
  - Modes and causes: fCacheLarge (PERIODIC), fCacheCritical and fEvoDbCacheCritical (64 MiB, IF_NEEDED), fPeriodicWrite (1 h), fPeriodicFlush (24 h) and prune.
  - With no best block, only the index is written (`:3078`).
  - `m_last_write`/`m_last_flush` start at the first call.
  - `ChainStateFlushed` is sent after a full flush, handed over at release (DEC-166).
  - In flight, WAIT causes and manual prune wait, and the rest stay pending.

  The marker protocol mirrors `CCoinsViewDB::BatchWrite` (`txdb.cpp:113-174`). The first batch erases BEST and writes HEAD = [new, old]. The batches in between are cut above `batch_size`, with the `-dbcrashratio` roll after each. The final batch erases HEAD and writes BEST.

  The two extra barriers are both needed under RocksDB's non-prefix WAL recovery: the Durable B1, and the pre-Bk `sync_wal` (M2 and M3 are killed). Disk checks match CheckDiskSpace (MIN_DISK_SPACE + 192·count) and Core's strings.
- **ReplayBlocks (focus 2).** The checks run in Core's order: unknown new, unknown old, LCA, evo == new, then the HAVE_DATA pre-check (D-b1-g).
  - The replay is blind: the undo leg goes in reverse tx order and restores with the coin-height check, then the forward leg runs. It converges from any surviving subset, including duplicate txids across branches.
  - Each block read decodes, checks PoW on the decoded nBits, then checks the hash (R1-1 closed). The undo checksum is over the index parent.
  - `FileRead::line` gives Core's texts. Open vs read is told apart by the `OpenFailed` stage (R2-1 closed).
  - Quirks 43 and 44 are registered and tested. The `head_blocks` length rule follows the manager's ruling.
  - The mutants on order (M11), input erases (M15), the open barrier (M16), the evo check (M17), HAVE_DATA (M18), coin height (M19), wiped CFs (M20) and the fork-block boundary (M26) are all killed.
- **Concurrency and shutdown (focus 3).**
  - `run_step` checks the latch first.
  - The `GlobalFailStop` latch and Parker registry have no lost wake. M24 (V not registered) is killed by `a_trip_elsewhere_wakes_v_waiting_for_w`.
  - W's crash path calls `on_crash`, then trips, then reports. The `Ended` drop guard trips and aborts on a panic.
  - A ReadOnly refusal is reported as refused, not as this call's failure.
  - There is no `unwrap`/`expect` on disk-derived data. The one `expect` in replay.rs is on the infallible encode path.
- **PF0 (focus 4).** The rows are honest:
  - Every step waits idle under the quiet RocksDB config (D-b1-i).
  - The shutdown row really flushes an unflushed 512 MiB×2 memtable, with no charged WBM.
  - `TablesAfter(chainstate, 2)` guards against a silently dropped flush.
  - The split row's per-step idle wait and the recovery row's forced memtable flush are bench choices, documented as such (n5).
  - The DEC-164 stall-retry path is not exercised deterministically, as documented.
- **Simplification (focus 6).** No stricter-than-Core invariant was found in the flush or recovery logic. The open question is M7/M8's pair checks (m2).
- **Security.** No possible Core security issue was found.

## Validation run

Setup: a detached scratch worktree at `ba03deb86`, with `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=` and an on-disk `TMPDIR`. Load was 70–190.

- **Baseline:** `cargo test -p dashd-chainstate --all-features -- --test-threads=4` gave 184 unit tests passed, plus 2 quirk tests and 2 doctests passed. No flakes were seen.
- **Mutation testing:** 25 env-switched mutants (M1–M26; M14 was dropped) (`CH2B_MUT=Mn`) were run against the lib's `flush::`, `recovery::` and `store::` tests. Each mutant build was checked to compile before its result was counted. The suites selected for the last batch (82 tests) passed clean with no mutant set.

| Result | Mutants |
|---|---|
| Killed (18) | M2 pre-Bk barrier; M3 split-B1 Buffered; M4 marker without old; M5 space ignores entries; M10 signal before B1; M11 undo order; M15 forward input erases; M16 open sync_wal; M17 evo at a marker; M18 HAVE_DATA; M19 coin height 0; M20 wiped CF accepted; M21 ALWAYS shortcut with other causes; M22 pending dropped; M23 last_flush not set; M24 V not registered with the latch; M25 CRITICAL in flight doesn't wait; M26 undo leg disconnects the fork block |
| **Survived (7)** | **M1** final/one-batch Buffered; **M12** recovery final Buffered; **M13** recovery pre-final sync_wal skipped (m1); **M6** ALWAYS shortcut ignores tip movement; **M7** check_pair ignores seq; **M8** check_pair ignores distance; **M9** last_write not advanced (m2) |

- A tinyformat probe confirmed n1.
- Core lines were checked at `0364ed626`: `validation.cpp:2961` and `:3096` (m3).
- Not run: the workspace gate, the PF0 perf gate, the ST3a CRASH suite and dashd integration tests (no SPV code changed).

**Verdict:** APPROVE-WITH-NITS
