# O1c independent Sol review, narrow round 4

Reviewed Core `54c5ae2075..66c4e6472f` and rust-dashcore `a5efa8756..3249e779f` in detached worktrees under `/work/scratch/o1c-sol-r4/`. Review only; no implementation commit or push.

## Closure

| Item | Result | Evidence |
|---|---|---|
| R3 blocker: stats client and the process-state class | **Behavior closed on successful teardown, refused start, and throwing start.** `ProcessGlobals::Save` moves the inactive stats client out before setup and `Restore` moves it back after destroying the node. The broad before/after sweep, contextual suite, and a direct stats-restoration mutant support the fix. A throwing teardown deliberately does not restore Core state while the node remains alive; the Rust pin is poisoned instead. | Core `oracle_session.cpp` `ProcessGlobals`, `FreeSession`; `process_state.rs` 2/2, `contextual.rs` 18/18. Skipping the stats restore makes the sweep's child die with SIGSEGV (exit 139). |
| (a) Other calls during a live session | **Closed for the safe Rust API.** All public Rust methods that call Core are under `shared`, `on_chain`, or the session's exclusive guard, except the documented global-free getters and `AddrMan::drop`. Same-thread calls return `SessionLive`; another thread waits. No callback into Rust or shared-to-exclusive upgrade was found. **Direct C callers can still break the no-call-during-session rule**; `oracle.h` explicitly documents the rule. | Entry-point scan across `lib.rs`, `context.rs`, `mnlist.rs`, `p2p.rs`, `addrman.rs`, `peer_session.rs`; `other_calls_wait_for_a_live_session` passes. Removing `merkle_root`'s shared guard is killed by that test. |
| (b) Init refusal fidelity | **Behavior closed; regression gap below.** The oracle-hook path now throws `AppInitParameterInteraction`'s error message. Core's `SetLoggingCategories` and `SetLoggingLevel` refuse invalid `-debug` and `-loglevel`; a review-only probe verified all three forms and recovery by a later valid session. | Core `setup_common.cpp:229`, `init.cpp:1332`, `init/common.cpp:59`; review probe 2/2 on the pin. |

## Process-global audit

| State touched by a session | Exit handling | Evidence and limit |
|---|---|---|
| Logger filters, levels, format, message capture, inline validation queue, EHF stand-in | Caller logger and capture restored; queue unset; stand-in reinstalled by the next context. | R2/R3 logger and context regressions, contextual suite and full process sweep pass. |
| `g_stats_client` | Saved before setup, restored after node destruction. | Full sweep includes `check_block` and `check_input_scripts`; stats mutant dies at the first post-session sweep. |
| Mock time, `g_best_block`, private-net flag, connection timeout, bytes per sigop, pruning flags/target, socket-events mode | Caller values saved/restored. | `ProcessGlobals` save/restore; broad sweep and repeat busy/plain transcripts cover observable effects. There is no direct assertion for each raw scalar. |
| BLS legacy scheme, X11 dispatch pointers | Caller values saved/restored. BLS secure allocator initialized once for both pre- and post-session oracle state. | BLS and X11 calls are in the sweep; transcript replay passes. |
| OS and internal thread name, umask, `LC_ALL` | Caller values saved/restored. | A review-only probe with umask `027`, invalid locale and a named thread passes. The committed suite does not directly assert these values. |
| Signature and execution caches; time-data samples; IBD flag; SOCKS interrupt; warnings; insecure test RNG | Returned to fresh-process state after teardown. | Reset calls in `ProcessGlobals::Restore`; sweep tests the result-producing calls, but does not inspect each cache or flag directly. The execution cache is deliberately reinitialized empty. |
| ChainLock coinbase, quorum-member and CbTx ChainLock caches; orphan sweep; validation warning/header-tip statics; init connection/services/filter statics | Reset to fresh-process state, with oracle-hook reset functions where function-local statics were inaccessible. | Core reset hooks are confined to `DASH_ORACLE_HOOKS`; busy session, refused fixture and four throwing starts are followed by the full sweep and repeated session transcripts. The sweep does not drive every cache entry. |
| `gArgs` options/path/network and selected chain/base params | Cleared and deselected. The next contextual or chain call selects its chain again. | `oracle.h` documents re-selection for direct C callers; Rust discards its cached chain when a session begins. Mainnet and regtest sweep calls pass after each teardown. |
| Validation interface registrations, scheduler, script-check workers, noui UI connections, datadir | Unregistered/stopped/disconnected/removed by setup destructors, including partially constructed setup cleanup. RPC table is registered once and kept intentionally. | Constructor paths and `FreeSession` audit; throwing-start cases and process sweep pass. |
| Failed teardown | **No restore:** Core node is retained because destruction would be unsafe. Free prefixes the last error with `teardown failed`; Rust records `Poisoned` before releasing its exclusive lock. | Both free and failed-start fault paths pass in child processes that call `_exit(0)`. Every result-returning Rust Core entry point checks pin usability. ABI/version, script-error text, thread-local last-error and `AddrMan::drop` remain deliberately callable. |

Stable one-time state (SHA implementation, RPC registration, allocator setup and content-keyed caches) and diagnostic counters are intentionally retained, as recorded in D4. `AddrMan::op` changes mock time only while holding Core's process-state mutex. Other shared calls that use process-global BLS or contextual state use that mutex too. I found no additional reachable process-global leak in the reviewed range.

## New findings

1. **R4-01, required regression gap:** The committed suites do not assert refusal of invalid `-debug` or `-loglevel`. The only `AppInitParameterInteraction` refusal in `process_state.rs` is `-prune=-1`. A mutant that accepts `-loglevel=not_a_level` passes all 28 peer-session and 2 process-state tests; the review-only invalid-options test kills it. Add a permanent test for invalid global and category log levels and an invalid debug category, asserting Core's reason and successful state recovery.
2. **R4-02, process-state regression gap:** `ProcessGlobals` restores thread names, umask and `LC_ALL`, but the committed before/after sweep never observes them. A review-only regression exercised an invalid locale, umask `027` and the Linux OS thread name across a session and passed. A mutant that skips umask restoration survives all 28 peer-session and 2 process-state tests but is killed by that probe (`077` observed instead of `027`). Add this or an equivalent child-process test so these restore paths remain covered. The internal thread name also lacks a direct assertion; the existing logger transcript is only an indirect check.

No new functional failure was found on the pinned code. R4-01 and R4-02 matter because this round's requested gate includes a regression for each process-global class and explicit invalid logging-option refusal.

## Mutations

| Mutation, applied alone in review worktrees | Test result |
|---|---|
| Remove the shared guard from `merkle_root` | **Killed:** `other_calls_wait_for_a_live_session` receives `Ok` where it requires `SessionLive`. |
| Skip `g_stats_client` restoration | **Killed:** `sessions_leave_the_process_as_a_fresh_one` fails; its child segfaults with exit 139 on the post-session sweep. |
| Treat the `teardown failed` prefix as success in Rust | **Killed:** `a_failed_teardown_poisons_the_pin` finds a successful decoder call on unknown Core state; its child then hits Core's static check-queue assertion. |
| Accept `-loglevel=not_a_level` despite Core's init refusal | **Survives committed focused suites:** 28/28 peer-session and 2/2 process-state pass. **Killed by review-only test:** start returns a session where Core's error is required. |
| Skip umask restoration | **Survives committed focused suites:** 28/28 peer-session and 2/2 process-state pass. **Killed by review-only test:** caller umask changes from `027` to `077`. |

Each mutation was restored and the file touched before the next build. C mutants used an isolated unpinned dev archive; pinned archives were not changed.

## Re-pin and validation

- The installed plain/fuzz SHA-256s match `pins.txt` and both MANIFESTs: `2691cedceb92d5027a5be79dc8765c53d996dfd3c44292f667b30a02053003f3` and `f36e2b59ce830b0c72cc516fd13d5cf0f30d2707866578419ac2dfd0d248b288`. MANIFEST recipe SHA `51b5ded2…` matches `build-oracle.sh`. The retained build logs show two clean roots reproducing each flavor; I verified the installed bytes and records, not independent two-root rebuilds.
- All 54 changed SB4 compressed vectors were decoded and compared semantically. Only provenance changes, except `bench-pnbh-mainnet-dgw` instructions `491803130 → 492321113` and KGW `870401287 → 870217508`. The CS3 check rows and primitive fixture payloads are unchanged; only provenance changes. No scratch or home absolute path appears in the vectors, CS3 fixtures or archive bytes.
- Independent pinned tests: `peer_session` 28/28, `process_state` 2/2, `contextual` 18/18, `l1_check_rows` 2/2 and `recorded_fixtures` 2/2. Review-only invalid-option and OS-state probes pass 2/2. The old `/work/scratch/o1c-r3-sol/` probe directory was absent; the corresponding logger/context/stats sequences were rerun through these current tests and the new probes.
- Full SB4 `regen.py --check --jobs 4` independently exited 0: 52 exact vectors and 2 tolerance-checked benchmarks, each under up, down and seeded heap perturbations; 8 delivery schedules passed for each eligible ChainLock scenario. The implementer's retained gate says EXIT 0, 3808/0 tests, 43/43 fuzz-smoke legs and passing oracle-diff; these are not an independent full-gate run.
- Validation used `RUSTC_WRAPPER=`, nice 10, mode-0700 disk `TMPDIR`, and an isolated `CARGO_TARGET_DIR` seeded by reflink. Long builds and regeneration ran under `systemd-run --user`.

**Verdict:** CHANGES-REQUIRED (regression gaps R4-01 and R4-02)

Sol gate satisfied: no
