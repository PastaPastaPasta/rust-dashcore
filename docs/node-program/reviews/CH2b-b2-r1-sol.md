# CH2b b2, round 1 — Sol independent review

Pinned diff: `8161858bf..797f8d4c5` (`node/ch2b-prune`, 28 files). I reviewed a detached worktree at the requested head, the approved CH2 rev5 design and DEC-95/99/102, the prior CH2b Sol reviews, and Dash Core `v24.0.0-rc.3` in the local `dash` checkout. No implementer worktree, branch ref, or GitHub state was changed.

## Findings

No actionable finding in the reviewed b2 slots. The already-assigned directory-fsync-after-unlink ruling is excluded from this round.

**Observed parity detail:** `dashd-chainstate/src/chain/index.rs:405-423` leaves a header-only entry with Core's implicit `nFile=0` clean when file 0 is pruned. Core `src/node/blockstorage.cpp:131-163` dirties and rewrites that entry, even though its `nStatus`, `nFile`, `nDataPos`, `nUndoPos`, and serialized record are unchanged. The Rust path removes its `m_blocks_unlinked` pair, and every entry with actual file-0 data is cleared and dirtied. In the concrete header-only child/file-0 case covered by `prune_clear_clears_the_files_entries_and_their_unlinked_pairs`, there is no different persisted index or recovery state; this is recorded as a harmless write-count difference, not a finding.

## Core parity

| Rule | Rust evidence | Core v24 reference | Result |
|---|---|---|---|
| Check due; manual height; no prune in reindex | `flush/policy.rs:251-280`, `flush/prune.rs:80-114` | `validation.cpp:2989-3017` | Matches the call-site gate. A deferred automatic check remains armed. |
| Automatic `PruneAfterHeight`, nonzero target | `flatfile/blocks.rs:733-740` | `node/blockstorage.cpp:188-198` | `tip <= after` and target zero return before the selector log. |
| `MIN_BLOCKS_TO_KEEP` and inclusive file height | `flatfile/blocks.rs:76-86,741,757-759,780-783` | `blockstorage.cpp:172-181,198-229` | Signed automatic and unsigned manual arithmetic, including Core's low-tip wrap, and `height_last <= bound`. |
| Prune-lock buffer and clamp | `flush/prune.rs:147-179` | `validation.cpp:2992-3007`, `validation.cpp:122` | Subtracts 10 plus one in signed arithmetic, clamps to 1, skips `INT_MAX`. b3 owns live subscriber locks. |
| Usage, next-allocation room, IBD margin | `flatfile/blocks.rs:631-634,743-762` | `blockstorage.cpp:198-239` | Counts blk and rev bytes; 17 MiB room; adds 10% target only after entering the prune branch in IBD; strict `<` stop test. |
| File choice and order | `flatfile/blocks.rs:705-710,748-762` | `blockstorage.cpp:174-183,213-237` | Ascending files before `last_file`; skips zero-sized blk files; manual takes all eligible, automatic stops on target. |
| Final selection | `flush/machine.rs:508-546`, `flatfile/blocks.rs:780-783` | `validation.cpp:3009-3017`, `blockstorage.cpp:174-181,213-229`; CH2 §4.2 T39 | `D ∩ eligible(actual)` in D's file order. The selector is not rerun. |
| Index clears and unlinked pairs | `chain/index.rs:397-423`, `dash-chain/src/chain.rs:2234-2252` | `blockstorage.cpp:131-163`, `chain.h` `CDiskBlockIndex` | Clears both HAVE bits, file, and positions for data-bearing entries, marks them dirty, serializes the cleared record, removes all matching unlinked pairs including implicit file 0. Header-only file-0 dirty difference described above. |
| File-info reset and `m_have_pruned` | `flatfile/blocks.rs:640-643`, `flush/machine.rs:710-716` | `blockstorage.cpp:160-162`, `validation.cpp:3019-3022` | The zero info and `meta.pruned_files=true` become durable with the index clears. Repeated true writes are idempotent. |
| Automatic re-arm | `flush/machine.rs:679-702`, `flatfile/blocks.rs:785-790` | Core's synchronous `validation.cpp:3017`; CH2 §4.2 T40 | Only when D had met the target and the filtered selection now leaves it unmet. This is the approved asynchronous extension. |
| Unlink best effort | `flatfile/blocks.rs:792-818`, `flush/machine.rs:654-665` | `validation.cpp:3068-3074`, `blockstorage.cpp:618-629` | Both files attempted after the durable index write; individual remove errors are logged and skipped. The pending directory-fsync ruling is out of scope. |

## Ordering and crash states

| Cut | Durable state and Core-reachable recovery result |
|---|---|
| Before B1 | No prune mutation is durable; all selected files remain. |
| After B1, before V's answer | B1 has the pre-clear index and file infos; the selected files remain. A split B1's marker replays as in b1. |
| After V clears and W resets infos, before d2/Bp/Bk | Only memory changed. Crash reloads B1's old index and infos, which still reference existing files. |
| After d2, before Bp/Bk | File data and directory changes covered by d2 are durable, but the old index/infos and physical files remain; replay can read them. |
| After Bp or Bk, before unlink | Cleared index records, zeroed infos and `pruned_files` are one durable write; leftover files are unreferenced and the startup scan can remove them. |
| Between blk and rev removal, or after unlink | The durable index references neither file. A remaining half or a reappearing name after a power loss is unreferenced and eligible for the startup scan. |

`flush/machine.rs:579-620,710-715` puts d2 before Bp/Bk and writes the clears there. `:823-855` puts `Unlink` before the release of a manual prune, copy point, or live-copy ALWAYS, and after the durable batch in every case. `:654-665` does not unlink on a failed clear, dropped request, or failed barrier. Core's order is selection/clear, `WriteBlockIndexDB`, then `UnlinkPrunedFiles` (`validation.cpp:3009-3074`); the CH2 split keeps the same durable pre-unlink condition.

## Claims verification

| Claim | Evidence | Result |
|---|---|---|
| p1 plan/apply split has no plan side effects | `flatfile/blocks.rs:692-773`; seeded Core-transcription grid at `flatfile/tests.rs:625-710` | Confirmed. |
| p1 `eligible`, target, re-arm, inclusive bound and IBD margin | Selector source above; five independent killed mutants below | Confirmed. |
| p2 W select/apply/d2/Bp/unlink steps | `flush/machine.rs:508-620,670-855`; one-batch and split tests `machine/tests.rs:869-965` | Confirmed. |
| p2 V clears and `NodeIndex::record` | `chain/index.rs:397-423`, `dash-chain/src/chain.rs:2234-2252`; index tests `chain/tests/index.rs:190-264` | Confirmed on data-bearing entries and file-0 unlinked pairs; header-only dirty detail above. |
| B1 unread-event drop | `flush/driver.rs:263-273,433-456`; `control/tests.rs:1281-1323` | Request drop wakes W; W aborts before apply. A request directly taken and held outside `FlushControl` is outside this production lifecycle. |
| S1 early unlink where W becomes idle at release | `flush/machine.rs:811-855`; six tail cases `machine/tests.rs:1050-1098` | Confirmed, with durable Bp/Bk first. |
| S2 failed clear/latch/refusal | `flush/control.rs:480-500`, `flush/clears.rs:142-172`, `flush/driver.rs:375-397`; `control/tests.rs:1199-1273` | V reports one AbortNode, drops the request; W's latch-registered wait wakes and refuses further writes. |
| BL-90(b) decision state | `coins/cache.rs:459-477`, `flush/control.rs:296-305`; diff from base | Only the log's Σ and B source changes: `core_size_arith` receives the same usage and total as the old `budget.size_state` path. |

## Independent mutation probes

Each mutant changed only `dashd-store/src/flatfile/blocks.rs`, was applied alone from `blocks.rs.saved`, and was restored by `cp -p` plus `touch`. `cmp` passed after every restore and at the end; `git status --short` was empty. The five mutations differ from the implementer's ten listed in the brief.

| Mutant | Focused test result |
|---|---|
| Remove the 10% IBD margin | Killed by `prune_plans_select_cores_files_and_change_nothing`: the mutant stops one file early. |
| Change `tip <= prune_after_height` to `<` | Killed by the same grid at tip 0, after-height 0: mutant prunes nine files where Core selects none. |
| Change initial `usage + buffer >= target` to `>` | Killed by the same grid at equality: mutant selects none where Core selects file 0. |
| Subtract only blk size, omitting undo bytes | Killed by the same grid: mutant selects an extra file. |
| Keep 287 instead of 288 blocks | Killed by `last_prunable_keeps_cores_signed_and_unsigned_arithmetic`: 713 versus Core's 712. |

Logs: `/work/scratch/ch2b-b2-sol-r1/mutant-*.log`.

## Validation and limits

`cargo test --locked -p dashd-store` passed (66 unit tests, 2 fail-stop tests, 1 feature-scope test, 2 WAL tests; ignored tests require a dedicated filesystem). `cargo test --locked -p dashd-chainstate --lib` passed 283/283; the same command with `--features alloc-tag,crash,planted-bugs` passed 300/300. `cargo test --locked -p dash-chain` passed 390 unit tests and 12 doctests. `cargo fmt --all -- --check`, feature-enabled targeted clippy with `-D warnings`, and `git diff --check 8161858bf 797f8d4c5` passed.

`dash-coreimport`'s `blockfiles_parity` test returned pass, but printed `no parity cases`: neither `DASHD_TEST_DATA` nor `ST2_PARITY_DATADIRS` is configured. This is a compilation/smoke check, not fresh byte-for-byte Core evidence. The selector grid is a source transcription of Core, not a live Core execution. CH1-node N7's production call-site wiring, b3 subscriber locks, the full workspace gate and physical power-cut tests are outside this head and this review.

**Verdict:** APPROVE

Sol gate satisfied: yes
