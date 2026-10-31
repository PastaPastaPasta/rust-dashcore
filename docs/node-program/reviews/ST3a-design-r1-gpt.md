REWORK

Independent STATE design review of `designs/ST3a-power-loss.md` rev0, byte-identical to commit `483137a1` (SHA-256 `49694c67180e41c03558be60df1717c856eada52fce665f2e97c9c76d73f60d4`). Read REVIEW-COMMON, DEC-59, BL-32, the requested PLAN/A1 context, ST1 on `node/integration`, ST2 at `0304cada`, and ST2's GPT round-2/round-3 reviews. Core citations below are from `git show` at the two requested tags, not the checkout's current source. No design/source edits, commits, pushes, GitHub posts, or daemon/oracle access. Experiments and source extracts are under `/work/scratch/st3a-review-gpt/`; tests used mode-700 `/dev/shm/st3a-gpt-tmp`.

1. **Blocker — the configured RocksDB does not guarantee the prefix that the model assumes.**

   **Location:** `designs/ST3a-power-loss.md:18`, `:157–170`, `:181`; A1 `:1835`; `dashd-store/src/rocks.rs:163–164` on `node/integration`.

   **Concrete counterexample, independently executed:** using the pinned RocksDB **11.8.1**, `atomic_flush=true`, `paranoid_checks=true`, WAL enabled, and `kPointInTimeRecovery`, write one baseline batch with `sync=true`, then five batches with `sync=false`. Make each logical WAL record exactly one 32 KiB log block. Terminate with `_exit`, without flushing memtables. In a private copy of that database, replace the first buffered record's 32 KiB region with zeros and retain the later records and file length. This represents an unsynced extension whose earlier pages remain holes/unwritten extents while later pages and the length survive, permitted by the weak write-back behavior the design claims to cover; no synced baseline byte is changed.

   Reopen succeeds and returns:

   ```text
   RocksDB 11.8.1 open=OK
   base=OK
   key0=NotFound
   key1=OK
   key2=OK
   key3=OK
   key4=OK
   ```

   This is not any prefix of the five buffered batches. `FaultyEngine` cannot produce it. `atomic_flush` cannot help: this reproducer has no SST flush at all. Evidence: `wal_probe.cc`, `wal_cases.py`, `wal-aligned-cases.log`, and the intact control in `wal-control.log`. This is an edited persisted-image recovery experiment, not a claim to have performed a physical power cut.

   The reason is exact: bundled `rocksdb/db/log_reader.cc:611–618` clears the input block and returns `kBadRecord` for a zero-type, zero-length header **without reporting corruption**. `ReadRecord`, `:298–304`, continues when no fragmented record was in progress. Thus PIT never learns of this gap. `db/db_impl/db_impl_open.cc:1521–1555` inserts the next batch without an unconditional sequence-continuity check; `:1613–1626` only uses continuity to revise an already reported corruption stop. The statement at design `:169` that zeroed preallocation is the end of the log is false for this case.

   I also tested all four WAL recovery modes. All four silently skip the aligned zero region. With an actual bad checksum instead, TolerateCorruptedTail/AbsoluteConsistency refuse to open, PIT recovers the baseline prefix, and SkipAnyCorruptedRecords keeps the later batches. A successful later `sync=true` or `sync_wal` protects preceding writes under A-DEV; this counterexample concerns the interval before that barrier. Sync being enabled on B1/Bk does not establish the general contract for buffered effects, N2, or other engine users.

   **Fix:** make this a required native-engine regression and correct A-PREFIX before treating W1 as an unreal-filesystem witness. Either enforce prefix recovery in the pinned engine (including zero-region handling and continuity across WALs) and validate it, or explicitly change the engine contract/protocol and default model to cover these non-prefix outcomes. Deferring validation to ST3b does not justify asserting that the present source already supplies this property. Document group commit separately: `db/write_thread.cc:510–516` prevents a sync follower joining a non-sync leader; a sync leader can include buffered followers, and `db_impl_write.cc:2235–2243` merges the group into one WAL record. A per-call prefix is a conservative overapproximation of such atomic groups, not their exact behavior; concurrent call arrival/return order must not be mistaken for WAL sequence order.

2. **Major — a directory-operation prefix excludes legitimate btrfs recovery states.**

   **Location:** `designs/ST3a-power-loss.md:108`, `:115`, `:129–130`, `:156`.

   **Concrete counterexample:** start with a previously durable directory. Create `a`, then create and write `b`, then fsync `b`, without fsyncing the directory or completing a full btrfs transaction. Crash. Btrfs tree-log recovery can retain `b` and omit the unrelated earlier `a`. Its file fsync logs that file and its inode references, rather than forcing all earlier changes in an existing parent directory. In Linux v6.12 `fs/btrfs/tree-log.c`, `btrfs_log_inode_parent:7054–7058,7106–7108` describes/logs the target; `log_new_ancestors_fast:6952–6955` logs an ancestor only when it was newly created in the current transaction. The existing parent's unrelated newly created child need not be logged. A pinned source copy is in scratch.

   The proposed directory domain is only `{}`, `{a}`, `{a,b}`. It cannot produce `{b}`. `Extended` does not change this domain; the only per-name alternative is a witness that also breaks rename atomicity. Declining a filesystem's extra durability is not automatically conservative when the remaining model imposes an ordering implication: here, survival of `b` incorrectly implies survival of `a`.

   **Fix:** allow non-prefix persistence of independent directory operations in the default model, while keeping each rename's two-name update atomic and preserving necessary dependencies. Add this btrfs tree-log case as a model/real-stack test. Keep “non-prefix directory updates” distinct from “non-atomic rename”; the former does not require violating A-REN. Narrow the claimed supported filesystem contract if a validated partial-order model is deferred.

3. **Major — truncation changes data, but the crash domain only records it as a size change.**

   **Location:** `designs/ST3a-power-loss.md:79`, `:92`, `:109–113`, `:766–767`.

   **Concrete counterexample:** durably write 4 KiB of `A`; call `truncate(file, 0)` and then `truncate(file, 4096)`, with no data write between them. On ext4/XFS the re-extended region reads as zeros. A crash can retain the truncation/re-extension metadata and recover length 4096 with zero contents. The actual syscall/read behavior was checked in scratch (`truncate-proof.log`); journalling those completed size/extent updates can persist that same image without an application fsync.

   In the specified crash domain, the size choices include 4096, but **there are no sector variables**: they exist only for sectors touched by pending `Write`s, and `SetLen` does not add any. Applying no writes over the durable content produces `A`, not zero. Even if the live `cur` implementation clears pages correctly, that alone does not repair the specified post-crash historical reconstruction. `Extended` adds versions to existing variables, so it does not supply these missing variables either. This is an explicit data-destructive operation, not corruption prohibited by A-DEV.

   **Fix:** make shrink/unmap/zero effects part of the inode's operation history and crash reconstruction, including partial-page tails and truncate followed by re-extension. Also represent the zero-write fallback of ST2 `OsFs::allocate` (`fs.rs:250–258`), instead of equating every allocation with only `SetLen`. Add eager-oracle tests for these histories and for copying over an existing inode; M3 must not simply restate a domain that excludes their data effects.

4. **Major — Q9's literal “except prune-modified records” fix can still omit replay blocks from B1.**

   **Location:** `designs/ST3a-power-loss.md:747`, `:958–961`; S5 at `:722`; DEC-59 Q9.

   **Concrete counterexample:** persist tip/index through height 10. Receive and connect through 320 without an intervening index-only write. The prune flush selects files containing heights 11–32. Those entries are both newly dirty and prune-modified. Under the proposed step 3, “every dirty record except those `PruneOneBlockFile` changed,” they are omitted from B1 entirely. B1 nevertheless durably writes `[320,10]`. Crash before Bk. Replay needs 11–320; the durable index never received 11–32. Keeping their physical files until Bk does not make their missing positions discoverable. The same issue applies to a file-info record with ordinary updates followed by a prune reset. `design-counterexamples.log` demonstrates the missing set for the literal batching rule.

   **Fix:** preserve the **pre-prune versions**, including newly dirty entries and file infos. B1 must write the complete ordinary index/file metadata needed for the marker's forward and undo paths. Bk then writes the prune clears/resets atomically with the tip and marker erase. Represent ordinary metadata updates and prune deltas separately; merely dropping an entire dirty record is insufficient. Add S5 with no prior periodic index write, assert D1 at B1, and check the durable lookup of every replay block before crashing.

   I independently **confirm the original Q9 hole at both Core pins**. The exact chain of evidence is:

   - Prune selection uses the live tip's `tip − 288`: v23 `src/node/blockstorage.cpp:169–185,202`; v24 `:165–181,198`.
   - `PruneOneBlockFile` clears the data/undo flags and all positions, and dirties the entries/file info: v23 `:140–148,165–166`; v24 `:136–144,161–162`.
   - The index is written synchronously: v23 `src/txdb.cpp:267–276`; v24 `:276–285`.
   - The flush performs that index write and unlinks before flushing coins: v23 `src/validation.cpp:2857–2885`; v24 `:3065–3093`.
   - The first partial coins batch installs the marker; the last removes it: v23 `src/txdb.cpp:125–130,143–156`; v24 `:134–139,152–171`.
   - Replay iterates every block from the fork to the new tip: v23 `src/validation.cpp:4937–4944`; v24 `:5288–5295`. `RollforwardBlock` requires an indexed disk read: v23 `:4779–4781`; v24 `:5187–5190`. Cleared positions cannot supply that read.

   Therefore a sufficiently stale coins tip can be stranded during this prune flush, including by a **process kill** after a partial coins batch. No power-loss assumption is needed. This refutes the unqualified “Core survives one anywhere” assertion at design `:818`. DEC-59 explicitly classifies Q9 as a durability bug; this review does not report it upstream.

   With the pre-prune-version correction, Bk placement and post-Bk unlinking introduce no corresponding early-unlink hole: before Bk, the persisted file infos remain nonzero so the startup scan keeps them; after Bk **returns successfully as Durable**, no marker requires them and both the tip and prune clears survive. An applied-but-failed Bk must cause abort without unlinking. A lost Bk causes recovery to finish with the old, unpruned index, which is safe; pruning may be retried later. Assert this ordering explicitly against ST2's `unlink_pruned_files` and `scan_and_unlink_already_pruned_files` (`blocks.rs:656–694`).

5. **Major — R6 does not prove the marker transition was validated before B1.**

   **Location:** `designs/ST3a-power-loss.md:570`, `:601`, `:666–671`; A1 I3 at `:1851`.

   **Planted ordering bug that passes the stated checks:** compute tentative coin/evo deltas and publish B1's marker **before ordinary `connect_block` validation finishes**. Use valid blocks with existing inputs, so their unchecked deltas equal the reference state. Sync their files first and record the correct positions. Crash immediately after B1. Startup now replays exactly the marker path coins-only and reaches the reference's valid `new` tip.

   R0 succeeds; R1/D1 see durable bytes and valid references; R2–R5 see correct markers, state and index; R6's `replayed()` is exactly the marker path and `best_valid` agrees; R7 converges; R8 has no error; prune/link/subscriber/effect checks are either satisfied or inapplicable; R12 connects another valid block. Nevertheless this violates I3: the marker's signature-free exception is only for blocks **fully connected before the flush began**. Valid input does not turn a skipped required validation into an executed validation. The existing S7 checks an invalid block outside the marker, not this ordering obligation.

   The adapter also cannot independently prove that non-marker activation called the ordinary validator: `ChainstateView` exposes replayed hashes but no validation-call evidence. Comparing final state and invalid flags is useful, but it is a different property.

   **Fix:** add test-observer evidence for successful normal connections and activation validation calls, including the ordinary assumevalid decision. Require every marker transition block to have completed normal connection before `FlushBegin`/B1; `RefExtractor` should not treat a marker alone as proof of that provenance. Add a planted “B1 before validation completion” variant on valid blocks, plus an invalid block incorrectly included in the marker. Put the observer contract into CH2's required seams rather than leaving each SUT to self-certify R6.

6. **Major — R7's exact-store equality is unsound for ProcessKill followed by PowerLoss.**

   **Location:** `designs/ST3a-power-loss.md:69`, `:440`, `:571`, `:754`; `dashd-store/src/memory.rs:79–98` on `node/integration`.

   **Concrete counterexample using the approved N2 negative control:** make B1 Buffered, with synced files and the marker/index/evo in that batch; crash by ProcessKill before Bk. The page cache preserves B1, so uninterrupted startup sees the marker and completes to `new`. Now interrupt that startup by PowerLoss after a buffered recovery coin batch and before its final durability barrier. It is legal to lose the original buffered B1 and all buffered recovery writes. Startup then finds the previous durable state and recovers `old`. Neither flush nor recovery had acknowledged `new`, and both outcomes are allowed. They are not the “same stores” required by R7.

   A process kill does not promote its surviving dirty page cache/WAL to power-loss durability. `MemoryEngine::restart` correctly retains the buffered list, unlike `simulate_power_loss`, which promotes the selected disk state to the new durable base. Holding the outer decision vector fixed must not accidentally confer that promotion on ProcessKill. Conversely, after an outer **PowerLoss**, surviving disk bytes are the new disk base and must not be independently lost on the second crash unless recovery modifies them. Buffered effects preserved by an initial process kill provide another legitimate difference between the two final stores.

   **Fix:** define epoch transitions and the equality projection explicitly. For mixed crash kinds, check the acknowledged durable floor, allowed-tip/reference consistency and recovery invariants; do not demand equality of all volatile survivors. Use exact convergence for a fixed durable outer image, or explicitly stabilize that image before claiming exact convergence. Add N2 with ProcessKill→PowerLoss, plus a file/effect that the first recovery never reads. Nested capture must retain unresolved symbolic choices with the correct first-crash provenance, not independently resample or silently persist them.

7. **Major — the exhaustive gate as specified cannot also guarantee zero truncation for its advertised record sizes.**

   **Location:** `designs/ST3a-power-loss.md:454`, `:560–565`, `:677`, `:710`, `:866–868`, `:918`.

   **Concrete counterexample:** plant T1/P1 on an 8 KiB record, a size explicitly allowed for ToyChain. A crash after its wrongly durable reference but before the file sync has at least sixteen independently old/new 512-byte sectors. On the branch with the directory entry and full length retained, R1's full-record `read_exact_at` resolves all sixteen before comparing the bytes. That is **65,536 assignments** for this read alone, versus the PR limit of 1,024. The design runs the store-level checks even before recovery and reports all violations, so “a correct protocol reads almost no variables” does not bound the planted-bug suite. A header crossing another sector increases the count. The nightly 65,536 bound is already consumed before adding directory, size, engine or nested choices.

   **Fix:** decide what is exhaustive and give exact scenario sizes/counts. Options include exhaustive small-model cases, symbolic equivalence/pruning that is proven to preserve the checked property, or a clearly separate witness-search mode that can stop once both R1 and D1 have been demonstrated. Do not label early termination or sampled remaining branches as exhaustive zero-truncation coverage. Add bytes inspected, history lengths and copied/map entries to work accounting; event counts alone do not bound full-record hashing, map copy-on-write, or fault reruns.

   The **60 CPU-s target is plausible only after this scope correction and measurement**, not established by the proposed lazy argument. The 320-block prune case, per-call fault reruns and nested checks also require actual counts. **1 CPU-hour/100,000 cases implies at most 36 ms CPU per case**, including the average fault rerun, recovery, byte decisions and depth-two work. A sampled release sweep may achieve that, but an exhaustive byte/nested expansion generally will not. Specify a deterministic maximum-work sweep and separately measure process CPU time on the calibrated build; report an unfinished/truncated sweep honestly. No ST3a implementation exists to benchmark here, so these are demonstrated search-size constraints and feasibility requirements, not invented runtime measurements.

8. **Minor — R8 classifies safe, intentionally ignored prune/unlink errors as fatal.**

   **Location:** `designs/ST3a-power-loss.md:458–469`, `:572`, `:852`; ST2 `dashd-store/src/flatfile/blocks.rs:666–681` at `0304cada`.

   **Concrete counterexample:** after successful Durable Bk, inject EIO on removal of one pruned blk file. ST2 deliberately logs and ignores that failure, removes the rev/other files, and syncs the directory. Since the durable index no longer references the failed removal's file, keeping it wastes space but is safe and startup retries it. R8 sees the filesystem error, a later successful mutation, and no `Mark::Abort`, so it flags this correct use of ST2. Advisory allocation failures have the same error-classification problem: `FlatFs::allocate` cannot return a fatal error to its caller.

   **Fix:** distinguish errors that the API exposes as fatal from advisory allocation and safe best-effort pruning outcomes. Enforce A1 I5 for fatal writes/barriers and the resulting chainstate abort; check ignored unlink failures through R9 and eventual cleanup. Preserve DEC-59's public-API-only ST2 integration. The relevant existing test, `unlink_failures_are_logged_and_skipped`, passes; the harness must accept that behavior too.

9. **Minor — one pass of decision simplification does not establish the promised 1-minimality.**

   **Location:** `designs/ST3a-power-loss.md:502–519`, `:776`.

   **Concrete counterexample:** let loss bits `a,b` start at `(1,1)`, with a failure of the same invariant whenever `a || !b`. Trying to clear `a` first passes, so retain it. Clearing `b` then still fails, so keep that simplification. The result `(1,0)` is not 1-minimal: clearing `a` now also fails. Recovery failures need not be monotone in losses; preserving a later record can expose another inconsistent state. `design_counterexamples.py` executes this example. A 2,000-replay budget can also expire before the stated guarantee is proved.

   **Fix:** repeat decision minimization to a fixed point and recheck each remaining loss after workload/schedule/granularity changes. Validate that fixed choices remain in the new domains and that the same expected invariant fails. If the budget expires, return a replayable best-effort result with `minimality_verified=false`; reserve “1-minimal” for the checked result.

The lazy DFS theorem itself is sound for independent, fixed-domain image variables when **every read and mutation dependency** is intercepted; state-dependent read order is not a counterexample. My small adaptive-read-order eager/lazy experiment agrees. The theorem is not a proof that the image domains above contain every real state, or that nested epochs preserve them correctly. Required adversarial cases should include an unread sector followed by `sync_data`/copy, link counts depending on another directory, truncate/re-extension, and nested captures of unresolved variables. These mutations must resolve or preserve their dependencies even when recovery never explicitly reads them. M10 should compare observable states/operation traces as well as sets of verdicts: two searches can both yield `{pass,fail}` while missing different states.

The other requested source checks support much of the design's analysis. Both pins use file→synced index→unlink→unsynced coins→unsynced EvoDB ordering (v23 `validation.cpp:2850–2889`; v24 `:3058–3097`). The current-pair-only sync and non-propagating flush failure are confirmed at v23 `blockstorage.cpp:557–566` and v24 `:585–604`; older undo is separately finalized only at the file's last undo height (`:735–736` / `:791–798`). `FlatFileSeq::Flush` checks file commit but ignores the directory commit result (`flatfile.cpp:87–95` / `:89–99`). Coins and EvoDB batch sync flags are as claimed. Beyond Q9, a process kill after the final coins batch erases the marker but before EvoDB's root commit can also leave coins ahead of EvoDB: `ReplayBlocks` is then a no-op (`validation.cpp:4886–4887` / `:5229–5230`), while a subsequent ordinary connection checks EvoDB's previous tip (`:2320–2324` / `:2525–2529`). Thus `feature_dbcrash`'s chosen partial-batch points do not prove arbitrary process-crash survival. Qualify that claim.

**BL-32 is independently confirmed at source level.** Cargo.lock pins `librocksdb-sys 0.19.0+11.8.1`. Its Darwin branch defines `OS_MACOSX` and POSIX flags at `build.rs:240–242`, and the build script contains no `HAVE_FULLFSYNC` definition. Bundled `env/io_posix.cc:1826–1850` uses `F_FULLFSYNC` only under that macro, otherwise `fdatasync`/`fsync`; directory handling has the same conditional at `:2146–2157`. CMake performs the missing detection/definition at `CMakeLists.txt:606–608`. The installed Rust std source uses Apple `F_FULLFSYNC` for both file sync functions (`sys/fs/unix.rs:1417–1433`), supporting ST2's mapping. This confirms the bundled default build claim, not every externally supplied RocksDB/CXXFLAGS configuration. I did not claim an APFS runtime/directory-fd test on this Linux VM; that remains pasta's explicit DEC-59/BL-32 Mac check. Keep BL-32 as a dependency of any claim that A-DUR holds on Apple hardware.

Validation: the reused ST2 `0304cada` store/model test artifact ran with **35 passed, 0 failed, 1 ignored** (`st2-tests.log`). The aligned RocksDB recovery experiment exercised all four recovery modes, plus an intact control and the fragmented-record comparison. The Python experiments cover the literal prune exclusion, adaptive DFS, the nonmonotone shrinker and branch-count lower bound. These do not substitute for ST3a implementation tests or physical-filesystem crash experiments.

The design has a useful separation of storage contracts, protocol checks and native-stack testing, but its current default engine domain excludes an executed native-stack outcome, its filesystem domains exclude valid states, and its prune erratum still needs pre-prune versions. Fix those and strengthen validation/nested-crash observability before implementation approval. **Final verdict: REWORK.**
