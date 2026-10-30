# CH2b b2, round 1: second independent review (Opus high)

**Head reviewed:** `0b6376d29` (`node/ch2b-prune`). It includes the dir-sync demotion commit, `fix(dashd-store): the unlink's directory sync is best-effort`, on top of the checkpoint `797f8d4c5`. **Base:** `8161858bf`, 28 files, +2568/−181.

**Method:** I used a detached worktree at `0b6376d29` (`/work/scratch/ch2b-b2-opus-r1/wt`, since removed), with Core `v24.0.0-rc.3` from `~/workspace/dash`. I read the CH2 design (§4.2 steps 0/7/10/12, §4.5, E-b1.7/E-b1.8, the crash-state tables), REVIEW-COMMON and Sol r1 (`/work/scratch/ch2b-b2-sol-r1/CH2b-b2-r1-sol.md`). I didn't re-derive Sol's parity table. This round targets interleavings, the Drop path, error paths, the `FlushIndex` refactor, the demotion commit and simplicity.

## Findings

No blocker or major findings. Two minor findings: m1 is design-level and routed to the manager; m2 is a test gap that p3 should close. The rest are nits.

### m1 (minor, design-level): step 7's bound is the freeze tip's, so a deep rewind inside the flush window can prune within `MIN_BLOCKS_TO_KEEP` of V's tip

**Where:**
- `dashd-chainstate/src/flush/machine.rs:513`: `let last_prune = d.tip_height`.
- `flush/policy.rs:271`: `tip_height: ctx.chain_height`, which is `new` at step 0.

This is what CH2 §4.2 step 7 specifies (`new − K`). b3's lock term, `height(LCA(locator, new))`, has the same shape.

**Core:**
- `validation.cpp:2991` computes `last_prune{m_chain.Height()}` under `cs_main` at the moment of selection.
- `DisconnectTip` (`:3249`) runs `FlushStateToDisk(IF_NEEDED)` after every disconnect (`:3304`). So during an `invalidateblock`, Core prunes against the descending tip. It never removes the undo data of the next block it has to disconnect, and never removes anything within 288 blocks of the tip it ends on.

**Scenario:**
1. An automatic prune check is armed, and V freezes at `new = 10 000`. D holds file f, whose blocks all sit at or below 9 712.
2. While W runs B1, an `invalidateblock` on V disconnects down to 9 599. V reads undo for 9 712…9 600 from f, which is still intact. This needs about 400 fast disconnects during one B1. A one-batch B1 of a large cache takes seconds, so the timing is plausible. `feature_pruning.py`'s `reorg_back` runs a >288-deep `invalidateblock` on a pruned node.
3. W's PruneSelect runs at bound 9 712 and V clears f. Bp, then unlink.
4. Result: the active tip is 9 599, and its block and undo, plus everything up to 9 712, are pruned.

That state isn't Core-reachable:
- If Core had pruned at 10 000 before the invalidation, the invalidation would stop at the first block whose undo is gone. Core logs `DisconnectTip(): Failed to read block`, or `UndoReadFromDisk: no undo data available` (`blockstorage.cpp:545`).
- If Core prunes during the invalidation, its bound follows the tip down, and nothing in f is selected.

When V crosses f *after* the clears, our run fails the way Core's prune-first linearization does. That outcome is fine. The divergence is the race-won case only.

**Consequences:**
- A later disconnect from that tip fails.
- `reconsiderblock` has to re-download 9 600…9 712.
- `NODE_NETWORK_LIMITED`'s last-288 promise is broken until the tip moves.

Nothing is corrupted: the index and files agree, and recovery is consistent.

**Why it's not a b2 defect:** the code implements the approved step 7 exactly. The fix is a protocol choice.

**Suggested fix (manager/design):**
- Put `bound` in `PruneClears`.
- V checks `last_prunable(V's tip height) >= bound` before it clears.
- Otherwise V answers "moved" (an empty answer plus a flag), and W skips the prune and re-arms the automatic check. `pruneblockchain` gets Core's own answer at the lower tip on its retry.

A cheaper option is a D-row: "a prune decided at `new` may cover blocks within K of V's tip when V rewinds more than K below `new` inside one flush". Either way it belongs with b3's lock term, which has the same `new`-anchoring. **Not blocking b2.**

### m2 (minor, test gap): Bp's durability before the unlink, and store failures at the new prune steps, are unpinned

- **Where:** `flush/machine/tests.rs:701` `every_store_call_can_fail` covers PresumedBarrier, B1, SubBatch, PreBkBarrier, Bk and AfterBk on a non-pruning split flush.
- **What's missing:**
  - PruneBarrier (d2) failing;
  - Bp's write failing;
  - Bk failing with the clears in it;
  - the T5/T13/T15 crash states between Bp/Bk and the unlink.
- **What should be asserted:**
  - no unlink (`w.on_disk(f)` is true);
  - `meta.pruned_files == false`;
  - the abort text (`System error while flushing: …` for Bp/Bk, ST2's flush text for d2), which matches Core's `:3121-3123` catch and `FlushBlockFile`;
  - exactly one `Abort` mark.
- The guard latching on the failed write is what stops the unlink today (the driver's latch check, `driver.rs:94`). My M6 shows that even a Bp downgraded to `Buffered` passes every test (see Mutation).
- p3 (the CRASH model with pruning, planned) is the natural owner. Make sure it covers "Bp/Bk not Durable before unlink" explicitly, and add one direct `every_store_call_can_fail` case for a pruning one-batch flush and a pruning split flush.

### n1 (nit): `prune_clear` mutates `unlinked` before its only fallible call

- **Where:** `dashd-chainstate/src/chain/index.rs:408-412`. The `self.unlinked.retain(…)` runs before `chain.node_index()?`.
- **Scenario:** if `node_index()` fails ("a chain that is not the node's"), V aborts with `m_blocks_unlinked` already pruned of file f's pairs, while the entries still have HAVE_DATA. It's memory-only, the node stops, and nothing durable changes. Still, the "nothing is pruned" contract on the `FlushIndex::prune_clear` doc (`control.rs:54ff`) is literally violated.
- **Fix:** take `node_index()` first, or compute the erase set and apply it after. This is V side, so CH1-node's call.

### n3 (nit, test gap): the demotion relies on the dir-unsynced flag surviving a failed sync, and nothing pins that

- **Where:**
  - `dashd-store/src/flatfile/blocks.rs:823-825` with `seq.rs:239-242`. `sync_dir` clears `dir_unsynced` only on success, and `sync_blocks_dir` (`blocks.rs:625`) marks the undo seq synced only on success.
  - So after a swallowed failure, the next files barrier (`sync_inner`) syncs `blocks/` again, and if that fails, it stops the node (BL-124's strictness).
- **Why it matters:** the unlink's sync also covers any blk/rev file V created between d2 and step 12, because V appends concurrently. If a later change cleared the flag on failure, the next barrier would skip the directory fsync. An index write could then reference a newly created file whose directory entry isn't durable, which is a real corruption on power loss.
- **The gap:** M8 (which clears both flags in the warning arm) **survived** both the new store test and the machine test.
- **Fix:** in `a_failed_directory_sync_after_the_unlink_is_logged_and_skipped` (`flatfile/tests.rs:1010`), after the failure, assert that the next `bf.sync()`'s ops end in `Op::SyncDir("blocks")`. The test already calls `bf.sync().unwrap()`, but it drops the ops.

### n4 (nit, informational): our removal failures log a warning; Core's are silent

- Core's `UnlinkPrunedFiles` (`blockstorage.cpp:618-629`) passes one `std::error_code` and never looks at it. Ours logs `warn!("Prune: {e}")` per failed remove and, after this commit, for a failed directory sync.
- These are extra log lines only, and CONS-28 chose to log. I mention it only so that compat's log-diff tooling lists `Prune: Failed to …` as an allowed extra line, if it doesn't already.

## Focus 5: the dir-sync demotion commit (`0b6376d29`)

| Check | Result |
|---|---|
| Warning only: logged at `warn`, target `blockstorage` | Yes: `blocks.rs:823-825`. The store test asserts target, level and prefix. The machine test asserts the `Prune: Failed to sync the blocks directory` prefix through a capturing subscriber. |
| Nothing latches, and the node keeps running | Yes. `unlink_pruned_files_inner` returns `Ok(())`, so `mutate` doesn't set `failed`. The store test asserts `!bf.has_failed()`. The machine test asserts that `latch().tripped()` is none, that the flush reaches `Step::Done` (`step.expect("no stop")`), and that no `StoreFailed` or `Abort` mark appears. |
| Removal-failure behaviour is unchanged | Yes. The same machine test loops both cases (`sync_fails` false/true). `w.on_disk(0) == !sync_fails`. |
| Scan path (`scan_and_unlink_already_pruned_files`) | Goes through `unlink_pruned_files`, so the same demotion applies. Core's `ScanAndUnlinkAlreadyPrunedFiles` (`blockstorage.cpp:412-427`) doesn't sync either. |
| Refusal after an earlier failure still stops | Yes: `mutate` refuses first, so `FlatFileError::Failed` is returned, giving `Halt::files_at(…,"unlink")` with `refused: true` (`machine.rs:654-665`), which is a silent refusal (the first failure's caller reports). The doc's `# Errors` says so. |
| No other durability step weakened | Confirmed. d1/d2's `sync_and_take_dirty_infos` still uses the fatal `FLUSH_BLOCK_FAILED` dir sync (BL-124 is out of scope). Bp/Bk are still `Commit::Durable`. The flag survival (n3) keeps the next barrier honest. Recovery's ordering (scan after the index load) is unchanged. |
| Clears durable before the unlink | Yes: `tail()` (`machine.rs:829`) puts Unlink only after Bp or Bk in every ordering. A removal undone by power loss leaves an unreferenced file with a zeroed, durable info, which the startup scan removes. |
| Core parity | Core never syncs after removing (D8 keeps our extra sync as durability only). Core keeps running when a removal fails (`ec` ignored). Matches. |

**Verdict on the commit:** correct and complete. The test assertions are the right ones. Add n3's assertion.

## Interleaving table

V = the validation thread, W = the flush thread. "OK" means the durable state is Core-reachable and the in-memory state is consistent.

| Interleaving | What happens at `0b6376d29` | Result |
|---|---|---|
| Automatic check fires while F is in flight | `policy.rs:189`: `check_due()` isn't called, so the check stays armed and `causes` are pending. It's decided afresh after the release. | OK (Core `:3017` clears only when it selects) |
| `pruneblockchain` while an automatic prune is in flight | `WaitThenFreeze`. V's wait services the in-flight job's `PruneClears` (`control.rs:483`). The manual decision comes after the release and doesn't take the automatic check (`policy.rs:263`). | OK |
| Automatic and manual in one decision | A manual height takes priority, and the automatic check stays armed for the next call. | OK (Core `:3009-3017`) |
| Nested/concurrent flushes | V has at most one job. An index-only write and a freeze are serialized on W. The next freeze's step 0 sees the pruned infos as zero, because apply runs before Bp, so nothing is re-selected. | OK |
| V appends (a new blk file) between step 0 and step 7 | `eligible` requires `file < last_file` and `size != 0` (a subset of D, T39). d2 also takes the V-ahead infos, and Bp writes them durably after the barrier. | OK, same as b1's d1 |
| V writes undo into rev f (f in the selection) before its clears | Only possible after a deep rewind (m1). V's HAVE_UNDO is cleared by the clears, W's apply zeroes the info, and the unlink removes it. The index and files agree. | OK for consistency; m1 for parity |
| Reorg/invalidation between selection and the clears, V's tip still ≥ new | The clears act on V's current index. Every selected block is ≤ new − K ≤ V's tip − K, so it's Core-reachable as "pruned at `new`, then the reorg". | OK |
| Same, new − K ≤ V's tip < new | Selected blocks are ≤ new − K, below V's tip, but some lie within K of it. Core's selection at V's tip would keep them, though Core pruning at `new` and then reorging gives the same state. | OK (prune-first linearization) |
| Same, V tip < new − K | **m1**: may prune within K of V's tip, including the tip. | Divergence (design) |
| Latch trips while W is parked in PruneApply | `park` returns `Err`. The next `run_step` checks the latch first (`driver.rs:94`) and returns `Refused`. There's no busy loop: the latch check precedes the re-poll. | OK |
| V's `prune_clear` fails | V trips the latch with its own text (`abort(db, log)`) and drops the request. W sees the latch, refuses, and issues no second `AbortNode`. | OK: one abort, V's message first |
| Latched failure elsewhere before V services the request | `latch().check()?` happens before `prune_clear` (`control.rs:489`), so V clears nothing. The request drops and W refuses. | OK |
| V drops the driver while W is parked, or before W reports | `Drop` drains the queue under the lock with `closed = true`. `report` drops events once `closed`. The `OneShot` drop wakes W with `Dropped`, giving `clears_dropped` → AbortNode, unless the latch had tripped (then `Refused`, because `op: None`). | OK. M7 (dropping the `closed` check in `report`) is caught by `dropping_the_thread_driver_ends_ws_wait_for_the_clears`, via its 120 s drop timeout |
| Orderly stop with an automatic prune in flight | ALWAYS waits for F and services its clears. The ALWAYS freeze may itself prune (the check may be re-armed by T40). V waits for its release, then drops. W runs the late Unlink with the queue closed, and the join waits for it. | OK |
| Shutdown/abort between B1 and Bp/Bk | Memory-only clears. Durable: B1's old index and infos, plus the files. | OK (Sol's crash table) |
| Power loss between Bp/Bk and the unlink, or mid-unlink | Durable cleared index, zeroed infos and `pruned_files`. Leftover files are unreferenced, and the startup scan removes them. | OK |
| Dir-sync failure at step 12, then V creates a file, then the next barrier | The flag survives the failure (`seq.rs:242`), so the next barrier syncs `blocks/` before any index references the new file. | OK, but unpinned (n3) |
| A reader that checked HAVE_DATA before V's clears and reads after the unlink | The read fails with "file not found". Core does its check and read under `cs_main` in v24's `ProcessGetBlockData`. | Not b2's scope. Owners of block-serving readers must treat a failed read of a just-pruned block as "not available", never as an assert. Noted for CH-net/ND1. |

## B1: the Drop path, every event type queued at drop

`DriverEvent` has four variants (`driver.rs:32-40`). `Drop` (`driver.rs:433`) discards whichever are unread:

| Event | Effect of dropping it unread | Lost information? |
|---|---|---|
| `Released(FlushReport)` | V doesn't run `release`. `FlushReport` is plain data with no `Arc` to F (W has already dropped F at step 11). | No, because V is going away |
| `PruneClears(req)` | The `OneShot` drop wakes W with `Dropped`, which gives AbortNode, or `Refused` if already latched. This is the intended behaviour. | No |
| `Failed` | `run_step` has already called `abort_sink().abort_node(…)` (`driver.rs:106-108`) before `Stopped(_, Failed)`. A `Refused` failure's own caller reported it. | No: the fail-stop report reaches the sink, not the queue |
| `Crashed` | `on_crash()` (the `_Exit` stand-in) runs on W, followed by the latch trip, *before* `report`. | No |

There's no other event type, and `BlockChecked` and the like aren't driver events. So Drop discards nothing but V's own bookkeeping and the request it's meant to drop. Sol's caveat holds: a `PruneClears` held outside `FlushControl` isn't covered.

## Error-path parity

| Path | Core v24 | Ours | Verdict |
|---|---|---|---|
| `UnlinkPrunedFiles` remove fails | `ec` is ignored, no log, and the node continues | Warn and continue (`blocks.rs:810-821`) | Parity (+ log, n4) |
| Dir sync after unlink fails | Core never syncs | Warn and continue (this commit). The next barrier retries. | Parity (D8) |
| d2 / files barrier fails | `FlushBlockFile` calls `AbortNode("Flushing block file to disk failed…")` and **continues**: it writes the index and unlinks | Stops at the barrier: no index write, no unlink. Message from ST2 = Core's text | Stricter, but b1's established I5 rule. The durable state = Core's crash before the flush. OK |
| Index write (Bp/Bk) fails after the clears | `WriteBlockIndexDB`, then `dbwrapper_error`, then `:3121-3123` "System error while flushing: …" (`AbortNode`), with no unlink (the throw skips it) | `Halt::system("write")` gives "System error while flushing: …", the guard latches, and there's no unlink | Parity. Untested (m2) |
| `fDoFullFlush` coins flush fails | In Core, the index write and unlink are already done when the coins flush fails | Ours fails in B1 *before* the selection, so nothing is pruned | Different order, but our durable state is a Core prefix (pre-prune). OK |
| V cannot clear | — (Core has no such path) | V's message goes to `AbortNode` once. W refuses silently. | OK |

Fail-stop order: in every case exactly one `AbortNode`, from the first failure, and no write or unlink after it. That matches E-b1.7 and FA7.

## Focus 4: the `FlushIndex` refactor

- **Same freeze point:** `cx.index.take_dirty()` replaces `(cx.take_dirty)()` at the same two sites (`control.rs`: the index-only write and the freeze packet). The bench input change is mechanical.
- **Records lent twice:** by design. `prune_clear` both returns the cleared records (for Bp/Bk) and leaves the entries dirty for the next B1 (`index.rs:421`). The records are encoded at answer time (`NodeIndex::record`), and the next B1 re-encodes from the then-current state.
  - So a newer change (a re-downloaded block, an invalidity bit) is never overwritten by the stale cleared record: W is serial, so Bp precedes the next B1.
  - Core writes them once, which makes this a write-count difference only.
- **Dropped on an error:** after a successful answer, any later W failure stops the node, so the in-memory clears needn't be durable. n1 is the one ordering wart.
- **`ArenaIndex::prune_clear`** is `unreachable!()` (`crash/chain.rs:335`). It's correct today (the chain SUT uses `NoPrune`), and p3 will need to replace it.

## Header-only file-0 judgment (Sol's observation)

Core's `PruneOneBlockFile(0)` (`blockstorage.cpp:131-163`) matches every entry with `nFile == 0`. That's every header-only entry and every previously pruned entry (`nTx > 0`, data gone). Core clears bits that are already clear and inserts each into `m_dirty_blockindex`. Ours erases those entries' unlinked pairs (`index.rs:408-411`, the `unwrap_or(0)`) but doesn't dirty them.

| Surface | Observable? | Why |
|---|---|---|
| Persisted index | **No** | The record content is unchanged: `nHeight`, `nStatus`, `nTx` and the header, with `nFile`/positions absent because neither HAVE bit is set (`chain.h:391-413`). Core's rewrite *does* change one byte sequence in Core's LevelDB: the leading `VARINT(client version)` (`chain.h:394-395`) is refreshed to the running version. Our `BlockIndexRecord` has no version prefix (`dash-chain/src/record.rs:18-19`), so there's nothing to refresh. |
| Recovery / load | No | It loads identical bytes. Core's load rebuilds `m_blocks_unlinked` from `nTx > 0` entries whatever `m_dirty_blockindex` holds. |
| `CheckBlockIndex` | No | It never reads the dirty set. Its unlinked assertions see the same `m_blocks_unlinked` as Core, because the pair erase *is* done (`check_block_index(…, true)` passes in `prune_clear_clears_the_files_entries_and_their_unlinked_pairs`). |
| `m_blocks_unlinked` | No | It's erased identically, duplicates included (OH-m8). |
| Logs / bench | No | `WriteBlockIndexDB` logs no count, only the "write block index to disk" timer. |
| I/O volume | Yes (write count) | Not a DEC-95 observable. Dirtying ~2 M header-only entries would also force them resident in our `HeaderChain` (dirty entries must be resident), which is a real cost, so not dirtying them is the better choice. |
| Self-healing | Only under another bug | Core's blanket rewrite would repair a header-only entry whose in-memory state had drifted from its record without being dirtied. That needs a missing-`mark_dirty` bug elsewhere, so it isn't an observable of correct code. |

**Judgment:** it's truly unobservable to every consumer of our store and every Core-parity surface. **No D-row is needed.** The code comment at `index.rs:383-390` is the right record. If the manager wants it on paper, a one-line note under D8's neighbours ("write-set only") is enough, but I don't recommend a D-row.

## Focus 6: simplicity

- The `tail()` iterator plus `tail_after` (`machine.rs:829-856`) encodes the four orderings in one place. It's slightly clever, but it replaced two booleans that had already produced S1, and its tests (`the_unlink_comes_before_a_release_after_which_w_is_idle`) enumerate all cases. Keep it.
- The custom `OneShot` (about 120 lines) is justified: it needs latch registration (E-b1.7), and std channels can't register a `Parker`. The manager has already ruled on it.
- `PruneDecision.target` duplicates the host's `-prune` target so that step 7 can evaluate T40 without the host. It's fine.
- Nothing is stricter than Core in b2 after this commit, except the files-barrier dir sync (BL-124, out of scope) and the d2 stop-on-failure (b1's I5 rule).
- `Halt::clears_dropped`'s AbortNode has no Core counterpart, but it only fires on an unorderly V drop. A plain `Refused` there would lose a W-side failure with no reporter, so keep it.

## Mutation table

Each mutant was applied alone from a saved copy (`mut/<file>.saved`) by line-addressed `sed`, and restored with `cp -p` + `touch`. `cmp` was verified after every restore, and `git status --short` was empty at the end. The suite was `cargo test --locked -p dashd-chainstate --lib --features alloc-tag,crash,planted-bugs` (300 tests). M8 also ran `-p dashd-store`. None of these mutants duplicates the implementer's ten (selector p1 set, S1, S2, latch, refuse, fatal dir sync) or Sol's five (IBD margin, `<=`/`<`, `>=`/`>`, undo bytes, 287).

| # | Mutant (file:line at `0b6376d29`) | What it breaks | Result |
|---|---|---|---|
| M1 | `chain/index.rs:409`: the unlinked erase uses `file.is_some_and(in_files)` (drops Core's `nFile` 0 for no-data entries) | `blockstorage.cpp:150-157` on file 0 | **Killed**: `prune_clear_clears_the_files_entries_and_their_unlinked_pairs` (index.rs:239) |
| M2 | `chain/index.rs:421`: no `ix.mark_dirty(id)` | Cleared entries are not rewritten by the next B1 (Core's `m_dirty_blockindex.insert`) | **Killed**: both `prune_clear_*` tests |
| M3 | `flush/machine.rs:716`: no `Meta::put_pruned_files(b, true)` in Bp/Bk | `m_have_pruned`/`prunedblockfiles` not durable, so the startup scan never runs | **Killed**: 7 tests (one-batch, split, manual, tail, unlink-failure, both control prune tests) |
| M4 | `flush/machine.rs:687`: re-arm on `unmet` alone (ignores D's `met_target`) | T40 | **Killed**: `a_split_flush_carries_the_clears_in_bk`, `an_empty_selection_writes_nothing_and_rearms_only_if_d_met_the_target` |
| M5 | `flush/machine.rs:715`: Bp/Bk get d2 with its infos emptied (`last_file` kept) | The pruned files' zeroed infos are never durable: after restart, the infos reference unlinked files | **Killed**: `a_one_batch_flush_prunes_in_bp_and_unlinks_after_its_release` FAILED. The run then hit my 900 s wrapper timeout on an unrelated slow model test at load ~90 (rc 124); the failure line is in the log |
| M6 | `flush/machine.rs:619`: Bp writes `Commit::Buffered` instead of `Durable` | The clears might not be durable before the unlink. Power loss between them gives an index that references deleted files (T5/T13 class) | **SURVIVED** (300/300). See m2: p3's CRASH model or a power-loss case must pin Bp's durability |
| M7 | `flush/driver.rs:267-269`: `report` ignores `closed` | B1: a `PruneClears` reported after Drop's drain sits in the queue, so W parks forever and the join hangs | **Killed**: `dropping_the_thread_driver_ends_ws_wait_for_the_clears` (the drop's 120 s timeout) |
| M8 | `dashd-store/src/flatfile/blocks.rs:824`: the warning arm also marks both seqs' directories synced | The next barrier skips the retry. A file V created before the failed sync is then referenced with no durable dir entry (n3) | **SURVIVED**: `-p dashd-store` all green, and `-p dashd-chainstate --lib … flush::` 60/60 |

Kill rate: 6/8. The two survivors are both durability-ordering properties that the unit tests observe only through visibility, not through power loss (m2, n3).

## Validation and limits

- **Tests at `0b6376d29`**, all EXIT 0 under load 41–49:
  - `cargo test --locked -p dashd-store`: 67 unit tests (1 ignored) plus fail-stop, feature-scope and WAL tests;
  - `-p dashd-chainstate --lib`: 283/283;
  - `-p dashd-chainstate --lib --features alloc-tag,crash,planted-bugs`: 300/300;
  - `-p dash-chain`: 390 unit tests plus 11 doctests;
  - `cargo fmt --all -- --check`;
  - `cargo clippy --locked -p dashd-store -p dashd-chainstate -p dash-chain --all-targets` with the chainstate features, `-D warnings`.
  - Logs: `/work/scratch/ch2b-b2-opus-r1/logs/` and `/work/scratch/ch2b-b2-opus-r1/mut/` (kept).
- **Not done:**
  - no gate copy;
  - no `blockfiles_parity` (Sol notes that its parity data is unconfigured);
  - no live Core run of m1's scenario. That one is from source: `validation.cpp:2991`, `:3249`, `:3304`; `blockstorage.cpp:540-545`.
- **Out of scope:** CH1-node's production `FlushIndex` impl and N7's in-hold `drain_fanout` (BL-123), b3's locks, p3's CRASH model, and BL-124.

**Verdict:** APPROVE-WITH-NITS
