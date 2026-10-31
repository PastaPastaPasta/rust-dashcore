Verdict: **CHANGES REQUIRED**. One must-fix before merge (E1, documentation only). M1 and L1 can land with it or before EV3 starts; the nits are optional.

# O2a-MNHF r1: Opus-h review (rdc `node/o2a-mnhf` 73a5ba99e, Core `node/o2a-mnhf-oracle-v24.0` 54381c5888)

**Scope:** `oracle_mnhf_step` (ABI 23), its `dash-oracle-sys` binding, the crafted and Core-mined tests, the dash-dml L0 vectors and reference model, the ABI 23 bookkeeping, and public-text hygiene. I'm a designated reader for EMBARGO-6 under PLAN §7. Anything about it is in a private note; see EMBARGO-6 note.

**Setup:**
- Detached worktree of 73a5ba99e in `/work/scratch/o2a-mnhf-r1-opus` (mode 0700). Separate `CARGO_TARGET_DIR`s for each flavour, seeded by reflink from the implementer's target.
- Core was read at 54381c5888, with the reference at 0364ed626e (v24.0.0-rc.3). `evo/mnhftx.cpp` and `chainparams.cpp` are identical between them. The base 5c9e9117a2 changes `evo/specialtxman.cpp` only by oracle/StateDigest hooks (the `TapEhf` tap after `ProcessBlock()`).

## Findings

| ID | Sev | Where | Finding | Schedule |
|---|---|---|---|---|
| E1 | must-fix | `docs/node-program/designs/O2a-MNHF-notes.md:22` (D10), `:25` (D13); `dash-oracle-sys/tests/data/PROVENANCE.md:462`; `dash-oracle-sys/scripts/gen-mnhf-regtest.py:258-260`, `:303`; `dash-dml/tests/data/mnhf/README.md:42-44` | Stub placement breaks PLAN §7.1 ("never beside the specific field"). Remove every "possible security note: see manager". Strip the reason from `spawn()`'s docstring and PROVENANCE's throw-away-node sentence, so they only say that bases below the side branch are judged on a second node started from a fresh copy of the datadir. If a pointer is wanted, add one section-level stub to the notes' header banner, worded as §7.1 requires. The branches are unpushed (§7.6), so amend or fix up 5840e5930 and 7585e9844 before the train merge. That keeps the strings out of `node/integration` history. The neutral mechanism text in `oracle.h:1840-1844` and in the commit messages of 54381c5888 and 7585e9844 can stay. | before merge |
| M1 | medium | Notes D13 (`O2a-MNHF-notes.md:25`, "the oracle cannot seed the cache"); `oracle.cpp:1582-1615` (`SeedSignalRows`), `:4786-4796` | D13's "cannot" is inaccurate. `CMNHFManager::AddSignal()` is public (`evo/mnhftx.h`, one bit per block; `mnhftx.cpp:385-390`). `AddToCache()` (`mnhftx.cpp:364-383`) is reachable with the shim's existing `PrivateAccess` pattern. As built, the step on a tip at `V20Height - 1` can only answer for a manager without that block's cache entry. The connecting node's verdict and row for the `V20Height` block are therefore unobtainable from the oracle; only its stage is (as the previous step's `stage_after`). A change at `V20Height - 1` that the caller passes is dropped silently (`tests/mnhf.rs:313`, "the first block with V20"). Either reword D13 to "does not", and refuse a non-empty change at a header where V20 is not active so the drop is explicit, or (better for EV3's L1 diff) add an opt-in to the MNHF context that seeds the tip's cache entry from that change via `AddToCache()`. That would be a format-2 context; ask the manager for the ABI number per DEC-238. | wording now; seed before EV3 L1 |
| L1 | low | `dash-dml/tests/data/mnhf/crafted.txt`; `dash-oracle-sys/tests/mnhf.rs:384-469` | Within each bit, Core checks the stage before the deployment window (`mnhftx.cpp:232` before `:237`). No vector pins that order. A reference-model mutant that calls `IsValidMNActivation()` before the stage check passes all 46 vectors (mutation run below). Add "a held non-EHF bit": rows `28:30`, bits `28`. The oracle gives `bad-mnhf-duplicate`, verified with a probe, where the mutant gives `bad-mnhf-non-mn-fork`. Optionally also add "a held bit twice" (rows `13:30`, bits `13 13` → `bad-mnhf-duplicates-in-block`, probe-verified), which pins duplicates-in-block before duplicate directly; today that order is pinned only indirectly, through "a non-EHF bit twice" together with "non-EHF before held". | with E1 |
| N1 | nit | `oracle.h:1822` ("v24 evo/specialtxman.cpp:997-1001"), `oracle.cpp` comment `(:1041)` | These are the pin's line numbers, not v24.0.0-rc.3's (`:973-976`, `:1015`), because the oracle hooks shift them. The notes' header says so; `oracle.h` says "v24". Write "the pin's". | optional |
| N2 | nit | `oracle.cpp:4750-4762` | The step does not run `CheckNextBlock()`, unlike `credit_pool_step`: an empty block or one whose first transaction is not a coinbase gets `ProcessBlock()`'s answer (index 0 skipped) instead of `ORACLE_E_ARG`. That answer is still Core's, but it covers a state no node reaches `ProcessBlock()` with. Refuse those blocks as the sibling entry does, or document the gap in `oracle.h`. | optional |
| I1 | info | `dash-dml/tests/mnhf_step_vectors.rs` `valid_activation`, `expired` | `IsValidMNActivation()` skips out-of-window deployments and continues to later ones on the same bit (`chainparams.cpp:113-119`). Expiry checks every deployment on the bit (`mnhftx.cpp:77-86`). Every regtest, testnet and mainnet chain has one deployment per bit, and `-vbparams` cannot move a bit, so no vector can pin the multi-deployment case: a "first deployment on the bit only" mutant survives. The reference model is correct. EV3 must port both loops verbatim. | EV3 brief |

## Scope answers

1. **Core's code paths, inputs and order.** The entry builds `pindex` the way `ConnectBlock()`/`TestBlockValidity()` do (`NextIndex`: `pprev` = tip, `nHeight` + 1, `phashBlock` = the block hash). It then calls Core's own `CMNHFManager::ProcessBlock(block, pindex, fJustCheck, state)` once, as `ProcessSpecialTxsInBlock()` does (pin `specialtxman.cpp:997`; rc.3 `:973`). The manager runs over the context's evodb. Nothing in the verdict is re-derived. The bit list the shim builds itself (`oracle.cpp:4761-4774`) only decides refusals: bits ≥ 32, and whether Core will read the block's median time past. That second decision matches `mnhftx.cpp:214` and `:230-238` exactly: no bit twice, and the first bit not held. `stage_before` is a separate `GetSignalsStage(tip)` call before `ProcessBlock()`. It runs the same Core function on the same `pprev`, so the cache warm-up does not change the result. After a connect, `UndoBlock()` runs as `UndoSpecialTxsInBlock()` calls it (pin `:1041`). The `IsError()` → `ORACLE_E_EXCEPTION` mapping mirrors `mnhftx.cpp:252-256`. The mismatch path in `AddToCache()` (`:368-378`) cannot fire here, because the block's hash is never a seeded header.
2. **Stage state.** Rows are written at exactly the headers where V20 is active, which is where `AddToCache()` writes evodb (`:368`). Below V20, `GetFromCache()` returns `{}` without evodb (`:343-346`), as on a node. A V20-active row is the same whether it comes from cache or evodb. The only difference between the two is the block at `V20Height - 1`, whose signals live in the cache alone. The step's `stage_after` gives the connecting node's answer; the next step on stored rows does not (D13, and M1 for what's missing). Refusals are exact and documented:
   - median time past below the headers: the held signals' blocks when V20 is active after the tip, and the block's own only once Core reaches `IsValidMNActivation()`. The `CoversMedianTimePast()` arithmetic (`height - 10 ≥ first`) matches what `GetMedianTimePast()` reads, including the block-plus-nine-below case;
   - bits ≥ 32 anywhere in the block (D5: broader than Core's assert, documented, and a state no node reaches);
   - a block that does not extend the tip;
   - a `just_check` other than 0 or 1;
   - signal changes outside the version-bits rules.
3. **Coverage.** Every reject reason, both window boundaries (start, and timeout on the inclusive side), expiry at the window start, unknown bits, `fJustCheck`, before V20 and `V20Height - 1` are covered. The mutation run on the reference model kills:
   - `held_before_inblock`, `lazy_inblock`, `all_dup_then_fork` and `first_bit_only` (order mutants);
   - `timeout ≥`, `start ≤` and `expire ≤` (boundary mutants);
   - `no_expiry`, `jc_adds` and `ignore_v20`.

   Two mutants survive: `fork_before_dup` (L1) and `first_dep_only` (I1, unreachable). On the parent-vs-block median time past: the crafted `mtp(H)` and `mtp(H-1)` differ by 600 s and the boundary cases sit at ±1, so a port reading the parent's MTP fails the oracle tests. The L0 lines carry the MTP precomputed, so they don't test that read; the README says so.
4. **ABI 23 bookkeeping.**
   - `oracle.h` `ORACLE_ABI_VERSION 23`, `ffi::ABI_VERSION = 23`, `tests/smoke.rs:135`, the README heading and the README entry row agree.
   - `oracle_v24_0_mnhf_step` is exported by both archives (115 entry points each, matching the MANIFEST).
   - `pins.txt` names 54381c5888 on `node/o2a-mnhf-oracle-v24.0`, and both cached archives hash to the pinned sha256 (plain `121dc512…`, fuzz `d2b3465c…`). The cached `oracle.h` is byte-identical to the branch head's. The implementer's `repro-{plain,fuzz}/{a,second-root}` logs show identical hashes in two roots. I did not rebuild them.
   - SB4 regen: decoded JSON of all 54 files differs only in `provenance` (archive and binary hashes, `core_commit`), plus `vector/instructions_u` in the two `bench-pnbh-mainnet-*` files (−0.12% and −0.16%, within regen.py's 0.5% tolerance). The 73a5ba99e commit message says "only their provenance changes". That's exact except for those two tolerated counts.
   - DEC-238 reserves 23 for O2a-MNHF as a rider build number, and the train driver renumbers at stacking. Nothing here pre-empts that.
5. **Embargo hygiene.** See E1. The Core-side diff, both Core commit messages and the rdc commit messages carry no pointer. The fixture's `provenance` block has no sensitive wording.

## What I ran

| Run | Result |
|---|---|
| `cargo test -p dash-oracle-sys --features oracle`, plain pinned archive (no `DASH_ORACLE_ALLOW_UNPINNED`) | EXIT 0: 225 passed / 0 failed. Includes `mnhf` 7/7, `mnhf_regtest` 4/4, `smoke` 18/18, `sb4_core_scenarios` 3/3. |
| `cargo test -p dash-dml --test mnhf_step_vectors` | EXIT 0, 2/2 (crafted 28 lines, regtest 18 lines). |
| `cargo test -p node-l1 --features oracle --test l1_versionbits`, plain (the `SeedSignalRows()` move; no node-l1 target calls the new entry) | EXIT 0, 2/2. |
| Fuzz pinned archive: `cargo test` does not link it (sancov symbols; README: "only links into libFuzzer binaries") | Linked `smoke`, `mnhf`, `mnhf_regtest` and the probe with clang's `libclang_rt.fuzzer_no_main` via `cargo rustc -- -C link-arg=…`. Results in the line below. |
| Fuzz flavour results | All EXIT 0: `smoke` 18/18, `mnhf` 7/7 (crafted vectors equal the committed ones), `mnhf_regtest` 4/4, probe the same answers as plain. |
| Uncommitted probe in the worktree (`tests/r1probe.rs`, plain) | Held non-EHF bit → `bad-mnhf-duplicate`; held bit twice → `bad-mnhf-duplicates-in-block`; held then another bit twice → `bad-mnhf-duplicates-in-block`. |
| Mutation run on the reference model (Python port, faithful model first: matches all 46 lines) | As in scope answer 3. |
| Gate evidence | `/work/scratch/gate-fast/logs/o2a-mnhf-t1.log` on 73a5ba99e: `GATE_FORCE_ORACLE set`, `tests passed 5760 failed 0`, `GATE EXIT 0`. |

Box load was 200–280 throughout. No test needed a rerun.
