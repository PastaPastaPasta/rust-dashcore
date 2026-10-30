# SB14 fix a2ebe1605 (CFHeaders reply matching): Opus-h narrow review, round 1 (two Sol attempts died)

Reviewer: Opus (narrow review). Worktree `/work/scratch/sb14-fix-r1-sol/wt`, parent `470b30656`.
Files: `dash-spv/src/sync/filter_headers/pipeline.rs`, `dash-spv/src/sync/filter_headers/manager.rs`.

## Verdict

**ACCEPT WITH NITS.** The fix is correct and minimal. Matching on (stop hash, header count) identifies
a batch uniquely. An ignored stale reply can't stall the pipeline, because the 20 s timeout/retry path
doesn't depend on the ignored message. Both new tests fail when the check is removed. There's no
behaviour change for an honest peer in normal sync. None of the nits blocks the merge.

## Findings

### N1 (nit, optional): `advance` still trusts the reply's count, not the batch length
- **Where:** `sync_manager.rs:64`, `:72` and `:94`, `:101` (`advance(count)` with `count = filter_hashes.len()`); `pipeline.rs:268-270`.
- **Scenario:** before the fix, a reply whose count differed from its batch moved `next_expected`
  off the batch-start grid. After that, no buffered batch (keyed by start height, `pipeline.rs:257`)
  could ever be popped, so this was a second stall mode next to the wrong-heights storage. The new
  check at `pipeline.rs:232` makes `count == batch length` an invariant, so this is now unreachable.
  But the invariant is only implied: nothing at the `advance` call sites states it.
- **Fix (optional):** add a one-line comment or `debug_assert!` where `receive` and `advance` run,
  noting that `match_response` guarantees the count equals the batch length. No code change is needed
  for correctness.

### N2 (info): a non-conforming peer now produces a quiet retry loop instead of corruption
- **Where:** `pipeline.rs:232-240` (debug-level log), `pipeline.rs:290-294`, and
  `download_coordinator.rs:120` (no retry cap).
- **Scenario:** if a peer always answers a batch with a different number of hashes (Dash Core never
  does: it returns exactly `stop - start + 1` and refuses ranges of 2000 or more, while our batches
  are at most 2000, `pipeline.rs:19`), the batch now times out every 20 s and is retried without
  limit. The per-retry `warn!` is logged, but the mismatch reason only appears at `debug`. That's a
  strictly better failure than the old behaviour, which stored headers at the wrong heights. I'm
  noting it, not requesting a change. If you want the cause visible in a field log, raising the
  "Ignoring CFHeaders" line to `warn!` for retries only (retry count > 0) would cover it. That isn't
  needed for this PR.

### N3 (nit): the manager-level replay only covers the too-long direction
- **Where:** `manager.rs:502-602` (the test). The too-short direction is covered only at the pipeline
  level, `pipeline.rs:568-573`.
- **Scenario:** the mirror case is real too. Suppose the old queue had `3000..=4000` (1001 headers,
  stop at 4000) in flight and the new queue has `2001..=4000`. Before the fix, the short reply was
  stored at `2001..=3001` and `next_expected` went off the grid. The pipeline unit test pins this
  path, so manager-level coverage is optional.
- **Fix (optional):** none required.

No correctness, liveness or regression findings.

## Requested checks

### 1. Is matching unambiguous?
Yes. Per BIP157, a CFHeaders reply with stop hash `S` (at height `e`) and `N` hashes covers
`e-N+1..=e`. A block hash fixes the height, so (stop hash, count) determines the range. The pipeline
keeps one range per stop hash (`pipeline.rs:40`), inserted at `pipeline.rs:100` and `:155`, and
accepts a reply only when `len == end - start + 1` (`pipeline.rs:232`). The sub-cases:
- **Same stop hash and same count, different range:** impossible, for the reason above. A stale
  reply that passes the check covers exactly the new batch's range. Since `S` fixes the chain up to
  `e`, its contents are also what the new request would return. The new request's own later reply
  then isn't in flight (`pipeline.rs:219`, after `receive` at `:250-251`) and is dropped, which is
  harmless.
- **Overlapping requests after repeated resets:** the old queues A and B may both have in-flight
  requests ending at `S` with different starts. Only the one whose length equals the current batch's
  is accepted, and by the point above it is that batch's answer. `init` clears `batches`
  (`pipeline.rs:138`) and `on_disconnect` replaces the whole pipeline (`sync_manager.rs:35`), so no
  stale range survives in the map. Within one pipeline the ranges are disjoint (`init` builds
  `start..=target` in steps; `extend_target` builds only `old_target+1..`, `pipeline.rs:87`), so no two
  live batches share a stop hash and the `insert` never overwrites.
- **Checkpoint/genesis first batch:** the range is `start..=min(start+1999, target)` with
  `start = 0` or the checkpoint height (`manager.rs:157-160`). A peer answers with exactly that many
  hashes, so the first batch is accepted. The checkpoint prev-header store (`manager.rs:120-135`)
  depends on `start_height`, which now comes from a correctly matched batch.
- **Final partial batch:** `batch_end = min(current+1999, target)` (`pipeline.rs:91`, `:146`). The
  range length equals what the peer returns, so it's accepted.
- **u32 arithmetic:** `end - start` can't underflow because every range is built with
  `current <= batch_end`. Widening to `u64` before `+ 1` avoids overflow at `u32::MAX`.

### 2. Liveness
The request's timeout and retry still fire. An ignored reply returns `None` before any state changes
(`pipeline.rs:240`): `coordinator.in_flight` keeps its `mark_sent` timestamp
(`download_coordinator.rs:148`) and `batches` keeps the range. `tick` calls `handle_timeouts`
(`sync_manager.rs:154`). That goes through `check_timeouts` (`download_coordinator.rs:189`), which
removes entries older than 20 s (`pipeline.rs:26`), and then `enqueue_retry`
(`download_coordinator.rs:120`). Next, `send_pending` (`sync_manager.rs:157`) reissues the request
using the range that was kept (`pipeline.rs:181`). Retries are uncapped, so an ignored reply can't
leave the pipeline waiting forever while ticks run. The normal case doesn't even need a timeout: the
peer answers the new request too, and that answer matches. A single-peer drop
(`on_peer_disconnect` → `requeue_in_flight`, `pipeline.rs:285`) keeps `batches`, so requeued batches
still have their ranges. The `is_complete` check on the no-match path (`sync_manager.rs:50-56`) is
unchanged and can't fire spuriously, because the batch is still in flight.

### 3. Tests
- With the fix, all 19 `sync::filter_headers` tests pass.
- **Mutation:** I changed the check at `pipeline.rs:232` to `if false && …`, which matches by stop
  hash only, as before the fix. Both new tests fail:
  - `test_match_response_requires_the_batch_it_names` panics at `pipeline.rs:562`: the stale
    2000-header reply was taken for the 1-height batch.
  - `test_stale_cfheaders_after_a_reset_do_not_land_at_the_wrong_heights` panics at
    `manager.rs:590`: `FilterHeadersStored` reported `end_height > 4000`, i.e. the old reply was
    stored at 4000..=5999. That is the dashd failure, reproduced.
  - The other 17 tests still pass. I restored the file with `git checkout`, and `git status` is clean.
- The pipeline test covers both directions: too long (2000 hashes for `16000..=16000`, `:562`) and
  too short (1 hash for `14001..=16000`, `:571`). Each direction is followed by the batch accepting
  its own answer, so a one-sided `<`/`>` check would also fail it.
- The manager test models the real sequence faithfully. Real code goes `stop_sync` →
  `WaitingForConnections` + `on_disconnect` (`sync_manager.rs:118-121`), then `start_sync` →
  `WaitForEvents` (`sync_manager.rs:108-113`), then `start_download`. The test's
  `on_disconnect(); set_state(WaitForEvents)` takes the same path.

### 4. Behaviour outside the reset path
None, for a conforming peer.
- `send_pending` uses `batch.start()`, the same value `batch_starts` used to hold
  (`pipeline.rs:181`, `:188`).
- `receive` removes the same key as before (`pipeline.rs:251`).
- Batch boundaries are built by the same loops; only the stored value changed, from `u32` to
  `RangeInclusive<u32>`.
- Progress counters (`add_processed`, `manager.rs:141`; `update_current_height`,
  `sync_manager.rs:73`, `:103`) only move when a reply is accepted. Honest replies are accepted as
  before. Stale mismatched replies no longer advance them, which is the intended change.
- The other edits are renames: the field, the error text "No batch entry for pending stop_hash"
  (`pipeline.rs:183`), the test `test_send_pending_errors_on_missing_batch`, and doc comments. The
  existing tests' fixtures were updated to real ranges (`1..=2000`, `2001..=4000`).
- The peer-level response timer in `network/manager.rs:2050` is cleared by any CFHeaders, stale or
  not, both before and after this commit.

## Validation
- `CARGO_TARGET_DIR=/work/scratch/sb14-fix-r1-sol/target RUSTC_WRAPPER= cargo test -p dash-spv --lib -j 8 sync::filter_headers`
  at `a2ebe1605` gives 19 passed, 0 failed.
- The same command with the count check mutated out gives 17 passed, 2 failed (the two new tests, at
  `pipeline.rs:562` and `manager.rs:590`). I restored the file afterwards and the worktree is clean.
- I didn't run the dashd integration test (`test_sync_with_peer_disconnection`); that's out of scope
  for this narrow round.
- I edited no source files and made no commits or pushes.
