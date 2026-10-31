# DW-E0-09 r3 — GPT-6.1-Sol confirmation review

Verdict: **CHANGES-REQUIRED**. The five round-2 findings are addressed under the now-explicit public-argument contract. A new P2 failure path loses the poisoned-session state on output errors and can enter the shutdown it was meant to avoid. **The Sol gate is not satisfied for the sequential Opus pass.**

Reviewed `56e639f6d25fd8b5ea882543806320fe49e5c8f7` on `dw/e0-09-dwcli-dashpay` against `f0bd5edd698266c3bc6958fbaf96ebc5a6e5df28`. Read REVIEW-COMMON first, both prior reviews, the complete nine-file fix diff, the m4 contract changes, and the relevant CLI, engine, vault and FFI/host error paths. Work was confined to a detached scratch worktree. No implementation-branch edits, commits, pushes or GitHub comments.

## Findings

### 1. P2 / major — output errors erase the failed-probe terminal state, allowing a shutdown hang

**Locations:** `rust/bin/dwcli/src/dashpay.rs:1095`, `:1101`, `:1107`, `:1116`, `:1117`; `rust/bin/dwcli/src/main.rs:519`–`526`.

`session_loop` learns that the engine failed its health probe before writing the request response. Every response write/flush uses `?`. If the writer fails, the function therefore returns `CliError { code: "io", panicked: false }`, including when writing or flushing the terminal `session_poisoned` envelope. `poisoned(&result)` then returns false. With `--spv`, `exec` can enter `stop_spv`; main enters `engine.shutdown()` instead of the bounded forced-exit path. A broken output pipe or failed log destination is enough to lose the terminal state.

**Native reproduction:** added a reviewer-only test to the scratch copy of `dashpay_heap_tests.rs`, using a real engine/session and `DWCLI_FAULT_INJECT=task-panic-poison`. Its writer permits either zero or one complete response line, then returns `BrokenPipe`. Both cases produce:

```text
fail_after=0: Err(CliError { code: "io", message: "io error: broken pipe", params: {}, panicked: false }), poisoned=false
fail_after=1: Err(CliError { code: "io", message: "io error: broken pipe", params: {}, panicked: false }), poisoned=false
```

The essential writer is:

```rust
struct FailWriter { lines: usize, fail_after: usize }
impl std::io::Write for FailWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.lines >= self.fail_after {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        self.lines += b.iter().filter(|c| **c == b'\n').count();
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}
// In a real session Ctx; run each fail_after in [0, 1].
let mut output = FailWriter { lines: 0, fail_after };
let request = b"{\"args\":[\"dashpay\",\"status\"],\"id\":1}\n";
let result = session_loop(&ctx, &mut &request[..], &mut output);
assert!(!poisoned(&result)); // Demonstrates the defect, not desired behavior.
```

Run the injected test with the same workspace feature graph:

```sh
umask 022
DWCLI_FAULT_INJECT=task-panic-poison TMPDIR=/dev/shm/e0-09-r3-sol-tmp \
  CARGO_BUILD_JOBS=12 RUSTC_WRAPPER= cargo test --workspace \
  reviewer_r3_poisoned_output_failure -- --nocapture --test-threads=1
```

The unmodified debug binary independently confirms unwanted teardown: redirecting stdout to `/dev/full` with this fault and `--verbose-events` emits `SessionClosed` after the failed probe, then exits 101 when subsequent reporting also panics on the failed output destination.

**Timeout consequence:** With `TOKIO_WORKER_THREADS=2`, I used reviewer-only fault injection to park both engine workers after the real injected task panic, while leaving the reviewed health/output/shutdown code unchanged. With a normal stdout pipe the process exits **1 in 10.113 s**, prints exactly the request error and `session_poisoned`, and never answers request 2. With the same stalled workers and stdout directed to `/dev/full`, it is **still alive at 15.066 s** and must be killed. It has taken the non-poisoned branch and is waiting for a shutdown task on the wedged runtime. The injected worker helper is shown below; call it twice after obtaining the panic result and before propagating that result with `?`.

```rust
// Reviewer-only method in impl NetworkSession; rt is its existing handle.
pub fn reviewer_wedge_worker(&self) {
    let (tx, rx) = std::sync::mpsc::channel();
    self.rt.spawn(async move {
        let _ = tx.send(());
        loop { std::thread::park(); }
    });
    rx.recv().unwrap();
}
```

Run the debug binary with `DWCLI_FAULT_INJECT=task-panic`, the two-worker setting, a request `{"args":["dashpay","status"],"id":1}` and a following name-check request. Compare captured stdout with `>/dev/full`; impose a parent-side timeout and kill only the experiment's process group. The parked-worker calls, rather than any new output/error logic, are the only injection needed for the hang reproduction.

This is controlled fault injection to test the promised failed-engine behavior, not a claim that an ordinary stub request wedges the runtime.

**Fix:** retain the failed-probe terminal state independently of response I/O. Once detected, attempt both envelopes best effort, skip SPV stop and engine shutdown even if a write/flush fails, and exit 1. Add a failing-writer regression for both envelopes/flushes and a genuinely stalled-probe/runtime test; a healthy runtime plus an artificial probe panic does not exercise this consequence.

### 2. P3 / minor — forced exit skips wiping the independently owned CLI passphrase

**Locations:** `rust/bin/dwcli/src/main.rs:337`–`341`, `:523`.

The passphrase is still an owned `Option<Zeroizing<Vec<u8>>>` in main's `run` when `std::process::exit(1)` executes. That call skips Rust destructors. A reviewer-only `atexit` observer of that allocation, registered immediately before the actual exit, confirms **the synthetic `HEAP-SECRET-PROBE-ONLY` passphrase is still present**. The observer prints only a boolean; it introduces no secret copy. It runs after the terminal stdout envelope, proving the zeroizing destructor was bypassed.

The request, decoded input, string id, raw line and unread `ZeroStdin` buffers have already dropped on the return from `dashpay::run`; this observation concerns the outer passphrase. It does not establish post-exit disclosure: the OS reclaims the address space. Forcing termination without taking possibly wedged engine/vault locks is a reasonable fail-stop tradeoff, so this is not another P1/P2 objection.

**Fix:** explicitly drop/wipe the CLI-owned passphrase before forced exit. That buffer can be cleared without acquiring engine/vault locks. Keep the bounded exit; do not restore unbounded shutdown to obtain cleanup.

## Round-2 confirmation

1. **P1, pre-clap credential copies — fixed within the public-argument contract.** The actual session parser and grammar are byte-identical to the copies in the independent probe. Native allocation scans find **0 unwiped frees** in all 14 prior-round parser/refusal cases: literal, slash/Unicode escaped, case/whitespace, percent-encoded, split and bare surplus credentials; malformed string id; refused shape; truncated escape; unknown key; duplicate input; invalid UTF-8; depth limit. The caller's source buffer is excluded. The native reviewer allocator starts allocations zero-filled, avoiding uninitialized spare-capacity reads or residue from earlier allocations.

   I also deliberately passed credential-shaped strings through every free-text category and the URL/path slots. These are **not universally zeroized**: the independent probe's 11 admitted public-value cases each have 3 unwiped frees; six actual `session_line` cases also each have 3. Examples include `--note dashpay%3A%2F%2Finvite%3Fpk%3DHEAP-SECRET-PROBE-ONLY`, `--avatar-url https://example.org/?p%6b=HEAP-SECRET-PROBE-ONLY`, and `--avatar-file /HEAP-SECRET-PROBE-ONLY.png`.

   **I accept the explicit contract as the resolution permitted in r2.** Credentials belong only in `input`; free text is explicitly enumerated/capped, and URL/path/id values are also explicitly outside the bearer-zeroization guarantee. The pattern check is only a diagnostic. The grammar removes the demonstrated accidental surplus/unknown-token copies before clap; it cannot make arbitrary accepted public text or an id-shaped key secret. Any claim that *all credential-shaped values in all args* are wiped would still be false. The implementer's zero-residue statement must remain scoped to protected input and grammar-refused argument cases.

2. **Grammar completeness — verified.** The recursive clap walk reaches **56 distinct leaves including `dashpay session`** and **158 value slots** including inherited wallet options. The advertised **65** is the existing table's scenario-row count, which includes command variants, rather than 65 distinct leaves. The passing `every_value_has_a_type` test recursively checks the entire RequestArgs/DashPayCommand tree; the passing table test supplies valid values and checks all 65 scenarios against the grammar and clap. No arbitrary-string value slot outside the explicit free-text, URL/path and typed-id categories was found. The enum names are checked against facade serde names, and both cursor types are distinguished correctly.

3. **P2, string id copy — fixed.** Bad-escape and refused-shape paths leave zero scanned residue. `Id::Str` stays zeroizing through validation/drop; output streams it from that buffer as the last answer key. The committed whole-loop heap check also passes with a string id and a sink writer. Echoed ids remain caller-public correlation data.

4. **P2, engine-task panic sanitization/classification — fixed.** A real `NetworkSession::run_on_engine` panic now converts to `Internal(TASK_PANICKED)` without its payload, becomes a panicked CLI error, and gets unknown-outcome wording and the health probe. The binary tests cover one-shot, continued session, and failed-probe session outcomes. Direct CLI unwinds remain sanitized.

   **FFI compatibility:** `rust/crates/dw-ffi/src/api/error.rs:62` and the domain conversions still map the same enum variants/codes. `docs/contracts/m1-engine.md:35` and `Sources/DashKit/DashKitError.swift:4` specify diagnostic detail rather than stable detail text; host adapters select behavior/copy by code. No host parser or exact-detail dependency was found. Withholding a task panic's message does not break that contract. Swift/macOS execution was not available on this Linux reviewer.

5. **P2, fail-closed health reads — fixed for observable errors/panics.** A real closed session now yields `wallet_infos` error and `engine_healthy == false`. Session-open, vault status, wallet-list result and runtime task are checked. The genuinely stalled-runtime probe also fails at its 10 s deadline; with working output, the last stdout line is `session_poisoned`, followed by exit 1, no later request, no final summary and no engine shutdown. The only extra stderr line in that controlled run is the reviewer-injected exit observer described above. The remaining problem is retaining that terminal decision through output failure (finding 1), not an ignored health read.

6. **P3, RFC 8259 integers — fixed.** Both binaries refuse `01` and `-01` as malformed JSON, omit the id, and do not dispatch those requests. The parser enforces the integer part's zero/nonzero rule; committed tests cover valid zero/negative zero and ordinary integers.

7. **Prior-round behavior remains intact.** Debug/release framing checks cover 65,534–65,539 and 131,074 content bytes with LF, CRLF and EOF, preserving the following request and count. The upfront `--no-platform` refusal still wins over invalid data/passphrase paths for all seven groups and session. Refused one-shot arguments omit synthetic credentials from usage/diagnostics. The release binary contains none of the eight fault needles checked (`DWCLI_FAULT_INJECT`, the three injected-fault/poison message fragments, `task-panic-poison`, `task-panic`, `panic-poison`, `PANIC-SECRET`). All four fault values leave its response equal to the release baseline.

## Validation and limits

The desktop gate and native build/test commands ran under `rust/` with **umask 022**, `TMPDIR=/dev/shm/e0-09-r3-sol-tmp` (**0700**), `CARGO_BUILD_JOBS=12`, and `RUSTC_WRAPPER=`:

- `cargo fmt --check`: **PASS**.
- `cargo clippy --workspace --all-targets -- -D warnings`: **PASS**.
- `cargo test --workspace --no-fail-fast`: **PASS — 593 passed, 0 failed, 1 ignored**, on unmodified head before reviewer test injection.
- `cargo build --release -p dwcli`: **PASS** after the cached-archive retry, on restored unmodified head.
- Binary probes: **48 debug cases**, **43 release functional cases**, and **5 release baseline/fault-ignored cases**, all passing their assertions; the separate final scan confirms all **8 fault needles absent**.
- Independent parser/grammar/clap heap probe plus native heap and failing-writer probes: **PASS**. Failing-writer assertions deliberately confirm the reported defect; they are not a clean-behavior test.
- Timeout probes: **PASS** — normal output exits in 10.113 s; failed output demonstrates the hang and was killed at 15.066 s. Both experiment process groups are gone.
- Reviewed binary SHA-256: debug `67fce2e84ae6209f07228e96ebfd75bc42ca54558e10b089dfe7ad93f3f2b050`; release `5e1c365991310ddb1891822f2766ec39a28341e6227b64efd43ae815d1304b2f`.
- Final restored-tree formatting/status: **PASS** — reviewer source changes restored byte-for-byte to head, tracked diff/status clean, and final `cargo fmt --check` passes before the release retry.

The first release attempt failed because DNS lookup for the Tenderdash source archive failed. The retry uses the already-built debug profile's identical cached `tenderdash-v1.8.0.zip` (SHA-256 `39489a7459121cff12dd4487fe0fa42b88bf607bda3a64c778bbc5a27f524c50`); no source or dependency revision was changed.

Scans measure bytes at deallocation, not practical remote heap disclosure. OS/IPC buffers and future non-stub facade parsers are outside this proof. All markers are synthetic. I did not independently rerun the implementer's seven planted regressions against f0bd5ed; r2's original native defect proofs and this round's head checks establish the before/after mechanisms.

Reviewer edits were restored, the detached worktree and all this round’s scratch artifacts were removed, and `/dev/shm/e0-09-r3-sol-tmp` was deleted. Reproduction fragments and observed results are recorded above because those transient artifacts are no longer retained. This report is not committed. No Opus pass was started. **Sol gate: NOT SATISFIED; verdict: CHANGES-REQUIRED.**
