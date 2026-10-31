# DW-E0-09 r4 — GPT-6.1-Sol confirmation review

Verdict: **CHANGES-REQUIRED**. The original r3 failing-writer defect and skipped passphrase wipe are fixed on the exercised paths. Two **P2 / major** termination gaps remain: an unlock task panic bypasses the abandonment check, and a full stdout pipe can block a process after its health probe has failed. **Sol gate: NOT SATISFIED for the sequential Opus-high pass.** No P1 finding.

Reviewed `5e95fb01a17b5894fb387ceac9ccc9492bf5444c` on `dw/e0-09-dwcli-dashpay`, based on `c19e441262ee2149dc788ba2ed664263ff6d97cb`. Commits: `92c42a3`, `1cb5741`, `64b6c90`, `f62015d`, `5e95fb0`. Read REVIEW-COMMON and the r3 report first. Inspected the complete r3 fix, the rebase range-diff, CLI execution/reporting, engine task/error/shutdown paths, and relevant E0-05 changes. Builds and experiments used detached `/work/scratch/e0-09-r4-sol/repo`; the implementation worktree was untouched. No commits, pushes, GitHub comments, or additional reviewer delegation.

## Findings

### 1. P2 / major — the initial unlock can return a task panic straight into wedged shutdown

**Locations:** `rust/bin/dwcli/src/dashpay.rs:864`, `:872`–`875`; `rust/bin/dwcli/src/main.rs:521`, `:534`–`536`.

`crate::unlock(...)?` returns before the context and post-command `abandon_if_unhealthy` check exist. An unlock `vault_op` task panic is correctly converted into a `CliError` with `panicked: true`, but `ABANDONED` remains false. Its code is `internal`, not `session_poisoned`, so `abandon_engine` is also false. Main therefore calls `engine.shutdown()` without probing this engine. Both session startup and one-shot commands use this entry point.

**Independent native reproduction:** added only a test to the scratch copy's existing `dashpay::tests` module. It uses a real two-worker engine, an encrypted vault with TEST KDF/MemoryOsStore, and an injected `EventSink`. Create the vault, lock it, and arm the sink immediately before calling `super::run` with a passphrase and parsed `dashpay status`. On the unlock's `VaultLockState` callback, the sink calls the new debug `wedge_engine` helper and then panics. This produces a real `vault_op` JoinError; no CLI error handling, probe, or shutdown code is changed.

Observed:

```text
result = Err(CliError { code: "internal", ..., panicked: true })
elapsed = 1.004431489 s
abandon_engine(&result) = false
engine_healthy(&ctx) = false   # actual runtime probe, 10 s deadline
engine.block_on(engine.shutdown()) still blocked after another 15 s
```

The essential injected sink behavior is:

```rust
fn emit(&self, event: EngineEvent) {
    if matches!(event, EngineEvent::VaultLockState { .. })
        && self.armed.swap(false, Ordering::SeqCst)
    {
        let s = self.session.lock().unwrap().as_ref().unwrap().clone();
        wedge_engine(&s);
        panic!("reviewer synthetic unlock callback panic");
    }
}
```

The test asserts the real panic classification and false abandonment decision, independently runs the actual health probe, then moves the engine to a thread running the exact shutdown call main selects and requires a 15 s receive timeout. It passed as a **defect confirmation**, not an acceptance test. The stalled thread ends with the isolated test process.

There is a related classification hole in the new `--spv` bridge: `pay.rs:175` converts a `start_spv` `EngineError` to `String`; `dashpay.rs:1115`–`1116` wraps that string in `EngineError::Spv`. A `TASK_PANICKED` error consequently loses `is_task_panic` and cannot trigger the post-command health check. This additional route was verified by source inspection; the native wedged-shutdown proof above exercises unlock, not an injected SPV-start panic.

**Suggested fix:** create the context before unlock and route unlock failures through the same panic-aware result/probe boundary as command failures. Preserve the typed engine panic classification through SPV setup as well. Add an early-unlock task-panic regression that fails its actual health probe and verifies abandonment instead of shutdown.

### 2. P2 / major — terminal writes still block indefinitely when stdout's reader stops

**Locations:** `rust/bin/dwcli/src/dashpay.rs:850`–`853`, `:1187`–`1199`, `:1217`–`1223`; `rust/bin/dwcli/src/main.rs:526`–`532`.

Ignoring write/flush errors does not bound a blocking write. In a poisoned session, `answer` writes the request envelope before returning the terminal result to main. A one-shot command reaches `dashpay::report` in the abandonment branch, which also writes synchronously before the passphrase wipe and `exit(1)`. Either path can remain stuck on a pipe whose reader is alive but no longer consuming data.

**Unmodified-binary reproduction:** initialize an encrypted test vault and wallet; run with `TOKIO_WORKER_THREADS=2`, `DWCLI_FAULT_INJECT=task-panic-wedge`, the passphrase file, and `--verbose-events`. Create a pipe, set its capacity to 4096 bytes, fill it, and retain its read end without reading. Give its write end to the child's stdout. For a session, send the status request with id 1 followed by the name-check request with id 2; also run one-shot `dashpay status`.

```python
read_fd, write_fd = os.pipe()
size = fcntl.fcntl(write_fd, fcntl.F_SETPIPE_SZ, 4096)
os.write(write_fd, b"x" * size)
child = subprocess.Popen(args, env=fault_env, stdin=subprocess.PIPE,
                         stdout=write_fd, stderr=subprocess.PIPE,
                         start_new_session=True)
os.close(write_fd)
# Keep read_fd open and unread; communicate with an 18 s parent deadline.
```

Both processes were still alive and had to be killed: **session 18.025 s; one-shot 18.030 s**. Immediately before killing, `/proc/<pid>/wchan` was **`anon_pipe_write`** for each main thread. Stderr contained the sanitized task-panic location, but neither the wipe marker nor `SessionClosed`. The same commands with captured stdout or `/dev/full` exit normally in approximately 12–13 s without `--spv`. Thus the new hang is terminal output after the failed probe, not the health-probe wait or engine shutdown.

**Suggested fix:** make output on the known-unhealthy terminal path bounded/nonblocking so it cannot prevent forced exit, and wipe the CLI-owned passphrase before potentially blocking terminal reporting. Add session and one-shot regressions with a saturated pipe and a stopped reader, alongside the immediate write/flush-error tests.

## r3 confirmation and rebase sanity

- **Original r3 P2, write/flush errors: closed for those errors.** `a_failed_probe_survives_output_errors` passes for FailWriter at 0 and 1 lines and both flush failures. The poisoned return survives output failure, and request 2 is not dispatched. The committed `dashpay_r3` binary tests also pass.
- **Stalled runtime with writable/failed stdout: passes.** Independently ran all eight combinations of session/one-shot, `--spv` present/absent, and captured stdout/`/dev/full`. Every process exits **1 in 12.327–15.887 s**, reports the passphrase wiped, and emits no `SessionClosed`. Captured session output is exactly the id-1 `internal` error and `session_poisoned`; one-shot output is exactly one `internal` error. No id-2 answer or session summary follows. All four `--spv` runs report SPV `Running` before the injected command panic, confirming the new wait is used.
- **Original r3 P3, wipe before forced exit: closed when exit is reached.** In four independent observer cases (session/one-shot × captured stdout/`/dev/full`), a reviewer-only `atexit` callback reads the original, still-allocated passphrase byte span after the explicit `zeroize()` and prints only a boolean. All four report **every original byte zero**, exit 1, and have the `wiped` marker. The observer registers the allocation immediately before the actual wipe and changes no termination logic. The blocked-output finding above prevents reaching that wipe.
- **Marker safety: passes.** `report_wiped` selects only fixed `absent`/`wiped`/`held` strings using presence and emptiness; it formats no passphrase bytes, hash, or length. A no-passphrase poisoned session reports `absent`. Synthetic passphrase and panic-payload markers appear in neither stream during the independent binary probes.
- **Release exclusion: passes.** Built unmodified head with `cargo build --release -p dwcli`. Actual binary grep and a separate byte scan find **all 13 needles absent**: the fault variable, five fault/payload fragments, injected-fault strings, and forced-exit/passphrase marker strings. All five fault values, including `task-panic-wedge`, leave the release status/stdout/stderr identical to its baseline. There are **39 passing release checks** total: baseline, five fault-inertness checks, eight up-front `--no-platform` refusals, 21 framing cases, and four `/dev/full` output paths.
- **Rebase content: passes the narrow sanity check.** `git range-diff 7bf0bf3..56e639f c19e441..f62015d` preserves the four prior patches; only the r1 patch's incorporation of the already-existing `no_platform` field/help/test fixture differs. `dashpay.rs`, grammar, parser, and prior regression tests are unchanged by the rebase itself. The E0-05 engine bring-up/runtime/restore implementation and tests are unchanged relative to `c19e441`; the branch's engine delta is its existing task-panic sanitization/export/test. Reviewed the other base additions and caller changes, including local backup scopes, restore signals, FFI notice filtering, regtest `--no-platform`, and the default-off shielded feature. This is not a new full review of E0-05.
- **Ordinary SPV timeout: passes.** `ensure_spv_running` is byte-identical to E0-05's helper; DashPay supplies 60 s. An independent real-engine test with a blackholed local DAPI, a created wallet, and a 100 ms supplied timeout returns `SPV did not start in 100ms` in **100.703 ms**, while the state remains `Starting`; engine shutdown then completes within 5 s and closes the session. The timeout test changes no implementation deadline or polling logic. The stringified panic classification noted in finding 1 is separate from this passing ordinary timeout path.

## Validation and cleanup

All gate commands ran in `rust/`, with **umask 022**, `TMPDIR=/dev/shm/e0-09-r4-sol-tmp` (**0700**), `CARGO_BUILD_JOBS=12`, and `RUSTC_WRAPPER=`:

- `cargo fmt --check`: **PASS** on unmodified head and again after restoring reviewer source edits.
- `cargo clippy --workspace --all-targets -- -D warnings`: **PASS**.
- `cargo test --workspace --no-fail-fast`: **PASS — 631 passed, 0 failed, 2 ignored**. Ignored: public-testnet identity fixture and OS-keyring round trip.
- `cargo build --release -p dwcli`: **PASS** on unmodified head. The first attempt stalled in shared sccache and only that build was cancelled; the successful retry disabled the wrapper and changed no source/dependency revision.
- `cargo test --workspace reviewer_r4_ -- --nocapture --test-threads=1`: **2 passed**: ordinary SPV timeout acceptance and early-unlock **defect confirmation**. A test-helper name collision in the initial reviewer probe was corrected to call `super::run` before this successful run; it was not a head failure.
- Instrumented observer binary built with `cargo test --workspace --no-run`: **PASS**; four allocation observer cases: **PASS**. The initial observer build was switched from the normal build graph to the existing workspace-test graph to reuse its artifacts.
- Unmodified binary SHA-256: debug **`a2c594f7c3605208d04eceb202ed8c6ee61c7584058a861576aa6c7d68c67e77`**; release **`faa6b1525778a273b75273ef6991ed4c03a53adf883d15bc7e246d9b2d7575e8`**. Observer instrumentation is excluded from these hashes.

Fault injection demonstrates the promised failure-handling behavior; it does not claim an ordinary stub command naturally wedges every worker. No real credentials, public-testnet fixture, real keyring operation, or existing dashd instance was used. The allocation observer proves the CLI-owned passphrase span was wiped when that exit path completed, not that all engine/OS buffers were scrubbed.

Reviewer source edits were restored byte-for-byte, tracked diff/status were clean, and no review CLI/test process remained before cleanup. The detached worktree, all `/work/scratch/e0-09-r4-sol/` artifacts, and `/dev/shm/e0-09-r4-sol-tmp` were removed. This report is not committed. No Opus pass was started.

**Verdict: CHANGES-REQUIRED. Sol gate: NOT SATISFIED.**
