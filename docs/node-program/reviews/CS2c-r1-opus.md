**Verdict: APPROVE-WITH-NITS.** No blockers and no majors. The five minors are cheap, and two of them (findings 1 and 2) should land with the CS0 rebase.

# CS2 phase C (`fp`, KGW, tolerance, subsidy bases): Opus review r1

| | |
|---|---|
| Branch | `node/cs2-fp` at `46cf12a3`, 13 commits on `d44631bb`. Reviewed in fresh detached worktrees `/work/scratch/cs2c-opus/{repo,mut}`; the branch was not touched |
| Brief | the second independent pre-merge reviewer; GPT is attacking bit-exactness and the KGW proof in parallel. Focus: re-verify the evidence independently, code quality, and whether design §2/§6.3/§10 acceptance is met |
| Read | `briefs/REVIEW-COMMON.md`, `designs/CS2-pow-subsidy.md` (rev1), `designs/CS2-fp-release-binaries.md`, `designs/CS2-fp-evidence/`, DEC-29/33/35/41/44, `reviews/CS2*`, memory `dashd-release-fp-findings`, the implementer's `/work/scratch/cs2c/` |
| Core | v24.0.0-rc.3 `pow.cpp:15-79`, `validation.cpp:1784-1870` and `:4474-4489`; v23.1.8 `:1458-1544` and `:4106-4124` |
| Oracle | `v23.1.8` and `v24.0.0-rc.3-5-g23769258c239` (the pinned archives from `pins.txt`; every run printed both) |
| Scratch | `/work/scratch/cs2c-opus/`: logs under `sweeps/`, `hist/` and `*.log`; my tools under `tools/` and `histcheck/`. TMPDIR was `/dev/shm/cs2c-opus-tmp` (0700), removed at the end |
| Load | 24–88 from other threads. My heavy runs used ≤ 8 threads (`nice 10`) and were paused while load was above 70 |

---

## Findings

### 1. minor: the `%`/`Rem` ban can be bypassed by a glob import or a macro, and nothing at link level backs it up

**Where:** `dash-consensus/tests/fp_confinement.rs:29-31`, `:285-302` (`remainder_lines`); `dash-consensus/clippy.toml:28-109`; the crate doc at `dash-consensus/src/lib.rs:24-26`, which claims the test "keeps … `%` (libm's `fmod`) out of it".

**Failure scenario (demonstrated in the scratch worktree `mut`, then reverted):**
- **Glob import.** `use core::ops::*;` in `fp/mod.rs`, then `difficulty = difficulty.rem(1.0e300);` in `Difficulty::from_bits`.
  - `cargo clippy -p dash-consensus --all-features --all-targets -- -D warnings`: clean.
  - `cargo test --test fp_confinement`: 2 passed. Rule 3 looks for the `%` punct and the idents `Rem`/`RemAssign`, never the method name `rem`.
  - The release rlib then has `U fmod`.
- **Macro.** `macro_rules! remainder { ($a:expr, $b:expr) => { $a % $b } }` in `pow/mod.rs`, plus `pub(crate) use remainder;`, then `Self(crate::pow::remainder!(difficulty, 1.0e300))` in `fp`.
  - Clippy is clean: `float_arithmetic` is allowed at the expansion site.
  - `fp_confinement` passes: rule 3 reads only `src/fp/` files, and rule 1 does not look for `%`.
  - The rlib has `U fmod`.

The implementer flagged the glob case themselves. There is no violation today: the clean HEAD rlib's only undefined non-Rust symbols are `__udivti3` (the u128 division in `div_u64`, from compiler-rt) and `rust_eh_personality`. But this is the consensus crate's libm guard, and one of the rule's three ban routes has two holes.

**Fix (both parts tested):**
- (a) Add these entries to `clippy.toml`'s `disallowed-methods`:
  - `{ path = "core::ops::Rem::rem", reason = "a float remainder is libm fmod (CS2 §2.2)" }`
  - `{ path = "core::ops::RemAssign::rem_assign", … }`

  With them, clippy flags the glob mutation (`use of a disallowed method core::ops::Rem::rem`). The crate's integer `%` (in `pow/`) is not flagged, because it is a built-in operator, not a method call. This closes the method route; it does not see a `%` that a macro expands.
- (b) Add a link-level check, which closes every route, macros and future dependencies included. Build `dash-consensus` in release and require that `nm -u` on the rlib lists none of:
  - `fmod`, `pow`, `exp*`, `log*`, `sqrt`, `cbrt`, `fma`, `fdim`;
  - `floor`, `ceil`, `round`, `trunc`, `rint`, `nearbyint`;
  - the trig and hyperbolic functions;
  - each name's `f` suffix variant.

  It could be a `contrib/node` script in the `node-pure` CI group, or a `#[ignore]`d test that `check-pure.sh` runs. It took about one minute here. It would have caught both mutations above.

### 2. minor: the sweep evidence is not tied to a commit, and the "no later change" sentence in PROVENANCE is imprecise

**Where:** `dash-consensus/tests/data/PROVENANCE.md:115-127`, in particular `:127`: "with this branch's `fp` and subsidy code, which no later change touched".

**What I found:**
- **Both sweeps ran from a dirty tree.** `sweeps/commit` = `d44631bb` and `sweeps/dirty` lists 16 modified files.
- **The bits-sweep binary was not frozen.** Its hash (`a220b633…`) was recorded at `contrib/node/cs2-diff/target/release/cs2-diff`, which has since been rebuilt (`88c85737…`). Only the subsidy binary is frozen. `/work/scratch/cs2c/bin/cs2-diff` hashes to `9f680c5e…`, matching `subsidy-binary.sha256`.
- **The recorded source hashes match no commit.**
  - The best matches are 18/19 files (bits: `6c2f761c`; subsidy: `7550519d`/`b0299fc9`). At `46cf12a3` only 5/19 match.
  - The code under test does match: the sweeps' `fp/mod.rs` and `subsidy.rs` equal `0c52198c`'s, and `kgw_pow.rs`, `params.rs` and `pow/dgw.rs` equal HEAD's.
  - `fp/mod.rs` and `subsidy.rs` *did* change afterwards:
    - `1eea1fb5`: `KGW_MAX_MASS`, `KgwMass` and `kgw_horizon_crossed(…, actual_secs: u32)`, and a removed `subsidy` test;
    - `dc46e164`: comments only.
  - I diffed every function the sweeps exercise (`Difficulty::{from_bits, from_bits_mainnet_early, at_most_75, subsidy_base_*}`, `truncate`, `block_subsidy_helper`, `decline`, `pre_v20_base`, `clamp`). They are unchanged, so the gate is met in substance. The sentence at `:127` is still wrong as written.

**Failure scenario:** a later reader, or the O17 re-pin checklist, takes `:127` at face value after a change to `fp/mod.rs`. Or they try to reproduce a bits-sweep binary that no longer exists. In either case there is no commit to rerun at.

**My re-runs at `46cf12a3` close the gap for this commit** (§(a) below):
- the full 2³² bits sweep;
- the full 2³² `main 20000` subsidy sweep, which covers both the CPU and GPU formula branches;
- four 2²⁸ slices: three on the frozen binary, which reproduced the implementer's slice logs exactly, and one more height at HEAD.

**Fix:**
- PROVENANCE names the commit whose `fp`/subsidy sources were swept (`0c52198c`), states that the swept functions are byte-identical at the merge commit, and records both binaries' sha256 and source manifests.
- At the merge candidate (after the CS0/re-pin rebase), rerun `exhaustive bits 8` (4–7 min at 8 threads).
- Rerun the subsidy sweep only if `git diff 0c52198c <merge> -- dash-consensus/src/fp dash-consensus/src/subsidy.rs` shows code changes.

### 3. minor: the L1 fuzz budgets have not run, and random KGW coverage in `diff` is thin (acceptable for merge, as a recorded carry-over)

**Where:** `contrib/node/cs2-diff/src/main.rs:386` (`let kgw = rng.chance(1, 50)`) and `cases.rs:190-258`. A KGW window comes out only when that 1-in-50 draw coincides with a last height in 15199..34138. The implementer's `diff 2000 424242` and `diff 2000 7` compared **3 and 2** KGW windows against about 21,445 non-KGW ones. Related: PLAN §4 "L1 … at merge, the 5-min smoke plus ≥ 24 CPU-h on new targets"; design §6.2's last paragraph; DEC-44's "phase C's KGW differential fuzzing must generate full 4,032-header ancestry".

**Assessment:**
- No `node-fuzz` crate and no O3 farm exist yet (STATUS: O3 is queued after CS0 and adopts CS0's runner). So `cs2-diff`, a workspace of its own like node-fuzz will be, is the right place today.
- The budgets cannot run in the planned form. Phase C's new surface is covered by stronger means:
  - exhaustive sweeps (`ConvertBitsToDouble`, the subsidy);
  - a proof plus fixtures (KGW decisions);
  - 16,076 deterministic boundary windows;
  - 18,940 real KGW heights.
- To test the thin spot directly I wrote an independent KGW-weighted random differential (`histcheck/src/bin/kgwrand.rs`). It builds full 4033-header windows, with next height in 15200..34139 and biased to both ends. The `nBits` are adversarial: any word, any size byte, negative, zero mantissa, powLimit, and the non-canonical 2²³² encodings plus 2²⁴⁰/2²⁴⁸. The timestamps are steady, bursty, reversed, or random anywhere in `u32`.
- **200,000 windows (seeds 11 and 12) against both pins: 0 mismatches**, with 32,251 and 32,330 distinct results.

**Fix (non-blocking):**
- Record the carry-over in BACKLOG/STATUS: "when O3 lands, port the `cs2-diff diff` legs to node-fuzz targets and run the ≥ 24 CPU-h budgets; `diff_next_work` weights KGW (draw the last height in `[kgw_height − 1, dgw_height − 2]` when `kgw` is chosen)".
- Meanwhile, `main.rs:386`'s KGW draw could pick its height inside the KGW range, so that a `diff` run exercises tens of KGW windows instead of 2–3.

### 4. minor: no PF0 workload guards `next_work`, and the "port 669 µs vs Core 1,199 µs" figure is an ad hoc wall-clock reading

**Where:**
- design §3.9 ("PF0 counters guard it after merge") and §6.5;
- `dash-consensus/benches/next_work.rs`;
- `contrib/node/perfgate/workloads.toml` on `node/pf0-counter-gate`. It declares only the hashes, sha256, tx, key-wallet and dash-spv workloads. PF0 measures only declared workloads, so a `next_work` regression would pass the gate silently.

**On fairness** (details in §(d)): the comparison is reasonable as an indication and not a merge criterion.
- Core is the O1 oracle archive, built with host GCC 15.2 and the oracle's flags, not the shipped Guix GCC 13.3 binary.
- Its per-call header decoding and index building are subtracted with a below-KGW baseline. That is fair.
- The port ran with `debug-assertions` and `overflow-checks` on, which is conservative.
- The workload is KGW's worst case, a full 4032-mass window. Real mainnet KGW breaks much earlier.
- It is one wall-clock run of n = 300 at load 30–50, which PF0's README calls not credible on agentbox (~30% spread). The program also wasn't committed (`/work/scratch/cs2c/corebench`).

My instruction counts (pinned, setup cancelled) give the same direction: Core 2.94 M vs port 1.35 M instructions per full window, **2.2×**. Part of that is by design: Core calls glibc `pow` 4,032 times, where the port does a table lookup.

**Fix:**
- At merge, the manager declares, for example:
  - `consensus-next-work-kgw = { bench = "dash-consensus/next_work", id = "next_work/kgw_4032_blocks" }`;
  - `…-dgw` (`next_work/dgw`);
  - `consensus-subsidy-formula` (`subsidy_formula`).
- The STATUS line "KGW 1.8× faster than Core" is reworded as "≈2× fewer instructions than the oracle build of Core's KGW (indicative)".

### 5. minor: `check-kgw-fp.sh` requires the CI runner's default ifunc to be glibc's FMA variant

**Where:** `contrib/node/check-kgw-fp.sh:38` (`run glibc-fma …`), against `tests/common/mod.rs`'s `expected_glibc_pow`, which already derives the expected variant from `/proc/cpuinfo`.

**Failure scenario:** a hosted runner without AVX2/FMA, or an FMA4-only AMD host. The test's host-libm check measures and prints `glibc-sse2` (or "the FMA4 variant"), the script expects `glibc-fma`, and the job fails although nothing is wrong. Today's `ubuntu-latest` runners have AVX2 and FMA, so this is latent.

**Fix:** take the default run's expectation from the same CPU-flag logic, and keep requiring that the masked run measures `glibc-sse2`. On an FMA4-only host, accept the FMA4 line for the default run. Alternatively, keep the hard requirement and say in the script that a non-FMA runner is a configuration error.

### 6. nit: the CS0 merge checklist in `provisional.rs` is stale, and the re-pin breaks one test import

**Where:** `dash-consensus/src/provisional.rs:8-30`; `dash-consensus/tests/common/mod.rs:16` (`dash_params::LlmqType`).

**What changed since the checklist was written:**
- The checklist describes five conflicts, as checked against CS0 at `61056e73` and `3e83e2ab`.
- Against CS0's current `97d4064f`, `git merge-tree --write-tree node/cs0-node-primitives 46cf12a3` conflicts in two files only:
  - `src/lib.rs`: two hunks, the module bullets and the module list. Resolve to the union, plus `#[allow(clippy::float_arithmetic)] pub mod fp;`.
  - `src/provisional.rs`: modify/delete; take the deletion.
- Everything else auto-merges:
  - `clippy.toml` merges to valid TOML: CS0's header, then phase C's 80 methods;
  - `FpPending` drops out of `lib.rs` cleanly;
  - CS0's `ctx.rs` items equal `provisional.rs`'s, modulo the `use` lines.
- STATUS orders the re-pin first. `node/bs-repin` moves base-sdk to `481f8c3`:
  - `dash-num` is unchanged in that range;
  - `dash-params` moves `LlmqType` to `dash_primitives`, so `tests/common/mod.rs:16` stops compiling. That is mechanical and caught at once.

**Fix:** at rebase time, follow the actual conflicts rather than the stale list, then rerun:
- `cs2-diff record --check`;
- `cs2-diff kgw`, default and SSE2;
- both history sweeps;
- `exhaustive bits 8`;
- `check-kgw-fp.sh`.

### 7. nit: design errata to fold in with STATUS's "§2.6 libm list erratum"

- §2.3's API gives `kgw_horizon_crossed(…, actual_secs: i64)`; the implementation takes `u32` (`fp/mod.rs:156`). That is an improvement: Core's clamp-to-zero becomes a `saturating_sub`, and no cast is left.
- §2.1's tolerance row and report §3.3 write `fabs(round(n1 − n2))`. That reads as C's `round()`. The instructions (`subsd; andpd`, `fabd`, `fsub.d; fabs.d`) and the port compute `fabs(fl(n1 − n2))`; write `fl` or "the rounded difference".
- §2.6 should list `rem_euclid`, `div_euclid`, the rounding methods and `abs_sub`, which `clippy.toml` already bans, and the `Rem::rem` entries from finding 1.

---

## (a) Independent re-verification

| Check | How | Result |
|---|---|---|
| Frozen subsidy binary | `sha256sum /work/scratch/cs2c/bin/cs2-diff` | `9f680c5e…` = `subsidy-binary.sha256` ✓ |
| Its sources vs the claimed commits | the recorded `subsidy-sources.sha256` against every commit in `d44631bb..46cf12a3` | code under test = `0c52198c`'s; the harness matches no commit (finding 2) |
| Bits-sweep binary | its recorded hash vs what is on disk | not frozen; gone (finding 2) |
| **Bits sweep, all 2³², at HEAD** | `cs2-diff exhaustive bits 8`, built `--locked` at `46cf12a3` (clean; sha `6a515f8e…`) | 4,294,967,296 words, `ConvertBitsToDouble` **equal on both pins**; all six bases in [0, 1111] (debug assertions on). Exit 0 |
| **Subsidy sweep, `main 20000`, all 2³², at HEAD** | 4 × 2³⁰ slices | equal on both pins (CPU formula for dDiff ≤ 75, GPU otherwise) |
| Subsidy slices on the frozen binary | `main 5000 [0x10000000,0x20000000)`, `main 4500 [0x10000000,0x20000000)`, `test 4500 [0xe0000000,0xf0000000)` | equal on both pins; identical to the implementer's slice logs |
| Subsidy slice at HEAD | `test 4500 [0x10000000,0x20000000)` | equal on both pins |
| `gen-kgw-pow.py --check --cr <evidence cr.txt>` | my own venv (gmpy2 2.3.2, MPFR 4.2.2) | `kgw_pow.rs` up to date; cr.txt equal ✓ |
| `KGW_POW`, independently of MPFR | mpmath 1.4.1 (pure Python) at 300 bits | all 4032 entries are the nearest double; the closest to a rounding tie is 6.3×10⁻⁵ ulp away (no double-rounding risk) |
| Fixtures | `tests/data/kgw_pow/*.txt` vs `designs/CS2-fp-evidence/out/pow-tables/` | all five byte-identical (PROVENANCE.md differs only by its copy note) |
| `check-kgw-fp.sh` | at HEAD, glibc 2.43 | default ifunc: `glibc-fma`; `-AVX2,-FMA`: `glibc-sse2`; exit 0 |
| `cs2-diff kgw`, default and SSE2 | the oracle's own KGW under each glibc variant | 16,076 windows equal on both pins under each, and 4,018 non-vacuous pairs |
| `cs2-diff record dash-consensus/tests/data --check` | at HEAD | 41 files, exit 0 |
| **History, mainnet 0..=100000, through the read-only oracle path** | Independent dump via `contrib/oracle-cli mainnet getblockheaders` (51 calls; sha `21081a24…`). Byte-identical to the first 100,001 headers of the implementer's pinned dump; links checked against `getblockhash` at 0, 15199/15200, 34139/34140, 68589/68590 and 99999. Then my own checker (`histcheck/src/main.rs`), which does not reuse `cs2-diff`'s sweep code: for every height, the port against Core's `GetNextWorkRequired` (5000-header windows, both pins), equality with the header's `nBits` above 68589, the tolerance against the literal predicate on Core's doubles at or below it, `nbits_acceptable`, PoW against Core's `CheckProofOfWork`, and block+treasury subsidy against Core | **0 failures**. 100,000 next-work answers equal on both pins, **18,940 of them KGW**. ≤ 68589: 34,122 equal and 34,467 different under the tolerance, the same split as the implementer's tally; 68,589 tolerance verdicts equal. 31,411 exact matches from 68590. 100,000 PoW and 100,000 subsidies equal |
| Random KGW differential | `histcheck/src/bin/kgwrand.rs` (finding 3) | 200,000 full windows, both pins, 0 mismatches |
| No FMA contraction where Core's builds fuse | `dash-consensus` rlib cross-built for aarch64 and riscv64gc, disassembled | 0 `fmadd`/`fmla`/`fmadd.d` anywhere. Separate `fmul`/`fadd`, plus `fabd` for the tolerance on aarch64 and `fsub.d`/`fabs.d` on riscv64. The port rounds the horizon in two steps everywhere, which is what test 1 assumes |
| No libm at link level | `nm -u` on the x86_64 release rlib | only `__udivti3` and `rust_eh_personality` |
| Gates (subset) at HEAD | `cargo fmt --all --check`; `cargo clippy -p dash-consensus --all-features --all-targets -D warnings`; `cargo test -p dash-consensus` (63 passed); `check-pure.sh`; `check-r2.sh`; `cs2-diff` clippy `--locked` | all exit 0. The implementer's full gate (`gate.log`, HEAD `46cf12a3`, clean; 2,889 tests) is consistent with this |

The KGW proof (`tests/quirk_40_kgw_fp_platform_invariance.rs`) is GPT's focus. I read it for structure only. The threshold search (`common::kgw_thresholds`) relies on the shape it states: `fl(t/a)` is monotone, above 1 below `a = t` and at most 1 from there (for t ≤ 604,800, `fl(t/(t−1)) > 1`), with `d > 1 > s`. The two-endpoint argument is sound for two reasons: both roundings of the horizon are monotone in `p`, and so is `s = fl(1/d)`. The fused rounding is exact integer code cross-checked against `mul_add`. The evaluation count is 4,019 × 4 = 16,076 ✓.

## (b) Acceptance: design §6.3 and §10

| Item | State | Merge? |
|---|---|---|
| §6.3: all 2³² `nBits` through `Difficulty::from_bits` against `oracle_convert_bits_to_double`, both pins, bit-exact | ✓ (re-run in full at HEAD) | met |
| §6.3: `block_subsidy_inner`, all 2³² × {formula 1, formula 2 (< 17000 and ≤ 75 paths), formula 3} × {main ≤ 4500, other}, `v20 = false`, both pins | ✓ six points: main 4500 / 5000 / 10000 / 20000 / 30000, test 4500. That is 25.8×10⁹ inputs. `main 20000` re-run in full at HEAD, plus 4 slices | met. Provenance wording: finding 2 |
| §6.3: bases in [0, 1111], the saturating cast never saturates | ✓ `truncate`'s `debug_assert`, with debug assertions on in the harness's release profile | met |
| §10 C: `fp` per §2.3 with `KGW_POW`, the generator, the fixtures, tests 1–4 of §2.4 | ✓ | met |
| §10 C: KGW per §3.5; subsidy bases; the literal tolerance predicate | ✓ line by line against `pow.cpp:15-79` and `validation.cpp:1784-1870`, `:4474-4489` at both pins | met |
| §10 C: quirks 9, 11, 15, 40; L0 `subsidy_tests` (12 records, all exact now); the confinement lint with rule 3 and its self-test | ✓ | met, with finding 1's gap |
| §2.4: CI runs test 4 twice, by default and with the tunable, asserting the variant | ✓ `check-kgw-fp.sh` and the `kgw-fp` job | met. Finding 5 is a robustness nit |
| §2.4 / §6.2: KGW boundary windows with both generator invariants, re-run under SSE2, asserting the variant | ✓ in `cs2-diff kgw`, and reproduced | met |
| §6.2 / DEC-44: KGW differential with full 4,032-header ancestry | ✓ the generator produces 4033 headers, but rarely draws KGW | met, carry-over in finding 3 |
| §10 D: L1 targets *to budget* (≥ 24 CPU-h); harness in `node-fuzz` | **deviation**: the harness is `cs2-diff` and the budgets have not run | **acceptable for merge.** `node-fuzz`/O3 does not exist yet, and phase C's own surface has exhaustive and proof coverage plus my 200k-window KGW run. Record the carry-over (finding 3) |
| §6.4: `getblockstats` subsidy leg and the masternode payout leg | **deviation**: they wait for the oracles to reach the tip (mainnet blocks at 1,393,163 of 2,552,027 headers today) | **acceptable**: PLAN §4 says acceptance evidence that only a later state can produce gates the milestone exit, not the task's merge, and design §6.4/§11 already make it an M1-exit item |
| §6.4: headers and subsidy history, both networks | ✓ (`reverify.log`, and my bounded run) | met |
| §3.9 / §6.5: the `next_work` bench, guarded by PF0 | the bench exists; no PF0 workload | finding 4; non-blocking, the manager declares it at merge |
| §10 D: self-review, `code-simplifier`, `code-review-validator`, report | `1eea1fb5` closes "the phase C review's minors"; the report is the implementer's summary | taken as met |

## (c) Code

- **Readability.** `fp/mod.rs` is 173 lines and reads like Core:
  - each function is one Core expression with `// Core:` anchors at both pins;
  - the doc comments state the domain facts that make each conversion exact (`truncate`'s [0, 1111], `kgw_past_seconds_min`'s positive timespan, `KGW_MAX_MASS` asserted equal to the table's length at compile time);
  - the tolerance keeps `!(x > y)` with an `#[expect(clippy::neg_cmp_op_on_partial_ord)]` that says why.

  `kgw.rs` mirrors `KimotoGravityWell` statement for statement, including the `PastBlocksMax > 0` guard, and the break that happens *before* `mass += 1`, so the last iteration's actual and target seconds survive.
- **Equivalences I checked.**
  - `actual_seconds = last.time.saturating_sub(header.time)` equals Core's `int64` difference of two `uint32` times clamped to ≥ 0 after the target is computed.
  - `next *= actual_seconds` (u32) is Core's `*= int64` resolving to `operator*=(uint32_t)`.
  - `div_u64`'s 32-bit-limb path equals dash-num's division; the test covers 20k random values × 13 divisors, including the values around `u32::MAX`.
- **libm and `%` bans.**
  - The method list in `clippy.toml` is complete for stable Rust's libm-backed `f32`/`f64` methods; I checked it against the inherent methods.
  - `abs`, `copysign`, `signum`, `max`/`min` (lowered inline) and `recip` are not libm, so leaving them unbanned is right.
  - `abs_sub` is banned. It is deprecated, but still callable under `#[allow(deprecated)]`.
  - The `Rem` route has the gap of finding 1.
- **Error handling and overflow.**
  - `fp` is total. The one `debug_assert` (`truncate`) holds for every `u32`, and the sweeps prove it.
  - KGW's `unreachable_mass` is unreachable: `PowParams::new` puts `7·timespan/spacing` in 1..=4032 wherever KGW is reachable, and KGW is dispatched only there.
  - The integer products are bounded by `PowParams`: spacing ≤ i64::MAX/72, timespan ≤ i64::MAX/7.
  - Nothing derived from input panics in release.
- **`no_std`.** Not a requirement: A1 X-1 and D65 drop it for pure crates, and CS0 §1 says the same. The crate is an ordinary `std` crate, and `check-pure.sh`/`check-r2.sh` pass.
- **Target guard.** The `compile_error!` covers the only x87 Rust targets, and the cross-builds show no contraction (§(a)).

## (d) Performance

The 669 µs vs 1,199 µs measurement is described in finding 4. My own instruction counts used `perf stat -e instructions:u`, pinned with `taskset`, on a plain-release build (`histcheck/src/bin/kgwbench.rs`). Each figure is the per-iteration median of (n = 6001) − (n = 2001), 3 × 3 runs. Startup in a binary that links the oracle is nondeterministic, about ±3×10⁸ instructions, most likely from Core's time-based RNG strengthening in its static initialisation; differencing two large n cancels it.

| Workload (4033 headers, all 4032 masses) | Instructions per call |
|---|---|
| Core, at KGW heights | 6,046,506 |
| Core, the same window below KGW (decode and index baseline) | 3,107,943 |
| **Core, KGW net of the baseline** | **2,938,563** |
| **Port** | **1,349,539** (Core/port 2.18) |

Both computed the same `nBits` (`1c12fd56`). As a claim this is fair, provided it is phrased as instruction counts against the oracle build. There is no PF0 workload for it yet (finding 4).

## (e) The CS0 rebase

Low risk. The details are in finding 6. To summarise:
- the trial merge against CS0 `97d4064f` has two trivial conflicts;
- `FpPending` (still in `node/integration`, 77 references) is removed cleanly, because CS0 never edited CS2's files after A+B;
- `clippy.toml` auto-merges into a superset;
- CS0's `check-pure.sh` makes the crate's `disallowed-methods` forbid-level for `--lib`, and phase C's `src/` has no `#[allow]`/`#[expect]` of that lint (only tests do, and the script lints `--lib`), so it holds there.

The only semantic exposure is the evidence: after the re-pin and CS0, the merge commit differs from every commit that was swept. That is handled by finding 2's rule: rerun the cheap legs and diff `fp/` and `subsidy.rs`.

## Summary

Phase C does what the approved design says, and I could reproduce every material claim independently at `46cf12a3`:
- **Exhaustive sweeps:** the full 2³² `ConvertBitsToDouble` sweep and a full 2³² subsidy sweep are bit-identical on both pins, and the frozen-binary slices reproduce the implementer's logs.
- **KGW:** the `KGW_POW` table is confirmed by a non-MPFR implementation. All 16,076 boundary windows match Core under both glibc ifuncs, and so do 200,000 adversarial random full windows.
- **History:** mainnet 0..100000 (18,940 KGW heights, the whole tolerance era) matches Core on both pins through a dump I made myself with read-only `oracle-cli`.
- **Floating-point code:** no FMA contraction on aarch64 or riscv64, and no libm at link level.

The deviations (harness in `cs2-diff`, fuzz budgets, the `getblockstats`/payout legs) are acceptable for merge, as recorded carry-overs. The work worth doing before or at merge:
- close the `Rem`/macro gap in the libm guard (finding 1);
- fix the sweep provenance and rerun the 4-minute bits sweep at the merge candidate (finding 2);
- declare the PF0 workloads (finding 4).
