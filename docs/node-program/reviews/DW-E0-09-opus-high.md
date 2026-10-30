# DW-E0-09 — Opus-high (+SEC) review

Verdict: **APPROVE-WITH-CHANGES.** One required change (F1, P3): the healthy path writes its last JSON line while dwcli still holds the passphrase unwiped. DEC-106 requires the wipe to come first. The rest is sound: secrets and bearer inputs, grants, the abandonment boundary, the facade-only boundary, the JSON error contract and the release exclusion. Both r7 items (R7-1, R7-2) are fixed. I accept the implementer's open item (F2) as is. No P1 or P2 finding.

Reviewed branch `dw/e0-09-dwcli-dashpay` at **e37741c0bbdb630dcb5be5bce2b627ff80743dea** in `~/workspace/dw-wt/e0-09-dwcli-dashpay`, the whole diff against main `de1c3737` (9 commits, 20 files, +5984/−42). I read the r1–r7 reports, DEC-106/110/118/133 and the m4 contract §1 secrets and dwcli sections. I made no branch edits, commits or GitHub posts. The only file kept is this report.

## Findings

### F1 — P3 / minor (SEC; DEC-106): the healthy path holds the passphrase through its last, possibly blocking write

**Location:** `rust/bin/dwcli/src/main.rs:523` (`Ok(done) => dashpay::report(result, …)`), with `passphrase` declared at `:350` and still alive until `run` returns.

DEC-106: "zeroize the passphrase *before* the terminal envelope writes, so a blocked write never holds an unwiped secret." The abandon path does this (`abandon` → `zeroize` → `report_abandoned`). The healthy path does not. After a successful `teardown::shut_down`, `dashpay::report` writes the one-shot line, or a session's `{"requests":N}` summary, while the `Zeroizing` passphrase in `run` is still unwiped. If the caller stops draining stdout (the accepted DEC-106 limit), the process blocks there indefinitely with the passphrase in memory. The engine is already shut down at that point, so dwcli's own buffer is the only copy left.

**Reproduction (A/B, unmodified binaries).** Setup: an encrypted vault plus wallet, passphrase `OPUSREVIEW-PASSPHRASE-7f3c91`, and `TOKIO_WORKER_THREADS=2`. Stdout is a pipe set to 4096 bytes (`F_SETPIPE_SZ`) and pre-filled, with its read end held open and never read. Run `dashpay status`, wait until `/proc/<pid>/wchan` is `anon_pipe_write`, then `sudo gcore` the process and count the passphrase bytes in the core. Script: `/dev/shm/e009-opus/blocked_core.py` (removed after the review; its logic is described here).

| Binary / path | Blocked in pipe write | Passphrase copies in core |
|---|---|---|
| release, healthy (`dashpay status`) | yes, after 4.7 s | **1** |
| debug, healthy (`dashpay status`) | yes, after 2.7 s | **1** |
| debug, abandon (`DWCLI_FAULT_INJECT=task-panic-poison`) | yes, after 3.2 s; stderr shows `forced exit, passphrase wiped` | **0** |

The abandon run still has a live, unlocked vault and shows no copy. So the healthy-path copy is dwcli's own `passphrase` buffer, not vault state.

**Required fix:** wipe before the line in the DashPay arm. Dropping the `Zeroizing` is enough:

```rust
Ok(done) => {
    // DEC-106: wiped before the last line, whose write may block.
    drop(passphrase);
    dashpay::report(result, done.map_err(|e| format!("shutdown: {e}")))
}
```

Add a regression. The simplest one reuses the existing debug-only marker (`report_wiped`): print it on the healthy path too whenever `DWCLI_FAULT_INJECT` is set to a value that injects nothing, and assert in a binary test that it comes before the JSON line. The gcore check needs sudo, so it can't run in CI. Why only P3: the copy lives only in memory while the caller deliberately stops reading, and it has no output channel. It is still a direct departure from a binding ruling, and the fix is two lines.

### F2 — P4 / accepted: the open item, a caller-thread panic before `shut_down`

**Location:** `main.rs:519–526` (the DashPay arm) and `main.rs:350/355` (declaration order).

**Judgment: acceptable; it does not block.** For this path to hang, three things must happen together:

1. **A dwcli panic on the caller thread** between `Engine::new` and `teardown::shut_down`, outside a session. I found no reachable one. The four `expect`s in the one-shot dispatch (`faucet_proof_file`, `to`, `contact`, `answer`'s object suffix) are guaranteed by clap `requires`, `required_unless_present` or the envelope's shape. `exec`'s SPV-stop warning is now a best-effort `writeln!` (r7).
2. **The OS refusing `Engine::drop`'s fallback thread.** DEC-118 already classifies OS thread exhaustion as accepted resource exhaustion.
3. **A wedged runtime**, for the drop to actually block.

One detail to record: locals in `run` drop in reverse order, so in this triple-fault case `engine` drops (and hangs) **before** `passphrase` is wiped. The hang would also hold the passphrase. That is no worse than the engine's own unlocked key material, which the hang keeps anyway.

**Optional hardening.** This is not a new teardown mechanism, so DEC-118's "no further mechanisms" doesn't apply. Wrap the `dashpay::run(...)` call in the DashPay arm in `catch_unwind` and map a panic to `Err(panic_error("dwcli", "the command"))`, as `session_line` already does per request. A dwcli panic then always reaches `shut_down`, with its `ManuallyDrop` handoff and its deadline. A dwcli-side panic doesn't set `engine_poisoned`, so the shutdown runs normally under the deadline. Leave it to the implementer.

Also noted, outside E0-09's scope: the pre-existing early `?` returns in `run`'s non-DashPay arms (for example `unlock_if_needed(...)?` in `Create`, `Import`, `Tools`, `Pay` and `Compat`) drop the engine on the caller without a shutdown. That is the same class as this item, and it predates the branch.

### F3 — P4 / observation: an engine-task panic mapped to an ordinary error mid-session is not probed until the session ends

**Location:** `dashpay.rs:1225–1226`.

A session probes the engine only after a request whose error is a panic (`CliError::is_panic`). Suppose an engine task panics and engine code maps the panic to some other error, or the panic happens in an unawaited background task. `engine_poisoned()` is then set, but the request reads as an ordinary error, which a caller might retry, and later requests run without a probe. The teardown check (DEC-110) still catches it when the session ends.

This matches the contract as written: an engine-task panic is defined by `TASK_PANICKED`. I found no facade path today that swallows one: facade spawns go through `NetworkSession::on_runtime`, and `JoinError` becomes `TASK_PANICKED`. **Suggestion, not required:** snapshot `engine_poisoned()` before each request. If it flips during the request, mark the result as outcome unknown (`panicked = true`) and run the probe. One-shot commands need nothing, because `shut_down` already checks the flag.

### F4 — P4 / pre-existing: a passphrase file that is a pipe

**Location:** `main.rs:208–213` (`read_passphrase`, not changed by this branch).

`std::fs::read` sizes its buffer from the file length. A regular file gets an exact allocation and leaves no stray copy. `--passphrase-file /dev/stdin` or `<(…)` (a pipe, length 0) reads first through std's 32-byte stack probe, and longer inputs then grow the `Vec`. Either way an unwiped copy is left on the stack or in freed heap. This is outside E0-09; a follow-up could read into a capped `Zeroizing` buffer the way `read_secret` does.

## Confirmations by focus area

**1. Secrets and bearer inputs: pass.**
- **Bearer inputs** (`invite claim`/`stash`, `contact scan`, `contact request --scanned`) are read only from stdin (`ZeroStdin`: a duplicated fd, read through a 16 KiB zeroizing buffer that is wiped as it is consumed, so std's stdin buffer is never used) or from `--input-file`. They go into one `Zeroizing` buffer pre-sized to 16 KiB+1 under a `take` cap, so it never reallocates; its trimmed spare capacity is wiped on drop. Then they go to `BearerSecret::from_utf8`, whose error doesn't quote. No bearer input travels in argv.
- **Session framing:** the line buffer is `Zeroizing`, pre-sized to 64 KiB+2 and wiped at every iteration. An over-long remainder goes through `skip_until`, and its consumed bytes are zeroized by `ZeroStdin`. `request.rs` decodes every string, `id` included, into a zeroizing buffer sized to the raw text. `id` is written straight from that buffer. Integers follow RFC 8259.
- **`args`:** each token is checked in place by `grammar.rs` before clap copies anything. Refusals name positions and the command's own names only.
- **Release-binary run:** one session of 9 requests. They carried a link in `input`, the same link as `name check`'s label, as `--text=`, as a surplus positional, and in a request with an unknown field, plus a non-JSON line, a nested session, a request `--spv` and an id with a leading zero. Every answer was the expected refusal or stub. The summary was `{"requests":9}` with exit 0, and **neither stdout nor stderr contained the secret**. One-shot stdin and `--input-file` gave the same result. A link passed in argv exits 2 with `error: unexpected argument found` plus the usage, no value. A bearer-shaped `--wallet` value got `invalid_argument` without quoting it.
- **Panics:** the hook prints only `file:line`. `TASK_PANICKED` withholds the panic's message, as `error.rs` and its test show. In release, `panic = "unwind"` keeps `catch_unwind` working.
- **Passphrase:** it comes from a file only, into an exact-size `Zeroizing` buffer. The clone moved into the unlock closure is `Zeroizing`. The abandon path zeroizes it before any write (verified, 0 copies in the core). The healthy path is F1.
- **Facade records printed:** `ScannedContact` carries only an id for the `dapk` proof, and `FaucetLockKey` only a key id and a public key.

**2. Grants: pass.** Every facade call that takes a grant gets one, bound to the wallet (`authorize(purpose, Some(wallet_id), credential)`):

| Write | Purpose and caps |
|---|---|
| `start_registration` (`identity register`, `invite claim`) | `PlatformOp` = `registration_quote(req).grant`, the same `req` |
| `top_up`, `withdraw` | `PlatformOp` = their quote's `grant` |
| `register_name`, `send_request`, `accept_request`, `set_private_details --publish`, `enable_dashpay_keys`, `update_profile` | `PlatformOp` = `grant_request(identity, action)` with the same identity and action |
| `resume_registration` | `PlatformOp` from `--max-duffs/--max-credits`, only when `waiting` is `Unlock` or `Authorize`; otherwise `None`, per contract §2 row |
| `finish_asset_locks`, `prepare_faucet_lock` | `PlatformOp` from `--max-duffs/--max-credits` (contract: no quote) |
| `discover_identities` | `IdentityScan`, the only purpose its §2.1 row accepts |

- A cap of 0 means not granted (`dw-vault/src/types.rs:100–103`, and `vault.rs:1796–1807` refuses funding or identity signers without one). So dwcli's default 0/0 asks for nothing it cannot use.
- `IdentityScan` is uncapped by design: it is the master-key scan. dwcli requests it only for `identity discover`.
- The calls that take no grant (`ignore`, `unignore`, `mark_read`, `set_main_identity`, `discard_registration`, `upload_avatar`, `stash_invitation`, `forget_invitation`, `sync_now`) match the facade signatures.
- dwcli never calls `begin_flow`, so it creates no leases.

**3. Teardown, abandonment and wipe: pass, apart from F1 and F2.**
- `shut_down` moves a `ManuallyDrop<Engine>` into the timed thread, and only a thread that actually started takes it out. A failed spawn drops the inert wrapper. `must_abandon` leads to `mem::forget`.
- The probe and SPV-stop closures capture only `Arc` clones.
- `healthy` fails closed on a spawn failure, a panic, an error or the 10 s timeout.
- `abandon` zeroizes, prints the debug marker, then writes the owed lines best effort and calls `exit(1)`.
- **R7-1 is fixed:** `run` skips the stall once the engine is abandoned, `wedge_engine` returns instead of panicking when a thread cannot start, and the SPV-stop warning is a best-effort `writeln!`. The committed `dashpay_r6` covers one-shot and session `--spv`, and it passed in my run.
- **R7-2 is fixed:** `IMPLEMENTED` gives exact lines, and the test also asserts that each `IMPLEMENTED` row exists in `TABLE`, so a stale row can't pass by accident.
- **`process::exit` on the abandon path** still runs atexit handlers and C++ static destructors. dwcli links no RocksDB or LevelDB (`cargo tree -p dwcli`, no `rocksdb::` symbols). libsqlite3 and OpenSSL register nothing that joins threads, so the exit cannot block there.
- **Debug fault injection** is gated by `cfg(debug_assertions)` throughout: `injected_fault`, `wedge_engine`, `inject_*`, `panic_engine_task`, the body of `refused_spawn`, `report_wiped`, and the probe's `panic!`. `profile.release` and `profile.dist` (which inherits release) don't enable debug assertions.

**4. Facade-only boundary and the JSON error contract: pass.**
- **Boundary:** dwcli depends on `dw-engine`/`dw-vault` only, with no platform-wallet crate. `dashpay.rs` imports `dw_engine::platform` facade items plus m1's `Engine`, `EngineError`, `NetworkSession`, `TASK_PANICKED` and dw-vault's `GrantPurpose` and `VaultError`. `run_on_engine` (pre-existing m1 API) is used only by the probe and the debug hooks.
- **Error contract:** the code comes from `code()` (§4, or m1 for vault and engine errors), the message from `Display`, and the params by name; `invite claim` adds `link_id`. Failures before the command runs print `setup` once (`REPORTED`). `--no-platform` prints `platform.feature_off{feature:"platform"}` before anything opens, with exit 1 (seen). A clap refusal exits 2 with no JSON. A shutdown error is a stderr warning that leaves the line unchanged.
- **Cosmetic:** one-shot errors also print their message again on stderr. That is diagnostic and allowed by the contract.

**5. What the Sol rounds may have normalized.**
- I checked the healthy path against DEC-106, which none of the rounds did: that is F1.
- I checked the atexit exposure on the abandon path: clean.
- I checked mid-session poison handling: F3.
- I checked whether a short-needle string scan can prove release exclusion. It can't on its own: `dwcli-shutdown` and `dwcli-stop-spv` (14 bytes, production thread names) don't appear in release rodata because they are stored as immediates. So short fault needles such as `-wedge` could hide the same way. The claim rests on the `cfg` reading above plus the behavioral check below.

## Release exclusion

`cargo build --release -p dwcli` passed. SHA-256 of the release binary: `77be8e6511cfdf854deadcb00b4f54a31226333b9c4f421d10c7b5fe51eefc34`.

- **Byte scan:** all 11 of these are absent: `DWCLI_FAULT_INJECT`, `PANIC-SECRET`, `injected poison`, `injected fault`, `injected task fault`, `-spawn-fail`, `spv-task-panic`, `task-panic`, `-poison`, `-wedge`, `forced exit, passphrase`. The `RLIMIT` matches are dw-vault's memlock messages, and `message withheld` is the production hook and `TASK_PANICKED`.
- **Behavior:** `dashpay status` on the encrypted test vault, with `DWCLI_FAULT_INJECT` unset and set to each of `panic`, `task-panic`, `task-panic-poison`, `task-panic-wedge`, `stall`, `stall-spawn-fail` and `unused-spawn-fail`: **8/8 identical**. Each gave the `platform.not_implemented` line, exit 1, the same stderr, no panic or abandonment line, and a run time of 2.4–10.7 s.

Debug binary used for F1's control: SHA-256 `16731fb3fe4feaf8db09f398a6da1067d0021716acb8aa7e90ffb3baacf5cc12`.

## Gate

Run with umask 022, `TMPDIR=/dev/shm/e009-opus/tmp` (mode 0700), `CARGO_BUILD_JOBS=12` and no shared target dir. Load average was 85–170 on 32 vCPUs.

- `cargo fmt --all --check`: **PASS**.
- `cargo clippy --workspace --all-targets -- -D warnings`: **PASS**.
- `cargo build --release -p dwcli`: **PASS** (7 min 4 s).
- `cargo test --workspace --no-fail-fast`: **655 passed, 1 failed, 3 ignored.**
  - The failure is E0-05's `a_held_key_read_does_not_hold_spv_past_the_budget` (`e0_05_bringup.rs:739`). This branch doesn't touch that file, and r7 saw the same load-sensitive failure.
  - Rerun alone at load ~110 it **passes** (15.30 s).
  - All `dwcli` targets passed, including `dashpay_r1`–`r6` and the heap-scan tests.
- Setup error of mine: my first test run created its TMPDIR as mode 0775 under the box's umask 002, and 21 targets failed on the storage layer's permission check ("database ancestor … group/other-writable"). That is the code working as intended, not a branch defect. I fixed the mode and reran the whole suite; the numbers above are from the rerun.

Not repeated from r7, which settled them: the seccomp native-refusal proof, the SIGINT/SIGTERM cases, and the 8-way spawn-failure matrix.

Cleanup: core files were deleted right after counting, and `/dev/shm/e009-opus` is removed after this report. The worktree is unchanged at e37741c with a clean status.

## Verdict

**APPROVE-WITH-CHANGES.** Required: F1 (drop or zeroize the passphrase before `dashpay::report` on the healthy path, plus a regression). Optional: F2's `catch_unwind` around `dashpay::run`, and F3's poison-flip check. F4 is a pre-existing follow-up. Once F1 is in, the manager can verify it against the A/B above (healthy-path count 0) with no further review round.
