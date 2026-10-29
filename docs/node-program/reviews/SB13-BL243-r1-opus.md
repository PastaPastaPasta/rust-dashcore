# BL-243 r1 review: 96e0d772c (range e1e3ee787..96e0d772c)

Reviewer: Opus (stand-in for Sol). Scope: concurrency of {disconnect, reconnect, completion} as seen by the block-headers, filter-headers and (because it consumes the same event) masternodes tasks.

## Verdict: CHANGES

The filter-headers half is correct. I traced every interleaving I could build (below) and found none where the epoch gate hangs or finishes early, provided the two counts agree. They agree in every case except one storage-error edge (F2), which also existed before this commit.

The commit does not deliver what it says ("SyncComplete never fired" fixed), though. **`MasternodesManager` has the same race on the same event, and the commit leaves it as it was** (F1). Masternodes are on by default (`ClientConfig::enable_masternodes: true`), and their state counts toward `SyncProgress::is_synced`. So in the default production config the same interleaving still hangs SyncComplete for good. The dashd test only stopped hanging because `tests/dashd_sync/setup.rs:310,323` build the client `without_masternodes()`. The fix for masternodes is small and reuses this commit's mechanism, so it belongs in the same change, or else in an explicit, tracked follow-up the manager agrees to.

## Findings

### F1 (major): MasternodesManager strands in `WaitForEvents` on the same interleaving

- `dash-spv/src/sync/masternodes/sync_manager.rs:323-341`: the `BlockHeaderSyncComplete` arm matches `..` and ignores `connection_epoch`. It only (re)starts on WaitingForConnections/WaitForEvents, on Syncing stuck at height 0, or on Synced-but-behind.
- MasternodesManager uses the default `start_sync` (`sync/sync_manager.rs:108-114`). On reconnect it goes to `WaitForEvents` and waits for a `BlockHeaderSyncComplete`.
- `tick` returns early outside Syncing|Synced (`masternodes/sync_manager.rs:413`). `BlockHeadersStored` only acts in `Synced` (`:277`).
- BlockHeadersManager enters `Syncing` only in `start_sync` (`block_headers/sync_manager.rs:64`), and `finalize_sync_if_complete` emits only from `Syncing`. So it emits **one** `BlockHeaderSyncComplete` per connection epoch.

Failing interleaving (masternodes caught up and `Synced` at epoch 0, no new blocks; the same timing as the original filter-headers hang):

1. Network broadcasts `PeersUpdated(0)`, then `PeersUpdated(1)`. Both are queued on every task's network receiver.
2. The BH task handles both (epoch 1, then `start_sync`), gets the peer's headers reply, and emits `BlockHeaderSyncComplete{tip, epoch 1}`.
3. The MN task's `select!` picks the sync receiver first. It is in `Synced` with `current == tip`, so `should_restart` is false and nothing happens.
4. The MN task handles `PeersUpdated(0)`: `stop_sync` puts it in `WaitingForConnections`.
5. The MN task handles `PeersUpdated(1)`: default `start_sync` puts it in `WaitForEvents`.
6. No further `BlockHeaderSyncComplete` comes until the next total disconnect. `tick` is a no-op and `BlockHeadersStored` is ignored outside `Synced`. MN stays in `WaitForEvents`, so `is_synced()` stays false and **SyncComplete never fires**.

Proof: the probe test `bl243_probe::probe_completion_before_its_disconnect_strands_masternodes` drives exactly these handler calls (plus 40 ticks and a `BlockHeadersStored`) and asserts the manager ends in `WaitForEvents` with no request sent. It ran in a throwaway copy, since deleted; the reviewed worktree was not touched. The source is kept at `/work/scratch/bl243-r1-sol/bl243_probe.rs`. Result: it passes, so the manager does strand (see "What I ran").

Suggested fix: give `MasternodesManager` the same two pieces as filter headers. (a) Count `on_disconnect` calls and keep the latest completion's epoch. (b) Override `start_sync` so that when `held_epoch >= own_epoch` it resumes with the same branch the event handler takes for WaitForEvents (`send_qrinfo_for_tip`). A small shared `HeaderCompletionGate { connection_epoch, synced_epoch: Option<u64> }` with `on_disconnect()`, `record(epoch)` and `synced()` would keep the two managers from drifting. Add a unit test for this ordering, like `test_header_completion_handled_before_its_disconnect_is_kept`.

As a side effect, the epoch also fixes the reverse MN case. Today a stale pre-disconnect completion handled in `WaitingForConnections` makes `should_restart` true and sends a QRInfo while the manager thinks it has no peers (`:340`).

### F2 (minor; it predates the commit, but the epoch invariant now rests on it): a fallible write runs before the epoch bump in the BH network handler

`dash-spv/src/sync/block_headers/sync_manager.rs:254-260`. The BH `PeersUpdated` arm calls `store_last_target_height(best).await?` **before** `if *connected_count == 0 { self.stop_sync() }`. Filter headers use the default handler, whose `update_target_height` cannot fail, so its `stop_sync` always runs.

`PeersUpdated{connected_count: 0, best_height: Some(_)}` can happen. In `network/manager.rs` a new peer is put in the pool (`pool.add_peer`) before `connected_peer_count.fetch_add`. A removal that runs in that window decrements to 0 and reads `pool.get_best_height()` with the new peer already in the pool.

Failing interleaving: the removal emits `PeersUpdated{0, Some(h)}`, and the metadata write fails (disk full or I/O error). BH returns `Err` before `stop_sync`, so its epoch stays at n. FH increments to n+1. From then on every BH completion carries an epoch below FH's count, so `block_headers_synced()` is false for good and filter headers hang. That is the BL-243 symptom again, now made permanent.

Before this commit the same error also left BH `Synced` without re-syncing, so this is not a regression. But the comment now says "they all count the same disconnects", and this path breaks that claim. Fix: do the `connected_count == 0` / `stop_sync` branch first, or don't let the metadata write `?` out of the handler (log it instead).

### F3 (nit, existing behaviour): a lagged receiver ends the task silently

`dash-spv/src/sync/sync_manager.rs:305-309,332-336`: `Err(RecvError::Lagged)` breaks the loop and `run` returns `Ok(identifier)`. `SyncCoordinator::tick` (`sync_coordinator.rs:267-268`) logs that at **debug** as "completed successfully" and carries on. So a lag gives the same symptom as BL-243 (sync never completes) with only one `error!` line in the log.

This is unchanged since #411, and with 10 000-slot channels (`network/manager.rs:37`, `sync_coordinator.rs:30`) it is unlikely, so it is not a new failure mode. Answering brief Q2: the brief lists "a task that lags behind exits" as part of the fix, but the diff does not touch it. The fix relies on an existing property; it adds nothing here. Suggest a follow-up: return `Err` on `Lagged` so `tick` turns it into a client error and `on_error`, not a silent stall.

### F4 (nit): count agreement depends on spawn order, which nothing documents

`dash-spv/src/sync/sync_coordinator.rs:39-40` (`spawn_manager!`) subscribes each manager's network receiver in turn, with an `.await` (`message_receiver`) between managers. BH is spawned before FH (`:228-229`). So if a network event were ever sent during spawning, only BH's count could come out higher. That direction is the safe one: FH accepts a stale completion early, which is the old #631 behaviour, not a hang. Today the network starts only after `coordinator.start` (`client/lifecycle.rs:215-229`), so nothing is sent during spawning anyway.

The new `events.rs` doc states the invariant flatly. Either subscribe every network receiver before spawning any task, or add a one-line comment at the spawn site that BH must stay first. Separately, and unchanged by this commit: `sync_event_receiver` is subscribed lazily inside `run()` (`sync_manager.rs:248`), not at spawn. A task first polled after BH emits its completion would miss it. That can't happen in practice, because the completion needs a network round-trip.

### F5 (nit, tests): the resume path is pinned only in the caught-up case

`dash-spv/src/sync/filter_headers/sync_manager.rs:55-58`. `test_header_completion_handled_before_its_disconnect_is_kept` starts with filter headers already at the tip. There, `handle_new_headers` only calls `try_complete_sync`. Mutant: replace `events.extend(self.handle_new_headers(tip, requests).await?)` with `events.extend(self.try_complete_sync())`. All three new tests still pass (I checked: 20/20 filter_headers tests pass with this mutant). But in the "behind" case (FH at 39 000, BH complete at 40 000 for the current epoch, completion already consumed) the reconnect then sends no GetCFHeaders. Recovery would then depend on `tick`'s storage-tip check, which won't fire because `progress.block_header_tip_height()` is already 40 000. FH would wait in `WaitForEvents` for good.

Add a test: after the stale disconnect and reconnect, the request channel carries a `GetCFHeaders`. The existing tests do pin the other halves:

- FH count: test 2 fails if `on_disconnect` stops incrementing.
- BH count: `test_sync_complete_carries_the_connection_epoch`.
- `>=`: test 1 kills both `>` and `==`.
- Resume removed: test 1 kills it.

All the tests are deterministic: direct handler calls, no timing.

### F6 (nit): `block_headers_synced_epoch` overwrites instead of taking the max

`filter_headers/sync_manager.rs:161`. That's fine today. There is one emitter, it emits epochs in increasing order, and the broadcast is FIFO per receiver. But `SyncCoordinator`'s event bus outlives each run (`sync_coordinator.rs:141`), so a straggler from a previous run's BH task could in principle deliver an old epoch. `Some(max(old, new))` costs nothing and removes the ordering assumption. Optional.

## Interleavings checked (filter headers, counts agreeing)

D = `PeersUpdated(0)`, R = `PeersUpdated(n>0)`, C(e) = completion with epoch e; c = FH's count.

| FH sees | Outcome |
|---|---|
| C(1), D, R | C(1) at c=0 counts (may emit FHSC on the old connection; harmless, and FHSC was already re-emitted per reconnect). D: c=1, WaitingForConnections. R: `start_sync` resumes, 1>=1, completes. Correct (test 1). |
| D, C(1), R | C(1) in WaitingForConnections, 1>=1: `handle_new_headers`, so either `try_complete` (Synced) or `start_download` (Syncing). R does nothing because the state is not WaitingForConnections. Correct, and the same as the old flag. |
| D, R, C(1) | Ordinary path. Correct. |
| C(0) late, after D | 0>=1 false: does not count. Correct (test 2). |
| D, D (two `PeersUpdated(0)`, e.g. concurrent removals both reading 0) | Both tasks count 2. The BH completion then carries 2. Correct. |
| C(1), C(2), D, R, D, R | synced=2. First R: 2>=1, resume. Second R: 2>=2, resume. Correct. |
| C(1), D, R, D, R, C(2) | First R resumes. Second D: c=2. Second R: 1>=2 false, WaitForEvents. C(2) completes. Correct. |
| D, no completion ever | c=1. Waits for BH. Correct. |

Q3: `start_sync` resumes only when the held epoch is >= c. That means BH completed after handling its c-th disconnect, i.e. in FH's current connection or a later one. So it never completes ahead of block headers for the current connection.

Q4: the public-event change is handled at every match site in the workspace. The FFI callback (`dash-spv-ffi/src/callbacks.rs:342`) and masternodes use `..`, and `dash-spv-bench/src/metrics.rs:125` already had `..`. The FFI ABI is unchanged (the callback still passes only `tip_height`). ARCHITECTURE.md is updated, and the commit carries `!` plus a BREAKING CHANGE trailer. `u64` overflow: not a concern (one increment per total peer loss). But see F1: masternodes match with `..` exactly where the epoch matters.

Q6 (simpler alternative): the root cause is that each task reads two independently ordered streams. Forwarding network events onto the sync-event bus, or giving every manager one merged, ordered inbox, would fix filter headers, masternodes and any future consumer at once. That is a much bigger change. Within the current design, the epoch gate is about as small as it gets. Factor it into one shared helper (F1) instead of a per-manager copy.

## What I ran

- `CARGO_TARGET_DIR=/work/scratch/bl243-r1-sol/target RUSTC_WRAPPER=sccache cargo test -p dash-spv --lib -j 8 -- sync::` on 96e0d772c: **373 passed, 0 failed** (includes the three new tests and `filter_headers::…::test_on_disconnect`).
- `cargo clippy -j 8 -p dash-spv -p dash-spv-ffi -p dash-spv-bench --all-targets -- -D warnings`: **EXIT 0, no warnings**.
- F1 probe (source `/work/scratch/bl243-r1-sol/bl243_probe.rs`), in a reflinked non-git copy of the worktree that has since been deleted, with its target dir: `cargo test -p dash-spv --lib -- bl243_probe --nocapture`: **passes, i.e. the bug reproduces**. It prints `masternodes state after reconnect: WaitForEvents` and no request was sent after 40 ticks plus a `BlockHeadersStored`.
- F5 mutant, same copy: replaced the resume call in `filter_headers/sync_manager.rs:57` with `events.extend(self.try_complete_sync())`, then ran `cargo test -p dash-spv --lib -- filter_headers::`: **20 passed, 0 failed**. The mutant survives.
- Did not rerun the dashd `test_sync_with_peer_disconnection` loop. The owner reports 50/50 solo, and F1 cannot show up there because that suite disables masternodes.
- Code reading: `sync_manager.rs` run loop and default handlers; `sync_coordinator.rs` spawn, tick and progress aggregation; `client/lifecycle.rs` start order; `network/manager.rs` `PeersUpdated` emission; the block-headers, filter-headers, masternodes and chainlock sync managers. ChainLock's `masternode_ready` reset has the same shape, but it ignores events in `WaitingForConnections`, re-arms on every `MasternodeStateUpdated`, and is not part of `is_synced`, so it is not affected.

Nothing was edited, committed or pushed in `/work/scratch/bl243-r1-sol/wt`.
