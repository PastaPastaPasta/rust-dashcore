# DW-E0-09 r7 — Sol narrow confirmation review

Verdict: **CHANGES-REQUIRED**. The original r6 **P2 ownership fix is verified in the production teardown paths**, and the original **P3 release-exclusion finding is closed**. Two minor, in-scope issues prevent the requested unconditional confirmation: the new debug hook has a combination that still drops the engine on the caller after teardown spawn failure, and the rebased command-table assertions weaken coverage. No new release blocker or major finding. **Sol gate satisfied: no**.

Reviewed branch `dw/e0-09-dwcli-dashpay`, exact head `0e529c594ba7341341b2f5c58df595ee1bb6373f`, in `/work/home/agent/workspace/dw-wt/e0-09-dwcli-dashpay`; base `de1c3737a2fff22a48c4c7185c3f20de977d17cb`. Read r6 first, REVIEW-COMMON, DEC-106/118 and the relevant DEC-110 context. Inspected the fix commit and the rebased CLI tests. Did not reopen the settled SPV-start/stop, framing, secret-buffer, or hook-chain reviews. Used the code-review-validator workflow. No branch edits, commits, pushes, GitHub comments, or delegation.

## Findings

### R7-1 — P3 / minor: the new debug stall/spawn-failure combination bypasses DEC-118 when SPV is enabled

**Locations:** `rust/bin/dwcli/src/dashpay.rs:876`, `:1049`; `rust/bin/dwcli/src/teardown.rs:199`; caller ownership/unwind at `rust/bin/dwcli/src/main.rs:521`.

With `DWCLI_FAULT_INJECT=stall-spawn-fail` and `--spv`, `exec` first reaches `stop_spv`. The refused-spawn hook sets Linux `RLIMIT_NPROC=0`, records abandonment, and returns the command's result. `dashpay::run` nevertheless calls its post-command `wedge_engine`. Its ordinary `std::thread::spawn` now fails and panics before main can call `teardown::shut_down`. The owning engine is still an ordinary local in main's `run`; unwinding drops it on the caller. Its fallback thread also fails, and the runtime is destroyed on that same caller. The `ManuallyDrop` guard is never reached.

**Independent reproduction on the unmodified debug binary:** encrypted vault plus wallet, two engine workers, `--shutdown-timeout 120`, fault `stall-spawn-fail`, then `dashpay status --spv` and `dashpay session --spv`. The latter received one status request with public id `r7-spv-stall`. Both exit 1 through the outer panic handler, with no forced-wipe marker or explicit abandonment diagnostic. One-shot output is `internal`; the session preserves its already-written request answer but replaces its expected `{"ok":true,"result":{"requests":1}}` summary with `internal` / “the command's outcome is unknown”.

A session run under `strace` confirms the ownership path on **one caller TID, 2509676**:

```text
prlimit64(0, RLIMIT_NPROC, {rlim_cur=0, rlim_max=0}, NULL) = 0
clone3(... stack_size=0x1ffcc0 ...) = -1 EAGAIN
clone3(... stack_size=0x7ffcc0 ...) = -1 EAGAIN
```

These are the wedge helper's approximately 2 MiB stack and Engine::drop's approximately 8 MiB fallback. The sanitized panic and subsequent Tokio timer panics also appear on stderr. Runs returned in **25.114 s** and **30.696 s**, including setup; neither hung. The missing marker alone does **not** establish unwiped allocation bytes: ordinary `Zeroizing` drop can wipe the passphrase during this unwind. The confirmed failure is taking caller-thread runtime destruction instead of the explicit DEC-106/118 abandonment boundary, and losing the command/session terminal outcome. A stalled runtime on that route has the original unbounded-drop problem. This combination is debug-only; release fault variables are inert as confirmed below.

**Suggested fix:** skip the post-command stall injection when teardown has already recorded abandonment, so main reaches `shut_down`'s forget path and `abandon`. Extend `dashpay_r6` with the one-shot and session `--spv` combinations, asserting the wipe marker, explicit abandonment, preserved terminal JSON and no panic/SessionClosed. This needs no additional teardown mechanism. DEC-118 and this review's all-path request have no stated debug-hook exception, so I cannot confirm the unconditional caller-drop guarantee on this head.

### R7-2 — P3 / minor: the rebase removes precise outcome coverage for five command rows

**Location:** `rust/bin/dwcli/src/dashpay.rs:2370`.

The new `IMPLEMENTED` branch accepts every result except an error whose code is `platform.not_implemented`. An unrelated error, a wrong success result, or null JSON therefore passes for `identity list`, `identity show` without an identity, `identity set-main I`, `identity discover`, and `dashpay events` without an identity. The command-enumeration, grammar and secret-output assertions remain; the result assertion is materially weaker.

**Automated counterexample:** copied the CLI sources to reviewer scratch, changed only `IdentityCmd::List` from `out(dp.identities()?)` to `Err(CliError::invalid("review-only wrong command result"))`, and compiled the actual unit tests against the normal CLI's matching dependency artifacts. The unmodified `every_command_reports_its_stub_as_a_json_error` test still **passes** with this broken routing. The implementation worktree was untouched.

**Suggested fix:** give these five rows their actual deterministic expectations in the existing fixture: list succeeds with `[]`; show, set-main and events fail with `identity.not_found`; discover fails with `platform.unavailable`. Independently checked all five outcomes through the unmodified binary. The separate discovery end-to-end change to `platform.unavailable` is justified and keeps a specific expectation; it is not this finding. The DP1-05 engine tests cover the real bodies but do not replace CLI routing/result assertions.

## Confirmations

- **Original P2 / production teardown ownership: verified.** `shut_down` captures the entire `ManuallyDrop<Engine>`; only a started worker calls `into_inner`. Actual spawn failure drops the inert wrapper. Failed health-probe and SPV-stop spawns capture session Arcs, retain the owning engine outside those closures, and lead to `must_abandon` plus `mem::forget`. Both main shutdown callers then reach the explicit passphrase zeroize before terminal writes. A worker that starts and subsequently stalls or blocks while dropping remains inside the caller's timed wait. R7-1 identifies a debug path before that ownership handoff.
- **Native OS refusal, without injection: pass.** A standalone reviewer program imports the unchanged production `teardown.rs`, opens a real two-worker engine with MemoryOsStore, parks both workers, and installs a caller-only seccomp filter returning EAGAIN for clone/clone3. It first verifies an actual refused thread spawn. Shutdown returns abandonment in **63.074 µs**; SPV stop followed by shutdown in **90.588 µs**; poisoned-engine health-probe failure followed by shutdown in **60.409 µs**. All have a 120 s deadline and zero SessionClosed events. Each case runs in its own process.
- **Eight independent binary cases: pass.** One-shot/session × SPV/no-SPV × ordinary `unused-spawn-fail`/poisoned `task-panic-spawn-fail`. All exit 1 within 40 s, wipe the main passphrase, print explicit abandonment, preserve JSON and public ids, emit no SessionClosed, and disclose neither the test passphrase nor PANIC-SECRET. Whole-process times **7.168–27.963 s**. The committed no-SPV stalled-engine `dashpay_r6` regression also passes. R7-1 is an additional combination those tests omit.
- **Signals: pass for the failure boundary.** SIGINT and SIGTERM each terminate a poisoned session while its large owed public id blocks in a deliberately undrained 4 KiB stdout pipe. The wipe marker is observed before either signal; no SessionClosed or secret output. The CLI installs no SIGINT/SIGTERM cleanup handler: these signals use OS termination, without Rust engine destructors. This is not a claim of an explicit application wipe if a fatal signal arrives before abandonment; DEC-106's normal write-blocking limit is unchanged.
- **Original P3 / actual release exclusion: closed.** `cargo build --release -p dwcli` passes. Byte scan finds **all 15 fault-variable/payload/marker/helper needles absent**, including `injected poison`, `DWCLI_FAULT_INJECT`, `PANIC-SECRET`, and `-spawn-fail`. Eight fault environment values, with and without SPV, match baseline exit status/stdout/stderr in **16 comparisons**. These release comparisons do not supply a passphrase; debug secret handling is checked separately above. The real health probe remains compiled in release.
- **R5 timing adjustment:** the stalled-command whole-process cap changes only from 8 to 25 s. Its configured teardown deadline remains 2 s; deadline-message, wipe, exit, JSON, and no-SessionClosed assertions remain. That edit is reasonable under load and is not a coverage finding.

## Validation and limits

Used **umask 022**, **TMPDIR=/dev/shm/e0-09-r7** (0700), `CARGO_BUILD_JOBS=12`, and `RUSTC_WRAPPER=sccache` for Cargo gates. No shared CARGO_TARGET_DIR.

- `cargo fmt --check`: **PASS**.
- `cargo clippy --workspace --all-targets -- -D warnings`: **PASS**.
- `cargo build --release -p dwcli`: **PASS**.
- `cargo test --workspace --no-fail-fast`: **652 passed, 3 failed, 3 ignored**, exit 101. The committed r6 test passes. Failures are E0-05's unchanged `a_held_key_read_does_not_hold_spv_past_the_budget` final UncleanShutdown assertion, and the r5 tests' elapsed-time assertions (healthy **43.133 s** against 10 s; stalled **36.664 s** against 25 s).
- The E0-05 case also fails alone at ordinary scheduling priority, then **passes** with `nice -10`. Both unmodified r5 tests **pass** with that priority (20.80 s for the suite). These reruns use the exact already-built workspace test executables under the agent user. A redundant subset Cargo rebuild was stopped and replaced with those executables. VM load averages were approximately **150–210 on 32 vCPUs**. This supports scheduling sensitivity; it does not turn the original workspace run into a clean 655/0/3 gate. No test limits or production source were changed to obtain the reruns.
- Reviewer native proof: **3/3 pass**; regular binary spawn-failure cases: **8/8 pass**; release environment comparisons: **16/16 pass**; fatal-signal cases: **2/2 pass**. The combined debug-hook failures and wrong-routing mutation are the negative proofs described above.

Unmodified binary SHA-256: debug **`ca1680f2f00a67715a59a64417e09deb03aa42be2f6718f884362f317615912d`**; release **`8a1d9503ca1018dbb7a2bb3a0a9e03a93390c98c1c18b97ad1f683e6df799747`**. Reviewer mutation artifacts are excluded from these hashes and Cargo gates. This round verifies explicit zeroize ordering and marker behavior, not another allocation-byte observer; r5's settled buffer-erasure review is unchanged.

Cleanup verified: no reviewer proof executable remains running; `/work/scratch/e0-09-r7-sol` and `/dev/shm/e0-09-r7` were removed and their absence checked. The reviewed worktree still has the exact requested head and clean Git status. Only this uncommitted review report is retained.

**Verdict: CHANGES-REQUIRED. Sol gate satisfied: no.**
