REWORK

Focused closure check of rev1 at `06b4a92b`, byte-identical to the reviewed file (SHA-256 `12dd0562f6596857c6ce01cacc0115be2f42b063fdb8c07bb80c4781a98b0fd9`). Read both r1 reviews, their retained evidence, REVIEW-COMMON, DEC-62 and DEC-63. “Closed” below means closed at design level; ST3a is not implemented. Only this review and private scratch were written; no design/source edits, commits, pushes, posts or daemon access.

**Q14 independently confirmed.** RocksDB 11.8.1 appends at `db_impl_write.cc:2343`, then syncs at `:2350–2387`. My Env wrapper stopped a real `sync=true` finalizing Write at WAL `Sync()` entry, before it returned. Zeroing only the earlier, unsynced 32 KiB coin record in a private copy yielded `tip=new`, marker absent, coin absent. An intact control retained the coin. With a completed pre-final `FlushWAL(true)`, retaining or losing the finalizer retained the dependencies. These are persisted-image recovery experiments, not physical power cuts. Evidence: `/work/scratch/st3a-r2-gpt/wal_finalize{.cc,_cases.py}`, `wal-finalize-cases.log`.

The barrier is sufficient **for dependency writes that returned before it starts**, on the same database, with WAL enabled and A-DEV/A-DUR holding. `SyncWalImpl` snapshots the current WAL number, pins/syncs all live WALs through it, and syncs the WAL directory (`db_impl.cc:2051–2083,2094–2145`). Already retired WALs have synced/SST protection. `atomic_flush` preserves cross-CF atomicity; it does not supply the missing pre-final barrier. A second native probe retained five live WALs, joined eight concurrent writers (one merged record with 16 operations), completed the barrier, and interrupted the finalizer: all 24 cross-CF dependency values recovered. Evidence: `wal_multi.cc`, `wal-multi-{write,read}.log`, `rocksdb-source-evidence.txt` in that scratch directory.

1. **Major — the prune acceptance scenarios cannot reach their advertised failures.**

   **Location:** `designs/ST3a-power-loss.md:790,797,822–824,841–855`; §8.2 S5 costs.

   **Counterexample:** running ST2 `365aa8cb` with exactly S5's 70 × 1 KiB blocks and `fastprune` puts heights 1–63 in file 0 (`size=65016`), not 1–55. Its last height 63 exceeds `70−K=62`; selection is empty. S5/S5b therefore do not exercise T5/T11/T13/T19. Independently, the mandatory subscriber's durable locator is at or behind the old acknowledged tip 5. Q10 forbids deleting any of 6–70, so increasing the block count alone still cannot make the stated replay-path overlap occur. The correct lock also masks T15. Even without that lock, S5c's V=86 has no completed file containing blocks above frozen tip 70 that the wrong bound 78 could delete; T15's **next-flush** replay failure is unreachable. Executed evidence: `prune-sizing.log`, scratch `calib/src/main.rs`; the lock argument is in `closure-probes.log`.

   **Fix:** give isolated prune/replay tests an explicit subscriber-free toy configuration, keeping Q10 and DEC-63's mandatory set in subscriber tests and production. Choose actual file boundaries and enough V-ahead blocks to satisfy `frozen_tip < selected_file.height_last <= V_tip−K`. Assert a nonempty selected set and the required replay-path intersection before searching. Keep correct-lock and in-memory-lock variants for S11, assert that their selections differ, and recalculate the affected work/budget rows.

2. **Major — T18 labels a now-safe no-prune variant as a corruption bug.**

   **Location:** `designs/ST3a-power-loss.md:272,800,854,875,918`.

   **Counterexample:** change only Bk to Buffered, retaining the new pre-Bk barrier. Every preceding coin batch is already durable. If Bk survives, its last coins, tip and marker erase survive atomically; if it vanishes, the durable marker causes replay. R3 cannot fire. Rev1 explicitly says the finalizer may be Buffered at line 272, then requires this variant to fail. An early acknowledgement can violate R4, and an early unlink can violate prune safety; neither produces the stated no-prune R3 witness.

   **Fix:** restore this no-prune variant as a negative control with an appropriate acknowledgement policy, or define T18 as **both** removing the pre-barrier and buffering Bk. Keep T26 as the distinct Durable-finalizer-without-pre-barrier case. Update W1, repro counts and dispositions accordingly.

3. **Major — R5's supersession rule conflicts with a surviving in-flight Bk.**

   **Location:** `designs/ST3a-power-loss.md:637`, against `:220,245,798–801`.

   **Counterexample:** complete B1 with a pre-prune index record containing `HAVE_DATA` and its position; complete the pre-Bk barrier; append Bk's atomic tip/marker/prune-clear changes; crash before Bk's barrier. Bk may survive legitimately. The recovered index now contains the cleared version, although no later **completed** durable batch changed B1's version. Read literally as record preservation, R5 rejects this correct state. Reading “present” as mere key existence instead leaves its value-checking obligation unspecified.

   **Fix:** define R5 against the completed-barrier base **overlaid in WAL order with the surviving pending batches**, permitting supersession by retained Buffered or in-flight Durable batches. Add a clean prune case at Bk's Append with its clears retained; require R5 to pass and still detect genuine loss of an unsuperseded protected record.

4. **Minor — W1 cannot be “not caught” while D1(f) remains unconditional.**

   **Location:** `designs/ST3a-power-loss.md:278,695,715,918`.

   **Counterexample:** under W1, run T26 with pending coin batches and append Bk without the pre-barrier. Prefix recovery removes the R3 crash witness, but D1(f) reports the missing barrier immediately, without any crash or survival decision. T22/T27 have the same contradiction.

   **Fix:** say W1 hides the **post-crash state violations**, while D1's production-policy diagnostic remains expected, or explicitly make the dependency policy conditional on the granted prefix contract in that witness run. Do not require an empty report under the current unconditional checker.

5. **Minor — the concurrent barrier precondition needs to be normative.**

   **Location:** `designs/ST3a-power-loss.md:211,272,742,751`.

   **Counterexample:** start a dependency Write on another thread, call `sync_wal` while its WAL append is blocked, then let it return and append the finalizer. RocksDB deliberately permits the sync to finish before that Write (`db_wal_test.cc:346–370`, `SyncWALNotWaitWrite`). WAL serialization order alone does not prove that the preceding barrier covered it. The single-thread harness avoids this, but CH2's multithreaded contract needs the same condition.

   **Fix:** require all dependency writes to return before starting the pre-final barrier, and await its success before appending the finalizer; equivalent explicit serialization is acceptable. State that this barrier covers that completed-write boundary, not every write earlier than the finalizer by the time the sync returns.

**GPT r1 closure (1–9):**

| # | Status | Evidence |
|---|---|---|
| 1 | Closed | §2.4 subset domain, atomic-group overapproximation, Append/Barrier split; rerun `wal_cases.py` confirms zero-hole skipping in all four modes; Q14 native probes above. New checker/acceptance issues are findings 2–5. |
| 2 | Closed | §2.2 independent operation bits admits `{b}`; a rename shares one bit across its two names; M5 and ST3b case specified. `closure_probes.py` checks these domains. |
| 3 | Closed | Shrink/Zero sector histories, ZeroFill allocation, copy-over and M3 now include the excluded zeros. Ordinary truncate/re-extend and copy-over controls passed, including existing-inode preservation. |
| 4 | Partial | Pre-prune versions in B1, clears/flag in Bk, successful-Bk-before-unlink and repaired startup ordering close the protocol counterexample. Required witnesses remain unreachable: finding 1. |
| 5 | Closed | R6(a–e), Connected from validating-connect success, assumevalid evidence, T20/T21 and CH2 observer seam address the valid-but-unvalidated marker counterexample. |
| 6 | Closed | §2.1 never promotes ProcessKill; L4 retains provenance; R7a projects a fixed durable image, R7b checks the mixed-kind allowed floor; S12/N5 retain unread effects/appends. |
| 7 | Partial | Exhaustive/witness split, small PR records, zero-truncation assertion and byte/history/clone/rerun accounting close the original search-size gap. Budget inventory needs reachable prune cases: finding 1. |
| 8 | Closed | Fatal set, advisory allocate, BestEffort regions, N6/N7 distinguish API outcomes; ST2 `blocks.rs:664–693` supports the exclusion. |
| 9 | Closed | §4.7 fixed-point decision simplification, final one-loss checks and best-effort flag; rerun original counterexample and corrected loop reaches `(0,0)`. |

**Opus r1 major closure (1–7):**

| # | Status | Evidence |
|---|---|---|
| 1 | Partial | Engine domain and new barrier rule verified; T18/W1 acceptance and R5 still contradict them: findings 2–4. |
| 2 | Closed | §7.2 correctly distinguishes both SIGKILL windows from `feature_dbcrash`'s limited coverage; T24 covers the coins→EvoDB order. Prune witness reachability is separately finding 1. |
| 3 | Partial | Frozen-tip selection, durable-locator locks, Bk flag and two dirty-info takes address all four protocol gaps. S5/S5c/S11 need reachable, discriminating selections: finding 1. |
| 4 | Closed | R1 position-within-recorded-size, D1(d) immutable referenced bytes, T14 and R12's repeated full checks address dropped file-info records. |
| 5 | Closed | L1–L6 explicitly force or preserve every dependency, retain each Var across epochs, and extend M10 to mutation/nested observation traces. Prior adaptive DFS/eager comparison reran successfully. |
| 6 | Partial | Component calibration with Q11 is supported; minimisation moved nightly; toy K, twin and witnesses are budgeted. Correct the scenario/count contradictions before relying on the totals. |
| 7 | Closed | Fixed salt/store ordering, coins-only profile, complete transactional digest scope, Meta fields, V-ahead seam and validation observer replace every listed contract gap. |

**Budget check:** the listed PR clean rows sum to 16.15 CPU-s (rounded 16.2); the full table sums to **30.8**. Nightly rows sum to **1,843 s = 0.512 CPU-h**. The retained calibration logs support the Q11 component timings. These are arithmetically sound forecasts, not measured full-harness runtimes. Re-measure the repaired scenario matrix after ToyChain/nested recovery exist; Phase B precedes those implementations. Evidence: `budget-arithmetic.txt`, original `calib{,-debug-overrides}.log`.

Summary: Q14 is correct, and the principal storage, provenance, lazy-resolution and mixed-crash repairs close their original counterexamples. Rev1 still specifies unreachable prune witnesses and conflicting acceptance/invariant rules; resolve those before implementation approval. **Final verdict: REWORK.**
