# SB4 `dash-chain` — Sol narrow r3

Reviewed exact head **20422f0349e9c2b47b4a037826f17900f22b11a9**, branch `node/sb4-dash-chain`, range **69745378a..20422f034** (53 commits, 41 changed files). Review and experiments ran in the detached worktree `/work/scratch/sb4-r3-sol/wt`. The branch under review was not edited; scratch mutations were restored, and the scratch worktree is clean. No commits, pushes, GitHub actions/comments, private reads, or protected chain/benchmark access.

Read Sol r1/r2, `SB4-opus-high.md`, `checkpoint-r2.md`, `review-r2-opus.md`, the SB5 API handoff, REVIEW-COMMON, SB4 rev7.1/D47 and DEC-140/187. Applied the code-review-validator and dash-core-security-auditor workflows. Core comparisons use tag `v24.0.0-rc.3` (`0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`) and the pinned oracle/harness shim `9c6dd45e0b10d245c7afe452426d8ff965416547`.

## Findings and scope qualification

**No new blocker, major, or minor code findings in this SB4 range.** The original MJ-1 and all five original Opus minors are closed. Sol r1's cold-tip and cold-walk fixes remain intact.

There is a residual documentation nit, detailed below. The reported assertion that **`open` now rejects stored anchors 1..=23 is not established on this head**: SB4 contains no `open`. Its API handoff tells SB5 to implement that check; the adjacent SB5 worktree has not incorporated this constructor change. This is an SB5 rebase/reopen obligation, not a claim that SB4's `debug_assert` validates persistent metadata in release builds. This approval covers SB4, not SB5's reopen integration.

## Closure of the original Opus items

| Item | Status and mechanism/evidence |
|---|---|
| **MJ-1 — SPV locator stops at window** | **Closed.** `reader.rs:229` uses fallible resident/cold links, Core's height density, and an explicit anchor terminus. `snapshot.rs:143` supplies cold-capable fork recovery. Density, cold-read fault, peer-fork and E3 recovery tests pass. Independently deleting the terminus kills both locator density and peer-fork tests. |
| **mi-1 — missing vectors/design halves** | **Closed for the SB4-owned halves.** Registrations are generated with the tests, checked against files on disk, and checked on each load/end. The non-regtest, tie, deep-fork and reload vectors now execute. A-1/A-2 CS3 rule rows, A-8 resident/cold/oracle, A-13 public API, A-37 publication/hold, A-27 cold leaves and E3 recovery are present and pass. The proposed cross-repository design-row ledger remains explicitly manager-owned (D-R2-B-10); it was not silently claimed implemented. |
| **mi-2 — stage 5 loses unapplied marks** | **Closed.** `Mark { status, applied }`, `set_status` (`chain.rs:1942`), `stage4_effects` (`:1993`) and `records_committed` (`:2117`) preserve unapplied marks. New writes reset `applied`; stage 4 sets it; stage 5 retains only unapplied marks. Writer reads retain overlay precedence, and reader publication waits for stage 4. Replacing retention with `clear()` kills two regressions. |
| **mi-3 — public node mutators in SPV** | **Closed.** All eight index mutators, including reset and ChainLock recording, are crate-private on `HeaderChain`. `NodeIndex` has a private chain field and is obtained only through the `WrongMode`-checked `node_index()` (`node_index.rs:53`). Six compile-fail doctests and the runtime mode test pass; the checked public API reflects the move. Remaining public node tip mutators call the mode guard. |
| **mi-4 — surviving mutants** | **Closed for each named survivor.** Independently killed the first best-header walkback deletion, mark-after-host-hook reorder, stale-plan origin deletion, cold-leaf `>=`→`>` change, retained active leaves and retained arena chunks. The existing production behavior was already correct; the missing witnesses now fail on actual mutations. Details below. |
| **mi-5 — loom not wired** | **Closed.** `contrib/node/check-loom.sh` and the workflow's `loom` job run the production `SeqGroup` model. `--exact`, `pipefail`, a timeout and the `1 passed` check refuse a renamed/cfg'd-out model or timeout. Independently ran the script: one model passed with the production two-preemption bound. |
| **n-1 — fatal partial accept** | **Closed.** `chain.rs:1029` documents the before-work refusals and fatal later errors, partial inserted index and absent publication. It does not promise recovery or a partial listener result after a fatal error. |
| **n-2 — IBD clock precision** | **Closed.** `NodeTime`/`Clock::node_time` preserve subsecond time. `shared.rs:113` compares the integer tip time plus max age against the full time; exactly at the threshold is fresh and a nanosecond later is old. IBD regression and all three dash-clock tests pass. |
| **n-3 — corrupt parent height overflow** | **Closed.** `child_height` (`chain.rs:658`) uses `checked_add`, reports typed corruption and trips the latch. Header verdict checks it before rule reads; the shared derivation checks it too. |
| **n-4 — SPV unwind latch** | **Closed.** `set_active_tip` catches unwind inside the critical section and trips the latch before resuming unwind (`chain.rs:1711`); `Hold::drop` does likewise during unwinding (`:2461`). Normal release/abandon are unaffected. The committed unwind test passes. |
| **n-5 — inaccurate eviction lock text** | **Still a documentation nit, explicitly handed to the manager by D-R2-C-7.** The code's lock behavior is accepted by D-A11-3, but the design text is still inaccurate. See residual finding below. |
| **n-6 — `Floor { enforced: None }` doc** | **Closed.** `chainlock.rs:25` now includes node recording, observed-off state and the second active-state check resetting the floor during a live spork change. |
| **n-7 — duplicated derivation** | **Closed.** A single `derive` (`chain.rs:602`) reads resident/cold ancestors through one shape and computes height, skip, work, time maximum and MTP together. |

## Requested items 1–7

### 1. Locator, Core density and anchor rule

Core `chain.cpp:32–54` pushes the current hash, chooses `max(height − step, 0)`, then doubles the step after `have.size() > 10`. `walk::locator_heights` preserves that order. `try_locator` visits those heights through `walk::ancestor` using `try_link`, keeps the entries at/above the accepted anchor, and appends the anchor once when Core's next jump would cross it. There is no window-edge cutoff. A context-entry locator is just that entry; locators from accepted tips end at the anchor, never at the context floor.

The committed w=10/H=40,000/tip=40,200 test verifies the cold doubling entries through 40,063 and the final 40,000 hash. Its peer is anchored eight blocks lower, so removing the terminus produces the wrong fallback rather than passing accidentally. My terminus mutant fails both tests. Read errors trip the latch; `try_locator` returns `Storage`, while the infallible form returns empty for the stopping client (D7).

E3 now gets the first matching **cold** locator entry on the retained active chain, not an unconditional anchor fallback. `wallet_locator_lost_disabled_suffix` returns height 37 and checks the own-tip and all-unknown controls. Its restoration is the declared SB4 `MemHeaderStore` stand-in (D-R2-A-5), not proof of SB5 `open` or SB14's wallet transaction.

### 2. Coverage registry, design halves, marks, mutators, mutants, loom

`scenario_test!` (`scenario_tests.rs:47`) generates both a plain test and its registration. The body runs in a nested function, so an early `return` still reaches the end check. `begin` checks registration and the libtest thread; each loader checks the vector; `end` requires every declared vector to have loaded. The coverage test (`coverage.rs:167`) requires exactly the disk vectors plus the reasoned exemptions, with no duplicate/disjointness mistakes. Only the two benchmark inputs remain `NOT_REPLAYED`; their verdict batches have separate replays. The original seven missed cases, including the two devnet variants, run in the passing suite.

**Requested ignore experiment:** I inserted `#[ignore]` inside the `replay_smoke_reload` declaration and ran the registry-check cargo command. It returned **101**, with `no rules expected ignore`; this refusal is at compilation, before the registry test runs. Separately, putting `#[ignore]` on the macro invocation produced rustc's warning and the scenario still ran: **1 passed, 0 ignored**. Thus an ordinary ignore cannot create a registered-but-skipped scenario. After restoration the exact registry test passed (1/0).

The stronger coverage is substantive: CS3 rebuilds and hashes Core's exact recorded inputs; 39 header rows run at node/RuleContext level, and 37 anchored rows run at both window sizes with the two node-only rows asserted by name. Scenario network arguments are rebuilt and checked against provenance. Reload compares every entry's status, height, parent and work plus unchanged record bytes. A-37 probes the instant of snapshot publication and the hold's interior; A-27's pinned generator asserts both cold-leaf and cold-only-target coverage.

Stage-5 preservation and node-only access are production changes, not injected test behavior. The publication test hook is test-only; production stage 4 still applies all marks before publishing, and hold release does so once.

### 3. Anchor boundary, A-M1/A-M2 and D70

`init_active_tip` (`chain.rs:1390`) refuses a tip below `shared.anchor`; `tip_target` (`:1577`), the sole `TipTarget` producer, refuses a fork below it before building legs. Both bounded and unbounded planning and hold substeps go through this guard. New peer headers whose parent is context already stop with `BelowAnchor` (`:1175`). The anchor id remains on every supported active chain, including after eviction and a disconnect back to the anchor. The reader fallback no longer panics if a manually constructed view lacks it.

`empty`, `Shared::new` and `ActiveChain::starting_at` take the anchor only and derive the floor through `floor_below`. Genesis gives 0; an anchored pack gives H−24. `check_pack` refuses low/pre-DGW packs, and `new_anchored` refuses `Permanent` before construction (`chain.rs:790`). These are configuration/support-boundary checks; they add no rule to the genesis-origin node path.

**Independent near-anchor reorg:** anchored regtest at H=40,000, w=10; advance A to H+100 so the anchor is cold. Receive a valid B branch rooted at H with 101 headers. Acceptance succeeds; the plan has fork H, **100 disconnects / 101 connects**; stage 4 publishes B40101; locator and unknown-locator fallback retain the anchor. Then receive a higher-work C branch rooted at H−1 that replaces the anchor and reaches H+102. The first header returns `BelowAnchor`; no ids are inserted, and tip/epoch remain unchanged. Direct tip/init requests for the H−1 context id also return `BelowAnchor`. Probe and log are retained in scratch.

**Yes, the H−1 refusal is stricter than Core and a real peer sequence can hit it.** I independently ran the corresponding header sequence through the pinned Core harness: A40100, B40101 rooted at A40000, and C40102 rooted at A39999 were all accepted; the best header moved to each higher-work branch. No ChainLock floor was installed. The scenario disables only the harness's expensive `CheckBlockIndex` consistency walk. Core's `AcceptBlockHeader` has no SPV context/anchor predicate.

That difference is already declared by SB4 §5.3/D13 and §10.1 n6: an anchored client accepts descendants of its chosen anchor, and a driver changes anchors by reconstructing the chain (SB15/anchor fallback). This round does not newly reject a previously accepted peer branch above the anchor. The H-fork reorg works; the H−1 reorg requires the host's anchor policy. Do not describe anchored SPV as unrestricted Core reorg parity.

**Stored anchors 1..=23:** `anchor.rs:73` adds the debug assertion accepted by Opus N3, and healthy SB4 construction refuses such packs. There is **no release-mode metadata refusal in SB4**. `sb5-r2-api.md:29` says “open must validate a stored anchor … otherwise return Corrupt.” Read-only inspection of the adjacent SB5 `chain/sb5/open.rs:53–57` still showed the old floor/anchor constructor call and saturating subtraction. I did not change or test that independently active worktree. SB5 must validate meta before calling `empty`, including a nonzero height paired with the genesis hash, and test the typed `Corrupt` result. This is outside the exact SB4 range.

### 4. DEC-187 disconnect list and consumers

`mark_conflicting_block` (`enforce.rs:262`) appends to the report only in node mode. `VirtualTip::disconnect_tip` (`host.rs:83`) always appends the old cursor to its own list before lowering it. The SPV step retains that `VirtualTip`; there is no code path that drains/discards its list as part of enforcement. `fork_target` uses its cursor; activation's separate `TipChange` is the actual tip-change event payload, not a consumer of the removed report copy.

All SB4 SPV report consumers/tests use `vt.disconnected()`. Node consumers retain `report.disconnected`. The A100/B110 regression asserts `(report length, virtual length) == (0, 30)`; my duplicate-SPV-report mutant is killed. The node critical-section test still verifies its complete disconnect list.

No production consumer outside dash-chain uses the field in this exact tree. SB5's handoff explicitly identifies SPV callers requiring migration. Read-only inspection confirms its reopen CL tests already use the split; other `r.disconnected` references include node enforcement and `TipChange`, so a textual replacement of every field access would be wrong. SB5's eventual rebase remains its own gate. N5's documented silent-empty API tradeoff is accepted by DEC-187.

### 5. D68 typed corruption for leaves/children

`StoreError` (`error.rs:12`) is a non-exhaustive `Io`/`Corrupt` enum with `what()` and `is_corrupt()`. `HeaderStore::leaves` and `children` return it directly (`store.rs:52,54`). `Shared::cold_children/cold_leaves` preserve the variant and trip the latch using Display (`shared.rs:294,299`); the latch message retains “corrupt”. Structural read checks funnel through `Shared::corrupt`, and undecodable memory-store rows use `Corrupt`; injected read/write failures remain `Io`.

My scratch probe corrupted the actual stored leaf's row kind, then called **both** `leaves()` and `children(parent_hash)`: both returned `Corrupt` naming that row. This exercises enumeration decoding, not just the error constructor. Reclassifying `Shared::corrupt` to `Io` kills the cold-link regression. Backend schema/index integrity checks still belong to their implementations; this change supplies and propagates the type, rather than validating every backend's enumeration inside SB4.

### 6. B-L5 KGW evidence is no longer vacuous

`sb4_kgw_max_walk.rs:36–53` unwraps every oracle result. It verifies the full 4,033-entry context's recorded target, the identical target with the guard header 15,967 omitted or its fields changed, and a different target when contributor 15,968's bits change. The comment correctly distinguishes Core's step to the guard from reading its fields.

I ran the pinned oracle test successfully. Shortening its control to 4,031 entries now fails at `unwrap` with `KGW needs 4032 headers`, so the former error-as-success path is gone. I also halved production Rust KGW's contributing window: **both** `kgw_max_walk_resident` and `kgw_max_walk_cold` failed, producing `0x1e00ffdf` instead of Core's `0x1e00ffef`. The restored tests passed (2/0). The oracle test itself links Core, not Rust KGW; it pins the expected answer, while dash-chain's two tests catch the Rust mutation.

### 7. a0952ed4e — per-operation OWED

`coverage.rs:67` keys obligations by `(vector, op, owner)`. `replay` (`scenario_tests.rs:646`) refuses every otherwise unhandled op unless that exact vector owes it, records the injected op, and finally requires equality with the owed set (`:668`). Unknown `build.submit` values panic. The coverage check rejects duplicate obligations and empty owners.

Independent mutants: removing mainnet stale-branch's `seed-cl` row fails with “neither replayed nor OWED”; adding an unused `timer` obligation to a19 fails with “OWED lists exactly the injected ops.” Both were restored. This closes the Opus re-check's m1.

## Further Opus re-check items

A-M1/A-M2, B-M1, D-M1 (A-37 hold release), A-L1 (peer below terminus), A-L3 (fallible recovery docs), B-L2 (arguments/SHA), B-L3 (anchored row count), B-L4 (A-18 verdicts), B-L5 (KGW), B-L6 (reload index), D-L1 (loom actually runs), D-L2 (pinned cold seed), D-L3 (reset/record access) and DEC-187 are closed by the mechanisms above and passing regressions.

The remaining declared qualifications have not disappeared:

- **N1 / D-R2-H-1:** registration-forgery prevention counts literal source text. A deliberate alias can evade it. This is an internal test-harness enforcement limit, not an ordinary `#[ignore]` bypass or runtime issue.
- **N2 / D-R2-H-2:** no current Core vector changes a status during reload, so the reload-status override survivor remains unexercised by an oracle vector. Production FAILED_CHILD propagation has its unit test; a harness invalid-mark-plus-reload vector remains owed to the next regeneration.
- **N3:** debug assertion and pack refusal are closed; SB5 metadata validation remains owed as explained above.
- **N4:** the identified long `NodeIndex` doc lines were wrapped at 20422f034.
- **N5:** accepted/documented mode split; SB5 callers must preserve the actual list through `VirtualTip`.

## Residual finding — nit: design still describes old stage-5 and lock behavior

**Location:** `docs/node-program/designs/SB4-dash-chain.md:157`, `:804`, `:925` (also stale in the canonical node-program design).

**Scenario/evidence:** a reader implementing the documented lock order sees “write lock only to insert, never while holding the tip lock,” but production `stage4_effects` calls eviction under that lock, and eviction calls `index.remove` (write lock). This is original Opus n-5/D-R2-C-7, not a new deadlock. Likewise, the unconditional “stage 5 drops the overlay” no longer describes a carried unapplied mark after an abandoned/failed stage 4; `records_committed` deliberately retains it.

**Fix:** update the manager-owned design's lock paragraph to cite D-A11-3 and include eviction's index write under the tip lock; update §3.7/§9.1 to say applied marks are removed and unapplied marks carry to the next stage 4. The production contract and crate docs already describe the repaired behavior. Nonblocking for this code gate.

## Independent validation and artifacts

Every build/test/harness run used **nice 10**, **CARGO_BUILD_JOBS=8**, **RUSTC_WRAPPER=** (same as the implementer's reported build), and **TMPDIR=/work/scratch/sb4-r3-sol/tmp** (0700). Seeded the per-worktree target using `cp -r --reflink=always`; it is deleted after review. Logs, scripts, probe sources and Core scenario evidence remain under `/work/scratch/sb4-r3-sol/`.

| Check | Result |
|---|---|
| `cargo test --locked -p dash-chain --all-features` | **198 unit passed, 1 ignored; 1 fixture integration passed; 11 doctests passed.** Six full-history and one memory integration case remain ignored. |
| `bash contrib/node/check-loom.sh` | **1 model passed**, 32.05 s model time; bounded two-preemption production seqlock model. |
| `cargo test --locked -p dash-oracle-sys --features oracle --test sb4_kgw_max_walk` | **1 passed** with the pinned archive. |
| `bash contrib/node/check-public-api.sh` | **Passed** against the checked listing and pinned nightly/tool. |
| `cargo clippy --locked -p dash-chain -p dash-clock --all-features --all-targets -- -D warnings` | **Passed.** |
| `cargo test --locked -p dash-clock` | **3 passed.** |
| `cargo fmt --all --check` after scratch restoration | **Passed.** |
| Near-anchor and enumeration scratch probes | **Passed**; log also includes the pre-existing r3 snapshot test matched by the filter. |
| Restored exact registry check / restored KGW tests | **1/0** and **2/0**. |
| Core near-anchor header scenario | **3/3 accepted**, best headers A40100 → B40101 → C40102. Harness binary SHA-256 matches its MANIFEST. |

Mutation logs independently show failures for: ignored scenario syntax; missing/unused OWED op; half KGW window; undersized oracle window; missing locator terminus; cleared unapplied overlay; missing first best-header walkback; mark after host hook; stale plan origin; strict cold-leaf bound; duplicate SPV disconnect list; corruption reclassified as I/O; retained active leaves; and retained arena chunks. The external-macro-ignore control runs rather than skipping. All mutations were reverted. An initial large Core run with `CheckBlockIndex` enabled was stopped by its explicitly identified owned PID and rerun with the harness's documented large-scenario setting; no process was killed by a command-line pattern.

The reported fecc3c6dd workspace-gate failures and low-load dashd reruns are intake evidence, not independently reproduced here. I did not rerun the full workspace gate, H-M/H-T nightly histories, memory/performance gates, SB5 reopen/feeds, CH1's block validation, or CH2b durability. This narrow review does not replace those stack gates.

**Verdict:** APPROVE-WITH-NITS

Sol gate satisfied: yes
