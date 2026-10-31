# HX1 — Sol narrow round 4

Reviewed `b4cc23dd8878687e1a91f422b69fdc6e496e0770` on `node/sb4-core-scenarios-hx1`, over r3's `d45567569`. R3-1/R3-2 are closed on the default random path; the gate remains unsatisfied because replay admission has bypasses and the seeded heap does not reproduce across processes. This round checks unconditional up/down/seeded replay, seed reproducibility, middle-entry tie coverage and the committed allocation audit. DEC-175 and the manager's one sequence-derived seeded replay ruling govern this review. Closed r1/r2 items are only checked for regressions where this change touches them.

Evidence directory: `/work/scratch/hx1-r4-sol/`. Commands use nice 10, `CARGO_BUILD_JOBS=6`, `RUSTC_WRAPPER=` and review-owned `TMPDIR=/work/scratch/hx1-r4-sol/tmp` (0700). The reviewed branch is read-only. PLAN §7 uses codenames only: no `~/private/` access and no reading or listing the directory named by `SB4_PRIVATE_FINDINGS`.

## Evidence and findings

### Validated closures

- **R3-1 — closed on the default random path.** The actual `random_job()` running the committed precious-reset tie returns `cut` with six operations retained, before step 7, under both boundary-flush and random delivery. The original seven-operation run and the retained prefix each run under plain, up, down and one numeric seed, for eight calls total per delivery policy, despite the original empty diagnostic label. Seeds observed: `1045198257280724482` for seven operations and `793185471762991932` for six (`seed=11`, index 0). Evidence: `review-probes.log`, `precious-*-random-job.json`.
- **R3-2 — closed by mechanism and the original evidence pairs.** Both r3 mixed-channel pairs now return step 2 in both argument orders, consistent with collecting all channel differences and taking their minimum. Evidence: `review-probes.log`; the inputs are the original r3 pair JSON files.
- The rebuilt binary is `5077e63555420388c7da915eff1a7fc04c1b7cb553bbdd68271345010ce13b22`; provenance verification checks the current harness source, v24.0 pin `a9a483e51efd…` and 24 linked inputs. All **67** fuzzctl self-tests pass.

### R4 findings

**R4-1 — major: ordinary regeneration and extra delivery schedules bypass the required heap replays.** `contrib/node/sb4-core-scenarios/regen.py:709`, `:768`, `:749`.

`work()` only runs its up/down/seeded checks when `a.check` is true; the ordinary write path then writes the plain result directly. A traced real `main()` invocation with `--smoke` exits successfully after exactly five plain calls and no perturbations. A second invocation points only the scenario/output paths to review-owned scratch fixtures and otherwise runs the unchanged real harness and regeneration control flow: the committed precious-reset tie is written successfully with all seven operations (eight vector steps) after one plain call. This is the same case whose up/down comparison requires a cut before step 7. No reviewed source or committed vector was changed. DEC-175 explicitly includes regeneration in the unconditional replay requirement.

The traced `--check --only hx1-enforce-fixed-point --schedules 1` exits successfully and invokes boundary-flush under `[plain, up, down, 502587382487792090]`, but its `random:1` delivery run is invoked exactly once, with `perturb=None`. Replaying a different delivery policy does not establish heap stability for this generated schedule. The random fuzz-smoke path's `--random-delivery` does receive all default replays; the missing path is the additional schedule validator inside committed regeneration/check.

Suggested fix: share the replay/admission mechanism across random generation, ordinary vector generation/check and each delivery-schedule run. Require the same sequence and delivery policy to pass up/down and a derived seeded heap before writing or accepting it; an unstable committed scenario should fail regeneration instead of emitting one heap layout. Keep the existing semantic comparison *between* delivery schedules. Add entry-point tests that observe actual replay modes so `--check` cannot be the only gate.

Evidence: `review-probes.py`, `smoke-generation-calls.json`, `sensitive-generation-calls.json`, `check-fixed-point-calls.json`, `sensitive-generation-output/hx1-r3-precious-reset-tie.json.gz`; `review-probes.log`.

**R4-2 — major: the manager's mandatory seeded replay can be disabled.** `contrib/node/sb4-core-scenarios/regen.py:676`, `:466`, `:560`, `:592`.

The CLI only rejects negative `--perturb-replays` values. A traced real `main()` with `--random 1 --seed 3 --random-length 1 --perturb-replays 0` exits 0 and admits its vector after exactly `[plain, up, down]`; there is no seeded run. `random_job(..., replays=0)` exposes the same bypass to callers. The manager ruled one seeded replay on every sequence to exercise middle-entry choices; changing the default to 1 does not enforce that rule.

Suggested fix: require at least one seeded replay at the shared mechanism/API boundary and reject zero at the CLI. Preserve counts above one as optional additional coverage. Test both the CLI and direct `random_job()` caller so neither can disable the required replay.

Evidence: `zero-replays-calls.json`, `zero-replays.log`, `review-probe-summary.json`; the fixture uses no private signatures.

### Seed reproducibility

The Python derivation gives the same `1045198257280724482` in three independent isolated Python processes (different `PYTHONHASHSEED` settings), and the real committed `--check` boundary-flush path includes its derived numeric seed. The C++ replay itself fails repeatability, as shown below.

**R4-3 — major: a fixed seeded heap changes an observable Core state across processes.** `contrib/node/sb4-core-scenarios/harness.cpp:170` (the common allocation stream at `:157–174`), `contrib/node/sb4-core-scenarios/README.md:318–333`.

Reproduction uses only supported operations: connect A1..A3; submit B1..B3 and C1..C3 as headers off G; reload; `mark-conflicting B3`. The three tips have equal work and loaded sequence id 0. Up and down pick the two extremes and produce identical complete vectors. A middle B3 pick makes Core's `MarkConflictingBlock` move `m_best_header` to B2 (pinned `validation.cpp:4089–4090`); an extreme pick retains the harness's address-tie set. This exposes the middle choice as a state difference at step 5.

With the **same case, binary, delivery policy and numeric seed `1066088033188675652`**, four independent normal runs (**without `--audit-index`**) end with `[tie-set, B2, B2, tie-set]`. Their earliest differences against the up vector are `[None, 5, 5, None]`; the first five vector steps (setup plus the first four operations) match exactly. Seeds `1006220522186421180` and `314087182294972517` also produce both outcomes across four process repeats. Thus replay can alternately expose or miss the exact same middle-sensitive step despite the seed remaining unchanged. The Python SHA derivation is deterministic, but that does not make its resulting heap replay deterministic.

The independent pointer audit likewise changes relative orders and selected minima across **all eight** sampled numeric seeds, each with its initial run plus three repeats. Up/down retain identical relative orders and picks across their four process runs. These are relative-order changes, not ASLR base differences. The seeded allocator draws its lane from one stream shared by **every** eligible main-thread allocation. Main-thread-only access prevents another thread from drawing directly, but does not stabilize which draw reaches each index node. I have not isolated the individual unrelated allocation responsible; the unchanged production binary's state-level counterexample establishes the failure without that attribution.

Suggested fix: make placement of the relevant index pointers a deterministic function of the seed and reproducible index-allocation identity/order, or otherwise stabilize the allocation stream those nodes consume. Add an independent-process regression using this three-way follow-up that checks the same seed gives the same vector and replay difference/cut boundary. Keep seeded replay mandatory; adding more probabilistic samples does not repair reproducibility.

Evidence: `three-way-mark-middle.json`, `middle-semantic.py`, `middle-semantic-summary.json`, `middle-semantic-1066088033188675652-{0,1,2,3}-plain-output.stdout`; `repeat-heap.py`, `heap-repeat-summary.json`. Reproduce with `TMPDIR=/work/scratch/hx1-r4-sol/tmp CARGO_BUILD_JOBS=6 RUSTC_WRAPPER= nice -n 10 python3 -I -B /work/scratch/hx1-r4-sol/middle-semantic.py`. Its summary preserves all repetitions; the nondeterministic choices need not recur in exactly the same order.

### Allocation audit

The committed `allocation-audit.py --bin /work/scratch/hx1-r4-sol/out` passes without a custom logging build: **552 + 168 + 140 = 860** index snapshots are inside the perturbation region, and **1,050 + 159 + 69 = 1,278** observed pairs reverse between up/down. These exactly reproduce r3's totals for dump/reload, the in-batch failed walk and the live precious tie. Sizes remain `CBlockIndex=144`, `BlockMap node=192`, alignment 8. This verifies the pinned supported paths; the README correctly requires a re-pin to rerun the audit and inspect new pointer-ordering sites. Evidence: `allocation-audit.log`.

### Focused regeneration

`--check` passes five byte-exact committed vectors (`hx1-dump-orders`, `hx1-enforce-fixed-point`, `hx1-failed-walk-early-reject`, `hx1-pnb-force`, `hx1-precious`) under their boundary-flush up/down/seeded checks, both address-sensitive regressions at their required steps, and two fixed-point delivery schedules. These checks preserve the touched r1/r2 closures; no closed finding is reopened. Evidence: `targeted-regen.log`.

### README and middle-entry measurement

The README's **“can pick a middle one”** wording is accurate. A cheap three-way reload under 64 SHA-derived numeric seeds chooses the up/down middle label B3 **21/64 times (32.8%)**, with the other minima C3 23 times and A3 20 times. The pick is inferred from the audited addresses and pinned Core's minimum-address comparator (`blockstorage.cpp:43–45`, `validation.cpp:5359`). The reload-only visible vectors all match because the tie is represented as a set; the supported mark-conflicting follow-up above exposes the selected middle entry in the vector.

**Chance, not guarantee, is acceptable under DEC-175 plus the manager's one-seeded-replay ruling.** Neither requires exhausting all permutations of a three-way tie. The issue is R4-3's failure to reproduce the *same* seeded experiment, independently of how much middle-entry coverage one experiment supplies. Evidence: `middle-summary.json`, `three-way-reload.json` and the saved `three-way-*.stderr` audits.

### Remaining validation and limits

The Rust vector/provenance and records-completeness checks pass. The test process reports three passes; its optional smoke regeneration test returns early because `SB4_CORE_SCENARIOS_BIN` was unset, and actual C++ regeneration is covered by the focused runs above. `fuzzctl_test.py` reports 67 passes. The full 52-exact-plus-two-tolerance regeneration and the implementer's 1,000/300-sequence smokes were not independently rerun in this narrow round. An optional 100-sequence random-delivery smoke was stopped during generation after disk contention slowed it; no pass/fail result is claimed for that run. Its exact owned process group was terminated, without cmdline-pattern kills.

The branch remains unchanged, with no commits, pushes or GitHub comments. No private directory was opened or listed. Only the optional smoke runner uses the private-signature environment setting; focused classification fixtures use neutral/no signatures. The report is uncommitted on integration.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
