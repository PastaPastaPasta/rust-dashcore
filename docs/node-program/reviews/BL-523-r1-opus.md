# ND1 BL-523 review, round 1 (Opus)

Branch `node/nd1-bl523`, head 46c914fa8 on base 7beef58c1. Read-only review of the frozen worktree; nothing was built.
Core reference: v24.0.0-rc.4 and pin 375c64d2317 (rc.4+151, oracle hooks only). `node/blockstorage.cpp`
and `node/chainstate.cpp` differ from the tag only in `#ifdef DASH_ORACLE_HOOKS` blocks, and `LoadBlockIndex`'s
`!fReindex` block is the same text at both. The branch cites lines at the pin, and every cite I checked is right:
`validation.cpp:5470-5473`, `:5493`; `blockstorage.cpp:395`, `:410`, `:412`, `:429`, `:635-646`; `chainstate.cpp:200-204`.

## Verdict: ACCEPT-WITH-NITS

0 M, 1 m, 3 n.

I found no datadir where Core starts and rdashd fails, and none where Core refuses and rdashd starts. The order
at open is Core's: blk-file check, then the flag line, then the scan, then the second-pass poll, then the genesis
and devnet checks, then the `-prune` state refusal, all before `LoadGenesisBlock`/`ReplayBlocks`. Merged with
BL-528, the CH5 reindex refusal still comes before all of that.

## Findings

### m-1: M5 (`0..=last_file`) survives the branch's tests. The plan's "size-0 info at `last_file`" test is missing.

`dashd-store/src/flatfile/blocks.rs:851` (the scan's range), made live by `dashd-chainstate/src/chain/load.rs:194`/`:223`.

PLAN §5 test 2 promised "a size-0 info at `last_file` is never unlinked", and §6 lists M5 as killed by it. No
test on the branch does that:
- The `Recording` tests in `chain/tests/blocks.rs` wrap `SharedFiles`, which is `Arc<Mutex<MemBlockFiles>>`
  (`support.rs:507`). Its scan is a no-op (`seams.rs:487-490`), so they record the call but never run ST2's
  range.
- `a_restart_after_a_prune_ended_at_its_unlink_removes_its_files_then_refuses` (`node_stores.rs`) has a last file
  that holds the newest blocks, so its size is non-zero.
- Both NodeSut tests (`sut.rs`, `a_pruned_chainstate_is_scanned_then_refused` and
  `an_empty_pruned_chainstate_is_scanned_then_refused_before_genesis`) set `last_file = 2` but plant only
  `blk00001.dat`/`rev00001.dat`.
- The one dashd-store scan test (`flatfile/tests.rs:1028`) also has a non-empty last file.

**Failing sequence under the mutant.** Change `(0..self.last_file)` to `(0..=self.last_file)`, then:
1. Plant `blk00002.dat` and `rev00002.dat` on a chainstate with `last_file = 2`, no info for file 2, and
   `pruned_files = 1`.
2. Start the node.

Core keeps both files (`for (file_number = 0; file_number < m_last_blockfile; …)`, `blockstorage.cpp:435`). The
mutant deletes them, and every BL-523 test still passes. R9 in the crash model checks only that pruned files
*below* the last file are gone, never that the last file is kept. I did not run the toy/crash suites, so they
might still catch it by chance.

**Fix (cheap):** in `an_empty_pruned_chainstate_is_scanned_then_refused_before_genesis`, also plant `blk00002.dat`
and `rev00002.dat`. Assert they stay, and make the "no genesis block written" check exclude them, or compare
their contents. You could also add them to the flagged case of `a_pruned_chainstate_is_scanned_then_refused`.

### n-1: on the empty-index paths, a refused scan ignores a requested shutdown. The stored path maps it to `Interrupted`.

`dashd-chainstate/src/chain/load.rs:223` (`scan_already_pruned_files(files, have_pruned)?`) and
`dashd-node/src/chain.rs:134`.

The stored path maps a refused scan through `failed()` (`load.rs:194`). That gives `Interrupted` when shutdown is
requested, which is Core's `chainstate.cpp:184` rule for a false `LoadBlockIndex`. Both empty-index paths use `?`
instead, so they always give `LoadingBlockDb` ("Error loading block database" plus the reindex question).

**Sequence:**
1. Start on an empty index with `pruned_files = 1`.
2. The guard's latch has already tripped, and the abort sink has requested shutdown.

Result: `LoadingBlockDb` here, but `Interrupted` on a stored index in the same state.

Core has no such path, and in production the latch can't be tripped before this point (steps 0–3 fail the open
themselves). So this is a consistency nit, not a parity break. **Fix:** use the same shutdown check (`failed()`,
or an equivalent in `open_parts`), and pin it in the `ScanRefused` test with the shutdown requested.

### n-2: `load_until`'s second pass still polls before the blk-file check and the scan.

`dashd-chainstate/src/chain/load.rs:188-200` with `chain/index.rs:487-490`.

Core runs its trial pass after `ScanAndUnlinkAlreadyPrunedFiles` and polls at `validation.cpp:5493`. rdashd
runs that pass inside `load_until`, before `block_files_present` and the scan. The new poll after the scan stands
for Core's poll, as the comment says.

**Sequence:** with `pruned_files = 1` and a stored index, shutdown is requested while `load_until`'s second pass
runs. rdashd returns `Interrupted` without scanning. In Core that pass runs after the scan, so the scan has
already unlinked the files first.

The outcome is `INTERRUPTED` either way, and the unlink is best-effort and retried on the next start. The only
difference is leftover pruned files on disk. No change needed; the comment at `load.rs:196-197` could say that
the earlier polls in `load_until`'s second pass interrupt before the scan.

### n-3: D77's reindex clause is correct but could be more precise.

`docs/node-program/designs/CH1-node-notes.md`, D77, and the same text in `load.rs` (the doc on
`scan_already_pruned_files`).

"Where Core would scan and then resume the reindex" is exact only with `prunedblockfiles` clear, where the scan is
a no-op, or set together with `-prune`. With the flag set and no `-prune`, Core scans and refuses at
`chainstate.cpp:202`, as the parenthesis says. Optional rewording: "Core would run the scan (a no-op without
`prunedblockfiles`), then resume the reindex, or, with `prunedblockfiles` set and no `-prune`, refuse at
`node/chainstate.cpp:202`".

## What I checked

1. **Order.**
   - Startup sequence (`steps.rs:280-360`, combined with dd3531df5 via `git merge-tree`, which is clean):
     `coins()`, then `ChainstateOpen::open` (steps 0–3: WAL sync, meta read, a resumed wipe only with `wiping`
     set, which is pre-existing CH5 behaviour), then BL-528's `-reindex`/`meta.reindexing` refusal, then
     `open_parts`. Inside `open_parts`: the shutdown poll, then the blk-file check (or the empty-index line), the
     flag line, the scan, the post-scan poll, the genesis and devnet checks, and `check_prune_state`. Then
     `LoadGenesisBlock`, `opening.recover` (ReplayBlocks) and `LoadChainTip`.
   - Nothing between `open()` and the refusal writes, apart from the scan's unlinks. The `Recording` tests pin
     "save" only after a passing prune check, and the NodeSut empty test pins that no genesis file is written.
   - Reindex flag plus `pruned_files`: rdashd refuses with CH5's Unsupported text (Incompatible, no question)
     before the scan. Core scans, then gives FAILURE with `PRUNED_DATADIR` and the question. Both refuse, so the
     rule allows it, and rdashd's text is the more accurate one for a node that can't reindex. The only side
     effect is leftover already-pruned files, which Core would have unlinked. D77 describes this correctly (n-3).
2. **The scan.**
   - Range `0..last_file` (excludes the last file, as Core's `<`). Predicate `size == 0`, Core's `nSize == 0`,
     not "no data, no undo".
   - The infos come from `file_infos(engine, meta.last_file)`, with defaults for missing records, as Core's
     `ReadBlockFileInfo` loop does.
   - A failed remove is a WARN plus skip (`unlink_pruned_files_inner`). Core's `ec` overload ignores it.
   - Log lines: the debug `blockstorage` line `Prune: UnlinkPrunedFiles deleted blk/rev (%05u)` once per file
     where either remove succeeded, and the info line `LoadBlockIndexDB(): Block files have previously been
     pruned` after the check line.
   - The extra dir sync and its WARN are pre-existing and accepted (DEC-212).
   - `LoadingBlockDb` needs `have_pruned` (the helper returns `Ok` before `mutate` otherwise). With the flag set,
     rdashd refuses anyway, and so does Core without `-prune`, which rdashd rejects at argument parsing. So no
     datadir starts in Core and fails here. Only the text differs, in an unreachable latch-tripped state.
3. **The refusal.**
   - The gate is `meta.pruned_files` (`steps.rs:350`), not `-prune`.
   - Absent key gives false (`meta.rs:204`). A non-0/1 byte is rdashd's own store's corruption error at step 1,
     which is pre-existing and not Core's format.
   - Production writers: only the flush machine's `put_clears`/T13. The node runs `NoPrune`
     (`chainstate.rs:580`), `pruneblockchain` always errors "not in prune mode" (`dashd-rpc stats.rs:467-470`),
     and dash-coreimport doesn't read or write the flag.
   - `PRUNED_DATADIR` is byte-exact, two spaces included (the `\` continuation strips only the next line's
     indent). It maps to `OpenError::Recoverable`, which is FAILURE, so it gets the reindex question
     (`init.cpp:2139-2147`), and the NodeSut test asserts `reindex_question(PRUNED_DATADIR)`.
   - The order after the genesis checks is pinned by the mainnet case (Incompatible, but the scan ran).
   - `VParts.have_pruned = false` is correct, since a flagged load never reaches V.
4. **Shutdown move.** `:5493` is the first poll after `:5473` (the trial loop's first entry, after the
   snapshot-base check, which has no rdashd counterpart). The new poll matches it. A missing file with shutdown requested still gives `Interrupted`
   without a scan (Core: `LoadBlockIndexDB` false, then `check_interrupt`). The empty index has no post-scan poll,
   which is also Core's (no loop entries). See n-2 for the polls inside `load_until`.
5. **Tests and mutants (by reading).**
   - M1 (drop the call): `the_startup_scan_…` calls, `a_restart_after_a_prune_…` files, NodeSut. Killed.
   - M2 (`false` in steps.rs): NodeSut flagged case. Killed.
   - M3 (gate ignored, in the helper or the chain wrapper): NodeSut clear case (files would go) and `scan false`
     records. Killed.
   - M4 (after the genesis checks or `recover`): mainnet case `["open 0", "scan true"]` plus Incompatible.
     Killed.
   - M5: **survives** (m-1).
   - M6 (a failed remove fails the start): ST2's `unlink_failures_are_logged_and_skipped`. A mutant that ignores
     the load's Err is killed by `a_refused_startup_scan_fails_the_load`, `latched_block_files_…` and
     `open_parts_scans_an_empty_index_…`.
   - M7 (empty paths): `an_empty_index_is_scanned_…` and the `ScanRefused` test. Killed.
   - Other mutants killed: the post-scan poll dropped, the prune check before the genesis checks, the flag line
     after the unlinks or logged unconditionally, PrunedDatadir mapped to Incompatible.
   - Cheap survivor besides M5: `load.rs:194` `failed()` replaced by `LoadingBlockDb` (no test refuses the scan
     with shutdown requested; see n-1).
   - The plan's claim that crash R9 kills M1 doesn't hold, because the crash recovery calls
     `store::scan_already_pruned_files` directly, not `open_block_index`. D77 states this correctly, and other
     tests kill M1.
6. **Merge with BL-528 (dd3531df5).** `git merge-tree --write-tree 46c914fa8 dd3531df5` is clean (a46c02f1a). In
   the combined `steps.rs`, the `-reindex || meta.reindexing` refusal sits right after `let meta` and before
   `index_records`/`open_parts`, so the order is reindex refusal, then scan, then the prune refusal. BL-528's new
   comment ("refuses … before its index load, having written nothing") stays true, because the scan comes after
   it.
