# DW-E0-09 r5 — Sol confirmation review

Verdict: **CHANGES-REQUIRED**. The r4 unlock reproduction, direct typed SPV-start-error bridge, and DEC-106 passphrase-before-output requirement pass. Two **P2 / major** paths still take an unhealthy engine into shutdown: a panic inside the actual asynchronous SPV startup task, and a task panic returned by SPV teardown. **Sol gate: NOT SATISFIED for the sequential Opus-high pass.** No P1 finding.

Reviewed `75aa0f7159e4c9204f07ba4f169dcb520e796286`, exactly one commit after r4's `5e95fb01a17b5894fb387ceac9ccc9492bf5444c`, on base `c19e441262ee2149dc788ba2ed664263ff6d97cb`. Read REVIEW-COMMON, the r4 report, DEC-92, DEC-106, and the program context. Inspected the five-file change, deferred reporting and id ownership, SPV helpers and callers, and engine startup/stop/shutdown paths. Builds and experiments used detached `/work/scratch/e0-09-r5-sol/repo`; the implementation worktree and branch were untouched. No commits, pushes, GitHub comments, or reviewer delegation.

## Findings

### 1. P2 / major — a panic in the actual SPV startup task becomes an ordinary timeout, then reaches wedged shutdown

**Locations:** `rust/bin/dwcli/src/pay.rs:185`–`195`; `rust/bin/dwcli/src/dashpay.rs:1158`–`1159`, `:889`; `rust/bin/dwcli/src/main.rs:536`. Underlying task/state: `rust/crates/dw-engine/src/platform/bringup.rs:137`–`146`, `rust/crates/dw-engine/src/platform/runtime.rs:266`–`270`, `rust/crates/dw-engine/src/session.rs:563`.

The new helper preserves a typed panic returned by `start_spv()`, but that call normally returns after scheduling a separate `supervise` task. A panic in that actual startup task does not become the helper's outer `EngineError`. The stored supervisor remains `running: false`, and `spv_state()` reports `Starting` without inspecting the task's failed completion. The helper eventually returns its ordinary inner timeout string; DashPay converts it to `EngineError::Spv`, yielding a non-panicked CLI error. Its post-result check therefore never runs a health probe or sets abandonment. Main selects `engine.shutdown()`.

**Independent native reproduction:** added only reviewer tests in the detached scratch copy. Use a real two-worker engine with MemoryOsStore, an open regtest session, local refusing DAPI/SPV endpoints, and no wallet. Its EventSink calls the existing debug `wedge_engine` helper and panics on `SpvStateChanged { running: true }`, inside the real startup task. Run the production `dashpay::run` with parsed `dashpay status --spv`. No new `spv-task-panic` hook, substituted engine error, shortened implementation deadline, or modified production control flow is used.

Observed:

```text
result = Err(CliError {
    code: "spv",
    message: "spv error: SPV did not start in 60s",
    params: {},
    panicked: false
})
run elapsed = 60.062399008 s
startup callback fired = true
abandon_engine(&result) = false
production engine_healthy(&ctx) = false
engine.block_on(engine.shutdown()) still blocked after another 15 s
```

The test requires an actual receive timeout, rather than accepting a crashed/disconnected shutdown thread. It passes as a **defect confirmation**.

The committed SPV-start regression and new binary fault cases pass because their hook runs an unrelated panicking engine task **before** `start_spv()` at `pay.rs:183`–`184`. They do not exercise the later startup task. The engine supervisor implementation already exists in the base; this is a remaining route through E0-09's promised setup-panic boundary, uncovered by the requested shutdown hunt.

**Suggested fix:** surface a startup supervisor's panic as the typed `TASK_PANICKED` error and route it through the existing probe/abandon boundary. At minimum, an SPV setup failure with a failed health probe must abandon the engine before shutdown. Add a regression that panics in the actual asynchronous startup task and verifies abandonment.

### 2. P2 / major — an SPV-stop task panic is discarded as a warning, so shutdown still runs on the wedged engine

**Locations:** `rust/bin/dwcli/src/dashpay.rs:1170`–`1179`, `:882`–`889`; `rust/bin/dwcli/src/main.rs:536`. Task: `rust/crates/dw-engine/src/platform/bringup.rs:168`–`178`, `rust/crates/dw-engine/src/session.rs:573`.

The health decision in `exec` examines the command result **before** stopping SPV. If `stop_spv()` then returns `Internal(TASK_PANICKED)`, the code logs a warning and returns the earlier result unchanged. For a successful result or ordinary command error, `run` consequently sees no panic and sets no abandonment flag. Main invokes shutdown. Preserving a command's outcome is appropriate; losing the independent unsafe-teardown decision is the defect.

**Independent native reproduction:** the same real-engine sink setup, armed instead for `SpvStateChanged { running: false }`. Let SPV start normally, then run `dashpay status --spv` with no wallet. The normal command result is `wallet_not_found`; the actual SPV-stop task wedges the workers and panics. Its real JoinError crosses the engine's existing typed conversion.

Observed:

```text
warning: stopping SPV: internal error: engine task panicked (message withheld)
result = Err(CliError {
    code: "wallet_not_found",
    message: "wallet not found: no wallet on this network",
    params: {},
    panicked: false
})
run elapsed = 1.118634068 s
stop callback fired = true
abandon_engine(&result) = false
production engine_healthy(&ctx) = false
engine.block_on(engine.shutdown()) still blocked after another 15 s
```

This test also passes as a **defect confirmation**. The common outer `--spv` teardown serves both one-shot and session execution; the native reproduction exercises one-shot execution. Stdout is writable throughout both native findings, so neither is the full-pipe limit accepted in DEC-106.

**Suggested fix:** inspect a returned SPV-stop task panic with the same bounded health probe and record abandonment independently of the original command result. Preserve the original outcome envelope, wipe the passphrase, and skip engine shutdown if that probe fails. Add a real SPV-stop panic regression.

## R4 and DEC-106 confirmation

- **Unlock task panic: closed for the r4 reproduction.** The context exists before unlock, its typed panic becomes a panicked CLI error, and the failed-probe decision reaches main. The committed real WedgeOnUnlock/EventSink reproduction passes on this head.
- **Direct SPV error bridge: fixed.** The helper's outer engine error reaches `CliError::from` intact; the new direct SPV-start hook is correctly classified and abandoned. The committed unit regression and independent binary cases pass. Finding 1 identifies a different task whose failure never reaches that outer result.
- **R4 stdout/error reproductions: pass under DEC-106.** Independently ran 18 unmodified-binary cases: session/one-shot × with/without `--spv` × captured stdout, `/dev/full`, and a full unread pipe, plus session/one-shot × those three outputs for the new SPV-start task-panic hook. All writable/error-output processes exit 1 without `SessionClosed`; session output contains exactly its first request's `internal` answer with its public Unicode string id and then `session_poisoned`, with no second request or summary. Setup panics produce one id-less `internal` envelope. Six full-pipe cases show the wipe marker while alive in `anon_pipe_write`, then emit the correct envelopes and exit 1 after draining. Observed exits/wipe markers span 13.726–20.653 s.
- **Actual passphrase bytes before blocked output: pass.** A reviewer-only observer captures the original allocation pointer and length before the existing wipe, then reads that still-allocated span immediately afterward, before marker/envelope output. It copies no passphrase and prints only a boolean. All six blocked-output cases report **every original byte zero**, remain in `anon_pipe_write`, and exit 1 after draining. This covers session/one-shot with and without SPV, plus SPV-start failures in both session and one-shot mode. The accepted blocking itself is not a finding.
- **OWED/id/input ownership: pass within the contract.** Only a panicked error enters OWED; that error is sanitized, and neither the request input nor passphrase is stored. The existing zeroizing string Id is moved without cloning. `write_abandoned` takes the owed value, releases the mutex before writing, streams the id from its buffer, and drops it even after write failure. A native allocation scan over the poisoned session and deferred report finds **0 unwiped id/input-marker frees** for both a sink writer and a BrokenPipe writer, and OWED is empty afterward. The reviewer allocator zero-initializes allocations to avoid reading uninitialized spare bytes. The echoed id remains public correlation data under m4's existing contract; input, passphrase, and synthetic panic-payload markers appear in neither stream in the independent binary cases.
- **Legacy ensure_spv_running behavior: preserved for ordinary pay/tools/sync operation.** Comparing with `5e95fb0`, the wrapper applies the same engine-error Display strings, and the delegated loop retains the same start check, timer placement, Running/Stopped handling, 100 ms polling, and timeout text. Callers and supplied durations are unchanged. A real-engine/local-blackhole regression returns exactly `SPV did not start in 100ms` in **101.343955 ms**, shuts down normally, and verifies the closed-session wrapper error equals the typed error's Display. Release status tests with SPV also pass.
- **Release fault exclusion: pass.** Built unmodified head with `cargo build --release -p dwcli`. Actual binary `rg -a -F -o` and an independent byte scan find all **16** fault variable/value, payload/marker, and hook-name needles absent, including both new SPV fault values and `inject_spv_start_fault`. All seven fault values leave exit status/stdout/stderr identical to baseline, with and without SPV: **14 passing comparisons**.

## Validation and cleanup

Required commands ran in `rust/` with **umask 022**, `TMPDIR=/dev/shm/e0-09-r5-sol-tmp` (**0700**), and `CARGO_BUILD_JOBS=12`:

- `cargo fmt --check`: **PASS** on unmodified head, and again after restoring reviewer instrumentation.
- `cargo clippy --workspace --all-targets -- -D warnings`: **PASS**.
- `cargo test --workspace --no-fail-fast`: **PASS — 634 passed, 0 failed, 2 ignored**. Ignored: public-testnet identity fixture and real OS-keyring round trip. Includes all three new committed regressions.
- `cargo build --release -p dwcli`: **PASS** on unmodified head.
- `cargo test --workspace reviewer_r5_ -- --nocapture --test-threads=1`: **4 passed**: two defect confirmations, deferred id/input cleanup, and ordinary wrapper timeout/closed-error behavior.
- Reviewer observer `cargo test --workspace --no-run`: **PASS**; six independent blocked-output byte-observer cases: **PASS**.

The first clippy compilation was cancelled by the reviewer to enable the functioning shared sccache; no head failure or source/dependency change prompted that restart. Successful build/test commands used `RUSTC_WRAPPER=sccache`. The initial formatting command used an empty wrapper.

Unmodified binary SHA-256: debug **`dd243ecf54acf2307122da3c49316e8115c9415e492f103c73013ef7a4177084`**; release **`db27961d3eabe167f2aabd4c758886b0f63f80836b8edace49f13770041377c5`**. Reviewer observer instrumentation is excluded from these hashes.

Fault injection establishes the failure-handling mechanisms, not that ordinary stub commands naturally wedge the runtime. The passphrase observer covers the CLI-owned allocation, not all engine/OS buffers. No real credentials, public-testnet fixture, real keyring operation, existing dashd instance, or management-network service was used.

Reviewer source edits were restored byte-for-byte; tracked diff/status and restored formatting were clean, and no review CLI/test process remained. The detached worktree, all `/work/scratch/e0-09-r5-sol/` artifacts, and `/dev/shm/e0-09-r5-sol-tmp` were removed. This report is not committed. No Opus pass was started.

**Verdict: CHANGES-REQUIRED. Sol gate: NOT SATISFIED.**

