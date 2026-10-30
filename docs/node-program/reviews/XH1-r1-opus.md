Verdict: **APPROVE-WITH-NITS**. No blockers or majors. Findings 1–5 are minor and 6–13 are nits. Findings 1 and 4 should land before CS0/BS15 adopt the crate. Findings 2 and 3 can be follow-ups.

# XH1 r1 — Opus review (`node/xh1-x11-backend` at 174e70cd)

**Emphasis:** code quality against repo patterns, maintainability of the vector code, the design choices (including the fallback question), an independent re-run of the evidence, and the integration plan. The GPT reviewer covers bit-exactness edge inputs, CPU dispatch and `unsafe`, so I only touched those where they bear on design.

**Setup:**
- Fresh detached worktree at 174e70cd (`/work/scratch/xh1-opus/wt`), parent ed085c2b.
- 3 commits, all carrying SSH signatures and using conventional messages.
- Scratch is in `/work/scratch/xh1-opus/`.

## Evidence I re-ran (all pass)

| Check | Result |
|---|---|
| Harness really links Core's objects | `build.sh … /work/bench/src/dash-<tag>` at both pins. The COMMIT files are 728f5055 and 0364ed62, matching the tags. The archives hold Core's own members: `libbitcoin_crypto_sph` (13 objects, including `dispatch`, `echo`, `shavite`, `aes`), `ssse3` (`echo`) and `x86_aesni` (`echo`, `shavite`). The B1-mode binaries' `.comment` carries `GCC: (Debian 12.2.0-14+deb12u1)` from those objects, beside the host's 15.2. `harness name` reports `echo_round=simd echo_shift_mix=simd shavite=simd` at both pins. |
| 1M differential, both pins (B1 objects, `JOBS=8`) | `differential.sh 1 1000000`: 8/8 runs pass. Per pin: 1,000,000 `random` (len 0..=200), 1,000,000 `header`, 100,000 `long` and 100,000 per stage × 11, each through all 5 selections this CPU has. |
| Extra: Core's soft paths, a different compiler | `differential.sh 1 300000` with `env DASH_X11_CORE_SOFT=1` against the **from-source** harnesses (GCC 15.2) at both pins: 8/8 pass. |
| Core vectors | `tests/vectors.rs` passes. Independently of `extract-vectors.py`: all 61 `known-hashes.json` hashes appear in v24's `chainparams.cpp` (3 genesis asserts and 58 checkpoints), and all 61 headers hash to them under all four harnesses (B1 and source, both pins). |
| Full suite | Release with `DASH_X11_CORE_HARNESS` set to both B1 harnesses: 18 tests and the doctest pass. Debug profile (what CI runs) passes in 3 m 55 s. aarch64 under QEMU (portable path) passes. |
| Whole chains | `examples/rehash chain` over CS2's dumps, under all 5 selections: all 2,551,767 mainnet and 1,568,460 testnet headers link to their successors and to the best header. |
| CS0 switch is output-preserving | Scratch crate `powcmp/`, dash-pow@33fcc59 (CS0's current X11) vs `dash_x11::x11`: equal on every mainnet header, plus 200K random inputs of 0..=1100 bytes (also vs `standard`). The byte order is the same: both return the first 32 bytes of ECHO. |
| Instruction counts | `ins-per-call.sh`: `auto` X11 39,048 (README 39,055); `standard` 149,984 (equal). Core harness `bench x11` against B1's objects: 169,617 at both pins (equal), so the stage rows reproduce. Core built **from source with GCC 15.2** gives 160,808, so portable is 0.93× and all-backends 0.24× there. As the SH1 review found, Core's side depends on how Core is built, and the README's B1 basis is the right one for PRD parity. |
| SIMD-512 NTT `i16` bounds | Mechanically re-derived (`simd_bounds_lanes.py`, per-lane interval arithmetic, Montgomery `m` treated as any `i16`). This reproduces **every** bound in `simd.rs` exactly: u −17340..=21675; v[0] −84..=323; v[1..15] −168..=169; v[15] −237..=246; first DFT4 −6551..=6514; second DFT4 ≤ 22,223 in magnitude. So the maximum is 22,223 < 32,767, and no input can overflow. |
| Builds and rules | Builds for `x86_64-unknown-none`, `thumbv7em-none-eabihf`, `aarch64`, `i686`, `wasm32-unknown-unknown`, and MSRV 1.89. `check-pure.sh`, `check-r2.sh`, `cargo deny check licenses bans`, `fmt --check` and `clippy --all-targets -D warnings` are all clean. |

## (c) The fallback decision: keep the fallback, but make it observable and eager

Recommendation: **`auto()` should keep falling back, not panic.** Before CS0 adopts the crate, add the reporting described in finding 1. Reasoning:

- **The policy matters only in one case: an instruction that runs but computes wrongly on the genesis input.** Typical causes are a mis-emulating hypervisor or TCG, or a build miscompile that genesis happens to hit. The other cases come out the same either way:
  - **OS disables YMM:** a hypervisor reports AVX2 but the OS hasn't enabled YMM. That's handled before the self-test (`x86_64/mod.rs:28-35` checks OSXSAVE and XCR0).
  - **Feature reported but `#UD`s:** the self-test dies of SIGILL under either policy, as Core would.
  - **Fast path wrong on other inputs but right on genesis** (miscompiled or data-dependent, e.g. an `i16` overflow): neither policy notices, because a single-input startup test can't. The defence is the differential, the chain rehash, and running the suite under the **shipping** profile; see finding 2.
- **In the one case where policy matters, the fallback is the more correct choice.**
  - The portable code uses only scalar instructions, which are far less likely to be mis-emulated.
  - It is checked by the same self-test before use.
  - The node then keeps validating correctly at about 4× the X11 cost. X11 is a small share of the node's CPU after IBD.
  - A masternode stays up instead of being PoSe-banned while its operator debugs a VPS.
- **Neither option makes us less conservative than Core.** Core has **no** X11 self-test (`dispatch.cpp`); it runs whatever CPUID says. Any X11 policy is therefore a deviation from Core, and fallback-after-check is strictly more conservative.
  - SH1's `auto()` panics, but there it reproduces Core's `assert(SelfTest())`, so the asymmetry is justified.
  - On a machine with a broken AVX2 unit, dash-sha256's self-test aborts anyway. The X11 fallback really only matters for a broken AES-NI or a broken X11-only vector path.
  - Record the asymmetry in DECISIONS so nobody "fixes" one crate to match the other.
- **What must change is visibility.** Today a degraded `auto()` is silent:
  - The only public way to learn what the CPU *should* run is `detect(Use::ALL)`, which panics in exactly that situation.
  - The self-test also runs lazily, on whichever thread first hashes. In the node that's the P2P/validation path mid-sync, not startup.
  - Core runs `SapphireAutoDetect()` eagerly in `kernel::Context`'s constructor (`v24 src/kernel/context.cpp:22`), next to the SHA-256 line it logs.
- **How the node should report it:**
  - At kernel/consensus init, call `dash_x11::Implementation::auto()` eagerly. Log `Using the '<name>' X11 implementation`, as Core logs SHA-256's (`context.cpp:24`).
  - If the CPU's preferred selection failed its self-test, log a **WARN** naming the failed and the chosen selection.
  - Expose the active name somewhere queryable: a debug/metrics field, and the PF0/node-bench provenance. Otherwise a fallback on a bench host shows up as an unexplained 4× X11 regression.
  - Offer an operator override (`-x11impl=standard|auto`, hidden). `Implementation::detect(Use)` already supports it.

## Findings

1. **minor: a fallback can't be observed; the self-test is lazy** (`dash-x11/src/dispatch.rs:218-241`, `:212-216`, `lib.rs:42-47`)
   - *Scenario:* on a VPS that mis-emulates AES-NI, `auto()` silently picks `sse,avx2`. The node can't tell; calling `detect(Use::ALL)` to find out panics. If even the portable code fails, the panic fires on the first header hashed, deep in sync, instead of at startup.
   - *Fix:* return what happened. For example, `pub fn auto_report() -> (&'static Implementation, Option<&'static Implementation>)` (the chosen and the failed preferred selection). Or make `select` public as `preferred(Use)` (no self-test) and `self_test` public. Document that callers should initialise at startup, and wire CS0/kernel init as in (c).
   - The doc claim "A broken vector unit thus costs speed, not availability" (`dispatch.rs:220`) should say "a vector path that fails the genesis check".

2. **minor: the startup self-test is one input, and the suite never runs under the shipping profile** (`dispatch.rs:189-201`, `:243-246`)
   - *Scenario:* `rdashd`'s release profile adds `lto="fat"`/`codegen-units=1` (or a packager sets `-C target-cpu`), and LLVM miscompiles one vector stage for some inputs. Genesis still passes. The crate's tests ran only under the workspace `release`/`dev` profiles.
   - *Fix:*
     - Run `cargo test -p dash-x11` plus one `rehash chain` under the profile and flags `rdashd` ships with, at least per release (gate item).
     - Optionally widen the startup battery cheaply (about 2 ms): testnet genesis, plus each stage on all-0x00 and all-0xFF 64-byte inputs with Core's digests embedded. Those are the carry and saturation extremes; SH1's Core self-test also uses several shapes.

3. **minor: the SIMD-512 `i16` bounds are correct but only in comments** (`x86_64/simd.rs:19`, `:251`, `:262`, `:282`, `:302`, `:324`)
   - I re-derived every bound exactly (table above), so there is no defect today; the margin is 22,223 vs 32,767. But the proof isn't in the repo.
   - *Scenario:* a maintainer swaps a `mulmod` for a shift, or reorders step 4, and creates an overflow reachable only by extreme lane values. Random and `{0,255}` tests rarely hit those extremes; a pattern hit by about 1 in 2^40 headers is invisible to tests, yet mainnet hashing reaches it.
   - *Fix:* commit the interval check as a unit test (about 60 lines). It would recompute the per-lane ranges from `TWIDDLE`, `TWIDDLE4` and `YOFF_N` and assert every intermediate fits in `i16`. `/work/scratch/xh1-opus/simd_bounds_lanes.py` is a ready model to port.

4. **minor: the PF0 workload lines aren't aligned yet** (PF0 `contrib/node/perfgate/workloads.toml` on `node/pf0-counter-gate`; P2a `designs/P2a-outbound.md:1436`; `dash-x11/benches/x11.rs:15-22`)
   - **There are no X11 rows yet.** DEC-51c and P2a §5.4 ask for one, named `x11_80b`, but workloads.toml uses kebab-case names (`sha256-standard`, `dsha256-1m`).
   - **`x11/0080b` measures `auto()`, so its meaning depends on the host.** The dash-sha256 rows instead pin their backend (`sha256/SHA256_AVX2`). On a host without AES-NI, or after a silent fallback (finding 1), the counter changes about 4× for reasons outside the code.
   - **The Core counterpart can't be expressed in PF0 as written.** Neither kind PF0's workloads.toml offers (`bench`, `node`) can run Core's harness or bench_dash. So "the X11 ratio as its own counter" needs B1's `Pow_X11_0080b` (169,617 at both pins) recorded as the fixed denominator.
   - **The two measuring methods differ.** `ins-per-call.sh`'s (N−0)/N and PF0's calibrated counts won't give identical numbers; say which one the "0.23×" in STATUS and PRD refers to.
   - *Fix:*
     - Add a pinned bench ID such as `x11/0080b/all` (`detect(Use::ALL)`) beside the existing `x11/0080b/standard`.
     - Add `x11-80b-all = { bench = "dash-x11/x11", id = "x11/0080b/all" }` and `x11-80b-standard = { bench = "dash-x11/x11", id = "x11/0080b/standard" }` to workloads.toml, and use the same names in P2a §5.4.
   - The criterion-0.5 shim covers every API the bench uses. `--list` shows the IDs.

5. **minor: the BS15 and XH1 plan rows are stale** (`node-program/PLAN.md:468`; PLAN has no XH1 row)
   - **BS15 still plans the dropped approach.** It plans "`dash-pow` AES-NI dispatch" and "B1 microbench decides the default". XH1 measured `aes_hw` as worthless (dash-pow is 2.37× Core with or without it), so BS15 should adopt dash-x11. That means `dashcore_hashes`' `x11` feature switches from `rs-x11-hash` to `dash-x11` (`hashes/Cargo.toml:19,29`).
   - **dash-x11 must become publishable.** It becomes a dependency of the published `dashcore_hashes`, which needs a versioned dependency and the crates.io name. It must also keep `no_std`, `wasm32` and iOS builds; `no_std` and `wasm32` are verified above.
   - **CS0 can drop dash-pow.** CS0's only `dash_pow` use is `hash.rs:60`, so the switch also removes dash-pow from `dash-consensus`.
   - **PLAN is missing XH1.** DEC-51b created XH1, but only STATUS lists it. Add the PLAN row, with its consumers P2a-core (headers), BS15 and CS0.

6. **nit: the Luffa AVX2 frame schedule is a lot of machinery for 0.5%, and its search isn't committed** (`x86_64/luffa.rs:33-55`, `:236-241`)
   - I measured it: with `FR_X8`/`CH_X8` zeroed, X11 costs 39,252 instead of 39,055 (+204, +0.5%; Luffa 4,520 vs 4,316). The zero schedule still passes 200K backend cases, so correctness doesn't depend on the schedule.
   - That is a good property, but it isn't stated. The constants say "found by exhaustive search", and the search isn't in the repo.
   - *Fix:* state in the doc that any schedule ending in frame 0 is correct, and give its measured payoff. Either commit the search or simplify to zero frames.
   - The `assert!`s in `L` (`raw`, `and`/`or`/`xor`) rely on const-folding ("A constant: no code"). That's fine, but only a PF0 X11 row (finding 4) would notice if folding stopped.

7. **nit: codegen steering in JH** (`x86_64/jh.rs:56-69` `twice`, `:232-249` and `:450` `black_box`)
   - Duplicated constant tables and `black_box` are used to coax specific LLVM output. They're fragile across rustc upgrades, though harmless to correctness.
   - Give the measured gain in the comment, so a future maintainer can judge whether to keep them when rustc changes.

8. **nit: two idioms in one backend directory** (`x86_64/echo.rs:10-20`)
   - `echo.rs` uses `#[inline(always)]` helpers with `unsafe {}` ("SSE2 is part of x86_64"). The rest use safe `#[target_feature]` fns.
   - Say why in the module doc: rustc 1.98 rejects `inline(always)` together with `target_feature`.

9. **nit: no per-file notices, unlike SH1**
   - SH1 keeps Core's per-file `// Copyright …` headers. dash-x11's files have none; the crate `LICENSE` carries both MIT texts, which is legally enough.
   - For consistency, add the Dash Core (2025) header to the ports of `dispatch.cpp`, `echo.cpp`, `shavite.cpp` and `x86_aesni/*`, and the SAPHIR notice to the sph ports. Or record that the crate LICENSE is the convention.

10. **nit: API edges** (`dispatch.rs:279-287`, `:17-28`)
    - `stage(&self, Stage, &[u8])` is public and panics on non-64-byte input after BLAKE. It's a test and diagnostic API: consider `#[doc(hidden)]`, or a typed `&[u8; 64]` variant.
    - `detect(Use::AES)` without `SSE` silently returns `standard`. This is documented, but `Use::AES`/`AVX2` could simply imply `SSE`.
    - `Use`'s `Debug` prints `Use(3)`.

11. **nit: test tooling**
    - `contrib/differential.sh:50` mangles multi-word harness commands in the summary (`simd env", "DASH_X11_CORE_SOFT=1", "/…`).
    - `tests/core.rs:88-94` formats both digests to hex for every case. Compare bytes first and format only on mismatch.
    - The debug suite takes about 4 minutes (props 123 s), which is acceptable.

12. **nit: README** (`README.md:77`)
    - `H=/work/scratch/xh1/bin` is an agentbox path; mark it as an example.
    - Mention that Core's ratio depends on Core's compiler (GCC 15.2 from source gives 160,808, so portable is 0.93×).

13. **nit: no aarch64 backends** (`README.md:31-34`)
    - Core accelerates ECHO and SHAvite with ARMv8 AES and NEON; on ARM, dash-x11 runs portable. Header-sync parity on an ARM tier-1 platform is therefore unmeasured.
    - Add a BACKLOG row: measure on the Mac Studio, and port `arm_crypto` ECHO/SHAvite if needed.

## (a) and (b) in brief

- **Patterns.** The crate mirrors SH1's shape: the `Use`/`Implementation`/`detect`/`auto`/`standard`/`name` API, `tests/common`, the Core harness under `contrib/core-harness`, provenance-tracked fixtures, criterion plus an `ins-per-*` script. Differences from SH1:
  - It is `no_std`, with a CPUID detector cross-checked against std's (`cpu_features_match_std`) and an `AtomicPtr` cache instead of `OnceLock`. That's stricter than node-pure needs, and right for BS15.
  - There are no feature flags, and the panic policy is limited to the documented `detect`, `auto` and `stage` cases.
  - `#![deny(unsafe_op_in_unsafe_fn)]` is on, with SAFETY comments throughout.
  - `auto_and_free_function_agree` asserts that `auto()` keeps every backend on a healthy CPU, so the fallback can't hide a broken backend in CI.
  - Joining `node-pure` in ci-groups is correct, and check-pure, R2 and deny pass.
- **Maintainability.**
  - Every portable port names the sph macro it implements (`SLu`, `Sb`, `Lb`, `INPUT_BUF1/2`, `SUB_CRUMB`, `MIX_WORD`, `MI5`, `P5`, `TWEAK5`, `STEP2_BIG`, `ONE_ROUND_BIG`, `WBREAD`, `REDS1`) and states its specialisation (one 64-byte message, which padding block).
  - Every vector module opens with the state layout and the algebra it relies on: JH's complement tracking, whose seven-round cycle is found and checked by const evaluation; Luffa's bitslicing, complement flags and frames; SIMD-512 as a 16×16 NTT with Montgomery mod 257 using 2^16 ≡ 1; Grøstl's round constants folded through `aesenclast` keys.
  - A maintainer can verify each against sphlib with the portable twin and the per-backend unit test. The SIMD bounds are the one part whose proof isn't in the repo (finding 3).
  - "Needlessly clever" applies only to the micro-steering in findings 6 and 7. The large reformulations (SIMD NTT, Grøstl via AES-NI) are where the 4× comes from.

## Summary

XH1 does what DEC-51b asked, and the evidence holds up when re-run independently. The harness links Core's own B1 objects at each pin. A 1M-case differential per pin, a soft-path and different-compiler differential, every mainnet and testnet header, Core's vectors (cross-checked against chainparams), and the instruction counts all reproduce. dash-pow and dash-x11 agree on every mainnet header, so CS0's switch is a pure performance change. The SIMD-512 `i16` bounds are exactly right.

On the open question, keep `auto()`'s fallback, but surface it: a status API, eager init with a log line like Core's SHA-256 line, and a WARN on degradation. Also record the asymmetry with SH1's panic, which is Core's own assert, in DECISIONS. The remaining items are plan hygiene (PF0 rows pinned per backend, BS15 rewritten around dash-x11, an XH1 PLAN row), committing the NTT bound check, and small documentation nits.

Scratch artefacts in `/work/scratch/xh1-opus/`:
- `bin/` (four harnesses);
- `diff-b1/` and `diff-soft/` logs;
- `simd_bounds_lanes.py` and `simd_bounds.py`;
- `powcmp/` (the dash-pow vs dash-x11 check);
- `rehash-*.out`.
