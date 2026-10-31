# BL-243 r2 review: fix commit 908afe112 (range 908afe112^..908afe112)

Reviewer: Opus (high). Scope: only the round-1 fix commit. The follow-merge 9bd742935 is not in scope; `git diff 908afe112 9bd742935 -- dash-spv/src` is empty, so the line numbers below hold for both.

## Verdict: ACCEPT WITH NITS

Every round-1 finding is closed. I found no order of {completion, disconnect, reconnect, start_sync, PeersUpdated} that strands filter headers or masternodes while the disconnect counts agree. The fix also closes the last known way the counts could disagree (F2). The two masternode tests and the behind-case test each fail at the intended assertion when the matching src hunk is reverted or mutated. The two nits are about test coverage and documentation, not behaviour.

## Round-1 findings: status

| r1 | Status | Evidence |
|----|--------|----------|
| F1 major: MN strands in WaitForEvents | **Closed** | `masternodes/sync_manager.rs:81` counts disconnects, `:84-98` resumes in `start_sync` via `start_masternode_sync` (the same path the event takes), and `:356` ignores stale completions. With the MN src reverted to the r1 head, `…starts_the_sync` fails with `WaitForEvents != Syncing` (the exact r1 probe outcome) and `…is_ignored` fails with `Syncing != WaitingForConnections` (the no-peers QRInfo). |
| F1 reverse case: stale completion sends QRInfo with no peers | **Closed** | `:356` returns before `should_restart`. Test `…from_before_a_disconnect_is_ignored`. |
| F2 minor: fallible metadata write before the epoch bump | **Closed in code**, untested (N1) | `block_headers/sync_manager.rs:256-265` logs the error, and `stop_sync` always runs. No other `?` sits before `stop_sync` in the BH `PeersUpdated` arm, and the default handler (FH, MN) has none. |
| F3 nit: lagged receiver exits silently | **Closed** | `sync_manager.rs:307-309,337-339,374-380` return `Err`, `Closed` still breaks. See Q3. |
| F4 nit: spawn order undocumented | **Closed** | `sync_coordinator.rs:228-234` comment. The direction it argues is correct: a late subscriber can only under-count, which means accepting a stale completion, not hanging. |
| F5 nit: resume pinned only in the caught-up case | **Closed** | `filter_headers/manager.rs:610` `…resumes_a_download`. The r1 F5 mutant (resume → `try_complete_sync`) now fails at `:644` (`WaitForEvents != Syncing`). |
| F6 nit: overwrite instead of max | **Closed** | `header_completion.rs:32`. The "gate overwrite" mutant fails `test_gate_never_moves_back`. |

## Q1: remaining orders that strand FH or MN

None found. D = `PeersUpdated(0)`, R = `PeersUpdated(n>0)`, C(e) = `BlockHeaderSyncComplete{epoch e}`, c = the manager's count. These are the masternodes interleavings I traced (the FH table in r1 still holds, since the FH logic only moved into the gate):

| MN sees | Outcome |
|---|---|
| C(1) in Synced/caught-up, D, R | record(1) true, no restart. D: c=1, WaitingForConnections. R: `start_sync` sees synced, so full QRInfo and Syncing. **The r1 F1 order, now correct** (test 1). |
| C(1) in Synced/behind, D, R | C(1) sends an incremental or QRInfo on the "old" view. D: `requeue_in_flight`. R: resume, `clear_pending`, full QRInfo. Correct. It ends in the same state as the D, R, C order. |
| C(1) in Synced with pending work, D, R | Early `return Ok(vec![])` after `record`. R resumes. Correct. |
| D, C(1), R | C(1) in WaitingForConnections: 1>=1, so start (as before). R does nothing (not WaitingForConnections). Correct; the network really has reconnected by then, because BH only emits C(1) after its own R. |
| D, R, C(1) | The normal path. Correct. |
| D, C(0) late, R, C(1) | C(0) ignored (no QRInfo without peers). R: synced is false, so WaitForEvents. C(1) starts. Correct (test 2). |
| C(1), D, R, D, R, C(2) | First R resumes. Second D: c=2. Second R: 1>=2 false, so WaitForEvents. C(2) starts. Correct. |
| D, D (double zero-count update) | MN, BH and FH all count 2. BH emits C(2). Correct. |
| First start: C(0) before R(1) | WaitingForConnections, 0>=0, so start. R does nothing. Correct (unchanged behaviour). |

Notes that are not findings:
- `start_sync` resumes into a full QRInfo even when MN was Synced and caught up before the drop. That matches what the in-order path (D, R, C) already does from WaitForEvents, so there's no new cost.
- `send_qrinfo_for_tip`'s early `Ok` returns (no tip, or genesis) and its `?` on a closed request channel leave MN in WaitForEvents with the completion already consumed. That is the same as before on the event path, and the only error is channel-closed, i.e. shutdown. It isn't a regression.

## Q2: the gate's logic

It is correct. `synced()` is `max_epoch_seen >= own_count`. `own_count` only grows, by one per D. BH stamps each completion with the number of Ds it has handled, all managers read the same network stream from spawn (`client/lifecycle.rs` starts the network after `coordinator.start`), and the managers are built fresh together on every start. A *fresh* completion (emitted after BH handled the c-th D) therefore always has `epoch >= c`, so the gate can't drop it. It can only wrongly drop one if a manager has counted **more** Ds than BH. There were two ways for that to happen: F2 (fixed) and a lagged BH network receiver (now fatal, so it can't hang silently). A manager subscribing late can only under-count, which is the safe direction. `record`'s `>=` and the `max` are both pinned by mutants (below).

## Q3: the lagged-receiver change

It is safe and consistent with existing policy. `client/event_handler.rs:82-86` already treats a lagged monitor receiver as a client failure (`on_failure` → `ChannelFailure` → `stop_failed`). The manager loop now does the same through `SyncCoordinator::tick` → `Err`. The behaviour change is real: a lag in *any* manager (even mempool or instantsend, whose lost events might not have mattered) now stops the whole client, where before only that task quietly died. The old outcome was also broken, though: a dead task with SyncComplete never firing, or with the mempool silently gone. There is no "Core-like continue" here, since Core has no lossy bus. Continuing after a lag would let the counts drift and bring back the BL-243 hang, and the error makes that impossible to miss. One plausible trigger exists: the large-wallet rescan, where 394K `BlockProcessed` events are produced while one manager's handler is slow. That scenario was already failing to complete; now it fails loudly. The commit message states the change. See N2 for the small residual issues.

## Q4: do the new tests fail on the r1 head?

They do. All runs used `CARGO_TARGET_DIR=/work/scratch/bl243-r2-opus/target`. Each mutation was applied to the detached worktree and restored with `git checkout`.

| Mutation | Result |
|---|---|
| M-R1: masternodes `manager.rs` + non-test `sync_manager.rs` at 908afe112^, new tests kept | **Both MN tests FAIL**: `starts_the_sync` gets `WaitForEvents != Syncing`, `is_ignored` gets `Syncing != WaitingForConnections` |
| M1: MN `start_sync` resume disabled | `…starts_the_sync` FAILS |
| M2: MN stale-ignore branch disabled | `…is_ignored` FAILS |
| M3: MN `on_disconnect` no longer counts | `…is_ignored` FAILS |
| M4: r1 F5 mutant (FH resume → `try_complete_sync`) | `…resumes_a_download` FAILS at `:644` (`WaitForEvents != Syncing`) |
| M5: gate overwrites instead of max | `test_gate_never_moves_back` FAILS |
| M6: `synced()` `>=` → `>` | 10 tests FAIL |
| M7: `record()` `>=` → `>` | gate test + MN `…is_ignored` FAIL |
| M8: BH metadata write `?` restored (F2 undone) | **survives**: 379/379 pass (N1) |

The behind-case test passes on the r1 head by design, because r1 already had the FH resume. It is a pin, and M4 shows it now pins that resume.

## Q5: the owner's masternodes-on loop argument

I accept it as supporting evidence only. The proof is the deterministic handler-level tests, which drive the exact r1 F1 interleaving and fail on the r1 src. "22 runs, none ended in WaitForEvents" doesn't show the race was actually hit in any of those runs. It does show the new resume path doesn't add a new strand in an end-to-end run, which is all that is needed here. On the claim that QRInfo goes unanswered: on the dashd_sync chain (no LLMQs) MN runs out its 10+30+60 s ladder and then `complete_pipeline`. So judging by end state rather than SyncComplete is reasonable. Optional: `tests/dashd_masternode` has a real quorum network but no disconnect test. A one-off disconnect loop there would be the only end-to-end check that could show SyncComplete with masternodes on. That's not required for this change.

## Findings

### N1 (nit): the F2 fix has no test
`dash-spv/src/sync/block_headers/sync_manager.rs:256-265`. Mutant M8, which puts back `store_last_target_height(..).await?`, passes all 379 sync tests. Scenario this would guard against: `PeersUpdated{connected_count: 0, best_height: Some(h)}` with a failing metadata write. BH would skip `stop_sync`, its epoch would stay one behind FH/MN for good, and every later completion would be ignored (the permanent BL-243 hang). Suggest a test with a failing `MetadataStorage` stub (BH is generic over `M`) that asserts `WaitingForConnections` and an incremented `connection_epoch` after that event. Cheap, and it protects the one invariant the gate rests on.

### N2 (nit): a lag during shutdown and the lag error path are untested
`dash-spv/src/sync/sync_manager.rs:307-309,337-339`. Unlike `client/event_handler.rs:81`, the arms don't check `shutdown.is_cancelled()`. `select!` picks at random among ready branches, so a lag that comes up during shutdown returns `Err`, which `SyncCoordinator::shutdown` (`sync_coordinator.rs:310`) logs as a spurious warn. It's harmless. A one-line guard would match the monitor policy. A unit test for `lagged` (capacity-1 bus, two sends, assert `run` returns `Err`) would pin the new behaviour. Optional.

## What I ran
- `cargo test -p dash-spv --lib -- sync::` at 9bd742935: **379 passed, 0 failed**.
- `cargo fmt --check -p dash-spv`: clean. `cargo clippy -p dash-spv --all-targets -- -D warnings`: clean.
- Mutants M-R1 and M1–M8 above.
- Read: the fix diff; the MN, FH and BH sync managers; the `SyncManager` default handlers and run loop; coordinator start, tick and shutdown; client lifecycle and sync loop; the event_handler lag policy; the filters manager (it doesn't gate on a flag cleared at disconnect, so it isn't affected).
- Commit 908afe112 has a good signature (`%G?` = G).
- Nothing was edited, committed or pushed on the branch. The worktree was detached and has been removed.
