# CS2 PoW, floating point, subsidy and payments — independent review, round 1

**Verdict: APPROVE-WITH-CHANGES. No blocker; four major findings.** The 4032-entry KGW table is a sound approach, and I reproduced the decision equivalence and 35,751-ulp margin. Retain the integer ports and the table. Correct the admissible-domain claims, qualify or complete the platform proof, and close the verification gaps below before calling the design gate complete. These are design findings; no CS2 Rust implementation was reviewed.

Reviewed `designs/CS2-pow-subsidy.md` (SHA256 `dc7ccbd76e4550a18d6b071e77bb562808e184b0ac07cf2e37b88630762031da`) and `designs/CS2-fp-release-binaries.md` (SHA256 `1e644c6a6047ae8cc9193065a09a311dfdf35382ee910aa0417f560f972d6877`), their evidence tools/listings, DEC-29, A1 §2.4/§4.5, R4 §1.5–1.7 and R4-B/C. Compared Core **v23.1.8** (`728f5055836c6d29806412fc7223ac8fe05af991`) and **v24.0.0-rc.3** (`0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`), including the older locations of MN arithmetic and superblock limits. Inspected base-sdk `33fcc59`'s `Arith256` and compact encoding, and the O1 shim's input guards. Applied the `dash-core-security-auditor` workflow. DEC-29's ownership and implementation-phase decisions are not reopened.

All reproduction downloads, generated listings, probes and logs are under `/work/scratch/cs2-review-gpt/`. This review task writes only this review file in the repository; it does not modify designs, decisions, source or evidence.

## Major findings

### M1 — Recorded history is not the admissible subsidy domain

**Severity:** Major.  
**Section:** CS2 §0 item 3; §2.5 F2, particularly lines 208–212; release report §5.  
**Core:** v24 `validation.cpp:4496–4504` checkpoint check, and the corresponding v23 check; both pins' subsidy helpers.

**Problem.** “A fork … would have to out-mine the real chain” does not establish impossibility or consensus invalidity. Checkpoint rejection depends on a checkpoint hash already being present in the node's index, and can be disabled. Minimum chain work is a work threshold, not a commitment to the transactions, targets or ancestry of the recorded chain. Before a checkpoint is present, an alternative ancestry can meet that threshold; imported/reindexed blocks also do not become a mathematical impossibility merely because their ancestry is expensive. A1 §4.5 expressly requires more than recorded history.

Consequently, the finite list of recorded pre-V20 `(prev_bits, prev_height)` pairs is a history test corpus, not a closed admissible domain. The mainnet tolerance rule itself permits targets absent from recorded history. This is particularly relevant when reasoning about intermediate floating-point values, not just final historical subsidies.

This does **not** reveal a subsidy arithmetic divergence: replacing the squares with `x*x` is supported by all four binaries, and the proposed all-`u32` sweep covers alternative bits. The flaw is the exclusion proof used to declare the gate closed.

**Fix.** Remove the alternative-history exclusion. Give a numerical proof over a superset of every valid compact input, plus the artificial superblock-limit input `nBits = 1`, and retain history as separate validation. A simple conservative domain suffices: for nonzero mantissas and exponents through 34, `65535/mantissa`, scaled by the stated powers of 256, stays normal, positive and finite; its maximum is about `4.5231e74`. Every formula's square is finite, its denominator is positive, and its quotient lies in `[0,1111]` before the integer cast. The `nBits = 1` superblock input is included in that bound. Zero mantissas give positive infinity and then a formula result of zero, without NaN. Keep the regtest induction and the devnet V20/configuration analysis, which do use actual dispatch rules. Make completion of §6.3 an explicit gate, rather than using a history exclusion as a substitute for it.

Also replace the near-`INT_MAX` assertion that such heights cannot exist with an exclusion based on Core's actual signed-overflow boundary. The O1 guard is `prev_height < floor(INT_MAX / interval) * interval`; computational cost is not an unreachability proof, especially on regtest.

### M2 — Valid noncanonical compact encodings invalidate the tolerance exponent bound

**Severity:** Major.  
**Section:** CS2 §2.5 F3, lines 215–218; §3.2 `check_proof_of_work`; §6 tolerance/PoW coverage.  
**Core:** both pins `pow.cpp:239–256`, `arith_uint256.cpp` `SetCompact`; v24 `validation.cpp:4476–4489` and v23 `:4110–4123`.

**Problem.** `CheckProofOfWork` checks the **decoded target** against `powLimit`. It does not require the compact encoding to be canonical, or compare the raw `nBits` word with `GetCompact(powLimit)`. Thus a successful mainnet PoW check does not imply an exponent at most 30.

These three words all decode to `2^232`, below mainnet's `2^236−1` powLimit, with neither negative nor overflow flags:

| `nBits` | Exponent | `ConvertBitsToDouble` |
|---|---:|---|
| `0x1e010000` | 30 | `0x1.fffe000000000p-9` |
| `0x1f000100` | 31 | `0x1.fffe000000000p-9` |
| `0x20000001` | 32 | `0x1.fffe000000000p-9` |

For a qualifying hash, the latter two pass `CheckProofOfWork`. Against expected bits `0x1e010000`, both also pass the pre-68590 tolerance with exactly zero difficulty difference. The later exact `nBits` rule rejects the alternate words. F3 therefore needs up to **three** `/256` steps on the candidate, not one. This remains far from subnormal or overflow; the numerical conclusion survives with corrected bounds.

The raw bits also matter to the early subsidy bug: at mainnet `prev_height <= 4500`, `0x1e010000` gives a base subsidy of **277 DASH**, while `0x20000001` gives **1 DASH**, despite their identical PoW target and tolerance difficulty. Normalizing stored header bits would change consensus-visible subsidy behavior.

**Fix.** State the candidate exponent bound as 32 on mainnet, accounting for noncanonical words. Expected bits are canonical when computed with `GetCompact`, but BTC between retargets can pass through a previous noncanonical word, so the expected side needs that possibility too. Specify the PoW predicate explicitly: reject negative, zero, overflowing or numerically over-limit decoded targets, then accept iff the little-endian hash value is **at most** the decoded target. Preserve raw header bits; do not add canonicalization. Add the three encodings above to Core-derived tests of PoW, `ConvertBitsToDouble`, early subsidy, tolerance and the 68589/68590 switch, with `hash == target`, `hash == target+1`, and negative/overflow cases. Include zero expected target as a tolerance rejection case.

### M3 — “Every shipped build” is broader than the demonstrated libm/platform matrix

**Severity:** Major, proof coverage; no observed divergence.  
**Section:** CS2 §0 items 1–2, §2.4 test 1, §8 deviations; release report §0 items 3/6, §4.1–4.2.  
**External source:** glibc 2.31 `sysdeps/x86_64/fpu/multiarch/e_pow.c` and `ifunc-fma4.h`.

**Problem.** The report presents x86_64 glibc as having only FMA/AVX2 and SSE2 implementations. glibc 2.31's `pow` selector also chooses **FMA4** when `FMA4_Usable` is set and AVX2/FMA is unavailable, as on older AMD CPUs. Neither the default Broadwell run nor disabling AVX2/FMA exercises that branch. Those Linux dashd release binaries can bind to it. Separately, the four inspected artifacts are Linux artifacts, while DEC-08 includes macOS/Windows at tier 2. A test using the current host's `pow` plus `mul_add` does not itself execute another architecture's libm or a tier-2 release binary.

The missing FMA4 case looks safe: its source includes the same generic algorithm as FMA, with fused operations, and that algorithm documents a worst-case error of 0.54 ulp. But this source/error-bound argument is not in the supplied proof. The Linux measurements establish equivalence for the measured matrix, not by themselves for every shipped runtime combination.

**Fix.** Separate the binary operation-shape claim, measured libm results, and general error-bound proof. Add an in-repo interval check around the correctly rounded table, with both fused and separate horizon arithmetic, and tie each additional runtime implementation to a documented bound. This can cover FMA4 without relying on access to an old AMD host. Pin/cite the selector and bound sources. Explicitly scope the current binary conclusion to the two Linux architectures and pins; add tier-2 shape/error evidence or record it as a required platform follow-up instead of claiming it already exists.

I independently checked every correctly rounded value and its adjacent binary64 neighbors with both horizon shapes: all **24,114** combinations have the same thresholds. I also checked an outward-expanded interval of relative `pow` error **±1.5e−9**, with four extra ulps at each endpoint, for all **16,076** endpoint/shape combinations. The thresholds remain unchanged; monotonicity in positive `p` covers the interiors. Thus the proposed bound is defensible and supplies a straightforward repair. My FMA4 emulation attempt did not exercise FMA4: this qemu build masks out that CPU feature. Its resulting table must not be labeled FMA4 evidence.

### M4 — The promised differential coverage omits payment splits and the tolerance predicate

**Severity:** Major, verification/contract gap.  
**Section:** CS2 §5 APIs and overflow paragraph; §6.1 properties, §6.2 target table; §7 oracle requirements.  
**Core:** v24 `masternode/payments.cpp:164–190`, `evo/providertx_util.cpp:88–117`; v23 operator split in `masternode/payments.cpp`; both header tolerance predicates.

**Problem.** `diff_subsidy_payments` lists subsidy, treasury, MN payment and platform share, but not `operator_reward`, `owner_payout_amounts`, `ProRataFloor` or `split_amount_by_shares`. The named L0 BRR vectors also do not verify these helpers. This leaves the intentional wrapping multiplication, signed truncation, owner remainder and the unsigned 128-bit intermediate outside the stated oracle comparison. `diff_next_work` compares the expected bits, not `nbits_acceptable`; checking that recorded headers pass does not compare rejection behavior at the tolerance boundaries.

The generic split API also needs a precise domain. Core returns an empty vector for empty shares, so the unconditional property “sums to total” is false for `total != 0` and empty weights. Core casts each `ProRataFloor` operand through `uint64_t` before multiplying/dividing its U256 value. A Rust `i64 as u128` cast has different semantics for negative inputs; either restrict the helper to validated nonnegative shares or state the exact casts. Nonempty positive collateral shares make the u128 reduction sound, but that precondition should be explicit.

Finally, `MoneyRange(nFees)` does not establish `MoneyRange(subsidy + fees)`. The sum must be bounded separately, including devnet high-subsidy overrides. For ordinary MoneyRange rewards, the ×600 and ×375 products are comfortably within i64, but that fact is not a proof for all configurations accepted by the subsidy parameter view.

**Fix.** Add pinned Core-derived fixtures or shim probes for the split helpers, plus an independent tolerance predicate reference. Include awkward satoshi remainders, one/multiple owners, final-owner remainder, operator bps 0/1/9999/10000, and rewards immediately below/above `floor(INT64_MAX / bps)` where wrapping is deliberately specified. Test valid positive share weights with a product above 64 bits; define empty, zero-total and invalid-weight behavior without silently normalizing weights or changing Core's remainder rule. Restrict the sum property to its valid nonempty domain, and test Core's empty result separately. Check both sides and equality of the ±50% boundary, noncanonical encodings from M2, zero expected target, and exact checking at 68590/on other networks. Write the caller/configuration bounds for `subsidy + fees` and the larger accepted high-subsidy settings, distinguishing Core overflow/assert cases from valid ordinary inputs. These can be additional verification requirements without changing DEC-29's approved two oracle additions.

## Minor findings

### m1 — The proposed confinement command scans the allowed `fp` directory

**Severity:** Minor. **Section:** §2.6, line 241.  
**Problem.** From the repository root, `rg -w 'f32|f64' dash-consensus/src --glob '!fp/**'` does not exclude `dash-consensus/src/fp/mod.rs`. A scratch reproduction with a single legitimate `f64` declaration reports that file, so the proposed gate fails when `fp` lands.  
**Fix.** Use an exclusion matching the actual path, such as `--glob '!**/fp/**'`, or run from `dash-consensus/src` and search `.` with the appropriately rooted glob. Verify that an allowed `fp` hit is excluded and an outside hit is reported.

### m2 — The division overload explanation is stale at both pins

**Severity:** Minor. **Section:** §0 item 4, §3.7.  
**Problem.** Both reviewed `arith_uint256.h` files already expose `operator/=(uint64_t)` and `operator/(..., uint64_t)`. These calls use the integer overload directly; the u64-to-`base_uint` constructor is used inside the division implementation for a divisor above u32, not as the default overload-resolution path.  
**Fix.** Correct the explanation. Retain `/ (x as u64)`: its result is correct. Retain the actual multiplication trap: `*= int64` selects `*= uint32_t`, with modular truncation.

## What the review verified

**KGW domain and crafted chains.** In both tags, `PastBlocksMass` starts at zero and increments once per loop iteration. `i > PastBlocksMax` is checked before incrementing or calling `pow`; on reachable mainnet KGW, `PastBlocksMax = 86400*7/150 = 4032`. Genesis is excluded. Test/devnet dispatch directly to DGW at their common KGW/DGW height; regtest returns before KGW. No supported override changes this. Header bits affect the integer target average, and timestamps affect `A` and earlier exit, but neither changes `x = fl(m/28.2)` or `y = fl(-1.228)`. Non-monotonic timestamps only clamp `A` to zero; `A` otherwise lies through `2^32−1`. `T = 150m` and `A` cast exactly to binary64. Equal 150-second spacing keeps every eligible mass below the horizon through 4032, which the independent probe checked. Crafted headers cannot escape the table on a supported network.

**Boundary proof.** For fixed mass and horizon, `fl(T/A)` is monotone in positive integer `A`; testing the fast/slow transitions therefore covers all positive timestamp gaps. `A=0` uses the literal ratio 1 and never crosses. The reproduced bisection thresholds agree with independent analytic floor/ceil thresholds calculated with 256-bit MPFR and checked at their IEEE edges. The minimum margin is **35,751 ulps at mass 2712, gap 407858**, including correctly rounded, measured-libm, fused and separate combinations. The independent sensitivity estimate is about `1.53410825e−9`; the outward-expanded endpoint test confirms the report's more conservative `1.5e−9` claim. All 4032 recorded correctly rounded entries also match a separate 256-bit MPFR evaluation.

**Subsidy and tolerance operations.** All four binaries fold the subsidy's squares into `mulsd`/`fmul`; the aarch64 CPU/GPU formulas share a tail. There is no subsidy FMA, x87 or libm call. `ConvertBitsToDouble` uses the full low **24** bits, while target decoding masks the sign bit to 23 bits: keep these distinct. Its power-of-256 loop order, the early mainnet no-exponent bug, double comparisons, truncation **before** clamps, previous-height/previous-bits inputs, integer yearly decline and strict treasury-start `>` are correctly described. `fabs(round(n1−n2)) > n1*0.5` is the actual tolerance shape on both architectures; `fabd` is the corresponding rounded difference/absolute value. Rust's float-to-i64 saturation does not change the finite formula outputs on the broader domain in M1. Zero expected target gives infinite expected difficulty and rejects against a finite valid header difficulty.

**Libm scope.** Repeated the math-function source search at both pins and inspected the regenerated FP listings. KGW's `pow` is the only libm call feeding the CS2 PoW/subsidy/payment decisions. The three source-level subsidy `pow` calls disappear in the inspected binaries. Payment/superblock-limit arithmetic contains no hidden libm call. Validation's work logarithms are logging; LLMQ's `log2` and retry-timing `exp2` are outside CS2. DEC-29 D4 already assigns the former to LQ1; this review does not claim all node consensus code is libm-free.

**Integer ports.** The design preserves DGW's 24 targets/23 timestamp intervals, its `height < 24` early return, pseudo-average `(avg*k + target)/(k+1)`, wrapping multiply/add and truncation at each step. KGW's ordered unsigned subtraction avoids underflow; its final multiplier still truncates to u32. BTC keeps its historical ancestor interval and clamped multiplication/division order. The min-difficulty dispatch order and strict time comparisons are correct: BTC `2*spacing`; post-BTC fixed 7200 seconds before `4*spacing`; ×10 wraps before the powLimit cap. The devnet spacing cases at `2^29−1`, `2^29` and beyond correctly expose the live u32 truncation. Base-sdk add/sub/multiply and oversized shifts have the described wrapping/zero semantics, and compact flags match Core. Both pins' division algorithms have the same mathematical quotient. The `no_retargeting` distinction between BTC returning previous bits and later dispatch returning powLimit is preserved.

**Subsidy/payment schedules.** The iterative decline with a fixed point below 14 is sound because the devnet factor is applied after it. The MN schedule's separate truncations, missing period 8, strict threshold comparisons, next-superblock-cycle BRR start, table cap, V20/era mapping and subsidy-only platform share agree with both pins. `GetPaymentsLimit` uses artificial bits 1 on mainnet and canonical powLimit where min difficulty is allowed, and tests V20 at the requested height. The operator split remains signed integer multiplication followed by division toward zero; both inspected x86_64 payment listings use a low-64-bit `imul` followed by signed constant division, supporting the proposed wrapping multiply, but this still needs the verification in M4. Shared positive collateral-weight products fit u128; the final share receives the remainder.

## Reproduction and limits

Ran the supplied `reproduce.sh /work/scratch/cs2-review-gpt`, seeded with copies of the existing release downloads. All four tarballs passed the release SHA256 checks; the script rebuilt the indices/listings, native and aarch64 probes, and the glibc 2.31/2.36 container tables. Python bytecode writes beside the reviewed tools were disabled. Results:

- **52 PASS, 0 FAIL** listing checks; all 12 regenerated FP excerpts byte-identical to the committed excerpts.
- Correctly rounded table, integer thresholds, listing-check output and `kgw-analysis.txt` byte-identical to committed outputs. `kgw-margins.txt` differs only in the final printed output pathname (`out/kgw-pow-cr.txt` versus `kgw-pow-cr.txt`); numerical results are identical.
- FMA/aarch64 differ from correctly rounded pow at the six documented masses; SSE2 at the five documented masses, always by one ulp. The FMA/SSE2 tables differ at three masses as reported. The container tables reproduce the per-variant equality across the sampled glibc versions.
- Independent checks/logs: `independent_kgw.py`, `independent-kgw.txt`, `independent_error_bound.py`, `independent-error-bound.txt`, plus `reproduce.log`. glibc FMA4 selector/implementation sources and the explicitly unsuccessful FMA4 execution attempt are retained under the same scratch directory.

Did not run the planned 2^32-input subsidy/conversion sweeps, a complete history sweep, or Rust implementation tests: those are future implementation gates, not existing results. Did not establish the binary shapes of macOS/Windows artifacts or execute FMA4 hardware. No floating-point decision mismatch was found in the reviewed Linux matrix or the independent bounded-error checks. The remaining changes concern proof completeness, admissible inputs, and regression coverage; they do not require abandoning the KGW table.
