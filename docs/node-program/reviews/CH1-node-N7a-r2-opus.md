# CH1-node N7a, round 2 (Opus high, narrow): `c89bb1d01..421c18cda`, plus the merge abc82fb09

Scope:
- 3432f619e: Sol r1 R1-m-1 and R1-m-2.
- c8c2aa594: Opus r1 M-1, M-2, L-1, L-2 and N-1 to N-5.
- 421c18cda: M-1's failing-job test.
- abc82fb09, the train-9 follow-merge: sanity check only.

Line numbers are at abc82fb09. Files under `dashd-chainstate/src/chain/` are written without that prefix.

## Item statuses

| Item | Status | Evidence |
|---|---|---|
| **M-1** stop order | **CLOSED** | `FlushSite::stop` (`flush.rs:58-68`, default no-op) runs after the last `service` on the orderly path (`actor.rs:719-727`). A `Fatal` from it takes `stop_serving` (`:728-729`). D45 states both layers: ND1's ALWAYS flushes through `Flush` first, and `stop` is the backstop. It also states that `ThreadDriver::drop`'s doc now holds. Tests: `tests/flush.rs:907 a_prune_flush_w_sends_after_shutdown_is_taken_is_applied` (W sends **after** `Shutdown` is taken, the prune applies, no abort, `have_pruned`) and `:944 a_job_failing_while_vs_stop_waits_takes_the_fatal_path`. The mutant that removes `f.stop(host)` fails both. See L-3 for how a failure in that wait is reported. |
| **M-2** devnet datadir check | **CLOSED** | `load.rs:68-93` `open_block_index` ports `node/chainstate.cpp:188-193` (wrong network) and `:195-198` (wrong devnet), in Core's order, before `NodeBlockIndex::load`. The messages are Core's exact strings (`load.rs:33-40`). Tests in `tests/blocks.rs`: `:517 a_devnet_index_opened_as_another_devnet_is_refused`, `:526 an_index_without_its_devnet_genesis_block_is_refused`, `:538 another_networks_index_is_refused`, and the restart in `:370 a_devnet_starts_on_its_devnet_genesis_block` (opens and does not repeat the step). The mutant that disables the devnet check fails the first two. "All devnets share block 0" holds: Core's devnet and regtest call `CreateGenesisBlock(1417713337, 1096447, 0x207fffff, 1, 50 * COIN)` with the same arguments. See N-8 for the -prune check that sits in the cited range. |
| **L-1** in-hold clears vs view readers | **CLOSED** | `FlushHost::prune_check_height` (`flush.rs:229-237`) is `min(working, published)` and returns `None` if either side has no tip. `published` is SB5's last published snapshot (`dash-chain/src/chain.rs:2314`), which inside a hold is the pre-hold tip. Between `hold.release()` and `publish_view` (`abc.rs:209-218`, `:694-703`) no `FlushSite` call runs, so the bound never exceeds what `ChainView` readers see. `FlushCx::tip` and the locators stay on the working tip (D46). Test: `tests/flush.rs:574 a_selection_only_a_forward_holds_working_tip_allows_is_declined`, which declines at h292 working / h291 published, then clears at idle. The working-only mutant fails it. |
| **L-2** latch on Fatal/panic stop | **CLOSED** | `stop_serving` calls `self.flush.abort(...)` before `self.abort.abort_node` (`actor.rs:822-834`). This covers the panic path too (`:712-713`). Tests: `tests/flush.rs:980 an_abort_trips_the_store_latch_before_the_sink` and `:1003 a_panic_trips_the_store_latch_before_the_sink`. Both assert `latched_before_sink == Some(true)` and that no queued job runs. The mutant that removes the call fails both. |
| **N-1** `have_pruned` on an empty set | **CLOSED** | `flush.rs:277` `if !files.is_empty()`. Asserted in `:574`'s tail (`prune(&[], 4)` leaves `have_pruned` false). |
| **N-2** two infallible forms | **CLOSED** | Both use `expect("V's chain is the node's")` (`flush.rs:269`, `:274`). |
| **N-3** duplication | **CLOSED** | One `take_ring` helper (`flush.rs:113-122`) is used at the select (`actor.rs:114`), `VWait` (`:679`) and the release point (`:1035`). `FlushHost::lend` (`flush.rs:252-264`) is shared by `V::with_flush` and `InHold::with_flush`. |
| **N-4** CPU-idle margin | **CLOSED** | `tests/flush.rs:695` now measures 1 s against a bar of 5 ticks (about 16 at peak load). It passed 3/3 at load 139-145. |
| **N-5** devnet rejection is an init error | **CLOSED** | `chain::start` (`actor.rs:535-547`) waits for V's first act. A rejection sets the shutdown flag and returns with no `AbortNode` (`:796-799`), and becomes `StartError::Load(LoadFailure::Failure { cause: None })`. A write error is `AcceptBlock`'s `AbortNode`, then the same init error (`:784-792`). Tests: `tests/blocks.rs:455 a_rejected_devnet_genesis_block_is_an_init_error` (no AbortNode, "Error initializing block database") and `:480 a_devnet_genesis_block_write_error_aborts_then_fails_init`. Mutants fail both: one restores the old `Some(Fatal)` path, the other reports `Ok` on the write error. See N-9 for ND1 not calling it yet. |
| **Sol R1-m-1** no-tip decline | **CLOSED** (V side) / adapter item | `FlushHost::tip()` and `prune_check_height()` are `None` before genesis connects. D47 (1) requires the adapter to decline, with no unwrap and no fabricated tip. Tests: `tests/flush.rs:848 a_prune_flush_before_genesis_is_connected_is_declined` and `:869 a_prune_flush_sent_at_the_devnet_step_is_declined`. The fabricated-`u32::MAX` mutant fails both. The fabricated-height-0 mutant survives; see N-7. |
| **Sol R1-m-2** event-only service, `hand_to` on every return | **CLOSED** (contract) | D47 (2) and (3). This is adapter-side, so no V code is involved. |

### The b3 adapter contract (items 1–9) in D47

All nine are stated. D47 numbers them differently (see N-6):

| b3 item | Where it is stated |
|---|---|
| 1 bells | D47 (4), D45 |
| 2 `retry_pending` | D47 (5), D45 |
| 3 production `impl FlushSite` | D44 ("CH2b's"), D47's preamble |
| 4 `FlushCx` per call | D47 (1) |
| 5 event-only service and `hand_to` | D47 (2), (3) |
| 6 no-tip | D47 (1) |
| 7 stop | D47 (6) |
| 8 abort trips only | D47 (7) |
| 9 min(working, published) with cx.tip = working | D47 (8), D46 |

## New findings

### L-3 (minor): a flush `Fatal` is reported twice once CH2b is wired. D47 (7)'s "W must not also report it" contradicts CH2b b2

- **Where:**
  - `actor.rs:822-834`: `stop_serving` reports every `Fatal` through V's sink, including one returned by `at_site`, `after_step`, `service` or `stop`.
  - D45 ("so the node still sees one `AbortNode`") and D47 (7) ("a failure `wait_release` returns to V is reported by V's sink alone, so W must not also report it").
  - The model's `fail_job` (`tests/flush.rs`, used by `:944`) trips the latch without reporting.
- **CH2b b2 does the opposite:**
  - W reports a failing job on its own thread (`flush/driver.rs:107`), then sends `DriverEvent::Failed`.
  - `FlushControl` documents its `Fatal` as "the failure has been reported" (`flush/control.rs:161`, `:213`).
  - V-side `abort()` reports too (`control.rs:538-546`).
  - W has to report itself, because a job can fail while V isn't waiting.
- **Failure:** with the real adapter, a B1 write failure or W's crash reaches V at its next site or bell service, or in `stop`'s wait. CH2b has already called `abort_node`, and V's `stop_serving` calls it again. Under ND1's `NodeAbort` (`dashd-node/src/abort.rs:39-51`, no dedupe) that gives two `*** …` log lines, two misc warnings and two `InitError`s.
- **Core:** Core reports once, at the failing `FlushStateToDisk` (`validation.cpp:3066`, `:3093`, `:3117` `AbortNode(state, …)`).
- **Reachability:** it can't happen in N7a, which runs `NoFlush` or the model W.
- **Fix:** make one rule for every `FlushSite` return, not just `stop`. The `Fatal` a `FlushSite` returns has already been reported (CH2b's existing contract), so V's `stop_serving` still trips the latch, sets the flag and disconnects replies, but skips its sink for a flush-origin `Fatal`. For example, give the seam a reported/unreported error, or have the adapter return a marker. The alternative is that CH2b reports nothing W-side and relies on the bell, but that changes b2 and delays the report to V's next wait.
- **Tests and docs:** amend D45 and D47 (7) to match. Make the model's `fail_job` report as CH2b does, and keep `:944`'s assertion of exactly one message. Relay this as an addition to b3 item 8.

### N-6 (nit): D47's numbering doesn't match the agreed b3 list

D47 numbers its items (1)–(8), and its "relays (1)–(3) and (6)–(8)" refers to that numbering. The CH2b b3 contract the manager relayed numbers them 1–9 (the table above), and its item 3 is in D44, not D47. Add the mapping line to D47, or renumber, so the two lists can't drift apart.

### N-7 (nit): the no-tip tests don't catch a fabricated height-0 tip

`prune_check_height` returning `Some(0)` with no tip passes `:848` and `:869`; that mutant survives. Both tests use the automatic rule, whose gate (`tip > after`) declines at 0 anyway. A `PruneRule::Manual` request isn't covered. Its gate is always true, and `last_prunable_manual(0, 0) = min(0, 0.wrapping_sub(288)) = 0` (`dashd-store/src/flatfile/blocks.rs:86-88`). So a fabricated genesis tip allows a manual selection with `highest == 0`, the case Sol r1 named. Core returns early at `chain_tip_height < 0` for both selectors (`node/blockstorage.cpp:170`, `:191`).

Fix: record `prune_check_height()` in the model next to `tip()` and assert `None`, or add a `Manual(0)` / `highest 0` request to `:848`.

### N-8 (nit): `open_block_index` cites `node/chainstate.cpp:189-212`, but Core's -prune state check in that range isn't ported or owned

`chainstate.cpp:200-204` (`m_have_pruned && !options.prune` → FAILURE "You need to rebuild the database using -reindex to go back to unpruned mode…") sits between the devnet check and `LoadGenesisBlock`. It isn't in `load.rs:59-93`, and no D-row or BL owns it. A grep of the notes, STATUS and PLAN found nothing.

Separately, the network check is at `:188-193`, not `:189-193` (`load.rs:31`).

Fix: narrow the cite to `:188-198, 210-212`, and give the -prune check an owner. CH8 loads `m_have_pruned`, so the check fits next to that load, or in N7b with BL-127.

### N-9 (nit): ND1 at abc82fb09 doesn't use `open_block_index` / `start` yet, and the merge message misdescribes why its step is harmless

- **D48's note says ND1 calls them.** At the merge head, ND1 still builds with `genesis_parts` → `load_genesis_block` (`dashd-node/src/chain.rs:58-76`), starts with `chain::spawn` (`:120`), and runs its own `Chain::load_devnet_genesis` (`:140-154`, `node.rs:232-240`). D48's "ND1 calls `open_block_index` and then `start`" is therefore a future state.
- **The merge message's reason is wrong.** abc82fb09's message says ND1's step "returns early once V's start step has connected the devnet genesis block (V has a tip)". V's step only accepts the block. V has no tip until init's ABC: `:869` shows the step's NONE site at tip `None`. So ND1 resubmits the block. PNB accepts it as already-have and connects genesis and the devnet block before step 11. That was ND1's behaviour before the merge too, it is harmless, and ND1's `a_devnet_starts_on_its_genesis_block` passes.
- **Under `spawn` a rejected step is silent.** V sets the shutdown flag and stops with no `AbortNode` (`actor.rs:796-799`). ND1 still ends with Core's `INIT_BLOCK_DB_FAILED` through "the chain stopped" as the cause, so there is no regression.
- **Fix:** name the slice that switches ND1 to `open_block_index` + `start` (and deletes `Chain::load_devnet_genesis`), and word D48's sentence as that future step. Consider making `spawn` `#[cfg(test)]` or documenting that it gives no init error for a rejected devnet step.

## BL-127 (not a finding)

Nothing in this range makes BL-127 harder; it makes it a little easier.
- Core's check (`node/blockstorage.cpp:377-390`, inside `LoadBlockIndexDB`) fails `LoadBlockIndex`, which gives FAILURE "Error loading block database" (`chainstate.cpp:183-186`). That is **before** the two datadir checks (`:188`, `:195`).
- `open_block_index`'s `Loaded` arm already takes `files: &mut dyn BlockFiles`, which it doesn't use yet. That is the natural home: the check goes first in that arm, before `WRONG_NETWORK_DATADIR`.
- One constraint for N7b: `LoadFailure::Failure`'s `Display` is fixed to `INIT_BLOCK_DB_FAILED`, so Core's "Error loading block database" needs its own variant or a message field. ND1 already maps FAILURE to its recoverable init error.

## Core cites checked (v24.0.0-rc.3)

- `node/chainstate.cpp`:
  - `:183-186` "Error loading block database";
  - `:188-193` wrong network (the code cites `:189-193`);
  - `:195-198` wrong devnet;
  - `:200-204` -prune state (not ported, N-8);
  - `:210-211` `LoadGenesisBlock` false → "Error initializing block database". All the strings match `load.rs:24-40` byte for byte.
- `validation.cpp:5390-5424` `LoadGenesisBlock`:
  - `:5400` early return on genesis in `m_block_index`;
  - `:5408-5416` the devnet step: `assert(CheckBlock)`, then `AcceptBlock(..., fRequested=true, ...)`, false → return false;
  - `:5418-5420` catch `runtime_error` → false (V's `load_genesis_block` write failure → `Failure { cause: Some }`).
- `AcceptBlock`'s write `catch` → `AbortNode("System error: …")`. That matches `:480`.
- `init.cpp:2115-2123`: INCOMPATIBLE_DB is a plain `InitError`, while FAILURE first gets the "rebuild the block database" question (`ThreadSafeQuestion`; under noui that is the log line and exit). ND1's `Recoverable` for FAILURE fits.
- `node/blockstorage.cpp:170`, `:191`: both selectors return early with no tip (R1-m-1).
- `validation.cpp:3066/3093/3117`: `FlushStateToDisk`'s single `AbortNode` (L-3).

## Checks run

- **Worktree:** a detached worktree at 421c18cda under `/work/scratch/ch1-node-n7a-r2-opus/src`, with a private target reflinked from n7-wt's. `nice -n 10`, `CARGO_BUILD_JOBS=8`, `TMPDIR=/dev/shm/ch1n7a-r2-tmp`.
- **`cargo test -p dashd-chainstate --all-features chain::` three times at 421c18cda:** 155/155 passed each time (9.6 s, 8.5 s, 6.8 s) at load 139–145. Logs: `logs/run-{1,2,3}.log`.
- **Mutants** (`mut/mutants.py`: each one reverts a fix, runs its targeted tests, then restores the file and touches it; the tree was confirmed clean afterwards). Log: `logs/mutants.txt`.

  | Mutant | Result |
  |---|---|
  | M-1 stop order (drop `f.stop`) | killed: `:907` and `:944` time out at `stop_began` |
  | no-tip decline, fabricated `u32::MAX` | killed: `:848`, `:869` |
  | no-tip decline, fabricated `0` | **survives** (N-7) |
  | min(working, published) → working only | killed: `:574` |
  | N-5 mapping: rejection → `Some(Fatal)` (old) | killed: `blocks.rs:455` (AbortNode recorded) |
  | N-5 mapping: write error → `Ok` | killed: `blocks.rs:480` |
  | M-2: devnet check disabled | killed: `blocks.rs:517`, `:526` |
  | L-2: drop `flush.abort` | killed: `:980`, `:1003` (`:944` passes, as expected: its model trips the latch itself) |

- **Merge abc82fb09:** `git show --cc abc82fb09` prints no hunks. Parents are 421c18cda and f4046b5f5, so it is conflict-free. At abc82fb09:
  - `cargo test -p dashd-chainstate --all-features`: EXIT 0. The lib ran 371/371, then 35 (4 ignored), 46 (1 ignored) and the single-test binaries all ok.
  - `cargo test -p dashd-node`: EXIT 0, 94 (1 ignored) and 11 passed.
- **Reading:** I read the c89bb1d01..421c18cda diff in full, and CH2b's `control.rs` / `driver.rs` reporting and `prune.rs` rule only where V's seam meets them. Nothing under `~/private` was read.

**Verdict: APPROVE-WITH-NITS.**
- Every r1 item (M-1, M-2, L-1, L-2, N-1 to N-5) and Sol's R1-m-1/R1-m-2 is CLOSED with tests, and the fixes are mutant-checked.
- L-3 is a contract correction for b3. It isn't reachable in N7a, but should be settled in D45/D47 and relayed to CH2b before the adapter is wired.
- N-6 to N-9 are doc and test-strength nits.
