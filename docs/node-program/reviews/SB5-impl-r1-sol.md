# SB5 implementation — Sol r1

Reviewed on 2026-10-11. **Two major memory-bound failures require changes.** The normal publication, feed, windowing and reopen paths passed the targeted checks described below; those passes do not establish the advertised SPV step bound for every accepted header tree and configuration.

## Scope and authority

- Branch: `node/sb5-feeds`; exact reviewed head: `b640572b4ff54375729bb7fc0a8c3f5796a6a6c6`.
- Base: `78eb1f79ec828404ceee0e2d1fe22e2bdb503b05`, SB4 fix-r2 integration including DEC-187's single disconnect list and the §9.1 recipe.
- Reviewed range: `78eb1f79e..b640572b4`, 52 commits, 58 changed files, 15,791 insertions and 171 deletions. Independently verified all 52 commit signatures as `G`. The implementer's assertion that every code commit builds independently was not separately replayed.
- Detached, clean review worktree: `/work/scratch/sb5-sol/wt`. All source locations below refer to the exact head. No repository source was changed, committed or pushed; no GitHub operations were performed.
- Contracts: approved SB5 rev3 at `e60728067` (DEC-159), its D1–D74 notes/errata at the implementation head, relevant SB0 shared-base contracts and applied errata, DEC-157, DEC-174, DEC-187, and `briefs/REVIEW-COMMON.md`. The external approved SB5 document was checked against the approved commit. Both requested design/implementation memory notes were read as context, rather than treated as proof of correctness.
- Review followed the code-review-validator workflow. PLAN §7 handling remained codenames only; no private material was read.

## Findings

### R1-M1 — major: store result vectors add an unbounded branch-width term to SPV steps

**Location:** `dash-chain/src/budget.rs:34`; `dash-chain/src/chain.rs:736` and `:761`; `dash-chain/src/chain/enforce.rs:221` and `:443`; `dash-chain/src/store.rs:52`. Relevant allocator gate: `dash-chain/tests/step_budget.rs:125` and `:208`.

**Scenario.** Use an anchored SPV chain at window 32, the mobile feed defaults, cold-cache capacity 256, and ChainLocks enabled. Accept 9,000 distinct valid one-header siblings of the anchor in legal batches, then a separate 100-header trunk. The siblings leave the resident window. Apply a ChainLock to the last sibling at anchor+1, through the plan-free SPV step recipe. Then execute an empty step with no new lock or marks.

**Failure.** `children` materializes all stored child records before collecting their ids. `cold_leaves` materializes all stored leaf records before filtering and scanning them. Neither collection is bounded by B, W, E or C. The cold-leaf work bound can force this scan again on a subsequent quiet step: CONFLICT-marked leaves remain stored leaves and retain their work. A bound stated solely as cap + M(m) + L(m) therefore misses branch width, including when the current step has m=0.

**Evidence.** Standalone probes link the unchanged compiled head and use the same counting-allocator observation as A-F12: peak counted live bytes minus the larger of the start/end baselines; store transaction work and test callbacks are excluded. Stage 1 does not call `tip_plan`.

| Probe / step | Observed transient bytes | Allowed bytes | Result |
|---|---:|---:|---|
| `wide.rs`, lock; m=9,099 | 4,651,904 | 705,952 | `Ok((100, 1))` |
| `wide.rs`, subsequent quiet step | 4,289,664 | 192,364 | `Ok((0, 0))` |
| `result-only.rs`, same lock after reopen | 1,863,936 | 705,952 | `Ok((100, 1))` |
| `result-only.rs`, subsequent quiet step | 1,584,880 | 192,364 | `Ok((0, 0))` |

The lock allowance is the mobile cap 189,824 + M(9,099) 319,520 + conservative L(9,099) 196,608. The quiet allowance even includes the minimum M(0)=2,528 and L(0)=12. The normal probe's budget account nevertheless reports `cap=189824, peak=4000, marks_peak=213008`, while returning success.

`result-only.rs` is a stronger isolation of the cause: a thin `HeaderStore` wrapper excludes *all* underlying MemHeaderStore allocation work for `children`/`leaves`, then charges only an exactly sized returned `Vec<HeaderRecord>`. Its 9,000-leaf result alone holds 1,584,000 bytes at 176 bytes per record. These returned, caller-owned heap records are neither SQLite page-cache memory nor persistent arena residency. The excess is therefore not an artifact of counting MemHeaderStore's internal decoding vectors.

`wide.rs` reproduced identically in debug and optimized builds. `result-only.rs` reproduced in an optimized build and again on rerun. Sources and output are preserved under `/work/scratch/sb5-sol/probes/{wide,result-only}.rs` and the corresponding `.log` files; `wide-release.log` records the optimized ordinary-store result.

**Design impact and attribution.** The full-vector store APIs and their chain consumers are inherited from SB4. This finding is against SB5's new all-step memory guarantee and its A-F12 gate, not a claim that SB5 introduced those primitives. DEC-187 corrected the overlay/list terms and removed the full stage-1 plan; those corrections do not cover branch-width read results. The existing 9k/50k allocator fixtures grow two long branches, leaving too few siblings/leaves to expose this failure.

**Fix.** Stream or page child and cold-leaf enumeration with bounded caller-owned storage. Preserve id order, writer-side dirty-child merging, resident/uncommitted filtering and exact fork-choice results without collecting every full record or retaining an oversized vector after collecting ids. Add counting-allocator regressions for a wide sibling tree, its lock step, and a quiet step after the lock, including after reopen. Changing the guarantee to add a separate branch-width term would require a manager-approved design correction; simply increasing M(m) cannot cover the quiet m=0 witness.

### R1-M2 — major: the fixed step term does not cover valid larger feed tables

**Location:** `dash-chain/src/budget.rs:28` and `:34`; `dash-chain/src/feed/table.rs:326` and `:470`; `dash-chain/src/feed.rs:225`.

**Scenario.** Use the valid configuration Q=E=W=C=1, B=5, `max_feeds=64`, window 32. Register and activate all 64 readers before the measured step; retain one ordinary registry snapshot. One initial header fills each Q=1 queue. In the next five-header segmented step, deregister eight readers at each writer wait, and drain the readers. Keep the reader objects alive so freeing their pre-step reservations does not distort the transient measurement.

**Failure.** Deregistration itself never waits on the held tip lock. The real writer's `retry_pending` subsequently retires closing entries and calls `republish` inside the step. That allocates registry vectors proportional to feed count, with growth/overlap across publications. `step_cap` ignores `max_feeds` and always budgets only 1,024 bytes for the fixed term, although validation accepts every positive feed count. There is no fixed-term check to reject this overrun.

**Evidence.** `feeds64.rs` uses the same A-F12 allocator observation. The deregistration/drain callback's allocations are excluded; registry rebuilding by the writer is counted. It reports:

```text
many-feeds: cap=1604 observed=9856 result=Ok(5) retained=64 new=32
```

This is 9,856 transient bytes against the entire 1,604-byte step cap. It also exceeds 4,144 bytes, even if the minimum M(0)+L(0) is allowed despite this being a mark-free step. The current registry's lasting allocation is already subtracted by the observation method. The optimized rerun reproduced the same result (`feeds64-release.log`).

A 1,000-feed version (`lifecycle.rs`) reports 66,176 bytes against the same 1,604-byte cap, with `Ok(5)`; debug and optimized results match. The 64-feed case suffices and does not require an extreme configuration. Sources and logs are in `/work/scratch/sb5-sol/probes/`.

**Design impact.** §6.2 explicitly places registry republish in the step fixed term, separately from §6.1's persistent per-feed reservations. §6 requires caps derived from configuration and real sizes. Carrying over the approved default 1,024-byte estimate does not satisfy that contract for every configuration the implementation accepts. The existing allocation fixture does not vary feed count together with small B/W/E/C or retire feeds during the measured hold.

**Fix.** Derive the registry transient allowance from `max_feeds`, the real entry/container sizes, actual vector capacity and overlapping allocations; reserve predictably to make that bound provable. Alternatively, validate a supported upper limit consistent with a proven fixed allowance. Include allocator tests with small B/W/E/C, larger feed counts, retained registry snapshots and in-hold deregistration. The observed peak, not just the manually charged sub-step legs, must satisfy the resulting cap.

### R1-m1 — minor: P2 and the directed replay helper still build the full stage-1 plan

**Location:** `dash-chain/src/chain/sb5/p2_tests.rs:141`; `dash-chain/src/chain/sb5/a23_tests.rs:112`.

**Scenario and evidence.** P2's shared `Side::step` calls `c.tip_plan(target.id)` before stage 3. The directed window/restart scenarios reuse this helper. The A-23 every-publication property also invokes the full planner before segmented activation. These calls warm cold ancestry and, in P2, install a pending full plan that stage 4 later consumes, rather than following DEC-187/D71's plan-free stage 1. The calls are present at the reviewed head; all of these tests pass.

**Impact.** This is a test-fidelity gap, not a demonstrated production chain error. P2 still provides useful equivalence and locator evidence, but proves the plan-warmed recipe on both sides. It cannot by itself establish equivalence of the required plan-free recipe or its memory behavior. The separate `test_util::SpvRig::step` correctly omits the full plan and supplies the memory probes above.

**Fix.** Derive the target `BlockRef` directly, pass it to `write_header_step` only when moved, and let stage 3 use `tip_target` and stage 4 use bounded sub-step plans. Apply the same recipe to the directed/P2 and A-23 helpers, then rerun them. Keep full-plan compatibility tests explicitly separate if still useful.

## Confirmations

These are supported by source inspection and the passing tests, subject to the limits below.

- **Tip publication and sequence protocol.** `HeaderChain::publish_locked` is the normal snapshot-store seam and calls `after_publish` once. The constructor's initial snapshot deliberately has no subscribers and skips it. `after_publish` publishes status after the snapshot and before final fanout. Intermediate hold steps fan out without publishing intermediate snapshots; the final snapshot carries the final seq. Segmented Composite `first_seq` is `last_seq + 1 - (d+c)`, with pure disconnect runs before connect runs. Quiet steps also publish once. The seq/fanout, segmented-versus-single, fence and every-store properties passed.
- **Feed data, control and backpressure.** Cursor advancement follows successful pushes; a cursor-bearing target receives no direct later fanout; pending segments trim below the minimum cursor. Queue plus staging obey `used ≤ Q`. A full slot produces shared pending data and `Blocked`; the fanout call does not wait for the subscriber. The service loop deliberately waits/drains before the next step or sub-step. This is the E6a/SPV-LO9 design, rather than a promise that the entire header activation never parks. Reader intake swaps reserved buffers. Per-kind controls bypass a full data queue, coalesce in the slot and survive compression. Slot sections do not allocate in the checked paths; drops and notifications happen outside those sections.
- **Registration and close.** Registration/handoff use `try_lock`; an active hold yields `HoldActive`, and the owner thread gets `WouldDeadlock`. Handoff's live cut excludes already assigned pending events. Deregistration linearizes at closing and uses lazy reclamation when step 4 cannot acquire the tip lock. Generation and slot identity checks protect delayed cleanup after reuse. Closing ends writer cursor waits. Holds clear ownership, release the guard, then fire release wakes; threaded tests and loom negatives cover the corresponding lost-wake/deadlock regressions and unwind paths.
- **Lock ordering.** Slot holders do not take the tip lock, invoke subscriber handlers or read the store. A hold's retry uses its existing guard. Cold reads may take cache → store during a hold, relying on the store contract that neither holder waits for the tip writer. Stage-5 invalidation runs after release. No additional demonstrated lock cycle or wake loss was found in the covered production paths.
- **Writer window and deep disconnect.** Segmented activation calls the shared writer windowing rule per sub-step. A disconnect below the active view's base re-extends its ids through stored ancestry; it does not reload cold entries into the arena. Below-floor reorgs remain refused. Directed 9k reorg/restart scenarios, per-sub-step window assertions and P2 passed, including full locator equality and retained snapshot comparisons.
- **Cold cache and snapshot ancestry.** Miss/read/insert and invalidation serialize under the same cache mutex; both id and hash indexes leave together. Stage 5 clears the cache when the invalidation list overflows. Range walks bypass the cache. Retained snapshot ancestry below the current window follows that snapshot's chain rather than the live active-height projection. D67 fault tests explicitly clear cached clean copies after planting corruption and passed.
- **Reopen and typed errors.** Reopen checks metadata/ids, loaded parent links, active projection and resident/cold skip target heights, recomputes best header from eligible stored candidates, and restores the lock floor. `StoreError::Corrupt` maps to `OpenError::Corrupt(Corruption::Store)`; I/O remains `Storage`. The corresponding latch/error tests passed. Restart tests cover a lost Buffered suffix, durable-prefix id preservation and possible suffix id reissue, later durable subscriber writes, and best-header recomputation.
- **The DEC-187 corrections that were claimed do hold in their fixtures.** The SPV disconnect report's duplicate is absent; the real overlay has 13-byte buckets, and the budget account now sizes its own map entry type. Its asserted exact table sizes are 213,008 bytes at 9k marks and 851,984 bytes at 50k. The production streamed-write and `SpvRig` recipe avoids a full stage-1 plan. The mark/list tests pass against the corrected allowances; R1-M1 shows that these are not the only adversarial allocations.

## Validation

All builds/tests used `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=` and `TMPDIR=/work/scratch/sb5-sol/tmp` (0700, on disk). Normal target: `/work/scratch/sb5-sol/target`; loom target: `/work/scratch/sb5-sol/target-loom`. Toolchain: rustc/cargo 1.98.1, x86-64 Linux. The explicit task TMPDIR requirement took precedence over REVIEW-COMMON's general recommendation.

| Check | Independent result | Log under `/work/scratch/sb5-sol/` |
|---|---|---|
| `cargo test -p dash-chain --all-features --lib` | 364 passed, 1 ignored; includes 64-case P2, directed, hold, reopen and cold-corruption suites | `lib-test.log` |
| `cargo test -p dash-chain --all-features --test step_budget --test feed_alloc -- --nocapture` | feed_alloc: 4 passed; step_budget: 8 passed, 3 ignored | `budget.log` |
| `cargo test -p dash-chain --all-features --test step_budget -- --ignored --nocapture --test-threads=1` | All 3 nightly allocator variants passed: 50k reorg, 50k marks, 100 stale chunks | `budget-nightly.log` |
| `RUSTFLAGS='--cfg loom' cargo test -p dash-chain --release --lib loom_ -- --test-threads=1` | 30 passed, 15 marked ignored; child-process harnesses executed the ignored negative controls and checked their intended failures | `loom.log` |
| `cargo clippy -p dash-chain --all-features --all-targets -- -D warnings` | Passed | `clippy.log` |
| `cargo fmt --all --check` | Passed | `fmt.log` |
| `cargo build -p dash-chain --features test-util --release` | Passed | `build-release.log` |
| `cargo test -p dash-chain --all-features --doc` | 9 passed | `doc-test.log` |
| Additional allocator probes against unchanged head | Both major findings reproduced, including optimized builds and result-only store isolation | `probes/*.log` |

Selected measurements: the ordinary 9k mark walk was 440,824 ≤ 705,952 bytes; 50k was 1,595,896 ≤ 2,254,240. The 9k and 50k two-branch reorgs both observed 44,848 bytes, with a 20,480-byte sub-step account. Typical residency peaks were 5,021,969 bytes at w=10,000 and 2,082,201 at w=2,048, within the corrected DEC-187 totals. The 100-stale-chunk case kept 24,605,752 bytes with 103 chunks, Nwin=2,149 and S=99. No timing failure needed an isolated rerun.

### Mutation sensitivity and additional reproduction

The existing planted mutants were actually run, not inferred from test names. Negative controls detect lost parker permits, missing post-arm rechecks, split reader calls under controls, ignoring closing cursors, blocking deregistration under holds/hub locks, cleanup without slot identity, giving a newly live subscriber an old cursor, planning before the pending drain, relocking a hold, dropping required flush service, incorrect status ordering, blocking actor ingress, and unlocking the cache around a read/insert. The slot allocator negative control detects an injected in-section allocation. D74's exact map-size assertion would reject the former hard-coded status-only accounting type.

The additional wide-tree and feed-lifecycle witnesses show important blind spots despite those passes. The existing suite accepts the current implementation with both bound violations; its ordinary mark slope and small-feed cases are not a universal allocation check. No ad hoc repository mutation was made during this read-only review.

To reproduce the additional probes after building the exact head with `--features test-util --release`, compile the preserved sources against that target. For example:

```sh
cd /work/scratch/sb5-sol/wt
export CARGO_BUILD_JOBS=8 RUSTC_WRAPPER=
export TMPDIR=/work/scratch/sb5-sol/tmp
export CARGO_TARGET_DIR=/work/scratch/sb5-sol/target
nice -n 10 cargo build -p dash-chain --features test-util --release
nice -n 10 rustc --edition=2024 -O /work/scratch/sb5-sol/probes/result-only.rs \
  --extern dash_chain=/work/scratch/sb5-sol/target/release/deps/libdash_chain-c11853a956b5f0ce.rlib \
  --extern dash_clock=/work/scratch/sb5-sol/target/release/deps/libdash_clock-32f2a469b0d4aca0.rlib \
  --extern dash_primitives=/work/scratch/sb5-sol/target/release/deps/libdash_primitives-0b1ba9d40c290a45.rlib \
  -L dependency=/work/scratch/sb5-sol/target/release/deps \
  -o /work/scratch/sb5-sol/probes/result-only
nice -n 10 /work/scratch/sb5-sol/probes/result-only
nice -n 10 rustc --edition=2024 -O /work/scratch/sb5-sol/probes/feeds64.rs \
  --extern dash_chain=/work/scratch/sb5-sol/target/release/deps/libdash_chain-c11853a956b5f0ce.rlib \
  -L dependency=/work/scratch/sb5-sol/target/release/deps \
  -o /work/scratch/sb5-sol/probes/feeds64-release
nice -n 10 /work/scratch/sb5-sol/probes/feeds64-release
```

The rlib hashes above are the verified artifacts from this review; a different toolchain/build can produce different suffixes. The small probe sources retain some unused imports from the copied allocator harness; those rustc warnings are unrelated to either result.

## Limits and integration assumptions

- Loom executes the real slot/reader/table/cache protocols through the synchronization facade. It substitutes a model mutex for the tip lock and mutex-backed publication for ArcSwap; feed exploration uses preemption bound 3. It models scheduling with Parker rather than the complete production task executor. L1/L2/L5/L9 and the additional table/hold/status/cache models therefore provide strong protocol evidence, not an exhaustive proof of the full HeaderChain/SQLite/task graph.
- SPV-LO9's actual task-local marker, actor reply checks, ingress adapter and driver belong to SB14. SB5's threaded and loom scenarios model those contracts. This review does not certify their future runtime integration. Subscriber handlers/wakers must obey the notification-only and no-wait contracts, and store read/write holders must not await the tip writer.
- Restart tests use MemHeaderStore's crash model; they are not SQLite WAL/fsync, device power-loss or mobile lifecycle experiments. The read-after-commit and leased writer-connection contracts remain SB13 integration requirements.
- P2's reference is the same Evicting implementation at w=1,000,000 and a large segment size, rather than a separate independently implemented engine. It catches window/segmentation differences, but common defects can agree on both sides. R1-m1 further narrows the recipe currently tested.
- Allocator numbers apply to the reviewed x86-64 build and the stated observation convention. The major witnesses subtract retained state and exclude store transaction/test-callback work; the result-only variant explicitly isolates returned records. They disprove the bound without needing a whole-process RSS estimate. Typical residency measurements are typical bounds, not an adversarial S=0 claim for every insertion order or retained snapshot count.
- Did not rerun the full workspace, MSRV, Miri, PF0 instruction baselines, all per-commit builds, actual dash-spv/mobile applications or the future full node driver. No dash-spv integration changes occur in the reviewed diff. The independent clippy run covers normal all-feature/all-target code; separate lib-only and loom clippy configurations remain implementer evidence.
- A standalone check confirmed deregistration closes a feed without waking its parked reader; the approved close contract promises producer and close-hook wakes, so this was not classified as a design violation. Driver cancellation must not rely on an undocumented reader wake. Unbounded repeated `on_release` registrations were also not promoted into a header-sequence finding: caller/waiter lifecycle assumptions need to be preserved by the drivers.

Both major issues should be repaired and their allocator regressions added before this gate passes. This report is intentionally uncommitted; the reviewed worktree remains at the exact requested head.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
