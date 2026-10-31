# DW U-07 round 5 narrow review (Sol)

Scope: `3a4fd10a..20662ce6` on `dw/u-07-ui-harness`. The implementation worktree was read only. All mutant builds, repros, and browser runs used `/work/scratch/dw-u07-r5-sol/repro`; there were no GitHub writes or pushes.

## Review items

| Item | Status | Evidence and result |
|---|---|---|
| Round-4 Major: copied fixture test data through a raw asset | **CLOSED** | `apps/desktop/scripts/fixture-sentinels.mjs:59-64,98-117,185-201` reads both JSON test-data files, includes their string values/keys and integer values, and requires each source to contribute a sentinel. `check-bundle.mjs:38-66,87-120` scans every output file and decodes base64 data URLs, percent escapes, and digit separators. The exact repro copied `src/fixture/vectors/accept.json` to `src/guard-copy/accept.json`, imported it from unmarked `data.ts` both as a bound and bare `?raw` import, and dynamically imported `data.ts` from `main.tsx`. `pnpm build` exited 1 at `check-bundle` with 32 vector sentinels in `assets/data-*.js`, including `lease-0123456789abcdef0123456789abcdef`. |
| Sentinel count and source ownership | **CLOSED** | Direct derivation reported **378 unique sentinels**: 74 owned by fixture modules and 304 by test data. The module source totals are 76 because two literals occur in more than one module; ownership in the sentinel map remains 74. `pnpm build` exited 0 with `none of 378 fixture sentinels`; `pnpm build:fixture` exited 0 with all 74 module sentinels. No test-data sentinel appeared in `dist-fixture/`. The per-source checks are pinned by `fixture.test.ts:46-72` and `sentinels.test.ts:65-75`. |
| Nine negative controls | **CLOSED** | `scripts/guard-negative-control.sh:101-108` runs wallet, mts, errors, `?raw`, `?url`, `.txt?inline`, `?url&inline`, plain JSON, and percent-encoded `.svg?inline`. The full script exited 0. Every case failed the production build after Vite reported `built`, at `check-bundle`, and printed the expected literal/source location. |
| `holds()` and output decoding | **CLOSED** | `check-bundle.mjs:46-66` reads all assets and decoded `data:` payloads; `fixture-sentinels.mjs:204-213` checks template-literal escapes and the single-quote form of double-quoted literals. The focused tests at `sentinels.test.ts:90-99` pass. The SVG control was found in the `escapes and separators read` view. |
| Generated-binding shared set | **CLOSED** | `fixture-sentinels.mjs:120-155,168-173` includes generated literal types and identifier field names, while `sentinels.test.ts:28-38` pins the codegen output directory and verifies that fixture sentinels do not overlap generated values. Comparing old and new derivation in the scratch clone found 47 current fixture overlaps (generated error codes plus four transaction types); only **four** newly excluded distinctive fixture literals were introduced by the widening: `masternodeRegistration`, `coinJoinCreateDenominations`, `coinJoinMakeCollaterals`, and `coinJoinCollateralPayment`. No module sentinel was newly excluded. All four are real production strings in `src/format/tx.ts:31-37,55-57` and were present in the production bundle. A copied fixture module/data containing an ordinary non-generated sentinel still failed the guard. |
| Vite import/asset paths | **CLOSED** | I tested unmarked copies through `new URL(..., import.meta.url)`, `import.meta.glob` with eager `?raw`, CSS `url()`, and a used `?worker`; each emitted content was caught by `check-bundle`. An unused worker was tree-shaken to no emitted content, which is the documented per-literal/per-tree-shaking limit (`docs/ci.md:149-151`). Sourcemaps are disabled in `vite.config.ts:55-61`. |
| Main merge and M4 contract | **CLOSED** | `6f426a79` is a conflict-free merge; the only overlapping file is `apps/desktop/src/api/events.ts` (the merge diff shows the intended invalidation additions). `docs/contracts/m4-dashpay-engine.md` remains Contract-Version 17. `cargo test --manifest-path rust/Cargo.toml -p dw-engine --test m4_dashpay_contract` passed **9/9**, including `surface_listing_matches_the_contract` (`rust/crates/dw-engine/tests/m4_dashpay_contract.rs:488-520`). `git diff --quiet 6f426a79..20662ce6 -- rust` exited 0, so no Rust changed after the gated merge. |
| Test claims | **CLOSED** | Vitest: **250/250** (`pnpm test`). Pinned `mcr.microsoft.com/playwright:v1.64.0-noble`: **156 passed / 50 skipped**. Visual run in the same image (`DW_VISUAL=1`, Chromium): **8/8 passed**. |
| Documentation | **CLOSED with nit** | `docs/ci.md:121-151` documents test-data sentinels, all output/assets/data-URL views, all nine controls, and the per-literal limit. It does not explicitly mention deliberately compressed or UTF-16 encoded copies; this is only a documentation nit under the accepted limits, not a guard defect. |

## Reproduction details

- Scratch clone: `/work/scratch/dw-u07-r5-sol/repro`, detached at `20662ce6`; source worktree remained clean.
- Raw-copy repro: `src/guard-copy/accept.json`, unmarked `src/guard-copy/data.ts` with both `import raw from "./accept.json?raw"` and a bare `import "./accept.json?raw"`, plus `void import("./guard-copy/data")` in `main.tsx`. Production build: **exit 1**, `check-bundle` identified 32 sentinels from `src/fixture/vectors/accept.json` in the emitted data chunk.
- Negative controls: **9/9** failed at the sentinel check and printed the expected source literal.
- Baselines: production **exit 0**, fixture **exit 0**, with the reported 378/74 counts.
- M4 contract: **9 passed, 0 failed**.
- `git diff --check 3a4fd10a..20662ce6`: clean; source worktree `git status --short`: clean.

possible security note: see manager

APPROVE-WITH-NITS
