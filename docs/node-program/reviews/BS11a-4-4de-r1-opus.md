# BS11a-4 slices 4d + 4e, independent review r1 (Opus)

Reviewer: Opus, 2026-10-26. Review only; no branch changed.

- rdc `node/bs11a-4`, `5840956e4..a22b894d8` (7 commits: 96048e58d, 834a4582e, e84916e03, 0c6b13b09, 7af827713,
  a22b894d8). The worktree head is now 57d9db3fa (two later docs-only commits, quirk 45 stamp; not reviewed).
- base-sdk fork `node/bsdk-bs11a-4` 35b1e571 (parent 84917742): `contrib/vectors/bls_oracle/check_upstream.py`,
  README, `oracle.py` (`must(flag=)`), `pkgs/pkc/corpus/PROVENANCE.md`.

Verdict: **ACCEPT WITH NITS**. There are no Majors. Three Minors (one test-coverage gap in the new leg, two on the
checker's bookkeeping against upstream) and five nits. The legs and the checker are correct, and every
classification the checker makes is honest and documented.

## What I ran (scratch `/work/scratch/bs11a4-4de-review`, tree = `git archive a22b894d8`, seeded target)

rdc:
- `l1_bls_verify`, `l1_bls_aggregate` and `l1_bls_threshold` at a22b894d8 all pass (5/5 each).
- The `bls_wrapper_seeds` example regenerates all four seed directories byte for byte (verify 22, aggregate 24,
  threshold 20, ies 16).
- `fuzzctl check` is ok (113 targets).
- Probe test (scratch only): it replayed every `diff_bls_verify` seed through the oracle and read Core's call log.
  - Every legacy verification over a point with no usable element, past the argument checks, reaches relic's pairing
    (`pp_map_sim_oatep_k12`). That covers all 20 VERIFY, 10 VERIFY_AGG and 15 VERIFY_SEC such records, so the
    `legacy_unusable_reached` assertions are met by records that really pair.
  - `OK` is reached by each op under each flag (VERIFY 11/11, VERIFY_AGG 5/7, VERIFY_SEC 6/6, basic/legacy).
- Random scripts: 140 of the test's 300 contain a verification, 353 verify records in all, so `random_scripts_agree`
  is not hollow.
- I checked the `VERIFY_AGG` model against the shim at 77dbec824 (`oracle.cpp:3165-3177`) and `bls.cpp:314-319`:
  an invalid signature gives false, then empty or unequal lists give `ORACLE_E_ARG`. The order matches.
- Env-gated mutants in a copy of `bls_wrapper.rs`, run against `l1_bls_verify`:
  - M1: `E_ARG` before the invalid-signature check. **Killed** (3 tests fail).
  - M2: `VERIFY_AGG` always uses the basic scheme. **Killed**.
  - M3: the unequal-length `E_ARG` check removed. **Killed**.
  - M4: `VERIFY_AGG` reads its hash count from the key count, ignoring bit 6. **Survives** (Minor 1).
  - M7: a legacy `VERIFY_AGG` `Valid` mapped to `REJECT`. **Killed**.
  - M9: `verify_agg` drops its `note()`. **Killed** by `public_seeds_reach_every_op`.

base-sdk:
- `check_upstream.py` against a probe at Core 77dbec824 (ABI 28) exits 0 with the implementer's counts: 732 values,
  255 reproduced (18 by glue), 30 verified, 30 not reachable, 417 inputs, 0 not reproduced.
- The ten files' sha256s match PROVENANCE.
- Corpus mutation harness (one value per run, 52 runs):
  - Every reproduced, verified, glue and input value I mutated was caught (exit 1). That covers each file, including
    the DKG shares and the operator signatures (verified), the IVs (glue) and the IES layout and length prefixes.
  - The survivors are all documented or consistent by construction:
    - `bls_hash` `tNN_hash`/`tNN_fp`: not reachable.
    - The filler bytes of the 252/253-byte IES images: not reachable, filler only.
    - Blob bytes of a 16-aligned IES image: PROVENANCE says the filler survives the round trip.
    - `bls_threshold` `recover_ids[0]` moved to another valid id: a different t-subset recovers the same signature,
      so the mutated file is still a true statement.
- Witnesses: no operator key in `bls_llmq_100` has a secret key anywhere in the corpus, and every `bls_aggregate` and
  `bls_secure_aggregate` key has one. So "verified" is used exactly where randomness is missing, and the
  verified-fallback branches of `check_aggregate` and `check_secure` are unused today.
- Upstream's `bls_dh.json5` (origin/develop) fails on exactly three `chia.ies` ciphertexts and their three IVs, as
  D13 says.

## Findings

### Minor

**m1. The coverage test does not pin `VERIFY_AGG`'s unequal-length path (`node-l1/tests/l1_bls_verify.rs:52`,
`node-l1/src/bls_wrapper.rs:577`).**

- Mutant M4 replaces `second_len(sig, n, at(1 + n))` with `n - 1`, so `VERIFY_AGG` never reads its own hash count.
  It passes all four tests.
- The scripts stay self-consistent, so they still agree with Core. `op_reaches(..., [ORACLE_REJECT, ORACLE_E_ARG])`
  is still met through the empty-list seeds.
- So a regression that makes unequal key/hash lists unreachable, both for the seeds and for the fuzzer's bit 6, goes
  unnoticed. D10 claims those seeds reach "unequal and empty lists".
- Fix, either of:
  - Assert a `VERIFY_AGG` `ORACLE_E_ARG` record with a non-empty key list. Add an operand fact to `Hit` (e.g.
    `unequal`), or decode the counts from `read(&VERIFY_OPS, seed).code`.
  - Add a unit test that the unequal seed parses to differing key and hash counts.

**m2. The checker and PROVENANCE are pinned to a pre-develop copy of three files, and this is not recorded
(`pkgs/pkc/corpus/PROVENANCE.md:5`, `check_upstream.py:874`).**

- upstream `origin/develop` 7f6b0ea6 (2026-10-14, "add known-answer vectors for message signing") adds new sections:
  - `sign_message` to `bls_sign.json5`;
  - `verify_message` to `bls_aggregate.json5` and `bls_secure_aggregate.json5`.
  These are the pre-1.0 Chia "Test vectors 1".
- Running the checker on develop's copies aborts all three files: "no check for 15 / 28 / 6 values", exit 1. That is
  loud, which is good.
- But D13 ("the ten files … upstream's own corpus") and PROVENANCE describe the fork's copies as if they were
  upstream's current ones. The next upstream sync will break the checker and invalidate three counts and sha256s.
- Fix:
  - Name the base the check covers. Note the develop sections, which are raw-message signing (Core signs a uint256,
    so some may be "not reachable").
  - File a backlog row to extend the checker when the fork takes 7f6b0ea6.

**m3. Nothing enforces the ten new PROVENANCE sha256 lines.**

- `corpus_has_provenance` (`tests/bls_dashbls.rs:277-288`) checks only `bls_dashbls.json5`.
- `every_file_has_provenance` (`tests/bls_oracle.rs:1573`) checks only `corpus/bls_oracle/`.
- D13's rule is that the files' bytes are kept as they are. Without a test, a sync or a hand edit changes them while
  PROVENANCE's hashes and counts go silently stale (see m2).
- Fix: extend `corpus_has_provenance` to the ten names. Mirror the `bls_oracle` test's list check so a new upstream
  file without a section also fails.

### Nits

- **n1** (`node-l1/tests/support/bls_wrapper.rs:133-141`):
  - `legacy_unusable_reached` documents "which it pairs on relic's engine". But a record with an invalid signature
    and an unusable key in its list also meets the predicate, and it never pairs: Core answers false first.
  - Today every matching record does pair (I checked the call logs).
  - Add `&& !h.operands.invalid` so the predicate implies what the doc says.
- **n2** (`check_upstream.py:928`):
  - The checker accepts any ABI ≥ 28 and only prints Core's version.
  - PROVENANCE's counts are bound to Core 77dbec824.
  - Warn (or fail without `--any-core`) when `probe.core_version` is not the pinned one, so a later run can't
    re-certify against another Core without saying so.
- **n3** (`README.md:69-70`, BS11a-4 notes D13): "glue Core runs outside its BLS library". The checker runs Python's
  copy of that glue (`o.sha256d`, `o.vec`, `dkg.commitment_hash`), not Core. "The checker's copy of glue Core runs
  …" would be exact. PROVENANCE's own "glue over Core's outputs" is fine.
- **n4** (`docs/node-program/designs/LQ2-notes.md:64`, D11):
  - The sentence now reads "It must pass the same twin, …, `a_single_member_check_without_a_verdict` (deleted at
    BS11a-4 4c, D2) unchanged". That asks a deleted test to pass.
  - Drop it from the list and add a separate clause saying it was deleted at 4c.
- **n5** (BS11a-4 notes D10, slice table row 4d): the slice's gate column (smoke 60 s, farm) is still open, waiting
  on DEC-492.
  - I ran `fuzzctl check` (ok) but not a libFuzzer build of `diff_bls_verify`.
  - Keep 4d out of "done" until the smoke has run at least once on the fuzz build. The `l1_target!` stub is trivial,
    but the leg has never executed under the fuzz harness.

## Docs vs code

- D10 "Landed (4d)" matches the code: `VERIFY_OPS` (19 entries, the six own ops weighted double), the `VERIFY_AGG`
  model, the removed skips, `Unsupported` as a divergence, `min_certify_execs` 200000, private route, 22 seeds.
- D12 (4d) matches `l1_bls_verify`.
- The seeds' PROVENANCE section matches `verify_seeds`.
- D13 (4e) matches the checker output in every count.
- The D8 citation `elem.rs:345-373` covers `from_impl` and `from_point` at 84917742.
- `commitment.cpp:90` is `VerifySecureAggregated` at rc.3.
- The `bls_arith.rs` doc changes match the removal of `skip`/`skipped`.
