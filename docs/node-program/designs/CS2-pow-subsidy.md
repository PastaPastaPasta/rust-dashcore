# CS2: proof of work, floating point, subsidy and payments

| | |
|---|---|
| Status | **rev1, after design review r1** (`reviews/CS2-design-r1-gpt.md`, `reviews/CS2-design-r1-opus.md`; both APPROVE-WITH-CHANGES, no blockers; DEC-29, DEC-33). The changes DEC-33 requires are applied; the dispositions per finding are in §12. §2 (`fp`) passed the "D (fp)" gate (DEC-33); phase C implements it. Phases A and B (§10) are being implemented on `node/cs2-pow-subsidy` (DEC-29 D6) |
| Date | 2026-10-08 |
| Author | CS2 implementer (Opus 5.5 xhigh), design phase |
| Task | `dash-consensus::{pow, fp, subsidy, payments}`: BTC/KGW/DGW retargets, the min-difficulty rules, the mainnet ≤ 68589 ±50% tolerance, `CheckProofOfWork`, the block subsidy and treasury split, the masternode payment schedule and the platform share |
| Binding inputs | A1 §2.4 and §4.5 (rev1), Appendix A; PLAN §0 and §4 row CS2; R4 §1.5 and §1.7, R4-B §3, R4-C §1–§2 and §7; PRD FR-CON-14, -15, -20, -21; DEC-05, DEC-07, DEC-25, DEC-26, DEC-29, DEC-33 |
| Evidence | **`designs/CS2-fp-release-binaries.md`** (rev1: all six shipped builds of both pins) and `designs/CS2-fp-evidence/` |
| Code read at | Dash Core `v23.1.8` (728f5055) and `v24.0.0-rc.3` (0364ed62); base-sdk `node/base-sdk` 33fcc59 (`dash-num`, `dash-params`); `node/integration` 4a3df7ac; CS0 design (`designs/CS0-node-primitives.md`); O1 `node/o1-oracle-sys` 89f33820; O4 `node/o4-core-vectors` 9224abd4; phase A+B branch `node/cs2-pow-subsidy` at cb67697e (read only) |
| Worktree | `~/workspace/rdc-wt/cs2-pow-subsidy`, branch `node/cs2-pow-subsidy` |

No part of CS2 is under PLAN §7 handling.

---

## 0. Summary

1. **The release binaries settle the floating-point question** (report §0). All 12 shipped builds were examined: x86_64, aarch64 and riscv64 Linux, win64, and x86_64 and arm64 macOS, each at v23.1.8 and v24.0.0-rc.3.
   - The subsidy's `pow(x, 2.0)` is a single multiplication in every build.
   - KGW's `pow(m/28.2, −1.228)` is a different routine on each platform family:
     - Linux: the host's glibc, chosen by CPU on x86_64 (FMA, FMA4 or SSE2 variant);
     - win64: a statically linked mingw-w64 x87 routine whose result depends on the thread's x87 precision control;
     - macOS: Apple's libm.
   - The horizon `1 + 0.7084·p` is rounded separately on x86_64 Linux, win64 and x86_64 macOS, and fused (one `fmadd`) on aarch64 Linux, riscv64 Linux and arm64 macOS.

   At least four distinct `pow` tables ship, and the horizon values differ between shipped builds at 41 masses. Yet every build makes **the same decision for every possible input**. The nearest input is 35,751 ulps from a boundary, and any `pow` within relative error 10⁻⁹ of the true value gives the same decisions.
2. **`fp` contains no libm, not even a port.** It uses only IEEE `+ − × ÷`, comparisons and integer↔float casts, in Core's operation order.
   - The subsidy is plain arithmetic.
   - KGW's one `pow` call becomes a lookup in a 4032-entry table of correctly rounded values, `fp::KGW_POW`. That is the whole input domain of that call.
   - The in-repo proof is **platform-independent**. A two-endpoint check per mass shows that every `pow` within 10⁻⁹ gives the implementation's decisions, under either rounding. The measured shipped tables are checked in as fixtures and asserted. A host-libm run asserts which variant it measured. A differential test against the oracle checks both sides of every decision boundary.
3. **The admissible domain is closed numerically** (§2.5):
   - KGW's domain is closed by the threshold proof.
   - The subsidy's floating-point inputs are every compact a valid header can carry, canonical or not, plus the artificial `nBits = 1`. All intermediates are normal and finite. The all-`u32` sweep against Core is an explicit merge gate. Recorded history is validation, no longer an exclusion argument.
   - Raw header bits are preserved everywhere, because non-canonical encodings change the early subsidy.
   - Values where Core has undefined behaviour or aborts are listed with their reasons.
4. **`pow`, `subsidy` and `payments` are integer ports** on base-sdk's `Arith256`, whose wrapping semantics match `arith_uint256` (§3.7). Core's one overload trap (`*= int64` resolving to `*= uint32`, a modular truncation) is written out explicitly. Where Core's C++ overflows signed integers, the port computes what the shipped binaries compute: a 64-bit wrapping product (§5).
5. **Verification** (§6):
   - L0: O4's transcribed `pow_tests`, `subsidy_tests`, `block_reward_reallocation_tests` and `evo_sharedmn_tests`.
   - L1: `diff_next_work`, `diff_subsidy_payments`, `diff_nbits_acceptable` and `diff_payout_split` against both oracle pins.
   - Exhaustive: all 2³² `nBits` for the subsidy formulas and `ConvertBitsToDouble` (a phase C merge gate).
   - History: every mainnet and testnet header's `nBits`, every height's subsidy, and every coinbase's masternode payout since DIP3 enforcement.
   - Quirk tests 8, 9, 11, 12, 15 and 40.

---

## 1. Scope

**In scope:**
- `pow`:
  - `get_next_work_required`: the dispatch, `GetNextWorkRequiredBTC`, `CalculateNextWorkRequired`, KGW, DGW, the testnet/devnet min-difficulty rules, devnet `nMinimumDifficultyBlocks` and regtest `fPowNoRetargeting`;
  - `check_proof_of_work`;
  - `nbits_acceptable`, which is `ContextualCheckBlockHeader`'s `nBits` rule: exact equality, or the mainnet ≤ 68589 tolerance.
- `fp`: every `f64` operation in consensus. That is the subsidy formulas, `ConvertBitsToDouble`, the tolerance comparison, and KGW's horizon test with its table.
- `subsidy`:
  - `GetBlockSubsidyHelper` and its two projections;
  - `GetBlockSubsidy` (the genesis case included);
  - `CSuperblock::GetPaymentsLimit`, which is a subsidy formula (quirk 15).
- `payments`:
  - `GetMasternodePayment`, `MnRewardEra` and `PlatformShare`;
  - the pure arithmetic of the owner/operator split: `operator_reward`, owner payout amounts, `SplitAmountByShares` and `ProRataFloor`.
- The parameter views these functions read (§3.1), until BS5 completes `dash-params`.
- Quirks 8, 9, 11, 12, 15 (R4 §1.15) and 40 (DEC-29 D2, text per DEC-33).

**Not in scope:**
- The reject strings and check order of `ContextualCheckBlockHeader` (CS3). CS2 gives CS3 the predicate.
- Header acceptance and the block index (CH1). CS2 reads the chain through CS0's `ChainAncestry`.
- Payee selection and assembly of the expected `CTxOut`s (`GetBlockTxOuts`, `GetMNPayee`): dash-evo (EV). CS2 provides the amounts.
- `IsBlockValueValid`, `IsBlockPayeeValid`, superblock triggers and the credit pool (EV/GV). CS2 provides `superblock_payments_limit` and `platform_share`.
- Chain-work arithmetic (`GetBlockProof`): base-sdk's `CompactTarget::block_proof`, with BS1 vectors.
- X11 (`dash-pow`, BS15).

---

## 2. Floating point (`fp`)

### 2.1 What the reference is (report §0, §3, §4)

| Site | What every shipped build computes | Reference for the port |
|---|---|---|
| Subsidy, `validation.cpp:1809-1870` | `ConvertBitsToDouble`: one `÷`, then a loop of `×256` (or `×2⁻⁸`), each rounded. Then `+`, `÷`, the square as `x·x`, `÷`, and truncation to `int64`. No libm, no FMA, no x87 (all 12 builds) | **exactly that operation sequence** (IEEE binary64, round to nearest even) |
| Tolerance, `validation.cpp:4476-4483` (v23.1.8: 4110-4117) | `fabs(round(n1 − n2)) > n1·0.5`, with `n1` and `n2` from `ConvertBitsToDouble`, false on NaN (all 12 builds) | exactly that, written literally (§2.3) |
| KGW, `pow.cpp:49-62` (same lines in v23.1.8) | `ratio = fl(T/A)`; `p = pow(fl(m/28.2), fl(−1.228))`; `d = 1 + 0.7084·p`; `s = 1/d`; break iff `ratio ≤ s ∨ ratio ≥ d`. `pow` comes from the host glibc on Linux (per x86_64 ifunc), a static mingw-w64 x87 routine on win64 (precision-control dependent), or Apple's libm on macOS. `d` is rounded separately on x86_64 Linux, win64 and x86_64 macOS, and fused on aarch64, riscv64 and arm64 macOS | **the break decision**, which is the same function of `(m, A)` for every build (report §4.2) |
| `pastSecondsMin = 86400 × 0.025` | rounds to exactly 2160.0 on every build | the constant computation, kept in `fp` |

### 2.2 Rules for `fp`

1. **Only IEEE basic operations.** `+ − × ÷`, comparisons, `as f64` from integers below 2⁵³ (exact), `as i64` from finite values in range, and `f64::from_bits` and `to_bits`. **No libm calls anywhere in `dash-consensus`, `fp` included.** That means no `powf`, `powi`, `exp`, `ln`, `sqrt` or `mul_add`, and no float intrinsics. Rust never contracts `a*b + c` into an FMA, so separate operations stay separate.
2. **Core's order, literally.** Each function mirrors one Core expression, with a `// Core: <file>:<line>` anchor at both pins. There is no algebraic simplification: the `÷256` loop stays a loop and is not an `ldexp`, `x·x` keeps Core's grouping, and the tolerance keeps `!(x > y)` (§2.3).
3. **Target guard.** `fp` refuses to compile where `f64` might not be IEEE binary64 with round-to-nearest. Concretely, that is 32-bit x86 without SSE2, the only Rust targets that use the x87 unit. Tier-1 and tier-2 targets (DEC-08) are unaffected:
   `#[cfg(all(target_arch = "x86", not(target_feature = "sse2")))] compile_error!(…)`
4. **The floating-point environment is a stated assumption.** The FP control state is per thread: MXCSR on x86_64, FPCR on aarch64. `fp` assumes every validating thread runs with IEEE defaults: round to nearest even, no flush-to-zero and no denormals-are-zero. Rust never changes them; a C dependency could, through `fesetround` or a `-ffast-math` `crtfastmath.o` constructor.
   - Flush-to-zero and denormals-are-zero cannot change an admissible result, because no subnormal arises on the admissible domain (§2.5 F2, F3). The first subnormal `dDiff` needs size byte 156 (0x9c).
   - A changed rounding mode would change results. Follow-up for ND: a startup self-check in `rdashd` that a rounding-sensitive `fp` expression returns its expected bits, on the main thread and on a validation-pool thread.
5. **`f64` does not escape `fp`.** Outside `fp`, `f64` values travel only inside opaque newtypes (`Difficulty`) or not at all: `fp`'s public API returns `bool` and `i64`.

### 2.3 API

```rust
// dash-consensus/src/fp/mod.rs: the only f64 code in dash-consensus (A1 §4.5).

/// Core `ConvertBitsToDouble`'s value, or the pre-4501 mainnet variant. Opaque: f64 stays in `fp`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Difficulty(f64);

impl Difficulty {
    /// Core: validation.cpp:1784 ConvertBitsToDouble (v23.1.8: 1458). Uses all 24 low bits
    /// (`nBits & 0x00ffffff`, sign bit included), unlike SetCompact's 23-bit word.
    pub fn from_bits(bits: u32) -> Self;
    /// Core: validation.cpp:1814-1816 (v23.1.8: 1488-1490): mainnet, nPrevHeight <= 4500: 65535.0 / (bits & 0xffffff), no exponent.
    pub fn from_bits_mainnet_early(bits: u32) -> Self;
    /// `dDiff <= 75` (validation.cpp:1837; v23.1.8: 1511), the formula-2 era test.
    pub fn at_most_75(self) -> bool;
    /// trunc(1111.0 / ((d + 1.0)²)), before clamping (validation.cpp:1834; v23.1.8: 1508).
    pub fn subsidy_base_early(self) -> i64;
    /// trunc(11111.0 / (((d + 51.0) / 6.0)²)) (validation.cpp:1840; v23.1.8: 1514).
    pub fn subsidy_base_cpu(self) -> i64;
    /// trunc(2222222.0 / (((d + 2600.0) / 9.0)²)) (validation.cpp:1846; v23.1.8: 1520).
    pub fn subsidy_base_gpu(self) -> i64;
}

/// Core validation.cpp:4479-4482 (v23.1.8: 4113-4116), the mainnet <= 68589 nBits rule.
/// Written literally as `!((n1 - n2).abs() > n1 * 0.5)`: true (accept) on NaN, and on
/// `n1 = +inf`, exactly as Core (§2.5 F3). A `<=` rewrite would differ on NaN.
pub fn within_pre_dgw_tolerance(header_bits: u32, expected_bits: u32) -> bool;

/// KGW's PastBlocksMass, 1..=4032: the domain of `pow` (report §4.1). Construction checks the range.
#[derive(Clone, Copy)] pub struct KgwMass(u16);
impl KgwMass { pub fn new(m: u64) -> Option<Self>; }

/// Core pow.cpp:49-62 (both pins): does KGW stop at this mass? `target_secs = spacing * mass`;
/// `actual_secs` is PastRateActualSeconds after Core's clamp to >= 0.
pub fn kgw_horizon_crossed(mass: KgwMass, target_secs: i64, actual_secs: i64) -> bool;

/// Core pow.cpp:28: `uint64_t(nPowTargetTimespan * 0.025)`.
pub fn kgw_past_seconds_min(target_timespan: i64) -> u64;
```

Each subsidy base returns the truncated `i64` before clamping, as Core's implicit `double → CAmount` conversion does (quirk 11: truncation before clamping). The clamps are integer code in `subsidy`.

`as i64` saturates and maps NaN to 0, where C++ has undefined behaviour. For **every** `u32` input the three bases lie in [0, 1111] and are never NaN (Opus n-2):
- `dDiff` is ≥ 0, or +∞ for a zero mantissa, which gives a base of 0;
- so each square is ≥ 1, and each quotient lies in [0, 1111].

So the saturating cast equals `cvttsd2si`, `fcvtzs` and `fcvt.l.d … rtz` everywhere. The exhaustive sweep (§6.3) asserts it on all 2³² inputs.

`kgw_horizon_crossed`, line by line against Core:
```rust
let ratio = if actual_secs != 0 && target_secs != 0 { target_secs as f64 / actual_secs as f64 } else { 1.0 };
let p = f64::from_bits(KGW_POW[usize::from(mass.0) - 1]);   // replaces pow(double(m)/double(28.2), -1.228)
let fast = 1.0 + 0.7084 * p;                                  // separate rounding (x86_64 Linux, win64, x86_64 macOS)
let slow = 1.0 / fast;
ratio <= slow || ratio >= fast
```

### 2.4 KGW's `pow`: the table and the proof

**Content.** `KGW_POW[m−1]` is the bit pattern of the **correctly rounded** `pow(fl(m/28.2), fl(−1.228))`, for m = 1..=4032. MPFR computes it through gmpy2 (`CS2-fp-evidence/out/pow-tables/cr.txt`). That covers all 4032 inputs the call can receive. Only m ≥ 14 affects a decision, but all are kept so that indexing mirrors Core.

**Why a table, and why these values.**

| Option | Verdict |
|---|---|
| Port a `pow` (A1 v1; "`fp::pow`" in PLAN) | Rejected. There is no single `pow` to port: the shipped builds use at least four tables (glibc FMA, glibc SSE2, mingw-w64 at two x87 precisions) plus Apple's libm and glibc's FMA4 variant, under two roundings. A 300-line port with tables is a large review surface that buys nothing over the 4032 values it would compute |
| Call the target libm (`f64::powf`) | Rejected. It makes consensus depend on each platform's libm. The tolerance argument would cover any accurate libm, but the port should not depend on one when a table removes the dependency |
| Integer break thresholds (`A_fast[m]`, `A_slow[m]`) | Equivalent, and it would make KGW float-free. Rejected because the code would no longer read like Core's, and the thresholds are derived data one step further from the source |
| **Correctly rounded `pow` table, Core's expression otherwise verbatim** | **Chosen** (DEC-29 D1, DEC-33). The one libm call is replaced by its exact mathematical value on the whole domain; the rest of the expression is Core's. The table is platform-neutral by construction |

The table stores `p`, not `d`, so that `1 + 0.7084·p` stays visible in the code as Core writes it. The separate rounding is the one written down; the fused rounding is covered by the proof.

**Provenance and regeneration.** `contrib/node/fp/gen-kgw-pow.py` (gmpy2/MPFR) writes `dash-consensus/src/fp/kgw_pow.rs`. The file's header records:
- the generator command, and the MPFR and gmpy2 versions;
- the sha256 of the value list (equal to `cr.txt`'s third column);
- a pointer to the report.

The generator asserts that each entry equals MPFR's 53-bit round-to-nearest value, and that a second evaluation at 256 bits rounds to the same double. This is derived data, not a Core fixture. Its equivalence with Core is proven by the tests below, two of which run Core's own code through the oracle (A1 §4.3 provenance rule).

**Fixtures.** The measured shipped tables are checked in under `dash-consensus/tests/data/kgw_pow/`, with `PROVENANCE.md` copied from the evidence:

| Fixture | Where it ships |
|---|---|
| `glibc-fma.txt` | glibc x86_64 FMA ifunc = glibc aarch64 = glibc riscv64 |
| `glibc-sse2.txt` | glibc x86_64 SSE2 ifunc |
| `mingw-win64-pc64.txt`, `mingw-win64-pc53.txt` | win64 `dashd.exe`'s own routine at x87 control words `0x37F` and `0x27F` |

This is A1 §4.5's "release binary's own routine" leg, made reproducible.

**Tests (all in `fp`, every PR):**
1. **`kgw_decisions_invariant_under_pow_error`: the primary proof, platform-independent.**
   - For each m ∈ 14..=4032, the break thresholds `A_fast(m)` and `A_slow(m)` are found by bisection over `a ∈ [1, 2³²−1]`, using IEEE division (the method of report §4.2).
   - Let `T(m)` be the implementation's thresholds (`KGW_POW` with separate rounding).
   - For both roundings, the test requires the thresholds at `p_lo = fl(p·(1−10⁻⁹))` and `p_hi = fl(p·(1+10⁻⁹))` to equal `T(m)`. Each endpoint is moved 4 ulps outward through `to_bits`, so the interval contains every double within relative error 10⁻⁹ of the exact `pow`.
   - Both roundings are non-decreasing in `p`, `s = fl(1/d)` is non-increasing, and the thresholds are monotone in `d` and `s`. So equality at the two endpoints proves it for every `p` between them.
   - The test therefore proves that **any `pow` within 10⁻⁹ reproduces Core's decisions, separate or fused.** That covers every shipped build:
     - measured tables: worst 5 ulps;
     - glibc FMA4: documented 0.54 ulp;
     - Apple libm: published 0.757 ulp on arm64; x86_64 per the stated assumption (report §4.3).
   - The fused rounding `fl(1 + 0.7084·p)` is computed exactly in integer arithmetic (a `u128` product of the significands, alignment, round-half-even), since `mul_add` is banned. A test-only cross-check compares that helper with `mul_add` (`#[expect(clippy::disallowed_methods)]`).
   - It runs in well under a second; `evidence/tools/kgw_proof.py` is the reference implementation.
2. **`kgw_shipped_tables_reproduce_decisions`.** Every fixture, under the rounding it ships with, has thresholds `T(m)` at every mass:
   - `glibc-fma`: separate (x86_64 Linux) and fused (aarch64, riscv64);
   - `glibc-sse2` and both `mingw` tables: separate.
3. **`kgw_horizon_matches_thresholds`.** For every m, `kgw_horizon_crossed(m, 150m, a)` equals the threshold predicate at `a ∈ {0, A_fast, A_fast+1, A_slow−1, A_slow}`.
4. **`kgw_host_libm_is_a_known_variant`** (`cfg(all(target_os = "linux", target_env = "gnu"))`). Extra evidence that **asserts which variant ran** (Opus M-2):
   - It computes the host table with `f64::powf` (`#[expect]`) and derives the expected fixture:
     - aarch64 and riscv64: `glibc-fma`;
     - x86_64 with FMA and AVX2 usable and `GLIBC_TUNABLES` not masking them: `glibc-fma`;
     - x86_64 when masked or absent: `glibc-sse2`;
     - x86_64 with FMA4 only: no fixture, so it asserts only the 10⁻⁹ interval.
   - It fails if the host table is not the expected one. So a tunable spelling the host glibc ignores (`-AVX2_Usable` on 2.33+, `-AVX2` on 2.31) fails loudly instead of measuring the FMA variant twice.
   - CI runs it twice: by default, and with `GLIBC_TUNABLES=glibc.cpu.hwcaps=-AVX2,-FMA`.

rev0's `kgw_margin_floor` (≥ 10,000 ulps of `d`) is withdrawn. Test 1 states the floor in the claim's own units: relative `pow` error 10⁻⁹, against the measured tolerance of 1.534×10⁻⁹.

**Differential (L1, against Core's compiled code with the host glibc).** `diff_next_work` builds mainnet header windows that steer KGW to break exactly at mass m:
- with `a = A_fast(m)` (break) and `A_fast(m)+1` (no break);
- likewise around `A_slow`;
- for every m ∈ 14..=4032.

The generator has two invariants (Opus m-3):
- **Non-monotonic timestamps are intended.** Earlier masses are held below the horizon with blocks exactly 150 s apart, which gives `ratio = i/(i−1) < d(i)` for every `i` (checked; smallest gap 0.00135, at m = 4032). A fast-side break at mass m then needs `A_fast(m) < 150(m−2)`, so the m-th block back is newer than the (m−1)-th. The generator documents this and must not be "fixed" to monotonic times.
- **Each pair must be non-vacuous.** The generator asserts that the oracle's `nBits` differ between `A_fast(m)` and `A_fast(m)+1`, and between `A_slow(m)−1` and `A_slow(m)`.

The run is repeated under the SSE2 tunable. Before each run, the runner computes the in-process host table and asserts which glibc variant the oracle is using, as in test 4.

### 2.5 The admissible domain

A1 §4.5's gate: the implementation equals the reference on every input a valid supported chain can reach. Inputs where Core's behaviour is undefined, or where Core aborts, are excluded, each with its reason. Recorded history is validation, not a domain argument (GPT M1).

**F1. KGW (all reachable inputs).**
- Reachable only on mainnet at heights 15200..34139:
  - testnet and devnet have `nPowKGWHeight = nPowDGWHeight`, and no override changes it;
  - regtest returns early on `fPowNoRetargeting` (`pow.cpp:187`).
- Inputs: `m ∈ 1..=4032`, `A ∈ [0, 2³²−1]` (a difference of two `uint32` times, clamped ≥ 0), and spacing 150. Header `nBits` reach only the integer average.
- **Closed:** §2.4 test 1 covers every `(m, A)` through the thresholds, for every `pow` within 10⁻⁹. There are no exclusions.
- This includes headers on forks below the checkpoints, which `AcceptBlockHeader` evaluates before checkpoint rejection.

**F2. Subsidy (`Difficulty` and the three formulas).** The floating-point inputs are:
- `prev_bits`: any compact a valid previous header can carry, or the artificial `1` that `GetPaymentsLimit` passes on mainnet;
- the formula and `dDiff` path that the height and network select.

*Valid compacts.* `CheckProofOfWork` accepts a header iff its decoded target is non-zero, not negative, not overflowing and ≤ powLimit. Nothing requires the encoding to be canonical, and Core stores and reads the raw word (GPT M2). That allows:
- a size byte of 1..34 (1..32 under the mainnet/testnet powLimit 2²³⁶−1, where e.g. `0x20000001` decodes to 2²³²);
- a mantissa `nBits & 0xffffff` of 1..0x7fffff (the sign bit is clear on a valid compact).

*Numerical proof.*
- For those inputs, `65535/mantissa ∈ [7.8×10⁻³, 65535]`. At most 28 `×256` or 5 `÷256` steps follow, so `dDiff ∈ [7.1×10⁻¹⁵, 2.7×10⁶⁷]`. With `nBits = 1` it is 4.52×10⁷⁴. All of these are normal and finite.
- Then `d + 1 ≥ 1`, `(d + 51)/6 ≥ 8.5` and `(d + 2600)/9 ≥ 288.9`. So every square is positive and finite (≤ 2.1×10¹⁴⁹), every quotient lies in (0, 1111], and the truncation is exact.
- The port performs the same IEEE operations in the same order as every shipped build (report §3.1), so it is bit-identical on this whole domain.

The all-`u32` sweep (§6.3) checks this against Core's compiled code. It covers zero mantissas, sign bits and size bytes 0 and 35..255 as well, and it is a **phase C merge gate**.

*Raw bits matter.* Non-canonical encodings of one target have the same `ConvertBitsToDouble` value but different early subsidies. `0x1e010000`, `0x1f000100` and `0x20000001` all decode to 2²³², and all give `0x1.fffe000000000p-9`. Yet at mainnet `prev_height ≤ 4500`, the no-exponent path gives a base of **277 DASH** for `0x1e010000` and **1 DASH** for the other two. So nothing in the node canonicalizes header bits: `HeaderCtx.bits` is the raw word, and every consumer reads it (§3.2).

*Which formulas each network reaches* (dispatch facts; no longer needed for closure, kept for targeted tests):

| Network | Pre-V20 inputs that reach the formulas |
|---|---|
| mainnet | blocks `h ∈ 1..1987775`: `(prev.nBits, h−1)`, with the `≤ 4500` path for `h−1 ≤ 4500`; superblock limit at `631408 + 16616k < 1987776`: `nBits = 1`, formula 3 |
| testnet | blocks `h ∈ 1..905099`; superblock limit at `4200 + 24k < 905100`: `nBits = 0x1e0fffff`, formulas 1–3 by height |
| regtest | every block has `nBits = 0x207fffff` (induction below). Pre-V20 heights reach all three formulas, because `-testactivationheight=v20@h` moves V20 to any `h ∈ [DIP0003Height, INT_MAX−1]` (`chainparams.cpp:1050,964`). The superblock limit uses the same `nBits` |
| devnet | `V20Height = 2` is fixed. Only block 1 is pre-V20 (prev = genesis `0x207fffff`, formula 1 when `nHighSubsidyBlocks > 0`). Superblock heights ≥ 4200 are V20 |

*Why regtest is always `0x207fffff`:*
- at `h < 15200` the BTC rule with min-difficulty either returns powLimit or walks back to the genesis `nBits`, and at a retarget boundary `fPowNoRetargeting` returns `last.nBits`;
- at `h ≥ 15200` it returns powLimit;
- the genesis `nBits` is `GetCompact(7fff…ff) = 0x207fffff`.

Induction over the height completes the proof.

**F3. Tolerance.**
- The header's `nBits` passed `CheckProofOfWork` on mainnet, so its size byte is ≤ 32 (non-canonical words included). `ConvertBitsToDouble` then takes at most three `÷256` steps, and `n1 ∈ [2.4×10⁻⁴, 2.7×10⁶⁷]`.
- The expected `nBits` comes from one of:
  - `GetCompact` (canonical, size ≤ 30);
  - the previous header's raw word, which the BTC rule returns between retargets and the min-difficulty walk-back returns (the same bounds as the header side);
  - a zero target, whose zero mantissa gives `n2 = +∞` and then `|n1 − ∞| > n1·0.5`, a rejection.
- Where `CheckProofOfWork` did not run, as for `getblocktemplate` proposals through `TestBlockValidity(fCheckPOW = false)` (Opus m-1), a zero-mantissa header gives `n1 = +∞`:
  - `∞ > ∞·0.5` is false, so the header is **accepted**;
  - with `n2 = +∞` too, the difference is NaN, which is also accepted.

  The literal `!(x > y)` form reproduces both. A `<=` form would not.

  Without PoW the size byte is unconstrained, and from 156 up `n1` would be subnormal. A header reaches this rule unchecked only through a proposal on a mainnet node whose tip is below 68589 and which is not in initial block download. Tips that old count as IBD unless `-maxtipage` is raised. Under the default floating-point environment (§2.2 rule 4) the literal form mirrors Core there too.

With PoW checked, no subnormals arise anywhere in F3. Coverage: §6.2 `diff_nbits_acceptable`, and the exhaustive `ConvertBitsToDouble` sweep (§6.3).

**Excluded, with reasons:**

| Input | Core's behaviour | Treatment in the port |
|---|---|---|
| Devnet `-powtargetspacing > 86400` **while the BTC rule is reachable** (`-minimumdifficultyblocks < nPowKGWHeight − 1 = 4000`) | `DifficultyAdjustmentInterval() = 86400/spacing = 0`, so `(h+1) % 0` divides by zero (SIGFPE) at the first header that reaches BTC (`pow.cpp:140`) | `PowParams::new` rejects that combination (§3.1) |
| Devnet `-powtargetspacing > ⌊i64::MAX / 72⌋` | DGW's `nTargetTimespan*3` overflows `int64` (undefined behaviour) | `PowParams::new` rejects it |
| `-powtargetspacing < 1` | rejected at startup (`chainparams.cpp:1363`) | rejected |
| `nPrevHeight < nHighSubsidyBlocks` off devnet | `assert(isDevnet)` aborts | unreachable: `SubsidyParams::new` requires `high_subsidy_blocks == 0` off devnet |
| `prev_height ≥ ⌊INT_MAX / interval⌋ · interval` | the decline loop's `i += interval` overflows `int`, which is undefined behaviour (Core's actual boundary; O1's guard). The x86_64 release binary keeps a wrapping 32-bit counter (`add esi,edi; cmp; jge` at `0x719f60`), and for some heights with an even interval the loop never terminates (Opus n-1) | excluded as undefined behaviour, since there is no reference; reachability is not the argument. The port returns the decline's fixed point, and the oracle refuses these inputs |
| Negative `prev_height` (only `GetPaymentsLimit(0)`, reachable from RPC) | aborts in `assert(isDevnet)` off devnet | `Err(SubsidyError::Unreachable)` |
| Regtest `-budgetparams` with a negative budget or superblock start (DEBUG_ONLY) | `ParseInt32` with no range check (`chainparams.cpp:1166-1186`, v23.1.8 `:1126-1146`). A budget start of −1 makes `nPrevHeight > nBudgetPaymentsStartBlock` (`validation.cpp:1865`, v23.1.8 `:1539`) true at `nPrevHeight = 0`, so block 1 pays a treasury part, which a start of 0 does not. A negative superblock start acts like 0 at every reachable height | excluded: `dash-params` types both starts `BlockHeight` (`u32`), so such a chain cannot be expressed. **BS5's `ChainParamsBuilder` must refuse negative `-budgetparams` values, never clamp them to 0**: clamping silently changes block 1's split (phase C's answer). BS5 may instead carry `i32`, and this row then becomes admissible (review CS2ab-r2 m4) |

These are **admissible, not excluded:**
- **Devnet `-powtargetspacing > 86400` with `-minimumdifficultyblocks ≥ 4000`** (BTC never reached). DGW and the min-difficulty rules use the spacing directly. DGW's `uint32` truncation in `bnNew *= nActualTimespan` becomes live once the lower clamp `8·spacing` reaches 2³² (spacing ≥ 2²⁹ = 536,870,912). It is ported exactly (§3.6).
- **Any `-highsubsidyfactor`** (Opus m-4). Core accepts any `int`. For `factor > ⌊i64::MAX/(500·COIN)⌋ = 184,467,440`, devnet block 1's `nSubsidy *= factor` overflows (undefined in C++), and both Linux release binaries wrap (`imul`; x86_64 at `0x719f93`). The port accepts every factor and uses `wrapping_mul`, which is the binaries' result. There is no startup deviation. Phase B already implements this.

### 2.6 Lints

- `dash-consensus/clippy.toml` (CS0 §2) gains `disallowed-methods`:
  - `f64::{powf, powi, exp, exp2, exp_m1, ln, log, log2, log10, ln_1p, sqrt, cbrt, mul_add, hypot, sin, cos, tan}`;
  - the same methods on `f32`.

  This implements the no-libm rule; tests 1 and 4 of §2.4 opt out with `#[expect]`.
- `#![deny(clippy::float_arithmetic)]` crate-wide, `#[allow]`ed only on `mod fp`.
- `contrib/node/check-fp-confinement.sh` (CI, `node-pure` group). Clippy cannot ban primitive types, so this script runs `rg -n -w 'f32|f64' dash-consensus/src --glob '!**/fp/**'` and requires empty output. GPT m1 found that rev0's `--glob '!fp/**'`, run from the repository root, does not exclude `dash-consensus/src/fp/`, so the gate would also flag `fp` itself. The script carries its own self-test: an `f64` under `fp/` must be excluded, and one outside must be reported.

  *Implemented as* `dash-consensus/tests/fp_confinement.rs`, which `cargo test` runs. An `rg` pattern misses inferred floats (`x <= 75.0`), and a line scanner let floats out of `fp` through trait items, one-line enums and `1_000f64` (reviews CS2ab-r1 m3, CS2ab-r2 m1). The test reads the source with Rust's own lexer and parser (`proc-macro2`, `syn`, dev-dependencies only):
  - outside `src/fp/`, no token may name `f32`/`f64`, be a float literal, or name a `…_f32`/`…_f64` item;
  - inside it, no signature, type, where clause, derive, variant or field the crate can see, no trait definition the crate can see, no trait impl's header or item signature (including impls inside bodies and const values), no type alias, no `use … as` renaming a float and no item-level macro may name a float, or `PartialOrd`/`Ord` (§2.3: `Difficulty` derives only `PartialEq`; float decisions are `fp` functions).

  Its self-test covers both directions, including every escape the reviews demonstrated. Macros are not expanded.

These close A1 §4.5's lint scope for `dash-consensus`. The IEEE allowlist A1 names (governance rate, superblock time estimate, DKG sleep, fee estimator, CoinJoin fees, RPC difficulty) lives in other crates.

---

## 3. `pow`

### 3.1 Parameters

`dash-params` at 33fcc59 has every field below except the devnet ones and the mainnet tolerance pin; BS5 adds those. Until then, CS2 defines narrow views. Each is a `Copy` struct with a validating constructor, built from `&dash_params::ChainParams` plus a `DevnetOverrides` (zero, zero and one by default).
- The tests build them from the oracle's `oracle_chainparams_json`, which is Core-produced.
- When BS5 lands, `from_chain_params` reads the devnet fields directly, and the function signatures do not change.
- BS5 must reproduce `GetIntArg`'s truncation of the three devnet arguments to `int` (`chainparams.cpp:1251-1253`), so `DevnetOverrides` receives the truncated `i32` (Opus m-4).
- BS5 must refuse a negative regtest `-budgetparams` start rather than clamp it (§2.5, excluded inputs).
- `from_chain_params` refuses a `network_id` that is not one of Core's four `NetworkIDString()`s (`ParamsError::UnknownNetwork`); with BS5's `NetworkKind` the match moves onto it.

```rust
pub struct PowParams {
    pub pow_limit: Arith256, pub pow_limit_bits: CompactTarget,     // powLimit, GetCompact(powLimit)
    pub target_spacing: i64, pub target_timespan: i64,              // nPowTargetSpacing (devnet -powtargetspacing), nPowTargetTimespan
    pub kgw_height: i32, pub dgw_height: i32,                       // nPowKGWHeight, nPowDGWHeight
    pub allow_min_difficulty_blocks: bool, pub no_retargeting: bool,
    pub minimum_difficulty_blocks: i32,                             // devnet -minimumdifficultyblocks
    pub pre_dgw_tolerance_until: Option<i32>,                       // Some(68589) iff mainnet (validation.cpp:4476, NetworkIDString()==MAIN)
}
```

`PowParams::new` enforces (§2.5):
- `1 ≤ target_spacing ≤ ⌊i64::MAX / 72⌋`;
- `target_spacing ≤ target_timespan` whenever the BTC rule is reachable (`minimum_difficulty_blocks < kgw_height − 1`);
- if KGW is reachable (`kgw_height < dgw_height && !no_retargeting`), `7·timespan / spacing ≤ 4032`, so `KgwMass` always fits. Only mainnet reaches KGW, and it has exactly 4032.

Hard-coded Core constants stay literal, each with its anchor: 7200 s (`2*60*60`, `pow.cpp:193`), the DGW window of 24, KGW's 0.025, ×7, 28.2, 0.7084 and −1.228, and the ×10 min-difficulty factor. The 68589 pin is a network value, so it is a parameter (A1 rule "no hardcoded network parameters"); BS5 moves it into `dash-params` with the other hard-coded pins.

### 3.2 API

```rust
/// Core pow.cpp:170 GetNextWorkRequired. `new_time` is the candidate's nTime, the only field Core reads from pblock.
pub fn get_next_work_required(chain: &dyn ChainAncestry, last: BlockId, new_time: u32, p: &PowParams) -> CompactTarget;

/// Core pow.cpp:239 CheckProofOfWork. `hash` in internal (little-endian) byte order.
pub fn check_proof_of_work(hash: &[u8; 32], bits: CompactTarget, p: &PowParams) -> bool;

/// Core validation.cpp:4476-4489, the nBits predicate of ContextualCheckBlockHeader (CS3 owns the reject).
pub fn nbits_acceptable(height: u32, header_bits: CompactTarget, expected: CompactTarget, p: &PowParams) -> bool;
```

**`check_proof_of_work`, exactly** (GPT M2):
1. `let t = CompactTarget::new(bits).expand()`.
2. Reject if `t.negative`, `t.value == 0`, `t.overflow`, or `t.value > p.pow_limit`. These are numeric comparisons of the decoded target.
3. Otherwise accept iff `Arith256::from_lendian(*hash) <= t.value`, so `hash == target` passes and `target + 1` fails.

`check_proof_of_work` never compares the raw word with `GetCompact(powLimit)` and never re-encodes it. (The BTC min-difficulty walk-back does compare raw words with the powLimit compact, exactly as Core does; §3.4.) **Header bits are never canonicalized.** `HeaderCtx.bits` holds the raw word, and every consumer reads it raw:
- the subsidy's `dDiff`;
- the tolerance's `n1`;
- `get_next_work_required`, which returns `last.bits` or a walked-back block's `bits` unchanged.

`ChainAncestry`, `BlockId` and `HeaderCtx` are CS0's (§8.4 there). `get_next_work_required` reads `header(id).{height, time, bits}` and walks the chain with `prev`, plus `ancestor` for the BTC retarget's `GetAncestor`.

### 3.3 Dispatch (`pow.cpp:170-211`), in Core's order

1. `last.height < minimum_difficulty_blocks` → powLimit compact.
2. `last.height + 1 < kgw_height` → BTC (§3.4).
3. `no_retargeting` → powLimit compact.
4. `allow_min_difficulty_blocks`:
   - `new_time > last.time + 7200` → powLimit compact;
   - `new_time > last.time + spacing·4` → `t = expand(last.bits).value * 10u32`, which **wraps mod 2²⁵⁶** (quirk 8, devnet); then `t > pow_limit ? powLimit compact : t.compact(false)`.

   Times are compared as `i64`, as Core's `GetBlockTime()` does.
5. `last.height + 1 < dgw_height` → KGW (§3.5); otherwise DGW (§3.6).

### 3.4 BTC (`pow.cpp:134-237`)

Let `interval = timespan / spacing` (576 at the default spacing).

**Not a retarget height** (`(last.height+1) % interval != 0`):
- With min-difficulty: if `new_time > last.time + 2·spacing` (300 s, not Bitcoin's 20 min), return powLimit compact. Otherwise walk back while `prev` exists, `height % interval != 0` and `bits == powLimit compact`, and return that block's **raw** `bits`.
- Without min-difficulty: return `last.bits` (raw).

**Retarget height:** `first = ancestor(last, last.height − (interval−1))`, then `CalculateNextWorkRequired`:
- `no_retargeting` → `last.bits`;
- otherwise `actual = clamp(last.time − first.time, timespan/4, timespan·4)`;
- `bn = expand(last.bits).value * (actual as u32)`, then `/ (timespan as u64)`, capped at `pow_limit`, then `.compact(false)`.

### 3.5 KGW (`pow.cpp:15-79`), integer part

- `past_seconds_min = fp::kgw_past_seconds_min(timespan)`, which is 2160.
- `past_blocks_min = past_seconds_min / spacing` (14); `past_blocks_max = 7·timespan / spacing` (4032). All are `u64`, as in Core.
- If `last.height == 0` or `last.height < past_blocks_min`, return powLimit compact.

**Loop:** `i = 1, 2, …` while `reading.height > 0`:
- If `i > past_blocks_max`, break **before** updating anything. The last iteration's `actual` and `target` survive, as in Core.
- `mass += 1`.
- `avg_i = expand(reading.bits).value`. For `i > 1`:
  - if `avg_i ≥ prev_avg`: `avg = (avg_i − prev_avg) / i + prev_avg`;
  - else: `avg = prev_avg − (prev_avg − avg_i) / i`.

  Both are exact unsigned operations with no wrap, because the operands are ordered. Then `prev_avg = avg`.
- `actual = max(0, last.time − reading.time)` (as `i64`); `target = spacing·mass` (`u64` arithmetic, assigned to `i64`, as in Core).
- If `mass ≥ past_blocks_min` and `fp::kgw_horizon_crossed(KgwMass::new(mass), target, actual)`, break. `KgwMass::new` cannot return `None` here (next paragraph).
- If `prev` is `None`, break. Otherwise `reading = prev`.

**After the loop:**
- `bn = avg`; if `actual ≠ 0 && target ≠ 0`: `bn = bn * (actual as u32)`, then `/ (target as u64)`.
- Cap at `pow_limit`, then `.compact(false)`.

`KgwMass::new` cannot fail inside this loop, because `mass ≤ past_blocks_max ≤ 4032` by `PowParams`. Where it would fail, the code takes Core's unreachable branch as described in §3.8.

### 3.6 DGW (`pow.cpp:81-132`)

- If `last.height < 24`, return powLimit compact.
- **Window** of k = 1..=24 over `last, last−1, …, last−23`:
  - `t = expand(bits).value`;
  - for k = 1, `avg = t`;
  - otherwise `avg = (avg * k + t) / (k + 1)`, where `* k` is `Mul<u32>` and **wraps**, `+` wraps, and `/` is `Div<u64>`. That is per-step truncation (quirk 8).
- `actual = last.time − window_first.time`. This spans **23 intervals** against a target of `24·spacing` (quirk 8).
- Clamp to `[target/3, target·3]` with integer division.
- `bn = avg * (actual as u32)`, then `/ (target as u64)`, capped at `pow_limit`, then `.compact(false)`.

The `as u32` truncation is dormant at standard spacing (`actual ≤ 10800`). On a devnet with `-powtargetspacing ≥ 2²⁹` and `-minimumdifficultyblocks ≥ 4000` it truncates for real (§2.5), exactly as Core's `operator*=(uint32_t)` does.

### 3.7 `arith_uint256` → `Arith256` (base-sdk `dash-num` 33fcc59, verified by reading)

| Core | Port | Note |
|---|---|---|
| `SetCompact(n)` (value) | `CompactTarget::new(n).expand().value` | sign bit masked out of the word; shifts ≥ 256 give 0 (`wrapping_shl`), as `base_uint::operator<<=` does. The underlying `bitcoin-internals` `U256::wrapping_shl` would mask the shift, and `Arith256` guards it (Opus §1(5)) |
| `SetCompact(n, &neg, &ovf)` | `.expand()` `{value, negative, overflow}` | the same predicates (`compact.rs:91-92`) |
| `GetCompact()` | `.compact(false)` | |
| `*= uint32_t`, `* uint32_t` | `* u32` (through `mul_u64(..).0`) | wraps mod 2²⁵⁶ |
| `*= int64` | `* (x as u32)` | **C++ picks `operator*=(uint32_t)`** (a standard conversion beats the user-defined one): modular truncation. Every call site is clamped first, except DGW on the large-spacing devnets of §2.5. The macOS symbols confirm the overload choice (`__ZN9base_uintILj256EEmLEj`) |
| `/= x`, `/ x` with an integer `x` | `/ (x as u64)` | Core resolves these to `base_uint& operator/=(uint64_t)` (v24 `arith_uint256.h:161`, `.cpp:77`; v23 `:163`, `.cpp:84`) and `friend operator/(const base_uint&, uint64_t)` (v24 `:211`, v23 `:213`), by standard conversion (GPT m2, Opus m-2). The member takes a limb-wise path for divisors ≤ `u32::MAX` and falls back to `/= base_uint(b)` above it; that fallback is live for DGW's `/= nTargetTimespan` on a devnet with `-powtargetspacing ≥ ⌈2³²/24⌉ = 178,956,971`. Both paths are exact floor division, as is base-sdk's `Div<u64>`. Divisors are never zero on any path |
| `+`, `-` | `+`, `-` (wrapping) | |
| `<`, `>`, `>=` | `Ord` (derived on `U256(high, low)`, so numeric) | |
| `UintToArith256(hash)` | `Arith256::from_lendian(hash)` | internal byte order |

### 3.8 Core's `assert`s

The `assert`s in `GetNextWorkRequired` and its callees are:
- `pindexLast` and `pblock` not null (guaranteed by the Rust types);
- `nHeightFirst ≥ 0` (the retarget height is ≥ 576);
- `pindexFirst` (`ancestor` is total for heights ≤ `last.height`);
- `pindex->pprev` in DGW (height ≥ 24 > 0);
- `BlockReading` in KGW.

All are structurally unreachable under `ChainAncestry`'s contract (`prev(id)` is `Some` iff height > 0). Following CS1's convention, nothing derived from input may panic. A `None` from `prev` where Core asserts is a broken contract: `debug_assert!`, then return the powLimit compact, Core's value for a chain too short to retarget. This is documented on the function. Real states can never reach it, and `diff_next_work` cannot generate it.

### 3.9 Performance (BG: `crit: next_work`)

- DGW: 24 window steps of 256-bit multiply-add-divide by a small divisor per header. That is parity with Core; header sync is dominated by X11 (B1 `spv-headers-*`).
- KGW: up to 4032 iterations (19k mainnet headers) of a 256-bit division by `i`, plus one table lookup and two divisions. Parity.
- The criterion bench `next_work` covers DGW (typical), KGW (a 4032-block window), BTC at a retarget, and the min-difficulty paths. PF0 counters guard it after merge.
- The only optimisation planned beyond Core is in `subsidy` (§4.2), and it is exact.

---

## 4. `subsidy`

### 4.1 Parameters and API

```rust
pub struct SubsidyParams {
    pub halving_interval: i32,            // nSubsidyHalvingInterval (> 0; 150 regtest, 210240 elsewhere)
    pub budget_payments_start: i32,       // nBudgetPaymentsStartBlock (regtest -budgetparams)
    pub high_subsidy_blocks: i32, pub high_subsidy_factor: i32,   // devnet only (0, 1 elsewhere); any int factor
    pub network: SubsidyNetwork,          // Main | Devnet | Other: Core's two NetworkIDString() checks (validation.cpp:1815, 1822)
    pub genesis_reward: Amount,           // GenesisBlock().vtx[0]->GetValueOut(), GetBlockSubsidy(genesis); never consensus
    // GetPaymentsLimit:
    pub superblock_start: i32, pub superblock_cycle: i32, pub v20_height: i32,
    pub limit_bits: u32,                  // fPowAllowMinDifficultyBlocks ? GetCompact(powLimit) : 1 (quirk 15)
}
pub struct SubsidySplit { pub block: Amount, pub treasury: Amount }   // Core's pair {nSubsidy - part, part}

/// Core validation.cpp:1809 GetBlockSubsidyHelper. `prev_bits` is the previous header's raw nBits.
pub fn block_subsidy_helper(prev_bits: u32, prev_height: i32, v20: bool, p: &SubsidyParams) -> Result<SubsidySplit, SubsidyError>;
pub fn block_subsidy_inner(…) -> Result<Amount, SubsidyError>;        // .block    (GetBlockSubsidyInner)
pub fn superblock_subsidy_inner(…) -> Result<Amount, SubsidyError>;   // .treasury (GetSuperblockSubsidyInner)
/// Core validation.cpp:1884 GetBlockSubsidy: prev = None for genesis. `v20_at_block` = DeploymentActiveAt(*pindex, V20).
pub fn block_subsidy(prev: Option<(u32 /*bits*/, u32 /*height*/)>, v20_at_block: bool, p: &SubsidyParams) -> Amount;
/// Core governance/superblock.cpp:145 GetPaymentsLimit (0 off superblock heights).
pub fn superblock_payments_limit(height: i32, p: &SubsidyParams) -> Result<Amount, SubsidyError>;
```

The only `Err` is `SubsidyError::Unreachable`, for the inputs where Core asserts (§2.5). Real chains never produce it.

### 4.2 Body, in Core's order (quirk 11)

1. `d = if network == Main && prev_height <= 4500 { Difficulty::from_bits_mainnet_early(prev_bits) } else { Difficulty::from_bits(prev_bits) }`.
2. `fixed = v20 || (network == Devnet && prev_height >= high_subsidy_blocks)`. Then:
   - fixed → base 5;
   - else if `prev_height < 5465` → `clamp_core(d.subsidy_base_early(), 1, 500)`;
   - else if `prev_height < 17000 || (d.at_most_75() && prev_height < 24000)` → `clamp_core(d.subsidy_base_cpu(), 25, 500)`;
   - else → `clamp_core(d.subsidy_base_gpu(), 5, 25)`.

   `clamp_core(x, lo, hi)` is `if x > hi { hi } else if x < lo { lo } else { x }`, Core's order.
3. `s = base * COIN`.
4. **Decline.** `for _ in 0..(prev_height / interval) { s -= s / 14 }` (zero iterations for `prev_height < interval`). This is exactly the number of passes Core's loop makes for every `prev_height` where Core is defined (§2.5).

   The loop stops early once `s < 14`. From there `s / 14 = 0`, so `s` is a fixed point and the remaining passes change nothing. `s ≤ 500·COIN` here (the devnet factor is applied after the loop), which bounds the work at about 300 passes, against up to 14M for regtest heights near 2³¹. It is exact, and a unit test compares it with the plain loop over the full range of regtest heights.
5. If `prev_height < high_subsidy_blocks`: `s = s.wrapping_mul(factor)`. Off devnet this is unreachable by construction. Any `int` factor is accepted; for factors above 184,467,440 the product wraps exactly as both release binaries' `imul` does (§2.5).
6. `treasury = if prev_height > budget_payments_start { s / (if v20 { 5 } else { 10 }) } else { 0 }`. This is a **strict `>`**, so mainnet's first treasury block is 328010.
7. Return `{block: s − treasury, treasury}`.

**`superblock_payments_limit(h)`:**
- 0 unless `h >= superblock_start && h % superblock_cycle == 0`;
- otherwise `superblock_subsidy_inner(limit_bits, h − 1, h >= v20_height) * superblock_cycle`. The product wraps like Core's binaries; only a devnet factor can make it overflow.

Quirk 15: `limit_bits` is 1 on mainnet and powLimit elsewhere, never the real difficulty.

---

## 5. `payments`

```rust
pub enum MnRewardEra { Classic, CreditPool, EvoReward }   // Core validation.cpp:5897 GetMnRewardEraAfter
impl MnRewardEra { pub fn after(v20_active_after: bool, mn_rr_active_after: bool) -> Self; }   // !V20 → Classic; !MN_RR → CreditPool; else EvoReward
pub struct PaymentParams { pub increase_block: i32, pub increase_period: i32, pub brr_height: i32, pub superblock_cycle: i32 }

/// Core masternode/payments.cpp:62 GetMasternodePayment (v23.1.8: validation.cpp:1565 with fV20Active = era != Classic).
pub fn masternode_payment(height: i32, block_value: Amount, era: MnRewardEra, p: &PaymentParams) -> Amount;
/// Core payments.cpp:54 PlatformShare: reward*375/1000; Core asserts MoneyRange → Err.
pub fn platform_share(mn_reward: Amount) -> Result<Amount, PaymentError>;
/// payments.cpp:164-168: (reward * bps) / 10000, only when bps != 0 and the operator script is set (the caller decides).
pub fn operator_reward(mn_reward: Amount, operator_bps: u16) -> Amount;
/// payments.cpp:181-190: (reward * bps_i) / MAX_REWARD for all but the last; the last gets the remainder.
pub fn owner_payout_amounts(reward: Amount, payout_bps: &[u16]) -> Vec<Amount>;
/// evo/providertx_util.cpp:88-117 ProRataFloor and SplitAmountByShares (u128 intermediates; the last share gets the remainder).
pub fn split_amount_by_shares(total: Amount, weights: &[Amount]) -> Result<Vec<Amount>, PaymentError>;
```

**`masternode_payment`** (quirk 12):
- `ret = bv/5`.
- Add `bv/20` if `h > ipb`, `h > ipb+P` and `h > ipb+2P`.
- Add `bv/40` if `h > ipb+kP` for k ∈ {3, 4, 5, 6, 7, **9**}; period 8 is skipped.

  Every increment is truncated on its own, so the "50%" step is not `bv/2`.
- If `h < brr`, return `ret`.
- `realloc_start = brr − brr % cycle + cycle`. If `h < realloc_start`, return `ret`.
- If `era != Classic`, return `bv*3/4`.
- Otherwise: `period = min((h − realloc_start) / (3·cycle), 18)`, and return `bv * TABLE[period] / 1000`, where TABLE is {513, 526, 533, 540, 546, 552, 557, 562, 567, 572, 577, 582, 585, 588, 591, 594, 597, 599, 600}.

v23.1.8 and v24.0.0-rc.3 differ only in the signature (`bool fV20Active` versus the era). The mapping is exact, because v23's platform-share path requires `fV20Active && MN_RR`, which is v24's `EvoReward`.

**Input bounds and overflow** (GPT M4). The caller passes `block_value = subsidy + fees`:
- **Fees:** `fees ∈ [0, MAX_MONEY]`, because `bad-txns-accumulated-fee-outofrange` keeps the running sum in `MoneyRange`. `MoneyRange(fees)` does not by itself bound the sum with the subsidy.
- **Subsidy:** at most 500·COIN on mainnet, testnet and regtest (the formula-1 clamp). On a devnet it can be `500·COIN·factor` at block 1 (Classic era, before BRR, so only divisions follow), and `5·COIN·decline·factor` (V20 and later) while `prev_height < nHighSubsidyBlocks`. The factor is any `int`, possibly negative.

The table below lists each product, when it can overflow, and what Core and the port then do. Where Core's C++ overflows, its behaviour is undefined, so the reference is the release binaries' machine semantics (both Linux architectures, both pins).

| Product | Can it overflow `i64`? | Core | Port |
|---|---|---|---|
| `masternode_payment`: `bv·permille` (≤ 600), `bv·3` | **No, on any admissible configuration.** The permille path runs only in the Classic era after BRR's realloc start. No devnet reaches it (V20 at height 2 makes every later block non-Classic), and the other networks have factor 1, so `bv ≤ MAX_MONEY + 500·COIN`. For the `·3` path, `bv ≤ 5·COIN·(2³¹−1) + MAX_MONEY ≈ 1.07×10¹⁸`, and `bv·3 < i64::MAX` | — | plain arithmetic; phase B's `wrapping_mul` is identical here |
| `platform_share`: `r·375`, then `/1000`, then `MoneyRange` | Yes, for `r > 2.46×10¹⁶` (devnet factor ≳ 6.6×10⁷). Below that, the share leaves `MoneyRange` for devnet factor ≳ 1.5×10⁷ | `assert(MoneyRange(…))` aborts when the quotient is outside the range. The binaries compute `imul …, 0x177` / `mul` (wrapping), then a magic-number signed ÷1000, then an unsigned compare with `MAX_MONEY` (x86_64 `0x9d3549` v24, `0x8b38bc` v23; aarch64 `0x83d680` v24, `0x73e170` v23). A wrapped product whose quotient lands inside the range is **accepted** | `mn_reward.wrapping_mul(375) / 1000`, then `Err(PaymentError)` iff outside `MoneyRange`. **Phase B note:** the branch currently uses `checked_mul`, which returns `Err` where the binaries can continue; switch to `wrapping_mul` |
| `operator_reward`, `owner_payout_amounts`: `r·bps` (bps ≤ 10000) | Yes, for `r > 9.22×10¹⁴` sat: ≥ 12.3M DASH of fees in one block (about the whole mainnet supply), or a devnet factor ≳ 2.5×10⁶ | undefined behaviour in C++. Both Linux release binaries compute a 64-bit wrapping product, then a magic-number signed ÷10000 (truncation toward zero). Listings: x86_64 `imul` at `0x9d3686`/`0x9d432f` (v24) and `0x8b3714` (v23); aarch64 `mul` at `0x83d8c0` (v24) and `0x73e020` (v23) | `wrapping_mul`, then `/ 10_000`: the binaries' result |

**`split_amount_by_shares`, domain and semantics:**
- Core's inputs are a validated shared-collateral ProTx's shares, each amount > 0 and summing to the collateral. So the `u128` product is exact and every quotient is ≤ `total`.
- **Empty `weights`:** Core returns an empty vector (the loop body never runs), and so does the port. The "sums to `total`" property holds only for a non-empty list.
- **One share:** the whole `total`, with no division.
- **Two or more shares with `weight_total == 0`:** Core's `arith_uint256` division throws `uint_error`. Validation makes that unreachable; the port returns `Err(PaymentError::ZeroShareTotal)`.
- **Casts:** Core passes `total`, `weight` and `weight_total` through `static_cast<uint64_t>` and returns `static_cast<CAmount>(GetLow64())`. The port uses `as u64` and `as u64 as i64`, the same bit semantics even for negative values.
- `weight_total` accumulates in `int64`. That is undefined on overflow in Core, and wrapping in the port.

`split_amount_by_shares` and `owner_payout_amounts` are pure arithmetic. EV's payout assembly owns the share types and scripts and calls them.

---

## 6. Verification

### 6.1 Every PR (`cargo test -p dash-consensus`)

- **L0 (O4, `test-vectors/core/0364ed626ec2/transcribed/`):**
  - `pow_tests.json`: 24 real mainnet DGW headers, plus the main and devnet `next_work` records, which cover the min-difficulty ×10 and the 2 h rules;
  - `subsidy_tests.json` (12 records);
  - `block_reward_reallocation_tests.json` (71 records: block subsidy, superblock subsidy, MN payment, era);
  - **`evo_sharedmn_tests`'s `SplitAmountByShares` cases** (test case `split_amount_by_shares`, `src/test/evo_sharedmn_tests.cpp:522-543`, v24), to be transcribed by O4 (§7).

  Each record runs through the public API. Expected values are Core's, never hand-typed.
- **`fp`:** the four KGW tests of §2.4, the KGW fixtures, and `Difficulty` unit tests against values the oracle recorded (O4-style JSON regenerated by a script).
- **Non-canonical encodings** (GPT M2), with expectations from the oracle:
  - `0x1e010000`, `0x1f000100` and `0x20000001` through `check_proof_of_work`, with hash `== target`, `target + 1`, and negative and overflowing variants;
  - the same words through `Difficulty::from_bits`, the early-mainnet subsidy (277 versus 1 DASH), `nbits_acceptable` at heights 68589 and 68590, and the BTC raw-bits return.
- **Quirk tests** (`#[ignore]`d by O5's scaffolding, un-ignored here). Expected values come from O4 records or from the oracle (recorded JSON):
  - `quirk_08_dgw_window`: 23 vs 24, per-step truncation, wrapping; the devnet ×10 wrap.
  - `quirk_09_mainnet_68589_tolerance`: both sides and equality of the 50% boundary, a zero expected target, a zero-mantissa header without PoW (NaN and +∞ accepted), and the exact rule at 68590.
  - `quirk_11_subsidy_prev_bits_and_height`: prev `nBits`/height; buggy dDiff ≤ 4500 including the non-canonical words; truncation before clamping; iterative decline from block 210241; treasury from 328010.
  - `quirk_12_mn_share_schedule`: separate truncation; period 8 skipped; realloc start 1379128.
  - `quirk_15_payments_limit_nbits`.
  - **`quirk_40_kgw_fp_platform_invariance`**: §2.4 test 1 under the register's name. The register text (DEC-33 a) reads: "Core's release builds evaluate KGW's horizon with different `pow` implementations (the host glibc, per x86_64 ifunc; win64's static mingw-w64 x87 `pow`, whose result depends on the thread's x87 precision control; Apple's libm) and with separate (x86_64 Linux, win64, x86_64 macOS) or fused (aarch64 and riscv64 Linux, arm64 macOS) rounding. At least four distinct `pow` tables ship, and the horizon values differ between builds at 41 masses. Decisions are identical for every `pow` within 10⁻⁹ relative error; the nearest input is 35,751 ulps from a boundary."
- **Property tests:**
  - `get_next_work_required` never panics, for arbitrary chains through a test `ChainAncestry`;
  - the early-exit decline equals the plain loop;
  - `split_amount_by_shares` sums to `total` for every **non-empty** positive weight list, and returns an empty vector for an empty one.

### 6.2 L1 differential (`dash-oracle-sys`, both pins, in `node-fuzz`)

| Target | Generator | Compares |
|---|---|---|
| `diff_next_work` | Synthetic header windows per network: main; test; regtest; devnet with `-powtargetspacing ∈ {1, 30, 60, 150, 86400}`; devnet with `-minimumdifficultyblocks ≥ 4000` and `-powtargetspacing ∈ {86401, 178956971, 2²⁹−1, 2²⁹, 2³¹, ⌊i64::MAX/72⌋}`, which exercises the live `uint32` truncation and the divisor > `u32::MAX` path. Inputs: random and adversarial `nBits` (negative, overflow, zero, non-canonical, near powLimit on devnet for the ×10 wrap), non-monotonic times, gaps around 300 s, 600 s and 7200 s, retarget boundaries, the KGW/DGW switch heights, and the KGW threshold windows of §2.4 (with their two generator invariants) | `nBits` |
| `diff_subsidy_payments` | `(prev_bits, prev_height, v20, era, block_value)` per network, with devnet high-subsidy overrides and regtest `-budgetparams`. It includes factors above 184,467,440 and above 6.6×10⁷, where C++ is undefined. There the expected values are the release binaries' wrapping results (§5); a disagreement in that range would point at the shim's compiler, and is checked against the shim archive's listing before being treated as a port bug | subsidy, treasury part, MN payment, platform share (or both refuse) |
| `diff_nbits_acceptable` (new, GPT M4 / Opus m-1) | `(header_bits, expected_bits)`: random pairs, plus edges: mantissa 0; size bytes 0, 1, 29–34 and 0xff; the non-canonical words; exact boundary pairs (`n2 = n1/2` and `n2 = 1.5·n1`, which are exact) and their one-mantissa-step neighbours; zero expected target | `within_pre_dgw_tolerance` against the literal predicate applied to **Core's own doubles** (`oracle_convert_bits_to_double`), independent of the port's `ConvertBitsToDouble`. The end-to-end verdict (Core's `ContextualCheckBlockHeader` itself, including the 68589/68590 switch and the exact rule on other networks) comes with CS3's `diff_contextual_check_block_header` (O2a) |
| `diff_payout_split` (new, GPT M4) | (a) `split_amount_by_shares` against `oracle_split_amount_by_shares` (Core's real function, §7): random positive weights, products above 2⁶⁴, awkward remainders, 1..N shares; (b) the history leg of §6.4 for `operator_reward` and `owner_payout_amounts` | amounts |
| `diff_pow_check` | random `(hash, bits)`, plus the non-canonical words with `hash ∈ {target, target+1}` | `check_proof_of_work` |

Core has no standalone function for the operator/owner split (it is inline in `GetBlockTxOuts`), so its Core-produced references are real coinbases (§6.4). The arithmetic edges no chain reaches are unit tests whose expected values are the binaries' semantics, backed by the listings in §5:
- operator bps 0, 1, 9999 and 10000;
- one and several owners, with an awkward final-owner remainder;
- rewards at `⌊i64::MAX/bps⌋` and one above.

These are the only expectations in CS2 not produced by Core, and the reason is recorded with them.

Budgets follow NFR-CEQ-04: the 5-minute smoke test per PR, and ≥ 24 CPU-h per new target before merge. The KGW boundary windows are a deterministic list (≈ 16k cases) run at every PR, with the default ifunc and with the SSE2 tunable, each run asserting which variant it measured.

### 6.3 Exhaustive sweeps: phase C merge gates

- `Difficulty::from_bits` against `oracle_convert_bits_to_double` (O1b) for all 2³² `nBits`, bit-exact.
- `block_subsidy_inner` against `oracle_block_subsidy_inner` for all 2³² `prev_bits` × {formula-1, formula-2 (both the < 17000 and the ≤ 75 path), formula-3 heights} × {main ≤ 4500 path, other}, with `v20 = false`. That is about 2.6×10¹⁰ calls, a few CPU-hours on agentbox.

Both sweeps also assert that every base lies in [0, 1111] and that the saturating cast never saturates.

These cover the whole `u32` input space, a strict superset of F2 and F3. **Phase C does not merge until both have completed against both oracle pins with zero mismatches** (GPT M1). After that they run nightly on O3's farm.

### 6.4 History sweeps (BS2 oracles)

- **Headers** (complete now): every mainnet and testnet header gets three checks.
  - `check_proof_of_work`.
  - `get_next_work_required(prev)` equals the header's `nBits` wherever Core checks exactly (testnet everywhere, mainnet ≥ 68590).
  - For mainnet ≤ 68589, it equals the oracle's `GetNextWorkRequired`, and `nbits_acceptable` holds.

  This includes every KGW evaluation (mainnet 15200–34139).
- **Subsidy** (headers only): every mainnet and testnet height's `block_subsidy` from the previous header equals the oracle's `GetBlockSubsidyInner`. Wherever the oracle has the block, it also equals `getblockstats <h> '["subsidy"]'`. That is a sample now and complete once the oracles reach the tip (an M1-exit item).
- **Masternode payouts** (needs the oracles' blocks; `diff_payout_split` b): for every mainnet and testnet block from DIP3 enforcement, and for the BS2 regtest-v24 chains `feature_masternode_payout_shares` and `feature_masternode_shares` (multiple payouts, shared collateral):
  - recompute the masternode amounts from Core's subsidy plus the block's fees and the era, then `platform_share`, `operator_reward` and `owner_payout_amounts` / `split_amount_by_shares`, with the payee's state at `h−1` (`protx info`);
  - require the accepted coinbase's masternode outputs to contain them.

  These are Core-produced references for the split arithmetic.

### 6.5 Bench

The criterion bench `next_work` (§3.9) and a `subsidy` microbench, recorded in the PF0 counter baseline.

---

## 7. What CS2 needs from other tasks

| From | What | Kind |
|---|---|---|
| CS0 | the `dash-consensus` skeleton (crate, `clippy.toml`, CI group), `ChainAncestry`/`BlockId`/`HeaderCtx` | merge dependency. Phases A and B develop on a provisional `lib.rs` that CS0 reconciles at merge (DEC-29 D6) |
| BS3 | the base-sdk `[patch]` wiring (`dash-num`, `dash-params`) | merge dependency (as for CS0) |
| O1b | `oracle_convert_bits_to_double(uint32, double*)` and `oracle_superblock_payments_limit(int32 height, int64*)`, both pins (DEC-29 D3). **Requested here (D7):** `oracle_split_amount_by_shares(int64 total, const int64* weights, size_t n, int64* out)`, which calls Core's `SplitAmountByShares` (v24 pin only; absent in v23.1.8) | approved (D3); requested (D7) |
| O4 | the transcribed vectors (exist, branch `node/o4-core-vectors`). **Requested here (D8):** transcribe `evo_sharedmn_tests.cpp`'s `SplitAmountByShares` cases | soft; read from the pinned Core tree if O4 has not merged |
| O5 | quirk scaffolding (`#[ignore]` stubs 8, 9, 11, 12, 15, 40) | soft |
| BS5 | devnet fields (with `GetIntArg`'s `int` truncation) and the 68589 pin in `dash-params`; refuse (not clamp) negative regtest `-budgetparams` starts, or carry them as `i32` (§2.5) | interface: CS2's `from_chain_params` switches over with no API change |
| BS2 | header dumps; the oracles' blocks and `protx info` for the payout leg; the regtest-v24 chains | exists |
| CS3 / O2a | `diff_contextual_check_block_header`, the end-to-end tolerance verdict | CS3's gate |

---

## 8. Deviations from A1 and PLAN

| # | A1 / PLAN | CS2 | Why |
|---|---|---|---|
| 1 | PLAN row: "the bit-exact floating-point port `fp::pow`"; A1 §2.4 "KGW (`f64` and `fp::pow`)"; A1 §4.5 "a libm port is kept … for KGW" | no `pow` port; the `KGW_POW` table of correctly rounded values plus the decision proof | there is no bit-exact Core reference to port (at least four tables and two roundings ship), the decisions provably agree for any `pow` within 10⁻⁹, and the domain is 4032 points (report §4). Accepted as DEC-29 D5 |
| 2 | A1 §4.5: libm calls allowed inside `fp` | none anywhere in `dash-consensus` | nothing needs one after deviation 1 |
| 3 | A1 §4.5: "10⁸ random inputs against glibc (FMA and non-FMA) and aarch64, and against the release binary's routine" | the platform-independent two-endpoint check over the whole domain, plus every measured shipped routine as a fixture (win64's own routine included) | strictly stronger |
| 4 | App. A: `get_next_work_required(chain, last, header: &BlockHeader, p: &ChainParams)` | `(chain, last, new_time: u32, p: &PowParams)` | Core reads only `pblock->GetBlockTime()`; `ChainParams` lacks devnet fields until BS5 |
| 5 | App. A: `block_subsidy_inner(prev_bits: CompactTarget, prev_height: u32, …, p: &ChainParams) -> Amount` | `(prev_bits: u32, prev_height: i32, v20, &SubsidyParams) -> Result<Amount, SubsidyError>` | Core passes the raw `int` bits and heights, including `h−1` from `GetPaymentsLimit`; `Err` replaces Core's abort on unreachable inputs (§2.5) |
| 6 | App. A: `masternode_payment(…, p: &ChainParams)` | `&PaymentParams` | as 4 |

---

## 9. Decisions

**Resolved** (DEC-29, DEC-33):
- D1: the table approach;
- D2: quirk 40, with the DEC-33 text in §6.1;
- D3: the O1b entry points;
- D4: F1 to LQ1;
- D5: the errata;
- D6: phases A and B started.

**Needed from the manager (new in rev1):**
- **D7. A third O1b entry point,** `oracle_split_amount_by_shares` (§7). It is a few lines in the v24 shim and calls Core's real function. It gives `diff_payout_split` (a) a Core-produced oracle for random weights. The fallback, if declined, is O4's transcription (D8) plus the regtest-v24 shared-collateral chain, which cover fewer shapes.
- **D8. O4 transcribes `evo_sharedmn_tests.cpp`'s `SplitAmountByShares` cases.**
- **D9. O17 PIN-DELTA checklist item:** at every re-pin, re-run `CS2-fp-evidence/tools/reproduce.sh` on all six builds of the new tag (report §7 F2).
- **D10. A note for LQ1 on F1** (report §7). The `log2` call is only reached for rotation quorums, whose `signingActiveQuorumCount` is 2 or 32. So every shipped libm (glibc, the mingw-w64 x87 `log2` on win64, Apple's) returns the exact integer, and `trailing_zeros` is the Rust equivalent. LQ1's domain check must still confirm that no other count reaches the call.

---

## 10. Implementation plan

| Phase | Content | State |
|---|---|---|
| A | `pow` integer: `PowParams`; dispatch; BTC; DGW; min-difficulty; `check_proof_of_work` (§3.2's exact predicate, raw bits); `nbits_acceptable`'s exact branch; L0 `pow_tests`; quirk 8; `diff_next_work` for non-KGW heights; bench | being implemented (`node/cs2-pow-subsidy`, DEC-29 D6) |
| B | `payments` (all of §5, with §5's bounds and split domain; `platform_share` wraps before its range check, §5); `subsidy`'s integer skeleton (eras, clamps, decline, treasury, `SubsidyParams` accepting any factor, the payments limit) over `fp` stubs; L0 BRR; quirk 12 | being implemented |
| C | `fp` (§2.3) with `KGW_POW`, its generator, the fixtures and tests 1–4 of §2.4; KGW (§3.5); the subsidy bases; the tolerance (literal predicate); quirks 9, 11, 15 and 40; L0 `subsidy_tests`; the confinement lint and its self-test (§2.6) | after this revision |
| D | L1 targets to budget, including `diff_nbits_acceptable` and `diff_payout_split`; the exhaustive sweeps (**a phase C merge gate**); the history sweeps; self-review, `code-simplifier`, `code-review-validator`; the report | with C |

The commits follow the phases, one logical change each (conventional commits).

---

## 11. Risks

| Risk | Mitigation |
|---|---|
| A future Core release (re-pin) changes the compiler, flags, ISA baseline or `pow` routine of a shipped build | Fused and separate roundings are both inside the proof, and so is any `pow` within 10⁻⁹. Re-pinning re-runs `reproduce.sh` on all six builds (D9); a new shipped `pow` table becomes a fixture |
| Apple's x86_64 `pow` is not measured | It is covered by the 10⁻⁹ argument under a stated assumption (report §4.3): 10⁻⁹ is about 4.5 million ulps, while the worst `pow` in the published survey is 636 ulps. An optional Mac run would make it a measurement (report §7 F3) |
| Table regenerated wrongly | The generator's double MPFR check; tests 1 and 2 of §2.4, and the oracle's boundary windows, fail on any decision change |
| The FP environment changed by a C dependency | Stated assumption (§2.2 rule 4). Flush-to-zero cannot matter on the admissible domain; a rounding-mode change is caught by the proposed `rdashd` startup self-check (ND follow-up) |
| Parameter plumbing before BS5 | Narrow structs with validating constructors; test params built from the oracle's chain-params JSON |
| Overload and wrap semantics of `arith_uint256` | The §3.7 table; `diff_next_work` with adversarial and non-canonical `nBits`; quirk 8 |
| History sweep against the oracles still in IBD | The subsidy and `nBits` sweeps need only headers, which are complete. The `getblockstats` and payout legs complete when the oracles reach the tip (an M1-exit item) |

---

## 12. Review dispositions (r1)

Reviews: `reviews/CS2-design-r1-gpt.md` (GPT, APPROVE-WITH-CHANGES) and `reviews/CS2-design-r1-opus.md` (Opus, APPROVE-WITH-CHANGES). Required changes: DEC-33 (a)–(d). Every finding is **accepted**. The evidence was extended and re-run (report rev1).

| Finding | DEC-33 | Disposition | Where |
|---|---|---|---|
| **Opus M-1**: "every shipped build" covered only two Linux builds | (a) | Accepted. All 12 builds (six platforms × both pins; the reviewer covered v24 only) were disassembled with a generalized `xref.py` (ELF, PE, Mach-O; riscv64 added). The new findings: win64's static mingw-w64 x87 `pow`, riscv64's and arm64 macOS's fused horizon, Apple libm on macOS. The win64 tables were measured natively from both pins' `dashd.exe`; they are identical between pins and match the reviewer's bit-for-bit. The claims now read: at least four distinct shipped tables, 41 masses, decisions identical everywhere. Quirk 40 has the DEC-33 text | report §0, §2, §3.2, §4.1; design §0, §2.1, §6.1 |
| **Opus M-1** fix 3: the Apple assumption | (b) | Accepted. Covered by the ε proof. The published arm64 measurement is 0.757 ulp; x86_64 macOS is a stated assumption. No Mac run is needed per DEC-33; an optional run is listed | report §4.3, §7 F3; design §11 |
| **Opus M-1** fix 4: six builds at re-pin | — | Accepted as D9 (O17 checklist item) | design §9; report §7 F2 |
| **Opus M-2**: the proof was host-dependent, and the SSE2 tunable can no-op silently | (b) | Accepted. Test 1 is now the two-endpoint monotonicity check at ε = 10⁻⁹ (4-ulp outward widening, both roundings, exact fused rounding without `mul_add`). The measured shipped tables are fixtures (test 2). The host-libm test asserts which variant ran, and the L1 tunable rerun does too. `kgw_proof.py` is the reference run: 16,076 endpoint evaluations pass | design §2.4; evidence `out/kgw-proof.txt` |
| **Opus M-2** fix 4: restate the margin floor | (b) | Accepted. `kgw_margin_floor` (ulps of `d`) is withdrawn; test 1 is the floor in relative-error units (10⁻⁹ against the measured 1.534×10⁻⁹) | design §2.4 |
| **GPT M3**: the proof was broader than the measured matrix (FMA4, tier 2) | (a)(b) | Accepted. The report separates operation shape (12 builds), measured tables, and the error-bound argument. FMA4 is covered by glibc 2.31's documented 0.54-ulp bound for the same `e_pow.c` built with `-mfma4` (signed tarball, file hashes and lines cited). The GPT reviewer's note that its qemu FMA4 attempt did not exercise FMA4 is respected: no FMA4 table is claimed | report §1, §4.3 |
| **GPT M1**: recorded history is not the admissible subsidy domain | (c) | Accepted. The checkpoint/minimum-work exclusion is removed. F2 is now a numerical proof over every valid compact (canonical or not, size bytes 1..34) plus `nBits = 1`; history is validation. The §6.3 sweeps are an explicit phase C merge gate. The near-`INT_MAX` row now rests on Core's signed-overflow boundary, not on cost | design §2.5 F2 and excluded table, §6.3 |
| **GPT M2**: valid non-canonical `nBits` (size up to 32) | (c) | Accepted. F3's bounds are corrected (size ≤ 32; up to three `÷256`; the expected side may be a raw non-canonical word via BTC; zero expected target rejects). `check_proof_of_work`'s exact predicate is specified. Raw bits are preserved everywhere, with no canonicalization. The three encodings join the PoW, `ConvertBitsToDouble`, early-subsidy (277 versus 1 DASH), tolerance and 68589/68590 tests | design §2.5 F2/F3, §3.2, §3.4, §6.1, §6.2 |
| **GPT M4**: no differential coverage for payment splits or tolerance rejection; the split domain; `subsidy + fees` bounds | (c) | Accepted. New `diff_nbits_acceptable` (Core's doubles with the literal predicate; end-to-end with CS3/O2a). New `diff_payout_split`: an O1b probe of Core's `SplitAmountByShares` (D7), O4's transcription of `evo_sharedmn_tests` (D8), and the mainnet/testnet/regtest-v24 coinbase history leg for the operator/owner split. The split domain is written out (empty → empty, one share, zero total → `Err`, `uint64` casts), and the sum property is restricted to non-empty lists. The bounds table covers ordinary and devnet-factor configurations, and the wrapping is backed by the binaries' `imul`/`mul` listings. Re-deriving the bounds also showed that `masternode_payment` cannot overflow on any admissible configuration, while `platform_share` can, on devnets with factor ≳ 6.6×10⁷. There the binaries wrap and may continue, so the port wraps before its `MoneyRange` check; phase B's `checked_mul` needs that one-line change (§5) | design §5, §6.1, §6.2, §6.4, §7, §10 |
| **Opus m-1**: tolerance bound wrong; NaN/∞ without PoW; no differential | (d) | Accepted. The bound is corrected (merged with GPT M2). The literal `!(x > y)` is mandated, with a doc comment and tests (mantissa 0 without PoW, NaN accepted). `diff_nbits_acceptable` was added | design §2.3, §2.5 F3, §6.2 |
| **Opus m-2 / GPT m2**: integer division mis-described | (d) | Accepted. The §3.7 row now cites `operator/=(uint64_t)` and `operator/(…, uint64_t)` at both pins, the `> u32::MAX` fallback, and the devnet spacing (178,956,971) that reaches it, which `diff_next_work` includes. §0 now speaks of one overload trap | design §0, §3.7, §6.2 |
| **Opus m-3**: generator invariants for the KGW boundary windows | (d) | Accepted. Non-monotonic timestamps are documented as intended, and the generator asserts differing `nBits` on both sides of each pair | design §2.4 |
| **Opus m-4**: `-highsubsidyfactor` both rejected and "mirrored" | (d) | Accepted, resolved toward accept-and-wrap (the shipped binaries' behaviour; phase B already does this). The rejection is removed; `GetIntArg`'s `int` truncation is noted for BS5 | design §2.5, §3.1, §4.1, §4.2 |
| **Opus m-5**: the LLMQ `log2` follow-up must cover win64 and macOS libms | (d) | Accepted, and narrowed. The call is reached only for rotation quorums (counts 2 and 32, powers of two), so every shipped libm is exact there. Passed to LQ1 as D10 | design §9 D10; report §7 F1 |
| **GPT m1**: the confinement glob scans `fp` | — | Accepted. `--glob '!**/fp/**'`, plus a self-test of both directions | design §2.6 |
| **Opus n-1**: the near-`INT_MAX` wording | — | Accepted. The row says Core is undefined, the binary may not terminate, and the port returns the fixed point | design §2.5 |
| **Opus n-2**: [0, 1111] holds for every `u32` | — | Accepted. Stated, and asserted by the sweeps | design §2.3, §6.3 |
| **Opus n-3**: subnormal threshold; per-thread MXCSR/FPCR; rounding mode | — | Accepted. Size byte 156; per-thread state; the rounding mode is named in the stated assumption, with an ND startup self-check follow-up | design §2.2 rule 4; report §3.1 |
| **Opus n-4**: `SHA256SUMS.asc` not GPG-verified | — | Accepted. Verified: good signatures at both pins from keys in Dash Core's `contrib/verify-commits/trusted-keys`, pinned in `evidence/keys/`. `reproduce.sh` fails on any bad or unknown signature | report §1; evidence `keys/`, `tools/reproduce.sh` |
