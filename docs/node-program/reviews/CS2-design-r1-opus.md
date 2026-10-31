# CS2 design review, round 1 (Opus)

| | |
|---|---|
| Reviewed | `designs/CS2-pow-subsidy.md` (draft of 2026-10-08), focus on §2 (`fp`, the "D (fp)" gate); `designs/CS2-fp-release-binaries.md`; `designs/CS2-fp-evidence/` (tools, excerpts, outputs) |
| Reviewer | Opus 5.5, independent design reviewer. I did not read the GPT round-1 review before writing this |
| Context read | DECISIONS.md DEC-29; A1 §2.4 and §4.5; R4 §1.5–§1.7 and §1.15; R4-B §2.7 and §3; R4-C §1, §2, §4.3 and §7 |
| Core checked | v23.1.8 and v24.0.0-rc.3: `pow.cpp`, `arith_uint256.{h,cpp}`, `validation.cpp` (`ConvertBitsToDouble`, `GetBlockSubsidyHelper` and its callers, `ContextualCheckBlockHeader`), `governance/superblock.cpp`, `masternode/payments.cpp` (v23: `validation.cpp` `GetMasternodePayment`), `chainparams.cpp` (all four networks and every override parser) |
| Also checked | base-sdk 33fcc59 `pkgs/num/src/{arith256,compact}.rs` and `bitcoin-internals` 0.7.0 `u256.rs` |
| Binaries | All **six** shipped v24.0.0-rc.3 builds, each verified against the release `SHA256SUMS.asc`: x86_64, aarch64 and riscv64 Linux, win64, and x86_64 and arm64 macOS |
| Scratch | `/work/scratch/cs2-review-opus/`. My own tools are there (they share no code with the evidence tools): `kgwpow.c` (host libm plus MPFR), `kgw_exact.py` (exact-rational thresholds), `kgw_extra.py`, `kgw_eps.py`, and `winpow.c` (runs win64 dashd's own `pow` natively). Also the per-libm tables `pow-*.txt` and the disassembly |

Severity scale:
- **Blocker:** the design is wrong and must change before approval.
- **Major:** the design must change before the affected phase (here phase C, `fp`/KGW) starts.
- **Minor:** fold the fix into the implementation brief.
- **Nit:** wording or bookkeeping.

## Verdict: APPROVE-WITH-CHANGES

There are no blockers. The central technical claim holds, and I re-derived it by a method independent of the evidence tools:
- KGW's `pow` has exactly 4032 possible inputs.
- Every shipped build I could measure makes the same break decision for every `(mass, actual-seconds)` pair.
- A correctly rounded table, used with Core's expression and separate rounding, reproduces those decisions.
- The closest input is **35,751.0 ulps** from a boundary (m = 2712).
- The subsidy is `x*x` in every build I disassembled. The integer plan matches Core's overload and wrapping semantics. D1 (the table) is the right call.

There are two major findings, and both concern the *scope* of the proof, not its conclusion:
- **M-1.** The report and design claim that "every shipped Core build" was covered. The evidence covers only the Linux x86_64 and aarch64 builds, two of the six that Dash ships.
  - win64 carries its **own statically linked mingw-w64 `pow`**: x87 `exp2l(y·log2l|x|)`, whose results depend on the run-time x87 precision control.
  - riscv64 fuses the horizon computation.
  - macOS calls Apple's libm.

  I measured everything except Apple. The decisions are identical, but the report's statements (three `pow` results, two horizon values, 21 masses, quirk 40's x86_64/aarch64 wording) are wrong and must be corrected.
- **M-2.** The in-repo proof (§2.4 test 1) checks the table only against the CI host's glibc, so it cannot cover four of the six builds. Its SSE2 leg depends on a glibc-version-specific tunable spelling that fails silently. There is a cheap, platform-independent replacement: an ε-interval test, which works because the decision is monotone in `p`.

---

## 1. Verified independently (no change needed)

**(1) KGW's input domain.** I re-derived it from `pow.cpp:15-79` at both pins:
- The only `pow` argument is `x = fl(m/28.2)`, with `m = PastBlocksMass`, and `y = fl(−1.228)`.
- `m = i` runs 1..=`PastBlocksMax`, which is 4032 on mainnet (`86400·7/150`). The loop breaks *before* mass 4033.
- Decisions are made only for `m ≥ PastBlocksMin = uint64(86400·0.025) = 2160/150 = 14`. The value 2160.0 is exact (the error of 0.025 is 1.2×10⁻¹³ < ½ ulp of 2160).
- `t = 150·m` exactly.
- `a = PastRateActualSeconds` is a difference of two `uint32` `nTime` values cast to `int64`, clamped at ≥ 0, so `a ∈ [0, 2³²−1]`. `a = 0` gives the literal ratio 1.0, and `s < 1 < d` for every `m`.
- Header `nBits` never reach the floating-point code; they feed only the integer average.

On adversarial chains, any timestamps (including non-monotonic ones) and any valid `nBits` stay inside this domain.

Reachability:
- testnet sets `nPowKGWHeight == nPowDGWHeight == 4002`, and devnet sets both to 4001. No override touches either (`chainparams.cpp` v24: 412-413, 594-595; `MaybeUpdateHeights` runs only for regtest).
- regtest returns at `fPowNoRetargeting` (`pow.cpp:187`).
- So KGW is mainnet-only. The domain claim of report §4.1 and design §2.5 F1 is **correct**.

**(2) The decisions and the 35,751-ulp margin.** I wrote my own generator (`kgwpow.c`; MPFR `mpfr_pow` at 53 bits RNDN for the correctly rounded column). Instead of bisection, I derived the thresholds in **exact rational arithmetic** from the round-to-nearest-even boundaries of `d` and `s`. For example, `A_fast = ⌊t/lm(d)⌋` with the tie rule, where `lm(d) = (pred d + d)/2`. Each threshold was then cross-checked with IEEE division at its neighbours. Results:

| Check | Result |
|---|---|
| glibc 2.43 x86_64 FMA ifunc vs correctly rounded | 1 ulp at m ∈ {1695, 2109, 2337, 2381, 3083, 3111}: matches report table A |
| glibc 2.43 x86_64 SSE2 ifunc (`GLIBC_TUNABLES=glibc.cpu.hwcaps=-AVX2,-FMA`) | 1 ulp at m ∈ {851, 2109, 2381, 3083, 3111}: matches table B |
| glibc 2.43 aarch64 (qemu) | identical to table A |
| design `out/kgw-pow-cr.txt` vs my MPFR table | identical, all 4032 entries |
| thresholds across the 8 combinations {CR, A, B, aarch64} × {separate, fused} | identical for all 4019 masses |
| design `out/kgw-thresholds.txt` vs my exact-rational thresholds | 0 mismatches |
| minimum margin, every combination | **35,751.0 ulps at m = 2712** (thresholds 405744, 407859; nearest `a` = 407858) |
| largest relative `pow` error that keeps every decision | 1.53×10⁻⁹ at m = 2712. ε = 10⁻⁹ passes at every mass for both roundings; 1.6×10⁻⁹ fails at m = 2712 (`kgw_eps.py`) |

**(3) The subsidy is `x*x`.** My own disassembly of v24.0.0-rc.3 x86_64 Linux (bounds from `.eh_frame`: `0x719e10–0x71a387`):
- All three bases are `addsd`, `divsd`, `mulsd xmm0,xmm0`, `divsd`, `cvttsd2si`, then the clamps, then `imul 0x5f5e100`.
- `ConvertBitsToDouble` is inlined: `65535/(bits&0xffffff)`, then a two-way unrolled `mulsd 256.0` loop or `mulsd 0.00390625` loop.
- There is no `pow` call, no FMA and no x87.

The same holds in the builds the report did not examine:
- win64: `mulsd x,x` ×3;
- riscv64: `fmul.d x,x,x` ×3 with `fcvt.l.d …,rtz`;
- arm64 macOS: `fmul d0,d0,d0` ×3 and `fcvtzs`, with no `fmadd` in `GetBlockSubsidyHelper`.

The design's subsidy reference is **correct for every shipped build I checked** (5 of 6; x86_64 macOS uses the same clang front end as arm64 macOS).

One harmless detail: the x86_64 binary evaluates `ConvertBitsToDouble` speculatively, *before* the `NetworkIDString` test. The operation is pure, so this is unobservable.

**(4) No other floating point.**
- `masternode/payments.cpp`, `governance/superblock.cpp`, `evo/creditpool.cpp`, `evo/providertx_util.cpp` and the CS2 parts of `chain.h` contain no `double`, `float` or libm calls at either pin.
- `GetPaymentsLimit` reaches floating point only through the subsidy.
- `nPowTargetSpacing = 2.5 * 60` is folded at compile time.
- The tolerance check is `mulsd 0.5`, `subsd`, `andpd` and `comisd` (my x86_64 listing at `0x7258ec–0x725900`), with `ConvertBitsToDouble` inlined twice.

**(5) Overload and wrapping traps.** The macOS binaries keep their symbols, which gives independent confirmation: `GetNextWorkRequired` calls `base_uint<256>::operator*=(unsigned int)` (`__ZN9base_uintILj256EEmLEj`) and `operator*(const base_uint&, unsigned int)` (`__ZmlRK9base_uintILj256EEj`). That confirms §3.7's `*= int64 → *= uint32` row.

base-sdk 33fcc59 matches Core where it matters:
- `Arith256::wrapping_shl` and `wrapping_shr` guard `shift >= 256 → 0`. This matters because the underlying `bitcoin-internals` `U256::wrapping_shl` *masks* the shift amount.
- `Mul<u32>` is `mul_u64(..).0`, a wrapping product.
- `Ord` is derived on `U256(high, low)`.
- `expand` computes `negative` and `overflow` on the post-shift word, as Core does.

For devnet `-powtargetspacing`:
- The `uint32` truncation threshold of 2²⁹ is right.
- The `⌊i64::MAX/72⌋` bound is right (DGW's `nTargetTimespan*3`).
- The BTC `DifficultyAdjustmentInterval() = 0` SIGFPE condition (`-minimumdifficultyblocks < 4000`) is right.

**Also verified:**
- The design's line-by-line `kgw_horizon_crossed` matches the x86_64 shape: `divsd ratio`, `call pow`, `mulsd 0.7084`, `addsd 1`, `divsd 1/d`, two compares.
- §3.3–§3.6 match `pow.cpp`, including:
  - "break before updating" at `i > PastBlocksMax`;
  - DGW's 23-interval window and per-step `Mul<u32>`;
  - BTC's 300 s rule and the walk-back.
- The regtest `0x207fffff` induction holds.
- The devnet domain holds: block 1 is the only pre-V20 block, its prev `nBits` is genesis `0x207fffff`, and devnet takes no `-testactivationheight`.
- `GetBlockSubsidyHelper` is byte-identical between the pins, and so is the tolerance block.
- The v23 `fV20Active` ↔ v24 `era != Classic` mapping holds.

---

## 2. Findings

### M-1 (Major, report §0/§2/§4 and design §0, §2.1, §2.4, §9 D2): the "every shipped build" claim covers only the two Linux builds

**Problem.** Dash ships six builds of each pin: x86_64, aarch64 and riscv64 Linux, win64, x86_64 macOS and arm64 macOS (`gh release view` for both tags). The report examined two of them.

The design nevertheless says:
- the table's "decisions equal every shipped Core build's";
- "Core thus has three bitwise-different `pow` results and two different horizon values";
- quirk 40 (DEC-29 D2) is "x86_64/aarch64".

A1 §4.5 asked for "the release binary's own routine", and win64 has one. What the four unexamined builds do (v24.0.0-rc.3; I verified each tarball against `SHA256SUMS.asc`):

| Build | KGW horizon | `pow` | Measured |
|---|---|---|---|
| riscv64 Linux | **fused** `fmadd.d fa5,fa0,fs4,fs1` (KGW at `0x3044d4`) | host glibc (`pow@GLIBC_2.27`) | glibc 2.36 riscv64 = table A; thresholds identical |
| **win64** | separate `mulsd 0.7084`, `addsd` | **statically linked mingw-w64 `pow`** at `0x140d90ff0`. `dashd.exe` imports no math functions from `msvcrt.dll`. The normal path is `(double) exp2l((long double)y · log2l(fabs x))` on the x87 unit (`fld`/`fmul`/`fstp TBYTE`, then calls to `log2l` and `exp2l`) | Run natively from the shipped `.exe` (`winpow.c` maps the PE at its ImageBase). With x87 precision control **64-bit** (`0x37F`, what `fninit` sets): 1 ulp at m ∈ {194, 851, 2381, 3111}, a **new table**. With **53-bit** (`0x27F`, the Windows default control word): differs from CR at **3073 masses, by up to 5 ulps**, another table. Thresholds identical in both cases; margin 35,751 ulps |
| x86_64 macOS | separate `mulsd`, `addsd` | Apple libm `_pow` (stub `0x100f2957e`, via `--indirect-symbols`) | not measurable on agentbox |
| arm64 macOS | **fused** `fmadd d0,d0,d1(0.7084),d8(1.0)` | Apple libm `_pow` (stub `0x100d96284`) | not measurable on agentbox |

Across the builds I could measure, there are at least **four** distinct `pow` tables, not two or three:
- glibc FMA ifunc, which equals aarch64 and riscv64;
- glibc SSE2;
- win64 at 64-bit precision;
- win64 at 53-bit precision.

The horizon value `(d, s)` differs between shipped builds at **41** masses, not 21.

The win64 result also adds a new kind of run-time dependence: the x87 precision-control word of whichever thread validates the header. mingw's startup `fninit` sets 64-bit precision; the Windows x64 default control word is 53-bit (`0x27F`). I did not establish which one dashd's validation threads actually run with, so I measured both.

The conclusion survives every case I measured, and Apple's libm is covered by the tolerance argument: its documented `pow` error is below 1 ulp, far inside 1.5×10⁻⁹ (about 6.9 million ulps). But the gate document currently states a proof it does not contain.

**Fix:**
1. Correct report §0 items 3–6 and §4.1, design §0.1 and §2.1, and the quirk-40 text in DEC-29 D2. Suggested wording: "Core's release builds evaluate KGW's horizon with different `pow` implementations (host glibc per ifunc; win64's static mingw-w64 x87 `pow`, whose result depends on the thread's x87 precision control; Apple libm) and with separate (x86_64 Linux, win64, x86_64 macOS) or fused (aarch64 and riscv64 Linux, arm64 macOS) rounding. Decisions are identical for every `pow` within 10⁻⁹ relative error; nearest input 35,751 ulps."
2. Add the riscv64 and win64 rows (tables, listings, the native-execution method) to the evidence, and extend `reproduce.sh` to them.
3. State the Apple assumption explicitly: Apple's `pow` has relative error < 10⁻⁹ on the 4032 points. Either cite Apple's accuracy statement, or have pasta run the 30-line `kgwpow.c` once on the Mac Studio (x86_64 under Rosetta and arm64). agentbox has no macOS.
4. Add an O17 PIN-DELTA item: on a re-pin, re-examine all six builds, not two.

### M-2 (Major, design §2.4 tests 1–4, §6.1): the in-repo proof is host-dependent and its SSE2 leg can no-op silently

**Problem.** Test 1 checks the table against "the host libm's `pow`" with separate and fused rounding, run twice under `GLIBC_TUNABLES`. That covers only what the CI host's glibc can produce: tables A and B. It cannot cover win64's mingw `pow` (either precision), Apple's libm, or a future glibc.

The SSE2 leg depends on the tunable spelling, which changed from `-AVX2_Usable,-FMA_Usable` (glibc ≤ 2.32; `reproduce.sh` itself passes both spellings to the 2.31 container) to `-AVX2,-FMA`. A wrong spelling is ignored without error. The test would then run the FMA variant twice and still pass, so "both glibc variants are covered" would be false without anyone noticing.

The report's strongest statement, that any `pow` within 1.5×10⁻⁹ gives the same decisions, is not a test at all.

**Fix: make the proof platform-independent.** The decision is monotone in `p`:
- `d = fl(1 + fl(0.7084·p))` and `fl(fma(p, 0.7084, 1))` are non-decreasing in `p`;
- `s = fl(1/d)` is non-increasing;
- `A_fast(d)` and `A_slow(s)` are monotone.

So checking two endpoints proves the result for every `p` between them:
1. **`kgw_decisions_invariant_under_pow_error`** (replaces test 1 as the primary proof). For each m ∈ 14..=4032 and both roundings, the thresholds at `p_lo = fl(p_cr·(1−ε))` and `p_hi = fl(p_cr·(1+ε))` must equal the implementation's, with ε = 10⁻¹⁰. I ran this with ε = 10⁻¹², 10⁻¹⁰ and 10⁻⁹: 0 failures. ε = 10⁻¹⁰ leaves about 450,000 ulps of headroom over the worst libm measured (5 ulps). The test needs no libm, runs anywhere, and covers every shipped and future build whose `pow` is that accurate. Compute the endpoints exactly: integer `(m, i, f)` scaling, or a `u128` rational, since `mul_add` is banned.
2. Check in the measured shipped tables (A, B, win64-PC64, win64-PC53) as fixtures, with their provenance, and assert their thresholds. This is the "release binary's own routine" leg of A1 §4.5, made reproducible.
3. Keep the host-libm run as a sanity test (`cfg(target_env = "gnu")`). It must **assert which variant ran**: the default run's table must equal fixture A, and the tunable run's must equal fixture B. Otherwise it fails rather than passing vacuously. The same applies to the L1 KGW windows' `GLIBC_TUNABLES` rerun (§2.4 "Differential", §6.2).
4. Restate test 4 (`kgw_margin_floor`) in the same units as the claim: a minimum tolerated relative error of at least 10⁻⁹ (measured 1.53×10⁻⁹), not 10,000 ulps of `d`.

### m-1 (Minor, design §2.5 F3): the tolerance domain is mis-stated

**Problem.** "The header's own `nBits`, which has already passed `CheckProofOfWork` (… so its size byte is ≤ 30)" is false. Under mainnet powLimit `2²³⁶−1`:
- `0x20000001` (target 2²³²) is valid, not negative and not overflowing;
- so is `0x1f000fff`;
- and so is `0x01010000` (target 1).

The size byte of a valid header ranges up to **32**, so `ConvertBitsToDouble` takes up to **three** `÷256` steps. `n1` then lies in about [2.4×10⁻⁴, 2.7×10⁶⁷]. That is still finite and normal, so the conclusion stands.

"`n1` is always finite, so no NaN arises" holds only where PoW was checked. `getblocktemplate` proposals reach `ContextualCheckBlockHeader` through `TestBlockValidity(fCheckPOW=false)`. A mantissa-0 `nBits` then gives `n1 = +∞`, and `∞ > ∞·0.5` is false, so the header is *accepted* by the tolerance check. With `n2 = +∞` as well, the difference is NaN, which is also accepted.

The design's `!(abs(n1 − n2) > n1·0.5)` reproduces both cases only if written literally.

**Fix:**
1. Correct the bound.
2. Require the literal `!(x > y)` form, with a comment. A `<=` rewrite changes the NaN case.
3. The tolerance is tested only on history. Add a cheap differential: random and edge `(bits, bits)` pairs, including mantissa 0, size bytes 0, 1, 29–34 and 0xff, compared with the predicate computed from `oracle_convert_bits_to_double`.

### m-2 (Minor, design §0 item 4 and §3.7): Core's integer division is described wrongly

**Problem.** §3.7 says integer divisors go "through `base_uint(uint64_t)`". Both pins in fact have `base_uint& operator/=(uint64_t)` (v24 `arith_uint256.h:161`, `.cpp:77`; v23 `:163`, `.cpp:84`) and `friend operator/(const base_uint&, uint64_t)` (v24 `:211`; v23 `:213`). Every integer division resolves to these by standard conversion:
- KGW's `/ i` and `/= PastRateTargetSeconds`;
- DGW's `/ (nCountBlocks+1)` and `/= nTargetTimespan`;
- BTC's `/= nPowTargetTimespan`.

The macOS symbols confirm it (`__ZN9base_uintILj256EEdVEy`). The member uses a limb-wise fast path for divisors ≤ `u32::MAX`, and falls back to `/= base_uint(b)` otherwise. That fallback is live on a devnet with `-powtargetspacing ≥ ⌈2³²/24⌉`.

Both paths compute exact floor division, the same as base-sdk's `Div<u64>`. **The port is correct; the description and the "two traps" framing are not.**

**Fix.** Rewrite the row as `/= x`, `/ x` (integer `x`) → `operator/=(uint64_t)` / `operator/(…, uint64_t)`, which is exact floor division at both pins, and so `/ (x as u64)`. Make `diff_next_work`'s devnet spacing list include a value above `⌈2³²/24⌉ = 178,956,971`, which it already does with 2²⁹.

### m-3 (Minor, design §2.4 "Differential" and §6.2): the KGW boundary windows need two generator invariants

**Problem.** A fast-side window that breaks exactly at mass m needs `a_m = A_fast(m) < a_{m−1} = 150(m−2)`. That is, the m-th block back must be *newer* than the (m−1)-th: non-monotonic timestamps. This is fine for a function-level differential, but someone may "fix" the generator to produce monotonic times and silently lose the fast side.

Separately, the boundary pair only tests something if the two sides produce different `nBits`. Equal `nBits` on both sides makes the pair vacuous. I expect the two sides to differ, since the targets differ by more than 0.1% against a 23-bit mantissa, but the generator should not rely on that.

**Fix.** In the generator:
1. assert that the oracle's `nBits` differ between `A_fast(m)` and `A_fast(m)+1`, and between `A_slow(m)−1` and `A_slow(m)`;
2. document the non-monotonic timestamps as intended.

### m-4 (Minor, design §2.5 "Excluded" and §4.2 step 5): `-highsubsidyfactor` is both rejected and "mirrored"

**Problem.** Core starts with any `int` factor. Overflow is possible only at devnet block 1: it is the only devnet block with a base above 5, and only when `nHighSubsidyBlocks > 0`. It overflows for `factor > ⌊i64::MAX/(500·COIN)⌋ = 184,467,440`. Both release binaries `imul` and wrap there (x86_64 listing `0x719f93`).

The design both rejects such configurations in `SubsidyParams::new` (so rdashd refuses to start a devnet that dashd starts) and justifies `wrapping_mul` as mirroring the binary. These contradict each other.

**Fix.** Pick one; I recommend accepting the factor and wrapping, which is the shipped binaries' behaviour and costs nothing. If the rejection stays, list it as a deliberate startup deviation, give it a clear error, and drop the "mirrors the binary" sentence.

`GetIntArg` → `int` truncation of the three devnet arguments (`chainparams.cpp:1251-1253`) belongs to BS5. Note it there so `DevnetOverrides` receives the truncated `i32`.

### m-5 (Minor, design §9 D4, report §7 F1): the log2 follow-up must also cover the non-glibc builds

**Problem.** F1 (owned by LQ1 under DEC-29) treats `std::log2(signingActiveQuorumCount)` as a glibc call. win64 has no math imports, so it uses a static mingw-w64 x87 `log2`, whose result may depend on precision control like its `pow`. macOS uses Apple's libm.

**Fix.** Pass a note to LQ1: the domain check must hold for every shipped libm. That is easy if the count is always a power of two, where every competent `log2` is exact. Otherwise derive the integer equivalent from the decision, as KGW's thresholds were derived.

### n-1 (Nit, design §2.5, row "nPrevHeight near INT_MAX")

"The port computes the mathematically intended result" is not what the binary does. The release build keeps a wrapping 32-bit counter (`add esi,edi; cmp r13d,esi; jge`, x86_64 at `0x719f60`). For `n > INT_MAX − interval`, `i` wraps and the loop continues, and with an even interval (150) it can never exit. The case is unreachable and the oracle refuses it, so say "Core's behaviour is undefined, and the release binary does not terminate for some heights; the port returns the decline's fixed point".

### n-2 (Nit, design §2.3)

"On the admissible domain every value lies in [0, 1111] and is never NaN" holds for **every** `u32` input: `dDiff` is ≥ 0 or +∞ and never NaN, so `x² ≥ 1` and each base is in [0, 1111]. The saturating `as i64` therefore equals x86's `cvttsd2si` and aarch64's `fcvtzs` everywhere. The exhaustive sweep can assert this on all 2³² inputs.

### n-3 (Nit, design §2.2 rule 4)

- "The exhaustive sweeps (§6.3) do reach subnormals for size bytes above 30" is too loose. `ConvertBitsToDouble` first produces a subnormal at size byte **156** (`0x9c`): `65535/0xffffff · 2⁻⁸⁽ˢⁱᶻᵉ⁻²⁹⁾ < 2⁻¹⁰²²`.
- "Process-wide flush-to-zero" is inaccurate: MXCSR (and FPCR) are per thread.
- The rounding mode (`fesetround` by a C dependency) belongs to the same stated-assumption class. Name it alongside FTZ/DAZ.

### n-4 (Nit, report §6 and the evidence README)

The SHA256SUMS file is checked, but its signature (`SHA256SUMS.asc`) is not verified with GPG, and neither the report nor `reproduce.sh` says so. Either verify it against the Dash release keys or say that only the published checksums were checked.

---

## 3. On the six questions asked

1. **KGW's domain:** confirmed: 4032 `pow` inputs; `(m, a) ∈ [14, 4032] × [0, 2³²−1]` for decisions; mainnet only. Adversarial timestamps and `nBits` stay inside it (§1(1)).
2. **Shipped builds and the table:** every measured shipped build (Linux x86_64 under both ifuncs, aarch64, riscv64, and win64 under both x87 precisions) makes identical decisions, and the CR table reproduces them. I reproduced 35,751.0 ulps independently. Apple's libm is not measurable here and is covered by the 10⁻⁹ tolerance (M-1, M-2).
3. **`x*x`:** confirmed by my own disassembly of x86_64 Linux (all three formulas) and of the builds the report skipped: win64, riscv64 and arm64 macOS (§1(3)).
4. **Other floating point:** none in the paths CS2 covers (§1(4)). The domain statement for the tolerance check needs correcting (m-1).
5. **Integer traps:** reproduced exactly. The `*=` trap is confirmed from symbols, and base-sdk's shift guard is confirmed. The division "trap" is mis-described but semantically right (m-2). The devnet factor policy is self-contradictory (m-4).
6. **Verification plan:** sound in structure (L0, L1 boundary windows, exhaustive 2³² sweeps, full history). Make the KGW proof platform-independent and self-checking (M-2), harden the boundary generator (m-3), and add a tolerance differential (m-1).
