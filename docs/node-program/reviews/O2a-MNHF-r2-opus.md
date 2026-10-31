Verdict: **ACCEPT WITH NITS**. Every r1 finding is closed. Two non-blocking items remain: R2-L1 is program bookkeeping for the manager, and R2-N1 is a comment nit.

# O2a-MNHF r2: Opus-h review, narrow (rdc `node/o2a-mnhf` 739b35e02, Core `node/o2a-mnhf-oracle-v24.0` 9e7f399455)

**Scope:**
- rdc `git diff 73a5ba99e..739b35e02`: the fix 5f3c06c04 and the pins commit 739b35e02.
- Core `git diff 54381c5888..9e7f399455`.
- The rewritten rdc history below the fix.

I'm a designated reader for EMBARGO-6 under PLAN §7. Anything about it is in a private note; see EMBARGO-6 note.

**Setup:**
- Detached worktree of 739b35e02 in `/work/scratch/o2a-mnhf-r2-opus` (mode 0700), with its own `CARGO_TARGET_DIR`s for plain and fuzz.
- `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12`, `DASH_ORACLE_CACHE=/work/cache/oracle`, and no `DASH_ORACLE_ALLOW_UNPINNED`.
- Core read at 9e7f399455, with v24.0.0-rc.3 `0364ed626e` as the reference. `evo/mnhftx.cpp` is identical between the two. `chainparams.cpp` differs only by the `DASH_ORACLE_HOOKS` `OracleForgetParams()` block at `:1422-1432`, outside `:109-130`.

## r1 findings

| r1 | Status | Evidence |
|---|---|---|
| E1 | **closed** | The rdc history was rewritten on the same base, a5d2a9c59. `git range-diff a5d2a9c59..73a5ba99e a5d2a9c59..e748ec034` shows that only the r1-listed strings changed, in d7be94323→c484e7c47, 5840e5930→2a8226e70 and 7585e9844→85cfe4b31. The other two commits are `=`. `git log -p` over a5d2a9c59..739b35e02 (patches and messages) and over Core 5c9e9117a2..9e7f399455 has no "security note", "see manager" or "throw-away" left. `spawn()`, `go_to()`, PROVENANCE `:462-463` and notes D10 now give no reason for the second node. The new strings are neutral; see scope 5. |
| M1 | **closed** | Notes D13 now says "does not". The refusal is `oracle.cpp:4783-4791`; its exactness is in scope 2. The seed is deferred as D16; see R2-L1 for its tracking. |
| L1 | **closed** | Both vectors were added: `crafted.txt:11-12` and `tests/mnhf.rs:419-426`. Rerunning the mutants on the dash-dml reference module kills `fork_before_dup` on "a held non-EHF bit". `held_before_inblock` is now killed directly, by "a held bit twice"; in r1 it was killed only indirectly. |
| N1 | **closed** | `oracle.h:1821` and `oracle.cpp:4807` / `:4813` say "the pin's". Notes D1 still writes the bare `specialtxman.cpp:997-1001`, but the notes' header says the specialtxman lines are the pin's, so that's fine. |
| N2 | **closed** | `oracle.cpp:4761-4765` refuses an empty block and a non-coinbase first transaction, with the same strings as `CheckNextBlock()` (`:4293-4304`). The subsidy-height check is rightly left out (D15). The new cases are in `tests/mnhf.rs:365-373`. |
| I1 | unchanged (info) | `first_dep_only` still survives, as expected. Notes D11 now records that the case can't be pinned and is for EV3. |

## Findings (r2)

| ID | Sev | Where | Finding | Schedule |
|---|---|---|---|---|
| R2-L1 | low (bookkeeping, manager) | Notes D16 (`docs/node-program/designs/O2a-MNHF-notes.md:28`); node-program `PLAN.md:590` (EV3 row, deps "EV2b, O2a-GV"); `BACKLOG.md` (no row) | The deferral is safe today, because nothing consumes the deferred case (scope 2). But D16 is recorded only in the O2a-MNHF notes. EV3's PLAN row does not depend on it, and no BACKLOG row tracks it. EV3 could therefore start its MNHF L1 target without the seed. Core: `evo/mnhftx.cpp:364-383` (`AddToCache()`, the cache entry D16 would seed) and `:337` (`GetFromCache()` reads the cache first). Fix: add a BACKLOG row for D16 (format-2 MNHF context, ABI number from the manager under DEC-238). Make it an `i:` dependency of EV3's MNHF L1 target, and put it in EV3's brief. | before the EV3 brief |
| R2-N1 | nit | `oracle.cpp:4783-4784` ("so a change there would be dropped"); Core commit 9e7f399455 message ("the row was dropped silently") | The refusal is broader than the drop it describes. A non-empty change below V20 was also the change in force for the V20-active headers after it (`SeedSignalRows()`, `oracle.cpp:1601-1611`). So "rows from V20Height hold S", written as a change at `V20Height - 1`, used to work and is now refused. The same state written at `V20Height` is accepted with the same answer (probe P1/P2 below). Notes D14 states the rule correctly ("puts the change at the first header where V20 is active"), so callers are told. Only the code comment is loose. Core: `mnhftx.cpp:368` (rows exist only where V20 is active). Optional: say "a change there sets no row there; put one that should hold from V20 at the first V20-active header (D14)". | optional |

**Info: a second coinbase.** A block with a coinbase first and another coinbase later (`bad-cb-multiple` in `CheckBlock()`) is not refused. The probe gets `Accept`. `oracle_credit_pool_step`'s `CheckNextBlock()` doesn't refuse it either. `extractSignals()` skips non-MNHF transactions (`mnhftx.cpp:190-208`), so the answer is Core's for the same signals. No action.

## Scope answers

1. **Each r1 finding is closed:** see the table. The r1 mutant schedule was rerun on the Rust reference module in `dash-dml/tests/mnhf_step_vectors.rs`, one mutant at a time, restoring and touching the file after each. Killed, with the first failing vector:
   - `fork_before_dup` ("a held non-EHF bit");
   - `held_before_inblock` ("a held bit twice");
   - `lazy_inblock` ("a non-EHF bit twice");
   - `all_dup_then_fork` ("non-EHF before held");
   - `first_bit_only` ("valid before non-EHF");
   - `timeout ≥` ("a non-EHF bit at its timeout");
   - `start ≤` ("a non-EHF bit at its start");
   - `expire ≤` ("re-signal at the window start");
   - `no_expiry` ("active 187");
   - `jc_adds` ("proposal control_signal_block_187");
   - `ignore_v20` ("before V20").

   Only `first_dep_only` survives (I1).
2. **The M1 refusal is exact.**
   - The predicate is `DeploymentActiveAt(*tip->GetAncestor(change.height), V20)` (`oracle.cpp:4785-4791`). It is exactly `AddToCache()`'s write condition (`mnhftx.cpp:368`), on the same header.
   - `SeedSignalRows()` has already bounded `change.height` to `[first_height, tip]` (`oracle.cpp:1588-1590`), so `GetAncestor()` cannot return null.
   - Empty changes are still taken.
   - Core keeps signals off evodb at exactly the refused headers. Of those, only the cache entry at `V20Height - 1` is ever read (`GetSignalsStage()` returns `{}` early below it, `:63`). So the refusal removes no Core-reachable stored state.
   - Every state the oracle could express before is still expressible. The rows at V20-active headers are written as a change at the first V20-active header, and that includes a row that holds the `V20Height - 1` signal (probe P2).
   - The one answer that was never obtainable is the `V20Height` step on a cache-seeded tip. Before, the oracle silently answered the restart case instead; now it refuses (probe P1, and `tests/mnhf.rs:325-339`).

   **Deferring the seed to D16 is safe now:**
   - No current consumer uses the deferred case. `oracle_mnhf_step`'s only callers are `dash-oracle-sys/tests/{mnhf,mnhf_regtest}.rs`. No node-l1 target calls it (grep). `l1_versionbits` goes through `oracle_versionbits_state`, whose rules D14 leaves unchanged (it passes).
   - The L0 vectors carry the stage as an input. The reference model is therefore stage → verdict, whatever the stage's origin, and the 48 vector lines (crafted 30, regtest 18) are unchanged apart from the two L1 additions.
   - The crafted "V20 from the next block" is the connecting step at `V20Height - 1`. It is still obtainable, and its line is unchanged.
   - EV3 has not started. R2-L1 makes sure it waits for D16 before its L1 target.
3. **No verdict, `stage_after` or boundary moved outside the findings.**
   - `crafted.txt` gains exactly two lines. `regtest.txt` is unchanged, and both oracle tests regenerate the vectors and compare.
   - The `signals_below_v20_are_not_stored` rewrite keeps the same `stage_before`, verdict and `connected` for "the first block with V20". The empty change at the tip gives an identical `MnhfStep`.
   - `mnhf_regtest` (4/4, including the 257-height `getdeploymentinfo` comparison and the proposals) passes on the new pin.
   - The new refusals come before any read of Core state, and every one is `ORACLE_E_ARG`.
4. **Pins and ABI bookkeeping are correct.**
   - `pins.txt` names 9e7f39945518… on `node/o2a-mnhf-oracle-v24.0`, the branch head. The cached archives hash to the pinned values: plain `81a27ea9…`, fuzz `a17dee7b…`.
   - Both MANIFESTs name the commit, and both archives export 115 `oracle_v24_0_*` symbols, as at r1. The cached `oracle.h` (plain and fuzz) is byte-identical to the Core head's.
   - The implementer's repro logs show identical hashes in two roots for each flavour. I did not rebuild them.
   - The ABI stays 23: `oracle.h:80`, `ffi::ABI_VERSION`, `tests/smoke.rs` and the README all agree. The change only adds refusals and docs to an unmerged rider. Under DEC-238, 23 is a rider build identifier, and the train driver assigns the installed number. D16 records its number as "next" from the manager, as the amended DEC-238 asks.
   - SB4 regen: the decoded JSON of all 54 files differs only in `provenance` (`core_commit` = 9e7f3994… in all 54, plus the archive and binary hashes). The exception is `vector/instructions_u` in `bench-pnbh-mainnet-dgw` (+0.24%) and `-kgw` (+0.07%), both inside regen.py's 0.5% tolerance. The 739b35e02 message says only "regenerate", which is accurate.
   - Gate `gfast-o2a-mnhf-t1r2` on 739b35e02: `GATE_FORCE_ORACLE set`, `tests passed 5760 failed 0`, `GATE EXIT 0`. Bench inputs are UNCHANGED 0/13 (`/work/scratch/o2a-mnhf/bench.log`).
5. **Embargo hygiene:** see EMBARGO-6 note. The public result is that every new string is mechanism-neutral and carries no pointer. That covers both commit messages on each side, the `ORACLE_E_ARG` message, `oracle.h:1844-1846` and `:1874`, `src/mnhf.rs:95-100`, notes D13–D16, the README and the generator docstrings. The rewritten history carries no pointer either.

## What I ran

| Run | Result |
|---|---|
| `cargo test -p dash-oracle-sys --features oracle`, plain pinned archive | EXIT 0: 227 passed / 0 failed. That is the branch's 225, as at r1, plus my 2 uncommitted probes. It includes `mnhf` 7/7, `mnhf_regtest` 4/4, `smoke` 18/18, `sb4_core_scenarios` 3/3 and `llmq` 28/28. No reruns were needed. |
| Fuzz pinned archive (`DASH_ORACLE_FLAVOR=fuzz`, its own target; `cargo rustc --profile test -- -C link-arg=libclang_rt.fuzzer_no_main-x86_64.a -C link-arg=-lstdc++`; 2119 `__sancov` symbols in the binary) | All EXIT 0: `smoke` 18/18, `mnhf` 7/7 (the crafted vectors equal the committed ones), `mnhf_regtest` 4/4, and the probes 2/2. |
| `cargo test -p dash-dml --test mnhf_step_vectors` | EXIT 0, 2/2. |
| `cargo test -p node-l1 --features oracle --test l1_versionbits`, plain. No node-l1 target calls `oracle_mnhf_step`. | EXIT 0, 2/2. |
| Uncommitted probe `tests/r2probe.rs`, plain and fuzz, V20 from 30 unless noted | Every probe answered as the refusal rule predicts. **P1:** a change at 29 `{13:29}` → `ORACLE_E_ARG`. **P2:** the same set at 30 → `stage_before` `{13:29}`, `bad-mnhf-duplicate`, and a beside-bit row `{13:29, 14:40}`. **P3:** a below-V20 change overridden at 30 → refused. **P4:** an empty change at 5, then `{13:30}` at 30 → taken. **P5:** a change at 12 → refused. **P6:** V20 from 50, a non-empty change → refused; an empty change at the tip gives the same answer as no change. **P7:** V20 from 39, a change at 39 → taken, and at 38 → refused. Empty block → `ORACLE_E_ARG`. A second coinbase → `Accept` (info above). |
| Mutation run on the dash-dml reference module (12 mutants) | 11 killed, 1 survivor (`first_dep_only`, I1); see scope 1. File restored afterwards (`git status` clean apart from the probe). |
| SB4 decoded-JSON diff, 73a5ba99e vs 739b35e02 | As in scope 4. |

The box load was about 100–110 throughout. The worktree and targets are deleted.
