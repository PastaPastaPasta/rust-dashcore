APPROVE-WITH-NITS

Reviewed task XH1 / DEC-51b as crypto/consensus code, at `174e70cdbe3f288d7d7b2055ba81eab842162c11` (three commits over `ed085c2b`). Review used a fresh detached worktree at `/work/scratch/xh1-gpt/repo`. No source under review was changed, and no commits, pushes or GitHub posts were made.

1. **Minor — the differential runner can report success after jobs never run.** `dash-x11/contrib/differential.sh:46` discards `xargs`' failure status with `|| true`; line 47 checks only log files that exist. A failure opening a job's redirected log leaves no file to check, and line 56 reports the number of surviving files instead of requiring the number of scheduled jobs.

   **Concrete failing configuration:** `JOBS=1`, one seed, 10 cases, one harness, and a log directory that becomes unwritable after the first job. I ran the unchanged script with a scratch Cargo/test stub: the first test emitted its normal case-summary and success lines, then changed the log directory to mode 555. The other three jobs could not open their output files. The script exited **0** and printed:

   ```text
   sh: 1: cannot create .../x11-header-h1-harness-1.log: Permission denied
   sh: 1: cannot create .../x11-long-h1-harness-1.log: Permission denied
   sh: 1: cannot create .../stages-h1-harness-1.log: Permission denied
   all 1 runs passed; logs in .../tooling/logs
   ```

   An exhausted filesystem or changed directory permissions can produce the same missing-log condition with the real test executable. This weakens the reliability of future differential evidence. It does not invalidate the comparisons independently completed in this review.

   **Fix:** retain and propagate the `xargs` exit status, even when continuing to collect diagnostics. Also require every scheduled job's expected log to exist and pass, and require the completed count to equal `${#jobs[@]}`. The reproduction and its stubs are retained in `/work/scratch/xh1-gpt/tooling/`; output is `missing-log-repro.log`.

No blocker or major finding was established. No digest mismatch, invalid dispatch selection, overflow panic or unsafe-memory violation was found in the checks below.

### Bit-exactness and upstream provenance

Dash Core pins resolved to:

- v23.1.8: `728f5055836c6d29806412fc7223ac8fe05af991`.
- v24.0.0-rc.3: `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`.

`git diff --exit-code v23.1.8 v24.0.0-rc.3 -- src/crypto/x11 src/hash_x11.h` is empty. The committed `blockfilters.json` is also byte-identical to Core's fixture. Harness builds checked the B1 build trees' `COMMIT` files and linked their own `libbitcoin_crypto_{sph,ssse3,x86_aesni}.a` objects. Core's checkout was not changed.

The variable-length interface is correctly confined to BLAKE. Inside X11, each subsequent function always receives a 64-byte digest; the public `stage` interface explicitly rejects other lengths. Therefore variable-length padding tests for Luffa/SIMD/etc. would test an interface this implementation does not offer. I instead tested their actual 64-byte message, padding and finalisation paths directly against Core.

Independent scratch programs generated and directly compared structured inputs, without the implementation's random generator or aggregate SHA-256 fixture as the comparison mechanism:

- **185,729 X11/BLAKE inputs:** every length 0..=1100 with zero, FF, 80, 01 and arithmetic-pattern bytes; every byte value at every position for lengths 1..=32; byte-value sweeps on zero/FF backgrounds around lengths 79/80/81, 111/112, 127/128/129, 239/240, 255/256/257 and 1023/1024/1025; every byte value at every header position.
- **99,584 stage inputs:** all 256 constant-byte buffers; every byte value at every one of 64 positions against both zero and FF backgrounds; all 65,536 alternating two-byte patterns; every single-bit flip against zero and FF backgrounds. Each input was checked through all eleven stages.
- Both corpora passed under **all five distinct implementations**, against **both pins**, with both Core's normal dispatch and forced software ECHO/SHAvite. All eight corpus/oracle groups completed. Buffers cycled through every address residue modulo 64.
- A separate alignment matrix passed for **every length 0..=1100 × zero/FF × every address residue 0..63 × all five implementations**. This includes exact BLAKE block and padding boundaries on unaligned buffers.
- Rust **1.89** builds passed direct Core comparisons on all 5,505 length-pattern cases and the first 33,024 stage cases, through all five implementations. The same subsets passed under QEMU AArch64 using the portable implementation. `x86_64-unknown-none` also compiled successfully with the vector module correctly gated out.

Padding/finalisation inspection agrees with Core: BLAKE's empty tail uses a zero compression counter and tails from 112 bytes require two final blocks; Luffa processes two message blocks, its padding block and two squeeze blocks; CubeHash performs the padding block's 16 rounds, flips state word 31, then performs 160 final rounds; SIMD processes the zero-filled message block and a separate final length block with its final twist. These paths were exercised in the direct stage comparisons.

Artifacts: `make_cases.py`, `core_structured.cpp`, `structured.rs`, `run_structured.py`, `structured.log`, `alignment.rs` and `alignment.log` under `/work/scratch/xh1-gpt/`.

### Dispatch and self-test assessment

The dispatch checks SSSE3/SSE4.1 and AES separately. AVX2 additionally requires CPUID leaf 7, AVX, OSXSAVE and XCR0's XMM/YMM bits. XGETBV is behind the OSXSAVE check. Private static implementation objects prevent callers from constructing a selection that bypasses these checks; `Use` requests unsupported features safely fall back.

QEMU tests exercised every public selection mask, stage comparisons on unaligned inputs, and X11 on boundary lengths and zero/FF buffers. All expected selections passed:

- `qemu64` → portable; `Nehalem` → SSE.
- `Westmere`, `SandyBridge` → SSE/AES.
- `Haswell`, `max` → SSE/AES/AVX2.
- `max,-aes` → SSE/AVX2.
- `max,-avx2`, `max,-avx`, `max,-xsave` → SSE/AES.
- `max,-ssse3`, `max,-sse4.1` → portable.

The masked-feature configurations did not execute unsupported backend instructions. The AArch64 probe selected portable for every mask. Evidence: `qemu.py`, `qemu.log`, `qemu-aarch64.log`, `aarch64-differential.log`.

The genesis self-test exercises the 80-byte BLAKE specialisation and **every vector function selected by that implementation**, including both JH/SIMD compressions and all Luffa/CubeHash finalisation rounds. An ALL selection uses AVX2 rather than the unused SSE alternatives; explicitly detecting SSE or considering it as a fallback tests its own functions. The general BLAKE path is not covered by the genesis self-test, but is covered by the independent length matrix and repository tests.

I also injected deterministic failures in separate scratch dispatch wrappers, leaving reviewed sources untouched. An AVX2 JH failure caused `detect(ALL)` to panic and `auto()` to choose SSE/AES; an AES Grøstl failure chose SSE/AVX2; both failures chose SSE. Corrupting shared BLAKE caused every candidate, including portable, to fail, and `auto()` panicked. The successfully selected fallbacks matched the clean portable implementation on additional boundary inputs. Evidence: `fault/` and `fault.log`.

**Fallback ruling:** the implementation mechanics work, and this review establishes no erroneous fallback digest in the pinned code. A passing genesis vector is not a proof against input-selective miscompilation or a fault affecting multiple backend families. Automatic fallback should not be described as a general hardware-health guarantee. For the full node's consensus initialisation, I recommend the existing strict `Implementation::detect(Use::ALL)` entry point so an observed self-test failure stops startup; the optional crate-level `auto()` fallback need not block XH1's merge.

### Unsafe code and local validation

Reviewed every production `unsafe` site and the intrinsic families. External input loads/stores use unaligned operations with fixed-size references or bounded slices. JH's aligned loads address complete chunks of `repr(C, align(32))` static tables at appropriate multiples of 16/32. Grøstl's byte-to-vector transmute preserves size and admits every bit pattern. Feature annotations match the dispatch requirements, and the SIMD i16 reduction/butterfly bounds stay within the lane range before intentional modular multiplications. The cached pointer addresses immutable static objects.

Completed checks:

- `cargo test -p dash-x11 --release -- --test-threads=2`, with live Core harnesses for both pins: all 17 tests and the doc test passed.
- `cargo test -p dash-x11 -- --test-threads=2`: all 17 tests and the doc test passed in the debug profile with overflow checks enabled.
- Miri on portable X11 boundary lengths and all stages with unaligned inputs: passed.
- Miri with explicitly enabled AES/SSSE3/SSE4.1/AVX/AVX2/XSAVE, directly invoking **all eleven vector entry points** on zero and FF inputs: passed, including alignment/bounds and intrinsic precondition checking. This scratch driver imports the original modules by path and bypasses CPUID only for these explicitly provisioned vector calls.
- `cargo clippy -p dash-x11 --all-targets -- -D warnings`, `cargo fmt -p dash-x11 -- --check`, and `contrib/node/check-pure.sh`: passed.
- Native MSRV build/dispatch and the cross-target checks described above: passed.

Logs and the portable/vector Miri drivers are retained under `/work/scratch/xh1-gpt/`. Miri does not validate actual CPUID/XGETBV or hardware execution; those were assessed by source inspection and native/QEMU runs.

### Performance and harness review

JH's internal `black_box` calls obscure fixed masks and the all-ones vector to control LLVM's rewrites; they preserve the operands and do not exclude hashing work from the measured loop. The workload also obscures input and consumes output on every iteration. Core's workload hashes the same zero-filled 80-byte buffer using its original objects. Compile-time folding of fixed padding/initial state is a legitimate optimisation of this specialised X11 interface.

Three independent `perf stat -e instructions:u` measurements per side, subtracting a zero-iteration run and dividing by 20,000 calls, reproduced the medians:

- Rust all backends: **39,055.016** instructions/header.
- Rust portable: **149,984.040**.
- Core v23.1.8 harness: **169,617.029**.
- Core v24.0.0-rc.3 harness: **169,617.024**.

Direct runs of each pin's original `bench_dash -filter=^Pow_X11_0080b$ -min-time=100` measured **169,630.424** and **169,630.407** instructions/header. The roughly 13-instruction difference is harness/benchmark overhead; it leaves the claimed **0.23× / 0.88×** ratios intact. The README appropriately ties these claims to B1's builds/compiler/flags; they should not be read as a guarantee against every possible Core build. Core's small-footprint JH/CubeHash settings and the effective `-O2` ordering were verified in the B1 Makefile. Evidence: `perf_counts.py`, `perf.log`, `bench-v*.json`.

The quoted-harness fix works: the unchanged `differential.sh`, with `JOBS=2`, completed all eight expected jobs for one plain harness and the quoted command `env DASH_X11_CORE_SOFT=1 /work/scratch/xh1-gpt/harness-v24.0.0-rc.3`. Both supplied pins and all implementations were exercised by the release suite. The runner's separate missing-log false-success defect is finding 1.

### Summary

The inspected backend is bit-exact with both Core pins on the independent structured corpora, respects tested CPU/OS feature masks, passes Miri and overflow-enabled debug checks, and reproduces the reported instruction advantage. One minor differential-runner failure-reporting issue should be fixed. Full-chain and blk-file scans reported by the implementer were inspected through their verifier code but were not independently repeated; this review concentrated on structured adversarial inputs. Load was checked and heavy work paused when it reached 70; differential input batches were at most 185,729 and review jobs were bounded below eight. The private mode-700 `TMPDIR` was removed after tests completed.

Final verdict: **APPROVE-WITH-NITS**.
