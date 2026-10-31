# BS4b r1 — Sol review

Reviewed **e8cdec55 → a723c851** (three commits), branch `node/bs4b-decode-evidence`, read-only at `/work/home/agent/workspace/rdc-wt/bs4b`. Contract: `briefs/REVIEW-COMMON.md`; requirements: PLAN §4.1, A1 §1.4 DDE #1–#10, and the BSX catalogue. PLAN §7 handling observed. The stated non-negative transaction-version scope is accepted.

**The comparisons are genuinely differential, and the reported counts reproduce. The coverage assertions and task-completion evidence are insufficient. No base-sdk decode divergence was found in the exercised inputs.**

## Findings

### 1. Major — DDE #10 lacks a meaningful field/version/class floor

**Locations:** `node-l1/tests/l1_bsdk_decode.rs:394`, `node-l1/tests/support/bsdk_rows.rs:251`, `docs/node-program/BS4B-EVIDENCE.md:110`.

The per-type floor only searches row names for `type {t} v`. It does not require each BLS field, version, zero/opposite-scheme/neither-scheme class, or its judged outcome. The generator silently skips fields without a non-zero seed representation. Consequently type 6 has DDE #10 rows only for version 1; its version 3/4 matrix rows contain null fields. Type 5's DDE #10 signature rows are only version 4. Null fields cannot establish correct handling of a non-null point under the version's scheme.

**Concrete proof:** in a scratch copy of the row builder, I retained only the first `zeros` row for each of the nine BLS types and one rejecting type-2 row. All other code and seeds stayed unchanged. The census passed both with zero mutations and with the normal **40 mutations per seed**, reporting **DDE #10: 9 accept / 1 reject** instead of 47/13. Thus deleting 50 of the 60 BLS rows, including every opposite-scheme row, does not fail the claimed floor. A regression affecting only a missing field/version/class has no required witness; random mutations are not an enforced substitute.

**Suggested fix:** give rows explicit coverage metadata and enforce successful comparisons for each required type × relevant version × BLS field × class × flag, with field-appropriate outcomes. Add non-null commitment witnesses for the basic and indexed versions instead of limiting rows to naturally non-null chain samples. This is feasible with existing points: my separate probe re-encoded the three valid fields of a Core-derived v1 commitment at versions 0–5 and tested all three classes for all three fields. **Six base cases plus 54 edited cases agreed under both flags.** Add equivalent permanent coverage and verify that deleting a required class makes the census fail.

### 2. Major — PLAN's campaign and full-history acceptance are unestablished

**Locations:** `docs/node-program/PLAN.md:465` (reviewed branch), `docs/node-program/BS4B-EVIDENCE.md:57`, `docs/node-program/BS4B-EVIDENCE.md:86`.

The PLAN row requires **each target ≥24 CPU-hours clean and BS2 full history**. The evidence supplies a finite seeded census and smoke budgets, but no per-target campaign receipt or full-history execution of the new base-sdk judges. Its 88 block samples, selected with a 60,000-byte seed limit, do not establish full-history coverage. My reproduced 400-mutation census completed in 81.31 seconds; its zero-divergence result is useful evidence, but does not establish either acceptance condition.

**Suggested fix:** record the required campaign and full-history results against the reviewed revision and pinned oracle, including target names, CPU budgets, chain ranges/counts, and divergence totals. Until those receipts exist, keep BS4b's PLAN acceptance pending; a green build/smoke gate cannot close this evidence task. If instrumentation is integrated first, distinguish that status explicitly from completion of BS4b.

### 3. Minor — the new DDE #3 rows do not establish the upper payload boundary

**Location:** `node-l1/tests/support/bsdk_rows.rs:393`.

The largest fully supplied payload is 100,000 bytes. The MAX_SIZE row supplies only a prefix and expects rejection for absent data. Both a correct 32 MiB limit and an incorrect intermediate cap can satisfy these rows. The DDE #3 transaction-prefix rows also contain no 9-byte length form, despite the evidence table's wording.

**Suggested fix:** add generated live tests with actual payloads at MAX_SIZE−1, MAX_SIZE, and MAX_SIZE+1, plus the claimed 9-byte prefix forms. These need not add giant recorded fixtures. My independent direct-base-sdk probe found agreement: 100,001; 1,000,000; 33,554,431; and 33,554,432 bytes accepted, and 33,554,433 rejected.

### 4. Minor — the census does not mutate every seed as documented

**Locations:** `node-l1/tests/l1_bsdk_decode.rs:349`, `docs/node-program/BS4B-EVIDENCE.md:59`.

When the requested mutation count exceeds 10, seeds larger than 20,000 bytes receive no mutations. One committed block seed meets this condition. Both the default and 400-mutation runs therefore exclude it from mutation, although they compare the original seed. The documented aggregate counts are correct; the claim of that mutation budget for every seed is not.

**Suggested fix:** document and report the skipped seed/mutation count, or give larger seeds a bounded non-zero mutation budget.

## Verified implementation properties

- **Independent Core side:** inspected the pinned Core shim at `a9a483e51efdadeee37cf40ff8697da62b3cd012`. Object entry points execute Core deserialization and `out << object`; the payload entry point also performs Core re-serialization while holding the process-wide BLS scheme lock. The Rust object reader splits those Core bytes directly. It does not decode Core's side with base-sdk or reuse the node's whole-object serializer.
- **BLS normalization:** lazy keys are placed by the encoding difference; global signatures use structural tail offsets. The repeated-byte signature rows passed. Patching is limited to the documented BLS fields. Lazy-key validity on use remains explicitly outside this target and belongs to the BLS vectors.
- **Pins:** existing targets retain `Pin::ALL`; the four additions select v24 only. Private replay iterates the selected target's pins, and `fuzzctl check` compares the Rust and TOML selections. Existing replay tests and registry self-tests passed. A separate type-12 replay probe confirmed the v24-only target succeeds without trying its unsupported v23 comparison.
- **Provenance/replay:** the seed generator deterministically reproduced all 277 seeds from the cited Core data. The 293 recorded rows reproduced **221 accept / 72 reject**, matched the live oracle, and passed the input SHA-256-bound replay without the oracle.
- DDE #2/#5/#7/#8/#9 are correctly assigned to the P2P work rather than these four targets.

## Independent validation

All Cargo commands used `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12`, a separate target directory, and `TMPDIR=/dev/shm/bs4b-r1-sol-tmp` in mode **700**. The private-data environment and unpinned-oracle override were unset. Logs and probe sources are under `/work/scratch/bs4b-r1-sol/`.

| Check | Result |
|---|---|
| `cargo test --locked --target-dir /work/scratch/bs4b-r1-sol/target -p node-l1 --features oracle --no-fail-fast` | 30 passed, 0 failed |
| Same target directory, `cargo test -p dash-oracle-sys --features oracle --no-fail-fast` | 62 passed, 0 failed |
| `BS4B_CENSUS_CASES=400 cargo test --release -p node-l1 --features oracle --test l1_bsdk_decode census -- --nocapture` | 110,782 inputs, 0 divergences; exact documented counts |
| `cargo test -p node-l1 --test bsdk_recorded` without oracle | 1 passed |
| `python3 -I node-l1/scripts/gen-bsdk-seeds.py <reviewed worktree> --check` | 277 seeds match |
| `python3 -I node-fuzz/fuzzctl.py check` / `selftest` | Registry passes; 51 self-tests pass |
| Scratch floor reduction, non-null quorum matrix, actual MAX_SIZE boundaries | All pass; floor reduction demonstrates finding 1 |
| `git diff --check e8cdec55 a723c851` | Pass |

The live census counts are tx **5,290/4,339**, block **16,914/17,830**, header **20,695/14,598**, and payload **13,301/17,815** (accepted/rejected). Payload inputs were judged under both flags.

The complete integration gate, private-corpus receipts, and the reported 18-leg fuzz smoke were not independently rerun. Registry leg counts and self-tests were verified. The reviewed branch was left untouched; experiments modified only scratch sources. No commits, pushes, GitHub comments, or private-data reads were made. This review file is intentionally uncommitted.

**Sol gate satisfied: no**

**Verdict: CHANGES-REQUIRED**
