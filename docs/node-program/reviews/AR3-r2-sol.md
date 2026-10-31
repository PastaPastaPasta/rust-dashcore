# AR3 `rdash-cli` independent review, round 2 (Sol)

Scope: the round-1 fixes only, `b40f5ecdc..eb8ca49d6`, reviewed in a detached private worktree at `eb8ca49d6`. All eight commits in the range report `G` from `git log --format='%h %G?'`. The implementer's worktree was untouched. The reference was the cached Dash Core `v24.0.0-rc.3` `dash-cli`/`dashd`.

## Round-1 closure

| # | Disposition | Independent evidence |
|---|---|---|
| 1 | **Closed for valid options and post-command values; possible security note: see manager.** | `main.rs:14-31` tracks the first non-option; `cli.rs:76-100` prints position and a plain option name only. Nine raw-argv probes put a canary in `-rpcpassword`, positional, dash-led passphrase, and `-stdin*` values, before and after the command. All exited 1, wrote nothing to stdout, and omitted the canary. See N1 for a malformed pre-command value that can still appear as an option name. |
| 2 | **Closed.** | `cli.rs:748-754` sets exit failure when `getnewaddress` returns an error with code 0. My scripted 500 and 200 replies, each with and without a generate count, all sent exactly one `getnewaddress`, exited 1, wrote the RPC error to stderr and nothing to stdout. |
| 3 | **Closed structurally; runtime on macOS/Windows remains CI's job.** | `tests/xcli/main.rs:11-20,186-255` runs the portable offline/conversion subset on other Unix systems and advertises `xcli_full_parity_is_linux_only` as an ignored test with a reason. `tests/xcli_unsupported.rs:1-11` names an ignored test off Unix. The `node-app` CI group contains `rdash-cli` on Ubuntu, macOS and Windows and installs v24 dashd. `PORTING.md:163-169` and crate docs qualify full parity to Linux. This review host cannot run macOS or Windows. |
| 4 | **Closed for the round-1 failure-premise concern.** | Every nonzero row now requires a recorded stock failure text (`support/says.rs:16-34`); fake-server rows check request count, with explicit zero-request exceptions, and the HTTP failure rows require exactly one `getblockcount` (`tests/xcli/main.rs:53-95,509-703`). The comparison still checks stdout, stderr, exit and identical request bytes. The full matrices pass. Generic RPC error text is weaker than a method-specific oracle for live rows, but those rows are output-parity checks, not the fake-server HTTP failure rows in finding 4. |
| 5 | **Closed for the CLI's prompt paths; possible security note: see manager.** | In fresh pseudo-terminals my own SIGINT, SIGTERM, SIGHUP and SIGQUIT probes each saw signal exit and restored echo; parent-ignored SIGINT kept the process waiting with echo off. A prompt that started with echo already off restored that state. In a separate background-job probe, SIGTERM killed `rdash-cli` without SIGTTOU stops and left echo on; stock left echo off. `cli.rs:666-704` uses the two `NoEcho` scopes sequentially. See N2 for the public guard's nested-use limitation. |
| 6 | **Closed as a transport bound; possible security note: see manager.** | `http.rs:52-70,253-310,433-474,484-575` bounds a line to 64 KiB, header count to 1000, status/header bytes to 256 KiB and received body bytes to 1 GiB. My socket probes of overlong status/header/chunk-size lines, header count, aggregate headers and trailers all exited 1 with libevent-compatible error code 5. A 1 GiB + 1 byte `Content-Length` body did too. Overflowed hexadecimal chunk sizes with a short body ended in EOF, proving they did not wrap to zero; a negative size gave code 5. The matrix also covers chunked and until-close bodies above 1 GiB and just-inside limit replies. The JSON parser's memory amplification after transport is a documented remaining limit, not a claim of process-memory safety. |

## New findings

| ID | Severity and location | Probe, effect, suggested fix |
|---|---|---|
| N1 | **Low — `rdash-cli/src/cli.rs:85-96`** | A raw pre-command argument `-CANARY-private-passphrase=\xff` exits 1 but prints `(option -CANARY-private-passphrase)` on stderr. This is parsed as an option by position, so valid `-rpcpassword` values and passphrases after a command are protected. A misordered secret with a plain dash-led prefix can still enter logs. To make the error fully content-free, print only the argument position, or name only known, fixed option keys. Possible security note: see manager. |
| N2 | **Low — `rdash-cli/src/stdin.rs:138-155`** | A scratch Rust/pty probe created two `NoEcho` guards and dropped the first while the second remained live: `both_live_echo=false`, then `second_live_echo=true`. The first drop also removes the restore handlers. Current CLI scopes do not overlap, so the shipped prompts are unaffected; the public guard is unsafe for nested use. Count live guards and restore terminal state/handlers only when the final guard drops, or forbid overlapping guards explicitly. Possible security note: see manager. |

## Mutation checks

Each mutant was applied alone in the detached worktree. The unchanged `xcli_deviations` row builder ran; the original source was then restored, touched, byte-compared with the saved copy using `cmp`, and `git diff --exit-code` checked before the next mutant. The baseline integration test was rebuilt and passed after the final restoration.

| Mutation | Failed rows / 34 | Representative row |
|---|---:|---|
| Print the rejected argv debug value in the UTF-8 error | 9 | `-rpcpassword value, then -version` |
| Remove the failed `-generate` exit-code assignment | 2 | `-generate, getnewaddress error with code 0` |
| Raise the line limit from 64 to 65 KiB | 2 | `limit: header line of 64 KiB + 1` |

## Validation and limits

- `cargo test -p rdash-cli -p dash-args -- --test-threads=1` passed with the pinned v24 binaries: XCLI **995/995 offline**, **234/234 dashd**, **34/34 deviations**, zero timing retries. The relevant library tests also passed. The restored baseline `xcli_deviations` rerun passed 34/34.
- `cargo clippy -p rdash-cli -p dash-args --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check b40f5ecdc..eb8ca49d6` passed. The detached worktree was clean after mutations.
- Independently scripted, stock and Rust outputs matched byte for byte for 24,120,114-byte `getblock` verbosity-2 and 21,390,033-byte `getrawmempool true` JSON replies. These exercise large valid-looking envelopes, not a chain containing that many transactions. I did not measure a live dashd reply near 1 GiB.
- The manager's workspace-gate failure is in untouched `dash-p2p-transport::v1_only_dashd_makes_us_reconnect_over_v1`, timed out at 60 seconds under load and passed twice alone in 4–5 seconds. I agree this is a load flake unrelated to AR3; I did not repeat the whole workspace gate.
- The 1 GiB body cap limits transport buffering, not total process memory. `PORTING.md:110-129` correctly documents parser amplification (its 20 MB dense-array probe reached 1.3 GiB). A separate parser budget would be needed for a strict process-memory guarantee.

**Verdict:** APPROVE-WITH-NITS

Sol gate satisfied: yes
