**Verdict: CHANGES-REQUIRED** — Sol r3, HX1 at `d4556756906a55cc2aac42796e7a639e889e9bd0`, over r2's `6fea28c88`. R2-1 and R2-2 are closed. The perturbation reaches the relevant pointers at this pin, but the pre-filter misses a live candidate tie.

## Per-finding closure

- **R2-1 — closed for the reported batch.** Reused `/work/scratch/hx1-r2-sol/walk-new-parent-batch.json` with the freshly built target. It is marked `failed_blocks at step 4`. Its first difference under perturbed replay is step 5: choosing the earlier failed ancestor additionally changes B2 and writes its record. The actual `regen.random_job()` returns `cut` with exactly the first **four** operations retained. The committed `address-sensitive/hx1-r2-walk-new-parent-batch.json` also passes the regeneration regression. Evidence: `r2-closure-summary.json`, `walk-new-parent-batch-random-job.json`, and `r2-closures.log` in the r3 scratch directory.
- **R2-2 — closed.** `dash-oracle-sys/tests/sb4_core_scenarios.rs:575` selects the header result by operation shape and requires `records_written` for both direct `pnbh` and `build {submit: headers}`, including a missing result. The new self-test passes. Repeating the original mutation against current vectors—deleting all 25 records from direct `pnbh` step 5 in `a18-invalid-chain-found`—makes the freshly compiled actual Rust gate fail with `step 5 without records_written`. Evidence: `reproduce-record-gate.py`, `records-gate-mutations.json`, `records-gate-probe.log`, and `records-closure.log`.
- **R1-3 — remains closed; no over-cut reproduced.** The original `/work/scratch/hx1-r1-sol/early-reject.json` is marked but stable under replay and remains complete with outcome `ok`. The committed pow-fail vector regenerates exactly. The r2 time, version, difficulty, cached-invalid batch and direct-PNB early-rejection cases also remain uncut through `random_job()`. A valid failed walk still differs and is eligible for cutting. R1-1, R1-2 and R1-4 are not reopened by this narrow review; the complete-vector gate, supplementary actual-function probes and fuzzctl self-tests pass.

## New R3 findings

**R3-1 — major: live negative-sequence ties bypass perturbed replay.** `contrib/node/sb4-core-scenarios/harness.cpp:1141`, `:1146`, and `contrib/node/sb4-core-scenarios/regen.py:575`.

Reproduction, using ordinary supported operations:

1. Build/connect A1..A3.
2. `precious A3` assigns A3 sequence id -1.
3. `invalidate A3` returns the tip to A2; there is only one failed block.
4. `precious A2` lowers the remembered precious chainwork.
5. Build/connect B2..B3 from A1.
6. `precious B3` resets Core's reverse sequence counter and assigns B3 id -1 too.
7. `reconsider A3` lets the equal-work, equal-sequence blocks compete by pointer address.

No reload occurs. A3 and B3 both have `(nSequenceId, nChainTx) = (-1, 4)`. The unmodified target ends at **A3 under `up` and B3 under `down`**, first differing at step 7; **both runs emit `possibly_address_sensitive: []`**. Giving this scenario to the actual `random_job()` by replacing only its scenario generator returns `ok` with all seven operations, without a perturbed replay. Core's counter reset is at pinned `validation.cpp:3868–3878`; the address fallback is at `node/blockstorage.cpp:43–45`. Checking only failed-set size and ties after a reload is therefore not conservative.

Suggested fix: mark live work/sequence ties as well, including negative ids, at boundaries that cover their decisions; a conservative mark on precious operations or replaying every sequence would also cover this case. Keep the mark as a replay trigger. Add the sequence as an address-sensitive regression and assert a cut before step 7.

Evidence: `/work/scratch/hx1-r3-sol/precious-reset-tie.json`, `precious-{plain,up,down,17,29}.stdout`, `precious-random-job.json`, and `precious-tie.log`. Reproduce with:

```sh
TMPDIR=/work/scratch/hx1-r3-sol/tmp RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  nice -n 10 python3 -I -B /work/scratch/hx1-r3-sol/reproduce-precious-tie.py
```

**R3-2 — minor: first-difference attribution prioritizes fields over the earliest step.** `contrib/node/sb4-core-scenarios/regen.py:469–480`.

A helper-level reproduction changes a transcript verdict at step 2 and a step result at step 5: `first_difference()` returns **5**. A second pair changes an acceptance verdict at step 2 and a transcript entry at step 4: it returns **4**. Each pair's first difference is step 2. Early returns from the step-view and transcript loops prevent comparing the earliest steps across all channels. This gives the initial cut a later boundary than DEC-172 requires. Iterative prefix replay can recover an earlier persistent difference; this reproduction establishes the attribution defect rather than a separate stored-vector escape.

Suggested fix: collect the first differing step from each channel and return their minimum. Add mixed-channel tests. Evidence: `earlier-transcript-later-view.json`, `earlier-accept-later-transcript.json`, and `first-difference.log`. Reproduce with:

```sh
TMPDIR=/work/scratch/hx1-r3-sol/tmp RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  nice -n 10 python3 -I -B /work/scratch/hx1-r3-sol/reproduce-first-difference.py
```

## Perturbation coverage and validation

The replacement does serve only main-thread allocations of at most 1 KiB. At this pinned build, however, the pointers ordered by `m_failed_blocks`, candidate ties and `m_blocks_unlinked` name `CBlockIndex` objects inside individually allocated `BlockMap` nodes. Those nodes are **192 bytes, alignment 8**; `CBlockIndex` itself is 144 bytes. Both insertion paths (`AddToBlockIndex`, `InsertBlockIndex`) execute on the main thread in the harness's setup, submissions and reloads. Allocation of a container's own nodes on another thread does not change the ordering of its stored index pointers. Larger bucket arrays do not contain the index objects.

A scratch-only logging probe against unchanged Core archives checked the dump/reload scenario, the r2 batch and the live precious tie under up, down and two seeds: **all 860 observed index-pointer snapshots were in the perturbation region**, and up/down reversed **all 1,278 observed pairs**. No relevant size, alignment or thread escape was reproduced for these supported paths. This establishes coverage for the current pin and harness operations, not a universal allocator replacement. Evidence: `allocation-summary.json`, `allocation-audit.log`, and `allocation-audit-out/harness-allocation-audit.cpp`.

The fresh target binary matches committed provenance byte-for-byte (`47d7b9a1f414a12b…`); build verification checked the source, pin and **24 linked inputs**. The Rust schema/provenance and records self-tests pass, as do three supplementary actual-function probes and **66 fuzzctl self-tests**. Targeted `regen.py --check` passes **seven exact committed vectors**, their prescribed heap replays, the address-sensitive batch regression and **eight fixed-point delivery schedules**. Seed 2's **300-sequence random-delivery smoke passes generation and replay**: `ok=295 cut=5 known=0 fail=0 differ=0`. Seed 1's 1,000-sequence smoke, full real-header/benchmark regeneration and the full seam proof were not rerun in this narrow round.

All new build products, probes, reports and TMPDIRs are under `/work/scratch/hx1-r3-sol/`, with nice 10, `RUSTC_WRAPPER=` and `CARGO_BUILD_JOBS=6`. The reviewed worktree remains unchanged; no commits or pushes were made. No private files were opened or listed by the reviewer; the requested `SB4_PRIVATE_FINDINGS` setting was supplied only to the smoke runner.

**The Sol gate is not satisfied.**
