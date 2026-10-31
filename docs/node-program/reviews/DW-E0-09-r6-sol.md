# DW-E0-09 r6 — Sol confirmation review

Verdict: **CHANGES-REQUIRED**. Both r5 SPV-panic reproductions are fixed, the required gates pass (**640 passed, 0 failed, 2 ignored**), and the approximately 70 s startup-panic path is acceptable for E0-09 with an E0-05 follow-up. One **P2 / major** failure of the deadline boundary remains: failed thread creation can drop a wedged runtime on the caller before the timed wait begins. There is also one **minor / P3** release-exclusion finding. **Sol gate: NOT SATISFIED for the sequential Opus-high pass.** No P1 finding.

Reviewed `75c960d1a5075aff962c39ecccc3c402da1b109c`, exactly one commit after r5's `75aa0f7159e4c9204f07ba4f169dcb520e796286`, on base `c19e441262ee2149dc788ba2ed664263ff6d97cb`. Read REVIEW-COMMON, the r5 report, DEC-92/106/110, the program context, and the updated m4 contract. Inspected the eleven-file delta, all explicit dwcli SPV-stop/shutdown callers, hook installation and chaining, and deferred reporting. Used detached `/work/scratch/e0-09-r6-sol/repo`. The implementation branch/worktree were untouched; no commits, pushes, GitHub comments, reviewer delegation, or Opus pass.

## Findings

### 1. P2 / major — a failed deadline-thread spawn can synchronously destroy the wedged runtime and hang before the deadline starts

**Locations:** `rust/bin/dwcli/src/teardown.rs:138`, `:159`–`164`; underlying fallback at `rust/crates/dw-engine/src/engine.rs:215`–`227`. CLI consequence: `rust/bin/dwcli/src/main.rs:522`, `:545`, `:560`.

`shut_down` moves its owning `Engine` into the closure passed to `within`. If the OS refuses to create that thread, `Builder::spawn` drops the closure on the caller. This invokes `Engine::drop`, which tries to create its own `dw-engine-drop` thread. Persistent thread exhaustion also refuses that spawn. Dropping this second closure drops its captured Tokio runtime synchronously; runtime destruction waits for the workers, which can be wedged indefinitely.

The caller never reaches `rx.recv_timeout`, the `None` handling, or `main`'s abandonment path. Consequently, the deadline does not bound this path, and main's passphrase wipe and terminal reporting are not reached. A stalled engine without a panic takes this route without a health probe, because the process poison flag is false. OS thread limits, a full PID cgroup, or thread-allocation failure can make the two spawn failures persist.

**Independent native reproduction on the unmodified production module:** a reviewer-only integration test imports the actual `dwcli/src/teardown.rs`. Create a real two-worker engine with MemoryOsStore and an open regtest session, park both workers without panicking, verify `engine_poisoned() == false`, and set the shutdown deadline to 1 s. Then install a seccomp filter on the test's caller thread only that makes `clone`/`clone3` return `EAGAIN`; existing engine threads are unaffected. A preliminary `Builder::spawn` confirms the expected OS error. Call the production `teardown::shut_down`.

Observed:

```text
EAGAIN confirmed; unpoisoned shutdown deadline=1s entering
shutdown still blocked after 20.054 s; no return or abandonment diagnostic
caller and both dw-engine workers: futex_do_wait
```

The external reviewer runner killed that isolated process. A second run under `strace` also remained blocked and was killed after 12 s. Its caller trace records three failed `clone3` calls: the preliminary check (2 MiB stack), the deadline thread (2 MiB), and the engine-drop fallback (8 MiB). This establishes actual OS errors and the fallback sequence, rather than a substituted engine error or a full stdout pipe.

**Suggested fix:** keep the engine in an ownership wrapper whose drop cannot destroy the runtime until the deadline worker actually starts. For example, capture `ManuallyDrop<Engine>` and call `ManuallyDrop::into_inner` inside the started worker. If thread creation fails, intentionally leave the engine behind and return abandonment to main. Preserve the current bounded behavior once a worker starts. Add an isolated regression for a stalled, unpoisoned engine with thread creation refused.

**Scratch counterproof:** applied only that ownership change in the detached copy. The same `EAGAIN` reproduction returned the abandonment reason in **48.573 µs**, and its process exited normally (**0.118 s** including setup). The ordinary stop/shutdown deadlines and hook-chain tests also passed. Restored the source byte-for-byte afterward. This counterproof is not an implementation committed to the reviewed branch.

### 2. Minor / P3 — the debug probe's synthetic panic remains in the release executable

**Location:** `rust/bin/dwcli/src/teardown.rs:94`–`95`.

The synthetic `if fail { panic!("injected poison") }` block moved into the general teardown module without a `#[cfg(debug_assertions)]` guard. Its payload survives in the release executable:

```text
rg -a -F -o 'injected poison' target/release/dwcli
injected poison
```

An independent byte scan agrees. Of 15 fault-variable, payload, marker, and helper-name needles, **14 are absent and `injected poison` is present**. The requested release-exclusion check therefore does not pass.

Normal release callers pass `false`, and all **16** independent comparisons of exit status/stdout/stderr against baseline (eight fault values, with and without SPV) pass. There is no evidence that an external fault environment variable activates this branch in release. The finding is the explicit binary-exclusion requirement, not a claim of a reachable production backdoor.

**Suggested fix:** compile the synthetic failure block only under `debug_assertions`, handling the unused release parameter without warnings, and repeat the release-binary scan. Keep the real health probe and its panic handling in release.

## Confirmations and accepted follow-up

- **R5 actual SPV-start panic: closed for the reported reproduction.** The committed native EventSink regression panics inside the real asynchronous startup task, wedges the workers, and passes its abandonment assertion. The process-wide hook captures the unawaited panic even though the command still receives the ordinary 60 s SPV-start timeout.
- **R5 actual SPV-stop panic: closed for the reported reproduction.** Its native regression passes. A stop's `TASK_PANICKED` conversion sets the flag, so main probes the engine and abandons it even when the command's earlier, non-panicked result is preserved.
- **Deadline behavior when the worker starts: pass.** Independent fresh-process tests measure a stalled, unpoisoned SPV stop plus skipped shutdown at **1.000225371 s**, and a stalled, unpoisoned shutdown after an earlier successful probe at **1.000162657 s**, with 1 s deadlines. This also exercises a stall after a successful probe. Finding 1 identifies the failure to start that worker.
- **Hook installation, previous host hook, and later chaining: pass.** The engine installs its hook before runtime creation. Dwcli installs its sanitized hook before `Engine::new`, so it becomes the previous hook. The committed unawaited-task test passes. An independent fresh-process test installs a host hook before the first engine, verifies a caller panic invokes it without poisoning the engine, installs a later hook that chains to the engine hook, and creates a second engine. Unawaited blocking-pool and async task panics invoke both hooks and poison the process; the second engine does not overwrite the later hook. Counts are **host = 3, later = 2**. The flag stays set after shutdown/drop. A poisoned but healthy engine stops and shuts down normally, measured at **7.002286 ms**. A later host replacement must chain as the API documentation states.
- **Removal of per-path handling: no additional lost behavior found.** Both explicit SPV stops (`dashpay::exec`, main's `sync`) and every explicit main shutdown use the shared teardown module. A poisoned session still records abandonment and moves its owed request/id before terminal output. `write_abandoned` and the ID ownership remain intact; their existing output-error and heap regressions pass. One-shot engine panics are covered by the hook/JoinError flag before teardown. Finding 1 prevents approval of the general deadline guarantee.
- **Independent binary behavior: pass in 11 cases.** Session/one-shot × with/without SPV × unpoisoned `stall`/`task-panic-wedge`: eight passing cases. Two additional stalled processes with stdout on `/dev/full` exit 1 after the wipe marker. A full unread pipe shows the wipe marker while the process remains blocked; draining it produces the unchanged successful session summary and exit 1. Panic cases preserve the public Unicode request id, print `internal` then `session_poisoned`, and run no second request. No `SessionClosed`, test passphrase, or panic-payload marker appears in those forced-exit outputs.
- **DEC-106 stdout limit: accepted, not a finding.** The wipe remains before terminal writes. This round checks that ordering and the debug marker; it does not repeat r5's allocation-byte observer. The same `Zeroizing<Vec<u8>>::zeroize` operation remains in main.
- **Successful command JSON with exit 1 after a shutdown deadline: accepted.** The empty successful session still reports `ok:true` while the process exits 1; the updated contract explicitly requires preserving the command's outcome.
- **Approximately 70 s startup-panic path: acceptable for E0-09 with an E0-05 follow-up.** It consists of the existing 60 s startup wait followed by the bounded 10 s health probe, and no subsequent shutdown wait when the probe fails. The real startup-panic regression passes its <80 s bound. E0-05's supervisor/state code should stop reporting `Starting` after the task dies and surface its failed completion promptly, including the typed panic classification. The delay and classification deserve that follow-up; they do not independently block E0-09 under DEC-110.

## Validation

Required gates ran in `rust/` on the unmodified head with **umask 022**, **`TMPDIR=/dev/shm/e0-09-r6-sol-tmp` (0700)**, `CARGO_BUILD_JOBS=12`, and `RUSTC_WRAPPER=sccache`:

- `cargo fmt --check`: **PASS**; also passes after restoring reviewer instrumentation.
- `cargo clippy --workspace --all-targets -- -D warnings`: **PASS**.
- `cargo test --workspace --no-fail-fast`: **PASS — 640 passed, 0 failed, 2 ignored**. The ignored tests require a public-testnet identity fixture and a real OS-keyring round trip.
- `cargo build --release -p dwcli`: **PASS** on unmodified production sources. Only an untracked reviewer integration test was added while this build was running; it is not a release build target.
- Reviewer `cargo test --workspace --no-run`: **PASS**. On the original module: three native confirmation tests pass and one isolated hang is reproduced. With the scratch ownership counterproof: **four native tests pass**.
- Original release executable: external fault environment values are inert in **16 comparisons**; the grep/byte exclusion check **FAILS** as finding 2 describes.

The VM was heavily CPU-contended. Binary whole-process measurements include vault and SPV startup: successful smoke reruns span **4.222–16.041 s**. An initial reviewer smoke assertion used an 8 s whole-process cap and rejected an SPV case at **8.013 s**; the cap was corrected to 25 s without changing any production source, the configured 2 s teardown deadlines, or the output/wipe assertions. Direct native teardown measurements above exclude setup and verify the deadline separately. These runs are failure-handling evidence, not performance benchmarks.

Unmodified binary SHA-256: debug **`43897c6e2a9a170086f9a56a2a9664ed8d468fa1923d689c51b2a5a8ef95094a`**; release **`e4cad01c3f4927073fb28a706b6835073d0680bdd2e0269079e99617c0c3c2ea`**. The scratch ownership counterproof is excluded from those hashes and the required gates.

Reviewer source edits and the added test were removed; tracked diff/status and restored formatting were clean, and no review binary process remained. The detached worktree, all `/work/scratch/e0-09-r6-sol/` artifacts, and `/dev/shm/e0-09-r6-sol-tmp` were removed and their absence verified. This report is not committed.

**Verdict: CHANGES-REQUIRED. Sol gate: NOT SATISFIED.**
