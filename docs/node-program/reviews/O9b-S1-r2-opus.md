# O9b S1 (`-consensusxcheck`) — code review r2, narrow re-check (Opus)

- Branch `node/o9b`, worktree `/work/home/agent/workspace/rdc-wt/o9b`, head 91e40657c (clean; read-only).
- Reviewed: the fix round 54c4b8913 (r1 m-1..n-4) and the bundle recipe 91e40657c (tmp-then-rename only).
- Sources: r1 (`reviews/O9b-S1-r1-opus.md`), DEC-708, O9b-notes D2/D4/D14–D16, BL-271's train-47 diff (`git -C rdc-wt/integration diff node/integration...node/train-47 -- dashd-chainstate dashd-node`), Core v24.0.0-rc.4 (`git -C ~/workspace/dash show v24.0.0-rc.4:…`).
- No build, no reflink copy. Everything below comes from reading the code, plus one look at the implementer's scratch TMPDIR (m-2).

## Verdict: **ACCEPT-WITH-NITS** (0 M / 2 m / 2 n)

m-3 and n-1..n-4 are closed as r1 asked. The m-2 test is sound and kills B1–B3. It isn't hermetic, though: the aborted child leaks a ~17 MB datadir on every run (m-2 below).

m-1 is only half closed. The new store/coins assertions are correct, but they can't tell "the connector committed nothing" from "the connector committed and the flush refused it". So mutant A isn't equivalent; it's masked. D4 also misnames why. Both minors are a few lines each. Fix them before READY; a manager delta check is enough, with no r3.

## Ruling on mutant A: **NOT equivalent.** It is killed in tier 1 at its own layer. At node level it is masked by a deliberate, tested, non-Core guard, and the node test should be made to see it (m-1).

1. **It is observable.**
   - The mutant leaves the coins cache's best block at 103 while V's tip is 102.
   - In this base, nothing writes after an `AbortNode`: the shutdown flushes are the WAL-sync stub, and `stop_flush_at_exit` only services and releases. So the store is untouched.
   - With BL-271's train-47 diff, `VExit::force_flush(ShutdownFlush::…)` → `NodeFlush::flush(Always)` → `FlushControl::flush` → `decide`:
     - The cache is dirty and has a best block, so `clean_at_durable` is false (`flush/control.rs:526-529`).
     - That gives `Freeze` (`flush/policy.rs:236-251`) → `execute` → `check_pair` (`flush/control.rs:603`, `:653-672`).
     - `check_pair` sees `best == Some(103) != cx.tip == 102`, so `abort()` (`:964-973`) trips the latch and reports a **second** `AbortNode`: `*** a freeze at <103> (seq …) that V's tip <102> (seq …), the coins' best block … does not match`.
     - Nothing reaches W, which is exactly the implementer's "store stays at 102 with the coins intact". The node does differ, though: two `***` lines instead of one, a tripped latch, and a failed shutdown flush.
2. **The layer that stops it is CH1-node §7.1's freeze-pair check** (`check_pair`, D3). That is the "layer below S1" D4 left unnamed.
   - **It is deliberate and tested**: `flush/control/tests.rs:355` `a_mislabelled_freeze_aborts_the_node`. Deleting the check fails that test.
   - **It is not Core-shaped, and not an abort-path guard.** Its own comment says Core checks nothing here: "Core's flush writes the best block it reads" (`control.rs:649-651`).
   - **Core's protection is structural, in ConnectTip.** The block's `CCoinsViewCache view(&CoinsTip())` is scoped. On `!rv` it returns before `view.Flush()`, so the view is discarded (rc.4 `validation.cpp:3436-3443` vs `:3452-3453`).
   - **Core's AbortNode does not skip the shutdown flush.** `Shutdown` runs `ForceFlushStateToDisk()` regardless (rc.4 `init.cpp:383` and `:453`, which DEC-241 mirrors).
   - So in Core, the analogue of mutant A (a `view.Flush()` on failure) would persist block 103's coins. In rdashd, a guard written for mislabelled packets catches it by accident.
   - rdashd's analogue of Core's scoping is the connector's `(out, Ok(_)) => rollback_tx` arm with the overlay dropped (`connect/connector.rs:341-345`). That arm is what mutant A breaks.
3. **Tier 1 already kills A.** `chain/tests/connector.rs:1432` (`xcheck_catches_a_memo_that_answers_the_wrong_input`, under dashd-chainstate's `planted-bugs`, which is in its gate `test-features`, `Cargo.toml:126`) asserts `coins_at(&w.coins) == genesis` after `w.rig.stop()` joins V, and `:1430` asserts "one report". "Survives" is true only of dashd-node's tests.
4. **The cheapest test that kills A at node level:** in `consensusxcheck_stops_the_node_on_a_planted_memo`, after `sut.restart(Teardown::Clean)`, read `lines` again and assert:
   - exactly one line contains `*** `;
   - no line contains `a freeze at`.

   Details:
   - `lines` spans the teardown and the reopen: `open()` reuses `self.ui.logger()` (`sut.rs:297-298`).
   - This kills A as soon as BL-271 is in the base. Train 47 brings it, so S1's follow-merge makes it load-bearing.
   - In the current base the shutdown doesn't flush, and there is no race-free node-level observable. Reading the coins cell after `aborted()` would race V's exit path, whose `lock_cell` only tries the lock (`connect/connector.rs:254-260` via `connect/mod.rs:30`; `flush/site.rs:167`). After BL-271, a lost try-lock there decides whether the shutdown flush runs at all. So use the log count, not the cell.
   - Removal of `check_pair` is already killed by its own test. A + no `check_pair` would also fail the node test's store assertions as written. No extra test is needed for the guard.

---

## Minor

### m-1. The m-1 fix's "nothing committed" checks pass under mutant A for the wrong reason, and D4 misstates why
`dashd-node/src/sut.rs:1135-1167` (the r2 assertions), `docs/node-program/designs/O9b-notes.md` D4.

**Mutant (A):** at `connect/connector.rs:341-345`, commit the overlay (`ov.commit(best)`) before returning the xcheck `Fatal`.

**What happens:**
- In this base, every r2 assertion still passes: the store, the reopened coins tip, V's tip, spent coins unspent, output absent.
- With BL-271, they still pass because `check_pair` aborts the shutdown flush. The log then holds a second `***` line, which the test doesn't look at: it only checks `any(… "*** {fatal}")`.

D4 says the mutant survives "for two reasons outside S1" and that "The checks stay because they test the stored state … and would fail on a store at the block or holding its coins". The first half is right. But the reason the store isn't at the block is a mislabel guard, not the absence of a commit. D4 doesn't record that A is killed at `connector.rs:1432`.

**Fix:**
1. Add the two assertions from ruling item 4 after the restart. Confirm in the existing BL-271 reflink recipe (`/work/scratch/o9b/bl271.patch`) that A now fails with them.
2. Reword D4 to say:
   - A is killed in tier 1 by `chain/tests/connector.rs:1432` (the coins' best block, and one report);
   - at node level it is masked by CH1-node §7.1's `check_pair` (`flush/control.rs:653`), whose own test is `a_mislabelled_freeze_aborts_the_node`;
   - after BL-271 the one-`***`-line assertion sees it;
   - Core's equivalent protection is ConnectTip's scoped view (rc.4 `validation.cpp:3436-3453`), not AbortNode: Core's shutdown flush runs after it (`init.cpp:383`, `:453`).

### m-2. The SIGABRT child leaks its datadir on every run
`dashd-node/src/sut.rs`, `consensusxcheck_reaches_the_coins_cache_prefetch_hits` (`:1204`): `let dir = tempfile::tempdir()` runs before the `PREFETCH_CHILD` branch, so the child makes its own `TempDir`.

**The leak:**
- The child dies in `std::process::abort()` (`store/abort.rs:56`), so `TempDir::drop` never runs.
- What it leaves is a regtest datadir of about 17 MB: the blk file's 16 MiB preallocation plus RocksDB.
- Every run of the test, every gate shard and every mutant iteration leaves one. Under the gate's `/dev/shm` slot TMPDIRs, that is shared RAM quota.

**Evidence:** `/work/scratch/o9b/tmp` holds 22 orphaned `.tmp*` regtest datadirs from 08:15–08:42 (the m-2 work window), 19 of them 17M. Some may come from other aborted runs, but the child can only leak.

No processes are left over: `output()` waits, and the abort ends every thread.

**Fix:** have the parent create the `TempDir` and pass its path as the env value. In the child, use `std::env::var_os(PREFETCH_CHILD).map(PathBuf::from)` as the datadir. The parent's `TempDir` then removes it after `output()` returns. Keep `ulimit -c 0`.

**Soundness (the rest of focus 2), with no finding:**
- **Only the right abort can pass it.**
  - Both stderr strings come only from `CoreAbortSink::read_failed`: `Error: Error reading from database, shutting down.` (`abort.rs:50-54`) and the logged `Error reading from database: … consensusxcheck: the prefetched coin differs from the database's` (`abort.rs:55`, `coins/cache.rs:228-240`, `store/guarded.rs:205-216`).
  - Both are written before `abort()`.
  - A double-panic SIGABRT, or another read failure, would lack the cause.
  - A returned read panics (`returned: …`), which gives exit 101, not signal 6.
  - A wrong test path runs 0 tests, which gives exit 0.
  - So neither "any abort" nor "a missing message" can pass.
- **`-noconsensusxcheck` really serves the prefetched coin.** The outpoint (`[7;32]:3`) isn't in the database, so `Some(coin)` can only come from the prefetch map (`peek_below`, `cache.rs:219-223`). `admit_prefetch(...) == 1` is asserted, so an unadmitted batch can't make it vacuous.
- **The mutants:**

  | Mutant | Killed by |
  |---|---|
  | `set_xcheck` deleted | the child returns, so exit 101 |
  | forced false | the child returns, so exit 101 |
  | forced true | the parent's `-noconsensusxcheck` leg gets `Err` |

  This is a different input from r1's "wrong coin for a mature coinbase" suggestion, and it is equally discriminating.

---

## Nits

### n-1. 91e40657c: a failure after the rename reports failure and tries to remove a partial that's already gone
`conformance/src/segment/bundle.rs:230-244`

**Failing input:** the parent directory's `sync_all` fails, for example EIO on the bundles dir.
- By then `rename(&partial, out)` has succeeded.
- The closure returns `Err`, and the error arm runs `remove_dir_all(&partial)`, which fails with NotFound. The error reads "… and removing .<id>.partial failed: No such file or directory".
- `out` stays behind as a complete, valid bundle (`segment open` accepts it), but `make` reported failure. A rerun is then refused by `ensure!(!out.exists())`.

This is harmless (nothing half-made is under the name), but it contradicts the doc's "a failed one removes the partial directory".

**Fix:** make the rename the commit point. Do the rename and the parent sync after the closure. A failed sync after the rename should return an error that says the bundle is complete at `out` and only its parent's sync failed, with no removal.

### n-2. 91e40657c: a rerun after a killed `make` is refused without saying why, and that behaviour has no test
`conformance/src/segment/bundle.rs:187`

A killed run leaves `.<id>.partial`. The rerun's `create_dir(&partial)` fails ("creating …/.<id>.partial: File exists (os error 17)") before the closure, so nothing is removed or overwritten. That is the right behaviour: the exclusive `create_dir` is also what keeps two concurrent makers apart, and auto-removing would break that.

The problem is that the operator gets a bare EEXIST. And nothing pins the behaviour, so a later "convenience" remove-and-retry would pass the tests.

**Fix:**
- Context on that error: "a previous run left it (killed?); remove it to rebuild".
- A test in `conformance/tests/segment.rs` that pre-creates `.c.partial` with a file in it, then asserts the refusal, that the file is untouched, and that `c` is absent.

**Checked, no finding (focus 4):**
- Killed `make`: nothing exists under the bundle's name until `rename`, so a killed run can't leave a half bundle under it.
- Consumers address bundles only by `<bundles>/<id>`: `segment-run.sh:65`, `--start-utxo "$bundles/$id/utxo.dat"`. No `read_dir` over the bundles dir, so `.<id>.partial` is never picked up.
- `segment open` (`bundle.rs:261-300`) requires the exact file set, each sha256 and the manifest/sidecar match. So even a power-loss-torn bundle (the manifest and SUMS are not `sync_all`'d before the rename) is refused, not mistaken for complete.
- The `ensure!(!out.exists())`→`rename` window can only replace a concurrently created *empty* directory (Linux `rename(2)`), which is immaterial.
- The test's added `.c.partial` assertion covers the failed-run cleanup.

---

## r1 items closed (focus 3)

| r1 | Status | Where |
|---|---|---|
| m-1 | **partly** (m-1 above) | `sut.rs:1135-1167`: ALWAYS flush at 102, teardown, unplanted reopen; the store tip, coins tip, V tip and coin assertions are correct; `unwrap_or` dropped |
| m-2 | closed; hermeticity is m-2 above | `sut.rs` prefetch test; D2 updated |
| m-3 | closed | CH3a D189 now "off by default in the node; `-consensusxcheck` (O9b S1, O9b-notes D1–D3) turns it on through `CoinsSetup::xcheck`"; `connector.rs:67-68` reworded; no other "until O9b" text in the tree (grep) |
| n-1 | closed | `support.rs:308-316` slices the Debugging/Testing group up to the next unindented heading. `DebugTest`→`NodeRelay`/`Rpc`/any earlier category leaves the option outside the slice, so the test fails. Headings are unindented and options indented, as in Core's `HelpMessageGroup`/`HelpMessageOpt` |
| n-2 | closed | the help text names the coin reads; D2 says CH7a widens it |
| n-3 | closed | `chainstate.rs:455-463` `assert!` that only `None \| L1 \| T12` reaches `connector.plant`; the doc comment says why |
| n-4 | closed | O9b-notes D15 gives the FT harness pass-through to O14 with its ALLOWED-DIFFS row (D5) |

## What I checked and ran

- Read `git show 54c4b8913` and `git show 91e40657c` in full. Worktree clean at 91e40657c.
- The abort path:
  - `chain/actor.rs` `run`, `stop_serving`, `stop_flush_at_exit` and `abort_at_fault` (`:1120-1370`);
  - `flush/site.rs` (`with_cx`/`run` build the cx at V's tip; `abort` leaves the latch open on `AbortNode`, CH2b-b4 D59 / DEC-241);
  - `flush/control.rs` `flush`/`decide`/`execute`/`check_pair`/`abort`;
  - `flush/policy.rs` `decide` (ALWAYS + dirty + best block → `Freeze`).
- The connector's connect bracket (`connect/connector.rs:262-354`).
- BL-271's train-47 diff for `VExit::force_flush` and `run`'s `open` hand-back (actor.rs), plus `NodeAbort::reports` (abort.rs).
- Core rc.4: `validation.cpp:3436-3453` (ConnectTip's scoped view, `Flush` only after `rv`) and `init.cpp:375-386`, `:453-454` (the shutdown's `ForceFlushStateToDisk`).
- The tests: the connector matrix `connector.rs:1395-1433` (`coins_at`, `:1220`); `flush/control/tests.rs:352-386`; `store/abort.rs:46-68`; `coins/cache.rs:217-240`; `store/guarded.rs:205-216`; `sut.rs` `restart`/`open`/session (logger reuse).
- `bundle.rs` `make`/`open`; `segment-run.sh` bundle use.
- Listed `/work/scratch/o9b/tmp` (m-2 evidence). Read only; nothing deleted.
- **Not run:** no build, tests, mutants or tier 1. Box load and the brief's read-first rule. The ruling on A's BL-271 behaviour (the second `***` line) is reasoned from the code above, not observed. The implementer's BL-271 reflink recipe can confirm it in one run (m-1 fix step 1).
