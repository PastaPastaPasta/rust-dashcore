# ND1 independent review, round 1 — Sol

Range: `c68b1c3e4..6e88abddd` (`node/nd1-rdashd`), reviewed in a detached worktree at `6e88abddd`. All 12 commits have a good (`G`) signature. The implementer's worktree and branch were not changed.

## Findings

1. **S1 — possible security note: see manager.** Location: `rdashd/src/main.rs:45-56` and `rdashd/tests/process.rs:86-103`.

2. **P1 — a full signal pipe can discard the only shutdown request.** Location: `rdashd/src/signals.rs:47-63, 113-137`. Core: `init.cpp:528-535` calls `StartShutdown`, whose atomic request flag is set before its wake token (`shutdown.cpp:66-84`). Here every SIGHUP writes an `H` to a nonblocking pipe, and SIGTERM/SIGINT writes a `T`; the handler ignores `EAGAIN`. If queued `H` bytes fill the pipe while its reader is delayed, `T` is lost. The reader reopens the log repeatedly, then blocks with `ShutdownState` still false. This is a direct state-machine counterexample; the normal signal and SIGHUP process test exercises only an empty pipe. Fix: latch shutdown and reopen independently in signal-safe atomics, and use the pipe only to wake the reader. On each wake, the reader must consume both latches, including after a failed write.

3. **P2 — `Interrupt` is neither once-only nor part of `Drop`.** Location: `dashd-node/src/node.rs:292-330, 343-346`. Core: `bitcoind.cpp:278-285` always invokes `Interrupt` before `Shutdown`, and `init.cpp:279-283` runs `ShutdownNotify` there. The binary path does this, but a library caller can call `Node::shutdown()` or drop a partly initialized `Node` and skip `-shutdownnotify`; two `interrupt()` calls execute it twice. The existing node tests call `interrupt()` explicitly and therefore do not exercise this contract. Fix: record whether interruption ran, make it idempotent, and invoke it from the common shutdown/Drop path before service teardown. Preserve the binary's one-call behavior.

4. **P2 — the in-process directory-lock registry does not retain a shared owner.** Location: `dashd-node/src/datadir.rs:68-100, 108-129`; `dashd-node/src/node.rs:45-46, 325-329`. Core: `util/fs_helpers.cpp:48-91` keeps the `FileLock` in the process registry until explicit release/process exit. A second `Node` in one process receives `Ok(None)` for the same lock and can start, but the first node's `DirLock::drop` removes the registry entry and closes the process-wide `fcntl` lock while the second node still runs. A third process can then take the data directory. The existing in-process lock test confirms the `Ok(None)` path but does not keep two owners alive across the first drop. Fix: return a reference-counted lock guard for every successful non-probe acquisition, or retain the OS lock in a process registry until the final owner exits. This does not affect the current one-node binary path.

5. **P2 test gap — pid removal order is unguarded.** Location: `rdashd/tests/common/mod.rs:154-158`, `rdashd/tests/features.rs:14-32`, and `dashd-node/src/node.rs:325-329`. Core: `init.cpp:504-519` removes the pid before directory locks are released. Moving `self.lock = None` before `pid.remove()` passed the entire `cargo test -p dashd-node -p rdashd` suite. Add a controlled shutdown race or an order-observing lock seam that proves a second instance cannot acquire the lock while the old pid file exists. The production order is currently correct.

## Core order and fidelity

| Phase | Core v24 reference | ND1 implementation and result |
| --- | --- | --- |
| Parse/config/help | `bitcoind.cpp:110-157` | `rdashd/src/main.rs:116-191`; config and selected-network refusal run before a write to a Core-shaped datadir. |
| Logging and soft-sets | `bitcoind.cpp:177-186`, `init.cpp:1000-1123` | `node.rs:103-119`; the v24 order is preserved. |
| Basic setup, steps 2–3 | `init.cpp:1144-1466` | `init/checks.rs`, `init/flags.rs`; 75 actual v24 init-error cases compare stderr and exit status; MN checks, governance, pruning, and logging errors are covered. |
| Sanity/probe/fork/lock | `init.cpp:1470-1507`, `bitcoind.cpp:198-233` | `node.rs:120-145`, `rdashd/src/daemon.rs`; no node-created thread precedes `fork`. |
| Pid, log, caches, scheduler | `init.cpp:1520-1592` | `node.rs:141-170`; default cache lines match Core exactly: 16 MiB each sig/script cache, 2.0 MiB block index, 8.0 MiB chain DB, 252.8 MiB UTXO. At `-dbcache=4 -maxsigcachesize=0`, both print 0 MiB/2 elements and 0.5/1.5/1.5 MiB. The explicit options are later refused by ND1's owner table as designed. |
| Chainstate and done | `init.cpp:1967-2156, 2388-2581, 2759-2787` | `node.rs:180-245`; current CH2 store seam opens/recoveries and the first CH1 node composition remains an interface dependency (DEC-199). Startup notify follows “Done loading.” |
| Interrupt and shutdown | `init.cpp:279-312, 314-519` | `main.rs:76-85`, `node.rs:292-330`; main process order and once-only shutdown pass. The one current WAL-sync flush is skipped after AbortNode, as the two eventual Core flush sites must be. Findings 3–5 describe API/order gaps. |

`dash-args`' generated registrations include every direct `SetupServerArgs` registration in Core's `init.cpp` (188 distinct names); ND1's support-table ratchet covers every registered option. The approved Inert values, including DTF's `-par`/`-parbls`, match plan §10 Q12. Logging format/options and DEC-200 target routing have unit coverage for prefixes, microseconds, thread/source flags, filtering, relative/absolute/disabled files, reopen, shrink, and buffering. Rust source locations necessarily name Rust modules. The active `-startupnotify` and `-shutdownnotify` hooks pass the configured command unchanged to `system(3)` with the inherited environment, like Core `util/system.cpp:1116-1126`; startup is detached and shutdown notify is joined by design. `%s` substitution belongs to currently unsupported block/alert/chainlock hooks, so ND1 does not widen those injection paths.

## Implementer-note dispositions

- TS_EOS success after the daemon setup token: accept as Core's parent behavior (`bitcoind.cpp:213-219`). The real post-fork lock-loss race returning parent exit 1 is the approved deviation.
- `SA_RESTART`: accept as a documented deviation; finding 2 concerns the missing durable signal state, not this flag.
- Fresh-HOME `-version` error: accept; the process test confirms rdashd's exit 1 and Core text, and Core's parse/config ordering supports it.
- No panic hook: accept for this cut; panics can print Rust's default line, outside the clean normal-stop claim.
- Non-UTF-8 argv: challenge; see S1 and the manager chat.
- `-mocktime=-1` as an InitError: accept per plan §11.
- Windows paths: uncompiled on this Linux reviewer host. macOS daemon zombie reaping assumption remains unvalidated here.
- `test-hooks`: accept the explicit `required-features` test gate, release `compile_error!` canary, and absence of self dev-dependency. The default release and bench builds passed without hooks.
- `umask(077)`: present and effective for newly created files; a fresh pid has mode 0600. An existing stale pid is overwritten and keeps its preexisting mode, as truncate-style creation does. The privacy test kills an `umask(022)` mutation.
- The script-verification-threads log line remains pending CH3a as ruled.

## Mutation results

Every mutant was applied alone in the detached worktree, then its original source was restored and touched. Exit 101 means a test killed it.

| Mutant | Test run | Result |
| --- | --- | --- |
| Skip Core-datadir refusal | `dashd_v24::a_data_directory_dashd_made_is_refused_untouched` | Killed (101) |
| Release lock before pid removal | Full `cargo test -p dashd-node -p rdashd` | **Survived (0)**; finding 5 |
| Flush after AbortNode | `rdashd --features test-hooks --test hooks` | Killed (101) |
| Replace `umask(077)` with `umask(022)` | `process::created_files_are_private` | Killed (101); the logging-only test did not detect it |
| Run startup notify after failed init | `features::notify_commands_run_once_per_start_and_stop` | Killed (101) |
| Format timestamps with a space instead of `T` | `logging::time::iso8601_matches_core_s_util_tests` | Killed (101) |

## Validation

- `cargo test -p dashd-node -p rdashd --no-fail-fast`: 87 library tests plus 2 binary unit and 3/6/7 process test groups passed; the differential group used the pinned `dashd v24.0.0-rc.3` binary for all 75 argument sets, compared real stderr and exit code, and did not skip.
- `cargo test -p rdashd --features test-hooks --test hooks`: 4 passed. `cargo build -p rdashd --release` and `cargo bench -p dashd-node -p rdashd --no-run` passed. A release build with `--features test-hooks` failed at the intended canary. `cargo fmt --all --check` and targeted `cargo clippy --all-targets -- -D warnings` passed.
- Default-feature `cargo tree -p rdashd -e features` contains neither `dash-spv` nor `test-hooks`; `contrib/node/check-layers.py` is absent on this base. Stable symlinked Core datadir refusal, stale pid replacement, two-process lock exclusion, clean SIGTERM, pid removal, and 0600 fresh pid/log/lock modes were probed on Linux. The process test suite passed again after all mutants were restored; `git diff` was empty.
- The Core comparison and process probes cover Linux. macOS and Windows process behavior, dynamic symlink-retarget races, and CH1-node/RP0 integration are outside this round's executable evidence.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
