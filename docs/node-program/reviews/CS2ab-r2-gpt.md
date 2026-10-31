# CS2 phases A+B — GPT implementation review r2

Reviewed 2026-10-08 by GPT-6.1-Sol under `briefs/REVIEW-COMMON.md`, fulfilling DEC-49's pre-merge review. Scope: `node/cs2-pow-subsidy` at **ee8203db8ff81d9fb05c6f42b160aa4dc7964c83**, based on **b6f7aeb5941431d2bd0052999aece458e71890b1**. Compared `pow/`, `subsidy.rs`, and `payments.rs` with the r1 code at **d88308d8**, and independently reviewed the new constructors, shared parameters, differential harness, and FP confinement test.

Read CS2 rev1, the relevant A1 sections, the program process/decisions, and both earlier implementation reviews. Applied the `code-review-validator` skill. Core source reference: **v23.1.8 (728f5055)** and **v24.0.0-rc.3 (0364ed62)**. The live oracle reported `v23.1.8` and `v24.0.0-rc.3-5-g23769258c239`; the latter includes the pinned oracle patches, as expected. Runs used `TMPDIR=/dev/shm/cs2-gpt2-tmp` with mode 0700 and `CARGO_BUILD_JOBS=12`.

**No blocker or major. The refactor preserves the consensus decisions implemented by A+B. The FP scan is improved substantially, but it can still be bypassed: two minor findings below need closure before phase C relies on it.**

## Findings

### m1 — minor: the tuple-field exception hides float literals in macro arguments

**Location:** `dash-consensus/tests/fp_confinement.rs:69–77`, specifically `dots != 1` at line 72.

The scanner treats every literal after one dot as a tuple-field access. Macro input is an arbitrary token stream, so this is valid Rust outside `fp`:

```rust
macro_rules! less {
    ($ignore:tt $a:literal, $other:tt $b:literal) => { $a < $b };
}
fn outside() -> bool { less!(. 0.5, . 1.5) }
```

The dots are discarded by the macro, leaving a native float comparison outside `fp`. The unmodified `float_lines`/`has_float` implementation reports no offender. I compiled and ran this code successfully, and reproduced the missed detection with the exact scanner copied into a scratch test. This is beyond the documented general macro-expansion limit: rule 1 specifically promises to inspect macro arguments, but its dot exception discards their actual float tokens. Clippy's float-arithmetic lint does not replace a float-comparison check.

**Suggested fix:** handle macro token streams conservatively, without the tuple-field exception. For ordinary Rust expressions, exempt actual tuple-field syntax using the parsed expression structure rather than the preceding punctuation alone. Add this probe while preserving the existing `t.0.1` negative test. There is no such offender in the current production source; this is a gate defect, not an A+B consensus mismatch.

### m2 — minor: ordering escapes through trait renaming and conditional derives

**Locations:** `dash-consensus/tests/fp_confinement.rs:122–125`, `:129–135`, `:145–146`, and `:210–214`.

Both of these independently pass rule 2 in `src/fp/mod.rs`:

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
#[cfg_attr(all(), derive(PartialOrd))]
pub struct Difficulty(f64);
```

With the permitted `pub fn difficulty(x: u32) -> Difficulty`, callers outside `fp` can write `fp::difficulty(a) < fp::difficulty(b)` without any float token. The first escape works because private `use` statements only check primitive float renames, and the impl header subsequently contains `Compare`. The second works because `check_derives` reads only attributes whose path is literally `derive`. `cfg_attr` is a compiler attribute, not an unknown procedural macro.

I compiled and ran both complete examples and verified that the exact scanner reports no offenders. The same impl written as `impl PartialOrd` is correctly rejected. Both escapes violate the scan's explicit ban on exporting float ordering and CS2 §2.3's opaque-type API.

**Suggested fix:** reject renaming `PartialOrd`/`Ord` in `fp` at every visibility, as already done for primitive float aliases; recursively inspect `cfg_attr`'s nested attributes for forbidden derives. Add both probes. These affect the forthcoming `fp` API; neither exists in the current A+B source.

## Refactor equivalence

| Change since d88308d8 | Independent assessment |
|---|---|
| `HeaderCtx` gains CS0 fields; `header()` returns a value; `ancestor()` returns `Option` | PoW still reads only height, time, and the raw compact word. The new fields do not enter any decision. Under the ancestry contract, each needed ancestor exists and has the same fields. |
| BTC min-difficulty walk caches the current header | Its loop predicate, walk order, and returned raw `bits` remain identical. Returning a value rather than a reference does not canonicalize it. |
| BTC retarget handles a missing ancestor | Valid chains follow the original retarget calculation. Missing ancestry takes the existing broken-contract policy, with both debug and release paths now tested. |
| `nbits_acceptable` calls `tolerance_applies` | Same height comparison and same equal-word shortcut; unequal words defer only on mainnet through 68589. |
| Parameter views now use `from_chain_params` and `DevnetOverrides` | Raw Core fields are copied; tolerance, network subsidy paths, and payments-limit bits are derived from the same Core conditions. Non-default overrides off devnet and unknown network IDs fail closed. |
| Shared `Superblocks` | Same positive-cycle validation, valid-height predicate, and cycle value used by both payments-limit and BRR scheduling. |
| Outer subsidy now takes `Option<&HeaderCtx>` | Same genesis reward, previous-height conversion, caller-supplied V20 state, and raw previous compact word. |
| DGW, PoW validity, decline/treasury arithmetic, MN schedule, platform/operator/owner arithmetic, and shared-payout arithmetic | Unchanged. `dgw.rs` has no diff; the other arithmetic changes are comments or replacing the cycle field with its accessor. |

The r1 overflow-comment nit is fixed without changing arithmetic. Fixed-base subsidy and existing integer answers remain real answers; KGW, difficulty-derived subsidy, and unequal early-mainnet tolerance remain explicit `FpPending` results.

## Independent validation

All commands below completed successfully unless a deliberate refusal is stated. Logs and probe sources are in `/work/scratch/cs2-gpt2/`.

| Check | Result |
|---|---|
| `cargo test --locked -p dash-consensus --all-features`, debug and release | **48 tests passed** in each configuration, plus successful doc-test target. Includes ancestry failures, constructors, parser/lexer self-tests, recorded oracle cases, history samples, Core vectors, and payment/share arithmetic. |
| Targeted all-target/all-feature Clippy with `-D warnings`; `cargo fmt --all --check`; `contrib/node/check-pure.sh` | All pass; pure gate reports **2 crates**. |
| `contrib/node/cs2-diff/run.sh record dash-consensus/tests/data --check` | **29 files checked**, exit 0; committed lockfiles and corpus untouched. |
| `run.sh sweep main /work/scratch/cs2/headers/mainnet.bin` | Requested counts reproduced exactly; see below. |
| `run.sh sweep test /work/scratch/cs2/headers/testnet.bin` | Requested counts reproduced exactly; see below. |
| `run.sh diff 2000 86753092026` | Fresh seed: **21,424 next-work**, **22,000 PoW**, **22,000 MN-payment**, **25,754 subsidy**, **558 payments-limit**, and **20,000 platform-share** agreements. Four KGW windows explicitly deferred; 572 short windows refused. Formula deferrals and expected Core assertion/loop-overflow refusals are separately tallied. |
| Additional constructor/decision probes at both pins | **288 parameter selections**, **36,300 next-work**, **4,992 subsidy-split**, and **8,640 MN-payment** comparisons agree. **66** PoW configurations meet documented constructor exclusions. |
| Adversarial sweeps | A 100,000-header mainnet truncation fails on the dump digest; with `--unpinned` it still fails before V20. Flipping `hashPrevBlock` at height 1234 fails the X11 link check even with `--unpinned`. All exit **101**, not success. |
| FP adversarial probes | Three missed detections reproduced; all corresponding complete Rust examples compile and run. Details in m1/m2. |

Mainnet dump SHA-256: `58e6e286297e40e9b9e3afa1e5a734c27bd76be065631fb27d3a5653bf5f8251`. All 2,551,767 headers link by X11; **43** Core block-hash anchors match. Results:

- Exact header `nBits` agreement from 68590: **2,483,177**.
- Early-mainnet next-work agreement with **both** Core pins: **49,649**; **15,182** equal header words and **34,467** differing words. Highest differing height: **68588**.
- KGW deferrals: exactly **15200..=34139**, **18,940** headers.
- V20+ subsidy agreement with both pins: **563,991**.
- PoW accepted for all **2,551,766** headers checked; the tip lacks a successor for this check.

Testnet dump SHA-256: `a88526024d3c3a2a82c797bd6348a8789ce5ee388ecf33742f3987e414659b27`. All 1,568,460 headers link by X11; **22** Core anchors match. Exact next-work agreement and PoW checks: **1,568,459** each. V20+ subsidy agreement with both pins: **663,360**. Both sweeps assert the complete recorded tally before exiting successfully.

## Devnet/regtest constructor attacks

The scratch probe independently copies Core's JSON into `ChainParams` and calls the public constructors; it does not use the in-repo JSON-to-view helper for that mapping. Its chain storage and genesis decoder reuse the existing test utilities. Decisions are compared live with each pin separately.

- Devnet minimum-difficulty values include `i32::MIN`, −1, 0, 3999, 4000, 4001, `i32::MAX`, 2³², and 2³²+1. Core's integer truncation produces the expected overrides, including wrap to 0/1.
- Spacings include 1, 150, 86400, 86401, 2²⁹, 2³¹, `i64::MAX/72`, and one above. Spacing above 86400 is accepted exactly when the minimum-difficulty window bypasses BTC; overflowing spacing is refused. Comparisons cover genesis, the DGW warmup, BTC retarget height 575, heights around 4000/4001, regtest's 15200 switch, and strict timestamp boundaries around 2×spacing, 4×spacing, and 7200 seconds. Non-canonical previous compact words are retained.
- High-subsidy block/factor combinations include negative and zero values, `i32` endpoints, and 2³²/2³²+1 truncation. The fixed-base and V20 split decisions agree with Core, including accepted signed factors.
- Regtest budget/superblock starts include 0, 1, 20, `INT_MAX−1`, and `INT_MAX`; BRR/V20 activation includes cycle-boundary and near-`INT_MAX` values. Selections obey Core's V20/DIP3 and MN_RR/V20 startup assertions. Zero spacing, negative spacing, invalid negative activation, and activation at `INT_MAX` are rejected by Core at both pins.
- Core accepts negative regtest budget starts at both pins. These remain the explicitly documented DEBUG_ONLY exclusion: unsigned `BlockHeight` cannot represent them, and BS5 must refuse rather than clamp them. This is not a newly hidden parity gap.
- Each non-default override field is independently refused on mainnet, testnet, and regtest. Unknown network IDs are refused in every view. `devnet` is the correct Core network ID; the devnet name remains separate.

Checked these against `chainparams.cpp` at both tags: defaults, devnet `GetIntArg`→`int` assignments, `ParseInt64` spacing and its positive bound, regtest `ParseInt32` budget starts, and activation-height validation. Also checked Core's subsidy network checks, the 68589 rule, and `GetPaymentsLimit`'s synthetic bits choice and shared schedule.

## Limits and handoff

This approves **A+B**, not phase C/D parity. The full KGW FP implementation, all-u32 subsidy/ConvertBits sweeps, tolerance boundary differential, live shared-payout differential, historical payout assembly, and fuzz/performance merge gates remain their existing follow-up work. I did not rerun the whole SDK workspace, repeat the CS0 trial merge, or add a performance comparison. No running dashd was queried or modified.

The parser rewrite closes the earlier trait/enum/literal/raw-string probes, but m1/m2 mean the scan cannot be called bypass-proof. Its documented dependency-inferred-float and source-path limitations also remain; its current source passes, and none of the new escapes is present in production.

Reviewed HEAD and worktree remained unchanged and clean. No implementation edits, commits, pushes, or GitHub messages. Scratch evidence is retained; `/dev/shm/cs2-gpt2-tmp` was removed after the runs.

**Verdict: APPROVE-WITH-NITS.** No blockers or majors for merging the provisional A+B implementation. Close m1/m2 before phase C relies on FP confinement.
