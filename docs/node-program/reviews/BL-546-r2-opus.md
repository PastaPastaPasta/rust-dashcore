<!-- Copy of /work/scratch/ch3/reviews/BL-546-r2-opus.md for reviews/ (DEC-984), 2026-10-31. -->

# BL-546 r2 review (Opus-h): node/ch3-bl546 @ b0ca84330 (r1 head 6f86fced5)

**Verdict: ACCEPT-WITH-NITS.** No blocker, no major, no minor. Two nits, both about docs: r2 n-1 and r2 n-2.

All four r1 findings are closed. M-1's fix sits on the only V-side path where a refused block-file mutation produces a `Fatal`. Every block-file reporter on V now latches exactly the text it reports, for every `FlatFileError` variant and context. The refused-save ruling matches Core rc.4. Each claimed mutant dies for the reason claimed.

## Status of the r1 findings

| r1 | Status | Why |
|---|---|---|
| M-1 | **closed** | `undo_failed`'s `Failed` arm (`connect/serial.rs:1218-1221`) answers `abort_text(latch.tripped())`. `self.abort` is the node's `AbortPath`, and in production its latch is the latch `GuardedFiles` trips (`dashd-node/src/chainstate.rs:380`/`:534` take `engine.latch()`, and `recovery/mod.rs:170` builds the files on the same latch). So the expect cannot fire: `mutate_as` returns `Failed` only after the latch has tripped, either before `op` or by the losing trip. The T12 deferred write goes through the same `mutate_as` and the same arm. The `WLatched` row is timing-independent: W's trip and report happen inside V's own rev write. It reaches the production path (`write_undo` → `mutate_as` → `undo_failed` → `abort_at_fault`) and pins V's Fatal, the sink's one report, and `ProcessNewBlock`'s line. Mutant 1 dies (summary: killed by the undo table). |
| m-1 | **closed** | `save_abort_text`/`save_failed` and `undo_abort_text`/`undo_failed` agree arm by arm (table below). Both latch and report come from one function, so they cannot drift apart. The `abort_text` doc and D221 are corrected. |
| n-1 | **closed** (with r2 n-2) | D221 records it beside the CH7b-2 undo note, as ruled. It names the connect's merge only; see r2 n-2. |
| n-2 | **closed** | The `crash/stores.rs:80` row says what the harness's subscriber guard now does and that the node has no subscriber store. That is accurate: `crash/subs.rs`'s guard latches at `WRITE_SITE`, and V's stop is UnlessLatched. |

## Findings

### n-1 (r2): `GuardedFiles::mutate`'s new doc line is false for the crash harness's two callers

- **Where:** `dashd-chainstate/src/store/guarded.rs:518` ("A failure it latches names the error's text, which its caller reports").
- **Sequence:**
  1. In `crash/chain.rs:510-513` (save) and `:670-673` (undo), the harness calls `s.files.mutate(…)`, which latches `e.to_string()`, for example `Failed to write block: <path>: <err>`.
  2. `Stores::files_failed` (`crash/stores.rs:565-576`) then reports Core's step text: the context, `DISK_SPACE_LOW`, or `BLOCK_WRITE_FAILED`/`UNDO_WRITE_FAILED`.
  3. So the latch and the report differ there.
- The DEC-969 sweep kept the harness model unchanged, and its tables accept "any of its path's messages", so nothing fails. Only the new contract sentence is wrong. The production callers of `mutate` are W's `Halt::files_at` (reports `e.to_string()`) and the prune host (its closures return `Ok`), and both match it.
- **Fix:** scope the sentence ("…which W's callers report"), or have the harness use `mutate_as` with `files_failed`'s text. Either is fine. This is harness-only, the same class as r1 n-2.

### n-2 (r2): D221's n-1 note points at the connect's merge, but the CH7b-2 case it sits beside is the undo's

- **Where:** `docs/node-program/designs/CH3b-notes.md:49` (D221, the sentence beginning "With it, when an apply reports…"). The code is at `connect/connector.rs:414-420`.
- **Sequence:** CH7b-2 lets an undo emit a snapshot (D221's preceding sentence).
  1. A disconnect returns an unreported `Err(f)`.
  2. Its `finish_attempt` applies the snapshot, and the snapshot write reports.
  3. The disconnect's own inline merge `(Err(f), Err(apply)) => Err(f)` (`:416-419`) keeps `f`. It does not call `finished_outcome`.
- So the note sends a CH7b-2 implementer to `finished_outcome` ("a connect's unreported `Err`") and misses the merge their undo goes through. r1 n-1 named both (`:127-139` and `:413-420`).
- **Fix:** name both merges: "`finished_outcome` and the disconnect's merge (`connect/connector.rs`)".

## Answers to the brief's questions

**1. Is M-1 fixed on every path, and is any other V-side refused mutation still answering its own text?**

These are all the V-side mutations, and how a refusal answers for each:

- **`write_undo` and T12's deferred write.** Fixed; this is the M-1 arm.
- **`NodeBlockFiles::save_block`.** A refusal gives `Ok(None)`, which becomes AcceptBlock's state "AcceptBlock: Failed to find position to write new block to disk" (`chain/accept.rs:319-327`). That is not a Fatal text. V's Fatal comes from its next latch read through `site.rs`'s `reported()`, which gives `abort_text`.
- **The prune host's `check_due` and `arm`.** A refusal gives `false` or `()`, with no text.
- **`store_quorum_snapshot`.** `write_failed` answers `abort_text(latch)` for the refusal too (`flush/machine.rs:242-255`).

Other checks:

- A grep of every `mutate(`, `mutate_as(` and `FlatFileError::Failed` in the workspace finds no other V-side consumer.
- dashd-node and rdashd don't call `GuardedFiles` directly.
- `dash-kv`'s `LatchedEngine` isn't used by dashd-chainstate or dashd-node.

**2. Does every block-file reporter latch exactly the text it reports?**

I traced the contexts `save_block_inner`, `find_block_pos`, `write_undo_inner` and `find_undo_pos` can produce (`dashd-store/src/flatfile/blocks.rs:406-625`). They are `NoSpace`, plus `Io` with these contexts: `DISK_SPACE_LOW` from `allocate` (with or without a failed open), `WRITE_BLOCK_FAILED`/`WRITE_UNDO_FAILED`, and `FLUSH_BLOCK_FAILED`/`FLUSH_UNDO_FAILED` (the rollover in `flush_block_file_inner` and the lagging rev file's flush).

| Error | Save: latch = report | Undo: latch = report |
|---|---|---|
| `NoSpace` | `DISK_SPACE_LOW` (user `DISK_SPACE_LOW`) | `DISK_SPACE_LOW` (user `DISK_SPACE_LOW`) |
| `Io{DISK_SPACE_LOW}`, open failed | `WRITE_BLOCK_FAILED`, via `write_failed(e, &text)` | `WRITE_UNDO_FAILED` |
| `Io{DISK_SPACE_LOW}`, no open | `System error: {source}`; AcceptBlock's `Fatal::new("System error: {msg}")` with `msg = source.to_string()` is the same string | `WRITE_UNDO_FAILED` |
| `Io{WRITE_*}` | `WRITE_BLOCK_FAILED` | `WRITE_UNDO_FAILED` |
| `Io{FLUSH_BLOCK_FAILED}` (save rollover) | the context | n/a |
| `Io{FLUSH_UNDO_FAILED}` | the context (save rollover) | `FLUSH_UNDO_FAILED` |
| `Corrupt` | `e.to_string()` | `WRITE_UNDO_FAILED` |
| `Failed` | never latched (`mutate_as` returns it only when its trip lost) | never latched |

W's block-file steps latch `e.to_string()` through `mutate` and report `e.to_string()` through `Halt::files_at`, so they match.

**Can the first trip come from another reporter whose text V would then misread?**

- `AbortPath::abort_node` latches and reports `log_msg`.
- The chainstate guard's sites are mapped by `abort_text`.
- The flush-site, panic, read and `dbcrashratio` sites latch their report's text (r1).
- If another store trips between a failing block-file `op` and `mutate_as`'s trip, the trip loses. The caller then gets `Failed` and reports nothing, and V reads the other reporter's text. The save gives AcceptBlock's state; the undo is M-1's arm.
- The only reporters that don't latch their own text are the crash harness's (r2 n-1). They aren't in the product.

The dashd-node `steps.rs` `debug_assert` (report text = Fatal text at the shutdown flush) now also holds after a block-file failure. At r1's head, a failed block write followed by the shutdown flush would have compared "Failed to write block" with `Failed to write block: <path>: <err>`.

**3. Is the refused-save ruling right against Core rc.4?**

Yes:

- `validation.cpp:4843-4846` (rc.4): a null `SaveBlockToDisk` position always becomes `state.Error("AcceptBlock: Failed to find position to write new block to disk")`, whichever `AbortNode` was reported underneath:
  - `FindBlockPos`'s "Disk space is too low!" at `blockstorage.cpp:713-716`;
  - `SaveBlockToDisk`'s "Failed to write block" at `:869`.
- `ProcessNewBlock` logs `AcceptBlock FAILED (%s)` with that state (`:4898`).
- Only the `runtime_error` catch gives `AbortNode("System error: " + e.what())` (`:4849-4850`), and rdashd's `Err` path keeps that.
- Core has no latch, so a refused save has no exact Core analogue. Answering with Core's state for a failed save, and leaving the Fatal to the funnel, is the right model.
- D221's `v:4845` cite is exact.

**4. Do the tests reach the production path, and does each mutant die?**

I read the mutant patches in `/work/scratch/ch3-bl546/mut-r1` and the summary:

- **1** dies through the `WLatched` row's Fatal and line asserts. The `Latched` row's assert, now `OTHER_STORE_FAILED`, would also catch it.
- **2** dies through the four non-NoSpace save tests and `a_failed_block_write_is_the_text_v_stops_on`. That test's exit Fatal is V's latch read, not a constant.
- **3** dies through the undo table's Write and Flush rows; NoSpace is equivalent.
- **4** dies through the snapshot test's `(text, None)`.

Cheap mutants I checked by reading:

- Dropping the `open_failed().is_none()` guard: killed by `a_blk_file_that_cannot_be_created_fails_the_write`'s report assert.
- Narrowing `WRITE_BLOCK_FAILED | DISK_SPACE_LOW` to `WRITE_BLOCK_FAILED`: killed by the same test.
- `undo_abort_text`'s flush arm to `WRITE_UNDO_FAILED`: killed by the Flush row.
- The M-1 arm returning the raw `latched.message`, without `abort_text`: killed by the `WLatched` row.
- The Corrupt arms of either text function survive any change, but both sides change together. `Corrupt` isn't reachable from `save_block_inner` or `write_undo_inner`; it comes only from the public `flush_undo_file` and from reads. So these mutants aren't worth a test.

**5. Are the D221, n-1 and n-2 texts accurate?**

- **D221:** accurate, apart from r2 n-2. Its r1 mutant list matches the summary.
- **The `abort_text` doc:** accurate.
- **The n-2 row:** accurate.

## What I checked

- `git diff 6f86fced5 b0ca84330` in full; the worktree is at b0ca84330 and clean.
- In dashd-chainstate:
  - `store/guarded.rs` (`mutate`/`mutate_as`);
  - `store/blocks.rs` (`save_failed`, `save_abort_text`, `save_block`);
  - `connect/serial.rs` (`write_undo`, `undo_failed`, `undo_abort_text`, `t12_defer`);
  - `chain/accept.rs:318-340`, `chain/load.rs:266`, `chain/seams.rs` `AbortPath`;
  - `flush/machine.rs` (`abort_text`, `write_failed`, `Halt::files_at`, every W `mutate` caller), `flush/prune.rs`'s callers;
  - `connect/connector.rs` merges;
  - `crash/stores.rs` and `crash/chain.rs` `files_failed`.
- `dashd-store/src/flatfile/blocks.rs`: the error contexts and the internal failed flag, which is set only on an error the guard latches.
- dashd-node: the latch wiring.
- Core: `git show v24.0.0-rc.4:src/validation.cpp` (AcceptBlock, ProcessNewBlock) and `src/node/blockstorage.cpp` (FindBlockPos, WriteBlockToDisk, SaveBlockToDisk).
- Mutant logs: `/work/scratch/ch3-bl546/logs/mut-r1/summary` and the copy's patch.
- I built and ran nothing; everything was settled by reading. I modified nothing.
- Outside BL-546, not a finding: `load_genesis_block`'s `Ok(None)` is an init-time `Fatal::new("AddGenesisBlock: …")`, not one of V's latch reads.
