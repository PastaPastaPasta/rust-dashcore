# RP1a-3a review, round 1

Branch `node/rp1a-3a`, reviewed at `46b4b003c5b8c66f3862c9b9fa8f86c54bd65afd` over `d90512244..46b4b003c`.

Verdict: **ACCEPT-WITH-NITS**

## Findings

### m-1: cover the Bech32/Bech32m equal-candidate tie in `LocateErrors`

- **File:** `dashd-rpc/src/bech32.rs:331-337`
- **Core:** `bech32.cpp:565-568`
- The strict `<` is correct: when both encodings produce the same number of candidate error locations, Core keeps the first encoding (Bech32). The unit vectors and the current edge case `edge_va_two_err` do not exercise that tie. Mutant RV-1, which lets the later Bech32m result replace an equal-size Bech32 result, survives the complete 158-test unit suite and the listed edge cases.
- **Scenario:** `validateaddress("tdash1q3g3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zy6dv7vl")` should return `error_locations: [19,34]` and `error: "Invalid Bech32 checksum"`. The tie mutant reports a Bech32m error with different locations.
- **Suggested fix:** add this vector to `bech32/tests.rs::core_bech32_invalid_vectors_locate_their_errors` and add it as an edge row in `util_commands_match_dashd_v24`.

### n-1: add checksum-length precedence coverage for invalid payloads

- **File:** `dashd-rpc/src/descriptor.rs:120-133`
- **Core:** `script/descriptor.cpp:1212-1235`
- The implementation checks checksum multiplicity and length before validating payload characters, matching Core. No test combines those conditions, so mutant RV-4 (payload validation moved before checksum length) survives the unit and differential cases.
- **Scenario:** `getdescriptorinfo("pk(\x01)#abc")` should fail with code `-5` and `"Expected 8 character checksum, not 3 characters"`, before the invalid-character error. `deriveaddresses("pk(\x01)")` should retain the `"Missing checksum"` precedence.
- **Suggested fix:** add both cases to `descriptor/tests.rs::checksum_errors_are_cores` or to the 89-case differential edge matrix.

### n-2: pin the one-million-position range boundary in a unit test

- **File:** `dashd-rpc/src/util.rs:180-184`
- **Core:** `rpc/util.cpp:1150-1151`
- The `>=` boundary is correct, and the differential matrix currently kills mutant RV-2 (`>=` changed to `>`). The crate unit test only uses a range far from the boundary, so the behavior is unprotected when dashd-backed tests are unavailable.
- **Scenario:** a ranged descriptor with `[5,1000005]` must fail with code `-8` and `"Range is too large"`; `[5,1000004]` is the adjacent accepted boundary (subject to subsequent derivation).
- **Suggested fix:** add the exact boundary refusal to `util/tests.rs::deriveaddresses_counts_a_null_range_and_checks_the_range_first`.

### n-3: correct the inference reachability documentation

- **Files:** `dashd-rpc/src/descriptor.rs:822-825`; `docs/node-program/designs/RP1a-notes.md:100-103`
- **Core:** `script/descriptor.cpp:1149-1210`
- The implementation is consistent with Core, but the comments say `sh(pk(...))` and the `raw()` fallback are reachable from `createmultisig`. That RPC always constructs a multisig redeem script. With no keys or origins in the provider, the reachable forms are `sh(multi(...))`; a hybrid key makes the inner multisig inference fail and reaches the top-level `addr()` fallback. The P2PK and raw arms remain generic Core-compatible code, but are dead for this caller. The 17–20 key-count arm is likewise unreachable for `createmultisig` because those redeem scripts exceed 520 bytes and are refused first. Mutants RV-8 (dropping the 17–20 arm) and RV-9 (dropping P2PK inference) survive, as equivalent changes.
- **Scenario:** `createmultisig` with one or more valid keys always supplies a multisig redeem script; it cannot produce a P2PK or raw top-level script.
- **Suggested fix:** narrow the comments/design note to `sh(multi(...))` and the hybrid `addr()` fallback, while describing the other arms as retained generic inference behavior.

### n-4: retain an auditable list of the 13 mutation definitions

- **File:** `/work/scratch/rp1a-3a/mut.sh:5-15` and the generated `mut-out` logs
- The recorded run supports the claim of 12 killed mutants and one equivalent survivor (RV-3, the `sh()` solvability recursion). The source copy used by the runner is no longer present, so the mutation numbers cannot be reconstructed from the repository or the review artifact. This makes the claimed coverage difficult to audit or reproduce.
- **Scenario:** a future reviewer can see `mutant 3 rc=0` but cannot determine the exact source mutation without the deleted `mut` tree.
- **Suggested fix:** commit a small mutation manifest or preserve the patch/diff for each `A6MUT` case next to the run log; document RV-3 as the accepted equivalent mutation.

## Checked and ran

- Reused `/work/scratch/rp1a-3a-rv-sol` as instructed. `git rev-parse HEAD` was `46b4b003c5b8c66f3862c9b9fa8f86c54bd65afd`; `git status --short` was clean before review work.
- Read all 10 commits in `d90512244..46b4b003c`, including the EINTR retry in `46b4b003c` and its design note.
- Compared `util.rs`, `descriptor.rs`, `key_io.rs`, `bech32.rs`, `message.rs`, and their tests against the supplied Dash Core v24.0.0-rc.4 sources.
- Ran `TMPDIR=/work/scratch/rp1a-3a-rv-sol/.tmp CARGO_BUILD_JOBS=12 cargo test -p dashd-rpc --all-features`: **158 passed, 0 failed**.
- Inspected the stored mutation run: **12/13 killed**, RV-3 survived and is equivalent as documented above.
- Reviewed the 214-case differential matrix (125 baseline replies plus 89 edge cases) and the 89 validator inputs. The full differential test was not completed in this pass because its one-second close watch makes the 214-request run exceed the available review window.
