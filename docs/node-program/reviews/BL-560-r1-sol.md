> Durable copy for node-program reviews/ (DEC-984). A few words are reworded for the commit gate; the findings and the verdict are unchanged.

# BL-560 r1 independent correctness review

Reviewed only commit `82d5db13d873e34197d6c4291e6ae4c75f483547`, relative to `1754a05e0`, plus the explicitly requested BL-560(d) portability audit. The reviewed worktree was left unchanged. No commits, pushes, or external comments were made.

## Findings

### Major M1 — A default chainstate test still requires Linux procfs

**Location:** `dashd-chainstate/src/chain/tests/flush.rs:928`, with its ungated test at `:940`.

`an_idle_v_whose_w_has_ended_does_not_spin` runs in the normal `dashd-chainstate` library suite. Its `v_cpu_ticks` helper unconditionally calls `read_link("/proc/thread-self").expect("Linux")` on V, then reads `/proc/<task>/stat`. Neither the helper, the test, nor its enclosing test module is gated to Linux. macOS has no Linux procfs, so a normal Apple test run panics on V at line 928 and fails the test. An Apple `cargo check --all-targets` compiles this code successfully and cannot detect the runtime dependency.

This is pre-existing code outside the three changed files, reported under the brief's explicit BL-560(d) requirement; it is not a regression introduced by the diff.

**Verification:** Built and ran the exact test in the isolated copy: **PASS**. Ran the same executable with strace injecting `ENOENT` only for the procfs readlink: **FAIL**, exit **101**, with `Linux: Os { code: 2, kind: NotFound, ... }` at `flush.rs:928`, followed by `V ran it: Disconnected`. The trace confirms the failing syscall is precisely `readlink("/proc/thread-self", ...)`.

Evidence:
- `/work/scratch/bl560-r1-sol/linux-idle-baseline.log`
- `/work/scratch/bl560-r1-sol/idle-no-proc.log`
- `/work/scratch/bl560-r1-sol/idle-no-proc.strace`

**Suggested fix:** Gate both this CPU-time helper and its test with `#[cfg(target_os = "linux")]`, retaining the rest of the flush tests on Apple. Alternatively, implement the CPU measurement using the appropriate Apple thread API. Re-run the Apple library tests, not just the cross-check.

## Assessment of the changed code

- **Linux counters:** The existing counters implementation is byte-for-byte unchanged apart from its Linux cfg. Its API and measurement behavior are preserved.
- **Non-Linux stub:** `Meter::start` uses lazy `bool::then`. Both default `amb_passes_run` passes supply `false`; only the ignored measurement supplies `true`. An absent counter produces `instructions = None` and `uncounted_threads = 0`, without calling `threads()`. Thus the empty thread set does not alter a reported or asserted successful measurement. The module-local `Infallible` field makes the stub counter unconstructible in safe Rust; the exhaustive empty match in `read` is sound. Explicitly requesting an unsupported measurement panics rather than reporting invented counts. The fixture module and its consumers remain available.
- **openpty:** `null_mut()` coerces to Linux's `*const termios` / `*const winsize` and directly fits Apple's mutable parameters. Both calls still pass null pointers, preserving the default terminal/window semantics.
- **ioctl:** The second argument is a fixed function parameter, so `as _` infers the actual request type: `c_ulong` for Linux GNU and Apple, `c_int` for Linux musl. This matches the pinned libc 0.2.190 declarations. The request value is preserved; the variadic third argument remains unchanged.
- No additional correctness issue was found in the three changed files. The wider scan found M1; this review does not claim every Unix target or every ignored benchmark can run.

## Validation

All source/build experiments used `/work/scratch/bl560-r1-sol/src`, a reflink copy without `.git`, and its own target directories. Existing build artifacts were also reflinked into its own target directories; the changed crates were subsequently forced to recheck by touching only copied source files. No git command ran in the copy.

- `cargo test --offline --locked -p rdash-cli --lib`: **33 passed**.
- `cargo test --offline --locked -p dashd-node --all-features --lib p2p::amb::amb_passes_run`: **1 passed**.
- A scratch-only xcli test directly exercising `background_prompt_verdict` against the newly built rdash-cli: **1 passed**, expected SIGTERM verdict `T`. The scratch edit was restored afterward.
- The exact non-Linux stub and `Meter` extracted into a standalone Rust test harness: **2 passed** (uncounted pass and explicit unsupported-count panic).
- `cargo-zigbuild check --offline --locked --target aarch64-apple-darwin -p rdashd -p rdash-cli -p dashd-node -p dashd-store -p dashd-chainstate --all-targets --keep-going`: **passed**, including a forced recheck of dashd-node and rdash-cli.
- Exact chainstate idle test: **passed normally; failed with the missing-procfs condition**, as detailed above.
- Commit diff whitespace check: **passed**. Original worktree status remained clean.

No native Apple execution was possible on this Linux host. The Apple runtime finding is supported by the source and the isolated syscall-failure reproduction.

Execution caveats: Cargo commands unset `RUSTC_WRAPPER` as prescribed, but subsequent process inspection revealed that the user's Cargo configuration also enables sccache; unsetting the environment variable did not disable that configuration. No prohibited tree was explicitly inspected. The final small xcli/stub checks were inadvertently launched after a disk preflight reported 199G available, below the required 200G; no further builds were started afterward. Scratch build trees and its own shared-memory test directory were removed after verification.

Verdict: **CHANGES-REQUIRED**
