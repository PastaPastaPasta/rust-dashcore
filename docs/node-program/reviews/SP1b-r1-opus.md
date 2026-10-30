# SP1b r1: Opus review (questions 2, 4, 5 and regressions)

Review of the frozen worktree `/work/scratch/sp1b-r1/wt` at **f8c61e079**, range `31c3c81a6..f8c61e079`. Core was read at `v24.0.0-rc.4`.

## Verdict: **ACCEPT-WITH-NITS**

- The measure definitions match Core's tick (Q2).
- The rc.4 allow-sync capture is genuine, and its tests catch wrong transcripts (Q4).
- The diff changes no non-test Rust, so it cannot regress rdashd.
- The nightly job is not ready to install as is. Fix Minors 1, 2 and 4 before the manager installs the units. They are small fixes in `timing.py`/the unit, not a redesign.
- Minor 3, the tier 1 evidence, is the manager's call.

## Findings

### Minor 1: `run` probes `dashd -version` without a private `-datadir`, so it writes the shared `~/.dashcore/settings.json` and can fail the nightly at random
- **Where:** `contrib/node/sp1b-timing/timing.py:558-560`.
- **Problem:** the call is `subprocess.run([dashd, "-version"], …)` with no `-datadir`.
  - dashd v24 rewrites `<datadir>/settings.json` on `-version`: it writes a temp file and renames it over the original.
  - With no `-datadir`, that's the `~/.dashcore` every thread shares.
  - It is a write outside the unit's WorkingDirectory (Q5).
  - It breaks DEC-569: "always pass a private 0700 `-datadir`, even for a single serial `-version` check".
- **Scenario:** at 00:41 UTC another thread (or a second harness) runs a `dashd -version` at the same moment.
  - The loser of the rename race exits 1 with empty stdout.
  - `version.splitlines()[0]` at :559 then raises `IndexError`, and `nightly.sh` exits 1 before any session runs.
  - That is a spurious nightly failure. The race is the same one behind the 10-11 `scanner_parity` "not v24 ()" failures (about 2–5 % with two probes).
- **Fix:** create `root` first and pass `-datadir=<a 0700 dir under root>` to the probe.

### Minor 2: resuming an OUT_DIR that is already complete crashes `run`, so `nightly.sh` exits 1 without running `ks`
- **Where:** `timing.py:638-658`.
- **Problem:** `loads` gets its first value only after the main loop's first 1 s sleep, and only while a worker thread is still alive.
  - If every pair already has `--samples` good sessions, all workers return at once and `loads` stays empty.
  - Then `min(loads)` raises `ValueError`.
- **Reproduced:** in a reflink copy (with the `-version` probe given a private datadir), `samples.tsv` held one good session per (node, kind) and the command was `run --samples 1 --jobs 1`. Result: `ValueError: min() iterable argument is empty`, exit 1.
- **Scenario:** a night's run finishes `run`, but the unit is killed during `ks` (a T3/user-manager restart kills user units) or someone reruns it.
  - The rerun resumes into the same `out/YYYYMMDD`, as D22 and the usage text advertise, and fails before `ks`.
  - So `ks.tsv` is never written for a run that had complete data.
  - The same crash hits when every remaining pair is over its failure cap.
- **Fix:** guard the summary (`"load1": … if loads else None`), or take one load sample before starting the workers.

### Minor 3: the tier 1 evidence is a red gate, run at an older commit than the head
- **Where:** `/work/scratch/sp1b/gate-217fbdfd1.log` ends `GATE EXIT 1`. The `test` step reports `tests passed 7353 failed 5`:
  - `digest_l2` and `digest_replay_core` hit the 900 s timeout;
  - dash-spv `dashd_sync` failed 4 tests, and its rerun passed 38/38;
  - rdashd `rpc_diff::disconnectnode_and_getaddednodeinfo_match_dashd_v24` failed (panic at `rdashd/tests/common/mod.rs:523`).
- **Not caused by this slice:** the range changes no non-test Rust (`git diff --stat 31c3c81a6..f8c61e079 -- '*.rs'` shows only the two `dashd-masternode/tests` files), and no other crate reads the fixture directory.
  - I reran the rdashd test alone at the head, and it passes in 12.7 s.
- **Still:** the gate ran at 217fbdfd1, four commits before the head. f3ea0dadb and 135697d86 change `timing.py`/`nightly.sh`, and the other two change docs only.
  - The failing digest binaries were not rerun inside the gate.
  - READY should come with a green gate at f8c61e079, or the manager's waiver for these load-flake binaries.

### Minor 4: the nightly leaves session datadirs outside its WorkingDirectory, and nothing ever removes them
- **Where:** the unit (`sp1b-timing-nightly.service:187`) sets `TMPDIR=/work/tmp`; see also `timing.py:564`, `:625` and `:648-649`.
- **Problem:**
  - Each night makes a new `/work/tmp/sp1b-XXXX` root.
  - Up to 20 failed-session datadirs are kept on purpose, and the root is kept with them. Nothing sweeps old roots.
  - When systemd kills the unit (`TimeoutStartSec=10h` after a 4 h load wait and a slow run, or a user-manager restart), every in-flight datadir stays behind. At 128 jobs and about 18 MB per datadir (measured on the diag run's datadirs), that is about 2.3 GB per killed night.
- **Q5:** "no writes outside WorkingDirectory" does not hold, either here or in Minor 1.
- **Fix:** set `TMPDIR=%h/…` or `/work/conformance/nightly/sp1b-timing/tmp` in the unit, and have `nightly.sh` remove the previous nights' `sp1b-*` roots at start, or cap them.

### Nit 5: the allow-sync replay does not compare the node's final netfulfilled state with dashd's file
- **Where:** `dashd-masternode/tests/sync_transcripts.rs:306-313`.
- **Problem:** mutant M4 (drop `nf.add_fulfilled_request(&peer.addr, &allow)` at `dashd-masternode/src/sync_manager.rs:440`) **survives all 10 `sync_transcripts` tests**.
  - Without the key, the manual and noban peers are cleared again at every tick.
  - Nothing on the wire changes within this transcript.
  - The lib unit tests do kill M4 (`full_sync_peers_are_disconnected_unless_unrestricted`, `the_allow_sync_key_is_set_once_from_system_time`), so this is a coverage gap in the Core differential, not a hole in the tick.
- **Fix:** dashd's `mnsync-allow-sync.netfulfilled.dat` already holds the expected end state:
  - `allow-sync` on 127.0.0.1:30973 and :42039;
  - `full-sync` on the two outbound peers, plus `mempool-sync` on peer 1.

  Comparing SP1's netfulfilled map at the end of the replay against it, with the `allow-sync-<ms>` key name normalised, would close the gap.

### Nit 6: the nightly's failure signal
- **Exit codes:** `nightly.sh` exits 2 when the load never falls below 40, and 1 on a KS failure. systemd shows both as the unit being `failed`.
  - There is no `OnFailure=` hook, so nobody hears about either unless they check `systemctl --user --failed` or `nightly.log`. CH3a has the same gap.
  - On this box, load is often above 40, so expect frequent exit-2 nights.
  - Consider `SuccessExitStatus=2` with a clear "skipped" line, or make the exit-2 case distinct in the heartbeat.
- **Attribution:** `run-summary.json` records only the rdashd *path* (`timing.py:654`). Add `sha256sum "$SP1B_RDASHD"` and `git -C checkout rev-parse HEAD` to `nightly.log`, so a night's verdict can be tied to a build.

### Nit 7: doc drift
- `docs/node-program/designs/SP1-notes.md:49` (D17) still says capture.py "records four P2a-format transcripts". There are now five; PROVENANCE says five.
- D21 (`:53`), like the brief, cites `node/sync_manager.cpp:231-238` for the `mempool` request. At rc.4 the switch is at :217, the `-syncmempool` block is at :220-233, and the push is at :227.

## Question 2: the measure definitions against Core v24.0.0-rc.4

All four gated measures time what `SyncManager::ProcessTick` does.

**The common timers:**
- The scheduler runs every 1 s (`node/sync_manager.cpp:22-30`).
- The tick's gate is `GetTime() - nTimeLastProcess < 6` (:157-162). Its time is set on every passing tick, including FINISHED ones, so a reset keeps the gate's phase.

**first6 and first30:**
- `submitblock` bumps `nTimeLastBumped` synchronously, through `AcceptedBlockHeader` and `NotifyHeaderTip`. Those are direct `Iterate` calls (`validationinterface.cpp:269-277`, `masternode/sync.cpp:96-119`), so the reference "after `submitblock` returns" is sound.
- `UpdatedBlockTip`, which sets `fReachedBestHeader` (sync.cpp:155), is queued (`validationinterface.cpp:193-201`). It runs before any tick could switch, so it adds nothing to the measure.
- The switch needs `GetTime() - GetLastBump() > 6` (or 30) at a passing tick, with `snap.Nodes().size() > 3` over `FullyConnectedOnly` peers (:205-207). The f6 session has four such peers, confirmed by ping and pong.
- The `mempool` push to the non-inbound, non-block-relay peer happens inside that same tick (:217-233).
- So the expected support is (6, 13) s and (30, 37) s. The acceptance quantiles, 6.310/12.672 and 30.332/36.662, fit.

**reset_tick:**
- `mnsync reset` calls `Reset(true)` (`rpc/node.cpp:143-146`). That sets BLOCKCHAIN (sync.cpp:35) and does not touch `nTimeLastProcess`.
- The next passing tick disconnects every peer with `full-sync` (:196-201). `SyncFinished` gave `full-sync` to every node (:339-346).
- So the expected support is [0, 6). The acceptance p1/p99 are 0.068/5.945.

**gap_tick:**
- The peer is re-dialled to the same listener, so it has the same `CService`. netfulfilled is keyed by the full `CService` (`netfulfilledman.cpp:31-35`), so the peer keeps `full-sync` and is disconnected at the next passing tick: one gate period.
- The f6 outbound peer is `addnode onetry`, which makes it manual. Its first passing tick after the handshake always comes before the switch, because the switch is at least 6.3 s after `submitblock` and that is at least one tick after the handshake. That tick sets `allow-sync`, and FINISHED adds `full-sync` afterwards. So D18's claim holds: it behaves like a plain outbound peer after the reset.
- I checked the long gap_tick values in `accept1/samples.tsv`:
  - Values above 6.5 s: 6 for dashd, 5 for rdashd. They are 6.6–11.7 s and **not** multiples of 6 s.
  - They all come from the consecutive session IDs 22846–22864, on both nodes. That is one box stall, not a missed reconnect.
  - There is no 12 s cluster, so D20's close-path difference does not leak into gap_tick through the reconnect deadline.

**The reset reference is the call, not its reply (4d5cba5a3).** That's correct.
- The reset lands inside the RPC handler, while the tick runs on the scheduler thread. So a disconnecting tick can come before the reply. Measured from the reply, samples near 0 would come out negative, the `ticks[0] < wall_reset` check would reject them, and the sample would be biased.
- Measured from the call, the only window left is the request latency before `Reset()` runs. A tick that passes in that window sees `IsSynced()` and returns before the peer loop (:166-168). It moves the gate, so that sample correctly reads about 6 s from the call.
- Any bias is the difference in RPC latency between the nodes divided by 6 s, so negligible.

**Dropping the mempool interval holds up.**
- With governance off, `mempool` goes out in the BLOCKCHAIN→GOVERNANCE switch tick.
- FINISHED comes one passing tick later: GOVERNANCE with `!m_gov_manager.IsValid()` calls `SwitchToNextAsset()` and returns (:239-242). That is +36 for the mempool and +42 for FINISHED in `mnsync-disabled.txt` and `mnsync-allow-sync.txt`.
- So a "FINISHED→mempool" interval does not exist (it would be −6 s, fixed), and `first6`/`first30` already are the bump→`mempool` delay.
- The GOVERNANCE→FINISHED step is not sampled in real time. It is the same 6 s gate as `gap_tick`, and the mock-time transcripts pin it exactly.

## Question 4: the capture and its tests

**The capture is genuine.**
- **Binary:** `~/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin/dashd` has sha256 `e1be2564…f823a7`, as PROVENANCE states, and reports `v24.0.0-rc.4`. Transcript header: `/Dash Core:24.0.0/ (240000)`, args including `-whitebind=noban@127.0.0.1:30960`.
- **Hashes:** all 15 PROVENANCE sha256s match the files (`sha256sum -c`: 15/15 OK). The four older fixtures are byte-unchanged.
- **netfulfilled.dat agrees with the transcript:**
  - `allow-sync-1793119594660` on 127.0.0.1:30973 (the noban source port, `NOBAN_SOURCE_PORT+3`) and on :42039 (the manual peer's kernel-chosen listener);
  - `full-sync` on :43483 (peer 4) and :44525 (peer 1);
  - `mempool-sync` on peer 1 only;
  - expiries at mocktime +348, +342 and +336, i.e. +48, +42 and +36 plus regtest's 300 s.
- **Wall clock:** the `allow-sync` key is Core's wall-clock `nSyncStart` (:145-146), 2026-10-27 16:46:34 UTC. That is 14 minutes before 37af2ad7c was committed, so it was a real run, not hand-made.
- **Peer order:** the "tick at +42 looks at peer 1 only" window depends on unshuffled snapshots. `NodesSnapshot` defaults to `shuffle = false` (net.h:1576-1577), so the window is deterministic, as D23 says.

**The tests catch wrong transcripts and wrong code.**
- All 10 `sync_transcripts` tests and 3 `core_flatfiles` tests pass. Both new negative controls panic at the expected `mnsync-allow-sync:159 time 1700000048: peer N`.
- I ran mutants in a reflink copy, each restored from a backup with a fresh mtime:

| mutant | result |
|---|---|
| M1 `unrestricted` without NoBan | killed (`mnsync_allow_sync`) |
| M2 `unrestricted` without Manual | killed (`mnsync_allow_sync`, `the_noban_peer_is_compared`) |
| M3 no `remove_all_fulfilled_requests` | killed (2 tests) |
| M4 `allow-sync` key never added | **survives** sync_transcripts; killed by lib unit tests (Nit 5) |
| M5 fixture: drop `< 4 closed` | killed |
| M6 fixture: extra `< 2 closed` at +48 | killed (3 tests) |
| M7 fixture: closes moved to +49 | killed (3 tests) |

## Question 5: the nightly job
- **Bounded runtime:** yes. `TimeoutStartSec=10h` bounds 4 h of load wait plus about 3 h of run plus `ks`. On timeout, systemd kills the whole cgroup, nodes included (default `KillMode=control-group`).
- **Memory:** `MemoryMax=48G`, against a 9.3 GB peak.
- **Unit files:** both pass `systemd-analyze --user verify`, and the calendar resolves.
- **ExecStart quoting:** `%%Y%%m%%d` is right for unit specifiers. `$(date …)` survives systemd's `$VAR` expansion, because `(` is not a name character.
- **Load ceiling:** sensible as a start gate. During the run the load is only recorded, not enforced; that's D22's documented policy, and the statistics side is Sol's.
- **Fails loudly:** yes, as far as exit codes go. A failed `selftest`, a failed `run`, any gated FAIL or "too few", or a failure share above 1% gives exit 1 → unit `failed`. But see Minor 2 (a complete run that fails before `ks`) and Nit 6 (no alert, exit 2 not told apart).
- **Writes outside WorkingDirectory:** **no, this does not hold.** There are two: Minor 1 (`~/.dashcore/settings.json`) and Minor 4 (datadirs left in `/work/tmp`).

## Question 6: regressions
- No production code changes.
- The refactors, in `capture.py` (`status()`, `steps_until`) and `sync_transcripts.rs` (`fixture()`, `run_text()`), keep the four old scenarios' behaviour. Their tests pass unchanged.
- `core_flatfiles.rs` only adds the new scenario's files.
- The acceptance run's frozen harness, `/work/scratch/sp1b/accept-src`, is byte-identical to the head's `timing.py` and `nightly.sh`. Only `SP1-notes.md` changed after 135697d86.

## Commands run (and results)
- `git log`/`git diff --stat 31c3c81a6..f8c61e079` and a full diff read: 9 commits, 12 files. The only Rust changes are in two test files.
- `git -C ~/workspace/dash show v24.0.0-rc.4:src/{node/sync_manager.cpp,masternode/sync.cpp,masternode/sync.h,rpc/node.cpp,netfulfilledman.cpp,validationinterface.cpp,dsnotificationinterface.cpp,net.h}`: the line citations above.
- `sha256sum` of the fixtures, and `sha256sum -c` against the PROVENANCE table: 15/15 OK. `xxd` of the allow-sync `.dat` files.
- **Disclosure:** I ran `sha256sum` and `dashd -version` on the rc.4 binary. That `-version` was a bare probe with **no private `-datadir`**, against DEC-569. It was one serial probe, but it probably rewrote `~/.dashcore/settings.json` (mtime 23:35:28Z). Nothing else is known to be affected.
- `cargo test -p dashd-masternode --test sync_transcripts --test core_flatfiles`, with the target `target-opus`, at nice 10: 10 + 3 passed.
- **Mutants:** in a reflink copy (`opus-scratch/mut`, target `opus-scratch/target-mut`), using `opus-scratch/mutants.sh` and `edit.py`; M1–M7 as in the table. M4 against `--lib`: 2 unit tests fail.
- `timing.py run` resume reproduction, on the copy with the probe given a private datadir: `SP1B_PORT_BASE=24900 SP1B_RDASHD=/bin/false … run resume --samples 1 --jobs 1` gave `ValueError` and exit 1.
- `cargo test -p rdashd --test rpc_diff disconnectnode_and_getaddednodeinfo_match_dashd_v24` at the head: 1 passed (12.7 s).
- `systemd-analyze --user verify` on scratch copies of the units: clean. `systemd-analyze calendar '*-*-* 00:41:00 UTC'`: OK.
- `awk` over `accept1/samples.tsv` for the gap_tick and reset_tick tails: see Question 2.
- **Not run:** no acceptance or smoke session run (none was needed for my scope), and no `dash-oracle-*` services touched.
- **Cleanup:** I deleted the mutant copy and its target. The reports, scripts and logs stay in `opus-scratch/`.
