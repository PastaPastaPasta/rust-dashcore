# CS1 phase A review r1: quality, evidence, performance, CS0 reconciliation (Opus, reviewer 2 of 2)

- **Under review:** rust-dashcore `node/cs1-script` at `620849c7`. The base is `77d9fd71` on node/integration, and the diff spans 33 files, mostly the new `dash-consensus/` script system plus the recorder, the census and O4 loader moves.
- **Design:** designs/CS1-script.md (approved; DEC-25, DEC-38). DEC-42 makes this the second Opus review. Reviewer 1 does the line-by-line Core differential, so this one covers quality, evidence, performance, the merge with CS0 and the author's deviations.
- **Method:**
  - Read every non-test source file in `dash-consensus/src`.
  - Used the `code-review` skill at high effort as one input. It found no consensus divergence; its points are folded in below.
  - Re-ran or rebuilt everything in a scratch clone: `/work/scratch/cs1-review-b/` (`repo/` is the clone, `merge/` the trial merge, `bench/`, `diff/` and `fuzz/` my harnesses, `scripts/` my scripts).
  - Nothing on the branch was modified.

## Verdict

**APPROVE-WITH-NITS.** No blockers and no majors.

- **Behaviour.** The port matches Core on everything I could throw at it:
  - **Differential.** I added my own differential against the in-process O1b oracle: 4,000,000 `EvalScript` comparisons over both pins, comparing the result, the error code and the final stack, plus 2,000,000 each of FindAndDelete, both sigop overloads, lax-DER parsing, CheckLowS and CheckSignatureEncoding. There were 0 mismatches.
  - **Panics.** A 12M-case campaign in a build with debug assertions and overflow checks, using a random mock checker so that the CHECKSIG/CHECKMULTISIG success paths run, produced no panic.
- **Fixture provenance.** I re-derived the recorded fixture from Core on both pins, and it matches byte for byte.
- **Performance.** Both claims reproduce, and there is no quadratic path beyond Core's.
- **Merge with CS0.** The author's plan compiles and passes every test, clippy and check-pure.

The findings are minor process and documentation items.

---

## 1. Code quality and fit

### 1.1 Purity rules
- **`contrib/node/check-pure.sh` passes at HEAD and on the CS0 trial merge.** That covers cargo-deny bans plus clippy with `pure-clippy.toml`, with all features and with no default features. So do `cargo clippy -p dash-consensus --all-targets --all-features -D warnings`, `cargo fmt --check`, `RUSTDOCFLAGS=-D warnings cargo doc` and `cargo +1.89 check` (MSRV).
- **Dependency features.** `secp256k1 =0.33.1` is built with only `std` and `alloc`: no `rand` and no `getrandom`.
- **Thread-local context.** `secp256k1::ecdsa::verify` uses the crate's thread-local `RefCell<Secp256k1<All>>` (`secp256k1-0.33.1/src/context/internal_std.rs:12`). The design's §4.7 already covers this; it is not a global of our own.
- **`#![forbid(unsafe_code)]`.** It holds. The lax DER parser is a safe port, and I checked its index arithmetic: every access is guarded by an `inputlen - pos` comparison, as in Core.

### 1.2 The panic contract: can any input panic the interpreter?
**No input can.** Only caller-chosen arguments can:

| Site | Trigger | Core | Ruling |
|---|---|---|---|
| `interpreter.rs:1021` `assert!` | `CLEANSTACK` without `P2SH`, once evaluation reaches the clean-stack check | `assert` (abort), `interpreter.cpp:1710` | Acceptable: the flags are code constants, and parity with Core's abort is the right call. See m-1. |
| `num.rs:45` `assert!(max_size <= 8)` | a caller passes a width over 8 | no equivalent (shift UB) | Fine and documented. |
| `num.rs:192/201/210`, `opcodes.rs:145`, `parse.rs:93` `debug_assert!` | public-API misuse only | `assert`, which is **always on** in Core | See m-2. |

Evidence:
- **Unreachable arms.** Every `top`/`pop`/`top_mut` failure arm and the CHECKMULTISIG dummy check are unreachable by construction. Region coverage confirms it: these are the only unreached regions under the Core-backed tests (§2.3).
- **Fuzzing.** I ran 12,000,000 generated scriptSig/scriptPubKey pairs: a quarter P2SH-wrapped, random flags with CLEANSTACK trimmed, a random-verdict checker so CHECKSIG/CHECKMULTISIG/CLTV/CSV success paths execute, plus sigops, FindAndDelete, `parse_der_lax` and `check_low_s` on the same bytes. The build had `debug-assertions` and `overflow-checks` on (`/work/scratch/cs1-review-b/fuzz`). There were no panics. Every error class appeared, and 13k–14k cases per 3M succeeded.

### 1.3 Fit with rust-dashcore patterns
- **Structure.** The modules, names and `// Core:` anchors follow the design's §3 layout, and the code reads as a faithful port.
- **Error types.** `ScriptError` writes `Display` and `Error` by hand rather than using thiserror, which CLAUDE.md asks for. In a pure crate that avoids a proc-macro dependency, so I accept it (n-4).

## 2. Test evidence

### 2.1 Provenance
- **O4's 4,726 records** (4,512 DIP0020 and 214 CHECKDATASIG; the counts are asserted in `tests/script_records.rs:52-58`).
  - **Origin.** They are O4's transcriptions from Core's own unit tests, made with O4's instrumentation at commit `0364ed626ec2` (v24.0.0-rc.3). They are listed in `test-vectors/core/0364ed626ec2/transcribed/SHA256SUMS` and documented in `PROVENANCE.md`.
  - **Pin coverage.** They cover only the v24 pin. The script layer is identical across the two pins (design §2), and the recorder below covers both, so that is sufficient.
- **The 14,850-record fixture** (`tests/data/script_recorded.json`; 16 sections that sum to 14,850).
  - **Reproduced.** I ran `CS1_SCRATCH=/work/scratch/cs1-review-b/recorder record.py --check`. It rebuilt `record.cpp` against both oracle-shim build trees and printed "matches both pins". `record.py` exits non-zero if the two pins' outputs differ.
  - **Clean sources.** The shim commits (`v23.1.8-5-g350ce242a5` and `v24.0.0-rc.3-5-g23769258c2`) touch only `src/Makefile.am`, `src/Makefile.oracle.include` and `src/test/oracle/*`, and both worktrees are clean. So the archives are Core's script code unmodified.
  - **Real Core calls.** The recorder calls Core's own `EvalScript`, `VerifyScript`, `FindAndDelete`, `GetSigOpCount`, `GetScriptOp`, `CScriptNum`/`CScriptNum10`, `CheckSignatureEncoding`, `CPubKey::Verify`/`CheckLowS`, `ecdsa_signature_parse_der_lax` and `ScriptErrorString`.
  - **Mock checker.** The Rust `MockChecker` (`tests/common/mod.rs:73-93`) matches `record.cpp:136-156` exactly.
- **Ported Core unit tests** (`tests/script_core_units.rs`).
  - **Expectations.** They are Core's literals. The exception is `scriptnum10`, whose reference values come from the recorder running Core's `CScriptNum10`, which is correct.
  - **Scope.** It covers `scriptnum_tests` (both cases), `sigopcount_tests/GetSigOpCount`, and the transaction-free cases of `script_tests`: PushData, cltv_truncated, standard_push, IsPushOnly_on_invalid_scripts and FindAndDelete. Scope is ruled on in §5.3.
- **Independent check (mine).** `/work/scratch/cs1-review-b/diff` links `dash-oracle-sys` (ABI 4, pinned archives for `350ce242`/`23769258`) and compares against Core at both pins over 2,000,000 generated scripts × 2 pins. About 5% of the scripts evaluate to OK. Mutated DER covers truncation, length-of-length forms, zero/0xff/0x7f padding and hash-type bytes. Result: **0 mismatches** in the error code, the result and the final stack, and in FindAndDelete (script and count), sigops (both modes), `parse_der_lax`, `check_low_s` and `check_signature_encoding`.

### 2.2 Is the line census meaningful?
- **Reproduced.** At HEAD: 1,082 lines reached, 88 error sites, 8 marked unreachable, 0 missed.
- **Mixed evidence.** As written, it counts lines reached by **any** `dash-consensus` test, including the in-crate unit tests and the proptests. Those compare against our own expectations, not Core's.
- **Core-only re-run.** I ran it again with only the Core-backed binaries: `script_records`, `script_core_recorded`, `script_core_units` and `quirks` (whose expectations are recorded from Core). The result was 1,071 reached and **11 missed**:
  - `parse.rs:64-67` `instructions()`, which only tests call;
  - `parse.rs:114`, `parse.rs:132-134`: pushes longer than 65,535 bytes, which are unreachable in consensus because pushes over 520 bytes fail first;
  - `checker.rs:54-56` `DirectVerifier`, which is unused until phase B.

  None of these is a consensus line, so the census is meaningful today. The design should still say that the gate means *reached by Core-backed evidence* (m-3).

### 2.3 How big is the branch-level blind spot?
I measured it rather than estimated it, with nightly `-Zcoverage-options=branch` plus region coverage from LLVM 22.1.8 on the same toolchain line, over the Core-backed tests only (`/work/scratch/cs1-review-b/scripts/branch-cov.sh`). I aggregated across monomorphizations.

| File | Branches | Not taken both ways |
|---|---|---|
| interpreter.rs | 276 | 9 |
| ecdsa.rs | 48 | 1 |
| encoding.rs | 60 | 2 |
| parse.rs | 50 | 3 |
| num.rs, sigops.rs | — | 0 |

- **What the misses are:**
  - the marked-unreachable sites: `interpreter.rs:952`, `:1006`, `encoding.rs:82`, `:96`;
  - debug-assert arms;
  - the >65,535-byte push arms;
  - two real gaps:
    - `ecdsa.rs:86`: the S-integer tag check never fails, so no Core-recorded signature ends right after R or has a wrong S tag. My differential's truncated and mutated DER covers it, with 0 mismatches.
    - `parse.rs:86`: `script[1] == 0x14` is never false for a 23-byte script starting `a9`.
- **Regions.** The only unreached ones in `interpreter.rs` are the `top()`/`pop()`/`top_mut()` failure arms behind `?`, which are guarded by explicit size checks.
- **The `?` concern.** The code-review skill flagged that `?` error paths share a line with the success path. The concern is real for the *method*, but every scriptnum `?` error path is reached by Core-backed tests:
  - CLTV and CSV decode errors (`:255`, `:271`);
  - PICK/ROLL (`:457`);
  - unary and binary numeric operations (`:559`, `:592-593`);
  - WITHIN (`:640-642`);
  - SPLIT (`:769`);
  - NUM2BIN (`:790`);
  - CHECKMULTISIG's key and signature counts (`:863`, `:882`).

**Conclusion:** the risk of a branch-level miss is low and now quantified. Two small gaps and a method fix go to m-3.

## 3. Performance

Instructions per operation (`instructions:u`, pinned to one CPU). My harnesses run fixed iteration counts, and the setup cost is differenced out. They are `/work/scratch/cs1-review-b/bench/{rust,core}`; `bench_dash` reports nanobench's own ins/op. The box load average was 60–72, so cycles are meaningless; instruction counts repeated to ±0.1 per operation.

| Bench | Rust port | bench_dash v23.1.8 | bench_dash v24.0.0-rc.3 | Core lib harness v23 / v24 | Claim |
|---|---|---|---|---|---|
| VerifyNestedIfScript | **554,287** | 670,498 (−17.3%) | 672,579 (−17.6%) | 625,769 / 627,049 (−11.4% / −11.6%) | "17.5% fewer": **holds against bench_dash**; see n-1 |
| ECDSAVerify | **379,829** | 378,330 (+0.40%) | 378,750 (+0.28%) | noisy (Core RNG strengthening in setup) | "within 0.5%": **holds** |
| ECDSAVerify_LargeBlock (per 1,000) | **379.90M** | 379.15M (+0.20%) | 379.22M (+0.18%) | — | holds |

- **VerifyNestedIfScript.** My Rust figure is exactly the author's "554k".
  - **The gap.** The bench_dash binaries (B1's `/work/bench/bin`, built in B1's container) execute about 44k more instructions per operation than the same Core script code built on agentbox with the same `-O2` and hardening flags. A profile shows the cause: `vector::emplace_back` is not inlined and EvalScript is compiled differently in the bench_dash build.
  - **What holds.** The direction holds either way; the size of the win depends on the Core build.

**Quadratic paths.** I found none beyond Core's, and the port is faster on the adversarial shapes:

| Shape | Rust port | Core v23.1.8 / v24.0.0-rc.3 |
|---|---|---|
| FindAndDelete and scriptCode copies: 66 × `2DUP CHECKSIG DROP` over a 9.7 KB scriptCode (9,400 unexecuted `OP_1`s, 72-byte signature) | **50.6M** instr/eval | 84.24M / 84.25M |
| Large stack: 998 items, 200 × `<997> OP_ROLL` | **0.60M** | 3.35M / 3.35M |

- **FindAndDelete.** It is linear per call. A match is tested only at the cursor, and a match of length *k* at an instruction boundary implies the instruction there has the pattern's opcode and length, so *k* ≤ that instruction's length. Summed over boundaries, that is O(script length). Core's algorithm is identical.
- **OP_CAT/OP_SPLIT.** Each operand is at most 520 bytes, and there are at most 201 counted ops. SPLIT clones before its range check (n-2), which is a constant cost.
- **Allocation.** Per-signature scriptCode copies and FindAndDelete's result buffer mirror Core's `CScript scriptCode(...)` and `result.insert(...)` (n-3).

## 4. Reconciliation with CS0 (trial merge)

`/work/scratch/cs1-review-b/merge`: CS0 (`61056e73`) with CS1 (`620849c7`) merged in.

**Conflicts (7 files, all mechanical):**

| File | Nature |
|---|---|
| `.github/ci-groups.yml` | `node-conformance`: CS0 adds `node-l1`, integration added `dash-conformance`. Keep both. |
| `Cargo.toml` | `members` (CS0's `node-sink`/`node-l1` against integration's `conformance`); the `dash-pow` workspace and `[patch]` lines. Union, keeping CS0's. |
| `Cargo.lock` | Regenerate. |
| `dash-consensus/Cargo.toml` | Description; union of dependencies (CS1 adds `bitcoin_hashes 1.2`, `secp256k1 =0.33.1`, `secp256k1-sys =0.14.1`); two `[[bench]]` entries. |
| `dash-consensus/clippy.toml` | add/add. See m-4. |
| `dash-consensus/src/hash.rs` | add/add. CS0's file plus CS1's `sha256`, `sha1`, `ripemd160` and `hash160`; drop `Sha256dEngine`. |
| `dash-consensus/src/lib.rs` | add/add. CS0's root plus `pub mod script;`; drop CS1's "provisional crate root" text. |

- **Integration drift.** About half the conflicts come from CS0's base (`89309bea`) being older than CS1's (`77d9fd71`). CS0's own rebase onto integration removes them.
- **Result with the author's plan:**
  - **0 compile errors.**
  - `Sha256dEngine` was referenced only by its own test, so dropping it costs nothing.
  - `cargo test -p dash-consensus` passes: all CS0 and CS1 suites, 0 failures.
  - `clippy --all-targets --all-features -D warnings` is clean.
  - `check-pure.sh` passes, using CS0's version, which also merges the crate's own `clippy.toml`.

**Two things the plan does not say (m-4, m-5):**
- **Which `clippy.toml` wins.** CS0's crate file carries the `dash_primitives::Block::merkle`, `BaseCodec::encode` and `Hashable::hash` bans. Clippy reads only the nearest `clippy.toml`, so taking CS1's file would silently drop those bans for this crate. CS1's extra I/O, time and thread bans are already in `pure-clippy.toml`, which CS0's `check-pure.sh` merges.
- **Where the release-dependent mandatory mask lives.** CS0 reserves `UngatedRules::mandatory_script_flags` (`release.rs:76`). CS1 defines `ScriptFlags::MANDATORY_V23_1` and `MANDATORY_V24_0` (`flags.rs:49-52`).

## 5. Rulings on the four deviations

### 5.1 `get_op` returns `Option<Op>` (a struct `{opcode, data}`), not `Result<Op, ()>` with an enum
**Accepted.**
- `Result<_, ()>` carries no more information than `Option`, and clippy flags it (`result_unit_err`).
- The struct form is Core's own (`opcodetype` plus `vchRet`), and it makes the "data cleared on every call" rule that `script_sigop_count_with_sig` depends on explicit (`parse.rs:6-10`).
- The failure-cursor contract is unchanged. It is tested against O4's independent `reference_get_op` at every start position (proptest), and against Core in the recorder's `get_op` section (514 records) and my differential.
- Fix the signature in the design's §4.3.

### 5.2 The base-sdk opcode-name check was skipped
**Accepted as a deferral, not a drop.**
- **Why skipping is reasonable.** `dash-script` is not a workspace dependency at the base-sdk pin. Adding one means a workspace entry plus a `[patch]` entry, done through `bump-base-sdk.sh`, for a test that guards base-sdk, not consensus. CS1 already checks its own table against Core's `GetOpName` on both pins (the `op_names` section, 256 records), which is the authority.
- **What I checked by hand.** `pkgs/script/src/opcode.rs` at base-sdk `33fcc59e` defines 114 names. All of them agree with Core's names except the small integers: base-sdk says `OP_0`, `OP_1NEGATE` and `OP_1..OP_16`, where Core's `GetOpName` says `0`, `-1` and `1..16`. That difference is not a consensus matter, but anything that renders asm through base-sdk will differ from Core's `decodescript`.
- **Follow-up.** Add a backlog item: put the drift check where `dash-script` is already a dependency (BS2 conformance or `dash-core-vectors`), and note the small-integer rendering difference for whoever owns asm output.

### 5.3 Scope of the unit-test port
**Accepted.**
- **What was left out.** Everything excluded needs a transaction or the sighash, or is not CS1's:
  - `script_json_test` (1,282 vectors);
  - `script_build`;
  - `script_CHECKMULTISIG12/23`;
  - `multisig_tests`;
  - `script_p2sh_tests`;
  - `sighash_tests`;
  - `script_combineSigs`, which is signing;
  - `script_GetScriptAsm`, which is core_write;
  - `dashconsensus_*`, which is the libdashconsensus API.
- **Phase B must port:** `script_json_test`, `tx_valid/tx_invalid` (script parts), `sighash_from_data`/`sighash_test`, `CHECKMULTISIG12/23`, `multisig_verify`, and `script_p2sh_tests` `sign`/`norecurse`/`switchover`.
- **Merge text.** The phase A merge note should not describe L0 as complete. The largest L0 corpus, `script_tests.json`, is not run yet.

### 5.4 The recorder instead of O1b in phase A
**Accepted for phase A, with a correction to the rationale.**
- **O1b is already available.** Its entry points are merged at CS1's base: `dash-oracle-sys` ABI 4 has `eval_script`, `find_and_delete`, `script_sigops(_with_sig)`, `parse_der_lax`, `check_low_s`, `pubkey_verify`, `check_signature_encoding`, the `scriptnum_*` functions and `check_minimal_push` (`dash-oracle-sys/src/lib.rs:781-946`). An ad-hoc differential costs about 100 lines (§2.1).
- **Why the fuzz targets still wait.** DEC-30 N5 puts every fuzz target behind CS0's runner, which is still on CS0's branch, so CS1's `diff_*` targets legitimately wait for it.
- **Why the recorder is the right phase A evidence.** It covers both pins, it is reproducible with `--check`, and its named cases are the round-1 review probes.
- **Required in phase B:**
  - the design's `script_oracle.rs`, a proptest differential behind the `oracle` feature, which does not need the fuzz runner;
  - the four L1 targets once CS0 merges.
- **Keep the recorder** as a regression fixture.

---

## Findings

### Blockers
None.

### Majors
None.

### Minors

- **m-1. Validate flags at construction (phase B).** `interpreter.rs:1021`, `flags.rs:89`.
  - **Scenario.** `verify_script(…, STANDARD.difference(P2SH), …)` panics on any spend that reaches the clean-stack check. The panic matches Core's abort and is documented, and no current caller can trigger it.
  - **Risk.** CS1 phase B's classification re-run (`flags & MANDATORY`) and CS6's check queue will compute flags. A future mask edit (for example a release profile whose mandatory set lacks P2SH) would turn an input failure into a node panic inside a worker.
  - **Fix.** Make phase B's `ScriptPolicy` or `InputCheck` constructors reject `!is_valid_combination()`, or `debug_assert!` it at every flag computation. Keep the `assert!` as the last line of defence.
- **m-2. The "debug-only like Core's assert" doc is wrong.** `opcodes.rs:141-145`; also `num.rs:192/201/210`.
  - **Facts.** Core cannot be built without assertions (`src/util/check.h:50`, `#error "Cannot compile without assertions!"`). Core's `DecodeOP_N` and `CScriptNum` operator asserts therefore abort in release. In a Rust release build, `decode_op_n(0x01)` silently returns 177 through `u8` wraparound.
  - **Impact.** It cannot be reached from the interpreter, since sigops only passes OP_1..16. But the functions are `pub`, and the comment misstates Core.
  - **Fix.** Correct the comment ("Core's assert is always on; here it is a debug assertion because no internal caller can violate it"), or make `decode_op_n` total (`Option<u32>`) or `pub(crate)`.
- **m-3. Census: say "Core-backed", and close two small gaps.** `contrib/node/script-branch-census.sh:35, :47, :57`.
  - **Method.** The census runs all tests, including self-referential proptests and unit tests. It is line-based, so `?` error arms share the success line. And it omits `flags.rs`, which holds `block_script_flags`; the Core-backed tests take none of its 10 branches.
  - **Measured.** The Core-only re-run misses only 11 non-consensus lines, and branch and region coverage shows every reachable scriptnum `?` arm is taken (§2.2, §2.3).
  - **Fix:**
    1. Run the census over the Core-backed binaries only (`--test script_records --test script_core_recorded --test script_core_units --test quirks`), or report both numbers.
    2. Optionally switch to `llvm-cov export` region data, which handles `?`.
    3. Add `flags.rs`.
    4. Add recorder cases for a DER signature with a missing or wrong S tag (`ecdsa.rs:86`) and a 23-byte `a9 <≠14> … 87` script (`parse.rs:86`).
    5. Show cargo's stderr rather than discarding it (`:29`, `:35`), so that a build failure is diagnosable.
- **m-4. The merge plan must name which `dash-consensus/clippy.toml` wins.**
  - **Risk.** Taking CS1's side drops CS0's `Block::merkle`, `BaseCodec::encode` and `Hashable::hash` bans for this crate, and nothing fails.
  - **Fix.** Take CS0's file. CS1's extra I/O, time and thread bans are covered by `pure-clippy.toml`, which CS0's `check-pure.sh` merges.
- **m-5. Two sources of truth for the mandatory mask after the merge.** CS1 `flags.rs:49-52` (`MANDATORY_V23_1`, `MANDATORY_V24_0`) and CS0 `release.rs:76` (`UngatedRules::mandatory_script_flags`, reserved for CS1).
  - **Fix.** In phase B, give `UngatedRules` the field typed `ScriptFlags`, set to these constants, and have the classification read only the field. Add this to the merge plan so the constants don't become a second selector.

### Nits

- **n-1. Bench reporting.** Record which Core build a ratio is measured against. The 17.5% holds against B1's `bench_dash` (670.5k/672.6k ins/op). Against Core's script archives built on agentbox with the same flags (626k/627k), it is 11.5%. The author's commit message cites the bench_dash numbers correctly.
- **n-2. OP_SPLIT clone.** `interpreter.rs:768` clones the operand before the position decode and range check. Checking first and then `split_off` on the popped value gives the same stack on every path without the clone.
- **n-3. ScriptCode allocations.** `interpreter.rs:157`, `:894` and `parse.rs:156`: two allocations and copies of the scriptCode per signature check, even when FindAndDelete finds nothing. This is Core-equivalent and the port is already 40% under Core on the worst case. Leave it to phase C's profiling.
- **n-4. Error-type ergonomics.** `num.rs` `ScriptNumError` implements neither `Display` nor `Error`, and `ScriptError` writes its impls by hand instead of using thiserror (CLAUDE.md). Fine for a pure crate; mention it in the module docs.
- **n-5. Exact pins.** `dash-consensus/Cargo.toml:28-29` pins `secp256k1 =0.33.1` and `-sys =0.14.1` in the crate, while the root workspace entry is a caret. The pin is deliberate (design §3) but invisible from the root. Move it into `[workspace.dependencies]` with the comment, so the next `dash-pkc`/`bitcoin-crypto` bump sees why resolution fails.
- **n-6. `record.py --check` and provenance.** `record.py:88` compares the whole file, including the `git describe` strings. The oracle-shim worktrees gain commits (O1b follow-ups), so `--check` will report a false "differs" with unchanged records. Compare the record sections, and check the provenance strings separately.
- **n-7. Shared P2SH template.** The P2SH scriptPubKey template is written out four times: `tests/script_core_units.rs:36`, `tests/script_properties.rs:127`, and the `sigops.rs` and `parse.rs` test modules. `tests/common` exists for this.

## Reproduction

All under `/work/scratch/cs1-review-b/`.

| What | Where |
|---|---|
| Line census (the author's), at HEAD | `repo/contrib/node/script-branch-census.sh` |
| Core-only census | `scripts/census-core-only.sh` |
| Branch census | `scripts/branch-cov.sh` (nightly-2026-07-15 + 1.98.1 llvm-tools) |
| Fixture re-derivation | `CS1_SCRATCH=…/recorder python3 repo/contrib/node/cs1-script-recorder/record.py --check` |
| Differential against O1b | `diff/target/release/cs1diff <iters> <seed>`, logs `diff/d-*.txt` |
| No-panic campaign | `fuzz/target/release/cs1fz <iters> <seed>`, logs `fuzz/out-*.txt` |
| Benches | `scripts/measure.sh <bin> {nested,ecdsa,large} <n1> <n2>` with `bench/core/h-v23.1`, `bench/core/h-v24.0`, `bench/rust/target/release/cs1h`; `bench_dash` copies in `bench/bd/`; worst-case scripts `bench/{worst,roll}.hex` (mode `fad`) |
| CS0 trial merge | `merge/` (branch `trial-merge` in the scratch clone only), log `merge-test.log` |
