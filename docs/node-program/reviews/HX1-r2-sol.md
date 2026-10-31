**Verdict: CHANGES-REQUIRED** — Sol r2, HX1 at `6fea28c88cad908cc8e8246f85533ff497d5cb5a`, reviewing the fix round over `ec0b9510a`.

## Per-finding closure

- **R1-1 — closed.** An op naming `brief` now exits with `` `brief` is not supported ``. Every scenario step retains its trace, status changes and X3 entries; header results retain `records_written`. The three formerly abbreviated steps have all 1,150, 1,150 and 34,200 entry changes. The fixed-point vector has all 21 trace events, and changing an event in its exact prefix changes `semantic()`. The gate recursively rejects `*_count` throughout each step, including nested objects and arrays; vector-level X1 counters remain outside that check. Scratch tests exercised the actual Rust functions. Gzip byte equality holds on agentbox's Python 3.14/zlib 1.3.1. Python 3.12/zlib 1.3.2 produces equal JSON with different gzip bytes and is rejected explicitly, as the README documents; cross-runtime gzip identity is not guaranteed.
- **R1-2 — closed.** The original duplicate of stored A2 still returns `ok=true, new_block=false`, but the actual Rust predicate rejects it because A2 already has `BLOCK_HAVE_DATA`. Scanning every committed scenario with `x2_outcomes()` finds exactly one qualifying vector, `hx1-pnb-force`; B9 goes absent → 2 after the unforced receipt → 11 after the forced receipt. The successful unforced result and before/after storage checks exclude rejection at another check.
- **R1-3 — closed for the reported early rejection.** The committed `hx1-failed-walk-early-reject` and the original reproduction now return Core's `invalid-header/high-hash`, with empty `records_written`. Additional pre-walk failures (`time-too-old`, `bad-version`, `bad-diffbits`, and an earlier cached-invalid header in a batch) keep their deterministic verdicts. A valid single header under B1 and B2 still cuts after the call. **The new batch gap below prevents accepting the overall refusal implementation.**
- **R1-4 — closed.** Empty, whitespace-only and kindless signatures are rejected. A signature on an earlier stderr line no longer makes an unrelated terminal failure `known`; the matching terminal line must also have the registered failure kind. Synthetic tests verify that only a terminal error refusal cuts, while an earlier refusal line or an abort does not.

## New R2 findings

**R2-1 — major: a new parent accepted earlier in a header batch can reach the address-ordered failed walk uncut.** `contrib/node/sb4-core-scenarios/harness.cpp:1324` and `:1326` (also the assumption at `:1308` and early return at `:1346`).

Reproduction: connect B1..B3, switch the active chain to A1..A5, invalidate B1 and B2 off the active chain, then build C4..C5 on B3 with `submit: headers`. B3 remains script-valid. Core therefore accepts C4 without scanning B3's failed ancestors; C4 is only header-valid, so C5's acceptance scans them and picks B1 or B2 by address. `FailedWalks()` skips C4 because B3 is script-valid and skips C5 because C4 is not indexed before the batch. Its empty result disables the post-call refusal. The unmodified target exits successfully and emits an `invalid-prev/bad-prevblk` result with C4 as `last_accepted`, instead of cutting step 5.

The address choice changes the oracle state: a scratch-only selector limits the failed set to B1 or B2 during that Core call, restores the full set immediately afterwards, and disables the refusal. Both choices reject C5; choosing B1 additionally changes B2 from 61 to 125 and writes B2's record, while choosing B2 leaves B2 at 61 and omits that record. Both mark B3 and C4. This demonstrates the two possible first choices of the unchanged pinned Core walk, rather than relying on a chance allocator layout.

Suggested fix: account for parents created within the batch when deriving failed ancestry, or inspect each header at its actual acceptance boundary. Keep the post-call check that preserves deterministic early rejections. Add this batch as a regression with an asserted cut.

Evidence: `/work/scratch/hx1-r2-sol/walk-new-parent-batch.json`, `new-parent-batch.stdout`, `selector-summary.json`, `walk-probe-out/harness-walk-probe.cpp`, and `r2-repro.log`. Run `TMPDIR=/work/scratch/hx1-r2-sol/tmp nice -n 10 python3 -I -B /work/scratch/hx1-r2-sol/reproduce-r2.py` after the recorded builds. The adapted r1 reproductions are in `reproduce-predicates.py`; Rust predicate probes are in `gate-probes.rs` and `gate-probes.log`.

**R2-2 — minor: the completeness gate skips `records_written` for direct `pnbh` steps.** `dash-oracle-sys/tests/sb4_core_scenarios.rs:637`.

`build {submit: headers}` nests its header result at `result.pnbh`, but a direct `pnbh` op returns that object at `result` itself (`harness.cpp:1835`). The new completeness assertion only visits the nested shape. In a scratch copy of the committed vector directory, removing all 25 entries by deleting `result.records_written` from step 5 of `a18-invalid-chain-found` still lets the complete `vectors_match_their_inputs_and_pin` test pass. This loses the guard promised by R1-1 without needing any `*_count` replacement. The current harness still emits the field; this is a regression-gate gap.

Suggested fix: select the header-result object by op shape, as `header_verdicts()` already does, and require `records_written` for both forms. Add a missing-field mutation test for the direct form. Reproduction: `reproduce-record-gate.py`, `records-gate-mutations.json`, and `records-gate-probe.log` in the review scratch directory; the copied Rust gate changes only `vectors_dir()` to point at those scratch vectors.

## Validation

The fresh C++ binary matches the recorded target binary byte-for-byte, and the seam proof passed against 23 pinned archives. The Rust schema/provenance gate and three additional actual-function predicate tests passed; fuzzctl self-tests passed (60). Targeted `regen.py --check` passed for the two devnet window cases, the anchored header-rules case, the fixed-point case, the new early-rejection case and the PNB force case: **6 byte-exact JSON/gzip vectors**, plus **8 matching delivery schedules** for the fixed-point case. All **54** committed vectors recompress byte-identically with agentbox's runtime. The separate Python 3.12/zlib 1.3.2 probe retained identical JSON but correctly failed gzip byte equality.

Seed 2 random-delivery smoke passed: `ok=284 cut=16 known=0 fail=0 differ=0` (300 sequences, generation and replay). Seed 1 boundary-flush smoke passed: `ok=942 cut=51 known=7 fail=0 differ=0` (1,000 sequences, generation and replay). Both cut counts are unchanged from r1. The optional full real-header/benchmark regeneration was stopped to keep this round focused; those jobs were not revalidated in full.

All build products and TMPDIRs used new review-owned directories under `/work/scratch/hx1-r2-sol/`, at nice 10 with `RUSTC_WRAPPER=` and `CARGO_BUILD_JOBS=6`. The branch is unchanged. No private files were opened or listed by the reviewer; the requested `SB4_PRIVATE_FINDINGS` setting was passed only to the smoke runner.

**The Sol gate is not satisfied.**
