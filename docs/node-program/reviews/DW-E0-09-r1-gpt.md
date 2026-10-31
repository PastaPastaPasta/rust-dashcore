# DW-E0-09 r1 — independent GPT review

Verdict: **CHANGES-REQUIRED**.

Reviewed `29bfceb` and `ae230eb596aa18d2c614cb256ccc7ff7e67a0037` against `7bf0bf3`, in a private detached worktree. Read m4 contract v4 §1/§6, the E0-09 ROADMAP row, and E0-04 P1 grant implementation/tests. The implementer's worktree was not edited. The accepted pay-contact stub, temporary explicit caps, and vault/facade error-code split are not objections.

## Findings, highest severity first

### 1. P1 — session secrets are not zeroized on parsing/refusal paths

**Locations:** `rust/bin/dwcli/src/dashpay.rs:968`–`981`, `:902`–`907`, `:985`–`989`.

The raw line is zeroized, and a successfully constructed `BearerSecret` is zeroized. However, parsing through `serde_json::Value` creates ordinary owned strings before converting to `Request`. If the request has the wrong shape, those strings are freed without wiping. `Request.args` and Clap's owned argument copies are also ordinary strings. JSON escapes additionally cause serde_json to allocate a decoding scratch buffer, which is not zeroized even on success. Wiping the line and final `BearerSecret` therefore does not wipe all copies created by dwcli.

**Concrete reproduction:** allocator probe `/work/scratch/e0-09-gpt/probe/src/main.rs`, using the same `Value` → `Request` parsing and the pinned zeroize 1.8.2, scans each allocation immediately before deallocation. Run:

```sh
cargo run --quiet --manifest-path /work/scratch/e0-09-gpt/probe/Cargo.toml
```

Inputs and observed allocations freed still containing `HEAP-SECRET-PROBE-ONLY`:

- Valid literal input: `{"args":["invite","stash"],"input":"dash:?invite=HEAP-SECRET-PROBE-ONLY","id":1}` → **0**.
- Valid escaped input: `{"args":["invite","stash"],"input":"dash:?invite=\u0048EAP-SECRET-PROBE-ONLY"}` → **1**.
- Invalid shape: `{"args":false,"input":"dash:?invite=HEAP-SECRET-PROBE-ONLY","id":2}` → **1**.
- Bearer mistakenly placed in args: `{"args":["invite","stash","dash:?invite=HEAP-SECRET-PROBE-ONLY"],"id":3}` → **1**, even before running Clap.

This probe isolates copies made by request parsing; it does not count OS/IPC buffers. It uses synthetic secrets only. Use a parsing approach that keeps decoded sensitive bytes and error-path allocations in zeroizing storage; account for escape-decoding scratch and rejected argument copies. Add an allocation/deallocation regression check, since assertions that output omits `SECRET` do not establish wiping.

### 2. P2 — an over-limit line ending exactly at the read boundary drops the next request

**Location:** `rust/bin/dwcli/src/dashpay.rs:939`–`940`.

The read takes 65,537 bytes. A line with 65,536 content bytes followed by LF consumes its delimiter and exceeds `MAX_LINE`; unconditional `skip_until(LF)` then consumes the following request as well. That request gets no response and is not counted. The same problem occurs with CRLF when the full line totals 65,537 bytes.

**Concrete reproduction:** feed `b'x' * 65536 + b'\n' + b'{"args":["name","check","alice"],"id":2}\n'` to `dwcli ... dashpay session`. Expected: oversized-line refusal, then response with `id:2`, then a final count of 2. Actual unmodified binary: one oversized-line JSON error and final `{"ok":true,"result":{"requests":1}}`; no response with `id:2`. The following request disappears. `/work/scratch/e0-09-gpt/framing-probe.rs` reproduces the exact read/discard loop; 65,535 and 65,537 content bytes preserve the next request, while 65,536 does not.

Only discard the remainder when the chunk did not already end in LF. Test both sides of the limit and the exact delimiter boundary, including CRLF.

### 3. P2 — the required `--no-platform` refusal is absent

**Locations:** `rust/bin/dwcli/src/main.rs:27`–`64`, `rust/bin/dwcli/src/dashpay.rs:818`–`831` and `:865`–`896`.

There is no `--no-platform` field, session option, or DashPay guard at this revision. Supplying the option is a generic Clap parse failure, not a DashPay JSON refusal with a clear code. No source occurrence of `no-platform` or `no_platform` exists in the Rust workspace. This fails the reviewer's explicit ruling.

**Concrete reproduction:** `dwcli --datadir /work/scratch/e0-09-gpt/data --dapi http://127.0.0.1:1 --no-platform dashpay --wallet <64 zeroes> status`. Expected: upfront structured disabled-Platform error. Actual: unsupported argument, exit 2, no JSON response. Add the switch and guard before opening/starting Platform or unlocking/authorizing a DashPay request.

### 4. P2 — no panic-to-JSON boundary for one-shot or session commands

**Locations:** `rust/bin/dwcli/src/main.rs:537`–`551`, `rust/bin/dwcli/src/dashpay.rs:945`, `:985`–`1008`.

`main` only handles `Result::Err`; `session_loop` directly calls `session_line`; neither catches unwinding panics or suppresses the default panic hook. A panic in a synchronous facade call or CLI dispatch bypasses JSON reporting and terminates the session. The default hook can print the panic payload to stderr. `Engine::block_on` does not catch panics; the engine's `on_runtime` conversion handles spawned engine tasks only, and does not cover synchronous facade calls or dwcli itself.

**Concrete reproduction:** controlled fault injection in the private scratch worktree: replace `StatusCmd::Status => out(dp.status()?)` with a block that calls `panic!("E0-09 synthetic panic")`. Rebuild with `cargo test --workspace --no-run` (same feature graph as the reviewed tests), then run a one-shot status and a session containing status followed by name check. **Observed:** both exit **101**, stdout is empty, stderr prints the synthetic panic payload and source location, and the session never responds to the next request. `/work/scratch/e0-09-gpt/panic-probe.py` performs this injection and restores the original source in a `finally` block; `.log` records the output. The required behavior is a sanitized JSON error (and a response for the next session request). This is a fault-injection check, not a claim that an unmodified current stub panics under ordinary input.

Add a sanitized unwind boundary around each session request and the one-shot setup/dispatch path, with a panic hook that does not leak payloads. Preserve normal grant/secret drops. Do not resume a state-mutating operation after an unknown outcome; report it accurately.

### 5. P2 — one-shot argument rejection echoes a bearer credential to stderr

**Location:** `rust/bin/dwcli/src/main.rs:537`.

The session parser deliberately avoids rendering Clap errors because they quote input; the one-shot parser still uses `Cli::parse()`, which prints the entire unexpected argument. An invitation accidentally supplied in the old/obvious positional form is copied into diagnostic output. The documentation allows exit 2 and usage text for parsing failures, but that does not require including the refused credential.

**Concrete reproduction on the unmodified binary:**

```sh
dwcli --datadir /work/scratch/e0-09-gpt/data --dapi http://127.0.0.1:1 invite stash 'dash:?invite=ARGV-SECRET'
```

Exit 2, empty stdout, stderr includes `error: unexpected argument 'dash:?invite=ARGV-SECRET' found`. The same applies to `contact scan <payload>`. This is already rejected as an input source, but secret hygiene still requires not repeating it into stderr/log collectors. Use a non-echoing Clap failure path for these commands, preserving the documented parse-failure exit policy if desired.

## Verification and review limits

- `TMPDIR=/dev/shm/e0-09-gpt CARGO_BUILD_JOBS=6 cargo test --workspace`: **PASS, 569 passed, 0 failed, 1 ignored**, after fixing the reviewer-created TMPDIR to mode 0700. The initial run correctly refused its mode 0775; this was a scratch-environment issue, not a code finding. Full successful log: `/work/scratch/e0-09-gpt/cargo-test-retry.log`.
- `TMPDIR=/dev/shm/e0-09-gpt CARGO_BUILD_JOBS=6 cargo clippy -p dwcli --all-targets -- -D warnings`: **PASS**. Log: `/work/scratch/e0-09-gpt/clippy.log`.
- `cargo fmt --all -- --check`: **PASS**.
- Binary probes: `/work/scratch/e0-09-gpt/binary-probes.py` and `.log`; allocator probe: `/work/scratch/e0-09-gpt/probe/`; panic fault injection: `/work/scratch/e0-09-gpt/panic-probe.py` and `.log`. The preserved unmodified binary is `/work/scratch/e0-09-gpt/dwcli-reviewed`.

The binary's normal stub errors give one JSON line and exit 1. Session errors give a response and continue, with the documented final summary and exit 0. Literal stdin/file/session bearer inputs accept 16,384 bytes and refuse 16,385 bytes before the facade. Length is checked before trimming, and JSON escapes are checked after decoding when the bearer is consumed. A bearer supplied as an extra session argument is rejected without echoing it, and the following request runs. Malformed JSON, invalid UTF-8, ordinary overlong lines, CRLF, empty lines (skipped), incomplete EOF (error), and complete EOF without LF (processed) recover correctly, except for finding 2's precise boundary.

Grant review: quote/grant-request caps are copied exactly into `GrantPurpose::PlatformOp`; each call authorizes afresh, scoped to `dp.wallet_id()`. No grant id or token is reused across requests. Discover uses `IdentityScan`. Caps default to zero and remain zero; `dw-vault/src/vault.rs:1806` explicitly refuses funding with `max_duffs == 0` and identity signing with `max_credits == 0`. The existing `platform_signer_scopes_are_capped_by_the_token` test exercises both zero cases and `{0,0}`. Budget consumption of implemented registration/top-up operations cannot be tested end to end yet because these facade bodies are stubs.

The coverage test really compares the table's command paths to Clap's subcommands for the seven listed groups, so an unlisted new leaf in an existing group fails it. It is not an exhaustive automatic discovery of DashPay groups: `GROUPS` is handwritten, and traversal is one level deep. A new top-level DashPay group can escape coverage if the author forgets to update `GROUPS`. The table checks the first stub reached, not the behavior after quotes succeed; its passing tests do not establish grant caps, panic behavior, or complete secret wiping.

Scratch probes, logs and the unmodified reviewed binary are kept under `/work/scratch/e0-09-gpt`; the detached worktree was removed after confirming its tracked files were restored. This report is not committed.
