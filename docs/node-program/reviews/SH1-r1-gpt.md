**SH1 round 1 — independent GPT review**

**Verdict: APPROVE-WITH-NITS. No blocker or major finding.**

Reviewed `node/sh1-sha256-backend` at `b355bed6b8d5ba6e8be31d2a571c126455a7be6e`, using `git diff node/integration...HEAD` (34e4725c, 21c90f92, b355bed6). Reference: Dash Core's committed sources at v23.1.8 and v24.0.0-rc.3, DEC-28, and the crate README. Review date: 2026-10-08. The SHA-256 implementation, dispatch, and public API appear correct within the declared scope. I found no incorrect digest, unsupported-instruction selection, out-of-bounds access, or ABI violation.

The branch was not edited and no commit was made. Reviewer programs and logs are in `/work/scratch/sh1-review-gpt/`.

**Findings, in severity order**

1. **MINOR — Vendored NIST whitespace conflicts with the existing pre-commit hook.** File: `dash-sha256/tests/data/nist/SHA256LongMsg.rsp:2`; related configuration: `.pre-commit-config.yaml:15`.

   **Scenario:** The new line is literally `b'#  "SHA-256 LongMsg" information '`, with a trailing space. `git diff --check node/integration...HEAD` flags it. The `trailing-whitespace` hook excludes Markdown only; its test-data exclusion belongs to the separate EOF hook. The existing PR workflow runs pre-commit on all files, so this line is eligible for modification and a failed hook. This does not affect any digest. The additional EOF warnings from `git diff --check` are already covered by the EOF hook's data exclusion.

   **Fix:** Exclude the vendored NIST `.rsp` files from `trailing-whitespace`, preserving the documented provenance, or intentionally trim that space and update the LF fixture hash/provenance. I inspected the hook configuration; I did not run a mutating formatter on the branch.

2. **MINOR — Retain a regression test for the 32-bit bit-length boundary.** File: `dash-sha256/tests/core.rs:107`; related vector coverage: `dash-sha256/tests/vectors.rs:99`.

   **Scenario:** Live random inputs stop below 20,000 bytes; committed Core vectors top out at 1,000,000 bytes. None tests the high word of the final 64-bit bit length at 536,870,912 bytes. The implementation currently handles it correctly: `bytes` is `u64`, and finalization shifts that counter before writing all eight bytes. I independently streamed a repeating 1 MiB buffer through all native selections, cloned the state, and checked lengths 536870911, 536870912, 536870913, 536870967, 536870968, 536870975, and 536870976 against Python `hashlib.sha256`; every result matched. This is missing durable regression coverage, not a demonstrated implementation defect.

   **Fix:** Keep a bounded-memory, possibly ignored/extended-suite test spanning this boundary and the 55/56/63/64-byte padding cases. Expected digests should come from an independent implementation or Core. The reviewer reproduction is `probe.rs` plus `long-oracle.py` in the scratch directory.

3. **MINOR — Make complete backend coverage an explicit repeatable gate.** Files: `dash-sha256/tests/common/mod.rs:29`, `dash-sha256/tests/core.rs:119`, `.github/ci-groups.yml:12`.

   **Scenario:** Ordinary CI runs the crate but does not set `DASH_SHA256_EXPECT` or a Core harness. Missing CPU features quietly reduce the implementations tested, and `core_live` returns successfully when its harness variable is absent. The existing ARM job provides useful coverage, but runner labels alone do not require particular extensions. Current implementation evidence is sufficient: I reran every backend under asserted native/QEMU selections and ran the live differential. Future changes would benefit from preserving that guarantee automatically.

   **Fix:** Add an integration/extended gate that builds the pinned Core harnesses, runs x86 QEMU `-cpu max` and aarch64 QEMU `-cpu max`, and supplies the expected implementation names. It can live in the node-program gate if that owns the Core checkout. Include an absence matrix for SSE4.1, SSSE3, AVX, and XSAVE. The current optional local test remains useful.

**Correctness and unsafe-code assessment**

- **Reference fidelity:** `git diff v23.1.8 v24.0.0-rc.3 -- src/crypto/sha256*.cpp src/crypto/sha256.h` contains only copyright changes and the guarded configuration include. I compared committed reference blobs, rather than relying on the current Core working tree. A reviewer script normalized the documented GCC operand/register mapping and matched all **903 SSE4 body instructions/labels** exactly. Separate checks matched all **192 SSE4.1 and 192 AVX2 round inputs/schedule operations**, all **64 portable Transform rounds**, and all **192 portable TransformD64 round inputs**. The shared macro assigns the last two schedule words where portable Core leaves them unassigned; those words are dead before the padding transform, so the difference is harmless. SHA-NI matched all **188 quad-round/shuffle/message-schedule calls**. ARM matched all **432 SHA arithmetic/schedule assignments** and the complete K/INIT/MIDS/FINS/FINAL tables. Adapter helpers, initialization, loads/stores, feed-forward, and round primitive definitions were also inspected.

- **Naked SSE4 ABI:** `src/x86_64/sse4.rs:85` explicitly uses `extern "sysv64"`, including on Windows. Five pushes preserve RBX/R12/R13/R14/R15; RBP is untouched. Entry RSP is 8 modulo 16; subtracting five eight-byte pushes and 32 local bytes makes the transfer slot 16-byte aligned. Every exit restores the frame and saved registers. The function makes no calls and uses allocated stack storage, not the red zone. XMM0–12 and the used volatile general registers may be clobbered under this ABI. RIP-relative constants have explicit 16-byte alignment. I emitted optimized Windows MSVC assembly and checked the Rust-to-SysV bridge: it saves/restores RSI/RDI and XMM6–15 and passes the arguments in RDI/RSI/RDX. There is no missing Windows XMM preservation at that boundary. Evidence: `windows-abi-bridge.txt`.

- **Unwinding/panics:** The naked body contains no Rust operations, calls, or panic paths and has a non-unwinding ABI. Public length assertions and detection self-tests occur outside it. Intrinsic adapters check slice bounds before raw accesses; the statically selected offsets are valid. The naked function lacks unwind metadata, as the README states. Consequently debugger/profiler stack walking while stopped inside it remains a documented limitation; hardware fault/SEH unwinding through it is not supported. I found no ordinary Rust panic that can unwind through the frame.

- **Feature safety and dispatch:** Implementation fields and backend modules are private; safe callers cannot construct arbitrary feature-requiring function pointers. x86 selection requires SSE4.1 and SSSE3 before SSE4, AVX2, or SHA-NI; SHA-NI replaces both multi-way SSE/AVX backends exactly as Core does. AVX/AVX2 detection uses Rust's standard runtime detector, which includes OSXSAVE/XCR0 AVX-state support; it is not a CPUID-bit-only test. ARM selection uses standard SHA2 detection and functions declare NEON/SHA2. No optimized code is selected merely because a caller allows it. All eight `Use` combinations matched Core under QEMU `max`, `max,-sha-ni`, `max,-sha-ni,-avx`, `max,-sha-ni,-xsave`, and `max,-sha-ni,-sse4.1`. With `max,-ssse3`, every Rust selection safely returned portable; Core was deliberately not invoked there because it assumes SSSE3. Evidence: `cpu-matrix.log`.

- **Memory, alignment, lengths, and zero blocks:** Streaming input is partitioned into typed 64-byte arrays, and safe Rust slice limits bound the assembly's shifted block count. Zero blocks branch to the restored return before any input load; Core's self-test exercises that call for every selected transform. Unaligned input/state loads and output stores use the appropriate intrinsics; only aligned constant loads and the explicitly aligned SSE transfer slot use aligned instructions. The D64 array types ensure exactly the required bytes exist. Independent probes checked every input offset 0–31, output offsets 31–0, D64 counts 0–41, small padding/block boundaries, and output canaries, natively and under x86 SHA-NI and ARM SHA2 emulation. All passed. Streaming uses a 64-bit byte count, and the independent ≥2^32-bit checks passed. These tests do not claim exhaustive verification for all possible messages or astronomical counter wraparound.

- **In-place merkle behavior:** `src/dispatch.rs:249` intentionally requires an even level. Core's `ComputeMerkleRoot` performs the mutation check and duplicates the final hash of an odd level before calling `SHA256D64` (`src/consensus/merkle.cpp:45–57`). The crate is a pair-hashing primitive, not a full merkle-root routine; its documented odd-length panic is consistent with that boundary. Automatic duplication here would move merkle policy into this primitive and could obscure the required mutation-check ordering. The temporary input copy precedes writes and subsequent unread pairs start beyond the output range. Empty and all tested even lengths work. The future CS0 caller must retain Core's `while len > 1`, mutation check, and odd duplication; no caller is introduced by these commits.

- **Other targets:** Rust 1.89 compiles the crate. Compile checks passed for aarch64 Linux, i686 Linux, RISC-V Linux, wasm32, and Windows GNU/MSVC. i686, RISC-V, and wasm use portable code; lack of 32-bit optimized backends is documented. This crate requires `std` and does not advertise bare-metal/no_std support. Windows received compile/assembly inspection, not a Windows runtime test. Apple builds/runtime were not verified on this Linux machine. SHA-NI and ARM correctness were checked under QEMU, not physical extension-capable hardware.

**Validation performed**

| Check | Result / scope |
|---|---|
| `cargo test -p dash-sha256 --all-features` | Passed: 12 integration tests and 1 doctest, native debug. Optional live differential absent in this initial run. |
| `cargo test -p dash-sha256 --all-features --release` | Passed: same suites, native release. |
| Native live v24 Core differential with `DASH_SHA256_EXPECT` | Passed: 10,000 streaming SHA-256 cases plus 205 D64 batch cases; standard, SSE4/SSE4.1, AVX2 required. |
| Full release suite under x86 QEMU `-cpu max`, with live Core and expectations | Passed: portable, SSE4/SSE4.1, AVX2, SHA-NI required; live differential took 79.24 seconds. |
| Full release suite under aarch64 QEMU `-cpu max`, with live Core and expectations | Passed: portable and ARM SHA2 required; live differential took 414.23 seconds. |
| Known answers | Both emulated full runs include all 65 CAVP short, 64 CAVP long, 100 Monte Carlo checkpoints, 11 Core vectors, 336 fixture SHA cases, and 34 fixture D64 cases. |
| Property tests | 512 SHA and 512 D64 generated cases per full run, across each available implementation; D64 includes in-place comparison. |
| Reviewer alignment/canary probes | Passed natively, x86 QEMU, and aarch64 QEMU. |
| Reviewer 512 MiB streaming boundary oracle | Passed all native selections against independent hashlib. |
| Reviewer instruction/round/intrinsic source comparisons | Passed; `compare-asm.log`, `compare-rounds.log`, `compare-intrinsics.log`. |
| Feature-absence/all-eight-flags matrix | Passed; `cpu-matrix.log`. |
| `cargo clippy -p dash-sha256 --all-features --all-targets -- -D warnings` | Passed. |
| `cargo fmt -p dash-sha256 --check` | Passed. |
| `RUSTDOCFLAGS='-D warnings' cargo doc -p dash-sha256 --all-features --no-deps` | Passed. |
| `cargo +1.89 check -p dash-sha256 --all-features` | Passed. |
| `cargo check -p dash-sha256 --target …` | Passed for x86_64-pc-windows-gnu, x86_64-pc-windows-msvc, wasm32-unknown-unknown, i686-unknown-linux-gnu, riscv64gc-unknown-linux-gnu. aarch64 release tests built and linked the crate. |
| `git diff --check node/integration...HEAD` | NIST fixture whitespace/EOF warnings only, addressed by finding 1. |

The Core harnesses were built independently using the branch's `build.sh` and unmodified archived v24 sources with per-file CPU flags. Both sides of each emulated live differential ran under the same QEMU CPU configuration. Expected backend assertions prevent these particular runs from silently falling back. D64 counts through 40 exercise the 8/4/2/1-way paths and every remainder combination; Core's transform self-test covers zero through eight blocks on deliberately offset input. Normal CI's successful-but-skipped live test should not be mistaken for this evidence.

The approval applies to this standalone backend crate and its tooling. Adoption into CS0, complete merkle mutation behavior, and the performance of the copying in-place merkle API require their own integration review. The benchmark explicitly uses a separate D64 output buffer while Core's benchmark uses an overlapping buffer; the README counters therefore do not directly establish in-place merkle performance parity.
