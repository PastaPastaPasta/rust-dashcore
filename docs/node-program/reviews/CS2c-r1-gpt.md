CHANGES-REQUIRED

Reviewed `node/cs2-fp` at **46cf12a3805879038a69580e8afaf363aef15c06**, the 13 commits above **d44631bb**, in a fresh detached worktree at `/work/scratch/cs2c-gpt/repo`. Read `REVIEW-COMMON.md`, CS2 §2/§6.3/§10, the release-binary report and evidence, DEC-29/33/35/41, all preceding CS2 design and implementation reviews, and `dashd-release-fp-findings`. Applied `code-review-validator`. Core references are **v23.1.8 (728f5055)** and **v24.0.0-rc.3 (0364ed62)**, read using `git show` without changing Core's checkout.

**No blocker, major, or demonstrated consensus-output mismatch. Three minor enforcement defects require closure before phase C relies on the FP gate.** Findings 2 and 3 reproduce the expressly deferred closure items in `CS2ab-r2-gpt.md`; that review approved A+B, not reliance on these gates in phase C. The current arithmetic, the correctly rounded table, and the platform decision proof survive this review.

## Numbered findings

### 1. Minor — the new remainder ban misses the method form through a glob import

**Location:** `dash-consensus/tests/fp_confinement.rs:287`, especially the identifier check at `:295`; `dash-consensus/clippy.toml:28`. The claimed enforcement is also described at `dash-consensus/src/lib.rs:25`.

**Concrete input to the gate:** this private function in `src/fp/`:

```rust
use core::ops::*;
fn remainder(x: f64) -> f64 { x.rem(2.0) }
```

For `x = 5.5`, it computes 1.5 through floating remainder. Neither `%` nor a `Rem` identifier occurs in these tokens. Both `fp_file_offenders` and `remainder_lines` return empty results. The exact scratch probe compiles under `#![forbid(unsafe_code)]`, the crate's Clippy configuration, `-D warnings`, and `-F clippy::disallowed_methods`. Its optimized machine code calls `fmod`; on this Rust toolchain the linked implementation is compiler-builtins' libm `fmod`. Thus the stated no-libm gate is incomplete even without an unsafe block, a dependency, a macro expansion, or a lint override. `rem_assign` has the analogous escape. The current production source does not use either one.

**Core citations at both pins:** `pow.cpp:56` is KGW's sole source-level `pow` in both releases. The three subsidy squares are `validation.cpp:1508/1514/1520` at v23.1.8 and `:1834/1840/1846` at v24.0.0-rc.3. Regenerated release listings confirm that the subsidy uses multiplication and KGW uses the measured `pow`; none of these FP expressions uses a remainder. CS2 §2.2/§2.6 requires this port to admit only the approved basic arithmetic and to ban libm throughout the crate.

**Fix:** reject `rem` and `rem_assign` method calls in `fp`, or forbid their method identifiers conservatively alongside `%`, `Rem`, and `RemAssign` (the approved module needs no integer remainder either). Alternatively use a verified type-aware trait-method ban. Add the glob-import method probes, including `rem_assign`, to the gate's self-test. Do not count only a literal `Rem::rem` spelling as coverage of method syntax.

**Evidence:** `/work/scratch/cs2c-gpt/scanner_probes.rs`, `scanner-probes.log`, `rem_probe.rs`, and `rem_probe`.

### 2. Minor — the previous ordering-escape finding remains open

**Location:** `dash-consensus/tests/fp_confinement.rs:125` and `:132` (`check_derives` and `check_renames`), with the trait-header check at `:148`.

**Concrete inputs to the gate:** either of these changes to the public opaque type passes the current scanner:

```rust
use core::cmp::PartialOrd as Compare;
#[derive(Clone, Copy, PartialEq)]
pub struct Difficulty(f64);
impl Compare for Difficulty {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.0.partial_cmp(&other.0)
    }
}
```

```rust
#[derive(Clone, Copy, PartialEq)]
#[cfg_attr(target_arch = "x86_64", derive(PartialOrd))]
pub struct Difficulty(f64);
```

A permitted constructor returning `Difficulty` then lets another module evaluate `fp::value(1) < fp::value(2)`. The comparison has no float token outside `fp` and passes `clippy::float_arithmetic`. I compiled and ran both examples with the ordinary `-D warnings` gate; copied scanner functions report no offenders. Renaming only rejects primitive float names, while derive inspection only visits literal `derive` attributes. These are the two escapes previously reported as CS2ab-r2 GPT m2, unchanged in this branch. Neither is present in the current `Difficulty` implementation.

**Core citations at both pins:** Core keeps the difficulty-dependent `dDiff <= 75` decision in `validation.cpp:1511` at v23.1.8 and `:1837` at v24.0.0-rc.3; its tolerance comparison is `:4113–4116` and `:4479–4482`, respectively. CS2 §2.2 rule 5 and §2.3 deliberately put those decisions in `fp`, and permit `Difficulty` to derive only `PartialEq`, not ordering.

**Fix:** reject renaming `PartialOrd` and `Ord` at every visibility, and recursively inspect `cfg_attr`'s nested derives for forbidden ordering. Preserve tests of direct trait implementations and add both complete escape examples. Resolve the earlier phase-C closure item explicitly.

**Evidence:** `scanner_probes.rs`, `scanner-probes.log`, `ordering_alias.rs`, `ordering_cfg_attr.rs`, and `escape-compilation.log` under `/work/scratch/cs2c-gpt/`.

### 3. Minor — the previous macro-float escape remains open

**Location:** `dash-consensus/tests/fp_confinement.rs:74`, specifically `dots != 1` at `:75`; the outside-`fp` scan invokes it at `:320`.

**Concrete input to the gate:** an ordinary source file outside `fp` containing:

```rust
macro_rules! less {
    ($ignore:tt $a:literal, $other:tt $b:literal) => { $a < $b };
}
fn outside() -> bool { less!(. 0.5, . 1.5) }
```

The result is `true`, computed by native float comparison outside `fp`. The scanner returns no offender because it exempts every numeric token preceded by one dot as a tuple-field access, even inside arbitrary macro input. The dots are discarded by this macro. This is the same reproducer as CS2ab-r2 GPT m1; the relevant scanner code is unchanged. The complete scratch program compiles and passes the current Clippy bans without allowing the float lint. This is not merely the acknowledged inability to expand macros: the gate promises to inspect macro argument tokens, but incorrectly exempts their actual float literals.

**Core citations at both pins:** the decisions being confined are KGW at `pow.cpp:53–62` in both tags, the CPU-formula selection at v23 `validation.cpp:1511` / v24 `:1837`, and the tolerance at v23 `:4113–4116` / v24 `:4479–4482`. CS2 §2.6 explicitly requires rejection of inferred floats and comparisons outside `fp`.

**Fix:** scan macro token streams conservatively without the dot exemption. Exempt tuple-field accesses in normal Rust using the parsed expression structure, and keep the `t.0.1` negative self-test. Add this macro argument case as a positive self-test and close the earlier phase-C requirement.

**Evidence:** `scanner_probes.rs`, `scanner-probes.log`, `macro_escape.rs`, and `escape-compilation.log` under `/work/scratch/cs2c-gpt/`.

## Independent assessment of the requested attacks

### Floating-point exactness and casts

`Difficulty::from_bits`, the early-mainnet variant, the three bases, and the tolerance mirror Core's literal operation order. The mask is all 24 low bits; stored header words remain raw. Zero mantissas yield positive infinity, never a NaN difficulty. Nonzero difficulties stay nonnegative, including subnormal results and eventual positive zero at artificial large exponents. The three formula denominators stay positive; an infinite denominator yields zero. Every base is finite and in `[0,1111]`, so Rust's saturating `as i64` never saturates and never applies its NaN-to-zero rule. It equals the releases' truncating conversion throughout the all-`u32` domain. The exhaustive subsidy sweep compares final integer subsidy, not every pre-clamp floating intermediate; the numerical bounds and identical operation shape supply that additional argument.

For supported KGW parameters, the only integer-to-double inputs are masses through 4032, target seconds `150m <= 604800`, actual seconds through `u32::MAX`, and the constant timespan 86400. All cast exactly. `86400 * 0.025` rounds to 2160, safely in the unsigned conversion range. DGW's `as u32` is deliberate modular truncation, not a floating conversion.

I compiled wrappers of the actual reviewed `fp` source with optimization and `+fma,+avx2`. LLVM emits ordinary `fmul` followed by `fadd`, without `contract`, `fast`, reassociation, or reciprocal flags; the assembly has separate scalar instructions and no FMA. The actual Cargo build configuration adds only the linker selection, with no FP flags. Basic Rust arithmetic in this build therefore cannot silently fuse the horizon. `mul_add`, `powf`, `powi`, `exp2`, and the other stable libm methods are named in the Clippy ban; a deliberate `powi` probe is rejected. Unsafe ISA intrinsics/FFI are prohibited by `forbid(unsafe_code)`. Rust 1.98's new `algebraic_*` methods would permit non-IEEE transformations, but the current `1.89` MSRV lint rejects them, and the implementation uses none. The concrete remaining libm hole is finding 1.

The x86-without-SSE2 compile guard excludes x87 arithmetic. The release subsidy and tolerance use binary64 on all six platforms; win64's x87 code is confined to its `pow` routine, whose PC53/PC64 results the decision proof covers. Round-to-nearest with the default per-thread MXCSR/FPCR remains the expressly approved assumption. A changed rounding mode can change results; I do not treat the default-environment sweep as proof for altered modes. DEC-35 assigns the validating-thread startup check to ND, so its absence here is not a newly introduced phase-C requirement. FTZ/DAZ cannot affect valid checked header inputs; artificial no-PoW proposals and the exhaustive invalid-word domain can reach subnormals and rely on the stated default environment.

### KGW proof, mass range, and accumulation

The table need not equal glibc's occasional one-ulp error: **the break decision** is the consensus output at this site. I independently regenerated all 4032 correctly rounded values using MPFR at **512-bit precision**. For each of the 4019 decision masses I derived integer thresholds analytically from exact rational binary64 rounding cells, rather than reusing the implementation's bisection. I checked their four neighboring integer gaps against IEEE division. All **16,076** endpoint/rounding cases at relative error `1e-9` reproduce those thresholds. I also verified with MPFR that the four-ulp outward widening contains the mathematical `1e-9` interval around the exact value.

Every measured shipped table reproduces the analytic thresholds. The smallest margin is **35,751 ulps**, at `m=2712`, `a=407858`, with thresholds **405744 / 407859**. A widened `1.5e-9` interval still passes at that mass; **`1.6e-9` fails**, moving the slow threshold to 407858. The advertised proof is therefore conservative and non-vacuous. Monotonicity in positive `p` covers the endpoint interiors, for both separate and fused horizon rounding.

At both Core pins, `pow.cpp:35–37` checks the 4032 cap before incrementing mass. `:49` computes actual seconds directly as last time minus the currently read time; it does **not** accumulate signed intervals. The port's saturating `u32` subtraction equals the clamped signed difference even for reversed/extreme timestamps. `:50` recomputes `150m`; no error accumulates between horizon evaluations. The unsigned target average is independent of `pow`, and follows the same ordered subtraction, floor division, and update. Earlier matching break decisions therefore imply matching windows and final target arithmetic by induction. Only mainnet reaches KGW: the test/devnet activation heights coincide, and regtest returns through no-retargeting. No supported override opens another mass or spacing domain. Masses below 14 cannot affect a break; gap zero resets the ratio to 1 inside the horizon. Apple's accepted accuracy assumption remains the limitation documented by DEC-33/35; this review does not claim a native Mac measurement.

### Integer edges and the pin split

`div_u64` computes the same exact floor quotient as both pins' `arith_uint256.cpp:84` (v23) / `:77` (v24). For radix `B`, appending a limb to `P=dQ+r` gives `PB+x=d(QB)+(rB+x)`, with `0 <= r < d`. Its 32-bit path fits `rB+x` in `u64`, and its 64-bit path fits it in `u128`; each quotient limb fits its output width. This proves all nonzero divisors, not only sampled values. Core throws at zero; the port returns zero, as documented. No valid caller divides by zero.

DGW matches the 24-target/23-interval window, wrapping multiply/add, floor division at each pseudo-average step, sequential clamps, and `operator*=(uint32_t)` conversion: v23 `pow.cpp:94–122`, v24 `:94–125`. The spacing ceiling prevents all live signed timespan products from overflowing. I generated fresh devnet cases around divisors `u32::MAX`/`2^32`, spacings `2^29-1`, `2^29`, `2^29+1`, `2^31`, and `i64::MAX/72`, with zero, reversed, and full-`u32` timestamp spans and wrapping/noncanonical targets. Lower clamps at `2^29` and `2^29+1` multiply by **0** and **8**, respectively. Unsupported configurations are refused instead of being counted as comparisons.

The two subsidy helpers are text-identical between the tags. Their v23 anchors are correct. Core's outer subsidy takes the previous bits/height and the deployment state at the block: v23 `validation.cpp:1558–1562`, v24 `:1884–1888`. The block/treasury split, V20 divisor, high-subsidy factor after decline, and synthetic payments-limit bits match their respective pins. `GetPaymentsLimit` uses height-based V20 at v23 `governance/classes.cpp:149–155` and v24 `governance/superblock.cpp:153–159`. DEC-41 requires each network's release behavior, but introduces no FP/subsidy-helper split here. The V24-only payout helper is properly tested only at V24; it is not silently presented as a V23 function. The documented signed-overflow/abort exclusions, including decline-counter overflow and negative regtest budget starts that the parameter type cannot represent, remain explicit.

### Gate execution

`check-kgw-fp.sh` measures the entire host table and checks the reported fixture name. A silently ignored tunable cannot make a second FMA run count as SSE2. It unsets inherited tunables for the default runtime and captures each invocation independently. My run reports **glibc-fma** followed by **glibc-sse2**, and replays all **16,076** recorded boundary windows in release mode. Its requirement for an FMA-capable default host is explicit and fails closed on unsupported hosts.

The job really is wired at `.github/workflows/rust.yml:111–120`, but **hosted GitHub execution is currently skipped** by `ci-plan.yml` because this branch uses the local `file://` base-sdk fork. This is the existing program arrangement, not a claim that GitHub ran it. The author's local `gate.sh` includes it, and `gate.log` at the reviewed HEAD records exit 0 for it and every other step. I independently counted **2,889** passed workspace tests in that retained log. Ordinary all-target Clippy applies the crate's method bans; the separate pure-crate script still substitutes its own configuration, as previously documented pending CS0 reconciliation.

## Validation and limits

All reviewer writes other than this report are under `/work/scratch/cs2c-gpt/`. Runs used `TMPDIR=/dev/shm/cs2c-gpt-tmp` (0700), at most eight build/test jobs, and load checks stayed below 70.

- `cargo test --locked -p dash-consensus --all-features`, debug and release: **63 tests pass in each**. The release run covers every recorded KGW boundary.
- Targeted all-target/all-feature Clippy with `-D warnings`, workspace formatting check, table generator `--check --cr`, and `check-kgw-fp.sh`: pass.
- Independent MPFR/exact-rational KGW analysis: pass, including the larger-error falsification above.
- Fresh adversarial public-API probes: **1,744 next-work comparisons** and **2,000 subsidy-split comparisons** against the individually selected pins; **24** documented parameter exclusions verified. The probes include devnet signed factor endpoints and overflow-producing factors, both V20 states, and changed regtest budget/activation parameters.
- Fresh-seed live differential: **1,778 next-work**, **1,580 subsidy**, **1,100 PoW**, **1,100 MN-payment**, **1,000 difficulty**, **1,000 tolerance**, **1,000 platform-share**, **577 payments-limit**, and **100 V24 shared-payout** comparisons pass. Oracle refusals are tallied separately.
- Full header-history reruns at this HEAD: **2,551,767 mainnet headers** and **1,568,460 testnet headers**, with dump SHA256, X11 ancestry, and Core hash anchors checked. All **2,551,766 / 1,568,459** successor subsidy and tolerance checks agree; mainnet's **68,589** early next-work decisions agree with both pins, including all **18,940 KGW heights**. Later mainnet and every testnet next-work word match their headers. Both sweeps finish with their exact recorded tallies. These are header/subsidy checks, not coinbase payout-assembly history.
- Small exhaustive slices at the current HEAD: **262,144** difficulty words and **393,216** subsidy inputs across all six sweep points match both pins, including the subnormal onset and the top of the word range. The full `2^32` sweeps were not repeated.
- Audited retained full-sweep logs: all **96** subsidy slices have contiguous, nonoverlapping coverage of six complete `2^32` domains and successful completion; the bits log covers its complete domain. Verified the frozen subsidy's SHA256. The snapshots correspond to earlier commits; the later subsidy changes are anchors/removal of a display test, and the later FP changes do not change difficulty or subsidy arithmetic. Thus byte-different source hashes do not conceal a changed swept formula.
- Regenerated **36 release listings** from the retained official binaries (all six platforms at both pins), byte-identical to the evidence listings; **152 assertions pass**. Artifacts are under `release/`. This reused existing binaries and indices; I did not repeat downloads/signature verification or run macOS binaries.
- The four scanner reproductions return no offenders as described, and the three confinement escape programs compile and run with the current Clippy bans. These are deliberately successful escape probes, not fixes to the branch.

The review does not repeat the whole workspace build, benchmark the reported 1.8× speedup, or reopen the accepted Apple accuracy/FP-environment assumptions. The existing upper-side end-to-end tolerance verdict at height 68590 still belongs to CS3/O2a; here its predicate and switch are checked against source and recorded tests.

## Short summary

The FP arithmetic, casts, KGW proof, supported input domain, integer truncation, and release-pin handling are sound on the evidence reviewed. Require the three small gate fixes above before merging phase C; no arithmetic rewrite or new full sweep is indicated. No implementation edits, commits, pushes, GitHub messages, or running-node changes were made.

**Verdict: CHANGES-REQUIRED.**
