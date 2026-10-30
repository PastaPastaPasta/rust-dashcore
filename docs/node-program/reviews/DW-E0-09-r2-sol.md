# DW-E0-09 r2 — GPT-6.1-Sol confirmation review

Verdict: **CHANGES-REQUIRED**. The framing, upfront Platform refusal and sanitized clap diagnostics are addressed. Secret parsing and panic handling have remaining gaps described below. **The Sol gate is not satisfied; do not start the sequential Opus pass yet.**

Reviewed `f0bd5edd698266c3bc6958fbaf96ebc5a6e5df28` (`dw/e0-09-dwcli-dashpay`) against round-1 head `ae230eb596aa18d2c614cb256ccc7ff7e67a0037`, using the requested detached worktree. Read the round-1 report, the complete six-file fix diff and the relevant CLI, engine and vault paths. No implementation changes or commits are part of this review.

## Findings and reproductions

### 1. P1 — pattern-based `args` rejection still lets refused credentials reach ordinary clap allocations

Locations: `rust/bin/dwcli/src/dashpay/request.rs:389` and `rust/bin/dwcli/src/dashpay.rs:1081`–`1087`.

The new zeroizing argument storage is correct until `RequestArgs::try_parse_from` copies the arguments. The guard checks seven ASCII substrings independently in each argument. JSON slash/Unicode escapes are decoded before checking, and case variants and leading whitespace do not bypass it. Percent encoding and splitting across arguments do bypass it:

```json
{"args":["invite","stash","dashpay%3A%2F%2Finvite%3Fpk%3DHEAP-SECRET-PROBE-ONLY"]}
{"args":["invite","stash","dashpay","://","invite?pk","=HEAP-SECRET-PROBE-ONLY"]}
{"args":["invite","stash","HEAP-SECRET-PROBE-ONLY"]}
```

These remain command errors; the issue is credential bytes in allocations freed unwiped, not acceptance by the invitation facade. The original caller buffer is excluded from the scan. The standalone allocator probe (using the exact parser and pinned clap 4.6.1) finds **2**, **1**, and **2** unwiped frees, respectively. The unmodified release CLI replies `bad arguments: unexpected argument found`, showing these values passed the pre-clap guard; the next request still runs, and neither output stream quotes the marker. The native probe calling the actual `session_line` independently confirms the same **2 / 1 / 2** unwiped frees. Setting `DWCLI_REVIEW_REQUIRE_CLEAN=1` makes its zero-residue assertion fail on the URL-encoded case (exit 101); log: `/work/scratch/e0-09-r2-sol/native-cleanliness-assertion.log`.

The pattern list is useful detection, **not a sound secret-storage boundary**. Do not fix this by accumulating more denylist patterns. Validate the allowed command/option grammar and each public value before giving anything to a copying parser; reject surplus tokens while still in zeroizing storage. A positive allowlist must include position/type validation, not just allowed flag names. Free-form labels/notes require an explicit public-data contract or a parsing path that keeps their copies zeroizing; a character allowlist cannot distinguish a Base58 private key from arbitrary permitted text.

### 2. P2 — string `id` creates an unwiped copy even when parsing subsequently fails

Location: `rust/bin/dwcli/src/dashpay/request.rs:166`.

`self.string()?.to_string()` copies the zeroizing result into an ordinary `Value::String`. In this malformed request, that copy is freed when the later invalid escape aborts parsing:

```json
{"args":[],"id":"dash:?invite=HEAP-SECRET-PROBE-ONLY","input":"\q"}
```

Both the standalone exact-parser probe and the native `session_line` probe report **one unwiped free**, despite returning a sanitized syntax error and no id. A refused shape (`{"args":false,"id":"dash:?invite=HEAP-SECRET-PROBE-ONLY"}`) also leaves one unwiped free when the parsed result is dropped. String ids are intentionally echoed for well-formed requests and should be treated as public by callers; this finding concerns the claimed all-string zeroizing parser and the avoidable parse-error allocation, not the intentional echo contract. Keep the decoded id in zeroizing storage through validation/error cleanup rather than copying it during parsing.

### 3. P2 — engine-task panics bypass sanitization and the post-panic health probe

Locations: `rust/crates/dw-engine/src/error.rs:266`–`270`; `rust/bin/dwcli/src/dashpay.rs:570`–`572`, `:710`–`716`, `:1034`.

The new hook correctly withholds a panic payload, but the engine's existing `JoinError` conversion formats that same payload into `EngineError::Internal`. `CliError::from` preserves it in `message`. This is a returned error, so the CLI unwind boundary never sees it; `is_panic()` also returns false because the message does not start with `dwcli panicked`.

The native probe, with `quiet_panics()` installed, calls the real public `NetworkSession::run_on_engine` with a panicking future, converts its returned error through the actual `CliError` implementation and renders the actual envelope:

```rust
let error = engine.block_on(session.run_on_engine::<(), _>(async {
    panic!("synthetic engine-task panic carrying dashpay://invite?pk=PANIC-SECRET")
})).unwrap_err();
let cli_error: CliError = error.into();
// cli_error.is_panic() == false
let answer = envelope(&Err(cli_error));
```

Observed JSON `message`:

```text
internal error: engine task panicked: task 15 panicked with message "synthetic engine-task panic carrying dashpay://invite?pk=PANIC-SECRET"
```

The hook's stderr line contains only the location, while JSON contains the credential-shaped panic payload. The false panic classification also means a session skips the post-panic health probe for this path. This is a controlled engine-task failure test; I do not claim an ordinary current stub input triggers it.

Sanitize the panic when converting a `JoinError`, preserve a reliable panic classification through the engine/CLI error path, and report the unknown outcome and run the session health check for converted task panics as well as directly caught unwinds. The debug fault currently tests only the direct CLI unwind and an artificial probe unwind, so it does not cover this path.

### 4. P2 — the session health probe reports a failing engine/session as healthy

Location: `rust/bin/dwcli/src/dashpay.rs:1054`–`1062`.

The closure discards `wallet_infos()`'s `Result`, and `.is_ok()` checks only whether the closure unwound. A normal `Err` is therefore success. The native test opens a real regtest engine/session, closes it via `Engine::close_network`, then observes `wallet_infos() == Err(NetworkNotOpen("regtest"))` while `engine_healthy() == true`. With the existing debug status-panic hook enabled, `session_loop` on that closed session answers the panic, executes the next request and returns `Ok({"requests":2})` instead of `session_poisoned`. This exercises the real helper and loop, not an equivalent mock.

The stated inability to detect non-panicking corruption is reasonable as a bounded guarantee; **ignoring an explicit health-probe error is not that limitation**. Inspect the probe result and session liveness as well as catching an unwind. The comment that a poisoned lock necessarily panics on next use is also inaccurate here: the vault and session recover poisoned locks with `into_inner()`.

### 5. P3 — the replacement JSON parser accepts invalid leading-zero numbers

Location: `rust/bin/dwcli/src/dashpay/request.rs:306`–`317`.

```json
{"args":["name","check","alice"],"id":01}
{"args":["name","check","alice"],"id":-01}
```

`serde_json` rejects both as malformed JSON; the new parser accepts them and executes the request. The unmodified release binary returns `platform.not_implemented{call:"check_username"}` with ids `1` and `-1`, proving dispatch rather than syntax rejection. Enforce JSON's integer grammar (`0` or a nonzero digit followed by digits, with optional minus). This is a protocol/parser regression, not a demonstrated authorization bypass, and is lower priority than the secret and panic gaps.

## Round-1 item status

- **1. Zeroization — PARTIALLY FIXED; P1 remains.** The raw stdin/line storage and decoded input, argument and key buffers wipe correctly on the tested ordinary parse/refusal paths. Native and independent allocation probes find zero residue for literal/JSON-escaped input, unknown keys, duplicates, depth-limit rejection, invalid UTF-8 and truncated escapes. `ZeroStdin` also drops unread bearer bytes with zero residue. Encoded/split surplus arguments reach clap and free credential bytes unwiped (finding 1), and string ids still introduce an ordinary copy (finding 2). The new string reader's preallocation is sufficient: decoded JSON strings are never longer than their raw representation, so its decoding buffer does not need to reallocate.
- **2. Framing — FIXED.** Unmodified debug and release binaries pass 21 content-length/ending cases each: 65,534–65,539 bytes and 131,074 bytes, with LF, CRLF or EOF. Valid padded requests at exactly 65,536 content bytes dispatch, over-limit content is refused, the following request is always answered and counted, and LF/CRLF do not consume content budget. This covers the old exact-delimiter loss and both sides of the new read boundary.
- **3. `--no-platform` — FIXED.** Both binaries return one `platform.feature_off` envelope with `params:{"feature":"platform"}` and exit 1 for all seven DashPay command groups plus session mode. Probes use a data directory under a blocking regular file, a nonexistent passphrase file and no DAPI endpoint; the feature refusal wins before setup can open/read them. The workspace regression also confirms a non-DashPay command still runs.
- **4. Panics — PARTIALLY FIXED.** One-shot direct panic: exactly one sanitized `internal` JSON line, unknown-outcome wording, exit 1. Session direct panic: response retains the id, the next request runs, final count is 2. Artificial `panic-poison`: request response then `session_poisoned`, no next request, exit 1. Hook output contains location only and withholds `PANIC-SECRET`. **Release check passes:** the executable contains none of `DWCLI_FAULT_INJECT`, `injected fault carrying` or `injected poison`; setting either fault value has exactly the baseline output. Converted engine-task panics and explicit health-read errors remain uncovered by the fix (findings 3–4).
- **5. Clap echo — FIXED.** The committed 18-position bearer test passes in the full workspace suite. Five additional one-shot rejection cases in each binary (including a secret-shaped unknown flag name and value-parser errors) exit 2 with no stdout JSON, show usage and omit the synthetic credential. Session refusals likewise do not echo it. Help rendering remains covered by the committed test.

## Implementer limits and review assumptions

- Pattern-based `args` detection: **not accepted as the boundary**; finding 1 demonstrates unwiped credential bytes on current refusal paths.
- No detection of non-panicking corruption: reasonable limitation if the probe checks observable failures; finding 4 is an explicit failed read that is discarded.
- OS pipe buffers are outside the heap scanner: accepted and consistent with the existing IPC/JSON transport residual risk. This does not exempt dwcli's own decoded or clap-owned copies.
- Future `BearerSecret::expose()` copies: accepted as a future implementation obligation. Current facade methods remain stubs, so these tests cannot prove later parsers/signers will wipe all copies.

All probe secrets are synthetic. Allocation scans establish residue at deallocation, not practical remote heap disclosure. A local memory/core-dump observer or later process memory disclosure is the confidentiality model, as in round 1. Panic and closed-session probes are controlled failure-state tests, not claims of an ordinary current input causing the stubs to panic.

## Validation and artifacts

- `TMPDIR=/dev/shm/e0-09-r2-sol CARGO_BUILD_JOBS=6 RUSTC_WRAPPER= cargo test --workspace`: **PASS — 579 passed, 0 failed, 1 ignored**, before reviewer test injection. TMPDIR is mode **0700**. Log: `/work/scratch/e0-09-r2-sol/cargo-test.log`.
- `TMPDIR=/dev/shm/e0-09-r2-sol CARGO_BUILD_JOBS=6 RUSTC_WRAPPER= cargo build --release -p dwcli`: **PASS**. Log: `/work/scratch/e0-09-r2-sol/release-build.log`.
- `cargo fmt --all -- --check`: **PASS**, including after restoring reviewer source changes. Logs: `fmt.log`, `fmt-final.log` under the scratch root.
- Binary probes: **46 debug cases** and **43 release functional cases** passed their assertions; the separate release fault-absence/baseline checks also passed. `debug-binary-probes.log`, `release-functional-probes.log`, `release-probes.log` record every response. Some assertions deliberately confirm the newly reported defects; “probe pass” is not approval.
- Independent exact-parser/clap allocator probe: `/work/scratch/e0-09-r2-sol/probe/`, output in `parser-probe.log`; unread stdin drop probe in `stdin-probe.log`. The copied parser's SHA-256 matches the reviewed source: `5afe2600914b434449de6d9e9d83082664906290da7852d0f2e67da57bf54296`.
- Native failure proof: `native-reviewer-probe.log` (**1 test passed, asserting the reported defects exist**); the zero-residue assertion run in `native-cleanliness-assertion.log` **fails as expected, exit 101**, at the encoded-argument residue. The unmodified full workspace suite above has no failures. The reviewer allocator starts allocations zero-filled, avoiding dependence on preexisting allocator contents when scanning spare capacity. Scans are armed only on the calling thread, and exclude the caller's source buffers and OS pipe buffers.
- Initial test/release/probe launches stalled in the shared sccache queue. Only this review's process groups were stopped; retries disable the compiler wrapper with `RUSTC_WRAPPER=`. The first package-only native-probe launch was also stopped because it changed the workspace feature graph; the successful native proof uses `--workspace`. These were build-environment/probe-launch adjustments, not implementation test failures.

Reproduce the independent proof without recreating the worktree:

```sh
TMPDIR=/dev/shm/e0-09-r2-sol CARGO_BUILD_JOBS=6 RUSTC_WRAPPER= \
  cargo run --manifest-path /work/scratch/e0-09-r2-sol/probe/Cargo.toml
```

Reproduce the native proof (the worktree must first be recreated at the reviewed head):

```sh
git -C ~/workspace/dashwallet-desktop worktree add --detach \
  /work/scratch/e0-09-r2-sol/repo f0bd5ed
python3 /work/scratch/e0-09-r2-sol/inject-reviewer-tests.py
cd /work/scratch/e0-09-r2-sol/repo/rust
DWCLI_FAULT_INJECT=panic TMPDIR=/dev/shm/e0-09-r2-sol \
  CARGO_BUILD_JOBS=6 RUSTC_WRAPPER= cargo test --workspace \
  reviewer_r2_actual_heap_and_health_probe -- --nocapture --test-threads=1
# Add DWCLI_REVIEW_REQUIRE_CLEAN=1 to require zero residue and reproduce the failure.
```

The probe's added Rust source is preserved as `reviewer-tests-added.rs`; the original test file is `dashpay_heap_tests.original.rs`. Reviewed unmodified binaries are preserved as `dwcli-debug-reviewed` and `dwcli-release-reviewed`, with hashes in `binary-sha256.log`. All of those artifacts are under `/work/scratch/e0-09-r2-sol`.

The reviewer test file was restored byte-for-byte, the final tracked diff/status were clean, and the detached worktree was removed after preserving the artifacts. This report is not committed. No Opus pass was started.
