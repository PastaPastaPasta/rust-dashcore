# AB-O5, arm o — Sol review

Verdict: **CHANGES-REQUIRED**

Reviewed `node/ab-o5-o` at `8ce4a4af5b7ad684cb99349c3bf0e26aaa1952b5`, against `AB-REVIEW.md`, `AB-O5.md`, PLAN §4 O5, R4 §1.15, A1 §1.4.1/§4.1, and the applicable decisions. Review used a detached worktree; no implementer worktree was edited, and no other calibration arm was reviewed.

## Findings

1. **major — `contrib/node/check-quirks.py:64–95`: source text is counted as a test even when Rust does not compile it.** The parser has no block-comment state and does not carry file/module configuration into its function records. In a separate source-copy fixture, wrapping the entire `#[test] fn quirk_04_checkdatasig` item in `/* ... */` leaves `check-quirks.py --root <fixture> --core ~/workspace/dash` exiting 0 with `42 quirks, 42 tests: 12 tested, 30 ignored`. Prepending `#![cfg(any())]` to `dash-consensus/tests/quirks.rs` also produces that same successful summary, although it removes quirks 4, 5 and 7 from the test binary. A minimal `rustc --test` reproduction with that file attribute lists `0 tests, 0 benchmarks`; a function inside `#[cfg(any())] mod hidden` is likewise classified as unconditional by `parse()`. These are ordinary ways a test can disappear while the central acceptance check stays green. Strip Rust comments correctly and account for enclosing/file attributes and actual test-target inclusion, or compare against Cargo's compiled test inventory with the required features. Add selftests for block comments and file/module configuration; the current direct-function `#[cfg]` selftest covers only one of these cases.

2. **minor — `docs/node-program/quirks.toml:366`: quirk 37's title overstates when a different serialization scheme produces zeros.** The title says lazy BLS objects serialize as zeros “under a scheme other than the buffered one” without the necessary unresolved-object condition. At the v24 pin, `src/bls/bls.h:444–456` serializes `obj.ToBytes(specificLegacyScheme)` on a scheme change. If `Get()` previously succeeded, `obj` is initialized and this re-encodes the valid object; it does not zero it. The pinned base-SDK test `explicit_scheme_serialization_trap` in `pkgs/pkc/tests/bls_wrapper.rs` explicitly asserts both cases: zeros before `get`, valid re-encoding after successful `get`. Qualify the title with “before a successful Get()” for the scheme-switch case, or limit it to the failed-Get behavior that A1 names.

3. **minor — `conformance/tests/quirks.rs:83–89,165–171` / `docs/node-program/quirks.toml:228,342`: scaffolds 22 and 34 are outside their existing owning pure crates.** AB-O5 explicitly says to put the test in the owning pure crate where it exists and calls out `dash-params` via base-sdk as existing today. The pinned fork has `pkgs/params/src/` and `pkgs/params/tests/`, including the superblock checkpoint in `src/mainnet.rs`; the new conformance module's opening comment incorrectly groups `dash-params` with crates that do not yet exist. Quirk 22 likewise lives in conformance even though PLAN assigns BS11b to the existing base-SDK `dash-pkc` crate, whose tests the register already references for quirks 35 and 37. Move these ignored scaffolds into the pinned base SDK's owning tests and register their `base-sdk:` references, or obtain an explicit exception to this placement requirement. The owners and ignore reasons themselves are correct.

4. **minor — `docs/node-program/quirks.toml:198` / `conformance/tests/quirks.rs:58–60`: quirk 19 cites the P2P rejection path for null commitments instead of the consensus acceptance path.** `src/llmq/blockprocessor.cpp:124` at `v24.0.0-rc.3` rejects a null P2P commitment with a misbehavior error. It does not establish the registered “null commitments of disabled types are accepted” behavior. The relevant transaction acceptance path is `CheckLLMQCommitment` in `src/llmq/commitment.cpp:240–310`, particularly the known-type/height/ancestor checks and the early successful `VerifyNull()` return; block processing's null acceptance is at `src/llmq/blockprocessor.cpp:369–378`. Update the register and scaffold comment to point at those paths. The `--core` check only verifies file existence and line bounds, so it cannot catch this semantic anchor error.


5. **minor — `docs/node-program/quirks.toml:104`: quirk 9 describes the tolerance around the wrong difficulty.** The title says nBits within ±50% of the computed difficulty is accepted. Core's `src/validation.cpp:4479–4484` instead tests `abs(n1 - n2) > n1 * 0.5`, where `n1` comes from the candidate block and `n2` is the computed difficulty. Thus the allowed difference is 50% of the candidate's difficulty; for an expected difficulty of 100, a candidate difficulty of 190 passes (90 ≤ 95), despite being outside 100 ± 50. The existing Core-produced `oracle/quirks.json` vector labelled `half: at the boundary` likewise records `accepts = true` with candidate difficulty `n1` exactly twice computed difficulty `n2`. Conversely, candidate 55 fails (45 > 27.5), despite being inside that stated interval. Also the comparison is on decoded difficulty, not a numerical interval around the compact nBits encoding. State Core's rejection predicate `abs(actual - expected) > 0.5 * actual`, or describe it as half-of-claimed-difficulty tolerance; retain the existing test's non-finite edge cases when spelling out the predicate. The referenced existing test and implementation preserve the correct rule; the new normative title is the defect.

## Acceptance and compliance

- The register contains all 38 requested IDs exactly once, plus existing later IDs 39–42. Including those four is justified by DEC-25(4), DEC-29/DEC-33 and DEC-50(4), and A1's append-new-quirks rule; it also avoids leaving existing quirk tests unregistered.
- `python3 -I contrib/node/check-quirks.py --core ~/workspace/dash` passes on the reviewed tree: **42 entries, 42 named test functions, 12 tested, 30 ignored, 0 dropped**. For IDs 1–38 alone: **11 tested, 27 ignored**. All scaffold ignore strings name their registered owners. The consistency guarantee nevertheless has finding 1's false-negative gap.
- Required explicit owners are correct: 1/CS0, 34/BS5, 35 and 37/BS11a-t, 36/LQ2, 38/LQ3. Other owners are real PLAN task rows, with secondary task assignments retained in `also`.
- Quirk 1 uses “quirk 1: see PLAN §7 handling” and references the existing test/checker. No private details were inspected or reproduced in this review.
- Implemented quirks reference existing tests and Core-produced data. Inspected the script recorded-data tests, DGW/payment-limit oracle-vector tests, KGW provenance and the pinned BLS wrapper tests. This branch adds no hand-written behavior fixtures. Quirks 35 and 37 refer to existing tests at base-SDK pin `481f8c351c4f00dfad08a11063d2852e74e06016`.
- Remaining ignored bodies fail explicitly if forced with `--ignored`, rather than pretending the unimplemented rule passes. Partial implementation of the FindAndDelete helper does not make all of quirk 6 implemented; leaving the full sighash scaffold ignored is justified.
- The checker is wired into `selftest-checks.sh`, with clean-tree and mutation cases, and prints the ignored count for STATUS. PROCESS documents it as a gate. The manager still needs to record the resulting counts in STATUS when integrating.
- No new v23.1 alternatives were introduced. None of the 38 registered behaviors was established as exclusively v23.1-only; historical pre-activation behavior still matters under DEC-82. Existing dual-pin vector provenance is not a new release-profile implementation.
- O5 has no benchmark gate and changes no production consensus behavior. The GNU tooling remains agentbox gate tooling, permitted by DEC-84.

## Core source spot-checks

Used `git show v24.0.0-rc.3:<path>` in `~/workspace/dash`, resolving the tag to `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`, rather than assuming the checkout was at the tag. These checks cover more than the required ten behaviors; no live dashd was needed for the source checks.

| Quirk | Source checked at the pin | Result |
|---|---|---|
| 2 | `src/consensus/tx_check.cpp:18–24,60–67` | nType-only exemptions and minimum CbTx scriptSig length confirmed |
| 3 | `src/validation.cpp:382`; `src/policy/policy.cpp:88` | consensus `>` versus policy `>=` confirmed |
| 8 | `src/pow.cpp:81–132` | 24 blocks, 23 intervals and per-step target arithmetic confirmed |
| 9 | `src/validation.cpp:4476–4486` | inclusive 68589 boundary and half-of-actual-difficulty tolerance confirmed; new title is inaccurate (finding 5) |
| 10 | `src/validation.cpp:4578–4586` | serialized height prefix and before-DIP3 condition confirmed |
| 14 | `src/masternode/payments.cpp:279–286,355–380`; `src/evo/chainhelper.cpp:41–51` | NoCheck old-budget acceptance and sync/ChainLock gate confirmed |
| 16 | `src/evo/cbtx.cpp:30–38` | DIP0008 evaluated at previous block, v2 requirement confirmed |
| 17 | `src/evo/simplifiedmns.h:72–77`; `src/evo/simplifiedmns.cpp:45–49,126–144` | version exclusion from hash and mutation rejection confirmed |
| 19 | `src/llmq/commitment.cpp:153–160,240–310`; `src/llmq/blockprocessor.cpp:369–378` | signer minimum and null acceptance confirmed; registered blockprocessor anchor is wrong (finding 4) |
| 23 | `src/evo/deterministicmns.h:440–481,524–552` | serialization-based property keys and two BLS encoding slots confirmed |
| 30 | `src/llmq/quorumsman.cpp:738–745,810–817` | `(2^n-1) & (b >> (64-n-1))` confirmed |
| 32 | `src/versionbits.cpp:64–67,256–265` | warning-height floor and int32 operand multiplication before int64 assignment confirmed |
| 33 | `src/kernel/coinstats.cpp:72–81` | conditional-expression precedence in VARINT confirmed |
| 34 | `src/chainparams.cpp:171–172,186–187`; `src/validation.cpp:2617–2621,6670`; `src/evo/deterministicmns.cpp:773–777` | checkpoint heights/hashes, rejection strings and testnet activation constant confirmed |
| 35 | `src/bls/bls.h:100–122,184–216` | reset/valid/error outcomes and opposite-scheme fallback confirmed |
| 36 | `src/llmq/commitment.h:118–131` | all three BLS fields tested with IsValid, with other null-field requirements, confirmed |
| 37 | `src/bls/bls.h:444–456,492–511` | failed Get invalidates buffer; cross-scheme zeroing needs unresolved object (finding 2) |
| 38 | `src/llmq/blockprocessor.cpp:115`; `src/llmq/signing.h:90–100`; `src/instantsend/net_instantsend.cpp:258` | SerializeHash paths checked; lazy default-scheme serialization preserves buffered bytes before Get |

## Validation

Validation used `TMPDIR=/dev/shm/ab-review-node-ab-o5-o` (mode 700), `CARGO_BUILD_JOBS=6`, and the exact detached head. Long runs were isolated user-systemd jobs. A copy of `/work/scratch/gate.sh` was redirected to this worktree with separate logs/datadirs and the review's job limit. The first clippy attempt stalled in the shared compiler cache; it was stopped and the run restarted with `RUSTC_WRAPPER=` for this run only, without editing repository configuration or restarting the shared cache.

| Check | Result |
|---|---|
| `cargo fmt --check` | PASS |
| `cargo clippy --workspace --all-features --all-targets -- -D warnings` | PASS (includes both touched crates) |
| `cargo test -p dash-consensus -p dash-conformance --all-features --no-fail-fast` | PASS: 300 passed, 0 failed, 30 ignored, including doctests |
| Base-SDK pins, cargo-deny, R2, pure crates, CI groups, no-libm, KGW FP | PASS; cargo-deny emitted existing unused-wrapper warnings |
| Performance-harness unit tests | PASS: 107 tests |
| `contrib/node/selftest-checks.sh` | PASS: 115 cases, including 27 quirk-register cases |
| Private-test gate / private selftest | PASS: 2 receipt PASS lines / selftest ok |
| Fuzz registry and tool selftests | PASS |
| Fuzz smoke | PASS: 14/14 legs, zero divergences, crashes, sink failures or triage errors |
| `cargo test --workspace --no-fail-fast` / copied gate exit | PASS: 3,129 passed, 0 failed, 67 ignored; `GATE EXIT 0`, no rerun needed |

The two pinned base-SDK BLS test functions were inspected at the registered revision; dependency-crate tests are not executed by the rust-dashcore workspace suite. No committed behavior fixture was generated or modified during this review.

The detached review worktree and temporary reproduction copies were removed after validation. The report is uncommitted.

## Commits reviewed

- `b12bc68a` test(quirks): one test per register quirk, scaffolds ignored with their owner
- `7f211ed5` docs(node-program): the quirk register
- `ba0091bc` feat(contrib/node): check-quirks.py keeps the register and the tests in step
- `0d3f30b6` fix(quirks): register 35 and 37 as tested in the base SDK; review fixes
- `8ce4a4af` fix(contrib/node): check-quirks.py cannot be passed by unreadable ignores

blockers=0 majors=1 minors=4 nits=0
