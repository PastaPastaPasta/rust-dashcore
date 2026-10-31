# CH1-node N7a, round 1 (Opus high): c89bb1d01

Scope: `7d1e8b257`, `a5430b1bb`, `4912cbd78`, `c89bb1d01` and `git diff 86c83da14 c89bb1d01`. CH2b b2 (`023b0b47b`) is taken as given; it is read only where V's seam meets it. Sol r1's R1-m-1 (the no-tip decline) and R1-m-2 (event-only service, `hand_to` on every return) are not repeated.

## Findings

### M-1 (Medium): D45's stop order loses a `PruneClears` that W sends after V's last service, and CH2b's driver then aborts the node on an orderly stop

- **Where:** `dashd-chainstate/src/chain/actor.rs:663-670` (`run`: one `service_flush`, then the flush side drops with `self`); notes D45 (`CH1-node-notes.md:39`, "one more `service`, which answers or drops the pending `PruneClears`").
- **What it meets:** `flush/driver.rs:265-273` (`report` drops events once `closed`), `driver.rs:433-445` (`ThreadDriver::drop` closes, then joins W after W runs its queued jobs), and `flush/machine.rs:546` (`Polled::Dropped` → `Halt::clears_dropped`, an `AbortNode` with "The block index dropped the prune clears unanswered").
- **Scenario:** a pruning node and an orderly stop.
  1. An IF_NEEDED or PERIODIC site submits a prune job. W is between steps 0 and 6 when `Shutdown` is taken. This is easy to hit, because `Shutdown` is class Control and is also taken at a release point mid-ABC (`actor.rs:771-775`).
  2. `serve` returns `None`, and `run` services once. Nothing is pending, so nothing is answered.
  3. `self` drops, and `ThreadDriver::drop` sets `closed` and joins W.
  4. W reaches step 7. `report` drops the `PruneClears`. `PruneApply` sees `Dropped`, halts the job and calls the abort sink. Bk is never written.
  5. The result is an `AbortNode` (exit status 1 under ND1) and a lost prune, on a stop that nothing failed.
- **Contract gap:** `ThreadDriver::drop` documents "V's orderly stop waits for the release first, as Core's shutdown flush does" (`driver.rs:436-437`), but V's stop doesn't wait. CH2 §4.2's shutdown order (two ALWAYS flushes before V stops) would cover this. It needs N7's `Flush` command, which isn't on the branch yet (`command.rs:3`), and nothing in D45 or D47 states it as a precondition.
- **Core:** the prune runs synchronously inside the flush (`v:2989-3024`, then the write and `UnlinkPrunedFiles`). `Shutdown` runs `ForceFlushStateToDisk` before the chainstate goes away (init.cpp, as CH2 §4.2 steps 1 and 4 cite). An orderly stop never abandons a selected prune.
- **Fix (either):**
  - (a) Add `FlushSite::stop(host)`, run where `run` now calls `service` on the non-fatal path. The adapter implements it as "while a job is in flight, `wait_release`", which answers `PruneClears` inline.
  - (b) Have D45 and D47 state the precondition: ND1's ALWAYS flushes go through N7's `Flush` command before `Shutdown`, so no job is in flight when V stops. Then correct `ThreadDriver::drop`'s doc to say who waits.

  In both cases, add a model-W test in which W sends after `Shutdown` is taken (not before, as `a_prune_flush_pending_when_v_stops_is_answered` does). With (a) it expects the answer; with (b) it documents the drop.

### M-2 (Medium): D48 closes D18, but Core's companion devnet datadir check is not ported or owned

- **Where:** notes D48 (`CH1-node-notes.md:42`, "D18 is closed"); `chain/load.rs` / `NodeBlockIndex::load` + `load_chain_tip` (the restart path, `tests/support.rs:487-498`).
- **Core:** `node/chainstate.cpp:195-198` (v24.0.0-rc.3), right before `LoadGenesisBlock` (`:210`):
  ```
  if (!hashDevnetGenesisBlock.IsNull() && !BlockIndex().empty() && !LookupBlockIndex(hashDevnetGenesisBlock))
      return {FAILURE_INCOMPATIBLE_DB, "Incorrect or no devnet genesis block found. Wrong datadir for devnet specified?"};
  ```
- **The "no repeat" rule:** it is Core's. Core's `LoadGenesisBlock` returns early on `m_block_index.count(genesis)` (`v:5397-5398`), not on the devnet block's HAVE_DATA. `devnet_pending` is set only on an empty index, so the two agree.
- **Gap:** what makes a restart safe in Core is this separate check, and nothing here ports it.
  - Restarting a devnet datadir under another `-devnet` name loads the old devnet's block 1 and its chain silently. All devnets share block 0, and `NodeBlockIndex::load` / SB4's load don't re-check `hashDevnetGenesisBlock`.
  - The same applies to a devnet index that holds genesis without the devnet block, the "partial step" state. Core refuses to start in both cases. Here V starts and never repeats the step.
  - The partial state can't arise from N7's own persistence, because genesis and block 1 are dirtied together before any flush. It can arise from datadir reuse or corruption.
- **Fix:** port the check where ND1 chooses between `load_genesis_block` and the load path (`FAILURE_INCOMPATIBLE_DB` with Core's text), with a test: build a devnet index, restart it under another devnet name, and expect the init error. If it belongs to ND1 or CH8, give it a D-row with an owner, and keep D18's closure scoped to `v:5408-5416`.

### L-1 (Low): in-hold clears are visible to view readers before the hold publishes the tip that permitted them

- **Where:** `chain/flush.rs:221-226` → `index.rs:426-436` (`ix.modify`). This is a seqlocked arena write, visible to every `ChainReader` at once (`dash-chain/src/chain.rs:2079-2085`), while the snapshot and `ChainView` still show the pre-hold tip until release.
- **Scenario:** a forward hold connects k blocks (ABC's step). DEC-211 checks the selection against the working tip `T+k`. A reader of the view sees tip `T` and an entry with HAVE_DATA cleared up to height `T+k-288` (for example, the P2P getdata serving path or a `getblock` through the view).
  - Core never shows that pair. The prune runs under `cs_main` at `m_chain.Height()` (`v:2992`, `blockstorage.cpp` `FindFilesToPrune`'s `nLastBlockWeCanPrune`), and readers that take `cs_main` see the tip after ABCStep, which is at least the pruning tip.
  - A backward hold is safe: there DEC-211's working tip is below the published one.
  - The bound is one hold's connects. The block file still exists until W's step-12 unlink, so the effect is "reported pruned early", never "HAVE_DATA with no file".
- **Fix:** record it in D46 as an accepted divergence (bounded by one hold), or have the adapter pass `min(working, published)` height for DEC-211's check. That declines more often, and costs only a retry at the next flush.

### L-2 (Low): on a fatal or panic stop, V doesn't trip the store latch, so W's later behaviour depends on what it is waiting for

- **Where:** `actor.rs:651-660, 666-667` (`stop_serving` calls only V's `AbortSink`), versus CH2b's `control.rs` `abort()`, which trips the latch and then reports.
- **Today (b2):** after a non-flush `Fatal` (for example ConnectTip's read failure) with a prune in flight, the dropped request makes W report a second `AbortNode` ("The block index dropped the prune clears unanswered") after V's. Core reports one. W also keeps running the queued jobs during `ThreadDriver::drop`'s join, although Core's `AbortNode` path skips the shutdown flush (CH2 §4.2 / A1 §3.5.6).
- **Forward (b3):** once W waits on subscribers (the `FlushDue`/ack wait, which needs V's `retry_pending`), a stop that leaves the latch untripped makes `ThreadDriver::drop`'s join wait on an event nobody delivers. V's thread then never returns. Today the only W→V wait is `PruneClears`, which the close drops, so there is no hang yet.
- **Fix:** D45: on a `Fatal` or panic stop, V trips the store latch before its sink (as CH2b's `abort()` does), or the adapter's drop does. Note this as a b3 merge requirement beside D47's.

### N-1 (Nit): `FlushHost::prune_clear` sets `m_have_pruned` for an empty selection

`flush.rs:224`. Core sets it only `if (!setFilesToPrune.empty())` (`v:3019-3023`). W never sends an empty selection (`machine.rs:518-521`), but the host is a public `FlushIndex`. Guard it with `!files.is_empty()`.

### N-2 (Nit): `take_dirty`'s `unreachable!` and `prune_clear`'s error map handle the same infallible call two ways

`flush.rs:215-217` versus `index.rs:426` + `flush.rs:222`. `node_index()` fails only in SPV, and V's chain is the node's. Pick one form; `expect` in both is fine.

### N-3 (Nit): duplication

- "Receive the bell, drop it on disconnect, service it" is written three times: `actor.rs:110-115`, `:618-632` and `:953-958`. One helper on `Option<Bell>` would keep the three in step. The C-1 spin bug was exactly such a copy going stale.
- `V::with_flush` and `InHold::with_flush` differ only in the sink (`enqueue` versus `staged.push`). They could share one function that takes the sink.

### N-4 (Nit): the CPU idle test's margin shrinks under extreme load

`tests/flush.rs:559-572`. At load 60–70, a spinning V gets about 25 ticks per 500 ms, far above the bar of 5. At the box's peak load (about 190 on 32 vCPUs) it gets about 8. The test never flakes, because a non-spinning V uses about 0, but the mutant's margin is thin. Consider 1 s, or `ticks * 4 < elapsed_ticks_of_a_busy_reference`.

### N-5 (Nit): a devnet rejection reaches ND1 as `AbortNode`, not as an init error

`accept.rs:185-188`. Core's false `LoadGenesisBlock` is `InitError("Error initializing block database")` (`chainstate.cpp:210-211`), with no `AbortNode` UI text. D48 should say how ND1 presents a V `Fatal` raised before init's ABC: as an init error, or with a distinct `Fatal` kind.

## Checks (beyond Sol's)

- **Every V wait, enumerated** (`grep` of park/recv/wait/join/lock in `chain/*.rs`):
  - the idle `Lanes::recv`;
  - `VWait` (`limit_queue`, `sync`);
  - `drain_fanout`'s park, which is the only in-hold wait;
  - `barrier.wait()`, which is reached only with the lanes closed and the bell gone;
  - CH2b's own `wait_release` inside `at_site`/`after_step`.

  `ValidationSignals::enqueue`/`splice` never wait. SB4's `enforce_block` makes no wait between `TipHost` calls (`dash-chain/src/chain/enforce.rs:176-247`). Every V wait takes the bell or calls `service`.
- **No lost ring.** The bell is a one-slot channel plus a permit `Parker` (`dash-chain/src/notify.rs:41-`).
  - A ring that follows its push is always seen: the slot is consumed by select, `try_recv` or `VWait`, or left full while `drain_fanout` services anyway, and the permit survives a notify before park.
  - This needs the adapter's driver to push before it rings, which is D47's bell requirement.
  - A panic while V holds a popped `PruneClears` drops it during unwinding, and the one-shot wakes W (`clears.rs:170`). A panic with the request still queued: `ThreadDriver::drop` drops it. Neither case hangs in b2; see L-2 for b3.
- **The DEC-211 tip inside `enforce_block`.** `Steps::Enforce` lends SB4's composite chain, and `connect_step`/`disconnect_step` update `self.tip` on every step (`dash-chain/src/chain.rs:1621-1654`). `FlushHost::tip` is therefore the walk's working tip there too. `Steps::Lent` is never seen by a `FlushHost`: every `InHold` call runs inside `VHost::on`.
- **Signal order in a hold.**
  - At a disconnect, releases are staged in `at_site`, `after_step` and `drain_fanout`, all before `BlockDisconnected(d)` (`abc.rs:605-620`). A release of the flush just decided therefore lands as `CSF(locator d)` before `BlockDisconnected(d)`, which is Core's `v:3303` then `:3320`.
  - Older releases lag, as DEC-166 allows.
  - `ChainReader::locator(Some(id))` walks ancestors only (`reader.rs:218-240`), so computing it when the release is handed over equals Core's `GetLocator` at the site.
- **`have_pruned` timing.** It is set in the same call as the clears, before any later `check_block_index`. It is not in `ChainView`; only `ChainQuery` reports it, on V. See L-1 for the statuses.
- **Devnet step.**
  - CheckBlock comes first, then requested `AcceptBlock` with `checked`. Core's `fChecked` re-entry is equivalent.
  - `NewPoWValidBlock` stays unsent (null tip ≠ pprev), as in Core.
  - A write error gives `Fatal("System error: …")`, then `stop_serving`: one `AbortNode`.
  - A CheckBlock failure is a panic, then `catch_unwind`, then `stop_serving("V panicked …")`, then unwind.
  - The rejection test (`NoRoomAt`) matches Core's null-`FlatFilePos` `state.Error` (`v:4836-4839`).
  - Nothing is persisted before the step: no B1 runs before V's first flush, and an `AbortNode` stop skips the shutdown flush. A failed step therefore restarts on an empty index and re-runs, as Core does.
- **In-hold tests are deterministic.** The gated subscriber doesn't pop until W's answer, so V must block in `drain_fanout` at the asserted step. `w_in_hold`'s published-tip assert runs before `go`. The release-point test's ring happens on V, before ABC starts. The stop test pushes before `Shutdown`.
- **Ten runs** of `cargo test -p dashd-chainstate --all-features --lib chain:: -- --test-threads=4` at load 61–72: **144/144 passed every time** (2.8–8.1 s per run). The logs are in `run-{1..10}.log`.
- The scratch worktree (detached at c89bb1d01, never modified) and its target were removed after the runs; the logs and the review are kept here.

**Verdict: APPROVE-WITH-NITS.** This assumes M-1 and M-2 are settled in the R1 fix round as D-row and doc changes, plus M-1's test. For M-1 that means the stop precondition or a `FlushSite::stop`; for M-2, the devnet datadir check's port or its owner. Neither needs new code for N7a to be correct today, because it runs with `NoFlush` or the model W. Both must be settled before CH2b's adapter is wired.
