# CH3a a9 (-par) review, Opus r1

**Branch:** `node/ch3a-a9`, head 887b464da (last code commit 70495899b), base ac41eae46. 26 commits.
**Plan:** DEC-323, DEC-450. **Notes read:** CH3a-notes D171–D176. **Reference:** Dash Core v24.0.0-rc.3 (da567d7dba): `validation.cpp` 2153-2238 and 2469-2790, `checkqueue.h` 60-260, `init.cpp` 340-365 and 1546-1570, `util/system.cpp` 1211-1214.
**Evidence read:** `/work/scratch/ch3a-a9/{mut,ibd,review,logs/pf0-adv-70495899b.log}`, and the validator's r1 and r2. I don't repeat their findings unless I disagree or add to them.
**Run here:** I ran these at 42b0a94bc, debug build. The implementer's follow-merge of train 33 (88b37e6c3) landed at 05:21Z, during this review. It changes only `dash-mnlist/` and `docs/`, so every a9 file is byte-identical to 887b464da (`git diff --stat 887b464da 42b0a94bc`). Built on a reflinked target under `/work/scratch/ch3a-a9-review-r1`, at load 95–116. All passed: the queue's unit tests, the connector queue and proposal tests (46), L3 CS6 (1), init caches and logging (26), the dashd thread census and `-par` help (2), rpc_diff's reasons test at four `-par` values (1), and node-l1's S0/S1 reasons test (1). Separately, I ran dashd and rdashd under a faked online-CPU count (m-1).

## Verdict: ACCEPT WITH NITS

The consensus wiring is right. I traced every return in Core's `ConnectBlock` between `CCheckQueueControl control(...)` (v:2649) and `control.Wait()` (v:2778), and every return in `connect_at` between `with_control` and `if !all_ok` (`dashd-chainstate/src/connect/serial.rs:744-990`). They correspond one for one, in the same order:

| Step | Core | Ours |
|---|---|---|
| ProcessSpecialTxsInBlock | v:2661 | serial.rs:824 |
| CheckTxInputs | v:2698 | :848 |
| CheckSharedCollateralSpends | v:2708 | :860 |
| fee range | v:2718 | :870 |
| BIP68 | v:2731 | :884 |
| sigops | v:2741 | :904 |
| inline CheckInputScripts | v:2752 | :933 |

Each of them returns before the wait, and the queue's verdict is ignored. Core gets that from the control's destructor (`checkqueue.h:241-244`); ours gets it from `Step::Stop`.

The two sides also agree on these points:
- **Mode choice.** `parallel = script_checks && has_threads` (serial.rs:744) is Core's `fScriptChecks && parallel_script_checks ? &queue : nullptr`.
- **Store policy.** Queued checks insert no exec-cache entry (`cacheFullScriptStore && !pvChecks`, v:2227 ↔ `script.rs` `check_input_scripts_unprobed`).
- **Assumevalid.** The skip builds the inline control (v:2649 ↔ serial.rs:744). BIP30 and `bad-sb-start` return before the control exists.

So (a), (b) and (c) follow from the structure. They are not a coincidence of the test data. I found no block where Core with threads reports a script reason: with `pvChecks` set, `CheckInputScripts` cannot fail on a script (`v:2196` queues the check instead of running it).

The thread-count arithmetic matches `init.cpp:1554-1565` bit for bit, including the `int` wrap. The one exception is Core's UB corner (the validator's I1), which I agree is unreachable. `get_num_cores` calls glibc's `__get_nprocs()`, the same function libstdc++'s `hardware_concurrency` calls. Concurrency is sound: the per-control `Shared` state, the scope-bounded borrows, and the sharded-mutex sigcache, whose soft-erase-on-hit is per bucket under the lock.

What remains are test-strength and evidence-method gaps. None changes a verdict.

## Findings

### Minor

**m-1. Mutant C (`_SC_NPROCESSORS_CONF`) is not equivalent in general, and it can be killed on this host without touching it.**
`dashd-node/src/init/caches.rs:134`. D176 records C as surviving because "CPUs 0–47 are possible, present and online". glibc reads `/sys/devices/system/cpu/online` for ONLN and counts the CPU directories for CONF. An unprivileged user and mount namespace can bind-mount a fake `online` over the real one, and that changes ONLN alone. I ran this here, as an unprivileged user, with no host change: `echo 0-3 > $D/online; unshare -rm sh -c 'mount --bind $D/online /sys/devices/system/cpu/online && exec <binary> -regtest -datadir=… -parbls=1 -listen=0 -server=0 …'`, then read the log line and counted `scriptch` threads in `/proc/<pid>/task/*/comm`. The scratch dir is deleted. Results:
- `getconf` gives ONLN 4 and CONF 48.
- dashd rc.3 logs "Script verification uses 3 additional threads" and runs 3 `d-scriptch.*`.
- rdashd `ad861088…` logs and runs the same.
- Mutant C would log 15.

This is also the only test that shows dashd itself counts *online* processors, not configured ones. That is DEC-450 (2)'s premise, and so far it rests on reading libstdc++.

*Fix:* add one case to `the_script_check_threads_are_dashd_s` (`rdashd/tests/dashd_v24.rs:473`) that runs both binaries under `unshare -rm` with a bind-mounted `online` (in `pre_exec`, or through a wrapper). Skip it with a message when `unshare` is refused. Then rerun C.

**m-2. Against dashd, the production connector's reason rule is proven for only three shapes.**
The three are rpc_diff's script-alone and script-then-`missingorspent` (`rdashd/tests/rpc_diff.rs:619-745`), and node-l1's script-then-`bad-txns-nonfinal` under S1 against the Core oracle (`node-l1/tests/l1_chain_diff.rs`). The connector matrix (`connector/queue.rs`) covers sigops-after and the post-`Wait` checks (`bad-cb-amount`, `bad-cb-payee`, `conflict-tx-lock`), but against a model of the rule, not Core. L3's (c) uses Core's data but runs a test-built loop (validator I3).

Two shapes from D172's own list have no row anywhere:
- **Script failure, then `bad-txns-accumulated-fee-outofrange`.** It is easy to build: `connector.rs:1431`'s seeds, with seed 1 locked by `FALSE`.
- **Script failure, then a `CheckSharedCollateralSpends` reject.** Only H8's *fault* is tested (`a_fault_after_a_queued_script_failure_is_a_system_error`).

Today the returns share one shape (`Step::Stop`), so the risk is low. But a later refactor that moves either check after `Wait` would survive every test.

*Fix:*
- Add both rows to `a_later_input_failure_wins_over_a_queued_script_failure` (`connector/queue.rs:164`). For the second, use a reject mode of the H8 stub.
- Add one block to rpc_diff's chain: a script failure plus a coinbase that overpays. Against dashd it pins "`Wait` precedes the coinbase checks", which is (b)/(c)'s other half. It costs one more `mine` call.

**m-3. The IBD harness subtracts one baseline run, and dashd's baselines vary by about 30%.**
`contrib/node/ch3a-par-ibd.py:203-214`. dashd's `-stopatheight=1` runs read:

| Release | -par=1 | -par=16 |
|---|---|---|
| 7ead1e957 | 1,087 M | 1,157 M |
| 9214b68e7 | 941 M | 836 M |

That is up to 1.7% of the 18.6 G IBD figure from one sample. This is most of why the two sessions disagree (0.918/0.930 against 0.910/0.910). rdashd's baselines hold to 0.5%.

The conclusion (rdashd ≤ dashd, by a margin of about 7–9%) survives this. A per-`-par` ratio quoted to three digits does not.

The measure is whole-process instructions:u. It shows total work, not that the work spreads. A pool where the master did every check would score the same.

*Fix:*
- Take the median of REPS baselines.
- Quote the ratio to two digits, or as a range across the two sessions.
- Record the instructions:u split between the main thread and the `scriptch` threads for both binaries (`perf stat --per-thread`, or each task's `utime` before exit) at -par=16. That shows the queue actually distributes the 32,000 checks.

### Nits

**n-1. Keep the PF0 attribution's A/B data on disk.**
D176's A/B (head release at `-par=1` against `-par=16`: +112 faults and +0.99 M instructions:u) is not under the scratch evidence paths. `pf0/` is empty and no log holds it.

I checked the PF0 row itself (`logs/pf0-adv-70495899b.log:52,59`: faults 11,286→11,400, +1.010%; instructions 57.993 M, +2.033%). The attribution is sound in kind:
- Genesis returns before the control, so the threads never take work.
- 7.6 faults and about 27 KiB per thread fit a rayon worker's stack touch, TLS and deque.
- About 66 k instructions per thread fits rayon's start-up and idle rounds before sleep.

The A/B explains 0.99 of the 1.18 M. The rest, 0.19 M, is base against head at `-par=1`: the new log line, `sysconf` reading `/sys`, `-par` parsing. Say so in D176.

*Fix:* save the `perf stat` output next to the PF0 logs, and add the base-against-head-at-`-par=1` leg so the decomposition closes.

**n-2. rayon's idle rounds are a cost Core doesn't pay.**
A rayon worker that runs out of work yields through its rounds before sleeping. Core's workers block on `m_worker_cv` at once. This is part of the measured +0.40% / +0.44% for 15 threads during IBD (68 M and 76 M over 305 blocks, about 250 k a block). It is harmless, but record it in D176 as the known source, so a future PF0 or IBD delta at -par=16 is read against it.

**n-3. `cs6-checkqueue-vectors.py:322` records `num_cores` from `os.cpu_count()`.**
On Python 3.13 and later (the box runs 3.14.4) that value honours `PYTHON_CPU_COUNT` and `-X cpu_count`. L3 cross-checks it against Core's logged counts, so a wrong value fails loudly. Still, record `os.sysconf("SC_NPROCESSORS_ONLN")`, the quantity DEC-450 names.

**n-4. D176 names "`digest replay`/l2/compare at 1/2/16/-4" in DEC-450 (3), but only `digest l2` takes `--par`.**
`conformance/src/digest/replay_core.rs:233` stays `CheckQueue::serial()`. The regtest nodes went through `l2` instead, which is fine, because `replay` runs the null connector. State in D176 that `replay` has no script path, so DEC-450 (3)'s "replay" is met by l2 over the same nodes.

**n-5. The `d-` strip in `logging/thread_name.rs` applies to any OS name beginning `d-` that rdashd never named, not only the script-check pool's.**
Today nothing else uses the prefix, and Core's `ThreadRename` would strip the same. Mention that in the doc comment, or match only `d-scriptch.`.

**n-6. CH5 forward note (the validator's I4, agreed).**
`-checklevel` is `Unsupported("CH5")` (`dashd-node/src/init/support.rs:121`). When level 4 lands, VerifyDB must reconnect on init's queue: Core's error text then carries `block-validation-failed` with threads.

## Concurrency (item 2), checked and clean

- **Data races.** Each control has its own `Arc<Shared>`, and `m_control_mutex` is mirrored (`checkqueue/mod.rs:278`). Checks borrow `'s` data inside `in_place_scope`, which joins every spawned helper before `control` returns. So no queued check outlives the block's slots or copies. `self.scratch.spent.reset()` runs after the scope (serial.rs, after `with_control`).
- **Deadlock.** `f` never opens a second control on the same queue. The BLS queue (`checkqueue/bls.rs:65`) is a separate pool and a separate lock. The master always drains (`shared.drain(false)`), so a pool starved of threads still finishes.
- **Panics.** A panicking check unwinds through rayon's scope to V's thread after the other jobs return. That is the existing V-panic path (connector.rs:2564 doc). Core would `std::terminate`. Either way the node dies, and no verdict leaks.
- **Shutdown and fork.** `-daemon` forks before `app_init_main` (`rdashd/src/main.rs:228-262`), so no pool exists at the fork. Dropping the rayon pool does not join its threads, unlike Core's `StopScriptCheckWorkerThreads`. V has stopped by then and no worker logs or writes, so nothing is observable (D171 says the same).
- **Reorgs and reindex.** One `Arc<CheckQueue>` lives for the process and is cloned into each `attach_parts` (`dashd-node/src/chainstate.rs:423`).
- **Caches.** Workers use `CachingVerifier` with `cacheStore = fJustCheck`, as `CScriptCheck` does. The sigcache is sharded `Mutex` buckets, and its soft erase on a hit happens under the bucket lock. A hit or a miss changes only speed, never the verdict: the entry commits to the sighash, the pubkey and the signature. This matches CS6.

## Thread count (item 3), checked

- `script_check_threads` (`checkqueue/mod.rs:62-71`) equals `init.cpp:1554-1565`, including the `int` wrap of `GetIntArg`'s `int64_t`.
- **sysconf failing.** It returns 0 cores, which is what libstdc++ returns when `get_nprocs()` < 0. On glibc, `sysconf(_SC_NPROCESSORS_ONLN)` *is* `__get_nprocs()`: it reads `/sys/.../online`, then `/proc/stat`, then `sched_getaffinity`. So the fallbacks agree too.
- **Affinity, cgroups and online CPUs.** Affinity and cgroup quotas are ignored, as in Core. The census's pinned case shows this, and m-1 adds the online-only case.
- **Clamping.** The cap of 15 is exercised at 16, 17, `i32::MAX`, and through the census at 16.

## Tests and mutants (item 4)

I read every mutant log in `/work/scratch/ch3a-a9/mut/`. Each verdict in D176 matches its log. Each kill comes from a non-tautological assertion against dashd, Core's recorded data, or a fixed expectation. `script_check_threads_follow_par` uses `get_num_cores()` only for the auto rows (the validator's L4), and the census anchors those to dashd.

The only surviving mutant is C (m-1). Missing rows: m-2.

## Verdict

**ACCEPT WITH NITS.** m-1 to m-3 are worth doing before the train, and none blocks it. m-1 is a few lines and closes the one surviving mutant. m-2 is two connector rows and one rpc_diff block. m-3 changes how the evidence is read, not the code. The `node-startup-genesis` re-anchor proposed in D176 is sound to take in a9's train.
