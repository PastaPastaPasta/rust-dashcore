> Durable copy for node-program reviews/ (DEC-984). A few words are reworded for the commit gate; the findings and the verdict are unchanged.

# BL-560 review r1 (Opus): correctness

- **Head:** `82d5db13d873e34197d6c4291e6ae4c75f483547` (`node/nd1-bl560`). I reviewed only that commit, on top of BL-555's `1754a05e0`.
- **Copy:** a reflink copy of `/work/home/agent/workspace/rdc-wt/nd1-bl560` at `/work/scratch/bl560-r1-opus/src`. I ran no git in it, used my own `CARGO_TARGET_DIR`, `RUSTC_WRAPPER` unset, `CARGO_INCREMENTAL=0`, `CARGO_BUILD_JOBS=8`, `nice -n 19`.
- **Libc:** `0.2.190`, from Cargo.lock. I read its source in Cargo's crate cache.

## Evidence (runs)

| Run | Target | Result |
|---|---|---|
| `cargo clippy --locked -p rdash-cli -p dashd-node --all-targets -- -D warnings` | x86_64-unknown-linux-gnu | rc=0 (`linux-clippy.log`) |
| `cargo test --locked -p rdash-cli --lib -- stdin::tests` | x86_64-unknown-linux-gnu | 2 passed, including `nested_guards_restore_only_when_the_last_drops` (the `openpty` change) |
| `cargo test --locked -p dashd-node --lib -- p2p::amb` | x86_64-unknown-linux-gnu | `amb_passes_run` ok; `amb_node_leg` ignored (`linux-test.log`) |
| `cargo-zigbuild check --locked --all-targets --keep-going` on rdashd, rdash-cli, dashd-node, dashd-store, dashd-chainstate, **plus** dashd-masternode, dashd-mining, dash-governance, dash-coreimport, node-l1, dash-testkit, dash-conformance, dash-oracle-sys | aarch64-apple-darwin | rc=0. The only warnings are 2 pre-existing ones in dash-oracle-sys (see N3) (`apple.log`) |
| `cargo-zigbuild clippy --locked -p rdash-cli -p dashd-node --all-targets -- -D warnings` | aarch64-apple-darwin | rc=0 |

- The darwin recipe follows `/work/scratch/nd1-bl555/NOTES.md`. I used my own copy of the clang shim, and pointed `DASH_ORACLE_CACHE` at a path that doesn't exist, so nothing read `/work/cache`.
- The extra crates in the darwin check are the full set of workspace crates that depend on dashd-store, directly or transitively (`cargo metadata`). Leaving out dash-fuzz, that set is the 12 crates beyond dashd-store itself. The owner's check covered only 5 of them.

## Check-by-check

### 1. Linux behaviour is unchanged

- **counters:** the Linux `mod counters` body is byte-identical. The diff adds only `#[cfg(target_os = "linux")]`, so the Linux path is the same code.
- **openpty (`rdash-cli/src/stdin.rs:259-261`):**
  - On linux_like, libc's `termp: *const termios` and `winp: *const winsize` (`unix/linux_like/mod.rs:2249`).
  - `ptr::null_mut()` gets its `T` from the expected type and then coerces `*mut T` → `*const T`. Both values are null, so the call is semantically identical.
  - Linux clippy is clean and the pty test passes.
  - The comment is accurate. FreeBSD, NetBSD, Haiku and uclibc are also `*mut`; OpenBSD, Fuchsia and Cygwin are `*const`. `null_mut` fits all of them.
- **ioctl (`rdash-cli/tests/xcli/main.rs:3119`):** in every case the inferred `_` is the request parameter's type, which is fixed (not variadic), so it is well-defined.

  | Target | `ioctl` request type | `TIOCSCTTY` type | Effect of `as _` |
  |---|---|---|---|
  | linux-gnu | `Ioctl = c_ulong` | `Ioctl = c_ulong` | identity |
  | linux-musl | `Ioctl = c_int` | `Ioctl = c_int` | identity |
  | Android | `c_int` | `c_int` | identity |
  | Apple | `c_ulong` (`unix/bsd/mod.rs:611`) | `c_uint` = `_IO('t', 97)` = 0x20007461 (`new/apple/xnu/sys/ttycom.rs:80`) | lossless zero-extend |
  | FreeBSD / NetBSD | `c_ulong` | `c_ulong` | identity |

  The third argument (`0`) is unchanged. Off Linux this path runs nowhere: `deviation_rows` → `sigint_prompt_rows` → `background_prompt_verdict` only runs once `xcli_deviations` passes its `cfg!(target_os = "linux")` early return (main.rs:222).

### 2. The stub

- **Who can reach `Counters::open`:** `Meter` and `counters` are local to `amb.rs`. chain.rs, realv.rs and realv_peers.rs import only `branch`, `node`, `regtest`, `Peer` and `headers_msg`.
  - `Meter::start(count)` is called only from `pass_a` and `pass_b`.
  - `count` is `false` from `amb_passes_run` (runs by default) and `true` only from `amb_node_leg`, which is `#[ignore]`.
  - `count.then(counters::Counters::open)` is lazy, so no default test reaches `open` off Linux.
- **`threads()` returning an empty set:** it is called only in `Meter::finish`, inside `self.counters.map_or(0, |c| …)`. Off Linux `counters` is always `None`, because `open` panics and nothing else builds a `Counters`. So the closure never runs, and `uncounted_threads` is 0 exactly as on Linux with `count = false`. `amb_passes_run` asserts only `instructions.is_none()`. No reported or asserted value changes.
- **The `Infallible` field:**
  - It is sound: the type is uninhabited, so no value of `Counters` can exist off Linux, and `match self.0 {}` is a valid empty match on a place of an uninhabited enum.
  - There is no `unsafe` and no `Drop`.
  - The darwin clippy run with `-D warnings` is clean, so there are no dead-code or unreachable warnings.
  - `open() -> Counters` with a `panic!` body type-checks as `!`.

### 3. Anything else in these files

- **amb.rs:** apart from the gated module, it uses only std networking, `tempfile` and `Instant`. No other platform-specific calls.
- **stdin.rs test:** `openpty`, `tcgetattr` + `ECHO`, `sigaction` and `close` are POSIX. On macOS `openpty` is in libSystem, so it links with no `-lutil`.
- **xcli:** the remaining Linux-only calls (`F_DUPFD_CLOEXEC`/`dup3`/`SYS_close_range`/`__errno_location`) are in the `#[cfg(target_os = "linux")]` block at main.rs:3101. Off Linux the helper keeps its descriptors where they are, and the comment says so.
- None of this ran on Apple: there is no Mac on this box. The darwin results are compile-only.

### 4. BL-560 (d): other dashd-store dependents on Apple

- **Result:** all 13 crates check clean for `--all-targets` on aarch64-apple-darwin. That is the 12 above plus rdash-cli; dash-fuzz was not covered.
- **dash-testkit:** its Linux-only `fallocate` and `prctl` code is already gated: `crash` is `cfg(all(feature = "crash", target_os = "linux"))` (lib.rs:22), and `kill_with_parent` is `cfg(target_os = "linux")`.
- **conformance:** the `/proc` reads (the conformance L2 reader `l2.rs` and its memory test) are runtime reads that compile anywhere. `l2.rs` returns `None` or the default when a read fails. Whether that memory test passes at runtime on macOS was not tested.
- **node-l1 `examples/chain_sample.rs`:** reads `/proc/self/stat` with `unwrap_or_default()`, so it compiles and degrades on macOS.

## Findings

No blockers. No majors.

- **N1 (nit), `dashd-node/src/p2p/amb.rs:249,344`:** the gate is `target_os = "linux"`, so Android, where `gettid`, `perf_event_open` and `/proc/self/task` all exist, now gets the panicking stub.
  - *Scenario:* someone runs `amb_node_leg -- --ignored` on an Android device. It panics where the real counters would have worked.
  - *Impact:* test-only and ignored; no CI lane runs dashd-node tests on Android.
  - *Suggested fix:* none needed. Optionally use `any(target_os = "linux", target_os = "android")` on both modules.
- **N2 (nit), `dashd-node/src/p2p/amb.rs:17-19`:** the module doc says `amb_node_leg` must run "with a PMU" but doesn't say it is Linux-only. The stub's doc and its panic message do say so.
  - *Suggested fix:* optionally add "(Linux)" to the `cargo test … amb_node_leg` line.
- **N3 (nit, pre-existing, not in this commit; BL-560 (d)), `dash-oracle-sys/tests/archive_check.rs:11`:** `cache_dir_name` is imported unconditionally but used only in the `#[cfg(target_os = "linux")]` test at :161-176.
  - *Scenario:* on aarch64-apple-darwin, `--all-targets` gives `unused import` plus `function cache_dir_name is never used`. A macOS lane that runs `clippy --workspace --all-targets -- -D warnings` would fail there.
  - The check itself (rc=0) is fine.
  - *Suggested fix:* gate the import, or move the `assert_eq!(cache_dir_name("fuzz"), …)` at :176 into an ungated test. Track it separately if the macOS lane will cover dash-oracle-sys.

## Verdict

**APPROVE-WITH-NITS.** All three fixes are correct. Linux behaviour is unchanged (same code, identity casts and coercions, tests pass), and the stub can't be reached by any default test. Every dashd-store dependent checks clean for `--all-targets` on aarch64-apple-darwin. The nits are optional.
