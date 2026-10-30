# DW U-07 round 4 narrow review (Sol)

Scope: `4f29070c..3a4fd10a` (`2673f944`, `6b41c611`, `3a4fd10a`) on `dw/u-07-ui-harness`. The implementer worktree was read only. All mutants, builds, and browser runs used `/work/scratch/dw-u07-r4-sol/repro`; no GitHub writes or pushes were made. The round-3 report was `/work/scratch/dw-u07-r3-sol/DW-U-07-r3-sol.md`.

## Findings

| Item | Status | Evidence and result |
|---|---|---|
| Round-3 Major 1: `.mts` and per-module sentinel contribution | **CLOSED** | `apps/desktop/scripts/fixture-sentinels.mjs:39-49,77-85,138-149` reads `.ts .tsx .mts .cts .js .jsx .mjs .cjs`, rejects other non-test-data files, and throws when a module has no own sentinel. `apps/desktop/src/fixture/fixture.test.ts:40-70` pins every fixture file and checks every module marker and contribution. The marked `.mts` plus unmarked dynamically imported copy failed `pnpm build` at `check-bundle` on its one sentinel. |
| Round-3 Major 2: relocated copy hidden by editing SHARED/GENERIC | **CLOSED** | There is no hand-edited `SHARED`; `GENERATED = "src/api/generated"` is mechanically read at `fixture-sentinels.mjs:51-52,129-133`, and `sentinels.test.ts:28-38` pins the codegen path and excludes exactly generated overlaps. `GENERIC` is empty; entries require a reason and shape at `sentinels.test.ts:41-55`, and stale entries fail `check-bundle.mjs:78-83`. An unmarked `errors.ts` copy without exemptions failed on six fixture sentinels. Adding all six as GENERIC made the build fail earlier because `errors.ts` then had no own sentinel (`fixture-sentinels.mjs:145-149`) and made the tests fail (7 tests). A single invalid message GENERIC entry failed the shape and exact-sentinel tests and the build still failed on the other literals. |
| Round-3 Minor 3: parent symlink wording | **CLOSED** | README `apps/desktop/README.md:75-76`, D16 `docs/design/E0-13-bindings.md:25`, and `rust/crates/dw-fs/src/lib.rs:128-136` say only the final component is refused; parent directory symlinks are followed. The test at `rust/crates/dw-fs/src/lib.rs:915-920` pins that behavior. |
| New: copied fixture test data can bypass the production guard through a raw asset | **OPEN — major** | `fixture-sentinels.mjs:54-61` explicitly excludes the two JSON vectors as `TEST_DATA`; only module literals become sentinels. In a scratch clone I copied `src/fixture/vectors/accept.json` to `src/guard-copy/accept.json`, loaded it from an unmarked `src/guard-copy/data.ts` with `import "./accept.json?raw"`, and dynamically imported `data.ts` from `main.tsx`. `vite` did not see a `src/fixture/` module or marker, and the output had a 33.69 kB `data-*.js` chunk containing the vector. `pnpm build` exited 0 and printed `check-bundle: dist: 89 files, neither fixture marker and none of 74 fixture sentinels`. This does not use the accepted per-function tree-shaking limit. If vectors are in the production fixture-data exclusion boundary, the guard needs to reject copied test-data assets or derive/check sentinels from them too. |

## Specific checks

### Trimmed plain-word rule

`fixture-sentinels.mjs:122-127` trims surrounding spaces and punctuation before deciding that a word is generic. Comparing old and new predicates over every fixture literal found only the intended short fragments (`"fixture: "`, `" failed)"`, `" characters"`, and query-label fragments such as `", categories: "`), which are repeated prose or field labels. The distinctive wallet labels, XSS payload, seed, long IDs, error payloads, and numeric values remain in the 74-sentinel set. `sentinels.test.ts:65-73` pins representative retained and dropped values. No important current sentinel was silently lost by the trim rule.

### GENERIC stale-entry refusal

A stale valid entry (`#4294967296`, with a valid power-of-two reason) made `pnpm build` exit 1 with `holds none of these GENERIC entries`. Hiding the complete `errors.ts` set instead reached the per-module no-sentinel refusal, and an invalid message entry failed the generic-shape test. The current stale check cannot be bypassed by an entry that is absent from production output without also changing the tests/configuration.

### Generated-directory and dynamic-import checks

`rust/crates/dw-app/src/codegen.rs:18-21,187-206` compares every committed generated file with a fresh codegen directory, while `sentinels.test.ts:29-38` pins the same path. No generated-directory relocation bypass was found. The wallet, `.mts`, and errors negative controls all use dynamic imports and failed production builds at the sentinel check. The raw copied-vector case above is the non-function dynamic/raw-asset bypass found.

### Documentation limit

The check-bundle header (`apps/desktop/scripts/check-bundle.mjs:14-17`) and CI docs (`docs/ci.md:140-142`) accurately state a per-literal, not per-function, limit and give the named-function/tree-shaking example. They do not claim function-level coverage. The new raw test-data gap is outside that documented limit.

### Rust range

`git diff --quiet 6b41c611..3a4fd10a -- rust` succeeded: no Rust changed after the gated `6b41c611` commit. `git diff --check 4f29070c..3a4fd10a` was clean.

## Reproductions and gate results

- Baseline `pnpm test`: **249/249 passed**.
- Baseline `pnpm build`: **EXIT 0**, 74 fixture sentinels absent from `dist/`.
- Baseline `pnpm build:fixture`: **EXIT 0**, all 74 sentinels present.
- `scripts/guard-negative-control.sh`: wallet, marked `.mts` plus unmarked copy, and errors copy all failed production builds for the expected sentinel source.
- Dropping `scenarios.ts` from `fixtureModules`: focused Vitest run **17 passed / 1 failed** (`scenarios.ts: expected 0 to be greater than 0`).
- Errors copy without list edits: production build failed at `check-bundle` on its six sentinels. With an invalid GENERIC edit, Vitest failed its exact-sentinel and shape checks; with all six exemptions, the build failed the no-sentinel-per-module check.
- Pinned `mcr.microsoft.com/playwright:v1.64.0-noble`: full run reached **154 passed / 2 WebKit axe timeouts / 50 skipped**; rerunning the two timed-out tests alone gave **2/2 passed**, yielding the claimed **156 passed / 50 skipped**. `DW_VISUAL=1 --project=chromium e2e/visual.spec.ts`: **8/8 passed**.
- `ci/github/selftest-judge-test.sh`: **EXIT 0**, all cases pass.

No target directories were created in the scratch clone.

CHANGES-REQUESTED
