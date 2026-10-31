# CH1-node N7b-1, Opus high review — `549457767..cae1d6187`

Detached checkout `/work/scratch/ch1-node-n7b1-r1-opus/wt` at `cae1d6187` (removed after the review). Core is `v24.0.0-rc.3` in `~/workspace/dash`, read with `git show` only. I read the design rows D47–D54 (`docs/node-program/designs/CH1-node-notes.md:41-56`), Sol's r1 and r2 reports, and the whole production diff. I did not reopen the manager's three rulings (interim `LocalShutdown` in `parts()`, `-stopatheight` refused until ND1 2c, D54's empty-store deviation).

## Summary

There are no Major findings. The load order, the poll points, the error classes, the abort-then-flag order and the `-stopatheight` predicate all match Core, as far as I traced them. There is one Minor code finding, on an error path that today's callers cannot reach. There are four Minor test gaps, each shown by a surviving mutant. In each case the code is correct but the D-row's claim is not pinned. Two notes are for ND1.

## Parity checks done (no finding)

1. **Load order, D53/D54** (`dashd-chainstate/src/chain/load.rs:112-180`):
   - The first step is Core's `:177` check_interrupt.
   - `load_records` stands for `LoadBlockIndexGuts` (`txdb.cpp:300-344`). It polls before each read, like `:307-309`. Its decode and PoW check use the configured network's `pow`, like `:328-331`. A regtest datadir opened as mainnet therefore fails PoW as LoadingBlockDb, as Core does.
   - `NodeBlockIndex::load_until` stands for the two height passes (`node/blockstorage.cpp:296`, `validation.cpp:5342`). Next comes the blk-file check (`blockstorage.cpp:377-390`, `BLOCK_HAVE_DATA`, `std::set` order, so ascending), then one poll for pass 2's first entry, then the two datadir checks (`chainstate.cpp:188-198`).
   - Core runs the blk-file check between its passes; V runs it after both. I enumerated the cases (a shutdown in pass 1, during the check, or in pass 2; a check that fails with or without shutdown). Every one gives the same `ChainstateLoadStatus`, because a non-empty index always reaches pass 2's first poll.
   - The NonContiguous refusal runs before any poll in Rust, but in Core it is interleaved with the polls. With shutdown requested, `failed()` re-reads the flag, so the class stays Interrupted, as `chainstate.cpp:183-184` gives.
   - The extra poll before the `next()` that finds the end of the scan only changes the outcome for an empty store: a non-empty store reaches pass 1's first poll in Core too. So D54's documented deviation is exactly as narrow as it says.
2. **Error classes:**
   - An I/O `Read` maps to `Store` ("Error opening block database"), matching Core's exception caught at `init.cpp:2035-2037`.
   - A corrupt row or an undecodable record maps to `LoadingBlockDb` ("failed to read value", `txdb.cpp:334-336`).
   - `Interrupted` is mapped without a log line, like Core's silent `return false`.
   - `Incompatible` is not re-classified under shutdown, which is right: Core returns FAILURE_INCOMPATIBLE_DB unconditionally.
3. **Network-root rule (focus 4).** I found no leak:
   - `HeaderChain.root` is set only by `publish_first`, in Node mode. `genesis()` is read only by `build_snapshot`'s pre-tip placeholder (`dash-chain/src/chain.rs:2045-2048`). `new_anchored` refuses Node mode (`:930-932`). SPV always has a tip.
   - For `load` (network_root=true), the root is the network's genesis, so the old lookup and the new field agree.
   - Only `open_block_index` calls `load_records` (`load.rs:143`). Both of its arms (`Stored`, `Loaded`) reach the network check, and nothing else reads the chain before that check except the read-only passes and file check. The chain uses `NoStore`, so a refused foreign index writes nothing, its FAILED_CHILD dirt included.
   - Once the network check passes, the single root must be the network's genesis: genesis has a null prev, so if it is indexed it is a root.
4. **Abort order (focus 2):**
   - `stop_serving` (`actor.rs:873-892`) trips the store latch, then calls the sink. Then `StopTail` sets V's latch and the node's `start_shutdown()`, and reaps. That is Core's `AbortNode` order (`shutdown.cpp:27-37`, `StartShutdown()` last).
   - The listener-panic path (`signals.rs:257-266`, with `fifo.rs:128-141`) calls the sink, then `ShutdownOnDrop` sets the flags, then `CloseOnDrop` closes the queue. So a barrier waiter woken by the close always sees the flag set.
   - Both paths set the flag even if the sink panics.
   - Races:
     - A listener panic and V's own `AbortNode` can both call the sink concurrently. That was already true before this change, and `start_shutdown` is idempotent.
     - An attach after the node's flag is set is harmless: ABC polls the flag.
     - An attach at a release point inside a class C operation is refused, because `composite` closes `coins_attachable` first.
5. **`-stopatheight` (focus 3):**
   - `core_int` truncates, which is C++'s int64→int on GCC/Clang, as `validation.cpp:3752` stores it.
   - The predicate `n != 0 && height >= n` is `:3830`'s, placed after the step and before the `:3841` poll, as in Core.
   - The value is read once at spawn, where Core calls `GetIntArg` per ABC; args are immutable, so the two are equivalent.
   - A value of 2^31 narrows to `INT_MIN` and stops at genesis, as dashd would.

## Major

None.

## Minor

**m-1 (code): if stopping the spawn's side fails, the incoming flush side is dropped without tripping its latch.** `dashd-chainstate/src/chain/actor.rs:1191-1210`.

`attach_coins` moves `flush` out of `AttachCoins`. If `self.with_flush(|f, host| { f.service(host)?; f.stop(host) })?` (`:1202-1205`) returns `Fatal`, the function returns before `self.flush = flush`. The new side drops unaborted, while the misuse path (`:1196-1200`) aborts it on purpose: "Its latch trips before it drops, so a W of its own writes nothing more" (D51, validator L-3). After that `Err`, `stop_serving` aborts only the old side.

ND1's order spawns on `NoFlush`, whose `service`/`stop` cannot fail, so this path is unreachable today. It becomes reachable for any caller that spawns with a real W and then attaches, which the code permits.

Fix: on the `Err` from the old side, call `flush.abort(&mut FlushHost::new(..))` before returning, as the misuse branch does. Add a test with a `ModelW` spawn side whose `stop` fails.

**m-2 (test gap, mutant M1 survived): V's `AbortNode` path does not pin "sink, then flag".** `actor.rs:873-892`.

Mutant M1 inserts `self.node_shutdown.start_shutdown()` before the sink in `stop_serving`. It survives all of `chain::tests::{actor,failure,flush}`.

The listener-panic path pins Core's order through `Probe` (`tests/signals.rs:634`, flag `false` at the sink). V's own path only checks that the flag is set afterwards (`tests/actor.rs:357-365`). D52 states this order for both paths (`shutdown.cpp:35-36`): ND1's main wait must not start its shutdown before the abort is marked.

Fix: in `an_abort_node_starts_the_nodes_shutdown_through_the_sink`, give the rig a sink that records `shutdown_requested()` at call time, and assert `false`.

**m-3 (test gap, mutant M12 survived): the D54 claim "a second root is still refused" has no `load_records` test.** `dash-chain/src/chain.rs:1103-1110`.

Mutant M12 changes `root.is_some() || (network_root && !genesis)` to `(network_root && root.is_some()) || (network_root && !genesis)`. It survives every `load` test in dash-chain. The existing "a second root" case (`chain.rs:4550-4554`) uses `load`, and its root sits at height 7, so `!genesis` or `HeightMismatch` refuses it anyway.

Under `load_records`, the unconditional `root.is_some()` is now the only thing that keeps two height-0 roots out of a node index. Example: testnet params, with both the testnet and the mainnet genesis records, which share `nBits` and both pass testnet PoW. With the mutant, that index passes the network check, and V runs on a two-rooted index, with the placeholder root possibly the foreign one.

(Core itself would load two roots. SB4's D21 refusal is the pre-existing deviation, and this finding does not reopen it.)

Fix: a `load_records` test with two valid height-0 roots, expecting `GenesisMismatch`. Optionally add the same case through `open_block_index`, expecting `LoadingBlockDb`.

**m-4 (test gap, mutant M14 survived): a refused stored index with shutdown requested is not tested as Interrupted.** `load.rs:157-161`.

Mutant M14 replaces `failed()` in the generic `LoadBlockIndexGuts` arm with `LoadingBlockDb`. It survives `chain::tests::blocks`. Today `failed()`'s shutdown branch is exercised only by the blk-file check (`a_failed_load_with_shutdown_requested_is_interrupted`).

Core: `LoadBlockIndex` false plus `check_interrupt` gives INTERRUPTED (`chainstate.cpp:183-184`). Example: a PoW failure on a later record, read after a signal that arrived during the decode.

Fix: a corrupt or undecodable record whose iterator requests shutdown when it is read, expecting `Interrupted`.

**m-5 (test gap / nit, mutant M10 survived): the empty arm's "Checking all blk files are present..." line is not asserted.** `load.rs:184-193`.

D53 (validator L-6) says the empty arm logs it, but deleting the log survives `chain::tests::blocks`. The only assertion of the line, `blocks.rs:625`, is on the loaded arm.

Fix: assert it after `stored_records_open_as_the_loaded_index`'s empty-records open (or after an `Empty` open), with `install_accept_log()`.

## Notes for ND1 (no change needed in N7b-1)

- **n-1: shutdown on a non-fatal failure.** Core's init treats any FAILURE under `ShutdownRequested()` as a quiet exit: the retry loop's `!ShutdownRequested()` guard skips the reindex question (`init.cpp:2119`), and init then logs "Shutdown requested. Exiting." (`:2143`). This applies to `LoadFailure::Store` (an exception) and `Failure` too, not only `Interrupted`. ND1's slice 2a mapping should read its flag for those two before it reports or asks. The `LoadFailure` docs (`load.rs:28-43`) describe this only for `Interrupted`. One sentence on `Store`/`Failure` would carry it to ND1.
- **n-2:** M16 (dropping `link_shutdown`) is killed only by the panicking-sink tests. That is correct: with a sink that returns, ND1's `NodeAbort` sets the flag anyway. I mention it only so nobody removes those two tests as redundant.

## Tests and mutants run

Environment:
- `CARGO_TARGET_DIR=/work/scratch/ch1-node-n7b1-r1-opus/target` (reflink-seeded from the implementer's target, read only; default sccache wrapper);
- `TMPDIR=/work/scratch/ch1-node-n7b1-r1-opus/tmp`;
- `nice -n 10`, at load around 30–40.

Baseline, all green:
- `cargo test -p dash-chain --lib`: 392 passed, 1 ignored.
- `cargo test -p dashd-chainstate --lib`: 378 passed.
- `cargo test -p dashd-node`: 95 passed and 1 ignored; asserted-log 11 passed.

Mutants (`mutate.py`, each restored after its run; worktree clean afterwards), chosen to avoid duplicating the implementer's (`/work/scratch/ch1-node/mut-n7b1-r1.log`, `mut-n7b-*.py`) and Sol's (`mutations.log`):

| # | Mutant | Result |
|---|---|---|
| M1 | `stop_serving` sets the node's flag before the sink | **SURVIVED** (m-2) |
| M2 | `StopTail` drops `node.start_shutdown()` | killed: `actor::a_panicking_abort_sink_still_disconnects_queued_replies` |
| M4 | V's `shutdown_requested()` ignores its own latch | killed: `failure::a_shutdown_during_the_walk_runs_the_final_scope_on_the_last_disconnected_block` |
| M5 | `-stopatheight` test `n > 0` instead of `n != 0` | killed: `abc::stop_at_height_minus_one_stops_at_genesis` |
| M6 | saturating narrowing instead of truncation | killed: `abc::stop_at_height_requests_shutdown` |
| M10 | empty arm does not log the blk-file line | **SURVIVED** (m-5) |
| M12 | `load_records` accepts a second root | **SURVIVED** (m-3) |
| M14 | Guts refusal with shutdown is `LoadingBlockDb` | **SURVIVED** (m-4) |
| M16 | V's spawn does not link the node's flag to the signals thread | killed: both `signals::a_listener_panic_with_a_panicking_sink_*` |
| M17 | every `Read` error is `Store` (no `is_corrupt` split) | killed: `blocks::a_stored_record_read_error_fails_the_open` |

## Verdict

None of the five Minor findings changes behaviour on any path that ND1 or the tests reach today. m-1 is a latent invariant hole. m-2 to m-5 are pins for claims the D-rows make. All can land as follow-ups in N7b-2 or with this branch's merge prep.

APPROVE-WITH-NITS
