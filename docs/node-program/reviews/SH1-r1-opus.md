# SH1 `dash-sha256` review, round 1 (Opus)

| | |
|---|---|
| Reviewer | independent Opus 5.5, high effort |
| Date | 2026-10-08 |
| Under review | rust-dashcore `node/sh1-sha256-backend` at `b355bed6` (`git diff node/integration...HEAD`, 3 commits, new crate `dash-sha256`) |
| Core source | `~/workspace/dash` at `v23.1.8` (728f505583) and `v24.0.0-rc.3` (0364ed626e), `src/crypto/sha256*.cpp`, `src/consensus/merkle.cpp`, `src/bench/crypto_hash.cpp` |
| Scratch | `/work/scratch/sh1-review-opus/` (scripts, Core builds, logs) |

## Verdict: APPROVE-WITH-NITS

I found no digest-level defect. Every backend is a faithful port, and I could check that mechanically wherever text could be compared. Every backend also ran against Core's own unmodified code, natively or under QEMU. The unsafe code is sound on the targets the node ships for (x86_64 Linux/Windows/macOS, aarch64 LE). The instruction-count numbers for the Rust side reproduce exactly.

No blocker and no major. There are three minors:
1. x32 and aarch64_be gating;
2. CI does not assert which backends ran (BL-18 already tracks this);
3. the Core column of the bench table depends on the compiler.

There are also a handful of nits. None of them need to block CS0 adoption.

Counts: blocker 0 · major 0 · minor 3 · nit 8.

## What I ran

All runs used my own target dir (`/work/scratch/sh1-review-opus/target`). I did not modify the branch.

| Run | Backends exercised (asserted with `DASH_SHA256_EXPECT`) | Core differential | Result |
|---|---|---|---|
| native agentbox (E5-2687W v4), `--release` | `standard`, `sse4(1way),sse41(4way)`, `sse4(1way),sse41(4way),avx2(8way)` | live, v24.0.0-rc.3 harness | 13/13 pass |
| `qemu-x86_64 -cpu max` (test binaries and harness both under QEMU) | the three above + `x86_shani(1way,2way)` | live, v24.0.0-rc.3 harness; again with a **v23.1.8** harness | all pass (both pins) |
| `--target aarch64-unknown-linux-gnu`, `qemu-aarch64 -cpu max` | `standard`, `arm_shani(1way,2way)` | live, v24.0.0-rc.3 aarch64 harness | all pass (core_live 429 s) |

I built the Core harnesses myself with `contrib/core-harness/build.sh` from the tagged commits. `harness.cpp` calls Core's `CSHA256`, `SHA256D64` and `SHA256AutoDetect`, and does not reimplement anything.

### Mechanical faithfulness checks

These scripts are in the scratch dir.
- **SSE4 asm** (`cmp_sse4.py`):
  - I took Core's `__asm__` body, substituted Core's operands with the register/stack mapping documented in `sse4.rs`, mapped the `%=` labels to `2/3/4/5` with jump directions, and diffed the result instruction by instruction against the naked function, minus its prologue and epilogue.
  - Result: 903 vs 903 instructions, **0 differences**.
  - The `K256`/`FLIP_MASK`/`SHUF_*` statics also match Core (317/317 constants).
- **Ordered hex-constant sequences** (`cmp_consts.py`, comments stripped), **0 differences** in each:
  - `shani.rs` vs `sha256_x86_shani.cpp`: 286/286;
  - `aarch64.rs` vs `sha256_arm_shani.cpp`: 150/150;
  - `macros.rs` (`block_rounds!` + `transform_d64!`) vs Core's `sha256::Transform` + `sha256::TransformD64`: 294/294.
- **Hasher and dispatch logic, by hand:**
  - `Sha256::update`/`finalize` match `CSHA256::Write`/`Finalize` line by line, including the pad formula `1 + ((119 - bytes%64) % 64)`.
  - `d64_wrapper` matches `TransformD64Wrapper`.
  - `sha256d64` uses widths in the order 8, 4, 2, 1, as `SHA256D64` does.
  - The self-test data, states and `result_d64` match `SelfTest`.
  - The selection order matches `SHA256AutoDetect`: SSE4.1 gate, SHA-NI replaces SSE4/AVX2, and AVX2 needs AVX + OSXSAVE + XCR0&6. std's `avx`/`avx2` include the XCR0 check.
- **The test suite really reaches each backend (mutation check).**
  - I copied the crate, disabled the self-test assert, and first mutated one `ror` in the SSE4 asm's last round. Native tests fail in `backends`, `core_fixture`, `nist_*`, `core_sha256testvectors` and `free_functions`.
  - Then I mutated only the last constant of the SHA-NI **2-way** transform-2 B lane. All native tests still **pass**. Under `qemu-x86_64 -cpu max` they fail in `sha256d64_matches_portable`, `core_fixture` (`n 2, x86_shani(1way,2way)`) and `core_sha256d64`.
  - So coverage is real, but for SHA-NI and ARM it depends on the CPU the tests run on (m-2).
- **Windows ABI bridge.** I cross-built `x86_64-pc-windows-gnu` and disassembled it. The Win64-ABI caller `sse4::transform` saves `rsi`, `rdi` and `xmm6–xmm15` around the `call` to the sysv64 naked function, and the naked function's frame is identical to the Linux one. That is correct.
- **No unwind info.** On Linux the naked function has no `.eh_frame` FDE (I checked with `readelf --debug-dump=frames`), as the README says.

### Instruction counts (`perf stat -e instructions:u`, (N − 0)/(N·bytes))

- **Rust side:** the branch's `examples/workload`.
- **Core side:** I wrote my own driver, `core_workload.cpp`, which runs bench_dash's loop bodies from `crypto_hash.cpp` over Core's unmodified v24.0.0-rc.3 `sha256*.cpp`, built with each file's Core flags. `cw-plain` is GCC 15.2 `-O2`. `cw-hard` adds Core's hardening flags (`-fPIE -fstack-protector-all -fcf-protection=full -fstack-clash-protection -D_FORTIFY_SOURCE=3`).

| Row | README dash-sha256 | **measured dash-sha256** | README Core | measured Core `-O2` | measured Core hardened |
|---|---|---|---|---|---|
| SHA256_STANDARD | 51.78 | **51.78** | 52.88 | 50.88 | 50.88 |
| SHA256_SSE4 | 36.80 | **36.80** | 36.80 | 36.80 | 36.80 |
| SHA256_32b_STANDARD | 111.75 | **111.75** | 115.78 | 109.94 | 110.88 |
| SHA256_32b_SSE4 | 81.44 | **81.44** | 83.41 | 81.63 | 82.41 |
| SHA256D64_1024_SSE4 | 52.42 | **52.42** | 58.52 | 60.62 | 62.75 |
| SHA256D64_1024_AVX2 | 18.45 | **18.45** | 20.75 | 20.11 | 19.92 |
| DSHA256_1M | 36.80 | **36.80** | 36.80 | 36.80 | 36.80 |

The dash-sha256 column reproduces to the hundredth on every row I ran. The single-stream SSE4 rows equal Core because the asm is identical. The Core column moves by up to about 7 % with the compiler and flags. See m-3.

## Findings

### Minor

**m-1. The x86_64 backends are gated on `target_arch = "x86_64"` only. On the x32 ABI the naked asm reads out of bounds. The ARM backend is not gated on endianness.**
- `sse4.rs:86`: `transform_asm(s: *mut u32, chunk: *const u8, blocks: usize)` does `shl $0x6,%rdx; add %rsi,%rdx` on 64-bit registers.
- On `x86_64-unknown-linux-gnux32` (`target_arch = "x86_64"`, `target_pointer_width = "32"`), `blocks` is a `u32`, and the psABI leaves the upper half of `rdx` unspecified. `inp_end` becomes garbage, and the loop reads far past the slice: UB in safe callers.
- `aarch64_be` is the analogous case. `load_be`/`store_be` assume little-endian lane order for `vld1q_u8` + `vrev32q_u8` + `vreinterpretq`. Core never builds that combination, and nothing tests it.
- Neither is a node target. Still, a one-line cfg removes the UB rather than relying on nobody building for those targets.
- Fix:
  - `#[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]` on `mod x86_64` and the x86 `select`;
  - `#[cfg(all(target_arch = "aarch64", target_endian = "little"))]` for the ARM backend;
  - other configurations fall back to `STANDARD`.

**m-2. CI never asserts which backends ran, and nothing exercises the backends the runner's CPU lacks (already in BACKLOG BL-18).**
- `.github/ci-groups.yml` puts the crate in `core`, which runs on ubuntu-latest, ubuntu-22.04-arm, macos-latest and windows-latest.
- Today's GitHub x86 runners (EPYC 7763 / Ice Lake) have SHA-NI and AVX2, and the ARM and macOS runners have SHA2, so CI *probably* covers every backend. Nothing checks that, though.
- My mutation run shows the stakes: a wrong constant in the SHA-NI 2-way code passes every test on a CPU without SHA-NI. The self-test would catch that example at startup, but not a bug outside the self-test's inputs.
- Fix: set `DASH_SHA256_EXPECT` per runner in `build-and-test.yml`:
  - x86 Linux/Windows: the four x86 names;
  - ARM Linux/macOS: `standard;arm_shani(1way,2way)`.
- Optionally add a QEMU job (`-cpu max`, which emulates SHA-NI) and a periodic `core_live` run.
- BL-18 records the first part. Do it before CS0 makes this the node's only SHA-256.

**m-3. The bench table's Core column is not reproducible as stated, and on the portable rows "every row is at or below Core" depends on the compiler.**
- The README and the `b355bed6` message compare against "Core v24.0.0-rc.3 `bench_dash`" but don't say how that bench_dash was built.
- With Core's own `sha256.cpp` built by GCC 15.2 `-O2`, Core's portable code takes **fewer** instructions than the port:
  - SHA256_STANDARD: 50.88 vs 51.78 (+1.8 %);
  - SHA256_32b_STANDARD: 109.94–110.88 vs 111.75 (+0.8–1.6 %).
- The SIMD rows hold under every build I tried. D64 SSE4 and AVX2 beat Core by 8–16 %, and the single stream ties.
- Impact is low: no x86_64 node runs the portable code, and aarch64 nodes use SHA2.
- STATUS.md's "matches/beats Core on every bench row" should still be scoped:
  - record the compiler, flags and source (Guix release / distro / self-built) of the bench_dash used;
  - say "portable rows within ±2 % depending on Core's compiler";
  - or measure against a Guix release `bench_dash`, which is what mainnet nodes run.

### Nits

- **n-1. `Use::SSE4` doc** (`dispatch.rs:26`). "Required by the others" is true only on x86_64. On aarch64, `detect(Use::SHANI)` returns `arm_shani` without SSE4, which matches Core. Say "on x86_64".
- **n-2. `sha256d64_in_place` docs leave the ordering implicit** (`dispatch.rs:240-247`, `lib.rs`). "Duplicating … and the mutation check are the caller's" should add: *check the pairs before pushing the duplicate*, as `ComputeMerkleRoot` does.
  - A caller that duplicates first flags every odd level as mutated and rejects valid blocks.
  - CS0 §7 already specifies the right order and tests it (`[a,b,b]` not mutated), so this is a footgun only for later adopters (`dash/src/merkle_tree`, BL-18).
  - Alternatively, ship a Core-exact `compute_merkle_root(Vec<[u8;32]>) -> ([u8;32], bool)` here so nobody reimplements it.
  - The odd-length `assert!` itself is the right choice: it fails loudly instead of silently differing from Core.
- **n-3. `sha256d64_in_place` copies more than it needs to.** It stages every group of 8 pairs through stack buffers (768 B of memcpy per 8 pairs). Only the first group can alias; from `done ≥ 8` on, the write range `[done, done+8)` lies before the read range. A `split_at_mut` would avoid the copies. Perf only.
- **n-4. Naked function without CFI** (tracked in BL-18). On ELF and Mach-O, hand-written `.cfi_startproc` / `.cfi_def_cfa_offset` / `.cfi_offset` / `.cfi_endproc` in the `naked_asm!` would give profilers correct stacks. Windows would need `.seh_*`. That matters for the CS0/CS1 perf work, where this function will dominate profiles.
- **n-5. `detect` runs the self-test on every call.** Core's `SHA256AutoDetect` does too, and the doc says so. Caching a per-static "passed" flag would make `Sha256::with_implementation(Implementation::detect(x))` cheap for callers that ignore that doc. Optional.
- **n-6. `contrib/ins-per-byte.sh` on hybrid CPUs.** Its awk picks up both `cpu_core/instructions:u/` and `cpu_atom/instructions:u/`, so `bc` gets two numbers. Sum the lines, or pin to `cpu_core`.
- **n-7. `DASH_SHA256_CORE_HARNESS` is split with `split_whitespace`** (`tests/core.rs:65`), so a harness path with spaces can't be used. Fine for agentbox; document it, or take a separate runner variable.
- **n-8. Untested target: Intel macOS (Mach-O x86_64).** The naked asm with `sym` + `(%rip)` should assemble there; rustc handles the underscore prefix. Neither CI (macos-latest is ARM) nor I built it. Low risk; noted for completeness.

## Answers to the brief

**Unsafe code.**
- **SSE4 naked asm.**
  - It is identical to Core's asm under the documented register mapping.
  - Its frame saves exactly the callee-saved registers it uses (`rbx`, `r12`–`r15`; `rbp` unused).
  - Stack alignment: entry `rsp ≡ 8 (mod 16)`, then 5 pushes and `sub $32`, so `xfer` at `(%rsp)` is 16-aligned for `movdqa`.
  - `blocks == 0` takes the `je 5f` path; the self-test exercises it with `blocks[..0]`.
  - XMM0–12 are caller-saved under sysv64, and on Windows the compiler bridges the ABIs correctly (verified in disassembly).
  - `transform` is a `#[target_feature(enable = "ssse3")]` safe fn that is only reachable through an `Implementation`. Implementations exist only as private statics returned by `detect`/`standard` after the feature checks, and `Implementation` has private fields and no constructor. The `unsafe fn` pointer invariant therefore holds.
- **Intrinsics backends (SSE4.1, AVX2, SHA-NI, ARM).**
  - All raw loads and stores go through bounds-checked 16-byte subslices or fixed-size arrays, with unaligned loads except the `Align16` constants.
  - The `target_feature` sets cover every intrinsic used: SHA-NI needs `sha,sse4.1,ssse3` for `blend`/`alignr`/`shuffle_epi8`, and ARM needs `neon,sha2`.
- **Detection.**
  - x86 uses `is_x86_feature_detected!`; the logic is checked under "Hasher and dispatch logic" above.
  - The extra SSSE3 requirement only ever turns the optimized path off, and never changes digests.
  - aarch64 uses std's `sha2`, which is stricter than Core's `HWCAP_SHA2`; it works on macOS and other OSes as well.
  - The only soundness gap is the cfg in m-1.

**Every backend is exercised:** yes, on agentbox plus QEMU, against live Core of both pins (table above). The aarch64 "no SHA2" fallback cannot be emulated (QEMU has no `sha2=off`). It is the same portable code that the x86 runs and the aarch64 `standard` selection exercise. CI coverage is unasserted (m-2).

**API footguns for CS0.**
- `sha256d64(out, input)` maps directly onto CS0 §7's two-buffer `sha256d_64_many(out, pairs)`, with no odd-count semantics.
- `sha256d64_in_place` panics on odd input rather than silently differing from Core. Its one sharp edge is the mutation-check order (n-2), which CS0 already handles.
- `Sha256`/`Sha256d` are `Clone` and copy the midstate, buffer and length, which CS1 C3 needs.
- `Send`/`Sync` are auto-derived and correct.
- A self-test failure panics, and every later `auto()` call retries and panics again (`OnceLock` doesn't cache a failed init), so a wrong digest is never returned. With `panic = "unwind"`, a caller that catches panics keeps running but cannot hash, which is the right failure mode.

**Instruction-count claims:** reproduced exactly for dash-sha256 on 7 rows. The Core column depends on how Core is built (m-3).

## Disposition of the `code-review` skill findings (high effort)

| Skill finding | Disposition |
|---|---|
| x32: naked asm assumes 64-bit `usize` | **Accepted** as m-1 (with aarch64_be added) |
| CI never sets `DASH_SHA256_EXPECT` / harness | **Accepted** as m-2; already BL-18 |
| `sha256d64_in_place` extra copies | **Accepted** as n-3 (perf nit) |
| `Sha256d::finalize` second hash through a generic `Sha256` | **Rejected**: that is exactly Core's `CHash256::Finalize` (`sha.Write(buf).Finalize`). Faithful by design; the gain is a few dozen instructions per txid |
| `detect` reruns the self-test | **Accepted** as n-5 (optional; Core does the same) |
| `Use::SSE4` doc wrong on aarch64 | **Accepted** as n-1 |
| Tests hand-roll `hex`/`unhex` instead of `hex-conservative` | **Rejected**: test-only, 6 lines, and it keeps the crate's dev-deps minimal |
| K/INIT duplicated across backends | **Rejected**: it mirrors Core's per-file tables, which keeps the diff against Core mechanical. I verified each copy against Core (0 diffs), and each backend's tests and self-test cover its own copy |
| `ins-per-byte.sh` on hybrid CPUs | **Accepted** as n-6 |
| Harness path `split_whitespace` | **Accepted** as n-7 |
