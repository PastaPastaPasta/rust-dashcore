# BS11b round 1 review

## What I reviewed

- Design notes D1–D31 in `rust-dashcore` (`docs/node-program/designs/BS11b-notes.md`).
- DEC-449 and DEC-463 in `~/workspace/node-program/DECISIONS.md`, including the rulings for D20, D24 and D26.
- The base-sdk fork worktree `~/workspace/bsdk-wt/bs11b`, branch `node/bsdk-bs11b`, range `569f4ad..4a6a9e4` (the six BS11b commits), with the review focused on `pkgs/pkc` and its BLS core implementation and tests.
- The rust-dashcore worktree `~/workspace/rdc-wt/bs11b`, branch `node/bs11b`, range `node/integration..de1ef98cd` (the ten BS11b commits), including the fork pin, `dash-evo` BLS benchmark and workload rows, the three `node-l1` differential fuzz legs and their comparison logic, and the oracle rows and provenance.
- Dash Core `v24.0.0-rc.3` source using `git show`, covering `src/bls/*`, `src/llmq/*`, and `src/test/bls_tests.cpp`, with the relevant wrapper, threshold, IES, batch-verifier and benchmark registrations.

The review checked typed-API behavior against Core for validity and scheme flags, legacy/basic verification behavior, secure aggregation ordering and weights, threshold share derivation/recovery/verification, IES encoding and length handling, DH, and `CBLSBatchVerifier` fallback behavior. I also checked mutation-sensitive tests, oracle provenance, the three fuzz legs' comparison and skip rules, and benchmark names. D21's invalid share inside an aggregated batch, fuzz corpora, and `/work/scratch/bs11b` were out of scope as requested.

## Validation run

- `cargo test -p dash-pkc` in `~/workspace/bsdk-wt/bs11b` — passed.
- `cargo test -p dash-evo` in `~/workspace/rdc-wt/bs11b` — passed.

Both runs used `TMPDIR=/work/scratch/bs11b-sol-r1`, a per-worktree `CARGO_TARGET_DIR` under that directory, and `CARGO_BUILD_JOBS=8`. No fuzzers, gates, CodeQL, combs or dashd were run.

## Findings

None. There are no Blocker, Major, Minor or Nit findings requiring a file/line fix.

## Checks with no finding

1. **Typed API and scheme behavior.** Wrapper validity checks, invalid and zero/infinity values, legacy and basic scheme flags, basic distinct-message rejection, and the legacy path's relic/unsupported handling agree with the cited Core code.
2. **Secure aggregation.** Keys are serialized under the selected flag, sorted in Core's order, and weighted with the same secure-aggregation construction. The implementation preserves Core's handling of invalid/exotic operands and repeated equal serialized keys. The D8 equal-key instability is explicitly documented and excluded by the design.
3. **Threshold operations.** Horner share evaluation uses Core's order; IDs reduce modulo the group order; zero IDs, repeated IDs modulo the order, unequal spans, invalid shares, and too-small recovery sets have the expected results; successful recovery and receiver-preservation behavior match Core. Share verification and public-key-share construction follow the same vector semantics.
4. **IES.** Multi-recipient encryption/decryption, partial containers, empty plaintexts, secret-key decrypt, length prefixes, truncation, and the `MAX_SIZE` cap match Core's read/length behavior. The IES key and DH derivations use the expected basic serialization and 32-byte truncation.
5. **Batch verifier.** Whole-batch, per-source and per-message fallback ordering, source/message bookkeeping, duplicate message IDs, sub-batch clearing, secure/insecure grouping and `Unsupported` propagation match `CBLSBatchVerifier` for inputs Core accepts. The claimed surviving mutation changing `||` to `&&` in `batch.rs::fall_back` is behaviorally equivalent: it can only cause a deterministic invalid/unsupported message already present in one duplicate-tracking collection to be checked again, and the externally visible result is the same set and verdict.
6. **Tests.** The added tests exercise valid and invalid paths, scheme variants, edge values, unsupported operands, fallback paths and receiver-preservation cases; they are not tautologies and the reviewed mutant coverage is meaningful.
7. **Oracle rows and provenance.** The new `ops.json5` and `quirk22.json5` rows identify the Core oracle source, generator/probe and hashes through the checked-in provenance material. D26's composition from existing operations does not require an ABI bump.
8. **Fuzz legs.** `diff_bls_aggregate`, `diff_bls_threshold` and `diff_bls_ies` use the shared script/comparison machinery, compare values and verdicts in the intended order, and skip only the legacy exotic verification cases that `dash-pkc` deliberately reports as unsupported. The D24 weighting decision is reflected without weakening comparison correctness.
9. **Bench rows.** The nine `dash-evo` `bls_core` benchmark names correspond to Core's `bls.cpp` and `bls_dkg.cpp` registrations, including the large-block, batched-verification and DKG contribution/share-vector rows.
10. **Integration and pin.** The rust-dashcore pin resolves to the reviewed fork head, and the surrounding node-l1 wiring exposes the reviewed operations without introducing a separate behavioral implementation.

## Verdict

**ACCEPT**
